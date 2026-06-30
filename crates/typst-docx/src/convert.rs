//! The top-level recursion entry and the native-element block dispatch.

use typst_library::diag::SourceResult;
use typst_library::foundations::Content;
use typst_library::introspection::TagElem;
use typst_library::math::EquationElem;
use typst_library::model::{
    EnumElem, FigureElem, HeadingElem, ListElem, OutlineElem, ParElem, ParbreakElem,
    QuoteElem, TableElem, TermsElem,
};
use typst_library::routines::Pair;

use crate::ctx::DocxCtx;
use crate::dom::{Block, Para, ParaChild, ParaProps, Run, RunProps};
use crate::mappers;

/// Lowers the top-level realized children into the document body blocks.
pub fn run(ctx: &mut DocxCtx, children: &[Pair]) -> SourceResult<Vec<Block>> {
    convert_children(ctx, children)
}

/// Lowers a slice of realized native pairs into blocks.
///
/// Because no show rules run for `Target::Docx`, inline-level native elements
/// (`StrongElem`, `EmphElem`, `TextElem`, …) reach us interleaved with
/// block-level elements rather than pre-grouped into `ParElem`s. We therefore
/// coalesce consecutive inline children into a single paragraph here, flushing
/// the buffer whenever a block-level element or paragraph break is hit.
pub fn convert_children(ctx: &mut DocxCtx, children: &[Pair]) -> SourceResult<Vec<Block>> {
    use typst_library::foundations::Resolve;
    use typst_library::layout::VElem;

    let mut blocks = Vec::new();
    // Buffered paragraph children currently being assembled, plus its
    // resolved paragraph properties (from the first `ParElem` seen).
    let mut pending: Vec<ParaChild> = Vec::new();
    let mut pending_props: Option<ParaProps> = None;
    let mut have_pending = false;
    // Accumulated `#v(..)` spacing (twips) waiting to be folded into the
    // `before` of the next paragraph produced (G4c). A trailing `#v()` thus
    // collapses to nothing, matching Typst's own behaviour.
    let mut pending_v: i32 = 0;
    // Whether the last paragraph-bearing child was a `ParElem` (vs. inline
    // content). Used to tell two adjacent paragraphs apart from one paragraph
    // that Typst split into `[par, inline-equation, par]`.
    let mut last_was_par = false;

    for (child, styles) in children {
        // A *leading* pagebreak (before any content) is page-setup machinery, not
        // a real break: a document that opens with `#set page(..)` — most theses /
        // books / papers — gets a synthetic pagebreak marking the start of the
        // page run, which as `<w:br>` would add a spurious blank first page. Drop
        // any pagebreak emitted before the first content, mirroring
        // `resolve_sections` (and typst-layout's page-run logic, which never
        // renders a page before the first one). A `#pagebreak()` *after* content is
        // a real break and is kept.
        if child.is::<typst_library::layout::PagebreakElem>()
            && !have_pending
            && pending.is_empty()
            // No *visible* content yet — leading introspection tags don't count.
            && !blocks.iter().any(|b| !matches!(b, Block::Tag(_)))
        {
            continue;
        }
        if child.is::<ParbreakElem>() {
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            last_was_par = false;
            continue;
        }
        if let Some(par) = child.to_packed::<ParElem>() {
            // Consecutive `ParElem`s are separate paragraphs and must be flushed
            // apart (`ParbreakElem`s are consumed during realization), otherwise
            // the whole document would merge into one `<w:p>` and per-paragraph
            // `w:jc`/spacing would be lost. BUT Typst splits a single paragraph
            // that contains an inline equation into `[par, equation, par]`; that
            // trailing `par` must *continue* the equation's paragraph, not start
            // a new one. It continues when the previous content was inline (not
            // another `par`) and there is buffered content to join.
            let continues = !last_was_par && have_pending;
            if !continues {
                let from = blocks.len();
                flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
                pending_v = apply_pending_v(&mut blocks, from, pending_v);
                // Typst's default `first-line-indent` (`all: false`) indents a
                // paragraph only when it directly follows another; Word's
                // `w:firstLine` has no such rule, so apply it here. Checked
                // *after* the flush so `blocks.last()` is the previous paragraph.
                let prev_was_para = matches!(blocks.last(), Some(Block::Para(_)));
                let mut props = ctx.resolve_par_props(par, *styles);
                if prev_was_para
                    && props.ind.as_ref().and_then(|i| i.first_line).is_none()
                    && let Some(amount) = ctx.consecutive_first_line_indent(*styles)
                {
                    props.ind.get_or_insert_with(Default::default).first_line = Some(amount);
                }
                pending_props = Some(props);
            }
            let par_start = pending.len();
            inline_children(ctx, &par.body, *styles, &mut pending)?;
            // A labeled paragraph (`text … <spot>`) is a valid `#link(<spot>)`
            // target, so bracket its content with a bookmark (otherwise the link
            // anchor is dangling).
            if child.label().is_some()
                && let Some(loc) = child.location()
            {
                let (id, name) = ctx.add_bookmark(loc);
                pending.insert(par_start, ParaChild::BookmarkStart { id, name });
                pending.push(ParaChild::BookmarkEnd { id });
            }
            have_pending = true;
            last_was_par = true;
        } else if let Some(elem) = child.to_packed::<TagElem>() {
            // Introspection tag: record as a block-level tag (kept for the
            // introspector). Transparent — does not change paragraph structure.
            if !have_pending {
                blocks.push(Block::Tag(elem.tag.clone()));
            } else {
                pending.push(ParaChild::Tag(elem.tag.clone()));
            }
        } else if let Some(elem) = child.to_packed::<VElem>() {
            // Vertical spacing (G4c): fold into the next paragraph's `before`
            // rather than dropping it. Fractional spacing has no fixed twip
            // value, so it is dropped.
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            if let typst_library::layout::Spacing::Rel(rel) = elem.amount {
                let twips = crate::props::abs_to_twip(rel.abs.resolve(*styles));
                pending_v = (pending_v + twips).max(0);
            }
            last_was_par = false;
        } else if let Some(eq) = child.to_packed::<EquationElem>()
            && !eq.block.get(*styles)
        {
            // An *inline* equation must stay in the current paragraph — otherwise
            // it flushes the surrounding text and breaks the sentence onto
            // separate lines. (Block equations fall through to `handle_block`.)
            push_inline(ctx, child, *styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else if is_inline(child) {
            push_inline(ctx, child, *styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else {
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            handle_block(ctx, child, *styles, &mut blocks)?;
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            last_was_par = false;
        }
    }
    let from = blocks.len();
    flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
    apply_pending_v(&mut blocks, from, pending_v);

    // Drop a *trailing* pagebreak-only paragraph: a document closing a `set page`
    // run leaves a page-boundary break after the last content which, as `<w:br>`,
    // would add a blank final page (the symmetric case to the leading break
    // dropped above). A real break is always followed by content, so this only
    // ever removes the spurious closing one.
    while let Some(Block::Para(para)) = blocks.last() {
        if !para.content.is_empty()
            && para.content.iter().all(|c| matches!(c, ParaChild::Run(Run::PageBreak)))
        {
            blocks.pop();
        } else {
            break;
        }
    }

    // A paragraph using a fractional `#h(1fr)` (a fill-tab) gets a right-aligned
    // tab stop at the content width, so the tab pushes the following content to
    // the right margin (the "Left … Right" header idiom) instead of stopping at
    // the next default tab stop.
    let content_twips = (ctx.raster_width.to_pt() * 20.0) as i32;
    if content_twips > 0 {
        use crate::dom::{TabAlign, TabStop};
        for block in &mut blocks {
            if let Block::Para(para) = block
                && para.content.iter().any(|c| matches!(c, ParaChild::Run(Run::FillTab)))
                && !para.props.tabs.iter().any(|t| matches!(t.val, TabAlign::End))
            {
                para.props.tabs.push(TabStop {
                    val: TabAlign::End,
                    leader: None,
                    pos: content_twips,
                });
            }
        }
    }
    Ok(blocks)
}

/// Folds an accumulated `#v(..)` spacing into the `before` of the first
/// paragraph produced at/after `from`. Returns the residual (0 if applied, or
/// the unchanged amount if no paragraph was found to carry it).
fn apply_pending_v(blocks: &mut [Block], from: usize, pending_v: i32) -> i32 {
    if pending_v == 0 {
        return 0;
    }
    for block in &mut blocks[from..] {
        if let Block::Para(para) = block {
            let sp = para.props.spacing.get_or_insert_with(Default::default);
            sp.before = Some(sp.before.unwrap_or(0) + pending_v);
            return 0;
        }
    }
    pending_v
}

/// Lowers an inline body into paragraph children, preserving hyperlinks.
fn inline_children(
    ctx: &mut DocxCtx,
    body: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<ParaChild>,
) -> SourceResult<()> {
    out.extend(ctx.inline_pchildren(body, styles, RunProps::default())?);
    Ok(())
}

/// Routes one inline child into paragraph children. Links become hyperlinks;
/// everything else lowers to runs via the run-level handler.
fn push_inline(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<ParaChild>,
) -> SourceResult<()> {
    use typst_library::model::LinkElem;
    if let Some(elem) = child.to_packed::<LinkElem>() {
        out.extend(ctx.link_children(elem, styles, &RunProps::default())?);
    } else {
        let mut runs = Vec::new();
        ctx.handle_inline(child, styles, &RunProps::default(), &mut runs)?;
        out.extend(runs.into_iter().map(ParaChild::Run));
    }
    Ok(())
}

/// Flushes buffered paragraph children into a paragraph block.
fn flush(
    pending: &mut Vec<ParaChild>,
    props: &mut Option<ParaProps>,
    have_pending: &mut bool,
    blocks: &mut Vec<Block>,
) {
    if !*have_pending && pending.is_empty() {
        *props = None;
        return;
    }
    let content = std::mem::take(pending);
    let props = props.take().unwrap_or_default();
    blocks.push(Block::Para(Para { props, content }));
    *have_pending = false;
}

/// Whether a native element is inline-level (formatting/text/refs/etc.).
///
/// `TagElem` and `EquationElem` are intentionally treated as block-level here so
/// that the block dispatch records introspection tags and routes equations
/// (which can be inline or block) through the math mapper.
fn is_inline(child: &Content) -> bool {
    use typst_library::layout::HElem;
    use typst_library::model::{EmphElem, LinkElem, RefElem, StrongElem};
    use typst_library::text::{
        HighlightElem, LinebreakElem, SmallcapsElem, SmartQuoteElem, SpaceElem, StrikeElem,
        SubElem, SuperElem, TextElem, UnderlineElem,
    };
    use typst_library::visualize::ImageElem;

    child.is::<TextElem>()
        || child.is::<SpaceElem>()
        || child.is::<LinebreakElem>()
        || child.is::<SmartQuoteElem>()
        || child.is::<HElem>()
        || child.is::<StrongElem>()
        || child.is::<EmphElem>()
        || child.is::<SubElem>()
        || child.is::<SuperElem>()
        || child.is::<UnderlineElem>()
        || child.is::<StrikeElem>()
        || child.is::<HighlightElem>()
        || child.is::<SmallcapsElem>()
        || child.is::<LinkElem>()
        || child.is::<RefElem>()
        || child.is::<ImageElem>()
}

/// Whether a container's body (a `#box`/`#pad`/… body) can be lowered to native
/// DOCX (extracted as real text/OMML) rather than rasterized to an image.
///
/// This is the central rasterize-vs-extract decision. Extraction is *strictly
/// better* (selectable text, smaller, reflows) and safe for almost everything —
/// because an element's introspection `Location` is assigned at *realize* time,
/// so cross-references to headings, figures, and ordinary labels inside an
/// extracted body still resolve. The one exception is introspection that only
/// exists after *layout*: a label placed *inside* an equation (a per-line
/// equation label `#<eqa>`), whose anchor is positioned per visual line by the
/// line breaker. Native OMML conversion never lays the equation out, so that
/// anchor is never produced and a reference to it would fail — such a body must
/// be rasterized (layout then runs, and the frame-tag harvest recovers it).
///
/// To extend this decision for a future layout-only-introspection case, add a
/// detector here; every container handler routes through this one function.
pub(crate) fn body_extractable(body: &Content) -> bool {
    use std::ops::ControlFlow;
    body.traverse(&mut |element: Content| {
        if let Some(eq) = element.to_packed::<EquationElem>()
            && equation_has_inner_label(&eq.body)
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })
    .is_continue()
}

/// Whether a body contains a footnote (or endnote). Word forbids footnotes
/// inside a text box (`wps:txbx`) — a file with one fails to open — so a framed
/// container whose body has a footnote must NOT become a text box; it is routed
/// to the flowing main-story representation (a shaded paragraph) or, inline,
/// extracted frameless, so the footnote stays in a legal position.
pub(crate) fn body_has_footnote(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::model::FootnoteElem;
    body.traverse(&mut |element: Content| {
        if element.is::<FootnoteElem>() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// Whether a framed container's body is safe to put inside a Word *text box*
/// (`wps:txbx`). A text box is only robust for plain text + inline formatting.
/// Richer content is either Word-fragile inside a text box (drawings — figures,
/// images, nested shapes/boxes — and complex fields / math) or is mis-handled by
/// the text-box build path (a figure/table counter inside is laid out twice —
/// once to size the box, once to extract — which reorders its introspection and
/// corrupts cross-reference numbers). Such a body is instead routed to the
/// flowing shaded-paragraph representation, which lays out once and is correct.
pub(crate) fn body_textbox_safe(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::{BoxElem, GridElem};
    use typst_library::math::EquationElem;
    use typst_library::model::{EnumElem, FigureElem, ListElem, TableElem, TermsElem};
    use typst_library::visualize::{
        CircleElem, EllipseElem, ImageElem, PolygonElem, RectElem, SquareElem,
    };
    body.traverse(&mut |e: Content| {
        let unsafe_in_textbox = e.is::<FigureElem>()
            || e.is::<ImageElem>()
            || e.is::<TableElem>()
            || e.is::<GridElem>()
            || e.is::<EquationElem>()
            || e.is::<ListElem>()
            || e.is::<EnumElem>()
            || e.is::<TermsElem>()
            // A nested framed container would become a text box inside a text box.
            || e.is::<BoxElem>()
            || e.is::<RectElem>()
            || e.is::<SquareElem>()
            || e.is::<EllipseElem>()
            || e.is::<CircleElem>()
            || e.is::<PolygonElem>();
        if unsafe_in_textbox { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_continue()
}

/// Whether an equation body carries a label *inside* it (a per-line label),
/// whose location only exists once the equation is laid out per visual line.
///
/// Such a label appears two ways: an ordinary `.label()` on an inner element, or
/// — because `<…>` is ambiguous in math — the `#<label>` form, which math parses
/// into a `raw` element whose source text is `<…>`.
fn equation_has_inner_label(eq_body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::text::{RawContent, RawElem};
    eq_body
        .traverse(&mut |element: Content| {
            let is_label = element.label().is_some()
                || element.to_packed::<RawElem>().is_some_and(|r| {
                    matches!(&r.text, RawContent::Text(s)
                        if s.len() > 2 && s.starts_with('<') && s.ends_with('>'))
                });
            if is_label {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break()
}

/// Dispatches one realized native block element.
fn handle_block(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    let block_start = out.len();
    handle_block_inner(ctx, child, styles, out)?;

    // Bookmark a LABELED block (a numbered equation `$…$ <eq>`, a labeled list,
    // …) so a `@ref`/`#link` to it resolves to a real target. Headings and
    // figures emit their own bookmark, so skip them to avoid a duplicate name.
    if child.label().is_some()
        && let Some(loc) = child.location()
        && !child.is::<HeadingElem>()
        && !child.is::<FigureElem>()
    {
        let (id, name) = ctx.add_bookmark(loc);
        bracket_bookmark(&mut out[block_start..], id, name);
    }
    Ok(())
}

/// Brackets `blocks` with a bookmark — `bookmarkStart` on its first paragraph,
/// `bookmarkEnd` on its last — without inserting any paragraph. Skips silently
/// when the range has no paragraph to anchor on (a bare table/image block, rare
/// as a labeled target).
fn bracket_bookmark(blocks: &mut [Block], id: u32, name: ecow::EcoString) {
    let first = blocks.iter().position(|b| matches!(b, Block::Para(_)));
    let last = blocks.iter().rposition(|b| matches!(b, Block::Para(_)));
    if let (Some(f), Some(l)) = (first, last) {
        if let Block::Para(p) = &mut blocks[f] {
            p.content.insert(0, ParaChild::BookmarkStart { id, name });
        }
        if let Block::Para(p) = &mut blocks[l] {
            p.content.push(ParaChild::BookmarkEnd { id });
        }
    }
}

fn handle_block_inner(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    if let Some(elem) = child.to_packed::<TagElem>() {
        out.push(Block::Tag(elem.tag.clone()));
    } else if let Some(elem) = child.to_packed::<typst_library::pdf::PdfMarkerTag>() {
        // A PDF accessibility delimiter wraps real content (`body`); it has no DOCX
        // meaning itself, so unwrap it and lower the body (otherwise the wrapped
        // content — a whole figure, list, paragraph — is dropped).
        out.extend(ctx.blocks(&elem.body, styles)?);
    } else if child.is::<typst_library::layout::PagebreakElem>() {
        // A page break maps to a `<w:br w:type="page"/>` in its own paragraph.
        out.push(Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(Run::PageBreak)],
        }));
    } else if let Some(elem) = child.to_packed::<ParElem>() {
        let props = ctx.resolve_par_props(elem, styles);
        let content = ctx.inline_pchildren(&elem.body, styles, RunProps::default())?;
        out.push(Block::Para(Para { props, content }));
    } else if child.is::<ParbreakElem>() {
        // Paragraph boundary; no-op marker.
    } else if let Some(elem) = child.to_packed::<HeadingElem>() {
        out.extend(mappers::heading::heading(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<ListElem>() {
        out.extend(mappers::list::list(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EnumElem>() {
        out.extend(mappers::list::enum_(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<TermsElem>() {
        out.extend(mappers::list::terms(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<TableElem>() {
        out.extend(mappers::table::table(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::GridElem>() {
        // A layout grid (CV sidebar, multi-column block, …) lowers to a w:tbl
        // like a table, keeping its content as editable text rather than being
        // rasterized or dropped.
        out.extend(mappers::table::grid(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::StackElem>() {
        // A `#stack` is a pure layout container (common in CV/resume entries):
        // a vertical stack lowers to its children in order, a horizontal one to
        // a borderless table row — keeping the text editable instead of
        // rasterizing the whole block.
        out.extend(mappers::stack::stack(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<OutlineElem>() {
        out.extend(mappers::outline::outline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EquationElem>() {
        // Block equation.
        match mappers::math::equation(elem, styles, ctx)? {
            mappers::math::EquationOut::Block(blocks) => out.extend(blocks),
            mappers::math::EquationOut::Inline(run) => {
                out.push(Block::Para(Para {
                    props: Default::default(),
                    content: vec![ParaChild::Run(run)],
                }));
            }
        }
    } else if let Some(elem) = child.to_packed::<FigureElem>() {
        // Figure: caption + body + cross-reference bookmark.
        out.extend(mappers::image::figure(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<QuoteElem>() {
        use typst_library::foundations::Resolve;
        let block = elem.block.get(styles);
        // Block quotes are padded 1em left and right (Typst's show-set default).
        let indent = block.then(|| {
            let em = crate::props::abs_to_twip(
                typst_library::layout::Em::new(1.0).resolve(styles),
            );
            crate::dom::Indent { left: Some(em), right: Some(em), ..Default::default() }
        });

        // Lower the body as blocks (not flat runs) so a multi-paragraph quote
        // stays multiple paragraphs instead of collapsing into one; style each as
        // a `Quote` paragraph and apply the block quote's left/right indent.
        let mut body_blocks = ctx.blocks(&elem.body, styles)?;
        for b in &mut body_blocks {
            if let Block::Para(para) = b {
                para.props.style.get_or_insert_with(|| "Quote".into());
                if let Some(ind) = &indent {
                    let pind = para.props.ind.get_or_insert_with(Default::default);
                    pind.left = Some(pind.left.unwrap_or(0) + ind.left.unwrap_or(0));
                    pind.right = Some(pind.right.unwrap_or(0) + ind.right.unwrap_or(0));
                }
            }
        }
        out.extend(body_blocks);

        // The attribution ("— author", or a prose citation) renders below a
        // block quote, right-aligned (Typst's default). Was previously dropped.
        if block
            && let Some(attribution) = elem.attribution.get_cloned(styles)
        {
            let realized = attribution.realize(elem.span());
            let attr_runs = ctx.inline_runs(&realized, styles, RunProps::default())?;
            if !attr_runs.is_empty() {
                out.push(Block::Para(Para {
                    props: crate::dom::ParaProps {
                        style: Some("Quote".into()),
                        ind: indent,
                        jc: Some(crate::dom::Jc::End),
                        ..Default::default()
                    },
                    content: attr_runs.into_iter().map(ParaChild::Run).collect(),
                }));
            }
        }
    } else if let Some(elem) = child.to_packed::<typst_library::visualize::LineElem>()
        && elem.end.get_ref(styles).is_none()
        && {
            // Horizontal `#line(length: ..)` (the common divider). Non-horizontal
            // or endpoint-defined lines fall through to the rasterization path.
            let deg = elem.angle.get(styles).to_deg().rem_euclid(180.0);
            deg < 1.0 || deg > 179.0
        }
    {
        use typst_library::visualize::Paint;
        // A horizontal rule → an empty paragraph with a bottom border (Word's
        // horizontal-rule idiom), instead of being dropped.
        let fx = elem.stroke.resolve(styles).unwrap_or_default();
        let color = match &fx.paint {
            Paint::Solid(c) => crate::props::color_to_hex(c),
            _ => [0, 0, 0],
        };
        let style = match &fx.dash {
            Some(pattern) if is_dotted(pattern) => "dotted",
            Some(_) => "dashed",
            None => "single",
        };
        let border = crate::dom::ParaBorder {
            style,
            sz: crate::props::pt_to_eighth_pt(fx.thickness.to_pt()),
            space: 4,
            color,
        };
        out.push(Block::Para(Para {
            props: crate::dom::ParaProps {
                pbdr: Some(crate::dom::ParaBorders {
                    bottom: Some(border),
                    ..Default::default()
                }),
                ..Default::default()
            },
            content: Vec::new(),
        }));
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PadElem>()
        && body_extractable(&elem.body)
    {
        // Keep `#pad(..)[body]` as real text, mapping horizontal padding to
        // paragraph indentation (vertical padding has no inline equivalent). A
        // body that is NOT extractable (it contains a per-line-labeled equation,
        // whose anchors only exist after layout) is not matched here and falls
        // through to the rasterization fallback, which preserves those anchors.
        use typst_library::foundations::Resolve;
        let left = crate::props::abs_to_twip(elem.left.get(styles).abs.resolve(styles));
        let right = crate::props::abs_to_twip(elem.right.get(styles).abs.resolve(styles));
        let mut blocks = ctx.blocks(&elem.body, styles)?;
        for b in &mut blocks {
            if let Block::Para(para) = b {
                let ind = para.props.ind.get_or_insert_with(Default::default);
                if left != 0 {
                    ind.left = Some(ind.left.unwrap_or(0) + left);
                }
                if right != 0 {
                    ind.right = Some(ind.right.unwrap_or(0) + right);
                }
            }
        }
        out.extend(blocks);
    } else if child.is::<typst_library::layout::ColbreakElem>() {
        // `#colbreak()` → a column break, moving the following content to the next
        // column of a multi-column section.
        out.push(Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(Run::ColumnBreak)],
        }));
    } else if child.is::<typst_library::layout::FlushElem>() {
        // A float-flush marker (`place` float ordering) has no DOCX equivalent and
        // carries no content of its own.
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PlaceElem>() {
        // Top-level `#place(..)` → a floating drawing (G8). The body is lowered
        // to an image (native or rasterized) wrapped in a `<wp:anchor>`.
        if let Some(block) = mappers::image::place(elem, styles, ctx)? {
            out.push(block);
        }
    } else if let Some(elem) = child.to_packed::<typst_library::layout::BlockElem>() {
        handle_block_box(ctx, elem, styles, out)?;
    } else if is_framed_container(child) && handle_block_framed(ctx, child, styles, out)? {
        // A block-level framed container (`#rect`/`#box`/`#square` standing as its
        // own block) with flowing content → shaded + bordered paragraphs that
        // break across pages, mirroring `#block`.
    } else if is_framed_container(child) && ctx.suppress_text_box {
        // A framed container in a *centered* context (a figure body): a centered
        // `wps:txbx` text box does not flow its text in LibreOffice. Rasterize the
        // box to an image instead — a centered inline image renders correctly in
        // every consumer (Word renders the text box fine, but this keeps both).
        if let Some(run) = mappers::image::laid_out_fallback(child, styles, ctx)? {
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(run)],
            }));
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    } else if is_framed_container(child) {
        // A short, single-line standalone framed container → a Word text box (a
        // sized, framed box). Standalone text boxes render correctly (an *inline*
        // one does not — that case is handled by run shading in `handle_inline`).
        // Not a text-box candidate (gradient fill, layout-bound body) → fall back
        // to the generic inline path so the content still survives.
        if let Some(run) = mappers::shape::text_box(child, styles, ctx)? {
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(run)],
            }));
        } else {
            let runs = ctx.inline_runs(child, styles, RunProps::default())?;
            if runs.is_empty() {
                ctx.warn_ignored(child.elem().name(), child.span());
            } else {
                out.push(Block::Para(Para {
                    props: ParaProps::default(),
                    content: runs.into_iter().map(ParaChild::Run).collect(),
                }));
            }
        }
    } else {
        // Fall back: treat anything else as inline content in a paragraph.
        let runs = ctx.inline_runs(child, styles, RunProps::default())?;
        if !runs.is_empty() {
            out.push(Block::Para(Para {
                props: Default::default(),
                content: runs.into_iter().map(ParaChild::Run).collect(),
            }));
        } else if let Some(run) = mappers::image::laid_out_fallback(child, styles, ctx)? {
            // A drawable block with no extractable text — a diagonal/endpoint
            // `#line`, `#polygon`, `#curve`, a `#layout`/`#stack`/`#move`/
            // `#rotate`/`#scale` body, … — rasterizes to an image so the visual
            // survives instead of being silently dropped.
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(run)],
            }));
        } else if !is_invisible_noop(child) {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    }
    Ok(())
}

/// Whether an element is an intentionally invisible no-op or pure layout
/// scaffolding that produces nothing visible — warning that it "was ignored" is
/// noise, since no content is lost (it renders nothing in the PDF either). This
/// is NOT for elements with real visual output that merely failed to map (a
/// shape, a drawn line): those keep their warning.
pub(crate) fn is_invisible_noop(child: &Content) -> bool {
    child.is::<ParbreakElem>()
        // Inter-block spacing (a leftover/fractional `#v`) has no flowing-document
        // equivalent, but it carries no content.
        || child.is::<typst_library::layout::VElem>()
        // `#hide[..]` is invisible by design (extractable bodies are kept as
        // hidden text elsewhere; a non-extractable one is genuinely nothing).
        || child.is::<typst_library::layout::HideElem>()
        // A float-flush marker (`place` float ordering): no DOCX equivalent.
        || child.is::<typst_library::layout::FlushElem>()
        // A tagged-PDF accessibility delimiter (unwrapped to its body elsewhere).
        || child.is::<typst_library::pdf::PdfMarkerTag>()
}

/// Lowers a raw [`BlockElem`] (`#block(..)` / `#rect(..)`-via-block), mapping
/// `fill:`/`stroke:`/`inset:` to paragraph shading + borders + indentation
/// (G5) and `above:`/`below:` to spacing (G4b) when the body is representable
/// as paragraphs. Gradient/tiling fills and layouter bodies fall through to the
/// rasterization fallback.
fn handle_block_box(
    ctx: &mut DocxCtx,
    elem: &typst_library::foundations::Packed<typst_library::layout::BlockElem>,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    use typst_library::foundations::Resolve;
    use typst_library::layout::{BlockBody, Spacing as TSpacing};
    use typst_library::visualize::Paint;

    let fill = elem.fill.get_cloned(styles);
    let stroke = elem.stroke.get_cloned(styles);
    let inset = elem.inset.get_cloned(styles);

    let solid_fill = matches!(&fill, Some(Paint::Solid(_)));
    // A gradient/tiling fill cannot be expressed as flat shading → rasterize.
    let representable = fill.is_none() || solid_fill;

    let body = elem.body.get_ref(styles);
    let content = match body {
        Some(BlockBody::Content(content)) if representable => content,
        _ => {
            // Layouter body OR gradient/tiling fill: rasterize the whole box.
            if let Some(run) =
                mappers::image::laid_out_fallback(elem.pack_ref(), styles, ctx)?
            {
                out.push(Block::Para(Para {
                    props: Default::default(),
                    content: vec![ParaChild::Run(run)],
                }));
            }
            return Ok(());
        }
    };

    // Recurse into the body to obtain its paragraphs.
    let mut inner = ctx.blocks(content, styles)?;

    // Resolve the box decorations once.
    let shd_fill = match &fill {
        Some(Paint::Solid(c)) => Some(crate::props::color_to_hex(c)),
        _ => None,
    };
    let pbdr = block_borders(&stroke, styles);

    // Inset: horizontal → left/right indent, vertical → before/after spacing.
    let ind_left = inset.left.and_then(|r| nonzero_twip(r, styles));
    let ind_right = inset.right.and_then(|r| nonzero_twip(r, styles));
    let inset_top = inset.top.and_then(|r| nonzero_twip(r, styles));
    let inset_bottom = inset.bottom.and_then(|r| nonzero_twip(r, styles));

    // Block above/below spacing (G4b). `Auto`/fractional → leave unset.
    let above = match elem.above.get(styles) {
        typst_library::foundations::Smart::Custom(TSpacing::Rel(rel)) => {
            Some(crate::props::abs_to_twip(rel.abs.resolve(styles)))
        }
        _ => None,
    };
    let below = match elem.below.get(styles) {
        typst_library::foundations::Smart::Custom(TSpacing::Rel(rel)) => {
            Some(crate::props::abs_to_twip(rel.abs.resolve(styles)))
        }
        _ => None,
    };

    stamp_box_decorations(
        &mut inner,
        shd_fill,
        &pbdr,
        Insets { left: ind_left, right: ind_right, top: inset_top, bottom: inset_bottom },
        above,
        below,
    );

    out.extend(inner);
    Ok(())
}

/// Resolved box insets in twips (each `None` when zero/absent).
#[derive(Default)]
struct Insets {
    left: Option<i32>,
    right: Option<i32>,
    top: Option<i32>,
    bottom: Option<i32>,
}

/// Stamps a box's shading, borders, insets and (optional) above/below spacing
/// onto its content paragraphs. Identical shading/borders on every paragraph
/// makes adjacent paragraphs inside one box render as a single visual box in
/// Word; `keep_lines`/`keep_next` hold a multi-paragraph box together.
fn stamp_box_decorations(
    inner: &mut [Block],
    shd_fill: Option<[u8; 3]>,
    pbdr: &Option<crate::dom::ParaBorders>,
    insets: Insets,
    above: Option<i32>,
    below: Option<i32>,
) {
    let has_box = shd_fill.is_some() || pbdr.is_some();
    let last = inner.len().saturating_sub(1);
    let para_count = inner.iter().filter(|b| matches!(b, Block::Para(_))).count();
    let multi_para = para_count > 1;

    let mut seen_para = 0usize;
    for (i, block) in inner.iter_mut().enumerate() {
        let Block::Para(para) = block else { continue };
        let p = &mut para.props;

        if let Some(f) = shd_fill {
            p.shd_fill.get_or_insert(f);
        }
        if let Some(b) = pbdr {
            if p.pbdr.is_none() {
                p.pbdr = Some(b.clone());
            }
        }
        if has_box && multi_para {
            p.keep_lines = true;
            if i != last {
                p.keep_next = true;
            }
        }

        // Horizontal inset → indent.
        if insets.left.is_some() || insets.right.is_some() {
            let ind = p.ind.get_or_insert_with(Default::default);
            if let Some(l) = insets.left {
                ind.left.get_or_insert(l);
            }
            if let Some(r) = insets.right {
                ind.right.get_or_insert(r);
            }
        }

        // Vertical inset + block above/below fold into the first/last para's
        // before/after spacing.
        let is_first = seen_para == 0;
        let is_last = seen_para == para_count.saturating_sub(1);
        let before = if is_first { sum_opt(above, insets.top) } else { None };
        let after = if is_last { sum_opt(below, insets.bottom) } else { None };
        if before.is_some() || after.is_some() {
            let sp = p.spacing.get_or_insert_with(Default::default);
            if let Some(b) = before {
                sp.before = Some(sp.before.unwrap_or(0) + b);
            }
            if let Some(a) = after {
                sp.after = Some(sp.after.unwrap_or(0) + a);
            }
        }
        seen_para += 1;
    }
}

/// Whether a native element is a framed container (`#box`/`#rect`/`#square`) that
/// might carry a body — the candidates for the block-level shaded-paragraph or
/// the inline text-box treatment.
fn is_framed_container(child: &Content) -> bool {
    use typst_library::layout::BoxElem;
    use typst_library::visualize::{RectElem, SquareElem};
    child.is::<BoxElem>() || child.is::<RectElem>() || child.is::<SquareElem>()
}

/// Maps a BLOCK-LEVEL framed container with *flowing* content to shaded +
/// bordered paragraphs (which break across pages), reusing the `#block`
/// decoration path. Returns `Ok(false)` — leaving the element for the inline
/// text-box path — when the container is better as a sized text box: a
/// gradient/tiling fill, no visible frame, or content that is just a single
/// inline line (a short label/badge).
fn handle_block_framed(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<bool> {
    use typst_library::visualize::Paint;

    let Some((body, fill, stroke_sides, inset)) = block_framed_parts(child, styles) else {
        return Ok(false);
    };
    // A gradient/tiling fill has no flat-shading form: keep it for the text-box /
    // rasterize path so the visual survives.
    if matches!(&fill, Some(p) if !matches!(p, Paint::Solid(_))) {
        return Ok(false);
    }
    // Layout-only introspection must rasterize; not a candidate for extraction.
    if !body_extractable(&body) {
        return Ok(false);
    }

    let shd_fill = match &fill {
        Some(Paint::Solid(c)) => Some(crate::props::color_to_hex(c)),
        _ => None,
    };
    let pbdr = block_borders(&stroke_sides, styles);
    // No visible frame → not a callout; let the caller handle it.
    if shd_fill.is_none() && pbdr.is_none() {
        return Ok(false);
    }
    // Only flowing/block content takes the main-story paragraph path; a short
    // single-line callout stays a (standalone, sized) text box — UNLESS the body
    // has a footnote (illegal in a text box) or content that is Word-fragile /
    // mis-laid inside one (figures, images, tables, math, nested frames), in
    // which case it must flow here to stay correct. Decided before extraction.
    if !body_is_flowing(&body, styles)
        && !body_has_footnote(&body)
        && body_textbox_safe(&body)
    {
        return Ok(false);
    }

    let mut inner = ctx.blocks(&body, styles)?;

    // Forward the content's introspection tags (cites/refs/labels inside the
    // callout must reach the introspector).
    crate::document::collect_tags(&inner, &mut ctx.deferred_tags);

    let insets = Insets {
        left: inset.left.and_then(|r| nonzero_twip(r, styles)),
        right: inset.right.and_then(|r| nonzero_twip(r, styles)),
        top: inset.top.and_then(|r| nonzero_twip(r, styles)),
        bottom: inset.bottom.and_then(|r| nonzero_twip(r, styles)),
    };
    stamp_box_decorations(&mut inner, shd_fill, &pbdr, insets, None, None);
    out.extend(inner);
    Ok(true)
}

/// Extracts `(body, fill, stroke-sides, inset)` from a framed container, or
/// `None` for a bodyless one. A `#rect`/`#square` carries a `Smart` per-side
/// stroke that defaults to a 1pt outline when unfilled; that default is
/// materialized here so the border path sees a concrete stroke.
#[allow(clippy::type_complexity)]
fn block_framed_parts(
    child: &Content,
    styles: typst_library::foundations::StyleChain,
) -> Option<(
    Content,
    Option<typst_library::visualize::Paint>,
    typst_library::layout::Sides<Option<Option<typst_library::visualize::Stroke>>>,
    typst_library::layout::Sides<Option<typst_library::layout::Rel<typst_library::layout::Length>>>,
)> {
    use typst_library::layout::BoxElem;
    use typst_library::visualize::{RectElem, SquareElem};

    if let Some(e) = child.to_packed::<BoxElem>() {
        let body = e.body.get_cloned(styles)?;
        Some((body, e.fill.get_cloned(styles), e.stroke.get_cloned(styles), e.inset.get_cloned(styles)))
    } else if let Some(e) = child.to_packed::<RectElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        let stroke = shape_stroke_sides(e.stroke.get_cloned(styles), &fill);
        Some((body, fill, stroke, e.inset.get_cloned(styles)))
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        let stroke = shape_stroke_sides(e.stroke.get_cloned(styles), &fill);
        Some((body, fill, stroke, e.inset.get_cloned(styles)))
    } else {
        None
    }
}

/// Resolves a `#rect`/`#square`'s `Smart` stroke: `Auto` becomes the Typst
/// default — a 1pt outline on every side when unfilled, no border when filled.
fn shape_stroke_sides(
    stroke: typst_library::foundations::Smart<
        typst_library::layout::Sides<Option<Option<typst_library::visualize::Stroke>>>,
    >,
    fill: &Option<typst_library::visualize::Paint>,
) -> typst_library::layout::Sides<Option<Option<typst_library::visualize::Stroke>>> {
    use typst_library::foundations::Smart;
    use typst_library::layout::Sides;
    match stroke {
        Smart::Custom(sides) => sides,
        Smart::Auto if fill.is_some() => Sides::splat(None),
        Smart::Auto => Sides::splat(Some(Some(typst_library::visualize::Stroke::default()))),
    }
}

/// Whether a framed container's body is *flowing* block content that should
/// break across pages (a multi-paragraph callout, a code listing, a list, a
/// table) rather than a short inline label. Checked on the raw body (no
/// extraction): flowing iff it contains a paragraph break, a block raw listing,
/// a list/enum/term list, a table/grid, or a nested block.
fn body_is_flowing(body: &Content, styles: typst_library::foundations::StyleChain) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::{BlockElem, GridElem};
    use typst_library::model::{EnumElem, ListElem, TableElem, TermsElem};
    use typst_library::text::RawElem;
    body.traverse(&mut |e: Content| {
        let flowing = e.is::<ParbreakElem>()
            || e.is::<ListElem>()
            || e.is::<EnumElem>()
            || e.is::<TermsElem>()
            || e.is::<TableElem>()
            || e.is::<GridElem>()
            || e.is::<BlockElem>()
            || e.to_packed::<RawElem>().is_some_and(|r| r.block.get(styles));
        if flowing { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_break()
}

/// Sums two optional twip values, returning `None` only when both are absent.
fn sum_opt(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// Resolves a relative inset to twips, returning `None` when zero.
fn nonzero_twip(
    rel: typst_library::layout::Rel<typst_library::layout::Length>,
    styles: typst_library::foundations::StyleChain,
) -> Option<i32> {
    use typst_library::foundations::Resolve;
    let twips = crate::props::abs_to_twip(rel.abs.resolve(styles));
    (twips != 0).then_some(twips)
}

/// Maps a block's stroke sides to paragraph borders, or `None` when no side is
/// stroked.
fn block_borders(
    stroke: &typst_library::layout::Sides<
        Option<Option<typst_library::visualize::Stroke>>,
    >,
    styles: typst_library::foundations::StyleChain,
) -> Option<crate::dom::ParaBorders> {
    use typst_library::foundations::Resolve;

    let side = |s: &Option<Option<typst_library::visualize::Stroke>>| {
        let stroke = s.clone().flatten()?;
        let fx = stroke.resolve(styles).unwrap_or_default();
        let color = match &fx.paint {
            typst_library::visualize::Paint::Solid(c) => crate::props::color_to_hex(c),
            _ => [0, 0, 0],
        };
        let style = match &fx.dash {
            Some(pattern) if is_dotted(pattern) => "dotted",
            Some(_) => "dashed",
            None => "single",
        };
        Some(crate::dom::ParaBorder {
            style,
            sz: crate::props::pt_to_eighth_pt(fx.thickness.to_pt()),
            space: 4,
            color,
        })
    };

    let borders = crate::dom::ParaBorders {
        top: side(&stroke.top),
        left: side(&stroke.left),
        bottom: side(&stroke.bottom),
        right: side(&stroke.right),
    };
    (!borders.is_empty()).then_some(borders)
}

/// Heuristic: a dash pattern whose first segment is a single line-width dot
/// renders as a dotted border.
fn is_dotted(
    pattern: &typst_library::visualize::DashPattern<
        typst_library::layout::Abs,
        typst_library::layout::Abs,
    >,
) -> bool {
    matches!(pattern.array.first(), Some(len) if len.to_pt() <= 1.0)
}

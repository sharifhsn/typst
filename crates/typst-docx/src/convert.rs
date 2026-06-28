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
            inline_children(ctx, &par.body, *styles, &mut pending)?;
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
    if let Some(elem) = child.to_packed::<TagElem>() {
        out.push(Block::Tag(elem.tag.clone()));
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

        let runs = ctx.inline_runs(&elem.body, styles, RunProps::default())?;
        out.push(Block::Para(Para {
            props: crate::dom::ParaProps {
                style: Some("Quote".into()),
                ind: indent.clone(),
                ..Default::default()
            },
            content: runs.into_iter().map(ParaChild::Run).collect(),
        }));

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
    } else if child.is::<typst_library::layout::FlushElem>()
        || child.is::<typst_library::layout::ColbreakElem>()
    {
        // Float-flush / column-break markers: no DOCX representation, no content.
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
        // break across pages, mirroring `#block`. Short / single-line content
        // returns `false` and falls through to the inline text-box path below
        // (a sized text box reads better for a badge/label than a full-width box).
    } else {
        // Fall back: treat anything else as inline content in a paragraph.
        let runs = ctx.inline_runs(child, styles, RunProps::default())?;
        if !runs.is_empty() {
            out.push(Block::Para(Para {
                props: Default::default(),
                content: runs.into_iter().map(ParaChild::Run).collect(),
            }));
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    }
    Ok(())
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

    // Only flowing/block content benefits from a paragraph representation; a
    // single inline line stays a (sized) text box — UNLESS the body has a
    // footnote, which is illegal inside a text box, so it must take the
    // main-story paragraph path here to keep both the frame and the footnote.
    let mut inner = ctx.blocks(&body, styles)?;
    if !is_flowing(&inner) && !body_has_footnote(&body) {
        return Ok(false);
    }

    let shd_fill = match &fill {
        Some(Paint::Solid(c)) => Some(crate::props::color_to_hex(c)),
        _ => None,
    };
    let pbdr = block_borders(&stroke_sides, styles);
    // No visible frame → not a callout; let the inline path handle it.
    if shd_fill.is_none() && pbdr.is_none() {
        return Ok(false);
    }

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

/// Whether extracted content should *flow* (break across pages) — true for
/// multi-block content, a table, or a paragraph holding explicit line breaks (a
/// raw code block). A single breakless paragraph (a short label) is not flowing.
fn is_flowing(blocks: &[Block]) -> bool {
    match blocks {
        [] => false,
        [Block::Para(p)] => p
            .content
            .iter()
            .any(|c| matches!(c, ParaChild::Run(Run::Break | Run::PageBreak))),
        _ => true,
    }
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

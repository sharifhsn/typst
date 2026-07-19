//! The top-level recursion entry and the native-element block dispatch.

use typst_library::diag::{At, SourceResult};
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::TagElem;
use typst_library::math::EquationElem;
use typst_library::model::{
    EnumElem, FigureElem, HeadingElem, ListElem, OutlineElem, ParElem, ParbreakElem,
    QuoteElem, TableCell, TableElem, TermsElem, TitleElem,
};
use typst_library::routines::Pair;

use crate::ctx::{DocxCtx, box_is_plain};
use crate::dom::{
    Block, Cell, CellBorders, Para, ParaChild, ParaProps, ReviewCandidateKind, Row,
    RowHeight, Run, RunProps, Spacing, Tbl, TblProps, VAlign,
};
use crate::mappers;
use crate::report::{DecisionReason, LossSet, Representation};

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
pub fn convert_children(
    ctx: &mut DocxCtx,
    children: &[Pair],
) -> SourceResult<Vec<Block>> {
    use typst_library::foundations::Resolve;
    use typst_library::layout::VElem;

    let mut blocks = Vec::new();
    // Buffered paragraph children currently being assembled, plus its
    // resolved paragraph properties (from the first `ParElem` seen).
    let mut pending: Vec<ParaChild> = Vec::new();
    let mut pending_page_markers: Vec<ParaChild> = Vec::new();
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
    // Whether `pending` currently holds nothing but a lone orphaned
    // `SpaceElem` — the single space realize leaves behind when a paragraph
    // is split around a promoted-to-block child (e.g. an inline equation
    // with a `show math.equation.where(block: true)` rule, or any block
    // sitting directly between two words with no other whitespace). Real
    // Typst layout treats that whitespace as insignificant at a block
    // boundary; naively flushing it here instead emits a standalone
    // one-space paragraph — a spurious blank line, repeated at every such
    // boundary, that can substantially inflate page count in text dense
    // with promoted-block content. Cleared as soon as `pending` gains any
    // other content, so a real paragraph that merely *starts* with a space
    // is unaffected.
    let mut pending_orphaned_whitespace = false;

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
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            last_was_par = false;
            continue;
        }
        if let Some(par) = child.to_packed::<ParElem>() {
            // A bare inline-level framed container (`#box`/`#rect`/`#square`)
            // sitting alone at block scope gets paragraph-wrapped by Typst's
            // own realize (there is no bare-inline-content block variant) —
            // it would otherwise be forced through the run-only inline path
            // in `handle_inline`, which structurally cannot carry
            // multi-paragraph block content or a non-uniform per-side stroke
            // (see `paragraph_sole_block_container`'s doc comment for both
            // regressions this fixes). Detect that narrow, validated-safe
            // shape and route it through the full block dispatch instead —
            // unless it's a link target (labels on a redirected block aren't
            // bookmarked below).
            if child.label().is_none()
                && let Some(sole) = paragraph_sole_block_container(&par.body, *styles)
            {
                let from = blocks.len();
                flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
                pending_v = apply_pending_v(&mut blocks, from, pending_v);
                handle_block(ctx, sole, *styles, &mut blocks)?;
                last_was_par = false;
                continue;
            }
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
                flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
                pending_v = apply_pending_v(&mut blocks, from, pending_v);
                // Typst's default `first-line-indent` (`all: false`) indents a
                // paragraph only when it directly follows another; Word's
                // `w:firstLine` has no such rule, so apply it here. Checked
                // *after* the flush so `blocks.last()` is the previous paragraph.
                let prev_was_para = matches!(blocks.last(), Some(Block::Para(_)));
                let mut props = ctx.resolve_par_props(par, *styles);
                props.review_origin = Some(ctx.review_origin(
                    review_span(&par.body),
                    ReviewCandidateKind::Paragraph,
                ));
                if prev_was_para
                    && props.ind.as_ref().and_then(|i| i.first_line).is_none()
                    && let Some(amount) = ctx.consecutive_first_line_indent(*styles)
                {
                    props.ind.get_or_insert_with(Default::default).first_line =
                        Some(amount);
                }
                pending_props = Some(props);
            }
            let par_start = pending.len();
            inline_children(ctx, &par.body, *styles, &mut pending)?;
            if !pending_page_markers.is_empty() {
                pending.splice(par_start..par_start, pending_page_markers.drain(..));
            }
            // A labeled paragraph (`text … <spot>`) is a valid `#link(<spot>)`
            // target, so bracket its content with a bookmark (otherwise the link
            // anchor is dangling).
            if child.label().is_some()
                && let Some(loc) = child.location()
                && let Some((id, name)) = ctx.bookmark_for_emission(loc)
            {
                pending.insert(par_start, ParaChild::BookmarkStart { id, name });
                pending.push(ParaChild::BookmarkEnd { id });
            }
            pending_orphaned_whitespace = false;
            have_pending = true;
            last_was_par = true;
        } else if let Some(elem) = child.to_packed::<TagElem>() {
            // Introspection tag: record as a block-level tag (kept for the
            // introspector). Transparent — does not change paragraph structure.
            if !have_pending {
                if let typst_library::introspection::Tag::Start(content, _) = &elem.tag
                    && !content.is::<typst_library::model::LinkElem>()
                    && let Some(loc) = content.location()
                    && let Some((id, name)) = ctx.page_bookmark_for_emission(loc)
                {
                    pending_page_markers.push(ParaChild::BookmarkStart { id, name });
                    pending_page_markers.push(ParaChild::BookmarkEnd { id });
                }
                blocks.push(Block::Tag(elem.tag.clone()));
            } else {
                if let typst_library::introspection::Tag::Start(content, _) = &elem.tag
                    && !content.is::<typst_library::model::LinkElem>()
                    && let Some(loc) = content.location()
                    && let Some((id, name)) = ctx.page_bookmark_for_emission(loc)
                {
                    pending.push(ParaChild::BookmarkStart { id, name });
                    pending.push(ParaChild::BookmarkEnd { id });
                }
                pending.push(ParaChild::Tag(elem.tag.clone()));
            }
        } else if let Some(elem) = child.to_packed::<VElem>() {
            // Vertical spacing (G4c): fold into the next paragraph's `before`
            // rather than dropping it. Fractional spacing has no fixed twip
            // value, so it is dropped.
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
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
            if let Some(props) = pending_props.as_mut() {
                props.review_origin = None;
            }
            pending_orphaned_whitespace = false;
            have_pending = true;
            last_was_par = false;
        } else if let Some(raw) = child.to_packed::<typst_library::text::RawElem>()
            && !raw.block.get(*styles)
        {
            // A surviving inline raw element must stay in the current paragraph;
            // `handle_inline` enters raw scope before re-realizing it.
            push_inline(ctx, child, *styles, &mut pending)?;
            if let Some(props) = pending_props.as_mut() {
                props.review_origin = None;
            }
            pending_orphaned_whitespace = false;
            have_pending = true;
            last_was_par = false;
        } else if let Some(boxed) = child.to_packed::<typst_library::layout::BoxElem>()
            && boxed
                .body
                .get_ref(*styles)
                .as_ref()
                .is_none_or(body_inline_extractable)
        {
            // Same split-paragraph problem as inline math/raw above, for a
            // `#box` — commonly the output of `show raw.where(block: false)`
            // wrapping an inline code span in a shaded pill. `#box` has no
            // block-level variant (unlike raw/equation, it cannot be authored
            // as block content), so realize can only split a paragraph around
            // one, never emit a *genuinely* standalone bare box here: Typst's
            // own realize always paragraph-wraps solitary inline content
            // (see `paragraph_sole_block_container` above, which already
            // redirects that ParElem-wrapped case to the block dispatch).
            // Reaching this loop as a bare, unwrapped child therefore always
            // means realize split a sentence around it, and it must stay in
            // the current paragraph — `handle_inline`'s existing box dispatch
            // (shape / shaded run / plain extraction) already does the right
            // thing once the box arrives inline instead of being flushed to
            // `handle_block`'s standalone-text-box path, whose Word `wps:txbx`
            // does not flow inline and breaks the sentence mid-word. Guarded
            // to `body_inline_extractable` bodies (bodyless, or no block-flow
            // element inside) so a `#layout(..)`-produced box wrapping a real
            // grid/figure/table — which needs `handle_block`'s dedicated
            // wrap-content-figure/mixed-canvas recovery, not plain inline
            // extraction — keeps taking the existing block dispatch below.
            push_inline(ctx, child, *styles, &mut pending)?;
            pending_orphaned_whitespace = false;
            have_pending = true;
            last_was_par = false;
        } else if is_inline(child) {
            if !have_pending && pending.is_empty() {
                pending_orphaned_whitespace = child.is::<typst_library::text::SpaceElem>();
                let props = ParaProps {
                    review_origin: Some(
                        ctx.review_origin(child.span(), ReviewCandidateKind::Paragraph),
                    ),
                    ..Default::default()
                };
                pending_props = Some(props);
            } else if !child.is::<typst_library::text::SpaceElem>() {
                pending_orphaned_whitespace = false;
            }
            push_inline(ctx, child, *styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else {
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
            handle_block(ctx, child, *styles, &mut blocks)?;
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            last_was_par = false;
        }
    }
    let from = blocks.len();
    flush(&mut pending, &mut pending_props, &mut have_pending, &mut pending_orphaned_whitespace, &mut blocks);
    apply_pending_v(&mut blocks, from, pending_v);

    // Drop a *trailing* pagebreak-only paragraph: a document closing a `set page`
    // run leaves a page-boundary break after the last content which, as `<w:br>`,
    // would add a blank final page (the symmetric case to the leading break
    // dropped above). A real break is always followed by content, so this only
    // ever removes the spurious closing one.
    while let Some(Block::Para(para)) = blocks.last() {
        if !para.content.is_empty()
            && para
                .content
                .iter()
                .all(|c| matches!(c, ParaChild::Run(Run::PageBreak)))
        {
            blocks.pop();
        } else {
            break;
        }
    }

    // A run-level page break is relative to the preceding content. If a tall
    // block or table has already auto-paginated, the break lands at the top of
    // the next physical page and advances once more, manufacturing a blank
    // page. Move a single boundary onto the following paragraph as Word's
    // semantic `<w:pageBreakBefore/>`, which is idempotent when that paragraph
    // already starts a page. Preserve additional consecutive breaks as explicit
    // runs so intentionally requested blank pages survive.
    move_page_breaks_before_following_blocks(&mut blocks);

    // A paragraph using a fractional `#h(1fr)` (a fill-tab) gets a right-aligned
    // tab stop at the content width, so the tab pushes the following content to
    // the right margin (the "Left … Right" header idiom) instead of stopping at
    // the next default tab stop.
    let content_twips = ctx.available_width_dxa();
    fold_right_aligned_heading_overlays(&mut blocks, content_twips);
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

fn move_page_breaks_before_following_blocks(blocks: &mut Vec<Block>) {
    let mut out = Vec::with_capacity(blocks.len());
    let mut pending = 0usize;
    let mut pending_weak = false;
    let mut pending_flow_dxa = 0i32;
    for mut block in std::mem::take(blocks) {
        let is_break = matches!(
            &block,
            Block::Para(para)
                if !para.content.is_empty()
                    && para.content.iter().all(|child| {
                        matches!(child, ParaChild::Run(Run::PageBreak))
                    })
        );
        if is_break {
            pending += 1;
            continue;
        }
        if matches!(block, Block::WeakPageBreak) {
            // Weak breaks collapse: any run of them contributes at most one
            // idempotent boundary, and one stacked onto hard breaks adds
            // nothing (the page is already fresh after a hard break). Only a
            // weak break *opening* the run matters — it fires before the hard
            // breaks do, adding one transition when content precedes.
            pending_weak |= pending == 0;
            continue;
        }
        if pending > 0 || pending_weak {
            if matches!(block, Block::Tag(_)) {
                out.push(block);
                continue;
            }
            if let Block::FlowSpace { dxa } = block {
                pending_flow_dxa = pending_flow_dxa.saturating_add(dxa);
                continue;
            }
            if let Block::Para(para) = &mut block {
                // A single hard break becomes idempotent pageBreakBefore. For
                // two or more authored hard breaks, keep every explicit break
                // as well: an explicit break followed by pageBreakBefore is
                // idempotent at the top of a page, so N explicit breaks
                // preserve N-1 blank pages while the paragraph property
                // protects auto-pagination. Weak breaks never emit an
                // explicit `<w:br>` themselves — the flag alone reproduces
                // their only-if-content-precedes semantics. A weak break
                // *followed by* hard breaks fires first (one extra transition
                // when content precedes): the flag rides on the first
                // explicit-break paragraph so the whole run keeps Typst's
                // count both mid-page and at a page top.
                if pending > 1 || (pending > 0 && pending_weak) {
                    for index in 0..pending {
                        let mut break_block = page_break_block();
                        if index == 0
                            && pending_weak
                            && let Block::Para(break_para) = &mut break_block
                        {
                            break_para.props.page_break_before = true;
                        }
                        out.push(break_block);
                    }
                }
                para.props.page_break_before = true;
                if pending_flow_dxa != 0 {
                    let spacing = para.props.spacing.get_or_insert_with(Spacing::default);
                    spacing.before = Some(
                        spacing.before.unwrap_or(0).saturating_add(pending_flow_dxa),
                    );
                }
            } else if pending > 0 {
                // Tables and section boundaries have no paragraph property on
                // which to carry the break without adding an empty line. Keep
                // their boundaries explicit rather than changing flow height.
                for _ in 0..pending {
                    out.push(page_break_block());
                }
                if pending_flow_dxa != 0 {
                    out.push(Block::FlowSpace { dxa: pending_flow_dxa });
                }
            } else {
                // A weak break in front of a table (Word ignores
                // `pageBreakBefore` inside table cells) or a section break
                // rides on a minimized empty paragraph carrying the flag:
                // still a no-op at a page top, at the cost of one ~twip line.
                // In front of a section break the carrier matters — a weak
                // break with content behind it fires *before* the section
                // transition does (thesis chapters lose one page start per
                // chapter without it), while at a page top it stays a no-op.
                out.push(weak_page_break_carrier_block(pending_flow_dxa));
            }
            pending = 0;
            pending_weak = false;
            pending_flow_dxa = 0;
        }
        out.push(block);
    }
    // Trailing hard breaks stay (an intentionally requested final blank page);
    // a trailing weak break is Typst's own no-op and is dropped.
    for _ in 0..pending {
        out.push(page_break_block());
    }
    if pending_flow_dxa != 0 {
        out.push(Block::FlowSpace { dxa: pending_flow_dxa });
    }
    *blocks = out;
}

/// A minimized empty paragraph whose only job is to carry `pageBreakBefore`
/// in front of a block that cannot carry it itself (a table). Costs one
/// exact-height twip of flow, well inside consumer drift.
fn weak_page_break_carrier_block(extra_before_dxa: i32) -> Block {
    Block::Para(Para {
        props: ParaProps {
            page_break_before: true,
            spacing: Some(Spacing {
                before: Some(extra_before_dxa.max(0)),
                after: Some(0),
                line: Some(1),
                line_rule_auto: false,
                line_rule_at_least: false,
            }),
            ..ParaProps::default()
        },
        content: Vec::new(),
    })
}

fn page_break_block() -> Block {
    Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::PageBreak)],
    })
}

/// Folds an accumulated `#v(..)` spacing into the `before` of the first
/// paragraph produced at/after `from`. Returns the residual (0 if applied, or
/// the unchanged amount if no paragraph was found to carry it).
fn apply_pending_v(blocks: &mut Vec<Block>, from: usize, pending_v: i32) -> i32 {
    if pending_v == 0 {
        return 0;
    }
    for index in from..blocks.len() {
        match &mut blocks[index] {
            Block::Tag(_) | Block::SectionBreak(_) | Block::WeakPageBreak => continue,
            Block::Para(para) => {
                let sp = para.props.spacing.get_or_insert_with(Default::default);
                sp.before = Some(sp.before.unwrap_or(0) + pending_v);
                return 0;
            }
            Block::FlowSpace { dxa } => {
                *dxa += pending_v;
                return 0;
            }
            Block::Table(_) | Block::Toc(_) => {
                blocks.insert(index, Block::FlowSpace { dxa: pending_v });
                return 0;
            }
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
    orphaned_whitespace: &mut bool,
    blocks: &mut Vec<Block>,
) {
    if !*have_pending && pending.is_empty() {
        *props = None;
        return;
    }
    // A lone space realize left behind at a block boundary (see
    // `pending_orphaned_whitespace`'s doc comment) has no visible effect in
    // real Typst layout — drop it instead of emitting a spurious blank
    // paragraph.
    if std::mem::take(orphaned_whitespace) {
        pending.clear();
        *props = None;
        *have_pending = false;
        return;
    }
    let content = std::mem::take(pending);
    let props = props.take().unwrap_or_default();
    blocks.push(Block::Para(Para { props, content }));
    *have_pending = false;
}

pub(crate) fn review_span(content: &Content) -> typst_syntax::Span {
    use std::ops::ControlFlow;

    let mut span = content.span();
    if !span.is_detached() {
        return span;
    }
    let _ = content.traverse(&mut |child: Content| {
        if child.span().is_detached() {
            ControlFlow::Continue(())
        } else {
            span = child.span();
            ControlFlow::Break(())
        }
    });
    span
}

/// Whether a native element is inline-level (formatting/text/refs/etc.).
///
/// `TagElem` and `EquationElem` are intentionally treated as block-level here so
/// that the block dispatch records introspection tags and routes equations
/// (which can be inline or block) through the math mapper.
fn is_inline(child: &Content) -> bool {
    use typst_library::introspection::CounterDisplayElem;
    use typst_library::layout::{HElem, HideElem};
    use typst_library::model::{EmphElem, LinkElem, RefElem, StrongElem};
    use typst_library::text::{
        HighlightElem, LinebreakElem, SmallcapsElem, SmartQuoteElem, SpaceElem,
        StrikeElem, SubElem, SuperElem, TextElem, UnderlineElem,
    };
    use typst_library::visualize::ImageElem;

    child.is::<CounterDisplayElem>()
        || child.is::<TextElem>()
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
        // `#hide[..]` has zero visual footprint by definition (Typst's own
        // paged export drops every one of its frame items but tags) and
        // `handle_inline` already lowers it to nothing but harvested tags —
        // but reaching this function at all means it sits at TOP level, not
        // yet inside a paragraph. Without this arm it fell to the generic
        // block dispatch, which flushes the paragraph being buffered before
        // and after it. A zero-width kerning idiom that wraps every inline
        // math/punctuation boundary in `hide(..)` calls (the `cjk-spacer`
        // package's "ghost width" trick, used to fix Latin/CJK spacing) then
        // fragments an otherwise-ordinary sentence into one paragraph per
        // word — observed inflating a 6-page document to 39 rendered pages.
        || child.is::<HideElem>()
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

/// Whether a `#box` body is purely inline-level flow that the inline run
/// pipeline can carry faithfully — so the box can be unwrapped to keep its text
/// selectable rather than rasterized.
///
/// A box wrapping *block-level* content (a figure with a caption + counter, a
/// list, a table/grid, a stack, a heading) must NOT be unwrapped inline:
/// inline lowering would drop the block structure (and, for a figure, its
/// caption and `SEQ` counter step — drifting cross-reference numbers). Such a
/// box keeps the rasterize path, which preserves the visual and emits the
/// hidden figure-counter step. Inline images, shapes and inline equations are
/// fine and stay extractable.
pub(crate) fn body_inline_extractable(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::{ColumnsElem, GridElem, StackElem};
    use typst_library::model::{
        EnumElem, FigureElem, HeadingElem, ListElem, OutlineElem, TableElem, TermsElem,
    };
    body.traverse(&mut |e: Content| {
        let is_block_flow = e.is::<FigureElem>()
            || e.is::<TableElem>()
            || e.is::<GridElem>()
            || e.is::<StackElem>()
            || e.is::<ColumnsElem>()
            || e.is::<ListElem>()
            || e.is::<EnumElem>()
            || e.is::<TermsElem>()
            || e.is::<HeadingElem>()
            || e.is::<OutlineElem>();
        if is_block_flow { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_continue()
}

/// Whether a frameless box body is a *wrap-content figure*: a `#grid` that
/// holds a `#figure`. This is the `wrap-it`/`wrap-content` shape
/// (`box(grid(figure, text))`), the one frameless-box case worth lowering
/// natively — narrow on purpose so a designed full-page layout box (which can
/// lose content through a native re-walk) is left to rasterize.
pub(crate) fn body_is_wrap_figure(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::GridElem;
    use typst_library::model::FigureElem;
    let mut has_grid = false;
    let mut has_figure = false;
    let _ = body.traverse(&mut |e: Content| {
        if e.is::<GridElem>() {
            has_grid = true;
        }
        if e.is::<FigureElem>() {
            has_figure = true;
        }
        if has_grid && has_figure {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    has_grid && has_figure
}

/// Peels transparent `#set`-style and pure single-child join wrappers
/// (`StyledElem`, a `SequenceElem` with exactly one non-trivial child) off a
/// content value, returning whatever is structurally underneath. Used to see
/// through the styling/joining Typst's own realization wraps around a bare
/// expression so the *actual* element can be identified.
fn peel_wrappers(mut body: &Content) -> &Content {
    use typst_library::foundations::{SequenceElem, StyledElem};
    use typst_library::introspection::TagElem;
    use typst_library::model::ParbreakElem;
    use typst_library::text::SpaceElem;

    loop {
        if let Some(styled) = body.to_packed::<StyledElem>() {
            body = &styled.child;
            continue;
        }
        if let Some(seq) = body.to_packed::<SequenceElem>() {
            let mut rest = seq.children.iter().filter(|c| {
                !c.is::<SpaceElem>() && !c.is::<ParbreakElem>() && !c.is::<TagElem>()
            });
            if let (Some(only), None) = (rest.next(), rest.next()) {
                body = only;
                continue;
            }
        }
        return body;
    }
}

/// Whether a frameless box/rect/square body IS (after [`peel_wrappers`])
/// directly one of the ordinary flowing block containers — `#columns`,
/// `#stack`, or a non-figure `#grid` — rather than some more elaborate
/// composition.
///
/// Deliberately narrow, mirroring [`body_is_wrap_figure`]'s "one specific
/// shape, not just contains X anywhere" contract: when a frameless box's
/// *entire* content collapses to one of these, flattening it via
/// `ctx.blocks` can only ever lose the (single-column-approximated) column
/// split — never introspection-order-dependent content buried inside a more
/// elaborate composition. That broader case is the "designed full-page
/// layout box" that a wholesale frameless-box unwrap previously regressed by
/// -148 words (ca8ce358d); this signature stays clear of it by requiring the
/// container to BE the whole body, not merely present somewhere inside it.
pub(crate) fn body_is_frameless_flow_container(body: &Content) -> bool {
    use typst_library::layout::{ColumnsElem, GridElem, StackElem};

    let inner = peel_wrappers(body);
    if inner.is::<ColumnsElem>() || inner.is::<StackElem>() {
        return true;
    }
    if inner.is::<GridElem>() {
        // A grid-of-figure is the `wrap-content` shape, already handled by
        // `body_is_wrap_figure` via the well-tested grid→figure/table mapper;
        // keep that gate separate rather than double-widening here.
        return !body_is_wrap_figure(inner);
    }
    false
}

/// Whether a `ParElem`'s body reduces, after [`peel_wrappers`], to a SOLE
/// framed container (`#box`/`#rect`/`#square`) that [`handle_block_framed`]
/// would render MORE faithfully than the run-only inline path can — i.e.
/// Typst paragraph-wrapped a bare inline-level container at block scope
/// (there is no bare-inline-content block variant) purely because it's
/// nominally inline, not because it sits alongside real running text. Two
/// narrow, independent triggers, each because the run-only inline path
/// structurally cannot represent the case correctly:
/// - the body is directly one ordinary flowing container (`body_is_wrap_
///   figure` / `body_is_frameless_flow_container`) — `handle_inline` has no
///   way to carry multi-paragraph block content, so e.g. `box(inset:
///   ..)[#columns(2, ..)]` rasterized an entire two-column A0 poster as one
///   page-spanning image (~26 near-blank pages; the pollux poster template);
/// - the stroke is non-uniform across sides (`stroke_sides_nonuniform`) —
///   the inline path's only bordered-run form (`w:bdr`, via `mappers::
///   shape::inline_frame`) is inherently a uniform box, so e.g. `box(stroke:
///   (bottom: ..))` (a common "border as a section-title underline" idiom in
///   CV/resume templates) silently became a full four-sided box.
/// - the framed body itself contains flowing/nested block content. Character
///   borders repeat around every wrapped run, turning a callout into a stack of
///   boxed text strips; the block path instead keeps one editable, breakable
///   paragraph container. Genuine mid-sentence framed boxes remain inline.
///
/// Mirrors `handle_block_framed`'s own acceptance tests exactly so a
/// redirect here only ever routes to a call it would have accepted anyway.
fn paragraph_sole_block_container<'a>(
    body: &'a Content,
    styles: typst_library::foundations::StyleChain,
) -> Option<&'a Content> {
    let inner = peel_wrappers(body);
    if !is_framed_container(inner) {
        return None;
    }
    let (fbody, fill, stroke_sides, _inset) = block_framed_parts(inner, styles)?;
    if !body_extractable(&fbody) {
        return None;
    }
    let has_border = block_borders(&stroke_sides, styles).is_some();
    if body_is_flowing(&fbody, styles) && (fill.is_some() || has_border) {
        return Some(inner);
    }
    if stroke_sides_nonuniform(&stroke_sides) {
        return Some(inner);
    }
    if fill.is_some() {
        return None;
    }
    if has_border {
        return None;
    }
    (body_is_wrap_figure(&fbody) || body_is_frameless_flow_container(&fbody))
        .then_some(inner)
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
            if is_label { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
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
    let deferred_before = ctx.deferred_tags.len();
    handle_block_inner(ctx, child, styles, out)?;
    let local_tags: Vec<_> = ctx.deferred_tags.drain(deferred_before..).collect();
    let local_tag_count = local_tags.len();
    out.splice(block_start..block_start, local_tags.into_iter().map(Block::Tag));
    let block_start = block_start + local_tag_count;

    // Bookmark a LABELED block (a numbered equation `$…$ <eq>`, a labeled list,
    // …) so a `@ref`/`#link` to it resolves to a real target. Headings and
    // figures emit their own bookmark, so skip them to avoid a duplicate name.
    if child.label().is_some()
        && let Some(loc) = child.location()
        && !child.is::<HeadingElem>()
        && !child.is::<FigureElem>()
    {
        let blocks = &mut out[block_start..];
        if blocks.iter().any(|block| matches!(block, Block::Para(_)))
            && let Some((id, name)) = ctx.bookmark_for_emission(loc)
        {
            bracket_bookmark(blocks, id, name);
        }
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
    } else if child.is::<typst_library::text::RawElem>() {
        out.extend(ctx.with_raw_scope(|ctx| ctx.blocks(child, styles))?);
    } else if let Some(elem) = child.to_packed::<typst_library::pdf::PdfMarkerTag>() {
        // A PDF accessibility delimiter wraps real content (`body`); it has no DOCX
        // meaning itself, so unwrap it and lower the body (otherwise the wrapped
        // content — a whole figure, list, paragraph — is dropped).
        out.extend(ctx.blocks(&elem.body, styles)?);
    } else if let Some(elem) = child.to_packed::<typst_library::pdf::ArtifactElem>() {
        // `#pdf.artifact[..]` marks content as decorative for PDF accessibility
        // (a repeated logo, a code listing's line-number gutter, a grid cell
        // wrapping one of these, …) — same idea as `PdfMarkerTag` above: DOCX has
        // no artifact concept, so unwrap and lower the body natively instead of
        // rasterizing the whole marked region.
        out.extend(ctx.blocks(&elem.body, styles)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::GridCell>() {
        // A bare `grid.cell(..)` reached as ordinary content — not via the
        // table mapper's own cell extraction (`mappers::table::cell_blocks`),
        // which unwraps a *resolved* grid entry's `GridCell` wrapper directly.
        // This happens one layer down: a user explicitly calling `grid.cell(..)`
        // (e.g. to set a per-cell `fill`) inside `#pdf.artifact(..)` produces
        // `GridCell(ArtifactElem(GridCell(body)))` once Typst's own grid
        // resolution adds its uniform outer `GridCell` wrapper — the inner,
        // user-authored `GridCell` has no meaning outside its parent grid's
        // cell lattice, so unwrap it and lower its own body like any other
        // wrapper instead of rasterizing.
        out.extend(ctx.blocks(&elem.body, styles)?);
    } else if let Some(elem) = child.to_packed::<TableCell>() {
        // Same as `GridCell` above, for `#table.cell(..)`.
        out.extend(ctx.blocks(&elem.body, styles)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PagebreakElem>()
    {
        // A hard page break maps to a `<w:br w:type="page"/>` in its own
        // paragraph — it must advance a page even when the current one is
        // empty, so stacked hard breaks yield real blank pages. A *weak*
        // break (or the even weaker `set page` boundary marker) only breaks
        // when content precedes it, and runs of them collapse; a marker block
        // carries that semantic to `move_page_breaks_before_following_blocks`.
        // A parity request (`to: "odd"`) keeps the explicit break even when
        // weak: it obliges a transition (plus possibly a parity blank) that a
        // droppable weak marker cannot carry — thesis chapter rules rely on
        // it. Flow lowering cannot open a parity *section* mid-block, so the
        // hard break is the closest honest approximation.
        let carries_parity = elem.to.get(styles).is_some();
        if (elem.weak.get(styles) || elem.boundary.get(styles)) && !carries_parity {
            out.push(Block::WeakPageBreak);
        } else {
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(Run::PageBreak)],
            }));
        }
    } else if let Some(elem) = child.to_packed::<ParElem>() {
        let mut props = ctx.resolve_par_props(elem, styles);
        props.review_origin = Some(
            ctx.review_origin(review_span(&elem.body), ReviewCandidateKind::Paragraph),
        );
        let content = ctx.inline_pchildren(&elem.body, styles, RunProps::default())?;
        out.push(Block::Para(Para { props, content }));
    } else if child.is::<ParbreakElem>() {
        // Paragraph boundary; no-op marker.
    } else if let Some(elem) = child.to_packed::<HeadingElem>() {
        out.extend(mappers::heading::heading(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<TitleElem>() {
        use typst_library::foundations::Resolve;
        use typst_library::layout::{BlockElem, Spacing as TSpacing};

        let body = elem.resolve_body(styles).at(elem.span())?;
        let mut props = ParaProps {
            style: Some("Title".into()),
            keep_next: true,
            jc: ctx.resolve_par_jc(styles),
            review_origin: Some(
                ctx.review_origin(review_span(&body), ReviewCandidateKind::Paragraph),
            ),
            ..Default::default()
        };
        let before = match styles.get(BlockElem::above) {
            typst_library::foundations::Smart::Custom(TSpacing::Rel(rel)) => {
                Some(crate::props::abs_to_twip(rel.abs.resolve(styles)))
            }
            _ => None,
        };
        let after = match styles.get(BlockElem::below) {
            typst_library::foundations::Smart::Custom(TSpacing::Rel(rel)) => {
                Some(crate::props::abs_to_twip(rel.abs.resolve(styles)))
            }
            _ => None,
        };
        if before.is_some() || after.is_some() {
            props.spacing = Some(Spacing { before, after, ..Default::default() });
        }
        let mut content = ctx.inline_pchildren(&body, styles, RunProps::default())?;
        if let Some(loc) = elem.location()
            && let Some((id, name)) = ctx.bookmark_for_emission(loc)
        {
            content.insert(0, ParaChild::BookmarkStart { id, name });
            content.push(ParaChild::BookmarkEnd { id });
        }
        out.push(Block::Para(Para { props, content }));
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
    } else if let Some(elem) = child.to_packed::<typst_library::layout::ColumnsElem>() {
        if mappers::columns::uses_table(elem, styles) {
            out.extend(mappers::columns::columns(elem, styles, ctx)?);
        } else {
            // Automatic block columns are wrapped in a continuous Word section by
            // `resolve_sections`; nested columns still lower as editable blocks.
            out.extend(ctx.blocks(&elem.body, styles)?);
        }
    } else if let Some(elem) = child.to_packed::<typst_library::layout::LayoutElem>() {
        // `#layout(size => ..)` hands the closure the container size and uses the
        // result. Responsive CV/poster templates wrap their entries in it, so
        // rasterizing the whole thing drops the text. Call the closure with the
        // page's content size and lower its result natively instead; if the
        // closure cannot run standalone, fall back to rasterizing it.
        handle_layout(ctx, elem, styles, out)?;
    } else if let Some(elem) = child.to_packed::<OutlineElem>() {
        out.extend(mappers::outline::outline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EquationElem>() {
        // Block equation.
        match mappers::math::equation(elem, styles, ctx)? {
            mappers::math::EquationOut::Block(blocks) => out.extend(blocks),
            mappers::math::EquationOut::Inline(runs) => {
                out.push(Block::Para(Para {
                    props: Default::default(),
                    content: runs.into_iter().map(ParaChild::Run).collect(),
                }));
            }
        }
    } else if let Some(elem) = child.to_packed::<FigureElem>() {
        // Figure: caption + body + cross-reference bookmark.
        out.extend(mappers::image::figure(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<typst_library::model::FigureCaption>() {
        // A standalone figure caption — a `show figure` rule that emits
        // `it.caption` separately from the figure body (common in two-column
        // paper templates) — realized to a `Caption`-styled paragraph instead
        // of rasterizing.
        out.extend(mappers::image::caption(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<QuoteElem>() {
        use typst_library::foundations::Resolve;
        let block = elem.block.get(styles);
        // Block quotes are padded 1em left and right (Typst's show-set default).
        let indent = block.then(|| {
            let em = crate::props::abs_to_twip(
                typst_library::layout::Em::new(1.0).resolve(styles),
            );
            crate::dom::Indent {
                left: Some(em),
                right: Some(em),
                ..Default::default()
            }
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
        if block && let Some(attribution) = elem.attribution.get_cloned(styles) {
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
        // Top-level `#place(..)` → an anchored drawing, or (for a float with
        // text-bearing content) the flowed blocks. See `mappers::image::place`.
        out.extend(mappers::image::place(elem, styles, ctx)?);
    } else if (child.is::<typst_library::layout::BlockElem>()
        || is_framed_container(child))
        && let Some(frame) = coherent_mixed_placed_canvas(child, styles, ctx)?
    {
        // Keep the nearest owning canvas atomic, but prefer a native grouped
        // DrawingML composition with editable positioned labels. Unsupported
        // clips/images/transforms retain the consumer-safe raster fallback.
        if let Some(run) = mappers::shape::mixed_canvas(ctx, &frame, child.span())? {
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(run)],
            }));
        } else if let Some(para) = fallback_para(
            mappers::image::coherent_placed_canvas_fallback(child, frame, ctx)?,
        ) {
            out.push(para);
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    } else if finite_layout_canvas_candidate(child, styles) {
        // A fixed-size `box(layout(..))` is an atomic procedural canvas even
        // when its final frame cannot satisfy the narrower mixed-canvas
        // classifier. This path is especially important in table cells: if we
        // let ordinary framed-block lowering unwrap the box first, the layout
        // callback's generated `place` children lose their shared coordinate
        // system and become unrelated Word anchors (or false content drops).
        // Keep the owning box intact, prefer one editable DrawingML group, and
        // otherwise rasterize exactly that bounded region once.
        if let Some(run) = mappers::shape::placed_shape_canvas(child, styles, ctx)? {
            out.push(Block::Para(Para {
                props: ParaProps::default(),
                content: vec![ParaChild::Run(run)],
            }));
        } else if let Some(para) =
            fallback_para(mappers::image::laid_out_block_fallback(child, styles, ctx)?)
        {
            out.push(para);
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    } else if (child.is::<typst_library::layout::BlockElem>()
        || is_framed_container(child))
        && contains_place(child)
        && placed_bodies_shape_only(child, styles)
        && let Some(run) = mappers::shape::transformed(child, styles, ctx)?
    {
        // A box/block/framed container whose ENTIRE content is a composition
        // of `#place`-positioned native shapes — the common way a QR/barcode
        // generator (e.g. `codetastic`) draws each module. `#place`'s own
        // non-floating layout already composites its child into the SAME
        // frame at an absolute position via an ordinary `push_frame` (not a
        // special wrapper) — so from `collect_shapes`'s point of view, laying
        // out the WHOLE container under `Target::Paged` (exactly what
        // `mappers::shape::transformed` already does for a bare
        // `#rotate`/`#scale`) makes this just an ordinary shape composition.
        // Recovered as one INLINE (non-floating) drawing, sidestepping the
        // "anchored drawing nested in a container" problem an earlier attempt
        // hit (see COVERAGE.md §7.1c) — no `wp:anchor` is involved at all
        // here.
        out.push(Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(run)],
        }));
    } else if is_empty_plain_box(child, styles) {
        // Empty boxes are layout struts/spacers, not lost content. They can
        // reach block dispatch after realization (for example Codly's
        // zero-width per-line height strut), where the framed-container path
        // would otherwise attempt a text box and report a false content drop.
        ctx.record_content_decision(
            child,
            Representation::Approximate,
            DecisionReason::EmptyBoxGeometryApproximation,
            LossSet::VISUAL_ONLY,
            0,
        );
    } else if let Some(elem) = child.to_packed::<typst_library::layout::BlockElem>() {
        handle_block_box(ctx, elem, styles, out)?;
    } else if is_framed_container(child) && handle_block_framed(ctx, child, styles, out)?
    {
        // A block-level framed container (`#rect`/`#box`/`#square` standing as its
        // own block) with flowing content → shaded + bordered paragraphs that
        // break across pages, mirroring `#block`.
    } else if is_framed_container(child) && ctx.suppress_text_box {
        // A framed container in a *centered* context (a figure body): a centered
        // `wps:txbx` text box does not flow its text in LibreOffice. Rasterize the
        // box to an image instead — a centered inline image renders correctly in
        // every consumer (Word renders the text box fine, but this keeps both).
        if let Some(para) =
            fallback_para(mappers::image::laid_out_block_fallback(child, styles, ctx)?)
        {
            out.push(para);
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    } else if is_framed_container(child) {
        // A short, single-line standalone framed container → a Word text box (a
        // sized, framed box). Standalone text boxes render correctly (an *inline*
        // one does not — that case is handled by run shading in `handle_inline`).
        // Not a text-box candidate (unrepresentable fill, layout-bound body) → fall back
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
        } else if let Some(para) =
            fallback_para(mappers::image::laid_out_block_fallback(child, styles, ctx)?)
        {
            // A drawable block with no extractable text — a diagonal/endpoint
            // `#line`, `#polygon`, `#curve`, a `#layout`/`#stack`/`#move`/
            // `#rotate`/`#scale` body, … — rasterizes to an image so the visual
            // survives instead of being silently dropped (with the frame's
            // recovered text appended as hidden searchable runs).
            out.push(para);
        } else if is_empty_plain_box(child, styles) {
            ctx.record_content_decision(
                child,
                Representation::Approximate,
                DecisionReason::EmptyBoxGeometryApproximation,
                LossSet::VISUAL_ONLY,
                0,
            );
        } else if !is_invisible_noop(child) {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    }
    Ok(())
}

/// Lowers a `#layout(size => ..)` by invoking its closure with the page's
/// content size, then lowering the produced content natively — so a responsive
/// CV/poster wrapper keeps its text editable instead of rasterizing. The
/// closure runs against the fully-resolved introspector (export is post-layout),
/// so counters and references inside it are stable. If it cannot be evaluated
/// standalone, fall back to rasterizing it so the visual still survives.
fn handle_layout(
    ctx: &mut DocxCtx,
    elem: &typst_library::foundations::Packed<typst_library::layout::LayoutElem>,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    match ctx.eval_layout_content(elem, styles) {
        Some(content) => {
            // A layout callback that returns one framed placement canvas owns
            // the generated coordinates. Preserve that root atomically even
            // when its internals exceed the native group mapper's capability;
            // recursively lowering the callback result first destroys the
            // only boundary at which an honest whole-region raster is
            // possible (notably Fletcher/CeTZ diagrams inside table cells).
            if is_framed_container(&content) && contains_place(&content) {
                if let Some(run) =
                    mappers::shape::placed_shape_canvas(&content, styles, ctx)?
                {
                    out.push(Block::Para(Para {
                        props: ParaProps::default(),
                        content: vec![ParaChild::Run(run)],
                    }));
                } else if let Some(para) = fallback_para(
                    mappers::image::laid_out_block_fallback(&content, styles, ctx)?,
                ) {
                    out.push(para);
                } else {
                    ctx.warn_ignored(content.elem().name(), content.span());
                }
            } else {
                out.extend(ctx.blocks(&content, styles)?);
            }
        }
        None => {
            if let Some(para) = fallback_para(mappers::image::laid_out_block_fallback(
                elem.pack_ref(),
                styles,
                ctx,
            )?) {
                out.push(para);
            } else {
                let source = elem.clone().pack();
                ctx.record_content_drop(
                    &source,
                    DecisionReason::LayoutCallbackUnavailable,
                    "layout callback and whole-region fallback produced no output",
                );
            }
        }
    }
    Ok(())
}

/// Wraps rasterization-fallback runs — a drawing plus the hidden, searchable
/// text recovered from its laid-out frame — in a single paragraph, so the
/// hidden text sits beside the image. Returns `None` when nothing was produced.
fn fallback_para(runs: Vec<Run>) -> Option<Block> {
    if runs.is_empty() {
        return None;
    }
    Some(Block::Para(Para {
        props: ParaProps::default(),
        content: runs.into_iter().map(ParaChild::Run).collect(),
    }))
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
        // `#hide[..]` is invisible by design — and documented as a redaction
        // tool, so its body is dropped entirely (only introspection tags are
        // kept, matching paged export's `Frame::hide`).
        || child.is::<typst_library::layout::HideElem>()
        // A float-flush marker (`place` float ordering): no DOCX equivalent.
        || child.is::<typst_library::layout::FlushElem>()
        // A tagged-PDF accessibility delimiter (unwrapped to its body elsewhere).
        || child.is::<typst_library::pdf::PdfMarkerTag>()
}

/// Whether `child` is an empty, unpainted box used solely as an inline layout
/// strut or spacer. Its lack of body means there is no semantic content to
/// drop; if a whole-region fallback produces no pixels, report only the lost
/// geometry instead of claiming content loss.
pub(crate) fn is_empty_plain_box(
    child: &Content,
    styles: typst_library::foundations::StyleChain,
) -> bool {
    child
        .to_packed::<typst_library::layout::BoxElem>()
        .is_some_and(|elem| {
            elem.body.get_ref(styles).is_none() && box_is_plain(elem, styles)
        })
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

    let body = elem.body.get_ref(styles);
    let content = match body {
        // Any content body (regardless of fill kind) → extract its paragraphs
        // so the text stays live. A gradient/tiling fill can't be a flat
        // paragraph shade, but approximating it (below) and keeping the text
        // beats rasterizing the whole box to a text-dead image.
        Some(BlockBody::Content(content)) => content,
        _ => {
            // A layouter body (`#block(width => ..)`) is an opaque closure with
            // no extractable content: rasterize the whole box (and recover its
            // laid-out text as hidden searchable runs beside the image).
            if let Some(para) = fallback_para(mappers::image::laid_out_block_fallback(
                elem.pack_ref(),
                styles,
                ctx,
            )?) {
                out.push(para);
            }
            return Ok(());
        }
    };

    // Recurse into the body to obtain its paragraphs.
    let mut inner = ctx.blocks(content, styles)?;

    // A fixed-height filled block is a bounded visual region (terminal panes,
    // cards, dashboards), not merely a sequence of shaded paragraphs. Word
    // paragraph shading cannot retain the requested empty height; a one-cell
    // table can, while keeping every child paragraph/table native and editable.
    // Use `atLeast`, not `exact`, so font substitution never clips the content.
    //
    // The relative base is the block's own container, not the page: a `height:
    // 100%` filled block used as a grid/table cell's ENTIRE body (a poster's
    // colored header/footer band, sized by a `grid(rows: (13%, 83%, 4%), ..)`
    // row) must resolve against that cell's own measured height
    // (`shape_height_base`, already scoped per cell in `mappers::table`) —
    // resolving it against the page's full available height instead turned a
    // 13%-of-the-page header band into a page-height-tall block, which then
    // exceeded the ratio guard below and rasterized/reflowed into a spurious
    // extra page of solid fill. `available_width` just below already receives
    // this same per-cell scoping (via `with_available_width`); only the height
    // axis lacked its equivalent.
    let fixed_height = match elem.height.get(styles) {
        typst_library::layout::Sizing::Rel(rel) => Some(
            rel.resolve(styles)
                .relative_to(ctx.shape_height_base.unwrap_or(ctx.available_height)),
        ),
        _ => None,
    };
    if let (Some(Paint::Solid(color)), Some(height)) = (&fill, fixed_height)
        && height.to_pt().is_finite()
        && height.to_pt() > 0.0
        // This native cell is for bounded panels such as terminal/code panes.
        // Page-sized slide/canvas blocks rely on fixed placement; turning them
        // into flowing tables can multiply one slide into many Word pages.
        && height.to_pt() <= ctx.available_height.to_pt() * 0.6
    {
        if !matches!(inner.last(), Some(Block::Para(_))) {
            inner.push(Block::Para(Para {
                props: ParaProps::default(),
                content: Vec::new(),
            }));
        }
        let width = match elem.width.get(styles) {
            typst_library::foundations::Smart::Custom(rel) => {
                rel.resolve(styles).relative_to(ctx.available_width)
            }
            _ => ctx.available_width,
        };
        let width_dxa = crate::props::abs_to_twip(width).max(1);
        let height_dxa = crate::props::abs_to_twip(height).max(1);
        out.push(Block::Table(Tbl {
            props: TblProps { width_dxa: Some(width_dxa), style: None, jc: None },
            grid: vec![width_dxa],
            rows: vec![Row {
                header: false,
                cant_split: true,
                height: Some(RowHeight { val: height_dxa, exact: false }),
                cells: vec![Cell {
                    w_dxa: Some(width_dxa),
                    grid_span: 1,
                    v_merge: None,
                    borders: CellBorders::default(),
                    shd_fill: Some(crate::props::color_to_hex(color)),
                    margins: crate::dom::CellMargins::default(),
                    valign: Some(VAlign::Top),
                    blocks: inner,
                }],
            }],
        }));
        return Ok(());
    }

    // Resolve the box decorations once. A gradient fill is approximated by its
    // first stop's colour (the dominant tone for most gradient backgrounds); a
    // tiling has no single-colour analogue, so it drops to no shade — the text
    // is preserved either way.
    let shd_fill = match &fill {
        Some(Paint::Solid(c)) => Some(crate::props::color_to_hex(c)),
        Some(Paint::Gradient(g)) => crate::props::gradient_shade_hex(g),
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
        Insets {
            left: ind_left,
            right: ind_right,
            top: inset_top,
            bottom: inset_bottom,
        },
        above,
        below,
    );

    out.extend(inner);
    Ok(())
}

/// Folds a paragraph followed by `place(bottom + right)[heading]` into one line
/// with a right-aligned tab stop. The semantic heading inside the placement is
/// the key signal: this is a heading-row label (location/date/status), not page
/// furniture. A page-relative floating text box is not an honest
/// representation: Word's page/paragraph anchors either send it to the margin
/// or destabilize flow in LibreOffice. A right tab stays native, editable, and
/// reflow-safe.
fn fold_right_aligned_heading_overlays(blocks: &mut Vec<Block>, width_dxa: i32) {
    use crate::dom::{TabAlign, TabStop};

    let mut index = 1usize;
    while index < blocks.len() {
        let owner_index = (0..index)
            .rev()
            .find(|candidate| !matches!(blocks[*candidate], Block::Tag(_)));
        let owner_is_para = owner_index
            .is_some_and(|candidate| matches!(blocks[candidate], Block::Para(_)));
        if !owner_is_para || !is_right_bottom_heading_overlay(&blocks[index]) {
            index += 1;
            continue;
        }
        let owner_index = owner_index.unwrap();

        let mut overlay = Vec::new();
        let Block::Para(anchor_para) = blocks.remove(index) else { unreachable!() };
        for child in anchor_para.content {
            match child {
                ParaChild::Run(Run::Drawing(drawing)) => {
                    let text_box = drawing.shape.unwrap().txbx.unwrap();
                    for block in text_box.blocks {
                        match block {
                            Block::Para(para) => overlay.extend(para.content),
                            Block::Tag(tag) => overlay.push(ParaChild::Tag(tag)),
                            _ => unreachable!(),
                        }
                    }
                }
                marker @ (ParaChild::BookmarkStart { .. }
                | ParaChild::BookmarkEnd { .. }
                | ParaChild::Tag(_)) => overlay.push(marker),
                _ => unreachable!(),
            }
        }

        let Block::Para(owner) = &mut blocks[owner_index] else { unreachable!() };
        owner.content.push(ParaChild::Run(Run::FillTab));
        owner.content.extend(overlay);
        if width_dxa > 0
            && !owner.props.tabs.iter().any(|tab| matches!(tab.val, TabAlign::End))
        {
            owner.props.tabs.push(TabStop {
                val: TabAlign::End,
                leader: None,
                pos: width_dxa,
            });
        }

        // Keep looking at the same index: another local overlay may follow the
        // same owner after removal.
    }
}

fn is_right_bottom_heading_overlay(block: &Block) -> bool {
    use crate::dom::AnchorWrap;

    let Block::Para(para) = block else { return false };
    let mut drawing = None;
    for child in &para.content {
        match child {
            ParaChild::Run(Run::Drawing(candidate)) if drawing.is_none() => {
                drawing = Some(candidate);
            }
            ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::Tag(_) => {}
            _ => return false,
        }
    }
    let Some(drawing) = drawing else { return false };
    let Some(anchor) = &drawing.anchor else { return false };
    if anchor.behind
        || !matches!(anchor.wrap, AnchorWrap::None)
        || anchor.pos_h.align != Some("right")
        || anchor.pos_h.offset.is_some()
        || anchor.pos_v.align != Some("bottom")
        || anchor.pos_v.offset.is_some()
        || drawing.group.is_some()
    {
        return false;
    }
    let Some(shape) = &drawing.shape else { return false };
    if shape.fill.is_some() || shape.stroke.is_some() {
        return false;
    }
    let Some(text_box) = &shape.txbx else { return false };
    let mut paragraphs = 0usize;
    text_box.blocks.iter().all(|block| match block {
        Block::Para(_) => {
            paragraphs += 1;
            paragraphs == 1
                && matches!(block, Block::Para(para) if para.props.outline_lvl.is_some())
        }
        Block::Tag(_) => true,
        _ => false,
    }) && paragraphs == 1
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
        if let Some(b) = pbdr
            && p.pbdr.is_none()
        {
            p.pbdr = Some(b.clone());
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
pub(crate) fn is_framed_container(child: &Content) -> bool {
    use typst_library::layout::BoxElem;
    use typst_library::visualize::{RectElem, SquareElem};
    child.is::<BoxElem>() || child.is::<RectElem>() || child.is::<SquareElem>()
}

/// Cheap pre-filter for [`mappers::shape::transformed`]'s container path:
/// whether `child` (a box/block/framed container) has a `#place(..)`
/// *anywhere* inside it — the shape it should attempt to recover. Skipping
/// the (much more expensive) layout-and-extract attempt for the overwhelming
/// majority of ordinary boxes/blocks that never use `#place` keeps this a
/// no-cost check for typical documents.
pub(crate) fn contains_place(child: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::PlaceElem;
    matches!(
        child.traverse(&mut |e: Content| if e.is::<PlaceElem>() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }),
        ControlFlow::Break(())
    )
}

/// Whether `child` is the nearest finite layout root for a coherent mixed
/// placed composition.
///
/// Object count is not evidence of coherence: a label centered in one waveform
/// is already coupled to that shape, while hundreds of pure shapes can remain a
/// native DrawingML group. Instead, ownership is structural. Every material
/// leaf under this root must belong to a non-floating `#place`, and the root
/// must mix at least one native-shape placement with one rich/text placement.
/// Nested block/framed roots are deliberately not traversed, so an outer prose
/// block cannot aggregate unrelated annotations or absorb a smaller canvas.
pub(crate) fn coherent_mixed_placed_canvas(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<typst_library::layout::Frame>> {
    use typst_library::introspection::{Location, Tag};
    use typst_library::layout::{Frame, FrameItem, PlaceElem};

    #[derive(Default)]
    struct Composition {
        active_places: Vec<Location>,
        places: usize,
        saw_shape: bool,
        saw_rich: bool,
        saw_shape_outside_place: bool,
        saw_text_outside_place: bool,
        invalid_tag: bool,
    }

    fn visit(frame: &Frame, composition: &mut Composition) {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Tag(Tag::Start(content, _)) if content.is::<PlaceElem>() => {
                    let Some(location) = content.location() else {
                        composition.invalid_tag = true;
                        continue;
                    };
                    composition.active_places.push(location);
                    composition.places += 1;
                }
                FrameItem::Tag(Tag::End(location, ..)) => {
                    if let Some(index) = composition
                        .active_places
                        .iter()
                        .rposition(|active| active == location)
                    {
                        composition.active_places.remove(index);
                    }
                }
                FrameItem::Tag(_) => {}
                FrameItem::Group(group) => visit(&group.frame, composition),
                FrameItem::Shape(_, _) => {
                    if composition.active_places.is_empty() {
                        composition.saw_shape_outside_place = true;
                    } else {
                        composition.saw_shape = true;
                    }
                }
                FrameItem::Text(_) => {
                    if composition.active_places.is_empty() {
                        composition.saw_text_outside_place = true;
                    } else {
                        composition.saw_rich = true;
                    }
                }
                // The native mixed-canvas collector deliberately rejects images
                // and link overlays. Do not route a finite live layout through
                // the atomic raster fallback merely because it contains either.
                FrameItem::Image(_, _, _) | FrameItem::Link(_, _) => {
                    composition.invalid_tag = true;
                }
            }
        }
    }

    // Source-visible `#place` nodes are the conservative ownership proof for
    // ordinary containers. A fixed-height inline box containing `#layout` is a
    // second safe root: its callback material exists only after layout, but the
    // explicit box bounds still own the complete composition. This covers
    // procedural headers/backgrounds without aggregating a flowing prose block.
    let finite_layout_root = finite_layout_canvas_candidate(child, styles);
    if !structurally_owned_placed_canvas(child, styles) && !finite_layout_root {
        return Ok(None);
    }

    let (frame, failed) =
        ctx.layout_export_frame(child, styles, child.span(), ctx.available_height)?;
    if failed {
        return Ok(None);
    }
    let Some(frame) = frame else { return Ok(None) };

    let mut composition = Composition::default();
    visit(&frame, &mut composition);
    let source_owned = !composition.saw_shape_outside_place
        && !composition.saw_text_outside_place
        && composition.saw_shape
        && composition.saw_rich;
    let finite_layout_owned = finite_layout_root
        && composition.saw_shape
        && (composition.saw_rich || composition.saw_text_outside_place);
    let coherent = !composition.invalid_tag
        && composition.places >= 2
        && (source_owned || finite_layout_owned);
    Ok(coherent.then_some(frame))
}

/// Whether an explicitly bounded inline box owns a procedural `#layout`
/// composition. The callback's generated placements are not present in the
/// source tree, so the frame probe supplies the final ownership evidence.
pub(crate) fn finite_layout_canvas_candidate(
    child: &Content,
    styles: StyleChain,
) -> bool {
    use std::ops::ControlFlow;
    use typst_library::foundations::Smart;
    use typst_library::layout::{BoxElem, LayoutElem};

    let fixed_height = child
        .to_packed::<BoxElem>()
        .is_some_and(|boxed| !matches!(boxed.height.get(styles), Smart::Auto));
    let contains_layout = matches!(
        child.traverse(&mut |content: Content| if content.is::<LayoutElem>() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }),
        ControlFlow::Break(())
    );
    fixed_height && contains_layout
}

/// Cheap ownership partition before the authoritative frame probe. It ensures
/// only roots whose material source is entirely non-floating placement reach
/// layout classification, so rejected ordinary containers do not advance the
/// real export locator. Nested finite roots stop the walk and own their own
/// placements when conversion recurses into them.
fn structurally_owned_placed_canvas(child: &Content, styles: StyleChain) -> bool {
    use typst_library::foundations::{SequenceElem, StyledElem};
    use typst_library::introspection::TagElem;
    use typst_library::layout::{
        AlignElem, BlockBody, BlockElem, BoxElem, MoveElem, PadElem, PlaceElem,
        RotateElem, ScaleElem, SkewElem, StackChild, StackElem,
    };
    use typst_library::text::SpaceElem;
    use typst_library::visualize::{RectElem, SquareElem};

    #[derive(Default)]
    struct Composition {
        places: usize,
        shape_places: usize,
        rich_places: usize,
    }

    fn visit(
        content: &Content,
        styles: StyleChain,
        composition: &mut Composition,
    ) -> bool {
        if let Some(place) = content.to_packed::<PlaceElem>() {
            if place.float.get(styles) {
                return false;
            }
            composition.places += 1;
            if body_shape_only(&place.body, styles) {
                composition.shape_places += 1;
            } else {
                composition.rich_places += 1;
            }
            return true;
        }

        if content.is::<BlockElem>()
            || content.is::<BoxElem>()
            || content.is::<RectElem>()
            || content.is::<SquareElem>()
        {
            return false;
        }
        if content.is::<TagElem>()
            || content.is::<SpaceElem>()
            || content.is::<ParbreakElem>()
        {
            return true;
        }
        if let Some(sequence) = content.to_packed::<SequenceElem>() {
            return sequence
                .children
                .iter()
                .all(|child| visit(child, styles, composition));
        }
        if let Some(styled) = content.to_packed::<StyledElem>() {
            return visit(&styled.child, styles.chain(&styled.styles), composition);
        }
        if let Some(align) = content.to_packed::<AlignElem>() {
            return visit(&align.body, styles, composition);
        }
        if let Some(moved) = content.to_packed::<MoveElem>() {
            return visit(&moved.body, styles, composition);
        }
        if let Some(padded) = content.to_packed::<PadElem>() {
            return visit(&padded.body, styles, composition);
        }
        if let Some(rotated) = content.to_packed::<RotateElem>() {
            return visit(&rotated.body, styles, composition);
        }
        if let Some(scaled) = content.to_packed::<ScaleElem>() {
            return visit(&scaled.body, styles, composition);
        }
        if let Some(skewed) = content.to_packed::<SkewElem>() {
            return visit(&skewed.body, styles, composition);
        }
        if let Some(stack) = content.to_packed::<StackElem>() {
            return stack.children.iter().all(|child| match child {
                StackChild::Spacing(_) => true,
                StackChild::Block(content) => visit(content, styles, composition),
            });
        }
        false
    }

    let body = if let Some(block) = child.to_packed::<BlockElem>() {
        match block.body.get_ref(styles).as_ref() {
            Some(BlockBody::Content(body)) => Some(body),
            _ => None,
        }
    } else if let Some(boxed) = child.to_packed::<BoxElem>() {
        boxed.body.get_ref(styles).as_ref()
    } else if let Some(rect) = child.to_packed::<RectElem>() {
        rect.body.get_ref(styles).as_ref()
    } else if let Some(square) = child.to_packed::<SquareElem>() {
        square.body.get_ref(styles).as_ref()
    } else {
        None
    };

    let mut composition = Composition::default();
    body.is_some_and(|body| visit(body, styles, &mut composition))
        && composition.places >= 2
        && composition.shape_places > 0
        && composition.rich_places > 0
}

/// Whether every `#place` body inside `child` is structurally a composition of
/// native shapes. This keeps the layout-derived shape shortcut deterministic:
/// a placed text/citation body can render empty before introspection stabilizes,
/// which makes the laid-out frame look shape-only for one iteration and textful
/// in the next.
pub(crate) fn placed_bodies_shape_only(
    child: &Content,
    styles: typst_library::foundations::StyleChain,
) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::PlaceElem;

    child
        .traverse(&mut |e: Content| {
            if let Some(place) = e.to_packed::<PlaceElem>()
                && !body_shape_only(&place.body, styles)
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
        .is_continue()
}

/// Whether `body` is structurally limited to elements the native shape-group
/// mapper can consume. Conservative false negatives are fine: they fall back to
/// the existing live-content/raster paths. False positives are not fine because
/// unresolved text/citations can disappear from the probe frame and make lowering
/// flap across iterations.
pub(crate) fn body_shape_only(
    body: &Content,
    styles: typst_library::foundations::StyleChain,
) -> bool {
    use std::ops::ControlFlow;
    use typst_library::foundations::{SequenceElem, StyledElem};
    use typst_library::introspection::TagElem;
    use typst_library::layout::{
        AlignElem, BoxElem, MoveElem, PadElem, PlaceElem, RotateElem, ScaleElem,
        StackElem,
    };
    use typst_library::text::SpaceElem;
    use typst_library::visualize::{
        CircleElem, CurveClose, CurveCubic, CurveElem, CurveLine, CurveMove, CurveQuad,
        EllipseElem, LineElem, PolygonElem, RectElem, SquareElem,
    };

    let mut saw_shape = false;
    let result = body.traverse(&mut |e: Content| {
        if e.is::<TagElem>() || e.is::<SpaceElem>() || e.is::<ParbreakElem>() {
            return ControlFlow::Continue(());
        }

        if e.is::<LineElem>() || e.is::<CurveElem>() {
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        // Curve command values are traversed as content children of `CurveElem`.
        // They are not standalone visual objects, but rejecting them here would
        // make every path-based Cetz placement look rich instead of shape-only.
        if e.is::<CurveMove>()
            || e.is::<CurveLine>()
            || e.is::<CurveQuad>()
            || e.is::<CurveCubic>()
            || e.is::<CurveClose>()
        {
            return ControlFlow::Continue(());
        }

        if let Some(rect) = e.to_packed::<RectElem>() {
            if rect.body.get_ref(styles).is_some() {
                return ControlFlow::Break(());
            }
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        if let Some(square) = e.to_packed::<SquareElem>() {
            if square.body.get_ref(styles).is_some() {
                return ControlFlow::Break(());
            }
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        if let Some(ellipse) = e.to_packed::<EllipseElem>() {
            if ellipse.body.get_ref(styles).is_some() {
                return ControlFlow::Break(());
            }
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        if let Some(circle) = e.to_packed::<CircleElem>() {
            if circle.body.get_ref(styles).is_some() {
                return ControlFlow::Break(());
            }
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        if e.is::<PolygonElem>() {
            saw_shape = true;
            return ControlFlow::Continue(());
        }

        if e.is::<SequenceElem>()
            || e.is::<StyledElem>()
            || e.is::<AlignElem>()
            || e.is::<BoxElem>()
            || e.is::<MoveElem>()
            || e.is::<PadElem>()
            || e.is::<PlaceElem>()
            || e.is::<RotateElem>()
            || e.is::<ScaleElem>()
            || e.is::<StackElem>()
        {
            return ControlFlow::Continue(());
        }

        ControlFlow::Break(())
    });

    result.is_continue() && saw_shape
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

    let Some((body, fill, stroke_sides, inset)) = block_framed_parts(child, styles)
    else {
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
    // No visible frame → not a callout.
    if shd_fill.is_none() && pbdr.is_none() {
        // A frameless box around one inline label is spacing, not artwork. This
        // is common in table headers (`box(inset: ..)[x0]`): flatten the body to
        // live text and carry the inset into paragraph spacing/indentation.
        // Restrict this to non-flowing, text-box-safe bodies so designed layout
        // canvases and nested structures still take their specialized paths.
        if !body_is_flowing(&body, styles)
            && !body_has_footnote(&body)
            && body_textbox_safe(&body)
        {
            let mut inner = ctx.blocks(&body, styles)?;
            crate::document::collect_tags(&inner, &mut ctx.deferred_tags);
            let insets = Insets {
                left: inset.left.and_then(|r| nonzero_twip(r, styles)),
                right: inset.right.and_then(|r| nonzero_twip(r, styles)),
                top: inset.top.and_then(|r| nonzero_twip(r, styles)),
                bottom: inset.bottom.and_then(|r| nonzero_twip(r, styles)),
            };
            stamp_box_decorations(&mut inner, None, &None, insets, None, None);
            out.extend(inner);
            return Ok(true);
        }
        // The specific recoverable case: a *frameless* block box wrapping a
        // `#grid` that holds a `#figure` — i.e. a wrap-content figure, which
        // lowers to `box(grid(figure, text))`. Rasterizing the whole box drops
        // the figure, its caption + `SEQ`, and the wrapped text; lowering the
        // grid natively (→ a table, figure → image + caption) recovers them, and
        // the well-tested grid mapper doesn't drop content. We deliberately do
        // NOT lower an arbitrary frameless box here — a designed full-page
        // layout box can lose content through a native re-walk — so the body
        // must structurally be a grid-of-figure (`body_is_wrap_figure`) or —
        // the second recoverable case — directly one ordinary flowing
        // container (`#columns`/`#stack`/a non-figure `#grid`,
        // `body_is_frameless_flow_container`): a frameless box whose *whole*
        // body is one of these (e.g. a poster's `box(inset: ..)[#columns(2,
        // ..)]`) has nothing else that could be lost by flattening it, unlike
        // a more elaborate full-page composition.
        if body_is_wrap_figure(&body) || body_is_frameless_flow_container(&body) {
            let inner = ctx.blocks(&body, styles)?;
            crate::document::collect_tags(&inner, &mut ctx.deferred_tags);
            out.extend(inner);
            return Ok(true);
        }
        return Ok(false);
    }
    // Only flowing/block content takes the main-story paragraph path; a short
    // single-line callout stays a (standalone, sized) text box — UNLESS the body
    // has a footnote (illegal in a text box), content that is Word-fragile /
    // mis-laid inside one (figures, images, tables, math, nested frames), OR the
    // stroke is non-uniform across sides (`stroke_sides_nonuniform` — e.g.
    // `box(stroke: (bottom: ..))`, the common "border as a section-title
    // underline" idiom): a DrawingML shape outline is inherently uniform around
    // all four sides, so routing a bottom-only stroke through the text-box path
    // would silently turn it into a full box — only this paragraph border
    // (`w:pBdr`, via `block_borders` below) can express per-side strokes
    // independently. In all these cases it must flow here to stay correct.
    // Decided before extraction.
    if !body_is_flowing(&body, styles)
        && !body_has_footnote(&body)
        && body_textbox_safe(&body)
        && !stroke_sides_nonuniform(&stroke_sides)
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
    typst_library::layout::Sides<
        Option<typst_library::layout::Rel<typst_library::layout::Length>>,
    >,
)> {
    use typst_library::layout::BoxElem;
    use typst_library::visualize::{RectElem, SquareElem};

    if let Some(e) = child.to_packed::<BoxElem>() {
        let body = e.body.get_cloned(styles)?;
        Some((
            body,
            e.fill.get_cloned(styles),
            e.stroke.get_cloned(styles),
            e.inset.get_cloned(styles),
        ))
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
        Smart::Auto => {
            Sides::splat(Some(Some(typst_library::visualize::Stroke::default())))
        }
    }
}

/// Whether a framed container's body is *flowing* block content that should
/// break across pages (a multi-paragraph callout, a code listing, a list, a
/// table) rather than a short inline label. Checked on the raw body (no
/// extraction): flowing iff it contains a paragraph break, a block raw listing,
/// a list/enum/term list, a table/grid, or a nested block.
fn body_is_flowing(
    body: &Content,
    styles: typst_library::foundations::StyleChain,
) -> bool {
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

/// Whether a per-side stroke definition has SOME sides set and others not —
/// e.g. `box(stroke: (bottom: 1pt + black))`, the common "border as a
/// section-title underline" idiom. Only a paragraph border (`w:pBdr`, via
/// [`block_borders`]/`handle_block_framed`'s bordered-paragraph path) can
/// express this independently per side; both the inline character border
/// (`w:bdr`, via `mappers::shape::inline_frame`) and a DrawingML shape
/// outline (the text-box path) are inherently uniform around all four
/// sides, so either would silently turn a bottom-only stroke into a full box.
pub(crate) fn stroke_sides_nonuniform(
    sides: &typst_library::layout::Sides<
        Option<Option<typst_library::visualize::Stroke>>,
    >,
) -> bool {
    let set = [
        matches!(sides.top, Some(Some(_))),
        matches!(sides.right, Some(Some(_))),
        matches!(sides.bottom, Some(Some(_))),
        matches!(sides.left, Some(Some(_))),
    ];
    set.contains(&true) && set.contains(&false)
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

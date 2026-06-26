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
    let mut blocks = Vec::new();
    // Buffered paragraph children currently being assembled, plus its
    // resolved paragraph properties (from the first `ParElem` seen).
    let mut pending: Vec<ParaChild> = Vec::new();
    let mut pending_props: Option<ParaProps> = None;
    let mut have_pending = false;

    for (child, styles) in children {
        if child.is::<ParbreakElem>() {
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            continue;
        }
        if let Some(par) = child.to_packed::<ParElem>() {
            // A paragraph fragment: contribute its inline content to the current
            // paragraph. Adjacent fragments (split by inline interruptions like
            // strong/emph) thus coalesce into a single `<w:p>`.
            if pending_props.is_none() {
                pending_props = Some(ctx.resolve_par_props(par, *styles));
            }
            inline_children(ctx, &par.body, *styles, &mut pending)?;
            have_pending = true;
        } else if let Some(elem) = child.to_packed::<TagElem>() {
            // Introspection tag: record as a block-level tag (kept for the
            // introspector). Does not break the current paragraph.
            if !have_pending {
                blocks.push(Block::Tag(elem.tag.clone()));
            } else {
                pending.push(ParaChild::Tag(elem.tag.clone()));
            }
        } else if is_inline(child) {
            push_inline(ctx, child, *styles, &mut pending)?;
            have_pending = true;
        } else {
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            handle_block(ctx, child, *styles, &mut blocks)?;
        }
    }
    flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
    Ok(blocks)
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

/// Dispatches one realized native block element.
fn handle_block(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: typst_library::foundations::StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    if let Some(elem) = child.to_packed::<TagElem>() {
        out.push(Block::Tag(elem.tag.clone()));
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
        let runs = ctx.inline_runs(&elem.body, styles, RunProps::default())?;
        let mut props = crate::dom::ParaProps::default();
        props.style = Some("Quote".into());
        out.push(Block::Para(Para {
            props,
            content: runs.into_iter().map(ParaChild::Run).collect(),
        }));
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

/// Ensures a paragraph is non-empty (used where Word requires content).
pub fn empty_para() -> Para {
    Para { props: Default::default(), content: vec![ParaChild::Run(Run::Text {
        props: RunProps::default(),
        text: "".into(),
    })] }
}

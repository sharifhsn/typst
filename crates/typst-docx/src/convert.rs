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

    for (child, styles) in children {
        if child.is::<ParbreakElem>() {
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            continue;
        }
        if let Some(par) = child.to_packed::<ParElem>() {
            // Each `ParElem` is one paragraph. `ParbreakElem`s are consumed
            // during realization (they never reach us), so consecutive
            // paragraphs arrive as back-to-back `ParElem`s with no separator —
            // we must flush between them rather than coalesce, otherwise every
            // paragraph in the document would merge into one `<w:p>` (and the
            // per-paragraph `w:jc`/spacing of all but the first would be lost).
            // Inline formatting (strong/emph/…) stays *within* a single
            // `ParElem` body thanks to the registered inline rules, so it never
            // splits a paragraph here.
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
            // Whether the block immediately preceding this paragraph is also a
            // paragraph. Typst's default `first-line-indent` (`all: false`)
            // indents a paragraph only when it directly follows another one;
            // Word's `w:firstLine` has no such rule, so we apply it here. This is
            // checked *after* the flush so `blocks.last()` is the just-emitted
            // previous paragraph rather than the one before it.
            let prev_was_para = matches!(blocks.last(), Some(Block::Para(_)));
            let mut props = ctx.resolve_par_props(par, *styles);
            if prev_was_para
                && props.ind.as_ref().and_then(|i| i.first_line).is_none()
                && let Some(amount) = ctx.consecutive_first_line_indent(*styles)
            {
                props.ind.get_or_insert_with(Default::default).first_line = Some(amount);
            }
            pending_props = Some(props);
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
        } else if is_inline(child) {
            push_inline(ctx, child, *styles, &mut pending)?;
            have_pending = true;
        } else {
            let from = blocks.len();
            flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
            handle_block(ctx, child, *styles, &mut blocks)?;
            pending_v = apply_pending_v(&mut blocks, from, pending_v);
        }
    }
    let from = blocks.len();
    flush(&mut pending, &mut pending_props, &mut have_pending, &mut blocks);
    apply_pending_v(&mut blocks, from, pending_v);
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
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PlaceElem>() {
        // Top-level `#place(..)` → a floating drawing (G8). The body is lowered
        // to an image (native or rasterized) wrapped in a `<wp:anchor>`.
        if let Some(block) = mappers::image::place(elem, styles, ctx)? {
            out.push(block);
        }
    } else if let Some(elem) = child.to_packed::<typst_library::layout::BlockElem>() {
        handle_block_box(ctx, elem, styles, out)?;
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

    let has_box = shd_fill.is_some() || pbdr.is_some();
    let last = inner.len().saturating_sub(1);
    let para_count = inner.iter().filter(|b| matches!(b, Block::Para(_))).count();
    let multi_para = para_count > 1;

    let mut seen_para = 0usize;
    for (i, block) in inner.iter_mut().enumerate() {
        let Block::Para(para) = block else { continue };
        let p = &mut para.props;

        // Stamp identical shading/borders on every paragraph so adjacent
        // paragraphs inside one box render as a single visual box in Word.
        if let Some(f) = shd_fill {
            p.shd_fill.get_or_insert(f);
        }
        if let Some(b) = &pbdr {
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
        if ind_left.is_some() || ind_right.is_some() {
            let ind = p.ind.get_or_insert_with(Default::default);
            if let Some(l) = ind_left {
                ind.left.get_or_insert(l);
            }
            if let Some(r) = ind_right {
                ind.right.get_or_insert(r);
            }
        }

        // Vertical inset + block above/below fold into the first/last para's
        // before/after spacing.
        let is_first = seen_para == 0;
        let is_last = seen_para == para_count.saturating_sub(1);
        let before = if is_first {
            sum_opt(above, inset_top)
        } else {
            None
        };
        let after = if is_last { sum_opt(below, inset_bottom) } else { None };
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

    out.extend(inner);
    Ok(())
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

/// Ensures a paragraph is non-empty (used where Word requires content).
pub fn empty_para() -> Para {
    Para { props: Default::default(), content: vec![ParaChild::Run(Run::Text {
        props: RunProps::default(),
        text: "".into(),
    })] }
}

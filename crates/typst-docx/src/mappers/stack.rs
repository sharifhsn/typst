//! Stack mapper: lowers a `StackElem` into native DOCX instead of rasterizing
//! it. A stack is a pure layout container — its children are ordinary block
//! content — so the flowing arrangement maps cleanly:
//!
//! - A **vertical** stack (`dir: ttb`/`btt`, the default) is just its children
//!   one after another → emit each child's blocks in stacking order.
//! - A **horizontal** stack (`dir: ltr`/`rtl`) places its children side by side
//!   → a single borderless table row, one cell per child (mirroring how a
//!   layout `#grid` lowers to a `w:tbl`).
//!
//! Fixed inter-child spacing is structural layout information: vertical gaps
//! become [`Block::FlowSpace`] and horizontal gaps become borderless table
//! tracks. Fractional spacing remains approximate because Word has no access to
//! Typst's measured leftover region, but it is retained as a flexible track and
//! recorded in the fidelity report instead of disappearing silently.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Packed, Resolve, StyleChain};
use typst_library::layout::{Axis, Spacing as StackSpacing, StackChild, StackElem};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Cell, CellBorders, Para, ParaChild, ParaProps, Row, Run, RunProps, Tbl,
    TblProps,
};
use crate::report::{DecisionReason, ExportSource, LossSet, Representation};

enum HorizontalItem<'a> {
    Body(&'a Content),
    FixedSpace(i32),
    FlexibleSpace(f64),
}

pub fn stack(
    elem: &Packed<StackElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let dir = elem.dir.get(styles);

    // A negative direction (`btt`/`rtl`) reverses source order when represented
    // in Word's top-to-bottom / left-to-right flow. Reverse spacing alongside
    // content so an explicit gap stays between the same logical neighbors.
    let mut children: Vec<_> = elem.children.iter().collect();
    if !dir.is_positive() {
        children.reverse();
    }

    if dir.axis() == Axis::Y {
        vertical_stack(elem, &children, styles, ctx)
    } else {
        horizontal_stack(elem, &children, styles, ctx)
    }
}

fn vertical_stack(
    elem: &Packed<StackElem>,
    children: &[&StackChild],
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let default_spacing = elem.spacing.get(styles);
    let mut deferred = None;
    let mut approximate = false;
    let mut out = Vec::new();

    for child in children {
        match child {
            StackChild::Spacing(spacing) => {
                approximate |= push_vertical_spacing(&mut out, *spacing, styles, ctx);
                deferred = None;
            }
            StackChild::Block(body) => {
                if let Some(spacing) = deferred.take() {
                    approximate |= push_vertical_spacing(&mut out, spacing, styles, ctx);
                }
                out.extend(ctx.blocks(body, styles)?);
                deferred = default_spacing;
            }
        }
    }

    if approximate {
        record_flexible_spacing(elem, ctx);
    }
    Ok(out)
}

fn push_vertical_spacing(
    out: &mut Vec<Block>,
    spacing: StackSpacing,
    styles: StyleChain,
    ctx: &DocxCtx,
) -> bool {
    match spacing {
        StackSpacing::Rel(rel) => {
            let abs = crate::props::abs_to_twip(rel.abs.resolve(styles));
            let relative = rel.rel.get() * ctx.raster_height.to_pt() * 20.0;
            let dxa = abs + relative.round() as i32;
            if dxa > 0 {
                out.push(Block::FlowSpace { dxa });
            }
            // A percentage vertical gap is tied to Typst's resolved region
            // height; the current page height is only a flowing-model proxy.
            !rel.rel.is_zero()
        }
        StackSpacing::Fr(_) => true,
    }
}

fn horizontal_stack(
    elem: &Packed<StackElem>,
    children: &[&StackChild],
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let available = ctx.available_width_dxa();
    let default_spacing = elem.spacing.get(styles);
    let mut deferred = None;
    let mut approximate = false;
    let mut items = Vec::new();

    for child in children {
        match child {
            StackChild::Spacing(spacing) => {
                approximate |=
                    push_horizontal_spacing(&mut items, *spacing, styles, available);
                deferred = None;
            }
            StackChild::Block(body) => {
                if let Some(spacing) = deferred.take() {
                    approximate |=
                        push_horizontal_spacing(&mut items, spacing, styles, available);
                }
                items.push(HorizontalItem::Body(body));
                deferred = default_spacing;
            }
        }
    }

    if !items.iter().any(|item| matches!(item, HorizontalItem::Body(_))) {
        return Ok(Vec::new());
    }

    let col_dxa = allocate_horizontal_widths(&items, available);
    let mut cells = Vec::with_capacity(items.len());
    for (item, &col_w) in items.iter().zip(&col_dxa) {
        match item {
            HorizontalItem::Body(body) => {
                let mut blocks =
                    ctx.with_available_width(col_w, |ctx| ctx.blocks(body, styles))?;
                ensure_ends_in_para(&mut blocks);
                cells.push(cell(col_w, blocks));
            }
            HorizontalItem::FixedSpace(_) | HorizontalItem::FlexibleSpace(_) => {
                cells.push(cell(col_w, vec![empty_para()]));
            }
        }
    }

    if approximate {
        record_flexible_spacing(elem, ctx);
    }

    Ok(vec![Block::Table(Tbl {
        props: TblProps { width_dxa: Some(col_dxa.iter().sum()), style: None },
        grid: col_dxa,
        rows: vec![Row {
            header: false,
            cant_split: false,
            height: None,
            cells,
        }],
    })])
}

fn push_horizontal_spacing<'a>(
    items: &mut Vec<HorizontalItem<'a>>,
    spacing: StackSpacing,
    styles: StyleChain,
    available: i32,
) -> bool {
    match spacing {
        StackSpacing::Rel(rel) => {
            let abs = crate::props::abs_to_twip(rel.abs.resolve(styles));
            let relative = rel.rel.get() * available as f64;
            let dxa = abs + relative.round() as i32;
            if dxa > 0 {
                items.push(HorizontalItem::FixedSpace(dxa));
            }
            false
        }
        StackSpacing::Fr(fr) => {
            items.push(HorizontalItem::FlexibleSpace(fr.get().max(0.0)));
            true
        }
    }
}

fn allocate_horizontal_widths(items: &[HorizontalItem<'_>], available: i32) -> Vec<i32> {
    let fixed: i32 = items
        .iter()
        .filter_map(|item| match item {
            HorizontalItem::FixedSpace(width) => Some(*width),
            _ => None,
        })
        .sum();
    let flexible_count = items
        .iter()
        .filter(|item| !matches!(item, HorizontalItem::FixedSpace(_)))
        .count() as i32;
    let weight: f64 = items
        .iter()
        .map(|item| match item {
            HorizontalItem::Body(_) => 1.0,
            HorizontalItem::FlexibleSpace(weight) => *weight,
            HorizontalItem::FixedSpace(_) => 0.0,
        })
        .sum();
    let target = available.max(fixed + flexible_count.max(1));
    let distributable = (target - fixed).max(flexible_count.max(1));
    let mut widths: Vec<i32> = items
        .iter()
        .map(|item| match item {
            HorizontalItem::FixedSpace(width) => *width,
            HorizontalItem::Body(_) => {
                ((distributable as f64 / weight.max(1.0)).round() as i32).max(1)
            }
            HorizontalItem::FlexibleSpace(item_weight) => {
                ((distributable as f64 * *item_weight / weight.max(1.0)).round() as i32)
                    .max(1)
            }
        })
        .collect();

    let delta = target - widths.iter().sum::<i32>();
    if let Some(index) = items
        .iter()
        .rposition(|item| !matches!(item, HorizontalItem::FixedSpace(_)))
    {
        widths[index] = (widths[index] + delta).max(1);
    }
    widths
}

fn record_flexible_spacing(elem: &Packed<StackElem>, ctx: &mut DocxCtx) {
    ctx.fidelity_report.record_span(
        ExportSource::new("stack", elem.span(), elem.location()),
        Representation::Approximate,
        DecisionReason::FlexibleStackSpacing,
        LossSet::VISUAL_ONLY,
        0,
    );
}

fn cell(width: i32, blocks: Vec<Block>) -> Cell {
    Cell {
        w_dxa: Some(width),
        grid_span: 1,
        v_merge: None,
        borders: CellBorders::default(),
        shd_fill: None,
        valign: None,
        blocks,
    }
}

fn empty_para() -> Block {
    Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Text {
            props: RunProps::default(),
            text: "".into(),
        })],
    })
}

/// Every `w:tc` must contain ≥1 block and end in a `w:p` (Word requirement).
fn ensure_ends_in_para(blocks: &mut Vec<Block>) {
    if !matches!(blocks.last(), Some(Block::Para(_))) {
        blocks.push(empty_para());
    }
}

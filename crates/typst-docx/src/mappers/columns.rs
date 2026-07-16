//! Manual block-column breaks need physical editable cells in DOCX.
//!
//! Word's native section columns are retained for automatic overflow. When an
//! authored `#colbreak()` advances within an explicit `#columns(..)` region, a
//! borderless one-row table preserves the already-known physical columns and
//! their top alignment in Word-compatible consumers.

use std::ops::ControlFlow;

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::layout::{Abs, ColbreakElem, ColumnsElem};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Cell, CellBorders, CellMargins, Para, ParaChild, ParaProps, Row, Run,
    RunProps, Tbl, TblProps, VAlign,
};

/// Whether this columns element can be represented as one physical table row.
///
/// A break after the final physical column advances beyond this region and is
/// intentionally left to native section flow instead.
pub(crate) fn uses_table(elem: &Packed<ColumnsElem>, styles: StyleChain) -> bool {
    let breaks = column_break_count(&elem.body);
    breaks > 0 && breaks < elem.count.get(styles).get()
}

pub(crate) fn columns(
    elem: &Packed<ColumnsElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let count = elem.count.get(styles).get();
    let available = ctx.available_width_dxa().max(count as i32);
    let reference = Abs::pt(available as f64 / 20.0);
    let mut gutter =
        crate::props::abs_to_twip(elem.gutter.resolve(styles).relative_to(reference))
            .max(0);
    if count > 1 {
        gutter = gutter.min((available - count as i32).max(0) / (count - 1) as i32);
    }
    let total_gutter = gutter.saturating_mul(count.saturating_sub(1) as i32);
    let content_width = (available - total_gutter).max(count as i32);
    let base_width = (content_width / count as i32).max(1);
    let mut widths = vec![base_width; count];
    if let Some(last) = widths.last_mut() {
        *last += content_width - base_width * count as i32;
    }

    // Every authored column has the same physical content width (apart from a
    // whole-twip rounding remainder assigned to the final cell). Lower relative
    // children against that width, not the enclosing page width. Otherwise a
    // `width: 100%` image or nested table is sized for the whole page before it
    // is inserted into a half-width Word cell, inflating the row and forcing
    // content onto later pages.
    let blocks =
        ctx.with_available_width(base_width, |ctx| ctx.blocks(&elem.body, styles))?;
    let mut columns = split_at_column_breaks(blocks);
    columns.resize_with(count, Vec::new);

    let mut grid = Vec::with_capacity(count * 2 - 1);
    let mut cells = Vec::with_capacity(count * 2 - 1);
    for (index, (mut blocks, width)) in columns.into_iter().zip(widths).enumerate() {
        ensure_ends_in_para(&mut blocks);
        grid.push(width);
        cells.push(cell(width, blocks));
        if index + 1 < count {
            grid.push(gutter);
            cells.push(cell(gutter, vec![empty_para()]));
        }
    }

    Ok(vec![Block::Table(Tbl {
        props: TblProps { width_dxa: Some(available), style: None, jc: None },
        grid,
        rows: vec![Row {
            header: false,
            cant_split: false,
            height: None,
            cells,
        }],
    })])
}

fn column_break_count(body: &Content) -> usize {
    let mut count = 0;
    let _ = body.traverse(&mut |content: Content| {
        if content.is::<ColbreakElem>() {
            count += 1;
        }
        ControlFlow::<()>::Continue(())
    });
    count
}

fn split_at_column_breaks(blocks: Vec<Block>) -> Vec<Vec<Block>> {
    let mut columns = vec![Vec::new()];
    for block in blocks {
        let Block::Para(Para { props, content }) = block else {
            columns.last_mut().unwrap().push(block);
            continue;
        };

        let mut paragraph = Vec::new();
        for child in content {
            if matches!(child, ParaChild::Run(Run::ColumnBreak)) {
                if !paragraph.is_empty() {
                    columns.last_mut().unwrap().push(Block::Para(Para {
                        props: props.clone(),
                        content: std::mem::take(&mut paragraph),
                    }));
                }
                columns.push(Vec::new());
            } else {
                paragraph.push(child);
            }
        }
        if !paragraph.is_empty() {
            columns
                .last_mut()
                .unwrap()
                .push(Block::Para(Para { props, content: paragraph }));
        }
    }
    columns
}

fn cell(width: i32, blocks: Vec<Block>) -> Cell {
    Cell {
        w_dxa: Some(width),
        grid_span: 1,
        v_merge: None,
        borders: CellBorders::default(),
        shd_fill: None,
        margins: CellMargins::default(),
        valign: Some(VAlign::Top),
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

/// Every `w:tc` must contain at least one block and end in `w:p`.
fn ensure_ends_in_para(blocks: &mut Vec<Block>) {
    if !matches!(blocks.last(), Some(Block::Para(_))) {
        blocks.push(empty_para());
    }
}

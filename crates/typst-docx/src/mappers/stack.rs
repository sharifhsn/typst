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
//! Inter-child `Spacing` carries no content and is dropped (a horizontal
//! spacer's only job — pushing neighbours apart — has no flowing equivalent,
//! and vertical spacing folds into the surrounding paragraph spacing). This is
//! the same "keep the text, approximate the geometry" trade the grid mapper
//! makes; the alternative is rasterizing the whole stack and losing the text.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::layout::{Axis, StackChild, StackElem};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Cell, CellBorders, Para, ParaChild, ParaProps, Row, Run, RunProps, Tbl,
    TblProps,
};

/// Default content width (dxa) used to size a horizontal stack's columns; the
/// real text-area width is not known at this post-realize stage. Matches the
/// table mapper's `DEFAULT_CONTENT_DXA`.
const DEFAULT_CONTENT_DXA: i32 = 9360;

pub fn stack(
    elem: &Packed<StackElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let dir = elem.dir.get(styles);

    // The block children, in stacking order (a negative direction — `btt`/`rtl`
    // — reverses the visual order relative to source order).
    let mut bodies: Vec<_> = elem
        .children
        .iter()
        .filter_map(|c| match c {
            StackChild::Block(content) => Some(content),
            StackChild::Spacing(_) => None,
        })
        .collect();
    if !dir.is_positive() {
        bodies.reverse();
    }

    if dir.axis() == Axis::Y {
        // Vertical: the children flow one below another — emit their blocks in
        // order.
        let mut out = Vec::new();
        for body in bodies {
            out.extend(ctx.blocks(body, styles)?);
        }
        Ok(out)
    } else {
        // Horizontal: a single row of side-by-side cells.
        if bodies.is_empty() {
            return Ok(Vec::new());
        }
        let ncols = bodies.len();
        let col_w = (DEFAULT_CONTENT_DXA / ncols as i32).max(1);
        let col_dxa = vec![col_w; ncols];

        let mut cells = Vec::with_capacity(ncols);
        for body in bodies {
            let mut blocks = ctx.blocks(body, styles)?;
            ensure_ends_in_para(&mut blocks);
            cells.push(Cell {
                w_dxa: Some(col_w),
                grid_span: 1,
                v_merge: None,
                borders: CellBorders::default(),
                shd_fill: None,
                valign: None,
                blocks,
            });
        }

        let tbl = Tbl {
            props: TblProps { width_dxa: Some(col_w * ncols as i32), style: None },
            grid: col_dxa,
            rows: vec![Row {
                header: false,
                cant_split: false,
                height: None,
                cells,
            }],
        };
        Ok(vec![Block::Table(tbl)])
    }
}

/// Every `w:tc` must contain ≥1 block and end in a `w:p` (Word requirement).
fn ensure_ends_in_para(blocks: &mut Vec<Block>) {
    if !matches!(blocks.last(), Some(Block::Para(_))) {
        blocks.push(Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(Run::Text {
                props: RunProps::default(),
                text: "".into(),
            })],
        }));
    }
}

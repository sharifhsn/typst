//! Table mapper: lowers a `TableElem`'s resolved [`CellGrid`] into the DOCX
//! `w:tbl` IR (grid + rows + cells, with colspan/rowspan/fill/stroke/align).
//!
//! See `research/tables.md` (§3–9) for the OOXML constructs and invariants this
//! honors:
//! - `w:tblGrid` with one `w:gridCol` per non-gutter column (dxa widths).
//! - `w:tr`/`w:tc`; `w:trPr/w:tblHeader` on `table.header` rows.
//! - colspan → `w:gridSpan`; rowspan → `w:vMerge` restart/continue (continuation
//!   placeholders are emitted as real `w:tc` with an empty `w:p`, per §7b).
//! - cell fill → `w:shd`; per-edge stroke → `w:tcBorders`; vertical align →
//!   `w:vAlign`; horizontal align → the cell paragraph's `w:jc`.
//!
//! The grid is already fully resolved (`elem.grid`), so colspan/rowspan/fill/
//! stroke are precomputed exactly as in the HTML `show_cellgrid` path.

use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, Smart, StyleChain};
use typst_library::layout::resolve::{Cell as ResolvedCell, CellGrid, Entry};
use typst_library::layout::{Abs, Alignment, Sizing, VAlignment};
use typst_library::layout::{GridCell, GridElem};
use typst_library::model::{TableCell, TableElem};
use typst_library::visualize::{Color, Paint, Stroke};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Border, Cell, CellBorders, Jc, Para, ParaChild, ParaProps, Row, RowHeight,
    Run, RunProps, Tbl, TblProps, VAlign, VMerge,
};

/// Default content width (in dxa / twips) used to size flexible (`fr`/`auto`)
/// columns when no absolute width is given. 9360 dxa = 6.5in = US Letter width
/// (8.5in) minus the default 1in left/right margins. The true laid-out widths
/// are not available at this (post-realize, pre-layout) stage.
//
// INTEGRATION-NEEDED: thread the active `SectPr` content width (page_w minus
// left/right margins) down to the table mapper so flexible columns size against
// the real text area instead of this US-Letter assumption. A `ctx` accessor
// returning the current section's content width would suffice.
const DEFAULT_CONTENT_DXA: f64 = 9360.0;

pub fn table(
    elem: &Packed<TableElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    cellgrid(elem.grid.as_ref().unwrap(), styles, ctx)
}

/// A layout grid (`#grid`) resolves to the same [`CellGrid`] as a table, so it
/// lowers to a `w:tbl` too — preserving its content as editable text instead of
/// rasterizing it (or, for a full-page layout grid like a CV sidebar/main split,
/// dropping it entirely). A grid's cells carry no stroke, so no cell borders are
/// emitted; the result reads as a borderless multi-column layout.
pub fn grid(
    elem: &Packed<GridElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let Some(grid) = elem.grid.as_ref() else {
        return Ok(Vec::new());
    };
    cellgrid(grid, styles, ctx)
}

/// Lowers a resolved [`CellGrid`] (shared by `#table` and `#grid`) into a
/// `w:tbl`.
fn cellgrid(
    grid: &CellGrid,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let ncols = grid.non_gutter_column_count();
    if ncols == 0 || grid.entries.is_empty() {
        return Ok(Vec::new());
    }
    let nrows = grid.entries.len() / ncols;

    // -- Column widths (dxa) for `w:tblGrid` -------------------------------
    let col_dxa = resolve_column_widths(grid, ncols);
    let width_dxa: i32 = col_dxa.iter().copied().sum();

    // -- Header rows (mark `table.header` rows for `w:tblHeader`) -----------
    // `grid.headers` ranges are in gutter-doubled row coordinates; convert to
    // non-gutter row indices (mirrors the HTML `show_cellgrid` header range
    // conversion).
    let is_header_row = |y: usize| -> bool {
        grid.headers.iter().any(|hd| {
            let range = if grid.has_gutter {
                hd.range.start / 2..hd.range.end.div_ceil(2)
            } else {
                hd.range.clone()
            };
            range.contains(&y)
        })
    };

    // -- Rows --------------------------------------------------------------
    let mut rows = Vec::with_capacity(nrows);
    for y in 0..nrows {
        let mut cells = Vec::with_capacity(ncols);

        let mut x = 0;
        while x < ncols {
            let entry = &grid.entries[y * ncols + x];
            match entry {
                Entry::Cell(cell) => {
                    let colspan = cell.colspan.get().max(1);
                    let rowspan = cell.rowspan.get().max(1);

                    // Cell preferred width = sum of the spanned column widths.
                    let span_end = (x + colspan).min(ncols);
                    let w_dxa: i32 = col_dxa[x..span_end].iter().copied().sum();

                    let v_merge = (rowspan > 1).then_some(VMerge::Restart);

                    cells.push(build_cell(
                        ctx,
                        cell,
                        styles,
                        colspan as u32,
                        v_merge,
                        Some(w_dxa),
                    )?);

                    x = span_end;
                }
                Entry::Merged { parent } => {
                    // Position of the cell this entry is merged with, in the
                    // same non-gutter entries lattice.
                    let py = parent / ncols;
                    if py < y {
                        // Vertical continuation: emit a real placeholder `w:tc`
                        // carrying `w:vMerge` (continue) and the origin's
                        // `gridSpan`, with an empty paragraph (§7b/§7c).
                        let origin = parent_cell(grid, *parent);
                        let colspan = origin.map_or(1, |c| c.colspan.get().max(1));
                        let span_end = (x + colspan).min(ncols);
                        let w_dxa: i32 = col_dxa[x..span_end].iter().copied().sum();

                        cells.push(continuation_cell(colspan as u32, Some(w_dxa)));
                        x = span_end;
                    } else {
                        // Horizontal merge: absorbed by the origin cell's
                        // `gridSpan` to our left; emit nothing for this column.
                        x += 1;
                    }
                }
            }
        }

        rows.push(Row {
            header: is_header_row(y),
            cant_split: row_cant_split(grid, y),
            height: row_height(grid, y),
            cells,
        });
    }

    let tbl = Tbl {
        props: TblProps { width_dxa: Some(width_dxa), style: None },
        grid: col_dxa,
        rows,
    };

    Ok(vec![Block::Table(tbl)])
}

/// Builds a content cell (`Entry::Cell`) into the DOCX IR.
fn build_cell(
    ctx: &mut DocxCtx,
    cell: &ResolvedCell,
    styles: StyleChain,
    grid_span: u32,
    v_merge: Option<VMerge>,
    w_dxa: Option<i32>,
) -> SourceResult<Cell> {
    // Cell fill → `w:shd`.
    let shd_fill = cell.fill.as_ref().and_then(paint_to_rgb);

    // Per-edge stroke → `w:tcBorders`.
    let borders = CellBorders {
        top: side_border(&cell.stroke.top),
        bottom: side_border(&cell.stroke.bottom),
        left: side_border(&cell.stroke.left),
        right: side_border(&cell.stroke.right),
    };

    // Alignment: the resolved cell folds its effective alignment back onto the
    // `TableCell` body, so read it from there.
    let (jc, valign) = cell_alignment(cell, styles);

    // Cell body → blocks. The body is the packed `TableCell`; lower its inner
    // `body` content through the shared block pipeline.
    let mut blocks = cell_blocks(ctx, cell, styles, jc)?;

    // §0/§2: every `w:tc` must contain ≥1 block and END in a `w:p`.
    ensure_ends_in_para(&mut blocks);

    Ok(Cell {
        w_dxa,
        grid_span: grid_span.max(1),
        v_merge,
        borders,
        shd_fill,
        valign,
        blocks,
    })
}

/// A vertical-merge continuation placeholder cell (§7b): real `w:tc` carrying
/// `w:vMerge` (continue) + the origin's `gridSpan`, content = a single empty
/// paragraph.
fn continuation_cell(grid_span: u32, w_dxa: Option<i32>) -> Cell {
    Cell {
        w_dxa,
        grid_span: grid_span.max(1),
        v_merge: Some(VMerge::Continue),
        borders: CellBorders::default(),
        shd_fill: None,
        valign: None,
        blocks: vec![empty_para_block()],
    }
}

/// Lowers a resolved cell's inner body into blocks, applying the cell's
/// horizontal alignment to each resulting top-level paragraph (Word puts
/// horizontal alignment on the cell paragraph's `w:jc`, not on `w:tcPr`).
fn cell_blocks(
    ctx: &mut DocxCtx,
    cell: &ResolvedCell,
    styles: StyleChain,
    jc: Option<Jc>,
) -> SourceResult<Vec<Block>> {
    // The resolved cell body is a packed `TableCell`; its `body` field is the
    // actual content. Fall back to the body content directly if it is not a
    // `TableCell` (e.g. a bare cell).
    let content = cell
        .body
        .to_packed::<TableCell>()
        .map(|tc| tc.body.clone())
        .or_else(|| cell.body.to_packed::<GridCell>().map(|gc| gc.body.clone()))
        .unwrap_or_else(|| cell.body.clone());

    let mut blocks = ctx.blocks(&content, styles)?;

    if let Some(jc) = jc {
        for block in &mut blocks {
            if let Block::Para(para) = block
                && para.props.jc.is_none()
            {
                para.props.jc = Some(jc);
            }
        }
    }

    Ok(blocks)
}

/// Reads the resolved cell's effective alignment off its `TableCell` body and
/// splits it into a paragraph `w:jc` (horizontal) + a `w:vAlign` (vertical).
/// `Smart::Auto` leaves both unspecified (Word's defaults — top/left — already
/// match Typst's cell defaults).
fn cell_alignment(cell: &ResolvedCell, styles: StyleChain) -> (Option<Jc>, Option<VAlign>) {
    // The resolved cell body is a `TableCell` for a `#table` but a `GridCell` for
    // a `#grid` (which the DOCX backend also lowers to a `w:tbl`); read alignment
    // off whichever it is, or neither.
    let align = if let Some(tc) = cell.body.to_packed::<TableCell>() {
        tc.align.get(styles)
    } else if let Some(gc) = cell.body.to_packed::<GridCell>() {
        gc.align.get(styles)
    } else {
        return (None, None);
    };
    let Smart::Custom(align) = align else {
        return (None, None);
    };
    align_to_docx(align)
}

/// Maps a Typst [`Alignment`] to (`w:jc`, `w:vAlign`).
fn align_to_docx(align: Alignment) -> (Option<Jc>, Option<VAlign>) {
    use typst_library::layout::HAlignment;

    let jc = align.x().map(|h| match h {
        HAlignment::Start => Jc::Start,
        HAlignment::Left => Jc::Start,
        HAlignment::Center => Jc::Center,
        HAlignment::Right => Jc::End,
        HAlignment::End => Jc::End,
    });

    let valign = align.y().map(|v| match v {
        VAlignment::Top => VAlign::Top,
        VAlignment::Horizon => VAlign::Center,
        VAlignment::Bottom => VAlign::Bottom,
    });

    (jc, valign)
}

/// Resolves the origin cell of a merged entry, if present.
fn parent_cell(grid: &CellGrid, parent: usize) -> Option<&ResolvedCell> {
    grid.entries.get(parent).and_then(Entry::as_cell)
}

/// Per-side stroke → a DOCX [`Border`]. After `resolve_cell`, every side is
/// present: `Some(stroke)` ⇒ draw it; `None` ⇒ no border for that edge.
//
// INTEGRATION-NEEDED: the encoder for `w:tcBorders` should emit an explicit
// `<w:* w:val="nil"/>` for sides whose `Border` is `None`, so that Typst
// `stroke: none` turns the inherited table-style border OFF rather than
// inheriting it (research/tables.md §6, §12.10). The `CellBorders` IR can only
// express "draw this border" (`Some`) vs "unspecified" (`None`); making
// `stroke: none` faithful needs the encoder to treat `None` sides as `nil`
// (acceptable here because the table itself emits no `w:tblBorders`/style, so
// "unspecified" already renders borderless in practice).
fn side_border(side: &Option<Arc<Stroke<Abs>>>) -> Option<Border> {
    let stroke = side.as_ref()?;
    Some(stroke_to_border(stroke))
}

/// Converts a resolved [`Stroke`] to a DOCX [`Border`] (thickness in eighths of
/// a point, color as `RRGGBB`). Word borders are solid single lines; dash/cap
/// nuance is flattened.
fn stroke_to_border(stroke: &Stroke<Abs>) -> Border {
    // Default Typst table stroke thickness is 1pt; `Smart::Auto` ⇒ inherit it.
    let thickness_pt = match stroke.thickness {
        Smart::Custom(abs) => abs.to_pt(),
        Smart::Auto => 1.0,
    };
    // `w:sz` is in eighths of a point; clamp to the valid 2..=96 range.
    let sz = ((thickness_pt * 8.0).round() as i64).clamp(2, 96) as u32;

    // Default stroke paint is black; `Smart::Auto` ⇒ black.
    let color = match &stroke.paint {
        Smart::Custom(paint) => paint_to_rgb(paint).unwrap_or([0, 0, 0]),
        Smart::Auto => [0, 0, 0],
    };

    Border { sz, color }
}

/// A solid [`Paint`] → its `RRGGBB` bytes, dropping alpha. Gradients/patterns
/// (which Word cannot represent on a cell background) are dropped.
fn paint_to_rgb(paint: &Paint) -> Option<[u8; 3]> {
    match paint {
        Paint::Solid(color) => Some(color_to_rgb(color)),
        _ => None,
    }
}

fn color_to_rgb(color: &Color) -> [u8; 3] {
    let [r, g, b, _] = color.to_vec4_u8();
    [r, g, b]
}

/// Per-column widths in dxa for the `w:tblGrid`. Absolute (`Rel`) columns use
/// their resolved width; flexible (`fr`/`auto`) columns share the remaining
/// content width — `fr` proportionally to its fraction, `auto` with weight 1.
fn resolve_column_widths(grid: &CellGrid, ncols: usize) -> Vec<i32> {
    // Iterate the non-gutter column tracks. `grid.cols` includes gutter tracks
    // (every other entry) when `has_gutter`; the content columns are the even
    // indices.
    let content_cols: Vec<Sizing> = if grid.has_gutter {
        grid.cols.iter().copied().step_by(2).take(ncols).collect()
    } else {
        grid.cols.iter().copied().take(ncols).collect()
    };

    // First pass: fixed widths + flexible weights.
    let mut widths = vec![0.0_f64; ncols];
    let mut fixed_total = 0.0;
    let mut flex_weight = 0.0;
    let mut flex_indices = Vec::new();

    for (i, sizing) in content_cols.iter().enumerate().take(ncols) {
        match sizing {
            Sizing::Rel(rel) => {
                // Absolute part (pt → dxa) plus relative part against the
                // default content width. `em` is ignored (rare in column
                // tracks; no font context available here).
                let abs_dxa = rel.abs.abs.to_pt() * 20.0;
                let rel_dxa = rel.rel.get() * DEFAULT_CONTENT_DXA;
                let w = abs_dxa + rel_dxa;
                widths[i] = w;
                fixed_total += w;
            }
            Sizing::Fr(fr) => {
                let weight = fr.get().max(0.0);
                flex_weight += weight;
                flex_indices.push((i, weight));
            }
            Sizing::Auto => {
                flex_weight += 1.0;
                flex_indices.push((i, 1.0));
            }
        }
    }

    // Distribute the remaining content width across flexible columns.
    if !flex_indices.is_empty() {
        let remaining = (DEFAULT_CONTENT_DXA - fixed_total).max(0.0);
        if flex_weight > 0.0 && remaining > 0.0 {
            for (i, weight) in &flex_indices {
                widths[*i] = remaining * weight / flex_weight;
            }
        } else {
            // No room left (or zero total weight): give each flexible column a
            // small nonzero share so the grid stays well-formed.
            let share = (remaining / flex_indices.len() as f64).max(1.0);
            for (i, _) in &flex_indices {
                widths[*i] = share;
            }
        }
    }

    widths
        .into_iter()
        .map(|w| (w.round() as i32).max(1))
        .collect()
}

/// Row height from the resolved row track, if it is an absolute size. `fr`/
/// `auto` rows grow with content ⇒ no explicit `trHeight`.
fn row_height(grid: &CellGrid, y: usize) -> Option<RowHeight> {
    let sizing = row_sizing(grid, y)?;
    match sizing {
        Sizing::Rel(rel) if rel.rel.get() == 0.0 => {
            let dxa = (rel.abs.abs.to_pt() * 20.0).round() as i32;
            // A fixed Typst row height is a MINIMUM — content taller than it
            // overflows, never clips. Word's `hRule="exact"` clips, so use
            // `atLeast` to match (LibreOffice grows either way, masking this).
            (dxa > 0).then_some(RowHeight { val: dxa, exact: false })
        }
        _ => None,
    }
}

/// The resolved [`Sizing`] of non-gutter row `y`.
fn row_sizing(grid: &CellGrid, y: usize) -> Option<Sizing> {
    if grid.has_gutter {
        grid.rows.get(y * 2).copied()
    } else {
        grid.rows.get(y).copied()
    }
}

/// Whether a row is unbreakable. A row is kept together when every cell whose
/// rowspan is fully contained in it is unbreakable.
fn row_cant_split(grid: &CellGrid, y: usize) -> bool {
    let ncols = grid.non_gutter_column_count();
    let mut any = false;
    for x in 0..ncols {
        if let Some(Entry::Cell(cell)) = grid.entries.get(y * ncols + x) {
            any = true;
            if cell.breakable {
                return false;
            }
        }
    }
    // Only assert `cantSplit` when the row actually owns ≥1 origin cell and all
    // are unbreakable; otherwise let Word paginate freely.
    any
}

/// Appends an empty paragraph if `blocks` is empty or does not end in one
/// (Word requires every `w:tc` to end in a `w:p`).
fn ensure_ends_in_para(blocks: &mut Vec<Block>) {
    let ends_in_para = matches!(blocks.last(), Some(Block::Para(_)));
    if blocks.is_empty() || !ends_in_para {
        blocks.push(empty_para_block());
    }
}

/// A single empty `<w:p/>` block.
fn empty_para_block() -> Block {
    Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Text {
            props: RunProps::default(),
            text: "".into(),
        })],
    })
}

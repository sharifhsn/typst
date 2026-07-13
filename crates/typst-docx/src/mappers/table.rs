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
use typst_library::foundations::{Content, Packed, Smart, StyleChain};
use typst_library::layout::resolve::{Cell as ResolvedCell, CellGrid, Entry};
use typst_library::layout::{Abs, Alignment, Sizing, VAlignment};
use typst_library::layout::{GridCell, GridElem};
use typst_library::model::{TableCell, TableElem};
use typst_library::visualize::{Color, Paint, Stroke};
use typst_utils::Numeric;

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Border, Cell, CellBorders, Jc, Para, ParaChild, ParaProps, Row, RowHeight,
    Run, RunProps, Tbl, TblProps, VAlign, VMerge,
};
use crate::report::{DecisionReason, LossSet, Representation};

enum TablePlan<'a> {
    Native { grid: &'a CellGrid, measured: Option<MeasuredTableGeometry> },
    Approximate { grid: &'a CellGrid, measured: Option<MeasuredTableGeometry> },
    Empty,
    Raster,
}

struct MeasuredTableGeometry {
    columns_dxa: Vec<i32>,
    row_heights: Vec<Option<RowHeight>>,
}

pub fn table(
    elem: &Packed<TableElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let source = elem.clone().pack();
    execute_table_plan(
        &source,
        preflight_table(&source, elem.grid.as_deref(), ctx),
        styles,
        ctx,
    )
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
    let source = elem.clone().pack();
    execute_table_plan(
        &source,
        preflight_table(&source, elem.grid.as_deref(), ctx),
        styles,
        ctx,
    )
}

fn preflight_table<'a>(
    source: &Content,
    grid: Option<&'a CellGrid>,
    ctx: &DocxCtx,
) -> TablePlan<'a> {
    let Some(grid) = grid else { return TablePlan::Raster };
    if grid.non_gutter_column_count() == 0 || grid.entries.is_empty() {
        return TablePlan::Empty;
    }
    let measured = measured_table_geometry(source, grid, ctx);
    if table_geometry_is_approximate(grid, measured.as_ref()) {
        TablePlan::Approximate { grid, measured }
    } else {
        TablePlan::Native { grid, measured }
    }
}

fn execute_table_plan(
    source: &Content,
    plan: TablePlan<'_>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    match plan {
        TablePlan::Native { grid, measured } => {
            ctx.record_content_decision(
                source,
                Representation::Native,
                DecisionReason::NativeTable,
                LossSet::default(),
                content_text_chars(source),
            );
            cellgrid(grid, styles, ctx, measured.as_ref())
        }
        TablePlan::Approximate { grid, measured } => {
            ctx.record_content_decision(
                source,
                Representation::Approximate,
                DecisionReason::TableGeometryApproximation,
                LossSet::VISUAL_ONLY,
                content_text_chars(source),
            );
            cellgrid(grid, styles, ctx, measured.as_ref())
        }
        TablePlan::Empty => Ok(Vec::new()),
        TablePlan::Raster => {
            let runs = crate::mappers::image::laid_out_fallback(source, styles, ctx)?;
            if runs.is_empty() {
                ctx.record_content_decision(
                    source,
                    Representation::Drop,
                    DecisionReason::TableResolutionUnavailable,
                    LossSet::DROP,
                    content_text_chars(source),
                );
                ctx.warn_message(
                    "table/grid resolution and whole-region fallback produced no output",
                    source.span(),
                );
                Ok(Vec::new())
            } else {
                Ok(vec![Block::Para(Para {
                    props: ParaProps::default(),
                    content: runs.into_iter().map(ParaChild::Run).collect(),
                })])
            }
        }
    }
}

fn table_geometry_is_approximate(
    grid: &CellGrid,
    measured: Option<&MeasuredTableGeometry>,
) -> bool {
    let column_sizing = measured.is_none()
        && grid.cols.iter().any(|sizing| match sizing {
            Sizing::Auto | Sizing::Fr(_) => true,
            Sizing::Rel(rel) => rel.rel.get() != 0.0 || !rel.abs.em.is_zero(),
        });
    let rows_are_measured =
        measured.is_some_and(|geometry| geometry.row_heights.iter().all(Option::is_some));
    let row_sizing = !rows_are_measured
        && grid.rows.iter().any(|sizing| match sizing {
            Sizing::Fr(_) => true,
            Sizing::Rel(rel) => rel.rel.get() != 0.0 || !rel.abs.em.is_zero(),
            Sizing::Auto => false,
        });
    let cell_visuals = grid.entries.iter().any(|entry| {
        let Some(cell) = entry.as_cell() else { return false };
        cell.fill.as_ref().is_some_and(paint_is_approximate)
            || [
                &cell.stroke.top,
                &cell.stroke.bottom,
                &cell.stroke.left,
                &cell.stroke.right,
            ]
            .into_iter()
            .flatten()
            .any(|stroke| stroke_is_approximate(stroke))
    });
    column_sizing || row_sizing || cell_visuals || grid.footer.is_some()
}

fn paint_is_approximate(paint: &Paint) -> bool {
    match paint {
        Paint::Solid(color) => color.to_vec4_u8()[3] != u8::MAX,
        _ => true,
    }
}

fn stroke_is_approximate(stroke: &Stroke<Abs>) -> bool {
    let paint = match &stroke.paint {
        Smart::Custom(paint) => paint_is_approximate(paint),
        Smart::Auto => false,
    };
    paint
        || matches!(stroke.dash, Smart::Custom(Some(_)))
        || matches!(stroke.cap, Smart::Custom(_))
        || matches!(stroke.join, Smart::Custom(_))
        || matches!(stroke.miter_limit, Smart::Custom(_))
}

fn content_text_chars(content: &Content) -> usize {
    use std::ops::ControlFlow;
    use typst_library::text::TextElem;

    let mut chars = 0;
    let _ = content.traverse(&mut |element: Content| {
        if let Some(text) = element.to_packed::<TextElem>() {
            chars += text.text.chars().count();
        }
        ControlFlow::<()>::Continue(())
    });
    chars
}

/// Lowers a resolved [`CellGrid`] (shared by `#table` and `#grid`) into a
/// `w:tbl`.
fn cellgrid(
    grid: &CellGrid,
    styles: StyleChain,
    ctx: &mut DocxCtx,
    measured: Option<&MeasuredTableGeometry>,
) -> SourceResult<Vec<Block>> {
    let ncols = grid.non_gutter_column_count();
    if ncols == 0 || grid.entries.is_empty() {
        return Ok(Vec::new());
    }
    let nrows = grid.entries.len() / ncols;

    // -- Column widths (dxa) for `w:tblGrid` -------------------------------
    // `CellGrid` doubles both axes when either axis has gutters, inserting
    // zero-sized tracks on the other axis. Do not leak those normalization-only
    // tracks into Word: a row-only gutter must not create phantom columns.
    let has_column_gutter = has_nonzero_column_gutter(grid);
    let col_dxa = measured.map_or_else(
        || resolve_column_widths(grid, ctx.available_width_dxa(), has_column_gutter),
        |geometry| geometry.columns_dxa.clone(),
    );
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
        let mut cells = Vec::with_capacity(col_dxa.len());

        let mut x = 0;
        while x < ncols {
            let entry = &grid.entries[y * ncols + x];
            match entry {
                Entry::Cell(cell) => {
                    let colspan = cell.colspan.get().max(1);
                    let rowspan = cell.rowspan.get().max(1);

                    let span_end = (x + colspan).min(ncols);
                    let (grid_start, grid_end) =
                        spanned_grid_range(has_column_gutter, x, span_end);
                    let w_dxa: i32 = col_dxa[grid_start..grid_end].iter().copied().sum();

                    let v_merge = (rowspan > 1).then_some(VMerge::Restart);

                    cells.push(build_cell(
                        ctx,
                        cell,
                        styles,
                        (grid_end - grid_start) as u32,
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
                        let (grid_start, grid_end) =
                            spanned_grid_range(has_column_gutter, x, span_end);
                        let w_dxa: i32 =
                            col_dxa[grid_start..grid_end].iter().copied().sum();

                        // A vMerge continuation still carries the merged region's
                        // SIDE borders on every row (left/right), its BOTTOM only
                        // on the final row, and no TOP (that edge is interior to
                        // the merged cell). Deriving them from the origin's
                        // resolved stroke keeps a rowspan cell's box closed in a
                        // bordered table; `CellBorders::default()` (all `nil`)
                        // left the lower rows open on the sides and bottom.
                        let borders = origin.map_or_else(CellBorders::default, |o| {
                            let last_row = py + o.rowspan.get().max(1) - 1;
                            CellBorders {
                                top: None,
                                bottom: if y == last_row {
                                    side_border(&o.stroke.bottom)
                                } else {
                                    None
                                },
                                left: side_border(&o.stroke.left),
                                right: side_border(&o.stroke.right),
                            }
                        });

                        cells.push(continuation_cell(
                            (grid_end - grid_start) as u32,
                            Some(w_dxa),
                            borders,
                        ));
                        x = span_end;
                    } else {
                        // Horizontal merge: absorbed by the origin cell's
                        // `gridSpan` to our left; emit nothing for this column.
                        x += 1;
                    }
                }
            }

            // A Typst column gutter is a real track, not width that vanishes.
            // Emit a borderless empty cell after the source cell/span so the
            // following content starts at the same x-position as in Typst.
            if let Some(gutter_width) =
                column_gutter_after(has_column_gutter, &col_dxa, x, ncols)
            {
                cells.push(spacer_cell(gutter_width, 1));
            }
        }

        rows.push(Row {
            header: is_header_row(y),
            cant_split: row_cant_split(grid, y),
            height: measured
                .and_then(|geometry| geometry.row_heights.get(y).cloned().flatten())
                .or_else(|| row_height(grid, y)),
            cells,
        });

        if y + 1 < nrows
            && let Some(height) = row_gutter_height(grid, y, ctx.raster_height)
        {
            rows.push(gutter_row(grid, y, ncols, &col_dxa, has_column_gutter, height));
        }
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
    let mut blocks = cell_blocks(ctx, cell, styles, jc, w_dxa)?;

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
fn continuation_cell(grid_span: u32, w_dxa: Option<i32>, borders: CellBorders) -> Cell {
    Cell {
        w_dxa,
        grid_span: grid_span.max(1),
        v_merge: Some(VMerge::Continue),
        borders,
        shd_fill: None,
        valign: None,
        blocks: vec![empty_para_block()],
    }
}

fn spacer_cell(width_dxa: i32, grid_span: u32) -> Cell {
    Cell {
        w_dxa: Some(width_dxa.max(1)),
        grid_span: grid_span.max(1),
        v_merge: None,
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
    width_dxa: Option<i32>,
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

    let mut blocks = if let Some(width_dxa) = width_dxa {
        ctx.with_available_width(width_dxa, |ctx| ctx.blocks(&content, styles))?
    } else {
        ctx.blocks(&content, styles)?
    };

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
fn cell_alignment(
    cell: &ResolvedCell,
    styles: StyleChain,
) -> (Option<Jc>, Option<VAlign>) {
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
        // Word cell shading cannot carry a gradient. A midpoint color retains
        // the source's visual tone instead of silently turning the cell white;
        // `TablePlan` reports the approximation before this executes.
        Paint::Gradient(gradient) => representative_gradient_rgb(gradient.stops_ref()),
        Paint::Tiling(_) => None,
    }
}

fn representative_gradient_rgb(
    stops: &[(Color, typst_library::layout::Ratio)],
) -> Option<[u8; 3]> {
    if stops.is_empty() {
        return None;
    }
    let mut sums = [0_u64; 3];
    for (color, _) in stops {
        for (sum, channel) in sums.iter_mut().zip(color_to_rgb(color)) {
            *sum += channel as u64;
        }
    }
    let count = stops.len() as u64;
    Some(sums.map(|sum| ((sum + count / 2) / count) as u8))
}

fn color_to_rgb(color: &Color) -> [u8; 3] {
    let (r, g, b, a) = color.to_rgb().into_format::<u8, u8>().into_components();
    // `w:shd` and table borders have no alpha. Composite over Word's default
    // white page/cell background rather than dropping alpha or making a
    // translucent color unexpectedly opaque and too dark.
    let composite = |channel: u8| -> u8 {
        let value = channel as u32 * a as u32 + 255 * (255 - a as u32);
        ((value + 127) / 255) as u8
    };
    [composite(r), composite(g), composite(b)]
}

/// Resolves Word's table grid from the final paged cell regions rather than
/// redistributing flexible tracks against an estimated flowing width.
fn measured_table_geometry(
    source: &Content,
    grid: &CellGrid,
    ctx: &DocxCtx,
) -> Option<MeasuredTableGeometry> {
    let logical_id = typst_export_common::paged::logical_id(source);
    let table = ctx.paged_geometry.first_table(logical_id)?;
    if table.cells.is_empty() || table.cells.iter().any(|cell| !cell.axis_aligned) {
        return None;
    }

    let ncols = grid.non_gutter_column_count();
    let nrows = grid.entries.len().checked_div(ncols)?;
    let mut column_samples = vec![Vec::<f64>::new(); ncols];
    let mut row_samples = vec![Vec::<(usize, f64)>::new(); nrows];
    for cell in &table.cells {
        if cell.colspan == 1 && cell.x < ncols {
            column_samples[cell.x].push(cell.width_pt);
        }
        if cell.rowspan == 1 && cell.y < nrows {
            row_samples[cell.y].push((cell.page, cell.height_pt));
        }
    }
    let columns = column_samples
        .into_iter()
        .map(median_positive)
        .collect::<Option<Vec<_>>>()?;

    let has_column_gutter = has_nonzero_column_gutter(grid);
    let gutters = if has_column_gutter {
        let mut samples = vec![Vec::<f64>::new(); ncols.saturating_sub(1)];
        for left in &table.cells {
            if left.colspan != 1 || left.x + 1 >= ncols {
                continue;
            }
            if let Some(right) = table.cells.iter().find(|right| {
                right.page == left.page
                    && right.y == left.y
                    && right.x == left.x + 1
                    && right.colspan == 1
            }) {
                let gap = right.left_pt - (left.left_pt + left.width_pt);
                if gap > 0.0 {
                    samples[left.x].push(gap);
                }
            }
        }
        samples.into_iter().map(median_positive).collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };

    let mut columns_dxa = Vec::with_capacity(columns.len() + gutters.len());
    for (index, width) in columns.into_iter().enumerate() {
        columns_dxa.push(pt_to_positive_dxa(width)?);
        if let Some(gutter) = gutters.get(index) {
            columns_dxa.push(pt_to_positive_dxa(*gutter)?);
        }
    }

    let row_heights = row_samples
        .into_iter()
        .map(|samples| {
            let pages = samples
                .iter()
                .map(|(page, _)| *page)
                .collect::<std::collections::BTreeSet<_>>();
            if pages.len() != 1 {
                return None;
            }
            median_positive(samples.into_iter().map(|(_, height)| height).collect())
                .and_then(|height| {
                    pt_to_positive_dxa(height).map(|val| RowHeight {
                        // `atLeast` preserves editability and avoids clipping if
                        // Word substitutes a font with taller metrics.
                        val,
                        exact: false,
                    })
                })
        })
        .collect();

    Some(MeasuredTableGeometry { columns_dxa, row_heights })
}

fn median_positive(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|value| value.is_finite() && *value > 0.0);
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

fn pt_to_positive_dxa(points: f64) -> Option<i32> {
    let dxa = (points * 20.0).round();
    (dxa.is_finite() && dxa >= 1.0 && dxa <= i32::MAX as f64).then_some(dxa as i32)
}

/// Per-track widths in dxa for `w:tblGrid`, including Typst gutter tracks.
/// Absolute/relative tracks resolve against the current scoped width; `fr` and
/// `auto` share what remains.
fn resolve_column_widths(
    grid: &CellGrid,
    available_dxa: i32,
    has_column_gutter: bool,
) -> Vec<i32> {
    let tracks: Vec<_> = if grid.has_gutter && !has_column_gutter {
        grid.cols.iter().step_by(2).copied().collect()
    } else {
        grid.cols.clone()
    };
    let available_dxa = available_dxa.max(1) as f64;
    let mut widths = vec![0.0_f64; tracks.len()];
    let mut fixed_total = 0.0;
    let mut flex_weight = 0.0;
    let mut flex_indices = Vec::new();

    for (i, sizing) in tracks.iter().enumerate() {
        match sizing {
            Sizing::Rel(rel) => {
                // `em` is ignored (rare in grid tracks; no font context here).
                let abs_dxa = rel.abs.abs.to_pt() * 20.0;
                let rel_dxa = rel.rel.get() * available_dxa;
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
        let remaining = (available_dxa - fixed_total).max(0.0);
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

    widths.into_iter().map(|w| (w.round() as i32).max(1)).collect()
}

/// Output-grid range occupied by source content columns `start..end`. With
/// gutters, a colspan includes each gutter between its content tracks.
fn spanned_grid_range(
    has_column_gutter: bool,
    start: usize,
    end: usize,
) -> (usize, usize) {
    if has_column_gutter { (start * 2, end * 2 - 1) } else { (start, end) }
}

fn column_gutter_after(
    has_column_gutter: bool,
    widths: &[i32],
    next_content_x: usize,
    ncols: usize,
) -> Option<i32> {
    (has_column_gutter && next_content_x < ncols)
        .then(|| widths.get(next_content_x * 2 - 1).copied())
        .flatten()
}

fn has_nonzero_column_gutter(grid: &CellGrid) -> bool {
    grid.has_gutter
        && grid
            .cols
            .iter()
            .skip(1)
            .step_by(2)
            .any(|track| !matches!(track, Sizing::Rel(rel) if rel.is_zero()))
}

fn row_gutter_height(grid: &CellGrid, y: usize, reference: Abs) -> Option<RowHeight> {
    if !grid.has_gutter {
        return None;
    }
    let Sizing::Rel(rel) = *grid.rows.get(y * 2 + 1)? else { return None };
    let dxa = rel.abs.abs.to_pt() * 20.0 + rel.rel.get() * reference.to_pt() * 20.0;
    (dxa > 0.0).then_some(RowHeight {
        val: dxa.round() as i32,
        // This is an actual spatial gap, not a minimum content row.
        exact: true,
    })
}

/// A physical spacer row for a Typst row gutter. Vertical merges that cross the
/// gap receive another `vMerge continue` cell so Word does not terminate the
/// merge at the inserted row.
fn gutter_row(
    grid: &CellGrid,
    y: usize,
    ncols: usize,
    widths: &[i32],
    has_column_gutter: bool,
    height: RowHeight,
) -> Row {
    let next_y = y + 1;
    let mut cells = Vec::with_capacity(widths.len());
    let mut x = 0;
    while x < ncols {
        let entry = &grid.entries[next_y * ncols + x];
        if let Entry::Merged { parent } = entry
            && parent / ncols <= y
            && let Some(origin) = parent_cell(grid, *parent)
        {
            let colspan = origin.colspan.get().max(1);
            let end = (x + colspan).min(ncols);
            let (grid_start, grid_end) = spanned_grid_range(has_column_gutter, x, end);
            let width = widths[grid_start..grid_end].iter().copied().sum();
            cells.push(continuation_cell(
                (grid_end - grid_start) as u32,
                Some(width),
                CellBorders {
                    top: None,
                    bottom: None,
                    left: side_border(&origin.stroke.left),
                    right: side_border(&origin.stroke.right),
                },
            ));
            x = end;
        } else {
            let grid_x = if has_column_gutter { x * 2 } else { x };
            cells.push(spacer_cell(widths[grid_x], 1));
            x += 1;
        }
        if let Some(gutter_width) =
            column_gutter_after(has_column_gutter, widths, x, ncols)
        {
            cells.push(spacer_cell(gutter_width, 1));
        }
    }

    Row {
        header: false,
        cant_split: true,
        height: Some(height),
        cells,
    }
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

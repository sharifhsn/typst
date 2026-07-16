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
use typst_library::foundations::{Content, Packed, Resolve, Smart, StyleChain};
use typst_library::layout::resolve::{
    Cell as ResolvedCell, CellGrid, Entry, Line, LinePosition,
};
use typst_library::layout::{Abs, Alignment, Sizing, VAlignment};
use typst_library::layout::{GridCell, GridElem};
use typst_library::model::{TableCell, TableElem};
use typst_library::visualize::{Color, Paint, Stroke};
use typst_utils::Numeric;

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Border, BreakKind, Cell, CellBorders, CellMargins, Jc, Para, ParaChild,
    ParaProps, ReviewCandidateKind, Row, RowHeight, Run, RunProps, Spacing, Tbl,
    TblProps, VAlign, VMerge,
};
use crate::report::{DecisionReason, LossSet, Representation};

enum TablePlan<'a> {
    Native { grid: &'a CellGrid, measured: Option<MeasuredTableGeometry> },
    Approximate { grid: &'a CellGrid, measured: Option<MeasuredTableGeometry> },
    Empty,
    Raster(DecisionReason),
}

struct MeasuredTableGeometry {
    columns_dxa: Vec<i32>,
    row_heights: Vec<Option<RowHeight>>,
}

#[derive(Clone, Copy)]
struct CellGeometry {
    width_dxa: Option<i32>,
    height_dxa: Option<i32>,
    layout_grid: bool,
    centered_grid_inset: bool,
}

/// Word stores table-cell insets in whole twips, while its text layout can
/// consume a little more horizontal space than Typst for the same embedded
/// face. Leave a one-tenth-point tolerance on each side of a centered,
/// symmetric layout-grid cell so a glyph that fits the authored physical cell
/// does not wrap solely at the integer-DXA boundary. Unlike vertical row-box
/// reconciliation, this tolerance does not depend on measured row geometry.
const LAYOUT_GRID_INLINE_TOLERANCE_DXA: i32 = 2;

#[derive(Clone, Copy, Eq, PartialEq)]
enum TableOrigin {
    SemanticTable,
    LayoutGrid,
}

pub fn table(
    elem: &Packed<TableElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let source = elem.clone().pack();
    execute_table_plan(
        &source,
        preflight_table(
            &source,
            elem.grid.as_deref(),
            TableOrigin::SemanticTable,
            styles,
            ctx,
        ),
        TableOrigin::SemanticTable,
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
        preflight_table(
            &source,
            elem.grid.as_deref(),
            TableOrigin::LayoutGrid,
            styles,
            ctx,
        ),
        TableOrigin::LayoutGrid,
        styles,
        ctx,
    )
}

fn preflight_table<'a>(
    source: &Content,
    grid: Option<&'a CellGrid>,
    origin: TableOrigin,
    styles: StyleChain,
    ctx: &DocxCtx,
) -> TablePlan<'a> {
    let Some(grid) = grid else {
        return TablePlan::Raster(DecisionReason::RasterFallback);
    };
    if grid.non_gutter_column_count() == 0 || grid.entries.is_empty() {
        return TablePlan::Empty;
    }
    let measured = measured_table_geometry(source, grid, origin, ctx);
    if origin == TableOrigin::SemanticTable
        && measured.as_ref().is_some_and(|geometry| {
            measured_table_needs_tight_typography_fallback(grid, geometry, styles, ctx)
        })
    {
        return TablePlan::Raster(DecisionReason::TightTableTypographyRasterFallback);
    }
    if table_geometry_is_approximate(grid, measured.as_ref()) {
        TablePlan::Approximate { grid, measured }
    } else {
        TablePlan::Native { grid, measured }
    }
}

/// Whether Typst's measured table fits the page but Word's native text model
/// provably cannot retain that height without clipping or shrinking glyphs.
///
/// Typst measures a text row from the font's actual frame edges. Word gives an
/// unconfigured table paragraph at least the nominal font size, then adds the
/// authored cell margins. Dense register/opcode maps can therefore fit exactly
/// in Typst while a native `w:tbl` must either split across pages or use an
/// `exact` row/line height that clips glyphs. Preserve those tables atomically
/// with the existing rendered fallback (and hidden searchable text) only when
/// the measured whole fits one page and this lower bound exceeds it.
fn measured_table_needs_tight_typography_fallback(
    grid: &CellGrid,
    measured: &MeasuredTableGeometry,
    styles: StyleChain,
    ctx: &DocxCtx,
) -> bool {
    let ncols = grid.non_gutter_column_count();
    if ncols == 0 || measured.row_heights.len() * ncols != grid.entries.len() {
        return false;
    }
    let Some(mut measured_total) = measured
        .row_heights
        .iter()
        .try_fold(0_i32, |sum, height| Some(sum.saturating_add(height.as_ref()?.val)))
    else {
        return false;
    };
    for y in 0..measured.row_heights.len().saturating_sub(1) {
        if let Some(gutter) = row_gutter_height(grid, y, ctx.raster_height) {
            measured_total = measured_total.saturating_add(gutter.val);
        }
    }
    let page_height = crate::props::abs_to_twip(ctx.available_height).max(1);
    if measured_total > page_height {
        return false;
    }

    let has_column_gutter = has_nonzero_column_gutter(grid);
    let mut native_minimum = 0_i32;
    for (y, height) in measured.row_heights.iter().enumerate() {
        let height = height.unwrap();
        let Some(nominal_line) = grid.entries[y * ncols..(y + 1) * ncols]
            .iter()
            .filter_map(Entry::as_cell)
            .map(|cell| single_line_nominal_text_dxa(&cell.body, styles))
            .try_fold(0_i32, |maximum, size| Some(maximum.max(size?)))
        else {
            return false;
        };
        let minimum = if nominal_line > 0 {
            nominal_line.saturating_add(row_vertical_inset(
                grid,
                y,
                &measured.columns_dxa,
                has_column_gutter,
                styles,
                height.val,
            ))
        } else {
            height.val
        };
        native_minimum = native_minimum.saturating_add(height.val.max(minimum));
        if y + 1 < measured.row_heights.len()
            && let Some(gutter) = row_gutter_height(grid, y, ctx.raster_height)
        {
            native_minimum = native_minimum.saturating_add(gutter.val);
        }
    }

    native_minimum > page_height
}

/// Returns the largest effective nominal font size in a cell whose material
/// text is provably single-line. Unknown rich containers deliberately return
/// `None`: the compatibility fallback is an optimization, never permission to
/// flatten a table whose Word line-box lower bound is uncertain.
fn single_line_nominal_text_dxa(content: &Content, styles: StyleChain) -> Option<i32> {
    use typst_library::foundations::{
        SequenceElem, ShowSet, Smart, StyledElem, SymbolElem,
    };
    use typst_library::introspection::TagElem;
    use typst_library::layout::{BoxElem, Sizing};
    use typst_library::model::StrongElem;
    use typst_library::text::{RawElem, SpaceElem, TextElem};

    if let Some(cell) = content.to_packed::<TableCell>() {
        return single_line_nominal_text_dxa(&cell.body, styles);
    }
    if let Some(cell) = content.to_packed::<GridCell>() {
        return single_line_nominal_text_dxa(&cell.body, styles);
    }
    if let Some(styled) = content.to_packed::<StyledElem>() {
        return single_line_nominal_text_dxa(&styled.child, styles.chain(&styled.styles));
    }
    if let Some(sequence) = content.to_packed::<SequenceElem>() {
        return sequence.children.iter().try_fold(0_i32, |maximum, child| {
            Some(maximum.max(single_line_nominal_text_dxa(child, styles)?))
        });
    }
    if let Some(strong) = content.to_packed::<StrongElem>() {
        return single_line_nominal_text_dxa(&strong.body, styles);
    }
    if let Some(text) = content.to_packed::<TextElem>() {
        if text.text.contains('\n') {
            return None;
        }
        return Some(if text.text.is_empty() {
            0
        } else {
            crate::props::abs_to_twip(styles.resolve(TextElem::size)).max(1)
        });
    }
    if let Some(symbol) = content.to_packed::<SymbolElem>() {
        if symbol.text.contains('\n') {
            return None;
        }
        return Some(if symbol.text.is_empty() {
            0
        } else {
            crate::props::abs_to_twip(styles.resolve(TextElem::size)).max(1)
        });
    }
    if let Some(raw) = content.to_packed::<RawElem>() {
        if raw.block.get(styles) || content.plain_text().contains('\n') {
            return None;
        }
        let raw_styles = raw.show_set(styles);
        let raw_styles = styles.chain(&raw_styles);
        return Some(if content.plain_text().is_empty() {
            0
        } else {
            crate::props::abs_to_twip(raw_styles.resolve(TextElem::size)).max(1)
        });
    }
    if let Some(boxed) = content.to_packed::<BoxElem>() {
        if boxed.width.get(styles) != Sizing::Auto
            || boxed.height.get(styles) != Smart::Auto
            || !boxed.inset.resolve(styles).unwrap_or_default().is_zero()
            || !crate::ctx::box_is_plain(boxed, styles)
        {
            return None;
        }
        return single_line_nominal_text_dxa(
            boxed.body.get_ref(styles).as_ref()?,
            styles,
        );
    }
    if content.is::<SpaceElem>() || content.is::<TagElem>() {
        return Some(0);
    }
    None
}

fn execute_table_plan(
    source: &Content,
    plan: TablePlan<'_>,
    origin: TableOrigin,
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
            cellgrid(grid, styles, ctx, measured.as_ref(), origin)
        }
        TablePlan::Approximate { grid, measured } => {
            ctx.record_content_decision(
                source,
                Representation::Approximate,
                DecisionReason::TableGeometryApproximation,
                LossSet::VISUAL_ONLY,
                content_text_chars(source),
            );
            cellgrid(grid, styles, ctx, measured.as_ref(), origin)
        }
        TablePlan::Empty => Ok(Vec::new()),
        TablePlan::Raster(reason) => {
            let runs = crate::mappers::image::laid_out_fallback_with_reason(
                source, styles, ctx, reason,
            )?;
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
    origin: TableOrigin,
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
        let measured_row_height =
            measured.and_then(|geometry| geometry.row_heights.get(y).copied().flatten());
        let full_row_height = measured_row_height.or_else(|| row_height(grid, y));
        let centered_grid_inset = origin == TableOrigin::LayoutGrid
            && measured_row_height.is_some_and(|height| {
                row_uses_centered_symmetric_inset(
                    grid,
                    y,
                    &col_dxa,
                    has_column_gutter,
                    styles,
                    height.val,
                )
            });
        let mut emitted_row_height = measured_row_height
            .map(|height| RowHeight {
                // Typst's physical cell region already includes its vertical
                // inset. Word adds `w:tcMar` outside the `w:trHeight` minimum,
                // so subtract the largest non-spanning cell inset or the row
                // is forced taller by that same inset a second time.
                val: if centered_grid_inset {
                    height.val
                } else {
                    height
                        .val
                        .saturating_sub(row_vertical_inset(
                            grid,
                            y,
                            &col_dxa,
                            has_column_gutter,
                            styles,
                            height.val,
                        ))
                        .max(1)
                },
                exact: height.exact,
            })
            .or(full_row_height);

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

                    let mut built = build_cell(
                        ctx,
                        cell,
                        styles,
                        (grid_end - grid_start) as u32,
                        v_merge,
                        CellGeometry {
                            width_dxa: Some(w_dxa),
                            height_dxa: full_row_height.map(|height| height.val),
                            layout_grid: origin == TableOrigin::LayoutGrid,
                            centered_grid_inset,
                        },
                    )?;
                    apply_explicit_grid_lines(
                        grid,
                        x,
                        y,
                        colspan,
                        rowspan,
                        &mut built.borders,
                    );
                    cells.push(built);

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
                        let mut borders = origin.map_or_else(CellBorders::default, |o| {
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
                        apply_explicit_grid_lines(grid, x, y, colspan, 1, &mut borders);

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

        // A physically measured row with no layout-bearing content cannot clip
        // text, math, drawings, fields, or authored breaks. Preserve its exact
        // Typst height instead of letting Word grow the mandatory empty cell
        // paragraphs to the consumer's default line box. This is especially
        // important for dense maps with intentionally blank/shaded rows, but is
        // provenance- and content-based rather than tied to any document.
        if measured_row_height.is_some()
            && row_is_layout_empty(&cells)
            && let Some(height) = emitted_row_height.as_mut()
        {
            height.exact = true;
        }

        rows.push(Row {
            header: is_header_row(y),
            cant_split: row_cant_split(grid, y),
            height: emitted_row_height,
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

fn row_is_layout_empty(cells: &[Cell]) -> bool {
    cells.iter().all(|cell| {
        cell.blocks.iter().all(|block| match block {
            Block::Para(para) => para.content.iter().all(para_child_is_layout_empty),
            Block::Tag(_) => true,
            Block::FlowSpace { dxa } => *dxa == 0,
            Block::Table(_) | Block::Toc(_) | Block::SectionBreak(_) => false,
        })
    })
}

fn para_child_is_layout_empty(child: &ParaChild) -> bool {
    match child {
        ParaChild::Run(Run::Text { props, text }) => {
            props.vanish || text.trim().is_empty()
        }
        ParaChild::Hyperlink { runs, .. } => runs.iter().all(|run| {
            matches!(
                run,
                Run::Text { props, text } if props.vanish || text.trim().is_empty()
            )
        }),
        ParaChild::BookmarkStart { .. }
        | ParaChild::BookmarkEnd { .. }
        | ParaChild::Tag(_) => true,
        ParaChild::Run(_) | ParaChild::OmmlPara(_) => false,
    }
}

/// A centered layout-grid cell already encodes equal top and bottom inset in
/// the measured physical row height. Word adds `w:tcMar` outside
/// `w:trHeight`, so carrying both makes short repeated grid rows grow by the
/// inset again. Preserve the full measured row and let the existing centered
/// alignment realize that symmetric space instead.
fn row_uses_centered_symmetric_inset(
    grid: &CellGrid,
    y: usize,
    col_dxa: &[i32],
    has_column_gutter: bool,
    styles: StyleChain,
    height_dxa: i32,
) -> bool {
    let ncols = grid.non_gutter_column_count();
    let mut has_inset = false;
    for x in 0..ncols {
        let Entry::Cell(cell) = &grid.entries[y * ncols + x] else {
            // Spans make the row's independent physical height ambiguous.
            return false;
        };
        if cell.rowspan.get() != 1 || cell.colspan.get() != 1 {
            return false;
        }
        let (grid_start, grid_end) = spanned_grid_range(has_column_gutter, x, x + 1);
        let width_dxa = col_dxa[grid_start..grid_end].iter().copied().sum();
        let margins = cell_margins(cell, styles, Some(width_dxa), Some(height_dxa));
        if margins.top != margins.bottom {
            return false;
        }
        if margins.top > 0 {
            let (_, valign) = cell_alignment(cell, styles);
            if valign != Some(VAlign::Center) {
                return false;
            }
            has_inset = true;
        }
    }
    has_inset
}

fn row_vertical_inset(
    grid: &CellGrid,
    y: usize,
    col_dxa: &[i32],
    has_column_gutter: bool,
    styles: StyleChain,
    height_dxa: i32,
) -> i32 {
    let ncols = grid.non_gutter_column_count();
    let mut maximum = 0;
    for x in 0..ncols {
        let Entry::Cell(cell) = &grid.entries[y * ncols + x] else { continue };
        if cell.rowspan.get() != 1 {
            continue;
        }
        let span_end = (x + cell.colspan.get().max(1)).min(ncols);
        let (grid_start, grid_end) = spanned_grid_range(has_column_gutter, x, span_end);
        let width_dxa = col_dxa[grid_start..grid_end].iter().copied().sum();
        let margins = cell_margins(cell, styles, Some(width_dxa), Some(height_dxa));
        maximum = maximum.max(margins.top.saturating_add(margins.bottom));
    }
    maximum
}

/// Builds a content cell (`Entry::Cell`) into the DOCX IR.
fn build_cell(
    ctx: &mut DocxCtx,
    cell: &ResolvedCell,
    styles: StyleChain,
    grid_span: u32,
    v_merge: Option<VMerge>,
    geometry: CellGeometry,
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
    let mut margins = cell_margins(cell, styles, geometry.width_dxa, geometry.height_dxa);
    if geometry.centered_grid_inset && valign == Some(VAlign::Center) {
        margins.top = 0;
        margins.bottom = 0;
    }
    if geometry.layout_grid
        && valign == Some(VAlign::Center)
        && margins.left == margins.right
        && margins.left > 0
    {
        margins.left = margins.left.saturating_sub(LAYOUT_GRID_INLINE_TOLERANCE_DXA);
        margins.right = margins.right.saturating_sub(LAYOUT_GRID_INLINE_TOLERANCE_DXA);
    }

    // Cell body → blocks. The body is the packed `TableCell`; lower its inner
    // `body` content through the shared block pipeline.
    let content_width = geometry.width_dxa.map(|width| {
        width
            .saturating_sub(margins.left)
            .saturating_sub(margins.right)
            .max(1)
    });
    let mut blocks = cell_blocks(ctx, cell, styles, jc, content_width)?;

    if geometry.height_dxa.is_some() {
        trim_trailing_structural_breaks(&mut blocks);
    }

    // §0/§2: every `w:tc` must contain ≥1 block and END in a `w:p`.
    ensure_ends_in_para(&mut blocks);

    Ok(Cell {
        w_dxa: geometry.width_dxa,
        grid_span: grid_span.max(1),
        v_merge,
        borders,
        shd_fill,
        margins,
        valign,
        blocks,
    })
}

/// Removes only trailing run-only spacing fallbacks from a physically measured
/// cell. Its measured row height already includes the authored paragraph/VElem
/// space, so serializing those fallbacks again as line breaks double-counts the
/// bottom of the cell. Authored linebreaks and all interior structural breaks
/// remain intact.
fn trim_trailing_structural_breaks(blocks: &mut [Block]) {
    let Some(Block::Para(para)) =
        blocks.iter_mut().rev().find(|block| !matches!(block, Block::Tag(_)))
    else {
        return;
    };

    let last_semantic = para.content.iter().rposition(|child| {
        !matches!(
            child,
            ParaChild::Tag(_)
                | ParaChild::Run(Run::Break { kind: BreakKind::Structural })
        )
    });
    let mut index = 0usize;
    para.content.retain(|child| {
        let keep = !matches!(
            child,
            ParaChild::Run(Run::Break {
                kind: BreakKind::Structural
            }) if last_semantic.is_none_or(|last| index > last)
        );
        index += 1;
        keep
    });
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
        margins: CellMargins::default(),
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
        margins: CellMargins::default(),
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

    collapse_par_spacing(&mut blocks);

    let paragraph_count =
        blocks.iter().filter(|block| matches!(block, Block::Para(_))).count();
    for block in &mut blocks {
        let Block::Para(para) = block else { continue };
        if let Some(origin) = para.props.review_origin.as_mut() {
            origin.kind = ReviewCandidateKind::TableCell;
        }
    }
    let has_review_origin = blocks.iter().any(
        |block| matches!(block, Block::Para(para) if para.props.review_origin.is_some()),
    );
    if paragraph_count == 1
        && !has_review_origin
        && blocks
            .iter()
            .all(|block| matches!(block, Block::Para(_) | Block::Tag(_)))
    {
        let origin = ctx.review_origin(
            crate::convert::review_span(&content),
            ReviewCandidateKind::TableCell,
        );
        if let Some(Block::Para(para)) =
            blocks.iter_mut().find(|block| matches!(block, Block::Para(_)))
        {
            para.props.review_origin = Some(origin);
        }
    }

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

/// Normalizes Typst's collapsing `par.spacing` in a sequence of Word blocks.
///
/// Typst contributes the maximum spacing at each adjacent paragraph boundary
/// and nothing at the sequence's outer edges. Word consumers do not consistently
/// collapse matching `after`/`before` values, so store each boundary once on the
/// following paragraph. Remove only the recorded paragraph-spacing component;
/// explicit `#v()` space folded into `before` remains intact.
pub(crate) fn collapse_par_spacing(blocks: &mut [Block]) {
    let mut run = Vec::new();
    for index in 0..blocks.len() {
        match &blocks[index] {
            Block::Para(_) => run.push(index),
            Block::Tag(_) => {}
            _ => {
                collapse_par_spacing_run(blocks, &run);
                run.clear();
            }
        }
    }
    collapse_par_spacing_run(blocks, &run);
}

fn collapse_par_spacing_run(blocks: &mut [Block], run: &[usize]) {
    let amounts = run
        .iter()
        .map(|&index| match &blocks[index] {
            Block::Para(para) => (
                para.props.typst_par_spacing_before.unwrap_or(0),
                para.props.typst_par_spacing_after.unwrap_or(0),
            ),
            _ => (0, 0),
        })
        .collect::<Vec<_>>();

    for &index in run {
        let Block::Para(para) = &mut blocks[index] else { continue };
        collapse_par_spacing_side(&mut para.props, true);
        collapse_par_spacing_side(&mut para.props, false);
    }

    for (position, &index) in run.iter().enumerate().skip(1) {
        let gap = amounts[position - 1].1.max(amounts[position].0);
        if gap == 0 {
            continue;
        }
        let Block::Para(para) = &mut blocks[index] else { continue };
        let spacing = para.props.spacing.get_or_insert_with(Default::default);
        spacing.before = Some(spacing.before.unwrap_or(0).saturating_add(gap));
    }
}

fn collapse_par_spacing_side(props: &mut ParaProps, before: bool) {
    let amount = if before {
        props.typst_par_spacing_before
    } else {
        props.typst_par_spacing_after
    };
    let Some(amount) = amount else { return };
    let Some(spacing) = props.spacing.as_mut() else { return };
    let side = if before { &mut spacing.before } else { &mut spacing.after };
    *side = side
        .map(|value| value.saturating_sub(amount).max(0))
        .filter(|value| *value != 0);
    if *spacing == Spacing::default() {
        props.spacing = None;
    }
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

/// Reads the resolved cell inset folded onto its packed table/grid cell and
/// maps it to Word's per-cell margins (`w:tcMar`). Percentage components use
/// the resolved cell width/row height as their axis reference; absolute insets
/// (the common case) map exactly to twips.
fn cell_margins(
    cell: &ResolvedCell,
    styles: StyleChain,
    width_dxa: Option<i32>,
    height_dxa: Option<i32>,
) -> CellMargins {
    let inset = if let Some(cell) = cell.body.to_packed::<TableCell>() {
        cell.inset.get(styles)
    } else if let Some(cell) = cell.body.to_packed::<GridCell>() {
        cell.inset.get(styles)
    } else {
        return CellMargins::default();
    };
    let Smart::Custom(inset) = inset else {
        return CellMargins::default();
    };

    let width = Abs::pt(width_dxa.unwrap_or(0).max(0) as f64 / 20.0);
    let height = Abs::pt(height_dxa.unwrap_or(0).max(0) as f64 / 20.0);
    let margin = |value: Option<
        typst_library::layout::Rel<typst_library::layout::Length>,
    >,
                  reference: Abs| {
        value.map_or(0, |value| {
            crate::props::abs_to_twip(value.resolve(styles).relative_to(reference)).max(0)
        })
    };

    CellMargins {
        top: margin(inset.top, height),
        right: margin(inset.right, width),
        bottom: margin(inset.bottom, height),
        left: margin(inset.left, width),
    }
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

/// Overlays resolved `table.hline` / `table.vline` instructions onto the cell
/// edges Word can represent. CellGrid keeps explicit lines separate from the
/// cells' base stroke, so ignoring these vectors silently drops authored rules.
/// A line must cover the complete edge of a spanning cell; partial rules inside
/// one merged Word cell remain outside the native border model.
fn apply_explicit_grid_lines(
    grid: &CellGrid,
    x: usize,
    y: usize,
    colspan: usize,
    rowspan: usize,
    borders: &mut CellBorders,
) {
    let x_end = x.saturating_add(colspan);
    let y_end = y.saturating_add(rowspan);

    apply_line_override(
        &mut borders.top,
        horizontal_line_override(grid, y, x, x_end, true),
    );
    // A vertically merged cell's bottom edge is serialized on its final
    // continuation row, not the restart cell. Its explicit bottom is applied
    // there by the continuation path.
    if rowspan == 1 {
        apply_line_override(
            &mut borders.bottom,
            horizontal_line_override(grid, y_end, x, x_end, false),
        );
    }
    apply_line_override(
        &mut borders.left,
        vertical_line_override(grid, x, y, y_end, true),
    );
    apply_line_override(
        &mut borders.right,
        vertical_line_override(grid, x_end, y, y_end, false),
    );
}

fn horizontal_line_override(
    grid: &CellGrid,
    boundary: usize,
    start: usize,
    end: usize,
    leading: bool,
) -> Option<Option<Border>> {
    let (index, position) =
        line_slot(boundary, grid.non_gutter_row_count(), grid.has_gutter, leading)?;
    covering_line(
        grid.hlines.get(index)?,
        position,
        start,
        end,
        grid.non_gutter_column_count(),
    )
}

fn vertical_line_override(
    grid: &CellGrid,
    boundary: usize,
    start: usize,
    end: usize,
    leading: bool,
) -> Option<Option<Border>> {
    let (index, position) =
        line_slot(boundary, grid.non_gutter_column_count(), grid.has_gutter, leading)?;
    covering_line(
        grid.vlines.get(index)?,
        position,
        start,
        end,
        grid.non_gutter_row_count(),
    )
}

/// Selects the explicit-line slot adjacent to a content track. Without gutters,
/// `After` lines are normalized by the resolver to `Before` at the next
/// boundary. With gutters the two faces stay distinct.
fn line_slot(
    boundary: usize,
    track_count: usize,
    has_gutter: bool,
    leading: bool,
) -> Option<(usize, LinePosition)> {
    if boundary > track_count {
        return None;
    }
    if !has_gutter || leading || boundary == track_count {
        Some((boundary, LinePosition::Before))
    } else {
        Some((boundary.checked_sub(1)?, LinePosition::After))
    }
}

fn covering_line(
    lines: &[Line],
    position: LinePosition,
    start: usize,
    end: usize,
    track_count: usize,
) -> Option<Option<Border>> {
    lines.iter().rev().find_map(|line| {
        let line_end = line.end.map_or(track_count, |value| value.get());
        (line.position == position && line.start <= start && line_end >= end)
            .then(|| line.stroke.as_deref().map(stroke_to_border))
    })
}

fn apply_line_override(border: &mut Option<Border>, line: Option<Option<Border>>) {
    if let Some(line) = line {
        *border = line;
    }
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
    origin: TableOrigin,
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
    if origin == TableOrigin::LayoutGrid {
        widen_repeated_auto_columns(
            logical_id,
            grid,
            ctx,
            has_column_gutter,
            &mut columns_dxa,
        );
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

/// A reused layout grid can have a wider intrinsic `auto` track in a later
/// physical occurrence (for example, a code gutter that grows from two to
/// three digits). The paged snapshot retains each occurrence, but the flowing
/// DOCX table plan otherwise uses only the first one. Raise auto tracks to the
/// widest measurement seen in the same-width context and take the exact delta
/// from fractional tracks so the table's outer width remains unchanged.
fn widen_repeated_auto_columns(
    logical_id: u128,
    grid: &CellGrid,
    ctx: &DocxCtx,
    has_column_gutter: bool,
    columns_dxa: &mut [i32],
) {
    let tracks: Vec<_> = if grid.has_gutter {
        grid.cols.iter().step_by(2).copied().collect()
    } else {
        grid.cols.clone()
    };
    let ncols = tracks.len();
    if ncols == 0 {
        return;
    }
    let column_index = |x: usize| if has_column_gutter { x * 2 } else { x };
    if column_index(ncols - 1) >= columns_dxa.len() {
        return;
    }

    let base_total: i32 = (0..ncols).map(|x| columns_dxa[column_index(x)]).sum();
    let mut floors: Vec<i32> = (0..ncols).map(|x| columns_dxa[column_index(x)]).collect();
    let mut occurrences = 0usize;
    for table in ctx
        .paged_geometry
        .tables()
        .iter()
        .filter(|table| table.logical_id == logical_id)
    {
        if table.cells.iter().any(|cell| !cell.axis_aligned) {
            continue;
        }
        let Some(widths) = (0..ncols)
            .map(|x| {
                median_positive(
                    table
                        .cells
                        .iter()
                        .filter(|cell| cell.x == x && cell.colspan == 1)
                        .map(|cell| cell.width_pt)
                        .collect(),
                )
                .and_then(pt_to_positive_dxa)
            })
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let occurrence_total: i32 = widths.iter().sum();
        if occurrence_total.abs_diff(base_total) > 2 {
            continue;
        }
        occurrences += 1;
        for (x, sizing) in tracks.iter().enumerate() {
            if matches!(sizing, Sizing::Auto) {
                floors[x] = floors[x].max(widths[x]);
            }
        }
    }
    if occurrences < 2 {
        return;
    }
    for (x, sizing) in tracks.iter().enumerate() {
        if matches!(sizing, Sizing::Auto) {
            floors[x] = floors[x].saturating_add(LAYOUT_GRID_INLINE_TOLERANCE_DXA);
        }
    }

    let delta: i32 = (0..ncols)
        .map(|x| floors[x].saturating_sub(columns_dxa[column_index(x)]))
        .sum();
    if delta == 0 {
        return;
    }
    let donors: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter_map(|(x, sizing)| matches!(sizing, Sizing::Fr(_)).then_some(x))
        .collect();
    let total_capacity: i32 = donors
        .iter()
        .map(|&x| columns_dxa[column_index(x)].saturating_sub(1))
        .sum();
    if total_capacity < delta {
        return;
    }

    for x in 0..ncols {
        columns_dxa[column_index(x)] = floors[x];
    }
    let mut remaining = delta;
    let mut remaining_capacity = total_capacity;
    for (position, &x) in donors.iter().enumerate() {
        let index = column_index(x);
        let capacity = columns_dxa[index].saturating_sub(1);
        let take = if position + 1 == donors.len() {
            remaining
        } else {
            ((remaining as i64 * capacity as i64) / remaining_capacity as i64) as i32
        }
        .min(capacity);
        columns_dxa[index] -= take;
        remaining -= take;
        remaining_capacity -= capacity;
    }
    debug_assert_eq!(remaining, 0);
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

/// Appends an empty paragraph if the last serialized block is not one (Word
/// requires every `w:tc` to end in a `w:p`). Internal `Block::Tag` markers emit
/// no XML, so trailing tags after a real paragraph must not manufacture a
/// second visible line in the cell.
fn ensure_ends_in_para(blocks: &mut Vec<Block>) {
    let ends_in_para = blocks
        .iter()
        .rev()
        .find(|block| !matches!(block, Block::Tag(_)))
        .is_some_and(|block| matches!(block, Block::Para(_)));
    if !ends_in_para {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text_run(text: &str) -> ParaChild {
        ParaChild::Run(Run::Text { props: RunProps::default(), text: text.into() })
    }

    #[test]
    fn measured_cells_trim_only_trailing_structural_breaks() {
        let mut blocks = vec![Block::Para(Para {
            props: ParaProps::default(),
            content: vec![
                text_run("body"),
                ParaChild::Run(Run::Break { kind: BreakKind::Structural }),
                ParaChild::Run(Run::Break { kind: BreakKind::Structural }),
            ],
        })];
        trim_trailing_structural_breaks(&mut blocks);
        let Block::Para(para) = &blocks[0] else { unreachable!() };
        assert_eq!(para.content.len(), 1);

        let mut authored_tail = vec![Block::Para(Para {
            props: ParaProps::default(),
            content: vec![
                text_run("body"),
                ParaChild::Run(Run::Break { kind: BreakKind::Structural }),
                ParaChild::Run(Run::Break { kind: BreakKind::Authored }),
            ],
        })];
        trim_trailing_structural_breaks(&mut authored_tail);
        let Block::Para(para) = &authored_tail[0] else { unreachable!() };
        assert_eq!(para.content.len(), 3);
    }

    #[test]
    fn exact_empty_rows_exclude_every_layout_bearing_child() {
        assert!(para_child_is_layout_empty(&text_run("   ")));

        let hidden = RunProps { vanish: true, ..RunProps::default() };
        assert!(para_child_is_layout_empty(&ParaChild::Run(Run::Text {
            props: hidden,
            text: "searchable fallback".into(),
        })));

        assert!(!para_child_is_layout_empty(&text_run("visible")));
        assert!(!para_child_is_layout_empty(&ParaChild::Run(Run::Break {
            kind: BreakKind::Authored,
        })));
        assert!(!para_child_is_layout_empty(&ParaChild::OmmlPara(
            "<m:oMathPara/>".into(),
        )));
    }
}

//! The `table` mapper: Word `w:tbl` → the Typst IR's [`tdoc::Table`].

use typst_ooxml_core::units::twip_to_abs;

use crate::lower::{lower_items, parse_hex_color, LowerCtx};
use crate::tdoc::{self, Border, BoxStroke, Sides, TableCell, TableRow, VAlign};
use crate::wml::model::{
    BorderEdge, Borders, Cell, CellMargins, Row, Table as WmlTable, TableBorders,
};

/// A source cell resolved against the table's vertical merges.
#[derive(Debug, Clone, Copy)]
struct Merged {
    /// Where the cell starts in *Word's* own grid: cells laid left to right by
    /// `w:gridSpan`, counting `w:vMerge` continuations, since Word physically
    /// writes one into every row a merge covers.
    column: usize,
    /// How many rows the cell spans; 1 unless it starts a merge run.
    rowspan: usize,
    /// A continuation absorbed by the restart above it. Typst models a
    /// vertical merge as a `rowspan` on the *first* cell with the covered
    /// slots simply absent, so these are dropped rather than emitted as the
    /// stray empty cells they used to become.
    absorbed: bool,
}

/// Resolve `w:vMerge` runs into Typst rowspans.
///
/// Word writes a vertical merge as a `restart` cell followed by one `continue`
/// cell per row beneath, each physically present in its row; Typst gives the
/// first cell a `rowspan` and expects the covered slots to be absent. This
/// walks each restart straight down its own column, absorbing the
/// continuations under it.
///
/// A continuation with no restart above it — malformed, but real documents do
/// it — stays unabsorbed and lowers as an ordinary cell, so no content is lost
/// to a merge that was never opened.
fn resolve_vertical_merges(rows: &[Row]) -> Vec<Vec<Merged>> {
    let mut merged: Vec<Vec<Merged>> = rows
        .iter()
        .map(|row| {
            let mut column = 0;
            row.cells
                .iter()
                .map(|cell| {
                    let starts_at = column;
                    column += cell.grid_span.max(1);
                    Merged { column: starts_at, rowspan: 1, absorbed: false }
                })
                .collect()
        })
        .collect();

    for row in 0..rows.len() {
        for index in 0..rows[row].cells.len() {
            if rows[row].cells[index].v_merge != Some(true) {
                continue;
            }
            let column = merged[row][index].column;
            let mut rowspan = 1;
            for below in (row + 1)..rows.len() {
                // The merge only continues while the row beneath has a
                // `continue` cell starting at exactly the same column.
                let Some(under) = merged[below].iter().position(|m| m.column == column) else {
                    break;
                };
                if rows[below].cells[under].v_merge != Some(false)
                    || merged[below][under].absorbed
                {
                    break;
                }
                merged[below][under].absorbed = true;
                rowspan += 1;
            }
            merged[row][index].rowspan = rowspan;
        }
    }

    merged
}

pub(crate) fn lower_table(table: &WmlTable, ctx: &mut LowerCtx) -> tdoc::Table {
    // The column count must accommodate the WIDEST row's total span, not just
    // `w:tblGrid`'s length: in real-world documents the declared grid and the
    // rows' actual `w:gridSpan`s routinely disagree, and a cell whose colspan
    // exceeds the declared columns makes Typst's `#table` reject the document
    // ("colspan would exceed the available columns"). Take the max of the two.
    let widest_row = table
        .rows
        .iter()
        .map(|row| row.cells.iter().map(|c| c.grid_span.max(1)).sum::<usize>())
        .max()
        .unwrap_or(0);
    let columns = table.grid.len().max(widest_row);

    // Column widths follow the grid; pad any columns beyond the grid (added to
    // fit a wider row) as `auto` so the widths vector always matches `columns`.
    let mut column_widths: Vec<Option<f64>> = table
        .grid
        .iter()
        .map(|&w| (w > 0).then(|| twip_to_abs(w as f64).to_pt()))
        .collect();
    column_widths.resize(columns, None);

    let merged = resolve_vertical_merges(&table.rows);

    // Typst's `#table` auto-flows cells into a fixed-width grid with no notion
    // of "rows": a row whose cells span fewer than `columns` leaves the flow
    // cursor mid-row, so a later cell's colspan can overflow the row it lands
    // in ("colspan would exceed the available columns"). Real-world tables are
    // routinely irregular this way. Pad every short row with empty cells so
    // each row fills the grid exactly and the next row starts aligned at
    // column 0. (A single cell can never exceed `columns`, since `columns` is
    // at least the widest row's total span.)
    //
    // A slot covered by a `rowspan` from an earlier row needs *no* padding —
    // Typst fills those itself — so `occupied` tracks how many more rows each
    // column is still spanned for, and those columns are discounted from the
    // row's width before padding.
    let mut occupied = vec![0usize; columns];
    let mut rows = Vec::with_capacity(table.rows.len());
    for (index, row) in table.rows.iter().enumerate() {
        let carried = occupied.iter().filter(|&&remaining| remaining > 0).count();
        for remaining in occupied.iter_mut() {
            *remaining = remaining.saturating_sub(1);
        }

        let mut cells = Vec::with_capacity(row.cells.len());
        for (position, cell) in row.cells.iter().enumerate() {
            let merge = merged[index][position];
            if merge.absorbed {
                continue;
            }
            if merge.rowspan > 1 {
                let colspan = cell.grid_span.max(1);
                let start = merge.column.min(occupied.len());
                let end = (merge.column + colspan).min(occupied.len());
                occupied[start..end].fill(merge.rowspan - 1);
            }
            cells.push(lower_cell(cell, merge.rowspan, ctx));
        }

        if columns > 0 {
            let span: usize = cells.iter().map(|c| c.colspan.max(1)).sum();
            for _ in (span + carried)..columns {
                cells.push(TableCell::empty());
            }
        }

        rows.push(TableRow { header: row.is_header, cells });
    }

    let row_heights = lower_row_heights(&table.rows, ctx);

    // Word's `w:jc` on a *table* places the whole table between the margins;
    // `w:tblInd` pushes it off the left one. Word applies the indent only to a
    // table it is actually laying out from the left edge — a centred or
    // right-aligned table is placed by its alignment and its indent goes
    // unused — so the two are resolved against each other here rather than
    // both being emitted and fighting in the output.
    let align = table.jc.as_deref().and_then(lower_table_jc);
    let indent_pt = match align {
        None | Some(tdoc::Align::Left) => table
            .indent_twips
            .filter(|&twips| twips > 0)
            .map(|twips| twip_to_abs(twips as f64).to_pt()),
        _ => {
            if table.indent_twips.is_some_and(|twips| twips > 0) {
                ctx.report.drop(
                    "table indent",
                    "Word ignores a table's indent once the table is centred or \
                     right-aligned; the alignment is kept instead",
                );
            }
            None
        }
    };

    let stroke = lower_table_stroke(&table.borders, ctx);

    tdoc::Table { columns, column_widths, rows, align, indent_pt, stroke, row_heights }
}

/// Per-row track sizes, or an empty vector when no row states one Typst can
/// honor.
///
/// Only `w:trHeight` with `@w:hRule="exact"` becomes a track size. Word's other
/// (and default) rule, `atLeast`, is a *minimum* that the row grows past when
/// its content needs more room — and Typst has no minimum: a fixed track "will
/// be exactly of this size", so importing an `atLeast` height as one would clip
/// or overflow every row whose content is taller than Word's floor. Sizing to
/// content is the closer approximation, so that's what those rows keep.
fn lower_row_heights(rows: &[Row], ctx: &mut LowerCtx) -> Vec<Option<f64>> {
    let mut heights = Vec::with_capacity(rows.len());
    let mut any_exact = false;
    for row in rows {
        if row.cant_split {
            ctx.report.approximate(
                "table row",
                "w:cantSplit can't be expressed per row; Typst decides where the \
                 table breaks",
            );
        }
        let height = match row.height_twips.filter(|&h| h > 0) {
            Some(h) if row.height_exact => {
                any_exact = true;
                Some(twip_to_abs(h as f64).to_pt())
            }
            Some(_) => {
                ctx.report.approximate(
                    "table row height",
                    "w:trHeight hRule=\"atLeast\" is a minimum height, which Typst \
                     track size for; the row sizes to its content instead",
                );
                None
            }
            None => None,
        };
        heights.push(height);
    }
    if any_exact { heights } else { Vec::new() }
}

/// `w:tblBorders` → the one stroke Typst's `table(stroke:)` applies to every
/// cell edge.
///
/// Word states six sides here (four outer plus the two interior ones) where
/// Typst takes a single stroke, so they have to be reconciled. When they
/// disagree the *interior* stroke wins: in any table bigger than one cell most
/// edges are interior, so it's the one that decides how the table reads.
///
/// `None` — leaving Typst's own 1pt grid — only when Word stated no border at
/// all. That distinction is the point of reading this element: a table whose
/// `w:tblBorders` says `nil` is deliberately borderless, and used to import
/// with a full grid Word never drew.
fn lower_table_stroke(borders: &TableBorders, ctx: &mut LowerCtx) -> Option<Border> {
    let outer = &borders.outer;
    let interior = [&borders.inside_h, &borders.inside_v];
    let stated: Vec<Border> = interior
        .into_iter()
        .chain([&outer.top, &outer.bottom, &outer.left, &outer.right])
        .filter_map(|edge| edge.as_ref().map(lower_border))
        .collect();

    let (first, rest) = stated.split_first()?;
    if rest.iter().any(|border| border != first) {
        ctx.report.approximate(
            "table borders",
            "w:tblBorders states the outer and interior edges separately; Typst's table \
             takes one stroke for every edge, so the interior one is used throughout",
        );
    }
    Some(*first)
}

/// `w:tblPr/w:jc` → the Typst alignment the table is wrapped in. `both`/
/// `distribute` are paragraph justifications with no meaning for a table, and
/// are left unread rather than turned into an alignment Word never asked for.
fn lower_table_jc(jc: &str) -> Option<tdoc::Align> {
    match jc {
        "center" => Some(tdoc::Align::Center),
        "right" | "end" => Some(tdoc::Align::Right),
        "left" | "start" => Some(tdoc::Align::Left),
        _ => None,
    }
}

fn lower_cell(cell: &Cell, rowspan: usize, ctx: &mut LowerCtx) -> TableCell {
    // A page/column break is meaningless inside a table cell (Typst rejects
    // it outright — "pagebreaks are not allowed inside of containers"), so
    // `lower_paragraph` needs to know it's lowering one; see
    // `LowerCtx::enter_container`'s doc comment.
    let was_in_container = ctx.enter_container();
    let body = lower_items(&cell.content, ctx);
    ctx.exit_container(was_in_container);
    TableCell {
        colspan: cell.grid_span.max(1),
        rowspan,
        fill: parse_hex_color(cell.shd_fill.as_deref()),
        stroke: lower_borders(&cell.borders),
        align: cell.v_align.as_deref().and_then(lower_v_align),
        inset: lower_margins(&cell.margins),
        body,
    }
}

fn lower_v_align(val: &str) -> Option<VAlign> {
    match val {
        "top" => Some(VAlign::Top),
        "center" => Some(VAlign::Horizon),
        "bottom" => Some(VAlign::Bottom),
        _ => None,
    }
}

/// Word measures a border's width in eighths of a point, defaulting to Word's
/// own half-point line when `w:sz` is absent.
fn lower_border(edge: &BorderEdge) -> Border {
    if edge.is_none() {
        return Border::None;
    }
    Border::Line {
        thickness_pt: edge.sz_eighth_pt.unwrap_or(4) as f64 / 8.0,
        color: parse_hex_color(edge.color.as_deref()),
    }
}

pub(crate) fn lower_borders(borders: &Borders) -> BoxStroke {
    BoxStroke {
        top: borders.top.as_ref().map(lower_border),
        bottom: borders.bottom.as_ref().map(lower_border),
        left: borders.left.as_ref().map(lower_border),
        right: borders.right.as_ref().map(lower_border),
    }
}

fn lower_margins(margins: &CellMargins) -> Option<Sides> {
    if margins.is_empty() {
        return None;
    }
    let twips = |t: Option<i64>| t.map(|t| twip_to_abs(t as f64).to_pt());
    Some(Sides {
        top: twips(margins.top),
        bottom: twips(margins.bottom),
        left: twips(margins.left),
        right: twips(margins.right),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
    use crate::wml::model::{
        BodyItem, BreakType, Paragraph, Run, RunContent, RunItem, RunProps, WmlPackage,
    };

    /// `[#pagebreak()],` inside a table cell — a real corpus failure
    /// ("pagebreaks are not allowed inside of containers"). The cell's own
    /// break-only paragraph must lower to nothing (reported), not a
    /// `Block::Break`.
    #[test]
    fn a_page_break_inside_a_table_cell_is_dropped_not_emitted() {
        let table = WmlTable {
            grid: vec![1000],
            rows: vec![Row {
                is_header: false,
                cells: vec![Cell {
                    grid_span: 1,
                    content: vec![BodyItem::Paragraph(Paragraph {
                        props: Default::default(),
                        runs: vec![RunItem::Run(Run {
                            props: RunProps::default(),
                            content: vec![RunContent::Break(BreakType::Page)],
                        })],
                    })],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let lowered = lower_table(&table, &mut ctx);

        assert_eq!(lowered.rows.len(), 1);
        let cell_body = &lowered.rows[0].cells[0].body;
        assert!(
            !cell_body.iter().any(|b| matches!(b, tdoc::Block::Break(_))),
            "expected no break block, got {cell_body:?}"
        );
        assert!(ctx.report.notes.iter().any(|n| n.what == "page/column break"));

        // `LowerCtx::in_container` must be restored to `false` once the
        // cell's own body is done lowering — it isn't itself inside another
        // container.
        assert!(!ctx.in_container());
    }

    fn merge_cell(text: &str, v_merge: Option<bool>) -> Cell {
        Cell {
            grid_span: 1,
            v_merge,
            content: vec![BodyItem::Paragraph(Paragraph {
                props: Default::default(),
                runs: vec![RunItem::Run(Run {
                    props: RunProps::default(),
                    content: vec![RunContent::Text(text.into())],
                })],
            })],
            ..Default::default()
        }
    }

    /// Word writes a vertical merge as a `restart` plus one `continue` cell per
    /// row beneath; Typst wants a `rowspan` on the first cell and no cell at
    /// all in the rows it covers. The continuations used to survive as stray
    /// empty cells, which is what made merged tables come back wrong.
    #[test]
    fn a_vertical_merge_becomes_a_rowspan_and_drops_its_continuations() {
        let table = WmlTable {
            grid: vec![1000, 1000],
            rows: vec![
                Row {
                    is_header: false,
                    cells: vec![merge_cell("spans", Some(true)), merge_cell("one", None)],
                    ..Default::default()
                },
                Row {
                    is_header: false,
                    cells: vec![merge_cell("", Some(false)), merge_cell("two", None)],
                    ..Default::default()
                },
                Row {
                    is_header: false,
                    cells: vec![merge_cell("", Some(false)), merge_cell("three", None)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let lowered = lower_table(&table, &mut ctx);

        assert_eq!(lowered.rows[0].cells[0].rowspan, 3);
        assert_eq!(lowered.rows[0].cells.len(), 2);
        // The covered rows keep only their own remaining cell. If the padding
        // didn't discount the spanned column they would each gain a stray
        // empty cell and push the table out of shape.
        assert_eq!(lowered.rows[1].cells.len(), 1);
        assert_eq!(lowered.rows[2].cells.len(), 1);
    }

    /// A `continue` with no `restart` above it is malformed, but real
    /// documents contain it — it must lower as an ordinary cell rather than
    /// being absorbed into a merge that was never opened.
    #[test]
    fn an_orphan_continuation_is_kept_as_an_ordinary_cell() {
        let table = WmlTable {
            grid: vec![1000, 1000],
            rows: vec![Row {
                is_header: false,
                cells: vec![merge_cell("orphan", Some(false)), merge_cell("beside", None)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let lowered = lower_table(&table, &mut ctx);

        assert_eq!(lowered.rows[0].cells.len(), 2);
        assert_eq!(lowered.rows[0].cells[0].rowspan, 1);
    }

    /// A merge that starts partway down the table must not swallow the rows
    /// above it, and a merge column wider than one grid column must reserve
    /// every column it covers.
    #[test]
    fn a_wide_merge_reserves_all_the_columns_it_covers() {
        let mut wide = merge_cell("wide", Some(true));
        wide.grid_span = 2;
        let mut wide_continue = merge_cell("", Some(false));
        wide_continue.grid_span = 2;

        let table = WmlTable {
            grid: vec![1000, 1000, 1000],
            rows: vec![
                Row {
                    is_header: false,
                    cells: vec![wide, merge_cell("side", None)],
                    ..Default::default()
                },
                Row {
                    is_header: false,
                    cells: vec![wide_continue, merge_cell("under", None)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let lowered = lower_table(&table, &mut ctx);

        assert_eq!(lowered.columns, 3);
        assert_eq!(lowered.rows[0].cells[0].colspan, 2);
        assert_eq!(lowered.rows[0].cells[0].rowspan, 2);
        // Two of the three columns are spanned from above, so the second row
        // needs no padding beyond its own single cell.
        assert_eq!(lowered.rows[1].cells.len(), 1);
    }
}

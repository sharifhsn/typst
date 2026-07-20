//! The `table` mapper: Word `w:tbl` → the Typst IR's [`tdoc::Table`].

use typst_ooxml_core::units::twip_to_abs;

use crate::lower::{lower_items, parse_hex_color, LowerCtx};
use crate::tdoc::{self, TableCell, TableRow};
use crate::wml::model::{Cell, Row, Table as WmlTable};

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

    let mut rows: Vec<TableRow> = table.rows.iter().map(|row| lower_row(row, ctx)).collect();

    // Typst's `#table` auto-flows cells into a fixed-width grid with no notion
    // of "rows": a row whose cells span fewer than `columns` leaves the flow
    // cursor mid-row, so a later cell's colspan can overflow the row it lands
    // in ("colspan would exceed the available columns"). Real-world tables are
    // routinely irregular this way. Pad every short row with empty cells so
    // each row fills the grid exactly and the next row starts aligned at
    // column 0. (A single cell can never exceed `columns`, since `columns` is
    // at least the widest row's total span.)
    if columns > 0 {
        for row in &mut rows {
            let span: usize = row.cells.iter().map(|c| c.colspan.max(1)).sum();
            for _ in span..columns {
                row.cells.push(TableCell {
                    colspan: 1,
                    rowspan: 1,
                    fill: None,
                    body: Vec::new(),
                });
            }
        }
    }

    tdoc::Table { columns, column_widths, rows }
}

fn lower_row(row: &Row, ctx: &mut LowerCtx) -> TableRow {
    let cells = row.cells.iter().map(|cell| lower_cell(cell, ctx)).collect();
    TableRow { header: row.is_header, cells }
}

fn lower_cell(cell: &Cell, ctx: &mut LowerCtx) -> TableCell {
    // `vMerge == Some(false)` (a "continue" cell) still needs its slot filled
    // — pragmatic v1: emit it like any other cell (usually empty content).
    let body = lower_items(&cell.content, ctx);
    TableCell {
        colspan: cell.grid_span.max(1),
        rowspan: 1,
        fill: parse_hex_color(cell.shd_fill.as_deref()),
        body,
    }
}

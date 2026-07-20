//! The `table` mapper: Word `w:tbl` → the Typst IR's [`tdoc::Table`].

use typst_ooxml_core::units::twip_to_abs;

use crate::lower::{lower_items, parse_hex_color};
use crate::opts::ImportOptions;
use crate::report::ImportReport;
use crate::tdoc::{self, TableCell, TableRow};
use crate::wml::model::{Cell, Row, Table as WmlTable, WmlPackage};

pub fn lower_table(
    table: &WmlTable,
    package: &WmlPackage,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> tdoc::Table {
    let columns = if !table.grid.is_empty() {
        table.grid.len()
    } else {
        table
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|c| c.grid_span.max(1)).sum::<usize>())
            .max()
            .unwrap_or(0)
    };

    let column_widths = table
        .grid
        .iter()
        .map(|&w| (w > 0).then(|| twip_to_abs(w as f64).to_pt()))
        .collect();

    let rows = table.rows.iter().map(|row| lower_row(row, package, options, report)).collect();

    tdoc::Table { columns, column_widths, rows }
}

fn lower_row(
    row: &Row,
    package: &WmlPackage,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> TableRow {
    let cells = row.cells.iter().map(|cell| lower_cell(cell, package, options, report)).collect();
    TableRow { header: row.is_header, cells }
}

fn lower_cell(
    cell: &Cell,
    package: &WmlPackage,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> TableCell {
    // `vMerge == Some(false)` (a "continue" cell) still needs its slot filled
    // — pragmatic v1: emit it like any other cell (usually empty content).
    let body = lower_items(&cell.content, package, options, report);
    TableCell {
        colspan: cell.grid_span.max(1),
        rowspan: 1,
        fill: parse_hex_color(cell.shd_fill.as_deref()),
        body,
    }
}

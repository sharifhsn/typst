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
    // A page/column break is meaningless inside a table cell (Typst rejects
    // it outright — "pagebreaks are not allowed inside of containers"), so
    // `lower_paragraph` needs to know it's lowering one; see
    // `LowerCtx::enter_container`'s doc comment.
    let was_in_container = ctx.enter_container();
    let body = lower_items(&cell.content, ctx);
    ctx.exit_container(was_in_container);
    TableCell {
        colspan: cell.grid_span.max(1),
        rowspan: 1,
        fill: parse_hex_color(cell.shd_fill.as_deref()),
        body,
    }
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
                    v_merge: None,
                    shd_fill: None,
                    content: vec![BodyItem::Paragraph(Paragraph {
                        props: Default::default(),
                        runs: vec![RunItem::Run(Run {
                            props: RunProps::default(),
                            content: vec![RunContent::Break(BreakType::Page)],
                        })],
                    })],
                }],
            }],
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
}

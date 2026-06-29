//! Table mapper: `TableElem`/`GridElem` (→ [`CellGrid`]) → the Pandoc `Table`
//! 6-tuple `[Attr, Caption, [ColSpec], TableHead, [TableBody], TableFoot]`
//! (FINAL-DESIGN §2.6).
//!
//! Both `#table` and `#grid` resolve to the same [`CellGrid`], so they share one
//! lowering. The grid is row-major (`entries.len() == ncols * nrows`, gutter-
//! doubled when `has_gutter`).
//!
//! LOAD-BEARING CORRECTNESS (research/table.md §7, §9): emit exactly one `Cell`
//! per `Entry::Cell` and SKIP every `Entry::Merged`. Slots covered by a colspan/
//! rowspan are OMITTED from the row's `[Cell]` array — never placeheld (the
//! inverse of OOXML `w:vMerge`). Pandoc does not validate: an over-full row
//! silently loses the colliding cell in its writers.
//!
//! Mapped: colspan/rowspan (native `RowSpan`/`ColSpan`), per-cell horizontal
//! alignment, header rows (`table.header` / repeated leading rows) → `TableHead`,
//! footer rows → `TableFoot`, absolute column widths → `ColWidth` fractions.
//! Lost (accepted): per-cell fill/stroke, row heights, vertical align, gutters,
//! intermediate (mid-table) heads.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, Smart, StyleChain};
use typst_library::layout::resolve::{Cell as ResolvedCell, CellGrid, Entry};
use typst_library::layout::{Alignment as TypstAlign, HAlignment, Sizing};
use typst_library::layout::{GridCell, GridElem};
use typst_library::model::{TableCell, TableElem};

use crate::ast::{
    self, Alignment, Block, Caption, Cell, ColSpec, ColWidth, Row, TableBody, TableFoot,
    TableHead,
};
use crate::ctx::PandocCtx;

pub fn table(
    elem: &Packed<TableElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let Some(grid) = elem.grid.as_ref() else {
        return Ok(Vec::new());
    };
    cellgrid(grid, styles, ctx)
}

pub fn grid(
    elem: &Packed<GridElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let Some(grid) = elem.grid.as_ref() else {
        return Ok(Vec::new());
    };
    cellgrid(grid, styles, ctx)
}

/// Lowers a resolved [`CellGrid`] (shared by `#table` and `#grid`) into a Pandoc
/// `Table` block.
fn cellgrid(
    grid: &CellGrid,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let ncols = grid.non_gutter_column_count();
    if ncols == 0 || grid.entries.is_empty() {
        return Ok(Vec::new());
    }
    let nrows = grid.entries.len() / ncols;

    // -- Column widths (idx 2: [ColSpec]) ----------------------------------
    // Per-column alignment is folded onto each resolved cell already, so the
    // ColSpec alignment stays AlignDefault and the real alignment rides on the
    // cells. Absolute widths become fractions of the total; flexible columns
    // stay ColWidthDefault.
    let col_specs = resolve_col_specs(grid, ncols);

    // -- Header / footer row ranges ----------------------------------------
    let is_header_row = |y: usize| -> bool {
        grid.headers.iter().any(|hd| header_range(grid, &hd.range).contains(&y))
    };
    let is_footer_row = |y: usize| -> bool {
        grid.footer
            .as_ref()
            .map(|ft| footer_range(grid, ft.range()).contains(&y))
            .unwrap_or(false)
    };

    // -- Walk rows ----------------------------------------------------------
    let mut head_rows: Vec<Row> = Vec::new();
    let mut foot_rows: Vec<Row> = Vec::new();
    let mut body_rows: Vec<Row> = Vec::new();

    for y in 0..nrows {
        let row = build_row(grid, ncols, y, styles, ctx)?;
        if is_header_row(y) {
            head_rows.push(row);
        } else if is_footer_row(y) {
            foot_rows.push(row);
        } else {
            body_rows.push(row);
        }
    }

    let head = TableHead(ast::empty_attr(), head_rows);
    let foot = TableFoot(ast::empty_attr(), foot_rows);
    // A single body: no row-header stub, no intermediate-head rows.
    let bodies = vec![TableBody(ast::empty_attr(), 0, Vec::new(), body_rows)];

    let table = Block::Table(
        ast::empty_attr(),
        Caption(None, Vec::new()),
        col_specs,
        head,
        bodies,
        foot,
    );

    Ok(vec![table])
}

/// Builds one `Row` for non-gutter row `y`, emitting a `Cell` per `Entry::Cell`
/// and SKIPPING every `Entry::Merged` (the omission rule — load-bearing).
fn build_row(
    grid: &CellGrid,
    ncols: usize,
    y: usize,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Row> {
    let mut cells = Vec::with_capacity(ncols);
    let mut x = 0;
    while x < ncols {
        match &grid.entries[y * ncols + x] {
            Entry::Cell(cell) => {
                let colspan = cell.colspan.get().max(1);
                let rowspan = cell.rowspan.get().max(1);
                cells.push(build_cell(cell, colspan, rowspan, styles, ctx)?);
                // Advance past the columns this cell's colspan covers; their
                // `Entry::Merged` slots are thereby skipped (omitted).
                x += colspan;
            }
            Entry::Merged { .. } => {
                // Covered by a colspan to the left or a rowspan from above.
                // Pandoc OMITS such slots — emit nothing, just step over it.
                x += 1;
            }
        }
    }
    Ok(Row(ast::empty_attr(), cells))
}

/// Builds one Pandoc `Cell` from a resolved origin cell.
/// `Cell = (Attr, Alignment, RowSpan, ColSpan, [Block])`.
fn build_cell(
    cell: &ResolvedCell,
    colspan: usize,
    rowspan: usize,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Cell> {
    let align = cell_alignment(cell, styles);
    let blocks = cell_blocks(cell, styles, ctx)?;
    Ok(Cell(
        ast::empty_attr(),
        align,
        rowspan as i32,
        colspan as i32,
        blocks,
    ))
}

/// Lowers a resolved cell's inner body into Pandoc blocks. The resolved
/// `cell.body` is a packed `TableCell`/`GridCell`; recurse on its inner `body`.
fn cell_blocks(
    cell: &ResolvedCell,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let content = cell
        .body
        .to_packed::<TableCell>()
        .map(|tc| tc.body.clone())
        .or_else(|| cell.body.to_packed::<GridCell>().map(|gc| gc.body.clone()))
        .unwrap_or_else(|| cell.body.clone());

    crate::convert::blocks(ctx, &content, styles)
}

/// Reads the resolved cell's effective horizontal alignment off its
/// `TableCell`/`GridCell` body. `Smart::Auto` or vertical-only → `AlignDefault`.
fn cell_alignment(cell: &ResolvedCell, styles: StyleChain) -> Alignment {
    let align = cell
        .body
        .to_packed::<TableCell>()
        .map(|tc| tc.align.get(styles))
        .or_else(|| cell.body.to_packed::<GridCell>().map(|gc| gc.align.get(styles)));

    match align {
        Some(Smart::Custom(a)) => halign_to_pandoc(a),
        _ => Alignment::AlignDefault,
    }
}

/// Maps the horizontal component of a Typst [`Alignment`] to a Pandoc
/// [`Alignment`]. A vertical-only alignment (no `x`) → `AlignDefault`.
fn halign_to_pandoc(align: TypstAlign) -> Alignment {
    match align.x() {
        Some(HAlignment::Left) => Alignment::AlignLeft,
        Some(HAlignment::Start) => Alignment::AlignLeft,
        Some(HAlignment::Center) => Alignment::AlignCenter,
        Some(HAlignment::Right) => Alignment::AlignRight,
        Some(HAlignment::End) => Alignment::AlignRight,
        None => Alignment::AlignDefault,
    }
}

/// Builds the per-column `[ColSpec]`. Alignment stays `AlignDefault` (the real
/// alignment rides on each cell); a column with an absolute width gets a
/// `ColWidth` fraction of the grid's total absolute width, everything else stays
/// `ColWidthDefault`.
fn resolve_col_specs(grid: &CellGrid, ncols: usize) -> Vec<ColSpec> {
    let content_cols: Vec<Sizing> = if grid.has_gutter {
        grid.cols.iter().copied().step_by(2).take(ncols).collect()
    } else {
        grid.cols.iter().copied().take(ncols).collect()
    };

    // Absolute width (pt) per column, where it has one; else None (flexible).
    let abs_pt: Vec<Option<f64>> = content_cols
        .iter()
        .map(|sizing| match sizing {
            // Only purely-absolute tracks contribute a known width; `rel`/`fr`/
            // `auto` have no resolved size at this (pre-layout) stage.
            Sizing::Rel(rel) if rel.rel.get() == 0.0 => {
                let pt = rel.abs.abs.to_pt();
                (pt > 0.0).then_some(pt)
            }
            _ => None,
        })
        .collect();

    // Only emit fractions when EVERY column is absolute (so the fractions are
    // meaningful and sum sensibly); otherwise leave all columns as default and
    // let the writer size them.
    let total: f64 = abs_pt.iter().filter_map(|w| *w).sum();
    let all_abs = abs_pt.iter().all(Option::is_some) && total > 0.0;

    (0..ncols)
        .map(|i| {
            let width = if all_abs {
                ColWidth::ColWidth(abs_pt[i].unwrap() / total)
            } else {
                ColWidth::ColWidthDefault
            };
            (Alignment::AlignDefault, width)
        })
        .collect()
}

/// Converts a header range from gutter-doubled row coordinates to non-gutter row
/// indices (mirrors the HTML/DOCX `show_cellgrid` header-range conversion).
fn header_range(
    grid: &CellGrid,
    range: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    if grid.has_gutter {
        range.start / 2..range.end.div_ceil(2)
    } else {
        range.clone()
    }
}

/// Converts a footer range from gutter-doubled row coordinates to non-gutter row
/// indices.
fn footer_range(grid: &CellGrid, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    if grid.has_gutter {
        range.start / 2..range.end.div_ceil(2)
    } else {
        range
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, Inline};

    fn plain_cell(text: &str, rowspan: i32, colspan: i32, align: Alignment) -> Cell {
        Cell(
            ast::empty_attr(),
            align,
            rowspan,
            colspan,
            vec![Block::Plain(vec![Inline::Str(text.into())])],
        )
    }

    /// Hand-construct the 6-tuple shape this mapper emits and assert it
    /// serializes to the pandoc-accepted `Table` form, including the omission
    /// rule: a `rowspan=2` origin in row 0 means row 1 lists ONE fewer cell.
    #[test]
    fn table_six_tuple_with_rowspan_omission() {
        // 2x2 grid: (0,0) rowspans 2; row 1 therefore has only its (1,1) cell.
        let head = TableHead(ast::empty_attr(), Vec::new());
        let body_rows = vec![
            Row(
                ast::empty_attr(),
                vec![
                    plain_cell("spanned", 2, 1, Alignment::AlignDefault),
                    plain_cell("a", 1, 1, Alignment::AlignDefault),
                ],
            ),
            // Row 1: the (1,0) slot is OMITTED (covered by the rowspan above).
            Row(
                ast::empty_attr(),
                vec![plain_cell("b", 1, 1, Alignment::AlignDefault)],
            ),
        ];
        let bodies = vec![TableBody(ast::empty_attr(), 0, Vec::new(), body_rows)];
        let foot = TableFoot(ast::empty_attr(), Vec::new());
        let cols = vec![
            (Alignment::AlignDefault, ColWidth::ColWidthDefault),
            (Alignment::AlignDefault, ColWidth::ColWidthDefault),
        ];

        let table = Block::Table(
            ast::empty_attr(),
            Caption(None, Vec::new()),
            cols,
            head,
            bodies,
            foot,
        );
        let json = serde_json::to_value(&table).unwrap();

        assert_eq!(json["t"], "Table");
        let c = json["c"].as_array().unwrap();
        assert_eq!(c.len(), 6, "Table payload is a 6-tuple");

        // idx 2: two ColSpecs → ncols = 2.
        assert_eq!(c[2].as_array().unwrap().len(), 2);

        // idx 4: the single body's body-rows (4-tuple idx 3).
        let body0 = &c[4][0];
        let rows = body0[3].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Row 0 lists 2 cells, the first with RowSpan 2.
        let r0_cells = rows[0][1].as_array().unwrap();
        assert_eq!(r0_cells.len(), 2);
        assert_eq!(r0_cells[0][2], 2, "RowSpan of the spanning cell is 2");
        // Row 1 lists ONLY 1 cell — the covered slot is omitted, not placeheld.
        let r1_cells = rows[1][1].as_array().unwrap();
        assert_eq!(r1_cells.len(), 1, "covered slot omitted, not placeheld");
    }

    /// Cell ordering: `Cell = (Attr, Alignment, RowSpan, ColSpan, [Block])`.
    #[test]
    fn cell_field_order() {
        let cell = plain_cell("x", 1, 2, Alignment::AlignCenter);
        let json = serde_json::to_value(&cell).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 5);
        assert_eq!(arr[1]["t"], "AlignCenter");
        assert_eq!(arr[2], 1); // RowSpan
        assert_eq!(arr[3], 2); // ColSpan
        assert_eq!(arr[4][0]["t"], "Plain");
    }

    #[test]
    fn halign_mapping() {
        use typst_library::layout::Alignment as A;
        assert!(matches!(halign_to_pandoc(A::LEFT), Alignment::AlignLeft));
        assert!(matches!(halign_to_pandoc(A::CENTER), Alignment::AlignCenter));
        assert!(matches!(halign_to_pandoc(A::RIGHT), Alignment::AlignRight));
        // A vertical-only alignment has no horizontal component → default.
        assert!(matches!(halign_to_pandoc(A::TOP), Alignment::AlignDefault));
    }
}

//! `a:tbl` → `#table`.

use ecow::EcoString;

use crate::lower::{emu, lower_fill, LowerCtx};
use crate::mappers::text;
use crate::pml::model::*;
use crate::resolve::inherit::Inherited;
use crate::tdoc;

pub fn lower(table: &Table, ctx: &mut LowerCtx<'_, '_>) -> tdoc::Block {
    let columns: Vec<f64> = table.grid.iter().map(|w| emu(*w)).collect();

    // A table cell's text inherits from nothing: PowerPoint resolves it from
    // the table *style*, which lives in a part this importer does not read.
    // Passing the empty inheritance keeps the cell's own properties intact
    // rather than inventing a floor for them.
    let inherited = Inherited::default();

    let mut rows = Vec::new();
    for row in &table.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            // A merged cell is covered by its neighbour's span and holds no
            // content of its own; emitting it would push the row one column
            // wide and Typst would reject the table.
            if cell.merged {
                continue;
            }
            let stroke = [0, 1, 2, 3].map(|i| {
                cell.borders[i].as_ref().and_then(|l| lower_cell_border(l, ctx))
            });
            cells.push(tdoc::Cell {
                paras: text::lower_paragraphs(&cell.paras, &inherited, ctx),
                colspan: cell.grid_span.max(1),
                rowspan: cell.row_span.max(1),
                stroke,
                fill: cell.fill.as_ref().and_then(|f| lower_fill(f, ctx)),
                align_y: cell.anchor.as_deref().and_then(|a| {
                    Some(match a {
                        "ctr" => "horizon".into(),
                        "b" => "bottom".into(),
                        "t" => return None,
                        _ => return None,
                    })
                }),
            });
        }
        rows.push(tdoc::Row {
            // PowerPoint's row height is a minimum that grows with content,
            // and a Typst track is exactly its stated size — so honouring it
            // would clip any row whose text outgrew the authored floor.
            height: None,
            cells,
        });
    }

    // A table whose borders live in `ppt/tableStyles.xml` is reported rather
    // than guessed at. Half the tables in the wild rely on that part, and
    // inventing a grid for them would repeat the Word importer's worst bug in
    // the opposite direction.
    if table.style_id.is_some()
        && table.rows.iter().flat_map(|r| &r.cells).all(|c| c.borders.iter().all(Option::is_none))
    {
        ctx.report.approximate(
            "table style",
            "this table takes its borders and banding from `ppt/tableStyles.xml`, \
             which this importer does not resolve; the table is drawn without \
             them rather than with a guessed grid",
        );
    }

    tdoc::Block::Table(tdoc::Table {
        columns,
        rows,
        header_rows: usize::from(table.first_row_header),
        auto_columns: 0,
    })
}

/// One `a:lnL`/`a:lnT`/`a:lnR`/`a:lnB` → a Typst stroke.
fn lower_cell_border(line: &Line, ctx: &mut LowerCtx<'_, '_>) -> Option<EcoString> {
    // An explicit `a:noFill` on an edge means "no line here", which is not the
    // same as stating nothing: it must override whatever the style would draw.
    if matches!(line.fill, Some(Fill::None)) {
        return Some("none".into());
    }
    let paint = line.fill.as_ref().and_then(|f| lower_fill(f, ctx));
    let width = line.width.map(emu);
    if paint.is_none() && width.is_none() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(p) = &paint {
        parts.push(format!("paint: {}", crate::emit::paint(p)));
    }
    if let Some(w) = width {
        parts.push(format!("thickness: {}", crate::emit::len(w)));
    }
    Some(format!("({})", parts.join(", ")).into())
}

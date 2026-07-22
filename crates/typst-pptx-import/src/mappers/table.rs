//! `a:tbl` → `#table`.

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
            cells.push(tdoc::Cell {
                paras: text::lower_paragraphs(&cell.paras, &inherited, ctx),
                colspan: cell.grid_span.max(1),
                rowspan: cell.row_span.max(1),
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

    tdoc::Block::Table(tdoc::Table {
        columns,
        rows,
        header_rows: usize::from(table.first_row_header),
    })
}

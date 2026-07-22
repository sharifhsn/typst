//! `c:chart` → the data table behind it.
//!
//! Typst has no chart element, and PowerPoint's chart is a live object over an
//! embedded workbook. What it *does* keep, in the chart part itself, is a
//! **cache** of every value it last drew — `c:strCache` for the category and
//! series names, `c:numCache` for the numbers. That cache is the data, and a
//! table of it is a far better answer than a report saying the chart is gone.
//!
//! The same call [`typst_docx_import`](../../typst-docx-import) makes for
//! Word charts, for the same reason: the reader gets their numbers back and
//! can plot them with whatever package they like.

use ecow::EcoString;

use crate::lower::LowerCtx;
use crate::pml::parse::{attr, child, is_el, local};
use crate::tdoc;

/// One series: its name, and its value per category.
struct Series {
    name: Option<EcoString>,
    values: Vec<Option<EcoString>>,
}

/// Read a chart part and build the table of its cached data.
pub fn lower(part: &str, ctx: &mut LowerCtx<'_, '_>) -> Option<tdoc::Block> {
    let bytes = ctx.parser.bytes(part)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let doc = roxmltree::Document::parse(&text).ok()?;

    let mut categories: Vec<EcoString> = Vec::new();
    let mut series: Vec<Series> = Vec::new();

    for ser in doc.descendants().filter(|n| is_el(*n, "ser")) {
        // The series name lives in `c:tx`, itself a cached string reference.
        let name = child(ser, "tx").and_then(|tx| cache_values(tx).into_iter().next().flatten());

        // Categories are shared across series; the first series that states
        // them wins, and later ones only matter if it stated none.
        if categories.is_empty()
            && let Some(cat) = child(ser, "cat")
        {
            categories = cache_values(cat).into_iter().flatten().collect();
        }
        let values = child(ser, "val").map(cache_values).unwrap_or_default();
        if name.is_some() || !values.is_empty() {
            series.push(Series { name, values });
        }
    }

    if series.is_empty() && categories.is_empty() {
        ctx.report.drop(
            "chart",
            "the chart part holds no cached categories or values, so there is \
             no data to recover — PowerPoint keeps the live data in an embedded \
             workbook this importer does not open",
        );
        return None;
    }

    ctx.report.approximate(
        "chart",
        "Typst has no chart element, so the chart's cached data is recovered as \
         a table; the plot itself, its axes and its styling are not drawn",
    );

    // Header row: a blank corner cell, then one column per series.
    let mut rows = Vec::new();
    let mut header = vec![cell(EcoString::new())];
    for s in &series {
        header.push(cell(s.name.clone().unwrap_or_default()));
    }
    let columns = header.len();
    rows.push(tdoc::Row { height: None, cells: header });

    let count = categories.len().max(series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    for index in 0..count {
        let mut cells = vec![cell(
            categories.get(index).cloned().unwrap_or_else(|| EcoString::from("")),
        )];
        for s in &series {
            cells.push(cell(
                s.values.get(index).cloned().flatten().unwrap_or_default(),
            ));
        }
        rows.push(tdoc::Row { height: None, cells });
    }

    Some(tdoc::Block::Table(tdoc::Table {
        // `auto` everywhere: the chart's box says how wide the *plot* was, not
        // how wide its numbers want to be.
        columns: Vec::new(),
        rows,
        header_rows: 1,
        auto_columns: columns,
    }))
}

fn cell(text: EcoString) -> tdoc::Cell {
    tdoc::Cell {
        paras: vec![tdoc::Para {
            inlines: if text.is_empty() {
                Vec::new()
            } else {
                vec![tdoc::Inline::Text(text)]
            },
            ..tdoc::Para::default()
        }],
        colspan: 1,
        rowspan: 1,
        fill: None,
        align_y: None,
        stroke: [None, None, None, None],
    }
}

/// Pull the `c:pt` values out of whichever cache a reference holds.
///
/// Indexed by `@idx` rather than by document order: a cache may omit an empty
/// point entirely, and reading positionally would then shift every later value
/// up a row against its category.
fn cache_values(node: roxmltree::Node) -> Vec<Option<EcoString>> {
    // The first cache that yields any point wins; a `c:tx`/`c:cat`/`c:val` can
    // hold both a `*Ref` (with its cache) and a `*Lit`, and reading past the
    // one that has data would double-count.
    for cache in node
        .descendants()
        .filter(|n| matches!(local(*n), "strCache" | "numCache" | "strLit" | "numLit"))
    {
        let out = typst_ooxml_core::chart::indexed_points(
            cache.children().filter(|n| is_el(*n, "pt")),
            |pt| attr(pt, "idx").and_then(|v| v.parse::<usize>().ok()),
            |pt| {
                let value = child(pt, "v").and_then(|v| v.text()).unwrap_or("");
                EcoString::from(value.trim())
            },
        );
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

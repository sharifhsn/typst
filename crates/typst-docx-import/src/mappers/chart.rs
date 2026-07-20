//! The `chart` mapper: a Word chart (parsed by [`crate::wml::parse`] into
//! [`crate::wml::model::WmlPackage::charts`]) → the Typst IR's [`Chart`] —
//! its underlying data table, captioned with the chart's title.
//!
//! Typst has no chart-drawing primitive and inventing one is out of scope,
//! but a chart part carries its full cached dataset (the exact numbers and
//! labels Word last plotted), so lowering it to a table keeps that
//! information instead of dropping it — the same "can't draw it, but can
//! still say something true" trade [`crate::mappers::drawing`] can't make
//! for an undecodable image format.

use ecow::EcoString;

use crate::report::ImportReport;
use crate::tdoc::{Block, Chart, Inline, ParStyle, Table, TableCell, TableRow};
use crate::wml::model::{ChartData, WmlPackage};

/// Resolve a chart's relationship + chart part into a [`Chart`]. Returns
/// `None` if the relationship can't be found, it doesn't resolve to a parsed
/// chart part, or the chart carries no data at all — the first two record a
/// [`ImportReport::drop`] (something was referenced but couldn't be
/// followed); the last does not, since a chart with no title, no series, and
/// no categories has nothing to lose by being skipped.
pub fn lower_chart(rel_id: &str, package: &WmlPackage, report: &mut ImportReport) -> Option<Chart> {
    let Some(rel) = package.rels.get(rel_id) else {
        report.drop("chart", "chart relationship not found");
        return None;
    };

    // Relationship targets for chart parts are relative to `word/` (e.g.
    // `charts/chart1.xml`), exactly like a drawing's media target — see
    // `mappers::drawing::lower_drawing`.
    let chart_key: EcoString = if rel.target.starts_with("word/") {
        rel.target.clone()
    } else {
        format!("word/{}", rel.target.trim_start_matches("./")).into()
    };

    let Some(data) = package.charts.get(&chart_key) else {
        report.drop("chart", "chart part not found or not recognized");
        return None;
    };

    if data.title.is_none() && data.series.is_empty() && data.categories.is_empty() {
        return None;
    }

    report.approximate(
        "chart",
        "imported as its underlying data table; the plot itself is not reproduced",
    );

    Some(Chart { title: data.title.clone(), table: chart_table(data) })
}

/// Build the data table: a header row of `["", series₁, series₂, …]` (the
/// leading corner cell blank) when the chart declares categories, then one
/// row per category — its label, followed by each series' value at that
/// index. A chart with no categories degrades to one row per series (its
/// name, then its own values, with nothing to align them against).
fn chart_table(data: &ChartData) -> Table {
    let mut rows: Vec<TableRow> = Vec::new();

    if data.categories.is_empty() {
        for series in &data.series {
            let mut cells = vec![text_cell(series.name.as_deref().unwrap_or(""))];
            cells.extend(series.values.iter().map(|v| text_cell(v)));
            rows.push(TableRow { header: false, cells });
        }
    } else {
        rows.push(TableRow {
            header: true,
            cells: std::iter::once(text_cell(""))
                .chain(data.series.iter().map(|s| text_cell(s.name.as_deref().unwrap_or(""))))
                .collect(),
        });
        for (i, category) in data.categories.iter().enumerate() {
            let mut cells = vec![text_cell(category)];
            for series in &data.series {
                let value = series.values.get(i).map(EcoString::as_str).unwrap_or("");
                cells.push(text_cell(value));
            }
            rows.push(TableRow { header: false, cells });
        }
    }

    // Rows built above are each their own natural width (a no-categories
    // series row is as wide as that series' own value count, which can
    // differ from its neighbours' — there's no shared category axis to pad
    // them against ahead of time). Pad every row up to the widest one so
    // Typst's `#table` — a fixed-width grid with no notion of "rows" — never
    // has a short row desync the flow into the next, the same hazard
    // `mappers::table::lower_table` guards against for an irregular `w:tbl`.
    let columns = rows.iter().map(|r| r.cells.len()).max().unwrap_or(1).max(1);
    for row in &mut rows {
        while row.cells.len() < columns {
            row.cells.push(TableCell { colspan: 1, rowspan: 1, fill: None, body: Vec::new() });
        }
    }

    Table { columns, column_widths: Vec::new(), rows }
}

fn text_cell(text: &str) -> TableCell {
    let body = if text.is_empty() {
        Vec::new()
    } else {
        vec![Block::Paragraph { style: ParStyle::default(), body: vec![Inline::Text(text.into())] }]
    };
    TableCell { colspan: 1, rowspan: 1, fill: None, body }
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::wml::model::{ChartSeries, Relationship};

    fn package_with(chart_name: &str, data: ChartData) -> WmlPackage {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: format!("charts/{chart_name}").into(), external: false },
        );
        let mut charts = FxHashMap::default();
        charts.insert(format!("word/charts/{chart_name}").into(), data);
        WmlPackage { rels, charts, ..Default::default() }
    }

    fn cell_text(cell: &TableCell) -> String {
        match cell.body.as_slice() {
            [] => String::new(),
            [Block::Paragraph { body, .. }] => match body.as_slice() {
                [Inline::Text(s)] => s.to_string(),
                _ => panic!("expected a single text inline, got {body:?}"),
            },
            other => panic!("expected a single paragraph, got {other:?}"),
        }
    }

    fn row_texts(row: &TableRow) -> Vec<String> {
        row.cells.iter().map(cell_text).collect()
    }

    #[test]
    fn chart_with_categories_lowers_to_a_captioned_table() {
        let data = ChartData {
            title: Some("Sales".into()),
            categories: vec!["Category 1".into(), "Category 2".into()],
            series: vec![ChartSeries {
                name: Some("Series 1".into()),
                values: vec!["4.3".into(), "2.5".into()],
            }],
        };
        let package = package_with("chart1.xml", data);
        let mut report = ImportReport::default();
        let chart = lower_chart("rId1", &package, &mut report).expect("expected a chart");

        assert_eq!(chart.title.as_deref(), Some("Sales"));
        assert_eq!(chart.table.columns, 2);
        assert_eq!(chart.table.rows.len(), 3); // header + 2 categories
        assert!(chart.table.rows[0].header);
        assert_eq!(row_texts(&chart.table.rows[0]), vec!["", "Series 1"]);
        assert_eq!(row_texts(&chart.table.rows[1]), vec!["Category 1", "4.3"]);
        assert_eq!(row_texts(&chart.table.rows[2]), vec!["Category 2", "2.5"]);

        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn chart_with_no_categories_degrades_to_one_row_per_series() {
        let data = ChartData {
            title: None,
            categories: vec![],
            series: vec![
                ChartSeries { name: Some("A".into()), values: vec!["1".into(), "2".into()] },
                ChartSeries { name: Some("B".into()), values: vec!["3".into()] },
            ],
        };
        let package = package_with("chart1.xml", data);
        let mut report = ImportReport::default();
        let chart = lower_chart("rId1", &package, &mut report).expect("expected a chart");

        assert!(chart.title.is_none());
        assert!(!chart.table.rows[0].header);
        assert_eq!(chart.table.columns, 3); // widest row: name + 2 values
        assert_eq!(row_texts(&chart.table.rows[0]), vec!["A", "1", "2"]);
        // Series B's row is padded out to the same width.
        assert_eq!(row_texts(&chart.table.rows[1]), vec!["B", "3", ""]);
    }

    #[test]
    fn dangling_chart_reference_degrades_gracefully() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        assert!(lower_chart("rId1", &package, &mut report).is_none());
        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn resolved_but_unrecognized_chart_part_degrades_gracefully() {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: "charts/chart1.xml".into(), external: false },
        );
        let package = WmlPackage { rels, ..Default::default() };
        let mut report = ImportReport::default();
        assert!(lower_chart("rId1", &package, &mut report).is_none());
        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn empty_chart_data_produces_nothing_and_no_report_note() {
        let package = package_with("chart1.xml", ChartData::default());
        let mut report = ImportReport::default();
        assert!(lower_chart("rId1", &package, &mut report).is_none());
        assert!(
            report.notes.is_empty(),
            "an empty chart has nothing to lose by being skipped: {:?}",
            report.notes
        );
    }
}

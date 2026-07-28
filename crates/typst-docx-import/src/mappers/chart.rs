//! The `chart` mapper: a Word chart (parsed by [`crate::wml::parse`] into
//! [`crate::wml::model::WmlPackage::charts`]) → the Typst IR's [`Chart`],
//! either as its underlying data table (the default) or as a drawn plot
//! (under [`crate::opts::ChartStyle::Plot`]) — see [`ChartContent`].
//!
//! Typst has no chart-drawing primitive of its own, and inventing one is out
//! of scope, but a chart part carries its full cached dataset (the exact
//! numbers and labels Word last plotted). By default that dataset becomes a
//! table — the same "can't draw it, but can still say something true" trade
//! [`crate::mappers::drawing`] can't make for an undecodable image format.
//! Opting into [`crate::opts::ChartStyle::Plot`] trades that self-contained
//! guarantee for a real plot, drawn with the `lilaq` package
//! ([`crate::emit`] emits the `#import` for it) — but only for the chart
//! kinds `lilaq` has a mark for ([`crate::wml::model::ChartKind`]) and only
//! when every series' cached values are complete and numeric
//! (`build_plot`'s doc comment). Anything else falls back to the table,
//! same as if `Plot` had never been requested — a chart is never dropped
//! outright just because it can't be drawn.

use typst_ooxml_core::units::emu_to_abs;

use ecow::EcoString;

use crate::lower::LowerCtx;
use crate::opts::ChartStyle;
use crate::tdoc::{
    Block, Chart, ChartContent, Inline, ParStyle, Plot, PlotKind, PlotSeries, Table,
    TableCell, TableRow,
};
use crate::wml::model::{ChartData, ChartKind, DrawingRef};

/// Resolve a chart's relationship + chart part into a [`Chart`]. Returns
/// `None` if the relationship can't be found, it doesn't resolve to a parsed
/// chart part, or the chart carries no data at all — the first two record a
/// [`crate::report::ImportReport::drop`] (something was referenced but
/// couldn't be followed); the last does not, since a chart with no title, no
/// series, and no categories has nothing to lose by being skipped.
///
/// The `Table`-vs-`Plot` decision itself happens after that: see
/// [`build_plot`] for exactly what makes a chart plottable.
pub(crate) fn lower_chart(d: &DrawingRef, ctx: &mut LowerCtx) -> Option<Chart> {
    let rel_id = d.rel_id.as_str();
    let package = ctx.package;
    let Some(rel) = package.rels.get(rel_id) else {
        ctx.report.drop("chart", "chart relationship not found");
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
        ctx.report.drop("chart", "chart part not found or not recognized");
        return None;
    };

    if data.title.is_none() && data.series.is_empty() && data.categories.is_empty() {
        return None;
    }

    let content = match ctx.options.charts {
        ChartStyle::Table => {
            ctx.report.approximate(
                "chart",
                "imported as its underlying data table; the plot itself is not reproduced",
            );
            ChartContent::Table(chart_table(data))
        }
        ChartStyle::Plot => match build_plot(data, d) {
            Ok(plot) => {
                // The plot itself isn't lost here, but drawing an area
                // chart's fill as a bare outline still is — worth its own
                // note even though the chart as a whole succeeded.
                if data.kind == ChartKind::Area {
                    ctx.report.approximate(
                        "chart",
                        "an area chart is drawn as its outline (a line); the filled region \
                         beneath it is not reproduced",
                    );
                }
                ChartContent::Plot(plot)
            }
            Err(reason) => {
                ctx.report.approximate("chart", reason);
                ChartContent::Table(chart_table(data))
            }
        },
    };

    Some(Chart { title: data.title.clone(), content })
}

/// Try to turn a chart's cached data into something `lilaq` can draw.
/// `Err` names the reason it can't (fed straight to
/// [`crate::report::ImportReport::approximate`] by the caller, so the wording
/// here *is* the user-facing note), and the caller falls back to the table in
/// every such case — a chart is drawable, or it degrades exactly as if
/// `Table` mode had been requested; there's no third, worse outcome.
///
/// Three separate reasons to say no:
/// - [`ChartKind::Other`] has no `lilaq` mark at all (pie, radar, stock,
///   surface, ChartEx's box-and-whisker/sunburst/…, — anything that isn't
///   `Bar`/`Line`/`Scatter`/`Area`).
/// - A cached value that doesn't parse as `f64`. Word writes these as decimal
///   strings, but a category axis mislabeled as a value series, or genuinely
///   non-numeric cached text, means there's nothing to plot.
/// - A *missing* value: Word's sparse `idx` handling (see
///   (`collect_indexed_pts`) fills a hidden/filtered
///   point with an empty string rather than omitting it, so a series can be
///   "complete" in length but have a hole in the middle. [`PlotSeries`] has
///   no per-point x coordinate of its own — a plotted point's x is simply its
///   position in `values` — so silently skipping the hole would shift every
///   later point one slot earlier, plotting it under the *next* category's
///   tick instead of its own. Coercing the hole to `0.0` would invent a data
///   point that was never there. Both are worse than not plotting at all, so
///   any hole anywhere drops the whole chart to the table instead.
fn build_plot(data: &ChartData, d: &DrawingRef) -> Result<Plot, &'static str> {
    let kind = match data.kind {
        ChartKind::Bar => PlotKind::Bar,
        ChartKind::Line => PlotKind::Line,
        ChartKind::Scatter => PlotKind::Scatter,
        // The outline of an area chart's fill is the same series a line
        // chart would plot; see this chart's own `ChartKind::Area` report
        // note in `lower_chart`, emitted alongside the plot this produces.
        ChartKind::Area => PlotKind::Line,
        ChartKind::Other => {
            return Err(
                "chart type has no plotting counterpart; imported as its underlying data \
                 table instead",
            );
        }
    };

    let mut series = Vec::with_capacity(data.series.len());
    let mut any_value = false;
    for s in &data.series {
        let mut values = Vec::with_capacity(s.values.len());
        for v in &s.values {
            if v.is_empty() {
                return Err(
                    "chart has missing data points that can't be plotted without misaligning \
                     the rest of the series; imported as its underlying data table instead",
                );
            }
            let Ok(n) = v.parse::<f64>() else {
                return Err(
                    "chart values are not numeric; imported as its underlying data table instead",
                );
            };
            values.push(n);
        }
        any_value |= !values.is_empty();
        series.push(PlotSeries { name: s.name.clone(), values });
    }

    if !any_value {
        return Err(
            "chart has no plottable series data; imported as its underlying data table instead",
        );
    }

    Ok(Plot {
        kind,
        legend: data.legend,
        width_pt: d.cx_emu.map(|cx| emu_to_abs(cx as f64).to_pt()),
        height_pt: d.cy_emu.map(|cy| emu_to_abs(cy as f64).to_pt()),
        categories: data.categories.clone(),
        series,
    })
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
                .chain(
                    data.series
                        .iter()
                        .map(|s| text_cell(s.name.as_deref().unwrap_or(""))),
                )
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
            row.cells.push(TableCell::empty());
        }
    }

    // No placement, stroke, or row sizing of its own: a chart's data table is
    // built here rather than read off a `w:tbl`, so there is nothing authored
    // to carry — it's embedded in a `figure(..)`, which is what positions it.
    Table {
        columns,
        column_widths: Vec::new(),
        rows,
        align: None,
        indent_pt: None,
        stroke: None,
        row_heights: Vec::new(),
    }
}

fn text_cell(text: &str) -> TableCell {
    let body = if text.is_empty() {
        Vec::new()
    } else {
        vec![Block::Paragraph {
            style: ParStyle::default(),
            body: vec![Inline::Text(text.into())],
        }]
    };
    TableCell { body, ..TableCell::empty() }
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
    use crate::wml::model::{ChartSeries, Relationship, WmlPackage};

    /// A chart reference with no extent — the plot then falls back to
    /// `lilaq`'s own default size, which is what these tests exercise.
    fn chart_ref(rel_id: &str) -> DrawingRef {
        DrawingRef { rel_id: rel_id.into(), ..Default::default() }
    }

    fn package_with(chart_name: &str, data: ChartData) -> WmlPackage {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship {
                target: format!("charts/{chart_name}").into(),
                external: false,
            },
        );
        let mut charts = FxHashMap::default();
        charts.insert(format!("word/charts/{chart_name}").into(), data);
        WmlPackage { rels, charts, ..Default::default() }
    }

    /// Run [`lower_chart`] against `package` under `options`, handing back
    /// both the result and the report — a `LowerCtx` only, since that's the
    /// entire state a test needs beyond the two arguments already passed in.
    fn lower(
        package: &WmlPackage,
        options: &ImportOptions,
        rel_id: &str,
    ) -> (Option<Chart>, ImportReport) {
        let mut report = ImportReport::default();
        let chart = {
            let mut ctx = LowerCtx::new(package, options, &mut report);
            lower_chart(&chart_ref(rel_id), &mut ctx)
        };
        (chart, report)
    }

    fn table_of(chart: &Chart) -> &Table {
        match &chart.content {
            ChartContent::Table(t) => t,
            ChartContent::Plot(_) => panic!("expected a table, got a plot"),
        }
    }

    fn plot_of(chart: &Chart) -> &Plot {
        match &chart.content {
            ChartContent::Plot(p) => p,
            ChartContent::Table(_) => panic!("expected a plot, got a table"),
        }
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

    /// Whether `report` carries a `"chart"` note whose detail names `needle`
    /// — the one check every fallback-reason test below makes.
    fn has_chart_note(report: &ImportReport, needle: &str) -> bool {
        report
            .notes
            .iter()
            .any(|n| n.what == "chart" && n.detail.contains(needle))
    }

    fn bar_chart_data(
        title: Option<&str>,
        categories: &[&str],
        series: Vec<ChartSeries>,
    ) -> ChartData {
        ChartData {
            legend: None,
            title: title.map(Into::into),
            categories: categories.iter().map(|c| (*c).into()).collect(),
            series,
            kind: ChartKind::Bar,
        }
    }

    #[test]
    fn chart_with_categories_lowers_to_a_captioned_table() {
        let data = ChartData {
            legend: None,
            title: Some("Sales".into()),
            categories: vec!["Category 1".into(), "Category 2".into()],
            series: vec![ChartSeries {
                name: Some("Series 1".into()),
                values: vec!["4.3".into(), "2.5".into()],
            }],
            kind: ChartKind::Bar,
        };
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &ImportOptions::default(), "rId1");
        let chart = chart.expect("expected a chart");
        let table = table_of(&chart);

        assert_eq!(chart.title.as_deref(), Some("Sales"));
        assert_eq!(table.columns, 2);
        assert_eq!(table.rows.len(), 3); // header + 2 categories
        assert!(table.rows[0].header);
        assert_eq!(row_texts(&table.rows[0]), vec!["", "Series 1"]);
        assert_eq!(row_texts(&table.rows[1]), vec!["Category 1", "4.3"]);
        assert_eq!(row_texts(&table.rows[2]), vec!["Category 2", "2.5"]);

        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn chart_with_no_categories_degrades_to_one_row_per_series() {
        let data = ChartData {
            legend: None,
            title: None,
            categories: vec![],
            series: vec![
                ChartSeries {
                    name: Some("A".into()),
                    values: vec!["1".into(), "2".into()],
                },
                ChartSeries { name: Some("B".into()), values: vec!["3".into()] },
            ],
            kind: ChartKind::Bar,
        };
        let package = package_with("chart1.xml", data);
        let (chart, _report) = lower(&package, &ImportOptions::default(), "rId1");
        let chart = chart.expect("expected a chart");
        let table = table_of(&chart);

        assert!(chart.title.is_none());
        assert!(!table.rows[0].header);
        assert_eq!(table.columns, 3); // widest row: name + 2 values
        assert_eq!(row_texts(&table.rows[0]), vec!["A", "1", "2"]);
        // Series B's row is padded out to the same width.
        assert_eq!(row_texts(&table.rows[1]), vec!["B", "3", ""]);
    }

    #[test]
    fn dangling_chart_reference_degrades_gracefully() {
        let package = WmlPackage::default();
        let (chart, report) = lower(&package, &ImportOptions::default(), "rId1");
        assert!(chart.is_none());
        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn resolved_but_unrecognized_chart_part_degrades_gracefully() {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship {
                target: "charts/chart1.xml".into(),
                external: false,
            },
        );
        let package = WmlPackage { rels, ..Default::default() };
        let (chart, report) = lower(&package, &ImportOptions::default(), "rId1");
        assert!(chart.is_none());
        assert!(report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn empty_chart_data_produces_nothing_and_no_report_note() {
        let package = package_with("chart1.xml", ChartData::default());
        let (chart, report) = lower(&package, &ImportOptions::default(), "rId1");
        assert!(chart.is_none());
        assert!(
            report.notes.is_empty(),
            "an empty chart has nothing to lose by being skipped: {:?}",
            report.notes
        );
    }

    // --- `ChartStyle::Plot` -------------------------------------------------

    fn plot_options() -> ImportOptions {
        ImportOptions { charts: ChartStyle::Plot, ..Default::default() }
    }

    #[test]
    fn bar_chart_lowers_to_a_plot_under_chart_style_plot() {
        let data = bar_chart_data(
            Some("Sales"),
            &["Category 1", "Category 2"],
            vec![ChartSeries {
                name: Some("Series 1".into()),
                values: vec!["4.3".into(), "2.5".into()],
            }],
        );
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        let chart = chart.expect("expected a chart");
        let plot = plot_of(&chart);

        assert_eq!(plot.kind, PlotKind::Bar);
        assert_eq!(plot.categories, vec!["Category 1", "Category 2"]);
        assert_eq!(plot.series.len(), 1);
        assert_eq!(plot.series[0].name.as_deref(), Some("Series 1"));
        assert_eq!(plot.series[0].values, vec![4.3, 2.5]);
        // A successfully drawn plot has nothing analogous to report — unlike
        // table mode, the plot itself *is* reproduced here.
        assert!(!report.notes.iter().any(|n| n.what == "chart"));
    }

    #[test]
    fn line_chart_lowers_to_a_plot() {
        let data = ChartData {
            legend: None,
            title: None,
            categories: vec![],
            series: vec![ChartSeries {
                name: Some("Temp".into()),
                values: vec!["1".into(), "2".into(), "3".into()],
            }],
            kind: ChartKind::Line,
        };
        let package = package_with("chart1.xml", data);
        let (chart, _) = lower(&package, &plot_options(), "rId1");
        let chart = chart.unwrap();
        let plot = plot_of(&chart);
        assert_eq!(plot.kind, PlotKind::Line);
        assert_eq!(plot.series[0].values, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn scatter_chart_lowers_to_a_plot() {
        let data = ChartData {
            kind: ChartKind::Scatter,
            series: vec![ChartSeries {
                name: None,
                values: vec!["1".into(), "-2.5".into()],
            }],
            ..Default::default()
        };
        let package = package_with("chart1.xml", data);
        let (chart, _) = lower(&package, &plot_options(), "rId1");
        let chart = chart.unwrap();
        let plot = plot_of(&chart);
        assert_eq!(plot.kind, PlotKind::Scatter);
        assert_eq!(plot.series[0].name, None);
    }

    /// An area chart is drawable — its outline is a line — but the fill it
    /// would normally have is lost, which is worth its own note even though
    /// the chart, unlike the fallback cases below, is not dropped to a table.
    #[test]
    fn area_chart_lowers_to_a_line_plot_with_an_approximation_note() {
        let data = ChartData {
            kind: ChartKind::Area,
            series: vec![ChartSeries {
                name: None,
                values: vec!["1".into(), "2".into()],
            }],
            ..Default::default()
        };
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        let chart = chart.unwrap();
        let plot = plot_of(&chart);
        assert_eq!(plot.kind, PlotKind::Line);
        assert!(
            has_chart_note(&report, "outline"),
            "expected an area-as-outline approximation note: {:?}",
            report.notes
        );
    }

    /// A chart type `lilaq` has no mark for (pie, radar, stock, surface, any
    /// ChartEx type — all `ChartKind::Other`) falls back to the table even
    /// under `ChartStyle::Plot`, with a note naming the reason.
    #[test]
    fn unplottable_chart_kind_falls_back_to_table_with_a_note() {
        let data = ChartData {
            kind: ChartKind::Other,
            title: Some("Share".into()),
            series: vec![ChartSeries {
                name: None,
                values: vec!["1".into(), "2".into()],
            }],
            ..Default::default()
        };
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        let chart = chart.expect("expected a chart");
        table_of(&chart); // falls back to a table, not a plot
        assert!(
            has_chart_note(&report, "no plotting counterpart"),
            "expected a reason naming the unplottable kind: {:?}",
            report.notes
        );
    }

    /// Non-numeric cached values (a value series that isn't actually
    /// numbers) can't be plotted; falls back to the table with a note.
    #[test]
    fn non_numeric_values_fall_back_to_table_with_a_note() {
        let data = bar_chart_data(
            None,
            &[],
            vec![ChartSeries {
                name: None,
                values: vec!["N/A".into(), "2.5".into()],
            }],
        );
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        table_of(&chart.expect("expected a chart"));
        assert!(
            has_chart_note(&report, "not numeric"),
            "expected a reason naming the non-numeric values: {:?}",
            report.notes
        );
    }

    /// A missing cached value (Word's sparse-`idx` gap filler — an empty
    /// string, not absence) can't be plotted without either misaligning the
    /// rest of the series against the category ticks or inventing a `0.0`
    /// that was never there; falls back to the table with a note.
    #[test]
    fn missing_value_falls_back_to_table_with_a_note() {
        let data = bar_chart_data(
            None,
            &["Category 1", "Category 2", "Category 3"],
            vec![ChartSeries {
                name: None,
                values: vec!["10".into(), "".into(), "30".into()],
            }],
        );
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        table_of(&chart.expect("expected a chart"));
        assert!(
            has_chart_note(&report, "missing data points"),
            "expected a reason naming the missing data point: {:?}",
            report.notes
        );
    }

    /// A chart with series but no series carrying any value at all (every
    /// series is empty) has nothing to plot.
    #[test]
    fn no_plottable_values_falls_back_to_table_with_a_note() {
        let data =
            bar_chart_data(None, &[], vec![ChartSeries { name: None, values: vec![] }]);
        let package = package_with("chart1.xml", data);
        let (chart, report) = lower(&package, &plot_options(), "rId1");
        table_of(&chart.expect("expected a chart"));
        assert!(
            has_chart_note(&report, "no plottable series data"),
            "expected a reason naming the lack of plottable data: {:?}",
            report.notes
        );
    }
}

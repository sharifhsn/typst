//! Versioned machine-readable fidelity manifest embedded in each DOCX.

use std::fmt::Write;

use typst_ooxml_core::xml::{XML_DECL, escape_attr};

use crate::dom::DocxDocument;

pub const PART_NAME: &str = "customXml/typstFidelity.xml";
pub const REL_TYPE: &str = "https://typst.app/schema/2026/relationships/fidelity";
const NS: &str = "https://typst.app/schema/2026/fidelity";

/// Serializes the document's stable export snapshot and fidelity report.
pub fn build(document: &DocxDocument) -> String {
    let report = document.fidelity_report();
    let snapshot = document.export_snapshot();
    let counts = report.counts();
    let dynamic_field_count: usize =
        report.dynamic_fields().iter().map(|field| field.occurrences).sum();
    let referenced_font_count: usize =
        report.fonts().iter().map(|font| font.occurrences).sum();
    let missing_font_count: usize = report
        .fonts()
        .iter()
        .filter(|font| !font.available_at_export)
        .map(|font| font.occurrences)
        .sum();
    let drawing_count = report.drawings().len();
    let unlabeled_drawing_count =
        report.drawings().iter().filter(|drawing| drawing.unlabeled()).count();
    let mut out = String::from(XML_DECL);
    let _ = write!(
        out,
        "<typst:fidelity xmlns:typst=\"{NS}\" version=\"1\" enrollment=\"partial\" snapshotId=\"{:032x}\">",
        snapshot.logical_id()
    );
    let _ = write!(
        out,
        "<typst:counts native=\"{}\" nativeWithFallback=\"{}\" approximate=\"{}\" raster=\"{}\" drop=\"{}\" dynamicFields=\"{}\" referencedFonts=\"{}\" missingFonts=\"{}\" drawings=\"{}\" unlabeledDrawings=\"{}\" measuredTables=\"{}\"/>",
        counts.native,
        counts.native_with_fallback,
        counts.approximate,
        counts.raster,
        counts.drop,
        dynamic_field_count,
        referenced_font_count,
        missing_font_count,
        drawing_count,
        unlabeled_drawing_count,
        snapshot.tables().len()
    );

    out.push_str("<typst:pages>");
    for (index, page) in snapshot.pages().iter().enumerate() {
        let _ = write!(
            out,
            "<typst:page index=\"{}\" widthPt=\"{}\" heightPt=\"{}\"/>",
            index + 1,
            page.width_pt,
            page.height_pt
        );
    }
    out.push_str("</typst:pages><typst:bibliography>");
    for entry in snapshot.bibliography_entries() {
        let _ = write!(
            out,
            "<typst:entry id=\"{:032x}\" key=\"{}\"/>",
            entry.logical_id,
            escape_attr(&entry.key)
        );
    }
    out.push_str("</typst:bibliography><typst:tables>");
    for table in snapshot.tables() {
        let _ = write!(
            out,
            "<typst:table sourceId=\"{:032x}\" page=\"{}\">",
            table.logical_id, table.page
        );
        for cell in &table.cells {
            let _ = write!(
                out,
                "<typst:cell page=\"{}\" x=\"{}\" y=\"{}\" colspan=\"{}\" rowspan=\"{}\" leftPt=\"{}\" topPt=\"{}\" widthPt=\"{}\" heightPt=\"{}\" axisAligned=\"{}\"/>",
                cell.page,
                cell.x,
                cell.y,
                cell.colspan,
                cell.rowspan,
                cell.left_pt,
                cell.top_pt,
                cell.width_pt,
                cell.height_pt,
                cell.axis_aligned
            );
        }
        out.push_str("</typst:table>");
    }
    out.push_str("</typst:tables><typst:nodes>");
    for node in snapshot.nodes() {
        let _ = write!(
            out,
            "<typst:node id=\"{:032x}\" element=\"{}\" semanticOccurrences=\"{}\">",
            node.source.logical_id,
            escape_attr(&node.source.element),
            node.semantic_occurrences
        );
        for position in &node.paged_positions {
            let _ = write!(
                out,
                "<typst:position page=\"{}\" xPt=\"{}\" yPt=\"{}\"/>",
                position.page, position.x_pt, position.y_pt
            );
        }
        out.push_str("</typst:node>");
    }
    out.push_str("</typst:nodes><typst:decisions>");
    for decision in report.decisions() {
        let losses = decision.losses;
        let _ = write!(
            out,
            "<typst:decision sourceId=\"{:032x}\" representation=\"{:?}\" reason=\"{:?}\" occurrences=\"{}\" affectedTextChars=\"{}\" affectedSemanticNodes=\"{}\" visual=\"{}\" semantic=\"{}\" editability=\"{}\" dynamic=\"{}\" accessibility=\"{}\" portability=\"{}\"/>",
            decision.source.logical_id,
            decision.representation,
            decision.reason,
            decision.occurrences,
            decision.affected_text_chars,
            decision.affected_semantic_nodes,
            losses.visual_fidelity,
            losses.semantic_structure,
            losses.editability,
            losses.dynamic_behavior,
            losses.accessibility,
            losses.portability
        );
    }
    out.push_str("</typst:decisions><typst:dynamicFields>");
    for field in report.dynamic_fields() {
        let _ = write!(
            out,
            "<typst:field id=\"{:032x}\" kind=\"{}\" instruction=\"{}\" owner=\"{:?}\" visibility=\"{:?}\" cache=\"{:?}\" occurrences=\"{}\"/>",
            field.logical_id,
            escape_attr(&field.kind),
            escape_attr(&field.instruction),
            field.owner,
            field.visibility,
            field.cache_status,
            field.occurrences
        );
    }
    out.push_str("</typst:dynamicFields><typst:fonts>");
    for font in report.fonts() {
        let _ = write!(
            out,
            "<typst:font id=\"{:032x}\" family=\"{}\" availableAtExport=\"{}\" embedded=\"{}\" occurrences=\"{}\"/>",
            font.logical_id,
            escape_attr(&font.family),
            font.available_at_export,
            font.embedded,
            font.occurrences
        );
    }
    out.push_str("</typst:fonts><typst:drawings>");
    for drawing in report.drawings() {
        let _ = write!(
            out,
            "<typst:drawing id=\"{:032x}\" docPrId=\"{}\" name=\"{}\" decorative=\"{}\" nativeText=\"{}\" unlabeled=\"{}\"",
            drawing.logical_id,
            drawing.docpr_id,
            escape_attr(&drawing.name),
            drawing.decorative,
            drawing.native_text,
            drawing.unlabeled()
        );
        if let Some(alt) = &drawing.alternative_text {
            let _ = write!(out, " alternativeText=\"{}\"", escape_attr(alt));
        }
        out.push_str("/>");
    }
    out.push_str("</typst:drawings><typst:suppressedDiagnostics>");
    for suppressed in report.suppressed_diagnostics() {
        let _ = write!(
            out,
            "<typst:diagnostic sourceId=\"{:032x}\" stage=\"{:?}\" kind=\"{:?}\" occurrences=\"{}\" message=\"{}\"/>",
            suppressed.source.logical_id,
            suppressed.stage,
            suppressed.kind,
            suppressed.occurrences,
            escape_attr(&suppressed.diagnostic.message)
        );
    }
    out.push_str("</typst:suppressedDiagnostics></typst:fidelity>");
    out
}

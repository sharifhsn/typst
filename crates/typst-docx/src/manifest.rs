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
    let mut out = String::from(XML_DECL);
    let _ = write!(
        out,
        "<typst:fidelity xmlns:typst=\"{NS}\" version=\"1\" enrollment=\"partial\" snapshotId=\"{:032x}\">",
        snapshot.logical_id()
    );
    let _ = write!(
        out,
        "<typst:counts native=\"{}\" nativeWithFallback=\"{}\" approximate=\"{}\" raster=\"{}\" drop=\"{}\"/>",
        counts.native,
        counts.native_with_fallback,
        counts.approximate,
        counts.raster,
        counts.drop
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
    out.push_str("</typst:pages><typst:nodes>");
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
    out.push_str("</typst:decisions><typst:suppressedDiagnostics>");
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

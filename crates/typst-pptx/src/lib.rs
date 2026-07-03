//! Typst's PPTX (Microsoft PowerPoint) exporter.
//!
//! Unlike the Word exporter, which reflows the realized element tree, this
//! crate consumes the *laid-out* [`PagedDocument`] — one Typst page becomes one
//! slide, with every element placed at its exact frame position. That makes it
//! a sibling of the PNG/SVG renderers rather than of the DOCX exporter: there
//! are no show rules, no convergence, and no engine.
//!
//! Each page's frame is walked into a slide IR (`dom`): positioned text boxes
//! (live, editable DrawingML runs), pictures, native vector shapes
//! (`a:custGeom`/`prstGeom` with solid, gradient, and translucent fills), and
//! groups. Anything with no OOXML equivalent is rasterized into a positioned
//! picture so the visual is preserved. The IR is then serialized into a
//! deterministic, schema-strict OPC package that opens without repair in
//! Microsoft PowerPoint and LibreOffice Impress.

#[allow(dead_code)]
mod dom;
mod encode;
mod image;
mod package;
mod shape;
mod slide;
mod text;
#[allow(dead_code)]
mod xml;

use typst_layout::PagedDocument;
use typst_library::diag::SourceResult;

use crate::dom::SlideCtx;

/// Reserved options for PPTX export.
pub struct PptxOptions {}

/// Export a paged Typst document as a PowerPoint presentation.
pub fn pptx(document: &PagedDocument, options: &PptxOptions) -> SourceResult<Vec<u8>> {
    let _ = options;
    let mut ctx = SlideCtx::default();
    let slides = slide::slides(document, &mut ctx);
    Ok(package::write(document, &slides, &ctx))
}

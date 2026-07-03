//! Typst's PPTX (Microsoft PowerPoint) exporter.
//!
//! This crate is currently the foundation layer: it converts each laid-out
//! Typst page into one slide IR with a solid background, then assembles a
//! deterministic, schema-friendly OPC package.

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

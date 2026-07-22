//! Typst's PPTX (Microsoft PowerPoint) exporter.
//!
//! Unlike the Word exporter, which reflows the realized element tree, this
//! crate consumes the *laid-out* [`PagedDocument`] — one Typst page becomes one
//! slide, with every element placed at its exact frame position. That makes it
//! a sibling of the PNG/SVG renderers rather than of the DOCX exporter: there
//! are no show rules and no convergence. The one exception is math: an
//! equation's frames are glyphs, and OMML needs the *structure* those glyphs
//! were laid out from. So the equation pass builds a short-lived post-layout
//! `Engine` (see `slide::equation_sources`), resolves each queried equation
//! back into Typst's math IR, and lowers that with `typst-omml` — the same
//! Typst→OMML mapping the DOCX exporter uses. What the detour costs, and what
//! happens to an equation it refuses, is recorded in the `FidelityReport`.
//!
//! Each page's frame is walked into a slide IR (`dom`): positioned text boxes
//! (live, editable DrawingML runs), pictures, native vector shapes
//! (`a:custGeom`/`prstGeom` with solid, gradient, and translucent fills), and
//! groups. Anything with no OOXML equivalent is rasterized into a positioned
//! picture so the visual is preserved. The IR is then serialized into a
//! deterministic, schema-strict OPC package that opens without repair in
//! Microsoft PowerPoint and LibreOffice Impress.

mod dom;
mod encode;
mod image;
mod package;
mod report;
mod shape;
mod slide;
mod table;
mod text;
mod xml;

use typst_layout::PagedDocument;
use typst_library::World;
use typst_library::diag::{SourceResult, bail};
use typst_library::foundations::{Label, Selector, Value};
use typst_library::introspection::Introspector;
use typst_syntax::Span;

use crate::dom::SlideCtx;
pub use crate::report::{
    DecisionReason, ExportDecision, ExportSource, FidelityReport, FontFact, LossSet,
    Representation, RepresentationCounts,
};

/// Reserved options for PPTX export.
#[derive(Default)]
pub struct PptxOptions {
    /// Speaker notes keyed by zero-based slide index.
    ///
    /// When this is `None`, the exporter extracts Touying/pdfpc notes from
    /// `#metadata(..) <pdfpc-file>` entries in the document introspector.
    pub speaker_notes: Option<Vec<SpeakerNote>>,
}

/// Plain-text speaker notes for one slide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeakerNote {
    /// Zero-based slide index in the exported presentation.
    pub slide_index: usize,
    /// Note text to place in PowerPoint's Notes pane.
    pub text: String,
}

/// Export a paged Typst document as a PowerPoint presentation.
///
/// `world` is the compilation environment the document came from. The exporter
/// consumes the laid-out document, so it needs no world for the frame walk
/// itself; the world is required only to build a short-lived post-layout
/// [`Engine`](typst_library::engine::Engine) for the math pass, which has to
/// re-enter library routines that are not reachable from frames alone.
pub fn pptx(
    document: &PagedDocument,
    world: &dyn World,
    options: &PptxOptions,
) -> SourceResult<Vec<u8>> {
    pptx_impl(document, world, options, None).map(PptxExport::into_bytes)
}

/// Export a filtered paged document while retaining its original physical-page
/// numbering for explicit slide links.
///
/// Each entry maps one zero-based original page to an optional zero-based slide
/// in `document`. Omitted or out-of-range targets are dropped.
pub fn pptx_with_page_mapping(
    document: &PagedDocument,
    world: &dyn World,
    options: &PptxOptions,
    physical_page_to_slide: &[Option<usize>],
) -> SourceResult<Vec<u8>> {
    pptx_impl(document, world, options, Some(physical_page_to_slide))
        .map(PptxExport::into_bytes)
}

/// Export a paged Typst document as a PowerPoint presentation, keeping the
/// structured [`FidelityReport`] alongside the package bytes.
///
/// This is the reporting sibling of [`pptx`]: same bytes, same behavior, but
/// the caller can additionally inspect every non-native representation
/// decision the exporter made (rasterized shapes/groups/text/images,
/// transformed-table fallback, tiling/gradient-text approximation, math's
/// OMML/text-fallback pair, and page-background substitution).
pub fn pptx_with_report(
    document: &PagedDocument,
    world: &dyn World,
    options: &PptxOptions,
) -> SourceResult<PptxExport> {
    pptx_impl(document, world, options, None)
}

/// [`pptx_with_page_mapping`], keeping the structured [`FidelityReport`]
/// alongside the package bytes. See [`pptx_with_report`].
pub fn pptx_with_page_mapping_and_report(
    document: &PagedDocument,
    world: &dyn World,
    options: &PptxOptions,
    physical_page_to_slide: &[Option<usize>],
) -> SourceResult<PptxExport> {
    pptx_impl(document, world, options, Some(physical_page_to_slide))
}

/// The result of a successful PPTX export: the finished OPC package bytes
/// plus the [`FidelityReport`] describing every non-native representation
/// decision made while producing them.
///
/// This is the PPTX analogue of how `typst-docx` exposes its report from
/// `DocxDocument::fidelity_report()`. The two crates' pipelines differ (DOCX
/// needs a typed `Document` stage to participate in Typst's generic compile
/// pipeline; this exporter is a direct post-layout pass over a
/// `PagedDocument`, more like `typst-pdf` or `typst-svg`), so rather than
/// inventing an intermediate typed-document stage this crate doesn't
/// otherwise need, the report simply travels with the bytes. [`pptx`] and
/// [`pptx_with_page_mapping`] remain thin wrappers over this that discard the
/// report for callers who don't need it.
#[derive(Debug, Clone)]
pub struct PptxExport {
    bytes: Vec<u8>,
    report: FidelityReport,
}

impl PptxExport {
    /// The finished OPC package bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the export, keeping only the package bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Structured fidelity decisions recorded while building this package.
    pub fn fidelity_report(&self) -> &FidelityReport {
        &self.report
    }
}

fn pptx_impl(
    document: &PagedDocument,
    world: &dyn World,
    options: &PptxOptions,
    physical_page_to_slide: Option<&[Option<usize>]>,
) -> SourceResult<PptxExport> {
    let mut ctx = SlideCtx {
        physical_page_to_slide: physical_page_to_slide.map(|mapping| mapping.to_vec()),
        ..SlideCtx::default()
    };
    // Math is the one part of the export that cannot be read off the frames: it
    // is lowered up front through a short-lived post-layout engine, then keyed
    // by location for the page walk to pick up.
    let equations = slide::equation_sources(document, world);
    let slides = slide::slides(document, &equations, &mut ctx);
    let extracted;
    let notes = match &options.speaker_notes {
        Some(notes) => notes.as_slice(),
        None => {
            extracted = speaker_notes(document);
            &extracted
        }
    };
    match package::write(document, &slides, &ctx, notes) {
        Ok(bytes) => Ok(PptxExport { bytes, report: ctx.fidelity_report }),
        Err(err) => bail!(Span::detached(), "failed to finalize PPTX package: {err}"),
    }
}

/// Extract Touying/pdfpc speaker notes from `#metadata(..) <pdfpc-file>`.
///
/// The pdfpc payload uses one-based slide numbers. If overlays or subslides
/// produce multiple entries for the same physical slide, package assembly
/// concatenates them in source order.
pub fn speaker_notes(document: &PagedDocument) -> Vec<SpeakerNote> {
    let Ok(label) = Label::construct("pdfpc-file".into()) else {
        return Vec::new();
    };

    document
        .introspector()
        .query(&Selector::Label(label))
        .into_iter()
        .filter_map(|content| content.field_by_name("value").ok())
        .flat_map(value_to_speaker_notes)
        .collect()
}

fn value_to_speaker_notes(value: Value) -> Vec<SpeakerNote> {
    // The real `<pdfpc-file>` value is a dict `{pdfpcFormat, disableMarkdown,
    // pages: [...]}` — what `typst query --field value --one "<pdfpc-file>"`
    // emits from touying `#note(..)` / the pdfpc integration. Tolerate a bare
    // array too, in case a producer stores the page list directly.
    let entries = match value {
        Value::Array(entries) => entries,
        Value::Dict(dict) => match dict.get("pages") {
            Ok(Value::Array(entries)) => entries.clone(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    entries
        .into_iter()
        .filter_map(|entry| {
            let Value::Dict(dict) = entry else {
                return None;
            };

            let idx = match dict.get("idx").ok()? {
                Value::Int(value) if *value >= 1 => usize::try_from(*value).ok()?,
                Value::Float(value) if *value >= 1.0 && value.fract() == 0.0 => {
                    usize::try_from(*value as i64).ok()?
                }
                _ => return None,
            };

            let Value::Str(note) = dict.get("note").ok()? else {
                return None;
            };

            let text = note.to_string();
            (!text.is_empty()).then_some(SpeakerNote { slide_index: idx - 1, text })
        })
        .collect()
}

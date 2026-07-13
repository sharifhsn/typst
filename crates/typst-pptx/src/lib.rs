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
use typst_library::foundations::{Label, Selector, Value};
use typst_library::introspection::Introspector;

use crate::dom::SlideCtx;

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
pub fn pptx(document: &PagedDocument, options: &PptxOptions) -> SourceResult<Vec<u8>> {
    let mut ctx = SlideCtx::default();
    let slides = slide::slides(document, &mut ctx);
    let extracted;
    let notes = match &options.speaker_notes {
        Some(notes) => notes.as_slice(),
        None => {
            extracted = speaker_notes(document);
            &extracted
        }
    };
    Ok(package::write(document, &slides, &ctx, notes))
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

//! Typst's DOCX (Microsoft Word) exporter.
//!
//! Mirrors `typst-html`: a `Target::Docx` for which no element show rules are
//! registered, so the realized native element tree reaches the converter intact.
//! The converter walks that tree into a typed OOXML IR and emits an OPC zip.

mod bibliography;
mod bookmark;
mod convert;
mod ctx;
mod document;
mod dom;
mod encode;
mod fallback;
mod introspect;
mod invariants;
mod manifest;
mod mappers;
mod package;
mod parts;
mod props;
mod report;
mod rules;
mod snapshot;
mod styles_part;
mod xml;

pub use self::ctx::DocxCtx;
pub use self::document::{
    docx_document, docx_document_with_paged_geometry,
    docx_document_with_paged_introspector,
};
pub use self::dom::DocxDocument;
pub use self::encode::{DocxOptions, docx};
pub use self::introspect::DocxIntrospector;
pub use self::mappers::math::equation_omml_fragment;
pub use self::report::{
    DecisionReason, DynamicFieldFact, ExportDecision, ExportSource, ExportStage,
    FidelityReport, FieldOwner, FieldVisibility, FontFact, LossSet,
    Representation, RepresentationCounts, SuppressedDiagnostic, SuppressedKind,
};
pub use self::rules::register;
pub use self::snapshot::{ExportSnapshot, SnapshotNode, SnapshotPage, SnapshotPosition};

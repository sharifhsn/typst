//! Typst's DOCX (Microsoft Word) exporter.
//!
//! Mirrors `typst-html`: a `Target::Docx` for which no element show rules are
//! registered, so the realized native element tree reaches the converter intact.
//! The converter walks that tree into a typed OOXML IR and emits an OPC zip.

mod bookmark;
mod convert;
mod ctx;
mod document;
mod dom;
mod encode;
mod introspect;
mod mappers;
mod package;
mod parts;
mod props;
mod rules;
mod styles_part;
mod xml;

pub use self::ctx::DocxCtx;
pub use self::document::{docx_document, docx_document_with_paged_introspector};
pub use self::dom::DocxDocument;
pub use self::encode::{DocxOptions, docx};
pub use self::introspect::DocxIntrospector;
pub use self::rules::register;

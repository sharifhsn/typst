//! Typst's Pandoc (JSON AST) exporter.
//!
//! Mirrors `typst-docx`/`typst-html`: a `Target::Pandoc` for which no element
//! show rules are registered, so the realized native element tree reaches the
//! converter intact. The converter walks that tree into a typed Pandoc AST
//! ([`ast`]) and serializes it to a single self-contained Pandoc JSON object.

mod ast;
mod convert;
mod ctx;
mod document;
mod dom;
mod encode;
mod introspect;
mod mappers;
mod normalize;
mod rules;

pub use self::ctx::PandocCtx;
pub use self::document::pandoc_document;
pub use self::dom::PandocDocument;
pub use self::encode::{PandocOptions, pandoc};
pub use self::introspect::PandocIntrospector;
pub use self::rules::register;

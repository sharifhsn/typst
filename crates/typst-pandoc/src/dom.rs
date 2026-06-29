//! The output document type that ties `PandocDocument` to `Target::Pandoc`.
//!
//! Unlike the DOCX exporter there is no OOXML IR here: the conversion produces
//! the vendored Pandoc [`ast`] types directly. This module only carries the
//! `Output`/`Document` glue plus the realized blocks, the document metadata,
//! and the introspector.

use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Output, StyleChain, Target};
use typst_library::introspection::Introspector;
use typst_library::model::{Document, DocumentInfo};

use crate::ast::Block;
use crate::introspect::PandocIntrospector;

/// Output document: the realized native tree lowered to the Pandoc AST, plus
/// metadata and the (pageless) introspector.
pub struct PandocDocument {
    pub(crate) info: DocumentInfo,
    pub(crate) blocks: Vec<Block>,
    pub(crate) introspector: Arc<PandocIntrospector>,
    /// The document's bibliography serialized to a BibLaTeX (`.bib`) string, or
    /// `None` if the document has no bibliography. The CLI writes this as a
    /// sidecar next to the JSON output so that `pandoc --citeproc` can re-resolve
    /// the structured `Cite` nodes the converter emits.
    pub(crate) bibliography: Option<String>,
}

impl PandocDocument {
    pub fn info(&self) -> &DocumentInfo {
        &self.info
    }

    /// The synthesized BibLaTeX (`.bib`) source for the document's bibliography,
    /// if any. Intended to be written as a sidecar beside the Pandoc JSON.
    pub fn bibliography(&self) -> Option<&str> {
        self.bibliography.as_deref()
    }
}

impl Document for PandocDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Output for PandocDocument {
    fn introspector(&self) -> &dyn Introspector {
        self.introspector.as_ref()
    }

    fn target() -> Target {
        Target::Pandoc
    }

    fn create(
        engine: &mut Engine,
        content: &Content,
        styles: StyleChain,
    ) -> SourceResult<Self> {
        crate::pandoc_document(engine, content, styles)
    }
}

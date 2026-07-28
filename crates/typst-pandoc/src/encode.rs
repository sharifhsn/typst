//! Serializes the Pandoc AST IR into a single JSON byte buffer.

use typst_library::diag::{At, SourceResult};
use typst_syntax::Span;

use crate::ast::{PANDOC_API_VERSION, Pandoc};
use crate::dom::PandocDocument;

/// Settings for Pandoc export.
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct PandocOptions {
    /// Whether to pretty-print the JSON.
    pub pretty: bool,
    /// The filename of the synthesized `.bib` sidecar, if one was written. When
    /// set, it is recorded in the document's `meta.bibliography` so that running
    /// `pandoc --citeproc` (which reads `bibliography` from the metadata)
    /// re-resolves the structured `Cite` nodes against it. The CLI sets this to
    /// the sidecar's path (relative to the JSON output, so the metadata is
    /// portable) after writing the sidecar.
    pub bibliography: Option<String>,
}

/// Serializes a Pandoc document into JSON bytes.
///
/// Builds the internal `Pandoc` envelope (api-version + meta + blocks) and
/// streams it out with a single `serde_json` pass — strictly less work than a
/// PDF compile, which this skips layout/raster/PDF for entirely. The in-memory value is
/// necessary and cheap (same order as the realized tree); we avoid building a
/// giant intermediate `String`.
#[typst_macros::time(name = "pandoc encode")]
pub fn pandoc(
    document: &PandocDocument,
    options: &PandocOptions,
) -> SourceResult<Vec<u8>> {
    let doc = Pandoc {
        pandoc_api_version: PANDOC_API_VERSION,
        meta: crate::document::build_meta(
            &document.info,
            options.bibliography.as_deref(),
        ),
        // The blocks are already built; borrow them into a throwaway envelope by
        // cloning is avoided by serializing a borrowed view instead.
        blocks: Vec::new(),
    };
    // Serialize via a borrowed view so we never clone the (potentially large)
    // block tree.
    let view = PandocView {
        pandoc_api_version: doc.pandoc_api_version,
        meta: &doc.meta,
        blocks: &document.blocks,
    };

    let mut buf = Vec::new();
    let res = if options.pretty {
        serde_json::to_writer_pretty(&mut buf, &view)
    } else {
        serde_json::to_writer(&mut buf, &view)
    };
    res.map_err(|err| ecow::eco_format!("failed to serialize Pandoc JSON ({err})"))
        .at(Span::detached())?;
    Ok(buf)
}

/// A borrowing view over a [`Pandoc`] document, so serialization never clones
/// the block tree. Serializes byte-identically to [`Pandoc`].
#[derive(serde::Serialize)]
struct PandocView<'a> {
    #[serde(rename = "pandoc-api-version")]
    pandoc_api_version: [u32; 4],
    meta: &'a crate::ast::Meta,
    blocks: &'a [crate::ast::Block],
}

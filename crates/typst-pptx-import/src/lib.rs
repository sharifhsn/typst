//! Convert PowerPoint presentations (`.pptx`) into Typst source, as
//! [touying](https://github.com/touying-typ/touying) slides.
//!
//! The mirror of the `typst-pptx` exporter, and the fourth corner of the
//! Office interop set:
//!
//! ```text
//! .pptx ─opc::Reader─► xml ─pml::parse─► PmlPackage ─lower─► TypstDoc ─emit─► .typ + assets
//!          (ooxml-core)         │          (PowerPoint IR)  (mappers) (Typst IR)  (touying)
//!                          resolve/ (masters, layouts, theme colours)
//! ```
//!
//! # Why this is not the Word importer with the nouns changed
//!
//! **A `.docx` is a flow; a `.pptx` is a coordinate system.** Word content is
//! a linear stream that Typst also lays out linearly, so that importer's job
//! is mostly translation. Every PowerPoint shape instead carries an absolute
//! `a:off`/`a:ext` in EMU, and there is no flow to recover — so this importer
//! reproduces the coordinates ([`Fidelity::Placed`], the default, and the only
//! mode that round-trips) or infers structure from the placeholders PowerPoint
//! itself labelled ([`Fidelity::Idiomatic`]).
//!
//! **Nothing states its own formatting.** A slide's shapes inherit position,
//! size, font, colour and bullet style from their layout, which inherits from
//! its master, whose colours are names only `theme1.xml` resolves. That chain
//! is in 100% of real presentations and has no Word analogue; see
//! [`resolve`].
//!
//! Everything that cannot come across is recorded in an [`ImportReport`]
//! rather than dropped in silence.

pub mod emit;
pub mod lower;
pub mod mappers;
pub mod opts;
pub mod pml;
pub mod report;
pub mod resolve;
pub mod tdoc;

use std::path::PathBuf;

use ecow::EcoString;
use typst_ooxml_core::opc::Reader;

pub use opts::{Fidelity, ImportOptions};
pub use report::ImportReport;
pub use tdoc::TypstDoc;

/// The Typst source, the media to write beside it, and what was lost.
pub struct ImportResult {
    pub source: String,
    /// `(path relative to the emitted file, bytes)`.
    pub assets: Vec<(PathBuf, Vec<u8>)>,
    pub report: ImportReport,
}

#[derive(Debug)]
pub enum ImportError {
    Package(typst_ooxml_core::opc::ReadError),
    /// `ppt/presentation.xml` was absent — not a PowerPoint presentation.
    NotAPresentation,
    Xml(EcoString),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Package(e) => write!(f, "invalid OPC package: {e}"),
            Self::NotAPresentation => {
                write!(f, "not a PowerPoint presentation (ppt/presentation.xml is missing)")
            }
            Self::Xml(e) => write!(f, "malformed XML: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Import a `.pptx` with the default options.
pub fn import_pptx(bytes: &[u8]) -> Result<ImportResult, ImportError> {
    import_pptx_with(bytes, &ImportOptions::default())
}

pub fn import_pptx_with(
    bytes: &[u8],
    opts: &ImportOptions,
) -> Result<ImportResult, ImportError> {
    let reader = Reader::open(bytes).map_err(ImportError::Package)?;
    let mut parser = pml::parse::Parser::new(reader);
    let package = parser.parse()?;

    // The parser stays alive through lowering: it owns both the archive reader
    // (for media bytes) and the relationship table (for resolving them).
    let mut report = std::mem::take(&mut parser.report);
    let (doc, assets) = lower::lower(&package, &mut parser, opts, &mut report);
    let source = emit::emit(&doc, opts);
    Ok(ImportResult { source, assets, report })
}

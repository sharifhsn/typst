//! Convert Word documents (`.docx`) into Typst source.
//!
//! This is the mirror image of the `typst-docx` exporter: where that walks a
//! Typst document and lowers it to OOXML, this parses OOXML and raises it to
//! Typst source. The pipeline has a named counterpart for every export phase:
//!
//! ```text
//! .docx ─opc::Reader─► xml ─wml::parse─► WmlPackage ─lower─► TypstDoc ─emit─► .typ + assets
//!         (ooxml-core)         │            (Word IR)   (mappers) (Typst IR) (pretty)
//!                         resolve/ (styles, numbering)                  passes/ (tier 2)
//! ```
//!
//! The two-IR split is deliberate: lowering to a Typst-shaped [`tdoc::TypstDoc`]
//! (rather than emitting strings straight from the Word IR) is what lets the
//! output be made idiomatic by *adding passes* instead of rewriting the emitter.
//! Everything the importer can't map cleanly is recorded in an
//! [`report::ImportReport`] rather than silently lost.

pub mod emit;
pub mod lower;
pub mod mappers;
pub mod opts;
pub mod passes;
pub mod report;
pub mod resolve;
pub mod tdoc;
pub mod wml;

use std::path::PathBuf;

use ecow::EcoString;

pub use opts::{ChartStyle, ImportOptions, Tier, TrackedChanges};
pub use report::ImportReport;
pub use tdoc::TypstDoc;

/// The result of importing a `.docx`: the Typst source, the image assets that
/// must be written beside it, and the loss report.
pub struct ImportResult {
    pub source: String,
    /// `(project-relative path, bytes)` for each extracted media asset. Mirrors
    /// the exporter's `package.rs` returning parts.
    pub assets: Vec<(PathBuf, Vec<u8>)>,
    pub report: ImportReport,
}

/// Errors that abort an import before any source is produced.
#[derive(Debug)]
pub enum ImportError {
    /// The package could not be opened or a required part was missing/invalid.
    Package(typst_ooxml_core::opc::ReadError),
    /// `word/document.xml` was absent — not a Word document.
    NotAWordDocument,
    /// An XML part was malformed.
    Xml(EcoString),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Package(e) => write!(f, "{e}"),
            ImportError::NotAWordDocument => {
                write!(f, "not a Word document (word/document.xml is missing)")
            }
            ImportError::Xml(e) => write!(f, "malformed XML: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<typst_ooxml_core::opc::ReadError> for ImportError {
    fn from(e: typst_ooxml_core::opc::ReadError) -> Self {
        ImportError::Package(e)
    }
}

/// Import a `.docx` (its raw bytes) into Typst source with default options.
pub fn import_docx(bytes: &[u8]) -> Result<ImportResult, ImportError> {
    import_docx_with(bytes, &ImportOptions::default())
}

/// Import a `.docx` with explicit options.
pub fn import_docx_with(
    bytes: &[u8],
    options: &ImportOptions,
) -> Result<ImportResult, ImportError> {
    let mut report = ImportReport::default();

    // 1. Open the OPC package and parse the Word IR.
    let package = wml::parse::parse_package(bytes, &mut report)?;

    // 2. Lower the Word IR to the Typst IR (tier-1 literal fidelity).
    let mut ctx = lower::LowerCtx::new(&package, options, &mut report);
    let mut doc = lower::lower(&mut ctx);

    // 3. Optionally run tier-2 semantic passes.
    if options.tier == Tier::Idiomatic {
        passes::run(&mut doc, &mut report);
    }

    // 4. Emit source, collecting the assets referenced by figures.
    let (source, assets) = emit::emit(&doc, &package, options);

    Ok(ImportResult { source, assets, report })
}

//! Loss/uncertainty reporting — the mirror of `typst-docx`'s `FidelityReport`.
//! Every construct the importer dropped or approximated is recorded here, so
//! "usually good, sometimes literal" output is *auditable* rather than silent.
//!
//! The two-severity dedup mechanism lives in
//! [`typst_ooxml_core::report`]; this module keeps only the DOCX-facing
//! surface — the public `notes` list and the severity-sorted [`summary`].
//!
//! [`summary`]: ImportReport::summary

use ecow::EcoString;
use rustc_hash::FxHashSet;
use typst_ooxml_core::report::{Entry, dedup_push};

pub use typst_ooxml_core::report::Severity;

/// A record of one construct the importer could not map cleanly: a severity,
/// what construct (e.g. "OMML equation", "field"), and how it was handled.
pub type Note = Entry;

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub notes: Vec<Note>,
    /// De-dup key for everything already in `notes`. A document that repeats
    /// the same unmapped construct many times over — e.g. 200 `PAGE` fields,
    /// or 50 unresolved hyperlinks — must not produce one identical summary
    /// line per occurrence.
    seen: FxHashSet<Note>,
}

impl ImportReport {
    pub fn approximate(
        &mut self,
        what: impl Into<EcoString>,
        detail: impl Into<EcoString>,
    ) {
        self.push(Severity::Approximate, what.into(), detail.into());
    }

    pub fn drop(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.push(Severity::Drop, what.into(), detail.into());
    }

    fn push(&mut self, severity: Severity, what: EcoString, detail: EcoString) {
        dedup_push(&mut self.notes, &mut self.seen, Note { severity, what, detail });
    }

    /// A one-line-per-note human summary, most severe first.
    pub fn summary(&self) -> String {
        let mut notes = self.notes.clone();
        // Most severe first; a stable sort keeps discovery order within a level.
        notes.sort_by_key(|n| std::cmp::Reverse(n.severity));
        notes
            .iter()
            .map(|n| {
                let sev = match n.severity {
                    Severity::Drop => "dropped",
                    Severity::Approximate => "approximated",
                };
                format!("- [{sev}] {}: {}", n.what, n.detail)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

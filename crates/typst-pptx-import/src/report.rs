//! What the import could not carry across, recorded rather than lost.
//!
//! The two-severity dedup mechanism lives in [`typst_ooxml_core::report`] (its
//! doc explains the Approximate/Drop distinction and the dedup); this module
//! keeps only the PPTX-facing surface — the `entries()` accessor and the
//! [`Display`](std::fmt::Display) rendering.

use rustc_hash::FxHashSet;
use typst_ooxml_core::report::dedup_push;

pub use typst_ooxml_core::report::{Entry, Severity};

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    entries: Vec<Entry>,
    seen: FxHashSet<Entry>,
}

impl ImportReport {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn approximate(
        &mut self,
        what: impl Into<ecow::EcoString>,
        detail: impl Into<ecow::EcoString>,
    ) {
        self.push(Severity::Approximate, what.into(), detail.into());
    }

    pub fn drop(
        &mut self,
        what: impl Into<ecow::EcoString>,
        detail: impl Into<ecow::EcoString>,
    ) {
        self.push(Severity::Drop, what.into(), detail.into());
    }

    fn push(
        &mut self,
        severity: Severity,
        what: ecow::EcoString,
        detail: ecow::EcoString,
    ) {
        dedup_push(&mut self.entries, &mut self.seen, Entry { severity, what, detail });
    }
}

impl std::fmt::Display for ImportReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.entries {
            let tag = match entry.severity {
                Severity::Approximate => "approximated",
                Severity::Drop => "dropped",
            };
            writeln!(f, "- [{tag}] {}: {}", entry.what, entry.detail)?;
        }
        Ok(())
    }
}

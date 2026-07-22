//! What the import could not carry across, recorded rather than lost.
//!
//! Same two-severity shape as
//! [`typst_docx_import::ImportReport`](../../typst-docx-import/src/report.rs),
//! and for the same reason: a converter that silently drops a construct is
//! indistinguishable from one that never saw it, and the difference is the
//! whole value of the tool. **Approximate** means "mapped, detail lost";
//! **Drop** means "content did not come across".
//!
//! Entries are deduplicated by `(severity, what, detail)`, so a deck with two
//! hundred animated shapes reports animation once rather than two hundred
//! times.

use ecow::EcoString;
use rustc_hash::FxHashSet;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Severity {
    /// Mapped, but with a stated difference.
    Approximate,
    /// Not carried across at all.
    Drop,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Entry {
    pub severity: Severity,
    /// The construct, named the way a reader would name it ("animation",
    /// "SmartArt diagram").
    pub what: EcoString,
    /// What exactly was lost, and why.
    pub detail: EcoString,
}

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

    pub fn approximate(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.push(Severity::Approximate, what.into(), detail.into());
    }

    pub fn drop(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.push(Severity::Drop, what.into(), detail.into());
    }

    fn push(&mut self, severity: Severity, what: EcoString, detail: EcoString) {
        let entry = Entry { severity, what, detail };
        if self.seen.insert(entry.clone()) {
            self.entries.push(entry);
        }
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

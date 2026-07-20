//! Loss/uncertainty reporting — the mirror of `typst-docx`'s `FidelityReport`.
//! Every construct the importer dropped or approximated is recorded here, so
//! "usually good, sometimes literal" output is *auditable* rather than silent.

use std::collections::HashSet;

use ecow::EcoString;

/// A record of one construct the importer could not map cleanly.
#[derive(Debug, Clone)]
pub struct Note {
    pub severity: Severity,
    /// What construct (e.g. "OMML equation", "content control", "field").
    pub what: EcoString,
    /// How it was handled ("emitted as fallback text", "dropped", …).
    pub detail: EcoString,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    /// Mapped, but approximately (a visual/semantic detail was lost).
    Approximate,
    /// The content was dropped entirely.
    Drop,
}

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub notes: Vec<Note>,
    /// De-dup key (severity, what, detail) for everything already in
    /// `notes`. A document that repeats the same unmapped construct many
    /// times over — e.g. 200 `PAGE` fields, or 50 unresolved hyperlinks —
    /// must not produce one identical summary line per occurrence.
    seen: HashSet<(Severity, EcoString, EcoString)>,
}

impl ImportReport {
    pub fn approximate(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.push(Severity::Approximate, what.into(), detail.into());
    }

    pub fn drop(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.push(Severity::Drop, what.into(), detail.into());
    }

    fn push(&mut self, severity: Severity, what: EcoString, detail: EcoString) {
        if self.seen.insert((severity, what.clone(), detail.clone())) {
            self.notes.push(Note { severity, what, detail });
        }
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

//! Loss/uncertainty reporting — the mirror of `typst-docx`'s `FidelityReport`.
//! Every construct the importer dropped or approximated is recorded here, so
//! "usually good, sometimes literal" output is *auditable* rather than silent.

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

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum Severity {
    /// Mapped, but approximately (a visual/semantic detail was lost).
    Approximate,
    /// The content was dropped entirely.
    Drop,
}

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub notes: Vec<Note>,
}

impl ImportReport {
    pub fn approximate(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.notes.push(Note {
            severity: Severity::Approximate,
            what: what.into(),
            detail: detail.into(),
        });
    }

    pub fn drop(&mut self, what: impl Into<EcoString>, detail: impl Into<EcoString>) {
        self.notes.push(Note {
            severity: Severity::Drop,
            what: what.into(),
            detail: detail.into(),
        });
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

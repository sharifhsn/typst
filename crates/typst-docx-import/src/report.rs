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
    /// `(is_endnote, id)` pairs currently being lowered — the cycle guard for
    /// `RunContent::NoteRef` resolution (see [`crate::mappers::note`]). A
    /// malformed document can have note 1 reference note 1, directly or
    /// through a chain, which would otherwise recurse forever.
    ///
    /// This lives here, as opposed to a dedicated parameter threaded through
    /// `lower`/the mappers, because `report` is already carried `&mut`
    /// through every function on the note-lowering call path — including
    /// `lower_items` and `lower_run_items`, whose signatures the table/field
    /// mappers depend on staying exactly as they are. A `Vec` doubles as the
    /// depth counter (its length is the current nesting depth) and, at the
    /// sizes a note chain can reach, a linear `contains` scan to check for a
    /// repeat is cheap — no need for a `HashSet` here.
    note_stack: Vec<(bool, i64)>,
}

/// How deep a chain of notes referencing other notes may nest before
/// [`ImportReport::enter_note`] refuses to go further — the note-lowering
/// counterpart of `MAX_TABLE_DEPTH`/`MAX_SDT_DEPTH` in `wml::parse`. Real
/// documents essentially never reference a note from within another note at
/// all; this only bites a pathological or hostile document, and backs up the
/// cycle check for a chain long enough to still not repeat any single id.
const MAX_NOTE_DEPTH: usize = 8;

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

    /// Try to enter `(endnote, id)`'s body for lowering. Returns `false` —
    /// without recording anything itself, so the caller can report the
    /// specific construct ("footnote" vs "endnote") — if `id` is already on
    /// the stack (a direct or indirect cycle) or the stack is already at
    /// [`MAX_NOTE_DEPTH`]. Every successful `true` must be paired with a
    /// matching [`Self::exit_note`] once that note's body is fully lowered.
    pub(crate) fn enter_note(&mut self, endnote: bool, id: i64) -> bool {
        if self.note_stack.len() >= MAX_NOTE_DEPTH || self.note_stack.contains(&(endnote, id)) {
            return false;
        }
        self.note_stack.push((endnote, id));
        true
    }

    /// Leave the note most recently entered via [`Self::enter_note`].
    pub(crate) fn exit_note(&mut self) {
        self.note_stack.pop();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_notes_nest_and_unwind_cleanly() {
        let mut report = ImportReport::default();
        assert!(report.enter_note(false, 1));
        assert!(report.enter_note(false, 2));
        report.exit_note();
        report.exit_note();
        // Nothing left on the stack, so id 1 can be entered again.
        assert!(report.enter_note(false, 1));
    }

    #[test]
    fn a_note_cannot_re_enter_itself_while_still_on_the_stack() {
        let mut report = ImportReport::default();
        assert!(report.enter_note(false, 1));
        // Direct self-reference: note 1, still being lowered, refers to
        // itself again.
        assert!(!report.enter_note(false, 1));
    }

    #[test]
    fn an_indirect_cycle_through_another_note_is_also_refused() {
        let mut report = ImportReport::default();
        assert!(report.enter_note(false, 1));
        assert!(report.enter_note(false, 2));
        // Note 2 refers back to note 1, which is still on the stack.
        assert!(!report.enter_note(false, 1));
    }

    #[test]
    fn footnote_and_endnote_ids_are_tracked_independently() {
        let mut report = ImportReport::default();
        assert!(report.enter_note(false, 1));
        // An endnote with the same numeric id is a different note.
        assert!(report.enter_note(true, 1));
    }

    #[test]
    fn a_long_non_cycling_chain_is_still_capped_by_depth() {
        let mut report = ImportReport::default();
        for id in 0..100 {
            if !report.enter_note(false, id) {
                // Must give up well before 100 distinct, never-repeating ids.
                assert!(id < 100);
                return;
            }
        }
        panic!("expected the depth cap to stop this chain");
    }
}

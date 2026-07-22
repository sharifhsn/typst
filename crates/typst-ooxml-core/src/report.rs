//! The deduplicating two-severity loss-recording mechanism shared by the
//! OOXML importers.
//!
//! A converter that silently drops a construct is indistinguishable from one
//! that never saw it, and the difference is the whole value of the tool. Both
//! importers record every construct they could not carry across cleanly, at
//! one of two severities — **Approximate** ("mapped, detail lost") or **Drop**
//! ("content did not come across") — and deduplicate by `(severity, what,
//! detail)` so a document that repeats the same unmapped construct two hundred
//! times reports it once.
//!
//! This module owns only that mechanism: the severity, the entry shape, and
//! the dedup rule. How the entries are named, sorted, and presented is each
//! importer's own policy, kept in its own crate.

use ecow::EcoString;
use rustc_hash::FxHashSet;

/// How completely a construct came across.
///
/// Variant order is load-bearing: importers sort "most severe first" by the
/// derived `Ord`, so [`Drop`](Self::Drop) must outrank
/// [`Approximate`](Self::Approximate).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    /// Mapped, but with a stated difference (a visual/semantic detail lost).
    Approximate,
    /// Not carried across at all.
    Drop,
}

/// One recorded loss: a severity, the construct's name, and what became of it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Entry {
    pub severity: Severity,
    /// The construct, named the way a reader would ("animation", "field TOC").
    pub what: EcoString,
    /// What exactly was lost, and why.
    pub detail: EcoString,
}

/// The shared dedup rule: push `entry` onto `entries` iff `seen` did not
/// already hold it. `seen` is the caller's dedup set over the exact entries it
/// has kept, so two entries agreeing on `(severity, what, detail)` collapse to
/// one recorded line.
pub fn dedup_push(entries: &mut Vec<Entry>, seen: &mut FxHashSet<Entry>, entry: Entry) {
    if seen.insert(entry.clone()) {
        entries.push(entry);
    }
}

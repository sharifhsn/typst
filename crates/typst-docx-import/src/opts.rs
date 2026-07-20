//! Importer configuration.

/// How idiomatic the emitted Typst source should be.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum Tier {
    /// Fidelity-first: literal per-run formatting, no semantic inference beyond
    /// what Word explicitly names (heading styles, list membership). Robust and
    /// bounded.
    Literal,
    /// Idiomatic: run the tier-2 passes (collapse uniform formatting into
    /// `#set`, promote bold/italic to `#strong`/`#emph`, hoist a preamble).
    /// Higher-level output, but relies on heuristics.
    #[default]
    Idiomatic,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub tier: Tier,
    /// Directory (project-relative) under which extracted images are placed.
    pub assets_dir: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { tier: Tier::default(), assets_dir: "assets".into() }
    }
}

// Tracked changes are always *accepted*: insertions (`w:ins`/`w:moveTo`) are
// kept as content, deletions (`w:del`/`w:moveFrom`) are dropped. That is what
// Word renders by default and what the author last meant the document to say.
// There is deliberately no option for it — this used to be a
// `accept_tracked_changes` flag that nothing read, which is worse than no
// option at all, since it documented behaviour the importer didn't have.

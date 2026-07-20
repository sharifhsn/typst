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
    /// Accept `w:ins` (tracked insertions) as content and drop `w:del`
    /// deletions. When false, both are reported and left out.
    pub accept_tracked_changes: bool,
    /// Directory (project-relative) under which extracted images are placed.
    pub assets_dir: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            tier: Tier::default(),
            accept_tracked_changes: true,
            assets_dir: "assets".into(),
        }
    }
}

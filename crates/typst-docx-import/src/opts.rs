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

/// How a Word chart is brought across.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum ChartStyle {
    /// Lower a chart to the data table behind it. The emitted source stays
    /// self-contained — it needs nothing but Typst itself.
    #[default]
    Table,
    /// Draw the chart with the `lilaq` plotting package. The emitted source
    /// gains an `#import "@preview/lilaq:..."`, so it no longer compiles
    /// without that package available: opt in deliberately.
    Plot,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub tier: Tier,
    /// Directory (project-relative) under which extracted images are placed.
    pub assets_dir: String,
    /// How a Word chart is brought across — see [`ChartStyle`].
    pub charts: ChartStyle,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { tier: Tier::default(), assets_dir: "assets".into(), charts: ChartStyle::default() }
    }
}

// Tracked changes are always *accepted*: insertions (`w:ins`/`w:moveTo`) are
// kept as content, deletions (`w:del`/`w:moveFrom`) are dropped. That is what
// Word renders by default and what the author last meant the document to say.
// There is deliberately no option for it — this used to be a
// `accept_tracked_changes` flag that nothing read, which is worse than no
// option at all, since it documented behaviour the importer didn't have.

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

/// What happens to a document's tracked changes (`w:ins`/`w:del`, and the
/// `w:moveTo`/`w:moveFrom` pair Word writes for moved text).
///
/// Both modes **render identically** — an insertion's text is shown, a
/// deletion's is not, which is the "all changes accepted" view Word displays
/// by default and what the author last meant the document to say. They differ
/// only in whether the revision *record* survives alongside it.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum TrackedChanges {
    /// Keep the record: each revision also emits an invisible `#metadata`
    /// anchor carrying its author, timestamp and — for a deletion — the
    /// removed text, so nothing the document said is actually thrown away.
    ///
    /// The default, because it is a strict information superset of
    /// [`Self::Accept`] at no visual cost (a metadata-only paragraph renders
    /// pixel-identically to no paragraph at all), and because discarding
    /// authored content silently is the one thing this importer tries never
    /// to do.
    #[default]
    Preserve,
    /// Accept the changes and forget them: insertions become ordinary text,
    /// deletions vanish. Choose this when the emitted source matters more than
    /// the edit history — a heavily-revised document carries a lot of anchors.
    Accept,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub tier: Tier,
    /// Directory (project-relative) under which extracted images are placed.
    pub assets_dir: String,
    /// How a Word chart is brought across — see [`ChartStyle`].
    pub charts: ChartStyle,
    /// What happens to tracked changes — see [`TrackedChanges`].
    pub tracked: TrackedChanges,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            tier: Tier::default(),
            assets_dir: "assets".into(),
            charts: ChartStyle::default(),
            tracked: TrackedChanges::default(),
        }
    }
}

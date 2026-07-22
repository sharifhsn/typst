//! Import options.

/// How faithfully to reproduce a slide's geometry.
///
/// A `.docx` is a flow and a `.pptx` is a coordinate system, so unlike the
/// Word importer's tiers this is not a cosmetic choice — it decides whether
/// the output is a picture of the deck or an editable one.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum Fidelity {
    /// Every shape keeps its authored position and size, emitted as a `#place`
    /// inside a slide-sized canvas.
    ///
    /// This is the default because it is the only mode that **round-trips**:
    /// `typst-pptx` exports from laid-out frames, so a shape placed at its
    /// original EMU offset comes back out at that same offset. The source is
    /// faithful rather than pretty.
    #[default]
    Placed,
    /// Placeholder shapes — the ones PowerPoint itself labels `title`, `body`,
    /// `ctrTitle` — become ordinary touying headings and flow content, and
    /// only the free-form shapes keep their coordinates.
    ///
    /// Nicer to edit, and correct for the 36% of real decks that use
    /// placeholders; wrong whenever a designer positioned a "body" box
    /// somewhere the flow would not put it.
    Idiomatic,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub fidelity: Fidelity,
    /// Directory for extracted media, relative to the emitted `.typ`.
    pub assets_dir: String,
    /// The touying version to pin in the emitted `#import`.
    pub touying_version: String,
    /// The touying theme to apply.
    pub theme: String,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            fidelity: Fidelity::default(),
            assets_dir: "assets".into(),
            // Pinned, not floating: the emitted source must keep compiling
            // when a later touying changes its API, exactly as the Word
            // importer pins the package it emits for charts.
            touying_version: "0.7.4".into(),
            // `default` is touying's bare theme — it draws no furniture of its
            // own, which is what an importer wants: every mark on the slide
            // should come from the source deck, not from the theme.
            theme: "default".into(),
        }
    }
}

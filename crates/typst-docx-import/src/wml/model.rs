//! The **Word IR** — a faithful, lightly-typed parse of `word/document.xml`
//! and its companions (`styles.xml`, `numbering.xml`, relationships, media).
//! Deliberately a sibling of `typst-docx`'s DOM rather than that type itself:
//! it drops export-only concerns (review origins, field cache status, bookmark
//! id allocation) and keeps only what the importer needs to lower to Typst.
//!
//! Property structs store *raw* OOXML values (twips, half-points, hex colors,
//! style ids) — resolution against the style hierarchy happens in
//! [`crate::resolve`], and unit conversion happens in the mappers.

use ecow::EcoString;
use rustc_hash::FxHashMap;

/// Everything parsed out of the package that the importer consumes.
#[derive(Debug, Default)]
pub struct WmlPackage {
    pub body: Body,
    pub styles: Styles,
    pub numbering: Numbering,
    /// `rId` → relationship target (image part name, hyperlink URL, …).
    pub rels: FxHashMap<EcoString, Relationship>,
    /// Media parts by zip name (`word/media/image1.png` → bytes).
    pub media: FxHashMap<EcoString, Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct Relationship {
    pub target: EcoString,
    pub external: bool,
}

#[derive(Debug, Default)]
pub struct Body {
    pub items: Vec<BodyItem>,
    /// The final `w:sectPr` (body-level page geometry).
    pub sect_pr: Option<SectPr>,
}

#[derive(Debug)]
// A body is a `Vec<BodyItem>`; the paragraph/table size gap doesn't matter for
// a heap-allocated sequence, and boxing every item would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum BodyItem {
    Paragraph(Paragraph),
    Table(Table),
}

#[derive(Debug, Default)]
pub struct Paragraph {
    pub props: ParaProps,
    pub runs: Vec<RunItem>,
}

#[derive(Debug)]
pub enum RunItem {
    Run(Run),
    /// `w:hyperlink` — a link wrapping runs; either an external `rel_id`
    /// (into [`WmlPackage::rels`]) or an internal `anchor` (bookmark name).
    Hyperlink { rel_id: Option<EcoString>, anchor: Option<EcoString>, runs: Vec<Run> },
}

#[derive(Debug, Default)]
pub struct Run {
    pub props: RunProps,
    pub content: Vec<RunContent>,
}

#[derive(Debug)]
pub enum RunContent {
    Text(EcoString),
    Tab,
    Break(BreakType),
    /// A `w:drawing` inline/anchored image → the `rId` of its blip.
    Drawing(DrawingRef),
    /// OMML math (`m:oMath`) captured as a raw XML fragment.
    Math(EcoString),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BreakType {
    Line,
    Page,
    Column,
}

#[derive(Debug, Clone)]
pub struct DrawingRef {
    pub rel_id: EcoString,
    /// Extent in EMU, if present (`wp:extent`).
    pub cx_emu: Option<i64>,
    pub cy_emu: Option<i64>,
    pub alt: Option<EcoString>,
}

// --- Paragraph properties (`w:pPr`) -----------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ParaProps {
    /// `w:pStyle` — the paragraph style id.
    pub style_id: Option<EcoString>,
    /// `w:jc`.
    pub jc: Option<EcoString>,
    /// `w:numPr` → (numId, ilvl).
    pub num: Option<NumRef>,
    /// `w:spacing/@w:before` in twips.
    pub spacing_before: Option<i64>,
    /// `w:spacing/@w:line` in twips.
    pub line: Option<i64>,
    /// `w:ind/@w:left` (or `@w:start`) in twips.
    pub indent_left: Option<i64>,
    /// Run properties on the paragraph mark (`w:pPr/w:rPr`) — the default for
    /// bare runs and empty paragraphs.
    pub mark_props: RunProps,
    /// Whether a `w:pBdr/w:bottom` (a rule-like bottom border) is present.
    pub bottom_border: bool,
}

#[derive(Debug, Copy, Clone)]
pub struct NumRef {
    pub num_id: i64,
    pub ilvl: i64,
}

// --- Run properties (`w:rPr`) -----------------------------------------------

/// Tri-state OOXML toggle: absent, or explicitly on/off (`w:val="0"`).
pub type Toggle = Option<bool>;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RunProps {
    /// `w:rStyle` — the character style id.
    pub style_id: Option<EcoString>,
    pub bold: Toggle,
    pub italic: Toggle,
    pub strike: Toggle,
    pub smallcaps: Toggle,
    /// `w:u/@w:val` (e.g. "single", "none").
    pub underline: Option<EcoString>,
    /// `w:color/@w:val` — hex `RRGGBB` (or "auto").
    pub color: Option<EcoString>,
    /// `w:sz/@w:val` in half-points.
    pub size_half_pt: Option<i64>,
    /// `w:rFonts/@w:ascii`.
    pub font: Option<EcoString>,
    /// `w:vertAlign/@w:val` ("superscript"/"subscript").
    pub vert_align: Option<EcoString>,
    /// `w:vanish` — hidden text.
    pub vanish: Toggle,
}

// --- Tables (`w:tbl`) -------------------------------------------------------

#[derive(Debug, Default)]
pub struct Table {
    /// `w:tblGrid` column widths in twips.
    pub grid: Vec<i64>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Default)]
pub struct Row {
    pub is_header: bool,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Default)]
pub struct Cell {
    /// `w:gridSpan`.
    pub grid_span: usize,
    /// `w:vMerge`: `Some(true)` = restart, `Some(false)` = continue.
    pub v_merge: Option<bool>,
    /// `w:shd/@w:fill` hex.
    pub shd_fill: Option<EcoString>,
    pub content: Vec<BodyItem>,
}

// --- Sections (`w:sectPr`) --------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct SectPr {
    /// `w:pgSz` width/height in twips.
    pub page_w: Option<i64>,
    pub page_h: Option<i64>,
    pub landscape: bool,
    /// `w:pgMar` in twips.
    pub margin_top: Option<i64>,
    pub margin_bottom: Option<i64>,
    pub margin_left: Option<i64>,
    pub margin_right: Option<i64>,
}

// --- Styles (`styles.xml`) --------------------------------------------------

#[derive(Debug, Default)]
pub struct Styles {
    /// docDefaults run/paragraph properties.
    pub default_run: RunProps,
    pub default_para: ParaProps,
    /// Style id → definition.
    pub by_id: FxHashMap<EcoString, Style>,
}

#[derive(Debug, Default, Clone)]
pub struct Style {
    pub id: EcoString,
    pub name: Option<EcoString>,
    pub kind: StyleKind,
    pub based_on: Option<EcoString>,
    /// The heading outline level (`w:pPr/w:outlineLvl`, 0-based), if any.
    pub outline_level: Option<u8>,
    pub run: RunProps,
    pub para: ParaProps,
}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum StyleKind {
    #[default]
    Paragraph,
    Character,
    Table,
    Numbering,
}

// --- Numbering (`numbering.xml`) --------------------------------------------

#[derive(Debug, Default)]
pub struct Numbering {
    /// `numId` → `abstractNumId`.
    pub instances: FxHashMap<i64, i64>,
    /// `abstractNumId` → per-level format.
    pub abstract_nums: FxHashMap<i64, FxHashMap<i64, LevelFormat>>,
}

#[derive(Debug, Clone)]
pub struct LevelFormat {
    /// `w:numFmt/@w:val` ("bullet", "decimal", …).
    pub num_fmt: EcoString,
}

impl Numbering {
    /// Whether a `(numId, ilvl)` reference is an ordered (numbered) list.
    pub fn is_ordered(&self, num_id: i64, ilvl: i64) -> bool {
        self.instances
            .get(&num_id)
            .and_then(|abs| self.abstract_nums.get(abs))
            .and_then(|levels| levels.get(&ilvl))
            .is_some_and(|fmt| fmt.num_fmt != "bullet" && fmt.num_fmt != "none")
    }
}

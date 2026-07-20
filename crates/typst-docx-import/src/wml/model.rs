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
    /// Header/footer parts number their own `rId`s independently of
    /// `document.xml` (see [`Self::furniture`]), so an id resolved from a
    /// furniture body is namespaced `"{part}!{rid}"` rather than bare —
    /// [`crate::wml::parse`] rewrites those ids as it parses each part, so
    /// every lookup against this map (bare or namespaced) just works.
    pub rels: FxHashMap<EcoString, Relationship>,
    /// Media parts by zip name (`word/media/image1.png` → bytes).
    pub media: FxHashMap<EcoString, Vec<u8>>,
    /// Parsed `w:hdr`/`w:ftr` parts by zip name (`word/header1.xml` → body).
    /// Headers and footers share one map: they are structurally identical and
    /// the `sectPr` reference is what gives a part's role. A flat item list
    /// rather than a [`Body`] — a header/footer part can never carry its own
    /// `w:sectPr`, so there is no section boundary to model.
    pub furniture: FxHashMap<EcoString, Vec<BodyItem>>,
    /// `settings.xml` declares `w:evenAndOddHeaders`.
    pub even_and_odd_headers: bool,
    /// `word/footnotes.xml` bodies by `w:id`, boilerplate separators excluded.
    /// Flat, same reasoning as [`Self::furniture`].
    pub footnotes: FxHashMap<i64, Vec<BodyItem>>,
    /// `word/endnotes.xml`, likewise.
    pub endnotes: FxHashMap<i64, Vec<BodyItem>>,
    /// Parsed chart parts by zip name (`word/charts/chart1.xml` → data). A
    /// chart's `r:id` reference (see [`RunContent::Chart`]) resolves through
    /// [`Self::rels`] to a target *name*; this map is keyed by the full zip
    /// name that target resolves to, the same convention [`Self::media`]
    /// uses for images.
    pub charts: FxHashMap<EcoString, ChartData>,
}

#[derive(Debug, Clone)]
pub struct Relationship {
    pub target: EcoString,
    pub external: bool,
}

/// `word/document.xml`'s `w:body`: an ordered list of sections, always at
/// least one. A `w:sectPr` inside a paragraph's `w:pPr` marks the *end* of a
/// section — that paragraph is the section's last item, and the `sectPr`
/// describes the section just finished; the body-level `w:sectPr` (a direct
/// child of `w:body`, always last) describes the final section. A document
/// with no `w:sectPr` at all — neither on a paragraph nor at the body's end —
/// is one section with default properties (see `wml::parse::parse_document_body`).
///
/// Only `word/document.xml`'s own body is shaped this way: a header/footer
/// part, a footnote/endnote body, and a text box's content are all the same
/// paragraph/table content but can never carry a section boundary of their
/// own, so they stay a plain `Vec<BodyItem>` (see [`WmlPackage::furniture`]).
#[derive(Debug, Default)]
pub struct Body {
    pub sections: Vec<Section>,
}

/// One Word section: the content it governs, and the page setup that applies
/// to it (`w:sectPr`).
#[derive(Debug, Default)]
pub struct Section {
    pub items: Vec<BodyItem>,
    pub props: SectPr,
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
    /// `w:hyperlink` — a link wrapping inline content; either an external
    /// `rel_id` (into [`WmlPackage::rels`]) or an internal `anchor` (bookmark
    /// name). `runs` holds `RunItem` (not `Run`) so a field can appear inside
    /// a hyperlink's content — common for cross-references and TOC entries,
    /// where the hyperlink supplies the jump target and a nested
    /// PAGEREF/REF field supplies the displayed page number.
    Hyperlink { rel_id: Option<EcoString>, anchor: Option<EcoString>, runs: Vec<RunItem> },
    /// A Word field. Both OOXML spellings — the `w:fldSimple` element and the
    /// flattened `w:fldChar` begin/separate/end run sequence — are folded
    /// back into this one logical item at parse time, so lowering sees a
    /// field as a field rather than as loose punctuation runs. See
    /// [`crate::mappers::field`] for how `instr` is interpreted.
    Field(Field),
}

/// A Word field: `w:fldSimple`, or the flattened `w:fldChar`
/// begin/separate/end run sequence, folded back into one logical item by
/// [`crate::wml::parse`].
#[derive(Debug, Default)]
pub struct Field {
    /// The raw instruction, e.g. ` PAGE ` or ` HYPERLINK "https://x" \o "t" `.
    pub instr: EcoString,
    /// The cached result — what Word last rendered for this field. Fields can
    /// nest (e.g. a TOC entry whose result contains a HYPERLINK field), so
    /// this is `RunItem`s rather than plain runs.
    pub result: Vec<RunItem>,
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
    /// `w:ruby` — a phonetic guide (furigana): `gloss` is the small reading
    /// set above `base`. Both halves hold ordinary runs, and `w:ruby` sits
    /// *inside* a `w:r`, which is why it is run content rather than a
    /// [`RunItem`] beside one.
    Ruby { base: Vec<RunItem>, gloss: Vec<RunItem> },
    /// A `w:footnoteReference`/`w:endnoteReference` — the marker in the body
    /// text. The note's content lives in a separate part, keyed by this id.
    NoteRef { endnote: bool, id: i64 },
    /// A shape's text (`w:txbxContent`) — the body content of a DrawingML
    /// text box (`wps:txbx`) or its VML equivalent (`v:textbox`). Word floats
    /// these; we keep the content and lose the geometry.
    TextBox(Vec<BodyItem>),
    /// A charted `w:drawing`. Reuses [`DrawingRef`] because a chart *is* a
    /// drawing: it carries the same `rId` and `wp:extent`, and Word's extent
    /// is the chart's authored size, which the plot renderer needs.
    /// The `rId` here points at its `c:chart`/`cx:chart` part.
    /// Typst has no chart-drawing primitive, but the chart's cached data
    /// lives in that separate part (resolved against
    /// [`WmlPackage::charts`]), not inline here, so lowering it to a table
    /// keeps the information instead of dropping it — see [`ChartData`].
    Chart(DrawingRef),
    /// WordArt (`v:textpath`'s `string` attribute) — genuine document text
    /// with no Typst equivalent for the curved/warped path it's drawn along,
    /// so it's kept as plain text and the styling loss is reported once at
    /// lower time (see `mappers::run`). A VML *picture* (`v:imagedata`)
    /// needs no variant of its own here — it resolves through the same
    /// relationship-id lookup as a DrawingML picture, so it's folded
    /// straight into [`Self::Drawing`] at parse time (see `wml::parse`'s
    /// `vml_shape_content`).
    VmlText(EcoString),
    /// A VML `v:rect`/`v:oval`/`v:roundrect`/`v:line` — a shape Typst *can*
    /// draw natively, unlike the custom-geometry case [`Self::VmlUnsupported`]
    /// covers. See `mappers::shape` for the width/height/color parsing and
    /// the `#rect`/`#circle`/`#ellipse`/`#line` mapping.
    VmlShape(VmlShape),
    /// A VML `v:shape` with nothing this importer can extract: no picture,
    /// no text box, no WordArt text — typically one with custom
    /// `v:path`/`v:formulas` geometry (a callout, a star, …), which would
    /// need a drawing package to render faithfully. Recorded as a drop at
    /// lower time rather than silently vanishing (see `wml::parse`'s
    /// `vml_shape_content` for why a shape holding *only* a plain text box
    /// never reaches this variant).
    VmlUnsupported,
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
    /// Extent in EMU, if present (`wp:extent`, or — for a VML picture — its
    /// `style` attribute's `width`/`height`, converted once at parse time so
    /// this field keeps a single unit contract regardless of which markup
    /// produced it).
    pub cx_emu: Option<i64>,
    pub cy_emu: Option<i64>,
    pub alt: Option<EcoString>,
}

// --- VML shapes (`v:rect`/`v:oval`/`v:roundrect`/`v:line`, in a `w:pict`) ----

/// A VML `v:rect`/`v:oval`/`v:roundrect`/`v:line`, parsed raw: `style` is
/// kept as the whole CSS-ish attribute string (`crate::wml::parse::
/// vml_length_pt` pulls `width`/`height` out of it), and the fill/stroke
/// colors are kept as whatever VML wrote (`#rrggbb` or a color name) —
/// `crate::mappers::shape` resolves both, mirroring how [`RunProps::color`]
/// stores a raw hex string for `crate::lower::parse_hex_color` to resolve
/// later rather than doing it here.
#[derive(Debug, Clone)]
pub struct VmlShape {
    pub kind: VmlShapeKind,
    /// `style="width:120pt;height:80pt;..."`, verbatim.
    pub style: EcoString,
    /// `fillcolor`, or a `v:fill` child's `color` — VML's two spellings for
    /// the same thing.
    pub fill_color: Option<EcoString>,
    /// `filled="f"` → `false`; VML's own default (the attribute absent, or
    /// any value other than `"f"`) is `true`.
    pub filled: bool,
    /// `strokecolor`, or a `v:stroke` child's `color`.
    pub stroke_color: Option<EcoString>,
    /// `stroked="f"` → `false`; default `true`, mirroring `filled`.
    pub stroked: bool,
    /// [`VmlShapeKind::Line`]'s endpoints, each a raw `"x,y"` pair — absent
    /// for every other kind.
    pub from: Option<EcoString>,
    pub to: Option<EcoString>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum VmlShapeKind {
    Rect,
    Oval,
    RoundRect,
    Line,
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
    /// `w:pPr/w:sectPr` — present only when this paragraph is the *last* item
    /// of a section (see [`Section`]); describes the section it closes. Direct,
    /// per-instance data: a style's own `pPr` is never a document section
    /// boundary, so this is never resolved through the `basedOn`/style chain
    /// (see `resolve::styles::merge_para`) — only [`crate::wml::parse::
    /// parse_document_body`] ever reads it, straight off the raw paragraph.
    pub sect_pr: Option<SectPr>,
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

// --- Charts (`word/charts/*.xml`) -------------------------------------------

/// A chart's cached data — the numbers and labels Word last plotted. Enough
/// to rebuild the chart as a table, which is what the importer does (Typst
/// has no chart-drawing primitive, and inventing one is out of scope).
///
/// This shape is classic-chart-first (`c:chartSpace`'s `c:ser`/`c:cat`/
/// `c:val`, one value per category per series): a plain category axis shared
/// by every series, each series a column. The newer ChartEx format
/// (`cx:chartSpace`, `word/charts/chartEx*.xml`, used for chart types
/// introduced after Office 2013 — box-and-whisker, sunburst, waterfall, …)
/// stores its data differently — a flat `cx:data` block per series-ish
/// grouping, with categories and values aligned by shared point index rather
/// than nested inside the series itself — but maps onto the same
/// `categories`/`series` shape well enough for the chart types this importer
/// has actually seen in the wild (a box-and-whisker chart's raw, unaggregated
/// data table *is* one row per point with a repeated category label, which
/// is exactly what this struct already represents). A chart type whose
/// category axis is genuinely hierarchical (e.g. a sunburst's nested
/// leaf/stem/branch levels) only keeps its finest (first) level here — the
/// coarser levels are a real, but comparatively minor, loss on top of the
/// larger one (the plot itself) this whole construct already accepts.
#[derive(Debug, Default, Clone)]
pub struct ChartData {
    pub title: Option<EcoString>,
    /// Category labels (the shared x-axis), if the chart declares any.
    pub categories: Vec<EcoString>,
    pub series: Vec<ChartSeries>,
    /// What kind of chart this is — see [`ChartKind`]. Drives whether
    /// [`crate::mappers::chart`] can draw it as a plot under
    /// [`crate::opts::ChartStyle::Plot`]; irrelevant to the table fallback,
    /// which works for any kind.
    pub kind: ChartKind,
    /// `c:legend/c:legendPos` — where Word placed the legend, or `None` when
    /// the chart declares no legend at all (in which case it shows none).
    pub legend: Option<LegendPos>,
}

/// A chart's plot type, as far as it maps onto something Typst's `lilaq`
/// package can draw. Only the classic-chart shape ([`ChartData`]'s doc
/// comment) carries a kind other than [`Self::Other`] — ChartEx charts
/// (box-and-whisker, sunburst, waterfall, …) have no `lilaq` counterpart
/// either, so they stay `Other` rather than being guessed at.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum ChartKind {
    Bar,
    Line,
    Scatter,
    Area,
    /// A type with no plotting counterpart (pie, radar, stock, surface, …) —
    /// these always fall back to the data table.
    #[default]
    Other,
}

/// `c:legendPos` — the edge Word put the chart legend on.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum LegendPos {
    Top,
    Bottom,
    Left,
    Right,
    TopRight,
}

#[derive(Debug, Default, Clone)]
pub struct ChartSeries {
    pub name: Option<EcoString>,
    /// Values, positionally aligned with `categories` where both exist.
    pub values: Vec<EcoString>,
}

// --- Sections (`w:sectPr`) --------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct SectPr {
    /// `w:pgSz` width/height in twips.
    pub page_w: Option<i64>,
    pub page_h: Option<i64>,
    pub landscape: bool,
    /// `w:cols/@w:num` — the section's text-column count. A two-column
    /// call-for-papers layout is a hard requirement of the template that
    /// imposes it, not a cosmetic detail.
    pub columns: Option<u32>,
    /// `w:pgMar` in twips.
    pub margin_top: Option<i64>,
    pub margin_bottom: Option<i64>,
    pub margin_left: Option<i64>,
    pub margin_right: Option<i64>,
    /// `w:headerReference` / `w:footerReference`, in document order.
    pub header_refs: Vec<FurnitureRef>,
    pub footer_refs: Vec<FurnitureRef>,
    /// `w:titlePg` — the first page takes its own header/footer.
    pub title_pg: bool,
    /// `w:type` — how this section starts. Absent means `nextPage` (Word's own
    /// default when the element is missing).
    pub start: SectionStart,
    /// `w:pgNumType/@w:fmt` — the page-number format, if the section sets one.
    /// Absent means "inherit whatever the previous section had" (Word never
    /// resets the format just because a section doesn't restate it).
    pub page_num_fmt: Option<EcoString>,
    /// `w:pgNumType/@w:start` — the number this section restarts at. Absent
    /// means no restart here (the counter just keeps incrementing).
    pub page_num_start: Option<i64>,
}

/// `w:sectPr/w:type/@w:val` — how a section starts relative to the one before
/// it. Mirrored (not duplicated) in the Typst IR as `tdoc::SectionStart`
/// (re-exported from here) since both sides mean exactly the same thing.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum SectionStart {
    /// Starts on a new page. Word's own default when `w:type` is absent.
    #[default]
    NextPage,
    /// Stays on the *same* page as the section before it — a column-count or
    /// page-numbering change with no visible page break. The one start type
    /// Typst can sometimes honor without a `#pagebreak()` at all (see
    /// `emit::render_section`).
    Continuous,
    /// Starts on the next even-numbered page.
    EvenPage,
    /// Starts on the next odd-numbered page.
    OddPage,
    /// Starts in the next column of a multi-column layout. Real documents use
    /// this vanishingly rarely; Typst has no "next column, possibly also next
    /// page" primitive, so it's treated the same as `NextPage`.
    NextColumn,
}

/// A `w:headerReference`/`w:footerReference`: which page class it applies to,
/// and the relationship pointing at the `w:hdr`/`w:ftr` part.
#[derive(Debug, Clone)]
pub struct FurnitureRef {
    pub kind: FurnitureKind,
    pub rel_id: EcoString,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FurnitureKind {
    Default,
    First,
    Even,
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

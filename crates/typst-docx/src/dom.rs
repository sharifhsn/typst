//! The typed DOCX intermediate representation.

use std::sync::Arc;

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Output, StyleChain, Target};
use typst_library::introspection::{Introspector, Tag};
use typst_library::model::{Document, DocumentInfo};

use crate::introspect::DocxIntrospector;
use crate::package::Rels;

/// Output document: realized native tree lowered to the OOXML IR + metadata +
/// introspector.
pub struct DocxDocument {
    pub(crate) info: DocumentInfo,
    pub(crate) body: Vec<Block>,
    pub(crate) sect: SectPr,
    pub(crate) footnotes: Vec<Footnote>,
    pub(crate) numbering: NumberingTable,
    pub(crate) media: Vec<MediaPart>,
    pub(crate) doc_rels: Rels,
    /// Relationships created while lowering footnote bodies — they belong in
    /// `word/_rels/footnotes.xml.rels`, not the document's, or Word rejects the
    /// file. Empty when no footnote contains an image/external link.
    pub(crate) footnote_rels: Rels,
    pub(crate) bookmarks: BookmarkTable,
    pub(crate) max_heading_level: u8,
    pub(crate) uses_fields: bool,
    pub(crate) uses_math: bool,
    pub(crate) introspector: Arc<DocxIntrospector>,
    /// Header parts (`word/headerN.xml`) referenced by the section(s).
    pub(crate) header_parts: Vec<HdrFtrPart>,
    /// Footer parts (`word/footerN.xml`) referenced by the section(s).
    pub(crate) footer_parts: Vec<HdrFtrPart>,
}

impl DocxDocument {
    pub fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Document for DocxDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Output for DocxDocument {
    fn introspector(&self) -> &dyn Introspector {
        self.introspector.as_ref()
    }

    fn target() -> Target {
        Target::Docx
    }

    fn create(
        engine: &mut Engine,
        content: &Content,
        styles: StyleChain,
    ) -> SourceResult<Self> {
        crate::docx_document(engine, content, styles)
    }
}

/// A block-level body item.
pub enum Block {
    Para(Para),
    Table(Tbl),
    /// A table of contents: a `TOC` complex field whose cached result is a set
    /// of baked entry paragraphs (so it shows without a manual field update).
    Toc(Toc),
    /// A non-final section break carrying its own `SectPr`.
    SectionBreak(SectPr),
    /// Introspection tag passthrough for the introspector + bookmarks.
    Tag(Tag),
}

/// A table-of-contents complex field. The field's `begin`/`instrText`/`separate`
/// wrap the first `entry` and its `end` closes the last, so the baked entries
/// render as the field's cached result. When `entries` is empty the `fallback`
/// runs are shown inside a single-paragraph field instead.
pub struct Toc {
    pub instr: EcoString,
    pub dirty: bool,
    /// Heading depth (`\o "1-N"`) to populate from after the body is converted.
    /// `Some` marks a heading table of contents.
    pub depth: Option<usize>,
    /// Caption category (`\c "Figure"` / `"Table"` / …) to populate from. `Some`
    /// marks a list of figures/tables.
    pub caption_category: Option<EcoString>,
    /// Right tab position (twips) for the dot leader + page number.
    pub tab_pos: i32,
    /// Baked entries, filled in a post-conversion pass from the headings/figures
    /// that were actually emitted (so the bookmarks they target always exist).
    pub entries: Vec<Para>,
    pub fallback: Vec<Run>,
}

/// A heading recorded during conversion, used to populate the table of contents
/// once every heading's real bookmark is known.
pub struct TocHeading {
    pub level: usize,
    /// The heading's bookmark name, when it emitted one (else a plain entry).
    pub anchor: Option<EcoString>,
    pub text: EcoString,
}

/// A captioned figure/table recorded during conversion, used to populate a list
/// of figures/tables once every figure's real bookmark is known.
pub struct TocFigure {
    /// The caption category (`Figure`/`Table`/…), matched against a `\c` switch.
    pub category: EcoString,
    /// The figure's bookmark name, when it emitted one (else a plain entry).
    pub anchor: Option<EcoString>,
    pub text: EcoString,
}

/// A paragraph.
pub struct Para {
    pub props: ParaProps,
    pub content: Vec<ParaChild>,
}

/// Paragraph-level content.
pub enum ParaChild {
    Run(Run),
    /// A display equation `<m:oMathPara>` (serialized XML).
    OmmlPara(String),
    /// `<w:hyperlink r:id|w:anchor>` wrapping runs.
    Hyperlink { rel: Option<EcoString>, anchor: Option<EcoString>, runs: Vec<Run> },
    BookmarkStart { id: u32, name: EcoString },
    BookmarkEnd { id: u32 },
    Tag(Tag),
}

/// A run-level item.
pub enum Run {
    Text { props: RunProps, text: EcoString },
    Break,
    PageBreak,
    Tab,
    /// A tab from a fractional `#h(1fr)` (the push-apart idiom). Encoded as a
    /// tab, but its paragraph gains a right-aligned tab stop at the content width
    /// so it pushes the following content to the right margin.
    FillTab,
    FootnoteRef { props: RunProps, id: i32 },
    /// The in-body footnote number mark (`<w:footnoteRef/>`, styled
    /// `FootnoteReference`). Prepended to a footnote body's first paragraph so
    /// Word/LibreOffice render the footnote's auto-number next to its text.
    FootnoteRefMark,
    Drawing(Drawing),
    /// An inline equation `<m:oMath>` (serialized XML).
    OmmlInline(String),
    Field(Field),
}

/// Flattened character formatting → `<w:rPr>`.
#[derive(Default, Clone, PartialEq)]
pub struct RunProps {
    pub style: Option<EcoString>,
    pub font: Option<EcoString>,
    pub bold: bool,
    pub italic: bool,
    pub smallcaps: bool,
    pub strike: bool,
    pub color: Option<[u8; 3]>,
    /// Character spacing / tracking in signed twips (`<w:spacing w:val=…>` in
    /// `rPr`). `text(tracking:)`. Default none.
    pub tracking: Option<i32>,
    /// Baseline shift in signed half-points (`<w:position>`). Positive = raised.
    /// `text(baseline:)` (downward-positive) is negated. Default none.
    pub position_half_pt: Option<i32>,
    pub size_half_pt: Option<u32>,
    pub shd_fill: Option<[u8; 3]>,
    /// `<w:bdr>` run border (a character border box). Renders an *inline* framed
    /// container (`#box(stroke:)[..]` mid-line) as boxed text that flows in the
    /// line — Word does not flow an inline text box's content. Default none.
    pub bdr: Option<ParaBorder>,
    /// `<w:u>` underline, when present. `text(underline:)` / `#underline`. The
    /// style (`w:val`) and colour are derived from the line's stroke (dash
    /// pattern → dotted/dash/dotDash, paint → `w:color`); a plain underline is
    /// `single` with no colour (byte-identical to the original `bool`).
    pub underline: Option<Underline>,
    /// `<w:vanish/>` — hidden text (`#hide`). Default false.
    pub vanish: bool,
    pub vert_align: Option<VertAlign>,
    /// `<w:rtl/>` (run reading order is RTL). `text(dir: rtl)`. Default false.
    pub rtl: bool,
    /// `<w:cs/>` (use complex-script formatting for this run). Pairs with `rtl`.
    /// Default false.
    pub cs: bool,
    pub lang: Option<EcoString>,
}

#[derive(Copy, Clone, PartialEq)]
pub enum VertAlign {
    Super,
    Sub,
}

/// A `<w:u>` underline: a Word line style plus an optional explicit colour.
#[derive(Clone, PartialEq)]
pub struct Underline {
    /// `w:val`: "single" | "double" | "thick" | "dotted" | "dash" | "dotDash".
    pub val: &'static str,
    /// `w:color` (`RRGGBB`), when the line carries a paint of its own; `None`
    /// makes the underline follow the run's text colour ("auto").
    pub color: Option<[u8; 3]>,
}

impl Underline {
    /// A plain single underline with no colour of its own.
    pub fn single() -> Self {
        Self { val: "single", color: None }
    }
}

/// Paragraph formatting → `<w:pPr>`.
#[derive(Default, Clone, PartialEq)]
pub struct ParaProps {
    pub style: Option<EcoString>,
    pub keep_next: bool,
    /// `<w:keepLines/>` (keep all lines on one page). Default false.
    pub keep_lines: bool,
    pub num: Option<(u32, u8)>,
    /// `<w:bidi/>` (paragraph base reading order is RTL). Default false.
    pub bidi: bool,
    pub spacing: Option<Spacing>,
    pub ind: Option<Indent>,
    /// `<w:contextualSpacing/>` (suppress before/after between like paragraphs).
    /// Default false.
    pub contextual_spacing: bool,
    pub jc: Option<Jc>,
    pub outline_lvl: Option<u8>,
    pub tabs: Vec<TabStop>,
    /// `<w:shd w:fill=…>` paragraph shading (`block(fill:)`). Default none.
    pub shd_fill: Option<[u8; 3]>,
    /// `<w:pBdr>` paragraph borders (`block(stroke:)`). Default none.
    pub pbdr: Option<ParaBorders>,
}

/// The four sides of a paragraph border (`<w:pBdr>`).
#[derive(Default, Clone, PartialEq)]
pub struct ParaBorders {
    pub top: Option<ParaBorder>,
    pub left: Option<ParaBorder>,
    pub bottom: Option<ParaBorder>,
    pub right: Option<ParaBorder>,
}

impl ParaBorders {
    /// Whether all four sides are absent.
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.left.is_none()
            && self.bottom.is_none()
            && self.right.is_none()
    }
}

/// One side of a paragraph border (`CT_Border`).
#[derive(Copy, Clone, PartialEq)]
pub struct ParaBorder {
    /// `w:val`: "single" | "dashed" | "dotted".
    pub style: &'static str,
    /// `w:sz` in eighths of a point.
    pub sz: u32,
    /// `w:space` in points (0..=31).
    pub space: u32,
    pub color: [u8; 3],
}

#[derive(Copy, Clone, PartialEq)]
pub enum Jc {
    Start,
    End,
    Center,
    Both,
}

#[derive(Default, Clone, PartialEq)]
pub struct Spacing {
    pub before: Option<i32>,
    pub after: Option<i32>,
    pub line: Option<i32>,
    /// `w:lineRule="auto"` (line value is 240ths of a line). Mutually exclusive
    /// with `line_rule_at_least`; both false → `"exact"`.
    pub line_rule_auto: bool,
    /// `w:lineRule="atLeast"` (line value is a twip minimum). Default false.
    pub line_rule_at_least: bool,
}

#[derive(Default, Clone, PartialEq)]
pub struct Indent {
    pub left: Option<i32>,
    pub right: Option<i32>,
    pub first_line: Option<i32>,
    pub hanging: Option<i32>,
}

#[derive(Clone, PartialEq)]
pub struct TabStop {
    pub val: TabAlign,
    pub leader: Option<TabLeader>,
    pub pos: i32,
}

#[derive(Copy, Clone, PartialEq)]
pub enum TabAlign {
    Start,
    End,
    Center,
}

#[derive(Copy, Clone, PartialEq)]
pub enum TabLeader {
    Dot,
    Hyphen,
    Underscore,
}

/// A complex field code run sequence.
pub struct Field {
    pub instr: EcoString,
    pub result: Vec<Run>,
    pub dirty: bool,
}

/// An image. Inline (`anchor: None`) or floating (`anchor: Some`).
pub struct Drawing {
    pub rel: EcoString,
    pub w_emu: i64,
    pub h_emu: i64,
    pub alt: Option<EcoString>,
    pub docpr_id: u32,
    pub name: EcoString,
    /// `None` = inline (`<wp:inline>`); `Some` = floating (`<wp:anchor>`).
    /// Defaults to `None` so every existing inline image is byte-identical.
    pub anchor: Option<Anchor>,
    /// `None` = a raster picture (`pic:pic`, uses `rel`); `Some` = a vector
    /// DrawingML shape (`wps:wsp`, ignores `rel`).
    pub shape: Option<ShapeSpec>,
}

/// A vector DrawingML shape (a `#rect`/`#circle`/`#polygon`/… mapped to a Word
/// `wps:wsp` instead of a rasterized image).
pub struct ShapeSpec {
    pub geom: ShapeGeom,
    /// Solid fill colour, or `None` for no fill.
    pub fill: Option<[u8; 3]>,
    pub stroke: Option<ShapeStroke>,
    /// Real editable text framed by the shape (`wps:txbx`). `Some` turns the
    /// shape into a Word *text box* (a `#box(fill|stroke)[text]`); `None` is a
    /// bare decorative shape. Default `None`.
    pub txbx: Option<TextBox>,
}

/// The text-box content of a shape (`wps:txbx` → `w:txbxContent`): real
/// paragraphs the consumer can edit, with the box's inset reproduced as the
/// text-frame insets `[left, top, right, bottom]` in EMU.
pub struct TextBox {
    pub ins: [i64; 4],
    pub blocks: Vec<Block>,
}

/// A shape's geometry. Coordinates for [`ShapeGeom::Path`] are in EMU within the
/// shape's bounding box.
pub enum ShapeGeom {
    Rect,
    RoundRect,
    Ellipse,
    /// A path: the points, and whether it is closed (a polygon) or open (a line).
    Path { points: Vec<(i64, i64)>, closed: bool },
}

pub struct ShapeStroke {
    pub color: [u8; 3],
    pub w_emu: i64,
}

/// Floating-image placement (`<wp:anchor>`): positionH/V + wrap.
pub struct Anchor {
    /// `relativeHeight` z-order (monotonic per drawing).
    pub z: u32,
    /// Horizontal position: `relativeFrom` + (align XOR offset).
    pub pos_h: AnchorPos,
    /// Vertical position: `relativeFrom` + (align XOR offset).
    pub pos_v: AnchorPos,
    pub wrap: AnchorWrap,
    /// `distT`/`distB`/`distL`/`distR` in EMU.
    pub dist: [i64; 4],
}

/// One axis of an anchor position (`<wp:positionH>` / `<wp:positionV>`).
pub struct AnchorPos {
    /// `relativeFrom`, e.g. "margin" | "page" (axis-specific; caller picks a
    /// valid value for the axis).
    pub rel_from: &'static str,
    /// `<wp:align>` value ("left|center|right" for H, "top|bottom|center" for
    /// V). Exactly one of `align` / `offset` is `Some`.
    pub align: Option<&'static str>,
    /// `<wp:posOffset>` in EMU. Exactly one of `align` / `offset` is `Some`.
    pub offset: Option<i64>,
}

/// The wrap mode for a floating drawing.
#[derive(Copy, Clone)]
pub enum AnchorWrap {
    /// `<wp:wrapTopAndBottom/>` — text flows above and below.
    TopAndBottom,
    /// `<wp:wrapSquare wrapText=…/>` — text wraps around the box.
    Square(&'static str),
    /// `<wp:wrapNone/>` — drawing floats over the text (overlap allowed).
    None,
}

/// A table.
pub struct Tbl {
    pub props: TblProps,
    pub grid: Vec<i32>,
    pub rows: Vec<Row>,
}

#[derive(Default)]
pub struct TblProps {
    pub width_dxa: Option<i32>,
    pub style: Option<EcoString>,
}

pub struct Row {
    pub header: bool,
    pub cant_split: bool,
    pub height: Option<RowHeight>,
    pub cells: Vec<Cell>,
}

pub struct RowHeight {
    pub val: i32,
    pub exact: bool,
}

pub struct Cell {
    pub w_dxa: Option<i32>,
    pub grid_span: u32,
    pub v_merge: Option<VMerge>,
    pub borders: CellBorders,
    pub shd_fill: Option<[u8; 3]>,
    pub valign: Option<VAlign>,
    pub blocks: Vec<Block>,
}

#[derive(Copy, Clone)]
pub enum VMerge {
    Restart,
    Continue,
}

#[derive(Copy, Clone)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Default)]
pub struct CellBorders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
}

#[derive(Copy, Clone)]
pub struct Border {
    /// Width in eighths of a point.
    pub sz: u32,
    pub color: [u8; 3],
}

/// Page setup → `<w:sectPr>`.
#[derive(Clone)]
pub struct SectPr {
    /// Page width in twips.
    pub page_w: i32,
    /// Page height in twips.
    pub page_h: i32,
    pub landscape: bool,
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub header: i32,
    pub footer: i32,
    pub columns: u32,
    /// Binding allowance in twips (`w:pgMar/@w:gutter`). Default 0.
    pub gutter: i32,
    /// Equal-width column gutter in twips (Word `w:cols/@w:space`). Default 720.
    pub col_space: i32,
    /// Page-number glyph format + start, if `set page(numbering:)` is active.
    pub pg_num: Option<PgNumType>,
    /// `w:type` (only for non-final sections / `pagebreak(to:)`); None = default `nextPage`.
    pub sect_type: Option<SectType>,
    /// Header references (r:id + type). Emitted BEFORE pgSz.
    pub headers: Vec<HdrFtrRef>,
    /// Footer references (r:id + type). Emitted after headers, BEFORE pgSz.
    pub footers: Vec<HdrFtrRef>,
    /// `<w:titlePg/>` (distinct first page). Default false.
    pub title_pg: bool,
}

/// Page-number format + start for `<w:pgNumType>`.
#[derive(Clone)]
pub struct PgNumType {
    /// `w:fmt` value: "decimal" | "lowerRoman" | "upperRoman" | "lowerLetter" |
    /// "upperLetter" | "decimalZero".
    pub fmt: &'static str,
    /// `w:start`, from `counter(page).update(n)`; None = omit.
    pub start: Option<i64>,
}

/// `<w:type>` value on a non-final section.
#[derive(Copy, Clone)]
pub enum SectType {
    NextPage,
    EvenPage,
    OddPage,
    Continuous,
}

/// A header/footer reference inside `<w:sectPr>`.
#[derive(Clone)]
pub struct HdrFtrRef {
    /// `w:type`: "default" | "first" | "even".
    pub kind: &'static str,
    /// Matching relationship id in document.xml.rels.
    pub rel: EcoString,
}

/// A header/footer part (`word/headerN.xml` / `word/footerN.xml`).
pub struct HdrFtrPart {
    /// File name e.g. "header1.xml" (relative to word/).
    pub part_name: EcoString,
    /// true = header (root `w:hdr`, header content-type), false = footer (`w:ftr`).
    pub is_header: bool,
    pub blocks: Vec<Block>,
    /// Relationships (images, external links) created while lowering this part's
    /// content. They MUST live in this part's own `word/_rels/<name>.rels` — an
    /// `r:id` in `header1.xml` resolves against `header1.xml.rels`, not the
    /// document's — or Word refuses to open the file.
    pub rels: crate::package::Rels,
}

impl Default for SectPr {
    fn default() -> Self {
        // US Letter, 1-inch margins (in twips: 1 inch = 1440). The driver
        // overwrites this with the resolved page setup; it remains a sane
        // fallback for callers that build a `SectPr` directly.
        Self {
            page_w: 12240,
            page_h: 15840,
            landscape: false,
            margin_top: 1440,
            margin_bottom: 1440,
            margin_left: 1440,
            margin_right: 1440,
            header: 720,
            footer: 720,
            columns: 1,
            gutter: 0,
            col_space: 720,
            pg_num: None,
            sect_type: None,
            headers: Vec::new(),
            footers: Vec::new(),
            title_pg: false,
        }
    }
}

/// One footnote entry routed to `footnotes.xml`.
pub struct Footnote {
    pub id: i32,
    pub blocks: Vec<Block>,
}

/// A media part to be embedded in `word/media/`.
pub struct MediaPart {
    pub rel: EcoString,
    pub part_name: EcoString,
    pub ext: EcoString,
    pub bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Numbering (lists / enums).
// ---------------------------------------------------------------------------

/// The numbering table for `numbering.xml`.
#[derive(Default)]
pub struct NumberingTable {
    /// Distinct abstract numbering definitions.
    pub abstracts: Vec<AbstractNum>,
    /// Concrete `<w:num>` instances mapping a numId to an abstractNumId.
    pub nums: Vec<NumInstance>,
}

pub struct AbstractNum {
    pub id: u32,
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
}

pub struct NumInstance {
    pub num_id: u32,
    pub abstract_id: u32,
    /// Optional level-0 start override.
    pub start_override: Option<u64>,
}

/// The numbering shape passed to `register_list`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListSpec {
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
    pub restart_at_1: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListLevel {
    pub num_fmt: NumFmt,
    pub lvl_text: EcoString,
    pub start: u64,
    pub ind_left: i32,
    pub ind_hanging: i32,
    pub bullet_font: Option<EcoString>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum NumFmt {
    Bullet,
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    None,
}

impl NumFmt {
    pub fn as_str(self) -> &'static str {
        match self {
            NumFmt::Bullet => "bullet",
            NumFmt::Decimal => "decimal",
            NumFmt::LowerLetter => "lowerLetter",
            NumFmt::UpperLetter => "upperLetter",
            NumFmt::LowerRoman => "lowerRoman",
            NumFmt::UpperRoman => "upperRoman",
            NumFmt::None => "none",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum MultiLevelType {
    SingleLevel,
    Multilevel,
    HybridMultilevel,
}

impl MultiLevelType {
    pub fn as_str(self) -> &'static str {
        match self {
            MultiLevelType::SingleLevel => "singleLevel",
            MultiLevelType::Multilevel => "multilevel",
            MultiLevelType::HybridMultilevel => "hybridMultilevel",
        }
    }
}

// ---------------------------------------------------------------------------
// Bookmarks.
// ---------------------------------------------------------------------------

use rustc_hash::FxHashMap;
use typst_library::introspection::Location;

/// Maps `Location`s to their assigned bookmark `(name, id)`.
#[derive(Default)]
pub struct BookmarkTable {
    pub by_location: FxHashMap<Location, (EcoString, u32)>,
}

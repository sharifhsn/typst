//! The **Typst document IR** — a Typst-shaped tree that the emitter
//! pretty-prints to source. This is the mirror of `typst-docx`'s DOM, but
//! target-side: it speaks headings/strong/emph/lists rather than
//! paragraphs/runs/`w:jc`. Semantic-inference passes (tier 2) rewrite *this*
//! tree; going straight from the Word IR ([`crate::wml`]) to a string is what
//! produces unreadable output, so everything routes through here.

use ecow::EcoString;

pub use crate::wml::model::{LegendPos, SectionStart};

/// A whole Typst document: a hoisted preamble (`#set`/`#show`/imports) plus the
/// body. The emitter renders the preamble first, then the body as markup.
#[derive(Debug, Default, Clone)]
pub struct TypstDoc {
    pub preamble: Vec<Stmt>,
    pub body: Vec<Block>,
}

impl TypstDoc {
    /// Every block tree in the document: the header/footer content hanging off
    /// a `#set page(..)` in the preamble, then the body.
    ///
    /// Furniture is ordinary content — paragraphs, tables, images — that merely
    /// happens to be reachable through the preamble rather than sitting in
    /// `body`. A pass that walks only `body` silently leaves headers and
    /// footers at tier-1 literal formatting while the body around them gets
    /// made idiomatic, which is both inconsistent and ugly. Passes should walk
    /// this instead.
    ///
    /// A [`Block::Section`] buried inside `body` — a later Word section's own
    /// content and page setup — is deliberately *not* also flattened into a
    /// separate entry here: `body` is returned as one whole `&mut Vec<Block>`,
    /// and Rust can't hand out both that and a second, independent mutable
    /// reference reached by indexing *into* one of its own elements at the
    /// same time (the classic "can't borrow the whole vec and one of its
    /// insides simultaneously" rule — this isn't a workaround-able API
    /// limitation, doing so would let the whole-vec handle invalidate the
    /// inner one, e.g. by clearing the vec out from under it). So a
    /// `Block::Section` is reached the same way [`Block::Table`]'s cells or
    /// [`Block::Figure`]'s caption already are: each pass's own block-walking
    /// function recurses into `Section::body` directly as it iterates this
    /// one tree, and reaches `Section::setup`'s own header/footer via
    /// [`push_furniture_trees`] — the same helper this method uses for the
    /// preamble's initial page setup, just called one section at a time
    /// instead of once. See `passes::collapse_style`/`passes::strong_emph`'s
    /// `Block::Section` arm.
    pub fn block_trees_mut(&mut self) -> Vec<&mut Vec<Block>> {
        let mut trees: Vec<&mut Vec<Block>> = Vec::new();
        for stmt in &mut self.preamble {
            if let Stmt::SetPage(page) = stmt {
                push_furniture_trees(page, &mut trees);
            }
        }
        trees.push(&mut self.body);
        trees
    }
}

/// Push `page`'s header/footer content (default/first/even, whichever are
/// present) as independent block trees. Shared by [`TypstDoc::block_trees_mut`]
/// (for the preamble's initial page setup) and, per-section, by every tier-2
/// pass's own `Block::Section` handling (for a later section's own page
/// setup) — both hang furniture off a [`PageSetup`] the exact same way.
pub(crate) fn push_furniture_trees<'a>(page: &'a mut PageSetup, trees: &mut Vec<&'a mut Vec<Block>>) {
    // Disjoint fields, so both may be borrowed mutably at once.
    let furniture = [page.header.as_mut(), page.footer.as_mut()];
    for furniture in furniture.into_iter().flatten() {
        trees.push(&mut furniture.default);
        trees.extend(furniture.first.as_mut());
        trees.extend(furniture.even.as_mut());
    }
}

/// A preamble statement. `Verbatim` is the escape hatch for anything the
/// structured variants don't cover yet.
#[derive(Debug, Clone)]
// `PageSetup` (with per-section page-numbering added) now outsizes
// `TextStyle`/`ParStyle` enough for clippy to flag it — the same non-issue as
// `wml::model::BodyItem`'s own allow: `Stmt` only ever lives in a
// heap-allocated `Vec<Stmt>` (the preamble), so the size difference between
// variants costs nothing, and boxing `PageSetup` would only add indirection
// to the one variant every document actually has.
#[allow(clippy::large_enum_variant)]
pub enum Stmt {
    SetDocument(DocumentInfo),
    SetPage(PageSetup),
    SetText(TextStyle),
    SetPar(ParStyle),
    Verbatim(EcoString),
}

/// Document metadata (`#set document(..)`), lowered from the package's core
/// properties. Word's single-string fields are already split into the shapes
/// Typst wants: an author list, a keyword list, and a calendar date.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DocumentInfo {
    pub title: Option<EcoString>,
    pub authors: Vec<EcoString>,
    pub keywords: Vec<EcoString>,
    pub date: Option<Date>,
}

impl DocumentInfo {
    pub fn is_empty(&self) -> bool {
        *self == DocumentInfo::default()
    }
}

/// A calendar date — the year/month/day Typst's `datetime` constructor takes.
/// Word stores a full W3CDTF timestamp, but `#set document(date:)` is only
/// ever displayed as a date, so the time of day is deliberately dropped.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// A block-level item — one line/unit of Typst markup.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// `= Heading` (level 1..=6 → one to six `=`).
    Heading { level: u8, body: Inlines },
    /// An ordinary markup paragraph.
    Paragraph { style: ParStyle, body: Inlines },
    /// A bullet/numbered list (possibly nested via item `level`).
    List(List),
    Table(Table),
    /// An image plus optional caption (a `#figure`).
    Figure(Figure),
    /// A Word chart — by default imported as the data table behind it (Typst
    /// has no native chart-drawing primitive), or as a real `lilaq` plot
    /// under [`crate::opts::ChartStyle::Plot`]. See [`Chart`].
    Chart(Chart),
    /// A fenced code block (` ```lang … ``` `).
    CodeBlock { lang: Option<EcoString>, text: EcoString },
    /// A block equation — Typst math source (or an OMML fallback string).
    Equation { body: EcoString },
    /// A horizontal rule (`#line(length: 100%)`).
    Rule,
    Break(BreakKind),
    /// A Word section after the first: its own page setup, how it starts, and
    /// the content it governs. Nested rather than flat because a `continuous`
    /// section that only changes columns renders as `#columns(n)[..]`, which
    /// has to *wrap* its content (see `emit::render_section`). Always a
    /// top-level sibling of other blocks — never itself nested inside a
    /// table cell, list item, footnote, or another section's body, since a
    /// Word section boundary can only ever occur at the document's own top
    /// level (see `wml::parse::parse_document_body`).
    Section(Section),
    /// Raw Typst source, emitted verbatim — the escape hatch for an unmapped
    /// construct. Always paired with an [`crate::report::ImportReport`] entry.
    Verbatim(EcoString),
}

/// [`Block::Section`]'s payload — a Word section that isn't the document's
/// first (whose page setup and content are instead flattened straight into
/// the preamble/body, exactly as a single-section document always has been;
/// see `lower::lower`).
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub setup: PageSetup,
    pub start: SectionStart,
    pub body: Vec<Block>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BreakKind {
    Page,
    Column,
}

/// A sequence of inline content.
pub type Inlines = Vec<Inline>;

/// An inline-level item.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    /// Literal text — markup-escaped at emit time.
    Text(EcoString),
    /// A single space (kept explicit so run boundaries don't glue words).
    Space,
    /// A forced line break (`\`).
    Linebreak,
    Strong(Inlines),
    Emph(Inlines),
    /// Inline raw/code span (`` `…` ``).
    Raw(EcoString),
    /// `#link("dest")[body]` — an external target.
    Link { dest: EcoString, body: Inlines },
    /// `#link(<label>)[body]` — a jump to a bookmark elsewhere in the
    /// document. Distinct from [`Self::Link`] because Typst takes a label as a
    /// bare `<..>` term, not a quoted string, so the two can't share one
    /// destination field without smuggling markup through it.
    LabelLink { label: EcoString, body: Inlines },
    /// Direct character formatting the semantic wrappers don't capture
    /// (font/size/color/underline/…): `#text(..)[body]`.
    Styled { style: TextStyle, body: Inlines },
    /// Inline equation source (`$…$`).
    Math(EcoString),
    /// A live page number for a bookmark (`PAGEREF`). Structured rather than
    /// `Verbatim` so `passes::resolve_labels` can find and downgrade it when
    /// the label turns out never to have been emitted.
    PageRef(EcoString),
    /// `<name>` — a Typst label, lowered from a `w:bookmarkStart`. A label
    /// attaches to whatever *precedes* it, so `mappers::para` hoists these to
    /// the end of their block: Word writes a bookmark at the start of the
    /// paragraph it marks, which in Typst markup would label the block before.
    Label(EcoString),
    /// `#ruby[base][gloss]` — a phonetic guide (furigana). Typst has no ruby
    /// primitive, so the emitter defines a `ruby` helper in the preamble when
    /// a document uses one.
    Ruby { base: Inlines, gloss: Inlines },
    /// `#footnote[…]` — the note's content inlined at the reference site,
    /// which is how Typst models footnotes (there is no separate note store).
    Footnote(Vec<Block>),
    /// `#box[…]` — a Word text box's content, inlined at its anchor. Word
    /// floats a text box at an arbitrary page position; Typst has no
    /// equivalent that survives reflow, so the content is kept inline and the
    /// geometry is dropped.
    TextBox(Vec<Block>),
    /// A drawn shape: a ready-made Typst call (`#rect(..)`, `#curve(..)`, …)
    /// plus, for a shape Word also gave a text box, the content that goes
    /// inside it.
    ///
    /// The call arrives pre-rendered because a shape is a closed expression
    /// with nothing for a tier-2 pass to promote — the same reasoning behind
    /// [`Self::Verbatim`], which the VML shape mapper still uses. It is a
    /// variant of its own only because `body` cannot ride inside a string:
    /// those are real blocks, and the passes have to reach them.
    Shape { call: EcoString, body: Vec<Block> },
    /// Raw Typst source, emitted verbatim (escape hatch).
    Verbatim(EcoString),
}

/// Direct character-level formatting. Used both inline ([`Inline::Styled`]) and
/// as a document default ([`Stmt::SetText`]). `bold`/`italic` live here for the
/// literal (tier-1) lowering; the tier-2 collapse pass promotes them to
/// [`Inline::Strong`]/[`Inline::Emph`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TextStyle {
    pub font: Option<EcoString>,
    pub size_pt: Option<f64>,
    pub color: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Underline>,
    pub strike: bool,
    pub smallcaps: bool,
    /// All-capitals display (`w:caps`) → `#upper[..]`.
    pub caps: bool,
    pub script: Option<Script>,
    /// A marker background (`w:highlight`) → `#highlight(fill: ..)`.
    pub highlight: Option<[u8; 3]>,
    /// Inter-character tracking in points (`w:rPr/w:spacing`); may be negative.
    pub tracking_pt: Option<f64>,
    /// A language tag split into Typst's two arguments: `("en", Some("US"))`.
    pub lang: Option<Lang>,
}

impl TextStyle {
    pub fn is_empty(&self) -> bool {
        *self == TextStyle::default()
    }
}

/// An underline's stroke. Word's `w:u` carries a line pattern and its own
/// color, both of which Typst expresses through `#underline(stroke: ..)`.
/// Patterns Typst has no dash for (Word's `wave`/`double`) are reported as
/// approximations by [`crate::mappers::run`] and drawn as a plain line — the
/// underline itself is never dropped just because its pattern is unusual.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Underline {
    pub color: Option<[u8; 3]>,
    /// A Typst stroke `dash:` name, when Word asked for a patterned line.
    /// `None` draws a solid line.
    pub dash: Option<&'static str>,
    /// `w:u/@w:val="thick"` — drawn with a heavier stroke.
    pub thick: bool,
}

/// A resolved language tag: Typst splits what Word stores as one `w:lang`
/// value ("en-US") into separate `lang:`/`region:` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lang {
    pub lang: EcoString,
    pub region: Option<EcoString>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Script {
    Super,
    Sub,
}

/// Paragraph-level formatting.
///
/// Word and Typst disagree about paragraph spacing: Word gives a paragraph an
/// independent space *before* and *after*, while Typst has a single
/// `par.spacing` for the gap *between* paragraphs. Both Word values are kept
/// here rather than pre-collapsed, because the two consumers need them
/// differently — a hoisted document default sums them into one `spacing:`
/// (see `emit::render_set_par`), while a paragraph that deviates from that
/// default keeps them apart as a block's `above:`/`below:` (see
/// `emit::render_paragraph`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParStyle {
    pub align: Option<Align>,
    /// Leading (line spacing) in points, when it differs from the default.
    pub leading_pt: Option<f64>,
    /// Space before the paragraph, in points.
    pub spacing_before_pt: Option<f64>,
    /// Space after the paragraph, in points.
    pub spacing_after_pt: Option<f64>,
    /// Left indent in points.
    pub indent_pt: Option<f64>,
    /// Right indent in points.
    pub indent_right_pt: Option<f64>,
    /// Extra indent on the first line only (`w:ind/@w:firstLine`).
    pub first_line_indent_pt: Option<f64>,
    /// First line pulled back out of the left indent (`w:ind/@w:hanging`).
    pub hanging_indent_pt: Option<f64>,
    /// Paragraph background shading (`w:pPr/w:shd`).
    pub fill: Option<[u8; 3]>,
}

impl ParStyle {
    pub fn is_empty(&self) -> bool {
        *self == ParStyle::default()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, PartialEq)]
pub struct List {
    pub items: Vec<ListItem>,
    /// A Typst `enum(numbering:)` pattern when the Word list uses formats
    /// other than plain decimal (`"i."`, `"1.a."`, …). `None` keeps Typst's
    /// default numbering.
    pub numbering: Option<EcoString>,
    /// The number the list counts from, when it isn't 1.
    pub start: Option<i64>,
    /// A Typst `list(marker:)` cycle — one authored bullet glyph per nesting
    /// depth, shallowest first — when the Word list's markers differ from
    /// Typst's own. Empty keeps Typst's defaults. Cycling by depth is exactly
    /// how Typst reads the argument, and exactly how Word stores it (one
    /// `w:lvlText` per `w:ilvl`), so the two line up without reinterpretation.
    pub markers: Vec<EcoString>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub ordered: bool,
    /// Nesting depth, 0-based (indentation in the emitted markup).
    pub level: u8,
    pub body: Inlines,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: usize,
    /// Per-column widths in points; `None` = auto.
    pub column_widths: Vec<Option<f64>>,
    pub rows: Vec<TableRow>,
    /// How the table sits between the margins (`w:tblPr/w:jc`) — emitted as an
    /// `#align(..)` wrapper. `None` leaves it where the flow puts it.
    pub align: Option<Align>,
    /// A left indent (`w:tblPr/w:tblInd`) in points, emitted as `#pad(left:)`.
    /// Only ever set for a table Word left at its default (left) alignment:
    /// Word itself ignores the indent on a centred or right-aligned table.
    pub indent_pt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    pub header: bool,
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub colspan: usize,
    pub rowspan: usize,
    pub fill: Option<[u8; 3]>,
    /// Per-side borders (`w:tcBorders`).
    pub stroke: CellStroke,
    /// Vertical alignment within the cell (`w:vAlign`). Horizontal alignment
    /// isn't here: it comes from the `w:jc` on the cell's own paragraphs,
    /// which lower through the ordinary paragraph path.
    pub align: Option<VAlign>,
    /// Inner padding (`w:tcMar`).
    pub inset: Option<Sides>,
    pub body: Vec<Block>,
}

impl TableCell {
    /// An ordinary single-slot cell with no formatting of its own — the
    /// filler used to pad a short row out to the grid width.
    pub fn empty() -> Self {
        TableCell {
            colspan: 1,
            rowspan: 1,
            fill: None,
            stroke: CellStroke::default(),
            align: None,
            inset: None,
            body: Vec::new(),
        }
    }
}

/// A cell's four border sides. A side left `None` says Word stated nothing and
/// the table's own stroke should show through — which is *not* the same as
/// [`Border::None`], Word explicitly drawing no line there.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CellStroke {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
}

impl CellStroke {
    pub fn is_empty(&self) -> bool {
        *self == CellStroke::default()
    }
}

/// One cell border side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Border {
    /// Word explicitly draws no border here (`w:val="nil"`).
    None,
    Line { thickness_pt: f64, color: Option<[u8; 3]> },
}

/// Four per-side lengths in points — Typst's `inset:`/`pad`-style dictionary.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Sides {
    pub top: Option<f64>,
    pub bottom: Option<f64>,
    pub left: Option<f64>,
    pub right: Option<f64>,
}

impl Sides {
    pub fn is_empty(&self) -> bool {
        *self == Sides::default()
    }
}

/// Vertical alignment inside a table cell. Named for the Typst alignments they
/// emit as, since `w:vAlign="center"` is Typst's `horizon`, not `center`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Horizon,
    Bottom,
}

/// A Word chart — [`Block::Chart`]'s payload. Brought across either as the
/// data table behind it or as a drawn plot, depending on
/// [`crate::opts::ChartStyle`]; see [`ChartContent`].
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub title: Option<EcoString>,
    pub content: ChartContent,
}

/// How a chart's data is represented in the Typst IR. `Table` is always
/// available (see [`crate::wml::model::ChartData`]'s doc comment); `Plot`
/// only when [`crate::opts::ChartStyle::Plot`] is requested *and*
/// [`crate::mappers::chart::lower_chart`] finds the chart plottable —
/// otherwise it falls back to `Table` there too.
#[derive(Debug, Clone, PartialEq)]
pub enum ChartContent {
    Table(Table),
    Plot(Plot),
}

/// A chart redrawn with the `lilaq` plotting package, rather than as its data
/// table.
#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    pub kind: PlotKind,
    /// The size Word laid the chart out at (`wp:extent`). Rendering at the
    /// plotting library's default instead makes a chart with several long
    /// category labels collide its own ticks and legend.
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    /// Where to put the legend. `None` means the chart declared none, so none
    /// is drawn — not "use the library default", which would invent a legend
    /// Word deliberately left off.
    pub legend: Option<LegendPos>,
    /// Category labels for the x axis. Empty means plot against the point
    /// index.
    pub categories: Vec<EcoString>,
    pub series: Vec<PlotSeries>,
}

/// The `lilaq` mark used to draw a [`Plot`] — a narrower set than
/// [`crate::wml::model::ChartKind`]: an area chart's outline maps onto `Line`
/// (see [`crate::mappers::chart`]'s lowering doc comment for why), and a
/// chart with no plotting counterpart never reaches this type at all.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PlotKind {
    Bar,
    Line,
    Scatter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlotSeries {
    pub name: Option<EcoString>,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Figure {
    /// Project-relative asset path (written into [`crate::ImportResult::assets`]).
    pub image_path: EcoString,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub alt: Option<EcoString>,
    pub caption: Option<Inlines>,
    /// Horizontal placement, for a drawing Word floated with a named
    /// alignment. `None` leaves the figure in the flow's own alignment.
    pub align: Option<Align>,
    /// A rounded outline Word framed the picture with
    /// (`pic:spPr/a:prstGeom prst="roundRect"`), as a corner radius in points
    /// — emitted as the `radius:` of a clipping `#box` around the image, the
    /// exact inverse of what `typst-docx` writes for
    /// `box(radius: .., clip: true)[image]`.
    pub radius_pt: Option<f64>,
    /// A crop (`pic:blipFill/a:srcRect`). Typst's `image` has no crop
    /// parameter, so this is expressed by oversizing the image inside the
    /// clipping box and offsetting it — see `emit::Emitter::render_figure`,
    /// which is where the arithmetic lives.
    pub crop: Option<Crop>,
}

/// How much of each side of a picture Word cropped away, as a *fraction* of
/// the original image's own extent (`0.1` = the leading tenth is hidden).
/// [`crate::wml::model::SrcRect`]'s 1000ths-of-a-percent are converted once,
/// at lower time, so the emitter deals in one unit.
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct Crop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// Page geometry (`#set page(..)`), from a `w:sectPr`.
///
/// For the document's first section this is always fully populated (there is
/// nothing before it to omit anything relative to); for a later section (see
/// [`Section`]) it's likewise the section's own complete, resolved setup —
/// `emit::render_section` is what decides which fields still need restating
/// against the section before, not this type itself.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PageSetup {
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub margin: Option<Margins>,
    pub flipped: bool,
    /// Text columns (`#set page(columns: n)`). `None` or 1 is a single
    /// column and emits nothing.
    pub columns: Option<u32>,
    pub header: Option<Furniture>,
    pub footer: Option<Furniture>,
    /// `w:pgNumType/@w:fmt`, already mapped to Typst's `numbering:` glyph
    /// string (`"1"`/`"i"`/`"I"`/`"a"`/`"A"`) — see `emit::page_numbering_arg`.
    /// `None` means this section doesn't set a format (Word: inherit whatever
    /// was already active).
    pub page_num_fmt: Option<EcoString>,
    /// `w:pgNumType/@w:start` — emits `#counter(page).update(n)` right after
    /// this section's `#set page(..)`. `None` means no restart here.
    pub page_num_start: Option<i64>,
}

impl PageSetup {
    /// Whether stepping from `previous` to `self` changes nothing except
    /// (possibly) the column count — i.e. nothing that would force Typst to
    /// start a new page even without an explicit `#pagebreak()` (page size,
    /// margins, orientation, header/footer content, and page-numbering are
    /// all page-level style-chain properties that can only take effect
    /// starting the *next* page; a pure column change is the one exception,
    /// since `#columns(n)[..]` is a body-level wrapper, not a page property).
    ///
    /// `page_num_start` is checked on `self` alone, not diffed against
    /// `previous`: it's a discrete "restart the counter here" action, not a
    /// standing property, so its mere presence on this section — regardless
    /// of what the previous section had — always means something beyond
    /// columns is happening.
    ///
    /// Used both while lowering a `Continuous` section (to decide whether it
    /// can become a `#columns(..)` wrapper, or must be reported as an
    /// approximation — see `lower::lower`) and while emitting one (to pick
    /// the same rendering path without re-deriving a different answer — see
    /// `emit::render_section`), so the two sides can never disagree.
    pub(crate) fn matches_except_columns(&self, previous: &PageSetup) -> bool {
        self.width_pt == previous.width_pt
            && self.height_pt == previous.height_pt
            && self.margin == previous.margin
            && self.flipped == previous.flipped
            && self.page_num_fmt == previous.page_num_fmt
            && self.page_num_start.is_none()
            && self.header == previous.header
            && self.footer == previous.footer
    }
}

/// Page furniture — a header or a footer. Word varies it by page class; Typst
/// has one `header:`/`footer:` per page setup, so the variants collapse into a
/// single `context`-conditional at emit time.
#[derive(Debug, Clone, PartialEq)]
pub struct Furniture {
    pub default: Vec<Block>,
    /// Only populated when `w:titlePg` is set.
    pub first: Option<Vec<Block>>,
    /// Only populated when `settings.xml` sets `w:evenAndOddHeaders`.
    pub even: Option<Vec<Block>>,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Margins {
    pub top_pt: f64,
    pub bottom_pt: f64,
    pub left_pt: f64,
    pub right_pt: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_setups_match_except_columns() {
        let a = PageSetup { width_pt: Some(400.0), ..Default::default() };
        let b = a.clone();
        assert!(a.matches_except_columns(&b));
    }

    #[test]
    fn a_pure_column_change_still_matches() {
        let a = PageSetup { width_pt: Some(400.0), columns: Some(2), ..Default::default() };
        let b = PageSetup { width_pt: Some(400.0), columns: Some(3), ..Default::default() };
        assert!(a.matches_except_columns(&b));
    }

    #[test]
    fn a_geometry_change_does_not_match() {
        let a = PageSetup { width_pt: Some(400.0), ..Default::default() };
        let b = PageSetup { width_pt: Some(500.0), ..Default::default() };
        assert!(!a.matches_except_columns(&b));
    }

    #[test]
    fn a_page_numbering_format_change_does_not_match() {
        let a = PageSetup { page_num_fmt: Some("decimal".into()), ..Default::default() };
        let b = PageSetup { page_num_fmt: Some("lowerRoman".into()), ..Default::default() };
        assert!(!a.matches_except_columns(&b));
    }

    /// A restart is checked on `self` alone — even if `previous` had one too,
    /// `self` wanting a fresh restart is still "something besides columns
    /// changed" (see the doc comment on why this isn't diffed against
    /// `previous` like every other field).
    #[test]
    fn a_page_number_restart_never_matches_regardless_of_the_previous_section() {
        let a = PageSetup { page_num_start: Some(1), ..Default::default() };
        let b = PageSetup { page_num_start: Some(1), ..Default::default() };
        assert!(!a.matches_except_columns(&b));
    }
}

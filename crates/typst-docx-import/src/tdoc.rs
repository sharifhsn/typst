//! The **Typst document IR** — a Typst-shaped tree that the emitter
//! pretty-prints to source. This is the mirror of `typst-docx`'s DOM, but
//! target-side: it speaks headings/strong/emph/lists rather than
//! paragraphs/runs/`w:jc`. Semantic-inference passes (tier 2) rewrite *this*
//! tree; going straight from the Word IR ([`crate::wml`]) to a string is what
//! produces unreadable output, so everything routes through here.

use ecow::EcoString;

pub use crate::wml::model::LegendPos;

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
    pub fn block_trees_mut(&mut self) -> Vec<&mut Vec<Block>> {
        let mut trees: Vec<&mut Vec<Block>> = Vec::new();
        for stmt in &mut self.preamble {
            if let Stmt::SetPage(page) = stmt {
                // Disjoint fields, so both may be borrowed mutably at once.
                let furniture = [page.header.as_mut(), page.footer.as_mut()];
                for furniture in furniture.into_iter().flatten() {
                    trees.push(&mut furniture.default);
                    trees.extend(furniture.first.as_mut());
                    trees.extend(furniture.even.as_mut());
                }
            }
        }
        trees.push(&mut self.body);
        trees
    }
}

/// A preamble statement. `Verbatim` is the escape hatch for anything the
/// structured variants don't cover yet.
#[derive(Debug, Clone)]
pub enum Stmt {
    SetPage(PageSetup),
    SetText(TextStyle),
    SetPar(ParStyle),
    Verbatim(EcoString),
}

/// A block-level item — one line/unit of Typst markup.
#[derive(Debug, Clone)]
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
    /// Raw Typst source, emitted verbatim — the escape hatch for an unmapped
    /// construct. Always paired with an [`crate::report::ImportReport`] entry.
    Verbatim(EcoString),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BreakKind {
    Page,
    Column,
}

/// A sequence of inline content.
pub type Inlines = Vec<Inline>;

/// An inline-level item.
#[derive(Debug, Clone)]
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
    /// `#link("dest")[body]`.
    Link { dest: EcoString, body: Inlines },
    /// Direct character formatting the semantic wrappers don't capture
    /// (font/size/color/underline/…): `#text(..)[body]`.
    Styled { style: TextStyle, body: Inlines },
    /// Inline equation source (`$…$`).
    Math(EcoString),
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
    pub underline: bool,
    pub strike: bool,
    pub smallcaps: bool,
    pub script: Option<Script>,
}

impl TextStyle {
    pub fn is_empty(&self) -> bool {
        *self == TextStyle::default()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Script {
    Super,
    Sub,
}

/// Paragraph-level formatting.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParStyle {
    pub align: Option<Align>,
    /// Leading (line spacing) in points, when it differs from the default.
    pub leading_pt: Option<f64>,
    /// Space before the paragraph, in points.
    pub spacing_before_pt: Option<f64>,
    pub indent_pt: Option<f64>,
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

#[derive(Debug, Clone)]
pub struct List {
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub ordered: bool,
    /// Nesting depth, 0-based (indentation in the emitted markup).
    pub level: u8,
    pub body: Inlines,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub columns: usize,
    /// Per-column widths in points; `None` = auto.
    pub column_widths: Vec<Option<f64>>,
    pub rows: Vec<TableRow>,
}

#[derive(Debug, Clone)]
pub struct TableRow {
    pub header: bool,
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone)]
pub struct TableCell {
    pub colspan: usize,
    pub rowspan: usize,
    pub fill: Option<[u8; 3]>,
    pub body: Vec<Block>,
}

/// A Word chart — [`Block::Chart`]'s payload. Brought across either as the
/// data table behind it or as a drawn plot, depending on
/// [`crate::opts::ChartStyle`]; see [`ChartContent`].
#[derive(Debug, Clone)]
pub struct Chart {
    pub title: Option<EcoString>,
    pub content: ChartContent,
}

/// How a chart's data is represented in the Typst IR. `Table` is always
/// available (see [`crate::wml::model::ChartData`]'s doc comment); `Plot`
/// only when [`crate::opts::ChartStyle::Plot`] is requested *and*
/// [`crate::mappers::chart::lower_chart`] finds the chart plottable —
/// otherwise it falls back to `Table` there too.
#[derive(Debug, Clone)]
pub enum ChartContent {
    Table(Table),
    Plot(Plot),
}

/// A chart redrawn with the `lilaq` plotting package, rather than as its data
/// table.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct PlotSeries {
    pub name: Option<EcoString>,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct Figure {
    /// Project-relative asset path (written into [`crate::ImportResult::assets`]).
    pub image_path: EcoString,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub alt: Option<EcoString>,
    pub caption: Option<Inlines>,
}

/// Page geometry (`#set page(..)`), from a `w:sectPr`.
#[derive(Debug, Default, Clone)]
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
}

/// Page furniture — a header or a footer. Word varies it by page class; Typst
/// has one `header:`/`footer:` per page setup, so the variants collapse into a
/// single `context`-conditional at emit time.
#[derive(Debug, Clone)]
pub struct Furniture {
    pub default: Vec<Block>,
    /// Only populated when `w:titlePg` is set.
    pub first: Option<Vec<Block>>,
    /// Only populated when `settings.xml` sets `w:evenAndOddHeaders`.
    pub even: Option<Vec<Block>>,
}

#[derive(Debug, Copy, Clone)]
pub struct Margins {
    pub top_pt: f64,
    pub bottom_pt: f64,
    pub left_pt: f64,
    pub right_pt: f64,
}

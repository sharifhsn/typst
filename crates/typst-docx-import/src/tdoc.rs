//! The **Typst document IR** — a Typst-shaped tree that the emitter
//! pretty-prints to source. This is the mirror of `typst-docx`'s DOM, but
//! target-side: it speaks headings/strong/emph/lists rather than
//! paragraphs/runs/`w:jc`. Semantic-inference passes (tier 2) rewrite *this*
//! tree; going straight from the Word IR ([`crate::wml`]) to a string is what
//! produces unreadable output, so everything routes through here.

use ecow::EcoString;

/// A whole Typst document: a hoisted preamble (`#set`/`#show`/imports) plus the
/// body. The emitter renders the preamble first, then the body as markup.
#[derive(Debug, Default, Clone)]
pub struct TypstDoc {
    pub preamble: Vec<Stmt>,
    pub body: Vec<Block>,
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
}

#[derive(Debug, Copy, Clone)]
pub struct Margins {
    pub top_pt: f64,
    pub bottom_pt: f64,
    pub left_pt: f64,
    pub right_pt: f64,
}

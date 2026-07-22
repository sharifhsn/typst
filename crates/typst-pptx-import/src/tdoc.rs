//! The Typst IR: what the output *means*, before it is text.
//!
//! The second of the two IRs, and the reason the emitter stays simple. Every
//! PowerPoint judgement has already been made by the time a value lands here —
//! a `Paint` is a Typst colour, not an unresolved `schemeClr`; a length is
//! points, not EMU. What remains is a tree that a pass can rewrite and an
//! emitter can print.

use ecow::EcoString;

pub struct TypstDoc {
    /// Slide canvas in points.
    pub width: f64,
    pub height: f64,
    pub title: Option<EcoString>,
    pub author: Option<EcoString>,
    pub slides: Vec<Slide>,
}

pub struct Slide {
    /// Present only in idiomatic fidelity: the title placeholder, promoted out
    /// of the shape list into a touying heading.
    pub heading: Option<Vec<Inline>>,
    pub items: Vec<Item>,
    pub notes: Option<EcoString>,
    pub fill: Option<Paint>,
    /// `p:sld/@show="0"`. Kept, but commented — dropping it would lose
    /// authored content, and emitting it would show a slide PowerPoint hides.
    pub hidden: bool,
}

pub enum Item {
    /// A shape at its authored position, in points from the slide's top-left.
    Placed {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        rot: f64,
        /// `a:xfrm/@flipH`/`@flipV`. Typst mirrors with a negative scale, so
        /// these are reproducible after all.
        flip_h: bool,
        flip_v: bool,
        /// `a:bodyPr` insets in points, which PowerPoint reserves inside the
        /// box before any text is set.
        inset: Option<(f64, f64, f64, f64)>,
        /// `a:bodyPr/@anchor`: where the text sits vertically in a box that is
        /// usually taller than it. Ignoring it top-aligns every centred
        /// caption in the deck.
        anchor: Option<VAlign>,
        block: Block,
    },
    /// Ordinary flow content, laid out by Typst.
    Flow(Block),
}

pub enum Block {
    /// A run of paragraphs — a text box's body, or a table cell's.
    Paras(Vec<Para>),
    Image(Image),
    /// A drawn shape. `call` is a complete Typst expression; `body` is the
    /// text it contains, if any, which stays structured so passes can reach it.
    Shape { call: EcoString, body: Option<Vec<Para>> },
    Table(Table),
    /// Nested content that keeps its own coordinate space.
    Group(Vec<Item>),
}

pub struct Image {
    pub path: EcoString,
    pub width: f64,
    pub height: f64,
    pub alt: Option<EcoString>,
    /// A corner radius that clips the picture.
    pub radius: Option<f64>,
    /// `a:srcRect` as fractions `[l, t, r, b]`, already normalised to 0..1.
    pub crop: Option<[f64; 4]>,
}

#[derive(Default)]
pub struct Para {
    pub inlines: Vec<Inline>,
    pub align: Option<Align>,
    /// Nesting depth for a bulleted or numbered item.
    pub list: Option<ListItem>,
    pub margin_left: Option<f64>,
    pub indent: Option<f64>,
    pub leading: Option<f64>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
}

pub struct ListItem {
    pub level: u8,
    pub ordered: bool,
    /// The literal marker, when it is not one Typst would produce itself.
    pub marker: Option<EcoString>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

pub enum Inline {
    Text(EcoString),
    /// Character formatting wrapping other inlines.
    Styled { props: TextProps, body: Vec<Inline> },
    Link { dest: LinkTarget, body: Vec<Inline> },
    LineBreak,
    /// A live slide number.
    SlideNumber,
}

pub enum LinkTarget {
    Url(EcoString),
    /// A jump to another slide, by zero-based index.
    Slide(usize),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TextProps {
    pub size: Option<f64>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub fill: Option<Paint>,
    /// One or more families, in fallback order. PowerPoint states a Latin and
    /// an East-Asian face for the same run and picks per glyph; Typst's
    /// `font:` list falls back per glyph too, so the pair maps exactly.
    pub font: Vec<EcoString>,
    pub tracking: Option<f64>,
    pub sub: bool,
    pub super_: bool,
    pub highlight: Option<Paint>,
    pub upper: bool,
}

impl TextProps {
    pub fn is_empty(&self) -> bool {
        *self == TextProps::default()
    }
}

pub struct Table {
    /// Column widths in points. Empty when the table sizes itself.
    pub columns: Vec<f64>,
    pub rows: Vec<Row>,
    pub header_rows: usize,
    /// Column count for a table with no stated widths — a chart's recovered
    /// data, whose numbers want their own width rather than the plot's.
    pub auto_columns: usize,
}

pub struct Row {
    pub height: Option<f64>,
    pub cells: Vec<Cell>,
}

pub struct Cell {
    pub paras: Vec<Para>,
    pub colspan: usize,
    pub rowspan: usize,
    pub fill: Option<Paint>,
    pub align_y: Option<EcoString>,
    /// Left, top, right, bottom. A side PowerPoint never stated stays `None`
    /// and draws nothing — the table's own stroke is `none`, so an unstated
    /// edge must not inherit Typst's default grid.
    pub stroke: [Option<EcoString>; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// `#rrggbb`, with alpha folded in when it is not opaque.
    Rgb([u8; 4]),
    Gradient { stops: Vec<(f64, [u8; 4])>, angle: f64, radial: bool },
}

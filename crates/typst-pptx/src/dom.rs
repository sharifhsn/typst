use ecow::EcoString;
pub use typst_ooxml_core::dml::{FillSpec, PathSegment, StrokeSpec};
pub use typst_ooxml_core::media::MediaId;
use typst_ooxml_core::media::MediaRegistry;

/// A slide-level intermediate representation.
pub struct SlideIr {
    pub bg: Option<FillSpec>,
    pub shapes: Vec<SlideShape>,
}

/// A drawable slide item in painter's order.
pub enum SlideShape {
    TextBox(TextBox),
    MathBox(MathBox),
    TableBox(TableBox),
    Pic(Pic),
    Geom(GeomShape),
    Group(GroupShape),
    LinkOverlay(LinkOverlay),
}

/// An invisible native shape carrying a hyperlink over authored linked text.
/// Keeping click behavior outside the text run prevents Office consumers from
/// replacing Typst's explicit run color with the presentation hyperlink theme.
pub struct LinkOverlay {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub link: RunLink,
}

/// A positioned text box.
pub struct TextBox {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub wrap: TextWrap,
    pub columns: Option<TextColumns>,
    pub placeholder: Option<Placeholder>,
    pub paras: Vec<TextPara>,
}

/// Multi-column text body properties for a DrawingML text box.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct TextColumns {
    pub count: usize,
    pub gutter_emu: i64,
}

/// Text wrapping behavior for a DrawingML text body.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum TextWrap {
    None,
    Square,
}

/// Placeholder type attached to a slide text shape.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Placeholder {
    Title,
    Body,
    SlideNumber,
}

/// A positioned native Office math object with a plain DrawingML fallback.
pub struct MathBox {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub omml: String,
    pub fallback: EcoString,
}

/// A positioned editable PowerPoint table.
pub struct TableBox {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub cols: Vec<i64>,
    pub rows: Vec<TableRow>,
}

/// One table row.
pub struct TableRow {
    pub h_emu: i64,
    pub cells: Vec<TableCell>,
}

/// One table cell, including merge continuation placeholders.
pub struct TableCell {
    pub grid_span: usize,
    pub row_span: usize,
    pub h_merge: bool,
    pub v_merge: bool,
    pub fill: Option<FillSpec>,
    pub borders: CellBorders,
    pub h_align: Option<CellHAlign>,
    pub v_align: Option<CellVAlign>,
    pub paras: Vec<TextPara>,
}

/// Horizontal alignment inherited by paragraphs in a table cell.
#[derive(Clone, Copy)]
pub enum CellHAlign {
    Start,
    Left,
    Center,
    Right,
    End,
}

/// Vertical alignment of text within a table cell.
#[derive(Clone, Copy)]
pub enum CellVAlign {
    Top,
    Center,
    Bottom,
}

/// Per-edge table-cell borders.
#[derive(Clone, Default)]
pub struct CellBorders {
    pub left: Option<StrokeSpec>,
    pub right: Option<StrokeSpec>,
    pub top: Option<StrokeSpec>,
    pub bottom: Option<StrokeSpec>,
}

/// A text paragraph.
#[derive(Clone)]
pub struct TextPara {
    pub children: Vec<TextChild>,
    pub rtl: bool,
    /// Paragraph left margin from the text box edge, in EMUs.
    pub margin_left_emu: Option<i64>,
    /// First-line offset relative to `margin_left_emu`, in EMUs.
    pub first_line_indent_emu: Option<i64>,
    /// Absolute line pitch (baseline-to-baseline) in 1/100 pt (`a:spcPts`).
    /// A percentage (`a:spcPct`) would multiply the FONT's single spacing -
    /// which already includes its internal leading - inflating the measured
    /// pitch and overflowing the box. `None` = explicit single spacing.
    pub line_spacing_100pt: Option<i32>,
    pub bullet: Option<ParaBullet>,
}

/// A child of a DrawingML text paragraph.
#[derive(Clone)]
pub enum TextChild {
    Run(TextRun),
    Math(InlineMath),
}

/// A text run.
#[derive(Clone)]
pub struct TextRun {
    pub text: EcoString,
    pub family: EcoString,
    pub sz_100pt: i32,
    pub b: bool,
    pub i: bool,
    /// Text color as straight (non-premultiplied) sRGB + alpha.
    pub color: [u8; 4],
    /// Native DrawingML text highlight color.
    pub highlight: Option<[u8; 4]>,
    pub spc_100pt: Option<i32>,
    pub field: Option<TextField>,
}

/// A live DrawingML field represented by a text run fallback.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum TextField {
    SlideNumber,
}

/// An inline native Office math object with a plain DrawingML fallback.
#[derive(Clone)]
pub struct InlineMath {
    pub omml: String,
    pub fallback: TextRun,
}

/// Native bullet or autonumbering properties for a paragraph.
#[derive(Clone)]
pub struct ParaBullet {
    pub lvl: u8,
    pub mar_l_emu: i64,
    pub indent_emu: i64,
    pub kind: BulletKind,
}

/// Bullet marker kind.
#[derive(Clone)]
pub enum BulletKind {
    Char(EcoString),
    AutoNum { ty: &'static str, start_at: u32 },
}

/// A run hyperlink target.
#[derive(Clone)]
pub enum RunLink {
    Url(EcoString),
    Slide(usize),
}

/// A positioned picture.
pub struct Pic {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub media: MediaId,
    pub svg_media: Option<MediaId>,
    pub alt: Option<EcoString>,
    pub link: Option<RunLink>,
    pub geom: PicGeom,
    /// Source-rectangle crop `[left, top, right, bottom]` in 1/1000 % (OOXML
    /// `a:srcRect`), for a cover-fitted image whose overflow the clip hides.
    pub src_rect: Option<[i32; 4]>,
}

/// A preset geometry for a picture.
pub enum PicGeom {
    Rect,
    RoundRect { adj_100k: i32 },
    Ellipse,
}

/// A positioned vector shape.
pub struct GeomShape {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub geom: GeomKind,
    pub fill: Option<FillSpec>,
    pub stroke: Option<StrokeSpec>,
    pub link: Option<RunLink>,
}

/// The PowerPoint shape kind for a vector shape.
pub enum GeomKind {
    Path(PathGeom),
    /// A loose straight connector. DrawingML's `line` preset is the diagonal
    /// of its transform box; flips choose the diagonal direction.
    Connector {
        flip_h: bool,
        flip_v: bool,
    },
}

/// A positioned group.
pub struct GroupShape {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub children: Vec<SlideShape>,
}

/// A path geometry.
pub enum PathGeom {
    Rect,
    Ellipse,
    Custom(Vec<PathSegment>),
}

/// Shared slide conversion context.
pub struct SlideCtx {
    pub media: MediaRegistry,
}

impl Default for SlideCtx {
    fn default() -> Self {
        Self { media: MediaRegistry::new("ppt/media") }
    }
}

impl SlideCtx {
    /// Add media bytes to the shared registry, deduplicating identical bytes.
    pub fn add_media(&mut self, bytes: &[u8], ext: &str) -> MediaId {
        self.media.add(bytes, ext)
    }
}

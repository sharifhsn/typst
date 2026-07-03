use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use typst_utils::hash128;

/// A slide-level intermediate representation.
pub struct SlideIr {
    pub bg: Option<FillSpec>,
    pub shapes: Vec<SlideShape>,
}

/// A drawable slide item in painter's order.
pub enum SlideShape {
    TextBox(TextBox),
    Pic(Pic),
    Geom(GeomShape),
    Group(GroupShape),
}

/// A positioned text box.
pub struct TextBox {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub paras: Vec<TextPara>,
}

/// A text paragraph.
pub struct TextPara {
    pub runs: Vec<TextRun>,
    pub rtl: bool,
}

/// A text run.
pub struct TextRun {
    pub text: EcoString,
    pub family: EcoString,
    pub sz_100pt: i32,
    pub b: bool,
    pub i: bool,
    pub color: [u8; 3],
    pub spc_100pt: Option<i32>,
    pub link: Option<RunLink>,
}

/// A run hyperlink target.
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
    pub alt: Option<EcoString>,
}

/// A positioned vector shape.
pub struct GeomShape {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub rot_60k: i32,
    pub geom: PathGeom,
    pub fill: Option<FillSpec>,
    pub stroke: Option<StrokeSpec>,
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

/// One custom geometry path segment.
pub enum PathSegment {
    MoveTo(i64, i64),
    LineTo(i64, i64),
    CubicTo(i64, i64, i64, i64, i64, i64),
    Close,
}

/// A fill specification.
pub enum FillSpec {
    Solid([u8; 3]),
    LinearGradient { angle_60k: i32, stops: Vec<GradientStop> },
}

/// A gradient stop.
pub struct GradientStop {
    pub pos_100k: i32,
    pub color: [u8; 3],
}

/// A stroke specification.
pub struct StrokeSpec {
    pub color: [u8; 3],
    pub w_emu: i64,
    pub cap: &'static str,
    pub dash: Option<&'static str>,
}

/// Index into the package media registry.
pub type MediaId = usize;

/// One media part in the package.
pub struct MediaPart {
    pub part_name: EcoString,
    pub ext: EcoString,
    pub bytes: Vec<u8>,
}

/// Shared slide conversion context.
#[derive(Default)]
pub struct SlideCtx {
    pub media: Vec<MediaPart>,
    media_dedup: FxHashMap<u128, MediaId>,
}

impl SlideCtx {
    /// Add media bytes to the shared registry, deduplicating identical bytes.
    pub fn add_media(&mut self, bytes: &[u8], ext: &str) -> MediaId {
        let hash = hash128(bytes);
        if let Some(&id) = self.media_dedup.get(&hash) {
            return id;
        }

        let clean_ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let id = self.media.len();
        self.media.push(MediaPart {
            part_name: eco_format!("ppt/media/image{}.{}", id + 1, clean_ext),
            ext: clean_ext.into(),
            bytes: bytes.to_vec(),
        });
        self.media_dedup.insert(hash, id);
        id
    }
}

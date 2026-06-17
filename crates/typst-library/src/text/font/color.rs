//! Utilities for color font handling

use std::io::Read;

use skrifa::MetadataProvider;
use skrifa::bitmap::BitmapData;
use skrifa::color::{Brush, ColorPainter, ColorStop, CompositeMode, Extend, Transform};
use skrifa::instance::{LocationRef, Size as SkrifaSize};
use skrifa::outline::DrawSettings;
use skrifa::outline::pen::SvgPen;
use skrifa::raw::TableProvider;
use skrifa::raw::types::{BoundingBox, GlyphId, Point};
use typst_syntax::Span;
use usvg::tiny_skia_path;
use xmlwriter::XmlWriter;

use crate::foundations::Bytes;
use crate::layout::{Abs, Frame, FrameItem, Point as TypstPoint, Rect, Size};
use crate::text::FontInstance;
use crate::visualize::{
    ExchangeFormat, FixedStroke, Geometry, Image, RasterImage, Shape, SvgImage,
};

/// An RGBA color resolved from a font's `CPAL` palette (or the foreground
/// color for the `0xFFFF` sentinel palette index).
#[derive(Copy, Clone)]
struct RgbaColor {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl RgbaColor {
    const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self { red, green, blue, alpha }
    }
}

/// Whether this glyph should be rendered via simple outlining instead of via
/// `glyph_frame`.
pub fn should_outline(font: &FontInstance, glyph_id: u16) -> bool {
    let font_ref = font.skrifa();
    let gid = GlyphId::from(glyph_id);
    (font_ref.glyf().is_ok() || font_ref.cff().is_ok() || font_ref.cff2().is_ok())
        && !has_png_bitmap(font, gid)
        && font_ref.color_glyphs().get(gid).is_none()
        && glyph_svg_data(font, gid).is_none()
}

/// Whether the glyph has a PNG bitmap in one of the font's bitmap strikes.
fn has_png_bitmap(font: &FontInstance, gid: GlyphId) -> bool {
    font.skrifa()
        .bitmap_strikes()
        .glyph_for_size(SkrifaSize::unscaled(), gid)
        .is_some_and(|glyph| matches!(glyph.data, BitmapData::Png(_)))
}

/// Look up a glyph's raw `SVG` table data, if present.
fn glyph_svg_data(font: &FontInstance, gid: GlyphId) -> Option<&[u8]> {
    font.skrifa().svg().ok()?.glyph_data(gid).ok().flatten()
}

/// A frame that can draw a glyph.
#[derive(Clone)]
pub struct GlyphFrame {
    pub upem: Abs,
    pub item: GlyphFrameItem,
}

impl GlyphFrame {
    /// The font unit square.
    pub fn size(&self) -> Size {
        Size::splat(self.upem)
    }
}

impl From<GlyphFrame> for Frame {
    fn from(g: GlyphFrame) -> Self {
        let mut frame = Frame::soft(Size::splat(g.upem));
        match g.item {
            GlyphFrameItem::Tofu(pos, shape) => {
                frame.push(pos, FrameItem::Shape(shape, Span::detached()))
            }
            GlyphFrameItem::Image(pos, image, size) => {
                frame.push(pos, FrameItem::Image(image, size, Span::detached()))
            }
        }
        frame
    }
}

/// The glyph item that is drawn.
#[derive(Clone)]
pub enum GlyphFrameItem {
    /// A fallback rectangle.
    Tofu(TypstPoint, Shape),
    /// An image glyph.
    Image(TypstPoint, Image, Size),
}

impl GlyphFrameItem {
    /// The position of the glyph item inside the parent frame.
    pub fn pos(&self) -> TypstPoint {
        match *self {
            GlyphFrameItem::Tofu(pos, _) => pos,
            GlyphFrameItem::Image(pos, _, _) => pos,
        }
    }
}

/// Returns a frame representing a glyph and whether it is a fallback tofu
/// frame.
///
/// Should only be called on glyphs for which [`should_outline`] returns false.
///
/// The glyphs are sized in font units, [`text.item.size`] is not taken into
/// account.
///
/// [`text.item.size`]: crate::text::TextItem::size
#[comemo::memoize]
pub fn glyph_frame(font: &FontInstance, glyph_id: u16) -> Option<GlyphFrame> {
    let upem = Abs::pt(font.units_per_em());
    let gid = GlyphId::from(glyph_id);

    if let Some(frame) = draw_glyph(font, upem, gid) {
        return Some(frame);
    }

    // Generate a fallback tofu if the glyph couldn't be drawn, unless it is
    // the space glyph. Then, an empty frame does the job. (This happens for
    // some rare CBDT fonts, which don't define a bitmap for the space, but
    // also don't have a glyf or CFF table.)
    let not_space = font.skrifa().charmap().map(' ') != Some(gid);
    not_space.then(|| draw_fallback_tofu(font, upem, gid))
}

/// Tries to draw a glyph.
fn draw_glyph(font: &FontInstance, upem: Abs, gid: GlyphId) -> Option<GlyphFrame> {
    let font_ref = font.skrifa();
    let png_bitmap = font_ref
        .bitmap_strikes()
        .glyph_for_size(SkrifaSize::unscaled(), gid)
        .filter(|glyph| matches!(glyph.data, BitmapData::Png(_)));

    let kind = if let Some(bitmap_glyph) = png_bitmap {
        draw_raster_glyph(font, upem, bitmap_glyph)
    } else if font_ref.color_glyphs().get(gid).is_some() {
        draw_colr_glyph(font, gid)
    } else if glyph_svg_data(font, gid).is_some() {
        draw_svg_glyph(font, gid)
    } else {
        None
    };

    kind.map(|item| GlyphFrame { upem, item })
}

/// Draws a fallback tofu box with the advance width of the glyph.
fn draw_fallback_tofu(font: &FontInstance, upem: Abs, gid: GlyphId) -> GlyphFrame {
    let advance = font
        .x_advance(gid.to_u32() as u16)
        .map(|advance| advance.at(upem))
        .unwrap_or(upem / 3.0);
    let inset = 0.15 * advance;
    let height = 0.7 * upem;
    let pos = TypstPoint::new(inset, upem - height);
    let size = Size::new(advance - inset * 2.0, height);
    let thickness = upem / 20.0;
    let stroke = FixedStroke { thickness, ..Default::default() };
    let shape = Geometry::Rect(size).stroked(stroke);
    GlyphFrame { upem, item: GlyphFrameItem::Tofu(pos, shape) }
}

/// Draws a raster glyph in a frame.
///
/// Supports only PNG images.
fn draw_raster_glyph(
    font: &FontInstance,
    upem: Abs,
    bitmap_glyph: skrifa::bitmap::BitmapGlyph,
) -> Option<GlyphFrameItem> {
    let BitmapData::Png(bytes) = bitmap_glyph.data else {
        return None;
    };
    let data = Bytes::new(bytes.to_vec());
    let image = Image::plain(RasterImage::plain(data, ExchangeFormat::Png).ok()?);

    let scale = upem / bitmap_glyph.ppem_x as f64;
    let image_width = scale * image.width();
    let image_height = scale * image.height();

    let x_offset = scale * bitmap_glyph.inner_bearing_x as f64;
    let mut y_offset = scale * bitmap_glyph.inner_bearing_y as f64;
    // Apple Color emoji doesn't provide offset information (or at least
    // not in a way ttf-parser understands), so we artificially shift their
    // baseline to make it look good.
    if font.info().family.to_lowercase() == "apple color emoji" {
        // This factor is just taken from krilla.
        y_offset -= 0.128 * upem;
    }

    // `skrifa` reports the inner bearing relative to the bitmap's placement
    // origin, unlike `ttf-parser` which always normalized to a bottom-left
    // origin. Honor the origin so the image sits on the baseline correctly:
    // for a top-left origin (e.g. CBDT/EBDT), the bearing already points at the
    // image's top edge, so we must not add the image height again.
    let position = match bitmap_glyph.placement_origin {
        skrifa::bitmap::Origin::TopLeft => TypstPoint::new(-x_offset, -y_offset),
        skrifa::bitmap::Origin::BottomLeft => {
            TypstPoint::new(-x_offset, -(image_height + y_offset))
        }
    };
    let size = Size::new(image_width, image_height);
    Some(GlyphFrameItem::Image(position, image, size))
}

/// Draws a glyph from the COLR table into the frame.
fn draw_colr_glyph(font: &FontInstance, gid: GlyphId) -> Option<GlyphFrameItem> {
    let svg_string = colr_glyph_to_svg(font, gid)?;

    let bbox = global_bounding_box(font)?;
    let width = (bbox.x_max - bbox.x_min) as f64;
    let height = (bbox.y_max - bbox.y_min) as f64;
    let x_min = bbox.x_min as f64;
    let y_max = bbox.y_max as f64;

    let data = Bytes::from_string(svg_string);
    let image = Image::plain(SvgImage::new(data).ok()?);

    let position = TypstPoint::new(Abs::pt(x_min), Abs::pt(-y_max));
    let size = Size::new(Abs::pt(width), Abs::pt(height));
    Some(GlyphFrameItem::Image(position, image, size))
}

/// The font's global bounding box (union of all glyph extents), used to size
/// the SVG viewport for COLR glyphs.
fn global_bounding_box(font: &FontInstance) -> Option<BoundingBox<f32>> {
    font.skrifa().metrics(SkrifaSize::unscaled(), font.location()).bounds
}

/// Convert a COLR glyph into an SVG file.
fn colr_glyph_to_svg(font: &FontInstance, gid: GlyphId) -> Option<String> {
    let mut svg = XmlWriter::new(xmlwriter::Options::default());

    let bbox = global_bounding_box(font)?;
    let width = (bbox.x_max - bbox.x_min) as f64;
    let height = (bbox.y_max - bbox.y_min) as f64;
    let x_min = bbox.x_min as f64;
    let y_max = bbox.y_max as f64;
    let tx = -x_min;
    let ty = -y_max;

    svg.start_element("svg");
    svg.write_attribute("xmlns", "http://www.w3.org/2000/svg");
    svg.write_attribute("xmlns:xlink", "http://www.w3.org/1999/xlink");
    svg.write_attribute("width", &width);
    svg.write_attribute("height", &height);
    svg.write_attribute_fmt("viewBox", format_args!("0 0 {width} {height}"));

    let mut path_buf = String::with_capacity(256);

    svg.start_element("g");
    svg.write_attribute_fmt(
        "transform",
        format_args!("matrix(1 0 0 -1 0 0) matrix(1 0 0 1 {tx} {ty})"),
    );

    let font_ref = font.skrifa();
    let color_glyph = font_ref.color_glyphs().get(gid)?;
    let mut glyph_painter = GlyphPainter {
        font: font_ref,
        location: font.location(),
        svg: &mut svg,
        path_buf: &mut path_buf,
        gradient_index: 1,
        clip_path_index: 1,
        transforms_stack: vec![Transform::default()],
        clip_transforms_stack: Vec::new(),
        foreground: RgbaColor::new(0, 0, 0, 255),
    };

    color_glyph.paint(font.location(), &mut glyph_painter).ok()?;
    svg.end_element();

    Some(svg.end_document())
}

/// Draws an SVG glyph in a frame.
fn draw_svg_glyph(font: &FontInstance, gid: GlyphId) -> Option<GlyphFrameItem> {
    // TODO: Our current conversion of the SVG table works for Twitter Color Emoji,
    // but might not work for others. See also: https://github.com/RazrFalcon/resvg/pull/776
    let mut data = glyph_svg_data(font, gid)?;

    // Decompress SVGZ.
    let mut decoded = vec![];
    if data.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(data);
        decoder.read_to_end(&mut decoded).ok()?;
        data = &decoded;
    }

    // Parse and simplify the SVG.
    let xml = std::str::from_utf8(data).ok()?;
    let document = roxmltree::Document::parse(xml).ok()?;
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_xmltree(&document, &opts).ok()?;
    let mut data = tree.to_string(&usvg::WriteOptions {
        indent: usvg::Indent::None,
        attributes_indent: usvg::Indent::None,
        ..Default::default()
    });

    // The SVG coordinates and the font coordinates are not the same: the Y axis
    // is mirrored. But the origin of the axes are the same (which means that
    // the horizontal axis in the SVG document corresponds to the baseline). See
    // the reference for more details:
    // https://learn.microsoft.com/en-us/typography/opentype/spec/svg#coordinate-systems-and-glyph-metrics
    //
    // Using this SVG directly can result in a cropped glyph. In order to avoid
    // clipping issues, we apply a translate transform so the top-left corner of
    // the bounding box is moved into the origin (0, 0) to make it fully fit
    // into the view port defined by `viewBox="0 0 width height"`, like a
    // conventional SVG.
    let bbox = tree.root().bounding_box();
    let view_box = Rect::new(
        TypstPoint::new(Abs::pt(bbox.left() as f64), Abs::pt(bbox.top() as f64)),
        TypstPoint::new(Abs::pt(bbox.right() as f64), Abs::pt(bbox.bottom() as f64)),
    );
    fixup_svg(&mut data, view_box);

    let data = Bytes::from_string(data);
    let image = Image::plain(SvgImage::new(data).ok()?);

    let position = TypstPoint::new(view_box.min.x, view_box.min.y);
    let size = view_box.size();
    Some(GlyphFrameItem::Image(position, image, size))
}

/// Replace or insert the size attributes (viewBox, width and height), and
/// insert a group with a transform that translates the `viewBox` so that the
/// top-left point is at the origin (0, 0).
fn fixup_svg(svg: &mut String, view_box: Rect) {
    let mut viewbox_range = None;
    let mut width_range = None;
    let mut height_range = None;

    let mut s = unscanny::Scanner::new(svg);
    s.eat_until("<svg");
    s.expect("<svg");

    let svg_attr_start = s.cursor();

    while !s.eat_if('>') && !s.done() {
        s.eat_whitespace();
        let start = s.cursor();

        let attr_name = s.eat_until('=').trim();
        // Eat the equal sign and the quote.
        s.expect('=');
        s.eat_until('"');
        s.expect('"');

        while !s.eat_if('"') && !s.done() {
            s.eat();
        }

        match attr_name {
            "viewBox" => viewbox_range = Some(start..s.cursor()),
            "width" => width_range = Some(start..s.cursor()),
            "height" => height_range = Some(start..s.cursor()),
            _ => {}
        }
    }

    let svg_body_start = s.cursor();
    let Some(svg_body_end) = svg.rfind("</svg>") else {
        return;
    };

    svg.insert_str(svg_body_end, "</g>");
    svg.insert_str(
        svg_body_start,
        &format!(
            r#"<g transform="translate({} {})">"#,
            -view_box.min.x.to_pt(),
            -view_box.min.y.to_pt()
        ),
    );

    let size = view_box.size();
    let mut edits = [
        (
            viewbox_range,
            format!("viewBox=\"0 0 {} {}\"", size.x.to_pt(), size.y.to_pt(),),
        ),
        (width_range, format!("width=\"{}\"", size.x.to_pt())),
        (height_range, format!("height=\"{}\"", size.y.to_pt())),
    ];

    // Sort edits by ranges; missing ranges will be moved to the start.
    edits.sort_by_key(|(range, _)| range.clone().map(|r| r.start));

    // Replace or insert the attribute. Iterate in reverse, so the modifying the
    // string doesn't affect the ranges of the edits that are applied later on.
    for (range, str) in edits.into_iter().rev() {
        if let Some(range) = range {
            svg.replace_range(range, &str);
        } else {
            svg.insert_str(svg_attr_start, &str);
            svg.insert(svg_attr_start, ' ');
        }
    }
}

// NOTE: This is only a best-effort translation of COLR into SVG. It's not feature-complete
// and it's also not possible to make it feature-complete using just raw SVG features.
//
// Unlike the previous ttf-parser implementation (which received already-resolved
// colors and concrete gradient coordinates), skrifa's `ColorPainter` callbacks
// hand us unresolved palette indices and normalized gradient geometry, which we
// resolve against the font's `CPAL` palette and translate into SVG here.
struct GlyphPainter<'a, 'b> {
    /// The font, used for outline (clip) and palette lookups.
    font: &'b skrifa::FontRef<'a>,
    /// The variation coordinates for outline and palette resolution.
    location: LocationRef<'b>,
    svg: &'b mut xmlwriter::XmlWriter,
    path_buf: &'b mut String,
    gradient_index: usize,
    clip_path_index: usize,
    /// The accumulated transform stack. The last entry is the current
    /// painter-space transform.
    transforms_stack: Vec<Transform>,
    /// The transform that was in effect when each currently-active clip was
    /// pushed. The last entry is the transform of the innermost clip, which is
    /// the coordinate space the fill geometry (`path_buf`) lives in.
    clip_transforms_stack: Vec<Transform>,
    /// The text/foreground color, used for the `0xFFFF` palette sentinel.
    foreground: RgbaColor,
}

impl GlyphPainter<'_, '_> {
    /// The current painter-space transform (top of the stack).
    fn current_transform(&self) -> Transform {
        self.transforms_stack.last().copied().unwrap_or_default()
    }

    /// The transform of the innermost active clip, i.e. the coordinate space
    /// the fill geometry in `path_buf` is expressed in.
    fn clip_transform(&self) -> Transform {
        self.clip_transforms_stack.last().copied().unwrap_or_default()
    }

    /// Resolve a palette index and additional alpha multiplier into an RGBA
    /// color. Palette index `0xFFFF` is the foreground/text color sentinel.
    fn resolve_color(&self, palette_index: u16, alpha: f32) -> RgbaColor {
        let base = if palette_index == 0xFFFF {
            self.foreground
        } else {
            self.font
                .color_palettes()
                .get(0)
                .and_then(|palette| {
                    palette
                        .colors()
                        .get(usize::from(palette_index))
                        .map(|c| RgbaColor::new(c.red, c.green, c.blue, c.alpha))
                })
                .unwrap_or(self.foreground)
        };
        // Multiply the palette color's own alpha with the stop/solid alpha.
        let combined = (f32::from(base.alpha) / 255.0) * alpha;
        let combined = (combined.clamp(0.0, 1.0) * 255.0).round() as u8;
        RgbaColor::new(base.red, base.green, base.blue, combined)
    }

    fn write_gradient_stops(&mut self, stops: &[ColorStop]) {
        for stop in stops {
            let color = self.resolve_color(stop.palette_index, stop.alpha);
            self.svg.start_element("stop");
            self.svg.write_attribute("offset", &stop.offset);
            self.write_color_attribute("stop-color", color);
            let opacity = f32::from(color.alpha) / 255.0;
            self.svg.write_attribute("stop-opacity", &opacity);
            self.svg.end_element();
        }
    }

    fn write_color_attribute(&mut self, name: &str, color: RgbaColor) {
        self.svg.write_attribute_fmt(
            name,
            format_args!("rgb({}, {}, {})", color.red, color.green, color.blue),
        );
    }

    fn write_transform_attribute(&mut self, name: &str, ts: Transform) {
        if ts == Transform::default() {
            return;
        }

        self.svg.write_attribute_fmt(
            name,
            format_args!(
                "matrix({} {} {} {} {} {})",
                ts.xx, ts.yx, ts.xy, ts.yy, ts.dx, ts.dy
            ),
        );
    }

    fn write_spread_method_attribute(&mut self, extend: Extend) {
        self.svg.write_attribute(
            "spreadMethod",
            match extend {
                Extend::Pad => &"pad",
                Extend::Repeat => &"repeat",
                Extend::Reflect => &"reflect",
                // Unknown values fall back to the default (pad).
                _ => &"pad",
            },
        );
    }

    fn paint_solid(&mut self, color: RgbaColor) {
        self.svg.start_element("path");
        self.write_color_attribute("fill", color);
        let opacity = f32::from(color.alpha) / 255.0;
        self.svg.write_attribute("fill-opacity", &opacity);
        self.write_transform_attribute("transform", self.clip_transform());
        self.svg.write_attribute("d", self.path_buf);
        self.svg.end_element();
    }

    fn paint_linear_gradient(
        &mut self,
        p0: Point<f32>,
        p1: Point<f32>,
        color_stops: &[ColorStop],
        extend: Extend,
    ) {
        let gradient_id = format!("lg{}", self.gradient_index);
        self.gradient_index += 1;

        // skrifa already normalizes the linear gradient down to a `p0` -> `p1`
        // segment (folding in the P2 rotation, fixing the previous "we ignore
        // x2, y2" gap), so we can emit the endpoints directly. The gradient and
        // the fill path live in the same local space; the fill path carries the
        // clip transform, so the gradient only needs the brush delta relative to
        // it as its `gradientTransform`.
        let gradient_transform =
            paint_transform(self.clip_transform(), self.current_transform());

        // TODO: The way spreadMode works in COLR and SVG is a bit different. In
        // SVG, the spreadMode will always be applied based on x1/y1 and x2/y2.
        // However, in COLR the spreadMode will be applied from the first/last
        // stop. So if we have a gradient with x1=0 x2=1, and a stop at x=0.4 and
        // x=0.6, then in SVG we will always see a padding, while in COLR we will
        // see the actual spreadMode. We need to account for that somehow.
        self.svg.start_element("linearGradient");
        self.svg.write_attribute("id", &gradient_id);
        self.svg.write_attribute("x1", &p0.x);
        self.svg.write_attribute("y1", &p0.y);
        self.svg.write_attribute("x2", &p1.x);
        self.svg.write_attribute("y2", &p1.y);
        self.svg.write_attribute("gradientUnits", &"userSpaceOnUse");
        self.write_spread_method_attribute(extend);
        self.write_transform_attribute("gradientTransform", gradient_transform);
        self.write_gradient_stops(color_stops);
        self.svg.end_element();

        self.svg.start_element("path");
        self.svg
            .write_attribute_fmt("fill", format_args!("url(#{gradient_id})"));
        self.write_transform_attribute("transform", self.clip_transform());
        self.svg.write_attribute("d", self.path_buf);
        self.svg.end_element();
    }

    fn paint_radial_gradient(
        &mut self,
        c0: Point<f32>,
        r0: f32,
        c1: Point<f32>,
        r1: f32,
        color_stops: &[ColorStop],
        extend: Extend,
    ) {
        let gradient_id = format!("rg{}", self.gradient_index);
        self.gradient_index += 1;

        let gradient_transform =
            paint_transform(self.clip_transform(), self.current_transform());

        // skrifa names `c0`/`r0` the focal (inner) circle and `c1`/`r1` the
        // outer circle. SVG radii must be non-negative, so truncate to zero.
        self.svg.start_element("radialGradient");
        self.svg.write_attribute("id", &gradient_id);
        self.svg.write_attribute("cx", &c1.x);
        self.svg.write_attribute("cy", &c1.y);
        self.svg.write_attribute("r", &r1.max(0.0));
        self.svg.write_attribute("fr", &r0.max(0.0));
        self.svg.write_attribute("fx", &c0.x);
        self.svg.write_attribute("fy", &c0.y);
        self.svg.write_attribute("gradientUnits", &"userSpaceOnUse");
        self.write_spread_method_attribute(extend);
        self.write_transform_attribute("gradientTransform", gradient_transform);
        self.write_gradient_stops(color_stops);
        self.svg.end_element();

        self.svg.start_element("path");
        self.svg
            .write_attribute_fmt("fill", format_args!("url(#{gradient_id})"));
        self.write_transform_attribute("transform", self.clip_transform());
        self.svg.write_attribute("d", self.path_buf);
        self.svg.end_element();
    }

    /// Draws the outline of `gid` into `path_buf` as an SVG path in glyph-local
    /// space (the painter transform is applied separately via SVG attributes).
    fn outline_into_path_buf(&mut self, gid: GlyphId) {
        self.path_buf.clear();
        let Some(outline) = self.font.outline_glyphs().get(gid) else {
            return;
        };
        let mut pen = SvgPen::new();
        if outline
            .draw(DrawSettings::unhinted(SkrifaSize::unscaled(), self.location), &mut pen)
            .is_err()
        {
            return;
        }
        self.path_buf.push_str(&pen);
    }

    fn clip_with_path(&mut self, path: &str) {
        let clip_id = format!("cp{}", self.clip_path_index);
        self.clip_path_index += 1;

        // The transform that is in effect when the clip is established is the
        // coordinate space both the clip and the subsequent fill geometry live
        // in. Remember it so fills can reproduce it.
        let transform = self.current_transform();
        self.clip_transforms_stack.push(transform);

        self.svg.start_element("clipPath");
        self.svg.write_attribute("id", &clip_id);
        self.svg.start_element("path");
        self.write_transform_attribute("transform", transform);
        self.svg.write_attribute("d", &path);
        self.svg.end_element();
        self.svg.end_element();

        self.svg.start_element("g");
        self.svg
            .write_attribute_fmt("clip-path", format_args!("url(#{clip_id})"));
    }
}

impl ColorPainter for GlyphPainter<'_, '_> {
    fn push_transform(&mut self, transform: Transform) {
        // Concatenate onto the current transform (`Matrix`'s `Mul` matches the
        // semantics of `ttf_parser::Transform::combine`).
        let combined = self.current_transform() * transform;
        self.transforms_stack.push(combined);
    }

    fn pop_transform(&mut self) {
        self.transforms_stack.pop();
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        self.outline_into_path_buf(glyph_id);
        let path = std::mem::take(self.path_buf);
        self.clip_with_path(&path);
        *self.path_buf = path;
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let x_min = clip_box.x_min;
        let x_max = clip_box.x_max;
        let y_min = clip_box.y_min;
        let y_max = clip_box.y_max;

        let clip_path = format!(
            "M {x_min} {y_min} L {x_max} {y_min} L {x_max} {y_max} L {x_min} {y_max} Z"
        );

        self.clip_with_path(&clip_path);
    }

    fn pop_clip(&mut self) {
        self.clip_transforms_stack.pop();
        self.svg.end_element(); // g
    }

    fn fill(&mut self, brush: Brush<'_>) {
        // Fills target the current clip region, which is emitted as an enclosing
        // `<g clip-path=...>` group. The fill geometry is the outline currently
        // in `path_buf` (set by the preceding `push_clip_glyph`), drawn in the
        // clip's coordinate space, matching the previous behavior.
        match brush {
            Brush::Solid { palette_index, alpha } => {
                let color = self.resolve_color(palette_index, alpha);
                self.paint_solid(color);
            }
            Brush::LinearGradient { p0, p1, color_stops, extend } => {
                self.paint_linear_gradient(p0, p1, color_stops, extend);
            }
            Brush::RadialGradient { c0, r0, c1, r1, color_stops, extend } => {
                self.paint_radial_gradient(c0, r0, c1, r1, color_stops, extend);
            }
            // SVG has no conic/sweep gradient, so this is a no-op (matching the
            // previous best-effort behavior).
            Brush::SweepGradient { .. } => {}
        }
    }

    fn push_layer(&mut self, composite_mode: CompositeMode) {
        self.svg.start_element("g");

        // TODO: Need to figure out how to represent the other blend modes
        // in SVG. The separable Porter-Duff modes cannot be expressed via SVG
        // `mix-blend-mode` and fall back to "normal" (same best-effort caveat as
        // before).
        let mode = match composite_mode {
            CompositeMode::SrcOver => "normal",
            CompositeMode::Screen => "screen",
            CompositeMode::Overlay => "overlay",
            CompositeMode::Darken => "darken",
            CompositeMode::Lighten => "lighten",
            CompositeMode::ColorDodge => "color-dodge",
            CompositeMode::ColorBurn => "color-burn",
            CompositeMode::HardLight => "hard-light",
            CompositeMode::SoftLight => "soft-light",
            CompositeMode::Difference => "difference",
            CompositeMode::Exclusion => "exclusion",
            CompositeMode::Multiply => "multiply",
            CompositeMode::HslHue => "hue",
            CompositeMode::HslSaturation => "saturation",
            CompositeMode::HslColor => "color",
            CompositeMode::HslLuminosity => "luminosity",
            // Clear/Src/Dest/SrcIn/DestIn/SrcOut/DestOut/SrcAtop/DestAtop/
            // Xor/Plus and any unknown values cannot be represented; fall back
            // to "normal".
            _ => "normal",
        };
        self.svg.write_attribute_fmt(
            "style",
            format_args!("mix-blend-mode: {mode}; isolation: isolate"),
        );
    }

    fn pop_layer(&mut self) {
        self.svg.end_element(); // g
    }
}

/// Compute the brush-space transform for a gradient relative to its fill path.
///
/// The fill path carries `clip_transform` as its SVG `transform`. Since SVG
/// applies the referencing element's transform to a `userSpaceOnUse` gradient
/// too, the gradient must compensate so that its effective space equals the
/// current painter transform: `gradientTransform = inverse(clip) * current`.
///
/// This mirrors the inversion the previous ttf-parser implementation performed
/// (via `tiny_skia`), preserving identical numerics.
fn paint_transform(clip_transform: Transform, current_transform: Transform) -> Transform {
    let clip = tiny_skia_path::Transform::from_row(
        clip_transform.xx,
        clip_transform.yx,
        clip_transform.xy,
        clip_transform.yy,
        clip_transform.dx,
        clip_transform.dy,
    );

    let current = tiny_skia_path::Transform::from_row(
        current_transform.xx,
        current_transform.yx,
        current_transform.xy,
        current_transform.yy,
        current_transform.dx,
        current_transform.dy,
    );

    let gradient_transform = clip
        .invert()
        // In theory, we should error out. But the transform shouldn't ever be
        // uninvertible, so let's ignore it.
        .unwrap_or_default()
        .pre_concat(current);

    Transform {
        xx: gradient_transform.sx,
        yx: gradient_transform.ky,
        xy: gradient_transform.kx,
        yy: gradient_transform.sy,
        dx: gradient_transform.tx,
        dy: gradient_transform.ty,
    }
}

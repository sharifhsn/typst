//! DrawingML geometry, fill, and stroke primitives shared by Office exporters.

use crate::color;
use crate::units;
use crate::xml::XmlWriter;
use typst_library::layout::{Abs, Point, Size, Transform};
use typst_library::visualize::{
    Color, Curve, CurveItem, FixedStroke, Geometry, Gradient, LineCap, Paint,
};

/// One custom geometry path segment.
pub enum PathSegment {
    MoveTo(i64, i64),
    LineTo(i64, i64),
    CubicTo(i64, i64, i64, i64, i64, i64),
    Close,
}

/// A fill specification. Colors are straight sRGB + alpha (`[r, g, b, a]`).
pub enum FillSpec {
    Solid([u8; 4]),
    LinearGradient { angle_60k: i32, stops: Vec<GradientStop> },
}

/// A gradient stop.
pub struct GradientStop {
    pub pos_100k: i32,
    pub color: [u8; 4],
}

/// A stroke specification.
pub struct StrokeSpec {
    pub color: [u8; 4],
    pub w_emu: i64,
    pub cap: &'static str,
    pub dash: Option<&'static str>,
}

/// Whether lowering should preserve source alpha or force opaque colors.
#[derive(Copy, Clone)]
pub enum AlphaMode {
    Preserve,
    Opaque,
}

fn srgb_rgba(color: &Color, mode: AlphaMode) -> [u8; 4] {
    let mut rgba = color::srgb_rgba(color);
    if matches!(mode, AlphaMode::Opaque) {
        rgba[3] = 255;
    }
    rgba
}

/// Lowers a Typst fill to a DrawingML fill.
pub fn resolved_fill(fill: &Option<Paint>, alpha: AlphaMode) -> Option<Option<FillSpec>> {
    match fill {
        None => Some(None),
        Some(Paint::Solid(color)) => Some(Some(FillSpec::Solid(srgb_rgba(color, alpha)))),
        Some(Paint::Gradient(gradient)) => {
            linear_gradient_fill(gradient, alpha).map(Some)
        }
        Some(Paint::Tiling(_)) => None,
    }
}

/// Lowers a Typst linear gradient to a DrawingML fill.
pub fn linear_gradient_fill(gradient: &Gradient, alpha: AlphaMode) -> Option<FillSpec> {
    let Gradient::Linear(linear) = gradient else { return None };
    let stops = linear
        .stops
        .iter()
        .map(|(color, pos)| GradientStop {
            pos_100k: (pos.get() * 100_000.0).round() as i32,
            color: srgb_rgba(color, alpha),
        })
        .collect();
    let angle_60k = (linear.angle.to_deg().rem_euclid(360.0) * 60_000.0).round() as i32;
    Some(FillSpec::LinearGradient { angle_60k, stops })
}

/// Lowers a resolved Typst stroke to a DrawingML stroke.
pub fn resolved_stroke(
    stroke: &Option<FixedStroke>,
    scale: f64,
    alpha: AlphaMode,
) -> Option<Option<StrokeSpec>> {
    match stroke {
        None => Some(None),
        Some(fixed) => match &fixed.paint {
            Paint::Solid(color) => {
                let thickness = fixed.thickness * scale;
                Some(Some(StrokeSpec {
                    color: srgb_rgba(color, alpha),
                    w_emu: units::abs_to_emu(thickness),
                    cap: line_cap_to_ooxml(fixed.cap),
                    dash: fixed
                        .dash
                        .as_ref()
                        .map(|dash| prst_dash(&dash.array, thickness)),
                }))
            }
            Paint::Gradient(_) | Paint::Tiling(_) => None,
        },
    }
}

/// Maps Typst's line caps to DrawingML `a:ln/@cap` values.
pub fn line_cap_to_ooxml(cap: LineCap) -> &'static str {
    match cap {
        LineCap::Butt => "flat",
        LineCap::Round => "rnd",
        LineCap::Square => "sq",
    }
}

/// Maps a resolved dash array to the closest DrawingML preset dash.
pub fn prst_dash(array: &[Abs], thickness: Abs) -> &'static str {
    if array.is_empty() {
        return "solid";
    }

    let (mut has_dot, mut has_dash) = (false, false);
    for on in array.iter().step_by(2) {
        if *on <= thickness * 1.2 {
            has_dot = true;
        } else {
            has_dash = true;
        }
    }

    match (has_dot, has_dash) {
        (true, true) => "dashDot",
        (true, false) => "sysDot",
        _ => "dash",
    }
}

/// A 2D affine transform's uniform scale factor, if it is a similarity.
pub fn similarity_scale(t: &Transform) -> Option<f64> {
    let (sx, ky, kx, sy) = (t.sx.get(), t.ky.get(), t.kx.get(), t.sy.get());
    let col1 = sx * sx + ky * ky;
    let col2 = kx * kx + sy * sy;
    let dot = sx * kx + ky * sy;
    const EPS: f64 = 1e-4;
    if col1 <= EPS || (col1 - col2).abs() > EPS * col1.max(col2) || dot.abs() > EPS * col1
    {
        return None;
    }
    Some(col1.sqrt())
}

/// Computes a PowerPoint `roundRect` adjustment value for a Typst radius.
pub fn round_rect_adj(radius: Abs, size: Size) -> i32 {
    let shorter = size.x.min(size.y).to_pt();
    if shorter <= 0.0 {
        return 0;
    }

    ((radius.to_pt() / shorter) * 100_000.0).round().clamp(0.0, 50_000.0) as i32
}

/// Detects the radius of a full-frame rounded rectangle clip.
pub fn rounded_rect_radius(clip: &Curve, size: Size) -> Option<Abs> {
    let [
        CurveItem::Move(m0),
        CurveItem::Cubic(c10, c20, e0),
        CurveItem::Line(l1),
        CurveItem::Cubic(c11, c21, e1),
        CurveItem::Line(l2),
        CurveItem::Cubic(c12, c22, e2),
        CurveItem::Line(l3),
        CurveItem::Cubic(c13, c23, e3),
        CurveItem::Close,
    ] = clip.0.as_slice()
    else {
        return None;
    };

    let w = size.x.to_pt();
    let h = size.y.to_pt();
    let r = m0.y.to_pt();
    const EPS: f64 = 0.01;
    if r <= EPS || r > w.min(h) / 2.0 + EPS || !near(m0.x.to_pt(), 0.0) {
        return None;
    }

    for (point, expected) in [
        (*m0, (0.0, r)),
        (*e0, (r, 0.0)),
        (*l1, (w - r, 0.0)),
        (*e1, (w, r)),
        (*l2, (w, h - r)),
        (*e2, (w - r, h)),
        (*l3, (r, h)),
        (*e3, (0.0, h - r)),
    ] {
        if !point_near(point, expected) {
            return None;
        }
    }

    for (c1, c2, start, end) in [
        (*c10, *c20, (0.0, r), (r, 0.0)),
        (*c11, *c21, (w - r, 0.0), (w, r)),
        (*c12, *c22, (w, h - r), (w - r, h)),
        (*c13, *c23, (r, h), (0.0, h - r)),
    ] {
        if !control_in_corner(c1, start, end) || !control_in_corner(c2, start, end) {
            return None;
        }
    }

    Some(Abs::pt(r.min(w.min(h) / 2.0)))
}

fn control_in_corner(c: Point, start: (f64, f64), end: (f64, f64)) -> bool {
    const PAD: f64 = 0.5;
    let (x, y) = (c.x.to_pt(), c.y.to_pt());
    (start.0.min(end.0) - PAD..=start.0.max(end.0) + PAD).contains(&x)
        && (start.1.min(end.1) - PAD..=start.1.max(end.1) + PAD).contains(&y)
}

fn point_near(point: Point, expected: (f64, f64)) -> bool {
    near(point.x.to_pt(), expected.0) && near(point.y.to_pt(), expected.1)
}

fn near(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 0.01
}

/// A curve/line command before shifting into non-negative custom-geometry space.
pub enum RawSeg {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

/// A normalized custom-geometry path plus its source-space bounds.
pub struct NormalizedPath {
    pub segments: Vec<PathSegment>,
    pub min_x: Abs,
    pub min_y: Abs,
    pub w: Abs,
    pub h: Abs,
}

/// Converts Typst geometry to raw path segments with `transform` applied.
pub fn geometry_to_raw(geometry: &Geometry, transform: Transform) -> Vec<RawSeg> {
    let at = |p: Point| p.transform(transform);
    match geometry {
        Geometry::Curve(curve) => raw_segments_from_curve(curve, transform),
        Geometry::Line(delta) => {
            vec![RawSeg::Move(at(Point::zero())), RawSeg::Line(at(*delta))]
        }
        Geometry::Rect(size) => vec![
            RawSeg::Move(at(Point::zero())),
            RawSeg::Line(at(Point::new(size.x, Abs::zero()))),
            RawSeg::Line(at(Point::new(size.x, size.y))),
            RawSeg::Line(at(Point::new(Abs::zero(), size.y))),
            RawSeg::Close,
        ],
    }
}

/// Converts a Typst curve to raw path segments with `transform` applied.
pub fn raw_segments_from_curve(curve: &Curve, transform: Transform) -> Vec<RawSeg> {
    curve
        .0
        .iter()
        .map(|item| match item {
            CurveItem::Move(p) => RawSeg::Move(p.transform(transform)),
            CurveItem::Line(p) => RawSeg::Line(p.transform(transform)),
            CurveItem::Cubic(c1, c2, end) => RawSeg::Cubic(
                c1.transform(transform),
                c2.transform(transform),
                end.transform(transform),
            ),
            CurveItem::Close => RawSeg::Close,
        })
        .collect()
}

/// Conservative control-point bounds for a raw path.
pub fn raw_bounds(raw: &[RawSeg]) -> (Abs, Abs, Abs, Abs) {
    let mut bounds: Option<(Abs, Abs, Abs, Abs)> = None;
    let mut expand = |p: Point| {
        bounds = Some(match bounds {
            None => (p.x, p.y, p.x, p.y),
            Some((min_x, min_y, max_x, max_y)) => {
                (min_x.min(p.x), min_y.min(p.y), max_x.max(p.x), max_y.max(p.y))
            }
        });
    };

    for seg in raw {
        match seg {
            RawSeg::Move(p) | RawSeg::Line(p) => expand(*p),
            RawSeg::Cubic(c1, c2, end) => {
                expand(*c1);
                expand(*c2);
                expand(*end);
            }
            RawSeg::Close => {}
        }
    }

    bounds.unwrap_or((Abs::zero(), Abs::zero(), Abs::zero(), Abs::zero()))
}

/// Shifts a raw path to non-negative custom-geometry coordinates.
pub fn normalize_segments(raw: Vec<RawSeg>) -> Option<NormalizedPath> {
    let (min_x, min_y, max_x, max_y) = raw_bounds(&raw);
    let (w, h) = (max_x - min_x, max_y - min_y);
    if !w.to_pt().is_finite() || !h.to_pt().is_finite() {
        return None;
    }
    if w.to_pt() <= 0.0 && h.to_pt() <= 0.0 {
        return None;
    }

    let shift = |p: Point| -> (i64, i64) {
        (units::abs_to_emu(p.x - min_x), units::abs_to_emu(p.y - min_y))
    };
    let segments = raw
        .into_iter()
        .map(|seg| match seg {
            RawSeg::Move(p) => {
                let (x, y) = shift(p);
                PathSegment::MoveTo(x, y)
            }
            RawSeg::Line(p) => {
                let (x, y) = shift(p);
                PathSegment::LineTo(x, y)
            }
            RawSeg::Cubic(c1, c2, end) => {
                let (c1x, c1y) = shift(c1);
                let (c2x, c2y) = shift(c2);
                let (ex, ey) = shift(end);
                PathSegment::CubicTo(c1x, c1y, c2x, c2y, ex, ey)
            }
            RawSeg::Close => PathSegment::Close,
        })
        .collect();

    Some(NormalizedPath { segments, min_x, min_y, w, h })
}

/// Emits an `a:prstGeom` child with an empty adjust-list.
pub fn write_prst_geom(w: &mut XmlWriter, prst: &'static str) {
    w.open("a:prstGeom").attr("prst", prst).start_children();
    w.leaf("a:avLst");
    w.close();
}

/// Emits an `a:prstGeom` child with a single `adj` guide.
pub fn write_prst_geom_with_adj(w: &mut XmlWriter, prst: &'static str, adj: i32) {
    w.open("a:prstGeom").attr("prst", prst).start_children();
    w.open("a:avLst").start_children();
    w.open("a:gd")
        .attr("name", "adj")
        .attr("fmla", &format!("val {}", adj.clamp(0, 50_000)))
        .empty();
    w.close();
    w.close();
}

/// Emits an `a:custGeom` child whose path coordinate space equals the extent.
pub fn write_custom_geom(
    w: &mut XmlWriter,
    segments: &[PathSegment],
    w_emu: i64,
    h_emu: i64,
) {
    let cx = w_emu.to_string();
    let cy = h_emu.to_string();
    w.open("a:custGeom").start_children();
    w.leaf("a:avLst");
    w.leaf("a:gdLst");
    w.leaf("a:ahLst");
    w.leaf("a:cxnLst");
    w.open("a:rect")
        .attr("l", "0")
        .attr("t", "0")
        .attr("r", &cx)
        .attr("b", &cy)
        .empty();
    w.open("a:pathLst").start_children();
    w.open("a:path").attr("w", &cx).attr("h", &cy).start_children();
    for segment in segments {
        match *segment {
            PathSegment::MoveTo(x, y) => {
                w.open("a:moveTo").start_children();
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::LineTo(x, y) => {
                w.open("a:lnTo").start_children();
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::CubicTo(x1, y1, x2, y2, x, y) => {
                w.open("a:cubicBezTo").start_children();
                write_pt(w, x1, y1);
                write_pt(w, x2, y2);
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::Close => w.leaf("a:close"),
        }
    }
    w.close();
    w.close();
    w.close();
}

fn write_pt(w: &mut XmlWriter, x: i64, y: i64) {
    w.open("a:pt")
        .attr("x", &x.to_string())
        .attr("y", &y.to_string())
        .empty();
}

/// Emits an `a:solidFill`, `a:gradFill`, or `a:noFill` child.
pub fn write_fill(
    w: &mut XmlWriter,
    fill: Option<&FillSpec>,
    gradient_scaled: &'static str,
) {
    match fill {
        Some(FillSpec::Solid(rgba)) => write_solid_fill(w, *rgba),
        Some(FillSpec::LinearGradient { angle_60k, stops }) => {
            w.open("a:gradFill").attr("rotWithShape", "1").start_children();
            w.open("a:gsLst").start_children();
            for stop in stops {
                w.open("a:gs")
                    .attr("pos", &stop.pos_100k.to_string())
                    .start_children();
                write_srgb(w, stop.color);
                w.close();
            }
            w.close();
            w.open("a:lin")
                .attr("ang", &angle_60k.to_string())
                .attr("scaled", gradient_scaled)
                .empty();
            w.close();
        }
        None => w.leaf("a:noFill"),
    }
}

/// Emits an `a:ln` child. `clamp_width` preserves callers with pre-existing
/// non-negative-width behavior without changing callers that format verbatim.
pub fn write_stroke(w: &mut XmlWriter, stroke: Option<&StrokeSpec>, clamp_width: bool) {
    match stroke {
        Some(stroke) => {
            let width = if clamp_width { stroke.w_emu.max(0) } else { stroke.w_emu };
            w.open("a:ln")
                .attr("w", &width.to_string())
                .attr("cap", stroke.cap)
                .start_children();
            write_solid_fill(w, stroke.color);
            if let Some(dash) = stroke.dash {
                w.open("a:prstDash").attr("val", dash).empty();
            }
            w.close();
        }
        None => {
            w.open("a:ln").start_children();
            w.leaf("a:noFill");
            w.close();
        }
    }
}

/// Emits an `a:solidFill` child around an `a:srgbClr`.
pub fn write_solid_fill(w: &mut XmlWriter, rgba: [u8; 4]) {
    w.open("a:solidFill").start_children();
    write_srgb(w, rgba);
    w.close();
}

/// Emits an `a:srgbClr` child, including alpha when not opaque.
pub fn write_srgb(w: &mut XmlWriter, rgba: [u8; 4]) {
    let [r, g, b, a] = rgba;
    if a == 255 {
        w.open("a:srgbClr").attr("val", &color::hex_rgb([r, g, b])).empty();
    } else {
        w.open("a:srgbClr")
            .attr("val", &color::hex_rgb([r, g, b]))
            .start_children();
        w.open("a:alpha")
            .attr("val", &color::alpha_to_100k(a).to_string())
            .empty();
        w.close();
    }
}

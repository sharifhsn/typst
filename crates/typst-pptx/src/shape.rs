#![allow(dead_code)]

use crate::dom::{
    FillSpec, GeomShape, GradientStop, PathGeom, PathSegment, PicGeom, StrokeSpec,
};

use typst_library::layout::{Abs, Point, Size, Transform};
use typst_library::visualize::{
    Color, Curve, CurveItem, FixedStroke, Geometry, Gradient, LineCap, Paint, Shape,
};
use typst_ooxml_core::color as ooxml_color;

/// Lower one laid-out Typst shape to the PPTX slide IR.
///
/// The transform is expected to be the frame-walk transform for this item. It
/// may translate, rotate, reflect, or uniformly scale the geometry; skew and
/// non-uniform scale return `None` so the caller can rasterize instead.
pub(crate) fn shape_to_geom(
    shape: &Shape,
    transform: Transform,
    rot_60k: i32,
) -> Option<GeomShape> {
    let scale = similarity_scale(&transform)?;
    let fill = resolved_fill(&shape.fill)?;
    let stroke = resolved_stroke(&shape.stroke, scale)?;
    let raw = geometry_to_raw(&shape.geometry, transform);
    let normalized = normalize_segments(raw)?;

    Some(GeomShape {
        x_emu: abs_to_emu(normalized.min_x),
        y_emu: abs_to_emu(normalized.min_y),
        w_emu: abs_to_emu(normalized.w).max(1),
        h_emu: abs_to_emu(normalized.h).max(1),
        rot_60k,
        geom: PathGeom::Custom(normalized.segments),
        fill,
        stroke,
    })
}

/// Classify a clip curve that can be represented as a preset picture geometry.
///
/// This is intentionally narrower than general shape lowering: it only accepts
/// the full-frame mask geometries that can be applied to the picture itself.
pub(crate) fn clip_to_pic_geom(clip: &Curve, size: Size) -> Option<PicGeom> {
    if !size.x.to_pt().is_finite()
        || !size.y.to_pt().is_finite()
        || size.x.to_pt() <= 0.0
        || size.y.to_pt() <= 0.0
    {
        return None;
    }

    if *clip == Curve::ellipse(size) {
        return Some(PicGeom::Ellipse);
    }

    let radius = rounded_rect_radius(clip, size)?;
    Some(PicGeom::RoundRect { adj_100k: round_rect_adj(radius, size) })
}

fn round_rect_adj(radius: Abs, size: Size) -> i32 {
    let shorter = size.x.min(size.y).to_pt();
    if shorter <= 0.0 {
        return 0;
    }

    ((radius.to_pt() / shorter) * 100_000.0).round().clamp(0.0, 50_000.0) as i32
}

fn rounded_rect_radius(clip: &Curve, size: Size) -> Option<Abs> {
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

    // The 8 endpoints above already pin a rounded rectangle of radius r, and
    // the segment pattern already forced the corners to be cubics (not chamfer
    // lines). Only sanity-check that each corner's control points bulge inside
    // that corner's box — i.e. a convex arc — without matching Typst's exact
    // bezier kappa, which PowerPoint's roundRect arc need not reproduce anyway.
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

/// Whether a corner-arc control point lies inside the box spanned by the arc's
/// two endpoints (padded), the signature of a convex quarter-arc.
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

/// A 2D affine transform's uniform scale factor, if it is a similarity:
/// translation, rotation/reflection, and optional uniform scale only.
pub(crate) fn similarity_scale(t: &Transform) -> Option<f64> {
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

fn geometry_to_raw(geometry: &Geometry, transform: Transform) -> Vec<RawSeg> {
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

fn raw_segments_from_curve(curve: &Curve, transform: Transform) -> Vec<RawSeg> {
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

pub(crate) fn resolved_fill(fill: &Option<Paint>) -> Option<Option<FillSpec>> {
    match fill {
        None => Some(None),
        Some(Paint::Solid(color)) => Some(Some(FillSpec::Solid(srgb_bytes(color)))),
        Some(Paint::Gradient(gradient)) => linear_gradient_fill(gradient).map(Some),
        Some(Paint::Tiling(_)) => None,
    }
}

fn linear_gradient_fill(gradient: &Gradient) -> Option<FillSpec> {
    let Gradient::Linear(linear) = gradient else { return None };
    let stops = linear
        .stops
        .iter()
        .map(|(color, pos)| GradientStop {
            pos_100k: (pos.get() * 100_000.0).round() as i32,
            color: srgb_bytes(color),
        })
        .collect();
    let angle_60k = (linear.angle.to_deg().rem_euclid(360.0) * 60_000.0).round() as i32;
    Some(FillSpec::LinearGradient { angle_60k, stops })
}

fn resolved_stroke(
    stroke: &Option<FixedStroke>,
    scale: f64,
) -> Option<Option<StrokeSpec>> {
    match stroke {
        None => Some(None),
        Some(fixed) => match &fixed.paint {
            Paint::Solid(color) => {
                let thickness = fixed.thickness * scale;
                Some(Some(StrokeSpec {
                    color: srgb_bytes(color),
                    w_emu: abs_to_emu(thickness),
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

pub(crate) fn srgb_bytes(color: &Color) -> [u8; 4] {
    ooxml_color::srgb_rgba(color)
}

fn line_cap_to_ooxml(cap: LineCap) -> &'static str {
    match cap {
        LineCap::Butt => "flat",
        LineCap::Round => "rnd",
        LineCap::Square => "sq",
    }
}

fn prst_dash(array: &[Abs], thickness: Abs) -> &'static str {
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

enum RawSeg {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

struct NormalizedPath {
    segments: Vec<PathSegment>,
    min_x: Abs,
    min_y: Abs,
    w: Abs,
    h: Abs,
}

/// Conservative control-point bounds for a raw path.
///
/// Seed from the first real coordinate, not the origin. Otherwise paths that
/// live away from `(0, 0)` acquire phantom padding when normalized.
fn raw_bounds(raw: &[RawSeg]) -> (Abs, Abs, Abs, Abs) {
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

fn normalize_segments(raw: Vec<RawSeg>) -> Option<NormalizedPath> {
    let (min_x, min_y, max_x, max_y) = raw_bounds(&raw);
    let (w, h) = (max_x - min_x, max_y - min_y);
    if !w.to_pt().is_finite() || !h.to_pt().is_finite() {
        return None;
    }
    if w.to_pt() <= 0.0 && h.to_pt() <= 0.0 {
        return None;
    }

    let shift =
        |p: Point| -> (i64, i64) { (abs_to_emu(p.x - min_x), abs_to_emu(p.y - min_y)) };
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

fn abs_to_emu(abs: Abs) -> i64 {
    let emu = (abs.to_pt() * 12_700.0).round();
    if emu.is_nan() {
        0
    } else if emu >= i64::MAX as f64 {
        i64::MAX
    } else if emu <= i64::MIN as f64 {
        i64::MIN
    } else {
        emu as i64
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use typst_library::foundations::Smart;
    use typst_library::layout::{Angle, Ratio, Size};
    use typst_library::visualize::{
        ColorSpace, FillRule, LinearGradient, Oklab, ProcessColor, ProcessColorSpace, Rgb,
    };

    use super::*;

    fn bare_shape(geometry: Geometry) -> Shape {
        Shape {
            geometry,
            fill: None,
            fill_rule: FillRule::default(),
            stroke: None,
        }
    }

    fn emu_pt(pt: f64) -> i64 {
        (pt * 12_700.0).round() as i64
    }

    #[test]
    fn rect_geometry_lowers_to_closed_four_corner_path() {
        let shape = bare_shape(Geometry::Rect(Size::new(Abs::pt(10.0), Abs::pt(20.0))));
        let geom = shape_to_geom(&shape, Transform::identity(), 0).unwrap();

        assert_eq!(geom.x_emu, 0);
        assert_eq!(geom.y_emu, 0);
        assert_eq!(geom.w_emu, emu_pt(10.0));
        assert_eq!(geom.h_emu, emu_pt(20.0));
        let PathGeom::Custom(segments) = geom.geom else {
            panic!("expected custom path");
        };
        assert_eq!(segments.len(), 5);
        assert!(matches!(segments[0], PathSegment::MoveTo(0, 0)));
        assert!(matches!(segments[1], PathSegment::LineTo(x, 0) if x == emu_pt(10.0)));
        assert!(
            matches!(segments[2], PathSegment::LineTo(x, y) if x == emu_pt(10.0) && y == emu_pt(20.0))
        );
        assert!(matches!(segments[3], PathSegment::LineTo(0, y) if y == emu_pt(20.0)));
        assert!(matches!(segments[4], PathSegment::Close));
    }

    #[test]
    fn curve_geometry_preserves_cubic_segment() {
        let mut curve = Curve::new();
        curve.move_(Point::zero());
        curve.cubic(
            Point::new(Abs::pt(5.0), Abs::pt(1.0)),
            Point::new(Abs::pt(10.0), Abs::pt(12.0)),
            Point::new(Abs::pt(20.0), Abs::pt(4.0)),
        );
        let shape = bare_shape(Geometry::Curve(curve));
        let geom = shape_to_geom(&shape, Transform::identity(), 0).unwrap();
        let PathGeom::Custom(segments) = geom.geom else {
            panic!("expected custom path");
        };

        assert!(matches!(segments[0], PathSegment::MoveTo(0, 0)));
        assert!(matches!(
            segments[1],
            PathSegment::CubicTo(c1x, c1y, c2x, c2y, ex, ey)
                if c1x == emu_pt(5.0)
                    && c1y == emu_pt(1.0)
                    && c2x == emu_pt(10.0)
                    && c2y == emu_pt(12.0)
                    && ex == emu_pt(20.0)
                    && ey == emu_pt(4.0)
        ));
    }

    #[test]
    fn negative_coordinates_shift_path_and_preserve_bounds() {
        let shape = bare_shape(Geometry::Line(Point::new(Abs::pt(5.0), Abs::pt(5.0))));
        let transform = Transform::translate(Abs::pt(-5.0), Abs::pt(-10.0));
        let geom = shape_to_geom(&shape, transform, 0).unwrap();

        assert_eq!(geom.x_emu, emu_pt(-5.0));
        assert_eq!(geom.y_emu, emu_pt(-10.0));
        assert_eq!(geom.w_emu, emu_pt(5.0));
        assert_eq!(geom.h_emu, emu_pt(5.0));
        let PathGeom::Custom(segments) = geom.geom else {
            panic!("expected custom path");
        };
        assert!(matches!(segments[0], PathSegment::MoveTo(0, 0)));
        assert!(
            matches!(segments[1], PathSegment::LineTo(x, y) if x == emu_pt(5.0) && y == emu_pt(5.0))
        );
    }

    #[test]
    fn linear_gradient_stops_are_converted_to_srgb() {
        let oklab = Color::Process(ProcessColor::Oklab(Oklab::new(0.7, 0.2, -0.1, 1.0)));
        let raw = oklab.to_vec4_u8();
        let gradient = Gradient::Linear(Arc::new(LinearGradient {
            stops: vec![
                (oklab.clone(), Ratio::zero()),
                (
                    Color::Process(ProcessColor::Rgb(Rgb::new(0.0, 0.0, 0.0, 1.0))),
                    Ratio::one(),
                ),
            ],
            angle: Angle::deg(30.0),
            space: ColorSpace::Process(ProcessColorSpace::Oklab),
            relative: Smart::Auto,
            anti_alias: true,
        }));

        let fill = resolved_fill(&Some(Paint::Gradient(gradient))).unwrap().unwrap();
        let FillSpec::LinearGradient { angle_60k, stops } = fill else {
            panic!("expected linear gradient");
        };

        assert_eq!(angle_60k, 30 * 60_000);
        assert_eq!(stops[0].pos_100k, 0);
        assert_eq!(stops[1].pos_100k, 100_000);
        assert_ne!(stops[0].color, raw);
        assert_eq!(stops[0].color, srgb_bytes(&oklab));
    }
}

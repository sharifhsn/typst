#![allow(dead_code)]

use crate::dom::{FillSpec, GeomShape, PathGeom, PicGeom};

use typst_library::layout::{Size, Transform};
use typst_library::visualize::{Color, Curve, Paint, Shape};
use typst_ooxml_core::dml::{self, AlphaMode};
use typst_ooxml_core::{color as ooxml_color, units};

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
    let scale = dml::similarity_scale(&transform)?;
    let fill = resolved_fill(&shape.fill)?;
    let stroke = dml::resolved_stroke(&shape.stroke, scale, AlphaMode::Preserve)?;
    let raw = dml::geometry_to_raw(&shape.geometry, transform);
    let normalized = dml::normalize_segments(raw)?;

    Some(GeomShape {
        x_emu: units::abs_to_emu(normalized.min_x),
        y_emu: units::abs_to_emu(normalized.min_y),
        w_emu: units::abs_to_emu(normalized.w).max(1),
        h_emu: units::abs_to_emu(normalized.h).max(1),
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

    let radius = dml::rounded_rect_radius(clip, size)?;
    Some(PicGeom::RoundRect { adj_100k: dml::round_rect_adj(radius, size) })
}

pub(crate) fn resolved_fill(fill: &Option<Paint>) -> Option<Option<FillSpec>> {
    dml::resolved_fill(fill, AlphaMode::Preserve)
}

pub(crate) fn srgb_bytes(color: &Color) -> [u8; 4] {
    ooxml_color::srgb_rgba(color)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::dom::PathSegment;
    use typst_library::foundations::Smart;
    use typst_library::layout::{Abs, Angle, Point, Ratio, Size, Transform};
    use typst_library::visualize::{
        ColorSpace, FillRule, LinearGradient, Oklab, ProcessColor, ProcessColorSpace, Rgb,
    };
    use typst_library::visualize::{Geometry, Gradient};

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

#![allow(dead_code)]

use crate::dom::{FillSpec, GeomKind, GeomShape, PathGeom, PicGeom, SlideCtx};
use crate::report::{DecisionReason, LossSet, Representation};

use typst_library::layout::{Size, Transform};
use typst_library::visualize::{Color, Curve, Geometry, Paint, Shape, Tiling};
use typst_ooxml_core::dml::{self, AlphaMode, TileImage};
use typst_ooxml_core::{color as ooxml_color, units};

/// Which part of a shape had no DrawingML form.
///
/// Kept separate from the `None` these lowering helpers return individually,
/// because "the exporter rasterized a shape" is not actionable on its own —
/// a skewed rectangle, a degenerate path, and a conic-gradient fill are three
/// different problems with three different answers, and the fidelity report
/// can only say which one it was if the failure is typed on the way out.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum ShapeFallback {
    /// The frame-walk transform is not a similarity: a skew, a non-uniform
    /// scale, or a reflection.
    Transform,
    /// The path is empty, degenerate (zero width *and* height), or has
    /// non-finite bounds.
    Geometry,
    /// The fill paint has no DrawingML equivalent: an off-center radial
    /// gradient, a conic gradient, or a tiling whose tile failed to render.
    Fill,
    /// The stroke is painted with a gradient or a tiling; `a:ln` carries a
    /// solid color only.
    Stroke,
}

impl ShapeFallback {
    pub(crate) fn reason(self) -> DecisionReason {
        match self {
            Self::Transform => DecisionReason::UnmappableShapeTransformRasterFallback,
            Self::Geometry => DecisionReason::UnmappableShapeGeometryRasterFallback,
            Self::Fill => DecisionReason::UnmappableShapeFillRasterFallback,
            Self::Stroke => DecisionReason::UnmappableShapeStrokeRasterFallback,
        }
    }

    /// Short tag for the `PPTX_DEBUG_RASTER` trace.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Transform => "transform",
            Self::Geometry => "geometry",
            Self::Fill => "fill",
            Self::Stroke => "stroke",
        }
    }
}

/// Lower one laid-out Typst shape to the PPTX slide IR.
///
/// The transform is expected to be the frame-walk transform for this item. It
/// may translate, rotate, reflect, or uniformly scale the geometry; skew and
/// non-uniform scale return [`ShapeFallback::Transform`] so the caller can
/// rasterize instead.
pub(crate) fn shape_to_geom(
    ctx: &mut SlideCtx,
    shape: &Shape,
    transform: Transform,
    rot_60k: i32,
) -> Result<GeomShape, ShapeFallback> {
    let scale = dml::similarity_scale(&transform).ok_or(ShapeFallback::Transform)?;
    let stroke = dml::resolved_stroke(&shape.stroke, scale, AlphaMode::Preserve)
        .ok_or(ShapeFallback::Stroke)?;

    if let Some(connector) = line_to_connector(shape, transform, rot_60k, stroke.clone())
    {
        return Ok(connector);
    }

    let raw = dml::geometry_to_raw(&shape.geometry, transform);
    let normalized = dml::normalize_segments(raw).ok_or(ShapeFallback::Geometry)?;
    let fill = resolved_fill(ctx, &shape.fill).ok_or(ShapeFallback::Fill)?;

    Ok(GeomShape {
        x_emu: units::abs_to_emu(normalized.min_x),
        y_emu: units::abs_to_emu(normalized.min_y),
        w_emu: units::abs_to_emu(normalized.w).max(1),
        h_emu: units::abs_to_emu(normalized.h).max(1),
        rot_60k,
        geom: GeomKind::Path(PathGeom::Custom(normalized.segments)),
        fill,
        stroke,
        link: None,
    })
}

fn line_to_connector(
    shape: &Shape,
    transform: Transform,
    rot_60k: i32,
    stroke: Option<dml::StrokeSpec>,
) -> Option<GeomShape> {
    let Geometry::Line(delta) = &shape.geometry else {
        return None;
    };

    if shape.fill.is_some() {
        return None;
    }

    let start = typst_library::layout::Point::zero().transform(transform);
    let end = delta.transform(transform);
    let (min_x, min_y) = (start.x.min(end.x), start.y.min(end.y));
    let (max_x, max_y) = (start.x.max(end.x), start.y.max(end.y));
    let (w, h) = (max_x - min_x, max_y - min_y);
    if !w.to_pt().is_finite() || !h.to_pt().is_finite() {
        return None;
    }
    if w.to_pt() <= 0.0 && h.to_pt() <= 0.0 {
        return None;
    }

    Some(GeomShape {
        x_emu: units::abs_to_emu(min_x),
        y_emu: units::abs_to_emu(min_y),
        w_emu: units::abs_to_emu(w).max(1),
        h_emu: units::abs_to_emu(h).max(1),
        rot_60k,
        geom: GeomKind::Connector { flip_h: start.x > end.x, flip_v: start.y > end.y },
        fill: None,
        stroke,
        link: None,
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

    if *clip == Curve::rect(size) {
        return Some(PicGeom::Rect);
    }

    let radius = dml::rounded_rect_radius(clip, size)?;
    Some(PicGeom::RoundRect { adj_100k: dml::round_rect_adj(radius, size) })
}

pub(crate) fn resolved_fill(
    ctx: &mut SlideCtx,
    fill: &Option<Paint>,
) -> Option<Option<FillSpec>> {
    match fill {
        Some(Paint::Tiling(tiling)) => tile_fill(ctx, tiling).map(Some),
        _ => dml::resolved_fill(fill, AlphaMode::Preserve),
    }
}

fn tile_fill(ctx: &mut SlideCtx, tiling: &Tiling) -> Option<FillSpec> {
    let tile = dml::render_tiling_tile(tiling)?;
    let media = ctx.add_media(&tile.png, "png");
    ctx.fidelity_report.record(
        ctx.current_slide,
        Representation::Approximate,
        DecisionReason::TilingFillRasterizedApproximation,
        LossSet::TILE_FILL,
        0,
    );
    Some(tile.fill(TileImage::Media(media)))
}

pub(crate) fn srgb_bytes(color: &Color) -> [u8; 4] {
    ooxml_color::srgb_rgba(color)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::dom::{GeomKind, PathSegment};
    use typst_library::foundations::Smart;
    use typst_library::layout::{Abs, Angle, Point, Ratio, Size, Transform};
    use typst_library::visualize::{
        ColorSpace, FillRule, FixedStroke, LinearGradient, Oklab, ProcessColor,
        ProcessColorSpace, RadialGradient, Rgb,
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
        let mut ctx = SlideCtx::default();
        let geom = shape_to_geom(&mut ctx, &shape, Transform::identity(), 0).unwrap();

        assert_eq!(geom.x_emu, 0);
        assert_eq!(geom.y_emu, 0);
        assert_eq!(geom.w_emu, emu_pt(10.0));
        assert_eq!(geom.h_emu, emu_pt(20.0));
        let GeomKind::Path(PathGeom::Custom(segments)) = geom.geom else {
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
        let mut ctx = SlideCtx::default();
        let geom = shape_to_geom(&mut ctx, &shape, Transform::identity(), 0).unwrap();
        let GeomKind::Path(PathGeom::Custom(segments)) = geom.geom else {
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
    fn line_geometry_lowers_to_loose_connector() {
        let shape = bare_shape(Geometry::Line(Point::new(Abs::pt(5.0), Abs::pt(5.0))));
        let transform = Transform::translate(Abs::pt(-5.0), Abs::pt(-10.0));
        let mut ctx = SlideCtx::default();
        let geom = shape_to_geom(&mut ctx, &shape, transform, 0).unwrap();

        assert_eq!(geom.x_emu, emu_pt(-5.0));
        assert_eq!(geom.y_emu, emu_pt(-10.0));
        assert_eq!(geom.w_emu, emu_pt(5.0));
        assert_eq!(geom.h_emu, emu_pt(5.0));
        assert!(matches!(
            geom.geom,
            GeomKind::Connector { flip_h: false, flip_v: false }
        ));
        assert!(geom.fill.is_none());
    }

    #[test]
    fn descending_line_connector_records_flip() {
        let shape = bare_shape(Geometry::Line(Point::new(Abs::pt(-5.0), Abs::pt(5.0))));
        let transform = Transform::translate(Abs::pt(10.0), Abs::pt(0.0));
        let mut ctx = SlideCtx::default();
        let geom = shape_to_geom(&mut ctx, &shape, transform, 0).unwrap();

        assert_eq!(geom.x_emu, emu_pt(5.0));
        assert_eq!(geom.y_emu, 0);
        assert_eq!(geom.w_emu, emu_pt(5.0));
        assert_eq!(geom.h_emu, emu_pt(5.0));
        assert!(matches!(geom.geom, GeomKind::Connector { flip_h: true, flip_v: false }));
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

        let mut ctx = SlideCtx::default();
        let fill = resolved_fill(&mut ctx, &Some(Paint::Gradient(gradient)))
            .unwrap()
            .unwrap();
        let FillSpec::LinearGradient { angle_60k, stops } = fill else {
            panic!("expected linear gradient");
        };

        assert_eq!(angle_60k, 30 * 60_000);
        assert_eq!(stops[0].pos_100k, 0);
        assert_eq!(stops.last().unwrap().pos_100k, 100_000);
        assert_ne!(stops[0].color, raw);
        assert_eq!(stops[0].color, srgb_bytes(&oklab));
        assert!(stops.len() > 2, "Oklab interpolation needs sampled sRGB stops");
    }

    fn unmappable_gradient() -> Gradient {
        Gradient::Radial(Arc::new(RadialGradient {
            stops: vec![
                (
                    Color::Process(ProcessColor::Rgb(Rgb::new(1.0, 0.0, 0.0, 1.0))),
                    Ratio::zero(),
                ),
                (
                    Color::Process(ProcessColor::Rgb(Rgb::new(0.0, 0.0, 1.0, 1.0))),
                    Ratio::one(),
                ),
            ],
            center: typst_library::layout::Axes::new(Ratio::new(0.4), Ratio::new(0.6)),
            radius: Ratio::new(0.7),
            focal_center: typst_library::layout::Axes::new(
                Ratio::new(0.3),
                Ratio::new(0.45),
            ),
            focal_radius: Ratio::new(0.1),
            space: ColorSpace::Process(ProcessColorSpace::Srgb),
            relative: Smart::Auto,
            anti_alias: true,
        }))
    }

    /// The whole point of the typed error: four different problems that all
    /// end in a picture must not arrive at the report as one indistinguishable
    /// "unmappable shape".
    #[test]
    fn each_unmappable_part_names_itself() {
        let mut ctx = SlideCtx::default();
        let square = Size::new(Abs::pt(10.0), Abs::pt(20.0));

        let cause = |ctx: &mut SlideCtx, shape: &Shape, transform| {
            shape_to_geom(ctx, shape, transform, 0).err()
        };

        // A shear is not a similarity, so the path cannot be placed.
        let skew = Transform { ky: Ratio::new(0.5), ..Transform::identity() };
        let rect = bare_shape(Geometry::Rect(square));
        assert_eq!(cause(&mut ctx, &rect, skew), Some(ShapeFallback::Transform));

        // A rectangle with no extent in either axis has no `custGeom`.
        let degenerate = bare_shape(Geometry::Rect(Size::zero()));
        assert_eq!(
            cause(&mut ctx, &degenerate, Transform::identity()),
            Some(ShapeFallback::Geometry)
        );

        // An off-center radial gradient has no `a:gradFill` equivalent.
        let filled = Shape {
            fill: Some(Paint::Gradient(unmappable_gradient())),
            ..bare_shape(Geometry::Rect(square))
        };
        assert_eq!(
            cause(&mut ctx, &filled, Transform::identity()),
            Some(ShapeFallback::Fill)
        );

        // `a:ln` carries a solid color only, whatever the gradient is.
        let stroked = Shape {
            stroke: Some(FixedStroke {
                paint: Paint::Gradient(unmappable_gradient()),
                thickness: Abs::pt(1.0),
                ..FixedStroke::default()
            }),
            ..bare_shape(Geometry::Rect(square))
        };
        assert_eq!(
            cause(&mut ctx, &stroked, Transform::identity()),
            Some(ShapeFallback::Stroke)
        );
    }

    #[test]
    fn radial_gradient_is_not_natively_mapped() {
        // Radial gradients bail to the raster fallback rather than a native
        // fill: an empirical LibreOffice check found the DrawingML
        // `a:path path="circle"`/`a:fillToRect` model renders visibly more
        // circular than Typst's own box-relative elliptical stretch on a
        // non-square shape, so `gradient_fill` intentionally never produces
        // `FillSpec::RadialGradient` for now (see its doc comment).
        let gradient = Gradient::Radial(Arc::new(RadialGradient {
            stops: vec![
                (
                    Color::Process(ProcessColor::Rgb(Rgb::new(1.0, 0.0, 0.0, 1.0))),
                    Ratio::zero(),
                ),
                (
                    Color::Process(ProcessColor::Rgb(Rgb::new(0.0, 0.0, 1.0, 1.0))),
                    Ratio::one(),
                ),
            ],
            center: typst_library::layout::Axes::new(Ratio::new(0.4), Ratio::new(0.6)),
            radius: Ratio::new(0.7),
            focal_center: typst_library::layout::Axes::new(
                Ratio::new(0.3),
                Ratio::new(0.45),
            ),
            focal_radius: Ratio::new(0.1),
            space: ColorSpace::Process(ProcessColorSpace::Srgb),
            relative: Smart::Auto,
            anti_alias: true,
        }));

        let mut ctx = SlideCtx::default();
        assert!(resolved_fill(&mut ctx, &Some(Paint::Gradient(gradient))).is_none());
    }
}

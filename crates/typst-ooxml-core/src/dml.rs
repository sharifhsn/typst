//! DrawingML geometry, fill, and stroke primitives shared by Office exporters.

use ecow::EcoString;

use crate::color;
use crate::media::MediaId;
use crate::ns;
use crate::units;
use crate::xml::XmlWriter;
use typst_export_common::raster;
use typst_library::layout::{Abs, Frame, Point, Ratio, Size, Transform};
use typst_library::visualize::{
    Color, Curve, CurveItem, DashPattern, FixedStroke, Geometry, Gradient,
    GradientStop as TypstGradientStop, LineCap, Paint, Tiling,
};

const A14_USE_LOCAL_DPI_EXT_URI: &str = "{28A0092B-C50C-407E-A947-70E740481C1C}";
const ASVG_SVG_BLIP_EXT_URI: &str = "{96DAC541-7B7A-43D3-8B79-37D633B846F1}";
const ASVG_NS: &str = "http://schemas.microsoft.com/office/drawing/2016/SVG/main";
const TILE_PIXEL_PER_PT: f64 = 2.0;
const OFFICE_DEFAULT_DPI: f64 = 96.0;
const PT_PER_IN: f64 = 72.0;

/// One custom geometry path segment.
pub enum PathSegment {
    MoveTo(i64, i64),
    LineTo(i64, i64),
    CubicTo(i64, i64, i64, i64, i64, i64),
    Close,
}

/// A fill specification. Colors are straight sRGB + alpha (`[r, g, b, a]`).
#[derive(Clone)]
pub enum FillSpec {
    Solid([u8; 4]),
    LinearGradient {
        angle_60k: i32,
        stops: Vec<GradientStop>,
    },
    RadialGradient {
        stops: Vec<GradientStop>,
        center_100k: [i32; 2],
        radius_100k: i32,
        focal_center_100k: [i32; 2],
        focal_radius_100k: i32,
    },
    Tile {
        image: TileImage,
        tx_emu: i64,
        ty_emu: i64,
        sx_100k: i32,
        sy_100k: i32,
        algn: &'static str,
    },
}

/// Where a tile fill's PNG is referenced from.
#[derive(Clone)]
pub enum TileImage {
    /// Exporter-level media registry id; resolved to a part-local `rId` while
    /// serializing the owning XML part.
    Media(MediaId),
    /// Already allocated relationship id in the owning XML part.
    Rel(EcoString),
}

/// A rasterized tile cell plus its DrawingML placement attributes.
pub struct RenderedTile {
    pub png: Vec<u8>,
    pub tx_emu: i64,
    pub ty_emu: i64,
    pub sx_100k: i32,
    pub sy_100k: i32,
    pub algn: &'static str,
}

impl RenderedTile {
    pub fn fill(self, image: TileImage) -> FillSpec {
        FillSpec::Tile {
            image,
            tx_emu: self.tx_emu,
            ty_emu: self.ty_emu,
            sx_100k: self.sx_100k,
            sy_100k: self.sy_100k,
            algn: self.algn,
        }
    }
}

/// A gradient stop.
#[derive(Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub pos_100k: i32,
    pub color: [u8; 4],
}

/// A stroke specification.
#[derive(Clone, Debug)]
pub struct StrokeSpec {
    pub color: [u8; 4],
    pub w_emu: i64,
    pub cap: &'static str,
    pub dash: Option<DashSpec>,
}

/// A stroke's dash pattern, in the two shapes DrawingML offers for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashSpec {
    /// An `a:prstDash` preset. A named pattern is what a consumer's own UI
    /// round-trips, so it is preferred wherever it is exact.
    Preset(&'static str),
    /// An `a:custDash` pattern spelling out the authored run lengths.
    Custom(Vec<DashStop>),
}

/// One `a:ds` entry: a dash and the space that follows it, each measured in
/// 1000ths of a percent of the line width (so `100_000` is one line width).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashStop {
    pub d: i32,
    pub sp: i32,
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
        Some(Paint::Gradient(gradient)) => gradient_fill(gradient, alpha).map(Some),
        Some(Paint::Tiling(_)) => None,
    }
}

/// Lowers a Typst gradient to a DrawingML fill.
///
/// Both models normalize to the painted box, so the elliptical stretch a
/// radial gradient takes in a non-square shape carries over for free. What does
/// *not* carry over is the extent: Typst parameterizes a radial gradient
/// between a focal circle and an outer circle of its own `radius`, while
/// DrawingML always runs from `a:fillToRect` to the shape's bounding box. The
/// focal circle is exactly what `a:fillToRect` expresses; the outer radius is
/// absorbed by rescaling the stop offsets into bounding-box space (see
/// [`stops_relative_to_bounding_box`]).
///
/// `None` (rasterize) for the two cases that have no faithful mapping:
/// * an outer circle that is not centered — DrawingML's outer path is always
///   the shape's own rectangle, so an off-center one cannot be expressed; and
/// * a conic gradient, which has no DrawingML analogue at all (`a:gradFill`
///   offers linear, rectangular, circular and shape paths — none sweeps by
///   angle).
pub fn gradient_fill(gradient: &Gradient, alpha: AlphaMode) -> Option<FillSpec> {
    match gradient {
        Gradient::Linear(linear) => {
            let angle_60k =
                (linear.angle.to_deg().rem_euclid(360.0) * 60_000.0).round() as i32;
            Some(FillSpec::LinearGradient {
                angle_60k,
                stops: gradient_stops(gradient, &linear.stops, alpha),
            })
        }
        Gradient::Radial(radial) => {
            let center = [ratio_100k(radial.center.x), ratio_100k(radial.center.y)];
            if center != [50_000, 50_000] {
                return None;
            }
            let radius_100k = ratio_100k(radial.radius);
            let focal_radius_100k = ratio_100k(radial.focal_radius);
            let stops = stops_relative_to_bounding_box(
                gradient_stops(gradient, &radial.stops, alpha),
                radius_100k,
                focal_radius_100k,
            )?;
            Some(FillSpec::RadialGradient {
                stops,
                center_100k: center,
                radius_100k,
                focal_center_100k: [
                    ratio_100k(radial.focal_center.x),
                    ratio_100k(radial.focal_center.y),
                ],
                focal_radius_100k,
            })
        }
        Gradient::Conic(_) => None,
    }
}

/// Reparameterizes radial stops from Typst's outer circle onto DrawingML's
/// bounding box.
///
/// Both models start their gradient at the focal circle — which is exactly what
/// `a:fillToRect` encodes — but they end it in different places. Typst reaches
/// its last stop at `radius` from the (centered) origin, in coordinates
/// normalized to the box, and holds the final color beyond that; DrawingML
/// reaches its last stop at the box edge, half a normalized unit out. A stop at
/// `p` therefore belongs at `p × (radius − focal_radius) / (0.5 − focal_radius)`:
///
/// * a small `radius` compresses every stop inward, and the color Typst holds
///   past it becomes an explicit terminal stop at the box edge;
/// * a large `radius` pushes the tail outside the shape, so the gradient is cut
///   at the edge with an interpolated stop — that tail is not visible in Typst
///   either.
///
/// `None` for a gradient there is nothing to draw: no stops, no extent, or a
/// focal circle that already reaches the box edge.
fn stops_relative_to_bounding_box(
    stops: Vec<GradientStop>,
    radius_100k: i32,
    focal_radius_100k: i32,
) -> Option<Vec<GradientStop>> {
    if stops.is_empty() || radius_100k <= focal_radius_100k || focal_radius_100k >= 50_000
    {
        return None;
    }
    let scale = f64::from(radius_100k - focal_radius_100k)
        / f64::from(50_000 - focal_radius_100k);

    let mut out: Vec<GradientStop> = Vec::with_capacity(stops.len() + 1);
    for stop in &stops {
        let pos = (f64::from(stop.pos_100k) * scale).round() as i32;
        if pos <= 100_000 {
            out.push(GradientStop { pos_100k: pos, color: stop.color });
            continue;
        }
        // The first stop past the box edge: land exactly on the edge, taking
        // the color the gradient has there. Everything after it is outside the
        // shape.
        let previous = out.last().copied();
        out.push(GradientStop {
            pos_100k: 100_000,
            color: match previous {
                Some(before) if pos > before.pos_100k => mix_rgba(
                    before.color,
                    stop.color,
                    f64::from(100_000 - before.pos_100k)
                        / f64::from(pos - before.pos_100k),
                ),
                _ => stop.color,
            },
        });
        break;
    }

    // A gradient that ends before the box edge holds its final color out to it,
    // matching Typst's clamp beyond `radius`.
    if out.last().is_some_and(|last| last.pos_100k < 100_000) {
        let color = out[out.len() - 1].color;
        out.push(GradientStop { pos_100k: 100_000, color });
    }
    Some(out)
}

/// Straight-alpha linear blend of two sRGB colors, `t` running from `a` to `b`.
fn mix_rgba(a: [u8; 4], b: [u8; 4], t: f64) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round().clamp(0.0, 255.0) as u8
    };
    [
        channel(a[0], b[0]),
        channel(a[1], b[1]),
        channel(a[2], b[2]),
        channel(a[3], b[3]),
    ]
}

/// Lowers a Typst linear gradient to a DrawingML fill.
pub fn linear_gradient_fill(gradient: &Gradient, alpha: AlphaMode) -> Option<FillSpec> {
    let Gradient::Linear(_) = gradient else { return None };
    gradient_fill(gradient, alpha)
}

/// Rasterizes one Typst tiling period for use in an OOXML `a:tile` fill.
pub fn render_tiling_tile(tiling: &Tiling) -> Option<RenderedTile> {
    let period = tiling.size() + tiling.spacing();
    let (w_pt, h_pt) = (period.x.to_pt(), period.y.to_pt());
    if !w_pt.is_finite() || !h_pt.is_finite() || w_pt <= 0.0 || h_pt <= 0.0 {
        return None;
    }

    let w_px = ((w_pt * TILE_PIXEL_PER_PT).round() as u32).max(1);
    let h_px = ((h_pt * TILE_PIXEL_PER_PT).round() as u32).max(1);

    let mut frame = Frame::hard(period);
    frame.push_frame(Point::zero(), tiling.frame().clone());
    let raster = raster::render_full_frame_to_png(frame, TILE_PIXEL_PER_PT)?;

    Some(RenderedTile {
        png: raster.png,
        tx_emu: units::abs_to_emu(tiling.offset().x),
        ty_emu: units::abs_to_emu(tiling.offset().y),
        sx_100k: tile_scale_100k(period.x, w_px),
        sy_100k: tile_scale_100k(period.y, h_px),
        algn: "tl",
    })
}

fn tile_scale_100k(target: Abs, pixels: u32) -> i32 {
    let natural_pt = pixels as f64 * PT_PER_IN / OFFICE_DEFAULT_DPI;
    ((target.to_pt() / natural_pt) * 100_000.0).round() as i32
}

fn gradient_stops(
    gradient: &Gradient,
    stops: &[(Color, Ratio)],
    alpha: AlphaMode,
) -> Vec<GradientStop> {
    let Some((first_color, first_pos)) = stops.first() else {
        return vec![];
    };

    let mut lowered = vec![GradientStop {
        pos_100k: ratio_100k(*first_pos),
        color: srgb_rgba(first_color, alpha),
    }];

    for pair in stops.windows(2) {
        // Office interpolates native stops in sRGB. Add only the samples needed
        // to approximate Typst's authored interpolation space while retaining an
        // editable DrawingML gradient rather than flattening it to a picture.
        let first = TypstGradientStop::new(pair[0].0.clone(), pair[0].1);
        let second = TypstGradientStop::new(pair[1].0.clone(), pair[1].1);
        lowered.extend(
            gradient
                .generate_intermediate_stops_for_rgb_interpolation(&first, &second)
                .map(|(color, pos)| GradientStop {
                    pos_100k: ratio_100k(pos),
                    color: srgb_rgba(&color, alpha),
                }),
        );
        lowered.push(GradientStop {
            pos_100k: ratio_100k(pair[1].1),
            color: srgb_rgba(&pair[1].0, alpha),
        });
    }

    lowered
}

fn ratio_100k(ratio: Ratio) -> i32 {
    (ratio.get() * 100_000.0).round() as i32
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
                    dash: fixed.dash.as_ref().map(|dash| dash_spec(dash, thickness)),
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

/// Lowers a resolved Typst dash pattern to DrawingML.
///
/// DrawingML can state a dash pattern exactly — `a:custDash` lists the run
/// lengths as percentages of the line width — so the authored pattern is
/// preserved rather than snapped to whichever preset happens to be nearest.
/// A preset still wins when the pattern *is* that preset, because a named
/// pattern is what a consumer's line UI shows and round-trips; the nearest-fit
/// [`prst_dash`] heuristic remains only for the patterns `a:custDash` cannot
/// state at all (see [`custom_dash`]).
pub fn dash_spec(dash: &DashPattern<Abs, Abs>, thickness: Abs) -> DashSpec {
    if dash.array.is_empty() {
        return DashSpec::Preset("solid");
    }

    // An odd-length array alternates its on/off roles on every repeat, which
    // every renderer resolves by repeating the array once; `a:ds` pairs each
    // dash with its following space, so it needs that same even array.
    let mut array = dash.array.clone();
    if array.len() % 2 == 1 {
        array.extend_from_within(..);
    }
    rotate_to_phase(&mut array, dash.phase);

    if let Some(preset) = exact_preset(&array, thickness) {
        return DashSpec::Preset(preset);
    }

    match custom_dash(&array, thickness) {
        Some(stops) => DashSpec::Custom(stops),
        // Fall back on the original (un-doubled) array so the heuristic keeps
        // reading the pattern the author actually wrote.
        None => DashSpec::Preset(prst_dash(&dash.array, thickness)),
    }
}

/// The `a:prstDash` presets ECMA-376 fixes as a width-relative run-length
/// sequence, and their sequences.
///
/// The `sys*` family is deliberately absent: those are *system* dashes whose
/// run lengths the consumer chooses, so matching a pattern onto one would hand
/// the geometry back to the renderer instead of preserving it.
const PRESET_RATIOS: &[(&str, &[f64])] = &[
    ("dot", &[1.0, 3.0]),
    ("dash", &[4.0, 3.0]),
    ("lgDash", &[8.0, 3.0]),
    ("dashDot", &[4.0, 3.0, 1.0, 3.0]),
    ("lgDashDot", &[8.0, 3.0, 1.0, 3.0]),
    ("lgDashDotDot", &[8.0, 3.0, 1.0, 3.0, 1.0, 3.0]),
];

/// The preset that renders exactly this pattern, if any.
fn exact_preset(array: &[Abs], thickness: Abs) -> Option<&'static str> {
    if thickness <= Abs::zero() {
        return None;
    }
    let ratios: Vec<f64> = array.iter().map(|length| *length / thickness).collect();
    PRESET_RATIOS
        .iter()
        .find(|(_, preset)| {
            preset.len() == ratios.len()
                && preset
                    .iter()
                    .zip(&ratios)
                    .all(|(preset, ratio)| (preset - ratio).abs() <= 1e-3)
        })
        .map(|(name, _)| *name)
}

/// Lowers an even-length dash array to `a:ds` entries.
///
/// `None` for the patterns DrawingML cannot state: run lengths are relative to
/// the line width, so a zero-width stroke has no scale to express them in, and
/// a run that rounds to zero (or overflows the 32-bit percentage) would collapse
/// the pattern rather than approximate it.
fn custom_dash(array: &[Abs], thickness: Abs) -> Option<Vec<DashStop>> {
    if thickness <= Abs::zero() {
        return None;
    }
    array
        .chunks_exact(2)
        .map(|pair| {
            Some(DashStop {
                d: line_width_percent(pair[0], thickness)?,
                sp: line_width_percent(pair[1], thickness)?,
            })
        })
        .collect()
}

/// A run length as a fraction of the line width, in `a:ds`'s unit of 1000ths
/// of a percent.
fn line_width_percent(length: Abs, thickness: Abs) -> Option<i32> {
    let percent = (length / thickness * 100_000.0).round();
    (1.0..=f64::from(i32::MAX)).contains(&percent).then_some(percent as i32)
}

/// Rotates a dash array so that it starts where a non-zero phase does.
///
/// `a:custDash` always begins at the head of its pattern, so a phase survives
/// only when it lands exactly on a run boundary — and only an even one, since
/// `a:ds` pairs each dash with the space after it. Any other phase is dropped;
/// the pattern itself still survives, which is strictly more than the nearest
/// preset would have kept.
fn rotate_to_phase(array: &mut [Abs], phase: Abs) {
    let period: f64 = array.iter().map(|length| length.to_pt()).sum();
    if period <= 0.0 {
        return;
    }
    let offset = phase.to_pt().rem_euclid(period);
    let tolerance = period * 1e-6;
    let mut boundary = 0.0;
    for index in (0..array.len()).step_by(2) {
        if (boundary - offset).abs() <= tolerance {
            array.rotate_left(index);
            return;
        }
        boundary += array[index].to_pt() + array[index + 1].to_pt();
    }
}

/// Maps a resolved dash array to the closest DrawingML preset dash.
pub fn prst_dash(array: &[Abs], thickness: Abs) -> &'static str {
    if array.is_empty() {
        return "solid";
    }

    // A compound pattern has alternating dash and dot components. Typst's
    // named dash-dotted presets always resolve to four or more entries; keep
    // that semantic shape even when a thick stroke makes every on-segment no
    // longer than the line width.
    if array.len() >= 4 {
        return "dashDot";
    }

    // Typst's `dashed` preset is an equal on/off pair (3pt, 3pt). At a 3pt
    // stroke width the old line-width-only heuristic called that a dot and
    // emitted `sysDot`, producing widely spaced round dots. An equal pair is
    // a short system dash; unequal short pairs remain the closest dot preset.
    if let [on, off] = array {
        let tolerance = (thickness * 0.15).max(Abs::pt(0.05));
        if (*on - *off).abs() <= tolerance {
            return "sysDash";
        }
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
    write_fill_with_tile_resolver(w, fill, gradient_scaled, |_| EcoString::new());
}

/// Emits an `a:solidFill`, `a:gradFill`, `a:blipFill`, or `a:noFill` child.
pub fn write_fill_with_tile_resolver(
    w: &mut XmlWriter,
    fill: Option<&FillSpec>,
    gradient_scaled: &'static str,
    mut tile_media_rid: impl FnMut(MediaId) -> EcoString,
) {
    match fill {
        Some(FillSpec::Solid(rgba)) => write_solid_fill(w, *rgba),
        Some(FillSpec::LinearGradient { angle_60k, stops }) => {
            w.open("a:gradFill").attr("rotWithShape", "1").start_children();
            write_gradient_stops(w, stops);
            w.open("a:lin")
                .attr("ang", &angle_60k.to_string())
                .attr("scaled", gradient_scaled)
                .empty();
            w.close();
        }
        Some(FillSpec::RadialGradient {
            stops,
            focal_center_100k,
            focal_radius_100k,
            ..
        }) => {
            let [l, t, r, b] =
                radial_focus_rect_100k(*focal_center_100k, *focal_radius_100k);
            w.open("a:gradFill").attr("rotWithShape", "1").start_children();
            write_gradient_stops(w, stops);
            w.open("a:path").attr("path", "circle").start_children();
            w.open("a:fillToRect")
                .attr("l", &l.to_string())
                .attr("t", &t.to_string())
                .attr("r", &r.to_string())
                .attr("b", &b.to_string())
                .empty();
            w.close();
            w.close();
        }
        Some(FillSpec::Tile { image, tx_emu, ty_emu, sx_100k, sy_100k, algn }) => {
            let embed = match image {
                TileImage::Media(media) => tile_media_rid(*media),
                TileImage::Rel(rid) => rid.clone(),
            };
            if embed.is_empty() {
                w.leaf("a:noFill");
                return;
            }
            w.open("a:blipFill").start_children();
            write_blip(w, &embed, None);
            w.open("a:tile")
                .attr("tx", &tx_emu.to_string())
                .attr("ty", &ty_emu.to_string())
                .attr("sx", &sx_100k.to_string())
                .attr("sy", &sy_100k.to_string())
                .attr("algn", algn)
                .empty();
            w.close();
        }
        None => w.leaf("a:noFill"),
    }
}

fn write_gradient_stops(w: &mut XmlWriter, stops: &[GradientStop]) {
    w.open("a:gsLst").start_children();
    for stop in stops {
        w.open("a:gs")
            .attr("pos", &stop.pos_100k.to_string())
            .start_children();
        write_srgb(w, stop.color);
        w.close();
    }
    w.close();
}

/// The DrawingML `a:fillToRect` rectangle (percent×1000, may run negative or
/// past 100k) that reproduces a focal circle at `focal_center`/`focal_radius`.
/// Shared by this module's own `a:gradFill` writer
/// ([`write_fill_with_tile_resolver`]) and the `typst-docx` crate's
/// `w14:gradFill` writer (`props::write_text_fill`), which both express a
/// radial gradient's focus rectangle the same way.
pub fn radial_focus_rect_100k(focal_center: [i32; 2], focal_radius: i32) -> [i32; 4] {
    let [x, y] = focal_center;
    [
        x - focal_radius,
        y - focal_radius,
        100_000 - x - focal_radius,
        100_000 - y - focal_radius,
    ]
}

/// Emits an `a:blip` child, including the Office SVG extension when present.
pub fn write_blip(w: &mut XmlWriter, embed: &str, svg_embed: Option<&str>) {
    w.open("a:blip").attr("r:embed", embed);
    if let Some(svg_embed) = svg_embed {
        w.start_children();
        w.open("a:extLst").start_children();
        w.open("a:ext")
            .attr("uri", A14_USE_LOCAL_DPI_EXT_URI)
            .start_children();
        w.open("a14:useLocalDpi")
            .attr("xmlns:a14", ns::A14)
            .attr("val", "0")
            .empty();
        w.close();
        w.open("a:ext").attr("uri", ASVG_SVG_BLIP_EXT_URI).start_children();
        w.open("asvg:svgBlip")
            .attr("xmlns:asvg", ASVG_NS)
            .attr("r:embed", svg_embed)
            .empty();
        w.close();
        w.close();
        w.close();
    } else {
        w.empty();
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
            write_dash(w, stroke.dash.as_ref());
            w.close();
        }
        None => {
            w.open("a:ln").start_children();
            w.leaf("a:noFill");
            w.close();
        }
    }
}

/// Emits an `a:ln`'s dash child. Both elements are optional, and their absence
/// means a solid line, so a `None` pattern writes nothing.
pub fn write_dash(w: &mut XmlWriter, dash: Option<&DashSpec>) {
    match dash {
        Some(DashSpec::Preset(preset)) => {
            w.open("a:prstDash").attr("val", preset).empty();
        }
        Some(DashSpec::Custom(stops)) => {
            w.open("a:custDash").start_children();
            for stop in stops {
                w.open("a:ds")
                    .attr("d", &stop.d.to_string())
                    .attr("sp", &stop.sp.to_string())
                    .empty();
            }
            w.close();
        }
        None => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_dash_distinguishes_equal_dashes_from_dots() {
        assert_eq!(prst_dash(&[Abs::pt(3.0), Abs::pt(3.0)], Abs::pt(3.0)), "sysDash");
        assert_eq!(prst_dash(&[Abs::pt(3.0), Abs::pt(2.0)], Abs::pt(3.0)), "sysDot");
        assert_eq!(
            prst_dash(
                &[Abs::pt(3.0), Abs::pt(2.0), Abs::pt(3.0), Abs::pt(2.0)],
                Abs::pt(3.0),
            ),
            "dashDot"
        );
        assert_eq!(prst_dash(&[Abs::pt(6.0), Abs::pt(3.0)], Abs::pt(3.0)), "dash");
    }

    fn pattern(array: &[f64], phase: f64) -> DashPattern<Abs, Abs> {
        DashPattern {
            array: array.iter().copied().map(Abs::pt).collect(),
            phase: Abs::pt(phase),
        }
    }

    fn dash_stops(pairs: &[(i32, i32)]) -> DashSpec {
        DashSpec::Custom(pairs.iter().map(|&(d, sp)| DashStop { d, sp }).collect())
    }

    #[test]
    fn custom_dash_states_the_authored_run_lengths() {
        // 3pt on / 1.5pt off at a 3pt width: one line width of dash, half of gap.
        assert_eq!(
            dash_spec(&pattern(&[3.0, 1.5], 0.0), Abs::pt(3.0)),
            dash_stops(&[(100_000, 50_000)])
        );
        // A four-entry dash-dot pattern keeps all four runs rather than
        // collapsing onto the `dashDot` preset's own proportions.
        assert_eq!(
            dash_spec(&pattern(&[3.0, 2.0, 1.0, 2.0], 0.0), Abs::pt(1.0)),
            dash_stops(&[(300_000, 200_000), (100_000, 200_000)])
        );
    }

    #[test]
    fn odd_dash_arrays_repeat_into_dash_space_pairs() {
        // `(4pt, 2pt, 1pt)` alternates roles on each repeat, so the resolved
        // pattern is the array twice over: 4 on, 2 off, 1 on, 4 off, 2 on, 1 off.
        assert_eq!(
            dash_spec(&pattern(&[4.0, 2.0, 1.0], 0.0), Abs::pt(1.0)),
            dash_stops(&[(400_000, 200_000), (100_000, 400_000), (200_000, 100_000)])
        );
    }

    #[test]
    fn exact_presets_win_over_a_custom_pattern() {
        // `dash` is 4 line widths on, 3 off.
        assert_eq!(
            dash_spec(&pattern(&[8.0, 6.0], 0.0), Abs::pt(2.0)),
            DashSpec::Preset("dash")
        );
        // A system preset is never matched onto — its run lengths belong to the
        // consumer — so an equal on/off pair stays exact.
        assert_eq!(
            dash_spec(&pattern(&[3.0, 3.0], 0.0), Abs::pt(3.0)),
            dash_stops(&[(100_000, 100_000)])
        );
    }

    #[test]
    fn a_phase_on_an_even_boundary_rotates_the_pattern() {
        // Starting 6pt into `(4pt, 2pt, 1pt, 2pt)` is the same as starting at
        // the third run.
        assert_eq!(
            dash_spec(&pattern(&[4.0, 2.0, 1.0, 2.0], 6.0), Abs::pt(1.0)),
            dash_stops(&[(100_000, 200_000), (400_000, 200_000)])
        );
        // A phase inside a run has no `a:custDash` equivalent; the pattern is
        // still stated exactly, only its offset is lost.
        assert_eq!(
            dash_spec(&pattern(&[4.0, 2.0, 1.0, 2.0], 1.0), Abs::pt(1.0)),
            dash_stops(&[(400_000, 200_000), (100_000, 200_000)])
        );
    }

    #[test]
    fn patterns_custom_dash_cannot_state_fall_back_to_a_preset() {
        // Percentages are relative to the line width, so a zero-width stroke
        // has no scale to express them in.
        assert_eq!(
            dash_spec(&pattern(&[3.0, 3.0], 0.0), Abs::zero()),
            DashSpec::Preset("sysDash")
        );
        // A run that rounds away would collapse the pattern instead of
        // approximating it.
        assert_eq!(
            dash_spec(&pattern(&[6.0, 0.0], 0.0), Abs::pt(3.0)),
            DashSpec::Preset("dash")
        );
        // An empty array is Typst's `solid`.
        assert_eq!(dash_spec(&pattern(&[], 0.0), Abs::pt(1.0)), DashSpec::Preset("solid"));
    }

    fn stops(pairs: &[(i32, u8)]) -> Vec<GradientStop> {
        pairs
            .iter()
            .map(|&(pos_100k, gray)| GradientStop {
                pos_100k,
                color: [gray, gray, gray, 255],
            })
            .collect()
    }

    fn positions(stops: &[GradientStop]) -> Vec<i32> {
        stops.iter().map(|stop| stop.pos_100k).collect()
    }

    #[test]
    fn default_radial_extent_needs_no_reparameterization() {
        let mapped =
            stops_relative_to_bounding_box(stops(&[(0, 0), (100_000, 255)]), 50_000, 0)
                .expect("the default radial gradient maps natively");
        assert_eq!(positions(&mapped), [0, 100_000]);
    }

    #[test]
    fn small_radial_extent_compresses_stops_and_holds_the_final_color() {
        let mapped =
            stops_relative_to_bounding_box(stops(&[(0, 0), (100_000, 255)]), 25_000, 0)
                .expect("a small radius maps natively");
        // Typst reaches its last stop a quarter of the way across the box, then
        // holds that color out to the edge.
        assert_eq!(positions(&mapped), [0, 50_000, 100_000]);
        assert_eq!(mapped[1].color, mapped[2].color);
    }

    #[test]
    fn large_radial_extent_is_cut_at_the_box_edge() {
        let mapped =
            stops_relative_to_bounding_box(stops(&[(0, 0), (100_000, 100)]), 100_000, 0)
                .expect("a large radius maps natively");
        // The box edge is halfway along the gradient, so the emitted terminal
        // stop carries the color the gradient has there, not its final one.
        assert_eq!(positions(&mapped), [0, 100_000]);
        assert_eq!(mapped[1].color, [50, 50, 50, 255]);
    }

    #[test]
    fn a_focal_circle_shifts_the_start_of_the_reparameterization() {
        // With the focal circle at 25% and the outer one at 50%, the visible
        // span is half of what DrawingML's own 0..50% span covers.
        let mapped = stops_relative_to_bounding_box(
            stops(&[(0, 0), (100_000, 255)]),
            50_000,
            25_000,
        )
        .expect("a focal circle maps natively");
        assert_eq!(positions(&mapped), [0, 100_000]);
    }

    #[test]
    fn degenerate_radial_extents_are_rejected() {
        assert!(stops_relative_to_bounding_box(stops(&[(0, 0)]), 0, 0).is_none());
        assert!(stops_relative_to_bounding_box(Vec::new(), 50_000, 0).is_none());
        assert!(
            stops_relative_to_bounding_box(stops(&[(0, 0)]), 60_000, 50_000).is_none(),
            "a focal circle reaching the box edge leaves no gradient to draw"
        );
    }

    #[test]
    fn write_fill_radial_gradient_emits_path_circle() {
        let fill = FillSpec::RadialGradient {
            stops: vec![
                GradientStop { pos_100k: 0, color: [255, 0, 0, 255] },
                GradientStop { pos_100k: 100_000, color: [0, 0, 255, 255] },
            ],
            center_100k: [50_000, 50_000],
            radius_100k: 50_000,
            focal_center_100k: [50_000, 50_000],
            focal_radius_100k: 50_000,
        };
        let mut w = XmlWriter::new(false);
        write_fill(&mut w, Some(&fill), "0");
        let xml = w.finish();
        assert!(xml.contains("<a:path path=\"circle\">"));
        assert!(xml.contains("<a:fillToRect l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>"));
        assert!(xml.contains("<a:srgbClr val=\"FF0000\"/>"));
        assert!(xml.contains("<a:srgbClr val=\"0000FF\"/>"));
    }
}

//! The `dml_shape` mapper: a DrawingML shape ([`DmlShape`] — Word's modern
//! `wps:wsp`) → a native Typst shape call.
//!
//! This is the inverse of `typst-docx`'s own `mappers::shape`, and the two
//! halves line up construct for construct: `a:custGeom`'s
//! `a:moveTo`/`a:lnTo`/`a:cubicBezTo`/`a:close` are exactly Typst's
//! `curve.move`/`curve.line`/`curve.cubic`/`curve.close`, `a:prstGeom`'s
//! presets are the primitives (`#rect`/`#ellipse`/`#polygon`/`#line`) the
//! exporter picks them for, and `a:solidFill`/`a:gradFill`/`a:ln` are
//! `fill:`/`stroke:`. So a shape Typst exported comes back as the shape it
//! started as, and a shape Word authored comes back as the nearest Typst
//! primitive rather than disappearing.
//!
//! Like the VML mapper this sits beside, the call is built as a ready-made
//! string ([`Inline::Shape`]'s `call`) because it is already a closed
//! expression with nothing for a tier-2 pass to promote. Unlike that one, a
//! shape can also *contain* text, which rides along as real blocks so the
//! passes can still reach it.
//!
//! **Position is deliberately not reproduced.** Word anchors a shape at a
//! page- or margin-relative offset, and Typst's `#place` reserves no space, so
//! honoring the offset would drop the shape on top of the body text. Every
//! shape is drawn inline at its anchor and the placement is reported as lost —
//! the same call `crate::tdoc::Inline::TextBox` already made, made the same
//! way.

use ecow::eco_format;
use typst_ooxml_core::units::emu_to_abs;

use crate::emit::{num, pt, rgba_lit};
use crate::lower::{lower_items, LowerCtx};
use crate::report::ImportReport;
use crate::tdoc::{Block, Inline};
use crate::wml::model::{
    DmlDash, DmlFill, DmlGeometry, DmlGradient, DmlGradientKind, DmlSeg, DmlShape, DmlStroke,
};

/// Word's own default line width when an `a:ln` states none (¾pt), used only
/// to scale an `a:custDash`, whose run lengths are relative to it.
const DEFAULT_LINE_WIDTH_PT: f64 = 0.75;

/// Lower one DrawingML shape, appending what it becomes to `out`.
///
/// Appends rather than returns because a shape can produce *two* inlines: the
/// primitives with no body parameter (`#line`, `#curve`) can't carry the text
/// Word put inside them, so that text follows as its own `Inline::TextBox`
/// rather than being thrown away. Appends *nothing* for geometry with no Typst
/// counterpart, which is recorded as a drop.
pub(crate) fn lower_dml_shape(shape: &DmlShape, ctx: &mut LowerCtx, out: &mut Vec<Inline>) {
    let width_pt = shape.cx_emu.map(emu_pt).filter(|w| *w > 0.0);
    let height_pt = shape.cy_emu.map(emu_pt).filter(|h| *h > 0.0);
    let style = Style::of(shape, &mut *ctx.report);

    let Some(call) = shape_call(shape, width_pt, height_pt, &style, &mut *ctx.report) else {
        return;
    };

    // A shape's text is ordinary body content, lowered the same way a table
    // cell's or a text box's is — including the container guard, since a
    // `#pagebreak()` is no more legal inside a shape than inside either of
    // those.
    let body: Vec<Block> = if shape.body.is_empty() {
        Vec::new()
    } else {
        let was_in_container = ctx.enter_container();
        let blocks = lower_items(&shape.body, ctx);
        ctx.exit_container(was_in_container);
        blocks
    };

    ctx.report.approximate(
        "DrawingML shape",
        "floating position not preserved; drawn inline at the anchor point",
    );

    if body.is_empty() || call.takes_body {
        out.push(Inline::Shape { call: call.src.into(), body });
    } else {
        // `#line`/`#curve` have no body parameter, so the text can only follow
        // the shape instead of sitting inside it.
        out.push(Inline::Shape { call: call.src.into(), body: Vec::new() });
        out.push(Inline::TextBox(body));
        ctx.report.approximate(
            "DrawingML shape",
            "text inside a line/curve shape cannot sit in it; kept beside it",
        );
    }
}

/// A rendered shape call, and whether the Typst function it names accepts a
/// trailing content block. `#rect`/`#ellipse`/`#circle` do; `#polygon`,
/// `#line`, and `#curve` don't.
struct ShapeCall {
    src: String,
    takes_body: bool,
}

impl ShapeCall {
    fn with_body(src: String) -> Self {
        ShapeCall { src, takes_body: true }
    }

    fn bare(src: String) -> Self {
        ShapeCall { src, takes_body: false }
    }
}

/// The `fill:`/`stroke:` arguments a shape's paint resolves to, each already
/// formatted as the *value* half (so callers only ever prepend the name).
/// `None` omits the argument, leaving Typst's own default — which for a shape
/// means "no fill" and "a 1pt black stroke unless a fill was given", matching
/// what Word draws for an unstated one closely enough not to invent anything.
#[derive(Default)]
struct Style {
    fill: Option<String>,
    stroke: Option<String>,
}

impl Style {
    fn of(shape: &DmlShape, report: &mut ImportReport) -> Self {
        Style {
            fill: fill_literal(&shape.fill, report),
            stroke: shape.stroke.as_ref().and_then(|s| stroke_literal(s, report)),
        }
    }

    /// Push whichever of the two are set onto an argument list.
    fn push_onto(&self, args: &mut Vec<String>) {
        if let Some(fill) = &self.fill {
            args.push(format!("fill: {fill}"));
        }
        if let Some(stroke) = &self.stroke {
            args.push(format!("stroke: {stroke}"));
        }
    }
}

/// The Typst call for a shape's geometry, or `None` (with a drop recorded) for
/// geometry that has no counterpart — a preset this mapper doesn't cover, or a
/// path too degenerate to draw. Guessing at an uncovered preset's outline is
/// deliberately not done: a rectangle standing in for a callout is a wrong
/// drawing, which is worse than a recorded absence.
fn shape_call(
    shape: &DmlShape,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
    style: &Style,
    report: &mut ImportReport,
) -> Option<ShapeCall> {
    match &shape.geom {
        DmlGeometry::Custom { path_w, path_h, segments } => {
            let points = path_points(segments, *path_w, *path_h, width_pt, height_pt);
            custom_call(&points, style, report)
        }
        DmlGeometry::Preset(geom) => {
            preset_call(&geom.prst, geom.adj, width_pt, height_pt, style, report)
        }
    }
}

// ---------------------------------------------------------------------------
// Custom geometry (`a:custGeom` → `#curve` / `#line`).
// ---------------------------------------------------------------------------

/// A path segment with its coordinates already in points.
enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    Cubic([(f64, f64); 3]),
    Close,
}

/// Scale a path's segments out of `a:path`'s own coordinate space and into
/// points.
///
/// `a:path/@w`/`@h` declare that space's extent, and the shape's `a:ext` is
/// what it renders at, so the ratio between them is the scale. When the shape
/// states no extent, the coordinates are read as EMU — which is what
/// `typst-docx`'s `write_custom_geom` writes, since it sets the path space
/// equal to the extent — rather than left unscaled and a thousand times too
/// large. A zero-extent axis (a horizontal rule's `h="1"`, say) collapses to
/// zero on that axis, which is exactly where its points already are.
fn path_points(
    segments: &[DmlSeg],
    path_w: i64,
    path_h: i64,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
) -> Vec<Seg> {
    let axis = |path: i64, rendered: Option<f64>| -> f64 {
        match rendered {
            Some(rendered) if path > 0 => rendered / path as f64,
            Some(_) => 0.0,
            None => emu_pt(1),
        }
    };
    let (sx, sy) = (axis(path_w, width_pt), axis(path_h, height_pt));
    let at = |x: i64, y: i64| (x as f64 * sx, y as f64 * sy);

    segments
        .iter()
        .map(|seg| match *seg {
            DmlSeg::MoveTo(x, y) => {
                let (x, y) = at(x, y);
                Seg::Move(x, y)
            }
            DmlSeg::LineTo(x, y) => {
                let (x, y) = at(x, y);
                Seg::Line(x, y)
            }
            DmlSeg::CubicTo(x1, y1, x2, y2, x, y) => {
                Seg::Cubic([at(x1, y1), at(x2, y2), at(x, y)])
            }
            DmlSeg::Close => Seg::Close,
        })
        .collect()
}

/// A custom path as `#line` (for the two-segment straight case `typst-docx`
/// writes for a diagonal `#line`, where a whole `#curve` would be noise) or
/// `#curve` (for everything else).
fn custom_call(
    points: &[Seg],
    style: &Style,
    report: &mut ImportReport,
) -> Option<ShapeCall> {
    // A path must open with a move: a `#curve` whose first item isn't
    // `curve.move` silently starts at the origin, which for a path whose
    // opening command was lost is a different shape, not this one.
    if !matches!(points.first(), Some(Seg::Move(..))) {
        report.drop("DrawingML shape", "custom path does not begin with a move; dropped");
        return None;
    }

    if let [Seg::Move(x0, y0), Seg::Line(x1, y1)] = points {
        let mut args = Vec::new();
        if *x0 != 0.0 || *y0 != 0.0 {
            args.push(format!("start: ({}, {})", pt(*x0), pt(*y0)));
        }
        args.push(format!("end: ({}, {})", pt(*x1), pt(*y1)));
        if let Some(stroke) = &style.stroke {
            args.push(format!("stroke: {stroke}"));
        }
        return Some(ShapeCall::bare(format!("#line({})", args.join(", "))));
    }

    let mut args = Vec::new();
    style.push_onto(&mut args);
    for seg in points {
        args.push(match seg {
            Seg::Move(x, y) => format!("curve.move(({}, {}))", pt(*x), pt(*y)),
            Seg::Line(x, y) => format!("curve.line(({}, {}))", pt(*x), pt(*y)),
            Seg::Cubic([c1, c2, end]) => format!(
                "curve.cubic(({}, {}), ({}, {}), ({}, {}))",
                pt(c1.0),
                pt(c1.1),
                pt(c2.0),
                pt(c2.1),
                pt(end.0),
                pt(end.1)
            ),
            Seg::Close => "curve.close()".to_string(),
        });
    }
    Some(ShapeCall::bare(format!("#curve({})", args.join(", "))))
}

// ---------------------------------------------------------------------------
// Preset geometry (`a:prstGeom` → the Typst primitives).
// ---------------------------------------------------------------------------

/// One named preset as its Typst primitive.
///
/// The polygon presets are built from their ECMA-376 *default* adjustments —
/// the only ones `PresetGeom` records is the first, which is all `roundRect`
/// needs — so a star or arrow whose proportions the author tuned comes back at
/// the stock proportions. That is a visual approximation of a shape that is
/// otherwise right, and it is reported as one; the alternative (dropping it)
/// loses the shape altogether.
fn preset_call(
    prst: &str,
    adj: Option<i64>,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
    style: &Style,
    report: &mut ImportReport,
) -> Option<ShapeCall> {
    let mut args = Vec::new();

    match prst {
        "rect" | "roundRect" => {
            if let Some(width) = width_pt {
                args.push(format!("width: {}", pt(width)));
            }
            if let Some(height) = height_pt {
                args.push(format!("height: {}", pt(height)));
            }
            if prst == "roundRect"
                && let (Some(width), Some(height)) = (width_pt, height_pt)
            {
                // The inverse of `typst_ooxml_core::dml::round_rect_adj`: the
                // adjustment is 1000ths of a percent of the shorter side.
                let adj = adj.unwrap_or(16667).clamp(0, 50_000) as f64;
                args.push(format!("radius: {}", pt(adj / 100_000.0 * width.min(height))));
            }
            style.push_onto(&mut args);
            Some(ShapeCall::with_body(format!("#rect({})", args.join(", "))))
        }
        "ellipse" => {
            // A circle is the more idiomatic call for what is visually one —
            // the same choice `mappers::shape` makes for a square `v:oval`.
            match (width_pt, height_pt) {
                (Some(w), Some(h)) if (w - h).abs() < 0.01 => {
                    args.push(format!("radius: {}", pt(w / 2.0)));
                    style.push_onto(&mut args);
                    Some(ShapeCall::with_body(format!("#circle({})", args.join(", "))))
                }
                _ => {
                    if let Some(width) = width_pt {
                        args.push(format!("width: {}", pt(width)));
                    }
                    if let Some(height) = height_pt {
                        args.push(format!("height: {}", pt(height)));
                    }
                    style.push_onto(&mut args);
                    Some(ShapeCall::with_body(format!("#ellipse({})", args.join(", "))))
                }
            }
        }
        // A preset line spans its own bounding box corner to corner.
        "line" | "straightConnector1" => {
            let (w, h) = (width_pt?, height_pt.unwrap_or(0.0));
            args.push(format!("end: ({}, {})", pt(w), pt(h)));
            if let Some(stroke) = &style.stroke {
                args.push(format!("stroke: {stroke}"));
            }
            Some(ShapeCall::bare(format!("#line({})", args.join(", "))))
        }
        "triangle" | "diamond" | "star5" | "rightArrow" => {
            let (w, h) = (width_pt?, height_pt?);
            let vertices = preset_vertices(prst, w, h)?;
            if prst != "triangle" && prst != "diamond" {
                report.approximate(
                    "DrawingML shape",
                    eco_format!("`{prst}` drawn at its default proportions"),
                );
            }
            style.push_onto(&mut args);
            for (x, y) in vertices {
                args.push(format!("({}, {})", pt(x), pt(y)));
            }
            Some(ShapeCall::bare(format!("#polygon({})", args.join(", "))))
        }
        other => {
            report.drop(
                "DrawingML shape",
                eco_format!("`{other}` preset geometry is not reproduced"),
            );
            None
        }
    }
}

/// ECMA-376's `star5`, as offsets from the shape's centre in units of its
/// half-width and half-height: five outer points inscribed in the box starting
/// straight up, five inner points at the stock 0.38196 (2 − φ) of that radius,
/// alternating. Tabulated rather than computed because the repository forbids
/// `f64::sin`/`cos` (non-deterministic across platforms) and a fixed ten-point
/// star has nothing to compute at run time anyway.
#[rustfmt::skip]
const STAR5_UNIT_VERTICES: [(f64, f64); 10] = [
    ( 0.0,         -1.0        ),
    ( 0.22451399,  -0.30901699),
    ( 0.95105652,  -0.30901699),
    ( 0.36327126,   0.11803399),
    ( 0.58778525,   0.80901699),
    ( 0.0,          0.38196601),
    (-0.58778525,   0.80901699),
    (-0.36327126,   0.11803399),
    (-0.95105652,  -0.30901699),
    (-0.22451399,  -0.30901699),
];

/// A polygon preset's vertices in points, laid out in its `w × h` box with the
/// origin at the top left — DrawingML's own coordinate convention, which is
/// also Typst's.
fn preset_vertices(prst: &str, w: f64, h: f64) -> Option<Vec<(f64, f64)>> {
    Some(match prst {
        "triangle" => vec![(w / 2.0, 0.0), (w, h), (0.0, h)],
        "diamond" => vec![(w / 2.0, 0.0), (w, h / 2.0), (w / 2.0, h), (0.0, h / 2.0)],
        "star5" => {
            let (cx, cy) = (w / 2.0, h / 2.0);
            STAR5_UNIT_VERTICES
                .iter()
                .map(|(dx, dy)| (cx + dx * cx, cy + dy * cy))
                .collect()
        }
        "rightArrow" => {
            // Stock adjustments: the shaft is half the height, and the head is
            // half the shorter side long.
            let shaft = h / 2.0;
            let (top, bottom) = ((h - shaft) / 2.0, (h + shaft) / 2.0);
            let neck = (w - w.min(h) / 2.0).max(0.0);
            vec![
                (0.0, top),
                (neck, top),
                (neck, 0.0),
                (w, h / 2.0),
                (neck, h),
                (neck, bottom),
                (0.0, bottom),
            ]
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Paint.
// ---------------------------------------------------------------------------

fn fill_literal(fill: &DmlFill, report: &mut ImportReport) -> Option<String> {
    match fill {
        // Both leave `fill:` unstated, which is Typst's "no fill" — right for
        // `a:noFill`, and the only safe reading of a fill this importer
        // couldn't resolve (a theme color, a pattern, a picture fill), since
        // any color it picked instead would be invented.
        DmlFill::None => None,
        DmlFill::Unstated => None,
        DmlFill::Solid(rgba) => Some(rgba_lit(*rgba)),
        DmlFill::Gradient(gradient) => Some(gradient_literal(gradient, report)),
    }
}

/// An `a:gradFill` as a Typst `gradient.linear`/`gradient.radial`.
///
/// The stop list is reproduced verbatim — `typst-docx` expands a Typst
/// gradient into many stops precisely so consumers with a different
/// interpolation space see the authored ramp, and collapsing them back to the
/// endpoints would undo that.
fn gradient_literal(
    gradient: &DmlGradient,
    report: &mut ImportReport,
) -> String {
    let stops: Vec<String> = gradient
        .stops
        .iter()
        .map(|(pos, rgba)| {
            format!("({}, {}%)", rgba_lit(*rgba), num(*pos as f64 / 1000.0))
        })
        .collect();

    match gradient.kind {
        DmlGradientKind::Linear { angle_60k } => {
            let mut args = stops;
            if angle_60k != 0 {
                args.push(format!("angle: {}deg", num(angle_60k as f64 / 60_000.0)));
            }
            format!("gradient.linear({})", args.join(", "))
        }
        DmlGradientKind::Radial { fill_to_rect } => {
            // `a:fillToRect` insets the gradient's focal rectangle from each
            // edge; `typst_ooxml_core::dml`'s `radial_focus_rect_100k` builds
            // it from a focal center and radius, and this inverts that. The
            // *outer* radius has no counterpart to recover: DrawingML always
            // runs the ramp out to the shape's bounding box, while Typst's
            // default is a circle of half the shape's extent.
            let [l, t, r, b] = fill_to_rect.map(|v| v as f64 / 1000.0);
            // The focal rectangle is square whenever `radial_focus_rect_100k`
            // built it, so the two axes agree; averaging them keeps a
            // hand-authored non-square one from landing off-centre, since
            // Typst's focal region is a circle either way.
            let focal_radius =
                (((100.0 - l - r) / 2.0).max(0.0) + ((100.0 - t - b) / 2.0).max(0.0)) / 2.0;
            let (cx, cy) = (l + (100.0 - l - r) / 2.0, t + (100.0 - t - b) / 2.0);

            let mut args = stops;
            if (cx - 50.0).abs() > 0.01 || (cy - 50.0).abs() > 0.01 {
                args.push(format!("focal-center: ({}%, {}%)", num(cx), num(cy)));
            }
            if focal_radius > 0.01 {
                args.push(format!("focal-radius: {}%", num(focal_radius)));
            }
            report.approximate(
                "gradient fill",
                "a radial gradient's extent is measured differently by Word; \
                 the ramp may end slightly short of where Word ends it",
            );
            format!("gradient.radial({})", args.join(", "))
        }
    }
}

/// An `a:ln` as a Typst `stroke:` value, or `None` to leave the argument off
/// entirely (an `a:ln` that states nothing this mapper reads — no color, no
/// width, no dash — is indistinguishable from Typst's own default).
fn stroke_literal(
    stroke: &DmlStroke,
    report: &mut ImportReport,
) -> Option<String> {
    if stroke.no_fill {
        // Explicit: without this, Typst's `auto` would draw a border on an
        // unfilled shape that Word left bare.
        return Some("none".to_string());
    }

    let thickness_pt = stroke.w_emu.filter(|w| *w > 0).map(emu_pt);
    let mut parts = Vec::new();
    if let Some(color) = stroke.color {
        parts.push(format!("paint: {}", rgba_lit(color)));
    }
    if let Some(thickness) = thickness_pt {
        parts.push(format!("thickness: {}", pt(thickness)));
    }
    if let Some(dash) = stroke.dash.as_ref().and_then(|dash| {
        dash_literal(dash, thickness_pt.unwrap_or(DEFAULT_LINE_WIDTH_PT), report)
    }) {
        parts.push(format!("dash: {dash}"));
    }

    match parts.as_slice() {
        [] => None,
        // A one-entry dictionary is still a dictionary in Typst (the colon
        // makes it one), so this needs no special case to stay valid.
        parts => Some(format!("({})", parts.join(", "))),
    }
}

/// A dash pattern. `a:custDash` states exact run lengths as multiples of the
/// line width, which Typst takes as an array of lengths — an exact
/// round-trip of `typst_ooxml_core::dml::custom_dash`. `a:prstDash` names a
/// pattern instead, mapped onto Typst's nearest named dash.
fn dash_literal(
    dash: &DmlDash,
    thickness_pt: f64,
    report: &mut ImportReport,
) -> Option<String> {
    match dash {
        DmlDash::Custom(stops) => {
            let lengths: Vec<String> = stops
                .iter()
                .flat_map(|(d, sp)| [*d, *sp])
                .map(|run| pt(run as f64 / 100_000.0 * thickness_pt))
                .collect();
            (!lengths.is_empty()).then(|| format!("({})", lengths.join(", ")))
        }
        DmlDash::Preset(preset) => {
            let name = preset_dash_name(preset)?;
            if matches!(preset.as_str(), "lgDashDotDot" | "sysDashDotDot") {
                report.approximate(
                    "dash pattern",
                    eco_format!("`{preset}` has no Typst counterpart; drawn as `{name}`"),
                );
            }
            Some(format!("\"{name}\""))
        }
    }
}

/// `a:prstDash/@val` → Typst's named dash patterns. `None` for `solid` (which
/// is the absence of a dash, not a pattern) and for any value outside the
/// ECMA-376 vocabulary. The `sys*` family is a *system* dash whose exact run
/// lengths the consumer chooses, so it maps onto the same visual family rather
/// than a distinct one.
fn preset_dash_name(preset: &str) -> Option<&'static str> {
    Some(match preset {
        "dot" | "sysDot" => "dotted",
        "dash" | "sysDash" => "dashed",
        "lgDash" => "loosely-dashed",
        "dashDot" | "sysDashDot" => "dash-dotted",
        "lgDashDot" => "loosely-dash-dotted",
        // Typst has no dash-dot-dot; the closest family is dash-dot, and the
        // caller reports the loss.
        "lgDashDotDot" => "loosely-dash-dotted",
        "sysDashDotDot" => "dash-dotted",
        _ => return None,
    })
}

/// EMU → points. `Abs` is the crate-wide unit bridge, so this keeps the same
/// conversion `mappers::drawing` uses for a picture's extent.
fn emu_pt(emu: i64) -> f64 {
    emu_to_abs(emu as f64).to_pt()
}

/// A DrawingML shape whose geometry this importer could not paint and which
/// carried no text either — recorded rather than left to vanish. Lives here
/// rather than in `mappers::run` so the wording stays beside the mapping it
/// describes.
pub(crate) fn report_unsupported(report: &mut ImportReport) {
    report.drop(
        "DrawingML shape",
        "no resolvable fill or outline color (a theme color) and no text; dropped",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::ImportOptions;
    use ImportReport;
    use crate::wml::model::{PresetGeom, WmlPackage};

    fn shape(geom: DmlGeometry) -> DmlShape {
        DmlShape {
            geom,
            cx_emu: Some(2_540_000),
            cy_emu: Some(558_800),
            fill: DmlFill::Unstated,
            stroke: None,
            body: Vec::new(),
        }
    }

    fn preset(prst: &str) -> DmlGeometry {
        DmlGeometry::Preset(PresetGeom { prst: prst.into(), adj: None })
    }

    /// Lower `shape` and return the single shape call it produced.
    fn call_of(shape: &DmlShape) -> String {
        let package = WmlPackage::default();
        let options = ImportOptions::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let mut out = Vec::new();
        lower_dml_shape(shape, &mut ctx, &mut out);
        match out.as_slice() {
            [Inline::Shape { call, .. }] => call.to_string(),
            other => panic!("expected exactly one shape, got {other:?}"),
        }
    }

    #[test]
    fn a_rect_preset_keeps_its_extent() {
        assert_eq!(call_of(&shape(preset("rect"))), "#rect(width: 200pt, height: 44pt)");
    }

    /// `roundRect`'s adjustment is 1000ths of a percent of the *shorter* side
    /// — the inverse of `typst_ooxml_core::dml::round_rect_adj`.
    #[test]
    fn a_round_rect_adjustment_becomes_a_corner_radius() {
        let geom = DmlGeometry::Preset(PresetGeom { prst: "roundRect".into(), adj: Some(16667) });
        let mut s = shape(geom);
        s.cx_emu = Some(1_778_000);
        s.cy_emu = Some(762_000);
        // 16667/100000 of the shorter side (60pt).
        assert_eq!(call_of(&s), "#rect(width: 140pt, height: 60pt, radius: 10pt)");
    }

    #[test]
    fn a_square_ellipse_becomes_a_circle() {
        let mut s = shape(preset("ellipse"));
        s.cx_emu = Some(1_016_000);
        s.cy_emu = Some(1_016_000);
        assert_eq!(call_of(&s), "#circle(radius: 40pt)");
    }

    /// A solid fill's `a:alpha` survives as Typst's eight-digit hex form —
    /// without it, a 50%-transparent shape would come back opaque.
    #[test]
    fn a_translucent_solid_fill_keeps_its_alpha() {
        let mut s = shape(preset("rect"));
        s.fill = DmlFill::Solid([0x1E, 0x78, 0xC8, 0x80]);
        assert!(call_of(&s).contains("fill: rgb(\"1E78C880\")"), "{}", call_of(&s));
    }

    /// The two-segment straight path `typst-docx` writes for a diagonal
    /// `#line` comes back as a `#line`, not as a one-segment `#curve`.
    #[test]
    fn a_two_point_custom_path_becomes_a_line() {
        let geom = DmlGeometry::Custom {
            path_w: 2_286_000,
            path_h: 381_000,
            segments: vec![DmlSeg::MoveTo(0, 0), DmlSeg::LineTo(2_286_000, 381_000)],
        };
        let mut s = shape(geom);
        s.cx_emu = Some(2_286_000);
        s.cy_emu = Some(381_000);
        s.stroke = Some(DmlStroke {
            w_emu: Some(19050),
            color: Some([0x00, 0x74, 0xD9, 255]),
            ..Default::default()
        });
        assert_eq!(
            call_of(&s),
            "#line(end: (180pt, 30pt), stroke: (paint: rgb(\"0074D9\"), thickness: 1.5pt))"
        );
    }

    /// A real path maps command-for-command onto `#curve`, with the path's own
    /// coordinate space scaled onto the shape's extent.
    #[test]
    fn a_multi_segment_custom_path_becomes_a_curve() {
        let geom = DmlGeometry::Custom {
            path_w: 1_270_000,
            path_h: 1_270_000,
            segments: vec![
                DmlSeg::MoveTo(0, 0),
                DmlSeg::LineTo(1_270_000, 0),
                DmlSeg::CubicTo(1_270_000, 635_000, 635_000, 1_270_000, 0, 1_270_000),
                DmlSeg::Close,
            ],
        };
        let mut s = shape(geom);
        s.cx_emu = Some(1_270_000);
        s.cy_emu = Some(1_270_000);
        assert_eq!(
            call_of(&s),
            "#curve(curve.move((0pt, 0pt)), curve.line((100pt, 0pt)), \
             curve.cubic((100pt, 50pt), (50pt, 100pt), (0pt, 100pt)), curve.close())"
        );
    }

    /// `a:custDash`'s runs are multiples of the line width, so they only mean
    /// anything once scaled by it.
    #[test]
    fn a_custom_dash_is_scaled_by_the_line_width() {
        let mut s = shape(preset("rect"));
        s.stroke = Some(DmlStroke {
            w_emu: Some(25400),
            color: Some([0xC0, 0x39, 0x2B, 255]),
            dash: Some(DmlDash::Custom(vec![(150_000, 150_000)])),
            ..Default::default()
        });
        assert!(call_of(&s).contains("dash: (3pt, 3pt)"), "{}", call_of(&s));
    }

    /// An `a:ln/a:noFill` has to be stated: Typst's `auto` stroke would
    /// otherwise draw a border Word explicitly switched off.
    #[test]
    fn an_explicitly_unstroked_shape_says_so() {
        let mut s = shape(preset("rect"));
        s.fill = DmlFill::Solid([1, 2, 3, 255]);
        s.stroke = Some(DmlStroke { no_fill: true, ..Default::default() });
        assert!(call_of(&s).contains("stroke: none"), "{}", call_of(&s));
    }

    /// A preset with no Typst counterpart is dropped *and recorded* — never
    /// approximated by a shape of a different outline.
    #[test]
    fn an_uncovered_preset_is_dropped_with_a_note() {
        let package = WmlPackage::default();
        let options = ImportOptions::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let mut out = Vec::new();
        lower_dml_shape(&shape(preset("wedgeRoundRectCallout")), &mut ctx, &mut out);
        assert!(out.is_empty());
        assert!(report.notes.iter().any(|n| n.detail.contains("wedgeRoundRectCallout")));
    }
}

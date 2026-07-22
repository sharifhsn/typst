//! DrawingML geometry → a Typst shape call.
//!
//! PowerPoint states a shape as a *named preset* plus adjustment guides, or as
//! an explicit path. The presets are a closed vocabulary of ~180 names and
//! Typst has four primitives, so this maps the ones that correspond exactly
//! and falls back to the bounding rectangle for the rest — reported, because a
//! star drawn as a rectangle is a visible lie if nobody says so.

use ecow::{eco_format, EcoString};

use crate::emit::{len, paint};
use crate::lower::{emu, lower_fill, LowerCtx};
use crate::pml::model::*;
use crate::tdoc;

/// Build the Typst call for a shape's own drawing, if it draws anything.
///
/// `None` means "this shape is a text container with no visible frame", which
/// is the overwhelmingly common case: PowerPoint gives every text box a
/// `prstGeom` of `rect` with no fill and no line.
pub fn lower(shape: &TextShape, ctx: &mut LowerCtx<'_, '_>) -> Option<EcoString> {
    let fill = shape.fill.as_ref().and_then(|f| lower_fill(f, ctx));
    let stroke = shape.line.as_ref().and_then(|l| lower_stroke(l, ctx));
    let geom = shape.geom.as_ref();

    // Nothing to draw: no paint, no outline. Emitting an empty `rect` would
    // add a Typst-default 1pt border PowerPoint never drew — the same class of
    // bug as the Word importer's borderless-table regression.
    if fill.is_none() && stroke.is_none() {
        return None;
    }

    let size = shape.xfrm.map(|x| (emu(x.cx), emu(x.cy)));

    // A line is not a box: `line` takes `start`/`end`, and handing it
    // `width`/`height` is a hard error rather than an ignored argument.
    if is_line_preset(shape.geom.as_ref()) {
        let (w, h) = size.unwrap_or((0.0, 0.0));
        let mut line_params = vec![format!("end: ({}, {})", len(w), len(h))];
        if let Some(stroke) = &stroke {
            line_params.push(format!("stroke: {stroke}"));
        }
        return Some(eco_format!("line({})", line_params.join(", ")));
    }

    let mut params = Vec::new();
    if let Some((w, h)) = size {
        params.push(format!("width: {}", len(w)));
        params.push(format!("height: {}", len(h)));
    }
    if let Some(p) = &fill {
        params.push(format!("fill: {}", paint(p)));
    }
    if let Some(stroke) = &stroke {
        params.push(format!("stroke: {stroke}"));
    }

    let call = match geom {
        // A path whose first element is not a `move` is not a path Typst can
        // start drawing, and one with no elements at all is not a path — both
        // occur in real decks, and both are a hard compile error rather than
        // an empty shape.
        Some(Geometry::Custom { w, h, segs })
            if matches!(segs.first(), Some(Seg::Move(..))) =>
        {
            return Some(custom_curve(*w, *h, segs, size, &fill, &stroke));
        }
        Some(Geometry::Preset { name, adjust }) => preset(name, adjust, &mut params, ctx, size),
        _ => "rect".into(),
    };
    Some(eco_format!("{call}({})", params.join(", ")))
}

/// Map a preset name onto a Typst primitive.
fn preset(
    name: &str,
    adjust: &[(EcoString, i64)],
    params: &mut Vec<String>,
    ctx: &mut LowerCtx<'_, '_>,
    size: Option<(f64, f64)>,
) -> EcoString {
    match name {
        "rect" => "rect".into(),
        "ellipse" => "ellipse".into(),
        "roundRect" => {
            // `adj` is the corner radius as a fraction of the shorter side,
            // in thousandths of a percent — the same encoding `typst-pptx`
            // writes on the way out, so a round-trip is exact.
            let adj = adjust
                .iter()
                .find(|(n, _)| n == "adj")
                .map(|(_, v)| *v)
                .unwrap_or(16667);
            if let Some((w, h)) = size {
                let radius = w.min(h) * (adj as f64 / 100_000.0);
                params.push(format!("radius: {}", len(radius)));
            }
            "rect".into()
        }
        _ => {
            // Most presets are just polygons, and PowerPoint states them as a
            // name because a name is smaller than a path. Drawing the real
            // outline is worth the table: a triangle rendered as a rectangle
            // is a visible lie, and these are the shapes decks actually use.
            if let Some(points) = preset_polygon(name) {
                return polygon(points, size, params);
            }
            ctx.report.approximate(
                "preset shape",
                eco_format!(
                    "`{name}` is a named PowerPoint preset with no Typst \
                     primitive and no polygon outline here; it is drawn as its \
                     bounding rectangle"
                ),
            );
            "rect".into()
        }
    }
}

/// Vertices for the presets that are plain polygons, as fractions of the
/// shape's own width and height.
///
/// Deliberately not parameterised by the preset's adjustment guides: the
/// guides shift a chevron's notch or an arrow's head, and reproducing the
/// default outline is far closer than reproducing a rectangle. A shape whose
/// guides were moved is drawn in its default proportions.
fn preset_polygon(name: &str) -> Option<&'static [(f64, f64)]> {
    Some(match name {
        "triangle" => &[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)],
        "rtTriangle" => &[(0.0, 0.0), (0.0, 1.0), (1.0, 1.0)],
        "diamond" => &[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)],
        "parallelogram" => &[(0.25, 0.0), (1.0, 0.0), (0.75, 1.0), (0.0, 1.0)],
        "trapezoid" => &[(0.25, 0.0), (0.75, 0.0), (1.0, 1.0), (0.0, 1.0)],
        "pentagon" => &[
            (0.5, 0.0),
            (1.0, 0.382),
            (0.809, 1.0),
            (0.191, 1.0),
            (0.0, 0.382),
        ],
        "hexagon" => &[
            (0.25, 0.0),
            (0.75, 0.0),
            (1.0, 0.5),
            (0.75, 1.0),
            (0.25, 1.0),
            (0.0, 0.5),
        ],
        "octagon" => &[
            (0.293, 0.0),
            (0.707, 0.0),
            (1.0, 0.293),
            (1.0, 0.707),
            (0.707, 1.0),
            (0.293, 1.0),
            (0.0, 0.707),
            (0.0, 0.293),
        ],
        "rightArrow" => &[
            (0.0, 0.25),
            (0.5, 0.25),
            (0.5, 0.0),
            (1.0, 0.5),
            (0.5, 1.0),
            (0.5, 0.75),
            (0.0, 0.75),
        ],
        "leftArrow" => &[
            (1.0, 0.25),
            (0.5, 0.25),
            (0.5, 0.0),
            (0.0, 0.5),
            (0.5, 1.0),
            (0.5, 0.75),
            (1.0, 0.75),
        ],
        "upArrow" => &[
            (0.25, 1.0),
            (0.25, 0.5),
            (0.0, 0.5),
            (0.5, 0.0),
            (1.0, 0.5),
            (0.75, 0.5),
            (0.75, 1.0),
        ],
        "downArrow" => &[
            (0.25, 0.0),
            (0.25, 0.5),
            (0.0, 0.5),
            (0.5, 1.0),
            (1.0, 0.5),
            (0.75, 0.5),
            (0.75, 0.0),
        ],
        "chevron" | "homePlate" => &[
            (0.0, 0.0),
            (0.75, 0.0),
            (1.0, 0.5),
            (0.75, 1.0),
            (0.0, 1.0),
        ],
        "plus" => &[
            (0.35, 0.0),
            (0.65, 0.0),
            (0.65, 0.35),
            (1.0, 0.35),
            (1.0, 0.65),
            (0.65, 0.65),
            (0.65, 1.0),
            (0.35, 1.0),
            (0.35, 0.65),
            (0.0, 0.65),
            (0.0, 0.35),
            (0.35, 0.35),
        ],
        "star5" => &[
            (0.5, 0.0),
            (0.618, 0.382),
            (1.0, 0.382),
            (0.691, 0.618),
            (0.809, 1.0),
            (0.5, 0.764),
            (0.191, 1.0),
            (0.309, 0.618),
            (0.0, 0.382),
            (0.382, 0.382),
        ],
        _ => return None,
    })
}

/// Emit a polygon as a closed `curve`, scaled to the shape's box.
fn polygon(
    points: &[(f64, f64)],
    size: Option<(f64, f64)>,
    params: &mut Vec<String>,
) -> EcoString {
    let (w, h) = size.unwrap_or((0.0, 0.0));
    // `width`/`height` were pushed for a box; a curve takes neither.
    params.retain(|p| !p.starts_with("width:") && !p.starts_with("height:"));
    let mut parts: Vec<String> = Vec::new();
    for (i, (fx, fy)) in points.iter().enumerate() {
        let point = format!("({}, {})", len(fx * w), len(fy * h));
        parts.push(if i == 0 {
            format!("curve.move({point})")
        } else {
            format!("curve.line({point})")
        });
    }
    parts.push("curve.close()".into());
    params.extend(parts);
    "curve".into()
}

/// `a:custGeom` → `#curve`, command for command.
///
/// The path is authored in its own coordinate space (`a:path/@w`,`@h`), which
/// is almost never the shape's on-slide size, so every point is scaled.
fn custom_curve(
    path_w: Emu,
    path_h: Emu,
    segs: &[Seg],
    size: Option<(f64, f64)>,
    fill: &Option<tdoc::Paint>,
    stroke: &Option<String>,
) -> EcoString {
    let (w, h) = size.unwrap_or((emu(path_w), emu(path_h)));
    let sx = if path_w != 0 { w / emu(path_w) } else { 1.0 };
    let sy = if path_h != 0 { h / emu(path_h) } else { 1.0 };
    let pt = |x: Emu, y: Emu| format!("({}, {})", len(emu(x) * sx), len(emu(y) * sy));

    let mut parts = Vec::new();
    for seg in segs {
        parts.push(match seg {
            Seg::Move(x, y) => format!("curve.move({})", pt(*x, *y)),
            Seg::Line(x, y) => format!("curve.line({})", pt(*x, *y)),
            Seg::Cubic(c1x, c1y, c2x, c2y, ex, ey) => format!(
                "curve.cubic({}, {}, {})",
                pt(*c1x, *c1y),
                pt(*c2x, *c2y),
                pt(*ex, *ey)
            ),
            Seg::Close => "curve.close()".into(),
        });
    }

    let mut params = Vec::new();
    if let Some(p) = fill {
        params.push(format!("fill: {}", paint(p)));
    }
    if let Some(s) = stroke {
        params.push(format!("stroke: {s}"));
    }
    params.extend(parts);
    eco_format!("curve({})", params.join(", "))
}

fn lower_stroke(line: &Line, ctx: &mut LowerCtx<'_, '_>) -> Option<String> {
    // An explicit `a:noFill` on the line means "no outline" — distinct from
    // stating no line at all, which inherits one.
    if matches!(line.fill, Some(Fill::None)) {
        return None;
    }
    let paint_value = line.fill.as_ref().and_then(|f| lower_fill(f, ctx));
    let width = line.width.map(emu);
    if paint_value.is_none() && width.is_none() && line.dash.is_none() {
        return None;
    }

    let mut params = Vec::new();
    if let Some(p) = &paint_value {
        params.push(format!("paint: {}", paint(p)));
    }
    if let Some(w) = width {
        params.push(format!("thickness: {}", len(w)));
    }
    if !line.custom_dash.is_empty() {
        // `a:custDash` run lengths are multiples of the line width, so they
        // only become absolute once that width is known.
        let unit = width.unwrap_or(0.75);
        let pattern: Vec<String> = line
            .custom_dash
            .iter()
            .flat_map(|(d, sp)| {
                [
                    len(*d as f64 / 100_000.0 * unit),
                    len(*sp as f64 / 100_000.0 * unit),
                ]
            })
            .collect();
        params.push(format!("dash: ({})", pattern.join(", ")));
    } else if let Some(name) = line.dash.as_deref().and_then(preset_dash) {
        params.push(format!("dash: \"{name}\""));
    }
    if params.is_empty() {
        return None;
    }
    Some(format!("({})", params.join(", ")))
}

/// `a:prstDash/@val` → Typst's named dash patterns.
fn preset_dash(value: &str) -> Option<&'static str> {
    Some(match value {
        "solid" => return None,
        "dot" | "sysDot" => "dotted",
        "dash" | "sysDash" => "dashed",
        "lgDash" => "loosely-dashed",
        "dashDot" | "sysDashDot" => "dash-dotted",
        "lgDashDot" | "lgDashDotDot" | "sysDashDotDot" => "loosely-dash-dotted",
        _ => return None,
    })
}

/// Whether this geometry is a straight line rather than a closed box.
fn is_line_preset(geom: Option<&Geometry>) -> bool {
    matches!(geom, Some(Geometry::Preset { name, .. })
        if name == "line" || name.starts_with("straightConnector"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solid_preset_dash_produces_no_dash_argument() {
        // "solid" is the absence of a pattern, not a pattern named solid.
        assert_eq!(preset_dash("solid"), None);
        assert_eq!(preset_dash("dash"), Some("dashed"));
        assert_eq!(preset_dash("sysDot"), Some("dotted"));
    }
}

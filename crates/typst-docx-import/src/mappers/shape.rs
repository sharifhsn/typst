//! The `shape` mapper: a VML `v:rect`/`v:oval`/`v:roundrect`/`v:line`
//! ([`VmlShape`]) → a native Typst shape call
//! (`#rect`/`#circle`/`#ellipse`/`#line`), built as a ready-made string and
//! handed back as [`Inline::Verbatim`] — the same escape hatch
//! [`crate::mappers::field`] uses for `PAGE`/`TOC`: the call is already a
//! self-contained expression, so there's nothing left for a tier-2 pass to
//! promote it to.
//!
//! Word floats every VML shape at an absolute page position. This importer
//! already made the "content inlined at the anchor, geometry dropped" call
//! for text boxes ([`crate::tdoc::Inline::TextBox`]); this mapper makes the
//! same one and records it the same way (deduplicated, once per document).

use crate::emit::{pt, rgb_lit};
use crate::lower::parse_hex_color;
use crate::report::ImportReport;
use crate::tdoc::Inline;
use crate::wml::model::{VmlShape, VmlShapeKind};
use crate::wml::parse::{vml_coord_pt, vml_length_pt};

/// Lower one VML shape to its Typst-native counterpart. `None` only for a
/// [`VmlShapeKind::Line`] that's either explicitly unstroked (`stroked="f"`
/// — nothing would be visible anyway) or whose endpoints [`line_length`]
/// can't turn into a sensible length; both are recorded as a drop rather
/// than emitting an invisible or wrong line. Every other shape always
/// produces something, even with no width/height/color at all — Typst's own
/// defaults take over, same as a bare `#rect()`.
pub(crate) fn lower_vml_shape(
    shape: &VmlShape,
    report: &mut ImportReport,
) -> Option<Inline> {
    let width = vml_length_pt(&shape.style, "width");
    let height = vml_length_pt(&shape.style, "height");
    let fill = fill_color(shape);

    let src = match shape.kind {
        VmlShapeKind::Rect | VmlShapeKind::RoundRect => {
            rect_call(width, height, fill, stroke_arg(shape))
        }
        VmlShapeKind::Oval => oval_call(width, height, fill, stroke_arg(shape)),
        VmlShapeKind::Line => {
            if !shape.stroked {
                report.drop("VML line", "explicitly unstroked; nothing would be visible");
                return None;
            }
            let Some(length) = line_length(shape.from.as_deref(), shape.to.as_deref())
            else {
                report.drop(
                    "VML line",
                    "endpoints could not be turned into a length; shape dropped",
                );
                return None;
            };
            line_call(length, stroke_arg(shape))
        }
    };

    report.approximate(
        "VML shape",
        "floating position not preserved; drawn inline at the anchor point",
    );
    Some(Inline::Verbatim(src.into()))
}

fn fill_color(shape: &VmlShape) -> Option<[u8; 3]> {
    if !shape.filled {
        return None;
    }
    shape.fill_color.as_deref().and_then(vml_hex_color)
}

/// The three outcomes a `stroke:` argument can end up in, mirroring what
/// Typst's own shape functions accept: left unstated (`auto` — a 1pt black
/// stroke, but only if no fill is given), explicitly `none` (VML's
/// `stroked="f"` — without stating this, `auto` would draw an unwanted
/// border on an unfilled shape), or an explicit color.
enum StrokeArg {
    Auto,
    None,
    Color([u8; 3]),
}

fn stroke_arg(shape: &VmlShape) -> StrokeArg {
    if !shape.stroked {
        return StrokeArg::None;
    }
    match shape.stroke_color.as_deref().and_then(vml_hex_color) {
        Some(c) => StrokeArg::Color(c),
        None => StrokeArg::Auto,
    }
}

/// VML colors are `#rrggbb`, a bare color name (`"red"`), a system color
/// (`"windowText"`), or a theme-indexed form real documents also emit
/// (`"#b2b2b2 [3205]"`). Only the plain hex form is unambiguous without a
/// color-name table, so anything else is left unrecognized rather than
/// guessed at — this rejects the theme-indexed suffix too, since the
/// trailing `" [3205]"` makes the hex prefix's own meaning unclear (is
/// `#b2b2b2` a fallback, or does the index override it?).
fn vml_hex_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    parse_hex_color(Some(hex))
}

fn push_fill_stroke(args: &mut Vec<String>, fill: Option<[u8; 3]>, stroke: StrokeArg) {
    if let Some(c) = fill {
        args.push(format!("fill: {}", rgb_lit(c)));
    }
    match stroke {
        StrokeArg::Auto => {}
        StrokeArg::None => args.push("stroke: none".to_string()),
        StrokeArg::Color(c) => args.push(format!("stroke: {}", rgb_lit(c))),
    }
}

fn rect_call(
    width: Option<f64>,
    height: Option<f64>,
    fill: Option<[u8; 3]>,
    stroke: StrokeArg,
) -> String {
    let mut args = Vec::new();
    if let Some(w) = width {
        args.push(format!("width: {}", pt(w)));
    }
    if let Some(h) = height {
        args.push(format!("height: {}", pt(h)));
    }
    push_fill_stroke(&mut args, fill, stroke);
    format!("#rect({})", args.join(", "))
}

/// `v:oval` → `#circle(..)` when both dimensions are known and (near enough)
/// equal — the more idiomatic call for what's visually a circle — or
/// `#ellipse(..)` otherwise, including when a dimension is missing (an
/// ellipse's `width`/`height` are independently optional; a circle's
/// `radius` isn't derivable from just one of them).
fn oval_call(
    width: Option<f64>,
    height: Option<f64>,
    fill: Option<[u8; 3]>,
    stroke: StrokeArg,
) -> String {
    match (width, height) {
        (Some(w), Some(h)) if (w - h).abs() < 0.01 => {
            let mut args = vec![format!("radius: {}", pt(w / 2.0))];
            push_fill_stroke(&mut args, fill, stroke);
            format!("#circle({})", args.join(", "))
        }
        _ => {
            let mut args = Vec::new();
            if let Some(w) = width {
                args.push(format!("width: {}", pt(w)));
            }
            if let Some(h) = height {
                args.push(format!("height: {}", pt(h)));
            }
            push_fill_stroke(&mut args, fill, stroke);
            format!("#ellipse({})", args.join(", "))
        }
    }
}

fn line_call(length_pt: f64, stroke: StrokeArg) -> String {
    match stroke {
        StrokeArg::Color(c) => {
            format!("#line(length: {}, stroke: {})", pt(length_pt), rgb_lit(c))
        }
        // An explicitly unstroked line never reaches this function (see
        // `lower_vml_shape`, which drops it before calling `line_call` at
        // all), so `None` can't actually occur here — but falling through to
        // the same bare call as `Auto` is harmless either way.
        StrokeArg::Auto | StrokeArg::None => format!("#line(length: {})", pt(length_pt)),
    }
}

/// The Euclidean distance between a `v:line`'s `from`/`to` endpoints, in
/// points. Direction is deliberately discarded — there's no `angle:` in the
/// target shape this feature was scoped to (`#line(length: ..)`), and VML's
/// coordinate system doesn't map onto Typst's inline flow anyway — so a
/// diagonal line becomes a horizontal one of the same length rather than
/// being reproduced at the wrong angle. `None` if either endpoint doesn't
/// parse under [`vml_coord_pt`]'s rules.
fn line_length(from: Option<&str>, to: Option<&str>) -> Option<f64> {
    let (fx, fy) = vml_coord_pt(from?)?;
    let (tx, ty) = vml_coord_pt(to?)?;
    let dx = tx - fx;
    let dy = ty - fy;
    Some((dx * dx + dy * dy).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: VmlShapeKind, style: &str) -> VmlShape {
        VmlShape {
            kind,
            style: style.into(),
            fill_color: None,
            filled: true,
            stroke_color: None,
            stroked: true,
            from: None,
            to: None,
        }
    }

    #[test]
    fn rect_with_size_and_fill_lowers_to_a_rect_call() {
        let mut s = shape(VmlShapeKind::Rect, "width:120pt;height:80pt");
        s.fill_color = Some("#FF0000".into());
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).expect("rect should lower");
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#rect(width: 120pt, height: 80pt, fill: rgb(\"FF0000\"))");
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "VML shape");
    }

    /// A literal `width:0` (Word's auto-stretch-horizontal-rule convention,
    /// e.g. `drawing.docx`'s `o:hr` rects in the POI corpus) must not become
    /// a literal `width: 0pt` — that would draw an invisible shape.
    #[test]
    fn zero_width_is_treated_as_unspecified_not_a_real_zero() {
        let mut s = shape(VmlShapeKind::Rect, "width:0;height:1.5pt");
        s.fill_color = Some("#ACA899".into());
        s.stroked = false;
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).expect("rect should lower");
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#rect(height: 1.5pt, fill: rgb(\"ACA899\"), stroke: none)");
    }

    #[test]
    fn filled_false_omits_the_fill_argument() {
        let mut s = shape(VmlShapeKind::Rect, "width:10pt;height:10pt");
        s.fill_color = Some("#FF0000".into());
        s.filled = false;
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).unwrap();
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert!(!src.contains("fill:"), "fill should be omitted: {src}");
    }

    #[test]
    fn equal_dimensions_oval_becomes_a_circle() {
        let s = shape(VmlShapeKind::Oval, "width:50pt;height:50pt");
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).unwrap();
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#circle(radius: 25pt)");
    }

    #[test]
    fn unequal_dimensions_oval_becomes_an_ellipse() {
        let s = shape(VmlShapeKind::Oval, "width:100pt;height:40pt");
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).unwrap();
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#ellipse(width: 100pt, height: 40pt)");
    }

    #[test]
    fn line_with_pt_coordinates_derives_a_length() {
        let mut s = shape(VmlShapeKind::Line, "");
        s.from = Some("252pt,146.8pt".into());
        s.to = Some("525.6pt,146.8pt".into());
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).expect("line should lower");
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#line(length: 273.6pt)");
    }

    /// `from`/`to` where a coordinate has no unit at all (not even a bare
    /// `"0"`) can't be trusted to be points — could be a custom `v:group`
    /// coordinate system — so the line is dropped rather than guessed at.
    #[test]
    fn line_with_unitless_coordinates_is_dropped_with_a_note() {
        let mut s = shape(VmlShapeKind::Line, "");
        s.from = Some("0,0".into());
        s.to = Some("100,0".into());
        let mut report = ImportReport::default();
        assert!(lower_vml_shape(&s, &mut report).is_none());
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "VML line");
    }

    #[test]
    fn line_with_missing_coordinates_is_dropped_with_a_note() {
        let s = shape(VmlShapeKind::Line, "");
        let mut report = ImportReport::default();
        assert!(lower_vml_shape(&s, &mut report).is_none());
        assert_eq!(report.notes.len(), 1);
    }

    #[test]
    fn explicitly_unstroked_line_is_dropped_with_a_note() {
        let mut s = shape(VmlShapeKind::Line, "");
        s.from = Some("0pt,0pt".into());
        s.to = Some("100pt,0pt".into());
        s.stroked = false;
        let mut report = ImportReport::default();
        assert!(lower_vml_shape(&s, &mut report).is_none());
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "VML line");
    }

    #[test]
    fn line_with_color_includes_a_stroke_argument() {
        let mut s = shape(VmlShapeKind::Line, "");
        s.from = Some("0pt,0pt".into());
        s.to = Some("0pt,99pt".into());
        s.stroke_color = Some("#B2B2B2".into());
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).unwrap();
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert_eq!(src, "#line(length: 99pt, stroke: rgb(\"B2B2B2\"))");
    }

    #[test]
    fn unrecognized_color_name_is_ignored_rather_than_guessed() {
        let mut s = shape(VmlShapeKind::Rect, "width:10pt;height:10pt");
        s.fill_color = Some("red".into());
        let mut report = ImportReport::default();
        let inline = lower_vml_shape(&s, &mut report).unwrap();
        let Inline::Verbatim(src) = inline else { panic!("expected verbatim") };
        assert!(!src.contains("fill:"), "named color should not be guessed at: {src}");
    }
}

//! Run/paragraph property serialization in canonical OOXML schema order, plus
//! numeric/color conversions.

use typst_library::layout::Abs;
use typst_library::visualize::{Color, Gradient};
use typst_ooxml_core::dml::{self, GradientStop};
use typst_ooxml_core::{color as ooxml_color, units};

use crate::dom::{
    Indent, Jc, ParaBorders, ParaProps, RunProps, Spacing, TextFill, VertAlign,
};
use crate::xml::{self, XmlWriter};

// ---------------------------------------------------------------------------
// Numeric conversions.
// ---------------------------------------------------------------------------

/// Points → half-points (the unit of `w:sz`).
pub fn pt_to_half_pt(pt: f64) -> u32 {
    units::pt_to_half_point(pt)
}

/// Points → eighths of a point (the unit of a border's `w:sz`).
pub fn pt_to_eighth_pt(pt: f64) -> u32 {
    units::pt_to_eighth_point(pt)
}

/// An absolute length → twips (twentieths of a point), the `w:pgSz`/`w:ind` unit.
pub fn abs_to_twip(abs: Abs) -> i32 {
    units::abs_to_twip(abs)
}

/// Writes a Word language element with the script-specific slot Word uses for
/// proofing and font selection.
///
/// `w:val` is the Latin/default slot. Word does not infer `w:eastAsia` or
/// `w:bidi` reliably from it, so CJK and RTL languages must also populate their
/// dedicated slot. The RTL set mirrors `typst_library::text::Lang::dir`.
pub(crate) fn write_language(w: &mut XmlWriter, element: &'static str, lang: &str) {
    let primary = lang.split('-').next().unwrap_or(lang).to_ascii_lowercase();
    w.open(element).attr(xml::W_VAL, lang);
    if matches!(primary.as_str(), "ja" | "ko" | "zh") {
        w.attr("w:eastAsia", lang);
    }
    if matches!(
        primary.as_str(),
        "ar" | "dv" | "fa" | "he" | "ks" | "pa" | "ps" | "sd" | "ug" | "ur" | "yi"
    ) {
        w.attr("w:bidi", lang);
    }
    w.empty();
}

/// An absolute length → EMU (914400 per inch = 12700 per point), the DrawingML unit.
pub fn abs_to_emu(abs: Abs) -> i64 {
    units::abs_to_emu(abs)
}

/// A color → opaque `RRGGBB` for WordprocessingML.
///
/// WordprocessingML color properties have no alpha channel, so translucent
/// colors are composited onto Word's default white background instead of
/// silently becoming fully saturated.
pub fn color_to_hex(color: &Color) -> [u8; 3] {
    ooxml_color::composite_rgb_on_white(color)
}

/// An already-lowered DrawingML colour (straight sRGB + alpha) → opaque
/// `RRGGBB`, for the WordprocessingML and VML properties that reuse a shape's
/// colour but have no alpha channel of their own (`w:bdr`, VML `fillcolor`).
pub fn shape_color_on_white(rgba: [u8; 4]) -> [u8; 3] {
    ooxml_color::composite_rgba_on_white(rgba)
}

/// A gradient approximated by a single solid colour — its first stop — for a
/// flat `w:shd` shade (see `handle_block_box` / `inline_frame`). A gradient's
/// stops are stored in its own interpolation space (Oklab by default);
/// `color_to_hex` converts to sRGB before flattening, matching DrawingML and
/// SVG color conversion. `None` for a stopless gradient.
pub fn gradient_shade_hex(
    gradient: &typst_library::visualize::Gradient,
) -> Option<[u8; 3]> {
    gradient.stops_ref().first().map(|(c, _)| color_to_hex(c))
}

/// Lowers a gradient text fill to Office 2010 `w14:textFill` extension data,
/// reusing the exact DrawingML gradient maths the shape exporter's
/// `a:gradFill` uses (`typst_ooxml_core::dml::gradient_fill`) for stop
/// sampling, the linear angle, and the radial focus rectangle — only the
/// WordprocessingML element names and attribute qualification differ (see
/// `write_text_fill`). `None` for a gradient DrawingML itself cannot express
/// (a conic sweep, an off-center radial outer circle, or no stops); the run
/// then keeps only the flat `gradient_shade_hex` fallback in `RunProps::color`.
pub fn text_fill_from_gradient(gradient: &Gradient) -> Option<TextFill> {
    match dml::gradient_fill(gradient, dml::AlphaMode::Preserve)? {
        dml::FillSpec::LinearGradient { angle_60k, stops } => {
            Some(TextFill::Linear { angle_60k, stops })
        }
        dml::FillSpec::RadialGradient {
            stops,
            focal_center_100k,
            focal_radius_100k,
            ..
        } => Some(TextFill::Radial { stops, focal_center_100k, focal_radius_100k }),
        dml::FillSpec::Solid(_) | dml::FillSpec::Tile { .. } => None,
    }
}

/// Formats an `RRGGBB` byte triple as uppercase hex.
pub fn hex(rgb: [u8; 3]) -> String {
    ooxml_color::hex_rgb(rgb)
}

// ---------------------------------------------------------------------------
// Run properties → `<w:rPr>` (canonical child order).
// ---------------------------------------------------------------------------

impl RunProps {
    /// Whether this run carries any formatting at all.
    pub fn is_empty(&self) -> bool {
        self.style.is_none()
            && self.semantic_rstyle().is_none()
            && self.font.is_none()
            && !self.writes_direct_bold()
            && !self.writes_direct_italic()
            && !self.caps
            && !self.smallcaps
            && !self.strike
            && !self.no_proof
            && self.color.is_none()
            && self.text_fill.is_none()
            && self.tracking.is_none()
            && self.position_half_pt.is_none()
            && self.size_half_pt.is_none()
            && self.highlight.is_none()
            && self.shd_fill.is_none()
            && self.bdr.is_none()
            && self.underline.is_none()
            && !self.vanish
            && self.vert_align.is_none()
            && !self.rtl
            && !self.cs
            && self.lang.is_none()
    }

    /// The Word semantic character style represented by Typst `#strong` or
    /// `#emph`, when no other character style already occupies `w:rStyle`.
    ///
    /// Word permits one `w:rStyle` plus direct run properties. For nested
    /// strong/emphasis we keep the stronger semantic style as `w:rStyle` and
    /// write the italic half as direct formatting.
    fn semantic_rstyle(&self) -> Option<&'static str> {
        if self.style.is_some() {
            return None;
        }
        if self.strong && self.bold {
            Some("Strong")
        } else if self.emphasis && self.italic {
            Some("Emphasis")
        } else {
            None
        }
    }

    fn writes_direct_bold(&self) -> bool {
        self.bold && self.semantic_rstyle() != Some("Strong")
    }

    fn writes_direct_italic(&self) -> bool {
        self.italic && self.semantic_rstyle() != Some("Emphasis")
    }

    /// Writes `<w:rPr>...</w:rPr>` in canonical order. Emits nothing if empty.
    pub fn write_rpr(&self, w: &mut XmlWriter) {
        if self.is_empty() {
            return;
        }
        w.open(xml::W_RPR).start_children();

        // 1. rStyle
        if let Some(style) = &self.style {
            w.open(xml::W_RSTYLE).attr(xml::W_VAL, style).empty();
        } else if let Some(style) = self.semantic_rstyle() {
            w.open(xml::W_RSTYLE).attr(xml::W_VAL, style).empty();
        }
        // 2. rFonts
        if let Some(font) = &self.font {
            w.open(xml::W_RFONTS)
                .attr("w:ascii", font)
                .attr("w:hAnsi", font)
                .attr("w:cs", font)
                .attr("w:eastAsia", font)
                .empty();
        }
        // 3. b / bCs
        if self.writes_direct_bold() {
            w.leaf(xml::W_B);
            w.leaf(xml::W_BCS);
        }
        // 4. i / iCs
        if self.writes_direct_italic() {
            w.leaf(xml::W_I);
            w.leaf(xml::W_ICS);
        }
        // 5. caps / smallCaps
        if self.caps {
            w.leaf("w:caps");
        }
        if self.smallcaps {
            w.leaf(xml::W_SMALLCAPS);
        }
        // 6. strike
        if self.strike {
            w.leaf(xml::W_STRIKE);
        }
        // 7. noProof (after strike/dstrike/outline/shadow/emboss/imprint;
        // before vanish/color/spacing).
        if self.no_proof {
            w.leaf("w:noProof");
        }
        // 8. vanish — hidden text (`#hide`).
        if self.vanish {
            w.open("w:vanish").empty();
        }
        // 9. color
        if let Some(c) = self.color {
            w.open(xml::W_COLOR).attr(xml::W_VAL, &hex(c)).empty();
        }
        // 10. spacing (character tracking)
        if let Some(tracking) = self.tracking {
            w.open(xml::W_SPACING).attr(xml::W_VAL, &tracking.to_string()).empty();
        }
        // 11. position (baseline shift, signed half-points)
        if let Some(pos) = self.position_half_pt {
            w.open("w:position").attr(xml::W_VAL, &pos.to_string()).empty();
        }
        // 12. sz / szCs
        if let Some(sz) = self.size_half_pt {
            w.open(xml::W_SZ).attr(xml::W_VAL, &sz.to_string()).empty();
            w.open(xml::W_SZCS).attr(xml::W_VAL, &sz.to_string()).empty();
        }
        // 13. highlight (after sz/szCs; before u/effect/bdr/shd).
        if let Some(value) = self.highlight {
            w.open("w:highlight").attr(xml::W_VAL, value).empty();
        }
        // 14. u (canonical pos 27, before shd at 30)
        if let Some(u) = &self.underline {
            w.open(xml::W_U).attr(xml::W_VAL, u.val);
            if let Some(c) = u.color {
                w.attr("w:color", &hex(c));
            }
            w.empty();
        }
        // 15. bdr — run border box (inline framed container).
        if let Some(b) = &self.bdr {
            w.open("w:bdr")
                .attr(xml::W_VAL, b.style)
                .attr("w:sz", &b.sz.to_string())
                .attr("w:space", &b.space.to_string())
                .attr("w:color", &hex(b.color))
                .empty();
        }
        // 16. shd
        if let Some(fill) = self.shd_fill {
            w.open(xml::W_SHD)
                .attr(xml::W_VAL, "clear")
                .attr("w:color", "auto")
                .attr("w:fill", &hex(fill))
                .empty();
        }
        // 17. vertAlign
        if let Some(va) = self.vert_align {
            let val = match va {
                VertAlign::Super => "superscript",
                VertAlign::Sub => "subscript",
            };
            w.open(xml::W_VERTALIGN).attr(xml::W_VAL, val).empty();
        }
        // 18. rtl / cs (run reading order + complex-script formatting)
        if self.rtl {
            w.leaf("w:rtl");
        }
        if self.cs {
            w.leaf("w:cs");
        }
        // 19. lang
        if let Some(lang) = &self.lang {
            write_language(w, xml::W_LANG, lang);
        }
        // 20. w14:textFill — the Office 2010 gradient text fill extension.
        // Unlike everything above, this is not part of the base ECMA-376
        // `CT_RPr` sequence at all: Word's own extended schema
        // (`EG_RPrTextEffects` in the `wordml/2010` namespace) appends it —
        // and its `w14:textOutline`/`w14:glow`/`w14:shadow`/… siblings we
        // don't emit — after every element declared above, and real Word
        // output always places it last. `mc:Ignorable="w14"` (already on
        // every part root) tells an older/non-Word consumer to skip it, so
        // its position relative to the canonical rPr sequence is otherwise
        // unconstrained; last matches Word's own emission order.
        if let Some(fill) = &self.text_fill {
            write_text_fill(w, fill);
        }

        w.close();
    }
}

/// Writes `<w14:textFill><w14:gradFill>…</w14:gradFill></w14:textFill>`.
///
/// Mirrors the structure `typst_ooxml_core::dml::write_fill_with_tile_resolver`
/// emits for a shape's `a:gradFill`, but in the `w14` namespace: unlike
/// DrawingML, the WordprocessingML 2010 extension schema qualifies every
/// attribute (`w14:pos`, `w14:ang`, `w14:val`, …), and `w14:gradFill` itself
/// has no `rotWithShape` attribute (`CT_GradientFillProperties` in
/// `wml-2010.xsd` takes none).
fn write_text_fill(w: &mut XmlWriter, fill: &TextFill) {
    w.open("w14:textFill").start_children();
    match fill {
        TextFill::Linear { angle_60k, stops } => {
            w.open("w14:gradFill").start_children();
            write_w14_gradient_stops(w, stops);
            w.open("w14:lin")
                .attr("w14:ang", &angle_60k.to_string())
                .attr("w14:scaled", "0")
                .empty();
            w.close(); // w14:gradFill
        }
        TextFill::Radial { stops, focal_center_100k, focal_radius_100k } => {
            let [l, t, r, b] =
                dml::radial_focus_rect_100k(*focal_center_100k, *focal_radius_100k);
            w.open("w14:gradFill").start_children();
            write_w14_gradient_stops(w, stops);
            w.open("w14:path").attr("w14:path", "circle").start_children();
            w.open("w14:fillToRect")
                .attr("w14:l", &l.to_string())
                .attr("w14:t", &t.to_string())
                .attr("w14:r", &r.to_string())
                .attr("w14:b", &b.to_string())
                .empty();
            w.close(); // w14:path
            w.close(); // w14:gradFill
        }
    }
    w.close(); // w14:textFill
}

fn write_w14_gradient_stops(w: &mut XmlWriter, stops: &[GradientStop]) {
    w.open("w14:gsLst").start_children();
    for stop in stops {
        w.open("w14:gs")
            .attr("w14:pos", &stop.pos_100k.to_string())
            .start_children();
        write_w14_srgb(w, stop.color);
        w.close();
    }
    w.close();
}

/// Emits a `w14:srgbClr` child, including `w14:alpha` when not opaque —
/// the same shape as `typst_ooxml_core::dml::write_srgb`'s DrawingML
/// `a:srgbClr`/`a:alpha`, qualified for the `w14` namespace.
fn write_w14_srgb(w: &mut XmlWriter, rgba: [u8; 4]) {
    let [r, g, b, a] = rgba;
    if a == 255 {
        w.open("w14:srgbClr").attr("w14:val", &hex([r, g, b])).empty();
    } else {
        w.open("w14:srgbClr")
            .attr("w14:val", &hex([r, g, b]))
            .start_children();
        w.open("w14:alpha")
            .attr("w14:val", &ooxml_color::alpha_to_100k(a).to_string())
            .empty();
        w.close();
    }
}

// ---------------------------------------------------------------------------
// Paragraph properties → `<w:pPr>` (canonical child order).
// ---------------------------------------------------------------------------

impl ParaProps {
    /// Whether this carries any properties at all.
    pub fn is_empty(&self) -> bool {
        self.style.is_none()
            && !self.keep_next
            && !self.page_break_before
            && !self.keep_lines
            && self.num.is_none()
            && !self.suppress_line_numbers
            && !self.bidi
            && self.spacing.is_none()
            && self.ind.is_none()
            && !self.contextual_spacing
            && self.jc.is_none()
            && self.outline_lvl.is_none()
            && self.tabs.is_empty()
            && self.shd_fill.is_none()
            && self.pbdr.is_none()
    }

    /// Writes `<w:pPr>...</w:pPr>` in canonical order. Emits nothing if empty.
    pub fn write_ppr(&self, w: &mut XmlWriter) {
        if self.is_empty() {
            return;
        }
        w.open(xml::W_PPR).start_children();

        // 1. pStyle
        if let Some(style) = &self.style {
            w.open(xml::W_PSTYLE).attr(xml::W_VAL, style).empty();
        }
        // 2. keepNext
        if self.keep_next {
            w.leaf("w:keepNext");
        }
        // 3. pageBreakBefore
        if self.page_break_before {
            w.leaf("w:pageBreakBefore");
        }
        // 4. keepLines
        if self.keep_lines {
            w.leaf("w:keepLines");
        }
        // 5. numPr
        if let Some((num_id, ilvl)) = self.num {
            w.open("w:numPr").start_children();
            w.open("w:ilvl").attr(xml::W_VAL, &ilvl.to_string()).empty();
            w.open("w:numId").attr(xml::W_VAL, &num_id.to_string()).empty();
            w.close();
        }
        // 5. suppressLineNumbers (paragraph opt-out inside a numbered section)
        if self.suppress_line_numbers {
            w.leaf("w:suppressLineNumbers");
        }
        // 6. bidi (paragraph base reading order)
        if self.bidi {
            w.leaf("w:bidi");
        }
        // 7. tabs
        if !self.tabs.is_empty() {
            w.open("w:tabs").start_children();
            for tab in &self.tabs {
                let val = match tab.val {
                    crate::dom::TabAlign::Start => "start",
                    crate::dom::TabAlign::End => "end",
                    crate::dom::TabAlign::Center => "center",
                };
                w.open("w:tab").attr(xml::W_VAL, val);
                if let Some(leader) = tab.leader {
                    let l = match leader {
                        crate::dom::TabLeader::Dot => "dot",
                        crate::dom::TabLeader::Hyphen => "hyphen",
                        crate::dom::TabLeader::Underscore => "underscore",
                    };
                    w.attr("w:leader", l);
                }
                w.attr("w:pos", &tab.pos.to_string()).empty();
            }
            w.close();
        }
        // 8. pBdr (paragraph borders, before shd)
        if let Some(b) = &self.pbdr {
            write_pbdr(w, b);
        }
        // 9. shd (paragraph shading)
        if let Some(fill) = self.shd_fill {
            w.open(xml::W_SHD)
                .attr(xml::W_VAL, "clear")
                .attr("w:color", "auto")
                .attr("w:fill", &hex(fill))
                .empty();
        }
        // 10. spacing
        if let Some(sp) = &self.spacing {
            write_spacing(w, sp);
        }
        // 11. ind
        if let Some(ind) = &self.ind {
            write_indent(w, ind);
        }
        // 12. contextualSpacing
        if self.contextual_spacing {
            w.leaf("w:contextualSpacing");
        }
        // 13. jc
        if let Some(jc) = self.jc {
            let val = match jc {
                Jc::Start => "start",
                Jc::End => "end",
                Jc::Center => "center",
                Jc::Both => "both",
            };
            w.open("w:jc").attr(xml::W_VAL, val).empty();
        }
        // 14. outlineLvl
        if let Some(lvl) = self.outline_lvl {
            w.open("w:outlineLvl").attr(xml::W_VAL, &lvl.to_string()).empty();
        }

        w.close();
    }
}

/// Writes `<w:pBdr>` with the canonical side order (top, left, bottom, right).
fn write_pbdr(w: &mut XmlWriter, b: &ParaBorders) {
    w.open("w:pBdr").start_children();
    for (name, side) in [
        ("w:top", &b.top),
        ("w:left", &b.left),
        ("w:bottom", &b.bottom),
        ("w:right", &b.right),
    ] {
        if let Some(border) = side {
            w.open(name)
                .attr(xml::W_VAL, border.style)
                .attr("w:sz", &border.sz.to_string())
                .attr("w:space", &border.space.to_string())
                .attr("w:color", &hex(border.color))
                .empty();
        }
    }
    w.close();
}

fn write_spacing(w: &mut XmlWriter, sp: &Spacing) {
    w.open(xml::W_SPACING);
    // `before`/`after` can carry a negative running total internally — an
    // authored negative `#v(..)` net-cancelling a natural paragraph-boundary
    // gap added later by `collapse_par_spacing_run` (see `apply_pending_v`'s
    // doc comment). Word's `w:spacing` has no negative primitive, so floor at
    // zero only here, once every upstream pass that could combine spacing has
    // already run.
    if let Some(before) = sp.before {
        w.attr("w:before", &before.max(0).to_string());
    }
    if let Some(after) = sp.after {
        w.attr("w:after", &after.max(0).to_string());
    }
    if let Some(line) = sp.line {
        w.attr("w:line", &line.to_string());
        let rule = if sp.line_rule_auto {
            "auto"
        } else if sp.line_rule_at_least {
            "atLeast"
        } else {
            "exact"
        };
        w.attr("w:lineRule", rule);
    }
    w.empty();
}

fn write_indent(w: &mut XmlWriter, ind: &Indent) {
    w.open("w:ind");
    if let Some(left) = ind.left {
        w.attr("w:left", &left.to_string());
    }
    if let Some(right) = ind.right {
        w.attr("w:right", &right.to_string());
    }
    if let Some(fl) = ind.first_line {
        w.attr("w:firstLine", &fl.to_string());
    }
    if let Some(h) = ind.hanging {
        w.attr("w:hanging", &h.to_string());
    }
    w.empty();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use typst_library::foundations::Smart;
    use typst_library::layout::{Angle, Axes, Ratio};
    use typst_library::visualize::{
        ConicGradient, LinearGradient, ProcessColor, ProcessColorSpace, Rgb,
    };

    use super::*;

    fn red() -> Color {
        Color::Process(ProcessColor::Rgb(Rgb::new(1.0, 0.0, 0.0, 1.0)))
    }

    fn blue() -> Color {
        Color::Process(ProcessColor::Rgb(Rgb::new(0.0, 0.0, 1.0, 1.0)))
    }

    /// `gradient.linear(red, blue)`, sRGB-interpolated so no extra stops are
    /// sampled between the two authored ones.
    fn linear_red_blue() -> Gradient {
        Gradient::Linear(Arc::new(LinearGradient {
            stops: vec![(red(), Ratio::zero()), (blue(), Ratio::one())],
            angle: Angle::deg(90.0),
            space: typst_library::visualize::ColorSpace::Process(ProcessColorSpace::Srgb),
            relative: Smart::Auto,
            anti_alias: false,
        }))
    }

    #[test]
    fn linear_gradient_lowers_to_a_w14_text_fill_with_the_real_stop_list() {
        let gradient = linear_red_blue();
        let Some(TextFill::Linear { angle_60k, stops }) =
            text_fill_from_gradient(&gradient)
        else {
            panic!("expected a linear w14:textFill");
        };
        // 90° clockwise from east, in 60,000ths of a degree.
        assert_eq!(angle_60k, 90 * 60_000);
        assert_eq!(stops.first().unwrap().color, [255, 0, 0, 255]);
        assert_eq!(stops.last().unwrap().color, [0, 0, 255, 255]);
    }

    #[test]
    fn conic_gradient_has_no_w14_analogue() {
        // No OOXML gradient path sweeps by angle, so DrawingML — and
        // therefore its `w14:textFill` mirror — has nothing to reuse.
        let gradient = Gradient::Conic(Arc::new(ConicGradient {
            stops: vec![(red(), Ratio::zero()), (blue(), Ratio::one())],
            angle: Angle::zero(),
            center: Axes::new(Ratio::new(0.5), Ratio::new(0.5)),
            space: typst_library::visualize::ColorSpace::Process(ProcessColorSpace::Srgb),
            relative: Smart::Auto,
            anti_alias: false,
        }));
        assert!(text_fill_from_gradient(&gradient).is_none());
    }

    #[test]
    fn gradient_text_run_keeps_a_flat_color_fallback_before_the_w14_extension() {
        let gradient = linear_red_blue();
        let props = RunProps {
            color: gradient_shade_hex(&gradient),
            text_fill: text_fill_from_gradient(&gradient),
            ..RunProps::default()
        };

        let mut w = XmlWriter::new(false);
        props.write_rpr(&mut w);
        let xml = w.finish();

        // The first stop's flat color always survives for a consumer that
        // ignores the MCE-ignorable `w14` extension.
        assert!(xml.contains(r#"<w:color w:val="FF0000"/>"#), "{xml}");
        // The gradient's real stop list and angle are also present…
        assert!(xml.contains("<w14:textFill>"), "{xml}");
        assert!(xml.contains("<w14:gradFill>"), "{xml}");
        assert!(xml.contains(r#"<w14:gs w14:pos="0">"#), "{xml}");
        assert!(xml.contains(r#"<w14:srgbClr w14:val="FF0000"/>"#), "{xml}");
        assert!(xml.contains(r#"<w14:srgbClr w14:val="0000FF"/>"#), "{xml}");
        assert!(xml.contains(r#"<w14:lin w14:ang="5400000" w14:scaled="0"/>"#), "{xml}");
        // … and `w14:textFill` comes after `w:color`, matching real Word
        // output (an MCE extension appended as `w:rPr`'s last child).
        let color_at = xml.find("<w:color").unwrap();
        let text_fill_at = xml.find("<w14:textFill>").unwrap();
        assert!(color_at < text_fill_at, "{xml}");
    }
}

//! Run/paragraph property serialization in canonical OOXML schema order, plus
//! numeric/color conversions.

use typst_library::layout::Abs;
use typst_library::visualize::Color;
use typst_ooxml_core::{color as ooxml_color, units};

use crate::dom::{Indent, Jc, ParaBorders, ParaProps, RunProps, Spacing, VertAlign};
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

/// An absolute length → EMU (914400 per inch = 12700 per point), the DrawingML unit.
pub fn abs_to_emu(abs: Abs) -> i64 {
    units::abs_to_emu(abs)
}

/// A color → `RRGGBB` hex.
pub fn color_to_hex(color: &Color) -> [u8; 3] {
    ooxml_color::raw_rgb(color)
}

/// A gradient approximated by a single solid colour — its first stop — for a
/// flat `w:shd` shade (see `handle_block_box` / `inline_frame`). A gradient's
/// stops are stored in its own interpolation space (Oklab by default);
/// `color_to_hex` reads a colour's components verbatim, so the stop MUST be
/// converted to sRGB first (exactly as `linear_gradient_fill` and the SVG
/// exporter do) — otherwise the Oklab L/a/b triple is reinterpreted as RGB,
/// yielding a plausible-looking but wrong colour (a pale lilac read as bright
/// red). `None` for a stopless gradient.
pub fn gradient_shade_hex(
    gradient: &typst_library::visualize::Gradient,
) -> Option<[u8; 3]> {
    use typst_library::visualize::{ColorSpace, ProcessColorSpace};
    let srgb = ColorSpace::Process(ProcessColorSpace::Srgb);
    gradient
        .stops_ref()
        .first()
        .map(|(c, _)| color_to_hex(&c.to_space(&srgb).unwrap_or_else(|_| c.clone())))
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
            w.open(xml::W_LANG).attr(xml::W_VAL, lang).empty();
        }

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
        // 3. keepLines
        if self.keep_lines {
            w.leaf("w:keepLines");
        }
        // 4. numPr
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
    if let Some(before) = sp.before {
        w.attr("w:before", &before.to_string());
    }
    if let Some(after) = sp.after {
        w.attr("w:after", &after.to_string());
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

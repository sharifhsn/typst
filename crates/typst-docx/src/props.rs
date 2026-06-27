//! Run/paragraph property serialization in canonical OOXML schema order, plus
//! numeric/color conversions.

use typst_library::layout::Abs;
use typst_library::visualize::Color;

use crate::dom::{Indent, Jc, ParaBorders, ParaProps, RunProps, Spacing, VertAlign};
use crate::xml::{self, XmlWriter};

// ---------------------------------------------------------------------------
// Numeric conversions.
// ---------------------------------------------------------------------------

/// Points → half-points (the unit of `w:sz`).
pub fn pt_to_half_pt(pt: f64) -> u32 {
    (pt * 2.0).round().max(0.0) as u32
}

/// Points → eighths of a point (the unit of a border's `w:sz`).
pub fn pt_to_eighth_pt(pt: f64) -> u32 {
    (pt * 8.0).round().max(0.0) as u32
}

/// An absolute length → twips (twentieths of a point), the `w:pgSz`/`w:ind` unit.
pub fn abs_to_twip(abs: Abs) -> i32 {
    (abs.to_pt() * 20.0).round() as i32
}

/// An absolute length → EMU (914400 per inch = 12700 per point), the DrawingML unit.
pub fn abs_to_emu(abs: Abs) -> i64 {
    (abs.to_pt() * 12700.0).round() as i64
}

/// A color → `RRGGBB` hex.
pub fn color_to_hex(color: &Color) -> [u8; 3] {
    let [r, g, b, _] = color.to_vec4_u8();
    [r, g, b]
}

/// Formats an `RRGGBB` byte triple as uppercase hex.
pub fn hex(rgb: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

// ---------------------------------------------------------------------------
// Run properties → `<w:rPr>` (canonical child order).
// ---------------------------------------------------------------------------

impl RunProps {
    /// Whether this run carries any formatting at all.
    pub fn is_empty(&self) -> bool {
        *self == RunProps::default()
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
        if self.bold {
            w.leaf(xml::W_B);
            w.leaf(xml::W_BCS);
        }
        // 4. i / iCs
        if self.italic {
            w.leaf(xml::W_I);
            w.leaf(xml::W_ICS);
        }
        // 5. smallCaps
        if self.smallcaps {
            w.leaf(xml::W_SMALLCAPS);
        }
        // 6. strike
        if self.strike {
            w.leaf(xml::W_STRIKE);
        }
        // 7. color
        if let Some(c) = self.color {
            w.open(xml::W_COLOR).attr(xml::W_VAL, &hex(c)).empty();
        }
        // 8. spacing (character tracking)
        if let Some(tracking) = self.tracking {
            w.open(xml::W_SPACING).attr(xml::W_VAL, &tracking.to_string()).empty();
        }
        // 9. position (baseline shift, signed half-points)
        if let Some(pos) = self.position_half_pt {
            w.open("w:position").attr(xml::W_VAL, &pos.to_string()).empty();
        }
        // 10. sz / szCs
        if let Some(sz) = self.size_half_pt {
            w.open(xml::W_SZ).attr(xml::W_VAL, &sz.to_string()).empty();
            w.open(xml::W_SZCS).attr(xml::W_VAL, &sz.to_string()).empty();
        }
        // 11. u (canonical pos 27, before shd at 30)
        if self.underline {
            w.open(xml::W_U).attr(xml::W_VAL, "single").empty();
        }
        // 12. shd
        if let Some(fill) = self.shd_fill {
            w.open(xml::W_SHD)
                .attr(xml::W_VAL, "clear")
                .attr("w:color", "auto")
                .attr("w:fill", &hex(fill))
                .empty();
        }
        // 13. vertAlign
        if let Some(va) = self.vert_align {
            let val = match va {
                VertAlign::Super => "superscript",
                VertAlign::Sub => "subscript",
            };
            w.open(xml::W_VERTALIGN).attr(xml::W_VAL, val).empty();
        }
        // 14. rtl / cs (run reading order + complex-script formatting)
        if self.rtl {
            w.leaf("w:rtl");
        }
        if self.cs {
            w.leaf("w:cs");
        }
        // 15. lang
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
        // 5. bidi (paragraph base reading order)
        if self.bidi {
            w.leaf("w:bidi");
        }
        // 6. tabs
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
        // 7. pBdr (paragraph borders, before shd)
        if let Some(b) = &self.pbdr {
            write_pbdr(w, b);
        }
        // 8. shd (paragraph shading)
        if let Some(fill) = self.shd_fill {
            w.open(xml::W_SHD)
                .attr(xml::W_VAL, "clear")
                .attr("w:color", "auto")
                .attr("w:fill", &hex(fill))
                .empty();
        }
        // 9. spacing
        if let Some(sp) = &self.spacing {
            write_spacing(w, sp);
        }
        // 10. ind
        if let Some(ind) = &self.ind {
            write_indent(w, ind);
        }
        // 11. contextualSpacing
        if self.contextual_spacing {
            w.leaf("w:contextualSpacing");
        }
        // 12. jc
        if let Some(jc) = self.jc {
            let val = match jc {
                Jc::Start => "start",
                Jc::End => "end",
                Jc::Center => "center",
                Jc::Both => "both",
            };
            w.open("w:jc").attr(xml::W_VAL, val).empty();
        }
        // 13. outlineLvl
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

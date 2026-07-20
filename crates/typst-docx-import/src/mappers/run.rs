//! The `run` mapper: Word runs (`w:r`) and hyperlinks (`w:hyperlink`) → the
//! Typst IR's [`Inlines`].

use typst_ooxml_core::units::half_point_to_pt;

use crate::lower::parse_hex_color;
use crate::mappers::math;
use crate::report::ImportReport;
use crate::resolve::styles::effective_run;
use crate::tdoc::{Inline, Inlines, Script, TextStyle};
use crate::wml::model::{BreakType, Paragraph, Run, RunContent, RunItem, RunProps, WmlPackage};

/// Lower a whole paragraph's run sequence (including hyperlinks) to inlines.
pub fn lower_paragraph_inlines(
    p: &Paragraph,
    package: &WmlPackage,
    report: &mut ImportReport,
) -> Inlines {
    let para_style_id = p.props.style_id.as_deref();
    let mut out = Vec::new();
    for run_item in &p.runs {
        match run_item {
            RunItem::Run(r) => out.extend(lower_run(r, package, para_style_id, report)),
            RunItem::Hyperlink { rel_id, anchor, runs } => {
                let mut inner = Vec::new();
                for r in runs {
                    inner.extend(lower_run(r, package, para_style_id, report));
                }
                match rel_id {
                    Some(id) => match package.rels.get(id) {
                        Some(rel) => {
                            out.push(Inline::Link { dest: rel.target.clone(), body: inner })
                        }
                        None => {
                            report.approximate(
                                "hyperlink",
                                "relationship target not found; text kept unlinked",
                            );
                            out.extend(inner);
                        }
                    },
                    None => {
                        if anchor.is_some() {
                            report.approximate(
                                "internal hyperlink",
                                "anchor not resolved; link dropped, text kept",
                            );
                        }
                        out.extend(inner);
                    }
                }
            }
        }
    }
    out
}

/// Lower a single run to zero or more inlines (empty if it's hidden text
/// (`w:vanish`) or carries no visible content).
fn lower_run(
    r: &Run,
    package: &WmlPackage,
    para_style_id: Option<&str>,
    report: &mut ImportReport,
) -> Inlines {
    let eff = effective_run(&package.styles, para_style_id, &r.props);
    if eff.vanish == Some(true) {
        return Vec::new();
    }

    let mut content = Vec::new();
    for c in &r.content {
        match c {
            RunContent::Text(s) => content.push(Inline::Text(s.clone())),
            RunContent::Tab => content.push(Inline::Text("\t".into())),
            RunContent::Break(BreakType::Line) => content.push(Inline::Linebreak),
            // Page/column breaks are handled at the paragraph level.
            RunContent::Break(BreakType::Page | BreakType::Column) => {}
            // Images are handled at the paragraph level (block-level figures).
            RunContent::Drawing(_) => {}
            RunContent::Math(frag) => content.push(math::omml_to_inline(frag, report)),
        }
    }

    if content.is_empty() {
        return Vec::new();
    }

    let style = text_style_from_run_props(&eff);
    if style.is_empty() {
        content
    } else {
        vec![Inline::Styled { style, body: content }]
    }
}

fn text_style_from_run_props(eff: &RunProps) -> TextStyle {
    TextStyle {
        font: eff.font.clone(),
        size_pt: eff.size_half_pt.map(|h| half_point_to_pt(h as f64)),
        color: parse_hex_color(eff.color.as_deref()),
        bold: eff.bold == Some(true),
        italic: eff.italic == Some(true),
        underline: eff.underline.as_deref().is_some_and(|u| u != "none"),
        strike: eff.strike == Some(true),
        smallcaps: eff.smallcaps == Some(true),
        script: match eff.vert_align.as_deref() {
            Some("superscript") => Some(Script::Super),
            Some("subscript") => Some(Script::Sub),
            _ => None,
        },
    }
}

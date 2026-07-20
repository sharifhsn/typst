//! The `run` mapper: Word runs (`w:r`), hyperlinks (`w:hyperlink`), and
//! fields (`w:fldSimple`/`w:fldChar` — see [`crate::mappers::field`]) → the
//! Typst IR's [`Inlines`].

use typst_ooxml_core::units::half_point_to_pt;

use crate::lower::parse_hex_color;
use crate::mappers::{field, math};
use crate::opts::ImportOptions;
use crate::report::ImportReport;
use crate::resolve::styles::effective_run;
use crate::tdoc::{Inline, Inlines, Script, TextStyle};
use crate::wml::model::{BreakType, Paragraph, Run, RunContent, RunItem, RunProps, WmlPackage};

/// Lower a whole paragraph's run sequence (runs, hyperlinks, fields) to
/// inlines.
pub fn lower_paragraph_inlines(
    p: &Paragraph,
    package: &WmlPackage,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> Inlines {
    lower_run_items(&p.runs, package, p.props.style_id.as_deref(), options, report)
}

/// Lower a sequence of run-level items ([`RunItem::Run`]/`Hyperlink`/`Field`)
/// to inlines. Shared by [`lower_paragraph_inlines`] (a paragraph's own runs)
/// and by [`crate::mappers::field::lower_field`] (a field's cached result,
/// and a hyperlink's content wraps back around to this same function too) —
/// all three positions can hold the same mix of runs, hyperlinks, and
/// (nested) fields.
pub(crate) fn lower_run_items(
    items: &[RunItem],
    package: &WmlPackage,
    para_style_id: Option<&str>,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> Inlines {
    let mut out = Vec::new();
    for run_item in items {
        match run_item {
            RunItem::Run(r) => out.extend(lower_run(r, package, para_style_id, report)),
            RunItem::Hyperlink { rel_id, anchor, runs } => {
                let inner = lower_run_items(runs, package, para_style_id, options, report);
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
            RunItem::Field(f) => out.extend(field::lower_field(f, package, options, report)),
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

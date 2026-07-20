//! The `para` mapper: classifies a single Word paragraph into what it
//! becomes in the Typst IR — a heading, list item, rule, page/column break,
//! figure, or an ordinary paragraph — building its inline content via
//! [`crate::mappers::run`]. List-item accumulation *across* paragraphs is a
//! cross-paragraph concern that stays in [`crate::lower`]; this mapper only
//! classifies one paragraph at a time.

use typst_ooxml_core::units::twip_to_abs;

use crate::mappers::drawing;
use crate::mappers::run::lower_paragraph_inlines;
use crate::report::ImportReport;
use crate::resolve::styles::{effective_para, heading_level};
use crate::tdoc::{Align, BreakKind, Figure, Inline, Inlines, ParStyle};
use crate::wml::model::{
    BreakType, DrawingRef, ParaProps, Paragraph, RunContent, RunItem, WmlPackage,
};

/// What a single Word paragraph lowers to.
pub enum ParaResult {
    Break(BreakKind),
    Rule,
    Heading { level: u8, body: Inlines },
    ListItem { ordered: bool, level: u8, body: Inlines },
    Figure(Figure),
    FigureAndParagraph { figure: Figure, style: ParStyle, body: Inlines },
    Paragraph { style: ParStyle, body: Inlines },
    /// A paragraph with no visible content — skipped to avoid blank-line spam.
    Empty,
}

pub fn lower_paragraph(p: &Paragraph, package: &WmlPackage, report: &mut ImportReport) -> ParaResult {
    if let Some(kind) = sole_break_kind(p) {
        return ParaResult::Break(kind);
    }

    let eff_para = effective_para(&package.styles, &p.props);
    let inlines = lower_paragraph_inlines(p, package, report);
    let has_text = inlines_have_text(&inlines);
    let drawing_ref = first_drawing(p);

    if eff_para.bottom_border && !has_text && drawing_ref.is_none() {
        return ParaResult::Rule;
    }

    if let Some(level) = heading_level(&package.styles, p.props.style_id.as_deref()) {
        return ParaResult::Heading { level, body: inlines };
    }

    if let Some(num) = eff_para.num {
        let ordered = package.numbering.is_ordered(num.num_id, num.ilvl);
        let level = num.ilvl.clamp(0, i64::from(u8::MAX)) as u8;
        return ParaResult::ListItem { ordered, level, body: inlines };
    }

    if let Some(figure) = drawing_ref.and_then(|d| drawing::lower_drawing(d, package, report)) {
        return if has_text {
            ParaResult::FigureAndParagraph { figure, style: par_style(&eff_para), body: inlines }
        } else {
            ParaResult::Figure(figure)
        };
    }
    // A drawing that couldn't be resolved (already reported) falls through
    // and is treated as an ordinary text paragraph if it has any content.

    if !has_text {
        return ParaResult::Empty;
    }

    ParaResult::Paragraph { style: par_style(&eff_para), body: inlines }
}

/// `Some(kind)` if this paragraph's only content, across all its runs, is a
/// single page/column break (plus optionally whitespace-only text).
fn sole_break_kind(p: &Paragraph) -> Option<BreakKind> {
    let mut kind = None;
    for run_item in &p.runs {
        let RunItem::Run(r) = run_item else { return None };
        for c in &r.content {
            match c {
                RunContent::Break(BreakType::Page) if kind.is_none() => kind = Some(BreakKind::Page),
                RunContent::Break(BreakType::Column) if kind.is_none() => {
                    kind = Some(BreakKind::Column)
                }
                RunContent::Text(t) if t.trim().is_empty() => {}
                _ => return None,
            }
        }
    }
    kind
}

fn first_drawing(p: &Paragraph) -> Option<&DrawingRef> {
    p.runs.iter().find_map(|run_item| match run_item {
        RunItem::Run(r) => r.content.iter().find_map(|c| match c {
            RunContent::Drawing(d) => Some(d),
            _ => None,
        }),
        RunItem::Hyperlink { .. } => None,
    })
}

fn inlines_have_text(inlines: &Inlines) -> bool {
    inlines.iter().any(|inline| match inline {
        Inline::Text(s) => !s.trim().is_empty(),
        Inline::Space | Inline::Linebreak => false,
        Inline::Strong(body) | Inline::Emph(body) => inlines_have_text(body),
        Inline::Raw(s) => !s.is_empty(),
        Inline::Link { body, .. } => inlines_have_text(body),
        Inline::Styled { body, .. } => inlines_have_text(body),
        Inline::Math(s) => !s.is_empty(),
        Inline::Verbatim(s) => !s.is_empty(),
    })
}

fn par_style(eff: &ParaProps) -> ParStyle {
    let align = eff.jc.as_deref().map(|jc| match jc {
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "distribute" => Align::Justify,
        _ => Align::Left,
    });
    ParStyle {
        align,
        leading_pt: eff.line.map(|l| twip_to_abs(l as f64).to_pt()),
        spacing_before_pt: eff.spacing_before.map(|s| twip_to_abs(s as f64).to_pt()),
        indent_pt: eff.indent_left.map(|i| twip_to_abs(i as f64).to_pt()),
    }
}

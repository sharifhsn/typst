//! The `para` mapper: classifies a single Word paragraph into what it
//! becomes in the Typst IR — a heading, list item, rule, page/column break,
//! figure, or an ordinary paragraph — building its inline content via
//! [`crate::mappers::run`]. List-item accumulation *across* paragraphs is a
//! cross-paragraph concern that stays in [`crate::lower`]; this mapper only
//! classifies one paragraph at a time.

use typst_ooxml_core::units::twip_to_abs;

use ecow::EcoString;

use crate::lower::LowerCtx;
use crate::mappers::run::lower_paragraph_inlines;
use crate::mappers::{chart, drawing};
use crate::resolve::styles::{effective_para, heading_level};
use crate::tdoc::{Align, Block, BreakKind, Inline, Inlines, ParStyle};
use crate::wml::model::{BreakType, DrawingRef, ParaProps, Paragraph, RunContent, RunItem};

/// What a single Word paragraph lowers to: the block Word *anchored* in it —
/// a figure or a chart, which Word hangs off a paragraph but Typst renders as
/// a block of its own — plus what the paragraph's own content becomes.
///
/// Keeping the two separate is what stops an anchored block from being lost
/// when the paragraph turns out to be something other than a plain paragraph.
/// A bulleted list item holding a screenshot is ordinary in real documents,
/// and classifying it as "a list item" used to discard the image entirely.
pub struct ParaResult {
    pub anchored: Option<Block>,
    pub kind: ParaKind,
}

/// What a paragraph's *own* content becomes, independent of anything anchored
/// in it.
pub enum ParaKind {
    Break(BreakKind),
    Rule,
    Heading { level: u8, body: Inlines },
    ListItem { ordered: bool, level: u8, body: Inlines },
    Paragraph { style: ParStyle, body: Inlines },
    /// A paragraph with no visible content — skipped to avoid blank-line spam.
    Empty,
}

impl ParaResult {
    fn bare(kind: ParaKind) -> Self {
        ParaResult { anchored: None, kind }
    }
}

pub(crate) fn lower_paragraph(p: &Paragraph, ctx: &mut LowerCtx) -> ParaResult {
    if let Some(kind) = sole_break_kind(p) {
        return ParaResult::bare(ParaKind::Break(kind));
    }

    let package = ctx.package;
    let eff_para = effective_para(&package.styles, &p.props);
    let inlines = lower_paragraph_inlines(p, ctx);
    let has_text = inlines_have_text(&inlines);
    let drawing_ref = first_drawing(p);

    // Resolve the anchored block up front, before classifying the paragraph,
    // so it survives every branch below. A drawing wins over a chart when a
    // paragraph somehow carries both; an unresolvable one (already reported)
    // simply yields `None` and the paragraph is treated as text-only.
    let anchored = drawing_ref
        .and_then(|d| drawing::lower_drawing(d, package, &mut *ctx.report))
        .map(Block::Figure)
        .or_else(|| {
            first_chart(p)
                .and_then(|rid| chart::lower_chart(rid, package, &mut *ctx.report))
                .map(Block::Chart)
        });

    let kind = if eff_para.bottom_border && !has_text && drawing_ref.is_none() {
        ParaKind::Rule
    } else if let Some(level) = heading_level(&package.styles, p.props.style_id.as_deref()) {
        ParaKind::Heading { level, body: inlines }
    } else if let Some(num) = eff_para.num {
        let ordered = package.numbering.is_ordered(num.num_id, num.ilvl);
        let level = num.ilvl.clamp(0, i64::from(u8::MAX)) as u8;
        ParaKind::ListItem { ordered, level, body: inlines }
    } else if has_text {
        ParaKind::Paragraph { style: par_style(&eff_para), body: inlines }
    } else {
        ParaKind::Empty
    };

    ParaResult { anchored, kind }
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
        // A drawing nested inside a hyperlink or a field's cached result
        // isn't discovered as the paragraph's figure — same simplification
        // as the pre-existing hyperlink exclusion; out of scope for v1.
        RunItem::Hyperlink { .. } | RunItem::Field(_) => None,
    })
}

/// The first chart reference among this paragraph's own runs — same
/// simplification (and same reasoning) as [`first_drawing`] just above.
fn first_chart(p: &Paragraph) -> Option<&EcoString> {
    p.runs.iter().find_map(|run_item| match run_item {
        RunItem::Run(r) => r.content.iter().find_map(|c| match c {
            RunContent::Chart(rel_id) => Some(rel_id),
            _ => None,
        }),
        RunItem::Hyperlink { .. } | RunItem::Field(_) => None,
    })
}

/// Whether any inline in this sequence carries visible text — recursively,
/// through strong/emph/link/styled wrappers. Also used by
/// [`crate::mappers::section`] to decide whether a lowered furniture body
/// (header/footer) is visually empty and should be dropped.
pub(crate) fn inlines_have_text(inlines: &Inlines) -> bool {
    inlines.iter().any(|inline| match inline {
        Inline::Text(s) => !s.trim().is_empty(),
        Inline::Space | Inline::Linebreak => false,
        Inline::Strong(body) | Inline::Emph(body) => inlines_have_text(body),
        Inline::Raw(s) => !s.is_empty(),
        Inline::Link { body, .. } => inlines_have_text(body),
        Inline::Styled { body, .. } => inlines_have_text(body),
        Inline::Math(s) => !s.is_empty(),
        // A footnote reference renders a visible marker at the reference
        // site regardless of what its own body contains — a paragraph whose
        // only content is one must not be classified `ParaResult::Empty`
        // and dropped, which would silently delete the note along with it.
        Inline::Footnote(_) => true,
        // A ruby's visible text is its base (plus the reading above it).
        Inline::Ruby { base, gloss } => inlines_have_text(base) || inlines_have_text(gloss),
        // Same reasoning, more consequential: a text box's own content lives
        // in its nested block sequence, not the paragraph's inline text.
        // Treating it as "no text" would risk classifying a paragraph whose
        // only content is a text box as `ParaResult::Empty`, dropping the
        // whole box along with it — the exact regression this construct
        // exists to fix.
        Inline::TextBox(_) => true,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wml::model::{BodyItem, Run, RunProps, WmlPackage};

    /// A paragraph whose *only* content is a text box (no other text, no
    /// image) must not be classified `ParaResult::Empty` — that would drop
    /// the whole box along with it, silently regressing back to this
    /// construct's original bug (text boxes dropped entirely).
    #[test]
    fn a_paragraph_containing_only_a_text_box_is_not_classified_empty() {
        let inner = BodyItem::Paragraph(Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::Text("boxed".into())],
            })],
        });
        let p = Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::TextBox(vec![inner])],
            })],
        };
        let package = WmlPackage::default();
        let mut report = crate::report::ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let result = lower_paragraph(&p, &mut ctx);

        match result.kind {
            ParaKind::Paragraph { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(&body[0], Inline::TextBox(_)));
            }
            ParaKind::Empty => panic!("the text box was dropped along with the paragraph"),
            _ => panic!("expected ParaKind::Paragraph, got a different variant"),
        }
    }
}

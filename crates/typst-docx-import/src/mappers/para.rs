//! The `para` mapper: classifies a single Word paragraph into what it
//! becomes in the Typst IR — a heading, list item, rule, page/column break,
//! figure, or an ordinary paragraph — building its inline content via
//! [`crate::mappers::run`]. List-item accumulation *across* paragraphs is a
//! cross-paragraph concern that stays in [`crate::lower`]; this mapper only
//! classifies one paragraph at a time.

use ecow::EcoString;
use typst_ooxml_core::units::twip_to_abs;

use crate::lower::{parse_hex_color, LowerCtx};
use crate::mappers::run::lower_paragraph_inlines;
use crate::mappers::{chart, drawing, math};
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
#[derive(Debug)]
pub enum ParaKind {
    Break(BreakKind),
    Rule,
    Heading { level: u8, body: Inlines },
    /// `num_id` is carried through so the caller can resolve the list's
    /// format and starting number once it knows every level the run uses —
    /// see `lower::PendingList`.
    ListItem { ordered: bool, level: u8, body: Inlines, num_id: Option<i64> },
    Paragraph { style: ParStyle, body: Inlines },
    /// A paragraph that *is* a display equation (`m:oMathPara`).
    Equation { body: EcoString },
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
        // A page/column break only means something at the document's own
        // flow level — inside a table cell, text box, footnote, or
        // header/footer, Typst rejects it outright ("pagebreaks are not
        // allowed inside of containers"). Dropping it here (rather than
        // filtering it out in the emitter) is the paragraph mapper's own
        // call to make: the emitter is a pretty-printer, not the place for a
        // semantic decision like this.
        if ctx.in_container() {
            ctx.report.drop(
                "page/column break",
                "pagebreaks are not allowed inside a table cell, text box, footnote, or \
                 header/footer; dropped",
            );
            return ParaResult::bare(ParaKind::Empty);
        }
        return ParaResult::bare(ParaKind::Break(kind));
    }

    // A paragraph that *is* a display equation becomes a block equation,
    // rather than an inline `$..$` marooned in a paragraph of its own.
    if let Some(xml) = sole_display_equation(p)
        && let Inline::Math(body) = math::omml_to_inline(xml, &mut *ctx.report)
    {
        return ParaResult::bare(ParaKind::Equation { body });
    }

    let package = ctx.package;
    let eff_para = effective_para(&package.styles, &p.props);
    let heading = heading_level(&package.styles, p.props.style_id.as_deref());

    // A TOC field lowered from inside a heading's own content must not
    // become a live `#outline()` (it renders every heading, including the
    // one containing it — infinite recursion), so `mappers::field` needs to
    // know whether this paragraph's inlines are a heading's. Computed before
    // lowering the inlines (rather than after, alongside the rest of this
    // paragraph's classification below) specifically so it's in place while
    // they're lowered.
    let was_in_heading = heading.is_some().then(|| ctx.enter_heading());
    let mut inlines = lower_paragraph_inlines(p, ctx);
    hoist_labels(&mut inlines);
    if let Some(was_in_heading) = was_in_heading {
        ctx.exit_heading(was_in_heading);
    }

    let has_text = inlines_have_text(&inlines);
    let drawing_ref = first_drawing(p);

    // Resolve the anchored block up front, before classifying the paragraph,
    // so it survives every branch below. A drawing wins over a chart when a
    // paragraph somehow carries both; an unresolvable one (already reported)
    // simply yields `None` and the paragraph is treated as text-only. `ctx`
    // isn't needed again in this function after this point, so the chart
    // branch can take it outright rather than reborrowing just its `report`
    // field the way the drawing branch does (chart lowering also reads
    // `ctx.options` to decide table-vs-plot, so it needs the whole context).
    let anchored = drawing_ref
        .and_then(|d| drawing::lower_drawing(d, package, &mut *ctx.report))
        .map(Block::Figure)
        .or_else(|| first_chart(p).and_then(|d| chart::lower_chart(d, ctx)).map(Block::Chart));

    let kind = if eff_para.bottom_border && !has_text && drawing_ref.is_none() {
        ParaKind::Rule
    } else if let Some(level) = heading {
        ParaKind::Heading { level, body: inlines }
    } else if let Some(num) = eff_para.num {
        let ordered = package.numbering.is_ordered(num.num_id, num.ilvl);
        let level = num.ilvl.clamp(0, i64::from(u8::MAX)) as u8;
        ParaKind::ListItem { ordered, level, body: inlines, num_id: Some(num.num_id) }
    } else if has_text {
        ParaKind::Paragraph { style: par_style(&eff_para), body: inlines }
    } else {
        ParaKind::Empty
    };

    ParaResult { anchored, kind }
}

/// Move every [`Inline::Label`] to the end of `inlines`.
///
/// Typst attaches a label to whatever *precedes* it, while Word writes a
/// `w:bookmarkStart` at the *start* of the paragraph it marks — emitted where
/// it was found, it would label the previous block instead. Relative order
/// among the labels is preserved, so a paragraph carrying several bookmarks
/// keeps them in document order.
fn hoist_labels(inlines: &mut Inlines) {
    if !inlines.iter().any(|inline| matches!(inline, Inline::Label(_))) {
        return;
    }
    let (labels, rest): (Vec<_>, Vec<_>) = std::mem::take(inlines)
        .into_iter()
        .partition(|inline| matches!(inline, Inline::Label(_)));
    *inlines = rest;

    // Typst allows exactly one label per element, so only the first can ride
    // on the block itself; each later one needs an element of its own to
    // attach to, or it silently overrides its predecessor and every reference
    // to the overridden name fails to compile. `#metadata(none)` is Typst's
    // invisible, labellable element — precisely a bare anchor. Word documents
    // hit this routinely: a heading commonly carries both a `_Toc` and a
    // `_Ref` bookmark.
    for (index, label) in labels.into_iter().enumerate() {
        if index > 0 {
            inlines.push(Inline::Verbatim("#metadata(none)".into()));
        }
        inlines.push(label);
    }
}

/// `Some(xml)` if this paragraph's only content is a single *display*
/// equation — Word's `m:oMathPara`. Mirrors [`sole_break_kind`]: Word pads a
/// display equation's paragraph with empty runs, so whitespace-only text is
/// tolerated, but any real content means this is a paragraph that merely
/// *contains* an equation rather than one that is one.
fn sole_display_equation(p: &Paragraph) -> Option<&EcoString> {
    let mut equation = None;
    for run_item in &p.runs {
        // A bookmark carries no visible content, so it never disqualifies an
        // otherwise-sole break/equation — it just rides along as a label.
        if matches!(run_item, RunItem::Bookmark(_)) {
            continue;
        }
        let RunItem::Run(r) = run_item else { return None };
        for c in &r.content {
            match c {
                RunContent::Math { xml, display: true } if equation.is_none() => {
                    equation = Some(xml)
                }
                RunContent::Text(t) if t.trim().is_empty() => {}
                _ => return None,
            }
        }
    }
    equation
}

/// `Some(kind)` if this paragraph's only content, across all its runs, is a
/// single page/column break (plus optionally whitespace-only text).
fn sole_break_kind(p: &Paragraph) -> Option<BreakKind> {
    let mut kind = None;
    for run_item in &p.runs {
        // A bookmark carries no visible content, so it never disqualifies an
        // otherwise-sole break/equation — it just rides along as a label.
        if matches!(run_item, RunItem::Bookmark(_)) {
            continue;
        }
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
        RunItem::Hyperlink { .. } | RunItem::Field(_) | RunItem::Bookmark(_) => None,
    })
}

/// The first chart reference among this paragraph's own runs — same
/// simplification (and same reasoning) as [`first_drawing`] just above.
fn first_chart(p: &Paragraph) -> Option<&DrawingRef> {
    p.runs.iter().find_map(|run_item| match run_item {
        RunItem::Run(r) => r.content.iter().find_map(|c| match c {
            RunContent::Chart(d) => Some(d),
            _ => None,
        }),
        RunItem::Hyperlink { .. } | RunItem::Field(_) | RunItem::Bookmark(_) => None,
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
        Inline::LabelLink { body, .. } => inlines_have_text(body),
        // A label renders nothing of its own — it only names the block it
        // rides on, so it can't make an otherwise-empty paragraph visible.
        Inline::Label(_) => false,
        // A page reference renders a number, so it is visible content.
        Inline::PageRef(_) => true,
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
        // A drawn shape is visible ink in its own right, whether or not Word
        // also put text inside it — the same reasoning as a text box, and the
        // same consequence: a paragraph whose only content is a shape must not
        // be classified `ParaKind::Empty` and dropped.
        Inline::Shape { .. } => true,
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
    let twips = |t: Option<i64>| t.map(|t| twip_to_abs(t as f64).to_pt());
    ParStyle {
        align,
        leading_pt: twips(eff.line),
        spacing_before_pt: twips(eff.spacing_before),
        spacing_after_pt: twips(eff.spacing_after),
        indent_pt: twips(eff.indent_left),
        indent_right_pt: twips(eff.indent_right),
        first_line_indent_pt: twips(eff.indent_first_line),
        hanging_indent_pt: twips(eff.indent_hanging),
        fill: parse_hex_color(eff.shd_fill.as_deref()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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

    fn page_break_paragraph() -> Paragraph {
        Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::Break(BreakType::Page)],
            })],
        }
    }

    /// A page/column break at the document's own flow level lowers to a
    /// real break, same as always — `LowerCtx::in_container` starts `false`.
    #[test]
    fn page_break_outside_a_container_lowers_normally() {
        let package = WmlPackage::default();
        let mut report = crate::report::ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let result = lower_paragraph(&page_break_paragraph(), &mut ctx);
        assert!(matches!(result.kind, ParaKind::Break(BreakKind::Page)));
    }

    /// The same break, but lowered while `LowerCtx` says a table
    /// cell/text box/footnote/header is being lowered — Typst rejects
    /// `#pagebreak()` inside any of those outright ("pagebreaks are not
    /// allowed inside of containers"), so it must be dropped (with a report
    /// note) instead of emitted.
    #[test]
    fn page_break_inside_a_container_is_dropped_and_reported() {
        let package = WmlPackage::default();
        let mut report = crate::report::ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        ctx.enter_container();
        let result = lower_paragraph(&page_break_paragraph(), &mut ctx);
        assert!(matches!(result.kind, ParaKind::Empty));
        assert_eq!(ctx.report.notes.len(), 1);
        assert_eq!(ctx.report.notes[0].what, "page/column break");
    }

    /// `= #outline()` end to end: a heading-styled paragraph whose only
    /// content is a TOC field must lower to the field's cached text, not a
    /// live `#outline()` — verifying that `lower_paragraph` actually enters
    /// `LowerCtx`'s heading scope *before* lowering the paragraph's own
    /// inlines (where `mappers::field::lower_field` reads it), not after.
    #[test]
    fn a_toc_field_inside_a_heading_paragraph_uses_its_cached_text() {
        use rustc_hash::FxHashMap;

        use crate::wml::model::{Field, Style, StyleKind, Styles, WmlPackage};

        let mut by_id = FxHashMap::default();
        by_id.insert(
            "Heading1".into(),
            Style {
                id: "Heading1".into(),
                name: Some("heading 1".into()),
                kind: StyleKind::Paragraph,
                based_on: None,
                outline_level: Some(0),
                run: RunProps::default(),
                para: ParaProps::default(),
            },
        );
        let package = WmlPackage {
            styles: Styles { by_id, ..Default::default() },
            ..Default::default()
        };

        let p = Paragraph {
            props: ParaProps { style_id: Some("Heading1".into()), ..Default::default() },
            runs: vec![RunItem::Field(Field {
                instr: " TOC \\o \"1-3\" \\h ".into(),
                result: vec![RunItem::Run(Run {
                    props: RunProps::default(),
                    content: vec![RunContent::Text("stale toc".into())],
                })],
            })],
        };

        let mut report = crate::report::ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let result = lower_paragraph(&p, &mut ctx);

        match result.kind {
            ParaKind::Heading { body, .. } => {
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "stale toc"));
            }
            _ => panic!("expected ParaKind::Heading, got a different variant"),
        }
        assert!(ctx.report.notes.iter().any(|n| n.what == "field TOC"));
        // The heading scope must not leak past this paragraph.
        assert!(!ctx.in_heading());
    }

    const MATH_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

    fn math_paragraph(display: bool, with_text: bool) -> Paragraph {
        let mut content = vec![RunContent::Math {
            xml: format!(r#"<m:oMath xmlns:m="{MATH_NS}"><m:r><m:t>x</m:t></m:r></m:oMath>"#)
                .into(),
            display,
        }];
        if with_text {
            content.push(RunContent::Text("and prose".into()));
        }
        Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run { props: RunProps::default(), content })],
        }
    }

    /// Word's `m:oMathPara` is a *block* equation, so a paragraph that is
    /// entirely one must become a `Block::Equation` rather than an inline
    /// `$..$` marooned in a paragraph of its own.
    #[test]
    fn a_display_equation_paragraph_becomes_a_block_equation() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let result = lower_paragraph(&math_paragraph(true, false), &mut ctx);

        assert!(
            matches!(result.kind, ParaKind::Equation { .. }),
            "expected a block equation, got {:?}",
            result.kind
        );
    }

    /// A display equation that shares its paragraph with prose can only be set
    /// inline — the paragraph is not itself an equation.
    #[test]
    fn an_equation_beside_text_stays_inline() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let result = lower_paragraph(&math_paragraph(true, true), &mut ctx);

        assert!(
            matches!(result.kind, ParaKind::Paragraph { .. }),
            "expected an ordinary paragraph, got {:?}",
            result.kind
        );
    }

    /// An `m:oMath` with no `m:oMathPara` around it is inline by definition.
    #[test]
    fn an_inline_equation_alone_in_a_paragraph_stays_inline() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let result = lower_paragraph(&math_paragraph(false, false), &mut ctx);

        assert!(
            matches!(result.kind, ParaKind::Paragraph { .. }),
            "expected an ordinary paragraph, got {:?}",
            result.kind
        );
    }
}

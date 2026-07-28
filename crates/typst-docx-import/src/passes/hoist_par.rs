//! Hoist common paragraph formatting into the preamble.
//!
//! Word documents repeat their body-text formatting on (almost) every
//! paragraph — justification, and the spacing/line-height that the `Normal`
//! style stamps onto everything — which `lower` turns into a per-paragraph
//! wrapper on each one. When a strong majority of top-level paragraphs agree,
//! this pass instead states it once in a `#set par(..)` and clears the
//! now-redundant fields, so `emit::render_paragraph` only wraps paragraphs
//! that genuinely deviate. Without this, a typical Word document would emit a
//! `#block(above: .., below: ..)` around every single paragraph.
//!
//! Conservative throughout: it acts only on a clear majority, never a bare
//! plurality, and for alignment only on `Align::Justify` (Word's common
//! body-text default).

use rustc_hash::FxHashMap;

use crate::tdoc::{Align, Block, ParStyle, Section, Stmt, TypstDoc};

/// The fraction of top-level paragraphs that must agree before this pass
/// hoists a value into the preamble.
const MAJORITY_THRESHOLD: f64 = 0.6;

/// Entry point: hoist document-wide paragraph formatting where a strong
/// majority of top-level paragraphs agree.
pub fn run(doc: &mut TypstDoc) {
    hoist_justify(doc);
    hoist_metrics(doc);
}

fn hoist_justify(doc: &mut TypstDoc) {
    let total = count_paragraphs(&doc.body);
    if total == 0 {
        return;
    }
    let justified = count_justified(&doc.body);

    if (justified as f64) / (total as f64) < MAJORITY_THRESHOLD {
        return;
    }

    set_preamble_justify(&mut doc.preamble);
    clear_justify(&mut doc.body);
}

/// The paragraph measurements this pass hoists as a unit.
///
/// Voted on together rather than field-by-field: Typst's single `par.spacing`
/// is fed by *both* of Word's spacing values (see [`ParStyle`]), so neither
/// can be cleared from a paragraph unless the other agrees too — clearing one
/// alone would silently change the gap the paragraph asked for. Leading rides
/// along in the same vote because a document that shares one of these
/// invariably shares all three.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Metrics {
    spacing_before_pt: Option<f64>,
    spacing_after_pt: Option<f64>,
    leading_pt: Option<f64>,
}

/// [`Metrics`] quantised for use as a vote key: `f64` is neither `Eq` nor
/// `Hash`, and two values differing by less than a thousandth of a point are
/// the same measurement as far as Word's twip-based source is concerned.
type MetricsKey = (Option<i64>, Option<i64>, Option<i64>);

impl Metrics {
    fn of(style: &ParStyle) -> Self {
        Metrics {
            spacing_before_pt: style.spacing_before_pt,
            spacing_after_pt: style.spacing_after_pt,
            leading_pt: style.leading_pt,
        }
    }

    fn key(&self) -> MetricsKey {
        let quantise = |v: Option<f64>| v.map(|v| (v * 1000.0).round() as i64);
        (
            quantise(self.spacing_before_pt),
            quantise(self.spacing_after_pt),
            quantise(self.leading_pt),
        )
    }

    /// Nothing to hoist — the paragraph states no measurement at all.
    fn is_empty(&self) -> bool {
        *self == Metrics::default()
    }
}

fn hoist_metrics(doc: &mut TypstDoc) {
    let total = count_paragraphs(&doc.body);
    if total == 0 {
        return;
    }

    let mut styles = Vec::with_capacity(total);
    collect_styles(&doc.body, &mut styles);

    let mut votes: FxHashMap<MetricsKey, (usize, Metrics)> = FxHashMap::default();
    for metrics in styles.iter().copied().map(Metrics::of) {
        let entry = votes.entry(metrics.key()).or_insert((0, metrics));
        entry.0 += 1;
    }

    let Some((count, winner)) = votes.into_values().max_by_key(|(count, _)| *count)
    else {
        return;
    };
    if winner.is_empty() || (count as f64) / (total as f64) < MAJORITY_THRESHOLD {
        return;
    }

    set_preamble_metrics(&mut doc.preamble, &winner);
    clear_metrics(&mut doc.body, &winner);
}

/// Every top-level paragraph's style, by the same walk as
/// [`count_paragraphs`].
fn collect_styles<'a>(blocks: &'a [Block], out: &mut Vec<&'a ParStyle>) {
    for block in blocks {
        match block {
            Block::Paragraph { style, .. } => out.push(style),
            Block::Section(Section { body, .. }) => collect_styles(body, out),
            _ => {}
        }
    }
}

/// Clear the hoisted measurements from every paragraph that agrees with them,
/// leaving deviating paragraphs to carry their own wrapper.
fn clear_metrics(blocks: &mut [Block], hoisted: &Metrics) {
    for block in blocks {
        match block {
            Block::Paragraph { style, .. }
                if Metrics::of(style).key() == hoisted.key() =>
            {
                style.spacing_before_pt = None;
                style.spacing_after_pt = None;
                style.leading_pt = None;
            }
            Block::Section(Section { body, .. }) => clear_metrics(body, hoisted),
            _ => {}
        }
    }
}

/// Merge `hoisted` into the preamble's existing `Stmt::SetPar`, or push a new
/// one if there isn't one yet — the [`set_preamble_justify`] counterpart.
fn set_preamble_metrics(preamble: &mut Vec<Stmt>, hoisted: &Metrics) {
    let apply = |style: &mut ParStyle| {
        style.spacing_before_pt = hoisted.spacing_before_pt;
        style.spacing_after_pt = hoisted.spacing_after_pt;
        style.leading_pt = hoisted.leading_pt;
    };
    for stmt in preamble.iter_mut() {
        if let Stmt::SetPar(style) = stmt {
            apply(style);
            return;
        }
    }
    let mut style = ParStyle::default();
    apply(&mut style);
    preamble.push(Stmt::SetPar(style));
}

/// Every top-level paragraph in `blocks`, recursing into a [`Block::Section`]'s
/// own content — a later Word section's body is still document body text and
/// gets the same vote the first section's paragraphs do — but never into a
/// table cell, list item, or footnote (sub-document content that shouldn't
/// sway a document-wide decision), and never into a section's *header/footer*
/// furniture either, for the same reason this pass never visits the
/// preamble's own header/footer (see this module's own doc comment).
fn count_paragraphs(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph { .. } => 1,
            Block::Section(Section { body, .. }) => count_paragraphs(body),
            _ => 0,
        })
        .sum()
}

/// Same walk as [`count_paragraphs`], counting only the `Align::Justify` ones.
fn count_justified(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph { style, .. } if style.align == Some(Align::Justify) => 1,
            Block::Section(Section { body, .. }) => count_justified(body),
            _ => 0,
        })
        .sum()
}

/// Clears the now-redundant per-paragraph `Align::Justify` — the same walk as
/// [`count_paragraphs`]/[`count_justified`].
fn clear_justify(blocks: &mut [Block]) {
    for block in blocks {
        match block {
            Block::Paragraph { style, .. } if style.align == Some(Align::Justify) => {
                style.align = None;
            }
            Block::Section(Section { body, .. }) => clear_justify(body),
            _ => {}
        }
    }
}

/// Set `align: Some(Justify)` on the preamble's existing `Stmt::SetPar`, or
/// push a new one if there isn't one yet.
fn set_preamble_justify(preamble: &mut Vec<Stmt>) {
    for stmt in preamble.iter_mut() {
        if let Stmt::SetPar(style) = stmt {
            style.align = Some(Align::Justify);
            return;
        }
    }
    preamble.push(Stmt::SetPar(ParStyle {
        align: Some(Align::Justify),
        ..Default::default()
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdoc::Inline;

    fn justified_para(text: &str) -> Block {
        Block::Paragraph {
            style: ParStyle { align: Some(Align::Justify), ..Default::default() },
            body: vec![Inline::Text(text.into())],
        }
    }

    fn plain_para(text: &str) -> Block {
        Block::Paragraph {
            style: ParStyle::default(),
            body: vec![Inline::Text(text.into())],
        }
    }

    #[test]
    fn majority_justify_is_hoisted_into_preamble() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                justified_para("a"),
                justified_para("b"),
                justified_para("c"),
                plain_para("d"),
            ],
        };
        run(&mut doc);

        assert!(doc.preamble.iter().any(
            |s| matches!(s, Stmt::SetPar(style) if style.align == Some(Align::Justify))
        ));
        for block in &doc.body[..3] {
            match block {
                Block::Paragraph { style, .. } => assert_eq!(style.align, None),
                _ => panic!("expected paragraph"),
            }
        }
    }

    #[test]
    fn no_clear_majority_does_nothing() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![justified_para("a"), plain_para("b"), plain_para("c")],
        };
        run(&mut doc);

        assert!(doc.preamble.is_empty());
        match &doc.body[0] {
            Block::Paragraph { style, .. } => {
                assert_eq!(style.align, Some(Align::Justify))
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn center_alignment_is_never_touched() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                Block::Paragraph {
                    style: ParStyle { align: Some(Align::Center), ..Default::default() },
                    body: vec![Inline::Text("title".into())],
                },
                justified_para("a"),
                justified_para("b"),
            ],
        };
        run(&mut doc);

        // 2/3 justify clears the majority bar; the Center paragraph is untouched.
        match &doc.body[0] {
            Block::Paragraph { style, .. } => {
                assert_eq!(style.align, Some(Align::Center))
            }
            _ => panic!("expected paragraph"),
        }
    }

    fn spaced_para(text: &str, before: f64, after: f64) -> Block {
        Block::Paragraph {
            style: ParStyle {
                spacing_before_pt: Some(before),
                spacing_after_pt: Some(after),
                ..Default::default()
            },
            body: vec![Inline::Text(text.into())],
        }
    }

    fn par_style_of(block: &Block) -> &ParStyle {
        match block {
            Block::Paragraph { style, .. } => style,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    /// The whole point of the metric hoist: Word stamps the same spacing onto
    /// every paragraph, and without this each one would emit its own
    /// `#block(above: .., below: ..)` wrapper.
    #[test]
    fn a_shared_spacing_is_hoisted_and_cleared_from_paragraphs() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![spaced_para("a", 6.0, 6.0), spaced_para("b", 6.0, 6.0)],
        };
        run(&mut doc);

        let hoisted = doc.preamble.iter().find_map(|s| match s {
            Stmt::SetPar(style) => Some(style),
            _ => None,
        });
        let hoisted = hoisted.expect("expected a hoisted #set par");
        assert_eq!(hoisted.spacing_before_pt, Some(6.0));
        assert_eq!(hoisted.spacing_after_pt, Some(6.0));

        for block in &doc.body {
            let style = par_style_of(block);
            assert_eq!(style.spacing_before_pt, None);
            assert_eq!(style.spacing_after_pt, None);
        }
    }

    /// A paragraph that genuinely deviates keeps its own measurements, so it
    /// still emits a wrapper while the majority stays bare.
    #[test]
    fn a_deviating_paragraph_keeps_its_own_spacing() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                spaced_para("a", 6.0, 6.0),
                spaced_para("b", 6.0, 6.0),
                spaced_para("pull-quote", 24.0, 24.0),
            ],
        };
        run(&mut doc);

        assert_eq!(par_style_of(&doc.body[0]).spacing_before_pt, None);
        assert_eq!(par_style_of(&doc.body[2]).spacing_before_pt, Some(24.0));
    }

    /// Spacing is voted as a pair — clearing only one half would silently
    /// change the gap the paragraph asked for, so a document that agrees on
    /// `before` but not `after` hoists neither.
    #[test]
    fn spacing_is_hoisted_only_when_both_halves_agree() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                spaced_para("a", 6.0, 2.0),
                spaced_para("b", 6.0, 10.0),
                spaced_para("c", 6.0, 18.0),
            ],
        };
        run(&mut doc);

        assert!(
            !doc.preamble.iter().any(|s| matches!(s, Stmt::SetPar(_))),
            "no (before, after) pair holds a majority, so nothing should hoist"
        );
        assert_eq!(par_style_of(&doc.body[0]).spacing_before_pt, Some(6.0));
    }

    /// Paragraphs that state no measurements at all must not hoist an empty
    /// `#set par()`.
    #[test]
    fn unmeasured_paragraphs_hoist_nothing() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                Block::Paragraph {
                    style: ParStyle::default(),
                    body: vec![Inline::Text("a".into())],
                },
                Block::Paragraph {
                    style: ParStyle::default(),
                    body: vec![Inline::Text("b".into())],
                },
            ],
        };
        run(&mut doc);

        assert!(doc.preamble.is_empty());
    }
}

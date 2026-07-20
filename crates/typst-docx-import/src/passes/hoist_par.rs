//! Hoist a common paragraph justification into the preamble.
//!
//! Word documents that justify body text do so on (almost) every paragraph,
//! which `lower` turns into a `#par(justify: true)[..]` wrapper on each one.
//! When a strong majority of top-level paragraphs agree, this pass instead
//! sets it once via `#set par(justify: true)` and clears the per-paragraph
//! flag on the paragraphs that had it — conservative: it only ever touches
//! `Align::Justify` (Word's common body-text default), and only when there's
//! a clear majority, never a bare plurality.

use crate::tdoc::{Align, Block, ParStyle, Section, Stmt, TypstDoc};

/// The fraction of top-level paragraphs that must be `Align::Justify` before
/// this pass hoists it into the preamble.
const MAJORITY_THRESHOLD: f64 = 0.6;

/// Entry point: hoist a document-wide `justify` if a strong majority of
/// top-level paragraphs agree.
pub fn run(doc: &mut TypstDoc) {
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
    preamble.push(Stmt::SetPar(ParStyle { align: Some(Align::Justify), ..Default::default() }));
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
        Block::Paragraph { style: ParStyle::default(), body: vec![Inline::Text(text.into())] }
    }

    #[test]
    fn majority_justify_is_hoisted_into_preamble() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![justified_para("a"), justified_para("b"), justified_para("c"), plain_para("d")],
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
            Block::Paragraph { style, .. } => assert_eq!(style.align, Some(Align::Justify)),
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
            Block::Paragraph { style, .. } => assert_eq!(style.align, Some(Align::Center)),
            _ => panic!("expected paragraph"),
        }
    }
}

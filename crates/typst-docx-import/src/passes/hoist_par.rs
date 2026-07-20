//! Hoist a common paragraph justification into the preamble.
//!
//! Word documents that justify body text do so on (almost) every paragraph,
//! which `lower` turns into a `#par(justify: true)[..]` wrapper on each one.
//! When a strong majority of top-level paragraphs agree, this pass instead
//! sets it once via `#set par(justify: true)` and clears the per-paragraph
//! flag on the paragraphs that had it — conservative: it only ever touches
//! `Align::Justify` (Word's common body-text default), and only when there's
//! a clear majority, never a bare plurality.

use crate::tdoc::{Align, Block, ParStyle, Stmt, TypstDoc};

/// The fraction of top-level paragraphs that must be `Align::Justify` before
/// this pass hoists it into the preamble.
const MAJORITY_THRESHOLD: f64 = 0.6;

/// Entry point: hoist a document-wide `justify` if a strong majority of
/// top-level paragraphs agree.
pub fn run(doc: &mut TypstDoc) {
    let total = doc.body.iter().filter(|b| matches!(b, Block::Paragraph { .. })).count();
    if total == 0 {
        return;
    }
    let justified = doc
        .body
        .iter()
        .filter(|b| matches!(b, Block::Paragraph { style, .. } if style.align == Some(Align::Justify)))
        .count();

    if (justified as f64) / (total as f64) < MAJORITY_THRESHOLD {
        return;
    }

    set_preamble_justify(&mut doc.preamble);

    for block in &mut doc.body {
        if let Block::Paragraph { style, .. } = block
            && style.align == Some(Align::Justify)
        {
            style.align = None;
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

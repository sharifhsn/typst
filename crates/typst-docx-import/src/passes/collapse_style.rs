//! Collapse redundant `#text(..)` styling.
//!
//! Every text run comes out of `lower` fully literal: it re-states the
//! document's default font/size/color even where that's already covered by
//! the preamble's `#set text(..)`. This pass diffs each [`Inline::Styled`]
//! run's [`TextStyle`] against the document default and drops whatever
//! matches, unwrapping the run entirely when nothing is left to say.
//!
//! Headings get an extra rule: a heading's size/weight is implied by the `=`
//! marker itself, so `size`/`bold`/`font`/`color` are cleared unconditionally
//! inside [`Block::Heading`] bodies, not just when they match the doc default.

use crate::tdoc::{Block, Figure, Inline, Inlines, List, Stmt, Table, TextStyle, TypstDoc};

/// Entry point: collapse every run in the document — body *and* header/footer
/// content — against the preamble's default text style.
pub fn run(doc: &mut TypstDoc) {
    let default = default_text_style(&doc.preamble);
    for tree in doc.block_trees_mut() {
        for block in tree {
            walk_block(block, &default);
        }
    }
}

fn default_text_style(preamble: &[Stmt]) -> TextStyle {
    preamble
        .iter()
        .find_map(|stmt| match stmt {
            Stmt::SetText(style) => Some(style.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn walk_block(block: &mut Block, default: &TextStyle) {
    match block {
        Block::Heading { body, .. } => collapse_in_place(body, default, true),
        Block::Paragraph { body, .. } => collapse_in_place(body, default, false),
        Block::List(List { items }) => {
            for item in items {
                collapse_in_place(&mut item.body, default, false);
            }
        }
        Block::Table(Table { rows, .. }) => {
            for row in rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.body {
                        walk_block(inner, default);
                    }
                }
            }
        }
        Block::Figure(Figure { caption, .. }) => {
            if let Some(caption) = caption {
                collapse_in_place(caption, default, false);
            }
        }
        Block::CodeBlock { .. }
        | Block::Equation { .. }
        | Block::Rule
        | Block::Break(_)
        | Block::Verbatim(_) => {}
    }
}

fn collapse_in_place(inlines: &mut Inlines, default: &TextStyle, in_heading: bool) {
    let taken = std::mem::take(inlines);
    *inlines = collapse_inlines(taken, default, in_heading);
}

fn collapse_inlines(inlines: Inlines, default: &TextStyle, in_heading: bool) -> Inlines {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        collapse_inline(inline, default, in_heading, &mut out);
    }
    out
}

fn collapse_inline(inline: Inline, default: &TextStyle, in_heading: bool, out: &mut Inlines) {
    match inline {
        Inline::Strong(body) => {
            out.push(Inline::Strong(collapse_inlines(body, default, in_heading)))
        }
        Inline::Emph(body) => out.push(Inline::Emph(collapse_inlines(body, default, in_heading))),
        Inline::Link { dest, body } => out.push(Inline::Link {
            dest,
            body: collapse_inlines(body, default, in_heading),
        }),
        Inline::Styled { style, body } => {
            let body = collapse_inlines(body, default, in_heading);
            let reduced = reduce_style(&style, default, in_heading);
            if reduced.is_empty() {
                // Nothing left to say — splice the body in place of the wrapper.
                out.extend(body);
            } else {
                out.push(Inline::Styled { style: reduced, body });
            }
        }
        other @ (Inline::Text(_)
        | Inline::Space
        | Inline::Linebreak
        | Inline::Raw(_)
        | Inline::Math(_)
        | Inline::Verbatim(_)) => out.push(other),
    }
}

/// Diff `style` against `default`, clearing whatever field is redundant.
/// `bold`/`italic`/`underline`/`strike`/`smallcaps`/`script` are left as-is
/// (the doc default is essentially always off for these), except inside a
/// heading where size/weight/font/color are implied by the heading itself.
fn reduce_style(style: &TextStyle, default: &TextStyle, in_heading: bool) -> TextStyle {
    let mut reduced = style.clone();

    if reduced.font == default.font {
        reduced.font = None;
    }
    if size_eq(reduced.size_pt, default.size_pt) {
        reduced.size_pt = None;
    }
    if reduced.color == default.color {
        reduced.color = None;
    }

    if in_heading {
        reduced.size_pt = None;
        reduced.bold = false;
        reduced.font = None;
        reduced.color = None;
    }

    reduced
}

fn size_eq(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() < 1e-3,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdoc::Stmt;

    fn default_doc(default: TextStyle, body: Vec<Block>) -> TypstDoc {
        TypstDoc { preamble: vec![Stmt::SetText(default)], body }
    }

    #[test]
    fn styled_run_matching_default_collapses_to_bare_text() {
        let default = TextStyle {
            font: Some("libertinus serif".into()),
            size_pt: Some(11.0),
            color: Some([0, 0, 0]),
            ..Default::default()
        };
        let mut doc = default_doc(
            default,
            vec![Block::Paragraph {
                style: Default::default(),
                body: vec![Inline::Styled {
                    style: TextStyle {
                        font: Some("libertinus serif".into()),
                        size_pt: Some(11.0),
                        color: Some([0, 0, 0]),
                        ..Default::default()
                    },
                    body: vec![Inline::Text("hello".into())],
                }],
            }],
        );
        run(&mut doc);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(&body[0], Inline::Text(s) if s == "hello"));
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn heading_run_loses_size_bold_font_and_color() {
        let default = TextStyle {
            font: Some("libertinus serif".into()),
            size_pt: Some(11.0),
            color: Some([0, 0, 0]),
            ..Default::default()
        };
        let mut doc = default_doc(
            default,
            vec![Block::Heading {
                level: 1,
                body: vec![Inline::Styled {
                    style: TextStyle {
                        font: Some("libertinus serif".into()),
                        size_pt: Some(24.0),
                        color: Some([0, 0, 0]),
                        bold: true,
                        ..Default::default()
                    },
                    body: vec![Inline::Text("Introduccion".into())],
                }],
            }],
        );
        run(&mut doc);
        match &doc.body[0] {
            Block::Heading { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(&body[0], Inline::Text(s) if s == "Introduccion"));
            }
            _ => panic!("expected heading"),
        }
    }

    #[test]
    fn distinct_color_run_keeps_its_wrapper() {
        let default = TextStyle {
            font: Some("libertinus serif".into()),
            size_pt: Some(11.0),
            color: Some([0, 0, 0]),
            ..Default::default()
        };
        let mut doc = default_doc(
            default,
            vec![Block::Paragraph {
                style: Default::default(),
                body: vec![Inline::Styled {
                    style: TextStyle {
                        font: Some("libertinus serif".into()),
                        size_pt: Some(11.0),
                        color: Some([51, 51, 51]),
                        ..Default::default()
                    },
                    body: vec![Inline::Text("link text".into())],
                }],
            }],
        );
        run(&mut doc);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => {
                assert_eq!(body.len(), 1);
                match &body[0] {
                    Inline::Styled { style, body } => {
                        assert_eq!(style.color, Some([51, 51, 51]));
                        assert!(style.font.is_none());
                        assert!(style.size_pt.is_none());
                        assert!(matches!(&body[0], Inline::Text(s) if s == "link text"));
                    }
                    other => panic!("expected Styled, got {other:?}"),
                }
            }
            _ => panic!("expected paragraph"),
        }
    }
}

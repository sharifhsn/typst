//! Promote bold/italic-only `#text(..)` runs to markup `*strong*`/`_emph_`.
//!
//! Must run after [`crate::passes::collapse_style`], which is what leaves
//! behind [`Inline::Styled`] runs whose only remaining fields are `bold`
//! and/or `italic` — those are exactly the runs this pass rewrites. Anything
//! that still carries a font/size/color/underline/etc. is left alone; it
//! needs a real `#text(..)` call.

use crate::tdoc::{Block, Figure, Inline, Inlines, List, Table, TextStyle, TypstDoc};

/// Entry point: rewrite bold/italic-only styled runs into `Strong`/`Emph`
/// throughout `doc.body`.
pub fn run(doc: &mut TypstDoc) {
    for block in &mut doc.body {
        walk_block(block);
    }
}

fn walk_block(block: &mut Block) {
    match block {
        Block::Heading { body, .. } => promote_in_place(body),
        Block::Paragraph { body, .. } => promote_in_place(body),
        Block::List(List { items }) => {
            for item in items {
                promote_in_place(&mut item.body);
            }
        }
        Block::Table(Table { rows, .. }) => {
            for row in rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.body {
                        walk_block(inner);
                    }
                }
            }
        }
        Block::Figure(Figure { caption, .. }) => {
            if let Some(caption) = caption {
                promote_in_place(caption);
            }
        }
        Block::CodeBlock { .. }
        | Block::Equation { .. }
        | Block::Rule
        | Block::Break(_)
        | Block::Verbatim(_) => {}
    }
}

fn promote_in_place(inlines: &mut Inlines) {
    let taken = std::mem::take(inlines);
    *inlines = promote_inlines(taken);
}

fn promote_inlines(inlines: Inlines) -> Inlines {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        promote_inline(inline, &mut out);
    }
    out
}

fn promote_inline(inline: Inline, out: &mut Inlines) {
    match inline {
        Inline::Strong(body) => out.push(Inline::Strong(promote_inlines(body))),
        Inline::Emph(body) => out.push(Inline::Emph(promote_inlines(body))),
        Inline::Link { dest, body } => {
            out.push(Inline::Link { dest, body: promote_inlines(body) })
        }
        Inline::Styled { style, body } => {
            let body = promote_inlines(body);
            match bold_italic_only(&style) {
                Some((true, true)) => out.push(Inline::Strong(vec![Inline::Emph(body)])),
                Some((true, false)) => out.push(Inline::Strong(body)),
                Some((false, true)) => out.push(Inline::Emph(body)),
                Some((false, false)) | None => out.push(Inline::Styled { style, body }),
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

/// If `style` sets nothing beyond `bold`/`italic`, return `(bold, italic)`.
fn bold_italic_only(style: &TextStyle) -> Option<(bool, bool)> {
    let TextStyle { font, size_pt, color, bold, italic, underline, strike, smallcaps, script } =
        style;
    if font.is_none()
        && size_pt.is_none()
        && color.is_none()
        && !underline
        && !strike
        && !smallcaps
        && script.is_none()
        && (*bold || *italic)
    {
        Some((*bold, *italic))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: Vec<Block>) -> TypstDoc {
        TypstDoc { preamble: vec![], body }
    }

    #[test]
    fn bold_only_becomes_strong() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::Styled {
                style: TextStyle { bold: true, ..Default::default() },
                body: vec![Inline::Text("Site Reliability Engineering".into())],
            }],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[0], Inline::Strong(inner)
                    if matches!(&inner[0], Inline::Text(s) if s == "Site Reliability Engineering")));
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn italic_only_becomes_emph() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::Styled {
                style: TextStyle { italic: true, ..Default::default() },
                body: vec![Inline::Text("emphasis".into())],
            }],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[0], Inline::Emph(_)));
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn bold_and_italic_nests_strong_around_emph() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::Styled {
                style: TextStyle { bold: true, italic: true, ..Default::default() },
                body: vec![Inline::Text("both".into())],
            }],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => match &body[0] {
                Inline::Strong(inner) => assert!(matches!(&inner[0], Inline::Emph(_))),
                other => panic!("expected Strong(Emph(..)), got {other:?}"),
            },
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn styled_run_with_color_is_left_alone() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::Styled {
                style: TextStyle { bold: true, color: Some([51, 51, 51]), ..Default::default() },
                body: vec![Inline::Text("link text".into())],
            }],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[0], Inline::Styled { .. }));
            }
            _ => panic!("expected paragraph"),
        }
    }
}

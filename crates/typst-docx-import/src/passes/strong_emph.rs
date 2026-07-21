//! Promote bold/italic-only `#text(..)` runs to markup `*strong*`/`_emph_`.
//!
//! Must run after [`crate::passes::collapse_style`], which is what leaves
//! behind [`Inline::Styled`] runs whose only remaining fields are `bold`
//! and/or `italic` — those are exactly the runs this pass rewrites. Anything
//! that still carries a font/size/color/underline/etc. is left alone; it
//! needs a real `#text(..)` call.

use crate::tdoc::{
    push_furniture_trees, Block, Chart, ChartContent, Figure, Inline, Inlines, List, Table,
    TextStyle, TypstDoc,
};

/// Entry point: rewrite bold/italic-only styled runs into `Strong`/`Emph`
/// throughout `doc.body`.
pub fn run(doc: &mut TypstDoc) {
    for tree in doc.block_trees_mut() {
        for block in tree {
            walk_block(block);
        }
    }
}

fn walk_block(block: &mut Block) {
    match block {
        Block::Heading { body, .. } => promote_in_place(body),
        Block::Paragraph { body, .. } => promote_in_place(body),
        Block::List(List { items, .. }) => {
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
        // See the matching arms (and their comment) in `collapse_style` — a
        // chart-as-table's cells are plain text today, but get the same walk
        // as `Block::Table`'s cells for the same two reasons; a
        // chart-as-plot has no `Inlines` in it at all.
        Block::Chart(Chart { content: ChartContent::Table(Table { rows, .. }), .. }) => {
            for row in rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.body {
                        walk_block(inner);
                    }
                }
            }
        }
        Block::Chart(Chart { content: ChartContent::Plot(_), .. }) => {}
        // A later section's own content and header/footer furniture — see
        // `collapse_style::walk_block`'s matching arm (and `TypstDoc::
        // block_trees_mut`'s doc comment) for why this has to happen here,
        // inline, rather than via a separate block-tree entry.
        Block::Section(section) => {
            let mut trees = Vec::new();
            push_furniture_trees(&mut section.setup, &mut trees);
            for tree in trees {
                for block in tree {
                    walk_block(block);
                }
            }
            for block in &mut section.body {
                walk_block(block);
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
        // See the matching arm in `collapse_style`: a comment's body is
        // walked as its own block sequence, exactly like a footnote's.
        Inline::Comment(mut anchor) => {
            if let Some(info) = anchor.info.as_mut() {
                for block in &mut info.body {
                    walk_block(block);
                }
            }
            out.push(Inline::Comment(anchor));
        }
        Inline::Strong(body) => out.push(Inline::Strong(promote_inlines(body))),
        Inline::Emph(body) => out.push(Inline::Emph(promote_inlines(body))),
        Inline::Link { dest, body } => {
            out.push(Inline::Link { dest, body: promote_inlines(body) })
        }
        Inline::LabelLink { label, body } => {
            out.push(Inline::LabelLink { label, body: promote_inlines(body) })
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
        // See the matching arm in `collapse_style` for why a footnote's body
        // is walked as its own block sequence rather than inline content.
        Inline::Footnote(mut blocks) => {
            for block in &mut blocks {
                walk_block(block);
            }
            out.push(Inline::Footnote(blocks));
        }
        // Both halves of a ruby are ordinary inline runs.
        Inline::Ruby { base, gloss } => {
            out.push(Inline::Ruby {
                base: promote_inlines(base),
                gloss: promote_inlines(gloss),
            })
        }
        // Same reasoning — a text box's body is its own block sequence too.
        Inline::TextBox(mut blocks) => {
            for block in &mut blocks {
                walk_block(block);
            }
            out.push(Inline::TextBox(blocks));
        }
        // Same reasoning for the text inside a drawn shape.
        Inline::Shape { call, mut body } => {
            for block in &mut body {
                walk_block(block);
            }
            out.push(Inline::Shape { call, body });
        }
        other @ (Inline::Text(_)
        | Inline::Space
        | Inline::Linebreak
        | Inline::Raw(_)
        | Inline::Math(_)
        | Inline::Label(_)
        | Inline::PageRef(_)
        | Inline::Verbatim(_)) => out.push(other),
    }
}

/// If `style` sets nothing beyond `bold`/`italic`, return `(bold, italic)`.
///
/// Destructured exhaustively on purpose: promoting a run to `*..*`/`_.._`
/// throws the rest of the style away, so a newly added [`TextStyle`] field
/// must fail to compile here rather than silently fall through and let the
/// markup swallow formatting it cannot express.
fn bold_italic_only(style: &TextStyle) -> Option<(bool, bool)> {
    let TextStyle {
        font,
        size_pt,
        color,
        bold,
        italic,
        underline,
        strike,
        smallcaps,
        caps,
        script,
        highlight,
        tracking_pt,
        lang,
    } = style;
    if font.is_none()
        && size_pt.is_none()
        && color.is_none()
        && underline.is_none()
        && !strike
        && !smallcaps
        && !caps
        && script.is_none()
        && highlight.is_none()
        && tracking_pt.is_none()
        && lang.is_none()
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

    /// A footnote's body hangs off an inline, not off `TypstDoc::body`
    /// directly, so it only gets tier-2 treatment if the inline walker
    /// descends into it explicitly.
    #[test]
    fn footnote_body_gets_bold_italic_promoted_too() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::Footnote(vec![Block::Paragraph {
                style: Default::default(),
                body: vec![Inline::Styled {
                    style: TextStyle { bold: true, ..Default::default() },
                    body: vec![Inline::Text("snoska".into())],
                }],
            }])],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => match &body[0] {
                Inline::Footnote(blocks) => match &blocks[0] {
                    Block::Paragraph { body, .. } => {
                        assert!(matches!(&body[0], Inline::Strong(_)));
                    }
                    other => panic!("expected paragraph, got {other:?}"),
                },
                other => panic!("expected a footnote, got {other:?}"),
            },
            _ => panic!("expected paragraph"),
        }
    }

    /// Same reasoning — a text box's body hangs off an inline too, and needs
    /// the same explicit descent.
    #[test]
    fn text_box_body_gets_bold_italic_promoted_too() {
        let mut d = doc(vec![Block::Paragraph {
            style: Default::default(),
            body: vec![Inline::TextBox(vec![Block::Paragraph {
                style: Default::default(),
                body: vec![Inline::Styled {
                    style: TextStyle { bold: true, ..Default::default() },
                    body: vec![Inline::Text("boxed".into())],
                }],
            }])],
        }]);
        run(&mut d);
        match &d.body[0] {
            Block::Paragraph { body, .. } => match &body[0] {
                Inline::TextBox(blocks) => match &blocks[0] {
                    Block::Paragraph { body, .. } => {
                        assert!(matches!(&body[0], Inline::Strong(_)));
                    }
                    other => panic!("expected paragraph, got {other:?}"),
                },
                other => panic!("expected a text box, got {other:?}"),
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

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

use crate::tdoc::{
    Block, Chart, ChartContent, Figure, Inline, Inlines, List, Stmt, Table, TextStyle,
    TypstDoc, push_furniture_trees,
};

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
        Block::List(List { items, .. }) => {
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
        // A chart-as-table's cells are built directly as plain `Inline::Text`
        // (see `mappers::chart`), so there's no styling left to collapse —
        // but they still get the same walk as `Block::Table`'s cells above,
        // both to keep this match exhaustive and on the chance a future
        // change gives a cell richer content. A chart-as-plot has no
        // `Inlines` anywhere in it at all — series names and category labels
        // are plain `EcoString`, escaped straight to markup at emit time
        // (see `emit::render_plot`) — so there's nothing to walk.
        Block::Chart(Chart {
            content: ChartContent::Table(Table { rows, .. }), ..
        }) => {
            for row in rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.body {
                        walk_block(inner, default);
                    }
                }
            }
        }
        Block::Chart(Chart { content: ChartContent::Plot(_), .. }) => {}
        // A later section's own content and header/footer furniture — see
        // `TypstDoc::block_trees_mut`'s doc comment for why this pass has to
        // reach both *here*, inline, rather than via a separate block-tree
        // entry (a `Block::Section` lives inside the same tree as everything
        // else in `doc.body`, so there is no second, independent mutable
        // borrow of it to hand out).
        Block::Section(section) => {
            let mut trees = Vec::new();
            push_furniture_trees(&mut section.setup, &mut trees);
            for tree in trees {
                for block in tree {
                    walk_block(block, default);
                }
            }
            for block in &mut section.body {
                walk_block(block, default);
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

fn collapse_inline(
    inline: Inline,
    default: &TextStyle,
    in_heading: bool,
    out: &mut Inlines,
) {
    match inline {
        // A comment's body is ordinary body content, so it gets the same
        // treatment as a footnote's — an annotation should read as idiomatic
        // Typst too, not stay at tier-1 literal formatting just because it
        // happens to live inside a metadata value.
        // A deletion's body is ordinary content, so it gets the same tier-2
        // treatment as a comment's or a footnote's.
        Inline::Revision(mut anchor) => {
            if let Some(info) = anchor.info.as_mut() {
                for block in &mut info.body {
                    walk_block(block, default);
                }
            }
            out.push(Inline::Revision(anchor));
        }
        Inline::Comment(mut anchor) => {
            if let Some(info) = anchor.info.as_mut() {
                for block in &mut info.body {
                    walk_block(block, default);
                }
            }
            out.push(Inline::Comment(anchor));
        }
        Inline::Strong(body) => {
            out.push(Inline::Strong(collapse_inlines(body, default, in_heading)))
        }
        Inline::Emph(body) => {
            out.push(Inline::Emph(collapse_inlines(body, default, in_heading)))
        }
        Inline::LabelLink { label, body } => out.push(Inline::LabelLink {
            label,
            body: collapse_inlines(body, default, in_heading),
        }),
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
        // A footnote's body is a block sequence of its own, not part of the
        // surrounding inline run — collapse it against the same document
        // default, the same way `Block::Table`'s cell bodies are walked
        // below, rather than against whatever `in_heading` happened to be at
        // the reference site (the note's content isn't part of a heading
        // just because its marker sits inside one).
        Inline::Footnote(mut blocks) => {
            for block in &mut blocks {
                walk_block(block, default);
            }
            out.push(Inline::Footnote(blocks));
        }
        // Both halves of a ruby are ordinary inline runs; the gloss is
        // re-sized by the helper, so collapsing its redundant styling here is
        // as safe as anywhere else.
        Inline::Ruby { base, gloss } => out.push(Inline::Ruby {
            base: collapse_inlines(base, default, in_heading),
            gloss: collapse_inlines(gloss, default, in_heading),
        }),
        // A text box's body is a block sequence of its own too — same
        // reasoning as the footnote arm just above.
        Inline::TextBox(mut blocks) => {
            for block in &mut blocks {
                walk_block(block, default);
            }
            out.push(Inline::TextBox(blocks));
        }
        // A shape's own call is a finished expression with no styling to
        // collapse, but the text Word put *inside* the shape is ordinary
        // content and gets the same treatment as a text box's.
        Inline::Shape { call, mut body } => {
            for block in &mut body {
                walk_block(block, default);
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

/// Diff `style` against `default`, clearing whatever field is redundant.
/// The decoration flags (`bold`/`italic`/`underline`/`strike`/`smallcaps`/
/// `caps`/`script`/`highlight`/`rtl`) are left as-is — the doc default is
/// essentially always off for these — except inside a heading where
/// size/weight/font/color are implied by the heading itself.
///
/// `lang` *is* reduced: Word stamps a language onto nearly every run, so
/// leaving it would put a redundant `lang:` argument on all of them.
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
    if reduced.lang == default.lang {
        reduced.lang = None;
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

    /// A footnote's body hangs off an inline, not off `TypstDoc::body`
    /// directly, so it only gets tier-2 treatment if the inline walker
    /// descends into it explicitly.
    #[test]
    fn footnote_body_is_collapsed_like_any_other_block_tree() {
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
                body: vec![Inline::Footnote(vec![Block::Paragraph {
                    style: Default::default(),
                    body: vec![Inline::Styled {
                        style: TextStyle {
                            font: Some("libertinus serif".into()),
                            size_pt: Some(11.0),
                            color: Some([0, 0, 0]),
                            ..Default::default()
                        },
                        body: vec![Inline::Text("snoska".into())],
                    }],
                }])],
            }],
        );
        run(&mut doc);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => match &body[0] {
                Inline::Footnote(blocks) => match &blocks[0] {
                    Block::Paragraph { body, .. } => {
                        assert_eq!(body.len(), 1);
                        assert!(matches!(&body[0], Inline::Text(s) if s == "snoska"));
                    }
                    other => panic!("expected paragraph, got {other:?}"),
                },
                other => panic!("expected a footnote, got {other:?}"),
            },
            _ => panic!("expected paragraph"),
        }
    }

    /// A text box's body hangs off an inline too, and gets the same explicit
    /// descent as a footnote's — see the matching arm in `collapse_inline`.
    #[test]
    fn text_box_body_is_collapsed_like_any_other_block_tree() {
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
                body: vec![Inline::TextBox(vec![Block::Paragraph {
                    style: Default::default(),
                    body: vec![Inline::Styled {
                        style: TextStyle {
                            font: Some("libertinus serif".into()),
                            size_pt: Some(11.0),
                            color: Some([0, 0, 0]),
                            ..Default::default()
                        },
                        body: vec![Inline::Text("boxed".into())],
                    }],
                }])],
            }],
        );
        run(&mut doc);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => match &body[0] {
                Inline::TextBox(blocks) => match &blocks[0] {
                    Block::Paragraph { body, .. } => {
                        assert_eq!(body.len(), 1);
                        assert!(matches!(&body[0], Inline::Text(s) if s == "boxed"));
                    }
                    other => panic!("expected paragraph, got {other:?}"),
                },
                other => panic!("expected a text box, got {other:?}"),
            },
            _ => panic!("expected paragraph"),
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

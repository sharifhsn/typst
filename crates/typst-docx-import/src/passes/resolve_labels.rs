//! Downgrade cross-references whose target label was never emitted.
//!
//! `wml::parse::collect_bookmarks` registers every `w:bookmarkStart` in the
//! document so that a `REF`/`PAGEREF` field or an internal hyperlink can
//! resolve against it — necessarily up front, since a reference routinely
//! appears *earlier* in the document than the bookmark it points at.
//!
//! Not every registered bookmark survives lowering, though. A bookmark inside
//! a `TOC` field's cached result is discarded along with that result (a live
//! `#outline()` replaces it), one on a paragraph with no visible content has
//! no element to attach a label to, and one on a paragraph that lowers to a
//! page break leaves nothing behind either. Referring to a label that isn't in
//! the document is a *hard compile error* in Typst ("label `<x>` does not
//! exist in the document"), which would take the whole import down with it.
//!
//! So this pass runs last, once the tree is final: it collects the labels
//! actually present and rewrites any reference to a missing one into plain
//! content. The link degrades to its own text — exactly what an unresolvable
//! reference already did before bookmarks were imported at all — rather than
//! failing the document.

use rustc_hash::FxHashSet;

use crate::report::ImportReport;
use crate::tdoc::{
    push_furniture_trees, Block, Chart, ChartContent, Figure, Inline, Inlines, List, Section,
    Table, TypstDoc,
};

type Labels = FxHashSet<ecow::EcoString>;

pub fn run(doc: &mut TypstDoc, report: &mut ImportReport) {
    let mut emitted = Labels::default();
    for tree in doc.block_trees_mut() {
        for block in tree.iter() {
            collect_block(block, &mut emitted);
        }
    }

    let mut downgraded = false;
    for tree in doc.block_trees_mut() {
        for block in tree.iter_mut() {
            rewrite_block(block, &emitted, &mut downgraded);
        }
    }

    if downgraded {
        report.approximate(
            "cross-reference",
            "target bookmark did not survive import; reference kept as plain text",
        );
    }
}

// --- Collecting the labels that are actually in the tree ---------------------

fn collect_block(block: &Block, out: &mut Labels) {
    match block {
        Block::Heading { body, .. } | Block::Paragraph { body, .. } => collect_inlines(body, out),
        Block::List(List { items, .. }) => {
            for item in items {
                collect_inlines(&item.body, out);
            }
        }
        Block::Table(Table { rows, .. })
        | Block::Chart(Chart { content: ChartContent::Table(Table { rows, .. }), .. }) => {
            for row in rows {
                for cell in &row.cells {
                    for inner in &cell.body {
                        collect_block(inner, out);
                    }
                }
            }
        }
        Block::Figure(Figure { caption, .. }) => {
            if let Some(caption) = caption {
                collect_inlines(caption, out);
            }
        }
        Block::Section(Section { body, .. }) => {
            for inner in body {
                collect_block(inner, out);
            }
        }
        Block::Chart(_)
        | Block::CodeBlock { .. }
        | Block::Equation { .. }
        | Block::Rule
        | Block::Break(_)
        | Block::Verbatim(_) => {}
    }
}

fn collect_inlines(inlines: &Inlines, out: &mut Labels) {
    for inline in inlines {
        match inline {
            Inline::Label(name) => {
                out.insert(name.clone());
            }
            // A comment anchor emits its label on a `#metadata` element of
            // its own, so the name is just as much "defined" as a bookmark's.
            Inline::Comment(anchor) => {
                out.insert(anchor.label.clone());
            }
            Inline::Revision(anchor) => {
                out.insert(anchor.label.clone());
            }
            Inline::Strong(body)
            | Inline::Emph(body)
            | Inline::Link { body, .. }
            | Inline::LabelLink { body, .. }
            | Inline::Styled { body, .. } => collect_inlines(body, out),
            Inline::Ruby { base, gloss } => {
                collect_inlines(base, out);
                collect_inlines(gloss, out);
            }
            Inline::Footnote(blocks)
            | Inline::TextBox(blocks)
            | Inline::Shape { body: blocks, .. } => {
                for block in blocks {
                    collect_block(block, out);
                }
            }
            Inline::Text(_)
            | Inline::Space
            | Inline::Linebreak
            | Inline::Raw(_)
            | Inline::Math(_)
            | Inline::PageRef(_)
            | Inline::Verbatim(_) => {}
        }
    }
}

// --- Rewriting references to labels that aren't there ------------------------

fn rewrite_block(block: &mut Block, emitted: &Labels, downgraded: &mut bool) {
    match block {
        Block::Heading { body, .. } | Block::Paragraph { body, .. } => {
            rewrite_in_place(body, emitted, downgraded)
        }
        Block::List(List { items, .. }) => {
            for item in items {
                rewrite_in_place(&mut item.body, emitted, downgraded);
            }
        }
        Block::Table(Table { rows, .. })
        | Block::Chart(Chart { content: ChartContent::Table(Table { rows, .. }), .. }) => {
            for row in rows {
                for cell in &mut row.cells {
                    for inner in &mut cell.body {
                        rewrite_block(inner, emitted, downgraded);
                    }
                }
            }
        }
        Block::Figure(Figure { caption, .. }) => {
            if let Some(caption) = caption {
                rewrite_in_place(caption, emitted, downgraded);
            }
        }
        Block::Section(section) => {
            let mut trees = Vec::new();
            push_furniture_trees(&mut section.setup, &mut trees);
            for tree in trees {
                for block in tree {
                    rewrite_block(block, emitted, downgraded);
                }
            }
            for inner in &mut section.body {
                rewrite_block(inner, emitted, downgraded);
            }
        }
        Block::Chart(_)
        | Block::CodeBlock { .. }
        | Block::Equation { .. }
        | Block::Rule
        | Block::Break(_)
        | Block::Verbatim(_) => {}
    }
}

fn rewrite_in_place(inlines: &mut Inlines, emitted: &Labels, downgraded: &mut bool) {
    let taken = std::mem::take(inlines);
    *inlines = rewrite_inlines(taken, emitted, downgraded);
}

fn rewrite_inlines(inlines: Inlines, emitted: &Labels, downgraded: &mut bool) -> Inlines {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            // The jump target is gone: keep the text, drop the link.
            Inline::LabelLink { label, body } if !emitted.contains(&label) => {
                *downgraded = true;
                out.extend(rewrite_inlines(body, emitted, downgraded));
            }
            // A page number for a bookmark that isn't there has nothing to
            // compute, so it goes entirely — there is no text to salvage.
            Inline::PageRef(label) if !emitted.contains(&label) => *downgraded = true,
            Inline::LabelLink { label, body } => out.push(Inline::LabelLink {
                label,
                body: rewrite_inlines(body, emitted, downgraded),
            }),
            Inline::Strong(body) => {
                out.push(Inline::Strong(rewrite_inlines(body, emitted, downgraded)))
            }
            Inline::Emph(body) => {
                out.push(Inline::Emph(rewrite_inlines(body, emitted, downgraded)))
            }
            Inline::Link { dest, body } => out
                .push(Inline::Link { dest, body: rewrite_inlines(body, emitted, downgraded) }),
            Inline::Styled { style, body } => out
                .push(Inline::Styled { style, body: rewrite_inlines(body, emitted, downgraded) }),
            Inline::Ruby { base, gloss } => out.push(Inline::Ruby {
                base: rewrite_inlines(base, emitted, downgraded),
                gloss: rewrite_inlines(gloss, emitted, downgraded),
            }),
            Inline::Footnote(mut blocks) => {
                for block in &mut blocks {
                    rewrite_block(block, emitted, downgraded);
                }
                out.push(Inline::Footnote(blocks));
            }
            Inline::TextBox(mut blocks) => {
                for block in &mut blocks {
                    rewrite_block(block, emitted, downgraded);
                }
                out.push(Inline::TextBox(blocks));
            }
            Inline::Shape { call, mut body } => {
                for block in &mut body {
                    rewrite_block(block, emitted, downgraded);
                }
                out.push(Inline::Shape { call, body });
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdoc::ParStyle;

    fn para(body: Inlines) -> Block {
        Block::Paragraph { style: ParStyle::default(), body }
    }

    /// Referring to a label that never made it into the document is a hard
    /// Typst compile error, so the reference must degrade to its own text.
    #[test]
    fn a_reference_to_a_missing_label_keeps_its_text() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![para(vec![Inline::LabelLink {
                label: "gone".into(),
                body: vec![Inline::Text("see above".into())],
            }])],
        };
        let mut report = ImportReport::default();
        run(&mut doc, &mut report);

        match &doc.body[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "see above"), "{body:?}");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "cross-reference");
    }

    /// A reference whose label *is* present must be left exactly as it was.
    #[test]
    fn a_reference_to_a_present_label_survives() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                para(vec![Inline::Text("Intro".into()), Inline::Label("here".into())]),
                para(vec![Inline::LabelLink {
                    label: "here".into(),
                    body: vec![Inline::Text("see above".into())],
                }]),
            ],
        };
        let mut report = ImportReport::default();
        run(&mut doc, &mut report);

        match &doc.body[1] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[..], [Inline::LabelLink { .. }]), "{body:?}");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
        assert!(report.notes.is_empty());
    }

    /// A dangling page reference has no text to fall back to, so it goes.
    #[test]
    fn a_page_reference_to_a_missing_label_is_dropped() {
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![para(vec![
                Inline::Text("page ".into()),
                Inline::PageRef("gone".into()),
            ])],
        };
        let mut report = ImportReport::default();
        run(&mut doc, &mut report);

        match &doc.body[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "page "), "{body:?}");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    /// A label emitted inside a table cell still counts as present.
    #[test]
    fn a_label_nested_in_a_table_cell_is_found() {
        use crate::tdoc::{TableCell, TableRow};
        let cell = TableCell {
            body: vec![para(vec![Inline::Label("deep".into())])],
            ..TableCell::empty()
        };
        let mut doc = TypstDoc {
            preamble: vec![],
            body: vec![
                Block::Table(Table {
                    align: None,
                    indent_pt: None,
                    stroke: None,
                    row_heights: Vec::new(),
                    columns: 1,
                    column_widths: vec![None],
                    rows: vec![TableRow { header: false, cells: vec![cell] }],
                }),
                para(vec![Inline::PageRef("deep".into())]),
            ],
        };
        let mut report = ImportReport::default();
        run(&mut doc, &mut report);

        match &doc.body[1] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[..], [Inline::PageRef(_)]), "{body:?}");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }
}

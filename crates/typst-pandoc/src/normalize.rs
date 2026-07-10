//! Post-lowering normalization for the Pandoc AST.
//!
//! The converter first builds the target AST from realized Typst content. This
//! module then performs whole-document passes that require a complete tree:
//! recovering structured citations from bibliography links and demoting
//! internal links whose anchors are absent from the emitted AST.

use std::collections::{HashMap, HashSet};

use ecow::EcoString;

use crate::ast::{Block, Inline};

/// Applies every whole-document normalization pass in dependency order.
pub(crate) fn run(blocks: &mut [Block], cite_anchors: &HashMap<EcoString, EcoString>) {
    // Recover structured `Cite` nodes before pruning: the wrapped citation link
    // targets a bibliography-entry anchor and must remain available as fallback.
    structure_cites(blocks, cite_anchors);
    prune_dangling_links(blocks);
}

/// Rewrites every realized in-text citation `Link` into a structured
/// `Inline::Cite` so that `pandoc --citeproc` can re-resolve it against the
/// synthesized `.bib` sidecar. A citation arrives as a `Link` whose `#`-fragment
/// URL is the bibliography entry's backlink anchor; `cite_anchors` maps that
/// anchor to the cite key. The original `Link` is kept inside the `Cite` as the
/// baked fallback, so non-`--citeproc` output is unchanged. A no-op when the map
/// is empty (no bibliography).
fn structure_cites(blocks: &mut [Block], anchors: &HashMap<EcoString, EcoString>) {
    if anchors.is_empty() {
        return;
    }
    for block in blocks {
        structure_cites_block(block, anchors);
    }
}

/// Recurses into a block, rewriting cite links within every inline it carries.
fn structure_cites_block(block: &mut Block, anchors: &HashMap<EcoString, EcoString>) {
    match block {
        Block::Plain(inl) | Block::Para(inl) => structure_cites_inlines(inl, anchors),
        Block::Header(_, _, inl) => structure_cites_inlines(inl, anchors),
        Block::Div(_, bs) | Block::BlockQuote(bs) => {
            for b in bs {
                structure_cites_block(b, anchors);
            }
        }
        Block::BulletList(items) | Block::OrderedList(_, items) => {
            for item in items {
                for b in item {
                    structure_cites_block(b, anchors);
                }
            }
        }
        Block::DefinitionList(items) => {
            for (term, defs) in items {
                structure_cites_inlines(term, anchors);
                for def in defs {
                    for b in def {
                        structure_cites_block(b, anchors);
                    }
                }
            }
        }
        Block::Figure(_, cap, bs) => {
            for b in &mut cap.1 {
                structure_cites_block(b, anchors);
            }
            for b in bs {
                structure_cites_block(b, anchors);
            }
        }
        Block::Table(_, _, _, head, bodies, foot) => {
            structure_cites_rows(&mut head.1, anchors);
            for body in bodies.iter_mut() {
                structure_cites_rows(&mut body.2, anchors);
                structure_cites_rows(&mut body.3, anchors);
            }
            structure_cites_rows(&mut foot.1, anchors);
        }
        Block::CodeBlock(..) | Block::RawBlock(..) | Block::HorizontalRule => {}
    }
}

/// Rewrites cite links inside every cell of a table row list.
fn structure_cites_rows(
    rows: &mut [crate::ast::Row],
    anchors: &HashMap<EcoString, EcoString>,
) {
    for row in rows {
        for cell in &mut row.1 {
            for block in &mut cell.4 {
                structure_cites_block(block, anchors);
            }
        }
    }
}

fn structure_cites_inlines(
    inlines: &mut [Inline],
    anchors: &HashMap<EcoString, EcoString>,
) {
    use crate::ast::{Citation, CitationMode};

    for node in inlines {
        // Recurse into containers first so nested cites are handled.
        match node {
            Inline::Emph(v)
            | Inline::Strong(v)
            | Inline::Underline(v)
            | Inline::Strikeout(v)
            | Inline::Superscript(v)
            | Inline::Subscript(v)
            | Inline::SmallCaps(v)
            | Inline::Quoted(_, v)
            | Inline::Span(_, v)
            | Inline::Cite(_, v) => structure_cites_inlines(v, anchors),
            Inline::Note(bs) => {
                for b in bs {
                    structure_cites_block(b, anchors);
                }
            }
            _ => {}
        }

        // Then, if this node is a bibliographic cite link, wrap it in a `Cite`.
        if let Inline::Link(_, body, (url, _)) = node {
            structure_cites_inlines(body, anchors);
            if let Some(anchor) = url.strip_prefix('#')
                && let Some(key) = anchors.get(anchor)
            {
                let citation = Citation {
                    id: key.to_string(),
                    prefix: Vec::new(),
                    suffix: Vec::new(),
                    mode: CitationMode::NormalCitation,
                    note_num: 0,
                    hash: 0,
                };
                // Keep the original link as the fallback so non-citeproc output
                // is unchanged (clickable `[1]` that still jumps to the entry).
                let original = std::mem::replace(node, Inline::Space);
                *node = Inline::Cite(vec![citation], vec![original]);
            }
        }
    }
}

/// Removes the `Link` wrapper from any internal (`#anchor`) link whose target
/// anchor does not exist anywhere in the document, replacing the `Link` with its
/// bare body inlines (the text is always kept; only the dead jump is dropped).
///
/// This is the single invariant that guarantees **no internal link ever
/// dangles** — load-bearing for citations. Typst emits some `Destination::
/// Location` jumps that have no representable anchor in the Pandoc AST:
/// - a bibliography entry's `[1]` prefix back-links to the *citation site*
///   (`links_to_citations`), but an in-text citation is plain realized text with
///   no anchor of its own;
/// - a `#link(<lbl>)` / `@ref` to a target that rasterized away (its anchor lives
///   only inside the image) or to a page-positioned location.
///
/// Rather than special-casing each producer, we resolve the id namespace once,
/// globally, after the whole tree is built — exactly the set a reader would see —
/// and demote every internal link that points outside it. External (`http(s)://`,
/// `mailto:`, …) links and resolved internal links are untouched.
fn prune_dangling_links(blocks: &mut [Block]) {
    let mut ids = HashSet::new();
    for block in blocks.iter() {
        collect_ids_block(block, &mut ids);
    }
    for block in blocks {
        prune_block(block, &ids);
    }
}

/// Collects every `Attr` identifier reachable in a block (the anchor namespace).
fn collect_ids_block(block: &Block, ids: &mut HashSet<String>) {
    let push = |attr: &crate::ast::Attr, ids: &mut HashSet<String>| {
        if !attr.0.is_empty() {
            ids.insert(attr.0.clone());
        }
    };
    match block {
        Block::Plain(inl) | Block::Para(inl) => {
            for i in inl {
                collect_ids_inline(i, ids);
            }
        }
        Block::Header(_, attr, inl) => {
            push(attr, ids);
            for i in inl {
                collect_ids_inline(i, ids);
            }
        }
        Block::Div(attr, bs) => {
            push(attr, ids);
            for b in bs {
                collect_ids_block(b, ids);
            }
        }
        Block::CodeBlock(attr, _) => push(attr, ids),
        Block::BlockQuote(bs) => {
            for b in bs {
                collect_ids_block(b, ids);
            }
        }
        Block::BulletList(items) | Block::OrderedList(_, items) => {
            for item in items {
                for b in item {
                    collect_ids_block(b, ids);
                }
            }
        }
        Block::DefinitionList(items) => {
            for (term, defs) in items {
                for i in term {
                    collect_ids_inline(i, ids);
                }
                for def in defs {
                    for b in def {
                        collect_ids_block(b, ids);
                    }
                }
            }
        }
        Block::Figure(attr, cap, bs) => {
            push(attr, ids);
            for b in &cap.1 {
                collect_ids_block(b, ids);
            }
            for b in bs {
                collect_ids_block(b, ids);
            }
        }
        Block::Table(attr, ..) => push(attr, ids),
        Block::HorizontalRule | Block::RawBlock(..) => {}
    }
}

/// Collects identifiers carried inline (spans/code/images, plus nested bodies).
fn collect_ids_inline(inline: &Inline, ids: &mut HashSet<String>) {
    let push = |attr: &crate::ast::Attr, ids: &mut HashSet<String>| {
        if !attr.0.is_empty() {
            ids.insert(attr.0.clone());
        }
    };
    match inline {
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Underline(v)
        | Inline::Strikeout(v)
        | Inline::Superscript(v)
        | Inline::Subscript(v)
        | Inline::SmallCaps(v)
        | Inline::Quoted(_, v) => {
            for i in v {
                collect_ids_inline(i, ids);
            }
        }
        Inline::Span(attr, v) => {
            push(attr, ids);
            for i in v {
                collect_ids_inline(i, ids);
            }
        }
        Inline::Code(attr, _) | Inline::Image(attr, ..) => push(attr, ids),
        Inline::Link(attr, v, _) => {
            push(attr, ids);
            for i in v {
                collect_ids_inline(i, ids);
            }
        }
        Inline::Cite(_, v) => {
            for i in v {
                collect_ids_inline(i, ids);
            }
        }
        Inline::Note(bs) => {
            for b in bs {
                collect_ids_block(b, ids);
            }
        }
        _ => {}
    }
}

/// Demotes dangling internal links to bare inlines, recursively, within a block.
fn prune_block(block: &mut Block, ids: &HashSet<String>) {
    match block {
        Block::Plain(inl) | Block::Para(inl) => prune_inlines(inl, ids),
        Block::Header(_, _, inl) => prune_inlines(inl, ids),
        Block::Div(_, bs) | Block::BlockQuote(bs) => {
            for b in bs {
                prune_block(b, ids);
            }
        }
        Block::BulletList(items) | Block::OrderedList(_, items) => {
            for item in items {
                for b in item {
                    prune_block(b, ids);
                }
            }
        }
        Block::DefinitionList(items) => {
            for (term, defs) in items {
                prune_inlines(term, ids);
                for def in defs {
                    for b in def {
                        prune_block(b, ids);
                    }
                }
            }
        }
        Block::Figure(_, cap, bs) => {
            for b in &mut cap.1 {
                prune_block(b, ids);
            }
            for b in bs {
                prune_block(b, ids);
            }
        }
        Block::CodeBlock(..)
        | Block::RawBlock(..)
        | Block::HorizontalRule
        | Block::Table(..) => {}
    }
}

/// Demotes dangling internal links within an inline list, in place.
fn prune_inlines(inlines: &mut Vec<Inline>, ids: &HashSet<String>) {
    let mut out = Vec::with_capacity(inlines.len());
    for mut node in inlines.drain(..) {
        prune_inline(&mut node, ids);
        match node {
            // An internal `#anchor` link with no matching anchor: drop the dead
            // jump, keep the body text.
            Inline::Link(_, body, (url, _))
                if url.starts_with('#') && !ids.contains(&url[1..]) =>
            {
                out.extend(body);
            }
            other => out.push(other),
        }
    }
    *inlines = out;
    crate::convert::coalesce_inlines(inlines);
}

/// Recurses into an inline node's children, pruning dangling links.
fn prune_inline(inline: &mut Inline, ids: &HashSet<String>) {
    match inline {
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Underline(v)
        | Inline::Strikeout(v)
        | Inline::Superscript(v)
        | Inline::Subscript(v)
        | Inline::SmallCaps(v)
        | Inline::Quoted(_, v)
        | Inline::Span(_, v)
        | Inline::Link(_, v, _)
        | Inline::Cite(_, v) => prune_inlines(v, ids),
        Inline::Note(bs) => {
            for b in bs {
                prune_block(b, ids);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{prune_dangling_links, structure_cites};
    use crate::ast::{Block, Inline, empty_attr, id_attr};

    /// An internal `#anchor` link that resolves to a real anchor (a `BibEntry`
    /// `Div` with that id) is preserved; one pointing nowhere is demoted to its
    /// bare body text. This is the no-dangling-cites invariant.
    #[test]
    fn prunes_only_dangling_internal_links() {
        let live = ("#ref-live".to_string(), String::new());
        let dead = ("#ref-dead".to_string(), String::new());
        let ext = ("https://example.com".to_string(), String::new());
        let mut blocks = vec![
            Block::Para(vec![
                Inline::Link(empty_attr(), vec![Inline::Str("[1]".into())], live),
                Inline::Link(empty_attr(), vec![Inline::Str("[2]".into())], dead),
                Inline::Link(empty_attr(), vec![Inline::Str("site".into())], ext),
            ]),
            Block::Div(
                id_attr("ref-live"),
                vec![Block::Para(vec![Inline::Str("Author, Title.".into())])],
            ),
        ];
        prune_dangling_links(&mut blocks);
        let Block::Para(inl) = &blocks[0] else { panic!() };
        let links: Vec<_> = inl
            .iter()
            .filter_map(|i| match i {
                Inline::Link(_, _, (u, _)) => Some(u.clone()),
                _ => None,
            })
            .collect();
        assert!(links.contains(&"#ref-live".to_string()), "live anchor kept");
        assert!(links.contains(&"https://example.com".to_string()), "external kept");
        assert!(!links.iter().any(|u| u == "#ref-dead"), "dangling link removed");
        let text: String = inl
            .iter()
            .filter_map(|i| match i {
                Inline::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(text.contains("[2]"), "demoted link keeps its text");
    }

    /// A realized in-text citation `Link` whose `#anchor` matches a bibliography
    /// entry anchor is rewritten into a structured `Inline::Cite` carrying the
    /// cite key, with the original `Link` kept as the baked fallback body.
    #[test]
    fn structure_cites_wraps_matching_link() {
        let mut anchors = HashMap::new();
        anchors.insert("ref-abc".into(), "smith21".into());
        let mut blocks = vec![Block::Para(vec![Inline::Link(
            empty_attr(),
            vec![Inline::Str("[1]".into())],
            ("#ref-abc".into(), String::new()),
        )])];
        structure_cites(&mut blocks, &anchors);
        let Block::Para(inl) = &blocks[0] else { panic!() };
        match &inl[0] {
            Inline::Cite(cites, fallback) => {
                assert_eq!(cites.len(), 1);
                assert_eq!(cites[0].id, "smith21");
                assert!(matches!(fallback[0], Inline::Link(..)));
            }
            _ => panic!("expected a Cite"),
        }
    }

    /// An empty anchor map (no bibliography) leaves all links untouched.
    #[test]
    fn structure_cites_noop_without_bibliography() {
        let anchors = HashMap::new();
        let mut blocks = vec![Block::Para(vec![Inline::Link(
            empty_attr(),
            vec![Inline::Str("[1]".into())],
            ("#ref-abc".into(), String::new()),
        )])];
        structure_cites(&mut blocks, &anchors);
        let Block::Para(inl) = &blocks[0] else { panic!() };
        assert!(matches!(inl[0], Inline::Link(..)), "link is untouched");
    }
}

//! Post-lowering normalization for the Pandoc AST.
//!
//! The converter first builds the target AST from realized Typst content. This
//! module then performs the whole-document passes that need a complete tree:
//! settling the anchor namespace — keeping the synthesized anchors some link
//! needs, deleting the rest, and demoting the links that still resolve nowhere
//! — and recovering structured citations from bibliography links.

use std::collections::{HashMap, HashSet};

use ecow::EcoString;

use crate::ast::{Block, Inline};

/// Applies every whole-document normalization pass in dependency order.
pub(crate) fn run(
    blocks: &mut Vec<Block>,
    cite_anchors: &HashMap<EcoString, EcoString>,
) {
    // Settle the anchor namespace first, because it re-coalesces every inline
    // list it touches: one realized citation arrives as several runs, each in
    // its own `Link`, and a synthesized anchor landing between two of them would
    // keep them from merging — leaving `structure_cites` to make two `Cite`s out
    // of one citation.
    //
    // That this also demotes dangling links before cites are structured is safe:
    // an in-text cite targets its bibliography entry's anchor, which the citation
    // mapper always emits, so a cite link is never among the demoted. What does
    // dangle is the entry's own `[1]` back-link to the citation site, which
    // `cite_anchors` does not key on and `structure_cites` would never have
    // wrapped.
    resolve_anchors(blocks);
    structure_cites(blocks, cite_anchors);
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

/// Settles the document's anchor namespace, so that **no internal link ever
/// dangles** — the one guarantee this module exists for.
///
/// Both halves of that guarantee need the same whole-document view, which is
/// exactly why they are decided here and not per construct: an element type
/// nobody anticipated would otherwise start dangling silently.
///
/// * A **synthesized anchor** — the empty, [`crate::ast::ANCHOR_CLASS`]-marked
///   `Span`/`Div`
///   [`crate::convert`] drops at every introspection tag — is kept when some
///   internal link targets its id and no real node already carries that id. It
///   then loses the marker class and becomes a plain id-only node, which is how
///   pandoc itself spells an anchor (`\phantomsection\label{…}`,
///   `<span id="…">`). Every other one is deleted, so the overwhelming majority
///   leave no trace at all. This is what keeps a `#ref` to a `show`-ruled
///   heading (whose `HeadingElem` the rule replaced) or to a label sitting on a
///   bare text run pointing at something real.
///
/// * An **internal link** whose target ends up in neither set loses its `Link`
///   wrapper and keeps only its body text. Some `Destination::Location` jumps
///   have no anchorable element behind them at all: a bibliography entry's `[1]`
///   prefix back-links to the *citation site*, which is plain realized text, and
///   `#link(here())` targets a bare position rather than an element. There is
///   nothing to anchor for those, and in every format pandoc goes on to render a
///   link that resolves nowhere is worse than plain text — so the jump goes and
///   the text stays.
fn resolve_anchors(blocks: &mut Vec<Block>) {
    let mut ns = Namespace::default();
    collect_blocks(blocks, &mut ns);

    // A synthesized anchor earns its place only when a link needs it and no real
    // node already offers the id.
    let keep: HashSet<String> = ns
        .refs
        .iter()
        .filter(|id| ns.anchors.contains(*id) && !ns.ids.contains(*id))
        .cloned()
        .collect();

    let mut live = ns.ids;
    live.extend(keep.iter().cloned());

    // One id can be owed by several tags — repeated content realizes the same
    // semantic identity more than once. Only the first occurrence may carry it;
    // a duplicate id is its own defect, and a reader resolves to the first
    // anyway.
    let mut bound = HashSet::new();
    rewrite_blocks(blocks, &live, &keep, &mut bound);
}

/// The id namespace of a finished document, gathered in a single pass.
#[derive(Default)]
struct Namespace {
    /// Ids carried by real nodes — everything an internal link resolves to today.
    ids: HashSet<String>,
    /// Ids offered by synthesized anchors, which may or may not be kept.
    anchors: HashSet<String>,
    /// The `#`-fragment target of every internal link in the document.
    refs: HashSet<String>,
}

impl Namespace {
    /// Files an `Attr` under [`Self::ids`] or [`Self::anchors`].
    fn attr(&mut self, attr: &crate::ast::Attr) {
        if attr.0.is_empty() {
            return;
        }
        if crate::ast::is_anchor_attr(attr) {
            self.anchors.insert(attr.0.clone());
        } else {
            self.ids.insert(attr.0.clone());
        }
    }

    /// Files a link target, keeping only internal (`#`-fragment) ones.
    fn target(&mut self, target: &crate::ast::Target) {
        if let Some(id) = target.0.strip_prefix('#') {
            self.refs.insert(id.to_string());
        }
    }
}

/// Collects the namespace of a block list.
fn collect_blocks(blocks: &[Block], ns: &mut Namespace) {
    for block in blocks {
        collect_block(block, ns);
    }
}

/// Collects the namespace reachable from one block. Every id-bearing and
/// link-bearing position must be visited: a target this misses reads as
/// "declared nowhere" and would have a live link demoted out from under it.
fn collect_block(block: &Block, ns: &mut Namespace) {
    match block {
        Block::Plain(inl) | Block::Para(inl) => collect_inlines(inl, ns),
        Block::Header(_, attr, inl) => {
            ns.attr(attr);
            collect_inlines(inl, ns);
        }
        Block::Div(attr, bs) => {
            ns.attr(attr);
            collect_blocks(bs, ns);
        }
        Block::CodeBlock(attr, _) => ns.attr(attr),
        Block::BlockQuote(bs) => collect_blocks(bs, ns),
        Block::BulletList(items) | Block::OrderedList(_, items) => {
            for item in items {
                collect_blocks(item, ns);
            }
        }
        Block::DefinitionList(items) => {
            for (term, defs) in items {
                collect_inlines(term, ns);
                for def in defs {
                    collect_blocks(def, ns);
                }
            }
        }
        Block::Figure(attr, cap, bs) => {
            ns.attr(attr);
            collect_caption(cap, ns);
            collect_blocks(bs, ns);
        }
        Block::Table(attr, cap, _, head, bodies, foot) => {
            ns.attr(attr);
            collect_caption(cap, ns);
            ns.attr(&head.0);
            collect_rows(&head.1, ns);
            for body in bodies {
                ns.attr(&body.0);
                collect_rows(&body.2, ns);
                collect_rows(&body.3, ns);
            }
            ns.attr(&foot.0);
            collect_rows(&foot.1, ns);
        }
        Block::HorizontalRule | Block::RawBlock(..) => {}
    }
}

/// Collects the namespace of a caption (short caption inlines + body blocks).
fn collect_caption(caption: &crate::ast::Caption, ns: &mut Namespace) {
    if let Some(short) = &caption.0 {
        collect_inlines(short, ns);
    }
    collect_blocks(&caption.1, ns);
}

/// Collects the namespace of a table row list, cell attributes included.
fn collect_rows(rows: &[crate::ast::Row], ns: &mut Namespace) {
    for row in rows {
        ns.attr(&row.0);
        for cell in &row.1 {
            ns.attr(&cell.0);
            collect_blocks(&cell.4, ns);
        }
    }
}

/// Collects the namespace of an inline list.
fn collect_inlines(inlines: &[Inline], ns: &mut Namespace) {
    for inline in inlines {
        collect_inline(inline, ns);
    }
}

/// Collects the namespace reachable from one inline node.
fn collect_inline(inline: &Inline, ns: &mut Namespace) {
    match inline {
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Underline(v)
        | Inline::Strikeout(v)
        | Inline::Superscript(v)
        | Inline::Subscript(v)
        | Inline::SmallCaps(v)
        | Inline::Quoted(_, v)
        | Inline::Cite(_, v) => collect_inlines(v, ns),
        Inline::Span(attr, v) => {
            ns.attr(attr);
            collect_inlines(v, ns);
        }
        Inline::Code(attr, _) => ns.attr(attr),
        Inline::Image(attr, alt, target) => {
            ns.attr(attr);
            collect_inlines(alt, ns);
            ns.target(target);
        }
        Inline::Link(attr, v, target) => {
            ns.attr(attr);
            collect_inlines(v, ns);
            ns.target(target);
        }
        Inline::Note(bs) => collect_blocks(bs, ns),
        _ => {}
    }
}

/// Rewrites a block list against the settled namespace: synthesized anchors are
/// kept-and-stripped or removed, dangling links are demoted, and a paragraph
/// left empty by a removal goes with it.
fn rewrite_blocks(
    blocks: &mut Vec<Block>,
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
    let mut out = Vec::with_capacity(blocks.len());
    for mut block in blocks.drain(..) {
        // Whether this block was *already* an empty paragraph before we touched
        // it: only one emptied by a removal here may be dropped.
        let was_empty =
            matches!(&block, Block::Para(v) | Block::Plain(v) if v.is_empty());
        rewrite_block(&mut block, live, keep, bound);
        match block {
            Block::Div(attr, body)
                if crate::ast::is_anchor_attr(&attr) && body.is_empty() =>
            {
                if keep.contains(&attr.0) && bound.insert(attr.0.clone()) {
                    // `Plain [Span]`, not the `Div` the marker used: pandoc's
                    // LaTeX writer treats *any* `Div` whose id begins `ref-` as a
                    // bibliography entry and writes `\bibitem` for it — which,
                    // outside a reference list, is broken LaTeX. Our unlabelled
                    // ids all begin `ref-<hash>`, so a block anchor must not be a
                    // `Div`. A `Span` is exempt and is how pandoc spells a
                    // standalone anchor (`[]{#id}`) anyway.
                    out.push(Block::Plain(vec![Inline::Span(
                        crate::ast::id_attr(attr.0),
                        Vec::new(),
                    )]));
                }
            }
            Block::Para(v) | Block::Plain(v) if v.is_empty() && !was_empty => {}
            other => out.push(other),
        }
    }
    *blocks = out;
}

/// Rewrites one block's children in place.
fn rewrite_block(
    block: &mut Block,
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
    match block {
        Block::Plain(inl) | Block::Para(inl) => rewrite_inlines(inl, live, keep, bound),
        Block::Header(_, _, inl) => rewrite_inlines(inl, live, keep, bound),
        Block::Div(_, bs) | Block::BlockQuote(bs) => {
            rewrite_blocks(bs, live, keep, bound)
        }
        Block::BulletList(items) | Block::OrderedList(_, items) => {
            for item in items {
                rewrite_blocks(item, live, keep, bound);
            }
        }
        Block::DefinitionList(items) => {
            for (term, defs) in items {
                rewrite_inlines(term, live, keep, bound);
                for def in defs {
                    rewrite_blocks(def, live, keep, bound);
                }
            }
        }
        Block::Figure(_, cap, bs) => {
            rewrite_caption(cap, live, keep, bound);
            rewrite_blocks(bs, live, keep, bound);
        }
        Block::Table(_, cap, _, head, bodies, foot) => {
            rewrite_caption(cap, live, keep, bound);
            rewrite_rows(&mut head.1, live, keep, bound);
            for body in bodies.iter_mut() {
                rewrite_rows(&mut body.2, live, keep, bound);
                rewrite_rows(&mut body.3, live, keep, bound);
            }
            rewrite_rows(&mut foot.1, live, keep, bound);
        }
        Block::CodeBlock(..) | Block::RawBlock(..) | Block::HorizontalRule => {}
    }
}

/// Rewrites a caption's short-caption inlines and body blocks.
fn rewrite_caption(
    caption: &mut crate::ast::Caption,
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
    if let Some(short) = &mut caption.0 {
        rewrite_inlines(short, live, keep, bound);
    }
    rewrite_blocks(&mut caption.1, live, keep, bound);
}

/// Rewrites every cell of a table row list.
fn rewrite_rows(
    rows: &mut [crate::ast::Row],
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
    for row in rows {
        for cell in &mut row.1 {
            rewrite_blocks(&mut cell.4, live, keep, bound);
        }
    }
}

/// Rewrites an inline list in place, then re-coalesces it — removing a node can
/// reunite two `Str`s or two halves of one `Link` that it had been separating.
fn rewrite_inlines(
    inlines: &mut Vec<Inline>,
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
    let mut out = Vec::with_capacity(inlines.len());
    for mut node in inlines.drain(..) {
        rewrite_inline(&mut node, live, keep, bound);
        match node {
            // A synthesized anchor: keep it (as a plain id) only if a link needs
            // it and no earlier node has already claimed the id.
            Inline::Span(attr, body)
                if crate::ast::is_anchor_attr(&attr) && body.is_empty() =>
            {
                if keep.contains(&attr.0) && bound.insert(attr.0.clone()) {
                    out.push(Inline::Span(crate::ast::id_attr(attr.0), Vec::new()));
                }
            }
            // An internal `#anchor` link with nothing to land on: drop the dead
            // jump, keep the body text.
            Inline::Link(_, body, (url, _))
                if url.starts_with('#') && !live.contains(&url[1..]) =>
            {
                out.extend(body);
            }
            other => out.push(other),
        }
    }
    *inlines = out;
    crate::convert::coalesce_inlines(inlines);
}

/// Rewrites an inline node's children.
fn rewrite_inline(
    inline: &mut Inline,
    live: &HashSet<String>,
    keep: &HashSet<String>,
    bound: &mut HashSet<String>,
) {
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
        | Inline::Image(_, v, _)
        | Inline::Cite(_, v) => rewrite_inlines(v, live, keep, bound),
        Inline::Note(bs) => rewrite_blocks(bs, live, keep, bound),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{resolve_anchors, structure_cites};
    use crate::ast::{Block, Inline, anchor_attr, empty_attr, id_attr};

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
        resolve_anchors(&mut blocks);
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

    /// A link inside a table cell is held to the same invariant as one in the
    /// body. Table content used to be invisible to this pass in both directions
    /// — ids declared in a cell went uncollected and links in a cell went
    /// unchecked — which is how every dangling link found in the corpus got
    /// through.
    #[test]
    fn table_cells_are_not_a_blind_spot() {
        use crate::ast::{Alignment, Caption, Cell, Row, TableBody, TableFoot, TableHead};

        let cell = |blocks| Cell(empty_attr(), Alignment::AlignDefault, 1, 1, blocks);
        let mut blocks = vec![Block::Table(
            empty_attr(),
            Caption(None, Vec::new()),
            Vec::new(),
            TableHead(empty_attr(), Vec::new()),
            vec![TableBody(
                empty_attr(),
                0,
                Vec::new(),
                vec![Row(
                    empty_attr(),
                    vec![
                        cell(vec![Block::Div(id_attr("in-cell"), Vec::new())]),
                        cell(vec![Block::Para(vec![
                            Inline::Link(
                                empty_attr(),
                                vec![Inline::Str("here".into())],
                                ("#in-cell".into(), String::new()),
                            ),
                            Inline::Link(
                                empty_attr(),
                                vec![Inline::Str("nowhere".into())],
                                ("#absent".into(), String::new()),
                            ),
                        ])]),
                    ],
                )],
            )],
            TableFoot(empty_attr(), Vec::new()),
        )];

        resolve_anchors(&mut blocks);

        let Block::Table(_, _, _, _, bodies, _) = &blocks[0] else { panic!() };
        let Block::Para(inl) = &bodies[0].3[0].1[1].4[0] else { panic!() };
        let links: Vec<_> = inl
            .iter()
            .filter_map(|i| match i {
                Inline::Link(_, _, (u, _)) => Some(u.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec!["#in-cell".to_string()], "cell-declared id resolves");
        let text: String = inl
            .iter()
            .filter_map(|i| match i {
                Inline::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(text.contains("nowhere"), "the demoted link keeps its text");
    }

    /// A synthesized anchor survives exactly when a link needs it: the targeted
    /// one is kept and loses its marker class, the untargeted one vanishes, and
    /// the paragraph that held nothing else goes with it.
    #[test]
    fn keeps_only_targeted_synthesized_anchors() {
        let mut blocks = vec![
            Block::Para(vec![Inline::Link(
                empty_attr(),
                vec![Inline::Str("go".into())],
                ("#wanted".into(), String::new()),
            )]),
            Block::Para(vec![
                Inline::Span(anchor_attr("wanted"), Vec::new()),
                Inline::Str("anchored text".into()),
            ]),
            Block::Para(vec![Inline::Span(anchor_attr("unwanted"), Vec::new())]),
        ];

        resolve_anchors(&mut blocks);

        assert_eq!(blocks.len(), 2, "the anchor-only paragraph is gone");
        let Block::Para(inl) = &blocks[0] else { panic!() };
        assert!(matches!(&inl[0], Inline::Link(..)), "the link survives");
        let Block::Para(inl) = &blocks[1] else { panic!() };
        match &inl[0] {
            Inline::Span(attr, body) => {
                assert_eq!(attr.0, "wanted");
                assert!(attr.1.is_empty(), "the marker class never reaches output");
                assert!(body.is_empty());
            }
            _ => panic!("expected the kept anchor to survive as a plain id"),
        }
    }

    /// A synthesized anchor never duplicates an id: not one a real node already
    /// carries, and not one an earlier anchor already claimed.
    #[test]
    fn synthesized_anchors_never_duplicate_an_id() {
        let mut blocks = vec![
            Block::Para(vec![
                Inline::Link(
                    empty_attr(),
                    vec![Inline::Str("a".into())],
                    ("#real".into(), String::new()),
                ),
                Inline::Link(
                    empty_attr(),
                    vec![Inline::Str("b".into())],
                    ("#twice".into(), String::new()),
                ),
            ]),
            // A real node already owns `real`, so the anchor offering it is
            // redundant; `twice` is owed by two tags but may be bound once.
            Block::Header(1, id_attr("real"), vec![Inline::Str("Heading".into())]),
            Block::Div(anchor_attr("real"), Vec::new()),
            Block::Div(anchor_attr("twice"), Vec::new()),
            Block::Div(anchor_attr("twice"), Vec::new()),
        ];

        resolve_anchors(&mut blocks);

        let ids: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Header(_, attr, _) => Some(attr.0.clone()),
                // A kept block anchor lands as `Plain [Span]` (see the writer
                // note in `rewrite_blocks`).
                Block::Plain(inl) => match inl.first() {
                    Some(Inline::Span(attr, _)) => Some(attr.0.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["real".to_string(), "twice".to_string()]);
    }

    /// Removing a synthesized anchor re-joins what it separated. An anchor that
    /// lands mid-citation must not leave one realized reference split into two
    /// `Link`s — that is the defect `coalesce_inlines` exists to prevent.
    #[test]
    fn removing_an_anchor_re_coalesces_the_run() {
        let target = ("#sec".to_string(), String::new());
        let mut blocks = vec![
            Block::Div(id_attr("sec"), Vec::new()),
            Block::Para(vec![
                Inline::Link(
                    empty_attr(),
                    vec![Inline::Str("Section".into())],
                    target.clone(),
                ),
                Inline::Span(anchor_attr("untargeted"), Vec::new()),
                Inline::Link(empty_attr(), vec![Inline::Str(" 1".into())], target),
            ]),
        ];

        resolve_anchors(&mut blocks);

        let Block::Para(inl) = &blocks[1] else { panic!() };
        assert_eq!(inl.len(), 1, "the two halves merged back into one link");
        match &inl[0] {
            Inline::Link(_, body, _) => {
                let text: String = body
                    .iter()
                    .filter_map(|i| match i {
                        Inline::Str(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(text, "Section 1", "both halves are in the one link");
            }
            _ => panic!("expected the merged link"),
        }
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

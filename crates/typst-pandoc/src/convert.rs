//! The top-level recursion entry and the native-element block/inline dispatch.
//!
//! Because no show rules run for `Target::Pandoc`, inline-level native elements
//! (`StrongElem`, `EmphElem`, `TextElem`, …) reach us interleaved with
//! block-level elements rather than pre-grouped into `ParElem`s. We coalesce
//! consecutive inline children into a single `Para` here, flushing the buffer
//! whenever a block-level element or paragraph break is hit. The control-flow
//! shape mirrors `typst_docx::convert`, but the leaf vocabulary emits Pandoc
//! AST nodes instead of OOXML runs.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::TagElem;
use typst_library::math::EquationElem;
use typst_library::model::{
    Destination, DirectLinkElem, EmphElem, EnumElem, FigureElem, HeadingElem, LinkElem,
    LinkMarker, ListElem, OutlineElem, ParElem, ParbreakElem, QuoteElem, RefElem,
    StrongElem, TableElem, TermsElem,
};
use typst_library::text::{
    HighlightElem, LinebreakElem, RawElem, SmallcapsElem, SmartQuoteElem, SmartQuotes,
    SpaceElem, StrikeElem, SubElem, SuperElem, TextElem, UnderlineElem,
};
use typst_library::model::FootnoteElem;
use typst_library::pdf::PdfMarkerTag;
use typst_library::visualize::ImageElem;
use typst_library::routines::{Arenas, FragmentKind, Pair, RealizationKind};

use crate::ast::{Block, Inline, QuoteType};
use crate::ctx::PandocCtx;
use crate::mappers;

/// Lowers the top-level realized children into the document body blocks.
pub fn run(ctx: &mut PandocCtx, children: &[Pair]) -> SourceResult<Vec<Block>> {
    let mut blocks = convert_children(ctx, children)?;
    // Recover structured `Cite` nodes from realized in-text citation `Link`s,
    // using the bibliography anchor → cite-key map. This runs *before*
    // `prune_dangling_links` so the wrapped cite link (which targets a
    // bibliography-entry anchor that has no representable id) is not demoted.
    structure_cites(&mut blocks, &ctx.cite_anchors);
    prune_dangling_links(&mut blocks);
    Ok(blocks)
}

/// Rewrites every realized in-text citation `Link` into a structured
/// `Inline::Cite` so that `pandoc --citeproc` can re-resolve it against the
/// synthesized `.bib` sidecar. A citation arrives as a `Link` whose `#`-fragment
/// URL is the bibliography entry's backlink anchor; [`PandocCtx::cite_anchors`]
/// maps that anchor to the cite key. The original `Link` is kept inside the
/// `Cite` as the baked fallback, so non-`--citeproc` output is unchanged. A
/// no-op when the map is empty (no bibliography).
fn structure_cites(
    blocks: &mut [Block],
    anchors: &std::collections::HashMap<ecow::EcoString, ecow::EcoString>,
) {
    if anchors.is_empty() {
        return;
    }
    for b in blocks.iter_mut() {
        structure_cites_block(b, anchors);
    }
}

/// Recurses into a block, rewriting cite links within every inline it carries.
fn structure_cites_block(
    block: &mut Block,
    anchors: &std::collections::HashMap<ecow::EcoString, ecow::EcoString>,
) {
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
    anchors: &std::collections::HashMap<ecow::EcoString, ecow::EcoString>,
) {
    for row in rows.iter_mut() {
        for cell in row.1.iter_mut() {
            for b in cell.4.iter_mut() {
                structure_cites_block(b, anchors);
            }
        }
    }
}

fn structure_cites_inlines(
    inlines: &mut [Inline],
    anchors: &std::collections::HashMap<ecow::EcoString, ecow::EcoString>,
) {
    use crate::ast::{Citation, CitationMode};
    for node in inlines.iter_mut() {
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
                // Keep the original link as the fallback so non-citeproc output is
                // unchanged (clickable `[1]` that still jumps to the entry).
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
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in blocks.iter() {
        collect_ids_block(b, &mut ids);
    }
    for b in blocks.iter_mut() {
        prune_block(b, &ids);
    }
}

/// Collects every `Attr` identifier reachable in a block (the anchor namespace).
fn collect_ids_block(block: &Block, ids: &mut std::collections::HashSet<String>) {
    let push = |attr: &crate::ast::Attr, ids: &mut std::collections::HashSet<String>| {
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
fn collect_ids_inline(inline: &Inline, ids: &mut std::collections::HashSet<String>) {
    let push = |attr: &crate::ast::Attr, ids: &mut std::collections::HashSet<String>| {
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
fn prune_block(block: &mut Block, ids: &std::collections::HashSet<String>) {
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
fn prune_inlines(inlines: &mut Vec<Inline>, ids: &std::collections::HashSet<String>) {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
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
    coalesce_inlines(inlines);
}

/// Recurses into an inline node's children, pruning dangling links.
fn prune_inline(inline: &mut Inline, ids: &std::collections::HashSet<String>) {
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

/// Lowers a slice of realized native pairs into blocks.
pub fn convert_children(
    ctx: &mut PandocCtx,
    children: &[Pair],
) -> SourceResult<Vec<Block>> {
    let mut blocks = Vec::new();
    // Buffered inline children of the paragraph currently being assembled.
    let mut pending: Vec<Inline> = Vec::new();
    let mut have_pending = false;
    // Whether the last paragraph-bearing child was a `ParElem` (vs. inline
    // content) — used to tell two adjacent paragraphs apart from one paragraph
    // Typst split into `[par, inline-equation, par]`.
    let mut last_was_par = false;

    for (child, styles) in children {
        let styles = *styles;
        if child.is::<ParbreakElem>() {
            flush_para(&mut pending, &mut have_pending, &mut blocks);
            last_was_par = false;
        } else if let Some(par) = child.to_packed::<ParElem>() {
            // Consecutive `ParElem`s are separate paragraphs; but a single
            // paragraph split by an inline equation into `[par, equation, par]`
            // must continue, not start anew. It continues when the previous
            // content was inline (not another par) and there is buffered content.
            let continues = !last_was_par && have_pending;
            if !continues {
                flush_para(&mut pending, &mut have_pending, &mut blocks);
            }
            inline_into(ctx, &par.body, styles, &mut pending)?;
            have_pending = true;
            last_was_par = true;
        } else if let Some(elem) = child.to_packed::<TagElem>() {
            // Introspection tag. There is no Pandoc tag node, so defer it for the
            // introspector regardless of context (block or inline). Transparent —
            // does not change paragraph structure. Load-bearing for convergence.
            ctx.deferred_tags.push(elem.tag.clone());
        } else if let Some(eq) = child.to_packed::<EquationElem>()
            && !eq.block.get(styles)
        {
            // An *inline* equation stays in the current paragraph.
            handle_inline(ctx, child, styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else if let Some(raw) = child.to_packed::<RawElem>()
            && !raw.block.get(styles)
        {
            // `RAW_RULE` is intentionally NOT registered for Pandoc (the `code`
            // mapper emits idiomatic `Code`/`CodeBlock` directly), so an inline
            // `RawElem` survives realization and must be kept *inline* — routing
            // it to `handle_block` would flush the paragraph mid-sentence and
            // split one paragraph into three (the `… and `code`.` defect).
            handle_inline(ctx, child, styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else if is_inline(child) {
            handle_inline(ctx, child, styles, &mut pending)?;
            have_pending = true;
            last_was_par = false;
        } else {
            flush_para(&mut pending, &mut have_pending, &mut blocks);
            handle_block(ctx, child, styles, &mut blocks)?;
            last_was_par = false;
        }
    }
    flush_para(&mut pending, &mut have_pending, &mut blocks);
    Ok(blocks)
}

/// Flushes buffered inline children into a `Para` block (dropping an empty one).
fn flush_para(pending: &mut Vec<Inline>, have_pending: &mut bool, blocks: &mut Vec<Block>) {
    if !*have_pending && pending.is_empty() {
        return;
    }
    let mut inlines = std::mem::take(pending);
    *have_pending = false;
    coalesce_inlines(&mut inlines);
    if !inlines.is_empty() {
        blocks.push(Block::Para(inlines));
    }
}

/// Merges adjacent inline nodes that pandoc itself would coalesce, keeping the
/// output compact and faithful: consecutive `Str` runs join, and consecutive
/// `Link`s with an identical `Attr` + `Target` merge their bodies into one
/// `Link`. The latter is load-bearing for cross-references: a realized `@ref`
/// arrives as several runs ("Section", nbsp, "1.1") each wrapped — by the
/// per-run `LinkElem::current` reconstruction — in its own `Link` to the same
/// `#`-anchor; without merging, one reference emits three separate links.
pub(crate) fn coalesce_inlines(inlines: &mut Vec<Inline>) {
    if inlines.len() < 2 {
        return;
    }
    let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
    for node in inlines.drain(..) {
        match (merged.last_mut(), node) {
            // Join adjacent plain text.
            (Some(Inline::Str(last)), Inline::Str(text)) => last.push_str(&text),
            // Merge adjacent links with the same attributes and destination.
            (
                Some(Inline::Link(la, lbody, lt)),
                Inline::Link(ra, mut rbody, rt),
            ) if *la == ra && *lt == rt => {
                lbody.append(&mut rbody);
            }
            (_, node) => merged.push(node),
        }
    }
    *inlines = merged;
}

/// Realizes an inline body as a paragraph interior (`RealizationKind::Par`) and
/// lowers each child into inlines.
pub fn inline_into(
    ctx: &mut PandocCtx,
    body: &Content,
    styles: StyleChain,
    out: &mut Vec<Inline>,
) -> SourceResult<()> {
    let arenas = Arenas::default();
    let children = (ctx.engine.library.routines.realize)(
        RealizationKind::Par,
        ctx.engine,
        ctx.locator,
        &arenas,
        body,
        styles,
    )?;
    let pairs: Vec<_> = children.to_vec();
    for (child, child_styles) in pairs {
        handle_inline(ctx, child, child_styles, out)?;
    }
    Ok(())
}

/// Realizes a block body into a sequence of blocks.
pub fn blocks(
    ctx: &mut PandocCtx,
    body: &Content,
    styles: StyleChain,
) -> SourceResult<Vec<Block>> {
    let arenas = Arenas::default();
    let children = (ctx.engine.library.routines.realize)(
        RealizationKind::Fragment { kind: &mut FragmentKind::Block },
        ctx.engine,
        ctx.locator,
        &arenas,
        body,
        styles,
    )?;
    let pairs: Vec<_> = children.to_vec();
    convert_children(ctx, &pairs)
}

/// Whether a native element is inline-level (formatting/text/refs/etc.).
///
/// `TagElem` and `EquationElem` are intentionally NOT here so that the block
/// dispatch records introspection tags and routes equations (inline or block)
/// through the math mapper.
fn is_inline(child: &Content) -> bool {
    use typst_library::layout::HElem;

    child.is::<TextElem>()
        || child.is::<SpaceElem>()
        || child.is::<LinebreakElem>()
        || child.is::<SmartQuoteElem>()
        || child.is::<HElem>()
        || child.is::<StrongElem>()
        || child.is::<EmphElem>()
        || child.is::<SubElem>()
        || child.is::<SuperElem>()
        || child.is::<UnderlineElem>()
        || child.is::<StrikeElem>()
        || child.is::<HighlightElem>()
        || child.is::<SmallcapsElem>()
        || child.is::<LinkElem>()
        || child.is::<RefElem>()
        || child.is::<ImageElem>()
        || child.is::<DirectLinkElem>()
        || child.is::<LinkMarker>()
        || child.is::<FootnoteElem>()
}

/// Handles a single realized inline child, appending inlines to `out`.
pub(crate) fn handle_inline(
    ctx: &mut PandocCtx,
    child: &Content,
    styles: StyleChain,
    out: &mut Vec<Inline>,
) -> SourceResult<()> {
    if let Some(elem) = child.to_packed::<TagElem>() {
        // Run-only context: defer the tag for the introspector.
        ctx.deferred_tags.push(elem.tag.clone());
    } else if child.is::<SpaceElem>() {
        out.push(Inline::Space);
        ctx.last_char = Some(' ');
    } else if let Some(elem) = child.to_packed::<TextElem>() {
        let text = apply_case(styles, elem.text.as_str());
        ctx.last_char = text.chars().last().or(ctx.last_char);
        // A cross-reference / `#link` whose `LinkMarker` was stripped during
        // realization leaves its resolved destination on `LinkElem::current`.
        // Wrap the text run in a `Link` so the jump survives (the shared `#`-
        // anchor namespace for an internal target, the raw URL otherwise).
        emit_linked(ctx, out, styles, text);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::HElem>() {
        // Horizontal spacing has no faithful inline node; approximate a non-zero,
        // non-fractional gap with a single `Space` rather than dropping it.
        if !elem.amount.is_zero() && !elem.amount.is_fractional() {
            out.push(Inline::Space);
            ctx.last_char = Some(' ');
        }
    } else if child.is::<LinebreakElem>() {
        out.push(Inline::LineBreak);
        ctx.last_char = None;
    } else if let Some(elem) = child.to_packed::<SmartQuoteElem>() {
        let double = elem.double.get(styles);
        if elem.enabled.get(styles) {
            // A real curly quote: emit it as text (matches paged/HTML output).
            let quotes = SmartQuotes::get(
                elem.quotes.get_ref(styles),
                styles.get(TextElem::lang),
                styles.get(TextElem::region),
                elem.alternative.get(styles),
            );
            let q = ctx.quoter.quote(ctx.last_char, &quotes, double);
            ctx.last_char = q.chars().last().or(ctx.last_char);
            emit_formatted(out, styles, q.to_string());
        } else {
            // Quoting disabled: emit a structural `Quoted` node so writers pick
            // the locale-appropriate glyph.
            let qt = if double { QuoteType::DoubleQuote } else { QuoteType::SingleQuote };
            out.push(Inline::Quoted(qt, Vec::new()));
        }
    } else if let Some(elem) = child.to_packed::<StrongElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Strong(inner));
    } else if let Some(elem) = child.to_packed::<EmphElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Emph(inner));
    } else if let Some(elem) = child.to_packed::<SubElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Subscript(inner));
    } else if let Some(elem) = child.to_packed::<SuperElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Superscript(inner));
    } else if let Some(elem) = child.to_packed::<UnderlineElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Underline(inner));
    } else if let Some(elem) = child.to_packed::<StrikeElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Strikeout(inner));
    } else if let Some(elem) = child.to_packed::<SmallcapsElem>() {
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::SmallCaps(inner));
    } else if let Some(elem) = child.to_packed::<HighlightElem>() {
        // No native highlight inline; wrap in a `Span` with a `mark` class.
        let inner = wrap(ctx, &elem.body, styles)?;
        out.push(Inline::Span(crate::ast::class_attr("mark"), inner));
    } else if let Some(elem) = child.to_packed::<RawElem>() {
        // Inline raw → `Code`.
        out.extend(mappers::code::raw_inline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EquationElem>() {
        // Inline equation (block equations go through `handle_block`).
        out.extend(mappers::math::equation_inline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<ImageElem>() {
        out.push(mappers::image::image_inline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<LinkElem>() {
        out.extend(mappers::link::link_inline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<RefElem>() {
        out.extend(mappers::citation::reference(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<FootnoteElem>() {
        out.push(mappers::footnote::footnote(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<DirectLinkElem>() {
        // The realized wrapper a `RefElem`/footnote cross-reference becomes once
        // `REF_RULE`/`DIRECT_LINK_RULE` run (registered for Pandoc). It carries
        // the target `Location` directly, so emit a `#`-fragment `Link` in the
        // shared anchor namespace around the lowered body.
        let mut body = Vec::new();
        inline_into(ctx, &elem.body, styles, &mut body)?;
        let id = ctx.anchor_id(elem.loc);
        out.push(Inline::Link(
            crate::ast::empty_attr(),
            body,
            (format!("#{id}"), String::new()),
        ));
    } else if let Some(elem) = child.to_packed::<LinkMarker>() {
        // The wrapper a `#link` becomes after `LINK_RULE` runs: the resolved
        // destination rides on `LinkElem::current` on the chain. Emit a `Link`
        // (URL or `#`-fragment); a positional/None destination drops the wrapper.
        let dest = styles.get_cloned(LinkElem::current);
        let mut body = Vec::new();
        inline_into(ctx, &elem.body, styles, &mut body)?;
        match dest {
            Some(Destination::Url(url)) => out.push(Inline::Link(
                crate::ast::empty_attr(),
                body,
                (url.into_inner().to_string(), String::new()),
            )),
            Some(Destination::Location(loc)) => {
                let id = ctx.anchor_id(loc);
                out.push(Inline::Link(
                    crate::ast::empty_attr(),
                    body,
                    (format!("#{id}"), String::new()),
                ));
            }
            // Positional jump (no page model) or no destination: keep the body.
            _ => out.extend(body),
        }
    } else if let Some(elem) = child.to_packed::<typst_library::layout::BoxElem>() {
        // A box: extract its body inlines, or rasterize to an inline image.
        match elem.body.get_cloned(styles) {
            None => {}
            Some(body) => inline_into(ctx, &body, styles, out)?,
        }
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PlaceElem>() {
        // `#place(..)` in a run context: keep the body inline (position lost).
        inline_into(ctx, &elem.body.clone(), styles, out)?;
    } else {
        // No idiomatic inline representation: rasterize and embed as an image.
        if let Some((url, _size)) = ctx.rasterize(child, styles, child.span())? {
            out.push(Inline::Image(
                crate::ast::empty_attr(),
                Vec::new(),
                (url.to_string(), String::new()),
            ));
        } else {
            ctx.warn_ignored(child.elem().name(), child.span());
        }
    }
    Ok(())
}

/// Wraps an inline body's realized children into a flat `Vec<Inline>`.
fn wrap(
    ctx: &mut PandocCtx,
    body: &Content,
    styles: StyleChain,
) -> SourceResult<Vec<Inline>> {
    let mut inner = Vec::new();
    inline_into(ctx, body, styles, &mut inner)?;
    coalesce_inlines(&mut inner);
    Ok(inner)
}

/// Pushes a `Str`, coalescing with a preceding `Str` (keeps output compact and
/// matches pandoc's own coalescing). Whitespace stays as separate `Space` nodes.
fn push_str(out: &mut Vec<Inline>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Inline::Str(last)) = out.last_mut() {
        last.push_str(&text);
        return;
    }
    out.push(Inline::Str(text));
}

/// Emits a (possibly formatted) text run, wrapping it in a `Link` when the
/// chain carries a resolved link destination on `LinkElem::current`.
///
/// A cross-reference (`@x`) or `#link` is realized — for the Pandoc target — into
/// content whose `LinkMarker` wrapper is stripped by `LINK_MARKER_RULE`, leaving
/// only the destination on the style chain (`LinkElem::current`). Reading it here
/// per-run reconstructs the `Link`. Adjacent runs under one link each get their
/// own `Link` node; pandoc's writers coalesce identical adjacent links, so the
/// rendered result is correct (just not maximally compact). A positional jump
/// (no page model) is dropped, keeping the bare text.
fn emit_linked(
    ctx: &mut PandocCtx,
    out: &mut Vec<Inline>,
    styles: StyleChain,
    text: String,
) {
    use typst_library::model::Destination;

    let dest = styles.get_cloned(LinkElem::current);
    let url = match dest {
        Some(Destination::Url(url)) => Some(url.into_inner().to_string()),
        Some(Destination::Location(loc)) => Some(format!("#{}", ctx.anchor_id(loc))),
        // No destination (the common case) or a positional jump: plain run.
        _ => None,
    };

    let Some(url) = url else {
        emit_formatted(out, styles, text);
        return;
    };

    // Build the formatted run into a scratch buffer, then wrap it in one `Link`.
    let mut inner = Vec::new();
    emit_formatted(&mut inner, styles, text);
    if inner.is_empty() {
        return;
    }
    out.push(Inline::Link(crate::ast::empty_attr(), inner, (url, String::new())));
}

/// Emits a text run, wrapping it in the Pandoc inline nodes that correspond to
/// the resolved inline-formatting style flags on the chain.
///
/// The inline-formatting layout rules registered for `Target::Pandoc` (see
/// `typst-layout/src/rules.rs`) fold `*strong*`/`_emph_`/`#sub`/`#super`/
/// `#underline`/`#strike`/`#highlight`/`#smallcaps` into `TextElem` style flags
/// (`delta`/`emph`/`style`/`shift_settings`/`deco`/`smallcaps`) — exactly as the
/// DOCX backend reads them in `resolve_text_props`. We read the same flags and
/// wrap the run in the matching Pandoc node. Formatting nests outermost→innermost
/// in a fixed order; adjacent same-format runs stay separate (pandoc coalesces
/// adjacent identical wrappers in its writers, so this is correct, just not
/// maximally compact).
fn emit_formatted(out: &mut Vec<Inline>, styles: StyleChain, text: String) {
    use typst_library::text::{DecoLine, ScriptKind};

    if text.is_empty() {
        return;
    }

    // `Strong`/`Emph` are SEMANTIC emphasis markers, not ambient font styling.
    // We key them on the explicit `*strong*`/`_emph_` signals (the `delta`
    // accumulated by `STRONG_RULE` and the `emph` toggle set by `EMPH_RULE`),
    // NOT on the absolute resolved weight/style. This is load-bearing: a
    // `HeadingElem` show-set sets `weight = BOLD` ambient (its body is not
    // semantically `*strong*`), and Pandoc writers already bold a `Header`, so
    // promoting that ambient weight to `Strong` would both be wrong and
    // double-bold. Likewise `#set text(weight: "bold")` is ambient font choice,
    // not emphasis. An explicit `*x*` thickens via a positive `delta`; an
    // explicit `_x_` flips the `emph` toggle.
    let bold = styles.get(TextElem::delta).0 > 0;
    let italic = styles.get(TextElem::emph).0;

    // Sub/super.
    let shift = styles
        .get_ref(TextElem::shift_settings)
        .as_ref()
        .map(|s| s.kind);

    // Decorations.
    let mut underline = false;
    let mut strike = false;
    let mut highlight = false;
    for deco in styles.get_cloned(TextElem::deco) {
        match &deco.line {
            DecoLine::Underline { .. } => underline = true,
            DecoLine::Strikethrough { .. } => strike = true,
            DecoLine::Highlight { .. } => highlight = true,
            _ => {}
        }
    }

    let smallcaps = styles.get(TextElem::smallcaps).is_some();

    // Build the innermost `Str`, then wrap outward. Order (innermost→outermost):
    // sub/super, smallcaps, strike, underline, emph, strong, highlight.
    let mut node = Inline::Str(text);
    match shift {
        Some(ScriptKind::Sub) => node = Inline::Subscript(vec![node]),
        Some(ScriptKind::Super) => node = Inline::Superscript(vec![node]),
        None => {}
    }
    if smallcaps {
        node = Inline::SmallCaps(vec![node]);
    }
    if strike {
        node = Inline::Strikeout(vec![node]);
    }
    if underline {
        node = Inline::Underline(vec![node]);
    }
    if italic {
        node = Inline::Emph(vec![node]);
    }
    if bold {
        node = Inline::Strong(vec![node]);
    }
    if highlight {
        node = Inline::Span(crate::ast::class_attr("mark"), vec![node]);
    }

    // Coalesce only when the run is unformatted (a plain `Str`) — formatted nodes
    // stay separate.
    if let Inline::Str(t) = &node {
        push_str(out, t.clone());
    } else {
        out.push(node);
    }
}

/// Applies `TextElem::case` to a string.
fn apply_case(styles: StyleChain, text: &str) -> String {
    if let Some(case) = styles.get(TextElem::case) {
        case.apply(text)
    } else {
        text.to_string()
    }
}

/// Dispatches one realized native block element.
fn handle_block(
    ctx: &mut PandocCtx,
    child: &Content,
    styles: StyleChain,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    if let Some(elem) = child.to_packed::<TagElem>() {
        ctx.deferred_tags.push(elem.tag.clone());
    } else if let Some(elem) = child.to_packed::<PdfMarkerTag>() {
        // Bibliography / list / term structural markers. The bibliography ones
        // (`Bibliography`/`BibEntry`) are load-bearing for citations: a `BibEntry`
        // carries the backlink `Location` that in-text cite Links anchor to, so we
        // lower it to a real, anchored, selectable block rather than letting the
        // whole reference list rasterize to one image (which would leave every
        // cite dangling). See `mappers/citation.rs`.
        out.extend(mappers::citation::marker_tag(elem, styles, ctx)?);
    } else if child.is::<typst_library::layout::PagebreakElem>() {
        // No page model: drop.
    } else if let Some(elem) = child.to_packed::<ParElem>() {
        let mut inlines = Vec::new();
        inline_into(ctx, &elem.body, styles, &mut inlines)?;
        coalesce_inlines(&mut inlines);
        if !inlines.is_empty() {
            out.push(Block::Para(inlines));
        }
    } else if child.is::<ParbreakElem>() {
        // Paragraph boundary; no-op marker.
    } else if let Some(elem) = child.to_packed::<HeadingElem>() {
        out.extend(mappers::outline::heading(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<ListElem>() {
        out.extend(mappers::list::list(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EnumElem>() {
        out.extend(mappers::list::enum_(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<TermsElem>() {
        out.extend(mappers::terms::terms(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<TableElem>() {
        out.extend(mappers::table::table(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::GridElem>() {
        out.extend(mappers::table::grid(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<OutlineElem>() {
        out.extend(mappers::outline::outline(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<EquationElem>() {
        out.extend(mappers::math::equation_block(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<FigureElem>() {
        out.extend(mappers::figure::figure(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<QuoteElem>() {
        out.extend(mappers::quote::quote(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<RawElem>()
        && elem.block.get(styles)
    {
        out.extend(mappers::code::raw_block(elem, styles, ctx)?);
    } else if let Some(elem) = child.to_packed::<typst_library::visualize::LineElem>()
        && elem.end.get_ref(styles).is_none()
        && {
            let deg = elem.angle.get(styles).to_deg().rem_euclid(180.0);
            deg < 1.0 || deg > 179.0
        }
    {
        // Horizontal `#line(length: ..)` divider → `HorizontalRule`.
        out.push(Block::HorizontalRule);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PadElem>() {
        // Keep `#pad(..)[body]` as real blocks (indent has no AST equivalent).
        out.extend(blocks(ctx, &elem.body.clone(), styles)?);
    } else if let Some(elem) = child.to_packed::<typst_library::layout::PlaceElem>() {
        // `#place(..)` is absolute positioning with no Pandoc AST equivalent: the
        // position is necessarily lost in a linear document. Rather than drop the
        // body wholesale (which empties layout-driven docs whose entire content
        // sits in `#place` — CVs, posters, game canvases), splice the body into
        // the flow in document order. A body with no idiomatic blocks (a bare
        // cetz/layouter canvas) still rasterizes via the recursion's fallbacks.
        out.extend(blocks(ctx, &elem.body.clone(), styles)?);
    } else if child.is::<typst_library::layout::FlushElem>()
        || child.is::<typst_library::layout::ColbreakElem>()
    {
        // No page/column model: drop.
    } else if let Some(elem) = child.to_packed::<typst_library::layout::BlockElem>() {
        // A `#block(..)` body: recurse into its content, dropping the visual box
        // (fills/borders have no Pandoc AST). A layouter body rasterizes.
        use typst_library::layout::BlockBody;
        match elem.body.get_ref(styles) {
            Some(BlockBody::Content(content)) => {
                out.extend(blocks(ctx, &content.clone(), styles)?);
            }
            _ => {
                if let Some((url, _)) = ctx.rasterize(child, styles, child.span())? {
                    out.push(Block::Para(vec![Inline::Image(
                        crate::ast::empty_attr(),
                        Vec::new(),
                        (url.to_string(), String::new()),
                    )]));
                }
            }
        }
    } else {
        // Fall back: treat anything else as inline content in a paragraph.
        let mut inlines = Vec::new();
        handle_inline(ctx, child, styles, &mut inlines)?;
        if !inlines.is_empty() {
            out.push(Block::Para(inlines));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{coalesce_inlines, prune_dangling_links, structure_cites};
    use crate::ast::{empty_attr, id_attr, Block, Inline};

    /// Adjacent `Link`s with the same `Attr` + `Target` merge their bodies into
    /// one `Link` (a realized `@ref` is emitted as several per-run links to one
    /// anchor; without merging, one reference becomes three links).
    #[test]
    fn merges_adjacent_same_target_links() {
        let target = ("#ref-x".to_string(), String::new());
        let mut inlines = vec![
            Inline::Link(empty_attr(), vec![Inline::Str("Section".into())], target.clone()),
            Inline::Link(empty_attr(), vec![Inline::Str("\u{a0}".into())], target.clone()),
            Inline::Link(empty_attr(), vec![Inline::Str("1.1".into())], target.clone()),
        ];
        coalesce_inlines(&mut inlines);
        assert_eq!(inlines.len(), 1);
        match &inlines[0] {
            Inline::Link(_, body, t) => {
                assert_eq!(*t, target);
                assert_eq!(body.len(), 3);
            }
            _ => panic!("expected one merged Link"),
        }
    }

    /// Links to *different* targets are NOT merged.
    #[test]
    fn keeps_distinct_target_links_separate() {
        let mut inlines = vec![
            Inline::Link(empty_attr(), vec![Inline::Str("a".into())], ("#x".into(), String::new())),
            Inline::Link(empty_attr(), vec![Inline::Str("b".into())], ("#y".into(), String::new())),
        ];
        coalesce_inlines(&mut inlines);
        assert_eq!(inlines.len(), 2);
    }

    /// Adjacent plain `Str` runs join.
    #[test]
    fn joins_adjacent_str() {
        let mut inlines = vec![
            Inline::Str("foo".into()),
            Inline::Str("bar".into()),
            Inline::Space,
            Inline::Str("baz".into()),
        ];
        coalesce_inlines(&mut inlines);
        // foobar, Space, baz
        assert_eq!(inlines.len(), 3);
        match &inlines[0] {
            Inline::Str(s) => assert_eq!(s, "foobar"),
            _ => panic!("expected joined Str"),
        }
    }

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
                // resolves → the bib entry Div below
                Inline::Link(empty_attr(), vec![Inline::Str("[1]".into())], live),
                // dangles → must become bare "[2]"
                Inline::Link(empty_attr(), vec![Inline::Str("[2]".into())], dead),
                // external → untouched
                Inline::Link(empty_attr(), vec![Inline::Str("site".into())], ext),
            ]),
            // The anchor the live link targets.
            Block::Div(id_attr("ref-live"), vec![Block::Para(vec![Inline::Str(
                "Author, Title.".into(),
            )])]),
        ];
        prune_dangling_links(&mut blocks);
        let Block::Para(inl) = &blocks[0] else { panic!() };
        // live link kept, external link kept, dead link demoted to a Str.
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
        // The demoted link's text survives somewhere in the paragraph.
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
        let mut anchors = std::collections::HashMap::new();
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
                // The original link is preserved as the fallback body.
                assert!(matches!(fallback[0], Inline::Link(..)));
            }
            _ => panic!("expected a Cite"),
        }
    }

    /// An empty anchor map (no bibliography) leaves all links untouched — the
    /// pass is a no-op, so non-cite documents are byte-unchanged.
    #[test]
    fn structure_cites_noop_without_bibliography() {
        let anchors = std::collections::HashMap::new();
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

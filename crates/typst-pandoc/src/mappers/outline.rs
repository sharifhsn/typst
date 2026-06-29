//! Heading + outline mappers.
//!
//! `heading` is implemented (one of the proof-of-life nodes):
//! `HeadingElem` → `Header(level, Attr{id}, [Inline])`, dropping the baked
//! number (the writer owns numbering), clamping the level to `1..=6`, and
//! attaching the heading's `id` from its `Location` (the shared id namespace
//! that internal `#id`-`Link`s target — load-bearing).
//!
//! `outline` is the table-of-contents mapper. Pandoc has NO TOC node: a live
//! table of contents is a *writer flag* (`--toc`), recomputed by each writer
//! from the document's `Header`s, and there is no flag at all for a list of
//! figures/tables. We therefore emit a **static** `BulletList` of internal
//! (`#`-anchor) `Link`s to each outlined element, reusing the same id namespace
//! (`ctx.anchor_id`) the heading/figure mappers stamp onto their `Attr`, so the
//! links resolve in the output. This loses live page numbers and live
//! recomputation (a static snapshot taken at export time), but it is the only
//! way to preserve a visible, navigable TOC — including list-of-figures/tables,
//! which have no `--toc` equivalent at all. See the gap note at the bottom.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::{HeadingElem, OutlineElem, OutlineEntry};

use crate::ast::{Attr, Block, Inline};
use crate::convert::inline_into;
use crate::ctx::PandocCtx;

/// `HeadingElem` → `Header(level, Attr{id}, [Inline])`.
pub fn heading(
    elem: &Packed<HeadingElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    // Absolute nesting level (folds offset + depth). Pandoc `Header` levels are
    // `1..=6`; clamp deeper headings (no flag for `outlined: false`).
    let level = elem.resolve_level(styles).get();
    let level = level.clamp(1, 6) as i32;

    // The heading `id`: the bookmark name a `#ref`/`#id`-`Link` targets. Derived
    // from the location so the same target produces the same id, matching what
    // `citation`/`link` mappers will emit. Drop the baked number entirely.
    let attr: Attr = match elem.location() {
        Some(loc) => crate::ast::id_attr(ctx.anchor_id(loc).to_string()),
        None => crate::ast::empty_attr(),
    };

    let mut inlines = Vec::new();
    inline_into(ctx, &elem.body, styles, &mut inlines)?;
    crate::convert::coalesce_inlines(&mut inlines);

    Ok(vec![Block::Header(level, attr, inlines)])
}

/// `OutlineElem` → a static `BulletList` of internal `#`-anchor `Link`s.
///
/// Pandoc has no TOC node (a live TOC is the writer flag `--toc`, and a
/// list-of-figures/tables has no flag at all), so we materialize the outline
/// the introspector resolves into a nested bullet list whose entries link to
/// the outlined elements via the shared `anchor_id` namespace. An optional
/// title becomes an `unnumbered` `Header`.
pub fn outline(
    elem: &Packed<OutlineElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    // Resolve the outlined entries (headings, or figures/tables/equations for a
    // list-of-X) the same way the layout backends do: a flat list paired with
    // its nesting level. Each entry already carries the depth filter applied.
    // `realize_flat` borrows the engine mutably; collect the owned entries and
    // their levels first, then release the borrow before lowering bodies (which
    // also needs the engine, via `ctx`).
    let entries: Vec<(usize, Packed<OutlineEntry>)> = {
        let engine = ctx.engine();
        elem.realize_flat(engine, styles)?
            .into_iter()
            .map(|entry| (entry.level.get(), entry))
            .collect()
    };

    let mut blocks = Vec::new();

    // Optional title → an `unnumbered` `Header` (so a downstream `--toc`, if the
    // user later runs one, does not pick the title up as its own entry, matching
    // the DOCX `TOCHeading` intent). `realize_title` wraps the resolved title in
    // a level-1 `HeadingElem`; we lower just its body.
    if let Some(title_body) = elem
        .realize_title(styles)
        .as_ref()
        .and_then(|c| c.to_packed::<HeadingElem>())
        .map(|h| h.body.clone())
    {
        let mut title_inlines = Vec::new();
        inline_into(ctx, &title_body, styles, &mut title_inlines)?;
        if !title_inlines.is_empty() {
            blocks.push(Block::Header(1, crate::ast::class_attr("unnumbered"), title_inlines));
        }
    }

    if entries.is_empty() {
        // Nothing to outline (e.g. a list of figures in a doc with no captioned
        // figures). Emit just the title, if any; drop the empty list.
        return Ok(blocks);
    }

    // Lower each entry to a single `Plain([Link])` block: the entry body as a
    // hyperlink to the element's anchor (shared id namespace). When the target
    // has no resolvable location, drop the link wrapper but keep the text so the
    // entry still appears. (We deliberately do NOT use `OutlineEntry::inner`,
    // which bakes in fill leaders + page numbers — Pandoc owns neither.)
    let mut items: Vec<(usize, Vec<Block>)> = Vec::with_capacity(entries.len());
    for (level, entry) in entries {
        let mut text = Vec::new();
        if let Ok(body) = entry.body() {
            inline_into(ctx, &body, styles, &mut text)?;
        }

        let inline = match entry.element_location() {
            Ok(loc) => {
                let anchor = ctx.anchor_id(loc);
                Inline::Link(
                    crate::ast::empty_attr(),
                    text,
                    (format!("#{anchor}"), String::new()),
                )
            }
            // No location to link to: keep the text as a bare span.
            Err(_) => Inline::Span(crate::ast::empty_attr(), text),
        };

        items.push((level, vec![Block::Plain(vec![inline])]));
    }

    blocks.push(build_nested(items));
    Ok(blocks)
}

/// Folds a flat `(level, blocks)` entry list into a nested `BulletList`. A child
/// (strictly deeper level) is appended as a trailing nested `BulletList` to the
/// most recent shallower item, matching how every layout backend renders the
/// outline tree. Levels that jump by more than one are tolerated (the deeper
/// item simply nests under whatever the current parent is). `Block` is not
/// `Clone`, so the entries' blocks are *consumed* via `Option::take` as the
/// cursor advances.
fn build_nested(mut items: Vec<(usize, Vec<Block>)>) -> Block {
    // The minimum level present is the root level for this sub-list.
    let root = items.iter().map(|(l, _)| *l).min().unwrap_or(1);
    let mut taken: Vec<(usize, Option<Vec<Block>>)> =
        items.drain(..).map(|(l, b)| (l, Some(b))).collect();
    let mut idx = 0;
    Block::BulletList(build_level(&mut taken, &mut idx, root))
}

/// Consumes entries at `level` (and recursively their deeper descendants),
/// returning one `Vec<Block>` per bullet item. Stops when it reaches an entry
/// shallower than `level` (which belongs to an enclosing list). Each item's
/// blocks are moved out exactly once (`Option::take`).
fn build_level(
    items: &mut [(usize, Option<Vec<Block>>)],
    idx: &mut usize,
    level: usize,
) -> Vec<Vec<Block>> {
    let mut out: Vec<Vec<Block>> = Vec::new();
    while *idx < items.len() {
        let item_level = items[*idx].0;
        if item_level < level {
            // Belongs to an outer list.
            break;
        }
        if item_level > level {
            // A deeper entry with no shallower parent in this run (e.g. the very
            // first entry is already deep, or a level was skipped). Start its own
            // item rather than dropping it.
            let children = build_level(items, idx, item_level);
            if let Some(last) = out.last_mut() {
                // Attach the deeper run as a nested list on the previous item.
                last.push(Block::BulletList(children));
            } else {
                out.push(vec![Block::BulletList(children)]);
            }
            continue;
        }
        // Same level: a new item. Move its blocks out.
        let mut blocks = items[*idx].1.take().unwrap_or_default();
        *idx += 1;
        // Pull in any strictly-deeper entries that immediately follow as nested.
        if *idx < items.len() && items[*idx].0 > level {
            let child_level = items[*idx].0;
            let children = build_level(items, idx, child_level);
            blocks.push(Block::BulletList(children));
        }
        out.push(blocks);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Block;

    /// Builds `(level, [Plain[Str(label)]])` test entries.
    fn entry(level: usize, label: &str) -> (usize, Vec<Block>) {
        (level, vec![Block::Plain(vec![Inline::Str(label.into())])])
    }

    /// Extracts the label from a `[Plain[Str]]` item body (panics otherwise).
    fn label_of(blocks: &[Block]) -> &str {
        match &blocks[0] {
            Block::Plain(inlines) => match &inlines[0] {
                Inline::Str(s) => s,
                _ => panic!("expected Str"),
            },
            _ => panic!("expected Plain"),
        }
    }

    #[test]
    fn flat_list() {
        let items = vec![entry(1, "A"), entry(1, "B")];
        let Block::BulletList(list) = build_nested(items) else { panic!() };
        assert_eq!(list.len(), 2);
        assert_eq!(label_of(&list[0]), "A");
        assert_eq!(label_of(&list[1]), "B");
        // No nested lists.
        assert_eq!(list[0].len(), 1);
    }

    #[test]
    fn nests_deeper_entry_under_previous() {
        // A(1) > B(2) > C(2) ; D(1)
        let items =
            vec![entry(1, "A"), entry(2, "B"), entry(2, "C"), entry(1, "D")];
        let Block::BulletList(list) = build_nested(items) else { panic!() };
        assert_eq!(list.len(), 2); // A and D at the top level.
        assert_eq!(label_of(&list[0]), "A");
        assert_eq!(label_of(&list[1]), "D");
        // A has a nested BulletList with B and C.
        let Block::BulletList(children) = &list[0][1] else {
            panic!("expected nested list under A")
        };
        assert_eq!(children.len(), 2);
        assert_eq!(label_of(&children[0]), "B");
        assert_eq!(label_of(&children[1]), "C");
    }

    #[test]
    fn deep_first_entry_starts_its_own_item() {
        // A doc whose first outlined entry is level 2 (no level-1 parent).
        let items = vec![entry(2, "X"), entry(2, "Y")];
        let Block::BulletList(list) = build_nested(items) else { panic!() };
        // Root level is 2; both are siblings.
        assert_eq!(list.len(), 2);
        assert_eq!(label_of(&list[0]), "X");
        assert_eq!(label_of(&list[1]), "Y");
    }

    #[test]
    fn three_levels() {
        // A(1) > B(2) > C(3) ; D(1)
        let items =
            vec![entry(1, "A"), entry(2, "B"), entry(3, "C"), entry(1, "D")];
        let Block::BulletList(list) = build_nested(items) else { panic!() };
        assert_eq!(list.len(), 2);
        let Block::BulletList(b_list) = &list[0][1] else { panic!() };
        assert_eq!(label_of(&b_list[0]), "B");
        let Block::BulletList(c_list) = &b_list[0][1] else { panic!() };
        assert_eq!(label_of(&c_list[0]), "C");
    }
}

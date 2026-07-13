//! List / enum mappers — Pandoc target.
//!
//! Pandoc has native list nodes, so unlike the OOXML target there is no
//! numbering registry, no `numId`/`abstractNum` shape, and no per-level indent
//! bookkeeping: a `ListElem` becomes a [`Block::BulletList`] and an `EnumElem`
//! becomes a [`Block::OrderedList`] carrying [`ListAttributes`].
//!
//! Nesting is the recursive `[[Block]]` structure itself: each item body is
//! re-realized through [`crate::convert::blocks`], and a nested list inside an
//! item simply surfaces as another `BulletList`/`OrderedList` block within that
//! item's block list. Typst's `ListElem::depth`/`EnumElem::parents` machinery is
//! irrelevant to the structure — pandoc derives the visual nesting from the tree
//! shape. (`parents` is still read to pick the *displayed* pattern segment.)
//!
//! Tightness (§2.4): a *tight* list collapses each item's paragraph to a
//! `Plain` (no inter-item paragraph spacing); a *loose* list keeps `Para`.
//! Typst exposes this as `ListElem::tight`/`EnumElem::tight`.
//!
//! Numbering (§3, LOSSY note): the closed family
//! decimal/lower-alpha/upper-alpha/lower-roman/upper-roman × Period/OneParen/
//! TwoParens maps cleanly onto pandoc's [`ListNumberStyle`]/[`ListNumberDelim`].
//! Anything pandoc cannot express as a monotonic native counter — a closure
//! (`numbering: n => ..`), `reversed`, explicit per-item `number`s, the `full`
//! (`1.2.a`) multi-level form, a non-empty literal prefix, or a CJK/symbol
//! numeral system — is rendered as a `BulletList` whose items bake the exact
//! Typst-computed marker as a leading `Str` (NEVER a malformed `OrderedList`).

use comemo::Track;
use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Context, Packed, Smart, StyleChain};
use typst_library::model::{EnumElem, EnumItem, ListElem, Numbering};
use typst_syntax::Span;

use crate::ast::{Block, Inline, ListAttributes, ListNumberDelim, ListNumberStyle};
use crate::ctx::PandocCtx;

// ===========================================================================
// Bullet lists.
// ===========================================================================

/// Lowers a `ListElem` (bullets) into `Block::BulletList([[Block]])`.
pub fn list(
    elem: &Packed<ListElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let tight = elem.tight.get(styles);

    let mut items: Vec<Vec<Block>> = Vec::with_capacity(elem.children.len());
    for child in &elem.children {
        items.push(item_blocks(ctx, &child.body, styles, tight)?);
    }

    Ok(vec![Block::BulletList(items)])
}

// ===========================================================================
// Enumerations.
// ===========================================================================

/// Lowers an `EnumElem` into `Block::OrderedList(ListAttributes, [[Block]])`
/// when the numbering is natively representable, or into a marker-baked
/// `BulletList` otherwise.
pub fn enum_(
    elem: &Packed<EnumElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let tight = elem.tight.get(styles);
    let reversed = elem.reversed.get(styles);
    let start = elem.start.get(styles);
    let full = elem.full.get(styles);
    let numbering = elem.numbering.get_ref(styles);

    // The live nesting depth (how many parent enum counters are folded in). For
    // a non-`full` enum the *displayed* segment is the `parents.len()`-th piece
    // of the pattern; that is the piece we must classify.
    let depth = styles.get_cloned(EnumElem::parents).len();

    // Explicit per-item numbers cannot be expressed by pandoc's monotonic
    // counter (it only knows a single `start`); `reversed` likewise; `full` (the
    // `1.2.a` ancestry form) has no native OrderedList analogue. All three take
    // the marker-baked static fallback.
    let has_explicit_numbers =
        elem.children.iter().any(|c| c.number.get(styles).is_custom());
    let native = if reversed || has_explicit_numbers || full {
        None
    } else {
        native_list_attributes(ctx, numbering, depth, start)
    };

    let Some(attrs) = native else {
        return enum_static_fallback(elem, styles, ctx, tight);
    };

    let mut items: Vec<Vec<Block>> = Vec::with_capacity(elem.children.len());
    for child in &elem.children {
        items.push(item_blocks(ctx, &child.body, styles, tight)?);
    }

    Ok(vec![Block::OrderedList(attrs, items)])
}

/// Classifies an enum's numbering pattern into pandoc [`ListAttributes`]
/// (`start`, [`ListNumberStyle`], [`ListNumberDelim`]) by *rendering* the
/// pattern's displayed piece for a handful of numbers and matching the
/// glyphs — this needs no `codex`/`NamedNumeralSystem` re-export, only the
/// public `NumberingPattern::apply_kth`.
///
/// Returns `None` for a closure, a non-empty literal prefix, an unrepresentable
/// delimiter, or any numeral system outside the closed pandoc family
/// (CJK/symbol/abjad/…), signalling the caller to bake the marker statically.
fn native_list_attributes(
    ctx: &mut PandocCtx,
    numbering: &Numbering,
    depth: usize,
    start: Smart<u64>,
) -> Option<ListAttributes> {
    let Numbering::Pattern(pattern) = numbering else {
        return None;
    };
    if pattern.pieces() == 0 {
        return None;
    }

    // `apply_kth(k, n)` renders `prefix + symbol(n) + suffix` for the k-th
    // displayed piece (k = `depth` for a non-`full` enum). Render for 1..=5 and
    // strip the constant prefix/suffix to recover the bare numeral, then
    // classify the family and the trailing delimiter off the glyphs.
    let span = Span::detached();
    let engine = ctx.engine();
    let rendered: Vec<EcoString> = (1u64..=5)
        .map(|n| pattern.apply_kth(engine, span, depth, n))
        .collect();

    let (style, delim) = classify(&rendered)?;
    let start = start.unwrap_or(1).min(i32::MAX as u64) as i32;
    Some((start, style, delim))
}

/// Classifies rendered markers (for numbers 1..=5) into a pandoc number style +
/// delimiter. The markers share a constant prefix and suffix (the pattern's
/// literal text); the varying middle is the numeral glyph.
fn classify(rendered: &[EcoString]) -> Option<(ListNumberStyle, ListNumberDelim)> {
    if rendered.len() < 5 {
        return None;
    }
    let prefix = common_prefix(rendered);
    let suffix = common_suffix(rendered, prefix.len());

    // Reject a non-empty leading literal (e.g. `step 1.`): pandoc's native
    // OrderedList cannot carry a prefix, so bake it statically instead.
    if !prefix.is_empty() {
        return None;
    }

    // The bare numeral for each of 1..=5.
    let numerals: Vec<&str> =
        rendered.iter().map(|s| &s[..s.len() - suffix.len()]).collect();

    // Classify the family. Matching the full 1..=5 sequence (not just 1 & 4)
    // rejects look-alike custom systems and guarantees a true monotonic family.
    let style = if numerals == ["1", "2", "3", "4", "5"] {
        ListNumberStyle::Decimal
    } else if numerals == ["a", "b", "c", "d", "e"] {
        ListNumberStyle::LowerAlpha
    } else if numerals == ["A", "B", "C", "D", "E"] {
        ListNumberStyle::UpperAlpha
    } else if numerals == ["i", "ii", "iii", "iv", "v"] {
        ListNumberStyle::LowerRoman
    } else if numerals == ["I", "II", "III", "IV", "V"] {
        ListNumberStyle::UpperRoman
    } else {
        // Symbol, CJK, abjad, Hebrew, Greek, … — no pandoc style.
        return None;
    };

    // The delimiter is the suffix. Typst's `(1)` form is prefix `(` + suffix
    // `)`, which we already reject above (non-empty prefix) and route to the
    // static path, so only `.`/`)`/none survive here.
    let delim = match suffix {
        "." => ListNumberDelim::Period,
        ")" => ListNumberDelim::OneParen,
        "" => ListNumberDelim::DefaultDelim,
        // `:`, `>`, `-`, … have no native delim → bake statically.
        _ => return None,
    };

    Some((style, delim))
}

/// The longest common prefix (on a char boundary) of all rendered markers.
fn common_prefix(rendered: &[EcoString]) -> String {
    let first = rendered[0].as_str();
    let mut len = first.len();
    for s in &rendered[1..] {
        len = len.min(common_prefix_len(first, s));
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    first[..len].to_string()
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// The longest common suffix (on a char boundary) of all markers, bounded so it
/// never overlaps the already-stripped prefix.
fn common_suffix(rendered: &[EcoString], prefix_len: usize) -> &str {
    let first = rendered[0].as_str();
    let mut len = first.len() - prefix_len;
    for s in &rendered[1..] {
        let cap = s.len().saturating_sub(prefix_len);
        len = len.min(cap.min(common_suffix_len(first, s)));
    }
    let mut start = first.len() - len;
    while !first.is_char_boundary(start) {
        start += 1;
    }
    &first[start..]
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    a.bytes()
        .rev()
        .zip(b.bytes().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

/// Renders an enum's items as a `BulletList` whose first text block carries the
/// exact Typst-computed marker baked in as a leading `Str` (plus a `Space`).
/// Used for closures, `reversed`, explicit per-item numbers, the `full` form,
/// a literal prefix, and any numeral system / delimiter pandoc cannot express
/// natively. Never emits a (malformed) `OrderedList`.
fn enum_static_fallback(
    elem: &Packed<EnumElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
    tight: bool,
) -> SourceResult<Vec<Block>> {
    let parents = styles.get_cloned(EnumElem::parents);
    let numbering = elem.numbering.get_ref(styles).clone();
    let reversed = elem.reversed.get(styles);
    let full = elem.full.get(styles);

    let mut number = elem.start.get(styles).unwrap_or(if reversed {
        elem.children.len() as u64
    } else {
        1
    });

    let mut items: Vec<Vec<Block>> = Vec::with_capacity(elem.children.len());
    for child in &elem.children {
        number = child.number.get(styles).unwrap_or(number);

        let marker =
            render_marker(ctx, styles, &numbering, &parents, number, full, child)?;
        let mut blocks = item_blocks(ctx, &child.body, styles, tight)?;
        prepend_marker(&mut blocks, marker);
        items.push(blocks);

        number =
            if reversed { number.saturating_sub(1) } else { number.saturating_add(1) };
    }

    Ok(vec![Block::BulletList(items)])
}

/// Renders the literal marker text for one enum item, mirroring the layout
/// code's pattern application (`typst-layout/src/lists.rs`): `full` shows the
/// whole ancestry, otherwise only this level's counter.
fn render_marker(
    ctx: &mut PandocCtx,
    styles: StyleChain,
    numbering: &Numbering,
    parents: &[u64],
    number: u64,
    full: bool,
    item: &Packed<EnumItem>,
) -> SourceResult<EcoString> {
    let span = item.span();
    let engine = ctx.engine();
    if full {
        let mut nums: Vec<u64> = parents.to_vec();
        nums.push(number);
        let context = Context::new(None, Some(styles));
        let value = numbering.apply(engine, context.track(), span, &nums)?;
        Ok(value.display().plain_text())
    } else {
        match numbering {
            Numbering::Pattern(pattern) => {
                Ok(pattern.apply_kth(engine, span, parents.len(), number))
            }
            other => {
                let context = Context::new(None, Some(styles));
                let value = other.apply(engine, context.track(), span, &[number])?;
                Ok(value.display().plain_text())
            }
        }
    }
}

/// Prepends the baked marker (followed by a `Space`) to an item's first
/// `Plain`/`Para`. If the item has no leading text block, a fresh `Plain`
/// carrying just the marker leads.
fn prepend_marker(blocks: &mut Vec<Block>, marker: EcoString) {
    match blocks.first_mut() {
        Some(Block::Plain(inlines)) | Some(Block::Para(inlines)) => {
            inlines.splice(0..0, [Inline::Str(marker.into()), Inline::Space]);
        }
        _ => {
            blocks
                .insert(0, Block::Plain(vec![Inline::Str(marker.into()), Inline::Space]));
        }
    }
}

// ===========================================================================
// Shared item emission.
// ===========================================================================

/// Re-realizes an item body into a block list, applying tightness: in a tight
/// list a body that lowered to `Para`s is collapsed to `Plain`s so pandoc's
/// writers render it without inter-item paragraph spacing. A loose list keeps
/// the `Para`. Nested lists / non-paragraph blocks pass through unchanged.
fn item_blocks(
    ctx: &mut PandocCtx,
    body: &Content,
    styles: StyleChain,
    tight: bool,
) -> SourceResult<Vec<Block>> {
    let mut blocks = crate::convert::blocks(ctx, body, styles)?;
    if tight {
        tighten(&mut blocks);
    }
    Ok(blocks)
}

/// Collapses top-level `Para`s into `Plain`s for a tight item, matching what
/// `pandoc -f markdown` produces for a tight list. Nested lists, block quotes,
/// code blocks, tables, etc. are left untouched.
fn tighten(blocks: &mut [Block]) {
    for block in blocks {
        if let Block::Para(inlines) = block {
            let inlines = std::mem::take(inlines);
            *block = Block::Plain(inlines);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecow::EcoString;

    fn markers(strs: &[&str]) -> Vec<EcoString> {
        strs.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn classify_decimal_period() {
        let r = markers(&["1.", "2.", "3.", "4.", "5."]);
        assert!(matches!(
            classify(&r),
            Some((ListNumberStyle::Decimal, ListNumberDelim::Period))
        ));
    }

    #[test]
    fn classify_lower_alpha_oneparen() {
        let r = markers(&["a)", "b)", "c)", "d)", "e)"]);
        assert!(matches!(
            classify(&r),
            Some((ListNumberStyle::LowerAlpha, ListNumberDelim::OneParen))
        ));
    }

    #[test]
    fn classify_roman() {
        let lower = markers(&["i.", "ii.", "iii.", "iv.", "v."]);
        assert!(matches!(
            classify(&lower),
            Some((ListNumberStyle::LowerRoman, ListNumberDelim::Period))
        ));
        let upper = markers(&["I.", "II.", "III.", "IV.", "V."]);
        assert!(matches!(
            classify(&upper),
            Some((ListNumberStyle::UpperRoman, ListNumberDelim::Period))
        ));
    }

    #[test]
    fn classify_no_delim() {
        let r = markers(&["1", "2", "3", "4", "5"]);
        assert!(matches!(
            classify(&r),
            Some((ListNumberStyle::Decimal, ListNumberDelim::DefaultDelim))
        ));
    }

    #[test]
    fn reject_literal_prefix() {
        // `(1)` form: prefix `(` is non-empty → not native.
        let r = markers(&["(1)", "(2)", "(3)", "(4)", "(5)"]);
        assert!(classify(&r).is_none());
        // Word prefix.
        let r = markers(&["step 1.", "step 2.", "step 3.", "step 4.", "step 5."]);
        assert!(classify(&r).is_none());
    }

    #[test]
    fn reject_unknown_delim_and_system() {
        // Unrepresentable delimiter.
        let r = markers(&["1:", "2:", "3:", "4:", "5:"]);
        assert!(classify(&r).is_none());
        // Non-closed numeral system (e.g. CJK) — not a recognized family.
        let r = markers(&["一.", "二.", "三.", "四.", "五."]);
        assert!(classify(&r).is_none());
    }

    #[test]
    fn prepend_marker_keeps_space() {
        let mut blocks = vec![Block::Plain(vec![Inline::Str("body".into())])];
        prepend_marker(&mut blocks, "1)".into());
        match &blocks[0] {
            Block::Plain(inlines) => {
                assert!(matches!(inlines[0], Inline::Str(ref s) if s == "1)"));
                assert!(matches!(inlines[1], Inline::Space));
                assert!(matches!(inlines[2], Inline::Str(ref s) if s == "body"));
            }
            _ => panic!("expected Plain"),
        }
    }

    #[test]
    fn prepend_marker_empty_body() {
        let mut blocks: Vec<Block> = Vec::new();
        prepend_marker(&mut blocks, "a.".into());
        assert!(matches!(&blocks[0], Block::Plain(i) if i.len() == 2));
    }

    #[test]
    fn tighten_collapses_para() {
        let mut blocks = vec![Block::Para(vec![Inline::Str("x".into())])];
        tighten(&mut blocks);
        assert!(matches!(blocks[0], Block::Plain(_)));
    }
}

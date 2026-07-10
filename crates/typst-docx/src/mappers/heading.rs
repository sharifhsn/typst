//! Heading mapper: `HeadingElem` → a `Heading{N}`-styled `w:p`.
//!
//! Emits an idiomatic Word heading paragraph:
//! - `w:pStyle="Heading{level}"` (1..=9 use the built-in magic names; deeper
//!   levels use a custom `Heading{level}` style — see the INTEGRATION-NEEDED note
//!   below for the `styles.xml` side),
//! - `w:keepNext` + `w:outlineLvl` (0-based, clamped to 8) so Word's Navigation
//!   pane and TOC field pick the heading up,
//! - the resolved heading number (the synthesized `numbers` field) as a leading
//!   run followed by a tab, mirroring how Word renders numbered headings,
//! - a `w:bookmarkStart`/`w:bookmarkEnd` pair wrapping the heading content so
//!   `@ref`/`PAGEREF` fields can target it (gated on `bookmarked`/`outlined`).
//!
//! See `research/styles.md` §6,7 and `map/model_elements.md` §1.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::HeadingElem;

use crate::ctx::DocxCtx;
use crate::dom::{Block, Para, ParaChild, ParaProps, Run, RunProps};

pub fn heading(
    elem: &Packed<HeadingElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    // Absolute nesting level (folds offset + depth). Always correct, even after
    // synthesis sets `level` to `Custom`.
    let level = elem.resolve_level(styles).get();
    let level_u8 = level.min(u8::MAX as usize) as u8;

    // Record the deepest level so `styles.xml` can generate `Heading1..N`.
    ctx.note_heading_level(level_u8);
    ctx.note_heading_style(level_u8, ctx.resolve_text_props(styles, RunProps::default()));

    // -- Paragraph properties --------------------------------------------------
    // Levels 1..=9 map to the built-in `Heading1..Heading9` magic styles; deeper
    // levels fall back to a custom `Heading{level}` style id.
    //
    // INTEGRATION-NEEDED: `styles_part.rs` must define a `Heading{level}` style
    // for every `level <= ctx.max_heading_level`, using the magic `w:name
    // w:val="heading {level}"` for 1..=9 (so the Navigation pane / TOC pick them
    // up) and a `w:customStyle="1"` style `basedOn` Heading9 for level >= 10.
    // This mapper only references the style id; it cannot emit the style itself.
    let props = ParaProps {
        style: Some(ecow::eco_format!("Heading{level}")),
        // Keep the heading with the paragraph that follows it.
        keep_next: true,
        // Outline level is 0-based and valid only for 0..=8; clamp deeper headings.
        outline_lvl: Some(level_u8.saturating_sub(1).min(8)),
        ..Default::default()
    };

    // -- Heading content -------------------------------------------------------
    let mut content: Vec<ParaChild> = Vec::new();

    // Whether to emit a bookmark for cross-references. Mirrors the PDF outline
    // gating: an explicit `bookmarked` wins, otherwise fall back to `outlined`.
    let bookmarked = elem
        .bookmarked
        .get(styles)
        .unwrap_or_else(|| elem.outlined.get(styles));
    let bookmark =
        if bookmarked { elem.location().map(|loc| ctx.add_bookmark(loc)) } else { None };

    if let Some((id, ref name)) = bookmark {
        content.push(ParaChild::BookmarkStart { id, name: name.clone() });
    }

    // Prepend the resolved heading number (e.g. "1.2.3") as a leading run plus a
    // tab, matching Word's idiom for numbered headings. `numbers` is the same
    // synthesized field PDF bookmarks use; it is `Some` only when a `numbering`
    // pattern is set and the heading is located.
    if let Some(numbers) = &elem.numbers
        && !numbers.is_empty()
    {
        content.push(ParaChild::Run(Run::Text {
            props: RunProps::default(),
            text: numbers.clone(),
        }));
        content.push(ParaChild::Run(Run::Tab));
    }

    // The heading title itself, lowered to flattened runs.
    let runs = ctx.inline_runs(&elem.body, styles, RunProps::default())?;
    content.extend(runs.into_iter().map(ParaChild::Run));

    if let Some((id, _)) = bookmark {
        content.push(ParaChild::BookmarkEnd { id });
    }

    // Record this heading (with the bookmark it actually emitted) so a table of
    // contents can list it later. Honour `outlined` — a heading excluded from
    // the outline must not appear in the TOC.
    if elem.outlined.get(styles) {
        let mut text = String::new();
        if let Some(numbers) = &elem.numbers
            && !numbers.is_empty()
        {
            text.push_str(numbers);
            text.push(' ');
        }
        text.push_str(&elem.body.plain_text());
        if !text.is_empty() {
            ctx.toc_headings.push(crate::dom::TocHeading {
                level,
                location: elem.location(),
                anchor: bookmark.map(|(_, name)| name),
                text: text.into(),
            });
        }
    }

    Ok(vec![Block::Para(Para { props, content })])
}

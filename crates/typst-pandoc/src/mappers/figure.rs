//! Figure mapper: `FigureElem` → `Figure(Attr, Caption, [Block])`.
//!
//! Lowers a `FigureElem` to Pandoc's native `Figure` node:
//! - the figure body (image / table / arbitrary content) becomes the figure's
//!   `[Block]` content, lowered through `convert::blocks` so a contained image,
//!   table, etc. is handled by its own mapper (and a non-representable body
//!   falls through to the shared rasterize fallback, yielding a `Para[Image]`);
//! - the caption becomes a `Caption(None, [Block])` built from the caption
//!   *body* only — we DROP the baked "Figure N:" supplement/number/separator,
//!   because the downstream writer owns figure numbering (`\caption` + SEQ).
//!   The short-caption slot is always `null`.
//! - the figure's `id` (from its `Location`, via `ctx.anchor_id`) is attached to
//!   the figure `Attr`, so a `#ref`/`#id`-`Link` to the figure resolves through
//!   the shared id namespace (the writer turns it into a `\label`/anchor).
//!
//! Lost (accepted): caption position (top vs bottom — Pandoc has no slot for it,
//! the writer places the caption per its own convention), float/placement, and
//! the per-kind supplement text.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::FigureElem;

use crate::ast::{Block, Caption};
use crate::convert::blocks;
use crate::ctx::PandocCtx;

/// `FigureElem` → `Figure(Attr{id}, Caption, [Block])`.
pub fn figure(
    elem: &Packed<FigureElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    // The figure's `id`: the anchor a `#ref`/`#id`-`Link` targets. Derived from
    // the location through the shared id namespace so it matches what the
    // heading/link/citation mappers emit. (No location → no anchor.)
    let attr = match elem.location() {
        Some(loc) => crate::ast::id_attr(ctx.anchor_id(loc).to_string()),
        None => crate::ast::empty_attr(),
    };

    // The caption body — WITHOUT the supplement / number / separator (the writer
    // owns numbering). `FigureCaption::body` is the user's caption content; we
    // lower it to blocks (typically a single `Para`). No caption → empty caption.
    let caption_blocks = match elem.caption.get_cloned(styles) {
        Some(cap) => blocks(ctx, &cap.body, styles)?,
        None => Vec::new(),
    };
    let caption = Caption(None, caption_blocks);

    // The figure body: an image, a table, or arbitrary content. Lowered through
    // the normal block recursion so each sub-element hits its own mapper; a body
    // with no native Pandoc form rasterizes (shared fallback → `Para[Image]`),
    // which is exactly what "wrap the Image in the Figure" requires.
    let body = blocks(ctx, &elem.body, styles)?;

    Ok(vec![Block::Figure(attr, caption, body)])
}

//! Quote mapper: block `QuoteElem` → `BlockQuote [Block]`.
//!
//! Implemented. A block quote (`block: true`) lowers its body to blocks wrapped
//! in a `Block::BlockQuote`; if it carries an attribution, the realized
//! attribution ("— author", or a prose citation for a `<label>`) is appended as
//! a trailing `Para` *inside* the same `BlockQuote` (Pandoc has no attribution
//! node). An inline quote (`block: false`) has no block structure of its own —
//! it emits just its body blocks (the smart quote marks are handled by the
//! `SmartQuoteElem` path in core, mirroring the DOCX backend, which likewise
//! does not re-wrap the body).
//
// NOTE: `QUOTE_RULE` is intentionally NOT registered for `Target::Pandoc` (see
// `typst-layout/src/rules.rs`), so the raw `QuoteElem` reaches this mapper
// intact — exactly like the DOCX target. Lost vs. paged: the 1em block-quote
// pad and the right-alignment of the attribution (Pandoc has no per-block
// indent/alignment node).

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::QuoteElem;

use crate::ast::{Block, Inline};
use crate::convert::blocks;
use crate::ctx::PandocCtx;

pub fn quote(
    elem: &Packed<QuoteElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let block = elem.block.get(styles);

    // Lower the quote body to blocks.
    let mut body = blocks(ctx, &elem.body, styles)?;

    if !block {
        // An inline quote has no block wrapper of its own: emit its body blocks
        // directly. (Lowering an inline body through `blocks` yields a single
        // `Para` of the inline content.)
        return Ok(body);
    }

    // The attribution ("— author", or a prose citation for a `<label>`) renders
    // below a block quote. Pandoc has no attribution node, so append it as a
    // trailing `Para` inside the `BlockQuote`.
    if let Some(attribution) = elem.attribution.get_cloned(styles) {
        let realized = attribution.realize(elem.span());
        let mut inlines: Vec<Inline> = Vec::new();
        crate::convert::inline_into(ctx, &realized, styles, &mut inlines)?;
        if !inlines.is_empty() {
            body.push(Block::Para(inlines));
        }
    }

    Ok(vec![Block::BlockQuote(body)])
}

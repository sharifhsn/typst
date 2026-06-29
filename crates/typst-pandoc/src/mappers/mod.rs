//! The per-element mapper modules. Each lowers one native element family into
//! the Pandoc AST. The foundation provides compiling stubs (rasterize-fallback
//! or minimal emission, marked `// TODO(phase2)`); phase-2 mapper agents fill
//! exactly one module each, keeping these public handler signatures identical.
//!
//! Shared helpers live on [`crate::ctx::PandocCtx`]: `rasterize` (the universal
//! image fallback returning a data-URI), `add_image`, `anchor_id` (the shared
//! id namespace for headings/figures/links — load-bearing), `next_locator`,
//! `engine`, `warn_ignored`, and the `deferred_tags` discipline. The block/
//! inline recursion entry points a mapper needs are [`crate::convert::blocks`],
//! [`crate::convert::inline_into`], and [`crate::convert::handle_inline`].

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, StyleChain};

use crate::ast::{Block, Inline};
use crate::ctx::PandocCtx;

/// The universal block-level rasterize fallback shared by stub mappers: lay the
/// element out, embed it as a data-URI `Image` in a `Para`, or drop it (with the
/// frame-tag harvest already preserving any inner labels/refs for convergence).
/// Phase-2 mappers replace their call with a structural emission.
pub(crate) fn rasterize_block(
    content: &Content,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    if let Some((url, _size)) = ctx.rasterize(content, styles, content.span())? {
        Ok(vec![Block::Para(vec![Inline::Image(
            crate::ast::empty_attr(),
            Vec::new(),
            (url.to_string(), String::new()),
        )])])
    } else {
        Ok(Vec::new())
    }
}

/// The universal inline-level rasterize fallback: lay the element out and embed
/// it as a data-URI `Image` inline, or drop it.
pub(crate) fn rasterize_inline(
    content: &Content,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    if let Some((url, _size)) = ctx.rasterize(content, styles, content.span())? {
        Ok(vec![Inline::Image(
            crate::ast::empty_attr(),
            Vec::new(),
            (url.to_string(), String::new()),
        )])
    } else {
        Ok(Vec::new())
    }
}

pub mod citation;
pub mod code;
pub mod figure;
pub mod footnote;
pub mod image;
pub mod link;
pub mod list;
pub mod math;
mod math_latex;
pub mod outline;
pub mod quote;
pub mod table;
pub mod terms;

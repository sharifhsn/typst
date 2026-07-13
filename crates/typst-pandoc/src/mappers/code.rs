//! Raw/code mapper: `RawElem` → `Code` (inline) / `CodeBlock` (block).
//!
//! Implemented (CLEAN): the raw text becomes a `Code`/`CodeBlock` literal, with
//! the language (if any) as the single `Attr` class — what pandoc's writers use
//! for syntax highlighting.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::text::{RawContent, RawElem};

use crate::ast::{Attr, Block, Inline};
use crate::ctx::PandocCtx;

/// The raw element's text, joining `Lines` with newlines.
fn raw_text(elem: &Packed<RawElem>) -> String {
    match &elem.text {
        RawContent::Text(t) => t.to_string(),
        RawContent::Lines(lines) => {
            lines.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join("\n")
        }
    }
}

/// The `Attr` carrying the language as a class, if set.
fn raw_attr(elem: &Packed<RawElem>, styles: StyleChain) -> Attr {
    match elem.lang.get_ref(styles) {
        Some(lang) if !lang.is_empty() => {
            let l: EcoString = lang.clone();
            crate::ast::class_attr(l.to_string())
        }
        _ => crate::ast::empty_attr(),
    }
}

pub fn raw_inline(
    elem: &Packed<RawElem>,
    styles: StyleChain,
    _ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    Ok(vec![Inline::Code(raw_attr(elem, styles), raw_text(elem))])
}

pub fn raw_block(
    elem: &Packed<RawElem>,
    styles: StyleChain,
    _ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    Ok(vec![Block::CodeBlock(raw_attr(elem, styles), raw_text(elem))])
}

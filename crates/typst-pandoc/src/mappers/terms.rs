//! Terms mapper: `TermsElem` → `DefinitionList`.
//!
//! `DefinitionList [([Inline] term, [[Block]] definitions)]`. Each `TermItem`
//! becomes one `(term inlines, [definition blocks])` pair — pandoc's native
//! definition list, a clean structural match (strictly better than the DOCX
//! fake, which had no def-list node). Per-item tightness (`tight`) and the
//! `separator`/`indent`/`hanging-indent` styling are lost (pandoc def-lists
//! carry no such attributes); the writer owns def-list rendering.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::TermsElem;

use crate::ast::{Block, Inline};
use crate::ctx::PandocCtx;

pub fn terms(
    elem: &Packed<TermsElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let mut items: Vec<(Vec<Inline>, Vec<Vec<Block>>)> =
        Vec::with_capacity(elem.children.len());

    for item in &elem.children {
        // Term: an inline body (lowered via Par realization).
        let mut term: Vec<Inline> = Vec::new();
        crate::convert::inline_into(ctx, &item.term, styles, &mut term)?;

        // Description: a block body. Pandoc allows a *list* of definitions per
        // term (`[[Block]]`); a Typst term item has exactly one description, so
        // we emit a single definition block-list.
        let def = crate::convert::blocks(ctx, &item.description, styles)?;

        items.push((term, vec![def]));
    }

    Ok(vec![Block::DefinitionList(items)])
}

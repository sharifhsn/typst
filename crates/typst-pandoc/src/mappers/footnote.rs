//! Footnote mapper: `FootnoteElem` → `Note [Block]` (body inlined at call site).
//!
//! Implemented (CLEAN): the footnote body is lowered to blocks and wrapped in a
//! pandoc `Note`. A `Note` body MUST be `[Block]` (a bare `[Inline]` hard-fails
//! pandoc's reader, `exit 64`) — [`crate::convert::blocks`] guarantees that, so
//! an inline-only body comes back as a single `Para` and the shape is valid.
//!
//! Re-referenced footnotes (`#footnote(<lbl>)`) cannot dedup: pandoc's `Note`
//! carries no shared id, so two reference sites can't point at one body. Instead
//! of dropping the re-reference to an empty note (visibly losing the text), we
//! resolve the *declaring* footnote's body and DUPLICATE it at each site. The
//! duplicated body renders identically; only the auto-assigned footnote *number*
//! diverges from the original (the writer renumbers), which is an accepted loss.
//!
//! Convergence: the body is always lowered through [`crate::convert::blocks`],
//! which walks the realized tree and pushes every introspection `TagElem` into
//! `ctx.deferred_tags`. A reference footnote that cannot yet be resolved (its
//! declaring label is not in the introspector during an early stabilization
//! pass) yields an empty note for that pass rather than erroring — the final
//! pass resolves it. Hard-failing here would abort the export before the
//! introspector learns the label (the classic co-first-author `#footnote(<lbl>)`
//! pattern in journal author blocks).

use typst_library::foundations::{Content, Packed, Selector, StyleChain};
use typst_library::introspection::QueryFirstIntrospection;
use typst_library::diag::SourceResult;
use typst_library::model::FootnoteElem;

use crate::ast::Inline;
use crate::convert::blocks;
use crate::ctx::PandocCtx;

/// Lowers a `FootnoteElem` reference site into an inline `Note` whose body is the
/// realized footnote content lowered to blocks.
pub fn footnote(
    elem: &Packed<FootnoteElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Inline> {
    // Resolve the body: a content footnote holds it directly; a reference
    // footnote (`#footnote(<lbl>)`) is chased to its declaring footnote so its
    // text is duplicated rather than lost. An unresolvable reference (early
    // introspection pass) gives `None` → an empty (but well-formed) note.
    let body_blocks = match resolve_body(elem, ctx)? {
        Some(content) => blocks(ctx, &content, styles)?,
        None => Vec::new(),
    };
    Ok(Inline::Note(body_blocks))
}

/// Returns this footnote's body content, chasing a reference footnote to its
/// declaration. `None` when a reference cannot (yet) be located.
fn resolve_body(
    elem: &Packed<FootnoteElem>,
    ctx: &mut PandocCtx,
) -> SourceResult<Option<Content>> {
    // Content footnote: body is held directly on this element.
    if let Some(content) = elem.body_content() {
        return Ok(Some(content.clone()));
    }

    // Reference footnote: locate the declaring footnote and read its body. The
    // declaration may be unresolvable during an early introspection pass — return
    // `None` (empty note this pass) rather than hard-failing so convergence can
    // proceed and the introspector learns the label for the next iteration.
    let Ok(decl) = elem.declaration_location(ctx.engine()) else {
        return Ok(None);
    };
    let selector = Selector::Location(decl);
    let body = ctx
        .engine()
        .introspect(QueryFirstIntrospection(selector, elem.span()))
        .as_ref()
        .and_then(|c| c.to_packed::<FootnoteElem>())
        .and_then(|note| note.body_content().cloned());
    Ok(body)
}

#[cfg(test)]
mod tests {
    use crate::ast::{Block, Inline, PANDOC_API_VERSION};

    /// Hand-construct the JSON shape this mapper emits for a content footnote and
    /// assert it serializes to the pandoc-accepted `Note [Para [...]]` form.
    ///
    /// The full pipeline (realize → lower) is exercised by the integration suite;
    /// this guards the AST shape the mapper is responsible for — specifically
    /// that a `Note` wraps `[Block]` (never bare `[Inline]`, which pandoc rejects
    /// with `exit 64`).
    #[test]
    fn note_wraps_blocks() {
        let note = Inline::Note(vec![Block::Para(vec![
            Inline::Str("The".into()),
            Inline::Space,
            Inline::Str("body.".into()),
        ])]);
        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(json["t"], "Note");
        // The body must be a list of blocks; the first is a Para.
        assert_eq!(json["c"][0]["t"], "Para");
        // Sanity: the api version constant is the one pandoc 3.x expects.
        assert_eq!(PANDOC_API_VERSION, [1, 23, 1, 1]);
    }

    /// An unresolvable reference footnote yields an empty — but well-formed —
    /// note (`Note []`), which pandoc accepts.
    #[test]
    fn empty_note_is_valid_shape() {
        let note = Inline::Note(Vec::new());
        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(json["t"], "Note");
        assert!(json["c"].as_array().unwrap().is_empty());
    }
}

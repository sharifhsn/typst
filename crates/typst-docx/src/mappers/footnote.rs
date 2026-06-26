//! Footnote mapper — lowers `FootnoteElem` into an inline reference run plus a
//! registered footnote body routed to `word/footnotes.xml`.
//!
//! Model (see `research/footnotes.md`): a footnote is a *separate text story*.
//! Two physically separated pieces share one integer `w:id`:
//!
//!  1. the **reference mark** — `<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/>
//!     </w:rPr><w:footnoteReference w:id="N"/></w:r>` — sits inline in
//!     `document.xml` where the `FootnoteElem` appears in the body flow;
//!  2. the **body** — `<w:footnote w:id="N">…</w:footnote>` — lives in
//!     `footnotes.xml` and holds the realized block content.
//!
//! `N` is allocated by [`DocxCtx::add_footnote`], keyed on the footnote's
//! *declaration* `Location` so a re-referenced footnote (`#footnote(<lbl>)`)
//! reuses the same id and the body is emitted only once. Word renders the
//! displayed number automatically (via the in-body `<w:footnoteRef/>` mark and
//! document order of the reference marks), so Typst's footnote counter / style
//! is intentionally discarded in favor of Word's auto-numbering.

use typst_library::diag::{At, SourceResult};
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::introspection::QueryFirstIntrospection;
use typst_library::model::FootnoteElem;

use crate::ctx::DocxCtx;
use crate::dom::{Block, Run, RunProps};

/// The conventional Word character-style id for the superscript footnote mark.
/// It is defined in `styles_part.rs` and applied to BOTH the inline
/// `w:footnoteReference` run and the in-body `w:footnoteRef` run.
const FOOTNOTE_REFERENCE_STYLE: &str = "FootnoteReference";

/// The conventional Word paragraph-style id for footnote body text.
const FOOTNOTE_TEXT_STYLE: &str = "FootnoteText";

/// Lowers a `FootnoteElem` reference site into the inline reference run, after
/// registering (once, deduplicated by declaration location) the footnote body.
pub fn footnote(
    elem: &Packed<FootnoteElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Run> {
    let span = elem.span();

    // The canonical declaration location is the linking + dedup key. For a
    // content footnote this is the element's own location; for a reference
    // footnote (`#footnote(<lbl>)`) it resolves to the declaring footnote, so
    // both sites share one `w:id` and one body in `footnotes.xml`.
    // For a reference footnote the declaration may be unresolvable during an
    // early introspection-stabilization pass; fall back to the element's own
    // location so the pass does not error (the final pass resolves correctly).
    let decl = elem
        .declaration_location(ctx.engine())
        .ok()
        .or_else(|| elem.location())
        .ok_or("footnote has no location")
        .at(span)?;

    // Lower the declaration's body into block content. `add_footnote` dedups by
    // `decl`: for a re-reference it returns the existing id and discards these
    // blocks, so the body is serialized exactly once.
    //
    // INTEGRATION-NEEDED (minor perf): `add_footnote` takes the body eagerly,
    // so a re-referenced footnote re-realizes + re-lowers its body only to throw
    // it away. A `DocxCtx::footnote_id(decl) -> Option<i32>` probe (or a closure
    // taking the body lazily) would let us skip that work. Correctness is
    // unaffected — the duplicate body is dropped by the existing dedup map.
    let body_blocks = self::body_blocks(elem, styles, ctx)?;
    let id = ctx.add_footnote(decl, body_blocks);

    // The inline reference mark: a run styled `FootnoteReference` (superscript)
    // carrying `<w:footnoteReference w:id="N"/>`. The encoder emits the
    // `<w:r><w:rPr>…</w:rPr>` wrapper for `Run::FootnoteRef`.
    let props = RunProps {
        style: Some(FOOTNOTE_REFERENCE_STYLE.into()),
        ..RunProps::default()
    };
    Ok(Run::FootnoteRef { props, id })
}

/// Resolves the footnote's body content and lowers it into block content for
/// `footnotes.xml`, applying the `FootnoteText` paragraph style.
fn body_blocks(
    elem: &Packed<FootnoteElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let body = resolve_body(elem, ctx)?;

    let mut blocks = match body {
        Some(content) => ctx.blocks(&content, styles)?,
        // A reference whose target could not be resolved, or an empty footnote:
        // emit a single empty footnote-text paragraph so the `w:id` still has a
        // matching, well-formed `w:footnote` body (a dangling id is the #1 cause
        // of a Word "repair" prompt).
        None => vec![Block::Para(crate::dom::Para {
            props: crate::dom::ParaProps::default(),
            content: Vec::new(),
        })],
    };

    // Apply the `FootnoteText` paragraph style to every body paragraph that does
    // not already carry an explicit style (e.g. a list item keeps its style).
    for block in &mut blocks {
        if let Block::Para(para) = block
            && para.props.style.is_none() {
                para.props.style = Some(FOOTNOTE_TEXT_STYLE.into());
            }
    }

    // INTEGRATION-NEEDED (`encode.rs::write_footnote` / IR): Word renders the
    // footnote's auto-number via a leading `<w:r><w:rPr><w:rStyle
    // w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r>` mark inside the
    // FIRST body paragraph. The current IR cannot express the bare
    // `<w:footnoteRef/>` element: `Run::FootnoteRef` serializes
    // `<w:footnoteReference w:id>` (the *reference* mark for `document.xml`), and
    // there is no `Run::FootnoteRefMark` / `Para`-prefix slot. The body therefore
    // currently lacks the in-text number. To fix during integration, EITHER:
    //   (a) add a `Run::FootnoteRefMark` (or `Run::Empty { props, name }`) IR
    //       variant emitting `<w:footnoteRef/>`, and prepend it (styled
    //       `FootnoteReference`) to the first paragraph here; OR
    //   (b) have `encode.rs::write_footnote` inject that leading run when it
    //       serializes each `Footnote` (cleanest: the encoder already owns the
    //       `footnoteRef` knowledge for the separator footnotes).
    // The footnote still links + renders without it; only the in-margin number
    // glyph is missing, so this is non-blocking for a valid, openable file.

    Ok(blocks)
}

/// Returns the body content of this footnote's declaration, or `None` if it is a
/// reference whose declaring footnote cannot be located.
fn resolve_body(
    elem: &Packed<FootnoteElem>,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Content>> {
    // Content footnote: the body is held directly on this element.
    if let Some(content) = elem.body_content() {
        return Ok(Some(content.clone()));
    }

    // Reference footnote: locate the declaring footnote and read its body.
    // `declaration_location` already resolved the linking key; query that exact
    // location to fetch the declaring element and its content body.
    let decl = elem.declaration_location(ctx.engine()).at(elem.span())?;
    let selector = typst_library::foundations::Selector::Location(decl);
    let body = ctx
        .engine()
        .introspect(QueryFirstIntrospection(selector, elem.span()))
        .as_ref()
        .and_then(|c| c.to_packed::<FootnoteElem>())
        .and_then(|note| note.body_content().cloned());
    Ok(body)
}

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
//! `N` is allocated by [`DocxCtx::add_footnote`], keyed on the declaration's
//! source-stable semantic identity. The first occurrence is native; a
//! re-reference (`#footnote(<lbl>)`) becomes a bookmarked `NOTEREF` field so it
//! retains the same visible number without creating a second Word footnote
//! occurrence. The body is emitted only once.

use typst_library::diag::{At, SourceResult};
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::introspection::QueryFirstIntrospection;
use typst_library::model::FootnoteElem;

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Field, FieldCacheStatus, FieldDisplay, FieldMode, ParaChild,
    ReviewCandidateKind, Run, RunProps,
};
use crate::report::ExportSource;

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

    // Resolve the declaration element itself and key it by its authored source
    // identity. Locations from the converged paged introspector and the DOCX
    // realization are not interchangeable; the source span is. Using the raw
    // Location here split one logical note into duplicate bodies in gb-ctr.
    let declaration = resolve_declaration(elem, ctx)
        .ok_or("footnote has no declaration")
        .at(span)?;
    let declaration_content = declaration.clone().pack();
    let declaration_id = ExportSource::from_content(&declaration_content).logical_id;

    // Lower the declaration's body into block content. `add_footnote` dedups by
    // stable declaration id: for a re-reference it returns the existing id and
    // discards these blocks, so the body is serialized exactly once.
    //
    // INTEGRATION-NEEDED (minor perf): `add_footnote` takes the body eagerly,
    // so a re-referenced footnote re-realizes + re-lowers its body only to throw
    // it away. A `DocxCtx::footnote_id(decl) -> Option<i32>` probe (or a closure
    // taking the body lazily) would let us skip that work. Correctness is
    // unaffected — the duplicate body is dropped by the existing dedup map.
    let body_blocks = self::body_blocks(&declaration, styles, ctx)?;
    let (id, bookmark_id, bookmark_name, is_first) =
        ctx.add_footnote(declaration_id, body_blocks);

    // The inline reference mark: a run styled `FootnoteReference` (superscript)
    // carrying `<w:footnoteReference w:id="N"/>`. The encoder emits the
    // `<w:r><w:rPr>…</w:rPr>` wrapper for `Run::FootnoteRef`.
    let props = RunProps {
        style: Some(FOOTNOTE_REFERENCE_STYLE.into()),
        ..RunProps::default()
    };
    if is_first {
        Ok(Run::FootnoteRef {
            props,
            id,
            bookmark: Some((bookmark_id, bookmark_name)),
        })
    } else {
        // A second native w:footnoteReference is a second occurrence to Word
        // and LibreOffice, even when it reuses the same w:id; both consumers
        // renumber those occurrences. NOTEREF preserves the Typst meaning: the
        // same visible mark, linked back to the one native footnote.
        Ok(Run::Field(Field {
            instr: ecow::eco_format!(" NOTEREF {bookmark_name} \\h "),
            result: vec![Run::Text { props, text: id.to_string().into() }],
            mode: FieldMode::Static,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::Resolved,
        }))
    }
}

/// Lowers a footnote that appears *inside another footnote's body*. Word forbids
/// nesting footnotes (a `w:footnoteReference` in the footnote story makes the
/// file unopenable), so the inner note's body is emitted inline as ordinary
/// runs — the content survives, just without a separate reference/number.
pub fn flatten_nested(
    elem: &Packed<FootnoteElem>,
    styles: StyleChain,
    props: &RunProps,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    match resolve_body(elem, ctx)? {
        Some(content) => ctx.inline_runs(&content, styles, props.clone()),
        None => Ok(Vec::new()),
    }
}

/// Resolves the footnote's body content and lowers it into block content for
/// `footnotes.xml`, applying the `FootnoteText` paragraph style.
fn body_blocks(
    elem: &Packed<FootnoteElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let body = resolve_body(elem, ctx)?;

    // Lower the body with `in_footnote` set, so any footnote nested inside this
    // one is flattened to inline text rather than emitting an (illegal) nested
    // `w:footnoteReference` into the footnote story.
    let was_in_footnote = ctx.in_footnote;
    ctx.in_footnote = true;
    let lowered = match &body {
        Some(content) => ctx.blocks(content, styles),
        None => Ok(Vec::new()),
    };
    ctx.in_footnote = was_in_footnote;

    let mut blocks = match &body {
        Some(_) => lowered?,
        // A reference whose target could not be resolved, or an empty footnote:
        // emit a single empty footnote-text paragraph so the `w:id` still has a
        // matching, well-formed `w:footnote` body (a dangling id is the #1 cause
        // of a Word "repair" prompt).
        None => vec![Block::Para(crate::dom::Para {
            props: crate::dom::ParaProps::default(),
            content: Vec::new(),
        })],
    };

    if let Some(content) = body.as_ref()
        && blocks.iter().filter(|block| matches!(block, Block::Para(_))).count() == 1
        && let Some(Block::Para(para)) =
            blocks.iter_mut().find(|block| matches!(block, Block::Para(_)))
    {
        para.props.review_origin = Some(ctx.review_origin(
            crate::convert::review_span(content),
            ReviewCandidateKind::Paragraph,
        ));
    }

    // Apply the `FootnoteText` paragraph style to every body paragraph that does
    // not already carry an explicit style (e.g. a list item keeps its style).
    for block in &mut blocks {
        if let Block::Para(para) = block
            && para.props.style.is_none()
        {
            para.props.style = Some(FOOTNOTE_TEXT_STYLE.into());
        }
    }

    // Prepend the auto-number mark (`<w:footnoteRef/>`, styled
    // `FootnoteReference`) to the FIRST body paragraph, followed by a tab, so
    // Word/LibreOffice render the footnote's number next to its text — matching
    // how Word itself writes footnote bodies. Without it the footnote links and
    // its text shows, but the leading number glyph is missing.
    if let Some(Block::Para(first)) =
        blocks.iter_mut().find(|b| matches!(b, Block::Para(_)))
    {
        first.content.insert(0, ParaChild::Run(Run::Tab));
        first.content.insert(0, ParaChild::Run(Run::FootnoteRefMark));
    }

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
    //
    // The reference may be unresolvable during an early introspection pass (the
    // declaring footnote's label is not yet in the introspector). Mirror the
    // tolerance in `footnote()`: return `None` rather than hard-failing, so the
    // pass completes and the introspector learns the label for the next
    // iteration. Hard-failing here aborts the whole export before convergence —
    // exactly the case of a shared footnote like `#footnote(<lbl>)` referencing
    // a `#footnote(..) <lbl>` defined elsewhere (common in journal author
    // blocks for co-first-author marks).
    let Some(decl) = elem.declaration_location(ctx.engine()).ok() else {
        return Ok(None);
    };
    let selector = typst_library::foundations::Selector::Location(decl);
    let body = ctx
        .engine()
        .introspect(QueryFirstIntrospection(selector, elem.span()))
        .as_ref()
        .and_then(|c| c.to_packed::<FootnoteElem>())
        .and_then(|note| note.body_content().cloned());
    Ok(body)
}

/// Returns the authored declaration element for either a content footnote or a
/// `#footnote(<label>)` re-reference.
fn resolve_declaration(
    elem: &Packed<FootnoteElem>,
    ctx: &mut DocxCtx,
) -> Option<Packed<FootnoteElem>> {
    if elem.body_content().is_some() {
        return Some(elem.clone());
    }

    let decl = elem.declaration_location(ctx.engine()).ok()?;
    let selector = typst_library::foundations::Selector::Location(decl);
    ctx.engine()
        .introspect(QueryFirstIntrospection(selector, elem.span()))
        .as_ref()
        .and_then(|content| content.to_packed::<FootnoteElem>())
        .cloned()
}

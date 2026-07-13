//! Link / reference mappers.
//!
//! Implements `LinkElem` and `RefElem` per
//! `fields_links_images_validation.md §1-3` + SPEC `§9`.
//!
//! Strategy:
//! - `LinkElem` with a URL destination → a `<w:hyperlink r:id>` (external
//!   relationship via [`DocxCtx::add_external_rel`]) wrapping the body runs
//!   while preserving Typst's authored run appearance.
//! - `LinkElem` / `RefElem` to an in-document location → a `<w:hyperlink
//!   w:anchor>` (for links) or a `REF`/`PAGEREF` complex field (for refs) that
//!   targets the bookmark the heading/figure mapper registered via
//!   [`DocxCtx::add_bookmark`]. The resolved number/text is emitted as the
//!   field-result fallback so the document reads correctly before Word
//!   recomputes the field.
//! - Bibliography citations (a `RefElem` whose target is not a locatable
//!   element) → the realized citation text, inline, with no field.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::introspection::Location;
use typst_library::model::{Destination, LinkElem, RefElem, RefForm};

use crate::ctx::DocxCtx;
use crate::dom::{
    Field, FieldCacheStatus, FieldDisplay, FieldMode, ParaChild, Run, RunProps,
};
use crate::report::{DecisionReason, LossSet, Representation};

/// Lowers a [`LinkElem`] into paragraph children.
///
/// Returns a single `Hyperlink` paragraph child wrapping the body runs (with
/// either an external `r:id` for URLs or a `w:anchor` for in-document targets),
/// or — for an unsupported positional destination — the bare body runs.
pub fn link(
    elem: &Packed<LinkElem>,
    styles: StyleChain,
    props: RunProps,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<ParaChild>> {
    let span = elem.span();

    // Resolve the destination (label → location) up front. A label-based link
    // can fail to resolve during an early introspection-stabilization iteration
    // (empty introspector); treat that as "no destination yet" and emit the bare
    // body so the pass does not error. The final stabilized pass resolves it.
    let dest = match elem.dest.resolve_early(ctx.engine(), span) {
        Ok(dest) => dest,
        Err(_) => {
            let runs = ctx.inline_runs(&elem.body, styles, props)?;
            return Ok(runs.into_iter().map(ParaChild::Run).collect());
        }
    };

    match dest {
        Destination::Url(url) => {
            // External hyperlink: allocate (or reuse) an External relationship
            // and reference it via `r:id`. The relationship carries
            // `TargetMode="External"` (handled by `add_external_rel`).
            let rel = ctx.add_external_rel(url.into_inner().as_str());
            let runs = ctx.inline_runs(&elem.body, styles, props.clone())?;
            Ok(vec![ParaChild::Hyperlink { rel: Some(rel), anchor: None, runs }])
        }
        Destination::Location(loc) => {
            // Internal hyperlink to a bookmark — no relationship, just an
            // anchor naming the target bookmark.
            let (_id, name) = ctx.add_bookmark(loc);
            let runs = ctx.inline_runs(&elem.body, styles, props.clone())?;
            Ok(vec![ParaChild::Hyperlink { rel: None, anchor: Some(name), runs }])
        }
        Destination::Position(_) => {
            // Positional (page + x/y) links have no DOCX equivalent; emit the
            // body without a hyperlink wrapper so the text is preserved.
            ctx.warn_approximate(
                "positional link",
                span,
                DecisionReason::PositionalLinkTarget,
                LossSet::LINK_TARGET,
            );
            let runs = ctx.inline_runs(&elem.body, styles, props)?;
            Ok(runs.into_iter().map(ParaChild::Run).collect())
        }
    }
}

/// Lowers a [`RefElem`] into runs.
///
/// For a reference whose target is a locatable in-document element, emits a
/// `REF`/`PAGEREF` complex field pointing at the target's bookmark, with the
/// resolved reference text as the cached field result. For a citation (no
/// locatable element), emits the realized citation text inline.
pub fn reference(
    elem: &Packed<RefElem>,
    styles: StyleChain,
    props: RunProps,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    let form = elem.form.get(styles);

    // The synthesized `element` field holds the referenced content (with a
    // `Location`) when the target is a locatable element; it is `Some(None)`
    // for bibliography citations and `None` if Typst has not yet discovered
    // the target.
    let target_loc: Option<Location> = elem
        .element
        .as_ref()
        .and_then(|o| o.as_ref())
        .and_then(|content| content.location());

    // Realize the reference into its linked, textual form. This yields the
    // supplement + number for a normal ref, or the citation text. We re-lower
    // it through `inline_runs` to obtain runs we can use as the field result
    // (and as the standalone rendering for citations). We strip the link that
    // `realize` would normally introduce (the field/anchor supplies the jump).
    // Realize via `engine.delay` so that an unresolved target during an early
    // introspection-stabilization iteration becomes a *delayed* (non-fatal)
    // error — exactly as the paged/HTML `RefElem` show rule does. Without this,
    // the first pass (empty introspector) would fail hard with "label does not
    // exist". On the final stabilized pass the target resolves and the delayed
    // error is discarded.
    let realized = {
        let result = elem.realize(ctx.engine(), styles);
        ctx.engine().delay(result)
    };
    let result_runs = ctx.inline_runs(&realized, styles, props.clone())?;

    let Some(loc) = target_loc else {
        // Citation / unresolved target: no bookmark to point at. Emit the
        // realized text inline.
        return Ok(result_runs);
    };

    // Page references realize as two semantic direct-link segments: a static
    // Typst-owned supplement (for example localized "page" + NBSP) and a live
    // consumer-owned page value. `inline_runs` has already lowered that plan;
    // wrapping it in another PAGEREF would create a nested field and let an
    // update erase the supplement.
    if form == RefForm::Page {
        return Ok(result_runs);
    }

    // In-document reference → REF (text) or PAGEREF (page number) complex
    // field targeting the element's bookmark.
    let (_id, name) = ctx.add_bookmark(loc);
    // Word's REF evaluator returns bookmarked content; it cannot reproduce
    // Typst's supplement + numbering rules. Keep the native field/link UX, but
    // lock the exact Typst-computed cached result against global update.
    let content = elem.clone().pack();
    ctx.record_content_decision(
        &content,
        Representation::Approximate,
        DecisionReason::TypstOwnedReferenceText,
        LossSet::DYNAMIC_BEHAVIOR,
        0,
    );
    // ` REF _Ref7 \h ` — `\h` makes the field result a hyperlink to the
    // bookmark. Leading/trailing spaces match every real-world emitter.
    let instr: EcoString = ecow::eco_format!(" REF {name} \\h ");

    Ok(vec![Run::Field(Field {
        instr,
        result: result_runs,
        mode: FieldMode::Static,
        display: FieldDisplay::Visible,
        cache_status: FieldCacheStatus::Resolved,
    })])
}

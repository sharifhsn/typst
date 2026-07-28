//! The `comment` mapper: Word's review comments (`word/comments.xml` plus the
//! `w:commentRangeStart`/`End`/`w:commentReference` anchors in the body) → the
//! Typst IR's [`Inline::Comment`].
//!
//! Typst has no comment construct, so the question is what a comment *becomes*
//! rather than how it maps. Rendering one as a footnote would turn an
//! annotation into printed content — it would change pagination and show up in
//! the PDF, which is precisely what a comment must not do. Dropping it loses
//! real authored information. So a comment lowers to a labelled `#metadata`:
//! invisible, carrying the full payload, and reachable through `#query`, which
//! lets a `#show` rule opt into rendering comments in the margin without the
//! import having decided that for the reader.
//!
//! Word anchors a comment either to a *span* (a `w:commentRangeStart`/`End`
//! pair around the commented words) or to a *point* (just the
//! `w:commentReference` mark). Both are handled by attaching the payload to
//! whichever anchor for a given id arrives first, and emitting a bare closing
//! anchor for the span's other end — see `lower_comment_anchor`.

use ecow::eco_format;

use crate::lower::{LowerCtx, lower_items};
use crate::tdoc::{CommentAnchor, CommentInfo, Inline};

/// Lower one comment anchor — a `w:commentRangeStart`/`End`, or the
/// `w:commentReference` mark — to its [`Inline::Comment`].
///
/// `closing` distinguishes the end of a span from every other anchor. The
/// payload rides on the *first* anchor seen for an id, which makes the two
/// shapes Word writes fall out without special-casing either: for a span the
/// first anchor is `w:commentRangeStart` and the trailing `w:commentReference`
/// adds nothing, while for a point comment the reference mark is the only
/// anchor there is and carries the payload itself.
///
/// `None` when there is nothing to anchor: a comment id with no entry in
/// `word/comments.xml` (dangling — Word doesn't produce this, but a
/// hand-edited document can), or a repeat anchor that would only duplicate a
/// label already emitted.
pub(crate) fn lower_comment_anchor(
    id: i64,
    closing: bool,
    ctx: &mut LowerCtx,
) -> Option<Inline> {
    if !ctx.package.comments.contains_key(&id) {
        ctx.report.drop(
            "comment",
            "the comment this anchor points at is missing from word/comments.xml",
        );
        return None;
    }

    let label: ecow::EcoString = if closing {
        eco_format!("comment-{id}-end")
    } else {
        eco_format!("comment-{id}")
    };
    // Word writes a `w:commentReference` *after* a range's closing anchor, so
    // the same id legitimately arrives more than once; only the first opening
    // anchor and the first closing one become labels, since a duplicate label
    // fails to compile.
    if !ctx.claim_label(&label) {
        return None;
    }

    let info = (!closing).then(|| {
        // Cloned out of the package before lowering: the body is ordinary body
        // content and lowering it borrows `ctx` mutably, which it cannot do
        // while a reference into `ctx.package` is still alive.
        let comment = &ctx.package.comments[&id];
        let (author, initials, date) =
            (comment.author.clone(), comment.initials.clone(), comment.date.clone());
        // A comment's body can hold anything a document body can, including a
        // page break — meaningless here, exactly as in a footnote or a table
        // cell, so the same container guard applies.
        let was_in_container = ctx.enter_container();
        let body = lower_items(&ctx.package.comments[&id].body, ctx);
        ctx.exit_container(was_in_container);
        CommentInfo { author, initials, date, body }
    });

    Some(Inline::Comment(CommentAnchor { label, info }))
}

//! Comment mapper — turns the labelled `#metadata` anchors that
//! `typst-docx-import` emits for Word review comments back into real
//! `word/comments.xml` entries plus the `w:commentRangeStart`/`End`/
//! `w:commentReference` anchors in `document.xml`.
//!
//! This mirrors the shipped import contract (`typst-docx-import`'s
//! `mappers::comment`, and `Emitter::render_comment` in its `emit.rs`) exactly:
//!
//! - The OPENING anchor is a `#metadata` whose value is a dict with
//!   `kind: "comment"`, optional `author`/`initials`/`date` strings, and
//!   `body` as content, labelled `<comment-N>`.
//! - The CLOSING anchor is `#metadata(none)` labelled `<comment-N-end>`,
//!   absent for a point-anchored comment.
//!
//! `N` is a foreign id from the imported document and is never reused as our
//! own Word id (`DocxCtx::register_comment` allocates fresh, sequential ids).
//! The label TEXT is used only as an opaque matching key between an opening
//! and its closing anchor.
//!
//! A `#metadata` that doesn't match this shape (no `kind: "comment"` field, a
//! `None` value whose label doesn't end in `-end`, or any other value) is not
//! ours — [`tag_children`] returns `None` and the caller keeps its ordinary
//! passthrough, so arbitrary user metadata is unaffected.

use ecow::{EcoString, eco_format};
use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Dict, Str, StyleChain, Value};
use typst_library::introspection::{MetadataElem, QueryLabelIntrospection, Tag};
use typst_syntax::Span;

use crate::ctx::DocxCtx;
use crate::dom::{Block, Para, ParaChild, ParaProps, Run, RunProps};

/// The conventional Word character-style id for the in-body comment marker.
/// Defined in `styles_part.rs`, mirroring `FootnoteReference`.
const COMMENT_REFERENCE_STYLE: &str = "CommentReference";

/// The conventional Word paragraph-style id for comment body text, mirroring
/// `FootnoteText`.
const COMMENT_TEXT_STYLE: &str = "CommentText";

/// If `tag` is the opening or closing anchor of a comment (per the contract
/// above), lowers it to the paragraph children that replace the generic
/// `ParaChild::Tag` passthrough. Returns `None` for every other tag — most
/// importantly the metadata element's own *closing introspection tag*
/// (`Tag::End`, which carries no content to inspect) and any unrelated
/// metadata — so the caller falls back to its ordinary handling.
pub(crate) fn tag_children(
    ctx: &mut DocxCtx,
    tag: &Tag,
    styles: StyleChain,
) -> SourceResult<Option<Vec<ParaChild>>> {
    let Tag::Start(content, _) = tag else { return Ok(None) };
    let Some(meta) = content.to_packed::<MetadataElem>() else { return Ok(None) };

    match &meta.value {
        Value::Dict(dict) => {
            if !is_comment_dict(dict) {
                return Ok(None);
            }
            open_comment(ctx, content, dict, styles).map(Some)
        }
        Value::None => {
            let Some(label) = content.label() else { return Ok(None) };
            let resolved = label.resolve();
            let Some(base) = resolved.as_str().strip_suffix("-end") else {
                return Ok(None);
            };
            Ok(Some(close_comment(ctx, base, content.span())))
        }
        _ => Ok(None),
    }
}

/// Whether a metadata dict is a comment's opening payload, per the contract:
/// `kind: "comment"`.
fn is_comment_dict(dict: &Dict) -> bool {
    matches!(dict.get("kind"), Ok(Value::Str(s)) if s.as_str() == "comment")
}

/// Lowers a comment's opening `#metadata` anchor: registers its body (always,
/// exactly once) and decides — by checking whether a matching `<label>-end>`
/// anchor exists anywhere in the (converged) document — whether this is a
/// span comment (emit a range start, awaiting the close) or a point comment
/// (emit just the reference, right here).
fn open_comment(
    ctx: &mut DocxCtx,
    content: &Content,
    dict: &Dict,
    styles: StyleChain,
) -> SourceResult<Vec<ParaChild>> {
    let author = str_field(dict, "author");
    let initials = str_field(dict, "initials");
    let date = str_field(dict, "date");
    let body = match dict.get("body") {
        Ok(Value::Content(body)) => body.clone(),
        _ => Content::empty(),
    };

    // Lower the body with `in_comment` set, mirroring the footnote mapper's
    // `in_footnote` scoping: an image/link inside must resolve against
    // `comments.xml`'s OWN relationships part, not the document's.
    let was_in_comment = ctx.in_comment;
    ctx.in_comment = true;
    let lowered = ctx.blocks(&body, styles);
    ctx.in_comment = was_in_comment;
    let mut blocks = lowered?;

    // An empty comment (or one whose body failed to lower) still needs a
    // well-formed `w:comment` — a dangling id with no paragraph is the classic
    // cause of a Word "repair" prompt.
    if blocks.is_empty() {
        blocks
            .push(Block::Para(Para { props: ParaProps::default(), content: Vec::new() }));
    }
    // Apply the `CommentText` paragraph style to every body paragraph that
    // does not already carry an explicit style, mirroring the footnote body's
    // `FootnoteText` treatment.
    for block in &mut blocks {
        if let Block::Para(para) = block
            && para.props.style.is_none()
        {
            para.props.style = Some(COMMENT_TEXT_STYLE.into());
        }
    }

    let id = ctx.register_comment(author, initials, date, blocks);

    // A comment is a span when a matching closing anchor exists anywhere in
    // the document — checked via the introspector (the same
    // `ctx.engine().introspect(..)` pattern the footnote mapper uses to
    // resolve a declaration elsewhere in the document), never guessed from
    // ordering, since the closing anchor can arrive arbitrarily later.
    let span = content.span();
    let mut is_span_key = None;
    if let Some(label) = content.label() {
        let key: EcoString = label.resolve().as_str().into();
        let end_label = eco_format!("{key}-end");
        if label_exists(ctx, &end_label, span) {
            is_span_key = Some(key);
        }
    }

    if let Some(key) = is_span_key {
        ctx.open_comment_span(key, id);
        Ok(vec![ParaChild::CommentRangeStart { id }])
    } else {
        // Point-anchored: no range, just the in-body marker right here.
        Ok(vec![ParaChild::Run(comment_reference(id))])
    }
}

/// Lowers a comment's closing `#metadata(none)` anchor, given the base label
/// (its own label with the `-end` suffix already stripped).
fn close_comment(ctx: &mut DocxCtx, base: &str, span: Span) -> Vec<ParaChild> {
    match ctx.close_comment_span(base) {
        Some(id) => {
            vec![ParaChild::CommentRangeEnd { id }, ParaChild::Run(comment_reference(id))]
        }
        None => {
            // A closing anchor with no matching opening one: malformed input
            // (a hand-edited or corrupted document could produce this, though
            // `typst-docx-import` itself never does). Skip it rather than
            // emitting a dangling `w:commentRangeEnd` Word would need to
            // repair, and report the loss through the existing machinery.
            ctx.warn_ignored(
                "comment closing anchor with no matching opening anchor",
                span,
            );
            Vec::new()
        }
    }
}

fn comment_reference(id: i32) -> Run {
    let props = RunProps {
        style: Some(COMMENT_REFERENCE_STYLE.into()),
        ..RunProps::default()
    };
    Run::CommentReference { props, id }
}

fn str_field(dict: &Dict, key: &str) -> Option<EcoString> {
    match dict.get(key) {
        Ok(Value::Str(s)) => Some(str_value(s)),
        _ => None,
    }
}

fn str_value(s: &Str) -> EcoString {
    s.as_str().into()
}

/// Whether a label with the given (already interned-ready) name exists
/// anywhere in the converged document.
fn label_exists(ctx: &mut DocxCtx, name: &str, span: Span) -> bool {
    let Some(label) =
        typst_library::foundations::Label::new(typst_utils::PicoStr::intern(name))
    else {
        return false;
    };
    ctx.engine().introspect(QueryLabelIntrospection(label, span)).is_ok()
}

#[cfg(test)]
mod tests {
    use typst_library::foundations::dict;

    use super::*;

    #[test]
    fn dict_with_comment_kind_is_recognized() {
        let value = dict! { "kind" => "comment", "body" => Content::empty() };
        assert!(is_comment_dict(&value));
    }

    #[test]
    fn dict_with_other_kind_is_not_a_comment() {
        let value = dict! { "kind" => "bookmark" };
        assert!(!is_comment_dict(&value));
    }

    #[test]
    fn dict_without_kind_is_not_a_comment() {
        // Arbitrary user metadata — must never be mistaken for a comment
        // anchor (see the module doc comment's passthrough guarantee).
        let value = dict! { "note" => "just some data" };
        assert!(!is_comment_dict(&value));
    }

    #[test]
    fn str_field_reads_a_present_string() {
        let value = dict! { "author" => "Ada Lovelace" };
        assert_eq!(str_field(&value, "author").as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn str_field_is_none_when_absent_or_wrong_type() {
        let value = dict! { "author" => 7 };
        assert_eq!(str_field(&value, "author"), None);
        assert_eq!(str_field(&value, "initials"), None);
    }
}

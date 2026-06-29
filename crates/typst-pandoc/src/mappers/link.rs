//! Link mapper: `LinkElem` → `Inline::Link(Attr, [Inline], (url, ""))`.
//!
//! Mirrors the DOCX `mappers/reference.rs` `link` arm, swapping the OOXML
//! `<w:hyperlink>`/relationship machinery for Pandoc's native `Link` node:
//! - URL destination → `Link(empty_attr, [body], (url, ""))`.
//! - In-document `Location` destination → `Link(empty_attr, [body],
//!   ("#"+anchor_id(loc), ""))`. The `#id` must match the SHARED id namespace
//!   that the heading/figure mappers emit via [`PandocCtx::anchor_id`], or the
//!   intra-document jump dangles (load-bearing).
//! - Positional (page + x/y) jump → no page model in Pandoc; drop the wrapper
//!   and keep the inner inlines (no content lost).
//!
//! Like the DOCX mapper, the destination is resolved *early* (`resolve_early`):
//! a label-based link can fail to resolve during an early introspection-
//! stabilization iteration (empty introspector). Rather than error, we treat
//! that as "no destination yet" and emit the bare body inlines; the final
//! stabilized pass resolves it.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::model::{Destination, LinkElem};

use crate::ast::{Inline, empty_attr};
use crate::convert::inline_into;
use crate::ctx::PandocCtx;

/// Lowers a [`LinkElem`] into inlines.
///
/// Returns a single `Link` wrapping the lowered body inlines (URL or `#anchor`
/// target), or — for a positional destination or an as-yet-unresolved label —
/// the bare body inlines.
pub fn link_inline(
    elem: &Packed<LinkElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    let span = elem.span();

    // Lower the body once; every branch reuses these inlines.
    let mut body = Vec::new();
    inline_into(ctx, &elem.body, styles, &mut body)?;

    // Resolve the destination (label → location) up front. An early
    // introspection iteration (empty introspector) can fail to resolve a
    // label-based link; emit the bare body so the pass does not error.
    let dest = match elem.dest.resolve_early(ctx.engine(), span) {
        Ok(dest) => dest,
        Err(_) => return Ok(body),
    };

    match dest {
        Destination::Url(url) => {
            let target = (url.into_inner().to_string(), String::new());
            Ok(vec![Inline::Link(empty_attr(), body, target)])
        }
        Destination::Location(loc) => {
            let id = ctx.anchor_id(loc);
            let target = (format!("#{id}"), String::new());
            Ok(vec![Inline::Link(empty_attr(), body, target)])
        }
        Destination::Position(_) => {
            // Page + x/y coordinate jump: no page model in Pandoc. Drop the
            // wrapper, keep the inner inlines (nothing lost).
            ctx.warn_ignored("positional link", span);
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{Inline, empty_attr};

    /// Build the JSON our mapper emits for a URL link, exactly as the encoder
    /// would, and assert its shape. This is the same value validated against
    /// real pandoc (`-f json -t latex` / `-t native`) by hand in the worklog.
    #[test]
    fn url_link_shape() {
        let link = Inline::Link(
            empty_attr(),
            vec![Inline::Str("Typst".into())],
            ("https://typst.app".into(), String::new()),
        );
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["t"], "Link");
        // c = [Attr, [Inline], [url, title]]
        assert_eq!(json["c"][0], serde_json::json!(["", [], []]));
        assert_eq!(json["c"][1][0]["t"], "Str");
        assert_eq!(json["c"][2], serde_json::json!(["https://typst.app", ""]));
    }

    #[test]
    fn internal_link_shape() {
        let link = Inline::Link(
            empty_attr(),
            vec![Inline::Str("see".into())],
            ("#ref-00000000deadbeef".into(), String::new()),
        );
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["c"][2][0], "#ref-00000000deadbeef");
    }
}

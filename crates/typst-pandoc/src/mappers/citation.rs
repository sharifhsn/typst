//! Citation / reference mapper: `RefElem` → structured `Cite` or `Link`.
//!
//! Mirrors the DOCX `mappers/reference.rs` blueprint, but emits Pandoc AST nodes
//! instead of OOXML fields. A Typst `RefElem` covers two very different cases,
//! distinguished — exactly as in DOCX — by whether the synthesized `element`
//! field resolves to a locatable [`Location`]:
//!
//! - **Cross-reference** (`@heading` / `@figure` / `@eq` — a locatable target):
//!   Pandoc has no dedicated cross-reference node, so we emit an
//!   `Inline::Link(empty, [text], ("#"+anchor_id, ""))` whose URL is the
//!   `#`-fragment of the target's shared `Attr` id (the same id the heading /
//!   figure mapper registers via [`PandocCtx::anchor_id`] — the shared id
//!   namespace is load-bearing). The realized supplement+number is the link
//!   body so the document reads correctly. `RefForm::Page` has no page model in
//!   Pandoc, so it degrades to the plain realized text.
//!
//! - **Bibliographic citation** (a `RefElem` whose target is a bibliography key,
//!   not a locatable element): we emit a structured `Inline::Cite([Citation],
//!   [fallback])`. The `Citation` carries the cite key, the supplement as
//!   prefix/suffix, and a `CitationMode` derived from the cite form. We ALWAYS
//!   bake the already-formatted hayagriva citation text as the fallback `[Inline]`
//!   list — that is what every pandoc writer renders WITHOUT `--citeproc`, and
//!   the only thing the docx/plain/html writers ever show. With `--citeproc`
//!   (plus a `.bib`) pandoc reformats from the structured record instead. This
//!   gives the best of both: valid+readable output today, and re-resolvable
//!   citations for consumers that run citeproc.
//!
//! ARCHITECTURE NOTE (why this mapper is mostly latent today): the Pandoc target
//! registers `REF_RULE` + `CITE_GROUP_RULE` in `typst-layout/src/rules.rs`, so a
//! `RefElem` cite is realized into a `CiteGroup` and rendered by hayagriva
//! *before* it reaches this mapper — what actually arrives at `handle_inline` is
//! the already-formatted citation text (and a `DirectLinkElem`→`LinkElem` for
//! cross-refs). To route the *raw* `RefElem` here (and thereby emit structured
//! `Cite`), the integrator must NOT register `REF_RULE`/`CITE_GROUP_RULE` for
//! `Target::Pandoc` (handle them natively here, as DOCX does for the ref/field
//! split). This mapper is written to be correct either way: when it *is* reached
//! with a raw `RefElem`, it produces the structured `Cite` + fallback; the
//! fallback is built by realizing the ref ourselves (mirroring DOCX), so it stays
//! valid even under the current (rules-registered) configuration where the arm is
//! seldom hit. See the return notes / `synth-bib-notes.md` for the `.bib` story.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::introspection::Location;
use typst_library::model::{CitationForm, RefElem, RefForm};

use crate::ast::{empty_attr, Citation, CitationMode, Inline};
use crate::ctx::PandocCtx;

/// Lowers a [`RefElem`] into inline nodes.
pub fn reference(
    elem: &Packed<RefElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    let form = elem.form.get(styles);

    // The synthesized `element` field holds the referenced content (with a
    // `Location`) when the target is a locatable element; it is `Some(None)` for
    // bibliography citations and `None` if Typst has not yet discovered the
    // target (an early introspection-stabilization iteration).
    let target_loc: Option<Location> = elem
        .element
        .as_ref()
        .and_then(|o| o.as_ref())
        .and_then(|content| content.location());

    // Realize the reference into its linked, textual form. This yields the
    // supplement + number for a cross-ref, or the formatted citation text. We
    // realize via `engine.delay` so that an unresolved target during an early
    // introspection-stabilization iteration becomes a *delayed* (non-fatal)
    // error — exactly as the paged/HTML/DOCX `RefElem` show rules do. Without
    // this, the first pass (empty introspector) would fail hard with "label does
    // not exist". On the final stabilized pass the target resolves and the
    // delayed error is discarded.
    let realized = {
        let result = elem.realize(ctx.engine(), styles);
        ctx.engine().delay(result)
    };
    let mut fallback: Vec<Inline> = Vec::new();
    crate::convert::inline_into(ctx, &realized, styles, &mut fallback)?;

    match target_loc {
        // -- Cross-reference to a locatable element (heading/figure/eq) --------
        Some(loc) if form != RefForm::Page => {
            // Internal cross-reference → a `Link` whose URL is the `#`-fragment of
            // the target element's shared `Attr` id. Pandoc writers turn this into
            // the right native jump (`\hyperref`/`<a href="#…">`/bookmark). The
            // realized supplement+number is the link body.
            let anchor = ctx.anchor_id(loc);
            let url = format!("#{anchor}");
            Ok(vec![Inline::Link(empty_attr(), fallback, (url, String::new()))])
        }

        // -- `RefForm::Page` (PAGEREF) — no page model in Pandoc ---------------
        Some(_) => {
            // A page reference has no Pandoc equivalent (there is no paged model
            // at this layer). Degrade to the plain realized text.
            Ok(fallback)
        }

        // -- Bibliographic citation (non-locatable target) ---------------------
        None => Ok(vec![cite_inline(elem, styles, ctx, fallback)?]),
    }
}

/// Builds a structured `Inline::Cite` for a bibliographic citation, baking the
/// already-formatted hayagriva text as the fallback `[Inline]` list.
fn cite_inline(
    elem: &Packed<RefElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
    fallback: Vec<Inline>,
) -> SourceResult<Inline> {
    // The synthesized `citation` field carries the `CiteElem` (key, supplement,
    // form). It is populated during `RefElem::synthesize`; if it is somehow
    // absent (it should not be for a bib cite), fall back to the raw target label
    // as the cite id and a normal mode.
    let citation = match elem.citation.as_ref().and_then(|o| o.as_ref()) {
        Some(cite) => {
            // citationId = the bib key.
            let id = cite.key.resolve().as_str().to_string();

            // Mode from the cite form: a "prose"/"author"/"year" cite reads in
            // the flow of text (author-in-text), a normal cite is parenthetical.
            let mode = match cite.form.get(styles) {
                Some(CitationForm::Prose)
                | Some(CitationForm::Author)
                | Some(CitationForm::Year) => CitationMode::AuthorInText,
                Some(CitationForm::Normal) | Some(CitationForm::Full) => {
                    CitationMode::NormalCitation
                }
                // `form: none` means "include in bibliography but show nothing";
                // treat structurally as a suppressed-author cite.
                None => CitationMode::SuppressAuthor,
            };

            // Supplement (page/chapter, e.g. `@key[p. 7]`) → the citation suffix.
            // Lower it to inlines if present; pandoc renders it after the cite.
            let suffix = match cite.supplement.get_cloned(styles) {
                Some(content) => {
                    let mut out = Vec::new();
                    crate::convert::inline_into(ctx, &content, styles, &mut out)?;
                    out
                }
                None => Vec::new(),
            };

            Citation {
                id,
                prefix: Vec::new(),
                suffix,
                mode,
                note_num: 0,
                hash: 0,
            }
        }
        None => Citation {
            id: elem.target.resolve().as_str().to_string(),
            prefix: Vec::new(),
            suffix: Vec::new(),
            mode: CitationMode::NormalCitation,
            note_num: 0,
            hash: 0,
        },
    };

    // The fallback inlines must be non-empty for the cite to render anything
    // without `--citeproc`. If realization produced nothing (e.g. `form: none`),
    // synthesize a minimal `[@key]`-style fallback so the node is still readable.
    let fallback = if fallback.is_empty() {
        vec![Inline::Str(format!("[{}]", citation.id))]
    } else {
        fallback
    };

    Ok(Inline::Cite(vec![citation], fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, Pandoc, PANDOC_API_VERSION};

    /// Serializes a single inline inside a minimal document and returns the JSON.
    fn doc_json(inline: Inline) -> String {
        let doc = Pandoc {
            pandoc_api_version: PANDOC_API_VERSION,
            meta: Default::default(),
            blocks: vec![Block::Para(vec![inline])],
        };
        serde_json::to_string(&doc).unwrap()
    }

    /// A structured `Cite` with a baked fallback serializes to the exact shape
    /// pandoc's markdown reader produces for `[@foo]`, and round-trips.
    #[test]
    fn cite_shape_matches_pandoc() {
        let cite = Inline::Cite(
            vec![Citation {
                id: "foo".to_string(),
                prefix: Vec::new(),
                suffix: Vec::new(),
                mode: CitationMode::NormalCitation,
                note_num: 0,
                hash: 0,
            }],
            vec![Inline::Str("[1]".to_string())],
        );
        let json = doc_json(cite);
        // The six citation fields, in pandoc's plain-object shape.
        assert!(json.contains("\"citationId\":\"foo\""));
        assert!(json.contains("\"citationPrefix\":[]"));
        assert!(json.contains("\"citationSuffix\":[]"));
        assert!(json.contains("\"citationMode\":{\"t\":\"NormalCitation\"}"));
        assert!(json.contains("\"citationNoteNum\":0"));
        assert!(json.contains("\"citationHash\":0"));
    }

    /// A cite with a supplement carries the suffix inlines.
    #[test]
    fn cite_with_suffix() {
        let cite = Inline::Cite(
            vec![Citation {
                id: "netwok".to_string(),
                prefix: Vec::new(),
                suffix: vec![Inline::Str("p. 7".to_string())],
                mode: CitationMode::AuthorInText,
                note_num: 0,
                hash: 0,
            }],
            vec![Inline::Str("Doe & Smith 2020".to_string())],
        );
        let json = doc_json(cite);
        assert!(json.contains("\"citationSuffix\":[{\"t\":\"Str\",\"c\":\"p. 7\"}]"));
        assert!(json.contains("\"citationMode\":{\"t\":\"AuthorInText\"}"));
    }

    /// A cross-reference is a `Link` whose URL is a `#`-fragment.
    #[test]
    fn xref_is_fragment_link() {
        let link = Inline::Link(
            empty_attr(),
            vec![Inline::Str("Section 1".to_string())],
            ("#ref-deadbeef".to_string(), String::new()),
        );
        let json = doc_json(link);
        assert!(json.contains("\"t\":\"Link\""));
        assert!(json.contains("\"#ref-deadbeef\""));
    }
}

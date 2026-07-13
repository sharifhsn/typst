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
//! ARCHITECTURE NOTE (the direction actually shipped — SELF-CONTAINED). The
//! Pandoc target KEEPS `REF_RULE` + `CITE_GROUP_RULE` + `BIBLIOGRAPHY_RULE`
//! registered in `typst-layout/src/rules.rs`. They are load-bearing for
//! convergence: `Works::generate` (inside `BIBLIOGRAPHY_RULE`) builds the table
//! every in-text citation looks up — un-registering them deadlocks the
//! introspection loop ("citation could not be located"). As a consequence a cite
//! is realized into formatted hayagriva text (with a `DirectLinkElem`→`LinkElem`
//! to the bib entry's well-known backlink `Location`) *before* it reaches this
//! mapper, so the raw-`RefElem` arm below ([`reference`]) is seldom hit for bib
//! cites — the in-text cite is just linked text that the generic walk already
//! emits as a `Link` to `#ref-<hash(location)>`.
//!
//! The two original defects were both downstream of the BIBLIOGRAPHY: it
//! rendered to a `GridElem`/layouter block with no Pandoc node, so the whole
//! reference list fell to the rasterize fallback — one opaque `Image`. That (a)
//! made the references un-selectable and (b) destroyed the per-entry backlink
//! anchors, so every in-text cite `Link` dangled. The fix, entirely on the
//! rendered-bib side, is [`marker_tag`] (below): it lowers the bibliography's
//! `PdfMarkerTag` structure to anchored, selectable `Div`/`Para` blocks — each
//! `BibEntry` carrying its backlink as `id = ref-<hash>` so the cite Links
//! resolve. `BIBLIOGRAPHY_RULE` is additionally tweaked for `Target::Pandoc` to
//! always take the linear-block path (never the rasterizing grid) so numbered
//! styles work too. The final normalization pass demotes any internal link with
//! no matching anchor to
//! bare text — the belt-and-suspenders guarantee that no cite ever dangles
//! (notably the bib entry's `[1]`-marker back-link to the un-anchored cite site).
//!
//! STRUCTURED `Cite` + synthesized `.bib` (the target goal) is NOT shipped — see
//! `finish-cite.md`: the pinned hayagriva 0.10.1 exposes no BibLaTeX/CSL-JSON
//! serializer (only a hayagriva-YAML writer pandoc rejects), and emitting
//! structured `Cite` nodes needs a cite-key↔`Location` map that is private to
//! `typst-library`. The [`reference`] arm below already builds a structured
//! `Inline::Cite` with a baked fallback should a raw `RefElem` ever reach it, so
//! the structured path is half-wired and forward-compatible.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, StyleChain};
use typst_library::introspection::Location;
use typst_library::model::{CitationForm, RefElem, RefForm};
use typst_library::pdf::{PdfMarkerTag, PdfMarkerTagKind};

use crate::ast::{Attr, Block, Citation, CitationMode, Inline, empty_attr, id_attr};
use crate::ctx::PandocCtx;

/// Lowers a [`PdfMarkerTag`] — the structural marker that wraps bibliography,
/// list, and term content during realization — into Pandoc blocks.
///
/// The convergence-critical case is the bibliography. `BIBLIOGRAPHY_RULE`
/// (registered for `Target::Pandoc`, see `typst-layout/src/rules.rs`) renders the
/// reference list *eagerly* via citeproc — that is what builds the `Works` table
/// in-text citations look up, so it cannot be un-registered without deadlocking
/// convergence. The rule wraps the list in `Bibliography(_)` and each entry in a
/// `BibEntry` whose body is `located(entry.backlink)` — the very `Location` an
/// in-text cite `Link` points at (`#ref-<hash(location)>`). The generic walk has
/// no idea what a `PdfMarkerTag` is, so without this mapper it falls through to
/// the rasterize fallback: the whole reference list becomes one opaque `Image`,
/// the backlink anchors vanish, and every cite Link dangles. Here we instead:
///
/// - `Bibliography(_)` → a `Div` with `id = "refs"` + class `references` (the
///   pandoc-citeproc convention, so `--citeproc` slots a regenerated list in the
///   same place) wrapping the recursively-converted entries — selectable text.
/// - `BibEntry` → the entry body converted to blocks, with the backlink
///   `Location` lowered to `id = ref-<hash>` (via [`PandocCtx::anchor_id`]) on the
///   block, so the matching in-text cite `Link` resolves to a real anchor.
/// - `ListItemLabel` (the `[1]` marker on a numbered style) and any other marker
///   → the body converted transparently (the marker reads as plain text).
pub fn marker_tag(
    elem: &Packed<PdfMarkerTag>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    match &elem.kind {
        PdfMarkerTagKind::Bibliography(_) => {
            let inner = crate::convert::blocks(ctx, &elem.body, styles)?;
            // The pandoc-citeproc reference-list container: a `Div` with id
            // `refs` and class `references`. Readable without citeproc (it holds
            // the rendered entries); with `--citeproc` pandoc regenerates the
            // list into this same div.
            let attr: Attr =
                ("refs".to_string(), vec!["references".to_string()], Vec::new());
            Ok(vec![Block::Div(attr, inner)])
        }
        PdfMarkerTagKind::BibEntry => {
            let inner = crate::convert::blocks(ctx, &elem.body, styles)?;
            // The backlink `Location` lives on the entry body. `anchor_id` yields
            // the exact `ref-<hash>` form the in-text cite Links target, so we use
            // it verbatim as the entry's `id` — making the anchor resolve.
            match elem.body.location() {
                Some(loc) => {
                    let id = ctx.anchor_id(loc).to_string();
                    Ok(vec![Block::Div(id_attr(id), inner)])
                }
                // No backlink (shouldn't happen for a real entry): emit as-is.
                None => Ok(inner),
            }
        }
        // Any other marker (list/term labels & bodies): transparent passthrough.
        _ => crate::convert::blocks(ctx, &elem.body, styles),
    }
}

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
    use crate::ast::{Block, PANDOC_API_VERSION, Pandoc};

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

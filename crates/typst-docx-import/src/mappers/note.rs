//! The `note` mapper: a `w:footnoteReference`/`w:endnoteReference`
//! ([`RunContent::NoteRef`]), resolved against
//! [`WmlPackage::footnotes`]/[`WmlPackage::endnotes`], → the Typst IR's
//! [`Inline::Footnote`].
//!
//! Typst has no separate end-of-document note store — `#footnote[..]` always
//! inlines the note's content at the reference site and renders it at the
//! foot of *that* page. A footnote maps onto this directly; an endnote does
//! not (Word collects endnotes at the document's end), so lowering one as a
//! footnote is a real, reported approximation rather than a silent
//! equivalence — see [`lower_note_ref`]'s `endnote` branch.

use crate::lower::{lower_items, LowerCtx};
use crate::tdoc::Inline;

/// Resolve one `RunContent::NoteRef { endnote, id }` to its `Inline::Footnote`,
/// or `None` if the reference can't be honored — recording why via `ctx.report`
/// in every such case:
///
/// - the id isn't a key in the relevant part's map (dangling reference —
///   Word itself never produces this, but a hand-edited or corrupt document
///   can);
/// - resolving it would re-enter a note already being lowered, directly or
///   through a chain (see [`LowerCtx::enter_note`]) — without this guard
///   a self- or mutually-referential note would recurse forever.
pub(crate) fn lower_note_ref(endnote: bool, id: i64, ctx: &mut LowerCtx) -> Option<Inline> {
    let package = ctx.package;
    let (notes, what) =
        if endnote { (&package.endnotes, "endnote") } else { (&package.footnotes, "footnote") };

    let Some(body) = notes.get(&id) else {
        ctx.report.drop(what, "referenced note not found in the part; reference dropped");
        return None;
    };

    if !ctx.enter_note(endnote, id) {
        ctx.report.drop(
            what,
            "note reference cycle (or nesting too deep); reference dropped to avoid \
             recursing forever",
        );
        return None;
    }

    if endnote {
        ctx.report.approximate(
            "endnote",
            "imported as a footnote; Typst has no end-of-document note store, so it renders \
             at the foot of its own page rather than collected with the document's other \
             endnotes",
        );
    }

    // A page/column break inside a footnote/endnote body is just as
    // meaningless as inside a table cell or text box (Typst rejects it
    // outright) — see `mappers::table::lower_cell`'s equivalent guard.
    let was_in_container = ctx.enter_container();
    let blocks = lower_items(body, ctx);
    ctx.exit_container(was_in_container);
    ctx.exit_note();

    Some(Inline::Footnote(blocks))
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
    use crate::tdoc::Block;
    use crate::wml::model::{
        BodyItem, Paragraph, Run, RunContent, RunItem, RunProps, WmlPackage,
    };

    fn text_body(text: &str) -> Vec<BodyItem> {
        vec![BodyItem::Paragraph(Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::Text(text.into())],
            })],
        })]
    }

    fn package_with_footnote(id: i64, body: Vec<BodyItem>) -> WmlPackage {
        let mut footnotes = FxHashMap::default();
        footnotes.insert(id, body);
        WmlPackage { footnotes, ..Default::default() }
    }

    #[test]
    fn footnote_resolves_to_its_lowered_body() {
        let package = package_with_footnote(1, text_body("snoska"));
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inline =
            lower_note_ref(false, 1, &mut ctx).expect("expected a resolved footnote");
        match inline {
            Inline::Footnote(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert!(matches!(&blocks[0], Block::Paragraph { .. }));
            }
            other => panic!("expected Inline::Footnote, got {other:?}"),
        }
        assert!(report.notes.is_empty());
    }

    #[test]
    fn dangling_reference_drops_with_a_report_note() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inline = lower_note_ref(false, 1, &mut ctx);
        assert!(inline.is_none());
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "footnote");
    }

    #[test]
    fn endnote_lowers_to_a_footnote_and_records_the_approximation() {
        let mut endnotes = FxHashMap::default();
        endnotes.insert(1, text_body("end note text"));
        let package = WmlPackage { endnotes, ..Default::default() };
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inline =
            lower_note_ref(true, 1, &mut ctx).expect("expected a resolved endnote");
        assert!(matches!(inline, Inline::Footnote(_)));
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "endnote");
    }

    /// A note that references itself must not recurse forever — the guard
    /// must terminate the lowering and report the drop instead of hanging.
    #[test]
    fn self_referential_note_terminates_and_reports_instead_of_recursing() {
        let self_ref_body = vec![BodyItem::Paragraph(Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::NoteRef { endnote: false, id: 1 }],
            })],
        })];
        let package = package_with_footnote(1, self_ref_body);
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);

        // Must return promptly (not hang) with the outer reference resolved
        // but its self-referential inner one dropped — leaving the note's
        // sole paragraph with no visible content, so it lowers to no blocks
        // at all (see `mappers::para::lower_paragraph`'s `Empty` case).
        let inline =
            lower_note_ref(false, 1, &mut ctx).expect("the outer reference still resolves");
        let Inline::Footnote(blocks) = inline else { panic!("expected Inline::Footnote") };
        assert!(blocks.is_empty(), "expected no blocks, got {blocks:?}");
        assert!(report.notes.iter().any(|n| n.what == "footnote"));
    }
}

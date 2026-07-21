//! The `revision` mapper: Word's tracked changes (`w:ins`/`w:del`, and the
//! `w:moveTo`/`w:moveFrom` pair written for moved text) → the Typst IR's
//! [`Inline::Revision`].
//!
//! Both import modes render the same thing — the "all changes accepted" view
//! Word shows by default, which is what the author last meant the document to
//! say. They differ only in whether the *record* survives beside it, and under
//! [`TrackedChanges::Preserve`] it does, as invisible `#metadata` (see
//! [`crate::mappers::comment`], which established the mechanism).
//!
//! The two halves are shaped differently because they are differently shaped
//! problems, not out of inconsistency:
//!
//! - An **insertion** wraps content that is still in the document. Its text
//!   flows through the ordinary run path and renders normally; the record is
//!   two anchors bracketing it, because a Typst label attaches to a single
//!   element and so a range must be expressed as two points.
//! - A **deletion** wraps content that is *gone*. There is nothing to bracket,
//!   so the removed runs never enter the paragraph's inline sequence at all —
//!   they are lowered into the anchor's own value, where they render nothing
//!   and take up no space (verified: a paragraph holding only metadata renders
//!   pixel-identically to no paragraph at all) but remain readable.

use ecow::eco_format;

use crate::lower::LowerCtx;
use crate::opts::TrackedChanges;
use crate::tdoc::{Block, Inline, RevisionAnchor, RevisionInfo};
use crate::wml::model::{RevisionInfo as WmlRevision, RunItem};

/// Lower the opening anchor of an insertion.
pub(crate) fn lower_insertion_start(
    info: &WmlRevision,
    ctx: &mut LowerCtx,
) -> Option<Inline> {
    if ctx.options.tracked == TrackedChanges::Accept {
        return None;
    }
    let label = ctx.next_revision_label("ins");
    Some(Inline::Revision(RevisionAnchor {
        label,
        info: Some(RevisionInfo {
            kind: "insertion",
            author: info.author.clone(),
            date: info.date.clone(),
            move_name: info.moved.then(|| info.move_name.clone()).flatten(),
            body: Vec::new(),
        }),
    }))
}

/// Lower the closing anchor of an insertion. `None` when the matching opening
/// anchor wasn't emitted — either the mode is `Accept`, or the depth cap in
/// `wml::parse::flatten_revisions` swallowed the opening wrapper.
pub(crate) fn lower_insertion_end(ctx: &mut LowerCtx) -> Option<Inline> {
    if ctx.options.tracked == TrackedChanges::Accept {
        return None;
    }
    let label = ctx.close_revision_label()?;
    Some(Inline::Revision(RevisionAnchor { label, info: None }))
}

/// Lower a deletion: one anchor carrying the removed content.
///
/// Under `Accept` the runs are dropped without being lowered at all, which is
/// the behaviour that predates the option — an accepted deletion leaves no
/// trace, and lowering content only to throw it away would risk side effects
/// (an image extracted to `assets/`, a report note) for text nobody asked to
/// keep.
pub(crate) fn lower_deletion(
    info: &WmlRevision,
    runs: &[RunItem],
    ctx: &mut LowerCtx,
) -> Option<Inline> {
    if ctx.options.tracked == TrackedChanges::Accept {
        return None;
    }
    let label = ctx.next_revision_label("del");
    // A deletion's runs are inline content, but the anchor's value holds
    // blocks — the same shape a comment body uses, so one paragraph wraps
    // them.
    let body = crate::mappers::run::lower_run_items(runs, None, ctx);
    let body = if body.is_empty() {
        Vec::new()
    } else {
        vec![Block::Paragraph { style: Default::default(), body }]
    };
    Some(Inline::Revision(RevisionAnchor {
        label,
        info: Some(RevisionInfo {
            kind: "deletion",
            author: info.author.clone(),
            date: info.date.clone(),
            move_name: info.moved.then(|| info.move_name.clone()).flatten(),
            body,
        }),
    }))
}

/// Report the revision constructs this importer deliberately doesn't map.
///
/// A format change (`w:rPrChange`/`w:pPrChange`) records what the formatting
/// *used to be*; carrying that would need a serialized mirror of Word's run
/// and paragraph properties for something nothing in Typst consumes. A
/// paragraph-mark change is the paragraph boundary itself rather than inline
/// content, so it has nowhere to hang an anchor. Both are named rather than
/// silently ignored.
pub(crate) fn report_unmapped(what: &str, ctx: &mut LowerCtx) {
    ctx.report.drop(
        "tracked formatting change",
        format!("{what} records previous formatting, which Typst has nowhere to keep"),
    );
}

/// Build the next unique anchor label for a revision, and remember it so the
/// closing anchor can match.
impl LowerCtx<'_> {
    fn next_revision_label(&mut self, prefix: &str) -> ecow::EcoString {
        self.revision_counter += 1;
        let label = eco_format!("{prefix}-{}", self.revision_counter);
        if prefix == "ins" {
            self.open_revisions.push(label.clone());
        }
        label
    }

    fn close_revision_label(&mut self) -> Option<ecow::EcoString> {
        self.open_revisions.pop().map(|label| eco_format!("{label}-end"))
    }
}

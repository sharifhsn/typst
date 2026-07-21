//! Tier-2 semantic passes over the Typst IR: promote bold/italic to
//! strong/emph, collapse uniform formatting into `#set text`, hoist a
//! preamble. Each is a self-contained rewrite, run in a fixed order:
//!
//! 1. [`collapse_style`] — drop `#text(..)` fields that just restate the
//!    document default (and, inside headings, the fields the heading itself
//!    implies), unwrapping runs that end up empty.
//! 2. [`strong_emph`] — promote what `collapse_style` left as a bold/italic-
//!    only `#text(..)` run into `*strong*`/`_emph_` markup.
//! 3. [`hoist_par`] — if a strong majority of paragraphs justify, hoist that
//!    into `#set par(justify: true)` instead of repeating it per paragraph.
//! 4. [`resolve_labels`] — downgrade any cross-reference whose target label
//!    didn't survive lowering, which Typst would otherwise reject outright.
//!
//! The first three are lossless rewrites (they only ever remove *redundant*
//! styling); `resolve_labels` is the one that can lose something, and records
//! it on `report`. It runs last, since it can only judge which labels exist
//! once the tree is final.
//!
//! The first two run over every block tree in the document
//! ([`TypstDoc::block_trees_mut`]), so header/footer content is made
//! idiomatic alongside the body. `hoist_par` deliberately does not: whether
//! the *body* justifies is a document-wide decision that a handful of header
//! paragraphs should not get a vote in.

mod collapse_style;
mod hoist_par;
mod resolve_labels;
mod strong_emph;

use crate::report::ImportReport;
use crate::tdoc::TypstDoc;

pub fn run(doc: &mut TypstDoc, report: &mut ImportReport) {
    collapse_style::run(doc);
    strong_emph::run(doc);
    hoist_par::run(doc);
    resolve_labels::run(doc, report);
}

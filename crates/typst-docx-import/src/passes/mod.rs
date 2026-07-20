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
//!
//! All three are lossless rewrites (they only ever remove *redundant*
//! styling), so `report` is currently unused — it's threaded through for
//! future passes that do need to record an approximation.

mod collapse_style;
mod hoist_par;
mod strong_emph;

use crate::report::ImportReport;
use crate::tdoc::TypstDoc;

pub fn run(doc: &mut TypstDoc, _report: &mut ImportReport) {
    collapse_style::run(doc);
    strong_emph::run(doc);
    hoist_par::run(doc);
}

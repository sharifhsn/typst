//! Resolution: turning what a slide *states* into what PowerPoint *shows*.
//!
//! This is the part with no Word analogue, and the part a naive importer gets
//! wrong on every slide. A shape on a slide typically states almost nothing —
//! not its position, not its font, not its colour. All of that is inherited
//! from the matching placeholder on its **layout**, which inherits from the
//! **master**, whose colours are names that only `theme1.xml` can resolve.
//!
//! Three levels and a theme, and the chain is keyed by *placeholder identity*
//! rather than by a style name, which is what makes it unlike
//! `typst-docx-import`'s one-dimensional `basedOn` walk.

pub mod color;
pub mod inherit;

//! Late pass assigning bookmark anchor names to linked-to `Location`s.
//!
//! Mirrors `typst-html`'s `link.rs`: after the body IR is built, every location
//! that is a link target needs a stable bookmark name so REF/PAGEREF fields and
//! `w:hyperlink w:anchor` can resolve to it.

use ecow::EcoString;
use rustc_hash::FxHashMap;
use typst_library::introspection::Location;

use crate::dom::BookmarkTable;

/// Produces the `Location -> anchor name` map from the bookmark table, for the
/// introspector's `anchor()` query.
pub fn anchors(table: &BookmarkTable) -> FxHashMap<Location, EcoString> {
    table
        .by_location
        .iter()
        .map(|(loc, (name, _))| (*loc, name.clone()))
        .collect()
}

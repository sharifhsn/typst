//! The introspector for Pandoc documents.
//!
//! Pandoc is pageless at export time, so this reuses the target-agnostic
//! [`ElementIntrospector`] over the introspection [`Tag`]s collected while
//! walking the IR (a near-copy of `typst_docx::DocxIntrospector`). Positions
//! are not meaningful, so a trivial [`HtmlPosition`] is used. Only `anchor()`
//! differs from DOCX: it stores the Pandoc `Attr` id (heading/figure/span)
//! instead of a `_Ref{id}` bookmark name.

use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroUsize;

use ecow::{EcoString, EcoVec};
use rustc_hash::FxHashMap;
use typst_library::diag::StrResult;
use typst_library::foundations::{Content, Label, Selector};
use typst_library::introspection::{
    DocumentPosition, ElementIntrospector, ElementIntrospectorBuilder, HtmlPosition,
    Introspector, Location, Tag,
};
use typst_library::model::Numbering;
use typst_syntax::VirtualPath;

/// An introspector implementation for Pandoc documents.
#[derive(Clone)]
pub struct PandocIntrospector {
    elements: ElementIntrospector<HtmlPosition>,
    anchors: FxHashMap<Location, EcoString>,
}

impl PandocIntrospector {
    /// Creates an introspector from the introspection tags collected while
    /// walking the IR.
    #[typst_macros::time(name = "introspect pandoc")]
    pub fn new(tags: &[Tag]) -> PandocIntrospector {
        let mut builder = ElementIntrospectorBuilder::<HtmlPosition>::new();
        let pos = HtmlPosition::new(EcoVec::new());
        for tag in tags {
            builder.discover_tag(tag, pos.clone());
        }
        PandocIntrospector {
            elements: builder.finalize(),
            anchors: FxHashMap::default(),
        }
    }

    /// The underlying element introspector.
    pub fn elements(&self) -> &ElementIntrospector<HtmlPosition> {
        &self.elements
    }

    /// Enriches the introspector with the late-assigned `Attr`-id anchors.
    pub fn set_anchors(&mut self, anchors: FxHashMap<Location, EcoString>) {
        self.anchors = anchors;
    }
}

impl Introspector for PandocIntrospector {
    fn query(&self, selector: &Selector) -> EcoVec<Content> {
        self.elements.query(selector)
    }

    fn query_first(&self, selector: &Selector) -> Option<Content> {
        self.elements.query_first(selector)
    }

    fn query_unique(&self, selector: &Selector) -> StrResult<Content> {
        self.elements.query_unique(selector)
    }

    fn query_label(&self, label: Label) -> StrResult<&Content> {
        self.elements.query_label(label)
    }

    fn query_labelled(&self) -> EcoVec<Content> {
        self.elements.query_labelled()
    }

    fn query_count_before(&self, selector: &Selector, end: Location) -> usize {
        self.elements.query_count_before(selector, end)
    }

    fn label_count(&self, label: Label) -> usize {
        self.elements.label_count(label)
    }

    fn locator(&self, key: u128, base: Location) -> Option<Location> {
        self.elements.locator(key, base)
    }

    fn pages(&self, _: Location) -> Option<NonZeroUsize> {
        None
    }

    fn page(&self, _: Location) -> Option<NonZeroUsize> {
        None
    }

    fn position(&self, location: Location) -> Option<DocumentPosition> {
        self.elements.position(location).cloned().map(DocumentPosition::Html)
    }

    fn page_numbering(&self, _: Location) -> Option<&Numbering> {
        None
    }

    fn page_supplement(&self, _: Location) -> Option<&Content> {
        None
    }

    fn anchor(&self, location: Location) -> Option<&EcoString> {
        self.anchors.get(&location)
    }

    fn document(&self, _: Location) -> Option<Location> {
        None
    }

    fn path(&self, _: Location) -> Option<&VirtualPath> {
        None
    }
}

impl Debug for PandocIntrospector {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.pad("PandocIntrospector(..)")
    }
}

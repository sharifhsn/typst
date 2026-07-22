//! The introspector for Pandoc documents.
//!
//! Pandoc is pageless at export time, so this reuses the target-agnostic
//! [`ElementIntrospector`] over the introspection [`Tag`]s collected while
//! walking the IR (a near-copy of `typst_docx::DocxIntrospector`). Positions
//! are not meaningful, so a trivial [`HtmlPosition`] is used.
//!
//! `anchor()` returns `None`: the Pandoc converter never routes internal links
//! through the introspector. Every heading/figure/equation id and every
//! reference/citation/outline target is resolved *inline during the walk* via
//! [`crate::ctx::PandocCtx::anchor_id`] and baked into the emitted AST as a
//! literal `#id`, so — unlike the HTML/paged link resolvers ([`LateLinkResolver`]
//! and `EarlyLinkResolver`), which are the trait method's only consumers and run
//! for neither DOCX nor Pandoc — nothing here ever queries an anchor map.
//!
//! [`LateLinkResolver`]: typst_library::model::LateLinkResolver

use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroUsize;

use ecow::{EcoString, EcoVec};
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
        PandocIntrospector { elements: builder.finalize() }
    }

    /// The underlying element introspector.
    pub fn elements(&self) -> &ElementIntrospector<HtmlPosition> {
        &self.elements
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

    fn anchor(&self, _: Location) -> Option<&EcoString> {
        // Unused for Pandoc: links are resolved inline during the walk (see the
        // module docs), never through this trait method.
        None
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

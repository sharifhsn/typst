//! The introspector for DOCX documents.
//!
//! DOCX is pageless at export time, so this reuses the target-agnostic
//! [`ElementIntrospector`] over the introspection [`Tag`]s collected while
//! walking the IR. Exact positions are unknowable for a flowing DOCX document,
//! so positions are synthetic but preserve the lowered IR's page and block
//! order.

use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroUsize;

use ecow::{EcoString, EcoVec};
use rustc_hash::FxHashMap;
use typst_library::diag::StrResult;
use typst_library::foundations::{Content, Label, Selector};
use typst_library::introspection::{
    DocumentPosition, ElementIntrospector, ElementIntrospectorBuilder, Introspector,
    Location, PagedPosition, Tag,
};
use typst_library::model::Numbering;
use typst_syntax::VirtualPath;

/// An introspector implementation for DOCX documents.
#[derive(Clone)]
pub struct DocxIntrospector {
    elements: ElementIntrospector<PagedPosition>,
    anchors: FxHashMap<Location, EcoString>,
    /// The synthetic page model: for every tag location, the 1-based count of
    /// explicit page/section breaks before it plus one, and its section index.
    /// A flowing document has no real pages, but templates legitimately read
    /// paged introspection (`@t(form: "page")`, `loc.page-numbering()`,
    /// `counter(page)`) — this keeps them compiling with plausible values
    /// (exact where pagination is break-structured, a lower bound where text
    /// auto-flows) instead of failing the whole export.
    page_model: FxHashMap<Location, (usize, usize)>,
    /// Total synthetic pages (1 + explicit break count).
    total_pages: usize,
    /// Each section's `set page(numbering:)`, in section order.
    section_numberings: Vec<Option<Numbering>>,
}

impl DocxIntrospector {
    /// Creates an introspector from the introspection tags collected while
    /// walking the IR.
    #[typst_macros::time(name = "introspect docx")]
    pub fn new(tags: &[(Tag, PagedPosition)]) -> DocxIntrospector {
        if std::env::var_os("DOCX_DEBUG_INTROSPECT").is_some() {
            let mut hist = std::collections::BTreeMap::new();
            for (tag, _) in tags {
                if let Tag::Start(elem, _) = tag {
                    *hist.entry(elem.func().name()).or_insert(0usize) += 1;
                }
            }
            eprintln!("INTROSPECT TAGS: {hist:?}");
        }
        let mut builder = ElementIntrospectorBuilder::<PagedPosition>::new();
        for (tag, pos) in tags {
            builder.discover_tag(tag, *pos);
        }
        DocxIntrospector {
            elements: builder.finalize(),
            anchors: FxHashMap::default(),
            page_model: FxHashMap::default(),
            total_pages: 1,
            section_numberings: Vec::new(),
        }
    }

    /// The underlying element introspector.
    pub fn elements(&self) -> &ElementIntrospector<PagedPosition> {
        &self.elements
    }

    /// Enriches the introspector with the late-assigned bookmark anchors.
    pub fn set_anchors(&mut self, anchors: FxHashMap<Location, EcoString>) {
        self.anchors = anchors;
    }

    /// Installs the synthetic page model (see the field docs).
    pub fn set_page_model(
        &mut self,
        page_model: FxHashMap<Location, (usize, usize)>,
        total_pages: usize,
        section_numberings: Vec<Option<Numbering>>,
    ) {
        self.page_model = page_model;
        self.total_pages = total_pages.max(1);
        self.section_numberings = section_numberings;
    }
}

impl Introspector for DocxIntrospector {
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
        NonZeroUsize::new(self.total_pages)
    }

    fn page(&self, location: Location) -> Option<NonZeroUsize> {
        // Locations outside the model (rasterize-deferred, header/footer tags)
        // are appended after the body, so the final page is the best guess.
        let page = self
            .page_model
            .get(&location)
            .map_or(self.total_pages, |&(page, _)| page);
        NonZeroUsize::new(page.max(1))
    }

    fn position(&self, location: Location) -> Option<DocumentPosition> {
        self.elements.position(location).copied().map(DocumentPosition::Paged)
    }

    fn page_numbering(&self, location: Location) -> Option<&Numbering> {
        match self.page_model.get(&location) {
            // A known location resolves against its own section — faithfully
            // `None` when that section has no `set page(numbering:)`, exactly
            // like referencing an unnumbered page in paged export.
            Some(&(_, section)) => {
                self.section_numberings.get(section).and_then(|n| n.as_ref())
            }
            // An unknown (deferred) location falls back to the first numbered
            // section, leniently: these targets live inside rasterized or
            // furniture content whose true section is unknowable.
            None => self.section_numberings.iter().find_map(|n| n.as_ref()),
        }
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

impl Debug for DocxIntrospector {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.pad("DocxIntrospector(..)")
    }
}

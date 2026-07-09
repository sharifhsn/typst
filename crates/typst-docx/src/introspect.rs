//! The introspector for DOCX documents.
//!
//! DOCX is pageless at export time, so this reuses the target-agnostic
//! [`ElementIntrospector`] over the introspection [`Tag`]s collected while
//! walking the IR. Exact positions are unknowable for a flowing DOCX document,
//! so positions are synthetic but preserve the lowered IR's page and block
//! order.

use std::cell::Cell;
use std::fmt::{self, Debug, Formatter};
use std::num::NonZeroUsize;
use std::sync::Arc;

use ecow::{EcoString, EcoVec};
use rustc_hash::{FxHashMap, FxHashSet};
use typst_library::diag::StrResult;
use typst_library::foundations::{Content, Label, Selector};
use typst_library::introspection::{
    DocumentPosition, ElementIntrospector, ElementIntrospectorBuilder, Introspector,
    Location, PagedPosition, Tag,
};
use typst_library::layout::Point;
use typst_library::model::Numbering;
use typst_syntax::VirtualPath;

thread_local! {
    static FURNITURE_PAGE_OVERRIDE: Cell<Option<NonZeroUsize>> =
        const { Cell::new(None) };
}

/// Runs `f` while unresolved DOCX page-furniture locations report `page`.
///
/// This is intentionally scoped to DOCX's own introspector and is used only
/// while lowering header/footer variants. Real paged locations keep delegating
/// to the paged introspector, so queries over body elements still see true
/// paged positions.
pub(crate) fn with_furniture_page<T>(page: NonZeroUsize, f: impl FnOnce() -> T) -> T {
    FURNITURE_PAGE_OVERRIDE.with(|slot| {
        let prev = slot.replace(Some(page));
        let result = f();
        slot.set(prev);
        result
    })
}

/// An introspector implementation for DOCX documents.
#[derive(Clone)]
pub struct DocxIntrospector {
    /// The target-agnostic element introspector built from the DOCX realization.
    ///
    /// This remains as a fallback for DOCX-only target branches and for content
    /// whose location is not present in the paged document.
    elements: ElementIntrospector<PagedPosition>,
    /// The fixed-point introspector from paged layout. This is the primary
    /// source for queries, positions, page numbers, page numbering, and
    /// bibliography/citation convergence whenever it has an answer.
    real: Option<Arc<typst_layout::PagedIntrospector>>,
    /// Maps locations produced by the DOCX realization to equivalent locations
    /// in the paged realization. This covers repeated page furniture in
    /// particular: DOCX lowers one header/footer part, while paged layout lays
    /// that same source content out once per page with distinct locations.
    real_aliases: FxHashMap<Location, Location>,
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
    /// walking the IR, optionally layered over the fixed-point paged
    /// introspector.
    #[typst_macros::time(name = "introspect docx")]
    pub fn new(
        tags: &[(Tag, PagedPosition)],
        real: Option<Arc<typst_layout::PagedIntrospector>>,
        real_alias_candidates: FxHashSet<Location>,
    ) -> DocxIntrospector {
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
        let mut real_aliases = FxHashMap::default();
        for (tag, pos) in tags {
            builder.discover_tag(tag, *pos);
            if let Some(real) = &real
                && let Tag::End(loc, key, flags) = tag
                && flags.introspectable
                && real_alias_candidates.contains(loc)
                && real.position(*loc).is_none()
                && let Some(real_loc) = real.locator(*key, *loc)
                && real.position(real_loc).is_some()
            {
                real_aliases.insert(*loc, real_loc);
            }
        }
        DocxIntrospector {
            elements: builder.finalize(),
            real,
            real_aliases,
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

    fn real_location(&self, location: Location) -> Option<Location> {
        let real = self.real.as_ref()?;
        if real.position(location).is_some() {
            Some(location)
        } else {
            self.real_aliases.get(&location).copied()
        }
    }

    fn furniture_page_override(&self, location: Location) -> Option<NonZeroUsize> {
        let page = FURNITURE_PAGE_OVERRIDE.with(Cell::get)?;
        match &self.real {
            Some(real) if real.position(location).is_none() => Some(page),
            None if self.elements.position(location).is_none() => Some(page),
            _ => None,
        }
    }

    /// Counts matches for `selector` (via [`Self::query`], so callers stay on
    /// the same source `query()` itself would use) whose resolved page is at
    /// or before `page`. This is a page-granularity approximation of "before
    /// `end`" for callers that only have a page number for `end`, not an
    /// exact resolvable location — used both when a furniture-page override
    /// is active and when falling back for a synthetic (unresolvable-in-`real`)
    /// end location that still has real-backed query results.
    fn count_matching_up_to_page(&self, selector: &Selector, page: NonZeroUsize) -> usize {
        self.query(selector)
            .iter()
            .filter(|elem| {
                elem.location()
                    .and_then(|loc| self.page(loc))
                    .is_none_or(|elem_page| elem_page <= page)
            })
            .count()
    }
}

impl Introspector for DocxIntrospector {
    fn query(&self, selector: &Selector) -> EcoVec<Content> {
        if let Some(result) = self.query_with_furniture_page(selector) {
            return result;
        }
        if let Some(real) = &self.real {
            let result = real.query(selector);
            if !result.is_empty() {
                return result;
            }
        }
        self.elements.query(selector)
    }

    fn query_first(&self, selector: &Selector) -> Option<Content> {
        self.real
            .as_ref()
            .and_then(|real| real.query_first(selector))
            .or_else(|| self.elements.query_first(selector))
    }

    fn query_unique(&self, selector: &Selector) -> StrResult<Content> {
        if let Some(real) = &self.real
            && !real.query(selector).is_empty()
        {
            return real.query_unique(selector);
        }
        self.elements.query_unique(selector)
    }

    fn query_label(&self, label: Label) -> StrResult<&Content> {
        if let Some(real) = &self.real
            && let Ok(content) = real.query_label(label)
        {
            return Ok(content);
        }
        self.elements.query_label(label)
    }

    fn query_labelled(&self) -> EcoVec<Content> {
        if let Some(real) = &self.real {
            let result = real.query_labelled();
            if !result.is_empty() {
                return result;
            }
        }
        self.elements.query_labelled()
    }

    fn query_count_before(&self, selector: &Selector, end: Location) -> usize {
        if let Some(page) = self.furniture_page_override(end) {
            return self.count_matching_up_to_page(selector, page);
        }
        if let Some(real) = &self.real {
            if let Some(real_end) = self.real_location(end) {
                return real.query_count_before(selector, real_end);
            }
            // `end` has no corresponding real-paged position of its own (for
            // example, it was produced inside a synthetic rasterization
            // sub-layout). `query()` still prefers `real`'s matches whenever
            // it has any for this selector, so the count must stay
            // consistent with that same source rather than falling through
            // to `self.elements` — which can record additional/duplicate
            // updates from rasterization passes that have no counterpart in
            // the real document. Using `self.elements`'s count here while
            // `sequence()` (in typst-library's state/counter introspection)
            // built its sequence from `query()`'s (real-backed) results
            // previously produced an offset one past the end of that
            // sequence — an out-of-bounds panic. Approximate "before" via
            // the element's real page instead, the same page-order strategy
            // the furniture-override branch above already uses.
            if !real.query(selector).is_empty()
                && let Some(page) = self.page(end)
            {
                return self.count_matching_up_to_page(selector, page);
            }
        }
        self.elements.query_count_before(selector, end)
    }

    fn label_count(&self, label: Label) -> usize {
        self.real
            .as_ref()
            .map(|real| real.label_count(label))
            .filter(|&count| count > 0)
            .unwrap_or_else(|| self.elements.label_count(label))
    }

    fn locator(&self, key: u128, base: Location) -> Option<Location> {
        self.real
            .as_ref()
            .and_then(|real| real.locator(key, base))
            .or_else(|| self.elements.locator(key, base))
    }

    fn pages(&self, location: Location) -> Option<NonZeroUsize> {
        self.real
            .as_ref()
            .and_then(|real| real.pages(location))
            .or_else(|| NonZeroUsize::new(self.total_pages))
    }

    fn page(&self, location: Location) -> Option<NonZeroUsize> {
        if let Some(page) = self.furniture_page_override(location) {
            return Some(page);
        }
        if let Some(real) = &self.real
            && let Some(real_loc) = self.real_location(location)
            && let Some(page) = real.page(real_loc)
        {
            return Some(page);
        }
        // Locations outside the model (rasterize-deferred, header/footer tags)
        // are appended after the body, so the final page is the best guess.
        let page = self
            .page_model
            .get(&location)
            .map_or(self.total_pages, |&(page, _)| page);
        NonZeroUsize::new(page.max(1))
    }

    fn position(&self, location: Location) -> Option<DocumentPosition> {
        if let Some(page) = self.furniture_page_override(location) {
            return Some(DocumentPosition::Paged(PagedPosition {
                page,
                point: Point::zero(),
            }));
        }
        if let Some(real) = &self.real
            && let Some(real_loc) = self.real_location(location)
            && let Some(pos) = real.position(real_loc)
        {
            return Some(DocumentPosition::Paged(pos));
        }
        self.elements.position(location).copied().map(DocumentPosition::Paged)
    }

    fn page_numbering(&self, location: Location) -> Option<&Numbering> {
        if let Some(real) = &self.real
            && let Some(real_loc) = self.real_location(location)
            && let Some(numbering) = real.page_numbering(real_loc)
        {
            return Some(numbering);
        }
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

    fn page_supplement(&self, location: Location) -> Option<&Content> {
        self.real.as_ref().and_then(|real| {
            let real_loc = self.real_location(location)?;
            real.page_supplement(real_loc)
        })
    }

    fn anchor(&self, location: Location) -> Option<&EcoString> {
        self.anchors
            .get(&location)
            .or_else(|| self.real.as_ref().and_then(|real| real.anchor(location)))
    }

    fn document(&self, location: Location) -> Option<Location> {
        self.real.as_ref().and_then(|real| {
            let real_loc = self.real_location(location)?;
            real.document(real_loc)
        })
    }

    fn path(&self, location: Location) -> Option<&VirtualPath> {
        self.real.as_ref().and_then(|real| {
            let real_loc = self.real_location(location)?;
            real.path(real_loc)
        })
    }
}

impl DocxIntrospector {
    fn query_with_furniture_page(&self, selector: &Selector) -> Option<EcoVec<Content>> {
        match selector {
            Selector::Before { selector, end, inclusive } => {
                let Selector::Location(end) = end.as_ref() else { return None };
                let page = self.furniture_page_override(*end)?;
                let mut list = self.query(selector);
                list.retain(|elem| {
                    elem.location().and_then(|loc| self.page(loc)).is_some_and(
                        |elem_page| elem_page < page || (*inclusive && elem_page == page),
                    )
                });
                Some(list)
            }
            Selector::After { selector, start, inclusive } => {
                let Selector::Location(start) = start.as_ref() else { return None };
                let page = self.furniture_page_override(*start)?;
                let mut list = self.query(selector);
                list.retain(|elem| {
                    elem.location().and_then(|loc| self.page(loc)).is_some_and(
                        |elem_page| elem_page > page || (*inclusive && elem_page == page),
                    )
                });
                Some(list)
            }
            _ => None,
        }
    }
}

impl Debug for DocxIntrospector {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.pad("DocxIntrospector(..)")
    }
}

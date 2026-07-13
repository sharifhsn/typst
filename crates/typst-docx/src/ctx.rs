//! The mutable conversion context [`DocxCtx`] and the inline/block flow.

use std::ops::Range;
use std::sync::Arc;

use ecow::{EcoString, eco_format};
use rustc_hash::{FxHashMap, FxHashSet};
use typst_library::diag::{SourceDiagnostic, SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::introspection::{Locator, SplitLocator, Tag, TagElem};
use typst_library::layout::{Abs, HElem};
use typst_library::math::EquationElem;
use typst_library::model::{
    Destination, DirectLinkElem, DirectLinkKind, FootnoteElem, LinkElem, LinkMarker,
    RefElem,
};
use typst_library::model::{EmphElem, StrongElem};
use typst_library::routines::{Arenas, FragmentKind, RealizationKind};
use typst_library::text::{HighlightElem, SmallcapsElem, StrikeElem, UnderlineElem};
use typst_library::text::{
    LinebreakElem, RawContent, RawElem, RawLine, SmartQuoteElem, SmartQuoter,
    SmartQuotes, SpaceElem, SubElem, SuperElem, TextElem,
};
use typst_library::visualize::{ImageElem, Paint};
use typst_library::{World, WorldExt};
use typst_ooxml_core::media::MediaRegistry;
use typst_ooxml_core::ns;
use typst_syntax::{FileId, Span};

use crate::dom::{
    Block, BookmarkTable, Field, FieldCacheStatus, FieldDisplay, FieldMode, Footnote,
    HeadingStyleSample, ListSpec, NumberingTable, ParaProps, ReviewCandidateKind,
    ReviewJoinId, ReviewOrigin, Run, RunProps, TocFigure, TocHeading, Underline,
    VertAlign,
};
use crate::fallback::CachedOverlay;
use crate::mappers;
use crate::package::{RelMode, Rels};
use crate::props;
use crate::report::{
    DecisionReason, ExportSource, ExportStage, FidelityReport, LossSet, Representation,
    SuppressedKind,
};

use typst_library::introspection::Location;

#[derive(Clone, PartialEq)]
struct RawRange {
    id: FileId,
    range: Range<usize>,
}

/// The relationship-type URI for an image part.
pub const REL_IMAGE: &str = ns::rel::IMAGE;
/// The relationship-type URI for an external hyperlink.
pub const REL_HYPERLINK: &str = ns::rel::HYPERLINK;
/// The relationship-type URI for a header part.
pub const REL_HEADER: &str = ns::rel::HEADER;
/// The relationship-type URI for a footer part.
pub const REL_FOOTER: &str = ns::rel::FOOTER;

/// The mutable package state accumulated during the post-realize walk.
pub struct DocxCtx<'a, 'e> {
    pub(crate) engine: &'a mut Engine<'e>,
    pub(crate) locator: &'a mut SplitLocator<'e>,

    pub(crate) footnotes: Vec<Footnote>,
    footnote_ids: FxHashMap<Location, i32>,
    next_footnote_id: i32,

    pub(crate) numbering: NumberingTable,
    list_shapes: FxHashMap<ListSpec, u32>,
    next_num_id: u32,
    next_review_join_id: u64,

    pub(crate) media: MediaRegistry,

    pub(crate) doc_rels: Rels,
    /// Relationships for footnote-body content (→ `footnotes.xml.rels`); used
    /// while [`Self::in_footnote`] is set.
    pub(crate) footnote_rels: Rels,
    /// When `Some`, relationships are routed here instead of `doc_rels` — used to
    /// collect a header/footer part's own relationships while its content is
    /// lowered (an `r:id` in `headerN.xml` must resolve against `headerN.xml.rels`).
    pub(crate) part_rels: Option<Rels>,
    next_bookmark_id: u32,
    snapshot_bookmark_names: FxHashMap<Location, EcoString>,
    emitted_bookmarks: FxHashSet<Location>,
    next_docpr_id: u32,
    /// Monotonic id for unique header/footer part names across sections.
    next_hdrftr_id: u32,
    /// Monotonic `relativeHeight` z-order for floating drawings (`<wp:anchor>`).
    next_z: u32,
    pub(crate) max_heading_level: u8,
    pub(crate) uses_math: bool,
    /// Structured representation choices and suppressed diagnostics.
    pub(crate) fidelity_report: FidelityReport,
    /// Final physical regions recovered from the converged paged frames.
    pub(crate) paged_geometry: Arc<typst_export_common::paged::PagedGeometry>,

    pub(crate) bookmarks: BookmarkTable,

    /// Introspection tags harvested from rasterized content (see
    /// [`Self::rasterize`]), so labels/refs inside an element that we rendered to
    /// an image remain present in the introspector.
    pub(crate) deferred_tags: Vec<Tag>,
    /// DOCX locations from page furniture (headers/footers) that may need to be
    /// aliased back to their repeated paged-layout locations.
    pub(crate) real_alias_locations: FxHashSet<Location>,

    /// Headings recorded in document order as they are converted, used to
    /// populate any table of contents once each heading's real bookmark exists.
    pub(crate) toc_headings: Vec<TocHeading>,

    /// Resolved heading style samples by emitted heading. The document pass
    /// majority-votes these into `HeadingN` style definitions.
    pub(crate) heading_style_samples: Vec<HeadingStyleSample>,

    /// Captioned figures/tables recorded in document order, used to populate any
    /// list of figures/tables once each figure's real bookmark exists.
    pub(crate) toc_figures: Vec<TocFigure>,

    /// Width available to the current lowering scope. At document level this is
    /// the active section's text area; nested table/stack cells temporarily
    /// narrow it to their own track. Native width planning, relative shapes,
    /// tabs, TOCs, and raster fallback all read the same value so they cannot
    /// drift onto different hard-coded page assumptions.
    pub(crate) available_width: Abs,

    /// Full text-area width for the active page section, before page columns
    /// narrow [`Self::available_width`]. Headers, footers, and parent-scoped
    /// floats use this authority.
    pub(crate) page_content_width: Abs,

    /// Text-area height for the active section (page height minus top/bottom
    /// margins). Positioned content resolves vertical alignment and percentage
    /// offsets against this rather than the full paper height.
    pub(crate) available_height: Abs,

    /// The finite page height, used only as a *retry* bound when rasterizing
    /// content that does not lay out under an infinite-height region — page-
    /// relative content such as a `place(bottom, ..)` cover, a slide, or a
    /// full-page background image, which resolves against the page height and
    /// otherwise collapses or runs to infinity. Normal content lays out under
    /// the infinite region first and never hits this. Set from the page geometry.
    pub(crate) raster_height: Abs,

    /// Smart-quote state, threaded through inline runs.
    quoter: SmartQuoter,
    /// The last character emitted into a text run, for smart quoting.
    last_char: Option<char>,

    /// Nesting depth while the DOCX walker is explicitly inside raw/code
    /// content. Used for paths where `RawElem`/`RawLine` survives to this layer.
    raw_depth: usize,

    /// Source ranges covered by raw/code content before realization. Inline raw
    /// can be unwrapped to styled `TextElem`s before DOCX sees a `RawElem`; this
    /// keeps the origin check tied to source raw spans instead of monospace font.
    raw_ranges: Vec<RawRange>,

    /// Whether the current body section emits Word line numbering. Paragraphs
    /// whose own style chain disables Typst line numbers become
    /// `w:suppressLineNumbers` only while this is true.
    pub(crate) line_numbering_active: bool,

    /// Whether we are currently lowering a footnote's body (into `footnotes.xml`).
    /// Word forbids a footnote *inside* a footnote — a `w:footnoteReference` in
    /// the footnote story makes the file unopenable — so an inner `FootnoteElem`
    /// is flattened to its body text inline instead of emitting a nested mark.
    pub(crate) in_footnote: bool,

    /// Whether the current block context will be *centered* (a figure body).
    /// A centered `wps:txbx` text box does not flow its text in LibreOffice (it
    /// renders as an empty frame with the text leaked to the margin); Word renders
    /// it fine, but for cross-consumer fidelity a framed box in this context is
    /// rasterized to a centered image instead.
    pub(crate) suppress_text_box: bool,

    /// Cache for page-overlay rasterization (`Self::rasterize_page_overlay`),
    /// keyed by a hash of the content, styles, and target box. A page
    /// background/foreground is lowered up to 5 times per section (to detect
    /// first/odd/even variance) and DOCX opens a new section on every
    /// header/footer/numbering change — a slide deck with a per-section
    /// context-sensitive header (e.g. "current chapter title") but a *static*
    /// background image re-runs the full layout+render+crop pipeline for
    /// byte-identical output dozens to hundreds of times (observed: a
    /// touying-style 50-slide deck went from timing out past 3 minutes to a
    /// few seconds). Caches the rendered PNG bytes and frame tags, not the
    /// per-part image relationship (which must stay scoped to that part's own
    /// `.rels`) — `add_image` is still called on every hit, but its own
    /// content-hash dedup (`MediaRegistry::add`) makes that cheap.
    pub(crate) overlay_cache: FxHashMap<u128, Arc<CachedOverlay>>,
}

impl<'a, 'e> DocxCtx<'a, 'e> {
    /// Creates a fresh context.
    pub fn new(engine: &'a mut Engine<'e>, locator: &'a mut SplitLocator<'e>) -> Self {
        Self {
            engine,
            locator,
            footnotes: Vec::new(),
            footnote_ids: FxHashMap::default(),
            next_footnote_id: 1,
            numbering: NumberingTable::default(),
            list_shapes: FxHashMap::default(),
            next_num_id: 1,
            next_review_join_id: 1,
            media: MediaRegistry::new("word/media"),
            doc_rels: Rels::new(),
            footnote_rels: Rels::new(),
            part_rels: None,
            next_bookmark_id: 1,
            snapshot_bookmark_names: FxHashMap::default(),
            emitted_bookmarks: FxHashSet::default(),
            next_docpr_id: 1,
            next_hdrftr_id: 1,
            next_z: 1,
            max_heading_level: 0,
            uses_math: false,
            fidelity_report: FidelityReport::default(),
            paged_geometry: Arc::new(typst_export_common::paged::PagedGeometry::default()),
            bookmarks: BookmarkTable::default(),
            deferred_tags: Vec::new(),
            real_alias_locations: FxHashSet::default(),
            toc_headings: Vec::new(),
            heading_style_samples: Vec::new(),
            toc_figures: Vec::new(),
            // A sane finite default (~A4 text width); overridden from the real
            // page geometry by `docx_document` before any conversion happens.
            available_width: Abs::pt(450.0),
            page_content_width: Abs::pt(450.0),
            available_height: Abs::pt(698.0),
            raster_height: Abs::pt(842.0),
            quoter: SmartQuoter::new(),
            last_char: None,
            raw_depth: 0,
            raw_ranges: Vec::new(),
            line_numbering_active: false,
            in_footnote: false,
            suppress_text_box: false,
            overlay_cache: FxHashMap::default(),
        }
    }

    pub(crate) fn review_origin(
        &mut self,
        span: Span,
        kind: ReviewCandidateKind,
    ) -> ReviewOrigin {
        let join_id = ReviewJoinId(self.next_review_join_id);
        self.next_review_join_id = self.next_review_join_id.saturating_add(1);
        ReviewOrigin { join_id, span, kind }
    }

    pub(crate) fn set_paged_geometry(
        &mut self,
        geometry: Arc<typst_export_common::paged::PagedGeometry>,
    ) {
        self.paged_geometry = geometry;
    }

    pub(crate) fn set_snapshot_bookmarks(
        &mut self,
        snapshot: &crate::snapshot::ExportSnapshot,
    ) {
        self.snapshot_bookmark_names = snapshot
            .nodes()
            .iter()
            .filter_map(|node| {
                node.source.location.map(|location| {
                    (location, eco_format!("_Typst{:032x}", node.source.logical_id))
                })
            })
            .collect();
    }

    // -- Borrowing helpers --------------------------------------------------

    /// Borrows the engine for sub-realization / decode / counter display.
    pub fn engine(&mut self) -> &mut Engine<'e> {
        self.engine
    }

    /// Splits a fresh locator for a sub-fragment.
    pub fn next_locator(&mut self, span: Span) -> Locator<'e> {
        self.locator.next(&span)
    }

    /// Current width budget in Word's dxa/twip unit.
    pub(crate) fn available_width_dxa(&self) -> i32 {
        ((self.available_width.to_pt() * 20.0).round() as i64).clamp(1, i32::MAX as i64)
            as i32
    }

    pub(crate) fn page_content_width_dxa(&self) -> i32 {
        ((self.page_content_width.to_pt() * 20.0).round() as i64)
            .clamp(1, i32::MAX as i64) as i32
    }

    /// Runs a nested lowering scope against a narrower width budget, restoring
    /// the parent budget even when lowering returns an error.
    pub(crate) fn with_available_width<T>(
        &mut self,
        width_dxa: i32,
        f: impl FnOnce(&mut Self) -> SourceResult<T>,
    ) -> SourceResult<T> {
        let previous = self.available_width;
        self.available_width = Abs::pt(width_dxa.max(1) as f64 / 20.0);
        let result = f(self);
        self.available_width = previous;
        result
    }

    /// Evaluates a realized `#layout(size => ..)` callback with the synthetic
    /// page-content size. Returns `None` when the callback cannot be evaluated
    /// outside the paged layouter; callers then fall back to rasterization.
    pub(crate) fn eval_layout_content(
        &mut self,
        elem: &Packed<typst_library::layout::LayoutElem>,
        styles: StyleChain,
    ) -> Option<Content> {
        use comemo::Track;
        use typst_library::foundations::{Context, dict};

        let context = Context::new(elem.location(), Some(styles));
        let args =
            [dict! { "width" => self.available_width, "height" => self.raster_height }];
        match elem.func.call(self.engine, context.track(), args) {
            Ok(value) => Some(value.display()),
            Err(errors) => {
                for diagnostic in errors {
                    self.fidelity_report.suppress_span(
                        elem.pack_ref().elem().name(),
                        elem.span(),
                        elem.location(),
                        ExportStage::LayoutCallback,
                        SuppressedKind::Error,
                        diagnostic,
                    );
                }
                None
            }
        }
    }

    /// Emits a hidden `SEQ \h` field (increment-without-display) for each
    /// captioned figure harvested into `deferred_tags` at/after index `before`.
    ///
    /// Such figures live inside content we rasterized, so they never reached the
    /// figure mapper and emitted no visible `SEQ` field — Word's caption counter
    /// would under-count and the (introspector-baked) cross-reference numbers
    /// would drift. The visible number is already baked into the rasterized
    /// image; this just keeps Word's running count correct for the figures that
    /// follow. Only captioned figures count, mirroring the figure mapper (which
    /// emits the visible `SEQ` together with the caption).
    fn emit_rasterized_figure_seqs(
        &mut self,
        before: usize,
        styles: StyleChain,
        out: &mut Vec<Run>,
    ) {
        use typst_library::model::FigureElem;
        let names: Vec<EcoString> = self.deferred_tags[before..]
            .iter()
            .filter_map(|tag| match tag {
                Tag::Start(c, _) => {
                    let fig = c.to_packed::<FigureElem>()?;
                    fig.caption.get_cloned(styles)?;
                    Some(mappers::image::seq_name(fig, styles))
                }
                _ => None,
            })
            .collect();
        for name in names {
            out.push(Run::Field(crate::dom::Field {
                instr: eco_format!(" SEQ {name} \\h "),
                result: Vec::new(),
                mode: crate::dom::FieldMode::Live,
                display: crate::dom::FieldDisplay::Hidden,
                cache_status: crate::dom::FieldCacheStatus::ConsumerRequired,
            }));
        }
    }

    /// Emits a non-fatal "X was ignored during DOCX export" warning.
    pub fn warn_ignored(&mut self, what: &str, span: Span) {
        self.fidelity_report.record_span(
            ExportSource::new(what, span, None),
            Representation::Drop,
            DecisionReason::UnsupportedContent,
            LossSet::DROP,
            0,
        );
        self.engine
            .sink
            .warn(warning!(span, "{what} was ignored during DOCX export"));
    }

    /// Emits an ignored-feature warning while recording that surrounding text
    /// survived in an approximate representation.
    pub(crate) fn warn_approximate(
        &mut self,
        what: &str,
        span: Span,
        reason: DecisionReason,
        losses: LossSet,
    ) {
        self.fidelity_report.record_span(
            ExportSource::new(what, span, None),
            Representation::Approximate,
            reason,
            losses,
            0,
        );
        self.engine
            .sink
            .warn(warning!(span, "{what} was ignored during DOCX export"));
    }

    pub(crate) fn record_span_decision(
        &mut self,
        what: &str,
        span: Span,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
    ) {
        self.fidelity_report.record_span(
            ExportSource::new(what, span, None),
            representation,
            reason,
            losses,
            0,
        );
    }

    /// Emits a warning already represented by a structured suppressed
    /// diagnostic, without adding a second representation decision.
    pub(crate) fn warn_without_decision(&mut self, what: &str, span: Span) {
        self.engine
            .sink
            .warn(warning!(span, "{what} was ignored during DOCX export"));
    }

    /// Emits a non-fatal exporter warning whose exact wording does not fit the
    /// legacy "was ignored" form.
    pub(crate) fn warn_message(&mut self, message: impl Into<EcoString>, span: Span) {
        self.engine.sink.warn(warning!(span, "{}", message.into()));
    }

    pub(crate) fn record_content_decision(
        &mut self,
        content: &Content,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
        affected_text_chars: usize,
    ) {
        self.fidelity_report.record_content(
            content,
            representation,
            reason,
            losses,
            affected_text_chars,
        );
    }

    /// Records the terminal state for a logical region after every planned
    /// representation and whole-region fallback has failed.
    pub(crate) fn record_content_drop(
        &mut self,
        content: &Content,
        reason: DecisionReason,
        message: &'static str,
    ) {
        // Source formatting around a visual-only body (for example a multiline
        // `place(rotate(line(..)))`) appears in `plain_text` as indentation and
        // newlines. It is layout syntax, not visible document text. Retain
        // internal spaces in real text, but do not turn an all-whitespace
        // decorative fallback failure into a corpus `content_loss` result.
        let text = content.plain_text();
        self.record_content_decision(
            content,
            Representation::Drop,
            reason,
            LossSet::DROP,
            text.trim().chars().count(),
        );
        self.warn_message(message, content.span());
    }

    fn record_native_page_reference(&mut self, content: &Content) {
        self.record_content_decision(
            content,
            Representation::Native,
            DecisionReason::NativePageReference,
            LossSet::default(),
            0,
        );
    }

    /// Retains a field-planning failure absorbed by a deliberate best-effort
    /// plan instead of losing the diagnostic or aborting the whole document.
    pub(crate) fn suppress_content_diagnostic(
        &mut self,
        content: &Content,
        stage: ExportStage,
        diagnostic: SourceDiagnostic,
    ) {
        self.fidelity_report.suppress_content(
            content,
            stage,
            SuppressedKind::Error,
            diagnostic,
        );
    }

    // -- Allocators ---------------------------------------------------------

    /// Registers a footnote body, returning its `w:id` (>= 1).
    pub fn add_footnote(&mut self, decl: Location, body_blocks: Vec<Block>) -> i32 {
        if let Some(&id) = self.footnote_ids.get(&decl) {
            return id;
        }
        let id = self.next_footnote_id;
        self.next_footnote_id += 1;
        self.footnote_ids.insert(decl, id);
        self.footnotes.push(Footnote { id, blocks: body_blocks });
        id
    }

    /// The relationships table the *current* content lowers into: a header/footer
    /// part's own table while one is being built, the footnote table while a
    /// footnote body is lowered, else the document's. An `r:id` used in a part
    /// must resolve against that part's `.rels`, so relationships created while
    /// lowering header/footer/footnote content must NOT land in `document.xml.rels`.
    fn active_rels(&mut self) -> &mut Rels {
        if let Some(rels) = self.part_rels.as_mut() {
            rels
        } else if self.in_footnote {
            &mut self.footnote_rels
        } else {
            &mut self.doc_rels
        }
    }

    /// Embeds image bytes as a media part (the part is deduped by byte hash and
    /// shared across the package); returns the rId of a relationship to it,
    /// allocated in the *active* part's relationships (see [`Self::active_rels`]).
    pub fn add_image(&mut self, bytes: &[u8], ext: &str) -> EcoString {
        let id = self.media.add(bytes, ext);
        let part = self.media.part(id);
        let target: EcoString = part
            .part_name
            .strip_prefix("word/")
            .unwrap_or(part.part_name.as_str())
            .into();
        self.active_rels().add(REL_IMAGE, &target, RelMode::Internal)
    }

    /// Allocates a unique `wp:docPr` id (>= 1) for a Drawing.
    pub fn next_drawing_id(&mut self) -> u32 {
        let id = self.next_docpr_id;
        self.next_docpr_id += 1;
        id
    }

    /// Allocates a unique header/footer part name (`header{N}.xml` /
    /// `footer{N}.xml`). A document with several sections produces several
    /// header/footer parts, which must not share a name or the OPC package gets
    /// a duplicate zip entry.
    pub fn next_hdrftr_name(&mut self, is_header: bool) -> EcoString {
        let n = self.next_hdrftr_id;
        self.next_hdrftr_id += 1;
        let kind = if is_header { "header" } else { "footer" };
        eco_format!("{kind}{n}.xml")
    }

    /// Allocates a monotonic `relativeHeight` z-order (>= 1) for a floating
    /// drawing (`<wp:anchor>`), so stacked floats don't share a z-index.
    pub fn next_z(&mut self) -> u32 {
        let z = self.next_z;
        self.next_z += 1;
        z
    }

    /// Registers (or reuses) a numbering shape; returns the `numId`.
    pub fn register_list(&mut self, spec: ListSpec) -> u32 {
        if !spec.restart_at_1
            && let Some(&num_id) = self.list_shapes.get(&spec)
        {
            return num_id;
        }

        // Find or create the abstract num for this shape.
        let abstract_id = self
            .numbering
            .abstracts
            .iter()
            .find(|a| a.levels == spec.levels && a.multilevel == spec.multilevel)
            .map(|a| a.id)
            .unwrap_or_else(|| {
                let id = self.numbering.abstracts.len() as u32;
                self.numbering.abstracts.push(crate::dom::AbstractNum {
                    id,
                    levels: spec.levels.clone(),
                    multilevel: spec.multilevel,
                });
                id
            });

        let num_id = self.next_num_id;
        self.next_num_id += 1;
        let start_override =
            if spec.restart_at_1 { spec.levels.first().map(|l| l.start) } else { None };
        self.numbering.nums.push(crate::dom::NumInstance {
            num_id,
            abstract_id,
            start_override,
        });

        if !spec.restart_at_1 {
            self.list_shapes.insert(spec, num_id);
        }
        num_id
    }

    /// Allocates a bookmark id + a stable name for a `Location`. Idempotent.
    pub fn add_bookmark(&mut self, loc: Location) -> (u32, EcoString) {
        if let Some(entry) = self.bookmarks.by_location.get(&loc) {
            return (entry.1, entry.0.clone());
        }
        let id = self.next_bookmark_id;
        self.next_bookmark_id += 1;
        let name = self
            .snapshot_bookmark_names
            .get(&loc)
            .cloned()
            .unwrap_or_else(|| eco_format!("_Ref{id}"));
        self.bookmarks.by_location.insert(loc, (name.clone(), id));
        (id, name)
    }

    /// Returns a bookmark only the first time its marker pair should be emitted.
    pub fn bookmark_for_emission(&mut self, loc: Location) -> Option<(u32, EcoString)> {
        self.emitted_bookmarks.insert(loc).then(|| self.add_bookmark(loc))
    }

    /// Allocates (or reuses) an external hyperlink relationship in the active
    /// part's relationships (document / footnote / header-footer); returns the rId.
    pub fn add_external_rel(&mut self, url: &str) -> EcoString {
        self.active_rels().add(REL_HYPERLINK, url, RelMode::External)
    }

    /// Registers a header part relationship (`Target` relative to `word/`);
    /// returns the rId for the matching `<w:headerReference>`.
    pub fn add_header_rel(&mut self, target: &str) -> EcoString {
        self.doc_rels.add(REL_HEADER, target, RelMode::Internal)
    }

    /// Registers a footer part relationship (`Target` relative to `word/`);
    /// returns the rId for the matching `<w:footerReference>`.
    pub fn add_footer_rel(&mut self, target: &str) -> EcoString {
        self.doc_rels.add(REL_FOOTER, target, RelMode::Internal)
    }

    /// Marks that math was emitted.
    pub fn mark_math(&mut self) {
        self.uses_math = true;
    }

    /// Runs `f` while marking emitted text as originating from raw/code.
    pub(crate) fn with_raw_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> SourceResult<T>,
    ) -> SourceResult<T> {
        self.raw_depth += 1;
        let result = f(self);
        self.raw_depth -= 1;
        result
    }

    /// Records raw/code source ranges before realization can unwrap them into
    /// ordinary styled text.
    pub(crate) fn record_raw_ranges(&mut self, content: &Content) {
        use std::ops::ControlFlow;

        let _ = content.traverse(&mut |element: Content| {
            if let Some(raw) = element.to_packed::<RawElem>() {
                match &raw.text {
                    RawContent::Text(_) => self.record_raw_span(raw.span()),
                    RawContent::Lines(lines) => {
                        for (_, span) in lines {
                            self.record_raw_span(*span);
                        }
                        if lines.is_empty() {
                            self.record_raw_span(raw.span());
                        }
                    }
                }
            }
            if let Some(line) = element.to_packed::<RawLine>() {
                self.record_raw_span(line.span());
            }
            ControlFlow::<()>::Continue(())
        });
    }

    fn record_raw_span(&mut self, span: Span) {
        if let Some(range) = self.raw_range(span)
            && range.range.start < range.range.end
            && !self.raw_ranges.contains(&range)
        {
            self.raw_ranges.push(range);
        }
    }

    fn raw_range(&self, span: Span) -> Option<RawRange> {
        let id = span.id()?;
        let range = self.engine.world.range(span)?;
        Some(RawRange { id, range })
    }

    fn span_is_raw(&self, span: Span) -> bool {
        if self.raw_depth > 0 {
            return true;
        }
        let Some(span) = self.raw_range(span) else {
            return false;
        };
        self.raw_ranges.iter().any(|raw| {
            raw.id == span.id
                && raw.range.start <= span.range.start
                && span.range.end <= raw.range.end
        })
    }

    /// Notes the deepest heading level seen.
    pub fn note_heading_level(&mut self, level: u8) {
        self.max_heading_level = self.max_heading_level.max(level);
    }

    /// Records the resolved run properties that should define this heading level.
    pub fn note_heading_style(&mut self, level: u8, props: RunProps) {
        self.heading_style_samples
            .push(HeadingStyleSample { level, rpr: style_owned_heading_props(props) });
    }

    // -- Property resolvers -------------------------------------------------

    /// Resolves a `TextElem`'s effective run properties.
    pub fn resolve_text_props(
        &self,
        styles: StyleChain,
        inherited: RunProps,
    ) -> RunProps {
        let mut p = inherited;
        if self.raw_depth > 0 {
            p.no_proof = true;
        }

        // Size. The document's most common size is later hoisted into
        // `docDefaults` and stripped from the runs that match it (see
        // `hoist_text_defaults`); here we record the absolute value.
        let size = props::pt_to_half_pt(styles.resolve(TextElem::size).to_pt());
        p.size_half_pt = Some(size);

        // Colour. Record the resolved value, including black. The document pass
        // strips it only when it matches the governing style/default; this lets a
        // black run remain black when `Normal` is non-black.
        if let typst_library::visualize::Paint::Solid(color) =
            styles.get_ref(TextElem::fill)
        {
            p.color = Some(props::color_to_hex(color));
            if p.style.as_deref() == Some("Hyperlink") && styles.has(TextElem::fill) {
                p.preserve_color = true;
            }
        }

        // Font (first family). The most common one is later hoisted into
        // `docDefaults` and stripped from matching runs.
        if let Some(first) = styles.get_ref(TextElem::font).into_iter().next() {
            p.font = Some(first.as_str().into());
        }

        // Weight → bold (base weight plus the semantic `strong` delta).
        let strong_delta = styles.get(TextElem::delta).0;
        if strong_delta > 0 {
            p.strong = true;
        }
        let weight = styles.get(TextElem::weight).to_number() as i64 + strong_delta;
        if weight >= 600 {
            p.bold = true;
        }

        // Italic, from the font style or an `emph` toggle.
        if styles.get(TextElem::style) != typst_library::text::FontStyle::Normal {
            p.italic = true;
        }
        let emph = styles.get(TextElem::emph).0;
        if emph {
            p.emphasis = true;
            p.italic = !p.italic;
        }

        // Super-/subscript, from `sub`/`super`.
        if let Some(shift) = styles.get_ref(TextElem::shift_settings) {
            p.vert_align = Some(match shift.kind {
                typst_library::text::ScriptKind::Sub => VertAlign::Sub,
                typst_library::text::ScriptKind::Super => VertAlign::Super,
            });
        }

        // Decorations, from `underline`/`strike`/`highlight`.
        for deco in styles.get_cloned(TextElem::deco) {
            match &deco.line {
                typst_library::text::DecoLine::Underline { stroke, .. } => {
                    p.underline = Some(underline_from_stroke(stroke));
                }
                typst_library::text::DecoLine::Strikethrough { .. } => p.strike = true,
                typst_library::text::DecoLine::Highlight { fill, .. } => {
                    apply_highlight(&mut p, fill.clone());
                }
                _ => {}
            }
        }
        // A user-specified link colour supplies the link's visual treatment.
        // Cancel the Hyperlink character style's inherited underline unless
        // Typst itself added one above. Ordinary links keep the conventional
        // style-provided blue underline.
        if p.preserve_color && p.underline.is_none() {
            p.underline = Some(Underline::none());
        }

        // Case and small capitals.
        if matches!(styles.get(TextElem::case), Some(typst_library::text::Case::Upper)) {
            p.caps = true;
        }
        if styles.get(TextElem::smallcaps).is_some() {
            p.smallcaps = true;
        }

        // G7 character spacing / tracking → run-level `w:spacing` (signed twips).
        // `tracking` resolves to an `Abs` (an `Em` relative to the font size).
        let tracking = styles.resolve(TextElem::tracking);
        if tracking != typst_library::layout::Abs::zero() {
            p.tracking = Some(props::abs_to_twip(tracking));
        }

        // G7b baseline shift → `w:position` (signed half-points). Typst's
        // `baseline` is downward-positive while `w:position` is upward-positive,
        // so negate.
        let baseline = styles.resolve(TextElem::baseline);
        if baseline != typst_library::layout::Abs::zero() {
            p.position_half_pt = Some((-baseline.to_pt() * 2.0).round() as i32);
        }

        // G6 run reading order. A run whose resolved direction is RTL gets
        // `w:rtl` (right-to-left glyph order) plus `w:cs` (so the complex-script
        // properties — `bCs`/`iCs`/`szCs`/`rFonts@cs` — actually apply).
        if !styles.resolve(TextElem::dir).is_positive() {
            p.rtl = true;
            p.cs = true;
        }

        // Language tag (`code[-region]`). The most common one is later hoisted
        // into `docDefaults` and stripped from matching runs.
        let lang_value = styles.get(TextElem::lang);
        let code = lang_value.as_str();
        p.lang = Some(match styles.get(TextElem::region) {
            Some(region) => ecow::eco_format!("{code}-{}", region.as_str()),
            None => code.into(),
        });

        p
    }

    /// Splits `text` into contiguous spans, each paired with the font family
    /// that should render it — mirroring Typst's own per-glyph font
    /// fallback (used during paged layout's shaping, `typst-layout`'s
    /// `get_font_and_covers`) instead of always using only the first
    /// declared family. A single declared font commonly can't cover every
    /// script in a run (e.g. a Latin heading font next to CJK glyphs);
    /// Typst's PDF path substitutes a covering font per-glyph during frame
    /// shaping, but typst-docx builds its runs from the pre-layout Content
    /// tree (a consequence of DOCX needing its own separate flowing
    /// realize — see `lib.rs`) and has no access to that per-glyph
    /// decision, so it previously just baked the first family into every
    /// OOXML font slot (including `w:eastAsia`), producing tofu wherever
    /// that family didn't cover a character (confirmed visually via a
    /// LibreOffice render: a Latin-only heading font produced tofu for CJK
    /// heading text while CJK body text, whose font matched the document's
    /// hoisted default, rendered fine). This walks `text` character by
    /// character using the same family list Typst's shaping code consults
    /// (`typst_library::text::families`), picks the first family (in
    /// priority order) that covers each character, falls back to the font
    /// book's script-aware fallback search when none of the declared
    /// families do, then coalesces consecutive same-font characters into
    /// maximal spans — so a single-script run (the common case) still
    /// produces exactly one span.
    fn split_by_font_coverage(
        &self,
        text: &str,
        styles: StyleChain,
    ) -> Vec<(EcoString, EcoString)> {
        let book = self.engine.world.book();
        let variant = typst_library::text::variant(styles);
        let families: Vec<&typst_library::text::FontFamily> =
            typst_library::text::families(styles).collect();
        let Some(first) = families.first() else {
            return vec![(EcoString::new(), text.into())];
        };

        // If the leading requested family isn't resolvable at all on this
        // machine, there is no coverage data to justify overriding it: DOCX
        // preserves font names as portable references for whatever
        // application eventually opens the file (unlike PDF, which must
        // embed real glyph outlines from a locally available font and so
        // is already limited to what's installed here) — a font simply
        // being absent from the compiling machine's font book is the
        // ordinary case, not evidence it lacks coverage. Keep the exact
        // original single-font behavior rather than guessing a local
        // substitute.
        let Some(first_info) =
            book.select(first.as_str(), variant).and_then(|id| book.info(id))
        else {
            return vec![(first.as_str().into(), text.into())];
        };

        // Fast path: the (locally resolvable) leading font already covers
        // every character — the overwhelming common case.
        if text.chars().all(|c| first_info.coverage.contains(c as u32)) {
            return vec![(first.as_str().into(), text.into())];
        }

        let like = Some(first_info);
        let mut spans: Vec<(EcoString, EcoString)> = Vec::new();
        for c in text.chars() {
            let mut chosen: Option<&str> = None;
            for family in &families {
                if let Some(id) = book.select(family.as_str(), variant)
                    && let Some(info) = book.info(id)
                    && info.coverage.contains(c as u32)
                {
                    chosen = Some(family.as_str());
                    break;
                }
            }
            let font_name: EcoString = match chosen.or_else(|| {
                // None of the declared families cover this character — fall
                // back the same way Typst's own shaping does.
                book.select_fallback(like, variant, c.encode_utf8(&mut [0; 4]))
                    .and_then(|id| book.info(id))
                    .map(|info| info.family.as_str())
            }) {
                Some(name) => name.into(),
                // No installed font covers this character at all — keep the
                // originally requested family; Word falls back to its own
                // missing-glyph handling, no worse than before this split.
                None => families.first().map(|f| f.as_str().into()).unwrap_or_default(),
            };
            match spans.last_mut() {
                Some((last_font, last_text)) if *last_font == font_name => {
                    last_text.push(c);
                }
                _ => spans.push((font_name, c.into())),
            }
        }
        spans
    }

    /// Applies `TextElem::case` to a string.
    pub fn apply_case(&self, styles: StyleChain, text: &EcoString) -> EcoString {
        match styles.get(TextElem::case) {
            Some(typst_library::text::Case::Lower) => {
                typst_library::text::Case::Lower.apply(text.as_str()).into()
            }
            Some(typst_library::text::Case::Upper) | None => text.clone(),
        }
    }

    /// Resolves a `ParElem`'s paragraph properties.
    pub fn resolve_par_props(
        &self,
        _elem: &Packed<typst_library::model::ParElem>,
        styles: StyleChain,
    ) -> ParaProps {
        use typst_library::foundations::Resolve;
        use typst_library::layout::{AlignElem, Em, FixedAlignment};
        use typst_library::model::{ParElem, ParLine};
        use typst_library::text::TextElem;

        let mut p = ParaProps::default();
        let rtl = !styles.resolve(TextElem::dir).is_positive();

        if self.line_numbering_active && styles.get_ref(ParLine::numbering).is_none() {
            p.suppress_line_numbers = true;
        }

        // G3 alignment. Justification (`w:jc="both"`) wins over horizontal
        // alignment; a left/start paragraph stays `None` for byte-identity with
        // the previously emitted output.
        if styles.get(ParElem::justify) {
            p.jc = Some(crate::dom::Jc::Both);
        } else {
            match styles.resolve(AlignElem::alignment).x {
                FixedAlignment::Center => p.jc = Some(crate::dom::Jc::Center),
                // Typst's fixed alignment is physical (Start = global left,
                // End = global right), while Word's `start`/`end` values are
                // logical and flip under `w:bidi`. Translate between the two.
                FixedAlignment::End if rtl => {
                    p.jc = Some(crate::dom::Jc::Start);
                }
                FixedAlignment::End => p.jc = Some(crate::dom::Jc::End),
                FixedAlignment::Start if rtl => {
                    p.jc = Some(crate::dom::Jc::End);
                }
                FixedAlignment::Start => {}
            }
        }

        // G6 paragraph base reading order (the run-level `w:rtl` is Slice C).
        if rtl {
            p.bidi = true;
        }

        let font_size = styles.resolve(TextElem::size);

        // G4 paragraph spacing -> symmetric Word before/after spacing. Typst
        // applies `par.spacing` above and below each paragraph and collapses
        // adjacent values to the greater one. Word applies the same maximum
        // rule to adjacent `w:after`/`w:before` values, so emitting both sides
        // preserves the model without inserting spacer paragraphs.
        let paragraph_spacing = props::abs_to_twip(styles.resolve(ParElem::spacing));
        if paragraph_spacing != 0 {
            p.typst_par_spacing = Some(paragraph_spacing);
            let spacing = p.spacing.get_or_insert_with(Default::default);
            spacing.before = Some(paragraph_spacing);
            spacing.after = Some(paragraph_spacing);
        }

        // G4 leading → `w:line` at-least, only when it differs from the engine
        // default of 0.65em (emitting it on every paragraph would change all
        // existing fixtures). A faithful at-least line height is the resolved
        // leading plus the font size.
        let leading = styles.resolve(ParElem::leading);
        let default_leading = Em::new(0.65).at(font_size);
        if (leading - default_leading).to_pt().abs() > 1e-3 {
            let line = props::abs_to_twip(leading + font_size);
            p.spacing.get_or_insert_with(Default::default).line = Some(line);
            // at-least (not exact, not auto-multiple): never clip a tall line.
            if let Some(sp) = &mut p.spacing {
                sp.line_rule_auto = false;
                sp.line_rule_at_least = true;
            }
        }

        // G4 first-line indent (apply on every paragraph when `all` is set; the
        // first-paragraph-only case is handled by the convert.rs loop).
        let fli = styles.get(ParElem::first_line_indent);
        let fli_amount = props::abs_to_twip(fli.amount().resolve(styles));
        if fli.all() && fli_amount != 0 {
            p.ind.get_or_insert_with(Default::default).first_line = Some(fli_amount);
        }

        // G4 hanging indent.
        let hang = props::abs_to_twip(styles.resolve(ParElem::hanging_indent));
        if hang != 0 {
            p.ind.get_or_insert_with(Default::default).hanging = Some(hang);
        }

        p
    }

    /// The first-line indent (twips) to apply to a paragraph that *follows*
    /// another paragraph, when `first-line-indent` is set with `all: false`
    /// (the default). Word's `w:firstLine` has no "skip the first paragraph"
    /// semantics, so the convert loop applies this only to consecutive
    /// paragraphs; the `all: true` case is handled inline in
    /// [`Self::resolve_par_props`]. Returns `None` when there is nothing to add.
    pub fn consecutive_first_line_indent(&self, styles: StyleChain) -> Option<i32> {
        use typst_library::foundations::Resolve;
        use typst_library::model::ParElem;
        let fli = styles.get(ParElem::first_line_indent);
        if fli.all() {
            return None;
        }
        let amount = props::abs_to_twip(fli.amount().resolve(styles));
        (amount != 0).then_some(amount)
    }

    // -- Inline flow --------------------------------------------------------

    /// Recursively lowers an inline body into runs.
    pub fn inline_runs(
        &mut self,
        body: &Content,
        styles: StyleChain,
        props: RunProps,
    ) -> SourceResult<Vec<Run>> {
        self.record_raw_ranges(body);
        // A `#place`-shape composition reached as paragraph content — e.g. a
        // `#box`/`#rect` with no visual of its own (so it has no block
        // structure of its own to preserve and its body is lowered here
        // directly, not via `convert_children`'s block dispatch) whose ENTIRE
        // content is a QR/barcode-style sequence of `#place`d shapes. See the
        // analogous check in `convert_children`/`handle_inline` for why laying
        // the whole thing out under `Target::Paged` recovers it as one native
        // drawing instead of rasterizing (or, before this check existed,
        // rasterizing once per `#place`).
        if crate::convert::contains_place(body)
            && crate::convert::placed_bodies_shape_only(body, styles)
            && let Some(run) = mappers::shape::transformed(body, styles, self)?
        {
            return Ok(vec![run]);
        }
        // Re-realize the body as a paragraph interior (`RealizationKind::Par`)
        // so inline content is NOT regrouped into separate block paragraphs.
        let arenas = Arenas::default();
        let children = (self.engine.library.routines.realize)(
            RealizationKind::Par,
            self.engine,
            self.locator,
            &arenas,
            body,
            styles,
        )?;
        let pairs: Vec<_> = children.to_vec();

        let mut runs = Vec::new();
        for (child, child_styles) in pairs {
            self.handle_inline(child, child_styles, &props, &mut runs)?;
        }
        Ok(runs)
    }

    /// Lowers a paragraph interior into paragraph children, preserving
    /// hyperlinks (`LinkElem` → `<w:hyperlink>`). Other inline elements lower to
    /// runs via [`Self::handle_inline`].
    pub fn inline_pchildren(
        &mut self,
        body: &Content,
        styles: StyleChain,
        props: RunProps,
    ) -> SourceResult<Vec<crate::dom::ParaChild>> {
        use crate::dom::ParaChild;
        self.record_raw_ranges(body);
        // See the identical check in `inline_runs`.
        if crate::convert::contains_place(body)
            && crate::convert::placed_bodies_shape_only(body, styles)
            && let Some(run) = mappers::shape::transformed(body, styles, self)?
        {
            return Ok(vec![ParaChild::Run(run)]);
        }
        let arenas = Arenas::default();
        let children = (self.engine.library.routines.realize)(
            RealizationKind::Par,
            self.engine,
            self.locator,
            &arenas,
            body,
            styles,
        )?;
        let pairs: Vec<_> = children.to_vec();

        let mut out: Vec<ParaChild> = Vec::new();
        // One semantic page reference can realize into multiple styled text
        // children (supplement, separator, number). They must share one complex
        // PAGEREF field, not become several fields that repeat the page number.
        let mut page_field: Option<(Span, Location, usize)> = None;
        for (child, child_styles) in pairs {
            // A labeled inline element (`… text <spot>`) is a valid `#link(<spot>)`
            // target; bracket the runs it produces with a bookmark so the link
            // resolves. (Tags carry no visible runs, so skip them.)
            let label_loc = (!child.is::<TagElem>())
                .then(|| child.location().filter(|_| child.label().is_some()))
                .flatten();
            let child_out_start = out.len();
            let direct_span = child_styles
                .get_cloned(LinkElem::direct_span)
                .unwrap_or_else(|| child.span());

            if let Some(elem) = child.to_packed::<TagElem>() {
                // Preserve inline introspection tags. These are how the
                // introspector learns about inline elements (citations,
                // references, inline labels, …); dropping them makes e.g.
                // a bibliography unable to find its citations, so they never
                // resolve and the document never converges.
                out.push(ParaChild::Tag(elem.tag.clone()));
            } else if let Some(elem) = child.to_packed::<LinkElem>() {
                out.extend(self.link_children(elem, child_styles, &props)?);
            } else if let Some(elem) = child.to_packed::<DirectLinkElem>() {
                let (_id, name) = self.add_bookmark(elem.loc);
                let runs = self.inline_runs(&elem.body, child_styles, props.clone())?;
                match elem.kind {
                    DirectLinkKind::PageReference => {
                        self.record_native_page_reference(&elem.clone().pack());
                        out.push(ParaChild::Run(Run::Field(Field {
                            instr: eco_format!(" PAGEREF {name} \\h "),
                            result: runs,
                            mode: FieldMode::Live,
                            display: FieldDisplay::Visible,
                            cache_status: FieldCacheStatus::Resolved,
                        })));
                    }
                    DirectLinkKind::Reference
                    | DirectLinkKind::PageReferenceSupplement
                    | DirectLinkKind::Other => {
                        if elem.kind == DirectLinkKind::Reference {
                            let content = elem.clone().pack();
                            self.record_content_decision(
                                &content,
                                Representation::Approximate,
                                DecisionReason::TypstOwnedReferenceText,
                                LossSet::DYNAMIC_BEHAVIOR,
                                0,
                            );
                        }
                        out.push(ParaChild::Hyperlink {
                            rel: None,
                            anchor: Some(name),
                            runs,
                        });
                    }
                }
            } else if let Some((fbody, fill, bdr)) =
                mappers::shape::inline_frame(child, child_styles)
                && (fill.is_some() || bdr.is_some())
                && crate::convert::body_extractable(&fbody)
            {
                // An inline framed container (`#box(fill|stroke)[..]`) at the
                // paragraph-child level: recurse through `inline_pchildren` (not
                // `inline_runs`) so a link inside keeps its `<w:hyperlink>` wrapper.
                // The box's shading + border is threaded onto every run — including
                // the link's — and adjacent identical run borders merge into one
                // visual box. (The run-level `handle_inline` path can only emit
                // runs, so it still flattens links there; that path is only hit in
                // nested run-only contexts where a hyperlink can't appear anyway.)
                let mut p = props.clone();
                if let Some(f) = fill {
                    p.shd_fill.get_or_insert(f);
                }
                if let Some(b) = bdr {
                    p.bdr.get_or_insert(b);
                }
                out.extend(self.inline_pchildren(&fbody, child_styles, p)?);
            } else if let Some(dest) = child_styles.get_cloned(LinkElem::current) {
                // Linked content whose marker element was stripped during
                // realization — a cross-reference (`@fig`), bibliography
                // back-reference, etc. The destination survives as the
                // `LinkElem::current` style (paged layout reads the same style),
                // so re-wrap the runs in a real `<w:hyperlink>` to make the
                // reference clickable. The runs keep their own colour (Word
                // cross-references are not blue-underlined like URL links).
                let mut runs = Vec::new();
                self.handle_inline(child, child_styles, &props, &mut runs)?;
                match dest {
                    Destination::Location(loc) => {
                        let (_id, name) = self.add_bookmark(loc);
                        match child_styles
                            .get_cloned(LinkElem::direct_kind)
                            .unwrap_or(DirectLinkKind::Other)
                        {
                            DirectLinkKind::PageReference => {
                                self.record_native_page_reference(child);
                                if let Some((span, previous_loc, index)) = page_field
                                    && span == direct_span
                                    && previous_loc == loc
                                    && let Some(ParaChild::Run(Run::Field(field))) =
                                        out.get_mut(index)
                                {
                                    field.result.extend(runs);
                                } else {
                                    let index = out.len();
                                    out.push(ParaChild::Run(Run::Field(Field {
                                        instr: eco_format!(" PAGEREF {name} \\h "),
                                        result: runs,
                                        mode: FieldMode::Live,
                                        display: FieldDisplay::Visible,
                                        cache_status: FieldCacheStatus::Resolved,
                                    })));
                                    page_field = Some((direct_span, loc, index));
                                }
                            }
                            DirectLinkKind::Reference => {
                                self.record_content_decision(
                                    child,
                                    Representation::Approximate,
                                    DecisionReason::TypstOwnedReferenceText,
                                    LossSet::DYNAMIC_BEHAVIOR,
                                    0,
                                );
                                out.push(ParaChild::Hyperlink {
                                    rel: None,
                                    anchor: Some(name),
                                    runs,
                                });
                            }
                            DirectLinkKind::PageReferenceSupplement
                            | DirectLinkKind::Other => {
                                out.push(ParaChild::Hyperlink {
                                    rel: None,
                                    anchor: Some(name),
                                    runs,
                                });
                            }
                        }
                    }
                    Destination::Url(url) => {
                        let rel = self.add_external_rel(url.as_str());
                        out.push(ParaChild::Hyperlink {
                            rel: Some(rel),
                            anchor: None,
                            runs,
                        });
                    }
                    // A page/coordinate destination has no DOCX equivalent: emit
                    // the text without a link.
                    Destination::Position(_) => {
                        out.extend(runs.into_iter().map(ParaChild::Run));
                    }
                }
            } else if child.is::<typst_library::layout::PlaceElem>()
                || (child
                    .to_packed::<typst_library::layout::BoxElem>()
                    .is_some_and(|b| box_is_plain(b, child_styles))
                    && crate::convert::contains_place(child)
                    // Only pure layout scaffolding (no visible text): a
                    // text-bearing place-box keeps its existing live-extraction
                    // path — rasterizing it here would demote live text to an
                    // image just to reposition its tags.
                    && !contains_visible_text(child))
            {
                // An inline `#place` — or a plain `#box` holding one — that the
                // shape-composition path (checked at the top of this function)
                // declined. It rasterizes exactly as before, but at this
                // paragraph-child level its harvested frame tags can stay AT
                // THIS POSITION instead of being deferred to the end of the
                // document. A placed body is often layout scaffolding whose
                // only real output is a state/counter update (e.g. the
                // `drafting` package stores page properties from inside a
                // `box(place(layout(..)))`), and a later `state.get()` only
                // sees the update if it precedes the read in tag order.
                let (tags, runs, failed) = mappers::image::laid_out_fallback_with_tags(
                    child,
                    child_styles,
                    self,
                )?;
                out.extend(tags.into_iter().map(ParaChild::Tag));
                out.extend(runs.into_iter().map(ParaChild::Run));
                if failed {
                    self.record_content_drop(
                        child,
                        DecisionReason::InlinePositionedContentUnavailable,
                        "inline placed content and whole-region fallback produced no output",
                    );
                }
            } else {
                let mut runs = Vec::new();
                self.handle_inline(child, child_styles, &props, &mut runs)?;
                out.extend(runs.into_iter().map(ParaChild::Run));
            }

            // Bracket a labeled inline child's output with a bookmark so a
            // `#link(<label>)` to it resolves.
            if let Some(loc) = label_loc
                && out.len() > child_out_start
                && let Some((id, name)) = self.bookmark_for_emission(loc)
            {
                out.insert(child_out_start, ParaChild::BookmarkStart { id, name });
                out.push(ParaChild::BookmarkEnd { id });
            }
        }

        // A cross-reference often realizes as several runs (supplement + space +
        // number), each carrying the same destination, so the loop above made one
        // `<w:hyperlink>` per run. Coalesce directly-adjacent hyperlinks with the
        // same target into a single one.
        let mut merged: Vec<ParaChild> = Vec::with_capacity(out.len());
        for child in out {
            if let ParaChild::Hyperlink { rel, anchor, mut runs } = child {
                if let Some(ParaChild::Hyperlink {
                    rel: prev_rel,
                    anchor: prev_anchor,
                    runs: prev_runs,
                }) = merged.last_mut()
                    && *prev_rel == rel
                    && *prev_anchor == anchor
                {
                    prev_runs.append(&mut runs);
                    continue;
                }
                merged.push(ParaChild::Hyperlink { rel, anchor, runs });
            } else {
                merged.push(child);
            }
        }
        Ok(merged)
    }

    /// Handles a single realized inline child, appending runs to `out`.
    pub(crate) fn handle_inline(
        &mut self,
        child: &Content,
        styles: StyleChain,
        props: &RunProps,
        out: &mut Vec<Run>,
    ) -> SourceResult<()> {
        if let Some(elem) = child.to_packed::<TagElem>() {
            // Run-only context (table cell / footnote / nested inline body): we
            // can't position the tag among runs, but the introspector still needs
            // it so labels and references inside resolve. Defer it.
            self.deferred_tags.push(elem.tag.clone());
        } else if child.is::<SpaceElem>() {
            self.push_text(out, props.clone(), " ".into());
        } else if let Some(elem) = child.to_packed::<TextElem>() {
            let text = self.apply_case(styles, &elem.text);
            let mut rp = self.resolve_text_props(styles, props.clone());
            if self.span_is_raw(elem.span()) {
                rp.no_proof = true;
            }
            let spans = self.split_by_font_coverage(&text, styles);
            let review_origin = (spans.len() == 1)
                .then(|| {
                    (!elem.span().is_detached()).then(|| {
                        self.review_origin(elem.span(), ReviewCandidateKind::InlineText)
                    })
                })
                .flatten()
                .or(rp.review_origin);
            for (font, span) in spans {
                let mut span_rp = rp.clone();
                span_rp.review_origin = review_origin;
                if !font.is_empty() {
                    span_rp.font = Some(font);
                }
                self.push_text(out, span_rp, span);
            }
        } else if let Some(elem) = child.to_packed::<HElem>() {
            use typst_library::foundations::Resolve;
            use typst_library::layout::Spacing;
            // Horizontal spacing. Word has no exact inline-advance primitive, so
            // approximate rather than drop it: fractional `#h(1fr)` (the
            // push-apart idiom) → a tab; a fixed `#h(..)` → proportional spaces;
            // zero → nothing.
            if elem.amount.is_zero() {
                // skip
            } else if elem.amount.is_fractional() {
                out.push(Run::FillTab);
            } else if let Spacing::Rel(rel) = elem.amount {
                let pt = rel.abs.resolve(styles).to_pt();
                let n = ((pt / 3.5).round() as i64).clamp(1, 40) as usize;
                self.push_text(out, props.clone(), " ".repeat(n).into());
            }
        } else if child.is::<LinebreakElem>() {
            out.push(Run::Break);
            self.last_char = None;
        } else if child.is::<typst_library::model::ParbreakElem>() {
            // A paragraph break that reached a run-only context (a footnote, a
            // table cell rendered inline, …) cannot start a new `<w:p>`, but it
            // must not silently merge the two paragraphs — emit a line break so
            // the visual separation survives. (Block contexts split into real
            // paragraphs upstream and never reach here.)
            out.push(Run::Break);
            self.last_char = None;
        } else if let Some(elem) = child.to_packed::<typst_library::layout::VElem>() {
            // Vertical spacing that reached a run-only context (a plain box's
            // body extracted inline, a footnote, …) — like `ParbreakElem` above,
            // Word has no run-level vertical-space primitive, so approximate
            // with a line break rather than silently dropping it (which would
            // otherwise crush the surrounding content together with no gap at
            // all — worse than an imprecise break). Zero amount → genuinely
            // nothing to preserve.
            if !elem.amount.is_zero() {
                out.push(Run::Break);
                self.last_char = None;
            }
        } else if let Some(elem) = child.to_packed::<SmartQuoteElem>() {
            let double = elem.double.get(styles);
            let quote: EcoString = if elem.enabled.get(styles) {
                let quotes = SmartQuotes::get(
                    elem.quotes.get_ref(styles),
                    styles.get(TextElem::lang),
                    styles.get(TextElem::region),
                    elem.alternative.get(styles),
                );
                self.quoter.quote(self.last_char, &quotes, double).into()
            } else {
                SmartQuotes::fallback(double).into()
            };
            let rp = self.resolve_text_props(styles, props.clone());
            self.push_text(out, rp, quote);
        } else if let Some(elem) = child.to_packed::<StrongElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            p.strong = true;
            p.bold = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<EmphElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            p.emphasis = true;
            p.italic = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<SubElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            p.vert_align = Some(VertAlign::Sub);
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<SuperElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            p.vert_align = Some(VertAlign::Super);
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<UnderlineElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            use typst_library::foundations::{Resolve, Smart};
            p.underline = Some(match elem.stroke.get_cloned(styles) {
                Smart::Custom(stroke) => underline_from_stroke(&stroke.resolve(styles)),
                Smart::Auto => Underline::single(),
            });
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<StrikeElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            p.strike = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<HighlightElem>() {
            let mut p = props.clone();
            p.review_origin =
                Some(self.review_origin(child.span(), ReviewCandidateKind::InlineText));
            apply_highlight(&mut p, elem.fill.get_cloned(styles));
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<RawElem>() {
            out.extend(self.with_raw_scope(|ctx| {
                ctx.inline_runs(elem.pack_ref(), styles, props.clone())
            })?);
        } else if let Some(elem) = child.to_packed::<RawLine>() {
            out.extend(self.with_raw_scope(|ctx| {
                ctx.inline_runs(&elem.body, styles, props.clone())
            })?);
        } else if let Some(elem) = child.to_packed::<SmallcapsElem>() {
            let mut p = props.clone();
            p.smallcaps = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<EquationElem>()
            && !elem.block.get(styles)
        {
            match mappers::math::equation(elem, styles, self)? {
                mappers::math::EquationOut::Inline(runs) => out.extend(runs),
                mappers::math::EquationOut::Block(_) => {}
            }
        } else if let Some(elem) = child.to_packed::<ImageElem>() {
            out.push(mappers::image::image(elem, styles, self)?);
        } else if let Some(elem) = child.to_packed::<RefElem>() {
            out.extend(mappers::reference::reference(elem, styles, props.clone(), self)?);
        } else if let Some(elem) = child.to_packed::<FootnoteElem>() {
            if self.in_footnote {
                // A footnote inside a footnote is illegal in Word; inline the
                // inner note's body text so the content survives without a nested
                // reference (which would make the file unopenable).
                out.extend(mappers::footnote::flatten_nested(elem, styles, props, self)?);
            } else {
                out.push(mappers::footnote::footnote(elem, styles, self)?);
            }
        } else if let Some(elem) = child.to_packed::<DirectLinkElem>() {
            let runs = self.inline_runs(&elem.body, styles, props.clone())?;
            match elem.kind {
                DirectLinkKind::PageReference => {
                    self.record_native_page_reference(&elem.clone().pack());
                    let (_id, name) = self.add_bookmark(elem.loc);
                    out.push(Run::Field(Field {
                        instr: eco_format!(" PAGEREF {name} \\h "),
                        result: runs,
                        mode: FieldMode::Live,
                        display: FieldDisplay::Visible,
                        cache_status: FieldCacheStatus::Resolved,
                    }));
                }
                DirectLinkKind::Reference => {
                    let (_id, name) = self.add_bookmark(elem.loc);
                    out.push(Run::Field(Field {
                        instr: eco_format!(" REF {name} \\h "),
                        result: runs,
                        mode: FieldMode::Static,
                        display: FieldDisplay::Visible,
                        cache_status: FieldCacheStatus::Resolved,
                    }));
                }
                DirectLinkKind::PageReferenceSupplement | DirectLinkKind::Other => {
                    out.extend(runs)
                }
            }
        } else if let Some(elem) = child.to_packed::<LinkMarker>() {
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::LayoutElem>()
        {
            // An inline `#layout(size => ..)` is often pure layout-time
            // scaffolding whose only real output is a state/counter update —
            // e.g. the `drafting` package's `set-page-properties` stores the
            // page dimensions via `place(layout(.. state.update ..))`, and its
            // margin notes then read that state *at their own position*.
            // Rasterizing the closure would ship its introspection tags through
            // `deferred_tags` (appended after the whole body), so the state
            // read — earlier in document order — would still see the initial
            // value. Evaluating the closure here (with the same synthetic page
            // size the block-level `handle_layout` uses) keeps any tags at the
            // exact position paged layout would give them. A closure that
            // cannot be evaluated standalone falls back to rasterization.
            if let Some(content) = self.eval_layout_content(elem, styles) {
                out.extend(self.inline_runs(&content, styles, props.clone())?);
            } else {
                self.rasterize_fallback(child, styles, out)?;
            }
        } else if let Some((body, fill, bdr)) =
            mappers::shape::inline_frame(child, styles)
            && (fill.is_some() || bdr.is_some())
            && crate::convert::body_extractable(&body)
        {
            // An *inline* framed container (`#box(fill|stroke)[..]` mid-line) →
            // boxed text via run shading + a run border, which flows correctly in
            // the line. (An inline Word *text box* does NOT flow its content —
            // Word/LibreOffice render it as a displaced empty frame — so text
            // boxes are reserved for standalone block-level boxes, handled in
            // `convert::handle_block`.) A footnote inside is fine here: its run
            // stays in the main story. A non-extractable body (a per-line equation
            // label) or a gradient-only fill falls through to rasterization.
            let mut p = props.clone();
            if let Some(f) = fill {
                p.shd_fill.get_or_insert(f);
            }
            if let Some(b) = bdr {
                p.bdr.get_or_insert(b);
            }
            out.extend(self.inline_runs(&body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::BoxElem>() {
            // A non-text-box `#box` (no visible frame, or a body that must
            // rasterize): keep the existing rasterize/extract handling.
            match elem.body.get_cloned(styles) {
                // An empty `#box` (`#box(width: 1em)` spacer): nothing to render.
                None => {}
                Some(body) => {
                    // A box whose sole body is a bare image with no size of its
                    // own (the common icon idiom — `box(height: 10pt,
                    // image(name))`, Typst's own documented example): the
                    // *box's* height is the only sizing information for this
                    // image, but the "plain box" extraction below discards the
                    // box's geometric constraint entirely (it has no inline-flow
                    // equivalent for ordinary text) — for an image that means
                    // falling back to its full intrinsic pixel size, producing a
                    // giant image where a small inline icon was intended
                    // (confirmed visually: a CV's tiny GitHub/GitLab/LinkedIn
                    // icons became half-page-sized images). Propagate the box's
                    // height onto a cloned image before lowering it — matching
                    // the aspect-ratio-preserving "height only" branch
                    // `display_extents` already implements for an image with its
                    // own explicit height — instead of discarding the
                    // constraint.
                    if let Some(image_elem) = body.to_packed::<ImageElem>()
                        && matches!(
                            image_elem.height.get(styles),
                            typst_library::layout::Sizing::Auto
                        )
                        && let typst_library::foundations::Smart::Custom(rel) =
                            elem.height.get(styles)
                    {
                        let sized = (**image_elem)
                            .clone()
                            .with_height(typst_library::layout::Sizing::Rel(rel));
                        out.push(mappers::image::image(
                            &typst_library::foundations::Packed::new(sized),
                            styles,
                            self,
                        )?);
                    } else if box_is_plain(elem, styles)
                        && crate::convert::body_extractable(&body)
                        && crate::convert::body_inline_extractable(&body)
                    {
                        // A *plain* box — no fill, stroke or clip, so nothing
                        // visual to preserve — is just inline content held
                        // together (`#box[..]` to prevent a line break,
                        // `#box(width: ..)[label]`, a baseline shift). Extract
                        // its runs so the text stays selectable instead of
                        // rasterizing it to an image; the only thing lost is the
                        // box's geometric constraint, which has no inline-flow
                        // equivalent. `body_extractable` still guards the one
                        // layout-bound case (a per-line equation label inside).
                        out.extend(self.inline_runs(&body, styles, props.clone())?);
                    } else {
                        // Rasterize the box (this keeps a styled box's visual
                        // and, for a box that lays out real content, its labels
                        // via the frame tag harvest). If it lays out to
                        // *nothing* — a degenerate box whose rasterization would
                        // otherwise be dropped — extract its text so the content
                        // survives. Such a box has no laid-out content and hence
                        // no labels to orphan, so plain extraction is safe; we
                        // discard any partial tag harvest first to be sure.
                        let mark = self.deferred_tags.len();
                        let runs =
                            mappers::image::laid_out_fallback(child, styles, self)?;
                        if !runs.is_empty() {
                            // Keep Word's figure counter consistent with any
                            // captioned figure the box rasterized (a hidden
                            // `SEQ … \h`), as the generic fallback does.
                            self.emit_rasterized_figure_seqs(mark, styles, out);
                            out.extend(runs);
                        } else {
                            self.deferred_tags.truncate(mark);
                            out.extend(self.inline_runs(&body, styles, props.clone())?);
                        }
                    }
                }
            }
        } else if let Some(elem) = child.to_packed::<typst_library::pdf::PdfMarkerTag>() {
            // A tagged-PDF accessibility delimiter wraps real inline content; it
            // has no DOCX meaning, so unwrap it and lower the body.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::pdf::ArtifactElem>() {
            // `#pdf.artifact[..]` marks content as decorative for PDF
            // accessibility (a repeated logo, a code listing's line-number
            // gutter, …) — DOCX has no artifact concept, so unwrap and lower
            // the body like any other content instead of rasterizing the
            // whole marked region (which had been silently discarding
            // whatever real text/shapes it wrapped).
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::GridCell>() {
            // A bare `grid.cell(..)` reached as ordinary inline content — see
            // the matching arm in `convert::convert_children` for how this
            // shows up (a user-authored `grid.cell(..)` nested inside
            // `#pdf.artifact(..)`, one layer inside Typst's own uniform outer
            // cell wrapper). Unwrap and lower its body like any other wrapper.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::model::TableCell>() {
            // Same as `GridCell` above, for `#table.cell(..)`.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::HideElem>() {
            // `#hide[..]`: the content is REMOVED, not embedded. Typst documents
            // `hide` as a redaction tool ("neither present visually nor
            // accessible to Assistive Technology"), and paged export honours
            // that by physically dropping every frame item (`Frame::hide`)
            // except introspection tags. Match it exactly: harvest the body's
            // tags (so a label or citation inside hidden content still
            // resolves — the same "traces" the paged model keeps) and emit
            // nothing. Lowering the body to `w:vanish` runs instead would ship
            // the text recoverable inside the package (Word reveals hidden
            // text with a single toggle) — a redaction leak.
            let _ = elem.body.traverse(&mut |c: Content| {
                if let Some(tag) = c.to_packed::<TagElem>() {
                    self.deferred_tags.push(tag.tag.clone());
                }
                std::ops::ControlFlow::<()>::Continue(())
            });
        } else if let Some(elem) = child.to_packed::<LinkElem>() {
            // In a run-only context (nested formatting, table/footnote bodies) we
            // cannot emit a `<w:hyperlink>` wrapper, so lower the link body to
            // runs. Paragraph-level links go through `link_children` instead.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(run) = mappers::shape::shape(child, styles, self)? {
            // A decorative vector shape (`#rect`/`#circle`/`#polygon`/…) maps to a
            // DrawingML `wps:wsp` shape instead of a rasterized image.
            out.push(run);
        } else if let Some(elem) =
            child.to_packed::<typst_library::visualize::CurveElem>()
            && let Some(run) = mappers::shape::curve(elem, styles, self)?
        {
            // `#curve` — straight and cubic-Bézier segments — maps to a native
            // `a:custGeom` path instead of rasterizing (a solid fill/stroke; a
            // gradient/tiling one falls through to the rasterize path below).
            out.push(run);
        } else if let Some(elem) = child.to_packed::<typst_library::visualize::LineElem>()
            && let Some(run) = mappers::shape::line(elem, styles, self)?
        {
            // A diagonal or explicit-endpoint `#line` (a horizontal rule is
            // already handled as a paragraph border, earlier in the block
            // dispatch) maps to a native open path, same as `curve` above.
            out.push(run);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::MoveElem>() {
            if let Some(run) = mappers::shape::move_(elem, styles, self)? {
                // `#move(dx:, dy:)[..]` whose ENTIRE body is native-representable
                // shapes/lines/curves maps to a single shape or a `wpg:wgp` group,
                // instead of rasterizing the whole composition — the dominant
                // real-world rasterize cause (a hand-drawn diagram built from a
                // few `#move`d primitives).
                out.push(run);
            } else if let Some(runs) =
                mappers::shape::move_text(elem, styles, props, self)?
            {
                // A pure vertical nudge (`dx` ~0) of plain text/inline content —
                // e.g. a baseline tweak on an icon's caption — recovers as real,
                // searchable/editable runs carrying a `w:position` shift instead
                // of rasterizing (Word's own contract for `w:position` is the
                // same "shift the run without affecting the line's height" as
                // `#move`'s own "without affecting layout").
                out.extend(runs);
            } else {
                self.rasterize_fallback(child, styles, out)?;
            }
        } else if (child.is::<typst_library::layout::RotateElem>()
            || child.is::<typst_library::layout::ScaleElem>())
            && let Some(run) = mappers::shape::transformed(child, styles, self)?
        {
            // A bare (not `#move`-wrapped) `#rotate`/`#scale` whose body is a
            // native shape/composition — the same recovery `move_` does, just
            // without a translate step (see `mappers::shape::transformed`).
            out.push(run);
        } else if (child.is::<typst_library::layout::BlockElem>()
            || crate::convert::is_framed_container(child))
            && crate::convert::contains_place(child)
            && crate::convert::placed_bodies_shape_only(child, styles)
            && let Some(run) = mappers::shape::transformed(child, styles, self)?
        {
            // A box/block/framed container reached as a paragraph's sole
            // content (the common case — `#box(..)[..]` at the top level is
            // wrapped in its own `ParElem`, so it lands here rather than in
            // `convert_children`'s block dispatch) whose ENTIRE content is a
            // composition of `#place`-positioned native shapes: the same
            // recovery as the analogous check in `convert_children` (see its
            // comment for why laying the whole container out under
            // `Target::Paged` works — `#place`'s own non-floating layout
            // already composites into the SAME frame via an ordinary
            // `push_frame`, so this is just an ordinary shape composition from
            // `collect_shapes`'s point of view).
            out.push(run);
        } else {
            self.rasterize_fallback(child, styles, out)?;
        }
        Ok(())
    }

    /// No idiomatic representation (a drawn shape, a PDF image, an
    /// externally-rendered figure, …): rasterize it and embed as an image so
    /// the content survives instead of being dropped.
    fn rasterize_fallback(
        &mut self,
        child: &Content,
        styles: StyleChain,
        out: &mut Vec<Run>,
    ) -> SourceResult<()> {
        let before = self.deferred_tags.len();
        let runs = mappers::image::laid_out_fallback(child, styles, self)?;
        // A figure whose *container* we rasterized (e.g. a `wrap-content`
        // figure) never reaches the figure mapper, so it emits no visible
        // `SEQ` field — Word's caption counter would then under-count and
        // drift from the introspector-baked cross-reference numbers. Emit a
        // hidden `SEQ \h` (increment without display) for each such figure,
        // keeping Word's numbering consistent with the references.
        self.emit_rasterized_figure_seqs(before, styles, out);
        if !runs.is_empty() {
            out.extend(runs);
        } else if !crate::convert::is_invisible_noop(child) {
            // Only warn about a genuine drop. Invisible no-ops (spacing, a
            // hidden body, layout scaffolding) render nothing in the PDF
            // either, so a warning would be a false alarm.
            self.warn_ignored(child.elem().name(), child.span())
        }
        Ok(())
    }

    /// Lowers a paragraph-level [`LinkElem`] into paragraph children, preserving
    /// the `<w:hyperlink>` wrapper (used by the block-flow paragraph assembler).
    pub fn link_children(
        &mut self,
        elem: &Packed<LinkElem>,
        styles: StyleChain,
        props: &RunProps,
    ) -> SourceResult<Vec<crate::dom::ParaChild>> {
        let mut props = props.clone();
        props.review_origin =
            Some(self.review_origin(elem.span(), ReviewCandidateKind::InlineText));
        mappers::reference::link(elem, styles, props, self)
    }

    /// Pushes a text run, coalescing with a preceding identical-props run.
    fn push_text(&mut self, out: &mut Vec<Run>, mut props: RunProps, text: EcoString) {
        if self.raw_depth > 0 {
            props.no_proof = true;
        }
        self.last_char = text.chars().last().or(self.last_char);
        if let Some(Run::Text { props: last_props, text: last_text }) = out.last_mut()
            && *last_props == props
        {
            last_text.push_str(&text);
            return;
        }
        out.push(Run::Text { props, text });
    }

    // -- Block flow ---------------------------------------------------------

    /// Recursively lowers a block body into a sequence of blocks.
    pub fn blocks(
        &mut self,
        body: &Content,
        styles: StyleChain,
    ) -> SourceResult<Vec<Block>> {
        self.record_raw_ranges(body);
        let arenas = Arenas::default();
        let children = self.realize_fragment(&arenas, body, styles)?;
        let pairs: Vec<_> = children.to_vec();
        crate::convert::convert_children(self, &pairs)
    }

    /// Lowers a separate OPC part's content (a header/footer body) into blocks,
    /// collecting the relationships it creates (images, external links) into that
    /// part's own relationships table — which the caller writes as
    /// `word/_rels/<part>.rels`. An `r:id` in a header/footer part must resolve
    /// against its own `.rels`, not the document's.
    pub fn part_blocks(
        &mut self,
        body: &Content,
        styles: StyleChain,
    ) -> SourceResult<(Vec<Block>, Rels)> {
        let saved = self.part_rels.take();
        self.part_rels = Some(Rels::new());
        let result = self.blocks(body, styles);
        let rels = self.part_rels.take().unwrap_or_default();
        self.part_rels = saved;
        Ok((result?, rels))
    }

    /// Realizes a fragment body into native pairs.
    fn realize_fragment<'b>(
        &mut self,
        arenas: &'b Arenas,
        content: &'b Content,
        styles: StyleChain<'b>,
    ) -> SourceResult<Vec<typst_library::routines::Pair<'b>>> {
        (self.engine.library.routines.realize)(
            RealizationKind::Fragment { kind: &mut FragmentKind::Block },
            self.engine,
            self.locator,
            arenas,
            content,
            styles,
        )
    }
}

/// Whether a `#box` carries no visual of its own (no fill, no stroke on any
/// side, no clip) — so it is pure inline layout and its body can be extracted
/// as runs rather than rasterized to preserve a background/border/clip.
/// Whether `content` contains any visible text (a non-empty `TextElem`
/// anywhere inside). Used to tell pure layout scaffolding — a `#box(place(
/// layout(..)))` whose only real output is a state/counter update — apart from
/// text-bearing content that should stay on a live-extraction path.
fn contains_visible_text(content: &Content) -> bool {
    use std::ops::ControlFlow;
    content
        .traverse(&mut |e: Content| {
            if let Some(text) = e.to_packed::<TextElem>()
                && !text.text.trim().is_empty()
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
        .is_break()
}

fn box_is_plain(
    elem: &typst_library::foundations::Packed<typst_library::layout::BoxElem>,
    styles: StyleChain,
) -> bool {
    if elem.fill.get_cloned(styles).is_some() || elem.clip.get(styles) {
        return false;
    }
    let s = elem.stroke.get_cloned(styles);
    s.top.is_none() && s.bottom.is_none() && s.left.is_none() && s.right.is_none()
}

/// Derives a Word underline style + colour from a resolved line stroke.
///
/// The dash pattern maps to `w:val` (a dotted/dashed/dot-dashed line); a paint
/// distinct from `auto` maps to `w:color`. An `auto` stroke (the common case)
/// yields a plain single underline with no colour — byte-identical to the
/// original boolean underline.
fn underline_from_stroke(stroke: &typst_library::visualize::Stroke<Abs>) -> Underline {
    use typst_library::foundations::Smart;
    use typst_library::visualize::Paint;

    let val = match &stroke.dash {
        Smart::Custom(Some(pattern)) => classify_dash(&pattern.array),
        _ => "single",
    };
    let color = match &stroke.paint {
        Smart::Custom(Paint::Solid(c)) => Some(props::color_to_hex(c)),
        _ => None,
    };
    Underline { val, color }
}

const TYPST_DEFAULT_HIGHLIGHT: [u8; 3] = [0xFF, 0xFD, 0x11];

fn apply_highlight(props: &mut RunProps, fill: Option<Paint>) {
    let rgb = highlight_rgb(fill);
    if let Some(name) = word_highlight_name(rgb) {
        props.highlight = Some(name);
        props.shd_fill = None;
    } else {
        props.highlight = None;
        props.shd_fill = Some(rgb);
    }
}

fn style_owned_heading_props(props: RunProps) -> RunProps {
    RunProps {
        font: props.font,
        bold: props.bold,
        italic: props.italic,
        color: props.color,
        size_half_pt: props.size_half_pt,
        ..RunProps::default()
    }
}

fn highlight_rgb(fill: Option<Paint>) -> [u8; 3] {
    match fill {
        Some(Paint::Solid(color)) => props::color_to_hex(&color),
        Some(Paint::Gradient(gradient)) => {
            props::gradient_shade_hex(&gradient).unwrap_or(TYPST_DEFAULT_HIGHLIGHT)
        }
        Some(Paint::Tiling(_)) | None => TYPST_DEFAULT_HIGHLIGHT,
    }
}

fn word_highlight_name(rgb: [u8; 3]) -> Option<&'static str> {
    const TOLERANCE: u8 = 24;
    const VALUES: &[(&str, [u8; 3])] = &[
        ("black", [0x00, 0x00, 0x00]),
        ("blue", [0x00, 0x00, 0xFF]),
        ("cyan", [0x00, 0xFF, 0xFF]),
        ("darkBlue", [0x00, 0x00, 0x80]),
        ("darkCyan", [0x00, 0x80, 0x80]),
        ("darkGray", [0x80, 0x80, 0x80]),
        ("darkGreen", [0x00, 0x80, 0x00]),
        ("darkMagenta", [0x80, 0x00, 0x80]),
        ("darkRed", [0x80, 0x00, 0x00]),
        ("darkYellow", [0x80, 0x80, 0x00]),
        ("green", [0x00, 0xFF, 0x00]),
        ("lightGray", [0xC0, 0xC0, 0xC0]),
        ("magenta", [0xFF, 0x00, 0xFF]),
        ("red", [0xFF, 0x00, 0x00]),
        ("white", [0xFF, 0xFF, 0xFF]),
        ("yellow", [0xFF, 0xFF, 0x00]),
    ];

    VALUES
        .iter()
        .filter_map(|(name, candidate)| {
            let distance = rgb
                .into_iter()
                .zip(*candidate)
                .map(|(a, b)| u8::abs_diff(a, b))
                .max()
                .unwrap_or(0);
            (distance <= TOLERANCE).then_some((*name, distance))
        })
        .min_by_key(|(_, distance)| *distance)
        .map(|(name, _)| name)
}

/// Classifies a resolved dash array's "on" segments (the even indices; the odd
/// ones are gaps) into the nearest Word underline style. The named Typst dash
/// presets use a line-width "dot" for dotted lines and explicit lengths for
/// dashes, so a dot-only pattern is `dotted`, a length-only one is `dash`, and a
/// mix (dash-dotted) is `dotDash`.
fn classify_dash(array: &[typst_library::visualize::DashLength<Abs>]) -> &'static str {
    use typst_library::visualize::DashLength;
    if array.is_empty() {
        return "single";
    }
    let mut has_dot = false;
    let mut has_dash = false;
    for on in array.iter().step_by(2) {
        match on {
            DashLength::LineWidth => has_dot = true,
            DashLength::Length(_) => has_dash = true,
        }
    }
    match (has_dot, has_dash) {
        (true, true) => "dotDash",
        (true, false) => "dotted",
        _ => "dash",
    }
}

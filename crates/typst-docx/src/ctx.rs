//! The mutable conversion context [`DocxCtx`] and the inline/block flow.

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use typst_library::diag::{SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::introspection::{Locator, SplitLocator, Tag, TagElem};
use typst_library::layout::{Abs, Frame, FrameItem, HElem};
use typst_library::math::EquationElem;
use typst_library::model::{EmphElem, StrongElem};
use typst_library::routines::{Arenas, FragmentKind, RealizationKind};
use typst_library::text::{
    LinebreakElem, SmartQuoteElem, SmartQuoter, SmartQuotes, SpaceElem, TextElem,
    SubElem, SuperElem,
};
use typst_library::text::{HighlightElem, SmallcapsElem, StrikeElem, UnderlineElem};
use typst_library::visualize::ImageElem;
use typst_library::model::{
    DirectLinkElem, Destination, FootnoteElem, LinkElem, LinkMarker, RefElem,
};
use typst_syntax::Span;

use crate::dom::{
    BookmarkTable, Block, Footnote, ListSpec, MediaPart, NumberingTable,
    ParaProps, Run, RunProps, TocFigure, TocHeading, Underline, VertAlign,
};
use crate::package::{RelMode, Rels};
use crate::mappers;
use crate::props;

use typst_library::introspection::Location;

/// The relationship-type URI for an image part.
pub const REL_IMAGE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// The relationship-type URI for an external hyperlink.
pub const REL_HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
/// The relationship-type URI for a header part.
pub const REL_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
/// The relationship-type URI for a footer part.
pub const REL_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

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

    pub(crate) media: Vec<MediaPart>,
    media_dedup: FxHashMap<u128, EcoString>,

    pub(crate) doc_rels: Rels,
    /// Relationships for footnote-body content (→ `footnotes.xml.rels`); used
    /// while [`Self::in_footnote`] is set.
    pub(crate) footnote_rels: Rels,
    /// When `Some`, relationships are routed here instead of `doc_rels` — used to
    /// collect a header/footer part's own relationships while its content is
    /// lowered (an `r:id` in `headerN.xml` must resolve against `headerN.xml.rels`).
    pub(crate) part_rels: Option<Rels>,
    next_bookmark_id: u32,
    next_docpr_id: u32,
    /// Monotonic id for unique header/footer part names across sections.
    next_hdrftr_id: u32,
    /// Monotonic `relativeHeight` z-order for floating drawings (`<wp:anchor>`).
    next_z: u32,
    pub(crate) max_heading_level: u8,
    pub(crate) uses_fields: bool,
    pub(crate) uses_math: bool,

    pub(crate) bookmarks: BookmarkTable,

    /// Introspection tags harvested from rasterized content (see
    /// [`Self::rasterize`]), so labels/refs inside an element that we rendered to
    /// an image remain present in the introspector.
    pub(crate) deferred_tags: Vec<Tag>,

    /// Headings recorded in document order as they are converted, used to
    /// populate any table of contents once each heading's real bookmark exists.
    pub(crate) toc_headings: Vec<TocHeading>,

    /// Captioned figures/tables recorded in document order, used to populate any
    /// list of figures/tables once each figure's real bookmark exists.
    pub(crate) toc_figures: Vec<TocFigure>,

    /// The finite width to give content that we rasterize (see
    /// [`Self::rasterize`]). Width-relative content (`layout(size => ..)`,
    /// `width: 100%`, gradients sized to the container) must lay out against a
    /// real page width: laying it out under an *infinite* width makes such a
    /// closure produce pathologically wide output (observed: a single
    /// `layout()` rendering a 2040pt-wide frame for ~100s). Set from the page
    /// geometry in [`crate::document::docx_document`].
    pub(crate) raster_width: Abs,

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
            media: Vec::new(),
            media_dedup: FxHashMap::default(),
            doc_rels: Rels::new(),
            footnote_rels: Rels::new(),
            part_rels: None,
            next_bookmark_id: 1,
            next_docpr_id: 1,
            next_hdrftr_id: 1,
            next_z: 1,
            max_heading_level: 0,
            uses_fields: false,
            uses_math: false,
            bookmarks: BookmarkTable::default(),
            deferred_tags: Vec::new(),
            toc_headings: Vec::new(),
            toc_figures: Vec::new(),
            // A sane finite default (~A4 text width); overridden from the real
            // page geometry by `docx_document` before any conversion happens.
            raster_width: Abs::pt(450.0),
            raster_height: Abs::pt(842.0),
            quoter: SmartQuoter::new(),
            last_char: None,
            in_footnote: false,
            suppress_text_box: false,
        }
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

    /// Lays `content` out to a single frame for export: under the `Paged` target
    /// (so layout rules — shapes, images, … — fire instead of being dropped),
    /// against the page's content width (height unbounded), through a sub-engine
    /// with a THROWAWAY sink.
    ///
    /// A *finite* width is essential: width-relative content (`layout(size =>
    /// ..)`, `width: 100%`) laid out under an infinite width produces
    /// pathologically wide output. `Axes::splat(false)` keeps the region
    /// non-expanding, so fixed-size content takes its natural size.
    ///
    /// The throwaway sink isolates this re-layout's *delayed errors*: it
    /// re-realizes the content under `Paged` with its own pass, and packages that
    /// compute layout-coupled values during it (algo's `#i` indent-state assert,
    /// a margin-note needing page properties, a cetz canvas whose size hasn't
    /// stabilized) raise errors that never clear here — this universe, unlike the
    /// main document, can't feed those values back to itself. They must not fail
    /// the whole export; the shared introspector (reads) is untouched, so
    /// labels/refs/bibliography convergence is unaffected. Returns `None` if the
    /// content cannot be laid out in this context (e.g. a pagebreak with no page
    /// flow). Does not harvest tags or render — callers decide what to do.
    pub(crate) fn layout_export_frame(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
        height: typst_library::layout::Abs,
    ) -> SourceResult<Option<typst_library::layout::Frame>> {
        use comemo::Track;
        use typst_library::foundations::{Target, TargetElem};
        use typst_library::layout::{Axes, Region, Size};

        let target = TargetElem::target.set(Target::Paged).wrap();
        let styles = styles.chain(&target);
        let region = Region::new(Size::new(self.raster_width, height), Axes::splat(false));
        let loc = self.locator.next(&span);
        let layout_frame = self.engine.library.routines.layout_frame;

        // Lay the content out in an isolated sub-engine. The layouter can *panic*
        // (not just error) on content it cannot handle frame-wise — e.g. a
        // presentation-package slide whose absolute placement resolves against the
        // infinite region height and trips `assert!(size.is_finite())` deep in flow
        // distribution. That is a raw Rust panic that would otherwise abort the
        // entire export with no usable message. Since rasterization is a
        // best-effort fallback, catch it and degrade: drop just this one piece of
        // content (and warn), so the rest of the document still exports. The panic
        // hook is silenced for the duration so no scary backtrace reaches the user.
        let (frame, panicked) = {
            let mut throwaway = typst_library::engine::Sink::new();
            let mut sub = typst_library::engine::Engine {
                world: self.engine.world,
                library: self.engine.library,
                introspector: typst_utils::Protected::from_raw(
                    self.engine.introspector.into_raw(),
                ),
                traced: self.engine.traced,
                sink: throwaway.track_mut(),
                route: typst_library::engine::Route::extend(self.engine.route.track()),
            };
            let prev_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                layout_frame(&mut sub, content, loc, styles, region)
            }));
            std::panic::set_hook(prev_hook);
            match caught {
                // A layout *error* (vs panic) means the content does not lay out on
                // this iteration — common and benign during introspection
                // convergence — so degrade quietly.
                Ok(result) => (result.ok(), false),
                Err(_) => (None, true),
            }
        };
        if panicked {
            self.warn_ignored("content that could not be laid out", span);
        }
        Ok(frame)
    }

    /// Lays out arbitrary content and rasterizes it to a PNG, embedding it as a
    /// media part. Returns the media relationship id and the content's size, or
    /// `None` if the content lays out to nothing. This is the universal fallback
    /// for content that has no idiomatic OOXML representation (drawn shapes, SVG
    /// images, externally-rendered figures, …).
    /// Rasterizes `content` to a PNG media part and returns its relationship
    /// id, size, and the plain text recovered from the laid-out frame (empty if
    /// none) — the caller can attach that text as hidden runs so the
    /// rasterized region stays searchable/selectable/accessible.
    pub fn rasterize(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<(EcoString, typst_library::layout::Size, String)>> {
        use typst_library::foundations::Smart;
        use typst_library::layout::{Abs, Sides};

        if std::env::var_os("DOCX_DEBUG_RASTER").is_some() {
            eprintln!("RASTERIZE: {}", content.elem().name());
        }

        // First lay out in an infinite-height region (so a tall figure is captured
        // whole, not page-clipped).
        let inf_frame = self.layout_export_frame(content, styles, span, Abs::inf())?;

        // Harvest introspection tags from the laid-out frame so that labels and
        // references on elements inside the rasterized content stay resolvable
        // (otherwise `@label` to something inside a drawn box fails to converge).
        //
        // This must happen *before* the size check below: content can lay out to
        // a degenerate (zero) size precisely *because* an introspecting element
        // inside it (a bibliography, a cite, a counter display) has not yet
        // stabilized — on the first iteration it renders empty, collapsing the
        // box. If we dropped such a frame without harvesting, its tags would
        // never reach the introspector, the element would never stabilize, and
        // the box would stay zero forever: a convergence deadlock. We harvest from
        // the infinite frame (it always holds the laid-out content, even when its
        // *size* is infinite); the page-height retry below is render-only, so tags
        // are collected exactly once.
        if let Some(f) = &inf_frame {
            collect_frame_tags(f, &mut self.deferred_tags);
        }

        // Use the infinite frame when it has a usable size. Otherwise — page-
        // relative content such as a `place(bottom, ..)` cover, a slide, or a
        // full-page background collapses or runs to infinity under an unbounded
        // height — retry bounded by the real page height, which lets such content
        // resolve and rasterize instead of being dropped.
        let frame = match inf_frame {
            Some(frame) if usable_size(frame.size()) => frame,
            _ => match self.layout_export_frame(content, styles, span, self.raster_height)? {
                Some(frame) if usable_size(frame.size()) => frame,
                _ => return Ok(None),
            },
        };
        let size = frame.size();
        // Recover the rasterized region's text (reading-order reconstructed from
        // the laid-out frame) so the caller can keep it searchable/accessible as
        // hidden runs alongside the image.
        let frame_text = frame_to_text(&frame);

        // Render to a pixmap at 2× for crispness, then PNG-encode.
        let page = typst_layout::Page {
            frame,
            bleed: Sides::splat(Abs::zero()),
            fill: Smart::Custom(None),
            numbering: None,
            supplement: Content::empty(),
            number: 1,
        };
        let options = typst_render::RenderOptions {
            pixel_per_pt: 2.0.into(),
            ..Default::default()
        };
        // The rasterizer can panic on pathological sub-frames (e.g. a gradient or
        // tiling that resolves to a zero-dimension pixmap: `tiny-skia` asserts
        // "Canvas length must be != 0"). Such a panic must not abort the whole
        // export — this is a best-effort fallback. Catch it and drop just this
        // one image; the introspection tags were already harvested above, so
        // convergence is unaffected.
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            typst_render::render(&page, &options).encode_png()
        }));
        let Ok(Ok(png)) = rendered else { return Ok(None) };

        Ok(Some((self.add_image(&png, "png"), size, frame_text)))
    }

    /// Lays content out and returns its outer size, without rasterizing or
    /// harvesting tags. Used to size a text box whose text is extracted (and so
    /// re-introspected) separately, so the frame's own tags would double-count.
    /// Returns `None` if the content lays out to nothing usable.
    pub fn measure(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<typst_library::layout::Size>> {
        use typst_library::layout::Abs;
        let Some(frame) = self.layout_export_frame(content, styles, span, Abs::inf())? else {
            return Ok(None);
        };
        let size = frame.size();
        Ok(usable_size(size).then_some(size))
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
                dirty: false,
            }));
            self.uses_fields = true;
        }
    }

    /// Emits a non-fatal "X was ignored during DOCX export" warning.
    pub fn warn_ignored(&mut self, what: &str, span: Span) {
        self.engine
            .sink
            .warn(warning!(span, "{what} was ignored during DOCX export"));
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
        let hash = typst_utils::hash128(bytes);
        // Resolve (or create) the shared media part → its `word/`-relative target.
        let target = if let Some(t) = self.media_dedup.get(&hash) {
            t.clone()
        } else {
            let n = self.media.len() + 1;
            let ext = ext.to_ascii_lowercase();
            let part_name: EcoString = eco_format!("word/media/image{n}.{ext}");
            let target: EcoString = eco_format!("media/image{n}.{ext}");
            self.media.push(MediaPart {
                // The rId is per-referencing-part (allocated below), not a
                // property of the media part itself.
                rel: EcoString::new(),
                part_name,
                ext: ext.into(),
                bytes: bytes.to_vec(),
            });
            self.media_dedup.insert(hash, target.clone());
            target
        };
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
            && let Some(&num_id) = self.list_shapes.get(&spec) {
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
        let name: EcoString = eco_format!("_Ref{id}");
        self.bookmarks.by_location.insert(loc, (name.clone(), id));
        (id, name)
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

    /// Marks that a complex field was emitted.
    pub fn mark_field(&mut self) {
        self.uses_fields = true;
    }

    /// Marks that math was emitted.
    pub fn mark_math(&mut self) {
        self.uses_math = true;
    }

    /// Notes the deepest heading level seen.
    pub fn note_heading_level(&mut self, level: u8) {
        self.max_heading_level = self.max_heading_level.max(level);
    }

    // -- Property resolvers -------------------------------------------------

    /// Resolves a `TextElem`'s effective run properties.
    pub fn resolve_text_props(&self, styles: StyleChain, inherited: RunProps) -> RunProps {
        let mut p = inherited;

        // Size. The document's most common size is later hoisted into
        // `docDefaults` and stripped from the runs that match it (see
        // `hoist_text_defaults`); here we record the absolute value.
        let size = props::pt_to_half_pt(styles.resolve(TextElem::size).to_pt());
        p.size_half_pt = Some(size);

        // Colour. Black is Word's own default (in `docDefaults`), so omit it and
        // inherit — this both keeps the body compact and lets a run carrying the
        // `Hyperlink` character style keep that style's blue (emitting black would
        // override it back to invisible body text). An explicitly non-black fill
        // (e.g. `#text(red)` or `#show link: set text(red)`) still wins.
        if let typst_library::visualize::Paint::Solid(color) =
            styles.get_ref(TextElem::fill)
        {
            let hex = props::color_to_hex(color);
            if hex != [0, 0, 0] {
                p.color = Some(hex);
            }
        }

        // Font (first family). The most common one is later hoisted into
        // `docDefaults` and stripped from matching runs.
        if let Some(first) = styles.get_ref(TextElem::font).into_iter().next() {
            p.font = Some(first.as_str().into());
        }

        // Weight → bold (base weight plus the `strong` delta).
        let weight =
            styles.get(TextElem::weight).to_number() as i64 + styles.get(TextElem::delta).0;
        if weight >= 600 {
            p.bold = true;
        }

        // Italic, from the font style or an `emph` toggle.
        if styles.get(TextElem::style) != typst_library::text::FontStyle::Normal {
            p.italic = true;
        }
        if styles.get(TextElem::emph).0 {
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
                typst_library::text::DecoLine::Highlight { .. } => {
                    p.shd_fill = Some([0xFF, 0xFF, 0x00]);
                }
                _ => {}
            }
        }

        // Small capitals.
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

    /// Applies `TextElem::case` to a string.
    pub fn apply_case(&self, styles: StyleChain, text: &EcoString) -> EcoString {
        if let Some(case) = styles.get(TextElem::case) {
            case.apply(text.as_str()).into()
        } else {
            text.clone()
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
        use typst_library::model::ParElem;
        use typst_library::text::TextElem;

        let mut p = ParaProps::default();

        // G3 alignment. Justification (`w:jc="both"`) wins over horizontal
        // alignment; a left/start paragraph stays `None` for byte-identity with
        // the previously emitted output.
        if styles.get(ParElem::justify) {
            p.jc = Some(crate::dom::Jc::Both);
        } else {
            match styles.resolve(AlignElem::alignment).x {
                FixedAlignment::Center => p.jc = Some(crate::dom::Jc::Center),
                FixedAlignment::End => p.jc = Some(crate::dom::Jc::End),
                FixedAlignment::Start => {}
            }
        }

        // G6 paragraph base reading order (the run-level `w:rtl` is Slice C).
        if !styles.resolve(TextElem::dir).is_positive() {
            p.bidi = true;
        }

        let font_size = styles.resolve(TextElem::size);

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
        // See the identical check in `inline_runs`.
        if crate::convert::contains_place(body)
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
        for (child, child_styles) in pairs {
            // A labeled inline element (`… text <spot>`) is a valid `#link(<spot>)`
            // target; bracket the runs it produces with a bookmark so the link
            // resolves. (Tags carry no visible runs, so skip them.)
            let label_loc = (!child.is::<TagElem>())
                .then(|| child.location().filter(|_| child.label().is_some()))
                .flatten();
            let child_out_start = out.len();

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
                // Realized ref/footnote link wrapper: emit an internal hyperlink
                // to the target's bookmark wrapping the body runs.
                let (_id, name) = self.add_bookmark(elem.loc);
                let runs = self.inline_runs(&elem.body, child_styles, props.clone())?;
                out.push(ParaChild::Hyperlink { rel: None, anchor: Some(name), runs });
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
                        out.push(ParaChild::Hyperlink {
                            rel: None,
                            anchor: Some(name),
                            runs,
                        });
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
            } else {
                let mut runs = Vec::new();
                self.handle_inline(child, child_styles, &props, &mut runs)?;
                out.extend(runs.into_iter().map(ParaChild::Run));
            }

            // Bracket a labeled inline child's output with a bookmark so a
            // `#link(<label>)` to it resolves.
            if let Some(loc) = label_loc
                && out.len() > child_out_start
            {
                let (id, name) = self.add_bookmark(loc);
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
            let rp = self.resolve_text_props(styles, props.clone());
            self.push_text(out, rp, text);
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
            p.bold = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<EmphElem>() {
            let mut p = props.clone();
            p.italic = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<SubElem>() {
            let mut p = props.clone();
            p.vert_align = Some(VertAlign::Sub);
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<SuperElem>() {
            let mut p = props.clone();
            p.vert_align = Some(VertAlign::Super);
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<UnderlineElem>() {
            let mut p = props.clone();
            use typst_library::foundations::{Resolve, Smart};
            p.underline = Some(match elem.stroke.get_cloned(styles) {
                Smart::Custom(stroke) => underline_from_stroke(&stroke.resolve(styles)),
                Smart::Auto => Underline::single(),
            });
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<StrikeElem>() {
            let mut p = props.clone();
            p.strike = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<HighlightElem>() {
            let mut p = props.clone();
            // Default highlight color (yellow) unless a fill is provided.
            p.shd_fill = Some([0xFF, 0xFF, 0x00]);
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<SmallcapsElem>() {
            let mut p = props.clone();
            p.smallcaps = true;
            out.extend(self.inline_runs(&elem.body, styles, p)?);
        } else if let Some(elem) = child.to_packed::<EquationElem>()
            && !elem.block.get(styles)
        {
            match mappers::math::equation(elem, styles, self)? {
                mappers::math::EquationOut::Inline(run) => out.push(run),
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
            // A `DirectLinkElem` is the realized link-wrapper around ref/footnote
            // content. In a run-only context we keep just the body; the enclosing
            // REF/PAGEREF field (or footnote mark) already provides the jump.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<LinkMarker>() {
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some((body, fill, bdr)) = mappers::shape::inline_frame(child, styles)
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
                // A *plain* box — no fill, stroke or clip, so nothing visual to
                // preserve — is just inline content held together (`#box[..]` to
                // prevent a line break, `#box(width: ..)[label]`, a baseline
                // shift). Extract its runs so the text stays selectable instead
                // of rasterizing it to an image; the only thing lost is the box's
                // geometric constraint, which has no inline-flow equivalent.
                // `body_extractable` still guards the one layout-bound case (a
                // per-line equation label inside).
                Some(body)
                    if box_is_plain(elem, styles)
                        && crate::convert::body_extractable(&body)
                        && crate::convert::body_inline_extractable(&body) =>
                {
                    out.extend(self.inline_runs(&body, styles, props.clone())?);
                }
                Some(body) => {
                    // Rasterize the box (this keeps a styled box's visual and, for
                    // a box that lays out real content, its labels via the frame
                    // tag harvest). If it lays out to *nothing* — a degenerate box
                    // whose rasterization would otherwise be dropped — extract its
                    // text so the content survives. Such a box has no laid-out
                    // content and hence no labels to orphan, so plain extraction is
                    // safe; we discard any partial tag harvest first to be sure.
                    let mark = self.deferred_tags.len();
                    let runs = mappers::image::laid_out_fallback(child, styles, self)?;
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
        } else if let Some(elem) =
            child.to_packed::<typst_library::model::TableCell>()
        {
            // Same as `GridCell` above, for `#table.cell(..)`.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<typst_library::layout::HideElem>() {
            // `#hide[..]` → hidden text (`<w:vanish/>`): invisible but present
            // (searchable, screen-reader-readable), instead of dropped. Extract
            // when the body has no layout-bound introspection; otherwise drop it
            // (it is invisible anyway, so rasterizing would be pointless).
            if crate::convert::body_extractable(&elem.body) {
                let hidden = RunProps { vanish: true, ..props.clone() };
                out.extend(self.inline_runs(&elem.body, styles, hidden)?);
            }
        } else if let Some(elem) = child.to_packed::<LinkElem>() {
            // In a run-only context (nested formatting, table/footnote bodies) we
            // cannot emit a `<w:hyperlink>` wrapper, so lower the link body to
            // runs. Paragraph-level links go through `link_children` instead.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(run) = mappers::shape::shape(child, styles, self)? {
            // A decorative vector shape (`#rect`/`#circle`/`#polygon`/…) maps to a
            // DrawingML `wps:wsp` shape instead of a rasterized image.
            out.push(run);
        } else if let Some(elem) = child.to_packed::<typst_library::visualize::CurveElem>()
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
            } else if let Some(runs) = mappers::shape::move_text(elem, styles, props, self)? {
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

    /// No idiomatic representation (a drawn shape, an SVG/PDF image, an
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
        mappers::reference::link(elem, styles, props.clone(), self)
    }

    /// Pushes a text run, coalescing with a preceding identical-props run.
    fn push_text(&mut self, out: &mut Vec<Run>, props: RunProps, text: EcoString) {
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

/// Whether a laid-out size is finite and strictly positive on both axes (Word
/// rejects zero/degenerate drawing extents).
fn usable_size(size: typst_library::layout::Size) -> bool {
    use typst_library::layout::Abs;
    size.x.to_pt().is_finite()
        && size.y.to_pt().is_finite()
        && size.x > Abs::zero()
        && size.y > Abs::zero()
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

/// Classifies a resolved dash array's "on" segments (the even indices; the odd
/// ones are gaps) into the nearest Word underline style. The named Typst dash
/// presets use a line-width "dot" for dotted lines and explicit lengths for
/// dashes, so a dot-only pattern is `dotted`, a length-only one is `dash`, and a
/// mix (dash-dotted) is `dotDash`.
fn classify_dash(
    array: &[typst_library::visualize::DashLength<Abs>],
) -> &'static str {
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

/// Recursively collects introspection tags from a laid-out frame.
fn collect_frame_tags(frame: &Frame, out: &mut Vec<Tag>) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect_frame_tags(&group.frame, out),
            FrameItem::Tag(tag) => out.push(tag.clone()),
            _ => {}
        }
    }
}

/// Collects each laid-out text run as `(top-left position, text, font size,
/// run width)`, recursing through groups by their translation (a
/// rotate/scale/skew is ignored — good enough for reading-order recovery).
fn collect_frame_text(
    frame: &Frame,
    offset: typst_library::layout::Point,
    out: &mut Vec<(typst_library::layout::Point, EcoString, Abs, Abs)>,
) {
    use typst_library::layout::Point;
    for (pos, item) in frame.items() {
        let p = offset + *pos;
        match item {
            FrameItem::Group(group) => {
                let t = &group.transform;
                collect_frame_text(&group.frame, p + Point::new(t.tx, t.ty), out);
            }
            FrameItem::Text(text) if !text.text.is_empty() => {
                out.push((p, text.text.clone(), text.size, text.width()));
            }
            _ => {}
        }
    }
}

/// Reconstructs approximate reading-order text from a laid-out frame's text
/// runs. Every word is present (each `TextItem` carries its plain `text`);
/// lines are recovered by clustering on the y-position and inter-word spaces
/// from x-gaps. Precise structure/formatting is lost, but the text is fully
/// recovered — this is what lets content a *layout closure* produced (which
/// yields an opaque `Frame`, not re-realizable blocks) still contribute
/// searchable/selectable text rather than being a pure image. Returned as a
/// single string with `\n` line separators; the caller decides visibility.
fn frame_to_text(frame: &Frame) -> String {
    use typst_library::layout::Point;
    let mut items: Vec<(Point, EcoString, Abs, Abs)> = Vec::new();
    collect_frame_text(frame, Point::zero(), &mut items);
    if items.is_empty() {
        return String::new();
    }
    // Reading order: top-to-bottom, then left-to-right.
    items.sort_by(|a, b| {
        a.0.y
            .partial_cmp(&b.0.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.x.partial_cmp(&b.0.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut s = String::new();
    let mut last_y: Option<Abs> = None;
    let mut last_x_end: Option<Abs> = None;
    for (pos, text, size, width) in items {
        if let Some(ly) = last_y
            && (pos.y - ly).abs() > size * 0.6
        {
            s.push('\n');
            last_x_end = None;
        } else if let Some(xe) = last_x_end
            && pos.x - xe > size * 0.25
            && !s.ends_with(char::is_whitespace)
        {
            s.push(' ');
        }
        s.push_str(&text);
        last_y = Some(pos.y);
        last_x_end = Some(pos.x + width);
    }
    s
}

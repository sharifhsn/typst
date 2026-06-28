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
use typst_library::model::{DirectLinkElem, FootnoteElem, LinkElem, LinkMarker, RefElem};
use typst_syntax::Span;

use crate::dom::{
    BookmarkTable, Block, Footnote, ListSpec, MediaPart, NumberingTable,
    ParaProps, Run, RunProps, TocFigure, TocHeading, VertAlign,
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

    /// Smart-quote state, threaded through inline runs.
    quoter: SmartQuoter,
    /// The last character emitted into a text run, for smart quoting.
    last_char: Option<char>,
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
            quoter: SmartQuoter::new(),
            last_char: None,
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

    /// Lays out arbitrary content and rasterizes it to a PNG, embedding it as a
    /// media part. Returns the media relationship id and the content's size, or
    /// `None` if the content lays out to nothing. This is the universal fallback
    /// for content that has no idiomatic OOXML representation (drawn shapes, SVG
    /// images, externally-rendered figures, …).
    pub fn rasterize(
        &mut self,
        content: &Content,
        styles: StyleChain,
        span: Span,
    ) -> SourceResult<Option<(EcoString, typst_library::layout::Size)>> {
        use typst_library::foundations::{Smart, Target, TargetElem};
        use typst_library::layout::{Abs, Axes, Region, Sides, Size};

        // Lay out under the paged target: layout rules (shapes, images, …) are
        // only registered for `Target::Paged`, so the content would otherwise be
        // dropped during its own layout.
        let target = TargetElem::target.set(Target::Paged).wrap();
        let styles = styles.chain(&target);

        // Lay the content out against the page's content width (height stays
        // unbounded). A *finite* width is essential: width-relative content
        // (`layout(size => ..)`, `width: 100%`) laid out under an infinite width
        // produces pathologically wide output. `Axes::splat(false)` keeps the
        // region non-expanding, so fixed-size content still takes its natural
        // size (and may exceed the width without being clipped). The fallback
        // must degrade gracefully: if the content cannot be laid out in this
        // context (e.g. a pagebreak with no page flow), drop it rather than
        // failing the export.
        let region =
            Region::new(Size::new(self.raster_width, Abs::inf()), Axes::splat(false));
        let loc = self.locator.next(&span);

        // Lay the content out through a sub-engine with a THROWAWAY sink, so any
        // delayed errors the re-layout produces are discarded rather than
        // promoted to fatal at the end of the introspection loop.
        //
        // The rasterization re-layout is a separate introspection universe: it
        // re-realizes the content under `Paged` with its own pass, and packages
        // that compute layout-coupled values during that pass (algo's `#i`
        // indent-state assert, a margin-note that needs page properties, a cetz
        // canvas whose size hasn't stabilized) raise errors there that never
        // clear — because this universe, unlike the main document, has no way to
        // feed those values back to itself. Those errors must not fail the whole
        // export; the content still rasterizes to its best-effort visual. We keep
        // the shared introspector (reads) and the tag harvest below intact, so
        // labels/refs/bibliography convergence are unaffected — only this
        // re-layout's own error reporting is isolated.
        use comemo::Track;
        let layout_frame = self.engine.library.routines.layout_frame;
        let mut throwaway = typst_library::engine::Sink::new();
        let frame = {
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
            match layout_frame(&mut sub, content, loc, styles, region) {
                Ok(frame) => frame,
                Err(_) => return Ok(None),
            }
        };

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
        // the box would stay zero forever: a convergence deadlock. Harvesting the
        // tags here lets the next iteration render the element with real content
        // (and real size).
        collect_frame_tags(&frame, &mut self.deferred_tags);

        let size = frame.size();
        if !size.x.to_pt().is_finite()
            || !size.y.to_pt().is_finite()
            || size.x <= Abs::zero()
            || size.y <= Abs::zero()
        {
            return Ok(None);
        }

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

        Ok(Some((self.add_image(&png, "png"), size)))
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

    /// Embeds image bytes as a media part (dedup by byte hash); returns the rId.
    pub fn add_image(&mut self, bytes: &[u8], ext: &str) -> EcoString {
        let hash = typst_utils::hash128(bytes);
        if let Some(rel) = self.media_dedup.get(&hash) {
            return rel.clone();
        }
        let n = self.media.len() + 1;
        let ext = ext.to_ascii_lowercase();
        let part_name: EcoString = eco_format!("word/media/image{n}.{ext}");
        // Targets in document.xml.rels are relative to word/.
        let target: EcoString = eco_format!("media/image{n}.{ext}");
        let rel = self.doc_rels.add(REL_IMAGE, &target, RelMode::Internal);
        self.media.push(MediaPart {
            rel: rel.clone(),
            part_name,
            ext: ext.into(),
            bytes: bytes.to_vec(),
        });
        self.media_dedup.insert(hash, rel.clone());
        rel
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

    /// Allocates (or reuses) an external hyperlink relationship; returns the rId.
    pub fn add_external_rel(&mut self, url: &str) -> EcoString {
        self.doc_rels.add(REL_HYPERLINK, url, RelMode::External)
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

        // Size.
        let size = styles.resolve(TextElem::size);
        p.size_half_pt = Some(props::pt_to_half_pt(size.to_pt()));

        // Color.
        if let typst_library::visualize::Paint::Solid(color) =
            styles.get_ref(TextElem::fill)
        {
            p.color = Some(props::color_to_hex(color));
        }

        // Font (first family).
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
                typst_library::text::DecoLine::Underline { .. } => p.underline = true,
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

        // Language tag.
        let lang = styles.get(TextElem::lang);
        p.lang = Some(lang.as_str().into());

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
            } else {
                let mut runs = Vec::new();
                self.handle_inline(child, child_styles, &props, &mut runs)?;
                out.extend(runs.into_iter().map(ParaChild::Run));
            }
        }
        Ok(out)
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
        } else if let Some(elem) = child.to_packed::<HElem>()
            && elem.amount.is_zero()
        {
            // Zero-width spacing: skip.
        } else if child.is::<LinebreakElem>() {
            out.push(Run::Break);
            self.last_char = None;
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
            p.underline = true;
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
            out.push(mappers::footnote::footnote(elem, styles, self)?);
        } else if let Some(elem) = child.to_packed::<DirectLinkElem>() {
            // A `DirectLinkElem` is the realized link-wrapper around ref/footnote
            // content. In a run-only context we keep just the body; the enclosing
            // REF/PAGEREF field (or footnote mark) already provides the jump.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<LinkMarker>() {
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else if let Some(elem) = child.to_packed::<LinkElem>() {
            // In a run-only context (nested formatting, table/footnote bodies) we
            // cannot emit a `<w:hyperlink>` wrapper, so lower the link body to
            // runs. Paragraph-level links go through `link_children` instead.
            out.extend(self.inline_runs(&elem.body, styles, props.clone())?);
        } else {
            // No idiomatic representation (a drawn shape, an SVG/PDF image, an
            // externally-rendered figure, …): rasterize it and embed as an image
            // so the content survives instead of being dropped.
            let before = self.deferred_tags.len();
            let run = mappers::image::laid_out_fallback(child, styles, self)?;
            // A figure whose *container* we rasterized (e.g. a `wrap-content`
            // figure) never reaches the figure mapper, so it emits no visible
            // `SEQ` field — Word's caption counter would then under-count and
            // drift from the introspector-baked cross-reference numbers. Emit a
            // hidden `SEQ \h` (increment without display) for each such figure,
            // keeping Word's numbering consistent with the references.
            self.emit_rasterized_figure_seqs(before, styles, out);
            match run {
                Some(run) => out.push(run),
                None => self.warn_ignored(child.elem().name(), child.span()),
            }
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

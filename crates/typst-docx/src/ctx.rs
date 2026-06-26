//! The mutable conversion context [`DocxCtx`] and the inline/block flow.

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use typst_library::diag::{SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Packed, StyleChain};
use typst_library::introspection::{Locator, SplitLocator, TagElem};
use typst_library::layout::HElem;
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
    ParaProps, Run, RunProps, VertAlign,
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
    pub(crate) max_heading_level: u8,
    pub(crate) uses_fields: bool,
    pub(crate) uses_math: bool,

    pub(crate) bookmarks: BookmarkTable,

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
            max_heading_level: 0,
            uses_fields: false,
            uses_math: false,
            bookmarks: BookmarkTable::default(),
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

        // Weight → bold heuristic.
        let weight = styles.get(TextElem::weight).to_number();
        if weight >= 600 {
            p.bold = true;
        }

        // Style → italic.
        if styles.get(TextElem::style) != typst_library::text::FontStyle::Normal {
            p.italic = true;
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
        let mut p = ParaProps::default();
        // Justification.
        if styles.get(typst_library::model::ParElem::justify) {
            p.jc = Some(crate::dom::Jc::Both);
        }
        p
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
            if let Some(elem) = child.to_packed::<LinkElem>() {
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
            // Tags are introspection passthrough; record nothing in runs here —
            // bookmarks/tags are handled at the block level. Ignore for runs.
            let _ = elem;
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
            self.warn_ignored(child.elem().name(), child.span());
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

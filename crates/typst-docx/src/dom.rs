//! The typed DOCX intermediate representation.

use std::sync::Arc;

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, Output, StyleChain, Target};
use typst_library::introspection::{Introspector, Location, Tag};
use typst_library::model::{Document, DocumentInfo};
pub use typst_ooxml_core::dml::{
    FillSpec as ShapeFill, GradientStop, PathSegment, StrokeSpec as ShapeStroke,
};
pub use typst_ooxml_core::media::MediaPart;
use typst_syntax::Span;

use crate::introspect::DocxIntrospector;
use crate::package::Rels;
use crate::report::FidelityReport;
use crate::snapshot::ExportSnapshot;

/// Process-local identity used to join an export review candidate to an
/// opt-in stable tag at serialization time.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ReviewJoinId(pub u64);

/// The kind of source-backed block offered for review tagging.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ReviewCandidateKind {
    Heading,
    Paragraph,
    ListItem,
    TableCell,
    InlineText,
}

/// Source provenance carried by a paragraph through nested DOCX structures.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) struct ReviewOrigin {
    pub join_id: ReviewJoinId,
    pub span: Span,
    pub kind: ReviewCandidateKind,
}

/// One conservative source-backed block that can be wrapped in a Word content
/// control without changing its visible content.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReviewCandidate {
    pub join_id: ReviewJoinId,
    pub span: Span,
    pub kind: ReviewCandidateKind,
    pub baseline: EcoString,
}

/// Output document: realized native tree lowered to the OOXML IR + metadata +
/// introspector.
pub struct DocxDocument {
    pub(crate) info: DocumentInfo,
    pub(crate) body: Vec<Block>,
    pub(crate) sect: SectPr,
    pub(crate) footnotes: Vec<Footnote>,
    pub(crate) numbering: NumberingTable,
    pub(crate) media: Vec<MediaPart>,
    /// License-permitted font programs needed by editable text in this export.
    /// They are obfuscated into `word/fonts/*.odttf` during packaging.
    pub(crate) embedded_fonts: Vec<EmbeddedFontProgram>,
    pub(crate) doc_rels: Rels,
    /// Relationships created while lowering footnote bodies — they belong in
    /// `word/_rels/footnotes.xml.rels`, not the document's, or Word rejects the
    /// file. Empty when no footnote contains an image/external link.
    pub(crate) footnote_rels: Rels,
    pub(crate) max_heading_level: u8,
    /// The document's root text properties, hoisted into `docDefaults`.
    pub(crate) text_defaults: TextDefaults,
    /// Document-derived heading style definitions (`Heading1..HeadingN`).
    pub(crate) heading_styles: Vec<HeadingStyle>,
    pub(crate) uses_math: bool,
    pub(crate) introspector: Arc<DocxIntrospector>,
    /// Header parts (`word/headerN.xml`) referenced by the section(s).
    pub(crate) header_parts: Vec<HdrFtrPart>,
    /// Footer parts (`word/footerN.xml`) referenced by the section(s).
    pub(crate) footer_parts: Vec<HdrFtrPart>,
    /// `set page(fill: solid-color)` — a flat page background colour, from the
    /// first section. Maps to the document-level `w:background` element
    /// (Word's "Page Color"), distinct from `set page(background:)` (a full-page
    /// image/art, which becomes a `behindDoc` drawing instead).
    pub(crate) background_color: Option<[u8; 3]>,
    /// Whether the document enables hyphenation (`#set text(hyphenate: ..)`,
    /// resolved at the root style chain). Emits `w:autoHyphenation`.
    pub(crate) hyphenate: bool,
    /// Whether any section emits distinct `even` header/footer references.
    /// Word ignores those references unless `w:evenAndOddHeaders` is enabled
    /// in `word/settings.xml`.
    pub(crate) even_and_odd_headers: bool,
    /// Whether any section uses inside/outside page margins. Emits the
    /// document-wide `<w:mirrorMargins/>` setting.
    pub(crate) mirror_margins: bool,
    /// Whether any mirrored-margin section uses a right-side binding gutter.
    /// Emits the document-wide `<w:rtlGutter/>` setting.
    pub(crate) rtl_gutter: bool,
    /// The document's bibliography, synthesized as a BibLaTeX (`.bib`) string
    /// (same call the Pandoc exporter uses for its sidecar). `None` when the
    /// document has no bibliography. Embedded as an inert sidecar part, not
    /// Word-native `CITATION`/`BIBLIOGRAPHY` fields — see `encode.rs`.
    pub(crate) bibliography: Option<String>,
    /// The same bibliography, mapped onto Word's native `b:Source` schema
    /// (see `crate::bibliography`), for the `customXml/item1.xml` part that
    /// backs References → Manage Sources. Empty when the document has no
    /// bibliography.
    pub(crate) word_sources: Vec<crate::bibliography::WordSource>,
    /// Structured representation decisions and deliberately suppressed
    /// diagnostics collected while lowering the document.
    pub(crate) fidelity_report: FidelityReport,
    /// Owned semantic/paged identity and geometry sidecar captured before
    /// target-specific lowering.
    pub(crate) export_snapshot: ExportSnapshot,
    pub(crate) review_candidates: Vec<ReviewCandidate>,
}

/// One of WordprocessingML's four embedded family style slots.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) enum EmbeddedFontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl EmbeddedFontStyle {
    pub(crate) fn element(self) -> &'static str {
        match self {
            Self::Regular => "w:embedRegular",
            Self::Bold => "w:embedBold",
            Self::Italic => "w:embedItalic",
            Self::BoldItalic => "w:embedBoldItalic",
        }
    }
}

/// Raw OpenType/TrueType program selected for one embedded family style.
pub(crate) struct EmbeddedFontProgram {
    pub(crate) family: EcoString,
    pub(crate) style: EmbeddedFontStyle,
    pub(crate) data: Vec<u8>,
}

impl DocxDocument {
    pub fn info(&self) -> &DocumentInfo {
        &self.info
    }

    /// Provides the DOCX introspector, including DOCX bookmark anchors and the
    /// synthetic fallback layer.
    pub fn introspector(&self) -> &Arc<DocxIntrospector> {
        &self.introspector
    }

    /// The primary section's page size in points (`width`, `height`).
    ///
    /// Exposed so the CLI can spot a slide-shaped document being written to
    /// `.docx` and suggest `.pptx` instead. Stored internally in twips
    /// (1 pt = 20 twips).
    pub fn page_size_pt(&self) -> (f64, f64) {
        (self.sect.page_w as f64 / 20.0, self.sect.page_h as f64 / 20.0)
    }

    /// Structured fidelity decisions made while lowering this document.
    pub fn fidelity_report(&self) -> &FidelityReport {
        &self.fidelity_report
    }

    /// Stable semantic nodes and their converged paged geometry.
    pub fn export_snapshot(&self) -> &ExportSnapshot {
        &self.export_snapshot
    }

    /// Conservative, uniquely source-backed blocks eligible for opt-in Word
    /// review content controls.
    pub fn review_candidates(&self) -> &[ReviewCandidate] {
        &self.review_candidates
    }

    /// Versioned machine-readable snapshot and fidelity report embedded in the
    /// package as `customXml/typstFidelity.xml`.
    pub fn fidelity_manifest_xml(&self) -> String {
        crate::manifest::build(self)
    }
}

impl Document for DocxDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }
}

impl Output for DocxDocument {
    fn introspector(&self) -> &dyn Introspector {
        self.introspector.as_ref()
    }

    fn target() -> Target {
        Target::Docx
    }

    fn create(
        engine: &mut Engine,
        content: &Content,
        styles: StyleChain,
    ) -> SourceResult<Self> {
        crate::docx_document(engine, content, styles)
    }
}

/// A block-level body item.
pub enum Block {
    Para(Para),
    Table(Tbl),
    /// Exact vertical flow space that cannot be attached to a neighboring
    /// paragraph (most commonly between two tables). Encodes as an empty
    /// paragraph with an exact line height.
    FlowSpace {
        dxa: i32,
    },
    /// A table of contents: a `TOC` complex field whose cached result is a set
    /// of baked entry paragraphs (so it shows without a manual field update).
    Toc(Toc),
    /// A non-final section break carrying its own `SectPr`.
    SectionBreak(SectPr),
    /// A weak page break (`pagebreak(weak: true)` or a `set page` boundary
    /// marker): breaks only when content precedes it on the page, and any run
    /// of them collapses to one. Lowered by
    /// `move_page_breaks_before_following_blocks` into an idempotent
    /// `w:pageBreakBefore` on the following paragraph — never an explicit
    /// `<w:br>`, which would manufacture blank pages exactly where Typst's
    /// weak semantics guarantee none.
    WeakPageBreak,
    /// Introspection tag passthrough for the introspector + bookmarks.
    Tag(Tag),
}

/// A table-of-contents complex field. The field's `begin`/`instrText`/`separate`
/// wrap the first `entry` and its `end` closes the last, so the baked entries
/// render as the field's cached result. When `entries` is empty the `fallback`
/// runs are shown inside a single-paragraph field instead.
pub struct Toc {
    pub instr: EcoString,
    pub mode: FieldMode,
    /// Heading depth (`\o "1-N"`) to populate from after the body is converted.
    /// `Some` marks a heading table of contents.
    pub depth: Option<usize>,
    /// Caption category (`\c "Figure"` / `"Table"` / …) to populate from. `Some`
    /// marks a list of figures/tables.
    pub caption_category: Option<EcoString>,
    /// The exact semantic entries selected by this Typst outline. These are
    /// merged with native heading records after conversion so custom show
    /// rules cannot remove entries or broaden a filtered outline.
    pub semantic_headings: Vec<TocHeading>,
    /// Right tab position (twips) for the dot leader + page number.
    pub tab_pos: i32,
    /// Resolved custom Typst outline indentation for levels 1 through 9, in
    /// twips. `None` retains Word's style fallback for context-dependent auto
    /// indentation.
    pub entry_indents: Vec<Option<i32>>,
    /// Baked entries, filled in a post-conversion pass from the headings/figures
    /// that were actually emitted (so the bookmarks they target always exist).
    pub entries: Vec<Para>,
    pub fallback: Vec<Run>,
}

/// A heading recorded during conversion, used to populate the table of contents
/// once every heading's real bookmark is known.
#[derive(Clone)]
pub struct TocHeading {
    pub level: usize,
    pub location: Option<Location>,
    /// Stable source identity across semantic introspection and native
    /// conversion, whose locators may assign different runtime locations.
    pub source_span: Span,
    /// Physical page captured from the semantic outline target before DOCX
    /// lowering assigns its own locations.
    pub page_text: Option<EcoString>,
    /// The heading's bookmark name, when it emitted one (else a plain entry).
    pub anchor: Option<EcoString>,
    pub text: EcoString,
}

/// A captioned figure/table recorded during conversion, used to populate a list
/// of figures/tables once every figure's real bookmark is known.
pub struct TocFigure {
    /// The caption category (`Figure`/`Table`/…), matched against a `\c` switch.
    pub category: EcoString,
    pub location: Option<Location>,
    /// The figure's bookmark name, when it emitted one (else a plain entry).
    pub anchor: Option<EcoString>,
    pub text: EcoString,
}

/// A paragraph.
pub struct Para {
    pub props: ParaProps,
    pub content: Vec<ParaChild>,
}

/// Paragraph-level content.
// The `Run` variant carries a `Drawing` and is large, but it is the common case
// and this IR is transient (built, then immediately serialized), so boxing every
// run to shrink the enum isn't worth the per-run allocation.
#[allow(clippy::large_enum_variant)]
pub enum ParaChild {
    Run(Run),
    /// A display equation `<m:oMathPara>` (serialized XML).
    OmmlPara(String),
    /// `<w:hyperlink r:id|w:anchor>` wrapping runs.
    Hyperlink {
        rel: Option<EcoString>,
        anchor: Option<EcoString>,
        runs: Vec<Run>,
    },
    BookmarkStart {
        id: u32,
        name: EcoString,
    },
    BookmarkEnd {
        id: u32,
    },
    Tag(Tag),
}

/// A run-level item.
// The `Drawing` variant is large but by far the common case (every image/shape);
// this IR is transient (built, then immediately serialized), so boxing isn't
// worth the per-drawing allocation.
#[allow(clippy::large_enum_variant)]
pub enum Run {
    Text {
        props: RunProps,
        text: EcoString,
    },
    Break {
        kind: BreakKind,
    },
    PageBreak,
    /// A `#colbreak()` → `<w:br w:type="column"/>`: moves the following content to
    /// the next column in a multi-column section.
    ColumnBreak,
    Tab,
    /// A tab from a fractional `#h(1fr)` (the push-apart idiom). Encoded as a
    /// tab, but its paragraph gains a right-aligned tab stop at the content width
    /// so it pushes the following content to the right margin.
    FillTab,
    FootnoteRef {
        props: RunProps,
        id: i32,
        /// Bookmark around the first native reference mark. Later Typst
        /// re-references target this with a NOTEREF field instead of creating a
        /// second native Word footnote occurrence (which Word would renumber).
        bookmark: Option<(u32, EcoString)>,
    },
    /// The in-body footnote number mark (`<w:footnoteRef/>`, styled
    /// `FootnoteReference`). Prepended to a footnote body's first paragraph so
    /// Word/LibreOffice render the footnote's auto-number next to its text.
    FootnoteRefMark,
    Drawing(Drawing),
    /// An inline equation `<m:oMath>` (serialized XML).
    OmmlInline(String),
    Field(Field),
}

/// Why a run-level line break exists.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BreakKind {
    /// An authored `#linebreak()` whose trailing position is meaningful.
    Authored,
    /// A fallback for paragraph/vertical separation in a run-only context.
    /// A measured container may already account for this space physically.
    Structural,
}

/// The document's root text properties, resolved once from the root style chain
/// and hoisted into `docDefaults` (`word/styles.xml`). Per-run `<w:rPr>` then
/// carries only the properties that *deviate* from these, so the body inherits
/// the document font/size/colour: changing the `Normal` style or theme font in
/// Word restyles the whole document, and `document.xml` stays compact.
#[derive(Clone)]
pub struct TextDefaults {
    /// Default font family (first of `text(font:)`).
    pub font: Option<EcoString>,
    /// Default size in half-points.
    pub size_half_pt: u32,
    /// Default text colour; `None` = not representable as a single solid colour.
    pub color: Option<[u8; 3]>,
    /// BCP-47 language tag for spell-check (e.g. `en-US`).
    pub lang: Option<EcoString>,
}

impl Default for TextDefaults {
    fn default() -> Self {
        // 11pt, Word's own default, until the real root styles are resolved.
        Self {
            font: None,
            size_half_pt: 22,
            color: None,
            lang: None,
        }
    }
}

/// A document-derived heading style definition.
#[derive(Clone)]
pub struct HeadingStyle {
    pub level: u8,
    /// Run properties owned by the style. Kept to the subset that can be safely
    /// inherited by text runs: font, size, bold, italic and colour.
    pub rpr: RunProps,
    /// Paragraph spacing owned by the style when all headings at this level agree.
    pub spacing: Option<Spacing>,
    /// The `w:numPr` numbering instance this style joins, when the document's
    /// heading numbers are live (see `crate::heading_numbering`). The level's
    /// `w:ilvl` is `level - 1`.
    pub num_id: Option<u32>,
}

/// One resolved heading style sample recorded while lowering a heading.
pub(crate) struct HeadingStyleSample {
    pub level: u8,
    pub rpr: RunProps,
}

/// Flattened character formatting → `<w:rPr>`.
#[derive(Default, Clone, PartialEq)]
pub struct RunProps {
    /// Non-serialized provenance for opt-in inline Word review controls.
    pub(crate) review_origin: Option<ReviewOrigin>,
    pub style: Option<EcoString>,
    pub font: Option<EcoString>,
    /// True when bold came from Typst's semantic `#strong` wrapper. This lets
    /// the encoder use Word's Strong character style instead of guessing from a
    /// resolved bold value that may have come from `#text(weight:)` or a style.
    pub strong: bool,
    pub bold: bool,
    /// True when italic came from Typst's semantic `#emph` wrapper.
    pub emphasis: bool,
    pub italic: bool,
    pub caps: bool,
    pub smallcaps: bool,
    pub strike: bool,
    /// `<w:noProof/>` — disables spelling/grammar proofing for code/raw runs.
    pub no_proof: bool,
    pub color: Option<[u8; 3]>,
    /// Office 2010 gradient text fill extension data (`w14:textFill`), set
    /// alongside `color` when the resolved fill is a gradient DrawingML can
    /// express as linear or radial. Reuses the shape exporter's DrawingML
    /// gradient maths (`typst_ooxml_core::dml::gradient_fill`); `None` for a
    /// solid/absent/tiling fill, or a gradient with no DrawingML analogue
    /// (a conic sweep, an off-center radial outer circle, no stops). `color`
    /// always still holds the flat first-stop fallback in that case, since
    /// `w14:textFill` is MCE-ignorable and a consumer that skips it (older
    /// Word, LibreOffice) must still see a sensible solid colour.
    pub text_fill: Option<TextFill>,
    /// Keep an explicitly styled hyperlink colour as direct formatting even
    /// when it matches a hoisted document/heading default. The Hyperlink
    /// character style defines its own blue and would otherwise override it.
    pub(crate) preserve_color: bool,
    /// Character spacing / tracking in signed twips (`<w:spacing w:val=…>` in
    /// `rPr`). `text(tracking:)`. Default none.
    pub tracking: Option<i32>,
    /// Baseline shift in signed half-points (`<w:position>`). Positive = raised.
    /// `text(baseline:)` (downward-positive) is negated. Default none.
    pub position_half_pt: Option<i32>,
    pub size_half_pt: Option<u32>,
    /// `<w:highlight w:val=...>` Word's named text highlighter colours.
    pub highlight: Option<&'static str>,
    pub shd_fill: Option<[u8; 3]>,
    /// `<w:bdr>` run border (a character border box). Renders an *inline* framed
    /// container (`#box(stroke:)[..]` mid-line) as boxed text that flows in the
    /// line — Word does not flow an inline text box's content. Default none.
    pub bdr: Option<ParaBorder>,
    /// `<w:u>` underline, when present. `text(underline:)` / `#underline`. The
    /// style (`w:val`) and colour are derived from the line's stroke (dash
    /// pattern → dotted/dash/dotDash, paint → `w:color`); a plain underline is
    /// `single` with no colour (byte-identical to the original `bool`).
    pub underline: Option<Underline>,
    /// `<w:vanish/>` — hidden text (`#hide`). Default false.
    pub vanish: bool,
    pub vert_align: Option<VertAlign>,
    /// `<w:rtl/>` (run reading order is RTL). `text(dir: rtl)`. Default false.
    pub rtl: bool,
    /// `<w:cs/>` (use complex-script formatting for this run). Pairs with `rtl`.
    /// Default false.
    pub cs: bool,
    pub lang: Option<EcoString>,
}

/// Office 2010 gradient text fill geometry (`w14:textFill`/`w14:gradFill`).
/// Mirrors the two DrawingML `a:gradFill` shapes the shape exporter emits
/// (see `typst_ooxml_core::dml::FillSpec`); a conic gradient or an off-center
/// radial outer circle has no DrawingML analogue and lowers to `None` instead
/// (`crate::props::text_fill_from_gradient`).
#[derive(Clone, PartialEq)]
pub enum TextFill {
    Linear { angle_60k: i32, stops: Vec<GradientStop> },
    Radial {
        stops: Vec<GradientStop>,
        focal_center_100k: [i32; 2],
        focal_radius_100k: i32,
    },
}

#[derive(Copy, Clone, PartialEq)]
pub enum VertAlign {
    Super,
    Sub,
}

/// A `<w:u>` underline: a Word line style plus an optional explicit colour.
#[derive(Clone, PartialEq)]
pub struct Underline {
    /// `w:val`: "single" | "double" | "thick" | "dotted" | "dash" | "dotDash".
    pub val: &'static str,
    /// `w:color` (`RRGGBB`), when the line carries a paint of its own; `None`
    /// makes the underline follow the run's text colour ("auto").
    pub color: Option<[u8; 3]>,
}

impl Underline {
    /// A plain single underline with no colour of its own.
    pub fn single() -> Self {
        Self { val: "single", color: None }
    }

    /// Explicitly disables an underline inherited from a character style.
    pub fn none() -> Self {
        Self { val: "none", color: None }
    }
}

/// Paragraph formatting → `<w:pPr>`.
#[derive(Default, Clone, PartialEq)]
pub struct ParaProps {
    /// Non-serialized provenance for opt-in Word review controls.
    pub(crate) review_origin: Option<ReviewOrigin>,
    /// Non-serialized Typst paragraph-spacing components. Keeping each side's
    /// provenance separately lets container mappers model one-sided boundaries
    /// (notably lists) without confusing them with explicit `#v()` space.
    pub(crate) typst_par_spacing_before: Option<i32>,
    pub(crate) typst_par_spacing_after: Option<i32>,
    pub style: Option<EcoString>,
    pub keep_next: bool,
    /// `<w:pageBreakBefore/>`: ensure this paragraph starts on a new page.
    /// Unlike a trailing break run, this is idempotent when preceding fixed-height
    /// content has already filled the page (important for slide-shaped pages).
    pub page_break_before: bool,
    /// `<w:keepLines/>` (keep all lines on one page). Default false.
    pub keep_lines: bool,
    pub num: Option<(u32, u8)>,
    /// `<w:suppressLineNumbers/>` for paragraphs whose Typst styles explicitly
    /// disable `par.line(numbering:)` inside a numbered section.
    pub suppress_line_numbers: bool,
    /// `<w:bidi/>` (paragraph base reading order is RTL). Default false.
    pub bidi: bool,
    pub spacing: Option<Spacing>,
    pub ind: Option<Indent>,
    /// `<w:contextualSpacing/>` (suppress before/after between like paragraphs).
    /// Default false.
    pub contextual_spacing: bool,
    pub jc: Option<Jc>,
    pub outline_lvl: Option<u8>,
    pub tabs: Vec<TabStop>,
    /// `<w:shd w:fill=…>` paragraph shading (`block(fill:)`). Default none.
    pub shd_fill: Option<[u8; 3]>,
    /// `<w:pBdr>` paragraph borders (`block(stroke:)`). Default none.
    pub pbdr: Option<ParaBorders>,
}

/// The four sides of a paragraph border (`<w:pBdr>`).
#[derive(Default, Clone, PartialEq)]
pub struct ParaBorders {
    pub top: Option<ParaBorder>,
    pub left: Option<ParaBorder>,
    pub bottom: Option<ParaBorder>,
    pub right: Option<ParaBorder>,
}

impl ParaBorders {
    /// Whether all four sides are absent.
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.left.is_none()
            && self.bottom.is_none()
            && self.right.is_none()
    }
}

/// One side of a paragraph border (`CT_Border`).
#[derive(Copy, Clone, PartialEq)]
pub struct ParaBorder {
    /// `w:val`: "single" | "dashed" | "dotted".
    pub style: &'static str,
    /// `w:sz` in eighths of a point.
    pub sz: u32,
    /// `w:space` in points (0..=31).
    pub space: u32,
    pub color: [u8; 3],
}

#[derive(Copy, Clone, PartialEq)]
pub enum Jc {
    Start,
    End,
    Center,
    Both,
}

#[derive(Default, Clone, PartialEq)]
pub struct Spacing {
    pub before: Option<i32>,
    pub after: Option<i32>,
    pub line: Option<i32>,
    /// `w:lineRule="auto"` (line value is 240ths of a line). Mutually exclusive
    /// with `line_rule_at_least`; both false → `"exact"`.
    pub line_rule_auto: bool,
    /// `w:lineRule="atLeast"` (line value is a twip minimum). Default false.
    pub line_rule_at_least: bool,
}

#[derive(Default, Clone, PartialEq)]
pub struct Indent {
    pub left: Option<i32>,
    pub right: Option<i32>,
    pub first_line: Option<i32>,
    pub hanging: Option<i32>,
}

#[derive(Clone, PartialEq)]
pub struct TabStop {
    pub val: TabAlign,
    pub leader: Option<TabLeader>,
    pub pos: i32,
}

#[derive(Copy, Clone, PartialEq)]
pub enum TabAlign {
    Start,
    End,
    Center,
}

#[derive(Copy, Clone, PartialEq)]
pub enum TabLeader {
    Dot,
    Hyphen,
    Underscore,
}

/// Who owns the value of a Word field after export.
///
/// Word's document-level `updateFields` setting recalculates *all* unlocked
/// fields. Keeping this policy on every field prevents a live TOC or page
/// reference from silently replacing a Typst-computed semantic reference or
/// custom figure number.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FieldMode {
    /// Typst owns the cached result. Emit `w:fldLock="true"` so a global field
    /// refresh cannot replace it with a non-equivalent Word interpretation.
    Static,
    /// The consumer owns the value and updates it through its normal layout or
    /// explicit field-update behavior. Does not request a global open refresh.
    Live,
}

/// Whether a field result participates in the visible document.
///
/// Word's `SEQ \h` switch hides a sequence result, but LibreOffice does not
/// honor that switch consistently. Hidden fields therefore also carry hidden
/// run formatting in their OOXML representation.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FieldDisplay {
    Visible,
    Hidden,
}

/// Provenance of the cached result carried by a complex field.
///
/// This is deliberately separate from [`FieldMode`]: ownership says who may
/// update the field after export, while this says whether the package already
/// contains a trustworthy value. Consumers treat a missing cache very
/// differently from an intentionally consumer-computed field, so lowering must
/// decide this before serialization rather than letting an empty `Vec` encode
/// both states.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FieldCacheStatus {
    /// Typst produced a usable cached result.
    Resolved,
    /// Typst emitted a visible placeholder that is not semantically
    /// trustworthy; the consumer owns refreshing the live field.
    BestEffort,
    /// The field is intentionally left for Word/Writer to compute (for example
    /// PAGE/NUMPAGES), or is hidden and has no visible result by design.
    ConsumerRequired,
    /// Typst attempted to compute a cache but failed. The field remains a
    /// best-effort consumer-owned fallback and the diagnostic is retained in
    /// the fidelity report.
    Unavailable,
}

impl FieldMode {
    pub(crate) fn locked(self) -> bool {
        self == Self::Static
    }
}

/// A complex field code run sequence with an explicit value-ownership policy.
pub struct Field {
    pub instr: EcoString,
    pub result: Vec<Run>,
    pub mode: FieldMode,
    pub display: FieldDisplay,
    pub cache_status: FieldCacheStatus,
}

/// An image. Inline (`anchor: None`) or floating (`anchor: Some`).
pub struct Drawing {
    /// The fallback raster image relationship used by `<a:blip r:embed>`.
    pub rel: EcoString,
    /// Optional native SVG relationship referenced from `<asvg:svgBlip>`.
    /// When present, `rel` remains the required raster fallback.
    pub svg_rel: Option<EcoString>,
    /// Optional pair of document-property IDs for a Word-2013 choice whose
    /// compatibility fallback tiles one full-container raster into two bands.
    pub compatibility_split_ids: Option<[u32; 2]>,
    pub w_emu: i64,
    pub h_emu: i64,
    /// Source-space origin normalized out of a native shape path. Applied to
    /// its eventual floating anchor so explicit line/curve coordinates survive.
    pub source_offset_emu: [i64; 2],
    pub alt: Option<EcoString>,
    /// Office 2019+ accessibility intent. Decorative drawings are deliberately
    /// skipped by assistive technology and therefore must not also carry alt
    /// text or native text-box content.
    pub decorative: bool,
    pub docpr_id: u32,
    pub name: EcoString,
    /// `None` = inline (`<wp:inline>`); `Some` = floating (`<wp:anchor>`).
    /// Defaults to `None` so every existing inline image is byte-identical.
    pub anchor: Option<Anchor>,
    /// `None` = a raster picture (`pic:pic`, uses `rel`); `Some` = a vector
    /// DrawingML shape (`wps:wsp`, ignores `rel`).
    pub shape: Option<ShapeSpec>,
    /// `Some` = multiple native shapes composed together (a `wpg:wgp` group —
    /// e.g. a `#move`d composition of several shapes/lines/curves that share
    /// one coordinate space), taking priority over `shape`/`rel`. `None` for
    /// every other drawing.
    pub group: Option<GroupSpec>,
    /// How the raster picture is framed. Default = the whole image in a plain
    /// rectangle, which is every picture that is not natively clipped.
    pub pic_clip: PicClip,
}

/// The DrawingML framing of a raster picture: the preset outline it is cut to,
/// and which part of the source image shows through it. Together these express
/// a Typst `#box(radius: .., clip: true)[image]` natively — the outline rounds
/// the corners and the source rectangle takes the cover overflow — instead of
/// flattening the clip into a rasterized region.
#[derive(Default, Clone, Copy)]
pub struct PicClip {
    pub geom: PicGeom,
    /// `<a:srcRect>` `[left, top, right, bottom]` insets in 1/1000 of a
    /// percent of the source image, or `None` for the whole image.
    pub src_rect: Option<[i32; 4]>,
}

/// A raster picture's preset outline (`pic:spPr/a:prstGeom`).
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum PicGeom {
    /// `prst="rect"` — an ordinary, unclipped picture.
    #[default]
    Rect,
    /// `prst="roundRect"`, with the corner radius as an `adj` guide in 1/1000
    /// of a percent of the shorter side. A full `adj` of 50000 is Word's
    /// circle/stadium, which is what a `radius: 50%` clip means.
    RoundRect { adj_100k: i32 },
}

impl Drawing {
    pub(crate) fn has_native_text(&self) -> bool {
        self.shape.as_ref().is_some_and(|shape| shape.txbx.is_some())
            || self.group.as_ref().is_some_and(|group| {
                group.children.iter().any(|child| child.shape.txbx.is_some())
            })
    }
}

/// Several native shapes sharing one local coordinate space (a `wpg:wgp`
/// group), used when a container's entire content is a composition of
/// natively-representable shapes — recovering it as one editable, grouped
/// drawing instead of rasterizing the whole composition.
pub struct GroupSpec {
    pub children: Vec<GroupChild>,
}

/// One shape within a [`GroupSpec`], positioned in the group's own local
/// coordinate space (which spans `[0, w] x [0, h]` of the group's overall
/// extent — the same non-negative convention `ShapeGeom::Path` already uses).
pub struct GroupChild {
    pub x_emu: i64,
    pub y_emu: i64,
    pub w_emu: i64,
    pub h_emu: i64,
    pub shape: ShapeSpec,
}

/// A vector DrawingML shape (a `#rect`/`#circle`/`#polygon`/… mapped to a Word
/// `wps:wsp` instead of a rasterized image).
pub struct ShapeSpec {
    pub geom: ShapeGeom,
    /// The shape's fill, or `None` for no fill.
    pub fill: Option<ShapeFill>,
    pub stroke: Option<ShapeStroke>,
    /// Real editable text framed by the shape (`wps:txbx`). `Some` turns the
    /// shape into a Word *text box* (a `#box(fill|stroke)[text]`); `None` is a
    /// bare decorative shape. Default `None`.
    pub txbx: Option<TextBox>,
}

/// The text-box content of a shape (`wps:txbx` → `w:txbxContent`): real
/// paragraphs the consumer can edit, with the box's inset reproduced as the
/// text-frame insets `[left, top, right, bottom]` in EMU.
pub struct TextBox {
    pub ins: [i64; 4],
    pub blocks: Vec<Block>,
    pub wrap: TextBoxWrap,
    /// Let Office resize the shape to its text. Authored standalone text boxes
    /// want this; positioned canvas labels must retain their measured extent.
    pub autofit: bool,
}

#[derive(Copy, Clone)]
pub enum TextBoxWrap {
    /// Respect the measured frame width and reflow text within it.
    Square,
    /// Natural-width placed text: do not introduce consumer-side line wraps.
    None,
}

/// A shape's geometry. Coordinates for [`ShapeGeom::Path`] are in EMU within the
/// shape's bounding box.
pub enum ShapeGeom {
    Rect,
    /// A rounded rectangle, with the corner radius as an `adj` guide in 1/1000
    /// of a percent of the shorter side.
    RoundRect { adj_100k: i32 },
    Ellipse,
    /// An arbitrary vector path — straight and cubic-Bézier segments, mapping
    /// 1:1 to `#curve`'s Move/Line/Cubic/Close (a `#polygon`, or a diagonal
    /// `#line`, is the all-straight-segment special case). Coordinates are
    /// pre-shifted so the whole path is non-negative, matching the shape's
    /// declared bounding box (the OOXML `a:custGeom` coordinate convention).
    Path(Vec<PathSegment>),
}

/// Floating-image placement (`<wp:anchor>`): positionH/V + wrap.
pub struct Anchor {
    /// `relativeHeight` z-order (monotonic per drawing).
    pub z: u32,
    /// Horizontal position: `relativeFrom` + (align XOR offset).
    pub pos_h: AnchorPos,
    /// Vertical position: `relativeFrom` + (align XOR offset).
    pub pos_v: AnchorPos,
    pub wrap: AnchorWrap,
    /// `distT`/`distB`/`distL`/`distR` in EMU.
    pub dist: [i64; 4],
    /// `behindDoc` — place the drawing *behind* the text (a page background /
    /// watermark) rather than in front.
    pub behind: bool,
}

/// One axis of an anchor position (`<wp:positionH>` / `<wp:positionV>`).
pub struct AnchorPos {
    /// `relativeFrom`, e.g. "margin" | "page" (axis-specific; caller picks a
    /// valid value for the axis).
    pub rel_from: &'static str,
    /// `<wp:align>` value ("left|center|right" for H, "top|bottom|center" for
    /// V). Exactly one of `align` / `offset` is `Some`.
    pub align: Option<&'static str>,
    /// `<wp:posOffset>` in EMU. Exactly one of `align` / `offset` is `Some`.
    pub offset: Option<i64>,
}

/// The wrap mode for a floating drawing.
#[derive(Copy, Clone)]
pub enum AnchorWrap {
    /// `<wp:wrapTopAndBottom/>` — text flows above and below.
    TopAndBottom,
    /// `<wp:wrapSquare wrapText=…/>` — text wraps around the box.
    Square(&'static str),
    /// `<wp:wrapNone/>` — drawing floats over the text (overlap allowed).
    None,
}

/// A table.
pub struct Tbl {
    pub props: TblProps,
    pub grid: Vec<i32>,
    pub rows: Vec<Row>,
}

#[derive(Default)]
pub struct TblProps {
    pub width_dxa: Option<i32>,
    pub style: Option<EcoString>,
    pub jc: Option<Jc>,
    /// `<w:tblInd>` — the table's left offset from the text margin (twips).
    /// Word indents a table with this rather than with the `w:ind` that
    /// indents a paragraph. Default none.
    pub ind_dxa: Option<i32>,
}

pub struct Row {
    pub header: bool,
    pub cant_split: bool,
    pub height: Option<RowHeight>,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct RowHeight {
    pub val: i32,
    pub exact: bool,
}

pub struct Cell {
    pub w_dxa: Option<i32>,
    pub grid_span: u32,
    pub v_merge: Option<VMerge>,
    pub borders: CellBorders,
    pub shd_fill: Option<[u8; 3]>,
    pub margins: CellMargins,
    pub valign: Option<VAlign>,
    pub blocks: Vec<Block>,
}

#[derive(Copy, Clone, Default)]
pub struct CellMargins {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

#[derive(Copy, Clone)]
pub enum VMerge {
    Restart,
    Continue,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Default)]
pub struct CellBorders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
}

#[derive(Copy, Clone)]
pub struct Border {
    /// Width in eighths of a point.
    pub sz: u32,
    pub color: [u8; 3],
}

/// Page setup → `<w:sectPr>`.
#[derive(Clone)]
pub struct SectPr {
    /// Page width in twips.
    pub page_w: i32,
    /// Page height in twips.
    pub page_h: i32,
    pub landscape: bool,
    pub margin_top: i32,
    pub margin_bottom: i32,
    pub margin_left: i32,
    pub margin_right: i32,
    pub header: i32,
    pub footer: i32,
    pub columns: u32,
    /// Binding allowance in twips (`w:pgMar/@w:gutter`). Default 0.
    pub gutter: i32,
    /// Equal-width column gutter in twips (Word `w:cols/@w:space`). Default 720.
    pub col_space: i32,
    /// Section line numbering (`w:lnNumType`) when `par.line(numbering:)` is
    /// active for this section.
    pub line_numbers: Option<LineNumbering>,
    /// Page-number glyph format + start, if `set page(numbering:)` is active.
    pub pg_num: Option<PgNumType>,
    /// `w:type` (only for non-final sections / `pagebreak(to:)`); None = default `nextPage`.
    pub sect_type: Option<SectType>,
    /// Vertical alignment of the section body (`w:vAlign`). None is Word's
    /// default top alignment.
    pub vertical_align: Option<VAlign>,
    /// Header references (r:id + type). Emitted BEFORE pgSz.
    pub headers: Vec<HdrFtrRef>,
    /// Footer references (r:id + type). Emitted after headers, BEFORE pgSz.
    pub footers: Vec<HdrFtrRef>,
    /// `<w:titlePg/>` (distinct first page). Default false.
    pub title_pg: bool,
}

/// Line numbering settings for `<w:lnNumType>`.
#[derive(Clone, PartialEq)]
pub struct LineNumbering {
    pub count_by: u32,
    pub start: u32,
    /// `continuous` | `newPage` | `newSection`.
    pub restart: &'static str,
    /// Distance between the body text and line number in twips.
    pub distance: i32,
}

/// Page-number format + start for `<w:pgNumType>`.
#[derive(Clone)]
pub struct PgNumType {
    /// `w:fmt` value: "decimal" | "lowerRoman" | "upperRoman" | "lowerLetter" |
    /// "upperLetter" | "decimalZero".
    pub fmt: &'static str,
    /// `w:start`, from `counter(page).update(n)`; None = omit.
    pub start: Option<i64>,
}

/// `<w:type>` value on a non-final section.
#[derive(Copy, Clone)]
pub enum SectType {
    NextPage,
    EvenPage,
    OddPage,
    Continuous,
}

/// A header/footer reference inside `<w:sectPr>`.
#[derive(Clone)]
pub struct HdrFtrRef {
    /// `w:type`: "default" | "first" | "even".
    pub kind: &'static str,
    /// Matching relationship id in document.xml.rels.
    pub rel: EcoString,
}

/// A header/footer part (`word/headerN.xml` / `word/footerN.xml`).
pub struct HdrFtrPart {
    /// File name e.g. "header1.xml" (relative to word/).
    pub part_name: EcoString,
    /// true = header (root `w:hdr`, header content-type), false = footer (`w:ftr`).
    pub is_header: bool,
    pub blocks: Vec<Block>,
    /// Relationships (images, external links) created while lowering this part's
    /// content. They MUST live in this part's own `word/_rels/<name>.rels` — an
    /// `r:id` in `header1.xml` resolves against `header1.xml.rels`, not the
    /// document's — or Word refuses to open the file.
    pub rels: crate::package::Rels,
}

impl Default for SectPr {
    fn default() -> Self {
        // US Letter, 1-inch margins (in twips: 1 inch = 1440). The driver
        // overwrites this with the resolved page setup; it remains a sane
        // fallback for callers that build a `SectPr` directly.
        Self {
            page_w: 12240,
            page_h: 15840,
            landscape: false,
            margin_top: 1440,
            margin_bottom: 1440,
            margin_left: 1440,
            margin_right: 1440,
            header: 720,
            footer: 720,
            columns: 1,
            gutter: 0,
            col_space: 720,
            line_numbers: None,
            pg_num: None,
            sect_type: None,
            vertical_align: None,
            headers: Vec::new(),
            footers: Vec::new(),
            title_pg: false,
        }
    }
}

/// One footnote entry routed to `footnotes.xml`.
pub struct Footnote {
    pub id: i32,
    pub blocks: Vec<Block>,
}

// ---------------------------------------------------------------------------
// Numbering (lists / enums).
// ---------------------------------------------------------------------------

/// The numbering table for `numbering.xml`.
#[derive(Default)]
pub struct NumberingTable {
    /// Distinct abstract numbering definitions.
    pub abstracts: Vec<AbstractNum>,
    /// Concrete `<w:num>` instances mapping a numId to an abstractNumId.
    pub nums: Vec<NumInstance>,
}

pub struct AbstractNum {
    pub id: u32,
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
}

pub struct NumInstance {
    pub num_id: u32,
    pub abstract_id: u32,
    /// Optional level-0 start override.
    pub start_override: Option<u64>,
}

/// The numbering shape passed to `register_list`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListSpec {
    pub levels: Vec<ListLevel>,
    pub multilevel: MultiLevelType,
    pub restart_at_1: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ListLevel {
    pub num_fmt: NumFmt,
    pub lvl_text: EcoString,
    pub start: u64,
    pub ind_left: i32,
    pub ind_hanging: i32,
    pub bullet_font: Option<EcoString>,
    /// `w:pStyle` — the paragraph style whose paragraphs this level numbers.
    /// A list joins its numbering per paragraph (`w:numPr` on each `w:p`);
    /// heading numbering instead binds the level to the `HeadingN` style, so
    /// every heading Word later creates in that style is numbered too.
    pub pstyle: Option<EcoString>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum NumFmt {
    Bullet,
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    None,
}

impl NumFmt {
    pub fn as_str(self) -> &'static str {
        match self {
            NumFmt::Bullet => "bullet",
            NumFmt::Decimal => "decimal",
            NumFmt::LowerLetter => "lowerLetter",
            NumFmt::UpperLetter => "upperLetter",
            NumFmt::LowerRoman => "lowerRoman",
            NumFmt::UpperRoman => "upperRoman",
            NumFmt::None => "none",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum MultiLevelType {
    SingleLevel,
    Multilevel,
    HybridMultilevel,
}

impl MultiLevelType {
    pub fn as_str(self) -> &'static str {
        match self {
            MultiLevelType::SingleLevel => "singleLevel",
            MultiLevelType::Multilevel => "multilevel",
            MultiLevelType::HybridMultilevel => "hybridMultilevel",
        }
    }
}

// ---------------------------------------------------------------------------
// Bookmarks.
// ---------------------------------------------------------------------------

use rustc_hash::FxHashMap;

/// Maps `Location`s to their assigned bookmark `(name, id)`.
#[derive(Default)]
pub struct BookmarkTable {
    pub by_location: FxHashMap<Location, (EcoString, u32)>,
}

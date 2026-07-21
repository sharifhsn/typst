//! The **Word IR** — a faithful, lightly-typed parse of `word/document.xml`
//! and its companions (`styles.xml`, `numbering.xml`, relationships, media).
//! Deliberately a sibling of `typst-docx`'s DOM rather than that type itself:
//! it drops export-only concerns (review origins, field cache status, bookmark
//! id allocation) and keeps only what the importer needs to lower to Typst.
//!
//! Property structs store *raw* OOXML values (twips, half-points, hex colors,
//! style ids) — resolution against the style hierarchy happens in
//! [`crate::resolve`], and unit conversion happens in the mappers.

use ecow::EcoString;
use rustc_hash::FxHashMap;

/// `docProps/core.xml` — the package's core properties (Word's File → Info
/// panel). Typst carries the same set on `#set document(..)` and `typst-docx`
/// already writes them on export, so importing them closes the round-trip
/// rather than leaving metadata as an export-only nicety.
///
/// Values are kept verbatim as Word wrote them; the list-shaped ones are split
/// at lower time (see `lower::document_info`), mirroring how [`RunProps`]
/// keeps raw hex colors for the mappers to resolve.
#[derive(Debug, Default, Clone)]
pub struct DocumentMeta {
    /// `dc:title`.
    pub title: Option<EcoString>,
    /// `dc:creator` — one semicolon-separated string when there are several
    /// authors, which is exactly the form `typst-docx` writes.
    pub creator: Option<EcoString>,
    /// `dc:description`. Typst's `document` has no counterpart, so this is
    /// reported as a drop rather than silently discarded.
    pub description: Option<EcoString>,
    /// `cp:keywords` — comma-separated, Word's own convention.
    pub keywords: Option<EcoString>,
    /// `dcterms:created`, a W3CDTF timestamp.
    pub created: Option<EcoString>,
}

/// Everything parsed out of the package that the importer consumes.
#[derive(Debug, Default)]
pub struct WmlPackage {
    pub body: Body,
    pub styles: Styles,
    pub numbering: Numbering,
    /// `docProps/core.xml`, if the package has one.
    pub meta: DocumentMeta,
    /// Every `w:bookmarkStart` name in the document, mapped to the Typst label
    /// it lowers to. Built once after parsing (see
    /// [`crate::wml::parse::collect_bookmarks`]) because resolution runs in
    /// both directions: a paragraph needs the label to *emit*, while a
    /// `REF`/`PAGEREF` field or an internal hyperlink elsewhere in the
    /// document — possibly earlier than the bookmark itself — needs to know
    /// the target exists before it can link to it.
    pub bookmarks: FxHashMap<EcoString, EcoString>,
    /// `rId` → relationship target (image part name, hyperlink URL, …).
    /// Header/footer parts number their own `rId`s independently of
    /// `document.xml` (see [`Self::furniture`]), so an id resolved from a
    /// furniture body is namespaced `"{part}!{rid}"` rather than bare —
    /// [`crate::wml::parse`] rewrites those ids as it parses each part, so
    /// every lookup against this map (bare or namespaced) just works.
    pub rels: FxHashMap<EcoString, Relationship>,
    /// Media parts by zip name (`word/media/image1.png` → bytes).
    pub media: FxHashMap<EcoString, Vec<u8>>,
    /// Parsed `w:hdr`/`w:ftr` parts by zip name (`word/header1.xml` → body).
    /// Headers and footers share one map: they are structurally identical and
    /// the `sectPr` reference is what gives a part's role. A flat item list
    /// rather than a [`Body`] — a header/footer part can never carry its own
    /// `w:sectPr`, so there is no section boundary to model.
    pub furniture: FxHashMap<EcoString, Vec<BodyItem>>,
    /// `settings.xml` declares `w:evenAndOddHeaders`.
    pub even_and_odd_headers: bool,
    /// `settings.xml` declares `w:mirrorMargins` — the left/right margins swap
    /// on facing pages, which is Typst's `page(binding:)` model rather than a
    /// per-section property (see [`SectPr::gutter`], its companion).
    pub mirror_margins: bool,
    /// `word/footnotes.xml` bodies by `w:id`, boilerplate separators excluded.
    /// Flat, same reasoning as [`Self::furniture`].
    pub footnotes: FxHashMap<i64, Vec<BodyItem>>,
    /// `word/endnotes.xml`, likewise.
    pub endnotes: FxHashMap<i64, Vec<BodyItem>>,
    /// `word/comments.xml` by `w:id`. Unlike a note, a comment carries its own
    /// authorship, so this is a struct rather than a bare item list.
    pub comments: FxHashMap<i64, Comment>,
    /// Parsed chart parts by zip name (`word/charts/chart1.xml` → data). A
    /// chart's `r:id` reference (see [`RunContent::Chart`]) resolves through
    /// [`Self::rels`] to a target *name*; this map is keyed by the full zip
    /// name that target resolves to, the same convention [`Self::media`]
    /// uses for images.
    pub charts: FxHashMap<EcoString, ChartData>,
}

/// One `w:comment` from `word/comments.xml`: who wrote it, when, and what
/// they wrote. The body is ordinary body content — a comment can hold several
/// paragraphs, formatting, even a table.
#[derive(Debug, Default)]
pub struct Comment {
    pub author: Option<EcoString>,
    pub initials: Option<EcoString>,
    /// `@w:date`, a W3CDTF timestamp, kept verbatim.
    pub date: Option<EcoString>,
    pub body: Vec<BodyItem>,
}

#[derive(Debug, Clone)]
pub struct Relationship {
    pub target: EcoString,
    pub external: bool,
}

/// `word/document.xml`'s `w:body`: an ordered list of sections, always at
/// least one. A `w:sectPr` inside a paragraph's `w:pPr` marks the *end* of a
/// section — that paragraph is the section's last item, and the `sectPr`
/// describes the section just finished; the body-level `w:sectPr` (a direct
/// child of `w:body`, always last) describes the final section. A document
/// with no `w:sectPr` at all — neither on a paragraph nor at the body's end —
/// is one section with default properties (see `wml::parse::parse_document_body`).
///
/// Only `word/document.xml`'s own body is shaped this way: a header/footer
/// part, a footnote/endnote body, and a text box's content are all the same
/// paragraph/table content but can never carry a section boundary of their
/// own, so they stay a plain `Vec<BodyItem>` (see [`WmlPackage::furniture`]).
#[derive(Debug, Default)]
pub struct Body {
    pub sections: Vec<Section>,
}

/// One Word section: the content it governs, and the page setup that applies
/// to it (`w:sectPr`).
#[derive(Debug, Default)]
pub struct Section {
    pub items: Vec<BodyItem>,
    pub props: SectPr,
}

#[derive(Debug)]
// A body is a `Vec<BodyItem>`; the paragraph/table size gap doesn't matter for
// a heap-allocated sequence, and boxing every item would only add indirection.
#[allow(clippy::large_enum_variant)]
pub enum BodyItem {
    Paragraph(Paragraph),
    Table(Table),
}

#[derive(Debug, Default)]
pub struct Paragraph {
    pub props: ParaProps,
    pub runs: Vec<RunItem>,
}

#[derive(Debug)]
pub enum RunItem {
    Run(Run),
    /// `w:hyperlink` — a link wrapping inline content; either an external
    /// `rel_id` (into [`WmlPackage::rels`]) or an internal `anchor` (bookmark
    /// name). `runs` holds `RunItem` (not `Run`) so a field can appear inside
    /// a hyperlink's content — common for cross-references and TOC entries,
    /// where the hyperlink supplies the jump target and a nested
    /// PAGEREF/REF field supplies the displayed page number.
    Hyperlink { rel_id: Option<EcoString>, anchor: Option<EcoString>, runs: Vec<RunItem> },
    /// `w:bookmarkStart` — a named anchor. Word writes a bookmark as a
    /// start/end pair *around* a range, but Typst has only point labels, so
    /// only the start is modelled: it is the position a `REF`/`PAGEREF` field
    /// or an internal hyperlink jumps to, which is all a jump target needs.
    Bookmark(EcoString),
    /// `w:commentRangeStart` / `w:commentRangeEnd` — the two ends of the span
    /// a comment annotates. Word writes these as siblings of the runs they
    /// bracket (the `w:commentReference` mark that goes *with* them lives
    /// inside a run instead — see [`RunContent::CommentRef`]).
    ///
    /// Modelled as two independent points rather than a range because that is
    /// what Typst can express: a label attaches to one element, so a span
    /// becomes an opening anchor and a closing one. Unlike a bookmark, which
    /// genuinely only needs its start, both ends matter here — they are what
    /// says *which words* the comment is about.
    CommentRange { id: i64, end: bool },
    /// A Word field. Both OOXML spellings — the `w:fldSimple` element and the
    /// flattened `w:fldChar` begin/separate/end run sequence — are folded
    /// back into this one logical item at parse time, so lowering sees a
    /// field as a field rather than as loose punctuation runs. See
    /// [`crate::mappers::field`] for how `instr` is interpreted.
    Field(Field),
}

/// A Word field: `w:fldSimple`, or the flattened `w:fldChar`
/// begin/separate/end run sequence, folded back into one logical item by
/// [`crate::wml::parse`].
#[derive(Debug, Default)]
pub struct Field {
    /// The raw instruction, e.g. ` PAGE ` or ` HYPERLINK "https://x" \o "t" `.
    pub instr: EcoString,
    /// The cached result — what Word last rendered for this field. Fields can
    /// nest (e.g. a TOC entry whose result contains a HYPERLINK field), so
    /// this is `RunItem`s rather than plain runs.
    pub result: Vec<RunItem>,
}

#[derive(Debug, Default)]
pub struct Run {
    pub props: RunProps,
    pub content: Vec<RunContent>,
}

#[derive(Debug)]
pub enum RunContent {
    Text(EcoString),
    Tab,
    Break(BreakType),
    /// A `w:drawing` inline/anchored image → the `rId` of its blip.
    Drawing(DrawingRef),
    /// OMML math (`m:oMath`) captured as a raw XML fragment.
    ///
    /// `display` marks an equation Word set as its own block — one that came
    /// from an `m:oMathPara` wrapper rather than sitting inline among text.
    /// The wrapper is flattened at parse time (one fragment per `m:oMath`), so
    /// without this flag the block/inline distinction would be lost and every
    /// equation would come back as inline `$..$`.
    Math { xml: EcoString, display: bool },
    /// `w:ruby` — a phonetic guide (furigana): `gloss` is the small reading
    /// set above `base`. Both halves hold ordinary runs, and `w:ruby` sits
    /// *inside* a `w:r`, which is why it is run content rather than a
    /// [`RunItem`] beside one.
    Ruby { base: Vec<RunItem>, gloss: Vec<RunItem> },
    /// `w:commentReference` — the mark Word draws at a comment's anchor,
    /// inside a run of its own. A comment always has one; the surrounding
    /// [`RunItem::CommentRange`] pair is what Word omits for a point comment,
    /// which is why the payload is attached to whichever anchor comes first
    /// (see [`crate::mappers::comment`]).
    CommentRef(i64),
    /// A `w:footnoteReference`/`w:endnoteReference` — the marker in the body
    /// text. The note's content lives in a separate part, keyed by this id.
    NoteRef { endnote: bool, id: i64 },
    /// A shape's text (`w:txbxContent`) — the body content of a DrawingML
    /// text box (`wps:txbx`) or its VML equivalent (`v:textbox`). Word floats
    /// these; we keep the content and lose the geometry.
    TextBox(Vec<BodyItem>),
    /// A charted `w:drawing`. Reuses [`DrawingRef`] because a chart *is* a
    /// drawing: it carries the same `rId` and `wp:extent`, and Word's extent
    /// is the chart's authored size, which the plot renderer needs.
    /// The `rId` here points at its `c:chart`/`cx:chart` part.
    /// Typst has no chart-drawing primitive, but the chart's cached data
    /// lives in that separate part (resolved against
    /// [`WmlPackage::charts`]), not inline here, so lowering it to a table
    /// keeps the information instead of dropping it — see [`ChartData`].
    Chart(DrawingRef),
    /// WordArt (`v:textpath`'s `string` attribute) — genuine document text
    /// with no Typst equivalent for the curved/warped path it's drawn along,
    /// so it's kept as plain text and the styling loss is reported once at
    /// lower time (see `mappers::run`). A VML *picture* (`v:imagedata`)
    /// needs no variant of its own here — it resolves through the same
    /// relationship-id lookup as a DrawingML picture, so it's folded
    /// straight into [`Self::Drawing`] at parse time (see `wml::parse`'s
    /// `vml_shape_content`).
    VmlText(EcoString),
    /// A VML `v:rect`/`v:oval`/`v:roundrect`/`v:line` — a shape Typst *can*
    /// draw natively, unlike the custom-geometry case [`Self::VmlUnsupported`]
    /// covers. See `mappers::shape` for the width/height/color parsing and
    /// the `#rect`/`#circle`/`#ellipse`/`#line` mapping.
    VmlShape(VmlShape),
    /// A VML `v:shape` with nothing this importer can extract: no picture,
    /// no text box, no WordArt text — typically one with custom
    /// `v:path`/`v:formulas` geometry (a callout, a star, …), which would
    /// need a drawing package to render faithfully. Recorded as a drop at
    /// lower time rather than silently vanishing (see `wml::parse`'s
    /// `vml_shape_content` for why a shape holding *only* a plain text box
    /// never reaches this variant).
    VmlUnsupported,
    /// A DrawingML shape (`wps:wsp`) — the modern spelling of everything
    /// [`Self::VmlShape`] covers, plus the arbitrary vector geometry VML only
    /// ever reached through the `v:path` mini-language this importer declines
    /// to interpret. See [`DmlShape`] and `mappers::dml_shape`.
    DmlShape(DmlShape),
    /// A `w:object` — an OLE embedding (an Excel sheet, a Visio drawing, an
    /// equation from a pre-2007 editor, an ActiveX control). The payload is a
    /// whole foreign application's document, which nothing here can revive, so
    /// only the loss is modelled: `prog_id` is `o:OLEObject/@ProgID`
    /// ("Excel.Sheet.12"), which is what makes the report say *what* was lost
    /// rather than just "an object".
    ///
    /// Word writes a rendered **preview picture** next to it (a `v:shape` with
    /// `v:imagedata`), and that is parsed by the ordinary VML path into a
    /// sibling [`Self::Drawing`] — so the object still *shows* what it looked
    /// like, it just isn't live any more.
    EmbeddedObject { prog_id: Option<EcoString> },
    /// A `wps:wsp` with geometry but nothing this importer can paint it with:
    /// no `a:srgbClr` fill or line colour (a theme-coloured shape, whose
    /// palette lives in `theme1.xml`), and no text box to fall back on. The
    /// DrawingML twin of [`Self::VmlUnsupported`], and recorded as a drop at
    /// lower time for the same reason — these used to vanish silently.
    DmlUnsupported,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BreakType {
    Line,
    Page,
    Column,
}

#[derive(Debug, Default, Clone)]
pub struct DrawingRef {
    pub rel_id: EcoString,
    /// Extent in EMU, if present (`wp:extent`, or — for a VML picture — its
    /// `style` attribute's `width`/`height`, converted once at parse time so
    /// this field keeps a single unit contract regardless of which markup
    /// produced it).
    pub cx_emu: Option<i64>,
    pub cy_emu: Option<i64>,
    pub alt: Option<EcoString>,
    /// `wp:anchor/wp:positionH/wp:align` — a floating drawing's *named*
    /// horizontal placement ("left"/"center"/"right"). An inline drawing has
    /// none. The absolute `wp:posOffset` spelling is deliberately not read:
    /// it is page-relative geometry with no equivalent in Typst's flow, and
    /// guessing at it would move images somewhere Word never put them.
    pub align: Option<EcoString>,
    /// `pic:spPr/a:prstGeom` — the outline Word frames the picture with. A
    /// plain `rect` (the overwhelmingly common case) is the *absence* of a
    /// frame and is not recorded here; only a shaped one is, because only a
    /// shaped one has to become a `#box(radius: .., clip: true)` around the
    /// image. See [`crate::mappers::drawing`] for the mapping.
    pub prst_geom: Option<PresetGeom>,
    /// `pic:blipFill/a:srcRect` — which sub-rectangle of the *source* image
    /// shows through the frame, i.e. a crop. `None` means the whole image.
    pub src_rect: Option<SrcRect>,
}

/// An `a:prstGeom`: the preset name plus its first adjustment guide
/// (`a:avLst/a:gd/@fmla="val N"`), which for `roundRect` is the corner radius
/// as 1000ths of a percent of the shape's *shorter* side — the inverse of
/// `typst_ooxml_core::dml::round_rect_adj`.
#[derive(Debug, Default, Clone)]
pub struct PresetGeom {
    pub prst: EcoString,
    pub adj: Option<i64>,
}

/// An `a:srcRect`: how much of each side of the source image is cropped away,
/// each in 1000ths of a percent **of the original image's own extent** (so
/// `l="10000"` hides the leftmost 10% of the picture). The exporter's
/// `cover_src_rect` writes exactly this; [`crate::mappers::drawing`] inverts it.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct SrcRect {
    pub l: i64,
    pub t: i64,
    pub r: i64,
    pub b: i64,
}

impl SrcRect {
    /// Whether this rectangle crops nothing at all — the form Word writes for
    /// an uncropped picture, which must not turn into a pointless oversize-
    /// and-offset construction.
    pub fn is_empty(&self) -> bool {
        *self == SrcRect::default()
    }
}

// --- DrawingML shapes (`wps:wsp`, in a `w:drawing`) -------------------------

/// A DrawingML shape: geometry, fill, stroke, and — for a shape Word also gave
/// a text box — the content inside it.
///
/// Kept raw the same way [`VmlShape`] is (EMU lengths, 1000ths-of-a-percent
/// ratios, packed RGBA); `crate::mappers::dml_shape` does every conversion.
/// Unlike VML, DrawingML states arbitrary vector paths in a form Typst's
/// `#curve` mirrors one-for-one, so this variant can carry geometry VML's
/// counterpart deliberately gives up on.
// Not `Clone`, for the same reason [`RunContent`] isn't: `body` holds
// [`BodyItem`]s, and the Word IR is walked once and consumed rather than
// copied around.
#[derive(Debug)]
pub struct DmlShape {
    pub geom: DmlGeometry,
    /// The shape's own `a:xfrm/a:ext`, falling back to the drawing's
    /// `wp:extent`, in EMU. `None` when neither is present.
    pub cx_emu: Option<i64>,
    pub cy_emu: Option<i64>,
    pub fill: DmlFill,
    /// `a:ln`. `None` means the element is absent entirely, which is not the
    /// same as an `a:ln` holding `a:noFill` (see [`DmlStroke::no_fill`]).
    pub stroke: Option<DmlStroke>,
    /// `wps:txbx/w:txbxContent` — the shape's text, as ordinary body content.
    /// Empty for a shape with no text box.
    pub body: Vec<BodyItem>,
}

/// A shape's outline: one of DrawingML's named presets, or an explicit path.
#[derive(Debug, Clone)]
pub enum DmlGeometry {
    Preset(PresetGeom),
    /// `a:custGeom`'s single `a:path`. `path_w`/`path_h` are the path's own
    /// coordinate space (`a:path/@w`/`@h`), which is *not* required to equal
    /// the shape's extent — the two are proportional, so the segment
    /// coordinates need scaling by `ext / path` before they mean anything in
    /// document space.
    Custom { path_w: i64, path_h: i64, segments: Vec<DmlSeg> },
}

/// One `a:path` command. Named for the DrawingML elements, which map 1:1 onto
/// Typst's `curve.move`/`curve.line`/`curve.cubic`/`curve.close`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DmlSeg {
    MoveTo(i64, i64),
    LineTo(i64, i64),
    /// `a:cubicBezTo`: two control points then the end point.
    CubicTo(i64, i64, i64, i64, i64, i64),
    Close,
}

/// A shape's fill. The `Unstated` / `None` distinction matters: Word's own
/// default for an absent fill element is the theme's, while `a:noFill` is an
/// explicit "draw nothing" that must suppress Typst's own default.
#[derive(Debug, Default, Clone)]
pub enum DmlFill {
    #[default]
    Unstated,
    /// `a:noFill`.
    None,
    /// `a:solidFill/a:srgbClr`, with `a:alpha` folded into the fourth channel.
    Solid([u8; 4]),
    Gradient(DmlGradient),
}

/// An `a:gradFill`.
#[derive(Debug, Clone)]
pub struct DmlGradient {
    /// `a:gsLst`: `(position in 1000ths of a percent, RGBA)`, in file order.
    pub stops: Vec<(i64, [u8; 4])>,
    pub kind: DmlGradientKind,
}

#[derive(Debug, Copy, Clone)]
pub enum DmlGradientKind {
    /// `a:lin/@ang`, in 60000ths of a degree clockwise from "to the right".
    Linear { angle_60k: i64 },
    /// `a:path path="circle"`'s `a:fillToRect`, each side in 1000ths of a
    /// percent — the inset of the gradient's *focal* rectangle.
    Radial { fill_to_rect: [i64; 4] },
}

/// An `a:ln`.
#[derive(Debug, Default, Clone)]
pub struct DmlStroke {
    /// `@w` in EMU. `None` leaves the thickness to Typst's default.
    pub w_emu: Option<i64>,
    /// `a:solidFill/a:srgbClr` with alpha folded in, when the line states one.
    pub color: Option<[u8; 4]>,
    /// `a:ln/a:noFill` — the line is explicitly invisible.
    pub no_fill: bool,
    pub dash: Option<DmlDash>,
}

#[derive(Debug, Clone)]
pub enum DmlDash {
    /// `a:prstDash/@val`.
    Preset(EcoString),
    /// `a:custDash`'s `a:ds` runs: `(dash, space)` pairs, each in 1000ths of a
    /// percent **of the line's own width** (100000 = one line width).
    Custom(Vec<(i64, i64)>),
}

// --- VML shapes (`v:rect`/`v:oval`/`v:roundrect`/`v:line`, in a `w:pict`) ----

/// A VML `v:rect`/`v:oval`/`v:roundrect`/`v:line`, parsed raw: `style` is
/// kept as the whole CSS-ish attribute string (`crate::wml::parse::
/// vml_length_pt` pulls `width`/`height` out of it), and the fill/stroke
/// colors are kept as whatever VML wrote (`#rrggbb` or a color name) —
/// `crate::mappers::shape` resolves both, mirroring how [`RunProps::color`]
/// stores a raw hex string for `crate::lower::parse_hex_color` to resolve
/// later rather than doing it here.
#[derive(Debug, Clone)]
pub struct VmlShape {
    pub kind: VmlShapeKind,
    /// `style="width:120pt;height:80pt;..."`, verbatim.
    pub style: EcoString,
    /// `fillcolor`, or a `v:fill` child's `color` — VML's two spellings for
    /// the same thing.
    pub fill_color: Option<EcoString>,
    /// `filled="f"` → `false`; VML's own default (the attribute absent, or
    /// any value other than `"f"`) is `true`.
    pub filled: bool,
    /// `strokecolor`, or a `v:stroke` child's `color`.
    pub stroke_color: Option<EcoString>,
    /// `stroked="f"` → `false`; default `true`, mirroring `filled`.
    pub stroked: bool,
    /// [`VmlShapeKind::Line`]'s endpoints, each a raw `"x,y"` pair — absent
    /// for every other kind.
    pub from: Option<EcoString>,
    pub to: Option<EcoString>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum VmlShapeKind {
    Rect,
    Oval,
    RoundRect,
    Line,
}

// --- Paragraph properties (`w:pPr`) -----------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ParaProps {
    /// `w:pStyle` — the paragraph style id.
    pub style_id: Option<EcoString>,
    /// `w:jc`.
    pub jc: Option<EcoString>,
    /// `w:numPr` → (numId, ilvl).
    pub num: Option<NumRef>,
    /// `w:spacing/@w:before` in twips.
    pub spacing_before: Option<i64>,
    /// `w:spacing/@w:after` in twips.
    pub spacing_after: Option<i64>,
    /// `w:spacing/@w:line` in twips.
    pub line: Option<i64>,
    /// `w:ind/@w:left` (or `@w:start`) in twips.
    pub indent_left: Option<i64>,
    /// `w:ind/@w:right` (or `@w:end`) in twips.
    pub indent_right: Option<i64>,
    /// `w:ind/@w:firstLine` in twips — an extra indent on the first line only.
    /// Mutually exclusive with [`Self::indent_hanging`] in practice: Word
    /// writes one or the other, never both.
    pub indent_first_line: Option<i64>,
    /// `w:ind/@w:hanging` in twips — the first line pulled *back* out of the
    /// left indent.
    pub indent_hanging: Option<i64>,
    /// `w:shd/@w:fill` — the paragraph's background shading, hex `RRGGBB` (or
    /// "auto"). The cell-level spelling of the same element is
    /// [`Cell::shd_fill`].
    pub shd_fill: Option<EcoString>,
    /// Run properties on the paragraph mark (`w:pPr/w:rPr`) — the default for
    /// bare runs and empty paragraphs.
    pub mark_props: RunProps,
    /// `w:pBdr` — the paragraph's own four border sides. Shares [`Borders`]
    /// with a cell's `w:tcBorders`: OOXML states both the same way, and both
    /// lower to the same Typst stroke.
    pub borders: Borders,
    /// `w:keepLines` — every line of the paragraph stays on one page.
    pub keep_lines: Toggle,
    /// `w:keepNext` — the paragraph stays on the page of the one *after* it.
    /// Kept only so [`crate::mappers::para`] can report the loss: Typst has no
    /// property that binds a block to its successor, and the heading idiom it
    /// exists for ("don't strand a heading at the page foot") is something
    /// Typst's layout already handles on its own.
    pub keep_next: Toggle,
    /// `w:pPr/w:sectPr` — present only when this paragraph is the *last* item
    /// of a section (see [`Section`]); describes the section it closes. Direct,
    /// per-instance data: a style's own `pPr` is never a document section
    /// boundary, so this is never resolved through the `basedOn`/style chain
    /// (see `resolve::styles::merge_para`) — only [`crate::wml::parse::
    /// parse_document_body`] ever reads it, straight off the raw paragraph.
    pub sect_pr: Option<SectPr>,
}

#[derive(Debug, Copy, Clone)]
pub struct NumRef {
    pub num_id: i64,
    pub ilvl: i64,
}

// --- Run properties (`w:rPr`) -----------------------------------------------

/// Tri-state OOXML toggle: absent, or explicitly on/off (`w:val="0"`).
pub type Toggle = Option<bool>;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RunProps {
    /// `w:rStyle` — the character style id.
    pub style_id: Option<EcoString>,
    pub bold: Toggle,
    pub italic: Toggle,
    pub strike: Toggle,
    /// `w:dstrike` — a double strikethrough. Typst has only one strike, so
    /// this lowers to the same `#strike` with the doubling reported as an
    /// approximation rather than being dropped outright.
    pub dstrike: Toggle,
    pub smallcaps: Toggle,
    /// `w:caps` — all-capitals *display* (the stored text is unchanged, which
    /// is why this is a run property rather than a rewrite of the text).
    pub caps: Toggle,
    /// `w:u/@w:val` (e.g. "single", "none").
    pub underline: Option<EcoString>,
    /// `w:u/@w:color` — hex `RRGGBB` (or "auto"), the underline's own color,
    /// which Word tracks independently of the text color.
    pub underline_color: Option<EcoString>,
    /// `w:highlight/@w:val` — one of Word's sixteen *named* marker colors
    /// (never a hex value; arbitrary backgrounds are `w:shd` instead). Kept as
    /// the raw name for [`crate::mappers::run`] to resolve, mirroring how
    /// [`Self::color`] keeps a raw hex string.
    pub highlight: Option<EcoString>,
    /// `w:color/@w:val` — hex `RRGGBB` (or "auto").
    pub color: Option<EcoString>,
    /// `w:sz/@w:val` in half-points.
    pub size_half_pt: Option<i64>,
    /// `w:spacing/@w:val` on a *run* — inter-character tracking in twips
    /// (signed; negative tightens). Not to be confused with
    /// [`ParaProps::spacing_before`], which is the same element name on a
    /// paragraph meaning something entirely different.
    pub letter_spacing: Option<i64>,
    /// `w:rFonts/@w:ascii`.
    pub font: Option<EcoString>,
    /// `w:lang/@w:val` — a BCP-47-ish tag ("en-US"), split into Typst's
    /// separate `lang`/`region` arguments at lower time.
    pub lang: Option<EcoString>,
    /// `w:vertAlign/@w:val` ("superscript"/"subscript").
    pub vert_align: Option<EcoString>,
    /// `w:rtl` — right-to-left run direction.
    pub rtl: Toggle,
    /// `w:vanish` — hidden text.
    pub vanish: Toggle,
}

// --- Tables (`w:tbl`) -------------------------------------------------------

#[derive(Debug, Default)]
pub struct Table {
    /// `w:tblGrid` column widths in twips.
    pub grid: Vec<i64>,
    pub rows: Vec<Row>,
    /// `w:tblPr/w:jc` — how the whole table sits between the margins
    /// ("center"/"right"/"end"). A table is a block, not a paragraph, so this
    /// is *not* the same element as [`ParaProps::jc`] and never reaches a
    /// paragraph's alignment.
    pub jc: Option<EcoString>,
    /// `w:tblPr/w:tblInd/@w:w` in twips, but only when `@w:type="dxa"` — the
    /// other types (`pct`, `auto`, `nil`) measure against a base this
    /// importer has no way to resolve, so they're left unread rather than
    /// misread as an absolute length.
    pub indent_twips: Option<i64>,
    /// `w:tblPr/w:tblBorders` — the table's blanket borders, which a cell's
    /// own `w:tcBorders` overrides where it states one. Reading this is what
    /// lets a *borderless* Word table come across as borderless: Typst's table
    /// draws a 1pt grid by default, so an unread `w:tblBorders` stating `nil`
    /// silently added lines Word never drew.
    pub borders: TableBorders,
}

#[derive(Debug, Default)]
pub struct Row {
    pub is_header: bool,
    pub cells: Vec<Cell>,
    /// `w:trPr/w:trHeight` — the row's authored height in twips, and whether
    /// `@w:hRule` made it an exact height rather than a minimum. Only an
    /// exact one reaches Typst; see [`crate::mappers::table`].
    pub height_twips: Option<i64>,
    pub height_exact: bool,
    /// `w:trPr/w:cantSplit` — the row may not break across pages.
    pub cant_split: bool,
}

#[derive(Debug, Default)]
pub struct Cell {
    /// `w:gridSpan`.
    pub grid_span: usize,
    /// `w:vMerge`: `Some(true)` = restart, `Some(false)` = continue.
    pub v_merge: Option<bool>,
    /// `w:shd/@w:fill` hex.
    pub shd_fill: Option<EcoString>,
    /// `w:tcBorders` — the cell's own border overrides.
    pub borders: Borders,
    /// `w:vAlign/@w:val` ("top"/"center"/"bottom").
    pub v_align: Option<EcoString>,
    /// `w:tcMar` — the cell's inner margins.
    pub margins: CellMargins,
    pub content: Vec<BodyItem>,
}

/// Four border sides, kept raw (`w:sz` in eighths of a point, colors as hex
/// strings) for the mappers to resolve, the same contract [`RunProps`] follows.
///
/// One type for both spellings OOXML gives this: a cell's `w:tcBorders` and a
/// paragraph's `w:pBdr` are structurally identical and both lower to the same
/// Typst stroke, so they share a parser ([`crate::wml::parse`]) and a resolver.
///
/// A side left `None` means Word said nothing about it, which is *not* the
/// same as Word explicitly switching it off — see [`BorderEdge::is_none`].
#[derive(Debug, Default, Clone)]
pub struct Borders {
    pub top: Option<BorderEdge>,
    pub bottom: Option<BorderEdge>,
    pub left: Option<BorderEdge>,
    pub right: Option<BorderEdge>,
}

impl Borders {
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
    }

    /// Whether the *only* side stated is a bottom rule — Word's "section-title
    /// underline" idiom, which lowers to a `#line` rather than a bordered
    /// block (see [`crate::mappers::para`]).
    pub fn is_bottom_only(&self) -> bool {
        self.bottom.is_some()
            && self.top.is_none()
            && self.left.is_none()
            && self.right.is_none()
    }
}

/// `w:tblPr/w:tblBorders` — a table's blanket borders. The four outer sides
/// plus the two *interior* ones, which have no cell-level counterpart and no
/// direct Typst equivalent either (Typst's `table(stroke:)` applies one stroke
/// to every cell edge); [`crate::mappers::table`] is where that reconciliation
/// happens.
#[derive(Debug, Default, Clone)]
pub struct TableBorders {
    pub outer: Borders,
    pub inside_h: Option<BorderEdge>,
    pub inside_v: Option<BorderEdge>,
}

/// One side of a [`Borders`].
#[derive(Debug, Clone)]
pub struct BorderEdge {
    /// `@w:val` — "single", "double", …, or "nil"/"none" for no border at all.
    pub val: EcoString,
    /// `@w:sz` in eighths of a point.
    pub sz_eighth_pt: Option<i64>,
    /// `@w:color` — hex `RRGGBB` (or "auto").
    pub color: Option<EcoString>,
    /// `@w:space` — the gap between the border and the text, in *points*
    /// (not twips, and not eighths: this one attribute is whole points).
    /// Only meaningful on a paragraph border, where it becomes the bordered
    /// block's inset; a cell's padding is `w:tcMar` instead.
    pub space_pt: Option<i64>,
}

impl BorderEdge {
    /// Whether this side explicitly draws *no* border.
    pub fn is_none(&self) -> bool {
        self.val == "nil" || self.val == "none"
    }
}

/// `w:tcMar` — a cell's four inner margins, in twips.
#[derive(Debug, Default, Clone, Copy)]
pub struct CellMargins {
    pub top: Option<i64>,
    pub bottom: Option<i64>,
    pub left: Option<i64>,
    pub right: Option<i64>,
}

impl CellMargins {
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.bottom.is_none()
            && self.left.is_none()
            && self.right.is_none()
    }
}

// --- Charts (`word/charts/*.xml`) -------------------------------------------

/// A chart's cached data — the numbers and labels Word last plotted. Enough
/// to rebuild the chart as a table, which is what the importer does (Typst
/// has no chart-drawing primitive, and inventing one is out of scope).
///
/// This shape is classic-chart-first (`c:chartSpace`'s `c:ser`/`c:cat`/
/// `c:val`, one value per category per series): a plain category axis shared
/// by every series, each series a column. The newer ChartEx format
/// (`cx:chartSpace`, `word/charts/chartEx*.xml`, used for chart types
/// introduced after Office 2013 — box-and-whisker, sunburst, waterfall, …)
/// stores its data differently — a flat `cx:data` block per series-ish
/// grouping, with categories and values aligned by shared point index rather
/// than nested inside the series itself — but maps onto the same
/// `categories`/`series` shape well enough for the chart types this importer
/// has actually seen in the wild (a box-and-whisker chart's raw, unaggregated
/// data table *is* one row per point with a repeated category label, which
/// is exactly what this struct already represents). A chart type whose
/// category axis is genuinely hierarchical (e.g. a sunburst's nested
/// leaf/stem/branch levels) only keeps its finest (first) level here — the
/// coarser levels are a real, but comparatively minor, loss on top of the
/// larger one (the plot itself) this whole construct already accepts.
#[derive(Debug, Default, Clone)]
pub struct ChartData {
    pub title: Option<EcoString>,
    /// Category labels (the shared x-axis), if the chart declares any.
    pub categories: Vec<EcoString>,
    pub series: Vec<ChartSeries>,
    /// What kind of chart this is — see [`ChartKind`]. Drives whether
    /// [`crate::mappers::chart`] can draw it as a plot under
    /// [`crate::opts::ChartStyle::Plot`]; irrelevant to the table fallback,
    /// which works for any kind.
    pub kind: ChartKind,
    /// `c:legend/c:legendPos` — where Word placed the legend, or `None` when
    /// the chart declares no legend at all (in which case it shows none).
    pub legend: Option<LegendPos>,
}

/// A chart's plot type, as far as it maps onto something Typst's `lilaq`
/// package can draw. Only the classic-chart shape ([`ChartData`]'s doc
/// comment) carries a kind other than [`Self::Other`] — ChartEx charts
/// (box-and-whisker, sunburst, waterfall, …) have no `lilaq` counterpart
/// either, so they stay `Other` rather than being guessed at.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum ChartKind {
    Bar,
    Line,
    Scatter,
    Area,
    /// A type with no plotting counterpart (pie, radar, stock, surface, …) —
    /// these always fall back to the data table.
    #[default]
    Other,
}

/// `c:legendPos` — the edge Word put the chart legend on.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum LegendPos {
    Top,
    Bottom,
    Left,
    Right,
    TopRight,
}

#[derive(Debug, Default, Clone)]
pub struct ChartSeries {
    pub name: Option<EcoString>,
    /// Values, positionally aligned with `categories` where both exist.
    pub values: Vec<EcoString>,
}

// --- Sections (`w:sectPr`) --------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct SectPr {
    /// `w:pgSz` width/height in twips.
    pub page_w: Option<i64>,
    pub page_h: Option<i64>,
    pub landscape: bool,
    /// `w:cols/@w:num` — the section's text-column count. A two-column
    /// call-for-papers layout is a hard requirement of the template that
    /// imposes it, not a cosmetic detail.
    pub columns: Option<u32>,
    /// `w:pgMar` in twips.
    pub margin_top: Option<i64>,
    pub margin_bottom: Option<i64>,
    pub margin_left: Option<i64>,
    pub margin_right: Option<i64>,
    /// `w:pgMar/@w:header` and `@w:footer` in twips — the distance from the
    /// page edge to where the header/footer *starts*, which is a different
    /// origin from Typst's `header-ascent`/`footer-descent` (the gap on the
    /// body side of the same band). [`crate::mappers::section`] converts.
    pub header_dist: Option<i64>,
    pub footer_dist: Option<i64>,
    /// `w:pgMar/@w:gutter` in twips — extra binding allowance added to the
    /// inner margin. Typst has no `gutter` of its own; it folds into the
    /// margin on the binding side.
    pub gutter: Option<i64>,
    /// `w:headerReference` / `w:footerReference`, in document order.
    pub header_refs: Vec<FurnitureRef>,
    pub footer_refs: Vec<FurnitureRef>,
    /// `w:titlePg` — the first page takes its own header/footer.
    pub title_pg: bool,
    /// `w:type` — how this section starts. Absent means `nextPage` (Word's own
    /// default when the element is missing).
    pub start: SectionStart,
    /// `w:pgNumType/@w:fmt` — the page-number format, if the section sets one.
    /// Absent means "inherit whatever the previous section had" (Word never
    /// resets the format just because a section doesn't restate it).
    pub page_num_fmt: Option<EcoString>,
    /// `w:pgNumType/@w:start` — the number this section restarts at. Absent
    /// means no restart here (the counter just keeps incrementing).
    pub page_num_start: Option<i64>,
}

/// `w:sectPr/w:type/@w:val` — how a section starts relative to the one before
/// it. Mirrored (not duplicated) in the Typst IR as `tdoc::SectionStart`
/// (re-exported from here) since both sides mean exactly the same thing.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum SectionStart {
    /// Starts on a new page. Word's own default when `w:type` is absent.
    #[default]
    NextPage,
    /// Stays on the *same* page as the section before it — a column-count or
    /// page-numbering change with no visible page break. The one start type
    /// Typst can sometimes honor without a `#pagebreak()` at all (see
    /// `emit::render_section`).
    Continuous,
    /// Starts on the next even-numbered page.
    EvenPage,
    /// Starts on the next odd-numbered page.
    OddPage,
    /// Starts in the next column of a multi-column layout. Real documents use
    /// this vanishingly rarely; Typst has no "next column, possibly also next
    /// page" primitive, so it's treated the same as `NextPage`.
    NextColumn,
}

/// A `w:headerReference`/`w:footerReference`: which page class it applies to,
/// and the relationship pointing at the `w:hdr`/`w:ftr` part.
#[derive(Debug, Clone)]
pub struct FurnitureRef {
    pub kind: FurnitureKind,
    pub rel_id: EcoString,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FurnitureKind {
    Default,
    First,
    Even,
}

// --- Styles (`styles.xml`) --------------------------------------------------

#[derive(Debug, Default)]
pub struct Styles {
    /// docDefaults run/paragraph properties.
    pub default_run: RunProps,
    pub default_para: ParaProps,
    /// Style id → definition.
    pub by_id: FxHashMap<EcoString, Style>,
}

#[derive(Debug, Default, Clone)]
pub struct Style {
    pub id: EcoString,
    pub name: Option<EcoString>,
    pub kind: StyleKind,
    pub based_on: Option<EcoString>,
    /// The heading outline level (`w:pPr/w:outlineLvl`, 0-based), if any.
    pub outline_level: Option<u8>,
    pub run: RunProps,
    pub para: ParaProps,
}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub enum StyleKind {
    #[default]
    Paragraph,
    Character,
    Table,
    Numbering,
}

// --- Numbering (`numbering.xml`) --------------------------------------------

#[derive(Debug, Default)]
pub struct Numbering {
    /// `numId` → `abstractNumId`.
    pub instances: FxHashMap<i64, i64>,
    /// `abstractNumId` → per-level format.
    pub abstract_nums: FxHashMap<i64, FxHashMap<i64, LevelFormat>>,
    /// `(numId, ilvl)` → `w:lvlOverride/w:startOverride`. Kept separate from
    /// [`Self::abstract_nums`] because an override belongs to the *instance*:
    /// several `w:num`s routinely share one `abstractNum` and restart at
    /// different numbers, so folding the override into the shared definition
    /// would leak one list's starting number into every sibling list.
    pub start_overrides: FxHashMap<(i64, i64), i64>,
}

#[derive(Debug, Clone)]
pub struct LevelFormat {
    /// `w:numFmt/@w:val` ("bullet", "decimal", …).
    pub num_fmt: EcoString,
    /// `w:start/@w:val` — the number this level counts from.
    pub start: Option<i64>,
    /// `w:lvlText/@w:val` — the literal text Word prints in front of an item.
    /// For a *numbered* level this is a template with `%1`-style placeholders
    /// (already covered by the `enum(numbering:)` pattern `lower` builds from
    /// `num_fmt`, so it goes unread there); for a **bullet** level it is the
    /// authored marker glyph itself, which is the only place that glyph is
    /// recorded. See [`Numbering::bullet_marker`].
    pub lvl_text: Option<EcoString>,
}

impl Numbering {
    /// The resolved level definition behind a `(numId, ilvl)` reference.
    pub fn level(&self, num_id: i64, ilvl: i64) -> Option<&LevelFormat> {
        self.instances
            .get(&num_id)
            .and_then(|abs| self.abstract_nums.get(abs))
            .and_then(|levels| levels.get(&ilvl))
    }

    /// Whether a `(numId, ilvl)` reference is an ordered (numbered) list.
    pub fn is_ordered(&self, num_id: i64, ilvl: i64) -> bool {
        self.level(num_id, ilvl)
            .is_some_and(|fmt| fmt.num_fmt != "bullet" && fmt.num_fmt != "none")
    }

    /// The marker glyph an *unordered* `(numId, ilvl)` level prints, when it
    /// states a usable one.
    ///
    /// `None` for a numbered level (whose `w:lvlText` is a `%1`-style template,
    /// not a glyph), for a level that states no `w:lvlText` at all, and — the
    /// case that matters most in practice — for Word's **private-use**
    /// placeholders. Word writes a Symbol/Wingdings bullet as a codepoint in
    /// the Unicode private-use area (`U+F0B7` for the classic Symbol dot),
    /// which only means anything alongside the `w:rFonts` that level also
    /// carries; emitted as a literal Typst marker it renders as tofu, so it is
    /// deliberately refused here and the caller falls back to the default
    /// bullet (recording the loss — see `lower::PendingList::finish`).
    pub fn bullet_marker(&self, num_id: i64, ilvl: i64) -> Option<&EcoString> {
        let level = self.level(num_id, ilvl)?;
        if level.num_fmt != "bullet" {
            return None;
        }
        let text = level.lvl_text.as_ref()?;
        let usable = !text.is_empty()
            && !text.chars().any(|c| {
                // The BMP private-use area plus the two supplementary planes
                // reserved for it — nothing outside a private agreement
                // between producer and font can be rendered from any of them.
                matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10fffd}')
            });
        usable.then_some(text)
    }

    /// The number a `(numId, ilvl)` reference counts from: the instance's own
    /// `w:startOverride` if it has one, else the shared definition's `w:start`.
    pub fn start(&self, num_id: i64, ilvl: i64) -> Option<i64> {
        self.start_overrides
            .get(&(num_id, ilvl))
            .copied()
            .or_else(|| self.level(num_id, ilvl).and_then(|level| level.start))
    }
}

//! Lower the Word IR ([`crate::wml`]) to the Typst IR ([`crate::tdoc`]).
//! The mirror of the exporter's `convert.rs` + `mappers/`.

use ecow::{eco_format, EcoString};
use typst_ooxml_core::units::half_point_to_pt;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::mappers;
use crate::mappers::para::{ParaKind, ParaResult};
use crate::opts::ImportOptions;
use crate::report::ImportReport;
use crate::resolve::styles::{effective_para, heading_level};
use crate::tdoc::{
    self, Block, Date, DocumentInfo, Inline, List, ListItem, ParStyle, Stmt, TextStyle, TypstDoc,
};
use crate::wml::model::{BodyItem, DocumentMeta, Numbering, RunProps, SectionStart, WmlPackage};

/// Everything the lowering phase threads through: the package being read, the
/// options that govern how it's lowered, the loss report, and the guards
/// that stop a malformed document from recursing forever.
pub(crate) struct LowerCtx<'a> {
    pub package: &'a WmlPackage,
    pub options: &'a ImportOptions,
    pub report: &'a mut ImportReport,
    /// `(is_endnote, id)` pairs currently being lowered — the cycle guard for
    /// note resolution. A malformed document can have note 1 reference note 1,
    /// directly or through a chain, which would otherwise recurse forever.
    note_stack: Vec<(bool, i64)>,
    /// Whether the content currently being lowered sits inside a table cell,
    /// text box, footnote/endnote, or header/footer — anywhere Typst forbids
    /// `#pagebreak()`/`#colbreak()` ("pagebreaks are not allowed inside of
    /// containers"). Set around each such body by [`Self::enter_container`]/
    /// [`Self::exit_container`]; [`crate::mappers::para::lower_paragraph`]
    /// reads it to drop a page/column break instead of emitting one.
    in_container: bool,
    /// Whether the content currently being lowered is the direct inline
    /// content of a heading paragraph. Set by [`Self::enter_heading`]/
    /// [`Self::exit_heading`] around exactly that scope (so it still reads
    /// `true` for, say, a hyperlink or a field's cached-result text nested
    /// inside the heading, which recurses back through the same run-lowering
    /// path); [`crate::mappers::field::lower_field`]'s `TOC` arm reads it to
    /// avoid emitting a live `#outline()` where it would recurse into itself
    /// (`#outline()` renders every heading, including the one that contains
    /// it — "maximum show rule depth exceeded").
    in_heading: bool,
    /// How many revision anchors have been emitted, so each gets a unique
    /// label. Word's own `w:id`s are per-revision and not reliably unique
    /// across a document's parts, so they aren't reused.
    pub(crate) revision_counter: usize,
    /// Labels of insertions whose closing anchor hasn't been emitted yet — a
    /// stack, because Word nests revisions (content inserted and then deleted
    /// again writes a `w:del` inside a `w:ins`).
    pub(crate) open_revisions: Vec<EcoString>,
    /// Endnote bodies in first-reference order, emitted together at the
    /// document's end — Word collects endnotes there, and Typst has no note
    /// store to route them through. See `mappers::note`.
    endnotes: Vec<Vec<Block>>,
    /// `endnote id` → the number already assigned to it, so a second
    /// reference to the same endnote reuses its number instead of collecting
    /// the body twice.
    endnote_numbers: FxHashMap<i64, usize>,
    /// Labels already emitted. A Typst label must be unique in a document,
    /// and real producers (LibreOffice in particular) do write the same
    /// `w:bookmarkStart` name twice — emitting both makes every reference to
    /// it ambiguous and fails the compile.
    emitted_labels: FxHashSet<EcoString>,
}

/// How deep a chain of notes referencing other notes may nest before
/// [`LowerCtx::enter_note`] refuses to go further — the note-lowering
/// counterpart of `MAX_TABLE_DEPTH`/`MAX_SDT_DEPTH` in `wml::parse`. Real
/// documents essentially never reference a note from within another note at
/// all; this only bites a pathological or hostile document, and backs up the
/// cycle check for a chain long enough to still not repeat any single id.
const MAX_NOTE_DEPTH: usize = 8;

impl<'a> LowerCtx<'a> {
    pub(crate) fn new(
        package: &'a WmlPackage,
        options: &'a ImportOptions,
        report: &'a mut ImportReport,
    ) -> Self {
        LowerCtx {
            package,
            options,
            report,
            note_stack: Vec::new(),
            revision_counter: 0,
            open_revisions: Vec::new(),
            in_container: false,
            in_heading: false,
            endnotes: Vec::new(),
            endnote_numbers: FxHashMap::default(),
            emitted_labels: FxHashSet::default(),
        }
    }

    /// Claim `label` for emission, or `false` if it has already been emitted
    /// elsewhere in the document — see [`Self::emitted_labels`].
    pub(crate) fn claim_label(&mut self, label: &EcoString) -> bool {
        self.emitted_labels.insert(label.clone())
    }

    /// The number already assigned to endnote `id`, if it has been referenced
    /// before. Checked *before* lowering so a repeat reference doesn't collect
    /// a second copy of the same body.
    pub(crate) fn endnote_number(&self, id: i64) -> Option<usize> {
        self.endnote_numbers.get(&id).copied()
    }

    /// Collect `body` as the next endnote and return its number.
    pub(crate) fn collect_endnote(&mut self, id: i64, body: Vec<Block>) -> usize {
        self.endnotes.push(body);
        let number = self.endnotes.len();
        self.endnote_numbers.insert(id, number);
        number
    }

    /// Take the collected endnotes, leaving the context empty.
    pub(crate) fn take_endnotes(&mut self) -> Vec<Vec<Block>> {
        std::mem::take(&mut self.endnotes)
    }

    /// Try to enter `(endnote, id)`'s body for lowering. Returns `false` —
    /// without recording anything itself, so the caller can report the
    /// specific construct ("footnote" vs "endnote") — if `id` is already on
    /// the stack (a direct or indirect cycle) or the stack is already at
    /// [`MAX_NOTE_DEPTH`]. Every successful `true` must be paired with a
    /// matching [`Self::exit_note`] once that note's body is fully lowered.
    ///
    /// A `Vec` doubles as the depth counter (its length is the current
    /// nesting depth) and, at the sizes a note chain can reach, a linear
    /// `contains` scan to check for a repeat is cheap — no need for a
    /// `HashSet` here.
    pub(crate) fn enter_note(&mut self, endnote: bool, id: i64) -> bool {
        if self.note_stack.len() >= MAX_NOTE_DEPTH || self.note_stack.contains(&(endnote, id)) {
            return false;
        }
        self.note_stack.push((endnote, id));
        true
    }

    /// Leave the note most recently entered via [`Self::enter_note`].
    pub(crate) fn exit_note(&mut self) {
        self.note_stack.pop();
    }

    /// Enter a container body (a table cell, a text box, a footnote/endnote,
    /// or a header/footer) for the duration of lowering it. Returns the
    /// previous value of [`Self::in_container`], which the caller must pass
    /// back to [`Self::exit_container`] once the body is fully lowered — a
    /// save/restore rather than a depth counter, since nesting only ever
    /// needs a yes/no answer: a break three containers deep is exactly as
    /// illegal as one, and unwinding to whatever the flag already was
    /// (rather than hard-resetting to `false`) is what keeps that nested
    /// case correct.
    pub(crate) fn enter_container(&mut self) -> bool {
        std::mem::replace(&mut self.in_container, true)
    }

    /// Leave the container most recently entered via [`Self::enter_container`].
    pub(crate) fn exit_container(&mut self, was_in_container: bool) {
        self.in_container = was_in_container;
    }

    /// Whether the content currently being lowered is inside a container —
    /// see [`Self::in_container`]'s doc comment.
    pub(crate) fn in_container(&self) -> bool {
        self.in_container
    }

    /// Enter a heading paragraph's own direct inline content. Same
    /// save/restore shape as [`Self::enter_container`], and for the same
    /// nesting reason: a field or hyperlink inside the heading that
    /// recurses back through [`crate::mappers::run::lower_run_items`] (a
    /// field's cached result can itself contain a nested field) must still
    /// see `true`.
    pub(crate) fn enter_heading(&mut self) -> bool {
        std::mem::replace(&mut self.in_heading, true)
    }

    /// Leave the heading most recently entered via [`Self::enter_heading`].
    pub(crate) fn exit_heading(&mut self, was_in_heading: bool) {
        self.in_heading = was_in_heading;
    }

    /// Whether the content currently being lowered is a heading's own direct
    /// inline content — see [`Self::in_heading`]'s doc comment.
    pub(crate) fn in_heading(&self) -> bool {
        self.in_heading
    }
}

pub(crate) fn lower(ctx: &mut LowerCtx) -> TypstDoc {
    let mut doc = TypstDoc::default();

    // Metadata leads the preamble: it identifies the document rather than
    // styling it. A package with no core properties pushes nothing, so the
    // preamble every existing fixture expects is unchanged.
    let info = document_info(&ctx.package.meta, &mut *ctx.report);
    if !info.is_empty() {
        doc.preamble.push(Stmt::SetDocument(info));
    }

    // The document's first section keeps the exact behaviour a single-section
    // document has always had: its page geometry (plus header/footer) goes to
    // the preamble as a `#set page(..)`, and its content is lowered flat into
    // `doc.body` — no `Block::Section` wrapper, no synthesized page break.
    // This is what makes a single-section import byte-identical to before.
    let Some((first, rest)) = ctx.package.body.sections.split_first() else {
        // `Body::sections` is only ever empty for a hand-built `Body::default()`
        // (real parses always produce at least one) — nothing to lower.
        return doc;
    };
    let first_setup = mappers::section::lower_section(&first.props, ctx);
    doc.preamble.push(Stmt::SetPage(first_setup.clone()));

    // A document default `#set text(..)` so bare runs inherit the doc's base
    // font/size — after the page setup, matching the order every existing
    // test and fixture already expects.
    if let Some(style) = default_text_style(&ctx.package.styles.default_run) {
        doc.preamble.push(Stmt::SetText(style));
    }

    if let Some(numbering) = heading_numbering(ctx.package) {
        doc.preamble
            .push(Stmt::Verbatim(eco_format!("#set heading(numbering: \"{numbering}\")")));
    }

    doc.body = lower_items(&first.items, ctx);

    // Every section after the first becomes one `Block::Section`, resolving
    // its own page setup and header/footer independently (the existing
    // `lower_furniture` machinery already takes a `SectPr`; this just calls it
    // once per section instead of once for the whole document).
    let mut previous = first_setup;
    for section in rest {
        let setup = mappers::section::lower_section(&section.props, ctx);
        let start = section.props.start;

        // A `continuous` section is only safe to render without a page break
        // when the *only* thing it changes is the column count (see
        // `PageSetup::matches_except_columns`) — Typst has no way to change
        // page size, margins, header/footer, or page numbering without
        // starting a new page. When something else changed too, `emit`
        // still has to break there (see `emit::render_section`), so the
        // approximation is recorded here, at lower time, where `ctx.report`
        // is reachable.
        if start == SectionStart::Continuous && !setup.matches_except_columns(&previous) {
            ctx.report.approximate(
                "continuous section",
                "Word kept this section on the same page, but its page setup changed in a \
                 way Typst can only apply starting a new page",
            );
        }

        let body = lower_items(&section.items, ctx);
        doc.body.push(Block::Section(tdoc::Section { setup: setup.clone(), start, body }));
        previous = setup;
    }

    // Word renders endnotes together at the document's end, after a
    // separator. Typst has no note store, so they are emitted here as
    // ordinary content in first-reference order — the closest faithful
    // equivalent, and much closer than scattering them across page feet.
    let endnotes = ctx.take_endnotes();
    if !endnotes.is_empty() {
        doc.body.push(Block::Rule);
        for (index, body) in endnotes.into_iter().enumerate() {
            doc.body.extend(numbered_endnote(index + 1, body));
        }
    }

    doc
}

/// Prefix a collected endnote's body with its number so the entries at the
/// document's end line up with the marks left in the text.
fn numbered_endnote(number: usize, mut body: Vec<Block>) -> Vec<Block> {
    let marker = Inline::Text(eco_format!("{number}. "));
    match body.first_mut() {
        // Fold the number into the note's own opening paragraph, so it reads
        // as one block rather than a stray number on a line of its own.
        Some(Block::Paragraph { body: inlines, .. }) => inlines.insert(0, marker),
        _ => body.insert(0, Block::Paragraph { style: ParStyle::default(), body: vec![marker] }),
    }
    body
}

/// Lower a sequence of body items (the document body, or a table cell's
/// content) to blocks, accumulating consecutive list-item paragraphs into a
/// single [`Block::List`] rather than emitting one list per item.
pub(crate) fn lower_items(items: &[BodyItem], ctx: &mut LowerCtx) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut pending_list: Option<PendingList> = None;

    for item in items {
        match item {
            BodyItem::Paragraph(p) => {
                let ParaResult { anchored, leading, kind } =
                    mappers::para::lower_paragraph(p, ctx);

                // An anchored figure/chart is a block in its own right and
                // leads the paragraph it hangs off. Typst can't place a block
                // between two items of one list, so an anchored block inside a
                // list item ends the list and starts a new one after it —
                // slightly worse than Word's layout, but it keeps the image.
                // Anchors hoisted out of a heading go in front of it, still
                // in document order and still rendering nothing.
                if !leading.is_empty() {
                    if let Some(pending) = pending_list.take() {
                        blocks.push(
                            pending.finish(&ctx.package.numbering, &mut *ctx.report),
                        );
                    }
                    let style = ParStyle::default();
                    blocks.push(Block::Paragraph { style, body: leading });
                }

                if let Some(block) = anchored {
                    if let Some(pending) = pending_list.take() {
                        blocks.push(pending.finish(&ctx.package.numbering, &mut *ctx.report));
                    }
                    blocks.push(block);
                }

                match kind {
                    ParaKind::ListItem { ordered, level, body, num_id } => {
                        let item = ListItem { ordered, level, body };
                        // A different `w:numId` is a different Word list, so it
                        // starts a new Typst list instead of merging into the
                        // one before it. Merging is also what made the
                        // numbering ambiguous: a Typst list states one format
                        // for the whole run, so a bullet list immediately
                        // followed by a roman one resolved its format from the
                        // bullet — and silently lost the roman numerals.
                        let continues =
                            pending_list.as_ref().is_some_and(|p| p.num_id == num_id);
                        if !continues
                            && let Some(pending) = pending_list.take()
                        {
                            blocks.push(pending.finish(&ctx.package.numbering, &mut *ctx.report));
                        }
                        match pending_list.as_mut() {
                            Some(pending) => pending.push(item),
                            None => pending_list = Some(PendingList::new(item, num_id)),
                        }
                    }
                    other => {
                        if let Some(pending) = pending_list.take() {
                            blocks.push(pending.finish(&ctx.package.numbering, &mut *ctx.report));
                        }
                        push_para_kind(&mut blocks, other);
                    }
                }
            }
            BodyItem::Table(t) => {
                if let Some(pending) = pending_list.take() {
                    blocks.push(pending.finish(&ctx.package.numbering, &mut *ctx.report));
                }
                blocks.push(Block::Table(mappers::table::lower_table(t, ctx)));
            }
        }
    }
    if let Some(pending) = pending_list.take() {
        blocks.push(pending.finish(&ctx.package.numbering, &mut *ctx.report));
    }
    blocks
}

/// A run of consecutive list-item paragraphs being accumulated into one
/// [`Block::List`], together with the Word numbering reference needed to
/// resolve the list's format and starting number once the run ends.
///
/// The `numId` can't be resolved item-by-item: Typst states an enum's
/// numbering once for the whole list, with one counting symbol per nesting
/// depth, so the pattern isn't knowable until every level the list actually
/// uses has been seen.
struct PendingList {
    list: List,
    num_id: Option<i64>,
    deepest: u8,
}

impl PendingList {
    fn new(item: ListItem, num_id: Option<i64>) -> Self {
        let deepest = item.level;
        let list =
            List { items: vec![item], numbering: None, start: None, markers: Vec::new() };
        PendingList { list, num_id, deepest }
    }

    fn push(&mut self, item: ListItem) {
        self.deepest = self.deepest.max(item.level);
        self.list.items.push(item);
    }

    fn finish(self, numbering: &Numbering, report: &mut ImportReport) -> Block {
        let mut list = self.list;
        if let Some(num_id) = self.num_id {
            if list.items.iter().any(|item| item.ordered) {
                list.numbering = enum_numbering(numbering, num_id, self.deepest);
                // Typst already counts from 1, so only a different start is
                // worth stating.
                list.start = numbering.start(num_id, 0).filter(|&start| start != 1);
            }
            if list.items.iter().any(|item| !item.ordered) {
                list.markers = bullet_markers(numbering, num_id, self.deepest, report);
            }
        }
        Block::List(list)
    }
}

/// Typst's own default bullet cycle, in depth order. A Word list whose markers
/// already match these needs no `#set list(marker:)` at all — emitting one
/// would be pure noise in the output.
const DEFAULT_MARKERS: [&str; 3] = ["\u{2022}", "\u{2023}", "\u{2013}"];

/// The authored bullet glyph for each nesting depth a list actually uses.
///
/// Typst cycles `list(marker:)` by depth and Word stores one `w:lvlText` per
/// `w:ilvl`, so the two line up as written — but only up to the deepest level
/// the list *uses*, since a Word `abstractNum` always defines all nine and
/// stating markers for levels no item reaches would be noise.
///
/// Returns an empty vector — leaving Typst's defaults in place — when no level
/// states a usable glyph, or when the glyphs Word states are the ones Typst
/// would have drawn anyway. A level whose glyph is unusable (a Wingdings
/// private-use placeholder; see [`Numbering::bullet_marker`]) falls back to
/// Typst's own marker for that depth and is reported, rather than dragging the
/// levels that *are* expressible down with it.
fn bullet_markers(
    numbering: &Numbering,
    num_id: i64,
    deepest: u8,
    report: &mut ImportReport,
) -> Vec<EcoString> {
    let mut markers = Vec::with_capacity(usize::from(deepest) + 1);
    let mut any_authored = false;
    let mut any_refused = false;

    for ilvl in 0..=i64::from(deepest) {
        let default = DEFAULT_MARKERS[ilvl as usize % DEFAULT_MARKERS.len()];
        match numbering.bullet_marker(num_id, ilvl) {
            Some(glyph) => {
                any_authored = true;
                markers.push(glyph.clone());
            }
            None => {
                // Only a level that *is* a bullet and *did* state a glyph
                // counts as a loss. A numbered level in a mixed list has no
                // marker to lose, and one that states none (or an empty one)
                // never asked for a particular glyph in the first place.
                any_refused |= numbering.level(num_id, ilvl).is_some_and(|level| {
                    level.num_fmt == "bullet"
                        && level.lvl_text.as_ref().is_some_and(|text| !text.is_empty())
                });
                markers.push(default.into());
            }
        }
    }

    if any_refused {
        report.approximate(
            "list bullet",
            "a symbol-font bullet (Wingdings/Symbol) has no portable glyph; \
             the default bullet is used",
        );
    }
    if !any_authored || markers.iter().zip(DEFAULT_MARKERS).all(|(m, d)| m == d) {
        return Vec::new();
    }
    markers
}

/// Typst's counting symbol for one Word `w:numFmt`.
///
/// `None` for a format Typst has no counting symbol for (`ordinal`,
/// `cardinalText`, the CJK and Hebrew sequences, …), which leaves the list on
/// Typst's default numbering rather than inventing a different one.
fn counting_symbol(num_fmt: &str) -> Option<&'static str> {
    match num_fmt {
        "decimal" | "decimalZero" => Some("1"),
        "lowerLetter" => Some("a"),
        "upperLetter" => Some("A"),
        "lowerRoman" => Some("i"),
        "upperRoman" => Some("I"),
        _ => None,
    }
}

/// Build the Typst `enum(numbering:)` pattern for a Word list.
///
/// Typst takes one counting symbol per nesting depth in a single pattern
/// string (`"1.a.i."`) and shows the deepest applicable one; Word stores a
/// format per level. Returns `None` when the list is plain decimal throughout
/// — that is already Typst's default — or when any level uses a format with no
/// Typst counterpart, since a partly-translated pattern would silently
/// renumber the levels it couldn't express.
fn enum_numbering(numbering: &Numbering, num_id: i64, deepest: u8) -> Option<EcoString> {
    let mut symbols = Vec::with_capacity(usize::from(deepest) + 1);
    for ilvl in 0..=i64::from(deepest) {
        symbols.push(counting_symbol(&numbering.level(num_id, ilvl)?.num_fmt)?);
    }
    if symbols.iter().all(|symbol| *symbol == "1") {
        return None;
    }
    Some(format!("{}.", symbols.join(".")).into())
}

fn push_para_kind(blocks: &mut Vec<Block>, kind: ParaKind) {
    match kind {
        ParaKind::Break(kind) => blocks.push(Block::Break(kind)),
        ParaKind::Rule => blocks.push(Block::Rule),
        ParaKind::Heading { level, body } => blocks.push(Block::Heading { level, body }),
        ParaKind::Paragraph { style, body } => blocks.push(Block::Paragraph { style, body }),
        ParaKind::Equation { body } => blocks.push(Block::Equation { body }),
        ParaKind::Empty => {}
        ParaKind::ListItem { .. } => unreachable!("list items are handled by the caller"),
    }
}

/// Lower the package's core properties to a `#set document(..)`.
///
/// Word's single-string fields are split into the shapes Typst wants: authors
/// on `;` (the separator `typst-docx` itself writes on export, so a
/// round-tripped multi-author list comes back intact) and keywords on `,`
/// (Word's own convention). `dc:description` has no `document` counterpart in
/// Typst, so it's reported as a drop rather than vanishing quietly.
fn document_info(meta: &DocumentMeta, report: &mut ImportReport) -> DocumentInfo {
    if meta.description.is_some() {
        report.drop("document description", "Typst's `document` has no description field");
    }
    DocumentInfo {
        title: meta.title.clone(),
        authors: split_metadata_list(meta.creator.as_deref(), ';'),
        keywords: split_metadata_list(meta.keywords.as_deref(), ','),
        date: meta.created.as_deref().and_then(parse_w3cdtf_date),
    }
}

/// Split one of Word's delimited metadata strings into its parts, trimming
/// whitespace and dropping empties.
fn split_metadata_list(value: Option<&str>, separator: char) -> Vec<EcoString> {
    value
        .into_iter()
        .flat_map(|value| value.split(separator))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part: &str| EcoString::from(part))
        .collect()
}

/// Pull the calendar date out of a W3CDTF timestamp ("2026-03-14T09:00:00Z").
/// Only the date survives — see [`Date`] for why the time of day doesn't.
fn parse_w3cdtf_date(value: &str) -> Option<Date> {
    let mut parts = value.split('T').next()?.split('-');
    let year = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    // Typst's `datetime` rejects an out-of-range component outright, which
    // would fail the whole compile over a cosmetic field.
    ((1..=12).contains(&month) && (1..=31).contains(&day))
        .then_some(Date { year, month, day })
}

/// The document's heading numbering, as a Typst `#set heading(numbering: ..)`
/// pattern.
///
/// Word hangs heading numbering off the `HeadingN` paragraph *styles* via
/// `w:numPr` rather than storing it as text, so without this a document whose
/// headings Word auto-numbers imports with its numbers silently gone. That
/// includes documents `typst-docx` itself produces, which now emit live
/// `w:numPr` heading numbering — so this is what closes that round-trip.
///
/// Typst's heading numbering is full-context by default (`"1.1"` renders 1,
/// 1.1, 1.1.1), which is the scheme Word's `%1.%2` `w:lvlText` states too.
fn heading_numbering(package: &WmlPackage) -> Option<EcoString> {
    let styles = &package.styles;
    // The level-1 heading style carries the reference the whole scheme hangs
    // off; the deeper levels share its `numId`.
    let num = styles
        .by_id
        .values()
        .filter(|style| heading_level(styles, Some(style.id.as_str())) == Some(1))
        .find_map(|style| effective_para(styles, &style.para).num)?;

    let mut symbols = Vec::new();
    for ilvl in 0..9 {
        let Some(level) = package.numbering.level(num.num_id, ilvl) else { break };
        // A level Typst has no counting symbol for abandons the whole scheme,
        // rather than renumbering the levels it couldn't express — the same
        // rule `enum_numbering` follows.
        symbols.push(counting_symbol(&level.num_fmt)?);
    }
    (!symbols.is_empty()).then(|| symbols.join(".").into())
}

/// A document-default `#set text(..)` from `styles.xml`'s docDefaults, so
/// bare text inherits the document's base font/size/color.
fn default_text_style(run: &RunProps) -> Option<TextStyle> {
    let mut style = TextStyle::default();
    let mut any = false;
    if let Some(font) = &run.font {
        style.font = Some(font.clone());
        any = true;
    }
    if let Some(size) = run.size_half_pt {
        style.size_pt = Some(half_point_to_pt(size as f64));
        any = true;
    }
    if let Some(color) = parse_hex_color(run.color.as_deref()) {
        style.color = Some(color);
        any = true;
    }
    // Hoisted for the same reason as the font: Word stamps `w:lang` onto
    // practically every run, so without a document-level default every single
    // run would carry a redundant `lang:` argument (see
    // `passes::collapse_style::reduce_style`, which clears the redundant ones).
    if let Some(lang) = run.lang.as_deref().and_then(crate::mappers::run::lower_lang) {
        style.lang = Some(lang);
        any = true;
    }
    any.then_some(style)
}

/// Parse an OOXML hex color (`w:color/@w:val`, e.g. `"FF0000"`) into RGB
/// bytes. `"auto"` (the "let the app decide" sentinel) has no fixed color.
pub(crate) fn parse_hex_color(s: Option<&str>) -> Option<[u8; 3]> {
    let s = s?;
    if s.len() != 6 || s.eq_ignore_ascii_case("auto") {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::tdoc::Inline;
    use crate::wml::model::{
        Body, LevelFormat, NumRef, Numbering, ParaProps, Paragraph, Run, RunContent, RunItem,
        RunProps, Section, SectPr, Style, StyleKind, Styles,
    };

    fn text_run(text: &str) -> RunItem {
        RunItem::Run(Run { props: RunProps::default(), content: vec![RunContent::Text(text.into())] })
    }

    /// A single-section `Body` with default page properties — the shape every
    /// test in this module wants, since none of them are testing sectioning
    /// itself (that's `wml::parse`'s own test module).
    fn single_section(items: Vec<BodyItem>) -> Body {
        Body { sections: vec![Section { items, props: SectPr::default() }] }
    }

    #[test]
    fn heading_style_lowers_to_heading_block() {
        let mut by_id = FxHashMap::default();
        by_id.insert(
            "Heading1".into(),
            Style {
                id: "Heading1".into(),
                name: Some("heading 1".into()),
                kind: StyleKind::Paragraph,
                based_on: None,
                outline_level: Some(0),
                run: RunProps::default(),
                para: ParaProps::default(),
            },
        );
        let package = WmlPackage {
            body: single_section(vec![BodyItem::Paragraph(Paragraph {
                props: ParaProps { style_id: Some("Heading1".into()), ..Default::default() },
                runs: vec![text_run("Title")],
            })]),
            styles: Styles { by_id, ..Default::default() },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::Heading { level, body } => {
                assert_eq!(*level, 1);
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "Title"));
            }
            other => panic!("expected a heading, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_list_paragraphs_become_one_list_block() {
        let mut instances = FxHashMap::default();
        instances.insert(1, 100);
        let mut level_fmt = FxHashMap::default();
        level_fmt.insert(0, LevelFormat { num_fmt: "bullet".into(), start: None, lvl_text: None });
        let mut abstract_nums = FxHashMap::default();
        abstract_nums.insert(100, level_fmt);

        let para = |text: &str| {
            BodyItem::Paragraph(Paragraph {
                props: ParaProps { num: Some(NumRef { num_id: 1, ilvl: 0 }), ..Default::default() },
                runs: vec![text_run(text)],
            })
        };

        let package = WmlPackage {
            body: single_section(vec![para("Item 1"), para("Item 2")]),
            numbering: Numbering { instances, abstract_nums, ..Default::default() },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::List(list) => {
                assert_eq!(list.items.len(), 2);
                assert!(!list.items[0].ordered);
                assert!(matches!(&list.items[0].body[..], [Inline::Text(t)] if t == "Item 1"));
                assert!(matches!(&list.items[1].body[..], [Inline::Text(t)] if t == "Item 2"));
            }
            other => panic!("expected a list, got {other:?}"),
        }
    }

    #[test]
    fn direct_bold_color_run_becomes_styled_inline() {
        let run = RunItem::Run(Run {
            props: RunProps { bold: Some(true), color: Some("FF0000".into()), ..Default::default() },
            content: vec![RunContent::Text("Hi".into())],
        });
        let package = WmlPackage {
            body: single_section(vec![BodyItem::Paragraph(Paragraph {
                props: ParaProps::default(),
                runs: vec![run],
            })]),
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => match &body[..] {
                [Inline::Styled { style, body }] => {
                    assert!(style.bold);
                    assert_eq!(style.color, Some([255, 0, 0]));
                    assert!(matches!(&body[..], [Inline::Text(t)] if t == "Hi"));
                }
                other => panic!("expected a styled run, got {other:?}"),
            },
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    /// The first section is lowered flat (no `Block::Section`); every section
    /// after it becomes its own `Block::Section`, in order.
    #[test]
    fn only_sections_after_the_first_become_block_section() {
        let package = WmlPackage {
            body: Body {
                sections: vec![
                    Section {
                        items: vec![para_item("first")],
                        props: SectPr { page_w: Some(12240), ..Default::default() },
                    },
                    Section {
                        items: vec![para_item("second")],
                        props: SectPr { page_w: Some(15840), ..Default::default() },
                    },
                ],
            },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

        // Section 1's geometry hoisted to the preamble, its content flat.
        assert!(matches!(&doc.preamble[0], Stmt::SetPage(p) if p.width_pt.is_some()));
        assert_eq!(doc.body.len(), 2);
        assert!(matches!(&doc.body[0], Block::Paragraph { .. }));

        match &doc.body[1] {
            Block::Section(section) => {
                assert_eq!(
                    section.setup.width_pt,
                    Some(typst_ooxml_core::units::twip_to_abs(15840.0).to_pt())
                );
                assert_eq!(section.body.len(), 1);
            }
            other => panic!("expected a Block::Section, got {other:?}"),
        }
    }

    /// A `continuous` section that changes something besides columns (here,
    /// the page width) can't stay on the same page in Typst, so lowering
    /// records the approximation right where it has `ctx.report` to do so —
    /// `emit` itself has none (see `emit`'s own module doc comment).
    #[test]
    fn continuous_section_forced_to_break_records_an_approximation() {
        let package = WmlPackage {
            body: Body {
                sections: vec![
                    Section {
                        items: vec![para_item("first")],
                        props: SectPr { page_w: Some(12240), ..Default::default() },
                    },
                    Section {
                        items: vec![para_item("second")],
                        props: SectPr {
                            page_w: Some(15840),
                            start: SectionStart::Continuous,
                            ..Default::default()
                        },
                    },
                ],
            },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        lower(&mut ctx);

        assert!(
            report.notes.iter().any(|n| n.what == "continuous section"),
            "{:?}",
            report.notes
        );
    }

    /// A `continuous` section that changes only the column count needs no
    /// approximation at all — that's exactly the case Typst can honor.
    #[test]
    fn continuous_section_changing_only_columns_records_no_approximation() {
        let package = WmlPackage {
            body: Body {
                sections: vec![
                    Section { items: vec![para_item("first")], props: SectPr::default() },
                    Section {
                        items: vec![para_item("second")],
                        props: SectPr {
                            columns: Some(2),
                            start: SectionStart::Continuous,
                            ..Default::default()
                        },
                    },
                ],
            },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        lower(&mut ctx);

        assert!(report.notes.iter().all(|n| n.what != "continuous section"), "{:?}", report.notes);
    }

    fn para_item(text: &str) -> BodyItem {
        BodyItem::Paragraph(Paragraph { props: ParaProps::default(), runs: vec![text_run(text)] })
    }

    // The five tests below moved here from `report.rs` — they exercise
    // `LowerCtx`'s note-cycle guard, which used to live on `ImportReport`.

    #[test]
    fn distinct_notes_nest_and_unwind_cleanly() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        assert!(ctx.enter_note(false, 2));
        ctx.exit_note();
        ctx.exit_note();
        // Nothing left on the stack, so id 1 can be entered again.
        assert!(ctx.enter_note(false, 1));
    }

    #[test]
    fn a_note_cannot_re_enter_itself_while_still_on_the_stack() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        // Direct self-reference: note 1, still being lowered, refers to
        // itself again.
        assert!(!ctx.enter_note(false, 1));
    }

    #[test]
    fn an_indirect_cycle_through_another_note_is_also_refused() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        assert!(ctx.enter_note(false, 2));
        // Note 2 refers back to note 1, which is still on the stack.
        assert!(!ctx.enter_note(false, 1));
    }

    #[test]
    fn footnote_and_endnote_ids_are_tracked_independently() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        // An endnote with the same numeric id is a different note.
        assert!(ctx.enter_note(true, 1));
    }

    #[test]
    fn a_long_non_cycling_chain_is_still_capped_by_depth() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        for id in 0..100 {
            if !ctx.enter_note(false, id) {
                // Must give up well before 100 distinct, never-repeating ids.
                assert!(id < 100);
                return;
            }
        }
        panic!("expected the depth cap to stop this chain");
    }

    /// Word joins multiple authors with `;` — the same separator `typst-docx`
    /// writes — so a round-tripped author list must come back as a list.
    #[test]
    fn core_properties_lower_to_document_info() {
        let mut report = ImportReport::default();
        let info = document_info(
            &DocumentMeta {
                title: Some("Quarterly Report".into()),
                creator: Some("Aoife Brennan; R. Okonkwo".into()),
                keywords: Some("safety, quarterly".into()),
                created: Some("2026-03-14T09:00:00Z".into()),
                description: None,
            },
            &mut report,
        );

        assert_eq!(info.title.as_deref(), Some("Quarterly Report"));
        assert_eq!(info.authors, ["Aoife Brennan", "R. Okonkwo"]);
        assert_eq!(info.keywords, ["safety", "quarterly"]);
        assert_eq!(info.date, Some(Date { year: 2026, month: 3, day: 14 }));
        assert!(report.notes.is_empty());
    }

    /// Typst's `document` has no description, so the field is reported rather
    /// than quietly discarded.
    #[test]
    fn a_description_is_reported_as_dropped() {
        let mut report = ImportReport::default();
        let info = document_info(
            &DocumentMeta { description: Some("A test.".into()), ..Default::default() },
            &mut report,
        );

        assert!(info.is_empty());
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "document description");
    }

    /// Build a one-instance `Numbering` whose levels use `formats` in order.
    fn numbering_with(formats: &[&str]) -> Numbering {
        let mut instances = FxHashMap::default();
        instances.insert(1, 100);
        let mut levels = FxHashMap::default();
        for (ilvl, fmt) in formats.iter().enumerate() {
            levels.insert(ilvl as i64, LevelFormat { num_fmt: (*fmt).into(), start: None, lvl_text: None });
        }
        let mut abstract_nums = FxHashMap::default();
        abstract_nums.insert(100, levels);
        Numbering { instances, abstract_nums, ..Default::default() }
    }

    #[test]
    fn a_roman_list_gets_a_typst_numbering_pattern() {
        let numbering = numbering_with(&["lowerRoman"]);
        assert_eq!(enum_numbering(&numbering, 1, 0).as_deref(), Some("i."));
    }

    /// Typst already numbers `1.`, so a plain decimal list must not be wrapped
    /// in a redundant `#set enum(numbering: "1.")`.
    #[test]
    fn a_plain_decimal_list_keeps_typsts_default() {
        assert_eq!(enum_numbering(&numbering_with(&["decimal"]), 1, 0), None);
    }

    /// One pattern carries a counting symbol per nesting depth.
    #[test]
    fn nested_levels_join_into_one_pattern() {
        let numbering = numbering_with(&["decimal", "lowerLetter", "lowerRoman"]);
        assert_eq!(enum_numbering(&numbering, 1, 2).as_deref(), Some("1.a.i."));
    }

    /// A partly-translated pattern would silently renumber the levels it
    /// couldn't express, so one untranslatable level abandons the whole thing.
    #[test]
    fn an_untranslatable_level_keeps_typsts_default() {
        let numbering = numbering_with(&["decimal", "cardinalText"]);
        assert_eq!(enum_numbering(&numbering, 1, 1), None);
        // …but only when that level is actually reached.
        assert_eq!(enum_numbering(&numbering, 1, 0), None);
    }

    /// A `w:startOverride` belongs to the instance, so two lists sharing an
    /// `abstractNum` must not inherit each other's starting number.
    #[test]
    fn a_start_override_beats_the_shared_definition() {
        let mut numbering = numbering_with(&["decimal"]);
        numbering.abstract_nums.get_mut(&100).unwrap().get_mut(&0).unwrap().start = Some(1);
        numbering.start_overrides.insert((1, 0), 7);
        assert_eq!(numbering.start(1, 0), Some(7));
        // An instance with no override still sees the shared start.
        numbering.instances.insert(2, 100);
        assert_eq!(numbering.start(2, 0), Some(1));
    }

    /// A malformed or out-of-range timestamp must not reach `datetime(..)`,
    /// which would fail the whole compile over a cosmetic field.
    #[test]
    fn an_unusable_timestamp_yields_no_date() {
        assert_eq!(parse_w3cdtf_date("not-a-date"), None);
        assert_eq!(parse_w3cdtf_date("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_w3cdtf_date("2026-00-10"), None);
        assert_eq!(
            parse_w3cdtf_date("2026-03-14"),
            Some(Date { year: 2026, month: 3, day: 14 })
        );
    }
}

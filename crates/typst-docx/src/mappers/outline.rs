//! Outline mapper — lowers `OutlineElem` to a native Word table-of-contents
//! complex field. Implemented per `fields_links_images_validation.md §1.4` +
//! `§outline`.
//!
//! Strategy: Word builds a real, navigable TOC from a `{ TOC ... }` field that
//! it recomputes from the document's heading styles (which the heading mapper
//! emits as `Heading1..9`). We therefore emit:
//!
//!   1. an optional title paragraph (styled `TOCHeading`), and
//!   2. a paragraph holding a single complex field whose instruction is
//!      `TOC \o "1-N" \h \z \u` (headings) or `TOC \h \z \c "Figure"` /
//!      `\c "Table"` (list-of-figures / list-of-tables).
//!
//! A TOC built entirely from native Word headings/captions stays live and can
//! be rebuilt through Word's "Update Table" affordance. It is not marked dirty
//! on open because Word turns that into a disruptive external-field warning.
//! If the baked result includes Typst-only fallback entries, the field is locked:
//! Word cannot reconstruct those entries and must not erase them on update.

use comemo::Track;
use ecow::{EcoString, eco_format};
use typst_library::diag::{SourceResult, warning};
use typst_library::engine::Engine;
use typst_library::foundations::{
    Context, Element, Packed, Repr, Resolve, Selector, Smart, StyleChain,
};
use typst_library::introspection::{
    Counter, CounterKey, Location, PageNumberingIntrospection,
};
use typst_library::model::{HeadingElem, OutlineElem};
use typst_syntax::Span;

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Field, FieldCacheStatus, FieldDisplay, FieldMode, Indent, Para, ParaChild,
    ParaProps, Run, RunProps, TabAlign, TabLeader, TabStop, Toc, TocFigure, TocHeading,
};
use crate::report::{
    DecisionReason, ExportSource, ExportStage, FidelityReport, LossSet, Representation,
    SuppressedKind,
};

/// The OOXML maximum used for the `\o "1-N"` switch when Typst's outline does
/// not constrain `depth`. Typst's `depth: none` includes every level, so Word's
/// UI default of three is not an equivalent fallback.
const DEFAULT_TOC_DEPTH: usize = 9;

pub fn outline(
    elem: &Packed<OutlineElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let mut blocks = Vec::new();

    // 1. Title paragraph (styled `TOCHeading`), if the outline has a title.
    //
    // We deliberately do NOT route the title through the heading mapper: a real
    // `Heading1` paragraph would itself be picked up by the `TOC` field and
    // appear as an entry in its own table of contents. `TOCHeading` is the
    // built-in Word style for this exact purpose. (`§1.4`.)
    // `realize_title` resolves the `auto`/`none`/custom title (and the localized
    // default name) into an `Option<Content>` wrapping a `HeadingElem`; we pull
    // out its body so the title becomes a `TOCHeading` paragraph rather than a
    // real heading that would recurse into its own TOC.
    if let Some(title_body) = elem
        .realize_title(styles)
        .as_ref()
        .and_then(|c| c.to_packed::<HeadingElem>())
        .map(|h| h.body.clone())
    {
        let runs = ctx.inline_runs(&title_body, styles, RunProps::default())?;
        if !runs.is_empty() {
            blocks.push(Block::Para(Para {
                props: ParaProps {
                    style: Some("TOCHeading".into()),
                    keep_next: true,
                    ..ParaProps::default()
                },
                content: runs.into_iter().map(ParaChild::Run).collect(),
            }));
        }
    }

    // 2. The TOC field. Its entries are baked in as the cached result so the
    // table of contents shows without a manual field update — but the actual
    // entry paragraphs are filled in a post-conversion pass ([`fill_tocs`]),
    // once every heading/figure's real bookmark exists. A heading TOC carries the
    // depth to populate from; a list-of-figures/tables carries its caption
    // category. The postpass below chooses whether Word can safely rebuild it.
    let instr = toc_instruction(elem, styles);

    let caption_category = toc_category(elem, styles);
    let semantic_headings = if caption_category.is_none() {
        let entries = elem.realize_flat(ctx.engine(), styles)?;
        let mut headings = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(heading) = entry.element.to_packed::<HeadingElem>() else {
                continue;
            };
            let mut text = EcoString::new();
            if let Some(numbers) = &heading.numbers
                && !numbers.is_empty()
            {
                text.push_str(numbers);
                text.push(' ');
            }
            // Resolve the title under *this* outline's styles: a bilingual
            // title built from `context text.lang` legitimately reads
            // differently in a Chinese and an English table of contents of the
            // same document.
            text.push_str(&ctx.resolved_plain_text(&heading.body, styles)?);
            if text.is_empty() {
                continue;
            }
            headings.push(TocHeading {
                level: entry.level.get(),
                location: heading.location(),
                source_span: heading.span(),
                anchor: None,
                text,
            });
        }
        headings
    } else {
        Vec::new()
    };
    let depth = caption_category.is_none().then(|| toc_depth(elem, styles));
    // Right-tab position (page content width, in twips) for the dot leader.
    let tab_pos = ctx.available_width_dxa();
    let mut entry_indents = Vec::with_capacity(DEFAULT_TOC_DEPTH);
    for level in 1..=DEFAULT_TOC_DEPTH {
        let level = std::num::NonZeroUsize::new(level).expect("TOC levels are nonzero");
        let resolved = match elem.indent.get_ref(styles) {
            // Auto indentation depends on measured numbering-prefix widths.
            // Retain the TOC style fallback until that introspection sidecar is
            // available instead of pretending the documented 1.2em fallback
            // covers numbered entries too.
            Smart::Auto => None,
            Smart::Custom(indent) => {
                let indent = indent.resolve(
                    ctx.engine(),
                    Context::new(elem.location(), Some(styles)).track(),
                    level,
                    elem.span(),
                )?;
                let resolved = indent.resolve(styles).relative_to(ctx.available_width);
                Some(crate::props::abs_to_twip(resolved))
            }
        };
        entry_indents.push(resolved);
    }

    // Shown only when no entries are baked (a list whose figures had no captions,
    // or a document with no headings): an italic "update me" placeholder.
    let fallback = vec![Run::Text {
        props: RunProps { italic: true, ..RunProps::default() },
        text: "Right-click to update the table of contents.".into(),
    }];

    blocks.push(Block::Toc(Toc {
        instr,
        mode: FieldMode::Live,
        depth,
        caption_category,
        semantic_headings,
        tab_pos,
        entry_indents,
        entries: Vec::new(),
        fallback,
    }));

    Ok(blocks)
}

/// Populates every [`Toc`] in the body. A heading TOC fills from `recorded` (the
/// headings emitted during conversion, each carrying its real bookmark), or —
/// when nothing was recorded (headings show-ruled / rasterized) — from
/// `fallback` (introspector-queried, plain text). A list of figures/tables fills
/// from `figures` of its caption category. Called once the whole body, across
/// all sections, has been converted.
pub(crate) struct TocPlanning<'a, 'e> {
    pub engine: &'a mut Engine<'e>,
    pub styles: StyleChain<'a>,
    pub fidelity_report: &'a mut FidelityReport,
    pub snapshot: &'a crate::snapshot::ExportSnapshot,
}

pub(crate) fn fill_tocs(
    blocks: &mut [Block],
    recorded: &[TocHeading],
    figures: &[TocFigure],
    planning: &mut TocPlanning<'_, '_>,
) {
    for block in blocks.iter_mut() {
        let Block::Toc(toc) = block else { continue };
        if let Some(depth) = toc.depth {
            let headings = merge_toc_headings(recorded, &toc.semantic_headings);
            let selected: Vec<_> = headings.iter().filter(|h| h.level <= depth).collect();
            toc.mode = if selected.iter().all(|h| h.anchor.is_some()) {
                FieldMode::Live
            } else {
                FieldMode::Static
            };
            toc.entries = selected
                .iter()
                .map(|h| {
                    let (page_text, cache_status) = cached_page_text(
                        planning.engine,
                        planning.styles,
                        h.location,
                        planning.fidelity_report,
                        planning.snapshot,
                    );
                    entry_para(
                        h.level,
                        &h.anchor,
                        &h.text,
                        page_text,
                        cache_status,
                        toc.tab_pos,
                        toc.entry_indents
                            .get(h.level.saturating_sub(1))
                            .copied()
                            .flatten(),
                    )
                })
                .collect();
        } else if let Some(category) = &toc.caption_category {
            let selected: Vec<_> =
                figures.iter().filter(|f| &f.category == category).collect();
            toc.mode = if selected.iter().all(|f| f.anchor.is_some()) {
                FieldMode::Live
            } else {
                FieldMode::Static
            };
            toc.entries = selected
                .iter()
                .map(|f| {
                    let (page_text, cache_status) = cached_page_text(
                        planning.engine,
                        planning.styles,
                        f.location,
                        planning.fidelity_report,
                        planning.snapshot,
                    );
                    entry_para(
                        1,
                        &f.anchor,
                        &f.text,
                        page_text,
                        cache_status,
                        toc.tab_pos,
                        toc.entry_indents.first().copied().flatten(),
                    )
                })
                .collect();
        }
    }
}

/// Merges the introspector's complete, document-ordered semantic heading stream
/// with headings that reached the native mapper. Native records contribute live
/// bookmark anchors; introspected records retain headings transformed away by
/// custom show rules. Matching by location avoids duplicating native headings.
fn merge_toc_headings(
    recorded: &[TocHeading],
    introspected: &[TocHeading],
) -> Vec<TocHeading> {
    let mut native: Vec<Option<TocHeading>> =
        recorded.iter().cloned().map(Some).collect();
    let mut merged = Vec::with_capacity(recorded.len().max(introspected.len()));

    for semantic in introspected {
        let matching = native.iter().position(|candidate| {
            let Some(candidate) = candidate else { return false };
            (!semantic.source_span.is_detached()
                && semantic.source_span == candidate.source_span)
                || match semantic.location {
                    Some(location) => candidate.location == Some(location),
                    None => {
                        candidate.location.is_none()
                            && candidate.level == semantic.level
                            && candidate.text == semantic.text
                    }
                }
        });
        if let Some(index) = matching {
            let native = native[index].take().expect("matched native heading");
            let mut semantic = semantic.clone();
            semantic.anchor = native.anchor;
            merged.push(semantic);
        } else {
            merged.push(semantic.clone());
        }
    }

    // A native record should normally also be introspectable. Preserve any
    // exceptional records rather than dropping a live heading from the TOC.
    merged.extend(native.into_iter().flatten());
    merged
}

/// Builds one `TOC{level}` entry paragraph: the text as a hyperlink to its
/// bookmark (when it has one), a right tab with a dot leader, and a `PAGEREF`
/// field whose page number the consumer fills in.
fn entry_para(
    level: usize,
    anchor: &Option<EcoString>,
    text: &EcoString,
    page_text: EcoString,
    cache_status: FieldCacheStatus,
    tab_pos: i32,
    indent_left: Option<i32>,
) -> Para {
    let text_run = Run::Text { props: RunProps::default(), text: text.clone() };
    let mut content = Vec::new();
    match anchor {
        Some(name) => content.push(ParaChild::Hyperlink {
            rel: None,
            anchor: Some(name.clone()),
            runs: vec![text_run],
        }),
        None => content.push(ParaChild::Run(text_run)),
    }
    content.push(ParaChild::Run(Run::Tab));
    if let Some(name) = anchor {
        content.push(ParaChild::Run(Run::Field(Field {
            instr: eco_format!(" PAGEREF {name} \\h "),
            result: vec![Run::Text { props: RunProps::default(), text: page_text }],
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status,
        })));
    } else {
        // Custom show rules can consume the visible heading paragraph, leaving
        // no Word bookmark for a live PAGEREF. The semantic outline still has
        // a resolved Typst page counter: bake that value so the cached TOC is
        // complete instead of ending the leader with a blank page column.
        content.push(ParaChild::Run(Run::Text {
            props: RunProps::default(),
            text: page_text,
        }));
    }
    Para {
        props: ParaProps {
            style: Some(eco_format!("TOC{}", level.min(9))),
            ind: indent_left.map(|left| Indent { left: Some(left), ..Indent::default() }),
            tabs: vec![TabStop {
                val: TabAlign::End,
                leader: Some(TabLeader::Dot),
                pos: tab_pos,
            }],
            ..ParaProps::default()
        },
        content,
    }
}

/// Typst's own page-reference text for a TOC entry, used as the field cache.
/// This keeps the baked TOC visually complete without Word's modal
/// document-wide update workflow; the live PAGEREF can still be refreshed by
/// the user after editing/reflow.
fn cached_page_text(
    engine: &mut Engine,
    styles: StyleChain,
    location: Option<Location>,
    fidelity_report: &mut FidelityReport,
    snapshot: &crate::snapshot::ExportSnapshot,
) -> (EcoString, FieldCacheStatus) {
    let source =
        || ExportSource::new("TOC page-number cache", Span::detached(), location);
    let unavailable = |fidelity_report: &mut FidelityReport| {
        fidelity_report.record_span(
            source(),
            Representation::Approximate,
            DecisionReason::FieldCacheUnavailable,
            LossSet::DYNAMIC_BEHAVIOR,
            1,
        );
        (EcoString::from("1"), FieldCacheStatus::BestEffort)
    };
    let Some(location) = location else { return unavailable(fidelity_report) };
    if let Some(display) = snapshot.page_counter_for_location(location) {
        return (display, FieldCacheStatus::Resolved);
    }
    let span = Span::detached();
    let Some(numbering) = engine.introspect(PageNumberingIntrospection(location, span))
    else {
        // No page numbering is in force, so the physical page *is* what Word
        // shows for the PAGEREF. That is a resolved answer, not a fallback.
        return (
            location.page(engine, span).get().to_string().into(),
            FieldCacheStatus::Resolved,
        );
    };
    match Counter::new(CounterKey::Page).display_at(
        engine,
        location,
        styles,
        &numbering.trimmed(),
        span,
    ) {
        Ok(content) => {
            let text = content.plain_text();
            if text.is_empty() {
                unavailable(fidelity_report)
            } else {
                (text, FieldCacheStatus::Resolved)
            }
        }
        Err(errors) => {
            for diagnostic in errors {
                fidelity_report.suppress_span(
                    "TOC page-number cache",
                    span,
                    Some(location),
                    ExportStage::FieldPlanning,
                    SuppressedKind::Error,
                    diagnostic,
                );
            }
            engine.sink.warn(warning!(
                span,
                "DOCX could not compute a cached TOC page number; Word must refresh the PAGEREF field"
            ));
            unavailable(fidelity_report)
        }
    }
}

/// The TOC depth (`\o "1-N"`): the outline's `depth`, or Word's default of 3,
/// clamped to the valid OOXML outline range.
fn toc_depth(elem: &Packed<OutlineElem>, styles: StyleChain) -> usize {
    elem.depth
        .get(styles)
        .map(|d| d.get())
        .unwrap_or(DEFAULT_TOC_DEPTH)
        .clamp(1, 9)
}

/// Builds the `instrText` for the TOC field (with the conventional leading and
/// trailing space). Headings produce an outline-level TOC; a `figure`/`table`
/// target produces a caption-category TOC via the `\c` switch.
fn toc_instruction(elem: &Packed<OutlineElem>, styles: StyleChain) -> EcoString {
    match toc_category(elem, styles) {
        // List of figures / tables: `\c "Figure"` builds from SEQ-captioned
        // entries of that category instead of from heading outline levels.
        // `\h` (hyperlinked entries) + `\z` (hide leader/page-number in Web
        // view) match Word's "Insert Table of Figures". (`§1.3`.)
        Some(category) => eco_format!(" TOC \\h \\z \\c \"{category}\" "),

        // Table of contents from heading styles:
        //   \o "1-N"  build from Heading 1..N outline levels
        //   \h        entries are hyperlinks
        //   \z        hide tab leader + page number in Web view
        //   \u        use the applied paragraph outline level
        // This is exactly what Word's "Automatic Table" inserts. (`§1.3`.)
        None => {
            let depth = toc_depth(elem, styles);
            eco_format!(" TOC \\o \"1-{depth}\" \\h \\z \\u ")
        }
    }
}

/// Determines the caption category for a list-of-figures/tables outline.
///
/// Returns `Some("Figure")` / `Some("Table")` (or another caption category)
/// when the outline targets figures, and `None` for a heading table of
/// contents (the default `target`). The category string is the SEQ identifier
/// Word matches against caption sequences.
fn toc_category(elem: &Packed<OutlineElem>, styles: StyleChain) -> Option<EcoString> {
    // Inspect the leaf element type the selector matches. The default target is
    // `heading` (→ a real TOC); anything else is treated as a caption list.
    let target = elem.target.get_cloned(styles).0;
    let element = leaf_element(&target)?;
    match element.name() {
        // Default heading TOC: no `\c` switch.
        "heading" => None,
        // Figure outlines. Distinguish a `figure.where(kind: table)` /
        // `kind: image` selector so the `\c` category matches Word's caption
        // label ("Table" / "Figure"). Falls back to "Figure".
        "figure" => Some(figure_category(&target)),
        // Any other locatable target: use a capitalized element name as the
        // SEQ category. This still yields a valid `\c` list even if Word finds
        // no matching captions.
        // INTEGRATION-NEEDED: caption categories for arbitrary `where`-selected
        // targets depend on how the figure/caption mappers name their SEQ
        // sequences; align this category string with that SEQ identifier so the
        // `\c` switch resolves. For headings/figures (the common cases) this is
        // already correct.
        name => Some(capitalize(name)),
    }
}

/// Walks a (possibly compound) selector to the element type it filters on.
fn leaf_element(selector: &Selector) -> Option<Element> {
    match selector {
        Selector::Elem(element, _) => Some(*element),
        // `figure.where(kind: table)` lowers to an `And`/`Elem` combination;
        // recurse into the first sub-selector that names an element.
        Selector::And(subs) | Selector::Or(subs) => subs.iter().find_map(leaf_element),
        Selector::Before { selector, .. }
        | Selector::After { selector, .. }
        | Selector::Within { selector, .. } => leaf_element(selector),
        _ => None,
    }
}

/// Derives the `\c` caption category for a figure target by inspecting the
/// `kind` field constraint on the selector, if any.
fn figure_category(selector: &Selector) -> EcoString {
    // `figure.where(kind: <elem>)` encodes the kind as a field constraint in
    // the `Selector::Elem` dictionary. We look for a constrained kind whose
    // value is the `table` (or `image`) element and map it to Word's caption
    // label. Without an explicit kind, the category is "Figure".
    if selector_targets_table(selector) { "Table".into() } else { "Figure".into() }
}

/// Whether the selector constrains figures to `kind: table`.
fn selector_targets_table(selector: &Selector) -> bool {
    match selector {
        Selector::Elem(_, Some(fields)) => fields.iter().any(|(_, value)| {
            // The kind value is a `FigureKind` wrapping an element; a table list
            // constrains it to `table`. Match on the value's string form, which
            // covers both the `table` element and a `"Table"` string kind.
            let s = value.repr();
            s.contains("table")
        }),
        Selector::And(subs) | Selector::Or(subs) => {
            subs.iter().any(selector_targets_table)
        }
        Selector::Before { selector, .. }
        | Selector::After { selector, .. }
        | Selector::Within { selector, .. } => selector_targets_table(selector),
        _ => false,
    }
}

/// ASCII-capitalizes the first character of an element name for use as a SEQ
/// caption category (e.g. `"figure"` → `"Figure"`).
fn capitalize(name: &str) -> EcoString {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(name.len());
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            out.into()
        }
        None => EcoString::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::merge_toc_headings;
    use crate::dom::TocHeading;
    use typst_library::introspection::Location;

    fn heading(
        level: usize,
        location: u128,
        anchor: Option<&str>,
        text: &str,
    ) -> TocHeading {
        TocHeading {
            level,
            location: Some(Location::new(location)),
            source_span: typst_syntax::Span::detached(),
            anchor: anchor.map(Into::into),
            text: text.into(),
        }
    }

    #[test]
    fn semantic_toc_stream_fills_custom_shown_gaps_without_native_duplicates() {
        let recorded = vec![heading(2, 2, Some("native_b"), "B")];
        let introspected = vec![
            heading(1, 1, None, "A"),
            heading(2, 2, None, "B"),
            heading(3, 3, None, "C"),
        ];

        let merged = merge_toc_headings(&recorded, &introspected);
        assert_eq!(merged.len(), 3);
        assert_eq!(
            merged.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        assert_eq!(merged[1].anchor.as_deref(), Some("native_b"));
        assert!(merged[0].anchor.is_none());
        assert!(merged[2].anchor.is_none());
    }
}

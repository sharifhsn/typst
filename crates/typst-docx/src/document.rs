//! The DOCX export driver: realizes the native element tree and walks it into
//! the typed IR.

use std::num::NonZeroUsize;
use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, NativeElement, Selector, StyleChain};
use typst_library::introspection::{Introspector, Locator, PagedPosition, Tag};
use typst_library::model::{DocumentInfo, HeadingElem};
use typst_library::routines::{Arenas, RealizationKind};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, DocxDocument, Field, HdrFtrPart, HdrFtrRef, HeadingStyle, HeadingStyleSample,
    LineNumbering, Para, ParaChild, ParaProps, PgNumType, Run, RunProps, SectPr,
    SectType, Spacing, TextDefaults, TocHeading,
};
use crate::introspect::DocxIntrospector;
use crate::props;

/// Produces a DOCX document (in-memory IR) from content.
///
/// First performs root-level realization, then walks the resulting native
/// elements into the typed DOCX IR. The OPC zip is written separately by
/// [`crate::docx`].
pub fn docx_document(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
) -> SourceResult<DocxDocument> {
    docx_document_impl(engine, content, styles, None)
}

/// Produces a DOCX document backed by the fixed-point paged introspector.
///
/// The DOCX realization still runs under `Target::Docx`, but introspection
/// queries during convergence can be seeded from the paged document and the
/// final DOCX introspector delegates to that paged source first. The synthetic
/// DOCX model remains as a fallback for locations that only exist in the DOCX
/// realization.
pub fn docx_document_with_paged_introspector(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
    paged_introspector: Arc<typst_layout::PagedIntrospector>,
) -> SourceResult<DocxDocument> {
    docx_document_impl(engine, content, styles, Some(paged_introspector))
}

#[typst_macros::time(name = "docx document")]
fn docx_document_impl(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
    paged_introspector: Option<Arc<typst_layout::PagedIntrospector>>,
) -> SourceResult<DocxDocument> {
    // Mark the external styles as document-level "outside".
    let styles = styles.to_map().outside();
    let styles = StyleChain::new(&styles);

    let mut locator = Locator::root().split();
    let arenas = Arenas::default();

    let mut info = DocumentInfo::default();
    info.populate(styles);
    info.populate_locale(styles);

    let children = (engine.library.routines.realize)(
        RealizationKind::Document { info: &mut info },
        engine,
        &mut locator,
        &arenas,
        content,
        styles,
    )?;

    let pairs: Vec<_> = children.to_vec();

    // Resolve the page setup (G1): split the document into geometry sections
    // (mirrors `typst-layout/src/pages/run.rs`). Most documents are one section;
    // a mid-document `set page(..)` change (e.g. a landscape appendix) yields
    // several. This read needs no engine, so it happens before the `DocxCtx`
    // body walk; header/footer *content* is lowered later on the same `ctx`.
    let sections = resolve_sections(&pairs, styles);
    // The width fed to rasterized content comes from the first section.
    let first_geom = sections
        .first()
        .map(|section| section.geom.clone())
        .unwrap_or_else(|| run_geometry(&[], styles));

    // Per-section `set page(numbering:)`, in section order — the synthetic page
    // model resolves `loc.page-numbering()` against these (see below).
    let section_numberings: Vec<Option<typst_library::model::Numbering>> =
        if sections.is_empty() {
            vec![first_geom.numbering.clone()]
        } else {
            sections
                .iter()
                .map(|section| section.geom.numbering.clone())
                .collect()
        };
    let mirror_margins = if sections.is_empty() {
        first_geom.mirror_margins
    } else {
        sections.iter().any(|section| section.geom.mirror_margins)
    };
    let rtl_gutter = if sections.is_empty() {
        first_geom.rtl_gutter
    } else {
        sections.iter().any(|section| section.geom.rtl_gutter)
    };

    // Walk the native element tree into the typed IR.
    let (
        mut body,
        sect,
        header_parts,
        footer_parts,
        footnotes,
        numbering,
        media,
        doc_rels,
        footnote_rels,
        bookmarks,
        max_heading_level,
        heading_style_samples,
        uses_fields,
        uses_math,
        deferred_tags,
        real_alias_locations,
        toc_headings,
        toc_figures,
    ) = {
        // Isolate the conversion walk's error sink. Lowering already-realized
        // content (figure/table/grid cells, …) can surface *delayed* errors for
        // values that only resolve during layout — e.g. a date `display(auto)`
        // or a page-number query — which the main realize never hit. Those must
        // not fail a best-effort export (same reasoning as the rasterize
        // re-layout isolation). The shared introspector and its convergence
        // constraint are kept, so refs/cites/bibliography still resolve; only
        // this walk's own delayed-error reporting is discarded. Warnings are
        // forwarded to the real sink.
        let mut conv_sink = typst_library::engine::Sink::new();
        let converted = {
            use comemo::Track;
            let mut sub = typst_library::engine::Engine {
                world: engine.world,
                library: engine.library,
                introspector: typst_utils::Protected::from_raw(
                    engine.introspector.into_raw(),
                ),
                traced: engine.traced,
                sink: conv_sink.track_mut(),
                route: typst_library::engine::Route::extend(engine.route.track()),
            };
            let mut ctx = DocxCtx::new(&mut sub, &mut locator);
            // Give rasterized content the real page content width (page minus L/R
            // margins, converted from twips → pt) so width-relative content does
            // not blow up under an infinite region. Guard a degenerate width.
            let content_twip =
                first_geom.page_w - first_geom.margin_left - first_geom.margin_right;
            if content_twip > 0 {
                ctx.raster_width =
                    typst_library::layout::Abs::pt(content_twip as f64 / 20.0);
            }
            if first_geom.page_h > 0 {
                ctx.raster_height =
                    typst_library::layout::Abs::pt(first_geom.page_h as f64 / 20.0);
            }
            // Record raw/code source ranges up front: inline raw is unwrapped to
            // styled `TextElem`s before the walker sees a `RawElem`, so runs are
            // tagged `w:noProof` by matching their source span, not the mono font.
            ctx.record_raw_ranges(content);

            // Build the body and the (final) section properties. A single-section
            // document converts all `pairs` at once (unchanged behaviour, so leading
            // pagebreaks etc. are preserved exactly); a multi-section document
            // converts each section's content separately and joins them with
            // `Block::SectionBreak`s carrying the earlier sections' `sectPr`.
            let (body, sect, header_parts, footer_parts) = if sections.len() <= 1 {
                ctx.line_numbering_active = first_geom.line_numbers.is_some();
                let body = crate::convert::run(&mut ctx, &pairs)?;
                let (sect, h, f) = build_section(&mut ctx, &first_geom, styles)?;
                (body, sect, h, f)
            } else {
                // Each section builds its OWN header/footer parts (a landscape
                // appendix may carry a different running head). Per-section header
                // content can re-lower code that queries page state we don't have
                // (`here().page-numbering()` → `none`) — but that is now a *delayed*
                // error absorbed by the conversion-sink isolation above, so it no
                // longer fails the export. A hard error still falls back to a
                // geometry-only sectPr for that section.
                let mut header_parts = Vec::new();
                let mut footer_parts = Vec::new();
                let mut body = Vec::new();
                let mut final_sect = None;
                let last = sections.len() - 1;
                for (idx, section) in sections.iter().enumerate() {
                    ctx.line_numbering_active = section.geom.line_numbers.is_some();
                    let mut blocks =
                        crate::convert::run(&mut ctx, &pairs[section.range.clone()])?;
                    body.append(&mut blocks);
                    let (mut s, mut h, mut f) =
                        build_section(&mut ctx, &section.geom, styles).unwrap_or_else(
                            |_| (sectpr_geometry(&section.geom), Vec::new(), Vec::new()),
                        );
                    header_parts.append(&mut h);
                    footer_parts.append(&mut f);
                    if idx == last {
                        final_sect = Some(s);
                    } else {
                        s.sect_type = section.break_after;
                        body.push(Block::SectionBreak(s));
                    }
                }
                (
                    body,
                    final_sect.expect("at least one section"),
                    header_parts,
                    footer_parts,
                )
            };
            (
                body,
                sect,
                header_parts,
                footer_parts,
                std::mem::take(&mut ctx.footnotes),
                std::mem::take(&mut ctx.numbering),
                std::mem::replace(
                    &mut ctx.media,
                    typst_ooxml_core::media::MediaRegistry::new("word/media"),
                )
                .into_parts(),
                std::mem::take(&mut ctx.doc_rels),
                std::mem::take(&mut ctx.footnote_rels),
                std::mem::take(&mut ctx.bookmarks),
                ctx.max_heading_level,
                std::mem::take(&mut ctx.heading_style_samples),
                ctx.uses_fields,
                ctx.uses_math,
                std::mem::take(&mut ctx.deferred_tags),
                std::mem::take(&mut ctx.real_alias_locations),
                std::mem::take(&mut ctx.toc_headings),
                std::mem::take(&mut ctx.toc_figures),
            )
        };
        // Forward conversion warnings to the real sink (delayed errors stay
        // isolated in `conv_sink` and are dropped).
        for w in conv_sink.warnings() {
            engine.sink.warn(w);
        }
        converted
    };

    // Synthesize the document's bibliography into a BibLaTeX (`.bib`) string,
    // embedded as an inert sidecar part (see `encode.rs`) so external tools can
    // recover structured citation data — Word's own CITATION/BIBLIOGRAPHY field
    // model is proprietary and lossy relative to Typst/Hayagriva, so the visible
    // body text stays the realized, formatted citations; this is metadata only.
    // Same call and reasoning as the Pandoc exporter's `.bib` sidecar: a pure
    // query of the (already-stabilized) shared introspector, so it cannot
    // perturb convergence. `None` when the document has no bibliography.
    let bibliography = {
        let introspector = engine.introspector.access(
            "querying bibliography elements to synthesize a .bib sidecar is a pure query",
        );
        typst_library::model::BibliographyElem::biblatex(*introspector)
    };

    // Fallback heading list, for documents whose headings are show-ruled or
    // rasterized and so never reach the heading mapper (nothing recorded): query
    // the introspector, which still holds every heading. These entries have no
    // bookmark to link to, so they are plain text (but the titles still show).
    let toc_fallback: Vec<TocHeading> = if toc_headings.is_empty() {
        let introspector = *engine.introspector.access(
            "list headings for a table of contents whose headings were not natively converted",
        );
        introspector
            .query(&Selector::Elem(HeadingElem::ELEM, None))
            .iter()
            .filter_map(|c| c.to_packed::<HeadingElem>())
            .filter(|h| h.outlined.get(styles))
            .filter_map(|h| {
                let level = h.resolve_level(styles).get();
                let mut text = String::new();
                if let Some(numbers) = &h.numbers
                    && !numbers.is_empty()
                {
                    text.push_str(numbers);
                    text.push(' ');
                }
                text.push_str(&h.body.plain_text());
                (!text.is_empty()).then(|| TocHeading {
                    level,
                    anchor: None,
                    text: text.into(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Now that every heading/figure's real bookmark is known, populate the
    // table(s) of contents and list(s) of figures in document order, across all
    // sections.
    crate::mappers::outline::fill_tocs(
        &mut body,
        &toc_headings,
        &toc_fallback,
        &toc_figures,
    );

    // Synthetic page model: a flowing document has no real pages, but templates
    // legitimately read paged introspection (`@target(form: "page")`,
    // `loc.page-numbering()`, `counter(page)`) — returning `None` fails the
    // whole export for them. Approximate: walk the IR in order and give every
    // tag the count of explicit page/section breaks before it (Word inserts a
    // page there too, so the number is exact for break-structured front matter
    // and a lower bound where text auto-flows). The per-tag section index
    // resolves `page-numbering()` against that section's real
    // `set page(numbering:)`.
    let mut page_model = rustc_hash::FxHashMap::default();
    let mut page = 1usize;
    let mut section = 0usize;
    let mut seen_content = false;
    let mut y = 0usize;
    let mut tags = Vec::new();
    collect_positioned_tags(
        &body,
        &mut page,
        &mut section,
        &mut seen_content,
        &mut y,
        &mut page_model,
        &mut tags,
    );
    for fnote in &footnotes {
        let mut footnote_tags = Vec::new();
        collect_tags(&fnote.blocks, &mut footnote_tags);
        append_positioned_tags(footnote_tags, page, &mut y, &mut tags);
    }
    // Tags harvested from content we rasterized — so labels/refs inside a drawn
    // figure or box still resolve.
    append_positioned_tags(deferred_tags, page, &mut y, &mut tags);

    // Derive `docDefaults`/Normal from what the document's runs ACTUALLY use
    // (majority vote), not from the root StyleChain: real templates apply their
    // `set text(..)` inside a `#show: template.with(..)` wrapper, so the root
    // chain sees only Typst's built-ins — voting keeps the emitted default
    // values identical to what the runs carry (and the paragraph-mark metrics
    // stable). The root chain is only the fallback when a property never
    // appears. Then derive the used heading styles from the resolved heading
    // chains and strip only the direct properties the governing style now owns.
    let text_defaults = {
        let root_styles = typst_library::foundations::Styles::root(&pairs, styles);
        let chain = text_defaults_from_styles(StyleChain::new(&root_styles));
        let mut votes = DefaultVotes::default();
        collect_default_votes(&body, &mut votes);
        TextDefaults {
            font: weighted_mode(votes.fonts).or(chain.font),
            size_half_pt: weighted_mode(votes.sizes).unwrap_or(chain.size_half_pt),
            color: weighted_mode(votes.colors).or(chain.color),
            lang: weighted_mode(votes.langs).or(chain.lang),
        }
    };
    let mut heading_styles =
        derive_heading_styles(max_heading_level, &heading_style_samples, &body);
    demote_unrepresentable_heading_booleans(&mut heading_styles, &mut body);
    apply_style_inheritance(&text_defaults, &heading_styles, &mut body);

    let mut introspector =
        DocxIntrospector::new(&tags, paged_introspector, real_alias_locations);
    introspector.set_anchors(crate::bookmark::anchors(&bookmarks));
    introspector.set_page_model(page_model, page, section_numberings);

    let even_and_odd_headers = section_uses_even_furniture(&sect)
        || body.iter().any(|block| {
            matches!(block, Block::SectionBreak(sect) if section_uses_even_furniture(sect))
        });

    Ok(DocxDocument {
        info,
        body,
        sect,
        footnotes,
        numbering,
        media,
        doc_rels,
        footnote_rels,
        max_heading_level,
        text_defaults,
        heading_styles,
        uses_fields,
        uses_math,
        introspector: Arc::new(introspector),
        header_parts,
        footer_parts,
        background_color: first_geom.background_color,
        hyphenate: first_geom.hyphenate,
        even_and_odd_headers,
        mirror_margins,
        rtl_gutter,
        bibliography,
    })
}

fn section_uses_even_furniture(sect: &SectPr) -> bool {
    sect.headers
        .iter()
        .chain(&sect.footers)
        .any(|reference| reference.kind == "even")
}

/// Resolves the root text properties that define `docDefaults` and `Normal`.
fn text_defaults_from_styles(styles: StyleChain) -> TextDefaults {
    use typst_library::text::TextElem;
    use typst_library::visualize::Paint;

    let font = styles
        .get_ref(TextElem::font)
        .into_iter()
        .next()
        .map(|font| font.as_str().into());
    let size_half_pt = props::pt_to_half_pt(styles.resolve(TextElem::size).to_pt());
    let color = match styles.get_ref(TextElem::fill) {
        Paint::Solid(color) => Some(props::color_to_hex(color)),
        _ => None,
    };
    let lang_value = styles.get(TextElem::lang);
    let code = lang_value.as_str();
    let lang = Some(match styles.get(TextElem::region) {
        Some(region) => ecow::eco_format!("{code}-{}", region.as_str()),
        None => code.into(),
    });

    TextDefaults { font, size_half_pt, color, lang }
}

/// Derives the used `HeadingN` styles from resolved heading style-chain samples.
fn derive_heading_styles(
    max_heading_level: u8,
    samples: &[HeadingStyleSample],
    body: &[Block],
) -> Vec<HeadingStyle> {
    let mut styles = Vec::new();
    for level in 1..=max_heading_level {
        let level_samples: Vec<&RunProps> = samples
            .iter()
            .filter(|sample| sample.level == level)
            .map(|sample| &sample.rpr)
            .collect();
        let rpr = if level_samples.is_empty() {
            RunProps { bold: true, ..RunProps::default() }
        } else {
            RunProps {
                font: mode(level_samples.iter().map(|p| p.font.clone())).unwrap_or(None),
                bold: mode_bool(level_samples.iter().map(|p| p.bold)),
                italic: mode_bool(level_samples.iter().map(|p| p.italic)),
                color: mode(level_samples.iter().map(|p| p.color)).unwrap_or(None),
                size_half_pt: mode(level_samples.iter().map(|p| p.size_half_pt))
                    .unwrap_or(None),
                ..RunProps::default()
            }
        };
        styles.push(HeadingStyle {
            level,
            rpr,
            spacing: uniform_heading_spacing(level, body),
        });
    }
    styles
}

fn mode<T: Ord + Clone>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut counts = std::collections::BTreeMap::<T, usize>::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }

    let mut best = None;
    for (value, count) in counts {
        if best.as_ref().is_none_or(|(_, best_count)| count > *best_count) {
            best = Some((value, count));
        }
    }
    best.map(|(value, _)| value)
}

fn mode_bool(values: impl Iterator<Item = bool>) -> bool {
    let mut true_count = 0usize;
    let mut false_count = 0usize;
    for value in values {
        if value {
            true_count += 1;
        } else {
            false_count += 1;
        }
    }
    true_count >= false_count
}

fn uniform_heading_spacing(level: u8, body: &[Block]) -> Option<Spacing> {
    let style_id = ecow::eco_format!("Heading{level}");
    let mut seen = None::<Option<Spacing>>;
    for block in body {
        let Block::Para(para) = block else { continue };
        if para.props.style.as_ref() != Some(&style_id) {
            continue;
        }
        let spacing = para.props.spacing.clone();
        if let Some(seen_spacing) = &seen {
            if *seen_spacing != spacing {
                return None;
            }
        } else {
            seen = Some(spacing);
        }
    }
    seen.flatten()
}

/// Drops `bold`/`italic` from a heading style when any run under a heading of
/// that level resolves them OFF. The boolean run model cannot emit
/// `w:b w:val="0"`, so a style-owned bold would silently re-bold a
/// deliberately regular span (`= Heading with #text(weight: "regular")[x]`);
/// demoting the style keeps bold as per-run direct formatting for that level.
fn demote_unrepresentable_heading_booleans(
    heading_styles: &mut [HeadingStyle],
    body: &mut [Block],
) {
    for style in heading_styles {
        let mut any_bold_off = false;
        let mut any_italic_off = false;
        for block in body.iter_mut() {
            let Block::Para(para) = block else { continue };
            if heading_level(&para.props.style) != Some(style.level) {
                continue;
            }
            visit_para_run_props(para, &mut |props| {
                any_bold_off |= !props.bold;
                any_italic_off |= !props.italic;
            });
        }
        if any_bold_off {
            style.rpr.bold = false;
        }
        if any_italic_off {
            style.rpr.italic = false;
        }
    }
}

/// Strips direct properties now supplied by `Normal` or a `HeadingN` style.
fn apply_style_inheritance(
    defaults: &TextDefaults,
    heading_styles: &[HeadingStyle],
    body: &mut [Block],
) {
    for block in body {
        let Block::Para(para) = block else { continue };
        if let Some(level) = heading_level(&para.props.style)
            && let Some(style) = heading_styles.iter().find(|style| style.level == level)
        {
            strip_heading_paragraph(para, style);
        }
        visit_para_run_props(para, &mut |props| strip_text_defaults(props, defaults));
    }
}

fn heading_level(style: &Option<ecow::EcoString>) -> Option<u8> {
    let rest = style.as_deref()?.strip_prefix("Heading")?;
    let level = rest.parse::<u8>().ok()?;
    (level > 0).then_some(level)
}

fn strip_heading_paragraph(para: &mut Para, style: &HeadingStyle) {
    if para.props.keep_next {
        para.props.keep_next = false;
    }
    if para.props.outline_lvl == Some(style.level.saturating_sub(1).min(8)) {
        para.props.outline_lvl = None;
    }
    if para.props.spacing == style.spacing {
        para.props.spacing = None;
    }
    visit_para_run_props(para, &mut |props| strip_heading_run_props(props, &style.rpr));
}

fn strip_heading_run_props(props: &mut RunProps, style: &RunProps) {
    if props.font == style.font {
        props.font = None;
    }
    if props.size_half_pt == style.size_half_pt {
        props.size_half_pt = None;
    }
    if props.color == style.color {
        props.color = None;
    }
    if style.bold && props.bold == style.bold {
        props.bold = false;
    }
    if style.italic && props.italic == style.italic {
        props.italic = false;
    }
}

fn strip_text_defaults(props: &mut RunProps, defaults: &TextDefaults) {
    if props.font == defaults.font {
        props.font = None;
    }
    if props.size_half_pt == Some(defaults.size_half_pt) {
        props.size_half_pt = None;
    }
    if props.color == defaults.color {
        props.color = None;
    }
    if props.lang == defaults.lang {
        props.lang = None;
    }
}

/// Accumulated (value, text-length) votes for the document defaults.
#[derive(Default)]
struct DefaultVotes {
    fonts: std::collections::BTreeMap<ecow::EcoString, usize>,
    sizes: std::collections::BTreeMap<u32, usize>,
    colors: std::collections::BTreeMap<[u8; 3], usize>,
    langs: std::collections::BTreeMap<ecow::EcoString, usize>,
}

/// Votes for `docDefaults` from BODY PROSE only, weighted by text length:
/// heading paragraphs and code (`no_proof`) runs are excluded — a heading-only
/// or code-heavy document must not define Normal — and length-weighting keeps
/// a short deviating span from tying with (and alphabetically beating) the
/// running text. TOC entries are generated content and are skipped.
fn collect_default_votes(blocks: &[Block], votes: &mut DefaultVotes) {
    fn vote_run(votes: &mut DefaultVotes, props: &RunProps, text: &str) {
        if props.no_proof {
            return;
        }
        let weight = text.chars().count().max(1);
        if let Some(f) = &props.font {
            *votes.fonts.entry(f.clone()).or_default() += weight;
        }
        if let Some(s) = props.size_half_pt {
            *votes.sizes.entry(s).or_default() += weight;
        }
        if let Some(c) = props.color {
            *votes.colors.entry(c).or_default() += weight;
        }
        if let Some(l) = &props.lang {
            *votes.langs.entry(l.clone()).or_default() += weight;
        }
    }
    fn vote_para(votes: &mut DefaultVotes, para: &Para) {
        if heading_level(&para.props.style).is_some() {
            return;
        }
        for child in &para.content {
            match child {
                ParaChild::Run(Run::Text { props, text }) => vote_run(votes, props, text),
                ParaChild::Hyperlink { runs, .. } => {
                    for run in runs {
                        if let Run::Text { props, text } = run {
                            vote_run(votes, props, text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Para(para) => vote_para(votes, para),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_default_votes(&cell.blocks, votes);
                    }
                }
            }
            Block::Toc(_) | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

/// The highest-weight value, requiring a strict win over ties (a tie between
/// two single-span values is no mandate — fall back to the chain default).
fn weighted_mode<T: Ord + Clone>(
    counts: std::collections::BTreeMap<T, usize>,
) -> Option<T> {
    let mut iter = counts.into_iter().collect::<Vec<_>>();
    iter.sort_by_key(|(_, weight)| std::cmp::Reverse(*weight));
    match iter.as_slice() {
        [] => None,
        [only] => Some(only.0.clone()),
        [first, second, ..] if first.1 > second.1 => Some(first.0.clone()),
        _ => None,
    }
}

fn visit_para_run_props(para: &mut Para, f: &mut dyn FnMut(&mut RunProps)) {
    for child in &mut para.content {
        match child {
            ParaChild::Run(Run::Text { props, .. }) => f(props),
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    if let Run::Text { props, .. } = run {
                        f(props);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The engine-free part of the resolved page setup. The page-number format
/// classification and the header/footer content lowering need the engine/ctx,
/// so they are deferred to [`build_section`]; this struct carries everything
/// they need.
#[derive(Clone)]
struct SectGeom {
    page_w: i32,
    page_h: i32,
    landscape: bool,
    margin_top: i32,
    margin_bottom: i32,
    margin_left: i32,
    margin_right: i32,
    header_band: i32,
    footer_band: i32,
    gutter: i32,
    columns: u32,
    col_space: i32,
    /// Whether this section uses Typst inside/outside margins. Word exposes the
    /// behavior as the document-wide `<w:mirrorMargins/>` setting.
    mirror_margins: bool,
    /// Whether this section needs Word's right-side gutter setting.
    rtl_gutter: bool,
    /// Section-level line numbering derived from `par.line(numbering:)`.
    line_numbers: Option<LineNumbering>,
    /// `set page(numbering:)`, if any (drives `pgNumType` + the PAGE field).
    numbering: Option<typst_library::model::Numbering>,
    /// Where the auto page-number marginal lands: Top → header, else footer.
    number_in_header: bool,
    /// The `w:jc` for an auto page-number paragraph (from `number_align.x()`).
    number_jc: Option<crate::dom::Jc>,
    /// Explicit `set page(header:)` content (`Smart::Custom(Some(_))`).
    header: Option<Content>,
    /// Whether the header band is explicitly suppressed (`Smart::Custom(None)`).
    header_suppressed: bool,
    /// Explicit `set page(footer:)` content.
    footer: Option<Content>,
    footer_suppressed: bool,
    /// `set page(background:)` content — a full-page image/art drawn behind the
    /// text. Emitted as a `behindDoc` page-anchored drawing in the header.
    background: Option<Content>,
    /// `set page(foreground:)` content — a full-page image/art drawn in front of
    /// the body text via a page-anchored drawing in the header.
    foreground: Option<Content>,
    /// `set page(fill: solid-color)` — a flat page background colour (Word's
    /// "Page Color"). `None` for `auto`/`none`/a gradient or tiling fill (which
    /// has no native `w:background` form and is left unset, matching Word's
    /// own default of no page colour).
    background_color: Option<[u8; 3]>,
    /// Whether the section's body resolves to hyphenation enabled
    /// (`#set text(hyphenate: ..)`, `auto` following justification). Emitted
    /// document-wide as `w:autoHyphenation` (OOXML has no per-section form).
    hyphenate: bool,
}

/// Splits the document into page-geometry sections, mirroring
/// `typst-layout/src/pages/run.rs`. Each entry is a section's geometry and the
/// range of `pairs` whose content belongs to it (consecutive page runs with the
/// same geometry are merged — their internal pagebreaks stay as `<w:br>`; a
/// geometry change starts a new section, and the boundary pagebreaks between
/// them are consumed by the section break). Engine-free.
struct SectionRun {
    geom: SectGeom,
    range: std::ops::Range<usize>,
    break_after: Option<SectType>,
}

fn resolve_sections(
    pairs: &[(&Content, StyleChain)],
    initial: StyleChain,
) -> Vec<SectionRun> {
    use typst_library::layout::{ColumnsElem, PagebreakElem, Parity};

    let mut sections: Vec<SectionRun> = Vec::new();
    let mut initial = initial;
    let mut i = 0;
    while i < pairs.len() {
        // Skip pagebreaks, folding non-boundary (`set page`) ones into the
        // section-initial chain. Boundary pagebreaks carry pre-rule styles, so
        // they must NOT be folded.
        let mut forced_break = None;
        while i < pairs.len() {
            if let Some(pb) = pairs[i].0.to_packed::<PagebreakElem>() {
                if !pb.boundary.get(pairs[i].1) {
                    initial = pairs[i].1;
                }
                forced_break = pb.to.get(pairs[i].1).map(|parity| match parity {
                    Parity::Even => SectType::EvenPage,
                    Parity::Odd => SectType::OddPage,
                });
                i += 1;
            } else {
                break;
            }
        }
        if let Some(sect_type) = forced_break
            && let Some(previous) = sections.last_mut()
        {
            previous.break_after = Some(sect_type);
        }
        if i >= pairs.len() {
            break;
        }
        // Take the run of non-pagebreak content.
        let start = i;
        while i < pairs.len() && !pairs[i].0.is::<PagebreakElem>() {
            i += 1;
        }
        let group = &pairs[start..i];
        if group.iter().any(|(child, _)| child.is::<ColumnsElem>()) {
            push_column_sections(&mut sections, pairs, start..i, initial);
        } else {
            let geom = run_geometry(group, initial);
            // Merge into the previous section if nothing section-scoped changed;
            // the pagebreaks between them then fall inside the merged range
            // (→ `<w:br>`).
            push_section_run(&mut sections, geom, start..i, None, false);
        }
    }
    sections
}

fn push_column_sections(
    sections: &mut Vec<SectionRun>,
    pairs: &[(&Content, StyleChain)],
    range: std::ops::Range<usize>,
    initial: StyleChain,
) {
    use typst_library::layout::ColumnsElem;

    let base_geom = run_geometry(&pairs[range.clone()], initial);
    let mut segment_start = range.start;
    let mut saw_columns = false;
    for i in range.clone() {
        let Some(columns) = pairs[i].0.to_packed::<ColumnsElem>() else {
            continue;
        };

        if segment_start < i {
            push_section_run(
                sections,
                base_geom.clone(),
                segment_start..i,
                Some(SectType::Continuous),
                false,
            );
        } else {
            close_previous_section_at(sections, &base_geom, i);
        }

        let geom = columns_section_geometry(&base_geom, columns, pairs[i].1);
        push_section_run(sections, geom, i..i + 1, Some(SectType::Continuous), false);
        saw_columns = true;
        segment_start = i + 1;
    }

    if segment_start < range.end {
        push_section_run(sections, base_geom, segment_start..range.end, None, false);
    } else if saw_columns {
        push_section_run(sections, base_geom, range.end..range.end, None, true);
    }
}

fn push_section_run(
    sections: &mut Vec<SectionRun>,
    geom: SectGeom,
    range: std::ops::Range<usize>,
    break_after: Option<SectType>,
    allow_empty: bool,
) {
    if let Some(last) = sections.last_mut()
        && last.break_after.is_none()
        && same_section(&last.geom, &geom)
    {
        last.range.end = range.end;
        last.break_after = break_after;
        return;
    }

    if range.is_empty() && !allow_empty {
        return;
    }

    sections.push(SectionRun { geom, range, break_after });
}

fn close_previous_section_at(
    sections: &mut [SectionRun],
    geom: &SectGeom,
    boundary: usize,
) {
    if let Some(last) = sections.last_mut()
        && last.break_after.is_none()
        && same_section(&last.geom, geom)
    {
        last.range.end = boundary;
        last.break_after = Some(SectType::Continuous);
    }
}

fn columns_section_geometry(
    base: &SectGeom,
    elem: &typst_library::foundations::Packed<typst_library::layout::ColumnsElem>,
    styles: StyleChain,
) -> SectGeom {
    use typst_library::layout::Abs;

    let mut geom = base.clone();
    geom.columns = elem.count.get(styles).get() as u32;
    let content_width = (base.page_w - base.margin_left - base.margin_right).max(0);
    let reference = Abs::pt(content_width as f64 / 20.0);
    geom.col_space =
        props::abs_to_twip(elem.gutter.resolve(styles).relative_to(reference));
    geom
}

/// Whether two page runs can share one `<w:sectPr>` — equal on every property
/// a section break exists to change: the page geometry (size, orientation,
/// margins, header/footer bands, columns) *and* the section-scoped furniture
/// (header/footer content, page numbering, background). Comparing only the
/// geometry would silently merge away a mid-document `set page(header: ..)`
/// or `set page(numbering: ..)` change — the section carrying the new
/// furniture would never be emitted. Content fields are compared by hash.
///
/// `background_color` and `hyphenate` are deliberately NOT compared: both are
/// emitted document-wide (`w:background` / `w:autoHyphenation` have no
/// per-section form), so splitting on them could not express the change.
fn same_section(a: &SectGeom, b: &SectGeom) -> bool {
    use typst_utils::hash128;
    a.page_w == b.page_w
        && a.page_h == b.page_h
        && a.landscape == b.landscape
        && a.margin_top == b.margin_top
        && a.margin_bottom == b.margin_bottom
        && a.margin_left == b.margin_left
        && a.margin_right == b.margin_right
        && a.header_band == b.header_band
        && a.footer_band == b.footer_band
        && a.columns == b.columns
        && a.col_space == b.col_space
        && a.gutter == b.gutter
        && a.numbering == b.numbering
        && a.line_numbers == b.line_numbers
        && a.number_in_header == b.number_in_header
        && a.number_jc == b.number_jc
        && a.header_suppressed == b.header_suppressed
        && a.footer_suppressed == b.footer_suppressed
        && hash128(&a.header) == hash128(&b.header)
        && hash128(&a.footer) == hash128(&b.footer)
        && hash128(&a.background) == hash128(&b.background)
        && hash128(&a.foreground) == hash128(&b.foreground)
}

/// Resolves one page run's geometry from its group of content pairs.
fn run_geometry(group: &[(&Content, StyleChain)], initial: StyleChain) -> SectGeom {
    use typst_library::foundations::{Resolve, Smart, Styles};
    use typst_library::layout::{
        Abs, Binding, Dir, Em, FixAlignment, FixedAlignment, Length, OuterVAlignment,
        PageElem, Paper, Rel, Sides, Size,
    };
    use typst_library::model::{LineNumberingScope, ParLine};
    use typst_library::text::TextElem;
    use typst_utils::Numeric;

    // Fold the section group's liftable set-rules into the section-initial chain.
    let sect_styles = Styles::root(group, initial);
    let sc = StyleChain::new(&sect_styles);

    let width = sc.resolve(PageElem::width).unwrap_or(Abs::inf());
    let height = sc.resolve(PageElem::height).unwrap_or(Abs::inf());
    let mut size = Size::new(width, height);
    if sc.get(PageElem::flipped) {
        std::mem::swap(&mut size.x, &mut size.y);
    }

    // The auto-margin reference is the smaller physical dimension.
    let mut minside = size.x.min(size.y);
    if !minside.is_finite() {
        minside = Paper::A4.width();
    }
    // `auto` page axes have no DOCX equivalent (pages are fixed-size): fall back
    // to the A4 dimension for that axis.
    if !size.x.is_finite() {
        size.x = Paper::A4.width();
    }
    if !size.y.is_finite() {
        size.y = Paper::A4.height();
    }

    let default_margin = Rel::<Length>::from((2.5 / 21.0) * minside);
    let margin = sc.get(PageElem::margin).unwrap_or_default();
    let mirror_margins = margin.two_sided.unwrap_or(false);
    let sides: Sides<Abs> = margin
        .sides
        .map(|s| s.and_then(Smart::custom).unwrap_or(default_margin))
        .resolve(sc)
        .relative_to(size);

    // After the physical swap, landscape iff width > height.
    let landscape = size.x > size.y;
    let columns = sc.get(PageElem::columns).get() as u32;
    let col_space = (props::abs_to_twip(size.x) as f64 * 0.04).round() as i32;

    // Header/footer band offsets (distance from the page edge to the band).
    // `header-ascent`/`footer-descent` are measured from the inner margin edge;
    // the band offset is `margin - ascent`, clamped into `(0, margin)`.
    let header_ascent = sc.resolve(PageElem::header_ascent).relative_to(sides.top);
    let footer_descent = sc.resolve(PageElem::footer_descent).relative_to(sides.bottom);
    let header_band = clamp_band(
        props::abs_to_twip(sides.top - header_ascent),
        props::abs_to_twip(sides.top),
    );
    let footer_band = clamp_band(
        props::abs_to_twip(sides.bottom - footer_descent),
        props::abs_to_twip(sides.bottom),
    );

    // Word represents the usual "inside margin is larger than outside margin"
    // book-binding setup as an outside base margin plus a gutter. Typst stores
    // inside/outside as left/right in the unresolved margin and swaps them during
    // page finalization; `<w:mirrorMargins/>` handles that alternating swap in
    // Word.
    let binding =
        sc.get(PageElem::binding)
            .unwrap_or_else(|| match sc.resolve(TextElem::dir) {
                Dir::LTR => Binding::Left,
                _ => Binding::Right,
            });
    let (margin_left, margin_right, gutter) = if mirror_margins {
        let inside = props::abs_to_twip(sides.left);
        let outside = props::abs_to_twip(sides.right);
        if inside >= outside {
            (outside, outside, inside - outside)
        } else {
            (inside, outside, 0)
        }
    } else {
        (props::abs_to_twip(sides.left), props::abs_to_twip(sides.right), 0)
    };
    let rtl_gutter = mirror_margins && binding == Binding::Right && gutter > 0;

    let line_numbers = sc.get_ref(ParLine::numbering).as_ref().map(|_| {
        let distance = match sc.get(ParLine::number_clearance) {
            Smart::Auto => {
                let reference_width = if sc.get(PageElem::flipped) {
                    sc.resolve(PageElem::height)
                } else {
                    sc.resolve(PageElem::width)
                }
                .unwrap_or_default();
                let font_size = sc.resolve(TextElem::size);
                props::abs_to_twip((0.026 * reference_width).clamp(
                    Em::new(0.75).at(font_size).max(Abs::zero()),
                    Em::new(2.5).at(font_size).max(Abs::zero()),
                ))
            }
            Smart::Custom(clearance) => props::abs_to_twip(clearance.resolve(sc)),
        };
        let restart = match sc.get(ParLine::numbering_scope) {
            LineNumberingScope::Document => "continuous",
            LineNumberingScope::Page => "newPage",
        };
        LineNumbering { count_by: 1, start: 1, restart, distance }
    });

    let numbering = sc.get_ref(PageElem::numbering).clone();
    let number_align = sc.get(PageElem::number_align);
    let number_in_header = matches!(number_align.y(), Some(OuterVAlignment::Top));
    let number_jc = number_align.x().map(|x| match x.fix(sc.resolve(TextElem::dir)) {
        FixedAlignment::Center => crate::dom::Jc::Center,
        FixedAlignment::End => crate::dom::Jc::End,
        FixedAlignment::Start => crate::dom::Jc::Start,
    });

    let (header, header_suppressed) = match sc.get_ref(PageElem::header) {
        Smart::Custom(Some(content)) => (Some(content.clone()), false),
        Smart::Custom(None) => (None, true),
        Smart::Auto => (None, false),
    };
    let (footer, footer_suppressed) = match sc.get_ref(PageElem::footer) {
        Smart::Custom(Some(content)) => (Some(content.clone()), false),
        Smart::Custom(None) => (None, true),
        Smart::Auto => (None, false),
    };
    let background = sc.get_ref(PageElem::background).clone().filter(|c| !c.is_empty());
    let foreground = sc.get_ref(PageElem::foreground).clone().filter(|c| !c.is_empty());
    // A flat solid page-colour maps natively; `auto` (none), an explicit `none`,
    // or a gradient/tiling paint have no `w:background` form and are left unset
    // (a gradient page fill still reaches Word via `background:` if the author
    // also sets one; otherwise it is silently not represented, matching how a
    // gradient shape fill behaves before the native-gradient shape work).
    let background_color = match sc.get_ref(PageElem::fill) {
        Smart::Custom(Some(typst_library::visualize::Paint::Solid(c))) => {
            Some(props::color_to_hex(c))
        }
        _ => None,
    };

    // `auto` (the default) follows justification, exactly as text layout
    // resolves it — so a justified, hyphenated Typst document keeps that
    // intent in Word instead of silently losing it (Word's own default is off).
    let hyphenate = match sc.get(TextElem::hyphenate) {
        Smart::Custom(v) => v,
        Smart::Auto => sc.get(typst_library::model::ParElem::justify),
    };

    SectGeom {
        page_w: props::abs_to_twip(size.x),
        page_h: props::abs_to_twip(size.y),
        landscape,
        margin_top: props::abs_to_twip(sides.top),
        margin_bottom: props::abs_to_twip(sides.bottom),
        margin_left,
        margin_right,
        header_band,
        footer_band,
        gutter,
        columns,
        col_space,
        mirror_margins,
        rtl_gutter,
        line_numbers,
        numbering,
        number_in_header,
        number_jc,
        header,
        header_suppressed,
        footer,
        footer_suppressed,
        background,
        foreground,
        background_color,
        hyphenate,
    }
}

/// Clamps a header/footer band offset into `(0, margin)`, falling back to the
/// conventional 720 twips (0.5in) when the computed value is degenerate.
fn clamp_band(band: i32, margin: i32) -> i32 {
    if band > 0 && band < margin {
        band
    } else if margin > 720 {
        720
    } else {
        (margin / 2).max(1)
    }
}

/// Builds the resolved `SectPr` and any header/footer parts. Runs on the body
/// `DocxCtx` so header/footer media and relationships join the document's
/// tables.
/// A geometry-only `sectPr` (page size/margins/columns, no headers/footers or
/// page numbering). Used as the section starting point and as a graceful
/// fallback when a section's header/footer content cannot be lowered.
fn sectpr_geometry(geom: &SectGeom) -> SectPr {
    SectPr {
        page_w: geom.page_w,
        page_h: geom.page_h,
        landscape: geom.landscape,
        margin_top: geom.margin_top,
        margin_bottom: geom.margin_bottom,
        margin_left: geom.margin_left,
        margin_right: geom.margin_right,
        header: geom.header_band,
        footer: geom.footer_band,
        columns: geom.columns,
        gutter: geom.gutter,
        col_space: geom.col_space,
        line_numbers: geom.line_numbers.clone(),
        pg_num: None,
        sect_type: None,
        headers: Vec::new(),
        footers: Vec::new(),
        title_pg: false,
    }
}

fn build_section(
    ctx: &mut DocxCtx,
    geom: &SectGeom,
    styles: StyleChain,
) -> SourceResult<(SectPr, Vec<HdrFtrPart>, Vec<HdrFtrPart>)> {
    let mut sect = sectpr_geometry(geom);

    let mut header_parts = Vec::new();
    let mut footer_parts = Vec::new();

    // Page numbering → pgNumType (glyph format) + a PAGE field in the auto band.
    if let Some(numbering) = &geom.numbering {
        sect.pg_num = Some(PgNumType { fmt: numbering_fmt(ctx, numbering), start: None });
    }

    // -- Header content + page background/foreground -----------------------
    // A `set page(background:)` image is emitted as a full-page `behindDoc`
    // page-anchored drawing at the *top* of the (default) header, so it repeats
    // on every page behind the body text — the Word idiom for a page background /
    // watermark. Foreground uses the same page-anchored mechanism with
    // `behindDoc="0"`, so it overlays the body text. These drawings and the
    // explicit header share ONE part so their image relationships live in a
    // single `headerN.xml.rels` (no rId collision).
    if geom.header.is_some() || geom.background.is_some() || geom.foreground.is_some() {
        build_furniture_refs(
            ctx,
            &mut sect,
            &mut header_parts,
            FurnitureSlot::Header,
            geom,
            FurnitureSource {
                content: geom.header.as_ref(),
                background: geom.background.as_ref(),
                foreground: geom.foreground.as_ref(),
            },
            styles,
        )?;
    }

    // -- Explicit footer content -------------------------------------------
    if let Some(content) = &geom.footer {
        build_furniture_refs(
            ctx,
            &mut sect,
            &mut footer_parts,
            FurnitureSlot::Footer,
            geom,
            FurnitureSource { content: Some(content), background: None, foreground: None },
            styles,
        )?;
    }

    // -- Synthetic page-number band (numbering set, band left as `auto`) ----
    if geom.numbering.is_some() {
        if geom.number_in_header && geom.header.is_none() && !geom.header_suppressed {
            let part_name = ctx.next_hdrftr_name(true);
            let rel = ctx.add_header_rel(&part_name);
            sect.headers.push(HdrFtrRef { kind: "default", rel });
            let para =
                page_number_para(ctx, geom.numbering.as_ref(), "Header", geom.number_jc);
            header_parts.push(HdrFtrPart {
                part_name,
                is_header: true,
                blocks: vec![para],
                rels: crate::package::Rels::new(),
            });
            ctx.mark_field();
        } else if !geom.number_in_header
            && geom.footer.is_none()
            && !geom.footer_suppressed
        {
            let part_name = ctx.next_hdrftr_name(false);
            let rel = ctx.add_footer_rel(&part_name);
            sect.footers.push(HdrFtrRef { kind: "default", rel });
            let para =
                page_number_para(ctx, geom.numbering.as_ref(), "Footer", geom.number_jc);
            footer_parts.push(HdrFtrPart {
                part_name,
                is_header: false,
                blocks: vec![para],
                rels: crate::package::Rels::new(),
            });
            ctx.mark_field();
        }
    }

    Ok((sect, header_parts, footer_parts))
}

#[derive(Copy, Clone)]
enum FurnitureSlot {
    Header,
    Footer,
}

impl FurnitureSlot {
    fn is_header(self) -> bool {
        matches!(self, Self::Header)
    }
}

struct LoweredFurniture {
    blocks: Vec<Block>,
    rels: crate::package::Rels,
    signature: String,
    emit_empty: bool,
}

#[derive(Copy, Clone)]
struct FurnitureSource<'a> {
    content: Option<&'a Content>,
    background: Option<&'a Content>,
    foreground: Option<&'a Content>,
}

fn build_furniture_refs(
    ctx: &mut DocxCtx,
    sect: &mut SectPr,
    parts: &mut Vec<HdrFtrPart>,
    slot: FurnitureSlot,
    geom: &SectGeom,
    source: FurnitureSource<'_>,
    styles: StyleChain,
) -> SourceResult<()> {
    let context_sensitive = source.content.is_some_and(contains_context)
        || source.background.is_some_and(contains_context)
        || source.foreground.is_some_and(contains_context);

    let first = lower_furniture(ctx, slot, geom, source, styles, 1)?;
    if !context_sensitive {
        emit_furniture(ctx, sect, parts, slot, "default", first);
        return Ok(());
    }

    let even = lower_furniture(ctx, slot, geom, source, styles, 2)?;
    let odd = lower_furniture(ctx, slot, geom, source, styles, 3)?;
    let even_again = lower_furniture(ctx, slot, geom, source, styles, 4)?;
    let odd_again = lower_furniture(ctx, slot, geom, source, styles, 5)?;

    // `first`/`even`/`default` can only express first-page and parity-stable
    // differences. A header that embeds the literal page number, for example,
    // changes on page 3 vs page 5 and must not be represented as one odd-page
    // default header that repeats page 3 forever.
    if even.signature != even_again.signature || odd.signature != odd_again.signature {
        emit_furniture(ctx, sect, parts, slot, "default", first);
        return Ok(());
    }

    let needs_even = even.signature != odd.signature;
    let needs_first = first.signature != odd.signature;

    if !needs_even && !needs_first {
        emit_furniture(ctx, sect, parts, slot, "default", first);
        return Ok(());
    }

    let mut first = Some(first);
    let mut even = Some(even);
    let mut odd = Some(odd);

    if needs_first {
        sect.title_pg = true;
        emit_furniture(ctx, sect, parts, slot, "first", first.take().unwrap());
    }

    if needs_even {
        emit_furniture(ctx, sect, parts, slot, "even", even.take().unwrap());
    }

    let default = if needs_first {
        odd.take().unwrap()
    } else if first.as_ref().unwrap().signature == odd.as_ref().unwrap().signature {
        first.take().unwrap()
    } else {
        odd.take().unwrap()
    };
    emit_furniture(ctx, sect, parts, slot, "default", default);

    Ok(())
}

fn lower_furniture(
    ctx: &mut DocxCtx,
    slot: FurnitureSlot,
    geom: &SectGeom,
    source: FurnitureSource<'_>,
    styles: StyleChain,
    page: usize,
) -> SourceResult<LoweredFurniture> {
    let page = NonZeroUsize::new(page).unwrap();
    crate::introspect::with_furniture_page(page, || {
        let saved = ctx.part_rels.take();
        ctx.part_rels = Some(crate::package::Rels::new());
        let mut blocks = Vec::new();

        if slot.is_header()
            && let Some(bg) = source.background
            && let Some(block) = page_overlay_block(ctx, bg, geom, styles, true, "Background")?
        {
            blocks.push(block);
        }

        if let Some(content) = source.content {
            let saved_h = ctx.raster_height;
            let saved_line_numbering = ctx.line_numbering_active;
            ctx.line_numbering_active = false;
            ctx.raster_height = match slot {
                FurnitureSlot::Header => {
                    typst_library::layout::Abs::pt(geom.margin_top as f64 / 20.0)
                }
                FurnitureSlot::Footer => {
                    typst_library::layout::Abs::pt(geom.margin_bottom as f64 / 20.0)
                }
            }
            .max(typst_library::layout::Abs::pt(6.0));
            let lowered = ctx.blocks(content, styles);
            ctx.raster_height = saved_h;
            ctx.line_numbering_active = saved_line_numbering;
            blocks.extend(lowered?);
        }

        if slot.is_header()
            && let Some(fg) = source.foreground
            && let Some(block) = page_overlay_block(ctx, fg, geom, styles, false, "Foreground")?
        {
            blocks.push(block);
        }

        let rels = ctx.part_rels.take().unwrap_or_default();
        ctx.part_rels = saved;
        let signature = furniture_signature(&blocks);
        Ok(LoweredFurniture {
            blocks,
            rels,
            signature,
            emit_empty: source.content.is_some(),
        })
    })
}

fn emit_furniture(
    ctx: &mut DocxCtx,
    sect: &mut SectPr,
    parts: &mut Vec<HdrFtrPart>,
    slot: FurnitureSlot,
    kind: &'static str,
    lowered: LoweredFurniture,
) {
    if !lowered.emit_empty && lowered.blocks.is_empty() {
        return;
    }

    // Header/footer content lives outside the body IR, so its introspection
    // tags would never reach the introspector unless harvested here.
    let mut tags = Vec::new();
    collect_tags(&lowered.blocks, &mut tags);
    ctx.real_alias_locations.extend(tags.iter().map(Tag::location));
    ctx.deferred_tags.extend(tags);

    let part_name = ctx.next_hdrftr_name(slot.is_header());
    let rel = if slot.is_header() {
        ctx.add_header_rel(&part_name)
    } else {
        ctx.add_footer_rel(&part_name)
    };
    let reference = HdrFtrRef { kind, rel };
    if slot.is_header() {
        sect.headers.push(reference);
    } else {
        sect.footers.push(reference);
    }
    parts.push(HdrFtrPart {
        part_name,
        is_header: slot.is_header(),
        blocks: lowered.blocks,
        rels: lowered.rels,
    });
}

fn contains_context(content: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::foundations::ContextElem;

    content
        .traverse(&mut |elem| {
            if elem.is::<ContextElem>() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break()
}

fn furniture_signature(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        sig_block(block, &mut out);
    }
    out
}

fn sig_block(block: &Block, out: &mut String) {
    use std::fmt::Write;

    match block {
        Block::Para(para) => sig_para(para, out),
        Block::Table(table) => {
            let _ = write!(
                out,
                "tbl(w={:?},style={:?},grid={:?}",
                table.props.width_dxa, table.props.style, table.grid
            );
            for row in &table.rows {
                let _ = write!(
                    out,
                    "row(h={},cs={},rh={:?}",
                    row.header,
                    row.cant_split,
                    row.height.as_ref().map(|h| (h.val, h.exact))
                );
                for cell in &row.cells {
                    let _ = write!(
                        out,
                        "cell(w={:?},span={},merge={},shd={:?},valign={}",
                        cell.w_dxa,
                        cell.grid_span,
                        vmerge_name(cell.v_merge),
                        cell.shd_fill,
                        valign_name(cell.valign)
                    );
                    for block in &cell.blocks {
                        sig_block(block, out);
                    }
                    out.push(')');
                }
                out.push(')');
            }
            out.push(')');
        }
        Block::Toc(toc) => {
            let _ = write!(
                out,
                "toc(instr={},dirty={},depth={:?},cat={:?},tab={})",
                toc.instr, toc.dirty, toc.depth, toc.caption_category, toc.tab_pos
            );
            for entry in &toc.entries {
                sig_para(entry, out);
            }
            for run in &toc.fallback {
                sig_run(run, out);
            }
        }
        Block::SectionBreak(sect) => {
            let _ = write!(
                out,
                "sect({},{},{},{},{},{},{},{},{},{},{},{},{},{:?})",
                sect.page_w,
                sect.page_h,
                sect.landscape,
                sect.margin_top,
                sect.margin_bottom,
                sect.margin_left,
                sect.margin_right,
                sect.header,
                sect.footer,
                sect.columns,
                sect.gutter,
                sect.col_space,
                sect.title_pg,
                sect.pg_num.as_ref().map(|pg| (pg.fmt, pg.start))
            );
        }
        Block::Tag(_) => {}
    }
}

fn sig_para(para: &Para, out: &mut String) {
    out.push_str("p(");
    sig_para_props(&para.props, out);
    for child in &para.content {
        sig_para_child(child, out);
    }
    out.push(')');
}

fn sig_para_child(child: &ParaChild, out: &mut String) {
    use std::fmt::Write;

    match child {
        ParaChild::Run(run) => sig_run(run, out),
        ParaChild::OmmlPara(xml) => {
            let _ = write!(out, "ommlp({xml})");
        }
        ParaChild::Hyperlink { anchor, runs, .. } => {
            let _ = write!(out, "link({anchor:?}");
            for run in runs {
                sig_run(run, out);
            }
            out.push(')');
        }
        ParaChild::BookmarkStart { .. } | ParaChild::BookmarkEnd { .. } => {}
        ParaChild::Tag(_) => {}
    }
}

fn sig_run(run: &Run, out: &mut String) {
    use std::fmt::Write;

    match run {
        Run::Text { props, text } => {
            out.push_str("r(");
            sig_run_props(props, out);
            let _ = write!(out, "text={text})");
        }
        Run::Break => out.push_str("br;"),
        Run::PageBreak => out.push_str("pagebr;"),
        Run::ColumnBreak => out.push_str("colbr;"),
        Run::Tab => out.push_str("tab;"),
        Run::FillTab => out.push_str("filltab;"),
        Run::FootnoteRef { props, id } => {
            out.push_str("fnref(");
            sig_run_props(props, out);
            let _ = write!(out, "{id})");
        }
        Run::FootnoteRefMark => out.push_str("fnmark;"),
        Run::Drawing(drawing) => sig_drawing(drawing, out),
        Run::OmmlInline(xml) => {
            let _ = write!(out, "ommli({xml})");
        }
        Run::Field(field) => {
            let _ = write!(out, "field({},dirty={}", field.instr, field.dirty);
            for run in &field.result {
                sig_run(run, out);
            }
            out.push(')');
        }
    }
}

fn sig_para_props(props: &ParaProps, out: &mut String) {
    use std::fmt::Write;

    let _ = write!(
        out,
        "style={:?};keep_next={};keep_lines={};num={:?};bidi={};jc={};outline={:?};shd={:?};",
        props.style,
        props.keep_next,
        props.keep_lines,
        props.num,
        props.bidi,
        jc_name(props.jc),
        props.outline_lvl,
        props.shd_fill
    );
    if let Some(spacing) = &props.spacing {
        let _ = write!(
            out,
            "spacing={:?},{:?},{:?},{},{};",
            spacing.before,
            spacing.after,
            spacing.line,
            spacing.line_rule_auto,
            spacing.line_rule_at_least
        );
    }
    if let Some(ind) = &props.ind {
        let _ = write!(
            out,
            "ind={:?},{:?},{:?},{:?};",
            ind.left, ind.right, ind.first_line, ind.hanging
        );
    }
    for tab in &props.tabs {
        let _ = write!(
            out,
            "tab={},leader={},pos={};",
            tab_align_name(tab.val),
            tab.leader.map(tab_leader_name).unwrap_or(""),
            tab.pos
        );
    }
}

fn sig_run_props(props: &RunProps, out: &mut String) {
    use std::fmt::Write;

    let _ = write!(
        out,
        "style={:?};font={:?};strong={};bold={};emph={};italic={};caps={};smallcaps={};strike={};noproof={};color={:?};tracking={:?};pos={:?};size={:?};highlight={:?};shd={:?};underline={};vanish={};vert={};rtl={};cs={};lang={:?};",
        props.style,
        props.font,
        props.strong,
        props.bold,
        props.emphasis,
        props.italic,
        props.caps,
        props.smallcaps,
        props.strike,
        props.no_proof,
        props.color,
        props.tracking,
        props.position_half_pt,
        props.size_half_pt,
        props.highlight,
        props.shd_fill,
        underline_name(props.underline.as_ref()),
        props.vanish,
        vert_align_name(props.vert_align),
        props.rtl,
        props.cs,
        props.lang
    );
}

fn sig_drawing(drawing: &crate::dom::Drawing, out: &mut String) {
    use std::fmt::Write;

    let _ = write!(
        out,
        "drawing(w={},h={},alt={:?},anchor={},shape={},group={})",
        drawing.w_emu,
        drawing.h_emu,
        drawing.alt,
        drawing.anchor.as_ref().map(anchor_signature).unwrap_or_default(),
        drawing.shape.as_ref().map(shape_signature).unwrap_or_default(),
        drawing.group.as_ref().map(group_signature).unwrap_or_default()
    );
}

fn anchor_signature(anchor: &crate::dom::Anchor) -> String {
    format!(
        "h={}:{}:{:?},v={}:{}:{:?},wrap={},dist={:?},behind={}",
        anchor.pos_h.rel_from,
        anchor.pos_h.align.unwrap_or(""),
        anchor.pos_h.offset,
        anchor.pos_v.rel_from,
        anchor.pos_v.align.unwrap_or(""),
        anchor.pos_v.offset,
        anchor_wrap_name(anchor.wrap),
        anchor.dist,
        anchor.behind
    )
}

fn shape_signature(shape: &crate::dom::ShapeSpec) -> String {
    format!(
        "geom={},fill={},stroke={},txbx={}",
        shape_geom_name(&shape.geom),
        fill_signature(shape.fill.as_ref()),
        stroke_signature(shape.stroke.as_ref()),
        shape
            .txbx
            .as_ref()
            .map(|txbx| {
                let mut out = format!("ins={:?};", txbx.ins);
                for block in &txbx.blocks {
                    sig_block(block, &mut out);
                }
                out
            })
            .unwrap_or_default()
    )
}

fn group_signature(group: &crate::dom::GroupSpec) -> String {
    let mut out = String::new();
    for child in &group.children {
        use std::fmt::Write;
        let _ = write!(
            out,
            "child({},{},{},{},{});",
            child.x_emu,
            child.y_emu,
            child.w_emu,
            child.h_emu,
            shape_signature(&child.shape)
        );
    }
    out
}

fn fill_signature(fill: Option<&crate::dom::ShapeFill>) -> String {
    match fill {
        Some(crate::dom::ShapeFill::Solid(rgba)) => format!("solid={rgba:?}"),
        Some(crate::dom::ShapeFill::LinearGradient { angle_60k, stops }) => {
            format!(
                "linear={angle_60k}:{:?}",
                stops
                    .iter()
                    .map(|stop| (stop.pos_100k, stop.color))
                    .collect::<Vec<_>>()
            )
        }
        Some(crate::dom::ShapeFill::RadialGradient {
            stops,
            center_100k,
            radius_100k,
            focal_center_100k,
            focal_radius_100k,
        }) => {
            format!(
                "radial={center_100k:?}:{radius_100k}:{focal_center_100k:?}:{focal_radius_100k}:{:?}",
                stops
                    .iter()
                    .map(|stop| (stop.pos_100k, stop.color))
                    .collect::<Vec<_>>()
            )
        }
        Some(crate::dom::ShapeFill::Tile {
            image,
            tx_emu,
            ty_emu,
            sx_100k,
            sy_100k,
            algn,
        }) => {
            let image_sig = match image {
                typst_ooxml_core::dml::TileImage::Media(id) => format!("media:{id}"),
                typst_ooxml_core::dml::TileImage::Rel(rid) => format!("rel:{rid}"),
            };
            format!("tile={image_sig}:{tx_emu}:{ty_emu}:{sx_100k}:{sy_100k}:{algn}")
        }
        None => String::new(),
    }
}

fn stroke_signature(stroke: Option<&crate::dom::ShapeStroke>) -> String {
    stroke
        .map(|stroke| {
            format!(
                "{:?}:{}:{}:{:?}",
                stroke.color, stroke.w_emu, stroke.cap, stroke.dash
            )
        })
        .unwrap_or_default()
}

fn shape_geom_name(geom: &crate::dom::ShapeGeom) -> String {
    match geom {
        crate::dom::ShapeGeom::Rect => "rect".into(),
        crate::dom::ShapeGeom::RoundRect => "roundrect".into(),
        crate::dom::ShapeGeom::Ellipse => "ellipse".into(),
        crate::dom::ShapeGeom::Path(segments) => {
            let mut out = String::from("path:");
            for segment in segments {
                use crate::dom::PathSegment;
                use std::fmt::Write;
                match segment {
                    PathSegment::MoveTo(x, y) => {
                        let _ = write!(out, "M{x},{y};");
                    }
                    PathSegment::LineTo(x, y) => {
                        let _ = write!(out, "L{x},{y};");
                    }
                    PathSegment::CubicTo(x1, y1, x2, y2, x, y) => {
                        let _ = write!(out, "C{x1},{y1},{x2},{y2},{x},{y};");
                    }
                    PathSegment::Close => out.push_str("Z;"),
                }
            }
            out
        }
    }
}

fn jc_name(jc: Option<crate::dom::Jc>) -> &'static str {
    match jc {
        Some(crate::dom::Jc::Start) => "start",
        Some(crate::dom::Jc::End) => "end",
        Some(crate::dom::Jc::Center) => "center",
        Some(crate::dom::Jc::Both) => "both",
        None => "",
    }
}

fn tab_align_name(align: crate::dom::TabAlign) -> &'static str {
    match align {
        crate::dom::TabAlign::Start => "start",
        crate::dom::TabAlign::End => "end",
        crate::dom::TabAlign::Center => "center",
    }
}

fn tab_leader_name(leader: crate::dom::TabLeader) -> &'static str {
    match leader {
        crate::dom::TabLeader::Dot => "dot",
        crate::dom::TabLeader::Hyphen => "hyphen",
        crate::dom::TabLeader::Underscore => "underscore",
    }
}

fn underline_name(underline: Option<&crate::dom::Underline>) -> String {
    underline
        .map(|underline| format!("{}:{:?}", underline.val, underline.color))
        .unwrap_or_default()
}

fn vert_align_name(align: Option<crate::dom::VertAlign>) -> &'static str {
    match align {
        Some(crate::dom::VertAlign::Super) => "super",
        Some(crate::dom::VertAlign::Sub) => "sub",
        None => "",
    }
}

fn vmerge_name(merge: Option<crate::dom::VMerge>) -> &'static str {
    match merge {
        Some(crate::dom::VMerge::Restart) => "restart",
        Some(crate::dom::VMerge::Continue) => "continue",
        None => "",
    }
}

fn valign_name(align: Option<crate::dom::VAlign>) -> &'static str {
    match align {
        Some(crate::dom::VAlign::Top) => "top",
        Some(crate::dom::VAlign::Center) => "center",
        Some(crate::dom::VAlign::Bottom) => "bottom",
        None => "",
    }
}

fn anchor_wrap_name(wrap: crate::dom::AnchorWrap) -> &'static str {
    match wrap {
        crate::dom::AnchorWrap::TopAndBottom => "top-bottom",
        crate::dom::AnchorWrap::Square(value) => value,
        crate::dom::AnchorWrap::None => "none",
    }
}

/// Rasterizes `set page(background:)` or `set page(foreground:)` content and
/// wraps it in a full-page page-anchored drawing (one paragraph). Rasterized at
/// the full page *width* so `width: 100%` fills the page; the drawing's extent
/// is the full page size so it covers the sheet edge-to-edge. `None` if the
/// overlay lays out to nothing. Must be called inside an active part-rels
/// context (the image relationship belongs to the header part).
fn page_overlay_block(
    ctx: &mut DocxCtx,
    content: &Content,
    geom: &SectGeom,
    styles: StyleChain,
    behind: bool,
    name: &'static str,
) -> SourceResult<Option<crate::dom::Block>> {
    use crate::dom::{
        Anchor, AnchorPos, AnchorWrap, Block, Drawing, Para, ParaChild, Run,
    };
    use typst_library::layout::Abs;

    // EMU per twip = 914400 / 1440.
    const EMU_PER_TWIP: i64 = 635;

    let saved_w = ctx.raster_width;
    ctx.raster_width = Abs::pt(geom.page_w as f64 / 20.0);
    // Uncropped: this drawing is stretched to the full page below, so the
    // render must keep its full extent (ink-cropping a corner watermark would
    // blow it up to full-bleed).
    let result = ctx.rasterize_uncropped(content, styles, content.span())?;
    ctx.raster_width = saved_w;
    let Some((rel, _size, _text)) = result else {
        return Ok(None);
    };

    let docpr_id = ctx.next_drawing_id();
    let drawing = Drawing {
        rel,
        svg_rel: None,
        w_emu: geom.page_w as i64 * EMU_PER_TWIP,
        h_emu: geom.page_h as i64 * EMU_PER_TWIP,
        alt: None,
        docpr_id,
        name: ecow::eco_format!("{name} {docpr_id}"),
        anchor: Some(Anchor {
            z: ctx.next_z(),
            pos_h: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            pos_v: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            wrap: AnchorWrap::None,
            dist: [0, 0, 0, 0],
            behind,
        }),
        shape: None,
        group: None,
    };
    Ok(Some(Block::Para(Para {
        props: crate::dom::ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })))
}

/// Classifies a page-numbering pattern's first counting symbol into a Word
/// `w:pgNumType/@w:fmt`.
fn numbering_fmt(
    ctx: &mut DocxCtx,
    numbering: &typst_library::model::Numbering,
) -> &'static str {
    numbering_fmt_kth(ctx, numbering, 0)
}

/// Classifies the `k`-th counting symbol of a page-numbering pattern into a Word
/// format token. Renders the symbol via the engine (avoiding a direct `codex`
/// dependency) and matches the glyph for 1 and 4.
fn numbering_fmt_kth(
    ctx: &mut DocxCtx,
    numbering: &typst_library::model::Numbering,
    k: usize,
) -> &'static str {
    use typst_library::model::Numbering;
    let Numbering::Pattern(pattern) = numbering else {
        return "decimal";
    };
    if k >= pattern.pieces() {
        return "decimal";
    }
    let span = typst_syntax::Span::detached();
    let one = pattern.apply_kth(ctx.engine(), span, k, 1);
    let four = pattern.apply_kth(ctx.engine(), span, k, 4);
    // `apply_kth` includes the trailing suffix; compare on a prefix basis.
    let g1 = one.trim_end_matches(|c: char| !c.is_alphanumeric());
    let g4 = four.trim_end_matches(|c: char| !c.is_alphanumeric());
    match (g1, g4) {
        ("1", "4") => "decimal",
        ("01", "04") => "decimalZero",
        ("a", "d") => "lowerLetter",
        ("A", "D") => "upperLetter",
        ("i", "iv") => "lowerRoman",
        ("I", "IV") => "upperRoman",
        _ => "decimal",
    }
}

/// The Word field format switch (`\* <fmt>`) for a page-number field rendered in
/// the given `w:pgNumType` format, or `None` for plain decimal (Word's default,
/// which `pgNumType` already applies to `PAGE`).
fn field_format_switch(fmt: &str) -> Option<&'static str> {
    match fmt {
        "lowerRoman" => Some("roman"),
        "upperRoman" => Some("ROMAN"),
        "lowerLetter" => Some("alphabetic"),
        "upperLetter" => Some("ALPHABETIC"),
        _ => None,
    }
}

/// Builds a single-paragraph header/footer carrying the page numbering as live
/// fields. A pattern with two or more counting slots (`"1 of 1"`, `"1 / 1"`) is
/// the "page X of Y" idiom: the first slot is the current page (`PAGE`), the
/// rest the document total (`NUMPAGES`); the pattern's literal text between and
/// around the slots is emitted verbatim, and each field carries a `\* <fmt>`
/// switch so a roman/alphabetic numbering renders its total in the same system.
/// Any other numbering (single slot, or a numbering *function*) → a bare `PAGE`.
fn page_number_para(
    ctx: &mut DocxCtx,
    numbering: Option<&typst_library::model::Numbering>,
    style: &str,
    jc: Option<crate::dom::Jc>,
) -> Block {
    use typst_library::model::Numbering;
    let props = ParaProps {
        style: Some(style.into()),
        jc: jc.or(Some(crate::dom::Jc::Center)),
        ..Default::default()
    };

    let page_field = |instr: ecow::EcoString| {
        ParaChild::Run(Run::Field(Field {
            instr,
            result: vec![Run::Text { props: RunProps::default(), text: "1".into() }],
            dirty: false,
        }))
    };
    let literal = |text: ecow::EcoString| {
        ParaChild::Run(Run::Text { props: RunProps::default(), text })
    };

    let content = match numbering {
        Some(num @ Numbering::Pattern(pattern)) if pattern.pieces() >= 2 => {
            let mut runs = Vec::new();
            for k in 0..pattern.pieces() {
                let prefix = pattern.pieces[k].0.clone();
                if !prefix.is_empty() {
                    runs.push(literal(prefix));
                }
                let base = if k == 0 { "PAGE" } else { "NUMPAGES" };
                let instr = match field_format_switch(numbering_fmt_kth(ctx, num, k)) {
                    Some(sw) => ecow::eco_format!(" {base} \\* {sw} "),
                    None => ecow::eco_format!(" {base} "),
                };
                runs.push(page_field(instr));
            }
            if !pattern.suffix.is_empty() {
                runs.push(literal(pattern.suffix.clone()));
            }
            runs
        }
        _ => vec![page_field(" PAGE ".into())],
    };

    Block::Para(Para { props, content })
}

/// Recursively collects introspection tags from the IR.
pub(crate) fn collect_tags(blocks: &[Block], out: &mut Vec<Tag>) {
    for block in blocks {
        match block {
            Block::Tag(tag) => out.push(tag.clone()),
            Block::Para(para) => {
                for child in &para.content {
                    if let ParaChild::Tag(tag) = child {
                        out.push(tag.clone());
                    }
                }
            }
            Block::Table(tbl) => {
                for row in &tbl.rows {
                    for cell in &row.cells {
                        collect_tags(&cell.blocks, out);
                    }
                }
            }
            Block::Toc(toc) => {
                for para in &toc.entries {
                    for child in &para.content {
                        if let ParaChild::Tag(tag) = child {
                            out.push(tag.clone());
                        }
                    }
                }
            }
            Block::SectionBreak(_) => {}
        }
    }
}

/// Builds the synthetic page model and positioned tag stream: walks the IR in
/// the same order as [`collect_tags`], counting explicit page breaks
/// (`Run::PageBreak`) and non-leading section breaks, and records the (page,
/// section) pair plus a monotonic synthetic block position for every tag.
///
/// DOCX has no layout positions at export time. The y coordinate below is only
/// an ordering approximation, but it is strictly better than reporting every
/// location at page origin: context code that asks whether one element is close
/// to the next can distinguish adjacent blocks from later blocks.
fn collect_positioned_tags(
    blocks: &[Block],
    page: &mut usize,
    section: &mut usize,
    seen_content: &mut bool,
    y: &mut usize,
    map: &mut rustc_hash::FxHashMap<
        typst_library::introspection::Location,
        (usize, usize),
    >,
    out: &mut Vec<(Tag, PagedPosition)>,
) {
    let record = |tag: &Tag,
                  map: &mut rustc_hash::FxHashMap<_, _>,
                  out: &mut Vec<(Tag, PagedPosition)>,
                  page: usize,
                  section: usize,
                  y: usize| {
        if let Tag::Start(elem, _) = tag
            && let Some(loc) = elem.location()
        {
            map.entry(loc).or_insert((page, section));
        }
        out.push((tag.clone(), synthetic_position(page, y)));
    };
    for block in blocks {
        match block {
            Block::Tag(tag) => record(tag, map, out, *page, *section, *y),
            Block::Para(para) => {
                let mut visible = false;
                for child in &para.content {
                    match child {
                        ParaChild::Tag(tag) => record(tag, map, out, *page, *section, *y),
                        ParaChild::Run(Run::PageBreak) if *seen_content => {
                            *page += 1;
                            *y = 0;
                        }
                        ParaChild::Run(Run::PageBreak) => {}
                        _ => {
                            visible = true;
                            *seen_content = true;
                        }
                    }
                }
                if visible {
                    *y += 1;
                }
            }
            Block::Table(tbl) => {
                *seen_content = true;
                for row in &tbl.rows {
                    for cell in &row.cells {
                        collect_positioned_tags(
                            &cell.blocks,
                            page,
                            section,
                            seen_content,
                            y,
                            map,
                            out,
                        );
                    }
                }
                *y += 1;
            }
            Block::Toc(toc) => {
                *seen_content = true;
                for para in &toc.entries {
                    for child in &para.content {
                        if let ParaChild::Tag(tag) = child {
                            record(tag, map, out, *page, *section, *y);
                        }
                    }
                }
                *y += 1;
            }
            Block::SectionBreak(_) => {
                if *seen_content {
                    *page += 1;
                    *y = 0;
                }
                *section += 1;
            }
        }
    }
}

fn append_positioned_tags(
    tags: Vec<Tag>,
    page: usize,
    y: &mut usize,
    out: &mut Vec<(Tag, PagedPosition)>,
) {
    for tag in tags {
        out.push((tag, synthetic_position(page, *y)));
        *y += 1;
    }
}

fn synthetic_position(page: usize, y: usize) -> PagedPosition {
    use typst_library::layout::{Abs, Point};

    const BLOCK_STEP_PT: f64 = 20.0;

    PagedPosition {
        page: NonZeroUsize::new(page.max(1)).unwrap(),
        point: Point::new(Abs::zero(), Abs::pt(y as f64 * BLOCK_STEP_PT)),
    }
}

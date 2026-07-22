//! The DOCX export driver: realizes the native element tree and walks it into
//! the typed IR.

use std::num::NonZeroUsize;
use std::sync::Arc;

use typst_library::World;
use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Introspector, Location, Locator, PagedPosition, Tag};
use typst_library::layout::Abs;
use typst_library::model::DocumentInfo;
use typst_library::routines::{Arenas, RealizationKind};
use typst_library::text::{
    FontBook, FontInfo, FontStretch, FontStyle, FontVariant, FontWeight,
};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, BookmarkTable, Comment, DocxDocument, EmbeddedFontProgram, EmbeddedFontStyle,
    Field, FieldCacheStatus as DomFieldCacheStatus, FieldDisplay, FieldMode, Footnote,
    HdrFtrPart, HdrFtrRef, HeadingStyle, HeadingStyleSample, LineNumbering, MediaPart,
    NumberingTable, Para, ParaChild, ParaProps, PgNumType, PicClip, ReviewCandidate,
    ReviewCandidateKind, ReviewOrigin, Run, RunProps, SectPr, SectType, Spacing,
    TextDefaults, TocFigure, TocHeading, VAlign,
};
use crate::introspect::DocxIntrospector;
use crate::package::Rels;
use crate::props;
use crate::report::{
    DecisionReason, ExportSource, ExportStage, FidelityReport,
    FieldCacheStatus as ReportFieldCacheStatus, FieldOwner, FieldVisibility, LossSet,
    Representation, SuppressedKind,
};

/// The complete product of the lowering walk before document-wide postpasses.
///
/// Keeping this named avoids the previous nineteen-field tuple and gives new
/// planning/reporting state an explicit home before it is frozen into
/// [`DocxDocument`].
struct LoweredDocx {
    body: Vec<Block>,
    sect: SectPr,
    header_parts: Vec<HdrFtrPart>,
    footer_parts: Vec<HdrFtrPart>,
    footnotes: Vec<Footnote>,
    comments: Vec<Comment>,
    numbering: NumberingTable,
    media: Vec<MediaPart>,
    doc_rels: Rels,
    footnote_rels: Rels,
    comment_rels: Rels,
    bookmarks: BookmarkTable,
    max_heading_level: u8,
    heading_style_samples: Vec<HeadingStyleSample>,
    heading_num_levels: Option<Option<Vec<crate::dom::ListLevel>>>,
    uses_math: bool,
    deferred_tags: Vec<Tag>,
    real_alias_locations: rustc_hash::FxHashSet<Location>,
    real_semantic_alias_locations: rustc_hash::FxHashSet<Location>,
    toc_headings: Vec<TocHeading>,
    toc_figures: Vec<TocFigure>,
    fidelity_report: FidelityReport,
}

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
    docx_document_impl(engine, content, styles, None, None, None)
}

/// Produces a DOCX document backed by the fixed-point paged introspector.
///
/// The DOCX realization still runs under `Target::Docx`, but introspection
/// queries during convergence can be seeded from the paged document and the
/// final DOCX introspector delegates to that paged source first. The synthetic
/// DOCX model remains as a fallback for locations that only exist in the DOCX
/// realization.
///
/// `paged_page_sizes` (each real page's `frame.size()`, indexed by physical
/// page number minus one) lets an `auto` page axis (`set page(width: ..,
/// height: auto)`, an extremely common ticket/certificate/single-page-diagram
/// idiom) resolve to Typst's own true content-driven size instead of a
/// hardcoded A4 fallback — see [`real_section_size`].
pub fn docx_document_with_paged_introspector(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
    paged_introspector: Arc<typst_layout::PagedIntrospector>,
    paged_page_sizes: Arc<Vec<typst_library::layout::Size>>,
) -> SourceResult<DocxDocument> {
    docx_document_impl(
        engine,
        content,
        styles,
        Some(paged_introspector),
        Some(paged_page_sizes),
        None,
    )
}

/// Produces a DOCX document backed by both paged introspection and an owned
/// frame-geometry sidecar. The CLI uses this entry point so table/grid preflight
/// can consume final physical cell sizes that are intentionally absent from the
/// queryable introspector.
pub fn docx_document_with_paged_geometry(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
    paged_introspector: Arc<typst_layout::PagedIntrospector>,
    paged_page_sizes: Arc<Vec<typst_library::layout::Size>>,
    paged_geometry: Arc<typst_export_common::paged::PagedGeometry>,
) -> SourceResult<DocxDocument> {
    docx_document_impl(
        engine,
        content,
        styles,
        Some(paged_introspector),
        Some(paged_page_sizes),
        Some(paged_geometry),
    )
}

#[typst_macros::time(name = "docx document")]
fn docx_document_impl(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
    paged_introspector: Option<Arc<typst_layout::PagedIntrospector>>,
    paged_page_sizes: Option<Arc<Vec<typst_library::layout::Size>>>,
    paged_geometry: Option<Arc<typst_export_common::paged::PagedGeometry>>,
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
    let real_ref = paged_introspector.as_deref();
    let page_sizes_ref = paged_page_sizes.as_deref().map(Vec::as_slice);
    let export_snapshot = crate::snapshot::ExportSnapshot::build(
        engine,
        styles,
        &pairs,
        real_ref,
        page_sizes_ref,
        paged_geometry.as_deref(),
    );
    let mut sections = resolve_sections(&pairs, styles, real_ref, page_sizes_ref);
    let section_backgrounds_vary = sections.first().is_some_and(|first| {
        sections
            .iter()
            .skip(1)
            .any(|section| section.geom.background_color != first.geom.background_color)
    });
    // `w:background` is document-global. When sections disagree, omit it;
    // explicitly coloured sections already carry a compatibility shape, while
    // an unfilled section naturally uses Word's white page. Manufacturing a
    // white full-page header shape for every section makes slide decks reflow.
    if section_backgrounds_vary {
        let mut preceding_colored_section = false;
        for section in &mut sections {
            let default_white = section.geom.background_color.is_none()
                || section.geom.background_color == Some([255, 255, 255]);
            if default_white {
                section.geom.background_color = None;
                // A missing header inherits the preceding section's header in
                // Word. Emit an empty part to stop a dark background shape
                // carrying into this white section, without adding another
                // full-page anchor.
                if preceding_colored_section
                    && section.geom.header.is_none()
                    && section.geom.background.is_none()
                    && section.geom.foreground.is_none()
                {
                    section.geom.header = Some(Content::empty());
                }
            }
            preceding_colored_section = !default_white;
        }
    }
    // The width fed to rasterized content comes from the first section.
    let first_geom = sections
        .first()
        .map(|section| section.geom.clone())
        .unwrap_or_else(|| run_geometry(&[], styles, real_ref, page_sizes_ref));

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
    let LoweredDocx {
        mut body,
        sect,
        mut header_parts,
        mut footer_parts,
        mut footnotes,
        mut comments,
        mut numbering,
        media,
        doc_rels,
        footnote_rels,
        comment_rels,
        bookmarks,
        max_heading_level,
        heading_style_samples,
        heading_num_levels,
        uses_math,
        deferred_tags,
        real_alias_locations,
        real_semantic_alias_locations,
        toc_headings,
        toc_figures,
        mut fidelity_report,
    } = {
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
        let mut converted = {
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
            ctx.set_paged_introspector(paged_introspector.clone());
            ctx.set_snapshot_bookmarks(&export_snapshot);
            if let Some(geometry) = &paged_geometry {
                ctx.set_paged_geometry(Arc::clone(geometry));
            }
            set_ctx_geometry(&mut ctx, &first_geom);
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
                let dense_visual_page = (content_text_chars(content) == 0)
                    .then(|| ctx.paged_geometry.dense_visual_page().cloned())
                    .flatten();
                let body = if let Some(frame) = dense_visual_page {
                    if let Some(block) =
                        dense_visual_page_block(&mut ctx, content, frame, &first_geom)
                    {
                        vec![block]
                    } else {
                        crate::convert::run(&mut ctx, &pairs)?
                    }
                } else {
                    crate::convert::run(&mut ctx, &pairs)?
                };
                let full_width = ctx.page_content_width_dxa();
                let (sect, h, f) = ctx.with_available_width(full_width, |ctx| {
                    build_section(ctx, &first_geom, styles)
                })?;
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
                    set_ctx_geometry(&mut ctx, &section.geom);
                    ctx.line_numbering_active = section.geom.line_numbers.is_some();
                    let mut blocks = Vec::new();
                    for _ in 0..section.leading_pagebreaks {
                        blocks.push(Block::Para(Para {
                            props: ParaProps::default(),
                            content: vec![ParaChild::Run(Run::PageBreak)],
                        }));
                    }
                    blocks.extend(crate::convert::run(
                        &mut ctx,
                        &pairs[section.range.clone()],
                    )?);
                    body.append(&mut blocks);
                    let full_width = ctx.page_content_width_dxa();
                    // `build_section` allocates a `word/_rels/document.xml.rels`
                    // entry for every header/footer part it decides to emit. The
                    // fallback below throws those parts away, so the entries have
                    // to go with them: a relationship whose target part is never
                    // written is an invalid package, and OPC finalization rightly
                    // refuses it.
                    let rels_savepoint = ctx.doc_rels.savepoint();
                    let (mut s, mut h, mut f) = match ctx
                        .with_available_width(full_width, |ctx| {
                            build_section(ctx, &section.geom, styles)
                        }) {
                        Ok(parts) => parts,
                        Err(errors) => {
                            ctx.doc_rels.rollback(rels_savepoint);
                            let source =
                                ecow::eco_format!("section {} properties", idx + 1);
                            for diagnostic in errors {
                                ctx.fidelity_report.suppress_span(
                                    source.clone(),
                                    typst_syntax::Span::detached(),
                                    None,
                                    ExportStage::SectionLowering,
                                    SuppressedKind::Error,
                                    diagnostic,
                                );
                            }
                            ctx.fidelity_report.record_span(
                                ExportSource::new(
                                    source,
                                    typst_syntax::Span::detached(),
                                    None,
                                ),
                                Representation::Approximate,
                                DecisionReason::SectionGeometryFallback,
                                LossSet::SECTION_GEOMETRY_ONLY,
                                0,
                            );
                            (sectpr_geometry(&section.geom), Vec::new(), Vec::new())
                        }
                    };
                    header_parts.append(&mut h);
                    footer_parts.append(&mut f);
                    // A section's `w:type` describes how *that* section itself
                    // starts (relative to the one before it) — so it must come
                    // from the *previous* section's `break_after` (the break
                    // requested between idx-1 and idx), not this section's own
                    // `break_after` (which describes the break to the *next*
                    // section instead). The first section has no previous break.
                    s.sect_type =
                        idx.checked_sub(1).and_then(|prev| sections[prev].break_after);
                    if idx == last {
                        final_sect = Some(s);
                    } else {
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
            LoweredDocx {
                body,
                sect,
                header_parts,
                footer_parts,
                footnotes: std::mem::take(&mut ctx.footnotes),
                comments: std::mem::take(&mut ctx.comments),
                numbering: std::mem::take(&mut ctx.numbering),
                media: std::mem::replace(
                    &mut ctx.media,
                    typst_ooxml_core::media::MediaRegistry::new("word/media"),
                )
                .into_parts(),
                doc_rels: std::mem::take(&mut ctx.doc_rels),
                footnote_rels: std::mem::take(&mut ctx.footnote_rels),
                comment_rels: std::mem::take(&mut ctx.comment_rels),
                bookmarks: std::mem::take(&mut ctx.bookmarks),
                max_heading_level: ctx.max_heading_level,
                heading_style_samples: std::mem::take(&mut ctx.heading_style_samples),
                heading_num_levels: ctx.heading_num_levels.take(),
                uses_math: ctx.uses_math,
                deferred_tags: std::mem::take(&mut ctx.deferred_tags),
                real_alias_locations: std::mem::take(&mut ctx.real_alias_locations),
                real_semantic_alias_locations: std::mem::take(
                    &mut ctx.real_semantic_alias_locations,
                ),
                toc_headings: std::mem::take(&mut ctx.toc_headings),
                toc_figures: std::mem::take(&mut ctx.toc_figures),
                fidelity_report: std::mem::take(&mut ctx.fidelity_report),
            }
        };
        // Forward warnings, but retain delayed errors in the fidelity report so
        // best-effort conversion is observable rather than silent.
        for diagnostic in conv_sink.delayed() {
            converted.fidelity_report.suppress_span(
                "document conversion",
                typst_syntax::Span::detached(),
                None,
                ExportStage::DocumentConversion,
                SuppressedKind::DelayedError,
                diagnostic,
            );
        }
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
    let bibliography = export_snapshot.bibliography_biblatex().map(str::to_owned);

    // Map the same snapshot-owned entries onto Word's native `b:Source` schema
    // so References → Manage Sources and the lossless sidecar cannot select
    // different semantic source sets.
    let word_sources =
        crate::bibliography::map_entries(&export_snapshot.bibliography_source_entries());

    // Now that every heading/figure's real bookmark is known, populate the
    // table(s) of contents and list(s) of figures in document order, across all
    // sections.
    let mut toc_planning = crate::mappers::outline::TocPlanning {
        engine,
        styles,
        fidelity_report: &mut fidelity_report,
        snapshot: &export_snapshot,
    };
    crate::mappers::outline::fill_tocs(
        &mut body,
        &toc_headings,
        &toc_figures,
        &mut toc_planning,
    );

    crate::mappers::table::collapse_par_spacing(&mut body);
    for footnote in &mut footnotes {
        crate::mappers::table::collapse_par_spacing(&mut footnote.blocks);
    }
    for comment in &mut comments {
        crate::mappers::table::collapse_par_spacing(&mut comment.blocks);
    }

    let review_candidates = collect_review_candidates(
        &body,
        &footnotes,
        &header_parts,
        &footer_parts,
        &export_snapshot,
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

    // Promote the baked heading numbers to Word auto-numbering. This needs the
    // finished heading styles (it hangs a `w:numPr` off them) and the finished
    // body (it replays Word's counters over the real heading sequence), so it
    // runs here rather than during lowering.
    crate::heading_numbering::apply(
        &mut crate::heading_numbering::Parts {
            body: &mut body,
            footnotes: &mut footnotes,
            headers: &mut header_parts,
            footers: &mut footer_parts,
        },
        heading_num_levels.as_ref().and_then(|levels| levels.as_ref()),
        &mut numbering,
        &mut heading_styles,
        &mut fidelity_report,
    );

    let mut introspector = DocxIntrospector::new(
        &tags,
        paged_introspector,
        real_alias_locations,
        real_semantic_alias_locations,
    );
    introspector.set_anchors(crate::bookmark::anchors(&bookmarks));
    introspector.set_page_model(page_model, page, section_numberings);

    let even_and_odd_headers = section_uses_even_furniture(&sect)
        || body.iter().any(|block| {
            matches!(block, Block::SectionBreak(sect) if section_uses_even_furniture(sect))
        });

    // Repeated content (subslides, running heads) re-emits the same bookmark;
    // keep only each part's first start/end pair so the finalized IR satisfies
    // the uniqueness invariants (see `invariants::dedupe_repeated_bookmarks`).
    crate::invariants::dedupe_repeated_bookmarks(
        &mut body,
        &mut header_parts,
        &mut footer_parts,
        &mut footnotes,
        &mut comments,
    );
    crate::invariants::resolve_leading_background_layers(
        &mut body,
        &mut header_parts,
        &mut footer_parts,
        &mut footnotes,
        &mut comments,
    );
    // Names handed out by `add_bookmark` that no lowering site materialized:
    // bind them to the target's own introspection tag so the link resolves,
    // before anything still unbound is demoted below.
    crate::invariants::bind_dangling_bookmarks(
        &mut body,
        &mut header_parts,
        &mut footer_parts,
        &mut footnotes,
        &mut comments,
        &bookmarks,
    );
    let dangling = crate::invariants::fallback_dangling_internal_fields(
        &mut body,
        &mut header_parts,
        &mut footer_parts,
        &mut footnotes,
        &mut comments,
    );
    for target in dangling.fields {
        fidelity_report.record_span(
            crate::report::ExportSource::new(
                format!("reference target {target}"),
                typst_syntax::Span::detached(),
                None,
            ),
            crate::report::Representation::Approximate,
            crate::report::DecisionReason::DanglingReferenceTextFallback,
            crate::report::LossSet::DYNAMIC_BEHAVIOR,
            0,
        );
    }
    for target in dangling.links {
        fidelity_report.record_span(
            crate::report::ExportSource::new(
                format!("link target {target}"),
                typst_syntax::Span::detached(),
                None,
            ),
            crate::report::Representation::Approximate,
            crate::report::DecisionReason::DanglingLinkTextFallback,
            crate::report::LossSet::DYNAMIC_BEHAVIOR,
            0,
        );
    }

    record_dynamic_field_inventory(
        &mut fidelity_report,
        export_snapshot.logical_id(),
        &body,
        &header_parts,
        &footer_parts,
        &footnotes,
    );
    record_font_inventory(
        &mut fidelity_report,
        export_snapshot.logical_id(),
        engine.world.book(),
        &text_defaults,
        &heading_styles,
        uses_math,
        &body,
        &header_parts,
        &footer_parts,
        &footnotes,
    );
    let embedded_fonts =
        collect_embeddable_fonts(fidelity_report.fonts(), engine.world.book(), |index| {
            engine.world.font(index)
        });
    for font in &embedded_fonts {
        fidelity_report.mark_font_embedded(&font.family);
    }
    record_drawing_inventory(
        &mut fidelity_report,
        export_snapshot.logical_id(),
        &body,
        &header_parts,
        &footer_parts,
        &footnotes,
    );

    Ok(DocxDocument {
        info,
        body,
        sect,
        footnotes,
        comments,
        numbering,
        media,
        embedded_fonts,
        doc_rels,
        footnote_rels,
        comment_rels,
        max_heading_level,
        text_defaults,
        heading_styles,
        uses_math,
        introspector: Arc::new(introspector),
        header_parts,
        footer_parts,
        background_color: (!section_backgrounds_vary)
            .then_some(first_geom.background_color)
            .flatten(),
        hyphenate: first_geom.hyphenate,
        even_and_odd_headers,
        mirror_margins,
        rtl_gutter,
        bibliography,
        word_sources,
        fidelity_report,
        export_snapshot,
        review_candidates,
    })
}

fn collect_review_candidates(
    body: &[Block],
    footnotes: &[Footnote],
    headers: &[HdrFtrPart],
    footers: &[HdrFtrPart],
    snapshot: &crate::snapshot::ExportSnapshot,
) -> Vec<ReviewCandidate> {
    let mut paragraphs = Vec::new();
    collect_review_paragraphs(body, &mut paragraphs);
    for footnote in footnotes {
        collect_review_paragraphs(&footnote.blocks, &mut paragraphs);
    }
    for part in headers.iter().chain(footers) {
        collect_review_paragraphs(&part.blocks, &mut paragraphs);
    }
    let mut seeds = Vec::new();
    for para in paragraphs {
        let run_count = review_text_runs(para).count();
        if run_count == 1
            && let Some(origin) = para.props.review_origin
            && let Some(text) = plain_review_text(para)
        {
            seeds.push((origin, text));
            continue;
        }
        for (origin, text) in review_text_runs(para) {
            seeds.push((origin, text.clone()));
        }
    }
    seeds
        .iter()
        .filter_map(|(origin, baseline)| {
            if origin.span.is_detached()
                || seeds.iter().filter(|(other, _)| other.span == origin.span).count()
                    != 1
            {
                return None;
            }
            if origin.kind == ReviewCandidateKind::Heading {
                let mut nodes = snapshot.nodes().iter().filter(|node| {
                    node.source.span == origin.span
                        && node.source.element.as_str() == "heading"
                        && node.semantic_occurrences == 1
                        && node.paged_positions.len() == 1
                });
                nodes.next()?;
                if nodes.next().is_some() {
                    return None;
                }
            }
            Some(ReviewCandidate {
                join_id: origin.join_id,
                span: origin.span,
                kind: origin.kind,
                baseline: baseline.clone(),
            })
        })
        .collect()
}

fn collect_review_paragraphs<'a>(blocks: &'a [Block], out: &mut Vec<&'a Para>) {
    for block in blocks {
        match block {
            Block::Para(para) => out.push(para),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_review_paragraphs(&cell.blocks, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn review_text_runs(
    para: &Para,
) -> impl Iterator<Item = (ReviewOrigin, &ecow::EcoString)> {
    para.content.iter().flat_map(|child| match child {
        ParaChild::Run(Run::Text { props, text }) => props
            .review_origin
            .map(|origin| vec![(origin, text)])
            .unwrap_or_default(),
        ParaChild::Hyperlink { runs, .. } => runs
            .iter()
            .filter_map(|run| {
                let Run::Text { props, text } = run else { return None };
                props.review_origin.map(|origin| (origin, text))
            })
            .collect(),
        _ => Vec::new(),
    })
}

fn plain_review_text(para: &Para) -> Option<ecow::EcoString> {
    let mut text = ecow::EcoString::new();
    for child in &para.content {
        match child {
            ParaChild::Run(Run::Text { text: part, .. }) => text.push_str(part),
            ParaChild::Run(
                Run::FootnoteRefMark | Run::Tab | Run::CommentReference { .. },
            ) => {}
            ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::CommentRangeStart { .. }
            | ParaChild::CommentRangeEnd { .. }
            | ParaChild::Tag(_) => {}
            _ => return None,
        }
    }
    (!text.is_empty()).then_some(text)
}

fn record_dynamic_field_inventory(
    report: &mut FidelityReport,
    snapshot_id: u128,
    body: &[Block],
    headers: &[HdrFtrPart],
    footers: &[HdrFtrPart],
    footnotes: &[Footnote],
) {
    record_block_fields(report, snapshot_id, body);
    for part in headers.iter().chain(footers) {
        record_block_fields(report, snapshot_id, &part.blocks);
    }
    for footnote in footnotes {
        record_block_fields(report, snapshot_id, &footnote.blocks);
    }
}

fn record_block_fields(report: &mut FidelityReport, snapshot_id: u128, blocks: &[Block]) {
    for block in blocks {
        match block {
            Block::WeakPageBreak => {}
            Block::Para(para) => record_para_fields(report, snapshot_id, para),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        record_block_fields(report, snapshot_id, &cell.blocks);
                    }
                }
            }
            Block::Toc(toc) => {
                report.record_dynamic_field(
                    snapshot_id,
                    &toc.instr,
                    field_owner(toc.mode),
                    FieldVisibility::Visible,
                    ReportFieldCacheStatus::Resolved,
                );
                for entry in &toc.entries {
                    record_para_fields(report, snapshot_id, entry);
                }
                for run in &toc.fallback {
                    record_run_fields(report, snapshot_id, run);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn record_para_fields(report: &mut FidelityReport, snapshot_id: u128, para: &Para) {
    for child in &para.content {
        match child {
            ParaChild::Run(run) => record_run_fields(report, snapshot_id, run),
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    record_run_fields(report, snapshot_id, run);
                }
            }
            ParaChild::OmmlPara(_)
            | ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::CommentRangeStart { .. }
            | ParaChild::CommentRangeEnd { .. }
            | ParaChild::Tag(_) => {}
        }
    }
}

fn record_run_fields(report: &mut FidelityReport, snapshot_id: u128, run: &Run) {
    match run {
        Run::Field(field) => {
            report.record_dynamic_field(
                snapshot_id,
                &field.instr,
                field_owner(field.mode),
                match field.display {
                    FieldDisplay::Visible => FieldVisibility::Visible,
                    FieldDisplay::Hidden => FieldVisibility::Hidden,
                },
                match field.cache_status {
                    DomFieldCacheStatus::Resolved => ReportFieldCacheStatus::Resolved,
                    DomFieldCacheStatus::BestEffort => ReportFieldCacheStatus::BestEffort,
                    DomFieldCacheStatus::ConsumerRequired => {
                        ReportFieldCacheStatus::ConsumerRequired
                    }
                    DomFieldCacheStatus::Unavailable => {
                        ReportFieldCacheStatus::Unavailable
                    }
                },
            );
            for result in &field.result {
                record_run_fields(report, snapshot_id, result);
            }
        }
        Run::Drawing(drawing) => {
            if let Some(text_box) =
                drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
            {
                record_block_fields(report, snapshot_id, &text_box.blocks);
            }
            if let Some(group) = &drawing.group {
                for child in &group.children {
                    if let Some(text_box) = &child.shape.txbx {
                        record_block_fields(report, snapshot_id, &text_box.blocks);
                    }
                }
            }
        }
        Run::Text { .. }
        | Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::Tab
        | Run::FillTab
        | Run::FootnoteRef { .. }
        | Run::FootnoteRefMark
        | Run::CommentReference { .. }
        | Run::OmmlInline(_) => {}
    }
}

fn field_owner(mode: FieldMode) -> FieldOwner {
    match mode {
        FieldMode::Static => FieldOwner::Typst,
        FieldMode::Live => FieldOwner::Consumer,
    }
}

#[allow(clippy::too_many_arguments)]
fn record_font_inventory(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    defaults: &TextDefaults,
    heading_styles: &[HeadingStyle],
    uses_math: bool,
    body: &[Block],
    headers: &[HdrFtrPart],
    footers: &[HdrFtrPart],
    footnotes: &[Footnote],
) {
    if let Some(font) = &defaults.font {
        record_font(report, snapshot_id, book, font);
    }
    for style in heading_styles {
        record_run_props_font(report, snapshot_id, book, &style.rpr);
    }
    if uses_math {
        record_font(report, snapshot_id, book, "Cambria Math");
    }
    record_block_fonts(report, snapshot_id, book, body);
    for part in headers.iter().chain(footers) {
        record_block_fonts(report, snapshot_id, book, &part.blocks);
    }
    for footnote in footnotes {
        record_block_fonts(report, snapshot_id, book, &footnote.blocks);
    }
}

fn record_block_fonts(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    blocks: &[Block],
) {
    for block in blocks {
        match block {
            Block::WeakPageBreak => {}
            Block::Para(para) => record_para_fonts(report, snapshot_id, book, para),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        record_block_fonts(report, snapshot_id, book, &cell.blocks);
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &toc.entries {
                    record_para_fonts(report, snapshot_id, book, entry);
                }
                for run in &toc.fallback {
                    record_run_fonts(report, snapshot_id, book, run);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn record_para_fonts(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    para: &Para,
) {
    for child in &para.content {
        match child {
            ParaChild::Run(run) => record_run_fonts(report, snapshot_id, book, run),
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    record_run_fonts(report, snapshot_id, book, run);
                }
            }
            ParaChild::OmmlPara(_)
            | ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::CommentRangeStart { .. }
            | ParaChild::CommentRangeEnd { .. }
            | ParaChild::Tag(_) => {}
        }
    }
}

fn record_run_fonts(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    run: &Run,
) {
    match run {
        Run::Text { props, .. }
        | Run::FootnoteRef { props, .. }
        | Run::CommentReference { props, .. } => {
            record_run_props_font(report, snapshot_id, book, props);
        }
        Run::Field(field) => {
            for result in &field.result {
                record_run_fonts(report, snapshot_id, book, result);
            }
        }
        Run::Drawing(drawing) => {
            if let Some(text_box) =
                drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
            {
                record_block_fonts(report, snapshot_id, book, &text_box.blocks);
            }
            if let Some(group) = &drawing.group {
                for child in &group.children {
                    if let Some(text_box) = &child.shape.txbx {
                        record_block_fonts(report, snapshot_id, book, &text_box.blocks);
                    }
                }
            }
        }
        Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::Tab
        | Run::FillTab
        | Run::FootnoteRefMark
        | Run::OmmlInline(_) => {}
    }
}

fn record_run_props_font(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    props: &RunProps,
) {
    if let Some(font) = &props.font {
        record_font(report, snapshot_id, book, font);
    }
}

fn record_font(
    report: &mut FidelityReport,
    snapshot_id: u128,
    book: &typst_library::text::FontBook,
    family: &str,
) {
    report.record_font(snapshot_id, family, book.contains_family(&family.to_lowercase()));
}

fn collect_embeddable_fonts(
    facts: &[crate::report::FontFact],
    book: &FontBook,
    mut load: impl FnMut(usize) -> Option<typst_library::text::Font>,
) -> Vec<EmbeddedFontProgram> {
    let targets = [
        (
            EmbeddedFontStyle::Regular,
            FontVariant::new(FontStyle::Normal, FontWeight::REGULAR, FontStretch::NORMAL),
        ),
        (
            EmbeddedFontStyle::Bold,
            FontVariant::new(FontStyle::Normal, FontWeight::BOLD, FontStretch::NORMAL),
        ),
        (
            EmbeddedFontStyle::Italic,
            FontVariant::new(FontStyle::Italic, FontWeight::REGULAR, FontStretch::NORMAL),
        ),
        (
            EmbeddedFontStyle::BoldItalic,
            FontVariant::new(FontStyle::Italic, FontWeight::BOLD, FontStretch::NORMAL),
        ),
    ];
    let mut programs = Vec::new();
    for fact in facts.iter().filter(|fact| fact.available_at_export) {
        let family = fact.family.to_lowercase();
        for (style, target) in targets {
            let Some(index) = book.select(&family, target) else { continue };
            let Some(info) = book.info(index) else { continue };
            if embedded_font_style(info) != style {
                // Do not put a synthetic fallback face into a distinct Word
                // style slot. Word can synthesize that style from the regular
                // embedded face more accurately than a mislabeled program.
                continue;
            }
            let Some(font) = load(index) else { continue };
            let data = font.data().as_slice();
            if data.len() < 32 || ttf_parser::fonts_in_collection(data).is_some() {
                // Word's obfuscated font part is a single TrueType/OpenType
                // program. Collections need face extraction, which we do not
                // yet perform, so retain the declared portable reference.
                continue;
            }
            let Ok(face) = ttf_parser::Face::parse(data, font.index()) else { continue };
            let Some(os2) = face.tables().os2 else { continue };
            if !os2.is_outline_embedding_allowed()
                || !matches!(
                    os2.permissions(),
                    Some(
                        ttf_parser::Permissions::Installable
                            | ttf_parser::Permissions::Editable
                    )
                )
            {
                // Restricted fonts must never be embedded. Preview-and-print
                // rights are also skipped because the DOCX exporter promises
                // an editable document rather than a read-only artifact.
                continue;
            }
            programs.push(EmbeddedFontProgram {
                family: fact.family.clone(),
                style,
                data: data.to_vec(),
            });
        }
    }
    programs
}

fn embedded_font_style(info: &FontInfo) -> EmbeddedFontStyle {
    let bold = info.variant.weight.to_number() >= FontWeight::SEMIBOLD.to_number();
    let italic = info.variant.style != FontStyle::Normal;
    match (bold, italic) {
        (false, false) => EmbeddedFontStyle::Regular,
        (true, false) => EmbeddedFontStyle::Bold,
        (false, true) => EmbeddedFontStyle::Italic,
        (true, true) => EmbeddedFontStyle::BoldItalic,
    }
}

fn record_drawing_inventory(
    report: &mut FidelityReport,
    snapshot_id: u128,
    body: &[Block],
    headers: &[HdrFtrPart],
    footers: &[HdrFtrPart],
    footnotes: &[Footnote],
) {
    record_block_drawings(report, snapshot_id, body);
    for part in headers.iter().chain(footers) {
        record_block_drawings(report, snapshot_id, &part.blocks);
    }
    for footnote in footnotes {
        record_block_drawings(report, snapshot_id, &footnote.blocks);
    }
}

fn record_block_drawings(
    report: &mut FidelityReport,
    snapshot_id: u128,
    blocks: &[Block],
) {
    for block in blocks {
        match block {
            Block::WeakPageBreak => {}
            Block::Para(para) => record_para_drawings(report, snapshot_id, para),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        record_block_drawings(report, snapshot_id, &cell.blocks);
                    }
                }
            }
            Block::Toc(toc) => {
                for entry in &toc.entries {
                    record_para_drawings(report, snapshot_id, entry);
                }
                for run in &toc.fallback {
                    record_run_drawings(report, snapshot_id, run);
                }
            }
            Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => {}
        }
    }
}

fn record_para_drawings(report: &mut FidelityReport, snapshot_id: u128, para: &Para) {
    for child in &para.content {
        match child {
            ParaChild::Run(run) => record_run_drawings(report, snapshot_id, run),
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    record_run_drawings(report, snapshot_id, run);
                }
            }
            ParaChild::OmmlPara(_)
            | ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::CommentRangeStart { .. }
            | ParaChild::CommentRangeEnd { .. }
            | ParaChild::Tag(_) => {}
        }
    }
}

fn record_run_drawings(report: &mut FidelityReport, snapshot_id: u128, run: &Run) {
    match run {
        Run::Drawing(drawing) => {
            report.record_drawing(
                snapshot_id,
                drawing.docpr_id,
                &drawing.name,
                drawing.alt.as_deref(),
                drawing.decorative,
                drawing.has_native_text(),
            );
            if let Some(text_box) =
                drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref())
            {
                record_block_drawings(report, snapshot_id, &text_box.blocks);
            }
            if let Some(group) = &drawing.group {
                for child in &group.children {
                    if let Some(text_box) = &child.shape.txbx {
                        record_block_drawings(report, snapshot_id, &text_box.blocks);
                    }
                }
            }
        }
        Run::Field(field) => {
            for result in &field.result {
                record_run_drawings(report, snapshot_id, result);
            }
        }
        Run::Text { .. }
        | Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::Tab
        | Run::FillTab
        | Run::FootnoteRef { .. }
        | Run::FootnoteRefMark
        | Run::CommentReference { .. }
        | Run::OmmlInline(_) => {}
    }
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
            num_id: None,
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
            visit_para_run_props(para, &mut |props| {
                strip_text_defaults_below_heading(props, defaults, &style.rpr)
            });
        } else {
            visit_para_run_props(para, &mut |props| strip_text_defaults(props, defaults));
        }
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
    if !props.preserve_color && props.color == style.color {
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
    if !props.preserve_color && props.color == defaults.color {
        props.color = None;
    }
    if props.lang == defaults.lang {
        props.lang = None;
    }
}

/// Strips Normal/docDefaults only where HeadingN does not define the same
/// property. A surviving direct value is a deviation from HeadingN and must not
/// disappear merely because it happens to equal Normal: doing so changes the
/// effective value back to HeadingN's property in Word's inheritance cascade.
fn strip_text_defaults_below_heading(
    props: &mut RunProps,
    defaults: &TextDefaults,
    heading: &RunProps,
) {
    if heading.font.is_none() && props.font == defaults.font {
        props.font = None;
    }
    if heading.size_half_pt.is_none() && props.size_half_pt == Some(defaults.size_half_pt)
    {
        props.size_half_pt = None;
    }
    if !props.preserve_color && heading.color.is_none() && props.color == defaults.color {
        props.color = None;
    }
    if heading.lang.is_none() && props.lang == defaults.lang {
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
            Block::WeakPageBreak => {}
            Block::Para(para) => vote_para(votes, para),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        collect_default_votes(&cell.blocks, votes);
                    }
                }
            }
            Block::FlowSpace { .. }
            | Block::Toc(_)
            | Block::SectionBreak(_)
            | Block::Tag(_) => {}
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
    /// Whole-page vertical alignment when the page run has one common resolved
    /// `align(..)` style.
    vertical_align: Option<VAlign>,
    /// Section-level line numbering derived from `par.line(numbering:)`.
    line_numbers: Option<LineNumbering>,
    /// `set page(numbering:)`, if any (drives `pgNumType` + the PAGE field).
    numbering: Option<typst_library::model::Numbering>,
    /// An explicit page-counter restart in this run — `counter(page).update(n)`
    /// → `w:pgNumType/@w:start`. A restart is an intentional section boundary
    /// (a thesis's roman front matter then an arabic body restarting at 1), so
    /// [`same_section`] compares it: two otherwise-identical runs that restart
    /// the page number are genuinely different sections and must not merge.
    page_num_start: Option<i64>,
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

/// Installs one section's geometry as the current lowering region. This must be
/// updated before lowering each section: native tables/stacks and fallback
/// layout now share this budget, so leaving the first section installed would
/// size a landscape appendix against the front matter.
fn set_ctx_geometry(ctx: &mut DocxCtx, geom: &SectGeom) {
    let content_twip = geom.page_w - geom.margin_left - geom.margin_right;
    if content_twip > 0 {
        ctx.page_content_width = Abs::pt(content_twip as f64 / 20.0);
        let columns = geom.columns.max(1) as i32;
        let total_gutter = geom.col_space.max(0) * (columns - 1);
        let column_twip = ((content_twip - total_gutter).max(columns)) / columns;
        ctx.available_width = Abs::pt(column_twip as f64 / 20.0);
    }
    let content_height = geom.page_h - geom.margin_top - geom.margin_bottom;
    if content_height > 0 {
        ctx.available_height = Abs::pt(content_height as f64 / 20.0);
        // Top-level flow resolves `height: 100%` against the text area, same
        // as Typst's own page region; cells override this with their measured
        // row box (`with_shape_height_base`).
        ctx.shape_height_base = Some(ctx.available_height);
    }
    if geom.page_h > 0 {
        ctx.raster_height = Abs::pt(geom.page_h as f64 / 20.0);
    }
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
    /// Page breaks beyond the one consumed by the preceding section boundary.
    /// These must render inside this section so consecutive explicit breaks do
    /// not collapse multiple requested blank pages into one.
    leading_pagebreaks: usize,
}

fn resolve_sections(
    pairs: &[(&Content, StyleChain)],
    initial: StyleChain,
    real: Option<&typst_layout::PagedIntrospector>,
    page_sizes: Option<&[typst_library::layout::Size]>,
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
        let break_start = i;
        let mut hard_breaks = 0usize;
        while i < pairs.len() {
            if let Some(pb) = pairs[i].0.to_packed::<PagebreakElem>() {
                if !pb.boundary.get(pairs[i].1) {
                    initial = pairs[i].1;
                }
                if !pb.weak.get(pairs[i].1) && !pb.boundary.get(pairs[i].1) {
                    hard_breaks += 1;
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
        let skipped_breaks = i - break_start;
        // `#columns(..)` ends with an empty restore-to-base section. If an
        // explicit page break follows immediately, that break becomes the
        // leading child of the restore section and `convert_children` correctly
        // discards it as section-initial setup. Carry the page transition on
        // the section boundary instead: the column section ends with
        // `nextPage` (or the requested parity), and only further consecutive
        // breaks remain as real blank pages in the restored section.
        let restored_after_columns = skipped_breaks > 0
            && sections.len() >= 2
            && sections.last().is_some_and(|section| section.range.is_empty())
            && matches!(
                sections[sections.len() - 2].break_after,
                Some(SectType::Continuous)
            );
        if restored_after_columns {
            let last = sections.len() - 1;
            sections[last - 1].break_after =
                Some(forced_break.unwrap_or(SectType::NextPage));
            // Weak/boundary breaks in the run collapse into the section
            // transition itself; only surplus *hard* breaks are real blank
            // pages.
            sections[last].leading_pagebreaks =
                skipped_breaks.saturating_sub(1).min(hard_breaks);
        } else if let Some(sect_type) = forced_break
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
        let previous_sections = sections.len();
        if group.iter().any(|(child, _)| child.is::<ColumnsElem>()) {
            push_column_sections(
                &mut sections,
                pairs,
                start..i,
                initial,
                real,
                page_sizes,
            );
        } else {
            let geom = run_geometry(group, initial, real, page_sizes);
            // Merge into the previous section if nothing section-scoped changed;
            // the pagebreaks between them then fall inside the merged range
            // (→ `<w:br>`).
            push_section_run(&mut sections, geom, start..i, None, false);
        }
        if previous_sections > 0 && sections.len() > previous_sections {
            // A page-style transition contributes one synthetic pagebreak, and
            // the new Word section itself replaces the first actual break. Any
            // further consecutive breaks are real blank pages in the new run.
            sections[previous_sections].leading_pagebreaks =
                skipped_breaks.saturating_sub(2).min(hard_breaks);
        }
    }
    merge_content_empty_sections(pairs, &mut sections);
    sections
}

/// Drops section boundaries that wrap nothing but invisible marker content
/// (`TagElem`s, with no paragraph/table/drawing in between). A `set page(..)`
/// wrapped in a `#context` block — the standard idiom for a template's
/// top-level `show: doc => {..}` rule that establishes page geometry before
/// laying out the real body — makes Typst insert a *boundary* pagebreak right
/// where the new page style takes effect, before anything has been drawn.
/// `resolve_sections` still (correctly, per its own geometry diff) sees a
/// page-style change there and gives it its own section — but a Word section
/// transition costs a full page even when the section it closes has zero
/// paragraphs, so the document opens on a blank page. Absorb such
/// content-empty sections into a neighbouring section instead of giving them
/// their own transition; the marker pairs still get walked as part of the
/// neighbour's range (mirroring how two same-geometry sections above already
/// merge across a skipped boundary pagebreak).
fn merge_content_empty_sections(
    pairs: &[(&Content, StyleChain)],
    sections: &mut Vec<SectionRun>,
) {
    let is_content_empty = |range: std::ops::Range<usize>| {
        !range.is_empty()
            && pairs[range]
                .iter()
                .all(|(child, _)| child.is::<typst_library::introspection::TagElem>())
    };
    let mut idx = 0;
    while idx < sections.len() {
        let section = &sections[idx];
        if section.break_after.is_some()
            || section.leading_pagebreaks != 0
            || !is_content_empty(section.range.clone())
        {
            idx += 1;
            continue;
        }
        let range = sections[idx].range.clone();
        if idx + 1 < sections.len() {
            sections[idx + 1].range.start = range.start;
            sections.remove(idx);
        } else if idx > 0 {
            sections[idx - 1].range.end = range.end;
            sections.remove(idx);
        } else {
            // The whole document is content-empty; nothing to merge into.
            idx += 1;
        }
    }
}

fn push_column_sections(
    sections: &mut Vec<SectionRun>,
    pairs: &[(&Content, StyleChain)],
    range: std::ops::Range<usize>,
    initial: StyleChain,
    real: Option<&typst_layout::PagedIntrospector>,
    page_sizes: Option<&[typst_library::layout::Size]>,
) {
    use typst_library::layout::ColumnsElem;

    let base_geom = run_geometry(&pairs[range.clone()], initial, real, page_sizes);
    let mut segment_start = range.start;
    let mut saw_columns = false;
    for i in range.clone() {
        let Some(columns) = pairs[i].0.to_packed::<ColumnsElem>() else {
            continue;
        };
        if crate::mappers::columns::uses_table(columns, pairs[i].1) {
            continue;
        }

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
    // A page-number restart (`geom.page_num_start.is_some()`) always begins a
    // new section — that is the whole reason it is a boundary. A run with *no*
    // restart continues the previous section's numbering, so it may still
    // merge into an otherwise-identical predecessor even if that predecessor
    // itself restarted: the continuation belongs to the section it extends
    // (see `same_section`, which no longer keys on the start for exactly this
    // reason — a symmetric equality there would wrongly split every
    // no-restart continuation off from the restart it follows).
    if let Some(last) = sections.last_mut()
        && last.break_after.is_none()
        && geom.page_num_start.is_none()
        && same_section(&last.geom, &geom)
    {
        last.range.end = range.end;
        last.break_after = break_after;
        return;
    }

    if range.is_empty() && !allow_empty {
        return;
    }

    sections.push(SectionRun { geom, range, break_after, leading_pagebreaks: 0 });
}

fn close_previous_section_at(
    sections: &mut [SectionRun],
    geom: &SectGeom,
    boundary: usize,
) {
    if let Some(last) = sections.last_mut()
        && last.break_after.is_none()
        && geom.page_num_start.is_none()
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
/// `background_color`, `vertical_align`, and `hyphenate` are deliberately NOT
/// compared. They are represented when another property already creates a
/// genuine Word section; making every Typst page (especially every slide) a
/// section can amplify ordinary reflow into dozens of extra pages.
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
///
/// `real`/`page_sizes` (the true converged paged layout and its per-page
/// frame sizes — see [`real_section_size`]) are consulted only when a page
/// axis is `auto`; both are `None` unless the hybrid paged-introspector
/// entry point (`docx_document_with_paged_introspector`) was used.
fn run_geometry(
    group: &[(&Content, StyleChain)],
    initial: StyleChain,
    real: Option<&typst_layout::PagedIntrospector>,
    page_sizes: Option<&[typst_library::layout::Size]>,
) -> SectGeom {
    use typst_library::foundations::{Resolve, Smart, Styles};
    use typst_library::introspection::{CounterUpdateElem, Tag, TagElem};
    use typst_library::layout::{
        Abs, AlignElem, Binding, Dir, Em, FixAlignment, FixedAlignment, Length,
        OuterVAlignment, PageElem, Paper, Rel, Sides, Size,
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

    // `auto` page axes have no DOCX equivalent (pages are fixed-size). Before
    // falling back to a hardcoded default, try the TRUE content-driven size
    // Typst's own paged layout already computed for this section's real
    // page(s) — an extremely common idiom (`set page(width: .., height:
    // auto)` for tickets/certificates/single-page diagrams/etc., found in a
    // majority of the real-world corpus) otherwise gets silently clipped or
    // misshapen to a fixed A4 axis instead of the size the author actually
    // designed for.
    if (!size.x.is_finite() || !size.y.is_finite())
        && let Some(real_size) = real_section_size(group, real, page_sizes)
    {
        if !size.x.is_finite() {
            size.x = real_size.x;
        }
        if !size.y.is_finite() {
            size.y = real_size.y;
        }
    }

    // The auto-margin reference is the smaller physical dimension.
    let mut minside = size.x.min(size.y);
    if !minside.is_finite() {
        minside = Paper::A4.width();
    }
    // Still-unresolved (no real page data available, e.g. `docx_document`'s
    // non-hybrid entry point) axes fall back to the A4 dimension.
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
    let mut body_alignments = group
        .iter()
        .filter(|(child, _)| !child.is::<typst_library::introspection::TagElem>())
        .map(|(_, pair_styles)| pair_styles.resolve(AlignElem::alignment).y);
    let vertical_align = body_alignments.next().and_then(|first| {
        body_alignments
            .all(|alignment| alignment == first)
            .then_some(match first {
                FixedAlignment::Center => Some(VAlign::Center),
                FixedAlignment::End => Some(VAlign::Bottom),
                FixedAlignment::Start => None,
            })?
    });

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
    // An explicit `counter(page).update(n)` at the top of this run is a page-
    // number restart — the exporter's counterpart of Word's `w:pgNumType`
    // `@w:start`. Only a `Set` to a literal is a restart worth a section
    // boundary; a `Step` or a closure `Func` is ordinary counting, not the
    // "this section renumbers from N" intent, so those are left alone. The
    // value is read straight from the realized element with no engine, exactly
    // as the page-counter frame walk in `introspection::counter` reads it.
    // The update reaches the realized flow wrapped in the introspection
    // `TagElem` that carries it (it never appears as a bare child), so we look
    // through the tag's start element — exactly as the page-counter frame walk
    // in `introspection::counter` does.
    let page_num_start = group.iter().find_map(|(child, _)| {
        let Tag::Start(elem, _) = &child.to_packed::<TagElem>()?.tag else {
            return None;
        };
        elem.to_packed::<CounterUpdateElem>()?.page_number_reset().map(|n| n as i64)
    });
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
        vertical_align,
        line_numbers,
        numbering,
        page_num_start,
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

/// Looks up the TRUE size Typst's real paged layout computed for the real
/// page(s) this section's content group spans, by finding any locatable
/// content in the group and asking `real` which physical page it landed on.
/// Word requires ONE fixed size per section, so when the group spans more
/// than one real page (an explicit `#pagebreak()` inside an otherwise
/// unchanged `set page(..)` run) — or when different pages disagree because
/// an auto axis genuinely varied with content — the maximum across all pages
/// touched is used, so no page's content is clipped by an undersized guess.
/// Returns `None` when there's no real data at all (no hybrid paged
/// introspector, or no locatable content found in the group).
fn real_section_size(
    group: &[(&Content, StyleChain)],
    real: Option<&typst_layout::PagedIntrospector>,
    page_sizes: Option<&[typst_library::layout::Size]>,
) -> Option<typst_library::layout::Size> {
    use typst_library::layout::Size;

    let real = real?;
    let page_sizes = page_sizes?;
    let mut max: Option<Size> = None;
    for (child, _) in group {
        let Some(loc) = child.location() else { continue };
        let Some(page) = real.page(loc) else { continue };
        let Some(&size) = page_sizes.get(page.get() - 1) else { continue };
        max = Some(match max {
            Some(m) => Size::new(m.x.max(size.x), m.y.max(size.y)),
            None => size,
        });
    }
    max
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
        vertical_align: geom.vertical_align,
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

    // Page numbering → pgNumType: the glyph format from `set page(numbering:)`
    // (which also drives a PAGE field in the auto band), and/or a restart
    // value from `counter(page).update(n)`. Either alone is enough to warrant
    // the element — a section can restart the page number without changing its
    // format (a thesis body restarting at arabic 1), in which case the format
    // is Word's default decimal.
    if geom.numbering.is_some() || geom.page_num_start.is_some() {
        let fmt = geom.numbering.as_ref().map(|n| numbering_fmt(ctx, n)).unwrap_or("decimal");
        sect.pg_num = Some(PgNumType { fmt, start: geom.page_num_start });
    }

    // -- Header content + page background/foreground -----------------------
    // A `set page(background:)` image is emitted as a full-page `behindDoc`
    // page-anchored drawing at the *top* of the (default) header, so it repeats
    // on every page behind the body text — the Word idiom for a page background /
    // watermark. Foreground uses the same page-anchored mechanism with
    // `behindDoc="0"`, so it overlays the body text. These drawings and the
    // explicit header share ONE part so their image relationships live in a
    // single `headerN.xml.rels` (no rId collision).
    if geom.header.is_some()
        || geom.background.is_some()
        || geom.background_color.is_some()
        || geom.foreground.is_some()
    {
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
            FurnitureSource {
                content: Some(content),
                background: None,
                foreground: None,
            },
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

struct FurnitureRefPlan {
    kind: &'static str,
    lowered: LoweredFurniture,
}

/// Whole-region representation selected before header/footer parts are
/// serialized. Word can express one first-page value plus stable odd/even
/// values. Anything more page-specific must be admitted as an approximation,
/// never silently mislabeled as an exact parity split.
enum FurniturePlan {
    Exact { title_page: bool, refs: Vec<FurnitureRefPlan> },
    Sampled { first: LoweredFurniture },
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
    match preflight_furniture(ctx, slot, geom, source, styles)? {
        FurniturePlan::Exact { title_page, refs } => {
            sect.title_pg |= title_page;
            if let Some(height) = uniform_single_line_furniture_height(&refs) {
                adjust_furniture_band(sect, slot, height);
            }
            for planned in refs {
                emit_furniture(ctx, sect, parts, slot, planned.kind, planned.lowered);
            }
        }
        FurniturePlan::Sampled { first } => {
            if let Some(height) = furniture_content_height(&first.blocks) {
                adjust_furniture_band(sect, slot, height);
            }
            let affected_text_chars = blocks_text_chars(&first.blocks);
            record_sampled_furniture(ctx, slot, source, affected_text_chars);
            emit_furniture(ctx, sect, parts, slot, "default", first);
        }
    }
    Ok(())
}

/// Typst places a simple header at the bottom of its marginal band and a
/// simple footer at the top of its band. Word's `w:header`/`w:footer` measure
/// from the page edge to the start of the content instead. For a single native
/// line, translate between those origins by subtracting the authored line
/// height from the band boundary. Complex/multiline furniture retains the
/// conservative boundary because its laid-out extent is not represented here.
fn adjust_furniture_band(sect: &mut SectPr, slot: FurnitureSlot, height: i32) {
    let (band, margin) = if slot.is_header() {
        (&mut sect.header, sect.margin_top)
    } else {
        (&mut sect.footer, sect.margin_bottom)
    };
    *band = band.saturating_sub(height).clamp(1, margin.saturating_sub(1).max(1));
}

/// The band distance is one section-wide property shared by every ref (e.g.
/// the title-page and default headers), so every ref that actually renders
/// something must agree on the same measured height before trusting it. A
/// ref with NO visible content (the common `context(if here().page() >= 2
/// [..])` idiom, whose title-page/first-page sample is empty because the
/// header is deliberately suppressed there) has no laid-out extent to
/// disagree with — it never occupies band space — so it is skipped rather
/// than forcing the whole computation to bail just because it isn't a
/// content shape `furniture_content_height` can measure.
fn uniform_single_line_furniture_height(refs: &[FurnitureRefPlan]) -> Option<i32> {
    let mut heights = refs
        .iter()
        .filter(|planned| !furniture_blocks_are_empty(&planned.lowered.blocks))
        .map(|planned| furniture_content_height(&planned.lowered.blocks));
    let first = heights.next()??;
    heights.all(|height| height == Some(first)).then_some(first)
}

fn furniture_blocks_are_empty(blocks: &[Block]) -> bool {
    blocks.iter().all(|block| matches!(block, Block::Tag(_)))
}

/// Measures a furniture region's (header/footer) laid-out height so
/// `adjust_furniture_band` can translate Typst's "band bottom" origin to
/// Word's "band top" origin. Two shapes are understood; anything else keeps
/// the conservative full-margin boundary (see `adjust_furniture_band`'s doc
/// comment) because its laid-out extent isn't represented here.
fn furniture_content_height(blocks: &[Block]) -> Option<i32> {
    let serialized: Vec<&Block> =
        blocks.iter().filter(|block| !matches!(block, Block::Tag(_))).collect();
    match serialized[..] {
        [Block::Para(_)] => single_line_furniture_height(blocks),
        // A one-row table (a common "name/title cell + logo cell" header
        // idiom) followed by a plain trailing paragraph (typically a bare
        // `line()` rule, `w:pBdr` only, no text) — the table's row already
        // carries its true measured height from `ctx.paged_geometry`
        // (`mappers::table::cellgrid`, the same measurement every body
        // table gets), which single-line font-size heuristics can't
        // reach. The trailing paragraph's own contribution is folded in via
        // `single_line_furniture_height` when it carries text, or ignored
        // (an empty `w:pBdr`-only rule paragraph's line height is small
        // relative to the table row, so omitting it only *under*-shrinks
        // the band — safe, unlike overshrinking, which would overlap body
        // content onto the furniture).
        [Block::Table(tbl), ref rest @ ..] if rest.len() <= 1 => {
            let mut height = tbl
                .rows
                .iter()
                .map(|row| row.height.map(|h| h.val))
                .sum::<Option<i32>>()?;
            if let [trailing_para @ Block::Para(_)] = rest
                && let Some(trailing) =
                    single_line_furniture_height(std::slice::from_ref(trailing_para))
            {
                height += trailing;
            }
            Some(height)
        }
        _ => None,
    }
}

fn single_line_furniture_height(blocks: &[Block]) -> Option<i32> {
    let mut serialized = blocks.iter().filter(|block| !matches!(block, Block::Tag(_)));
    let Block::Para(para) = serialized.next()? else { return None };
    if serialized.next().is_some() {
        return None;
    }

    let mut max_half_points = 0;
    for child in &para.content {
        match child {
            ParaChild::Run(run) => {
                accumulate_single_line_run_size(run, &mut max_half_points)?;
            }
            ParaChild::Hyperlink { runs, .. } => {
                for run in runs {
                    accumulate_single_line_run_size(run, &mut max_half_points)?;
                }
            }
            ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::CommentRangeStart { .. }
            | ParaChild::CommentRangeEnd { .. }
            | ParaChild::Tag(_) => {}
            ParaChild::OmmlPara(_) => return None,
        }
    }
    (max_half_points > 0).then_some(max_half_points as i32 * 10)
}

fn accumulate_single_line_run_size(run: &Run, maximum: &mut u32) -> Option<()> {
    match run {
        Run::Text { props, .. } | Run::FootnoteRef { props, .. } => {
            *maximum = (*maximum).max(props.size_half_pt.unwrap_or(0));
        }
        // Invisible in the flow (no `w:t`), so it neither contributes to nor
        // disqualifies a single-line height measurement — like a bare tab.
        Run::Tab | Run::FillTab | Run::CommentReference { .. } => {}
        Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::FootnoteRefMark
        | Run::Drawing(_)
        | Run::OmmlInline(_)
        | Run::Field(_) => return None,
    }
    Some(())
}

fn preflight_furniture(
    ctx: &mut DocxCtx,
    slot: FurnitureSlot,
    geom: &SectGeom,
    source: FurnitureSource<'_>,
    styles: StyleChain,
) -> SourceResult<FurniturePlan> {
    let context_sensitive = source.content.is_some_and(contains_context)
        || source.background.is_some_and(contains_context)
        || source.foreground.is_some_and(contains_context);

    let first = lower_furniture(ctx, slot, geom, source, styles, 1)?;
    if !context_sensitive {
        return Ok(FurniturePlan::Exact {
            title_page: false,
            refs: vec![FurnitureRefPlan { kind: "default", lowered: first }],
        });
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
        return Ok(FurniturePlan::Sampled { first });
    }

    let needs_even = even.signature != odd.signature;
    let needs_first = first.signature != odd.signature;

    if !needs_even && !needs_first {
        return Ok(FurniturePlan::Exact {
            title_page: false,
            refs: vec![FurnitureRefPlan { kind: "default", lowered: first }],
        });
    }

    let refs = match (needs_first, needs_even) {
        (true, true) => vec![
            FurnitureRefPlan { kind: "first", lowered: first },
            FurnitureRefPlan { kind: "even", lowered: even },
            FurnitureRefPlan { kind: "default", lowered: odd },
        ],
        (true, false) => vec![
            FurnitureRefPlan { kind: "first", lowered: first },
            FurnitureRefPlan { kind: "default", lowered: odd },
        ],
        (false, true) => vec![
            FurnitureRefPlan { kind: "even", lowered: even },
            FurnitureRefPlan { kind: "default", lowered: first },
        ],
        (false, false) => unreachable!(),
    };
    Ok(FurniturePlan::Exact { title_page: needs_first, refs })
}

fn record_sampled_furniture(
    ctx: &mut DocxCtx,
    slot: FurnitureSlot,
    source: FurnitureSource<'_>,
    sampled_text_chars: usize,
) {
    let contextual = [source.content, source.background, source.foreground]
        .into_iter()
        .flatten()
        .filter(|content| contains_context(content));
    let mut warning_span = None;
    let mut sampled_chars_unattributed = sampled_text_chars;
    for content in contextual {
        warning_span.get_or_insert(content.span());
        let source_chars = content_text_chars(content);
        let affected_text_chars = if source_chars == 0 {
            std::mem::take(&mut sampled_chars_unattributed)
        } else {
            sampled_chars_unattributed =
                sampled_chars_unattributed.saturating_sub(source_chars);
            source_chars
        };
        ctx.record_content_decision(
            content,
            Representation::Approximate,
            DecisionReason::PageFurnitureSampled,
            LossSet::PAGE_FURNITURE_SAMPLED,
            affected_text_chars,
        );
    }

    if let Some(span) = warning_span {
        let kind = if slot.is_header() { "header" } else { "footer" };
        ctx.warn_message(
            format!(
                "page-varying {kind} cannot be represented by Word's first/even/default model; the page 1 value will repeat"
            ),
            span,
        );
    }
}

fn content_text_chars(content: &Content) -> usize {
    use std::ops::ControlFlow;
    use typst_library::text::TextElem;

    let mut chars = 0;
    let _ = content.traverse(&mut |element: Content| {
        if let Some(text) = element.to_packed::<TextElem>() {
            chars += text.text.chars().count();
        }
        ControlFlow::<()>::Continue(())
    });
    chars
}

fn blocks_text_chars(blocks: &[Block]) -> usize {
    blocks.iter().map(block_text_chars).sum()
}

fn block_text_chars(block: &Block) -> usize {
    match block {
        Block::WeakPageBreak => 0,
        Block::Para(para) => para.content.iter().map(para_child_text_chars).sum(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .map(|cell| blocks_text_chars(&cell.blocks))
            .sum(),
        Block::Toc(toc) => {
            let entries: usize = toc
                .entries
                .iter()
                .flat_map(|para| &para.content)
                .map(para_child_text_chars)
                .sum();
            entries + toc.fallback.iter().map(run_text_chars).sum::<usize>()
        }
        Block::FlowSpace { .. } | Block::SectionBreak(_) | Block::Tag(_) => 0,
    }
}

fn para_child_text_chars(child: &ParaChild) -> usize {
    match child {
        ParaChild::Run(run) => run_text_chars(run),
        ParaChild::Hyperlink { runs, .. } => runs.iter().map(run_text_chars).sum(),
        ParaChild::OmmlPara(_)
        | ParaChild::BookmarkStart { .. }
        | ParaChild::BookmarkEnd { .. }
        | ParaChild::CommentRangeStart { .. }
        | ParaChild::CommentRangeEnd { .. }
        | ParaChild::Tag(_) => 0,
    }
}

fn run_text_chars(run: &Run) -> usize {
    match run {
        Run::Text { text, .. } => text.chars().count(),
        Run::Field(field) => field.result.iter().map(run_text_chars).sum(),
        Run::Drawing(drawing) => {
            let shape = drawing
                .shape
                .as_ref()
                .and_then(|shape| shape.txbx.as_ref())
                .map_or(0, |text_box| blocks_text_chars(&text_box.blocks));
            let group = drawing.group.as_ref().map_or(0, |group| {
                group
                    .children
                    .iter()
                    .filter_map(|child| child.shape.txbx.as_ref())
                    .map(|text_box| blocks_text_chars(&text_box.blocks))
                    .sum()
            });
            shape + group
        }
        Run::Break { .. }
        | Run::PageBreak
        | Run::ColumnBreak
        | Run::Tab
        | Run::FillTab
        | Run::FootnoteRef { .. }
        | Run::FootnoteRefMark
        | Run::CommentReference { .. }
        | Run::OmmlInline(_) => 0,
    }
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
        // The part's own relationship table has to be reinstated even when the
        // lowering fails, or every later `r:id` in the body would be filed
        // against a table nobody writes (same shape as `DocxCtx::part_blocks`).
        let saved = ctx.part_rels.take();
        ctx.part_rels = Some(crate::package::Rels::new());
        let outcome = lower_furniture_blocks(ctx, slot, geom, source, styles);
        let rels = ctx.part_rels.take().unwrap_or_default();
        ctx.part_rels = saved;

        let blocks = outcome?;
        let signature = furniture_signature(&blocks);
        Ok(LoweredFurniture {
            blocks,
            rels,
            signature,
            emit_empty: source.content.is_some(),
        })
    })
}

fn lower_furniture_blocks(
    ctx: &mut DocxCtx,
    slot: FurnitureSlot,
    geom: &SectGeom,
    source: FurnitureSource<'_>,
    styles: StyleChain,
) -> SourceResult<Vec<Block>> {
    let mut blocks = Vec::new();

    let mut has_background_overlay = false;
    if slot.is_header()
        && let Some(bg) = source.background
        && let Some(block) =
            page_overlay_block(ctx, bg, geom, styles, true, "Background")?
    {
        blocks.push(block);
        has_background_overlay = true;
    }

    // A successful background raster already includes the solid page fill.
    // Keeping a separate compatibility rectangle makes LibreOffice paint it
    // over the raster because it reverses the two drawings' z-order.
    if slot.is_header()
        && !has_background_overlay
        && let Some(color) = geom.background_color
    {
        blocks.push(solid_page_fill_block(ctx, color, geom));
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
        // A contextual page-counter callback is evaluated while its inline
        // body is lowered, after the paragraph shell has been synthesized.
        // Capture its effective nested alignment and transfer it onto the
        // separate-part paragraph that actually owns the PAGE field.
        ctx.reset_page_counter_paragraph_alignment();
        let lowered = ctx.blocks(content, styles);
        let page_counter_jc = ctx.take_page_counter_paragraph_alignment();
        ctx.raster_height = saved_h;
        ctx.line_numbering_active = saved_line_numbering;
        let mut lowered = lowered?;
        if let Some(jc) = furniture_horizontal_alignment(content, styles) {
            for block in &mut lowered {
                if let Block::Para(para) = block
                    && para.props.jc.is_none()
                {
                    para.props.jc = Some(jc);
                }
            }
        } else if let Some(jc) = page_counter_jc {
            for block in &mut lowered {
                if let Block::Para(para) = block
                    && para.props.jc.is_none()
                    && para_has_page_field(para)
                {
                    para.props.jc = Some(jc);
                }
            }
        }
        blocks.extend(lowered);
    }

    if slot.is_header()
        && let Some(fg) = source.foreground
        && let Some(block) =
            page_overlay_block(ctx, fg, geom, styles, false, "Foreground")?
    {
        blocks.push(block);
    }

    crate::mappers::table::collapse_par_spacing(&mut blocks);

    Ok(blocks)
}

fn para_has_page_field(para: &Para) -> bool {
    para.content.iter().any(|child| {
        matches!(
            child,
            ParaChild::Run(Run::Field(field))
                if field.instr.split_whitespace().next() == Some("PAGE")
        )
    })
}

/// `AlignElem` is a block-level layout wrapper and is consumed while a
/// header/footer fragment is realized, before its synthesized paragraph sees
/// the wrapper's style chain. Preserve the wrapper's explicit horizontal
/// alignment on the resulting Word paragraphs.
fn furniture_horizontal_alignment(
    content: &Content,
    styles: StyleChain,
) -> Option<crate::dom::Jc> {
    use typst_library::layout::{AlignElem, HAlignment};

    let align = content.to_packed::<AlignElem>()?.alignment.get(styles);
    align.x().map(|value| match value {
        HAlignment::Start | HAlignment::Left => crate::dom::Jc::Start,
        HAlignment::Center => crate::dom::Jc::Center,
        HAlignment::Right | HAlignment::End => crate::dom::Jc::End,
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
        Block::WeakPageBreak => out.push_str("weakbr;"),
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
        Block::FlowSpace { dxa } => {
            let _ = write!(out, "space({dxa})");
        }
        Block::Toc(toc) => {
            let _ = write!(
                out,
                "toc(instr={},mode={:?},depth={:?},cat={:?},tab={})",
                toc.instr, toc.mode, toc.depth, toc.caption_category, toc.tab_pos
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
        ParaChild::CommentRangeStart { .. } | ParaChild::CommentRangeEnd { .. } => {}
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
        Run::Break { kind } => match kind {
            crate::dom::BreakKind::Authored => out.push_str("br;"),
            crate::dom::BreakKind::Structural => out.push_str("sbr;"),
        },
        Run::PageBreak => out.push_str("pagebr;"),
        Run::ColumnBreak => out.push_str("colbr;"),
        Run::Tab => out.push_str("tab;"),
        Run::FillTab => out.push_str("filltab;"),
        Run::FootnoteRef { props, id, .. } => {
            out.push_str("fnref(");
            sig_run_props(props, out);
            let _ = write!(out, "{id})");
        }
        Run::FootnoteRefMark => out.push_str("fnmark;"),
        Run::CommentReference { props, id } => {
            out.push_str("cref(");
            sig_run_props(props, out);
            let _ = write!(out, "{id})");
        }
        Run::Drawing(drawing) => sig_drawing(drawing, out),
        Run::OmmlInline(xml) => {
            let _ = write!(out, "ommli({xml})");
        }
        Run::Field(field) => {
            let _ = write!(
                out,
                "field({},mode={:?},display={:?}",
                field.instr, field.mode, field.display
            );
            for run in &field.result {
                if field.mode == crate::dom::FieldMode::Live {
                    sig_live_field_result(run, out);
                } else {
                    sig_run(run, out);
                }
            }
            out.push(')');
        }
    }
}

/// A live field's cached glyphs are not part of the furniture identity: PAGE
/// legitimately carries a different resolved cache on every probe page while
/// remaining one reusable Word footer. Keep the result's formatting shape in
/// the signature so genuinely different styled fields still split sections.
fn sig_live_field_result(run: &Run, out: &mut String) {
    match run {
        Run::Text { props, .. } => {
            out.push_str("r(");
            sig_run_props(props, out);
            out.push_str("text=*)");
        }
        _ => sig_run(run, out),
    }
}

fn sig_para_props(props: &ParaProps, out: &mut String) {
    use std::fmt::Write;

    let _ = write!(
        out,
        "style={:?};keep_next={};page_break_before={};keep_lines={};num={:?};bidi={};jc={};outline={:?};shd={:?};",
        props.style,
        props.keep_next,
        props.page_break_before,
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
        crate::dom::ShapeGeom::RoundRect { adj_100k } => {
            format!("roundrect:{adj_100k}")
        }
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

/// Replaces a one-page, text-free DrawingML shape swarm with one full-page
/// raster. This is a consumer-safety fallback: LibreOffice crosses from an
/// 18-second open to a multi-minute hang around 950 independent custom shapes,
/// while the converged Typst page frame is already the exact visual authority.
fn dense_visual_page_block(
    ctx: &mut DocxCtx,
    source: &Content,
    frame: typst_library::layout::Frame,
    geom: &SectGeom,
) -> Option<crate::dom::Block> {
    use crate::dom::{
        Anchor, AnchorPos, AnchorWrap, Block, Drawing, Para, ParaChild, Run,
    };

    const EMU_PER_TWIP: i64 = 635;
    let (rel, _size, _text) = ctx.rasterize_dense_visual_page(source, frame)?;
    let docpr_id = ctx.next_drawing_id();
    let drawing = Drawing {
        rel,
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu: geom.page_w as i64 * EMU_PER_TWIP,
        h_emu: geom.page_h as i64 * EMU_PER_TWIP,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: false,
        docpr_id,
        name: ecow::eco_format!("Dense visual page {docpr_id}"),
        anchor: Some(Anchor {
            z: ctx.next_z(),
            pos_h: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            pos_v: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            wrap: AnchorWrap::None,
            dist: [0, 0, 0, 0],
            behind: false,
        }),
        shape: None,
        group: None,
        pic_clip: PicClip::default(),
    };
    Some(Block::Para(Para {
        props: crate::dom::ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    }))
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

    let saved_w = ctx.available_width;
    ctx.available_width = Abs::pt(geom.page_w as f64 / 20.0);
    // Rendered into a region *expanded* to the full page box (not shrink-fit
    // to the content's own measured size): a watermark/background is commonly
    // built purely from `place(..)`, which positions content absolutely
    // without contributing to the parent's measured size, so a shrink-fit
    // region would collapse it to a degenerate zero-size frame and this would
    // wrongly conclude there was nothing to draw. The drawing is stretched to
    // the full page below regardless, so the content's own measured size was
    // never load-bearing — only the expanded render's pixels are.
    let page_h = Abs::pt(geom.page_h as f64 / 20.0);
    let canvas_fill = behind.then_some(geom.background_color).flatten();
    let result =
        ctx.rasterize_page_overlay(content, styles, content.span(), page_h, canvas_fill);
    ctx.available_width = saved_w;
    let result = result?;
    let Some((rel, _size, text)) = result else {
        return Ok(None);
    };

    let docpr_id = ctx.next_drawing_id();
    let drawing = Drawing {
        rel,
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu: geom.page_w as i64 * EMU_PER_TWIP,
        h_emu: geom.page_h as i64 * EMU_PER_TWIP,
        source_offset_emu: [0, 0],
        alt: (!behind)
            .then(|| text.replace('\n', " ").into())
            .filter(|text: &ecow::EcoString| !text.trim().is_empty()),
        decorative: behind,
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
        pic_clip: PicClip::default(),
    };
    Ok(Some(Block::Para(Para {
        props: crate::dom::ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })))
}

/// Adds a native full-page DrawingML rectangle behind the document as a
/// compatibility companion to `w:background`. Word keeps its native Page Color
/// semantics, while consumers such as headless LibreOffice—which omit
/// `w:background` when exporting PDF—still paint the sheet correctly.
fn solid_page_fill_block(
    ctx: &mut DocxCtx,
    color: [u8; 3],
    geom: &SectGeom,
) -> crate::dom::Block {
    use crate::dom::{
        Anchor, AnchorPos, AnchorWrap, Block, Drawing, Para, ParaChild, Run, ShapeFill,
        ShapeGeom, ShapeSpec,
    };

    const EMU_PER_TWIP: i64 = 635;
    let docpr_id = ctx.next_drawing_id();
    let drawing = Drawing {
        rel: ecow::EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu: geom.page_w as i64 * EMU_PER_TWIP,
        h_emu: geom.page_h as i64 * EMU_PER_TWIP,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: true,
        docpr_id,
        name: ecow::eco_format!("Page Color {docpr_id}"),
        anchor: Some(Anchor {
            z: ctx.next_z(),
            pos_h: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            pos_v: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            wrap: AnchorWrap::None,
            dist: [0, 0, 0, 0],
            behind: true,
        }),
        shape: Some(ShapeSpec {
            geom: ShapeGeom::Rect,
            fill: Some(ShapeFill::Solid([color[0], color[1], color[2], 255])),
            stroke: None,
            txbx: None,
        }),
        group: None,
        pic_clip: PicClip::default(),
    };
    Block::Para(Para {
        props: crate::dom::ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })
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
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: DomFieldCacheStatus::Resolved,
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
            Block::WeakPageBreak => {}
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
            Block::FlowSpace { .. } | Block::SectionBreak(_) => {}
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
            // Mirrors Typst's weak-break semantics in the synthetic page
            // model: advance only off a non-empty page, and land on a fresh
            // one (so a run of weak breaks advances at most once).
            Block::WeakPageBreak => {
                if *seen_content {
                    *page += 1;
                    *y = 0;
                    *seen_content = false;
                }
            }
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
            Block::FlowSpace { .. } => {
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

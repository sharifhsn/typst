//! The DOCX export driver: realizes the native element tree and walks it into
//! the typed IR.

use std::sync::Arc;

use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, NativeElement, Selector, StyleChain};
use typst_library::introspection::{Introspector, Locator, Tag};
use typst_library::model::{DocumentInfo, HeadingElem};
use typst_library::routines::{Arenas, RealizationKind};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, DocxDocument, Field, HdrFtrPart, HdrFtrRef, Para, ParaChild, ParaProps,
    PgNumType, Run, RunProps, SectPr, TocHeading,
};
use crate::introspect::DocxIntrospector;
use crate::props;

/// Produces a DOCX document (in-memory IR) from content.
///
/// First performs root-level realization, then walks the resulting native
/// elements into the typed DOCX IR. The OPC zip is written separately by
/// [`crate::docx`].
#[typst_macros::time(name = "docx document")]
pub fn docx_document(
    engine: &mut Engine,
    content: &Content,
    styles: StyleChain,
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
        .map(|(g, _)| g.clone())
        .unwrap_or_else(|| run_geometry(&[], styles));

    // Per-section `set page(numbering:)`, in section order — the synthetic page
    // model resolves `loc.page-numbering()` against these (see below).
    let section_numberings: Vec<Option<typst_library::model::Numbering>> =
        if sections.is_empty() {
            vec![first_geom.numbering.clone()]
        } else {
            sections.iter().map(|(g, _)| g.numbering.clone()).collect()
        };

    // Walk the native element tree into the typed IR.
    let (
        mut body,
        sect,
        mut header_parts,
        mut footer_parts,
        mut footnotes,
        numbering,
        media,
        doc_rels,
        footnote_rels,
        bookmarks,
        max_heading_level,
        uses_fields,
        uses_math,
        deferred_tags,
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

        // Build the body and the (final) section properties. A single-section
        // document converts all `pairs` at once (unchanged behaviour, so leading
        // pagebreaks etc. are preserved exactly); a multi-section document
        // converts each section's content separately and joins them with
        // `Block::SectionBreak`s carrying the earlier sections' `sectPr`.
        let (body, sect, header_parts, footer_parts) = if sections.len() <= 1 {
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
            for (idx, (geom, range)) in sections.iter().enumerate() {
                let mut blocks = crate::convert::run(&mut ctx, &pairs[range.clone()])?;
                body.append(&mut blocks);
                let (s, mut h, mut f) = build_section(&mut ctx, geom, styles)
                    .unwrap_or_else(|_| (sectpr_geometry(geom), Vec::new(), Vec::new()));
                header_parts.append(&mut h);
                footer_parts.append(&mut f);
                if idx == last {
                    final_sect = Some(s);
                } else {
                    body.push(Block::SectionBreak(s));
                }
            }
            (body, final_sect.expect("at least one section"), header_parts, footer_parts)
        };
        (
            body,
            sect,
            header_parts,
            footer_parts,
            std::mem::take(&mut ctx.footnotes),
            std::mem::take(&mut ctx.numbering),
            std::mem::take(&mut ctx.media),
            std::mem::take(&mut ctx.doc_rels),
            std::mem::take(&mut ctx.footnote_rels),
            std::mem::take(&mut ctx.bookmarks),
            ctx.max_heading_level,
            ctx.uses_fields,
            ctx.uses_math,
            std::mem::take(&mut ctx.deferred_tags),
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
                (!text.is_empty()).then(|| TocHeading { level, anchor: None, text: text.into() })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Now that every heading/figure's real bookmark is known, populate the
    // table(s) of contents and list(s) of figures in document order, across all
    // sections.
    crate::mappers::outline::fill_tocs(&mut body, &toc_headings, &toc_fallback, &toc_figures);

    // Collect introspection tags from the IR for the introspector.
    let mut tags = Vec::new();
    collect_tags(&body, &mut tags);
    for fnote in &footnotes {
        collect_tags(&fnote.blocks, &mut tags);
    }
    // Tags harvested from content we rasterized — so labels/refs inside a drawn
    // figure or box still resolve.
    tags.extend(deferred_tags);

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
    collect_page_model(&body, &mut page, &mut section, &mut page_model);

    // Hoist the document's most common font/size/language into `docDefaults` and
    // strip them from matching runs, so the body inherits (restylable in Word,
    // compact `document.xml`).
    let text_defaults =
        hoist_text_defaults(&mut body, &mut header_parts, &mut footer_parts, &mut footnotes);

    let mut introspector = DocxIntrospector::new(&tags);
    introspector.set_anchors(crate::bookmark::anchors(&bookmarks));
    introspector.set_page_model(page_model, page, section_numberings);

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
        uses_fields,
        uses_math,
        introspector: Arc::new(introspector),
        header_parts,
        footer_parts,
        background_color: first_geom.background_color,
        hyphenate: first_geom.hyphenate,
    })
}

/// Computes the document's most common run properties — font, size and language —
/// over the body, returns them as the [`TextDefaults`] to hoist into
/// `docDefaults`, and strips them from every run that matches across all the
/// block groups (body, headers/footers, footnotes). The body then inherits its
/// font/size/language from `docDefaults` (so editing the `Normal` style or the
/// theme font in Word restyles the whole document) and each run's `<w:rPr>`
/// carries only deviations, keeping `document.xml` compact.
fn hoist_text_defaults(
    body: &mut [Block],
    headers: &mut [HdrFtrPart],
    footers: &mut [HdrFtrPart],
    footnotes: &mut [crate::dom::Footnote],
) -> crate::dom::TextDefaults {
    use rustc_hash::FxHashMap;

    // The dominant text — the body — decides the defaults.
    let mut fonts: FxHashMap<ecow::EcoString, u32> = FxHashMap::default();
    let mut sizes: FxHashMap<u32, u32> = FxHashMap::default();
    let mut langs: FxHashMap<ecow::EcoString, u32> = FxHashMap::default();
    visit_run_props(body, &mut |p| {
        if let Some(f) = &p.font {
            *fonts.entry(f.clone()).or_default() += 1;
        }
        if let Some(s) = p.size_half_pt {
            *sizes.entry(s).or_default() += 1;
        }
        if let Some(l) = &p.lang {
            *langs.entry(l.clone()).or_default() += 1;
        }
    });
    let mode = |m: FxHashMap<ecow::EcoString, u32>| {
        m.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k)
    };
    let defaults = crate::dom::TextDefaults {
        font: mode(fonts),
        size_half_pt: sizes.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k).unwrap_or(22),
        color: None,
        lang: mode(langs),
    };

    // Strip the defaults from every matching run so it inherits from docDefaults.
    let mut strip = |p: &mut crate::dom::RunProps| {
        if p.font == defaults.font {
            p.font = None;
        }
        if p.size_half_pt == Some(defaults.size_half_pt) {
            p.size_half_pt = None;
        }
        if p.lang == defaults.lang {
            p.lang = None;
        }
    };
    visit_run_props(body, &mut strip);
    for h in headers {
        visit_run_props(&mut h.blocks, &mut strip);
    }
    for f in footers {
        visit_run_props(&mut f.blocks, &mut strip);
    }
    for fnote in footnotes {
        visit_run_props(&mut fnote.blocks, &mut strip);
    }
    defaults
}

/// Visits every text run's [`RunProps`] in `blocks`, recursing through table
/// cells and table-of-contents entries.
fn visit_run_props(blocks: &mut [Block], f: &mut dyn FnMut(&mut crate::dom::RunProps)) {
    use crate::dom::{ParaChild, Run};
    fn visit_children(
        children: &mut [ParaChild],
        f: &mut dyn FnMut(&mut crate::dom::RunProps),
    ) {
        for c in children {
            match c {
                ParaChild::Run(Run::Text { props, .. }) => f(props),
                ParaChild::Hyperlink { runs, .. } => {
                    for r in runs {
                        if let Run::Text { props, .. } = r {
                            f(props);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Para(para) => visit_children(&mut para.content, f),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        visit_run_props(&mut cell.blocks, f);
                    }
                }
            }
            Block::Toc(t) => {
                for para in &mut t.entries {
                    visit_children(&mut para.content, f);
                }
                for r in &mut t.fallback {
                    if let Run::Text { props, .. } = r {
                        f(props);
                    }
                }
            }
            Block::SectionBreak(_) | Block::Tag(_) => {}
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
fn resolve_sections(
    pairs: &[(&Content, StyleChain)],
    initial: StyleChain,
) -> Vec<(SectGeom, std::ops::Range<usize>)> {
    use typst_library::layout::PagebreakElem;

    let mut sections: Vec<(SectGeom, std::ops::Range<usize>)> = Vec::new();
    let mut initial = initial;
    let mut i = 0;
    while i < pairs.len() {
        // Skip pagebreaks, folding non-boundary (`set page`) ones into the
        // section-initial chain. Boundary pagebreaks carry pre-rule styles, so
        // they must NOT be folded.
        while i < pairs.len() {
            if let Some(pb) = pairs[i].0.to_packed::<PagebreakElem>() {
                if !pb.boundary.get(pairs[i].1) {
                    initial = pairs[i].1;
                }
                i += 1;
            } else {
                break;
            }
        }
        if i >= pairs.len() {
            break;
        }
        // Take the run of non-pagebreak content.
        let start = i;
        while i < pairs.len() && !pairs[i].0.is::<PagebreakElem>() {
            i += 1;
        }
        let geom = run_geometry(&pairs[start..i], initial);
        // Merge into the previous section if nothing section-scoped changed;
        // the pagebreaks between them then fall inside the merged range
        // (→ `<w:br>`).
        if let Some(last) = sections.last_mut()
            && same_section(&last.0, &geom)
        {
            last.1.end = i;
            continue;
        }
        sections.push((geom, start..i));
    }
    sections
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
        && a.number_in_header == b.number_in_header
        && a.number_jc == b.number_jc
        && a.header_suppressed == b.header_suppressed
        && a.footer_suppressed == b.footer_suppressed
        && hash128(&a.header) == hash128(&b.header)
        && hash128(&a.footer) == hash128(&b.footer)
        && hash128(&a.background) == hash128(&b.background)
}

/// Resolves one page run's geometry from its group of content pairs.
fn run_geometry(group: &[(&Content, StyleChain)], initial: StyleChain) -> SectGeom {
    use typst_library::foundations::{Resolve, Smart, Styles};
    use typst_library::layout::{
        Abs, FixAlignment, FixedAlignment, Length, OuterVAlignment, PageElem, Paper, Rel,
        Sides, Size,
    };
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
    let footer_descent =
        sc.resolve(PageElem::footer_descent).relative_to(sides.bottom);
    let header_band = clamp_band(props::abs_to_twip(sides.top - header_ascent), props::abs_to_twip(sides.top));
    let footer_band = clamp_band(props::abs_to_twip(sides.bottom - footer_descent), props::abs_to_twip(sides.bottom));

    // Binding allowance → gutter (informational; default LTR binding = left).
    let gutter = 0;

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
    let background =
        sc.get_ref(PageElem::background).clone().filter(|c| !c.is_empty());
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
        margin_left: props::abs_to_twip(sides.left),
        margin_right: props::abs_to_twip(sides.right),
        header_band,
        footer_band,
        gutter,
        columns,
        col_space,
        numbering,
        number_in_header,
        number_jc,
        header,
        header_suppressed,
        footer,
        footer_suppressed,
        background,
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

    // -- Header content + page background ----------------------------------
    // A `set page(background:)` image is emitted as a full-page `behindDoc`
    // page-anchored drawing at the *top* of the (default) header, so it repeats
    // on every page behind the body text — the Word idiom for a page background /
    // watermark. The background and the explicit header share ONE part so their
    // image relationships live in a single `headerN.xml.rels` (no rId collision).
    if geom.header.is_some() || geom.background.is_some() {
        let saved = ctx.part_rels.take();
        ctx.part_rels = Some(crate::package::Rels::new());
        let mut blocks = Vec::new();
        if let Some(bg) = &geom.background
            && let Some(block) = background_block(ctx, bg, geom, styles)?
        {
            blocks.push(block);
        }
        if let Some(content) = &geom.header {
            blocks.extend(ctx.blocks(content, styles)?);
        }
        let rels = ctx.part_rels.take().unwrap_or_default();
        ctx.part_rels = saved;
        // Header content lives outside the body IR, so its introspection tags
        // would never reach the introspector — harvest them here (a labeled
        // element in a running head is a real query target; templates read
        // page furniture via `query(<label>)`). Appended tags sort after the
        // whole body, which is also where an `.after(here())` furniture query
        // expects them. Duplicate locations across sections are deduped by the
        // introspector builder.
        collect_tags(&blocks, &mut ctx.deferred_tags);
        // Emit the header part whenever a header is explicitly set (even if it
        // lowered to nothing) — matching the prior unconditional behaviour — or
        // when the background produced a drawing.
        if geom.header.is_some() || !blocks.is_empty() {
            let part_name = ctx.next_hdrftr_name(true);
            let rel = ctx.add_header_rel(&part_name);
            sect.headers.push(HdrFtrRef { kind: "default", rel });
            header_parts.push(HdrFtrPart { part_name, is_header: true, blocks, rels });
        }
    }

    // -- Explicit footer content -------------------------------------------
    if let Some(content) = &geom.footer {
        let (blocks, rels) = ctx.part_blocks(content, styles)?;
        // Same as the header above: footer tags must reach the introspector.
        collect_tags(&blocks, &mut ctx.deferred_tags);
        let part_name = ctx.next_hdrftr_name(false);
        let rel = ctx.add_footer_rel(&part_name);
        sect.footers.push(HdrFtrRef { kind: "default", rel });
        footer_parts.push(HdrFtrPart { part_name, is_header: false, blocks, rels });
    }

    // -- Synthetic page-number band (numbering set, band left as `auto`) ----
    if geom.numbering.is_some() {
        if geom.number_in_header && geom.header.is_none() && !geom.header_suppressed {
            let part_name = ctx.next_hdrftr_name(true);
            let rel = ctx.add_header_rel(&part_name);
            sect.headers.push(HdrFtrRef { kind: "default", rel });
            header_parts.push(HdrFtrPart {
                part_name,
                is_header: true,
                blocks: vec![page_number_para("Header", geom.number_jc)],
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
            footer_parts.push(HdrFtrPart {
                part_name,
                is_header: false,
                blocks: vec![page_number_para("Footer", geom.number_jc)],
                rels: crate::package::Rels::new(),
            });
            ctx.mark_field();
        }
    }

    Ok((sect, header_parts, footer_parts))
}

/// Rasterizes a `set page(background:)` body and wraps it in a full-page
/// `behindDoc` page-anchored drawing (one paragraph). Rasterized at the full page
/// *width* so a `width: 100%` background fills the page; the drawing's extent is
/// the full page size so it covers the sheet edge-to-edge. `None` if the
/// background lays out to nothing. Must be called inside an active part-rels
/// context (the image relationship belongs to the header part).
fn background_block(
    ctx: &mut DocxCtx,
    bg: &Content,
    geom: &SectGeom,
    styles: StyleChain,
) -> SourceResult<Option<crate::dom::Block>> {
    use crate::dom::{Anchor, AnchorPos, AnchorWrap, Block, Drawing, Para, ParaChild, Run};
    use typst_library::layout::Abs;

    // EMU per twip = 914400 / 1440.
    const EMU_PER_TWIP: i64 = 635;

    let saved_w = ctx.raster_width;
    ctx.raster_width = Abs::pt(geom.page_w as f64 / 20.0);
    let result = ctx.rasterize(bg, styles, bg.span())?;
    ctx.raster_width = saved_w;
    let Some((rel, _size, _text)) = result else {
        return Ok(None);
    };

    let docpr_id = ctx.next_drawing_id();
    let drawing = Drawing {
        rel,
        w_emu: geom.page_w as i64 * EMU_PER_TWIP,
        h_emu: geom.page_h as i64 * EMU_PER_TWIP,
        alt: None,
        docpr_id,
        name: ecow::eco_format!("Background {docpr_id}"),
        anchor: Some(Anchor {
            z: ctx.next_z(),
            pos_h: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            pos_v: AnchorPos { rel_from: "page", align: None, offset: Some(0) },
            wrap: AnchorWrap::None,
            dist: [0, 0, 0, 0],
            behind: true,
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
/// `w:pgNumType/@w:fmt`. Renders the symbol via the engine (avoiding a direct
/// `codex` dependency) and matches the glyph for 1 and 4.
fn numbering_fmt(
    ctx: &mut DocxCtx,
    numbering: &typst_library::model::Numbering,
) -> &'static str {
    use typst_library::model::Numbering;
    let Numbering::Pattern(pattern) = numbering else {
        return "decimal";
    };
    if pattern.pieces() == 0 {
        return "decimal";
    }
    let span = typst_syntax::Span::detached();
    let one = pattern.apply_kth(ctx.engine(), span, 0, 1);
    let four = pattern.apply_kth(ctx.engine(), span, 0, 4);
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

/// Builds a single-paragraph header/footer carrying a live `{ PAGE }` field.
fn page_number_para(style: &str, jc: Option<crate::dom::Jc>) -> Block {
    let props = ParaProps {
        style: Some(style.into()),
        jc: jc.or(Some(crate::dom::Jc::Center)),
        ..Default::default()
    };
    let field = Field {
        instr: " PAGE ".into(),
        result: vec![Run::Text { props: RunProps::default(), text: "1".into() }],
        dirty: false,
    };
    Block::Para(Para { props, content: vec![ParaChild::Run(Run::Field(field))] })
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

/// Builds the synthetic page model: walks the IR in the same order as
/// [`collect_tags`], counting explicit page breaks (`Run::PageBreak`) and
/// section breaks (each of which also starts a new page), and records the
/// (page, section) pair in effect at every `Tag::Start` location. Tags not in
/// the map (rasterize-deferred, header/footer) are appended after the body, so
/// their lookups fall back to the final page.
fn collect_page_model(
    blocks: &[Block],
    page: &mut usize,
    section: &mut usize,
    map: &mut rustc_hash::FxHashMap<
        typst_library::introspection::Location,
        (usize, usize),
    >,
) {
    let record = |tag: &Tag, map: &mut rustc_hash::FxHashMap<_, _>, page: usize, section: usize| {
        if let Tag::Start(elem, _) = tag
            && let Some(loc) = elem.location()
        {
            map.entry(loc).or_insert((page, section));
        }
    };
    for block in blocks {
        match block {
            Block::Tag(tag) => record(tag, map, *page, *section),
            Block::Para(para) => {
                for child in &para.content {
                    match child {
                        ParaChild::Tag(tag) => record(tag, map, *page, *section),
                        ParaChild::Run(Run::PageBreak) => *page += 1,
                        _ => {}
                    }
                }
            }
            Block::Table(tbl) => {
                for row in &tbl.rows {
                    for cell in &row.cells {
                        collect_page_model(&cell.blocks, page, section, map);
                    }
                }
            }
            Block::Toc(toc) => {
                for para in &toc.entries {
                    for child in &para.content {
                        if let ParaChild::Tag(tag) = child {
                            record(tag, map, *page, *section);
                        }
                    }
                }
            }
            Block::SectionBreak(_) => {
                *page += 1;
                *section += 1;
            }
        }
    }
}

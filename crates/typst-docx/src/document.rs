//! The DOCX export driver: realizes the native element tree and walks it into
//! the typed IR.

use std::sync::Arc;

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::engine::Engine;
use typst_library::foundations::{Content, StyleChain};
use typst_library::introspection::{Locator, Tag};
use typst_library::model::DocumentInfo;
use typst_library::routines::{Arenas, RealizationKind};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, DocxDocument, Field, HdrFtrPart, HdrFtrRef, Para, ParaChild, ParaProps,
    PgNumType, Run, RunProps, SectPr,
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

    // Resolve the active page setup (G1). Single-section MVP: the whole document
    // is one section, resolved over all `pairs` with the document-root chain as
    // the section-initial styles (mirrors `typst-layout/src/pages/run.rs`). This
    // read needs no engine, so it happens before the `DocxCtx` body walk; the
    // header/footer *content* is lowered later on the same `ctx`.
    let sect_geom = resolve_geometry(&pairs, styles);

    // Walk the native element tree into the typed IR.
    let (
        body,
        sect,
        header_parts,
        footer_parts,
        footnotes,
        numbering,
        media,
        doc_rels,
        bookmarks,
        max_heading_level,
        uses_fields,
        uses_math,
        deferred_tags,
    ) = {
        let mut ctx = DocxCtx::new(engine, &mut locator);
        let body = crate::convert::run(&mut ctx, &pairs)?;
        // Build the section properties + header/footer parts on the same ctx so
        // any inner media/rels join the document's tables.
        let (sect, header_parts, footer_parts) =
            build_section(&mut ctx, &sect_geom, styles)?;
        (
            body,
            sect,
            header_parts,
            footer_parts,
            std::mem::take(&mut ctx.footnotes),
            std::mem::take(&mut ctx.numbering),
            std::mem::take(&mut ctx.media),
            std::mem::take(&mut ctx.doc_rels),
            std::mem::take(&mut ctx.bookmarks),
            ctx.max_heading_level,
            ctx.uses_fields,
            ctx.uses_math,
            std::mem::take(&mut ctx.deferred_tags),
        )
    };

    // Collect introspection tags from the IR for the introspector.
    let mut tags = Vec::new();
    collect_tags(&body, &mut tags);
    for fnote in &footnotes {
        collect_tags(&fnote.blocks, &mut tags);
    }
    // Tags harvested from content we rasterized — so labels/refs inside a drawn
    // figure or box still resolve.
    tags.extend(deferred_tags);

    let mut introspector = DocxIntrospector::new(&tags);
    introspector.set_anchors(crate::bookmark::anchors(&bookmarks));

    Ok(DocxDocument {
        info,
        body,
        sect,
        footnotes,
        numbering,
        media,
        doc_rels,
        bookmarks,
        max_heading_level,
        uses_fields,
        uses_math,
        introspector: Arc::new(introspector),
        header_parts,
        footer_parts,
    })
}

/// The engine-free part of the resolved page setup. The page-number format
/// classification and the header/footer content lowering need the engine/ctx,
/// so they are deferred to [`build_section`]; this struct carries everything
/// they need.
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
}

/// Resolves the active page geometry for the single-section MVP, mirroring
/// `typst-layout/src/pages/run.rs`. Engine-free.
fn resolve_geometry(
    pairs: &[(&Content, StyleChain)],
    initial: StyleChain,
) -> SectGeom {
    use typst_library::foundations::{Resolve, Smart, Styles};
    use typst_library::layout::{
        Abs, FixAlignment, FixedAlignment, Length, OuterVAlignment, PageElem, PagebreakElem,
        Paper, Rel, Sides, Size,
    };
    use typst_library::text::TextElem;
    use typst_utils::Numeric;

    // A `set page(...)` rule injects a leading weak (non-boundary) `PagebreakElem`
    // whose styles carry the page set rules. Mirror `pages::collect`: advance the
    // section-initial chain across any leading non-boundary pagebreaks, then take
    // the first run of non-pagebreak content as this (single) section's group.
    // (Trailing `set page` scope-boundary pagebreaks carry the *pre*-rule styles,
    // so we must NOT fold them in.)
    let mut initial = initial;
    let mut rest = pairs;
    while let Some(&(elem, styles)) = rest.first() {
        if let Some(pb) = elem.to_packed::<PagebreakElem>() {
            if !pb.boundary.get(styles) {
                initial = styles;
            }
            rest = &rest[1..];
        } else {
            break;
        }
    }
    let group_end = rest.iter().take_while(|(c, _)| !c.is::<PagebreakElem>()).count();
    let group = &rest[..group_end];

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
fn build_section(
    ctx: &mut DocxCtx,
    geom: &SectGeom,
    styles: StyleChain,
) -> SourceResult<(SectPr, Vec<HdrFtrPart>, Vec<HdrFtrPart>)> {
    let mut sect = SectPr {
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
    };

    let mut header_parts = Vec::new();
    let mut footer_parts = Vec::new();

    // Page numbering → pgNumType (glyph format) + a PAGE field in the auto band.
    if let Some(numbering) = &geom.numbering {
        sect.pg_num = Some(PgNumType { fmt: numbering_fmt(ctx, numbering), start: None });
    }

    // -- Explicit header content -------------------------------------------
    if let Some(content) = &geom.header {
        let blocks = ctx.blocks(content, styles)?;
        let part_name: EcoString = "header1.xml".into();
        let rel = ctx.add_header_rel(&part_name);
        sect.headers.push(HdrFtrRef { kind: "default", rel });
        header_parts.push(HdrFtrPart { part_name, is_header: true, blocks });
    }

    // -- Explicit footer content -------------------------------------------
    if let Some(content) = &geom.footer {
        let blocks = ctx.blocks(content, styles)?;
        let part_name: EcoString = "footer1.xml".into();
        let rel = ctx.add_footer_rel(&part_name);
        sect.footers.push(HdrFtrRef { kind: "default", rel });
        footer_parts.push(HdrFtrPart { part_name, is_header: false, blocks });
    }

    // -- Synthetic page-number band (numbering set, band left as `auto`) ----
    if geom.numbering.is_some() {
        if geom.number_in_header && geom.header.is_none() && !geom.header_suppressed {
            let part_name: EcoString = "header1.xml".into();
            let rel = ctx.add_header_rel(&part_name);
            sect.headers.push(HdrFtrRef { kind: "default", rel });
            header_parts.push(HdrFtrPart {
                part_name,
                is_header: true,
                blocks: vec![page_number_para("Header", geom.number_jc)],
            });
            ctx.mark_field();
        } else if !geom.number_in_header
            && geom.footer.is_none()
            && !geom.footer_suppressed
        {
            let part_name: EcoString = "footer1.xml".into();
            let rel = ctx.add_footer_rel(&part_name);
            sect.footers.push(HdrFtrRef { kind: "default", rel });
            footer_parts.push(HdrFtrPart {
                part_name,
                is_header: false,
                blocks: vec![page_number_para("Footer", geom.number_jc)],
            });
            ctx.mark_field();
        }
    }

    Ok((sect, header_parts, footer_parts))
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
fn collect_tags(blocks: &[Block], out: &mut Vec<Tag>) {
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
            Block::SectionBreak(_) => {}
        }
    }
}

//! Image mapper: `ImageElem` / `FigureElem` → DrawingML inline image.
//!
//! Implements the image element family per `fields_links_images_validation.md`
//! §4 (inline DrawingML) and SPEC §14.
//!
//! ## What this emits
//!
//! - [`image`] lowers an [`ImageElem`] into a single [`Run::Drawing`] — the IR
//!   for a `<w:drawing><wp:inline>…<a:blip r:embed="rId">` inline picture. The
//!   image bytes are embedded as a `word/media/imageN.ext` part via
//!   [`DocxCtx::add_image`] (which dedups identical bytes and registers the
//!   content-type + relationship), and a unique `wp:docPr` id is allocated via
//!   [`DocxCtx::next_drawing_id`]. The display extents are computed in EMU
//!   (914400 EMU/inch = 12700 EMU/pt) from the resolved width/height, falling
//!   back to the image's intrinsic pixel size and DPI.
//!
//! - [`figure`] lowers a [`FigureElem`] into the figure body (centered) plus a
//!   `Caption`-styled caption paragraph carrying the realized figure number
//!   (supplement + counter + separator), honoring `caption.position` (top vs
//!   bottom) and registering a bookmark so `@fig` cross-references resolve.
//!
//! - [`laid_out_fallback`] is the generic escape hatch: any element the block
//!   dispatch cannot represent natively can be rasterized to a PNG and embedded
//!   as an inline picture. (See the INTEGRATION-NEEDED note on that function —
//!   rasterization requires a renderer that is not yet a dependency of this
//!   crate.)
//!
//! ## Format handling
//!
//! Word embeds raster *exchange* formats directly: PNG, JPEG and GIF bytes are
//! stored verbatim (no re-encode), preserving fidelity and file size. WebP, SVG
//! and PDF sources have no universally-safe native Word representation and must
//! be rasterized to PNG first; that path is flagged INTEGRATION-NEEDED because
//! it needs `typst-render` (not currently a dependency — adding it touches
//! `Cargo.toml`, which mappers may not edit).

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Packed, Smart, StyleChain};
use typst_library::layout::{Abs, OuterVAlignment, Sizing, VAlignment};
use typst_library::model::{FigureElem, FigureKind};
use typst_library::text::TextElem;
use typst_library::visualize::{
    ExchangeFormat, Image, ImageElem, ImageKind, RasterFormat,
};

use crate::ctx::DocxCtx;
use crate::dom::{
    Anchor, AnchorPos, AnchorWrap, Block, Drawing, Field, Jc, Para, ParaChild, ParaProps,
    Run, RunProps,
};

/// English Metric Units per point, as an `i64` factor for whole-point offsets.
const EMU_PER_PT_I: i64 = 12700;

/// English Metric Units per point (914400 EMU/inch ÷ 72 pt/inch).
const EMU_PER_PT: f64 = 12700.0;

/// The `Caption` paragraph-style id (defined in `styles.xml`).
const CAPTION_STYLE: &str = "Caption";

/// Lowers an [`ImageElem`] into an inline DrawingML picture run.
///
/// The returned [`Run::Drawing`] is inline-anchored: it is a sibling of text
/// runs and so is valid both inside a paragraph (when the image appears inline)
/// and as the sole content of a paragraph (a block image / figure body). The
/// caller (figure handling or the block dispatch) is responsible for placing it
/// in a — typically centered — paragraph.
pub fn image(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Run> {
    let span = elem.span();

    // Decode the image so we know its real format and intrinsic pixel size.
    let decoded = elem.decode(ctx.engine(), styles)?;

    // Obtain embeddable bytes + the lowercase extension Word understands.
    let Some((bytes, ext)) = embeddable_bytes(&decoded) else {
        // Vector / WebP / PDF have no Word-embeddable raster form, so lay the
        // image out and rasterize it to a PNG via the generic fallback.
        let content = elem.clone().pack();
        if let Some(run) = laid_out_fallback(&content, styles, ctx)? {
            return Ok(run);
        }
        ctx.warn_ignored("image could not be rasterized for DOCX export", span);
        return Ok(Run::Text { props: RunProps::default(), text: "".into() });
    };

    // Embed the bytes (dedup by content hash) → the `r:embed` relationship id.
    let rel = ctx.add_image(&bytes, &ext);

    // Compute the display size in EMU from the resolved width/height, falling
    // back to the intrinsic point-size derived from pixels + DPI.
    let (w_emu, h_emu) = display_extents(elem, styles, &decoded);

    // A unique, ≥1 non-visual id (Word repairs on duplicate `wp:docPr` ids).
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");

    // Alt text → `descr` for accessibility.
    let alt = elem.alt.get_cloned(styles);

    Ok(Run::Drawing(Drawing { rel, w_emu, h_emu, alt, docpr_id, name, anchor: None, shape: None }))
}

/// Lowers a [`FigureElem`] into its body blocks plus a caption paragraph.
///
/// The figure body is lowered via [`DocxCtx::blocks`] (so a contained image,
/// table, etc. is handled by its own mapper) and centered. The caption — if any
/// — is realized via `FigureCaption::realize` (which prepends the supplement +
/// figure number + separator) and emitted as a `Caption`-styled paragraph,
/// placed above or below the body per `caption.position`. The figure's
/// `Location` is registered as a bookmark so `@fig`-style cross-references can
/// target it with a `REF`/`PAGEREF` field.
///
/// INTEGRATION-NEEDED: `crate::convert::handle_block`'s `FigureElem` arm
/// currently calls `ctx.blocks(&elem.body, styles)` directly, dropping the
/// caption and bookmark. Route it through this function instead:
/// `out.extend(mappers::image::figure(elem, styles, ctx)?);`
pub fn figure(
    elem: &Packed<FigureElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let mut blocks: Vec<Block> = Vec::new();

    // Register a bookmark so cross-references to this figure resolve. We bracket
    // the whole figure (caption + body) with the start/end markers, attaching
    // them to the first and last emitted paragraphs.
    let bookmark = elem.location().map(|loc| ctx.add_bookmark(loc));

    // G9: build the caption with a `SEQ` field for the number (so Word
    // auto-renumbers) instead of a baked-in static counter value.
    let caption = elem.caption.get_cloned(styles);
    let (position, caption_blocks) = match &caption {
        Some(cap) => {
            let position = cap.position.get(styles);
            let runs = caption_runs(elem, cap, styles, ctx)?;
            let mut props = ParaProps::default();
            props.style = Some(CAPTION_STYLE.into());
            let para = Para {
                props,
                content: runs.into_iter().map(ParaChild::Run).collect(),
            };
            (position, vec![Block::Para(para)])
        }
        None => (OuterVAlignment::Bottom, Vec::new()),
    };

    // Record a numbered captioned figure so a list of figures/tables can list it
    // later (Word's `\c` field draws from SEQ-captioned figures of a category).
    // The text is the realized caption ("Figure 1: …"); the bookmark, when the
    // figure has one, makes the entry a live link.
    if let Some(cap) = &caption
        && elem.numbering.get_ref(styles).is_some()
    {
        let text = cap.realize(ctx.engine(), styles)?.plain_text();
        if !text.is_empty() {
            ctx.toc_figures.push(crate::dom::TocFigure {
                category: seq_name(elem, styles),
                anchor: bookmark.as_ref().map(|(_, name)| name.clone()),
                text,
            });
        }
    }

    // G8: a placed figure (`placement: top|bottom|auto`) floats via `<wp:anchor>`
    // instead of flowing inline. The caption stays in flow (Word convention).
    let placement = elem.placement.get(styles);
    let mut body_blocks = if let Some(place) = placement {
        float_figure_body(elem, place, styles, ctx)?
    } else {
        // In-flow body. Figures are centered by their show-set rule; mirror that
        // by centering each top-level paragraph the body produces. A framed box in
        // this centered context must NOT become a `wps:txbx` text box (centered
        // text boxes don't flow their text in LibreOffice); the flag makes such a
        // box rasterize to a centered image instead, which renders everywhere.
        let saved = ctx.suppress_text_box;
        ctx.suppress_text_box = true;
        let blocks = ctx.blocks(&elem.body, styles);
        ctx.suppress_text_box = saved;
        let mut body_blocks = blocks?;
        for block in &mut body_blocks {
            if let Block::Para(para) = block
                && para.props.jc.is_none()
            {
                para.props.jc = Some(Jc::Center);
            }
        }
        body_blocks
    };

    // Assemble in caption-position order.
    match position {
        OuterVAlignment::Top => {
            blocks.extend(caption_blocks);
            blocks.append(&mut body_blocks);
        }
        OuterVAlignment::Bottom => {
            blocks.append(&mut body_blocks);
            blocks.extend(caption_blocks);
        }
    }

    // Bracket the figure with bookmark markers (attached to the first/last
    // paragraph). A zero-length bookmark is legal, so if the figure produced no
    // paragraphs we still emit a marker pair on a fresh empty paragraph.
    if let Some((id, name)) = bookmark {
        attach_bookmark(&mut blocks, id, name);
    }

    Ok(blocks)
}

/// Lowers a top-level `#place(..)` into a floating drawing (G8).
///
/// The placed body is lowered to a single `Drawing` (image body → the inline
/// `pic:pic` payload; otherwise the rasterized PNG fallback), then wrapped in a
/// `<wp:anchor>` whose position follows the place alignment + `dx`/`dy` offsets:
///
/// - an axis with an absolute `dx`/`dy` → `<wp:posOffset>` in EMU;
/// - otherwise the alignment component → `<wp:align>` (pure-`%` offsets, which
///   have no fixed value without layout, fall back to the alignment);
/// - `float: true` → wrap top-and-bottom; `float: false` → `wrapNone` (overlap).
///
/// Returns `None` if the body lays out to nothing (the caller then skips it).
pub fn place(
    elem: &Packed<typst_library::layout::PlaceElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Block>> {
    use typst_library::layout::HAlignment;

    let Some(mut drawing) = place_body_drawing(&elem.body, styles, ctx)? else {
        return Ok(None);
    };

    let font_size = styles.resolve(TextElem::size);
    let align = elem.alignment.get(styles);
    let float = elem.float.get(styles);

    // Resolve dx/dy: a nonzero absolute (non-`%`) component → EMU offset;
    // pure-`%` (or zero) → fall back to alignment.
    let dx = elem.dx.get(styles);
    let dy = elem.dy.get(styles);
    let dx_abs = if dx.rel.is_zero() { dx.abs.at(font_size) } else { Abs::zero() };
    let dy_abs = if dy.rel.is_zero() { dy.abs.at(font_size) } else { Abs::zero() };
    let dx_emu = (dx_abs != Abs::zero()).then(|| crate::props::abs_to_emu(dx_abs));
    let dy_emu = (dy_abs != Abs::zero()).then(|| crate::props::abs_to_emu(dy_abs));

    // The alignment component, if any (Smart::Auto → none).
    let h_comp = match align {
        Smart::Custom(a) => a.x(),
        Smart::Auto => None,
    };
    let v_comp = match align {
        Smart::Custom(a) => a.y(),
        Smart::Auto => None,
    };

    // Horizontal: offset wins, else alignment component, else default left.
    let h_align: &'static str = match h_comp {
        Some(HAlignment::Center) => "center",
        Some(HAlignment::Right | HAlignment::End) => "right",
        _ => "left",
    };
    let pos_h = match dx_emu {
        Some(off) => AnchorPos { rel_from: "margin", align: None, offset: Some(off) },
        None => AnchorPos { rel_from: "margin", align: Some(h_align), offset: None },
    };

    // Vertical: offset wins, else alignment component, else default top.
    let v_align: &'static str = match v_comp {
        Some(VAlignment::Bottom) => "bottom",
        Some(VAlignment::Horizon) => "center",
        _ => "top",
    };
    let pos_v = match dy_emu {
        Some(off) => AnchorPos { rel_from: "margin", align: None, offset: Some(off) },
        None => AnchorPos { rel_from: "margin", align: Some(v_align), offset: None },
    };

    let wrap = if float { AnchorWrap::TopAndBottom } else { AnchorWrap::None };
    drawing.anchor = Some(Anchor {
        z: ctx.next_z(),
        pos_h,
        pos_v,
        wrap,
        dist: [0, 0, 0, 0],
        behind: false,
    });

    Ok(Some(Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })))
}

/// Lowers a `#place` body to a single `Drawing` (image payload or a rasterized
/// fallback). Returns `None` if it lays out to nothing.
fn place_body_drawing(
    body: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Drawing>> {
    // Lower the body like any block and pull out its first standalone drawing
    // (covers a bare image, a centered figure-less image, etc.).
    let mut blocks = ctx.blocks(body, styles)?;
    if let Some(drawing) = take_first_drawing(&mut blocks) {
        return Ok(Some(drawing));
    }
    // No native image inside: rasterize the whole placed body to a PNG.
    match laid_out_fallback(body, styles, ctx)? {
        Some(Run::Drawing(drawing)) => Ok(Some(drawing)),
        _ => Ok(None),
    }
}

/// Builds the caption runs with a `SEQ` field carrying the number (G9).
///
/// When the figure is numbered, the caption is reconstructed as
/// `supplement` + `{ SEQ Kind \* ARABIC }` + `separator` + `body`, where the
/// SEQ field caches the realized number as its result so the caption reads
/// correctly before Word recomputes fields. The `SEQ` name follows the figure
/// `kind` (image→`Figure`, table→`Table`, raw→`Listing`, else the name) so
/// distinct kinds keep independent counters. Unnumbered captions fall back to
/// the plain realized text.
fn caption_runs(
    elem: &Packed<FigureElem>,
    cap: &Packed<typst_library::model::FigureCaption>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    // Only emit SEQ for actually-numbered figures; otherwise plain realized text
    // (byte-identical to the previous behaviour for unnumbered captions).
    let numbered = elem.numbering.get_ref(styles).is_some();
    if !numbered {
        let realized = cap.realize(ctx.engine(), styles)?;
        return ctx.inline_runs(&realized, styles, RunProps::default());
    }

    let mut runs: Vec<Run> = Vec::new();

    // Supplement (e.g. "Figure") + the non-breaking space the realizer inserts.
    if let Some(Some(supplement)) = cap.supplement.clone()
        && !supplement.is_empty()
    {
        let mut sup = supplement;
        sup += TextElem::packed('\u{a0}');
        runs.extend(ctx.inline_runs(&sup, styles, RunProps::default())?);
    }

    // The realized number, used as the SEQ field's cached result so the caption
    // is readable before Word updates fields.
    let number_runs = match (cap.counter.clone(), cap.numbering.clone(), cap.figure_location.clone())
    {
        (Some(Some(counter)), Some(Some(numbering)), Some(Some(location))) => {
            let number =
                counter.display_at(ctx.engine(), location, styles, &numbering, cap.span())?;
            ctx.inline_runs(&number, styles, RunProps::default())?
        }
        _ => Vec::new(),
    };

    // The SEQ complex field. Word recomputes the number on open / field update.
    let seq = seq_name(elem, styles);
    runs.push(Run::Field(Field {
        instr: ecow::eco_format!(" SEQ {seq} \\* ARABIC "),
        result: number_runs,
        dirty: false,
    }));
    ctx.mark_field();

    // Separator (e.g. ": "). Synthesized into the `separator` field by
    // `FigureCaption`'s `Synthesize` impl, so read it off the chain.
    if let Smart::Custom(sep) = cap.separator.get_cloned(styles) {
        runs.extend(ctx.inline_runs(&sep, styles, RunProps::default())?);
    }

    // Caption body.
    runs.extend(ctx.inline_runs(&cap.body, styles, RunProps::default())?);

    Ok(runs)
}

/// Maps a figure's `kind` to a `SEQ` field name (a stable per-kind counter
/// identifier). Matches the Word convention `Figure`/`Table`/`Listing`.
pub(crate) fn seq_name(elem: &Packed<FigureElem>, styles: StyleChain) -> EcoString {
    use typst_library::foundations::NativeElement;
    use typst_library::model::TableElem;
    use typst_library::text::RawElem;
    match elem.kind.get_ref(styles) {
        Smart::Custom(FigureKind::Elem(func)) => {
            if *func == TableElem::ELEM {
                "Table".into()
            } else if *func == RawElem::ELEM {
                "Listing".into()
            } else {
                "Figure".into()
            }
        }
        Smart::Custom(FigureKind::Name(name)) => {
            // Use the custom name verbatim as the counter id (sanitized of
            // spaces, which would break the field-code token).
            name.replace(" ", "_").into()
        }
        Smart::Auto => "Figure".into(),
    }
}

/// Lowers a figure body to a single floating drawing (G8).
///
/// The body is lowered like any block, then the first picture it produces is
/// turned into a floating anchor (image body → the inline `pic:pic`; otherwise
/// the rasterized PNG fallback). `placement` chooses the vertical alignment
/// (`Auto`/`Top` → top, `Bottom` → bottom); horizontal is centered. The float
/// wraps text top-and-bottom (Word's figure convention). If the body produces
/// no drawing (e.g. a table figure), it falls back to the in-flow centered
/// paragraphs unchanged.
fn float_figure_body(
    elem: &Packed<FigureElem>,
    placement: Smart<VAlignment>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let mut body_blocks = ctx.blocks(&elem.body, styles)?;

    // Find the first standalone drawing run in the lowered body.
    let drawing = take_first_drawing(&mut body_blocks);
    let Some(mut drawing) = drawing else {
        // Non-image body (table, multi-paragraph): keep it in flow, centered.
        for block in &mut body_blocks {
            if let Block::Para(para) = block
                && para.props.jc.is_none()
            {
                para.props.jc = Some(Jc::Center);
            }
        }
        return Ok(body_blocks);
    };

    let v_align = match placement {
        Smart::Custom(VAlignment::Bottom) => "bottom",
        // Auto / Top / Horizon → top.
        _ => "top",
    };
    let dist = EMU_PER_PT_I * 9; // ~9pt clearance around the float.
    drawing.anchor = Some(Anchor {
        z: ctx.next_z(),
        pos_h: AnchorPos { rel_from: "margin", align: Some("center"), offset: None },
        pos_v: AnchorPos { rel_from: "margin", align: Some(v_align), offset: None },
        wrap: AnchorWrap::TopAndBottom,
        dist: [0, 0, dist, dist],
        behind: false,
    });

    // The anchored drawing lives in its own paragraph; any other body blocks
    // (rare for a figure) stay after it.
    let mut out = Vec::with_capacity(body_blocks.len() + 1);
    out.push(Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    }));
    out.append(&mut body_blocks);
    Ok(out)
}

/// Removes and returns the first `Run::Drawing` found in `blocks`, leaving any
/// sibling content (e.g. introspection tags) in place so it isn't lost. If the
/// host paragraph is left empty after extraction, it is dropped. Returns `None`
/// if no drawing exists (the body has no native image — e.g. a table figure).
fn take_first_drawing(blocks: &mut Vec<Block>) -> Option<Drawing> {
    for block in blocks.iter_mut() {
        if let Block::Para(para) = block {
            // Find the index of the (first) drawing child in this paragraph.
            let pos = para
                .content
                .iter()
                .position(|c| matches!(c, ParaChild::Run(Run::Drawing(_))));
            if let Some(pos) = pos {
                let child = para.content.remove(pos);
                if let ParaChild::Run(Run::Drawing(d)) = child {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// Renders an arbitrary laid-out element to a PNG and embeds it as an inline
/// picture — the generic fallback for any block the dispatch cannot represent
/// natively (custom layout, raw frames, boxes, …).
///
/// INTEGRATION-NEEDED: rasterizing a `Frame`/`Page` to PNG bytes requires
/// `typst-render` (→ `tiny-skia` pixmap → `Pixmap::encode_png`) **and** layout
/// of the element to a `Frame`. Neither `typst-render` nor an image encoder is
/// a dependency of `typst-docx`, and adding one edits `Cargo.toml`, which the
/// mapper phase may not touch. The intended integration is:
///
/// 1. Add `typst-render` (and `typst-layout` if the element must first be laid
///    out) to `crates/typst-docx/Cargo.toml`.
/// 2. Lay the element out to a single-page `Frame`/`PagedDocument` and call
///    `typst_render::render(&page, &RenderOptions { pixel_per_pt, .. })` to get
///    a `tiny_skia::Pixmap`, then `pixmap.encode_png()` for the bytes.
/// 3. Feed those bytes here: embed via `ctx.add_image(&png, "png")`, allocate a
///    `docpr_id`, and build the `Drawing` exactly as [`image`] does, using the
///    frame's point size for the extents.
///
/// Returns `None` only if the content lays out to nothing; callers then fall
/// back to `warn_ignored`.
pub fn laid_out_fallback(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let Some((rel, size)) = ctx.rasterize(content, styles, content.span())? else {
        return Ok(None);
    };
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel,
        w_emu: crate::props::abs_to_emu(size.x),
        h_emu: crate::props::abs_to_emu(size.y),
        alt: None,
        docpr_id,
        name,
        anchor: None,
        shape: None,
    })))
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Returns the bytes + lowercase extension to embed for an image, or `None` if
/// the format needs rasterization that is not yet available.
///
/// Raster *exchange* formats (PNG/JPEG/GIF) are embedded verbatim — no
/// re-encode, preserving fidelity. WebP, SVG and PDF return `None` (see the
/// module docs / [`laid_out_fallback`] INTEGRATION-NEEDED note).
fn embeddable_bytes(image: &Image) -> Option<(Vec<u8>, EcoString)> {
    match image.kind() {
        ImageKind::Raster(raster) => match raster.format() {
            RasterFormat::Exchange(ExchangeFormat::Png) => {
                Some((raster.data().to_vec(), "png".into()))
            }
            RasterFormat::Exchange(ExchangeFormat::Jpg) => {
                // Word's content-type Default for `.jpeg` covers `.jpg` too, but
                // we use the canonical `jpeg` extension to match the registered
                // Default content-type (`image/jpeg`).
                Some((raster.data().to_vec(), "jpeg".into()))
            }
            RasterFormat::Exchange(ExchangeFormat::Gif) => {
                Some((raster.data().to_vec(), "gif".into()))
            }
            // WebP is not a Word-native image type; it must be transcoded to
            // PNG. Pixel-format (raw) rasters likewise have no exchange bytes
            // to embed and must be PNG-encoded.
            //
            // INTEGRATION-NEEDED: transcode WebP / raw-pixel rasters to PNG.
            // `RasterImage::dynamic()` yields an `image::DynamicImage` that can
            // be `write_to(.., ImageFormat::Png)`-encoded, but the `image`
            // crate is not a direct dependency of `typst-docx`. Add it (or
            // route through a small helper exposed by `typst-library`) and
            // return `(png_bytes, "png")` here.
            RasterFormat::Exchange(ExchangeFormat::Webp)
            | RasterFormat::Pixel(_) => None,
        },
        // Vector sources (SVG / PDF) must be rasterized to PNG. See
        // `laid_out_fallback`'s INTEGRATION-NEEDED note (needs `typst-render`).
        ImageKind::Svg(_) | ImageKind::Pdf(_) => None,
    }
}

/// Computes the inline display extents `(cx, cy)` in EMU.
///
/// Resolution order, per axis:
/// 1. an explicit *absolute* `width`/`height` on the element → used directly;
/// 2. otherwise the image's intrinsic point-size (`pixels / dpi × 72`), scaled
///    proportionally if the *other* axis was given absolutely (preserving the
///    aspect ratio — Word's `noChangeAspect` lock assumes the extents already
///    match the picture).
///
/// Relative (`%`) and fractional (`fr`) sizes have no fixed value without
/// layout, so they fall back to the intrinsic size.
fn display_extents(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    image: &Image,
) -> (i64, i64) {
    // Font size, to resolve any `em` component of an absolute length.
    let font_size = styles.resolve(TextElem::size);

    // Intrinsic point dimensions from pixels + DPI.
    let dpi = image.dpi().unwrap_or(Image::DEFAULT_DPI).max(1.0);
    let intrinsic_w_pt = image.width() / dpi * 72.0;
    let intrinsic_h_pt = image.height() / dpi * 72.0;
    let aspect = if intrinsic_w_pt > 0.0 { intrinsic_h_pt / intrinsic_w_pt } else { 1.0 };

    // Resolve an explicit absolute width, if any.
    let abs_w: Option<Abs> = match elem.width.get(styles) {
        Smart::Custom(rel) if rel.rel.is_zero() => Some(rel.abs.at(font_size)),
        _ => None,
    };
    // Resolve an explicit absolute height, if any.
    let abs_h: Option<Abs> = match elem.height.get(styles) {
        Sizing::Rel(rel) if rel.rel.is_zero() => Some(rel.abs.at(font_size)),
        _ => None,
    };

    let (w_pt, h_pt) = match (abs_w, abs_h) {
        // Both explicit: use as given (may distort — mirrors `fit: stretch`).
        (Some(w), Some(h)) => (w.to_pt(), h.to_pt()),
        // Width only: scale height to preserve aspect ratio.
        (Some(w), None) => (w.to_pt(), w.to_pt() * aspect),
        // Height only: scale width to preserve aspect ratio.
        (None, Some(h)) => {
            let w = if aspect > 0.0 { h.to_pt() / aspect } else { h.to_pt() };
            (w, h.to_pt())
        }
        // Neither: intrinsic size.
        (None, None) => (intrinsic_w_pt, intrinsic_h_pt),
    };

    // Guard against degenerate zero/negative extents (Word rejects `cx="0"`).
    let cx = ((w_pt * EMU_PER_PT).round() as i64).max(1);
    let cy = ((h_pt * EMU_PER_PT).round() as i64).max(1);
    (cx, cy)
}

/// Attaches a bookmark `(id, name)` bracketing the given blocks.
///
/// The `bookmarkStart` is prepended to the first paragraph and the matching
/// `bookmarkEnd` appended to the last paragraph. If there is no paragraph (the
/// figure produced only tables, or nothing), a fresh empty paragraph carrying a
/// zero-length bookmark is inserted — a legal cross-reference target.
fn attach_bookmark(blocks: &mut Vec<Block>, id: u32, name: EcoString) {
    // Find the first and last paragraph indices.
    let first_para = blocks.iter().position(|b| matches!(b, Block::Para(_)));
    let last_para = blocks.iter().rposition(|b| matches!(b, Block::Para(_)));

    match (first_para, last_para) {
        (Some(first), Some(last)) => {
            if let Block::Para(p) = &mut blocks[first] {
                p.content.insert(0, ParaChild::BookmarkStart { id, name });
            }
            if let Block::Para(p) = &mut blocks[last] {
                p.content.push(ParaChild::BookmarkEnd { id });
            }
        }
        _ => {
            // No paragraph to bracket: emit a zero-length bookmark paragraph at
            // the front so the target still exists.
            blocks.insert(
                0,
                Block::Para(Para {
                    props: ParaProps::default(),
                    content: vec![
                        ParaChild::BookmarkStart { id, name },
                        ParaChild::BookmarkEnd { id },
                    ],
                }),
            );
        }
    }
}

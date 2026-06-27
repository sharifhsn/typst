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
use typst_library::layout::{Abs, OuterVAlignment, Sizing};
use typst_library::model::FigureElem;
use typst_library::text::TextElem;
use typst_library::visualize::{
    ExchangeFormat, Image, ImageElem, ImageKind, RasterFormat,
};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Drawing, Jc, Para, ParaChild, ParaProps, Run, RunProps,
};

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
        // No native Word-embeddable representation and no rasterizer available
        // yet (WebP / SVG / PDF). Warn and drop rather than corrupt the package.
        ctx.warn_ignored(
            "image (vector/WebP rasterization not yet available in DOCX export)",
            span,
        );
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

    Ok(Run::Drawing(Drawing { rel, w_emu, h_emu, alt, docpr_id, name }))
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

    // Realize the caption (supplement + number + separator + body) into a
    // `Caption`-styled paragraph, if the figure has one.
    let caption = elem.caption.get_cloned(styles);
    let (position, caption_blocks) = match &caption {
        Some(cap) => {
            let position = cap.position.get(styles);
            let realized = cap.realize(ctx.engine(), styles)?;
            let runs = ctx.inline_runs(&realized, styles, RunProps::default())?;
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

    // Lower the figure body. Figures are centered by their show-set rule; mirror
    // that by centering each top-level paragraph the body produces.
    let mut body_blocks = ctx.blocks(&elem.body, styles)?;
    for block in &mut body_blocks {
        if let Block::Para(para) = block
            && para.props.jc.is_none()
        {
            para.props.jc = Some(Jc::Center);
        }
    }

    // Assemble in caption-position order.
    match position {
        OuterVAlignment::Top => {
            blocks.extend(caption_blocks);
            blocks.extend(body_blocks);
        }
        OuterVAlignment::Bottom => {
            blocks.extend(body_blocks);
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

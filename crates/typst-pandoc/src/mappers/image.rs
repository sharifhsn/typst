//! Image mapper: `ImageElem` → `Image Attr [alt] (url, title)`.
//!
//! Implemented. A raster *exchange* image (PNG/JPEG/GIF) is passed THROUGH
//! directly: its original encoded bytes are embedded as a self-contained `data:`
//! URI (no re-encode, no media files — `ctx.add_image` base64-encodes the
//! original bytes), so fidelity and file size are preserved and every pandoc
//! writer can recover the picture. SVG/PDF/WebP and raw-pixel rasters have no
//! universally-recoverable exchange form, so they are rasterized to a PNG
//! data-URI via `ctx.rasterize` (the universal fallback).
//!
//! The resolved `width`/`height` become Pandoc `Attr` key/value pairs
//! (`["width","60%"]`, `["height","2cm"]`) — the dimension vocabulary every
//! pandoc writer understands (LaTeX `\includegraphics`, HTML `style`, …). The
//! image's `alt` text becomes the `Image` alt-inline sequence.
//
// The dispatch in `convert::handle_inline` routes a standalone block-level image
// here too (it reaches `handle_inline` as a single inline child of its own
// `Para`), so no separate block entry point is needed.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, Smart, StyleChain};
use typst_library::layout::{Length, Rel, Sizing};
use typst_library::visualize::{
    ExchangeFormat, Image, ImageElem, ImageKind, RasterFormat,
};
use typst_utils::Numeric;

use crate::ast::{Attr, Inline, empty_attr};
use crate::ctx::PandocCtx;

use super::rasterize_inline;

/// Lowers an [`ImageElem`] into a single Pandoc `Image` inline.
///
/// PNG/JPEG/GIF pass through as a data-URI of the original bytes; every other
/// kind (SVG/PDF/WebP/raw pixels) is rasterized to a PNG data-URI. The `Attr`
/// carries the resolved `width`/`height` as pandoc dimension kvs, and the alt
/// text becomes the alt-inline sequence.
pub fn image_inline(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Inline> {
    // Decode so we know the real format (the `format` field may be `Auto`).
    let decoded = elem.decode(ctx.engine(), styles)?;

    // Try to pass the original encoded bytes through directly.
    let Some((bytes, ext)) = passthrough_bytes(&decoded) else {
        // SVG/PDF/WebP/raw pixels: rasterize to a PNG data-URI. The rasterized
        // node already carries no size attrs; re-attach the resolved width/
        // height + alt so an explicit `width: 60%` still reaches the writer.
        let raster = rasterize_inline(elem.pack_ref(), styles, ctx)?;
        return Ok(finish_rasterized(raster, elem, styles));
    };

    // Embed the original bytes as a self-contained `data:` URI.
    let url = ctx.add_image(&bytes, &ext);

    Ok(Inline::Image(
        size_attr(elem, styles),
        alt_inlines(elem, styles),
        (url.to_string(), String::new()),
    ))
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Returns the original encoded bytes + a lowercase extension for a raster
/// *exchange* image (PNG/JPEG/GIF), or `None` if the image needs rasterization
/// (SVG, PDF, WebP, or a raw pixel buffer with no exchange encoding).
fn passthrough_bytes(image: &Image) -> Option<(Vec<u8>, EcoString)> {
    match image.kind() {
        ImageKind::Raster(raster) => match raster.format() {
            RasterFormat::Exchange(ExchangeFormat::Png) => {
                Some((raster.data().to_vec(), "png".into()))
            }
            RasterFormat::Exchange(ExchangeFormat::Jpg) => {
                Some((raster.data().to_vec(), "jpeg".into()))
            }
            RasterFormat::Exchange(ExchangeFormat::Gif) => {
                Some((raster.data().to_vec(), "gif".into()))
            }
            // WebP has no broadly-recoverable form in pandoc's writers, and a
            // raw pixel buffer has no exchange bytes to embed → rasterize.
            RasterFormat::Exchange(ExchangeFormat::Webp) | RasterFormat::Pixel(_) => None,
        },
        // Vector sources (SVG / PDF) → rasterize (current path). Passing SVG
        // bytes through as `image/svg+xml` is a possible future refinement, but
        // not every pandoc writer renders an SVG data-URI, so rasterizing is the
        // safe, universally-recoverable choice.
        ImageKind::Svg(_) | ImageKind::Pdf(_) => None,
    }
}

/// Builds the `Attr` carrying the resolved `width`/`height` as pandoc dimension
/// kvs. Only *representable* sizes are emitted: an absolute length (`2cm`,
/// `100pt`) or a ratio (`60%`). `Auto`, fractional (`fr`), and font-relative
/// (`em`) sizes have no fixed pandoc dimension without layout and are dropped
/// (the picture then uses its intrinsic size, as in markdown).
fn size_attr(elem: &Packed<ImageElem>, styles: StyleChain) -> Attr {
    let mut kvs: Vec<(String, String)> = Vec::new();

    if let Some(w) = width_dim(elem.width.get(styles)) {
        kvs.push(("width".into(), w));
    }
    if let Some(h) = height_dim(elem.height.get(styles)) {
        kvs.push(("height".into(), h));
    }

    (String::new(), Vec::new(), kvs)
}

/// Formats a `width: Smart<Rel<Length>>` as a pandoc dimension string, or `None`
/// if it is `Auto` / not cleanly representable.
fn width_dim(width: Smart<Rel<Length>>) -> Option<String> {
    match width {
        Smart::Custom(rel) => rel_dim(rel),
        Smart::Auto => None,
    }
}

/// Formats a `height: Sizing` as a pandoc dimension string, or `None` if it is
/// `Auto` / fractional / not cleanly representable.
fn height_dim(height: Sizing) -> Option<String> {
    match height {
        Sizing::Rel(rel) => rel_dim(rel),
        Sizing::Auto | Sizing::Fr(_) => None,
    }
}

/// Formats a `Rel<Length>` (relative + absolute parts) as a pandoc dimension.
///
/// A pure ratio (`60%`, no absolute part) → `"60%"`. A pure absolute length
/// (`2cm`) → its point value `"56.69pt"`. A mixed `100% - 2pt` or one carrying a
/// font-relative `em` part has no single fixed pandoc dimension without layout,
/// so it is dropped (`None`).
fn rel_dim(rel: Rel<Length>) -> Option<String> {
    let has_ratio = !rel.rel.is_zero();
    let has_abs = !rel.abs.abs.is_zero();
    let has_em = !rel.abs.em.is_zero();

    // A font-relative component can't be resolved to a fixed dimension here.
    if has_em {
        return None;
    }

    match (has_ratio, has_abs) {
        // Pure ratio: pandoc accepts a bare percentage, e.g. "60%".
        (true, false) => Some(fmt_num(rel.rel.get() * 100.0) + "%"),
        // Pure absolute length: emit its point value, e.g. "56.69pt".
        (false, true) => Some(fmt_num(rel.abs.abs.to_pt()) + "pt"),
        // Zero (both parts zero) or mixed ratio+absolute: not representable.
        _ => None,
    }
}

/// Formats a number compactly: integers without a trailing `.0`, otherwise up
/// to a few decimals with trailing zeros trimmed.
fn fmt_num(n: f64) -> String {
    if n.fract().abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        let s = format!("{n:.4}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    }
}

/// The alt-inline sequence for the image (`[Str alt]`, or empty when unset).
fn alt_inlines(elem: &Packed<ImageElem>, styles: StyleChain) -> Vec<Inline> {
    match elem.alt.get_ref(styles) {
        Some(alt) if !alt.is_empty() => vec![Inline::Str(alt.to_string())],
        _ => Vec::new(),
    }
}

/// Re-attaches the resolved size `Attr` + alt text to a rasterized `Image`
/// (the universal `rasterize_inline` fallback emits a bare `Image` with an empty
/// attr and no alt). If the image laid out to nothing, falls back to an empty
/// alt-less placeholder `Image` so the node count stays stable.
fn finish_rasterized(
    raster: Vec<Inline>,
    elem: &Packed<ImageElem>,
    styles: StyleChain,
) -> Inline {
    match raster.into_iter().next() {
        Some(Inline::Image(_attr, _alt, target)) => {
            Inline::Image(size_attr(elem, styles), alt_inlines(elem, styles), target)
        }
        // Either the fallback dropped it (laid out to nothing) or produced a
        // non-image (shouldn't happen): emit a stable empty placeholder.
        _ => Inline::Image(empty_attr(), Vec::new(), (String::new(), String::new())),
    }
}

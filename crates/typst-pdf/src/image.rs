use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use ecow::eco_format;
use image::{DynamicImage, EncodableLayout, GenericImageView, ImageFormat, Rgba};
use krilla::image::{BitsPerComponent, CustomImage, ImageColorspace};
use krilla::pdf::PdfDocument;
use krilla::surface::Surface;
use krilla_svg::{SurfaceExt, SvgSettings};
use typst_library::diag::{At, SourceResult};
use typst_library::foundations::{Bytes, Smart};
use typst_library::layout::{Abs, Angle, Ratio, Size, Transform};
use typst_library::visualize::{
    ExchangeFormat, Image, ImageKind, ImageScaling, PdfImage, RasterFormat, RasterImage,
};
use typst_syntax::Span;
use typst_utils::{defer, hash128};

use crate::convert::{FrameContext, GlobalContext};
use crate::tags;
use crate::util::{SizeExt, TransformExt};

#[typst_macros::time(name = "handle image")]
pub(crate) fn handle_image(
    gc: &mut GlobalContext,
    fc: &mut FrameContext,
    image: &Image,
    size: Size,
    surface: &mut Surface,
    span: Span,
) -> SourceResult<()> {
    gc.image_spans.insert(span);

    // Draw-skip reuse: run the tag (bbox + marked-content id + tree leaf), but
    // skip the surface transform and the actual image drawing — the content
    // stream is injected from the cache. (Image pages are not `is_simple`, so
    // they don't actually get reused; this branch is defensive.)
    if fc.draw_skip {
        let _handle = tags::image(gc, fc, surface, image, size);
        return Ok(());
    }

    surface.push_transform(&fc.state().transform().to_krilla());
    surface.set_location(span.into_raw());
    let mut surface = defer(surface, |s| {
        s.pop();
        s.reset_location();
    });

    let interpolate = image.scaling() == Smart::Custom(ImageScaling::Smooth);

    let mut handle = tags::image(gc, fc, &mut surface, image, size);
    let surface = handle.surface();

    match image.kind() {
        ImageKind::Raster(raster) => {
            // Optionally downsample an oversized raster to the configured DPI
            // cap. The downsampled image bakes in any EXIF rotation and is
            // re-encoded (PNG, or JPEG for JPEG sources), so it is drawn with an
            // identity transform at the layout size (no EXIF compensation).
            let downsampled = gc.options.image_dpi.and_then(|dpi| {
                downsample_raster(raster, size, fc.state().transform(), dpi)
            });

            let (transform, draw_size, raster) = match downsampled {
                Some(ds) => (Transform::identity(), size, ds),
                None => {
                    let (transform, new_size) = exif_transform(raster, size);
                    (transform, new_size, raster.clone())
                }
            };

            surface.push_transform(&transform.to_krilla());
            let mut surface = defer(surface, |s| s.pop());

            let image = convert_raster(raster, interpolate)
                .map_err(|err| eco_format!("failed to process image ({err})"))
                .at(span)?;

            if !gc.image_to_spans.contains_key(&image) {
                gc.image_to_spans.insert(image.clone(), span);
            }

            if let Some(size) = draw_size.to_krilla() {
                surface.draw_image(image, size);
            }
        }
        ImageKind::Svg(svg) => {
            if let Some(size) = size.to_krilla() {
                // Convert each unique SVG to a reusable form XObject once (keyed
                // by content hash), rendered at the SVG's native size. krilla then
                // emits the XObject a single time and references it via `/Do` on
                // every use, instead of re-converting the tree and re-emitting all
                // of its drawing operations into the content stream each time.
                let hash = hash128(svg);
                if !gc.svg_graphics.contains_key(&hash) {
                    let tree = svg.tree();
                    if let Some(native) = krilla::geom::Size::from_wh(
                        tree.size().width(),
                        tree.size().height(),
                    ) {
                        let mut stream_builder = surface.stream_builder();
                        let mut sub = stream_builder.surface();
                        sub.draw_svg(
                            tree,
                            native,
                            SvgSettings { embed_text: true, ..Default::default() },
                        );
                        sub.finish();
                        let graphic =
                            krilla::graphic::Graphic::new(stream_builder.finish(), false);
                        gc.svg_graphics.insert(hash, graphic);
                    }
                }

                match gc.svg_graphics.get(&hash) {
                    Some(graphic) => {
                        // Scale the native-size graphic to the requested size.
                        let graphic = graphic.clone();
                        let native = svg.tree().size();
                        surface.push_transform(&krilla::geom::Transform::from_scale(
                            size.width() / native.width(),
                            size.height() / native.height(),
                        ));
                        surface.draw_graphic(graphic);
                        surface.pop();
                    }
                    // Degenerate SVG size; fall back to drawing it inline.
                    None => {
                        surface.draw_svg(
                            svg.tree(),
                            size,
                            SvgSettings { embed_text: true, ..Default::default() },
                        );
                    }
                }
            }
        }
        ImageKind::Pdf(pdf) => {
            if let Some(size) = size.to_krilla() {
                surface.draw_pdf_page(&convert_pdf(pdf), size, pdf.page_index());
            }
        }
    }

    Ok(())
}

/// A wrapper around `RasterImage` so that we can implement `CustomImage`.
#[derive(Clone)]
struct PdfRasterImage(Arc<PdfRasterImageInner>);

/// The internal representation of a [`PdfRasterImage`].
struct PdfRasterImageInner {
    /// The original, underlying raster image.
    raster: RasterImage,
    /// The alpha channel of the raster image, if existing.
    alpha_channel: OnceLock<Option<Vec<u8>>>,
    /// A (potentially) converted version of the dynamic image stored `raster` that is
    /// guaranteed to either be in luma8 or rgb8, and thus can be used for the
    /// `color_channel` method of `CustomImage`.
    actual_dynamic: OnceLock<Arc<DynamicImage>>,
}

impl PdfRasterImage {
    /// Wraps a raster image.
    pub fn new(raster: RasterImage) -> Self {
        Self(Arc::new(PdfRasterImageInner {
            raster,
            alpha_channel: OnceLock::new(),
            actual_dynamic: OnceLock::new(),
        }))
    }
}

impl Hash for PdfRasterImage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // `alpha_channel` and `actual_dynamic` are generated from the underlying `RasterImage`,
        // so this is enough. Since `raster` is prehashed, this is also very cheap.
        self.0.raster.hash(state);
    }
}

impl CustomImage for PdfRasterImage {
    fn color_channel(&self) -> &[u8] {
        self.0
            .actual_dynamic
            .get_or_init(|| {
                let dynamic = self.0.raster.dynamic();
                let channel_count = dynamic.color().channel_count();

                match (dynamic.as_ref(), channel_count) {
                    // Pure luma8 or rgb8 image, can use it directly.
                    (DynamicImage::ImageLuma8(_), _) => dynamic.clone(),
                    (DynamicImage::ImageRgb8(_), _) => dynamic.clone(),
                    // Grey-scale image, convert to luma8.
                    (_, 1 | 2) => Arc::new(DynamicImage::ImageLuma8(dynamic.to_luma8())),
                    // Anything else, convert to rgb8.
                    _ => Arc::new(DynamicImage::ImageRgb8(dynamic.to_rgb8())),
                }
            })
            .as_bytes()
    }

    fn alpha_channel(&self) -> Option<&[u8]> {
        self.0
            .alpha_channel
            .get_or_init(|| {
                self.0.raster.dynamic().color().has_alpha().then(|| {
                    self.0
                        .raster
                        .dynamic()
                        .pixels()
                        .map(|(_, _, Rgba([_, _, _, a]))| a)
                        .collect()
                })
            })
            .as_ref()
            .map(|v| &**v)
    }

    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }

    fn size(&self) -> (u32, u32) {
        (self.0.raster.width(), self.0.raster.height())
    }

    fn icc_profile(&self) -> Option<&[u8]> {
        if matches!(
            self.0.raster.dynamic().as_ref(),
            DynamicImage::ImageLuma8(_)
                | DynamicImage::ImageLumaA8(_)
                | DynamicImage::ImageRgb8(_)
                | DynamicImage::ImageRgba8(_)
        ) {
            self.0.raster.icc().map(|b| b.as_bytes())
        } else {
            // In all other cases, the dynamic will be converted into RGB8 or LUMA8, so the ICC
            // profile may become invalid, and thus we don't include it.
            None
        }
    }

    fn color_space(&self) -> ImageColorspace {
        // Remember that we convert all images to either RGB or luma.
        if self.0.raster.dynamic().color().has_color() {
            ImageColorspace::Rgb
        } else {
            ImageColorspace::Luma
        }
    }
}

/// Downsamples a raster so it has no more than `dpi` pixels per inch at its
/// rendered size. Returns `None` (leaving the image untouched) when it is
/// already at or below the target resolution, carries an ICC color profile, or
/// has a degenerate size.
fn downsample_raster(
    raster: &RasterImage,
    size: Size,
    transform: Transform,
    dpi: u32,
) -> Option<RasterImage> {
    // Preserve color-managed images: re-encoding as PNG would drop the ICC
    // profile and could shift colors.
    if raster.icc().is_some() {
        return None;
    }

    let (source_w, source_h) = (raster.width(), raster.height());
    if source_w == 0 || source_h == 0 {
        return None;
    }

    // The rendered size in inches, accounting for the active transform's scale
    // (an image placed inside a scaled group renders larger or smaller than its
    // layout box). 1pt = 1/72in.
    let scale_x = (transform.sx.get().powi(2) + transform.ky.get().powi(2)).sqrt();
    let scale_y = (transform.kx.get().powi(2) + transform.sy.get().powi(2)).sqrt();
    let inches_w = size.x.to_pt() * scale_x / 72.0;
    let inches_h = size.y.to_pt() * scale_y / 72.0;
    if !(inches_w > 0.0 && inches_h > 0.0) {
        return None;
    }

    // Target pixel dimensions, preserving the source aspect ratio.
    let target_w = (inches_w * dpi as f64).ceil();
    let target_h = (inches_h * dpi as f64).ceil();
    let factor = (target_w / source_w as f64).min(target_h / source_h as f64);
    if factor >= 1.0 {
        return None;
    }
    let new_w = ((source_w as f64 * factor).round() as u32).max(1);
    let new_h = ((source_h as f64 * factor).round() as u32).max(1);
    if new_w >= source_w && new_h >= source_h {
        return None;
    }

    downsample_impl(raster.clone(), new_w, new_h)
}

/// Quality used when re-encoding a downsampled JPEG. Photographic content
/// downsampled to a display-resolution cap is visually unaffected at this level,
/// while staying far smaller than a lossless re-encode.
const DOWNSAMPLE_JPEG_QUALITY: u8 = 85;

/// Resizes a raster to `new_w`×`new_h` and re-encodes it. JPEG sources are
/// re-encoded as JPEG to keep their photographic (DCT) compression; every other
/// format is re-encoded as PNG (lossless, suited to graphics, text, and
/// screenshots). The downsampled image always bakes in any EXIF rotation and
/// carries no orientation tag, so callers draw it with an identity transform.
/// Memoized so a repeated (image, size) pair is resampled only once and the
/// result stays deduplicated by krilla via its content hash.
#[comemo::memoize]
fn downsample_impl(raster: RasterImage, new_w: u32, new_h: u32) -> Option<RasterImage> {
    let resized = raster
        .dynamic()
        .resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);
    let mut buf = Vec::new();
    let format = if let RasterFormat::Exchange(ExchangeFormat::Jpg) = raster.format() {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut buf,
            DOWNSAMPLE_JPEG_QUALITY,
        );
        // JPEG has no alpha channel; ensure an RGB8 buffer before encoding.
        DynamicImage::ImageRgb8(resized.to_rgb8()).write_with_encoder(encoder).ok()?;
        ExchangeFormat::Jpg
    } else {
        resized.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png).ok()?;
        ExchangeFormat::Png
    };
    RasterImage::plain(Bytes::new(buf), format).ok()
}

#[comemo::memoize]
fn convert_raster(
    raster: RasterImage,
    interpolate: bool,
) -> Result<krilla::image::Image, String> {
    // JPEG (DCTDecode) and eligible PNGs (FlateDecode + a PNG predictor) can be
    // embedded into the PDF losslessly, so we hand krilla the original encoded
    // bytes instead of decoded-and-re-encoded pixels. Other rasters keep the
    // `from_custom` path, which reuses the pixels we already decoded.
    match raster.format() {
        RasterFormat::Exchange(ExchangeFormat::Jpg) => {
            let image_data: Arc<dyn AsRef<[u8]> + Send + Sync> =
                Arc::new(raster.data().clone());
            krilla::image::Image::from_jpeg_with_icc(
                image_data.into(),
                icc_data(&raster),
                interpolate,
            )
        }
        RasterFormat::Exchange(ExchangeFormat::Png)
            if png_passthrough_eligible(raster.data()) =>
        {
            let image_data: Arc<dyn AsRef<[u8]> + Send + Sync> =
                Arc::new(raster.data().clone());
            krilla::image::Image::from_png_with_icc(
                image_data.into(),
                icc_data(&raster),
                interpolate,
            )
        }
        _ => krilla::image::Image::from_custom(PdfRasterImage::new(raster), interpolate),
    }
}

/// The image's resolved ICC profile as krilla [`Data`](krilla::Data), if any.
fn icc_data(raster: &RasterImage) -> Option<krilla::Data> {
    raster.icc().map(|i| {
        let i: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(i.clone());
        i.into()
    })
}

/// Whether a PNG qualifies for krilla's lossless passthrough: non-interlaced,
/// opaque (no alpha channel and no `tRNS` chunk), grayscale or truecolor, at a
/// bit depth of 8. This mirrors krilla's own eligibility check so that we only
/// divert PNGs from the established `from_custom` decode path when the result is
/// byte-for-byte the same image. Reads only the PNG's chunk headers, not pixels.
fn png_passthrough_eligible(data: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 8 || &data[..8] != SIGNATURE {
        return false;
    }

    let mut pos = 8;
    let mut eligible = false;
    while pos + 8 <= data.len() {
        let Ok(len_bytes) = data[pos..pos + 4].try_into() else { return false };
        let len = u32::from_be_bytes(len_bytes) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = pos + 8;
        let Some(end) = body.checked_add(len) else { return false };
        // Each chunk's data is followed by a 4-byte CRC.
        if end.checked_add(4).is_none_or(|e| e > data.len()) {
            return false;
        }

        match kind {
            b"IHDR" if len >= 13 => {
                let bit_depth = data[body + 8];
                let color_type = data[body + 9];
                let interlace = data[body + 12];
                // Mirror krilla's `png_passthrough` eligibility: non-interlaced,
                // and grayscale (0) / palette (3) at 1/2/4/8-bit or truecolor
                // RGB (2) at 8-bit. krilla still validates the finer points (a
                // present `PLTE`, no embedded ICC on palette) and falls back to
                // decoding, so this only needs to gate the common case to avoid
                // re-decoding images that can't be passed through.
                eligible = interlace == 0
                    && match color_type {
                        0 | 3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                        2 => bit_depth == 8,
                        _ => false,
                    };
            }
            // Color-key transparency expands to an alpha mask, so disqualify. It
            // always precedes the image data, so checking up to `IDAT` suffices.
            b"tRNS" => return false,
            b"IDAT" | b"IEND" => break,
            _ => {}
        }

        pos = end + 4;
    }

    eligible
}

#[comemo::memoize]
fn convert_pdf(pdf: &PdfImage) -> PdfDocument {
    PdfDocument::new(pdf.document().pdf().clone())
}

fn exif_transform(image: &RasterImage, size: Size) -> (Transform, Size) {
    // For JPEGs, we want to apply the EXIF orientation as a transformation
    // because we don't recode them. For other formats, the transform is already
    // baked into the dynamic image data.
    if image.format() != RasterFormat::Exchange(ExchangeFormat::Jpg) {
        return (Transform::identity(), size);
    }

    let base = |hp: bool, vp: bool, mut base_ts: Transform, size: Size| {
        if hp {
            // Flip horizontally in-place.
            base_ts = base_ts.pre_concat(
                Transform::scale(-Ratio::one(), Ratio::one())
                    .pre_concat(Transform::translate(-size.x, Abs::zero())),
            )
        }

        if vp {
            // Flip vertically in-place.
            base_ts = base_ts.pre_concat(
                Transform::scale(Ratio::one(), -Ratio::one())
                    .pre_concat(Transform::translate(Abs::zero(), -size.y)),
            )
        }

        base_ts
    };

    let no_flipping =
        |hp: bool, vp: bool| (base(hp, vp, Transform::identity(), size), size);

    let with_flipping = |hp: bool, vp: bool| {
        let base_ts = Transform::rotate_at(Angle::deg(90.0), Abs::zero(), Abs::zero())
            .pre_concat(Transform::scale(Ratio::one(), -Ratio::one()));
        let inv_size = Size::new(size.y, size.x);
        (base(hp, vp, base_ts, inv_size), inv_size)
    };

    match image.exif_rotation() {
        Some(2) => no_flipping(true, false),
        Some(3) => no_flipping(true, true),
        Some(4) => no_flipping(false, true),
        Some(5) => with_flipping(false, false),
        Some(6) => with_flipping(false, true),
        Some(7) => with_flipping(true, true),
        Some(8) => with_flipping(true, false),
        _ => no_flipping(false, false),
    }
}

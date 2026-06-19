use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::{Arc, OnceLock};

use crate::diag::{StrResult, bail};
use crate::foundations::{Bytes, Cast, Dict, Smart, Value, cast, dict};
use ecow::{EcoString, eco_format};
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{
    DynamicImage, ImageBuffer, ImageDecoder, ImageResult, Limits, Pixel, guess_format,
};

/// A decoded raster image.
#[derive(Clone, Hash)]
pub struct RasterImage(Arc<RasterImageInner>);

/// The internal representation of a [`RasterImage`].
struct RasterImageInner {
    data: Bytes,
    format: RasterFormat,
    /// Pixel width, with any EXIF rotation already applied.
    width: u32,
    /// Pixel height, with any EXIF rotation already applied.
    height: u32,
    exif_rotation: Option<u32>,
    icc: Option<Bytes>,
    dpi: Option<f64>,
    /// The decoded pixels. For JPEGs this is filled lazily on first
    /// [`dynamic`](RasterImage::dynamic) access, since they are embedded
    /// losslessly into PDF/SVG output without ever decoding the pixels. All
    /// other formats populate it eagerly during construction.
    dynamic: OnceLock<Arc<DynamicImage>>,
}

impl RasterImage {
    /// Decode a raster image.
    pub fn new(
        data: Bytes,
        format: impl Into<RasterFormat>,
        icc: Smart<Bytes>,
    ) -> StrResult<Self> {
        Self::new_impl(data, format.into(), icc)
    }

    /// Create a raster image with optional properties set to the default.
    pub fn plain(data: Bytes, format: impl Into<RasterFormat>) -> StrResult<Self> {
        Self::new(data, format, Smart::Auto)
    }

    /// The internal, non-generic implementation.
    #[comemo::memoize]
    #[typst_macros::time(name = "load raster image")]
    fn new_impl(
        data: Bytes,
        format: RasterFormat,
        icc: Smart<Bytes>,
    ) -> StrResult<RasterImage> {
        let inner = match format {
            RasterFormat::Exchange(exchange) => {
                // Read metadata from the header without decoding the pixels.
                let exif = exif::Reader::new()
                    .read_from_container(&mut io::Cursor::new(&data))
                    .ok();
                let exif_rotation = exif.as_ref().and_then(exif_rotation);
                let dpi = determine_dpi(&data, exif.as_ref());

                match exchange {
                    // JPEGs and PNGs are embedded into PDF/SVG output without
                    // their decoded pixels (JPEG via DCTDecode; PNG via
                    // FlateDecode + a predictor, with any alpha split off into a
                    // soft mask). So read just the dimensions and ICC profile from
                    // the header now and defer the expensive pixel decode to the
                    // first `dynamic()` access — needed only when rasterizing.
                    ExchangeFormat::Jpg | ExchangeFormat::Png => {
                        let (w, h, icc) = if exchange == ExchangeFormat::Jpg {
                            let mut decoder = JpegDecoder::new(io::Cursor::new(&data))
                                .map_err(format_image_error)?;
                            let (w, h) = decoder.dimensions();
                            (w, h, pick_icc(icc, &mut decoder))
                        } else {
                            let mut decoder = PngDecoder::new(io::Cursor::new(&data))
                                .map_err(format_image_error)?;
                            let (w, h) = decoder.dimensions();
                            (w, h, pick_icc(icc, &mut decoder))
                        };
                        let (width, height) = rotated_dimensions(w, h, exif_rotation);
                        RasterImageInner {
                            data,
                            format,
                            width,
                            height,
                            exif_rotation,
                            icc,
                            dpi,
                            dynamic: OnceLock::new(),
                        }
                    }
                    // GIF and WebP are always rasterized for output, so decode
                    // eagerly to surface any error at load time.
                    ExchangeFormat::Gif | ExchangeFormat::Webp => {
                        let (dynamic, icc) =
                            decode_exchange(exchange, &data, icc, exif_rotation)
                                .map_err(format_image_error)?;
                        let (width, height) = (dynamic.width(), dynamic.height());
                        let cell = OnceLock::new();
                        let _ = cell.set(Arc::new(dynamic));
                        RasterImageInner {
                            data,
                            format,
                            width,
                            height,
                            exif_rotation,
                            icc,
                            dpi,
                            dynamic: cell,
                        }
                    }
                }
            }

            RasterFormat::Pixel(pixel) => {
                let dynamic = decode_pixel(&data, pixel)?;
                let cell = OnceLock::new();
                let _ = cell.set(Arc::new(dynamic));
                RasterImageInner {
                    data,
                    format,
                    width: pixel.width,
                    height: pixel.height,
                    exif_rotation: None,
                    icc: icc.custom(),
                    dpi: None,
                    dynamic: cell,
                }
            }
        };

        Ok(Self(Arc::new(inner)))
    }

    /// The raw image data.
    pub fn data(&self) -> &Bytes {
        &self.0.data
    }

    /// The image's format.
    pub fn format(&self) -> RasterFormat {
        self.0.format
    }

    /// The image's pixel width (with EXIF rotation applied).
    pub fn width(&self) -> u32 {
        self.0.width
    }

    /// The image's pixel height (with EXIF rotation applied).
    pub fn height(&self) -> u32 {
        self.0.height
    }

    /// The EXIF orientation value of the original image.
    ///
    /// The [`dynamic`](Self::dynamic) image already has this factored in. This
    /// value is only relevant to consumers of the raw [`data`](Self::data).
    pub fn exif_rotation(&self) -> Option<u32> {
        self.0.exif_rotation
    }

    /// The image's pixel density in pixels per inch, if known.
    ///
    /// This is guaranteed to be positive.
    pub fn dpi(&self) -> Option<f64> {
        self.0.dpi
    }

    /// Access the underlying dynamic image, decoding it on first access.
    ///
    /// For JPEGs the pixel decode is deferred to here, and is skipped entirely
    /// when the image is only embedded into PDF/SVG output.
    pub fn dynamic(&self) -> &Arc<DynamicImage> {
        self.0.dynamic.get_or_init(|| {
            let RasterFormat::Exchange(format) = self.0.format else {
                // Pixel images always populate the cell eagerly.
                unreachable!("pixel images are decoded eagerly");
            };
            let dynamic =
                decode_exchange(format, &self.0.data, Smart::Auto, self.0.exif_rotation)
                    .map(|(dynamic, _)| dynamic)
                    .unwrap_or_else(|_| {
                        // The header decoded fine but the pixel data is corrupt
                        // (rare). Degrade to a blank image so rendering doesn't
                        // panic; layout still uses the header dimensions.
                        DynamicImage::new_rgba8(1, 1)
                    });
            Arc::new(dynamic)
        })
    }

    /// Access the ICC profile, if any.
    pub fn icc(&self) -> Option<&Bytes> {
        self.0.icc.as_ref()
    }
}

impl Hash for RasterImageInner {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // The image is fully defined by data, format, and ICC profile.
        self.data.hash(state);
        self.format.hash(state);
        self.icc.hash(state);
    }
}

/// Pick the ICC profile: a user-provided one wins, else the image's embedded one.
fn pick_icc<T: ImageDecoder>(icc: Smart<Bytes>, decoder: &mut T) -> Option<Bytes> {
    icc.custom().or_else(|| {
        decoder
            .icc_profile()
            .ok()
            .flatten()
            .filter(|icc| !icc.is_empty())
            .map(Bytes::new)
    })
}

/// The pixel dimensions after applying an EXIF rotation. The 90°/270° rotations
/// (orientation values 5–8) swap width and height.
fn rotated_dimensions(width: u32, height: u32, rotation: Option<u32>) -> (u32, u32) {
    match rotation {
        Some(5 | 6 | 7 | 8) => (height, width),
        _ => (width, height),
    }
}

/// Fully decode an exchange-format image, extracting the ICC profile and
/// applying any EXIF rotation.
fn decode_exchange(
    format: ExchangeFormat,
    data: &[u8],
    icc: Smart<Bytes>,
    rotation: Option<u32>,
) -> ImageResult<(DynamicImage, Option<Bytes>)> {
    fn decode<T: ImageDecoder>(
        decoder: ImageResult<T>,
        icc: Smart<Bytes>,
        rotation: Option<u32>,
    ) -> ImageResult<(DynamicImage, Option<Bytes>)> {
        let mut decoder = decoder?;
        let icc = pick_icc(icc, &mut decoder);
        decoder.set_limits(Limits::default())?;
        let mut dynamic = DynamicImage::from_decoder(decoder)?;
        if let Some(rotation) = rotation {
            apply_rotation(&mut dynamic, rotation);
        }
        Ok((dynamic, icc))
    }

    let cursor = io::Cursor::new(data);
    match format {
        ExchangeFormat::Jpg => decode(JpegDecoder::new(cursor), icc, rotation),
        ExchangeFormat::Png => decode(PngDecoder::new(cursor), icc, rotation),
        ExchangeFormat::Gif => decode(GifDecoder::new(cursor), icc, rotation),
        ExchangeFormat::Webp => decode(WebPDecoder::new(cursor), icc, rotation),
    }
}

/// Build a dynamic image from raw, already-decoded pixel data.
fn decode_pixel(data: &Bytes, format: PixelFormat) -> StrResult<DynamicImage> {
    if format.width == 0 || format.height == 0 {
        bail!("zero-sized images are not allowed");
    }

    let channels = match format.encoding {
        PixelEncoding::Rgb8 => 3,
        PixelEncoding::Rgba8 => 4,
        PixelEncoding::Luma8 => 1,
        PixelEncoding::Lumaa8 => 2,
    };

    let Some(expected_size) = format
        .width
        .checked_mul(format.height)
        .and_then(|size| size.checked_mul(channels))
    else {
        bail!("pixel dimensions are too large");
    };

    if expected_size as usize != data.len() {
        bail!("pixel dimensions and pixel data do not match");
    }

    fn to<P: Pixel<Subpixel = u8>>(
        data: &Bytes,
        format: PixelFormat,
    ) -> ImageBuffer<P, Vec<u8>> {
        ImageBuffer::from_raw(format.width, format.height, data.to_vec()).unwrap()
    }

    Ok(match format.encoding {
        PixelEncoding::Rgb8 => to::<image::Rgb<u8>>(data, format).into(),
        PixelEncoding::Rgba8 => to::<image::Rgba<u8>>(data, format).into(),
        PixelEncoding::Luma8 => to::<image::Luma<u8>>(data, format).into(),
        PixelEncoding::Lumaa8 => to::<image::LumaA<u8>>(data, format).into(),
    })
}

/// A raster graphics format.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum RasterFormat {
    /// A format typically used in image exchange.
    Exchange(ExchangeFormat),
    /// A format of raw pixel data.
    Pixel(PixelFormat),
}

impl From<ExchangeFormat> for RasterFormat {
    fn from(format: ExchangeFormat) -> Self {
        Self::Exchange(format)
    }
}

impl From<PixelFormat> for RasterFormat {
    fn from(format: PixelFormat) -> Self {
        Self::Pixel(format)
    }
}

cast! {
    RasterFormat,
    self => match self {
        Self::Exchange(v) => v.into_value(),
        Self::Pixel(v) => v.into_value(),
    },
    v: ExchangeFormat => Self::Exchange(v),
    v: PixelFormat => Self::Pixel(v),
}

/// A raster format typically used in image exchange, with efficient encoding.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Cast)]
pub enum ExchangeFormat {
    /// Raster format for illustrations and transparent graphics.
    Png,
    /// Lossy raster format suitable for photos.
    Jpg,
    /// Raster format that is typically used for short animated clips. Typst can
    /// load GIFs, but they will become static.
    Gif,
    /// Raster format that supports both lossy and lossless compression.
    Webp,
}

impl ExchangeFormat {
    /// Try to detect the format of data in a buffer.
    pub fn detect(data: &[u8]) -> Option<Self> {
        guess_format(data).ok().and_then(|format| format.try_into().ok())
    }
}

impl From<ExchangeFormat> for image::ImageFormat {
    fn from(format: ExchangeFormat) -> Self {
        match format {
            ExchangeFormat::Png => image::ImageFormat::Png,
            ExchangeFormat::Jpg => image::ImageFormat::Jpeg,
            ExchangeFormat::Gif => image::ImageFormat::Gif,
            ExchangeFormat::Webp => image::ImageFormat::WebP,
        }
    }
}

impl TryFrom<image::ImageFormat> for ExchangeFormat {
    type Error = EcoString;

    fn try_from(format: image::ImageFormat) -> StrResult<Self> {
        Ok(match format {
            image::ImageFormat::Png => ExchangeFormat::Png,
            image::ImageFormat::Jpeg => ExchangeFormat::Jpg,
            image::ImageFormat::Gif => ExchangeFormat::Gif,
            image::ImageFormat::WebP => ExchangeFormat::Webp,
            _ => bail!("format not yet supported"),
        })
    }
}

/// Information that is needed to understand a pixmap buffer.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct PixelFormat {
    /// The channel encoding.
    pub encoding: PixelEncoding,
    /// The pixel width.
    pub width: u32,
    /// The pixel height.
    pub height: u32,
}

/// Determines the channel encoding of raw pixel data.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Cast)]
pub enum PixelEncoding {
    /// Three 8-bit channels: Red, green, blue.
    Rgb8,
    /// Four 8-bit channels: Red, green, blue, alpha.
    Rgba8,
    /// One 8-bit channel.
    Luma8,
    /// Two 8-bit channels: Luma and alpha.
    Lumaa8,
}

cast! {
    PixelFormat,
    self => Value::Dict(self.into()),
    mut dict: Dict => {
        let format = Self {
            encoding: dict.take("encoding")?.cast()?,
            width: dict.take("width")?.cast()?,
            height: dict.take("height")?.cast()?,
        };
        dict.finish(&["encoding", "width", "height"])?;
        format
    }
}

impl From<PixelFormat> for Dict {
    fn from(format: PixelFormat) -> Self {
        dict! {
            "encoding" => format.encoding,
            "width" => format.width,
            "height" => format.height,
        }
    }
}

/// Try to get the rotation from the EXIF metadata.
fn exif_rotation(exif: &exif::Exif) -> Option<u32> {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)
}

/// Apply an EXIF rotation to a dynamic image.
fn apply_rotation(image: &mut DynamicImage, rotation: u32) {
    use image::imageops as ops;
    match rotation {
        2 => ops::flip_horizontal_in_place(image),
        3 => ops::rotate180_in_place(image),
        4 => ops::flip_vertical_in_place(image),
        5 => {
            ops::flip_horizontal_in_place(image);
            *image = image.rotate270();
        }
        6 => *image = image.rotate90(),
        7 => {
            ops::flip_horizontal_in_place(image);
            *image = image.rotate90();
        }
        8 => *image = image.rotate270(),
        _ => {}
    }
}

/// Try to determine the DPI (dots per inch) of the image.
///
/// This is guaranteed to be a positive value, or `None` if invalid or
/// unspecified.
fn determine_dpi(data: &[u8], exif: Option<&exif::Exif>) -> Option<f64> {
    // Try to extract the DPI from the EXIF metadata. If that doesn't yield
    // anything, fall back to specialized procedures for extracting JPEG or PNG
    // DPI metadata. GIF does not have any.
    exif.and_then(exif_dpi)
        .or_else(|| jpeg_dpi(data))
        .or_else(|| png_dpi(data))
        .filter(|&dpi| dpi > 0.0)
}

/// Try to get the DPI from the EXIF metadata.
fn exif_dpi(exif: &exif::Exif) -> Option<f64> {
    let axis = |tag| {
        let dpi = exif.get_field(tag, exif::In::PRIMARY)?;
        let exif::Value::Rational(rational) = &dpi.value else { return None };
        Some(rational.first()?.to_f64())
    };

    [axis(exif::Tag::XResolution), axis(exif::Tag::YResolution)]
        .into_iter()
        .flatten()
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
}

/// Tries to extract the DPI from raw JPEG data (by inspecting the JFIF APP0
/// section).
fn jpeg_dpi(data: &[u8]) -> Option<f64> {
    let validate_at = |index: usize, expect: &[u8]| -> Option<()> {
        data.get(index..)?.starts_with(expect).then_some(())
    };
    let u16_at = |index: usize| -> Option<u16> {
        data.get(index..index + 2)?.try_into().ok().map(u16::from_be_bytes)
    };

    validate_at(0, b"\xFF\xD8\xFF\xE0\0")?;
    validate_at(6, b"JFIF\0")?;
    validate_at(11, b"\x01")?;

    let len = u16_at(4)?;
    if len < 16 {
        return None;
    }

    let units = *data.get(13)?;
    let x = u16_at(14)?;
    let y = u16_at(16)?;
    let dpu = x.max(y) as f64;

    Some(match units {
        1 => dpu,        // already inches
        2 => dpu * 2.54, // cm -> inches
        _ => return None,
    })
}

/// Tries to extract the DPI from raw PNG data.
fn png_dpi(mut data: &[u8]) -> Option<f64> {
    let mut decoder = png::StreamingDecoder::new();
    loop {
        let (consumed, event) = decoder.update(data, None).ok()?;
        match event {
            // Bail as soon as there is anything data-like.
            png::Decoded::ChunkBegin(_, png::chunk::IDAT)
            | png::Decoded::ImageData
            | png::Decoded::ImageDataFlushed => break,
            _ => {}
        }
        data = data.get(consumed..)?;
        if consumed == 0 {
            break;
        }
    }

    let dims = decoder.info().and_then(|i| i.pixel_dims)?;
    let dpu = dims.xppu.max(dims.yppu) as f64;
    match dims.unit {
        png::Unit::Meter => Some(dpu * 0.0254), // meter -> inches
        png::Unit::Unspecified => None,
    }
}

/// Format the user-facing raster graphic decoding error message.
fn format_image_error(error: image::ImageError) -> EcoString {
    match error {
        image::ImageError::Limits(_) => "file is too large".into(),
        err => eco_format!("failed to decode image ({err})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_dpi() {
        #[track_caller]
        fn test(path: &str, format: ExchangeFormat, dpi: f64) {
            let data = typst_dev_assets::get(path).unwrap();
            let bytes = Bytes::new(data);
            let image = RasterImage::plain(bytes, format).unwrap();
            assert_eq!(image.dpi().map(f64::round), Some(dpi));
        }

        test("images/f2t.jpg", ExchangeFormat::Jpg, 220.0);
        test("images/tiger.jpg", ExchangeFormat::Jpg, 72.0);
        test("images/graph.png", ExchangeFormat::Png, 144.0);
    }
}

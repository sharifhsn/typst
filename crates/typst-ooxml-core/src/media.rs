//! Media registry and embeddable-image helpers shared by Office exporters.

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use typst_library::visualize::{ExchangeFormat, Image, ImageKind, RasterFormat};
use typst_utils::hash128;

/// Index into a package media registry.
pub type MediaId = usize;

/// One media part in the package.
pub struct MediaPart {
    pub part_name: EcoString,
    pub ext: EcoString,
    pub bytes: Vec<u8>,
}

/// Hash-deduplicating package media registry.
pub struct MediaRegistry {
    part_prefix: &'static str,
    parts: Vec<MediaPart>,
    dedup: FxHashMap<u128, MediaId>,
}

impl MediaRegistry {
    pub fn new(part_prefix: &'static str) -> Self {
        Self {
            part_prefix,
            parts: Vec::new(),
            dedup: FxHashMap::default(),
        }
    }

    pub fn add(&mut self, bytes: &[u8], ext: &str) -> MediaId {
        let hash = hash128(bytes);
        if let Some(&id) = self.dedup.get(&hash) {
            return id;
        }

        let clean_ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let id = self.parts.len();
        self.parts.push(MediaPart {
            part_name: eco_format!("{}/image{}.{}", self.part_prefix, id + 1, clean_ext),
            ext: clean_ext.into(),
            bytes: bytes.to_vec(),
        });
        self.dedup.insert(hash, id);
        id
    }

    pub fn part(&self, id: MediaId) -> &MediaPart {
        &self.parts[id]
    }

    pub fn get(&self, id: MediaId) -> Option<&MediaPart> {
        self.parts.get(id)
    }

    pub fn parts(&self) -> &[MediaPart] {
        &self.parts
    }

    pub fn into_parts(self) -> Vec<MediaPart> {
        self.parts
    }
}

/// Embeddable image bytes plus canonical extension.
pub struct EmbeddableImage<'a> {
    pub bytes: &'a [u8],
    pub ext: &'static str,
}

/// Returns exchange-format bytes that Office can embed without rerendering.
pub fn embeddable_image_bytes(image: &Image) -> Option<EmbeddableImage<'_>> {
    match image.kind() {
        ImageKind::Raster(raster) => match raster.format() {
            RasterFormat::Exchange(ExchangeFormat::Png) => {
                Some(EmbeddableImage { bytes: raster.data().as_slice(), ext: "png" })
            }
            RasterFormat::Exchange(ExchangeFormat::Jpg) => {
                Some(EmbeddableImage { bytes: raster.data().as_slice(), ext: "jpeg" })
            }
            RasterFormat::Exchange(ExchangeFormat::Gif) => {
                Some(EmbeddableImage { bytes: raster.data().as_slice(), ext: "gif" })
            }
            RasterFormat::Exchange(ExchangeFormat::Webp) | RasterFormat::Pixel(_) => None,
        },
        ImageKind::Svg(_) | ImageKind::Pdf(_) => None,
    }
}

/// Picks the content type for a media extension.
pub fn image_content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

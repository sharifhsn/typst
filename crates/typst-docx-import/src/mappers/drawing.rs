//! The `drawing` mapper: a Word inline/anchored image (`w:drawing`) → the
//! Typst IR's [`Figure`].

use ecow::{eco_format, EcoString};
use typst_ooxml_core::units::emu_to_abs;

use crate::report::ImportReport;
use crate::tdoc::Figure;
use crate::wml::model::{DrawingRef, WmlPackage};

/// Resolve a drawing's relationship + media part into a [`Figure`]. Returns
/// `None` (recording a [`ImportReport::drop`]) if the relationship or the
/// media part it points to can't be found.
pub fn lower_drawing(
    d: &DrawingRef,
    package: &WmlPackage,
    report: &mut ImportReport,
) -> Option<Figure> {
    let Some(rel) = package.rels.get(&d.rel_id) else {
        report.drop("image", "drawing relationship not found");
        return None;
    };

    // Relationship targets for media parts are relative to `word/` (e.g.
    // `media/image1.png`); `package.media` keys are full zip names.
    let media_key: EcoString = if rel.target.starts_with("word/") {
        rel.target.clone()
    } else {
        format!("word/{}", rel.target.trim_start_matches("./")).into()
    };

    if !package.media.contains_key(&media_key) {
        report.drop("image", "media part not found");
        return None;
    }

    // Word embeds Windows metafiles (`.wmf`/`.emf`) as readily as bitmaps, and
    // Typst cannot decode them. Emitting an `#image` for one doesn't degrade
    // the picture — it fails the whole document with "unknown image format",
    // so a single unsupported drawing would cost every other page. Drop it and
    // say so instead.
    if !is_supported_image(&media_key) {
        let ext = extension(&media_key).unwrap_or_default();
        report.drop("image", eco_format!("{ext} images are not supported by Typst"));
        return None;
    }

    Some(Figure {
        image_path: media_key,
        width_pt: d.cx_emu.map(|cx| emu_to_abs(cx as f64).to_pt()),
        height_pt: d.cy_emu.map(|cy| emu_to_abs(cy as f64).to_pt()),
        alt: d.alt.clone(),
        caption: None,
    })
}

/// The raster/vector formats Typst's `image` function can decode. Anything
/// else — metafiles, TIFF, BMP — has to be dropped rather than referenced.
const SUPPORTED_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "svgz", "webp"];

fn is_supported_image(part: &str) -> bool {
    extension(part).is_some_and(|ext| SUPPORTED_IMAGE_EXTS.contains(&ext.as_str()))
}

fn extension(part: &str) -> Option<EcoString> {
    let name = part.rsplit(['/', '\\']).next()?;
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase().into())
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::wml::model::Relationship;

    fn package_with(media_name: &str) -> WmlPackage {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: format!("media/{media_name}").into(), external: false },
        );
        let mut media = FxHashMap::default();
        media.insert(format!("word/media/{media_name}").into(), vec![0u8; 4]);
        WmlPackage { rels, media, ..Default::default() }
    }

    fn drawing() -> DrawingRef {
        DrawingRef { rel_id: "rId1".into(), cx_emu: None, cy_emu: None, alt: None }
    }

    #[test]
    fn supported_formats_lower_to_a_figure() {
        for name in ["image1.png", "image1.JPEG", "image1.svg", "image1.webp"] {
            let mut report = ImportReport::default();
            assert!(
                lower_drawing(&drawing(), &package_with(name), &mut report).is_some(),
                "{name} should lower to a figure"
            );
        }
    }

    /// A Windows metafile must be dropped with a note, not referenced: Typst
    /// cannot decode one, and an `#image` pointing at it fails the *entire*
    /// document rather than just that picture.
    #[test]
    fn metafiles_are_dropped_rather_than_referenced() {
        for name in ["image1.wmf", "image3.emf", "scan.tiff"] {
            let mut report = ImportReport::default();
            assert!(
                lower_drawing(&drawing(), &package_with(name), &mut report).is_none(),
                "{name} should be dropped"
            );
            assert!(!report.notes.is_empty(), "dropping {name} must be recorded");
        }
    }
}

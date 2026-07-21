//! The `drawing` mapper: a Word inline/anchored image (`w:drawing`) → the
//! Typst IR's [`Figure`].

use ecow::{eco_format, EcoString};
use typst_ooxml_core::units::emu_to_abs;

use crate::report::ImportReport;
use crate::tdoc::{Align, Figure};
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

    let Some(bytes) = package.media.get(&media_key) else {
        report.drop("image", "media part not found");
        return None;
    };

    // A part's file *extension* is not to be trusted — real producers get it
    // wrong (`lo-sw-floattable-del-empty.docx`'s `word/media/image1.jpeg`
    // actually begins `\x89PNG`), and Typst picks its decoder from the
    // extension, not the content. So the real format is sniffed from the
    // leading bytes first; the extension is only a fallback for when
    // sniffing can't tell (see `sniff_image_format`'s doc comment for what
    // it does and doesn't recognise).
    if is_metafile(bytes) {
        // Checked ahead of the generic "supported?" gate below because a
        // metafile can just as easily carry a *lying* extension that claims
        // a format Typst does support — without this, it would sail through
        // on its extension alone and fail the whole document exactly like
        // the bug this fix closes, just from the opposite direction.
        report.drop("image", "Windows metafiles (WMF/EMF) are not supported by Typst");
        return None;
    }
    let sniffed = sniff_image_format(bytes);
    let is_supported = sniffed.is_some()
        || extension(&media_key).is_some_and(|ext| SUPPORTED_IMAGE_EXTS.contains(&ext.as_str()));
    if !is_supported {
        let ext = extension(&media_key).unwrap_or_default();
        report.drop("image", eco_format!("{ext} images are not supported by Typst"));
        return None;
    }

    // The emitted asset takes the *sniffed* extension when sniffing was
    // conclusive, since that's what Typst will actually try to decode it as
    // — keeping the part's own (possibly lying) extension here would just
    // move the "failed to decode image" failure from import time to the
    // point the emitted `.typ` is compiled. `emit::Emitter::resolve_asset`
    // matches this back to the real zip member by filename *stem*, since the
    // two extensions can now legitimately differ.
    let image_path = match sniffed {
        Some(real) => with_extension(&media_key, real),
        None => media_key.clone(),
    };

    Some(Figure {
        image_path,
        width_pt: d.cx_emu.map(|cx| emu_to_abs(cx as f64).to_pt()),
        height_pt: d.cy_emu.map(|cy| emu_to_abs(cy as f64).to_pt()),
        alt: d.alt.clone(),
        // Word's named float placement is the one part of a floating
        // drawing's geometry Typst's flow can honour; the absolute offset and
        // the text wrap around it can't be, and stay reported as lost.
        align: d.align.as_deref().and_then(|align| match align {
            "left" => Some(Align::Left),
            "center" => Some(Align::Center),
            "right" => Some(Align::Right),
            _ => None,
        }),
        caption: None,
    })
}

/// The raster/vector formats Typst's `image` function can decode. Anything
/// else — metafiles, TIFF, BMP — has to be dropped rather than referenced.
/// Used both as [`sniff_image_format`]'s vocabulary and, per its own doc
/// comment, as the *fallback* check when sniffing the leading bytes is
/// inconclusive.
const SUPPORTED_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "svgz", "webp"];

fn extension(part: &str) -> Option<EcoString> {
    let name = part.rsplit(['/', '\\']).next()?;
    let (_, ext) = name.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase().into())
}

/// Replaces `part`'s extension (the text after its last `.`) with `ext` —
/// used to rename the *emitted* asset to match a format [`sniff_image_format`]
/// found that disagrees with the part's own (lying) name.
fn with_extension(part: &str, ext: &str) -> EcoString {
    match part.rsplit_once('.') {
        Some((stem, _old_ext)) => eco_format!("{stem}.{ext}"),
        None => eco_format!("{part}.{ext}"),
    }
}

/// Sniffs the real image format from its leading bytes, by magic-number
/// signature (PNG, JPEG, GIF, WebP) or, for SVG — plain XML text with no
/// fixed binary signature — by looking for its root element. Returns the
/// extension Typst's `image` function should treat the part as, or `None`
/// when the bytes don't match anything recognised here (most commonly a
/// TIFF, BMP, or another format this mapper has no reason to special-case);
/// [`lower_drawing`] falls back to the part's own extension in that case.
fn sniff_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("jpg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if looks_like_svg(bytes) {
        return Some("svg");
    }
    None
}

/// Whether the leading bytes look like an SVG document: XML text whose root
/// element is `<svg`, optionally preceded by a UTF-8 BOM and/or an `<?xml
/// …?>` declaration — SVG has no fixed binary signature (it's just XML), so
/// this inspects a bounded text prefix rather than requiring the whole part
/// to parse, which this mapper has no other reason to do.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let Ok(text) = std::str::from_utf8(head) else { return false };
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && text.contains("<svg"))
}

/// Windows metafile signatures (`.wmf`/`.emf`), checked explicitly rather
/// than left to fall through [`sniff_image_format`]'s `None` — a metafile
/// with a *lying* extension that claims a format Typst supports must still
/// be dropped (see [`lower_drawing`]'s call site), which relying on the
/// extension-based fallback alone would get wrong.
fn is_metafile(bytes: &[u8]) -> bool {
    // Placeable WMF (the common on-disk form) and the two "standard" WMF
    // header variants (memory- vs disk-based, `mtType` 1 vs 2).
    bytes.starts_with(b"\xd7\xcd\xc6\x9a")
        || bytes.starts_with(b"\x01\x00\x09\x00")
        || bytes.starts_with(b"\x02\x00\x09\x00")
        // EMF: `iType == EMR_HEADER (1)` as a little-endian u32, and the
        // literal signature `" EMF"` always sits at a fixed offset in the
        // (fixed-layout) header record.
        || (bytes.starts_with(b"\x01\x00\x00\x00") && bytes.get(40..44) == Some(b" EMF".as_slice()))
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
        DrawingRef { rel_id: "rId1".into(), cx_emu: None, cy_emu: None, alt: None, align: None }
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

    /// A package with `media_name` holding `bytes` as its content (rather
    /// than [`package_with`]'s zero-filled filler) — for exercising the
    /// signature-sniffing path, where the actual bytes matter.
    fn package_with_bytes(media_name: &str, bytes: Vec<u8>) -> WmlPackage {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: format!("media/{media_name}").into(), external: false },
        );
        let mut media = FxHashMap::default();
        media.insert(format!("word/media/{media_name}").into(), bytes);
        WmlPackage { rels, media, ..Default::default() }
    }

    /// `lo-sw-floattable-del-empty.docx`'s real bug: a part named
    /// `image1.jpeg` that actually begins `\x89PNG`. The lying extension
    /// must not decide the outcome — sniffing does, and the emitted asset is
    /// renamed to the extension that matches the real bytes.
    #[test]
    fn a_png_with_a_lying_jpeg_extension_is_sniffed_and_renamed() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 8]);
        let package = package_with_bytes("image1.jpeg", bytes);

        let mut report = ImportReport::default();
        let figure = lower_drawing(&drawing(), &package, &mut report)
            .expect("a PNG sniffed under a lying extension should still lower to a figure");
        assert_eq!(figure.image_path, "word/media/image1.png");
        assert!(report.notes.is_empty(), "no loss here — the image works fine: {:?}", report.notes);
    }

    /// The opposite direction: a metafile that lies the *other* way, naming
    /// itself as a format Typst does support. It must still be dropped —
    /// relying on the extension alone here would silently reintroduce the
    /// "failed to decode image" whole-document failure.
    #[test]
    fn a_metafile_with_a_lying_supported_extension_is_still_dropped() {
        let mut bytes = b"\xd7\xcd\xc6\x9a".to_vec();
        bytes.extend_from_slice(&[0u8; 8]);
        let package = package_with_bytes("image1.png", bytes);

        let mut report = ImportReport::default();
        assert!(lower_drawing(&drawing(), &package, &mut report).is_none());
        assert!(!report.notes.is_empty());
    }

    /// A genuine, honestly-named PNG keeps its own extension — sniffing
    /// agreeing with the file name must not spuriously rename anything.
    #[test]
    fn a_correctly_named_image_keeps_its_extension() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 8]);
        let package = package_with_bytes("image1.png", bytes);

        let mut report = ImportReport::default();
        let figure =
            lower_drawing(&drawing(), &package, &mut report).expect("expected a figure");
        assert_eq!(figure.image_path, "word/media/image1.png");
    }

    /// A gzip-compressed SVG (`.svgz`) has no text-based signature
    /// [`sniff_image_format`] can recognise — sniffing is inconclusive, so
    /// this must fall back to the part's own extension rather than being
    /// mistaken for an unsupported format.
    #[test]
    fn svgz_falls_back_to_its_extension_when_sniffing_is_inconclusive() {
        let package = package_with_bytes("image1.svgz", vec![0x1f, 0x8b, 0x08, 0, 0, 0]);
        let mut report = ImportReport::default();
        assert!(lower_drawing(&drawing(), &package, &mut report).is_some());
    }
}

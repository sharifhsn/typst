//! The `drawing` mapper: a Word inline/anchored image (`w:drawing`) → the
//! Typst IR's [`Figure`].

use ecow::EcoString;
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

    Some(Figure {
        image_path: media_key,
        width_pt: d.cx_emu.map(|cx| emu_to_abs(cx as f64).to_pt()),
        height_pt: d.cy_emu.map(|cy| emu_to_abs(cy as f64).to_pt()),
        alt: d.alt.clone(),
        caption: None,
    })
}

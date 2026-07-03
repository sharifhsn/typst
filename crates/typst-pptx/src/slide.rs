use typst_layout::{Page, PagedDocument};
use typst_library::visualize::{Paint, ProcessColorSpace};

use crate::dom::{SlideCtx, SlideIr};

/// Convert all pages into slide IR.
pub fn slides(document: &PagedDocument, ctx: &mut SlideCtx) -> Vec<SlideIr> {
    document.pages().iter().map(|page| slide(page, ctx)).collect()
}

fn slide(page: &Page, _ctx: &mut SlideCtx) -> SlideIr {
    let mut shapes = Vec::new();
    shapes.extend(crate::text::emit_text_boxes());
    shapes.extend(crate::shape::emit_shapes());
    shapes.extend(crate::image::emit_images());

    SlideIr { bg: solid_background(page), shapes }
}

fn solid_background(page: &Page) -> Option<[u8; 3]> {
    match page.fill_or_white() {
        Some(Paint::Solid(color)) => {
            let [r, g, b, _] =
                color.to_process_space(ProcessColorSpace::Srgb).to_vec4_u8();
            Some([r, g, b])
        }
        Some(Paint::Gradient(_) | Paint::Tiling(_)) => Some([255, 255, 255]),
        None => None,
    }
}

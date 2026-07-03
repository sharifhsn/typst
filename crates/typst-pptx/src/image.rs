use crate::dom::SlideShape;

/// SPEC contract: embed raster images verbatim when possible, rasterize SVG/PDF
/// and unsupported raster formats to PNG, crop blank ink, and deduplicate media
/// through the shared slide context.
///
/// Foundation scope intentionally emits no pictures yet.
pub(crate) fn emit_images() -> Vec<SlideShape> {
    Vec::new()
}

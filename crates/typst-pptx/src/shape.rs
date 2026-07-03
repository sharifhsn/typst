use crate::dom::SlideShape;

/// SPEC contract: lower Typst geometry, solid/linear-gradient fills, fixed
/// strokes, line caps, and dash patterns to native PPTX DrawingML geometry,
/// rasterizing only unsupported paint or transform cases.
///
/// Foundation scope intentionally emits no vector shapes yet.
pub(crate) fn emit_shapes() -> Vec<SlideShape> {
    Vec::new()
}

use crate::dom::SlideShape;

/// SPEC contract: cluster `FrameItem::Text` entries by rotation and baseline,
/// apply the measured baseline rule, merge compatible runs, attach text links,
/// and emit native `SlideShape::TextBox` values in deterministic painter order.
///
/// Foundation scope intentionally emits no text boxes yet.
pub(crate) fn emit_text_boxes() -> Vec<SlideShape> {
    Vec::new()
}

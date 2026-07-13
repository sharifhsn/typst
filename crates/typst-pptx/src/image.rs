use std::panic::{AssertUnwindSafe, catch_unwind};

use typst_export_common::raster as render;
use typst_export_common::raster::RasterOptions;
use typst_library::foundations::{Content, Smart};
use typst_library::layout::{Abs, Frame, FrameItem, Point, Sides, Size};
use typst_library::visualize::{Image, ImageKind};
use typst_ooxml_core::media;

use crate::dom::{MediaId, SlideCtx};

pub(crate) struct EmbeddedImage {
    pub media: MediaId,
    pub svg_media: Option<MediaId>,
    pub offset: Point,
    pub size: Size,
}

/// Embed a laid-out image and return the media id, crop offset, and display size.
///
/// PNG/JPEG/GIF exchange rasters are embedded verbatim. WebP, raw-pixel rasters,
/// and PDF are rendered at the supplied display size and embedded as PNG. SVG
/// embeds both a native SVG media part and a rendered PNG fallback.
#[allow(dead_code)]
pub(crate) fn embed_image(
    ctx: &mut SlideCtx,
    image: &Image,
    size: Size,
) -> Option<EmbeddedImage> {
    if let Some(embeddable) = media::embeddable_image_bytes(image) {
        return Some(EmbeddedImage {
            media: ctx.add_media(embeddable.bytes, embeddable.ext),
            svg_media: None,
            offset: Point::zero(),
            size,
        });
    }

    if let ImageKind::Svg(svg) = image.kind() {
        let svg_media = ctx.add_media(svg.data().as_slice(), "svg");
        let mut frame = Frame::soft(size);
        frame.push(
            Point::zero(),
            FrameItem::Image(image.clone(), size, typst_syntax::Span::detached()),
        );
        let (media, offset, size) = raster_fallback(ctx, frame)?;
        return Some(EmbeddedImage { media, svg_media: Some(svg_media), offset, size });
    }

    let mut frame = Frame::soft(size);
    frame.push(
        Point::zero(),
        FrameItem::Image(image.clone(), size, typst_syntax::Span::detached()),
    );
    let (media, offset, size) = raster_fallback(ctx, frame)?;
    Some(EmbeddedImage { media, svg_media: None, offset, size })
}

/// Embed an exchange-format raster image without re-rendering it.
pub(crate) fn embed_original_image(
    ctx: &mut SlideCtx,
    image: &Image,
    size: Size,
) -> Option<(MediaId, Point, Size)> {
    let embeddable = media::embeddable_image_bytes(image)?;
    Some((ctx.add_media(embeddable.bytes, embeddable.ext), Point::zero(), size))
}

/// Rasterize an already laid-out frame to a cropped PNG media part.
///
/// The returned offset is relative to the input frame's origin and includes both
/// the geometric ink rect offset and any subsequent transparent-pixel crop.
#[allow(dead_code)]
pub(crate) fn raster_fallback(
    ctx: &mut SlideCtx,
    frame: Frame,
) -> Option<(MediaId, Point, Size)> {
    let raster = render::render_frame_to_png(
        frame,
        RasterOptions { pixel_per_pt: 2.0, crop_to_ink: true },
    )?;
    let media = ctx.add_media(&raster.png, "png");
    Some((media, raster.offset, raster.size))
}

/// Rasterize a single item by wrapping it in a soft frame of the supplied size.
#[allow(dead_code)]
pub(crate) fn raster_fallback_item(
    ctx: &mut SlideCtx,
    item: FrameItem,
    size: Size,
) -> Option<(MediaId, Point, Size)> {
    let mut frame = Frame::soft(size);
    frame.push(Point::zero(), item);
    raster_fallback(ctx, frame)
}

/// Whether a group's clip provably clips nothing, so its children can be
/// walked natively instead of rasterizing the whole group (which would bake
/// its live text into a picture).
///
/// Two tiers, both exact:
/// 1. Geometric fast path — the clip is the plain axis-aligned rectangle at
///    the frame's own bounds (what `#box(clip: true)` produces) and every
///    drawn item's ink stays inside it.
/// 2. Render probe — for every other clip shape (rounded cards, tight rects),
///    render the frame with and without the clip at the same resolution the
///    raster fallback would use; byte-identical pixels prove the clip removes
///    no visible ink.
///
/// A false negative only means an unnecessary raster — never a wrongly
/// unclipped slide.
pub(crate) fn clip_is_noop(
    clip: &typst_library::visualize::Curve,
    frame: &Frame,
) -> bool {
    if *clip == typst_library::visualize::Curve::rect(frame.size()) {
        let mut ink = None;
        render::frame_ink_rect(frame, Point::zero(), &mut ink);
        let fits = match ink {
            None => return true,
            Some(rect) => {
                let eps = Abs::pt(0.05);
                rect.min.x >= -eps
                    && rect.min.y >= -eps
                    && rect.max.x <= frame.size().x + eps
                    && rect.max.y <= frame.size().y + eps
            }
        };
        if fits {
            return true;
        }
    }
    // The render probe exists to rescue live TEXT from rasterization; for a
    // text-free group (progress bars, clipped image cards) the raster loses
    // nothing editable, so don't pay the double render.
    frame_has_text(frame) && clip_is_noop_by_render(clip, frame)
}

/// Whether a frame tree draws any text (early exit).
fn frame_has_text(frame: &Frame) -> bool {
    frame.items().any(|(_, item)| match item {
        FrameItem::Text(_) => true,
        FrameItem::Group(group) => frame_has_text(&group.frame),
        _ => false,
    })
}

/// Renders `frame` with and without `clip` over the identical canvas and
/// compares pixels. Equality is judged at the raster fallback's own
/// resolution, so "no visible difference" means exactly "no difference in
/// what we would otherwise ship as a picture".
fn clip_is_noop_by_render(clip: &typst_library::visualize::Curve, frame: &Frame) -> bool {
    let mut ink = None;
    render::frame_ink_rect(frame, Point::zero(), &mut ink);
    let Some(ink) = ink else { return true };
    if !render::finite_rect(ink) {
        return false;
    }
    // Cap probe cost: past ~16 Mpx the double render is not worth avoiding
    // one embedded picture; keep the raster fallback.
    let px = (ink.size().x.to_pt() * 2.0) * (ink.size().y.to_pt() * 2.0);
    if !(0.0..=16_000_000.0).contains(&px) {
        return false;
    }
    let probe = |clip: Option<typst_library::visualize::Curve>| -> Option<Vec<u8>> {
        let mut group = typst_library::layout::GroupItem::new(frame.clone());
        group.clip = clip;
        let size =
            Size::new(ink.size().x.max(Abs::pt(0.5)), ink.size().y.max(Abs::pt(0.5)));
        let mut canvas = Frame::hard(size);
        canvas.push(Point::new(-ink.min.x, -ink.min.y), FrameItem::Group(group));
        let page = typst_layout::Page {
            frame: canvas,
            bleed: Sides::splat(Abs::zero()),
            fill: Smart::Custom(None),
            numbering: None,
            supplement: Content::empty(),
            number: 1,
        };
        let options = typst_render::RenderOptions {
            pixel_per_pt: 2.0.into(),
            ..Default::default()
        };
        let pixmap =
            catch_unwind(AssertUnwindSafe(|| typst_render::render(&page, &options)))
                .ok()?;
        Some(pixmap.data().to_vec())
    };
    match (probe(Some(clip.clone())), probe(None)) {
        (Some(clipped), Some(unclipped)) => clipped == unclipped,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use typst_library::layout::{Abs, Frame, FrameItem, Point, Size};
    use typst_library::visualize::{Color, Geometry};
    use typst_syntax::Span;

    use super::*;

    #[test]
    fn media_deduplicates_identical_bytes() {
        let mut ctx = SlideCtx::default();
        let first = ctx.add_media(b"same image bytes", "png");
        let second = ctx.add_media(b"same image bytes", "png");

        assert_eq!(first, second);
        assert_eq!(ctx.media.parts().len(), 1);
    }

    #[test]
    fn blank_frame_rasterizes_to_none() {
        let mut ctx = SlideCtx::default();
        let frame = Frame::soft(Size::new(Abs::pt(100.0), Abs::pt(100.0)));

        assert!(raster_fallback(&mut ctx, frame).is_none());
        assert!(ctx.media.parts().is_empty());
    }

    #[test]
    fn offset_shape_returns_adjusted_offset() {
        let mut ctx = SlideCtx::default();
        let mut frame = Frame::soft(Size::new(Abs::pt(100.0), Abs::pt(100.0)));
        let offset = Point::new(Abs::pt(30.0), Abs::pt(40.0));
        let shape =
            Geometry::Rect(Size::new(Abs::pt(20.0), Abs::pt(10.0))).filled(Color::BLACK);
        frame.push(offset, FrameItem::Shape(shape, Span::detached()));

        let Some((media, adjusted_offset, adjusted_size)) =
            raster_fallback(&mut ctx, frame)
        else {
            panic!("offset shape should rasterize");
        };

        assert_eq!(media, 0);
        assert_eq!(ctx.media.parts().len(), 1);
        assert!((adjusted_offset.x.to_pt() - 30.0).abs() < 0.001);
        assert!((adjusted_offset.y.to_pt() - 40.0).abs() < 0.001);
        assert!((adjusted_size.x.to_pt() - 20.0).abs() < 0.001);
        assert!((adjusted_size.y.to_pt() - 10.0).abs() < 0.001);
    }
}

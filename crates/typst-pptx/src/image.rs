use std::panic::{AssertUnwindSafe, catch_unwind};

use typst_library::foundations::{Content, Smart};
use typst_library::layout::{Abs, Frame, FrameItem, Point, Rect, Sides, Size};
use typst_library::visualize::{ExchangeFormat, Image, ImageKind, RasterFormat};

use crate::dom::{MediaId, SlideCtx};

/// Embed a laid-out image and return the media id, crop offset, and display size.
///
/// PNG/JPEG/GIF exchange rasters are embedded verbatim. WebP, raw-pixel rasters,
/// SVG, and PDF are rendered at the supplied display size and embedded as PNG.
#[allow(dead_code)]
pub(crate) fn embed_image(
    ctx: &mut SlideCtx,
    image: &Image,
    size: Size,
) -> Option<(MediaId, Point, Size)> {
    if let Some((bytes, ext)) = embeddable_bytes(image) {
        return Some((ctx.add_media(bytes, ext), Point::zero(), size));
    }

    let mut frame = Frame::soft(size);
    frame.push(
        Point::zero(),
        FrameItem::Image(image.clone(), size, typst_syntax::Span::detached()),
    );
    raster_fallback(ctx, frame)
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
    let mut ink = None;
    frame_ink_rect(&frame, Point::zero(), &mut ink);
    let ink = ink?;
    if !finite_rect(ink) {
        return None;
    }

    let size = Size::new(ink.size().x.max(Abs::pt(0.5)), ink.size().y.max(Abs::pt(0.5)));
    let mut canvas = Frame::hard(size);
    canvas.push_frame(Point::new(-ink.min.x, -ink.min.y), frame);

    let page = typst_layout::Page {
        frame: canvas,
        bleed: Sides::splat(Abs::zero()),
        fill: Smart::Custom(None),
        numbering: None,
        supplement: Content::empty(),
        number: 1,
    };
    let options =
        typst_render::RenderOptions { pixel_per_pt: 2.0.into(), ..Default::default() };
    let rendered =
        catch_unwind(AssertUnwindSafe(|| typst_render::render(&page, &options)));
    let Ok(pixmap) = rendered else {
        return None;
    };

    let (pixmap, crop_offset, size) = crop_to_ink(pixmap, size)?;
    let png = pixmap.encode_png().ok()?;
    let media = ctx.add_media(&png, "png");
    Some((media, ink.min + crop_offset, size))
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

fn embeddable_bytes(image: &Image) -> Option<(&[u8], &'static str)> {
    match image.kind() {
        ImageKind::Raster(raster) => match raster.format() {
            RasterFormat::Exchange(ExchangeFormat::Png) => {
                Some((raster.data().as_slice(), "png"))
            }
            RasterFormat::Exchange(ExchangeFormat::Jpg) => {
                Some((raster.data().as_slice(), "jpeg"))
            }
            RasterFormat::Exchange(ExchangeFormat::Gif) => {
                Some((raster.data().as_slice(), "gif"))
            }
            RasterFormat::Exchange(ExchangeFormat::Webp) | RasterFormat::Pixel(_) => None,
        },
        ImageKind::Svg(_) | ImageKind::Pdf(_) => None,
    }
}

fn finite_rect(rect: Rect) -> bool {
    rect.min.x.to_pt().is_finite()
        && rect.min.y.to_pt().is_finite()
        && rect.max.x.to_pt().is_finite()
        && rect.max.y.to_pt().is_finite()
}

/// The geometric bounding box of everything a frame draws, in frame space.
fn frame_ink_rect(frame: &Frame, offset: Point, out: &mut Option<Rect>) {
    fn push(out: &mut Option<Rect>, rect: Rect) {
        if !finite_rect(rect) {
            return;
        }

        *out = Some(match *out {
            None => rect,
            Some(existing) => Rect::new(
                Point::new(
                    existing.min.x.min(rect.min.x),
                    existing.min.y.min(rect.min.y),
                ),
                Point::new(
                    existing.max.x.max(rect.max.x),
                    existing.max.y.max(rect.max.y),
                ),
            ),
        });
    }

    for (pos, item) in frame.items() {
        let p = offset + *pos;
        match item {
            FrameItem::Text(text) => {
                // Metrics-based bounds are intentional. Glyph-outline bboxes can
                // be empty for spaces and for some CJK fonts, which would erase
                // text from the raster canvas.
                let metrics = text.font.metrics();
                let ascent = metrics.ascender.at(text.size);
                let descent = -metrics.descender.at(text.size);
                push(
                    out,
                    Rect::new(
                        p + Point::new(Abs::zero(), -ascent),
                        p + Point::new(text.width(), descent.max(Abs::zero())),
                    ),
                );
            }
            FrameItem::Shape(shape, _) => {
                let bounds = shape.bbox(true);
                push(out, Rect::new(p + bounds.min, p + bounds.max));
            }
            FrameItem::Image(_, size, _) => {
                push(out, Rect::from_pos_size(p, *size));
            }
            FrameItem::Group(group) => {
                let mut inner = None;
                frame_ink_rect(&group.frame, Point::zero(), &mut inner);
                if let Some(bounds) = inner {
                    let corners = [
                        bounds.min,
                        Point::new(bounds.max.x, bounds.min.y),
                        Point::new(bounds.min.x, bounds.max.y),
                        bounds.max,
                    ];
                    let mut min = Point::splat(Abs::inf());
                    let mut max = Point::splat(-Abs::inf());
                    for corner in corners {
                        let transformed = corner.transform(group.transform);
                        min.x = min.x.min(transformed.x);
                        min.y = min.y.min(transformed.y);
                        max.x = max.x.max(transformed.x);
                        max.y = max.y.max(transformed.y);
                    }
                    push(out, Rect::new(p + min, p + max));
                }
            }
            FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
        }
    }
}

fn crop_to_ink(
    pixmap: tiny_skia::Pixmap,
    size: Size,
) -> Option<(tiny_skia::Pixmap, Point, Size)> {
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }

    let data = pixmap.data();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0usize, 0usize);
    for y in 0..h {
        let row = &data[y * w * 4..(y + 1) * w * 4];
        let mut any = false;
        for (x, px) in row.chunks_exact(4).enumerate() {
            if px[3] != 0 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                any = true;
            }
        }
        if any {
            min_y = min_y.min(y);
            max_y = y;
        }
    }

    if min_x > max_x {
        return None;
    }

    const PAD: usize = 2;
    let x0 = min_x.saturating_sub(PAD);
    let y0 = min_y.saturating_sub(PAD);
    let x1 = (max_x + PAD + 1).min(w);
    let y1 = (max_y + PAD + 1).min(h);

    if (x1 - x0) * 100 >= w * 97 && (y1 - y0) * 100 >= h * 97 {
        return Some((pixmap, Point::zero(), size));
    }

    let rect = tiny_skia::IntRect::from_ltrb(x0 as i32, y0 as i32, x1 as i32, y1 as i32)?;
    let cropped = pixmap.clone_rect(rect)?;

    let offset =
        Point::new(size.x * (x0 as f64 / w as f64), size.y * (y0 as f64 / h as f64));
    let cropped_size = Size::new(
        size.x * ((x1 - x0) as f64 / w as f64),
        size.y * ((y1 - y0) as f64 / h as f64),
    );
    Some((cropped, offset, cropped_size))
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
        assert_eq!(ctx.media.len(), 1);
    }

    #[test]
    fn blank_frame_rasterizes_to_none() {
        let mut ctx = SlideCtx::default();
        let frame = Frame::soft(Size::new(Abs::pt(100.0), Abs::pt(100.0)));

        assert!(raster_fallback(&mut ctx, frame).is_none());
        assert!(ctx.media.is_empty());
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
        assert_eq!(ctx.media.len(), 1);
        assert!((adjusted_offset.x.to_pt() - 30.0).abs() < 0.001);
        assert!((adjusted_offset.y.to_pt() - 40.0).abs() < 0.001);
        assert!((adjusted_size.x.to_pt() - 20.0).abs() < 0.001);
        assert!((adjusted_size.y.to_pt() - 10.0).abs() < 0.001);
    }
}

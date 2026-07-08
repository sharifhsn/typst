//! Raster fallback primitives shared by Office exporters.

use std::panic::{AssertUnwindSafe, catch_unwind};

use typst_library::foundations::{Content, Smart};
use typst_library::layout::{Abs, Frame, FrameItem, Point, Rect, Sides, Size};

/// Options for rendering a laid-out frame to PNG.
pub struct RasterOptions {
    pub pixel_per_pt: f64,
    pub crop_to_ink: bool,
}

/// A PNG raster fallback plus its logical offset and size.
pub struct RasterizedPng {
    pub png: Vec<u8>,
    pub offset: Point,
    pub size: Size,
}

/// A transparent-pixel-cropped pixmap plus its logical offset and size.
pub struct CroppedPixmap {
    pub pixmap: tiny_skia::Pixmap,
    pub offset: Point,
    pub size: Size,
}

/// Whether all rectangle coordinates are finite.
pub fn finite_rect(rect: Rect) -> bool {
    rect.min.x.to_pt().is_finite()
        && rect.min.y.to_pt().is_finite()
        && rect.max.x.to_pt().is_finite()
        && rect.max.y.to_pt().is_finite()
}

/// The geometric bounding box of everything a frame draws, in frame space.
pub fn frame_ink_rect(frame: &Frame, offset: Point, out: &mut Option<Rect>) {
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

/// Crops a pixmap to non-transparent pixels, preserving logical size.
pub fn crop_to_ink(pixmap: tiny_skia::Pixmap, size: Size) -> Option<CroppedPixmap> {
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
        return Some(CroppedPixmap { pixmap, offset: Point::zero(), size });
    }

    let rect = tiny_skia::IntRect::from_ltrb(x0 as i32, y0 as i32, x1 as i32, y1 as i32)?;
    let cropped = pixmap.clone_rect(rect)?;

    let offset =
        Point::new(size.x * (x0 as f64 / w as f64), size.y * (y0 as f64 / h as f64));
    let cropped_size = Size::new(
        size.x * ((x1 - x0) as f64 / w as f64),
        size.y * ((y1 - y0) as f64 / h as f64),
    );
    Some(CroppedPixmap { pixmap: cropped, offset, size: cropped_size })
}

/// Renders a laid-out frame to PNG, optionally cropping transparent pixels.
pub fn render_frame_to_png(
    frame: Frame,
    options: RasterOptions,
) -> Option<RasterizedPng> {
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
    let render_options = typst_render::RenderOptions {
        pixel_per_pt: options.pixel_per_pt.into(),
        ..Default::default()
    };
    let rendered =
        catch_unwind(AssertUnwindSafe(|| typst_render::render(&page, &render_options)));
    let Ok(pixmap) = rendered else {
        return None;
    };

    let (pixmap, offset, size) = if options.crop_to_ink {
        let cropped = crop_to_ink(pixmap, size)?;
        (cropped.pixmap, cropped.offset, cropped.size)
    } else {
        (pixmap, Point::zero(), size)
    };
    let png = pixmap.encode_png().ok()?;
    Some(RasterizedPng { png, offset: ink.min + offset, size })
}

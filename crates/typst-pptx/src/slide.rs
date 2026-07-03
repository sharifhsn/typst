use ecow::EcoString;
use typst_layout::{Page, PagedDocument};
use typst_library::layout::{Abs, Frame, FrameItem, Point, Size, Transform};
use typst_library::model::Destination;
use typst_library::visualize::{ColorSpace, Paint, ProcessColorSpace};

use crate::dom::{SlideCtx, SlideIr, SlideShape};
use crate::text::{LinkTarget, TextSource};

/// Convert all pages into slide IR.
pub fn slides(document: &PagedDocument, ctx: &mut SlideCtx) -> Vec<SlideIr> {
    document
        .pages()
        .iter()
        .map(|page| slide(document, page, ctx))
        .collect()
}

fn slide(document: &PagedDocument, page: &Page, ctx: &mut SlideCtx) -> SlideIr {
    let mut walker = Walker::new(document, ctx);
    walker.walk_frame(&page.frame, Transform::identity());
    attach_links(&mut walker.text, &walker.links);

    let mut ordered = walker.shapes;
    ordered.extend(
        crate::text::cluster_text(walker.text)
            .into_iter()
            .map(|cluster| OrderedShape { order: cluster.order, shape: cluster.shape }),
    );
    ordered.sort_by_key(|entry| entry.order);

    SlideIr {
        bg: solid_background(page),
        shapes: ordered.into_iter().map(|entry| entry.shape).collect(),
    }
}

struct Walker<'a, 'b> {
    document: &'a PagedDocument,
    ctx: &'b mut SlideCtx,
    next_order: usize,
    shapes: Vec<OrderedShape>,
    text: Vec<TextSource<'a>>,
    links: Vec<LinkRect>,
}

struct OrderedShape {
    order: usize,
    shape: SlideShape,
}

struct LinkRect {
    rect: Rect,
    target: LinkTarget,
}

#[derive(Copy, Clone)]
struct Rect {
    min: Point,
    max: Point,
}

#[derive(Copy, Clone)]
struct Similarity {
    rot_60k: i32,
    scale: f64,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(document: &'a PagedDocument, ctx: &'b mut SlideCtx) -> Self {
        Self {
            document,
            ctx,
            next_order: 0,
            shapes: Vec::new(),
            text: Vec::new(),
            links: Vec::new(),
        }
    }

    fn walk_frame(&mut self, frame: &'a Frame, transform: Transform) {
        for (pos, item) in frame.items() {
            let order = self.reserve_order();
            let item_transform = transform.pre_concat(Transform::translate(pos.x, pos.y));
            match item {
                FrameItem::Group(group) => {
                    let group_transform = item_transform.pre_concat(group.transform);
                    if group.clip.is_none()
                        && classify_similarity(group_transform).is_some()
                    {
                        self.walk_frame(&group.frame, group_transform);
                    } else {
                        self.emit_image_stub(order);
                    }
                }
                FrameItem::Text(text) => {
                    if let Some(similarity) = classify_similarity(item_transform) {
                        let baseline = Point::zero().transform(item_transform);
                        self.text.push(TextSource {
                            order,
                            baseline,
                            item: text,
                            rot_60k: similarity.rot_60k,
                            scale: similarity.scale,
                            link: None,
                        });
                    } else {
                        self.emit_image_stub(order);
                    }
                }
                FrameItem::Shape(_, _) => {
                    self.emit_shape_stub(order);
                }
                FrameItem::Image(_, _, _) => {
                    self.emit_image_stub(order);
                }
                FrameItem::Link(dest, size) => {
                    if let Some(target) = self.destination(dest) {
                        self.links.push(LinkRect {
                            rect: transformed_rect(item_transform, *size),
                            target,
                        });
                    }
                }
                FrameItem::Tag(_) => {}
            }
        }
    }

    fn reserve_order(&mut self) -> usize {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    fn emit_shape_stub(&mut self, order: usize) {
        self.shapes.extend(
            crate::shape::emit_shapes()
                .into_iter()
                .map(|shape| OrderedShape { order, shape }),
        );
    }

    fn emit_image_stub(&mut self, order: usize) {
        let _ = &mut self.ctx;
        self.shapes.extend(
            crate::image::emit_images()
                .into_iter()
                .map(|shape| OrderedShape { order, shape }),
        );
    }

    fn destination(&self, dest: &Destination) -> Option<LinkTarget> {
        match dest {
            Destination::Url(url) => Some(LinkTarget::Url(EcoString::from(url.as_str()))),
            Destination::Position(pos) => {
                Some(LinkTarget::Slide(pos.page.get().saturating_sub(1)))
            }
            Destination::Location(loc) => self
                .document
                .introspector()
                .position(*loc)
                .map(|pos| LinkTarget::Slide(pos.page.get().saturating_sub(1))),
        }
    }
}

fn attach_links(text: &mut [TextSource<'_>], links: &[LinkRect]) {
    for source in text {
        let rect = text_rect(source.item, source.baseline, source.scale);
        source.link = links
            .iter()
            .find(|link| rect.overlaps(link.rect))
            .map(|link| link.target.clone());
    }
}

fn classify_similarity(transform: Transform) -> Option<Similarity> {
    let sx = transform.sx.get();
    let ky = transform.ky.get();
    let kx = transform.kx.get();
    let sy = transform.sy.get();

    let len_x = sx.hypot(ky);
    let len_y = kx.hypot(sy);
    let dot = sx * kx + ky * sy;
    let det = sx * sy - kx * ky;
    let eps = 1e-9_f64;

    if (len_x - 1.0).abs() <= eps
        && (len_y - 1.0).abs() <= eps
        && dot.abs() <= eps
        && det > 0.0
        && kx.abs() <= eps
        && ky.abs() <= eps
    {
        return Some(Similarity { rot_60k: 0, scale: 1.0 });
    }

    if len_x <= eps
        || len_y <= eps
        || (len_x - len_y).abs() > 1e-6
        || dot.abs() > 1e-6
        || det <= 0.0
    {
        return None;
    }

    let theta = ky.atan2(sx).to_degrees();
    Some(Similarity {
        rot_60k: (theta * 60000.0).round() as i32,
        scale: len_x,
    })
}

fn text_rect(text: &typst_library::text::TextItem, baseline: Point, scale: f64) -> Rect {
    let size = text.size * scale.abs();
    let descent = (-text.font.metrics().descender).at(size);
    Rect {
        min: Point::new(baseline.x, baseline.y - size),
        max: Point::new(baseline.x + text.width() * scale, baseline.y + descent),
    }
}

fn transformed_rect(transform: Transform, size: Size) -> Rect {
    let points =
        [Point::zero(), Point::with_x(size.x), Point::with_y(size.y), size.to_point()];
    let mut min = Point::splat(Abs::inf());
    let mut max = Point::splat(-Abs::inf());
    for point in points {
        let transformed = point.transform(transform);
        min = min.min(transformed);
        max = max.max(transformed);
    }
    Rect { min, max }
}

impl Rect {
    fn overlaps(self, other: Rect) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }
}

fn solid_background(page: &Page) -> Option<[u8; 3]> {
    match page.fill_or_white() {
        Some(Paint::Solid(color)) => {
            let srgb =
                color.to_space(&ColorSpace::Process(ProcessColorSpace::Srgb)).ok()?;
            let [r, g, b, _] = srgb.to_vec4_u8();
            Some([r, g, b])
        }
        Some(Paint::Gradient(_) | Paint::Tiling(_)) => Some([255, 255, 255]),
        None => None,
    }
}

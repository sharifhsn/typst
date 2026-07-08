use ecow::EcoString;
use rustc_hash::FxHashMap;
use typst_layout::{Page, PagedDocument};
use typst_library::foundations::{NativeElement, StyleChain};
use typst_library::introspection::{Introspector, Location, Tag};
use typst_library::layout::{Abs, Frame, FrameItem, Point, Size, Transform};
use typst_library::math::EquationElem;
use typst_library::model::Destination;
use typst_library::visualize::{Paint, Shape};

use crate::dom::{FillSpec, MathBox, PicGeom, SlideCtx, SlideIr, SlideShape};
use crate::text::{InlineMathSource, LinkTarget, TextSource};

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
        crate::text::cluster_text(walker.text, walker.inline_math)
            .into_iter()
            .map(|cluster| OrderedShape { order: cluster.order, shape: cluster.shape }),
    );
    ordered.sort_by_key(|entry| entry.order);

    SlideIr {
        bg: background(page),
        shapes: ordered.into_iter().map(|entry| entry.shape).collect(),
    }
}

struct Walker<'a, 'b> {
    document: &'a PagedDocument,
    ctx: &'b mut SlideCtx,
    next_order: usize,
    shapes: Vec<OrderedShape>,
    text: Vec<TextSource<'a>>,
    inline_math: Vec<InlineMathSource>,
    links: Vec<LinkRect>,
    equations: FxHashMap<Location, MathSource>,
    active_math: Vec<ActiveMath>,
}

struct OrderedShape {
    order: usize,
    shape: SlideShape,
}

struct LinkRect {
    rect: Rect,
    target: LinkTarget,
}

struct MathSource {
    omml: String,
    fallback: EcoString,
    block: bool,
}

struct ActiveMath {
    loc: Location,
    order: usize,
    bounds: Option<Rect>,
    baseline: Option<Point>,
    rot_60k: i32,
    fallback: EcoString,
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
            inline_math: Vec::new(),
            links: Vec::new(),
            equations: equation_sources(document),
            active_math: Vec::new(),
        }
    }

    fn walk_frame(&mut self, frame: &'a Frame, transform: Transform) {
        for (pos, item) in frame.items() {
            let order = self.reserve_order();
            let item_transform = transform.pre_concat(Transform::translate(pos.x, pos.y));
            if !matches!(item, FrameItem::Tag(_))
                && self.capture_math_item(item, item_transform)
            {
                continue;
            }
            match item {
                FrameItem::Group(group) => {
                    let group_transform = item_transform.pre_concat(group.transform);
                    let clip_ok = match &group.clip {
                        None => true,
                        // A defensive clip that provably clips nothing (the
                        // common `#box(clip: true)` card) must not swallow
                        // its live text into a picture.
                        Some(clip) => crate::image::clip_is_noop(clip, &group.frame),
                    };
                    if clip_ok && classify_similarity(group_transform).is_some() {
                        self.walk_frame(&group.frame, group_transform);
                    } else if let Some(clip) = &group.clip
                        && let Some(geom) =
                            crate::shape::clip_to_pic_geom(clip, group.frame.size())
                        && self.try_emit_clipped_image(
                            order,
                            &group.frame,
                            group_transform,
                            geom,
                        )
                    {
                    } else {
                        debug_raster(
                            "group",
                            if group.clip.is_some() { "clip" } else { "transform" },
                            frame_text_chars(&group.frame),
                        );
                        // A clip or a non-similarity transform (skew,
                        // non-uniform scale) has no PPTX form: render the
                        // whole group through its own transform and place
                        // the picture where the ink lands. `item_transform`
                        // (not `group_transform`) — the group item carries
                        // its own transform inside.
                        self.raster_item(
                            order,
                            FrameItem::Group(group.clone()),
                            item_transform,
                            None,
                        );
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
                        debug_raster("text", "transform", text.text.chars().count());
                        self.raster_item(
                            order,
                            FrameItem::Text(text.clone()),
                            item_transform,
                            Some(text.text.clone()),
                        );
                    }
                }
                FrameItem::Shape(shape, span) => {
                    match crate::shape::shape_to_geom(shape, item_transform, 0) {
                        Some(geom) => self
                            .shapes
                            .push(OrderedShape { order, shape: SlideShape::Geom(geom) }),
                        None => {
                            debug_raster("shape", "unmappable", 0);
                            self.raster_item(
                                order,
                                FrameItem::Shape(shape.clone(), *span),
                                item_transform,
                                None,
                            )
                        }
                    }
                }
                FrameItem::Image(image, size, span) => {
                    self.emit_image(order, image, *size, *span, item_transform);
                }
                FrameItem::Link(dest, size) => {
                    if let Some(target) = self.destination(dest) {
                        self.links.push(LinkRect {
                            rect: transformed_rect(item_transform, *size),
                            target,
                        });
                    }
                }
                FrameItem::Tag(tag) => self.handle_tag(tag, order),
            }
        }
    }

    fn reserve_order(&mut self) -> usize {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    fn handle_tag(&mut self, tag: &Tag, order: usize) {
        match tag {
            Tag::Start(..) => {
                let loc = tag.location();
                if self.equations.contains_key(&loc) {
                    self.active_math.push(ActiveMath {
                        loc,
                        order,
                        bounds: None,
                        baseline: None,
                        rot_60k: 0,
                        fallback: EcoString::new(),
                    });
                }
            }
            Tag::End(loc, ..) => {
                let Some(index) =
                    self.active_math.iter().rposition(|active| active.loc == *loc)
                else {
                    return;
                };
                let active = self.active_math.remove(index);
                self.emit_math_box(active);
            }
        }
    }

    fn capture_math_item(
        &mut self,
        item: &'a FrameItem,
        item_transform: Transform,
    ) -> bool {
        if self.active_math.is_empty() {
            return false;
        }

        match item {
            FrameItem::Group(group) => {
                let group_transform = item_transform.pre_concat(group.transform);
                self.add_math_bounds(transformed_rect(
                    group_transform,
                    group.frame.size(),
                ));
                self.record_math_frame_text(&group.frame, group_transform);
                append_frame_text(
                    &group.frame,
                    &mut self.active_math.last_mut().unwrap().fallback,
                );
            }
            FrameItem::Text(text) => {
                self.add_math_bounds(text_item_rect(text, item_transform));
                self.record_math_text(text, item_transform);
                self.active_math.last_mut().unwrap().fallback.push_str(&text.text);
            }
            FrameItem::Shape(shape, _) => {
                self.add_math_bounds(transformed_layout_rect(
                    item_transform,
                    shape.bbox(true),
                ));
            }
            FrameItem::Image(_, size, _) | FrameItem::Link(_, size) => {
                self.add_math_bounds(transformed_rect(item_transform, *size));
            }
            FrameItem::Tag(_) => {}
        }

        true
    }

    fn record_math_frame_text(&mut self, frame: &Frame, transform: Transform) {
        for (pos, item) in frame.items() {
            let item_transform = transform.pre_concat(Transform::translate(pos.x, pos.y));
            match item {
                FrameItem::Text(text) => self.record_math_text(text, item_transform),
                FrameItem::Group(group) => {
                    self.record_math_frame_text(
                        &group.frame,
                        item_transform.pre_concat(group.transform),
                    );
                }
                _ => {}
            }
        }
    }

    fn record_math_text(
        &mut self,
        _text: &typst_library::text::TextItem,
        item_transform: Transform,
    ) {
        let Some(similarity) = classify_similarity(item_transform) else {
            return;
        };
        let active = self.active_math.last_mut().expect("active math exists");
        if active.baseline.is_none() {
            active.baseline = Some(Point::zero().transform(item_transform));
            active.rot_60k = similarity.rot_60k;
        }
    }

    fn add_math_bounds(&mut self, rect: Rect) {
        let active = self.active_math.last_mut().expect("active math exists");
        active.bounds = Some(match active.bounds {
            Some(bounds) => bounds.union(rect),
            None => rect,
        });
    }

    fn emit_math_box(&mut self, active: ActiveMath) {
        let Some(source) = self.equations.get(&active.loc) else {
            return;
        };
        let Some(bounds) = active.bounds else {
            return;
        };
        let block = source.block;
        let omml = source.omml.clone();
        let source_fallback = source.fallback.clone();

        let size = bounds.size();
        let fallback =
            if active.fallback.is_empty() { source_fallback } else { active.fallback };

        if block {
            self.shapes.push(OrderedShape {
                order: active.order,
                shape: SlideShape::MathBox(MathBox {
                    x_emu: crate::text::emu(bounds.min.x),
                    y_emu: crate::text::emu(bounds.min.y),
                    w_emu: crate::text::extent_emu(size.x),
                    h_emu: crate::text::extent_emu(size.y),
                    rot_60k: 0,
                    omml,
                    fallback,
                }),
            });
        } else {
            self.inline_math.push(InlineMathSource {
                order: active.order,
                baseline: active
                    .baseline
                    .unwrap_or_else(|| Point::new(bounds.min.x, bounds.max.y)),
                min: bounds.min,
                max: bounds.max,
                rot_60k: active.rot_60k,
                omml,
                fallback,
            });
        }
    }

    fn emit_image(
        &mut self,
        order: usize,
        image: &typst_library::visualize::Image,
        size: Size,
        span: typst_syntax::Span,
        item_transform: Transform,
    ) {
        // A translation-only placement embeds the original bytes verbatim (or
        // a natural-size render for SVG/PDF kinds); anything rotated, scaled,
        // or skewed goes through the transform-carrying render fallback.
        if let Some(sim) = classify_similarity(item_transform)
            && sim.rot_60k == 0
            && (sim.scale - 1.0).abs() < 1e-6
            && let Some((media, off, sz)) =
                crate::image::embed_image(self.ctx, image, size)
        {
            let pos = Point::zero().transform(item_transform) + off;
            self.push_pic(order, media, pos, sz, image.alt().map(Into::into));
            return;
        }
        debug_raster("image", "transform-or-kind", 0);
        self.raster_item(
            order,
            FrameItem::Image(image.clone(), size, span),
            item_transform,
            image.alt().map(Into::into),
        );
    }

    /// Renders one frame item through its full accumulated transform and
    /// places the picture where the ink lands — the render-what-you-see
    /// fallback for anything without a native PPTX form.
    fn raster_item(
        &mut self,
        order: usize,
        item: FrameItem,
        item_transform: Transform,
        alt: Option<EcoString>,
    ) {
        let mut inner = Frame::soft(Size::zero());
        inner.push(Point::zero(), item);
        let mut group = typst_library::layout::GroupItem::new(inner);
        group.transform = item_transform;
        let mut outer = Frame::soft(Size::zero());
        outer.push(Point::zero(), FrameItem::Group(group));
        if let Some((media, off, size)) = crate::image::raster_fallback(self.ctx, outer) {
            self.push_pic(order, media, off, size, alt);
        }
    }

    fn try_emit_clipped_image(
        &mut self,
        order: usize,
        frame: &'a Frame,
        group_transform: Transform,
        geom: PicGeom,
    ) -> bool {
        let Some((pos, image, size)) = single_frame_image(frame) else {
            return false;
        };
        // Only translation is representable as a `p:pic` placement.
        let Some(sim) = classify_similarity(group_transform) else {
            return false;
        };
        if sim.rot_60k != 0 || (sim.scale - 1.0).abs() > 1e-6 {
            return false;
        }
        // The image must COVER the whole frame; the visible frame is the crop
        // the clip reveals. A gap (image smaller than the frame on any side)
        // would expose background we can't represent, so fall back to raster.
        let Some(src_rect) = cover_src_rect(pos, size, frame.size()) else {
            return false;
        };
        // Embed the original bytes and place the picture at the frame's bounds
        // (in outer coordinates); the geom rounds it and the srcRect crops the
        // cover overflow.
        let Some((media, off, _sz)) =
            crate::image::embed_original_image(self.ctx, image, size)
        else {
            return false;
        };
        if !point_is_zero(off) {
            return false;
        }
        let pic_pos = Point::zero().transform(group_transform);
        self.push_pic_with_geom(
            order,
            media,
            pic_pos,
            frame.size(),
            image.alt().map(Into::into),
            (geom, (src_rect != [0; 4]).then_some(src_rect)),
        );
        true
    }

    fn push_pic(
        &mut self,
        order: usize,
        media: crate::dom::MediaId,
        pos: Point,
        size: Size,
        alt: Option<EcoString>,
    ) {
        self.push_pic_with_geom(order, media, pos, size, alt, (PicGeom::Rect, None));
    }

    /// `shape` is the preset geometry paired with an optional `a:srcRect` crop.
    fn push_pic_with_geom(
        &mut self,
        order: usize,
        media: crate::dom::MediaId,
        pos: Point,
        size: Size,
        alt: Option<EcoString>,
        shape: (PicGeom, Option<[i32; 4]>),
    ) {
        let (geom, src_rect) = shape;
        self.shapes.push(OrderedShape {
            order,
            shape: SlideShape::Pic(crate::dom::Pic {
                x_emu: crate::text::emu(pos.x),
                y_emu: crate::text::emu(pos.y),
                w_emu: crate::text::extent_emu(size.x),
                h_emu: crate::text::extent_emu(size.y),
                rot_60k: 0,
                media,
                alt,
                geom,
                src_rect,
            }),
        });
    }

    fn destination(&self, dest: &Destination) -> Option<LinkTarget> {
        match dest {
            Destination::Url(url) => Some(LinkTarget::Url(EcoString::from(url.as_str()))),
            Destination::Position(pos) => self.slide_target(pos.page.get()),
            Destination::Location(loc) => self
                .document
                .introspector()
                .position(*loc)
                .and_then(|pos| self.slide_target(pos.page.get())),
        }
    }

    /// A same-deck jump target for a 1-based page number, dropped if it falls
    /// outside the exported slides — otherwise a `--pages` subset (or a jump
    /// past the last page) would emit a relationship to a missing slide, which
    /// PowerPoint treats as a corrupt file.
    fn slide_target(&self, page_1based: usize) -> Option<LinkTarget> {
        let index = page_1based.checked_sub(1)?;
        (index < self.document.pages().len()).then_some(LinkTarget::Slide(index))
    }
}

fn single_frame_image(
    frame: &Frame,
) -> Option<(Point, &typst_library::visualize::Image, Size)> {
    let mut image = None;
    for (pos, item) in frame.items() {
        match item {
            FrameItem::Image(value, size, _) => {
                if image.is_some() {
                    return None;
                }
                image = Some((*pos, value, *size));
            }
            // A clipped image is wrapped in one or more plain pass-through
            // groups (Typst nests content + introspection tags): recurse into a
            // group that doesn't clip and only translates, mapping the found
            // image's position back into this frame's coordinates.
            FrameItem::Group(group) => {
                // Recurse through a wrapper group that only translates and whose
                // clip (if any) is a plain bounding rectangle — a no-op the
                // image fills exactly. `#box(radius:.., clip: true, image)`
                // nests a rounded-clip group (captured by the outer geom) around
                // an inner rect-clip group holding the image.
                let clip_ok = match &group.clip {
                    None => true,
                    Some(c) => {
                        *c == typst_library::visualize::Curve::rect(group.frame.size())
                    }
                };
                if !clip_ok || image.is_some() {
                    return None;
                }
                let sim = classify_similarity(group.transform)?;
                if sim.rot_60k != 0 || (sim.scale - 1.0).abs() > 1e-6 {
                    return None;
                }
                let (ipos, value, size) = single_frame_image(&group.frame)?;
                let tpos = ipos.transform(group.transform);
                image = Some((Point::new(pos.x + tpos.x, pos.y + tpos.y), value, size));
            }
            FrameItem::Shape(shape, _) if shape_draws_no_ink(shape) => {}
            FrameItem::Link(_, _) | FrameItem::Tag(_) => {}
            _ => return None,
        }
    }
    image
}

fn equation_sources(document: &PagedDocument) -> FxHashMap<Location, MathSource> {
    let styles = StyleChain::default();
    document
        .introspector()
        .query(&EquationElem::ELEM.select())
        .into_iter()
        .filter_map(|content| {
            let elem = content.to_packed::<EquationElem>()?;
            let block = elem.block.get(styles);
            let loc = elem.location()?;
            let omml = typst_ooxml_core::omml::equation_omml_fragment(elem)?;
            let fallback = elem
                .alt
                .get_cloned(styles)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| elem.body.plain_text());
            Some((loc, MathSource { omml, fallback, block }))
        })
        .collect()
}

/// The `a:srcRect` crop `[l, t, r, b]` (1/1000 %) that reveals `frame` out of an
/// image placed at `pos` with `size`, or `None` if the image does not fully
/// cover the frame (a gap would expose background the clip can't represent).
fn cover_src_rect(pos: Point, size: Size, frame: Size) -> Option<[i32; 4]> {
    let (ix, iy) = (pos.x.to_pt(), pos.y.to_pt());
    let (iw, ih) = (size.x.to_pt(), size.y.to_pt());
    let (fw, fh) = (frame.x.to_pt(), frame.y.to_pt());
    const EPS: f64 = 0.05;
    if iw <= EPS || ih <= EPS {
        return None;
    }
    // The image must extend past every edge of the frame (cover, not contain).
    if ix > EPS || iy > EPS || ix + iw < fw - EPS || iy + ih < fh - EPS {
        return None;
    }
    let frac = |amount: f64, span: f64| {
        ((amount.max(0.0) / span) * 100_000.0).round().clamp(0.0, 99_000.0) as i32
    };
    Some([frac(-ix, iw), frac(-iy, ih), frac(ix + iw - fw, iw), frac(iy + ih - fh, ih)])
}

fn shape_draws_no_ink(shape: &Shape) -> bool {
    let fill_draws = shape.fill.as_ref().is_some_and(paint_draws_ink);
    let stroke_draws = shape.stroke.as_ref().is_some_and(|stroke| {
        stroke.thickness.to_pt() > 0.0 && paint_draws_ink(&stroke.paint)
    });
    !fill_draws && !stroke_draws
}

fn paint_draws_ink(paint: &Paint) -> bool {
    match paint {
        Paint::Solid(color) => crate::shape::srgb_bytes(color)[3] != 0,
        Paint::Gradient(_) | Paint::Tiling(_) => true,
    }
}

fn point_is_zero(point: Point) -> bool {
    near_abs(point.x, Abs::zero()) && near_abs(point.y, Abs::zero())
}

fn near_abs(a: Abs, b: Abs) -> bool {
    (a - b).abs().to_pt() <= 0.01
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

    // Not f64::hypot / f64::atan2 below: those are platform-dependent libm
    // calls and would break byte-reproducible output; sqrt is IEEE-exact and
    // Angle::atan2 is the deterministic wrapper.
    let len_x = (sx * sx + ky * ky).sqrt();
    let len_y = (kx * kx + sy * sy).sqrt();
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

    let theta = typst_library::layout::Angle::atan2(ky, sx).to_deg();
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

fn text_item_rect(text: &typst_library::text::TextItem, transform: Transform) -> Rect {
    let size = text.size;
    let descent = (-text.font.metrics().descender).at(size);
    let min = Point::new(Abs::zero(), -size);
    let max = Point::new(text.width(), descent);
    transformed_corners(transform, min, max)
}

fn transformed_rect(transform: Transform, size: Size) -> Rect {
    transformed_corners(transform, Point::zero(), size.to_point())
}

fn transformed_layout_rect(
    transform: Transform,
    rect: typst_library::layout::Rect,
) -> Rect {
    transformed_corners(transform, rect.min, rect.max)
}

fn transformed_corners(transform: Transform, min: Point, max: Point) -> Rect {
    let points = [min, Point::new(max.x, min.y), Point::new(min.x, max.y), max];
    let mut min = Point::splat(Abs::inf());
    let mut max = Point::splat(-Abs::inf());
    for point in points {
        let transformed = point.transform(transform);
        min = min.min(transformed);
        max = max.max(transformed);
    }
    Rect { min, max }
}

fn append_frame_text(frame: &Frame, out: &mut EcoString) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Text(text) => out.push_str(&text.text),
            FrameItem::Group(group) => append_frame_text(&group.frame, out),
            _ => {}
        }
    }
}

impl Rect {
    fn union(self, other: Rect) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn size(self) -> Size {
        Size::new(
            (self.max.x - self.min.x).max(Abs::pt(0.1)),
            (self.max.y - self.min.y).max(Abs::pt(0.1)),
        )
    }

    fn overlaps(self, other: Rect) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }
}

/// Logs one rasterization-fallback event when `PPTX_DEBUG_RASTER` is set —
/// the audit hook for measuring how much content bypasses the native mappers
/// (mirrors `DOCX_DEBUG_RASTER`). `text_chars` counts the live text characters
/// swallowed by the raster; a nonzero count on a group is the signal that
/// editable text was lost to a picture.
fn debug_raster(kind: &str, reason: &str, text_chars: usize) {
    if std::env::var_os("PPTX_DEBUG_RASTER").is_some() {
        eprintln!("RASTERIZE kind={kind} reason={reason} text_chars={text_chars}");
    }
}

/// Total text characters in a frame tree (for the raster audit).
fn frame_text_chars(frame: &Frame) -> usize {
    let mut n = 0;
    for (_, item) in frame.items() {
        match item {
            FrameItem::Text(text) => n += text.text.chars().count(),
            FrameItem::Group(group) => n += frame_text_chars(&group.frame),
            _ => {}
        }
    }
    n
}

fn background(page: &Page) -> Option<FillSpec> {
    let fill = page.fill_or_white();
    fill.as_ref()?;
    // A solid or linear-gradient page fill maps to a native slide background;
    // a tiling or non-linear gradient we cannot represent falls back to white.
    match crate::shape::resolved_fill(&fill) {
        Some(spec) => spec,
        None => Some(FillSpec::Solid([255, 255, 255, 255])),
    }
}

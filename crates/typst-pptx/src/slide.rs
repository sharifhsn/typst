use ecow::EcoString;
use rustc_hash::FxHashMap;
use typst_layout::{Page, PagedDocument};
use typst_library::foundations::{NativeElement, StyleChain};
use typst_library::introspection::{CounterDisplayElem, Introspector, Location, Tag};
use typst_library::layout::{
    Abs, ColumnRegion, Frame, FrameItem, Point, Size, Transform,
};
use typst_library::math::EquationElem;
use typst_library::model::{Destination, Numbering};
use typst_library::visualize::{Geometry, Paint, Shape};
use typst_ooxml_core::{dml, units};

use crate::dom::{
    FillSpec, LinkOverlay, MathBox, PicGeom, RunLink, SlideCtx, SlideIr, SlideShape,
    TextBox, TextColumns, TextPara, TextWrap,
};
use crate::table::{ActiveTable, ActiveTableCell, CapturedTableCell};
use crate::text::{InlineMathSource, LinkTarget, TextSource};

/// Convert all pages into slide IR.
pub fn slides(document: &PagedDocument, ctx: &mut SlideCtx) -> Vec<SlideIr> {
    document
        .pages()
        .iter()
        .enumerate()
        .map(|(index, page)| slide(document, page, index, ctx))
        .collect()
}

fn slide(
    document: &PagedDocument,
    page: &Page,
    slide_index: usize,
    ctx: &mut SlideCtx,
) -> SlideIr {
    let mut walker = Walker::new(document, page, slide_index, ctx);
    walker.walk_frame(&page.frame, Transform::identity());
    walker.emit_loose_tables();
    walker
        .link_overlays
        .extend(text_link_overlays(&walker.text, &walker.links));
    attach_highlights(&mut walker.text, &mut walker.shapes, &walker.highlight_candidates);
    attach_shape_links(&mut walker.shapes, &walker.links);

    let mut ordered = walker.shapes;
    ordered.extend(
        crate::text::cluster_text(walker.text, walker.inline_math)
            .into_iter()
            .map(|cluster| OrderedShape { order: cluster.order, shape: cluster.shape }),
    );
    ordered.sort_by_key(|entry| entry.order);

    let mut shapes: Vec<_> = ordered.into_iter().map(|entry| entry.shape).collect();
    shapes.extend(walker.link_overlays.into_iter().map(SlideShape::LinkOverlay));
    SlideIr { bg: background(page), shapes }
}

pub(super) struct Walker<'a, 'b> {
    document: &'a PagedDocument,
    pub(super) ctx: &'b mut SlideCtx,
    next_order: usize,
    pub(super) shapes: Vec<OrderedShape>,
    text: Vec<TextSource<'a>>,
    inline_math: Vec<InlineMathSource>,
    links: Vec<LinkRect>,
    pub(super) link_overlays: Vec<LinkOverlay>,
    highlight_candidates: Vec<HighlightCandidate>,
    equations: FxHashMap<Location, MathSource>,
    page_size: Size,
    slide_number_fallback: Option<EcoString>,
    active_math: Vec<ActiveMath>,
    active_slide_numbers: Vec<ActiveSlideNumber>,
    // Native table capture is implemented in `table`; the walker owns the
    // stacks so traversal order and nested frame recursion stay centralized.
    pub(super) active_tables: Vec<ActiveTable>,
    pub(super) loose_table_cells: Vec<CapturedTableCell>,
    pub(super) active_table_cells: Vec<ActiveTableCell<'a>>,
    active_columns: Vec<ActiveColumnRegion<'a>>,
}

pub(super) struct OrderedShape {
    pub(super) order: usize,
    pub(super) shape: SlideShape,
}

pub(super) struct LinkRect {
    pub(super) rect: Rect,
    pub(super) target: LinkTarget,
}

#[derive(Copy, Clone)]
pub(super) struct HighlightCandidate {
    order: usize,
    rect: Rect,
    color: [u8; 4],
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

struct ActiveSlideNumber {
    loc: Location,
    expected: EcoString,
    text_indices: Vec<usize>,
    fallback: EcoString,
    bounds: Option<Rect>,
}

struct ActiveColumnRegion<'a> {
    loc: Location,
    order: usize,
    rect: Rect,
    count: usize,
    gutter: Abs,
    manual_break: bool,
    text: Vec<TextSource<'a>>,
    math: Vec<InlineMathSource>,
    links: Vec<LinkRect>,
    highlights: Vec<HighlightCandidate>,
}

#[derive(Copy, Clone)]
pub(super) struct Rect {
    pub(super) min: Point,
    pub(super) max: Point,
}

#[derive(Copy, Clone)]
pub(super) struct Similarity {
    pub(super) rot_60k: i32,
    pub(super) scale: f64,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(
        document: &'a PagedDocument,
        page: &Page,
        slide_index: usize,
        ctx: &'b mut SlideCtx,
    ) -> Self {
        Self {
            document,
            ctx,
            next_order: 0,
            shapes: Vec::new(),
            text: Vec::new(),
            inline_math: Vec::new(),
            links: Vec::new(),
            link_overlays: Vec::new(),
            highlight_candidates: Vec::new(),
            equations: equation_sources(document),
            page_size: page.frame.size(),
            slide_number_fallback: slide_number_fallback(page, slide_index),
            active_math: Vec::new(),
            active_slide_numbers: Vec::new(),
            active_tables: Vec::new(),
            loose_table_cells: Vec::new(),
            active_table_cells: Vec::new(),
            active_columns: Vec::new(),
        }
    }

    pub(super) fn walk_frame(&mut self, frame: &'a Frame, transform: Transform) {
        for (pos, item) in frame.items() {
            let order = self.reserve_order();
            let item_transform = transform.pre_concat(Transform::translate(pos.x, pos.y));
            if !matches!(item, FrameItem::Tag(_)) {
                // Equation glyphs inside a table cell must reach the math collector
                // before the cell's general text collector flattens them into runs.
                if !self.active_table_cells.is_empty()
                    && self.capture_math_item(item, item_transform)
                {
                    continue;
                }
                if self.capture_table_item(order, item, item_transform) {
                    continue;
                }
                if !self.active_tables.is_empty() && !matches!(item, FrameItem::Group(_))
                {
                    continue;
                }
                // Equation tags can be nested inside a column region. Capture their
                // rendered items before the column's general text collector, or the
                // equation is flattened into glyph runs and loses its OMML structure.
                if self.capture_math_item(item, item_transform) {
                    continue;
                }
                if self.capture_column_item(order, item, item_transform) {
                    continue;
                }
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
                    } else if !self.active_tables.is_empty() {
                        continue;
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
                        self.ctx.add_font(text.font.font());
                        let baseline = Point::zero().transform(item_transform);
                        let index = self.text.len();
                        self.text.push(TextSource {
                            order,
                            baseline,
                            item: text,
                            rot_60k: similarity.rot_60k,
                            scale: similarity.scale,
                            highlight: None,
                            slide_number: false,
                        });
                        self.record_slide_number_text(index, text, item_transform);
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
                    let highlight =
                        highlight_candidate(shape, *span, item_transform, order);
                    match crate::shape::shape_to_geom(self.ctx, shape, item_transform, 0)
                    {
                        Some(geom) => {
                            self.shapes.push(OrderedShape {
                                order,
                                shape: SlideShape::Geom(geom),
                            });
                            if let Some(highlight) = highlight {
                                if let Some(column) = self.active_columns.last_mut() {
                                    column.highlights.push(highlight);
                                } else {
                                    self.highlight_candidates.push(highlight);
                                }
                            }
                        }
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
                FrameItem::Tag(tag) => self.handle_tag(tag, order, item_transform),
            }
        }
    }

    fn reserve_order(&mut self) -> usize {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    fn handle_tag(&mut self, tag: &Tag, order: usize, item_transform: Transform) {
        match tag {
            Tag::Start(..) => {
                if self.start_table_cell(tag, order, item_transform) {
                    return;
                }
                if self.start_table(tag, order) {
                    return;
                }
                if self.start_column_region(tag, order, item_transform) {
                    return;
                }
                if self.start_slide_number(tag) {
                    return;
                }
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
                if self.end_slide_number(*loc) {
                    return;
                }
                if self.end_table_cell(*loc) {
                    return;
                }
                if self.end_table(*loc) {
                    return;
                }
                if self.end_column_region(*loc) {
                    return;
                }
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

    fn start_slide_number(&mut self, tag: &Tag) -> bool {
        let Some(expected) = self.slide_number_fallback.clone() else {
            return false;
        };
        let Tag::Start(elem, ..) = tag else {
            return false;
        };
        if elem.to_packed::<CounterDisplayElem>().is_none() {
            return false;
        }

        self.active_slide_numbers.push(ActiveSlideNumber {
            loc: tag.location(),
            expected,
            text_indices: Vec::new(),
            fallback: EcoString::new(),
            bounds: None,
        });
        true
    }

    fn end_slide_number(&mut self, loc: Location) -> bool {
        let Some(index) =
            self.active_slide_numbers.iter().rposition(|active| active.loc == loc)
        else {
            return false;
        };
        let active = self.active_slide_numbers.remove(index);
        let Some(bounds) = active.bounds else {
            return true;
        };
        if active.text_indices.len() == 1
            && active.fallback.trim() == active.expected.as_str()
            && slide_number_region(bounds, self.page_size)
        {
            for idx in active.text_indices {
                if let Some(source) = self.text.get_mut(idx) {
                    source.slide_number = true;
                }
            }
        }
        true
    }

    fn record_slide_number_text(
        &mut self,
        index: usize,
        text: &typst_library::text::TextItem,
        item_transform: Transform,
    ) {
        let Some(active) = self.active_slide_numbers.last_mut() else {
            return;
        };
        active.text_indices.push(index);
        active.fallback.push_str(&text.text);
        let rect = text_item_rect(text, item_transform);
        active.bounds = Some(match active.bounds {
            Some(bounds) => bounds.union(rect),
            None => rect,
        });
    }

    fn start_column_region(
        &mut self,
        tag: &Tag,
        order: usize,
        item_transform: Transform,
    ) -> bool {
        let Tag::Start(elem, ..) = tag else {
            return false;
        };
        let Some(region) = elem.to_packed::<ColumnRegion>() else {
            return false;
        };
        let Some(similarity) = classify_similarity(item_transform) else {
            return true;
        };
        if similarity.rot_60k != 0 {
            return true;
        }

        let origin = Point::zero().transform(item_transform);
        let size =
            Size::new(region.width * similarity.scale, region.height * similarity.scale);
        self.active_columns.push(ActiveColumnRegion {
            loc: tag.location(),
            order,
            rect: Rect { min: origin, max: origin + size.to_point() },
            count: region.count.get(),
            gutter: region.gutter * similarity.scale,
            manual_break: region.manual_break,
            text: Vec::new(),
            math: Vec::new(),
            links: Vec::new(),
            highlights: Vec::new(),
        });
        true
    }

    fn end_column_region(&mut self, loc: Location) -> bool {
        let Some(index) =
            self.active_columns.iter().rposition(|active| active.loc == loc)
        else {
            return false;
        };
        let mut active = self.active_columns.remove(index);
        attach_highlights(&mut active.text, &mut self.shapes, &active.highlights);
        self.link_overlays
            .extend(text_link_overlays(&active.text, &active.links));
        self.shapes.extend(column_shapes(active));
        true
    }

    fn capture_column_item(
        &mut self,
        order: usize,
        item: &'a FrameItem,
        item_transform: Transform,
    ) -> bool {
        if self.active_columns.is_empty() {
            return false;
        }

        match item {
            FrameItem::Text(text) => {
                let Some(similarity) = classify_similarity(item_transform) else {
                    return false;
                };
                self.ctx.add_font(text.font.font());
                let baseline = Point::zero().transform(item_transform);
                self.active_columns.last_mut().unwrap().text.push(TextSource {
                    order,
                    baseline,
                    item: text,
                    rot_60k: similarity.rot_60k,
                    scale: similarity.scale,
                    highlight: None,
                    slide_number: false,
                });
                true
            }
            FrameItem::Link(dest, size) => {
                let Some(target) = self.destination(dest) else {
                    return true;
                };
                self.active_columns.last_mut().unwrap().links.push(LinkRect {
                    rect: transformed_rect(item_transform, *size),
                    target,
                });
                true
            }
            _ => false,
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
            if source_fallback.is_empty() { active.fallback } else { source_fallback };

        if block && self.active_table_cells.is_empty() {
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
            let math = InlineMathSource {
                order: active.order,
                baseline: active
                    .baseline
                    .unwrap_or_else(|| Point::new(bounds.min.x, bounds.max.y)),
                min: bounds.min,
                max: bounds.max,
                rot_60k: active.rot_60k,
                omml,
                fallback,
            };
            // A table cell cannot host a free-standing slide shape, but its
            // text body can carry OMML. Keep both inline and display equations
            // in the cell's editable text flow.
            if let Some(cell) = self.active_table_cells.last_mut() {
                cell.math.push(math);
            } else if let Some(column) = self.active_columns.last_mut() {
                column.math.push(math);
            } else {
                self.inline_math.push(math);
            }
        }
    }

    pub(super) fn emit_image(
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
            && let Some(embedded) = crate::image::embed_image(self.ctx, image, size)
        {
            let pos = Point::zero().transform(item_transform) + embedded.offset;
            self.push_pic(
                order,
                (embedded.media, embedded.svg_media),
                pos,
                embedded.size,
                image.alt().map(Into::into),
            );
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
    pub(super) fn raster_item(
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
            self.push_pic(order, (media, None), off, size, alt);
        }
    }

    pub(super) fn try_emit_clipped_image(
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
        let Some(embedded) = crate::image::embed_original_image(self.ctx, image, size)
        else {
            return false;
        };
        if !point_is_zero(embedded.offset) {
            return false;
        }
        let pic_pos = Point::zero().transform(group_transform);
        self.push_pic_with_geom(
            order,
            (embedded.media, embedded.svg_media),
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
        media: (crate::dom::MediaId, Option<crate::dom::MediaId>),
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
        media: (crate::dom::MediaId, Option<crate::dom::MediaId>),
        pos: Point,
        size: Size,
        alt: Option<EcoString>,
        shape: (PicGeom, Option<[i32; 4]>),
    ) {
        let (media, svg_media) = media;
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
                svg_media,
                alt,
                link: None,
                geom,
                src_rect,
            }),
        });
    }

    pub(super) fn destination(&self, dest: &Destination) -> Option<LinkTarget> {
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

fn slide_number_fallback(page: &Page, slide_index: usize) -> Option<EcoString> {
    let physical = u64::try_from(slide_index).ok()?.checked_add(1)?;
    if page.number != physical {
        return None;
    }

    let Numbering::Pattern(pattern) = page.numbering.as_ref()? else {
        return None;
    };
    if pattern.pieces() != 1 {
        return None;
    }

    let expected = EcoString::from(physical.to_string());
    let rendered = pattern.apply(None, &[page.number]).ok()?;
    (rendered == expected).then_some(rendered)
}

fn slide_number_region(bounds: Rect, page_size: Size) -> bool {
    let page_w = page_size.x.to_pt();
    let page_h = page_size.y.to_pt();
    if page_w <= 0.0 || page_h <= 0.0 {
        return false;
    }

    let width = (bounds.max.x - bounds.min.x).to_pt().max(0.0);
    let height = (bounds.max.y - bounds.min.y).to_pt().max(0.0);
    let center_y = (bounds.min.y.to_pt() + bounds.max.y.to_pt()) / 2.0;
    let near_header_or_footer = center_y <= page_h * 0.18 || center_y >= page_h * 0.82;

    near_header_or_footer && width <= page_w * 0.25 && height <= page_h * 0.12
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
                .or_else(|| typst_ooxml_core::omml::omml_fallback_text(&omml))
                .unwrap_or_else(|| elem.body.plain_text());
            Some((loc, MathSource { omml, fallback, block }))
        })
        .collect()
}

fn column_shapes(active: ActiveColumnRegion<'_>) -> Vec<OrderedShape> {
    if active.count <= 1 {
        return Vec::new();
    }

    let rect = active.rect;
    let count = active.count;
    let gutter = active.gutter;
    let manual_break = active.manual_break;
    let buckets = column_region_buckets(active.text, active.math, rect, count, gutter);
    if buckets.is_empty() {
        return Vec::new();
    }

    let size = rect.size();
    if !manual_break {
        let paras = buckets.into_iter().flat_map(|bucket| bucket.paras).collect();
        return vec![OrderedShape {
            order: active.order,
            shape: SlideShape::TextBox(TextBox {
                x_emu: crate::text::emu(rect.min.x),
                y_emu: crate::text::emu(rect.min.y),
                w_emu: crate::text::extent_emu(size.x),
                h_emu: crate::text::extent_emu(size.y),
                rot_60k: 0,
                wrap: TextWrap::Square,
                columns: Some(TextColumns {
                    count,
                    gutter_emu: crate::text::extent_emu(gutter),
                }),
                placeholder: None,
                paras,
            }),
        }];
    }

    let total_gutter = gutter * (count.saturating_sub(1) as f64);
    let col_width = ((size.x - total_gutter) / count as f64).max(Abs::pt(1.0));
    let stride = col_width + gutter;
    buckets
        .into_iter()
        .map(|bucket| {
            let x = rect.min.x + stride * bucket.index as f64;
            OrderedShape {
                order: bucket.order,
                shape: SlideShape::TextBox(TextBox {
                    x_emu: crate::text::emu(x),
                    y_emu: crate::text::emu(rect.min.y),
                    w_emu: crate::text::extent_emu(col_width),
                    h_emu: crate::text::extent_emu(size.y),
                    rot_60k: 0,
                    wrap: TextWrap::Square,
                    columns: None,
                    placeholder: None,
                    paras: bucket.paras,
                }),
            }
        })
        .collect()
}

struct ColumnBucket {
    index: usize,
    order: usize,
    paras: Vec<TextPara>,
}

/// Reassemble each physical column independently in true reading order.
///
/// Automatic overflow can concatenate these buckets into one native multi-column
/// text box. Explicit `#colbreak()` regions retain the buckets as independent
/// editable text boxes because DrawingML has no manual column-break primitive.
fn column_region_buckets(
    text: Vec<TextSource<'_>>,
    math: Vec<InlineMathSource>,
    rect: Rect,
    count: usize,
    gutter: Abs,
) -> Vec<ColumnBucket> {
    let total_gutter = gutter * (count.saturating_sub(1) as f64);
    let col_width = ((rect.size().x - total_gutter) / count as f64).max(Abs::pt(1.0));
    let stride = col_width + gutter;

    let mut text_buckets: Vec<Vec<TextSource<'_>>> = vec![Vec::new(); count];
    for source in text {
        let offset = (source.baseline.x - rect.min.x).max(Abs::zero());
        let index = ((offset.to_pt() / stride.to_pt()) as usize).min(count - 1);
        text_buckets[index].push(source);
    }
    let mut math_buckets: Vec<Vec<InlineMathSource>> = vec![Vec::new(); count];
    for source in math {
        let offset = (source.min.x - rect.min.x).max(Abs::zero());
        let index = ((offset.to_pt() / stride.to_pt()) as usize).min(count - 1);
        math_buckets[index].push(source);
    }

    // Order columns by reading order (the minimum walk-order of their
    // contents), not raw bucket index, so this stays correct for RTL columns.
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| {
        text_buckets[i]
            .iter()
            .map(|s| s.order)
            .chain(math_buckets[i].iter().map(|s| s.order))
            .min()
            .unwrap_or(usize::MAX)
    });

    let mut buckets = Vec::new();
    for i in order {
        let text = std::mem::take(&mut text_buckets[i]);
        let math = std::mem::take(&mut math_buckets[i]);
        if text.is_empty() && math.is_empty() {
            continue;
        }
        let order = text
            .iter()
            .map(|source| source.order)
            .chain(math.iter().map(|source| source.order))
            .min()
            .unwrap_or(usize::MAX);
        let mut paras = Vec::new();
        for cluster in crate::text::cluster_text(text, math) {
            if let SlideShape::TextBox(text) = cluster.shape {
                paras.extend(text.paras);
            }
        }
        if !paras.is_empty() {
            buckets.push(ColumnBucket { index: i, order, paras });
        }
    }
    buckets
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

pub(super) fn highlight_candidate(
    shape: &Shape,
    span: typst_syntax::Span,
    transform: Transform,
    order: usize,
) -> Option<HighlightCandidate> {
    if !span.is_detached() || shape.stroke.is_some() {
        return None;
    }
    let Geometry::Rect(size) = shape.geometry else {
        return None;
    };
    let Some(Paint::Solid(color)) = &shape.fill else {
        return None;
    };
    Some(HighlightCandidate {
        order,
        rect: transformed_rect(transform, size),
        color: crate::shape::srgb_bytes(color),
    })
}

pub(super) fn attach_highlights(
    text: &mut [TextSource<'_>],
    shapes: &mut Vec<OrderedShape>,
    candidates: &[HighlightCandidate],
) {
    let mut consumed = Vec::new();
    for candidate in candidates {
        let mut best: Option<(usize, f64)> = None;
        for (index, source) in text.iter().enumerate() {
            if source.rot_60k != 0
                || source.scale <= 0.0
                || source.order.abs_diff(candidate.order) > 2
            {
                continue;
            }
            let rect = text_rect(source.item, source.baseline, source.scale);
            let font_size = source.item.size * source.scale;
            if candidate.rect.size().y < font_size * 0.4
                || candidate.rect.size().y > font_size * 1.75
                || candidate.rect.size().x > rect.size().x + font_size * 0.5
            {
                continue;
            }
            let overlap_x = (candidate.rect.max.x.min(rect.max.x)
                - candidate.rect.min.x.max(rect.min.x))
            .max(Abs::zero());
            let overlap_y = (candidate.rect.max.y.min(rect.max.y)
                - candidate.rect.min.y.max(rect.min.y))
            .max(Abs::zero());
            if overlap_x <= Abs::zero() || overlap_y <= Abs::zero() {
                continue;
            }
            let score = overlap_x.to_pt() * overlap_y.to_pt();
            if best.as_ref().is_none_or(|(_, current)| score > *current) {
                best = Some((index, score));
            }
        }
        if let Some((index, _)) = best {
            text[index].highlight = Some(candidate.color);
            consumed.push(candidate.order);
        }
    }
    shapes.retain(|shape| !consumed.contains(&shape.order));
}

pub(super) fn text_link_overlays(
    text: &[TextSource<'_>],
    links: &[LinkRect],
) -> Vec<LinkOverlay> {
    links
        .iter()
        .filter(|link| {
            text.iter().any(|source| {
                text_rect(source.item, source.baseline, source.scale).overlaps(link.rect)
            })
        })
        .map(|link| {
            let size = link.rect.size();
            LinkOverlay {
                x_emu: units::abs_to_emu(link.rect.min.x),
                y_emu: units::abs_to_emu(link.rect.min.y),
                w_emu: units::abs_to_emu(size.x),
                h_emu: units::abs_to_emu(size.y),
                link: match &link.target {
                    LinkTarget::Url(url) => RunLink::Url(url.clone()),
                    LinkTarget::Slide(slide) => RunLink::Slide(*slide),
                },
            }
        })
        .collect()
}

fn attach_shape_links(shapes: &mut [OrderedShape], links: &[LinkRect]) {
    for entry in shapes {
        let (rect, target) = match &mut entry.shape {
            SlideShape::Geom(shape) => (
                shape_rect(shape.x_emu, shape.y_emu, shape.w_emu, shape.h_emu),
                &mut shape.link,
            ),
            SlideShape::Pic(pic) => {
                (shape_rect(pic.x_emu, pic.y_emu, pic.w_emu, pic.h_emu), &mut pic.link)
            }
            _ => continue,
        };
        *target = links.iter().find(|link| rect.overlaps(link.rect)).map(|link| {
            match &link.target {
                LinkTarget::Url(url) => RunLink::Url(url.clone()),
                LinkTarget::Slide(slide) => RunLink::Slide(*slide),
            }
        });
    }
}

fn shape_rect(x_emu: i64, y_emu: i64, w_emu: i64, h_emu: i64) -> Rect {
    let x = Abs::pt(x_emu as f64 / 12_700.0);
    let y = Abs::pt(y_emu as f64 / 12_700.0);
    Rect {
        min: Point::new(x, y),
        max: Point::new(
            x + Abs::pt(w_emu as f64 / 12_700.0),
            y + Abs::pt(h_emu as f64 / 12_700.0),
        ),
    }
}

pub(super) fn classify_similarity(transform: Transform) -> Option<Similarity> {
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

pub(super) fn transformed_rect(transform: Transform, size: Size) -> Rect {
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

    pub(super) fn size(self) -> Size {
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
pub(super) fn debug_raster(kind: &str, reason: &str, text_chars: usize) {
    if std::env::var_os("PPTX_DEBUG_RASTER").is_some() {
        eprintln!("RASTERIZE kind={kind} reason={reason} text_chars={text_chars}");
    }
}

/// Total text characters in a frame tree (for the raster audit).
pub(super) fn frame_text_chars(frame: &Frame) -> usize {
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
    match dml::resolved_fill(&fill, dml::AlphaMode::Preserve) {
        Some(spec) => spec,
        None => Some(FillSpec::Solid([255, 255, 255, 255])),
    }
}

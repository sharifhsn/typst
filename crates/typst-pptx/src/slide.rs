use ecow::EcoString;
use rustc_hash::FxHashMap;
use typst_layout::{Page, PagedDocument};
use typst_library::foundations::{NativeElement, Smart, StyleChain};
use typst_library::introspection::{CounterDisplayElem, Introspector, Location, Tag};
use typst_library::layout::{
    Abs, Frame, FrameItem, GridCell, GridCellRegion, GridElem, Point, Sides, Size,
    Transform,
};
use typst_library::math::EquationElem;
use typst_library::model::{
    Destination, Numbering, TableCell as TypstTableCell, TableElem,
};
use typst_library::visualize::{LineCap, Paint, Shape, Stroke};

use crate::dom::{
    CellBorders, FillSpec, MathBox, PicGeom, SlideCtx, SlideIr, SlideShape, StrokeSpec,
    TableBox, TableCell, TableRow, TextPara,
};
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
    page_size: Size,
    slide_number_fallback: Option<EcoString>,
    active_math: Vec<ActiveMath>,
    active_slide_numbers: Vec<ActiveSlideNumber>,
    active_tables: Vec<ActiveTable>,
    loose_table_cells: Vec<CapturedTableCell>,
    active_table_cells: Vec<ActiveTableCell<'a>>,
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

struct ActiveSlideNumber {
    loc: Location,
    expected: EcoString,
    text_indices: Vec<usize>,
    fallback: EcoString,
    bounds: Option<Rect>,
}

struct ActiveTable {
    loc: Location,
    order: usize,
    cells: Vec<CapturedTableCell>,
}

struct ActiveTableCell<'a> {
    loc: Location,
    order: usize,
    table: bool,
    x: usize,
    y: usize,
    colspan: usize,
    rowspan: usize,
    rect: Rect,
    fill: Option<FillSpec>,
    borders: CellBorders,
    text: Vec<TextSource<'a>>,
    links: Vec<LinkRect>,
}

struct CapturedTableCell {
    order: usize,
    table: bool,
    x: usize,
    y: usize,
    colspan: usize,
    rowspan: usize,
    rect: Rect,
    fill: Option<FillSpec>,
    borders: CellBorders,
    paras: Vec<TextPara>,
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
            equations: equation_sources(document),
            page_size: page.frame.size(),
            slide_number_fallback: slide_number_fallback(page, slide_index),
            active_math: Vec::new(),
            active_slide_numbers: Vec::new(),
            active_tables: Vec::new(),
            loose_table_cells: Vec::new(),
            active_table_cells: Vec::new(),
        }
    }

    fn walk_frame(&mut self, frame: &'a Frame, transform: Transform) {
        for (pos, item) in frame.items() {
            let order = self.reserve_order();
            let item_transform = transform.pre_concat(Transform::translate(pos.x, pos.y));
            if !matches!(item, FrameItem::Tag(_)) {
                if self.capture_table_item(order, item, item_transform) {
                    continue;
                }
                if !self.active_tables.is_empty() && !matches!(item, FrameItem::Group(_))
                {
                    continue;
                }
                if self.capture_math_item(item, item_transform) {
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
                        let baseline = Point::zero().transform(item_transform);
                        let index = self.text.len();
                        self.text.push(TextSource {
                            order,
                            baseline,
                            item: text,
                            rot_60k: similarity.rot_60k,
                            scale: similarity.scale,
                            link: None,
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

    fn start_table(&mut self, tag: &Tag, order: usize) -> bool {
        let Tag::Start(elem, ..) = tag else {
            return false;
        };
        if elem.to_packed::<TableElem>().is_none()
            && elem.to_packed::<GridElem>().is_none()
        {
            return false;
        }

        self.active_tables.push(ActiveTable {
            loc: tag.location(),
            order,
            cells: Vec::new(),
        });
        true
    }

    fn end_table(&mut self, loc: Location) -> bool {
        let Some(index) = self.active_tables.iter().rposition(|active| active.loc == loc)
        else {
            return false;
        };
        let active = self.active_tables.remove(index);
        if let Some(shape) = table_shape(active.order, active.cells) {
            self.shapes.push(shape);
        }
        true
    }

    fn start_table_cell(
        &mut self,
        tag: &Tag,
        order: usize,
        item_transform: Transform,
    ) -> bool {
        let Tag::Start(elem, ..) = tag else {
            return false;
        };
        let Some(region) = elem.to_packed::<GridCellRegion>() else {
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
        let styles = StyleChain::default();
        self.active_table_cells.push(ActiveTableCell {
            loc: tag.location(),
            order,
            table: region.body.is::<TypstTableCell>(),
            x: region.x,
            y: region.y,
            colspan: region.colspan.get(),
            rowspan: region.rowspan.get(),
            rect: Rect { min: origin, max: origin + size.to_point() },
            fill: region_fill(&region.body, styles),
            borders: region_borders(&region.body, styles),
            text: Vec::new(),
            links: Vec::new(),
        });
        true
    }

    fn end_table_cell(&mut self, loc: Location) -> bool {
        let Some(index) =
            self.active_table_cells.iter().rposition(|active| active.loc == loc)
        else {
            return false;
        };
        let mut active = self.active_table_cells.remove(index);
        attach_links(&mut active.text, &active.links);
        let paras = table_cell_paras(active.text);
        let cell = CapturedTableCell {
            order: active.order,
            table: active.table,
            x: active.x,
            y: active.y,
            colspan: active.colspan,
            rowspan: active.rowspan,
            rect: active.rect,
            fill: active.fill,
            borders: active.borders,
            paras,
        };

        if let Some(table) = self.active_tables.last_mut() {
            table.cells.push(cell);
        } else {
            self.loose_table_cells.push(cell);
        }
        true
    }

    fn capture_table_item(
        &mut self,
        order: usize,
        item: &'a FrameItem,
        item_transform: Transform,
    ) -> bool {
        if self.active_table_cells.is_empty() {
            return false;
        }

        match item {
            FrameItem::Group(group) => {
                let group_transform = item_transform.pre_concat(group.transform);
                let clip_ok = match &group.clip {
                    None => true,
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
                        "table-cell-group",
                        if group.clip.is_some() { "clip" } else { "transform" },
                        frame_text_chars(&group.frame),
                    );
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
                    self.active_table_cells.last_mut().unwrap().text.push(TextSource {
                        order,
                        baseline,
                        item: text,
                        rot_60k: similarity.rot_60k,
                        scale: similarity.scale,
                        link: None,
                        slide_number: false,
                    });
                } else {
                    debug_raster(
                        "table-cell-text",
                        "transform",
                        text.text.chars().count(),
                    );
                    self.raster_item(
                        order,
                        FrameItem::Text(text.clone()),
                        item_transform,
                        Some(text.text.clone()),
                    );
                }
            }
            FrameItem::Link(dest, size) => {
                if let Some(target) = self.destination(dest) {
                    self.active_table_cells.last_mut().unwrap().links.push(LinkRect {
                        rect: transformed_rect(item_transform, *size),
                        target,
                    });
                }
            }
            FrameItem::Shape(shape, span) => {
                match crate::shape::shape_to_geom(shape, item_transform, 0) {
                    Some(geom) => self
                        .shapes
                        .push(OrderedShape { order, shape: SlideShape::Geom(geom) }),
                    None => {
                        debug_raster("table-cell-shape", "unmappable", 0);
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
            FrameItem::Tag(_) => {}
        }

        true
    }

    fn emit_loose_tables(&mut self) {
        let cells = std::mem::take(&mut self.loose_table_cells);
        for (order, cells) in split_table_groups(cells) {
            if let Some(shape) = table_shape(order, cells) {
                self.shapes.push(shape);
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
                .unwrap_or_else(|| elem.body.plain_text());
            Some((loc, MathSource { omml, fallback, block }))
        })
        .collect()
}

fn table_cell_paras(text: Vec<TextSource<'_>>) -> Vec<TextPara> {
    // Cell-local inline-math detection isn't wired up yet (equations inside
    // a table cell aren't captured by the cell walker); this keeps native
    // tables compiling and correct for the common text-only case rather than
    // blocking on that follow-up.
    crate::text::cluster_text(text, Vec::new())
        .into_iter()
        .flat_map(|cluster| match cluster.shape {
            SlideShape::TextBox(text) => text.paras,
            _ => Vec::new(),
        })
        .collect()
}

fn split_table_groups(
    mut cells: Vec<CapturedTableCell>,
) -> Vec<(usize, Vec<CapturedTableCell>)> {
    cells.sort_by_key(|cell| cell.order);
    let mut groups: Vec<(usize, Vec<CapturedTableCell>)> = Vec::new();
    for cell in cells {
        let starts_new = cell.x == 0
            && cell.y == 0
            && groups.last().is_some_and(|(_, group)| !group.is_empty());
        if starts_new || groups.is_empty() {
            groups.push((cell.order, Vec::new()));
        }
        groups.last_mut().unwrap().1.push(cell);
    }
    groups
}

fn table_shape(order: usize, mut cells: Vec<CapturedTableCell>) -> Option<OrderedShape> {
    if cells.is_empty() {
        return None;
    }
    cells.sort_by_key(|cell| (cell.y, cell.x, cell.order));
    let _has_semantic_table_cells = cells.iter().any(|cell| cell.table);

    let cols = cells.iter().map(|cell| cell.x + cell.colspan).max()?;
    let rows = cells.iter().map(|cell| cell.y + cell.rowspan).max()?;
    if cols == 0 || rows == 0 {
        return None;
    }

    let mut min = Point::splat(Abs::inf());
    let mut max = Point::splat(-Abs::inf());
    for cell in &cells {
        min = min.min(cell.rect.min);
        max = max.max(cell.rect.max);
    }
    let total =
        Size::new((max.x - min.x).max(Abs::pt(0.1)), (max.y - min.y).max(Abs::pt(0.1)));
    let col_widths = track_widths(&cells, cols, true, total.x);
    let row_heights = track_widths(&cells, rows, false, total.y);

    let mut table_rows = Vec::with_capacity(rows);
    for (y, row_height) in row_heights.iter().enumerate().take(rows) {
        let mut row_cells = Vec::with_capacity(cols);
        for x in 0..cols {
            if let Some(cell) = cells.iter().find(|cell| cell.x == x && cell.y == y) {
                row_cells.push(TableCell {
                    grid_span: cell.colspan.max(1),
                    row_span: cell.rowspan.max(1),
                    h_merge: false,
                    v_merge: false,
                    fill: cell.fill.clone(),
                    borders: cell.borders.clone(),
                    paras: cell.paras.clone(),
                });
            } else if let Some(origin) = covering_cell(&cells, x, y) {
                row_cells.push(TableCell {
                    grid_span: 1,
                    row_span: 1,
                    h_merge: x > origin.x,
                    v_merge: y > origin.y,
                    fill: None,
                    borders: CellBorders::default(),
                    paras: Vec::new(),
                });
            } else {
                row_cells.push(TableCell {
                    grid_span: 1,
                    row_span: 1,
                    h_merge: false,
                    v_merge: false,
                    fill: None,
                    borders: CellBorders::default(),
                    paras: Vec::new(),
                });
            }
        }
        table_rows.push(TableRow {
            h_emu: crate::text::extent_emu(*row_height),
            cells: row_cells,
        });
    }

    Some(OrderedShape {
        order,
        shape: SlideShape::TableBox(TableBox {
            x_emu: crate::text::emu(min.x),
            y_emu: crate::text::emu(min.y),
            w_emu: crate::text::extent_emu(total.x),
            h_emu: crate::text::extent_emu(total.y),
            cols: col_widths.into_iter().map(crate::text::extent_emu).collect(),
            rows: table_rows,
        }),
    })
}

fn covering_cell(
    cells: &[CapturedTableCell],
    x: usize,
    y: usize,
) -> Option<&CapturedTableCell> {
    cells.iter().find(|cell| {
        cell.x <= x
            && x < cell.x + cell.colspan
            && cell.y <= y
            && y < cell.y + cell.rowspan
    })
}

fn track_widths(
    cells: &[CapturedTableCell],
    count: usize,
    columns: bool,
    total: Abs,
) -> Vec<Abs> {
    let mut tracks = vec![None; count];
    for cell in cells {
        let (index, span, size) = if columns {
            (cell.x, cell.colspan, cell.rect.size().x)
        } else {
            (cell.y, cell.rowspan, cell.rect.size().y)
        };
        if span == 1
            && index < count
            && tracks[index].is_none_or(|current| size > current)
        {
            tracks[index] = Some(size);
        }
    }

    for cell in cells {
        let (index, span, size) = if columns {
            (cell.x, cell.colspan, cell.rect.size().x)
        } else {
            (cell.y, cell.rowspan, cell.rect.size().y)
        };
        if span <= 1 || index + span > count {
            continue;
        }
        let mut known = Abs::zero();
        let mut missing = Vec::new();
        for offset in 0..span {
            match tracks[index + offset] {
                Some(width) => known += width,
                None => missing.push(index + offset),
            }
        }
        if !missing.is_empty() {
            let share = (size - known).max(Abs::pt(1.0)) / missing.len() as f64;
            for index in missing {
                tracks[index] = Some(share);
            }
        }
    }

    let known: Abs = tracks.iter().filter_map(|width| *width).sum();
    let missing = tracks.iter().filter(|width| width.is_none()).count();
    let fallback = if missing == 0 {
        Abs::pt(1.0)
    } else {
        (total - known).max(Abs::pt(missing as f64)) / missing as f64
    };
    tracks
        .into_iter()
        .map(|width| width.unwrap_or(fallback).max(Abs::pt(0.1)))
        .collect()
}

fn region_fill(
    body: &typst_library::foundations::Content,
    styles: StyleChain,
) -> Option<FillSpec> {
    let fill = body
        .to_packed::<TypstTableCell>()
        .and_then(|cell| smart_fill(cell.fill.get_cloned(styles)))
        .or_else(|| {
            body.to_packed::<GridCell>()
                .and_then(|cell| smart_fill(cell.fill.get_cloned(styles)))
        })?;
    crate::shape::resolved_fill(&Some(fill)).flatten()
}

fn smart_fill(fill: Smart<Option<Paint>>) -> Option<Paint> {
    match fill {
        Smart::Custom(fill) => fill,
        Smart::Auto => None,
    }
}

fn region_borders(
    body: &typst_library::foundations::Content,
    styles: StyleChain,
) -> CellBorders {
    if let Some(cell) = body.to_packed::<TypstTableCell>() {
        return borders_from_sides(cell.stroke.resolve(styles));
    }
    if let Some(cell) = body.to_packed::<GridCell>() {
        return borders_from_sides(cell.stroke.resolve(styles));
    }
    CellBorders::default()
}

fn borders_from_sides(
    sides: Sides<Option<Option<std::sync::Arc<Stroke<Abs>>>>>,
) -> CellBorders {
    CellBorders {
        left: sides
            .left
            .as_ref()
            .and_then(|side| side.as_deref())
            .and_then(stroke_spec),
        right: sides
            .right
            .as_ref()
            .and_then(|side| side.as_deref())
            .and_then(stroke_spec),
        top: sides
            .top
            .as_ref()
            .and_then(|side| side.as_deref())
            .and_then(stroke_spec),
        bottom: sides
            .bottom
            .as_ref()
            .and_then(|side| side.as_deref())
            .and_then(stroke_spec),
    }
}

fn stroke_spec(stroke: &Stroke<Abs>) -> Option<StrokeSpec> {
    let fixed = stroke.clone().unwrap_or_default();
    let Paint::Solid(color) = fixed.paint else {
        return None;
    };
    Some(StrokeSpec {
        color: crate::shape::srgb_bytes(&color),
        w_emu: crate::text::extent_emu(fixed.thickness),
        cap: line_cap(fixed.cap),
        dash: None,
    })
}

fn line_cap(cap: LineCap) -> &'static str {
    match cap {
        LineCap::Butt => "flat",
        LineCap::Round => "rnd",
        LineCap::Square => "sq",
    }
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

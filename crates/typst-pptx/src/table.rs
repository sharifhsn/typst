//! Native PowerPoint table capture and reconstruction.
//!
//! The slide walker owns traversal and painter order. This module consumes the
//! hidden table/grid cell-region tags emitted during layout, captures the frame
//! items inside each cell, and rebuilds editable DrawingML table rows and cells.

use typst_library::foundations::{Smart, StyleChain};
use typst_library::introspection::{Location, Tag};
use typst_library::layout::{
    Abs, Alignment, FrameItem, GridCell, GridCellRegion, GridElem, HAlignment, Point,
    Sides, Size, Transform, VAlignment,
};
use typst_library::model::{TableCell as TypstTableCell, TableElem};
use typst_library::visualize::{LineCap, Paint, Stroke};

use crate::dom::{
    CellBorders, CellHAlign, CellVAlign, FillSpec, SlideShape, StrokeSpec, TableBox,
    TableCell, TableRow, TextPara,
};
use crate::slide::{
    HighlightCandidate, LinkRect, OrderedShape, Rect, Walker, attach_highlights,
    classify_similarity, debug_raster, frame_text_chars, highlight_candidate,
    text_link_overlays, transformed_rect,
};
use crate::text::{InlineMathSource, TextSource};

pub(super) struct ActiveTable {
    loc: Location,
    order: usize,
    cells: Vec<CapturedTableCell>,
}

pub(super) struct ActiveTableCell<'a> {
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
    h_align: Option<CellHAlign>,
    v_align: Option<CellVAlign>,
    text: Vec<TextSource<'a>>,
    pub(super) math: Vec<InlineMathSource>,
    links: Vec<LinkRect>,
    highlights: Vec<HighlightCandidate>,
}

pub(super) struct CapturedTableCell {
    order: usize,
    table: bool,
    x: usize,
    y: usize,
    colspan: usize,
    rowspan: usize,
    rect: Rect,
    fill: Option<FillSpec>,
    borders: CellBorders,
    h_align: Option<CellHAlign>,
    v_align: Option<CellVAlign>,
    paras: Vec<TextPara>,
}

impl<'a, 'b> Walker<'a, 'b> {
    pub(super) fn start_table(&mut self, tag: &Tag, order: usize) -> bool {
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

    pub(super) fn end_table(&mut self, loc: Location) -> bool {
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

    pub(super) fn start_table_cell(
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
        let fill = region_fill(self.ctx, &region.body, styles);
        let borders = region_borders(&region.body, styles);
        let (h_align, v_align) = region_alignment(&region.body, styles);
        self.active_table_cells.push(ActiveTableCell {
            loc: tag.location(),
            order,
            table: region.body.is::<TypstTableCell>(),
            x: region.x,
            y: region.y,
            colspan: region.colspan.get(),
            rowspan: region.rowspan.get(),
            rect: Rect { min: origin, max: origin + size.to_point() },
            fill,
            borders,
            h_align,
            v_align,
            text: Vec::new(),
            math: Vec::new(),
            links: Vec::new(),
            highlights: Vec::new(),
        });
        true
    }

    pub(super) fn end_table_cell(&mut self, loc: Location) -> bool {
        let Some(index) =
            self.active_table_cells.iter().rposition(|active| active.loc == loc)
        else {
            return false;
        };
        let mut active = self.active_table_cells.remove(index);
        attach_highlights(&mut active.text, &mut self.shapes, &active.highlights);
        self.link_overlays
            .extend(text_link_overlays(&active.text, &active.links));
        let paras = table_cell_paras(active.text, active.math);
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
            h_align: active.h_align,
            v_align: active.v_align,
            paras,
        };

        if let Some(table) = self.active_tables.last_mut() {
            table.cells.push(cell);
        } else {
            self.loose_table_cells.push(cell);
        }
        true
    }

    pub(super) fn capture_table_item(
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
                        highlight: None,
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
                let highlight = highlight_candidate(shape, *span, item_transform, order);
                match crate::shape::shape_to_geom(self.ctx, shape, item_transform, 0) {
                    Some(geom) => {
                        self.shapes
                            .push(OrderedShape { order, shape: SlideShape::Geom(geom) });
                        if let Some(highlight) = highlight {
                            self.active_table_cells
                                .last_mut()
                                .unwrap()
                                .highlights
                                .push(highlight);
                        }
                    }
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

    pub(super) fn emit_loose_tables(&mut self) {
        let cells = std::mem::take(&mut self.loose_table_cells);
        for (order, cells) in split_table_groups(cells) {
            if let Some(shape) = table_shape(order, cells) {
                self.shapes.push(shape);
            }
        }
    }
}

fn table_cell_paras(
    text: Vec<TextSource<'_>>,
    math: Vec<InlineMathSource>,
) -> Vec<TextPara> {
    crate::text::cluster_text(text, math)
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
                    h_align: cell.h_align,
                    v_align: cell.v_align,
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
                    h_align: None,
                    v_align: None,
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
                    h_align: None,
                    v_align: None,
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
    ctx: &mut crate::dom::SlideCtx,
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
    crate::shape::resolved_fill(ctx, &Some(fill)).flatten()
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

fn region_alignment(
    body: &typst_library::foundations::Content,
    styles: StyleChain,
) -> (Option<CellHAlign>, Option<CellVAlign>) {
    let align = if let Some(cell) = body.to_packed::<TypstTableCell>() {
        cell.align.get(styles)
    } else if let Some(cell) = body.to_packed::<GridCell>() {
        cell.align.get(styles)
    } else {
        return (None, None);
    };
    let Smart::Custom(align) = align else {
        return (None, None);
    };
    alignment_parts(align)
}

fn alignment_parts(align: Alignment) -> (Option<CellHAlign>, Option<CellVAlign>) {
    let horizontal = align.x().map(|align| match align {
        HAlignment::Start => CellHAlign::Start,
        HAlignment::Left => CellHAlign::Left,
        HAlignment::Center => CellHAlign::Center,
        HAlignment::Right => CellHAlign::Right,
        HAlignment::End => CellHAlign::End,
    });
    let vertical = align.y().map(|align| match align {
        VAlignment::Top => CellVAlign::Top,
        VAlignment::Horizon => CellVAlign::Center,
        VAlignment::Bottom => CellVAlign::Bottom,
    });
    (horizontal, vertical)
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

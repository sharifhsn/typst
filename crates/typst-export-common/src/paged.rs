//! Owned geometry facts extracted from a converged paged document.
//!
//! Layout emits hidden, non-introspectable tags for physical regions that
//! exporters need. This scanner turns those frame-local tags into a stable,
//! arena-free sidecar before a target-specific realization starts.

use typst_layout::PagedDocument;
use typst_library::foundations::Content;
use typst_library::introspection::{Location, Tag};
use typst_library::layout::GridElem;
use typst_library::layout::{Frame, FrameItem, GridCellRegion, Point, Transform};
use typst_library::model::TableElem;

/// Stable identity shared by independent realizations of source-backed content.
pub fn logical_id(content: &Content) -> u128 {
    let span = content.span();
    let element = content.elem().name();
    if span.is_detached() {
        typst_utils::hash128(&(span, content.location(), element))
    } else {
        typst_utils::hash128(&(span, element))
    }
}

/// Geometry recovered from all converged page frames.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PagedGeometry {
    tables: Vec<PagedTableGeometry>,
}

impl PagedGeometry {
    pub fn from_document(document: &PagedDocument) -> Self {
        let mut scanner = Scanner::default();
        for (index, page) in document.pages().iter().enumerate() {
            scanner.page = index + 1;
            scanner.walk_frame(&page.frame, Transform::identity());
        }
        Self { tables: scanner.tables }
    }

    pub fn tables(&self) -> &[PagedTableGeometry] {
        &self.tables
    }

    pub fn first_table(&self, logical_id: u128) -> Option<&PagedTableGeometry> {
        self.tables.iter().find(|table| table.logical_id == logical_id)
    }
}

/// One physical occurrence of a semantic table or grid.
#[derive(Debug, Clone, PartialEq)]
pub struct PagedTableGeometry {
    pub logical_id: u128,
    pub page: usize,
    pub cells: Vec<PagedCellGeometry>,
}

/// One final physical cell region in points.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct PagedCellGeometry {
    pub page: usize,
    pub x: usize,
    pub y: usize,
    pub colspan: usize,
    pub rowspan: usize,
    pub left_pt: f64,
    pub top_pt: f64,
    pub width_pt: f64,
    pub height_pt: f64,
    /// False for rotation, skew, mirroring, or a degenerate transform.
    pub axis_aligned: bool,
}

#[derive(Default)]
struct Scanner {
    page: usize,
    tables: Vec<PagedTableGeometry>,
    active_tables: Vec<ActiveTable>,
}

struct ActiveTable {
    location: Location,
    table_index: usize,
}

impl Scanner {
    fn walk_frame(&mut self, frame: &Frame, transform: Transform) {
        for (position, item) in frame.items() {
            let item_transform =
                transform.pre_concat(Transform::translate(position.x, position.y));
            match item {
                FrameItem::Group(group) => {
                    self.walk_frame(
                        &group.frame,
                        item_transform.pre_concat(group.transform),
                    );
                }
                FrameItem::Tag(tag) => self.handle_tag(tag, item_transform),
                FrameItem::Text(_)
                | FrameItem::Shape(_, _)
                | FrameItem::Image(_, _, _)
                | FrameItem::Link(_, _) => {}
            }
        }
    }

    fn handle_tag(&mut self, tag: &Tag, transform: Transform) {
        match tag {
            Tag::Start(content, ..) => {
                if let Some(region) = content.to_packed::<GridCellRegion>() {
                    self.record_cell(region, transform);
                    return;
                }
                if content.to_packed::<TableElem>().is_some()
                    || content.to_packed::<GridElem>().is_some()
                {
                    let table_index = self.tables.len();
                    self.tables.push(PagedTableGeometry {
                        logical_id: logical_id(content),
                        page: self.page,
                        cells: Vec::new(),
                    });
                    self.active_tables
                        .push(ActiveTable { location: tag.location(), table_index });
                }
            }
            Tag::End(location, ..) => {
                if let Some(index) = self
                    .active_tables
                    .iter()
                    .rposition(|active| active.location == *location)
                {
                    self.active_tables.remove(index);
                }
            }
        }
    }

    fn record_cell(&mut self, region: &GridCellRegion, transform: Transform) {
        let Some(active) = self.active_tables.last() else { return };
        let origin = Point::zero().transform(transform);
        let axis_aligned = transform.kx.is_zero()
            && transform.ky.is_zero()
            && transform.sx.get() > 0.0
            && transform.sy.get() > 0.0;
        let width_pt = region.width.to_pt() * transform.sx.get().abs();
        let height_pt = region.height.to_pt() * transform.sy.get().abs();
        self.tables[active.table_index].cells.push(PagedCellGeometry {
            page: self.page,
            x: region.x,
            y: region.y,
            colspan: region.colspan.get(),
            rowspan: region.rowspan.get(),
            left_pt: origin.x.to_pt(),
            top_pt: origin.y.to_pt(),
            width_pt,
            height_pt,
            axis_aligned,
        });
    }
}

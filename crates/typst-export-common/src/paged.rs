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
#[derive(Debug, Clone, Default)]
pub struct PagedGeometry {
    tables: Vec<PagedTableGeometry>,
    dense_visual_page: Option<Frame>,
}

// The retained frame is an exporter cache, not part of the recovered semantic
// table geometry. Keep the equality contract that callers had before the cache
// was introduced.
impl PartialEq for PagedGeometry {
    fn eq(&self, other: &Self) -> bool {
        self.tables == other.tables
    }
}

impl PagedGeometry {
    pub fn from_document(document: &PagedDocument) -> Self {
        let mut scanner = Scanner::default();
        for (index, page) in document.pages().iter().enumerate() {
            scanner.page = index + 1;
            scanner.walk_frame(&page.frame, Transform::identity());
        }
        // Word and LibreOffice become pathologically slow when a visual-only
        // page is emitted as roughly a thousand independent DrawingML shapes.
        // Retain the converged frame only for that narrow one-page case so the
        // DOCX exporter can replace the consumer-hostile shape swarm with one
        // exact page raster. Ordinary documents keep no duplicate page frame.
        let dense_visual_page = document
            .pages()
            .first()
            .filter(|_| document.pages().len() == 1)
            .filter(|page| dense_visual_only(&page.frame))
            .map(|page| page.frame.clone());
        Self { tables: scanner.tables, dense_visual_page }
    }

    pub fn tables(&self) -> &[PagedTableGeometry] {
        &self.tables
    }

    pub fn first_table(&self, logical_id: u128) -> Option<&PagedTableGeometry> {
        self.tables.iter().find(|table| table.logical_id == logical_id)
    }

    /// The occurrence matching a specific element instance. `logical_id` is
    /// span-based, so it is shared not only by a table's own header row
    /// repeating across a page break, but also by every call to a *reusable*
    /// grid/table-producing function (the same `grid(..)` call site invoked
    /// once per section of a CV, once per code listing, etc.) — genuinely
    /// different content that happens to originate from one source line.
    /// `location` (an introspection identity assigned per realized element
    /// instance, not per source span) disambiguates the two: true repeats of
    /// one call site share it, distinct calls to a shared function do not.
    /// Falls back to the plain span match when `location` is unavailable —
    /// synthetic content and cases where the scan tag carried none — which
    /// preserves the old (occasionally wrong, but no worse than before this
    /// distinction existed) first-match behavior rather than losing geometry
    /// entirely.
    pub fn table_for(
        &self,
        logical_id: u128,
        location: Option<Location>,
    ) -> Option<&PagedTableGeometry> {
        if let Some(location) = location
            && let Some(table) = self.tables.iter().find(|table| {
                table.logical_id == logical_id && table.location == Some(location)
            })
        {
            return Some(table);
        }
        self.first_table(logical_id)
    }

    /// A one-page, text-free frame whose native shape count exceeds the
    /// consumer-safety budget.
    pub fn dense_visual_page(&self) -> Option<&Frame> {
        self.dense_visual_page.as_ref()
    }
}

fn dense_visual_only(frame: &Frame) -> bool {
    const SHAPE_BUDGET: usize = 900;

    fn scan(frame: &Frame, shapes: &mut usize, has_rich_content: &mut bool) {
        for (_, item) in frame.items() {
            match item {
                FrameItem::Group(group) => scan(&group.frame, shapes, has_rich_content),
                FrameItem::Shape(..) => *shapes += 1,
                FrameItem::Text(..) | FrameItem::Image(..) | FrameItem::Link(..) => {
                    *has_rich_content = true
                }
                FrameItem::Tag(..) => {}
            }
        }
    }

    let mut shapes = 0;
    let mut has_rich_content = false;
    scan(frame, &mut shapes, &mut has_rich_content);
    !has_rich_content && shapes > SHAPE_BUDGET
}

/// One physical occurrence of a semantic table or grid.
#[derive(Debug, Clone, PartialEq)]
pub struct PagedTableGeometry {
    pub logical_id: u128,
    /// The introspection location of the specific element instance this
    /// occurrence came from, when the layout tag carried one. Distinguishes
    /// genuinely different call sites that happen to share `logical_id` (see
    /// [`PagedGeometry::table_for`]) from true repeats of one call site
    /// (a table's own header row repeated across a page break), which share
    /// both `logical_id` *and* `location`.
    pub location: Option<Location>,
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
                        location: Some(tag.location()),
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
        let cells = &mut self.tables[active.table_index].cells;
        // Some constructs (e.g. the bibliography's paged-only two-column
        // rendering, built via `BlockElem::multi_layouter` specifically to
        // avoid generating its own introspection tag — see
        // `BIBLIOGRAPHY_RULE` in typst-layout) lay out a grid without ever
        // opening a `TableElem`/`GridElem` scope of their own. Its cells'
        // region tags still fire (`tag_cell_region` runs unconditionally per
        // cell), so with no scope of their own they land on whatever real
        // table happens to be open around them — a poster section's `[..]`
        // grid cell containing a `#bibliography(..)`, say. A well-formed
        // table only ever places one cell origin per (x, y) in a single
        // frame walk, so a second claim to an already-recorded origin is
        // exactly that kind of orphaned tag, not a legitimate resize/retry:
        // drop it rather than let it corrupt the real cell's column/row
        // median with a foreign grid's unrelated dimensions.
        if cells.iter().any(|cell| cell.x == region.x && cell.y == region.y) {
            return;
        }
        let origin = Point::zero().transform(transform);
        let axis_aligned = transform.kx.is_zero()
            && transform.ky.is_zero()
            && transform.sx.get() > 0.0
            && transform.sy.get() > 0.0;
        let width_pt = region.width.to_pt() * transform.sx.get().abs();
        let height_pt = region.height.to_pt() * transform.sy.get().abs();
        cells.push(PagedCellGeometry {
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

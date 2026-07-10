//! Stable, owned semantic/paged sidecar captured before DOCX lowering.

use typst_export_common::paged::{PagedGeometry, PagedTableGeometry};
use typst_layout::PagedIntrospector;
use typst_library::foundations::Content;
use typst_library::introspection::Introspector;
use typst_library::layout::Size;
use typst_library::routines::Pair;

use crate::report::ExportSource;

/// One converged paged position associated with a semantic source node.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct SnapshotPosition {
    pub page: usize,
    pub x_pt: f64,
    pub y_pt: f64,
}

/// One page in the converged paged oracle.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct SnapshotPage {
    pub width_pt: f64,
    pub height_pt: f64,
}

/// One owned semantic node and every paged occurrence matched by source span.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotNode {
    pub source: ExportSource,
    pub semantic_occurrences: usize,
    pub paged_positions: Vec<SnapshotPosition>,
}

/// Stable bridge between the DOCX semantic realization and converged paged
/// geometry. This deliberately owns only identities and resolved facts, never
/// arena-backed `Content` or `StyleChain` values.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSnapshot {
    logical_id: u128,
    nodes: Vec<SnapshotNode>,
    pages: Vec<SnapshotPage>,
    tables: Vec<PagedTableGeometry>,
}

impl ExportSnapshot {
    pub fn logical_id(&self) -> u128 {
        self.logical_id
    }

    pub fn nodes(&self) -> &[SnapshotNode] {
        &self.nodes
    }

    pub fn pages(&self) -> &[SnapshotPage] {
        &self.pages
    }

    /// Final physical table/grid cell regions recovered from paged frames.
    pub fn tables(&self) -> &[PagedTableGeometry] {
        &self.tables
    }

    pub(crate) fn build(
        pairs: &[Pair<'_>],
        paged: Option<&PagedIntrospector>,
        page_sizes: Option<&[Size]>,
        paged_geometry: Option<&PagedGeometry>,
    ) -> Self {
        let pages = page_sizes
            .unwrap_or_default()
            .iter()
            .map(|size| SnapshotPage {
                width_pt: size.x.to_pt(),
                height_pt: size.y.to_pt(),
            })
            .collect::<Vec<_>>();

        let mut nodes = Vec::<SnapshotNode>::new();
        for (content, _) in pairs {
            use std::ops::ControlFlow;

            // Top-level unlocated nodes still define lowering regions. Nested
            // nodes join the sidecar when Typst assigned them a semantic
            // location (headings, figures, links, counters, notes, etc.).
            if content.location().is_none() {
                push_node(&mut nodes, content, paged);
            }
            let _ = content.traverse(&mut |element: Content| {
                if element.location().is_some() {
                    push_node(&mut nodes, &element, paged);
                }
                ControlFlow::<()>::Continue(())
            });
        }

        let identity_nodes = nodes
            .iter()
            .map(|node| (node.source.logical_id, node.semantic_occurrences))
            .collect::<Vec<_>>();
        let identity_pages = pages
            .iter()
            .map(|page| (page.width_pt.to_bits(), page.height_pt.to_bits()))
            .collect::<Vec<_>>();
        let tables = paged_geometry
            .map(|geometry| geometry.tables().to_vec())
            .unwrap_or_default();
        let identity_tables = tables
            .iter()
            .map(|table| {
                (
                    table.logical_id,
                    table.page,
                    table
                        .cells
                        .iter()
                        .map(|cell| {
                            (
                                cell.page,
                                cell.x,
                                cell.y,
                                cell.colspan,
                                cell.rowspan,
                                cell.left_pt.to_bits(),
                                cell.top_pt.to_bits(),
                                cell.width_pt.to_bits(),
                                cell.height_pt.to_bits(),
                                cell.axis_aligned,
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let logical_id =
            typst_utils::hash128(&(identity_nodes, identity_pages, identity_tables));
        Self { logical_id, nodes, pages, tables }
    }
}

fn push_node(
    nodes: &mut Vec<SnapshotNode>,
    content: &Content,
    paged: Option<&PagedIntrospector>,
) {
    let source = ExportSource::from_content(content);
    let paged_positions = matched_positions(content, paged);
    if let Some(existing) = nodes
        .iter_mut()
        .find(|node| node.source.logical_id == source.logical_id)
    {
        existing.semantic_occurrences += 1;
        existing.paged_positions.extend(paged_positions);
        normalize_positions(&mut existing.paged_positions);
    } else {
        nodes.push(SnapshotNode { source, semantic_occurrences: 1, paged_positions });
    }
}

fn matched_positions(
    content: &Content,
    paged: Option<&PagedIntrospector>,
) -> Vec<SnapshotPosition> {
    let Some(paged) = paged else { return Vec::new() };
    let mut positions = Vec::new();

    // Locations are target-realization-specific, so span + element identity is
    // the primary bridge. This also captures repeated running furniture: one
    // semantic node can own several converged paged positions.
    for candidate in paged.query(&content.elem().select()) {
        if candidate.span() != content.span() {
            continue;
        }
        let Some(location) = candidate.location() else { continue };
        let Some(position) = paged.position(location) else { continue };
        positions.push(SnapshotPosition {
            page: position.page.get(),
            x_pt: position.point.x.to_pt(),
            y_pt: position.point.y.to_pt(),
        });
    }
    normalize_positions(&mut positions);
    positions
}

fn normalize_positions(positions: &mut Vec<SnapshotPosition>) {
    positions.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then_with(|| a.y_pt.total_cmp(&b.y_pt))
            .then_with(|| a.x_pt.total_cmp(&b.x_pt))
    });
    positions.dedup_by(|a, b| {
        a.page == b.page
            && a.x_pt.to_bits() == b.x_pt.to_bits()
            && a.y_pt.to_bits() == b.y_pt.to_bits()
    });
}

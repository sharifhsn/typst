//! Stable, owned semantic/paged sidecar captured before DOCX lowering.

use comemo::Track;
use ecow::EcoString;
use typst_export_common::paged::{PagedGeometry, PagedTableGeometry};
use typst_layout::PagedIntrospector;
use typst_library::engine::Engine;
use typst_library::foundations::StyleChain;
use typst_library::foundations::{Content, Selector};
use typst_library::introspection::{Counter, CounterKey, Introspector};
use typst_library::layout::Size;
use typst_library::routines::Pair;
use typst_library::{
    foundations::Label,
    model::{BibliographyElem, Destination, LinkElem},
};

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
    pub page_counters: Vec<SnapshotPageCounter>,
}

/// Resolved page-counter value at one semantic node occurrence.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotPageCounter {
    pub page: usize,
    pub display: EcoString,
}

/// One bibliography entry selected by the converged paged document.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotBibliographyEntry {
    pub logical_id: u128,
    pub key: EcoString,
    pub(crate) label: Label,
    pub(crate) entry: hayagriva::Entry,
}

/// Resolved target of one semantic link in the paged reference document.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotLinkTarget {
    Url(EcoString),
    Node(u128),
    Position { page: usize, x_pt: f64, y_pt: f64 },
}

/// One stable semantic link edge.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotLink {
    pub logical_id: u128,
    pub source_id: u128,
    pub target: SnapshotLinkTarget,
    pub occurrences: usize,
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
    links: Vec<SnapshotLink>,
    bibliography_biblatex: Option<String>,
    bibliography_entries: Vec<SnapshotBibliographyEntry>,
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

    pub fn links(&self) -> &[SnapshotLink] {
        &self.links
    }

    /// Lossless bibliography source payload resolved by the paged reference
    /// document before target-specific lowering begins.
    pub fn bibliography_biblatex(&self) -> Option<&str> {
        self.bibliography_biblatex.as_deref()
    }

    /// Citation keys represented by the paged document's visible bibliography.
    pub fn bibliography_entries(&self) -> &[SnapshotBibliographyEntry] {
        &self.bibliography_entries
    }

    pub(crate) fn bibliography_source_entries(&self) -> Vec<(Label, hayagriva::Entry)> {
        self.bibliography_entries
            .iter()
            .map(|entry| (entry.label, entry.entry.clone()))
            .collect()
    }

    pub(crate) fn build(
        engine: &mut Engine,
        styles: StyleChain,
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
                push_node(&mut nodes, content, paged, engine, styles);
            }
            let _ = content.traverse(&mut |element: Content| {
                if element.location().is_some() {
                    push_node(&mut nodes, &element, paged, engine, styles);
                }
                ControlFlow::<()>::Continue(())
            });
        }

        let links = collect_links(pairs, paged);
        let identity_nodes = nodes
            .iter()
            .map(|node| {
                (
                    node.source.logical_id,
                    node.semantic_occurrences,
                    node.page_counters
                        .iter()
                        .map(|counter| (counter.page, counter.display.as_str()))
                        .collect::<Vec<_>>(),
                )
            })
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
        let (bibliography_biblatex, bibliography_entries) = paged.map_or_else(
            || (None, Vec::new()),
            |paged| {
                let biblatex =
                    BibliographyElem::biblatex((paged as &dyn Introspector).track());
                let mut entries =
                    BibliographyElem::entries((paged as &dyn Introspector).track());
                entries.sort_by_key(|(label, _)| label.resolve().as_str().to_owned());
                let entries = entries
                    .into_iter()
                    .map(|(label, entry)| {
                        let key = EcoString::from(label.resolve().as_str());
                        let logical_id = typst_utils::hash128(&(key.as_str(), &entry));
                        SnapshotBibliographyEntry { logical_id, key, label, entry }
                    })
                    .collect();
                (biblatex, entries)
            },
        );
        let bibliography_entry_ids = bibliography_entries
            .iter()
            .map(|entry| entry.logical_id)
            .collect::<Vec<_>>();
        let identity_links = links
            .iter()
            .map(|link| (link.logical_id, link.occurrences))
            .collect::<Vec<_>>();
        let logical_id = typst_utils::hash128(&(
            identity_nodes,
            identity_pages,
            identity_tables,
            identity_links,
            &bibliography_biblatex,
            bibliography_entry_ids,
        ));
        Self {
            logical_id,
            nodes,
            pages,
            tables,
            links,
            bibliography_biblatex,
            bibliography_entries,
        }
    }

    pub(crate) fn page_counter_for_location(
        &self,
        location: typst_library::introspection::Location,
    ) -> Option<EcoString> {
        let node = self
            .nodes
            .iter()
            .find(|node| node.source.location == Some(location))?;

        // A source location can be realized more than once by the paged
        // oracle. Prefer the counter for the first concrete occurrence rather
        // than blindly selecting the page-1 entry for every TOC item.
        let page = node.paged_positions.first().map(|position| position.page);
        let counter = node
            .page_counters
            .iter()
            .find(|counter| Some(counter.page) == page)
            .or_else(|| node.page_counters.first());
        match (page, counter) {
            (_, Some(counter)) => Some(counter.display.clone()),
            // The default Typst page numbering has no explicit numbering
            // pattern, so there is no `page_counters` entry. The paged oracle
            // still records the physical page and that is the exact cache
            // value Word should show before refreshing fields.
            (Some(page), None) => Some(page.to_string().into()),
            _ => None,
        }
    }
}

fn collect_links(
    pairs: &[Pair<'_>],
    paged: Option<&PagedIntrospector>,
) -> Vec<SnapshotLink> {
    use std::ops::ControlFlow;

    let Some(paged) = paged else { return Vec::new() };
    let mut links = Vec::<SnapshotLink>::new();
    for (content, _) in pairs {
        let _ = content.traverse(&mut |element: Content| {
            let Some(link) = element.to_packed::<LinkElem>() else {
                return ControlFlow::<()>::Continue(());
            };
            let Ok(destination) = link.dest.resolve_late(paged) else {
                return ControlFlow::<()>::Continue(());
            };
            let source_id = ExportSource::from_content(&element).logical_id;
            let Some(target) = snapshot_link_target(destination, paged) else {
                return ControlFlow::<()>::Continue(());
            };
            let logical_id = match &target {
                SnapshotLinkTarget::Url(url) => {
                    typst_utils::hash128(&(source_id, "url", url))
                }
                SnapshotLinkTarget::Node(target) => {
                    typst_utils::hash128(&(source_id, "node", target))
                }
                SnapshotLinkTarget::Position { page, x_pt, y_pt } => {
                    typst_utils::hash128(&(
                        source_id,
                        "position",
                        page,
                        x_pt.to_bits(),
                        y_pt.to_bits(),
                    ))
                }
            };
            if let Some(existing) =
                links.iter_mut().find(|existing| existing.logical_id == logical_id)
            {
                existing.occurrences += 1;
            } else {
                links.push(SnapshotLink {
                    logical_id,
                    source_id,
                    target,
                    occurrences: 1,
                });
            }
            ControlFlow::Continue(())
        });
    }
    links.sort_by_key(|link| link.logical_id);
    links
}

fn snapshot_link_target(
    destination: Destination,
    paged: &PagedIntrospector,
) -> Option<SnapshotLinkTarget> {
    match destination {
        Destination::Url(url) => {
            Some(SnapshotLinkTarget::Url(url.into_inner().as_str().into()))
        }
        Destination::Location(location) => {
            paged.query(&Selector::Location(location)).first().map(|target| {
                SnapshotLinkTarget::Node(ExportSource::from_content(target).logical_id)
            })
        }
        Destination::Position(position) => Some(SnapshotLinkTarget::Position {
            page: position.page.get(),
            x_pt: position.point.x.to_pt(),
            y_pt: position.point.y.to_pt(),
        }),
    }
}

fn push_node(
    nodes: &mut Vec<SnapshotNode>,
    content: &Content,
    paged: Option<&PagedIntrospector>,
    engine: &mut Engine,
    styles: StyleChain,
) {
    let source = ExportSource::from_content(content);
    let paged_positions = matched_positions(content, paged);
    let page_counters = matched_page_counters(content, paged, engine, styles);
    if let Some(existing) = nodes
        .iter_mut()
        .find(|node| node.source.logical_id == source.logical_id)
    {
        existing.semantic_occurrences += 1;
        existing.paged_positions.extend(paged_positions);
        normalize_positions(&mut existing.paged_positions);
        existing.page_counters.extend(page_counters);
        normalize_page_counters(&mut existing.page_counters);
    } else {
        nodes.push(SnapshotNode {
            source,
            semantic_occurrences: 1,
            paged_positions,
            page_counters,
        });
    }
}

fn matched_page_counters(
    content: &Content,
    paged: Option<&PagedIntrospector>,
    engine: &mut Engine,
    styles: StyleChain,
) -> Vec<SnapshotPageCounter> {
    use typst_library::foundations::{Target, TargetElem};

    let Some(paged) = paged else { return Vec::new() };
    let locations = paged
        .query(&content.elem().select())
        .iter()
        .filter(|candidate| candidate.span() == content.span())
        .filter_map(|candidate| candidate.location())
        .collect::<Vec<_>>();
    let mut counters = Vec::new();
    for location in locations {
        let Some(page) = paged.page(location) else { continue };
        let Some(numbering) = paged.page_numbering(location) else { continue };
        let mut sink = typst_library::engine::Sink::new();
        let mut sub = Engine {
            world: engine.world,
            library: engine.library,
            introspector: typst_utils::Protected::new(
                (paged as &dyn Introspector).track(),
            ),
            traced: engine.traced,
            sink: sink.track_mut(),
            route: typst_library::engine::Route::extend(engine.route.track()),
        };
        let target = TargetElem::target.set(Target::Paged).wrap();
        let paged_styles = styles.chain(&target);
        let Ok(display) = Counter::new(CounterKey::Page).display_at(
            &mut sub,
            location,
            paged_styles,
            numbering,
            content.span(),
        ) else {
            continue;
        };
        counters.push(SnapshotPageCounter {
            page: page.get(),
            display: display.plain_text(),
        });
    }
    normalize_page_counters(&mut counters);
    counters
}

fn normalize_page_counters(counters: &mut Vec<SnapshotPageCounter>) {
    counters.sort_by(|a, b| a.page.cmp(&b.page).then_with(|| a.display.cmp(&b.display)));
    counters.dedup();
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

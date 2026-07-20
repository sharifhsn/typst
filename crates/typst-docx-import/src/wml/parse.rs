//! Parse an OPC package into the [`WmlPackage`] Word IR.
//!
//! This is a straightforward, pragmatic walk of `word/document.xml` and its
//! companion parts using `roxmltree`. It matches elements by *local* name
//! (namespace prefixes are a red herring across real-world producers) and
//! stores raw OOXML values verbatim — twips, half-points, hex colors, style
//! ids — exactly as [`crate::wml::model`] documents. Resolution against the
//! style hierarchy and unit conversion happen later, in [`crate::resolve`]
//! and the mappers.

use ecow::{EcoString, eco_format};
use roxmltree::{Document, Node};
use rustc_hash::FxHashMap;
use typst_ooxml_core::{ns, opc::Reader};

use crate::ImportError;
use crate::report::ImportReport;
use crate::wml::model::{
    Body, BodyItem, BreakType, Cell, ChartData, ChartSeries, DrawingRef, Field, FurnitureKind,
    FurnitureRef, LevelFormat, NumRef, Numbering, ParaProps, Paragraph, Relationship, Row, Run,
    RunContent, RunItem, RunProps, SectPr, Style, StyleKind, Styles, Table, WmlPackage,
};

/// Open the `.docx` and parse `word/document.xml`, `styles.xml`,
/// `numbering.xml`, relationships and media into the Word IR.
pub fn parse_package(
    bytes: &[u8],
    _report: &mut ImportReport,
) -> Result<WmlPackage, ImportError> {
    // Fail early with a real error if it isn't even a package, so the stub is
    // honest end-to-end.
    let mut reader = Reader::open(bytes)?;
    if !reader.has("word/document.xml") {
        return Err(ImportError::NotAWordDocument);
    }

    let mut rels = parse_rels(&mut reader)?;
    let even_and_odd_headers = parse_even_and_odd_headers(&mut reader)?;

    let mut media = FxHashMap::default();
    let media_names: Vec<EcoString> = reader
        .names()
        .iter()
        .filter(|name| name.starts_with("word/media/"))
        .cloned()
        .collect();
    for name in media_names {
        if let Some(bytes) = reader.part_bytes(&name)? {
            media.insert(name, bytes);
        }
    }

    let styles = match reader.xml_part("word/styles.xml")? {
        Some(xml) => parse_styles(&xml)?,
        None => Styles::default(),
    };

    let numbering = match reader.xml_part("word/numbering.xml")? {
        Some(xml) => parse_numbering(&xml)?,
        None => Numbering::default(),
    };

    // Guaranteed present by the `has` check above.
    let doc_xml = reader.xml_part("word/document.xml")?.unwrap_or_default();
    let body = parse_document(&doc_xml)?;

    let furniture = parse_furniture_parts(&mut reader, &mut rels)?;
    let footnotes = parse_notes_part(&mut reader, &mut rels, "word/footnotes.xml", "footnote")?;
    let endnotes = parse_notes_part(&mut reader, &mut rels, "word/endnotes.xml", "endnote")?;
    let charts = parse_chart_parts(&mut reader)?;

    Ok(WmlPackage {
        body,
        styles,
        numbering,
        rels,
        media,
        furniture,
        even_and_odd_headers,
        footnotes,
        endnotes,
        charts,
    })
}

// --- Relationships -----------------------------------------------------------

fn parse_rels(reader: &mut Reader) -> Result<FxHashMap<EcoString, Relationship>, ImportError> {
    Ok(parse_rels_for(reader, "word/document.xml")?.into_iter().collect())
}

/// Reads `<dir>/_rels/<file>.rels` for `part_name` (`word/header1.xml` →
/// `word/_rels/header1.xml.rels`) — the OPC convention every part-owned
/// relationships part follows, mirroring the write side's
/// `opc::relationship_part_name`. Empty (not an error) if the part has no
/// relationships of its own.
///
/// Returns a `Vec` in document order rather than a map: [`parse_rels`]
/// collects it into one, but [`parse_furniture_parts`] and
/// [`parse_notes_part`] instead merge each entry into the shared, namespaced
/// `rels` map one at a time — iterating a `Vec` there keeps that merge's
/// order deterministic, rather than iterating a hash map in arbitrary order.
fn parse_rels_for(
    reader: &mut Reader,
    part_name: &str,
) -> Result<Vec<(EcoString, Relationship)>, ImportError> {
    let mut rels = Vec::new();
    let rels_name = match part_name.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part_name}.rels"),
    };
    let Some(xml) = reader.xml_part(&rels_name)? else {
        return Ok(rels);
    };
    let document = Document::parse(&xml).map_err(xml_err)?;
    for node in document.descendants().filter(|n| is_element(*n, "Relationship")) {
        let (Some(id), Some(target)) = (attr(node, "Id"), attr(node, "Target")) else {
            continue;
        };
        let external = attr(node, "TargetMode") == Some("External");
        rels.push((id.into(), Relationship { target: target.into(), external }));
    }
    Ok(rels)
}

// --- word/document.xml --------------------------------------------------------

fn parse_document(xml: &str) -> Result<Body, ImportError> {
    let document = Document::parse(xml).map_err(xml_err)?;
    let root = document.root_element();
    let body_node = root
        .children()
        .find(|n| is_element(*n, "body"))
        .ok_or_else(|| ImportError::Xml("word/document.xml has no w:body".into()))?;
    Ok(parse_body_content(body_node, 0))
}

/// How deep a nest of *transparent wrapper* elements [`unwrap_wrappers`]/
/// [`splice_wrappers`] will follow before giving up and dropping the
/// content, rather than recursing further. Bounded for the same reason as
/// [`MAX_TABLE_DEPTH`]: a hostile document must not be able to drive
/// unbounded recursion. Shared by both wrappers [`splice_node`] handles (see
/// its doc comment) since either can nest inside the other.
const MAX_WRAPPER_DEPTH: usize = 32;

/// The element children of `node`, with every transparent wrapper replaced by
/// the children of whichever of its own children actually holds the content
/// — see [`splice_node`] for which wrappers and why.
fn unwrap_wrappers<'a>(node: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    let mut out = Vec::new();
    splice_children(node, 0, &mut out);
    out
}

/// Splice an iterator of already-selected children (the caller may have
/// filtered some out, as the paragraph walk does for `w:pPr`). Generic, but
/// *not* recursive — the recursion lives in the two concrete functions below,
/// because a generic function that recurses on a freshly-filtered iterator
/// instantiates a new closure type per level and never stops monomorphizing.
fn splice_wrappers<'a>(children: impl Iterator<Item = Node<'a, 'a>>, out: &mut Vec<Node<'a, 'a>>) {
    for child in children {
        splice_node(child, 0, out);
    }
}

fn splice_children<'a>(parent: Node<'a, 'a>, depth: usize, out: &mut Vec<Node<'a, 'a>>) {
    for child in parent.children().filter(|n| n.is_element()) {
        splice_node(child, depth, out);
    }
}

/// Two Word wrapper elements render nothing themselves — everything the
/// reader sees lives inside one particular child — so both are spliced away
/// wherever they appear, transparently, rather than matched as content in
/// their own right:
///
/// - `w:sdt` (a *structured document tag* — Word's content control: date
///   picker, drop-down, rich-text placeholder, the wrapper Word puts around a
///   cover page or a footer's page-number field). Everything lives in its
///   `w:sdtContent` child. Real documents lean on these heavily — the POI
///   corpus's `Bug60341.docx` wraps its entire footer in one — and ignoring
///   the element drops that content silently, which is the one failure mode
///   this importer must not have.
/// - `mc:AlternateContent` (Markup Compatibility and Extensibility). Word
///   writes anything with more than one possible XML spelling this way —
///   most visibly a modern text box, duplicated as *both* a DrawingML
///   `wps:txbx` (inside `mc:Choice`) and a VML `v:textbox` (inside
///   `mc:Fallback`) holding the *same* content. Per the MCE spec (ECMA-376
///   Part 3), a consumer takes the first `mc:Choice` whose `Requires`
///   namespace it supports and ignores every other branch, using
///   `mc:Fallback` only when no `mc:Choice` applies at all. This importer
///   doesn't maintain a namespace-support table to decide "applies" with —
///   every real producer (Word itself) emits at most one `mc:Choice` per
///   `mc:AlternateContent`, so unconditionally preferring it over
///   `mc:Fallback` reaches the same answer without needing one. Treating the
///   element as opaque (or, worse, naively collecting every `w:txbxContent`
///   in the document regardless of which wrapper it's under) duplicates
///   everything it wraps — `shapes-with-text.docx` in the POI corpus writes
///   every text box exactly this way, and a naive walk doubles every one of
///   them.
///
/// Both can nest (an `mc:AlternateContent` inside a `w:sdt`'s content, either
/// inside itself), so both share one recursive function and one depth cap
/// ([`MAX_WRAPPER_DEPTH`]) rather than each guarding its own.
/// Whether an `mc:Choice`'s `Requires` is something we can render better than
/// the `mc:Fallback` beside it.
///
/// MCE's contract is that a consumer takes a `Choice` only if it supports that
/// choice's requirement, and otherwise falls back. Taking every `Choice`
/// unconditionally is the right default here — the common requirements (`wps`
/// text boxes, `wpg` groups, the `a14`/`w14` drawing extensions) all name
/// constructs this importer handles *better* than the legacy VML fallback
/// beside them.
///
/// `cx` — the 2014 extended charts (box-and-whisker, sunburst, waterfall,
/// treemap) — is the exception. Typst can't draw those, and flattening a
/// sunburst's hierarchy into a table misrepresents it. Word helpfully puts a
/// picture of the chart it already rendered in the `Fallback`, which is
/// exactly what MCE is for, so we take it: a correct image of the chart beats
/// a lossy table of its data.
fn honors_requirement(choice: Node) -> bool {
    /// Requirements whose `Fallback` reproduces the content better than we can.
    const UNSUPPORTED: &[&str] = &["cx"];

    attr(choice, "Requires").is_none_or(|requires| {
        !requires.split_whitespace().any(|req| UNSUPPORTED.contains(&req))
    })
}

fn splice_node<'a>(child: Node<'a, 'a>, depth: usize, out: &mut Vec<Node<'a, 'a>>) {
    match child.tag_name().name() {
        // Wrappers that carry no content of their own, only annotation:
        // `w:smartTag` is Word's old auto-recognition markup (place names,
        // dates), which nests several deep around a single run;
        // `w:bdo`/`w:dir` are bidirectional overrides. Their *children* are
        // ordinary runs, so splicing them in preserves the text. The
        // annotation itself has no Typst counterpart and is dropped — losing
        // a bidi override is a far smaller error than losing the sentence.
        "smartTag" | "bdo" | "dir" => {
            if depth < MAX_WRAPPER_DEPTH {
                splice_children(child, depth + 1, out);
            }
        }
        "sdt" => {
            if depth < MAX_WRAPPER_DEPTH
                && let Some(content) = child.children().find(|n| is_element(*n, "sdtContent"))
            {
                splice_children(content, depth + 1, out);
            }
        }
        "AlternateContent" => {
            if depth < MAX_WRAPPER_DEPTH {
                let branch = child
                    .children()
                    .find(|n| is_element(*n, "Choice") && honors_requirement(*n))
                    .or_else(|| child.children().find(|n| is_element(*n, "Fallback")));
                if let Some(branch) = branch {
                    splice_children(branch, depth + 1, out);
                }
            }
        }
        _ => out.push(child),
    }
}

/// Walk a body-shaped container's children into a [`Body`]: paragraphs,
/// tables, and (only meaningful for `w:body`) a trailing `w:sectPr`. Shared
/// by [`parse_document`] (`word/document.xml`'s `w:body`),
/// [`parse_furniture_part`] (a `w:hdr`/`w:ftr` part's root), and
/// [`parse_txbx_content`] (a text box's own content, which is this same
/// paragraph/table shape) — all hold the same content, so this is the one
/// place that matches child element names rather than copies drifting apart.
///
/// `tb_depth` is how many text boxes deep this call is nested — 0 at every
/// top-level part, incremented only by [`parse_txbx_content`] — see
/// [`MAX_TEXTBOX_DEPTH`].
fn parse_body_content(node: Node, tb_depth: usize) -> Body {
    let mut body = Body::default();
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "p" => body.items.push(BodyItem::Paragraph(parse_paragraph(child, tb_depth))),
            "tbl" => body.items.push(BodyItem::Table(parse_table(child, 0, tb_depth))),
            "sectPr" => body.sect_pr = Some(parse_sectpr(child)),
            _ => {}
        }
    }
    body
}

// --- word/header*.xml, word/footer*.xml (`w:hdr`/`w:ftr`) ---------------------

/// Parse every `w:hdr`/`w:ftr` part in the package into the shared furniture
/// map, merging each part's own relationships into `rels` first.
///
/// Header/footer parts number their `rId`s independently of `document.xml` —
/// `word/_rels/header1.xml.rels` starts back at `rId1` regardless of what
/// `document.xml.rels` already used that id for (the POI corpus's
/// `headerPic.docx` proves this collides for real: its header's `rId1` is an
/// image, while `document.xml.rels`'s `rId1` is `styles.xml`). Rather than
/// merge blindly and silently resolve a header's image against the wrong
/// target, every relationship from a furniture part's `.rels` is inserted
/// under a `"{part}!{rid}"` key, and [`namespace_furniture_rels`] rewrites
/// that part's own content to reference ids in the same namespaced form.
/// Downstream lookup code (`package.rels.get(id)`) then needs no change at
/// all — it just resolves whatever id it's handed, bare or namespaced.
fn parse_furniture_parts(
    reader: &mut Reader,
    rels: &mut FxHashMap<EcoString, Relationship>,
) -> Result<FxHashMap<EcoString, Body>, ImportError> {
    let mut furniture = FxHashMap::default();
    let names: Vec<EcoString> = reader
        .names()
        .iter()
        .filter(|name| {
            (name.starts_with("word/header") || name.starts_with("word/footer"))
                && name.ends_with(".xml")
        })
        .cloned()
        .collect();

    for name in names {
        let Some(xml) = reader.xml_part(&name)? else { continue };
        let mut body = parse_furniture_part(&xml)?;
        namespace_rel_ids(&mut body.items, &name);

        for (rid, rel) in parse_rels_for(reader, &name)? {
            rels.insert(eco_format!("{name}!{rid}"), rel);
        }

        furniture.insert(name, body);
    }
    Ok(furniture)
}

/// Parse a `w:hdr`/`w:ftr` part into a [`Body`]. Unlike `word/document.xml`,
/// there's no wrapping `w:body` — the root element itself is the content
/// container — but its children are otherwise the same paragraph/table
/// content [`parse_body_content`] already walks; a furniture part never
/// carries its own `w:sectPr`.
fn parse_furniture_part(xml: &str) -> Result<Body, ImportError> {
    let document = Document::parse(xml).map_err(xml_err)?;
    Ok(parse_body_content(document.root_element(), 0))
}

/// Rewrite every relationship id inside a freshly parsed part body to its
/// namespaced form (see [`parse_furniture_parts`] for why a part gets its own
/// namespace). Shared by furniture parts (`w:hdr`/`w:ftr`) and, per note, by
/// [`parse_notes_part`] (`word/footnotes.xml`/`word/endnotes.xml`) — anywhere
/// a part numbers its `rId`s independently of `document.xml`. Recurses into
/// hyperlinks, fields, nested tables, and (transitively, via
/// [`namespace_run_items`]) a text box's own content, so nothing in a part's
/// content tree is missed; a hyperlink's `rel_id` — and an image inside a
/// text box floating in a header/footer/note — is namespaced for the same
/// reason a bare drawing's is: all are looked up in the same shared `rels`
/// map, so all are exposed to the same cross-part collision.
fn namespace_rel_ids(items: &mut [BodyItem], part: &str) {
    for item in items {
        match item {
            BodyItem::Paragraph(p) => namespace_run_items(&mut p.runs, part),
            BodyItem::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        namespace_rel_ids(&mut cell.content, part);
                    }
                }
            }
        }
    }
}

fn namespace_run_items(items: &mut [RunItem], part: &str) {
    for item in items {
        match item {
            RunItem::Run(r) => {
                for c in &mut r.content {
                    match c {
                        RunContent::Drawing(d) => {
                            d.rel_id = eco_format!("{part}!{}", d.rel_id);
                        }
                        // A chart reference is a relationship id too — same
                        // reasoning as `Drawing`'s just above.
                        RunContent::Chart(rel_id) => {
                            *rel_id = eco_format!("{part}!{rel_id}");
                        }
                        // A text box's own content is itself a body — it can
                        // hold drawings and hyperlinks of its own, which need
                        // the same namespacing as everything else in this
                        // part.
                        RunContent::TextBox(body_items) => {
                            namespace_rel_ids(body_items, part);
                        }
                        _ => {}
                    }
                }
            }
            RunItem::Hyperlink { rel_id, runs, .. } => {
                if let Some(id) = rel_id {
                    *id = eco_format!("{part}!{id}");
                }
                namespace_run_items(runs, part);
            }
            RunItem::Field(f) => namespace_run_items(&mut f.result, part),
        }
    }
}

// --- word/footnotes.xml, word/endnotes.xml ------------------------------------

/// `w:type` values that mark Word's own rule-line boilerplate — the
/// separator, continuation-separator, and continuation-notice notes every
/// document with footnotes carries automatically (drawn once at the point a
/// note run continues across a page break). Never authored content, so
/// [`parse_notes_part`] skips these entirely rather than importing Word's
/// horizontal-rule furniture as if it were a real note.
fn is_boilerplate_note(node: Node) -> bool {
    matches!(
        attr(node, "type"),
        Some("separator" | "continuationSeparator" | "continuationNotice")
    )
}

/// Parse `word/footnotes.xml`/`word/endnotes.xml` (`element_name` is
/// `"footnote"`/`"endnote"`, matching the child element `w:footnotes`/
/// `w:endnotes` actually holds) into a map of `w:id` → [`Body`].
///
/// A note's children are ordinary body content — [`parse_body_content`] is
/// reused verbatim, which is also what gives a note's own `w:sdt` wrappers
/// (content controls) the same transparent unwrapping as everywhere else.
/// Notes can carry images and hyperlinks too, so this part's relationships
/// are merged into the shared `rels` map under the same `"{part}!{rid}"`
/// namespace [`parse_furniture_parts`] uses — `word/footnotes.xml` and
/// `word/endnotes.xml` each number their `rId`s independently of
/// `document.xml`, exactly like a header/footer part.
fn parse_notes_part(
    reader: &mut Reader,
    rels: &mut FxHashMap<EcoString, Relationship>,
    part_name: &str,
    element_name: &str,
) -> Result<FxHashMap<i64, Body>, ImportError> {
    let mut notes = FxHashMap::default();
    let Some(xml) = reader.xml_part(part_name)? else {
        return Ok(notes);
    };
    let document = Document::parse(&xml).map_err(xml_err)?;
    let root = document.root_element();

    for child in root.children().filter(|n| is_element(*n, element_name)) {
        if is_boilerplate_note(child) {
            continue;
        }
        // A note with no parsable `w:id` can never be resolved against a
        // `RunContent::NoteRef`, so it's not worth keeping.
        let Some(id) = attr(child, "id").and_then(parse_i64) else { continue };
        let mut body = parse_body_content(child, 0);
        namespace_rel_ids(&mut body.items, part_name);
        notes.insert(id, body);
    }

    for (rid, rel) in parse_rels_for(reader, part_name)? {
        rels.insert(eco_format!("{part_name}!{rid}"), rel);
    }

    Ok(notes)
}

// --- word/settings.xml ---------------------------------------------------------

/// Whether `settings.xml` declares `<w:evenAndOddHeaders/>` — the switch that
/// makes an `even`-typed header/footer reference active (see
/// [`crate::mappers::section`]).
fn parse_even_and_odd_headers(reader: &mut Reader) -> Result<bool, ImportError> {
    let Some(xml) = reader.xml_part("word/settings.xml")? else {
        return Ok(false);
    };
    let document = Document::parse(&xml).map_err(xml_err)?;
    let root = document.root_element();
    Ok(root.children().any(|n| is_element(n, "evenAndOddHeaders")))
}

fn parse_paragraph(node: Node, tb_depth: usize) -> Paragraph {
    let props = node
        .children()
        .find(|n| is_element(*n, "pPr"))
        .map(parse_para_props)
        .unwrap_or_default();
    let runs = fold_field_children(
        node.children().filter(|n| n.is_element() && n.tag_name().name() != "pPr"),
        tb_depth,
    );
    Paragraph { props, runs }
}

fn parse_hyperlink(node: Node, tb_depth: usize) -> RunItem {
    let rel_id = attr_ns(node, ns::R, "id").map(EcoString::from);
    let anchor = attr(node, "anchor").map(EcoString::from);
    let runs = fold_field_children(node.children().filter(|n| n.is_element()), tb_depth);
    RunItem::Hyperlink { rel_id, anchor, runs }
}

// --- Fields (`w:fldSimple`, `w:fldChar`) --------------------------------------

/// Maximum simultaneous open field frames (`begin`s without a matching
/// `end`) [`fold_field_children`] will track — the field-folding counterpart
/// of [`MAX_TABLE_DEPTH`]. Real documents nest a handful of fields deep at
/// most (e.g. a TOC entry's PAGEREF nested inside a HYPERLINK); this only
/// bites a pathological or corrupt document. Past the cap, further `begin`s
/// are treated as ordinary (in practice content-free) runs instead of
/// growing the stack, so a hostile document can't drive unbounded
/// allocation.
const MAX_FIELD_DEPTH: usize = 64;

/// One open field frame during [`fold_field_children`]'s walk. Instruction
/// text accumulates directly into `instr`; ordinary content is only ever
/// appended to `result`, and only once `separated` (Word's `w:fldChar
/// w:fldCharType="separate"`) has been seen — content between `begin` and
/// `separate` is field plumbing (proofing marks, bookmarks, …), not part of
/// either the instruction or the visible result, so it's dropped.
#[derive(Default)]
struct FieldFrame {
    instr: EcoString,
    separated: bool,
    result: Vec<RunItem>,
}

/// What one `w:r` contributes to the field-folding state machine — a
/// private, parse-time-only classification that never leaves this module (it
/// would let field plumbing leak into [`RunContent`], which the ordinary
/// lowering path has no use for). A `w:fldChar` marker or `w:instrText`
/// inside a run overrides whatever else that run might contain, mirroring
/// Word's own convention that a marker run carries no other meaningful
/// content.
enum RunKind {
    /// An ordinary run — its fully parsed [`Run`].
    Content(Run),
    /// `<w:fldChar w:fldCharType="begin"/>`.
    FieldBegin,
    /// `<w:fldChar w:fldCharType="separate"/>`.
    FieldSeparate,
    /// `<w:fldChar w:fldCharType="end"/>`.
    FieldEnd,
    /// `<w:instrText>` content, concatenated across however many
    /// `w:instrText` children the run has (in practice zero or one).
    InstrText(EcoString),
}

fn classify_run(node: Node, tb_depth: usize) -> RunKind {
    if let Some(fld) = node.children().find(|n| is_element(*n, "fldChar")) {
        return match attr(fld, "fldCharType") {
            Some("begin") => RunKind::FieldBegin,
            Some("separate") => RunKind::FieldSeparate,
            Some("end") => RunKind::FieldEnd,
            // Not a marker type this folder understands — fall back to
            // parsing the run normally rather than dropping it.
            _ => RunKind::Content(parse_run(node, tb_depth)),
        };
    }
    let mut instr = EcoString::new();
    for t in node.children().filter(|n| is_element(*n, "instrText")) {
        instr.push_str(t.text().unwrap_or_default());
    }
    if !instr.is_empty() {
        return RunKind::InstrText(instr);
    }
    RunKind::Content(parse_run(node, tb_depth))
}

/// Fold a paragraph's (or hyperlink's) content children into [`RunItem`]s,
/// collapsing the flattened `w:fldChar` begin/separate/end run sequence back
/// into [`RunItem::Field`]s. This is a state machine over *siblings* rather
/// than a per-child function (the old `push_inline_child`), because a
/// complex field's boundaries are siblings, not nested elements: `begin` and
/// `end` mark a *span* of the child list to collapse into one logical item.
/// `w:fldSimple` needs no such folding itself (its instruction is an
/// attribute, not a sibling span) but its own children are routed back
/// through this function via [`parse_fld_simple`], so a field or hyperlink
/// nested inside a simple field's cached result still works.
///
/// Everything this function doesn't recognize (bookmarks, proofing errors,
/// comment markers, revision wrappers) is silently skipped — out of scope
/// for v1, same as the function it replaces.
fn fold_field_children<'a>(
    children: impl Iterator<Item = Node<'a, 'a>>,
    tb_depth: usize,
) -> Vec<RunItem> {
    let mut top: Vec<RunItem> = Vec::new();
    let mut stack: Vec<FieldFrame> = Vec::new();

    // Content controls wrap *inline* content too, and a field's begin/end pair
    // can straddle one. Splicing them away before folding means the state
    // machine below sees a flat run sequence, exactly as if Word had never
    // wrapped it.
    let mut flat = Vec::new();
    splice_wrappers(children, &mut flat);

    for child in flat {
        match child.tag_name().name() {
            "r" => match classify_run(child, tb_depth) {
                RunKind::FieldBegin => {
                    if stack.len() < MAX_FIELD_DEPTH {
                        stack.push(FieldFrame::default());
                    } else {
                        // Nesting cap hit: don't grow the stack further (see
                        // MAX_FIELD_DEPTH) — treat the marker run as
                        // ordinary content instead, which in practice is
                        // empty (a `begin` run carries no other content by
                        // Word's convention) but preserves anything it does
                        // carry rather than silently discarding it.
                        push_item(&mut stack, &mut top, RunItem::Run(parse_run(child, tb_depth)));
                    }
                }
                RunKind::FieldSeparate => {
                    if let Some(frame) = stack.last_mut() {
                        frame.separated = true;
                    }
                }
                RunKind::FieldEnd => {
                    if let Some(frame) = stack.pop() {
                        let field =
                            RunItem::Field(Field { instr: frame.instr, result: frame.result });
                        push_item(&mut stack, &mut top, field);
                    }
                    // A stray `end` with no open frame: nothing to close.
                }
                RunKind::InstrText(text) => {
                    if let Some(frame) = stack.last_mut() {
                        // Word always emits instrText before `separate`; if a
                        // malformed document has some after, dropping it is
                        // safer than misfiling raw instruction text as
                        // either the instruction or the visible result.
                        if !frame.separated {
                            frame.instr.push_str(&text);
                        }
                    }
                }
                RunKind::Content(run) => push_item(&mut stack, &mut top, RunItem::Run(run)),
            },
            "fldSimple" => push_item(&mut stack, &mut top, parse_fld_simple(child, tb_depth)),
            "hyperlink" => push_item(&mut stack, &mut top, parse_hyperlink(child, tb_depth)),
            "oMath" => push_item(&mut stack, &mut top, RunItem::Run(math_run(child))),
            "oMathPara" => {
                for m in child.children().filter(|n| is_element(*n, "oMath")) {
                    push_item(&mut stack, &mut top, RunItem::Run(math_run(m)));
                }
            }
            _ => {}
        }
    }

    // Unterminated fields (a `begin` with no matching `end` before the
    // paragraph/hyperlink ends): flatten each still-open frame's
    // accumulated *result* content into its parent, rather than dropping it
    // or wrapping a half-formed instruction as a `RunItem::Field` — losing
    // visible text is the one failure mode this folder must avoid. A frame
    // that never reached `separate` has an empty `result` (nothing visible
    // was ever accumulated), so nothing is lost there either.
    while let Some(frame) = stack.pop() {
        for item in frame.result {
            push_item(&mut stack, &mut top, item);
        }
    }

    top
}

/// `<w:fldSimple w:instr="...">...</w:fldSimple>` — the "simple field" OOXML
/// spelling: the instruction lives in an attribute, and the element's
/// children are the cached result (what Word last rendered), which may
/// itself nest further fields or hyperlinks.
fn parse_fld_simple(node: Node, tb_depth: usize) -> RunItem {
    let instr = attr(node, "instr").unwrap_or_default().into();
    let result = fold_field_children(node.children().filter(|n| n.is_element()), tb_depth);
    RunItem::Field(Field { instr, result })
}

/// Append `item` to the innermost open field frame's result — but only if
/// that frame has been `separate`d (see [`FieldFrame`]); otherwise the item
/// is instruction-phase content and is dropped. With no open frame, `item`
/// goes to the top-level list.
fn push_item(stack: &mut [FieldFrame], top: &mut Vec<RunItem>, item: RunItem) {
    match stack.last_mut() {
        Some(frame) if frame.separated => frame.result.push(item),
        Some(_) => {}
        None => top.push(item),
    }
}

/// `w:ruby` — a phonetic guide (furigana): `w:rt` holds the small reading
/// printed above `w:rubyBase`'s text. Both halves hold ordinary runs, and both
/// are real document text, so dropping the element loses the sentence itself,
/// not just an annotation.
fn parse_ruby(node: Node, tb_depth: usize) -> RunContent {
    let half = |name: &str| {
        node.children()
            .find(|n| is_element(*n, name))
            .map(|n| fold_field_children(unwrap_wrappers(n).into_iter(), tb_depth))
            .unwrap_or_default()
    };
    RunContent::Ruby { base: half("rubyBase"), gloss: half("rt") }
}

fn parse_run(node: Node, tb_depth: usize) -> Run {
    let mut run = Run::default();
    // A run's own children need wrapper-splicing too, not just a
    // paragraph's: Word wraps a `w:drawing`/`w:pict` pair's `mc:Choice`/
    // `mc:Fallback` *inside* the `w:r` that hosts the drawing (`<w:r>
    // <w:rPr/><mc:AlternateContent>...</mc:AlternateContent></w:r>`), not as
    // a sibling of it at the paragraph level — that's where a text box's
    // `mc:AlternateContent` actually lives, per the hazard this importer
    // must get right (see `splice_node`). A plain `children().filter(..)`
    // here would silently drop the whole thing into the `_ => {}` arm below.
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "rPr" => run.props = parse_run_props(child),
            "t" => {
                // `w:t` text is significant; roxmltree never collapses or
                // trims text-node content, so this already preserves
                // whitespace exactly regardless of `xml:space`.
                let text: EcoString = child.text().unwrap_or_default().into();
                run.content.push(RunContent::Text(text));
            }
            // `w:ptab` (a *positional* tab — jump to a margin/page-relative
            // stop, e.g. "left|center|right") is the element real-world
            // headers/footers actually use for the classic three-column
            // layout — `ThreeColHeadFoot.docx` in the POI corpus has no
            // `w:tab` at all. Typst has no positional-tab primitive either,
            // so this collapses to the same plain `RunContent::Tab` a
            // regular `w:tab` produces; see `mappers::section`'s tab-stop
            // report note for the resulting approximation.
            "tab" | "ptab" => run.content.push(RunContent::Tab),
            "br" => {
                let kind = match attr(child, "type") {
                    Some("page") => BreakType::Page,
                    Some("column") => BreakType::Column,
                    _ => BreakType::Line,
                };
                run.content.push(RunContent::Break(kind));
            }
            "drawing" => {
                if let Some(drawing) = parse_drawing(child) {
                    run.content.push(RunContent::Drawing(drawing));
                } else {
                    // No raster image — a shape, chart, or (what we're after
                    // here) a text box. `direct_txbx_contents` finds every
                    // `w:txbxContent` that's a sibling shape of this drawing
                    // (a group can hold more than one), so a picture-with-
                    // caption drawing (blip present) never reaches this
                    // branch at all: the image wins and its caption box, if
                    // any, is simply not looked for — not contorting this
                    // for a case real documents rarely combine.
                    let txbx_contents = direct_txbx_contents(child);
                    if !txbx_contents.is_empty() {
                        for txbx in txbx_contents {
                            run.content
                                .push(RunContent::TextBox(parse_txbx_content(txbx, tb_depth)));
                        }
                    } else if let Some(rel_id) = parse_chart_ref(child) {
                        // Checked last: a chart reference has neither a blip
                        // nor a text box of its own, so this only fires once
                        // both of those have come up empty.
                        run.content.push(RunContent::Chart(rel_id));
                    }
                }
            }
            // `w:pict` — the VML spelling of a drawing. Modern Word only
            // ever writes this as the `mc:Fallback` half of an
            // `mc:AlternateContent` (already resolved away from the
            // `mc:Choice` branch by `splice_node`, so this arm only ever
            // sees it when there's no `mc:Choice` at all — a document
            // authored VML-only, or the corpus's older fixtures). A real VML
            // *picture* (`v:imagedata`) has no text content and so no
            // `w:txbxContent` inside it, so this arm does nothing for one,
            // same as it did before this arm existed.
            "pict" => {
                for txbx in direct_txbx_contents(child) {
                    run.content.push(RunContent::TextBox(parse_txbx_content(txbx, tb_depth)));
                }
            }
            "oMath" => run.content.push(RunContent::Math(raw_xml(child))),
            "ruby" => run.content.push(parse_ruby(child, tb_depth)),
            // The marker in the body text; the note's own content lives in
            // `word/footnotes.xml`/`word/endnotes.xml`, resolved later by
            // `crate::mappers::note` against [`crate::wml::model::WmlPackage`]'s
            // `footnotes`/`endnotes` maps. A reference with no parsable
            // `w:id` has nothing to resolve, so it's dropped here rather than
            // kept as a marker nothing can look up.
            "footnoteReference" => {
                if let Some(id) = attr(child, "id").and_then(parse_i64) {
                    run.content.push(RunContent::NoteRef { endnote: false, id });
                }
            }
            "endnoteReference" => {
                if let Some(id) = attr(child, "id").and_then(parse_i64) {
                    run.content.push(RunContent::NoteRef { endnote: true, id });
                }
            }
            // `w:footnoteRef`/`w:endnoteRef` — the *number placeholder*
            // inside a note's own body (Word substitutes it with the note's
            // rendered number), not the reference marker in the main body.
            // Typst renumbers footnotes itself, so this must never surface as
            // stray text in the imported note.
            "footnoteRef" | "endnoteRef" => {}
            _ => {}
        }
    }
    run
}

/// A `m:oMath` captured as a single-content run, used when the equation
/// appears directly in paragraph/hyperlink content (its normal position —
/// `m:oMath` is a sibling of `w:r`, not a child of one).
fn math_run(node: Node) -> Run {
    Run { props: RunProps::default(), content: vec![RunContent::Math(raw_xml(node))] }
}

/// A `w:drawing`'s embedded raster image: the blip's relationship id, its
/// extent, and alt text. `None` if no `a:blip` is present (a shape, chart, or
/// text box with no raster image — dropped for v1).
fn parse_drawing(node: Node) -> Option<DrawingRef> {
    let blip = node.descendants().find(|n| is_element(*n, "blip"))?;
    let rel_id = attr_ns(blip, ns::R, "embed")?.into();
    let extent = node.descendants().find(|n| is_element(*n, "extent"));
    let cx_emu = extent.and_then(|n| attr(n, "cx")).and_then(parse_i64);
    let cy_emu = extent.and_then(|n| attr(n, "cy")).and_then(parse_i64);
    let alt = node
        .descendants()
        .find(|n| is_element(*n, "docPr"))
        .and_then(|n| attr(n, "descr"))
        .map(EcoString::from);
    Some(DrawingRef { rel_id, cx_emu, cy_emu, alt })
}

/// A `w:drawing`'s chart reference: the `r:id` of its `c:chart` graphic-data
/// element. Word spells this element `c:chart` — local name "chart" in the
/// classic drawingml/2006/chart namespace — for a ChartEx chart too (only
/// the *target part* it points at differs; see [`ChartData`]'s doc comment),
/// so matching by local name covers both without distinguishing them here.
/// `None` if there's no such element (a shape or an image, not a chart) or it
/// has no `r:id` (nothing to resolve).
fn parse_chart_ref(node: Node) -> Option<EcoString> {
    let chart = node.descendants().find(|n| is_element(*n, "chart"))?;
    attr_ns(chart, ns::R, "id").map(EcoString::from)
}

// --- Text boxes (`wps:txbx`/`v:textbox`'s `w:txbxContent`) --------------------

/// Maximum text-box nesting depth: a text box's own content
/// ([`parse_txbx_content`]) can itself contain a `w:drawing` whose shape is
/// another text box. Bounded for the same reason as [`MAX_TABLE_DEPTH`]/
/// [`MAX_WRAPPER_DEPTH`] — a hostile document nesting text boxes arbitrarily
/// deep must not be able to blow the stack or spend unbounded work. Past the
/// cap, a nested text box's content is dropped rather than parsed; no real
/// document nests anywhere near this deep.
const MAX_TEXTBOX_DEPTH: usize = 16;

/// Parse a `w:txbxContent` node (found inside a `wps:txbx` or `v:textbox`) as
/// ordinary body content — the same paragraph/table shape a table cell or a
/// furniture part holds — via [`parse_body_content`], one level deeper in
/// `tb_depth`. A `w:txbxContent` never carries a `w:sectPr` of its own, so
/// only the items matter. Capped by [`MAX_TEXTBOX_DEPTH`]: past it, the
/// content is dropped rather than parsed.
fn parse_txbx_content(node: Node, tb_depth: usize) -> Vec<BodyItem> {
    if tb_depth >= MAX_TEXTBOX_DEPTH {
        return Vec::new();
    }
    parse_body_content(node, tb_depth + 1).items
}

/// Every `w:txbxContent` that belongs directly to one of `node`'s shapes,
/// found by walking `node`'s subtree but stopping at each `w:txbxContent`
/// boundary rather than descending into it.
///
/// A single `w:drawing`/`w:pict` can hold more than one shape side by side —
/// a *group* (`wpg:wgp`/`v:group`) of several `wps:wsp`/`v:shape`s, each its
/// own floating text box. `shapes-with-text.docx` in the POI corpus has
/// exactly this: one `w:drawing` groups two text boxes ("A group of shapes" /
/// "Where some contain text"), and a scan that only ever takes the *first*
/// `w:txbxContent` it finds — the same simplification [`parse_drawing`] makes
/// for a drawing's first `a:blip` — would silently drop the second box's
/// text.
///
/// A plain `descendants().filter(..)` deep scan would find both of those
/// correctly, but would *also* find any `w:txbxContent` nested inside one of
/// them — a text box containing a drawing that is itself a text box — which
/// [`parse_txbx_content`]'s own recursive call (made once *that* box's
/// paragraph content is parsed) discovers a second time, duplicating it.
/// Stopping the walk at every `w:txbxContent` it finds, instead of
/// descending into it, finds every sibling shape while avoiding that
/// double-count.
fn direct_txbx_contents<'a>(node: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    let mut out = Vec::new();
    collect_direct_txbx_contents(node, &mut out);
    out
}

fn collect_direct_txbx_contents<'a>(node: Node<'a, 'a>, out: &mut Vec<Node<'a, 'a>>) {
    for child in node.children() {
        if is_element(child, "txbxContent") {
            out.push(child);
        } else {
            collect_direct_txbx_contents(child, out);
        }
    }
}

// --- Paragraph / run properties -----------------------------------------------

fn parse_para_props(node: Node) -> ParaProps {
    let mut props = ParaProps::default();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "pStyle" => props.style_id = attr(child, "val").map(EcoString::from),
            "jc" => props.jc = attr(child, "val").map(EcoString::from),
            "numPr" => props.num = parse_num_pr(child),
            "spacing" => {
                if let Some(before) = attr(child, "before").and_then(parse_i64) {
                    props.spacing_before = Some(before);
                }
                if let Some(line) = attr(child, "line").and_then(parse_i64) {
                    props.line = Some(line);
                }
            }
            "ind" => {
                props.indent_left = attr(child, "left")
                    .or_else(|| attr(child, "start"))
                    .and_then(parse_i64);
            }
            "pBdr" => {
                props.bottom_border =
                    child.children().any(|n| is_element(n, "bottom"));
            }
            "rPr" => props.mark_props = parse_run_props(child),
            _ => {}
        }
    }
    props
}

fn parse_num_pr(node: Node) -> Option<NumRef> {
    let mut num_id = None;
    let mut ilvl = None;
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "numId" => num_id = attr(child, "val").and_then(parse_i64),
            "ilvl" => ilvl = attr(child, "val").and_then(parse_i64),
            _ => {}
        }
    }
    Some(NumRef { num_id: num_id?, ilvl: ilvl.unwrap_or(0) })
}

fn parse_run_props(node: Node) -> RunProps {
    let child = |name: &str| node.children().find(|n| is_element(*n, name));

    RunProps {
        style_id: child("rStyle").and_then(|n| attr(n, "val")).map(EcoString::from),
        bold: toggle(child("b")),
        italic: toggle(child("i")),
        strike: toggle(child("strike")),
        smallcaps: toggle(child("smallCaps")),
        underline: child("u").and_then(|n| attr(n, "val")).map(EcoString::from),
        color: child("color").and_then(|n| attr(n, "val")).map(EcoString::from),
        size_half_pt: child("sz").and_then(|n| attr(n, "val")).and_then(parse_i64),
        font: child("rFonts").and_then(|n| attr(n, "ascii")).map(EcoString::from),
        vert_align: child("vertAlign").and_then(|n| attr(n, "val")).map(EcoString::from),
        vanish: toggle(child("vanish")),
    }
}

/// OOXML tri-state toggle: `None` if the element is absent, `Some(true)` if
/// present with no `w:val` or a val other than `"0"`/`"false"`, `Some(false)`
/// for an explicit `w:val="0"`/`"false"`.
fn toggle(node: Option<Node>) -> Option<bool> {
    node.map(|n| !matches!(attr(n, "val"), Some("0" | "false")))
}

// --- Tables (`w:tbl`) ---------------------------------------------------------

/// Maximum table-nesting depth the parser will descend into. Word's own
/// practical nesting is a handful of levels; anything beyond this is either a
/// pathological/DoS document (POI's `deep-table-cell.docx` nests 5000 deep) or
/// corrupt. Capping here bounds the Word IR — and therefore the recursive
/// lowering that mirrors it — so no input can overflow the stack. Content
/// below the cap is dropped; no real document reaches it.
const MAX_TABLE_DEPTH: usize = 24;

fn parse_table(node: Node, depth: usize, tb_depth: usize) -> Table {
    let mut table = Table::default();
    // Rows can be wrapped in a content control too — Word does this for
    // repeating table sections — so this walk unwraps like the others.
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "tblGrid" => {
                for col in child.children().filter(|n| is_element(*n, "gridCol")) {
                    if let Some(w) = attr(col, "w").and_then(parse_i64) {
                        table.grid.push(w);
                    }
                }
            }
            "tr" => table.rows.push(parse_row(child, depth, tb_depth)),
            _ => {}
        }
    }
    table
}

fn parse_row(node: Node, depth: usize, tb_depth: usize) -> Row {
    let mut row = Row::default();
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "trPr" => {
                row.is_header =
                    child.children().any(|n| is_element(n, "tblHeader"));
            }
            "tc" => row.cells.push(parse_cell(child, depth, tb_depth)),
            _ => {}
        }
    }
    row
}

fn parse_cell(node: Node, depth: usize, tb_depth: usize) -> Cell {
    let mut cell =
        Cell { grid_span: 1, v_merge: None, shd_fill: None, content: Vec::new() };
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "tcPr" => parse_cell_props(child, &mut cell),
            "p" => cell.content.push(BodyItem::Paragraph(parse_paragraph(child, tb_depth))),
            "tbl" if depth < MAX_TABLE_DEPTH => {
                cell.content.push(BodyItem::Table(parse_table(child, depth + 1, tb_depth)));
            }
            _ => {}
        }
    }
    cell
}

fn parse_cell_props(node: Node, cell: &mut Cell) {
    for prop in node.children().filter(|n| n.is_element()) {
        match prop.tag_name().name() {
            "gridSpan" => {
                if let Some(span) =
                    attr(prop, "val").and_then(|s| s.parse::<usize>().ok())
                {
                    cell.grid_span = span;
                }
            }
            "vMerge" => {
                cell.v_merge = Some(attr(prop, "val") == Some("restart"));
            }
            "shd" => {
                cell.shd_fill = attr(prop, "fill").map(EcoString::from);
            }
            _ => {}
        }
    }
}

// --- Charts (`word/charts/*.xml`) ---------------------------------------------

/// Parse every chart part into a [`ChartData`] map, keyed by zip name so
/// [`crate::mappers::chart`] can resolve a [`RunContent::Chart`]'s `rId`
/// straight through [`WmlPackage::rels`] into this map, the same two-step
/// resolution [`crate::mappers::drawing`] does against `media`.
///
/// `word/charts/` also holds the chart's color/style siblings
/// (`colorsN.xml`/`styleN.xml`) — real producers name them this way, but
/// this doesn't rely on the filename: it parses every `.xml` part under the
/// directory and keeps only the ones whose root is actually a chart
/// (`chartSpace`, classic or ChartEx — see [`parse_chart_space`]), matching
/// by local name exactly as the rest of this module does.
fn parse_chart_parts(reader: &mut Reader) -> Result<FxHashMap<EcoString, ChartData>, ImportError> {
    let mut charts = FxHashMap::default();
    let names: Vec<EcoString> = reader
        .names()
        .iter()
        .filter(|name| name.starts_with("word/charts/") && name.ends_with(".xml"))
        .cloned()
        .collect();

    for name in names {
        let Some(xml) = reader.xml_part(&name)? else { continue };
        let document = Document::parse(&xml).map_err(xml_err)?;
        let root = document.root_element();
        if root.tag_name().name() != "chartSpace" {
            continue;
        }
        charts.insert(name, parse_chart_space(root));
    }
    Ok(charts)
}

/// Parse a chart part's root (`c:chartSpace`, or the ChartEx `cx:chartSpace`)
/// into its underlying data table. The two formats store series/category/
/// value data in different shapes — see [`parse_chart_classic`] and
/// [`parse_chartex`] — distinguished here only by whether a `chartData`
/// child is present, which a classic chart never emits.
fn parse_chart_space(root: Node) -> ChartData {
    match root.children().find(|n| is_element(*n, "chartData")) {
        Some(chart_data) => parse_chartex(root, chart_data),
        None => parse_chart_classic(root),
    }
}

/// A classic chart (`c:chartSpace`): `c:chart/c:title` for the title, and a
/// `c:ser` per series — wherever it's nested (`c:plotArea`'s child differs
/// per chart type: `c:barChart`, `c:pieChart`, `c:lineChart`, …, so this
/// searches for `c:ser` by local name anywhere under `c:chart` rather than
/// naming every chart-type wrapper individually).
fn parse_chart_classic(root: Node) -> ChartData {
    let mut data = ChartData::default();
    let Some(chart) = root.children().find(|n| is_element(*n, "chart")) else {
        return data;
    };
    if let Some(title) = chart.children().find(|n| is_element(*n, "title")) {
        data.title = parse_chart_title(title);
    }

    let mut have_categories = false;
    for ser in chart.descendants().filter(|n| is_element(*n, "ser")) {
        let name = series_name(ser);

        // Categories: taken from the first series that declares any — see
        // `ChartData`'s doc comment; series normally share one category axis,
        // so every series after the first is assumed to agree with it.
        if !have_categories {
            let categories = indexed_points_under(ser, "cat", pt_text_nested_v);
            if !categories.is_empty() {
                data.categories = categories;
                have_categories = true;
            }
        }

        let values = indexed_points_under(ser, "val", pt_text_nested_v);
        data.series.push(ChartSeries { name, values });
    }
    data
}

/// A ChartEx chart (`cx:chartSpace`, used for chart types introduced after
/// Office 2013 — box-and-whisker, sunburst, waterfall, funnel, …). Its data
/// lives in a `cx:chartData` sibling of `cx:chart`, as a flat list of
/// `cx:data` blocks (each with its own `id`) rather than nested inside the
/// series the way a classic chart's `c:ser` holds its own `c:cat`/`c:val`;
/// a `cx:series` instead points at one of those blocks via `cx:dataId`. See
/// [`ChartData`]'s doc comment for how well this actually maps onto the same
/// categories/series shape [`parse_chart_classic`] produces.
fn parse_chartex(root: Node, chart_data: Node) -> ChartData {
    let mut data = ChartData::default();

    // Every `cx:data` block's own (categories, values) pair, keyed by its
    // `id` — a series links to one via `cx:dataId`. A chart type with a
    // hierarchical category axis (a sunburst's leaf/stem/branch) nests more
    // than one `cx:lvl` under `cx:strDim`; only the first (finest) level is
    // kept, per `ChartData`'s doc comment.
    let mut blocks: FxHashMap<EcoString, (Vec<EcoString>, Vec<EcoString>)> = FxHashMap::default();
    for block in chart_data.children().filter(|n| is_element(*n, "data")) {
        let Some(id) = attr(block, "id").map(EcoString::from) else { continue };
        let categories = block
            .children()
            .find(|n| is_element(*n, "strDim"))
            .and_then(|dim| dim.children().find(|n| is_element(*n, "lvl")))
            .map(|lvl| collect_indexed_pts(lvl, pt_text_direct))
            .unwrap_or_default();
        let values = block
            .children()
            .find(|n| is_element(*n, "numDim"))
            .and_then(|dim| dim.children().find(|n| is_element(*n, "lvl")))
            .map(|lvl| collect_indexed_pts(lvl, pt_text_direct))
            .unwrap_or_default();
        blocks.insert(id, (categories, values));
    }

    let Some(chart) = root.children().find(|n| is_element(*n, "chart")) else {
        return data;
    };
    if let Some(title) = chart.children().find(|n| is_element(*n, "title")) {
        data.title = parse_chart_title(title);
    }

    let mut have_categories = false;
    for series in chart.descendants().filter(|n| is_element(*n, "series")) {
        let name = series_name(series);
        let data_id = series
            .children()
            .find(|n| is_element(*n, "dataId"))
            .and_then(|n| attr(n, "val"));
        let (categories, values) =
            data_id.and_then(|id| blocks.get(id)).cloned().unwrap_or_default();

        if !have_categories && !categories.is_empty() {
            data.categories = categories;
            have_categories = true;
        }
        data.series.push(ChartSeries { name, values });
    }
    data
}

/// A series' display name — the first `v` (local name) found beneath its
/// `tx` child. Classic (`c:tx` wrapping a cached `c:strRef`/`c:strCache`/
/// `c:pt`, or a literal `c:v` directly) and ChartEx (`cx:tx`'s `cx:txData`
/// wrapping a `cx:v` directly) nest this differently, but both land on a `v`
/// element at some depth beneath `tx`, so one lookup covers both.
fn series_name(ser: Node) -> Option<EcoString> {
    ser.children()
        .find(|n| is_element(*n, "tx"))
        .and_then(|tx| tx.descendants().find(|n| is_element(*n, "v")))
        .and_then(|v| v.text())
        .map(EcoString::from)
}

/// Concatenate every `a:t` run beneath a chart's `title` element, in
/// document order — real chart titles split across several runs just like a
/// Word paragraph's text does (`"my chart"` + `" looks nice"` in the POI
/// corpus's `chartex.docx`), so only the concatenation is meaningful, not any
/// single run alone. `None` if the title carries no text at all.
fn parse_chart_title(title: Node) -> Option<EcoString> {
    let mut out = EcoString::new();
    for t in title.descendants().filter(|n| is_element(*n, "t")) {
        out.push_str(t.text().unwrap_or_default());
    }
    (!out.is_empty()).then_some(out)
}

/// `parent`'s `child_name` child (`c:cat`/`c:val`), collected via
/// [`collect_indexed_pts`] — or an empty `Vec` if `parent` has no such child
/// at all (e.g. a chart type with no category axis).
fn indexed_points_under<'a>(
    parent: Node<'a, 'a>,
    child_name: &str,
    text_of: impl Fn(Node<'a, 'a>) -> Option<EcoString>,
) -> Vec<EcoString> {
    parent
        .children()
        .find(|n| is_element(*n, child_name))
        .map(|container| collect_indexed_pts(container, text_of))
        .unwrap_or_default()
}

/// Collect every `pt`/`idx` point found anywhere beneath `container` into a
/// dense, zero-based `Vec` sized to the largest `idx` seen. Word's cached
/// category/value lists are sparse — `idx` skips indices with no data (a
/// hidden or filtered row) — so pushing each `pt` in document order instead
/// of placing it at its own `idx` would silently misalign a series' values
/// against the wrong categories the moment one index is missing. A point
/// with no parsable `idx` is skipped; any index with no point at all is
/// filled with an empty string. `text_of` extracts one point's text — nested
/// under a `v` child for a classic chart's `c:pt`, or directly on the
/// element itself for a ChartEx `cx:pt` — see the two call sites.
fn collect_indexed_pts<'a>(
    container: Node<'a, 'a>,
    text_of: impl Fn(Node<'a, 'a>) -> Option<EcoString>,
) -> Vec<EcoString> {
    let mut points: Vec<(usize, EcoString)> = Vec::new();
    let mut max_idx = 0usize;
    for pt in container.descendants().filter(|n| is_element(*n, "pt")) {
        let Some(idx) = attr(pt, "idx").and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        max_idx = max_idx.max(idx);
        points.push((idx, text_of(pt).unwrap_or_default()));
    }
    if points.is_empty() {
        return Vec::new();
    }
    let mut out = vec![EcoString::new(); max_idx + 1];
    for (idx, text) in points {
        out[idx] = text;
    }
    out
}

/// A classic chart's `c:pt`: text lives on a nested `c:v` child.
fn pt_text_nested_v(pt: Node) -> Option<EcoString> {
    pt.children().find(|n| is_element(*n, "v")).and_then(|v| v.text()).map(EcoString::from)
}

/// A ChartEx `cx:pt`: text sits directly on the point element itself.
fn pt_text_direct(pt: Node) -> Option<EcoString> {
    pt.text().map(EcoString::from)
}

// --- Sections (`w:sectPr`) -----------------------------------------------------

fn parse_sectpr(node: Node) -> SectPr {
    let mut sect = SectPr::default();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "pgSz" => {
                sect.page_w = attr(child, "w").and_then(parse_i64);
                sect.page_h = attr(child, "h").and_then(parse_i64);
                sect.landscape = attr(child, "orient") == Some("landscape");
            }
            "pgMar" => {
                sect.margin_top = attr(child, "top").and_then(parse_i64);
                sect.margin_bottom = attr(child, "bottom").and_then(parse_i64);
                sect.margin_left = attr(child, "left").and_then(parse_i64);
                sect.margin_right = attr(child, "right").and_then(parse_i64);
            }
            "headerReference" => {
                if let Some(r) = parse_furniture_ref(child) {
                    sect.header_refs.push(r);
                }
            }
            "footerReference" => {
                if let Some(r) = parse_furniture_ref(child) {
                    sect.footer_refs.push(r);
                }
            }
            "titlePg" => sect.title_pg = true,
            _ => {}
        }
    }
    sect
}

/// `<w:headerReference w:type="..." r:id="..."/>` (and the `footerReference`
/// counterpart, same shape) → a [`FurnitureRef`]. `None` if there's no
/// `r:id` — a reference with nothing to resolve is not worth keeping.
fn parse_furniture_ref(node: Node) -> Option<FurnitureRef> {
    let rel_id = attr_ns(node, ns::R, "id")?.into();
    // Word only ever emits "default"/"first"/"even", but an absent or
    // unrecognized `w:type` (a hand-edited or foreign-producer document)
    // falls back to `Default` rather than dropping the reference.
    let kind = match attr(node, "type") {
        Some("first") => FurnitureKind::First,
        Some("even") => FurnitureKind::Even,
        _ => FurnitureKind::Default,
    };
    Some(FurnitureRef { kind, rel_id })
}

// --- word/styles.xml -----------------------------------------------------------

fn parse_styles(xml: &str) -> Result<Styles, ImportError> {
    let document = Document::parse(xml).map_err(xml_err)?;
    let root = document.root_element();
    let mut styles = Styles::default();
    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "docDefaults" => parse_doc_defaults(child, &mut styles),
            "style" => {
                let style = parse_style(child);
                styles.by_id.insert(style.id.clone(), style);
            }
            _ => {}
        }
    }
    Ok(styles)
}

fn parse_doc_defaults(node: Node, styles: &mut Styles) {
    for defaults in node.children().filter(|n| n.is_element()) {
        match defaults.tag_name().name() {
            "rPrDefault" => {
                if let Some(rpr) = defaults.children().find(|n| is_element(*n, "rPr")) {
                    styles.default_run = parse_run_props(rpr);
                }
            }
            "pPrDefault" => {
                if let Some(ppr) = defaults.children().find(|n| is_element(*n, "pPr")) {
                    styles.default_para = parse_para_props(ppr);
                }
            }
            _ => {}
        }
    }
}

fn parse_style(node: Node) -> Style {
    let mut style = Style {
        id: attr(node, "styleId").unwrap_or_default().into(),
        kind: match attr(node, "type") {
            Some("character") => StyleKind::Character,
            Some("table") => StyleKind::Table,
            Some("numbering") => StyleKind::Numbering,
            _ => StyleKind::Paragraph,
        },
        ..Style::default()
    };
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "name" => style.name = attr(child, "val").map(EcoString::from),
            "basedOn" => style.based_on = attr(child, "val").map(EcoString::from),
            "rPr" => style.run = parse_run_props(child),
            "pPr" => {
                style.outline_level = child
                    .children()
                    .find(|n| is_element(*n, "outlineLvl"))
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| v.parse::<u8>().ok());
                style.para = parse_para_props(child);
            }
            _ => {}
        }
    }
    style
}

// --- word/numbering.xml ---------------------------------------------------------

fn parse_numbering(xml: &str) -> Result<Numbering, ImportError> {
    let document = Document::parse(xml).map_err(xml_err)?;
    let root = document.root_element();
    let mut numbering = Numbering::default();
    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "abstractNum" => {
                let Some(abstract_num_id) =
                    attr(child, "abstractNumId").and_then(parse_i64)
                else {
                    continue;
                };
                let mut levels = FxHashMap::default();
                for lvl in child.children().filter(|n| is_element(*n, "lvl")) {
                    let Some(ilvl) = attr(lvl, "ilvl").and_then(parse_i64) else {
                        continue;
                    };
                    let num_fmt = lvl
                        .children()
                        .find(|n| is_element(*n, "numFmt"))
                        .and_then(|n| attr(n, "val"))
                        .unwrap_or("decimal")
                        .into();
                    levels.insert(ilvl, LevelFormat { num_fmt });
                }
                numbering.abstract_nums.insert(abstract_num_id, levels);
            }
            "num" => {
                let Some(num_id) = attr(child, "numId").and_then(parse_i64) else {
                    continue;
                };
                if let Some(abstract_num_id) = child
                    .children()
                    .find(|n| is_element(*n, "abstractNumId"))
                    .and_then(|n| attr(n, "val"))
                    .and_then(parse_i64)
                {
                    numbering.instances.insert(num_id, abstract_num_id);
                }
            }
            _ => {}
        }
    }
    Ok(numbering)
}

// --- Small XML helpers ---------------------------------------------------------

fn is_element(node: Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

/// Attribute lookup by local name only — namespace prefixes are a red
/// herring across real-world OOXML producers, and none of the local names
/// this importer reads collide across namespaces on the same element (see
/// [`attr_ns`] for the handful that could).
fn attr<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// Namespace-scoped attribute lookup, for `r:id`/`r:embed` — these share a
/// local name with unrelated attributes in other namespaces, so a plain
/// [`attr`] lookup isn't safe for them.
fn attr_ns<'a>(node: Node<'a, 'a>, namespace: &str, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace() == Some(namespace) && a.name() == name)
        .map(|a| a.value())
}

fn parse_i64(s: &str) -> Option<i64> {
    s.parse().ok()
}

/// The exact source slice for a node, byte-for-byte — used to capture a
/// `m:oMath` fragment (OMML) verbatim without a lossy XML re-serialization.
fn raw_xml(node: Node) -> EcoString {
    let source = node.document().input_text();
    source[node.range()].into()
}

fn xml_err(e: roxmltree::Error) -> ImportError {
    ImportError::Xml(eco_format!("{e}"))
}

#[cfg(test)]
mod tests {
    use typst_ooxml_core::opc::{Package, PackageOptions, RelMode, Rels};

    use super::*;
    use crate::wml::model::{BreakType, RunContent, RunItem, StyleKind};

    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"
            xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
  <w:body>
    <w:p>
      <w:pPr>
        <w:pStyle w:val="Heading1"/>
        <w:jc w:val="center"/>
        <w:numPr><w:ilvl w:val="1"/><w:numId w:val="5"/></w:numPr>
        <w:spacing w:before="240" w:line="360"/>
        <w:ind w:left="720"/>
        <w:pBdr><w:bottom w:val="single" w:sz="4" w:color="auto"/></w:pBdr>
      </w:pPr>
      <w:r>
        <w:rPr><w:b/><w:i w:val="0"/><w:sz w:val="32"/><w:color w:val="FF0000"/><w:rFonts w:ascii="Arial"/></w:rPr>
        <w:t xml:space="preserve">Hello,  World</w:t>
      </w:r>
      <w:r><w:tab/></w:r>
      <w:r><w:br w:type="page"/></w:r>
      <w:hyperlink r:id="rId2">
        <w:r><w:t>link text</w:t></w:r>
      </w:hyperlink>
      <w:r>
        <w:drawing>
          <wp:inline>
            <wp:extent cx="914400" cy="457200"/>
            <wp:docPr id="1" name="pic" descr="a picture"/>
            <a:graphic><a:graphicData>
              <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
            </a:graphicData></a:graphic>
          </wp:inline>
        </w:drawing>
      </w:r>
      <m:oMath><m:r><m:t>x+y</m:t></m:r></m:oMath>
    </w:p>
    <w:tbl>
      <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid>
      <w:tr>
        <w:trPr><w:tblHeader/></w:trPr>
        <w:tc>
          <w:tcPr><w:gridSpan w:val="2"/><w:shd w:val="clear" w:color="auto" w:fill="CCCCCC"/></w:tcPr>
          <w:p><w:r><w:t>Header</w:t></w:r></w:p>
        </w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:sectPr>
      <w:pgSz w:w="12240" w:h="15840" w:orient="landscape"/>
      <w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
    </w:sectPr>
  </w:body>
</w:document>"#;

    const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults>
    <w:rPrDefault><w:rPr><w:sz w:val="22"/><w:i w:val="0"/></w:rPr></w:rPrDefault>
    <w:pPrDefault><w:pPr><w:jc w:val="both"/></w:pPr></w:pPrDefault>
  </w:docDefaults>
  <w:style w:type="paragraph" w:styleId="Heading1">
    <w:name w:val="heading 1"/>
    <w:basedOn w:val="Normal"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
  </w:style>
</w:styles>"#;

    const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="1">
    <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl>
    <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="5"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;

    /// Builds a minimal but structurally valid `.docx` in memory (via the OPC
    /// writer already used by the exporter, so the reader side gets a
    /// genuinely well-formed package) exercising every construct this parser
    /// handles, then feeds it back through [`parse_package`].
    fn build_docx() -> Vec<u8> {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });

        let mut doc_rels = Rels::new();
        let image_rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            "media/image1.png",
            RelMode::Internal,
        );
        let link_rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
            "https://example.com",
            RelMode::External,
        );
        assert_eq!(image_rid, "rId1");
        assert_eq!(link_rid, "rId2");

        package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
        package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
        package.add_xml("word/numbering.xml", "application/xml", NUMBERING_XML.into());
        package.add_media("word/media/image1.png", "png", "image/png", vec![0x89, 0x50, 0x4E, 0x47]);
        package.add_relationships("word/document.xml", &doc_rels).unwrap();

        package.finish(&Rels::new()).unwrap()
    }

    #[test]
    fn parses_a_full_document() {
        let docx = build_docx();
        let mut report = ImportReport::default();
        let package = parse_package(&docx, &mut report).unwrap();

        // -- rels + media --
        assert_eq!(package.rels["rId1"].target, "media/image1.png");
        assert!(!package.rels["rId1"].external);
        assert_eq!(package.rels["rId2"].target, "https://example.com");
        assert!(package.rels["rId2"].external);
        assert_eq!(
            package.media["word/media/image1.png"],
            vec![0x89, 0x50, 0x4E, 0x47]
        );

        // -- styles --
        assert_eq!(package.styles.default_run.size_half_pt, Some(22));
        assert_eq!(package.styles.default_run.italic, Some(false));
        assert_eq!(package.styles.default_para.jc.as_deref(), Some("both"));
        let heading = &package.styles.by_id["Heading1"];
        assert_eq!(heading.name.as_deref(), Some("heading 1"));
        assert_eq!(heading.based_on.as_deref(), Some("Normal"));
        assert_eq!(heading.kind, StyleKind::Paragraph);
        assert_eq!(heading.outline_level, Some(0));

        // -- numbering --
        assert_eq!(package.numbering.instances[&5], 1);
        assert!(package.numbering.is_ordered(5, 0)); // decimal
        assert!(!package.numbering.is_ordered(5, 1)); // bullet

        // -- body --
        assert_eq!(package.body.items.len(), 2);
        let sect = package.body.sect_pr.as_ref().unwrap();
        assert_eq!(sect.page_w, Some(12240));
        assert_eq!(sect.page_h, Some(15840));
        assert!(sect.landscape);
        assert_eq!(sect.margin_top, Some(1440));
        assert_eq!(sect.margin_right, Some(1440));
        assert_eq!(sect.margin_bottom, Some(1440));
        assert_eq!(sect.margin_left, Some(1440));

        let BodyItem::Paragraph(p) = &package.body.items[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(p.props.style_id.as_deref(), Some("Heading1"));
        assert_eq!(p.props.jc.as_deref(), Some("center"));
        let num = p.props.num.expect("expected a numPr");
        assert_eq!(num.num_id, 5);
        assert_eq!(num.ilvl, 1);
        assert_eq!(p.props.spacing_before, Some(240));
        assert_eq!(p.props.line, Some(360));
        assert_eq!(p.props.indent_left, Some(720));
        assert!(p.props.bottom_border);

        assert_eq!(p.runs.len(), 6);

        let RunItem::Run(r0) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r0.props.bold, Some(true));
        assert_eq!(r0.props.italic, Some(false));
        assert_eq!(r0.props.size_half_pt, Some(32));
        assert_eq!(r0.props.color.as_deref(), Some("FF0000"));
        assert_eq!(r0.props.font.as_deref(), Some("Arial"));
        assert_eq!(r0.content.len(), 1);
        let RunContent::Text(text) = &r0.content[0] else { panic!("expected text") };
        // Whitespace (the double space) must survive exactly.
        assert_eq!(text.as_str(), "Hello,  World");

        let RunItem::Run(r1) = &p.runs[1] else { panic!("expected a run") };
        assert!(matches!(r1.content.as_slice(), [RunContent::Tab]));

        let RunItem::Run(r2) = &p.runs[2] else { panic!("expected a run") };
        assert!(matches!(r2.content.as_slice(), [RunContent::Break(BreakType::Page)]));

        let RunItem::Hyperlink { rel_id, anchor, runs } = &p.runs[3] else {
            panic!("expected a hyperlink")
        };
        assert_eq!(rel_id.as_deref(), Some("rId2"));
        assert!(anchor.is_none());
        assert_eq!(runs.len(), 1);
        let RunItem::Run(hr0) = &runs[0] else { panic!("expected a run") };
        let RunContent::Text(text) = &hr0.content[0] else { panic!("expected text") };
        assert_eq!(text.as_str(), "link text");

        let RunItem::Run(r4) = &p.runs[4] else { panic!("expected a run") };
        let RunContent::Drawing(d) = &r4.content[0] else { panic!("expected a drawing") };
        assert_eq!(d.rel_id, "rId1");
        assert_eq!(d.cx_emu, Some(914400));
        assert_eq!(d.cy_emu, Some(457200));
        assert_eq!(d.alt.as_deref(), Some("a picture"));

        let RunItem::Run(r5) = &p.runs[5] else { panic!("expected a run") };
        let RunContent::Math(raw) = &r5.content[0] else { panic!("expected math") };
        assert!(raw.starts_with("<m:oMath>"));
        assert!(raw.contains("x+y"));

        let BodyItem::Table(table) = &package.body.items[1] else {
            panic!("expected a table");
        };
        assert_eq!(table.grid, vec![2000, 3000]);
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0].is_header);
        assert_eq!(table.rows[0].cells[0].grid_span, 2);
        assert_eq!(table.rows[0].cells[0].shd_fill.as_deref(), Some("CCCCCC"));
        assert_eq!(table.rows[1].cells[0].v_merge, Some(true));
        assert_eq!(table.rows[1].cells[0].grid_span, 1);
        assert_eq!(table.rows[1].cells[1].v_merge, None);
    }

    #[test]
    fn missing_document_xml_is_not_a_word_document() {
        let mut report = ImportReport::default();
        let empty_zip =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] })
                .finish(&Rels::new())
                .unwrap();
        assert!(matches!(
            parse_package(&empty_zip, &mut report),
            Err(ImportError::NotAWordDocument)
        ));
    }

    #[test]
    fn missing_optional_parts_are_empty_not_errors() {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(parsed.body.items.is_empty());
        assert!(parsed.styles.by_id.is_empty());
        assert!(parsed.numbering.instances.is_empty());
        assert!(parsed.rels.is_empty());
        assert!(parsed.media.is_empty());
    }

    // --- Fields ------------------------------------------------------------

    /// Parses a `<w:p>` built from `inner_xml` (paragraph content only, no
    /// `w:pPr`) and returns the resulting [`Paragraph`] — a lighter-weight rig
    /// than [`build_docx`] for tests that only care about run/field folding.
    fn parse_test_paragraph(inner_xml: &str) -> Paragraph {
        let xml = format!(
            r#"<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner_xml}</w:p>"#
        );
        let document = Document::parse(&xml).unwrap();
        parse_paragraph(document.root_element(), 0)
    }

    #[test]
    fn fld_simple_folds_into_a_field_with_its_cached_result() {
        let p = parse_test_paragraph(
            r#"<w:fldSimple w:instr=" FILENAME \* MERGEFORMAT ">
                 <w:r><w:t>report.docx</w:t></w:r>
               </w:fldSimple>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Field(field) = &p.runs[0] else { panic!("expected a field") };
        assert_eq!(field.instr.as_str(), " FILENAME \\* MERGEFORMAT ");
        assert_eq!(field.result.len(), 1);
        let RunItem::Run(r) = &field.result[0] else { panic!("expected a run") };
        let RunContent::Text(t) = &r.content[0] else { panic!("expected text") };
        assert_eq!(t.as_str(), "report.docx");
    }

    #[test]
    fn fld_char_sequence_folds_and_concatenates_instr_text_across_runs() {
        // The instruction is split " PA" + "GE " across two `w:instrText`
        // runs — Word routinely splits instructions arbitrarily.
        let p = parse_test_paragraph(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>
               <w:r><w:instrText xml:space="preserve"> PA</w:instrText></w:r>
               <w:r><w:instrText xml:space="preserve">GE </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r>
               <w:r><w:t>7</w:t></w:r>
               <w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Field(field) = &p.runs[0] else { panic!("expected a field") };
        assert_eq!(field.instr.as_str(), " PAGE ");
        assert_eq!(field.result.len(), 1);
        let RunItem::Run(r) = &field.result[0] else { panic!("expected a run") };
        let RunContent::Text(t) = &r.content[0] else { panic!("expected text") };
        assert_eq!(t.as_str(), "7");
    }

    #[test]
    fn a_run_carrying_a_fld_char_and_other_content_only_acts_as_the_marker() {
        // Real documents sometimes pack extra (ignorable) content onto the
        // marker run itself; only the `fldChar` should matter.
        let p = parse_test_paragraph(
            r#"<w:r><w:fldChar w:fldCharType="begin"/><w:t>ignored</w:t></w:r>
               <w:r><w:instrText> PAGE </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r>
               <w:r><w:t>1</w:t></w:r>
               <w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Field(field) = &p.runs[0] else { panic!("expected a field") };
        assert_eq!(field.instr.as_str(), " PAGE ");
    }

    #[test]
    fn unterminated_field_flushes_its_result_instead_of_losing_text() {
        // A `begin`/`separate` with no matching `end` before the paragraph
        // ends — the cached result text must still survive.
        let p = parse_test_paragraph(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>
               <w:r><w:instrText> AUTHOR </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r>
               <w:r><w:t>Jane Doe</w:t></w:r>"#,
        );

        // Flattened straight into the paragraph — no `RunItem::Field`
        // wrapper for a field that never actually closed.
        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run, not a field") };
        let RunContent::Text(t) = &r.content[0] else { panic!("expected text") };
        assert_eq!(t.as_str(), "Jane Doe");
    }

    #[test]
    fn nested_field_survives_inside_its_parents_result() {
        // A TOC entry whose cached result contains a HYPERLINK field — the
        // shape real documents produce for a clickable table of contents.
        let p = parse_test_paragraph(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>
               <w:r><w:instrText> TOC \o "1-3" \h </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r>
               <w:r><w:fldChar w:fldCharType="begin"/></w:r>
               <w:r><w:instrText> HYPERLINK "https://x" </w:instrText></w:r>
               <w:r><w:fldChar w:fldCharType="separate"/></w:r>
               <w:r><w:t>link text</w:t></w:r>
               <w:r><w:fldChar w:fldCharType="end"/></w:r>
               <w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Field(outer) = &p.runs[0] else { panic!("expected the outer field") };
        assert!(outer.instr.contains("TOC"));
        assert_eq!(outer.result.len(), 1);
        let RunItem::Field(inner) = &outer.result[0] else {
            panic!("expected the nested field")
        };
        assert!(inner.instr.contains("HYPERLINK"));
        let RunItem::Run(r) = &inner.result[0] else { panic!("expected a run") };
        let RunContent::Text(t) = &r.content[0] else { panic!("expected text") };
        assert_eq!(t.as_str(), "link text");
    }

    #[test]
    fn field_nesting_beyond_the_cap_does_not_grow_the_stack() {
        // MAX_FIELD_DEPTH+extra `begin`s with no `separate`/`end` at all —
        // must not panic, loop, or blow the stack; excess `begin`s beyond the
        // cap are treated as ordinary (here content-free) runs.
        let mut xml = String::new();
        for _ in 0..MAX_FIELD_DEPTH + 10 {
            xml.push_str(r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#);
        }
        let p = parse_test_paragraph(&xml);
        // Everything beyond the cap is dropped-through content, which is
        // empty here — nothing surfaces as paragraph-level `RunItem`s.
        assert!(p.runs.is_empty());
    }

    #[test]
    fn hyperlink_wrapping_a_field_folds_the_field_inside_it() {
        // A PAGEREF field inside a `w:hyperlink` — the common shape for an
        // internal cross-reference (the hyperlink supplies the jump target,
        // the field supplies the displayed page number).
        let p = parse_test_paragraph(
            r#"<w:hyperlink w:anchor="_Toc1">
                 <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                 <w:r><w:instrText> PAGEREF _Toc1 \h </w:instrText></w:r>
                 <w:r><w:fldChar w:fldCharType="separate"/></w:r>
                 <w:r><w:t>5</w:t></w:r>
                 <w:r><w:fldChar w:fldCharType="end"/></w:r>
               </w:hyperlink>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Hyperlink { anchor, runs, .. } = &p.runs[0] else {
            panic!("expected a hyperlink")
        };
        assert_eq!(anchor.as_deref(), Some("_Toc1"));
        assert_eq!(runs.len(), 1);
        let RunItem::Field(field) = &runs[0] else { panic!("expected a field") };
        assert_eq!(field.instr.as_str(), " PAGEREF _Toc1 \\h ");
    }

    // --- Headers / footers ---------------------------------------------------

    /// Parses a `<w:sectPr>` built from `inner_xml` and returns the resulting
    /// [`SectPr`] — mirrors [`parse_test_paragraph`] but for section
    /// properties.
    fn parse_test_sectpr(inner_xml: &str) -> SectPr {
        let xml = format!(
            r#"<w:sectPr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{inner_xml}</w:sectPr>"#
        );
        let document = Document::parse(&xml).unwrap();
        parse_sectpr(document.root_element())
    }

    #[test]
    fn sectpr_collects_header_and_footer_references_in_document_order() {
        // The shape `headerFooter.docx` (POI corpus) actually uses: all three
        // header types plus one footer type, no particular ordering.
        let sect = parse_test_sectpr(
            r#"<w:headerReference w:type="even" r:id="rId4"/>
               <w:headerReference w:type="default" r:id="rId5"/>
               <w:headerReference w:type="first" r:id="rId8"/>
               <w:footerReference w:type="default" r:id="rId7"/>
               <w:titlePg/>"#,
        );
        assert_eq!(sect.header_refs.len(), 3);
        assert_eq!(sect.header_refs[0].kind, FurnitureKind::Even);
        assert_eq!(sect.header_refs[0].rel_id.as_str(), "rId4");
        assert_eq!(sect.header_refs[1].kind, FurnitureKind::Default);
        assert_eq!(sect.header_refs[1].rel_id.as_str(), "rId5");
        assert_eq!(sect.header_refs[2].kind, FurnitureKind::First);
        assert_eq!(sect.footer_refs.len(), 1);
        assert_eq!(sect.footer_refs[0].kind, FurnitureKind::Default);
        assert!(sect.title_pg);
    }

    #[test]
    fn no_title_pg_and_no_header_references_is_the_default_sectpr() {
        let sect = parse_test_sectpr(r#"<w:pgSz w:w="12240" w:h="15840"/>"#);
        assert!(sect.header_refs.is_empty());
        assert!(sect.footer_refs.is_empty());
        assert!(!sect.title_pg);
    }

    #[test]
    fn header_reference_with_absent_or_unrecognized_type_defaults_to_default_kind() {
        let sect = parse_test_sectpr(
            r#"<w:headerReference r:id="rId1"/>
               <w:headerReference w:type="odd" r:id="rId2"/>"#,
        );
        assert_eq!(sect.header_refs.len(), 2);
        assert_eq!(sect.header_refs[0].kind, FurnitureKind::Default);
        assert_eq!(sect.header_refs[1].kind, FurnitureKind::Default);
    }

    #[test]
    fn header_reference_without_rid_is_dropped_not_kept_unresolved() {
        let sect = parse_test_sectpr(r#"<w:headerReference w:type="default"/>"#);
        assert!(sect.header_refs.is_empty());
    }

    #[test]
    fn ptab_lowers_to_a_tab_like_a_plain_wtab() {
        // `ThreeColHeadFoot.docx` (POI corpus) uses `w:ptab` — a *positional*
        // tab — for its three-column header, not `w:tab`.
        let p = parse_test_paragraph(
            r#"<w:r><w:t>Left</w:t></w:r>
               <w:r><w:ptab w:relativeTo="margin" w:alignment="center" w:leader="none"/></w:r>
               <w:r><w:t>Right</w:t></w:r>"#,
        );
        assert_eq!(p.runs.len(), 3);
        let RunItem::Run(r) = &p.runs[1] else { panic!("expected a run") };
        assert!(matches!(r.content.as_slice(), [RunContent::Tab]));
    }

    /// Builds a `.docx` shaped like `headerPic.docx` in the POI corpus: the
    /// document's own `rId1` means one thing (`styles.xml`), while the
    /// header part's *own* `rId1` — numbered independently, per
    /// `word/_rels/header1.xml.rels` — means something else entirely (an
    /// embedded image). Only a per-part-scoped merge resolves both correctly.
    fn build_docx_with_header_image_collision() -> Vec<u8> {
        const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:t>Body</w:t></w:r></w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:pgSz w:w="12240" w:h="15840"/>
    </w:sectPr>
  </w:body>
</w:document>"#;

        const HEADER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
       xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:p>
    <w:r>
      <w:drawing>
        <wp:inline>
          <wp:extent cx="914400" cy="457200"/>
          <wp:docPr id="1" name="pic"/>
          <a:graphic><a:graphicData>
            <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
          </a:graphicData></a:graphic>
        </wp:inline>
      </w:drawing>
    </w:r>
  </w:p>
</w:hdr>"#;

        const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });

        let mut doc_rels = Rels::new();
        let styles_rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
            "styles.xml",
            RelMode::Internal,
        );
        let header_rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
            "header1.xml",
            RelMode::Internal,
        );
        // The document's `rId1` means `styles.xml` — this is the id the
        // header's own (differently-scoped) `rId1` must not collide with.
        assert_eq!(styles_rid, "rId1");
        assert_eq!(header_rid, "rId2");

        let mut header_rels = Rels::new();
        let image_rid = header_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            "media/image1.png",
            RelMode::Internal,
        );
        assert_eq!(image_rid, "rId1");

        package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
        package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
        package.add_xml("word/header1.xml", "application/xml", HEADER_XML.into());
        package.add_media(
            "word/media/image1.png",
            "png",
            "image/png",
            vec![0x89, 0x50, 0x4E, 0x47],
        );
        package.add_relationships("word/document.xml", &doc_rels).unwrap();
        package.add_relationships("word/header1.xml", &header_rels).unwrap();

        package.finish(&Rels::new()).unwrap()
    }

    #[test]
    fn furniture_parts_get_their_own_namespaced_relationships() {
        let docx = build_docx_with_header_image_collision();
        let mut report = ImportReport::default();
        let package = parse_package(&docx, &mut report).unwrap();

        // The document-level `rId1` is untouched: still `styles.xml`.
        assert_eq!(package.rels["rId1"].target, "styles.xml");

        // The header part landed in the furniture map...
        let header_body =
            package.furniture.get("word/header1.xml").expect("header1.xml in furniture");
        let BodyItem::Paragraph(p) = &header_body.items[0] else {
            panic!("expected a paragraph")
        };
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        let RunContent::Drawing(d) = &r.content[0] else { panic!("expected a drawing") };

        // ...and its drawing's relationship id was rewritten to the
        // namespaced form, which resolves through the merged `rels` map to
        // the header's OWN `rId1` target (the image) — not the document's.
        assert_eq!(d.rel_id.as_str(), "word/header1.xml!rId1");
        assert_eq!(package.rels[d.rel_id.as_str()].target, "media/image1.png");
    }

    #[test]
    fn footer_part_without_a_numeric_suffix_is_discovered() {
        // `Bug60341.docx` (POI corpus) names its footer part `footer.xml`,
        // not `footer1.xml`.
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        package.add_xml(
            "word/footer.xml",
            "application/xml",
            "<w:ftr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:p><w:r><w:t>F</w:t></w:r></w:p></w:ftr>"
                .into(),
        );
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(parsed.furniture.contains_key("word/footer.xml"));
    }

    #[test]
    fn even_and_odd_headers_read_from_settings_xml() {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        package.add_xml(
            "word/settings.xml",
            "application/xml",
            "<w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:evenAndOddHeaders/></w:settings>"
                .into(),
        );
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(parsed.even_and_odd_headers);
    }

    #[test]
    fn missing_settings_xml_defaults_even_and_odd_headers_to_false() {
        let docx = build_docx();
        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(!parsed.even_and_odd_headers);
    }

    // --- Footnotes / endnotes -------------------------------------------------

    #[test]
    fn footnote_and_endnote_references_parse_with_their_ids() {
        let p = parse_test_paragraph(
            r#"<w:r><w:footnoteReference w:id="1"/></w:r>
               <w:r><w:endnoteReference w:id="2"/></w:r>"#,
        );
        assert_eq!(p.runs.len(), 2);
        let RunItem::Run(r0) = &p.runs[0] else { panic!("expected a run") };
        assert!(matches!(
            r0.content.as_slice(),
            [RunContent::NoteRef { endnote: false, id: 1 }]
        ));
        let RunItem::Run(r1) = &p.runs[1] else { panic!("expected a run") };
        assert!(matches!(
            r1.content.as_slice(),
            [RunContent::NoteRef { endnote: true, id: 2 }]
        ));
    }

    #[test]
    fn note_reference_without_an_id_is_dropped_not_kept_unresolved() {
        let p = parse_test_paragraph(r#"<w:r><w:footnoteReference/></w:r>"#);
        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert!(r.content.is_empty());
    }

    /// `w:footnoteRef`/`w:endnoteRef` are the *number placeholder* inside a
    /// note's own body — Typst renumbers footnotes itself, so these must
    /// never surface as stray text.
    #[test]
    fn footnote_ref_and_endnote_ref_placeholders_produce_no_content() {
        let p = parse_test_paragraph(
            r#"<w:r><w:footnoteRef/></w:r><w:r><w:endnoteRef/></w:r>"#,
        );
        for run_item in &p.runs {
            let RunItem::Run(r) = run_item else { panic!("expected a run") };
            assert!(r.content.is_empty(), "placeholder leaked content: {r:?}");
        }
    }

    const FOOTNOTES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
  <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>
  <w:footnote w:id="1"><w:p><w:r><w:t>snoska</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#;

    const ENDNOTES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:endnote>
  <w:endnote w:id="1"><w:p><w:r><w:t>end note text</w:t></w:r></w:p></w:endnote>
</w:endnotes>"#;

    fn build_docx_with_notes() -> Vec<u8> {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        package.add_xml("word/footnotes.xml", "application/xml", FOOTNOTES_XML.into());
        package.add_xml("word/endnotes.xml", "application/xml", ENDNOTES_XML.into());
        package.finish(&Rels::new()).unwrap()
    }

    #[test]
    fn footnotes_and_endnotes_parts_exclude_boilerplate_separators() {
        let docx = build_docx_with_notes();
        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();

        // Only the real note (id 1) survives; the separator (-1) and
        // continuation-separator (0) are Word's own rule-line furniture.
        assert_eq!(parsed.footnotes.len(), 1);
        assert!(!parsed.footnotes.contains_key(&-1));
        assert!(!parsed.footnotes.contains_key(&0));
        let body = &parsed.footnotes[&1];
        let BodyItem::Paragraph(p) = &body.items[0] else { panic!("expected a paragraph") };
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert!(matches!(&r.content[0], RunContent::Text(t) if t == "snoska"));

        assert_eq!(parsed.endnotes.len(), 1);
        assert!(!parsed.endnotes.contains_key(&-1));
        let body = &parsed.endnotes[&1];
        let BodyItem::Paragraph(p) = &body.items[0] else { panic!("expected a paragraph") };
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert!(matches!(&r.content[0], RunContent::Text(t) if t == "end note text"));
    }

    #[test]
    fn missing_footnotes_and_endnotes_parts_are_empty_not_errors() {
        let docx = build_docx();
        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(parsed.footnotes.is_empty());
        assert!(parsed.endnotes.is_empty());
    }

    #[test]
    fn footnote_without_a_parsable_id_is_dropped() {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        package.add_xml(
            "word/footnotes.xml",
            "application/xml",
            "<w:footnotes xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:footnote><w:p><w:r><w:t>x</w:t></w:r></w:p></w:footnote></w:footnotes>"
                .into(),
        );
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert!(parsed.footnotes.is_empty());
    }

    /// Footnotes/endnotes can carry images, numbered independently of
    /// `document.xml` (`word/_rels/footnotes.xml.rels` starts back at
    /// `rId1`) — same collision shape as `headerPic.docx` for headers, so the
    /// same per-part namespacing must apply.
    #[test]
    fn footnote_image_resolves_through_the_parts_own_relationships() {
        const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body/></w:document>"#;

        const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

        const FOOTNOTES_WITH_IMAGE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
             xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
             xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
             xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
             xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:footnote w:id="1">
    <w:p>
      <w:r>
        <w:drawing>
          <wp:inline>
            <wp:extent cx="914400" cy="457200"/>
            <wp:docPr id="1" name="pic"/>
            <a:graphic><a:graphicData>
              <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
            </a:graphicData></a:graphic>
          </wp:inline>
        </w:drawing>
      </w:r>
    </w:p>
  </w:footnote>
</w:footnotes>"#;

        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });

        let mut doc_rels = Rels::new();
        let styles_rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
            "styles.xml",
            RelMode::Internal,
        );
        assert_eq!(styles_rid, "rId1");

        let mut footnotes_rels = Rels::new();
        let image_rid = footnotes_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            "media/image1.png",
            RelMode::Internal,
        );
        assert_eq!(image_rid, "rId1");

        package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
        package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
        package.add_xml(
            "word/footnotes.xml",
            "application/xml",
            FOOTNOTES_WITH_IMAGE_XML.into(),
        );
        package.add_media("word/media/image1.png", "png", "image/png", vec![0x89, 0x50, 0x4E, 0x47]);
        package.add_relationships("word/document.xml", &doc_rels).unwrap();
        package.add_relationships("word/footnotes.xml", &footnotes_rels).unwrap();
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let package = parse_package(&docx, &mut report).unwrap();

        // The document-level `rId1` is untouched: still `styles.xml`.
        assert_eq!(package.rels["rId1"].target, "styles.xml");

        let body = &package.footnotes[&1];
        let BodyItem::Paragraph(p) = &body.items[0] else { panic!("expected a paragraph") };
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        let RunContent::Drawing(d) = &r.content[0] else { panic!("expected a drawing") };

        assert_eq!(d.rel_id.as_str(), "word/footnotes.xml!rId1");
        assert_eq!(package.rels[d.rel_id.as_str()].target, "media/image1.png");
    }

    // --- Text boxes / shapes ------------------------------------------------

    /// Concatenates every `w:t` found in a `Vec<BodyItem>` (a text box's
    /// parsed content) — enough to assert on the text a test's fixture
    /// carries without writing out the full `BodyItem`/`RunItem` match by
    /// hand at every call site.
    fn body_items_text(items: &[BodyItem]) -> String {
        let mut out = String::new();
        for item in items {
            let BodyItem::Paragraph(p) = item else { continue };
            for run_item in &p.runs {
                let RunItem::Run(r) = run_item else { continue };
                for c in &r.content {
                    if let RunContent::Text(s) = c {
                        out.push_str(s);
                    }
                }
            }
        }
        out
    }

    /// The hazard the task exists to fix: Word writes a modern text box
    /// *twice* — once as `wps:txbx` inside `mc:Choice`, once as its VML
    /// equivalent `v:textbox` inside `mc:Fallback` — both holding the same
    /// content, and the wrapper sits *inside* the run that hosts the
    /// drawing. Per the MCE spec, only the `mc:Choice` branch is honored;
    /// the `mc:Fallback` text must not surface at all, and the `mc:Choice`
    /// text must not be duplicated.
    #[test]
    fn mc_choice_and_fallback_text_boxes_are_deduplicated_by_taking_the_choice() {
        let p = parse_test_paragraph(
            r#"<w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
                 <mc:Choice Requires="wps">
                   <w:drawing xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                     <wps:wsp><wps:txbx><w:txbxContent>
                       <w:p><w:r><w:t>CHOICE TEXT</w:t></w:r></w:p>
                     </w:txbxContent></wps:txbx></wps:wsp>
                   </w:drawing>
                 </mc:Choice>
                 <mc:Fallback>
                   <w:pict xmlns:v="urn:schemas-microsoft-com:vml">
                     <v:shape><v:textbox><w:txbxContent>
                       <w:p><w:r><w:t>FALLBACK TEXT</w:t></w:r></w:p>
                     </w:txbxContent></v:textbox></v:shape>
                   </w:pict>
                 </mc:Fallback>
               </mc:AlternateContent></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 1, "the fallback must not add a second item: {:?}", r.content);
        let RunContent::TextBox(items) = &r.content[0] else { panic!("expected a text box") };
        assert_eq!(body_items_text(items), "CHOICE TEXT");
    }

    /// The VML-only spelling (`w:pict`/`v:textbox`), with no `mc:Choice` in
    /// sight at all — an older document, or one that never went through
    /// modern Word's MCE dance.
    #[test]
    fn vml_only_pict_text_box_is_parsed() {
        let p = parse_test_paragraph(
            r#"<w:r><w:pict xmlns:v="urn:schemas-microsoft-com:vml">
                 <v:shape><v:textbox><w:txbxContent>
                   <w:p><w:r><w:t>VML box text</w:t></w:r></w:p>
                 </w:txbxContent></v:textbox></v:shape>
               </w:pict></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        let RunContent::TextBox(items) = &r.content[0] else { panic!("expected a text box") };
        assert_eq!(body_items_text(items), "VML box text");
    }

    /// A real VML *picture* (no text box at all) must still produce nothing
    /// for this run, exactly as before the `w:pict` arm existed.
    #[test]
    fn vml_picture_without_a_text_box_produces_no_content() {
        let p = parse_test_paragraph(
            r#"<w:r><w:pict xmlns:v="urn:schemas-microsoft-com:vml">
                 <v:shape><v:imagedata/></v:shape>
               </w:pict></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert!(r.content.is_empty(), "a text-less VML picture shouldn't produce content");
    }

    /// A bare `mc:AlternateContent` with only an `mc:Fallback` — no
    /// `mc:Choice` at all. Per [`splice_node`]'s doc comment, the fallback is
    /// used whenever there's no choice to prefer over it. Exercised at the
    /// paragraph-children level (wrapping a whole `w:r`, not a drawing inside
    /// one) to cover the general wrapper mechanism, not just the text-box
    /// case built on top of it.
    #[test]
    fn alternate_content_with_only_a_fallback_uses_it() {
        let p = parse_test_paragraph(
            r#"<mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
                 <mc:Fallback>
                   <w:r><w:t>FALLBACK ONLY</w:t></w:r>
                 </mc:Fallback>
               </mc:AlternateContent>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        let RunContent::Text(t) = &r.content[0] else { panic!("expected text") };
        assert_eq!(t.as_str(), "FALLBACK ONLY");
    }

    /// A `w:drawing` wrapping a *group* of two shapes side by side, each its
    /// own text box — the shape `shapes-with-text.docx` in the POI corpus
    /// actually uses (one `w:drawing` grouping "A group of shapes" and
    /// "Where some contain text" as two separate `wps:wsp`s). Both must
    /// come back, not just the first found.
    #[test]
    fn a_grouped_drawing_recovers_every_sibling_shapes_text_box() {
        let p = parse_test_paragraph(
            r#"<w:r><w:drawing xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"
                              xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                 <wpg:wgp>
                   <wps:wsp><wps:txbx><w:txbxContent>
                     <w:p><w:r><w:t>first shape</w:t></w:r></w:p>
                   </w:txbxContent></wps:txbx></wps:wsp>
                   <wps:wsp><wps:txbx><w:txbxContent>
                     <w:p><w:r><w:t>second shape</w:t></w:r></w:p>
                   </w:txbxContent></wps:txbx></wps:wsp>
                 </wpg:wgp>
               </w:drawing></w:r>"#,
        );

        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 2, "expected two separate text boxes: {:?}", r.content);
        let RunContent::TextBox(a) = &r.content[0] else { panic!("expected a text box") };
        let RunContent::TextBox(b) = &r.content[1] else { panic!("expected a text box") };
        assert_eq!(body_items_text(a), "first shape");
        assert_eq!(body_items_text(b), "second shape");
    }

    /// A drawing with both an `a:blip` and a `w:txbxContent` — a picture
    /// with a caption box. The image wins; the caption's text is not
    /// separately recovered (the "don't contort the code for it" case the
    /// task calls out explicitly).
    #[test]
    fn drawing_with_both_blip_and_text_box_prefers_the_image() {
        let p = parse_test_paragraph(
            r#"<w:r><w:drawing
                  xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                  xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"
                  xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
                  xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">
                <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
                <wps:wsp><wps:txbx><w:txbxContent>
                  <w:p><w:r><w:t>Caption</w:t></w:r></w:p>
                </w:txbxContent></wps:txbx></wps:wsp>
              </w:drawing></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 1);
        assert!(
            matches!(&r.content[0], RunContent::Drawing(_)),
            "expected the image to win, got {:?}",
            r.content[0]
        );
    }

    /// A text box whose content is a drawing that is itself another text
    /// box, nested well past [`MAX_TEXTBOX_DEPTH`]. Must terminate promptly
    /// — not hang or overflow the stack — with the outer levels' text kept
    /// and everything past the cap simply dropped.
    #[test]
    fn nested_text_boxes_terminate_at_the_depth_cap() {
        /// One level of "a paragraph whose run holds a drawing that is a
        /// text box containing another such paragraph", `remaining` levels
        /// deep, bottoming out in a plain, driving-distance-away marker.
        fn nested_level(remaining: usize) -> String {
            if remaining == 0 {
                return "<w:p><w:r><w:t>innermost</w:t></w:r></w:p>".to_string();
            }
            format!(
                r#"<w:p><w:r><w:t>level{remaining}</w:t></w:r><w:r><w:drawing xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></w:drawing></w:r></w:p>"#,
                nested_level(remaining - 1)
            )
        }

        let depth = MAX_TEXTBOX_DEPTH + 5;
        let xml = format!(
            r#"<w:r><w:drawing xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></w:drawing></w:r>"#,
            nested_level(depth)
        );

        // Reaching this line at all demonstrates termination.
        let p = parse_test_paragraph(&xml);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        let RunContent::TextBox(items) = &r.content[0] else { panic!("expected a text box") };
        let text = body_items_text(items);

        assert!(
            text.contains(&format!("level{depth}")),
            "expected the outermost nested level to survive:\n{text}"
        );
        assert!(!text.contains("innermost"), "content past the depth cap should be dropped:\n{text}");
    }

    // --- Charts --------------------------------------------------------------

    /// A `w:drawing` with a `c:chart` reference and neither a blip nor a text
    /// box must parse as a chart — checked last in `parse_run`'s "drawing"
    /// arm, after both of those have come up empty (see
    /// `drawing_with_both_blip_and_text_box_prefers_the_image` above for the
    /// image-wins half of that ordering).
    #[test]
    fn drawing_with_a_chart_reference_and_no_blip_or_text_box_parses_as_a_chart() {
        let p = parse_test_paragraph(
            r#"<w:r><w:drawing
                  xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                  xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
                  xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
                  <c:chart r:id="rId5"/>
                </a:graphicData></a:graphic>
              </w:drawing></w:r>"#,
        );
        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 1);
        let RunContent::Chart(rel_id) = &r.content[0] else {
            panic!("expected a chart, got {:?}", r.content[0])
        };
        assert_eq!(rel_id.as_str(), "rId5");
    }

    /// Parses `inner` as the content of a `c:chartSpace` root (namespaces for
    /// both the classic and ChartEx spellings pre-declared, so a test's inner
    /// XML can use either `c:`/`a:` or `cx:` elements without redeclaring
    /// anything) and returns the resulting [`ChartData`] — mirrors
    /// [`parse_test_paragraph`]/[`parse_test_sectpr`]'s "just the fragment
    /// this test needs" rig.
    fn parse_test_chart_space(inner: &str) -> ChartData {
        let xml = format!(
            r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
                              xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
                              xmlns:cx="http://schemas.microsoft.com/office/drawing/2014/chartex">{inner}</c:chartSpace>"#
        );
        let document = Document::parse(&xml).unwrap();
        parse_chart_space(document.root_element())
    }

    /// The exact shape `chartex.docx` (POI corpus) uses: a title split across
    /// two `a:r`/`a:t` runs ("my chart" + " looks nice"), one series, and a
    /// dense category/value axis — reproduces the task's own worked example.
    #[test]
    fn chart_title_splits_across_runs_and_categories_values_align_by_idx() {
        let data = parse_test_chart_space(
            r#"<c:chart>
                 <c:title><c:tx><c:rich>
                   <a:p><a:r><a:t>my chart</a:t></a:r><a:r><a:t> looks nice</a:t></a:r></a:p>
                 </c:rich></c:tx></c:title>
                 <c:plotArea><c:barChart><c:ser>
                   <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Series 1</c:v></c:pt></c:strCache></c:strRef></c:tx>
                   <c:cat><c:strRef><c:strCache>
                     <c:pt idx="0"><c:v>Category 1</c:v></c:pt>
                     <c:pt idx="1"><c:v>Category 2</c:v></c:pt>
                     <c:pt idx="2"><c:v>Category 3</c:v></c:pt>
                     <c:pt idx="3"><c:v>Category 4</c:v></c:pt>
                   </c:strCache></c:strRef></c:cat>
                   <c:val><c:numRef><c:numCache>
                     <c:pt idx="0"><c:v>4.3</c:v></c:pt>
                     <c:pt idx="1"><c:v>2.5</c:v></c:pt>
                     <c:pt idx="2"><c:v>3.5</c:v></c:pt>
                     <c:pt idx="3"><c:v>4.5</c:v></c:pt>
                   </c:numCache></c:numRef></c:val>
                 </c:ser></c:barChart></c:plotArea>
               </c:chart>"#,
        );

        assert_eq!(data.title.as_deref(), Some("my chart looks nice"));
        assert_eq!(data.categories, vec!["Category 1", "Category 2", "Category 3", "Category 4"]);
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].name.as_deref(), Some("Series 1"));
        assert_eq!(data.series[0].values, vec!["4.3", "2.5", "3.5", "4.5"]);
    }

    /// The one most likely to be wrong (per the task): a series whose cached
    /// values skip indices (`idx="0"` and `idx="2"`, no `idx="1"`) must land
    /// each value at its *own* index, not at its position in document order.
    /// A naive "append each `c:pt` as found" reading would zip `[10, 30]`
    /// against the first two categories (`Category 1` → 10, `Category 2` →
    /// 30); the correct, `idx`-aware reading instead places 10 at index 0 and
    /// 30 at index 2, leaving index 1 empty — this is what tells the two
    /// apart.
    #[test]
    fn sparse_idx_values_land_at_their_own_index_not_at_append_position() {
        let data = parse_test_chart_space(
            r#"<c:chart><c:plotArea><c:barChart><c:ser>
                 <c:cat><c:strRef><c:strCache>
                   <c:pt idx="0"><c:v>Category 1</c:v></c:pt>
                   <c:pt idx="1"><c:v>Category 2</c:v></c:pt>
                   <c:pt idx="2"><c:v>Category 3</c:v></c:pt>
                   <c:pt idx="3"><c:v>Category 4</c:v></c:pt>
                 </c:strCache></c:strRef></c:cat>
                 <c:val><c:numRef><c:numCache>
                   <c:pt idx="0"><c:v>10</c:v></c:pt>
                   <c:pt idx="2"><c:v>30</c:v></c:pt>
                 </c:numCache></c:numRef></c:val>
               </c:ser></c:barChart></c:plotArea></c:chart>"#,
        );

        assert_eq!(data.categories, vec!["Category 1", "Category 2", "Category 3", "Category 4"]);
        assert_eq!(
            data.series[0].values,
            vec!["10", "", "30"],
            "expected the gap at index 1 to survive, not a naive append: {:?}",
            data.series[0].values
        );
    }

    /// The ChartEx shape (`cx:chartData`/`cx:chart`, distinct from the
    /// classic `c:chart`/`c:ser` shape above): a series links to its data
    /// via `cx:dataId` rather than nesting `cx:cat`/`cx:val` inside itself,
    /// and two series backed by two different `cx:data` blocks must each get
    /// their own values while sharing the category axis both blocks declare
    /// — the shape `chartex.docx`'s box-and-whisker charts actually use.
    #[test]
    fn chartex_series_resolve_their_values_through_their_data_id() {
        let data = parse_test_chart_space(
            r#"<cx:chartData>
                 <cx:data id="0">
                   <cx:strDim type="cat"><cx:lvl ptCount="2">
                     <cx:pt idx="0">Category 1</cx:pt><cx:pt idx="1">Category 2</cx:pt>
                   </cx:lvl></cx:strDim>
                   <cx:numDim type="val"><cx:lvl ptCount="2">
                     <cx:pt idx="0">-7</cx:pt><cx:pt idx="1">11</cx:pt>
                   </cx:lvl></cx:numDim>
                 </cx:data>
                 <cx:data id="1">
                   <cx:strDim type="cat"><cx:lvl ptCount="2">
                     <cx:pt idx="0">Category 1</cx:pt><cx:pt idx="1">Category 2</cx:pt>
                   </cx:lvl></cx:strDim>
                   <cx:numDim type="val"><cx:lvl ptCount="2">
                     <cx:pt idx="0">-3</cx:pt><cx:pt idx="1">34</cx:pt>
                   </cx:lvl></cx:numDim>
                 </cx:data>
               </cx:chartData>
               <cx:chart>
                 <cx:title><cx:tx><cx:rich>
                   <a:p><a:r><a:t>this is a box and whisker chart</a:t></a:r></a:p>
                 </cx:rich></cx:tx></cx:title>
                 <cx:plotArea><cx:plotAreaRegion>
                   <cx:series layoutId="boxWhisker">
                     <cx:tx><cx:txData><cx:v>Series1</cx:v></cx:txData></cx:tx>
                     <cx:dataId val="0"/>
                   </cx:series>
                   <cx:series layoutId="boxWhisker">
                     <cx:tx><cx:txData><cx:v>Series2</cx:v></cx:txData></cx:tx>
                     <cx:dataId val="1"/>
                   </cx:series>
                 </cx:plotAreaRegion></cx:plotArea>
               </cx:chart>"#,
        );

        assert_eq!(data.title.as_deref(), Some("this is a box and whisker chart"));
        assert_eq!(data.categories, vec!["Category 1", "Category 2"]);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name.as_deref(), Some("Series1"));
        assert_eq!(data.series[0].values, vec!["-7", "11"]);
        assert_eq!(data.series[1].name.as_deref(), Some("Series2"));
        assert_eq!(data.series[1].values, vec!["-3", "34"]);
    }

    /// An empty chart part (no title, no `c:chart` at all) must parse to an
    /// empty [`ChartData`] rather than panicking — `mappers::chart` is the
    /// one that decides to skip it entirely; this only proves parsing itself
    /// degrades cleanly.
    #[test]
    fn chart_space_with_no_chart_element_parses_to_empty_chart_data() {
        let data = parse_test_chart_space("");
        assert!(data.title.is_none());
        assert!(data.categories.is_empty());
        assert!(data.series.is_empty());
    }

    /// `word/charts/` also holds a chart's color/style siblings
    /// (`colorsN.xml`/`styleN.xml`), which end in `.xml` and live in the same
    /// directory but are not chart parts at all (their root is `cs:
    /// colorStyle`/`cs:chartStyle`, not `chartSpace`) — only the latter must
    /// end up in [`WmlPackage::charts`].
    #[test]
    fn chart_parts_are_recognized_by_root_element_not_by_filename() {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml(
            "word/document.xml",
            "application/xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
               <w:body/></w:document>"
                .into(),
        );
        package.add_xml(
            "word/charts/chart1.xml",
            "application/xml",
            "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
               <c:chart/></c:chartSpace>"
                .into(),
        );
        package.add_xml(
            "word/charts/colors1.xml",
            "application/xml",
            "<cs:colorStyle xmlns:cs=\"http://schemas.microsoft.com/office/drawing/2012/chartStyle\"/>"
                .into(),
        );
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report).unwrap();
        assert_eq!(parsed.charts.len(), 1, "expected only chart1.xml: {:?}", parsed.charts.keys());
        assert!(parsed.charts.contains_key("word/charts/chart1.xml"));
    }
}

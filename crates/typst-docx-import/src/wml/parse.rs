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
    Body, BodyItem, BreakType, Cell, DrawingRef, Field, FurnitureKind, FurnitureRef, LevelFormat,
    NumRef, Numbering, ParaProps, Paragraph, Relationship, Row, Run, RunContent, RunItem, RunProps,
    SectPr, Style, StyleKind, Styles, Table, WmlPackage,
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

    Ok(WmlPackage { body, styles, numbering, rels, media, furniture, even_and_odd_headers })
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
/// collects it into one, but [`parse_furniture_parts`] instead merges each
/// entry into the shared, namespaced `rels` map one at a time — iterating a
/// `Vec` there keeps that merge's order deterministic, rather than iterating
/// a hash map in arbitrary order.
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
    Ok(parse_body_content(body_node))
}

/// Walk a body-shaped container's children into a [`Body`]: paragraphs,
/// tables, and (only meaningful for `w:body`) a trailing `w:sectPr`. Shared
/// by [`parse_document`] (`word/document.xml`'s `w:body`) and
/// [`parse_furniture_part`] (a `w:hdr`/`w:ftr` part's root) — the three
/// containers hold the same paragraph/table content, so this is the one place
/// that matches child element names rather than three copies drifting apart.
/// How deep a `w:sdt` nest [`unwrap_sdt`] will follow. Bounded for the same
/// reason as [`MAX_TABLE_DEPTH`]: a hostile document must not be able to drive
/// unbounded recursion.
const MAX_SDT_DEPTH: usize = 32;

/// The element children of `node`, with every `w:sdt` wrapper replaced by the
/// children of its `w:sdtContent`.
///
/// A `w:sdt` — a *structured document tag*, Word's content control (date
/// picker, drop-down, rich-text placeholder, the wrapper Word puts around a
/// cover page or a footer's page-number field) — renders nothing itself:
/// everything the reader sees lives in its `w:sdtContent` child. So the tag is
/// transparent, and we splice its content in where the tag stood. Real
/// documents lean on these heavily — the POI corpus's `Bug60341.docx` wraps
/// its entire footer in one — and ignoring the element drops that content
/// silently, which is the one failure mode this importer must not have.
fn unwrap_sdt<'a>(node: Node<'a, 'a>) -> Vec<Node<'a, 'a>> {
    let mut out = Vec::new();
    splice_children(node, 0, &mut out);
    out
}

/// Splice an iterator of already-selected children (the caller may have
/// filtered some out, as the paragraph walk does for `w:pPr`). Generic, but
/// *not* recursive — the recursion lives in the two concrete functions below,
/// because a generic function that recurses on a freshly-filtered iterator
/// instantiates a new closure type per level and never stops monomorphizing.
fn splice_sdt<'a>(children: impl Iterator<Item = Node<'a, 'a>>, out: &mut Vec<Node<'a, 'a>>) {
    for child in children {
        splice_node(child, 0, out);
    }
}

fn splice_children<'a>(parent: Node<'a, 'a>, depth: usize, out: &mut Vec<Node<'a, 'a>>) {
    for child in parent.children().filter(|n| n.is_element()) {
        splice_node(child, depth, out);
    }
}

fn splice_node<'a>(child: Node<'a, 'a>, depth: usize, out: &mut Vec<Node<'a, 'a>>) {
    if child.tag_name().name() == "sdt" {
        if depth < MAX_SDT_DEPTH
            && let Some(content) = child.children().find(|n| is_element(*n, "sdtContent"))
        {
            splice_children(content, depth + 1, out);
        }
    } else {
        out.push(child);
    }
}

fn parse_body_content(node: Node) -> Body {
    let mut body = Body::default();
    for child in unwrap_sdt(node) {
        match child.tag_name().name() {
            "p" => body.items.push(BodyItem::Paragraph(parse_paragraph(child))),
            "tbl" => body.items.push(BodyItem::Table(parse_table(child, 0))),
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
        namespace_furniture_rels(&mut body.items, &name);

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
    Ok(parse_body_content(document.root_element()))
}

/// Rewrite every relationship id inside a freshly parsed furniture body to
/// its namespaced form (see [`parse_furniture_parts`] for why). Recurses into
/// hyperlinks, fields, and nested tables so nothing in a header/footer's
/// content tree is missed; a hyperlink's `rel_id` is namespaced for the same
/// reason a drawing's is — both are looked up in the same shared `rels` map,
/// so both are exposed to the same cross-part collision.
fn namespace_furniture_rels(items: &mut [BodyItem], part: &str) {
    for item in items {
        match item {
            BodyItem::Paragraph(p) => namespace_run_items(&mut p.runs, part),
            BodyItem::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        namespace_furniture_rels(&mut cell.content, part);
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
                    if let RunContent::Drawing(d) = c {
                        d.rel_id = eco_format!("{part}!{}", d.rel_id);
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

fn parse_paragraph(node: Node) -> Paragraph {
    let props = node
        .children()
        .find(|n| is_element(*n, "pPr"))
        .map(parse_para_props)
        .unwrap_or_default();
    let runs = fold_field_children(
        node.children().filter(|n| n.is_element() && n.tag_name().name() != "pPr"),
    );
    Paragraph { props, runs }
}

fn parse_hyperlink(node: Node) -> RunItem {
    let rel_id = attr_ns(node, ns::R, "id").map(EcoString::from);
    let anchor = attr(node, "anchor").map(EcoString::from);
    let runs = fold_field_children(node.children().filter(|n| n.is_element()));
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

fn classify_run(node: Node) -> RunKind {
    if let Some(fld) = node.children().find(|n| is_element(*n, "fldChar")) {
        return match attr(fld, "fldCharType") {
            Some("begin") => RunKind::FieldBegin,
            Some("separate") => RunKind::FieldSeparate,
            Some("end") => RunKind::FieldEnd,
            // Not a marker type this folder understands — fall back to
            // parsing the run normally rather than dropping it.
            _ => RunKind::Content(parse_run(node)),
        };
    }
    let mut instr = EcoString::new();
    for t in node.children().filter(|n| is_element(*n, "instrText")) {
        instr.push_str(t.text().unwrap_or_default());
    }
    if !instr.is_empty() {
        return RunKind::InstrText(instr);
    }
    RunKind::Content(parse_run(node))
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
fn fold_field_children<'a>(children: impl Iterator<Item = Node<'a, 'a>>) -> Vec<RunItem> {
    let mut top: Vec<RunItem> = Vec::new();
    let mut stack: Vec<FieldFrame> = Vec::new();

    // Content controls wrap *inline* content too, and a field's begin/end pair
    // can straddle one. Splicing them away before folding means the state
    // machine below sees a flat run sequence, exactly as if Word had never
    // wrapped it.
    let mut flat = Vec::new();
    splice_sdt(children, &mut flat);

    for child in flat {
        match child.tag_name().name() {
            "r" => match classify_run(child) {
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
                        push_item(&mut stack, &mut top, RunItem::Run(parse_run(child)));
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
            "fldSimple" => push_item(&mut stack, &mut top, parse_fld_simple(child)),
            "hyperlink" => push_item(&mut stack, &mut top, parse_hyperlink(child)),
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
fn parse_fld_simple(node: Node) -> RunItem {
    let instr = attr(node, "instr").unwrap_or_default().into();
    let result = fold_field_children(node.children().filter(|n| n.is_element()));
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

fn parse_run(node: Node) -> Run {
    let mut run = Run::default();
    for child in node.children().filter(|n| n.is_element()) {
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
                }
            }
            "oMath" => run.content.push(RunContent::Math(raw_xml(child))),
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

fn parse_table(node: Node, depth: usize) -> Table {
    let mut table = Table::default();
    // Rows can be wrapped in a content control too — Word does this for
    // repeating table sections — so this walk unwraps like the others.
    for child in unwrap_sdt(node) {
        match child.tag_name().name() {
            "tblGrid" => {
                for col in child.children().filter(|n| is_element(*n, "gridCol")) {
                    if let Some(w) = attr(col, "w").and_then(parse_i64) {
                        table.grid.push(w);
                    }
                }
            }
            "tr" => table.rows.push(parse_row(child, depth)),
            _ => {}
        }
    }
    table
}

fn parse_row(node: Node, depth: usize) -> Row {
    let mut row = Row::default();
    for child in unwrap_sdt(node) {
        match child.tag_name().name() {
            "trPr" => {
                row.is_header =
                    child.children().any(|n| is_element(n, "tblHeader"));
            }
            "tc" => row.cells.push(parse_cell(child, depth)),
            _ => {}
        }
    }
    row
}

fn parse_cell(node: Node, depth: usize) -> Cell {
    let mut cell =
        Cell { grid_span: 1, v_merge: None, shd_fill: None, content: Vec::new() };
    for child in unwrap_sdt(node) {
        match child.tag_name().name() {
            "tcPr" => parse_cell_props(child, &mut cell),
            "p" => cell.content.push(BodyItem::Paragraph(parse_paragraph(child))),
            "tbl" if depth < MAX_TABLE_DEPTH => {
                cell.content.push(BodyItem::Table(parse_table(child, depth + 1)));
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
        parse_paragraph(document.root_element())
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
}

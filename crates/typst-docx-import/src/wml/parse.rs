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
use roxmltree::{Document, Node, TextPos};
use rustc_hash::{FxHashMap, FxHashSet};
use typst_ooxml_core::ns;
use typst_ooxml_core::opc::{self, Reader};
use typst_ooxml_core::xmlread::{attr, attr_ns, is_el as is_element};

use crate::ImportError;
use crate::report::ImportReport;
use crate::wml::model::{
    Body, BodyItem, BorderEdge, Borders, BreakType, Cell, CellMargins, ChartData,
    ChartKind, ChartSeries, Comment, DmlDash, DmlFill, DmlGeometry, DmlGradient,
    DmlGradientKind, DmlSeg, DmlShape, DmlStroke, DocumentMeta, DrawingRef, Field,
    FurnitureKind, FurnitureRef, LegendPos, LevelFormat, NumRef, Numbering, ParaProps,
    Paragraph, PresetGeom, Relationship, RevisionInfo, Row, Run, RunContent, RunItem,
    RunProps, SectPr, Section, SectionStart, SrcRect, Style, StyleKind, Styles, Table,
    TableBorders, TableStyleProps, VmlShape, VmlShapeKind, WmlPackage, WordSource,
};

// ===========================================================================
// Resilient XML parsing.
// ===========================================================================
//
// A single malformed element anywhere in a part's XML fails `Document::parse`
// for the *whole* part — `roxmltree` builds one tree in one pass, so there is
// no "skip just the bad bit" short of not handing it a broken tree in the
// first place. Real-world producer bugs are responsible for this crate's
// worst failures (a whole document refusing to import over one malformed
// equation), but they also tend to be narrow and mechanically recognisable,
// so [`repair_xml`] attempts one bounded, text-level fix — keyed off the
// *shape* of `roxmltree`'s own typed error, never its rendered message —
// before giving up. [`parse_xml`]/[`try_parse_xml`] are the two entry points
// every part parse in this module funnels through: the former degrades to a
// safe default on any unrepairable failure (every companion part), the
// latter propagates the error so the one caller that has no safe default
// (`parse_document`, for `word/document.xml` itself — there is no document
// without it) can turn it into a fatal [`ImportError`].

/// Parses `xml` and hands the resulting tree to `f`, retrying once against
/// [`repair_xml`]'s text-level repair if the first attempt fails. A
/// successful repair is itself recorded as a `what`-named [`Severity::Drop`]
/// note — even though the rest of the part came through, whatever the repair
/// excised (a duplicate attribute, a malformed equation) is still lost. On
/// total failure (parsing failed outright, or [`repair_xml`] doesn't apply,
/// or the repaired text *still* doesn't parse), returns the *original*
/// error so the caller can decide what to do with it — nothing is recorded
/// here in that case, since [`parse_xml`]'s callers each need their own
/// wording for what "give up on this part" means for them.
///
/// [`Severity::Drop`]: crate::report::Severity::Drop
fn try_parse_xml<T>(
    xml: &str,
    what: &str,
    report: &mut ImportReport,
    f: impl for<'d> FnOnce(Document<'d>) -> T,
) -> Result<T, roxmltree::Error> {
    match Document::parse(xml) {
        Ok(doc) => Ok(f(doc)),
        Err(e) => {
            let Some((repaired, detail)) = repair_xml(xml, &e) else { return Err(e) };
            match Document::parse(&repaired) {
                Ok(doc) => {
                    report.drop(what, eco_format!("malformed XML, repaired: {detail}"));
                    Ok(f(doc))
                }
                Err(_) => Err(e),
            }
        }
    }
}

/// [`try_parse_xml`], but degrading to `default` (with a `what`-named
/// [`Severity::Drop`] note) on total failure instead of propagating it —
/// what every part parsed by this module wants *except*
/// `word/document.xml` (see `parse_document`, which calls [`try_parse_xml`]
/// directly so it can turn the same failure into a fatal [`ImportError`]
/// instead).
///
/// [`Severity::Drop`]: crate::report::Severity::Drop
fn parse_xml<T>(
    xml: &str,
    what: &str,
    report: &mut ImportReport,
    default: T,
    f: impl for<'d> FnOnce(Document<'d>) -> T,
) -> T {
    try_parse_xml(xml, what, report, f).unwrap_or_else(|e| {
        report.drop(what, eco_format!("malformed XML: {e}; part skipped"));
        default
    })
}

/// Reads a part's XML, treating both "the part is absent" and "the part
/// couldn't be read" (a corrupt zip entry, an XXE/size-limit rejection, …)
/// as `None` — the latter with a `what`-named [`Severity::Drop`] note, since
/// unlike genuine absence it *is* a loss. Every part read this way has a
/// safe empty/default fallback; only `word/document.xml` (read directly in
/// [`parse_package`], guarded by its own `reader.has` check) does not.
///
/// [`Severity::Drop`]: crate::report::Severity::Drop
fn read_optional_part(
    reader: &mut Reader,
    part_name: &str,
    what: &str,
    report: &mut ImportReport,
) -> Option<String> {
    match reader.xml_part(part_name) {
        Ok(xml) => xml,
        Err(e) => {
            report.drop(what, eco_format!("could not read part: {e}"));
            None
        }
    }
}

/// Attempts a bounded, text-level repair for `err`, keyed off *what kind* of
/// error `roxmltree` reported (never its rendered message — matching two
/// error strings is exactly the special-casing this is meant to avoid), and
/// returns the repaired text plus a human description of what changed.
/// Handles two shapes real-world producers are seen to emit:
///
/// - [`roxmltree::Error::DuplicatedAttribute`] — a repeated attribute on one
///   element is illegal XML, but unarguably redundant: whichever value a
///   tolerant reader would pick, keeping the *first* occurrence and
///   dropping the repeat changes nothing a well-formed sibling document
///   could have meant (see [`remove_duplicate_attribute`]);
/// - anything else, *if* the error's position falls inside an
///   `<m:oMath>`/`<m:oMathPara>` region — this crate already treats an OMML
///   equation as fully droppable content (see
///   [`crate::mappers::math::omml_to_inline`]'s own fallback for one that
///   fails to parse in isolation), so whatever specifically broke it
///   (mismatched tags, an unescaped character, …), the safe move is to drop
///   the whole equation rather than interpret a partially-written tag soup
///   (see [`drop_enclosing_equation`]).
///
/// Anything else returns `None` — there's no general, safe rewrite for "some
/// unrelated tag somewhere doesn't nest correctly" without guessing at what
/// the writer meant, so the original error must stand.
fn repair_xml(xml: &str, err: &roxmltree::Error) -> Option<(String, EcoString)> {
    if let roxmltree::Error::DuplicatedAttribute(name, pos) = err {
        let repaired = remove_duplicate_attribute(xml, *pos)?;
        return Some((repaired, eco_format!("removed a duplicate '{name}' attribute")));
    }
    let offset = text_offset_at(xml, err.pos());
    let repaired = drop_enclosing_equation(xml, offset)?;
    Some((repaired, EcoString::from("dropped a malformed OMML equation")))
}

/// Converts a 1-based `(row, col)` [`TextPos`] — `roxmltree`'s own error
/// position, where `col` counts *characters*, not bytes — back into a byte
/// offset into `text`. The inverse of `roxmltree::Document::text_pos_at`,
/// which only goes the other way; the repairs below need to slice the
/// original text at the position an error was reported at.
fn text_offset_at(text: &str, pos: TextPos) -> usize {
    let mut row = 1u32;
    let mut line_start = 0usize;
    if pos.row > 1 {
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                row += 1;
                if row == pos.row {
                    line_start = i + 1;
                    break;
                }
            }
        }
    }
    for (col, (i, _)) in (1u32..).zip(text[line_start..].char_indices()) {
        if col == pos.col {
            return line_start + i;
        }
    }
    text.len()
}

/// Excises the *second* occurrence of a duplicated attribute — `pos` points
/// at its very first character (the start of its, possibly prefixed,
/// qualified name; verified against `roxmltree`'s own attribute-resolution
/// code, which reports the position of the repeat, not the original) — along
/// with the run of whitespace immediately before it, so the tag doesn't come
/// out with a doubled space where the attribute used to be.
fn remove_duplicate_attribute(text: &str, pos: TextPos) -> Option<String> {
    let start = text_offset_at(text, pos);
    let bytes = text.as_bytes();

    let mut i = start;
    while i < bytes.len() && bytes[i] != b'=' {
        i += 1;
    }
    i += 1; // Past `=`.
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let quote = *bytes.get(i)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    i += 1;
    while i < bytes.len() && bytes[i] != quote {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let end = i + 1; // Past the closing quote.

    let mut trim_start = start;
    while trim_start > 0 && (bytes[trim_start - 1] as char).is_whitespace() {
        trim_start -= 1;
    }

    let mut out = String::with_capacity(text.len() - (end - trim_start));
    out.push_str(&text[..trim_start]);
    out.push_str(&text[end..]);
    Some(out)
}

/// If `offset` falls inside an `<m:oMath>`/`<m:oMathPara>` region, returns
/// the text with that whole region excised. `m:oMath` is tried first (the
/// narrowest droppable unit — a single equation): it also matches every
/// `m:oMath` nested inside an `m:oMathPara`, so a document with several
/// equations under one `m:oMathPara` only loses the one that's actually
/// broken. Only if `offset` isn't inside any `m:oMath` region does this fall
/// back to `m:oMathPara` itself (the error is in the wrapper's own markup,
/// not any equation it contains). Returns `None` if `offset` isn't inside
/// either — the caller's signal that this repair doesn't apply here.
fn drop_enclosing_equation(text: &str, offset: usize) -> Option<String> {
    for tag in ["m:oMath", "m:oMathPara"] {
        for (start, end) in tag_regions(text, tag) {
            if (start..end).contains(&offset) {
                let mut out = String::with_capacity(text.len() - (end - start));
                out.push_str(&text[..start]);
                out.push_str(&text[end..]);
                return Some(out);
            }
        }
    }
    None
}

/// Every non-overlapping `<tag ...>...</tag>` region in `text`, found by
/// plain substring/tag-boundary matching rather than requiring `text` to
/// parse as XML at all — the whole point, since this runs precisely when it
/// doesn't. Safe for `m:oMath`/`m:oMathPara` specifically because neither
/// ever nests inside another instance of itself, so "the next literal close
/// tag after this open tag" is always the right match, however broken the
/// content between them is — the malformed tag soup this exists to step
/// over lives entirely *inside* a region, never in whether one starts or
/// ends.
fn tag_regions(text: &str, tag: &str) -> Vec<(usize, usize)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut regions = Vec::new();
    let mut i = 0;
    while let Some(rel) = text[i..].find(&open) {
        let start = i + rel;
        let after_name = start + open.len();
        // Reject a prefix collision (`<m:oMathPara` also contains
        // `<m:oMath` as a literal prefix): a genuine `m:oMath` tag name ends
        // right there, at `>`, `/`, or whitespace.
        let boundary = text[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c == '/' || c.is_whitespace());
        if !boundary {
            i = after_name;
            continue;
        }
        let Some(tag_close_rel) = find_unquoted_gt(&text[after_name..]) else { break };
        let tag_close = after_name + tag_close_rel;
        let self_closing = text.as_bytes()[tag_close - 1] == b'/';
        if self_closing {
            i = tag_close + 1;
            continue;
        }
        let search_from = tag_close + 1;
        let Some(close_rel) = text[search_from..].find(&close) else {
            i = search_from;
            continue;
        };
        let end = search_from + close_rel + close.len();
        regions.push((start, end));
        i = end;
    }
    regions
}

/// The index of the first `>` in `s` that isn't inside a quoted attribute
/// value — the end of one XML start/self-closing tag.
fn find_unquoted_gt(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            _ => {}
        }
    }
    None
}

/// Open the `.docx` and parse `word/document.xml`, `styles.xml`,
/// `numbering.xml`, relationships and media into the Word IR.
pub fn parse_package(
    bytes: &[u8],
    report: &mut ImportReport,
) -> Result<WmlPackage, ImportError> {
    // Fail early with a real error if it isn't even a package, so the stub is
    // honest end-to-end.
    let mut reader = Reader::open(bytes)?;
    if !reader.has("word/document.xml") {
        return Err(ImportError::NotAWordDocument);
    }

    // Every part below except `word/document.xml` itself degrades to a safe
    // default (recording a `Severity::Drop` note) on any failure — missing,
    // unreadable, or malformed XML that `try_parse_xml`'s one repair
    // attempt couldn't fix — rather than aborting the whole import. There is
    // no document without `word/document.xml`, so it alone still propagates
    // a fatal `ImportError` (see `parse_document`).
    let mut rels = parse_rels(&mut reader, report);
    let settings = parse_settings(&mut reader, report);

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

    let styles =
        match read_optional_part(&mut reader, "word/styles.xml", "styles.xml", report) {
            Some(xml) => {
                parse_xml(&xml, "styles.xml", report, Styles::default(), parse_styles)
            }
            None => Styles::default(),
        };

    let numbering = match read_optional_part(
        &mut reader,
        "word/numbering.xml",
        "numbering.xml",
        report,
    ) {
        Some(xml) => parse_xml(
            &xml,
            "numbering.xml",
            report,
            Numbering::default(),
            parse_numbering,
        ),
        None => Numbering::default(),
    };

    let meta =
        match read_optional_part(&mut reader, CORE_PROPS_PART, CORE_PROPS_PART, report) {
            Some(xml) => parse_xml(
                &xml,
                CORE_PROPS_PART,
                report,
                DocumentMeta::default(),
                parse_core_properties,
            ),
            None => DocumentMeta::default(),
        };

    // Guaranteed present by the `has` check above.
    let doc_xml = reader.xml_part("word/document.xml")?.unwrap_or_default();
    let body = parse_document(&doc_xml, report)?;

    let furniture = parse_furniture_parts(&mut reader, &mut rels, report);
    let footnotes = parse_notes_part(
        &mut reader,
        &mut rels,
        "word/footnotes.xml",
        "footnote",
        report,
    );
    let endnotes =
        parse_notes_part(&mut reader, &mut rels, "word/endnotes.xml", "endnote", report);
    let comments = parse_comments_part(&mut reader, &mut rels, report);
    let sources = parse_bibliography(&mut reader, report);
    let charts = parse_chart_parts(&mut reader, report);

    let bookmarks = collect_bookmarks(&body);

    Ok(WmlPackage {
        body,
        styles,
        numbering,
        meta,
        bookmarks,
        rels,
        media,
        furniture,
        even_and_odd_headers: settings.even_and_odd_headers,
        mirror_margins: settings.mirror_margins,
        footnotes,
        endnotes,
        comments,
        sources,
        charts,
    })
}

// --- Bookmarks ---------------------------------------------------------------

/// Rewrite a Word bookmark name into something Typst's `<..>` label syntax
/// accepts. A label may not contain whitespace or `>` (which would close it);
/// everything outside a conservative safe set becomes `-`.
fn sanitize_label(name: &str) -> EcoString {
    let label: EcoString = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if label.is_empty() { "bookmark".into() } else { label }
}

/// Map every `w:bookmarkStart` in the document to a unique Typst label.
///
/// Word's own names are unique per document, but sanitising can collide two
/// distinct names onto one label, and a duplicate label makes a Typst
/// reference ambiguous — so collisions take a numeric suffix. Word's hidden
/// `_GoBack` bookmark is skipped: it records where the editor's cursor last
/// was, not a target anyone authored.
fn collect_bookmarks(body: &Body) -> FxHashMap<EcoString, EcoString> {
    let mut labels = FxHashMap::default();
    let mut used = FxHashSet::default();
    for section in &body.sections {
        collect_bookmarks_in_items(&section.items, &mut labels, &mut used);
    }
    labels
}

type Labels = FxHashMap<EcoString, EcoString>;
type UsedLabels = FxHashSet<EcoString>;

fn collect_bookmarks_in_items(
    items: &[BodyItem],
    labels: &mut Labels,
    used: &mut UsedLabels,
) {
    for item in items {
        match item {
            BodyItem::Paragraph(p) => {
                for run_item in &p.runs {
                    collect_bookmarks_in_run_item(run_item, labels, used);
                }
            }
            BodyItem::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_bookmarks_in_items(&cell.content, labels, used);
                    }
                }
            }
        }
    }
}

fn collect_bookmarks_in_run_item(
    item: &RunItem,
    labels: &mut Labels,
    used: &mut UsedLabels,
) {
    match item {
        RunItem::Bookmark(name) => register_bookmark(name, labels, used),
        // Comment anchors get their labels at lower time, from the comment's
        // own id, so there is no name to reserve here.
        RunItem::CommentRange { .. }
        | RunItem::RevisionStart(_)
        | RunItem::RevisionEnd => {}
        RunItem::Deletion { runs, .. } => {
            for run_item in runs {
                collect_bookmarks_in_run_item(run_item, labels, used);
            }
        }
        RunItem::Hyperlink { runs, .. } => {
            for run_item in runs {
                collect_bookmarks_in_run_item(run_item, labels, used);
            }
        }
        RunItem::Field(field) => {
            for run_item in &field.result {
                collect_bookmarks_in_run_item(run_item, labels, used);
            }
        }
        // A text box's body is ordinary content and can carry bookmarks too.
        RunItem::Run(run) => {
            for content in &run.content {
                if let RunContent::TextBox(items) = content {
                    collect_bookmarks_in_items(items, labels, used);
                }
            }
        }
    }
}

fn register_bookmark(name: &EcoString, labels: &mut Labels, used: &mut UsedLabels) {
    if name == "_GoBack" || labels.contains_key(name) {
        return;
    }
    let base = sanitize_label(name);
    let mut label = base.clone();
    let mut suffix = 2;
    while !used.insert(label.clone()) {
        label = eco_format!("{base}-{suffix}");
        suffix += 1;
    }
    labels.insert(name.clone(), label);
}

#[cfg(test)]
mod bookmark_tests {
    use super::*;

    fn bookmarked(names: &[&str]) -> Body {
        let runs = names.iter().map(|n| RunItem::Bookmark((*n).into())).collect();
        Body {
            sections: vec![Section {
                items: vec![BodyItem::Paragraph(Paragraph {
                    props: Default::default(),
                    runs,
                })],
                props: SectPr::default(),
            }],
        }
    }

    /// A name Typst's `<..>` syntax can't hold becomes a safe label.
    #[test]
    fn an_unusable_name_is_sanitised() {
        assert_eq!(sanitize_label("odd name/with*chars"), "odd-name-with-chars");
        assert_eq!(sanitize_label("_Toc12345"), "_Toc12345");
        assert_eq!(sanitize_label(""), "bookmark");
    }

    /// Sanitising can collide two distinct Word names onto one label, and a
    /// duplicate label makes a Typst reference ambiguous.
    #[test]
    fn colliding_names_get_distinct_labels() {
        let labels = collect_bookmarks(&bookmarked(&["a b", "a/b"]));
        assert_eq!(labels.len(), 2);
        let mut resolved: Vec<_> = labels.values().cloned().collect();
        resolved.sort();
        assert_eq!(resolved, ["a-b", "a-b-2"]);
    }

    /// `_GoBack` records where the editor's cursor last was — it is never a
    /// target anyone authored, so it must not become a label.
    #[test]
    fn the_hidden_goback_bookmark_is_skipped() {
        let labels = collect_bookmarks(&bookmarked(&["_GoBack", "Real"]));
        assert_eq!(labels.len(), 1);
        assert!(labels.contains_key("Real"));
    }
}

// --- Core properties (`docProps/core.xml`) -----------------------------------

/// The package part holding the document's core properties. Named once so the
/// part path and the label used in report notes can't drift apart.
const CORE_PROPS_PART: &str = "docProps/core.xml";

/// Parse `docProps/core.xml` into [`DocumentMeta`].
///
/// Matched on local names, so it reads Word's `dc:`/`cp:`/`dcterms:` prefixes
/// and any other producer's equally well — the same producer-agnostic rule the
/// rest of this parser follows. An absent or unreadable part is simply no
/// metadata; it never fails an import.
fn parse_core_properties(doc: Document) -> DocumentMeta {
    let mut meta = DocumentMeta::default();
    for child in doc.root_element().children().filter(|n| n.is_element()) {
        let Some(text) = child.text().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        match child.tag_name().name() {
            "title" => meta.title = Some(text.into()),
            "creator" => meta.creator = Some(text.into()),
            "description" => meta.description = Some(text.into()),
            "keywords" => meta.keywords = Some(text.into()),
            "created" => meta.created = Some(text.into()),
            _ => {}
        }
    }
    meta
}

// --- Relationships -----------------------------------------------------------

fn parse_rels(
    reader: &mut Reader,
    report: &mut ImportReport,
) -> FxHashMap<EcoString, Relationship> {
    parse_rels_for(reader, "word/document.xml", report)
        .into_iter()
        .collect()
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
    report: &mut ImportReport,
) -> Vec<(EcoString, Relationship)> {
    let rels_name = opc::rels_part_name(part_name);
    let Some(xml) = read_optional_part(reader, &rels_name, &rels_name, report) else {
        return Vec::new();
    };
    parse_xml(&xml, &rels_name, report, Vec::new(), |document| {
        opc::rel_entries(document.root())
            .into_iter()
            .map(|rel| {
                (rel.id, Relationship { target: rel.target, external: rel.external })
            })
            .collect()
    })
}

// --- word/document.xml --------------------------------------------------------

/// The one part in the whole package with no safe fallback — there is no
/// document without it — so unlike every other part parsed by this module,
/// failure here (even after [`try_parse_xml`]'s one repair attempt) is fatal.
fn parse_document(xml: &str, report: &mut ImportReport) -> Result<Body, ImportError> {
    let body = try_parse_xml(xml, "word/document.xml", report, |document| {
        document
            .root_element()
            .children()
            .find(|n| is_element(*n, "body"))
            .map(parse_document_body)
    })
    .map_err(xml_err)?;
    body.ok_or_else(|| ImportError::Xml("word/document.xml has no w:body".into()))
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

/// One item of a paragraph's flattened inline content, as
/// [`flatten_revisions`] produces it.
///
/// A revision wrapper cannot simply be spliced away like the other transparent
/// wrappers when its record is being kept: `w:ins` *brackets* content, and the
/// bracket is the information. Since the flattened list is otherwise made of
/// XML nodes, and a marker is not a node, the list becomes this small enum
/// instead — which is also what lets a deletion carry nodes that never enter
/// the run sequence at all.
enum Flat<'a> {
    Node(Node<'a, 'a>),
    /// Opens an insertion (`w:ins`, or `w:moveTo`).
    RevisionStart(RevisionInfo),
    RevisionEnd,
    /// A deletion (`w:del`/`w:moveFrom`) and the nodes it removed. They are
    /// held aside rather than spliced into the run sequence: an accepted
    /// deletion is *not* part of the text, so its content must not flow into
    /// the paragraph — only into the revision record.
    Deleted {
        info: RevisionInfo,
        nodes: Vec<Node<'a, 'a>>,
    },
}

/// `w:author`/`w:date` off a revision wrapper, plus the `w:name` that ties the
/// two halves of a move together.
fn revision_info(node: Node, moved: bool) -> RevisionInfo {
    RevisionInfo {
        author: attr(node, "author").map(EcoString::from),
        date: attr(node, "date").map(EcoString::from),
        move_name: attr(node, "name").map(EcoString::from),
        moved,
    }
}

/// Flatten a paragraph's inline children, turning revision wrappers into
/// [`Flat`] markers rather than splicing them away.
///
/// Always records them: the Word IR's job is to say what the document
/// contains, and whether a revision reaches the *output* is a lowering
/// decision (see [`crate::opts::TrackedChanges`] and `mappers::revision`).
///
/// Generic, but *not* recursive — same constraint as [`splice_children`] and
/// for the same reason: recursing generically on a freshly-filtered iterator
/// instantiates a new closure type per level and never stops monomorphizing.
/// The recursion lives in the two concrete functions below.
fn flatten_revisions<'a>(
    children: impl Iterator<Item = Node<'a, 'a>>,
    out: &mut Vec<Flat<'a>>,
) {
    for child in children {
        flatten_revision_node(child, 0, out);
    }
}

fn flatten_revision_children<'a>(
    parent: Node<'a, 'a>,
    depth: usize,
    out: &mut Vec<Flat<'a>>,
) {
    for child in parent.children().filter(|n| n.is_element()) {
        flatten_revision_node(child, depth, out);
    }
}

fn flatten_revision_node<'a>(child: Node<'a, 'a>, depth: usize, out: &mut Vec<Flat<'a>>) {
    let name = child.tag_name().name();
    let moved = name == "moveTo" || name == "moveFrom";
    match name {
        "ins" | "moveTo" if depth < MAX_WRAPPER_DEPTH => {
            out.push(Flat::RevisionStart(revision_info(child, moved)));
            // Recursed rather than spliced, so a revision nested inside
            // another (content inserted and then deleted again — Word writes
            // exactly that) keeps both records.
            flatten_revision_children(child, depth + 1, out);
            out.push(Flat::RevisionEnd);
        }
        "del" | "moveFrom" if depth < MAX_WRAPPER_DEPTH => {
            let mut nodes = Vec::new();
            splice_children(child, depth + 1, &mut nodes);
            out.push(Flat::Deleted { info: revision_info(child, moved), nodes });
        }
        // Everything else — including a revision wrapper past the depth cap —
        // falls through to the ordinary wrapper splicing, which is exactly
        // the accept-the-change behaviour that predates this.
        _ => {
            let mut spliced = Vec::new();
            splice_node(child, 0, &mut spliced);
            out.extend(spliced.into_iter().map(Flat::Node));
        }
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
        // Tracked changes are *accepted*, which is what Word shows by default
        // and what the document's author last meant it to say. An insertion
        // (`w:ins`, or `w:moveTo` for text moved in) is part of the final
        // text, so the wrapper is spliced away and its runs kept. A deletion
        // (`w:del`/`w:moveFrom`) is not, so it falls through to the catch-all
        // and is dropped — its text lives in `w:delText`, which `parse_run`
        // doesn't read, so nothing leaks even if the wrapper is reached
        // another way.
        "ins" | "moveTo" => {
            if depth < MAX_WRAPPER_DEPTH {
                splice_children(child, depth + 1, out);
            }
        }
        "del" | "moveFrom" => {}
        "sdt" => {
            if depth < MAX_WRAPPER_DEPTH
                && let Some(content) =
                    child.children().find(|n| is_element(*n, "sdtContent"))
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

/// Walk a body-shaped container's children into a flat item list: paragraphs
/// and tables, in document order. Shared by [`parse_furniture_part`] (a
/// `w:hdr`/`w:ftr` part's root), [`parse_notes_part`] (a footnote/endnote's
/// own body), and [`parse_txbx_content`] (a text box's own content) — all
/// hold the same paragraph/table content, so this is the one place that
/// matches child element names rather than copies drifting apart. None of
/// these three can carry a section boundary of their own: `w:sectPr` is only
/// valid directly inside `word/document.xml`'s `w:body`, or a paragraph's
/// `w:pPr` there — see [`parse_document_body`] for the one walk that does
/// need to recognize one.
///
/// `tb_depth` is how many text boxes deep this call is nested — 0 at every
/// top-level part, incremented only by [`parse_txbx_content`] — see
/// [`MAX_TEXTBOX_DEPTH`].
fn parse_body_content(node: Node, tb_depth: usize) -> Vec<BodyItem> {
    let mut items = Vec::new();
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "p" => items.push(BodyItem::Paragraph(parse_paragraph(child, tb_depth))),
            "tbl" => items.push(BodyItem::Table(parse_table(child, 0, tb_depth))),
            _ => {}
        }
    }
    items
}

/// Walk `word/document.xml`'s `w:body` into a [`Body`] of [`Section`]s. A
/// paragraph whose own `w:pPr` carries a `w:sectPr` is the *last* item of the
/// section that `sectPr` describes; the body's own trailing `w:sectPr` (a
/// direct child of `w:body`, per the schema always last) closes the final
/// section the same way. Unlike [`parse_body_content`], this can't just
/// collect a flat item list and split it afterward — a section-closing
/// paragraph has to end up as the last item of *its own* section, which means
/// watching for `w:pPr/w:sectPr` while items are still being collected.
///
/// A document with no `w:sectPr` at all — neither on a paragraph nor at the
/// body's end — is one section with default properties: this is what
/// guarantees the returned `Body::sections` is never empty.
fn parse_document_body(node: Node) -> Body {
    let mut sections = Vec::new();
    let mut items = Vec::new();
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "p" => {
                let paragraph = parse_paragraph(child, 0);
                let closing = paragraph.props.sect_pr.clone();
                items.push(BodyItem::Paragraph(paragraph));
                if let Some(props) = closing {
                    sections.push(Section { items: std::mem::take(&mut items), props });
                }
            }
            "tbl" => items.push(BodyItem::Table(parse_table(child, 0, 0))),
            "sectPr" => {
                let props = parse_sectpr(child);
                sections.push(Section { items: std::mem::take(&mut items), props });
            }
            _ => {}
        }
    }
    // Trailing content with no closing `sectPr` still needs a home — and a
    // document with no `sectPr` anywhere at all falls here too, producing
    // the single default-properties section every caller can rely on.
    if !items.is_empty() || sections.is_empty() {
        sections.push(Section { items, props: SectPr::default() });
    }
    Body { sections }
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
    report: &mut ImportReport,
) -> FxHashMap<EcoString, Vec<BodyItem>> {
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
        // A malformed header/footer part degrades to an empty body (dropped
        // downstream by `mappers::section`'s own "visually empty" check)
        // rather than aborting the rest of the document — same policy as
        // every other companion part.
        let Some(xml) = read_optional_part(reader, &name, &name, report) else {
            continue;
        };
        let mut items = parse_xml(&xml, &name, report, Vec::new(), parse_furniture_part);
        namespace_rel_ids(&mut items, &name);

        for (rid, rel) in parse_rels_for(reader, &name, report) {
            rels.insert(eco_format!("{name}!{rid}"), rel);
        }

        furniture.insert(name, items);
    }
    furniture
}

/// Parse a `w:hdr`/`w:ftr` part into a flat item list. Unlike
/// `word/document.xml`, there's no wrapping `w:body` — the root element
/// itself is the content container — but its children are otherwise the same
/// paragraph/table content [`parse_body_content`] already walks; a furniture
/// part never carries its own `w:sectPr`.
fn parse_furniture_part(document: Document) -> Vec<BodyItem> {
    parse_body_content(document.root_element(), 0)
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
            // A bookmark name is document-global, not a per-part relationship
            // id, so it needs no namespacing. Nor is a comment id, which
            // keys `word/comments.xml` for the whole package.
            RunItem::Bookmark(_)
            | RunItem::CommentRange { .. }
            | RunItem::RevisionStart(_)
            | RunItem::RevisionEnd => {}
            RunItem::Deletion { runs, .. } => namespace_run_items(runs, part),
            RunItem::Run(r) => {
                for c in &mut r.content {
                    match c {
                        RunContent::Drawing(d) => {
                            d.rel_id = eco_format!("{part}!{}", d.rel_id);
                        }
                        // A chart reference is a relationship id too — same
                        // reasoning as `Drawing`'s just above.
                        RunContent::Chart(d) => {
                            d.rel_id = eco_format!("{part}!{}", d.rel_id);
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

/// Parse Word's Source Manager (`b:Sources`) out of whichever `customXml`
/// item holds it.
///
/// Unlike every other part this module reads, the bibliography has no fixed
/// name: Word numbers `customXml/itemN.xml` by insertion order, so the store
/// is found by *content* — the first item whose root is `b:Sources`. Matching
/// on the local name keeps that producer-agnostic, exactly as elsewhere here.
///
/// Word writes the store routinely and usually leaves it empty, so an empty
/// result is the normal case, not a failure.
fn parse_bibliography(reader: &mut Reader, report: &mut ImportReport) -> Vec<WordSource> {
    let names: Vec<EcoString> = reader
        .names()
        .iter()
        .filter(|n| n.starts_with("customXml/item") && n.ends_with(".xml"))
        .cloned()
        .collect();

    for name in names {
        let Some(xml) = read_optional_part(reader, &name, &name, report) else {
            continue;
        };
        let sources = parse_xml(&xml, &name, report, Vec::new(), |document| {
            let root = document.root_element();
            if root.tag_name().name() != "Sources" {
                return Vec::new();
            }
            root.children()
                .filter(|n| is_element(*n, "Source"))
                .filter_map(parse_source)
                .collect()
        });
        if !sources.is_empty() {
            return sources;
        }
    }
    Vec::new()
}

/// One `b:Source`. `None` without a `b:Tag`: that is the citation key, and an
/// entry nothing can cite is not worth carrying into the sidecar.
fn parse_source(node: Node) -> Option<WordSource> {
    let mut source = WordSource::default();
    for child in node.children().filter(|n| n.is_element()) {
        let text = || child.text().map(EcoString::from).filter(|t| !t.is_empty());
        match child.tag_name().name() {
            "Tag" => source.tag = text()?,
            "SourceType" => source.source_type = text().unwrap_or_default(),
            "Title" => source.title = text(),
            "Year" => source.year = text(),
            "Month" => source.month = text(),
            "Day" => source.day = text(),
            "Publisher" => source.publisher = text(),
            "City" => source.city = text(),
            // Word uses a different container element per source type, and
            // they are mutually exclusive, so one field holds whichever came.
            "JournalName" | "BookTitle" | "PeriodicalTitle" | "ConferenceName"
            | "ProductionCompany" => {
                source.container = source.container.clone().or_else(text);
            }
            "Volume" => source.volume = text(),
            "Issue" => source.issue = text(),
            "Pages" => source.pages = text(),
            "URL" => source.url = text(),
            "DOI" => source.doi = text(),
            "Edition" => source.edition = text(),
            "Author" => parse_source_authors(child, &mut source),
            _ => {}
        }
    }
    (!source.tag.is_empty()).then_some(source)
}

/// `b:Author` nests a second `b:Author` inside itself (Word's schema
/// distinguishes the *role* from the names), holding either a `b:NameList` of
/// people or a single `b:Corporate` name — never both.
fn parse_source_authors(node: Node, source: &mut WordSource) {
    for role in node.children().filter(|n| n.is_element()) {
        for inner in role.children().filter(|n| n.is_element()) {
            match inner.tag_name().name() {
                "Corporate" => {
                    source.corporate = inner.text().map(EcoString::from);
                }
                "NameList" => {
                    for person in inner.children().filter(|n| is_element(*n, "Person")) {
                        let part = |name| {
                            person
                                .children()
                                .find(|n| is_element(*n, name))
                                .and_then(|n| n.text())
                                .map(EcoString::from)
                                .filter(|t| !t.is_empty())
                        };
                        if let Some(last) = part("Last") {
                            source.persons.push((last, part("First"), part("Middle")));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Parse `word/comments.xml` into a map of `w:id` → [`Comment`].
///
/// Structurally the same job as [`parse_notes_part`] — a companion part of
/// id-keyed body content, whose own relationships are merged under the shared
/// `"{part}!{rid}"` namespace so a comment can carry images and hyperlinks
/// like anything else — with one difference: a comment also carries *who*
/// wrote it, so its attributes are read alongside the body.
fn parse_comments_part(
    reader: &mut Reader,
    rels: &mut FxHashMap<EcoString, Relationship>,
    report: &mut ImportReport,
) -> FxHashMap<i64, Comment> {
    const PART: &str = "word/comments.xml";
    let Some(xml) = read_optional_part(reader, PART, PART, report) else {
        return FxHashMap::default();
    };
    let comments = parse_xml(&xml, PART, report, FxHashMap::default(), |document| {
        let mut comments = FxHashMap::default();
        let root = document.root_element();
        for child in root.children().filter(|n| is_element(*n, "comment")) {
            // An id-less comment can never be matched to its anchors.
            let Some(id) = attr(child, "id").and_then(parse_i64) else { continue };
            let mut body = parse_body_content(child, 0);
            namespace_rel_ids(&mut body, PART);
            comments.insert(
                id,
                Comment {
                    author: attr(child, "author").map(EcoString::from),
                    initials: attr(child, "initials").map(EcoString::from),
                    date: attr(child, "date").map(EcoString::from),
                    body,
                },
            );
        }
        comments
    });

    for (rid, rel) in parse_rels_for(reader, PART, report) {
        rels.insert(eco_format!("{PART}!{rid}"), rel);
    }

    comments
}

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
/// `w:endnotes` actually holds) into a map of `w:id` → flat item list.
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
    report: &mut ImportReport,
) -> FxHashMap<i64, Vec<BodyItem>> {
    let Some(xml) = read_optional_part(reader, part_name, part_name, report) else {
        return FxHashMap::default();
    };
    // A malformed notes part degrades to no notes at all, same policy as
    // every other companion part — the references to it in the main body
    // simply fail to resolve (already handled, and reported, by
    // `mappers::note::lower_note_ref`).
    let notes = parse_xml(&xml, part_name, report, FxHashMap::default(), |document| {
        let mut notes = FxHashMap::default();
        for child in document
            .root_element()
            .children()
            .filter(|n| is_element(*n, element_name))
        {
            if is_boilerplate_note(child) {
                continue;
            }
            // A note with no parsable `w:id` can never be resolved against a
            // `RunContent::NoteRef`, so it's not worth keeping.
            let Some(id) = attr(child, "id").and_then(parse_i64) else { continue };
            let mut items = parse_body_content(child, 0);
            namespace_rel_ids(&mut items, part_name);
            notes.insert(id, items);
        }
        notes
    });

    for (rid, rel) in parse_rels_for(reader, part_name, report) {
        rels.insert(eco_format!("{part_name}!{rid}"), rel);
    }

    notes
}

// --- word/settings.xml ---------------------------------------------------------

/// The two document-wide switches this importer reads out of `settings.xml`:
/// `<w:evenAndOddHeaders/>`, which makes an `even`-typed header/footer
/// reference active (see [`crate::mappers::section`]), and
/// `<w:mirrorMargins/>`, which makes the left/right page margins swap on
/// facing pages. Both are single flags on the settings root, so one read of
/// the part answers both.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Settings {
    pub even_and_odd_headers: bool,
    pub mirror_margins: bool,
}

fn parse_settings(reader: &mut Reader, report: &mut ImportReport) -> Settings {
    let Some(xml) =
        read_optional_part(reader, "word/settings.xml", "settings.xml", report)
    else {
        return Settings::default();
    };
    parse_xml(&xml, "settings.xml", report, Settings::default(), |document| {
        let flag = |name| document.root_element().children().any(|n| is_element(n, name));
        Settings {
            even_and_odd_headers: flag("evenAndOddHeaders"),
            mirror_margins: flag("mirrorMargins"),
        }
    })
}

fn parse_paragraph(node: Node, tb_depth: usize) -> Paragraph {
    let mut props = node
        .children()
        .find(|n| is_element(*n, "pPr"))
        .map(parse_para_props)
        .unwrap_or_default();
    // A run's own `w:rPrChange` sits several levels down, so it is found by
    // one descendant scan rather than threaded up through run parsing — the
    // mapper only needs to know *that* the paragraph has one, since there is
    // nothing in Typst to map it onto.
    props.format_revision |= node
        .descendants()
        .any(|n| n.is_element() && n.tag_name().name() == "rPrChange");
    let runs = fold_field_children(
        node.children()
            .filter(|n| n.is_element() && n.tag_name().name() != "pPr"),
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
// Same non-issue as `wml::model::BodyItem`'s own allow: a `RunKind` is a
// short-lived local produced one per run and immediately destructured, never
// stored in bulk, so the marker variants costing less than `Content` buys
// nothing — and boxing the run would add an allocation to the common case.
#[allow(clippy::large_enum_variant)]
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
    flatten_revisions(children, &mut flat);

    for item in flat {
        let child = match item {
            Flat::Node(node) => node,
            // An insertion's own runs follow this marker and are ordinary
            // content; only the record rides beside them.
            Flat::RevisionStart(info) => {
                push_item(&mut stack, &mut top, RunItem::RevisionStart(info));
                continue;
            }
            Flat::RevisionEnd => {
                push_item(&mut stack, &mut top, RunItem::RevisionEnd);
                continue;
            }
            // A deletion's runs are parsed but kept *out* of the paragraph's
            // own sequence — they are not part of the text any more.
            Flat::Deleted { info, nodes } => {
                let runs = nodes
                    .into_iter()
                    .filter(|n| is_element(*n, "r"))
                    .map(|n| RunItem::Run(parse_run(n, tb_depth)))
                    .collect();
                push_item(&mut stack, &mut top, RunItem::Deletion { info, runs });
                continue;
            }
        };
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
                        push_item(
                            &mut stack,
                            &mut top,
                            RunItem::Run(parse_run(child, tb_depth)),
                        );
                    }
                }
                RunKind::FieldSeparate => {
                    if let Some(frame) = stack.last_mut() {
                        frame.separated = true;
                    }
                }
                RunKind::FieldEnd => {
                    if let Some(frame) = stack.pop() {
                        let field = RunItem::Field(Field {
                            instr: frame.instr,
                            result: frame.result,
                        });
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
                RunKind::Content(run) => {
                    push_item(&mut stack, &mut top, RunItem::Run(run))
                }
            },
            "fldSimple" => {
                push_item(&mut stack, &mut top, parse_fld_simple(child, tb_depth))
            }
            "hyperlink" => {
                push_item(&mut stack, &mut top, parse_hyperlink(child, tb_depth))
            }
            // A named anchor. Word writes bookmarks at the *start* of the
            // paragraph they mark, so position is meaningless here; the
            // paragraph mapper hoists the label to where Typst wants it.
            "bookmarkStart" => {
                if let Some(name) = attr(child, "name").filter(|n| !n.is_empty()) {
                    push_item(&mut stack, &mut top, RunItem::Bookmark(name.into()));
                }
            }
            // The two ends of a commented span. Kept in place (unlike a
            // bookmark, which the paragraph mapper hoists) because *where*
            // they sit is the whole point — they say which words the comment
            // is about.
            "commentRangeStart" | "commentRangeEnd" => {
                if let Some(id) = attr(child, "id").and_then(parse_i64) {
                    let end = child.tag_name().name() == "commentRangeEnd";
                    push_item(&mut stack, &mut top, RunItem::CommentRange { id, end });
                }
            }
            "oMath" => {
                push_item(&mut stack, &mut top, RunItem::Run(math_run(child, false)))
            }
            // An `m:oMathPara` is Word's *block* equation wrapper. Flattening
            // it to its `m:oMath` children keeps one fragment per equation,
            // and the flag preserves the block-ness the wrapper carried.
            "oMathPara" => {
                for m in child.children().filter(|n| is_element(*n, "oMath")) {
                    push_item(&mut stack, &mut top, RunItem::Run(math_run(m, true)));
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
    let result =
        fold_field_children(node.children().filter(|n| n.is_element()), tb_depth);
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
                    // No raster image — a shape, a chart, or a text box.
                    // `collect_dml_content` walks the graphic frame for all
                    // three shapes of shape content (see its doc comment); a
                    // picture-with-caption drawing (blip present) never
                    // reaches this branch at all, since the image wins above
                    // and its caption box, if any, is simply not looked for —
                    // not contorting this for a case real documents rarely
                    // combine.
                    let before = run.content.len();
                    collect_dml_content(child, 0, tb_depth, &mut run.content);
                    if run.content.len() == before
                        && let Some(rel_id) = parse_chart_ref(child)
                    {
                        // Checked last: a chart reference has neither a blip,
                        // a shape, nor a text box of its own, so this only
                        // fires once all of those have come up empty.
                        run.content.push(RunContent::Chart(rel_id));
                    }
                }
            }
            // `w:pict` — the VML spelling of a drawing. Modern Word only
            // ever writes this as the `mc:Fallback` half of an
            // `mc:AlternateContent` (already resolved away from the
            // `mc:Choice` branch by `splice_node`, so this arm only ever
            // sees it when there's no `mc:Choice` at all — a document
            // authored VML-only, or the corpus's older fixtures).
            //
            // Text boxes are found first, exactly as before this arm grew
            // the rest of VML: a blanket scan for `w:txbxContent` anywhere
            // inside, regardless of which VML element wraps it (there are
            // more spellings than `collect_vml_content` enumerates — a bare
            // `v:textbox` with no wrapping shape is one real one). Pictures
            // (`v:imagedata`), WordArt (`v:textpath`), and native shapes
            // (`v:rect`/`v:oval`/`v:roundrect`/`v:line`, possibly grouped
            // via `v:group`) are collected on top of that, unconditionally —
            // see `collect_vml_content`'s doc comment for why the two scans
            // stay separate rather than merging into one walk.
            "pict" => {
                for txbx in direct_txbx_contents(child) {
                    run.content
                        .push(RunContent::TextBox(parse_txbx_content(txbx, tb_depth)));
                }
                collect_vml_content(child, 0, &mut run.content);
            }
            // An OLE embedding. Structurally a `w:pict` with an
            // `o:OLEObject` beside the VML: the same `v:shape`/`v:imagedata`
            // preview picture, which the shared VML walk turns into an
            // ordinary drawing. Before this arm existed the whole element fell
            // through, so the preview — the only part of an embedded object
            // that *can* survive — was discarded with it.
            "object" => parse_object(child, tb_depth, &mut run.content),
            "oMath" => run
                .content
                .push(RunContent::Math { xml: raw_xml(child), display: false }),
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
            // A deletion's text. Word writes removed text as `w:delText`
            // rather than `w:t` precisely so a naive reader won't show it —
            // it reaches this parser only from inside a `w:del` whose record
            // is being kept, never from the document's live text.
            "delText" => {
                if let Some(t) = child.text() {
                    run.content.push(RunContent::Text(t.into()));
                }
            }
            // The comment mark at an anchor.
            "commentReference" => {
                if let Some(id) = attr(child, "id").and_then(parse_i64) {
                    run.content.push(RunContent::CommentRef(id));
                }
            }
            // The number placeholder inside a *comment's own* body — the
            // comment-mark twin of `w:footnoteRef` above, and suppressed for
            // the same reason: Word substitutes it with the rendered mark, so
            // left alone it would surface as stray text inside the comment.
            "annotationRef" => {}
            _ => {}
        }
    }
    run
}

/// A `m:oMath` captured as a single-content run, used when the equation
/// appears directly in paragraph/hyperlink content (its normal position —
/// `m:oMath` is a sibling of `w:r`, not a child of one).
fn math_run(node: Node, display: bool) -> Run {
    Run {
        props: RunProps::default(),
        content: vec![RunContent::Math { xml: raw_xml(node), display }],
    }
}

/// A `w:drawing`'s embedded raster image: the blip's relationship id, its
/// extent, alt text, and the frame Word draws it through (a shaped
/// `a:prstGeom` outline and/or an `a:srcRect` crop). `None` if no `a:blip` is
/// present (a shape, chart, or text box with no raster image — see the
/// `"drawing"` arm of [`parse_run`] for where those go instead).
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
    let align = node
        .descendants()
        .find(|n| is_element(*n, "positionH"))
        .and_then(|n| n.children().find(|c| is_element(*c, "align")))
        .and_then(|n| n.text())
        .map(|text| EcoString::from(text.trim()));

    // Both of these are scoped to the `pic:pic` that owns *this* blip rather
    // than scanned across the whole `w:drawing`: a group can hold a picture
    // beside a shape, and reading the shape's `a:prstGeom` as the picture's
    // frame would round the wrong thing off.
    let pic = blip.ancestors().find(|n| is_element(*n, "pic"));
    let prst_geom = pic
        .and_then(|pic| pic.descendants().find(|n| is_element(*n, "prstGeom")))
        .and_then(parse_preset_geom)
        // A plain `rect` is the shape of an *unframed* picture — the default
        // Word writes for every ordinary image — so recording it would put a
        // pointless `#box(clip: true)` around almost every imported picture.
        .filter(|geom| geom.prst != "rect");
    let src_rect = blip
        .ancestors()
        .find(|n| is_element(*n, "blipFill"))
        .and_then(|fill| fill.children().find(|c| is_element(*c, "srcRect")))
        .map(parse_src_rect)
        .filter(|rect| !rect.is_empty());

    Some(DrawingRef {
        rel_id,
        cx_emu,
        cy_emu,
        alt,
        align,
        prst_geom,
        src_rect,
    })
}

/// An `a:prstGeom` element: its preset name plus the first adjustment guide in
/// its `a:avLst`. Only the first is read because every preset this importer
/// maps takes at most one (`roundRect`'s corner radius); a preset with several
/// isn't reproduced at all, so its remaining guides have nothing to inform.
fn parse_preset_geom(node: Node) -> Option<PresetGeom> {
    let prst = attr(node, "prst")?.into();
    let adj = node
        .descendants()
        .find(|n| is_element(*n, "gd"))
        // `fmla="val 16667"` — the only formula form an adjustment *value*
        // ever takes. Anything else is a computed guide, which needs the
        // preset's own geometry definition to evaluate and is left unread.
        .and_then(|n| attr(n, "fmla"))
        .and_then(|fmla| fmla.strip_prefix("val "))
        .and_then(|value| parse_i64(value.trim()));
    Some(PresetGeom { prst, adj })
}

/// An `a:srcRect`. Every side defaults to zero (no crop on that edge), which
/// is also what an absent attribute means.
fn parse_src_rect(node: Node) -> SrcRect {
    let side = |name| attr(node, name).and_then(parse_i64).unwrap_or(0);
    SrcRect {
        l: side("l"),
        t: side("t"),
        r: side("r"),
        b: side("b"),
    }
}

/// A `w:drawing`'s chart reference: the `r:id` of its `c:chart` graphic-data
/// element. Word spells this element `c:chart` — local name "chart" in the
/// classic drawingml/2006/chart namespace — for a ChartEx chart too (only
/// the *target part* it points at differs; see [`ChartData`]'s doc comment),
/// so matching by local name covers both without distinguishing them here.
/// `None` if there's no such element (a shape or an image, not a chart) or it
/// has no `r:id` (nothing to resolve).
fn parse_chart_ref(node: Node) -> Option<DrawingRef> {
    let chart = node.descendants().find(|n| is_element(*n, "chart"))?;
    let rel_id = attr_ns(chart, ns::R, "id")?.into();
    // Same `wp:extent` a picture carries — this is the size Word laid the
    // chart out at, and a plot rendered at a library default instead will
    // collide its own axis labels and legend.
    let extent = node.descendants().find(|n| is_element(*n, "extent"));
    let cx_emu = extent.and_then(|n| attr(n, "cx")).and_then(parse_i64);
    let cy_emu = extent.and_then(|n| attr(n, "cy")).and_then(parse_i64);
    Some(DrawingRef { rel_id, cx_emu, cy_emu, ..Default::default() })
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
    parse_body_content(node, tb_depth + 1)
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

// --- DrawingML shapes (`wps:wsp`/`wpg:wgp`, found inside a `w:drawing`) ------

/// How deep [`collect_dml_content`] follows the graphic-frame wrappers and
/// `wpg:wgp` group nesting inside one `w:drawing` — the DrawingML counterpart
/// of [`MAX_VML_GROUP_DEPTH`], bounded for the same reason.
const MAX_DML_DEPTH: usize = 32;

/// Walk a `w:drawing`'s graphic frame for the two things it can hold besides a
/// picture or a chart: DrawingML shapes (`wps:wsp`, singly or nested in a
/// `wpg:wgp` group) and any `w:txbxContent` reached by some other route.
///
/// Deliberately *one* walk, unlike the VML side — where [`collect_vml_content`]
/// runs beside a separate blanket `w:txbxContent` scan. A DrawingML shape
/// carries its geometry and its text in the same element (`wps:wsp`), and a
/// painted one lowers to a single `#rect(..)[text]`-shaped call with that text
/// as its *body*, so a second, independent text-box scan would emit the text
/// twice. Stopping the walk at every `w:txbxContent` it does reach keeps the
/// property that blanket scan exists for: a wrapper element this function
/// doesn't recognise still cannot lose the text inside it.
///
/// Each level goes through [`unwrap_wrappers`] rather than plain `children()`:
/// Word nests a *second* `mc:AlternateContent` inside a drawing whenever a
/// group holds a shape with more than one spelling, and descending into both
/// its branches would find the same text box twice — once as the `mc:Choice`
/// shape's own, once as the `mc:Fallback`'s `v:textbox`. That is exactly the
/// duplication `splice_node` exists to prevent, so this walk uses it too.
fn collect_dml_content(
    node: Node,
    depth: usize,
    tb_depth: usize,
    out: &mut Vec<RunContent>,
) {
    if depth >= MAX_DML_DEPTH {
        return;
    }
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            // `wps:wsp` in a document, `wpg:sp`/`pic:sp` in a group — same
            // element by another prefix.
            "wsp" | "sp" => collect_dml_shape(child, tb_depth, out),
            "txbxContent" => {
                out.push(RunContent::TextBox(parse_txbx_content(child, tb_depth)))
            }
            _ => collect_dml_content(child, depth + 1, tb_depth, out),
        }
    }
}

/// One `wps:wsp` — a shape, a text box, or both.
///
/// A shape that paints nothing this importer can resolve (no `a:srgbClr` fill
/// or line colour: an unfilled frame, or one coloured from the document theme,
/// which needs `theme1.xml`'s palette to mean anything) is treated as *only*
/// its text box. That is the overwhelmingly common form of a Word text box,
/// and it is precisely what [`RunContent::TextBox`] already models; drawing a
/// Typst shape around it instead would put a default black border around text
/// Word left unboxed. A shape that does paint something keeps its geometry and
/// takes its text box as its body.
///
/// A painting-nothing shape with no text either is recorded — [`RunContent::
/// DmlUnsupported`] — rather than vanishing, which is what it used to do.
fn collect_dml_shape(wsp: Node, tb_depth: usize, out: &mut Vec<RunContent>) {
    let sp_pr = wsp.children().find(|n| is_element(*n, "spPr"));
    let geom = sp_pr.and_then(parse_dml_geometry);
    let fill = sp_pr.map(parse_dml_fill).unwrap_or_default();
    let stroke = sp_pr
        .and_then(|n| n.children().find(|c| is_element(*c, "ln")))
        .map(parse_dml_stroke);

    let paints = matches!(fill, DmlFill::Solid(_) | DmlFill::Gradient(_))
        || stroke.as_ref().is_some_and(|s| !s.no_fill && s.color.is_some());

    let txbx = direct_txbx_contents(wsp);
    match geom {
        Some(geom) if paints => {
            let (cx_emu, cy_emu) = dml_extent(wsp);
            let body = txbx
                .into_iter()
                .flat_map(|node| parse_txbx_content(node, tb_depth))
                .collect();
            out.push(RunContent::DmlShape(DmlShape {
                geom,
                cx_emu,
                cy_emu,
                fill,
                stroke,
                body,
            }));
        }
        geom if txbx.is_empty() => {
            if geom.is_some() {
                out.push(RunContent::DmlUnsupported);
            }
        }
        _ => {
            for node in txbx {
                out.push(RunContent::TextBox(parse_txbx_content(node, tb_depth)));
            }
        }
    }
}

/// A shape's own size: its `a:xfrm/a:ext`, or — for a shape that states no
/// transform of its own — the size of the whole drawing it sits in
/// (`wp:extent`), which for the single-shape drawing that produces is the same
/// thing.
fn dml_extent(wsp: Node) -> (Option<i64>, Option<i64>) {
    let ext = wsp
        .children()
        .find(|n| is_element(*n, "spPr"))
        .and_then(|n| n.children().find(|c| is_element(*c, "xfrm")))
        .and_then(|n| n.children().find(|c| is_element(*c, "ext")))
        .or_else(|| {
            wsp.ancestors()
                .find(|n| is_element(*n, "inline") || is_element(*n, "anchor"))
                .and_then(|n| n.children().find(|c| is_element(*c, "extent")))
        });
    match ext {
        Some(ext) => {
            (attr(ext, "cx").and_then(parse_i64), attr(ext, "cy").and_then(parse_i64))
        }
        None => (None, None),
    }
}

/// A `wps:spPr`'s geometry: `a:prstGeom` (a named preset) or `a:custGeom` (an
/// explicit path).
fn parse_dml_geometry(sp_pr: Node) -> Option<DmlGeometry> {
    for child in sp_pr.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "prstGeom" => return parse_preset_geom(child).map(DmlGeometry::Preset),
            "custGeom" => return parse_custom_geom(child),
            _ => {}
        }
    }
    None
}

/// An `a:custGeom`'s `a:pathLst`.
///
/// Every `a:path` in the list contributes its segments to one Typst `#curve`
/// (a fresh `curve.move` starts each subpath, which is how Typst spells a
/// multi-subpath curve anyway), but only the *first* path's `@w`/`@h` define
/// the coordinate space. A list whose paths declare different spaces would
/// need per-subpath scaling that no producer this importer has seen actually
/// emits — `typst-docx`'s own `write_custom_geom` writes exactly one path.
fn parse_custom_geom(node: Node) -> Option<DmlGeometry> {
    let path_lst = node.children().find(|n| is_element(*n, "pathLst"))?;
    let paths: Vec<Node> =
        path_lst.children().filter(|n| is_element(*n, "path")).collect();
    let first = paths.first()?;
    let path_w = attr(*first, "w").and_then(parse_i64).unwrap_or(0);
    let path_h = attr(*first, "h").and_then(parse_i64).unwrap_or(0);

    let mut segments = Vec::new();
    for path in paths {
        for cmd in path.children().filter(|n| n.is_element()) {
            let pts: Vec<(i64, i64)> = cmd
                .children()
                .filter(|n| is_element(*n, "pt"))
                .filter_map(|pt| {
                    Some((
                        attr(pt, "x").and_then(parse_i64)?,
                        attr(pt, "y").and_then(parse_i64)?,
                    ))
                })
                .collect();
            match (cmd.tag_name().name(), pts.as_slice()) {
                ("moveTo", [(x, y)]) => segments.push(DmlSeg::MoveTo(*x, *y)),
                ("lnTo", [(x, y)]) => segments.push(DmlSeg::LineTo(*x, *y)),
                ("cubicBezTo", [(x1, y1), (x2, y2), (x, y)]) => {
                    segments.push(DmlSeg::CubicTo(*x1, *y1, *x2, *y2, *x, *y))
                }
                ("close", _) => segments.push(DmlSeg::Close),
                // `a:arcTo`/`a:quadBezTo` have no direct `#curve` counterpart
                // (an arc would have to be flattened to cubics against the
                // preceding point, which this parse has no state for). The
                // path is abandoned rather than silently drawn with a gap
                // where the missing segment was.
                _ => return None,
            }
        }
    }
    Some(DmlGeometry::Custom { path_w, path_h, segments })
}

/// A `wps:spPr`'s fill. Only the forms with a Typst counterpart are recorded;
/// a pattern (`a:pattFill`) or picture (`a:blipFill`) fill on a *shape* leaves
/// the fill [`DmlFill::Unstated`], which reads as "not reproduced" rather than
/// as "no fill" — the two are visually opposite.
fn parse_dml_fill(sp_pr: Node) -> DmlFill {
    for child in sp_pr.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "noFill" => return DmlFill::None,
            "solidFill" => {
                return match parse_dml_color(child) {
                    Some(rgba) => DmlFill::Solid(rgba),
                    None => DmlFill::Unstated,
                };
            }
            "gradFill" => {
                if let Some(gradient) = parse_dml_gradient(child) {
                    return DmlFill::Gradient(gradient);
                }
                return DmlFill::Unstated;
            }
            _ => {}
        }
    }
    DmlFill::Unstated
}

/// An `a:gradFill`. `None` when it states no stops, or when its geometry is
/// neither of the two DrawingML spells that Typst's `gradient` has a
/// counterpart for (`a:lin` and a circular `a:path`).
fn parse_dml_gradient(node: Node) -> Option<DmlGradient> {
    let stops: Vec<(i64, [u8; 4])> = node
        .children()
        .find(|n| is_element(*n, "gsLst"))?
        .children()
        .filter(|n| is_element(*n, "gs"))
        .filter_map(|gs| {
            Some((attr(gs, "pos").and_then(parse_i64)?, parse_dml_color(gs)?))
        })
        .collect();
    if stops.len() < 2 {
        return None;
    }

    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "lin" => {
                let angle_60k = attr(child, "ang").and_then(parse_i64).unwrap_or(0);
                return Some(DmlGradient {
                    stops,
                    kind: DmlGradientKind::Linear { angle_60k },
                });
            }
            "path" if attr(child, "path") == Some("circle") => {
                let rect = child.children().find(|n| is_element(*n, "fillToRect"));
                let side = |name| {
                    rect.and_then(|n| attr(n, name)).and_then(parse_i64).unwrap_or(0)
                };
                let fill_to_rect = [side("l"), side("t"), side("r"), side("b")];
                return Some(DmlGradient {
                    stops,
                    kind: DmlGradientKind::Radial { fill_to_rect },
                });
            }
            _ => {}
        }
    }
    None
}

/// An `a:ln`.
fn parse_dml_stroke(node: Node) -> DmlStroke {
    let mut stroke = DmlStroke {
        w_emu: attr(node, "w").and_then(parse_i64),
        ..Default::default()
    };
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "noFill" => stroke.no_fill = true,
            "solidFill" => stroke.color = parse_dml_color(child),
            "prstDash" => {
                stroke.dash = attr(child, "val").map(|val| DmlDash::Preset(val.into()))
            }
            "custDash" => {
                let stops: Vec<(i64, i64)> = child
                    .children()
                    .filter(|n| is_element(*n, "ds"))
                    .filter_map(|ds| {
                        Some((
                            attr(ds, "d").and_then(parse_i64)?,
                            attr(ds, "sp").and_then(parse_i64)?,
                        ))
                    })
                    .collect();
                if !stops.is_empty() {
                    stroke.dash = Some(DmlDash::Custom(stops));
                }
            }
            _ => {}
        }
    }
    stroke
}

/// The `a:srgbClr` inside a colour-bearing element (`a:solidFill`, `a:gs`),
/// with its optional `a:alpha` folded into the fourth channel.
///
/// `None` for every other colour spelling — `a:schemeClr` above all, which
/// names a slot in `theme1.xml`'s palette rather than a colour, and which this
/// importer does not resolve. Returning `None` there is what keeps a
/// theme-coloured shape on the text-box path (see [`collect_dml_shape`])
/// instead of drawing it in an invented colour.
fn parse_dml_color(node: Node) -> Option<[u8; 4]> {
    let clr = node.children().find(|n| is_element(*n, "srgbClr"))?;
    let hex = attr(clr, "val")?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    // `a:alpha/@val` is 1000ths of a percent of full opacity; its absence is
    // fully opaque.
    let alpha = clr
        .children()
        .find(|n| is_element(*n, "alpha"))
        .and_then(|n| attr(n, "val"))
        .and_then(parse_i64);
    let a = match alpha {
        Some(val) => (val.clamp(0, 100_000) as f64 / 100_000.0 * 255.0).round() as u8,
        None => 255,
    };
    Some([r, g, b, a])
}

// --- VML shapes (`v:shape`/`v:rect`/`v:oval`/`v:roundrect`/`v:line`/`v:group`,
//     found inside a `w:pict`) -------------------------------------------------

/// How deep a `v:group` nesting [`collect_vml_content`] will follow before
/// giving up — the VML counterpart of [`MAX_WRAPPER_DEPTH`]; real documents
/// never nest a canvas group more than a level or two.
const MAX_VML_GROUP_DEPTH: usize = 32;

/// Walk a `w:pict` (or, recursively, a `v:group`'s children) for everything
/// *besides* text boxes: pictures (`v:imagedata`), WordArt (`v:textpath`),
/// and native shapes (`v:rect`/`v:oval`/`v:roundrect`/`v:line`). Text boxes
/// are deliberately handled elsewhere — the blanket, unconditional
/// [`direct_txbx_contents`] scan the `"pict"` arm already ran before calling
/// this — rather than being folded into this same walk, because that scan
/// finds a `w:txbxContent` regardless of which VML element wraps it, and
/// there are more wrapping spellings than this function enumerates (a bare
/// `v:textbox` with no shape around it at all, seen in the wild, is one).
/// Keeping the two scans separate means a VML tag this function doesn't
/// recognize still can't lose a text box inside it — only the new
/// picture/WordArt/shape handling is confined to the tags it actually knows.
///
/// A `v:group` can hold several shapes side by side (a canvas with a picture
/// *and* a caption, `WordWithAttachments.docx` in the POI corpus has exactly
/// this), so every child is visited, not just the first.
fn collect_vml_content(node: Node, depth: usize, out: &mut Vec<RunContent>) {
    if depth >= MAX_VML_GROUP_DEPTH {
        return;
    }
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "group" => collect_vml_content(child, depth + 1, out),
            "shape" => vml_shape_content(child, out),
            "rect" => out.push(vml_primitive_shape(child, VmlShapeKind::Rect)),
            "oval" => out.push(vml_primitive_shape(child, VmlShapeKind::Oval)),
            "roundrect" => out.push(vml_primitive_shape(child, VmlShapeKind::RoundRect)),
            "line" => out.push(vml_primitive_shape(child, VmlShapeKind::Line)),
            // `v:shapetype`/`v:path`/`v:formulas`/`v:f`/`v:handles` are shape
            // *definitions* (a little geometry language), not instances —
            // out of scope per this feature's design, and correctly inert
            // here since they never match any arm above.
            _ => {}
        }
    }
}

/// Parse a `w:object` (an OLE embedding) into its salvageable parts: the
/// preview picture Word rendered beside the payload, plus a note naming what
/// the payload was.
///
/// The VML inside is structurally identical to a `w:pict`'s, so the same two
/// scans run over it — a text box can legitimately appear here too (an
/// embedded control with a caption).
///
/// The one difference is what an *unrecognized* shape means. Elsewhere a
/// `v:shape` this importer can't draw is reported as unsupported geometry;
/// inside a `w:object` that shape is the object's own placeholder, and the
/// [`RunContent::EmbeddedObject`] note already says precisely what it was.
/// Reporting both would be two notes for one loss, so the vaguer one is
/// dropped in favour of the specific one.
fn parse_object(node: Node, tb_depth: usize, out: &mut Vec<RunContent>) {
    for txbx in direct_txbx_contents(node) {
        out.push(RunContent::TextBox(parse_txbx_content(txbx, tb_depth)));
    }
    let mut vml = Vec::new();
    collect_vml_content(node, 0, &mut vml);
    vml.retain(|c| !matches!(c, RunContent::VmlUnsupported));
    out.append(&mut vml);

    let prog_id = node
        .children()
        .find(|n| is_element(*n, "OLEObject"))
        .and_then(|ole| attr(ole, "ProgID"))
        .map(EcoString::from);
    out.push(RunContent::EmbeddedObject { prog_id });
}

/// A generic `v:shape`'s own content: a picture (`v:imagedata`), WordArt
/// text (`v:textpath` with a `string`), or neither — in which case it's a
/// shape with custom `v:path`/`v:formulas` geometry (a callout, a star, …)
/// that this importer can't draw, *unless* its text box was already picked
/// up by the blanket scan in `parse_run` (a `v:shape` legitimately holding
/// only a plain rectangular text box — the overwhelmingly common case,
/// `type="#_x0000_t202"` — must not also get an "unsupported geometry" drop
/// note next to it). A shape's picture and WordArt text are mutually
/// exclusive in practice (Word never writes both), so the first match wins.
fn vml_shape_content(shape: Node, out: &mut Vec<RunContent>) {
    if let Some(imagedata) = shape.children().find(|n| is_element(*n, "imagedata"))
        && let Some(drawing) = parse_vml_imagedata(imagedata, shape)
    {
        out.push(RunContent::Drawing(drawing));
        return;
    }
    if let Some(textpath) = shape.children().find(|n| is_element(*n, "textpath"))
        && let Some(s) = attr(textpath, "string")
        && !s.is_empty()
    {
        out.push(RunContent::VmlText(s.into()));
        return;
    }
    if direct_txbx_contents(shape).is_empty() {
        out.push(RunContent::VmlUnsupported);
    }
}

/// A VML `v:imagedata`'s relationship id, and (from the owning `v:shape`'s
/// `style` attribute) its authored size — the same [`DrawingRef`] shape a
/// DrawingML picture parses to (see [`parse_drawing`]), so it flows through
/// the identical `mappers::drawing::lower_drawing` → `Figure` pipeline,
/// unsupported-format guard included, with no second image path. `None`
/// only for a `v:imagedata` with no `r:id` at all (malformed; not seen in
/// practice — VML always writes one).
fn parse_vml_imagedata(imagedata: Node, shape: Node) -> Option<DrawingRef> {
    let rel_id = attr_ns(imagedata, ns::R, "id")?.into();
    let style = attr(shape, "style").unwrap_or_default();
    Some(DrawingRef {
        rel_id,
        cx_emu: vml_length_pt(style, "width").map(pt_to_emu),
        cy_emu: vml_length_pt(style, "height").map(pt_to_emu),
        ..Default::default()
    })
}

/// A `v:rect`/`v:oval`/`v:roundrect`/`v:line` element's own attributes,
/// captured raw — `crate::mappers::shape` does the unit conversion and color
/// resolution.
fn vml_primitive_shape(node: Node, kind: VmlShapeKind) -> RunContent {
    RunContent::VmlShape(VmlShape {
        kind,
        style: attr(node, "style").unwrap_or_default().into(),
        fill_color: vml_color_attr(node, "fillcolor", "fill"),
        filled: attr(node, "filled") != Some("f"),
        stroke_color: vml_color_attr(node, "strokecolor", "stroke"),
        stroked: attr(node, "stroked") != Some("f"),
        from: attr(node, "from").map(EcoString::from),
        to: attr(node, "to").map(EcoString::from),
    })
}

/// `fillcolor`/`strokecolor`, or — VML's other equivalent spelling — the
/// `color` attribute of a `v:fill`/`v:stroke` child, whichever is present;
/// the direct attribute wins on the rare document that (redundantly) writes
/// both.
fn vml_color_attr(node: Node, attr_name: &str, child_name: &str) -> Option<EcoString> {
    attr(node, attr_name)
        .or_else(|| {
            node.children()
                .find(|n| is_element(*n, child_name))
                .and_then(|n| attr(n, "color"))
        })
        .map(EcoString::from)
}

/// A single CSS-style length token (e.g. `"120pt"`, `"0.75in"`) → points.
/// Recognizes `pt` (identity), `px` (CSS px is 1/96in and pt is 1/72in, so
/// `pt = px * 0.75`), `in`, `cm`, `mm` — the units real documents actually
/// use. A bare number with no unit suffix is deliberately left unparsed
/// rather than assumed to be points: Word writes exactly `width:0` (no unit
/// — moot anyway, since every unit of zero is the same zero) on an
/// auto-stretch horizontal-rule shape's width, and treating that literal `0`
/// as a real `0pt` would draw an invisible shape — worse than omitting the
/// argument and letting Typst size it instead.
fn vml_length_value(value: &str) -> Option<f64> {
    let value = value.trim();
    let split_at = value.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = value.split_at(split_at);
    let number: f64 = number.parse().ok()?;
    let pt = match unit {
        "pt" => number,
        "px" => number * 0.75,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        _ => return None,
    };
    Some(pt)
}

/// One `width`/`height` declaration out of a VML `style="width:120pt;
/// height:80pt;margin-left:0"` CSS-ish attribute, in points. `None` for a
/// missing declaration or one [`vml_length_value`] can't parse either —
/// both are fine, since every caller threads the result straight into an
/// `Option` field ([`DrawingRef`]'s extent, or an argument
/// `crate::mappers::shape` may simply omit).
pub(crate) fn vml_length_pt(style: &str, prop: &str) -> Option<f64> {
    let value = style.split(';').find_map(|decl| {
        let (name, value) = decl.split_once(':')?;
        (name.trim() == prop).then_some(value)
    })?;
    vml_length_value(value)
}

/// A `v:line` endpoint (`from`/`to`, e.g. `"252pt,146.8pt"`) → `(x, y)` in
/// points. `None` if either component doesn't parse under
/// [`vml_length_value`]'s rules — see `mappers::shape::line_length` for why
/// that means the whole line is dropped rather than guessed at.
pub(crate) fn vml_coord_pt(s: &str) -> Option<(f64, f64)> {
    let (x, y) = s.split_once(',')?;
    Some((vml_length_value(x)?, vml_length_value(y)?))
}

/// Points → EMU (English Metric Units, exactly 12700 per point) — the unit
/// [`DrawingRef::cx_emu`]/`cy_emu` are always in. A VML picture's CSS-pt size
/// is converted once here, right where it's parsed, so `DrawingRef` keeps a
/// single unit contract regardless of which markup produced it.
fn pt_to_emu(pt: f64) -> i64 {
    (pt * 12700.0).round() as i64
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
                if let Some(after) = attr(child, "after").and_then(parse_i64) {
                    props.spacing_after = Some(after);
                }
                if let Some(line) = attr(child, "line").and_then(parse_i64) {
                    props.line = Some(line);
                }
            }
            "ind" => {
                props.indent_left = attr(child, "left")
                    .or_else(|| attr(child, "start"))
                    .and_then(parse_i64);
                props.indent_right = attr(child, "right")
                    .or_else(|| attr(child, "end"))
                    .and_then(parse_i64);
                props.indent_first_line = attr(child, "firstLine").and_then(parse_i64);
                props.indent_hanging = attr(child, "hanging").and_then(parse_i64);
            }
            "shd" => props.shd_fill = attr(child, "fill").map(EcoString::from),
            "pBdr" => props.borders = parse_borders(child),
            "pPrChange" => props.format_revision = true,
            "keepLines" => props.keep_lines = toggle(Some(child)),
            "keepNext" => props.keep_next = toggle(Some(child)),
            "rPr" => props.mark_props = parse_run_props(child),
            // Marks this paragraph as the *last* item of a section — see
            // `parse_document_body`, the one place that reads this field.
            "sectPr" => props.sect_pr = Some(parse_sectpr(child)),
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
        dstrike: toggle(child("dstrike")),
        smallcaps: toggle(child("smallCaps")),
        caps: toggle(child("caps")),
        // A bare `<w:u/>` means a single underline; only an explicit
        // `w:val="none"` turns one off. Defaulting here keeps "no `w:u` at
        // all" (`None`) distinguishable from "underlined, style unstated".
        underline: child("u")
            .map(|n| EcoString::from(attr(n, "val").unwrap_or("single"))),
        underline_color: child("u").and_then(|n| attr(n, "color")).map(EcoString::from),
        highlight: child("highlight").and_then(|n| attr(n, "val")).map(EcoString::from),
        color: child("color").and_then(|n| attr(n, "val")).map(EcoString::from),
        size_half_pt: child("sz").and_then(|n| attr(n, "val")).and_then(parse_i64),
        // `w:rPr/w:spacing` is tracking; the identically-named element on a
        // `w:pPr` is paragraph spacing (see `parse_para_props`).
        letter_spacing: child("spacing").and_then(|n| attr(n, "val")).and_then(parse_i64),
        font: child("rFonts").and_then(|n| attr(n, "ascii")).map(EcoString::from),
        lang: child("lang").and_then(|n| attr(n, "val")).map(EcoString::from),
        vert_align: child("vertAlign").and_then(|n| attr(n, "val")).map(EcoString::from),
        rtl: toggle(child("rtl")),
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
            "tblPr" => parse_table_props(child, &mut table),
            "tr" => table.rows.push(parse_row(child, depth, tb_depth)),
            _ => {}
        }
    }
    // One descendant scan, for the same reason the paragraph parser uses one
    // for `w:rPrChange`: the mapper only needs to know *that* a record exists,
    // and these three sit at three different levels (`w:tblPr`, `w:trPr`,
    // `w:tcPr`) — none of them inside a `w:p`, which is why a table whose only
    // tracked change was a formatting one used to report nothing.
    table.format_revision = node.descendants().any(|n| {
        n.is_element()
            && matches!(n.tag_name().name(), "tblPrChange" | "trPrChange" | "tcPrChange")
    });
    table
}

/// A table *style*'s `w:tblPr`: the blanket borders and the default cell
/// padding it gives every table that names it. The same two properties a
/// table can state directly (see [`parse_table_props`]), which is why they
/// resolve against each other in `resolve::styles::effective_table`.
fn parse_table_style_pr(node: Node, out: &mut TableStyleProps) {
    for prop in node.children().filter(|n| n.is_element()) {
        match prop.tag_name().name() {
            "tblBorders" => out.borders = parse_table_borders(prop),
            "tblCellMar" => out.cell_margins = parse_cell_margins(prop),
            _ => {}
        }
    }
}

/// The table-level properties this importer can express: how the table sits
/// between the margins (`w:jc`), how far it is pushed off the left one
/// (`w:tblInd`), and its blanket borders (`w:tblBorders`). The table style and
/// the various width declarations stay out of scope — cell widths already come
/// from `w:tblGrid`.
fn parse_table_props(node: Node, table: &mut Table) {
    for prop in node.children().filter(|n| n.is_element()) {
        match prop.tag_name().name() {
            "jc" => table.jc = attr(prop, "val").map(EcoString::from),
            "tblBorders" => table.borders = parse_table_borders(prop),
            "tblCellMar" => table.cell_margins = parse_cell_margins(prop),
            "tblStyle" => table.style_id = attr(prop, "val").map(EcoString::from),
            // Only `dxa` (twips) is an absolute length. `pct` measures against
            // the text width and `auto`/`nil` against the table's own layout,
            // neither of which is resolvable here, so they are left unread
            // rather than silently reinterpreted as twips.
            "tblInd" if attr(prop, "type").unwrap_or("dxa") == "dxa" => {
                table.indent_twips = attr(prop, "w").and_then(parse_i64);
            }
            _ => {}
        }
    }
}

/// A `w:tblBorders`: the four outer sides plus the two interior ones. Shared
/// by a table's own `w:tblPr` and by a table style's.
fn parse_table_borders(node: Node) -> TableBorders {
    let edge =
        |name| node.children().find(|n| is_element(*n, name)).map(parse_border_edge);
    TableBorders {
        outer: parse_borders(node),
        inside_h: edge("insideH"),
        inside_v: edge("insideV"),
    }
}

fn parse_row(node: Node, depth: usize, tb_depth: usize) -> Row {
    let mut row = Row::default();
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "trPr" => {
                row.is_header = child.children().any(|n| is_element(n, "tblHeader"));
                row.cant_split = child.children().any(|n| is_element(n, "cantSplit"));
                let height = child.children().find(|n| is_element(*n, "trHeight"));
                if let Some(height) = height {
                    row.height_twips = attr(height, "val").and_then(parse_i64);
                    // Word's own default when `@w:hRule` is absent is
                    // `atLeast` — a minimum, not a fixed height.
                    row.height_exact = attr(height, "hRule") == Some("exact");
                }
            }
            "tc" => row.cells.push(parse_cell(child, depth, tb_depth)),
            _ => {}
        }
    }
    row
}

fn parse_cell(node: Node, depth: usize, tb_depth: usize) -> Cell {
    let mut cell = Cell { grid_span: 1, ..Default::default() };
    for child in unwrap_wrappers(node) {
        match child.tag_name().name() {
            "tcPr" => parse_cell_props(child, &mut cell),
            "p" => cell
                .content
                .push(BodyItem::Paragraph(parse_paragraph(child, tb_depth))),
            "tbl" if depth < MAX_TABLE_DEPTH => {
                cell.content.push(BodyItem::Table(parse_table(
                    child,
                    depth + 1,
                    tb_depth,
                )));
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
            "tcBorders" => cell.borders = parse_borders(prop),
            "vAlign" => cell.v_align = attr(prop, "val").map(EcoString::from),
            "tcMar" => cell.margins = parse_cell_margins(prop),
            _ => {}
        }
    }
}

/// Parse the four sides of a `w:tcBorders`, a `w:pBdr`, or a `w:tblBorders`
/// — OOXML states all three identically, so one parser reads all three (the
/// table's two extra interior sides are picked up by its own caller, see
/// `parse_table_props`). Word names the horizontal sides `start`/`end` in its
/// newer, direction-neutral spelling as well as `left`/`right`; both are
/// accepted, mirroring how `w:ind` is read (see `parse_para_props`).
fn parse_borders(node: Node) -> Borders {
    let mut borders = Borders::default();
    for side in node.children().filter(|n| n.is_element()) {
        let edge = parse_border_edge(side);
        match side.tag_name().name() {
            "top" => borders.top = Some(edge),
            "bottom" => borders.bottom = Some(edge),
            "left" | "start" => borders.left = Some(edge),
            "right" | "end" => borders.right = Some(edge),
            _ => {}
        }
    }
    borders
}

fn parse_border_edge(side: Node) -> BorderEdge {
    BorderEdge {
        val: attr(side, "val").unwrap_or("single").into(),
        sz_eighth_pt: attr(side, "sz").and_then(parse_i64),
        color: attr(side, "color").map(EcoString::from),
        space_pt: attr(side, "space").and_then(parse_i64),
    }
}

/// Parse `w:tcMar`. Each side carries its measurement on `@w:w`, not `@w:val`.
fn parse_cell_margins(node: Node) -> CellMargins {
    let mut margins = CellMargins::default();
    for side in node.children().filter(|n| n.is_element()) {
        let value = attr(side, "w").and_then(parse_i64);
        match side.tag_name().name() {
            "top" => margins.top = value,
            "bottom" => margins.bottom = value,
            "left" | "start" => margins.left = value,
            "right" | "end" => margins.right = value,
            _ => {}
        }
    }
    margins
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
fn parse_chart_parts(
    reader: &mut Reader,
    report: &mut ImportReport,
) -> FxHashMap<EcoString, ChartData> {
    let mut charts = FxHashMap::default();
    let names: Vec<EcoString> = reader
        .names()
        .iter()
        .filter(|name| name.starts_with("word/charts/") && name.ends_with(".xml"))
        .cloned()
        .collect();

    for name in names {
        // A malformed chart part degrades to no chart at all (the anchoring
        // drawing then simply has no chart data to resolve, same as any
        // other unresolvable reference) — same policy as every other
        // companion part.
        let Some(xml) = read_optional_part(reader, &name, &name, report) else {
            continue;
        };
        let chart = parse_xml(&xml, &name, report, None, |document| {
            let root = document.root_element();
            (root.tag_name().name() == "chartSpace").then(|| parse_chart_space(root))
        });
        if let Some(chart) = chart {
            charts.insert(name, chart);
        }
    }
    charts
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
    data.kind = chart_kind(chart);
    data.legend = chart_legend(chart);

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

/// A classic chart's plot type, from the first chart-type element found under
/// `c:plotArea` (by local name, same convention as the rest of this module).
/// `c:plotArea` also holds axis elements (`c:catAx`, `c:valAx`, …) alongside
/// the chart-type wrapper, so this skips anything that isn't one of the known
/// wrappers rather than assuming the wrapper is `plotArea`'s first child. A
/// combo chart (e.g. bars with a line series overlaid) nests more than one
/// wrapper; the first one in document order wins, per [`ChartKind`]'s doc
/// comment.
/// `c:legend/c:legendPos`. A chart with no `c:legend` element shows no
/// legend at all, which is a different thing from "wherever the default is" —
/// so absence is preserved rather than defaulted.
fn chart_legend(chart: Node) -> Option<LegendPos> {
    let legend = chart.descendants().find(|n| is_element(*n, "legend"))?;
    let pos = legend
        .children()
        .find(|n| is_element(*n, "legendPos"))
        .and_then(|n| attr(n, "val"));
    Some(match pos {
        Some("t") => LegendPos::Top,
        Some("l") => LegendPos::Left,
        Some("b") => LegendPos::Bottom,
        Some("tr") => LegendPos::TopRight,
        // `r` is also OOXML's own default when `legendPos` is absent.
        _ => LegendPos::Right,
    })
}

fn chart_kind(chart: Node) -> ChartKind {
    let Some(plot_area) = chart.children().find(|n| is_element(*n, "plotArea")) else {
        return ChartKind::default();
    };
    plot_area
        .children()
        .find_map(|child| match child.tag_name().name() {
            "barChart" | "bar3DChart" => Some(ChartKind::Bar),
            "lineChart" | "line3DChart" => Some(ChartKind::Line),
            "scatterChart" | "bubbleChart" => Some(ChartKind::Scatter),
            "areaChart" | "area3DChart" => Some(ChartKind::Area),
            _ => None,
        })
        .unwrap_or_default()
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
    let mut blocks: FxHashMap<EcoString, (Vec<EcoString>, Vec<EcoString>)> =
        FxHashMap::default();
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
    // The shared reader caps `@idx` (an attacker-controlled allocation size);
    // it hands back gaps as `None`, which a Word cached list renders as the
    // empty string it always has.
    typst_ooxml_core::chart::indexed_points(
        container.descendants().filter(|n| is_element(*n, "pt")),
        |pt| attr(pt, "idx").and_then(|s| s.parse::<usize>().ok()),
        |pt| text_of(pt).unwrap_or_default(),
    )
    .into_iter()
    .map(Option::unwrap_or_default)
    .collect()
}

/// A classic chart's `c:pt`: text lives on a nested `c:v` child.
fn pt_text_nested_v(pt: Node) -> Option<EcoString> {
    pt.children()
        .find(|n| is_element(*n, "v"))
        .and_then(|v| v.text())
        .map(EcoString::from)
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
            // `w:cols` without `w:num` is a single column; the attribute is
            // only written when there is more than one.
            "cols" => {
                sect.columns = attr(child, "num").and_then(|v| v.parse::<u32>().ok());
            }
            "pgMar" => {
                sect.margin_top = attr(child, "top").and_then(parse_i64);
                sect.margin_bottom = attr(child, "bottom").and_then(parse_i64);
                sect.margin_left = attr(child, "left").and_then(parse_i64);
                sect.margin_right = attr(child, "right").and_then(parse_i64);
                sect.header_dist = attr(child, "header").and_then(parse_i64);
                sect.footer_dist = attr(child, "footer").and_then(parse_i64);
                // Word writes `w:gutter="0"` on essentially every document;
                // only a real allowance is worth carrying.
                sect.gutter =
                    attr(child, "gutter").and_then(parse_i64).filter(|g| *g > 0);
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
            // Absent means `nextPage` — `SectionStart`'s own default, so an
            // unrecognized `w:val` (a hand-edited or foreign-producer
            // document) falls back the same way rather than being dropped.
            "type" => sect.start = parse_section_start(attr(child, "val")),
            "pgNumType" => {
                sect.page_num_fmt = attr(child, "fmt").map(EcoString::from);
                sect.page_num_start = attr(child, "start").and_then(parse_i64);
            }
            _ => {}
        }
    }
    sect
}

/// `w:sectPr/w:type/@w:val` → [`SectionStart`]. `"nextColumn"` is real but
/// vanishingly rare in practice; everything unrecognized falls back to
/// `NextPage`, matching the type's own default for an absent element.
fn parse_section_start(val: Option<&str>) -> SectionStart {
    match val {
        Some("continuous") => SectionStart::Continuous,
        Some("evenPage") => SectionStart::EvenPage,
        Some("oddPage") => SectionStart::OddPage,
        Some("nextColumn") => SectionStart::NextColumn,
        _ => SectionStart::NextPage,
    }
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

fn parse_styles(document: Document) -> Styles {
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
    styles
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
            "link" => style.link = attr(child, "val").map(EcoString::from),
            "rPr" => style.run = parse_run_props(child),
            // A table style's `w:tblPr`/`w:tcPr` are the table and cell
            // defaults it hands every table that names it; `w:tblStylePr` is
            // its per-region conditional formatting, recorded only so the
            // mapper can report it (see `TableStyleProps`).
            "tblPr" if style.kind == StyleKind::Table => {
                parse_table_style_pr(child, &mut style.table);
            }
            "tcPr" if style.kind == StyleKind::Table => {
                for prop in child.children().filter(|n| n.is_element()) {
                    let val = |name| attr(prop, name).map(EcoString::from);
                    match prop.tag_name().name() {
                        "shd" => style.table.cell_shd_fill = val("fill"),
                        "vAlign" => style.table.cell_v_align = val("val"),
                        _ => {}
                    }
                }
            }
            "tblStylePr" => style.table.conditional = true,
            "pPr" => {
                // `w:outlineLvl` numbers heading levels 1..=9 as 0..=8; **9 is
                // the sentinel for "body text"**, not a tenth heading level.
                // Recording it verbatim made every paragraph of a style that
                // says "I am body text" — which is exactly what a style like
                // `Body` says — come out as a heading. Normalised away here,
                // beside the other OOXML sentinels (see `parse_hex_color`'s
                // handling of `"auto"`), so the Word IR means what it says.
                style.outline_level = child
                    .children()
                    .find(|n| is_element(*n, "outlineLvl"))
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| v.parse::<u8>().ok())
                    .filter(|&level| level <= 8);
                style.para = parse_para_props(child);
            }
            _ => {}
        }
    }
    style
}

// --- word/numbering.xml ---------------------------------------------------------

fn parse_numbering(document: Document) -> Numbering {
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
                    let start = lvl
                        .children()
                        .find(|n| is_element(*n, "start"))
                        .and_then(|n| attr(n, "val"))
                        .and_then(parse_i64);
                    // Read off the *level element itself*, not resolved
                    // through any style chain: a bullet's glyph is authored
                    // per level here, and it is the only record of what Word
                    // actually prints in front of an item.
                    let lvl_text = lvl
                        .children()
                        .find(|n| is_element(*n, "lvlText"))
                        .and_then(|n| attr(n, "val"))
                        .map(EcoString::from);
                    levels.insert(ilvl, LevelFormat { num_fmt, start, lvl_text });
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
                // A `w:lvlOverride` restarts this *instance* at its own
                // number without disturbing the shared `abstractNum`.
                for over in child.children().filter(|n| is_element(*n, "lvlOverride")) {
                    let Some(ilvl) = attr(over, "ilvl").and_then(parse_i64) else {
                        continue;
                    };
                    if let Some(start) = over
                        .children()
                        .find(|n| is_element(*n, "startOverride"))
                        .and_then(|n| attr(n, "val"))
                        .and_then(parse_i64)
                    {
                        numbering.start_overrides.insert((num_id, ilvl), start);
                    }
                }
            }
            _ => {}
        }
    }
    numbering
}

// --- Small XML helpers ---------------------------------------------------------
//
// The local-name element/attribute readers (`is_element` — the shared
// `is_el` under this crate's historical name — plus `attr` and `attr_ns`) are
// shared with the other OOXML importers and now live in
// [`typst_ooxml_core::xmlread`]; they are imported at the top of this module.

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
        package.add_media(
            "word/media/image1.png",
            "png",
            "image/png",
            vec![0x89, 0x50, 0x4E, 0x47],
        );
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
        assert_eq!(package.media["word/media/image1.png"], vec![0x89, 0x50, 0x4E, 0x47]);

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
        assert_eq!(package.body.sections.len(), 1);
        assert_eq!(package.body.sections[0].items.len(), 2);
        let sect = &package.body.sections[0].props;
        assert_eq!(sect.page_w, Some(12240));
        assert_eq!(sect.page_h, Some(15840));
        assert!(sect.landscape);
        assert_eq!(sect.margin_top, Some(1440));
        assert_eq!(sect.margin_right, Some(1440));
        assert_eq!(sect.margin_bottom, Some(1440));
        assert_eq!(sect.margin_left, Some(1440));

        let BodyItem::Paragraph(p) = &package.body.sections[0].items[0] else {
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
        assert!(p.props.borders.is_bottom_only());

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
        let RunContent::Math { xml: raw, .. } = &r5.content[0] else {
            panic!("expected math")
        };
        assert!(raw.starts_with("<m:oMath>"));
        assert!(raw.contains("x+y"));

        let BodyItem::Table(table) = &package.body.sections[0].items[1] else {
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
        assert_eq!(parsed.body.sections.len(), 1);
        assert!(parsed.body.sections[0].items.is_empty());
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
        let RunItem::Field(outer) = &p.runs[0] else {
            panic!("expected the outer field")
        };
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
    fn absent_w_type_defaults_to_next_page() {
        let sect = parse_test_sectpr(r#"<w:pgSz w:w="12240" w:h="15840"/>"#);
        assert_eq!(sect.start, SectionStart::NextPage);
    }

    #[test]
    fn w_type_maps_every_recognized_value() {
        for (val, expected) in [
            ("continuous", SectionStart::Continuous),
            ("evenPage", SectionStart::EvenPage),
            ("oddPage", SectionStart::OddPage),
            ("nextColumn", SectionStart::NextColumn),
            ("nextPage", SectionStart::NextPage),
        ] {
            let sect = parse_test_sectpr(&format!(r#"<w:type w:val="{val}"/>"#));
            assert_eq!(sect.start, expected, "w:type={val}");
        }
    }

    #[test]
    fn unrecognized_w_type_falls_back_to_next_page() {
        let sect = parse_test_sectpr(r#"<w:type w:val="somethingHandEdited"/>"#);
        assert_eq!(sect.start, SectionStart::NextPage);
    }

    #[test]
    fn pg_num_type_surfaces_format_and_start() {
        let sect = parse_test_sectpr(r#"<w:pgNumType w:fmt="lowerRoman" w:start="4"/>"#);
        assert_eq!(sect.page_num_fmt.as_deref(), Some("lowerRoman"));
        assert_eq!(sect.page_num_start, Some(4));
    }

    #[test]
    fn pg_num_type_with_only_fmt_leaves_start_absent() {
        let sect = parse_test_sectpr(r#"<w:pgNumType w:fmt="upperLetter"/>"#);
        assert_eq!(sect.page_num_fmt.as_deref(), Some("upperLetter"));
        assert!(sect.page_num_start.is_none());
    }

    #[test]
    fn no_pg_num_type_leaves_both_absent() {
        let sect = parse_test_sectpr(r#"<w:pgSz w:w="12240" w:h="15840"/>"#);
        assert!(sect.page_num_fmt.is_none());
        assert!(sect.page_num_start.is_none());
    }

    /// A paragraph's own `w:pPr/w:sectPr` closes a section — that paragraph
    /// is its *last* item — and the body's trailing `w:sectPr` closes the
    /// final one. Three sections here (two paragraph-level closes plus the
    /// trailing body-level one), each keeping only the items that came before
    /// its own closing `sectPr`.
    #[test]
    fn a_paragraph_level_sectpr_closes_its_section_and_starts_a_new_one() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
  <w:p><w:r><w:t>one</w:t></w:r></w:p>
  <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:cols w:num="3"/></w:sectPr></w:pPr><w:r><w:t>two-closer</w:t></w:r></w:p>
  <w:p><w:r><w:t>three</w:t></w:r></w:p>
  <w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>four-closer</w:t></w:r></w:p>
  <w:p><w:r><w:t>five</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="15840" w:h="12240"/></w:sectPr>
</w:body>
</w:document>"#;
        let mut report = ImportReport::default();
        let body = parse_document(xml, &mut report).expect("should parse");

        assert_eq!(body.sections.len(), 3);

        assert_eq!(body.sections[0].items.len(), 2);
        assert_eq!(body.sections[0].props.columns, Some(3));

        assert_eq!(body.sections[1].items.len(), 2);
        assert_eq!(body.sections[1].props.start, SectionStart::Continuous);

        assert_eq!(body.sections[2].items.len(), 1);
        assert_eq!(body.sections[2].props.page_w, Some(15840));
    }

    /// No `w:sectPr` at all — neither on a paragraph nor at the body's end —
    /// is one section with default properties, never zero sections.
    #[test]
    fn a_document_with_no_sectpr_at_all_is_one_default_section() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body><w:p><w:r><w:t>only content</w:t></w:r></w:p></w:body>
</w:document>"#;
        let mut report = ImportReport::default();
        let body = parse_document(xml, &mut report).expect("should parse");
        assert_eq!(body.sections.len(), 1);
        assert_eq!(body.sections[0].items.len(), 1);
        assert_eq!(body.sections[0].props.start, SectionStart::NextPage);
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
        let header_body = package
            .furniture
            .get("word/header1.xml")
            .expect("header1.xml in furniture");
        let BodyItem::Paragraph(p) = &header_body[0] else {
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
        let BodyItem::Paragraph(p) = &body[0] else { panic!("expected a paragraph") };
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert!(matches!(&r.content[0], RunContent::Text(t) if t == "snoska"));

        assert_eq!(parsed.endnotes.len(), 1);
        assert!(!parsed.endnotes.contains_key(&-1));
        let body = &parsed.endnotes[&1];
        let BodyItem::Paragraph(p) = &body[0] else { panic!("expected a paragraph") };
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
        package.add_media(
            "word/media/image1.png",
            "png",
            "image/png",
            vec![0x89, 0x50, 0x4E, 0x47],
        );
        package.add_relationships("word/document.xml", &doc_rels).unwrap();
        package
            .add_relationships("word/footnotes.xml", &footnotes_rels)
            .unwrap();
        let docx = package.finish(&Rels::new()).unwrap();

        let mut report = ImportReport::default();
        let package = parse_package(&docx, &mut report).unwrap();

        // The document-level `rId1` is untouched: still `styles.xml`.
        assert_eq!(package.rels["rId1"].target, "styles.xml");

        let body = &package.footnotes[&1];
        let BodyItem::Paragraph(p) = &body[0] else { panic!("expected a paragraph") };
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
        assert_eq!(
            r.content.len(),
            1,
            "the fallback must not add a second item: {:?}",
            r.content
        );
        let RunContent::TextBox(items) = &r.content[0] else {
            panic!("expected a text box")
        };
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
        let RunContent::TextBox(items) = &r.content[0] else {
            panic!("expected a text box")
        };
        assert_eq!(body_items_text(items), "VML box text");
    }

    /// A real VML *picture* (`v:imagedata`, no text box at all) must produce
    /// the same [`RunContent::Drawing`] a DrawingML picture would — real
    /// content that used to be dropped on the floor entirely.
    #[test]
    fn vml_picture_without_a_text_box_produces_a_drawing() {
        let p = parse_test_paragraph(
            r#"<w:r><w:pict xmlns:v="urn:schemas-microsoft-com:vml"
                            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                 <v:shape style="width:54pt;height:38.25pt"><v:imagedata r:id="rId8"/></v:shape>
               </w:pict></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 1);
        let RunContent::Drawing(d) = &r.content[0] else { panic!("expected a drawing") };
        assert_eq!(d.rel_id, "rId8");
        // 54pt/38.25pt converted to EMU (12700 per point).
        assert_eq!(d.cx_emu, Some(54 * 12700));
        assert_eq!(d.cy_emu, Some((38.25_f64 * 12700.0).round() as i64));
    }

    /// A `v:shape` with no `r:id` on its `v:imagedata` (malformed — never
    /// seen in practice, but must not panic or silently fabricate a
    /// relationship id) and no text box falls all the way through to
    /// [`RunContent::VmlUnsupported`], the same as any other shape with
    /// nothing this importer can extract.
    #[test]
    fn vml_imagedata_without_a_relationship_id_is_unsupported() {
        let p = parse_test_paragraph(
            r#"<w:r><w:pict xmlns:v="urn:schemas-microsoft-com:vml">
                 <v:shape><v:imagedata/></v:shape>
               </w:pict></w:r>"#,
        );

        assert_eq!(p.runs.len(), 1);
        let RunItem::Run(r) = &p.runs[0] else { panic!("expected a run") };
        assert_eq!(r.content.len(), 1);
        assert!(matches!(&r.content[0], RunContent::VmlUnsupported));
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
        assert_eq!(
            r.content.len(),
            2,
            "expected two separate text boxes: {:?}",
            r.content
        );
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
        let RunContent::TextBox(items) = &r.content[0] else {
            panic!("expected a text box")
        };
        let text = body_items_text(items);

        assert!(
            text.contains(&format!("level{depth}")),
            "expected the outermost nested level to survive:\n{text}"
        );
        assert!(
            !text.contains("innermost"),
            "content past the depth cap should be dropped:\n{text}"
        );
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
        assert_eq!(rel_id.rel_id.as_str(), "rId5");
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
        assert_eq!(
            data.categories,
            vec!["Category 1", "Category 2", "Category 3", "Category 4"]
        );
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

        assert_eq!(
            data.categories,
            vec!["Category 1", "Category 2", "Category 3", "Category 4"]
        );
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
        assert_eq!(data.kind, ChartKind::default());
    }

    /// [`ChartKind`] detection: one case per recognized local name, plus the
    /// two ways a classic chart ends up `Other` (an unrecognized type, or no
    /// `c:plotArea` at all).
    #[test]
    fn chart_kind_is_detected_from_the_plot_area_child_by_local_name() {
        let cases: &[(&str, ChartKind)] = &[
            ("barChart", ChartKind::Bar),
            ("bar3DChart", ChartKind::Bar),
            ("lineChart", ChartKind::Line),
            ("line3DChart", ChartKind::Line),
            ("scatterChart", ChartKind::Scatter),
            ("bubbleChart", ChartKind::Scatter),
            ("areaChart", ChartKind::Area),
            ("area3DChart", ChartKind::Area),
            ("pieChart", ChartKind::Other),
            ("doughnutChart", ChartKind::Other),
            ("radarChart", ChartKind::Other),
            ("stockChart", ChartKind::Other),
            ("surfaceChart", ChartKind::Other),
        ];
        for (tag, expected) in cases {
            let data = parse_test_chart_space(&format!(
                r#"<c:chart><c:plotArea><c:{tag}><c:ser></c:ser></c:{tag}></c:plotArea></c:chart>"#
            ));
            assert_eq!(data.kind, *expected, "unexpected kind for c:{tag}");
        }
    }

    #[test]
    fn chart_kind_defaults_to_other_with_no_plot_area_at_all() {
        let data = parse_test_chart_space("<c:chart></c:chart>");
        assert_eq!(data.kind, ChartKind::Other);
    }

    /// A combo chart (bars with a line series overlaid) nests more than one
    /// chart-type element under `c:plotArea`; the first one in document
    /// order wins, per [`ChartKind`]'s own doc comment.
    #[test]
    fn combo_chart_takes_the_first_plot_type_in_document_order() {
        let data = parse_test_chart_space(
            r#"<c:chart><c:plotArea>
                 <c:lineChart><c:ser></c:ser></c:lineChart>
                 <c:barChart><c:ser></c:ser></c:barChart>
               </c:plotArea></c:chart>"#,
        );
        assert_eq!(data.kind, ChartKind::Line);
    }

    /// `c:plotArea` also holds axis elements alongside the chart-type
    /// wrapper (`c:catAx`/`c:valAx` here, before the actual `c:barChart`) —
    /// those must be skipped rather than mistaken for "no recognized type".
    #[test]
    fn axis_elements_in_plot_area_do_not_confuse_kind_detection() {
        let data = parse_test_chart_space(
            r#"<c:chart><c:plotArea>
                 <c:catAx/><c:valAx/>
                 <c:barChart><c:ser></c:ser></c:barChart>
               </c:plotArea></c:chart>"#,
        );
        assert_eq!(data.kind, ChartKind::Bar);
    }

    /// ChartEx parts (`cx:chartSpace`) never carry a [`ChartKind`] other than
    /// `Other` — `lilaq` has no counterpart for any ChartEx type either, so
    /// there is nothing to detect.
    #[test]
    fn chartex_chart_always_keeps_the_default_other_kind() {
        let data = parse_test_chart_space(
            r#"<cx:chartData><cx:data id="0"/></cx:chartData>
               <cx:chart><cx:plotArea><cx:plotAreaRegion>
                 <cx:series layoutId="boxWhisker"><cx:dataId val="0"/></cx:series>
               </cx:plotAreaRegion></cx:plotArea></cx:chart>"#,
        );
        assert_eq!(data.kind, ChartKind::Other);
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
        assert_eq!(
            parsed.charts.len(),
            1,
            "expected only chart1.xml: {:?}",
            parsed.charts.keys()
        );
        assert!(parsed.charts.contains_key("word/charts/chart1.xml"));
    }

    // --- Resilient XML parsing (malformed companion parts / OMML fragments) ---

    #[test]
    fn text_offset_at_matches_a_known_row_col() {
        let text = "line one\nline two\nabc";
        assert_eq!(text_offset_at(text, TextPos::new(1, 1)), 0);
        assert_eq!(
            text_offset_at(text, TextPos::new(3, 1)),
            "line one\nline two\n".len()
        );
        assert_eq!(text_offset_at(text, TextPos::new(2, 6)), "line one\nline ".len());
    }

    #[test]
    fn remove_duplicate_attribute_excises_only_the_repeat() {
        let text = r#"<w:jc xmlns:w="ns" w:val="center" w:val="center"/>"#;
        let roxmltree::Error::DuplicatedAttribute(_, pos) =
            Document::parse(text).unwrap_err()
        else {
            panic!("expected a DuplicatedAttribute error")
        };
        let repaired = remove_duplicate_attribute(text, pos).expect("expected a repair");
        assert_eq!(repaired, r#"<w:jc xmlns:w="ns" w:val="center"/>"#);
        Document::parse(&repaired).expect("repaired text must actually parse");
    }

    #[test]
    fn tag_regions_finds_o_math_without_confusing_it_for_o_math_para() {
        let text =
            "<a><m:oMath>1</m:oMath><m:oMathPara><m:oMath>2</m:oMath></m:oMathPara></a>";
        let regions = tag_regions(text, "m:oMath");
        assert_eq!(regions.len(), 2, "{regions:?}");
        assert_eq!(&text[regions[0].0..regions[0].1], "<m:oMath>1</m:oMath>");
        assert_eq!(&text[regions[1].0..regions[1].1], "<m:oMath>2</m:oMath>");
    }

    #[test]
    fn drop_enclosing_equation_removes_only_the_broken_equation() {
        let text = r#"<w:p><w:r><w:t>before</w:t></w:r><m:oMath xmlns:m="ns"><m:r><m:t xml:space="preserve">a</m:sPre></m:r></m:oMath><w:r><w:t>after</w:t></w:r></w:p>"#;
        let offset = text.find("m:sPre").unwrap();
        let repaired =
            drop_enclosing_equation(text, offset).expect("expected the equation dropped");
        assert!(!repaired.contains("oMath"), "{repaired}");
        assert!(repaired.contains("before") && repaired.contains("after"), "{repaired}");
    }

    /// The `lo-sw-math-malformed_xml` corpus failure, reproduced directly: a
    /// mismatched closing tag inside an `m:oMath` (`m:t` opened, `m:sPre`
    /// closes it — `roxmltree`'s real `expected 'm:t' tag, not 'm:sPre'`
    /// error) used to abort the entire document. It must now degrade to
    /// dropping just that one equation.
    #[test]
    fn a_malformed_equation_degrades_instead_of_aborting_the_document() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                                  xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
          <w:body>
            <w:p><m:oMath><m:r><m:t xml:space="preserve">+</m:t></m:r><m:r><m:t xml:space="preserve">a</m:sPre></m:r></m:oMath></w:p>
            <w:p><w:r><w:t>still here</w:t></w:r></w:p>
          </w:body>
        </w:document>"#;
        let mut report = ImportReport::default();
        let body = parse_document(xml, &mut report).expect("should degrade, not abort");
        assert_eq!(body.sections.len(), 1);
        assert_eq!(body.sections[0].items.len(), 2);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.what == "word/document.xml" && n.detail.contains("OMML")),
            "{:?}",
            report.notes
        );
    }

    /// The `lo-sw-tdf165348_broken_package` corpus failure, reproduced
    /// directly: a duplicated `w:val` attribute (`roxmltree`'s real
    /// `attribute 'val' at 7:30 is already defined` error) used to abort the
    /// entire document too, even though it's nowhere near an equation.
    #[test]
    fn a_duplicate_attribute_degrades_instead_of_aborting_the_document() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:body>
            <w:p><w:pPr><w:jc w:val="center" w:val="center"/></w:pPr><w:r><w:t>hi</w:t></w:r></w:p>
          </w:body>
        </w:document>"#;
        let mut report = ImportReport::default();
        let body = parse_document(xml, &mut report).expect("should degrade, not abort");
        assert_eq!(body.sections.len(), 1);
        assert_eq!(body.sections[0].items.len(), 1);
        assert!(
            report.notes.iter().any(|n| n.detail.contains("duplicate")),
            "{:?}",
            report.notes
        );
    }

    /// Not every malformed `word/document.xml` is recoverable — one with no
    /// shape [`repair_xml`] recognises must still be fatal (there is no
    /// document without it).
    #[test]
    fn a_genuinely_unrecoverable_document_xml_is_still_fatal() {
        let xml = "<w:document><w:body><w:p><w:r><w:t>oops";
        let mut report = ImportReport::default();
        assert!(parse_document(xml, &mut report).is_err());
    }

    /// A malformed *companion* part (here, `styles.xml`, in a shape neither
    /// repair recognises) must not abort the whole import — it degrades to
    /// an empty `Styles` with a report note, the same as any other
    /// companion part, while `word/document.xml` — the one part with no
    /// such fallback — still imports normally.
    ///
    /// Assembled as a raw OPC zip (the `Package` writer used everywhere else
    /// in this module validates every part is well-formed XML before it will
    /// write one at all, which is exactly backwards for a test that needs a
    /// part to *not* be).
    #[test]
    fn a_malformed_companion_part_degrades_without_aborting_the_import() {
        use std::io::{Cursor, Write};

        const CT: &str = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        const RELS: &str = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
        const DOC: &str = "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body><w:p><w:r><w:t>hi</w:t></w:r></w:p></w:body></w:document>";
        // Malformed beyond either repair's reach: an unterminated element
        // with no recognisable shape to fix.
        const BAD_STYLES: &str = "<w:styles><w:style";

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (name, body) in [
            ("[Content_Types].xml", CT),
            ("_rels/.rels", RELS),
            ("word/document.xml", DOC),
            ("word/styles.xml", BAD_STYLES),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        let docx = zip.finish().unwrap().into_inner();

        let mut report = ImportReport::default();
        let parsed = parse_package(&docx, &mut report)
            .expect("must not abort on a bad companion part");
        assert_eq!(parsed.body.sections.len(), 1);
        assert_eq!(parsed.body.sections[0].items.len(), 1);
        assert!(parsed.styles.by_id.is_empty());
        assert!(
            report.notes.iter().any(|n| n.what == "styles.xml"),
            "{:?}",
            report.notes
        );
    }
}

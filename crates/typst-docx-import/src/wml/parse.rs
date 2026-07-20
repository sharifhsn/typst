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
    Body, BodyItem, BreakType, Cell, DrawingRef, LevelFormat, NumRef, Numbering, ParaProps,
    Paragraph, Relationship, Row, Run, RunContent, RunItem, RunProps, SectPr, Style, StyleKind,
    Styles, Table, WmlPackage,
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

    let rels = parse_rels(&mut reader)?;

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

    Ok(WmlPackage { body, styles, numbering, rels, media })
}

// --- Relationships -----------------------------------------------------------

fn parse_rels(reader: &mut Reader) -> Result<FxHashMap<EcoString, Relationship>, ImportError> {
    let mut rels = FxHashMap::default();
    let Some(xml) = reader.xml_part("word/_rels/document.xml.rels")? else {
        return Ok(rels);
    };
    let document = Document::parse(&xml).map_err(xml_err)?;
    for node in document.descendants().filter(|n| is_element(*n, "Relationship")) {
        let (Some(id), Some(target)) = (attr(node, "Id"), attr(node, "Target")) else {
            continue;
        };
        let external = attr(node, "TargetMode") == Some("External");
        rels.insert(id.into(), Relationship { target: target.into(), external });
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

    let mut body = Body::default();
    for child in body_node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "p" => body.items.push(BodyItem::Paragraph(parse_paragraph(child))),
            "tbl" => body.items.push(BodyItem::Table(parse_table(child))),
            "sectPr" => body.sect_pr = Some(parse_sectpr(child)),
            _ => {}
        }
    }
    Ok(body)
}

fn parse_paragraph(node: Node) -> Paragraph {
    let mut para = Paragraph::default();
    for child in node.children().filter(|n| n.is_element()) {
        if child.tag_name().name() == "pPr" {
            para.props = parse_para_props(child);
        } else {
            push_inline_child(&mut para.runs, child);
        }
    }
    para
}

/// Appends one paragraph-level content child (`w:r`, `w:hyperlink`, or a
/// `m:oMath`/`m:oMathPara` equation) as a [`RunItem`]. Everything else
/// (bookmarks, proofing errors, comment markers, revision wrappers) is
/// silently skipped — out of scope for v1.
fn push_inline_child(items: &mut Vec<RunItem>, child: Node) {
    match child.tag_name().name() {
        "r" => items.push(RunItem::Run(parse_run(child))),
        "hyperlink" => items.push(parse_hyperlink(child)),
        "oMath" => items.push(RunItem::Run(math_run(child))),
        "oMathPara" => {
            for m in child.children().filter(|n| is_element(*n, "oMath")) {
                items.push(RunItem::Run(math_run(m)));
            }
        }
        _ => {}
    }
}

fn parse_hyperlink(node: Node) -> RunItem {
    let rel_id = attr_ns(node, ns::R, "id").map(EcoString::from);
    let anchor = attr(node, "anchor").map(EcoString::from);
    let mut runs = Vec::new();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "r" => runs.push(parse_run(child)),
            "oMath" => runs.push(math_run(child)),
            "oMathPara" => {
                for m in child.children().filter(|n| is_element(*n, "oMath")) {
                    runs.push(math_run(m));
                }
            }
            _ => {}
        }
    }
    RunItem::Hyperlink { rel_id, anchor, runs }
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
            "tab" => run.content.push(RunContent::Tab),
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

fn parse_table(node: Node) -> Table {
    let mut table = Table::default();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "tblGrid" => {
                for col in child.children().filter(|n| is_element(*n, "gridCol")) {
                    if let Some(w) = attr(col, "w").and_then(parse_i64) {
                        table.grid.push(w);
                    }
                }
            }
            "tr" => table.rows.push(parse_row(child)),
            _ => {}
        }
    }
    table
}

fn parse_row(node: Node) -> Row {
    let mut row = Row::default();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "trPr" => {
                row.is_header =
                    child.children().any(|n| is_element(n, "tblHeader"));
            }
            "tc" => row.cells.push(parse_cell(child)),
            _ => {}
        }
    }
    row
}

fn parse_cell(node: Node) -> Cell {
    let mut cell =
        Cell { grid_span: 1, v_merge: None, shd_fill: None, content: Vec::new() };
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "tcPr" => parse_cell_props(child, &mut cell),
            "p" => cell.content.push(BodyItem::Paragraph(parse_paragraph(child))),
            "tbl" => cell.content.push(BodyItem::Table(parse_table(child))),
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
            _ => {}
        }
    }
    sect
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
        let RunContent::Text(text) = &runs[0].content[0] else { panic!("expected text") };
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
}

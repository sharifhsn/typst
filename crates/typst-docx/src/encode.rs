//! Serializes the typed DOCX IR into an OPC zip package.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::Smart;
use typst_library::model::DocumentInfo;

use crate::dom::{
    Anchor, AnchorPos, AnchorWrap, Block, Border, Cell, CellBorders, Drawing, DocxDocument,
    Field, Footnote, HdrFtrPart, Para, ParaChild, Row, Run, SectPr, SectType, ShapeGeom,
    ShapeSpec, Tbl, Toc, VAlign, VMerge,
};
use crate::package::{Package, RelMode, Rels};
use crate::styles_part;
use crate::xml::{self, XmlWriter};

/// Settings for DOCX export.
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct DocxOptions {
    /// Whether to pretty-print the XML parts.
    pub pretty: bool,
}

// Relationship-type URIs.
const REL_OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const REL_CORE_PROPS: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_EXTENDED_PROPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const REL_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const REL_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const REL_FOOTNOTES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const REL_SETTINGS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";

// Content types.
const CT_DOCUMENT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const CT_STYLES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
const CT_NUMBERING: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
const CT_SETTINGS: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
const CT_CORE: &str =
    "application/vnd.openxmlformats-package.core-properties+xml";
const CT_EXTENDED: &str =
    "application/vnd.openxmlformats-officedocument.extended-properties+xml";
const CT_HEADER: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
const CT_FOOTER: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml";

/// Serializes a DOCX document into the OPC zip bytes.
#[typst_macros::time(name = "docx encode")]
pub fn docx(document: &DocxDocument, options: &DocxOptions) -> SourceResult<Vec<u8>> {
    let pretty = options.pretty;
    let mut package = Package::new();
    let mut root_rels = Rels::new();

    // The document's own relationships (images/hyperlinks accumulated during
    // conversion, plus styles/numbering/footnotes/settings added here).
    let mut doc_rels = clone_rels(&document.doc_rels);

    // -- word/styles.xml --
    let styles_xml =
        styles_part::build(&document.info, document.max_heading_level, pretty);
    package.add_xml("word/styles.xml", CT_STYLES, styles_xml);
    doc_rels.add(REL_STYLES, "styles.xml", RelMode::Internal);

    // -- word/settings.xml --
    let settings_xml = build_settings(document, pretty);
    package.add_xml("word/settings.xml", CT_SETTINGS, settings_xml);
    doc_rels.add(REL_SETTINGS, "settings.xml", RelMode::Internal);

    // -- word/numbering.xml (conditional) --
    if !document.numbering.abstracts.is_empty() {
        let numbering_xml = build_numbering(document, pretty);
        package.add_xml("word/numbering.xml", CT_NUMBERING, numbering_xml);
        doc_rels.add(REL_NUMBERING, "numbering.xml", RelMode::Internal);
    }

    // -- word/footnotes.xml (conditional) --
    if !document.footnotes.is_empty() {
        let footnotes_xml = build_footnotes(document, pretty);
        package.add_xml("word/footnotes.xml", CT_FOOTNOTES, footnotes_xml);
        doc_rels.add(REL_FOOTNOTES, "footnotes.xml", RelMode::Internal);
    }

    // -- header/footer parts --
    // The relationships were already registered in `doc_rels` during section
    // resolution (document.rs), so the `r:id` on each headerReference/
    // footerReference matches the Relationship here. We only emit the part file
    // + its content-type Override.
    for part in &document.header_parts {
        let xml = build_hdrftr(part, pretty);
        package.add_xml(&format!("word/{}", part.part_name), CT_HEADER, xml);
    }
    for part in &document.footer_parts {
        let xml = build_hdrftr(part, pretty);
        package.add_xml(&format!("word/{}", part.part_name), CT_FOOTER, xml);
    }

    // -- media parts --
    for media in &document.media {
        package.add_media(
            &media.part_name,
            &media.ext,
            media_content_type(&media.ext),
            media.bytes.clone(),
        );
    }

    // -- word/document.xml --
    let document_xml = build_document(document, pretty);
    package.add_xml("word/document.xml", CT_DOCUMENT, document_xml);

    // -- word/_rels/document.xml.rels --
    package.add_xml(
        "word/_rels/document.xml.rels",
        "application/vnd.openxmlformats-package.relationships+xml",
        doc_rels.to_xml(),
    );

    // -- docProps/core.xml + app.xml --
    package.add_xml("docProps/core.xml", CT_CORE, build_core(&document.info, pretty));
    package.add_xml("docProps/app.xml", CT_EXTENDED, build_app(pretty));

    // -- package root relationships --
    root_rels.add(REL_OFFICE_DOCUMENT, "word/document.xml", RelMode::Internal);
    root_rels.add(REL_CORE_PROPS, "docProps/core.xml", RelMode::Internal);
    root_rels.add(REL_EXTENDED_PROPS, "docProps/app.xml", RelMode::Internal);

    Ok(package.finish(&root_rels))
}

/// Clones the conversion-time `doc_rels` so `encode` can append the static
/// document parts (styles/numbering/footnotes/settings) without mutating the
/// document's own relationship table.
fn clone_rels(src: &Rels) -> Rels {
    src.clone()
}

/// Picks the content type for a media extension.
fn media_content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// word/document.xml
// ---------------------------------------------------------------------------

/// Declares the OOXML namespace prefixes on a part's root element.
///
/// Each part (`document.xml`, `header*.xml`, `footer*.xml`, `footnotes.xml`) is
/// parsed standalone, so every prefix it uses must be declared on its OWN root.
/// A drawing (`wp:`/`a:`/`pic:`) or math (`m:`) inside a header/footer/footnote
/// therefore needs these here too — otherwise the prefix is undefined and strict
/// consumers (Microsoft Word, LibreOffice) refuse to load the whole document.
/// Declaring an unused namespace is harmless, so all parts get the full set.
fn decl_ooxml_namespaces(w: &mut XmlWriter) {
    w.attr("xmlns:w", "http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        .attr("xmlns:r", "http://schemas.openxmlformats.org/officeDocument/2006/relationships")
        .attr("xmlns:m", "http://schemas.openxmlformats.org/officeDocument/2006/math")
        .attr(
            "xmlns:wp",
            "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
        )
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:pic", "http://schemas.openxmlformats.org/drawingml/2006/picture")
        .attr("xmlns:mc", "http://schemas.openxmlformats.org/markup-compatibility/2006");
}

fn build_document(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open(xml::W_DOCUMENT);
    decl_ooxml_namespaces(&mut w);
    w.attr("mc:Ignorable", "w14 wp14").start_children();

    w.open(xml::W_BODY).start_children();

    let mut ends_with_para = false;
    for block in &document.body {
        ends_with_para = write_block(&mut w, block);
    }

    // The body must end in a paragraph before the sectPr.
    if !ends_with_para {
        w.leaf(xml::W_P);
    }

    write_sectpr(&mut w, &document.sect);

    w.close(); // w:body
    w.close(); // w:document
    w.finish()
}

/// Writes a block; returns whether it ended with a paragraph.
fn write_block(w: &mut XmlWriter, block: &Block) -> bool {
    match block {
        Block::Para(para) => {
            write_para(w, para);
            true
        }
        Block::Table(tbl) => {
            write_table(w, tbl);
            // A table must be followed by a paragraph in Word; report "not a
            // paragraph" so the body terminator inserts one if this is the last
            // block.
            false
        }
        Block::Toc(toc) => {
            write_toc(w, toc);
            true
        }
        Block::SectionBreak(sect) => {
            // A non-final section ends with a paragraph whose `pPr` carries the
            // ending section's `sectPr` (the final section's `sectPr` lives at
            // body level). This both delimits the section and applies its page
            // geometry to all preceding content; the default `nextPage` type
            // also performs the page transition.
            w.open(xml::W_P).start_children();
            w.open(xml::W_PPR).start_children();
            write_sectpr(w, sect);
            w.close(); // pPr
            w.close(); // p
            true
        }
        Block::Tag(_) => false,
    }
}

fn write_para(w: &mut XmlWriter, para: &Para) {
    w.open(xml::W_P).start_children();
    para.props.write_ppr(w);
    for child in &para.content {
        write_para_child(w, child);
    }
    w.close();
}

/// Serializes a `w:tbl` (grid + rows + cells) into `document.xml`.
fn write_table(w: &mut XmlWriter, tbl: &Tbl) {
    w.open("w:tbl").start_children();

    // -- w:tblPr -----------------------------------------------------------
    w.open("w:tblPr").start_children();
    if let Some(width) = tbl.props.width_dxa {
        w.open("w:tblW")
            .attr("w:w", &width.to_string())
            .attr("w:type", "dxa")
            .empty();
    } else {
        w.open("w:tblW").attr("w:w", "0").attr("w:type", "auto").empty();
    }
    // A visible single-line border on all edges + insides, so the table is not
    // borderless by default (Word's no-style default is invisible).
    w.open("w:tblBorders").start_children();
    for edge in ["w:top", "w:left", "w:bottom", "w:right", "w:insideH", "w:insideV"] {
        w.open(edge)
            .attr("w:val", "single")
            .attr("w:sz", "4")
            .attr("w:space", "0")
            .attr("w:color", "auto")
            .empty();
    }
    w.close(); // w:tblBorders
    w.open("w:tblLayout").attr("w:type", "fixed").empty();
    w.close(); // w:tblPr

    // -- w:tblGrid ---------------------------------------------------------
    w.open("w:tblGrid").start_children();
    for col in &tbl.grid {
        w.open("w:gridCol").attr("w:w", &col.to_string()).empty();
    }
    w.close(); // w:tblGrid

    // -- rows --------------------------------------------------------------
    for row in &tbl.rows {
        write_row(w, row);
    }

    w.close(); // w:tbl
}

fn write_row(w: &mut XmlWriter, row: &Row) {
    w.open("w:tr").start_children();

    if row.header || row.cant_split || row.height.is_some() {
        w.open("w:trPr").start_children();
        if let Some(h) = &row.height {
            w.open("w:trHeight")
                .attr("w:val", &h.val.to_string())
                .attr("w:hRule", if h.exact { "exact" } else { "atLeast" })
                .empty();
        }
        if row.header {
            w.open("w:tblHeader").empty();
        }
        if row.cant_split {
            w.open("w:cantSplit").empty();
        }
        w.close(); // w:trPr
    }

    for cell in &row.cells {
        write_cell(w, cell);
    }

    w.close(); // w:tr
}

fn write_cell(w: &mut XmlWriter, cell: &Cell) {
    w.open("w:tc").start_children();

    // -- w:tcPr ------------------------------------------------------------
    w.open("w:tcPr").start_children();
    if let Some(width) = cell.w_dxa {
        w.open("w:tcW")
            .attr("w:w", &width.to_string())
            .attr("w:type", "dxa")
            .empty();
    }
    if cell.grid_span > 1 {
        w.open("w:gridSpan").attr("w:val", &cell.grid_span.to_string()).empty();
    }
    match cell.v_merge {
        Some(VMerge::Restart) => {
            w.open("w:vMerge").attr("w:val", "restart").empty();
        }
        Some(VMerge::Continue) => {
            w.open("w:vMerge").attr("w:val", "continue").empty();
        }
        None => {}
    }
    write_cell_borders(w, &cell.borders);
    if let Some([r, g, b]) = cell.shd_fill {
        w.open("w:shd")
            .attr("w:val", "clear")
            .attr("w:color", "auto")
            .attr("w:fill", &format!("{r:02X}{g:02X}{b:02X}"))
            .empty();
    }
    if let Some(valign) = cell.valign {
        let v = match valign {
            VAlign::Top => "top",
            VAlign::Center => "center",
            VAlign::Bottom => "bottom",
        };
        w.open("w:vAlign").attr("w:val", v).empty();
    }
    w.close(); // w:tcPr

    // -- cell content (always ≥1 block, ending in a w:p) -------------------
    for block in &cell.blocks {
        write_block(w, block);
    }

    w.close(); // w:tc
}

fn write_cell_borders(w: &mut XmlWriter, borders: &CellBorders) {
    if borders.top.is_none()
        && borders.bottom.is_none()
        && borders.left.is_none()
        && borders.right.is_none()
    {
        return;
    }
    w.open("w:tcBorders").start_children();
    write_border_side(w, "w:top", &borders.top);
    write_border_side(w, "w:left", &borders.left);
    write_border_side(w, "w:bottom", &borders.bottom);
    write_border_side(w, "w:right", &borders.right);
    w.close();
}

fn write_border_side(w: &mut XmlWriter, name: &'static str, border: &Option<Border>) {
    match border {
        Some(b) => {
            let [r, g, b_] = b.color;
            w.open(name)
                .attr("w:val", "single")
                .attr("w:sz", &b.sz.to_string())
                .attr("w:space", "0")
                .attr("w:color", &format!("{r:02X}{g:02X}{b_:02X}"))
                .empty();
        }
        None => {
            // Turn the inherited edge off explicitly.
            w.open(name).attr("w:val", "nil").empty();
        }
    }
}

/// Serializes a `w:drawing` — an inline (`<wp:inline>`) or floating
/// (`<wp:anchor>`) DrawingML picture, sharing the same `a:graphic`/`pic:pic`
/// payload.
fn write_drawing(w: &mut XmlWriter, d: &Drawing) {
    w.open(xml::W_R).start_children();
    w.open("w:drawing").start_children();
    match &d.anchor {
        None => write_inline_envelope(w, d),
        Some(a) => write_anchor_envelope(w, d, a),
    }
    w.close(); // w:drawing
    w.close(); // w:r
}

/// Emits the `<wp:inline>` envelope (the original inline image body).
fn write_inline_envelope(w: &mut XmlWriter, d: &Drawing) {
    w.open("wp:inline")
        .attr("distT", "0")
        .attr("distB", "0")
        .attr("distL", "0")
        .attr("distR", "0")
        .start_children();
    w.open("wp:extent")
        .attr("cx", &d.w_emu.to_string())
        .attr("cy", &d.h_emu.to_string())
        .empty();
    w.open("wp:effectExtent")
        .attr("l", "0")
        .attr("t", "0")
        .attr("r", "0")
        .attr("b", "0")
        .empty();
    w.open("wp:docPr")
        .attr("id", &d.docpr_id.to_string())
        .attr("name", &d.name);
    if let Some(alt) = &d.alt {
        w.attr("descr", alt);
    }
    w.empty();
    w.open("wp:cNvGraphicFramePr").start_children();
    w.open("a:graphicFrameLocks")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("noChangeAspect", "1")
        .empty();
    w.close(); // wp:cNvGraphicFramePr
    write_pic_payload(w, d);
    w.close(); // wp:inline
}

/// Emits the `<wp:anchor>` envelope (a floating image), wrapping the same
/// `a:graphic`/`pic:pic` payload as the inline form. The five required
/// booleans/uint (`simplePos`/`relativeHeight`/`behindDoc`/`locked`/
/// `layoutInCell`/`allowOverlap`) MUST all be present or Word repairs the file.
fn write_anchor_envelope(w: &mut XmlWriter, d: &Drawing, a: &Anchor) {
    w.open("wp:anchor")
        .attr("distT", &a.dist[0].to_string())
        .attr("distB", &a.dist[1].to_string())
        .attr("distL", &a.dist[2].to_string())
        .attr("distR", &a.dist[3].to_string())
        .attr("simplePos", "0")
        .attr("relativeHeight", &a.z.to_string())
        .attr("behindDoc", "0")
        .attr("locked", "0")
        .attr("layoutInCell", "1")
        .attr("allowOverlap", "1")
        .start_children();
    // Required even when simplePos="0".
    w.open("wp:simplePos").attr("x", "0").attr("y", "0").empty();
    write_anchor_pos(w, "wp:positionH", &a.pos_h);
    write_anchor_pos(w, "wp:positionV", &a.pos_v);
    w.open("wp:extent")
        .attr("cx", &d.w_emu.to_string())
        .attr("cy", &d.h_emu.to_string())
        .empty();
    w.open("wp:effectExtent")
        .attr("l", "0")
        .attr("t", "0")
        .attr("r", "0")
        .attr("b", "0")
        .empty();
    match a.wrap {
        AnchorWrap::TopAndBottom => w.leaf("wp:wrapTopAndBottom"),
        AnchorWrap::Square(text) => {
            w.open("wp:wrapSquare").attr("wrapText", text).empty();
        }
        AnchorWrap::None => w.leaf("wp:wrapNone"),
    }
    w.open("wp:docPr")
        .attr("id", &d.docpr_id.to_string())
        .attr("name", &d.name);
    if let Some(alt) = &d.alt {
        w.attr("descr", alt);
    }
    w.empty();
    w.open("wp:cNvGraphicFramePr").start_children();
    w.open("a:graphicFrameLocks")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("noChangeAspect", "1")
        .empty();
    w.close(); // wp:cNvGraphicFramePr
    write_pic_payload(w, d);
    w.close(); // wp:anchor
}

/// Emits one `<wp:positionH>` / `<wp:positionV>` carrying exactly one of
/// `<wp:align>` / `<wp:posOffset>`.
fn write_anchor_pos(w: &mut XmlWriter, name: &'static str, pos: &AnchorPos) {
    w.open(name).attr("relativeFrom", pos.rel_from).start_children();
    if let Some(align) = pos.align {
        w.open("wp:align").start_children();
        w.text(align);
        w.close();
    } else {
        let off = pos.offset.unwrap_or(0);
        w.open("wp:posOffset").start_children();
        w.text(&off.to_string());
        w.close();
    }
    w.close(); // wp:positionH/V
}

/// Emits the shared `<a:graphic>`/`<pic:pic>` payload (identical for inline and
/// anchored drawings).
fn write_pic_payload(w: &mut XmlWriter, d: &Drawing) {
    if let Some(shape) = &d.shape {
        write_shape_payload(w, d, shape);
        return;
    }
    w.open("a:graphic")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .start_children();
    w.open("a:graphicData")
        .attr("uri", "http://schemas.openxmlformats.org/drawingml/2006/picture")
        .start_children();
    w.open("pic:pic")
        .attr("xmlns:pic", "http://schemas.openxmlformats.org/drawingml/2006/picture")
        .start_children();
    // pic:nvPicPr
    w.open("pic:nvPicPr").start_children();
    w.open("pic:cNvPr").attr("id", &d.docpr_id.to_string()).attr("name", &d.name);
    if let Some(alt) = &d.alt {
        w.attr("descr", alt);
    }
    w.empty();
    w.open("pic:cNvPicPr").empty();
    w.close(); // pic:nvPicPr
    // pic:blipFill
    w.open("pic:blipFill").start_children();
    w.open("a:blip").attr("r:embed", &d.rel).empty();
    w.open("a:stretch").start_children();
    w.open("a:fillRect").empty();
    w.close(); // a:stretch
    w.close(); // pic:blipFill
    // pic:spPr
    w.open("pic:spPr").start_children();
    w.open("a:xfrm").start_children();
    w.open("a:off").attr("x", "0").attr("y", "0").empty();
    w.open("a:ext")
        .attr("cx", &d.w_emu.to_string())
        .attr("cy", &d.h_emu.to_string())
        .empty();
    w.close(); // a:xfrm
    w.open("a:prstGeom").attr("prst", "rect").start_children();
    w.open("a:avLst").empty();
    w.close(); // a:prstGeom
    w.close(); // pic:spPr
    w.close(); // pic:pic
    w.close(); // a:graphicData
    w.close(); // a:graphic
}

/// Emits a vector DrawingML shape (`wps:wsp`) payload in place of `pic:pic`.
fn write_shape_payload(w: &mut XmlWriter, d: &Drawing, shape: &ShapeSpec) {
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const WPS: &str =
        "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";
    let hex = |c: [u8; 3]| format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]);

    w.open("a:graphic").attr("xmlns:a", A).start_children();
    w.open("a:graphicData").attr("uri", WPS).start_children();
    w.open("wps:wsp").attr("xmlns:wps", WPS).start_children();
    w.open("wps:cNvSpPr").empty();
    w.open("wps:spPr").start_children();

    w.open("a:xfrm").start_children();
    w.open("a:off").attr("x", "0").attr("y", "0").empty();
    w.open("a:ext")
        .attr("cx", &d.w_emu.to_string())
        .attr("cy", &d.h_emu.to_string())
        .empty();
    w.close(); // a:xfrm

    match &shape.geom {
        ShapeGeom::Rect | ShapeGeom::RoundRect | ShapeGeom::Ellipse => {
            let prst = match shape.geom {
                ShapeGeom::RoundRect => "roundRect",
                ShapeGeom::Ellipse => "ellipse",
                _ => "rect",
            };
            w.open("a:prstGeom").attr("prst", prst).start_children();
            w.open("a:avLst").empty();
            w.close();
        }
        ShapeGeom::Path { points, closed } => {
            let (cx, cy) = (d.w_emu.to_string(), d.h_emu.to_string());
            w.open("a:custGeom").start_children();
            for empty in ["a:avLst", "a:gdLst", "a:ahLst", "a:cxnLst"] {
                w.open(empty).empty();
            }
            w.open("a:rect")
                .attr("l", "0")
                .attr("t", "0")
                .attr("r", &cx)
                .attr("b", &cy)
                .empty();
            w.open("a:pathLst").start_children();
            w.open("a:path").attr("w", &cx).attr("h", &cy).start_children();
            if let Some((x0, y0)) = points.first() {
                w.open("a:moveTo").start_children();
                w.open("a:pt").attr("x", &x0.to_string()).attr("y", &y0.to_string()).empty();
                w.close();
                for (x, y) in &points[1..] {
                    w.open("a:lnTo").start_children();
                    w.open("a:pt").attr("x", &x.to_string()).attr("y", &y.to_string()).empty();
                    w.close();
                }
                if *closed {
                    w.open("a:close").empty();
                }
            }
            w.close(); // a:path
            w.close(); // a:pathLst
            w.close(); // a:custGeom
        }
    }

    match shape.fill {
        Some(c) => {
            w.open("a:solidFill").start_children();
            w.open("a:srgbClr").attr("val", &hex(c)).empty();
            w.close();
        }
        None => {
            w.open("a:noFill").empty();
        }
    }
    match &shape.stroke {
        Some(s) => {
            w.open("a:ln").attr("w", &s.w_emu.to_string()).start_children();
            w.open("a:solidFill").start_children();
            w.open("a:srgbClr").attr("val", &hex(s.color)).empty();
            w.close();
            w.close(); // a:ln
        }
        None => {
            w.open("a:ln").start_children();
            w.open("a:noFill").empty();
            w.close();
        }
    }

    w.close(); // wps:spPr

    match &shape.txbx {
        // A text box: real editable paragraphs framed by the shape.
        Some(tb) => {
            w.open("wps:txbx").start_children();
            w.open("w:txbxContent").start_children();
            let mut ends_with_para = false;
            for block in &tb.blocks {
                ends_with_para = write_block(w, block);
            }
            // `w:txbxContent` (like the document body) must end with a paragraph;
            // this also gives an empty text box its one required paragraph.
            if !ends_with_para {
                w.leaf(xml::W_P);
            }
            w.close(); // w:txbxContent
            w.close(); // wps:txbx
            // Reproduce the box inset as the text-frame insets, and auto-fit the
            // frame to the text so Word can re-flow it when edited.
            w.open("wps:bodyPr")
                .attr("wrap", "square")
                .attr("lIns", &tb.ins[0].to_string())
                .attr("tIns", &tb.ins[1].to_string())
                .attr("rIns", &tb.ins[2].to_string())
                .attr("bIns", &tb.ins[3].to_string())
                .attr("anchor", "t")
                .start_children();
            w.leaf("a:spAutoFit");
            w.close(); // wps:bodyPr
        }
        None => {
            w.open("wps:bodyPr").empty();
        }
    }

    w.close(); // wps:wsp
    w.close(); // a:graphicData
    w.close(); // a:graphic
}

fn write_para_child(w: &mut XmlWriter, child: &ParaChild) {
    match child {
        ParaChild::Run(run) => write_run(w, run),
        ParaChild::OmmlPara(xml_str) => w.raw(xml_str),
        ParaChild::Hyperlink { rel, anchor, runs } => {
            w.open(xml::W_HYPERLINK);
            if let Some(rel) = rel {
                w.attr("r:id", rel);
            }
            if let Some(anchor) = anchor {
                w.attr("w:anchor", anchor);
            }
            w.attr("w:history", "1").start_children();
            for run in runs {
                write_run(w, run);
            }
            w.close();
        }
        ParaChild::BookmarkStart { id, name } => {
            w.open(xml::W_BOOKMARK_START)
                .attr("w:id", &id.to_string())
                .attr("w:name", name)
                .empty();
        }
        ParaChild::BookmarkEnd { id } => {
            w.open(xml::W_BOOKMARK_END).attr("w:id", &id.to_string()).empty();
        }
        ParaChild::Tag(_) => {}
    }
}

fn write_run(w: &mut XmlWriter, run: &Run) {
    match run {
        Run::Text { props, text } => {
            w.open(xml::W_R).start_children();
            props.write_rpr(w);
            w.open(xml::W_T).attr("xml:space", "preserve").start_children();
            w.text(text);
            w.close(); // w:t
            w.close(); // w:r
        }
        Run::Break => {
            w.open(xml::W_R).start_children();
            w.leaf(xml::W_BR);
            w.close();
        }
        Run::PageBreak => {
            w.open(xml::W_R).start_children();
            w.open(xml::W_BR).attr("w:type", "page").empty();
            w.close();
        }
        Run::Tab | Run::FillTab => {
            w.open(xml::W_R).start_children();
            w.leaf(xml::W_TAB);
            w.close();
        }
        Run::FootnoteRef { props, id } => {
            w.open(xml::W_R).start_children();
            props.write_rpr(w);
            w.open(xml::W_FOOTNOTE_REF).attr("w:id", &id.to_string()).empty();
            w.close();
        }
        Run::FootnoteRefMark => {
            w.open(xml::W_R).start_children();
            w.open(xml::W_RPR).start_children();
            w.open(xml::W_RSTYLE).attr(xml::W_VAL, "FootnoteReference").empty();
            w.close(); // rPr
            w.open(xml::W_FOOTNOTE_REF_MARK).empty();
            w.close(); // r
        }
        Run::Drawing(drawing) => write_drawing(w, drawing),
        Run::OmmlInline(xml_str) => {
            w.raw(xml_str);
        }
        Run::Field(field) => write_field(w, field),
    }
}

fn write_field(w: &mut XmlWriter, field: &Field) {
    // begin
    w.open(xml::W_R).start_children();
    let fld = w.open("w:fldChar").attr("w:fldCharType", "begin");
    if field.dirty {
        fld.attr("w:dirty", "true");
    }
    w.empty();
    w.close();
    // instrText
    w.open(xml::W_R).start_children();
    w.open("w:instrText").attr("xml:space", "preserve").start_children();
    w.text(&field.instr);
    w.close();
    w.close();
    // separate
    w.open(xml::W_R).start_children();
    w.open("w:fldChar").attr("w:fldCharType", "separate").empty();
    w.close();
    // cached result
    for run in &field.result {
        write_run(w, run);
    }
    // end
    w.open(xml::W_R).start_children();
    w.open("w:fldChar").attr("w:fldCharType", "end").empty();
    w.close();
}

/// Serializes a table of contents. With baked entries the `TOC` field spans
/// several paragraphs: `begin`/`instrText`/`separate` lead the first entry and
/// `end` closes the last, so the entries are the field's cached result and show
/// without a manual update. With no entries, falls back to a single-paragraph
/// field holding the placeholder runs.
fn write_toc(w: &mut XmlWriter, toc: &Toc) {
    // Emits the field `begin` + instruction + `separate` run sequence.
    let write_begin = |w: &mut XmlWriter| {
        w.open(xml::W_R).start_children();
        let fld = w.open("w:fldChar").attr("w:fldCharType", "begin");
        if toc.dirty {
            fld.attr("w:dirty", "true");
        }
        w.empty();
        w.close();
        w.open(xml::W_R).start_children();
        w.open("w:instrText").attr("xml:space", "preserve").start_children();
        w.text(&toc.instr);
        w.close();
        w.close();
        w.open(xml::W_R).start_children();
        w.open("w:fldChar").attr("w:fldCharType", "separate").empty();
        w.close();
    };
    let write_end = |w: &mut XmlWriter| {
        w.open(xml::W_R).start_children();
        w.open("w:fldChar").attr("w:fldCharType", "end").empty();
        w.close();
    };

    if toc.entries.is_empty() {
        w.open(xml::W_P).start_children();
        write_begin(w);
        for run in &toc.fallback {
            write_run(w, run);
        }
        write_end(w);
        w.close();
        return;
    }

    let last = toc.entries.len() - 1;
    for (i, para) in toc.entries.iter().enumerate() {
        w.open(xml::W_P).start_children();
        para.props.write_ppr(w);
        if i == 0 {
            write_begin(w);
        }
        for child in &para.content {
            write_para_child(w, child);
        }
        if i == last {
            write_end(w);
        }
        w.close(); // w:p
    }
}

fn write_sectpr(w: &mut XmlWriter, sect: &SectPr) {
    w.open(xml::W_SECTPR).start_children();

    // CT_SectPr child order is strict: headerReference/footerReference precede
    // everything, then (type) → pgSz → pgMar → pgNumType → cols → titlePg.
    for h in &sect.headers {
        w.open("w:headerReference")
            .attr("w:type", h.kind)
            .attr("r:id", &h.rel)
            .empty();
    }
    for f in &sect.footers {
        w.open("w:footerReference")
            .attr("w:type", f.kind)
            .attr("r:id", &f.rel)
            .empty();
    }
    if let Some(t) = sect.sect_type {
        let v = match t {
            SectType::NextPage => "nextPage",
            SectType::EvenPage => "evenPage",
            SectType::OddPage => "oddPage",
            SectType::Continuous => "continuous",
        };
        w.open("w:type").attr("w:val", v).empty();
    }

    let pg = w.open("w:pgSz")
        .attr("w:w", &sect.page_w.to_string())
        .attr("w:h", &sect.page_h.to_string());
    if sect.landscape {
        pg.attr("w:orient", "landscape");
    }
    w.empty();
    w.open("w:pgMar")
        .attr("w:top", &sect.margin_top.to_string())
        .attr("w:right", &sect.margin_right.to_string())
        .attr("w:bottom", &sect.margin_bottom.to_string())
        .attr("w:left", &sect.margin_left.to_string())
        .attr("w:header", &sect.header.to_string())
        .attr("w:footer", &sect.footer.to_string())
        .attr("w:gutter", &sect.gutter.to_string())
        .empty();
    if let Some(pn) = &sect.pg_num {
        w.open("w:pgNumType").attr("w:fmt", pn.fmt);
        if let Some(s) = pn.start {
            w.attr("w:start", &s.to_string());
        }
        w.empty();
    }
    if sect.columns > 1 {
        w.open("w:cols")
            .attr("w:num", &sect.columns.to_string())
            .attr("w:space", &sect.col_space.to_string())
            .attr("w:equalWidth", "1")
            .empty();
    } else {
        w.open("w:cols").attr("w:space", &sect.col_space.to_string()).empty();
    }
    if sect.title_pg {
        w.leaf("w:titlePg");
    }
    w.close();
}

/// Serializes a header (`w:hdr`) or footer (`w:ftr`) part. Never emits an empty
/// root: a trailing empty `<w:p/>` is appended if the content doesn't end in a
/// paragraph (a bare `w:hdr`/`w:ftr` is non-conformant in some Word builds).
fn build_hdrftr(part: &HdrFtrPart, pretty: bool) -> String {
    let root = if part.is_header { "w:hdr" } else { "w:ftr" };
    let mut w = XmlWriter::new(pretty);
    w.open(root);
    decl_ooxml_namespaces(&mut w);
    w.start_children();
    let mut ends_with_para = false;
    for block in &part.blocks {
        ends_with_para = write_block(&mut w, block);
    }
    if !ends_with_para {
        w.leaf(xml::W_P);
    }
    w.close();
    w.finish()
}

// ---------------------------------------------------------------------------
// word/settings.xml
// ---------------------------------------------------------------------------

fn build_settings(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("w:settings")
        .attr("xmlns:w", "http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        .start_children();
    if document.uses_fields {
        w.open("w:updateFields").attr(xml::W_VAL, "true").empty();
    }
    if !document.footnotes.is_empty() {
        w.open("w:footnotePr").start_children();
        w.open("w:footnote").attr("w:id", "-1").empty();
        w.open("w:footnote").attr("w:id", "0").empty();
        w.close();
    }
    w.close();
    w.finish()
}

// ---------------------------------------------------------------------------
// word/numbering.xml
// ---------------------------------------------------------------------------

fn build_numbering(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("w:numbering")
        .attr("xmlns:w", "http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        .start_children();

    for abs in &document.numbering.abstracts {
        w.open("w:abstractNum")
            .attr("w:abstractNumId", &abs.id.to_string())
            .start_children();
        w.open("w:multiLevelType").attr(xml::W_VAL, abs.multilevel.as_str()).empty();
        for (i, level) in abs.levels.iter().enumerate() {
            w.open("w:lvl").attr("w:ilvl", &i.to_string()).start_children();
            w.open("w:start").attr(xml::W_VAL, &level.start.to_string()).empty();
            w.open("w:numFmt").attr(xml::W_VAL, level.num_fmt.as_str()).empty();
            w.open("w:lvlText").attr(xml::W_VAL, &level.lvl_text).empty();
            w.open("w:lvlJc").attr(xml::W_VAL, "left").empty();
            w.open(xml::W_PPR).start_children();
            w.open("w:ind")
                .attr("w:left", &level.ind_left.to_string())
                .attr("w:hanging", &level.ind_hanging.to_string())
                .empty();
            w.close();
            if let Some(font) = &level.bullet_font {
                w.open(xml::W_RPR).start_children();
                w.open(xml::W_RFONTS)
                    .attr("w:ascii", font)
                    .attr("w:hAnsi", font)
                    .attr("w:cs", font)
                    .empty();
                w.close();
            }
            w.close(); // w:lvl
        }
        w.close(); // w:abstractNum
    }

    for num in &document.numbering.nums {
        w.open("w:num").attr("w:numId", &num.num_id.to_string()).start_children();
        w.open("w:abstractNumId").attr(xml::W_VAL, &num.abstract_id.to_string()).empty();
        if let Some(start) = num.start_override {
            w.open("w:lvlOverride").attr("w:ilvl", "0").start_children();
            w.open("w:startOverride").attr(xml::W_VAL, &start.to_string()).empty();
            w.close();
        }
        w.close();
    }

    w.close();
    w.finish()
}

// ---------------------------------------------------------------------------
// word/footnotes.xml
// ---------------------------------------------------------------------------

fn build_footnotes(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("w:footnotes");
    decl_ooxml_namespaces(&mut w);
    w.start_children();

    // Separators.
    write_separator(&mut w, -1, "separator");
    write_separator(&mut w, 0, "continuationSeparator");

    for footnote in &document.footnotes {
        write_footnote(&mut w, footnote);
    }

    w.close();
    w.finish()
}

fn write_separator(w: &mut XmlWriter, id: i32, kind: &'static str) {
    w.open("w:footnote")
        .attr("w:type", kind)
        .attr("w:id", &id.to_string())
        .start_children();
    w.open(xml::W_P).start_children();
    w.open(xml::W_R).start_children();
    w.leaf(if kind == "separator" { "w:separator" } else { "w:continuationSeparator" });
    w.close();
    w.close();
    w.close();
}

fn write_footnote(w: &mut XmlWriter, footnote: &Footnote) {
    w.open("w:footnote").attr("w:id", &footnote.id.to_string()).start_children();
    let mut ended_para = false;
    for block in &footnote.blocks {
        ended_para = write_block(w, block);
    }
    if !ended_para {
        w.leaf(xml::W_P);
    }
    w.close();
}

// ---------------------------------------------------------------------------
// docProps
// ---------------------------------------------------------------------------

fn build_core(info: &DocumentInfo, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("cp:coreProperties")
        .attr("xmlns:cp", "http://schemas.openxmlformats.org/package/2006/metadata/core-properties")
        .attr("xmlns:dc", "http://purl.org/dc/elements/1.1/")
        .attr("xmlns:dcterms", "http://purl.org/dc/terms/")
        .attr("xmlns:dcmitype", "http://purl.org/dc/dcmitype/")
        .attr("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance")
        .start_children();

    if let Some(title) = &info.title {
        w.elem_text("dc:title", title);
    }
    if !info.author.is_empty() {
        w.elem_text("dc:creator", &info.author.join("; "));
    }
    if let Some(desc) = &info.description {
        w.elem_text("dc:description", desc);
    }
    if !info.keywords.is_empty() {
        w.elem_text("cp:keywords", &info.keywords.join(", "));
    }
    if let Smart::Custom(Some(date)) = &info.date
        && let Some(s) = w3cdtf(date) {
            w.open("dcterms:created")
                .attr("xsi:type", "dcterms:W3CDTF")
                .start_children();
            w.text(&s);
            w.close();
            w.open("dcterms:modified")
                .attr("xsi:type", "dcterms:W3CDTF")
                .start_children();
            w.text(&s);
            w.close();
        }

    w.close();
    w.finish()
}

fn build_app(pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("Properties")
        .attr("xmlns", "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties")
        .start_children();
    w.elem_text("Application", "Typst");
    w.close();
    w.finish()
}

/// Formats a `Datetime` as a W3CDTF timestamp, if it has at least a date.
fn w3cdtf(date: &typst_library::foundations::Datetime) -> Option<EcoString> {
    let year = date.year()?;
    let month = date.month().unwrap_or(1);
    let day = date.day().unwrap_or(1);
    let hour = date.hour().unwrap_or(0);
    let minute = date.minute().unwrap_or(0);
    let second = date.second().unwrap_or(0);
    Some(ecow::eco_format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

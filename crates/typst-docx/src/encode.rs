//! Serializes the typed DOCX IR into an OPC zip package.

use ecow::EcoString;
use typst_library::diag::{SourceResult, bail};
use typst_library::foundations::Smart;
use typst_library::model::DocumentInfo;
use typst_ooxml_core::{dml, ns};
use typst_syntax::Span;

use crate::dom::{
    Anchor, AnchorPos, AnchorWrap, Block, Border, Cell, CellBorders, DocxDocument,
    Drawing, Field, FieldDisplay, FieldMode, Footnote, GroupSpec, HdrFtrPart, Para,
    ParaChild, ParaProps, Row, Run, SectPr, SectType, ShapeFill, ShapeGeom, ShapeSpec,
    Spacing, Tbl, Toc, VAlign, VMerge,
};
use crate::package::{DOCX_PACKAGE_OPTIONS, Package, RelMode, Rels};
use crate::styles_part;
use crate::xml::{self, XmlWriter};

/// Settings for DOCX export.
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct DocxOptions {
    /// Whether to pretty-print the XML parts.
    pub pretty: bool,
}

// Relationship-type URIs.
const REL_OFFICE_DOCUMENT: &str = ns::rel::OFFICE_DOCUMENT;
const REL_CORE_PROPS: &str = ns::rel::CORE_PROPS;
const REL_EXTENDED_PROPS: &str = ns::rel::EXTENDED_PROPS;
const REL_CUSTOM_PROPERTIES: &str = ns::rel::CUSTOM_PROPERTIES;
const REL_STYLES: &str = ns::rel::STYLES;
const REL_NUMBERING: &str = ns::rel::NUMBERING;
const REL_FOOTNOTES: &str = ns::rel::FOOTNOTES;
const REL_ENDNOTES: &str = ns::rel::ENDNOTES;
const REL_SETTINGS: &str = ns::rel::SETTINGS;
const REL_THEME: &str = ns::rel::THEME;
const REL_FONT_TABLE: &str = ns::rel::FONT_TABLE;
const REL_WEB_SETTINGS: &str = ns::rel::WEB_SETTINGS;

// Content types.
const CT_DOCUMENT: &str = ns::ct::WORD_DOCUMENT;
const CT_STYLES: &str = ns::ct::WORD_STYLES;
const CT_NUMBERING: &str = ns::ct::WORD_NUMBERING;
const CT_FOOTNOTES: &str = ns::ct::WORD_FOOTNOTES;
const CT_ENDNOTES: &str = ns::ct::WORD_ENDNOTES;
const CT_SETTINGS: &str = ns::ct::WORD_SETTINGS;
const CT_CORE: &str = ns::ct::CORE_PROPS;
const CT_EXTENDED: &str = ns::ct::EXTENDED_PROPS;
const CT_CUSTOM_PROPERTIES: &str = ns::ct::CUSTOM_PROPERTIES;
const CT_HEADER: &str = ns::ct::WORD_HEADER;
const CT_FOOTER: &str = ns::ct::WORD_FOOTER;
const CT_THEME: &str = ns::ct::THEME;
const CT_FONT_TABLE: &str = ns::ct::WORD_FONT_TABLE;
const CT_WEB_SETTINGS: &str = ns::ct::WORD_WEB_SETTINGS;

fn push_font(fonts: &mut Vec<String>, font: &str) {
    if !fonts.iter().any(|existing| existing == font) {
        fonts.push(font.to_string());
    }
}

/// Serializes a DOCX document into the OPC zip bytes.
#[typst_macros::time(name = "docx encode")]
pub fn docx(document: &DocxDocument, options: &DocxOptions) -> SourceResult<Vec<u8>> {
    if let Err(err) = crate::invariants::validate(document) {
        bail!(Span::detached(), "invalid finalized DOCX IR: {err}");
    }
    let pretty = options.pretty;
    let mut package = Package::new(DOCX_PACKAGE_OPTIONS);
    let mut root_rels = Rels::new();

    // The document's own relationships (images/hyperlinks accumulated during
    // conversion, plus styles/numbering/footnotes/settings added here).
    let mut doc_rels = clone_rels(&document.doc_rels);

    // -- word/styles.xml --
    let styles_xml = styles_part::build(
        &document.info,
        &document.text_defaults,
        &document.heading_styles,
        document.max_heading_level,
        pretty,
    );
    package.add_xml("word/styles.xml", CT_STYLES, styles_xml);
    doc_rels.add(REL_STYLES, "styles.xml", RelMode::Internal);

    // -- word/settings.xml --
    let settings_xml = build_settings(document, pretty);
    package.add_xml("word/settings.xml", CT_SETTINGS, settings_xml);
    doc_rels.add(REL_SETTINGS, "settings.xml", RelMode::Internal);

    // -- word/theme/theme1.xml --
    package.add_xml(
        "word/theme/theme1.xml",
        CT_THEME,
        crate::parts::build_theme(&document.text_defaults, pretty),
    );
    doc_rels.add(REL_THEME, "theme/theme1.xml", RelMode::Internal);

    // -- word/fontTable.xml --
    // Every font referenced by the finalized IR + standard auxiliary fonts.
    let mut fonts: Vec<String> = Vec::new();
    for font in document.fidelity_report().fonts() {
        push_font(&mut fonts, &font.family);
    }
    for f in ["Symbol", "Courier New"] {
        push_font(&mut fonts, f);
    }
    if document.uses_math {
        push_font(&mut fonts, "Cambria Math");
    }
    package.add_xml(
        "word/fontTable.xml",
        CT_FONT_TABLE,
        crate::parts::build_font_table(&fonts, pretty),
    );
    doc_rels.add(REL_FONT_TABLE, "fontTable.xml", RelMode::Internal);

    // -- word/webSettings.xml --
    package.add_xml(
        "word/webSettings.xml",
        CT_WEB_SETTINGS,
        crate::parts::build_web_settings(pretty),
    );
    doc_rels.add(REL_WEB_SETTINGS, "webSettings.xml", RelMode::Internal);

    // -- word/numbering.xml (conditional) --
    if !document.numbering.abstracts.is_empty() {
        let numbering_xml = build_numbering(document, pretty);
        package.add_xml("word/numbering.xml", CT_NUMBERING, numbering_xml);
        doc_rels.add(REL_NUMBERING, "numbering.xml", RelMode::Internal);
    }

    // -- word/footnotes.xml + word/endnotes.xml --
    // Word writes both parts in every document — a stub with just the separator
    // definitions when there are no notes — so emit them unconditionally.
    let footnotes_xml = build_footnotes(document, pretty);
    package.add_xml("word/footnotes.xml", CT_FOOTNOTES, footnotes_xml);
    doc_rels.add(REL_FOOTNOTES, "footnotes.xml", RelMode::Internal);
    // A footnote body that holds an image / external link references it by r:id;
    // that id resolves against footnotes.xml's OWN rels part, not the document's.
    // Without this, Word refuses to open the file.
    write_part_rels(&mut package, "word/footnotes.xml", &document.footnote_rels)?;
    package.add_xml("word/endnotes.xml", CT_ENDNOTES, build_endnotes(pretty));
    doc_rels.add(REL_ENDNOTES, "endnotes.xml", RelMode::Internal);

    // -- header/footer parts --
    // The headerReference/footerReference relationships live in `doc_rels`; the
    // part's OWN relationships (images, external links in its content) live in a
    // sibling `word/_rels/<part>.rels` so their r:ids resolve correctly.
    // Each part gets a disjoint `w14:paraId` base (1 MiB of headroom per part)
    // so paragraph identities never collide across the package. The body uses
    // the `0x0000_0000` lane; headers `0x1nnn_….`, footers `0x4nnn_…`, notes
    // `0x7000_0000` (see `build_footnotes`).
    for (i, part) in document.header_parts.iter().enumerate() {
        let xml = build_hdrftr(part, 0x1000_0000 + i as u32 * 0x0010_0000, pretty);
        package.add_xml(&format!("word/{}", part.part_name), CT_HEADER, xml);
        write_part_rels(&mut package, &format!("word/{}", part.part_name), &part.rels)?;
    }
    for (i, part) in document.footer_parts.iter().enumerate() {
        let xml = build_hdrftr(part, 0x4000_0000 + i as u32 * 0x0010_0000, pretty);
        package.add_xml(&format!("word/{}", part.part_name), CT_FOOTER, xml);
        write_part_rels(&mut package, &format!("word/{}", part.part_name), &part.rels)?;
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

    // -- word/typstBibliography.xml (conditional) --
    // A lossless BibLaTeX sidecar for external tools (e.g. `pandoc
    // --citeproc`), under a private relationship type Word itself does not
    // recognize. Kept alongside the native `customXml/item1.xml` part below:
    // that part is lossy (Word's fixed field set can't hold everything
    // Hayagriva has), so this sidecar remains the full-fidelity channel.
    if let Some(bib) = &document.bibliography {
        package.add_xml(
            "word/typstBibliography.xml",
            "application/xml",
            build_typst_bibliography(bib, pretty),
        );
        doc_rels.add(
            "https://typst.app/schema/2026/relationships/bibliography",
            "typstBibliography.xml",
            RelMode::Internal,
        );
    }

    // -- customXml/item1.xml + itemProps1.xml (conditional) --
    // Word's own native bibliography schema (`b:Sources`/`b:Source`), the
    // part that backs References → Manage Sources. The visible body keeps the
    // realized, formatted citation text — Word's own CITATION/BIBLIOGRAPHY
    // field model is proprietary and would let Word's independent
    // citation-formatting engine silently reformat it on auto-update — so
    // this part is metadata only, not live fields. Word always accompanies
    // `item1.xml` with a schema-association `itemProps1.xml` (see
    // `crate::bibliography`), so both are emitted together.
    if !document.word_sources.is_empty() {
        package.add_xml(
            "customXml/item1.xml",
            "application/xml",
            build_word_sources(&document.word_sources, pretty),
        );
        let guid = crate::bibliography::package_guid(&document.word_sources);
        package.add_xml(
            "customXml/itemProps1.xml",
            ns::ct::CUSTOM_XML_PROPS,
            build_item_props(&guid, pretty),
        );
        let mut item_rels = Rels::new();
        item_rels.add(ns::rel::CUSTOM_XML_PROPS, "itemProps1.xml", RelMode::Internal);
        write_part_rels(&mut package, "customXml/item1.xml", &item_rels)?;
        doc_rels.add(ns::rel::CUSTOM_XML, "../customXml/item1.xml", RelMode::Internal);
    }

    // -- customXml/typstFidelity.xml ---------------------------------------
    // Versioned, machine-readable export evidence. The canonical customXml
    // part is ideal for tooling, but Writer drops arbitrary customXml on save;
    // docProps/custom.xml below redundantly carries the exact payload through
    // that round trip.
    let fidelity_manifest = document.fidelity_manifest_xml();
    package.add_xml(
        crate::manifest::PART_NAME,
        "application/xml",
        fidelity_manifest.clone(),
    );
    doc_rels.add(
        crate::manifest::REL_TYPE,
        "../customXml/typstFidelity.xml",
        RelMode::Internal,
    );

    // -- word/document.xml --
    let document_xml = build_document(document, pretty);
    package.add_xml("word/document.xml", CT_DOCUMENT, document_xml);

    // -- word/_rels/document.xml.rels --
    write_part_rels(&mut package, "word/document.xml", &doc_rels)?;

    // -- docProps/core.xml + app.xml --
    package.add_xml("docProps/core.xml", CT_CORE, build_core(&document.info, pretty));
    package.add_xml("docProps/app.xml", CT_EXTENDED, build_app(pretty));
    package.add_xml(
        "docProps/custom.xml",
        CT_CUSTOM_PROPERTIES,
        build_custom_properties(&fidelity_manifest, pretty),
    );

    // -- package root relationships --
    root_rels.add(REL_OFFICE_DOCUMENT, "word/document.xml", RelMode::Internal);
    root_rels.add(REL_CORE_PROPS, "docProps/core.xml", RelMode::Internal);
    root_rels.add(REL_EXTENDED_PROPS, "docProps/app.xml", RelMode::Internal);
    root_rels.add(REL_CUSTOM_PROPERTIES, "docProps/custom.xml", RelMode::Internal);

    if let Err(err) = crate::schema::validate_package(&package) {
        bail!(Span::detached(), "invalid finalized DOCX XML sequence: {err}");
    }

    match package.finish(&root_rels) {
        Ok(bytes) => Ok(bytes),
        Err(err) => bail!(Span::detached(), "failed to finalize DOCX package: {err}"),
    }
}

/// Clones the conversion-time `doc_rels` so `encode` can append the static
/// document parts (styles/numbering/footnotes/settings) without mutating the
/// document's own relationship table.
fn clone_rels(src: &Rels) -> Rels {
    src.clone()
}

/// Picks the content type for a media extension.
fn media_content_type(ext: &str) -> &'static str {
    typst_ooxml_core::media::image_content_type(ext)
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
    w.attr("xmlns:w", ns::W)
        .attr("xmlns:r", ns::R)
        .attr("xmlns:m", ns::M)
        .attr("xmlns:wp", ns::WP)
        .attr("xmlns:a", ns::A)
        .attr("xmlns:pic", ns::PIC)
        .attr("xmlns:mc", ns::MC)
        // `mc:Ignorable="w14 wp14"` (below) names these prefixes, so they MUST be
        // declared or the Markup-Compatibility markup is invalid: Word then
        // refuses to open the file ("unreadable content", offers to repair) on
        // EVERY document. LibreOffice silently tolerates the dangling prefixes,
        // which is why this hid until tested in real Word.
        .attr("xmlns:w14", ns::W14)
        .attr("xmlns:wp14", ns::WP14)
        // `wps` is named by `mc:Choice Requires="wps"` around a text box, so the
        // prefix must be in scope at the root; `v` is the legacy VML used in the
        // matching `mc:Fallback`.
        .attr("xmlns:wps", ns::WPS)
        .attr("xmlns:v", ns::V);
}

fn build_document(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open(xml::W_DOCUMENT);
    decl_ooxml_namespaces(&mut w);
    w.attr("mc:Ignorable", "w14 wp14").start_children();

    // A flat page-colour (`set page(fill:)`) is a document-level element — one
    // per package, a sibling of `w:body` — mirroring Word's own "Page Color".
    if let Some(c) = document.background_color {
        w.open("w:background").attr("w:color", &crate::props::hex(c)).empty();
    }

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
        Block::FlowSpace { dxa } => {
            write_para(
                w,
                &Para {
                    props: ParaProps {
                        spacing: Some(Spacing {
                            before: Some(0),
                            after: Some(0),
                            line: Some((*dxa).max(1)),
                            line_rule_auto: false,
                            line_rule_at_least: false,
                        }),
                        ..ParaProps::default()
                    },
                    content: Vec::new(),
                },
            );
            true
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
            open_para(w);
            w.open(xml::W_PPR).start_children();
            write_sectpr(w, sect);
            w.close(); // pPr
            w.close(); // p
            true
        }
        Block::Tag(_) => false,
    }
}

/// Opens a `<w:p>` carrying a fresh `w14:paraId`/`w14:textId` — the stable
/// paragraph identity Word stamps on every content paragraph (it anchors
/// comments, tracked-changes and co-authoring) — and leaves it open for
/// children. The `w14` attributes are valid on `w:p` because every part root
/// lists `w14` in its `mc:Ignorable`.
fn open_para(w: &mut XmlWriter) {
    let pid = w.next_para_id();
    w.open(xml::W_P)
        .attr("w14:paraId", &pid)
        .attr("w14:textId", &pid)
        .start_children();
}

fn write_para(w: &mut XmlWriter, para: &Para) {
    open_para(w);
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
        w.open("w:gridSpan")
            .attr("w:val", &cell.grid_span.to_string())
            .empty();
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
    // Always emit all four sides: after Typst resolves the grid, a `None` side
    // means "no stroke" (not "inherit"), so it must be written as an explicit
    // `w:val="nil"` to turn OFF the table's default border. Returning early for an
    // all-`None` (e.g. `stroke: none`) cell let the blanket `w:tblBorders` show
    // through — the exact opposite of what was asked. (Cells that DO have strokes
    // are unchanged: they already emitted a full `w:tcBorders`.)
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
    // A text box (`wps:txbx`) is a 2010 DrawingML feature. Wrap it in
    // `mc:AlternateContent`: the modern `wps` drawing in `mc:Choice Requires="wps"`,
    // and a legacy VML `v:textbox` in `mc:Fallback` so a consumer that does not
    // support `wps` (Word 2007, some others) still renders the framed text instead
    // of dropping it. Plain pictures and vector shapes need no fallback.
    if let Some(shape) = &d.shape
        && let Some(tb) = &shape.txbx
    {
        w.open(xml::W_R).start_children();
        w.open("mc:AlternateContent").start_children();
        w.open("mc:Choice").attr("Requires", "wps").start_children();
        w.open("w:drawing").start_children();
        match &d.anchor {
            None => write_inline_envelope(w, d),
            Some(a) => write_anchor_envelope(w, d, a),
        }
        w.close(); // w:drawing
        w.close(); // mc:Choice
        w.open("mc:Fallback").start_children();
        write_vml_textbox(w, d, shape, tb);
        w.close(); // mc:Fallback
        w.close(); // mc:AlternateContent
        w.close(); // w:r
        return;
    }

    w.open(xml::W_R).start_children();
    w.open("w:drawing").start_children();
    match &d.anchor {
        None => write_inline_envelope(w, d),
        Some(a) => write_anchor_envelope(w, d, a),
    }
    w.close(); // w:drawing
    w.close(); // w:r
}

/// Emits the legacy VML fallback (`<w:pict><v:rect><v:textbox>…`) for a text box,
/// carrying the same fill/stroke/inset and the same editable paragraphs as the
/// modern `wps` form, sized in points (VML's unit).
fn write_vml_textbox(
    w: &mut XmlWriter,
    d: &Drawing,
    shape: &ShapeSpec,
    tb: &crate::dom::TextBox,
) {
    let pt = |emu: i64| format!("{:.2}", emu as f64 / 12700.0);
    let hex = |c: [u8; 4]| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);

    w.open("w:pict").start_children();
    w.open("v:rect")
        .attr("style", &format!("width:{}pt;height:{}pt", pt(d.w_emu), pt(d.h_emu)));
    // VML (the legacy fallback for pre-2007 consumers) has no gradient form
    // worth the complexity here; approximate with the gradient's first stop —
    // the modern `wps` Choice (the one virtually every consumer picks) has the
    // real gradient.
    match &shape.fill {
        Some(ShapeFill::Solid(c)) => {
            w.attr("fillcolor", &hex(*c));
        }
        Some(ShapeFill::LinearGradient { stops, .. })
        | Some(ShapeFill::RadialGradient { stops, .. }) => {
            if let Some(stop) = stops.first() {
                w.attr("fillcolor", &hex(stop.color));
            }
        }
        Some(ShapeFill::Tile { .. }) => {}
        None => {
            w.attr("filled", "f");
        }
    }
    match &shape.stroke {
        Some(s) => {
            w.attr("strokecolor", &hex(s.color))
                .attr("strokeweight", &format!("{}pt", pt(s.w_emu)));
        }
        None => {
            w.attr("stroked", "f");
        }
    }
    w.start_children();
    let inset = format!(
        "{}pt,{}pt,{}pt,{}pt",
        pt(tb.ins[0]),
        pt(tb.ins[1]),
        pt(tb.ins[2]),
        pt(tb.ins[3])
    );
    w.open("v:textbox").attr("inset", &inset).start_children();
    w.open("w:txbxContent").start_children();
    let mut ends_with_para = false;
    for block in &tb.blocks {
        ends_with_para = write_block(w, block);
    }
    if !ends_with_para {
        w.leaf(xml::W_P);
    }
    w.close(); // w:txbxContent
    w.close(); // v:textbox
    w.close(); // v:rect
    w.close(); // w:pict
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
    write_drawing_doc_properties(w, d);
    w.open("wp:cNvGraphicFramePr").start_children();
    w.open("a:graphicFrameLocks")
        .attr("xmlns:a", ns::A)
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
        .attr("behindDoc", if a.behind { "1" } else { "0" })
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
    write_drawing_doc_properties(w, d);
    w.open("wp:cNvGraphicFramePr").start_children();
    w.open("a:graphicFrameLocks")
        .attr("xmlns:a", ns::A)
        .attr("noChangeAspect", "1")
        .empty();
    w.close(); // wp:cNvGraphicFramePr
    write_pic_payload(w, d);
    w.close(); // wp:anchor
}

/// Emits the drawing's document-level non-visual properties and accessibility
/// intent. Office's decorative flag is an extension child of `wp:docPr`, not an
/// attribute on the picture payload.
fn write_drawing_doc_properties(w: &mut XmlWriter, d: &Drawing) {
    w.open("wp:docPr")
        .attr("id", &d.docpr_id.to_string())
        .attr("name", &d.name);
    if let Some(alt) = &d.alt {
        w.attr("descr", alt);
    }
    if !d.decorative {
        w.empty();
        return;
    }
    w.start_children();
    w.open("a:extLst").attr("xmlns:a", ns::A).start_children();
    w.open("a:ext")
        .attr("uri", "{C183D7F6-B498-43B3-948B-1728B52AA6E4}")
        .start_children();
    w.open("adec:decorative")
        .attr("xmlns:adec", ns::ADEC)
        .attr("val", "1")
        .empty();
    w.close(); // a:ext
    w.close(); // a:extLst
    w.close(); // wp:docPr
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
    if let Some(group) = &d.group {
        write_group_payload(w, d, group);
        return;
    }
    if let Some(shape) = &d.shape {
        write_shape_payload(w, d, shape);
        return;
    }
    w.open("a:graphic").attr("xmlns:a", ns::A).start_children();
    w.open("a:graphicData").attr("uri", ns::PIC).start_children();
    w.open("pic:pic").attr("xmlns:pic", ns::PIC).start_children();
    // pic:nvPicPr
    w.open("pic:nvPicPr").start_children();
    w.open("pic:cNvPr")
        .attr("id", &d.docpr_id.to_string())
        .attr("name", &d.name);
    if let Some(alt) = &d.alt {
        w.attr("descr", alt);
    }
    w.empty();
    w.open("pic:cNvPicPr").empty();
    w.close(); // pic:nvPicPr
    // pic:blipFill
    w.open("pic:blipFill").start_children();
    dml::write_blip(w, &d.rel, d.svg_rel.as_deref());
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

/// The `WordprocessingShape` namespace URI, shared by every `wps:*` element
/// (a lone shape's payload, and each child of a group).
const WPS_NS: &str = ns::WPS;

/// Emits a vector DrawingML shape (`wps:wsp`) payload in place of `pic:pic`.
fn write_shape_payload(w: &mut XmlWriter, d: &Drawing, shape: &ShapeSpec) {
    const A: &str = ns::A;
    w.open("a:graphic").attr("xmlns:a", A).start_children();
    w.open("a:graphicData").attr("uri", WPS_NS).start_children();
    write_wsp(w, 0, 0, d.w_emu, d.h_emu, shape);
    w.close(); // a:graphicData
    w.close(); // a:graphic
}

/// Emits a `wpg:wgp` (`WordprocessingGroup`) payload: several native shapes
/// sharing one coordinate space — e.g. a `#move`d composition of several
/// shapes/lines/curves — as one editable, grouped drawing instead of a single
/// rasterized image.
fn write_group_payload(w: &mut XmlWriter, d: &Drawing, group: &GroupSpec) {
    const A: &str = ns::A;
    const WPG: &str = ns::WPG;
    let (cx, cy) = (d.w_emu.to_string(), d.h_emu.to_string());

    w.open("a:graphic").attr("xmlns:a", A).start_children();
    w.open("a:graphicData").attr("uri", WPG).start_children();
    w.open("wpg:wgp")
        .attr("xmlns:wpg", WPG)
        .attr("xmlns:wps", WPS_NS)
        .start_children();
    w.open("wpg:cNvGrpSpPr").empty();
    w.open("wpg:grpSpPr").start_children();
    w.open("a:xfrm").start_children();
    w.open("a:off").attr("x", "0").attr("y", "0").empty();
    w.open("a:ext").attr("cx", &cx).attr("cy", &cy).empty();
    // The child coordinate space: children's own `a:xfrm` offsets/extents are
    // expressed directly in this space, which we set 1:1 with the group's own
    // extent (`chOff` = 0, `chExt` = the same `cx`/`cy`), so no extra scaling
    // is needed between a child's local EMU coordinates and the group's.
    w.open("a:chOff").attr("x", "0").attr("y", "0").empty();
    w.open("a:chExt").attr("cx", &cx).attr("cy", &cy).empty();
    w.close(); // a:xfrm
    w.close(); // wpg:grpSpPr

    for child in &group.children {
        write_wsp(w, child.x_emu, child.y_emu, child.w_emu, child.h_emu, &child.shape);
    }

    w.close(); // wpg:wgp
    w.close(); // a:graphicData
    w.close(); // a:graphic
}

/// Emits one `wps:wsp` shape — the geometry/fill/stroke/text-box body shared
/// by a lone shape drawing ([`write_shape_payload`]) and each child of a group
/// ([`write_group_payload`]) — positioned at `(off_x, off_y)` within whatever
/// coordinate space the caller established (the drawing's own top-left corner
/// for a lone shape; the group's local child space for a group member).
fn write_wsp(
    w: &mut XmlWriter,
    off_x: i64,
    off_y: i64,
    w_emu: i64,
    h_emu: i64,
    shape: &ShapeSpec,
) {
    w.open("wps:wsp").attr("xmlns:wps", WPS_NS).start_children();
    w.open("wps:cNvSpPr").empty();
    w.open("wps:spPr").start_children();

    w.open("a:xfrm").start_children();
    w.open("a:off")
        .attr("x", &off_x.to_string())
        .attr("y", &off_y.to_string())
        .empty();
    w.open("a:ext")
        .attr("cx", &w_emu.to_string())
        .attr("cy", &h_emu.to_string())
        .empty();
    w.close(); // a:xfrm

    match &shape.geom {
        ShapeGeom::Rect | ShapeGeom::RoundRect | ShapeGeom::Ellipse => {
            let prst = match shape.geom {
                ShapeGeom::RoundRect => "roundRect",
                ShapeGeom::Ellipse => "ellipse",
                _ => "rect",
            };
            dml::write_prst_geom(w, prst);
        }
        ShapeGeom::Path(segments) => {
            dml::write_custom_geom(w, segments, w_emu, h_emu);
        }
    }

    dml::write_fill(w, shape.fill.as_ref(), "1");
    dml::write_stroke(w, shape.stroke.as_ref(), false);

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
            let wrap = match tb.wrap {
                crate::dom::TextBoxWrap::Square => "square",
                crate::dom::TextBoxWrap::None => "none",
            };
            w.open("wps:bodyPr")
                .attr("wrap", wrap)
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
        Run::ColumnBreak => {
            w.open(xml::W_R).start_children();
            w.open(xml::W_BR).attr("w:type", "column").empty();
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
    write_field_begin(w, &field.instr, field.mode, field.display);
    // cached result
    if field.display == FieldDisplay::Hidden {
        debug_assert!(field.result.is_empty(), "hidden fields have no visible cache");
        write_hidden_field_run(w, None);
    } else {
        for run in &field.result {
            write_run(w, run);
        }
    }
    write_field_end(w, field.display);
}

fn write_field_run_start(w: &mut XmlWriter, display: FieldDisplay) {
    w.open(xml::W_R).start_children();
    if display == FieldDisplay::Hidden {
        w.open(xml::W_RPR).start_children();
        w.leaf("w:vanish");
        w.close();
    }
}

fn write_hidden_field_run(w: &mut XmlWriter, instr: Option<&str>) {
    write_field_run_start(w, FieldDisplay::Hidden);
    if let Some(instr) = instr {
        w.open("w:instrText").attr("xml:space", "preserve").start_children();
        w.text(instr);
        w.close();
    } else {
        // Give consumers a result run whose character formatting they can
        // retain when recalculating the field. A zero-width space avoids an
        // empty run being discarded during import before recalculation.
        w.open(xml::W_T).attr("xml:space", "preserve").start_children();
        w.text("\u{200b}");
        w.close();
    }
    w.close();
}

fn write_field_begin(
    w: &mut XmlWriter,
    instr: &str,
    mode: FieldMode,
    display: FieldDisplay,
) {
    write_field_run_start(w, display);
    let fld = w.open("w:fldChar").attr("w:fldCharType", "begin");
    if mode.locked() {
        fld.attr("w:fldLock", "true");
    }
    w.empty();
    w.close();
    if display == FieldDisplay::Hidden {
        write_hidden_field_run(w, Some(instr));
    } else {
        w.open(xml::W_R).start_children();
        w.open("w:instrText").attr("xml:space", "preserve").start_children();
        w.text(instr);
        w.close();
        w.close();
    }
    write_field_run_start(w, display);
    w.open("w:fldChar").attr("w:fldCharType", "separate").empty();
    w.close();
}

fn write_field_end(w: &mut XmlWriter, display: FieldDisplay) {
    write_field_run_start(w, display);
    w.open("w:fldChar").attr("w:fldCharType", "end").empty();
    w.close();
}

/// Serializes a table of contents. A heading TOC is wrapped in a Word "Table of
/// Contents" content control (`w:sdt`/`docPartObj`) — the idiomatic form that
/// gives it the gallery identity and the "Update Table" affordance; a list of
/// figures/tables stays a bare field (Word does not wrap those). The field
/// itself spans the baked entry paragraphs (or a single placeholder paragraph
/// when empty), so the entries show without a manual update.
fn write_toc(w: &mut XmlWriter, toc: &Toc) {
    let as_content_control = toc.depth.is_some();
    if as_content_control {
        w.open("w:sdt").start_children();
        w.open("w:sdtPr").start_children();
        w.open("w:docPartObj").start_children();
        w.open("w:docPartGallery")
            .attr(xml::W_VAL, "Table of Contents")
            .empty();
        w.leaf("w:docPartUnique");
        w.close(); // w:docPartObj
        w.close(); // w:sdtPr
        w.open("w:sdtContent").start_children();
    }

    write_toc_body(w, toc);

    if as_content_control {
        w.close(); // w:sdtContent
        w.close(); // w:sdt
    }
}

/// Emits the TOC field's paragraphs (the field code + baked entries, or a
/// single-paragraph placeholder when there are none).
fn write_toc_body(w: &mut XmlWriter, toc: &Toc) {
    // Emits the field `begin` + instruction + `separate` run sequence.
    let write_begin = |w: &mut XmlWriter| {
        write_field_begin(w, &toc.instr, toc.mode, FieldDisplay::Visible);
    };

    if toc.entries.is_empty() {
        w.open(xml::W_P).start_children();
        write_begin(w);
        for run in &toc.fallback {
            write_run(w, run);
        }
        write_field_end(w, FieldDisplay::Visible);
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
            write_field_end(w, FieldDisplay::Visible);
        }
        w.close(); // w:p
    }
}

fn write_sectpr(w: &mut XmlWriter, sect: &SectPr) {
    w.open(xml::W_SECTPR).start_children();

    // CT_SectPr child order is strict: headerReference/footerReference precede
    // everything, then (type) → pgSz → pgMar → lnNumType → pgNumType → cols →
    // titlePg.
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

    let pg = w
        .open("w:pgSz")
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
    if let Some(line_numbers) = &sect.line_numbers {
        w.open("w:lnNumType")
            .attr("w:countBy", &line_numbers.count_by.to_string())
            .attr("w:start", &line_numbers.start.to_string())
            .attr("w:restart", line_numbers.restart)
            .attr("w:distance", &line_numbers.distance.to_string())
            .empty();
    }
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
    // The line grid Word always writes in a section (default line pitch).
    w.open("w:docGrid").attr("w:linePitch", "360").empty();
    w.close();
}

/// Writes a part's own relationships next to it as `<dir>/_rels/<file>.rels`,
/// but only when the part actually has relationships (images, external links
/// in its content). OPC associates a `.rels` part with its part by this
/// naming convention, so no explicit reference is needed. An `r:id` in the
/// part's own content resolves against THIS rels part, not
/// `word/_rels/document.xml.rels`.
fn write_part_rels(
    package: &mut Package,
    part_path: &str,
    rels: &Rels,
) -> SourceResult<()> {
    match package.add_relationships(part_path, rels) {
        Ok(()) => Ok(()),
        Err(err) => bail!(Span::detached(), "failed to register relationships: {err}"),
    }
}

/// Serializes a header (`w:hdr`) or footer (`w:ftr`) part. Never emits an empty
/// root: a trailing empty `<w:p/>` is appended if the content doesn't end in a
/// paragraph (a bare `w:hdr`/`w:ftr` is non-conformant in some Word builds).
fn build_hdrftr(part: &HdrFtrPart, base: u32, pretty: bool) -> String {
    let root = if part.is_header { "w:hdr" } else { "w:ftr" };
    let mut w = XmlWriter::new(pretty);
    w.set_para_base(base);
    w.open(root);
    decl_ooxml_namespaces(&mut w);
    // `w14` (the `w14:paraId`/`textId` on each paragraph) must be MCE-ignorable
    // on this part's own root, exactly as Word writes it.
    w.attr("mc:Ignorable", "w14 wp14");
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
        .attr("xmlns:w", ns::W)
        .attr("xmlns:m", ns::M)
        .attr("xmlns:o", ns::O)
        .attr("xmlns:v", ns::V)
        .attr("xmlns:w14", ns::W14)
        .attr("xmlns:mc", ns::MC)
        .attr("mc:Ignorable", "w14")
        .start_children();
    // The settings Word itself writes, in canonical schema order.
    w.open("w:zoom").attr("w:percent", "100").empty();
    w.open("w:proofState")
        .attr("w:spelling", "clean")
        .attr("w:grammar", "clean")
        .empty();
    if document.mirror_margins {
        w.leaf("w:mirrorMargins");
    }
    if document.rtl_gutter {
        w.leaf("w:rtlGutter");
    }
    w.open("w:defaultTabStop").attr(xml::W_VAL, "720").empty();
    // A bare element (no `w:val`) — Word's own CT_OnOff-by-presence convention —
    // between defaultTabStop and characterSpacingControl, the schema position
    // Word itself uses.
    if document.hyphenate {
        w.leaf("w:autoHyphenation");
    }
    if document.even_and_odd_headers {
        w.leaf("w:evenAndOddHeaders");
    }
    w.open("w:characterSpacingControl")
        .attr(xml::W_VAL, "doNotCompress")
        .empty();
    // Header drawing-canvas defaults (the header sibling of shapeDefaults).
    w.raw(
        "<w:hdrShapeDefaults><o:shapedefaults v:ext=\"edit\" spidmax=\"1026\"/>\
           </w:hdrShapeDefaults>",
    );
    // Footnote/endnote separator references (Word writes both in every document,
    // pointing at the separator definitions in footnotes.xml / endnotes.xml).
    w.open("w:footnotePr").start_children();
    w.open("w:footnote").attr("w:id", "-1").empty();
    w.open("w:footnote").attr("w:id", "0").empty();
    w.close();
    w.open("w:endnotePr").start_children();
    w.open("w:endnote").attr("w:id", "-1").empty();
    w.open("w:endnote").attr("w:id", "0").empty();
    w.close();
    // Mark the document with the modern (Word 2013+) feature set. Without a
    // `<w:compat>` block Word assumes legacy behaviour and opens the file in
    // "Compatibility Mode" (a banner in the title bar, and the older layout
    // engine); declaring `compatibilityMode = 15` opens it as a native document.
    // The other settings are the ones Word writes alongside it.
    w.open("w:compat").start_children();
    for (name, val) in [
        ("compatibilityMode", "15"),
        ("overrideTableStyleFontSizeAndJustification", "1"),
        ("enableOpenTypeFeatures", "1"),
        ("doNotFlipMirrorIndents", "1"),
        ("differentiateMultirowTableHeaders", "1"),
    ] {
        w.open("w:compatSetting")
            .attr("w:name", name)
            .attr("w:uri", "http://schemas.microsoft.com/office/word")
            .attr("w:val", val)
            .empty();
    }
    w.close(); // compat
    // A revision-save-id block. Word stamps these to track editing sessions;
    // every real document carries one, so emit a deterministic root id.
    let fp = format!("{:08X}", doc_fingerprint(document));
    w.open("w:rsids").start_children();
    w.open("w:rsidRoot").attr(xml::W_VAL, &fp).empty();
    w.open("w:rsid").attr(xml::W_VAL, &fp).empty();
    w.close(); // rsids
    // Office Math defaults (Cambria Math, the standard break/justification rules)
    // so OMML equations render exactly as in Word's equation editor. Word writes
    // these even in a document with no equations.
    w.open("m:mathPr").start_children();
    w.open("m:mathFont").attr("m:val", "Cambria Math").empty();
    w.open("m:brkBin").attr("m:val", "before").empty();
    w.open("m:brkBinSub").attr("m:val", "--").empty();
    w.open("m:smallFrac").attr("m:val", "0").empty();
    w.leaf("m:dispDef");
    w.open("m:lMargin").attr("m:val", "0").empty();
    w.open("m:rMargin").attr("m:val", "0").empty();
    w.open("m:defJc").attr("m:val", "centerGroup").empty();
    w.open("m:wrapIndent").attr("m:val", "1440").empty();
    w.open("m:intLim").attr("m:val", "subSup").empty();
    w.open("m:naryLim").attr("m:val", "undOvr").empty();
    w.close(); // mathPr
    // Spell-check language for the theme fonts.
    let lang = document.text_defaults.lang.as_deref().unwrap_or("en-US");
    crate::props::write_language(&mut w, "w:themeFontLang", lang);
    // Map the colour-scheme slots to the theme (what Word writes for a doc using
    // the Office theme).
    w.open("w:clrSchemeMapping")
        .attr("w:bg1", "light1")
        .attr("w:t1", "dark1")
        .attr("w:bg2", "light2")
        .attr("w:t2", "dark2")
        .attr("w:accent1", "accent1")
        .attr("w:accent2", "accent2")
        .attr("w:accent3", "accent3")
        .attr("w:accent4", "accent4")
        .attr("w:accent5", "accent5")
        .attr("w:accent6", "accent6")
        .attr("w:hyperlink", "hyperlink")
        .attr("w:followedHyperlink", "followedHyperlink")
        .empty();
    // VML shape defaults (what Word writes for drawing-canvas bookkeeping).
    w.raw(
        "<w:shapeDefaults><o:shapedefaults v:ext=\"edit\" spidmax=\"1026\"/>\
           <o:shapelayout v:ext=\"edit\"><o:idmap v:ext=\"edit\" data=\"1\"/>\
           </o:shapelayout></w:shapeDefaults>",
    );
    w.open("w:decimalSymbol").attr(xml::W_VAL, ".").empty();
    w.open("w:listSeparator").attr(xml::W_VAL, ",").empty();
    // The per-document id Word stamps (in the w14 extension namespace, declared +
    // marked ignorable on the root). Deterministic from the document content.
    w.open("w14:docId").attr("w14:val", &fp).empty();
    w.close();
    w.finish()
}

/// A deterministic 32-bit fingerprint of the document (FNV-1a over the title,
/// block count and heading depth) used for the rsid / docId values. It only needs
/// to be stable and reasonably document-specific; Word regenerates it on save.
fn doc_fingerprint(document: &DocxDocument) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
    };
    if let Some(title) = &document.info.title {
        feed(title.as_bytes());
    }
    feed(&(document.body.len() as u32).to_le_bytes());
    feed(&[document.max_heading_level]);
    h
}

// ---------------------------------------------------------------------------
// word/numbering.xml
// ---------------------------------------------------------------------------

fn build_numbering(document: &DocxDocument, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("w:numbering").attr("xmlns:w", ns::W).start_children();

    for abs in &document.numbering.abstracts {
        w.open("w:abstractNum")
            .attr("w:abstractNumId", &abs.id.to_string())
            .start_children();
        w.open("w:multiLevelType")
            .attr(xml::W_VAL, abs.multilevel.as_str())
            .empty();
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
        w.open("w:num")
            .attr("w:numId", &num.num_id.to_string())
            .start_children();
        w.open("w:abstractNumId")
            .attr(xml::W_VAL, &num.abstract_id.to_string())
            .empty();
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
    // The notes lane for `w14:paraId` (disjoint from body/header/footer ranges).
    w.set_para_base(0x7000_0000);
    w.open("w:footnotes");
    decl_ooxml_namespaces(&mut w);
    w.attr("mc:Ignorable", "w14 wp14");
    w.start_children();

    // Separators.
    write_separator(&mut w, "w:footnote", -1, "separator");
    write_separator(&mut w, "w:footnote", 0, "continuationSeparator");

    for footnote in &document.footnotes {
        write_footnote(&mut w, footnote);
    }

    w.close();
    w.finish()
}

/// Builds `word/endnotes.xml`. Typst has no endnotes, so this is always the stub
/// (just the separator definitions) that Word writes in every document.
fn build_endnotes(pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("w:endnotes");
    decl_ooxml_namespaces(&mut w);
    w.attr("mc:Ignorable", "w14 wp14");
    w.start_children();
    write_separator(&mut w, "w:endnote", -1, "separator");
    write_separator(&mut w, "w:endnote", 0, "continuationSeparator");
    w.close();
    w.finish()
}

fn write_separator(w: &mut XmlWriter, elem: &'static str, id: i32, kind: &'static str) {
    w.open(elem)
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
    w.open("w:footnote")
        .attr("w:id", &footnote.id.to_string())
        .start_children();
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
// word/typstBibliography.xml
// ---------------------------------------------------------------------------

/// A private, inert sidecar part carrying the document's bibliography as a
/// BibLaTeX string, under a namespace Word does not recognize. See the call
/// site in `docx` for why this exists instead of native Word citation fields.
fn build_typst_bibliography(bib: &str, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("typstBibliography")
        .attr("xmlns", "https://typst.app/schema/2026/docx-bibliography")
        .start_children();
    w.elem_text("biblatex", bib);
    w.close();
    w.finish()
}

// ---------------------------------------------------------------------------
// customXml/item1.xml + itemProps1.xml
// ---------------------------------------------------------------------------

/// Word's native bibliography schema: a `b:Sources` root holding one
/// `b:Source` per entry. `SelectedStyle`/`StyleName` mirror what Word itself
/// always emits (a citation style for Source Manager's own UI, independent
/// of Typst's realized in-body citation formatting).
fn build_word_sources(
    sources: &[crate::bibliography::WordSource],
    pretty: bool,
) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("b:Sources")
        .attr("xmlns:b", ns::B)
        .attr("xmlns", ns::B)
        .attr("SelectedStyle", "\\APA.XSL")
        .attr("StyleName", "APA")
        .start_children();
    for source in sources {
        build_source(&mut w, source);
    }
    w.close();
    w.finish()
}

fn build_source(w: &mut XmlWriter, source: &crate::bibliography::WordSource) {
    w.open("b:Source").start_children();
    w.elem_text("b:Tag", &source.tag);
    w.elem_text("b:SourceType", source.source_type);
    w.elem_text("b:Guid", &source.guid);
    build_author(w, &source.author);
    if let Some(title) = &source.title {
        w.elem_text("b:Title", title);
    }
    if let Some(year) = &source.year {
        w.elem_text("b:Year", year);
    }
    if let Some(publisher) = &source.publisher {
        w.elem_text("b:Publisher", publisher);
    }
    if let Some(city) = &source.city {
        w.elem_text("b:City", city);
    }
    if let Some(journal) = &source.journal_name {
        w.elem_text("b:JournalName", journal);
    }
    if let Some(volume) = &source.volume {
        w.elem_text("b:Volume", volume);
    }
    if let Some(issue) = &source.issue {
        w.elem_text("b:Issue", issue);
    }
    if let Some(pages) = &source.pages {
        w.elem_text("b:Pages", pages);
    }
    if let Some(url) = &source.url {
        w.elem_text("b:URL", url);
    }
    w.close();
}

/// Word nests authors as `b:Author/b:Author/b:NameList/b:Person` (persons) or
/// `b:Author/b:Author/b:Corporate` (an organizational name) — the doubled
/// `b:Author` wrapper is how Word's own schema/UI distinguishes "the author
/// field" from "one author entry", confirmed against real Word-authored
/// sample documents.
fn build_author(w: &mut XmlWriter, author: &crate::bibliography::WordAuthor) {
    use crate::bibliography::WordAuthor;
    match author {
        WordAuthor::None => {}
        WordAuthor::Corporate(name) => {
            w.open("b:Author").start_children();
            w.open("b:Author").start_children();
            w.elem_text("b:Corporate", name);
            w.close();
            w.close();
        }
        WordAuthor::Persons(persons) => {
            w.open("b:Author").start_children();
            w.open("b:Author").start_children();
            w.open("b:NameList").start_children();
            for person in persons {
                w.open("b:Person").start_children();
                w.elem_text("b:Last", &person.last);
                if let Some(first) = &person.first {
                    w.elem_text("b:First", first);
                }
                w.close();
            }
            w.close();
            w.close();
            w.close();
        }
    }
}

/// The custom-XML "data store properties" part that associates `item1.xml`
/// with the bibliography schema, so Word's Source Manager recognizes it as a
/// bibliography rather than arbitrary custom XML. Every Word-authored
/// bibliography-bearing docx carries this part alongside `item1.xml`.
fn build_item_props(guid: &str, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("ds:datastoreItem")
        .attr("ds:itemID", guid)
        .attr("xmlns:ds", ns::DS)
        .start_children();
    w.open("ds:schemaRefs").start_children();
    w.open("ds:schemaRef").attr("ds:uri", ns::B).empty();
    w.close();
    w.close();
    w.finish()
}

// ---------------------------------------------------------------------------
// docProps
// ---------------------------------------------------------------------------

/// Redundant standards-based carrier for the fidelity payload.
///
/// LibreOffice Writer drops arbitrary `customXml` parts on save, but preserves
/// custom document properties. Keeping the canonical XML part and duplicating
/// its exact text here lets tools recover the evidence after either Word or
/// Writer round trips without placing hidden content in the document body.
fn build_custom_properties(fidelity_manifest: &str, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("Properties")
        .attr("xmlns", ns::CUSTOM_PROPERTIES)
        .attr("xmlns:vt", ns::DOC_PROPS_VTYPES)
        .start_children();
    w.open("property")
        .attr("fmtid", "{D5CDD505-2E9C-101B-9397-08002B2CF9AE}")
        .attr("pid", "2")
        .attr("name", "TypstFidelityManifestV1")
        .start_children();
    w.elem_text("vt:lpwstr", fidelity_manifest);
    w.close();
    w.close();
    w.finish()
}

fn build_core(info: &DocumentInfo, pretty: bool) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open("cp:coreProperties")
        .attr("xmlns:cp", ns::CP)
        .attr("xmlns:dc", ns::DC)
        .attr("xmlns:dcterms", ns::DCTERMS)
        .attr("xmlns:dcmitype", ns::DCMITYPE)
        .attr("xmlns:xsi", ns::XSI)
        .start_children();

    if let Some(title) = &info.title {
        w.elem_text("dc:title", title);
    }
    if !info.author.is_empty() {
        let authors = info.author.join("; ");
        w.elem_text("dc:creator", &authors);
        // Word also stores who last touched the document; with no editing
        // history the author is the best answer.
        w.elem_text("cp:lastModifiedBy", &authors);
    }
    if let Some(desc) = &info.description {
        w.elem_text("dc:description", desc);
    }
    if !info.keywords.is_empty() {
        w.elem_text("cp:keywords", &info.keywords.join(", "));
    }
    // A freshly generated document is revision 1.
    w.elem_text("cp:revision", "1");
    if let Smart::Custom(Some(date)) = &info.date
        && let Some(s) = w3cdtf(date)
    {
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
        .attr("xmlns", ns::EXTENDED_PROPS)
        .start_children();
    // The standard extended properties Word writes, in its usual field order. The
    // document statistics (Pages/Words/Characters/Lines/Paragraphs) are recomputed
    // by Word the moment it opens or saves the file, so they start at zero; the
    // rest is the fixed scaffolding every Word document carries.
    w.elem_text("Template", "Normal.dotm");
    w.elem_text("TotalTime", "0");
    w.elem_text("Pages", "1");
    w.elem_text("Words", "0");
    w.elem_text("Characters", "0");
    w.elem_text("Application", "Typst");
    w.elem_text("DocSecurity", "0");
    w.elem_text("Lines", "0");
    w.elem_text("Paragraphs", "0");
    w.elem_text("ScaleCrop", "false");
    w.elem_text("Company", "");
    w.elem_text("LinksUpToDate", "false");
    w.elem_text("CharactersWithSpaces", "0");
    w.elem_text("SharedDoc", "false");
    w.elem_text("HyperlinksChanged", "false");
    w.elem_text("AppVersion", "16.0000");
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

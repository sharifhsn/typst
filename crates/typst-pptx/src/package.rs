//! OPC (Open Packaging Conventions) zip package assembly.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};

use ecow::{EcoString, eco_format};
use rustc_hash::FxHashMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use typst_layout::PagedDocument;
use typst_library::layout::{Abs, Size};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, DateTime};

use crate::dom::{SlideCtx, SlideIr};
use crate::xml::{self, XmlWriter, escape_attr};

const REL_OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const REL_CORE_PROPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_EXTENDED_PROPS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const REL_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
const REL_SLIDE_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
const REL_SLIDE_LAYOUT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
const REL_THEME: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
const REL_PRES_PROPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps";
const REL_VIEW_PROPS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps";
const REL_TABLE_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles";

const CT_RELS: &str = "application/vnd.openxmlformats-package.relationships+xml";
const CT_PRESENTATION: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const CT_SLIDE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
const CT_SLIDE_MASTER: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
const CT_SLIDE_LAYOUT: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml";
const CT_THEME: &str = "application/vnd.openxmlformats-officedocument.theme+xml";
const CT_PRES_PROPS: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presProps+xml";
const CT_VIEW_PROPS: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml";
const CT_TABLE_STYLES: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml";
const CT_CORE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const CT_EXTENDED: &str =
    "application/vnd.openxmlformats-officedocument.extended-properties+xml";

/// Relationship target mode.
#[derive(Copy, Clone, Eq, PartialEq)]
enum RelMode {
    Internal,
    External,
}

#[derive(Clone)]
struct RelEntry {
    id: EcoString,
    type_uri: EcoString,
    target: EcoString,
    mode: RelMode,
}

/// A relationships container for one source part.
#[derive(Clone)]
struct Rels {
    next: u32,
    entries: Vec<RelEntry>,
    by_target: FxHashMap<EcoString, EcoString>,
}

impl Rels {
    fn new() -> Self {
        Self {
            next: 1,
            entries: Vec::new(),
            by_target: FxHashMap::default(),
        }
    }

    fn add(&mut self, type_uri: &str, target: &str, mode: RelMode) -> EcoString {
        let key: EcoString = eco_format!("{type_uri}\u{0}{target}");
        if let Some(existing) = self.by_target.get(&key) {
            return existing.clone();
        }

        let id: EcoString = eco_format!("rId{}", self.next);
        self.next += 1;
        self.entries.push(RelEntry {
            id: id.clone(),
            type_uri: type_uri.into(),
            target: target.into(),
            mode,
        });
        self.by_target.insert(key, id.clone());
        id
    }

    fn to_xml(&self) -> String {
        let mut s = String::from(xml::XML_DECL);
        s.push_str(
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for e in &self.entries {
            s.push_str("<Relationship Id=\"");
            s.push_str(&e.id);
            s.push_str("\" Type=\"");
            s.push_str(&escape_attr(&e.type_uri));
            s.push_str("\" Target=\"");
            s.push_str(&escape_attr(&e.target));
            s.push('"');
            if e.mode == RelMode::External {
                s.push_str(" TargetMode=\"External\"");
            }
            s.push_str("/>");
        }
        s.push_str("</Relationships>");
        s
    }
}

impl Default for Rels {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone)]
enum Compress {
    Deflate,
    Store,
}

struct Package {
    parts: Vec<(String, Vec<u8>, Compress)>,
    defaults: BTreeMap<String, &'static str>,
    overrides: Vec<(String, &'static str)>,
}

impl Package {
    fn new() -> Self {
        let mut defaults = BTreeMap::new();
        defaults.insert("gif".to_string(), "image/gif");
        defaults.insert("jpeg".to_string(), "image/jpeg");
        defaults.insert("jpg".to_string(), "image/jpeg");
        defaults.insert("png".to_string(), "image/png");
        defaults.insert("rels".to_string(), CT_RELS);
        defaults.insert("xml".to_string(), "application/xml");
        Self { parts: Vec::new(), defaults, overrides: Vec::new() }
    }

    fn add_xml(&mut self, part_name: &str, content_type: &'static str, body: String) {
        self.overrides.push((format!("/{part_name}"), content_type));
        self.parts
            .push((part_name.to_string(), body.into_bytes(), Compress::Deflate));
    }

    fn add_media(
        &mut self,
        part_name: &str,
        ext: &str,
        content_type: &'static str,
        bytes: Vec<u8>,
    ) {
        self.defaults.entry(ext.to_ascii_lowercase()).or_insert(content_type);
        self.parts.push((part_name.to_string(), bytes, Compress::Store));
    }

    fn content_types_xml(&self) -> String {
        let mut s = String::from(xml::XML_DECL);
        s.push_str(
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        );
        for (ext, ct) in &self.defaults {
            s.push_str("<Default Extension=\"");
            s.push_str(&escape_attr(ext));
            s.push_str("\" ContentType=\"");
            s.push_str(ct);
            s.push_str("\"/>");
        }
        for (part, ct) in &self.overrides {
            s.push_str("<Override PartName=\"");
            s.push_str(&escape_attr(part));
            s.push_str("\" ContentType=\"");
            s.push_str(ct);
            s.push_str("\"/>");
        }
        s.push_str("</Types>");
        s
    }

    fn finish(mut self, root_rels: &Rels) -> Vec<u8> {
        let content_types = self.content_types_xml();
        let root_rels_xml = root_rels.to_xml();

        let cursor = Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(cursor);
        let mtime = DateTime::default();

        let write_one = |zip: &mut ZipWriter<Cursor<Vec<u8>>>,
                         name: &str,
                         bytes: &[u8],
                         c: Compress| {
            let method = match c {
                Compress::Deflate => CompressionMethod::Deflated,
                Compress::Store => CompressionMethod::Stored,
            };
            let opts = SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(mtime)
                .unix_permissions(0o644);
            zip.start_file(name, opts).expect("zip start_file");
            zip.write_all(bytes).expect("zip write_all");
        };

        write_one(
            &mut zip,
            "[Content_Types].xml",
            content_types.as_bytes(),
            Compress::Deflate,
        );
        write_one(&mut zip, "_rels/.rels", root_rels_xml.as_bytes(), Compress::Deflate);

        let parts = std::mem::take(&mut self.parts);
        for (name, bytes, c) in &parts {
            write_one(&mut zip, name, bytes, *c);
        }

        zip.finish().expect("zip finish").into_inner()
    }
}

/// Write a complete PPTX package.
pub fn write(document: &PagedDocument, slides: &[SlideIr], ctx: &SlideCtx) -> Vec<u8> {
    let mut package = Package::new();
    let mut root_rels = Rels::new();
    let mut pres_rels = Rels::new();

    let slide_master_rid = pres_rels.add(
        REL_SLIDE_MASTER,
        "slideMasters/slideMaster1.xml",
        RelMode::Internal,
    );
    pres_rels.add(REL_PRES_PROPS, "presProps.xml", RelMode::Internal);
    pres_rels.add(REL_VIEW_PROPS, "viewProps.xml", RelMode::Internal);
    pres_rels.add(REL_THEME, "theme/theme1.xml", RelMode::Internal);
    pres_rels.add(REL_TABLE_STYLES, "tableStyles.xml", RelMode::Internal);

    let slide_rids = (0..slides.len())
        .map(|i| {
            pres_rels.add(
                REL_SLIDE,
                &format!("slides/slide{}.xml", i + 1),
                RelMode::Internal,
            )
        })
        .collect::<Vec<_>>();

    let (cx, cy) = first_page_size(document);
    package.add_xml(
        "ppt/presentation.xml",
        CT_PRESENTATION,
        presentation_xml(&slide_master_rid, &slide_rids, cx, cy),
    );
    package.add_xml("ppt/_rels/presentation.xml.rels", CT_RELS, pres_rels.to_xml());

    package.add_xml("ppt/presProps.xml", CT_PRES_PROPS, pres_props_xml());
    package.add_xml("ppt/viewProps.xml", CT_VIEW_PROPS, view_props_xml());
    package.add_xml("ppt/tableStyles.xml", CT_TABLE_STYLES, table_styles_xml());

    package.add_xml("ppt/theme/theme1.xml", CT_THEME, theme_xml());
    package.add_xml(
        "ppt/slideMasters/slideMaster1.xml",
        CT_SLIDE_MASTER,
        slide_master_xml(),
    );
    let mut master_rels = Rels::new();
    master_rels.add(
        REL_SLIDE_LAYOUT,
        "../slideLayouts/slideLayout1.xml",
        RelMode::Internal,
    );
    master_rels.add(REL_THEME, "../theme/theme1.xml", RelMode::Internal);
    package.add_xml(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        CT_RELS,
        master_rels.to_xml(),
    );

    package.add_xml(
        "ppt/slideLayouts/slideLayout1.xml",
        CT_SLIDE_LAYOUT,
        slide_layout_xml(),
    );
    let mut layout_rels = Rels::new();
    layout_rels.add(
        REL_SLIDE_MASTER,
        "../slideMasters/slideMaster1.xml",
        RelMode::Internal,
    );
    package.add_xml(
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        CT_RELS,
        layout_rels.to_xml(),
    );

    for (i, slide) in slides.iter().enumerate() {
        package.add_xml(
            &format!("ppt/slides/slide{}.xml", i + 1),
            CT_SLIDE,
            crate::encode::slide_xml(slide),
        );
        let mut slide_rels = Rels::new();
        slide_rels.add(
            REL_SLIDE_LAYOUT,
            "../slideLayouts/slideLayout1.xml",
            RelMode::Internal,
        );
        package.add_xml(
            &format!("ppt/slides/_rels/slide{}.xml.rels", i + 1),
            CT_RELS,
            slide_rels.to_xml(),
        );
    }

    for media in &ctx.media {
        package.add_media(
            &media.part_name,
            &media.ext,
            media_content_type(&media.ext),
            media.bytes.clone(),
        );
    }

    package.add_xml("docProps/core.xml", CT_CORE, core_xml());
    package.add_xml("docProps/app.xml", CT_EXTENDED, app_xml(slides.len()));

    root_rels.add(REL_OFFICE_DOCUMENT, "ppt/presentation.xml", RelMode::Internal);
    root_rels.add(REL_CORE_PROPS, "docProps/core.xml", RelMode::Internal);
    root_rels.add(REL_EXTENDED_PROPS, "docProps/app.xml", RelMode::Internal);

    package.finish(&root_rels)
}

fn first_page_size(document: &PagedDocument) -> (i64, i64) {
    let size = document
        .pages()
        .first()
        .map(|page| page.frame.size())
        .unwrap_or_else(|| Size::new(Abs::pt(720.0), Abs::pt(540.0)));
    (extent_emu(size.x), extent_emu(size.y))
}

fn emu(abs: Abs) -> i64 {
    (abs.to_pt() * 12700.0) as i64
}

fn extent_emu(abs: Abs) -> i64 {
    emu(abs).max(1)
}

fn presentation_xml(
    master_rid: &str,
    slide_rids: &[EcoString],
    cx: i64,
    cy: i64,
) -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:presentation")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .attr(
            "xmlns:r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        )
        .start_children();

    w.open("p:sldMasterIdLst").start_children();
    w.open("p:sldMasterId")
        .attr("id", "2147483648")
        .attr("r:id", master_rid)
        .empty();
    w.close();

    w.open("p:sldIdLst").start_children();
    for (i, rid) in slide_rids.iter().enumerate() {
        w.open("p:sldId")
            .attr("id", &(256 + i).to_string())
            .attr("r:id", rid)
            .empty();
    }
    w.close();

    w.open("p:sldSz")
        .attr("cx", &cx.to_string())
        .attr("cy", &cy.to_string())
        .attr("type", "custom")
        .empty();
    w.open("p:notesSz")
        .attr("cx", "6858000")
        .attr("cy", "9144000")
        .empty();
    w.close();
    w.finish()
}

fn slide_master_xml() -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:sldMaster")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .attr(
            "xmlns:r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        )
        .start_children();
    w.open("p:cSld").start_children();
    crate::encode::write_empty_shape_tree(&mut w);
    w.close();
    w.open("p:clrMap")
        .attr("bg1", "lt1")
        .attr("tx1", "dk1")
        .attr("bg2", "lt2")
        .attr("tx2", "dk2")
        .attr("accent1", "accent1")
        .attr("accent2", "accent2")
        .attr("accent3", "accent3")
        .attr("accent4", "accent4")
        .attr("accent5", "accent5")
        .attr("accent6", "accent6")
        .attr("hlink", "hlink")
        .attr("folHlink", "folHlink")
        .empty();
    w.open("p:sldLayoutIdLst").start_children();
    w.open("p:sldLayoutId")
        .attr("id", "2147483649")
        .attr("r:id", "rId1")
        .empty();
    w.close();
    write_text_styles(&mut w);
    w.close();
    w.finish()
}

fn slide_layout_xml() -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:sldLayout")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .attr(
            "xmlns:r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        )
        .attr("type", "blank")
        .attr("preserve", "1")
        .start_children();
    w.open("p:cSld").attr("name", "Blank").start_children();
    crate::encode::write_empty_shape_tree(&mut w);
    w.close();
    w.open("p:clrMapOvr").start_children();
    w.leaf("a:masterClrMapping");
    w.close();
    w.close();
    w.finish()
}

fn write_text_styles(w: &mut XmlWriter) {
    w.open("p:txStyles").start_children();
    for name in ["p:titleStyle", "p:bodyStyle", "p:otherStyle"] {
        w.open(name).start_children();
        w.open("a:lvl1pPr").attr("algn", "l").start_children();
        w.open("a:defRPr").attr("sz", "1800").start_children();
        w.open("a:solidFill").start_children();
        w.open("a:schemeClr").attr("val", "tx1").empty();
        w.close();
        w.open("a:latin").attr("typeface", "Arial").empty();
        w.open("a:ea").attr("typeface", "Arial").empty();
        w.open("a:cs").attr("typeface", "Arial").empty();
        w.close();
        w.close();
        w.close();
    }
    w.close();
}

fn theme_xml() -> String {
    format!(
        "{}{}",
        xml::XML_DECL,
        r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Typst">
<a:themeElements>
<a:clrScheme name="Typst">
<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
<a:dk2><a:srgbClr val="1F1F1F"/></a:dk2>
<a:lt2><a:srgbClr val="F2F2F2"/></a:lt2>
<a:accent1><a:srgbClr val="4472C4"/></a:accent1>
<a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
<a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>
<a:accent4><a:srgbClr val="FFC000"/></a:accent4>
<a:accent5><a:srgbClr val="5B9BD5"/></a:accent5>
<a:accent6><a:srgbClr val="70AD47"/></a:accent6>
<a:hlink><a:srgbClr val="0563C1"/></a:hlink>
<a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
</a:clrScheme>
<a:fontScheme name="Typst">
<a:majorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>
<a:minorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont>
</a:fontScheme>
<a:fmtScheme name="Typst">
<a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst>
<a:lnStyleLst>
<a:ln w="9525" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>
<a:ln w="25400" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>
<a:ln w="38100" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln>
</a:lnStyleLst>
<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>
<a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>
</a:fmtScheme>
</a:themeElements>
</a:theme>"#
    )
}

fn pres_props_xml() -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:presentationPr")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .empty();
    w.finish()
}

fn view_props_xml() -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:viewPr")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .start_children();
    w.open("p:normalViewPr").start_children();
    w.open("p:restoredLeft").attr("sz", "15620").empty();
    w.open("p:restoredTop").attr("sz", "94660").empty();
    w.close();
    w.close();
    w.finish()
}

fn table_styles_xml() -> String {
    let mut w = XmlWriter::new(false);
    w.open("a:tblStyleLst")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("def", "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}")
        .empty();
    w.finish()
}

fn core_xml() -> String {
    let ts = source_date_epoch_timestamp();
    let mut w = XmlWriter::new(false);
    w.open("cp:coreProperties")
        .attr(
            "xmlns:cp",
            "http://schemas.openxmlformats.org/package/2006/metadata/core-properties",
        )
        .attr("xmlns:dc", "http://purl.org/dc/elements/1.1/")
        .attr("xmlns:dcterms", "http://purl.org/dc/terms/")
        .attr("xmlns:dcmitype", "http://purl.org/dc/dcmitype/")
        .attr("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance")
        .start_children();
    w.elem_text("dc:creator", "Typst");
    w.elem_text("cp:lastModifiedBy", "Typst");
    w.elem_text("cp:revision", "1");
    for name in ["dcterms:created", "dcterms:modified"] {
        w.open(name).attr("xsi:type", "dcterms:W3CDTF").start_children();
        w.text(&ts);
        w.close();
    }
    w.close();
    w.finish()
}

fn source_date_epoch_timestamp() -> String {
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn app_xml(slides: usize) -> String {
    let mut w = XmlWriter::new(false);
    w.open("Properties")
        .attr(
            "xmlns",
            "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties",
        )
        .attr(
            "xmlns:vt",
            "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes",
        )
        .start_children();
    w.elem_text("Application", "Typst");
    w.elem_text("PresentationFormat", "Custom");
    w.elem_text("Slides", &slides.to_string());
    w.elem_text("Notes", "0");
    w.elem_text("HiddenSlides", "0");
    w.elem_text("MMClips", "0");
    w.elem_text("ScaleCrop", "false");
    w.elem_text("Company", "");
    w.elem_text("LinksUpToDate", "false");
    w.elem_text("SharedDoc", "false");
    w.elem_text("HyperlinksChanged", "false");
    w.elem_text("AppVersion", "16.0000");
    w.close();
    w.finish()
}

fn media_content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

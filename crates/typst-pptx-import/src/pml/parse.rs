//! PresentationML → [`PmlPackage`].
//!
//! Element matching is by **local name only**, never by prefix: a `.pptx` may
//! come from PowerPoint, LibreOffice, Google Slides, WPS or Keynote, and they
//! do not agree on prefixes. The same producer-agnostic rule the Word importer
//! follows, for the same reason.
//!
//! Relationship ids are namespaced per source part (`"slide1!rId2"`) before
//! they leave this module. Every part numbers its own relationships from
//! `rId1`, so an unqualified id is ambiguous the moment a second part is read
//! — the bug that cost the Word importer a debugging session.

use ecow::{EcoString, eco_format};
use roxmltree::{Document, Node};
use typst_ooxml_core::opc::{self, Reader, rels_part_name, resolve_target};

use super::model::*;
use crate::report::ImportReport;

pub struct Parser<'a> {
    reader: Reader<'a>,
    pub report: ImportReport,
    /// Every relationship in the package, keyed by namespaced id.
    rels: Vec<(EcoString, EcoString, EcoString)>,
}

/// What a resolved relationship points at.
pub struct Target {
    pub part: EcoString,
    pub kind: EcoString,
    pub external: bool,
}

impl<'a> Parser<'a> {
    pub fn new(reader: Reader<'a>) -> Self {
        Self {
            reader,
            report: ImportReport::default(),
            rels: Vec::new(),
        }
    }

    /// Read the whole package.
    ///
    /// Takes `&mut self` rather than `self` on purpose: the relationship table
    /// this builds is needed again during lowering, to resolve a picture's
    /// `r:embed` to a part. Consuming the parser here left the media lookups
    /// querying an empty table, and every picture in every deck silently
    /// vanished — the round trip caught it, no unit test would have.
    pub fn parse(&mut self) -> Result<PmlPackage, crate::ImportError> {
        let mut pkg = PmlPackage::default();

        let Some(pres_xml) = self.xml("ppt/presentation.xml")? else {
            return Err(crate::ImportError::NotAPresentation);
        };
        self.load_rels("ppt/presentation.xml");

        let doc = parse_xml(&pres_xml)?;
        let root = doc.root_element();

        // Slide size first: every geometry decision downstream is relative to
        // it, including the emitted page.
        if let Some(sz) = child(root, "sldSz") {
            pkg.size = SlideSize {
                cx: num(sz, "cx").unwrap_or(9144000),
                cy: num(sz, "cy").unwrap_or(6858000),
            };
        }

        // Masters and layouts must exist before slides, since a slide's
        // formatting is resolved against them.
        let master_parts = self.rel_parts("ppt/presentation.xml", "slideMaster");
        for part in &master_parts {
            let master = self.parse_master(part)?;
            pkg.masters.push(master);
        }
        // A layout is reached from its master, not from the presentation.
        let mut layout_parts: Vec<(EcoString, usize)> = Vec::new();
        for (mi, part) in master_parts.iter().enumerate() {
            for lp in self.rel_parts(part, "slideLayout") {
                layout_parts.push((lp, mi));
            }
        }
        for (part, master) in &layout_parts {
            let mut layout = self.parse_layout(part)?;
            layout.master = Some(*master);
            pkg.layouts.push(layout);
        }

        // The theme hangs off the first master; decks with several masters and
        // several themes exist, but one colour scheme is all a Typst document
        // can carry.
        if let Some(master) = master_parts.first()
            && let Some(theme_part) = self.rel_parts(master, "theme").first()
        {
            pkg.theme = self.parse_theme(theme_part)?;
        }

        // `p:sldIdLst` is the presentation order. Archive order is not.
        for slide_part in self.slide_parts(root, "ppt/presentation.xml") {
            let mut slide = self.parse_slide(&slide_part)?;
            slide.layout = self
                .rel_parts(&slide_part, "slideLayout")
                .first()
                .and_then(|lp| layout_parts.iter().position(|(p, _)| p == lp));
            pkg.slides.push(slide);
        }

        Ok(pkg)
    }

    fn xml(&mut self, part: &str) -> Result<Option<String>, crate::ImportError> {
        self.reader.xml_part(part).map_err(crate::ImportError::Package)
    }

    pub fn bytes(&mut self, part: &str) -> Option<Vec<u8>> {
        self.reader.part_bytes(part).ok().flatten()
    }

    /// Read a part's `_rels` sidecar and record every relationship under a
    /// namespaced id.
    fn load_rels(&mut self, part: &str) {
        let rels_name = rels_part_name(part);
        let Ok(Some(xml)) = self.reader.xml_part(&rels_name) else { return };
        let Ok(doc) = Document::parse(&xml) else { return };
        for rel in opc::rel_entries(doc.root()) {
            let resolved = if rel.external {
                rel.target.clone()
            } else {
                resolve_target(part, &rel.target)
            };
            let kind = rel.type_uri.rsplit('/').next().unwrap_or(&rel.type_uri);
            self.rels.push((
                eco_format!("{part}!{}", rel.id),
                EcoString::from(kind),
                resolved,
            ));
        }
    }

    /// Every part this part points at with the given relationship type.
    fn rel_parts(&mut self, part: &str, kind: &str) -> Vec<EcoString> {
        self.load_rels(part);
        let prefix = eco_format!("{part}!");
        self.rels
            .iter()
            .filter(|(id, k, _)| id.starts_with(prefix.as_str()) && k == kind)
            .map(|(_, _, target)| target.clone())
            .collect()
    }

    /// Resolve a namespaced relationship id.
    pub fn target(&self, id: &str) -> Option<Target> {
        self.rels
            .iter()
            .find(|(rid, _, _)| rid == id)
            .map(|(_, kind, target)| Target {
                part: target.clone(),
                kind: kind.clone(),
                external: !target.starts_with("ppt/") && target.contains(':'),
            })
    }

    /// Slide parts in `p:sldIdLst` order.
    fn slide_parts(&mut self, root: Node, part: &str) -> Vec<EcoString> {
        let Some(list) = child(root, "sldIdLst") else { return Vec::new() };
        list.children()
            .filter(|n| is_el(*n, "sldId"))
            .filter_map(|n| rel_attr(n))
            .filter_map(|id| {
                let key = eco_format!("{part}!{id}");
                self.rels
                    .iter()
                    .find(|(rid, _, _)| *rid == key)
                    .map(|(_, _, t)| t.clone())
            })
            .collect()
    }

    fn parse_slide(&mut self, part: &str) -> Result<Slide, crate::ImportError> {
        let Some(xml) = self.xml(part)? else { return Ok(Slide::default()) };
        self.load_rels(part);
        let doc = parse_xml(&xml)?;
        let root = doc.root_element();

        let mut slide = Slide {
            part: part.into(),
            hidden: attr(root, "show") == Some("0"),
            hide_master_shapes: attr(root, "showMasterSp") == Some("0"),
            bg: child(root, "cSld")
                .and_then(|c| child(c, "bg"))
                .and_then(|b| self.parse_bg(b)),
            ..Slide::default()
        };
        if let Some(tree) = child(root, "cSld").and_then(|c| child(c, "spTree")) {
            slide.shapes = self.parse_shape_tree(tree, part);
        }
        // Speaker notes live in their own part, reached by relationship.
        if let Some(notes_part) = self.rel_parts(part, "notesSlide").first().cloned()
            && let Some(notes_xml) = self.xml(&notes_part)?
        {
            let notes_doc = parse_xml(&notes_xml)?;
            let text = notes_text(notes_doc.root_element());
            if !text.trim().is_empty() {
                slide.notes = Some(text);
            }
        }
        Ok(slide)
    }

    fn parse_layout(&mut self, part: &str) -> Result<SlideLayout, crate::ImportError> {
        let Some(xml) = self.xml(part)? else { return Ok(SlideLayout::default()) };
        self.load_rels(part);
        let doc = parse_xml(&xml)?;
        let root = doc.root_element();
        let mut layout = SlideLayout {
            name: child(root, "cSld").and_then(|c| attr(c, "name")).unwrap_or("").into(),
            hide_master_shapes: attr(root, "showMasterSp") == Some("0"),
            bg: child(root, "cSld")
                .and_then(|c| child(c, "bg"))
                .and_then(|b| self.parse_bg(b)),
            ..SlideLayout::default()
        };
        if let Some(tree) = child(root, "cSld").and_then(|c| child(c, "spTree")) {
            layout.shapes = self.parse_shape_tree(tree, part);
        }
        Ok(layout)
    }

    fn parse_master(&mut self, part: &str) -> Result<SlideMaster, crate::ImportError> {
        let Some(xml) = self.xml(part)? else { return Ok(SlideMaster::default()) };
        self.load_rels(part);
        let doc = parse_xml(&xml)?;
        let root = doc.root_element();
        let mut master = SlideMaster {
            bg: child(root, "cSld")
                .and_then(|c| child(c, "bg"))
                .and_then(|b| self.parse_bg(b)),
            ..SlideMaster::default()
        };
        if let Some(map) = child(root, "clrMap") {
            master.color_map = ColorMap {
                bg1: attr(map, "bg1").unwrap_or("lt1").into(),
                tx1: attr(map, "tx1").unwrap_or("dk1").into(),
                bg2: attr(map, "bg2").unwrap_or("lt2").into(),
                tx2: attr(map, "tx2").unwrap_or("dk2").into(),
            };
        }
        if let Some(tree) = child(root, "cSld").and_then(|c| child(c, "spTree")) {
            master.shapes = self.parse_shape_tree(tree, part);
        }
        if let Some(styles) = child(root, "txStyles") {
            master.text_styles = TextStyles {
                title: child(styles, "titleStyle").map(level_styles).unwrap_or_default(),
                body: child(styles, "bodyStyle").map(level_styles).unwrap_or_default(),
                other: child(styles, "otherStyle").map(level_styles).unwrap_or_default(),
            };
        }
        Ok(master)
    }

    fn parse_theme(&mut self, part: &str) -> Result<Theme, crate::ImportError> {
        let Some(xml) = self.xml(part)? else { return Ok(Theme::default()) };
        let doc = parse_xml(&xml)?;
        let root = doc.root_element();
        let mut theme = Theme::default();
        let Some(elements) = child(root, "themeElements") else { return Ok(theme) };

        if let Some(scheme) = child(elements, "clrScheme") {
            for slot in scheme.children().filter(|n| n.is_element()) {
                let name = EcoString::from(local(slot));
                if let Some(rgb) = scheme_slot_rgb(slot) {
                    theme.colors.push((name, rgb));
                }
            }
        }
        if let Some(fonts) = child(elements, "fontScheme") {
            theme.major_font = child(fonts, "majorFont")
                .and_then(|f| child(f, "latin"))
                .and_then(|l| attr(l, "typeface"))
                .filter(|t| !t.is_empty())
                .map(Into::into);
            theme.minor_font = child(fonts, "minorFont")
                .and_then(|f| child(f, "latin"))
                .and_then(|l| attr(l, "typeface"))
                .filter(|t| !t.is_empty())
                .map(Into::into);
        }
        Ok(theme)
    }

    fn parse_bg(&mut self, bg: Node) -> Option<Fill> {
        // `p:bgPr` states a fill directly; `p:bgRef` points into the theme's
        // background fill list, which this importer does not resolve — the
        // reference is reported by the caller rather than guessed at.
        child(bg, "bgPr").and_then(|pr| self.parse_fill_container(pr))
    }

    fn parse_shape_tree(&mut self, tree: Node, part: &str) -> Vec<Shape> {
        tree.children()
            .filter(|n| n.is_element())
            .filter_map(|n| self.parse_shape(n, part))
            .collect()
    }

    fn parse_shape(&mut self, node: Node, part: &str) -> Option<Shape> {
        match local(node) {
            "sp" => Some(Shape::Text(self.parse_text_shape(node))),
            "cxnSp" => Some(Shape::Connector(self.parse_text_shape(node))),
            "pic" => self.parse_picture(node, part).map(Shape::Picture),
            "grpSp" => Some(Shape::Group(self.parse_group(node, part))),
            "graphicFrame" => self.parse_graphic_frame(node, part),
            // A media object or an embedded control: recognised, no Typst home.
            "contentPart" => Some(Shape::Unsupported {
                kind: "an embedded content part".into(),
                xfrm: None,
            }),
            _ => None,
        }
    }

    fn parse_group(&mut self, node: Node, part: &str) -> Group {
        let mut group = Group::default();
        if let Some(pr) = child(node, "grpSpPr")
            && let Some(x) = child(pr, "xfrm")
        {
            group.xfrm = Some(parse_xfrm(x));
            if let Some(off) = child(x, "chOff") {
                group.child_off =
                    Some((num(off, "x").unwrap_or(0), num(off, "y").unwrap_or(0)));
            }
            if let Some(ext) = child(x, "chExt") {
                group.child_ext =
                    Some((num(ext, "cx").unwrap_or(0), num(ext, "cy").unwrap_or(0)));
            }
        }
        group.shapes = node
            .children()
            .filter(|n| n.is_element() && !matches!(local(*n), "nvGrpSpPr" | "grpSpPr"))
            .filter_map(|n| self.parse_shape(n, part))
            .collect();
        group
    }

    fn parse_text_shape(&mut self, node: Node) -> TextShape {
        let mut shape = TextShape::default();

        if let Some(nv) = child(node, "nvSpPr") {
            if let Some(cnv) = child(nv, "cNvSpPr") {
                shape.is_text_box = attr(cnv, "txBox") == Some("1");
            }
            if let Some(ph) = child(nv, "nvPr").and_then(|p| child(p, "ph")) {
                shape.placeholder = Some(Placeholder {
                    kind: PhKind::parse(attr(ph, "type").unwrap_or("")),
                    idx: attr(ph, "idx").and_then(|v| v.parse().ok()),
                });
            }
        }
        if let Some(pr) = child(node, "spPr") {
            shape.xfrm = child(pr, "xfrm").map(parse_xfrm);
            shape.geom = parse_geometry(pr);
            shape.fill = self.parse_fill_container(pr);
            shape.line = child(pr, "ln").map(|l| self.parse_line(l));
        }
        if let Some(body) = child(node, "txBody") {
            if let Some(list) = child(body, "lstStyle") {
                shape.list_style = level_styles(list);
            }
            if let Some(body_pr) = child(body, "bodyPr") {
                shape.anchor = attr(body_pr, "anchor").map(Into::into);
                let d = Insets::default();
                shape.insets = Insets {
                    l: num(body_pr, "lIns").unwrap_or(d.l),
                    t: num(body_pr, "tIns").unwrap_or(d.t),
                    r: num(body_pr, "rIns").unwrap_or(d.r),
                    b: num(body_pr, "bIns").unwrap_or(d.b),
                };
            }
            shape.paras = body
                .children()
                .filter(|n| is_el(*n, "p"))
                .map(|p| self.parse_para(p))
                .collect();
        }
        shape
    }

    fn parse_picture(&mut self, node: Node, part: &str) -> Option<Picture> {
        let mut pic = Picture::default();
        if let Some(nv) = child(node, "nvPicPr")
            && let Some(cnv) = child(nv, "cNvPr")
        {
            pic.alt = attr(cnv, "descr").filter(|d| !d.is_empty()).map(Into::into);
        }
        if let Some(fill) = child(node, "blipFill") {
            let blip = child(fill, "blip")?;
            let embed = rel_attr(blip)?;
            pic.rel_id = eco_format!("{part}!{embed}");
            // A native SVG rides in an extension list beside the raster.
            if let Some(ext_lst) = child(blip, "extLst") {
                for ext in ext_lst.children().filter(|n| is_el(*n, "ext")) {
                    for c in ext.children().filter(|n| is_el(*n, "svgBlip")) {
                        if let Some(id) = rel_attr(c) {
                            pic.svg_rel_id = Some(eco_format!("{part}!{id}"));
                        }
                    }
                }
            }
            if let Some(src) = child(fill, "srcRect") {
                let crop = [
                    num(src, "l").unwrap_or(0) as i32,
                    num(src, "t").unwrap_or(0) as i32,
                    num(src, "r").unwrap_or(0) as i32,
                    num(src, "b").unwrap_or(0) as i32,
                ];
                if crop != [0; 4] {
                    pic.crop = Some(crop);
                }
            }
        }
        if let Some(pr) = child(node, "spPr") {
            pic.xfrm = child(pr, "xfrm").map(parse_xfrm);
            pic.geom = parse_geometry(pr);
        }
        (!pic.rel_id.is_empty()).then_some(pic)
    }

    /// A `p:graphicFrame` is the envelope for a table, a chart or SmartArt —
    /// three very different things behind one element, told apart by the URI
    /// on their `a:graphicData`.
    fn parse_graphic_frame(&mut self, node: Node, part: &str) -> Option<Shape> {
        let xfrm = child(node, "xfrm").map(parse_xfrm);
        let data = child(node, "graphic").and_then(|g| child(g, "graphicData"))?;
        let uri = attr(data, "uri").unwrap_or("");

        if let Some(tbl) = child(data, "tbl") {
            let mut table = self.parse_table(tbl);
            table.xfrm = xfrm;
            return Some(Shape::Table(table));
        }
        // A chart names its part by relationship, and that part holds the
        // cached values PowerPoint last drew — enough to rebuild the data.
        if uri.contains("/chart")
            && let Some(id) = descend(data, "chart").and_then(rel_attr)
        {
            return Some(Shape::Chart { rel_id: eco_format!("{part}!{id}"), xfrm });
        }
        let kind: EcoString = if uri.contains("/chart") {
            "a chart".into()
        } else if uri.contains("/diagram") {
            "a SmartArt diagram".into()
        } else if uri.contains("/ole") {
            "an embedded OLE object".into()
        } else {
            "an unrecognised graphic frame".into()
        };
        Some(Shape::Unsupported { kind, xfrm })
    }

    fn parse_table(&mut self, node: Node) -> Table {
        let mut table = Table::default();
        if let Some(pr) = child(node, "tblPr") {
            table.first_row_header = attr(pr, "firstRow") == Some("1");
            table.style_id = child(pr, "tableStyleId")
                .and_then(|s| s.text())
                .filter(|t| !t.is_empty())
                .map(Into::into);
        }
        if let Some(grid) = child(node, "tblGrid") {
            for col in grid.children().filter(|n| is_el(*n, "gridCol")) {
                table.grid.push(num(col, "w").unwrap_or(0));
            }
        }
        for tr in node.children().filter(|n| is_el(*n, "tr")) {
            let mut row = TableRow {
                height: num(tr, "h").unwrap_or(0),
                cells: Vec::new(),
            };
            for tc in tr.children().filter(|n| is_el(*n, "tc")) {
                let mut cell = TableCell {
                    grid_span: num(tc, "gridSpan").unwrap_or(1).max(1) as usize,
                    row_span: num(tc, "rowSpan").unwrap_or(1).max(1) as usize,
                    merged: attr(tc, "hMerge") == Some("1")
                        || attr(tc, "vMerge") == Some("1"),
                    ..TableCell::default()
                };
                if let Some(body) = child(tc, "txBody") {
                    cell.paras = body
                        .children()
                        .filter(|n| is_el(*n, "p"))
                        .map(|p| self.parse_para(p))
                        .collect();
                }
                if let Some(pr) = child(tc, "tcPr") {
                    // Order matters: left, top, right, bottom, matching the
                    // order the mapper reads them back in.
                    for (index, name) in ["lnL", "lnT", "lnR", "lnB"].iter().enumerate() {
                        cell.borders[index] = child(pr, name).map(|l| self.parse_line(l));
                    }
                    cell.anchor = attr(pr, "anchor").map(Into::into);
                    cell.fill = self.parse_fill_container(pr);
                    let d = Insets::default();
                    cell.insets = Insets {
                        l: num(pr, "marL").unwrap_or(d.l),
                        t: num(pr, "marT").unwrap_or(d.t),
                        r: num(pr, "marR").unwrap_or(d.r),
                        b: num(pr, "marB").unwrap_or(d.b),
                    };
                }
                row.cells.push(cell);
            }
            table.rows.push(row);
        }
        table
    }

    fn parse_para(&mut self, node: Node) -> Para {
        let mut para = Para::default();
        if let Some(pr) = child(node, "pPr") {
            para.props = parse_para_props(pr);
        }
        for child_node in node.children().filter(|n| n.is_element()) {
            match local(child_node) {
                "r" => para.runs.push(self.parse_run(child_node)),
                "br" => para.runs.push(Run { line_break: true, ..Run::default() }),
                "fld" => {
                    let mut run = self.parse_run(child_node);
                    run.field = attr(child_node, "type").map(Into::into);
                    para.runs.push(run);
                }
                _ => {}
            }
        }
        para
    }

    fn parse_run(&mut self, node: Node) -> Run {
        let mut run = Run::default();
        if let Some(t) = child(node, "t") {
            run.text = t.text().unwrap_or("").into();
        }
        if let Some(pr) = child(node, "rPr") {
            run.props = self.parse_run_props(pr);
            if let Some(link) = child(pr, "hlinkClick")
                && let Some(id) = rel_attr(link)
            {
                run.link = Some(Hyperlink::Rel(id.into()));
            }
        }
        run
    }

    fn parse_run_props(&mut self, pr: Node) -> RunProps {
        RunProps {
            size: attr(pr, "sz").and_then(|v| v.parse().ok()),
            bold: attr(pr, "b").map(|v| v == "1" || v == "true"),
            italic: attr(pr, "i").map(|v| v == "1" || v == "true"),
            underline: attr(pr, "u").filter(|v| *v != "none").map(Into::into),
            strike: attr(pr, "strike").filter(|v| *v != "noStrike").map(Into::into),
            color: child(pr, "solidFill").and_then(color_in),
            font: child(pr, "latin")
                .and_then(|l| attr(l, "typeface"))
                .filter(|t| !t.is_empty() && !t.starts_with('+'))
                .map(Into::into),
            east_asian: child(pr, "ea")
                .and_then(|l| attr(l, "typeface"))
                .filter(|t| !t.is_empty() && !t.starts_with('+'))
                .map(Into::into),
            spacing: attr(pr, "spc").and_then(|v| v.parse().ok()),
            baseline: attr(pr, "baseline").and_then(|v| v.parse().ok()),
            highlight: child(pr, "highlight").and_then(color_in),
            caps: attr(pr, "cap").filter(|v| *v != "none").map(Into::into),
            lang: attr(pr, "lang").map(Into::into),
        }
    }

    fn parse_fill_container(&mut self, pr: Node) -> Option<Fill> {
        for c in pr.children().filter(|n| n.is_element()) {
            match local(c) {
                "noFill" => return Some(Fill::None),
                "solidFill" => return color_in(c).map(Fill::Solid),
                "gradFill" => return Some(parse_gradient(c)),
                "blipFill" => {
                    return child(c, "blip")
                        .and_then(rel_attr)
                        .map(|id| Fill::Picture { rel_id: id.into() });
                }
                "pattFill" => {
                    return child(c, "fgClr")
                        .and_then(color_in)
                        .map(|fg| Fill::Pattern { fg });
                }
                _ => {}
            }
        }
        None
    }

    fn parse_line(&mut self, ln: Node) -> Line {
        Line {
            width: num(ln, "w"),
            fill: self.parse_fill_container(ln),
            dash: child(ln, "prstDash").and_then(|d| attr(d, "val")).map(Into::into),
            arrowheads: child(ln, "headEnd").is_some() || child(ln, "tailEnd").is_some(),
            custom_dash: child(ln, "custDash")
                .map(|c| {
                    c.children()
                        .filter(|n| is_el(*n, "ds"))
                        .map(|d| (num(d, "d").unwrap_or(0), num(d, "sp").unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

// --- free helpers -----------------------------------------------------------

fn parse_xml(text: &str) -> Result<Document<'_>, crate::ImportError> {
    Document::parse(text).map_err(|e| crate::ImportError::Xml(eco_format!("{e}")))
}

// The local-name XML read helpers are shared with the DOCX importer; re-export
// them so the rest of this crate keeps referring to `crate::pml::parse::*`.
pub use typst_ooxml_core::xmlread::{attr, child, is_el, local};

/// A descendant search, for the elements OOXML buries a level or two down.
pub fn descend<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.descendants().find(|n| is_el(*n, name))
}

/// `r:id` / `r:embed`, whichever the element uses.
fn rel_attr<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .find(|a| matches!(a.name(), "id" | "embed" | "link") && a.namespace().is_some())
        .map(|a| a.value())
}

fn num(node: Node, name: &str) -> Option<i64> {
    attr(node, name).and_then(|v| v.parse().ok())
}

fn parse_xfrm(node: Node) -> Xfrm {
    let off = child(node, "off");
    let ext = child(node, "ext");
    Xfrm {
        x: off.and_then(|o| num(o, "x")).unwrap_or(0),
        y: off.and_then(|o| num(o, "y")).unwrap_or(0),
        cx: ext.and_then(|e| num(e, "cx")).unwrap_or(0),
        cy: ext.and_then(|e| num(e, "cy")).unwrap_or(0),
        rot: num(node, "rot").unwrap_or(0) as i32,
        flip_h: attr(node, "flipH") == Some("1"),
        flip_v: attr(node, "flipV") == Some("1"),
    }
}

fn parse_geometry(pr: Node) -> Option<Geometry> {
    if let Some(preset) = child(pr, "prstGeom") {
        let name = attr(preset, "prst").unwrap_or("rect");
        let adjust = child(preset, "avLst")
            .map(|av| {
                av.children()
                    .filter(|n| is_el(*n, "gd"))
                    .filter_map(|gd| {
                        let name = attr(gd, "name")?;
                        // `fmla` is "val 12345" for a literal adjustment.
                        let fmla = attr(gd, "fmla")?;
                        let v = fmla.strip_prefix("val ")?.trim().parse().ok()?;
                        Some((EcoString::from(name), v))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Some(Geometry::Preset { name: name.into(), adjust });
    }
    if let Some(custom) = child(pr, "custGeom") {
        let path = child(custom, "pathLst").and_then(|l| child(l, "path"))?;
        let mut segs = Vec::new();
        for seg in path.children().filter(|n| n.is_element()) {
            match local(seg) {
                "moveTo" => {
                    if let Some((x, y)) = pt_child(seg) {
                        segs.push(Seg::Move(x, y));
                    }
                }
                "lnTo" => {
                    if let Some((x, y)) = pt_child(seg) {
                        segs.push(Seg::Line(x, y));
                    }
                }
                "cubicBezTo" => {
                    let pts: Vec<_> = seg
                        .children()
                        .filter(|n| is_el(*n, "pt"))
                        .filter_map(|p| Some((num(p, "x")?, num(p, "y")?)))
                        .collect();
                    if let [c1, c2, end] = pts.as_slice() {
                        segs.push(Seg::Cubic(c1.0, c1.1, c2.0, c2.1, end.0, end.1));
                    }
                }
                "close" => segs.push(Seg::Close),
                _ => {}
            }
        }
        return Some(Geometry::Custom {
            w: num(path, "w").unwrap_or(0),
            h: num(path, "h").unwrap_or(0),
            segs,
        });
    }
    None
}

fn pt_child(node: Node) -> Option<(i64, i64)> {
    let pt = child(node, "pt")?;
    Some((num(pt, "x")?, num(pt, "y")?))
}

fn parse_gradient(node: Node) -> Fill {
    let mut stops = Vec::new();
    if let Some(list) = child(node, "gsLst") {
        for gs in list.children().filter(|n| is_el(*n, "gs")) {
            let pos = num(gs, "pos").unwrap_or(0) as u32;
            if let Some(color) = color_in(gs) {
                stops.push((pos, color));
            }
        }
    }
    let angle = child(node, "lin").and_then(|l| num(l, "ang")).map(|a| a as i32);
    let radial = child(node, "path").is_some();
    Fill::Gradient { stops, angle, radial }
}

/// The colour *inside* a container (`a:solidFill`, `a:gs`, `a:fgClr`).
fn color_in(node: Node) -> Option<Color> {
    for c in node.children().filter(|n| n.is_element()) {
        match local(c) {
            "srgbClr" => {
                let rgb = hex(attr(c, "val")?)?;
                return Some(Color::Srgb(rgb));
            }
            "schemeClr" => {
                return Some(Color::Scheme {
                    slot: attr(c, "val")?.into(),
                    transforms: color_transforms(c),
                });
            }
            "sysClr" => {
                let rgb = attr(c, "lastClr").and_then(hex).unwrap_or([0, 0, 0]);
                return Some(Color::System(rgb));
            }
            _ => {}
        }
    }
    None
}

fn color_transforms(node: Node) -> Vec<ColorTransform> {
    node.children()
        .filter(|n| n.is_element())
        .filter_map(|n| {
            let v: u32 = attr(n, "val")?.parse().ok()?;
            Some(match local(n) {
                "alpha" => ColorTransform::Alpha(v),
                "lumMod" => ColorTransform::LumMod(v),
                "lumOff" => ColorTransform::LumOff(v),
                "shade" => ColorTransform::Shade(v),
                "tint" => ColorTransform::Tint(v),
                _ => return None,
            })
        })
        .collect()
}

fn scheme_slot_rgb(slot: Node) -> Option<[u8; 3]> {
    for c in slot.children().filter(|n| n.is_element()) {
        match local(c) {
            "srgbClr" => return attr(c, "val").and_then(hex),
            "sysClr" => return attr(c, "lastClr").and_then(hex),
            _ => {}
        }
    }
    None
}

fn hex(value: &str) -> Option<[u8; 3]> {
    let v = value.trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&v[0..2], 16).ok()?,
        u8::from_str_radix(&v[2..4], 16).ok()?,
        u8::from_str_radix(&v[4..6], 16).ok()?,
    ])
}

fn parse_para_props(pr: Node) -> ParaProps {
    let mut props = ParaProps {
        level: attr(pr, "lvl").and_then(|v| v.parse().ok()).unwrap_or(0),
        align: attr(pr, "algn").map(Into::into),
        margin_left: num(pr, "marL"),
        indent: num(pr, "indent"),
        rtl: attr(pr, "rtl") == Some("1"),
        ..ParaProps::default()
    };
    for c in pr.children().filter(|n| n.is_element()) {
        match local(c) {
            "buNone" => props.bullet = Some(Bullet::None),
            "buChar" => {
                // `a:buFont` is a sibling, not a child — and it is the whole
                // difference between a bullet and a tofu box, since most
                // themed decks pick their glyph out of Wingdings.
                let font = child(pr, "buFont")
                    .and_then(|f| attr(f, "typeface"))
                    .filter(|t| !t.is_empty())
                    .map(EcoString::from);
                props.bullet = attr(c, "char")
                    .map(|ch| Bullet::Char { glyph: EcoString::from(ch), font });
            }
            "buAutoNum" => {
                props.bullet = Some(Bullet::AutoNum {
                    kind: attr(c, "type").unwrap_or("arabicPeriod").into(),
                    start: attr(c, "startAt").and_then(|v| v.parse().ok()).unwrap_or(1),
                });
            }
            "lnSpc" => props.line_spacing = spacing_in(c),
            "spcBef" => props.space_before = spacing_in(c),
            "spcAft" => props.space_after = spacing_in(c),
            _ => {}
        }
    }
    props
}

fn spacing_in(node: Node) -> Option<Spacing> {
    for c in node.children().filter(|n| n.is_element()) {
        match local(c) {
            "spcPct" => return num(c, "val").map(Spacing::Percent),
            "spcPts" => return num(c, "val").map(Spacing::Points),
            _ => {}
        }
    }
    None
}

/// The nine `a:lvlNpPr` children of a master text style, in level order.
fn level_styles(node: Node) -> Vec<LevelStyle> {
    let mut out = vec![LevelStyle::default(); 9];
    for c in node.children().filter(|n| n.is_element()) {
        let name = local(c);
        let Some(rest) = name.strip_prefix("lvl") else { continue };
        let Some(digit) = rest.strip_suffix("pPr") else { continue };
        let Ok(level) = digit.parse::<usize>() else { continue };
        if !(1..=9).contains(&level) {
            continue;
        }
        let mut style = LevelStyle { para: parse_para_props(c), ..LevelStyle::default() };
        if let Some(def) = child(c, "defRPr") {
            style.run = RunProps {
                size: attr(def, "sz").and_then(|v| v.parse().ok()),
                bold: attr(def, "b").map(|v| v == "1"),
                italic: attr(def, "i").map(|v| v == "1"),
                color: child(def, "solidFill").and_then(color_in),
                font: child(def, "latin")
                    .and_then(|l| attr(l, "typeface"))
                    .filter(|t| !t.is_empty() && !t.starts_with('+'))
                    .map(Into::into),
                ..RunProps::default()
            };
        }
        out[level - 1] = style;
    }
    out
}

/// All text in a notes slide's body placeholder, flattened.
fn notes_text(root: Node) -> EcoString {
    let mut out = EcoString::new();
    for shape in root.descendants().filter(|n| is_el(*n, "sp")) {
        // Only the body placeholder holds the note; the slide-image
        // placeholder and the number are furniture.
        let is_body = child(shape, "nvSpPr")
            .and_then(|nv| child(nv, "nvPr"))
            .and_then(|nv| child(nv, "ph"))
            .and_then(|ph| attr(ph, "type"))
            .is_none_or(|t| t == "body");
        if !is_body {
            continue;
        }
        for para in shape.descendants().filter(|n| is_el(*n, "p")) {
            let mut line = EcoString::new();
            for t in para.descendants().filter(|n| is_el(*n, "t")) {
                line.push_str(t.text().unwrap_or(""));
            }
            if !line.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&line);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_targets_resolve_relative_to_their_own_part() {
        // The case that matters: a slide points at its layout with `../`.
        assert_eq!(
            resolve_target("ppt/slides/slide1.xml", "../slideLayouts/slideLayout2.xml"),
            "ppt/slideLayouts/slideLayout2.xml"
        );
        assert_eq!(
            resolve_target("ppt/presentation.xml", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_target("ppt/presentation.xml", "/docProps/app.xml"),
            "docProps/app.xml"
        );
    }

    #[test]
    fn rels_sidecar_name_is_derived_from_the_part() {
        assert_eq!(
            rels_part_name("ppt/slides/slide1.xml"),
            "ppt/slides/_rels/slide1.xml.rels"
        );
        assert_eq!(
            rels_part_name("ppt/presentation.xml"),
            "ppt/_rels/presentation.xml.rels"
        );
    }
}

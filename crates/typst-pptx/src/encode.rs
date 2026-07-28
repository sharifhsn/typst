use ecow::EcoString;
use typst_ooxml_core::{dml, ns};

use crate::dom::{
    BulletKind, CellHAlign, CellVAlign, FillSpec, GeomKind, GeomShape, GroupShape,
    InlineMath, LinkOverlay, MathBox, MediaId, PathGeom, Pic, PicGeom, Placeholder,
    RunLink, SlideIr, SlideShape, StrokeSpec, TableBox, TableCell, TextBox, TextChild,
    TextColumns, TextField, TextPara, TextRun, TextWrap,
};
use crate::xml::{self, XmlWriter};

/// Relationship hooks needed while emitting slide XML.
pub(crate) trait SlideRelSink {
    fn image_rid(&mut self, media: MediaId) -> EcoString;
    fn hyperlink_rid(&mut self, target: &str) -> EcoString;
    fn slide_rid(&mut self, slide: usize) -> EcoString;
}

/// Build a slide XML part.
pub(crate) fn slide_xml(slide: &SlideIr, rels: &mut impl SlideRelSink) -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:sld")
        .attr("xmlns:a", ns::A)
        .attr("xmlns:p", ns::P)
        .attr("xmlns:r", ns::R);
    if slide_contains_math(slide) {
        w.attr("xmlns:mc", ns::MC)
            .attr("xmlns:m", ns::M)
            .attr("xmlns:a14", ns::A14)
            .attr("mc:Ignorable", "a14");
    }
    w.start_children();

    w.open("p:cSld").start_children();
    if let Some(fill) = &slide.bg {
        write_background(&mut w, fill);
    }
    write_shape_tree(&mut w, slide, rels);
    w.close();

    w.open("p:clrMapOvr").start_children();
    w.leaf("a:masterClrMapping");
    w.close();
    w.close();
    w.finish()
}

fn write_background(w: &mut XmlWriter, fill: &FillSpec) {
    w.open("p:bg").start_children();
    w.open("p:bgPr").start_children();
    dml::write_fill(w, Some(fill), "0");
    w.leaf("a:effectLst");
    w.close();
    w.close();
}

fn write_shape_tree(w: &mut XmlWriter, slide: &SlideIr, rels: &mut impl SlideRelSink) {
    w.open("p:spTree").start_children();
    write_group_nv(w, 1, "");
    w.leaf("p:grpSpPr");

    let mut ids = Ids::new();
    for shape in &slide.shapes {
        write_shape(w, shape, &mut ids, rels);
    }

    w.close();
}

fn write_shape(
    w: &mut XmlWriter,
    shape: &SlideShape,
    ids: &mut Ids,
    rels: &mut impl SlideRelSink,
) {
    match shape {
        SlideShape::TextBox(text) => write_text_box(w, text, ids.next(), rels),
        SlideShape::MathBox(math) => write_math_box(w, math, ids.next(), rels),
        SlideShape::TableBox(table) => write_table_box(w, table, ids.next(), rels),
        SlideShape::Pic(pic) => write_pic(w, pic, ids.next(), rels),
        SlideShape::Geom(geom) => write_geom_shape(w, geom, ids.next(), rels),
        SlideShape::Group(group) => write_group_shape(w, group, ids, rels),
        SlideShape::LinkOverlay(link) => write_link_overlay(w, link, ids.next(), rels),
    }
}

fn write_text_box(
    w: &mut XmlWriter,
    text: &TextBox,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:sp").start_children();
    let name = match text.placeholder {
        Some(Placeholder::Title) => format!("Title {id}"),
        Some(Placeholder::Body) => format!("Content Placeholder {id}"),
        Some(Placeholder::SlideNumber) => format!("Slide Number Placeholder {id}"),
        None => format!("TextBox {id}"),
    };
    write_sp_nv(w, id, &name, true, text.placeholder, None, rels);
    w.open("p:spPr").start_children();
    write_xfrm(w, text.x_emu, text.y_emu, text.w_emu, text.h_emu, text.rot_60k);
    dml::write_prst_geom(w, "rect");
    w.leaf("a:noFill");
    w.open("a:ln").start_children();
    w.leaf("a:noFill");
    w.close();
    w.close();

    w.open("p:txBody").start_children();
    write_body_pr(w, text.wrap, text.columns.as_ref());
    w.leaf("a:lstStyle");
    for para in &text.paras {
        write_para(w, para, rels);
    }
    w.close();
    w.close();
}

fn write_math_box(
    w: &mut XmlWriter,
    math: &MathBox,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:sp").start_children();
    write_sp_nv(w, id, &format!("Math {id}"), true, None, None, rels);
    w.open("p:spPr").start_children();
    write_xfrm(w, math.x_emu, math.y_emu, math.w_emu, math.h_emu, math.rot_60k);
    dml::write_prst_geom(w, "rect");
    w.leaf("a:noFill");
    w.open("a:ln").start_children();
    w.leaf("a:noFill");
    w.close();
    w.close();

    w.open("p:txBody").start_children();
    write_body_pr(w, TextWrap::None, None);
    w.leaf("a:lstStyle");
    w.open("a:p").start_children();
    w.open("a:pPr").attr("algn", "l").start_children();
    w.open("a:lnSpc").start_children();
    w.open("a:spcPct").attr("val", "100000").empty();
    w.close();
    w.close();
    w.open("mc:AlternateContent").start_children();
    w.open("mc:Choice").attr("Requires", "a14").start_children();
    w.open("a14:m").start_children();
    w.open("m:oMathPara").start_children();
    w.open("m:oMathParaPr").start_children();
    w.open("m:jc").attr("m:val", "center").empty();
    w.close();
    w.raw(&math.omml);
    w.close();
    w.close();
    w.close();
    w.open("mc:Fallback").start_children();
    write_text_run(w, &math_fallback_run(math), rels);
    w.close();
    w.close();
    w.close();
    w.close();
    w.close();
}

fn write_body_pr(w: &mut XmlWriter, wrap: TextWrap, columns: Option<&TextColumns>) {
    w.open("a:bodyPr")
        .attr("lIns", "0")
        .attr("tIns", "0")
        .attr("rIns", "0")
        .attr("bIns", "0")
        .attr("anchor", "t")
        .attr(
            "wrap",
            match wrap {
                TextWrap::None => "none",
                TextWrap::Square => "square",
            },
        )
        .attr("horzOverflow", "overflow")
        .attr("vertOverflow", "overflow");
    if let Some(columns) = columns {
        w.attr("numCol", &columns.count.max(1).to_string())
            .attr("spcCol", &columns.gutter_emu.max(0).to_string());
    }
    w.start_children();
    w.leaf("a:noAutofit");
    w.close();
}

fn write_para(w: &mut XmlWriter, para: &TextPara, rels: &mut impl SlideRelSink) {
    write_para_aligned(w, para, None, rels);
}

fn write_para_aligned(
    w: &mut XmlWriter,
    para: &TextPara,
    align: Option<CellHAlign>,
    rels: &mut impl SlideRelSink,
) {
    w.open("a:p").start_children();
    let align = match align {
        Some(CellHAlign::Start) => {
            if para.rtl {
                "r"
            } else {
                "l"
            }
        }
        Some(CellHAlign::Left) => "l",
        Some(CellHAlign::Center) => "ctr",
        Some(CellHAlign::Right) => "r",
        Some(CellHAlign::End) => {
            if para.rtl {
                "l"
            } else {
                "r"
            }
        }
        None => {
            if para.rtl {
                "r"
            } else {
                "l"
            }
        }
    };
    w.open("a:pPr").attr("algn", align);
    if para.rtl {
        w.attr("rtl", "1");
    }
    if let Some(bullet) = &para.bullet {
        w.attr("lvl", &bullet.lvl.to_string())
            .attr("marL", &bullet.mar_l_emu.to_string())
            .attr("indent", &bullet.indent_emu.to_string());
    } else {
        if let Some(margin) = para.margin_left_emu {
            w.attr("marL", &margin.to_string());
        }
        if let Some(indent) = para.first_line_indent_emu {
            w.attr("indent", &indent.to_string());
        }
    }
    w.start_children();
    w.open("a:lnSpc").start_children();
    match para.line_spacing_100pt {
        // The measured pitch, absolute: a percentage would compound with the
        // font's own line height and overflow the box.
        Some(pts) => w.open("a:spcPts").attr("val", &pts.to_string()).empty(),
        None => w.open("a:spcPct").attr("val", "100000").empty(),
    }
    w.close();
    if let Some(bullet) = &para.bullet {
        write_bullet(w, bullet);
    }
    w.close();

    for child in &para.children {
        match child {
            TextChild::Run(run) => write_text_run(w, run, rels),
            TextChild::Math(math) => write_inline_math(w, math, rels),
        }
    }
    w.close();
}

fn write_inline_math(w: &mut XmlWriter, math: &InlineMath, rels: &mut impl SlideRelSink) {
    w.open("mc:AlternateContent").start_children();
    w.open("mc:Choice").attr("Requires", "a14").start_children();
    w.open("a14:m").start_children();
    w.raw(&math.omml);
    w.close();
    w.close();
    w.open("mc:Fallback").start_children();
    write_text_run(w, &math.fallback, rels);
    w.close();
    w.close();
}

fn math_fallback_run(math: &MathBox) -> TextRun {
    TextRun {
        text: math.fallback.clone(),
        family: EcoString::from("New Computer Modern Math"),
        sz_100pt: math.fallback_sz_100pt,
        b: false,
        i: false,
        color: [0, 0, 0, 255],
        highlight: None,
        spc_100pt: None,
        field: None,
    }
}

fn write_table_box(
    w: &mut XmlWriter,
    table: &TableBox,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:graphicFrame").start_children();
    w.open("p:nvGraphicFramePr").start_children();
    w.open("p:cNvPr")
        .attr("id", &id.to_string())
        .attr("name", &format!("Table {id}"))
        .empty();
    w.open("p:cNvGraphicFramePr").start_children();
    w.open("a:graphicFrameLocks").attr("noGrp", "1").empty();
    w.close();
    w.leaf("p:nvPr");
    w.close();

    write_graphic_frame_xfrm(w, table.x_emu, table.y_emu, table.w_emu, table.h_emu);

    w.open("a:graphic").start_children();
    w.open("a:graphicData")
        .attr("uri", "http://schemas.openxmlformats.org/drawingml/2006/table")
        .start_children();
    w.open("a:tbl").start_children();

    w.open("a:tblPr")
        .attr("firstRow", "0")
        .attr("bandRow", "0")
        .start_children();
    w.elem_text("a:tableStyleId", "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}");
    w.close();

    w.open("a:tblGrid").start_children();
    for width in &table.cols {
        w.open("a:gridCol").attr("w", &(*width).max(1).to_string()).empty();
    }
    w.close();

    for row in &table.rows {
        w.open("a:tr")
            .attr("h", &row.h_emu.max(1).to_string())
            .start_children();
        for cell in &row.cells {
            write_table_cell(w, cell, rels);
        }
        w.close();
    }

    w.close();
    w.close();
    w.close();
    w.close();
}

fn write_table_cell(w: &mut XmlWriter, cell: &TableCell, rels: &mut impl SlideRelSink) {
    w.open("a:tc");
    if cell.grid_span > 1 {
        w.attr("gridSpan", &cell.grid_span.to_string());
    }
    if cell.row_span > 1 {
        w.attr("rowSpan", &cell.row_span.to_string());
    }
    if cell.h_merge {
        w.attr("hMerge", "1");
    }
    if cell.v_merge {
        w.attr("vMerge", "1");
    }
    w.start_children();

    w.open("a:txBody").start_children();
    write_body_pr(w, TextWrap::Square, None);
    w.leaf("a:lstStyle");
    if cell.paras.is_empty() {
        w.leaf("a:p");
    } else {
        for para in &cell.paras {
            write_para_aligned(w, para, cell.h_align, rels);
        }
    }
    w.close();

    w.open("a:tcPr")
        .attr("marL", &cell.insets.left_emu.max(0).to_string())
        .attr("marT", &cell.insets.top_emu.max(0).to_string())
        .attr("marR", &cell.insets.right_emu.max(0).to_string())
        .attr("marB", &cell.insets.bottom_emu.max(0).to_string());
    if let Some(align) = cell.v_align {
        w.attr(
            "anchor",
            match align {
                CellVAlign::Top => "t",
                CellVAlign::Center => "ctr",
                CellVAlign::Bottom => "b",
            },
        );
    }
    w.start_children();
    // CT_TableCellProperties orders borders before the cell fill. PowerPoint
    // treats a fill-first `tcPr` as corrupt even though more permissive
    // consumers accept it.
    write_cell_border(w, "a:lnL", cell.borders.left.as_ref());
    write_cell_border(w, "a:lnR", cell.borders.right.as_ref());
    write_cell_border(w, "a:lnT", cell.borders.top.as_ref());
    write_cell_border(w, "a:lnB", cell.borders.bottom.as_ref());
    dml::write_fill_with_tile_resolver(w, cell.fill.as_ref(), "0", |media| {
        rels.image_rid(media)
    });
    w.close();
    w.close();
}

fn write_cell_border(w: &mut XmlWriter, name: &'static str, stroke: Option<&StrokeSpec>) {
    match stroke {
        Some(stroke) => {
            w.open(name)
                .attr("w", &stroke.w_emu.max(0).to_string())
                .attr("cap", stroke.cap)
                .start_children();
            dml::write_solid_fill(w, stroke.color);
            dml::write_dash(w, stroke.dash.as_ref());
            w.close();
        }
        None => {
            w.open(name).start_children();
            w.leaf("a:noFill");
            w.close();
        }
    }
}

fn slide_contains_math(slide: &SlideIr) -> bool {
    slide.shapes.iter().any(shape_contains_math)
}

fn shape_contains_math(shape: &SlideShape) -> bool {
    match shape {
        SlideShape::MathBox(_) => true,
        SlideShape::TextBox(text) => paras_contain_math(&text.paras),
        SlideShape::TableBox(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| paras_contain_math(&cell.paras)),
        SlideShape::Group(group) => group.children.iter().any(shape_contains_math),
        SlideShape::Pic(_) | SlideShape::Geom(_) | SlideShape::LinkOverlay(_) => false,
    }
}

fn paras_contain_math(paras: &[TextPara]) -> bool {
    paras
        .iter()
        .any(|para| para.children.iter().any(|child| matches!(child, TextChild::Math(_))))
}

fn write_bullet(w: &mut XmlWriter, bullet: &crate::dom::ParaBullet) {
    match &bullet.kind {
        BulletKind::Char(ch) => {
            w.open("a:buChar").attr("char", ch).empty();
        }
        BulletKind::AutoNum { ty, start_at } => {
            w.open("a:buAutoNum")
                .attr("type", ty)
                .attr("startAt", &start_at.to_string())
                .empty();
        }
    }
}

fn write_text_run(w: &mut XmlWriter, run: &TextRun, _rels: &mut impl SlideRelSink) {
    if matches!(run.field, Some(TextField::SlideNumber)) {
        write_slide_number_field(w, run);
        return;
    }

    let mut first = true;
    for part in run.text.split('\n') {
        if !first {
            w.leaf("a:br");
        }
        first = false;
        if part.is_empty() {
            continue;
        }
        w.open("a:r").start_children();
        write_r_pr(w, run);
        w.elem_text("a:t", part);
        w.close();
    }
}

fn write_slide_number_field(w: &mut XmlWriter, run: &TextRun) {
    if run.text.is_empty() {
        return;
    }
    w.open("a:fld")
        .attr("id", &field_id(run))
        .attr("type", "slidenum")
        .start_children();
    write_r_pr(w, run);
    w.elem_text("a:t", &run.text);
    w.close();
}

fn field_id(run: &TextRun) -> String {
    let hash = typst_utils::hash128(&(
        "typst-pptx-slidenum",
        run.text.as_str(),
        run.family.as_str(),
        run.sz_100pt,
        run.b,
        run.i,
        run.color,
        run.spc_100pt,
    ));
    xml::guid_from_hash(hash)
}

fn write_r_pr(w: &mut XmlWriter, run: &TextRun) {
    w.open("a:rPr")
        .attr("lang", "en-US")
        .attr("sz", &run.sz_100pt.to_string());
    if run.b {
        w.attr("b", "1");
    }
    if run.i {
        w.attr("i", "1");
    }
    if let Some(spc) = run.spc_100pt {
        w.attr("spc", &spc.to_string());
    }
    w.start_children();
    dml::write_solid_fill(w, run.color);
    if let Some(color) = run.highlight {
        w.open("a:highlight").start_children();
        dml::write_srgb(w, color);
        w.close();
    }
    w.open("a:latin").attr("typeface", &run.family).empty();
    w.open("a:ea").attr("typeface", &run.family).empty();
    w.open("a:cs").attr("typeface", &run.family).empty();
    w.close();
}

fn write_link_overlay(
    w: &mut XmlWriter,
    overlay: &LinkOverlay,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:sp").start_children();
    write_sp_nv(
        w,
        id,
        &format!("Hyperlink {id}"),
        false,
        None,
        Some(&overlay.link),
        rels,
    );
    w.open("p:spPr").start_children();
    write_xfrm(w, overlay.x_emu, overlay.y_emu, overlay.w_emu, overlay.h_emu, 0);
    write_geom(w, &PathGeom::Rect, overlay.w_emu, overlay.h_emu);
    dml::write_fill(w, Some(&FillSpec::Solid([255, 255, 255, 0])), "0");
    dml::write_stroke(w, None, true);
    w.close();
    w.close();
}

fn write_pic(w: &mut XmlWriter, pic: &Pic, id: u32, rels: &mut impl SlideRelSink) {
    let rid = rels.image_rid(pic.media);
    let svg_rid = pic.svg_media.map(|media| rels.image_rid(media));
    w.open("p:pic").start_children();
    w.open("p:nvPicPr").start_children();
    w.open("p:cNvPr")
        .attr("id", &id.to_string())
        .attr("name", &format!("Picture {id}"));
    if let Some(alt) = &pic.alt {
        w.attr("descr", alt);
    }
    if let Some(link) = &pic.link {
        w.start_children();
        write_hlink_click(w, link, rels);
        w.close();
    } else {
        w.empty();
    }
    w.open("p:cNvPicPr").start_children();
    w.open("a:picLocks").attr("noChangeAspect", "1").empty();
    w.close();
    w.leaf("p:nvPr");
    w.close();

    w.open("p:blipFill").start_children();
    dml::write_blip(w, &rid, svg_rid.as_deref());
    if let Some([l, t, r, b]) = pic.src_rect {
        w.open("a:srcRect")
            .attr("l", &l.to_string())
            .attr("t", &t.to_string())
            .attr("r", &r.to_string())
            .attr("b", &b.to_string())
            .empty();
    }
    w.open("a:stretch").start_children();
    w.leaf("a:fillRect");
    w.close();
    w.close();

    w.open("p:spPr").start_children();
    write_xfrm(w, pic.x_emu, pic.y_emu, pic.w_emu, pic.h_emu, pic.rot_60k);
    write_pic_geom(w, &pic.geom);
    w.close();
    w.close();
}

fn write_pic_geom(w: &mut XmlWriter, geom: &PicGeom) {
    match geom {
        PicGeom::Rect => dml::write_prst_geom(w, "rect"),
        PicGeom::RoundRect { adj_100k } => {
            dml::write_prst_geom_with_adj(w, "roundRect", *adj_100k)
        }
        PicGeom::Ellipse => dml::write_prst_geom(w, "ellipse"),
    }
}

fn write_geom_shape(
    w: &mut XmlWriter,
    geom: &GeomShape,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    match &geom.geom {
        GeomKind::Path(path) => {
            w.open("p:sp").start_children();
            write_sp_nv(
                w,
                id,
                &format!("Shape {id}"),
                false,
                None,
                geom.link.as_ref(),
                rels,
            );
            w.open("p:spPr").start_children();
            write_xfrm(w, geom.x_emu, geom.y_emu, geom.w_emu, geom.h_emu, geom.rot_60k);
            write_geom(w, path, geom.w_emu, geom.h_emu);
            dml::write_fill_with_tile_resolver(w, geom.fill.as_ref(), "0", |media| {
                rels.image_rid(media)
            });
            dml::write_stroke(w, geom.stroke.as_ref(), true);
            w.close();
            w.close();
        }
        GeomKind::Connector { flip_h, flip_v } => {
            write_connector_shape(w, geom, id, *flip_h, *flip_v, rels);
        }
    }
}

fn write_connector_shape(
    w: &mut XmlWriter,
    geom: &GeomShape,
    id: u32,
    flip_h: bool,
    flip_v: bool,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:cxnSp").start_children();
    write_cxn_nv(w, id, &format!("Connector {id}"), geom.link.as_ref(), rels);
    w.open("p:spPr").start_children();
    write_xfrm_with_flips(
        w,
        geom.x_emu,
        geom.y_emu,
        geom.w_emu,
        geom.h_emu,
        geom.rot_60k,
        Some((flip_h, flip_v)),
    );
    dml::write_prst_geom(w, "line");
    dml::write_stroke(w, geom.stroke.as_ref(), true);
    w.close();
    w.close();
}

fn write_group_shape(
    w: &mut XmlWriter,
    group: &GroupShape,
    ids: &mut Ids,
    rels: &mut impl SlideRelSink,
) {
    let id = ids.next();
    w.open("p:grpSp").start_children();
    write_group_nv(w, id, &format!("Group {id}"));
    w.open("p:grpSpPr").start_children();
    w.open("a:xfrm");
    if group.rot_60k != 0 {
        w.attr("rot", &group.rot_60k.to_string());
    }
    w.start_children();
    w.open("a:off")
        .attr("x", &group.x_emu.to_string())
        .attr("y", &group.y_emu.to_string())
        .empty();
    w.open("a:ext")
        .attr("cx", &group.w_emu.to_string())
        .attr("cy", &group.h_emu.to_string())
        .empty();
    w.open("a:chOff").attr("x", "0").attr("y", "0").empty();
    w.open("a:chExt")
        .attr("cx", &group.w_emu.to_string())
        .attr("cy", &group.h_emu.to_string())
        .empty();
    w.close();
    w.close();
    for child in &group.children {
        write_shape(w, child, ids, rels);
    }
    w.close();
}

fn write_sp_nv(
    w: &mut XmlWriter,
    id: u32,
    name: &str,
    text_box: bool,
    placeholder: Option<Placeholder>,
    link: Option<&RunLink>,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:nvSpPr").start_children();
    w.open("p:cNvPr").attr("id", &id.to_string()).attr("name", name);
    if let Some(link) = link {
        w.start_children();
        write_hlink_click(w, link, rels);
        w.close();
    } else {
        w.empty();
    }
    w.open("p:cNvSpPr");
    if text_box && placeholder.is_none() {
        w.attr("txBox", "1");
    }
    if placeholder.is_some() {
        w.start_children();
        w.open("a:spLocks").attr("noGrp", "1").empty();
        w.close();
    } else {
        w.empty();
    }
    if let Some(placeholder) = placeholder {
        w.open("p:nvPr").start_children();
        match placeholder {
            Placeholder::Title => {
                w.open("p:ph").attr("type", "title").attr("idx", "0").empty();
            }
            Placeholder::Body => {
                w.open("p:ph").attr("type", "body").attr("idx", "1").empty();
            }
            Placeholder::SlideNumber => {
                w.open("p:ph").attr("type", "sldNum").attr("idx", "10").empty();
            }
        }
        w.close();
    } else {
        w.leaf("p:nvPr");
    }
    w.close();
}

fn write_group_nv(w: &mut XmlWriter, id: u32, name: &str) {
    w.open("p:nvGrpSpPr").start_children();
    w.open("p:cNvPr")
        .attr("id", &id.to_string())
        .attr("name", name)
        .empty();
    w.leaf("p:cNvGrpSpPr");
    w.leaf("p:nvPr");
    w.close();
}

fn write_cxn_nv(
    w: &mut XmlWriter,
    id: u32,
    name: &str,
    link: Option<&RunLink>,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:nvCxnSpPr").start_children();
    w.open("p:cNvPr").attr("id", &id.to_string()).attr("name", name);
    if let Some(link) = link {
        w.start_children();
        write_hlink_click(w, link, rels);
        w.close();
    } else {
        w.empty();
    }
    w.leaf("p:cNvCxnSpPr");
    w.leaf("p:nvPr");
    w.close();
}

fn write_hlink_click(w: &mut XmlWriter, link: &RunLink, rels: &mut impl SlideRelSink) {
    let rid = match link {
        RunLink::Url(url) => rels.hyperlink_rid(url),
        RunLink::Slide(slide) => rels.slide_rid(*slide),
    };
    w.open("a:hlinkClick").attr("r:id", &rid).attr("tooltip", "Open link");
    if matches!(link, RunLink::Slide(_)) {
        w.attr("action", "ppaction://hlinksldjump");
    }
    w.empty();
}

fn write_xfrm(w: &mut XmlWriter, x: i64, y: i64, cx: i64, cy: i64, rot_60k: i32) {
    write_xfrm_with_flips(w, x, y, cx, cy, rot_60k, None);
}

/// Writes the transform for a PresentationML graphic frame.
///
/// Graphic frames use `p:xfrm`, unlike shapes and pictures whose transforms
/// are DrawingML `a:xfrm` elements.
fn write_graphic_frame_xfrm(w: &mut XmlWriter, x: i64, y: i64, cx: i64, cy: i64) {
    w.open("p:xfrm").start_children();
    write_xfrm_body(w, x, y, cx, cy);
    w.close();
}

fn write_xfrm_with_flips(
    w: &mut XmlWriter,
    x: i64,
    y: i64,
    cx: i64,
    cy: i64,
    rot_60k: i32,
    flips: Option<(bool, bool)>,
) {
    w.open("a:xfrm");
    if rot_60k != 0 {
        w.attr("rot", &rot_60k.to_string());
    }
    let (flip_h, flip_v) = flips.unwrap_or((false, false));
    if flip_h {
        w.attr("flipH", "1");
    }
    if flip_v {
        w.attr("flipV", "1");
    }
    w.start_children();
    write_xfrm_body(w, x, y, cx, cy);
    w.close();
}

fn write_xfrm_body(w: &mut XmlWriter, x: i64, y: i64, cx: i64, cy: i64) {
    w.open("a:off")
        .attr("x", &x.to_string())
        .attr("y", &y.to_string())
        .empty();
    w.open("a:ext")
        .attr("cx", &cx.max(1).to_string())
        .attr("cy", &cy.max(1).to_string())
        .empty();
}

fn write_geom(w: &mut XmlWriter, geom: &PathGeom, w_emu: i64, h_emu: i64) {
    match geom {
        PathGeom::Rect => dml::write_prst_geom(w, "rect"),
        PathGeom::Custom(segments) => {
            dml::write_custom_geom(w, segments, w_emu.max(1), h_emu.max(1))
        }
    }
}

struct Ids {
    next: u32,
}

impl Ids {
    fn new() -> Self {
        Self { next: 2 }
    }

    fn next(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }
}

use ecow::EcoString;
use typst_ooxml_core::color as ooxml_color;
use typst_ooxml_core::ns;

use crate::dom::{
    BulletKind, FillSpec, GeomShape, GroupShape, MathBox, MediaId, PathGeom, PathSegment,
    Pic, PicGeom, Placeholder, RunLink, SlideIr, SlideShape, StrokeSpec, TextBox,
    TextPara, TextRun, TextWrap,
};
use crate::xml::XmlWriter;

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
    write_fill(w, Some(fill));
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
        SlideShape::Pic(pic) => write_pic(w, pic, ids.next(), rels),
        SlideShape::Geom(geom) => write_geom_shape(w, geom, ids.next()),
        SlideShape::Group(group) => write_group_shape(w, group, ids, rels),
    }
}

fn write_text_box(
    w: &mut XmlWriter,
    text: &TextBox,
    id: u32,
    rels: &mut impl SlideRelSink,
) {
    w.open("p:sp").start_children();
    let is_title = matches!(text.placeholder, Some(Placeholder::Title));
    let name = if is_title { format!("Title {id}") } else { format!("TextBox {id}") };
    write_sp_nv(w, id, &name, !is_title, text.placeholder);
    w.open("p:spPr").start_children();
    write_xfrm(w, text.x_emu, text.y_emu, text.w_emu, text.h_emu, text.rot_60k);
    write_prst_geom(w, "rect");
    w.leaf("a:noFill");
    w.open("a:ln").start_children();
    w.leaf("a:noFill");
    w.close();
    w.close();

    w.open("p:txBody").start_children();
    write_body_pr(w, text.wrap);
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
    write_sp_nv(w, id, &format!("Math {id}"), true, None);
    w.open("p:spPr").start_children();
    write_xfrm(w, math.x_emu, math.y_emu, math.w_emu, math.h_emu, math.rot_60k);
    write_prst_geom(w, "rect");
    w.leaf("a:noFill");
    w.open("a:ln").start_children();
    w.leaf("a:noFill");
    w.close();
    w.close();

    w.open("p:txBody").start_children();
    write_body_pr(w, TextWrap::None);
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

fn write_body_pr(w: &mut XmlWriter, wrap: TextWrap) {
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
        .attr("vertOverflow", "overflow")
        .start_children();
    w.leaf("a:noAutofit");
    w.close();
}

fn write_para(w: &mut XmlWriter, para: &TextPara, rels: &mut impl SlideRelSink) {
    w.open("a:p").start_children();
    w.open("a:pPr").attr("algn", if para.rtl { "r" } else { "l" });
    if para.rtl {
        w.attr("rtl", "1");
    }
    if let Some(bullet) = &para.bullet {
        w.attr("lvl", &bullet.lvl.to_string())
            .attr("marL", &bullet.mar_l_emu.to_string())
            .attr("indent", &bullet.indent_emu.to_string());
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

    for run in &para.runs {
        write_text_run(w, run, rels);
    }
    w.close();
}

fn math_fallback_run(math: &MathBox) -> TextRun {
    TextRun {
        text: math.fallback.clone(),
        family: EcoString::from("New Computer Modern Math"),
        sz_100pt: 1800,
        b: false,
        i: false,
        color: [0, 0, 0, 255],
        spc_100pt: None,
        link: None,
    }
}

fn slide_contains_math(slide: &SlideIr) -> bool {
    slide.shapes.iter().any(shape_contains_math)
}

fn shape_contains_math(shape: &SlideShape) -> bool {
    match shape {
        SlideShape::MathBox(_) => true,
        SlideShape::Group(group) => group.children.iter().any(shape_contains_math),
        SlideShape::TextBox(_) | SlideShape::Pic(_) | SlideShape::Geom(_) => false,
    }
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

fn write_text_run(w: &mut XmlWriter, run: &TextRun, rels: &mut impl SlideRelSink) {
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
        write_r_pr(w, run, rels);
        w.elem_text("a:t", part);
        w.close();
    }
}

fn write_r_pr(w: &mut XmlWriter, run: &TextRun, rels: &mut impl SlideRelSink) {
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
    write_solid_fill(w, run.color);
    w.open("a:latin").attr("typeface", &run.family).empty();
    w.open("a:ea").attr("typeface", &run.family).empty();
    w.open("a:cs").attr("typeface", &run.family).empty();
    if let Some(link) = &run.link {
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
    w.close();
}

fn write_pic(w: &mut XmlWriter, pic: &Pic, id: u32, rels: &mut impl SlideRelSink) {
    let rid = rels.image_rid(pic.media);
    w.open("p:pic").start_children();
    w.open("p:nvPicPr").start_children();
    w.open("p:cNvPr")
        .attr("id", &id.to_string())
        .attr("name", &format!("Picture {id}"));
    if let Some(alt) = &pic.alt {
        w.attr("descr", alt);
    }
    w.empty();
    w.open("p:cNvPicPr").start_children();
    w.open("a:picLocks").attr("noChangeAspect", "1").empty();
    w.close();
    w.leaf("p:nvPr");
    w.close();

    w.open("p:blipFill").start_children();
    w.open("a:blip").attr("r:embed", &rid).empty();
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
        PicGeom::Rect => write_prst_geom(w, "rect"),
        PicGeom::RoundRect { adj_100k } => {
            write_prst_geom_with_adj(w, "roundRect", *adj_100k)
        }
        PicGeom::Ellipse => write_prst_geom(w, "ellipse"),
    }
}

fn write_geom_shape(w: &mut XmlWriter, geom: &GeomShape, id: u32) {
    w.open("p:sp").start_children();
    write_sp_nv(w, id, &format!("Shape {id}"), false, None);
    w.open("p:spPr").start_children();
    write_xfrm(w, geom.x_emu, geom.y_emu, geom.w_emu, geom.h_emu, geom.rot_60k);
    write_geom(w, &geom.geom, geom.w_emu, geom.h_emu);
    write_fill(w, geom.fill.as_ref());
    write_stroke(w, geom.stroke.as_ref());
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
) {
    w.open("p:nvSpPr").start_children();
    w.open("p:cNvPr")
        .attr("id", &id.to_string())
        .attr("name", name)
        .empty();
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

fn write_xfrm(w: &mut XmlWriter, x: i64, y: i64, cx: i64, cy: i64, rot_60k: i32) {
    w.open("a:xfrm");
    if rot_60k != 0 {
        w.attr("rot", &rot_60k.to_string());
    }
    w.start_children();
    w.open("a:off")
        .attr("x", &x.to_string())
        .attr("y", &y.to_string())
        .empty();
    w.open("a:ext")
        .attr("cx", &cx.max(1).to_string())
        .attr("cy", &cy.max(1).to_string())
        .empty();
    w.close();
}

fn write_geom(w: &mut XmlWriter, geom: &PathGeom, w_emu: i64, h_emu: i64) {
    match geom {
        PathGeom::Rect => write_prst_geom(w, "rect"),
        PathGeom::Ellipse => write_prst_geom(w, "ellipse"),
        PathGeom::Custom(segments) => write_custom_geom(w, segments, w_emu, h_emu),
    }
}

fn write_prst_geom(w: &mut XmlWriter, prst: &'static str) {
    w.open("a:prstGeom").attr("prst", prst).start_children();
    w.leaf("a:avLst");
    w.close();
}

fn write_prst_geom_with_adj(w: &mut XmlWriter, prst: &'static str, adj: i32) {
    w.open("a:prstGeom").attr("prst", prst).start_children();
    w.open("a:avLst").start_children();
    w.open("a:gd")
        .attr("name", "adj")
        .attr("fmla", &format!("val {}", adj.clamp(0, 50_000)))
        .empty();
    w.close();
    w.close();
}

fn write_custom_geom(
    w: &mut XmlWriter,
    segments: &[PathSegment],
    w_emu: i64,
    h_emu: i64,
) {
    w.open("a:custGeom").start_children();
    w.leaf("a:avLst");
    w.leaf("a:gdLst");
    w.leaf("a:ahLst");
    w.leaf("a:cxnLst");
    // The text rectangle in LITERAL coordinates. `r="r" b="b"` reference
    // guide names that must be defined in `<a:gdLst>` — with an empty gdLst
    // they are undefined, which PowerPoint *repairs* (LibreOffice tolerates
    // it). Our path space equals the extent, so the rect is the full box.
    w.open("a:rect")
        .attr("l", "0")
        .attr("t", "0")
        .attr("r", &w_emu.max(1).to_string())
        .attr("b", &h_emu.max(1).to_string())
        .empty();
    w.open("a:pathLst").start_children();
    // The path's own coordinate space. Without explicit w/h a consumer cannot
    // normalize the (EMU-valued) points against the shape extent and stretches
    // the path arbitrarily — LibreOffice blew a 120pt rect up to slide width.
    // Our points already live in [0, ext], so the space equals the extent.
    w.open("a:path")
        .attr("w", &w_emu.max(1).to_string())
        .attr("h", &h_emu.max(1).to_string())
        .start_children();
    for segment in segments {
        match *segment {
            PathSegment::MoveTo(x, y) => {
                w.open("a:moveTo").start_children();
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::LineTo(x, y) => {
                w.open("a:lnTo").start_children();
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::CubicTo(x1, y1, x2, y2, x, y) => {
                w.open("a:cubicBezTo").start_children();
                write_pt(w, x1, y1);
                write_pt(w, x2, y2);
                write_pt(w, x, y);
                w.close();
            }
            PathSegment::Close => w.leaf("a:close"),
        }
    }
    w.close();
    w.close();
    w.close();
}

fn write_pt(w: &mut XmlWriter, x: i64, y: i64) {
    w.open("a:pt")
        .attr("x", &x.to_string())
        .attr("y", &y.to_string())
        .empty();
}

fn write_fill(w: &mut XmlWriter, fill: Option<&FillSpec>) {
    match fill {
        Some(FillSpec::Solid(rgb)) => write_solid_fill(w, *rgb),
        Some(FillSpec::LinearGradient { angle_60k, stops }) => {
            w.open("a:gradFill").attr("rotWithShape", "1").start_children();
            w.open("a:gsLst").start_children();
            for stop in stops {
                w.open("a:gs")
                    .attr("pos", &stop.pos_100k.to_string())
                    .start_children();
                // CT_GradientStop holds the color element DIRECTLY — wrapping
                // it in a:solidFill is schema-invalid and consumers drop the
                // whole fill (the shape rendered invisible in LibreOffice).
                write_srgb(w, stop.color);
                w.close();
            }
            w.close();
            w.open("a:lin")
                .attr("ang", &angle_60k.to_string())
                .attr("scaled", "0")
                .empty();
            w.close();
        }
        None => w.leaf("a:noFill"),
    }
}

fn write_stroke(w: &mut XmlWriter, stroke: Option<&StrokeSpec>) {
    match stroke {
        Some(stroke) => {
            w.open("a:ln")
                .attr("w", &stroke.w_emu.max(0).to_string())
                .attr("cap", stroke.cap)
                .start_children();
            write_solid_fill(w, stroke.color);
            if let Some(dash) = stroke.dash {
                w.open("a:prstDash").attr("val", dash).empty();
            }
            w.close();
        }
        None => {
            w.open("a:ln").start_children();
            w.leaf("a:noFill");
            w.close();
        }
    }
}

fn write_solid_fill(w: &mut XmlWriter, rgba: [u8; 4]) {
    w.open("a:solidFill").start_children();
    write_srgb(w, rgba);
    w.close();
}

fn write_srgb(w: &mut XmlWriter, rgba: [u8; 4]) {
    let [r, g, b, a] = rgba;
    if a == 255 {
        w.open("a:srgbClr").attr("val", &hex([r, g, b])).empty();
    } else {
        // Straight alpha as a percentage in thousandths (DrawingML CT_Color).
        w.open("a:srgbClr").attr("val", &hex([r, g, b])).start_children();
        w.open("a:alpha")
            .attr("val", &ooxml_color::alpha_to_100k(a).to_string())
            .empty();
        w.close();
    }
}

pub fn hex(rgb: [u8; 3]) -> String {
    ooxml_color::hex_rgb(rgb)
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

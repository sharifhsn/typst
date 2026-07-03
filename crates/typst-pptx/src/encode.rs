use crate::dom::SlideIr;
use crate::xml::XmlWriter;

/// Build a slide XML part.
pub fn slide_xml(slide: &SlideIr) -> String {
    let mut w = XmlWriter::new(false);
    w.open("p:sld")
        .attr("xmlns:a", "http://schemas.openxmlformats.org/drawingml/2006/main")
        .attr("xmlns:p", "http://schemas.openxmlformats.org/presentationml/2006/main")
        .attr(
            "xmlns:r",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        )
        .start_children();

    w.open("p:cSld").start_children();
    if let Some(rgb) = slide.bg {
        write_background(&mut w, rgb);
    }
    write_empty_shape_tree(&mut w);
    w.close();

    w.open("p:clrMapOvr").start_children();
    w.leaf("a:masterClrMapping");
    w.close();
    w.close();
    w.finish()
}

fn write_background(w: &mut XmlWriter, rgb: [u8; 3]) {
    w.open("p:bg").start_children();
    w.open("p:bgPr").start_children();
    write_solid_fill(w, rgb);
    w.leaf("a:effectLst");
    w.close();
    w.close();
}

pub fn write_empty_shape_tree(w: &mut XmlWriter) {
    w.open("p:spTree").start_children();
    w.open("p:nvGrpSpPr").start_children();
    w.open("p:cNvPr").attr("id", "1").attr("name", "").empty();
    w.leaf("p:cNvGrpSpPr");
    w.leaf("p:nvPr");
    w.close();
    w.leaf("p:grpSpPr");
    w.close();
}

fn write_solid_fill(w: &mut XmlWriter, rgb: [u8; 3]) {
    w.open("a:solidFill").start_children();
    w.open("a:srgbClr").attr("val", &hex(rgb)).empty();
    w.close();
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

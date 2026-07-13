//! Structural and well-formedness tests for the PPTX exporter foundation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use typst_pptx::{PptxOptions, pptx};

/// A minimal world: the embedded Typst fonts and a single detached source.
struct TestWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: Source,
}

impl TestWorld {
    fn new(text: &str) -> Self {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let book = FontBook::from_fonts(&fonts);
        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            main: Source::detached(text),
        }
    }
}

impl World for TestWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }
    fn main(&self) -> FileId {
        self.main.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(Default::default()))
        }
    }
    fn file(&self, _: FileId) -> FileResult<Bytes> {
        Err(FileError::NotFound(Default::default()))
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }
    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}

fn pptx_bytes(src: &str) -> Vec<u8> {
    let world = TestWorld::new(src);
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .expect("compilation failed");
    pptx(&doc, &PptxOptions::default()).expect("pptx export failed")
}

/// Compiles `src` to a PPTX and returns all package parts as `name -> bytes`.
fn binary_parts(src: &str) -> HashMap<String, Vec<u8>> {
    let bytes = pptx_bytes(src);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut map = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut bytes = vec![];
        f.read_to_end(&mut bytes).unwrap();
        map.insert(name, bytes);
    }
    map
}

/// Compiles `src` to a PPTX and returns text parts as `name -> text`.
fn parts(src: &str) -> HashMap<String, String> {
    let bytes = pptx_bytes(src);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut map = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() {
            map.insert(name, s);
        }
    }
    map
}

fn text_parts_from_binary(parts: &HashMap<String, Vec<u8>>) -> HashMap<String, String> {
    parts
        .iter()
        .filter_map(|(name, bytes)| {
            std::str::from_utf8(bytes)
                .ok()
                .map(|text| (name.clone(), text.into()))
        })
        .collect()
}

/// Parses every XML part with the namespace-aware parser.
fn assert_all_wellformed(parts: &HashMap<String, String>) {
    let mut names: Vec<&String> = parts.keys().collect();
    names.sort();
    for name in names {
        let xml = &parts[name];
        if name.ends_with(".xml") || name.ends_with(".rels") {
            roxmltree::Document::parse(xml)
                .unwrap_or_else(|e| panic!("{name} is not namespace-well-formed: {e}"));
        }
    }
}

fn slide_count(presentation: &str) -> usize {
    let doc =
        roxmltree::Document::parse(presentation).expect("presentation should parse");
    doc.descendants()
        .filter(|node| {
            node.tag_name().name() == "sldId"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/presentationml/2006/main")
        })
        .count()
}

fn count_xml_nodes(xml: &str, local: &str) -> usize {
    let doc = roxmltree::Document::parse(xml).expect("xml should parse");
    doc.descendants()
        .filter(|node| node.tag_name().name() == local)
        .count()
}

#[test]
fn required_parts_are_present_and_wellformed() {
    let p = parts("= Hello\nSome text.");
    for name in [
        "[Content_Types].xml",
        "_rels/.rels",
        "docProps/core.xml",
        "docProps/app.xml",
        "ppt/presentation.xml",
        "ppt/_rels/presentation.xml.rels",
        "ppt/presProps.xml",
        "ppt/viewProps.xml",
        "ppt/tableStyles.xml",
        "ppt/theme/theme1.xml",
        "ppt/slideMasters/slideMaster1.xml",
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        "ppt/slideLayouts/slideLayout1.xml",
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        "ppt/slides/slide1.xml",
        "ppt/slides/_rels/slide1.xml.rels",
    ] {
        assert!(p.contains_key(name), "missing {name}");
    }
    assert_all_wellformed(&p);
}

#[test]
fn slide_count_matches_page_count() {
    let p = parts("one #pagebreak() two #pagebreak() three");
    assert_eq!(slide_count(&p["ppt/presentation.xml"]), 3);
    assert!(p.contains_key("ppt/slides/slide3.xml"));
    assert!(p.contains_key("ppt/slides/_rels/slide3.xml.rels"));
    assert_all_wellformed(&p);
}

#[test]
fn pdfpc_metadata_exports_speaker_notes() {
    // The real `<pdfpc-file>` value is a dict with a `pages` array (the exact
    // shape `typst query --field value --one "<pdfpc-file>"` emits from touying
    // `#note(..)`), NOT a bare array — regression guard for that.
    let p = parts(
        r#"#set page(width: 200pt, height: 100pt)
#metadata((
  pdfpcFormat: 2,
  disableMarkdown: false,
  pages: (
    (idx: 1, note: "First slide note\nsecond line"),
    (idx: 2, note: "Second <note> & more"),
  ),
)) <pdfpc-file>
Slide one
#pagebreak()
Slide two"#,
    );

    for name in [
        "ppt/notesMasters/notesMaster1.xml",
        "ppt/notesMasters/_rels/notesMaster1.xml.rels",
        "ppt/notesSlides/notesSlide1.xml",
        "ppt/notesSlides/_rels/notesSlide1.xml.rels",
        "ppt/notesSlides/notesSlide2.xml",
        "ppt/notesSlides/_rels/notesSlide2.xml.rels",
    ] {
        assert!(p.contains_key(name), "missing {name}");
    }

    let content_types = &p["[Content_Types].xml"];
    assert!(content_types.contains("/ppt/notesMasters/notesMaster1.xml"));
    assert!(content_types.contains("/ppt/notesSlides/notesSlide1.xml"));
    assert!(content_types.contains("/ppt/notesSlides/notesSlide2.xml"));

    let presentation_rels = &p["ppt/_rels/presentation.xml.rels"];
    assert!(presentation_rels.contains("notesMasters/notesMaster1.xml"));

    let slide1_rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    assert!(slide1_rels.contains("../notesSlides/notesSlide1.xml"));
    let slide2_rels = &p["ppt/slides/_rels/slide2.xml.rels"];
    assert!(slide2_rels.contains("../notesSlides/notesSlide2.xml"));

    let notes1 = &p["ppt/notesSlides/notesSlide1.xml"];
    assert!(notes1.contains("<p:ph type=\"body\""));
    assert!(notes1.contains("<a:t>First slide note</a:t>"));
    assert!(notes1.contains("<a:t>second line</a:t>"));

    let notes2 = &p["ppt/notesSlides/notesSlide2.xml"];
    assert!(notes2.contains("<a:t>Second &lt;note&gt; &amp; more</a:t>"));

    let notes1_rels = &p["ppt/notesSlides/_rels/notesSlide1.xml.rels"];
    assert!(notes1_rels.contains("../notesMasters/notesMaster1.xml"));
    assert_all_wellformed(&p);
}

#[test]
fn slide_size_uses_first_page_size() {
    let p = parts("#set page(width: 200pt, height: 100pt)\nHello");
    let doc = roxmltree::Document::parse(&p["ppt/presentation.xml"]).unwrap();
    let sld_sz = doc
        .descendants()
        .find(|node| node.tag_name().name() == "sldSz")
        .expect("missing p:sldSz");
    assert_eq!(sld_sz.attribute("cx"), Some("2540000"));
    assert_eq!(sld_sz.attribute("cy"), Some("1270000"));
    assert_all_wellformed(&p);
}

#[test]
fn export_is_deterministic() {
    let a = pptx_bytes("= Hello\nSome text.");
    let b = pptx_bytes("= Hello\nSome text.");
    assert_eq!(a, b);
}

#[test]
fn page_fill_becomes_solid_slide_background() {
    let p = parts("#set page(fill: aqua)\nHello");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<p:bg>"), "slide should carry a background");
    assert!(slide.contains("val=\"7FDBFF\""), "aqua background should be sRGB");
    assert_all_wellformed(&p);
}

#[test]
fn table_exports_as_native_drawingml_table() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt)
#table(columns: 3, [a], [b], [c], [d], [e], [f])"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<p:graphicFrame>"));
    assert!(slide.contains("<a:tbl>"));
    assert_eq!(count_xml_nodes(slide, "gridCol"), 3);
    assert_eq!(count_xml_nodes(slide, "tr"), 2);
    assert_eq!(count_xml_nodes(slide, "tc"), 6);
    for text in ["a", "b", "c", "d", "e", "f"] {
        assert!(slide.contains(&format!("<a:t>{text}</a:t>")), "missing {text}");
    }
    assert_all_wellformed(&p);
}

#[test]
fn table_colspan_and_rowspan_emit_merge_attrs() {
    let p = parts(
        r#"#set page(width: 360pt, height: 200pt)
#table(
  columns: 3,
  table.cell(colspan: 2)[wide], [c],
  table.cell(rowspan: 2)[tall], [e], [f],
  [h], [i],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:tbl>"));
    assert!(slide.contains("gridSpan=\"2\""));
    assert!(slide.contains("rowSpan=\"2\""));
    assert!(slide.contains("vMerge=\"1\""));
    for text in ["wide", "tall", "c", "e", "f", "h", "i"] {
        assert!(slide.contains(&format!("<a:t>{text}</a:t>")), "missing {text}");
    }
    assert_all_wellformed(&p);
}

#[test]
fn table_cell_alignment_is_preserved() {
    let p = parts(
        r#"#set page(width: 360pt, height: 200pt)
#table(
  columns: 3,
  align: (left + top, center + horizon, right + bottom),
  [left], [center], [right],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("algn=\"l\""), "left cell paragraph alignment");
    assert!(slide.contains("algn=\"ctr\""), "center cell paragraph alignment");
    assert!(slide.contains("algn=\"r\""), "right cell paragraph alignment");
    assert!(slide.contains("anchor=\"t\""), "top cell vertical alignment");
    assert!(slide.contains("anchor=\"ctr\""), "center cell vertical alignment");
    assert!(slide.contains("anchor=\"b\""), "bottom cell vertical alignment");
    assert_all_wellformed(&p);
}

#[test]
fn gradient_page_fill_becomes_gradient_slide_background() {
    let p = parts(
        "#set page(fill: gradient.linear(rgb(\"#1A1A2E\"), rgb(\"#16213E\")))\nHello",
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let bg = slide.split("<p:bg>").nth(1).unwrap().split("</p:bg>").next().unwrap();
    assert!(bg.contains("<a:gradFill"), "gradient page fill should emit a:gradFill");
    assert!(bg.contains("val=\"1A1A2E\""), "first gradient stop color");
    assert!(bg.contains("val=\"16213E\""), "last gradient stop color");
    // The stop color must sit directly in the gs, never wrapped in solidFill.
    assert!(!bg.contains("<a:solidFill>"), "gradient stops must not wrap solidFill");
    assert_all_wellformed(&p);
}

#[test]
fn text_run_emits_family_size_and_color() {
    let p = parts(
        r##"#set text(font: "New Computer Modern", size: 20pt, fill: rgb("#123456"))
Hello"##,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Hello</a:t>"), "text should be live DrawingML");
    assert!(slide.contains("typeface=\"New Computer Modern\""));
    assert!(slide.contains("sz=\"2000\""));
    assert!(slide.contains("val=\"123456\""));
    assert_all_wellformed(&p);
}

#[test]
fn text_highlight_is_native_and_does_not_cover_the_text() {
    let p = parts("Before #highlight(fill: yellow)[Highlighted] after.");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:highlight>"), "highlight is a native run property");
    assert!(slide.contains("<a:t>Highlighted</a:t>"), "highlighted text is its own run");
    assert!(slide.contains("val=\"FFDC00\""), "the Typst yellow is preserved");
    assert_all_wellformed(&p);

    let card = parts("#rect(width: 100pt, height: 30pt, fill: yellow)[Card text]");
    let card_slide = &card["ppt/slides/slide1.xml"];
    assert!(
        !card_slide.contains("<a:highlight>"),
        "a card is not mistaken for a highlight"
    );
    assert!(card_slide.contains("val=\"FFDC00\""), "the card background remains");
}

#[test]
fn block_equation_exports_native_omml_with_fallback() {
    let p = parts(
        r#"#set page(width: 240pt, height: 120pt, margin: 12pt)
$ sum_(i=1)^n i = (n(n+1))/2 $"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let doc = roxmltree::Document::parse(slide).unwrap();

    let has_alternate = doc.descendants().any(|node| {
        node.tag_name().name() == "AlternateContent"
            && node.tag_name().namespace()
                == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
    });
    assert!(has_alternate, "math should be wrapped in mc:AlternateContent");

    let choice = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Choice"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        })
        .expect("math should have an mc:Choice");
    assert_eq!(choice.attribute("Requires"), Some("a14"));

    assert!(
        doc.descendants().any(|node| {
            node.tag_name().name() == "m"
                && node.tag_name().namespace()
                    == Some("http://schemas.microsoft.com/office/drawing/2010/main")
        }),
        "choice should contain a14:m"
    );
    assert!(
        doc.descendants().any(|node| {
            node.tag_name().name() == "oMath"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/officeDocument/2006/math")
        }),
        "choice should contain m:oMath"
    );
    assert!(
        doc.descendants().any(|node| {
            node.tag_name().name() == "Fallback"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        }),
        "math should carry an mc:Fallback"
    );
    assert!(
        slide.contains("<a:t>") && slide.contains("</a:t>"),
        "fallback should contain normal DrawingML text"
    );
    assert!(!slide.contains("<p:pic"), "display math should not rasterize");
    assert_all_wellformed(&p);
}

#[test]
fn math_fallback_preserves_scripts_limits_and_fraction_semantics() {
    let p = parts(
        r#"#set page(width: 300pt, height: 140pt, margin: 12pt)
$ integral_0^1 x^2 dif x = 1/3 $"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let doc = roxmltree::Document::parse(slide).unwrap();
    let fallback = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Fallback"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        })
        .expect("math should have a compatibility fallback")
        .descendants()
        .filter(|node| {
            node.tag_name().name() == "t"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/drawingml/2006/main")
        })
        .filter_map(|node| node.text())
        .collect::<String>();

    assert!(fallback.contains('₀'), "lower limit should remain a subscript: {fallback}");
    assert!(
        fallback.contains('¹'),
        "upper limit should remain a superscript: {fallback}"
    );
    assert!(fallback.contains('²'), "exponent should remain a superscript: {fallback}");
    assert!(fallback.contains("1/3"), "fraction bar should remain readable: {fallback}");
    assert_all_wellformed(&p);
}

#[test]
fn inline_equation_splices_native_omml_between_text_runs() {
    let p = parts(
        r#"#set page(width: 260pt, height: 120pt, margin: 12pt)
before $x^2$ after"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let doc = roxmltree::Document::parse(slide).unwrap();

    assert_eq!(
        text_shape_count(slide),
        1,
        "inline math should stay in the surrounding text box"
    );
    assert_eq!(
        drawingml_paragraph_count(slide),
        1,
        "inline math should stay in the surrounding paragraph"
    );

    let para = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "p"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/drawingml/2006/main")
                && node.descendants().any(|desc| {
                    desc.tag_name().name() == "t"
                        && desc.text().is_some_and(|text| text.contains("before"))
                })
        })
        .expect("paragraph with surrounding prose");

    let children = para
        .children()
        .filter(|node| node.is_element())
        .filter(|node| node.tag_name().name() != "pPr")
        .collect::<Vec<_>>();
    let child_names =
        children.iter().map(|node| node.tag_name().name()).collect::<Vec<_>>();
    assert_eq!(
        child_names,
        ["r", "AlternateContent", "r"],
        "paragraph children should preserve text/math/text order"
    );

    let first_text = children[0]
        .descendants()
        .filter(|node| node.tag_name().name() == "t")
        .filter_map(|node| node.text())
        .collect::<String>();
    let last_text = children[2]
        .descendants()
        .filter(|node| node.tag_name().name() == "t")
        .filter_map(|node| node.text())
        .collect::<String>();
    assert_eq!(first_text, "before ");
    assert_eq!(last_text, " after");

    let alternate = children[1];
    let choice = alternate
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Choice"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        })
        .expect("inline math should have an mc:Choice");
    assert_eq!(choice.attribute("Requires"), Some("a14"));
    assert!(
        choice.descendants().any(|node| {
            node.tag_name().name() == "m"
                && node.tag_name().namespace()
                    == Some("http://schemas.microsoft.com/office/drawing/2010/main")
        }),
        "choice should contain a14:m"
    );
    assert!(
        choice.descendants().any(|node| {
            node.tag_name().name() == "oMath"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/officeDocument/2006/math")
        }),
        "inline math should use bare m:oMath"
    );
    assert!(
        !choice.descendants().any(|node| {
            node.tag_name().name() == "oMathPara"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/officeDocument/2006/math")
        }),
        "inline math must not use display m:oMathPara"
    );

    let fallback = alternate
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Fallback"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        })
        .expect("inline math should have an mc:Fallback");
    let fallback_text = fallback
        .descendants()
        .filter(|node| node.tag_name().name() == "t")
        .filter_map(|node| node.text())
        .collect::<String>();
    assert!(!fallback_text.is_empty(), "fallback should contain linear text");

    assert!(!slide.contains("<m:oMathPara"), "inline-only slide has no display math");
    assert!(!slide.contains("<p:pic"), "inline math should not rasterize");
    assert_all_wellformed(&p);
}

#[test]
fn non_math_slides_do_not_gain_math_namespaces() {
    let p = parts("Plain text only.");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(!slide.contains("markup-compatibility/2006"));
    assert!(!slide.contains("officeDocument/2006/math"));
    assert!(!slide.contains("drawing/2010/main"));
    assert_all_wellformed(&p);
}

#[test]
fn gradient_filled_text_is_kept_as_solid() {
    // A non-solid text fill must not drop the whole run; it is approximated
    // with the first gradient stop so the text stays visible.
    let p = parts(
        r##"#set text(font: "New Computer Modern", fill: gradient.linear(rgb("#FF0000"), rgb("#0000FF")))
Gradient"##,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Gradient</a:t>"), "gradient text must survive");
    assert!(slide.contains("val=\"FF0000\""), "approximated with first stop");
    assert_all_wellformed(&p);
}

#[test]
fn radial_gradient_shape_does_not_native_map() {
    // A radial gradient has no verified-correct OOXML shape-relative form
    // here (an empirical LibreOffice check found the a:path/a:fillToRect
    // model renders visibly more circular than Typst's own box-relative
    // elliptical stretch on a non-square shape), so it stays on the raster
    // fallback rather than ship a subtly-wrong native mapping.
    let p = parts(
        r#"#set page(width: 160pt, height: 100pt, margin: 0pt)
#rect(width: 100pt, height: 60pt, fill: gradient.radial(red, blue))"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(!slide.contains("<a:gradFill"), "radial gradients are not natively mapped");
    assert_all_wellformed(&p);
}

#[test]
fn tiling_shape_fill_emits_native_blip_tile() {
    let p = binary_parts(
        r#"#set page(width: 240pt, height: 140pt, margin: 0pt)
#let pat = tiling(
  size: (20pt, 20pt),
  circle(radius: 8pt, fill: blue),
)
#rect(width: 200pt, height: 100pt, fill: pat)"#,
    );
    let text = text_parts_from_binary(&p);
    let slide = &text["ppt/slides/slide1.xml"];
    let rels = &text["ppt/slides/_rels/slide1.xml.rels"];

    assert!(slide.contains("<a:blipFill>"), "tiling fill should be image-backed");
    assert!(slide.contains("<a:tile "), "tiling fill should emit a:tile");
    assert!(slide.contains("sx=\"66667\""), "tile scale compensates PNG DPI");
    assert!(slide.contains("sy=\"66667\""), "tile scale compensates PNG DPI");
    assert!(slide.contains("algn=\"tl\""), "tile origin should align top-left");
    assert!(!slide.contains("<p:pic"), "shape should not rasterize as a picture");

    assert!(rels.contains("relationships/image"), "slide rels should include image");
    assert!(rels.contains("Target=\"../media/image1.png\""), "tile PNG target");

    let media: Vec<_> =
        p.iter().filter(|(name, _)| name.starts_with("ppt/media/")).collect();
    assert_eq!(media.len(), 1, "exactly one tile PNG media part");
    assert!(media[0].1.starts_with(b"\x89PNG\r\n\x1a\n"), "tile media is a PNG");
    assert_all_wellformed(&text);
}

#[test]
fn radial_gradient_page_background_does_not_native_map() {
    let p = parts(
        r#"#set page(width: 160pt, height: 100pt, margin: 0pt, fill: gradient.radial(red, blue))
Background"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(
        !slide.contains("<p:bg><p:bgPr><a:gradFill"),
        "radial page backgrounds are not natively mapped"
    );
    assert_all_wellformed(&p);
}

#[test]
fn svg_image_embeds_native_svg_with_png_fallback() {
    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40" viewBox="0 0 80 40"><rect width="80" height="40" fill="#0b6"/><circle cx="20" cy="20" r="12" fill="#fff"/></svg>"##;
    let src = format!(
        "#set page(width: 120pt, height: 80pt, margin: 0pt)\n\
         #image({}, width: 40pt, alt: \"Brand mark\")",
        bytes_literal(SVG),
    );

    let raw = binary_parts(&src);
    let p = text_parts_from_binary(&raw);
    let slide = &p["ppt/slides/slide1.xml"];
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];

    assert!(slide.contains("<a:blip r:embed=\""), "PNG fallback is the normal blip");
    assert!(slide.contains("uri=\"{28A0092B-C50C-407E-A947-70E740481C1C}\""));
    assert!(slide.contains("<a14:useLocalDpi"));
    assert!(slide.contains("uri=\"{96DAC541-7B7A-43D3-8B79-37D633B846F1}\""));
    assert!(slide.contains("<asvg:svgBlip"));
    assert!(slide.contains(
        "xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\""
    ));
    assert!(slide.contains("descr=\"Brand mark\""));
    assert!(rels.contains(".png\""), "slide rels should include the PNG fallback");
    assert!(rels.contains(".svg\""), "slide rels should include the native SVG");

    let svg_parts: Vec<_> = raw
        .iter()
        .filter(|(name, _)| name.starts_with("ppt/media/") && name.ends_with(".svg"))
        .collect();
    let png_parts: Vec<_> = raw
        .iter()
        .filter(|(name, _)| name.starts_with("ppt/media/") && name.ends_with(".png"))
        .collect();
    assert_eq!(svg_parts.len(), 1, "exactly one native SVG media part");
    assert_eq!(png_parts.len(), 1, "exactly one PNG fallback media part");
    assert_eq!(svg_parts[0].1.as_slice(), SVG, "the SVG part stores source bytes");
    assert!(
        png_parts[0].1.starts_with(b"\x89PNG\r\n\x1a\n"),
        "fallback media part must be a valid PNG"
    );
    assert!(
        p["[Content_Types].xml"]
            .contains("<Default Extension=\"svg\" ContentType=\"image/svg+xml\"/>"),
        "package declares the SVG media content type"
    );
    assert_all_wellformed(&p);
}

#[test]
fn wrapped_paragraph_merges_into_one_flowing_text_box() {
    let p = parts(
        r#"#set page(width: 220pt, height: 140pt, margin: 0pt)
#set text(size: 12pt)
#place(top + left, dx: 20pt, dy: 20pt)[
  #box(width: 95pt)[This paragraph wraps across several visual lines with #strong[bold text] inside for run splitting.]
]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(text_shape_count(slide), 1, "wrapped paragraph should be one box");
    assert_eq!(
        drawingml_paragraph_count(slide),
        1,
        "wrapped paragraph should be one a:p"
    );
    assert!(slide.contains("wrap=\"square\""), "merged paragraph should wrap");
    assert!(
        slide.matches("<a:r>").count() >= 2,
        "paragraph should preserve multiple runs"
    );
    assert_all_wellformed(&p);
}

#[test]
fn wrapped_paragraph_preserves_native_first_line_indent() {
    let p = parts(
        r#"#set page(width: 135pt, height: 140pt, margin: 20pt)
#set text(size: 12pt)
#set par(first-line-indent: (amount: 24pt, all: true))
This paragraph wraps across several visual lines so its first-line indent remains editable."#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(text_shape_count(slide), 1, "wrapped paragraph should be one box");
    assert!(
        slide.contains("<a:pPr algn=\"l\" marL=\"0\" indent=\"304800\">"),
        "24pt first-line indent should remain a native DrawingML paragraph property: {slide}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn wrapped_paragraph_preserves_wide_native_line_spacing() {
    let p = parts(
        r#"#set page(width: 200pt, height: 180pt, margin: 20pt)
#set text(size: 12pt)
#set par(leading: 2em)
First paragraph line one wraps with enough words to force a second line.

Second paragraph remains separate."#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(
        slide.contains("<a:spcPts val=\"3190\"/>"),
        "measured 31.9pt baseline pitch should remain native absolute line spacing: {slide}"
    );
    assert_eq!(
        drawingml_paragraph_count(slide),
        2,
        "wide leading must not merge the following source paragraph"
    );
    assert_all_wellformed(&p);
}

#[test]
fn bullet_list_uses_native_buchar_without_literal_marker_text() {
    let p = parts(
        r#"#set page(width: 240pt, height: 140pt, margin: 0pt)
#set text(size: 12pt)
- First native bullet
- Second native bullet"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(text_shape_count(slide), 1, "bullet list should be one box");
    assert_eq!(slide.matches("<a:buChar char=\"•\"/>").count(), 2);
    assert!(
        slide.contains("wrap=\"none\""),
        "single-line bullet items should not be rewrapped by consumer font metrics"
    );
    assert_no_text_node_contains(slide, "•");
    assert_all_wellformed(&p);
}

#[test]
fn enum_list_uses_native_autonumbering_without_literal_marker_text() {
    let p = parts(
        r#"#set page(width: 240pt, height: 140pt, margin: 0pt)
#set text(size: 12pt)
+ First native enum
+ Second native enum"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(text_shape_count(slide), 1, "enum list should be one box");
    assert!(slide.contains("<a:buAutoNum type=\"arabicPeriod\" startAt=\"1\"/>"));
    assert!(slide.contains("<a:buAutoNum type=\"arabicPeriod\" startAt=\"2\"/>"));
    assert_no_text_node_contains(slide, "1.");
    assert_no_text_node_contains(slide, "2.");
    assert_all_wellformed(&p);
}

#[test]
fn top_heading_is_bound_to_title_placeholder() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt, margin: 18pt)
= Native Title
Body text."#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let layout = &p["ppt/slideLayouts/slideLayout1.xml"];
    let master = &p["ppt/slideMasters/slideMaster1.xml"];
    assert!(has_title_placeholder(slide), "slide title shape should be a title ph");
    assert!(has_title_placeholder(layout), "layout should inherit a title ph");
    assert!(has_title_placeholder(master), "master should inherit a title ph");
    assert_all_wellformed(&p);
}

#[test]
fn main_body_text_is_bound_to_body_placeholder() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt, margin: 18pt)
= Native Title

Some body text."#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let layout = &p["ppt/slideLayouts/slideLayout1.xml"];
    let master = &p["ppt/slideMasters/slideMaster1.xml"];

    assert_eq!(placeholder_count(slide, "body"), 1, "slide should mark one body ph");
    assert!(has_placeholder(layout, "body"), "layout should inherit a body ph");
    assert!(has_placeholder(master, "body"), "master should inherit a body ph");
    assert_all_wellformed(&p);
}

#[test]
fn ambiguous_non_title_text_boxes_are_not_body_placeholders() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt, margin: 0pt)
#place(top + left, dx: 20pt, dy: 12pt)[#text(size: 24pt)[Deck Title]]
#place(top + left, dx: 20pt, dy: 70pt)[Column]
#place(top + left, dx: 180pt, dy: 70pt)[Column]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let layout = &p["ppt/slideLayouts/slideLayout1.xml"];
    let master = &p["ppt/slideMasters/slideMaster1.xml"];

    assert_eq!(placeholder_count(slide, "body"), 0, "ambiguous columns stay plain");
    assert!(!has_placeholder(layout, "body"), "layout should not gain body ph");
    assert!(!has_placeholder(master, "body"), "master should not gain body ph");
    assert_all_wellformed(&p);
}

#[test]
fn page_numbering_emits_live_slide_number_field() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt, margin: 24pt, numbering: "1")
= First
Body
#pagebreak()
= Second
More"#,
    );
    let slide1 = &p["ppt/slides/slide1.xml"];
    let slide2 = &p["ppt/slides/slide2.xml"];
    let layout = &p["ppt/slideLayouts/slideLayout1.xml"];
    let master = &p["ppt/slideMasters/slideMaster1.xml"];

    assert_eq!(placeholder_count(slide1, "sldNum"), 1);
    assert_eq!(placeholder_count(slide2, "sldNum"), 1);
    assert_eq!(slide_number_fallback(slide1), Some("1".into()));
    assert_eq!(slide_number_fallback(slide2), Some("2".into()));
    assert!(has_placeholder(layout, "sldNum"), "layout should inherit sldNum ph");
    assert!(has_placeholder(master, "sldNum"), "master should inherit sldNum ph");
    assert_all_wellformed(&p);
}

#[test]
fn translucent_fill_emits_alpha() {
    let p = parts(
        "#set page(width: 200pt, height: 100pt, margin: 0pt)\n\
         #place(top + left, rect(width: 80pt, height: 40pt, fill: rgb(255, 0, 0, 128)))",
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:alpha val=\"50196\"/>"), "50% alpha as thousandths");
    assert!(slide.contains("val=\"FF0000\""), "opaque channel unchanged");
    assert_all_wellformed(&p);
}

#[test]
fn straight_line_exports_as_loose_connector() {
    let p = parts(
        r#"#set page(width: 200pt, height: 100pt, margin: 0pt)
#line(
  start: (0pt, 0pt),
  end: (100pt, 50pt),
  stroke: (paint: red, thickness: 3pt, cap: "round", dash: "dashed"),
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let doc = roxmltree::Document::parse(slide).unwrap();
    let connectors = doc
        .descendants()
        .filter(|node| node.tag_name().name() == "cxnSp")
        .collect::<Vec<_>>();
    assert_eq!(connectors.len(), 1, "straight line should be one connector");
    assert_eq!(count_xml_nodes(slide, "sp"), 0, "line must not emit p:sp");

    let connector = connectors[0];
    assert!(
        connector
            .descendants()
            .any(|node| node.tag_name().name() == "nvCxnSpPr"),
        "connector should use connector non-visual properties"
    );
    assert!(
        connector
            .descendants()
            .any(|node| node.tag_name().name() == "cNvCxnSpPr"),
        "connector should use cNvCxnSpPr"
    );

    let xfrm = connector
        .descendants()
        .find(|node| node.tag_name().name() == "xfrm")
        .expect("connector should have a:xfrm");
    assert_eq!(xfrm.attribute("flipH"), None);
    assert_eq!(xfrm.attribute("flipV"), None);
    let ext = xfrm
        .descendants()
        .find(|node| node.tag_name().name() == "ext")
        .expect("connector should have a:ext");
    assert_eq!(ext.attribute("cx"), Some("1270000"));
    assert_eq!(ext.attribute("cy"), Some("635000"));

    let prst = connector
        .descendants()
        .find(|node| node.tag_name().name() == "prstGeom")
        .expect("connector should have preset geometry");
    assert_eq!(prst.attribute("prst"), Some("line"));
    let stroke = connector
        .descendants()
        .find(|node| node.tag_name().name() == "ln")
        .expect("connector should have stroke");
    assert_eq!(stroke.attribute("w"), Some("38100"));
    assert_eq!(stroke.attribute("cap"), Some("rnd"));
    assert!(
        stroke.descendants().any(|node| node.tag_name().name() == "prstDash"
            && node.attribute("val") == Some("sysDash")),
        "connector should preserve dash preset"
    );
    assert!(
        !stroke.descendants().any(|node| node.tag_name().name() == "prstDash"
            && node.attribute("val") == Some("sysDot")),
        "equal dashed segments must not degrade to round dots"
    );
    assert_all_wellformed(&p);
}

#[test]
fn descending_straight_line_connector_uses_flip() {
    let p = parts(
        r#"#set page(width: 200pt, height: 100pt, margin: 0pt)
#line(start: (100pt, 0pt), end: (0pt, 50pt), stroke: 2pt)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let doc = roxmltree::Document::parse(slide).unwrap();
    let connector = doc
        .descendants()
        .find(|node| node.tag_name().name() == "cxnSp")
        .expect("straight line should emit a connector");
    let xfrm = connector
        .descendants()
        .find(|node| node.tag_name().name() == "xfrm")
        .expect("connector should have a:xfrm");
    assert_eq!(xfrm.attribute("flipH"), Some("1"));
    assert_eq!(xfrm.attribute("flipV"), None);
    let ext = xfrm
        .descendants()
        .find(|node| node.tag_name().name() == "ext")
        .expect("connector should have a:ext");
    assert_eq!(ext.attribute("cx"), Some("1270000"));
    assert_eq!(ext.attribute("cy"), Some("635000"));
    assert_all_wellformed(&p);
}

#[test]
fn curves_and_rectangles_remain_regular_shapes() {
    let p = parts(
        r#"#set page(width: 200pt, height: 120pt, margin: 0pt)
#rect(width: 40pt, height: 20pt, fill: teal)
#curve(
  stroke: 2pt,
  curve.move((0pt, 0pt)),
  curve.cubic((10pt, 0pt), (20pt, 50pt), (50pt, 50pt)),
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(count_xml_nodes(slide, "cxnSp"), 0, "non-lines stay p:sp");
    assert_eq!(count_xml_nodes(slide, "sp"), 2, "rect and curve stay regular shapes");
    assert!(slide.contains("<a:custGeom>"), "regular shapes still use custGeom");
    assert!(slide.contains("<a:cubicBezTo>"), "curve cubic segment is preserved");
    assert_all_wellformed(&p);
}

#[test]
fn noop_clip_keeps_text_live() {
    // A clipped card whose content fits inside the clip must not bake its
    // text into a picture — the render probe proves the clip is a no-op.
    let p =
        parts("#box(radius: 8pt, clip: true, fill: luma(240), inset: 12pt)[Card text]");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Card text</a:t>"), "clipped card text must stay live");
    assert!(!slide.contains("<p:pic"), "a no-op clip must not rasterize");
    assert_all_wellformed(&p);
}

#[test]
fn real_clip_still_rasterizes_exactly() {
    // Content that genuinely overflows its clip box has no native PPTX form;
    // the visual must be preserved via the raster fallback.
    let p = parts(
        "#box(width: 80pt, height: 30pt, radius: 8pt, clip: true, fill: luma(240))[\n\
           #box(width: 200pt)[This long text is genuinely cut off by the clip]\n\
         ]",
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<p:pic"), "a real clip must keep the exact raster");
    assert!(
        !slide.contains("<a:t>This long text"),
        "clipped-off text must not leak as an unclipped live run"
    );
    assert_all_wellformed(&p);
}

#[test]
fn rounded_clip_single_image_becomes_native_round_rect_picture() {
    let png = tiny_png();
    let src = format!(
        "#set page(width: 100pt, height: 70pt, margin: 0pt)\n\
         #box(width: 60pt, height: 40pt, radius: 10pt, clip: true,\n\
         image({}, width: 100%, height: 100%, fit: \"stretch\"))",
        bytes_literal(&png),
    );

    let p = binary_parts(&src);
    let slide = std::str::from_utf8(&p["ppt/slides/slide1.xml"]).unwrap();
    assert!(slide.contains("<p:pic"), "image should remain a native picture");
    assert!(
        slide.contains("prst=\"roundRect\""),
        "picture should carry roundRect geometry"
    );
    assert!(
        slide.contains("<a:gd name=\"adj\" fmla=\"val 25000\"/>"),
        "10pt radius over 40pt short side should become adj=25000"
    );

    let mut media: Vec<_> =
        p.iter().filter(|(name, _)| name.starts_with("ppt/media/")).collect();
    media.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(media.len(), 1, "should not add a rendered fallback image");
    assert_eq!(media[0].1, &png, "media part should be the original PNG bytes");
    assert_all_wellformed(&text_parts_from_binary(&p));
}

#[test]
fn rounded_cover_image_crops_the_overflow_with_src_rect() {
    // A square image `fit: "cover"` into a 60x40 box scales to 60x60 and
    // overflows top and bottom. The rounded picture must carry the original
    // bytes and crop the overflow with `a:srcRect` (rather than rasterize the
    // visible slice), so 10pt of overflow on each 60pt edge = 16.667%.
    let png = tiny_png();
    let src = format!(
        "#set page(width: 100pt, height: 70pt, margin: 0pt)\n\
         #box(width: 60pt, height: 40pt, radius: 10pt, clip: true,\n\
         image({}, width: 100%, height: 100%, fit: \"cover\"))",
        bytes_literal(&png),
    );

    let p = binary_parts(&src);
    let slide = std::str::from_utf8(&p["ppt/slides/slide1.xml"]).unwrap();
    assert!(slide.contains("prst=\"roundRect\""), "cover picture stays a roundRect");
    assert!(
        slide.contains("<a:srcRect l=\"0\" t=\"16667\" r=\"0\" b=\"16667\"/>"),
        "the vertical cover overflow should be cropped, got: {slide}"
    );

    let media: Vec<_> =
        p.iter().filter(|(name, _)| name.starts_with("ppt/media/")).collect();
    assert_eq!(media.len(), 1, "cover crop must not add a rendered fallback image");
    assert_eq!(media[0].1, &png, "media part must be the original PNG bytes");
    assert_all_wellformed(&text_parts_from_binary(&p));
}

#[test]
fn svg_cover_crop_preserves_native_source_and_src_rect() {
    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40" viewBox="0 0 80 40"><rect width="10" height="40" fill="red"/><rect x="10" width="60" height="40" fill="blue"/><rect x="70" width="10" height="40" fill="green"/></svg>"##;
    let src = format!(
        "#set page(width: 180pt, height: 180pt, margin: 0pt)\n\
         #box(width: 144pt, height: 144pt, clip: true,\n\
         image({}, width: 100%, height: 100%, fit: \"cover\"))",
        bytes_literal(SVG),
    );

    let p = binary_parts(&src);
    let slide = std::str::from_utf8(&p["ppt/slides/slide1.xml"]).unwrap();
    assert!(
        slide.contains("<a:srcRect l=\"25000\" t=\"0\" r=\"25000\" b=\"0\"/>"),
        "2:1 SVG cover-fitted into a square should keep a native center crop: {slide}"
    );
    assert!(slide.contains("<asvg:svgBlip"), "picture should retain native SVG media");

    let svg_parts: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("ppt/media/") && name.ends_with(".svg"))
        .collect();
    let png_parts: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("ppt/media/") && name.ends_with(".png"))
        .collect();
    assert_eq!(svg_parts.len(), 1, "one native SVG source");
    assert_eq!(svg_parts[0].1.as_slice(), SVG, "SVG bytes stay original and editable");
    assert_eq!(png_parts.len(), 1, "one full-canvas compatibility fallback");
    assert_all_wellformed(&text_parts_from_binary(&p));
}

#[test]
fn out_of_range_page_link_is_dropped() {
    // A jump to a page that does not exist must not emit a slide relationship
    // (PowerPoint treats a dangling slide target as a corrupt file).
    let p = parts("#link((page: 99, x: 0pt, y: 0pt))[Jump]");
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    assert!(!rels.contains("slide99.xml"), "no relationship to a missing slide");
    assert!(!rels.contains("hlinksldjump"), "no dangling slide-jump");
    assert_all_wellformed(&p);
}

#[test]
fn bold_and_italic_map_to_run_properties() {
    let p = parts(
        r#"#set text(font: "New Computer Modern")
#text(weight: 700, style: "italic")[Bold Italic]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Bold Italic</a:t>"));
    assert!(slide.contains(" b=\"1\""), "bold text should emit b=1");
    assert!(slide.contains(" i=\"1\""), "italic text should emit i=1");
    assert_all_wellformed(&p);
}

#[test]
fn explicit_columns_emit_single_multicolumn_text_box() {
    let p = parts(
        r#"#set page(width: 240pt, height: 120pt, margin: 10pt)
#columns(2, gutter: 20pt)[
  #text(size: 8pt)[#lorem(80)]
]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("numCol=\"2\""), "columns should set bodyPr numCol");
    assert!(
        slide.contains("spcCol=\"254000\""),
        "20pt gutter should be emitted as 254000 EMU, got: {slide}"
    );
    assert_eq!(
        slide.matches("txBox=\"1\"").count(),
        1,
        "real columns should be one editable text box"
    );
    assert_all_wellformed(&p);
}

#[test]
fn explicit_columns_preserve_reading_order_across_wrapped_lines() {
    // Regression test: each column here wraps across multiple lines whose
    // baselines land at nearly the same height as the other columns' lines.
    // A naive vertical-position sort across the whole region would treat
    // same-height lines from different columns as one reading row and
    // interleave them; the paragraphs must instead stay grouped per physical
    // column, each read top-to-bottom, in left-to-right column order.
    let p = parts(
        r#"#set page(width: 260pt, height: 120pt, margin: 10pt)
#set text(size: 9pt)
#columns(3, gutter: 12pt)[
  Alpha one alpha two alpha three alpha four.

  #colbreak()

  Bravo one bravo two bravo three bravo four.

  #colbreak()

  Charlie one charlie two charlie three four.
]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let alpha = slide.find("Alpha").expect("alpha column text present");
    let bravo = slide.find("Bravo").expect("bravo column text present");
    let charlie = slide.find("Charlie").expect("charlie column text present");
    assert!(
        alpha < bravo && bravo < charlie,
        "columns must stay in left-to-right reading order, got positions \
         alpha={alpha} bravo={bravo} charlie={charlie} in: {slide}"
    );
    // Each column's own wrapped lines must not be split apart by another
    // column's content landing in between.
    let alpha_para_end = slide[alpha..].find("</a:p>").map(|i| alpha + i).unwrap();
    assert!(
        bravo > alpha_para_end,
        "bravo column text must not be interleaved inside alpha's paragraph"
    );
    let bravo_para_end = slide[bravo..].find("</a:p>").map(|i| bravo + i).unwrap();
    assert!(
        charlie > bravo_para_end,
        "charlie column text must not be interleaved inside bravo's paragraph"
    );
    assert_all_wellformed(&p);
}

#[test]
fn text_columns_split_into_separate_boxes() {
    let p = parts(
        r#"#set page(width: 200pt, height: 100pt, margin: 0pt)
#place(top + left, dx: 10pt, dy: 50pt)[Left]
#place(top + left, dx: 130pt, dy: 50pt)[Right]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Left</a:t>"));
    assert!(slide.contains("<a:t>Right</a:t>"));
    assert_eq!(slide.matches("txBox=\"1\"").count(), 2);
    assert_all_wellformed(&p);
}

#[test]
fn url_link_emits_hlink_click_and_relationship() {
    let p = parts(r#"#link("https://example.com/")[linked]"#);
    let slide = &p["ppt/slides/slide1.xml"];
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    assert!(slide.contains("<a:t>linked</a:t>"));
    assert!(slide.contains("<a:hlinkClick"));
    assert!(slide.contains("name=\"Hyperlink "), "linked text gets a native click area");
    assert!(
        slide.contains("<a:alpha val=\"0\"/>"),
        "the click area must remain visually transparent"
    );
    assert!(slide.contains("r:id=\"rId"));
    assert!(rels.contains("relationships/hyperlink"));
    assert!(rels.contains("Target=\"https://example.com/\""));
    assert!(rels.contains("TargetMode=\"External\""));
    assert_all_wellformed(&p);
}

#[test]
fn explicitly_styled_url_link_keeps_run_appearance() {
    let p = parts(
        r#"#set text(fill: rgb("222222"))
#link("https://example.com")[#text(fill: red, weight: "bold")[Styled link]]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    assert!(
        slide.contains("<a:rPr lang=\"en-US\" sz=\"1100\" b=\"1\">"),
        "styled link should stay bold without hyperlink theme formatting: {slide}"
    );
    assert!(slide.contains("<a:srgbClr val=\"FF4136\"/>"), "red remains direct");
    assert!(slide.contains("name=\"Hyperlink "), "link gets an overlay shape");
    assert!(slide.contains("<a:hlinkClick"), "overlay remains clickable");
    assert!(slide.contains("<a:alpha val=\"0\"/>"), "overlay remains invisible");
    assert!(rels.contains("Target=\"https://example.com\""));
    assert_all_wellformed(&p);
}

#[test]
fn enclosing_link_makes_the_native_shape_clickable() {
    let p = parts(
        r#"#link("https://example.com/")[#rect(width: 80pt, height: 30pt, fill: teal)[Linked shape]]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    let doc = roxmltree::Document::parse(slide).unwrap();
    let shape_props = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "cNvPr"
                && node.attribute("name").is_some_and(|name| name.starts_with("Shape "))
        })
        .expect("native linked shape");
    assert!(
        shape_props
            .descendants()
            .any(|node| node.tag_name().name() == "hlinkClick"),
        "the whole shape hit area should carry the hyperlink"
    );
    assert!(rels.contains("Target=\"https://example.com/\""));
    assert!(rels.contains("TargetMode=\"External\""));
    assert_all_wellformed(&p);
}

#[test]
fn enclosing_link_makes_the_native_picture_clickable() {
    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40"><rect width="80" height="40" fill="#0b6"/></svg>"##;
    let p = parts(&format!(
        "#link(\"https://example.com/image\")[#image({}, width: 80pt)]",
        bytes_literal(SVG),
    ));
    let slide = &p["ppt/slides/slide1.xml"];
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    let doc = roxmltree::Document::parse(slide).unwrap();
    let picture_props = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "cNvPr"
                && node
                    .attribute("name")
                    .is_some_and(|name| name.starts_with("Picture "))
        })
        .expect("native linked picture");
    assert!(
        picture_props
            .descendants()
            .any(|node| node.tag_name().name() == "hlinkClick"),
        "the whole picture hit area should carry the hyperlink"
    );
    assert!(rels.contains("Target=\"https://example.com/image\""));
    assert_all_wellformed(&p);
}

#[test]
fn powerpoint_schema_invariants_hold() {
    // Real Microsoft PowerPoint (unlike LibreOffice) repairs a file that
    // violates these; each was a verified ship-blocker.
    let p = parts("#rect(width: 40pt, height: 20pt, fill: teal)");
    let theme = &p["ppt/theme/theme1.xml"];
    // CT_StyleMatrix requires a minimum of THREE entries in each style list.
    for (list, item) in [
        ("fillStyleLst", ["solidFill", "gradFill"]),
        ("bgFillStyleLst", ["solidFill", "gradFill"]),
    ] {
        let body = theme.split(&format!("<a:{list}>")).nth(1).unwrap();
        let body = body.split(&format!("</a:{list}>")).next().unwrap();
        let n = item
            .iter()
            .map(|i| body.matches(&format!("<a:{i}")).count())
            .sum::<usize>();
        assert!(n >= 3, "{list} has {n} entries; PowerPoint needs >= 3");
    }
    let effects = theme.split("<a:effectStyleLst>").nth(1).unwrap();
    let effects = effects.split("</a:effectStyleLst>").next().unwrap();
    assert!(
        effects.matches("<a:effectStyle>").count() >= 3,
        "effectStyleLst needs >= 3 entries"
    );
    // `.rels` parts must NOT be content-type Overrides (the rels Default
    // covers them); PowerPoint repairs a package that lists them.
    let ct = &p["[Content_Types].xml"];
    assert!(
        !ct.contains(".rels\""),
        "no .rels part may appear as a content-type Override"
    );
    // custGeom's text rectangle must use literal coordinates, not undefined
    // guide names (`r="r"`).
    let slide = &p["ppt/slides/slide1.xml"];
    if slide.contains("<a:custGeom>") {
        assert!(
            !slide.contains("r=\"r\"") && !slide.contains("b=\"b\""),
            "custGeom a:rect must not reference undefined guides"
        );
    }
    assert_all_wellformed(&p);
}

#[test]
fn baseline_position_uses_measured_box_top_rule() {
    let p = parts(
        r#"#set page(width: 200pt, height: 100pt, margin: 0pt)
#place(top + left, dy: 50pt)[X]"#,
    );
    let y = text_box_y(&p["ppt/slides/slide1.xml"], "X");
    assert!(
        (30 * 12700..=50 * 12700).contains(&y),
        "text box y {y} should be between 30pt and 50pt in EMU"
    );
    assert_all_wellformed(&p);
}

fn text_box_y(slide: &str, needle: &str) -> i64 {
    let doc = roxmltree::Document::parse(slide).unwrap();
    for shape in doc.descendants().filter(|node| node.tag_name().name() == "sp") {
        let has_text = shape
            .descendants()
            .any(|node| node.tag_name().name() == "t" && node.text() == Some(needle));
        if has_text {
            let off = shape
                .descendants()
                .find(|node| node.tag_name().name() == "off")
                .expect("text box should have a:xfrm/a:off");
            return off.attribute("y").unwrap().parse().unwrap();
        }
    }
    panic!("missing text box for {needle}");
}

fn text_shape_count(slide: &str) -> usize {
    let doc = roxmltree::Document::parse(slide).unwrap();
    doc.descendants()
        .filter(|node| node.tag_name().name() == "sp")
        .filter(|shape| {
            shape.descendants().any(|node| node.tag_name().name() == "txBody")
        })
        .count()
}

fn drawingml_paragraph_count(slide: &str) -> usize {
    let doc = roxmltree::Document::parse(slide).unwrap();
    doc.descendants()
        .filter(|node| {
            node.tag_name().name() == "p"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/drawingml/2006/main")
        })
        .count()
}

fn assert_no_text_node_contains(slide: &str, marker: &str) {
    let doc = roxmltree::Document::parse(slide).unwrap();
    for node in doc.descendants().filter(|node| node.tag_name().name() == "t") {
        assert!(
            !node.text().unwrap_or_default().contains(marker),
            "literal marker {marker:?} leaked into text node {:?}",
            node.text()
        );
    }
}

fn has_title_placeholder(xml: &str) -> bool {
    has_placeholder(xml, "title")
}

fn has_placeholder(xml: &str, ty: &str) -> bool {
    placeholder_count(xml, ty) > 0
}

fn placeholder_count(xml: &str, ty: &str) -> usize {
    let doc = roxmltree::Document::parse(xml).unwrap();
    doc.descendants()
        .filter(|node| {
            node.tag_name().name() == "ph" && node.attribute("type") == Some(ty)
        })
        .count()
}

fn slide_number_fallback(xml: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).unwrap();
    let field = doc.descendants().find(|node| {
        node.tag_name().name() == "fld" && node.attribute("type") == Some("slidenum")
    })?;
    Some(
        field
            .descendants()
            .filter(|node| node.tag_name().name() == "t")
            .filter_map(|node| node.text())
            .collect(),
    )
}

fn tiny_png() -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(2, 2).unwrap();
    pixmap.fill(tiny_skia::Color::from_rgba8(210, 80, 40, 255));
    pixmap.encode_png().unwrap()
}

fn bytes_literal(bytes: &[u8]) -> String {
    let mut out = String::from("bytes((");
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write!(&mut out, "{byte}").unwrap();
    }
    out.push_str("))");
    out
}

//! Structural and well-formedness tests for the PPTX exporter foundation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::model::Document;
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use typst_pptx::{PptxOptions, pptx, pptx_with_page_mapping};

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
    pptx(&doc, &world, &PptxOptions::default()).expect("pptx export failed")
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

fn text_parts(bytes: Vec<u8>) -> HashMap<String, String> {
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

/// Compiles `src` to a PPTX and returns text parts as `name -> text`.
fn parts(src: &str) -> HashMap<String, String> {
    text_parts(pptx_bytes(src))
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
fn editable_text_embeds_license_permitted_fonts() {
    let binary = binary_parts("Hello, portable presentation.");
    let p = text_parts_from_binary(&binary);
    let presentation = &p["ppt/presentation.xml"];
    let rels = &p["ppt/_rels/presentation.xml.rels"];
    let content_types = &p["[Content_Types].xml"];

    assert!(presentation.contains("embedTrueTypeFonts=\"1\""));
    assert!(presentation.contains("<p:embeddedFontLst>"));
    assert!(presentation.contains("<p:font typeface=\"Libertinus Serif\""));
    assert!(presentation.contains("<p:regular r:id=\""));
    assert!(rels.contains(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/font"
    ));
    assert!(rels.contains("Target=\"fonts/font1.fntdata\""));
    assert!(
        content_types
            .contains("Extension=\"fntdata\" ContentType=\"application/x-fontdata\"")
    );

    let fonts = binary
        .iter()
        .filter(|(name, _)| name.starts_with("ppt/fonts/") && name.ends_with(".fntdata"))
        .collect::<Vec<_>>();
    assert!(!fonts.is_empty(), "expected at least one embedded font part");
    for (name, data) in fonts {
        let eot_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let font_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let eot_magic = u16::from_le_bytes(data[34..36].try_into().unwrap());
        assert_eq!(eot_size, data.len(), "{name} EOT size must cover the part");
        assert_eq!(version, 0x0001_0000, "{name} must use EOT 1.0");
        assert_eq!(eot_magic, 0x504C, "{name} must carry the EOT magic");
        let magic = data
            .get(data.len() - font_size..data.len() - font_size + 4)
            .expect("EOT should end with an sfnt program");
        assert!(
            matches!(magic, b"OTTO" | b"\0\x01\0\0" | b"true" | b"typ1"),
            "{name} does not contain an OpenType/TrueType program: {magic:02X?}"
        );
    }
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
fn transformed_table_content_uses_picture_fallback() {
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt)
#rotate(20deg)[#table(columns: 2, [A], [B], [C], [D])]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    // A rotated table has no native DrawingML representation. Its content
    // must remain present through the generic picture fallback instead of
    // disappearing when table capture rejects the transform.
    assert!(!slide.contains("<a:tbl>"));
    assert!(slide.contains("<p:pic>"), "transformed table needs picture fallback");
    let rels = &p["ppt/slides/_rels/slide1.xml.rels"];
    assert!(
        rels.contains("/image"),
        "fallback picture should have an image relationship"
    );

    let doc = roxmltree::Document::parse(slide).unwrap();
    let transparent_text: String = doc
        .descendants()
        .filter(|node| {
            node.tag_name().name() == "sp"
                && node.descendants().any(|child| {
                    child.tag_name().name() == "alpha"
                        && child.attribute("val") == Some("0")
                })
        })
        .flat_map(|shape| {
            shape
                .descendants()
                .filter(|node| node.tag_name().name() == "t")
                .filter_map(|node| node.text())
        })
        .collect();
    assert_eq!(
        transparent_text, "ABCD",
        "the exact table picture must retain searchable/editable cell text"
    );
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
fn table_cell_insets_are_preserved() {
    let p = parts(
        r#"#set page(width: 360pt, height: 200pt)
#table(
  columns: 2,
  inset: (left: 28pt, right: 14pt, top: 16pt, bottom: 8pt),
  [left], [right],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(
        slide
            .matches(
                "<a:tcPr marL=\"355600\" marT=\"203200\" marR=\"177800\" marB=\"101600\""
            )
            .count(),
        2,
        "each native cell should keep its authored DrawingML table margins"
    );
    assert_all_wellformed(&p);
}

#[test]
fn table_border_dash_and_cap_are_preserved() {
    let p = parts(
        r#"#set page(width: 360pt, height: 200pt)
#table(
  columns: 2,
  stroke: (paint: red, thickness: 3pt, dash: "dashed", cap: "round"),
  [left], [right],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("cap=\"rnd\""), "round table border caps");
    assert!(
        slide.contains("<a:custDash><a:ds d=\"100000\" sp=\"100000\"/></a:custDash>"),
        "table borders should preserve the authored dash lengths exactly"
    );

    // A pattern that *is* a DrawingML preset stays one, so PowerPoint's border
    // UI shows a named dash rather than a custom pattern.
    let preset = parts(
        r#"#set page(width: 360pt, height: 200pt)
#table(
  columns: 2,
  stroke: (paint: red, thickness: 2pt, dash: (8pt, 6pt)),
  [left], [right],
)"#,
    );
    assert!(
        preset["ppt/slides/slide1.xml"].contains("<a:prstDash val=\"dash\"/>"),
        "an exact preset pattern stays a preset"
    );
    assert_all_wellformed(&p);
    assert_all_wellformed(&preset);
}

#[test]
fn table_gutters_become_editable_spacer_tracks() {
    let p = parts(
        r#"#set page(width: 8in, height: 4.5in, margin: 24pt)
#table(
  columns: (100pt, 100pt),
  gutter: 24pt,
  [Alpha], [Beta],
  [Left], [Right],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(count_xml_nodes(slide, "gridCol"), 3);
    assert!(
        slide.contains("<a:gridCol w=\"304800\"/>"),
        "24pt horizontal gutter should be a borderless spacer column"
    );
    assert_eq!(count_xml_nodes(slide, "tr"), 3);
    assert!(
        slide.contains("<a:tr h=\"304800\">"),
        "24pt vertical gutter should be a borderless spacer row"
    );
    assert_eq!(count_xml_nodes(slide, "tc"), 9);
    for text in ["Alpha", "Beta", "Left", "Right"] {
        assert_eq!(slide.matches(&format!("<a:t>{text}</a:t>")).count(), 1);
    }
    assert_all_wellformed(&p);
}

#[test]
fn table_gutter_tracks_participate_in_cell_spans() {
    let p = parts(
        r#"#set page(width: 360pt, height: 220pt)
#table(
  columns: 3,
  gutter: 10pt,
  table.cell(colspan: 2)[wide], [c],
  table.cell(rowspan: 2)[tall], [e], [f],
  [h], [i],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert_eq!(count_xml_nodes(slide, "gridCol"), 5);
    assert_eq!(count_xml_nodes(slide, "tr"), 5);
    assert!(
        slide.contains("gridSpan=\"3\""),
        "a two-column cell also spans its internal spacer track"
    );
    assert!(
        slide.contains("rowSpan=\"3\""),
        "a two-row cell also spans its internal spacer track"
    );
    for text in ["wide", "tall", "c", "e", "f", "h", "i"] {
        assert_eq!(slide.matches(&format!("<a:t>{text}</a:t>")).count(), 1);
    }
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
#set text(size: 28pt)
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
    assert!(!fallback.contains("^("), "single limits should not gain parentheses");
    let fallback_run = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "Fallback"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
        })
        .and_then(|node| {
            node.descendants().find(|child| child.tag_name().name() == "rPr")
        })
        .expect("fallback run properties");
    assert_eq!(fallback_run.attribute("sz"), Some("2800"));
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
fn radial_gradient_shape_maps_to_a_native_circle_path() {
    // Both models normalize the gradient to the painted box, so the elliptical
    // stretch of a radial fill in a non-square shape carries over natively.
    let p = parts(
        r#"#set page(width: 160pt, height: 100pt, margin: 0pt)
#rect(width: 100pt, height: 60pt, fill: gradient.radial(red, blue))"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:path path=\"circle\">"), "radial maps to a circle path");
    assert!(!slide.contains("<a:blip"), "not rasterized");

    // A conic gradient has no `a:gradFill` path that sweeps by angle, and an
    // off-centre outer circle cannot be expressed at all (DrawingML's outer
    // path is always the shape's own rectangle): both keep the raster fallback.
    for fill in [
        "gradient.conic(red, blue)",
        "gradient.radial(red, blue, center: (20%, 80%))",
    ] {
        let raster = parts(&format!(
            "#set page(width: 160pt, height: 100pt, margin: 0pt)\n\
             #rect(width: 100pt, height: 60pt, fill: {fill})"
        ));
        assert!(
            !raster["ppt/slides/slide1.xml"].contains("<a:gradFill"),
            "{fill} is not claimed as a native gradient"
        );
    }
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
fn radial_gradient_page_background_maps_natively() {
    let p = parts(
        r#"#set page(width: 160pt, height: 100pt, margin: 0pt, fill: gradient.radial(red, blue))
Background"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(
        slide.contains("<p:bg><p:bgPr><a:gradFill"),
        "a radial page background is a native slide background: {slide}"
    );
    assert!(slide.contains("<a:path path=\"circle\">"), "with a circle path");
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
    let ds = stroke
        .descendants()
        .find(|node| node.tag_name().name() == "ds")
        .expect("connector should carry an exact dash pattern");
    // `dashed` is an equal 3pt on/off pair, which at a 3pt width is one line
    // width of each — not any preset's proportions.
    assert_eq!(ds.attribute("d"), Some("100000"));
    assert_eq!(ds.attribute("sp"), Some("100000"));
    assert!(
        !stroke.descendants().any(|node| node.tag_name().name() == "prstDash"),
        "an exactly stated pattern must not also emit a preset"
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
    let doc = roxmltree::Document::parse(slide).unwrap();
    let fallbacks: Vec<_> = doc
        .descendants()
        .filter(|node| {
            node.tag_name().name() == "sp"
                && node.descendants().any(|child| {
                    child.tag_name().name() == "alpha"
                        && child.attribute("val") == Some("0")
                })
        })
        .collect();
    assert_eq!(fallbacks.len(), 1, "one transparent fallback text box per raster");
    let recovered: String = fallbacks[0]
        .descendants()
        .filter(|node| node.tag_name().name() == "t")
        .filter_map(|node| node.text())
        .collect();
    assert_eq!(
        recovered, "This long text is genuinely cut off by the clip",
        "rasterized text should remain searchable and editable"
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
fn filtered_pages_remap_physical_slide_links_and_drop_omitted_targets() {
    let src = r#"#link((page: 3, x: 0pt, y: 0pt))[Keep]
#link((page: 2, x: 0pt, y: 0pt))[Drop]
#pagebreak()
Page two
#pagebreak()
Page three"#;
    let world = TestWorld::new(src);
    let document = typst::compile::<PagedDocument>(&world)
        .output
        .expect("compilation failed");

    let full = text_parts(pptx(&document, &world, &PptxOptions::default()).unwrap());
    let full_rels = &full["ppt/slides/_rels/slide1.xml.rels"];
    assert!(
        full_rels.contains("slide2.xml"),
        "full export keeps the page-2 link: {full_rels}",
    );
    assert!(
        full_rels.contains("slide3.xml"),
        "full export keeps the page-3 link: {full_rels}",
    );

    let pages = ecow::eco_vec![document.pages()[0].clone(), document.pages()[2].clone()];
    let filtered = PagedDocument::new(pages, document.info().clone());
    let filtered = text_parts(
        pptx_with_page_mapping(
            &filtered,
            &world,
            &PptxOptions::default(),
            &[Some(0), None, Some(1)],
        )
        .unwrap(),
    );
    let filtered_rels = &filtered["ppt/slides/_rels/slide1.xml.rels"];
    assert!(
        filtered_rels.contains("slide2.xml"),
        "physical page 3 becomes exported slide 2: {filtered_rels}",
    );
    assert_eq!(
        filtered_rels.matches("slide2.xml").count(),
        1,
        "the omitted physical page 2 does not create a second slide jump",
    );
    assert!(!filtered_rels.contains("slide3.xml"));
    assert_all_wellformed(&filtered);

    let malformed = text_parts(
        pptx_with_page_mapping(
            &PagedDocument::new(
                ecow::eco_vec![document.pages()[0].clone(), document.pages()[2].clone()],
                document.info().clone(),
            ),
            &world,
            &PptxOptions::default(),
            &[Some(0), None, Some(99)],
        )
        .unwrap(),
    );
    let malformed_rels = &malformed["ppt/slides/_rels/slide1.xml.rels"];
    assert!(
        !malformed_rels.contains("../slides/"),
        "an invalid caller map cannot create a dangling slide relationship",
    );
}

#[test]
fn filtered_location_links_use_the_renumbered_introspector_page() {
    let src = r#"#link(<third>)[Jump to third]
#pagebreak()
Page two
#pagebreak()
Page three <third>"#;
    let world = TestWorld::new(src);
    let document = typst::compile::<PagedDocument>(&world)
        .output
        .expect("compilation failed");
    let filtered = PagedDocument::new(
        ecow::eco_vec![document.pages()[0].clone(), document.pages()[2].clone()],
        document.info().clone(),
    );
    let parts = text_parts(
        pptx_with_page_mapping(
            &filtered,
            &world,
            &PptxOptions::default(),
            &[Some(0), None, Some(1)],
        )
        .unwrap(),
    );
    let slide = &parts["ppt/slides/slide1.xml"];
    let rels = &parts["ppt/slides/_rels/slide1.xml.rels"];
    assert!(slide.contains("ppaction://hlinksldjump"));
    assert!(rels.contains("slide2.xml"));
    assert_all_wellformed(&parts);
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
fn explicit_colbreak_uses_independent_editable_column_boxes() {
    let p = parts(
        r#"#set page(width: 240pt, height: 120pt, margin: 10pt)
#columns(2, gutter: 20pt)[Left column.#colbreak()Right column.]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:t>Left column.</a:t>"));
    assert!(slide.contains("<a:t>Right column.</a:t>"));
    assert_eq!(
        slide.matches("txBox=\"1\"").count(),
        2,
        "a manual break needs independently editable physical columns"
    );
    assert!(
        !slide.contains("numCol=\"2\""),
        "DrawingML native columns only support automatic overflow"
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
fn explicit_columns_keep_inline_math_in_the_editable_text_flow() {
    let p = parts(
        r#"#set page(width: 240pt, height: 120pt, margin: 10pt)
#columns(2)[Before $x^2 + y^2 = z^2$ after]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("numCol=\"2\""), "columns stay native");
    assert!(slide.contains("<m:oMath>"), "inline math should remain native OMML");
    assert!(slide.contains("<m:sSup>"), "superscripts should retain their structure");
    assert_eq!(
        slide.matches("txBox=\"1\"").count(),
        1,
        "text and inline math should remain in one editable multicolumn text box"
    );
    assert_all_wellformed(&p);
}

#[test]
fn explicit_columns_keep_highlight_as_a_native_run_property() {
    let p = parts(
        r#"#set page(width: 240pt, height: 120pt, margin: 10pt)
#columns(2)[Before #highlight(fill: yellow)[Highlighted] after]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("numCol=\"2\""), "columns stay native");
    assert!(slide.contains("<a:highlight>"), "highlight should stay native");
    assert!(slide.contains("<a:t>Highlighted</a:t>"));
    assert_eq!(
        slide.matches("<p:sp>").count(),
        1,
        "the detached highlight rectangle should be consumed by the text run"
    );
    assert_all_wellformed(&p);
}

#[test]
fn table_cell_inline_math_stays_native_and_editable() {
    let p = parts("#table(columns: 1, [Cell equation $x^2 + y^2 = z^2$])");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:tbl>"), "the table stays native");
    assert!(slide.contains("<m:oMath>"), "cell math should remain native OMML");
    assert!(slide.contains("<m:sSup>"), "superscripts should retain structure");
    assert_all_wellformed(&p);
}

#[test]
fn table_cell_highlight_becomes_a_native_run_property() {
    let p = parts("#table(columns: 1, [Cell #highlight(fill: yellow)[Marked text]])");
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<a:tbl>"), "the table stays native");
    assert!(slide.contains("<a:highlight>"), "highlight should stay native");
    assert!(slide.contains("<a:t>Marked text</a:t>"));
    assert_eq!(
        slide.matches("<p:sp>").count(),
        0,
        "the detached highlight rectangle should be consumed"
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

#[test]
fn rotated_live_text_uses_rotation_neutral_bounds() {
    let p = parts(
        r#"#set page(width: 240pt, height: 160pt, margin: 10pt)
#rotate(90deg)[Rotated *live* text]"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let (x, y, cx, cy, rot) = text_box_transform(slide, "Rotated");
    assert_eq!(rot, 90 * 60_000, "keeps the editable DrawingML rotation");
    assert!(x >= 0 && y >= 0, "rotated text box must stay on-slide: ({x}, {y})");
    assert!(cx > cy, "the unrotated text box retains horizontal text extents");
    assert_eq!(
        text_shape_count(slide),
        1,
        "styled runs on one rotated line stay in one editable text box"
    );
    assert_all_wellformed(&p);
}

#[test]
fn mixed_page_sizes_fit_inside_the_global_slide_canvas() {
    let p = parts(
        r#"#set page(width: 240pt, height: 160pt, margin: 10pt)
First landscape page
#pagebreak()
#set page(width: 160pt, height: 240pt, margin: 10pt)
#place(bottom + right)[Second portrait page]"#,
    );
    let presentation = &p["ppt/presentation.xml"];
    assert!(
        presentation.contains("<p:sldSz cx=\"3048000\" cy=\"2032000\""),
        "the first page remains PowerPoint's one global slide size"
    );

    let (x, y, cx, cy, rot) =
        text_box_transform(&p["ppt/slides/slide2.xml"], "Second portrait");
    assert_eq!(rot, 0);
    assert!(x >= 0 && y >= 0, "fitted content begins inside the canvas");
    assert!(
        x + cx <= 3_048_000 && y + cy <= 2_032_000,
        "off-size page content must not be cropped: off=({x},{y}) ext=({cx},{cy})"
    );
    assert!(x > 0, "portrait page is centered with horizontal letterboxing");
    assert_all_wellformed(&p);
}

fn text_box_transform(slide: &str, needle: &str) -> (i64, i64, i64, i64, i32) {
    let doc = roxmltree::Document::parse(slide).unwrap();
    for shape in doc.descendants().filter(|node| node.tag_name().name() == "sp") {
        let has_text = shape.descendants().any(|node| {
            node.tag_name().name() == "t"
                && node.text().is_some_and(|text| text.contains(needle))
        });
        if has_text {
            let xfrm = shape
                .descendants()
                .find(|node| node.tag_name().name() == "xfrm")
                .expect("text box should have a:xfrm");
            let off = xfrm
                .children()
                .find(|node| node.tag_name().name() == "off")
                .expect("text box should have a:xfrm/a:off");
            let ext = xfrm
                .children()
                .find(|node| node.tag_name().name() == "ext")
                .expect("text box should have a:xfrm/a:ext");
            return (
                off.attribute("x").unwrap().parse().unwrap(),
                off.attribute("y").unwrap().parse().unwrap(),
                ext.attribute("cx").unwrap().parse().unwrap(),
                ext.attribute("cy").unwrap().parse().unwrap(),
                xfrm.attribute("rot").unwrap_or("0").parse().unwrap(),
            );
        }
    }
    panic!("missing text box for {needle}");
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

/// Collects every `<a:tcPr ...>...</a:tcPr>` block in document order.
fn tc_props(slide: &str) -> Vec<String> {
    let mut blocks = vec![];
    let mut rest = slide;
    while let Some(start) = rest.find("<a:tcPr") {
        let after = &rest[start..];
        let end = after.find("</a:tcPr>").map(|e| e + "</a:tcPr>".len());
        // A cell with no borders self-closes, so fall back to that form.
        let end = end.or_else(|| after.find("/>").map(|e| e + 2)).unwrap_or(after.len());
        blocks.push(after[..end].to_string());
        rest = &after[end..];
    }
    blocks
}

#[test]
fn table_cell_em_inset_uses_the_cell_font_size_not_a_default_chain() {
    // A font-relative inset must be measured against the font the cell is
    // actually set in. The cell-region tag carries the resolver's inset with
    // its `em` component already resolved under the cell's real style chain,
    // so 1em at 30pt is 30pt (381000 EMU).
    //
    // Before the region tag carried it, the exporter re-resolved the body
    // element's inset under `StyleChain::default()`, whose font size is the
    // 11pt default: both of the documents below emitted the *same*
    // `marL="139700"` (11pt), silently shrinking every cell margin in a deck
    // whose text is not 11pt.
    let big = parts(
        r#"#set page(width: 600pt, height: 400pt)
#set text(size: 30pt)
#table(columns: 2, inset: 1em, [a], [b], [c], [d])"#,
    );
    let big_slide = &big["ppt/slides/slide1.xml"];
    let big_props = tc_props(big_slide);
    assert_eq!(big_props.len(), 4, "four native cells");
    for block in &big_props {
        assert!(
            block.starts_with(
                "<a:tcPr marL=\"381000\" marT=\"381000\" marR=\"381000\" marB=\"381000\""
            ),
            "1em at 30pt text must be a 30pt cell margin, got {block}"
        );
    }

    // The default-size document is the control: it was already correct, and
    // must stay correct.
    let small = parts(
        r#"#set page(width: 600pt, height: 400pt)
#table(columns: 2, inset: 1em, [a], [b], [c], [d])"#,
    );
    let small_props = tc_props(&small["ppt/slides/slide1.xml"]);
    assert_eq!(small_props.len(), 4);
    for block in &small_props {
        assert!(
            block.starts_with(
                "<a:tcPr marL=\"139700\" marT=\"139700\" marR=\"139700\" marB=\"139700\""
            ),
            "1em at the 11pt default must stay an 11pt cell margin, got {block}"
        );
    }
    assert_all_wellformed(&big);
    assert_all_wellformed(&small);
}

#[test]
fn table_cell_fills_survive_every_resolver_route() {
    // Cell fill now comes from the resolved `Cell` carried on the region tag
    // rather than from re-reading the body element under a synthetic style
    // chain. Every route the grid resolver can produce a fill by must land in
    // the DrawingML cell: a table-level value, a `Celled` function, a `Celled`
    // array, a `#set table.cell` rule, and an explicit per-cell fill.
    let fill_of = |src: &str| -> Vec<Option<String>> {
        let p = parts(src);
        let slide = &p["ppt/slides/slide1.xml"];
        assert_all_wellformed(&p);
        tc_props(slide)
            .iter()
            .map(|block| {
                // The cell fill is the first child of `tcPr`; everything from
                // the first `<a:ln*>` onward is border paint, not cell paint.
                let body = &block[block.find('>')? + 1..];
                let body = &body[..body.find("<a:ln").unwrap_or(body.len())];
                let start = body.find("<a:solidFill><a:srgbClr val=\"")?;
                let rest = &body[start + "<a:solidFill><a:srgbClr val=\"".len()..];
                Some(rest[..6].to_string())
            })
            .collect()
    };

    let page = "#set page(width: 360pt, height: 200pt)\n";

    // A plain table-level fill reaches every cell.
    assert_eq!(
        fill_of(&format!("{page}#table(columns: 2, fill: red, [a], [b], [c], [d])")),
        vec![Some("FF4136".into()); 4],
    );

    // `Celled::Func` is evaluated per coordinate, so the columns alternate.
    assert_eq!(
        fill_of(&format!(
            "{page}#table(columns: 2, fill: (x, _) => if calc.even(x) {{ blue }} \
             else {{ green }}, [a], [b], [c], [d])"
        )),
        vec![
            Some("0074D9".into()),
            Some("2ECC40".into()),
            Some("0074D9".into()),
            Some("2ECC40".into()),
        ],
    );

    // `Celled::Array` cycles across the columns.
    assert_eq!(
        fill_of(&format!(
            "{page}#table(columns: 2, fill: (aqua, yellow), [a], [b], [c], [d])"
        )),
        vec![
            Some("7FDBFF".into()),
            Some("FFDC00".into()),
            Some("7FDBFF".into()),
            Some("FFDC00".into()),
        ],
    );

    // A `#set table.cell` rule reaches every cell.
    assert_eq!(
        fill_of(&format!(
            "{page}#set table.cell(fill: purple)\n\
             #table(columns: 2, [a], [b], [c], [d])"
        )),
        vec![Some("B10DC9".into()); 4],
    );

    // An explicit per-cell fill applies to exactly that cell; the rest stay
    // unfilled rather than inheriting it.
    assert_eq!(
        fill_of(&format!(
            "{page}#table(columns: 2, table.cell(fill: orange)[a], [b], [c], [d])"
        )),
        vec![Some("FF851B".into()), None, None, None],
    );
}

#[test]
fn table_level_stroke_and_alignment_survive_on_the_region_tag() {
    // Stroke and alignment also come off the region tag now. A table-level
    // stroke must reach every cell edge, and a `#set table.cell` alignment
    // must reach the cell's paragraph and body anchors.
    let p = parts(
        r#"#set page(width: 360pt, height: 200pt)
#set table.cell(align: right + bottom)
#table(
  columns: 2,
  stroke: 2pt + blue,
  [a], [b], [c], [d],
)"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    let props = tc_props(slide);
    assert_eq!(props.len(), 4, "four native cells");
    for block in &props {
        for side in ["lnL", "lnR", "lnT", "lnB"] {
            assert!(
                block.contains(&format!("<a:{side} w=\"25400\" cap=\"flat\">")),
                "2pt table stroke must reach {side}, got {block}"
            );
        }
        assert_eq!(
            block.matches("<a:srgbClr val=\"0074D9\"/>").count(),
            4,
            "every side keeps the authored blue, got {block}"
        );
    }
    assert_eq!(
        slide.matches("algn=\"r\"").count(),
        4,
        "the set rule's horizontal alignment reaches every cell paragraph"
    );
    assert_eq!(
        slide.matches("anchor=\"b\"").count(),
        4,
        "the set rule's vertical alignment reaches every cell body"
    );
    assert_all_wellformed(&p);
}

// ===========================================================================
// Math: exact OMML per construct.
//
// These are the reviewable artifact for lowering math through Typst's resolved
// `MathItem` IR (`typst-omml`, shared with the DOCX exporter) instead of
// walking unrealized content. The point of the exercise is that constructs the
// old walk flattened to concatenated text — matrices, vectors, cases,
// cancellation, over/underbraces, aligned rows — now have real OMML
// structures, so the fragments are asserted whole rather than probed for a
// substring: a whole fragment is what a reviewer can actually read, and it
// pins the child order OOXML requires.
// ===========================================================================

/// Compiles a one-equation slide and returns its `<m:oMath>` fragment.
fn omath(equation: &str) -> String {
    let p = parts(&format!(
        "#set page(width: 320pt, height: 180pt, margin: 12pt)\n{equation}"
    ));
    let slide = p["ppt/slides/slide1.xml"].clone();
    let start = slide.find("<m:oMath>").expect("slide should contain native OMML");
    let end = slide.find("</m:oMath>").expect("OMML should be closed")
        + "</m:oMath>".len();
    slide[start..end].to_owned()
}

#[test]
fn trivial_math_is_prestyled_plane_one_with_upright_runs() {
    // Typst applies math italics by *remapping* the letter to its Plane-1
    // math-alphanumeric codepoint (`a` → 𝑎 U+1D44E), so every run also carries
    // `m:nor` to stop Word slanting an already-slanted glyph a second time.
    assert_eq!(
        omath("$a + b = c$"),
        "<m:oMath>\
           <m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D44E}</m:t></m:r>\
           <m:r><m:rPr><m:nor/></m:rPr><m:t>+</m:t></m:r>\
           <m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D44F}</m:t></m:r>\
           <m:r><m:rPr><m:nor/></m:rPr><m:t>=</m:t></m:r>\
           <m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D450}</m:t></m:r>\
         </m:oMath>"
    );
}

#[test]
fn block_operator_takes_under_over_limits_and_an_integral_does_not() {
    // This is the assertion that guards the one non-obvious step in lowering a
    // *queried* equation: `EquationElem::size` is chain-only (`#[ghost]`), so
    // an element read back from the introspector has lost it and claims to be
    // inline. Re-applying the element's own show-set is what makes a display
    // sum put its bounds under and over the operator. `show_set` reads nothing
    // but `self.block` today; if that ever changes, this fails loudly instead
    // of silently moving every limit in every deck.
    let block_sum = omath("$ sum_(i=1)^n i $");
    assert!(
        block_sum.contains(r#"<m:limLoc m:val="undOvr"/>"#),
        "a display sum's bounds go under and over: {block_sum}"
    );

    // The same sum inline keeps its bounds beside the operator.
    let inline_sum = omath("$sum_(i=1)^n i$");
    assert!(
        inline_sum.contains(r#"<m:limLoc m:val="subSup"/>"#),
        "an inline sum's bounds stay beside it: {inline_sum}"
    );

    // An integral keeps sub/superscript limits even in display: that is the
    // typographic convention, and it proves `undOvr` above is not just the
    // constant every n-ary gets.
    let integral = omath("$ integral_0^1 x dif x $");
    assert!(
        integral.contains(r#"<m:limLoc m:val="subSup"/>"#),
        "an integral's bounds stay beside it: {integral}"
    );
}

#[test]
fn matrix_vector_and_cases_become_real_omml_matrices() {
    assert_eq!(
        omath("$ mat(a, b; c, d) $"),
        "<m:oMath><m:d><m:e>\
           <m:m>\
             <m:mPr><m:baseJc m:val=\"center\"/><m:plcHide m:val=\"on\"/>\
               <m:mcs><m:mc><m:mcPr>\
                 <m:count m:val=\"2\"/><m:mcJc m:val=\"center\"/>\
               </m:mcPr></m:mc></m:mcs>\
             </m:mPr>\
             <m:mr>\
               <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D44E}</m:t></m:r></m:e>\
               <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D44F}</m:t></m:r></m:e>\
             </m:mr>\
             <m:mr>\
               <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D450}</m:t></m:r></m:e>\
               <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D451}</m:t></m:r></m:e>\
             </m:mr>\
           </m:m>\
         </m:e></m:d></m:oMath>"
    );

    // A vector is the same structure with one column.
    let vector = omath("$ vec(a, b) $");
    assert!(vector.contains(r#"<m:count m:val="1"/>"#), "one column: {vector}");
    assert_eq!(vector.matches("<m:mr>").count(), 2, "two rows: {vector}");

    // `cases` differs only in its fence: a left brace and no right delimiter.
    let cases = omath("$ cases(a, b) $");
    assert!(
        cases.contains(r#"<m:begChr m:val="{"/><m:endChr m:val=""/>"#),
        "a one-sided brace fence: {cases}"
    );
    assert!(cases.contains("<m:m>"), "cases is a matrix body: {cases}");
}

#[test]
fn cancel_becomes_a_struck_border_box() {
    assert_eq!(
        omath("$ cancel(x) $"),
        "<m:oMath>\
           <m:borderBox>\
             <m:borderBoxPr>\
               <m:hideTop m:val=\"on\"/><m:hideBot m:val=\"on\"/>\
               <m:hideLeft m:val=\"on\"/><m:hideRight m:val=\"on\"/>\
               <m:strikeBLTR m:val=\"on\"/>\
             </m:borderBoxPr>\
             <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D465}</m:t></m:r></m:e>\
           </m:borderBox>\
         </m:oMath>"
    );
}

#[test]
fn braces_stretch_as_group_chars_with_their_annotation_as_a_limit() {
    assert_eq!(
        omath("$ underbrace(x, n) $"),
        "<m:oMath>\
           <m:limLow>\
             <m:e><m:groupChr>\
               <m:groupChrPr>\
                 <m:chr m:val=\"\u{23DF}\"/><m:pos m:val=\"bot\"/>\
                 <m:vertJc m:val=\"top\"/>\
               </m:groupChrPr>\
               <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D465}</m:t></m:r></m:e>\
             </m:groupChr></m:e>\
             <m:lim><m:r><m:rPr><m:nor/></m:rPr><m:t>\u{1D45B}</m:t></m:r></m:lim>\
           </m:limLow>\
         </m:oMath>"
    );

    let over = omath("$ overbrace(x, n) $");
    assert!(over.contains("<m:limUpp>"), "the annotation goes above: {over}");
    assert!(
        over.contains(r#"<m:chr m:val="&#x23DE;"/><m:pos m:val="top"/>"#)
            || over.contains("<m:chr m:val=\"\u{23DE}\"/><m:pos m:val=\"top\"/>"),
        "an over-brace grouping char: {over}"
    );
}

#[test]
fn aligned_rows_become_a_gapless_two_column_matrix() {
    // `m:eqArr` cannot express per-column alignment, so an aligned body is a
    // borderless matrix whose columns alternate right/left with no column gap
    // — the same thing Word's own aligned equations do.
    let aligned = omath(r"$ a &= b \ c &= d $");
    assert!(
        aligned.contains(
            "<m:mc><m:mcPr><m:count m:val=\"1\"/><m:mcJc m:val=\"right\"/></m:mcPr>\
             </m:mc><m:mc><m:mcPr><m:count m:val=\"1\"/><m:mcJc m:val=\"left\"/>\
             </m:mcPr></m:mc>"
        ),
        "columns alternate right then left: {aligned}"
    );
    assert!(
        aligned.contains(r#"<m:cGp m:val="0"/>"#),
        "no gap at the alignment point: {aligned}"
    );
}

#[test]
fn slide_math_never_carries_wordprocessing_markup() {
    // OMML has no colour of its own and borrows the host format's run
    // properties. Word's are `w:rPr`/`w:color`; a slide has no `w:` prefix
    // declared at all, so PowerPoint's answer to that hook is "no colour"
    // rather than a foreign element PowerPoint merely tolerates.
    let p = parts(
        r#"#set page(width: 320pt, height: 180pt, margin: 12pt)
$ sum_(i=1)^n #text(red)[x] = #text(blue)[y] $"#,
    );
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(slide.contains("<m:oMath>"), "the equation is still native OMML");
    assert!(!slide.contains("w:"), "no Wordprocessing markup in a slide part");
    assert_all_wellformed(&p);
}

#[test]
fn math_fallback_text_reads_back_the_new_structures() {
    // The compatibility branch is what LibreOffice Impress shows. It must
    // survive the richer OMML: matrices need separators, aligned rows need to
    // stay separate lines, and the Plane-1 codepoints have to come back to
    // ASCII, because the fallback is an ordinary text run in whatever font the
    // consumer resolves and most have no Plane-1 math coverage.
    fn fallback(equation: &str) -> String {
        let p = parts(&format!(
            "#set page(width: 320pt, height: 180pt, margin: 12pt)\n{equation}"
        ));
        let slide = &p["ppt/slides/slide1.xml"];
        let doc = roxmltree::Document::parse(slide).unwrap();
        doc.descendants()
            .find(|node| {
                node.tag_name().name() == "Fallback"
                    && node.tag_name().namespace()
                        == Some("http://schemas.openxmlformats.org/markup-compatibility/2006")
            })
            .expect("math should have a compatibility fallback")
            .descendants()
            .filter(|node| node.tag_name().name() == "t")
            .filter_map(|node| node.text())
            .collect()
    }

    assert_eq!(fallback("$a + b = c$"), "a+b=c");
    assert_eq!(fallback("$ mat(a, b; c, d) $"), "(a, b; c, d)");
    assert_eq!(fallback(r"$ a &= b \ c &= d $"), "a=b; c=d");
    assert_eq!(fallback("$ underbrace(x, n) $"), "⏟x\u{2099}");
    assert_eq!(fallback("$ cancel(x) $"), "x");
}

#[test]
fn math_with_an_inline_box_falls_back_to_painted_text_and_says_so() {
    // A `box(..)` inside math is arbitrary laid-out content: OMML has no form
    // for it, and a native subtree may not simply omit one child, because that
    // changes the equation while still looking plausible. So the whole
    // equation is refused — and refusing it means *not* starting a math box,
    // which leaves the equation's own glyphs in the frame walk to be painted
    // as ordinary runs. The content survives; only its mathness does not.
    let world = TestWorld::new(
        r#"#set page(width: 320pt, height: 180pt, margin: 12pt)
$ x = #box(width: 20pt, rect()) + y $"#,
    );
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .expect("compilation failed");
    let export = typst_pptx::pptx_with_report(&doc, &world, &PptxOptions::default())
        .expect("pptx export failed");

    let decision = export
        .fidelity_report()
        .decisions()
        .iter()
        .find(|decision| {
            decision.reason == typst_pptx::DecisionReason::UnsupportedMathTextFallback
        })
        .expect("the refusal should be recorded, not silent");
    assert_eq!(decision.representation, typst_pptx::Representation::Approximate);
    assert!(decision.losses.semantic_structure && decision.losses.editability);

    let p = text_parts(export.into_bytes());
    let slide = &p["ppt/slides/slide1.xml"];
    assert!(!slide.contains("<m:oMath"), "no half-native equation is emitted");
    assert!(slide.contains("<a:t>"), "the equation is still painted as text");
    assert_all_wellformed(&p);
}

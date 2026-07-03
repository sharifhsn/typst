//! Structural and well-formedness tests for the PPTX exporter foundation.

use std::collections::HashMap;
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
    pptx(&doc, &PptxOptions {}).expect("pptx export failed")
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
    assert!(slide.contains("r:id=\"rId"));
    assert!(rels.contains("relationships/hyperlink"));
    assert!(rels.contains("Target=\"https://example.com/\""));
    assert!(rels.contains("TargetMode=\"External\""));
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

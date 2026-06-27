//! Structural and well-formedness tests for the DOCX exporter.
//!
//! These compile small Typst snippets through the real pipeline
//! (`typst::compile::<DocxDocument>` + [`typst_docx::docx`]) and assert on the
//! produced OPC package: every part is namespace-well-formed (parsed with the
//! namespace-aware `roxmltree`, which rejects an undeclared prefix — the class
//! of bug that makes Word/LibreOffice refuse to open a file), plus targeted
//! checks on the structural mappings.

use std::collections::HashMap;
use std::io::Read;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_docx::{DocxDocument, DocxOptions, docx};

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

/// Compiles `src` to a DOCX and returns its parts as `name -> text`.
fn parts(src: &str) -> HashMap<String, String> {
    let world = TestWorld::new(src);
    let doc = typst::compile::<DocxDocument>(&world)
        .output
        .expect("compilation failed");
    let bytes = docx(&doc, &DocxOptions { pretty: false }).expect("docx export failed");

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

/// Parses every XML part with the namespace-aware parser, asserting that no
/// part uses an undeclared namespace prefix.
fn assert_all_wellformed(parts: &HashMap<String, String>) {
    for (name, xml) in parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            roxmltree::Document::parse(xml)
                .unwrap_or_else(|e| panic!("{name} is not namespace-well-formed: {e}"));
        }
    }
}

#[test]
fn package_is_wellformed_and_minimal() {
    let p = parts("Hello *world*.");
    assert!(p.contains_key("[Content_Types].xml"));
    assert!(p.contains_key("word/document.xml"));
    assert!(p.contains_key("_rels/.rels"));
    assert_all_wellformed(&p);
}

#[test]
fn heading_maps_to_heading_style() {
    let p = parts("= Introduction\n\nBody text.");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("w:pStyle"), "heading should carry a paragraph style");
    assert!(doc.contains("Heading"), "heading style id should be Heading*");
    assert_all_wellformed(&p);
}

#[test]
fn strong_maps_to_bold_run() {
    let p = parts("Normal *bold* text.");
    assert!(p["word/document.xml"].contains("<w:b/>"), "strong should emit <w:b/>");
    assert_all_wellformed(&p);
}

#[test]
fn table_maps_to_wtbl() {
    let p = parts("#table(columns: 2, [a], [b], [c], [d])");
    assert!(p["word/document.xml"].contains("<w:tbl>"), "table should emit <w:tbl>");
    assert_all_wellformed(&p);
}

#[test]
fn math_maps_to_omml() {
    let p = parts("$ x^2 + y^2 = z^2 $");
    assert!(
        p["word/document.xml"].contains("m:oMath"),
        "math should emit OMML (m:oMath), not an image"
    );
    assert_all_wellformed(&p);
}

#[test]
fn footnote_has_in_text_reference_and_body_mark() {
    let p = parts("A claim.#footnote[The supporting note.]");
    assert!(p.contains_key("word/footnotes.xml"), "footnotes part should exist");
    assert!(
        p["word/document.xml"].contains("w:footnoteReference"),
        "the in-text mark should be a w:footnoteReference"
    );
    assert!(
        p["word/footnotes.xml"].contains("w:footnoteRef"),
        "the footnote body should carry the in-body number mark w:footnoteRef"
    );
    assert_all_wellformed(&p);
}

#[test]
fn figure_emits_seq_field() {
    let p = parts(
        "#figure(rect(width: 20pt, height: 20pt), caption: [A box]) <f>\n\nSee @f.",
    );
    assert!(
        p["word/document.xml"].contains("SEQ Figure"),
        "a captioned figure should number via a SEQ field"
    );
    assert_all_wellformed(&p);
}

#[test]
fn image_in_header_declares_drawing_namespaces() {
    // An image in a header part used to leave `wp:`/`a:`/`pic:` undeclared on
    // the header root, making Word/LibreOffice refuse to open the document.
    let p = parts(
        "#set page(header: box(fill: blue, width: 30pt, height: 8pt))\n\nBody.",
    );
    let header = p
        .iter()
        .find(|(n, _)| n.starts_with("word/header"))
        .map(|(_, x)| x)
        .expect("a header part should exist");
    assert!(header.contains("xmlns:wp"), "header root must declare xmlns:wp");
    // The namespace-aware parse is the real guard against the regression.
    assert_all_wellformed(&p);
}

#[test]
fn rasterized_container_keeps_figure_count() {
    // A figure whose container is rasterized (here a `box`, which has no native
    // OOXML form) must still increment Word's figure counter via a hidden
    // `SEQ ... \h`, or caption/cross-reference numbers drift apart.
    let p = parts(
        "#figure(rect(width: 10pt, height: 10pt), caption: [First]) <a>\n\n\
         #box(figure(rect(width: 10pt, height: 10pt), caption: [Boxed])) <b>\n\n\
         #figure(rect(width: 10pt, height: 10pt), caption: [Third]) <c>",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("SEQ Figure"), "figures should number via SEQ");
    assert!(
        doc.contains("\\h"),
        "the rasterized figure should emit a hidden SEQ (\\h) so the count stays consistent"
    );
    assert_all_wellformed(&p);
}

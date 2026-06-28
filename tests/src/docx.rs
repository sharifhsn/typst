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
fn nary_operator_nests_its_operand() {
    // The integrand must sit inside the n-ary's `m:e`, not after an empty one
    // (an empty `<m:e/>` renders as a spurious box).
    let p = parts("$ integral_0^1 x dif x = 1 $");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("m:nary"), "an integral should be an n-ary operator");
    assert!(
        !doc.contains("<m:e/></m:nary>"),
        "the n-ary `m:e` must hold the operand, not be empty"
    );
    assert_all_wellformed(&p);
}

#[test]
fn outline_bakes_entries_with_resolvable_bookmarks() {
    // A heading table of contents bakes its entries (so it shows without a
    // manual field update), and every entry's PAGEREF must target a real
    // bookmark — a dangling one renders as "Error! Bookmark not defined".
    let p = parts("#outline()\n\n= Alpha\n\n== Beta\n\n= Gamma");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("w:val=\"TOC1\""), "a heading TOC bakes TOC1 entries");
    assert!(doc.contains("w:val=\"TOC2\""), "nested headings bake TOC2 entries");

    let bookmarks: Vec<&str> = doc
        .match_indices("w:name=\"")
        .map(|(i, _)| {
            let rest = &doc[i + 8..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect();
    for (i, _) in doc.match_indices("PAGEREF ") {
        let rest = &doc[i + 8..];
        let name = &rest[..rest.find(' ').unwrap()];
        assert!(
            bookmarks.contains(&name),
            "PAGEREF target {name} has no matching bookmark (dangling)"
        );
    }
    assert_all_wellformed(&p);
}

#[test]
fn inline_equation_stays_in_its_paragraph() {
    // Typst splits a paragraph containing an inline equation into
    // `[par, equation, par]`; the exporter must rejoin them, or the equation
    // (and the text after it) breaks onto separate lines.
    let p = parts("Before the equation $x^2 + y^2$ and text after it.");
    let doc = &p["word/document.xml"];
    let para = doc
        .split("<w:p>")
        .find(|p| p.contains("Before the equation"))
        .expect("a paragraph with the text");
    let para = &para[..para.find("</w:p>").unwrap()];
    assert!(para.contains("m:oMath"), "the inline equation shares the text's paragraph");
    assert!(para.contains("text after it"), "text after the equation stays in the paragraph");
    assert_all_wellformed(&p);
}

#[test]
fn list_of_figures_bakes_caption_entries() {
    // `outline(target: figure.where(kind: image))` becomes a list of figures
    // that bakes one entry per captioned figure (matched by category), so it
    // shows without a field update.
    let p = parts(
        "#outline(target: figure.where(kind: image))\n\n\
         #figure(rect(), caption: [First picture]) <a>\n\n\
         #figure(rect(), caption: [Second picture]) <b>",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("w:val=\"TOC1\""), "the list of figures bakes entries");
    assert!(doc.contains("First picture"), "the caption text appears in the list");
    assert!(doc.contains("Second picture"), "every captioned figure is listed");
    assert_all_wellformed(&p);
}

#[test]
fn outline_falls_back_to_introspected_headings() {
    // When headings are show-ruled away there is no native heading paragraph and
    // nothing is recorded, but the introspector still holds them — the TOC
    // populates from there (plain text, since there is no bookmark to target).
    let p = parts("#show heading: it => block(it.body)\n#outline()\n\n= Alpha\n\n= Beta");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("w:val=\"TOC1\""),
        "the TOC populates from introspected headings"
    );
    assert!(doc.contains("Alpha"), "the heading title appears in the TOC");
    // No bookmark to target, so no PAGEREF and nothing to dangle.
    assert!(!doc.contains("PAGEREF"), "fallback entries carry no PAGEREF");
    assert_all_wellformed(&p);
}

#[test]
fn pad_extracts_its_text_as_real_runs() {
    // The rasterize-vs-extract decision: a plain `#pad` body has no
    // layout-produced introspection, so it is extracted as real, indented text
    // rather than rasterized to an image.
    let p = parts("#pad(left: 2em)[A padded paragraph of real text.]");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("A padded paragraph of real text"), "the text is extracted");
    assert!(doc.contains("w:ind"), "the padding becomes a paragraph indent");
    assert!(!doc.contains("a:blip"), "and it is not a rasterized image");
    assert_all_wellformed(&p);
}

#[test]
fn decorative_shape_becomes_a_vector_drawing() {
    // A `#rect`/`#circle`/… with an explicit size and no body maps to a vector
    // DrawingML shape (`wps:wsp`), not a rasterized image.
    let p = parts("#rect(width: 2cm, height: 1cm, fill: blue, stroke: 1pt + red)");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("wps:wsp"), "the rect is a vector shape");
    assert!(doc.contains("prst=\"rect\""), "with rectangle preset geometry");
    assert!(!doc.contains("a:blip"), "and is not an embedded raster image");
    assert!(doc.contains("a:solidFill"), "the solid fill is carried");
    assert_all_wellformed(&p);
}

#[test]
fn polygon_becomes_a_custom_geometry_shape() {
    let p = parts("#polygon((0pt, 0pt), (2cm, 0pt), (1cm, 1cm), fill: green)");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("a:custGeom"), "a polygon uses a custom path geometry");
    assert!(doc.contains("a:close"), "the polygon path is closed");
    assert_all_wellformed(&p);
}

#[test]
fn term_list_merges_term_and_definition() {
    // Typst renders "**term** definition" inline with a hanging indent, not the
    // term on its own line.
    let p = parts("/ Term: the definition of it.");
    let doc = &p["word/document.xml"];
    let para = doc
        .split("<w:p>")
        .find(|p| p.contains("Term"))
        .expect("a paragraph with the term");
    let para = &para[..para.find("</w:p>").unwrap()];
    assert!(para.contains("<w:b/>"), "the term is bold");
    assert!(para.contains("the definition of it"), "the definition shares the paragraph");
    assert!(para.contains("w:hanging"), "the entry uses a hanging indent");
    assert_all_wellformed(&p);
}

#[test]
fn block_quote_keeps_attribution_and_indent() {
    let p = parts(
        "#quote(block: true, attribution: [Albert Einstein])[Imagination matters.]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Albert Einstein"), "the attribution must not be dropped");
    assert!(doc.contains("w:ind"), "a block quote is indented");
    assert!(doc.contains("w:jc w:val=\"end\""), "the attribution is right-aligned");
    assert_all_wellformed(&p);
}

#[test]
fn fractional_h_pushes_to_the_right_margin() {
    // `#h(1fr)` is the "Left … Right" push-apart idiom: a tab plus a
    // right-aligned tab stop, rather than being dropped.
    let p = parts("Left #h(1fr) Right");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:tab/>"), "the fractional space becomes a tab");
    assert!(
        doc.contains("w:val=\"end\""),
        "the paragraph gains a right-aligned tab stop"
    );
    assert_all_wellformed(&p);
}

#[test]
fn horizontal_line_becomes_a_rule() {
    // `#line(length: 100%)` (a divider) → a bottom-bordered paragraph, not dropped.
    let p = parts("Above\n\n#line(length: 100%)\n\nBelow");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("w:pBdr"), "a horizontal line becomes a paragraph border");
    assert!(doc.contains("w:bottom"), "the rule is a bottom border");
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
fn page_geometry_change_emits_a_section_break() {
    // A mid-document orientation change must produce a second section: the
    // landscape `sectPr` lives in a paragraph's `pPr`, the final portrait one
    // at body level.
    let p = parts(
        "Portrait body.\n\n#set page(flipped: true)\n\nLandscape body.",
    );
    let doc = &p["word/document.xml"];
    assert_eq!(
        doc.matches("<w:sectPr>").count(),
        2,
        "an orientation change should yield two sections"
    );
    assert!(
        doc.contains("w:orient=\"landscape\""),
        "the flipped section should be landscape"
    );
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

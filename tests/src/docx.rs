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
use std::sync::Arc;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration, Label};
use typst::introspection::Introspector;
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::{LazyHash, PicoStr};
use typst::{Library, LibraryExt, World};
use typst_docx::{DocxDocument, DocxOptions, docx};
use typst_layout::PagedDocument;

/// A minimal world: the embedded Typst fonts and a single detached source.
struct TestWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: Source,
    files: HashMap<FileId, Bytes>,
}

impl TestWorld {
    fn new(text: &str) -> Self {
        Self::with_files(text, &[])
    }

    /// Serves the given `(path, bytes)` pairs as files resolvable from the
    /// detached main source (for example, `bibliography("refs.bib")`).
    fn with_files(text: &str, files: &[(&str, &[u8])]) -> Self {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let book = FontBook::from_fonts(&fonts);
        let main = Source::detached(text);
        let files = files
            .iter()
            .map(|(path, bytes)| {
                let id = typst::syntax::RootedPath::new(
                    typst::syntax::VirtualRoot::Project,
                    typst::syntax::VirtualPath::new(path).unwrap(),
                )
                .intern();
                (id, Bytes::new(bytes.to_vec()))
            })
            .collect();
        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            main,
            files,
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
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files
            .get(&id)
            .cloned()
            .ok_or_else(|| FileError::NotFound(Default::default()))
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
    parts_with_files(src, &[])
}

fn parts_with_files(src: &str, files: &[(&str, &[u8])]) -> HashMap<String, String> {
    package_bytes_with_files(src, files)
        .into_iter()
        .filter_map(|(name, bytes)| String::from_utf8(bytes).ok().map(|s| (name, s)))
        .collect()
}

fn package_bytes_with_files(
    src: &str,
    files: &[(&str, &[u8])],
) -> HashMap<String, Vec<u8>> {
    let doc = compile_docx(src, files);
    let bytes = docx(&doc, &DocxOptions { pretty: false }).expect("docx export failed");

    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut map = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).unwrap();
        map.insert(name, bytes);
    }
    map
}

fn compile_docx(src: &str, files: &[(&str, &[u8])]) -> DocxDocument {
    let world = TestWorld::with_files(src, files);
    compile_docx_with_world(&world)
}

fn compile_docx_with_world(world: &TestWorld) -> DocxDocument {
    let paged = typst::compile::<PagedDocument>(world)
        .output
        .expect("paged compilation failed");
    let primary = Arc::clone(paged.introspector());
    let seed = Arc::clone(&primary);
    typst::compile_with::<DocxDocument, _>(
        world,
        Some(seed.as_ref()),
        move |engine, content, styles| {
            typst_docx::docx_document_with_paged_introspector(
                engine,
                content,
                styles,
                Arc::clone(&primary),
            )
        },
    )
    .output
    .expect("docx compilation failed")
}

fn compile_paged_and_docx(src: &str) -> (PagedDocument, DocxDocument) {
    let world = TestWorld::new(src);
    let paged = typst::compile::<PagedDocument>(&world)
        .output
        .expect("paged compilation failed");
    let primary = Arc::clone(paged.introspector());
    let seed = Arc::clone(&primary);
    let doc = typst::compile_with::<DocxDocument, _>(
        &world,
        Some(seed.as_ref()),
        move |engine, content, styles| {
            typst_docx::docx_document_with_paged_introspector(
                engine,
                content,
                styles,
                Arc::clone(&primary),
            )
        },
    )
    .output
    .expect("docx compilation failed");
    (paged, doc)
}

/// Parses every XML part with the namespace-aware parser, asserting that no
/// part uses an undeclared namespace prefix.
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

fn visible_text(xml: &str) -> String {
    let doc = roxmltree::Document::parse(xml).expect("document XML should parse");
    doc.descendants()
        .filter(|node| {
            node.tag_name().name() == "t"
                && node.tag_name().namespace()
                    == Some(
                        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                    )
        })
        .filter_map(|node| node.text())
        .collect()
}

fn sect_pr_chunks(doc: &str) -> Vec<&str> {
    let positions: Vec<_> = doc.match_indices("<w:sectPr>").map(|(pos, _)| pos).collect();
    positions
        .iter()
        .enumerate()
        .map(|(idx, start)| {
            let end = positions.get(idx + 1).copied().unwrap_or(doc.len());
            &doc[*start..end]
        })
        .collect()
}

const REFS_BIB: &[u8] = br#"@article{alpha,
  title = {Alpha Source},
  author = {Able, Alice},
  year = {2020},
  journal = {Journal of Sources},
}

@article{beta,
  title = {Beta Source},
  author = {Baker, Bob},
  year = {2021},
  journal = {Journal of Sources},
}
"#;

/// Same as `REFS_BIB`, plus an entry ("gamma") that no test document below
/// ever cites — used to verify uncited library entries stay out of the
/// native Word sources part.
const REFS_BIB_WITH_UNCITED: &[u8] = br#"@article{alpha,
  title = {Alpha Source},
  author = {Able, Alice},
  year = {2020},
  journal = {Journal of Sources},
}

@article{beta,
  title = {Beta Source},
  author = {Baker, Bob},
  year = {2021},
  journal = {Journal of Sources},
}

@article{gamma,
  title = {Gamma Source},
  author = {Carter, Cara},
  year = {2022},
  journal = {Journal of Sources},
}
"#;

fn run_fragment_containing<'a>(doc_xml: &'a str, text: &str) -> &'a str {
    for frag in doc_xml.split("<w:r>").skip(1) {
        let Some(end) = frag.find("</w:r>") else { continue };
        let run = &frag[..end];
        if run.contains(text) {
            return run;
        }
    }
    panic!("run containing {text:?} not found");
}

fn para_fragment_containing<'a>(doc_xml: &'a str, text: &str) -> &'a str {
    let text_at = doc_xml.find(text).unwrap_or_else(|| panic!("{text:?} not found"));
    let start = doc_xml[..text_at].rfind("<w:p").expect("paragraph start");
    let end = text_at + doc_xml[text_at..].find("</w:p>").expect("paragraph end");
    &doc_xml[start..end]
}

fn style_fragment<'a>(styles_xml: &'a str, style_id: &str) -> &'a str {
    let marker = format!("w:styleId=\"{style_id}\"");
    let at = styles_xml
        .find(&marker)
        .unwrap_or_else(|| panic!("style {style_id} not found"));
    let start = styles_xml[..at].rfind("<w:style").expect("style start");
    let end =
        at + styles_xml[at..].find("</w:style>").expect("style end") + "</w:style>".len();
    &styles_xml[start..end]
}

fn run_text_and_child(doc_xml: &str, child: &str) -> Vec<(String, bool)> {
    let doc = roxmltree::Document::parse(doc_xml).expect("document XML should parse");
    doc.descendants()
        .filter(|node| {
            node.tag_name().name() == "r"
                && node.tag_name().namespace()
                    == Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")
        })
        .map(|run| {
            let text = run
                .descendants()
                .filter(|node| {
                    node.tag_name().name() == "t"
                        && node.tag_name().namespace()
                            == Some(
                                "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                            )
                })
                .filter_map(|node| node.text())
                .collect();
            let has_child = run.descendants().any(|node| {
                node.tag_name().name() == child
                    && node.tag_name().namespace()
                        == Some("http://schemas.openxmlformats.org/wordprocessingml/2006/main")
            });
            (text, has_child)
        })
        .collect()
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
fn strong_maps_to_strong_character_style() {
    let p = parts("Normal *bold* text.");
    let run = run_fragment_containing(&p["word/document.xml"], "bold");
    assert!(
        run.contains("<w:rStyle w:val=\"Strong\"/>"),
        "strong should emit the Strong character style: {run}"
    );
    assert!(
        !run.contains("<w:b"),
        "Strong style should supply bold without direct formatting: {run}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn raw_code_runs_disable_proofing_but_prose_does_not() {
    let p = parts(
        r#"Plain prose before `let inline_code = 1;` after.

```rust
let block_code = 2;
```
"#,
    );
    let doc = &p["word/document.xml"];
    let runs = run_text_and_child(doc, "noProof");

    assert!(
        runs.iter()
            .any(|(text, no_proof)| text.contains("inline_code") && *no_proof),
        "inline raw code should emit <w:noProof/>"
    );
    assert!(
        runs.iter()
            .any(|(text, no_proof)| text.contains("block_code") && *no_proof),
        "block raw code should emit <w:noProof/>"
    );
    assert!(
        runs.iter()
            .any(|(text, no_proof)| text.contains("Plain prose") && !*no_proof),
        "ordinary prose should not emit <w:noProof/>"
    );
    assert_all_wellformed(&p);
}

#[test]
fn highlight_default_uses_word_highlight_yellow() {
    let p = parts("#highlight[x]");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("<w:highlight w:val=\"yellow\"/>"),
        "default Typst highlight should map to Word's yellow highlighter"
    );
    assert_all_wellformed(&p);
}

#[test]
fn highlight_custom_green_is_not_hard_coded_yellow() {
    let p = parts("#highlight(fill: rgb(\"00FF00\"))[x]");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("<w:highlight w:val=\"green\"/>")
            || doc.contains("w:fill=\"00FF00\""),
        "custom green highlight should be emitted as green"
    );
    assert!(
        !doc.contains("w:fill=\"FFFF00\"") && !doc.contains("w:val=\"yellow\""),
        "custom green highlight must not fall back to the old hard-coded yellow"
    );
    assert_all_wellformed(&p);
}

#[test]
fn highlight_arbitrary_color_keeps_exact_shading() {
    let p = parts("#highlight(fill: rgb(\"123456\"))[x]");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"123456\"/>"),
        "arbitrary highlight colors should keep exact run shading"
    );
    assert!(
        !doc.contains("<w:highlight"),
        "arbitrary highlight colors should not be forced into Word's named palette"
    );
    assert_all_wellformed(&p);
}

#[test]
fn table_maps_to_wtbl() {
    let p = parts("#table(columns: 2, [a], [b], [c], [d])");
    assert!(p["word/document.xml"].contains("<w:tbl>"), "table should emit <w:tbl>");
    assert_all_wellformed(&p);
}

#[test]
fn stroke_none_table_has_no_cell_borders() {
    // `stroke: none` must turn borders OFF — every cell side becomes an explicit
    // `w:val="nil"` (not left to inherit the table's default border).
    let p = parts("#table(columns: 2, stroke: none, [a], [b], [c], [d])");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:tcBorders>"), "a borderless cell still emits tcBorders");
    assert!(doc.contains("w:val=\"nil\""), "with sides turned off (nil)");
    // A normal table keeps visible borders.
    let normal = parts("#table(columns: 2, [a], [b])");
    assert!(
        normal["word/document.xml"].contains("<w:top w:val=\"single\""),
        "a default table keeps single borders"
    );
    assert_all_wellformed(&p);
}

#[test]
fn curve_maps_to_a_native_bezier_path() {
    // `#curve` (straight + cubic-Bézier segments) must map to a native
    // `a:custGeom` path with real `a:cubicBezTo` commands — not rasterize —
    // when its fill/stroke are solid colours.
    let p = parts(
        "#curve(\
           fill: blue, \
           curve.move((0pt, 50pt)), \
           curve.line((100pt, 50pt)), \
           curve.cubic(none, (90pt, 0pt), (50pt, 0pt)), \
           curve.close(), \
         )",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<a:custGeom>"), "curve becomes a custom geometry");
    assert!(doc.contains("<a:cubicBezTo>"), "the Bézier segment is kept, not flattened");
    assert!(
        !doc.contains("<w:drawing><wp:inline") || !doc.contains("<a:blip"),
        "not rasterized"
    );
    assert_all_wellformed(&p);
}

#[test]
fn diagonal_line_maps_to_a_native_shape() {
    // A diagonal (or explicit-endpoint) `#line` has no paragraph-border form
    // (that's reserved for the horizontal-rule idiom) — it must become a
    // native open path instead of a rasterized image.
    let p = parts("#line(start: (0pt, 0pt), end: (80pt, 40pt), stroke: 2pt + red)");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<a:custGeom>"), "a diagonal line becomes a custom geometry");
    assert!(!doc.contains("<a:blip"), "not rasterized to an image");
    assert_all_wellformed(&p);

    // A horizontal line keeps its existing (nicer) paragraph-border mapping,
    // unaffected by the new diagonal-line path.
    let h = parts("#line(length: 100%)");
    assert!(
        h["word/document.xml"].contains("<w:pBdr>"),
        "a horizontal line still becomes a paragraph border"
    );
}

#[test]
fn linear_gradient_fill_maps_to_native_gradfill() {
    // A linear-gradient fill must become a native `a:gradFill`/`a:lin`, with
    // stops converted to sRGB hex — NOT rasterize. Gradient stops are stored in
    // the gradient's own interpolation space (Oklab by default), so reading
    // their raw components verbatim (skipping the sRGB conversion) silently
    // produces the wrong colour; this guards that regression directly with an
    // exact hex match.
    let p = parts(
        "#rect(width: 100pt, height: 50pt, fill: gradient.linear(rgb(\"#ff0000\"), rgb(\"#0000ff\")))",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<a:gradFill"), "gradient fill becomes a:gradFill");
    assert!(doc.contains("<a:srgbClr val=\"FF0000\"/>"), "first stop is exact red");
    assert!(
        doc.contains("<a:srgbClr val=\"0000FF\"/>"),
        "second stop is exact blue, not Oklab-misread"
    );
    assert!(doc.contains("<a:lin ang=\"0\""), "0deg (left-to-right) maps to ang=0");
    assert!(!doc.contains("<a:blip"), "not rasterized");

    // A vertical (90deg) gradient maps to the OOXML angle convention (60,000ths
    // of a degree, clockwise from left-to-right).
    let v = parts(
        "#rect(width: 100pt, height: 50pt, fill: gradient.linear(angle: 90deg, red, blue))",
    );
    assert!(
        v["word/document.xml"].contains("<a:lin ang=\"5400000\""),
        "90deg maps to ang=5400000"
    );

    // A radial gradient has no representable OOXML shape-relative form here and
    // still rasterizes, same as before. (An empirical LibreOffice check found
    // the emitted a:path/a:fillToRect renders visibly more circular than
    // Typst's own box-relative elliptical stretch on a non-square shape — the
    // exact mismatch this comment originally warned about — so it stays
    // scoped out rather than ship a subtly-wrong native mapping.)
    let r = parts("#rect(width: 100pt, height: 50pt, fill: gradient.radial(red, blue))");
    assert!(
        !r["word/document.xml"].contains("<a:gradFill"),
        "radial gradients are not (yet) natively mapped"
    );
    assert_all_wellformed(&p);
}

#[test]
fn shape_stroke_dash_and_cap_are_carried_natively() {
    // A shape's stroke dash pattern and line cap must reach the native
    // `a:ln`'s `cap` attribute and `a:prstDash` child, not silently flatten to
    // a plain solid line.
    let p = parts(
        "#rect(width: 100pt, height: 40pt, \
           stroke: (paint: red, thickness: 2pt, dash: \"dashed\", cap: \"round\"))",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("cap=\"rnd\""), "round cap maps to rnd");
    assert!(doc.contains("<a:prstDash val=\"dash\"/>"), "dashed maps to the dash preset");

    // Non-horizontal, so it maps to a native shape (a horizontal line keeps
    // its existing paragraph-border mapping, which has no `cap` concept).
    let dotted = parts(
        "#line(length: 100pt, angle: 20deg, stroke: (paint: blue, thickness: 1pt, dash: \"dotted\", cap: \"square\"))",
    );
    let doc2 = &dotted["word/document.xml"];
    assert!(doc2.contains("cap=\"sq\""), "square cap maps to sq");
    assert!(doc2.contains("<a:prstDash val=\"sysDot\"/>"), "dotted maps to a dot preset");

    // A plain solid stroke still carries an explicit cap but no prstDash.
    let solid = parts("#rect(width: 100pt, height: 40pt, stroke: black)");
    assert!(
        !solid["word/document.xml"].contains("<a:prstDash"),
        "solid line has no dash element"
    );
    assert_all_wellformed(&p);
}

#[test]
fn rasterized_content_keeps_its_text_as_hidden_runs() {
    // Content with no native mapping (here `#skew`) still rasterizes to an image,
    // but the text laid out inside it must NOT be lost: the frame's glyph runs
    // are recovered and emitted as hidden (`w:vanish`) runs beside the drawing,
    // so the region stays searchable/selectable/accessible — the image carries
    // the exact visual, the hidden text carries the words.
    let p = parts("#skew(ax: 20deg)[HiddenSkewWord]");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("<a:blip"),
        "skew has no native form, so it rasterizes to an image"
    );
    assert!(doc.contains("<w:vanish/>"), "the recovered text is emitted as a hidden run");
    assert!(
        doc.contains("HiddenSkewWord"),
        "the rasterized word survives as searchable text"
    );
    // The image also gets the recovered text as accessibility alt text.
    assert!(doc.contains("descr=\"HiddenSkewWord\""), "the drawing carries alt text");
    assert_all_wellformed(&p);
}

#[test]
fn svg_image_embeds_native_svg_with_png_fallback() {
    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40" viewBox="0 0 80 40"><rect width="80" height="40" fill="#0b6"/><circle cx="20" cy="20" r="12" fill="#fff"/></svg>"##;

    let raw = package_bytes_with_files(
        r#"#image("logo.svg", width: 40pt, alt: "Brand mark")"#,
        &[("logo.svg", SVG)],
    );
    let p: HashMap<String, String> = raw
        .iter()
        .filter_map(|(name, bytes)| {
            String::from_utf8(bytes.clone()).ok().map(|s| (name.clone(), s))
        })
        .collect();
    let doc = &p["word/document.xml"];
    let rels = &p["word/_rels/document.xml.rels"];

    assert!(doc.contains("<a:blip r:embed=\""), "PNG fallback is the normal blip");
    assert!(doc.contains("uri=\"{28A0092B-C50C-407E-A947-70E740481C1C}\""));
    assert!(doc.contains("<a14:useLocalDpi"));
    assert!(doc.contains("uri=\"{96DAC541-7B7A-43D3-8B79-37D633B846F1}\""));
    assert!(doc.contains("<asvg:svgBlip"));
    assert!(doc.contains(
        "xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\""
    ));
    assert!(doc.contains("descr=\"Brand mark\""));

    let png_rel = doc
        .split("<a:blip r:embed=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("fallback relationship id");
    let svg_rel = doc
        .split("<asvg:svgBlip")
        .nth(1)
        .and_then(|s| s.split("r:embed=\"").nth(1))
        .and_then(|s| s.split('"').next())
        .expect("svg relationship id");

    let png_target = relationship_target(rels, png_rel);
    let svg_target = relationship_target(rels, svg_rel);
    assert!(png_target.ends_with(".png"), "fallback target is PNG: {png_target}");
    assert!(svg_target.ends_with(".svg"), "native target is SVG: {svg_target}");

    let png_part = format!("word/{png_target}");
    let svg_part = format!("word/{svg_target}");
    assert_eq!(&raw[&svg_part], SVG, "the SVG media part stores the source bytes");
    assert!(
        raw[&png_part].starts_with(b"\x89PNG\r\n\x1a\n"),
        "fallback media part must be a valid PNG"
    );
    assert!(
        p["[Content_Types].xml"]
            .contains("<Default Extension=\"svg\" ContentType=\"image/svg+xml\"/>"),
        "package declares the SVG media content type"
    );
    assert_all_wellformed(&p);
}

fn relationship_target(rels_xml: &str, id: &str) -> String {
    let rels = roxmltree::Document::parse(rels_xml).expect("rels XML should parse");
    rels.descendants()
        .find(|node| {
            node.tag_name().name() == "Relationship" && node.attribute("Id") == Some(id)
        })
        .and_then(|node| node.attribute("Target"))
        .unwrap_or_else(|| panic!("relationship {id} should exist"))
        .to_string()
}

#[test]
fn grid_cell_alignment_is_kept() {
    // `#grid` cell alignment must reach `w:jc` (it was only read off `#table`
    // cells before, silently dropping it for grids).
    let p = parts("#grid(columns: 2, align: center, grid.cell[A], [B])");
    assert!(
        p["word/document.xml"].contains("w:jc w:val=\"center\""),
        "grid cell alignment becomes w:jc"
    );
    assert_all_wellformed(&p);
}

#[test]
fn vertical_stack_lowers_to_sequential_paragraphs() {
    // A `#stack` is a pure layout container — common in CV/resume entries — and
    // must keep its text editable, not rasterize. A vertical stack's children
    // flow one below another.
    let p = parts("#stack(dir: ttb, spacing: 6pt, [First entry], [Second entry])");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("First entry"), "stack child text is kept");
    assert!(doc.contains("Second entry"), "all stack children are kept");
    assert!(!doc.contains("<w:drawing>"), "a text stack is not rasterized");
    assert_all_wellformed(&p);
}

#[test]
fn horizontal_stack_lowers_to_a_table_row() {
    // A horizontal stack places children side by side → a borderless table row,
    // mirroring how a layout `#grid` lowers.
    let p = parts("#stack(dir: ltr, [Left col], 1fr, [Right col])");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:tbl>"), "a horizontal stack becomes a table");
    assert!(doc.contains("Left col") && doc.contains("Right col"), "both columns kept");
    assert!(!doc.contains("<w:drawing>"), "not rasterized");
    assert_all_wellformed(&p);
}

#[test]
fn layout_closure_is_invoked_and_extracted() {
    // `#layout(size => ..)` is the responsive-CV/poster idiom. Its closure is
    // invoked with the page's content size and the result lowered natively, so
    // the text stays editable instead of the whole block rasterizing.
    let p = parts("#layout(size => [Responsive paragraph content here.])");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Responsive paragraph content"), "layout body is extracted");
    assert!(!doc.contains("<w:drawing>"), "a text layout is not rasterized");
    assert_all_wellformed(&p);
}

#[test]
fn plain_inline_box_keeps_its_text_selectable() {
    // A plain `#box[..]` (no fill/stroke/clip — used to prevent a line break or
    // to size inline content) must keep its text as runs, not rasterize it to
    // an image. A *framed* box still rasterizes / shades to preserve its visual.
    let plain = parts("before #box[keep together] after");
    let doc = &plain["word/document.xml"];
    assert!(doc.contains("keep together"), "plain box text stays as runs");
    assert!(!doc.contains("<w:drawing>"), "a plain box is not rasterized");

    // A filled box still renders its background (shaded run), text kept.
    let filled = parts("#box(fill: yellow)[hi]");
    assert!(filled["word/document.xml"].contains("hi"), "filled box keeps text too");
    assert_all_wellformed(&plain);
}

#[test]
fn wrap_content_figure_is_recovered_not_rasterized() {
    // A `wrap-content` figure lowers to `layout(=> box(grid(figure, text)))`.
    // The frameless block box must NOT rasterize the whole thing (which drops
    // the figure, its caption and the wrapped text) — the grid-of-figure is
    // lowered natively so the caption + table survive.
    let p = parts(
        "#layout(size => box(grid(columns: 2, \
           figure(table(columns: 1, [x]), caption: [A sample table]), \
           [Wrapped paragraph text here.])))",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("A sample table"), "the figure caption is recovered");
    assert!(doc.contains("Wrapped paragraph text"), "the wrapped text is recovered");
    assert!(doc.contains("<w:tbl>"), "the grid + table are native");
    assert_all_wellformed(&p);
}

#[test]
fn frameless_box_wrapping_columns_flows_instead_of_rasterizing() {
    // A bare top-level `#box(inset: ..)[#columns(2, ..)]` (the poster-template
    // idiom — pollux's own layout) is paragraph-wrapped by Typst's realize
    // (there's no bare-inline-content block variant), which used to force it
    // through the run-only inline path: `#columns` fails
    // `body_inline_extractable`, so the *entire* multi-section body rasterized
    // as ONE image many times taller than the page, spilling across dozens of
    // near-blank pages in Word/LibreOffice. A frameless box whose whole body is
    // `#columns`/`#stack`/a non-figure `#grid` now flows as ordinary native
    // blocks instead (single-column-approximated — the box has no fill/stroke
    // for a column split to interact with, so nothing else is lost).
    let p = parts(
        "#box(inset: 1cm)[#columns(2, [Introduction text here. Method text here.])]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Introduction text here"), "columns body is extracted");
    assert!(doc.contains("Method text here"), "columns body is extracted");
    assert!(!doc.contains("<w:drawing>"), "the box is not rasterized wholesale");
    assert_all_wellformed(&p);

    // A box with a fill/stroke around the SAME body is intentionally NOT
    // widened by this change (only the frameless case is validated safe here)
    // — it keeps its pre-existing rasterize behavior, preserving the visual.
    let framed = parts(
        "#box(inset: 1cm, fill: yellow)[#columns(2, [Framed section text.])]",
    );
    let doc = &framed["word/document.xml"];
    assert!(doc.contains("<w:drawing>"), "a filled box still rasterizes its visual");
    assert_all_wellformed(&framed);
}

#[test]
fn box_with_bottom_only_stroke_keeps_a_bottom_only_border() {
    // `box(height: 20pt, width: 100%, stroke: (bottom: 0.5pt + black))[Heading]`
    // is the common CV/resume "border as a section-title underline" idiom
    // (found in a real corpus doc, bwaklog-vita). Reached via a `ParElem` at
    // block scope, it used to fall through to the run-only inline path's
    // character border (`w:bdr`, via `mappers::shape::inline_frame`), which is
    // inherently uniform around all four sides — silently turning the intended
    // bottom-only underline into a full box. It must now flow as a genuine
    // paragraph with a `w:pBdr` carrying ONLY the bottom side.
    let p = parts(
        "#box(height: 20pt, width: 100%, stroke: (bottom: 0.5pt + black))[Summary]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Summary"), "the heading text is extracted");
    assert!(!doc.contains("<w:bdr"), "no uniform character border is emitted");
    assert!(!doc.contains("<w:drawing>"), "the box is not rasterized");
    assert!(doc.contains("<w:pBdr>"), "a paragraph border is used instead");
    assert!(doc.contains("<w:bottom "), "the bottom side is bordered");
    assert!(!doc.contains("<w:top "), "the top side is NOT bordered");
    assert!(!doc.contains("<w:left "), "the left side is NOT bordered");
    assert!(!doc.contains("<w:right "), "the right side is NOT bordered");
    assert_all_wellformed(&p);

    // Mid-sentence (genuinely inline, not a paragraph's sole content), the same
    // partial stroke still can't be a run-level border — it now rasterizes
    // (preserves the visual) instead of silently becoming a full box.
    let inline = parts(
        "before #box(stroke: (bottom: 0.5pt + black))[mid] after",
    );
    let doc = &inline["word/document.xml"];
    assert!(doc.contains("before"), "surrounding text is preserved");
    assert!(doc.contains("after"), "surrounding text is preserved");
    assert!(!doc.contains("<w:bdr"), "no uniform character border is emitted");
    assert_all_wellformed(&inline);
}

#[test]
fn block_columns_emit_continuous_sections() {
    // `#columns(n)[..]` is section-scoped in Word: split into a continuous
    // multi-column section for the block, then immediately return to the
    // surrounding column count.
    let p = parts(
        "Intro text.\n\
         #columns(2, gutter: 12pt)[Column content starts here. #colbreak() \
         Column content continues here.]\n\
         More text after.",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Intro text"), "pre-column text is kept");
    assert!(doc.contains("Column content starts"), "column text is kept");
    assert!(doc.contains("More text after"), "post-column text is kept");
    assert!(!doc.contains("<w:drawing>"), "columns are not rasterized");
    assert!(doc.contains("w:type=\"column\""), "explicit column breaks survive");

    let intro = doc.find("Intro text").unwrap();
    let column = doc.find("Column content starts").unwrap();
    let after = doc.find("More text after").unwrap();
    let sects = sect_pr_chunks(doc);
    assert_eq!(sects.len(), 3, "block columns create before/block/after sections");
    assert!(intro < doc.find("<w:sectPr>").unwrap());
    assert!(doc.find("<w:sectPr>").unwrap() < column);
    assert!(column < doc.rfind("<w:sectPr>").unwrap());
    assert!(after < doc.rfind("<w:sectPr>").unwrap());

    // A section's `w:type` describes how *that* section itself starts
    // (relative to the one before it) — so the type requested for a
    // transition lives on the section that begins *after* it, not the one
    // that ends there. The first section has no predecessor, so it carries
    // no type; the continuous transitions into and out of the column block
    // land on sections 1 and 2 respectively.
    assert!(
        !sects[0].contains("<w:type"),
        "the first section has no predecessor to transition from"
    );
    assert!(sects[1].contains("<w:type w:val=\"continuous\"/>"));
    assert!(sects[2].contains("<w:type w:val=\"continuous\"/>"));
    assert!(
        !sects[0].contains("w:num="),
        "the surrounding section remains single-column"
    );
    assert!(
        sects[1].contains("<w:cols w:num=\"2\"") && sects[1].contains("w:space=\"240\""),
        "the columns block gets two columns and its 12pt gutter"
    );
    assert!(
        !sects[2].contains("w:num="),
        "the post-column section restores single-column layout"
    );
    assert_all_wellformed(&p);
}

#[test]
fn block_columns_restore_page_level_column_count() {
    let p = parts(
        "#set page(columns: 2)\n\
         Before.\n\
         #columns(3)[First. #colbreak() Second. #colbreak() Third.]\n\
         After.",
    );
    let doc = &p["word/document.xml"];
    let sects = sect_pr_chunks(doc);
    assert_eq!(sects.len(), 3, "block columns split the page-level section");
    assert!(sects[0].contains("<w:cols w:num=\"2\""));
    assert!(
        !sects[0].contains("<w:type"),
        "the first section has no predecessor to transition from"
    );
    assert!(sects[1].contains("<w:cols w:num=\"3\""));
    assert!(sects[1].contains("<w:type w:val=\"continuous\"/>"));
    assert!(
        sects[2].contains("<w:cols w:num=\"2\""),
        "the section after #columns() restores page-level columns"
    );
    assert!(
        sects[2].contains("<w:type w:val=\"continuous\"/>"),
        "returning to page-level columns is also a continuous transition"
    );
    assert_all_wellformed(&p);
}

#[test]
fn nested_bullets_indent_by_level() {
    // A nested bullet list must descend ilvl (depth fold), not stay flat at 0.
    let p = parts("- a\n- b\n  - b1\n    - b1a");
    let doc = &p["word/document.xml"];
    for lvl in ["0", "1", "2"] {
        assert!(
            doc.contains(&format!("<w:ilvl w:val=\"{lvl}\"/>")),
            "nested bullets reach ilvl {lvl}"
        );
    }
    assert_all_wellformed(&p);
}

#[test]
fn ordered_enum_uses_native_word_numbering() {
    let p = parts("+ first\n+ second\n+ third");
    let doc = &p["word/document.xml"];
    let numbering = &p["word/numbering.xml"];
    assert!(doc.contains("<w:numPr>"), "enum paragraphs link to numbering");
    assert!(
        numbering.contains("<w:numFmt w:val=\"decimal\"/>"),
        "default enum uses decimal Word numbering"
    );
    assert!(
        numbering.contains("<w:lvlText w:val=\"%1.\"/>"),
        "default enum level text remains 1."
    );
    assert!(
        !doc.contains("<w:t>1.</w:t>"),
        "native numbering must not bake marker text into document.xml"
    );
    assert_all_wellformed(&p);
}

#[test]
fn letter_and_roman_enums_use_native_word_numbering() {
    let lettered = parts("#set enum(numbering: \"a.\")\n+ alpha\n+ beta");
    let letter_numbering = &lettered["word/numbering.xml"];
    assert!(
        letter_numbering.contains("<w:numFmt w:val=\"lowerLetter\"/>"),
        "lettered enum maps to lowerLetter"
    );
    assert!(
        lettered["word/document.xml"].contains("<w:numPr>"),
        "lettered enum uses native numPr"
    );

    let roman = parts("#set enum(numbering: \"I.\")\n+ one\n+ two");
    let roman_numbering = &roman["word/numbering.xml"];
    assert!(
        roman_numbering.contains("<w:numFmt w:val=\"upperRoman\"/>"),
        "Roman enum maps to upperRoman"
    );
    assert!(
        roman["word/document.xml"].contains("<w:numPr>"),
        "Roman enum uses native numPr"
    );
    assert_all_wellformed(&lettered);
    assert_all_wellformed(&roman);
}

#[test]
fn non_native_enums_keep_static_marker_fallback() {
    let closure = parts("#set enum(numbering: n => str(n) + \")\")\n+ alpha\n+ beta");
    let closure_doc = &closure["word/document.xml"];
    assert!(
        !closure_doc.contains("<w:numPr>"),
        "numbering closures cannot use native Word counters"
    );
    assert!(
        visible_text(closure_doc).contains("1)"),
        "closure marker is baked as literal text"
    );

    let symbols = parts("#set enum(numbering: \"* \")\n+ alpha\n+ beta");
    let symbol_doc = &symbols["word/document.xml"];
    assert!(
        !symbol_doc.contains("<w:numPr>"),
        "symbol numbering stays on the static fallback"
    );
    assert!(
        visible_text(symbol_doc).contains('*'),
        "symbol marker is preserved as document text"
    );
    assert_all_wellformed(&closure);
    assert_all_wellformed(&symbols);
}

#[test]
fn nested_full_enum_numbers_include_ancestry() {
    // `#set enum(full: true)` nested numbering must use a level-1 template that
    // references the parent and child counters, with level-1 paragraph indents.
    let p = parts("#set enum(full: true)\n+ one\n  + one-a\n+ two");
    let doc = &p["word/document.xml"];
    let numbering = &p["word/numbering.xml"];
    assert!(doc.contains("<w:ilvl w:val=\"1\"/>"), "nested enum reaches Word level 1");
    assert!(
        numbering.contains("<w:lvlText w:val=\"%1.%2.\"/>"),
        "nested full enum shows parent and child counters"
    );
    assert!(
        numbering.contains("w:left=\"1440\""),
        "level-1 enum has the second-level indent"
    );
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
fn upright_letters_get_m_nor() {
    // Typst pre-applies italic by remapping to Plane-1 codepoints, so a plain
    // letter reaching the converter is upright-intended (uppercase Greek,
    // `upright(..)`, the differential `d`) and must carry `m:nor` — otherwise
    // Word slants it.
    let p = parts("$ Gamma + upright(B) $");
    let doc = &p["word/document.xml"];
    // Every math run is upright now (Plane-1 italic glyphs carry their own slant).
    assert!(doc.contains("m:nor"), "upright math letters carry <m:nor/>");
    assert!(!doc.contains("Γ</m:t></m:r>") || doc.contains("<m:nor/>"), "Γ is upright");
    assert_all_wellformed(&p);
}

#[test]
fn bare_nary_operator_and_operand_boundary() {
    // A large operator without bounds is still typeset as an n-ary (not a small
    // literal glyph), and its operand stops at a binary operator so sibling sums
    // do not nest.
    let p = parts("$ integral f dif x $ and $ sum_i a_i + sum_j b_j $");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("m:nary"), "a bare integral is an n-ary operator");
    // Two sums + one integral = 3 n-ary operators; if the first sum swallowed the
    // second there would be only 2.
    assert_eq!(doc.matches("<m:nary>").count(), 3, "sibling sums are not nested");
    assert_all_wellformed(&p);
}

#[test]
fn spaced_scripted_nary_operators_are_siblings() {
    // Two *scripted* big operators separated only by spacing must stay siblings,
    // not nest the second inside the first's operand (which rendered garbled).
    // The scripted `product_(i)` / `union.big_(j)` are `Scripts` items, so the
    // boundary check has to see through the script wrapper, not just bare glyphs.
    let p = parts("$ product_(i=1)^n a_i quad union.big_(j=1)^m b_j $");
    let doc = &p["word/document.xml"];
    assert_eq!(doc.matches("<m:nary>").count(), 2, "two n-ary operators");
    let first_close = doc.find("</m:nary>").unwrap();
    let second_open = doc.match_indices("<m:nary>").nth(1).unwrap().0;
    assert!(
        second_open > first_close,
        "the second operator must not be nested inside the first's operand"
    );
    // A nested sum (no separator) must still nest: the inner operator is the
    // very first operand item.
    let q = parts("$ sum_(i) sum_(j) a_(i j) $");
    let d2 = &q["word/document.xml"];
    let fc = d2.find("</m:nary>").unwrap();
    let so = d2.match_indices("<m:nary>").nth(1).unwrap().0;
    assert!(so < fc, "adjacent sums still nest");
    assert_all_wellformed(&p);
}

#[test]
fn colored_math_carries_its_color() {
    // `#text(red)[$x$]` inside an equation must color the math run (a `w:rPr`
    // colour on the math `m:r`), not render black.
    let p = parts("$ y = #text(red)[x] + b $");
    let doc = &p["word/document.xml"];
    assert!(
        doc.contains("<w:color w:val=\"FF4136\""),
        "the red math run carries its color"
    );
    assert_all_wellformed(&p);
}

#[test]
fn over_spreader_stretches() {
    // overbrace/overbracket span the base (stretchy `m:groupChr`), unlike a hat
    // (a single-glyph `m:acc`).
    let p = parts("$ overbrace(x+y+z, n) $ and $ hat(a) $");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<m:groupChr>"), "overbrace stretches via groupChr");
    assert!(doc.contains("<m:acc>"), "a hat stays a single-glyph accent");
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
fn heading_outline_is_a_toc_content_control() {
    // A heading table of contents is wrapped in a Word "Table of Contents"
    // content control (`w:sdt`/`docPartObj`) — the idiomatic, gallery-aware form.
    let p = parts("#outline()\n\n= Alpha\n\n= Beta");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:sdt>"), "the TOC is wrapped in a content control");
    assert!(
        doc.contains("w:val=\"Table of Contents\""),
        "with the Table of Contents docPart gallery"
    );
    assert!(doc.contains("<w:sdtContent>"), "and its entries live in sdtContent");
    assert_all_wellformed(&p);
}

#[test]
fn core_properties_carry_author_and_revision() {
    let p =
        parts("#set document(title: \"T\", author: \"Ada Lovelace\")\n#outline()\n\n= H");
    let core = &p["docProps/core.xml"];
    assert!(core.contains("<dc:title>T</dc:title>"), "title is recorded");
    assert!(core.contains("Ada Lovelace"), "author is the creator");
    assert!(core.contains("cp:lastModifiedBy"), "and the last-modified-by");
    assert!(core.contains("<cp:revision>1</cp:revision>"), "with a revision number");
    assert_all_wellformed(&p);
}

#[test]
fn cross_reference_is_a_clickable_hyperlink() {
    // `@label` to a heading/figure renders the correct number AND is a real
    // clickable hyperlink to the target's bookmark (the destination survives as
    // the `LinkElem::current` style after the marker is stripped in realize).
    let p = parts(
        "#set heading(numbering: \"1.\")\n= Intro <intro>\n\n= Methods\n\nAs in @intro.",
    );
    let doc = &p["word/document.xml"];
    // The ref paragraph carries a hyperlink, not bare text.
    let para = doc.split("<w:p>").find(|p| p.contains("As in")).expect("ref para");
    assert!(para.contains("<w:hyperlink"), "the cross-reference is a hyperlink");
    let anchor = {
        let i = para.find("w:anchor=\"").expect("anchor") + 10;
        &para[i..][..para[i..].find('"').unwrap()]
    };
    // …and it targets a bookmark that actually exists.
    assert!(
        doc.contains(&format!("w:name=\"{anchor}\"")),
        "the ref anchor {anchor} resolves to a real bookmark"
    );
    assert_all_wellformed(&p);
}

#[test]
fn url_link_looks_like_a_link() {
    // A `#link("url")[text]` must render as a real Word hyperlink: blue +
    // underline. The `Hyperlink` character style supplies that, and the run must
    // NOT carry an explicit black colour (which would override the style back to
    // invisible body text — the bug this guards against).
    let p = parts("See #link(\"https://typst.app\")[the site] now.");
    let doc = &p["word/document.xml"];
    let styles = &p["word/styles.xml"];
    // The Hyperlink character style is defined with a colour + underline.
    let hl = styles
        .split("w:styleId=\"Hyperlink\"")
        .nth(1)
        .and_then(|s| s.split("</w:style>").next())
        .expect("Hyperlink style");
    assert!(hl.contains("<w:color"), "Hyperlink style sets a colour");
    assert!(hl.contains("<w:u "), "Hyperlink style underlines");
    // The link run uses the style and does NOT pin its own black colour.
    let link = doc
        .split("<w:hyperlink")
        .nth(1)
        .and_then(|s| s.split("</w:hyperlink>").next())
        .expect("a hyperlink");
    assert!(link.contains("w:val=\"Hyperlink\""), "link run uses the Hyperlink style");
    assert!(
        !link.contains("<w:color w:val=\"000000\""),
        "link run must not override the style with black"
    );
    assert_all_wellformed(&p);
}

#[test]
fn text_box_has_a_vml_fallback() {
    // A `wps:txbx` text box is a 2010 DrawingML feature; it is wrapped in
    // `mc:AlternateContent` with a legacy VML `v:textbox` fallback so consumers
    // that don't support `wps` still render the framed text.
    let p = parts("#rect(width: 4cm, height: 1cm, fill: aqua, stroke: 1pt)[box text]");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<mc:AlternateContent"), "wrapped in mc:AlternateContent");
    assert!(doc.contains("Requires=\"wps\""), "the modern choice requires wps");
    assert!(doc.contains("<wps:txbx"), "modern wps text box in the Choice");
    assert!(doc.contains("<v:textbox"), "legacy VML text box in the Fallback");
    assert_all_wellformed(&p);
}

#[test]
fn standard_word_parts_are_present() {
    // A document Word itself writes always ships a theme, a font table and web
    // settings; emit them so the package looks native.
    let p = parts("Hello.");
    assert!(p.contains_key("word/theme/theme1.xml"), "theme present");
    assert!(p.contains_key("word/fontTable.xml"), "font table present");
    assert!(p.contains_key("word/webSettings.xml"), "web settings present");
    // settings.xml carries the compat block + the standard settings.
    let s = &p["word/settings.xml"];
    assert!(s.contains("compatibilityMode") && s.contains("w:val=\"15\""), "compat 15");
    assert!(
        s.contains("clrSchemeMapping") && s.contains("defaultTabStop"),
        "rich settings"
    );
    assert_all_wellformed(&p);
}

#[test]
fn standard_gallery_and_linked_heading_styles_are_defined() {
    // A survey of real Word documents shows they universally define the gallery
    // styles (Title/Subtitle/Strong/Emphasis/Table Grid) and pair each heading
    // with a linked character style. We define them too so the Styles gallery
    // matches a Word-authored package and heading char formatting works.
    let p = parts("= Heading one\n== Heading two\nBody.");
    let styles = &p["word/styles.xml"];
    for id in [
        "Title",
        "TitleChar",
        "Subtitle",
        "Strong",
        "Emphasis",
        "TableGrid",
        "FollowedHyperlink",
        "PageNumber",
    ] {
        assert!(
            styles.contains(&format!("w:styleId=\"{id}\"")),
            "gallery style {id} should be defined"
        );
    }
    // Each used heading level is paired with its linked character style.
    assert!(styles.contains("w:styleId=\"Heading1Char\""), "Heading1 is linked");
    assert!(styles.contains("w:styleId=\"Heading2Char\""), "Heading2 is linked");
    assert!(
        styles.contains("<w:link w:val=\"Heading1Char\"/>"),
        "the heading paragraph style links to its char style"
    );
    assert_all_wellformed(&p);
}

#[test]
fn strong_and_emph_use_word_character_styles() {
    let p = parts("#strong[semantic bold]\n\n#emph[semantic italic]");
    let doc = &p["word/document.xml"];

    let strong = run_fragment_containing(doc, "semantic bold");
    assert!(
        strong.contains("<w:rStyle w:val=\"Strong\"/>"),
        "strong run should use the Strong character style: {strong}"
    );
    assert!(
        !strong.contains("<w:b"),
        "strong run should not duplicate bold as direct formatting: {strong}"
    );

    let emphasis = run_fragment_containing(doc, "semantic italic");
    assert!(
        emphasis.contains("<w:rStyle w:val=\"Emphasis\"/>"),
        "emph run should use the Emphasis character style: {emphasis}"
    );
    assert!(
        !emphasis.contains("<w:i"),
        "emph run should not duplicate italic as direct formatting: {emphasis}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn manual_bold_and_italic_stay_direct_formatting() {
    let p = parts(
        "#text(weight: \"bold\")[manual bold]\n\n\
         #text(style: \"italic\")[manual italic]",
    );
    let doc = &p["word/document.xml"];

    let bold = run_fragment_containing(doc, "manual bold");
    assert!(!bold.contains("w:rStyle w:val=\"Strong\""), "manual bold is not Strong");
    assert!(bold.contains("<w:b/>"), "manual bold remains direct: {bold}");

    let italic = run_fragment_containing(doc, "manual italic");
    assert!(
        !italic.contains("w:rStyle w:val=\"Emphasis\""),
        "manual italic is not Emphasis"
    );
    assert!(italic.contains("<w:i/>"), "manual italic remains direct: {italic}");
    assert_all_wellformed(&p);
}

#[test]
fn heading_bold_is_not_misclassified_as_strong() {
    let p = parts("= Styled Heading");
    let styles = &p["word/styles.xml"];
    let doc = &p["word/document.xml"];

    let heading_style = style_fragment(styles, "Heading1");
    assert!(heading_style.contains("<w:b/>"), "Heading1 owns heading bold");

    let run = run_fragment_containing(doc, "Styled Heading");
    assert!(
        !run.contains("w:rStyle w:val=\"Strong\""),
        "heading-inherited bold is not semantic Strong: {run}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn nested_strong_emphasis_layers_character_style_with_direct_formatting() {
    let p = parts(
        "Plain body text long enough to keep black as the document default.\n\n\
         #strong[#emph[#text(fill: rgb(\"AA0000\"))[combined red]]]",
    );
    let doc = &p["word/document.xml"];

    let run = run_fragment_containing(doc, "combined red");
    assert!(
        run.contains("<w:rStyle w:val=\"Strong\"/>"),
        "combined strong/emph uses Strong as the character style: {run}"
    );
    assert!(!run.contains("<w:b"), "the Strong character style supplies bold: {run}");
    assert!(run.contains("<w:i/>"), "nested emphasis is layered as direct italic: {run}");
    assert!(
        run.contains("<w:color w:val=\"AA0000\"/>"),
        "other direct deviations must survive beside the character style: {run}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn paragraphs_carry_unique_w14_para_ids() {
    // Word stamps every content paragraph with a `w14:paraId`/`w14:textId`
    // (the identity its comments/revisions/co-authoring anchor to). We emit
    // them deterministically, unique within and across parts, and declare
    // `w14` in each part root's `mc:Ignorable` so they are MCE-valid on `w:p`.
    let p = parts(
        "#set page(header: [Head], numbering: \"1\")\n\
         = Intro\n\
         A paragraph with a note.#footnote[A note.]\n\n\
         Another paragraph.",
    );

    let collect_ids = |xml: &str| -> Vec<String> {
        xml.match_indices("w14:paraId=\"")
            .map(|(i, m)| {
                let rest = &xml[i + m.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    };

    let doc = &p["word/document.xml"];
    let body_ids = collect_ids(doc);
    assert!(body_ids.len() >= 3, "every body paragraph has a paraId");
    // The `w14` attributes are only legal on `w:p` because the root marks them
    // ignorable.
    assert!(doc.contains("mc:Ignorable=\"w14"), "document root ignores w14");

    // Collect ids from every part; they must be globally unique (disjoint
    // per-part lanes), which a strict validator requires.
    let mut all = Vec::new();
    let mut names: Vec<&String> = p.keys().collect();
    names.sort();
    for name in names {
        let xml = &p[name];
        if name.ends_with(".xml") {
            all.extend(collect_ids(xml));
        }
    }
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "paraIds are globally unique: {all:?}");

    // The footnote body (a separate part) is in its own lane, not the body's.
    let notes = &p["word/footnotes.xml"];
    assert!(
        collect_ids(notes).iter().any(|id| id.starts_with("7")),
        "footnote paragraphs use the notes lane"
    );
    assert_all_wellformed(&p);
}

#[test]
fn multi_slot_page_numbering_emits_page_of_numpages() {
    // `numbering: "1 of 1"` is the "page X of Y" idiom: the first counting slot
    // is the current page (a `PAGE` field), the second the document total (a
    // `NUMPAGES` field), with the literal " of " between them — not a bare PAGE
    // that silently drops the total.
    let p = parts("#set page(numbering: \"1 of 1\")\nBody.");
    let footer = p
        .iter()
        .find(|(name, _)| name.starts_with("word/footer"))
        .map(|(_, xml)| xml.as_str())
        .expect("a numbered footer part");
    assert!(footer.contains("PAGE "), "current page is a PAGE field");
    assert!(footer.contains("NUMPAGES "), "the total is a NUMPAGES field");
    assert!(
        footer.contains("> of <") || footer.contains("of"),
        "keeps the ' of ' literal"
    );
    let page_at = footer.find("PAGE ").unwrap();
    let num_at = footer.find("NUMPAGES ").unwrap();
    assert!(page_at < num_at, "PAGE (current) precedes NUMPAGES (total)");
    assert_all_wellformed(&p);
}

#[test]
fn roman_multi_slot_numbering_switches_both_fields() {
    // A roman "i of i" must render the total in roman too, so both fields carry
    // the `\* roman` format switch (NUMPAGES otherwise defaults to arabic).
    let p = parts("#set page(numbering: \"i of i\")\nBody.");
    let footer = p
        .iter()
        .find(|(name, _)| name.starts_with("word/footer"))
        .map(|(_, xml)| xml.as_str())
        .expect("a numbered footer part");
    assert_eq!(footer.matches("\\* roman").count(), 2, "PAGE and NUMPAGES both roman");
    assert_all_wellformed(&p);
}

#[test]
fn single_slot_numbering_stays_a_bare_page_field() {
    // A plain `numbering: "1"` must not gain a spurious NUMPAGES.
    let p = parts("#set page(numbering: \"1\")\nBody.");
    let footer = p
        .iter()
        .find(|(name, _)| name.starts_with("word/footer"))
        .map(|(_, xml)| xml.as_str())
        .expect("a numbered footer part");
    assert!(footer.contains("PAGE "), "has the PAGE field");
    assert!(!footer.contains("NUMPAGES"), "no total for a single-slot numbering");
    assert_all_wellformed(&p);
}

#[test]
fn uppercase_text_uses_caps_without_rewriting_text() {
    let p = parts("#upper[Hello World]");
    let doc = &p["word/document.xml"];
    let run = run_fragment_containing(doc, "Hello World");
    assert!(run.contains("<w:caps/>"), "upper-case text uses the Word caps toggle");
    assert!(!doc.contains("HELLO WORLD"), "raw run text keeps the original mixed case");
    assert_all_wellformed(&p);
}

#[test]
fn document_default_text_props_are_hoisted_into_doc_defaults_and_normal() {
    // The root StyleChain's font/size/color is hoisted into `docDefaults` and
    // Normal; body runs that match inherit it, while deviations stay direct.
    let p = parts(
        "#set text(font: \"Liberation Serif\", size: 12pt, fill: rgb(\"123456\"))\n\
         Plain body text here.\n\n\
         #text(font: \"Liberation Mono\", fill: rgb(\"AA0000\"))[deviating run]",
    );
    let styles = &p["word/styles.xml"];
    let doc = &p["word/document.xml"];

    // docDefaults carries the document's root font + size + color.
    let dd = &styles[styles.find("<w:docDefaults>").unwrap()..];
    let dd = &dd[..dd.find("</w:docDefaults>").unwrap()];
    assert!(dd.contains("liberation serif"), "default font hoisted: {dd}");
    assert!(
        dd.contains("w:val=\"24\""),
        "default size (12pt = 24 half-pt) hoisted: {dd}"
    );
    assert!(dd.contains("<w:color w:val=\"123456\"/>"), "default color hoisted: {dd}");

    // Normal carries the same defaults so restyling Normal is effective.
    let normal = style_fragment(styles, "Normal");
    assert!(normal.contains("liberation serif"), "Normal owns default font");
    assert!(normal.contains("w:val=\"24\""), "Normal owns default size");
    assert!(normal.contains("<w:color w:val=\"123456\"/>"), "Normal owns color");

    // The body run has no duplicate rPr; only the deviating run emits overrides.
    let plain = run_fragment_containing(doc, "Plain body text here.");
    assert!(!plain.contains("<w:rPr>"), "plain body inherits defaults: {plain}");
    let deviating = run_fragment_containing(doc, "deviating run");
    assert!(deviating.contains("liberation mono"), "deviating font stays direct");
    assert!(
        deviating.contains("<w:color w:val=\"AA0000\"/>"),
        "deviating color stays direct: {deviating}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn heading_style_owns_matching_run_formatting() {
    let p = parts(
        "#set text(font: \"Liberation Serif\", size: 11pt)\n\
         #show heading.where(level: 1): set text(font: \"Liberation Sans\", size: 20pt, fill: rgb(\"224466\"))\n\
         = Styled Heading\n\n\
         Body.",
    );
    let styles = &p["word/styles.xml"];
    let doc = &p["word/document.xml"];

    let heading_style = style_fragment(styles, "Heading1");
    assert!(heading_style.contains("liberation sans"), "Heading1 owns font");
    assert!(heading_style.contains("<w:sz w:val=\"40\"/>"), "Heading1 owns size");
    assert!(heading_style.contains("<w:color w:val=\"224466\"/>"), "Heading1 owns color");
    assert!(heading_style.contains("<w:b/>"), "Heading1 owns bold");

    let para = para_fragment_containing(doc, "Styled Heading");
    assert!(para.contains("w:pStyle w:val=\"Heading1\""), "heading uses style");
    assert!(!para.contains("<w:keepNext/>"), "keepNext comes from style");
    assert!(!para.contains("<w:outlineLvl"), "outline level comes from style");

    let run = run_fragment_containing(doc, "Styled Heading");
    assert!(!run.contains("<w:rFonts"), "matching font stripped: {run}");
    assert!(!run.contains("<w:sz"), "matching size stripped: {run}");
    assert!(!run.contains("<w:color"), "matching color stripped: {run}");
    assert!(!run.contains("<w:b"), "matching bold stripped: {run}");
    assert_all_wellformed(&p);
}

#[test]
fn deviating_heading_run_keeps_only_the_deviation() {
    let p = parts(
        "#show heading.where(level: 1): set text(fill: rgb(\"224466\"))\n\
         = #text(fill: rgb(\"AA0000\"))[Warning]",
    );
    let styles = &p["word/styles.xml"];
    let doc = &p["word/document.xml"];

    let heading_style = style_fragment(styles, "Heading1");
    assert!(
        heading_style.contains("<w:color w:val=\"224466\"/>"),
        "Heading1 owns the style-chain color"
    );

    let run = run_fragment_containing(doc, "Warning");
    assert!(
        run.contains("<w:color w:val=\"AA0000\"/>"),
        "manual heading color remains as a direct deviation: {run}"
    );
    assert!(!run.contains("<w:sz"), "matching heading size is stripped: {run}");
    assert!(!run.contains("<w:b"), "matching heading bold is stripped: {run}");
    assert_all_wellformed(&p);
}

#[test]
fn colbreak_becomes_a_column_break() {
    // `#colbreak()` is a real layout instruction (move to the next column), not a
    // no-op — it must survive as `<w:br w:type="column"/>`.
    let p = parts("#set page(columns: 2)\nLeft.\n#colbreak()\nNext column.");
    assert!(
        p["word/document.xml"].contains("w:type=\"column\""),
        "a column break must be emitted"
    );
    assert_all_wellformed(&p);
}

#[test]
fn page_level_columns_stay_a_single_section() {
    let p = parts("#set page(columns: 2)\nLeft.\n#colbreak()\nNext column.");
    let doc = &p["word/document.xml"];
    assert_eq!(
        doc.matches("<w:sectPr>").count(),
        1,
        "page-level columns are already native section columns"
    );
    assert!(doc.contains("<w:cols w:num=\"2\""));
    assert_all_wellformed(&p);
}

#[test]
fn multi_paragraph_block_quote_keeps_its_paragraphs() {
    // A two-paragraph block quote must stay two paragraphs — the internal parbreak
    // is real separation, not something to silently drop (which would merge them).
    let p = parts("#quote(block: true)[First para.\n\nSecond para.]");
    let n = p["word/document.xml"].matches("w:val=\"Quote\"").count();
    assert_eq!(n, 2, "both quoted paragraphs keep the Quote style as separate <w:p>");
    assert_all_wellformed(&p);
}

#[test]
fn par_line_numbering_becomes_section_line_numbering() {
    // Typst line numbering is paragraph-style driven; Word enables it at the
    // section level. Preserve the section-level controls Word can express.
    let p = parts(
        "#set par.line(numbering: \"1\", numbering-scope: \"page\", number-clearance: 5pt)\n\
         Numbered first line. \\\n\
         Numbered second line.",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:lnNumType"), "section enables line numbering");
    assert!(doc.contains("w:countBy=\"1\""), "line numbers count every line");
    assert!(doc.contains("w:start=\"1\""), "line numbering starts at one");
    assert!(
        doc.contains("w:restart=\"newPage\""),
        "Typst page-scoped line numbering resets on each Word page"
    );
    assert!(doc.contains("w:distance=\"100\""), "5pt number-clearance becomes 100 twips");
    let plain = parts("No line numbering here.");
    assert!(
        !plain["word/document.xml"].contains("<w:lnNumType"),
        "plain documents do not gain section line numbering"
    );
    assert_all_wellformed(&p);
    assert_all_wellformed(&plain);
}

#[test]
fn block_columns_keep_line_numbering_on_split_sections() {
    let p = parts(
        "#set par.line(numbering: \"1\", numbering-scope: \"page\", number-clearance: 5pt)\n\
         Before columns. \\\n\
         #columns(2)[Column line one. \\\n\
         Column line two.]\n\
         After columns.",
    );
    let doc = &p["word/document.xml"];
    let sects = sect_pr_chunks(doc);
    assert_eq!(sects.len(), 3, "columns split into three sections");
    assert_eq!(
        doc.matches("<w:lnNumType").count(),
        3,
        "line numbering stays active in every split section"
    );
    for sect in sects {
        assert!(sect.contains("w:restart=\"newPage\""));
        assert!(sect.contains("w:distance=\"100\""));
    }
    assert_all_wellformed(&p);
}

#[test]
fn page_background_becomes_a_behind_text_header_image() {
    // `set page(background: ..)` → a full-page `behindDoc`, page-anchored image in
    // the (default) header, so it repeats on every page behind the body text.
    let p = parts(
        "#set page(background: rect(width: 100%, height: 100%, fill: aqua))\nBody text.",
    );
    let header = p
        .keys()
        .find(|k| k.starts_with("word/header") && k.ends_with(".xml"))
        .map(|k| &p[k])
        .expect("a header part for the background");
    assert!(header.contains("behindDoc=\"1\""), "background sits behind the text");
    assert!(header.contains("relativeFrom=\"page\""), "positioned against the page");
    assert!(header.contains("<a:blip"), "the background is an embedded image");
    // The body text is unaffected.
    assert!(p["word/document.xml"].contains("Body text"));
    assert_all_wellformed(&p);
}

#[test]
fn page_foreground_becomes_a_front_of_text_header_image() {
    // `set page(foreground: ..)` uses the same page-anchored header drawing
    // idiom as backgrounds, but in front of body text.
    let p = parts("#set page(foreground: rotate(45deg)[DRAFT])\nBody text.");
    let header = p
        .iter()
        .find(|(k, xml)| {
            k.starts_with("word/header")
                && k.ends_with(".xml")
                && xml.contains("Foreground")
        })
        .map(|(_, xml)| xml)
        .expect("a header part for the foreground");
    assert!(header.contains("behindDoc=\"0\""), "foreground sits in front of text");
    assert!(header.contains("relativeFrom=\"page\""), "positioned against the page");
    assert!(header.contains("<a:blip"), "the foreground is an embedded image");
    assert!(p["word/document.xml"].contains("Body text"), "body text remains in flow");
    assert_all_wellformed(&p);
}

#[test]
fn place_only_foreground_still_rasterizes() {
    // A watermark is commonly built purely from `place(..)`, which positions
    // content absolutely without contributing to its parent's *measured*
    // size. Rendering it in a shrink-fit region previously collapsed it to a
    // degenerate zero-size frame, silently dropping the foreground entirely
    // (no header part, no drawing, no media at all) — caught only by
    // opening the export in real Microsoft Word. The overlay must be
    // rendered into a region expanded to the full page box instead.
    let raw = package_bytes_with_files(
        "#set page(foreground: place(center, text(64pt)[DRAFT]))\nBody text.",
        &[],
    );
    // `parts()` drops binary entries (only valid-UTF-8 parts survive), so a
    // media part's mere presence must be checked against the raw byte map.
    let p: HashMap<String, String> = raw
        .iter()
        .filter_map(|(name, bytes)| {
            String::from_utf8(bytes.clone()).ok().map(|s| (name.clone(), s))
        })
        .collect();
    let header = p
        .iter()
        .find(|(k, xml)| {
            k.starts_with("word/header")
                && k.ends_with(".xml")
                && xml.contains("Foreground")
        })
        .map(|(_, xml)| xml)
        .expect("a header part for the place()-only foreground");
    assert!(header.contains("<a:blip"), "the foreground is an embedded image");
    assert!(
        raw.keys().any(|k| k.starts_with("word/media/")),
        "the rasterized watermark is embedded as a media part"
    );
    assert_all_wellformed(&p);
}

#[test]
fn solid_page_fill_becomes_a_native_page_color() {
    // `set page(fill: solid-color)` (Word's "Page Color") maps to the
    // document-level `w:background` element — no image, no header part.
    let p = parts("#set page(fill: rgb(\"#f0e6d2\"))\nBody text.");
    assert!(
        p["word/document.xml"].contains("<w:background w:color=\"F0E6D2\"/>"),
        "solid page fill becomes a native w:background"
    );
    // A gradient page fill has no native `w:background` form and is left unset
    // (distinct from `background:`, which still rasterizes to a behindDoc image).
    let g = parts("#set page(fill: gradient.linear(red, blue))\nBody text.");
    assert!(
        !g["word/document.xml"].contains("<w:background"),
        "a gradient page fill is not forced into a flat w:background"
    );
    assert_all_wellformed(&p);
}

#[test]
fn inside_outside_page_margins_emit_gutter_and_mirror_margins() {
    let p = parts("#set page(margin: (inside: 3cm, outside: 2cm))\nBody text.");
    let doc = &p["word/document.xml"];
    let settings = &p["word/settings.xml"];
    assert!(
        settings.contains("<w:mirrorMargins/>"),
        "inside/outside margins enable Word mirrored margins"
    );
    assert!(
        doc.contains("w:gutter=\"567\""),
        "inside margin extra becomes a nonzero Word gutter"
    );
    assert!(
        doc.contains("w:left=\"1134\"") && doc.contains("w:right=\"1134\""),
        "outside margin is the base margin on both sides"
    );

    let plain = parts("#set page(margin: (left: 3cm, right: 2cm))\nBody text.");
    assert!(
        !plain["word/settings.xml"].contains("mirrorMargins"),
        "plain left/right margins do not enable mirrored margins"
    );
    assert!(
        plain["word/document.xml"].contains("w:gutter=\"0\""),
        "plain left/right margins keep a zero gutter"
    );
    assert_all_wellformed(&p);
    assert_all_wellformed(&plain);
}

#[test]
fn hyphenation_intent_becomes_auto_hyphenation_setting() {
    // `#set text(hyphenate: true)` (or plain justification, since `auto`
    // follows it) must survive as `w:autoHyphenation` — otherwise Word (which
    // defaults hyphenation OFF) silently drops the author's line-breaking
    // intent. This is a document-wide flag, read from the body's own resolved
    // style chain (NOT the pre-realize root styles, which don't see the body's
    // own `#set` rules).
    let explicit = parts("#set text(hyphenate: true)\nSome body text.");
    assert!(
        explicit["word/settings.xml"].contains("<w:autoHyphenation/>"),
        "explicit hyphenate: true sets autoHyphenation"
    );

    let via_justify = parts("#set par(justify: true)\nSome body text.");
    assert!(
        via_justify["word/settings.xml"].contains("<w:autoHyphenation/>"),
        "auto hyphenate follows justification, like Typst's own layout"
    );

    let off = parts("Some body text.");
    assert!(
        !off["word/settings.xml"].contains("autoHyphenation"),
        "no hyphenation intent means no setting (Word's own default)"
    );
    assert_all_wellformed(&explicit);
}

#[test]
fn framed_box_in_a_figure_is_rasterized_not_a_textbox() {
    // A framed box (`#figure(rect[..])`) is centered by the figure, and a
    // *centered* `wps:txbx` text box does not flow its text in LibreOffice. Such a
    // body must rasterize to a (centered) image so it renders in every consumer.
    let p =
        parts("#figure(rect(width: 3cm, height: 1cm, fill: aqua)[box], caption: [c])");
    let doc = &p["word/document.xml"];
    assert!(
        !doc.contains("<w:txbxContent"),
        "a framed box inside a figure must not become a centered text box"
    );
    assert!(doc.contains("<a:blip"), "it is rasterized to an inline image instead");
    // A *standalone* framed box (not centered) still uses a real text box.
    let q = parts("#rect(width: 3cm, height: 1cm, fill: aqua)[box]");
    assert!(
        q["word/document.xml"].contains("<w:txbxContent"),
        "a standalone framed box is still an editable text box"
    );
    assert_all_wellformed(&p);
}

#[test]
fn labeled_targets_get_bookmarks_so_refs_resolve() {
    // A `@ref` to a numbered equation and a `#link` to a plain labeled paragraph
    // both need a bookmark at the target, or the hyperlink anchor dangles. The
    // converter brackets labeled blocks (the equation) and labeled inline content
    // (the `<spot>` on a paragraph) with bookmarks.
    let p = parts(
        "#set math.equation(numbering: \"(1)\")\n\
         $ E = m c^2 $ <emc>\n\n\
         As in @emc.\n\n\
         A spot to jump to. <spot>\n\n\
         #link(<spot>)[go]",
    );
    let doc = &p["word/document.xml"];
    let collect = |key: &str, skip: usize| -> std::collections::BTreeSet<String> {
        doc.match_indices(key)
            .map(|(i, _)| {
                let s = &doc[i + skip..];
                s[..s.find('"').unwrap()].to_string()
            })
            .collect()
    };
    let anchors = collect("w:anchor=\"", 10);
    let bookmarks = collect("w:name=\"", 8);
    assert!(anchors.len() >= 2, "an equation ref and a label link, got {anchors:?}");
    let dangling: Vec<_> = anchors.difference(&bookmarks).collect();
    assert!(
        dangling.is_empty(),
        "every link anchor resolves to a bookmark: {dangling:?}"
    );
    assert_all_wellformed(&p);
}

#[test]
fn aligned_equation_keeps_its_alignment() {
    // `a + b &= c \ x &= y` must vertically align the `=` columns. `m:eqArr`
    // cannot express per-column alignment, so the converter emits a matrix whose
    // columns alternate right/left justification (matching Typst's layout).
    let p = parts("$ a + b &= c \\\n  x &= y $");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<m:m>"), "aligned equation becomes a matrix");
    // Two alignment columns: the first right-aligned, the second left-aligned.
    let jc: Vec<_> = doc
        .match_indices("m:mcJc m:val=\"")
        .map(|(i, _)| {
            let s = &doc[i + 14..];
            s[..s.find('"').unwrap()].to_string()
        })
        .collect();
    assert_eq!(jc, vec!["right", "left"], "columns alternate right/left");
    // Plain (unaligned) multi-line stays a centered equation array.
    let q = parts("$ a \\\n b $");
    assert!(q["word/document.xml"].contains("<m:eqArr"), "gather stays an eqArr");
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
    assert!(
        para.contains("text after it"),
        "text after the equation stays in the paragraph"
    );
    // The spaces flanking the equation must survive: Typst trims them when it
    // splits the paragraph at a raw inline equation, so the converter relies on
    // the PAR grouping rule keeping the equation inline. Check for a lone-space
    // run immediately before `<m:oMath>` and immediately after `</m:oMath>`.
    let omath = para.find("<m:oMath>").unwrap();
    let omath_end = para.find("</m:oMath>").unwrap();
    assert!(
        para[..omath].trim_end().ends_with("</w:r>")
            && para[..omath].contains("<w:t xml:space=\"preserve\"> </w:t>"),
        "a space run precedes the inline equation"
    );
    assert!(
        para[omath_end..].contains("<w:t xml:space=\"preserve\"> </w:t>"),
        "a space run follows the inline equation"
    );
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
fn header_link_relationship_lives_in_the_header_part_rels() {
    // A link/image in a header references a relationship by r:id; that id must
    // resolve against the header part's OWN .rels, not document.xml.rels, or Word
    // refuses to open the file.
    let p =
        parts("#set page(header: [#link(\"https://example.com\")[site] head])\nBody.");
    let header = p
        .keys()
        .find(|k| k.starts_with("word/header") && k.ends_with(".xml"))
        .expect("a header part");
    let rid = {
        let h = &p[header];
        let i = h.find("r:id=\"").expect("header references an r:id") + 6;
        h[i..][..h[i..].find('"').unwrap()].to_string()
    };
    let rels_name = format!("word/_rels/{}.rels", header.trim_start_matches("word/"));
    let rels = p.get(&rels_name).expect("the header part has its own .rels");
    assert!(
        rels.contains(&format!("Id=\"{rid}\"")) && rels.contains("example.com"),
        "the header's r:id resolves in its own .rels"
    );
    assert_all_wellformed(&p);
}

#[test]
fn hide_is_redaction_not_hidden_text() {
    // `#hide` is documented as a redaction tool ("neither present visually nor
    // accessible to Assistive Technology"), and paged export physically drops
    // the hidden frame items. The DOCX must match: the content may NOT ship
    // inside the package in any form — not even as `w:vanish` hidden text,
    // which Word reveals with a single toggle.
    let p = parts("Shown #hide[redactedsecret] and more.");
    let doc = &p["word/document.xml"];
    assert!(
        !doc.contains("redactedsecret"),
        "hidden content must not be recoverable from the package"
    );
    assert!(doc.contains("Shown"), "the visible text stays");
    assert_all_wellformed(&p);
}

#[test]
fn hide_is_redacted_at_block_level_too() {
    // The block path (a hidden heading) goes through the rasterize fallback,
    // where `Frame::hide` empties the frame — nothing may leak there either.
    let p = parts("Before\n\n#hide[= SecretHeading]\n\nAfter");
    let doc = &p["word/document.xml"];
    assert!(!doc.contains("SecretHeading"), "a hidden heading must not leak");
    assert_all_wellformed(&p);
}

#[test]
fn blank_raster_is_dropped_not_embedded() {
    // `#hide` keeps its space in layout, so a skewed hidden box lays out to a
    // real-sized frame that renders NOTHING. The old path embedded that blank
    // render as a full-size PNG (phantom space in the flow); the ink crop must
    // drop it outright.
    let p = parts("A #skew(ax: 20deg, box(width: 200pt, height: 100pt, hide[gone]))b");
    let doc = &p["word/document.xml"];
    assert!(!doc.contains("<a:blip"), "a blank render must not embed an image");
    assert_all_wellformed(&p);
}

#[test]
fn mostly_blank_raster_is_cropped_to_ink() {
    // A skewed wide box whose only ink is a small corner square: the raster
    // must be cropped to (roughly) the square, not shipped at the full
    // 300x100pt frame size. 300pt = 3_810_000 EMU; the cropped extent should
    // be a small fraction of that.
    let p = parts(
        "#skew(ax: 10deg, box(width: 300pt, height: 100pt, \
         align(bottom + end, square(size: 10pt, fill: red))))",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<a:blip"), "the skewed box still rasterizes");
    let cx: i64 = doc
        .split("<wp:extent cx=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.parse().ok())
        .expect("drawing has an extent");
    assert!(
        cx < 1_000_000,
        "the raster is cropped to its ink, not the 300pt frame (got {cx} EMU)"
    );
    assert_all_wellformed(&p);
}

#[test]
fn styled_underline_carries_dash_and_color() {
    // A plain underline stays a single, uncolored line; a styled one carries the
    // dash pattern as `w:val` and the paint as `w:color`.
    let p = parts(
        "#underline[plain] \
         #underline(stroke: red)[red] \
         #underline(stroke: (dash: \"dotted\"))[dotted] \
         #underline(stroke: (paint: blue, dash: \"dashed\"))[dash]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:u w:val=\"single\"/>"), "a plain underline stays single");
    assert!(
        doc.contains("<w:u w:val=\"single\" w:color=\"FF4136\"/>"),
        "a colored underline carries its paint as w:color"
    );
    assert!(doc.contains("<w:u w:val=\"dotted\"/>"), "a dotted dash maps to dotted");
    assert!(
        doc.contains("<w:u w:val=\"dash\" w:color=\"0074D9\"/>"),
        "a dashed blue underline carries both val and color"
    );
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
fn inline_styled_box_becomes_boxed_inline_text() {
    // An inline `#box(fill|stroke)[text]` becomes boxed *inline* text — run
    // shading (`w:shd`) + a run border (`w:bdr`) — which flows correctly in the
    // line. An inline Word text box does NOT flow its content (it renders as a
    // displaced empty frame), so it must not be used here.
    let p = parts(
        "Tail #box(fill: luma(230), stroke: 1pt + blue, inset: 6pt)\
         [a framed #link(\"https://typst.app\")[link]] end.",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:bdr"), "the box stroke becomes a run border");
    assert!(doc.contains("<w:shd "), "the box fill becomes run shading");
    assert!(doc.contains("a framed"), "the text is real and inline");
    assert!(!doc.contains("wps:txbx"), "and NOT an ill-flowing inline text box");
    assert!(!p.keys().any(|k| k.starts_with("word/media/")), "nor a raster");
    // A link inside the inline box stays clickable, with the box styling on its
    // run (the <w:hyperlink> wrapper survives at the paragraph-child level).
    assert!(doc.contains("<w:hyperlink"), "a link inside the box stays clickable");
    let hl = &doc[doc.find("<w:hyperlink").unwrap()..];
    let hl = &hl[..hl.find("</w:hyperlink>").unwrap()];
    assert!(
        hl.contains("<w:bdr") && hl.contains("<w:shd "),
        "with the box's shading + border"
    );
    assert_all_wellformed(&p);
}

#[test]
fn rect_with_text_becomes_a_text_box() {
    // A `#rect`/`#square` carrying content (a callout) is a text box too — not a
    // bodyless decorative shape and not a raster.
    let p = parts("#rect(fill: aqua, inset: 6pt)[A boxed callout note.]");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("wps:txbx"), "a rect with text is a text box");
    assert!(doc.contains("A boxed callout note"), "its text is real and editable");
    assert!(!p.keys().any(|k| k.starts_with("word/media/")), "and nothing is rasterized");
    assert_all_wellformed(&p);
}

#[test]
fn block_level_callout_flows_as_a_shaded_paragraph() {
    // A block-level framed container with flowing content (a multi-paragraph
    // callout, a code listing) maps to shaded + bordered paragraphs that break
    // across pages — NOT a text box (which would clip if taller than a page).
    let p = parts(
        "#rect(fill: luma(230), stroke: 1pt + blue, inset: 8pt)[\
         First callout paragraph.\n\nSecond callout paragraph.]",
    );
    let doc = &p["word/document.xml"];
    assert!(!doc.contains("wps:txbx"), "a flowing block callout is not a text box");
    assert!(doc.contains("<w:pBdr>"), "it carries paragraph borders");
    assert!(doc.contains("<w:shd "), "and paragraph shading");
    assert!(doc.contains("keepNext"), "multi-paragraph box is held together");
    assert!(
        doc.contains("First callout") && doc.contains("Second callout"),
        "text flows"
    );
    assert_all_wellformed(&p);
}

#[test]
fn footnote_in_a_box_never_lands_in_a_text_box() {
    // Word forbids a footnote inside a text box (`wps:txbx`) — the file fails to
    // open. A footnote-bearing framed container must stay in the main story: an
    // inline box extracts frameless, a block callout flows as a shaded paragraph.
    // Either way there must be NO text box, and the footnote body must be emitted.
    let inline = parts("Tail #box(fill: aqua)[note#footnote[the note]] end.");
    assert!(
        !inline["word/document.xml"].contains("wps:txbx"),
        "an inline box with a footnote must not become a text box"
    );
    assert!(
        inline.contains_key("word/footnotes.xml"),
        "and the footnote body is emitted"
    );
    assert_all_wellformed(&inline);

    let block =
        parts("#rect(fill: green, inset: 6pt)[Callout with a #footnote[fn] here.]");
    assert!(
        !block["word/document.xml"].contains("wps:txbx"),
        "a block callout with a footnote flows as a shaded paragraph, not a text box"
    );
    assert!(block["word/document.xml"].contains("<w:shd "), "with shading preserved");
    assert!(block.contains_key("word/footnotes.xml"), "and the footnote body is emitted");
    assert_all_wellformed(&block);
}

#[test]
fn figure_in_a_box_is_not_a_text_box() {
    // A figure/image/table inside a framed container must NOT become a text box
    // (Word-fragile, and the size+extract double-layout corrupts its cross-ref
    // number). It flows as a shaded paragraph instead, which lays out once.
    let p = parts(
        "#figure(rect(width: 1cm, height: 1cm), caption: [A]) <a>\n\n\
         #rect(fill: aqua)[#figure(rect(width: 1cm, height: 1cm), caption: [B]) <b>]\n\n\
         See @a and @b.",
    );
    let doc = &p["word/document.xml"];
    assert!(!doc.contains("wps:txbx"), "a figure-bearing box is not a text box");
    assert!(doc.contains("<w:shd "), "it flows as a shaded paragraph");
    assert_all_wellformed(&p);
}

#[test]
fn short_block_rect_stays_a_text_box() {
    // A short single-line framed container keeps the sized text-box look.
    let p = parts("#rect(fill: yellow, inset: 4pt)[Short label]");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("wps:txbx"), "a short framed label is a sized text box");
    assert!(doc.contains("Short label"), "with its text");
    assert_all_wellformed(&p);
}

#[test]
fn leading_page_setup_does_not_emit_a_blank_first_page() {
    // A document that opens with `#set page(..)` gets a synthetic leading
    // pagebreak; emitting it as `<w:br w:type="page"/>` would add a blank first
    // page. It must be dropped — but a real `#pagebreak()` after content is kept.
    let p = parts("#set page(\"a5\")\n= Heading\n\nBody.");
    let doc = &p["word/document.xml"];
    assert!(
        !doc.contains("w:type=\"page\""),
        "a leading page-setup break must not become a page break"
    );

    let q = parts("First.\n\n#pagebreak()\n\nSecond.");
    assert_eq!(
        q["word/document.xml"].matches("w:type=\"page\"").count(),
        1,
        "a real mid-document pagebreak is preserved"
    );
    assert_all_wellformed(&p);
}

#[test]
fn pagebreak_to_parity_emits_odd_even_section_breaks() {
    // Word's `w:type` describes how *that* section itself starts (relative
    // to the one before it) — so the oddPage/evenPage constraint requested
    // by `#pagebreak(to: ..)` must land on the section that begins *after*
    // the break (here, the final/body-level sectPr), not the one ending at
    // the break (the first, paragraph-embedded sectPr, which has no
    // predecessor and so carries no type). Landing it on the wrong section
    // makes the constraint a no-op in real Word: verified interactively that
    // only this placement actually makes Word insert a blank page to reach
    // the next odd page.
    let odd = parts("Before.\n#pagebreak(to: \"odd\")\nAfter.");
    let odd_doc = &odd["word/document.xml"];
    let odd_sects = sect_pr_chunks(odd_doc);
    assert_eq!(
        odd_sects.len(),
        2,
        "odd pagebreak should split the document into two sections"
    );
    assert!(
        !odd_sects[0].contains("<w:type"),
        "the first section has no predecessor to transition from"
    );
    assert!(
        odd_sects[1].contains("<w:type w:val=\"oddPage\"/>"),
        "the section starting after the break carries the oddPage constraint"
    );
    assert!(
        !odd_doc.contains("w:type=\"page\""),
        "parity pagebreak is not emitted as a plain page break"
    );

    let even = parts("Before.\n#pagebreak(to: \"even\")\nAfter.");
    assert!(
        even["word/document.xml"].contains("<w:type w:val=\"evenPage\"/>"),
        "even pagebreak uses an evenPage section break"
    );
    assert_all_wellformed(&odd);
    assert_all_wellformed(&even);
}

#[test]
fn plain_pagebreak_stays_a_page_break() {
    let p = parts("Before.\n#pagebreak()\nAfter.");
    let doc = &p["word/document.xml"];
    assert_eq!(
        doc.matches("w:type=\"page\"").count(),
        1,
        "plain pagebreak remains a run-level page break"
    );
    assert!(
        !doc.contains("oddPage") && !doc.contains("evenPage"),
        "plain pagebreak does not become a parity section"
    );
    assert_all_wellformed(&p);
}

#[test]
fn bodyless_rect_stays_a_vector_shape() {
    // A `#rect` with no body is still a bare decorative vector shape, not a
    // (empty) text box.
    let p = parts("#rect(width: 2cm, height: 1cm, fill: blue)");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("wps:wsp"), "a bodyless rect is a vector shape");
    assert!(!doc.contains("wps:txbx"), "with no text-box content");
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
    let p =
        parts("#set page(header: box(fill: blue, width: 30pt, height: 8pt))\n\nBody.");
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
    let p = parts("Portrait body.\n\n#set page(flipped: true)\n\nLandscape body.");
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
fn header_only_change_emits_a_section_break() {
    // A mid-document `set page(header: ..)` change with UNCHANGED geometry must
    // still start a new section — Word carries running headers on `sectPr`, so
    // merging the runs would silently keep the first header for the whole
    // document.
    let p = parts(
        "#set page(header: [First head])\nBody one.\n\n\
         #set page(header: [Second head])\nBody two.",
    );
    let doc = &p["word/document.xml"];
    assert_eq!(
        doc.matches("<w:sectPr>").count(),
        2,
        "a header-only change should yield two sections"
    );
    let headers: String = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/header"))
        .map(|(_, xml)| xml.as_str())
        .collect();
    assert!(headers.contains("First head"), "the first header is emitted");
    assert!(headers.contains("Second head"), "the second header is emitted");
    assert_all_wellformed(&p);
}

#[test]
fn page_reference_resolves_via_the_synthetic_page_model() {
    // `@target(form: "page")` needs `page_numbering()` + a page number from the
    // introspector — both used to be `None` (pageless), failing the whole
    // export. The synthetic model counts explicit page breaks: the target sits
    // after one `#pagebreak()`, so its synthetic page is 2 — "ii" under roman
    // page numbering.
    let p = parts(
        "#set page(numbering: \"i\")\nIntro.\n#pagebreak()\n= Target <t>\n\
         Body.\n#pagebreak()\nSee #ref(<t>, form: \"page\").",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("ii"), "the page reference resolves to the synthetic page");
    assert_all_wellformed(&p);
}

#[test]
fn docx_locations_match_paged_locations_for_source_elements() {
    // The real paged introspector can only back DOCX realization if source-
    // derived locations line up across the Paged and Docx targets. Check both
    // layers directly: the paged introspector and the DOCX synthetic fallback
    // should assign the same `Location` to ordinary source labels.
    let (paged, docx) = compile_paged_and_docx(
        "= Intro <intro>\n\nBody.\n#pagebreak()\n= Second <second>\nMore.",
    );

    for name in ["intro", "second"] {
        let label = Label::new(PicoStr::intern(name)).unwrap();
        let paged_loc = paged
            .introspector()
            .query_label(label)
            .expect("label exists in paged document")
            .location()
            .expect("paged label has a location");
        let docx_loc = docx
            .introspector()
            .elements()
            .query_label(label)
            .expect("label exists in docx realization")
            .location()
            .expect("docx label has a location");
        assert_eq!(docx_loc, paged_loc, "location mismatch for <{name}>");
    }
}

#[test]
fn body_here_page_uses_real_paged_page() {
    let p = parts(
        "#set page(numbering: \"1\")\n\
         First page.\n#pagebreak()\n\
         #context [BODY-#here().page()-END]",
    );
    let text = visible_text(&p["word/document.xml"]);
    assert!(text.contains("BODY-2-END"), "body `here().page()` uses page 2");
    assert_all_wellformed(&p);
}

#[test]
fn header_here_page_uses_real_paged_page() {
    // Paged layout discovers tags in page furniture. Because repeated header
    // content deduplicates by location, the shared header part should bake the
    // first real paged occurrence, not the synthetic fallback's final-page
    // guess.
    let p = parts(
        "#set page(header: context [HEAD-#here().page()-END])\n\
         First page.\n#pagebreak()\nSecond page.",
    );
    let header = p
        .iter()
        .find(|(name, _)| name.starts_with("word/header"))
        .map(|(_, xml)| xml)
        .expect("a header part should exist");
    let text = visible_text(header);
    assert!(text.contains("HEAD-1-END"), "header uses the first real page");
    assert!(
        !text.contains("HEAD-2-END"),
        "header must not fall back to the synthetic final page"
    );
    assert_all_wellformed(&p);
}

#[test]
fn contextual_odd_even_furniture_emits_even_references_and_setting() {
    let p = parts(
        "#set page(\n\
         \theader: context if calc.odd(here().page()) [Odd header] else [Even header],\n\
         \tfooter: context if calc.odd(here().page()) [Odd footer] else [Even footer],\n\
         )\n\
         First page.\n#pagebreak()\nSecond page.\n#pagebreak()\nThird page.",
    );
    let doc = &p["word/document.xml"];
    let settings = &p["word/settings.xml"];

    assert!(
        settings.contains("<w:evenAndOddHeaders/>"),
        "even/odd references require the document-level Word setting"
    );
    assert!(doc.contains("<w:headerReference w:type=\"default\""));
    assert!(doc.contains("<w:headerReference w:type=\"even\""));
    assert!(doc.contains("<w:footerReference w:type=\"default\""));
    assert!(doc.contains("<w:footerReference w:type=\"even\""));
    assert!(!doc.contains("<w:titlePg"), "parity-only furniture is not first-page");

    let headers: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/header") && name.ends_with(".xml"))
        .map(|(_, xml)| visible_text(xml))
        .collect();
    assert!(headers.iter().any(|text| text.contains("Odd header")));
    assert!(headers.iter().any(|text| text.contains("Even header")));

    let footers: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/footer") && name.ends_with(".xml"))
        .map(|(_, xml)| visible_text(xml))
        .collect();
    assert!(footers.iter().any(|text| text.contains("Odd footer")));
    assert!(footers.iter().any(|text| text.contains("Even footer")));
    assert_all_wellformed(&p);
}

#[test]
fn block_columns_keep_contextual_odd_even_furniture() {
    let p = parts(
        "#set page(\n\
         \theader: context if calc.odd(here().page()) [Odd header] else [Even header],\n\
         \tfooter: context if calc.odd(here().page()) [Odd footer] else [Even footer],\n\
         )\n\
         Before.\n\
         #columns(2)[Column section body.]\n\
         #pagebreak()\nSecond page.\n#pagebreak()\nThird page.",
    );
    let doc = &p["word/document.xml"];
    let settings = &p["word/settings.xml"];

    assert_eq!(sect_pr_chunks(doc).len(), 3, "columns still split the body");
    assert!(settings.contains("<w:evenAndOddHeaders/>"));
    assert!(doc.contains("<w:headerReference w:type=\"default\""));
    assert!(doc.contains("<w:headerReference w:type=\"even\""));
    assert!(doc.contains("<w:footerReference w:type=\"default\""));
    assert!(doc.contains("<w:footerReference w:type=\"even\""));

    let headers: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/header") && name.ends_with(".xml"))
        .map(|(_, xml)| visible_text(xml))
        .collect();
    assert!(headers.iter().any(|text| text.contains("Odd header")));
    assert!(headers.iter().any(|text| text.contains("Even header")));

    let footers: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/footer") && name.ends_with(".xml"))
        .map(|(_, xml)| visible_text(xml))
        .collect();
    assert!(footers.iter().any(|text| text.contains("Odd footer")));
    assert!(footers.iter().any(|text| text.contains("Even footer")));
    assert_all_wellformed(&p);
}

#[test]
fn contextual_first_page_header_emits_titlepg_and_first_reference() {
    let p = parts(
        "#set page(header: context if here().page() == 1 [First header] else [Rest header])\n\
         First page.\n#pagebreak()\nSecond page.\n#pagebreak()\nThird page.",
    );
    let doc = &p["word/document.xml"];
    let settings = &p["word/settings.xml"];

    assert!(doc.contains("<w:titlePg/>"), "section enables first-page header");
    assert!(doc.contains("<w:headerReference w:type=\"first\""));
    assert!(doc.contains("<w:headerReference w:type=\"default\""));
    assert!(!doc.contains("<w:headerReference w:type=\"even\""));
    assert!(
        !settings.contains("<w:evenAndOddHeaders/>"),
        "first-page-only furniture does not enable even/odd mode"
    );

    let headers: Vec<_> = p
        .iter()
        .filter(|(name, _)| name.starts_with("word/header") && name.ends_with(".xml"))
        .map(|(_, xml)| visible_text(xml))
        .collect();
    assert!(headers.iter().any(|text| text.contains("First header")));
    assert!(headers.iter().any(|text| text.contains("Rest header")));
    assert_all_wellformed(&p);
}

#[test]
fn per_page_literal_header_does_not_fake_an_odd_even_split() {
    // A literal page number changes on page 3 vs page 5. Word's
    // first/even/default references cannot express that, so the exporter must
    // keep the existing single sampled header instead of pretending page 3 is
    // the header for every later odd page.
    let p = parts(
        "#set page(header: context [HEAD-#here().page()-END])\n\
         One.\n#pagebreak()\nTwo.\n#pagebreak()\nThree.\n#pagebreak()\nFour.\n#pagebreak()\nFive.",
    );
    let doc = &p["word/document.xml"];
    let settings = &p["word/settings.xml"];

    assert!(!settings.contains("<w:evenAndOddHeaders/>"));
    assert!(!doc.contains("<w:titlePg"));
    assert!(!doc.contains("w:type=\"even\""));
    assert_eq!(
        p.keys()
            .filter(|name| name.starts_with("word/header") && name.ends_with(".xml"))
            .count(),
        1,
        "non-parity-stable furniture stays a single sampled header"
    );
    assert_all_wellformed(&p);
}

#[test]
fn citations_and_bibliography_converge_against_paged_introspection() {
    let p = parts_with_files(
        "First @beta and then @alpha.\n\n#bibliography(\"refs.bib\", style: \"ieee\")",
        &[("refs.bib", REFS_BIB)],
    );
    let text = visible_text(&p["word/document.xml"]);
    assert!(text.contains("[1]"), "first citation number is present: {text}");
    assert!(text.contains("[2]"), "second citation number is present: {text}");
    assert!(text.contains("Beta Source"), "first cited bibliography entry is present");
    assert!(text.contains("Alpha Source"), "second cited bibliography entry is present");
    assert_all_wellformed(&p);
}

#[test]
fn bibliography_gets_a_biblatex_sidecar_part() {
    // The visible body keeps the realized, formatted citation text (Word's own
    // CITATION/BIBLIOGRAPHY field model is proprietary and lossy relative to
    // Typst/Hayagriva); the sidecar is metadata only, for external tools that
    // want the structured bibliography back.
    let p = parts_with_files(
        "First @beta and then @alpha.\n\n#bibliography(\"refs.bib\", style: \"ieee\")",
        &[("refs.bib", REFS_BIB)],
    );
    let sidecar = p
        .get("word/typstBibliography.xml")
        .expect("bibliography sidecar part should be present");
    assert!(sidecar.contains("https://typst.app/schema/2026/docx-bibliography"));
    assert!(sidecar.contains("@article{alpha") || sidecar.contains("@article{beta"));
    assert!(sidecar.contains("Alpha Source"));
    assert!(sidecar.contains("Beta Source"));

    let rels = &p["word/_rels/document.xml.rels"];
    assert!(
        rels.contains("https://typst.app/schema/2026/relationships/bibliography"),
        "document.xml.rels should reference the sidecar under a private relationship type"
    );
    assert!(rels.contains("Target=\"typstBibliography.xml\""));

    let content_types = &p["[Content_Types].xml"];
    assert!(
        content_types.contains("/word/typstBibliography.xml"),
        "the sidecar's content type must be declared or strict consumers repair the file"
    );
    assert_all_wellformed(&p);
}

#[test]
fn no_bibliography_means_no_sidecar_part() {
    let p = parts("Just some plain text, no citations at all.");
    assert!(
        !p.contains_key("word/typstBibliography.xml"),
        "a document with no bibliography should not get a sidecar part"
    );
    assert!(!p["word/_rels/document.xml.rels"].contains("docx-bibliography"));
    assert_all_wellformed(&p);
}

#[test]
fn bibliography_gets_a_native_word_sources_part() {
    // Alongside the lossless BibLaTeX sidecar, the document should also carry
    // Word's own `b:Sources` schema so References -> Manage Sources shows
    // real, correctly-typed sources — without live CITATION/BIBLIOGRAPHY
    // fields wrapping the visible (already-realized) body text.
    let p = parts_with_files(
        "First @beta and then @alpha.\n\n#bibliography(\"refs.bib\", style: \"ieee\")",
        &[("refs.bib", REFS_BIB)],
    );

    let item1 = p
        .get("customXml/item1.xml")
        .expect("native b:Sources part should be present");
    assert!(item1.contains("<b:Sources"));
    assert!(item1.contains(
        "xmlns:b=\"http://schemas.openxmlformats.org/officeDocument/2006/bibliography\""
    ));
    assert_eq!(item1.matches("<b:Source>").count(), 2, "one b:Source per cited entry");
    assert!(item1.contains("<b:Tag>alpha</b:Tag>"));
    assert!(item1.contains("<b:Tag>beta</b:Tag>"));
    assert!(item1.contains("<b:SourceType>ArticleInAPeriodical</b:SourceType>"));
    assert!(item1.contains("<b:Title>Alpha Source</b:Title>"));
    assert!(item1.contains("<b:Title>Beta Source</b:Title>"));
    assert!(item1.contains("<b:Year>2020</b:Year>"));
    assert!(item1.contains("<b:Year>2021</b:Year>"));
    assert!(item1.contains("<b:JournalName>Journal of Sources</b:JournalName>"));
    assert!(item1.contains("<b:Last>Able</b:Last>"));
    assert!(item1.contains("<b:First>Alice</b:First>"));
    assert!(item1.contains("<b:Last>Baker</b:Last>"));
    assert!(item1.contains("<b:First>Bob</b:First>"));
    // Two distinct, deterministic GUIDs (repeat exports must be byte-identical).
    let guids: Vec<&str> = item1.match_indices("<b:Guid>").map(|(i, _)| &item1[i..i + 46]).collect();
    assert_eq!(guids.len(), 2);
    assert_ne!(guids[0], guids[1], "each source gets its own GUID");

    let item_props = p
        .get("customXml/itemProps1.xml")
        .expect("schema-association part should accompany item1.xml");
    assert!(item_props.contains("<ds:datastoreItem"));
    assert!(item_props.contains(
        "xmlns:ds=\"http://schemas.openxmlformats.org/officeDocument/2006/customXml\""
    ));
    assert!(item_props.contains(
        "ds:uri=\"http://schemas.openxmlformats.org/officeDocument/2006/bibliography\""
    ));

    let item_rels = p
        .get("customXml/_rels/item1.xml.rels")
        .expect("item1.xml needs its own rels part pointing at itemProps1.xml");
    assert!(item_rels.contains("customXmlProps"));
    assert!(item_rels.contains("Target=\"itemProps1.xml\""));

    let doc_rels = &p["word/_rels/document.xml.rels"];
    assert!(
        doc_rels.contains(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml"
        ),
        "document.xml.rels should reference item1.xml under the real customXml relationship type"
    );
    assert!(doc_rels.contains("Target=\"../customXml/item1.xml\""));

    let content_types = &p["[Content_Types].xml"];
    assert!(content_types.contains("/customXml/itemProps1.xml"));
    assert!(content_types.contains("customXmlProperties+xml"));

    assert_all_wellformed(&p);
}

#[test]
fn no_bibliography_means_no_native_word_sources_part() {
    let p = parts("Just some plain text, no citations at all.");
    assert!(!p.contains_key("customXml/item1.xml"));
    assert!(!p.contains_key("customXml/itemProps1.xml"));
    assert!(!p.contains_key("customXml/_rels/item1.xml.rels"));
    assert!(!p["word/_rels/document.xml.rels"].contains("customXml"));
    assert_all_wellformed(&p);
}

#[test]
fn native_word_sources_excludes_uncited_library_entries() {
    // A `.bib` source file commonly holds far more entries than any one
    // document cites (a shared master reference list). References -> Manage
    // Sources should reflect what THIS document actually cites, not the
    // whole backing file.
    let p = parts_with_files(
        "Only @alpha is cited here.\n\n#bibliography(\"refs.bib\", style: \"ieee\")",
        &[("refs.bib", REFS_BIB_WITH_UNCITED)],
    );
    let item1 = &p["customXml/item1.xml"];
    assert_eq!(item1.matches("<b:Source>").count(), 1, "only the cited entry is included");
    assert!(item1.contains("<b:Tag>alpha</b:Tag>"));
    assert!(!item1.contains("<b:Tag>beta</b:Tag>"), "beta was never cited");
    assert!(!item1.contains("<b:Tag>gamma</b:Tag>"), "gamma was never cited");
    assert_all_wellformed(&p);
}

#[test]
fn native_word_sources_full_flag_includes_uncited_entries() {
    // `#bibliography(full: true)` prints every reference from the library,
    // cited or not — the native sources part should mirror that.
    let p = parts_with_files(
        "Only @alpha is cited here.\n\n#bibliography(\"refs.bib\", style: \"ieee\", full: true)",
        &[("refs.bib", REFS_BIB_WITH_UNCITED)],
    );
    let item1 = &p["customXml/item1.xml"];
    assert_eq!(item1.matches("<b:Source>").count(), 3, "full: true includes every entry");
    assert!(item1.contains("<b:Tag>alpha</b:Tag>"));
    assert!(item1.contains("<b:Tag>beta</b:Tag>"));
    assert!(item1.contains("<b:Tag>gamma</b:Tag>"));
    assert_all_wellformed(&p);
}

#[test]
fn native_word_sources_dedupes_across_bibliography_elements() {
    // Two separate #bibliography() calls loading the same file (e.g. a
    // shared references list split by chapter) can both decode the same
    // cited key. The native sources part must not emit `alpha` twice with
    // the same Tag/Guid.
    let p = parts_with_files(
        "First chapter cites @alpha.\n\n#bibliography(\"refs.bib\", style: \"ieee\")\n\n\
         Second chapter also cites @alpha.\n\n#bibliography(\"refs.bib\", style: \"ieee\")",
        &[("refs.bib", REFS_BIB_WITH_UNCITED)],
    );
    let item1 = &p["customXml/item1.xml"];
    assert_eq!(
        item1.matches("<b:Tag>alpha</b:Tag>").count(),
        1,
        "the shared cited entry appears exactly once, not once per bibliography element"
    );
    assert_all_wellformed(&p);
}

#[test]
fn page_refs_follow_real_numbering_across_sections() {
    let p = parts(
        "#set page(numbering: \"i\")\n\
         Front <front>\n#pagebreak()\n\
         #set page(numbering: \"1\")\n#counter(page).update(1)\n\
         Main <main>\n\n\
         #context [FRONT-#ref(<front>, form: \"page\") MAIN-#ref(<main>, form: \"page\")]",
    );
    let text = visible_text(&p["word/document.xml"]).replace('\u{a0}', " ");
    assert!(text.contains("FRONT-page i"), "front matter keeps roman numbering: {text}");
    assert!(text.contains("MAIN-page 1"), "main matter resets to arabic 1: {text}");
    assert_all_wellformed(&p);
}

#[test]
fn leading_page_setup_does_not_advance_the_synthetic_page() {
    // A top-of-document `set page(..)` produces page-run machinery before the
    // first real body content. That setup must not count as a physical page,
    // otherwise the first content page is reported as page 2.
    let p = parts(
        "#set page(numbering: \"1\")\n= Target <t>\nUNIQUE-#ref(<t>, form: \"page\")-END",
    );
    let doc = &p["word/document.xml"];
    let text = visible_text(doc).replace('\u{a0}', " ");
    assert!(
        text.contains("UNIQUE-page 1-END"),
        "the first content page stays page 1: {text}"
    );
    assert!(
        !text.contains("UNIQUE-page 2-END"),
        "leading page setup must not advance to page 2"
    );
    assert_all_wellformed(&p);
}

#[test]
fn synthetic_positions_distinguish_adjacent_blocks() {
    // Some templates compare `location().position()` values. DOCX positions are
    // approximate, but they must preserve block order instead of reporting every
    // location at origin.
    let p = parts(
        "= First <first>\n\
         = Second <second>\n\
         #context {\n\
         \tlet a = query(<first>).first().location().position()\n\
         \tlet b = query(<second>).first().location().position()\n\
         \tif b.y > a.y [ORDERED] else [ORIGIN]\n\
         }",
    );
    let doc = &p["word/document.xml"];
    assert!(
        visible_text(doc).contains("ORDERED"),
        "later blocks get later synthetic positions"
    );
    assert_all_wellformed(&p);
}

#[test]
fn scaffolding_state_update_stays_in_document_order() {
    // The `drafting` package stores page properties via
    // `box(place(layout(size => state.update(..))))` and its margin notes read
    // that state at their own (later) position. The update's introspection tag
    // must land AT the scaffolding's position — deferring it to the end of the
    // document makes every earlier-or-equal read see the initial value.
    let p = parts(
        "#let s = state(\"pgprops\", none)\n\
         Before.\n\
         #box(place(layout(size => s.update(size.width))))\n\
         #context if s.get() != none [INITIALIZED] else [MISSING]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("INITIALIZED"), "the state update precedes the read");
    assert!(!doc.contains("MISSING"), "the update tag must not defer to the end");
    assert_all_wellformed(&p);
}

#[test]
fn labels_inside_mixed_placed_boxes_stay_queryable() {
    // A placed body mixing shapes with labeled text (a sidenote with a rule
    // line) must not be claimed by the native shape-composition path: on early
    // iterations unresolved text can render empty, making the frame look
    // shape-only and the lowering flap — the label's tag then flickers across
    // iterations and queries never stabilise.
    let p = parts(
        "#box(place(dx: 2pt, rect(width: 4pt, height: 4pt, fill: blue) + \
         [note <sidenote>]))\n\
         #context if query(<sidenote>).len() > 0 [FOUND] else [MISSING]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("FOUND"), "the placed label is queryable");
    assert!(!doc.contains("MISSING"), "the label tag must be stable");
    assert_all_wellformed(&p);
}

#[test]
fn footer_labels_reach_the_introspector() {
    // A labeled element in the page footer is a real query target (templates
    // read page furniture state via `query(<label>)`), but footer content
    // lives outside the body IR — its tags must be harvested explicitly.
    let p = parts(
        "#set page(footer: [foot <ftr>])\n\
         Body #context if query(<ftr>).len() > 0 [FOUND] else [MISSING]",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("FOUND"), "the footer label is queryable");
    assert!(!doc.contains("MISSING"), "the footer label must not be invisible");
    assert_all_wellformed(&p);
}

#[test]
fn failing_figure_numbering_closure_does_not_abort_the_export() {
    // A user numbering closure that errors (e.g. reads introspection state that
    // only exists in a paged model) must not abort the export: the caption's
    // cached number is best-effort — the SEQ field is the live truth in Word.
    let p = parts(
        "#set figure(numbering: _ => if target() == \"docx\" { (1,).at(9) } else { \"1\" })\n\
         #figure(rect(), caption: [Survives])",
    );
    let doc = &p["word/document.xml"];
    assert!(doc.contains("Survives"), "the caption text is kept");
    assert!(doc.contains(" SEQ "), "the live SEQ field is still emitted");
    assert_all_wellformed(&p);
}

#[test]
fn numbering_only_change_emits_a_section_break() {
    // Front-matter roman numerals switching to arabic (`set page(numbering:)`)
    // is section-scoped in Word (`w:pgNumType`); same-geometry runs must not
    // merge across it.
    let p = parts(
        "#set page(numbering: \"i\")\nFront matter.\n\n\
         #set page(numbering: \"1\")\nMain matter.",
    );
    let doc = &p["word/document.xml"];
    assert_eq!(
        doc.matches("<w:sectPr>").count(),
        2,
        "a numbering-only change should yield two sections"
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

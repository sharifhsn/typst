//! End-to-end integration test for the DOCX → Typst importer: builds a small
//! but realistic Word document in memory (via the OPC writer the exporter also
//! uses, so the reader gets a genuinely well-formed package) and asserts the
//! emitted Typst source is idiomatic — headings as `=`, emphasis as
//! `*`/`_`, and lists as `-`/`+`.

use typst_docx_import::{import_docx, import_docx_with, ImportOptions, Tier};
use typst_ooxml_core::opc::{Package, PackageOptions, RelMode, Rels};

const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
    <w:r><w:t>Project Overview</w:t></w:r></w:p>
  <w:p>
    <w:r><w:t xml:space="preserve">The status is </w:t></w:r>
    <w:r><w:rPr><w:b/></w:rPr><w:t>on track</w:t></w:r>
    <w:r><w:t xml:space="preserve"> and the risk is </w:t></w:r>
    <w:r><w:rPr><w:i/></w:rPr><w:t>low</w:t></w:r>
    <w:r><w:t>.</w:t></w:r>
  </w:p>
  <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
    <w:r><w:t>Tasks</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr></w:pPr>
    <w:r><w:t>Write the parser</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr></w:pPr>
    <w:r><w:t>Write the emitter</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
</w:body>
</w:document>"#;

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/></w:style>
</w:styles>"#;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;

fn build_docx() -> Vec<u8> {
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
    package.add_xml("word/numbering.xml", "application/xml", NUMBERING_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    package.finish(&Rels::new()).unwrap()
}

#[test]
fn imports_to_idiomatic_typst() {
    let docx = build_docx();
    let result = import_docx(&docx).expect("import should succeed");
    let src = &result.source;

    // Headings recovered from the Word heading styles.
    assert!(src.contains("= Project Overview"), "missing H1:\n{src}");
    assert!(src.contains("== Tasks"), "missing H2:\n{src}");

    // Emphasis promoted to markup (tier-2), not literal #text wrappers.
    assert!(src.contains("*on track*"), "bold not promoted:\n{src}");
    assert!(src.contains("_low_"), "italic not promoted:\n{src}");
    assert!(!src.contains("weight: \"bold\""), "left a literal weight wrapper:\n{src}");

    // The bullet list.
    assert!(src.contains("- Write the parser"), "missing bullet item:\n{src}");
    assert!(src.contains("- Write the emitter"), "missing bullet item:\n{src}");

    // Full sentence stays on one line (runs not fragmented across paragraphs).
    assert!(
        src.contains("The status is *on track* and the risk is _low_."),
        "sentence fragmented:\n{src}"
    );

    // Page geometry hoisted to a preamble.
    assert!(src.contains("#set page("), "missing page setup:\n{src}");
}

#[test]
fn literal_tier_keeps_explicit_formatting() {
    let docx = build_docx();
    let opts = ImportOptions { tier: Tier::Literal, ..Default::default() };
    let result = import_docx_with(&docx, &opts).expect("import should succeed");
    // In literal tier, bold stays an explicit #text(weight: "bold") wrapper
    // rather than being promoted to `*...*`.
    assert!(
        result.source.contains("weight: \"bold\""),
        "literal tier should keep the explicit weight:\n{}",
        result.source
    );
    assert!(!result.source.contains("*on track*"));
}

#[test]
fn not_a_word_document_is_an_error() {
    // A zip that isn't a Word package.
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("some/other.xml", "application/xml", "<x/>".into());
    let bytes = package.finish(&Rels::new()).unwrap();
    assert!(import_docx(&bytes).is_err());
}

/// A pathologically deep document (POI's `deep-table-cell.docx` nests 5000
/// tables) must be *refused with an error*, never crash the process with a
/// stack overflow. Guards the XML-depth limit in `opc::Reader`. The fixture is
/// assembled as a raw OPC zip — the shape of a genuine `.docx` — rather than
/// via the `Package` writer (which is for well-formed output, not synthetic
/// torture input).
#[test]
fn deeply_nested_tables_are_refused_not_crashed() {
    use std::io::{Cursor, Write};

    let mut doc = String::from(
        r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    let depth = 1000; // > the 256 default cap, < a real stack-overflow depth
    for _ in 0..depth {
        doc.push_str("<w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p>");
    }
    for _ in 0..depth {
        doc.push_str("</w:tc></w:tr></w:tbl>");
    }
    doc.push_str("</w:body></w:document>");

    const CT: &str = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    const RELS: &str = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
    for (name, body) in [
        ("[Content_Types].xml", CT),
        ("_rels/.rels", RELS),
        ("word/document.xml", doc.as_str()),
    ] {
        zip.start_file(name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();

    // Returns an error rather than aborting — the whole point.
    assert!(import_docx(&bytes).is_err());
}

/// An irregular table — a short row followed by a wide-spanning row — must
/// still lower to a `#table` whose every row fills the column grid exactly, so
/// Typst's cell auto-flow never overflows ("colspan would exceed the available
/// columns"). Reproduces the shape that broke on POI's `drawing.docx`.
#[test]
fn irregular_table_rows_stay_compilable() {
    // Row 1: one cell (a short row). Row 2: a single cell spanning 3 columns.
    // A naive lowering leaves row 1 short, desyncing the flow so row 2's
    // colspan overflows.
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:tbl>
    <w:tblGrid><w:gridCol w:w="100"/><w:gridCol w:w="100"/><w:gridCol w:w="100"/></w:tblGrid>
    <w:tr><w:tc><w:p><w:r><w:t>solo</w:t></w:r></w:p></w:tc></w:tr>
    <w:tr><w:tc><w:tcPr><w:gridSpan w:val="3"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("#table("), "expected a table:\n{src}");
    assert!(src.contains("colspan: 3"), "expected the wide cell:\n{src}");
    assert!(src.contains("solo") && src.contains("wide"), "content lost:\n{src}");
}

/// End-to-end field handling: a complex `PAGE` field (the flattened
/// `w:fldChar` begin/separate/end run sequence) and a `w:fldSimple`
/// `HYPERLINK` field, both landing in the same paragraph, must survive the
/// full parse → lower → emit pipeline as live Typst constructs rather than
/// their stale cached text.
#[test]
fn fields_lower_to_idiomatic_typst_constructs() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p>
    <w:r><w:t xml:space="preserve">Page </w:t></w:r>
    <w:r><w:fldChar w:fldCharType="begin"/></w:r>
    <w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>
    <w:r><w:fldChar w:fldCharType="separate"/></w:r>
    <w:r><w:t>1</w:t></w:r>
    <w:r><w:fldChar w:fldCharType="end"/></w:r>
    <w:r><w:t xml:space="preserve"> — see </w:t></w:r>
    <w:fldSimple w:instr=" HYPERLINK &quot;https://example.com&quot; ">
      <w:r><w:t>our site</w:t></w:r>
    </w:fldSimple>
  </w:p>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(
        src.contains("#context counter(page).display()"),
        "PAGE field not lowered live:\n{src}"
    );
    assert!(
        src.contains("#link(\"https://example.com\")[our site]"),
        "HYPERLINK field not lowered to a link:\n{src}"
    );
    // The stale cached page number ("1") must not leak into the output as
    // literal text.
    assert!(!src.contains("Page 1 —"), "stale cached PAGE text leaked:\n{src}");
}

// --- Headers / footers -------------------------------------------------------

/// Builds a `.docx` with one `w:sectPr` `headerReference` per entry in
/// `header_parts` (`(w:type value, w:hdr body content)`), each pointing at
/// its own `word/headerN.xml` part — the part's *file name* must start with
/// `header` to be discovered as furniture (see `parse_furniture_parts`),
/// which is why it's generated here rather than reusing the `w:type` value.
fn docx_with_header_relationships(
    doc_body: &str,
    sect_pr_extra: &str,
    header_parts: &[(&str, &str)],
    settings_xml: Option<&str>,
) -> Vec<u8> {
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });

    let mut doc_rels = Rels::new();
    let mut refs = String::new();
    let mut parts = Vec::new();
    for (i, (kind, body_xml)) in header_parts.iter().enumerate() {
        let file_name = format!("header{}", i + 1);
        let rid = doc_rels.add(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
            &format!("{file_name}.xml"),
            RelMode::Internal,
        );
        refs.push_str(&format!(r#"<w:headerReference w:type="{kind}" r:id="{rid}"/>"#));
        parts.push((file_name, *body_xml));
    }
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    {doc_body}
    <w:sectPr>{refs}<w:pgSz w:w="12240" w:h="15840"/>{sect_pr_extra}</w:sectPr>
  </w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    for (file_name, body_xml) in &parts {
        let hdr = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{body_xml}</w:hdr>"#
        );
        package.add_xml(&format!("word/{file_name}.xml"), "application/xml", hdr);
    }
    if let Some(settings) = settings_xml {
        package.add_xml("word/settings.xml", "application/xml", settings.into());
    }
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.finish(&Rels::new()).unwrap()
}

const SIMPLE_BODY: &str = r#"<w:p><w:r><w:t>Body text.</w:t></w:r></w:p>"#;
const EVEN_AND_ODD_HEADERS_SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:evenAndOddHeaders/>
</w:settings>"#;

/// A default-only header reaches `#set page(header:` as a plain content
/// block — no `context` needed since there's no page-varying content.
#[test]
fn default_only_header_reaches_set_page_header() {
    let docx = docx_with_header_relationships(
        SIMPLE_BODY,
        "",
        &[("default", "<w:p><w:r><w:t>Company Report</w:t></w:r></w:p>")],
        None,
    );
    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#set page("), "missing page setup:\n{src}");
    assert!(src.contains("header: [Company Report]"), "missing header:\n{src}");
    assert!(!src.contains("context"), "shouldn't need a context block:\n{src}");
}

/// `w:titlePg` activates the `first` reference, which lowers to a
/// `context`-conditional branching on `p == 1`, falling back to `default`.
#[test]
fn title_pg_produces_the_first_page_conditional() {
    let docx = docx_with_header_relationships(
        SIMPLE_BODY,
        "<w:titlePg/>",
        &[
            ("default", "<w:p><w:r><w:t>Default header</w:t></w:r></w:p>"),
            ("first", "<w:p><w:r><w:t>Title page header</w:t></w:r></w:p>"),
        ],
        None,
    );
    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("header: context {"), "missing context block:\n{src}");
    assert!(
        src.contains("if p == 1 [Title page header]"),
        "missing first-page branch:\n{src}"
    );
    assert!(src.contains("else [Default header]"), "missing default fallback:\n{src}");
}

/// Without `w:evenAndOddHeaders` in `settings.xml`, an `even`-typed reference
/// exists but Word (and so this importer) ignores it — same shape as
/// `headerFooter.docx` in the POI corpus, which declares all three types with
/// neither `evenAndOddHeaders` nor `titlePg` set.
#[test]
fn even_reference_is_ignored_without_even_and_odd_headers() {
    let docx = docx_with_header_relationships(
        SIMPLE_BODY,
        "",
        &[
            ("default", "<w:p><w:r><w:t>Odd page header</w:t></w:r></w:p>"),
            ("even", "<w:p><w:r><w:t>Even page header</w:t></w:r></w:p>"),
        ],
        None,
    );
    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("header: [Odd page header]"), "missing plain header:\n{src}");
    assert!(!src.contains("context"), "even ref shouldn't be honored:\n{src}");
    assert!(!src.contains("Even page header"), "even content leaked into output:\n{src}");
}

/// The same document, but with `settings.xml` declaring
/// `w:evenAndOddHeaders` — now the `even` reference is honored as a
/// `context`-conditional branch.
#[test]
fn even_reference_is_honored_with_even_and_odd_headers() {
    let docx = docx_with_header_relationships(
        SIMPLE_BODY,
        "",
        &[
            ("default", "<w:p><w:r><w:t>Odd page header</w:t></w:r></w:p>"),
            ("even", "<w:p><w:r><w:t>Even page header</w:t></w:r></w:p>"),
        ],
        Some(EVEN_AND_ODD_HEADERS_SETTINGS),
    );
    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("header: context {"), "missing context block:\n{src}");
    assert!(
        src.contains("calc.even(p) [Even page header]"),
        "missing even-page branch:\n{src}"
    );
    assert!(src.contains("else [Odd page header]"), "missing default fallback:\n{src}");
}

/// Word's own placeholder header shape — a single empty paragraph, no
/// visible text (see `headerFooter.docx`'s "even"/"first" parts in the POI
/// corpus) — must not surface as a `header: []` argument at all.
#[test]
fn empty_placeholder_header_produces_no_header_argument() {
    let docx = docx_with_header_relationships(
        SIMPLE_BODY,
        "",
        &[("default", r#"<w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr></w:p>"#)],
        None,
    );
    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(!src.contains("header:"), "empty placeholder header should be dropped:\n{src}");
}

/// The collision case: the document's own `rId1` means one thing
/// (`styles.xml`), while the header part's *own* `rId1` — numbered
/// independently, per `word/_rels/header1.xml.rels` — means something else
/// entirely (an embedded image). Reproduces `headerPic.docx` in the POI
/// corpus. Only a per-part-scoped relationship merge resolves the header's
/// image against the header's own target rather than silently reusing
/// whatever the document's `rId1` happens to mean.
#[test]
fn header_image_resolves_through_the_headers_own_relationships() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:t>Body text.</w:t></w:r></w:p>
    <w:sectPr>
      <w:headerReference w:type="default" r:id="rId2"/>
      <w:pgSz w:w="12240" w:h="15840"/>
    </w:sectPr>
  </w:body>
</w:document>"#;

    const HEADER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
       xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:p>
    <w:r>
      <w:drawing>
        <wp:inline>
          <wp:extent cx="914400" cy="457200"/>
          <wp:docPr id="1" name="Logo"/>
          <a:graphic><a:graphicData>
            <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
          </a:graphicData></a:graphic>
        </wp:inline>
      </w:drawing>
    </w:r>
  </w:p>
</w:hdr>"#;

    const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

    const HEADER_IMAGE_BYTES: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });

    let mut doc_rels = Rels::new();
    let styles_rid = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
        "styles.xml",
        RelMode::Internal,
    );
    let header_rid = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        "header1.xml",
        RelMode::Internal,
    );
    // The document's own `rId1` means `styles.xml` — a different target than
    // whatever the header's independently-numbered `rId1` (below) means.
    assert_eq!(styles_rid, "rId1");
    assert_eq!(header_rid, "rId2");

    let mut header_rels = Rels::new();
    let image_rid = header_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/headerimg.png",
        RelMode::Internal,
    );
    assert_eq!(image_rid, "rId1");

    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
    package.add_xml("word/header1.xml", "application/xml", HEADER_XML.into());
    package.add_media(
        "word/media/headerimg.png",
        "png",
        "image/png",
        HEADER_IMAGE_BYTES.to_vec(),
    );
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.add_relationships("word/header1.xml", &header_rels).unwrap();
    let docx = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        result.source.contains("header: [#image("),
        "missing header image:\n{}",
        result.source
    );
    assert!(
        result.source.contains("headerimg.png"),
        "wrong (or missing) asset name:\n{}",
        result.source
    );

    let (_, bytes) = result
        .assets
        .iter()
        .find(|(path, _)| path.to_str().unwrap().contains("headerimg.png"))
        .expect("headerimg.png should have been extracted as an asset");
    assert_eq!(bytes, HEADER_IMAGE_BYTES);
}

// --- Structured document tags (content controls) -----------------------------

/// A `w:sdt` (content control) is a *wrapper*, not content: Word puts them
/// around cover-page placeholders, date pickers, and — as in the POI corpus's
/// `Bug60341.docx` — an entire footer. Skipping the element drops everything
/// inside it silently, so the parser splices `w:sdtContent` in where the tag
/// stood. This must hold at block level, inside table cells, and inline within
/// a paragraph, since Word wraps at all three.
#[test]
fn structured_document_tags_are_transparent() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:sdt><w:sdtPr/><w:sdtContent>
    <w:p><w:r><w:t>BlockLevelText</w:t></w:r></w:p>
    <w:sdt><w:sdtContent>
      <w:p><w:r><w:t>NestedBlockText</w:t></w:r></w:p>
    </w:sdtContent></w:sdt>
  </w:sdtContent></w:sdt>
  <w:p>
    <w:r><w:t xml:space="preserve">before </w:t></w:r>
    <w:sdt><w:sdtContent><w:r><w:t>InlineText</w:t></w:r></w:sdtContent></w:sdt>
    <w:r><w:t xml:space="preserve"> after</w:t></w:r>
  </w:p>
  <w:tbl>
    <w:tblGrid><w:gridCol w:w="100"/></w:tblGrid>
    <w:tr><w:tc>
      <w:sdt><w:sdtContent><w:p><w:r><w:t>CellText</w:t></w:r></w:p></w:sdtContent></w:sdt>
    </w:tc></w:tr>
  </w:tbl>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    for expected in ["BlockLevelText", "NestedBlockText", "CellText"] {
        assert!(src.contains(expected), "lost {expected} inside a w:sdt:\n{src}");
    }
    // The inline case must keep its neighbours' word spacing rather than
    // gluing the spliced run onto them.
    assert!(src.contains("before InlineText after"), "inline w:sdt mishandled:\n{src}");
}

//! End-to-end integration test for the DOCX → Typst importer: builds a small
//! but realistic Word document in memory (via the OPC writer the exporter also
//! uses, so the reader gets a genuinely well-formed package) and asserts the
//! emitted Typst source is idiomatic — headings as `=`, emphasis as
//! `*`/`_`, and lists as `-`/`+`.

use typst_docx_import::{import_docx, import_docx_with, ChartStyle, ImportOptions, Tier};
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

// --- Footnotes / endnotes -----------------------------------------------------

/// Builds a `.docx` with `doc_body` as the body's raw inner XML, plus
/// optional `word/footnotes.xml`/`word/endnotes.xml` parts — mirroring
/// `docx_with_header_relationships`'s approach of assembling exactly the
/// parts a given test needs.
fn build_docx_with_notes(
    doc_body: &str,
    footnotes_xml: Option<&str>,
    endnotes_xml: Option<&str>,
) -> Vec<u8> {
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{doc_body}</w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    if let Some(xml) = footnotes_xml {
        package.add_xml("word/footnotes.xml", "application/xml", xml.to_string());
    }
    if let Some(xml) = endnotes_xml {
        package.add_xml("word/endnotes.xml", "application/xml", xml.to_string());
    }
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    package.finish(&Rels::new()).unwrap()
}

const NOTE_BODY: &str = r#"<w:p><w:r><w:t>{TEXT}</w:t></w:r></w:p>"#;

fn note_xml(root: &str, elem: &str, id: &str, text: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:{elem} w:id="{id}">{body}</w:{elem}>
</w:{root}>"#,
        body = NOTE_BODY.replace("{TEXT}", text)
    )
}

/// A `w:footnoteReference` must both keep its marker's place in the running
/// text and pull in the note's actual content from `word/footnotes.xml`,
/// landing as a live `#footnote[..]` rather than vanishing (today's bug —
/// neither the marker nor the text survives at all).
#[test]
fn footnote_reference_lowers_to_a_footnote_call_containing_the_note_text() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">See this. </w:t></w:r>
      <w:r><w:footnoteReference w:id="1"/></w:r>
    </w:p>"#;
    let footnotes_xml = note_xml("footnotes", "footnote", "1", "snoska");
    let docx = build_docx_with_notes(doc_body, Some(&footnotes_xml), None);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#footnote[snoska]"), "note text not inlined as a footnote:\n{src}");
    assert!(src.contains("See this."), "surrounding text lost:\n{src}");
}

/// Word's own rule-line boilerplate (`w:type="separator"` /
/// `"continuationSeparator"`) must never surface as if it were authored
/// content, no matter what it contains.
#[test]
fn boilerplate_separator_notes_never_appear_in_output() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Body text. </w:t></w:r>
      <w:r><w:footnoteReference w:id="1"/></w:r>
    </w:p>"#;
    let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:t>SEPARATOR_MARKER</w:t></w:r></w:p></w:footnote>
  <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:t>CONTINUATION_MARKER</w:t></w:r></w:p></w:footnote>
  <w:footnote w:id="1"><w:p><w:r><w:t>snoska</w:t></w:r></w:p></w:footnote>
</w:footnotes>"#;
    let docx = build_docx_with_notes(doc_body, Some(footnotes_xml), None);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(!src.contains("SEPARATOR_MARKER"), "separator boilerplate leaked:\n{src}");
    assert!(
        !src.contains("CONTINUATION_MARKER"),
        "continuation-separator boilerplate leaked:\n{src}"
    );
    assert!(src.contains("snoska"), "the real note was lost along with the boilerplate:\n{src}");
}

/// A reference whose id has nothing to resolve against (missing part, or an
/// id absent from it) must degrade gracefully: the import still succeeds,
/// the marker is simply dropped, and the drop is recorded rather than
/// silently swallowed.
#[test]
fn dangling_footnote_reference_degrades_gracefully() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Body text. </w:t></w:r>
      <w:r><w:footnoteReference w:id="99"/></w:r>
    </w:p>"#;
    // A footnotes.xml part exists, but has no note with id 99.
    let footnotes_xml = note_xml("footnotes", "footnote", "1", "unrelated note");
    let docx = build_docx_with_notes(doc_body, Some(&footnotes_xml), None);

    let result = import_docx(&docx).expect("import should succeed despite the dangling ref");
    assert!(
        !result.source.contains("#footnote["),
        "a dangling ref must not fabricate a footnote:\n{}",
        result.source
    );
    assert!(result.source.contains("Body text."), "surrounding text lost:\n{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "footnote"),
        "expected a report note about the dangling footnote:\n{:?}",
        result.report.notes
    );
}

/// A footnote that references itself (directly, or — as covered at the unit
/// level in `mappers::note` — through a chain) must not hang the importer.
/// This is the one test in this file where *terminating at all* is the
/// assertion: if the cycle guard were broken, this test would never return.
#[test]
fn self_referential_footnote_does_not_hang_the_importer() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Body text. </w:t></w:r>
      <w:r><w:footnoteReference w:id="1"/></w:r>
    </w:p>"#;
    // Note 1's own body references note 1 again.
    let footnotes_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:footnote w:id="1">
    <w:p><w:r><w:t xml:space="preserve">self-ref: </w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>
  </w:footnote>
</w:footnotes>"#;
    let docx = build_docx_with_notes(doc_body, Some(footnotes_xml), None);

    // Reaching this line at all demonstrates termination.
    let result = import_docx(&docx).expect("import should succeed, not hang or crash");
    assert!(result.source.contains("Body text."), "surrounding text lost:\n{}", result.source);
    assert!(
        result.source.contains("self-ref:"),
        "the note's own (non-cyclic) text was lost:\n{}",
        result.source
    );
    assert!(
        result.report.notes.iter().any(|n| n.what == "footnote"),
        "expected a report note about the note cycle:\n{:?}",
        result.report.notes
    );
}

/// Typst has no end-of-document note store, so an endnote is lowered as a
/// footnote at its reference site — a real, reported approximation.
#[test]
fn endnote_lowers_to_a_footnote_with_the_approximation_recorded() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Body text. </w:t></w:r>
      <w:r><w:endnoteReference w:id="1"/></w:r>
    </w:p>"#;
    let endnotes_xml = note_xml("endnotes", "endnote", "1", "end note text");
    let docx = build_docx_with_notes(doc_body, None, Some(&endnotes_xml));

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        result.source.contains("#footnote[end note text]"),
        "endnote not lowered to a footnote call:\n{}",
        result.source
    );
    let is_endnote_approximation = |n: &typst_docx_import::report::Note| {
        n.what == "endnote" && n.severity == typst_docx_import::report::Severity::Approximate
    };
    assert!(
        result.report.notes.iter().any(is_endnote_approximation),
        "expected the endnote-as-footnote approximation to be recorded:\n{:?}",
        result.report.notes
    );
}

// --- Text boxes / shapes (mc:AlternateContent, wps:txbx, v:textbox) ---------

/// Builds a `.docx` with `doc_body` as the body's raw inner XML, declaring
/// every namespace prefix a text-box fixture needs (`mc`, `wps`, `v`) on the
/// document root — mirrors `build_docx_with_notes`'s "just the parts this
/// test needs" approach.
fn docx_with_body(doc_body: &str) -> Vec<u8> {
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"
            xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"
            xmlns:v="urn:schemas-microsoft-com:vml">
<w:body>{doc_body}</w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    package.finish(&Rels::new()).unwrap()
}

/// The hazard this task exists to fix: Word writes a modern text box
/// *twice* — a DrawingML `wps:txbx` inside `mc:Choice`, a VML `v:textbox`
/// inside `mc:Fallback` — holding the *same* content. Only the `mc:Choice`
/// branch must survive the full parse → lower → emit pipeline; the
/// `mc:Fallback` text must never appear, and the `mc:Choice` text must never
/// be duplicated. Reproduces the shape `shapes-with-text.docx` in the POI
/// corpus uses for every one of its text boxes.
#[test]
fn mc_alternate_content_text_box_is_not_duplicated() {
    let doc_body = r#"<w:p><w:r><mc:AlternateContent>
      <mc:Choice Requires="wps">
        <w:drawing><wps:wsp><wps:txbx><w:txbxContent>
          <w:p><w:r><w:t>CHOICE TEXT</w:t></w:r></w:p>
        </w:txbxContent></wps:txbx></wps:wsp></w:drawing>
      </mc:Choice>
      <mc:Fallback>
        <w:pict><v:shape><v:textbox><w:txbxContent>
          <w:p><w:r><w:t>FALLBACK TEXT</w:t></w:r></w:p>
        </w:txbxContent></v:textbox></v:shape></w:pict>
      </mc:Fallback>
    </mc:AlternateContent></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert_eq!(
        src.matches("CHOICE TEXT").count(),
        1,
        "the choice text must appear exactly once:\n{src}"
    );
    assert!(!src.contains("FALLBACK TEXT"), "the fallback text must not appear at all:\n{src}");
    assert!(src.contains("#box["), "expected a text box:\n{src}");
}

/// The VML-only spelling (`w:pict`/`v:textbox`), with no `mc:Choice` in
/// sight — an older document, or one that never went through modern Word's
/// MCE dance.
#[test]
fn vml_only_text_box_is_imported() {
    let doc_body = r#"<w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent>
      <w:p><w:r><w:t>VML box text</w:t></w:r></w:p>
    </w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#box[VML box text]"), "missing VML text box:\n{src}");
}

/// A bare `mc:AlternateContent` with only an `mc:Fallback` — no `mc:Choice`
/// at all — must still use the fallback rather than dropping the content,
/// per [`splice_node`]'s doc comment in `wml::parse`.
#[test]
fn alternate_content_with_only_a_fallback_uses_it() {
    let doc_body = r#"<w:p><mc:AlternateContent>
      <mc:Fallback><w:r><w:t>FALLBACK ONLY</w:t></w:r></mc:Fallback>
    </mc:AlternateContent></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("FALLBACK ONLY"), "missing fallback text:\n{src}");
}

/// A text box containing a drawing that is itself a text box, nested a few
/// levels deep, must import and compile — not just terminate (the depth cap
/// itself is covered at the unit level in `wml::parse`), but survive the
/// full pipeline through to idiomatic Typst source with both levels' text
/// intact.
#[test]
fn nested_text_boxes_survive_the_full_pipeline() {
    let doc_body = r#"<w:p><w:r><w:drawing><wps:wsp><wps:txbx><w:txbxContent>
      <w:p><w:r><w:t>outer</w:t></w:r>
        <w:r><w:drawing><wps:wsp><wps:txbx><w:txbxContent>
          <w:p><w:r><w:t>inner</w:t></w:r></w:p>
        </w:txbxContent></wps:txbx></wps:wsp></w:drawing></w:r>
      </w:p>
    </w:txbxContent></wps:txbx></wps:wsp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("outer"), "missing outer text box content:\n{src}");
    assert!(src.contains("inner"), "missing nested text box content:\n{src}");
    assert_eq!(src.matches("#box[").count(), 2, "expected two nested boxes:\n{src}");
}

// --- Charts (c:chart / ChartEx) -----------------------------------------------

/// Builds a `.docx` with one paragraph containing a `w:drawing` chart
/// reference (`r:id="rId1"`) and — unless `chart_target` is `None` — a
/// `word/charts/chart1.xml` part whose content is `chart_inner` wrapped in a
/// `c:chartSpace` root. `chart_target`, when given, overrides what the
/// relationship actually points at (letting a test aim it at a part that
/// exists but isn't a chart, without needing an unresolvable `r:id` — the
/// OPC writer validates that an internal relationship's target actually
/// exists in the package, so a genuinely dangling `r:id` has to be exercised
/// at the unit level instead — see `mappers::chart`'s own tests).
fn docx_with_chart(chart_inner: &str, chart_target: Option<&str>) -> Vec<u8> {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:drawing>
      <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
        <c:chart r:id="rId1"/>
      </a:graphicData></a:graphic>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#;
    const COLORS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<cs:colorStyle xmlns:cs="http://schemas.microsoft.com/office/drawing/2012/chartStyle"/>"#;

    let chart_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
              xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
              xmlns:cx="http://schemas.microsoft.com/office/drawing/2014/chartex">{chart_inner}</c:chartSpace>"#
    );

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    let target = chart_target.unwrap_or("charts/chart1.xml");
    let chart_rid = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
        target,
        RelMode::Internal,
    );
    assert_eq!(chart_rid, "rId1"); // matches the fixture's hardcoded r:id

    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/charts/chart1.xml", "application/xml", chart_xml);
    if chart_target.is_some() {
        // The redirected target must still exist for the writer to accept
        // the relationship (see this function's doc comment).
        package.add_xml("word/charts/colors1.xml", "application/xml", COLORS_XML.into());
    }
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.finish(&Rels::new()).unwrap()
}

/// A classic chart with a title split across runs, a category axis, and one
/// series must import as `#figure(table(..), caption: [..])` — the table
/// carrying the exact numbers and labels the chart cached, not the plot
/// itself. Reproduces the shape `chart1.xml` in the POI corpus's
/// `chartex.docx` actually uses (see `wml::parse`'s matching unit test for
/// the same fixture at the parsing level).
#[test]
fn chart_lowers_to_a_captioned_data_table() {
    let chart_inner = r#"<c:chart>
      <c:title><c:tx><c:rich>
        <a:p><a:r><a:t>my chart</a:t></a:r><a:r><a:t> looks nice</a:t></a:r></a:p>
      </c:rich></c:tx></c:title>
      <c:plotArea><c:barChart><c:ser>
        <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Series 1</c:v></c:pt></c:strCache></c:strRef></c:tx>
        <c:cat><c:strRef><c:strCache>
          <c:pt idx="0"><c:v>Category 1</c:v></c:pt>
          <c:pt idx="1"><c:v>Category 2</c:v></c:pt>
        </c:strCache></c:strRef></c:cat>
        <c:val><c:numRef><c:numCache>
          <c:pt idx="0"><c:v>4.3</c:v></c:pt>
          <c:pt idx="1"><c:v>2.5</c:v></c:pt>
        </c:numCache></c:numRef></c:val>
      </c:ser></c:barChart></c:plotArea>
    </c:chart>"#;
    let docx = docx_with_chart(chart_inner, None);

    let result = import_docx(&docx).expect("import should succeed");
    let src = &result.source;
    assert!(src.contains("#figure(table("), "expected a captioned table:\n{src}");
    assert!(src.contains("caption: [my chart looks nice]"), "missing/wrong caption:\n{src}");
    assert!(src.contains("table.header([], [Series 1])"), "missing header row:\n{src}");
    assert!(src.contains("[Category 1]") && src.contains("[4.3]"), "missing data:\n{src}");
    assert!(src.contains("[Category 2]") && src.contains("[2.5]"), "missing data:\n{src}");

    let is_chart_approximation = |n: &typst_docx_import::report::Note| {
        n.what == "chart" && n.severity == typst_docx_import::report::Severity::Approximate
    };
    assert!(
        result.report.notes.iter().any(is_chart_approximation),
        "expected the chart-as-table approximation to be recorded:\n{:?}",
        result.report.notes
    );
}

/// A chart reference that resolves through the relationship but not to a
/// recognized chart part (here: redirected at the chart's own `colors1.xml`
/// sibling — see `docx_with_chart`'s doc comment for why this, rather than
/// an actually-unresolvable `r:id`, is what an OPC-valid fixture can
/// exercise) must degrade gracefully: the import still succeeds, no table is
/// fabricated, and the drop is recorded rather than silently swallowed.
#[test]
fn dangling_chart_reference_degrades_gracefully() {
    let docx = docx_with_chart("", Some("charts/colors1.xml"));

    let result = import_docx(&docx).expect("import should succeed despite the dangling ref");
    assert!(
        !result.source.contains("table("),
        "a dangling chart ref must not fabricate a table:\n{}",
        result.source
    );
    assert!(
        result.report.notes.iter().any(|n| n.what == "chart"),
        "expected a report note about the dangling chart reference:\n{:?}",
        result.report.notes
    );
}

/// A chart part that parses (a well-formed `c:chartSpace`) but carries no
/// title, no series, and no categories at all must be skipped entirely —
/// no empty `#table(..)` left behind — since there's nothing about it to
/// lose by dropping it.
#[test]
fn chart_with_no_data_at_all_produces_no_output() {
    let docx = docx_with_chart("", None);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        !result.source.contains("table("),
        "an empty chart must not produce an empty table:\n{}",
        result.source
    );
    assert!(
        !result.report.notes.iter().any(|n| n.what == "chart"),
        "an empty chart has nothing to report: {:?}",
        result.report.notes
    );
}

// --- `ChartStyle::Plot` ------------------------------------------------------

/// Like `docx_with_chart`, but with two chart references (`rId1`/`rId2` →
/// `charts/chart1.xml`/`charts/chart2.xml`) in separate paragraphs — for the
/// test asserting the `lilaq` import is emitted only once however many
/// charts in the document actually use it.
fn docx_with_two_charts(chart1_inner: &str, chart2_inner: &str) -> Vec<u8> {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <w:body>
    <w:p><w:r><w:drawing>
      <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
        <c:chart r:id="rId1"/>
      </a:graphicData></a:graphic>
    </w:drawing></w:r></w:p>
    <w:p><w:r><w:drawing>
      <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
        <c:chart r:id="rId2"/>
      </a:graphicData></a:graphic>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#;

    let wrap = |inner: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
              xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">{inner}</c:chartSpace>"#
        )
    };

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    let rid1 = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
        "charts/chart1.xml",
        RelMode::Internal,
    );
    let rid2 = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
        "charts/chart2.xml",
        RelMode::Internal,
    );
    assert_eq!(rid1, "rId1");
    assert_eq!(rid2, "rId2");

    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/charts/chart1.xml", "application/xml", wrap(chart1_inner));
    package.add_xml("word/charts/chart2.xml", "application/xml", wrap(chart2_inner));
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.finish(&Rels::new()).unwrap()
}

/// The task's own worked example: three series, each 3 points, no categories
/// — deliberately the shape most likely to get the grouped-bar-offset math
/// wrong (see the doc comment on the test that uses this).
fn three_series_bar_chart_xml() -> &'static str {
    r#"<c:chart><c:plotArea><c:barChart>
      <c:ser>
        <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>S1</c:v></c:pt></c:strCache></c:strRef></c:tx>
        <c:val><c:numRef><c:numCache>
          <c:pt idx="0"><c:v>4.3</c:v></c:pt>
          <c:pt idx="1"><c:v>2.5</c:v></c:pt>
          <c:pt idx="2"><c:v>3.5</c:v></c:pt>
        </c:numCache></c:numRef></c:val>
      </c:ser>
      <c:ser>
        <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>S2</c:v></c:pt></c:strCache></c:strRef></c:tx>
        <c:val><c:numRef><c:numCache>
          <c:pt idx="0"><c:v>2.4</c:v></c:pt>
          <c:pt idx="1"><c:v>4.4</c:v></c:pt>
          <c:pt idx="2"><c:v>1.8</c:v></c:pt>
        </c:numCache></c:numRef></c:val>
      </c:ser>
      <c:ser>
        <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>S3</c:v></c:pt></c:strCache></c:strRef></c:tx>
        <c:val><c:numRef><c:numCache>
          <c:pt idx="0"><c:v>2.0</c:v></c:pt>
          <c:pt idx="1"><c:v>2.0</c:v></c:pt>
          <c:pt idx="2"><c:v>3.0</c:v></c:pt>
        </c:numCache></c:numRef></c:val>
      </c:ser>
    </c:barChart></c:plotArea></c:chart>"#
}

fn plot_options() -> ImportOptions {
    ImportOptions { charts: typst_docx_import::ChartStyle::Plot, ..Default::default() }
}

/// The default options (`ChartStyle::Table`) must produce exactly the same
/// output as before this feature existed, for a chart that — under `Plot` —
/// would actually be plottable. In particular: no `#import` of `lilaq`
/// anywhere, since the emitted source stays self-contained by default.
#[test]
fn default_chart_style_stays_a_table_and_never_imports_lilaq() {
    let docx = docx_with_chart(three_series_bar_chart_xml(), None);
    let result = import_docx(&docx).expect("import should succeed");
    assert!(result.source.contains("#table("), "{}", result.source);
    assert!(!result.source.contains("lilaq"), "{}", result.source);
    assert!(!result.source.contains("#import"), "{}", result.source);
}

/// A bar chart under `ChartStyle::Plot` draws grouped bars: three
/// `lq.bar(..)` calls with the width/offset formula from the task (`width =
/// 1/(N+1)`, offset `(i - (N-1)/2) * width`), and the `lilaq` import appears
/// exactly once even with *two* charts in the document.
#[test]
fn bar_chart_under_chart_style_plot_produces_grouped_lq_bar_calls_and_one_import() {
    let docx = docx_with_two_charts(three_series_bar_chart_xml(), three_series_bar_chart_xml());
    let result =
        import_docx_with(&docx, &plot_options()).expect("import should succeed");
    let src = &result.source;

    assert!(
        src.contains("lq.bar((-0.25, 0.75, 1.75), (4.3, 2.5, 3.5), width: 0.25, label: [S1])"),
        "{src}"
    );
    assert!(
        src.contains("lq.bar((0, 1, 2), (2.4, 4.4, 1.8), width: 0.25, label: [S2])"),
        "{src}"
    );
    assert!(
        src.contains("lq.bar((0.25, 1.25, 2.25), (2, 2, 3), width: 0.25, label: [S3])"),
        "{src}"
    );
    assert!(!src.contains("table("), "expected no fallback table:\n{src}");
    assert_eq!(
        src.matches("#import \"@preview/lilaq:0.6.0\" as lq").count(),
        1,
        "expected exactly one lilaq import for two plottable charts:\n{src}"
    );
}

/// A line chart under `ChartStyle::Plot` draws with `lq.plot(..)`.
#[test]
fn line_chart_under_chart_style_plot_produces_lq_plot() {
    let chart_inner = r#"<c:chart><c:plotArea><c:lineChart><c:ser>
      <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Temp</c:v></c:pt></c:strCache></c:strRef></c:tx>
      <c:val><c:numRef><c:numCache>
        <c:pt idx="0"><c:v>1</c:v></c:pt>
        <c:pt idx="1"><c:v>2</c:v></c:pt>
      </c:numCache></c:numRef></c:val>
    </c:ser></c:lineChart></c:plotArea></c:chart>"#;
    let docx = docx_with_chart(chart_inner, None);
    let result =
        import_docx_with(&docx, &plot_options()).expect("import should succeed");
    assert!(
        result.source.contains("lq.plot((0, 1), (1, 2), label: [Temp])"),
        "{}",
        result.source
    );
}

/// A pie chart has no `lilaq` counterpart, so even under `ChartStyle::Plot`
/// it still falls back to the data table, with a note explaining why.
#[test]
fn pie_chart_under_chart_style_plot_still_falls_back_to_table_with_a_note() {
    let chart_inner = r#"<c:chart><c:plotArea><c:pieChart><c:ser>
      <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Share</c:v></c:pt></c:strCache></c:strRef></c:tx>
      <c:val><c:numRef><c:numCache>
        <c:pt idx="0"><c:v>40</c:v></c:pt>
        <c:pt idx="1"><c:v>60</c:v></c:pt>
      </c:numCache></c:numRef></c:val>
    </c:ser></c:pieChart></c:plotArea></c:chart>"#;
    let docx = docx_with_chart(chart_inner, None);
    let result =
        import_docx_with(&docx, &plot_options()).expect("import should succeed");
    assert!(result.source.contains("#table("), "{}", result.source);
    assert!(!result.source.contains("lilaq"), "{}", result.source);
    assert!(
        result
            .report
            .notes
            .iter()
            .any(|n| n.what == "chart" && n.detail.contains("no plotting counterpart")),
        "expected a reason naming the unplottable kind: {:?}",
        result.report.notes
    );
}

/// A series whose cached values aren't actually numbers can't be plotted;
/// falls back to the table with a note under `ChartStyle::Plot`.
#[test]
fn non_numeric_chart_values_fall_back_to_table_with_a_note_under_plot_mode() {
    let chart_inner = r#"<c:chart><c:plotArea><c:barChart><c:ser>
      <c:val><c:numRef><c:numCache>
        <c:pt idx="0"><c:v>N/A</c:v></c:pt>
        <c:pt idx="1"><c:v>2.5</c:v></c:pt>
      </c:numCache></c:numRef></c:val>
    </c:ser></c:barChart></c:plotArea></c:chart>"#;
    let docx = docx_with_chart(chart_inner, None);
    let result =
        import_docx_with(&docx, &plot_options()).expect("import should succeed");
    assert!(result.source.contains("#table("), "{}", result.source);
    assert!(!result.source.contains("lilaq"), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "chart" && n.detail.contains("not numeric")),
        "expected a reason naming the non-numeric values: {:?}",
        result.report.notes
    );
}

/// When *every* chart in a document falls back to the table under
/// `ChartStyle::Plot` (here: a lone pie chart), the `lilaq` import must not
/// appear at all — nothing in the emitted source actually needs it.
#[test]
fn no_lilaq_import_when_every_chart_falls_back_under_plot_mode() {
    let chart_inner = r#"<c:chart><c:plotArea><c:pieChart><c:ser>
      <c:val><c:numRef><c:numCache>
        <c:pt idx="0"><c:v>40</c:v></c:pt>
        <c:pt idx="1"><c:v>60</c:v></c:pt>
      </c:numCache></c:numRef></c:val>
    </c:ser></c:pieChart></c:plotArea></c:chart>"#;
    let docx = docx_with_chart(chart_inner, None);
    let result =
        import_docx_with(&docx, &plot_options()).expect("import should succeed");
    assert!(!result.source.contains("#import"), "{}", result.source);
    assert!(!result.source.contains("lilaq"), "{}", result.source);
}

/// MCE's contract is that a consumer takes an `mc:Choice` only if it supports
/// that choice's requirement. We handle `wps` text boxes better than the
/// legacy VML fallback beside them, so that Choice wins — but we cannot draw
/// `cx` extended charts (sunburst, box-and-whisker), and Word puts a picture
/// of the chart it already rendered in the Fallback. Taking that picture beats
/// flattening a hierarchical chart into a table.
#[test]
fn mce_choice_is_honored_only_for_requirements_we_render_better() {
    let doc = |requires: &str| {
        format!(
            r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
  <w:body><w:p><w:r>
    <mc:AlternateContent>
      <mc:Choice Requires="{requires}">
        <w:pict><v:textbox xmlns:v="urn:schemas-microsoft-com:vml"><w:txbxContent>
          <w:p><w:r><w:t>ChoiceBranch</w:t></w:r></w:p>
        </w:txbxContent></v:textbox></w:pict>
      </mc:Choice>
      <mc:Fallback>
        <w:pict><v:textbox xmlns:v="urn:schemas-microsoft-com:vml"><w:txbxContent>
          <w:p><w:r><w:t>FallbackBranch</w:t></w:r></w:p>
        </w:txbxContent></v:textbox></w:pict>
      </mc:Fallback>
    </mc:AlternateContent>
  </w:r></w:p></w:body></w:document>"#
        )
    };

    let import = |requires: &str| {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml("word/document.xml", "application/xml", doc(requires));
        package.add_relationships("word/document.xml", &Rels::new()).unwrap();
        let bytes = package.finish(&Rels::new()).unwrap();
        import_docx(&bytes).expect("import should succeed").source
    };

    // A requirement we render better than the fallback: take the Choice.
    let src = import("wps");
    assert!(src.contains("ChoiceBranch"), "wps Choice not taken:\n{src}");
    assert!(!src.contains("FallbackBranch"), "wps fallback leaked (duplicate):\n{src}");

    // Extended charts: we cannot draw them, so the fallback wins.
    let src = import("cx");
    assert!(src.contains("FallbackBranch"), "cx fallback not taken:\n{src}");
    assert!(!src.contains("ChoiceBranch"), "cx Choice should have been skipped:\n{src}");
}

/// Furigana (`w:ruby`) is real document text, not decoration: the reading and
/// the base together *are* the sentence, so dropping the element loses both.
/// Typst has no ruby primitive, so the emitter defines a helper — but only for
/// documents that actually use one.
#[test]
fn ruby_annotations_survive_with_a_generated_helper() {
    let ruby_doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:r>
    <w:ruby>
      <w:rubyPr><w:lid w:val="ja-JP"/></w:rubyPr>
      <w:rt><w:r><w:t>とうきょう</w:t></w:r></w:rt>
      <w:rubyBase><w:r><w:t>東京</w:t></w:r></w:rubyBase>
    </w:ruby>
  </w:r></w:p>
</w:body></w:document>"#;

    let build = |body: &str| {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml("word/document.xml", "application/xml", body.into());
        package.add_relationships("word/document.xml", &Rels::new()).unwrap();
        let bytes = package.finish(&Rels::new()).unwrap();
        import_docx(&bytes).expect("import should succeed").source
    };

    let src = build(ruby_doc);
    assert!(src.contains("#let ruby("), "ruby helper not defined:\n{src}");
    assert!(src.contains("#ruby[東京][とうきょう]"), "ruby call wrong:\n{src}");
    // The helper must precede its first use, or the document won't compile.
    assert!(
        src.find("#let ruby(").unwrap() < src.find("#ruby[").unwrap(),
        "helper defined after use:\n{src}"
    );

    // A document with no ruby must not carry the helper.
    let plain = build(
        r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:r><w:t>plain</w:t></w:r></w:p>
</w:body></w:document>"#,
    );
    assert!(!plain.contains("#let ruby("), "helper emitted for a doc without ruby:\n{plain}");
}

/// Word anchors a picture or chart *on* a paragraph, and that paragraph can
/// simultaneously be a list item — a bulleted step illustrated with a
/// screenshot is ordinary. Classifying the paragraph as "a list item" must not
/// discard what is anchored on it; that is silent content loss, and it is
/// exactly what `chartex.docx` hit (its first chart sat on a numbered
/// paragraph and vanished).
#[test]
fn a_figure_anchored_on_a_list_item_is_not_dropped() {
    const NUMBERING: &str = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    let image_rid = rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/step.png",
        RelMode::Internal,
    );

    let doc = format!(
        r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr></w:pPr>
      <w:r><w:t>First step</w:t></w:r></w:p>
    <w:p><w:pPr><w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr></w:pPr>
      <w:r><w:drawing><a:blip r:embed="{image_rid}"/></w:drawing></w:r></w:p>
  </w:body>
</w:document>"#
    );

    package.add_xml("word/document.xml", "application/xml", doc);
    package.add_xml("word/numbering.xml", "application/xml", NUMBERING.into());
    package.add_media("word/media/step.png", "png", "image/png", vec![0xDE, 0xAD]);
    package.add_relationships("word/document.xml", &rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("- First step"), "list item lost:\n{src}");
    assert!(src.contains("#image("), "the anchored image was dropped:\n{src}");
}

/// `w:smartTag` (Word's old auto-recognition markup for place names, dates and
/// the like) wraps runs and nests several deep around a single one. Like every
/// other annotation-only wrapper it must be transparent: its children are the
/// document's actual words, and ignoring the element loses them.
#[test]
fn smart_tags_and_bidi_overrides_are_transparent() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p>
    <w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="PlaceName">
      <w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="place">
        <w:r><w:t>Carnegie</w:t></w:r>
      </w:smartTag>
      <w:r><w:t xml:space="preserve"> Mellon</w:t></w:r>
    </w:smartTag>
    <w:r><w:t xml:space="preserve"> University</w:t></w:r>
  </w:p>
  <w:p><w:bdo w:val="rtl"><w:r><w:t>Overridden</w:t></w:r></w:bdo></w:p>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(
        src.contains("Carnegie Mellon University"),
        "nested smart tags lost text:\n{src}"
    );
    assert!(src.contains("Overridden"), "w:bdo lost its text:\n{src}");
}

/// Tracked changes are accepted: `w:ins` wraps runs that *are* part of the
/// final text, so the wrapper must be transparent, while `w:del` holds text
/// the author removed and must not come back. `delins.docx` in the POI corpus
/// has 43 insertions that were being dropped wholesale.
#[test]
fn tracked_insertions_are_kept_and_deletions_dropped() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p>
    <w:r><w:t xml:space="preserve">Kept </w:t></w:r>
    <w:ins w:id="1" w:author="a"><w:r><w:t xml:space="preserve">InsertedText </w:t></w:r></w:ins>
    <w:del w:id="2" w:author="a"><w:r><w:delText>DeletedText </w:delText></w:r></w:del>
    <w:moveTo w:id="3"><w:r><w:t>MovedIn</w:t></w:r></w:moveTo>
    <w:moveFrom w:id="4"><w:r><w:delText>MovedOut</w:delText></w:r></w:moveFrom>
  </w:p>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("Kept"), "plain run lost:\n{src}");
    assert!(src.contains("InsertedText"), "w:ins content was dropped:\n{src}");
    assert!(src.contains("MovedIn"), "w:moveTo content was dropped:\n{src}");
    assert!(!src.contains("DeletedText"), "w:del content leaked back in:\n{src}");
    assert!(!src.contains("MovedOut"), "w:moveFrom content leaked back in:\n{src}");
}

/// A plotted chart must use Word's own extent and legend placement. Rendering
/// at the plotting library's default size makes four category labels collide
/// and puts the legend on top of the bars; `lilaq` also draws legends *inside*
/// the data area, whereas Word's four edge positions are outside it.
#[test]
fn a_plotted_chart_uses_words_size_and_legend_placement() {
    const CHART: &str = r#"<?xml version="1.0"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"
              xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <c:chart>
    <c:plotArea>
      <c:barChart>
        <c:ser>
          <c:tx><c:v>Series 1</c:v></c:tx>
          <c:cat><c:strRef><c:strCache>
            <c:pt idx="0"><c:v>Alpha</c:v></c:pt>
            <c:pt idx="1"><c:v>Beta</c:v></c:pt>
          </c:strCache></c:strRef></c:cat>
          <c:val><c:numRef><c:numCache>
            <c:pt idx="0"><c:v>1.5</c:v></c:pt>
            <c:pt idx="1"><c:v>2.5</c:v></c:pt>
          </c:numCache></c:numRef></c:val>
        </c:ser>
      </c:barChart>
    </c:plotArea>
    <c:legend><c:legendPos val="b"/></c:legend>
  </c:chart>
</c:chartSpace>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    let rid = rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart",
        "charts/chart1.xml",
        RelMode::Internal,
    );
    // 5486400 EMU = 432pt wide, 2743200 = 216pt tall.
    let doc = format!(
        r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
  <w:body><w:p><w:r><w:drawing><wp:inline>
    <wp:extent cx="5486400" cy="2743200"/>
    <c:chart r:id="{rid}"/>
  </wp:inline></w:drawing></w:r></w:p></w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", doc);
    package.add_xml("word/charts/chart1.xml", "application/xml", CHART.into());
    package.add_relationships("word/document.xml", &rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let opts = ImportOptions { charts: ChartStyle::Plot, ..Default::default() };
    let src = import_docx_with(&bytes, &opts).expect("import should succeed").source;

    assert!(src.contains("width: 432pt"), "Word's extent not used:\n{src}");
    assert!(src.contains("height: 216pt"), "Word's extent not used:\n{src}");
    // `b` is an outside-bottom legend, not lilaq's inside default.
    assert!(
        src.contains("legend: (position: top + center, dy: 100%"),
        "legend placement not mapped:\n{src}"
    );
    assert!(src.contains("lq.bar("), "expected a bar plot:\n{src}");
}

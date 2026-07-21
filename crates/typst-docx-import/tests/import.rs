//! End-to-end integration test for the DOCX → Typst importer: builds a small
//! but realistic Word document in memory (via the OPC writer the exporter also
//! uses, so the reader gets a genuinely well-formed package) and asserts the
//! emitted Typst source is idiomatic — headings as `=`, emphasis as
//! `*`/`_`, and lists as `-`/`+`.

use typst_docx_import::{
    import_docx, import_docx_with, ChartStyle, ImportOptions, Tier, TrackedChanges,
};
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

/// Typst has no end-of-document note store, so an endnote leaves a superscript
/// mark at its reference and its body is collected at the document's end —
/// a real, reported approximation, but one that keeps Word's placement.
#[test]
fn endnote_is_marked_in_place_and_collected_at_the_document_end() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">Body text. </w:t></w:r>
      <w:r><w:endnoteReference w:id="1"/></w:r>
    </w:p>"#;
    let endnotes_xml = note_xml("endnotes", "endnote", "1", "end note text");
    let docx = build_docx_with_notes(doc_body, None, Some(&endnotes_xml));

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        result.source.contains("#super[1]"),
        "endnote reference did not leave a mark:\n{}",
        result.source
    );
    assert!(
        result.source.contains("1. end note text"),
        "endnote body not collected at the document's end:\n{}",
        result.source
    );
    assert!(
        !result.source.contains("#footnote[end note text]"),
        "endnote must not render at a page foot:\n{}",
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
            xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
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

// --- VML shapes (`v:imagedata`/`v:textpath`/`v:rect`/`v:oval`/`v:line`/`v:group`) --

/// `v:imagedata` names a relationship exactly like a DrawingML blip does —
/// real content (`WordWithAttachments.docx`/`drawing.docx` in the POI
/// corpus both use it) that used to be dropped on the floor entirely. This
/// must resolve through the *same* `Figure`/asset pipeline a DrawingML
/// picture does — size included — and actually extract the asset bytes, not
/// just reference the file name.
#[test]
fn vml_imagedata_picture_is_imported_and_extracted_as_an_asset() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:v="urn:schemas-microsoft-com:vml">
  <w:body>
    <w:p><w:r><w:pict>
      <v:shape style="width:54pt;height:38.25pt"><v:imagedata r:id="rId1"/></v:shape>
    </w:pict></w:r></w:p>
  </w:body>
</w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    let image_rid = rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/picture.png",
        RelMode::Internal,
    );
    assert_eq!(image_rid, "rId1");

    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_media("word/media/picture.png", "png", "image/png", vec![0xDE, 0xAD, 0xBE, 0xEF]);
    package.add_relationships("word/document.xml", &rels).unwrap();
    let docx = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&docx).expect("import should succeed");
    assert!(result.source.contains("#image("), "missing image call:\n{}", result.source);
    assert!(result.source.contains("picture.png"), "wrong asset name:\n{}", result.source);
    assert!(result.source.contains("width: 54pt"), "missing size:\n{}", result.source);

    let (_, bytes) = result
        .assets
        .iter()
        .find(|(path, _)| path.to_str().unwrap().contains("picture.png"))
        .expect("picture.png should have been extracted as an asset");
    assert_eq!(bytes, &[0xDE, 0xAD, 0xBE, 0xEF]);
}

/// `v:textpath`'s `string` attribute is WordArt's actual text — genuine
/// document content, currently lost. It must survive as plain text, with the
/// lost curved/warped styling recorded once.
#[test]
fn vml_textpath_wordart_becomes_plain_text_with_a_note() {
    let doc_body = r#"<w:p><w:r><w:pict>
      <v:shape><v:textpath string="My Text Here"/></v:shape>
    </w:pict></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(result.source.contains("My Text Here"), "missing WordArt text:\n{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "WordArt"),
        "expected a WordArt approximation note: {:?}",
        result.report.notes
    );
}

#[test]
fn vml_rect_becomes_a_native_rect_call() {
    let doc_body = r##"<w:p><w:r><w:pict>
      <v:rect style="width:100pt;height:50pt" fillcolor="#FF0000"/>
    </w:pict></w:r></w:p>"##;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(
        src.contains("#rect(width: 100pt, height: 50pt, fill: rgb(\"FF0000\"))"),
        "missing rect call:\n{src}"
    );
}

#[test]
fn vml_line_becomes_a_native_line_call() {
    let doc_body = r#"<w:p><w:r><w:pict>
      <v:line from="0pt,0pt" to="100pt,0pt"/>
    </w:pict></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#line(length: 100pt)"), "missing line call:\n{src}");
}

/// A `v:group` can hold several shapes side by side; every one of them must
/// survive, not just the first — the same hazard `direct_txbx_contents`
/// already guards against for text boxes, now exercised for native shapes.
#[test]
fn vml_group_yields_every_shape_not_just_the_first() {
    let doc_body = r#"<w:p><w:r><w:pict>
      <v:group>
        <v:rect style="width:10pt;height:10pt"/>
        <v:oval style="width:20pt;height:20pt"/>
      </v:group>
    </w:pict></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#rect("), "missing the group's rect:\n{src}");
    assert!(src.contains("#circle("), "missing the group's oval:\n{src}");
}

/// A `v:shape` with custom `v:path`/`v:formulas` geometry and no text box —
/// out of scope by design, since Typst has no drawing-package-free way to
/// render it — must still degrade gracefully: the surrounding text survives,
/// and the dropped geometry is recorded rather than silently vanishing.
#[test]
fn vml_shape_with_custom_geometry_and_no_text_box_is_dropped_with_a_note() {
    let doc_body = r##"<w:p>
      <w:r><w:t xml:space="preserve">before </w:t></w:r>
      <w:r><w:pict><v:shape type="#_x0000_t100"><v:path/></v:shape></w:pict></w:r>
      <w:r><w:t xml:space="preserve"> after</w:t></w:r>
    </w:p>"##;
    let docx = docx_with_body(doc_body);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        result.source.contains("before") && result.source.contains("after"),
        "surrounding text should survive:\n{}",
        result.source
    );
    assert!(
        result.report.notes.iter().any(|n| n.what == "VML shape" && n.detail.contains("geometry")),
        "expected a dropped-geometry note: {:?}",
        result.report.notes
    );
}

// --- Math (OMML) ---------------------------------------------------------

/// End-to-end: a real `m:oMath` fragment — the exporter's own shape for
/// `x^2 + sqrt(y) - frac(a, b)` (three constructs, `sSup`/`rad`/`f`, at
/// once) — sitting inline among ordinary paragraph text, through the full
/// parse -> lower -> emit pipeline. `mappers::math`'s own unit tests cover
/// each construct in isolation; this checks the mapper is actually wired up
/// end to end and that the equation lands as real Typst math, not the old
/// linearized-to-text fallback (which would have produced unrelated,
/// unstructured text here instead of `sqrt`/`frac`/`^`).
#[test]
fn omml_equation_lowers_to_real_typst_math_inline() {
    let doc_body = r#"<w:p>
      <w:r><w:t xml:space="preserve">The result is </w:t></w:r>
      <m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
        <m:sSup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e>
                <m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>2</m:t></m:r></m:sup></m:sSup>
        <m:r><m:rPr><m:nor/></m:rPr><m:t>+</m:t></m:r>
        <m:rad><m:radPr><m:degHide m:val="on"/></m:radPr><m:deg/>
          <m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑦</m:t></m:r></m:e></m:rad>
        <m:r><m:rPr><m:nor/></m:rPr><m:t>−</m:t></m:r>
        <m:f><m:num><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑎</m:t></m:r></m:num>
             <m:den><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑏</m:t></m:r></m:den></m:f>
      </m:oMath>
      <w:r><w:t>.</w:t></w:r>
    </w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(
        src.contains("$x^2 + sqrt(y) - frac(a, b)$"),
        "expected real Typst math (sSup/rad/f folded to plain letters), not the old \
         linearized fallback:\n{src}"
    );
    assert!(src.contains("The result is"), "surrounding paragraph text must survive:\n{src}");
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

/// Tracked changes render as "all changes accepted" in **both** modes: an
/// insertion's text is live content, a deletion's is not. `delins.docx` in the
/// POI corpus has 43 insertions that were once being dropped wholesale.
///
/// Under the default `Preserve` they also keep their record, as invisible
/// metadata — so the deleted words are readable through `#query` without ever
/// reaching the page.
#[test]
fn tracked_changes_render_accepted_and_keep_their_record() {
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
    // Insertions are live text, bracketed by a record.
    assert!(src.contains("InsertedText"), "w:ins content was dropped:\n{src}");
    assert!(src.contains("MovedIn"), "w:moveTo content was dropped:\n{src}");
    assert!(src.contains("kind: \"insertion\""), "no insertion record:\n{src}");
    assert!(src.contains("<ins-1>"), "insertion not opened:\n{src}");
    assert!(src.contains("<ins-1-end>"), "insertion not closed:\n{src}");
    // A deletion's text is inside the metadata *value* — never loose in the
    // markup, so it cannot render.
    assert!(
        src.contains("kind: \"deletion\", author: \"a\", body: [DeletedText"),
        "deleted text not carried in the record:\n{src}"
    );
    let loose = src.replace("body: [DeletedText ]", "").replace("body: [MovedOut]", "");
    assert!(!loose.contains("DeletedText"), "w:del content leaked into the text:\n{src}");
    assert!(!loose.contains("MovedOut"), "w:moveFrom leaked into the text:\n{src}");

    // `Accept` throws the record away and leaves the same visible text.
    let opts = ImportOptions { tracked: TrackedChanges::Accept, ..Default::default() };
    let accepted = import_docx_with(&bytes, &opts).expect("import should succeed").source;
    assert!(accepted.contains("Kept") && accepted.contains("InsertedText"));
    assert!(!accepted.contains("DeletedText"), "accept kept a deletion:\n{accepted}");
    assert!(!accepted.contains("metadata"), "accept mode emitted a record:\n{accepted}");
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

/// Fix: `[content](args)` is Typst's own call-with-trailing-argument-list
/// syntax, so a parenthetical citation right after a colored run's closing
/// `#text(..)[..]` used to be read as that call's own arguments — surfacing
/// as `the character & is not valid in code`. The parenthesis must render
/// literally instead.
#[test]
fn parenthetical_after_a_styled_run_does_not_become_a_trailing_argument_list() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p>
    <w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t xml:space="preserve">volatility </w:t></w:r>
    <w:r><w:t>(Easterly &amp; Kraay, 2000)</w:t></w:r>
  </w:p>
</w:body></w:document>"#;
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("\\(Easterly & Kraay, 2000)"), "{src}");
}

/// Fix: `[#pagebreak()],` inside a table cell — Typst rejects a page break
/// inside any container ("pagebreaks are not allowed inside of
/// containers"). The cell's break-only paragraph must be dropped, not
/// emitted, and the rest of the table must survive.
#[test]
fn page_break_inside_a_table_cell_is_dropped_and_the_rest_of_the_table_survives() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:tbl>
    <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
    <w:tr><w:tc><w:p><w:r><w:br w:type="page"/></w:r></w:p></w:tc></w:tr>
    <w:tr><w:tc><w:p><w:r><w:t>still here</w:t></w:r></w:p></w:tc></w:tr>
  </w:tbl>
</w:body></w:document>"#;
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    assert!(!result.source.contains("#pagebreak()"), "{}", result.source);
    assert!(result.source.contains("still here"), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "page/column break"),
        "{:?}",
        result.report.notes
    );
}

/// Fix: `= #outline()` — a TOC field directly inside a heading. A live
/// `#outline()` there renders every heading (including this one) again,
/// forever ("maximum show rule depth exceeded"); it must fall back to the
/// field's cached text instead, same as any other unmapped field.
#[test]
fn toc_field_inside_a_heading_falls_back_to_cached_text() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
    <w:r><w:fldChar w:fldCharType="begin"/></w:r>
    <w:r><w:instrText xml:space="preserve"> TOC \o "1-3" \h </w:instrText></w:r>
    <w:r><w:fldChar w:fldCharType="separate"/></w:r>
    <w:r><w:t>Stale Contents</w:t></w:r>
    <w:r><w:fldChar w:fldCharType="end"/></w:r>
  </w:p>
</w:body></w:document>"#;
    let styles = r#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
</w:styles>"#;
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_xml("word/styles.xml", "application/xml", styles.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    assert!(!result.source.contains("#outline()"), "{}", result.source);
    assert!(result.source.contains("= Stale Contents"), "{}", result.source);
    assert!(result.report.notes.iter().any(|n| n.what == "field TOC"), "{:?}", result.report.notes);
}

/// Fix: `word/media/image1.jpeg` that actually begins `\x89PNG` — a real
/// producer bug (`lo-sw-floattable-del-empty.docx`). Typst decodes by
/// extension, so the lying name must be corrected by sniffing the real
/// bytes, not just trusted or rejected.
#[test]
fn an_image_with_a_lying_extension_is_sniffed_and_renamed() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
<w:body><w:p><w:r><w:drawing><wp:inline>
  <wp:extent cx="914400" cy="914400"/>
  <a:graphic><a:graphicData>
    <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
  </a:graphicData></a:graphic>
</wp:inline></w:drawing></w:r></w:p></w:body>
</w:document>"#;
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    let image_rid = rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/image1.jpeg",
        RelMode::Internal,
    );
    assert_eq!(image_rid, "rId1");

    let mut png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png_bytes.extend_from_slice(&[0u8; 16]);
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_media("word/media/image1.jpeg", "jpeg", "image/jpeg", png_bytes.clone());
    package.add_relationships("word/document.xml", &rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    assert!(result.source.contains("image1.png"), "{}", result.source);
    assert!(!result.source.contains("image1.jpeg"), "{}", result.source);
    let (path, asset_bytes) =
        result.assets.first().expect("expected the sniffed PNG to be extracted as an asset");
    assert_eq!(path.to_str().unwrap(), "assets/image1.png");
    assert_eq!(asset_bytes, &png_bytes);
}

/// Fix: a malformed OMML equation or a duplicated attribute anywhere in
/// `word/document.xml` used to abort the whole import (`roxmltree` builds
/// one tree per part, so a single bad element failed the entire parse).
/// Reproduces both real corpus failures (`lo-sw-math-malformed_xml`,
/// `lo-sw-tdf165348_broken_package`) end to end via [`import_docx`]: the
/// import must now succeed, dropping only the offending fragment.
#[test]
fn malformed_equation_and_duplicate_attribute_degrade_instead_of_aborting_the_import() {
    use std::io::{Cursor, Write};

    const CT: &str = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    const RELS: &str = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    // `lo-sw-math-malformed_xml`: `<m:t>...</m:sPre>` inside an otherwise
    // well-formed `word/document.xml`.
    let equation_doc = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                                       xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math">
      <w:body>
        <w:p><m:oMath><m:r><m:t xml:space="preserve">+</m:t></m:r><m:r><m:t xml:space="preserve">a</m:sPre></m:r></m:oMath></w:p>
        <w:p><w:r><w:t>still here</w:t></w:r></w:p>
      </w:body>
    </w:document>"#;

    // `lo-sw-tdf165348_broken_package`: `<w:jc w:val="center" w:val="center"/>`.
    let duplicate_attr_doc = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
      <w:body>
        <w:p><w:pPr><w:jc w:val="center" w:val="center"/></w:pPr><w:r><w:t>hi</w:t></w:r></w:p>
      </w:body>
    </w:document>"#;

    for doc in [equation_doc, duplicate_attr_doc] {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (name, body) in
            [("[Content_Types].xml", CT), ("_rels/.rels", RELS), ("word/document.xml", doc)]
        {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();

        let result = import_docx(&bytes).expect("import should degrade, not abort");
        assert!(result.source.contains("still here") || result.source.contains("hi"));
        assert!(
            result.report.notes.iter().any(|n| n.what == "word/document.xml"),
            "{:?}",
            result.report.notes
        );
    }
}

/// `w:outlineLvl` numbers heading levels 1..=9 as 0..=8, and reserves **9 for
/// body text**. A style that says "I am body text" must not import as a
/// heading — `lo-sw-tdf128245.docx` has eight paragraphs in a `Body` style
/// carrying `outlineLvl 9`, and every one of them was becoming a heading.
#[test]
fn outline_level_nine_means_body_text_not_a_heading() {
    const STYLES: &str = r#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/>
    <w:pPr><w:outlineLvl w:val="9"/></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="H2"><w:name w:val="Custom Section"/>
    <w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
</w:styles>"#;

    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:pPr><w:pStyle w:val="Body"/></w:pPr><w:r><w:t>Ordinary prose</w:t></w:r></w:p>
  <w:p><w:pPr><w:pStyle w:val="H2"/></w:pPr><w:r><w:t>A Real Heading</w:t></w:r></w:p>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_xml("word/styles.xml", "application/xml", STYLES.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(
        !src.contains("= Ordinary prose") && !src.contains("====== Ordinary prose"),
        "body text imported as a heading:\n{src}"
    );
    assert!(src.contains("Ordinary prose"), "body text lost:\n{src}");
    // An outline level that really is a heading level still works.
    assert!(src.contains("== A Real Heading"), "real heading lost:\n{src}");
}

/// A literal comma is an argument separator inside every maths call, so one
/// arriving as *text* silently changes the call's arity. Much of the world
/// writes decimals with a comma, and this surfaced on Russian documents whose
/// `frac(1, 565 , 49 …)` became a three-argument `frac`. A half-open interval
/// is the matching fence bug: a bare `(` paired with a `]` leaves the `lr(..)`
/// call unbalanced, so mismatched fences both become symbols.
#[test]
fn literal_commas_and_mismatched_fences_stay_valid_maths() {
    // `m:d` with begChr "(" and endChr "]", containing "0,5" as text.
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><w:body>
  <w:p><m:oMath>
    <m:d>
      <m:dPr><m:begChr m:val="("/><m:endChr m:val="]"/></m:dPr>
      <m:e><m:r><m:t>0,5</m:t></m:r></m:e>
    </m:d>
  </m:oMath></w:p>
</w:body></w:document>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("comma"), "literal comma left as a separator:\n{src}");
    assert!(!src.contains("0,5"), "raw decimal comma survived into maths:\n{src}");
    // Mismatched fences must both be symbols, so nothing is left unbalanced.
    assert!(src.contains("paren.l"), "unmatched `(` fence left bare:\n{src}");
    assert!(src.contains("bracket.r"), "`]` fence not symbolised:\n{src}");
}

/// A mandated template's column count is a hard requirement, not a cosmetic
/// detail: an IEEE call-for-papers layout is two-column and an ACM one is
/// three, and a submission that comes back single-column does not conform.
#[test]
fn section_column_count_survives() {
    let doc = |cols: &str| {
        format!(
            r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:r><w:t>Body</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="12240" w:h="15840"/>{cols}</w:sectPr>
</w:body></w:document>"#
        )
    };
    let import = |body: String| {
        let mut package =
            Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
        package.add_xml("word/document.xml", "application/xml", body);
        package.add_relationships("word/document.xml", &Rels::new()).unwrap();
        let bytes = package.finish(&Rels::new()).unwrap();
        import_docx(&bytes).expect("import should succeed").source
    };

    let src = import(doc(r#"<w:cols w:num="2" w:space="360"/>"#));
    assert!(src.contains("columns: 2"), "two-column layout lost:\n{src}");

    // A single column is Typst's default and shouldn't be restated.
    let src = import(doc(r#"<w:cols w:space="708"/>"#));
    assert!(!src.contains("columns:"), "single column needlessly stated:\n{src}");
}

// --- Multi-section documents -------------------------------------------------

fn import_document(doc_xml: &str) -> String {
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", doc_xml.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();
    import_docx(&bytes).expect("import should succeed").source
}

/// Two `nextPage` sections (the WSU-thesis shape: every section starts a new
/// page) must produce a real `#pagebreak()` between them and a *second*
/// `#set page(..)` for the section after it — the top blocker this feature
/// exists to fix (today only the final section's geometry is honored at all).
#[test]
fn two_next_page_sections_produce_a_break_and_a_second_set_page() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>First section</w:t></w:r></w:p>
  <w:p><w:r><w:t>Second section</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="16838" w:h="11906"/></w:sectPr>
</w:body></w:document>"#;

    let src = import_document(doc);
    assert!(src.contains("First section"), "{src}");
    assert!(src.contains("Second section"), "{src}");
    assert!(src.contains("#pagebreak()"), "missing page break between sections:\n{src}");
    // The preamble's own `#set page(..)` plus a second one for the section
    // after the break — not the "final section applied to the whole
    // document" behaviour this feature replaces.
    assert_eq!(src.matches("#set page(").count(), 2, "{src}");
    assert!(src.contains("width:"), "second section's changed width was dropped:\n{src}");
}

/// The ACM case, and the most important one to get right: a `continuous`
/// section that only changes the column count must render as
/// `#columns(n)[..]` with **no** page break — Word kept the title block and
/// the multi-column body on the same page, and emitting a break here would
/// add a page and wreck the very pagination this feature is meant to
/// preserve.
#[test]
fn continuous_column_change_wraps_in_columns_with_no_page_break() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Title block</w:t></w:r></w:p>
  <w:p><w:r><w:t>Body in columns</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:cols w:num="2"/><w:type w:val="continuous"/></w:sectPr>
</w:body></w:document>"#;

    let src = import_document(doc);
    assert!(src.contains("#columns(2)["), "missing the columns wrapper:\n{src}");
    assert!(src.contains("Body in columns"), "{src}");
    assert!(
        !src.contains("#pagebreak()"),
        "a continuous, geometry-unchanged column switch must not break the page:\n{src}"
    );
    // No new `#set page(..)` either — only the column count changed, and
    // that's expressed by the wrapper, not page geometry.
    assert_eq!(src.matches("#set page(").count(), 1, "{src}");
}

/// A thesis's front matter in roman numerals, restarting at arabic 1 for the
/// body (the Georgia Tech shape) — `w:pgNumType/@w:fmt` must reach `set
/// page(numbering:)` and `@w:start` must reach `#counter(page).update(..)`,
/// on the section that actually declares them.
#[test]
fn page_number_format_and_restart_produce_numbering_and_counter_update() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
  <w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgNumType w:fmt="lowerRoman"/></w:sectPr></w:pPr><w:r><w:t>Front matter</w:t></w:r></w:p>
  <w:p><w:r><w:t>Chapter one</w:t></w:r></w:p>
  <w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgNumType w:fmt="decimal" w:start="1"/></w:sectPr>
</w:body></w:document>"#;

    let src = import_document(doc);
    assert!(src.contains("numbering: \"i\""), "front matter's roman numbering lost:\n{src}");
    assert!(src.contains("numbering: \"1\""), "body's decimal format lost:\n{src}");
    assert!(
        src.contains("#counter(page).update(1)"),
        "the restart to page 1 was dropped:\n{src}"
    );
}

/// Each section resolves its own headers/footers independently — a template
/// whose front matter and body carry different running heads must keep both,
/// not just the document's *final* section's (the behaviour this feature
/// replaces).
#[test]
fn per_section_headers_differ() {
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
  <w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId1"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr></w:pPr><w:r><w:t>Front matter</w:t></w:r></w:p>
  <w:p><w:r><w:t>Chapter one</w:t></w:r></w:p>
  <w:sectPr><w:headerReference w:type="default" r:id="rId2"/><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
</w:body></w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        "header1.xml",
        RelMode::Internal,
    );
    rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        "header2.xml",
        RelMode::Internal,
    );
    package.add_xml("word/document.xml", "application/xml", doc.into());
    package.add_xml(
        "word/header1.xml",
        "application/xml",
        r#"<?xml version="1.0"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Front matter header</w:t></w:r></w:p></w:hdr>"#.into(),
    );
    package.add_xml(
        "word/header2.xml",
        "application/xml",
        r#"<?xml version="1.0"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Body header</w:t></w:r></w:p></w:hdr>"#.into(),
    );
    package.add_relationships("word/document.xml", &rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("header: [Front matter header]"), "first section's header lost:\n{src}");
    assert!(src.contains("header: [Body header]"), "second section's header lost:\n{src}");
}

/// The floor this whole feature must not break: a single-section document
/// (today's only case) must import exactly as it always has — no
/// `Block::Section`, no synthesized page break, no stray `#set page(..)`.
#[test]
fn single_section_document_is_unaffected() {
    let docx = build_docx();
    let src = import_docx(&docx).expect("import should succeed").source;

    assert_eq!(src.matches("#set page(").count(), 1, "{src}");
    assert!(!src.contains("#pagebreak()"), "no section boundary should mean no break:\n{src}");
    assert!(!src.contains("#columns("), "single section should never wrap in columns:\n{src}");
}

// --- DrawingML shapes (`wps:wsp`/`wpg:wgp` inside a `w:drawing`) --------------

/// A `wps:wsp` used to return `None` from the drawing parse and vanish
/// entirely unless it happened to hold a text box. A painted preset shape must
/// now come back as the Typst primitive it names, at the size Word gave it.
#[test]
fn drawingml_preset_shape_becomes_a_native_shape_call() {
    let doc_body = r#"<w:p><w:r><w:drawing><wp:inline
        xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
      <wp:extent cx="2540000" cy="558800"/>
      <wps:wsp><wps:spPr>
        <a:xfrm><a:ext cx="2540000" cy="558800"/></a:xfrm>
        <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        <a:solidFill><a:srgbClr val="C0392B"/></a:solidFill>
      </wps:spPr></wps:wsp>
    </wp:inline></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(
        result.source.contains("#rect(width: 200pt, height: 44pt, fill: rgb(\"C0392B\"))"),
        "missing rect call:\n{}",
        result.source
    );
    assert!(
        result.report.notes.iter().any(|n| n.what == "DrawingML shape"),
        "the lost floating position must be recorded: {:?}",
        result.report.notes
    );
}

/// `a:custGeom` is the geometry VML's counterpart deliberately gives up on,
/// and it maps onto `#curve` command for command. The path's own coordinate
/// space (`a:path/@w`/`@h`) has to be scaled onto the shape's extent, not read
/// as points.
#[test]
fn drawingml_custom_geometry_becomes_a_curve() {
    let doc_body = r#"<w:p><w:r><w:drawing><wps:wsp><wps:spPr>
        <a:xfrm><a:ext cx="1270000" cy="1270000"/></a:xfrm>
        <a:custGeom><a:pathLst><a:path w="1270000" h="1270000">
          <a:moveTo><a:pt x="0" y="0"/></a:moveTo>
          <a:lnTo><a:pt x="1270000" y="0"/></a:lnTo>
          <a:lnTo><a:pt x="1270000" y="1270000"/></a:lnTo>
          <a:close/>
        </a:path></a:pathLst></a:custGeom>
        <a:solidFill><a:srgbClr val="2ECC40"/></a:solidFill>
      </wps:spPr></wps:wsp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(
        src.contains(
            "#curve(fill: rgb(\"2ECC40\"), curve.move((0pt, 0pt)), curve.line((100pt, 0pt)), \
             curve.line((100pt, 100pt)), curve.close())"
        ),
        "missing curve call:\n{src}"
    );
}

/// A shape that both paints and holds text is *one* element in Word and must
/// stay one call in Typst — the text as the shape's body, not beside it, and
/// never emitted twice (the hazard the single-walk `collect_dml_content`
/// exists to avoid).
#[test]
fn a_painted_shape_takes_its_text_box_as_its_body() {
    let doc_body = r#"<w:p><w:r><w:drawing><wps:wsp>
      <wps:spPr>
        <a:xfrm><a:ext cx="2540000" cy="558800"/></a:xfrm>
        <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        <a:noFill/>
        <a:ln w="25400"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln>
      </wps:spPr>
      <wps:txbx><w:txbxContent><w:p><w:r><w:t>Dashed border</w:t></w:r></w:p></w:txbxContent></wps:txbx>
    </wps:wsp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#rect("), "missing rect call:\n{src}");
    assert!(src.contains(")[Dashed border]"), "text should be the shape's body:\n{src}");
    assert_eq!(src.matches("Dashed border").count(), 1, "text emitted twice:\n{src}");
}

/// The regression guard for the rule above: Word's ordinary text box is a
/// `wps:wsp` that paints *nothing*. It must stay a plain `#box[..]` rather
/// than growing a default black border it never had.
#[test]
fn an_unpainted_shape_stays_a_plain_text_box() {
    let doc_body = r#"<w:p><w:r><w:drawing><wps:wsp>
      <wps:spPr>
        <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        <a:noFill/><a:ln><a:noFill/></a:ln>
      </wps:spPr>
      <wps:txbx><w:txbxContent><w:p><w:r><w:t>just words</w:t></w:r></w:p></w:txbxContent></wps:txbx>
    </wps:wsp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#box[just words]"), "expected a plain text box:\n{src}");
    assert!(!src.contains("#rect("), "an unpainted shape must not become a rect:\n{src}");
}

/// A `wpg:wgp` group holds several shapes side by side; every one must
/// survive, not just the first — the same hazard the VML group walk guards
/// against.
#[test]
fn drawingml_group_yields_every_shape() {
    let doc_body = r#"<w:p><w:r><w:drawing><wpg:wgp>
      <wps:wsp><wps:spPr>
        <a:xfrm><a:ext cx="635000" cy="635000"/></a:xfrm>
        <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>
      </wps:spPr></wps:wsp>
      <wps:wsp><wps:spPr>
        <a:xfrm><a:ext cx="635000" cy="635000"/></a:xfrm>
        <a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom>
        <a:solidFill><a:srgbClr val="0000FF"/></a:solidFill>
      </wps:spPr></wps:wsp>
    </wpg:wgp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#rect(width: 50pt, height: 50pt, fill: rgb(\"FF0000\"))"), "{src}");
    assert!(src.contains("#circle(radius: 25pt, fill: rgb(\"0000FF\"))"), "{src}");
}

/// A shape whose colors this importer can't resolve (a theme color) and which
/// holds no text has nothing left to draw — but it must be *recorded* rather
/// than vanishing, which is what it used to do.
#[test]
fn an_unresolvable_empty_shape_is_dropped_with_a_note() {
    let doc_body = r#"<w:p><w:r><w:drawing><wps:wsp><wps:spPr>
        <a:xfrm><a:ext cx="635000" cy="635000"/></a:xfrm>
        <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
        <a:solidFill><a:schemeClr val="accent1"/></a:solidFill>
      </wps:spPr></wps:wsp></w:drawing></w:r></w:p>"#;
    let docx = docx_with_body(doc_body);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(!result.source.contains("#rect("), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "DrawingML shape"),
        "{:?}",
        result.report.notes
    );
}

// --- Picture frames (`a:prstGeom` + `a:srcRect` on a `pic:pic`) ---------------

/// The inverse of `typst-docx`'s native clipped picture: a `roundRect` frame
/// plus an `a:srcRect` crop. The frame becomes a clipping `#box(radius: ..)`,
/// and the crop — which Typst's `image` cannot state directly — becomes an
/// oversized, offset image inside it.
#[test]
fn a_round_rect_picture_frame_and_crop_survive() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:body>
    <w:p><w:r><w:drawing><wp:inline>
      <wp:extent cx="1524000" cy="1016000"/>
      <a:graphic><a:graphicData>
        <pic:pic>
          <pic:blipFill>
            <a:blip r:embed="rId1"/>
            <a:srcRect l="0" t="1515" r="0" b="1515"/>
          </pic:blipFill>
          <pic:spPr>
            <a:prstGeom prst="roundRect"><a:avLst>
              <a:gd name="adj" fmla="val 15000"/>
            </a:avLst></a:prstGeom>
          </pic:spPr>
        </pic:pic>
      </a:graphicData></a:graphic>
    </wp:inline></w:drawing></w:r></w:p>
  </w:body>
</w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/swatch.png",
        RelMode::Internal,
    );
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_media("word/media/swatch.png", "png", "image/png", vec![0x89, b'P', b'N', b'G']);
    package.add_relationships("word/document.xml", &rels).unwrap();
    let docx = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&docx).expect("import should succeed").source;
    // 15000/100000 of the shorter (80pt) side.
    assert!(src.contains("radius: 12pt"), "missing corner radius:\n{src}");
    assert!(src.contains("clip: true"), "the frame must clip:\n{src}");
    // 80pt showing all but 2 × 1.515% of the picture's height ⇒ 82.5pt tall,
    // slid up by the hidden 1.515% band.
    assert!(src.contains("height: 82.5pt"), "image not oversized for the crop:\n{src}");
    assert!(src.contains("dy: -1.25pt"), "image not offset for the crop:\n{src}");
}

/// A picture Word framed with an outline Typst's clipping `#box` cannot draw
/// is left unframed and recorded — never approximated by a rounded rectangle,
/// which would be a different shape.
#[test]
fn an_unmappable_picture_frame_is_reported_rather_than_guessed() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:body>
    <w:p><w:r><w:drawing>
      <pic:pic>
        <pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill>
        <pic:spPr><a:prstGeom prst="star5"><a:avLst/></a:prstGeom></pic:spPr>
      </pic:pic>
    </w:drawing></w:r></w:p>
  </w:body>
</w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut rels = Rels::new();
    rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/swatch.png",
        RelMode::Internal,
    );
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_media("word/media/swatch.png", "png", "image/png", vec![0x89, b'P', b'N', b'G']);
    package.add_relationships("word/document.xml", &rels).unwrap();
    let docx = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&docx).expect("import should succeed");
    assert!(!result.source.contains("clip: true"), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "picture frame"),
        "{:?}",
        result.report.notes
    );
}

// --- Table placement (`w:tblPr/w:jc`, `w:tblPr/w:tblInd`) ---------------------

/// A table's own `w:jc` places the whole table between the margins — a
/// different element from the paragraph `w:jc` inside its cells, and the one
/// that closes the round-trip with `typst-docx`'s `#align(center)[table]`.
#[test]
fn a_centered_table_is_wrapped_in_an_align() {
    let doc_body = r#"<w:tbl>
      <w:tblPr><w:jc w:val="center"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#align(center)[#table("), "table not centred:\n{src}");
}

/// `w:tblInd` is a left indent in twips, which becomes a `#pad`.
#[test]
fn a_table_indent_becomes_a_pad() {
    let doc_body = r#"<w:tbl>
      <w:tblPr><w:tblInd w:w="720" w:type="dxa"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>"#;
    let docx = docx_with_body(doc_body);

    let src = import_docx(&docx).expect("import should succeed").source;
    assert!(src.contains("#pad(left: 36pt)[#table("), "table not indented:\n{src}");
}

/// Word itself ignores a table's indent once the table is centred, so the two
/// must not both be emitted and fight each other.
#[test]
fn a_centered_table_drops_its_indent_with_a_note() {
    let doc_body = r#"<w:tbl>
      <w:tblPr><w:jc w:val="center"/><w:tblInd w:w="720" w:type="dxa"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>"#;
    let docx = docx_with_body(doc_body);

    let result = import_docx(&docx).expect("import should succeed");
    assert!(result.source.contains("#align(center)["), "{}", result.source);
    assert!(!result.source.contains("#pad(left:"), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "table indent"),
        "{:?}",
        result.report.notes
    );
}

// --- Custom bullet markers (`w:lvlText` on a bullet level) -------------------

/// A bullet level's `w:lvlText` is the authored marker glyph, and the only
/// record of it. Without this every custom bullet came back as Typst's
/// default dot.
#[test]
fn an_authored_bullet_glyph_becomes_a_list_marker() {
    let doc_body = r#"
      <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>first</w:t></w:r></w:p>
      <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>second</w:t></w:r></w:p>"#;
    let numbering = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#x2023;"/></w:lvl>
    <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xB7;"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;
    let src = import_docx(&docx_with_body_and_numbering(doc_body, numbering))
        .expect("import should succeed")
        .source;
    assert!(src.contains("#set list(marker: ([\u{2023}], [\u{b7}]))"), "{src}");
}

/// Word stores a Symbol/Wingdings bullet as a private-use codepoint that only
/// means anything alongside that level's font. Emitted literally it renders as
/// tofu, so it must fall back to the default bullet and say so.
#[test]
fn a_private_use_bullet_falls_back_to_the_default_marker() {
    let doc_body = r#"
      <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>first</w:t></w:r></w:p>"#;
    let numbering = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#xF0B7;"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;
    let result = import_docx(&docx_with_body_and_numbering(doc_body, numbering))
        .expect("import should succeed");
    assert!(!result.source.contains("#set list(marker:"), "{}", result.source);
    assert!(!result.source.contains('\u{f0b7}'), "{}", result.source);
    assert!(
        result.report.notes.iter().any(|n| n.what == "list bullet"),
        "{:?}",
        result.report.notes
    );
}

/// A list whose markers already *are* Typst's defaults needs no set rule —
/// emitting one would be pure noise in the output.
#[test]
fn default_bullet_glyphs_emit_no_set_rule() {
    let doc_body = r#"
      <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
        <w:r><w:t>first</w:t></w:r></w:p>"#;
    let numbering = r#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#x2022;"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;
    let src = import_docx(&docx_with_body_and_numbering(doc_body, numbering))
        .expect("import should succeed")
        .source;
    assert!(!src.contains("#set list(marker:"), "{src}");
    assert!(src.contains("- first"), "{src}");
}

/// A document body plus its own `word/numbering.xml` — the numbering-bearing
/// counterpart of [`docx_with_body`].
fn docx_with_body_and_numbering(doc_body: &str, numbering_xml: &str) -> Vec<u8> {
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{doc_body}</w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    package.add_xml("word/numbering.xml", "application/xml", numbering_xml.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    package.finish(&Rels::new()).unwrap()
}

/// A document body plus a `word/settings.xml` — the document-wide switches
/// (`w:mirrorMargins`, `w:evenAndOddHeaders`) live there, not in the body.
fn docx_with_body_and_settings(doc_body: &str, settings_xml: &str) -> Vec<u8> {
    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<w:body>{doc_body}</w:body>
</w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    package.add_xml("word/settings.xml", "application/xml", settings_xml.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    package.finish(&Rels::new()).unwrap()
}

/// Word's default table has *no* visible borders; Typst's draws a 1pt grid.
/// Leaving `w:tblBorders` unread therefore didn't lose a detail, it invented
/// lines the document never had — so a table that states `nil` all round must
/// come across as `stroke: none`.
#[test]
fn a_borderless_word_table_does_not_gain_typsts_default_grid() {
    let body = r#"<w:tbl>
        <w:tblPr><w:tblBorders>
          <w:top w:val="nil"/><w:bottom w:val="nil"/>
          <w:left w:val="nil"/><w:right w:val="nil"/>
          <w:insideH w:val="nil"/><w:insideV w:val="nil"/>
        </w:tblBorders></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:p><w:r><w:t>plain</w:t></w:r></w:p></w:tc></w:tr>
      </w:tbl>"#;
    let src = import_docx(&docx_with_body(body)).expect("import should succeed").source;
    assert!(src.contains("stroke: none"), "borderless table gained a grid:\n{src}");
}

/// A stated width comes across as that width, in Typst's own units: Word's
/// `w:sz` is eighths of a point, so `sz="16"` is a 2pt rule.
#[test]
fn a_table_border_width_survives_in_points() {
    let body = r#"<w:tbl>
        <w:tblPr><w:tblBorders>
          <w:top w:val="single" w:sz="16" w:color="FF0000"/>
          <w:bottom w:val="single" w:sz="16" w:color="FF0000"/>
          <w:left w:val="single" w:sz="16" w:color="FF0000"/>
          <w:right w:val="single" w:sz="16" w:color="FF0000"/>
          <w:insideH w:val="single" w:sz="16" w:color="FF0000"/>
          <w:insideV w:val="single" w:sz="16" w:color="FF0000"/>
        </w:tblBorders></w:tblPr>
        <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:tc><w:p><w:r><w:t>ruled</w:t></w:r></w:p></w:tc></w:tr>
      </w:tbl>"#;
    let src = import_docx(&docx_with_body(body)).expect("import should succeed").source;
    assert!(src.contains("stroke: 2pt + rgb(\"FF0000\")"), "border lost:\n{src}");
}

/// Typst's fixed track "will be exactly of this size", so only Word's `exact`
/// rule can become one. `atLeast` is a *minimum* the row grows past, which has
/// no Typst track size at all — importing it as a fixed height would clip
/// every row whose content outgrew Word's floor, so those stay content-sized
/// and the loss is reported instead.
#[test]
fn only_an_exact_row_height_becomes_a_track_size() {
    let body = r#"<w:tbl>
        <w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>
        <w:tr><w:trPr><w:trHeight w:val="1440" w:hRule="exact"/></w:trPr>
          <w:tc><w:p><w:r><w:t>fixed</w:t></w:r></w:p></w:tc></w:tr>
        <w:tr><w:trPr><w:trHeight w:val="2880" w:hRule="atLeast"/></w:trPr>
          <w:tc><w:p><w:r><w:t>floor</w:t></w:r></w:p></w:tc></w:tr>
      </w:tbl>"#;
    let result = import_docx(&docx_with_body(body)).expect("import should succeed");
    let src = &result.source;
    // 1440 twips = 72pt for the exact row; the `atLeast` row stays `auto`
    // rather than becoming a 144pt track.
    assert!(src.contains("rows: (72pt, auto)"), "row heights wrong:\n{src}");
    assert!(!src.contains("144pt"), "an atLeast minimum became a fixed height:\n{src}");
    assert!(
        result.report.notes.iter().any(|n| n.what == "table row height"),
        "the unrepresentable minimum went unreported"
    );
}

/// A paragraph boxed on all four sides becomes a bordered block — while the
/// lone-bottom-border idiom (Word's "section title underline") keeps lowering
/// to a `#line`, which is what it actually looks like.
#[test]
fn a_boxed_paragraph_gets_a_stroke_but_a_lone_bottom_rule_stays_a_line() {
    let body = r#"<w:p><w:pPr><w:pBdr>
        <w:top w:val="single" w:sz="8" w:space="4"/>
        <w:bottom w:val="single" w:sz="8" w:space="4"/>
        <w:left w:val="single" w:sz="8" w:space="4"/>
        <w:right w:val="single" w:sz="8" w:space="4"/>
      </w:pBdr></w:pPr><w:r><w:t>boxed</w:t></w:r></w:p>
      <w:p><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="4"/></w:pBdr></w:pPr></w:p>"#;
    let src = import_docx(&docx_with_body(body)).expect("import should succeed").source;
    assert!(src.contains("stroke: (top: 1pt"), "paragraph box lost:\n{src}");
    assert!(src.contains("inset: 4pt"), "border spacing lost:\n{src}");
    assert!(src.contains("#line(length: 100%)"), "bottom rule idiom regressed:\n{src}");
}

/// `w:keepLines` has an exact Typst counterpart; `w:keepNext` does not, and
/// says so rather than vanishing.
#[test]
fn keep_lines_becomes_an_unbreakable_block_and_keep_next_is_reported() {
    let body = r#"<w:p><w:pPr><w:keepLines/><w:keepNext/></w:pPr>
        <w:r><w:t>together</w:t></w:r></w:p>"#;
    let result = import_docx(&docx_with_body(body)).expect("import should succeed");
    assert!(
        result.source.contains("breakable: false"),
        "keepLines lost:\n{}",
        result.source
    );
    assert!(
        result.report.notes.iter().any(|n| n.what == "keep with next"),
        "keepNext went unreported"
    );
}

/// Mirrored margins are Typst's `inside`/`outside` pair — the one spelling
/// that swaps on facing pages the way Word does — and Word's separate binding
/// allowance folds into the inner one.
#[test]
fn mirrored_margins_become_inside_outside_and_absorb_the_gutter() {
    let body = r#"<w:p><w:r><w:t>text</w:t></w:r></w:p>
      <w:sectPr><w:pgSz w:w="12240" w:h="15840"/>
        <w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:gutter="720"/>
      </w:sectPr>"#;
    let settings = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:mirrorMargins/></w:settings>"#;
    let src = import_docx(&docx_with_body_and_settings(body, settings))
        .expect("import should succeed")
        .source;
    // 1440 twips = 72pt, plus a 720-twip (36pt) gutter on the binding side.
    assert!(src.contains("inside: 108pt"), "gutter not folded in:\n{src}");
    assert!(src.contains("outside: 72pt"), "outer margin wrong:\n{src}");
    assert!(!src.contains("left: 108pt"), "still emitting fixed sides:\n{src}");
}

/// Word measures its header band from the page edge; Typst measures the gap on
/// the *body* side of it. A header pushed unusually far down the page must
/// therefore come across as a *narrower* ascent, not be silently defaulted.
#[test]
fn an_unusual_header_distance_reaches_the_page_setup() {
    const HEADER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:p><w:r><w:t>Running head</w:t></w:r></w:p></w:hdr>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    let header_rid = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        "header1.xml",
        RelMode::Internal,
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<w:body>
  <w:p><w:r><w:t>Body.</w:t></w:r></w:p>
  <w:sectPr>
    <w:headerReference w:type="default" r:id="{header_rid}"/>
    <w:pgSz w:w="12240" w:h="15840"/>
    <w:pgMar w:top="2880" w:bottom="1440" w:left="1440" w:right="1440" w:header="2160"/>
  </w:sectPr>
</w:body></w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    package.add_xml("word/header1.xml", "application/xml", HEADER_XML.into());
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.add_relationships("word/header1.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    // Top margin 144pt, header starting 108pt down, one 13.2pt line of text:
    // 144 - 108 - 13.2 = 22.8pt, against the 43.2pt Typst would have defaulted.
    assert!(src.contains("header-ascent: 22.8pt"), "header band not converted:\n{src}");
}

/// An *inline* picture is placed by the paragraph holding it, so a centred
/// figure is a plain `w:jc` — reading only the float spelling
/// (`wp:anchor/wp:positionH`) left every centred image flush left.
#[test]
fn a_centred_picture_paragraph_centres_the_figure() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
            xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
            xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
  <w:body>
    <w:p>
      <w:pPr><w:jc w:val="center"/></w:pPr>
      <w:r><w:drawing><wp:inline>
        <wp:extent cx="914400" cy="457200"/>
        <wp:docPr id="1" name="Figure"/>
        <a:graphic><a:graphicData>
          <pic:pic><pic:blipFill><a:blip r:embed="rId1"/></pic:blipFill></pic:pic>
        </a:graphicData></a:graphic>
      </wp:inline></w:drawing></w:r>
    </w:p>
  </w:body></w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/image1.png",
        RelMode::Internal,
    );
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_media(
        "word/media/image1.png",
        "png",
        "image/png",
        vec![0x89, b'P', b'N', b'G'],
    );
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("#image("), "the picture itself was lost:\n{src}");
    assert!(src.contains("#align(center)["), "the paragraph's centring was lost:\n{src}");
}

/// Word permits a *negative* page margin — the body then bleeds up into the
/// header band. `tdf119952_negativeMargins` in the wide corpus does exactly
/// that, and it panicked the furniture-band conversion outright (a clamp whose
/// range ran backwards) before this guard.
#[test]
fn a_negative_page_margin_does_not_panic_the_furniture_band() {
    const HEADER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:p><w:r><w:t>Head</w:t></w:r></w:p></w:hdr>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    let header_rid = doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header",
        "header1.xml",
        RelMode::Internal,
    );
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<w:body>
  <w:p><w:r><w:t>Body.</w:t></w:r></w:p>
  <w:sectPr>
    <w:headerReference w:type="default" r:id="{header_rid}"/>
    <w:pgSz w:w="12240" w:h="15840"/>
    <w:pgMar w:top="-1134" w:bottom="1440" w:left="1440" w:right="1440" w:header="720"/>
  </w:sectPr>
</w:body></w:document>"#
    );
    package.add_xml("word/document.xml", "application/xml", document_xml);
    package.add_xml("word/header1.xml", "application/xml", HEADER_XML.into());
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    package.add_relationships("word/header1.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    // No band to place furniture within, so no gap is stated at all.
    assert!(!src.contains("header-ascent"), "invented a band gap:\n{src}");
    assert!(src.contains("Body."), "content lost:\n{src}");
}

/// An OLE embedding (`w:object`) is a whole foreign application's document —
/// an Excel sheet, a MathType equation — that nothing here can revive. But
/// Word renders a **preview picture** beside it, and the entire element used
/// to fall through the run parser, so that preview went with it. Now the
/// picture is kept and the payload is reported by name.
#[test]
fn an_ole_object_keeps_its_preview_picture_and_reports_what_was_embedded() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
            xmlns:v="urn:schemas-microsoft-com:vml"
            xmlns:o="urn:schemas-microsoft-com:office:office">
  <w:body>
    <w:p><w:r>
      <w:object w:dxaOrig="7247" w:dyaOrig="2920">
        <v:shape id="_x0000_i1025" style="width:120pt;height:60pt" o:ole="">
          <v:imagedata r:id="rId1" o:title=""/>
        </v:shape>
        <o:OLEObject Type="Embed" ProgID="Excel.Sheet.12" ShapeID="_x0000_i1025"
                     DrawAspect="Content" ObjectID="_1452851351" r:id="rId2"/>
      </w:object>
    </w:r></w:p>
  </w:body></w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    let mut doc_rels = Rels::new();
    doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
        "media/image1.png",
        RelMode::Internal,
    );
    // The payload itself — a real package declares it, and the importer never
    // follows it. Present so the fixture is a well-formed OLE embedding.
    doc_rels.add(
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject",
        "embeddings/oleObject1.bin",
        RelMode::Internal,
    );
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_media(
        "word/media/image1.png",
        "png",
        "image/png",
        vec![0x89, b'P', b'N', b'G'],
    );
    package.add_media(
        "word/embeddings/oleObject1.bin",
        "bin",
        "application/vnd.openxmlformats-officedocument.oleObject",
        vec![0xD0, 0xCF, 0x11, 0xE0],
    );
    package.add_relationships("word/document.xml", &doc_rels).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    let src = &result.source;
    // The preview survives, at the size the shape's own `style` declared.
    assert!(src.contains("#image("), "the preview picture was lost:\n{src}");
    assert!(src.contains("width: 120pt"), "the preview's size was lost:\n{src}");
    // And the payload is named, not merely counted.
    let note = result
        .report
        .notes
        .iter()
        .find(|n| n.what == "embedded object")
        .expect("the embedded object went unreported");
    assert!(
        note.detail.contains("Excel.Sheet.12"),
        "the report should name the producer, got: {}",
        note.detail
    );
    // Exactly one note for one loss: the object's own placeholder shape must
    // not also raise a vague "unsupported VML geometry" drop beside it.
    assert!(
        !result.report.notes.iter().any(|n| n.what == "VML shape"),
        "double-reported one embedded object"
    );
}

/// A Word comment is an *annotation*, not content: it must not reach the page.
/// It lowers to a labelled `#metadata`, which renders nothing but is reachable
/// through `#query`, so the information survives without the import deciding
/// to print it. A commented *span* becomes two anchors, because a Typst label
/// attaches to a single element.
#[test]
fn comments_become_invisible_queryable_metadata_delimiting_the_commented_span() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">Keep </w:t></w:r>
      <w:commentRangeStart w:id="7"/>
      <w:r><w:t>this bit</w:t></w:r>
      <w:commentRangeEnd w:id="7"/>
      <w:r><w:commentReference w:id="7"/></w:r>
      <w:r><w:t xml:space="preserve"> intact.</w:t></w:r>
    </w:p>
  </w:body></w:document>"#;

    const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:id="7" w:author="Ada Lovelace" w:initials="AL" w:date="2026-07-21T09:00:00Z">
    <w:p>
      <w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:annotationRef/></w:r>
      <w:r><w:t xml:space="preserve">Please </w:t></w:r>
      <w:r><w:rPr><w:b/></w:rPr><w:t>double-check</w:t></w:r>
      <w:r><w:t xml:space="preserve"> this.</w:t></w:r>
    </w:p>
  </w:comment>
</w:comments>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/comments.xml", "application/xml", COMMENTS_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;

    // The annotated text is untouched, and the anchors bracket exactly it.
    assert!(src.contains("Keep "), "body text lost:\n{src}");
    assert!(src.contains("intact."), "body text lost:\n{src}");
    let open = src.find("<comment-7>").expect("no opening anchor");
    let close = src.find("<comment-7-end>").expect("no closing anchor");
    let bit = src.find("this bit").expect("commented text lost");
    assert!(open < bit && bit < close, "anchors don't bracket the commented span:\n{src}");

    // The payload rides on the opening anchor, as an invisible `#metadata`.
    assert!(src.contains("kind: \"comment\""), "no comment payload:\n{src}");
    assert!(src.contains("author: \"Ada Lovelace\""), "author lost:\n{src}");
    assert!(src.contains("initials: \"AL\""), "initials lost:\n{src}");
    assert!(src.contains("date: \"2026-07-21T09:00:00Z\""), "date lost:\n{src}");
    // The body stays content, so its own formatting survives into the value.
    assert!(src.contains("*double-check*"), "comment body formatting lost:\n{src}");
    // `w:annotationRef` is the mark placeholder Word substitutes; left alone
    // it would surface as stray text inside the comment body.
    assert!(!src.contains("annotationRef"), "annotation ref leaked:\n{src}");
    // The closing anchor carries no payload — one comment, stated once.
    assert!(src.contains("#metadata(none) <comment-7-end>"), "bad closing anchor:\n{src}");
}

/// A comment Word anchored to a *point* writes no range pair at all — only
/// the reference mark. The payload has to ride on that instead, or a
/// point-anchored comment would import as nothing.
#[test]
fn a_point_anchored_comment_carries_its_payload_on_the_reference_mark() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Some text.</w:t></w:r>
      <w:r><w:commentReference w:id="3"/></w:r>
    </w:p>
  </w:body></w:document>"#;

    const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:id="3" w:author="Grace Hopper">
    <w:p><w:r><w:t>A point remark.</w:t></w:r></w:p>
  </w:comment>
</w:comments>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/comments.xml", "application/xml", COMMENTS_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    assert!(src.contains("author: \"Grace Hopper\""), "point comment lost:\n{src}");
    assert!(src.contains("<comment-3>"), "no anchor:\n{src}");
    assert!(!src.contains("comment-3-end"), "invented a range that Word never wrote:\n{src}");
}

/// An anchor whose comment is missing from `word/comments.xml` is dangling.
/// Word never writes that, but a hand-edited document can — it must be
/// reported rather than emitting a label with nothing behind it.
#[test]
fn a_dangling_comment_anchor_is_reported_and_emits_nothing() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:commentRangeStart w:id="9"/>
      <w:r><w:t>Orphaned.</w:t></w:r>
      <w:commentRangeEnd w:id="9"/>
    </w:p>
  </w:body></w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    assert!(result.source.contains("Orphaned."), "text lost with the anchor:\n{}", result.source);
    assert!(!result.source.contains("comment-9"), "emitted a label with nothing behind it");
    assert!(result.report.notes.iter().any(|n| n.what == "comment"), "went unreported");
}

/// A label binds to the element it follows — *except* at the end of a heading,
/// where it binds to the heading instead of to the `#metadata` it was written
/// after, which left the record unreachable by `#query`. Anchors on a heading
/// are therefore hoisted to their own block just before it. (Lists, ordinary
/// paragraphs and table cells all bind correctly and keep their anchors in
/// place, where they mark the exact words.)
#[test]
fn an_anchor_on_a_heading_is_hoisted_so_its_label_still_binds_to_the_record() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:commentRangeStart w:id="4"/>
      <w:r><w:t>Chapter One</w:t></w:r>
      <w:commentRangeEnd w:id="4"/>
    </w:p>
  </w:body></w:document>"#;

    const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>
</w:styles>"#;

    const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:comment w:id="4" w:author="Reviewer">
    <w:p><w:r><w:t>Retitle this.</w:t></w:r></w:p></w:comment>
</w:comments>"#;

    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("word/styles.xml", "application/xml", STYLES_XML.into());
    package.add_xml("word/comments.xml", "application/xml", COMMENTS_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let src = import_docx(&bytes).expect("import should succeed").source;
    let heading = src.find("= Chapter One").expect("heading lost");
    let anchor = src.find("<comment-4>").expect("comment lost");
    // Before the heading, on its own line — not trailing inside it, where the
    // label would silently bind to the heading element instead.
    assert!(anchor < heading, "anchor not hoisted ahead of the heading:\n{src}");
    assert!(
        !src[heading..].starts_with("= Chapter One #metadata"),
        "anchor left inside the heading:\n{src}"
    );
    assert!(src.contains("author: \"Reviewer\""), "payload lost:\n{src}");
}

/// Word's Source Manager lives in a `customXml` part, keyed by a `b:Tag` that
/// is exactly what a `CITATION` field names — and exactly what `#cite` needs.
/// So the store becomes a hayagriva sidecar and the citations become live.
#[test]
fn word_sources_become_a_hayagriva_sidecar_with_live_citations() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t xml:space="preserve">As shown </w:t></w:r>
      <w:fldSimple w:instr=" CITATION Kra06 \l 1033 ">
        <w:r><w:t>(Kramer &amp; Chen, 2006)</w:t></w:r>
      </w:fldSimple>
      <w:r><w:t>.</w:t></w:r>
    </w:p>
  </w:body></w:document>"#;

    const SOURCES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<b:Sources xmlns:b="http://schemas.openxmlformats.org/officeDocument/2006/bibliography">
  <b:Source>
    <b:Tag>Kra06</b:Tag><b:SourceType>Book</b:SourceType>
    <b:Author><b:Author><b:NameList>
      <b:Person><b:Last>Kramer</b:Last><b:First>James</b:First><b:Middle>D</b:Middle></b:Person>
      <b:Person><b:Last>Chen</b:Last><b:First>Jackey</b:First></b:Person>
    </b:NameList></b:Author></b:Author>
    <b:Title>How to Write Bibliographies</b:Title>
    <b:Year>2006</b:Year><b:City>Chicago</b:City>
    <b:Publisher>Adventure Works Press</b:Publisher>
  </b:Source>
</b:Sources>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_xml("customXml/item1.xml", "application/xml", SOURCES_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    let src = &result.source;
    // The citation is live, not Word's frozen "(Kramer & Chen, 2006)".
    assert!(src.contains("#cite(<Kra06>)"), "citation not made live:\n{src}");
    assert!(src.contains("#bibliography(\"bibliography.yml\")"), "no bibliography:\n{src}");

    // ...and the sidecar it names rides out as an asset, or the emitted
    // source would not compile.
    let (path, bytes) = result
        .assets
        .iter()
        .find(|(p, _)| p.to_string_lossy() == "bibliography.yml")
        .expect("sidecar not emitted");
    assert_eq!(path.to_string_lossy(), "bibliography.yml");
    let yaml = String::from_utf8(bytes.clone()).expect("sidecar should be utf-8");
    assert!(yaml.contains("\nKra06:\n"), "{yaml}");
    assert!(yaml.contains("  type: book\n"), "{yaml}");
    assert!(yaml.contains("    - \"Kramer, James D\"\n"), "{yaml}");
    assert!(yaml.contains("    name: \"Adventure Works Press\"\n"), "{yaml}");
}

/// A `CITATION` naming a tag with no matching `b:Source` must keep Word's
/// cached text: a `#cite` pointing at nothing fails the whole compile, which
/// is a far worse outcome than a citation that no longer updates.
#[test]
fn a_citation_with_no_matching_source_keeps_its_cached_text() {
    const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:fldSimple w:instr=" CITATION Ghost99 \l 1033 ">
        <w:r><w:t>(Ghost, 1999)</w:t></w:r>
      </w:fldSimple>
    </w:p>
  </w:body></w:document>"#;

    let mut package = Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", DOCUMENT_XML.into());
    package.add_relationships("word/document.xml", &Rels::new()).unwrap();
    let bytes = package.finish(&Rels::new()).unwrap();

    let result = import_docx(&bytes).expect("import should succeed");
    assert!(result.source.contains("(Ghost, 1999)"), "cached text lost:\n{}", result.source);
    assert!(!result.source.contains("#cite("), "cited a source that does not exist");
    // No citation resolved, so no bibliography and no sidecar to go with it.
    assert!(!result.source.contains("#bibliography"), "emitted an empty bibliography");
    assert!(result.assets.is_empty(), "wrote a sidecar nothing refers to");
}

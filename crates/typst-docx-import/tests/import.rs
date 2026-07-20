//! End-to-end integration test for the DOCX → Typst importer: builds a small
//! but realistic Word document in memory (via the OPC writer the exporter also
//! uses, so the reader gets a genuinely well-formed package) and asserts the
//! emitted Typst source is idiomatic — headings as `=`, emphasis as
//! `*`/`_`, and lists as `-`/`+`.

use typst_docx_import::{import_docx, import_docx_with, ImportOptions, Tier};
use typst_ooxml_core::opc::{Package, PackageOptions, Rels};

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

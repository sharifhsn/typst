//! Builds `word/styles.xml`: docDefaults + the mandatory built-in styles +
//! `Heading1..HeadingN`.

use typst_library::model::DocumentInfo;

use crate::props;
use crate::xml::{self, XmlWriter};

/// Builds the `styles.xml` part.
pub fn build(
    info: &DocumentInfo,
    defaults: &crate::dom::TextDefaults,
    max_heading_level: u8,
    pretty: bool,
) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open(xml::W_STYLES)
        .attr(
            "xmlns:w",
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        )
        .start_children();

    // docDefaults: the document's root font / size / colour / language, which the
    // whole body inherits (each run only overrides what differs). Editing the
    // `Normal` style or the theme font in Word then restyles the document.
    let _ = info;
    w.open("w:docDefaults").start_children();
    w.open("w:rPrDefault").start_children();
    w.open(xml::W_RPR).start_children();
    if let Some(font) = &defaults.font {
        w.open(xml::W_RFONTS)
            .attr("w:ascii", font)
            .attr("w:hAnsi", font)
            .attr("w:cs", font)
            .attr("w:eastAsia", font)
            .empty();
    }
    w.open(xml::W_SZ).attr(xml::W_VAL, &defaults.size_half_pt.to_string()).empty();
    w.open(xml::W_SZCS).attr(xml::W_VAL, &defaults.size_half_pt.to_string()).empty();
    if let Some(c) = defaults.color {
        w.open("w:color").attr(xml::W_VAL, &props::hex(c)).empty();
    }
    if let Some(lang) = &defaults.lang {
        w.open("w:lang").attr(xml::W_VAL, lang).empty();
    }
    w.close(); // rPr
    w.close(); // rPrDefault
    w.close(); // docDefaults

    // The standard Word `latentStyles` block: it declares the visibility, sort
    // priority and quick-format flags of the ~370 built-in styles, so the Styles
    // gallery and pane behave exactly as in a document Word itself produced (the
    // recommended styles show, the obscure ones stay hidden). It is the same fixed
    // block every Word document carries.
    w.raw(include_str!("latent_styles.xml"));

    // Normal (default paragraph style).
    style(&mut w, "Normal", "Normal", None, true, false);

    // Heading styles.
    for level in 1..=max_heading_level.max(1) {
        let id = format!("Heading{level}");
        let name = format!("heading {level}");
        w.open("w:style")
            .attr("w:type", "paragraph")
            .attr("w:styleId", &id)
            .start_children();
        w.open("w:name").attr(xml::W_VAL, &name).empty();
        w.open("w:basedOn").attr(xml::W_VAL, "Normal").empty();
        w.open("w:next").attr(xml::W_VAL, "Normal").empty();
        w.open(xml::W_PPR).start_children();
        w.leaf("w:keepNext");
        w.open("w:outlineLvl").attr(xml::W_VAL, &(level - 1).to_string()).empty();
        w.close();
        w.open(xml::W_RPR).start_children();
        w.leaf(xml::W_B);
        w.close();
        w.close(); // style
    }

    // ListParagraph.
    style(&mut w, "ListParagraph", "List Paragraph", Some("Normal"), false, false);
    // Caption.
    style(&mut w, "Caption", "caption", Some("Normal"), false, false);
    // Quote.
    style(&mut w, "Quote", "Quote", Some("Normal"), false, false);
    // FootnoteText (paragraph) + FootnoteReference (character).
    style(&mut w, "FootnoteText", "footnote text", Some("Normal"), false, false);
    char_style(&mut w, "FootnoteReference", "footnote reference", true);
    // Hyperlink (character) — blue + single underline, Word's default so links
    // actually look like links (the run suppresses its own default-black colour
    // for this style; an explicitly coloured link still overrides).
    w.open("w:style")
        .attr("w:type", "character")
        .attr("w:styleId", "Hyperlink")
        .start_children();
    w.open("w:name").attr(xml::W_VAL, "Hyperlink").empty();
    w.open(xml::W_RPR).start_children();
    w.open("w:color").attr(xml::W_VAL, "0563C1").empty();
    w.open("w:u").attr(xml::W_VAL, "single").empty();
    w.close(); // w:rPr
    w.close(); // w:style
    // TOC1..9.
    for level in 1..=9 {
        let id = format!("TOC{level}");
        let name = format!("toc {level}");
        style(&mut w, &id, &name, Some("Normal"), false, false);
    }
    // Bibliography.
    style(&mut w, "Bibliography", "Bibliography", Some("Normal"), false, false);

    w.close(); // w:styles
    w.finish()
}

/// Emits a simple paragraph style.
fn style(
    w: &mut XmlWriter,
    id: &str,
    name: &str,
    based_on: Option<&str>,
    default: bool,
    _heading: bool,
) {
    let s = w.open("w:style").attr("w:type", "paragraph").attr("w:styleId", id);
    if default {
        s.attr("w:default", "1");
    }
    w.start_children();
    w.open("w:name").attr(xml::W_VAL, name).empty();
    if let Some(b) = based_on {
        w.open("w:basedOn").attr(xml::W_VAL, b).empty();
    }
    w.close();
}

/// Emits a simple character style.
fn char_style(w: &mut XmlWriter, id: &str, name: &str, vert_super: bool) {
    w.open("w:style")
        .attr("w:type", "character")
        .attr("w:styleId", id)
        .start_children();
    w.open("w:name").attr(xml::W_VAL, name).empty();
    if vert_super {
        w.open(xml::W_RPR).start_children();
        w.open(xml::W_VERTALIGN).attr(xml::W_VAL, "superscript").empty();
        w.close();
    }
    w.close();
}

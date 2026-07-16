//! Builds `word/styles.xml`: docDefaults + the mandatory built-in styles +
//! `Heading1..HeadingN`.

use typst_library::model::DocumentInfo;

use crate::dom::{HeadingStyle, RunProps, Spacing, TextDefaults};
use crate::props;
use crate::xml::{self, XmlWriter};
use typst_ooxml_core::ns;

/// Builds the `styles.xml` part.
pub fn build(
    info: &DocumentInfo,
    defaults: &TextDefaults,
    heading_styles: &[HeadingStyle],
    max_heading_level: u8,
    pretty: bool,
) -> String {
    let mut w = XmlWriter::new(pretty);
    w.open(xml::W_STYLES).attr("xmlns:w", ns::W).start_children();

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
    w.open(xml::W_SZ)
        .attr(xml::W_VAL, &defaults.size_half_pt.to_string())
        .empty();
    w.open(xml::W_SZCS)
        .attr(xml::W_VAL, &defaults.size_half_pt.to_string())
        .empty();
    if let Some(c) = defaults.color {
        w.open("w:color").attr(xml::W_VAL, &props::hex(c)).empty();
    }
    if let Some(lang) = &defaults.lang {
        props::write_language(&mut w, "w:lang", lang);
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

    // Normal (default paragraph style). Repeat the document defaults here so
    // editing Normal in Word restyles the body even when the consumer ignores
    // `docDefaults`.
    normal_style(&mut w, defaults);

    // The three implicit defaults every Word document defines: the default
    // character style (base of all character styles), the default table style
    // (base of every table) and the default list style.
    w.raw(
        r#"<w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont">"#,
    );
    w.raw(r#"<w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/><w:unhideWhenUsed/></w:style>"#);
    w.raw(r#"<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style>"#);
    w.raw(r#"<w:style w:type="numbering" w:default="1" w:styleId="NoList"><w:name w:val="No List"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/></w:style>"#);

    // Heading styles, each paired with a linked character style
    // (`HeadingNChar`) exactly as Word writes them — so applying heading
    // formatting to a *span* works and the Styles pane shows the same linked
    // pair a Word-authored document carries.
    for level in 1..=max_heading_level.max(1) {
        let id = format!("Heading{level}");
        let char_id = format!("Heading{level}Char");
        let name = format!("heading {level}");
        let style = heading_styles.iter().find(|style| style.level == level);
        let fallback;
        let rpr = if let Some(style) = style {
            &style.rpr
        } else {
            fallback = RunProps { bold: true, ..RunProps::default() };
            &fallback
        };
        w.open("w:style")
            .attr("w:type", "paragraph")
            .attr("w:styleId", &id)
            .start_children();
        w.open("w:name").attr(xml::W_VAL, &name).empty();
        w.open("w:basedOn").attr(xml::W_VAL, "Normal").empty();
        w.open("w:next").attr(xml::W_VAL, "Normal").empty();
        w.open("w:link").attr(xml::W_VAL, &char_id).empty();
        w.open(xml::W_PPR).start_children();
        w.leaf("w:keepNext");
        w.open("w:outlineLvl")
            .attr(xml::W_VAL, &(level - 1).to_string())
            .empty();
        if let Some(spacing) = style.and_then(|style| style.spacing.as_ref()) {
            write_spacing(&mut w, spacing);
        }
        w.close();
        rpr.write_rpr(&mut w);
        w.close(); // style

        // The linked character style carries the same run formatting.
        w.open("w:style")
            .attr("w:type", "character")
            .attr("w:styleId", &char_id)
            .start_children();
        w.open("w:name")
            .attr(xml::W_VAL, &format!("Heading {level} Char"))
            .empty();
        w.open("w:basedOn").attr(xml::W_VAL, "DefaultParagraphFont").empty();
        w.open("w:link").attr(xml::W_VAL, &id).empty();
        rpr.write_rpr(&mut w);
        w.close(); // style
    }

    // Define TOCHeading explicitly instead of relying on a consumer's latent
    // built-in style. It copies Heading 1's resolved visual properties without
    // inheriting its outline level, so a refreshed Word TOC cannot list its own
    // title as an entry.
    let toc_heading = heading_styles.iter().find(|style| style.level == 1);
    w.open("w:style")
        .attr("w:type", "paragraph")
        .attr("w:styleId", "TOCHeading")
        .start_children();
    w.open("w:name").attr(xml::W_VAL, "TOC Heading").empty();
    w.open("w:basedOn").attr(xml::W_VAL, "Normal").empty();
    w.open("w:next").attr(xml::W_VAL, "Normal").empty();
    w.open("w:uiPriority").attr(xml::W_VAL, "39").empty();
    w.open("w:unhideWhenUsed").empty();
    w.open("w:qFormat").empty();
    if let Some(spacing) = toc_heading.and_then(|style| style.spacing.as_ref()) {
        w.open(xml::W_PPR).start_children();
        w.leaf("w:keepNext");
        write_spacing(&mut w, spacing);
        w.close();
    }
    if let Some(style) = toc_heading {
        style.rpr.write_rpr(&mut w);
    } else {
        RunProps {
            bold: true,
            size_half_pt: Some(28),
            ..RunProps::default()
        }
        .write_rpr(&mut w);
    }
    w.close();

    // The standard gallery styles Word always offers — Title/Subtitle (paired
    // with their linked character styles), the Strong/Emphasis character styles
    // (the bold/italic toggles in the ribbon), and the Table Grid table style.
    // We render with direct formatting, but defining these makes the same
    // built-in styles available when the document is edited in Word, matching a
    // Word-authored package.
    w.raw(r#"<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:link w:val="TitleChar"/><w:uiPriority w:val="10"/><w:qFormat/><w:pPr><w:spacing w:after="0"/></w:pPr><w:rPr><w:sz w:val="56"/><w:szCs w:val="56"/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="TitleChar"><w:name w:val="Title Char"/><w:basedOn w:val="DefaultParagraphFont"/><w:link w:val="Title"/><w:uiPriority w:val="10"/><w:rPr><w:sz w:val="56"/><w:szCs w:val="56"/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:link w:val="SubtitleChar"/><w:uiPriority w:val="11"/><w:qFormat/><w:rPr><w:i/><w:iCs/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="SubtitleChar"><w:name w:val="Subtitle Char"/><w:basedOn w:val="DefaultParagraphFont"/><w:link w:val="Subtitle"/><w:uiPriority w:val="11"/><w:rPr><w:i/><w:iCs/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="22"/><w:qFormat/><w:rPr><w:b/><w:bCs/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="Emphasis"><w:name w:val="Emphasis"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="20"/><w:qFormat/><w:rPr><w:i/><w:iCs/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/><w:uiPriority w:val="39"/><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:right w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders></w:tblPr></w:style>"#);

    // Header / Footer paragraph styles (+ their linked character styles) — the
    // running-head/foot styles every Word document defines and that header/footer
    // paragraphs use.
    w.raw(r#"<w:style w:type="paragraph" w:styleId="Header"><w:name w:val="header"/><w:basedOn w:val="Normal"/><w:link w:val="HeaderChar"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:pPr><w:tabs><w:tab w:val="center" w:pos="4680"/><w:tab w:val="right" w:pos="9360"/></w:tabs></w:pPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="HeaderChar"><w:name w:val="Header Char"/><w:basedOn w:val="DefaultParagraphFont"/><w:link w:val="Header"/><w:uiPriority w:val="99"/></w:style>"#);
    w.raw(r#"<w:style w:type="paragraph" w:styleId="Footer"><w:name w:val="footer"/><w:basedOn w:val="Normal"/><w:link w:val="FooterChar"/><w:uiPriority w:val="99"/><w:unhideWhenUsed/><w:pPr><w:tabs><w:tab w:val="center" w:pos="4680"/><w:tab w:val="right" w:pos="9360"/></w:tabs></w:pPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="FooterChar"><w:name w:val="Footer Char"/><w:basedOn w:val="DefaultParagraphFont"/><w:link w:val="Footer"/><w:uiPriority w:val="99"/></w:style>"#);

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
    // FollowedHyperlink + PageNumber — the other two common built-in character
    // styles Word offers (a visited link, a page-number field); defined so they
    // are available when editing, matching a Word-authored package.
    w.raw(r#"<w:style w:type="character" w:styleId="FollowedHyperlink"><w:name w:val="FollowedHyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/><w:rPr><w:color w:val="954F72"/><w:u w:val="single"/></w:rPr></w:style>"#);
    w.raw(r#"<w:style w:type="character" w:styleId="PageNumber"><w:name w:val="page number"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:semiHidden/><w:unhideWhenUsed/></w:style>"#);
    // TOC1..9. Word's TOC is a hierarchy, not nine aliases for Normal: indent
    // each nested level by one em at the 10pt document default, emphasize the
    // two structural levels, and give top-level groups breathing room. Entry
    // paragraphs carry their own right-aligned leader tab because its position
    // depends on section geometry.
    for level in 1..=9 {
        let id = format!("TOC{level}");
        let name = format!("toc {level}");
        w.open("w:style")
            .attr("w:type", "paragraph")
            .attr("w:styleId", &id)
            .start_children();
        w.open("w:name").attr(xml::W_VAL, &name).empty();
        w.open("w:basedOn").attr(xml::W_VAL, "Normal").empty();
        w.open("w:uiPriority").attr(xml::W_VAL, "39").empty();
        w.open("w:unhideWhenUsed").empty();
        w.open(xml::W_PPR).start_children();
        if level == 1 {
            w.open(xml::W_SPACING).attr("w:before", "400").empty();
        } else {
            w.open("w:ind")
                .attr("w:left", &((level - 1) * 200).to_string())
                .empty();
        }
        w.close();
        if level <= 2 {
            w.open(xml::W_RPR).start_children();
            w.open("w:b").empty();
            w.open("w:bCs").empty();
            w.close();
        }
        w.close();
    }
    // Bibliography.
    style(&mut w, "Bibliography", "Bibliography", Some("Normal"), false, false);

    w.close(); // w:styles
    w.finish()
}

/// Emits the default paragraph style with the document's root run properties.
fn normal_style(w: &mut XmlWriter, defaults: &TextDefaults) {
    w.open("w:style")
        .attr("w:type", "paragraph")
        .attr("w:styleId", "Normal")
        .attr("w:default", "1")
        .start_children();
    w.open("w:name").attr(xml::W_VAL, "Normal").empty();
    defaults_run_props(defaults).write_rpr(w);
    w.close();
}

fn defaults_run_props(defaults: &TextDefaults) -> RunProps {
    RunProps {
        font: defaults.font.clone(),
        color: defaults.color,
        size_half_pt: Some(defaults.size_half_pt),
        lang: defaults.lang.clone(),
        ..RunProps::default()
    }
}

fn write_spacing(w: &mut XmlWriter, sp: &Spacing) {
    w.open(xml::W_SPACING);
    if let Some(before) = sp.before {
        w.attr("w:before", &before.to_string());
    }
    if let Some(after) = sp.after {
        w.attr("w:after", &after.to_string());
    }
    if let Some(line) = sp.line {
        w.attr("w:line", &line.to_string());
        let rule = if sp.line_rule_auto {
            "auto"
        } else if sp.line_rule_at_least {
            "atLeast"
        } else {
            "exact"
        };
        w.attr("w:lineRule", rule);
    }
    w.empty();
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

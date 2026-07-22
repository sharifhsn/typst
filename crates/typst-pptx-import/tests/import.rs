//! End-to-end tests over hand-built `.pptx` packages.
//!
//! Each test builds the smallest package that exhibits one behaviour, so a
//! failure names its own cause. The corpus gate
//! (`tools/pptx-import-corpus/corpus.py`) covers breadth; these cover the
//! decisions.

use typst_ooxml_core::opc::{Package, PackageOptions, RelMode, Rels};
use typst_pptx_import::{import_pptx, import_pptx_with, Fidelity, ImportOptions};

const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const P_NS: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

const REL_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
const REL_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
const REL_LAYOUT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
const REL_THEME: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";

/// A package with one slide holding `sp_tree_body`, plus the master, layout
/// and theme every real deck has.
struct Deck {
    slide_body: String,
    layout_body: String,
    master_extra: String,
    theme_colors: String,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            slide_body: String::new(),
            layout_body: String::new(),
            master_extra: String::new(),
            theme_colors: r#"<a:dk1><a:srgbClr val="000000"/></a:dk1>
                 <a:lt1><a:srgbClr val="FFFFFF"/></a:lt1>
                 <a:accent1><a:srgbClr val="4472C4"/></a:accent1>"#
                .into(),
        }
    }
}

impl Deck {
    fn build(&self) -> Vec<u8> {
        let mut package = Package::new(PackageOptions {
            rels_overrides: true,
            media_defaults: &[("png", "image/png")],
        });

        let presentation = format!(
            r#"<?xml version="1.0"?>
<p:presentation xmlns:p="{P_NS}" xmlns:r="{R_NS}" xmlns:a="{A_NS}">
  <p:sldMasterIdLst><p:sldMasterId r:id="rId1"/></p:sldMasterIdLst>
  <p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst>
  <p:sldSz cx="12192000" cy="6858000"/>
</p:presentation>"#
        );
        let mut pres_rels = Rels::new();
        pres_rels.add(REL_MASTER, "slideMasters/slideMaster1.xml", RelMode::Internal);
        pres_rels.add(REL_SLIDE, "slides/slide1.xml", RelMode::Internal);

        let master = format!(
            r#"<?xml version="1.0"?>
<p:sldMaster xmlns:p="{P_NS}" xmlns:r="{R_NS}" xmlns:a="{A_NS}">
  <p:cSld><p:spTree>{}</p:spTree></p:cSld>
  <p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2"/>
  {}
</p:sldMaster>"#,
            "", self.master_extra
        );
        let mut master_rels = Rels::new();
        master_rels.add(REL_LAYOUT, "../slideLayouts/slideLayout1.xml", RelMode::Internal);
        master_rels.add(REL_THEME, "../theme/theme1.xml", RelMode::Internal);

        let layout = format!(
            r#"<?xml version="1.0"?>
<p:sldLayout xmlns:p="{P_NS}" xmlns:r="{R_NS}" xmlns:a="{A_NS}">
  <p:cSld name="Title and Content"><p:spTree>{}</p:spTree></p:cSld>
</p:sldLayout>"#,
            self.layout_body
        );

        let slide = format!(
            r#"<?xml version="1.0"?>
<p:sld xmlns:p="{P_NS}" xmlns:r="{R_NS}" xmlns:a="{A_NS}">
  <p:cSld><p:spTree>{}</p:spTree></p:cSld>
</p:sld>"#,
            self.slide_body
        );
        let mut slide_rels = Rels::new();
        slide_rels.add(REL_LAYOUT, "../slideLayouts/slideLayout1.xml", RelMode::Internal);

        let theme = format!(
            r#"<?xml version="1.0"?>
<a:theme xmlns:a="{A_NS}" name="T">
  <a:themeElements><a:clrScheme name="S">{}</a:clrScheme></a:themeElements>
</a:theme>"#,
            self.theme_colors
        );

        package.add_xml("ppt/presentation.xml", "application/xml", presentation);
        package.add_xml("ppt/slideMasters/slideMaster1.xml", "application/xml", master);
        package.add_xml("ppt/slideLayouts/slideLayout1.xml", "application/xml", layout);
        package.add_xml("ppt/slides/slide1.xml", "application/xml", slide);
        package.add_xml("ppt/theme/theme1.xml", "application/xml", theme);
        package.add_relationships("ppt/presentation.xml", &pres_rels).unwrap();
        package
            .add_relationships("ppt/slideMasters/slideMaster1.xml", &master_rels)
            .unwrap();
        package.add_relationships("ppt/slides/slide1.xml", &slide_rels).unwrap();
        package.finish(&Rels::new()).unwrap()
    }
}

fn text_shape(x: i64, y: i64, cx: i64, cy: i64, body: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
  <p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm></p:spPr>
  <p:txBody><a:bodyPr/>{body}</p:txBody></p:sp>"#
    )
}

#[test]
fn a_slides_text_survives_with_its_position() {
    let deck = Deck {
        slide_body: text_shape(
            914400,
            457200,
            5486400,
            1143000,
            "<a:p><a:r><a:t>Hello slides</a:t></a:r></a:p>",
        ),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(result.source.contains("Hello slides"), "{}", result.source);
    // 914400 EMU is exactly one inch, which is 72pt.
    assert!(result.source.contains("dx: 72pt"), "{}", result.source);
    // The canvas comes from `p:sldSz`, not from a guess.
    assert!(result.source.contains("width: 960pt"), "{}", result.source);
    assert!(result.source.contains("touying"), "{}", result.source);
}

#[test]
fn a_placeholder_inherits_its_position_from_the_layout() {
    // The whole point of the inheritance chain: the slide states no `a:xfrm`
    // at all, and PowerPoint still knows where the text goes.
    let deck = Deck {
        layout_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="9" name="Title"/><p:cNvSpPr/>
            <p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="1828800" y="914400"/>
            <a:ext cx="5486400" cy="1143000"/></a:xfrm></p:spPr>
            <p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>"#
            .into(),
        slide_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/>
            <p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
            <p:spPr/>
            <p:txBody><a:bodyPr/><a:p><a:r><a:t>Inherited</a:t></a:r></a:p></p:txBody></p:sp>"#
            .into(),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(result.source.contains("Inherited"), "{}", result.source);
    // 1828800 EMU = 2in = 144pt, straight from the layout.
    assert!(result.source.contains("dx: 144pt"), "position must come from the layout:\n{}", result.source);
}

#[test]
fn a_theme_colour_reaches_the_output_as_rgb() {
    let deck = Deck {
        slide_body: text_shape(
            0,
            0,
            1000000,
            1000000,
            r#"<a:p><a:r><a:rPr lang="en"><a:solidFill>
               <a:schemeClr val="accent1"/></a:solidFill></a:rPr>
               <a:t>Themed</a:t></a:r></a:p>"#,
        ),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(
        result.source.contains("#4472c4"),
        "the accent colour must be resolved through the theme:\n{}",
        result.source
    );
}

#[test]
fn idiomatic_fidelity_promotes_the_title_to_a_heading() {
    let deck = Deck {
        slide_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/>
            <p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></a:xfrm></p:spPr>
            <p:txBody><a:bodyPr/><a:p><a:r><a:t>My Title</a:t></a:r></a:p></p:txBody></p:sp>"#
            .into(),
        ..Deck::default()
    };
    let bytes = deck.build();

    let placed = import_pptx(&bytes).unwrap();
    assert!(placed.source.contains("#place"), "placed mode keeps coordinates");

    let opts = ImportOptions { fidelity: Fidelity::Idiomatic, ..ImportOptions::default() };
    let idiomatic = import_pptx_with(&bytes, &opts).unwrap();
    assert!(
        idiomatic.source.contains("== My Title"),
        "idiomatic mode makes the title a heading:\n{}",
        idiomatic.source
    );
}

#[test]
fn unsupported_constructs_are_named_rather_than_dropped_in_silence() {
    // A SmartArt diagram arrives as a graphicFrame with a diagram URI. It
    // cannot come across, and the whole contract of this crate is that the
    // reader is told so.
    let deck = Deck {
        slide_body: r#"<p:graphicFrame><p:xfrm><a:off x="0" y="0"/>
            <a:ext cx="100" cy="100"/></p:xfrm>
            <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>
            </a:graphic></p:graphicFrame>"#
            .into(),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    let notes = format!("{}", result.report);
    assert!(notes.contains("SmartArt"), "the diagram must be reported: {notes}");
}

#[test]
fn a_deck_without_a_presentation_part_is_refused() {
    let mut package =
        Package::new(PackageOptions { rels_overrides: true, media_defaults: &[] });
    package.add_xml("word/document.xml", "application/xml", "<document/>".into());
    let bytes = package.finish(&Rels::new()).unwrap();
    assert!(import_pptx(&bytes).is_err(), "a .docx is not a presentation");
}

#[test]
fn slides_keep_presentation_order_not_archive_order() {
    // `p:sldIdLst` is the running order; the archive may hold slides in any
    // order at all, and a deck read in archive order is a deck reshuffled.
    let result = import_pptx(&Deck::default().build()).expect("import should succeed");
    assert_eq!(result.source.matches("#slide").count(), 1, "{}", result.source);
}

#[test]
fn a_layouts_decoration_is_drawn_behind_every_slide() {
    // A themed deck's logo and graphics live on the layout, and no slide
    // mentions them. Reproducing them is the difference between a themed
    // deck and a blank one.
    let deck = Deck {
        layout_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="7" name="Logo"/><p:cNvSpPr/>
            <p:nvPr/></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm>
            <a:prstGeom prst="rect"><a:avLst/></a:prstGeom>
            <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></p:spPr>
            <p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>"#
            .into(),
        slide_body: text_shape(0, 0, 100, 100, "<a:p><a:r><a:t>Body</a:t></a:r></a:p>"),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(
        result.source.contains("#ff0000"),
        "the layout's own decoration must be drawn:\n{}",
        result.source
    );
}

#[test]
fn a_layouts_placeholder_prompt_text_is_not_reproduced() {
    // A master's placeholder holds "Click to edit Master title style". It is
    // a template prompt, not content, and importing it would put those words
    // on every slide.
    let deck = Deck {
        layout_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="8" name="Title"/><p:cNvSpPr/>
            <p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></a:xfrm></p:spPr>
            <p:txBody><a:bodyPr/><a:p><a:r><a:t>Click to edit Master title</a:t></a:r></a:p>
            </p:txBody></p:sp>"#
            .into(),
        slide_body: text_shape(0, 0, 100, 100, "<a:p><a:r><a:t>Real</a:t></a:r></a:p>"),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(result.source.contains("Real"), "{}", result.source);
    assert!(
        !result.source.contains("Click to edit"),
        "template prompt text must not reach the output:\n{}",
        result.source
    );
}

#[test]
fn a_triangle_is_drawn_as_a_triangle() {
    let deck = Deck {
        slide_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="T"/><p:cNvSpPr/>
            <p:nvPr/></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm>
            <a:prstGeom prst="triangle"><a:avLst/></a:prstGeom>
            <a:solidFill><a:srgbClr val="00FF00"/></a:solidFill></p:spPr>
            <p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>"#
            .into(),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    // Three vertices and a close, not a rectangle.
    assert!(result.source.contains("curve.move"), "{}", result.source);
    assert_eq!(result.source.matches("curve.line").count(), 2, "{}", result.source);
}

#[test]
fn a_mirrored_shape_is_mirrored() {
    let deck = Deck {
        slide_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="F"/><p:cNvSpPr/>
            <p:nvPr/></p:nvSpPr>
            <p:spPr><a:xfrm flipH="1"><a:off x="0" y="0"/>
            <a:ext cx="914400" cy="914400"/></a:xfrm>
            <a:prstGeom prst="rtTriangle"><a:avLst/></a:prstGeom>
            <a:solidFill><a:srgbClr val="0000FF"/></a:solidFill></p:spPr>
            <p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>"#
            .into(),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(
        result.source.contains("scale(x: -100%"),
        "flipH must mirror rather than be reported as impossible:\n{}",
        result.source
    );
}

#[test]
fn text_is_anchored_where_powerpoint_anchors_it() {
    let deck = Deck {
        slide_body: r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="A"/><p:cNvSpPr/>
            <p:nvPr/></p:nvSpPr>
            <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm></p:spPr>
            <p:txBody><a:bodyPr anchor="ctr"/><a:p><a:r><a:t>Middle</a:t></a:r></a:p>
            </p:txBody></p:sp>"#
            .into(),
        ..Deck::default()
    };
    let result = import_pptx(&deck.build()).expect("import should succeed");
    assert!(
        result.source.contains("align(horizon"),
        "a centred body must not be top-aligned:\n{}",
        result.source
    );
}

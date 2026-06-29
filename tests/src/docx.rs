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
fn nested_full_enum_numbers_include_ancestry() {
    // `#set enum(full: true)` nested numbering must read `1.`, `1.1.`, `2.` — the
    // parent ancestry folded onto each item body.
    let p = parts("#set enum(full: true)\n+ one\n  + one-a\n+ two");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("1.1."), "nested full enum shows the parent path (1.1.)");
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
fn colored_math_carries_its_color() {
    // `#text(red)[$x$]` inside an equation must color the math run (a `w:rPr`
    // colour on the math `m:r`), not render black.
    let p = parts("$ y = #text(red)[x] + b $");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("<w:color w:val=\"FF4136\""), "the red math run carries its color");
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
    let p = parts(
        "#set document(title: \"T\", author: \"Ada Lovelace\")\n#outline()\n\n= H",
    );
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
fn header_link_relationship_lives_in_the_header_part_rels() {
    // A link/image in a header references a relationship by r:id; that id must
    // resolve against the header part's OWN .rels, not document.xml.rels, or Word
    // refuses to open the file.
    let p = parts(
        "#set page(header: [#link(\"https://example.com\")[site] head])\nBody.",
    );
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
fn hide_becomes_hidden_text() {
    // `#hide` content → `<w:vanish/>`: invisible in the page but present in the
    // document (searchable / screen-reader-readable), rather than dropped.
    let p = parts("Shown #hide[a secret] and more.");
    let doc = &p["word/document.xml"];
    assert!(doc.contains("w:vanish"), "#hide content becomes hidden text");
    assert!(doc.contains("a secret"), "the hidden text is preserved");
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
    assert!(hl.contains("<w:bdr") && hl.contains("<w:shd "), "with the box's shading + border");
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
    assert!(doc.contains("First callout") && doc.contains("Second callout"), "text flows");
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
    assert!(inline.contains_key("word/footnotes.xml"), "and the footnote body is emitted");
    assert_all_wellformed(&inline);

    let block = parts("#rect(fill: green, inset: 6pt)[Callout with a #footnote[fn] here.]");
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

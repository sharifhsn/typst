//! Outline mapper — lowers `OutlineElem` to a native Word table-of-contents
//! complex field. Implemented per `fields_links_images_validation.md §1.4` +
//! `§outline`.
//!
//! Strategy: Word builds a real, navigable TOC from a `{ TOC ... }` field that
//! it recomputes from the document's heading styles (which the heading mapper
//! emits as `Heading1..9`). We therefore emit:
//!
//!   1. an optional title paragraph (styled `TOCHeading`), and
//!   2. a paragraph holding a single complex field whose instruction is
//!      `TOC \o "1-N" \h \z \u` (headings) or `TOC \h \z \c "Figure"` /
//!      `\c "Table"` (list-of-figures / list-of-tables).
//!
//! The field carries `w:dirty` so Word refreshes it on open; combined with the
//! integration phase's `<w:updateFields w:val="true"/>` in `settings.xml`
//! (driven by `ctx.mark_field()`), the user gets a fully populated TOC without
//! manual intervention. The cached field result is an italic "update field"
//! placeholder, matching what every real-world emitter ships.

use ecow::{EcoString, eco_format};
use typst_library::diag::SourceResult;
use typst_library::foundations::{Element, Packed, Repr, Selector, StyleChain};
use typst_library::model::{HeadingElem, OutlineElem};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Field, Para, ParaChild, ParaProps, Run, RunProps, TabAlign, TabLeader,
    TabStop, Toc, TocFigure, TocHeading,
};

/// The default outline depth used for the `\o "1-N"` switch when the outline
/// does not constrain `depth`. Word's "Automatic Table" uses `1-3`; we match it.
const DEFAULT_TOC_DEPTH: usize = 3;

pub fn outline(
    elem: &Packed<OutlineElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let mut blocks = Vec::new();

    // 1. Title paragraph (styled `TOCHeading`), if the outline has a title.
    //
    // We deliberately do NOT route the title through the heading mapper: a real
    // `Heading1` paragraph would itself be picked up by the `TOC` field and
    // appear as an entry in its own table of contents. `TOCHeading` is the
    // built-in Word style for this exact purpose. (`§1.4`.)
    // `realize_title` resolves the `auto`/`none`/custom title (and the localized
    // default name) into an `Option<Content>` wrapping a `HeadingElem`; we pull
    // out its body so the title becomes a `TOCHeading` paragraph rather than a
    // real heading that would recurse into its own TOC.
    if let Some(title_body) = elem
        .realize_title(styles)
        .as_ref()
        .and_then(|c| c.to_packed::<HeadingElem>())
        .map(|h| h.body.clone())
    {
        let runs = ctx.inline_runs(&title_body, styles, RunProps::default())?;
        if !runs.is_empty() {
            blocks.push(Block::Para(Para {
                props: ParaProps {
                    style: Some("TOCHeading".into()),
                    keep_next: true,
                    ..ParaProps::default()
                },
                content: runs.into_iter().map(ParaChild::Run).collect(),
            }));
        }
    }

    // 2. The TOC field. Its entries are baked in as the cached result so the
    // table of contents shows without a manual field update — but the actual
    // entry paragraphs are filled in a post-conversion pass ([`fill_tocs`]),
    // once every heading/figure's real bookmark exists. A heading TOC carries the
    // depth to populate from; a list-of-figures/tables carries its caption
    // category. The field stays `dirty` so Word refreshes page numbers when able.
    let instr = toc_instruction(elem, styles);
    ctx.mark_field();

    let caption_category = toc_category(elem, styles);
    let depth = caption_category.is_none().then(|| toc_depth(elem, styles));
    // Right-tab position (page content width, in twips) for the dot leader.
    let tab_pos = (ctx.raster_width.to_pt() * 20.0) as i32;

    // Shown only when no entries are baked (a list whose figures had no captions,
    // or a document with no headings): an italic "update me" placeholder.
    let fallback = vec![Run::Text {
        props: RunProps { italic: true, ..RunProps::default() },
        text: "Right-click to update the table of contents.".into(),
    }];

    blocks.push(Block::Toc(Toc {
        instr,
        dirty: true,
        depth,
        caption_category,
        tab_pos,
        entries: Vec::new(),
        fallback,
    }));

    Ok(blocks)
}

/// Populates every [`Toc`] in the body. A heading TOC fills from `recorded` (the
/// headings emitted during conversion, each carrying its real bookmark), or —
/// when nothing was recorded (headings show-ruled / rasterized) — from
/// `fallback` (introspector-queried, plain text). A list of figures/tables fills
/// from `figures` of its caption category. Called once the whole body, across
/// all sections, has been converted.
pub(crate) fn fill_tocs(
    blocks: &mut [Block],
    recorded: &[TocHeading],
    fallback: &[TocHeading],
    figures: &[TocFigure],
) {
    let headings = if recorded.is_empty() { fallback } else { recorded };
    for block in blocks.iter_mut() {
        let Block::Toc(toc) = block else { continue };
        if let Some(depth) = toc.depth {
            toc.entries = headings
                .iter()
                .filter(|h| h.level <= depth)
                .map(|h| entry_para(h.level, &h.anchor, &h.text, toc.tab_pos))
                .collect();
        } else if let Some(category) = &toc.caption_category {
            toc.entries = figures
                .iter()
                .filter(|f| &f.category == category)
                .map(|f| entry_para(1, &f.anchor, &f.text, toc.tab_pos))
                .collect();
        }
    }
}

/// Builds one `TOC{level}` entry paragraph: the text as a hyperlink to its
/// bookmark (when it has one), a right tab with a dot leader, and a `PAGEREF`
/// field whose page number the consumer fills in.
fn entry_para(
    level: usize,
    anchor: &Option<EcoString>,
    text: &EcoString,
    tab_pos: i32,
) -> Para {
    let text_run = Run::Text { props: RunProps::default(), text: text.clone() };
    let mut content = Vec::new();
    match anchor {
        Some(name) => content.push(ParaChild::Hyperlink {
            rel: None,
            anchor: Some(name.clone()),
            runs: vec![text_run],
        }),
        None => content.push(ParaChild::Run(text_run)),
    }
    content.push(ParaChild::Run(Run::Tab));
    if let Some(name) = anchor {
        content.push(ParaChild::Run(Run::Field(Field {
            instr: eco_format!(" PAGEREF {name} \\h "),
            result: Vec::new(),
            dirty: false,
        })));
    }
    Para {
        props: ParaProps {
            style: Some(eco_format!("TOC{}", level.min(9))),
            tabs: vec![TabStop {
                val: TabAlign::End,
                leader: Some(TabLeader::Dot),
                pos: tab_pos,
            }],
            ..ParaProps::default()
        },
        content,
    }
}

/// The TOC depth (`\o "1-N"`): the outline's `depth`, or Word's default of 3,
/// clamped to the valid OOXML outline range.
fn toc_depth(elem: &Packed<OutlineElem>, styles: StyleChain) -> usize {
    elem.depth
        .get(styles)
        .map(|d| d.get())
        .unwrap_or(DEFAULT_TOC_DEPTH)
        .clamp(1, 9)
}

/// Builds the `instrText` for the TOC field (with the conventional leading and
/// trailing space). Headings produce an outline-level TOC; a `figure`/`table`
/// target produces a caption-category TOC via the `\c` switch.
fn toc_instruction(elem: &Packed<OutlineElem>, styles: StyleChain) -> EcoString {
    match toc_category(elem, styles) {
        // List of figures / tables: `\c "Figure"` builds from SEQ-captioned
        // entries of that category instead of from heading outline levels.
        // `\h` (hyperlinked entries) + `\z` (hide leader/page-number in Web
        // view) match Word's "Insert Table of Figures". (`§1.3`.)
        Some(category) => eco_format!(" TOC \\h \\z \\c \"{category}\" "),

        // Table of contents from heading styles:
        //   \o "1-N"  build from Heading 1..N outline levels
        //   \h        entries are hyperlinks
        //   \z        hide tab leader + page number in Web view
        //   \u        use the applied paragraph outline level
        // This is exactly what Word's "Automatic Table" inserts. (`§1.3`.)
        None => {
            let depth = toc_depth(elem, styles);
            eco_format!(" TOC \\o \"1-{depth}\" \\h \\z \\u ")
        }
    }
}

/// Determines the caption category for a list-of-figures/tables outline.
///
/// Returns `Some("Figure")` / `Some("Table")` (or another caption category)
/// when the outline targets figures, and `None` for a heading table of
/// contents (the default `target`). The category string is the SEQ identifier
/// Word matches against caption sequences.
fn toc_category(elem: &Packed<OutlineElem>, styles: StyleChain) -> Option<EcoString> {
    // Inspect the leaf element type the selector matches. The default target is
    // `heading` (→ a real TOC); anything else is treated as a caption list.
    let target = elem.target.get_cloned(styles).0;
    let element = leaf_element(&target)?;
    match element.name() {
        // Default heading TOC: no `\c` switch.
        "heading" => None,
        // Figure outlines. Distinguish a `figure.where(kind: table)` /
        // `kind: image` selector so the `\c` category matches Word's caption
        // label ("Table" / "Figure"). Falls back to "Figure".
        "figure" => Some(figure_category(&target)),
        // Any other locatable target: use a capitalized element name as the
        // SEQ category. This still yields a valid `\c` list even if Word finds
        // no matching captions.
        // INTEGRATION-NEEDED: caption categories for arbitrary `where`-selected
        // targets depend on how the figure/caption mappers name their SEQ
        // sequences; align this category string with that SEQ identifier so the
        // `\c` switch resolves. For headings/figures (the common cases) this is
        // already correct.
        name => Some(capitalize(name)),
    }
}

/// Walks a (possibly compound) selector to the element type it filters on.
fn leaf_element(selector: &Selector) -> Option<Element> {
    match selector {
        Selector::Elem(element, _) => Some(*element),
        // `figure.where(kind: table)` lowers to an `And`/`Elem` combination;
        // recurse into the first sub-selector that names an element.
        Selector::And(subs) | Selector::Or(subs) => subs.iter().find_map(leaf_element),
        Selector::Before { selector, .. }
        | Selector::After { selector, .. }
        | Selector::Within { selector, .. } => leaf_element(selector),
        _ => None,
    }
}

/// Derives the `\c` caption category for a figure target by inspecting the
/// `kind` field constraint on the selector, if any.
fn figure_category(selector: &Selector) -> EcoString {
    // `figure.where(kind: <elem>)` encodes the kind as a field constraint in
    // the `Selector::Elem` dictionary. We look for a constrained kind whose
    // value is the `table` (or `image`) element and map it to Word's caption
    // label. Without an explicit kind, the category is "Figure".
    if selector_targets_table(selector) { "Table".into() } else { "Figure".into() }
}

/// Whether the selector constrains figures to `kind: table`.
fn selector_targets_table(selector: &Selector) -> bool {
    match selector {
        Selector::Elem(_, Some(fields)) => fields.iter().any(|(_, value)| {
            // The kind value is a `FigureKind` wrapping an element; a table list
            // constrains it to `table`. Match on the value's string form, which
            // covers both the `table` element and a `"Table"` string kind.
            let s = value.repr();
            s.contains("table")
        }),
        Selector::And(subs) | Selector::Or(subs) => {
            subs.iter().any(selector_targets_table)
        }
        Selector::Before { selector, .. }
        | Selector::After { selector, .. }
        | Selector::Within { selector, .. } => selector_targets_table(selector),
        _ => false,
    }
}

/// ASCII-capitalizes the first character of an element name for use as a SEQ
/// caption category (e.g. `"figure"` → `"Figure"`).
fn capitalize(name: &str) -> EcoString {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(name.len());
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            out.into()
        }
        None => EcoString::new(),
    }
}

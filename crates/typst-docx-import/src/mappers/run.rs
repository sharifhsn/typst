//! The `run` mapper: Word runs (`w:r`), hyperlinks (`w:hyperlink`), and
//! fields (`w:fldSimple`/`w:fldChar` — see [`crate::mappers::field`]) → the
//! Typst IR's [`Inlines`]. A run's footnote/endnote references
//! ([`RunContent::NoteRef`]) resolve here too, via
//! [`crate::mappers::note::lower_note_ref`] — one more thing a run's content
//! can hold, alongside text/tabs/breaks/drawings/math/text boxes.

use typst_ooxml_core::units::{half_point_to_pt, twip_to_abs};

use crate::lower::{lower_items, parse_hex_color, LowerCtx};
use crate::mappers::{dml_shape, field, math, note, shape};
use crate::report::ImportReport;
use crate::resolve::styles::effective_run;
use crate::tdoc::{Inline, Inlines, Lang, Script, TextStyle, Underline};
use crate::wml::model::{BreakType, Paragraph, Run, RunContent, RunItem, RunProps};

/// Lower a whole paragraph's run sequence (runs, hyperlinks, fields) to
/// inlines.
pub(crate) fn lower_paragraph_inlines(p: &Paragraph, ctx: &mut LowerCtx) -> Inlines {
    lower_run_items(&p.runs, p.props.style_id.as_deref(), ctx)
}

/// Lower a sequence of run-level items ([`RunItem::Run`]/`Hyperlink`/`Field`)
/// to inlines. Shared by [`lower_paragraph_inlines`] (a paragraph's own runs)
/// and by [`crate::mappers::field::lower_field`] (a field's cached result,
/// and a hyperlink's content wraps back around to this same function too) —
/// all three positions can hold the same mix of runs, hyperlinks, and
/// (nested) fields.
pub(crate) fn lower_run_items(
    items: &[RunItem],
    para_style_id: Option<&str>,
    ctx: &mut LowerCtx,
) -> Inlines {
    let mut out = Vec::new();
    for run_item in items {
        match run_item {
            RunItem::Run(r) => out.extend(lower_run(r, para_style_id, ctx)),
            RunItem::Hyperlink { rel_id, anchor, runs } => {
                let inner = lower_run_items(runs, para_style_id, ctx);
                match rel_id {
                    Some(id) => match ctx.package.rels.get(id) {
                        Some(rel) => {
                            out.push(Inline::Link { dest: rel.target.clone(), body: inner })
                        }
                        None => {
                            ctx.report.approximate(
                                "hyperlink",
                                "relationship target not found; text kept unlinked",
                            );
                            out.extend(inner);
                        }
                    },
                    None => match anchor.as_ref().and_then(|a| ctx.package.bookmarks.get(a)) {
                        // An internal jump: Typst's `#link` takes a label just
                        // as happily as a URL, so the link survives intact.
                        Some(label) => {
                            out.push(Inline::LabelLink { label: label.clone(), body: inner })
                        }
                        None => {
                            if anchor.is_some() {
                                ctx.report.approximate(
                                    "internal hyperlink",
                                    "anchor has no matching bookmark; link dropped, text kept",
                                );
                            }
                            out.extend(inner);
                        }
                    },
                }
            }
            // A bookmark lowers to a label only if it survived collection —
            // `_GoBack` and duplicates are deliberately absent.
            RunItem::Bookmark(name) => {
                // Claimed rather than just looked up: a name that appears
                // twice in the document may only be emitted once, or Typst
                // rejects every reference to it as ambiguous.
                if let Some(label) = ctx.package.bookmarks.get(name).cloned()
                    && ctx.claim_label(&label)
                {
                    out.push(Inline::Label(label));
                }
            }
            RunItem::Field(f) => out.extend(field::lower_field(f, ctx)),
        }
    }
    out
}

/// Lower a single run to zero or more inlines (empty if it's hidden text
/// (`w:vanish`) or carries no visible content).
fn lower_run(r: &Run, para_style_id: Option<&str>, ctx: &mut LowerCtx) -> Inlines {
    let package = ctx.package;
    let eff = effective_run(&package.styles, para_style_id, &r.props);
    if eff.vanish == Some(true) {
        return Vec::new();
    }

    let mut content = Vec::new();
    for c in &r.content {
        match c {
            RunContent::Text(s) => content.push(Inline::Text(s.clone())),
            RunContent::Tab => content.push(Inline::Text("\t".into())),
            RunContent::Break(BreakType::Line) => content.push(Inline::Linebreak),
            // Page/column breaks are handled at the paragraph level.
            RunContent::Break(BreakType::Page | BreakType::Column) => {}
            // Images are handled at the paragraph level (block-level figures).
            RunContent::Drawing(_) => {}
            // Charts are handled at the paragraph level too (block-level,
            // like a drawing) — see `mappers::para`/`mappers::chart`.
            RunContent::Chart(_) => {}
            // A display equation reaching here is one that shares its
            // paragraph with other content, so it can only be set inline —
            // `mappers::para` intercepts the paragraphs that are *entirely* a
            // display equation before they ever get this far.
            RunContent::Math { xml, .. } => {
                content.push(math::omml_to_inline(xml, &mut *ctx.report))
            }
            // Furigana. Both halves are ordinary runs, so they lower through
            // the same path as any other inline content; the emitter supplies
            // the `ruby` helper Typst lacks. A ruby with no reading above it
            // is just text — unwrap it rather than pulling in the helper for
            // an annotation that isn't there.
            RunContent::Ruby { base, gloss } => {
                let base = lower_run_items(base, para_style_id, ctx);
                let gloss = lower_run_items(gloss, para_style_id, ctx);
                if gloss.is_empty() {
                    content.extend(base);
                } else {
                    content.push(Inline::Ruby { base, gloss });
                }
            }
            RunContent::NoteRef { endnote, id } => {
                if let Some(inline) = note::lower_note_ref(*endnote, *id, ctx) {
                    content.push(inline);
                }
            }
            // A Word shape/text box: lowered as ordinary body content
            // (`lower_items`, the same entry point a table cell or a
            // footnote's body goes through) and kept inline at the anchor
            // point, since that's the only place Typst can put it — see
            // `Inline::TextBox`'s doc comment for why the floating position
            // and size can't come along. Recorded once (per document, via
            // `ImportReport`'s own dedup) rather than per text box, the same
            // way a repeated unmapped field only reports once.
            RunContent::TextBox(items) => {
                // Same reasoning as a table cell's own body (see
                // `mappers::table::lower_cell`): a page/column break inside a
                // text box is meaningless and Typst rejects it outright.
                let was_in_container = ctx.enter_container();
                let blocks = lower_items(items, ctx);
                ctx.exit_container(was_in_container);
                content.push(Inline::TextBox(blocks));
                ctx.report.approximate(
                    "text box",
                    "floating position and size not preserved; content inlined at the \
                     anchor point",
                );
            }
            // WordArt: genuine document text, just with no Typst equivalent
            // for the curved/warped path it was drawn along — kept as plain
            // text rather than lost, with the styling loss reported once.
            RunContent::VmlText(s) => {
                content.push(Inline::Text(s.clone()));
                ctx.report.approximate(
                    "WordArt",
                    "curved/styled text path not reproduced; kept as plain text",
                );
            }
            // A native VML shape (`v:rect`/`v:oval`/`v:roundrect`/`v:line`) —
            // see `mappers::shape` for the `#rect`/`#circle`/`#ellipse`/
            // `#line` mapping and its own report notes (a position note on
            // success, a drop note for a `v:line` that can't be lowered).
            RunContent::VmlShape(vml_shape) => {
                if let Some(inline) = shape::lower_vml_shape(vml_shape, &mut *ctx.report) {
                    content.push(inline);
                }
            }
            // A VML shape with custom `v:path`/`v:formulas` geometry (or
            // anything else this importer found nothing extractable in) —
            // out of scope by design (see `mappers::shape`'s module doc for
            // why), recorded as a drop rather than silently vanishing.
            RunContent::VmlUnsupported => {
                ctx.report.drop(
                    "VML shape",
                    "custom geometry (v:path/v:formulas) is not reproduced",
                );
            }
            // A DrawingML shape — the modern spelling, which unlike VML states
            // arbitrary paths in a form Typst's `#curve` mirrors exactly. See
            // `mappers::dml_shape`, which appends (it can produce a shape
            // *and* the text that wouldn't fit inside it) rather than
            // returning one inline.
            RunContent::DmlShape(shape) => {
                dml_shape::lower_dml_shape(shape, ctx, &mut content)
            }
            RunContent::DmlUnsupported => {
                dml_shape::report_unsupported(&mut *ctx.report)
            }
            // An OLE embedding: a whole foreign application's document, which
            // nothing here can revive. Word's rendered preview picture comes
            // through as an ordinary sibling drawing, so the *look* survives
            // and only the liveness is lost — which is what this says, naming
            // the producer so the note is actionable rather than merely
            // truthful.
            RunContent::EmbeddedObject { prog_id } => {
                let detail = match prog_id {
                    Some(id) => format!(
                        "an embedded {id} object cannot be re-opened from Typst; Word's \
                         preview picture is kept in its place"
                    ),
                    None => "an embedded OLE object cannot be re-opened from Typst; \
                             Word's preview picture is kept in its place"
                        .into(),
                };
                ctx.report.drop("embedded object", &detail);
            }
        }
    }

    if content.is_empty() {
        return Vec::new();
    }

    let style = text_style_from_run_props(&eff, &mut *ctx.report);
    if style.is_empty() {
        content
    } else {
        vec![Inline::Styled { style, body: content }]
    }
}

/// Word's sixteen named highlight colors. Mirrors the exporter's own table
/// (`typst-docx`'s `word_highlight_name`) value-for-value so a marker survives
/// a Typst → DOCX → Typst round-trip with the RGB it started with, rather than
/// drifting to a differently-named neighbour on each pass.
const HIGHLIGHT_COLORS: &[(&str, [u8; 3])] = &[
    ("black", [0x00, 0x00, 0x00]),
    ("blue", [0x00, 0x00, 0xFF]),
    ("cyan", [0x00, 0xFF, 0xFF]),
    ("darkBlue", [0x00, 0x00, 0x80]),
    ("darkCyan", [0x00, 0x80, 0x80]),
    ("darkGray", [0x80, 0x80, 0x80]),
    ("darkGreen", [0x00, 0x80, 0x00]),
    ("darkMagenta", [0x80, 0x00, 0x80]),
    ("darkRed", [0x80, 0x00, 0x00]),
    ("darkYellow", [0x80, 0x80, 0x00]),
    ("green", [0x00, 0xFF, 0x00]),
    ("lightGray", [0xC0, 0xC0, 0xC0]),
    ("magenta", [0xFF, 0x00, 0xFF]),
    ("red", [0xFF, 0x00, 0x00]),
    ("white", [0xFF, 0xFF, 0xFF]),
    ("yellow", [0xFF, 0xFF, 0x00]),
];

/// Resolve a `w:highlight` name to its RGB. `"none"` — and any name outside
/// Word's fixed set — yields `None`, i.e. no marker at all.
fn highlight_color(name: &str) -> Option<[u8; 3]> {
    HIGHLIGHT_COLORS.iter().find(|(n, _)| *n == name).map(|(_, rgb)| *rgb)
}

/// Map `w:u` onto a Typst underline stroke. Word names far more line patterns
/// than Typst has dashes for; the ones with no counterpart still underline
/// (with the pattern loss reported) rather than losing the decoration.
fn underline_from_val(val: &str, color: Option<[u8; 3]>, report: &mut ImportReport) -> Underline {
    let dash = match val {
        "dotted" | "dottedHeavy" => Some("dotted"),
        "dash" | "dashedHeavy" | "dashLong" | "dashLongHeavy" => Some("dashed"),
        "dotDash" | "dashDotHeavy" | "dotDotDash" | "dashDotDotHeavy" => Some("dash-dotted"),
        _ => None,
    };
    if matches!(val, "double" | "wave" | "wavyHeavy" | "wavyDouble") {
        report.approximate(
            "underline",
            "Typst has no double/wavy underline; drawn as a single line",
        );
    }
    Underline { color, dash, thick: matches!(val, "thick" | "wavyHeavy" | "dottedHeavy") }
}

/// Split Word's single `w:lang` value ("en-US") into Typst's separate
/// `lang:`/`region:` arguments.
///
/// Typst validates both halves and *hard-errors* on anything else — an
/// ISO 639 language and an ISO 3166-1 alpha-2 region — which would fail the
/// whole compile over a cosmetic attribute. Word writes plenty that doesn't
/// fit: script subtags (`zh-Hans`), numeric UN regions (`es-419`), private
/// tags. Anything that isn't the exact shape Typst accepts is therefore left
/// off rather than passed through: a missing language is a far smaller error
/// than a document that won't build.
pub(crate) fn lower_lang(tag: &str) -> Option<Lang> {
    let is_alpha = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphabetic())
    };

    let mut parts = tag.split(['-', '_']);
    let lang = parts.next()?.trim();
    if !is_alpha(lang, 2..=3) {
        return None;
    }
    let region = parts.next().map(str::trim).filter(|r| is_alpha(r, 2..=2));
    Some(Lang { lang: lang.to_lowercase().into(), region: region.map(|r| r.to_uppercase().into()) })
}

fn text_style_from_run_props(eff: &RunProps, report: &mut ImportReport) -> TextStyle {
    if eff.dstrike == Some(true) {
        report.approximate("strikethrough", "double strikethrough drawn as a single line");
    }
    TextStyle {
        font: eff.font.clone(),
        size_pt: eff.size_half_pt.map(|h| half_point_to_pt(h as f64)),
        color: parse_hex_color(eff.color.as_deref()),
        bold: eff.bold == Some(true),
        italic: eff.italic == Some(true),
        underline: eff.underline.as_deref().filter(|u| *u != "none").map(|val| {
            underline_from_val(val, parse_hex_color(eff.underline_color.as_deref()), report)
        }),
        strike: eff.strike == Some(true) || eff.dstrike == Some(true),
        smallcaps: eff.smallcaps == Some(true),
        caps: eff.caps == Some(true),
        script: match eff.vert_align.as_deref() {
            Some("superscript") => Some(Script::Super),
            Some("subscript") => Some(Script::Sub),
            _ => None,
        },
        highlight: eff.highlight.as_deref().and_then(highlight_color),
        tracking_pt: eff.letter_spacing.map(|s| twip_to_abs(s as f64).to_pt()),
        lang: eff.lang.as_deref().and_then(lower_lang),
        // `w:rtl` is deliberately *not* carried across. Word needs an explicit
        // per-run direction because its layout won't infer one; Typst resolves
        // bidi from the Unicode text itself, so the flag tells it nothing it
        // doesn't already know — and forcing `dir` onto an inline span fights
        // that resolution (it panicked Typst's shaper on a real corpus
        // document, POI's `stress004`). Same reasoning as the `w:bdo`/`w:dir`
        // wrappers `wml::parse` already unwraps: losing a redundant direction
        // override is a far smaller error than mis-setting the text.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
    use crate::tdoc::Block;
    use crate::wml::model::{Paragraph, Run, RunProps, WmlPackage};

    fn text_paragraph(text: &str) -> crate::wml::model::BodyItem {
        crate::wml::model::BodyItem::Paragraph(Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::Text(text.into())],
            })],
        })
    }

    fn text_box_run(items: Vec<crate::wml::model::BodyItem>) -> RunItem {
        RunItem::Run(Run { props: RunProps::default(), content: vec![RunContent::TextBox(items)] })
    }

    #[test]
    fn text_box_lowers_to_an_inline_text_box_with_its_content() {
        let p = Paragraph {
            props: Default::default(),
            runs: vec![text_box_run(vec![text_paragraph("boxed text")])],
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inlines = lower_paragraph_inlines(&p, &mut ctx);

        assert_eq!(inlines.len(), 1);
        let Inline::TextBox(blocks) = &inlines[0] else {
            panic!("expected a text box, got {inlines:?}")
        };
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::Paragraph { body, .. } => {
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "boxed text"));
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }

        // The floating-position/size approximation is recorded once.
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "text box");
    }

    /// A document with several text boxes must not repeat the same
    /// approximation note once per box — the same dedup [`ImportReport`]
    /// already gives every other repeated construct (see
    /// `mappers::field`'s equivalent test).
    #[test]
    fn repeated_text_boxes_produce_one_approximation_note() {
        let p = Paragraph {
            props: Default::default(),
            runs: vec![
                text_box_run(vec![text_paragraph("one")]),
                text_box_run(vec![text_paragraph("two")]),
            ],
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inlines = lower_paragraph_inlines(&p, &mut ctx);

        assert_eq!(inlines.len(), 2);
        assert_eq!(
            report.notes.len(),
            1,
            "expected the note to be deduplicated: {:?}",
            report.notes
        );
    }

    /// An empty text box (no visible content inside) must still lower
    /// cleanly — an empty `Vec<Block>`, not dropped or panicking.
    #[test]
    fn empty_text_box_lowers_to_an_empty_block_list() {
        let p = Paragraph { props: Default::default(), runs: vec![text_box_run(vec![])] };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inlines = lower_paragraph_inlines(&p, &mut ctx);

        assert_eq!(inlines.len(), 1);
        let Inline::TextBox(blocks) = &inlines[0] else { panic!("expected a text box") };
        assert!(blocks.is_empty());
    }

    /// Word's named highlights must resolve to exactly the RGB the *exporter*
    /// writes for the same name, so a marker survives a
    /// Typst → DOCX → Typst round-trip instead of drifting each pass.
    #[test]
    fn highlight_resolves_to_words_named_color() {
        assert_eq!(highlight_color("yellow"), Some([0xFF, 0xFF, 0x00]));
        assert_eq!(highlight_color("darkBlue"), Some([0x00, 0x00, 0x80]));
        // "none" — and any name outside Word's fixed set — means no marker.
        assert_eq!(highlight_color("none"), None);
        assert_eq!(highlight_color("chartreuse"), None);
    }

    #[test]
    fn lang_splits_into_typst_language_and_region() {
        assert_eq!(
            lower_lang("en-US"),
            Some(Lang { lang: "en".into(), region: Some("US".into()) })
        );
        assert_eq!(lower_lang("de"), Some(Lang { lang: "de".into(), region: None }));
        // Underscore separators and odd casing normalise the same way.
        assert_eq!(
            lower_lang("PT_br"),
            Some(Lang { lang: "pt".into(), region: Some("BR".into()) })
        );
        assert_eq!(lower_lang(""), None);
    }

    /// Typst hard-errors on a region that isn't ISO 3166-1 alpha-2 and on a
    /// non-ISO-639 language, so tags Word writes but Typst rejects must lose
    /// the offending half rather than fail the document's compile. A real
    /// corpus document (`WordWithAttachments`) did exactly this.
    #[test]
    fn a_tag_typst_would_reject_loses_the_offending_half() {
        // Script subtag, not a region.
        assert_eq!(lower_lang("zh-Hans"), Some(Lang { lang: "zh".into(), region: None }));
        // Numeric UN region.
        assert_eq!(lower_lang("es-419"), Some(Lang { lang: "es".into(), region: None }));
        // Not an ISO 639 language at all — nothing usable survives.
        assert_eq!(lower_lang("x-none"), None);
        assert_eq!(lower_lang("1033"), None);
    }

    #[test]
    fn underline_pattern_maps_to_a_typst_dash() {
        let mut report = ImportReport::default();
        assert_eq!(underline_from_val("dotted", None, &mut report).dash, Some("dotted"));
        assert_eq!(underline_from_val("dashLong", None, &mut report).dash, Some("dashed"));
        assert_eq!(underline_from_val("single", None, &mut report).dash, None);
        assert!(underline_from_val("thick", None, &mut report).thick);
        assert!(report.notes.is_empty(), "{:?}", report.notes);
    }

    /// A pattern Typst has no dash for still underlines — the decoration is
    /// never dropped just because its pattern is unusual — and reports it.
    #[test]
    fn wavy_underline_still_underlines_and_reports_the_pattern_loss() {
        let mut report = ImportReport::default();
        let underline = underline_from_val("wave", None, &mut report);
        assert_eq!(underline.dash, None);
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "underline");
    }

    /// A bare `<w:u/>` (no `w:val`) is a single underline, not "no underline";
    /// only an explicit `w:val="none"` turns one off.
    #[test]
    fn underline_toggles_off_only_for_an_explicit_none() {
        let mut report = ImportReport::default();
        let on = text_style_from_run_props(
            &RunProps { underline: Some("single".into()), ..Default::default() },
            &mut report,
        );
        assert!(on.underline.is_some());
        let off = text_style_from_run_props(
            &RunProps { underline: Some("none".into()), ..Default::default() },
            &mut report,
        );
        assert!(off.underline.is_none());
    }

    #[test]
    fn double_strike_lowers_to_a_single_strike_and_reports() {
        let mut report = ImportReport::default();
        let style = text_style_from_run_props(
            &RunProps { dstrike: Some(true), ..Default::default() },
            &mut report,
        );
        assert!(style.strike);
        assert_eq!(report.notes[0].what, "strikethrough");
    }

    #[test]
    fn caps_tracking_and_highlight_carry_through() {
        let mut report = ImportReport::default();
        let style = text_style_from_run_props(
            &RunProps {
                caps: Some(true),
                letter_spacing: Some(20),
                highlight: Some("green".into()),
                ..Default::default()
            },
            &mut report,
        );
        assert!(style.caps);
        assert_eq!(style.highlight, Some([0x00, 0xFF, 0x00]));
        // 20 twips is exactly one point.
        assert_eq!(style.tracking_pt, Some(1.0));
    }

    /// `w:rtl` must not become an inline `dir:` override — Typst resolves bidi
    /// from the text itself, and forcing direction onto a span panicked its
    /// shaper on a real corpus document (POI's `stress004`).
    #[test]
    fn an_rtl_run_adds_no_direction_override() {
        let mut report = ImportReport::default();
        let style = text_style_from_run_props(
            &RunProps { rtl: Some(true), ..Default::default() },
            &mut report,
        );
        assert!(style.is_empty(), "expected no styling from w:rtl alone: {style:?}");
    }
}

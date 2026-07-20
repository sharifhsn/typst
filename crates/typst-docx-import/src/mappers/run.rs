//! The `run` mapper: Word runs (`w:r`), hyperlinks (`w:hyperlink`), and
//! fields (`w:fldSimple`/`w:fldChar` — see [`crate::mappers::field`]) → the
//! Typst IR's [`Inlines`]. A run's footnote/endnote references
//! ([`RunContent::NoteRef`]) resolve here too, via
//! [`crate::mappers::note::lower_note_ref`] — one more thing a run's content
//! can hold, alongside text/tabs/breaks/drawings/math/text boxes.

use typst_ooxml_core::units::half_point_to_pt;

use crate::lower::{lower_items, parse_hex_color, LowerCtx};
use crate::mappers::{field, math, note, shape};
use crate::resolve::styles::effective_run;
use crate::tdoc::{Inline, Inlines, Script, TextStyle};
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
                    None => {
                        if anchor.is_some() {
                            ctx.report.approximate(
                                "internal hyperlink",
                                "anchor not resolved; link dropped, text kept",
                            );
                        }
                        out.extend(inner);
                    }
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
            RunContent::Math(frag) => content.push(math::omml_to_inline(frag, &mut *ctx.report)),
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
        }
    }

    if content.is_empty() {
        return Vec::new();
    }

    let style = text_style_from_run_props(&eff);
    if style.is_empty() {
        content
    } else {
        vec![Inline::Styled { style, body: content }]
    }
}

fn text_style_from_run_props(eff: &RunProps) -> TextStyle {
    TextStyle {
        font: eff.font.clone(),
        size_pt: eff.size_half_pt.map(|h| half_point_to_pt(h as f64)),
        color: parse_hex_color(eff.color.as_deref()),
        bold: eff.bold == Some(true),
        italic: eff.italic == Some(true),
        underline: eff.underline.as_deref().is_some_and(|u| u != "none"),
        strike: eff.strike == Some(true),
        smallcaps: eff.smallcaps == Some(true),
        script: match eff.vert_align.as_deref() {
            Some("superscript") => Some(Script::Super),
            Some("subscript") => Some(Script::Sub),
            _ => None,
        },
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
}

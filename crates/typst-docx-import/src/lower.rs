//! Lower the Word IR ([`crate::wml`]) to the Typst IR ([`crate::tdoc`]).
//! The mirror of the exporter's `convert.rs` + `mappers/`.

use typst_ooxml_core::units::half_point_to_pt;

use crate::mappers;
use crate::mappers::para::{ParaKind, ParaResult};
use crate::opts::ImportOptions;
use crate::report::ImportReport;
use crate::tdoc::{Block, List, ListItem, Stmt, TextStyle, TypstDoc};
use crate::wml::model::{BodyItem, RunProps, WmlPackage};

pub fn lower(package: &WmlPackage, options: &ImportOptions, report: &mut ImportReport) -> TypstDoc {
    let mut doc = TypstDoc::default();

    // Preamble: page geometry (plus header/footer) from the body's
    // `w:sectPr`, and a document default `#set text(..)` so bare runs
    // inherit the doc's base font/size.
    if let Some(sect_pr) = &package.body.sect_pr {
        let page = mappers::section::lower_section(sect_pr, package, options, report);
        doc.preamble.push(Stmt::SetPage(page));
    }
    if let Some(style) = default_text_style(&package.styles.default_run) {
        doc.preamble.push(Stmt::SetText(style));
    }

    doc.body = lower_items(&package.body.items, package, options, report);
    doc
}

/// Lower a sequence of body items (the document body, or a table cell's
/// content) to blocks, accumulating consecutive list-item paragraphs into a
/// single [`Block::List`] rather than emitting one list per item.
pub(crate) fn lower_items(
    items: &[BodyItem],
    package: &WmlPackage,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut pending_list: Option<List> = None;

    for item in items {
        match item {
            BodyItem::Paragraph(p) => {
                let ParaResult { anchored, kind } =
                    mappers::para::lower_paragraph(p, package, options, report);

                // An anchored figure/chart is a block in its own right and
                // leads the paragraph it hangs off. Typst can't place a block
                // between two items of one list, so an anchored block inside a
                // list item ends the list and starts a new one after it —
                // slightly worse than Word's layout, but it keeps the image.
                if let Some(block) = anchored {
                    if let Some(list) = pending_list.take() {
                        blocks.push(Block::List(list));
                    }
                    blocks.push(block);
                }

                match kind {
                    ParaKind::ListItem { ordered, level, body } => {
                        let li = ListItem { ordered, level, body };
                        match pending_list.as_mut() {
                            Some(list) => list.items.push(li),
                            None => pending_list = Some(List { items: vec![li] }),
                        }
                    }
                    other => {
                        if let Some(list) = pending_list.take() {
                            blocks.push(Block::List(list));
                        }
                        push_para_kind(&mut blocks, other);
                    }
                }
            }
            BodyItem::Table(t) => {
                if let Some(list) = pending_list.take() {
                    blocks.push(Block::List(list));
                }
                blocks.push(Block::Table(mappers::table::lower_table(t, package, options, report)));
            }
        }
    }
    if let Some(list) = pending_list.take() {
        blocks.push(Block::List(list));
    }
    blocks
}

fn push_para_kind(blocks: &mut Vec<Block>, kind: ParaKind) {
    match kind {
        ParaKind::Break(kind) => blocks.push(Block::Break(kind)),
        ParaKind::Rule => blocks.push(Block::Rule),
        ParaKind::Heading { level, body } => blocks.push(Block::Heading { level, body }),
        ParaKind::Paragraph { style, body } => blocks.push(Block::Paragraph { style, body }),
        ParaKind::Empty => {}
        ParaKind::ListItem { .. } => unreachable!("list items are handled by the caller"),
    }
}

/// A document-default `#set text(..)` from `styles.xml`'s docDefaults, so
/// bare text inherits the document's base font/size/color.
fn default_text_style(run: &RunProps) -> Option<TextStyle> {
    let mut style = TextStyle::default();
    let mut any = false;
    if let Some(font) = &run.font {
        style.font = Some(font.clone());
        any = true;
    }
    if let Some(size) = run.size_half_pt {
        style.size_pt = Some(half_point_to_pt(size as f64));
        any = true;
    }
    if let Some(color) = parse_hex_color(run.color.as_deref()) {
        style.color = Some(color);
        any = true;
    }
    any.then_some(style)
}

/// Parse an OOXML hex color (`w:color/@w:val`, e.g. `"FF0000"`) into RGB
/// bytes. `"auto"` (the "let the app decide" sentinel) has no fixed color.
pub(crate) fn parse_hex_color(s: Option<&str>) -> Option<[u8; 3]> {
    let s = s?;
    if s.len() != 6 || s.eq_ignore_ascii_case("auto") {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::tdoc::Inline;
    use crate::wml::model::{
        Body, LevelFormat, NumRef, Numbering, ParaProps, Paragraph, Run, RunContent, RunItem,
        RunProps, Style, StyleKind, Styles,
    };

    fn text_run(text: &str) -> RunItem {
        RunItem::Run(Run { props: RunProps::default(), content: vec![RunContent::Text(text.into())] })
    }

    #[test]
    fn heading_style_lowers_to_heading_block() {
        let mut by_id = FxHashMap::default();
        by_id.insert(
            "Heading1".into(),
            Style {
                id: "Heading1".into(),
                name: Some("heading 1".into()),
                kind: StyleKind::Paragraph,
                based_on: None,
                outline_level: Some(0),
                run: RunProps::default(),
                para: ParaProps::default(),
            },
        );
        let package = WmlPackage {
            body: Body {
                items: vec![BodyItem::Paragraph(Paragraph {
                    props: ParaProps { style_id: Some("Heading1".into()), ..Default::default() },
                    runs: vec![text_run("Title")],
                })],
                sect_pr: None,
            },
            styles: Styles { by_id, ..Default::default() },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let doc = lower(&package, &ImportOptions::default(), &mut report);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::Heading { level, body } => {
                assert_eq!(*level, 1);
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "Title"));
            }
            other => panic!("expected a heading, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_list_paragraphs_become_one_list_block() {
        let mut instances = FxHashMap::default();
        instances.insert(1, 100);
        let mut level_fmt = FxHashMap::default();
        level_fmt.insert(0, LevelFormat { num_fmt: "bullet".into() });
        let mut abstract_nums = FxHashMap::default();
        abstract_nums.insert(100, level_fmt);

        let para = |text: &str| {
            BodyItem::Paragraph(Paragraph {
                props: ParaProps { num: Some(NumRef { num_id: 1, ilvl: 0 }), ..Default::default() },
                runs: vec![text_run(text)],
            })
        };

        let package = WmlPackage {
            body: Body { items: vec![para("Item 1"), para("Item 2")], sect_pr: None },
            numbering: Numbering { instances, abstract_nums },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let doc = lower(&package, &ImportOptions::default(), &mut report);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::List(list) => {
                assert_eq!(list.items.len(), 2);
                assert!(!list.items[0].ordered);
                assert!(matches!(&list.items[0].body[..], [Inline::Text(t)] if t == "Item 1"));
                assert!(matches!(&list.items[1].body[..], [Inline::Text(t)] if t == "Item 2"));
            }
            other => panic!("expected a list, got {other:?}"),
        }
    }

    #[test]
    fn direct_bold_color_run_becomes_styled_inline() {
        let run = RunItem::Run(Run {
            props: RunProps { bold: Some(true), color: Some("FF0000".into()), ..Default::default() },
            content: vec![RunContent::Text("Hi".into())],
        });
        let package = WmlPackage {
            body: Body {
                items: vec![BodyItem::Paragraph(Paragraph {
                    props: ParaProps::default(),
                    runs: vec![run],
                })],
                sect_pr: None,
            },
            ..Default::default()
        };

        let mut report = ImportReport::default();
        let doc = lower(&package, &ImportOptions::default(), &mut report);

        assert_eq!(doc.body.len(), 1);
        match &doc.body[0] {
            Block::Paragraph { body, .. } => match &body[..] {
                [Inline::Styled { style, body }] => {
                    assert!(style.bold);
                    assert_eq!(style.color, Some([255, 0, 0]));
                    assert!(matches!(&body[..], [Inline::Text(t)] if t == "Hi"));
                }
                other => panic!("expected a styled run, got {other:?}"),
            },
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }
}

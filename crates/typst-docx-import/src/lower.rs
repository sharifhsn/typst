//! Lower the Word IR ([`crate::wml`]) to the Typst IR ([`crate::tdoc`]).
//! The mirror of the exporter's `convert.rs` + `mappers/`.

use typst_ooxml_core::units::half_point_to_pt;

use crate::mappers;
use crate::mappers::para::{ParaKind, ParaResult};
use crate::opts::ImportOptions;
use crate::report::ImportReport;
use crate::tdoc::{Block, List, ListItem, Stmt, TextStyle, TypstDoc};
use crate::wml::model::{BodyItem, RunProps, WmlPackage};

/// Everything the lowering phase threads through: the package being read, the
/// options that govern how it's lowered, the loss report, and the guards
/// that stop a malformed document from recursing forever.
pub(crate) struct LowerCtx<'a> {
    pub package: &'a WmlPackage,
    pub options: &'a ImportOptions,
    pub report: &'a mut ImportReport,
    /// `(is_endnote, id)` pairs currently being lowered — the cycle guard for
    /// note resolution. A malformed document can have note 1 reference note 1,
    /// directly or through a chain, which would otherwise recurse forever.
    note_stack: Vec<(bool, i64)>,
}

/// How deep a chain of notes referencing other notes may nest before
/// [`LowerCtx::enter_note`] refuses to go further — the note-lowering
/// counterpart of `MAX_TABLE_DEPTH`/`MAX_SDT_DEPTH` in `wml::parse`. Real
/// documents essentially never reference a note from within another note at
/// all; this only bites a pathological or hostile document, and backs up the
/// cycle check for a chain long enough to still not repeat any single id.
const MAX_NOTE_DEPTH: usize = 8;

impl<'a> LowerCtx<'a> {
    pub(crate) fn new(
        package: &'a WmlPackage,
        options: &'a ImportOptions,
        report: &'a mut ImportReport,
    ) -> Self {
        LowerCtx { package, options, report, note_stack: Vec::new() }
    }

    /// Try to enter `(endnote, id)`'s body for lowering. Returns `false` —
    /// without recording anything itself, so the caller can report the
    /// specific construct ("footnote" vs "endnote") — if `id` is already on
    /// the stack (a direct or indirect cycle) or the stack is already at
    /// [`MAX_NOTE_DEPTH`]. Every successful `true` must be paired with a
    /// matching [`Self::exit_note`] once that note's body is fully lowered.
    ///
    /// A `Vec` doubles as the depth counter (its length is the current
    /// nesting depth) and, at the sizes a note chain can reach, a linear
    /// `contains` scan to check for a repeat is cheap — no need for a
    /// `HashSet` here.
    pub(crate) fn enter_note(&mut self, endnote: bool, id: i64) -> bool {
        if self.note_stack.len() >= MAX_NOTE_DEPTH || self.note_stack.contains(&(endnote, id)) {
            return false;
        }
        self.note_stack.push((endnote, id));
        true
    }

    /// Leave the note most recently entered via [`Self::enter_note`].
    pub(crate) fn exit_note(&mut self) {
        self.note_stack.pop();
    }
}

pub(crate) fn lower(ctx: &mut LowerCtx) -> TypstDoc {
    let mut doc = TypstDoc::default();
    let package = ctx.package;

    // Preamble: page geometry (plus header/footer) from the body's
    // `w:sectPr`, and a document default `#set text(..)` so bare runs
    // inherit the doc's base font/size.
    if let Some(sect_pr) = &package.body.sect_pr {
        let page = mappers::section::lower_section(sect_pr, ctx);
        doc.preamble.push(Stmt::SetPage(page));
    }
    if let Some(style) = default_text_style(&package.styles.default_run) {
        doc.preamble.push(Stmt::SetText(style));
    }

    doc.body = lower_items(&package.body.items, ctx);
    doc
}

/// Lower a sequence of body items (the document body, or a table cell's
/// content) to blocks, accumulating consecutive list-item paragraphs into a
/// single [`Block::List`] rather than emitting one list per item.
pub(crate) fn lower_items(items: &[BodyItem], ctx: &mut LowerCtx) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut pending_list: Option<List> = None;

    for item in items {
        match item {
            BodyItem::Paragraph(p) => {
                let ParaResult { anchored, kind } = mappers::para::lower_paragraph(p, ctx);

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
                blocks.push(Block::Table(mappers::table::lower_table(t, ctx)));
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let doc = lower(&mut ctx);

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

    // The five tests below moved here from `report.rs` — they exercise
    // `LowerCtx`'s note-cycle guard, which used to live on `ImportReport`.

    #[test]
    fn distinct_notes_nest_and_unwind_cleanly() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        assert!(ctx.enter_note(false, 2));
        ctx.exit_note();
        ctx.exit_note();
        // Nothing left on the stack, so id 1 can be entered again.
        assert!(ctx.enter_note(false, 1));
    }

    #[test]
    fn a_note_cannot_re_enter_itself_while_still_on_the_stack() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        // Direct self-reference: note 1, still being lowered, refers to
        // itself again.
        assert!(!ctx.enter_note(false, 1));
    }

    #[test]
    fn an_indirect_cycle_through_another_note_is_also_refused() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        assert!(ctx.enter_note(false, 2));
        // Note 2 refers back to note 1, which is still on the stack.
        assert!(!ctx.enter_note(false, 1));
    }

    #[test]
    fn footnote_and_endnote_ids_are_tracked_independently() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        assert!(ctx.enter_note(false, 1));
        // An endnote with the same numeric id is a different note.
        assert!(ctx.enter_note(true, 1));
    }

    #[test]
    fn a_long_non_cycling_chain_is_still_capped_by_depth() {
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        for id in 0..100 {
            if !ctx.enter_note(false, id) {
                // Must give up well before 100 distinct, never-repeating ids.
                assert!(id < 100);
                return;
            }
        }
        panic!("expected the depth cap to stop this chain");
    }
}

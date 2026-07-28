//! Turning Typst's resolved heading numbers into Word auto-numbering.
//!
//! Typst hands the exporter a *finished* number for every heading (`1.2.3`), so
//! the cheap mapping is to bake it in as a leading run — which is what
//! [`crate::mappers::heading`] does. That reads correctly but is dead text: it
//! does not renumber when the reader inserts a section in Word, and it cannot
//! be edited as numbering.
//!
//! Word's own idiom is a multilevel `w:abstractNum` whose levels are bound to
//! the `HeadingN` paragraph styles (a `w:pStyle` inside each `w:lvl`), with
//! those styles carrying the matching `w:numPr`. Every heading in the document
//! — and every heading the reader adds later — is then numbered by Word.
//!
//! The catch is that the two numbering models are not the same machine. Typst
//! numbers a heading from a counter that show rules, `counter` updates and
//! `offset` can move anywhere, and formats it with a pattern that may be an
//! arbitrary closure; Word increments one counter per level and formats it with
//! `w:numFmt` + `w:lvlText`. Rather than assume the two agree, this pass
//! *checks*: it maps the pattern onto Word's model, replays Word's numbering
//! over the document's heading sequence, and goes live only when every heading
//! comes out with the number Typst computed. Anything else keeps the frozen
//! text and is recorded as an approximation, because a heading numbered `3.1`
//! where Typst said `2.1` is worse than one that does not renumber.

use ecow::{EcoString, eco_format};
use typst_library::model::Numbering;
use typst_syntax::Span;

use crate::dom::{
    AbstractNum, Block, Footnote, HdrFtrPart, HeadingStyle, ListLevel, MultiLevelType,
    NumFmt, NumInstance, NumberingTable, Para, ParaChild, Run,
};
use crate::report::{
    DecisionReason, ExportSource, FidelityReport, LossSet, Representation,
};

/// The deepest level Word's numbering model addresses (`w:ilvl` 0..=8).
const MAX_LEVELS: usize = 9;

/// The reserved `w:numId` that cancels numbering for one paragraph. It is a
/// sentinel rather than a real instance, so it is never registered in
/// `numbering.xml`.
pub(crate) const NO_NUMBERING_ID: u32 = 0;

/// Derives the nine `w:lvl` shapes a heading `numbering:` pattern maps onto.
///
/// The pattern → `w:lvlText`/`w:numFmt` mapping is the one enumerations already
/// use ([`crate::mappers::list`]); heading numbering differs only in always
/// showing a level's full ancestry, in binding each level to a `HeadingN`
/// style, and in its indents.
///
/// `None` when Word cannot state the pattern: a closure has no `w:lvlText` at
/// all, and neither does a numeral system outside the five `w:numFmt` values
/// Word shares with Typst.
pub(crate) fn derive_levels(numbering: &Numbering) -> Option<Vec<ListLevel>> {
    use crate::mappers::list::{full_level_text, native_num_fmt};

    let Numbering::Pattern(pattern) = numbering else { return None };
    let last = pattern.pieces.last()?;
    (0..MAX_LEVELS)
        .map(|index| {
            let (_, system) = pattern.pieces.get(index).unwrap_or(last);
            Some(ListLevel {
                num_fmt: native_num_fmt(*system)?,
                lvl_text: full_level_text(pattern, index),
                start: 1,
                // Word's built-in heading numbering hangs the number in the
                // margin; Typst's sits inline with a tab after it, which is
                // exactly a zero indent plus `w:lvl`'s default tab suffix.
                ind_left: 0,
                ind_hanging: 0,
                bullet_font: None,
                pstyle: Some(eco_format!("Heading{}", index + 1)),
            })
        })
        .collect()
}

/// The mutable document parts this pass rewrites.
pub(crate) struct Parts<'a> {
    pub body: &'a mut Vec<Block>,
    pub footnotes: &'a mut [Footnote],
    pub headers: &'a mut [HdrFtrPart],
    pub footers: &'a mut [HdrFtrPart],
}

impl Parts<'_> {
    /// Every part's blocks: the flowing body plus the furniture and notes that
    /// can hold their own references.
    fn all(&mut self) -> impl Iterator<Item = &mut Vec<Block>> {
        std::iter::once(&mut *self.body)
            .chain(self.footnotes.iter_mut().map(|note| &mut note.blocks))
            .chain(
                self.headers
                    .iter_mut()
                    .chain(&mut *self.footers)
                    .map(|part| &mut part.blocks),
            )
    }
}

/// Rewrites the document's frozen heading numbers into live Word numbering,
/// when that reproduces Typst's numbers exactly; otherwise records why it did
/// not.
pub(crate) fn apply(
    parts: &mut Parts,
    levels: Option<&Vec<ListLevel>>,
    numbering: &mut NumberingTable,
    heading_styles: &mut [HeadingStyle],
    report: &mut FidelityReport,
) {
    let mut sequence = Vec::new();
    collect_headings(parts.body, &mut sequence);
    // A document whose headings are all unnumbered has nothing to make live —
    // and nothing it lost by staying that way.
    if sequence.iter().all(|(_, number)| number.is_none()) {
        return;
    }

    let levels = match plan(&sequence, levels, parts) {
        Ok(levels) => levels,
        Err(reason) => {
            report.record_span(
                ExportSource::new("live heading numbering", Span::detached(), None),
                Representation::Approximate,
                reason,
                LossSet::DYNAMIC_BEHAVIOR,
                0,
            );
            return;
        }
    };

    let num_id = register(numbering, levels);
    for style in heading_styles.iter_mut() {
        if usize::from(style.level) <= MAX_LEVELS {
            style.num_id = Some(num_id);
        }
    }

    let mut emptied = Vec::new();
    strip_frozen_numbers(parts.body, &mut emptied);
    for blocks in parts.all() {
        retarget_number_references(blocks, &emptied);
    }
}

/// Decides whether Word's numbering model can reproduce this document's heading
/// numbers, returning the levels to register or the reason it cannot.
fn plan(
    sequence: &[(u8, Option<EcoString>)],
    levels: Option<&Vec<ListLevel>>,
    parts: &Parts,
) -> Result<Vec<ListLevel>, DecisionReason> {
    // A closure, a numeral system Word lacks, or two headings governed by
    // different patterns: one `w:abstractNum` cannot describe the document.
    let levels = levels.ok_or(DecisionReason::TypstOwnedHeadingNumber)?;

    // Numbering binds to the `HeadingN` *style*, so a heading-styled paragraph
    // this pass does not rewrite would be numbered twice — once by Word and
    // once by its own frozen run. Headings outside the flowing body are also
    // outside Word's numbering sequence entirely.
    if parts.footnotes.iter().any(|note| contains_heading(&note.blocks))
        || parts
            .headers
            .iter()
            .chain(parts.footers.iter())
            .any(|part| contains_heading(&part.blocks))
    {
        return Err(DecisionReason::TypstOwnedHeadingNumber);
    }

    // A composite caption number reaches the nearest chapter prefix with a
    // `STYLEREF` at the `TypstHeadingNumber{N}` run this pass would delete.
    // Word can retrieve a paragraph's *number* through that field too, but only
    // by naming the heading style rather than the character style, and the
    // exact switch spelling is not something this exporter can verify against
    // Word; leaving such a document frozen keeps a working caption prefix.
    let styleref = parts
        .body
        .iter()
        .chain(parts.footnotes.iter().flat_map(|note| &note.blocks))
        .chain(
            parts
                .headers
                .iter()
                .chain(parts.footers.iter())
                .flat_map(|part| &part.blocks),
        );
    if styleref.into_iter().any(uses_heading_number_styleref) {
        return Err(DecisionReason::TypstOwnedHeadingNumber);
    }

    // Replay Word's numbering and require it to agree with Typst's, heading by
    // heading. This is what makes the mapping safe without having to reason
    // about every way a Typst counter can be moved.
    let mut counters = [0u64; MAX_LEVELS];
    for (level, expected) in sequence {
        let Some(index) = usize::from(*level).checked_sub(1).filter(|i| *i < MAX_LEVELS)
        else {
            // Deeper than Word numbers. Its style gains no `w:numPr`, so it
            // keeps its frozen text and — like an unnumbered heading — leaves
            // both models' counters alone.
            continue;
        };
        let Some(expected) = expected else {
            // An unnumbered heading leaves Word's counters alone too, which is
            // what the `w:numId="0"` override emitted below guarantees.
            continue;
        };
        counters[index] += 1;
        counters[index + 1..].fill(0);
        if render(levels, index, &counters).as_ref() != Some(expected) {
            return Err(DecisionReason::TypstOwnedHeadingNumber);
        }
    }

    Ok(levels.clone())
}

/// Renders one `w:lvl` the way Word does: substitute `%n` with counter `n`
/// formatted in *level n's* own `w:numFmt`, and keep everything else literal.
///
/// `None` for a number no level can state — notably a zero counter under a
/// roman or alphabetic format, which Word renders as nothing.
fn render(levels: &[ListLevel], index: usize, counters: &[u64]) -> Option<EcoString> {
    let mut out = EcoString::new();
    let mut rest = levels[index].lvl_text.as_str();
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let referenced = rest[at + 1..].chars().next()?.to_digit(10)? as usize;
        let level = levels.get(referenced.checked_sub(1)?)?;
        out.push_str(&format_counter(level.num_fmt, *counters.get(referenced - 1)?)?);
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    Some(out)
}

/// Formats a counter value in a `w:numFmt`.
fn format_counter(fmt: NumFmt, value: u64) -> Option<EcoString> {
    if value == 0 {
        // A level nothing has reached yet: Word prints `0` for a decimal and
        // nothing at all for the others, so only decimal can match Typst.
        return matches!(fmt, NumFmt::Decimal).then(|| "0".into());
    }
    match fmt {
        NumFmt::Decimal => Some(eco_format!("{value}")),
        NumFmt::LowerLetter => letter(value, 'a'),
        NumFmt::UpperLetter => letter(value, 'A'),
        NumFmt::LowerRoman => roman(value).map(|numeral| numeral.to_lowercase()),
        NumFmt::UpperRoman => roman(value),
        NumFmt::Bullet | NumFmt::None => None,
    }
}

/// Word's alphabetic numbering: `a`..`z`, then the letter repeats (`aa`, `bb`)
/// rather than carrying.
fn letter(value: u64, base: char) -> Option<EcoString> {
    let index = u32::try_from(value - 1).ok()?;
    let letter = char::from_u32(u32::from(base) + index % 26)?;
    Some(std::iter::repeat_n(letter, index as usize / 26 + 1).collect())
}

fn roman(value: u64) -> Option<EcoString> {
    const NUMERALS: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    // Word's roman levels stop at 32767; Typst's own system stops at 3999, so
    // anything larger has no shared representation.
    if value > 3999 {
        return None;
    }
    let mut out = EcoString::new();
    let mut rest = value;
    for (amount, numeral) in NUMERALS {
        while rest >= amount {
            out.push_str(numeral);
            rest -= amount;
        }
    }
    Some(out)
}

/// Adds the heading `w:abstractNum` and the single `w:num` instance the
/// `HeadingN` styles join.
fn register(numbering: &mut NumberingTable, levels: Vec<ListLevel>) -> u32 {
    let abstract_id = numbering.abstracts.len() as u32;
    numbering.abstracts.push(AbstractNum {
        id: abstract_id,
        levels,
        multilevel: MultiLevelType::Multilevel,
    });
    // List instances come from `DocxCtx::next_num_id`, which has already
    // finished handing them out; continue past the highest one in use.
    let num_id = numbering.nums.iter().map(|num| num.num_id).max().unwrap_or(0) + 1;
    numbering
        .nums
        .push(NumInstance { num_id, abstract_id, start_override: None });
    num_id
}

/// Collects every heading paragraph's level and baked number, in document
/// order.
fn collect_headings(blocks: &[Block], out: &mut Vec<(u8, Option<EcoString>)>) {
    for_each_para(blocks, &mut |para| {
        if let Some(level) = heading_level(para) {
            out.push((level, frozen_number(para, level).cloned()));
        }
    });
}

fn contains_heading(blocks: &[Block]) -> bool {
    let mut found = false;
    for_each_para(blocks, &mut |para| found |= heading_level(para).is_some());
    found
}

fn for_each_para(blocks: &[Block], f: &mut dyn FnMut(&Para)) {
    for block in blocks {
        match block {
            Block::Para(para) => f(para),
            Block::Table(tbl) => {
                for cell in tbl.rows.iter().flat_map(|row| &row.cells) {
                    for_each_para(&cell.blocks, f);
                }
            }
            _ => {}
        }
    }
}

fn heading_level(para: &Para) -> Option<u8> {
    let level = para.props.style.as_deref()?.strip_prefix("Heading")?;
    let level = level.parse::<u8>().ok()?;
    (level > 0).then_some(level)
}

/// The character style [`crate::mappers::heading`] puts on a baked number run.
/// It doubles as the `STYLEREF` handle composite caption numbers retrieve a
/// chapter prefix through.
fn number_style(level: u8) -> EcoString {
    eco_format!("TypstHeadingNumber{level}")
}

fn frozen_number(para: &Para, level: u8) -> Option<&EcoString> {
    let style = number_style(level);
    para.content.iter().find_map(|child| match child {
        ParaChild::Run(Run::Text { props, text })
            if props.style.as_ref() == Some(&style) =>
        {
            Some(text)
        }
        _ => None,
    })
}

/// Drops each heading's baked number run and the tab behind it, leaving Word's
/// numbering to supply both, and opts unnumbered headings out of the style's
/// numbering.
///
/// The bookmark that wrapped a number stays, now empty: `REF … \w` reports the
/// *paragraph's* number whatever the bookmark spans, so keeping it is what lets
/// existing references survive (see [`retarget_number_references`]). Those
/// bookmarks' names are collected into `emptied`.
fn strip_frozen_numbers(blocks: &mut [Block], emptied: &mut Vec<EcoString>) {
    for block in blocks {
        match block {
            Block::Para(para) => {
                if let Some(level) = heading_level(para) {
                    strip_frozen_number(para, level, emptied);
                }
            }
            Block::Table(tbl) => {
                for row in &mut tbl.rows {
                    for cell in &mut row.cells {
                        strip_frozen_numbers(&mut cell.blocks, emptied);
                    }
                }
            }
            _ => {}
        }
    }
}

fn strip_frozen_number(para: &mut Para, level: u8, emptied: &mut Vec<EcoString>) {
    if usize::from(level) > MAX_LEVELS {
        // Deeper than Word numbers; its style carries no `w:numPr`, so the
        // frozen text is still the only number this heading has.
        return;
    }
    let style = number_style(level);
    let Some(at) = para.content.iter().position(|child| {
        matches!(child, ParaChild::Run(Run::Text { props, .. })
            if props.style.as_ref() == Some(&style))
    }) else {
        // Word numbers every paragraph in a style that carries `w:numPr`, so a
        // heading Typst left unnumbered has to opt out explicitly.
        para.props.num = Some((NO_NUMBERING_ID, level - 1));
        return;
    };

    if let Some(ParaChild::BookmarkStart { name, .. }) =
        at.checked_sub(1).map(|before| &para.content[before])
    {
        emptied.push(name.clone());
    }
    para.content.remove(at);
    // The tab the mapper wrote after the number sits either directly at `at` or
    // just past the number bookmark's end.
    let tab = match para.content.get(at) {
        Some(ParaChild::BookmarkEnd { .. }) => at + 1,
        _ => at,
    };
    if matches!(para.content.get(tab), Some(ParaChild::Run(Run::Tab))) {
        para.content.remove(tab);
    }
}

/// Points the document's live number references at the paragraph number.
///
/// `DocxCtx::live_number_reference_runs` emits ` REF <bookmark> \h `, which
/// returns the bookmark's *text* — the baked number this pass just removed.
/// `\w` instead returns the number of the paragraph the bookmark sits in, in
/// full context: precisely Word's own "Heading number (full context)"
/// cross-reference, and what Typst's `@ref` renders.
fn retarget_number_references(blocks: &mut [Block], emptied: &[EcoString]) {
    if emptied.is_empty() {
        return;
    }
    visit_field_instrs(blocks, &mut |instr: &mut EcoString| {
        if let Some(name) =
            instr.strip_prefix(" REF ").and_then(|r| r.strip_suffix(" \\h "))
            && emptied.iter().any(|emptied| emptied == name)
        {
            *instr = eco_format!(" REF {name} \\w \\h ");
        }
    });
}

/// Visits every field instruction in a block tree, including the ones nested in
/// a field's own cached result.
fn visit_field_instrs(blocks: &mut [Block], f: &mut dyn FnMut(&mut EcoString)) {
    for block in blocks {
        match block {
            Block::Para(para) => visit_para_field_instrs(para, f),
            Block::Toc(toc) => {
                for entry in &mut toc.entries {
                    visit_para_field_instrs(entry, f);
                }
                visit_run_field_instrs(&mut toc.fallback, f);
            }
            Block::Table(tbl) => {
                for row in &mut tbl.rows {
                    for cell in &mut row.cells {
                        visit_field_instrs(&mut cell.blocks, f);
                    }
                }
            }
            _ => {}
        }
    }
}

fn visit_para_field_instrs(para: &mut Para, f: &mut dyn FnMut(&mut EcoString)) {
    for child in &mut para.content {
        match child {
            ParaChild::Run(run) => visit_run_field_instrs(std::slice::from_mut(run), f),
            ParaChild::Hyperlink { runs, .. } => visit_run_field_instrs(runs, f),
            _ => {}
        }
    }
}

fn visit_run_field_instrs(runs: &mut [Run], f: &mut dyn FnMut(&mut EcoString)) {
    for run in runs {
        if let Run::Field(field) = run {
            f(&mut field.instr);
            visit_run_field_instrs(&mut field.result, f);
        }
    }
}

/// Whether anything in this block reads a heading number out of its
/// `TypstHeadingNumber{N}` run.
fn uses_heading_number_styleref(block: &Block) -> bool {
    let mut found = false;
    for_each_para(std::slice::from_ref(block), &mut |para| {
        for child in &para.content {
            let runs: &[Run] = match child {
                ParaChild::Run(run) => std::slice::from_ref(run),
                ParaChild::Hyperlink { runs, .. } => runs,
                _ => continue,
            };
            found |= runs_use_heading_number_styleref(runs);
        }
    });
    found
}

fn runs_use_heading_number_styleref(runs: &[Run]) -> bool {
    runs.iter().any(|run| match run {
        Run::Field(field) => {
            field.instr.contains("STYLEREF TypstHeadingNumber")
                || runs_use_heading_number_styleref(&field.result)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(fmt: NumFmt, lvl_text: &str) -> ListLevel {
        ListLevel {
            num_fmt: fmt,
            lvl_text: lvl_text.into(),
            start: 1,
            ind_left: 0,
            ind_hanging: 0,
            bullet_font: None,
            pstyle: None,
        }
    }

    #[test]
    fn levels_render_each_component_in_its_own_format() {
        let levels = [
            level(NumFmt::UpperLetter, "%1"),
            level(NumFmt::Decimal, "%1.%2"),
            level(NumFmt::LowerRoman, "%1.%2.%3"),
        ];
        assert_eq!(render(&levels, 0, &[3, 0, 0]).unwrap(), "C");
        assert_eq!(render(&levels, 1, &[3, 2, 0]).unwrap(), "C.2");
        assert_eq!(render(&levels, 2, &[3, 2, 4]).unwrap(), "C.2.iv");
    }

    #[test]
    fn a_skipped_level_renders_as_word_does() {
        let levels = [level(NumFmt::Decimal, "%1"), level(NumFmt::Decimal, "%1.%2")];
        assert_eq!(render(&levels, 1, &[0, 1]).unwrap(), "0.1");
        // A roman or alphabetic level has no zero, so there is nothing that
        // could agree with Typst's rendering.
        let alpha = [level(NumFmt::UpperLetter, "%1"), level(NumFmt::Decimal, "%1.%2")];
        assert!(render(&alpha, 1, &[0, 1]).is_none());
    }

    #[test]
    fn word_alphabetic_numbering_repeats_rather_than_carries() {
        assert_eq!(letter(1, 'a').unwrap(), "a");
        assert_eq!(letter(26, 'A').unwrap(), "Z");
        assert_eq!(letter(27, 'a').unwrap(), "aa");
        assert_eq!(letter(53, 'a').unwrap(), "aaa");
    }

    #[test]
    fn roman_numerals_match_the_shared_range() {
        assert_eq!(roman(4).unwrap(), "IV");
        assert_eq!(roman(1990).unwrap(), "MCMXC");
        assert!(roman(4000).is_none());
    }
}

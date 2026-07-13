//! List / enum / terms mappers (`numbering_lists.md` §3,4,5,9).
//!
//! Strategy. OOXML separates a list's *shape* (`w:abstractNum`, up to 9 levels)
//! from its *instance* (`w:num`/`numId`); a paragraph joins a list by carrying
//! `w:pPr/w:numPr = { w:ilvl, w:numId }`. A single Typst `ListElem`/`EnumElem`
//! describes exactly *one* nesting level — deeper levels arrive lazily when we
//! re-realize an item body through [`DocxCtx::blocks`], which routes the nested
//! list straight back into these handlers. We therefore:
//!
//! * register a full 9-level shape (so the nested-level indents/formats are
//!   already present in the `abstractNum`, and identical shapes dedup), and
//! * read the live nesting depth from the style chain — `ListElem::depth` for
//!   bullets and `EnumElem::parents.len()` for enums (exactly the values the
//!   layout code folds in, see `typst-layout/src/lists.rs`) — to pick `w:ilvl`.
//!
//! Each top-level list/enum gets its own `numId` (independent counter); nested
//! lists get their own `numId` too, which is correct for Typst semantics (a
//! nested `enum` is a *separate* enumeration that restarts) while sharing the
//! deduped `abstractNum` so the per-level indent/format are consistent.
//!
//! `terms` has no native OOXML analogue: we emit a bold-term paragraph followed
//! by a hanging-indented description paragraph, never going through numbering.

use comemo::Track;
use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Context, Depth, Packed, Resolve, StyleChain};
use typst_library::layout::Abs;
use typst_library::model::{
    EnumElem, EnumItem, ListElem, NamedNumeralSystem, Numbering, NumberingPattern,
    ParElem, TermsElem,
};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Indent, ListLevel, ListSpec, MultiLevelType, NumFmt, Para, ParaChild,
    ParaProps, ReviewCandidateKind, Run, RunProps,
};

/// The `w:pStyle` applied to every list/enum item paragraph.
const LIST_PARAGRAPH: &str = "ListParagraph";

/// The conventional bullet glyph for each nesting level (cycled past level 2),
/// mirroring Typst's default `•`/`‣`/`–` marker cycle. Plain Unicode glyphs in a
/// normal font are accepted by Word (`numbering_lists.md` §3.3); we avoid the
/// Symbol-font PUA route so no font metadata is required.
const BULLET_GLYPHS: [&str; 3] = ["\u{2022}", "\u{2023}", "\u{2013}"];

/// Twips per nesting level for the text-start indent (Word's stock 0.5"/level).
const LEVEL_INDENT_TWIPS: i32 = 720;
/// Hanging indent (the marker gutter): 0.25".
const HANGING_TWIPS: i32 = 360;

// ===========================================================================
// Bullet lists.
// ===========================================================================

/// Lowers a `ListElem` (bullets) into `ListParagraph` items carrying `w:numPr`.
pub fn list(
    elem: &Packed<ListElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    // The live nesting depth folded in by the layout machinery: 0 at the
    // outermost list, +1 per enclosing list item body.
    let Depth(depth) = styles.get(ListElem::depth);
    let ilvl = depth.min(8) as u8;
    let paragraph_spacing = crate::props::abs_to_twip(styles.resolve(ParElem::spacing));

    let num_id = ctx.register_list(bullet_spec());

    let mut out = Vec::new();
    for item in &elem.children {
        emit_item(ctx, &item.body, styles, num_id, ilvl, paragraph_spacing, &mut out)?;
    }
    if ilvl == 0 {
        apply_list_boundary_spacing(&mut out, paragraph_spacing);
    }
    Ok(out)
}

/// Builds the 9-level bullet shape (one `abstractNum`, `hybridMultilevel`).
fn bullet_spec() -> ListSpec {
    let levels = (0..9u8)
        .map(|i| ListLevel {
            num_fmt: NumFmt::Bullet,
            // For `numFmt="bullet"` the level text is taken literally.
            lvl_text: BULLET_GLYPHS[(i as usize) % BULLET_GLYPHS.len()].into(),
            start: 1,
            ind_left: LEVEL_INDENT_TWIPS * (i as i32 + 1),
            ind_hanging: HANGING_TWIPS,
            bullet_font: None,
        })
        .collect();
    ListSpec {
        levels,
        multilevel: MultiLevelType::HybridMultilevel,
        restart_at_1: false,
    }
}

// ===========================================================================
// Enumerations.
// ===========================================================================

/// Lowers an `EnumElem` into `ListParagraph` items carrying `w:numPr`, honoring
/// the numbering pattern, `start`, per-item `number`, and `reversed`.
pub fn enum_(
    elem: &Packed<EnumElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    // Nesting depth: the number of parent enum numbers folded in so far.
    let parents = styles.get_cloned(EnumElem::parents);
    let ilvl = parents.len().min(8) as u8;
    let paragraph_spacing = crate::props::abs_to_twip(styles.resolve(ParElem::spacing));

    let numbering = elem.numbering.get_ref(styles);
    let reversed = elem.reversed.get(styles);
    let start = elem.start.get(styles);
    let full = elem.full.get(styles);

    // Resolve the per-level number format + marker template from the pattern.
    // For a closure (`numbering: n => ...`) or any pattern Word cannot express
    // natively, fall back to baking the marker as literal run text (§9.4).
    //
    // `reversed` and explicit per-item numbers also cannot be expressed by
    // Word's monotonic counter, so they take the static path too.
    let has_explicit_numbers =
        elem.children.iter().any(|c| c.number.get(styles).is_custom());
    let levels = if reversed || has_explicit_numbers {
        None
    } else {
        native_enum_levels(numbering, full)
    };
    let Some(mut spec_levels) = levels else {
        return enum_static_fallback(elem, styles, ctx, paragraph_spacing, ilvl);
    };

    // The starting value of this enumeration.
    let start_value = start.unwrap_or(1);
    if let Some(level) = spec_levels.get_mut(ilvl as usize) {
        level.start = start_value;
    }
    // Every Typst `enum` is an independent counter that restarts, so always
    // request a fresh `numId`. `register_list` honors this via `restart_at_1`
    // (a `w:num` with its own `w:lvlOverride/w:startOverride`), so two enums
    // sharing a shape never continue one another's count. The override value is
    // taken from the spec level's `start` (set above) by the numbering writer.
    let spec = ListSpec {
        levels: spec_levels,
        multilevel: MultiLevelType::Multilevel,
        restart_at_1: true,
    };
    let num_id = ctx.register_list(spec);

    let mut out = Vec::new();
    let mut number = start_value;
    for item in &elem.children {
        let item_body = item
            .body
            .clone()
            .set(EnumElem::parents, core::iter::once(number).collect());
        emit_item(ctx, &item_body, styles, num_id, ilvl, paragraph_spacing, &mut out)?;
        number = number.saturating_add(1);
    }
    if ilvl == 0 {
        apply_list_boundary_spacing(&mut out, paragraph_spacing);
    }
    Ok(out)
}

/// Builds the 9-level numeric shape from a numbering *pattern*, or returns
/// `None` if the numbering is a closure / not natively representable, in which
/// case the caller uses the static-text fallback.
///
/// When `full` is set, level `k` shows the full ancestry (`%1.%2.…`); when not,
/// each level shows only its own counter (`%k+1`), matching Typst's `full:
/// false` default (`numbering_lists.md` §9.2).
fn native_enum_levels(numbering: &Numbering, full: bool) -> Option<Vec<ListLevel>> {
    let Numbering::Pattern(pattern) = numbering else {
        return None;
    };
    if pattern.pieces() == 0 {
        return None;
    }

    let last = pattern.pieces.last()?;
    let mut levels = Vec::with_capacity(9);
    for i in 0..9 {
        let (_, system) = pattern.pieces.get(i).unwrap_or(last);
        let num_fmt = native_num_fmt(*system)?;
        let lvl_text = if full {
            full_level_text(pattern, i)
        } else {
            single_level_text(pattern, i)
        };
        levels.push(ListLevel {
            num_fmt,
            lvl_text,
            start: 1,
            ind_left: LEVEL_INDENT_TWIPS * (i as i32 + 1),
            ind_hanging: HANGING_TWIPS,
            bullet_font: None,
        });
    }
    Some(levels)
}

fn native_num_fmt(system: NamedNumeralSystem) -> Option<NumFmt> {
    let one = system.system().represent(1).ok()?.to_string();
    let four = system.system().represent(4).ok()?.to_string();
    Some(match (one.as_str(), four.as_str()) {
        ("1", "4") => NumFmt::Decimal,
        ("a", "d") => NumFmt::LowerLetter,
        ("A", "D") => NumFmt::UpperLetter,
        ("i", "iv") => NumFmt::LowerRoman,
        ("I", "IV") => NumFmt::UpperRoman,
        _ => return None,
    })
}

fn full_level_text(pattern: &NumberingPattern, level: usize) -> EcoString {
    let mut text = EcoString::new();
    for i in 0..=level {
        text.push_str(full_level_prefix(pattern, i));
        text.push('%');
        text.push_str(&(i + 1).to_string());
    }
    text.push_str(&pattern.suffix);
    text
}

fn full_level_prefix(pattern: &NumberingPattern, level: usize) -> &str {
    if let Some((prefix, _)) = pattern.pieces.get(level) {
        prefix.as_str()
    } else if let Some((prefix, _)) = pattern.pieces.last() {
        if prefix.is_empty() { pattern.suffix.as_str() } else { prefix.as_str() }
    } else {
        ""
    }
}

fn single_level_text(pattern: &NumberingPattern, level: usize) -> EcoString {
    let mut text = EcoString::new();
    if let Some((prefix, _)) = pattern.pieces.first() {
        text.push_str(prefix);
    }
    text.push('%');
    text.push_str(&(level + 1).to_string());
    text.push_str(&pattern.suffix);
    text
}

/// Renders an enum's items as plain paragraphs with the Typst-computed marker
/// baked in as literal leading text and a hanging indent — used for closures,
/// reversed counters, explicit per-item numbers, and non-native formats
/// (`numbering_lists.md` §9.4). No `w:numPr` is emitted so Word does not
/// auto-number.
fn enum_static_fallback(
    elem: &Packed<EnumElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
    paragraph_spacing: i32,
    ilvl: u8,
) -> SourceResult<Vec<Block>> {
    let parents = styles.get_cloned(EnumElem::parents);
    let ind_left = LEVEL_INDENT_TWIPS * (ilvl as i32 + 1);

    let numbering = elem.numbering.get_ref(styles).clone();
    let reversed = elem.reversed.get(styles);
    let full = elem.full.get(styles);

    let mut number = elem.start.get(styles).unwrap_or(if reversed {
        elem.children.len() as u64
    } else {
        1
    });

    let mut out = Vec::new();
    for item in &elem.children {
        number = item.number.get(styles).unwrap_or(number);

        let marker =
            render_marker(ctx, styles, &numbering, &parents, number, full, item)?;
        let ind = Indent {
            left: Some(ind_left),
            right: None,
            first_line: None,
            hanging: Some(HANGING_TWIPS),
        };
        // Fold this item's number into `EnumElem::parents` on the body (as the
        // layout pipeline does), so a NESTED enum sees the full ancestry — for its
        // indent level and for `full: true` numbers like `1.2.`. Without it every
        // nested enum collapses to level 0 with a flat number.
        let item_body = item
            .body
            .clone()
            .set(EnumElem::parents, core::iter::once(number).collect());
        emit_static_marker_item(
            ctx,
            &item_body,
            styles,
            marker,
            ind,
            paragraph_spacing,
            &mut out,
        )?;

        number =
            if reversed { number.saturating_sub(1) } else { number.saturating_add(1) };
    }
    if ilvl == 0 {
        apply_list_boundary_spacing(&mut out, paragraph_spacing);
    }
    Ok(out)
}

/// Renders the literal marker text for one enum item, mirroring the layout
/// code's pattern application (`typst-layout/src/lists.rs`).
fn render_marker(
    ctx: &mut DocxCtx,
    styles: StyleChain,
    numbering: &Numbering,
    parents: &[u64],
    number: u64,
    full: bool,
    item: &Packed<EnumItem>,
) -> SourceResult<EcoString> {
    let span = item.span();
    let engine = ctx.engine();
    if full {
        let mut nums: Vec<u64> = parents.to_vec();
        nums.push(number);
        let context = Context::new(None, Some(styles));
        let value = numbering.apply(engine, context.track(), span, &nums)?;
        Ok(value.display().plain_text())
    } else {
        match numbering {
            Numbering::Pattern(pattern) => {
                Ok(pattern.apply_kth(engine, span, parents.len(), number))
            }
            other => {
                let context = Context::new(None, Some(styles));
                let value = other.apply(engine, context.track(), span, &[number])?;
                Ok(value.display().plain_text())
            }
        }
    }
}

// ===========================================================================
// Term lists (definition lists).
// ===========================================================================

/// Lowers a `TermsElem` into a bold-term paragraph followed by a hanging-indent
/// description paragraph per item. No numbering is used (`numbering_lists.md`
/// §9.3).
pub fn terms(
    elem: &Packed<TermsElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    // The description hang: the term's own indent plus the configured hanging
    // indent, converted to twips.
    let indent: Abs = elem.indent.get(styles).resolve(styles);
    let hanging: Abs = elem.hanging_indent.get(styles).resolve(styles);
    let term_left = crate::props::abs_to_twip(indent);
    let body_left = term_left + crate::props::abs_to_twip(hanging);

    let hanging = (body_left - term_left).max(0);
    let mut out = Vec::new();
    for item in &elem.children {
        let term_props = RunProps { bold: true, ..RunProps::default() };
        let term_runs = ctx.inline_runs(&item.term, styles, term_props)?;
        // Description: re-realized as full blocks (it may contain paragraphs,
        // nested lists, etc.).
        let mut desc = ctx.blocks(&item.description, styles)?;

        if let Some(Block::Para(first)) = desc.first_mut() {
            // Merge the bold term onto the first description line with a hanging
            // indent, matching Typst's "**term** description" rendering (the term
            // leads, a tab jumps to the hanging stop, the description follows and
            // wraps aligned under itself). Continuation paragraphs align there too.
            let mut content: Vec<ParaChild> =
                term_runs.into_iter().map(ParaChild::Run).collect();
            content.push(ParaChild::Run(Run::Tab));
            content.append(&mut first.content);
            first.content = content;
            first.props.ind = Some(Indent {
                left: Some(body_left),
                right: None,
                first_line: None,
                hanging: Some(hanging),
            });
            apply_left_indent(&mut desc[1..], body_left);
            out.append(&mut desc);
        } else {
            // No paragraph to merge onto (empty or block-only description): keep
            // the term on its own line, then the description indented below.
            let mut term_para_props = ParaProps::default();
            if term_left != 0 {
                term_para_props.ind = Some(Indent {
                    left: Some(term_left),
                    right: None,
                    first_line: None,
                    hanging: None,
                });
            }
            out.push(Block::Para(Para {
                props: term_para_props,
                content: term_runs.into_iter().map(ParaChild::Run).collect(),
            }));
            apply_left_indent(&mut desc, body_left);
            out.extend(desc);
        }
    }
    Ok(out)
}

// ===========================================================================
// Shared item emission.
// ===========================================================================

/// Emits one list/enum item: its body re-realized to blocks, with the first
/// paragraph carrying the `ListParagraph` style + `w:numPr`, and any following
/// paragraphs of the same item indented to align under the body (no marker).
fn emit_item(
    ctx: &mut DocxCtx,
    body: &Content,
    styles: StyleChain,
    num_id: u32,
    ilvl: u8,
    paragraph_spacing: i32,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    let review_origin = ctx
        .review_origin(crate::convert::review_span(body), ReviewCandidateKind::ListItem);
    // Fold `ListElem::depth += 1` onto the item body (exactly as the layout
    // pipeline does, `typst-layout/src/lists.rs`), so a list NESTED inside this
    // item sees the incremented depth and indents one level deeper. Without it
    // every nested bullet list collapses back to level 0.
    let body = body.clone().set(ListElem::depth, Depth(1));
    let blocks = ctx.blocks(&body, styles)?;
    let body_indent = LEVEL_INDENT_TWIPS * (ilvl as i32 + 1);

    let mut numbered = false;
    for block in blocks {
        match block {
            Block::Para(mut para) => {
                if !numbered && para.props.num.is_none() {
                    // The marker-bearing paragraph. (Guarded by `num.is_none()`
                    // so a nested list's own already-numbered paragraph that
                    // surfaces here is never hijacked.)
                    para.props.style = Some(LIST_PARAGRAPH.into());
                    para.props.num = Some((num_id, ilvl));
                    para.props.review_origin = Some(review_origin);
                    strip_inherited_item_spacing(&mut para, paragraph_spacing);
                    numbered = true;
                } else if para.props.num.is_none() {
                    // A continuation paragraph in the same item: keep it inside
                    // the list visually via a matching left indent, but do not
                    // re-emit the marker.
                    if para.props.style.is_none() {
                        para.props.style = Some(LIST_PARAGRAPH.into());
                    }
                    if para.props.ind.is_none() {
                        para.props.ind = Some(Indent {
                            left: Some(body_indent),
                            right: None,
                            first_line: None,
                            hanging: None,
                        });
                    }
                }
                out.push(Block::Para(para));
            }
            // Non-paragraph blocks (tables, tags) pass through unchanged. A
            // nested list/enum has, by this point, already been recursively
            // lowered into its own numbered paragraphs with a deeper `ilvl`.
            other => out.push(other),
        }
    }

    // An empty item still needs a marker paragraph so the bullet/number shows.
    if !numbered {
        let props = ParaProps {
            style: Some(LIST_PARAGRAPH.into()),
            num: Some((num_id, ilvl)),
            ..Default::default()
        };
        out.push(Block::Para(Para {
            props,
            content: vec![ParaChild::Run(Run::Text {
                props: RunProps::default(),
                text: "".into(),
            })],
        }));
    }
    Ok(())
}

/// Emits an item whose marker is baked as literal leading run text (static
/// fallback): the marker run, a tab, then the item body inline.
fn emit_static_marker_item(
    ctx: &mut DocxCtx,
    body: &Content,
    styles: StyleChain,
    marker: EcoString,
    ind: Indent,
    paragraph_spacing: i32,
    out: &mut Vec<Block>,
) -> SourceResult<()> {
    let blocks = ctx.blocks(body, styles)?;

    // The marker (+ tab) prepended to the first paragraph's runs. `Run` is not
    // `Clone`, so build it fresh wherever it is consumed.
    let make_prefix = |marker: EcoString| -> Vec<ParaChild> {
        vec![
            ParaChild::Run(Run::Text { props: RunProps::default(), text: marker }),
            ParaChild::Run(Run::Tab),
        ]
    };

    let mut emitted_marker = false;
    let mut marker = Some(marker);
    for block in blocks {
        match block {
            Block::Para(mut para) if !emitted_marker => {
                let mut content = make_prefix(marker.take().unwrap_or_default());
                content.append(&mut para.content);
                para.content = content;
                if para.props.ind.is_none() {
                    para.props.ind = Some(ind.clone());
                }
                strip_inherited_item_spacing(&mut para, paragraph_spacing);
                out.push(Block::Para(para));
                emitted_marker = true;
            }
            other => out.push(other),
        }
    }

    if !emitted_marker {
        // Empty body: still emit the marker line.
        out.push(Block::Para(Para {
            props: ParaProps { ind: Some(ind), ..ParaProps::default() },
            content: make_prefix(marker.take().unwrap_or_default()),
        }));
    }
    Ok(())
}

/// A list owns the vertical rhythm between its item frames. A paragraph
/// realized inside the first item can otherwise inherit `par.spacing` and put
/// that full gap both before and after only that marker, making nested lists
/// jump while their siblings remain tight. Remove only values equal to the
/// inherited paragraph spacing; explicit line-height and other spacing survive.
fn strip_inherited_item_spacing(para: &mut Para, paragraph_spacing: i32) {
    let Some(spacing) = &mut para.props.spacing else { return };
    if spacing.before == Some(paragraph_spacing) {
        spacing.before = None;
    }
    if spacing.after == Some(paragraph_spacing) {
        spacing.after = None;
    }
    if spacing.before.is_none()
        && spacing.after.is_none()
        && spacing.line.is_none()
        && !spacing.line_rule_auto
        && !spacing.line_rule_at_least
    {
        para.props.spacing = None;
    }
}

/// Typst still separates a top-level list block from the following block with
/// normal paragraph spacing. Put that gap on the final list paragraph, where
/// Word's adjacent-spacing collapse can combine it with whatever follows.
/// Nested lists do not receive this boundary gap; their parent list owns the
/// surrounding item rhythm.
fn apply_list_boundary_spacing(blocks: &mut [Block], paragraph_spacing: i32) {
    if paragraph_spacing == 0 {
        return;
    }
    let Some(para) = blocks.iter_mut().rev().find_map(|block| match block {
        Block::Para(para) => Some(para),
        _ => None,
    }) else {
        return;
    };
    let spacing = para.props.spacing.get_or_insert_with(Default::default);
    spacing.after = Some(spacing.after.unwrap_or(0).max(paragraph_spacing));
}

/// Applies a left indent to every paragraph in a block list (used to push a
/// term description under its term).
fn apply_left_indent(blocks: &mut [Block], left: i32) {
    if left == 0 {
        return;
    }
    for block in blocks {
        if let Block::Para(para) = block
            && para.props.ind.is_none()
            && para.props.num.is_none()
        {
            para.props.ind = Some(Indent {
                left: Some(left),
                right: None,
                first_line: None,
                hanging: None,
            });
        }
    }
}

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
    EnumElem, EnumItem, ListElem, Numbering, TermsElem,
};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Indent, ListLevel, ListSpec, MultiLevelType, NumFmt, Para, ParaChild,
    ParaProps, Run, RunProps,
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

    let num_id = ctx.register_list(bullet_spec());

    let mut out = Vec::new();
    for item in &elem.children {
        emit_item(ctx, &item.body, styles, num_id, ilvl, &mut out)?;
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
    ListSpec { levels, multilevel: MultiLevelType::HybridMultilevel, restart_at_1: false }
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
        return enum_static_fallback(elem, styles, ctx);
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
    for item in &elem.children {
        emit_item(ctx, &item.body, styles, num_id, ilvl, &mut out)?;
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

    // INTEGRATION-NEEDED: numeral-system → `w:numFmt` classification.
    //
    // To emit a native `w:numFmt`/`w:lvlText` per level we must read each
    // counting symbol's numeral system off `pattern.pieces` (an
    // `EcoVec<(EcoString /*prefix*/, NamedNumeralSystem)>`) and the trailing
    // `pattern.suffix`. But `NamedNumeralSystem` (from the `codex` crate) is NOT
    // re-exported through `typst-library`, and this module may not add a `codex`
    // dependency to `Cargo.toml` nor edit shared crates. So the per-piece
    // format cannot be determined here yet, and we return `None` to take the
    // always-correct static-text fallback (which prints the exact Typst-rendered
    // numbers, only losing Word-side live re-numbering).
    //
    // One-line fix for integration: re-export `NamedNumeralSystem` from
    // `typst_library::model`, then implement `native_enum_levels` as below
    // (sketch — engine-free, classifies by the system's representation of 1 and
    // 4, which disambiguates every closed-enum family):
    //
    //   for i in 0..9 {
    //       let last = pattern.pieces.last()?;
    //       let (prefix, system) = pattern.pieces.get(i).unwrap_or(last);
    //       let fmt = match (
    //           system.system().represent(1).ok()?.to_string().as_str(),
    //           system.system().represent(4).ok()?.to_string().as_str(),
    //       ) {
    //           ("1", "4")  => NumFmt::Decimal,
    //           ("a", "d")  => NumFmt::LowerLetter,
    //           ("A", "D")  => NumFmt::UpperLetter,
    //           ("i", "iv") => NumFmt::LowerRoman,
    //           ("I", "IV") => NumFmt::UpperRoman,
    //           _ => return None, // symbol/CJK/abjad: no native numFmt
    //       };
    //       // lvl_text = prefix + (full ? "%1.…%{i+1}" : "%{i+1}") + suffix
    //       //   where suffix = pattern.suffix iff this is the last piece.
    //       levels.push(ListLevel { num_fmt: fmt, lvl_text, start: 1,
    //           ind_left: LEVEL_INDENT_TWIPS*(i as i32+1), ind_hanging: HANGING_TWIPS,
    //           bullet_font: None });
    //   }
    //
    // Alternatively, add a `DocxCtx::enum_marker_kth(pattern, k, n)` helper that
    // calls `pattern.apply_kth(engine, span, k, n)` (engine in hand) so the
    // classification runs off rendered strings without naming `codex`.
    let _ = (pattern, full);
    None
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
) -> SourceResult<Vec<Block>> {
    let parents = styles.get_cloned(EnumElem::parents);
    let ilvl = parents.len().min(8) as u8;
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
        let item_body =
            item.body.clone().set(EnumElem::parents, core::iter::once(number).collect());
        emit_static_marker_item(ctx, &item_body, styles, marker, ind, &mut out)?;

        number = if reversed {
            number.saturating_sub(1)
        } else {
            number.saturating_add(1)
        };
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
    out: &mut Vec<Block>,
) -> SourceResult<()> {
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
        let mut props = ParaProps::default();
        props.style = Some(LIST_PARAGRAPH.into());
        props.num = Some((num_id, ilvl));
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

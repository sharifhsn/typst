//! Resolve a run/paragraph's *effective* properties by walking `basedOn`
//! chains + docDefaults + direct formatting.

use crate::wml::model::{ParaProps, RunProps, Style, Styles};

/// Maximum `basedOn` hops to walk before giving up — guards against a cyclic
/// style chain (which would otherwise loop forever).
const MAX_BASED_ON_HOPS: usize = 32;

/// The `basedOn` chain for `id`: root ancestor first, `id`'s own style last.
/// A dangling `basedOn` reference (or a style id with no explicit definition,
/// e.g. the implicit "Normal") simply ends the chain early.
fn style_chain<'a>(styles: &'a Styles, id: &str) -> Vec<&'a Style> {
    let mut chain = Vec::new();
    let mut current = styles.by_id.get(id);
    let mut hops = 0;
    while let Some(style) = current {
        chain.push(style);
        hops += 1;
        if hops >= MAX_BASED_ON_HOPS {
            break;
        }
        current = style.based_on.as_deref().and_then(|parent| styles.by_id.get(parent));
    }
    chain.reverse();
    chain
}

/// Layer `over`'s explicit fields on top of `base`: a `Some` value in `over`
/// wins, otherwise `base`'s value carries through.
fn merge_run(base: &RunProps, over: &RunProps) -> RunProps {
    RunProps {
        style_id: over.style_id.clone().or_else(|| base.style_id.clone()),
        bold: over.bold.or(base.bold),
        italic: over.italic.or(base.italic),
        strike: over.strike.or(base.strike),
        smallcaps: over.smallcaps.or(base.smallcaps),
        underline: over.underline.clone().or_else(|| base.underline.clone()),
        color: over.color.clone().or_else(|| base.color.clone()),
        size_half_pt: over.size_half_pt.or(base.size_half_pt),
        font: over.font.clone().or_else(|| base.font.clone()),
        vert_align: over.vert_align.clone().or_else(|| base.vert_align.clone()),
        vanish: over.vanish.or(base.vanish),
    }
}

/// Layer `over`'s explicit fields on top of `base`, the [`ParaProps`]
/// counterpart of [`merge_run`]. `bottom_border` has no "unset" state in the
/// model (it's a plain `bool`, not a `Toggle`), so it's OR'd: a border
/// inherited from a style isn't clearable by a paragraph that simply doesn't
/// mention one. `sect_pr` is never meaningfully set on a *style's* `pPr` (a
/// section boundary is document-instance data, not a formatting template), so
/// it follows the same "direct wins" rule as every other field here purely
/// for mechanical consistency — nothing actually reads it off the resolved,
/// effective properties this function produces (see [`ParaProps::sect_pr`]'s
/// own doc comment for who does).
fn merge_para(base: &ParaProps, over: &ParaProps) -> ParaProps {
    ParaProps {
        style_id: over.style_id.clone().or_else(|| base.style_id.clone()),
        jc: over.jc.clone().or_else(|| base.jc.clone()),
        num: over.num.or(base.num),
        spacing_before: over.spacing_before.or(base.spacing_before),
        line: over.line.or(base.line),
        indent_left: over.indent_left.or(base.indent_left),
        mark_props: merge_run(&base.mark_props, &over.mark_props),
        bottom_border: base.bottom_border || over.bottom_border,
        sect_pr: over.sect_pr.clone().or_else(|| base.sect_pr.clone()),
    }
}

/// Effective run properties: direct `rPr` over character-style over
/// paragraph-style-run over docDefaults.
pub fn effective_run(styles: &Styles, para_style_id: Option<&str>, direct: &RunProps) -> RunProps {
    let mut acc = styles.default_run.clone();

    if let Some(pid) = para_style_id {
        for style in style_chain(styles, pid) {
            acc = merge_run(&acc, &style.run);
        }
    }

    if let Some(cid) = direct.style_id.as_deref() {
        for style in style_chain(styles, cid) {
            acc = merge_run(&acc, &style.run);
        }
    }

    merge_run(&acc, direct)
}

/// Effective paragraph properties: direct `pPr` over paragraph-style (with
/// `basedOn`) over docDefaults.
pub fn effective_para(styles: &Styles, direct: &ParaProps) -> ParaProps {
    let mut acc = styles.default_para.clone();

    if let Some(pid) = direct.style_id.as_deref() {
        for style in style_chain(styles, pid) {
            acc = merge_para(&acc, &style.para);
        }
    }

    merge_para(&acc, direct)
}

/// The 1..=6 heading level for a paragraph style id, if it (or an ancestor in
/// its `basedOn` chain) is a heading style: either `w:outlineLvl` is set, or
/// the style's name/id matches "Heading N" / "HeadingN" (case-insensitive).
/// The most specific style in the chain wins.
pub fn heading_level(styles: &Styles, para_style_id: Option<&str>) -> Option<u8> {
    let pid = para_style_id?;
    for style in style_chain(styles, pid).into_iter().rev() {
        if let Some(level) = style.outline_level {
            return Some(level.saturating_add(1).min(6));
        }
        if let Some(n) = style.name.as_deref().and_then(parse_heading_number) {
            return Some(n.clamp(1, 6));
        }
        if let Some(n) = parse_heading_number(&style.id) {
            return Some(n.clamp(1, 6));
        }
    }
    None
}

/// Parse a "Heading N" / "heading N" / "HeadingN" style name or id into its
/// numeral, if it matches that pattern exactly (no trailing junk after the
/// digits).
fn parse_heading_number(s: &str) -> Option<u8> {
    let lower = s.to_ascii_lowercase();
    let rest = lower.strip_prefix("heading")?.trim();
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

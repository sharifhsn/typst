//! Resolve a run/paragraph's *effective* properties by walking `basedOn`
//! chains + docDefaults + direct formatting.

use crate::wml::model::{
    BorderEdge, Borders, CellMargins, ParaProps, RunProps, Style, StyleKind, Styles,
    Table, TableBorders, TableStyleProps,
};

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
        dstrike: over.dstrike.or(base.dstrike),
        smallcaps: over.smallcaps.or(base.smallcaps),
        caps: over.caps.or(base.caps),
        underline: over.underline.clone().or_else(|| base.underline.clone()),
        underline_color: over
            .underline_color
            .clone()
            .or_else(|| base.underline_color.clone()),
        highlight: over.highlight.clone().or_else(|| base.highlight.clone()),
        color: over.color.clone().or_else(|| base.color.clone()),
        size_half_pt: over.size_half_pt.or(base.size_half_pt),
        letter_spacing: over.letter_spacing.or(base.letter_spacing),
        font: over.font.clone().or_else(|| base.font.clone()),
        lang: over.lang.clone().or_else(|| base.lang.clone()),
        vert_align: over.vert_align.clone().or_else(|| base.vert_align.clone()),
        rtl: over.rtl.or(base.rtl),
        vanish: over.vanish.or(base.vanish),
    }
}

/// Layer `over`'s explicit fields on top of `base`, the [`ParaProps`]
/// counterpart of [`merge_run`]. `borders` resolves per side, so a paragraph
/// that adds a box around itself doesn't discard the rule its style already
/// drew underneath — and one that mentions no border at all keeps the style's
/// entirely. `sect_pr` is never meaningfully set on a *style's* `pPr` (a
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
        spacing_after: over.spacing_after.or(base.spacing_after),
        line: over.line.or(base.line),
        indent_left: over.indent_left.or(base.indent_left),
        indent_right: over.indent_right.or(base.indent_right),
        indent_first_line: over.indent_first_line.or(base.indent_first_line),
        indent_hanging: over.indent_hanging.or(base.indent_hanging),
        shd_fill: over.shd_fill.clone().or_else(|| base.shd_fill.clone()),
        mark_props: merge_run(&base.mark_props, &over.mark_props),
        borders: merge_borders(&base.borders, &over.borders),
        format_revision: base.format_revision || over.format_revision,
        keep_lines: over.keep_lines.or(base.keep_lines),
        keep_next: over.keep_next.or(base.keep_next),
        sect_pr: over.sect_pr.clone().or_else(|| base.sect_pr.clone()),
    }
}

/// Resolve two `w:pBdr`s side by side: a side the paragraph states wins, a
/// side it doesn't keeps whatever the style chain already had there.
fn merge_borders(base: &Borders, over: &Borders) -> Borders {
    let side = |over: &Option<BorderEdge>, base: &Option<BorderEdge>| {
        over.clone().or(base.clone())
    };
    Borders {
        top: side(&over.top, &base.top),
        bottom: side(&over.bottom, &base.bottom),
        left: side(&over.left, &base.left),
        right: side(&over.right, &base.right),
    }
}

/// Effective run properties: direct `rPr` over character-style over
/// paragraph-style-run over docDefaults.
pub fn effective_run(
    styles: &Styles,
    para_style_id: Option<&str>,
    direct: &RunProps,
) -> RunProps {
    let mut acc = styles.default_run.clone();

    if let Some(pid) = para_style_id {
        for style in style_chain(styles, pid) {
            // A *linked* style is one style Word lets you apply either as a
            // paragraph style or as a character style, and its run formatting
            // may sit in either half. Resolving the halves independently
            // therefore loses that formatting entirely whenever the paragraph
            // half is the empty one — the case for 740 styles across the wide
            // corpus, among them the `Header`/`Footer` pairs Word itself
            // writes. So the linked character style is merged in underneath
            // the paragraph style's own run properties, which still win.
            acc = merge_run(&acc, &linked_run(styles, style));
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

/// The run properties of `style`'s linked character style, if it has one.
///
/// Followed for exactly one hop and only from a paragraph style to a
/// character style. The link is symmetric — each half names the other — so
/// following it further, or in the other direction, would bounce between the
/// two forever; and a character style's own `basedOn` chain is already walked
/// when that style is applied directly through `w:rStyle`.
fn linked_run(styles: &Styles, style: &Style) -> RunProps {
    let Some(link) = style.link.as_deref() else { return RunProps::default() };
    match styles.by_id.get(link) {
        Some(linked) if linked.kind == StyleKind::Character => linked.run.clone(),
        _ => RunProps::default(),
    }
}

/// Effective table properties: the table's own `w:tblPr` over its named
/// `w:tblStyle` (with `basedOn`).
///
/// Word's built-in `TableGrid` — "all borders", and by far the most-referenced
/// table style in real documents — carries the table's entire appearance here
/// and nothing at all on the table itself, so a table that names it used to
/// arrive with no borders and Typst's own default grid instead.
///
/// Returns the resolved borders and cell padding plus the style's cell
/// defaults, which a cell's own `w:tcPr` still overrides.
pub fn effective_table(styles: &Styles, table: &Table) -> TableStyleProps {
    let mut acc = TableStyleProps::default();

    if let Some(sid) = table.style_id.as_deref() {
        for style in style_chain(styles, sid) {
            acc = merge_table(&acc, &style.table);
        }
    }

    // The table's own properties are direct formatting and win outright.
    merge_table(
        &acc,
        &TableStyleProps {
            borders: table.borders.clone(),
            cell_margins: table.cell_margins,
            ..TableStyleProps::default()
        },
    )
}

fn merge_table(base: &TableStyleProps, over: &TableStyleProps) -> TableStyleProps {
    let side = |over: &Option<BorderEdge>, base: &Option<BorderEdge>| {
        over.clone().or(base.clone())
    };
    let margin = |over: Option<i64>, base: Option<i64>| over.or(base);
    TableStyleProps {
        borders: TableBorders {
            outer: merge_borders(&base.borders.outer, &over.borders.outer),
            inside_h: side(&over.borders.inside_h, &base.borders.inside_h),
            inside_v: side(&over.borders.inside_v, &base.borders.inside_v),
        },
        cell_margins: CellMargins {
            top: margin(over.cell_margins.top, base.cell_margins.top),
            bottom: margin(over.cell_margins.bottom, base.cell_margins.bottom),
            left: margin(over.cell_margins.left, base.cell_margins.left),
            right: margin(over.cell_margins.right, base.cell_margins.right),
        },
        cell_shd_fill: over.cell_shd_fill.clone().or_else(|| base.cell_shd_fill.clone()),
        cell_v_align: over.cell_v_align.clone().or_else(|| base.cell_v_align.clone()),
        conditional: base.conditional || over.conditional,
    }
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

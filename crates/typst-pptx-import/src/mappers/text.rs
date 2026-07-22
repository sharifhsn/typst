//! Paragraphs and runs → Typst inlines.

use crate::lower::{emu, LowerCtx};
use crate::pml::model::*;
use crate::resolve::inherit::{level_para_props, level_run_props, Inherited};
use crate::tdoc;

pub fn lower_paragraphs(
    paras: &[Para],
    inherited: &Inherited,
    ctx: &mut LowerCtx<'_, '_>,
) -> Vec<tdoc::Para> {
    paras.iter().map(|p| lower_paragraph(p, inherited, ctx)).collect()
}

fn lower_paragraph(
    para: &Para,
    inherited: &Inherited,
    ctx: &mut LowerCtx<'_, '_>,
) -> tdoc::Para {
    // The master's list style for this outline level is the floor; the
    // paragraph's own properties sit on top. Getting the order wrong makes a
    // deck's own formatting lose to its template's.
    let base_para = level_para_props(inherited, para.props.level);
    let props = para.props.over(&base_para);
    let base_run = level_run_props(inherited, para.props.level);

    let mut inlines = Vec::new();
    for run in &para.runs {
        if run.line_break {
            inlines.push(tdoc::Inline::LineBreak);
            continue;
        }
        if let Some(inline) = lower_run(run, &base_run, ctx) {
            inlines.push(inline);
        }
    }

    tdoc::Para {
        inlines,
        align: props.align.as_deref().and_then(lower_align),
        list: lower_list(&props),
        margin_left: props.margin_left.map(emu).filter(|v| *v > 0.0),
        indent: props.indent.map(emu),
        leading: lower_spacing(props.line_spacing, &base_run),
        space_before: lower_spacing(props.space_before, &base_run),
        space_after: lower_spacing(props.space_after, &base_run),
    }
}

fn lower_run(
    run: &Run,
    base: &RunProps,
    ctx: &mut LowerCtx<'_, '_>,
) -> Option<tdoc::Inline> {
    let text = if run.field.is_some() && run.text.is_empty() {
        // A slide-number field with no cached result still has to render
        // something, and touying's own counter is the live equivalent.
        return Some(tdoc::Inline::SlideNumber);
    } else {
        run.text.clone()
    };
    if text.is_empty() {
        return None;
    }

    let props = run.props.over(base);
    let mut inline = tdoc::Inline::Text(text);

    let styled = tdoc::TextProps {
        size: props.size.map(|v| v as f64 / 100.0),
        bold: props.bold.unwrap_or(false),
        italic: props.italic.unwrap_or(false),
        underline: props.underline.is_some(),
        strike: props.strike.is_some(),
        fill: props.color.as_ref().and_then(|c| ctx.paint(c)),
        font: [props.font.clone(), props.east_asian.clone()]
            .into_iter()
            .flatten()
            // A run whose two faces are the same name needs it once.
            .fold(Vec::new(), |mut acc, f| {
                if !acc.contains(&f) {
                    acc.push(f);
                }
                acc
            }),
        tracking: props.spacing.map(|v| v as f64 / 100.0),
        // `@baseline` is a percentage of the font size; its sign is the only
        // thing that distinguishes a superscript from a subscript.
        sub: props.baseline.is_some_and(|v| v < 0),
        super_: props.baseline.is_some_and(|v| v > 0),
        highlight: props.highlight.as_ref().and_then(|c| ctx.paint(c)),
        upper: props.caps.as_deref() == Some("all"),
    };
    if !styled.is_empty() {
        inline = tdoc::Inline::Styled { props: styled, body: vec![inline] };
    }

    // Typst's `#underline` draws one plain rule. A double or wavy underline
    // is a different mark, and saying so is cheaper than pretending.
    if props
        .underline
        .as_deref()
        .is_some_and(|u| u != "sng" && u != "single")
    {
        ctx.report.approximate(
            "underline",
            "Typst draws one plain rule, so a double, wavy or heavy underline              comes across as a single line",
        );
    }

    // A slide-number field wrapped in a link is not a thing; a text run
    // wrapped in one is common.
    if let Some(Hyperlink::Rel(id)) = &run.link
        && let Some(target) = ctx.parser.target(id)
    {
        {
            let dest = if target.external {
                tdoc::LinkTarget::Url(target.part.clone())
            } else if let Some(index) = ctx.slide_index.get(&target.part) {
                tdoc::LinkTarget::Slide(*index)
            } else {
                // A jump to a slide that is not in this deck's slide list —
                // a notes master, or a deleted slide's leftover relationship.
                ctx.report.approximate(
                    "hyperlink",
                    "a same-deck jump pointed at a part that is not a slide; the \
                     text is kept unlinked",
                );
                return Some(inline);
            };
            inline = tdoc::Inline::Link { dest, body: vec![inline] };
        }
    }
    Some(inline)
}

fn lower_align(value: &str) -> Option<tdoc::Align> {
    Some(match value {
        "l" => tdoc::Align::Left,
        "ctr" => tdoc::Align::Center,
        "r" => tdoc::Align::Right,
        "just" | "justLow" => tdoc::Align::Justify,
        // `dist`/`thaiDist` distribute glyphs, which Typst has no setting for.
        _ => return None,
    })
}

fn lower_list(props: &ParaProps) -> Option<tdoc::ListItem> {
    match props.bullet.as_ref()? {
        // An explicit `a:buNone` is the whole reason `Bullet::None` exists as
        // a value rather than an absence: it must beat an inherited bullet.
        Bullet::None => None,
        Bullet::Char { glyph, font } => {
            let glyph = symbol_bullet(glyph, font.as_deref());
            Some(tdoc::ListItem {
                level: props.level,
                ordered: false,
                // Typst's own bullet is `•`; anything else is authored and
                // has to be reproduced literally.
                marker: (glyph.as_str() != "\u{2022}").then_some(glyph),
            })
        }
        Bullet::AutoNum { .. } => {
            Some(tdoc::ListItem { level: props.level, ordered: true, marker: None })
        }
    }
}

/// Translate a symbol-font bullet into the Unicode character it depicts.
///
/// A themed deck almost always takes its bullet from Wingdings or Symbol,
/// where the *character* is an ordinary letter and the font is what makes it
/// a glyph — `a:buChar char="q"` with `buFont typeface="Wingdings"` is a
/// hollow square, not a `q`. Reproducing the letter and hoping Wingdings is
/// installed yields a tofu box on every bullet of every such deck, so the
/// common ones are mapped to the Unicode characters that mean the same thing.
///
/// Anything unmapped keeps its own character: a wrong guess is worse than a
/// letter, and non-symbol fonts state real characters already.
fn symbol_bullet(glyph: &str, font: Option<&str>) -> ecow::EcoString {
    let Some(font) = font else { return glyph.into() };
    let symbolic = font.eq_ignore_ascii_case("wingdings")
        || font.eq_ignore_ascii_case("wingdings 2")
        || font.eq_ignore_ascii_case("wingdings 3")
        || font.eq_ignore_ascii_case("webdings")
        || font.eq_ignore_ascii_case("symbol");
    if !symbolic {
        return glyph.into();
    }
    let mut chars = glyph.chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return glyph.into();
    };
    // Symbol fonts are also addressed through the private use area at U+F0xx,
    // which is the same code point with the high byte set.
    let code = match ch as u32 {
        c @ 0xF000..=0xF0FF => c - 0xF000,
        c => c,
    };
    let mapped = match (font.to_ascii_lowercase().as_str(), code) {
        ("symbol", 0xB7) => '\u{2022}',       // ·  → •
        ("symbol", 0x2D) => '\u{2013}',       // -  → –
        (_, 0x6C) => '\u{25CF}',              // l  → ●
        (_, 0x6E) => '\u{25A0}',              // n  → ■
        (_, 0x71) => '\u{2751}',              // q  → ❑
        (_, 0x76) => '\u{2756}',              // v  → ❖
        (_, 0x75) => '\u{2022}',              // u  → •
        (_, 0xA7) => '\u{25AA}',              // §  → ▪
        (_, 0xA8) => '\u{25AB}',              // ¨  → ▫
        (_, 0xD8) => '\u{27A2}',              // Ø  → ➢
        (_, 0xFC) => '\u{2713}',              // ü  → ✓
        (_, 0xFE) => '\u{2611}',              // þ  → ☑
        (_, 0x4A) => '\u{263A}',              // J  → ☺
        _ => return glyph.into(),
    };
    ecow::EcoString::from(mapped)
}

/// `a:lnSpc`/`a:spcBef`/`a:spcAft` → points.
///
/// A percentage is relative to the font size, so it needs one — which is why
/// this takes the resolved run properties rather than working on the spacing
/// alone.
fn lower_spacing(spacing: Option<Spacing>, base: &RunProps) -> Option<f64> {
    match spacing? {
        Spacing::Points(v) => Some(v as f64 / 100.0),
        Spacing::Percent(v) => {
            let size = base.size.unwrap_or(1800) as f64 / 100.0;
            let factor = v as f64 / 100_000.0;
            // 100% is PowerPoint's "single spaced", which is what Typst
            // already does — emitting a computed leading for it would replace
            // a good default with an approximation of itself.
            if (factor - 1.0).abs() < 0.01 {
                return None;
            }
            // Typst's `leading` is the gap *between* lines and PowerPoint's
            // percentage scales the whole line, so the extra goes on top of
            // Typst's own default gap of 0.65em.
            Some((size * (factor - 1.0) + size * 0.65).max(0.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_no_bullet_beats_an_inherited_one() {
        let props = ParaProps { bullet: Some(Bullet::None), ..ParaProps::default() };
        assert!(lower_list(&props).is_none());
    }

    #[test]
    fn typsts_own_bullet_glyph_is_not_repeated_literally() {
        let bullet = |g: &str, f: Option<&str>| ParaProps {
            bullet: Some(Bullet::Char { glyph: g.into(), font: f.map(Into::into) }),
            ..ParaProps::default()
        };
        assert!(lower_list(&bullet("\u{2022}", None)).unwrap().marker.is_none());
        assert_eq!(
            lower_list(&bullet("\u{25B8}", None)).unwrap().marker.as_deref(),
            Some("\u{25B8}")
        );
    }

    #[test]
    fn a_wingdings_bullet_becomes_the_glyph_it_depicts() {
        let bullet = |g: &str, f: Option<&str>| ParaProps {
            bullet: Some(Bullet::Char { glyph: g.into(), font: f.map(Into::into) }),
            ..ParaProps::default()
        };
        // `q` in Wingdings is a hollow square. Emitting the letter renders a
        // tofu box wherever Wingdings is not installed, which is everywhere
        // this importer's output is likely to be compiled.
        assert_eq!(
            lower_list(&bullet("q", Some("Wingdings"))).unwrap().marker.as_deref(),
            Some("\u{2751}")
        );
        // The same code point addressed through the private use area.
        assert_eq!(
            lower_list(&bullet("\u{F071}", Some("Wingdings"))).unwrap().marker.as_deref(),
            Some("\u{2751}")
        );
        // A real font states real characters; leave them alone.
        assert_eq!(
            lower_list(&bullet("q", Some("Arial"))).unwrap().marker.as_deref(),
            Some("q")
        );
    }

    #[test]
    fn percentage_line_spacing_becomes_a_gap_not_a_height() {
        // Single spacing is Typst's own default, so nothing is emitted.
        let base = RunProps { size: Some(1800), ..RunProps::default() };
        assert!(lower_spacing(Some(Spacing::Percent(100_000)), &base).is_none());
        // 150% adds half a line on top of the default 0.65em gap.
        let leading = lower_spacing(Some(Spacing::Percent(150_000)), &base).unwrap();
        assert!((leading - (9.0 + 11.7)).abs() < 1e-9, "{leading}");
    }
}

//! [`TypstDoc`] → touying source.
//!
//! The emitter makes no decisions. Everything it prints was decided in
//! `mappers`; if output looks wrong, the bug is upstream of this file.
//!
//! Two things here are load-bearing rather than cosmetic:
//!
//! * **Markup escaping.** Slide text is arbitrary user text and Typst markup
//!   is dense with significant characters. Every literal goes through
//!   [`escape`], and a `#` or `*` in a title has to survive as a `#` or `*`.
//! * **`#place` inside a fixed canvas.** A `.pptx` is a coordinate system, and
//!   `#place` is the only Typst construct that reserves no space — which is
//!   exactly what reproducing one requires, and what makes the output
//!   round-trip back through `typst-pptx` at the same coordinates.

use std::fmt::Write;

use ecow::EcoString;

use crate::opts::ImportOptions;
use crate::tdoc::*;

pub fn emit(doc: &TypstDoc, opts: &ImportOptions) -> String {
    let mut out = String::new();
    preamble(&mut out, doc, opts);
    for (index, slide) in doc.slides.iter().enumerate() {
        emit_slide(&mut out, slide, index, doc);
    }
    out
}

fn preamble(out: &mut String, doc: &TypstDoc, opts: &ImportOptions) {
    let version = &opts.touying_version;
    let theme = &opts.theme;
    let _ = writeln!(out, "#import \"@preview/touying:{version}\": *");
    let _ = writeln!(out, "#import themes.{theme}: *");
    out.push('\n');

    // The page is stated in absolute points rather than by `aspect-ratio`,
    // because a real deck's canvas is whatever `p:sldSz` says — 4:3, 16:9,
    // A4-landscape and custom poster sizes all occur, and only the exact size
    // keeps `#place` coordinates meaning what they meant in PowerPoint.
    let _ = writeln!(out, "#show: {theme}-theme.with(");
    let _ = writeln!(
        out,
        "  config-page(width: {}, height: {}, margin: 0pt),",
        len(doc.width),
        len(doc.height)
    );
    if doc.title.is_some() || doc.author.is_some() {
        let _ = write!(out, "  config-info(");
        let mut first = true;
        if let Some(title) = &doc.title {
            let _ = write!(out, "title: [{}]", escape(title));
            first = false;
        }
        if let Some(author) = &doc.author {
            if !first {
                out.push_str(", ");
            }
            let _ = write!(out, "author: [{}]", escape(author));
        }
        let _ = writeln!(out, "),");
    }
    let _ = writeln!(out, ")");
    out.push('\n');
}

fn emit_slide(out: &mut String, slide: &Slide, index: usize, doc: &TypstDoc) {
    if slide.hidden {
        // Kept rather than dropped: the content was authored, and a reader
        // deleting a comment is a smaller surprise than a reader discovering
        // a slide vanished.
        let _ = writeln!(out, "// Hidden in the source presentation (p:sld/@show=\"0\").");
    }
    if let Some(notes) = &slide.notes {
        // The same `<pdfpc-file>` payload `typst-pptx` reads back on export,
        // so notes survive a round trip instead of needing a second
        // convention. One entry per slide, one-based as pdfpc expects.
        let _ = writeln!(
            out,
            "#metadata((pdfpcFormat: 2, pages: ((idx: {}, note: {}),)))<pdfpc-file>",
            index + 1,
            string_lit(notes)
        );
    }

    let mut args = Vec::new();
    if let Some(fill) = &slide.fill {
        args.push(format!("config-page(fill: {})", paint(fill)));
    }
    if args.is_empty() {
        let _ = writeln!(out, "#slide[");
    } else {
        let _ = writeln!(out, "#slide(config: utils.merge-dicts({}))[", args.join(", "));
    }

    if let Some(heading) = &slide.heading {
        let _ = writeln!(out, "  == {}", inlines(heading));
    }
    for item in &slide.items {
        emit_item(out, item, 1, doc);
    }
    let _ = writeln!(out, "]\n");
}

fn emit_item(out: &mut String, item: &Item, depth: usize, doc: &TypstDoc) {
    let pad = "  ".repeat(depth);
    match item {
        Item::Placed { x, y, w, h, rot, flip_h, flip_v, inset, anchor, block } => {
            // `#place` with an explicit box is the pair that reproduces a
            // PowerPoint shape: the place fixes the origin, the box fixes the
            // extent, and neither disturbs the flow around it.
            let mut body = block_string(block, depth + 1, doc);
            // PowerPoint anchors text vertically inside a box that is usually
            // taller than the text. Ignoring it top-aligns every centred
            // caption in the deck.
            if let Some(slot) = anchor {
                let name = match slot {
                    VAlign::Top => "top",
                    VAlign::Middle => "horizon",
                    VAlign::Bottom => "bottom",
                };
                body = format!("#align({name} + left)[{body}]");
            }
            // A rotation of a whole turn is the identity; real decks state
            // `rot="21600000"` and it only adds noise.
            let rot = rot.rem_euclid(360.0);
            // A mirrored shape is a negative scale, which Typst has and this
            // importer once claimed it did not.
            if *flip_h || *flip_v {
                body = format!(
                    "#scale(x: {}%, y: {}%, reflow: false, box(width: {}, height: {})[{body}])",
                    if *flip_h { -100 } else { 100 },
                    if *flip_v { -100 } else { 100 },
                    len(*w),
                    len(*h)
                );
            }
            let inner = if rot.abs() > 0.001 {
                // PowerPoint turns a shape about its own centre, which is
                // `#rotate`'s default origin too.
                format!(
                    "rotate({}, box(width: {}, height: {}{})[{}])",
                    angle(rot),
                    len(*w),
                    len(*h),
                    inset_arg(inset),
                    body
                )
            } else {
                format!(
                    "box(width: {}, height: {}{})[{}]",
                    len(*w),
                    len(*h),
                    inset_arg(inset),
                    body
                )
            };
            let _ = writeln!(out, "{pad}#place(top + left, dx: {}, dy: {}, {inner})", len(*x), len(*y));
        }
        Item::Flow(block) => {
            let _ = writeln!(out, "{pad}{}", block_string(block, depth, doc));
        }
    }
}

/// `a:bodyPr`'s insets as a `box` argument, when they are not all zero.
fn inset_arg(inset: &Option<(f64, f64, f64, f64)>) -> String {
    match inset {
        Some((l, t, r, b)) if *l > 0.0 || *t > 0.0 || *r > 0.0 || *b > 0.0 => format!(
            ", inset: (left: {}, top: {}, right: {}, bottom: {})",
            len(*l),
            len(*t),
            len(*r),
            len(*b)
        ),
        _ => String::new(),
    }
}

fn block_string(block: &Block, depth: usize, doc: &TypstDoc) -> String {
    match block {
        Block::Paras(paras) => paragraphs(paras, depth),
        Block::Image(image) => image_call(image),
        // The `#` is not decoration: a shape call sits in markup position
        // inside its `box[..]`, where a bare `curve(..)` is literal text.
        //
        // The text is *overlaid* rather than nested, because `curve` and
        // `line` take no content body at all — and overlaying is what
        // PowerPoint does anyway: a shape's text floats over its fill rather
        // than flowing inside it.
        Block::Shape { call, body } => match body {
            Some(paras) if !paras.is_empty() => format!(
                "#place(top + left, {call})\n{}{}",
                "  ".repeat(depth),
                paragraphs(paras, depth)
            ),
            _ => format!("#{call}"),
        },
        Block::Table(table) => table_call(table, depth),
        Block::Group(items) => {
            let mut inner = String::new();
            for item in items {
                emit_item(&mut inner, item, depth + 1, doc);
            }
            format!("{{\n{inner}{}}}", "  ".repeat(depth))
        }
    }
}

fn paragraphs(paras: &[Para], depth: usize) -> String {
    let pad = "  ".repeat(depth);
    let mut out = String::new();
    for (i, para) in paras.iter().enumerate() {
        if i > 0 {
            // Consecutive list items must stay in *one* Typst list: a blank
            // line between them ends the list and starts another, which
            // renders as paragraph-spaced items rather than a tight list.
            let both_list = para.list.is_some() && paras[i - 1].list.is_some();
            if both_list {
                let _ = write!(out, "\n{pad}");
            } else {
                let _ = write!(out, "\n\n{pad}");
            }
        }
        out.push_str(&paragraph(para, depth));
    }
    out
}

fn paragraph(para: &Para, depth: usize) -> String {
    let body = inlines(&para.inlines);
    if body.trim().is_empty() && para.list.is_none() {
        return String::new();
    }

    let mut out = body;

    // A list item's marker is Typst's own when it can be; a literal marker is
    // only emitted when PowerPoint asked for a glyph Typst would not produce.
    if let Some(list) = &para.list {
        let indent = "  ".repeat(list.level as usize);
        out = match (&list.marker, list.ordered) {
            (Some(marker), _) => {
                format!("{indent}- #box[{}] {out}", escape(marker))
            }
            (None, true) => format!("{indent}+ {out}"),
            (None, false) => format!("{indent}- {out}"),
        };
        return out;
    }

    let mut wrappers: Vec<(String, String)> = Vec::new();
    if let Some(align) = para.align {
        let name = match align {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "left",
        };
        if align == Align::Justify {
            wrappers.push(("par(justify: true)[".into(), "]".into()));
        } else if align != Align::Left {
            wrappers.push((format!("align({name})["), "]".into()));
        }
    }
    // `above`/`below` are *block* spacing and `leading` is *paragraph* line
    // spacing. Passing the first two to `par` is a hard error, which is how
    // the corpus found this: 60-odd real decks failed to compile on it.
    if para.leading.is_some() || para.space_before.is_some() || para.space_after.is_some() {
        let mut block_params = Vec::new();
        if let Some(before) = para.space_before {
            block_params.push(format!("above: {}", len(before)));
        }
        if let Some(after) = para.space_after {
            block_params.push(format!("below: {}", len(after)));
        }
        let inner = match para.leading {
            Some(leading) => format!("#par(leading: {})[", len(leading)),
            None => String::new(),
        };
        let close = if inner.is_empty() { "]" } else { "]]" };
        wrappers.push((
            format!("block({})[{inner}", block_params.join(", ")),
            close.into(),
        ));
    }
    if let Some(left) = para.margin_left.filter(|v| *v > 0.0) {
        wrappers.push((format!("pad(left: {})[", len(left)), "]".into()));
    }

    for (open, close) in wrappers.into_iter().rev() {
        out = format!("#{open}{out}{close}");
    }
    let _ = depth;
    out
}

fn inlines(items: &[Inline]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            Inline::Text(text) => out.push_str(&escape(text)),
            Inline::LineBreak => out.push_str(" \\\n"),
            Inline::SlideNumber => {
                out.push_str("#context utils.slide-counter.display()");
            }
            Inline::Link { dest, body } => {
                let target = match dest {
                    LinkTarget::Url(url) => string_lit(url),
                    // A same-deck jump: touying numbers slides from 1.
                    LinkTarget::Slide(index) => format!("<slide-{}>", index + 1),
                };
                let _ = write!(out, "#link({target})[{}]", inlines(body));
            }
            Inline::Styled { props, body } => {
                out.push_str(&styled(props, &inlines(body)));
            }
        }
    }
    out
}

fn styled(props: &TextProps, body: &str) -> String {
    if body.is_empty() {
        return String::new();
    }
    let mut out = body.to_string();

    // Innermost first: the scripts and decorations wrap the text, and the
    // font/size/colour set wraps all of it.
    if props.upper {
        out = format!("#upper[{out}]");
    }
    if props.sub {
        out = format!("#sub[{out}]");
    }
    if props.super_ {
        out = format!("#super[{out}]");
    }
    if props.strike {
        out = format!("#strike[{out}]");
    }
    if props.underline {
        out = format!("#underline[{out}]");
    }
    if props.bold {
        out = format!("#strong[{out}]");
    }
    if props.italic {
        out = format!("#emph[{out}]");
    }
    if let Some(highlight) = &props.highlight {
        out = format!("#highlight(fill: {})[{out}]", paint(highlight));
    }

    let mut params = Vec::new();
    if let Some(size) = props.size {
        params.push(format!("size: {}", len(size)));
    }
    match props.font.as_slice() {
        [] => {}
        [one] => params.push(format!("font: {}", string_lit(one))),
        many => params.push(format!(
            "font: ({})",
            many.iter().map(|f| string_lit(f)).collect::<Vec<_>>().join(", ")
        )),
    }
    if let Some(fill) = &props.fill {
        params.push(format!("fill: {}", paint(fill)));
    }
    if let Some(tracking) = props.tracking {
        params.push(format!("tracking: {}", len(tracking)));
    }
    if !params.is_empty() {
        out = format!("#text({})[{out}]", params.join(", "));
    }
    out
}

fn image_call(image: &Image) -> String {
    let mut params = vec![
        string_lit(&image.path),
        format!("width: {}", len(image.width)),
        format!("height: {}", len(image.height)),
    ];
    if let Some(alt) = &image.alt {
        params.push(format!("alt: {}", string_lit(alt)));
    }
    let mut call = format!("image({})", params.join(", "));

    // Typst's `image` has no crop parameter, so a crop becomes geometry: the
    // picture is oversized by the reciprocal of the visible fraction and the
    // hidden band is pushed outside a clipping box. The same inversion the
    // Word importer performs for `a:srcRect`.
    if let Some([l, t, r, b]) = image.crop {
        let vis_w = (1.0 - l - r).max(0.01);
        let vis_h = (1.0 - t - b).max(0.01);
        let full_w = image.width / vis_w;
        let full_h = image.height / vis_h;
        let inner = format!(
            "image({}, width: {}, height: {})",
            string_lit(&image.path),
            len(full_w),
            len(full_h)
        );
        call = format!(
            "box(width: {}, height: {}, clip: true, place(top + left, dx: {}, dy: {}, {inner}))",
            len(image.width),
            len(image.height),
            len(-full_w * l),
            len(-full_h * t)
        );
    }
    if let Some(radius) = image.radius {
        call = format!("box(radius: {}, clip: true, {call})", len(radius));
    }
    format!("#{call}")
}

fn table_call(table: &Table, depth: usize) -> String {
    let pad = "  ".repeat(depth);
    let mut out = String::from("#table(\n");
    if table.columns.is_empty() {
        let _ = writeln!(out, "{pad}  columns: {},", table.auto_columns.max(1));
    } else {
        let _ = writeln!(
            out,
            "{pad}  columns: ({}),",
            table.columns.iter().map(|w| len(*w)).collect::<Vec<_>>().join(", ")
        );
    }
    if table.rows.iter().any(|r| r.height.is_some()) {
        let rows: Vec<String> = table
            .rows
            .iter()
            .map(|r| r.height.map(len).unwrap_or_else(|| "auto".into()))
            .collect();
        let _ = writeln!(out, "{pad}  rows: ({}),", rows.join(", "));
    }
    let _ = writeln!(out, "{pad}  inset: 5pt,");
    // `none`, not Typst's default: PowerPoint draws a table's edges from the
    // cells and the style, so a table that states no borders has none. The
    // Word importer learned this the hard way — a deliberately borderless
    // table arriving with a 1pt grid nobody drew.
    // A recovered chart is a data table and reads as one; a slide's own table
    // draws only what its cells state.
    if table.columns.is_empty() {
        let _ = writeln!(out, "{pad}  stroke: 0.5pt + gray,");
    } else {
        let _ = writeln!(out, "{pad}  stroke: none,");
    }

    for (index, row) in table.rows.iter().enumerate() {
        let cells: Vec<String> = row
            .cells
            .iter()
            .map(|cell| {
                let body = paragraphs(&cell.paras, depth + 2);
                let mut params = Vec::new();
                if cell.colspan > 1 {
                    params.push(format!("colspan: {}", cell.colspan));
                }
                if cell.rowspan > 1 {
                    params.push(format!("rowspan: {}", cell.rowspan));
                }
                if let Some(fill) = &cell.fill {
                    params.push(format!("fill: {}", paint(fill)));
                }
                if let Some(align) = &cell.align_y {
                    params.push(format!("align: {align}"));
                }
                let sides = ["left", "top", "right", "bottom"];
                let stated: Vec<String> = cell
                    .stroke
                    .iter()
                    .zip(sides)
                    .filter_map(|(s, name)| s.as_ref().map(|v| format!("{name}: {v}")))
                    .collect();
                if !stated.is_empty() {
                    params.push(format!("stroke: ({})", stated.join(", ")));
                }
                if params.is_empty() {
                    format!("[{body}]")
                } else {
                    format!("table.cell({})[{body}]", params.join(", "))
                }
            })
            .collect();
        let line = cells.join(", ");
        if index < table.header_rows {
            let _ = writeln!(out, "{pad}  table.header({line}),");
        } else {
            let _ = writeln!(out, "{pad}  {line},");
        }
    }
    let _ = write!(out, "{pad})");
    out
}

pub fn paint(paint: &Paint) -> String {
    match paint {
        Paint::Rgb([r, g, b, a]) => {
            if *a == 255 {
                format!("rgb(\"#{r:02x}{g:02x}{b:02x}\")")
            } else {
                format!("rgb({r}, {g}, {b}, {a})")
            }
        }
        Paint::Gradient { stops, angle: ang, radial } => {
            let list: Vec<String> = stops
                .iter()
                .map(|(pos, [r, g, b, a])| {
                    let color = if *a == 255 {
                        format!("rgb(\"#{r:02x}{g:02x}{b:02x}\")")
                    } else {
                        format!("rgb({r}, {g}, {b}, {a})")
                    };
                    format!("({color}, {}%)", num(*pos * 100.0))
                })
                .collect();
            if *radial {
                format!("gradient.radial({})", list.join(", "))
            } else {
                format!("gradient.linear({}, angle: {})", list.join(", "), angle(*ang))
            }
        }
    }
}

/// A length in points, printed without a trailing `.0`.
pub fn len(value: f64) -> String {
    format!("{}pt", num(value))
}

fn angle(value: f64) -> String {
    format!("{}deg", num(value))
}

/// Four decimal places is well below a printer's resolution and keeps the
/// output diffable; trailing zeros are trimmed so round numbers read as round.
pub fn num(value: f64) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let mut s = format!("{value:.4}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" { "0".into() } else { s }
}

/// A Typst string literal.
pub fn string_lit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Escape text for Typst *markup*.
///
/// Every character Typst gives meaning to in markup position has to be
/// neutralised, or a slide reading `#1 * 2` becomes a code expression and a
/// syntax error. Erring towards over-escaping is right here: an unnecessary
/// backslash renders as nothing, a missing one changes the document.
pub fn escape(text: &str) -> EcoString {
    let mut out = EcoString::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            // Only significant at a line start, but a slide's text can be
            // re-flowed by a later pass, so neutralise it wherever it appears.
            '-' | '+' | '=' | '/' => {
                out.push('\\');
                out.push(ch);
            }
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_significant_characters_are_neutralised() {
        // A real slide title that would otherwise be parsed as code.
        assert_eq!(escape("#1 * 2 = 2"), "\\#1 \\* 2 \\= 2");
        assert_eq!(escape("a_b"), "a\\_b");
        assert_eq!(escape("<tag>"), "\\<tag\\>");
    }

    #[test]
    fn lengths_print_without_noise() {
        assert_eq!(len(12.0), "12pt");
        assert_eq!(len(12.5), "12.5pt");
        assert_eq!(len(0.0), "0pt");
        // A value that rounds to negative zero must not print as "-0pt".
        assert_eq!(len(-0.00001), "0pt");
    }

    #[test]
    fn string_literals_escape_quotes_and_newlines() {
        assert_eq!(string_lit("a\"b"), "\"a\\\"b\"");
        assert_eq!(string_lit("a\nb"), "\"a\\nb\"");
    }
}

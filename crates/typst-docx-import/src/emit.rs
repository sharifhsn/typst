//! Emit Typst source from the Typst IR ([`crate::tdoc`]), plus the media
//! assets referenced by [`crate::tdoc::Figure`] blocks.
//!
//! This is a pure pretty-printer: it does not infer anything about the
//! document, it just renders the tree it is handed. Any idiomatic-vs-literal
//! decisions already happened upstream ([`crate::lower`], [`crate::passes`]).

use std::collections::HashSet;
use std::path::PathBuf;

use crate::opts::ImportOptions;
use crate::tdoc::{
    Align, Block, BreakKind, Figure, Inline, Inlines, List, Margins, PageSetup, ParStyle, Script,
    Stmt, Table, TableCell, TextStyle, TypstDoc,
};
use crate::wml::model::WmlPackage;

/// Render a [`TypstDoc`] to Typst source text, resolving each [`Figure`]'s
/// `image_path` against `package.media` and collecting the extracted assets.
pub fn emit(
    doc: &TypstDoc,
    package: &WmlPackage,
    options: &ImportOptions,
) -> (String, Vec<(PathBuf, Vec<u8>)>) {
    if doc.preamble.is_empty() && doc.body.is_empty() {
        return (String::new(), Vec::new());
    }

    let mut emitter = Emitter {
        package,
        options,
        assets: Vec::new(),
        seen_assets: HashSet::new(),
    };

    let mut out = String::new();
    for stmt in &doc.preamble {
        out.push_str(&render_stmt(stmt));
        out.push('\n');
    }
    if !doc.preamble.is_empty() && !doc.body.is_empty() {
        out.push('\n');
    }
    for block in &doc.body {
        out.push_str(&emitter.render_block(block));
        out.push_str("\n\n");
    }

    // Normalize trailing whitespace down to exactly one newline at EOF.
    while out.ends_with('\n') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }

    (out, emitter.assets)
}

/// Per-document emission state: needs `&mut self` only because rendering a
/// [`Figure`] resolves and collects a media asset.
struct Emitter<'a> {
    package: &'a WmlPackage,
    options: &'a ImportOptions,
    assets: Vec<(PathBuf, Vec<u8>)>,
    seen_assets: HashSet<PathBuf>,
}

impl Emitter<'_> {
    fn render_block(&mut self, block: &Block) -> String {
        match block {
            Block::Heading { level, body } => {
                let marker = "=".repeat((*level).max(1) as usize);
                format!("{marker} {}", render_inlines(body))
            }
            Block::Paragraph { style, body } => {
                let text = render_inlines(body);
                match style.align {
                    Some(Align::Center) => format!("#align(center)[{text}]"),
                    Some(Align::Right) => format!("#align(right)[{text}]"),
                    Some(Align::Justify) => format!("#par(justify: true)[{text}]"),
                    Some(Align::Left) | None => text,
                }
            }
            Block::List(list) => render_list(list),
            Block::Table(table) => self.render_table(table),
            Block::Figure(figure) => self.render_figure(figure),
            Block::CodeBlock { lang, text } => {
                let lang = lang.as_deref().unwrap_or("");
                format!("```{lang}\n{text}\n```")
            }
            Block::Equation { body } => format!("$ {body} $"),
            Block::Rule => "#line(length: 100%)".to_string(),
            Block::Break(BreakKind::Page) => "#pagebreak()".to_string(),
            Block::Break(BreakKind::Column) => "#colbreak()".to_string(),
            Block::Verbatim(s) => s.to_string(),
        }
    }

    fn render_table(&mut self, table: &Table) -> String {
        let columns_arg = table_columns_arg(table);
        let mut lines = vec!["#table(".to_string(), format!("  columns: {columns_arg},")];

        let mut header_used = false;
        for row in &table.rows {
            let cells: Vec<String> =
                row.cells.iter().map(|cell| self.render_cell(cell)).collect();
            if row.header && !header_used {
                lines.push(format!("  table.header({}),", cells.join(", ")));
                header_used = true;
            } else {
                lines.push(format!("  {},", cells.join(", ")));
            }
        }

        lines.push(")".to_string());
        lines.join("\n")
    }

    fn render_cell(&mut self, cell: &TableCell) -> String {
        let content = self.render_cell_body(&cell.body);

        let mut args = Vec::new();
        if let Some(fill) = cell.fill {
            args.push(format!("fill: {}", rgb_lit(fill)));
        }
        if cell.colspan > 1 {
            args.push(format!("colspan: {}", cell.colspan));
        }
        if cell.rowspan > 1 {
            args.push(format!("rowspan: {}", cell.rowspan));
        }

        if args.is_empty() {
            format!("[{content}]")
        } else {
            format!("table.cell({})[{content}]", args.join(", "))
        }
    }

    fn render_cell_body(&mut self, blocks: &[Block]) -> String {
        match blocks {
            [] => String::new(),
            [single] => self.render_block(single),
            many => many
                .iter()
                .map(|block| self.render_block(block))
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }

    fn render_figure(&mut self, figure: &Figure) -> String {
        let path = self.resolve_asset(&figure.image_path);

        let mut args = vec![string_literal(&path)];
        if let Some(width) = figure.width_pt {
            args.push(format!("width: {}", pt(width)));
        }
        if let Some(height) = figure.height_pt {
            args.push(format!("height: {}", pt(height)));
        }
        if let Some(alt) = &figure.alt {
            args.push(format!("alt: {}", string_literal(alt)));
        }
        let image_call = format!("image({})", args.join(", "));

        match &figure.caption {
            Some(caption) => {
                let caption = render_inlines(caption);
                format!("#figure({image_call}, caption: [{caption}])")
            }
            None => format!("#{image_call}"),
        }
    }

    /// Map a `Figure::image_path` (a `word/media/...` part name) into the
    /// configured assets directory, collecting the backing bytes from
    /// `package.media` (deduped by the emitted path) the first time it's seen.
    fn resolve_asset(&mut self, image_path: &str) -> String {
        let basename = image_path
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(image_path);
        let dir = self.options.assets_dir.trim_end_matches('/');
        let emitted = if dir.is_empty() {
            basename.to_string()
        } else {
            format!("{dir}/{basename}")
        };
        let emitted_path = PathBuf::from(&emitted);

        if self.seen_assets.insert(emitted_path.clone()) {
            let bytes = self.package.media.get(image_path).or_else(|| {
                self.package
                    .media
                    .iter()
                    .find(|(name, _)| name.as_str().rsplit(['/', '\\']).next() == Some(basename))
                    .map(|(_, bytes)| bytes)
            });
            if let Some(bytes) = bytes {
                self.assets.push((emitted_path, bytes.clone()));
            }
        }

        emitted
    }
}

fn table_columns_arg(table: &Table) -> String {
    let all_auto =
        table.column_widths.is_empty() || table.column_widths.iter().all(Option::is_none);
    if all_auto {
        return table.columns.to_string();
    }

    let parts: Vec<String> = (0..table.columns)
        .map(|i| match table.column_widths.get(i).copied().flatten() {
            Some(width) => pt(width),
            None => "auto".to_string(),
        })
        .collect();

    if parts.len() == 1 {
        format!("({},)", parts[0])
    } else {
        format!("({})", parts.join(", "))
    }
}

fn render_list(list: &List) -> String {
    list.items
        .iter()
        .map(|item| {
            let indent = "  ".repeat(item.level as usize);
            let marker = if item.ordered { "+" } else { "-" };
            format!("{indent}{marker} {}", render_inlines(&item.body))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_stmt(stmt: &Stmt) -> String {
    match stmt {
        Stmt::SetPage(page) => render_set_page(page),
        Stmt::SetText(style) => render_set_text(style),
        Stmt::SetPar(style) => render_set_par(style),
        Stmt::Verbatim(s) => s.to_string(),
    }
}

fn render_set_page(page: &PageSetup) -> String {
    let mut args = Vec::new();
    if let Some(width) = page.width_pt {
        args.push(format!("width: {}", pt(width)));
    }
    if let Some(height) = page.height_pt {
        args.push(format!("height: {}", pt(height)));
    }
    if let Some(margin) = &page.margin {
        args.push(format!("margin: {}", render_margins(margin)));
    }
    if page.flipped {
        args.push("flipped: true".to_string());
    }
    format!("#set page({})", args.join(", "))
}

fn render_margins(margin: &Margins) -> String {
    format!(
        "(top: {}, bottom: {}, left: {}, right: {})",
        pt(margin.top_pt),
        pt(margin.bottom_pt),
        pt(margin.left_pt),
        pt(margin.right_pt),
    )
}

fn render_set_text(style: &TextStyle) -> String {
    let mut args = Vec::new();
    if let Some(font) = &style.font {
        args.push(format!("font: {}", string_literal(font)));
    }
    if let Some(size) = style.size_pt {
        args.push(format!("size: {}", pt(size)));
    }
    if let Some(color) = style.color {
        args.push(format!("fill: {}", rgb_lit(color)));
    }
    format!("#set text({})", args.join(", "))
}

fn render_set_par(style: &ParStyle) -> String {
    let mut args = Vec::new();
    if style.align == Some(Align::Justify) {
        args.push("justify: true".to_string());
    }
    if let Some(leading) = style.leading_pt {
        args.push(format!("leading: {}", pt(leading)));
    }
    if let Some(spacing) = style.spacing_before_pt {
        args.push(format!("spacing: {}", pt(spacing)));
    }
    format!("#set par({})", args.join(", "))
}

fn render_inlines(inlines: &Inlines) -> String {
    inlines.iter().map(render_inline).collect()
}

fn render_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(s) => escape_markup(s),
        Inline::Space => " ".to_string(),
        Inline::Linebreak => " \\\n".to_string(),
        Inline::Strong(body) => format!("*{}*", render_inlines(body)),
        Inline::Emph(body) => format!("_{}_", render_inlines(body)),
        Inline::Raw(s) => {
            if s.contains('`') {
                format!("#raw({})", string_literal(s))
            } else {
                format!("`{s}`")
            }
        }
        Inline::Link { dest, body } => {
            format!("#link({})[{}]", string_literal(dest), render_inlines(body))
        }
        Inline::Styled { style, body } => render_styled(style, body),
        Inline::Math(s) => format!("${s}$"),
        Inline::Verbatim(s) => s.to_string(),
    }
}

fn render_styled(style: &TextStyle, body: &Inlines) -> String {
    if style.is_empty() {
        return render_inlines(body);
    }

    let mut content = render_inlines(body);

    content = match style.script {
        Some(Script::Super) => format!("#super[{content}]"),
        Some(Script::Sub) => format!("#sub[{content}]"),
        None => content,
    };
    if style.smallcaps {
        content = format!("#smallcaps[{content}]");
    }
    if style.strike {
        content = format!("#strike[{content}]");
    }
    if style.underline {
        content = format!("#underline[{content}]");
    }

    let mut args = Vec::new();
    if let Some(font) = &style.font {
        args.push(format!("font: {}", string_literal(font)));
    }
    if let Some(size) = style.size_pt {
        args.push(format!("size: {}", pt(size)));
    }
    if style.bold {
        args.push("weight: \"bold\"".to_string());
    }
    if style.italic {
        args.push("style: \"italic\"".to_string());
    }
    if let Some(color) = style.color {
        args.push(format!("fill: {}", rgb_lit(color)));
    }

    if args.is_empty() {
        content
    } else {
        format!("#text({})[{content}]", args.join(", "))
    }
}

/// Escape literal text for insertion as Typst *markup* (not a string
/// literal). Escapes only Typst's special markup characters, backslash-
/// prefixing them in place; a `=`/`-`/`+`/`/` is additionally escaped when it
/// is the first non-space character of the run (to avoid it being read as a
/// heading/list/comment marker). A run containing raw control characters —
/// where per-character escaping is ambiguous — falls back to the code-string
/// form `#("...")`.
pub fn escape_markup(text: &str) -> String {
    if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return code_string_fallback(text);
    }

    let mut out = String::with_capacity(text.len());
    let mut at_run_start = true;
    for ch in text.chars() {
        let leading_marker = at_run_start && matches!(ch, '=' | '-' | '+' | '/');
        if ch != ' ' {
            at_run_start = false;
        }

        if leading_marker || matches!(ch, '#' | '*' | '_' | '`' | '$' | '<' | '@' | '\\' | '~') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The escape hatch for text that plain markup-escaping can't safely
/// represent: emit it as a Typst string-literal expression instead.
fn code_string_fallback(text: &str) -> String {
    format!("#({})", string_literal(text))
}

/// Encode a Rust string as a Typst string literal (`"..."`, with `\` and `"`
/// escaped). Used for function-argument strings (paths, font names, link
/// destinations) as well as the [`code_string_fallback`].
fn string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn rgb_lit(color: [u8; 3]) -> String {
    format!("rgb(\"{:02X}{:02X}{:02X}\")", color[0], color[1], color[2])
}

/// Format a point value with up to two decimals, trimming trailing zeros
/// (`11.000000` -> `11`, `71.5` -> `71.5`).
fn fmt_pt(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract().abs() < f64::EPSILON {
        format!("{}", rounded as i64)
    } else {
        let s = format!("{rounded:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// A point value with the `pt` unit suffix, e.g. `11pt` / `71.5pt`.
fn pt(value: f64) -> String {
    format!("{}pt", fmt_pt(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdoc::{Align, BreakKind, ListItem, TableRow};

    fn doc(preamble: Vec<Stmt>, body: Vec<Block>) -> TypstDoc {
        TypstDoc { preamble, body }
    }

    fn run(doc: &TypstDoc) -> String {
        let package = WmlPackage::default();
        let options = ImportOptions::default();
        emit(doc, &package, &options).0
    }

    #[test]
    fn empty_doc_is_empty() {
        let d = doc(vec![], vec![]);
        let (source, assets) = emit(&d, &WmlPackage::default(), &ImportOptions::default());
        assert_eq!(source, "");
        assert!(assets.is_empty());
    }

    #[test]
    fn heading_levels() {
        let d = doc(
            vec![],
            vec![
                Block::Heading { level: 1, body: vec![Inline::Text("Title".into())] },
                Block::Heading { level: 3, body: vec![Inline::Text("Sub".into())] },
            ],
        );
        assert_eq!(run(&d), "= Title\n\n=== Sub\n");
    }

    #[test]
    fn bold_and_italic_paragraph() {
        let body = vec![
            Inline::Strong(vec![Inline::Text("bold".into())]),
            Inline::Space,
            Inline::Emph(vec![Inline::Text("italic".into())]),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "*bold* _italic_\n");
    }

    #[test]
    fn centered_paragraph_and_page_break() {
        let d = doc(
            vec![],
            vec![
                Block::Paragraph {
                    style: ParStyle { align: Some(Align::Center), ..Default::default() },
                    body: vec![Inline::Text("Hi".into())],
                },
                Block::Break(BreakKind::Page),
            ],
        );
        assert_eq!(run(&d), "#align(center)[Hi]\n\n#pagebreak()\n");
    }

    #[test]
    fn unordered_list() {
        let list = List {
            items: vec![
                ListItem { ordered: false, level: 0, body: vec![Inline::Text("one".into())] },
                ListItem { ordered: false, level: 1, body: vec![Inline::Text("two".into())] },
            ],
        };
        let d = doc(vec![], vec![Block::List(list)]);
        assert_eq!(run(&d), "- one\n  - two\n");
    }

    #[test]
    fn simple_table_with_header_and_fill() {
        let table = Table {
            columns: 2,
            column_widths: vec![],
            rows: vec![
                TableRow {
                    header: true,
                    cells: vec![
                        TableCell {
                            colspan: 1,
                            rowspan: 1,
                            fill: None,
                            body: vec![Block::Paragraph {
                                style: ParStyle::default(),
                                body: vec![Inline::Text("A".into())],
                            }],
                        },
                        TableCell {
                            colspan: 1,
                            rowspan: 1,
                            fill: None,
                            body: vec![Block::Paragraph {
                                style: ParStyle::default(),
                                body: vec![Inline::Text("B".into())],
                            }],
                        },
                    ],
                },
                TableRow {
                    header: false,
                    cells: vec![
                        TableCell {
                            colspan: 1,
                            rowspan: 1,
                            fill: Some([255, 0, 0]),
                            body: vec![Block::Paragraph {
                                style: ParStyle::default(),
                                body: vec![Inline::Text("1".into())],
                            }],
                        },
                        TableCell {
                            colspan: 1,
                            rowspan: 1,
                            fill: None,
                            body: vec![Block::Paragraph {
                                style: ParStyle::default(),
                                body: vec![Inline::Text("2".into())],
                            }],
                        },
                    ],
                },
            ],
        };
        let d = doc(vec![], vec![Block::Table(table)]);
        assert_eq!(
            run(&d),
            "#table(\n  columns: 2,\n  table.header([A], [B]),\n  table.cell(fill: rgb(\"FF0000\"))[1], [2],\n)\n"
        );
    }

    #[test]
    fn escape_markup_leading_and_special_chars() {
        assert_eq!(escape_markup("- not a list"), "\\- not a list");
        assert_eq!(escape_markup("a - b"), "a - b");
        assert_eq!(escape_markup("#tag *bold* $math$"), "\\#tag \\*bold\\* \\$math\\$");
        assert_eq!(escape_markup("50% off"), "50% off");
    }

    #[test]
    fn set_page_and_set_text_preamble() {
        let d = doc(
            vec![
                Stmt::SetPage(PageSetup {
                    width_pt: Some(595.28),
                    height_pt: Some(841.89),
                    margin: Some(Margins {
                        top_pt: 72.0,
                        bottom_pt: 72.0,
                        left_pt: 72.0,
                        right_pt: 72.0,
                    }),
                    flipped: false,
                }),
                Stmt::SetText(TextStyle {
                    font: Some("Roboto".into()),
                    size_pt: Some(11.0),
                    ..Default::default()
                }),
            ],
            vec![Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("Hello".into())],
            }],
        );
        let out = run(&d);
        assert!(out.starts_with(
            "#set page(width: 595.28pt, height: 841.89pt, margin: (top: 72pt, bottom: 72pt, left: 72pt, right: 72pt))\n#set text(font: \"Roboto\", size: 11pt)\n\nHello\n"
        ));
    }

    #[test]
    fn set_par_justify_renders_as_justify_true() {
        let d = doc(
            vec![Stmt::SetPar(ParStyle { align: Some(Align::Justify), ..Default::default() })],
            vec![Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("Hello".into())],
            }],
        );
        assert!(run(&d).starts_with("#set par(justify: true)\n"));
    }
}

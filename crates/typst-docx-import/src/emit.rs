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
    Align, Block, BreakKind, Chart, ChartContent, Figure, Furniture, Inline, Inlines, List,
    LegendPos, Margins, PageSetup, ParStyle, Plot, PlotKind, PlotSeries, Script, Stmt, Table,
    TableCell,
    TextStyle, TypstDoc,
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
        used_ruby: false,
        used_plot: false,
    };

    let mut out = String::new();
    for stmt in &doc.preamble {
        out.push_str(&emitter.render_stmt(stmt));
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

    // Helper definitions/imports go above everything, and only once it's
    // known which ones the rendered body actually called for. Order between
    // the two doesn't matter to Typst (neither depends on the other), so
    // this just always puts the import above the helper.
    if emitter.used_ruby {
        out.insert_str(0, &format!("{RUBY_HELPER}\n\n"));
    }
    if emitter.used_plot {
        out.insert_str(0, &format!("{LILAQ_IMPORT}\n\n"));
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
    /// Set when an [`Inline::Ruby`] is rendered, so [`RUBY_HELPER`] is emitted
    /// only for documents that actually use it.
    used_ruby: bool,
    /// Set when a [`Plot`] is actually rendered (as opposed to every chart in
    /// the document falling back to a table), so [`LILAQ_IMPORT`] is emitted
    /// only for documents that actually need the package. Mirrors
    /// `used_ruby` exactly — see its doc comment.
    used_plot: bool,
}

/// Word's `w:ruby` has no Typst counterpart, so documents that use furigana
/// get this definition prepended.
///
/// The reading is *placed* above the base rather than stacked with it: a stack
/// lifts the base off the surrounding baseline and inflates the line, while
/// `place` leaves the sentence sitting exactly where it would without the
/// annotation — which is what furigana is supposed to look like. Verified by
/// rendering both against a real Japanese sentence.
const RUBY_HELPER: &str =
    "#let ruby(base, gloss) = box(place(top + center, dy: -0.85em, text(size: 0.5em, gloss)) + base)";

/// A chart under [`crate::opts::ChartStyle::Plot`] is drawn with `lilaq`
/// (`typst.app/universe/package/lilaq`), so a document that renders at least
/// one gains this import. Version pinned, exactly as verified against
/// `lilaq:0.6.0` — the syntax [`Emitter::render_plot`] emits is not
/// guaranteed to keep working across a `lilaq` major/minor bump.
const LILAQ_IMPORT: &str = "#import \"@preview/lilaq:0.6.0\" as lq";

impl Emitter<'_> {
    fn render_block(&mut self, block: &Block) -> String {
        match block {
            Block::Heading { level, body } => {
                let marker = "=".repeat((*level).max(1) as usize);
                format!("{marker} {}", self.render_inlines(body))
            }
            Block::Paragraph { style, body } => {
                let text = self.render_inlines(body);
                match style.align {
                    Some(Align::Center) => format!("#align(center)[{text}]"),
                    Some(Align::Right) => format!("#align(right)[{text}]"),
                    Some(Align::Justify) => format!("#par(justify: true)[{text}]"),
                    Some(Align::Left) | None => text,
                }
            }
            Block::List(list) => self.render_list(list),
            Block::Table(table) => format!("#{}", self.render_table(table)),
            Block::Figure(figure) => self.render_figure(figure),
            Block::Chart(chart) => self.render_chart(chart),
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

    /// Render a `table(..)` *expression* — deliberately without the leading
    /// `#` a bare block-level table needs, since [`Self::render_chart`] also
    /// uses this to embed the table as an argument to `figure(..)`, where a
    /// `#` would be a syntax error. [`Self::render_block`]'s `Block::Table`
    /// arm adds the `#` itself for the stand-alone case.
    fn render_table(&mut self, table: &Table) -> String {
        let columns_arg = table_columns_arg(table);
        let mut lines = vec!["table(".to_string(), format!("  columns: {columns_arg},")];

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

    fn render_stmt(&mut self, stmt: &Stmt) -> String {
        match stmt {
            Stmt::SetPage(page) => self.render_set_page(page),
            Stmt::SetText(style) => render_set_text(style),
            Stmt::SetPar(style) => render_set_par(style),
            Stmt::Verbatim(s) => s.to_string(),
        }
    }

    fn render_set_page(&mut self, page: &PageSetup) -> String {
        let mut simple_args = Vec::new();
        if let Some(width) = page.width_pt {
            simple_args.push(format!("width: {}", pt(width)));
        }
        if let Some(height) = page.height_pt {
            simple_args.push(format!("height: {}", pt(height)));
        }
        if let Some(margin) = &page.margin {
            simple_args.push(format!("margin: {}", render_margins(margin)));
        }
        if page.flipped {
            simple_args.push("flipped: true".to_string());
        }

        // No furniture: keep the single-line form every other `#set page(..)`
        // call already uses. A header/footer's content can itself span
        // several lines (a `context` block with conditional branches), so
        // once one is present every argument gets its own indented line
        // instead — a 900-character `#set page(..)` line is not "readable".
        if page.header.is_none() && page.footer.is_none() {
            return format!("#set page({})", simple_args.join(", "));
        }

        let mut lines = vec!["#set page(".to_string()];
        for arg in &simple_args {
            lines.push(format!("  {arg},"));
        }
        if let Some(header) = &page.header {
            push_indented(&mut lines, &self.render_furniture_arg("header", header));
        }
        if let Some(footer) = &page.footer {
            push_indented(&mut lines, &self.render_furniture_arg("footer", footer));
        }
        lines.push(")".to_string());
        lines.join("\n")
    }

    /// Render one `header:`/`footer:` argument. A furniture with only a
    /// default variant is a plain content block; one with an active
    /// `first`/`even` variant becomes a `context` block that branches on the
    /// current page number, always falling back to `default` (or an empty
    /// content block, if there isn't one) last.
    fn render_furniture_arg(&mut self, name: &str, furniture: &Furniture) -> String {
        if furniture.first.is_none() && furniture.even.is_none() {
            let content = self.render_furniture_content(&furniture.default);
            return format!("{name}: [{content}]");
        }

        let mut branches: Vec<(&str, String)> = Vec::new();
        if let Some(first) = &furniture.first {
            branches.push(("p == 1", self.render_furniture_content(first)));
        }
        if let Some(even) = &furniture.even {
            branches.push(("calc.even(p)", self.render_furniture_content(even)));
        }
        let default_content = self.render_furniture_content(&furniture.default);

        let mut lines = vec![
            format!("{name}: context {{"),
            "  let p = counter(page).get().first()".to_string(),
        ];
        for (i, (cond, content)) in branches.iter().enumerate() {
            let keyword = if i == 0 { "if" } else { "else if" };
            lines.push(format!("  {keyword} {cond} [{content}]"));
        }
        lines.push(format!("  else [{default_content}]"));
        lines.push("}".to_string());
        lines.join("\n")
    }

    /// Render a furniture's blocks (ordinary paragraphs/figures/tables — the
    /// same content a table cell can hold) as inline markup content, the same
    /// way [`Self::render_cell_body`] does for a cell.
    fn render_furniture_content(&mut self, blocks: &[Block]) -> String {
        self.render_cell_body(blocks)
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
                let caption = self.render_inlines(caption);
                format!("#figure({image_call}, caption: [{caption}])")
            }
            None => format!("#{image_call}"),
        }
    }

    /// A [`Chart`] renders as its content expression — either the bare
    /// `table(..)` expression from [`Self::render_table`], or the bare
    /// `lq.diagram(..)` expression from [`Self::render_plot`] — captioned
    /// with the chart's title if it has one, exactly like
    /// [`Self::render_figure`] wraps an `image(..)` call. The title is plain
    /// text (see [`crate::wml::parse::parse_chart_title`]), not structured
    /// `Inlines`, so it's markup-escaped directly rather than routed through
    /// [`Self::render_inlines`]. Deliberately never also passes the title to
    /// `lq.diagram`'s own `title:` argument — the figure caption is the one
    /// Typst-idiomatic place for it, and setting both would print it twice.
    fn render_chart(&mut self, chart: &Chart) -> String {
        let content_expr = match &chart.content {
            ChartContent::Table(table) => self.render_table(table),
            ChartContent::Plot(plot) => self.render_plot(plot),
        };
        match &chart.title {
            Some(title) => {
                format!("#figure({content_expr}, caption: [{}])", escape_markup(title))
            }
            None => format!("#{content_expr}"),
        }
    }

    /// Render a `lq.diagram(..)` *expression* — the plot counterpart of
    /// [`Self::render_table`], same bare-expression convention (no leading
    /// `#`) so [`Self::render_chart`] can embed it in `figure(..)` the same
    /// way. Category ticks become an explicit `xaxis:` argument; an empty
    /// [`Plot::categories`] omits it entirely, leaving `lilaq`'s default
    /// numeric axis (which is exactly the point index every mark below plots
    /// against anyway).
    fn render_plot(&mut self, plot: &Plot) -> String {
        self.used_plot = true;

        let mut lines = vec!["lq.diagram(".to_string()];
        // Word's own extent for the chart. Without it `lilaq` uses its default
        // size, which is far narrower than a typical Word chart — enough that
        // four category labels overlap each other and the legend covers the
        // last series' bars.
        if let Some(width) = plot.width_pt {
            lines.push(format!("  width: {},", pt(width)));
        }
        if let Some(height) = plot.height_pt {
            lines.push(format!("  height: {},", pt(height)));
        }
        // Word's own legend placement. A chart that declared no legend gets
        // `none` rather than the library default — drawing a legend Word
        // deliberately left off is an invention, not an approximation.
        lines.push(format!("  legend: {},", legend_arg(plot.legend)));
        if !plot.categories.is_empty() {
            let ticks: Vec<String> = plot
                .categories
                .iter()
                .enumerate()
                .map(|(i, category)| format!("({i}, [{}])", escape_markup(category)))
                .collect();
            lines.push(format!("  xaxis: (ticks: {}),", fmt_tuple_of(ticks)));
        }
        for mark in self.render_plot_marks(plot) {
            lines.push(format!("  {mark},"));
        }
        lines.push(")".to_string());
        lines.join("\n")
    }

    /// One `lq.plot`/`lq.scatter`/`lq.bar` call per series — see
    /// [`Self::render_xy_marks`] and [`Self::render_bar_marks`] for the two
    /// shapes ([`PlotKind::Bar`] needs grouped x-offsets; the other two plot
    /// a series straight against its own point index).
    fn render_plot_marks(&mut self, plot: &Plot) -> Vec<String> {
        match plot.kind {
            PlotKind::Bar => render_bar_marks(&plot.series),
            PlotKind::Line => self.render_xy_marks("lq.plot", &plot.series),
            PlotKind::Scatter => self.render_xy_marks("lq.scatter", &plot.series),
        }
    }

    /// A line/scatter series plotted against its own point index: `func((0,
    /// 1, …), (v₀, v₁, …), label: [name])`. `label:` is omitted for an
    /// unnamed series (see [`Self::render_bar_marks`] for the same rule on
    /// the bar path).
    fn render_xy_marks(&mut self, func: &str, series: &[PlotSeries]) -> Vec<String> {
        series
            .iter()
            .map(|s| {
                let xs = fmt_tuple((0..s.values.len()).map(|i| i as f64));
                let ys = fmt_tuple(s.values.iter().copied());
                format!("{func}({xs}, {ys}{})", render_series_label(s))
            })
            .collect()
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

impl Emitter<'_> {
    fn render_list(&mut self, list: &List) -> String {
        list.items
            .iter()
            .map(|item| {
                let indent = "  ".repeat(item.level as usize);
                let marker = if item.ordered { "+" } else { "-" };
                format!("{indent}{marker} {}", self.render_inlines(&item.body))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Append a (possibly multi-line) rendered argument to the `#set page(..)`
/// call's `lines`, indenting every physical line by two spaces and adding the
/// trailing comma Typst wants between call arguments.
fn push_indented(lines: &mut Vec<String>, arg: &str) {
    let mut arg_lines = arg.lines();
    let Some(first) = arg_lines.next() else { return };
    let mut buf = format!("  {first}");
    for rest in arg_lines {
        buf.push('\n');
        buf.push_str("  ");
        buf.push_str(rest);
    }
    buf.push(',');
    lines.push(buf);
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

impl Emitter<'_> {
    fn render_inlines(&mut self, inlines: &Inlines) -> String {
        // Render every inline up front: whether a `*`/`_` shorthand is safe
        // depends on the characters either side of it, so the neighbours have
        // to exist before the decision can be made. A `Strong`/`Emph` piece
        // always begins with `*`, `_` or `#` — never alphanumeric — so using
        // the shorthand rendering to probe the *next* piece's first character
        // gives the same answer as the form eventually chosen for it.
        let pieces: Vec<String> = inlines.iter().map(|inline| self.render_inline(inline)).collect();

        let mut out = String::new();
        for (i, (inline, piece)) in inlines.iter().zip(&pieces).enumerate() {
            let (body, delim, function) = match inline {
                Inline::Strong(body) => (body, '*', "strong"),
                Inline::Emph(body) => (body, '_', "emph"),
                _ => {
                    out.push_str(piece);
                    // Two text boxes are independent floating objects that
                    // Word anchors at unrelated page positions; they were
                    // never adjacent *text*. Inlining them back to back would
                    // fuse the last word of one onto the first word of the
                    // next, so consecutive boxes get a separator. (Ordinary
                    // runs deliberately don't: Word splits words across run
                    // boundaries mid-word all the time.)
                    if matches!(inline, Inline::TextBox(_))
                        && matches!(inlines.get(i + 1), Some(Inline::TextBox(_)))
                    {
                        out.push(' ');
                    }
                    continue;
                }
            };

            let prev = out.chars().next_back();
            let next = pieces[i + 1..].iter().find_map(|p| p.chars().next());
            let rendered = self.render_inlines(body);
            if shorthand_is_safe(prev, next, &rendered) {
                out.push(delim);
                out.push_str(&rendered);
                out.push(delim);
            } else {
                out.push_str(&format!("#{function}[{rendered}]"));
            }
        }
        out
    }
}

/// Whether the `*`/`_` markup shorthand parses as a delimiter in this position.
///
/// Typst only reads it as one at a word boundary: `a *b* c` is strong, but
/// `a*b*c` is an unclosed-delimiter **error**, and the `_` equivalent degrades
/// silently to literal underscores. Word, meanwhile, happily applies character
/// formatting across sub-word run boundaries — bolding the middle of a word is
/// routine — so the shorthand is only correct when both delimiters sit next to
/// something non-alphanumeric. Everywhere else the function form is used: it is
/// less pretty but always parses, and correctness outranks prettiness here.
fn shorthand_is_safe(prev: Option<char>, next: Option<char>, body: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    // The delimiter must also hug its content — `* b *` does not open a strong.
    boundary(prev)
        && boundary(next)
        && !body.is_empty()
        && !body.starts_with(char::is_whitespace)
        && !body.ends_with(char::is_whitespace)
}

impl Emitter<'_> {
    fn render_inline(&mut self, inline: &Inline) -> String {
        match inline {
            Inline::Text(s) => escape_markup(s),
            Inline::Space => " ".to_string(),
            Inline::Linebreak => " \\\n".to_string(),
            Inline::Strong(body) => format!("*{}*", self.render_inlines(body)),
            Inline::Emph(body) => format!("_{}_", self.render_inlines(body)),
            Inline::Raw(s) => {
                if s.contains('`') {
                    format!("#raw({})", string_literal(s))
                } else {
                    format!("`{s}`")
                }
            }
            Inline::Link { dest, body } => {
                format!("#link({})[{}]", string_literal(dest), self.render_inlines(body))
            }
            Inline::Styled { style, body } => self.render_styled(style, body),
            Inline::Math(s) => format!("${s}$"),
            // The note's content, rendered as a content block exactly like a
            // table cell's or a furniture body's — see `render_cell_body`.
            // Typst's own `#footnote[..]` call inlines the body right here;
            // there is no separate note store to route it through.
            Inline::Footnote(blocks) => {
                let content = self.render_cell_body(blocks);
                format!("#footnote[{content}]")
            }
            // Typst has no ruby primitive, so this calls a helper the emitter
            // defines in the preamble (see `RUBY_HELPER`). Rendering the call
            // is what marks the helper as needed.
            Inline::Ruby { base, gloss } => {
                self.used_ruby = true;
                let base = self.render_inlines(base);
                let gloss = self.render_inlines(gloss);
                format!("#ruby[{base}][{gloss}]")
            }
            // A text box's content, rendered the same way — see the
            // `Footnote` arm just above. `#box[..]` is the closest Typst
            // primitive: it happily takes multi-paragraph content (verified
            // by hand before wiring this in), even though — like a footnote
            // — it's built for a shorter inline body; the geometry Word
            // floated this at is simply gone.
            Inline::TextBox(blocks) => {
                let content = self.render_cell_body(blocks);
                format!("#box[{content}]")
            }
            Inline::Verbatim(s) => s.to_string(),
        }
    }

    fn render_styled(&mut self, style: &TextStyle, body: &Inlines) -> String {
        if style.is_empty() {
            return self.render_inlines(body);
        }

        let mut content = self.render_inlines(body);

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
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let leading_marker = at_run_start && matches!(ch, '=' | '-' | '+' | '/');
        if ch != ' ' {
            at_run_start = false;
        }

        // `//` opens a line comment, which would swallow the rest of the line
        // including the closing `]`. URLs in prose ("http://…") hit this
        // constantly. Escaping the first slash is enough to break the pair —
        // `\/` consumes both characters, leaving a harmless lone slash. (`/*`
        // needs no special case: `*` is escaped unconditionally below.)
        let comment_start = ch == '/' && chars.peek() == Some(&'/');

        // `[`/`]` delimit content blocks. Literal brackets are common in real
        // prose ("[100]", "[Lower bound]"), and an unescaped one closes the
        // enclosing `#text(..)[..]` early — emitting source that doesn't parse.
        if leading_marker
            || comment_start
            || matches!(
                ch,
                '#' | '*' | '_' | '`' | '$' | '<' | '@' | '\\' | '~' | '[' | ']'
            )
        {
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

/// `c:legendPos` → `lilaq`'s legend `position`, or `none` for a chart that
/// declares no legend.
fn legend_arg(pos: Option<LegendPos>) -> &'static str {
    // `lilaq` places a legend *inside* the data area by default, which is
    // where Word puts one only for `tr` (its overlay corner). For the four
    // edge positions Word draws the legend outside the plot, and the package's
    // documented way to do that is to anchor on the opposite edge and shift by
    // the full data-area extent — otherwise the legend sits on top of the bars.
    match pos {
        None => "none",
        Some(LegendPos::Top) => "(position: bottom + center, dy: -100%, pad: 8pt)",
        Some(LegendPos::Bottom) => "(position: top + center, dy: 100%, pad: 8pt)",
        Some(LegendPos::Left) => "(position: right + horizon, dx: -100%, pad: 8pt)",
        Some(LegendPos::Right) => "(position: left + horizon, dx: 100%, pad: 8pt)",
        // Word's `tr` is an overlay corner inside the plot — the one case
        // where `lilaq`'s own default placement is already right.
        Some(LegendPos::TopRight) => "(position: top + right)",
    }
}

/// `pub(crate)` (rather than private) because `mappers::shape` builds its
/// `#rect`/`#circle`/`#ellipse`/`#line` calls as ready-made strings — the
/// same [`Inline::Verbatim`] escape hatch `mappers::field` uses — and needs
/// the exact same color-literal formatting this emitter already uses
/// everywhere else, rather than a second, drifting copy of it.
pub(crate) fn rgb_lit(color: [u8; 3]) -> String {
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
/// `pub(crate)` for the same reason as [`rgb_lit`] just above.
pub(crate) fn pt(value: f64) -> String {
    format!("{}pt", fmt_pt(value))
}

/// A series' `label:` argument (`, label: [name]`), or an empty string for an
/// unnamed series — the one bit [`Emitter::render_xy_marks`] and
/// [`render_bar_marks`] share, since a bar mark's argument list otherwise
/// looks nothing like a line/scatter mark's.
fn render_series_label(series: &PlotSeries) -> String {
    match &series.name {
        Some(name) => format!(", label: [{}]", escape_markup(name)),
        None => String::new(),
    }
}

/// A Typst tuple literal from unformatted values, each passed through
/// [`fmt_pt`] — e.g. `(0, 0.25, 1.75)`. A single-element tuple needs its
/// trailing comma to parse as an array rather than a parenthesized
/// expression, the same rule [`table_columns_arg`] already follows for a
/// one-column table; zero elements is `()`, which needs no comma either way.
fn fmt_tuple(values: impl Iterator<Item = f64>) -> String {
    fmt_tuple_of(values.map(fmt_pt).collect())
}

/// [`fmt_tuple`]'s tuple-literal formatting, taking already-rendered pieces —
/// what [`Emitter::render_plot`] uses for its category-tick pairs, which
/// aren't plain numbers.
fn fmt_tuple_of(parts: Vec<String>) -> String {
    match parts.len() {
        0 => "()".to_string(),
        1 => format!("({},)", parts[0]),
        _ => format!("({})", parts.join(", ")),
    }
}

/// The grouped-bar marks for [`PlotKind::Bar`]: with `N` series, series `i`
/// (0-based) gets its own width `1/(N+1)` and x offset `(i - (N-1)/2) *
/// 1/(N+1)`, so the `N` bars for one category sit side by side centered on
/// that category's tick rather than stacked on top of each other. Verified
/// against `lilaq:0.6.0` for `N = 3` (offsets `-0.25, 0, 0.25`, width
/// `0.25`) — see this crate's docs for the worked example this generalizes.
fn render_bar_marks(series: &[PlotSeries]) -> Vec<String> {
    let n = series.len();
    let width = 1.0 / (n as f64 + 1.0);
    series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let offset = (i as f64 - (n as f64 - 1.0) / 2.0) * width;
            let xs = fmt_tuple((0..s.values.len()).map(|j| j as f64 + offset));
            let ys = fmt_tuple(s.values.iter().copied());
            format!("lq.bar({xs}, {ys}, width: {}{})", fmt_pt(width), render_series_label(s))
        })
        .collect()
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

    /// Word bolds sub-word spans routinely ("abc**de**fg"), but Typst only
    /// reads `*`/`_` as delimiters at a word boundary — mid-word, `*` is an
    /// unclosed-delimiter error and `_` silently stays literal. Those spans
    /// must fall back to the function form.
    #[test]
    fn mid_word_emphasis_falls_back_to_the_function_form() {
        let body = vec![
            Inline::Text("abc".into()),
            Inline::Strong(vec![Inline::Text("de".into())]),
            Inline::Text("fg".into()),
            Inline::Emph(vec![Inline::Text("hi".into())]),
            Inline::Text("jk".into()),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "abc#strong[de]fg#emph[hi]jk\n");

        // A span touching a word on only one side is still unsafe.
        let body = vec![
            Inline::Text("abc".into()),
            Inline::Strong(vec![Inline::Text("de".into())]),
            Inline::Space,
            Inline::Text("x".into()),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "abc#strong[de] x\n");

        // Punctuation is a boundary, so the shorthand survives where it can.
        let body = vec![
            Inline::Text("(".into()),
            Inline::Strong(vec![Inline::Text("de".into())]),
            Inline::Text(")".into()),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "(*de*)\n");
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
    fn footnote_renders_as_a_footnote_call_with_its_body_inlined() {
        let body = vec![
            Inline::Text("before".into()),
            Inline::Footnote(vec![Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("snoska".into())],
            }]),
            Inline::Text("after".into()),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "before#footnote[snoska]after\n");
    }

    /// A footnote body with more than one block joins them the same way
    /// `render_cell_body` joins a multi-block table cell — blank-line
    /// separated — since that's exactly what it delegates to.
    #[test]
    fn footnote_with_multiple_blocks_joins_them_like_a_cell_body() {
        let body = vec![Inline::Footnote(vec![
            Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("first".into())],
            },
            Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("second".into())],
            },
        ])];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "#footnote[first\n\nsecond]\n");
    }

    /// A text box renders as `#box[..]`, inlined at its anchor — the closest
    /// Typst has, since the floating position/size can't come along.
    #[test]
    fn text_box_renders_as_a_box_call_with_its_content_inlined() {
        let body = vec![
            Inline::Text("before".into()),
            Inline::TextBox(vec![Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("boxed".into())],
            }]),
            Inline::Text("after".into()),
        ];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "before#box[boxed]after\n");
    }

    /// A text box's content with more than one block — several paragraphs,
    /// or block-level content like a heading or a list — joins the same way
    /// a multi-block table cell or footnote body does (verified by hand with
    /// `typst compile` that `#box[..]` accepts this before wiring it in: it
    /// compiles even though, like a footnote, it's built for a shorter
    /// inline body).
    #[test]
    fn text_box_with_multiple_blocks_joins_them_like_a_cell_body() {
        let body = vec![Inline::TextBox(vec![
            Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("first".into())],
            },
            Block::Paragraph {
                style: ParStyle::default(),
                body: vec![Inline::Text("second".into())],
            },
        ])];
        let d = doc(vec![], vec![Block::Paragraph { style: ParStyle::default(), body }]);
        assert_eq!(run(&d), "#box[first\n\nsecond]\n");
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

    /// Brackets and `//` are the two escapes real Word prose reaches for
    /// constantly — bracketed numbering and URLs. Both used to emit source
    /// that failed to parse: an unescaped `]` closed the enclosing content
    /// block early, and `//` opened a line comment that ate the rest of it.
    #[test]
    fn escape_markup_brackets_and_line_comments() {
        assert_eq!(escape_markup("[100] a. Plot"), "\\[100\\] a. Plot");
        assert_eq!(escape_markup("see http://x.org/~u/d.csv"), "see http:\\//x.org/\\~u/d.csv");
        // A lone slash is harmless and stays readable; only the pair is broken up.
        assert_eq!(escape_markup("and/or"), "and/or");
        // Runs of slashes leave no unescaped `//` pair behind.
        assert_eq!(escape_markup("a///b"), "a\\/\\//b");
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
                    ..Default::default()
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

    fn para(text: &str) -> Block {
        Block::Paragraph { style: ParStyle::default(), body: vec![Inline::Text(text.into())] }
    }

    #[test]
    fn default_only_header_renders_as_a_plain_content_block() {
        let d = doc(
            vec![Stmt::SetPage(PageSetup {
                header: Some(Furniture { default: vec![para("Simple header")], first: None, even: None }),
                ..Default::default()
            })],
            vec![para("Hi")],
        );
        let out = run(&d);
        assert!(out.contains("header: [Simple header]"), "{out}");
        assert!(!out.contains("context"), "{out}");
    }

    #[test]
    fn first_and_even_variants_render_as_a_context_block_with_default_fallback() {
        let furniture = Furniture {
            default: vec![para("Default")],
            first: Some(vec![para("First")]),
            even: Some(vec![para("Even")]),
        };
        let d = doc(
            vec![Stmt::SetPage(PageSetup { header: Some(furniture), ..Default::default() })],
            vec![para("Hi")],
        );
        let out = run(&d);
        assert!(out.contains("header: context {"), "{out}");
        assert!(out.contains("let p = counter(page).get().first()"), "{out}");
        assert!(out.contains("if p == 1 [First]"), "{out}");
        assert!(out.contains("else if calc.even(p) [Even]"), "{out}");
        assert!(out.contains("else [Default]"), "{out}");
    }

    #[test]
    fn variant_without_a_default_falls_back_to_an_empty_content_block() {
        let furniture = Furniture { default: vec![], first: Some(vec![para("First")]), even: None };
        let d = doc(
            vec![Stmt::SetPage(PageSetup { footer: Some(furniture), ..Default::default() })],
            vec![],
        );
        let out = run(&d);
        assert!(out.contains("else []"), "{out}");
    }

    #[test]
    fn header_and_footer_coexist_with_page_geometry_in_one_readable_call() {
        let d = doc(
            vec![Stmt::SetPage(PageSetup {
                width_pt: Some(595.28),
                header: Some(Furniture { default: vec![para("H")], first: None, even: None }),
                footer: Some(Furniture { default: vec![para("F")], first: None, even: None }),
                ..Default::default()
            })],
            vec![],
        );
        let out = run(&d);
        // Multi-line and indented, not one giant line.
        assert!(out.starts_with("#set page(\n  width: 595.28pt,\n"), "{out}");
        assert!(out.contains("  header: [H],\n"), "{out}");
        assert!(out.contains("  footer: [F],\n"), "{out}");
    }

    // --- Charts: `ChartContent::Table` / `ChartContent::Plot` ---------------

    fn series(name: Option<&str>, values: &[f64]) -> PlotSeries {
        PlotSeries { name: name.map(Into::into), values: values.to_vec() }
    }

    #[test]
    fn table_chart_renders_like_before_with_no_lilaq_import() {
        let table = Table {
            columns: 1,
            column_widths: vec![],
            rows: vec![TableRow {
                header: false,
                cells: vec![TableCell {
                    colspan: 1,
                    rowspan: 1,
                    fill: None,
                    body: vec![para("1")],
                }],
            }],
        };
        let chart = Chart { title: Some("Sales".into()), content: ChartContent::Table(table) };
        let d = doc(vec![], vec![Block::Chart(chart)]);
        let out = run(&d);
        assert!(out.contains("#figure(table("), "{out}");
        assert!(out.contains("caption: [Sales]"), "{out}");
        assert!(!out.contains("lilaq"), "table mode must never pull in lilaq:\n{out}");
    }

    #[test]
    fn line_plot_renders_one_lq_plot_call_per_series_against_point_index() {
        let plot = Plot {
            kind: PlotKind::Line,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec![],
            series: vec![series(Some("Series 1"), &[4.3, 2.5, 3.5])],
        };
        let chart = Chart { title: None, content: ChartContent::Plot(plot) };
        let d = doc(vec![], vec![Block::Chart(chart)]);
        let out = run(&d);
        assert!(
            out.contains("lq.plot((0, 1, 2), (4.3, 2.5, 3.5), label: [Series 1])"),
            "{out}"
        );
        // No categories: `xaxis:` is omitted entirely rather than emitted empty.
        assert!(!out.contains("xaxis:"), "{out}");
        assert!(out.contains(LILAQ_IMPORT), "import missing:\n{out}");
    }

    #[test]
    fn scatter_plot_uses_lq_scatter() {
        let plot = Plot {
            kind: PlotKind::Scatter,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec![],
            series: vec![series(None, &[1.0, -2.5])],
        };
        let chart = Chart { title: None, content: ChartContent::Plot(plot) };
        let out = run(&doc(vec![], vec![Block::Chart(chart)]));
        // Unnamed series: no `label:` argument.
        assert!(out.contains("lq.scatter((0, 1), (1, -2.5))"), "{out}");
    }

    /// The category axis becomes an explicit `xaxis: (ticks: ..)` argument,
    /// and — since there's a title — the whole diagram is wrapped in a
    /// `#figure(..)` the same way a table chart is.
    #[test]
    fn categories_become_xaxis_ticks_and_a_titled_plot_is_captioned() {
        let plot = Plot {
            kind: PlotKind::Line,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec!["Cat 1".into(), "Cat 2".into(), "Cat 3".into()],
            series: vec![series(None, &[1.0, 2.0, 3.0])],
        };
        let chart = Chart { title: Some("Trend".into()), content: ChartContent::Plot(plot) };
        let out = run(&doc(vec![], vec![Block::Chart(chart)]));
        assert!(
            out.contains("xaxis: (ticks: ((0, [Cat 1]), (1, [Cat 2]), (2, [Cat 3]))),"),
            "{out}"
        );
        assert!(out.starts_with(&format!("{LILAQ_IMPORT}\n\n")), "{out}");
        assert!(out.contains("#figure(lq.diagram("), "{out}");
        assert!(out.contains("caption: [Trend]"), "{out}");
    }

    /// The worked example from the task: 3 series, grouped bars with width
    /// `1/(N+1)` and offsets `(i - (N-1)/2) * 1/(N+1)` — verified rendering
    /// correctly in real `lilaq:0.6.0` output before this test was written.
    #[test]
    fn grouped_bar_offsets_match_the_verified_three_series_example() {
        let plot = Plot {
            kind: PlotKind::Bar,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec![],
            series: vec![
                series(Some("S1"), &[4.3, 2.5, 3.5]),
                series(Some("S2"), &[2.4, 4.4, 1.8]),
                series(Some("S3"), &[2.0, 2.0, 3.0]),
            ],
        };
        let chart = Chart { title: None, content: ChartContent::Plot(plot) };
        let out = run(&doc(vec![], vec![Block::Chart(chart)]));
        assert!(
            out.contains(
                "lq.bar((-0.25, 0.75, 1.75), (4.3, 2.5, 3.5), width: 0.25, label: [S1])"
            ),
            "{out}"
        );
        assert!(
            out.contains("lq.bar((0, 1, 2), (2.4, 4.4, 1.8), width: 0.25, label: [S2])"),
            "{out}"
        );
        // `fmt_pt`-style formatting trims a trailing `.0` (`2.0` -> `2`), the
        // same rule every other number in this emitter's output already
        // follows — see `fmt_pt`'s own doc comment.
        assert!(
            out.contains("lq.bar((0.25, 1.25, 2.25), (2, 2, 3), width: 0.25, label: [S3])"),
            "{out}"
        );
    }

    /// A single bar series needs no grouping at all — `N = 1` gives width
    /// `0.5` and offset `0`, which still renders (not a special case in the
    /// formula, just its `N = 1` instance).
    #[test]
    fn single_series_bar_chart_is_not_offset() {
        let plot = Plot {
            kind: PlotKind::Bar,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec![],
            series: vec![series(None, &[1.0, 2.0])],
        };
        let chart = Chart { title: None, content: ChartContent::Plot(plot) };
        let out = run(&doc(vec![], vec![Block::Chart(chart)]));
        assert!(out.contains("lq.bar((0, 1), (1, 2), width: 0.5)"), "{out}");
    }

    /// Two plottable charts in one document must still pull in the `lilaq`
    /// import exactly once, not once per chart.
    #[test]
    fn lilaq_import_is_emitted_exactly_once_for_two_plot_charts() {
        let plot = || Plot {
            kind: PlotKind::Line,
            width_pt: None,
            height_pt: None,
            legend: None,
            categories: vec![],
            series: vec![series(None, &[1.0])],
        };
        let d = doc(
            vec![],
            vec![
                Block::Chart(Chart { title: None, content: ChartContent::Plot(plot()) }),
                Block::Chart(Chart { title: None, content: ChartContent::Plot(plot()) }),
            ],
        );
        let out = run(&d);
        assert_eq!(
            out.matches(LILAQ_IMPORT).count(),
            1,
            "expected exactly one lilaq import:\n{out}"
        );
    }

    /// A document where every chart is `ChartContent::Table` (e.g. every
    /// chart fell back under `ChartStyle::Plot`) must not carry the `lilaq`
    /// import at all — nothing in the source actually needs it.
    #[test]
    fn no_lilaq_import_when_every_chart_is_a_table() {
        let table = Table { columns: 1, column_widths: vec![], rows: vec![] };
        let d = doc(
            vec![],
            vec![Block::Chart(Chart { title: None, content: ChartContent::Table(table) })],
        );
        let out = run(&d);
        assert!(!out.contains("lilaq"), "{out}");
    }
}

//! `MathItem` IR → LaTeX-string emitter for the Pandoc math node.
//!
//! Pandoc stores math as an opaque LaTeX *string* (`Math InlineMath "<tex>"` /
//! `Math DisplayMath "<tex>"`). Typst has no Typst-math→LaTeX emitter, so we
//! walk the [`MathItem`] IR — the same IR the DOCX backend lowers to OMML — and
//! emit LaTeX tokens instead. The IR has already done all the structural
//! grouping (fractions, radicals, scripts, fences, tables, accents, …), so each
//! [`MathKind`] variant maps to one LaTeX construct and the recursion composes:
//! `binom(n,k)` arrives as a `Fenced` wrapping a no-bar `Fraction`, `mat(delim:
//! "[")` as a `Fenced` wrapping a `Table`, both falling out of the per-variant
//! handlers for free.
//!
//! The emitter is built around a **fallibility contract**: any construct or
//! glyph it cannot represent *cleanly* makes it return `Err(Unrepresentable)`,
//! and the caller falls back to the rasterize-to-`Image` path for that whole
//! equation. A partial emitter that bails to raster is a strict improvement over
//! always-raster, and never emits broken LaTeX. The hard bail-outs are:
//! `MathKind::Box` (inline content laid out inside math), `MathKind::External`
//! (placed elements), and any unmapped non-ASCII glyph that texmath would not
//! understand.
//!
//! The long tail is the per-glyph **symbol→command table** ([`symbol`]): ~190 of
//! the most common Typst math glyphs (Greek, relations, arrows, operators, big
//! operators, set theory, logic, dots, delimiters). ASCII passes through as-is;
//! a mapped symbol emits its `\command`; an unmapped non-ASCII glyph is the bail
//! trigger.
//!
//! The produced LaTeX is validated by piping the resulting Pandoc JSON through
//! the real `pandoc -f json -t latex` (the LaTeX is sensible) and `-t docx`
//! (texmath parses it back into OMML, exit 0).

use typst_library::math::ir::{
    AccentItem, AlignedRow, FencedItem, FractionItem, GlyphItem, MathComponent, MathItem,
    MathKind, MultilineItem, NumberItem, Position, RadicalItem, ScriptsItem,
    SkewedFractionItem, TableItem, TextItem,
};

mod symbol;

/// The emitter failed because a sub-item has no clean LaTeX representation; the
/// caller must fall back to rasterizing the whole equation.
pub struct Unrepresentable;

type Result<T> = std::result::Result<T, Unrepresentable>;

/// Records a bail reason when `DR_MATH_DBG` is set (coverage diagnostics only),
/// and returns the error. Compiles to a plain `Err` in the common case.
#[inline]
fn bail<T>(reason: &str) -> Result<T> {
    if std::env::var_os("DR_MATH_DBG").is_some() {
        eprintln!("DR_MATH_BAIL {reason}");
    }
    Err(Unrepresentable)
}

/// Walks a resolved equation IR into a LaTeX string, or returns
/// [`Unrepresentable`] if any sub-item cannot be cleanly emitted (the caller
/// then rasterizes the equation).
pub fn emit(item: &MathItem) -> Result<String> {
    let mut e = Emitter { buf: String::new() };
    e.emit_row(item)?;
    Ok(e.buf.trim().to_string())
}

struct Emitter {
    buf: String,
}

impl Emitter {
    /// Emits a top-level row (flattening a leading group).
    fn emit_row(&mut self, item: &MathItem) -> Result<()> {
        self.emit_items(item.as_slice())
    }

    /// Emits a run of items, treating a leading n-ary operator (∑ ∫ ∏ …) with
    /// limits as `\op_{lo}^{hi}` followed — by simple linear juxtaposition — by
    /// the operand. Unlike OMML, LaTeX wants no nesting of the integrand inside
    /// the operator, so this is just `\sum_{lo}^{hi} <rest>`; the limits attach
    /// to the operator and the rest follows naturally.
    fn emit_items(&mut self, items: &[MathItem]) -> Result<()> {
        for item in items {
            self.emit_item(item)?;
        }
        Ok(())
    }

    /// Emits one item, inserting a separating space so adjacent atoms/commands
    /// do not glue (`\alpha beta` would parse as `\alphabeta`).
    fn emit_item(&mut self, item: &MathItem) -> Result<()> {
        let comp = match item {
            MathItem::Component(comp) => comp,
            // A regular inter-atom space, or explicit spacing (`quad`, `#h(..)`):
            // a single space is a safe near-lossless choice (TeX recomputes math
            // spacing anyway).
            MathItem::Space => {
                self.push_token("\\ ");
                return Ok(());
            }
            MathItem::Spacing(..) => {
                self.push_token("\\,");
                return Ok(());
            }
            // Introspection tags carry no rendered LaTeX, but the caller harvests
            // them via the rasterize tag-harvest only when it falls back. On the
            // clean path the equation's own tags reach the introspector through
            // the realized tree (the equation element itself is locatable); an
            // *inner* per-line label tag, however, is only inside this IR. We
            // cannot represent it in a LaTeX string, so its presence forces a
            // fallback so the rasterize path can harvest it (convergence-safe).
            MathItem::Tag(_) => return bail("inner tag (label/ref/cite)"),
        };
        self.emit_kind(comp)
    }

    fn emit_kind(&mut self, comp: &MathComponent) -> Result<()> {
        match &comp.kind {
            MathKind::Group(group) => {
                // A group is a transparent horizontal run wrapped in `{…}` so it
                // composes as a single unit when it lands in a script/argument.
                self.push_token("{");
                self.emit_items(&group.items)?;
                self.buf.push('}');
                Ok(())
            }
            MathKind::Glyph(glyph) => self.emit_glyph(glyph),
            MathKind::Number(num) => self.emit_number(num),
            MathKind::Text(text) => self.emit_text(text),
            MathKind::Primes(primes) => {
                for _ in 0..primes.count {
                    self.buf.push('\'');
                }
                Ok(())
            }
            MathKind::Fraction(frac) => self.emit_fraction(frac),
            MathKind::SkewedFraction(frac) => self.emit_skewed_fraction(frac),
            MathKind::Radical(rad) => self.emit_radical(rad),
            MathKind::Scripts(scripts) => self.emit_scripts(scripts),
            MathKind::Accent(acc) => self.emit_accent(acc),
            MathKind::Line(line) => self.emit_bar(&line.base, line.position),
            MathKind::Fenced(fenced) => self.emit_fenced(fenced),
            MathKind::Table(table) => self.emit_table(table),
            MathKind::Multiline(multi) => self.emit_multiline(multi),
            MathKind::Cancel(item) => {
                self.push_token("\\cancel{");
                self.emit_group(&item.base)?;
                self.buf.push('}');
                Ok(())
            }
            // No clean LaTeX form: bail so the caller rasterizes the equation.
            MathKind::Box(_) => bail("box() in math"),
            MathKind::External(_) => bail("external/placed in math"),
            MathKind::Mathml(item) => match &item.body {
                Some(body) => self.emit_item(body),
                None => Ok(()),
            },
        }
    }

    // -- Leaves -------------------------------------------------------------

    fn emit_glyph(&mut self, glyph: &GlyphItem) -> Result<()> {
        let text = glyph.text.as_str();
        // Single-codepoint glyph: map through the symbol table (handles ASCII
        // escaping + unicode→command + bail on the unmapped). Multi-codepoint
        // graphemes are rare here; map char-by-char.
        for c in text.chars() {
            match symbol::map_char(c) {
                Some(s) => self.push_token(s.as_str()),
                None => return bail(&format!("glyph U+{:04X}", c as u32)),
            }
        }
        Ok(())
    }

    fn emit_number(&mut self, num: &NumberItem) -> Result<()> {
        // Digits and the decimal point pass through verbatim. A digit ends any
        // preceding control word, so no separating space is needed.
        self.buf.push_str(num.text.as_str());
        Ok(())
    }

    fn emit_text(&mut self, text: &TextItem) -> Result<()> {
        // A quoted text run or an operator name (`sin`, `lim`, …). Known operator
        // names get their `\command` (so `\sin`, `\lim` typeset upright with
        // correct spacing); anything else becomes `\text{…}`.
        let s = text.text.as_str();
        if let Some(op) = symbol::operator_name(s) {
            self.push_token(op);
        } else {
            self.push_token("\\text{");
            // `\text{}` body: escape LaTeX specials.
            for c in s.chars() {
                push_text_escaped(&mut self.buf, c);
            }
            self.buf.push('}');
        }
        Ok(())
    }

    // -- Fractions ----------------------------------------------------------

    fn emit_fraction(&mut self, frac: &FractionItem) -> Result<()> {
        if frac.line {
            self.push_token("\\frac{");
            self.emit_group_body(&frac.numerator)?;
            self.buf.push_str("}{");
            self.emit_group_body(&frac.denominator)?;
            self.buf.push('}');
        } else {
            // A line-less stack. `\atop`/`\genfrac` are not understood by
            // texmath, so use `\binom`, the one widely-supported no-bar form.
            // It draws parentheses, which is correct for the overwhelmingly
            // common case (this fraction is the inner body of `binom(..)`); the
            // outer `Fenced` parens are then collapsed in `emit_fenced` to avoid
            // doubling.
            self.emit_binom(&frac.numerator, &frac.denominator)?;
        }
        Ok(())
    }

    /// Emits `\binom{num}{den}` (a parens-drawing no-bar fraction).
    fn emit_binom(&mut self, num: &MathItem, den: &MathItem) -> Result<()> {
        self.push_token("\\binom{");
        self.emit_group_body(num)?;
        self.buf.push_str("}{");
        self.emit_group_body(den)?;
        self.buf.push('}');
        Ok(())
    }

    fn emit_skewed_fraction(&mut self, frac: &SkewedFractionItem) -> Result<()> {
        // Skewed (inline) fraction: `a/b` rendered with a slash.
        self.push_token("{");
        self.emit_group(&frac.numerator)?;
        self.buf.push_str(" / ");
        self.emit_group(&frac.denominator)?;
        self.buf.push('}');
        Ok(())
    }

    // -- Radicals -----------------------------------------------------------

    fn emit_radical(&mut self, rad: &RadicalItem) -> Result<()> {
        match &rad.index {
            None => {
                self.push_token("\\sqrt{");
                self.emit_group_body(&rad.radicand)?;
                self.buf.push('}');
            }
            Some(index) => {
                self.push_token("\\sqrt[");
                self.emit_group_body(index)?;
                self.buf.push_str("]{");
                self.emit_group_body(&rad.radicand)?;
                self.buf.push('}');
            }
        }
        Ok(())
    }

    // -- Scripts / n-ary ----------------------------------------------------

    fn emit_scripts(&mut self, scripts: &ScriptsItem) -> Result<()> {
        // Pre-scripts (`{}^{a}_{b} base`) are uncommon; tensor notation requires
        // an empty-brace base.
        let has_pre = scripts.top_left.is_some() || scripts.bottom_left.is_some();
        if has_pre {
            self.push_token("{}");
            if let Some(tl) = &scripts.top_left {
                self.buf.push('^');
                self.emit_group(tl)?;
            }
            if let Some(bl) = &scripts.bottom_left {
                self.buf.push('_');
                self.emit_group(bl)?;
            }
        }

        // The base. An n-ary/large operator base (∑ ∏ ⋃ …) must be emitted
        // *unbraced* so a following `\limits` attaches to it (`{\sum}\limits`
        // is a LaTeX error). When it carries top/bottom limits we add `\limits`
        // so they stack as in display math; integrals keep sub/sup naturally.
        let nary = is_nary_base(&scripts.base);
        if nary {
            self.emit_group_body(&scripts.base)?;
            let lim = scripts.top.is_some() || scripts.bottom.is_some();
            if lim {
                self.buf.push_str("\\limits");
            }
        } else {
            self.emit_group(&scripts.base)?;
        }

        // Right scripts and under/over limits both map to `_`/`^` in LaTeX.
        let sup = scripts.top_right.as_ref().or(scripts.top.as_ref());
        let sub = scripts.bottom_right.as_ref().or(scripts.bottom.as_ref());
        if let Some(sub) = sub {
            self.buf.push('_');
            self.emit_group(sub)?;
        }
        if let Some(sup) = sup {
            self.buf.push('^');
            self.emit_group(sup)?;
        }
        Ok(())
    }

    // -- Accents and bars ---------------------------------------------------

    fn emit_accent(&mut self, acc: &AccentItem) -> Result<()> {
        let chr = accent_char(&acc.accent);
        let cmd = match chr {
            Some(c) => accent_command(c, acc.position).ok_or(Unrepresentable)?,
            None => return Err(Unrepresentable),
        };
        self.push_token(cmd);
        self.buf.push('{');
        self.emit_group_body(&acc.base)?;
        self.buf.push('}');
        Ok(())
    }

    fn emit_bar(&mut self, base: &MathItem, position: Position) -> Result<()> {
        let cmd = match position {
            Position::Above => "\\overline{",
            Position::Below => "\\underline{",
        };
        self.push_token(cmd);
        self.emit_group_body(base)?;
        self.buf.push('}');
        Ok(())
    }

    // -- Delimiters ---------------------------------------------------------

    fn emit_fenced(&mut self, fenced: &FencedItem) -> Result<()> {
        // `binom(n, k)` arrives as `( <no-bar Fraction> )`. `\binom` already
        // draws the parens, so collapse the `(`-fence + inner no-bar fraction
        // into a single `\binom` to avoid doubled parentheses.
        if delimiter_char_is(fenced.open.as_ref(), '(')
            && delimiter_char_is(fenced.close.as_ref(), ')')
            && let Some((num, den)) = single_nobar_fraction(&fenced.body)
        {
            return self.emit_binom(num, den);
        }

        let beg = fenced
            .open
            .as_ref()
            .and_then(delimiter_char)
            .map(|c| latex_delim(c))
            .unwrap_or(Some("."))
            .ok_or(Unrepresentable)?;
        let end = fenced
            .close
            .as_ref()
            .and_then(delimiter_char)
            .map(|c| latex_delim(c))
            .unwrap_or(Some("."))
            .ok_or(Unrepresentable)?;

        self.push_token("\\left");
        self.buf.push_str(beg);
        self.buf.push(' ');
        self.emit_row(&fenced.body)?;
        self.buf.push_str(" \\right");
        self.buf.push_str(end);
        Ok(())
    }

    // -- Tables / matrices --------------------------------------------------

    fn emit_table(&mut self, table: &TableItem) -> Result<()> {
        // A bare `matrix` (no delimiters); when wrapped in a `Fenced`, the
        // `\left..\right` come from the parent, and pandoc/texmath round-trips
        // `\left(\begin{matrix}…\end{matrix}\right)` into a `pmatrix` fine.
        self.push_token("\\begin{matrix} ");
        self.emit_table_rows(&table.cells)?;
        self.buf.push_str(" \\end{matrix}");
        Ok(())
    }

    fn emit_table_rows(&mut self, cells: &[Vec<AlignedRow>]) -> Result<()> {
        for (r, row) in cells.iter().enumerate() {
            if r > 0 {
                self.buf.push_str(" \\\\ ");
            }
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    self.buf.push_str(" & ");
                }
                // A cell is an `AlignedRow` of sub-columns; flatten its columns
                // (alignment points inside a matrix cell are not separately
                // expressible) into a single LaTeX expression.
                for sub in cell.iter() {
                    self.emit_item(sub)?;
                }
            }
        }
        Ok(())
    }

    fn emit_multiline(&mut self, multi: &MultilineItem) -> Result<()> {
        // `align`/`gather`/`cases` rows → `\begin{aligned} … \end{aligned}`.
        // Alignment columns within a row become `&` separators (the same `&`
        // texmath expects for an aligned environment).
        self.push_token("\\begin{aligned} ");
        for (r, row) in multi.rows.iter().enumerate() {
            if r > 0 {
                self.buf.push_str(" \\\\ ");
            }
            for (c, col) in row.iter().enumerate() {
                if c > 0 {
                    self.buf.push_str(" & ");
                }
                self.emit_item(col)?;
            }
        }
        self.buf.push_str(" \\end{aligned}");
        Ok(())
    }

    // -- Helpers ------------------------------------------------------------

    /// Emits an item already wrapped so it forms a single LaTeX group `{…}`
    /// (suitable as a script/argument). A bare single glyph is left unbraced
    /// only when it is one token; otherwise braces guarantee grouping.
    fn emit_group(&mut self, item: &MathItem) -> Result<()> {
        self.push_token("{");
        self.emit_group_body(item)?;
        self.buf.push('}');
        Ok(())
    }

    /// Emits an item's contents *without* adding braces (the caller already
    /// opened a group / argument). Flattens a leading group so we don't double
    /// brace.
    fn emit_group_body(&mut self, item: &MathItem) -> Result<()> {
        self.emit_items(item.as_slice())
    }

    /// Pushes a token, inserting a single separating space first when the token
    /// starts with a backslash-letter command and the previous char was also a
    /// command letter — preventing `\alpha` + `beta` → `\alphabeta`. Cheap and
    /// always-safe (extra math spaces are insignificant in LaTeX).
    fn push_token(&mut self, token: &str) {
        if needs_space(&self.buf, token) {
            self.buf.push(' ');
        }
        self.buf.push_str(token);
    }
}

/// Whether inserting `token` after the current buffer needs a separating space
/// to avoid gluing a control word to a following letter.
fn needs_space(buf: &str, token: &str) -> bool {
    let Some(prev) = buf.chars().last() else { return false };
    // If the buffer currently ends inside a control word (`…\alpha`) and the new
    // token begins with an ASCII letter, they would glue.
    let token_starts_letter = token.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    prev.is_ascii_alphabetic() && token_starts_letter && buf_ends_in_control_word(buf)
}

/// Whether `buf` ends inside a `\command` control word (a backslash followed by
/// one or more ASCII letters, with no intervening break).
fn buf_ends_in_control_word(buf: &str) -> bool {
    let mut saw_letter = false;
    for c in buf.chars().rev() {
        if c.is_ascii_alphabetic() {
            saw_letter = true;
        } else if c == '\\' {
            return saw_letter;
        } else {
            return false;
        }
    }
    false
}

/// Escapes a char for the body of a `\text{…}` group.
fn push_text_escaped(buf: &mut String, c: char) {
    match c {
        '\\' => buf.push_str("\\textbackslash{}"),
        '{' => buf.push_str("\\{"),
        '}' => buf.push_str("\\}"),
        '$' => buf.push_str("\\$"),
        '&' => buf.push_str("\\&"),
        '#' => buf.push_str("\\#"),
        '%' => buf.push_str("\\%"),
        '_' => buf.push_str("\\_"),
        '~' => buf.push_str("\\textasciitilde{}"),
        '^' => buf.push_str("\\textasciicircum{}"),
        _ => buf.push(c),
    }
}

/// Whether the base of a scripts item is a large/n-ary operator that should use
/// `\limits` for stacked top/bottom limits (∑ ∏ ⋃ ⋂ ⋀ ⋁ ∐ …, but *not*
/// integrals, which keep sub/sup).
fn is_nary_base(item: &MathItem) -> bool {
    let MathItem::Component(comp) = item else { return false };
    let MathKind::Glyph(glyph) = &comp.kind else { return false };
    let mut chars = glyph.text.chars();
    let Some(c) = chars.next() else { return false };
    if chars.next().is_some() {
        return false;
    }
    matches!(
        c,
        '∑' | '∏' | '∐' | '⋃' | '⋂' | '⋁' | '⋀' | '⨄' | '⨆' | '⨅' | '⨀' | '⨁' | '⨂'
    ) || c == '⋃'
}

/// Extracts a single-character glyph from a fence/accent side item.
fn delimiter_char(item: &MathItem) -> Option<char> {
    single_glyph_char(item)
}

/// Whether the optional fence side is exactly the given single-char delimiter.
fn delimiter_char_is(item: Option<&MathItem>, want: char) -> bool {
    item.and_then(delimiter_char) == Some(want)
}

/// If the fenced body is (after flattening a leading group) a single no-bar
/// `Fraction`, returns its numerator and denominator — used to collapse
/// `( <no-bar fraction> )` into `\binom`.
fn single_nobar_fraction<'a, 'b>(
    body: &'b MathItem<'a>,
) -> Option<(&'b MathItem<'a>, &'b MathItem<'a>)> {
    let items = body.as_slice();
    let [MathItem::Component(comp)] = items else { return None };
    let MathKind::Fraction(frac) = &comp.kind else { return None };
    (!frac.line).then_some((&frac.numerator, &frac.denominator))
}

fn single_glyph_char(item: &MathItem) -> Option<char> {
    let MathItem::Component(comp) = item else { return None };
    let MathKind::Glyph(glyph) = &comp.kind else { return None };
    let mut chars = glyph.text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// Maps a delimiter char to its LaTeX form for `\left`/`\right`. `Some(".")` is
/// the LaTeX "no delimiter". Returns `None` for an unrepresentable delimiter
/// (→ caller rasterizes).
fn latex_delim(c: char) -> Option<&'static str> {
    Some(match c {
        '(' => "(",
        ')' => ")",
        '[' => "[",
        ']' => "]",
        '{' => "\\{",
        '}' => "\\}",
        '|' => "|",
        '‖' | '∥' => "\\|",
        '⟨' => "\\langle",
        '⟩' => "\\rangle",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        '.' => ".",
        '/' => "/",
        '\\' => "\\backslash",
        _ => return None,
    })
}

/// Extracts the accent mark character from an accent's mark sub-item.
fn accent_char(item: &MathItem) -> Option<char> {
    single_glyph_char(item).or_else(|| {
        // The mark may be wrapped; take the first glyph char found.
        let MathItem::Component(comp) = item else { return None };
        let MathKind::Glyph(glyph) = &comp.kind else { return None };
        glyph.text.chars().next()
    })
}

/// Maps an accent mark char (spacing or combining) to its LaTeX accent command,
/// honoring above/below placement. Returns `None` for an unmapped mark.
fn accent_command(c: char, position: Position) -> Option<&'static str> {
    // Below-accents: only a few have LaTeX commands; underbrace/underline are
    // handled by `Line`, so an under-accent here is e.g. an under-arrow.
    if position == Position::Below {
        return Some(match c {
            '→' | '\u{20D7}' => "\\underrightarrow",
            '←' | '\u{20D6}' => "\\underleftarrow",
            '~' | '\u{0303}' => "\\utilde",
            _ => return None,
        });
    }
    Some(match c {
        '^' | '\u{0302}' | 'ˆ' => "\\hat",
        '~' | '\u{0303}' | '˜' => "\\tilde",
        '¯' | '\u{0304}' | '\u{02C9}' | '‾' => "\\bar",
        '\u{0305}' => "\\overline",
        '˙' | '\u{0307}' => "\\dot",
        '¨' | '\u{0308}' => "\\ddot",
        '\u{20DB}' => "\\dddot",
        '`' | '\u{0300}' => "\\grave",
        '´' | '\u{0301}' => "\\acute",
        '˘' | '\u{0306}' => "\\breve",
        'ˇ' | '\u{030C}' => "\\check",
        '°' | '˚' | '\u{030A}' => "\\mathring",
        '→' | '\u{20D7}' => "\\vec",
        '↼' => "\\overleftharpoon",
        '⇀' => "\\overrightharpoon",
        '↔' | '\u{20E1}' => "\\overleftrightarrow",
        _ => return None,
    })
}


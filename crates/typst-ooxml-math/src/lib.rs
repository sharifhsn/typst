//! Convert OOXML math (OMML, the `<m:oMath>` XML Microsoft Word uses for
//! equations) into **idiomatic** Typst math source.
//!
//! This is the inverse of the OMML *generator* in `crates/typst-docx`: that
//! crate walks Typst's math IR and emits `m:f` (fractions), `m:sSup`/`m:sSub`
//! (scripts), `m:rad` (radicals), `m:nary` (∑/∫/∏), `m:d` (delimiters), `m:m`
//! (matrices), `m:acc` (accents) and italicises variables by remapping letters
//! to their Plane-1 math-alphanumeric codepoints. Here we recognise those exact
//! shapes and turn them back into the Typst a user would actually write.
//!
//! The design goal is *paste-ready* output. Where the popular docx→typst path
//! (pandoc) produces `upright("𝑎")^(upright("2"))` for `a^2`, this crate emits
//! `a^2`. Concretely that means:
//!
//! * Plane-1 italic variables (𝑎, 𝑥, 𝛼) become bare `a`, `x`, `alpha` — never
//!   `upright(..)`. Only genuinely upright ASCII text becomes `upright(..)` /
//!   `op(..)` / a recognised function name.
//! * Every symbol codepoint is mapped to the shortest idiomatic Typst token
//!   (`≤` → `<=`, `×` → `times`, `→` → `->`, `∈` → `in`).
//! * Sub/superscripts are minimally parenthesised: `a^2`, `x_i`, but `e^(x+1)`.
//! * `m:nary` becomes `sum_(..)^(..)`, `integral_(..)^(..)`, `product_(..)`.
//! * `m:d` becomes real parens/brackets/`abs()`/`norm()`/`floor()`/`ceil()`
//!   where the delimiters make it obvious, otherwise `lr(..)`.
//! * `m:m`/`m:eqArr` become `mat(a, b; c, d)` / `cases`-style multi-line.
//! * `m:acc` becomes `hat`/`bar`/`dot`/`tilde`/`vec`/`arrow` as appropriate.
//!
//! Anything unrecognised degrades gracefully to a readable best-effort; the
//! converter never panics on malformed or unexpected OMML.

mod symbols;

use roxmltree::{Document, Node};
use symbols::{Style, math_alpha, symbol_name};

/// Convert an OMML fragment into idiomatic Typst math source.
///
/// `omml_xml` may be a bare `<m:oMath>…</m:oMath>` element, an
/// `<m:oMathPara>` wrapper, or a document containing one — the first `oMath`
/// element found is converted. The returned string is what you would place
/// between `$…$` (it does not include the dollar signs). On a parse error the
/// input is returned lightly cleaned so the caller still gets *something*
/// usable rather than an error.
pub fn omml_to_typst(omml_xml: &str) -> String {
    // The `m:` prefix is meaningless without its namespace binding, and real
    // fragments extracted from `document.xml` often omit it. Wrap the fragment
    // in a root that declares the math namespace so `roxmltree` parses the
    // prefixed elements. If the input already carries the declaration this
    // extra wrapper is harmless (its own declaration shadows nothing).
    let wrapped = format!(
        "<root xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\">{omml_xml}</root>"
    );
    let doc = match Document::parse(&wrapped) {
        Ok(doc) => doc,
        // Fall back to parsing the raw input (it may declare its own ns).
        Err(_) => match Document::parse(omml_xml) {
            Ok(doc) => return convert_doc(&doc),
            Err(_) => return sanitize_fallback(omml_xml),
        },
    };
    convert_doc(&doc)
}

fn convert_doc(doc: &Document) -> String {
    let root = doc.root_element();
    // Locate the first `oMath` element (possibly the root itself).
    let math = if local(root) == "oMath" {
        Some(root)
    } else {
        descendants(root).find(|n| local(*n) == "oMath")
    };
    match math {
        Some(node) => {
            let out = convert_row(node);
            out.trim().to_string()
        }
        // No math element: convert whatever children we can (best-effort).
        None => convert_row(root).trim().to_string(),
    }
}

/// A last-resort textual cleanup for input we could not parse as XML at all:
/// strip tags, keep text, and map any stray math codepoints to Typst names.
fn sanitize_fallback(input: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let mut out = String::new();
    for c in text.chars() {
        push_char_token(&mut out, c);
        out.push(' ');
    }
    out.trim().to_string()
}

// ===========================================================================
// Core: convert an element's children into a spaced sequence of atoms.
// ===========================================================================

/// An emitted atom together with whether it is a single "tight" token (a lone
/// letter/number/symbol that can be a script base or operand without parens)
/// or a compound expression that must be parenthesised when used as one.
struct Atom {
    text: String,
    /// True if `text` is a single token safe to place before `^`/`_` or use as
    /// a script operand without wrapping in parens (e.g. `a`, `2`, `alpha`,
    /// `x_i` counts as tight because it already binds, but `x + 1` does not).
    tight: bool,
}

impl Atom {
    fn tight(text: impl Into<String>) -> Self {
        Atom { text: text.into(), tight: true }
    }
    fn loose(text: impl Into<String>) -> Self {
        Atom { text: text.into(), tight: false }
    }
}

/// Convert the children of `node` (an `m:oMath`, `m:e`, `m:num`, …) into Typst
/// source, joining atoms with single spaces where needed.
fn convert_row(node: Node) -> String {
    let atoms = convert_children(node);
    join_atoms(&atoms)
}

/// Convert the children of `node` into a list of atoms, in document order.
fn convert_children(node: Node) -> Vec<Atom> {
    let mut atoms = Vec::new();
    for child in node.children() {
        convert_element(child, &mut atoms);
    }
    atoms
}

/// Convert a single OMML element into zero or more atoms, appended to `out`.
fn convert_element(node: Node, out: &mut Vec<Atom>) {
    if !node.is_element() {
        return;
    }
    match local(node) {
        "r" => convert_run(node, out),
        "f" => out.push(convert_fraction(node)),
        "rad" => out.push(convert_radical(node)),
        "sSup" => out.push(convert_ssup(node)),
        "sSub" => out.push(convert_ssub(node)),
        "sSubSup" => out.push(convert_ssubsup(node)),
        "sPre" => out.push(convert_spre(node)),
        "nary" => out.push(convert_nary(node)),
        "d" => out.push(convert_delim(node)),
        "m" => out.push(convert_matrix(node)),
        "eqArr" => out.push(convert_eqarr(node)),
        "acc" => out.push(convert_accent(node)),
        "bar" => out.push(convert_bar(node)),
        "groupChr" => out.push(convert_groupchr(node)),
        "borderBox" | "box" => out.push(convert_box(node)),
        "func" => out.push(convert_func(node)),
        "limLow" => out.push(convert_limlow(node)),
        "limUpp" => out.push(convert_limupp(node)),
        // A grouping/phantom wrapper: recurse transparently.
        "e" | "num" | "den" | "oMath" | "phant" => {
            out.extend(convert_children(node));
        }
        // Property blocks carry no rendered content.
        name if name.ends_with("Pr") => {}
        // Unknown element: recurse so we still surface any text inside it.
        _ => out.extend(convert_children(node)),
    }
}

// ===========================================================================
// Runs and text.
// ===========================================================================

/// Convert an `m:r` run (a sequence of `m:t` text). Splits the run text into
/// atoms so that e.g. a variable followed by an operator become separate atoms
/// with correct spacing, and multi-letter upright words are wrapped once.
fn convert_run(node: Node, out: &mut Vec<Atom>) {
    // Whether the run is explicitly upright (`m:nor`). Note: the DOCX exporter
    // sets `m:nor` on *every* glyph, including already-italic Plane-1 letters,
    // so `nor` alone does NOT mean "wrap in upright" — we decide per character
    // from the codepoint. `nor` only matters for disambiguating plain ASCII
    // letters that a real Word author typed upright.
    let text = collect_text(node);
    if text.is_empty() {
        return;
    }
    tokenize_text(&text, out);
}

/// Gather the concatenated text of all `m:t` descendants of a run.
fn collect_text(node: Node) -> String {
    let mut s = String::new();
    for t in node.children().filter(|n| local(*n) == "t") {
        if let Some(text) = t.text() {
            s.push_str(text);
        }
    }
    s
}

/// Split a run's text into idiomatic Typst atoms. Consecutive ASCII letters
/// that are *upright* (plain, not Plane-1) form a word: a recognised function
/// name is emitted bare, an unknown word is wrapped in `upright("..")`. Italic
/// Plane-1 letters, digits, and symbols each become their own atom.
fn tokenize_text(text: &str, out: &mut Vec<Atom>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' {
            i += 1;
            continue;
        }
        // Run of plain upright ASCII letters → a word (function or upright text).
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            push_word(&word, out);
            continue;
        }
        // Run of digits → a number.
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let num: String = chars[start..i].iter().collect();
            out.push(Atom::tight(num));
            continue;
        }
        // A styled Plane-1 letter/digit, a named symbol, or a literal.
        let mut atom = String::new();
        let tight = push_char_token(&mut atom, c);
        out.push(if tight { Atom::tight(atom) } else { Atom::loose(atom) });
        i += 1;
    }
}

/// Push a whole upright ASCII word. Multi-letter words that Typst recognises as
/// built-in operators (`sin`, `lim`, `max`, …) are emitted bare; other words
/// are wrapped so they stay upright and don't render as spaced italic letters.
fn push_word(word: &str, out: &mut Vec<Atom>) {
    let lower = word.to_ascii_lowercase();
    if word.len() == 1 {
        // A single upright ASCII letter. In Typst a bare letter is italic, so a
        // deliberately upright single letter needs `upright(..)`. But the very
        // common case is a Plane-1 letter that already decoded to ASCII; those
        // arrive via `push_char_token`, not here. A lone ASCII letter in a real
        // Word doc is usually an upright constant (e.g. the differential `d`).
        // Keep it bare: over-wrapping single letters hurts far more common
        // cases than it helps, and Typst users rarely mean upright here.
        out.push(Atom::tight(word.to_string()));
        return;
    }
    if is_known_function(&lower) {
        out.push(Atom::tight(lower));
    } else {
        // Preserve as upright text so it renders as a word, not italic letters.
        out.push(Atom::tight(format!("upright(\"{word}\")")));
    }
}

/// Push the Typst token for a single character to `buf`; returns whether the
/// result is a *tight* token (safe as a bare script base/operand).
fn push_char_token(buf: &mut String, c: char) -> bool {
    // A styled Plane-1 math-alphanumeric letter/digit: recover base + wrapper.
    if let Some((base, style)) = math_alpha(c) {
        let mut inner = String::new();
        // The base may itself be a Greek letter needing a symbol name.
        if let Some(name) = symbol_name(base) {
            inner.push_str(name);
        } else {
            inner.push(base);
        }
        match style {
            Style::Italic => {
                buf.push_str(&inner);
                return true;
            }
            Style::Bold => buf.push_str(&format!("bold({inner})")),
            Style::Cal => buf.push_str(&format!("cal({inner})")),
            Style::Frak => buf.push_str(&format!("frak({inner})")),
            Style::Bb => buf.push_str(&format!("bb({inner})")),
            Style::Sans => buf.push_str(&format!("sans({inner})")),
            Style::SansBold => buf.push_str(&format!("bold(sans({inner}))")),
            Style::Mono => buf.push_str(&format!("mono({inner})")),
        }
        return true;
    }
    // A named symbol (Greek, operators, arrows, set theory, …).
    if let Some(name) = symbol_name(c) {
        buf.push_str(name);
        // A shorthand like `<=` or `->` is a single relation token; a name like
        // `alpha` is tight. Both are safe as operands, so report tight.
        return true;
    }
    // Otherwise emit the character literally (ASCII operators, punctuation,
    // and any Unicode we have no name for).
    buf.push(c);
    // ASCII letters/digits are tight; operators are their own tokens.
    true
}

// ===========================================================================
// Structural constructs.
// ===========================================================================

/// `m:f` — fraction. `frac(num, den)`, or `binom(..)` when the bar is hidden.
fn convert_fraction(node: Node) -> Atom {
    let num = convert_row(child(node, "num").unwrap_or(node));
    let den = convert_row(child(node, "den").unwrap_or(node));
    // A `noBar` fraction is a stacked pair — most often a binomial.
    let ty = frac_type(node);
    match ty {
        FracType::NoBar => Atom::tight(format!("binom({}, {})", trim_arg(&num), trim_arg(&den))),
        FracType::Skewed | FracType::Linear => {
            // A skewed/linear fraction: `a\/b` reads best as `num\/den`.
            Atom::loose(format!("{}\\/{}", paren_if_loose(&num), paren_if_loose(&den)))
        }
        FracType::Bar => {
            Atom::tight(format!("frac({}, {})", trim_arg(&num), trim_arg(&den)))
        }
    }
}

enum FracType {
    Bar,
    NoBar,
    Skewed,
    Linear,
}

fn frac_type(node: Node) -> FracType {
    let Some(pr) = child(node, "fPr") else { return FracType::Bar };
    let Some(ty) = child(pr, "type") else { return FracType::Bar };
    match mval(ty).as_deref() {
        Some("noBar") => FracType::NoBar,
        Some("skw") => FracType::Skewed,
        Some("lin") => FracType::Linear,
        _ => FracType::Bar,
    }
}

/// `m:rad` — radical. `sqrt(x)` or `root(n, x)` with a degree.
fn convert_radical(node: Node) -> Atom {
    let radicand = convert_row(child(node, "e").unwrap_or(node));
    let deg = child(node, "deg").map(convert_row).unwrap_or_default();
    // A hidden or empty degree ⇒ square root.
    if deg.trim().is_empty() || deg_hidden(node) {
        Atom::tight(format!("sqrt({})", trim_arg(&radicand)))
    } else {
        Atom::tight(format!("root({}, {})", trim_arg(&deg), trim_arg(&radicand)))
    }
}

fn deg_hidden(node: Node) -> bool {
    child(node, "radPr")
        .and_then(|pr| child(pr, "degHide"))
        .map(|h| is_on(h))
        .unwrap_or(false)
}

/// `m:sSup` — superscript. `a^2`, `e^(x+1)`.
fn convert_ssup(node: Node) -> Atom {
    let base = convert_base_atom(child(node, "e"));
    let sup = convert_row(child(node, "sup").unwrap_or(node));
    Atom::tight(format!("{}^{}", base, script_operand(&sup)))
}

/// `m:sSub` — subscript. `x_i`, `a_(i+1)`.
fn convert_ssub(node: Node) -> Atom {
    let base = convert_base_atom(child(node, "e"));
    let sub = convert_row(child(node, "sub").unwrap_or(node));
    Atom::tight(format!("{}_{}", base, script_operand(&sub)))
}

/// `m:sSubSup` — combined sub- and superscript. `x_i^2`.
fn convert_ssubsup(node: Node) -> Atom {
    let base = convert_base_atom(child(node, "e"));
    let sub = convert_row(child(node, "sub").unwrap_or(node));
    let sup = convert_row(child(node, "sup").unwrap_or(node));
    Atom::tight(format!("{}_{}^{}", base, script_operand(&sub), script_operand(&sup)))
}

/// `m:sPre` — pre-scripts. `""^n_k x` style; Typst uses `attach(..)`.
fn convert_spre(node: Node) -> Atom {
    let base = convert_row(child(node, "e").unwrap_or(node));
    let sub = child(node, "sub").map(convert_row).unwrap_or_default();
    let sup = child(node, "sup").map(convert_row).unwrap_or_default();
    let mut args = paren_if_loose(&base);
    if !sup.trim().is_empty() {
        args.push_str(&format!(", tl: {}", trim_arg(&sup)));
    }
    if !sub.trim().is_empty() {
        args.push_str(&format!(", bl: {}", trim_arg(&sub)));
    }
    Atom::tight(format!("attach({args})"))
}

/// `m:nary` — n-ary operator (∑ ∫ ∏ …) with optional limits and an integrand.
fn convert_nary(node: Node) -> Atom {
    let pr = child(node, "naryPr");
    // The operator char defaults to ∫ when `m:chr` is absent (Word's rule).
    let chr = pr
        .and_then(|pr| child(pr, "chr"))
        .and_then(|c| mval(c))
        .and_then(|s| s.chars().next())
        .unwrap_or('∫');
    let op = symbol_name(chr).map(str::to_string).unwrap_or_else(|| chr.to_string());

    let sub_hidden = hidden(pr, "subHide");
    let sup_hidden = hidden(pr, "supHide");
    let sub = if sub_hidden { String::new() } else { child(node, "sub").map(convert_row).unwrap_or_default() };
    let sup = if sup_hidden { String::new() } else { child(node, "sup").map(convert_row).unwrap_or_default() };
    let body = child(node, "e").map(convert_row).unwrap_or_default();

    let mut s = op;
    if !sub.trim().is_empty() {
        s.push('_');
        s.push_str(&script_operand(&sub));
    }
    if !sup.trim().is_empty() {
        s.push('^');
        s.push_str(&script_operand(&sup));
    }
    if !body.trim().is_empty() {
        s.push(' ');
        s.push_str(body.trim());
    }
    // The whole thing (`sum_(i=1)^n x_i`) is a loose expression.
    Atom::loose(s)
}

/// `m:limLow` — a limit below a base (`lim_(x -> 0)`, `min_(..)`, `underbrace`).
fn convert_limlow(node: Node) -> Atom {
    let base = convert_base_atom(child(node, "e"));
    let lim = convert_row(child(node, "lim").unwrap_or(node));
    if lim.trim().is_empty() {
        return Atom::tight(base);
    }
    Atom::loose(format!("{}_{}", base, script_operand(&lim)))
}

/// `m:limUpp` — a limit above a base (`overbrace`-style / over-limit).
fn convert_limupp(node: Node) -> Atom {
    let base = convert_base_atom(child(node, "e"));
    let lim = convert_row(child(node, "lim").unwrap_or(node));
    if lim.trim().is_empty() {
        return Atom::tight(base);
    }
    Atom::loose(format!("{}^{}", base, script_operand(&lim)))
}

/// `m:d` — delimiters. Map obvious pairs to real Typst syntax, else `lr(..)`.
fn convert_delim(node: Node) -> Atom {
    let pr = child(node, "dPr");
    let beg = delim_char(pr, "begChr", '(');
    let end = delim_char(pr, "endChr", ')');

    // Special case: a fence whose *sole* content is a matrix. Word represents
    // `mat`/`vec`/`cases` as a delimiter wrapping an `m:m`. Recover the native
    // Typst constructor instead of double-wrapping the matrix in parens.
    if let Some(matrix) = sole_matrix(node) {
        // A one-sided `{` fence ⇒ `cases(..)` (one case per row).
        if beg == Some('{') && end.is_none() {
            let rows = matrix_rows(matrix);
            return Atom::tight(format!("cases({})", rows.join(", ")));
        }
        // Default `( )` parens ⇒ bare `mat(..)` (mat's own default delimiter).
        if beg == Some('(') && end == Some(')') {
            return convert_matrix(matrix);
        }
        // Another delimiter ⇒ `mat(delim: "[", ..)`.
        if let (Some(b), Some(_)) = (beg, end) {
            let inner = convert_matrix(matrix);
            let body = inner.text.trim_start_matches("mat(").to_string();
            return Atom::tight(format!("mat(delim: \"{b}\", {body}"));
        }
    }

    // Multiple `m:e` cells ⇒ a delimiter-separated list; join the cells.
    let cells: Vec<String> = node
        .children()
        .filter(|n| local(*n) == "e")
        .map(convert_row)
        .collect();
    let inner = if cells.len() > 1 {
        // Word uses separators (default `|`) between cells; join with `,` which
        // is the common Typst reading of a fenced list.
        cells.join(", ")
    } else {
        cells.into_iter().next().unwrap_or_default()
    };
    let inner = inner.trim().to_string();

    match (beg, end) {
        // `( )` around a construct that already carries its own parens
        // (`binom(..)`) — Word wraps `binom` in a paren delimiter, but Typst's
        // `binom` renders with parens itself, so drop the redundant fence.
        (Some('('), Some(')')) if inner.starts_with("binom(") && is_single_token(&inner) => {
            Atom::tight(inner)
        }
        (Some('('), Some(')')) => Atom::tight(format!("({inner})")),
        (Some('['), Some(']')) => Atom::tight(format!("[{inner}]")),
        (Some('{'), Some('}')) => Atom::tight(format!("{{{inner}}}")),
        (Some('|'), Some('|')) => Atom::tight(format!("abs({inner})")),
        (Some('‖'), Some('‖')) => Atom::tight(format!("norm({inner})")),
        (Some('⌊'), Some('⌋')) => Atom::tight(format!("floor({inner})")),
        (Some('⌈'), Some('⌉')) => Atom::tight(format!("ceil({inner})")),
        (Some('⟨'), Some('⟩')) => Atom::tight(format!("angle.l {inner} angle.r")),
        // A one-sided or unusual fence: fall back to explicit `lr(..)` with the
        // literal delimiter characters so nothing is lost.
        _ => {
            let l = beg.map(delim_token).unwrap_or_else(|| "\"\"".into());
            let r = end.map(delim_token).unwrap_or_else(|| "\"\"".into());
            Atom::tight(format!("lr({l} {inner} {r})"))
        }
    }
}

/// `m:m` — matrix. `mat(a, b; c, d)` with a semicolon between rows.
fn convert_matrix(node: Node) -> Atom {
    Atom::tight(format!("mat({})", matrix_rows(node).join("; ")))
}

/// Each matrix row rendered as a comma-joined string of its cells.
fn matrix_rows(node: Node) -> Vec<String> {
    node.children()
        .filter(|n| local(*n) == "mr")
        .map(|r| {
            r.children()
                .filter(|n| local(*n) == "e")
                .map(convert_row)
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect()
}

/// If `node`'s single `m:e` child contains exactly one `m:m` matrix (and no
/// other rendered content), return that matrix node. Used to unwrap the
/// delimiter Word puts around `mat`/`vec`/`cases`.
fn sole_matrix<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let cells: Vec<Node> = node.children().filter(|n| local(*n) == "e").collect();
    let [e] = cells.as_slice() else { return None };
    let elems: Vec<Node> = e.children().filter(Node::is_element).collect();
    match elems.as_slice() {
        [only] if local(*only) == "m" => Some(*only),
        _ => None,
    }
}

/// `m:eqArr` — an equation array (multi-line / `gather`). Rendered as a `cases`
/// when short, else joined with line breaks inside a `mat` column is overkill;
/// a plain `\ `-separated body reads best for gather-style stacks.
fn convert_eqarr(node: Node) -> Atom {
    let rows: Vec<String> = node
        .children()
        .filter(|n| local(*n) == "e")
        .map(convert_row)
        .map(|s| s.trim().to_string())
        .collect();
    // Newline-separate the lines with Typst's math line break `\`.
    Atom::loose(rows.join(" \\ "))
}

/// `m:acc` — accent. Map the combining mark to `hat`/`bar`/`dot`/`vec`/….
fn convert_accent(node: Node) -> Atom {
    let base = convert_row(child(node, "e").unwrap_or(node));
    let chr = child(node, "accPr")
        .and_then(|pr| child(pr, "chr"))
        .and_then(|c| mval(c))
        .and_then(|s| s.chars().next())
        // The OMML default accent (when `m:chr` is absent) is a combining hat.
        .unwrap_or('\u{0302}');
    let func = accent_func(chr).unwrap_or("hat");
    Atom::tight(format!("{func}({})", trim_arg(&base)))
}

/// `m:bar` — over/under bar. `overline(..)` / `underline(..)`.
fn convert_bar(node: Node) -> Atom {
    let base = convert_row(child(node, "e").unwrap_or(node));
    let pos = child(node, "barPr")
        .and_then(|pr| child(pr, "pos"))
        .and_then(|p| mval(p));
    let func = if pos.as_deref() == Some("bot") { "underline" } else { "overline" };
    Atom::tight(format!("{func}({})", trim_arg(&base)))
}

/// `m:groupChr` — a stretched grouping character (over/under brace, etc.).
fn convert_groupchr(node: Node) -> Atom {
    let base = convert_row(child(node, "e").unwrap_or(node));
    let pr = child(node, "groupChrPr");
    let chr = pr
        .and_then(|pr| child(pr, "chr"))
        .and_then(|c| mval(c))
        .and_then(|s| s.chars().next());
    let pos = pr.and_then(|pr| child(pr, "pos")).and_then(|p| mval(p));
    let below = pos.as_deref() == Some("bot");
    let func = match chr {
        Some('⏟') | Some('\u{FE38}') => if below { "underbrace" } else { "overbrace" },
        Some('⏞') => "overbrace",
        Some('⏝') | Some('⎵') => "underbracket",
        Some('⏜') | Some('⎴') => "overbracket",
        _ => {
            return if below {
                Atom::tight(format!("underline({})", trim_arg(&base)))
            } else {
                Atom::tight(format!("overline({})", trim_arg(&base)))
            };
        }
    };
    Atom::tight(format!("{func}({})", trim_arg(&base)))
}

/// `m:borderBox`/`m:box` — a boxed base; a struck box is `cancel(..)`.
fn convert_box(node: Node) -> Atom {
    let base = convert_row(child(node, "e").unwrap_or(node));
    // A diagonal strike ⇒ `cancel`.
    let struck = child(node, "borderBoxPr")
        .map(|pr| {
            child(pr, "strikeBLTR").map(is_on).unwrap_or(false)
                || child(pr, "strikeTLBR").map(is_on).unwrap_or(false)
                || child(pr, "strikeH").map(is_on).unwrap_or(false)
        })
        .unwrap_or(false);
    if struck {
        Atom::tight(format!("cancel({})", trim_arg(&base)))
    } else {
        Atom::loose(base)
    }
}

/// `m:func` — a named function application: `fName` applied to `e`. Real Word
/// uses this for `sin θ`, `lim …`, etc. `fName` is typically an upright word.
fn convert_func(node: Node) -> Atom {
    let name = child(node, "fName").map(convert_row).unwrap_or_default();
    let arg = child(node, "e").map(convert_row).unwrap_or_default();
    let name = name.trim();
    let arg = arg.trim();
    if arg.is_empty() {
        Atom::loose(name.to_string())
    } else {
        Atom::loose(format!("{name} {arg}"))
    }
}

// ===========================================================================
// Small helpers.
// ===========================================================================

/// Convert the base of a script (`m:e`) into a single string, parenthesising a
/// compound base so `(a+b)^2` binds correctly.
fn convert_base_atom(e: Option<Node>) -> String {
    let Some(e) = e else { return String::new() };
    let atoms = convert_children(e);
    match atoms.len() {
        0 => String::new(),
        1 => {
            let a = &atoms[0];
            if a.tight { a.text.clone() } else { format!("({})", a.text.trim()) }
        }
        _ => {
            let joined = join_atoms(&atoms);
            format!("({})", joined.trim())
        }
    }
}

/// Format a script sub/superscript operand: bare if it is a single tight token,
/// parenthesised otherwise. `a^2` stays `a^2`; `e^(x+1)` gets the parens.
/// Inside a script, a bound like `i = 1` reads more idiomatically tightened to
/// `i=1`, so we collapse spaces around `=` for the parenthesised form.
fn script_operand(s: &str) -> String {
    let t = s.trim();
    if is_single_token(t) {
        t.to_string()
    } else {
        format!("({})", tighten_bounds(t))
    }
}

/// Collapse the spaces around `=` in a script bound so `i = 1` becomes `i=1`
/// (the way users write `sum_(i=1)`). Other operators keep their spacing.
fn tighten_bounds(s: &str) -> String {
    s.replace(" = ", "=")
}

/// Whether `s` is a single Typst token safe to place bare after `^`/`_`.
/// A lone letter, digit, number, or symbol name; a `func(..)` call; or an
/// already-bracketed group. Anything containing a space or top-level operator
/// needs parentheses.
fn is_single_token(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // A single character.
    if s.chars().count() == 1 {
        return true;
    }
    // A bare identifier or number (letters/digits/underscore-free dotted name).
    if s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return true;
    }
    // A function call or bracketed group that spans the whole string:
    // `sqrt(x)`, `(a+b)`, `frac(a, b)`.
    if is_balanced_wrapped(s) {
        return true;
    }
    // A dotted symbol name like `arrow.r` (no spaces, no operators).
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        return true;
    }
    false
}

/// Whether `s` is a single `name(...)` call or `(...)`/`[...]` group spanning
/// the whole string with balanced delimiters.
fn is_balanced_wrapped(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Find the first opening bracket.
    let Some(open_pos) = s.find(['(', '[', '{']) else { return false };
    // Everything before it must be a bare identifier (the function name) or empty.
    if !s[..open_pos].chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        return false;
    }
    let open = bytes[open_pos];
    let close = match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return false,
    };
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open_pos) {
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                // Balanced closes exactly at the end ⇒ single wrapped group.
                return i == bytes.len() - 1;
            }
        }
    }
    false
}

/// Join atoms into a source string with idiomatic spacing. A leading `,`/`;`/
/// `!`/`'`/`.` binds tightly to what precedes it (no space before punctuation),
/// matching how a user writes `f(x), g(y)` and `x'`.
fn join_atoms(atoms: &[Atom]) -> String {
    let mut out = String::new();
    for a in atoms {
        let text = a.text.as_str();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() && !binds_tight_left(text) {
            out.push(' ');
        }
        out.push_str(text);
    }
    out
}

/// Whether `text` should attach with no leading space to the token before it.
/// Only *lone* punctuation binds tight — a multi-char atom like `!=` (from `≠`)
/// is a relation and keeps its space.
fn binds_tight_left(text: &str) -> bool {
    let mut chars = text.chars();
    let first = chars.next();
    let lone = chars.next().is_none();
    match first {
        Some(',' | ';' | '.') => true,
        Some('!' | '?' | '\'') => lone,
        _ => false,
    }
}

/// Trim an argument for use inside `f(..)`: collapse surrounding whitespace.
fn trim_arg(s: &str) -> String {
    s.trim().to_string()
}

/// Parenthesise a multi-token expression so it acts as one operand.
fn paren_if_loose(s: &str) -> String {
    let t = s.trim();
    if is_single_token(t) { t.to_string() } else { format!("({t})") }
}

/// Map a delimiter char to a Typst delimiter token for use inside `lr(..)`.
fn delim_token(c: char) -> String {
    match c {
        '(' | ')' | '[' | ']' | '|' => c.to_string(),
        '{' => "{".to_string(),
        '}' => "}".to_string(),
        _ => symbol_name(c).map(str::to_string).unwrap_or_else(|| format!("\"{c}\"")),
    }
}

/// Read a delimiter char from `dPr/<tag>`; falls back to `default` if absent,
/// and returns `None` for an explicitly empty (one-sided) delimiter.
fn delim_char(pr: Option<Node>, tag: &str, default: char) -> Option<char> {
    let Some(pr) = pr else { return Some(default) };
    let Some(node) = child(pr, tag) else { return Some(default) };
    match mval(node) {
        Some(s) if s.is_empty() => None,
        Some(s) => s.chars().next(),
        None => Some(default),
    }
}

/// Map a combining accent mark (or its spacing form) to a Typst accent function.
fn accent_func(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{0302}' | '^' => "hat",
        '\u{0303}' | '~' => "tilde",
        '\u{0304}' | '\u{00AF}' | '\u{02C9}' => "macron",
        '\u{0305}' => "overline",
        '\u{0300}' | '`' => "grave",
        '\u{0301}' | '\u{00B4}' => "acute",
        '\u{0307}' | '\u{02D9}' => "dot",
        '\u{0308}' | '\u{00A8}' => "dot.double",
        '\u{20DB}' => "dot.triple",
        '\u{030C}' | '\u{02C7}' => "caron",
        '\u{0306}' | '\u{02D8}' => "breve",
        '\u{030A}' | '\u{00B0}' => "circle",
        '\u{20D7}' | '\u{2192}' | '\u{2020}' => "arrow",
        '\u{20D6}' | '\u{2190}' => "arrow.l",
        '\u{20E1}' | '\u{2194}' => "arrow.l.r",
        _ => return None,
    })
}

/// Known multi-letter operators/functions Typst recognises as bare identifiers.
fn is_known_function(word: &str) -> bool {
    matches!(
        word,
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc"
            | "sinh" | "cosh" | "tanh" | "coth"
            | "arcsin" | "arccos" | "arctan"
            | "sech" | "csch"
            | "asin" | "acos" | "atan"
            | "log" | "ln" | "lg" | "exp"
            | "lim" | "limsup" | "liminf"
            | "max" | "min" | "sup" | "inf"
            | "arg" | "det" | "dim" | "ker" | "deg"
            | "gcd" | "hom" | "mod" | "Pr"
            | "sgn" | "tr" | "id"
    )
}

// ===========================================================================
// XML utilities.
// ===========================================================================

/// The local (namespace-stripped) tag name of an element.
fn local<'input>(node: Node<'_, 'input>) -> &'input str {
    node.tag_name().name()
}

/// The first element child of `node` with the given local name.
fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.is_element() && local(*n) == name)
}

/// All descendants (for locating `oMath`).
fn descendants<'a, 'input>(node: Node<'a, 'input>) -> impl Iterator<Item = Node<'a, 'input>> {
    node.descendants().filter(|n| n.is_element())
}

/// The `m:val` (or bare `val`) attribute of a property element.
fn mval(node: Node) -> Option<String> {
    node.attributes()
        .find(|a| a.name() == "val")
        .map(|a| a.value().to_string())
}

/// Whether a boolean property element is "on"/"1"/"true" (default: present ⇒ on).
fn is_on(node: Node) -> bool {
    matches!(mval(node).as_deref(), None | Some("on" | "1" | "true"))
}

/// Whether a hide flag (`subHide`/`supHide`/`degHide`) is set inside `pr`.
fn hidden(pr: Option<Node>, tag: &str) -> bool {
    pr.and_then(|pr| child(pr, tag)).map(is_on).unwrap_or(false)
}

#[cfg(test)]
mod tests;

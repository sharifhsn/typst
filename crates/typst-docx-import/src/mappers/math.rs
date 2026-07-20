//! The `math` mapper: OMML (`m:oMath`) → real Typst math source
//! ([`Inline::Math`]) — the inverse of `typst-docx`'s equation mapper
//! (`crates/typst-docx/src/mappers/math.rs`), which is the best reference for
//! which OMML shapes actually matter and how Word structures them: it walks
//! Typst's math IR and emits `m:f`/`m:sSup`/`m:sSub`/`m:rad`/`m:nary`/`m:d`/
//! `m:m`/`m:acc`/…, and remaps every letter to its Plane-1 math-alphanumeric
//! codepoint (`x` → 𝑥 U+1D465) because the *character itself* then carries
//! the italic. This module recognises those exact shapes and turns them back
//! into the Typst a user would actually write — replacing the previous
//! fallback path, which only linearized OMML into read-aloud text (so
//! `mat(1,2;3,4)` came back as `$(1234)$` and every accent/brace/limit was
//! silently dropped).
//!
//! Two hazards shape almost every decision below:
//!
//! 1. **A multi-letter run is a hard error in Typst math.** `$abc$` is
//!    `error: unknown variable: abc` — not a word, and not even a run of
//!    three italic letters. So the letters of a run must be emitted
//!    **space-separated** (`a b c`), *unless* the run is upright text (see
//!    hazard 2), in which case it becomes a quoted string (`"abc"`). Single
//!    letters and known operator/function names (`x`, `sin`, `alpha`) are
//!    fine as-is — this only bites multi-letter, unstyled runs.
//!
//! 2. **Mathematical-alphanumeric normalisation.** Word (and this repo's own
//!    exporter) writes math variables as U+1D400–U+1D7FF codepoints — `𝑥`
//!    (U+1D465), `𝛼` (U+1D6FC) — because *the character* already carries the
//!    italic; see `emit_glyph`'s doc comment in the exporter for why it also
//!    always sets `m:nor` on every glyph run (styled or not), which means
//!    `m:nor`'s presence alone does **not** distinguish italic from upright
//!    here the way it would for a hand-typed Word run. So: a folded
//!    math-alphanumeric letter is always a *variable* (space-separated per
//!    hazard 1, regardless of `m:nor`); a run of plain ASCII letters that
//!    *does* carry `m:nor` is upright *text* (`"…"`). Every fold target here
//!    was verified empirically against this repo's own exporter (see
//!    `fold_math_alphanumeric`'s doc comment) rather than derived from the
//!    Unicode block layout alone.
//!
//! The one non-negotiable, per the mapper's own design brief: **the emitted
//! source must always parse.** An equation that fails to compile breaks the
//! whole document, which is worse than a crude-but-valid approximation. So
//! every structural form below over-parenthesises rather than trying to be
//! minimal (`(e)^(sup)`, never a bare `e^sup` that might mis-bind), and
//! anything this mapper doesn't specifically recognise still recurses into
//! its children — never dropped, never spliced in as raw XML.

use ecow::{eco_format, EcoString};
use roxmltree::Node;

use crate::report::ImportReport;
use crate::tdoc::Inline;

/// Bounds recursion into deeply/adversarially nested OMML — the math
/// mapper's counterpart to `wml::parse`'s `MAX_WRAPPER_DEPTH`/
/// `MAX_TABLE_DEPTH`. Real equations never come close; a hostile document
/// nesting `m:d` inside itself thousands of times must not blow the stack.
const MAX_MATH_DEPTH: usize = 64;

pub fn omml_to_inline(fragment: &str, report: &mut ImportReport) -> Inline {
    let annotated = ensure_namespaces(fragment);
    let Ok(doc) = roxmltree::Document::parse(&annotated) else {
        report.drop("OMML equation", "could not parse as XML; dropped");
        return Inline::Text(EcoString::new());
    };
    let mut atoms = Vec::new();
    convert_element(doc.root_element(), &mut atoms, report, 0);
    let src = join_atoms(&atoms);
    if src.trim().is_empty() {
        report.drop("OMML equation", "no convertible content; dropped");
        return Inline::Text(EcoString::new());
    }
    Inline::Math(src)
}

/// [`roxmltree`] needs the `m:`/`w:` prefixes bound to parse a fragment at
/// all. Word's own OMML fragments usually declare `xmlns:m` themselves, but
/// an `m:r` occasionally carries a sibling `w:rPr` (a character-formatting
/// override), which uses the `w:` prefix — undeclared in a fragment that only
/// bothered to declare `m:`, so the whole equation would otherwise fail to
/// parse as XML. Declare whichever of the two is missing directly on the
/// fragment's own root element (rather than wrapping it in a synthetic outer
/// element), so the fragment stays a single well-formed root.
fn ensure_namespaces(fragment: &str) -> String {
    let has_m = fragment.contains("xmlns:m=");
    let has_w = fragment.contains("xmlns:w=");
    if has_m && has_w {
        return fragment.to_string();
    }
    let Some(gt) = fragment.find('>') else {
        return fragment.to_string();
    };
    if fragment[..gt].ends_with('/') {
        // A self-closing root tag (`<m:oMath/>`) — nothing to annotate.
        return fragment.to_string();
    }
    let mut extra = String::new();
    if !has_m {
        extra.push_str(" xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\"");
    }
    if !has_w {
        extra.push_str(" xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"");
    }
    format!("{}{}{}", &fragment[..gt], extra, &fragment[gt..])
}

// ===========================================================================
// Dispatch: one OMML element -> zero or more Typst-source atoms.
// ===========================================================================

/// Converts a single OMML element, appending the Typst-source atom(s) it
/// produces to `out`. An "atom" is a self-contained piece of source
/// (`"x"`, `"frac(a, b)"`, `"+"`, …); [`join_atoms`] space-separates them,
/// which is always safe (Typst's math layout computes inter-atom spacing
/// itself; source whitespace does not affect rendering) and is what keeps a
/// multi-letter run from becoming a single illegal identifier (hazard 1).
fn convert_element(node: Node, out: &mut Vec<EcoString>, report: &mut ImportReport, depth: usize) {
    if !node.is_element() {
        return;
    }
    if depth >= MAX_MATH_DEPTH {
        report.approximate(
            "OMML equation",
            "equation nesting exceeded the depth limit; flattened to text",
        );
        let text = plain_text(node);
        if !text.trim().is_empty() {
            out.push(quote(text.trim()));
        }
        return;
    }
    let next_depth = depth + 1;
    match local(node) {
        "r" => convert_run(node, out, report),
        "f" => out.push(convert_fraction(node, report, next_depth)),
        "rad" => out.push(convert_radical(node, report, next_depth)),
        "sSup" => out.push(convert_ssup(node, report, next_depth)),
        "sSub" => out.push(convert_ssub(node, report, next_depth)),
        "sSubSup" => out.push(convert_ssubsup(node, report, next_depth)),
        "sPre" => out.push(convert_spre(node, report, next_depth)),
        "nary" => out.push(convert_nary(node, report, next_depth)),
        "d" => out.push(convert_delim(node, report, next_depth)),
        "m" => out.push(convert_matrix(node, report, next_depth)),
        "eqArr" => out.push(convert_eqarr(node, report, next_depth)),
        "acc" => out.push(convert_accent(node, report, next_depth)),
        "bar" => out.push(convert_bar(node, report, next_depth)),
        "groupChr" => out.push(convert_groupchr(node, report, next_depth)),
        // Word has no in-math box primitive Typst can represent, and the
        // content is what matters — unwrap silently (no report: this is a
        // deliberate simplification, not a loss anyone needs to audit).
        "box" | "borderBox" => out.push(convert_row(child(node, "e").unwrap_or(node), report, next_depth)),
        "func" => out.push(convert_func(node, report, next_depth)),
        "limLow" => out.push(convert_limlow(node, report, next_depth)),
        "limUpp" => out.push(convert_limupp(node, report, next_depth)),
        "phant" => out.push(convert_phant(node, report, next_depth)),
        // Transparent schema containers: no rendering of their own, so
        // recurse without treating this as an unmapped construct. `oMath`
        // reaches here for the (practically unreachable, since
        // `wml::parse` already splits `m:oMathPara` into one fragment per
        // `m:oMath` before this module ever sees it — see
        // `wml::parse::fold_run_items`) case of a raw `m:oMathPara` fragment:
        // each `m:oMath` child converts in document order, exactly the
        // "equations it contains, in order" the mapping calls for.
        "oMath" | "oMathPara" | "e" | "num" | "den" | "sub" | "sup" | "lim" | "deg" => {
            for c in node.children() {
                convert_element(c, out, report, next_depth);
            }
        }
        // Property blocks (`m:fPr`, `m:naryPr`, …) carry no rendered content
        // of their own; each construct above reads the specific properties
        // it needs directly rather than visiting this generically.
        name if name.ends_with("Pr") => {}
        // Anything else: an OMML element this mapper doesn't specifically
        // recognise (a schema extension, a future version, an editor's
        // proprietary addition). Never drop it and never splice its raw XML
        // into the output — recurse into its children so any text inside
        // still surfaces, and record that this subtree was approximated.
        name => {
            report.approximate(
                "OMML equation",
                eco_format!("unrecognised element <m:{name}>; used its content as-is"),
            );
            for c in node.children() {
                convert_element(c, out, report, next_depth);
            }
        }
    }
}

/// Converts the children of `node` (an `m:e`/`m:num`/`m:oMath`/…) to a list
/// of atoms, in document order.
fn convert_children(node: Node, report: &mut ImportReport, depth: usize) -> Vec<EcoString> {
    let mut out = Vec::new();
    for c in node.children() {
        convert_element(c, &mut out, report, depth);
    }
    out
}

/// Converts the children of `node` to a single joined Typst-source string —
/// what goes between `$…$`, or into one argument slot of a constructor like
/// `frac(..)`.
fn convert_row(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    join_atoms(&convert_children(node, report, depth))
}

/// Space-separates atoms. Always safe: Typst's math layout derives spacing
/// from each atom's class, not from source whitespace, so this can never
/// make correct input render differently — and it's what turns a run of
/// separate letter-atoms into the *legal* `a b c` instead of the *illegal*
/// `abc` (hazard 1).
fn join_atoms(atoms: &[EcoString]) -> EcoString {
    let mut out = String::new();
    for a in atoms {
        if a.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(a);
    }
    EcoString::from(out)
}

// ===========================================================================
// Runs and text (hazards 1 and 2).
// ===========================================================================

fn convert_run(node: Node, out: &mut Vec<EcoString>, report: &mut ImportReport) {
    let nor = child(node, "rPr").is_some_and(|pr| child(pr, "nor").is_some());
    let text = collect_text(node);
    if text.is_empty() {
        return;
    }
    tokenize(&text, nor, out, report);
}

/// The concatenated text of all `m:t` children of a run (Word can, in
/// principle, split a run's text across more than one `m:t`).
fn collect_text(node: Node) -> String {
    let mut s = String::new();
    for t in node.children().filter(|n| n.is_element() && local(*n) == "t") {
        if let Some(text) = t.text() {
            s.push_str(text);
        }
    }
    s
}

/// Splits a run's text into Typst atoms, applying both hazards character by
/// character (a run can mix folded math-alphanumerics, plain ASCII, digits,
/// and symbols — e.g. `m:nary`'s bound text `i=1` is one run).
fn tokenize(text: &str, nor: bool, out: &mut Vec<EcoString>, report: &mut ImportReport) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            // Inter-atom spacing is handled structurally by `join_atoms`; an
            // explicit space run must not add a *second* space on top of it.
            i += 1;
            continue;
        }

        // Invisible OOXML-only operators (invisible times/function
        // application) and math-style variation selectors (Word appends
        // U+FE00 after some `cal`/script letters to pick a glyph variant):
        // no visual meaning of their own, so drop them rather than surface
        // mystery codepoints.
        if matches!(c, '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}')
            || ('\u{FE00}'..='\u{FE0F}').contains(&c)
        {
            i += 1;
            continue;
        }

        // A superscript/subscript digit *in plain text* (not a structural
        // `m:sSup`/`m:sSub`): recover real script structure rather than
        // emitting the literal small glyph, which Typst would lay out as
        // ordinary (non-raised) text.
        if let Some(d) = superscript_digit(c) {
            let mut digits = String::from(d);
            i += 1;
            while i < chars.len() {
                let Some(d) = superscript_digit(chars[i]) else { break };
                digits.push(d);
                i += 1;
            }
            report.approximate(
                "OMML equation",
                "superscript character in text recovered as `^`; original glyph shape not preserved",
            );
            attach_script(out, '^', &digits);
            continue;
        }
        if let Some(d) = subscript_digit(c) {
            let mut digits = String::from(d);
            i += 1;
            while i < chars.len() {
                let Some(d) = subscript_digit(chars[i]) else { break };
                digits.push(d);
                i += 1;
            }
            report.approximate(
                "OMML equation",
                "subscript character in text recovered as `_`; original glyph shape not preserved",
            );
            attach_script(out, '_', &digits);
            continue;
        }

        // A math-alphanumeric letter (or one of its legacy single-codepoint
        // substitutes): always a *variable*, regardless of `m:nor` — see the
        // module doc's hazard 2. Each one is its own atom (hazard 1).
        if let Some((base, exact)) = fold_math_alphanumeric(c) {
            if !exact {
                report.approximate(
                    "OMML equation",
                    "non-italic math letter style (bold/script/fraktur/double-struck/sans/mono) \
                     not preserved; folded to a plain letter",
                );
            }
            out.push(EcoString::from(base));
            i += 1;
            continue;
        }

        if c == '\u{2212}' {
            // Typographic minus -> the ASCII operator Typst source normally
            // uses (an author would rarely type U+2212 directly).
            out.push(EcoString::from("-"));
            i += 1;
            continue;
        }

        if c.is_ascii_alphabetic() {
            let start = i;
            if nor {
                // Upright text (e.g. an `OpElem` string like `"some text"`)
                // can carry embedded spaces that must stay part of the same
                // quoted phrase, not scatter into separate hazard-1 atoms —
                // so an upright run absorbs interior spaces too, trimmed off
                // the end below.
                while i < chars.len() && (chars[i].is_ascii_alphabetic() || chars[i] == ' ') {
                    i += 1;
                }
            } else {
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
            }
            let word: String = chars[start..i].iter().collect::<String>().trim_end().to_string();
            push_word(&word, nor, out, report);
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            out.push(EcoString::from(&chars[start..i].iter().collect::<String>()[..]));
            continue;
        }

        // A character that is itself Typst math *syntax*, not just a glyph —
        // see `math_syntax_symbol`'s doc comment. Swap in its symbol name
        // instead of the bare character, so the glyph survives without
        // triggering the syntax reading.
        if let Some(name) = math_syntax_symbol(c) {
            out.push(EcoString::from(name));
            i += 1;
            continue;
        }

        // Anything else — operators, punctuation, other Unicode symbols
        // (`⊂`, `∪`, `→`, `×`, …): Typst accepts a bare Unicode symbol
        // directly in math source (typing `∪` is equivalent to typing
        // `union`), so passing it through verbatim is both safe and, unlike
        // a name lookup, never wrong.
        out.push(EcoString::from(c));
        i += 1;
    }
}

/// Maps a character that is itself Typst math *syntax* — not just a glyph —
/// to its symbol name, so `tokenize` (and [`escape_delim_glyph`], for the
/// same character appearing as an `m:d` fence/separator instead of plain
/// text) can emit the glyph without also triggering the syntax reading.
/// Word's own OMML text can contain any of these as ordinary prose:
///
/// - `(`/`)` always open/close a group in math — even bare, unpaired, plain
///   *text* ones (real corpus example: an `m:rad` whose radicand is the
///   literal text `)2(`, from a document that types visual grouping as
///   plain characters rather than real OMML delimiters). Left as bare
///   characters, they don't just risk misparsing *this* construct — an
///   unbalanced one silently shifts where every enclosing call's own
///   parentheses are read as closing;
/// - `[`/`]` open/close an *array* in math (as in `mat`'s row syntax), so a
///   bare one parses as array syntax rather than content —
///   `expected content, found array` (real corpus example: a French
///   half-open interval `[0 ; +∞[`, both as plain text *and*, via
///   [`escape_delim_glyph`], as an `m:d` fence character);
/// - `;` is the 2-D row separator inside `mat(a, b; c, d)`-style argument
///   lists, with the same failure mode;
/// - `#` introduces a code expression (`#`-prefixed, exactly like markup),
///   so a bare one tries to parse whatever follows it as code;
/// - `$` would end the enclosing equation outright, same as it does in
///   `$...$` markup.
///
/// Every mapping here is verified against `codex` (the crate backing
/// Typst's own `sym.*` table): `paren.l`/`paren.r` are `(`/`)`,
/// `bracket.l`/`bracket.r` are `[`/`]`, `semi` is `;`, `hash` is `#`,
/// `dollar` is `$` — each an exact glyph match, so nothing about how the
/// character *looks* changes, only how it parses. Other characters that are
/// ordinary punctuation at the top level of a math body (`,`, `_`/`^`
/// outside a construct this mapper already builds structurally, …) aren't
/// included: they either carry no special meaning there or are never
/// produced as bare text by this tokenizer to begin with.
fn math_syntax_symbol(c: char) -> Option<&'static str> {
    Some(match c {
        '(' => "paren.l",
        ')' => "paren.r",
        '[' => "bracket.l",
        ']' => "bracket.r",
        ';' => "semi",
        '#' => "hash",
        '$' => "dollar",
        _ => return None,
    })
}

/// Applies [`math_syntax_symbol`] to a delimiter/separator character read
/// from OMML data (`m:begChr`/`m:endChr`/`m:sepChr`'s `m:val`) rather than
/// plain run text — the same hazard, reached through [`convert_delim`]
/// instead of [`tokenize`], since Word lets an author put *any* character
/// there, not just the conventional fence glyphs (real corpus example: a
/// French half-open interval's `m:begChr`/`m:endChr` of `[`).
///
/// `(`/`)` are deliberately left bare even though [`math_syntax_symbol`]
/// maps them: used as the *outermost* fence around `lr(..)`'s own content,
/// a literal paren is already balanced by `lr(..)`'s own call parens and
/// can't misparse the way an unpaired one embedded in running *text* can
/// (see that function's doc comment for the case that does need escaping)
/// — so `lr(( x ))`, the overwhelmingly common shape, stays exactly that
/// rather than the uglier-but-equivalent `lr(paren.l x paren.r)`. Every
/// other syntax-significant character (`[`/`]`/`;`/`#`/`$`) still gets
/// escaped in *any* position: those parse as syntax by their mere presence,
/// not by being unbalanced, so being "the fence" doesn't make them safe.
///
/// `s` is expected to be a single character (OMML's own convention for
/// these attributes); anything else — empty, or, rarely, a producer-specific
/// multi-character value — passes through unchanged.
fn escape_delim_glyph(s: &str) -> EcoString {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some('(' | ')'), None) => EcoString::from(s),
        (Some(c), None) => match math_syntax_symbol(c) {
            Some(name) => EcoString::from(name),
            None => EcoString::from(c),
        },
        _ => EcoString::from(s),
    }
}

/// Glues a recovered `^digits`/`_digits` onto the previous atom (a script
/// must immediately follow its base with no space) — or, if there is no
/// preceding atom to attach to, falls back to the bare digits so the content
/// is not lost even though the raised structure could not be recovered.
fn attach_script(out: &mut [EcoString], marker: char, digits: &str) {
    let body = if digits.chars().count() == 1 {
        digits.to_string()
    } else {
        format!("({digits})")
    };
    if let Some(prev) = out.last_mut() {
        *prev = eco_format!("{prev}{marker}{body}");
    }
}

/// Pushes a run of plain ASCII letters as one or more atoms.
fn push_word(word: &str, nor: bool, out: &mut Vec<EcoString>, report: &mut ImportReport) {
    if word.chars().count() == 1 {
        out.push(EcoString::from(word));
        return;
    }
    if KNOWN_OPERATORS.contains(&word) {
        out.push(EcoString::from(word));
        return;
    }
    if nor {
        out.push(quote(word));
        return;
    }
    // No `m:nor`, and not a recognised operator name: Typst forbids a bare
    // multi-letter run (`$ abc $` is `error: unknown variable: abc`, not
    // three variables) — the exact failure this mapper exists to avoid. The
    // safe, meaning-preserving reading here is a run of adjacent italic
    // single-letter variables (what Word produces when a real equation
    // editor merges consecutive default-italic letters into one run), so
    // split it the same way a user would type it themselves.
    report.approximate(
        "OMML equation",
        "adjacent letters with no upright marker space-separated as individual italic variables",
    );
    for ch in word.chars() {
        out.push(EcoString::from(ch));
    }
}

/// Typst operators/functions recognised as bare identifiers in math mode
/// (mirrors the `ops!` table in `typst-library`'s `math::op` exactly — using
/// anything outside this list bare would be its own multi-letter hazard).
const KNOWN_OPERATORS: &[&str] = &[
    "arccos", "arcsin", "arctan", "arg", "cos", "cosh", "cot", "coth", "csc", "csch", "ctg",
    "deg", "det", "dim", "exp", "gcd", "lcm", "hom", "id", "im", "inf", "ker", "lg", "lim", "ln",
    "log", "max", "min", "mod", "Pr", "sec", "sech", "sin", "sinc", "sinh", "sup", "tan", "tanh",
    "tg", "tr",
];

/// Superscript digit `⁰`–`⁹` -> its ASCII digit.
fn superscript_digit(c: char) -> Option<char> {
    Some(match c {
        '⁰' => '0',
        '¹' => '1',
        '²' => '2',
        '³' => '3',
        '⁴' => '4',
        '⁵' => '5',
        '⁶' => '6',
        '⁷' => '7',
        '⁸' => '8',
        '⁹' => '9',
        _ => return None,
    })
}

/// Subscript digit `₀`–`₉` -> its ASCII digit.
fn subscript_digit(c: char) -> Option<char> {
    Some(match c {
        '₀' => '0',
        '₁' => '1',
        '₂' => '2',
        '₃' => '3',
        '₄' => '4',
        '₅' => '5',
        '₆' => '6',
        '₇' => '7',
        '₈' => '8',
        '₉' => '9',
        _ => return None,
    })
}

// ===========================================================================
// Mathematical-alphanumeric folding (hazard 2).
// ===========================================================================

/// Folds a single Unicode Mathematical Alphanumeric Symbols codepoint (or one
/// of its legacy Letterlike Symbols substitutes — the block has several
/// "holes" plugged by pre-existing single codepoints like `ℝ`/`ℎ`) back to a
/// plain base character: an ASCII Latin letter, a Greek letter/operator
/// glyph, or a digit.
///
/// Returns `(base, exact)`. `exact` is true only when Typst's own *default*
/// math rendering already looks like the source glyph — bare Latin letters
/// and Greek lowercase are italic by default, so folding an italic Plane-1
/// letter back to its base is a lossless round-trip. Every other encoded
/// style (bold, script, fraktur, double-struck, sans-serif, monospace, or a
/// styled digit — Typst never uses Plane-1 for a *plain* digit, so any
/// digit-block hit is itself a styled digit) folds to the same base letter
/// but loses that styling, which the caller reports once.
///
/// The block/style layout (which style is "the default look", and where
/// each of Latin's 13 and Greek's 5 style blocks starts) was verified
/// empirically against this repo's own exporter rather than assumed from the
/// Unicode standard: e.g. `bold(x)` in Typst emits U+1D499 (*bold italic*
/// small x, not plain bold), confirming that a style wrapper always adds
/// onto Typst's italic default rather than replacing it, and `sans(A)`
/// emits U+1D608 (sans-serif *italic*, not plain sans-serif) for the same
/// reason.
fn fold_math_alphanumeric(c: char) -> Option<(char, bool)> {
    if let Some(hit) = fold_legacy_letterlike(c) {
        return Some(hit);
    }
    let cp = c as u32;

    // Latin letters: 13 styles (bold, italic, bold italic, script, bold
    // script, fraktur, double-struck, bold fraktur, sans-serif, sans-serif
    // bold, sans-serif italic, sans-serif bold italic, monospace) of 52
    // codepoints each (26 upper, 26 lower). Style index 1 = italic, the
    // default look for a bare Typst variable.
    const LATIN_BASE: u32 = 0x1D400;
    const LATIN_END: u32 = 0x1D6A4; // exclusive
    if (LATIN_BASE..LATIN_END).contains(&cp) {
        let offset = cp - LATIN_BASE;
        let style = offset / 52;
        let within = offset % 52;
        let (index, upper) = if within < 26 { (within, true) } else { (within - 26, false) };
        let base = (if upper { b'A' } else { b'a' }) + index as u8;
        return Some((base as char, style == 1));
    }
    // Dotless italic i/j (used as an accent base, e.g. `hat(dotless.i)`):
    // the default italic look, same as any other bare italic letter.
    match cp {
        0x1D6A4 => return Some(('i', true)),
        0x1D6A5 => return Some(('j', true)),
        _ => {}
    }

    // Greek letters plus nabla/partial and six alternate letterforms
    // (epsilon/theta/kappa/phi/rho/pi "symbol" variants): 5 styles (bold,
    // italic, bold italic, sans-serif bold, sans-serif bold italic) of 58
    // codepoints each. Style index 1 = italic, the default for Greek
    // *lowercase* (Greek uppercase is deliberately never Plane-1 — see
    // `emit_glyph`'s doc comment in the exporter — so it never reaches this
    // function at all).
    const GREEK_BASE: u32 = 0x1D6A8;
    const GREEK_END: u32 = GREEK_BASE + 5 * 58; // exclusive
    if (GREEK_BASE..GREEK_END).contains(&cp) {
        let offset = cp - GREEK_BASE;
        let style = offset / 58;
        let within = offset % 58;
        let (base, slot_exact) = greek_slot(within);
        return Some((base, slot_exact && style == 1));
    }

    // Styled digits: 5 styles (bold, double-struck, sans-serif, sans-serif
    // bold, monospace) of 10 digits each. Typst never emits a *plain*
    // Plane-1 digit (its own exporter writes bare ASCII digits — see the
    // `m:t` text in any `m:sup`/`m:sub`/`m:num` in the module tests), so
    // reaching this arm always means an explicitly styled digit (`bold(5)`);
    // any hit here loses that styling.
    const DIGIT_BASE: u32 = 0x1D7CE;
    const DIGIT_END: u32 = DIGIT_BASE + 50; // exclusive
    if (DIGIT_BASE..DIGIT_END).contains(&cp) {
        let digit = (cp - DIGIT_BASE) % 10;
        return Some(((b'0' + digit as u8) as char, false));
    }

    None
}

/// The base character at index `within` (0..58) of one Greek Math
/// Alphanumeric style block: 25 capital slots (Alpha..Rho, a gap at index 17
/// for the capital "theta symbol" glyph variant, Sigma..Omega), nabla, 25
/// lowercase slots (alpha..rho, a gap at index 17 for final-form sigma ς,
/// sigma..omega), partial-differential, then six "variant" letterforms
/// (epsilon/theta/kappa/phi/rho/pi symbol variants). Returns whether this
/// slot is an exact glyph match for its base letter — the two gap slots and
/// the six variants are themselves a distinct (rarer) glyph collapsed onto
/// the plain letter, so they're never "exact" even in the italic style.
fn greek_slot(within: u32) -> (char, bool) {
    const UPPER: [char; 17] =
        ['Α', 'Β', 'Γ', 'Δ', 'Ε', 'Ζ', 'Η', 'Θ', 'Ι', 'Κ', 'Λ', 'Μ', 'Ν', 'Ξ', 'Ο', 'Π', 'Ρ'];
    const UPPER2: [char; 7] = ['Σ', 'Τ', 'Υ', 'Φ', 'Χ', 'Ψ', 'Ω'];
    const LOWER: [char; 17] =
        ['α', 'β', 'γ', 'δ', 'ε', 'ζ', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ'];
    const LOWER2: [char; 7] = ['σ', 'τ', 'υ', 'φ', 'χ', 'ψ', 'ω'];
    const VARIANTS: [char; 6] = ['ε', 'θ', 'κ', 'φ', 'ρ', 'π'];
    match within {
        0..=16 => (UPPER[within as usize], true),
        17 => ('Θ', false),
        18..=24 => (UPPER2[(within - 18) as usize], true),
        25 => ('∇', true),
        26..=42 => (LOWER[(within - 26) as usize], true),
        43 => ('σ', false),
        44..=50 => (LOWER2[(within - 44) as usize], true),
        51 => ('∂', true),
        _ => (VARIANTS[(within - 52) as usize], false),
    }
}

/// Legacy Letterlike Symbols codepoints Unicode reuses instead of allocating
/// a new Plane-1 slot, for a handful of letters that already had a
/// widely-used single-codepoint form: italic *h* (the Planck constant
/// symbol, ℎ — verified empirically: Typst emits U+210E for a bare italic
/// `h`, never a Plane-1 codepoint), and the several script/fraktur/
/// double-struck capitals and lowercase that predate Unicode's math-alphanumeric
/// block (ℬ, ℭ, ℝ, …).
fn fold_legacy_letterlike(c: char) -> Option<(char, bool)> {
    Some(match c {
        '\u{210E}' => ('h', true), // PLANCK CONSTANT = the default italic h
        '\u{212C}' => ('B', false), // SCRIPT CAPITAL B
        '\u{2130}' => ('E', false), // SCRIPT CAPITAL E
        '\u{2131}' => ('F', false), // SCRIPT CAPITAL F
        '\u{210B}' => ('H', false), // SCRIPT CAPITAL H
        '\u{2110}' => ('I', false), // SCRIPT CAPITAL I
        '\u{2112}' => ('L', false), // SCRIPT CAPITAL L
        '\u{2133}' => ('M', false), // SCRIPT CAPITAL M
        '\u{211B}' => ('R', false), // SCRIPT CAPITAL R
        '\u{212F}' => ('e', false), // SCRIPT SMALL E
        '\u{210A}' => ('g', false), // SCRIPT SMALL G
        '\u{2134}' => ('o', false), // SCRIPT SMALL O
        '\u{212D}' => ('C', false), // BLACK-LETTER CAPITAL C (fraktur)
        '\u{210C}' => ('H', false), // BLACK-LETTER CAPITAL H
        '\u{2111}' => ('I', false), // BLACK-LETTER CAPITAL I
        '\u{211C}' => ('R', false), // BLACK-LETTER CAPITAL R
        '\u{2128}' => ('Z', false), // BLACK-LETTER CAPITAL Z
        '\u{2102}' => ('C', false), // DOUBLE-STRUCK CAPITAL C
        '\u{210D}' => ('H', false), // DOUBLE-STRUCK CAPITAL H
        '\u{2115}' => ('N', false), // DOUBLE-STRUCK CAPITAL N
        '\u{2119}' => ('P', false), // DOUBLE-STRUCK CAPITAL P
        '\u{211A}' => ('Q', false), // DOUBLE-STRUCK CAPITAL Q
        '\u{211D}' => ('R', false), // DOUBLE-STRUCK CAPITAL R
        '\u{2124}' => ('Z', false), // DOUBLE-STRUCK CAPITAL Z
        _ => return None,
    })
}

// ===========================================================================
// Structural constructs.
// ===========================================================================

/// The zero-width space (Typst's `zws` symbol, U+200B) substituted for an
/// operand that converts to nothing at all — an empty (or entirely
/// unconvertible) `m:e`/`m:num`/`m:den`/`m:sub`/`m:sup`/…
///
/// No maths construct below may emit a call with a missing *required*
/// argument: `sqrt()` is `missing argument: radicand`, not an empty radical,
/// and the same is true of `frac(, y)`/`attach(x, bl: ())`/`hat()`/etc. —
/// every construct here shares the exposure (`frac`, `root`, `attach`,
/// accents, and the rest all read an operand the same way), so this is
/// handled once, centrally, wherever a converted operand is about to be
/// embedded — rather than each construct inventing its own empty-argument
/// guard. Verified: `$sqrt(zws)$` compiles.
fn operand(converted: &str) -> &str {
    let trimmed = converted.trim();
    if trimmed.is_empty() { "zws" } else { trimmed }
}

/// `m:f` — fraction: `frac(num, den)`, or `binom(num, den)` when the bar is
/// hidden (`m:fPr/m:type val="noBar"`), or `num \/ den` when linear/skewed.
fn convert_fraction(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let num = convert_row(child(node, "num").unwrap_or(node), report, depth);
    let den = convert_row(child(node, "den").unwrap_or(node), report, depth);
    match frac_type(node) {
        FracType::NoBar => eco_format!("binom({}, {})", operand(&num), operand(&den)),
        FracType::Linear => eco_format!("{} \\/ {}", operand(&num), operand(&den)),
        FracType::Bar => eco_format!("frac({}, {})", operand(&num), operand(&den)),
    }
}

enum FracType {
    Bar,
    NoBar,
    Linear,
}

fn frac_type(node: Node) -> FracType {
    let Some(pr) = child(node, "fPr") else { return FracType::Bar };
    let Some(ty) = child(pr, "type") else { return FracType::Bar };
    match mval(ty) {
        Some("noBar") => FracType::NoBar,
        // A skewed fraction has no direct Typst equivalent; a linear
        // `num/den` reads the same shape as skewed and is unambiguous.
        Some("lin") | Some("skw") => FracType::Linear,
        _ => FracType::Bar,
    }
}

/// `m:rad` — radical: `sqrt(e)` when the degree is hidden or empty (the
/// square-root case), else `root(deg, e)`. The degree comes *before* the
/// radicand in OMML (the reverse of `root`'s own argument order, which the
/// exporter's doc comment calls out too), so this reads `m:deg` explicitly
/// rather than assuming argument order.
fn convert_radical(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let radicand = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let deg_hidden = child(node, "radPr").and_then(|pr| child(pr, "degHide")).is_some_and(is_on);
    let deg = child(node, "deg").map(|d| convert_row(d, report, depth)).unwrap_or_default();
    if deg_hidden || deg.trim().is_empty() {
        eco_format!("sqrt({})", operand(&radicand))
    } else {
        eco_format!("root({}, {})", operand(&deg), operand(&radicand))
    }
}

/// Parenthesise an attachment operand unless it demonstrably can't mis-bind.
///
/// This module's one hard requirement is never to emit source that might
/// parse differently than intended, so the default is parentheses. But an
/// operand that is already a single indivisible unit binds the same either
/// way, and `x^2` reads far better than `(x)^(2)` in a document someone has
/// to maintain. Only three shapes qualify, all of them unambiguous:
/// a single character, a run of digits, and a whole-string function call
/// like `sqrt(y)` or `lr(( a + b ))` whose parentheses already close at the
/// very end. Anything else — anything containing a space or an operator at
/// the top level — keeps its parentheses.
///
/// An empty operand goes through [`operand`] first, same as every other
/// construct's — an attachment is just as much a "call with a missing
/// argument" as `sqrt()` is (`e^()`/`e_()` is nothing to attach), it's just
/// spelled with Typst's postfix syntax instead of a named function.
fn attach_operand(s: &str) -> EcoString {
    let s = operand(s);
    if is_atomic(s) { s.into() } else { eco_format!("({s})") }
}

fn is_atomic(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.chars().count() == 1 {
        return true;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }

    // A single call: an identifier, then a parenthesised group that closes
    // exactly at the end of the string. `sqrt(y)` qualifies; `sqrt(y) + 1`
    // does not, because its first group closes early.
    let Some(open) = s.find('(') else { return false };
    let (name, rest) = s.split_at(open);
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '.') {
        return false;
    }
    let mut depth = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i + c.len_utf8() == rest.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// `m:sSup` — superscript: `e^(sup)`, parenthesising either side only where
/// it could otherwise mis-bind (see [`attach_operand`]).
fn convert_ssup(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let sup = convert_row(child(node, "sup").unwrap_or(node), report, depth);
    eco_format!("{}^{}", attach_operand(&e), attach_operand(&sup))
}

/// `m:sSub` — subscript: `e_(sub)`.
fn convert_ssub(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let sub = convert_row(child(node, "sub").unwrap_or(node), report, depth);
    eco_format!("{}_{}", attach_operand(&e), attach_operand(&sub))
}

/// `m:sSubSup` — combined sub/superscript: `e_(sub)^(sup)`.
fn convert_ssubsup(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let sub = convert_row(child(node, "sub").unwrap_or(node), report, depth);
    let sup = convert_row(child(node, "sup").unwrap_or(node), report, depth);
    eco_format!(
        "{}_{}^{}",
        attach_operand(&e),
        attach_operand(&sub),
        attach_operand(&sup)
    )
}

/// `m:sPre` — pre-scripts (e.g. isotope notation `""^235_92 U`): `attach(e,
/// bl: sub, tl: sup)`. A side missing its XML element entirely is omitted
/// from the call rather than passed through as an empty `bl: ()`; a side
/// that *is* present but converts to nothing gets [`operand`]'s `zws`
/// placeholder instead, the same as every other construct's required
/// argument.
fn convert_spre(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let mut args = operand(&e).to_string();
    if let Some(sub) = child(node, "sub") {
        let sub = convert_row(sub, report, depth);
        args.push_str(&format!(", bl: ({})", operand(&sub)));
    }
    if let Some(sup) = child(node, "sup") {
        let sup = convert_row(sup, report, depth);
        args.push_str(&format!(", tl: ({})", operand(&sup)));
    }
    eco_format!("attach({args})")
}

/// `m:nary` — n-ary operator (∑ ∫ ∏ …): `<op>_(sub)^(sup) e`, honouring
/// `m:subHide`/`m:supHide` (and treating genuinely empty content as hidden
/// too, for real-world files that leave the flag off but the element empty).
fn convert_nary(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let pr = child(node, "naryPr");
    let chr = pr.and_then(|pr| child(pr, "chr")).and_then(mval).and_then(|s| s.chars().next());
    let op: EcoString = match chr {
        None | Some('∫') => "integral".into(),
        Some('∑') => "sum".into(),
        Some('∏') => "product".into(),
        Some('∐') => "product.co".into(),
        Some('∬') => "integral.double".into(),
        Some('∭') => "integral.triple".into(),
        Some('∮') => "integral.cont".into(),
        Some('⋃') => "union.big".into(),
        Some('⋂') => "sect.big".into(),
        Some('⋁') => "or.big".into(),
        Some('⋀') => "and.big".into(),
        Some(other) => {
            report.approximate(
                "OMML equation",
                "n-ary operator without a named Typst equivalent kept as its literal symbol",
            );
            EcoString::from(other)
        }
    };

    let sub_hidden = pr.and_then(|pr| child(pr, "subHide")).is_some_and(is_on);
    let sup_hidden = pr.and_then(|pr| child(pr, "supHide")).is_some_and(is_on);
    let sub = child(node, "sub").map(|n| convert_row(n, report, depth)).unwrap_or_default();
    let sup = child(node, "sup").map(|n| convert_row(n, report, depth)).unwrap_or_default();
    let e = child(node, "e").map(|n| convert_row(n, report, depth)).unwrap_or_default();

    let mut s = op.to_string();
    if !sub_hidden && !sub.trim().is_empty() {
        s.push_str(&format!("_({})", sub.trim()));
    }
    if !sup_hidden && !sup.trim().is_empty() {
        s.push_str(&format!("^({})", sup.trim()));
    }
    if !e.trim().is_empty() {
        s.push(' ');
        s.push_str(e.trim());
    }
    EcoString::from(s)
}

/// `m:func` — a named function application (`m:fName`, `m:e`): `fName (e)`.
fn convert_func(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let name = child(node, "fName").map(|n| convert_row(n, report, depth)).unwrap_or_default();
    let e = child(node, "e").map(|n| convert_row(n, report, depth)).unwrap_or_default();
    eco_format!("{} ({})", name.trim(), operand(&e))
}

/// `m:limLow` — a limit below a base (`lim_(x -> 0)`): `limits(e)_(lim)`.
fn convert_limlow(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let lim = convert_row(child(node, "lim").unwrap_or(node), report, depth);
    eco_format!("limits({})_({})", operand(&e), operand(&lim))
}

/// `m:limUpp` — a limit above a base: `limits(e)^(lim)`.
fn convert_limupp(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let lim = convert_row(child(node, "lim").unwrap_or(node), report, depth);
    eco_format!("limits({})^({})", operand(&e), operand(&lim))
}

/// `m:d` — delimiters: `lr(<beg> inner <end>)`, several `m:e` cells joined by
/// `m:sepChr` (default `,`). If a side's char is explicitly empty (a
/// one-sided fence), the inner content is emitted *without* `lr(..)` rather
/// than an unbalanced delimiter.
///
/// Special case: Word represents `mat`/`cases` as a delimiter wrapping a
/// single `m:m` matrix (verified against this repo's own exporter: `cases(..)`
/// lowers to a one-sided `{`-fenced single-column table, not `m:eqArr` —
/// `m:eqArr` is what a *real* Word "Cases" equation-gallery insert produces,
/// and what this mapper honours for that OMML shape via [`convert_eqarr`]).
/// Recognising the sole-matrix shape recovers the real constructor instead of
/// stringifying the matrix source inside a redundant `lr(..)`.
fn convert_delim(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let pr = child(node, "dPr");
    // Raw (unescaped) — [`convert_matrix_delimited`] pattern-matches these
    // against literal `"("`/`")"`/`"{"` to recognise `mat`/`cases`, so the
    // syntax-escaping below only happens at the point these are actually
    // embedded as bare math source, just below, never here.
    let beg = delim_char(pr, "begChr", '(');
    let end = delim_char(pr, "endChr", ')');
    let sep_raw = pr
        .and_then(|pr| child(pr, "sepChr"))
        .and_then(mval)
        .filter(|s| !s.is_empty())
        .unwrap_or(",");

    let cells: Vec<Node> = children(node, "e").collect();
    if let [only] = cells.as_slice()
        && let Some(matrix) = sole_child(*only, "m")
    {
        return convert_matrix_delimited(matrix, beg.as_deref(), end.as_deref(), report, depth);
    }

    // Escaped here, at the point of embedding as bare math source — a fence
    // or separator character is exactly as liable to collide with Typst
    // math syntax as any other bare character (see `math_syntax_symbol`'s
    // doc comment; the French-interval corpus example reaches this exact
    // path via an explicit `m:begChr`/`m:endChr` of `[`).
    let sep = escape_delim_glyph(sep_raw);
    let inner_cells: Vec<EcoString> =
        cells.iter().map(|e| convert_row(*e, report, depth)).collect();
    let inner = inner_cells
        .iter()
        .map(|s| operand(s))
        .collect::<Vec<_>>()
        .join(&format!("{sep} "));

    let beg = beg.as_deref().map(escape_delim_glyph);
    let end = end.as_deref().map(escape_delim_glyph);
    match (&beg, &end) {
        (Some(b), Some(e)) => eco_format!("lr({b} {inner} {e})"),
        // One side has no delimiter char at all: an unbalanced `lr(..)` isn't
        // valid, so drop the fence entirely rather than guess a pairing.
        _ => EcoString::from(inner.trim()),
    }
}

/// Reads a delimiter side's char from `dPr/<tag>`: `default` if `dPr` or the
/// child is absent, `None` for an explicitly empty value (a one-sided
/// fence), or the given value.
fn delim_char(pr: Option<Node>, tag: &str, default: char) -> Option<EcoString> {
    let Some(pr) = pr else { return Some(EcoString::from(default)) };
    let Some(el) = child(pr, tag) else { return Some(EcoString::from(default)) };
    match mval(el) {
        Some("") => None,
        Some(s) => Some(EcoString::from(s)),
        None => Some(EcoString::from(default)),
    }
}

/// The delimiter-wrapped-matrix special case: `mat(..)` for the default `( )`
/// pair, `cases(..)` for a one-sided `{`, `mat(delim: "x", ..)` for any other
/// pairing.
fn convert_matrix_delimited(
    matrix: Node,
    beg: Option<&str>,
    end: Option<&str>,
    report: &mut ImportReport,
    depth: usize,
) -> EcoString {
    let rows = matrix_rows(matrix, report, depth);
    match (beg, end) {
        (Some("{"), None) => {
            let cases: Vec<String> = rows.iter().map(|r| r.join(", ")).collect();
            eco_format!("cases({})", cases.join(", "))
        }
        (Some("("), Some(")")) => {
            eco_format!("mat({})", rows.iter().map(|r| r.join(", ")).collect::<Vec<_>>().join("; "))
        }
        (b, _) => {
            let delim = b.unwrap_or("(");
            eco_format!(
                "mat(delim: \"{}\", {})",
                escape_str(delim),
                rows.iter().map(|r| r.join(", ")).collect::<Vec<_>>().join("; ")
            )
        }
    }
}

/// `m:m` — matrix: `mat(a, b; c, d)`.
fn convert_matrix(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let rows = matrix_rows(node, report, depth);
    eco_format!("mat({})", rows.iter().map(|r| r.join(", ")).collect::<Vec<_>>().join("; "))
}

/// Each `m:mr` row's `m:e` cells, converted and trimmed. An empty cell gets
/// [`operand`]'s `zws` placeholder — `mat(1, , 3)`'s middle slot is a missing
/// positional argument to `mat(..)`, the exact shape [`operand`] guards
/// against everywhere else.
fn matrix_rows(node: Node, report: &mut ImportReport, depth: usize) -> Vec<Vec<EcoString>> {
    children(node, "mr")
        .map(|row| {
            children(row, "e")
                .map(|e| EcoString::from(operand(&convert_row(e, report, depth))))
                .collect()
        })
        .collect()
}

/// `m:eqArr` — an equation array (Word's own "Cases" gallery construct, and a
/// plain multi-line/gather body without alignment): `cases(row1, row2)`.
fn convert_eqarr(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let rows: Vec<EcoString> = children(node, "e")
        .map(|e| EcoString::from(operand(&convert_row(e, report, depth))))
        .collect();
    eco_format!("cases({})", rows.join(", "))
}

/// `m:acc` — accent: `hat(e)`, `macron(e)`, `arrow(e)`, `dot(e)`,
/// `dot.double(e)`, `tilde(e)`, `breve(e)`, `caron(e)`, `acute(e)`,
/// `grave(e)`, or `circle(e)`, by `m:chr` (default, and fallback for an
/// unrecognised mark, is `hat` — OMML's own default when `m:chr` is absent).
fn convert_accent(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let chr = child(node, "accPr").and_then(|pr| child(pr, "chr")).and_then(mval).and_then(|s| s.chars().next());
    let func = match chr {
        None => "hat",
        Some('\u{0302}' | '^') => "hat",
        Some('\u{0304}' | '\u{00AF}') => "macron",
        Some('\u{20D7}') => "arrow",
        Some('\u{0307}') => "dot",
        Some('\u{0308}') => "dot.double",
        Some('\u{0303}' | '~') => "tilde",
        Some('\u{0306}') => "breve",
        Some('\u{030C}') => "caron",
        Some('\u{0301}') => "acute",
        Some('\u{0300}') => "grave",
        Some('\u{030A}') => "circle",
        Some(_) => {
            report.approximate(
                "OMML equation",
                "accent mark without a named Typst equivalent defaulted to `hat`",
            );
            "hat"
        }
    };
    eco_format!("{func}({})", operand(&e))
}

/// `m:bar` — over/under bar: `overline(e)` (`m:barPr/m:pos val="top"`) or
/// `underline(e)` (`"bot"`, the default).
fn convert_bar(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let pos = child(node, "barPr").and_then(|pr| child(pr, "pos")).and_then(mval);
    let func = if pos == Some("top") { "overline" } else { "underline" };
    eco_format!("{func}({})", operand(&e))
}

/// `m:groupChr` — a stretched grouping character: `overbrace(e)` (`⏞`, or
/// `m:pos val="top"`), `underbrace(e)` (`⏟`, or `"bot"`); any other grouping
/// char has no Typst brace/bracket equivalent, so the base content is kept
/// unwrapped rather than guessing.
fn convert_groupchr(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    let e = convert_row(child(node, "e").unwrap_or(node), report, depth);
    let pr = child(node, "groupChrPr");
    let chr = pr.and_then(|pr| child(pr, "chr")).and_then(mval).and_then(|s| s.chars().next());
    let pos = pr.and_then(|pr| child(pr, "pos")).and_then(mval);
    if chr == Some('⏞') || pos == Some("top") {
        eco_format!("overbrace({})", operand(&e))
    } else if chr == Some('⏟') || pos == Some("bot") {
        eco_format!("underbrace({})", operand(&e))
    } else {
        report.approximate(
            "OMML equation",
            "grouping character (m:groupChr) has no Typst brace/bracket equivalent; base content kept",
        );
        EcoString::from(e.trim())
    }
}

/// `m:phant` — a phantom (spacing-only placeholder, invisible in Word). Both
/// showing and dropping its content are "wrong" in some sense; showing it
/// keeps the content (never losing text), so that's the one this mapper
/// picks — and reports, since the result now looks slightly different from
/// the source (visible where Word rendered nothing).
fn convert_phant(node: Node, report: &mut ImportReport, depth: usize) -> EcoString {
    report.approximate(
        "OMML equation",
        "phantom (m:phant) kept as visible content; its spacing-only placeholder role is not simulated",
    );
    convert_row(child(node, "e").unwrap_or(node), report, depth)
}

// ===========================================================================
// Small helpers.
// ===========================================================================

fn local<'a>(node: Node<'a, 'a>) -> &'a str {
    node.tag_name().name()
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.is_element() && local(*n) == name)
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(move |n| n.is_element() && local(*n) == name)
}

/// The first element child of `node` if it is the *sole* element child and
/// has local name `name` — used to detect "a delimiter whose only content is
/// a matrix" ([`convert_delim`]'s sole-matrix special case).
fn sole_child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    let mut elems = node.children().filter(|n| n.is_element());
    let first = elems.next()?;
    if elems.next().is_some() {
        return None;
    }
    (local(first) == name).then_some(first)
}

/// The `m:val` attribute of a property element, by local name (namespace
/// prefixes are a red herring across real-world OOXML producers, same as
/// the rest of this crate — see `wml::parse::attr`'s doc comment).
fn mval<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == "val").map(|a| a.value())
}

/// Whether a boolean toggle property (`m:degHide`, `m:subHide`, …) is "on" —
/// OOXML's convention is that the element's mere *presence* already means on,
/// with `m:val` only present to explicitly say `"0"`/`"false"`/`"off"`.
fn is_on(node: Node) -> bool {
    !matches!(mval(node), Some("0") | Some("false") | Some("off"))
}

/// All `m:t`-descendant text under `node`, ignoring structure entirely — the
/// [`MAX_MATH_DEPTH`] fallback's last resort, and nothing else (every other
/// path preserves structure).
fn plain_text(node: Node) -> String {
    let mut s = String::new();
    for t in node.descendants().filter(|n| n.is_element() && local(*n) == "t") {
        if let Some(text) = t.text() {
            s.push_str(text);
        }
    }
    s
}

/// A Typst string literal for `s`, escaping `"` and `\`.
fn quote(s: &str) -> EcoString {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&escape_str(s));
    out.push('"');
    EcoString::from(out)
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts a bare OMML fragment (without the outer namespace/report
    /// plumbing) and returns the Typst source that would go inside `$…$`.
    fn convert(fragment: &str) -> String {
        let mut report = ImportReport::default();
        match omml_to_inline(fragment, &mut report) {
            Inline::Math(s) => s.to_string(),
            other => panic!("expected Inline::Math, got {other:?}"),
        }
    }

    const NS: &str = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;

    fn o_math(inner: &str) -> String {
        format!(r#"<m:oMath {NS}>{inner}</m:oMath>"#)
    }

    fn run(text: &str) -> String {
        format!(r#"<m:r><m:rPr><m:nor/></m:rPr><m:t>{text}</m:t></m:r>"#)
    }

    fn plain_run(text: &str) -> String {
        format!(r#"<m:r><m:t>{text}</m:t></m:r>"#)
    }

    #[test]
    fn run_folds_italic_math_alphanumeric_letters_and_space_separates_them() {
        // 𝑥 (U+1D465), 𝑦 (U+1D466): two separate runs, as the exporter emits.
        let src = o_math(&format!("{}{}", run("𝑥"), run("𝑦")));
        assert_eq!(convert(&src), "x y");
    }

    #[test]
    fn upright_multi_letter_run_becomes_a_quoted_string() {
        let src = o_math(&run("some text"));
        assert_eq!(convert(&src), "\"some text\"");
    }

    #[test]
    fn known_function_name_stays_bare() {
        let src = o_math(&run("sin"));
        assert_eq!(convert(&src), "sin");
    }

    #[test]
    fn unstyled_multi_letter_run_is_space_separated_not_a_bare_identifier() {
        // No `m:nor`, no math-alphanumeric styling: still must not become
        // the illegal bare `abc`.
        let src = o_math(&plain_run("abc"));
        assert_eq!(convert(&src), "a b c");
    }

    #[test]
    fn ssup() {
        let inner = format!(
            r#"<m:sSup><m:e>{}</m:e><m:sup>{}</m:sup></m:sSup>"#,
            run("𝑥"),
            run("2")
        );
        assert_eq!(convert(&o_math(&inner)), "x^2");
    }

    #[test]
    fn ssub() {
        let inner = format!(
            r#"<m:sSub><m:e>{}</m:e><m:sub>{}</m:sub></m:sSub>"#,
            run("𝑥"),
            run("𝑖")
        );
        assert_eq!(convert(&o_math(&inner)), "x_i");
    }

    /// Attachment operands lose their parentheses only when they cannot
    /// mis-bind. A compound operand keeps them — dropping them there would
    /// silently change what the expression means.
    #[test]
    fn compound_attachment_operands_keep_their_parentheses() {
        let compound = format!("{}{}{}", run("𝑖"), run("+"), run("1"));
        let inner = format!(
            r#"<m:sSup><m:e>{}</m:e><m:sup>{}</m:sup></m:sSup>"#,
            run("𝑥"),
            compound
        );
        assert_eq!(convert(&o_math(&inner)), "x^(i + 1)");
    }

    #[test]
    fn ssubsup() {
        let inner = format!(
            r#"<m:sSubSup><m:e>{}</m:e><m:sub>{}</m:sub><m:sup>{}</m:sup></m:sSubSup>"#,
            run("𝑥"),
            run("𝑖"),
            run("2")
        );
        assert_eq!(convert(&o_math(&inner)), "x_i^2");
    }

    #[test]
    fn spre() {
        let inner = format!(
            r#"<m:sPre><m:sub>{}</m:sub><m:sup>{}</m:sup><m:e>{}</m:e></m:sPre>"#,
            run("92"),
            run("235"),
            run("𝑈")
        );
        assert_eq!(convert(&o_math(&inner)), "attach(U, bl: (92), tl: (235))");
    }

    #[test]
    fn fraction_bar() {
        let inner = format!(
            r#"<m:f><m:num>{}</m:num><m:den>{}</m:den></m:f>"#,
            run("𝑎"),
            run("𝑏")
        );
        assert_eq!(convert(&o_math(&inner)), "frac(a, b)");
    }

    #[test]
    fn fraction_no_bar_is_binom() {
        let inner = format!(
            r#"<m:f><m:fPr><m:type m:val="noBar"/></m:fPr><m:num>{}</m:num><m:den>{}</m:den></m:f>"#,
            run("𝑛"),
            run("𝑘")
        );
        assert_eq!(convert(&o_math(&inner)), "binom(n, k)");
    }

    #[test]
    fn radical_sqrt_when_degree_hidden() {
        let inner = format!(
            r#"<m:rad><m:radPr><m:degHide m:val="on"/></m:radPr><m:deg/><m:e>{}</m:e></m:rad>"#,
            run("𝑦")
        );
        assert_eq!(convert(&o_math(&inner)), "sqrt(y)");
    }

    #[test]
    fn radical_root_with_degree() {
        let inner = format!(
            r#"<m:rad><m:deg>{}</m:deg><m:e>{}</m:e></m:rad>"#,
            run("3"),
            run("𝑦")
        );
        assert_eq!(convert(&o_math(&inner)), "root(3, y)");
    }

    #[test]
    fn nary_sum_with_limits() {
        let inner = format!(
            r#"<m:nary><m:naryPr><m:chr m:val="∑"/><m:limLoc m:val="undOvr"/></m:naryPr>
               <m:sub>{}{}{}</m:sub><m:sup>{}</m:sup><m:e>{}</m:e></m:nary>"#,
            run("𝑖"),
            run("="),
            run("1"),
            run("𝑛"),
            run("𝑖")
        );
        assert_eq!(convert(&o_math(&inner)), "sum_(i = 1)^(n) i");
    }

    #[test]
    fn nary_default_operator_is_integral() {
        let inner = format!(
            r#"<m:nary><m:naryPr/><m:sub><m:subHide/></m:sub><m:sup><m:supHide/></m:sup><m:e>{}</m:e></m:nary>"#,
            run("𝑓")
        );
        assert_eq!(convert(&o_math(&inner)), "integral f");
    }

    #[test]
    fn func() {
        let inner = format!(
            r#"<m:func><m:fName>{}</m:fName><m:e>{}</m:e></m:func>"#,
            run("sin"),
            run("𝑥")
        );
        assert_eq!(convert(&o_math(&inner)), "sin (x)");
    }

    #[test]
    fn lim_low() {
        let inner = format!(
            r#"<m:limLow><m:e>{}</m:e><m:lim>{}</m:lim></m:limLow>"#,
            run("𝐿"),
            run("𝑛")
        );
        assert_eq!(convert(&o_math(&inner)), "limits(L)_(n)");
    }

    #[test]
    fn lim_upp() {
        let inner = format!(
            r#"<m:limUpp><m:e>{}</m:e><m:lim>{}</m:lim></m:limUpp>"#,
            run("𝐿"),
            run("𝑛")
        );
        assert_eq!(convert(&o_math(&inner)), "limits(L)^(n)");
    }

    #[test]
    fn delimiter_parens() {
        let inner = format!(r#"<m:d><m:e>{}</m:e></m:d>"#, run("𝑥"));
        assert_eq!(convert(&o_math(&inner)), "lr(( x ))");
    }

    #[test]
    fn delimiter_one_sided_drops_lr() {
        let inner = format!(
            r#"<m:d><m:dPr><m:begChr m:val="{{"/><m:endChr m:val=""/></m:dPr><m:e>{}</m:e></m:d>"#,
            run("𝑥")
        );
        assert_eq!(convert(&o_math(&inner)), "x");
    }

    #[test]
    fn matrix_wrapped_in_default_parens_becomes_mat() {
        let mr = |a: &str, b: &str| {
            format!(
                r#"<m:mr><m:e>{}</m:e><m:e>{}</m:e></m:mr>"#,
                run(a),
                run(b)
            )
        };
        let m = format!(r#"<m:m>{}{}</m:m>"#, mr("1", "2"), mr("3", "4"));
        let inner = format!(r#"<m:d><m:e>{m}</m:e></m:d>"#);
        assert_eq!(convert(&o_math(&inner)), "mat(1, 2; 3, 4)");
    }

    #[test]
    fn one_sided_brace_over_single_column_matrix_becomes_cases() {
        let mr = |a: &str| format!(r#"<m:mr><m:e>{}</m:e></m:mr>"#, run(a));
        let m = format!(r#"<m:m>{}{}</m:m>"#, mr("x &gt; 0"), mr("y &lt; 1"));
        let inner = format!(
            r#"<m:d><m:dPr><m:begChr m:val="{{"/><m:endChr m:val=""/></m:dPr><m:e>{m}</m:e></m:d>"#
        );
        assert_eq!(convert(&o_math(&inner)), "cases(x > 0, y < 1)");
    }

    #[test]
    fn bare_matrix() {
        let mr = |a: &str, b: &str| {
            format!(
                r#"<m:mr><m:e>{}</m:e><m:e>{}</m:e></m:mr>"#,
                run(a),
                run(b)
            )
        };
        let inner = format!(r#"<m:m>{}{}</m:m>"#, mr("1", "2"), mr("3", "4"));
        assert_eq!(convert(&o_math(&inner)), "mat(1, 2; 3, 4)");
    }

    #[test]
    fn eq_arr_becomes_cases() {
        let inner = format!(
            r#"<m:eqArr><m:e>{}</m:e><m:e>{}</m:e></m:eqArr>"#,
            run("x"),
            run("y")
        );
        assert_eq!(convert(&o_math(&inner)), "cases(x, y)");
    }

    #[test]
    fn accent_hat() {
        let inner = format!(
            r#"<m:acc><m:accPr><m:chr m:val="&#x0302;"/></m:accPr><m:e>{}</m:e></m:acc>"#,
            run("𝑥")
        );
        assert_eq!(convert(&o_math(&inner)), "hat(x)");
    }

    #[test]
    fn accent_arrow() {
        let inner = format!(
            r#"<m:acc><m:accPr><m:chr m:val="&#x20D7;"/></m:accPr><m:e>{}</m:e></m:acc>"#,
            run("𝑣")
        );
        assert_eq!(convert(&o_math(&inner)), "arrow(v)");
    }

    #[test]
    fn bar_top_is_overline() {
        let inner = format!(
            r#"<m:bar><m:barPr><m:pos m:val="top"/></m:barPr><m:e>{}</m:e></m:bar>"#,
            run("𝑤")
        );
        assert_eq!(convert(&o_math(&inner)), "overline(w)");
    }

    #[test]
    fn bar_bot_is_underline() {
        let inner = format!(
            r#"<m:bar><m:barPr><m:pos m:val="bot"/></m:barPr><m:e>{}</m:e></m:bar>"#,
            run("𝑞")
        );
        assert_eq!(convert(&o_math(&inner)), "underline(q)");
    }

    #[test]
    fn group_chr_overbrace() {
        let inner = format!(
            r#"<m:groupChr><m:groupChrPr><m:chr m:val="&#x23DE;"/><m:pos m:val="top"/></m:groupChrPr><m:e>{}</m:e></m:groupChr>"#,
            run("𝑎")
        );
        assert_eq!(convert(&o_math(&inner)), "overbrace(a)");
    }

    #[test]
    fn group_chr_underbrace() {
        let inner = format!(
            r#"<m:groupChr><m:groupChrPr><m:chr m:val="&#x23DF;"/><m:pos m:val="bot"/></m:groupChrPr><m:e>{}</m:e></m:groupChr>"#,
            run("𝑐")
        );
        assert_eq!(convert(&o_math(&inner)), "underbrace(c)");
    }

    #[test]
    fn box_and_border_box_unwrap_silently() {
        let inner = format!(r#"<m:borderBox><m:e>{}</m:e></m:borderBox>"#, run("𝑥"));
        let mut report = ImportReport::default();
        let Inline::Math(s) = omml_to_inline(&o_math(&inner), &mut report) else {
            panic!("expected math")
        };
        assert_eq!(s.as_str(), "x");
        assert!(report.notes.is_empty(), "borderBox must not report: {:?}", report.notes);
    }

    #[test]
    fn phantom_kept_and_reported() {
        let inner = format!(r#"<m:phant><m:e>{}</m:e></m:phant>"#, run("𝑥"));
        let mut report = ImportReport::default();
        let Inline::Math(s) = omml_to_inline(&o_math(&inner), &mut report) else {
            panic!("expected math")
        };
        assert_eq!(s.as_str(), "x");
        assert_eq!(report.notes.len(), 1);
        assert!(report.notes[0].detail.contains("phantom"));
    }

    #[test]
    fn o_math_para_converts_each_equation_in_order() {
        let src = format!(
            r#"<m:oMathPara {NS}><m:oMath>{}</m:oMath><m:oMath>{}</m:oMath></m:oMathPara>"#,
            run("𝑥"),
            run("𝑦")
        );
        assert_eq!(convert(&src), "x y");
    }

    #[test]
    fn unrecognised_element_recurses_and_reports() {
        let inner = format!(r#"<m:weirdFutureThing>{}</m:weirdFutureThing>"#, run("𝑥"));
        let mut report = ImportReport::default();
        let Inline::Math(s) = omml_to_inline(&o_math(&inner), &mut report) else {
            panic!("expected math")
        };
        assert_eq!(s.as_str(), "x");
        assert_eq!(report.notes.len(), 1);
    }

    #[test]
    fn unparsable_fragment_is_dropped_not_spliced_raw() {
        let mut report = ImportReport::default();
        let inline = omml_to_inline("<m:oMath><m:r><m:t>unterminated", &mut report);
        assert!(matches!(inline, Inline::Text(t) if t.is_empty()));
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].severity, crate::report::Severity::Drop);
    }

    #[test]
    fn bold_style_folds_to_plain_letter_and_reports_loss() {
        // 𝒙 U+1D499 = bold *italic* small x (Typst's `bold(x)` — see
        // `fold_math_alphanumeric`'s doc comment for why bold isn't plain
        // U+1D400-range bold).
        let src = o_math(&run("𝒙"));
        let mut report = ImportReport::default();
        let Inline::Math(s) = omml_to_inline(&src, &mut report) else { panic!("expected math") };
        assert_eq!(s.as_str(), "x");
        assert_eq!(report.notes.len(), 1);
    }

    #[test]
    fn superscript_digit_in_text_recovers_caret_structure() {
        let src = o_math(&plain_run("x²"));
        assert_eq!(convert(&src), "x^2");
    }

    /// A French half-open interval `[0 ; +∞[` (from a real corpus document):
    /// `[`/`]`/`;` are Typst math *syntax* (array/row separators), so a bare
    /// one parses as array syntax instead of content. They must come out as
    /// their symbol names, not the literal character.
    #[test]
    fn bracket_and_semicolon_in_text_become_symbol_names_not_array_syntax() {
        let src = o_math(&plain_run("[0 ; +"));
        assert_eq!(convert(&src), "bracket.l 0 semi +");
    }

    /// `#` introduces a code expression and `$` would end the equation
    /// outright — both need the same symbol-name treatment as brackets/semi.
    #[test]
    fn hash_and_dollar_in_text_become_symbol_names() {
        let src = o_math(&plain_run("#x$"));
        assert_eq!(convert(&src), "hash x dollar");
    }

    /// `m:rad` with an empty `m:e` (the actual corpus bug: `sqrt()`, which
    /// fails with `missing argument: radicand`) must use the `zws` placeholder
    /// instead of collapsing to a call with no argument at all.
    #[test]
    fn empty_radical_uses_zws_placeholder_not_an_empty_call() {
        let inner = "<m:rad><m:e/></m:rad>";
        assert_eq!(convert(&o_math(inner)), "sqrt(zws)");
    }

    /// The same guard applies to every other construct that embeds an
    /// operand directly — accents included (`hat()` is just as much a
    /// missing-argument error as `sqrt()`).
    #[test]
    fn empty_accent_base_uses_zws_placeholder() {
        let inner = "<m:acc><m:e/></m:acc>";
        assert_eq!(convert(&o_math(inner)), "hat(zws)");
    }

    /// …and attachments (`m:sSup`/`m:sSub`/`m:sSubSup`), which use Typst's
    /// postfix `^`/`_` syntax rather than a named call but hit the exact same
    /// "nothing to attach" gap when a side is present but empty.
    #[test]
    fn empty_attachment_operand_uses_zws_placeholder() {
        let inner = format!(r#"<m:sSup><m:e>{}</m:e><m:sup/></m:sSup>"#, run("𝑥"));
        assert_eq!(convert(&o_math(&inner)), "x^(zws)");
    }

    /// The `lo-sw-tdf158023_import` corpus failure: an `m:rad` whose radicand
    /// is the literal plain text `)2(` (Word lets an author type visual
    /// grouping as ordinary characters rather than real OMML delimiters).
    /// Left bare, an unpaired `)`/`(` doesn't just misrender — it shifts
    /// where the enclosing `sqrt(..)` call's own parentheses are read as
    /// closing (`sqrt() 2 ()`, i.e. `sqrt()` with `2 ()` as unrelated
    /// trailing content), which is `missing argument: radicand`, not a
    /// cosmetic issue.
    #[test]
    fn unbalanced_literal_parens_in_plain_text_are_escaped_as_symbols() {
        let inner = format!(r#"<m:rad><m:radPr><m:degHide m:val="on"/></m:radPr><m:deg/><m:e>{}</m:e></m:rad>"#, plain_run(")2("));
        assert_eq!(convert(&o_math(&inner)), "sqrt(paren.r 2 paren.l)");
    }

    /// The `lo-sw-tdf170171` corpus failure, completed: a French half-open
    /// interval `[0 ; +∞[` where the fence characters themselves — not just
    /// the text between them — are `[`, set via an explicit
    /// `m:begChr`/`m:endChr` (`bracket_and_semicolon_in_text_becomes_symbol_
    /// names_not_array_syntax` above only covers the *text*; the fence
    /// glyphs are a separate code path, `convert_delim`/`escape_delim_glyph`,
    /// not `tokenize`).
    #[test]
    fn explicit_delimiter_char_that_is_syntax_significant_is_escaped() {
        let inner = format!(
            r#"<m:d><m:dPr><m:begChr m:val="["/><m:endChr m:val="["/></m:dPr><m:e>{}</m:e></m:d>"#,
            run("𝑥")
        );
        assert_eq!(convert(&o_math(&inner)), "lr(bracket.l x bracket.l)");
    }

    /// The ordinary, overwhelmingly common case must stay pretty: `(`/`)` as
    /// the *outermost* fence are already balanced by `lr(..)`'s own call
    /// parens, so `escape_delim_glyph` leaves them bare rather than
    /// producing the uglier (if equivalent) `lr(paren.l x paren.r)`.
    #[test]
    fn parens_as_the_outermost_delimiter_fence_stay_bare() {
        let inner = format!(
            r#"<m:d><m:dPr><m:begChr m:val="("/><m:endChr m:val=")"/></m:dPr><m:e>{}</m:e></m:d>"#,
            run("𝑥")
        );
        assert_eq!(convert(&o_math(&inner)), "lr(( x ))");
    }

    /// `m:sPre` (`attach(..)`) with a present-but-empty `m:sub`/`m:sup`:
    /// `bl: ()`/`tl: ()` get the same placeholder as every other operand.
    #[test]
    fn empty_spre_side_uses_zws_placeholder() {
        let inner = format!(
            r#"<m:sPre><m:sub/><m:sup>{}</m:sup><m:e>{}</m:e></m:sPre>"#,
            run("235"),
            run("𝑈")
        );
        assert_eq!(convert(&o_math(&inner)), "attach(U, bl: (zws), tl: (235))");
    }
}

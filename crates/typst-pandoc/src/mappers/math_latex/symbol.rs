//! The Unicode-glyph → LaTeX-command table for the math emitter.
//!
//! Typst math is Unicode glyphs (α, ≤, ∫, →, …); LaTeX/texmath wants `\alpha`,
//! `\leq`, `\int`, `\to`. This is a focused table of the ~190 most common math
//! glyphs across Greek, relations, arrows, binary operators, big operators, set
//! theory, logic, dots, and letterlike symbols. The policy:
//!
//! - **ASCII** that is legal bare in math passes through; LaTeX-special ASCII
//!   (`%`, `#`, `&`, `_`, …) is escaped to its math-mode command.
//! - A **mapped Unicode glyph** emits its `\command`.
//! - An **unmapped non-ASCII glyph** returns `None`, which the emitter turns
//!   into a fallback to rasterize (never emit a raw control-character or a glyph
//!   texmath would choke on). This keeps the emitter honest: partial coverage
//!   that degrades to raster, never broken LaTeX.

use ecow::{EcoString, eco_format};

/// Maps a single character to its LaTeX representation, or `None` if it has no
/// clean representation (→ the emitter rasterizes the whole equation).
pub fn map_char(c: char) -> Option<EcoString> {
    // ASCII fast path.
    if c.is_ascii() {
        return Some(ascii(c));
    }
    // Variation selectors / zero-width joiners (U+FE00–U+FE0F, U+200D): these are
    // presentation hints with no math semantics; drop them (emit nothing).
    if matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}') {
        return Some(EcoString::new());
    }
    // Mathematical Alphanumeric Symbols (U+1D400–U+1D7FF): Typst pre-applies the
    // math variant (italic/bold/bb/cal/frak/sans/mono) to letters and digits,
    // emitting them as these styled codepoints. LaTeX/texmath wants the *base*
    // ASCII letter (italic is the math default) optionally wrapped in a style
    // command, so reverse the mapping.
    if let Some(s) = math_alphanumeric(c) {
        return Some(s);
    }
    // Styled Greek (U+1D6A8–U+1D7CB): normalize back to the base Greek letter
    // and map that through the table (`α → \alpha`). Italic is the math default.
    if let Some(base) = math_greek_base(c) {
        return table(base).map(EcoString::from);
    }
    table(c).map(EcoString::from)
}

/// Normalizes a styled Greek codepoint in the Mathematical Alphanumeric Symbols
/// Greek range to its base Greek letter. Each of the five style blocks lays out
/// the same 58-slot Greek sequence (25 uppercase Α–Ω incl. Θ-variant + nabla, 25
/// lowercase α–ω incl. ∂ and variants), so we reduce modulo the block size and
/// map the slot to its canonical Greek char.
fn math_greek_base(c: char) -> Option<char> {
    let u = c as u32;
    // Five style blocks, each 58 codepoints wide, starting here.
    const STARTS: [u32; 5] = [0x1D6A8, 0x1D6E2, 0x1D71C, 0x1D756, 0x1D790];
    const WIDTH: u32 = 58;
    let slot = STARTS
        .iter()
        .find_map(|&s| (u >= s && u < s + WIDTH).then(|| u - s))?;
    // Slot → base Greek char. The sequence (per Unicode):
    // 0..25  uppercase: Α Β Γ Δ Ε Ζ Η Θ Ι Κ Λ Μ Ν Ξ Ο Π Ρ ϴ Σ Τ Υ Φ Χ Ψ Ω
    // 25     ∇ (nabla)
    // 26..51 lowercase: α β γ δ ε ζ η θ ι κ λ μ ν ξ ο π ρ ς σ τ υ φ χ ψ ω
    // 51     ∂ (partial)
    // 52..58 variants: ϵ ϑ ϰ ϕ ϱ ϖ
    const SEQ: &[char] = &[
        'Α', 'Β', 'Γ', 'Δ', 'Ε', 'Ζ', 'Η', 'Θ', 'Ι', 'Κ', 'Λ', 'Μ', 'Ν', 'Ξ', 'Ο', 'Π',
        'Ρ', 'ϴ', 'Σ', 'Τ', 'Υ', 'Φ', 'Χ', 'Ψ', 'Ω', '∇', 'α', 'β', 'γ', 'δ', 'ε', 'ζ',
        'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ', 'ς', 'σ', 'τ', 'υ', 'φ',
        'χ', 'ψ', 'ω', '∂', 'ϵ', 'ϑ', 'ϰ', 'ϕ', 'ϱ', 'ϖ',
    ];
    SEQ.get(slot as usize).copied()
}

/// Reverses a Mathematical Alphanumeric Symbols codepoint to its base ASCII
/// letter/digit with the appropriate LaTeX style wrapper. Returns `None` for
/// codepoints outside the block (or the reserved holes, which fall through to
/// the letterlike table — e.g. ℎ, ℝ).
fn math_alphanumeric(c: char) -> Option<EcoString> {
    let u = c as u32;
    if !(0x1D400..=0x1D7FF).contains(&u) {
        return None;
    }
    // Each style spans 52 letters (A–Z then a–z); digit styles span 10.
    // Map (style, base-char) for the letter blocks, then the digit blocks.
    let letter = |base: u8, idx: u32| -> char {
        // idx 0..26 → 'A'+idx ; 26..52 → 'a'+(idx-26)
        if idx < 26 {
            (base + idx as u8) as char
        } else {
            (b'a' + (idx - 26) as u8) as char
        }
    };

    // Letter style blocks: (start, wrapper). 52 letters each.
    const LETTER_BLOCKS: &[(u32, &str)] = &[
        (0x1D400, "\\mathbf{{{}}}"),         // bold
        (0x1D434, "{}"),                     // italic (default — no wrapper)
        (0x1D468, "\\mathbf{{{}}}"),         // bold italic → bold
        (0x1D49C, "\\mathcal{{{}}}"),        // script
        (0x1D4D0, "\\mathcal{{{}}}"),        // bold script
        (0x1D504, "\\mathfrak{{{}}}"),       // fraktur
        (0x1D538, "\\mathbb{{{}}}"),         // double-struck
        (0x1D56C, "\\mathfrak{{{}}}"),       // bold fraktur
        (0x1D5A0, "\\mathsf{{{}}}"),         // sans-serif
        (0x1D5D4, "\\mathsf{{{}}}"),         // sans bold
        (0x1D608, "\\mathsf{{{}}}"),         // sans italic
        (0x1D63C, "\\mathsf{{{}}}"),         // sans bold italic
        (0x1D670, "\\mathtt{{{}}}"),         // monospace
    ];
    for &(start, wrapper) in LETTER_BLOCKS {
        if (start..start + 52).contains(&u) {
            let base = letter(b'A', u - start);
            return Some(if wrapper == "{}" {
                eco_format!("{base}")
            } else {
                // wrapper has `{{{}}}` → `\mathbf{X}`
                EcoString::from(wrapper.replacen("{{{}}}", &format!("{{{base}}}"), 1))
            });
        }
    }

    // Digit style blocks: 10 digits each.
    const DIGIT_BLOCKS: &[(u32, &str)] = &[
        (0x1D7CE, "\\mathbf{{{}}}"),  // bold digits
        (0x1D7D8, "\\mathbb{{{}}}"),  // double-struck digits
        (0x1D7E2, "{}"),              // sans digits → plain
        (0x1D7EC, "\\mathbf{{{}}}"),  // sans bold digits → bold
        (0x1D7F6, "\\mathtt{{{}}}"),  // mono digits
    ];
    for &(start, wrapper) in DIGIT_BLOCKS {
        if (start..start + 10).contains(&u) {
            let d = char::from(b'0' + (u - start) as u8);
            return Some(if wrapper == "{}" {
                eco_format!("{d}")
            } else {
                EcoString::from(wrapper.replacen("{{{}}}", &format!("{{{d}}}"), 1))
            });
        }
    }
    None
}

/// LaTeX form of an ASCII char in math mode (escaping the specials).
fn ascii(c: char) -> EcoString {
    match c {
        '%' => "\\%".into(),
        '#' => "\\#".into(),
        '&' => "\\&".into(),
        '_' => "\\_".into(),
        '$' => "\\$".into(),
        '{' => "\\{".into(),
        '}' => "\\}".into(),
        '~' => "\\sim ".into(),
        '^' => "\\hat{} ".into(),
        '\\' => "\\backslash ".into(),
        // Everything else (letters, digits, `+ - = < > ( ) [ ] | / . , ; : ! ? *`)
        // is legal bare in math mode.
        _ => eco_format!("{c}"),
    }
}

/// Maps a known operator/function name (`sin`, `lim`, …) to its LaTeX command,
/// so it typesets upright with correct spacing. Returns `None` for an unknown
/// name (the emitter wraps it in `\text{…}`).
pub fn operator_name(s: &str) -> Option<&'static str> {
    Some(match s {
        "sin" => "\\sin",
        "cos" => "\\cos",
        "tan" => "\\tan",
        "cot" => "\\cot",
        "sec" => "\\sec",
        "csc" => "\\csc",
        "sinh" => "\\sinh",
        "cosh" => "\\cosh",
        "tanh" => "\\tanh",
        "coth" => "\\coth",
        "arcsin" => "\\arcsin",
        "arccos" => "\\arccos",
        "arctan" => "\\arctan",
        "log" => "\\log",
        "ln" => "\\ln",
        "lg" => "\\lg",
        "exp" => "\\exp",
        "lim" => "\\lim",
        "limsup" => "\\limsup",
        "liminf" => "\\liminf",
        "max" => "\\max",
        "min" => "\\min",
        "sup" => "\\sup",
        "inf" => "\\inf",
        "det" => "\\det",
        "deg" => "\\deg",
        "dim" => "\\dim",
        "ker" => "\\ker",
        "gcd" => "\\gcd",
        "hom" => "\\hom",
        "arg" => "\\arg",
        "Pr" => "\\Pr",
        "mod" => "\\bmod",
        _ => return None,
    })
}

/// The Unicode → LaTeX command table (non-ASCII only).
fn table(c: char) -> Option<&'static str> {
    Some(match c {
        // -- Greek lowercase ------------------------------------------------
        'α' => "\\alpha",
        'β' => "\\beta",
        'γ' => "\\gamma",
        'δ' => "\\delta",
        'ε' => "\\varepsilon",
        'ϵ' => "\\epsilon",
        'ζ' => "\\zeta",
        'η' => "\\eta",
        'θ' => "\\theta",
        'ϑ' => "\\vartheta",
        'ϴ' => "\\Theta",
        'ι' => "\\iota",
        'κ' => "\\kappa",
        'ϰ' => "\\varkappa",
        'λ' => "\\lambda",
        'μ' => "\\mu",
        'ν' => "\\nu",
        'ξ' => "\\xi",
        'ο' => "o",
        'π' => "\\pi",
        'ϖ' => "\\varpi",
        'ρ' => "\\rho",
        'ϱ' => "\\varrho",
        'σ' => "\\sigma",
        'ς' => "\\varsigma",
        'τ' => "\\tau",
        'υ' => "\\upsilon",
        'φ' => "\\varphi",
        'ϕ' => "\\phi",
        'χ' => "\\chi",
        'ψ' => "\\psi",
        'ω' => "\\omega",
        // -- Greek uppercase ------------------------------------------------
        'Γ' => "\\Gamma",
        'Δ' => "\\Delta",
        'Θ' => "\\Theta",
        'Λ' => "\\Lambda",
        'Ξ' => "\\Xi",
        'Π' => "\\Pi",
        'Σ' => "\\Sigma",
        'Υ' => "\\Upsilon",
        'Φ' => "\\Phi",
        'Ψ' => "\\Psi",
        'Ω' => "\\Omega",
        // -- Relations ------------------------------------------------------
        '≤' => "\\leq",
        '≥' => "\\geq",
        '≠' => "\\neq",
        '≈' => "\\approx",
        '≡' => "\\equiv",
        '∼' => "\\sim",
        '≃' => "\\simeq",
        '≅' => "\\cong",
        '≪' => "\\ll",
        '≫' => "\\gg",
        '≺' => "\\prec",
        '≻' => "\\succ",
        '⪯' => "\\preceq",
        '⪰' => "\\succeq",
        '∝' => "\\propto",
        '≜' => "\\triangleq",
        '≝' => "\\overset{\\text{def}}{=}",
        '⊨' => "\\models",
        '⊢' => "\\vdash",
        '⊣' => "\\dashv",
        '≐' => "\\doteq",
        '⩽' => "\\leqslant",
        '⩾' => "\\geqslant",
        '⊥' => "\\perp",
        '∥' => "\\parallel",
        '≮' => "\\nless",
        '≯' => "\\ngtr",
        '≰' => "\\nleq",
        '≱' => "\\ngeq",
        // -- Arrows ---------------------------------------------------------
        '→' => "\\to",
        '←' => "\\leftarrow",
        '↔' => "\\leftrightarrow",
        '⇒' => "\\Rightarrow",
        '⇐' => "\\Leftarrow",
        '⇔' => "\\Leftrightarrow",
        '↦' => "\\mapsto",
        '↑' => "\\uparrow",
        '↓' => "\\downarrow",
        '↕' => "\\updownarrow",
        '⇑' => "\\Uparrow",
        '⇓' => "\\Downarrow",
        '⟶' => "\\longrightarrow",
        '⟵' => "\\longleftarrow",
        '⟷' => "\\longleftrightarrow",
        '⟹' => "\\Longrightarrow",
        '⟸' => "\\Longleftarrow",
        '⟺' => "\\Longleftrightarrow",
        '↪' => "\\hookrightarrow",
        '↩' => "\\hookleftarrow",
        '⇀' => "\\rightharpoonup",
        '↼' => "\\leftharpoonup",
        '⇌' => "\\rightleftharpoons",
        '↗' => "\\nearrow",
        '↘' => "\\searrow",
        '↖' => "\\nwarrow",
        '↙' => "\\swarrow",
        // -- Binary operators ----------------------------------------------
        '×' => "\\times",
        '÷' => "\\div",
        '⋅' => "\\cdot",
        '∘' => "\\circ",
        '±' => "\\pm",
        '∓' => "\\mp",
        '∗' => "\\ast",
        '⋆' => "\\star",
        '⊕' => "\\oplus",
        '⊖' => "\\ominus",
        '⊗' => "\\otimes",
        '⊘' => "\\oslash",
        '⊙' => "\\odot",
        '⊞' => "\\boxplus",
        '⊠' => "\\boxtimes",
        '∙' => "\\bullet",
        '∔' => "\\dotplus",
        '⊓' => "\\sqcap",
        '⊔' => "\\sqcup",
        '⊎' => "\\uplus",
        '⋄' => "\\diamond",
        '△' => "\\triangle",
        '▽' => "\\triangledown",
        '◁' => "\\triangleleft",
        '▷' => "\\triangleright",
        '⊲' => "\\lhd",
        '⊳' => "\\rhd",
        '∧' => "\\wedge",
        '∨' => "\\vee",
        '⊼' => "\\barwedge",
        '†' => "\\dagger",
        '‡' => "\\ddagger",
        '≀' => "\\wr",
        '⊛' => "\\circledast",
        '∖' => "\\setminus",
        // -- Big operators --------------------------------------------------
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∐' => "\\coprod",
        '∫' => "\\int",
        '∬' => "\\iint",
        '∭' => "\\iiint",
        '∮' => "\\oint",
        '∯' => "\\oiint",
        '∰' => "\\oiiint",
        '⋃' => "\\bigcup",
        '⋂' => "\\bigcap",
        '⨄' => "\\biguplus",
        '⨆' => "\\bigsqcup",
        '⋁' => "\\bigvee",
        '⋀' => "\\bigwedge",
        '⨀' => "\\bigodot",
        '⨁' => "\\bigoplus",
        '⨂' => "\\bigotimes",
        // -- Set theory -----------------------------------------------------
        '∈' => "\\in",
        '∉' => "\\notin",
        '∋' => "\\ni",
        '⊂' => "\\subset",
        '⊃' => "\\supset",
        '⊆' => "\\subseteq",
        '⊇' => "\\supseteq",
        '⊊' => "\\subsetneq",
        '⊋' => "\\supsetneq",
        '∪' => "\\cup",
        '∩' => "\\cap",
        '∅' => "\\emptyset",
        '⊄' => "\\not\\subset",
        '∁' => "\\complement",
        // -- Logic & misc ---------------------------------------------------
        '∀' => "\\forall",
        '∃' => "\\exists",
        '∄' => "\\nexists",
        '¬' => "\\neg",
        '∇' => "\\nabla",
        '∂' => "\\partial",
        '∞' => "\\infty",
        'ℵ' => "\\aleph",
        'ℶ' => "\\beth",
        '∠' => "\\angle",
        '∡' => "\\measuredangle",
        '√' => "\\surd",
        '′' => "\\prime",
        '∎' => "\\blacksquare",
        '⋯' => "\\cdots",
        '⋮' => "\\vdots",
        '⋱' => "\\ddots",
        '…' => "\\ldots",
        '⊤' => "\\top",
        '∴' => "\\therefore",
        '∵' => "\\because",
        '∶' => ":",
        '⁇' => "??",
        '∆' => "\\Delta",
        '−' => "-",  // U+2212 MINUS SIGN → ASCII hyphen-minus
        '·' => "\\cdot",
        '°' => "^\\circ",
        '∣' => "\\mid",
        '‖' => "\\|",
        '⟨' => "\\langle",
        '⟩' => "\\rangle",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        // -- Letterlike / blackboard ---------------------------------------
        'ℝ' => "\\mathbb{R}",
        'ℂ' => "\\mathbb{C}",
        'ℕ' => "\\mathbb{N}",
        'ℤ' => "\\mathbb{Z}",
        'ℚ' => "\\mathbb{Q}",
        'ℍ' => "\\mathbb{H}",
        'ℙ' => "\\mathbb{P}",
        'ℓ' => "\\ell",
        'ℏ' => "\\hbar",
        'ℎ' => "h",          // U+210E PLANCK CONSTANT = italic h
        'ℑ' => "\\Im",
        'ℜ' => "\\Re",
        '℘' => "\\wp",
        'ℒ' => "\\mathcal{L}",
        'ℱ' => "\\mathcal{F}",
        'ℳ' => "\\mathcal{M}",
        'ℬ' => "\\mathcal{B}",
        'ℰ' => "\\mathcal{E}",
        'ℋ' => "\\mathcal{H}",
        'ℐ' => "\\mathcal{I}",
        'ℛ' => "\\mathcal{R}",
        'ℯ' => "e",
        'ℴ' => "o",
        'ℊ' => "g",
        '⅀' => "\\sum",
        // -- More symbols (corpus tail) ------------------------------------
        '□' => "\\square",
        '■' => "\\blacksquare",
        '◻' => "\\square",
        '◊' => "\\diamond",
        '★' => "\\star",
        '☆' => "\\star",
        '✓' => "\\checkmark",
        '⟂' => "\\perp",     // U+27C2 PERPENDICULAR (U+22A5 ⊥ is in relations)
        '≔' => "\\coloneqq", // U+2254 COLON EQUALS
        '≕' => "\\eqqcolon",
        '″' => "\\prime\\prime", // U+2033 double prime
        '‴' => "\\prime\\prime\\prime",
        '—' => "\\text{---}",    // em dash
        '–' => "\\text{--}",     // en dash
        '℃' => "{}^\\circ\\mathrm{C}",
        '℉' => "{}^\\circ\\mathrm{F}",
        'Å' => "\\text{\\AA}",
        '⌀' => "\\diameter",
        // -- Spaces / punctuation ------------------------------------------
        '\u{00A0}' => "\\ ", // nbsp
        '\u{2009}' => "\\,", // thin space
        '\u{2002}' => "\\;", // en space
        '\u{2003}' => "\\quad ", // em space
        _ => return None,
    })
}

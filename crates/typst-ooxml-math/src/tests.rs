//! Unit tests over **real** OMML fixtures.
//!
//! Every `OMML_*` constant below was extracted verbatim from `word/document.xml`
//! of a `.docx` produced by `typst compile --format docx` (the DOCX exporter in
//! `crates/typst-docx`) — not hand-written — so the tests exercise the exact
//! shapes the generator emits, and each asserted output is idiomatic,
//! paste-ready Typst that round-trips through the real compiler.

use super::omml_to_typst;

// --- Fixtures (verbatim from a typst-generated .docx) ----------------------

const OMML_POW: &str = r#"<m:oMath><m:sSup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑎</m:t></m:r></m:e><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath>"#;

const OMML_FRAC: &str = r#"<m:oMath><m:f><m:num><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑎</m:t></m:r></m:num><m:den><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑏</m:t></m:r></m:den></m:f></m:oMath>"#;

const OMML_SQRT: &str = r#"<m:oMath><m:rad><m:radPr><m:degHide m:val="on"/></m:radPr><m:deg/><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e></m:rad></m:oMath>"#;

const OMML_ROOT: &str = r#"<m:oMath><m:rad><m:deg><m:r><m:rPr><m:nor/></m:rPr><m:t>3</m:t></m:r></m:deg><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e></m:rad></m:oMath>"#;

const OMML_SUB: &str = r#"<m:oMath><m:sSub><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑖</m:t></m:r></m:sub></m:sSub></m:oMath>"#;

const OMML_POW_GROUP: &str = r#"<m:oMath><m:sSup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑒</m:t></m:r></m:e><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>+</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>1</m:t></m:r></m:sup></m:sSup></m:oMath>"#;

const OMML_SUBSUP: &str = r#"<m:oMath><m:sSubSup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑖</m:t></m:r></m:sub><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>2</m:t></m:r></m:sup></m:sSubSup></m:oMath>"#;

const OMML_SUM: &str = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∑"/><m:limLoc m:val="undOvr"/><m:grow m:val="1"/></m:naryPr><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑖</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>=</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>1</m:t></m:r></m:sub><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑛</m:t></m:r></m:sup><m:e><m:sSub><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑖</m:t></m:r></m:sub></m:sSub></m:e></m:nary></m:oMath>"#;

const OMML_INTEGRAL: &str = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∫"/><m:limLoc m:val="subSup"/><m:grow m:val="1"/></m:naryPr><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>0</m:t></m:r></m:sub><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>1</m:t></m:r></m:sup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑓</m:t></m:r></m:e></m:nary></m:oMath>"#;

const OMML_PRODUCT: &str = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∏"/><m:limLoc m:val="undOvr"/><m:grow m:val="1"/></m:naryPr><m:sub><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑘</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>=</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>1</m:t></m:r></m:sub><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑛</m:t></m:r></m:sup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑘</m:t></m:r></m:e></m:nary></m:oMath>"#;

const OMML_MATRIX: &str = r#"<m:oMath><m:d><m:e><m:m><m:mPr><m:baseJc m:val="center"/><m:plcHide m:val="on"/><m:mcs><m:mc><m:mcPr><m:count m:val="2"/><m:mcJc m:val="center"/></m:mcPr></m:mc></m:mcs></m:mPr><m:mr><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑎</m:t></m:r></m:e><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑏</m:t></m:r></m:e></m:mr><m:mr><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑐</m:t></m:r></m:e><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑑</m:t></m:r></m:e></m:mr></m:m></m:e></m:d></m:oMath>"#;

const OMML_GREEK: &str = r#"<m:oMath><m:r><m:rPr><m:nor/></m:rPr><m:t>𝛼</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>+</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝛽</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>=</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝛾</m:t></m:r></m:oMath>"#;

const OMML_RELATIONS: &str = r#"<m:oMath><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>≤</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑦</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>≥</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑧</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>≠</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑤</m:t></m:r></m:oMath>"#;

const OMML_ARROWS: &str = r#"<m:oMath><m:r><m:rPr><m:nor/></m:rPr><m:t>𝐴</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>→</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝐵</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>⇒</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝐶</m:t></m:r></m:oMath>"#;

const OMML_BINOM: &str = r#"<m:oMath><m:d><m:e><m:f><m:fPr><m:type m:val="noBar"/></m:fPr><m:num><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑛</m:t></m:r></m:num><m:den><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑘</m:t></m:r></m:den></m:f></m:e></m:d></m:oMath>"#;

const OMML_BOLD: &str = r#"<m:oMath><m:r><m:rPr><m:nor/></m:rPr><m:t>∇</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>×</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑭</m:t></m:r></m:oMath>"#;

const OMML_SETS: &str = r#"<m:oMath><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>∈</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>ℝ</m:t></m:r></m:oMath>"#;

const OMML_BAR: &str = r#"<m:oMath><m:bar><m:barPr><m:pos m:val="top"/></m:barPr><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e></m:bar><m:r><m:rPr><m:nor/></m:rPr><m:t>+</m:t></m:r><m:bar><m:barPr><m:pos m:val="bot"/></m:barPr><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑦</m:t></m:r></m:e></m:bar></m:oMath>"#;

const OMML_LIM: &str = r#"<m:oMath><m:limLow><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>lim</m:t></m:r></m:e><m:lim><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>→</m:t></m:r><m:r><m:rPr><m:nor/></m:rPr><m:t>0</m:t></m:r></m:lim></m:limLow></m:oMath>"#;

const OMML_HAT: &str = r#"<m:oMath><m:acc><m:accPr><m:chr m:val="̂"/></m:accPr><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑥</m:t></m:r></m:e></m:acc></m:oMath>"#;

const OMML_BB_POW: &str = r#"<m:oMath><m:sSup><m:e><m:r><m:rPr><m:nor/></m:rPr><m:t>ℝ</m:t></m:r></m:e><m:sup><m:r><m:rPr><m:nor/></m:rPr><m:t>𝑛</m:t></m:r></m:sup></m:sSup></m:oMath>"#;

// --- Tests, in roughly increasing complexity -------------------------------

#[test]
fn superscript_is_bare_not_upright() {
    // The headline case: a plane-1 italic `𝑎` and a digit `2` must become the
    // bare `a^2`, never pandoc's `upright("𝑎")^(upright("2"))`.
    assert_eq!(omml_to_typst(OMML_POW), "a^2");
}

#[test]
fn fraction() {
    // Simple single-token operands read as `a/b` (what a Typst author writes),
    // not the verbose `frac(a, b)`.
    assert_eq!(omml_to_typst(OMML_FRAC), "a/b");
}

#[test]
fn fraction_keeps_frac_for_compound_operands() {
    // A structured numerator/denominator must stay `frac(..)`, which is safe
    // in any position; `/` is reserved for atomic operands.
    let omml = r#"<m:oMath><m:f><m:num><m:r><m:t>𝑎+𝑏</m:t></m:r></m:num><m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath>"#;
    assert_eq!(omml_to_typst(omml), "frac(a + b, 2)");
}

#[test]
fn unary_sign_attaches_tight() {
    // A leading `-` is a sign, not a binary operator: `-x`, not `- x`.
    let omml = r#"<m:oMath><m:r><m:t>-</m:t></m:r><m:r><m:t>𝑥</m:t></m:r></m:oMath>"#;
    assert_eq!(omml_to_typst(omml), "-x");
}

#[test]
fn square_root_hides_empty_degree() {
    assert_eq!(omml_to_typst(OMML_SQRT), "sqrt(x)");
}

#[test]
fn nth_root_uses_degree() {
    assert_eq!(omml_to_typst(OMML_ROOT), "root(3, x)");
}

#[test]
fn subscript_needs_no_parens() {
    assert_eq!(omml_to_typst(OMML_SUB), "x_i");
}

#[test]
fn superscript_groups_multi_token_operand() {
    // A single token stays bare; a compound operand is parenthesised.
    assert_eq!(omml_to_typst(OMML_POW_GROUP), "e^(x + 1)");
}

#[test]
fn combined_sub_and_superscript() {
    assert_eq!(omml_to_typst(OMML_SUBSUP), "x_i^2");
}

#[test]
fn nary_sum_with_limits_and_summand() {
    // The summand (`x_i`) is bound into the sum, and the lower bound tightens
    // to `i=1` the way a user writes it.
    assert_eq!(omml_to_typst(OMML_SUM), "sum_(i=1)^n x_i");
}

#[test]
fn nary_integral_with_bounds() {
    assert_eq!(omml_to_typst(OMML_INTEGRAL), "integral_0^1 f");
}

#[test]
fn nary_product() {
    assert_eq!(omml_to_typst(OMML_PRODUCT), "product_(k=1)^n k");
}

#[test]
fn matrix_drops_default_paren_fence() {
    // Word wraps `mat` in a `( )` delimiter; we recover the bare `mat(..)`.
    assert_eq!(omml_to_typst(OMML_MATRIX), "mat(a, b; c, d)");
}

#[test]
fn greek_letters_by_name() {
    assert_eq!(omml_to_typst(OMML_GREEK), "alpha + beta = gamma");
}

#[test]
fn relations_use_shorthands() {
    assert_eq!(omml_to_typst(OMML_RELATIONS), "x <= y >= z != w");
}

#[test]
fn arrows_use_shorthands() {
    assert_eq!(omml_to_typst(OMML_ARROWS), "A -> B => C");
}

#[test]
fn binom_drops_redundant_parens() {
    assert_eq!(omml_to_typst(OMML_BINOM), "binom(n, k)");
}

#[test]
fn bold_letter_and_operators() {
    assert_eq!(omml_to_typst(OMML_BOLD), "nabla times bold(F)");
}

#[test]
fn set_membership_and_blackboard() {
    assert_eq!(omml_to_typst(OMML_SETS), "x in bb(R)");
}

#[test]
fn over_and_under_bar() {
    assert_eq!(omml_to_typst(OMML_BAR), "overline(x) + underline(y)");
}

#[test]
fn limit_below_base() {
    assert_eq!(omml_to_typst(OMML_LIM), "lim_(x -> 0)");
}

#[test]
fn hat_accent() {
    assert_eq!(omml_to_typst(OMML_HAT), "hat(x)");
}

#[test]
fn blackboard_base_with_superscript() {
    assert_eq!(omml_to_typst(OMML_BB_POW), "bb(R)^n");
}

// --- Robustness ------------------------------------------------------------

#[test]
fn empty_math_is_empty() {
    assert_eq!(omml_to_typst("<m:oMath/>"), "");
}

#[test]
fn garbage_never_panics() {
    // Malformed / non-XML input must degrade gracefully, not panic.
    let _ = omml_to_typst("not xml at all <<<");
    let _ = omml_to_typst("<m:oMath><m:f><m:num>");
    let _ = omml_to_typst("");
    let _ = omml_to_typst("𝑥 ≤ 𝑦 → 𝑧"); // bare styled text, no tags
}

#[test]
fn unwrapped_fragment_without_namespace() {
    // A fragment lacking the `m:` namespace declaration still converts (we wrap
    // it in a namespaced root before parsing).
    assert_eq!(omml_to_typst(OMML_POW), "a^2");
}

#[test]
fn finds_math_inside_a_wrapper() {
    // An `m:oMathPara` (block-equation) wrapper is transparent.
    let wrapped = format!(
        r#"<m:oMathPara><m:oMathParaPr/><m:oMath>{}</m:oMath></m:oMathPara>"#,
        r#"<m:sSup><m:e><m:r><m:t>𝑎</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup>"#
    );
    assert_eq!(omml_to_typst(&wrapped), "a^2");
}

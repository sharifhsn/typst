//! OMML → readable Unicode, for the plain-text compatibility branch.
//!
//! This is the *reverse* direction from the rest of the OMML code: `omml_build`
//! spells fragments and `typst_omml` decides what to spell, while this module
//! reads a finished fragment back and flattens it into one line of Unicode.
//!
//! That line is what a consumer without the OMML extension sees — an
//! `mc:Fallback` DrawingML run in PPTX. Concatenating the `m:t` leaves alone
//! would lose exactly the structure OMML exists to carry, so limits, scripts,
//! fraction bars, radicals, fences, matrices and multi-line rows each get a
//! linear spelling here.

use ecow::EcoString;
use roxmltree::Node;
use unicode_normalization::UnicodeNormalization;

use crate::xmlread::child;

/// Produces a readable, editable Unicode fallback for an OMML fragment.
///
/// PowerPoint consumes the native OMML branch, but consumers such as
/// LibreOffice may select DrawingML's plain-text compatibility branch instead.
/// Concatenating visual glyphs loses the role of limits, scripts, and fraction
/// bars, so recover those semantics from the structured OMML tree.
pub fn omml_fallback_text(fragment: &str) -> Option<EcoString> {
    let wrapped;
    let source = if fragment.contains("xmlns:m=") {
        fragment
    } else {
        wrapped = format!(
            "<root xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\">{fragment}</root>"
        );
        &wrapped
    };
    let document = roxmltree::Document::parse(source).ok()?;
    let text = linearize_omml(document.root_element())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then(|| EcoString::from(text))
}

fn linearize_omml(node: Node<'_, '_>) -> String {
    let name = node.tag_name().name();
    match name {
        "t" => destyle(node.text().unwrap_or_default()),
        "r" => destyle(
            node.descendants()
                .find(|child| child.is_element() && child.tag_name().name() == "t")
                .and_then(|child| child.text())
                .unwrap_or_default(),
        ),
        "nary" => {
            let chr = child(node, "naryPr")
                .and_then(|pr| child(pr, "chr"))
                .and_then(attribute_val)
                .unwrap_or_else(|| "∑".to_owned());
            let sub = child_text(node, "sub");
            let sup = child_text(node, "sup");
            let body = child_text(node, "e");
            format!("{chr}{}{}{body}", script_text(&sub, false), script_text(&sup, true))
        }
        "sSup" => format!(
            "{}{}",
            child_text(node, "e"),
            script_text(&child_text(node, "sup"), true)
        ),
        "sSub" => format!(
            "{}{}",
            child_text(node, "e"),
            script_text(&child_text(node, "sub"), false)
        ),
        "sSubSup" => format!(
            "{}{}{}",
            child_text(node, "e"),
            script_text(&child_text(node, "sub"), false),
            script_text(&child_text(node, "sup"), true)
        ),
        "sPre" => format!(
            "{}{}{}",
            script_text(&child_text(node, "sub"), false),
            script_text(&child_text(node, "sup"), true),
            child_text(node, "e")
        ),
        "f" => {
            let num = child_text(node, "num");
            let den = child_text(node, "den");
            format!("{}/{}", fraction_operand(&num), fraction_operand(&den))
        }
        "rad" => {
            let degree = child_text(node, "deg");
            let body = child_text(node, "e");
            if degree.trim().is_empty() {
                if readable_math_atom(&body) {
                    format!("√{}", body.trim())
                } else {
                    format!("√({body})")
                }
            } else {
                format!("root_{}({body})", degree.trim())
            }
        }
        "d" => {
            let props = child(node, "dPr");
            let begin = props
                .and_then(|pr| child(pr, "begChr"))
                .and_then(attribute_val)
                .unwrap_or_else(|| "(".to_owned());
            let end = props
                .and_then(|pr| child(pr, "endChr"))
                .and_then(attribute_val)
                .unwrap_or_else(|| ")".to_owned());
            format!("{begin}{}{end}", child_text(node, "e"))
        }
        "acc" => {
            let accent = child(node, "accPr")
                .and_then(|pr| child(pr, "chr"))
                .and_then(attribute_val)
                .unwrap_or_default();
            format!("{}{accent}", child_text(node, "e"))
        }
        "bar" => child_text(node, "e"),
        // A limit written under or over its base (`lim_(x→0)`, and the
        // annotation of an under/overbrace) reads the same linearly as a
        // sub/superscript does.
        "limLow" => format!(
            "{}{}",
            child_text(node, "e"),
            script_text(&child_text(node, "lim"), false)
        ),
        "limUpp" => format!(
            "{}{}",
            child_text(node, "e"),
            script_text(&child_text(node, "lim"), true)
        ),
        // A stretchy arrow's `m:e` holds a hidden phantom seeded with a *copy*
        // of the wider limit, purely to give the arrow its authored width.
        // Reading it back would print that limit twice.
        "phant" => String::new(),
        // A grouping character (over/underbrace, and the stretchy-arrow base)
        // carries its brace in `m:groupChrPr`; the annotation, if any, is
        // attached outside as a limit. Keep the base and the brace.
        "groupChr" => {
            let chr = child(node, "groupChrPr")
                .and_then(|pr| child(pr, "chr"))
                .and_then(attribute_val)
                .unwrap_or_default();
            let body = child_text(node, "e");
            if body.trim().is_empty() {
                chr
            } else if readable_math_atom(&body) {
                format!("{chr}{}", body.trim())
            } else {
                format!("{chr}({body})")
            }
        }
        // A matrix and a multi-line aligned body are both `m:m`. They read
        // differently: matrix cells are independent entries and want a
        // separator, whereas the columns of an aligned body are one broken
        // line and must rejoin seamlessly. Our own emitter marks the latter
        // with a zero column gap, which a matrix never sets.
        "m" => {
            let aligned = child(node, "mPr").and_then(|pr| child(pr, "cGp")).is_some();
            let separator = if aligned { "" } else { ", " };
            node.children()
                .filter(|child| child.is_element() && child.tag_name().name() == "mr")
                .map(|row| {
                    row.children()
                        .filter(|cell| cell.is_element())
                        .map(linearize_omml)
                        .collect::<Vec<_>>()
                        .join(separator)
                })
                .collect::<Vec<_>>()
                .join("; ")
        }
        // Rows of a `gather`-style multi-line equation.
        "eqArr" => node
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "e")
            .map(linearize_omml)
            .collect::<Vec<_>>()
            .join("; "),
        _ if name.ends_with("Pr") => String::new(),
        _ => node
            .children()
            .filter(|child| child.is_element())
            .map(linearize_omml)
            .collect(),
    }
}

/// Maps Typst's pre-styled math letters back to their base characters.
///
/// The lowering emits italics and other math variants as the Unicode codepoint
/// that *is* that variant — `x` becomes 𝑥 (U+1D465), `RR` becomes ℝ. That is
/// right for the native branch, where Word picks a math font. The fallback
/// branch is a plain DrawingML run in whatever font the consumer resolves, and
/// most text fonts have no Plane-1 math coverage at all, so those codepoints
/// arrive as tofu. Their compatibility decomposition is exactly the base letter,
/// which every font has.
///
/// Applied only inside the two supplementary-plane math blocks. Compatibility
/// decomposition is far too broad to run over arbitrary text — it would flatten
/// ligatures, and turn the superscript digits this very module produces back
/// into ordinary ones — and the *basic*-plane letterlike symbols that fill the
/// holes in those blocks (ℝ ℕ ℓ ℎ …) are deliberately left alone: substitute
/// fonts do commonly carry them, and `ℝ` says more than `R`.
fn destyle(text: &str) -> String {
    if !text.chars().any(is_styled_math_letter) {
        return text.to_owned();
    }
    text.chars()
        .map(|chr| {
            if !is_styled_math_letter(chr) {
                return chr;
            }
            let mut decomposed = chr.nfkd();
            match (decomposed.next(), decomposed.next()) {
                (Some(base), None) => base,
                _ => chr,
            }
        })
        .collect()
}

fn is_styled_math_letter(chr: char) -> bool {
    matches!(chr as u32,
        // Mathematical Alphanumeric Symbols, and the Arabic equivalent.
        0x1D400..=0x1D7FF
        | 0x1EE00..=0x1EEFF
        // ℎ, the one hole in the italic run of the block above, and the only
        // letterlike symbol that is *purely* a styling stand-in. The script,
        // fraktur and double-struck letterlike symbols (ℒ ℭ ℝ …) are alphabets
        // in their own right, and are left alone with the rest of their runs.
        | 0x210E)
}

fn child_text(node: Node<'_, '_>, name: &str) -> String {
    child(node, name).map(linearize_omml).unwrap_or_default()
}

fn attribute_val(node: Node<'_, '_>) -> Option<String> {
    node.attributes()
        .find(|attribute| attribute.name() == "val")
        .map(|attribute| attribute.value().to_owned())
}

fn script_text(text: &str, superscript: bool) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let converted = trimmed
        .chars()
        .map(|chr| script_char(chr, superscript))
        .collect::<Option<String>>();
    converted.unwrap_or_else(|| match (superscript, trimmed.chars().count()) {
        (true, 1) => format!("^{trimmed}"),
        (false, 1) => format!("_{trimmed}"),
        (true, _) => format!("^({trimmed})"),
        (false, _) => format!("_({trimmed})"),
    })
}

fn script_char(chr: char, superscript: bool) -> Option<char> {
    if superscript {
        if "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿˣ".contains(chr) {
            return Some(chr);
        }
        if chr == '−' {
            return Some('⁻');
        }
        if chr == 'x' {
            return Some('ˣ');
        }
    } else if "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₙ".contains(chr) {
        return Some(chr);
    }

    let table = if superscript {
        "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿ"
    } else {
        "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₙ"
    };
    let source = "0123456789+-=()n";
    source
        .chars()
        .position(|candidate| candidate == chr)
        .and_then(|index| table.chars().nth(index))
}

fn fraction_operand(text: &str) -> String {
    let trimmed = text.trim();
    if readable_math_atom(trimmed) { trimmed.to_owned() } else { format!("({trimmed})") }
}

/// Characters that decorate a base rather than being one: the radical sign, the
/// prime, and the super/subscript forms this module itself produces.
const DECORATIONS: &str = "√′⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿˣ₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₙ";

/// Whether `text` can stand under a radical or beside a fraction bar without
/// parentheses — that is, whether it is a *single* decorated atom.
///
/// Counting atoms rather than accepting any alphanumeric run matters because
/// the resolved IR carries no inter-atom spaces: `sqrt(x y)` arrives as `xy`,
/// and `√xy` would read as `(√x)y`. A digit run is one atom, so `10/3` still
/// needs no parentheses.
fn readable_math_atom(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|chr| chr.is_alphanumeric() || DECORATIONS.contains(chr))
    {
        return false;
    }
    let mut atoms = 0;
    let mut in_digits = false;
    for chr in trimmed.chars() {
        if DECORATIONS.contains(chr) {
            continue;
        }
        let digit = chr.is_ascii_digit();
        if !(digit && in_digits) {
            atoms += 1;
        }
        in_digits = digit;
    }
    atoms <= 1
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod fallback_tests {
    use super::omml_fallback_text;

    #[test]
    fn linearizes_structured_math_for_plain_text_consumers() {
        let omml = r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:nary><m:naryPr><m:chr m:val="∫"/></m:naryPr><m:sub><m:r><m:t>0</m:t></m:r></m:sub><m:sup><m:r><m:t>1</m:t></m:r></m:sup><m:e><m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:e></m:nary><m:r><m:t>=</m:t></m:r><m:f><m:num><m:r><m:t>1</m:t></m:r></m:num><m:den><m:r><m:t>3</m:t></m:r></m:den></m:f></m:oMath>"#;
        assert_eq!(omml_fallback_text(omml).as_deref(), Some("∫₀¹x²=1/3"));
    }

    #[test]
    fn compacts_common_unicode_math_fallbacks() {
        let omml = r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:nary><m:naryPr><m:chr m:val="∫"/></m:naryPr><m:sub><m:r><m:t>0</m:t></m:r></m:sub><m:sup><m:r><m:t>∞</m:t></m:r></m:sup><m:e><m:sSup><m:e><m:r><m:t>e</m:t></m:r></m:e><m:sup><m:r><m:t>−x²</m:t></m:r></m:sup></m:sSup></m:e></m:nary><m:r><m:t>=</m:t></m:r><m:f><m:num><m:rad><m:deg/><m:e><m:r><m:t>π</m:t></m:r></m:e></m:rad></m:num><m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath>"#;
        assert_eq!(omml_fallback_text(omml).as_deref(), Some("∫₀^∞e⁻ˣ²=√π/2"));
    }

    #[test]
    fn juxtaposed_atoms_keep_their_parentheses() {
        // Resolved math carries no inter-atom spaces, so a radicand or a
        // fraction operand of more than one atom has nothing but parentheses
        // to keep it from binding wrongly: `√xy` reads as `(√x)y`.
        let rad = |body: &str| {
            format!(
                r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:rad><m:deg/><m:e>{body}</m:e></m:rad></m:oMath>"#
            )
        };
        let run = |t: &str| format!("<m:r><m:t>{t}</m:t></m:r>");
        assert_eq!(
            omml_fallback_text(&rad(&(run("x") + &run("y")))).as_deref(),
            Some("√(xy)")
        );
        // One atom, decorated or multi-digit, still needs none.
        assert_eq!(omml_fallback_text(&rad(&run("π"))).as_deref(), Some("√π"));
        assert_eq!(omml_fallback_text(&rad(&run("10"))).as_deref(), Some("√10"));
    }

    #[test]
    fn supplementary_plane_math_letters_come_back_as_plain_ones() {
        // The lowering emits math italics as the codepoint that *is* italic
        // (𝑎 U+1D44E). The fallback is an ordinary text run, so those have to
        // come back — but the basic-plane letterlike symbols do not, because
        // substitute fonts carry them and ℝ says more than R.
        let omml = "<m:oMath xmlns:m=\"http://schemas.openxmlformats.org/\
                    officeDocument/2006/math\"><m:r><m:t>\u{1D44E}\u{1D461}\
                    \u{210E}\u{211D}</m:t></m:r></m:oMath>";
        assert_eq!(omml_fallback_text(omml).as_deref(), Some("ath\u{211D}"));
    }

    #[test]
    fn matrices_and_aligned_rows_stay_apart() {
        let cell = |t: &str| format!("<m:e><m:r><m:t>{t}</m:t></m:r></m:e>");
        let matrix = format!(
            r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:m><m:mPr><m:baseJc m:val="center"/></m:mPr><m:mr>{}{}</m:mr><m:mr>{}{}</m:mr></m:m></m:oMath>"#,
            cell("a"),
            cell("b"),
            cell("c"),
            cell("d")
        );
        assert_eq!(omml_fallback_text(&matrix).as_deref(), Some("a, b; c, d"));

        // The aligned-body matrix is marked by its zero column gap: its
        // columns are one broken line, so they rejoin without a separator.
        let aligned = format!(
            r#"<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:m><m:mPr><m:cGp m:val="0"/></m:mPr><m:mr>{}{}</m:mr><m:mr>{}{}</m:mr></m:m></m:oMath>"#,
            cell("a"),
            cell("=b"),
            cell("c"),
            cell("=d")
        );
        assert_eq!(omml_fallback_text(&aligned).as_deref(), Some("a=b; c=d"));
    }
}

//! OMML fragment writer shared by OOXML exporters.

use ecow::EcoString;
use roxmltree::Node;

use crate::omml_build::{Omml, is_integral_char, is_nary_operator, to_combining};
use crate::xmlread::child;
use typst_library::foundations::{
    Content, Packed, SequenceElem, StyleChain, StyledElem, SymbolElem,
};
use typst_library::layout::HElem;
use typst_library::math::ir::Position;
use typst_library::math::{
    AccentElem, AttachElem, BinomElem, ClassElem, EquationElem, FracElem, FracStyle,
    LimitsElem, LrElem, OpElem, OverlineElem, PrimesElem, RootElem, ScriptsElem,
    StretchElem, UnderlineElem,
};
use typst_library::text::{LinebreakElem, SpaceElem, TextElem as TypstTextElem};

/// Serializes an already-realized equation body to an `<m:oMath>` fragment.
///
/// This is a narrow bridge for layout-driven exporters such as PPTX, where the
/// paged document no longer has an [`Engine`] available for
/// [`resolve_equation`]. The DOCX exporter itself still uses the full
/// engine-backed path above. This helper walks the realized math content tree
/// directly and emits the same OMML vocabulary for common native structures
/// (runs, scripts, fractions, roots, fences, accents, and n-ary operators).
pub fn equation_omml_fragment(elem: &Packed<EquationElem>) -> Option<String> {
    let mut emitter = DirectEmitter::new();
    emitter.buf.open("m:oMath").children();
    emitter.emit_content(&elem.body);
    emitter.buf.close();

    emitter.emitted.then(|| emitter.buf.into_string())
}

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
        "t" => node.text().unwrap_or_default().to_owned(),
        "r" => node
            .descendants()
            .find(|child| child.is_element() && child.tag_name().name() == "t")
            .and_then(|child| child.text())
            .unwrap_or_default()
            .to_owned(),
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
        _ if name.ends_with("Pr") => String::new(),
        _ => node
            .children()
            .filter(|child| child.is_element())
            .map(linearize_omml)
            .collect(),
    }
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

fn readable_math_atom(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|chr| {
            chr.is_alphanumeric() || "√⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿˣ₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₙ".contains(chr)
        })
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
}

/// Best-effort realized-content → OMML emitter used when no `Engine` is
/// available. It intentionally stays independent from [`DocxCtx`] so callers
/// can use it without DOCX package state.
struct DirectEmitter {
    buf: Omml,
    emitted: bool,
}

impl DirectEmitter {
    fn new() -> Self {
        Self { buf: Omml::new(), emitted: false }
    }

    fn emit_content(&mut self, content: &Content) {
        let mut items = Vec::new();
        collect_math_row(content, &mut items);
        self.emit_items(&items);
    }

    fn emit_items(&mut self, items: &[&Content]) {
        let mut i = 0;
        while i < items.len() {
            if let Some((chr, lower, upper)) = nary_attach_content(items[i]) {
                let j = direct_operand_end(items, i + 1);
                self.emit_nary(chr, lower.as_ref(), upper.as_ref(), &items[i + 1..j]);
                i = j;
            } else if let Some(chr) = nary_content_char(items[i]) {
                let j = direct_operand_end(items, i + 1);
                self.emit_nary(chr, None, None, &items[i + 1..j]);
                i = j;
            } else {
                self.emit_item(items[i]);
                i += 1;
            }
        }
    }

    fn emit_item(&mut self, content: &Content) {
        if let Some(elem) = content.to_packed::<SequenceElem>() {
            self.emit_items(&elem.children.iter().collect::<Vec<_>>());
        } else if let Some(elem) = content.to_packed::<StyledElem>() {
            self.emit_content(&elem.child);
        } else if let Some(elem) = content.to_packed::<EquationElem>() {
            self.emit_content(&elem.body);
        } else if let Some(elem) = content.to_packed::<SymbolElem>() {
            self.text_run(&elem.text, false);
        } else if let Some(elem) = content.to_packed::<TypstTextElem>() {
            self.text_run(&elem.text, false);
        } else if content.is::<SpaceElem>()
            || content.is::<HElem>()
            || content.is::<LinebreakElem>()
        {
            self.text_run(" ", false);
        } else if let Some(elem) = content.to_packed::<FracElem>() {
            self.emit_fraction(elem);
        } else if let Some(elem) = content.to_packed::<AttachElem>() {
            self.emit_attach(elem);
        } else if let Some(elem) = content.to_packed::<RootElem>() {
            self.emit_root(elem);
        } else if let Some(elem) = content.to_packed::<LrElem>() {
            self.emit_fenced(&elem.body);
        } else if let Some(elem) = content.to_packed::<BinomElem>() {
            self.emit_binom(elem);
        } else if let Some(elem) = content.to_packed::<AccentElem>() {
            self.emit_accent(elem);
        } else if let Some(elem) = content.to_packed::<PrimesElem>() {
            let s: EcoString = std::iter::repeat_n('′', elem.count).collect();
            self.text_run(&s, false);
        } else if let Some(elem) = content.to_packed::<OpElem>() {
            self.text_run(&elem.text.plain_text(), true);
        } else if let Some(elem) = content.to_packed::<ClassElem>() {
            self.emit_content(&elem.body);
        } else if let Some(elem) = content.to_packed::<ScriptsElem>() {
            self.emit_content(&elem.body);
        } else if let Some(elem) = content.to_packed::<LimitsElem>() {
            self.emit_content(&elem.body);
        } else if let Some(elem) = content.to_packed::<StretchElem>() {
            self.emit_content(&elem.body);
        } else if let Some(elem) = content.to_packed::<OverlineElem>() {
            self.emit_bar(&elem.body, Position::Above);
        } else if let Some(elem) = content.to_packed::<UnderlineElem>() {
            self.emit_bar(&elem.body, Position::Below);
        } else {
            let text = content.plain_text();
            if !text.is_empty() {
                self.text_run(&text, false);
            }
        }
    }

    fn emit_fraction(&mut self, frac: &Packed<FracElem>) {
        match frac.style.get(StyleChain::default()) {
            FracStyle::Horizontal => {
                self.emit_content(&frac.num);
                self.text_run("/", false);
                self.emit_content(&frac.denom);
            }
            FracStyle::Skewed | FracStyle::Vertical => {
                self.emitted = true;
                self.buf.open("m:f").children();
                if frac.style.get(StyleChain::default()) == FracStyle::Skewed {
                    self.buf.open("m:fPr").children();
                    self.buf.open("m:type").attr("m:val", "skw").empty();
                    self.buf.close();
                }
                self.wrap_content("m:num", &frac.num);
                self.wrap_content("m:den", &frac.denom);
                self.buf.close();
            }
        }
    }

    fn emit_attach(&mut self, attach: &Packed<AttachElem>) {
        self.emitted = true;
        let styles = StyleChain::default();
        let upper = attach.t.get_cloned(styles).or_else(|| attach.tr.get_cloned(styles));
        let lower = attach.b.get_cloned(styles).or_else(|| attach.br.get_cloned(styles));
        let upper_left = attach.tl.get_cloned(styles);
        let lower_left = attach.bl.get_cloned(styles);

        let base = render_direct(&attach.base);
        let with_right = match (lower.as_ref(), upper.as_ref()) {
            (None, None) => base,
            (Some(sub), None) => script_raw("m:sSub", &base, Some(sub), None),
            (None, Some(sup)) => script_raw("m:sSup", &base, None, Some(sup)),
            (Some(sub), Some(sup)) => {
                script_raw("m:sSubSup", &base, Some(sub), Some(sup))
            }
        };

        if upper_left.is_none() && lower_left.is_none() {
            self.buf.raw(&with_right);
            return;
        }

        self.buf.open("m:sPre").children();
        match lower_left.as_ref() {
            Some(content) => self.wrap_content("m:sub", content),
            None => self.buf.open("m:sub").empty(),
        }
        match upper_left.as_ref() {
            Some(content) => self.wrap_content("m:sup", content),
            None => self.buf.open("m:sup").empty(),
        }
        self.buf.wrap_raw("m:e", &with_right);
        self.buf.close();
    }

    fn emit_root(&mut self, root: &Packed<RootElem>) {
        self.emitted = true;
        self.buf.open("m:rad").children();
        match root.index.get_cloned(StyleChain::default()) {
            Some(index) => self.wrap_content("m:deg", &index),
            None => {
                self.buf.open("m:radPr").children();
                self.buf.open("m:degHide").attr("m:val", "on").empty();
                self.buf.close();
                self.buf.open("m:deg").empty();
            }
        }
        self.wrap_content("m:e", &root.radicand);
        self.buf.close();
    }

    fn emit_fenced(&mut self, body: &Content) {
        let mut items = Vec::new();
        collect_math_row(body, &mut items);

        let first = items.first().and_then(|content| single_content_char(content));
        let last = items.last().and_then(|content| single_content_char(content));
        let has_pair = items.len() >= 2
            && first.is_some_and(is_open_delimiter)
            && last.is_some_and(is_close_delimiter);

        if !has_pair {
            self.emit_items(&items);
            return;
        }

        self.emitted = true;
        self.buf.open("m:d").children();
        self.buf.open("m:dPr").children();
        self.buf
            .open("m:begChr")
            .attr("m:val", &first.unwrap().to_string())
            .empty();
        self.buf
            .open("m:endChr")
            .attr("m:val", &last.unwrap().to_string())
            .empty();
        self.buf.close();
        self.buf.open("m:e").children();
        self.emit_items(&items[1..items.len() - 1]);
        self.buf.close();
        self.buf.close();
    }

    fn emit_binom(&mut self, binom: &Packed<BinomElem>) {
        self.emitted = true;
        self.buf.open("m:d").children();
        self.buf.open("m:dPr").children();
        self.buf.open("m:begChr").attr("m:val", "(").empty();
        self.buf.open("m:endChr").attr("m:val", ")").empty();
        self.buf.close();
        self.buf.open("m:e").children();
        self.buf.open("m:f").children();
        self.buf.open("m:fPr").children();
        self.buf.open("m:type").attr("m:val", "noBar").empty();
        self.buf.close();
        self.wrap_content("m:num", &binom.upper);
        self.buf.open("m:den").children();
        for (i, lower) in binom.lower.iter().enumerate() {
            if i > 0 {
                self.text_run(",", false);
            }
            self.emit_content(lower);
        }
        self.buf.close();
        self.buf.close();
        self.buf.close();
        self.buf.close();
    }

    fn emit_accent(&mut self, accent: &Packed<AccentElem>) {
        self.emitted = true;
        self.buf.open("m:acc").children();
        self.buf.open("m:accPr").children();
        self.buf
            .open("m:chr")
            .attr("m:val", &to_combining(accent.accent.0).to_string())
            .empty();
        self.buf.close();
        self.wrap_content("m:e", &accent.base);
        self.buf.close();
    }

    fn emit_bar(&mut self, body: &Content, position: Position) {
        self.emitted = true;
        let pos = match position {
            Position::Above => "top",
            Position::Below => "bot",
        };
        self.buf.open("m:bar").children();
        self.buf.open("m:barPr").children();
        self.buf.open("m:pos").attr("m:val", pos).empty();
        self.buf.close();
        self.wrap_content("m:e", body);
        self.buf.close();
    }

    fn emit_nary(
        &mut self,
        chr: char,
        lower: Option<&Content>,
        upper: Option<&Content>,
        integrand: &[&Content],
    ) {
        self.emitted = true;
        self.buf.open("m:nary").children();
        self.buf.open("m:naryPr").children();
        self.buf.open("m:chr").attr("m:val", &chr.to_string()).empty();
        self.buf
            .open("m:limLoc")
            .attr("m:val", if is_integral_char(chr) { "subSup" } else { "undOvr" })
            .empty();
        self.buf.open("m:grow").attr("m:val", "1").empty();
        if lower.is_none() {
            self.buf.open("m:subHide").attr("m:val", "on").empty();
        }
        if upper.is_none() {
            self.buf.open("m:supHide").attr("m:val", "on").empty();
        }
        self.buf.close();

        match lower {
            Some(content) => self.wrap_content("m:sub", content),
            None => self.buf.open("m:sub").empty(),
        }
        match upper {
            Some(content) => self.wrap_content("m:sup", content),
            None => self.buf.open("m:sup").empty(),
        }
        self.buf.open("m:e").children();
        self.emit_items(integrand);
        self.buf.close();
        self.buf.close();
    }

    fn wrap_content(&mut self, wrapper: &str, content: &Content) {
        let inner = render_direct(content);
        self.buf.wrap_raw(wrapper, &inner);
    }

    fn text_run(&mut self, text: &str, upright: bool) {
        if text.is_empty() {
            return;
        }
        self.emitted = true;
        self.buf.open("m:r").children();
        if upright {
            self.buf.open("m:rPr").children();
            self.buf.leaf("m:nor");
            self.buf.close();
        }
        self.buf.open("m:t");
        if text.starts_with(' ') || text.ends_with(' ') {
            self.buf.attr("xml:space", "preserve");
        }
        self.buf.children().text(text);
        self.buf.close();
        self.buf.close();
    }
}

fn render_direct(content: &Content) -> String {
    let mut emitter = DirectEmitter::new();
    emitter.emit_content(content);
    emitter.buf.into_string()
}

fn script_raw(
    tag: &str,
    base: &str,
    sub: Option<&Content>,
    sup: Option<&Content>,
) -> String {
    let mut w = Omml::new();
    w.open(tag).children();
    w.wrap_raw("m:e", base);
    if let Some(content) = sub {
        w.wrap_raw("m:sub", &render_direct(content));
    }
    if let Some(content) = sup {
        w.wrap_raw("m:sup", &render_direct(content));
    }
    w.close();
    w.into_string()
}

fn collect_math_row<'a>(content: &'a Content, out: &mut Vec<&'a Content>) {
    if let Some(sequence) = content.to_packed::<SequenceElem>() {
        for child in &sequence.children {
            collect_math_row(child, out);
        }
    } else if let Some(styled) = content.to_packed::<StyledElem>() {
        collect_math_row(&styled.child, out);
    } else {
        out.push(content);
    }
}

fn nary_attach_content(
    content: &Content,
) -> Option<(char, Option<Content>, Option<Content>)> {
    let attach = content.to_packed::<AttachElem>()?;
    let chr = nary_content_char(&attach.base)?;
    let styles = StyleChain::default();
    let lower = attach.b.get_cloned(styles).or_else(|| attach.br.get_cloned(styles));
    let upper = attach.t.get_cloned(styles).or_else(|| attach.tr.get_cloned(styles));
    Some((chr, lower, upper))
}

fn nary_content_char(content: &Content) -> Option<char> {
    let c = single_content_char(content)?;
    is_nary_operator(c).then_some(c)
}

fn single_content_char(content: &Content) -> Option<char> {
    let text = if let Some(symbol) = content.to_packed::<SymbolElem>() {
        symbol.text.as_str()
    } else if let Some(text) = content.to_packed::<TypstTextElem>() {
        text.text.as_str()
    } else if let Some(styled) = content.to_packed::<StyledElem>() {
        return single_content_char(&styled.child);
    } else if let Some(equation) = content.to_packed::<EquationElem>() {
        return single_content_char(&equation.body);
    } else {
        return None;
    };

    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn direct_operand_end(items: &[&Content], start: usize) -> usize {
    let mut j = start;
    while j < items.len() {
        if j > start
            && (nary_attach_content(items[j]).is_some()
                || nary_content_char(items[j]).is_some())
        {
            break;
        }
        if single_content_char(items[j]).is_some_and(is_relation_or_binary) {
            break;
        }
        j += 1;
    }
    j
}

fn is_relation_or_binary(c: char) -> bool {
    matches!(
        c,
        '=' | '<'
            | '>'
            | '≤'
            | '≥'
            | '≠'
            | '≈'
            | '≃'
            | '≅'
            | '≡'
            | '∼'
            | '∝'
            | '+'
            | '-'
            | '−'
            | '×'
            | '⋅'
            | '÷'
    )
}

fn is_open_delimiter(c: char) -> bool {
    matches!(c, '(' | '[' | '{' | '⟨' | '⌊' | '⌈' | '|')
}

fn is_close_delimiter(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '⟩' | '⌋' | '⌉' | '|')
}

//! The `math` mapper: OMML (`m:oMath`) → a fallback-text [`Inline::Math`].
//! Full OMML→Typst-math translation is out of scope for v1: this linearizes
//! the OMML into readable text (roughly what a screen reader would say), so
//! the equation's *content* survives at a fidelity cost that's recorded in
//! the [`ImportReport`].

use ecow::EcoString;
use typst_ooxml_core::omml::omml_fallback_text;

use crate::report::ImportReport;
use crate::tdoc::Inline;

pub fn omml_to_inline(fragment: &str, report: &mut ImportReport) -> Inline {
    let annotated = ensure_namespaces(fragment);
    match omml_fallback_text(&annotated) {
        Some(text) => {
            report.approximate("OMML equation", "emitted as fallback text; verify");
            Inline::Math(text)
        }
        None => {
            // No safe text to fall back to: never splice the raw (possibly
            // unparsable-as-math) XML fragment into `$..$` markup, since that
            // reliably produces invalid Typst source. Drop the equation
            // instead — recorded so it's auditable, not silently lost.
            report.drop("OMML equation", "could not extract equation text; dropped");
            Inline::Text(EcoString::new())
        }
    }
}

/// [`omml_fallback_text`] only auto-declares the `m:` namespace when it wraps
/// an un-namespaced fragment (checking for a literal `"xmlns:m="` substring).
/// Word's OMML runs sometimes carry a `w:rPr` sibling inside `m:r` (a
/// character-formatting override), which uses the `w:` prefix — undeclared in
/// that wrap, so the fragment fails to parse as XML at all and the whole
/// equation is lost. Declare both namespaces directly on the fragment's own
/// root element instead, so `omml_fallback_text` sees `xmlns:m=` already
/// present and parses the fragment as-is (as a single well-formed root).
fn ensure_namespaces(fragment: &str) -> String {
    // Word's captured `m:oMath` fragments typically already declare
    // `xmlns:m` themselves (unlike `omml_fallback_text`'s own synthetic
    // fragments, which rely on its wrap) — so the two declarations must be
    // checked independently; a fragment can have one without the other.
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

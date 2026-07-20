//! The `field` mapper: Word fields (`w:fldSimple` and the flattened
//! `w:fldChar` begin/separate/end run sequence, folded into one [`Field`] by
//! [`crate::wml::parse`]) → the Typst IR's [`Inlines`].
//!
//! Every field splits into two halves: the *instruction* (what to compute,
//! e.g. `PAGE` or `HYPERLINK "https://x"`) and the *cached result* — what
//! Word last rendered, i.e. what a reader actually sees without recomputing
//! fields. A handful of field types have an obvious idiomatic Typst
//! equivalent (page numbers, hyperlinks, a table of contents); everything
//! else falls back to its cached result, since that result is literally the
//! document's visible content and dropping it would be a much bigger
//! fidelity hit than leaving it non-live.

use ecow::EcoString;

use crate::lower::LowerCtx;
use crate::mappers::run::lower_run_items;
use crate::tdoc::{Inline, Inlines};
use crate::wml::model::Field;

pub(crate) fn lower_field(field: &Field, ctx: &mut LowerCtx) -> Inlines {
    let field_type = first_token_upper(&field.instr);

    match field_type.as_str() {
        // `#context` makes these reflow with the page they land on after
        // Typst's own pagination, which is both more idiomatic and more
        // correct than trusting Word's last-computed (and possibly stale)
        // cached number.
        "PAGE" => vec![Inline::Verbatim("#context counter(page).display()".into())],
        "NUMPAGES" => {
            vec![Inline::Verbatim("#context counter(page).final().first()".into())]
        }
        "HYPERLINK" => lower_hyperlink(&field_type, field, ctx),
        // The cached result is the *stale* rendered table of contents (page
        // numbers baked in from whenever Word last updated fields); a live
        // `#outline()` is both idiomatic and correct, so the result is
        // deliberately discarded here rather than lowered.
        "TOC" => vec![Inline::Verbatim("#outline()".into())],
        _ => lower_fallback(&field_type, field, ctx),
    }
}

/// `HYPERLINK "dest" \o "tooltip" ...` → `#link(dest)[body]`, with `body`
/// falling back to the destination text itself when the field has no cached
/// result (e.g. a document whose fields were never updated by Word). A
/// `HYPERLINK` instruction with no quoted destination (only switches, like a
/// bookmark-only `\l anchor`) has nothing to link to, so it falls through to
/// the same generic cached-result handling as any other unmapped field.
fn lower_hyperlink(field_type: &str, field: &Field, ctx: &mut LowerCtx) -> Inlines {
    let Some(dest) = first_quoted_arg(&field.instr) else {
        return lower_fallback(field_type, field, ctx);
    };
    let mut body = lower_run_items(&field.result, None, ctx);
    if body.is_empty() {
        body = vec![Inline::Text(dest.clone())];
    }
    vec![Inline::Link { dest, body }]
}

/// The load-bearing fallback for every field type without a structured
/// mapping: lower the cached result as ordinary inline content, and record
/// an [`crate::report::ImportReport::approximate`] note naming the field
/// type — unless the field is entirely blank (no instruction, no result),
/// which carries nothing worth reporting.
fn lower_fallback(field_type: &str, field: &Field, ctx: &mut LowerCtx) -> Inlines {
    let body = lower_run_items(&field.result, None, ctx);
    if !field_type.is_empty() {
        ctx.report.approximate(
            format!("field {field_type}"),
            "imported as its last-rendered text (not recomputed)",
        );
    }
    body
}

/// The first whitespace-separated token of a field instruction, uppercased —
/// e.g. `" PAGE "` → `"PAGE"`, `" hyperlink \"x\""` → `"HYPERLINK"`. Empty
/// for a blank/whitespace-only instruction.
fn first_token_upper(instr: &str) -> EcoString {
    instr.split_whitespace().next().unwrap_or_default().to_ascii_uppercase().into()
}

/// The first `"..."`-quoted argument in a field instruction, e.g. the URL in
/// ` HYPERLINK "https://x" \o "tooltip" `. A pragmatic scan (first quote to
/// next quote, no escape handling) — real-world `HYPERLINK` destinations
/// don't embed literal quotes.
fn first_quoted_arg(instr: &str) -> Option<EcoString> {
    let start = instr.find('"')?;
    let rest = &instr[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::ImportReport;
    use crate::wml::model::{RunItem, WmlPackage};

    fn text_result(text: &str) -> Vec<RunItem> {
        use crate::wml::model::{Run, RunContent, RunProps};
        vec![RunItem::Run(Run {
            props: RunProps::default(),
            content: vec![RunContent::Text(text.into())],
        })]
    }

    #[test]
    fn page_field_lowers_to_counter_verbatim() {
        let field = Field { instr: " PAGE ".into(), result: text_result("7") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(
            &inlines[..],
            [Inline::Verbatim(s)] if s == "#context counter(page).display()"
        ));
        // The result was discarded, not reported as approximate.
        assert!(report.notes.is_empty());
    }

    #[test]
    fn toc_field_discards_stale_result() {
        let field = Field { instr: " TOC \\o \"1-3\" \\h ".into(), result: text_result("stale") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(&inlines[..], [Inline::Verbatim(s)] if s == "#outline()"));
    }

    #[test]
    fn hyperlink_field_becomes_link_with_cached_body() {
        let field = Field {
            instr: " HYPERLINK \"https://example.com\" \\o \"tip\" ".into(),
            result: text_result("click me"),
        };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        match &inlines[..] {
            [Inline::Link { dest, body }] => {
                assert_eq!(dest.as_str(), "https://example.com");
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "click me"));
            }
            other => panic!("expected a link, got {other:?}"),
        }
        assert!(report.notes.is_empty());
    }

    #[test]
    fn hyperlink_field_falls_back_to_dest_text_when_result_empty() {
        let field =
            Field { instr: " HYPERLINK \"https://example.com\" ".into(), result: Vec::new() };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        match &inlines[..] {
            [Inline::Link { dest, body }] => {
                assert_eq!(dest.as_str(), "https://example.com");
                assert!(matches!(&body[..], [Inline::Text(t)] if t == "https://example.com"));
            }
            other => panic!("expected a link, got {other:?}"),
        }
    }

    #[test]
    fn hyperlink_field_without_quoted_dest_falls_through_to_fallback() {
        // No quoted argument at all (an unquoted `\l` bookmark switch) — the
        // instruction carries no destination `lower_hyperlink` can extract.
        let field = Field { instr: " HYPERLINK \\l _Toc1 ".into(), result: text_result("5") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(&inlines[..], [Inline::Text(t)] if t == "5"));
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "field HYPERLINK");
    }

    #[test]
    fn unmapped_field_falls_back_to_cached_result_and_reports_once() {
        let field = Field { instr: " FILENAME \\* MERGEFORMAT ".into(), result: text_result("report.docx") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(&inlines[..], [Inline::Text(t)] if t == "report.docx"));
        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].what, "field FILENAME");
        assert!(report.notes[0].detail.contains("not recomputed"));
    }

    #[test]
    fn duplicate_unmapped_fields_produce_one_note() {
        let field = Field { instr: " AUTHOR ".into(), result: text_result("Jane Doe") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        for _ in 0..200 {
            lower_field(&field, &mut ctx);
        }
        assert_eq!(report.notes.len(), 1);
    }

    #[test]
    fn blank_field_produces_no_inlines_and_no_note() {
        let field = Field { instr: "   ".into(), result: Vec::new() };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let mut ctx = LowerCtx::new(&package, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(inlines.is_empty());
        assert!(report.notes.is_empty());
    }
}

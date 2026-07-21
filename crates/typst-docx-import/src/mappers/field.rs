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
        //
        // …unless the field itself sits inside a heading (`= #outline()`,
        // seen in the wild): `#outline()` renders every heading's own
        // content to build its listing, so a TOC field inside one of those
        // headings would render itself again, forever ("maximum show rule
        // depth exceeded"). A TOC field is only meaningful at block level
        // anyway, so this falls back to the same cached-result text every
        // other unmapped field already uses instead.
        "TOC" if ctx.in_heading() => lower_fallback(&field_type, field, ctx),
        "TOC" => vec![Inline::Verbatim("#outline()".into())],
        // Cross-references become live again — a real jump and a recomputed
        // page number — rather than staying frozen at whatever Word last
        // rendered, provided the bookmark they name actually exists.
        "REF" => lower_reference(&field_type, field, ctx),
        "PAGEREF" => lower_page_reference(&field_type, field, ctx),
        _ => lower_fallback(&field_type, field, ctx),
    }
}

/// The bookmark a `REF`/`PAGEREF` names: the first instruction token after
/// the field type that isn't a `\switch`.
fn bookmark_argument(instr: &str) -> Option<EcoString> {
    let mut skip_switch_argument = false;
    for token in instr.split_whitespace().skip(1) {
        if let Some(switch) = token.strip_prefix('\\') {
            // The formatting switches take a following argument
            // (`\* MERGEFORMAT`); the flag switches (`\h`, `\p`, `\n`) don't,
            // so only the former may swallow the next token.
            skip_switch_argument = matches!(switch, "*" | "#" | "@");
            continue;
        }
        if skip_switch_argument {
            skip_switch_argument = false;
            continue;
        }
        return Some(token.trim_matches('"').into());
    }
    None
}

/// `REF bookmark` → `#link(<label>)[cached text]`.
///
/// The displayed text deliberately stays Word's cached result: `REF` shows the
/// *content* of the bookmarked range, and Typst has no equivalent for that
/// (`@label` renders a numbered reference, not the referenced text). So the
/// text is kept as-is and only the jump is made live — a strictly smaller loss
/// than the generic fallback, which keeps the text but no link at all.
fn lower_reference(field_type: &str, field: &Field, ctx: &mut LowerCtx) -> Inlines {
    let Some(label) =
        bookmark_argument(&field.instr).and_then(|name| ctx.package.bookmarks.get(&name).cloned())
    else {
        return lower_fallback(field_type, field, ctx);
    };
    let mut body = lower_run_items(&field.result, None, ctx);
    if body.is_empty() {
        body = vec![Inline::Text(label.clone())];
    }
    ctx.report.approximate(
        "field REF",
        "displayed text is Word's cached text; the jump itself is live",
    );
    vec![Inline::LabelLink { label, body }]
}

/// `PAGEREF bookmark` → `#context counter(page).at(<label>).first()`, which
/// recomputes the page number under Typst's own pagination instead of
/// repeating whatever Word last cached — the same reasoning as `PAGE`.
fn lower_page_reference(field_type: &str, field: &Field, ctx: &mut LowerCtx) -> Inlines {
    let Some(label) =
        bookmark_argument(&field.instr).and_then(|name| ctx.package.bookmarks.get(&name).cloned())
    else {
        return lower_fallback(field_type, field, ctx);
    };
    vec![Inline::PageRef(label)]
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

#[cfg(test)]
mod bookmark_argument_tests {
    use super::bookmark_argument;

    /// The bookmark is the first token after the field type that isn't a
    /// `\switch` — switches routinely precede it in real instructions.
    #[test]
    fn reads_the_bookmark_past_any_switches() {
        assert_eq!(bookmark_argument(" REF _Ref47 \\h ").as_deref(), Some("_Ref47"));
        assert_eq!(bookmark_argument(" PAGEREF \\h _Toc9 ").as_deref(), Some("_Toc9"));
        assert_eq!(bookmark_argument(r#" REF "_Ref47" "#).as_deref(), Some("_Ref47"));
        assert_eq!(bookmark_argument(" REF ").as_deref(), None);
        // `\*` takes an argument, so `MERGEFORMAT` is not the bookmark.
        assert_eq!(
            bookmark_argument(" REF \\* MERGEFORMAT _Ref9 ").as_deref(),
            Some("_Ref9")
        );
    }
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
    use crate::opts::ImportOptions;
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(
            &inlines[..],
            [Inline::Verbatim(s)] if s == "#context counter(page).display()"
        ));
        // The result was discarded, not reported as approximate.
        assert!(report.notes.is_empty());
    }

    /// `= #outline()` — a TOC field lowered from inside a heading's own
    /// content. `#outline()` renders every heading, including the one
    /// containing it, so a live one here recurses forever ("maximum show
    /// rule depth exceeded"). The field must fall back to its cached text
    /// instead, exactly like any other unmapped field.
    #[test]
    fn toc_field_inside_a_heading_falls_back_to_cached_text_instead_of_a_live_outline() {
        let field = Field { instr: " TOC \\o \"1-3\" \\h ".into(), result: text_result("stale") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        ctx.enter_heading();
        let inlines = lower_field(&field, &mut ctx);
        assert!(matches!(&inlines[..], [Inline::Text(t)] if t == "stale"));
        assert_eq!(ctx.report.notes.len(), 1);
        assert_eq!(ctx.report.notes[0].what, "field TOC");
    }

    #[test]
    fn toc_field_discards_stale_result() {
        let field = Field { instr: " TOC \\o \"1-3\" \\h ".into(), result: text_result("stale") };
        let package = WmlPackage::default();
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
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
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let inlines = lower_field(&field, &mut ctx);
        assert!(inlines.is_empty());
        assert!(report.notes.is_empty());
    }
}

//! The `section` mapper: Word `w:sectPr` → the Typst IR's [`tdoc::PageSetup`].

use ecow::EcoString;
use typst_ooxml_core::units::twip_to_abs;

use crate::lower::{lower_items, LowerCtx};
use crate::mappers::para::inlines_have_text;
use crate::tdoc::{Block, Furniture, Margins, PageSetup};
use crate::wml::model::{BodyItem, FurnitureKind, FurnitureRef, RunContent, RunItem, SectPr};

pub(crate) fn lower_section(sect: &SectPr, ctx: &mut LowerCtx) -> PageSetup {
    let margin = if sect.margin_top.is_some()
        || sect.margin_bottom.is_some()
        || sect.margin_left.is_some()
        || sect.margin_right.is_some()
    {
        Some(Margins {
            top_pt: twip(sect.margin_top),
            bottom_pt: twip(sect.margin_bottom),
            left_pt: twip(sect.margin_left),
            right_pt: twip(sect.margin_right),
        })
    } else {
        None
    };

    let header = lower_furniture(&sect.header_refs, sect.title_pg, ctx);
    let footer = lower_furniture(&sect.footer_refs, sect.title_pg, ctx);

    // Both fidelity gaps below are structural to this feature, not specific
    // to any one document, so they're recorded whenever furniture is emitted
    // at all rather than gated on some more specific (and here, unavailable)
    // signal — `ImportReport` dedupes, so this only ever costs one line.
    if header.is_some() || footer.is_some() {
        ctx.report.approximate(
            "header/footer margins",
            "w:pgMar's header/footer page-edge distance isn't mapped to Typst's page margins",
        );
        ctx.report.approximate(
            "header/footer sections",
            "only the document's final section's header/footer is honored; it is applied to every page",
        );
    }

    PageSetup {
        width_pt: sect.page_w.map(|w| twip_to_abs(w as f64).to_pt()),
        height_pt: sect.page_h.map(|h| twip_to_abs(h as f64).to_pt()),
        margin,
        flipped: sect.landscape,
        header,
        footer,
    }
}

fn twip(v: Option<i64>) -> f64 {
    v.map(|v| twip_to_abs(v as f64).to_pt()).unwrap_or(0.0)
}

/// Lower one furniture kind (header or footer) from its `w:sectPr`
/// references to the Typst IR. `title_pg` gates whether a `First` reference
/// is honored; `package.even_and_odd_headers` gates `Even` — Word ignores
/// both references otherwise, and so must this importer (see the module
/// docs' `headerFooter.docx` example, which declares all three types but
/// activates neither). Returns `None` if there's nothing to emit at all: no
/// default reference and no active variant, or every resolved variant turned
/// out visually empty.
fn lower_furniture(
    refs: &[FurnitureRef],
    title_pg: bool,
    ctx: &mut LowerCtx,
) -> Option<Furniture> {
    let package = ctx.package;
    let default = resolve_variant(refs, FurnitureKind::Default, ctx).unwrap_or_default();
    let first = title_pg.then(|| resolve_variant(refs, FurnitureKind::First, ctx)).flatten();
    let even = package
        .even_and_odd_headers
        .then(|| resolve_variant(refs, FurnitureKind::Even, ctx))
        .flatten();

    if default.is_empty() && first.is_none() && even.is_none() {
        return None;
    }
    Some(Furniture { default, first, even })
}

/// Resolve a single `(refs, kind)` reference to its lowered blocks — `None`
/// if there's no reference of this kind, the relationship/part can't be
/// resolved, or the referenced part's content is visually empty. Word itself
/// emits an empty placeholder header/footer routinely (a lone empty
/// paragraph — see `headerFooter.docx`'s "even"/"first" parts in the POI
/// corpus), and lowering that to `header: []` would only add noise.
fn resolve_variant(
    refs: &[FurnitureRef],
    kind: FurnitureKind,
    ctx: &mut LowerCtx,
) -> Option<Vec<Block>> {
    let package = ctx.package;
    let furniture_ref = refs.iter().find(|r| r.kind == kind)?;
    let Some(rel) = package.rels.get(&furniture_ref.rel_id) else {
        ctx.report.drop("header/footer", "relationship target not found");
        return None;
    };
    let key = furniture_key(&rel.target);
    let Some(body) = package.furniture.get(&key) else {
        ctx.report.drop("header/footer", "referenced part not found in package");
        return None;
    };

    if uses_tab_stops(&body.items) {
        ctx.report.approximate(
            "header/footer tab stops",
            "w:tab/w:ptab columns become plain spaced text, not a multi-column layout",
        );
    }

    // A page/column break inside a header/footer body is just as meaningless
    // as inside a table cell (Typst rejects it outright) — see
    // `mappers::table::lower_cell`'s equivalent guard.
    let was_in_container = ctx.enter_container();
    let blocks = lower_items(&body.items, ctx);
    ctx.exit_container(was_in_container);
    (!is_visually_empty(&blocks)).then_some(blocks)
}

/// Resolve a `w:headerReference`/`w:footerReference`'s relationship target
/// (relative to `word/`, e.g. `header1.xml`) to the zip name used as
/// [`WmlPackage::furniture`]'s key. Mirrors how a document-level drawing's
/// relationship target becomes a media part name ([`crate::mappers::drawing`])
/// — `document.xml` and `headerN.xml` are both direct children of `word/`, so
/// the same relative resolution applies — but is also robust to a producer
/// that emits an absolute `/word/...` target (`Bug60341.docx` in the POI
/// corpus does exactly this for its footer).
fn furniture_key(target: &str) -> EcoString {
    let trimmed = target.trim_start_matches("./");
    if let Some(rest) = trimmed.strip_prefix('/') {
        return if rest.starts_with("word/") {
            rest.into()
        } else {
            format!("word/{rest}").into()
        };
    }
    if trimmed.starts_with("word/") {
        trimmed.into()
    } else {
        format!("word/{trimmed}").into()
    }
}

/// A furniture body is "visually empty" if it has no visible text and no
/// figure/table — see [`resolve_variant`] for why that's dropped rather than
/// emitted as `header: []`.
fn is_visually_empty(blocks: &[Block]) -> bool {
    blocks.iter().all(|block| match block {
        Block::Paragraph { body, .. } | Block::Heading { body, .. } => !inlines_have_text(body),
        Block::List(_)
        | Block::Table(_)
        | Block::Chart(_)
        | Block::Figure(_)
        | Block::CodeBlock { .. }
        | Block::Equation { .. } => false,
        Block::Rule | Block::Break(_) => true,
        Block::Verbatim(s) => s.trim().is_empty(),
    })
}

/// Whether any run in this furniture body carries a `w:tab`/`w:ptab` — the
/// "left⇥centre⇥right" idiom several corpus documents (`ThreeColHeadFoot.docx`)
/// use to lay out a three-column header/footer. Typst has no positional-tab
/// primitive, so [`resolve_variant`] reports this as an approximation when
/// detected: the tabs survive as spaced text, not a multi-column layout.
fn uses_tab_stops(items: &[BodyItem]) -> bool {
    items.iter().any(|item| match item {
        BodyItem::Paragraph(p) => p.runs.iter().any(run_item_has_tab),
        BodyItem::Table(t) => {
            t.rows.iter().any(|r| r.cells.iter().any(|c| uses_tab_stops(&c.content)))
        }
    })
}

fn run_item_has_tab(item: &RunItem) -> bool {
    match item {
        RunItem::Run(r) => r.content.iter().any(|c| matches!(c, RunContent::Tab)),
        RunItem::Hyperlink { runs, .. } => runs.iter().any(run_item_has_tab),
        RunItem::Field(f) => f.result.iter().any(run_item_has_tab),
    }
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::opts::ImportOptions;
    use crate::report::ImportReport;
    use crate::wml::model::{Body, Paragraph, Relationship, Run, RunProps, WmlPackage};

    fn text_paragraph(text: &str) -> BodyItem {
        BodyItem::Paragraph(Paragraph {
            props: Default::default(),
            runs: vec![RunItem::Run(Run {
                props: RunProps::default(),
                content: vec![RunContent::Text(text.into())],
            })],
        })
    }

    fn empty_paragraph() -> BodyItem {
        BodyItem::Paragraph(Paragraph::default())
    }

    fn default_ref() -> FurnitureRef {
        FurnitureRef { kind: FurnitureKind::Default, rel_id: "rId1".into() }
    }

    /// A minimal package with one furniture part (`word/header1.xml`,
    /// reachable via `rId1`) holding `items`.
    fn package_with_furniture(items: Vec<BodyItem>, even_and_odd_headers: bool) -> WmlPackage {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: "header1.xml".into(), external: false },
        );
        let mut furniture = FxHashMap::default();
        furniture.insert("word/header1.xml".into(), Body { items, sect_pr: None });
        WmlPackage { rels, furniture, even_and_odd_headers, ..Default::default() }
    }

    #[test]
    fn default_only_reference_produces_furniture_with_no_variants() {
        let package = package_with_furniture(vec![text_paragraph("Header text")], false);
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let furniture =
            lower_furniture(&[default_ref()], false, &mut ctx).expect("expected furniture");
        assert_eq!(furniture.default.len(), 1);
        assert!(furniture.first.is_none());
        assert!(furniture.even.is_none());
    }

    #[test]
    fn no_default_and_no_active_variant_returns_none() {
        let package = package_with_furniture(vec![text_paragraph("Header text")], false);
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        // Only a `First` reference exists, and `title_pg` is false — inactive.
        let refs = vec![FurnitureRef { kind: FurnitureKind::First, rel_id: "rId1".into() }];
        let furniture = lower_furniture(&refs, false, &mut ctx);
        assert!(furniture.is_none());
    }

    #[test]
    fn title_pg_gates_whether_the_first_variant_is_honored() {
        let mut rels = FxHashMap::default();
        rels.insert(
            "rId1".into(),
            Relationship { target: "header1.xml".into(), external: false },
        );
        rels.insert(
            "rId2".into(),
            Relationship { target: "header2.xml".into(), external: false },
        );
        let mut furniture = FxHashMap::default();
        furniture.insert(
            "word/header1.xml".into(),
            Body { items: vec![text_paragraph("Default")], sect_pr: None },
        );
        furniture.insert(
            "word/header2.xml".into(),
            Body { items: vec![text_paragraph("First page")], sect_pr: None },
        );
        let package = WmlPackage { rels, furniture, ..Default::default() };

        let refs =
            vec![default_ref(), FurnitureRef { kind: FurnitureKind::First, rel_id: "rId2".into() }];
        let mut report = ImportReport::default();
        let options = ImportOptions::default();

        // Without `title_pg`, the `First` reference exists but is ignored.
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let without = lower_furniture(&refs, false, &mut ctx).unwrap();
        assert!(without.first.is_none());

        // With `title_pg`, it's honored.
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let with = lower_furniture(&refs, true, &mut ctx).unwrap();
        assert!(with.first.is_some());
    }

    #[test]
    fn even_and_odd_headers_setting_gates_whether_the_even_variant_is_honored() {
        fn furniture_map() -> FxHashMap<EcoString, Body> {
            let mut furniture = FxHashMap::default();
            furniture.insert(
                "word/header1.xml".into(),
                Body { items: vec![text_paragraph("Default")], sect_pr: None },
            );
            furniture.insert(
                "word/header2.xml".into(),
                Body { items: vec![text_paragraph("Even page")], sect_pr: None },
            );
            furniture
        }
        fn rels_map() -> FxHashMap<EcoString, Relationship> {
            let mut rels = FxHashMap::default();
            rels.insert(
                "rId1".into(),
                Relationship { target: "header1.xml".into(), external: false },
            );
            rels.insert(
                "rId2".into(),
                Relationship { target: "header2.xml".into(), external: false },
            );
            rels
        }

        let refs =
            vec![default_ref(), FurnitureRef { kind: FurnitureKind::Even, rel_id: "rId2".into() }];
        let mut report = ImportReport::default();
        let options = ImportOptions::default();

        let without = WmlPackage {
            rels: rels_map(),
            furniture: furniture_map(),
            even_and_odd_headers: false,
            ..Default::default()
        };
        let mut ctx = LowerCtx::new(&without, &options, &mut report);
        let result = lower_furniture(&refs, false, &mut ctx).unwrap();
        assert!(result.even.is_none());

        let with = WmlPackage {
            rels: rels_map(),
            furniture: furniture_map(),
            even_and_odd_headers: true,
            ..Default::default()
        };
        let mut ctx = LowerCtx::new(&with, &options, &mut report);
        let result = lower_furniture(&refs, false, &mut ctx).unwrap();
        assert!(result.even.is_some());
    }

    #[test]
    fn visually_empty_variant_is_dropped_entirely() {
        // A lone empty paragraph — Word's own placeholder shape (see
        // `headerFooter.docx` in the POI corpus).
        let package = package_with_furniture(vec![empty_paragraph()], false);
        let mut report = ImportReport::default();
        let options = ImportOptions::default();
        let mut ctx = LowerCtx::new(&package, &options, &mut report);
        let furniture = lower_furniture(&[default_ref()], false, &mut ctx);
        assert!(furniture.is_none());
    }

    #[test]
    fn furniture_key_resolves_relative_and_absolute_targets() {
        assert_eq!(furniture_key("header1.xml"), "word/header1.xml");
        assert_eq!(furniture_key("./header1.xml"), "word/header1.xml");
        assert_eq!(furniture_key("/word/footer.xml"), "word/footer.xml");
        assert_eq!(furniture_key("word/header1.xml"), "word/header1.xml");
    }
}

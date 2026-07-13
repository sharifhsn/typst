use comemo::Track;
use ecow::{EcoVec, eco_format};
use smallvec::smallvec;
use typst_library::diag::{At, SourceResult, bail};
use typst_library::foundations::{
    Content, Context, NativeElement, NativeRuleMap, Packed, Resolve, ShowFn, Smart,
    StyleChain, Synthesize, Target, TargetElem, dict,
};
use typst_library::introspection::{Counter, Locator, LocatorLink};
use typst_library::layout::{
    Abs, AlignElem, Alignment, Axes, BlockBody, BlockElem, ColumnsElem, Em,
    FixedAlignment, GridCell, GridChild, GridElem, GridItem, HAlignment, HElem, HideElem,
    InlineElem, LayoutElem, Length, MoveElem, OuterVAlignment, PadElem, PageElem,
    PlaceElem, PlacementScope, Region, Rel, RepeatElem, RotateElem, ScaleElem, Sides,
    Size, Sizing, SkewElem, Spacing, StackChild, StackElem, TrackSizings, VElem,
};
use typst_library::math::EquationElem;
use typst_library::model::{
    Attribution, BibliographyElem, CiteElem, CiteGroup, CslIndentElem, CslLightElem,
    Destination, DirectLinkElem, DividerElem, EmphElem, EnumElem, FigureCaption,
    FigureElem, FootnoteElem, FootnoteEntry, HeadingElem, LinkElem, LinkMarker, ListElem,
    OutlineElem, OutlineEntry, ParElem, ParbreakElem, QuoteElem, RefElem, StrongElem,
    TableCell, TableElem, TermsElem, TitleElem, Works,
};
use typst_library::pdf::{ArtifactElem, ArtifactKind, AttachElem, PdfMarkerTag};
use typst_library::text::{
    DecoLine, Decoration, HighlightElem, ItalicToggle, LinebreakElem, LocalName,
    OverlineElem, RawElem, RawLine, ScriptKind, ShiftSettings, Smallcaps, SmallcapsElem,
    SmartQuoteElem, SmartQuotes, SpaceElem, StrikeElem, SubElem, SuperElem, TextElem,
    TextSize, UnderlineElem, WeightDelta,
};
use typst_library::visualize::{
    CircleElem, CurveElem, EllipseElem, ImageElem, LineElem, PolygonElem, RectElem,
    SquareElem, Stroke,
};
use typst_utils::{Get, Numeric};

/// Register show rules for the [paged target](Target::Paged).
pub fn register(rules: &mut NativeRuleMap) {
    use Target::{Docx, Paged, Pandoc};

    // Model.
    rules.register(Paged, STRONG_RULE);
    rules.register(Paged, EMPH_RULE);
    rules.register(Paged, LIST_RULE);
    rules.register(Paged, ENUM_RULE);
    rules.register(Paged, TERMS_RULE);
    rules.register(Paged, LINK_MARKER_RULE);
    rules.register(Paged, LINK_RULE);
    rules.register(Paged, DIRECT_LINK_RULE);
    rules.register(Paged, DIVIDER_RULE);
    rules.register(Paged, TITLE_RULE);
    rules.register(Paged, HEADING_RULE);
    rules.register(Paged, FIGURE_RULE);
    rules.register(Paged, FIGURE_CAPTION_RULE);
    rules.register(Paged, QUOTE_RULE);
    rules.register(Paged, FOOTNOTE_RULE);
    rules.register(Paged, FOOTNOTE_ENTRY_RULE);
    rules.register(Paged, OUTLINE_RULE);
    rules.register(Paged, OUTLINE_ENTRY_RULE);
    rules.register(Paged, REF_RULE);
    rules.register(Paged, CITE_GROUP_RULE);
    rules.register(Paged, BIBLIOGRAPHY_RULE);
    rules.register(Paged, CSL_LIGHT_RULE);
    rules.register(Paged, CSL_INDENT_RULE);
    rules.register(Paged, TABLE_RULE);
    rules.register(Paged, TABLE_CELL_RULE);

    // Text.
    rules.register(Paged, SUB_RULE);
    rules.register(Paged, SUPER_RULE);
    rules.register(Paged, UNDERLINE_RULE);
    rules.register(Paged, OVERLINE_RULE);
    rules.register(Paged, STRIKE_RULE);
    rules.register(Paged, HIGHLIGHT_RULE);
    rules.register(Paged, SMALLCAPS_RULE);
    rules.register(Paged, RAW_RULE);
    rules.register(Paged, RAW_LINE_RULE);

    // Inline formatting normalizes into text styles (`delta`, `emph`,
    // `shift_settings`, `deco`, `smallcaps`), which the DOCX backend reads back
    // when building runs. Registering the same rules for the DOCX target keeps
    // this content inline during realization (so paragraphs aren't split and the
    // surrounding spaces survive) instead of leaving raw `StrongElem`s etc. that
    // would interrupt paragraph grouping.
    rules.register(Docx, STRONG_RULE);
    rules.register(Docx, EMPH_RULE);
    rules.register(Docx, SUB_RULE);
    rules.register(Docx, SUPER_RULE);
    rules.register(Docx, UNDERLINE_RULE);
    rules.register(Docx, OVERLINE_RULE);
    rules.register(Docx, STRIKE_RULE);
    rules.register(Docx, HIGHLIGHT_RULE);
    rules.register(Docx, SMALLCAPS_RULE);

    // Raw/code blocks normalize (via their target-independent show-set) into a
    // `BlockElem` of highlighted monospace `TextElem` runs separated by
    // `LinebreakElem`s — which the DOCX backend lowers to monospace `w:r` runs
    // (the mono font + per-token colors flow through `resolve_text_props`).
    // Without these, a `RawElem` survives realization and falls through to the
    // rasterization fallback (G2).
    rules.register(Docx, RAW_RULE);
    rules.register(Docx, RAW_LINE_RULE);

    // `#align(..)[body]` normalizes into a `set align` on the body so the DOCX
    // backend can read the alignment off the style chain (→ `w:jc`) instead of
    // seeing a raw `AlignElem` that would interrupt paragraph grouping.
    rules.register(Docx, ALIGN_RULE);

    // `@key` references are turned into citations (or cross-reference links) by
    // the ref rule. Without it, a citation stays a raw `RefElem`, no `CiteGroup`
    // is formed, and `Works` can never locate the citation. (Cross-references
    // then lower to linked text rather than a `REF` field, which is fine — the
    // `DirectLinkElem` they produce still becomes a navigable hyperlink.)
    rules.register(Docx, REF_RULE);
    rules.register(Docx, LINK_MARKER_RULE);
    rules.register(Docx, DIRECT_LINK_RULE);

    // Citations and bibliographies resolve through citeproc in these rules
    // (building the `Works` that citations look up). Without them, citations
    // cannot be located. The DOCX backend lowers the resulting formatted
    // content like any other text/blocks.
    rules.register(Docx, CITE_GROUP_RULE);
    rules.register(Docx, BIBLIOGRAPHY_RULE);
    rules.register(Docx, CSL_LIGHT_RULE);
    rules.register(Docx, CSL_INDENT_RULE);

    // The Pandoc target mirrors the Docx target's reuse of the inline-formatting
    // normalization rules: they fold `StrongElem`/`EmphElem`/`SubElem`/… into
    // `TextElem` style flags (`delta`, `emph`, `shift_settings`, `deco`,
    // `smallcaps`), which keeps inline formatting *inline* during realization (so
    // paragraphs aren't split and the surrounding spaces survive) instead of
    // leaving raw formatting elements that would interrupt paragraph grouping.
    // The Pandoc backend reads these flags back off the style chain and wraps the
    // run in the matching Pandoc inline node (`Strong`/`Emph`/…). Without these,
    // the spaces around `*bold*`/`_emph_` are trimmed (paragraph-boundary
    // collapse) — the canonical inter-word-space defect.
    rules.register(Pandoc, STRONG_RULE);
    rules.register(Pandoc, EMPH_RULE);
    rules.register(Pandoc, SUB_RULE);
    rules.register(Pandoc, SUPER_RULE);
    rules.register(Pandoc, UNDERLINE_RULE);
    rules.register(Pandoc, OVERLINE_RULE);
    rules.register(Pandoc, STRIKE_RULE);
    rules.register(Pandoc, HIGHLIGHT_RULE);
    rules.register(Pandoc, SMALLCAPS_RULE);

    // `#align(..)[body]` normalizes into a `set align` on the body so it does not
    // interrupt paragraph grouping (the Pandoc backend currently ignores the
    // resulting alignment — Pandoc has no per-block alignment node — but keeping
    // the content inline preserves spaces and paragraph structure).
    rules.register(Pandoc, ALIGN_RULE);

    // `@key` references → citations / cross-reference links; citations and
    // bibliographies resolve through citeproc here (building the `Works` that
    // citations look up). Without them a citation stays a raw `RefElem`, no
    // `CiteGroup` forms, and the bibliography can never locate its citations
    // (a convergence deadlock). The Pandoc backend lowers the resulting formatted
    // content like any other inline text.
    rules.register(Pandoc, REF_RULE);
    rules.register(Pandoc, LINK_MARKER_RULE);
    rules.register(Pandoc, DIRECT_LINK_RULE);
    rules.register(Pandoc, CITE_GROUP_RULE);
    rules.register(Pandoc, BIBLIOGRAPHY_RULE);
    rules.register(Pandoc, CSL_LIGHT_RULE);
    rules.register(Pandoc, CSL_INDENT_RULE);

    // Layout.
    rules.register(Paged, ALIGN_RULE);
    rules.register(Paged, PAD_RULE);
    rules.register(Paged, COLUMNS_RULE);
    rules.register(Paged, STACK_RULE);
    rules.register(Paged, GRID_RULE);
    rules.register(Paged, GRID_CELL_RULE);
    rules.register(Paged, MOVE_RULE);
    rules.register(Paged, SCALE_RULE);
    rules.register(Paged, ROTATE_RULE);
    rules.register(Paged, SKEW_RULE);
    rules.register(Paged, REPEAT_RULE);
    rules.register(Paged, HIDE_RULE);
    rules.register(Paged, LAYOUT_RULE);

    // Visualize.
    rules.register(Paged, IMAGE_RULE);
    rules.register(Paged, LINE_RULE);
    rules.register(Paged, RECT_RULE);
    rules.register(Paged, SQUARE_RULE);
    rules.register(Paged, ELLIPSE_RULE);
    rules.register(Paged, CIRCLE_RULE);
    rules.register(Paged, POLYGON_RULE);
    rules.register(Paged, CURVE_RULE);

    // Math.
    rules.register(Paged, EQUATION_RULE);

    // PDF.
    rules.register(Paged, ATTACH_RULE);
    rules.register(Paged, ARTIFACT_RULE);
    rules.register(Paged, PDF_MARKER_TAG_RULE);
}

const STRONG_RULE: ShowFn<StrongElem> = |elem, _, styles| {
    Ok(elem
        .body
        .clone()
        .set(TextElem::delta, WeightDelta(elem.delta.get(styles))))
};

const EMPH_RULE: ShowFn<EmphElem> =
    |elem, _, _| Ok(elem.body.clone().set(TextElem::emph, ItalicToggle(true)));

const LIST_RULE: ShowFn<ListElem> = |elem, _, styles| {
    let tight = elem.tight.get(styles);

    let mut realized = BlockElem::multi_layouter(elem.clone(), crate::lists::layout_list)
        .pack()
        .spanned(elem.span());

    if tight {
        let spacing = elem
            .spacing
            .get(styles)
            .unwrap_or_else(|| styles.get(ParElem::leading));
        let v = VElem::new(spacing.into()).with_weak(true).with_attach(true).pack();
        realized = v + realized;
    }

    Ok(realized)
};

const ENUM_RULE: ShowFn<EnumElem> = |elem, _, styles| {
    let tight = elem.tight.get(styles);

    let mut realized = BlockElem::multi_layouter(elem.clone(), crate::lists::layout_enum)
        .pack()
        .spanned(elem.span());

    if tight {
        let spacing = elem
            .spacing
            .get(styles)
            .unwrap_or_else(|| styles.get(ParElem::leading));
        let v = VElem::new(spacing.into()).with_weak(true).with_attach(true).pack();
        realized = v + realized;
    }

    Ok(realized)
};

const TERMS_RULE: ShowFn<TermsElem> = |elem, _, styles| {
    let span = elem.span();
    let tight = elem.tight.get(styles);

    let separator = elem.separator.get_ref(styles);
    let indent = elem.indent.get(styles);
    let hanging_indent = elem.hanging_indent.get(styles);
    let gutter = elem.spacing.get(styles).unwrap_or_else(|| {
        if tight { styles.get(ParElem::leading) } else { styles.get(ParElem::spacing) }
    });

    let pad = hanging_indent + indent;
    let unpad = (!hanging_indent.is_zero())
        .then(|| HElem::new((-hanging_indent).into()).pack().spanned(span));

    let mut children = vec![];
    for child in elem.children.iter() {
        let mut seq = vec![];
        seq.extend(unpad.clone());
        seq.push(PdfMarkerTag::TermsItemLabel(child.term.clone().strong()));
        seq.push(separator.clone().artifact(ArtifactKind::Other));
        seq.push(child.description.clone());

        // Text in wide term lists shall always turn into paragraphs.
        if !tight {
            seq.push(ParbreakElem::shared().clone());
        }

        let item = Content::sequence(seq).spanned(child.span());
        children.push(StackChild::Block(PdfMarkerTag::TermsItemBody(item)));
    }

    let padding =
        Sides::default().with(styles.resolve(TextElem::dir).start(), pad.into());

    let mut realized = StackElem::new(children)
        .with_spacing(Some(gutter.into()))
        .pack()
        .spanned(span)
        .padded(padding)
        .set(TermsElem::within, true);

    if tight {
        let spacing = elem
            .spacing
            .get(styles)
            .unwrap_or_else(|| styles.get(ParElem::leading));
        let v = VElem::new(spacing.into())
            .with_weak(true)
            .with_attach(true)
            .pack()
            .spanned(span);
        realized = v + realized;
    }

    Ok(realized)
};

const LINK_MARKER_RULE: ShowFn<LinkMarker> = |elem, _, _| Ok(elem.body.clone());

const LINK_RULE: ShowFn<LinkElem> = |elem, engine, styles| {
    let span = elem.span();
    let body = elem.body.clone();
    let dest = elem.dest.resolve_early(engine, span)?;
    let alt = dest.alt_text(engine, styles, span)?;
    // Manually construct link marker that spans the whole link elem, not just
    // the body.
    Ok(LinkMarker::new(body, Some(alt))
        .pack()
        .spanned(span)
        .set(LinkElem::current, Some(dest)))
};

const DIRECT_LINK_RULE: ShowFn<DirectLinkElem> = |elem, _, _| {
    let dest = Destination::Location(elem.loc);
    Ok(elem.body.clone().linked(dest, elem.alt.clone()))
};

const DIVIDER_RULE: ShowFn<DividerElem> =
    |elem, _, _| Ok(LineElem::new().pack().spanned(elem.span()));

const TITLE_RULE: ShowFn<TitleElem> =
    |elem, _, styles| Ok(BlockElem::packed(elem.resolve_body(styles).at(elem.span())?));

const HEADING_RULE: ShowFn<HeadingElem> = |elem, engine, styles| {
    const SPACING_TO_NUMBERING: Em = Em::new(0.3);

    let span = elem.span();
    let mut realized = elem.body.clone();

    let hanging_indent = elem.hanging_indent.get(styles);
    let mut indent = match hanging_indent {
        Smart::Custom(length) => length.resolve(styles),
        Smart::Auto => Abs::zero(),
    };

    if let Some(numbering) = elem.numbering.get_ref(styles).as_ref() {
        let location = elem.location().unwrap();
        let numbering = Counter::of(HeadingElem::ELEM)
            .display_at(engine, location, styles, numbering, span)?
            .spanned(span);
        let align = styles.resolve(AlignElem::alignment);

        if hanging_indent.is_auto() && align.x == FixedAlignment::Start {
            let pod = Region::new(Axes::splat(Abs::inf()), Axes::splat(false));

            // We don't have a locator for the numbering here, so we just
            // use the measurement infrastructure for now.
            let link = LocatorLink::measure(location, span);
            let size = (engine.library.routines.layout_frame)(
                engine,
                &numbering,
                Locator::link(&link),
                styles,
                pod,
            )?
            .size();

            indent = size.x + SPACING_TO_NUMBERING.resolve(styles);
        }

        let spacing = HElem::new(SPACING_TO_NUMBERING.into()).with_weak(true).pack();

        realized = numbering + spacing + realized;
    }

    Ok(if indent != Abs::zero() {
        let body = HElem::new((-indent).into()).pack() + realized;
        let inset = Sides::default()
            .with(styles.resolve(TextElem::dir).start(), Some(indent.into()));
        BlockElem::new()
            .with_inset(inset)
            .with_body(Some(BlockBody::Content(body)))
            .pack()
    } else {
        BlockElem::packed(realized)
    })
};

const FIGURE_RULE: ShowFn<FigureElem> = |elem, _, styles| {
    let span = elem.span();
    let mut realized = elem.body.clone();

    // Build the caption, if any.
    if let Some(caption) = elem.caption.get_cloned(styles) {
        let (first, second) = match caption.position.get(styles) {
            OuterVAlignment::Top => (caption.pack(), realized),
            OuterVAlignment::Bottom => (realized, caption.pack()),
        };
        realized = Content::sequence(vec![
            first,
            VElem::new(elem.gap.get(styles).into())
                .with_weak(true)
                .pack()
                .spanned(span),
            second,
        ]);
    }

    // Ensure that the body is considered a paragraph.
    realized += ParbreakElem::shared().clone().spanned(span);

    // Wrap the contents in a block.
    realized = BlockElem::packed(realized).spanned(span);

    // Wrap in a float.
    if let Some(align) = elem.placement.get(styles) {
        realized = PlaceElem::new(realized)
            .with_alignment(align.map(|align| HAlignment::Center + align))
            .with_scope(elem.scope.get(styles))
            .with_float(true)
            .pack()
            .spanned(span);
    } else if elem.scope.get(styles) == PlacementScope::Parent {
        bail!(
            span,
            "parent-scoped placement is only available for floating figures";
            hint: "you can enable floating placement with `figure(placement: auto, ..)`";
        );
    }

    Ok(realized)
};

const FIGURE_CAPTION_RULE: ShowFn<FigureCaption> =
    |elem, engine, styles| Ok(BlockElem::packed(elem.realize(engine, styles)?));

const QUOTE_RULE: ShowFn<QuoteElem> = |elem, _, styles| {
    let span = elem.span();
    let block = elem.block.get(styles);

    let mut realized = elem.body.clone();

    if elem.quotes.get(styles).unwrap_or(!block) {
        // Add zero-width weak spacing to make the quotes "sticky".
        let hole = HElem::hole();
        let sticky = Content::sequence([hole.clone(), realized, hole.clone()]);
        realized = QuoteElem::quoted(sticky, styles);
    }

    let attribution = elem.attribution.get_ref(styles);

    if block {
        realized = BlockElem::packed(realized).spanned(span);

        if let Some(attribution) = attribution.as_ref() {
            // Bring the attribution a bit closer to the quote.
            let gap = Spacing::Rel(Em::new(0.9).into());
            let v = VElem::new(gap).with_weak(true).pack();
            realized += v;
            realized +=
                BlockElem::packed(attribution.realize(span)).aligned(Alignment::END);
        }

        realized = PadElem::new(realized).pack();
    } else if let Some(Attribution::Label(label)) = attribution {
        realized += SpaceElem::shared().clone();
        realized += CiteElem::new(*label).pack().spanned(span);
    }

    Ok(realized)
};

const FOOTNOTE_RULE: ShowFn<FootnoteElem> = |elem, engine, styles| {
    // The footnote number that links to the footnote entry.
    let link = elem.realize(engine, styles)?;
    let sup = SuperElem::new(link).pack().spanned(elem.span());
    Ok(HElem::hole().clone() + PdfMarkerTag::Label(sup))
};

const FOOTNOTE_ENTRY_RULE: ShowFn<FootnoteEntry> = |elem, engine, styles| {
    let number_gap = Em::new(0.05);
    let (sup, body) = elem.realize(engine, styles)?;
    let prefix = PdfMarkerTag::Label(sup);
    Ok(Content::sequence([
        HElem::new(elem.indent.get(styles).into()).pack(),
        prefix,
        HElem::new(number_gap.into()).with_weak(true).pack(),
        body,
    ]))
};

const OUTLINE_RULE: ShowFn<OutlineElem> = |elem, engine, styles| {
    let title = elem.realize_title(styles);
    let entries = elem.realize_flat(engine, styles)?;
    let entries = entries.into_iter().map(|entry| entry.pack());
    let body = PdfMarkerTag::OutlineBody(Content::sequence(entries));
    Ok(Content::sequence(title.into_iter().chain(Some(body))))
};

const OUTLINE_ENTRY_RULE: ShowFn<OutlineEntry> = |elem, engine, styles| {
    let span = elem.span();
    let context = Context::new(None, Some(styles));
    let context = context.track();

    let prefix = elem.prefix(engine, context, span)?;
    let body = elem.body().at(span)?;
    let page = elem.page(engine, context, span)?;
    let alt = {
        let prefix = prefix.as_ref().map(|p| p.plain_text()).unwrap_or_default();
        let body = body.plain_text();
        let page_str = PageElem::local_name_in(styles);
        let page_nr = page.plain_text();
        let quotes = SmartQuotes::get(
            styles.get_ref(SmartQuoteElem::quotes),
            styles.get(TextElem::lang),
            styles.get(TextElem::region),
            styles.get(SmartQuoteElem::alternative),
        );
        let open = quotes.double_open;
        let close = quotes.double_close;
        eco_format!("{prefix} {open}{body}{close} {page_str} {page_nr}",)
    };
    let inner = elem.build_inner(context, span, body, page)?;
    let block = if elem.element.is::<EquationElem>() {
        // Equation has no body and no levels, so indenting makes no sense.
        let body = prefix.unwrap_or_default() + inner;
        BlockElem::packed(body).spanned(span)
    } else {
        elem.indented(engine, context, span, prefix, inner, Em::new(0.5).into())?
    };

    let loc = elem.element_location().at(span)?;
    Ok(block.linked(Destination::Location(loc), Some(alt)))
};

const REF_RULE: ShowFn<RefElem> = |elem, engine, styles| elem.realize(engine, styles);

const CITE_GROUP_RULE: ShowFn<CiteGroup> = |elem, engine, _| elem.realize(engine);

const BIBLIOGRAPHY_RULE: ShowFn<BibliographyElem> = |elem, engine, styles| {
    const COLUMN_GUTTER: Em = Em::new(0.65);
    const INDENT: Em = Em::new(1.5);

    let loc = elem.location().unwrap();
    let span = elem.span();

    let mut seq = vec![];
    seq.extend(elem.realize_title(styles));

    let works = Works::generate(engine, elem.span())?;
    let bibliography = works.bibliography(loc, span)?;

    // The Pandoc target has no native two-column grid node and rasterizes any
    // grid wholesale — which would turn the reference list into one opaque image
    // and, fatally, drop the per-entry backlink anchors that in-text citations
    // resolve to (`ref-<location>`), leaving every cite Link dangling. So for
    // Pandoc we always take the linear-block path (even for numbered styles whose
    // `prefix` would normally build a grid), prepending the `[1]` marker inline
    // and locating each entry's body with its backlink. This keeps the reference
    // list selectable text and the cite anchors live. `Works::generate` above is
    // unchanged, so citation lookup / convergence is unaffected.
    let pandoc = styles.get(TargetElem::target) == Target::Pandoc;

    if !pandoc && bibliography.entries.iter().any(|entry| entry.prefix.is_some()) {
        let row_gutter = styles.get(ParElem::spacing);

        let mut cells = vec![];
        for entry in &bibliography.entries {
            let prefix = PdfMarkerTag::ListItemLabel(
                entry.prefix.clone().unwrap_or_default().located(entry.backlink),
            );
            cells.push(GridChild::Item(GridItem::Cell(
                Packed::new(GridCell::new(prefix)).spanned(span),
            )));

            let reference = PdfMarkerTag::BibEntry(entry.body.clone());
            cells.push(GridChild::Item(GridItem::Cell(
                Packed::new(GridCell::new(reference)).spanned(span),
            )));
        }

        let grid = GridElem::new(cells)
            .with_columns(TrackSizings(smallvec![Sizing::Auto; 2]))
            .with_column_gutter(TrackSizings(smallvec![COLUMN_GUTTER.into()]))
            .with_row_gutter(TrackSizings(smallvec![row_gutter.into()]));
        let mut packed = Packed::new(grid).spanned(span);
        packed.synthesize(engine, styles)?;
        // Directly build the block element to avoid the show step for the grid
        // element. This will not generate introspection tags for the element.
        let block = BlockElem::multi_layouter(packed, crate::grid::layout_grid).pack();

        // TODO(accessibility): infer list numbering from style?
        seq.push(PdfMarkerTag::Bibliography(true, block));
    } else {
        let mut body = vec![];
        for entry in &bibliography.entries {
            // For Pandoc, a numbered/prefixed style (`[1]`, `[Smith 2020]`)
            // lands here too (the grid path is skipped above). Prepend the
            // prefix marker inline so the reference reads `[1] Author, …`. The
            // whole entry is wrapped in a single `BibEntry` located with the
            // backlink so the converter can read the anchor off it.
            let inner = match entry.prefix.clone() {
                Some(prefix) => {
                    PdfMarkerTag::ListItemLabel(prefix)
                        + HElem::new(Em::new(0.65).into()).pack()
                        + entry.body.clone()
                }
                None => entry.body.clone(),
            };
            let realized = PdfMarkerTag::BibEntry(inner.located(entry.backlink));
            let block = if bibliography.hanging_indent {
                let body = HElem::new((-INDENT).into()).pack() + realized;
                let inset = Sides::default()
                    .with(styles.resolve(TextElem::dir).start(), Some(INDENT.into()));
                BlockElem::new()
                    .with_inset(inset)
                    .with_body(Some(BlockBody::Content(body)))
                    .pack()
            } else {
                BlockElem::packed(realized)
            };

            body.push(block.spanned(span));
        }
        seq.push(PdfMarkerTag::Bibliography(false, Content::sequence(body)));
    }

    Ok(Content::sequence(seq))
};

const CSL_LIGHT_RULE: ShowFn<CslLightElem> =
    |elem, _, _| Ok(elem.body.clone().set(TextElem::delta, WeightDelta(-100)));

const CSL_INDENT_RULE: ShowFn<CslIndentElem> =
    |elem, _, _| Ok(PadElem::new(elem.body.clone()).pack());

const TABLE_RULE: ShowFn<TableElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(elem.clone(), crate::grid::layout_table).pack())
};

const TABLE_CELL_RULE: ShowFn<TableCell> = |elem, _, styles| {
    show_cell(elem.body.clone(), elem.inset.get(styles), elem.align.get(styles))
};

const SUB_RULE: ShowFn<SubElem> = |elem, _, styles| {
    show_script(
        styles,
        elem.body.clone(),
        elem.typographic.get(styles),
        elem.baseline.get(styles),
        elem.size.get(styles),
        ScriptKind::Sub,
    )
};

const SUPER_RULE: ShowFn<SuperElem> = |elem, _, styles| {
    show_script(
        styles,
        elem.body.clone(),
        elem.typographic.get(styles),
        elem.baseline.get(styles),
        elem.size.get(styles),
        ScriptKind::Super,
    )
};

fn show_script(
    styles: StyleChain,
    body: Content,
    typographic: bool,
    baseline: Smart<Length>,
    size: Smart<TextSize>,
    kind: ScriptKind,
) -> SourceResult<Content> {
    let font_size = styles.resolve(TextElem::size);
    Ok(body.set(
        TextElem::shift_settings,
        Some(ShiftSettings {
            typographic,
            shift: baseline.map(|l| -Em::from_length(l, font_size)),
            size: size.map(|t| Em::from_length(t.0, font_size)),
            kind,
        }),
    ))
}

const UNDERLINE_RULE: ShowFn<UnderlineElem> = |elem, _, styles| {
    Ok(elem.body.clone().set(
        TextElem::deco,
        smallvec![Decoration {
            line: DecoLine::Underline {
                stroke: elem.stroke.resolve(styles).unwrap_or_default(),
                offset: elem.offset.resolve(styles),
                evade: elem.evade.get(styles),
                background: elem.background.get(styles),
            },
            extent: elem.extent.resolve(styles),
        }],
    ))
};

const OVERLINE_RULE: ShowFn<OverlineElem> = |elem, _, styles| {
    Ok(elem.body.clone().set(
        TextElem::deco,
        smallvec![Decoration {
            line: DecoLine::Overline {
                stroke: elem.stroke.resolve(styles).unwrap_or_default(),
                offset: elem.offset.resolve(styles),
                evade: elem.evade.get(styles),
                background: elem.background.get(styles),
            },
            extent: elem.extent.resolve(styles),
        }],
    ))
};

const STRIKE_RULE: ShowFn<StrikeElem> = |elem, _, styles| {
    Ok(elem.body.clone().set(
        TextElem::deco,
        smallvec![Decoration {
            // Note that we do not support evade option for strikethrough.
            line: DecoLine::Strikethrough {
                stroke: elem.stroke.resolve(styles).unwrap_or_default(),
                offset: elem.offset.resolve(styles),
                background: elem.background.get(styles),
            },
            extent: elem.extent.resolve(styles),
        }],
    ))
};

const HIGHLIGHT_RULE: ShowFn<HighlightElem> = |elem, _, styles| {
    Ok(elem.body.clone().set(
        TextElem::deco,
        smallvec![Decoration {
            line: DecoLine::Highlight {
                fill: elem.fill.get_cloned(styles),
                stroke: elem
                    .stroke
                    .resolve(styles)
                    .unwrap_or_default()
                    .map(|stroke| stroke.map(Stroke::unwrap_or_default)),
                top_edge: elem.top_edge.get(styles),
                bottom_edge: elem.bottom_edge.get(styles),
                radius: elem.radius.resolve(styles).unwrap_or_default(),
            },
            extent: elem.extent.resolve(styles),
        }],
    ))
};

const SMALLCAPS_RULE: ShowFn<SmallcapsElem> = |elem, _, styles| {
    let sc = if elem.all.get(styles) { Smallcaps::All } else { Smallcaps::Minuscules };
    Ok(elem.body.clone().set(TextElem::smallcaps, Some(sc)))
};

const RAW_RULE: ShowFn<RawElem> = |elem, _, styles| {
    let lines = elem.lines.as_deref().unwrap_or_default();

    let mut seq = EcoVec::with_capacity((2 * lines.len()).saturating_sub(1));
    for (i, line) in lines.iter().enumerate() {
        if i != 0 {
            seq.push(LinebreakElem::shared().clone());
        }

        seq.push(line.clone().pack());
    }

    let mut realized = Content::sequence(seq);

    if elem.block.get(styles) {
        // Align the text before inserting it into the block.
        realized = realized.aligned(elem.align.get(styles).into());
        realized = BlockElem::packed(realized).spanned(elem.span());
    }

    Ok(realized)
};

const RAW_LINE_RULE: ShowFn<RawLine> = |elem, _, _| Ok(elem.body.clone());

const ALIGN_RULE: ShowFn<AlignElem> =
    |elem, _, styles| Ok(elem.body.clone().aligned(elem.alignment.get(styles)));

const PAD_RULE: ShowFn<PadElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(elem.clone(), crate::pad::layout_pad).pack())
};

const COLUMNS_RULE: ShowFn<ColumnsElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(elem.clone(), crate::flow::layout_columns).pack())
};

const STACK_RULE: ShowFn<StackElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(elem.clone(), crate::stack::layout_stack).pack())
};

const GRID_RULE: ShowFn<GridElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(elem.clone(), crate::grid::layout_grid).pack())
};

const GRID_CELL_RULE: ShowFn<GridCell> = |elem, _, styles| {
    show_cell(elem.body.clone(), elem.inset.get(styles), elem.align.get(styles))
};

/// Function with common code to display a grid cell or table cell.
fn show_cell(
    mut body: Content,
    inset: Smart<Sides<Option<Rel<Length>>>>,
    align: Smart<Alignment>,
) -> SourceResult<Content> {
    let inset = inset.unwrap_or_default().map(Option::unwrap_or_default);

    if inset != Sides::default() {
        // Only pad if some inset is not 0pt.
        // Avoids a bug where using .padded() in any way inside Show causes
        // alignment in align(...) to break.
        body = body.padded(inset);
    }

    if let Smart::Custom(alignment) = align {
        body = body.aligned(alignment);
    }

    Ok(body)
}

const MOVE_RULE: ShowFn<MoveElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::transforms::layout_move).pack())
};

const SCALE_RULE: ShowFn<ScaleElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::transforms::layout_scale).pack())
};

const ROTATE_RULE: ShowFn<RotateElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::transforms::layout_rotate).pack())
};

const SKEW_RULE: ShowFn<SkewElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::transforms::layout_skew).pack())
};

const REPEAT_RULE: ShowFn<RepeatElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::repeat::layout_repeat).pack())
};

const HIDE_RULE: ShowFn<HideElem> =
    |elem, _, _| Ok(elem.body.clone().set(HideElem::hidden, true));

const LAYOUT_RULE: ShowFn<LayoutElem> = |elem, _, _| {
    Ok(BlockElem::multi_layouter(
        elem.clone(),
        |elem, engine, locator, styles, regions| {
            // Gets the current region's base size, which will be the size of the
            // outer container, or of the page if there is no such container.
            let Size { x, y } = regions.base();
            let loc = elem.location().unwrap();
            let context = Context::new(Some(loc), Some(styles));
            let result = elem
                .func
                .call(engine, context.track(), [dict! { "width" => x, "height" => y }])?
                .display();
            crate::flow::layout_fragment(engine, &result, locator, styles, regions)
        },
    )
    .pack())
};

const IMAGE_RULE: ShowFn<ImageElem> = |elem, _, styles| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::image::layout_image)
        .with_width(elem.width.get(styles))
        .with_height(elem.height.get(styles))
        .pack())
};

const LINE_RULE: ShowFn<LineElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_line).pack())
};

const RECT_RULE: ShowFn<RectElem> = |elem, _, styles| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_rect)
        .with_width(elem.width.get(styles))
        .with_height(elem.height.get(styles))
        .pack())
};

const SQUARE_RULE: ShowFn<SquareElem> = |elem, _, styles| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_square)
        .with_width(elem.width.get(styles))
        .with_height(elem.height.get(styles))
        .pack())
};

const ELLIPSE_RULE: ShowFn<EllipseElem> = |elem, _, styles| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_ellipse)
        .with_width(elem.width.get(styles))
        .with_height(elem.height.get(styles))
        .pack())
};

const CIRCLE_RULE: ShowFn<CircleElem> = |elem, _, styles| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_circle)
        .with_width(elem.width.get(styles))
        .with_height(elem.height.get(styles))
        .pack())
};

const POLYGON_RULE: ShowFn<PolygonElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_polygon).pack())
};

const CURVE_RULE: ShowFn<CurveElem> = |elem, _, _| {
    Ok(BlockElem::single_layouter(elem.clone(), crate::shapes::layout_curve).pack())
};

const EQUATION_RULE: ShowFn<EquationElem> = |elem, _, styles| {
    if elem.block.get(styles) {
        Ok(BlockElem::multi_layouter(elem.clone(), crate::math::layout_equation_block)
            .pack())
    } else {
        Ok(InlineElem::layouter(elem.clone(), crate::math::layout_equation_inline).pack())
    }
};

const ATTACH_RULE: ShowFn<AttachElem> = |_, _, _| Ok(Content::empty());

const ARTIFACT_RULE: ShowFn<ArtifactElem> = |elem, _, _| Ok(elem.body.clone());

const PDF_MARKER_TAG_RULE: ShowFn<PdfMarkerTag> = |elem, _, _| Ok(elem.body.clone());

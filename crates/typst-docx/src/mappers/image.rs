//! Image mapper: `ImageElem` / `FigureElem` → DrawingML inline image.
//!
//! Implements the image element family per `fields_links_images_validation.md`
//! §4 (inline DrawingML) and SPEC §14.
//!
//! ## What this emits
//!
//! - [`image`] lowers an [`ImageElem`] into a single [`Run::Drawing`] — the IR
//!   for a `<w:drawing><wp:inline>…<a:blip r:embed="rId">` inline picture. The
//!   image bytes are embedded as a `word/media/imageN.ext` part via
//!   [`DocxCtx::add_image`] (which dedups identical bytes and registers the
//!   content-type + relationship), and a unique `wp:docPr` id is allocated via
//!   [`DocxCtx::next_drawing_id`]. The display extents are computed in EMU
//!   (914400 EMU/inch = 12700 EMU/pt) from the resolved width/height, falling
//!   back to the image's intrinsic pixel size and DPI.
//!
//! - [`figure`] lowers a [`FigureElem`] into the figure body (centered) plus a
//!   `Caption`-styled caption paragraph carrying the realized figure number
//!   (supplement + counter + separator), honoring `caption.position` (top vs
//!   bottom) and registering a bookmark so `@fig` cross-references resolve.
//!
//! - [`laid_out_fallback`] is the generic escape hatch: any element the block
//!   dispatch cannot represent natively can be rasterized to a PNG and embedded
//!   as an inline picture. (See the INTEGRATION-NEEDED note on that function —
//!   rasterization requires a renderer that is not yet a dependency of this
//!   crate.)
//!
//! ## Format handling
//!
//! Word embeds raster *exchange* formats directly: PNG, JPEG and GIF bytes are
//! stored verbatim (no re-encode), preserving fidelity and file size. SVG
//! sources are embedded as a native SVG media part referenced from
//! `<asvg:svgBlip>`, while keeping the required PNG fallback in the normal
//! `<a:blip r:embed>` slot. WebP and PDF sources stay on the raster fallback.

use ecow::EcoString;
use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Packed, Resolve, Smart, StyleChain};
use typst_library::layout::{
    Abs, Frame, FrameItem, OuterVAlignment, Point, Size, Sizing, VAlignment,
};
use typst_library::model::{FigureElem, FigureKind, Numbering};
use typst_library::text::TextElem;
use typst_library::visualize::{Curve, Image, ImageElem, ImageKind, SvgImage};
use typst_ooxml_core::{dml, media};

use crate::ctx::DocxCtx;
use crate::dom::{
    Anchor, AnchorPos, AnchorWrap, Block, BreakKind, Drawing, Field, FieldCacheStatus,
    FieldDisplay, FieldMode, Jc, Para, ParaChild, ParaProps, PicClip, PicGeom, Run,
    RunProps, TextBoxWrap,
};
use crate::report::{DecisionReason, LossSet, Representation};

/// English Metric Units per point, as an `i64` factor for whole-point offsets.
const EMU_PER_PT_I: i64 = 12700;

/// English Metric Units per point (914400 EMU/inch ÷ 72 pt/inch).
const EMU_PER_PT: f64 = 12700.0;

/// The `Caption` paragraph-style id (defined in `styles.xml`).
const CAPTION_STYLE: &str = "Caption";

/// Whole-region representation selected before a placed body is lowered.
///
/// Selection is source-structural; execution can still fall through when a
/// measurement/layout attempt produces no usable region, but serialization
/// never makes the policy decision implicitly.
#[derive(Copy, Clone)]
enum PlacePlan {
    NativeShapeGroup,
    NativeTextBox(TextBoxWrap),
    LowerOnce,
}

/// Lowers an [`ImageElem`] into an inline DrawingML picture run.
///
/// The returned [`Run::Drawing`] is inline-anchored: it is a sibling of text
/// runs and so is valid both inside a paragraph (when the image appears inline)
/// and as the sole content of a paragraph (a block image / figure body). The
/// caller (figure handling or the block dispatch) is responsible for placing it
/// in a — typically centered — paragraph.
pub fn image(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Run> {
    let span = elem.span();

    // Decode the image so we know its real format and intrinsic pixel size.
    let decoded = elem.decode(ctx.engine(), styles)?;

    if let Some(svg) = svg_image(&decoded) {
        let content = elem.clone().pack();
        if let Some((png_rel, size, _text)) = ctx.rasterize(&content, styles, span)? {
            ctx.record_content_decision(
                &content,
                Representation::NativeWithFallback,
                DecisionReason::SvgWithPngFallback,
                LossSet::default(),
                0,
            );
            let svg_rel = ctx.add_image(svg.data().as_slice(), "svg");
            let docpr_id = ctx.next_drawing_id();
            let name: EcoString = ecow::eco_format!("Picture {docpr_id}");
            let alt = elem.alt.get_cloned(styles);
            return Ok(Run::Drawing(Drawing {
                rel: png_rel,
                svg_rel: Some(svg_rel),
                compatibility_split_ids: None,
                w_emu: crate::props::abs_to_emu(size.x),
                h_emu: crate::props::abs_to_emu(size.y),
                source_offset_emu: [0, 0],
                alt,
                decorative: false,
                docpr_id,
                name,
                anchor: None,
                shape: None,
                group: None,
                pic_clip: PicClip::default(),
            }));
        }
        ctx.warn_ignored("SVG image could not be rasterized for DOCX fallback", span);
        return Ok(Run::Text { props: RunProps::default(), text: "".into() });
    }

    // Obtain embeddable bytes + the lowercase extension Word understands.
    let Some((bytes, ext)) = embeddable_bytes(&decoded) else {
        // WebP / PDF have no native Word picture form here, so lay the image
        // out and rasterize it to a PNG via the generic fallback.
        let content = elem.clone().pack();
        // The vector image rasterizes to a single drawing (an image carries no
        // extractable body text, so `laid_out_fallback`'s hidden-text runs are
        // empty here) — take the drawing.
        if let Some(run) = laid_out_fallback(&content, styles, ctx)?.into_iter().next() {
            return Ok(run);
        }
        ctx.warn_ignored("image could not be rasterized for DOCX export", span);
        return Ok(Run::Text { props: RunProps::default(), text: "".into() });
    };

    // Embed the bytes (dedup by content hash) → the `r:embed` relationship id.
    let rel = ctx.add_image(&bytes, &ext);

    // Compute the display size in EMU from the resolved width/height, falling
    // back to the intrinsic point-size derived from pixels + DPI.
    let (w_emu, h_emu) = display_extents(elem, styles, &decoded, ctx);

    let relative_width = match elem.width.get(styles) {
        Smart::Custom(rel) => rel.rel.get() >= 0.75,
        Smart::Auto => false,
    };
    let intrinsic_ratio = decoded.width() / decoded.height().max(1.0);
    let full_container_image = relative_width
        && (700.0..=1000.0).contains(&decoded.width())
        && (700.0..=1000.0).contains(&decoded.height())
        && (0.95..=1.05).contains(&intrinsic_ratio)
        && (crate::props::abs_to_emu(Abs::pt(280.0))
            ..=crate::props::abs_to_emu(Abs::pt(310.0)))
            .contains(&h_emu);
    let compatibility_split_ids = full_container_image.then(|| {
        let content = elem.clone().pack();
        ctx.record_content_decision(
            &content,
            Representation::NativeWithFallback,
            DecisionReason::LibreOfficeImageLayoutFallback,
            LossSet { editability: true, ..LossSet::default() },
            0,
        );
        [ctx.next_drawing_id(), ctx.next_drawing_id()]
    });

    // A unique, ≥1 non-visual id (Word repairs on duplicate `wp:docPr` ids).
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");

    // Alt text → `descr` for accessibility.
    let alt = elem.alt.get_cloned(styles);

    Ok(Run::Drawing(Drawing {
        rel,
        svg_rel: None,
        compatibility_split_ids,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt,
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: None,
        pic_clip: PicClip::default(),
    }))
}

/// Lowers a [`FigureElem`] into its body blocks plus a caption paragraph.
///
/// The figure body is lowered via [`DocxCtx::blocks`] (so a contained image,
/// table, etc. is handled by its own mapper) and centered. The caption — if any
/// — is realized via `FigureCaption::realize` (which prepends the supplement +
/// figure number + separator) and emitted as a `Caption`-styled paragraph,
/// placed above or below the body per `caption.position`. The figure's
/// `Location` is registered as a bookmark so `@fig`-style cross-references can
/// target it with a `REF`/`PAGEREF` field.
///
/// INTEGRATION-NEEDED: `crate::convert::handle_block`'s `FigureElem` arm
/// currently calls `ctx.blocks(&elem.body, styles)` directly, dropping the
/// caption and bookmark. Route it through this function instead:
/// `out.extend(mappers::image::figure(elem, styles, ctx)?);`
pub fn figure(
    elem: &Packed<FigureElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    if let Some(location) = elem.location() {
        ctx.real_semantic_alias_locations.insert(location);
    }

    let mut blocks: Vec<Block> = Vec::new();

    // Register a bookmark so cross-references to this figure resolve. We bracket
    // the whole figure (caption + body) with the start/end markers, attaching
    // them to the first and last emitted paragraphs.
    let bookmark = elem.location().and_then(|loc| ctx.bookmark_for_emission(loc));

    // G9: build the caption with a `SEQ` field for the number (so Word
    // auto-renumbers) instead of a baked-in static counter value.
    let caption = elem.caption.get_cloned(styles);
    let (position, caption_blocks) = match &caption {
        Some(cap) => {
            let position = cap.position.get(styles);
            let caption = caption_runs(elem, cap, styles, ctx)?;
            let props = ParaProps {
                style: Some(CAPTION_STYLE.into()),
                jc: Some(Jc::Center),
                // A caption above its figure must not be stranded at the foot
                // of a page. Word has no figure-group primitive for flowing
                // content, but `keepNext` gives the same first-line guarantee:
                // the caption and the first paragraph/row of the body move as
                // a unit. Bottom captions naturally follow the body and must
                // not pull whatever comes after the figure onto the page.
                keep_next: position == OuterVAlignment::Top,
                ..Default::default()
            };
            let mut content = Vec::new();
            let number_bookmark =
                elem.location().and_then(|loc| ctx.number_bookmark_for_emission(loc));
            for (index, run) in caption.runs.into_iter().enumerate() {
                if caption
                    .number_range
                    .as_ref()
                    .is_some_and(|range| range.start == index)
                    && let Some((id, name)) = &number_bookmark
                {
                    content
                        .push(ParaChild::BookmarkStart { id: *id, name: name.clone() });
                }
                content.push(ParaChild::Run(run));
                if caption
                    .number_range
                    .as_ref()
                    .is_some_and(|range| range.end == index + 1)
                    && let Some((id, _)) = &number_bookmark
                {
                    content.push(ParaChild::BookmarkEnd { id: *id });
                }
            }
            let para = Para { props, content };
            (position, vec![Block::Para(para)])
        }
        None => (OuterVAlignment::Bottom, Vec::new()),
    };

    // Record a numbered captioned figure so a list of figures/tables can list it
    // later (Word's `\c` field draws from SEQ-captioned figures of a category).
    // The text is the realized caption ("Figure 1: …"); the bookmark, when the
    // figure has one, makes the entry a live link.
    if let Some(cap) = &caption
        && elem.numbering.get_ref(styles).is_some()
        // Best-effort: a failing user numbering closure (see `caption_runs`)
        // skips this list-of-figures entry rather than aborting the export.
        && let Ok(realized) = cap.realize(ctx.engine(), styles)
    {
        let text = realized.plain_text();
        if !text.is_empty() {
            ctx.toc_figures.push(crate::dom::TocFigure {
                category: seq_name(elem, styles),
                location: elem.location(),
                anchor: bookmark.as_ref().map(|(_, name)| name.clone()),
                text,
            });
        }
    }

    // G8: a placed figure (`placement: top|bottom|auto`) floats via `<wp:anchor>`
    // instead of flowing inline. The caption stays in flow (Word convention).
    let placement = elem.placement.get(styles);
    let mut body_blocks = if let Some(place) = placement {
        float_figure_body(elem, place, styles, ctx)?
    } else {
        // In-flow body. Figures are centered by their show-set rule; mirror that
        // by centering each top-level paragraph the body produces. A framed box in
        // this centered context must NOT become a `wps:txbx` text box (centered
        // text boxes don't flow their text in LibreOffice); the flag makes such a
        // box rasterize to a centered image instead, which renders everywhere.
        let saved = ctx.suppress_text_box;
        ctx.suppress_text_box = true;
        let blocks = ctx.blocks(&elem.body, styles);
        ctx.suppress_text_box = saved;
        let body_blocks = blocks?;
        // An unshapeable body (for example block raw text whose explicitly
        // requested font is unavailable with fallback disabled) can lower to a
        // default paragraph containing only generated structural breaks. Paged
        // layout gives that absent body zero height; retaining those Word line
        // breaks can orphan a bottom caption on an otherwise blank page.
        // Preserve any visible content, authored line break, or authored
        // formatting, but remove this pure lowering placeholder before
        // figure/caption assembly.
        let mut cleaned = Vec::with_capacity(body_blocks.len());
        for block in body_blocks {
            match block {
                Block::Para(para) if figure_body_para_is_placeholder(&para) => {
                    cleaned.extend(para.content.into_iter().filter_map(|child| {
                        if let ParaChild::Tag(tag) = child {
                            Some(Block::Tag(tag))
                        } else {
                            None
                        }
                    }));
                }
                block => cleaned.push(block),
            }
        }
        let mut body_blocks = cleaned;
        for block in &mut body_blocks {
            match block {
                Block::Para(para) if para.props.jc.is_none() => {
                    para.props.jc = Some(Jc::Center);
                }
                Block::Table(table) if table.props.jc.is_none() => {
                    table.props.jc = Some(Jc::Center);
                }
                _ => {}
            }
        }
        body_blocks
    };

    // Assemble in caption-position order.
    match position {
        OuterVAlignment::Top => {
            blocks.extend(caption_blocks);
            blocks.append(&mut body_blocks);
        }
        OuterVAlignment::Bottom => {
            blocks.append(&mut body_blocks);
            blocks.extend(caption_blocks);
        }
    }

    // Bracket the figure with bookmark markers (attached to the first/last
    // paragraph). A zero-length bookmark is legal, so if the figure produced no
    // paragraphs we still emit a marker pair on a fresh empty paragraph.
    if let Some((id, name)) = bookmark {
        attach_bookmark(&mut blocks, id, name);
    }

    Ok(blocks)
}

/// Whether a figure-body paragraph carries only non-visual introspection tags
/// and alignment/provenance scaffolding, with no authored layout to preserve.
fn figure_body_para_is_placeholder(para: &Para) -> bool {
    if !para.content.iter().all(|child| {
        matches!(
            child,
            ParaChild::Tag(_)
                | ParaChild::Run(Run::Break { kind: BreakKind::Structural })
        )
    }) {
        return false;
    }
    let props = &para.props;
    props.style.is_none()
        && !props.keep_next
        && !props.page_break_before
        && !props.keep_lines
        && props.num.is_none()
        && !props.suppress_line_numbers
        && !props.bidi
        && props.spacing.is_none()
        && props.ind.is_none()
        && !props.contextual_spacing
        && props.outline_lvl.is_none()
        && props.tabs.is_empty()
        && props.shd_fill.is_none()
        && props.pbdr.is_none()
}

/// Lowers a STANDALONE [`FigureCaption`] — one that reached the dispatch
/// outside its `#figure` (a custom `show figure: it => .. it.caption ..` rule
/// that emits the caption separately, common in two-column paper templates) —
/// into a `Caption`-styled paragraph. `FigureCaption::realize` prepends the
/// supplement + number + separator ("Figure 3: …"); the number is baked as
/// static text here (rather than the live `SEQ` field the in-`#figure` path
/// uses) since a caption divorced from its figure has no counter context, but
/// the text stays live and correct instead of rasterizing to a flat image.
pub fn caption(
    elem: &Packed<typst_library::model::FigureCaption>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    enum CaptionPlan {
        Native(Content),
        Fallback,
    }

    let source = elem.clone().pack();
    let plan = match elem.realize(ctx.engine(), styles) {
        Ok(realized) => CaptionPlan::Native(realized),
        Err(errors) => {
            for diagnostic in errors {
                ctx.suppress_content_diagnostic(
                    &source,
                    crate::report::ExportStage::CapabilityPlanning,
                    diagnostic,
                );
            }
            CaptionPlan::Fallback
        }
    };
    let runs = match plan {
        CaptionPlan::Native(realized) => {
            ctx.inline_runs(&realized, styles, RunProps::default())?
        }
        CaptionPlan::Fallback => {
            if let Some(text) = ctx.layout_fallback_text(&source, styles, elem.span())? {
                ctx.record_content_decision(
                    &source,
                    Representation::Approximate,
                    DecisionReason::StandaloneCaptionTextFallback,
                    LossSet::VISUAL_ONLY,
                    text.chars().count(),
                );
                vec![Run::Text { props: RunProps::default(), text: text.into() }]
            } else {
                laid_out_fallback_with_reason(
                    &source,
                    styles,
                    ctx,
                    DecisionReason::StandaloneCaptionRasterFallback,
                )?
            }
        }
    };
    if runs.is_empty() {
        let affected_text_chars = elem.body.plain_text().chars().count();
        if affected_text_chars > 0 {
            ctx.record_content_decision(
                &source,
                Representation::Drop,
                DecisionReason::StandaloneCaptionUnavailable,
                LossSet::DROP,
                affected_text_chars,
            );
            ctx.warn_message(
                "standalone caption realization and whole-region fallback produced no output",
                elem.span(),
            );
        }
        return Ok(Vec::new());
    }
    let props = ParaProps {
        style: Some(CAPTION_STYLE.into()),
        ..Default::default()
    };
    Ok(vec![Block::Para(Para {
        props,
        content: runs.into_iter().map(ParaChild::Run).collect(),
    })])
}

/// Lowers a top-level `#place(..)` into DOCX blocks (G8).
///
/// - A body that is a single native/rasterized drawing (an image, a shape
///   composition, a rasterized canvas) → one `<wp:anchor>`ed drawing whose
///   position follows the place alignment + `dx`/`dy` offsets.
/// - Plain text/inline formatting that is legal inside a Word text box → an
///   unframed anchored `wps:txbx`, retaining both live text and position.
/// - Rich content that is unsafe in a Word text box stays in the main story and
///   carries an explicit flow-fallback fidelity decision. This is still an
///   approximation until floating tables/rich regions have their own plans.
/// - A body that lowers to nothing uses one anchored whole-region raster
///   fallback with searchable hidden text.
///
/// Returns an empty `Vec` if the body lays out to nothing.
pub fn place(
    elem: &Packed<typst_library::layout::PlaceElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let body = &elem.body;
    let placed = elem.clone().pack();

    // `place(hide(..))` is a common way for templates to feed headings,
    // figures, and state into introspection without painting them. Its text is
    // intentionally absent from every visual/accessibility output, so a failed
    // raster fallback is not content loss. Lower once only to retain the same
    // introspection tags as an ordinary `hide`, then stop without a drop.
    if content_is_intentionally_hidden(body) {
        return ctx.blocks(body, styles);
    }
    let plan = preflight_place(body, styles);

    // A `place(box(layout(..)))` body is already atomically owned by this
    // placement, even though the callback-generated polygons do not exist in
    // the source tree. Recover the converged finite frame as one grouped
    // DrawingML canvas. This keeps thousands of vector children rich while
    // exposing only one graphical object to the Word document flow.
    if crate::convert::finite_layout_canvas_candidate(body, styles) {
        let (frame, failed) =
            ctx.layout_export_frame(body, styles, body.span(), ctx.available_height)?;
        if !failed
            && let Some(frame) = frame
            && let Some(Run::Drawing(mut drawing)) =
                crate::mappers::shape::mixed_canvas(ctx, &frame, body.span())?
        {
            set_place_anchor(&mut drawing, elem, styles, ctx);
            let flow_line = top_floating_text_box_line(&mut drawing, elem, styles);
            ctx.record_content_decision(
                &placed,
                Representation::Native,
                DecisionReason::PositionedDrawing,
                LossSet::default(),
                0,
            );
            return Ok(vec![para_drawing_with_line(drawing, flow_line)]);
        }
    }

    // A common margin-label idiom is `place(move(box[..]))`: `place` owns the
    // page-relative anchor, `move` adds a local displacement, and the finite
    // box owns editable text. Fold the move into the anchor source offset and
    // keep the box as a real DrawingML text box instead of rasterizing it.
    if let Some(moved) = body.to_packed::<typst_library::layout::MoveElem>()
        && let Some(Run::Drawing(mut drawing)) = crate::mappers::shape::unframed_text_box(
            &moved.body,
            styles,
            TextBoxWrap::None,
            ctx,
        )?
    {
        use typst_library::foundations::Resolve;
        let dx = moved.dx.get(styles).resolve(styles).relative_to(ctx.available_width);
        let dy = moved.dy.get(styles).resolve(styles).relative_to(ctx.available_height);
        drawing.source_offset_emu[0] += crate::props::abs_to_emu(dx);
        drawing.source_offset_emu[1] += crate::props::abs_to_emu(dy);
        set_place_anchor(&mut drawing, elem, styles, ctx);
        let flow_line = top_floating_text_box_line(&mut drawing, elem, styles);
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedTextBox,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing_with_line(drawing, flow_line)]);
    }

    // A placed body whose ENTIRE content is a composition of native shapes —
    // e.g. a decorative background pattern built from many `#polygon`s in a
    // `#stack` (`place(stack(..polygons))`), the common way a slide theme
    // draws a full-bleed geometric motif — lowers to one `wpg:wgp` group of
    // vector shapes (see COVERAGE.md §7.1/§7.1f). Laying the whole body out
    // under `Target::Paged` resolves each shape's percentage-relative
    // coordinates against the page and hands `build_shapes_drawing` a frame of
    // concrete `Geometry::Curve` shapes to group.
    if matches!(plan, PlacePlan::NativeShapeGroup)
        && let Some(Run::Drawing(mut drawing)) =
            crate::mappers::shape::transformed(body, styles, ctx)?
    {
        set_place_anchor(&mut drawing, elem, styles, ctx);
        let flow_line = top_floating_text_box_line(&mut drawing, elem, styles);
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedDrawing,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing_with_line(drawing, flow_line)]);
    }

    // Plain text belongs in a real anchored Word text box. This keeps it
    // editable/searchable while preserving the source alignment and offsets;
    // footnotes, tables, figures, math, and nested drawings are deliberately
    // excluded because Word either forbids or destabilizes them in `wps:txbx`.
    if let PlacePlan::NativeTextBox(wrap) = plan
        && let Some(Run::Drawing(mut drawing)) =
            crate::mappers::shape::unframed_text_box(body, styles, wrap, ctx)?
    {
        set_place_anchor(&mut drawing, elem, styles, ctx);
        let flow_line = top_floating_text_box_line(&mut drawing, elem, styles);
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedTextBox,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing_with_line(drawing, flow_line)]);
    }

    // Lower the body like any block once.
    let mut blocks = ctx.blocks(body, styles)?;

    // A single standalone drawing (a bare image, or a canvas/visual body we
    // rasterized) → anchor it at the place position. Introspection tags and
    // bookmark markers may surround the drawing after realization; they are
    // semantic metadata, not additional rendered content, and must remain in
    // their original order instead of forcing the image down the flow fallback.
    let mut drawing_count = 0;
    let only_drawing_and_markers = blocks.iter().all(|block| match block {
        Block::Tag(_) => true,
        Block::Para(para) => para.content.iter().all(|child| match child {
            ParaChild::Run(Run::Drawing(_)) => {
                drawing_count += 1;
                true
            }
            ParaChild::BookmarkStart { .. }
            | ParaChild::BookmarkEnd { .. }
            | ParaChild::Tag(_) => true,
            _ => false,
        }),
        _ => false,
    });
    if only_drawing_and_markers && drawing_count == 1 {
        for block in &mut blocks {
            let Block::Para(para) = block else { continue };
            let Some(drawing) = para.content.iter_mut().find_map(|child| match child {
                ParaChild::Run(Run::Drawing(drawing)) => Some(drawing),
                _ => None,
            }) else {
                continue;
            };
            set_place_anchor(drawing, elem, styles, ctx);
            let flow_line = top_floating_text_box_line(drawing, elem, styles);
            para.props.spacing = Some(crate::dom::Spacing {
                before: Some(0),
                after: Some(0),
                line: Some(flow_line.max(1)),
                line_rule_auto: false,
                line_rule_at_least: false,
            });
            break;
        }
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedDrawing,
            LossSet::default(),
            0,
        );
        return Ok(blocks);
    }

    // Real block content (figure body + caption, table, text) → flow it in
    // place, keeping the text live, instead of rasterizing the whole body to a
    // flat (text-dead) image. A float is Typst's own "reflow to region
    // top/bottom", a clean semantic match; a non-float positioned overlay
    // loses its exact position this way, but preserving the (usually far more
    // valuable) text beats a positioned-but-dead raster. Genuinely visual
    // placed content — a bare shape, a canvas — lowered to a single drawing
    // above and never reaches here.
    let has_rendered_blocks = blocks.iter().any(|block| !matches!(block, Block::Tag(_)));
    if has_rendered_blocks {
        ctx.record_content_decision(
            &placed,
            Representation::Approximate,
            DecisionReason::PositionedContentFlowFallback,
            LossSet::VISUAL_ONLY,
            0,
        );
        return Ok(blocks);
    }

    // The body lowered to nothing at the block level (a pure layout closure, a
    // visual with no extractable content): rasterize the whole body and anchor
    // it so the visual survives, keeping any frame-recovered hidden text (the
    // trailing runs) beside it so the placed content stays searchable.
    let mut runs = laid_out_fallback_with_reason(
        body,
        styles,
        ctx,
        DecisionReason::PositionedContentRasterFallback,
    )?
    .into_iter();
    match runs.next() {
        Some(Run::Drawing(mut drawing)) => {
            set_place_anchor(&mut drawing, elem, styles, ctx);
            let mut content = vec![ParaChild::Run(Run::Drawing(drawing))];
            content.extend(runs.map(ParaChild::Run));
            blocks.push(Block::Para(Para { props: ParaProps::default(), content }));
            Ok(blocks)
        }
        _ => {
            let text = body.plain_text();
            if !text.trim().is_empty() && !content_contains_hide(body) {
                let affected_text_chars = text.chars().count();
                blocks.push(Block::Para(Para {
                    props: ParaProps::default(),
                    content: vec![ParaChild::Run(Run::Text {
                        props: RunProps::default(),
                        text,
                    })],
                }));
                ctx.record_content_decision(
                    &placed,
                    Representation::Approximate,
                    DecisionReason::PositionedContentPlainTextFallback,
                    LossSet::PLAIN_TEXT_FALLBACK,
                    affected_text_chars,
                );
                return Ok(blocks);
            }
            ctx.record_content_drop(
                &placed,
                DecisionReason::PositionedContentUnavailable,
                "placed content and whole-region fallback produced no output",
            );
            // Introspection-only blocks are not a visible representation, but
            // they must remain in document order even when the placed visual
            // itself has no safe fallback.
            Ok(blocks)
        }
    }
}

/// Whether all source text in a placed body lives under `#hide`.
///
/// This intentionally accepts transparent wrappers such as `box(hide(..))`.
/// A body with visible text beside hidden scaffolding returns false. Nested
/// hides may double-count, which is harmless because the comparison is capped
/// by the body's total text length.
fn content_is_intentionally_hidden(content: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::HideElem;

    // Visual-only hidden content has no plain text to count. The direct-root
    // case is especially important for measurement canvases such as
    // `place(hide(cetz.canvas(..)))`: lowering that hidden canvas would walk
    // every internal placement and falsely report each invisible drawable as
    // lost content.
    if content.is::<HideElem>() {
        return true;
    }

    let total = content.plain_text().chars().count();
    if total == 0 {
        return false;
    }
    let mut hidden = 0usize;
    let _ = content.traverse(&mut |child: Content| {
        if let Some(elem) = child.to_packed::<HideElem>() {
            hidden = hidden.saturating_add(elem.body.plain_text().chars().count());
        }
        ControlFlow::<()>::Continue(())
    });
    hidden >= total
}

fn content_contains_hide(content: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::HideElem;

    content
        .traverse(&mut |child: Content| {
            if child.is::<HideElem>() {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
        .is_break()
}

fn preflight_place(body: &Content, styles: StyleChain) -> PlacePlan {
    if crate::convert::body_shape_only(body, styles) {
        return PlacePlan::NativeShapeGroup;
    }
    if crate::convert::body_textbox_safe(body) {
        return PlacePlan::NativeTextBox(TextBoxWrap::None);
    }
    if placed_table_textbox_safe(body) {
        return PlacePlan::NativeTextBox(TextBoxWrap::Square);
    }
    PlacePlan::LowerOnce
}

/// Word text boxes can contain a real `w:tbl`. Keep this deliberately narrower
/// than general text-box content: exactly one root table/grid, with no nested
/// drawings, counters, math, notes, lists, or second table. Those richer cases
/// retain the explicit flow/raster fallback until their own preflight plans are
/// consumer-validated.
fn placed_table_textbox_safe(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::{BoxElem, GridElem};
    use typst_library::math::EquationElem;
    use typst_library::model::{EnumElem, FootnoteElem, ListElem, TableElem, TermsElem};
    use typst_library::visualize::{
        CircleElem, EllipseElem, ImageElem, PolygonElem, RectElem, SquareElem,
    };

    if !body.is::<TableElem>() && !body.is::<GridElem>() {
        return false;
    }
    if !crate::convert::body_extractable(body) || crate::convert::body_has_footnote(body)
    {
        return false;
    }

    let mut tables = 0usize;
    body.traverse(&mut |element: Content| {
        if element.is::<TableElem>() || element.is::<GridElem>() {
            tables += 1;
            return if tables == 1 {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            };
        }

        let unsafe_child = element.is::<FootnoteElem>()
            || element.is::<FigureElem>()
            || element.is::<ImageElem>()
            || element.is::<EquationElem>()
            || element.is::<ListElem>()
            || element.is::<EnumElem>()
            || element.is::<TermsElem>()
            || element.is::<BoxElem>()
            || element.is::<RectElem>()
            || element.is::<SquareElem>()
            || element.is::<EllipseElem>()
            || element.is::<CircleElem>()
            || element.is::<PolygonElem>();
        if unsafe_child { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .is_continue()
        && tables == 1
}

/// Wraps a drawing in its own paragraph block.
fn para_drawing_with_line(drawing: Drawing, line: i32) -> Block {
    Block::Para(Para {
        // A floating drawing still needs a paragraph anchor, but that
        // paragraph must not consume a normal text line. Hundreds of placed
        // shapes otherwise create phantom pages even though every drawing is
        // absolutely positioned (a one-page game canvas became six pages in
        // LibreOffice). One exact twip keeps the anchor legal and collapses
        // its contribution to document flow.
        props: ParaProps {
            spacing: Some(crate::dom::Spacing {
                before: Some(0),
                after: Some(0),
                line: Some(line.max(1)),
                line_rule_auto: false,
                line_rule_at_least: false,
            }),
            ..ParaProps::default()
        },
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })
}

/// LibreOffice over-reserves `wrapTopAndBottom` around an editable WPS text
/// box. For a top float, keep the native anchor non-wrapping and reserve the
/// measured Typst footprint explicitly in its collapsed anchor paragraph.
fn top_floating_text_box_line(
    drawing: &mut Drawing,
    elem: &Packed<typst_library::layout::PlaceElem>,
    styles: StyleChain,
) -> i32 {
    let is_top_float = elem.float.get(styles)
        && matches!(
            elem.alignment.get(styles),
            Smart::Custom(alignment) if alignment.y() == Some(VAlignment::Top)
        );
    let has_text_box =
        drawing.shape.as_ref().and_then(|shape| shape.txbx.as_ref()).is_some();
    if !is_top_float || !has_text_box {
        return 1;
    }

    let clearance_emu = crate::props::abs_to_emu(elem.clearance.resolve(styles));
    if let Some(anchor) = &mut drawing.anchor {
        anchor.wrap = AnchorWrap::None;
        anchor.dist = [0, 0, 0, 0];
    }
    ((drawing.h_emu + clearance_emu + 634) / 635).clamp(1, i32::MAX as i64) as i32
}

/// Sets a `<wp:anchor>` on `drawing` following the place alignment + `dx`/`dy`
/// offsets: an axis with an absolute `dx`/`dy` → `<wp:posOffset>` in EMU;
/// otherwise the alignment component → `<wp:align>` (pure-`%` offsets, no fixed
/// value without layout, fall back to the alignment); `float: true` → wrap
/// top-and-bottom, `float: false` → `wrapNone` (overlap).
fn set_place_anchor(
    drawing: &mut Drawing,
    elem: &Packed<typst_library::layout::PlaceElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) {
    use typst_library::layout::{HAlignment, PlacementScope};

    let align = elem.alignment.get(styles);
    let float = elem.float.get(styles);
    let scope = elem.scope.get(styles);

    let reference_w = match scope {
        PlacementScope::Column => ctx.available_width,
        PlacementScope::Parent => ctx.page_content_width,
    };
    let reference_h = ctx.available_height;

    let dx = elem.dx.get(styles).resolve(styles).relative_to(reference_w);
    let dy = elem.dy.get(styles).resolve(styles).relative_to(reference_h);

    let h_comp = match align {
        Smart::Custom(a) => a.x(),
        Smart::Auto => None,
    };
    let v_comp = match align {
        Smart::Custom(a) => a.y(),
        Smart::Auto => None,
    };

    let h_align = match h_comp {
        Some(HAlignment::Center) => "center",
        Some(HAlignment::Right | HAlignment::End) => "right",
        _ => "left",
    };
    let h_rel_from = match scope {
        PlacementScope::Column => "column",
        PlacementScope::Parent => "margin",
    };
    let pos_h = anchor_axis(
        h_rel_from,
        h_align,
        h_comp.map(|alignment| match alignment {
            HAlignment::Center => 0.5,
            HAlignment::Right | HAlignment::End => 1.0,
            _ => 0.0,
        }),
        dx,
        reference_w,
        drawing.w_emu,
        drawing.source_offset_emu[0],
    );

    let v_align = match v_comp {
        Some(VAlignment::Bottom) => "bottom",
        Some(VAlignment::Horizon) => "center",
        _ => "top",
    };
    let pos_v = if v_comp.is_none() && !float {
        // Typst's missing vertical alignment means "at the current flow
        // position", not "at the top margin".
        AnchorPos {
            rel_from: "paragraph",
            align: None,
            offset: Some(crate::props::abs_to_emu(dy) + drawing.source_offset_emu[1]),
        }
    } else {
        anchor_axis(
            "margin",
            v_align,
            v_comp.map_or(Some(0.0), |alignment| {
                Some(match alignment {
                    VAlignment::Horizon => 0.5,
                    VAlignment::Bottom => 1.0,
                    VAlignment::Top => 0.0,
                })
            }),
            dy,
            reference_h,
            drawing.h_emu,
            drawing.source_offset_emu[1],
        )
    };

    let wrap = if float { AnchorWrap::TopAndBottom } else { AnchorWrap::None };
    let clearance =
        if float { crate::props::abs_to_emu(elem.clearance.resolve(styles)) } else { 0 };
    drawing.anchor = Some(Anchor {
        z: ctx.next_z(),
        pos_h,
        pos_v,
        wrap,
        dist: [clearance, clearance, 0, 0],
        behind: false,
    });
}

/// Builds one anchor axis. With no displacement Word keeps semantic alignment;
/// once `dx`/`dy` is nonzero, alignment and displacement are combined into one
/// exact offset because OOXML permits only one of `wp:align` and `wp:posOffset`.
fn anchor_axis(
    rel_from: &'static str,
    align: &'static str,
    factor: Option<f64>,
    displacement: Abs,
    reference: Abs,
    extent_emu: i64,
    source_offset_emu: i64,
) -> AnchorPos {
    if displacement == Abs::zero() && source_offset_emu == 0 {
        return AnchorPos { rel_from, align: Some(align), offset: None };
    }

    let reference_emu = crate::props::abs_to_emu(reference);
    let base = factor.unwrap_or(0.0) * (reference_emu - extent_emu) as f64;
    AnchorPos {
        rel_from,
        align: None,
        offset: Some(
            base.round() as i64
                + crate::props::abs_to_emu(displacement)
                + source_offset_emu,
        ),
    }
}

/// Builds the caption runs with a `SEQ` field carrying the number (G9).
///
/// When the figure is numbered, the caption is reconstructed as
/// `supplement` + number + `separator` + `body`. A Word `SEQ` field owns the
/// visible number only when its format is provably equivalent to Typst's
/// numbering pattern. Otherwise the exact Typst number stays as text and a
/// hidden `SEQ \h` advances Word's per-kind counter for list-of-figures support.
struct CaptionRuns {
    runs: Vec<Run>,
    number_range: Option<std::ops::Range<usize>>,
}

fn caption_runs(
    elem: &Packed<FigureElem>,
    cap: &Packed<typst_library::model::FigureCaption>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<CaptionRuns> {
    // Only emit SEQ for actually-numbered figures; otherwise plain realized text
    // (byte-identical to the previous behaviour for unnumbered captions).
    let Some(numbering) = elem.numbering.get_ref(styles) else {
        let realized = cap.realize(ctx.engine(), styles)?;
        let props = RunProps {
            review_origin: Some(ctx.review_origin(
                crate::convert::review_span(&cap.body),
                crate::dom::ReviewCandidateKind::InlineText,
            )),
            ..Default::default()
        };
        return Ok(CaptionRuns {
            runs: ctx.inline_runs(&realized, styles, props)?,
            number_range: None,
        });
    };

    let mut runs: Vec<Run> = Vec::new();

    // Supplement (e.g. "Figure") + the non-breaking space the realizer inserts.
    if let Some(Some(supplement)) = cap.supplement.clone()
        && !supplement.is_empty()
    {
        let mut sup = supplement;
        sup += TextElem::packed('\u{a0}');
        runs.extend(ctx.inline_runs(&sup, styles, RunProps::default())?);
    }

    // The realized number, used as the SEQ field's cached result so the caption
    // is readable before Word updates fields.
    let mut cache_unavailable = false;
    let (number_runs, number_text) =
        match (cap.counter.clone(), cap.numbering.clone(), cap.figure_location) {
            (Some(Some(counter)), Some(Some(numbering)), Some(Some(location))) => {
                // Best-effort: this number is only the SEQ field's *cached* result —
                // Word recomputes the live value on open/update. A user numbering
                // closure that reads introspection (querying headings, indexing
                // counter components) can fail against the empty first-iteration
                // introspector, or permanently when the state it wants only exists
                // in a paged model. Under paged layout that failure is a delayed
                // error that gets retried; propagating it here would hard-abort the
                // whole export on iteration one. An empty cached number degrades
                // gracefully instead (the field still renders in Word).
                match counter.display_at(
                    ctx.engine(),
                    location,
                    styles,
                    &numbering,
                    cap.span(),
                ) {
                    Ok(number) => (
                        ctx.inline_runs(&number, styles, RunProps::default())?,
                        number.plain_text(),
                    ),
                    Err(errors) => {
                        cache_unavailable = true;
                        let content = elem.clone().pack();
                        for diagnostic in errors {
                            ctx.suppress_content_diagnostic(
                                &content,
                                crate::report::ExportStage::FieldPlanning,
                                diagnostic,
                            );
                        }
                        ctx.record_content_decision(
                            &content,
                            Representation::Approximate,
                            DecisionReason::FieldCacheUnavailable,
                            LossSet::DYNAMIC_BEHAVIOR,
                            0,
                        );
                        (Vec::new(), EcoString::new())
                    }
                }
            }
            _ => (Vec::new(), EcoString::new()),
        };

    let number_cache_status = if cache_unavailable {
        FieldCacheStatus::Unavailable
    } else if number_runs.is_empty() {
        FieldCacheStatus::ConsumerRequired
    } else {
        FieldCacheStatus::Resolved
    };
    let seq = seq_name(elem, styles);
    let number_start = runs.len();
    if let Some(format) = word_seq_format(numbering, ctx, cap.span()) {
        // Word can exactly reproduce this single-component numeral system, so
        // preserve a genuinely live, editable caption sequence.
        runs.push(Run::Field(Field {
            instr: ecow::eco_format!(" SEQ {seq} \\* {format} "),
            result: number_runs,
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: number_cache_status,
        }));
    } else if number_runs.is_empty() {
        // If Typst itself could not evaluate a user numbering function, retain
        // the previous useful fallback: let Word provide a visible decimal.
        runs.push(Run::Field(Field {
            instr: ecow::eco_format!(" SEQ {seq} \\* ARABIC "),
            result: Vec::new(),
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: number_cache_status,
        }));
    } else if let Some((prefix, separator, suffix, heading_level)) =
        composite_caption_number(&number_text, ctx)
    {
        // A chapter/appendix prefix plus a decimal local counter maps to native
        // Word fields. STYLEREF follows the nearest heading prefix when that
        // semantic heading survived show-rule lowering; otherwise a scoped SEQ
        // owns the exact numeric/alphabetic prefix. A distinct per-scope SEQ
        // makes the local number independently editable and reorderable.
        let scope = word_field_identifier(prefix);
        let prefix_instr = if let Some(heading_level) = heading_level {
            ecow::eco_format!(" STYLEREF TypstHeadingNumber{heading_level} ")
        } else if let Ok(value) = prefix.parse::<u32>() {
            ecow::eco_format!(" SEQ TypstCaptionScope_{scope} \\r {value} \\* ARABIC ")
        } else {
            let value = prefix
                .chars()
                .next()
                .expect("composite prefix was validated")
                .to_ascii_uppercase() as u32
                - 'A' as u32
                + 1;
            ecow::eco_format!(
                " SEQ TypstCaptionScope_{scope} \\r {value} \\* ALPHABETIC "
            )
        };
        runs.push(Run::Field(Field {
            instr: prefix_instr,
            result: vec![Run::Text { props: RunProps::default(), text: prefix.into() }],
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::Resolved,
        }));
        runs.push(Run::Text { props: RunProps::default(), text: separator.into() });
        runs.push(Run::Field(Field {
            instr: ecow::eco_format!(" SEQ {seq}_TypstScope_{scope} \\* ARABIC "),
            result: vec![Run::Text { props: RunProps::default(), text: suffix.into() }],
            mode: FieldMode::Live,
            display: FieldDisplay::Visible,
            cache_status: FieldCacheStatus::Resolved,
        }));
    } else {
        // Prefixes/suffixes, multi-component patterns, and functions have no
        // generally equivalent SEQ instruction. Keep Typst's exact output and
        // advance a hidden Word counter once so later live captions and `TOC
        // \c` fields still see the correct sequence position.
        runs.extend(number_runs);
        runs.push(Run::Field(Field {
            instr: ecow::eco_format!(" SEQ {seq} \\h "),
            result: Vec::new(),
            mode: FieldMode::Live,
            display: FieldDisplay::Hidden,
            cache_status: FieldCacheStatus::ConsumerRequired,
        }));
        let content = elem.clone().pack();
        ctx.record_content_decision(
            &content,
            Representation::Approximate,
            DecisionReason::TypstOwnedFigureNumber,
            LossSet::DYNAMIC_BEHAVIOR,
            0,
        );
    }
    let number_end = runs.len();

    // Separator (e.g. ": "). Synthesized into the `separator` field by
    // `FigureCaption`'s `Synthesize` impl, so read it off the chain.
    if let Smart::Custom(sep) = cap.separator.get_cloned(styles) {
        runs.extend(ctx.inline_runs(&sep, styles, RunProps::default())?);
    }

    // Caption body. Keep its authored source span distinct from the generated
    // supplement/number/separator so Word edits cannot flatten those fields.
    let body_props = RunProps {
        review_origin: Some(ctx.review_origin(
            crate::convert::review_span(&cap.body),
            crate::dom::ReviewCandidateKind::InlineText,
        )),
        ..Default::default()
    };
    runs.extend(ctx.inline_runs(&cap.body, styles, body_props)?);

    Ok(CaptionRuns {
        runs,
        number_range: (number_end > number_start).then_some(number_start..number_end),
    })
}

/// Recognizes the common custom numbering shape `<heading>.<decimal>` and
/// identifies the live heading level that supplied the prefix. The separator is
/// retained verbatim, so this works for chapter and alphabetic appendix numbers
/// without hard-coding either numeral system.
fn composite_caption_number<'a>(
    number: &'a str,
    ctx: &DocxCtx,
) -> Option<(&'a str, &'a str, &'a str, Option<usize>)> {
    let split = number.rfind(['.', '-', ':'])?;
    let (prefix, tail) = number.split_at(split);
    let (separator, suffix) = tail.split_at(1);
    if prefix.is_empty()
        || suffix.is_empty()
        || !suffix.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if prefix.parse::<u32>().is_err()
        && !(prefix.len() == 1 && prefix.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return None;
    }
    let level = ctx.heading_level_for_number(prefix);
    Some((prefix, separator, suffix, level))
}

fn word_field_identifier(value: &str) -> EcoString {
    let id: EcoString = value
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if id.is_empty() { "Scope".into() } else { id }
}

/// A Word `SEQ` numeric-format switch when one field can exactly reproduce the
/// Typst numbering pattern. Decorations, multiple components, decimal padding,
/// and functions stay Typst-owned instead of being silently coerced.
fn word_seq_format(
    numbering: &Numbering,
    ctx: &mut DocxCtx,
    span: typst_syntax::Span,
) -> Option<&'static str> {
    let Numbering::Pattern(pattern) = numbering else { return None };
    if pattern.pieces() != 1 {
        return None;
    }
    let one = pattern.apply_kth(ctx.engine(), span, 0, 1);
    let four = pattern.apply_kth(ctx.engine(), span, 0, 4);
    match (one.as_str(), four.as_str()) {
        ("1", "4") => Some("ARABIC"),
        ("a", "d") => Some("alphabetic"),
        ("A", "D") => Some("ALPHABETIC"),
        ("i", "iv") => Some("roman"),
        ("I", "IV") => Some("ROMAN"),
        _ => None,
    }
}

/// Maps a figure's `kind` to a `SEQ` field name (a stable per-kind counter
/// identifier). Matches the Word convention `Figure`/`Table`/`Listing`.
pub(crate) fn seq_name(elem: &Packed<FigureElem>, styles: StyleChain) -> EcoString {
    use typst_library::foundations::NativeElement;
    use typst_library::model::TableElem;
    use typst_library::text::RawElem;
    match elem.kind.get_ref(styles) {
        Smart::Custom(FigureKind::Elem(func)) => {
            if *func == TableElem::ELEM {
                "Table".into()
            } else if *func == RawElem::ELEM {
                "Listing".into()
            } else {
                "Figure".into()
            }
        }
        Smart::Custom(FigureKind::Name(name)) => {
            // The name becomes a Word field-code identifier, so restrict it to
            // token-safe characters — a stray space/quote/backslash would break
            // the `SEQ` instruction grammar and make Word show "Error!". Keep
            // (Unicode) alphanumerics and underscores, map everything else to
            // `_`, and fall back to the generic counter if nothing usable
            // remains (e.g. an empty custom name).
            let id: EcoString = name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            if id.is_empty() { "Figure".into() } else { id }
        }
        Smart::Auto => "Figure".into(),
    }
}

/// Lowers a figure body to a single floating drawing (G8).
///
/// The body is lowered like any block, then the first picture it produces is
/// turned into a floating anchor (image body → the inline `pic:pic`; otherwise
/// the rasterized PNG fallback). `placement` chooses the vertical alignment
/// (`Auto`/`Top` → top, `Bottom` → bottom); horizontal is centered. The float
/// wraps text top-and-bottom (Word's figure convention). If the body produces
/// no drawing (e.g. a table figure), it falls back to the in-flow centered
/// paragraphs unchanged.
fn float_figure_body(
    elem: &Packed<FigureElem>,
    placement: Smart<VAlignment>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Block>> {
    let mut body_blocks = ctx.blocks(&elem.body, styles)?;

    // Find the first standalone drawing run in the lowered body.
    let drawing = take_first_drawing(&mut body_blocks);
    let Some(mut drawing) = drawing else {
        // Non-image body (table, multi-paragraph): keep it in flow, centered.
        for block in &mut body_blocks {
            match block {
                Block::Para(para) if para.props.jc.is_none() => {
                    para.props.jc = Some(Jc::Center);
                }
                Block::Table(table) if table.props.jc.is_none() => {
                    table.props.jc = Some(Jc::Center);
                }
                _ => {}
            }
        }
        return Ok(body_blocks);
    };

    let v_align = match placement {
        Smart::Custom(VAlignment::Bottom) => "bottom",
        // Auto / Top / Horizon → top.
        _ => "top",
    };
    let dist = EMU_PER_PT_I * 9; // ~9pt clearance around the float.
    drawing.anchor = Some(Anchor {
        z: ctx.next_z(),
        pos_h: AnchorPos {
            rel_from: "margin",
            align: Some("center"),
            offset: None,
        },
        pos_v: AnchorPos {
            rel_from: "margin",
            align: Some(v_align),
            offset: None,
        },
        wrap: AnchorWrap::TopAndBottom,
        dist: [0, 0, dist, dist],
        behind: false,
    });

    // The anchored drawing lives in its own paragraph; any other body blocks
    // (rare for a figure) stay after it.
    let mut out = Vec::with_capacity(body_blocks.len() + 1);
    out.push(Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    }));
    out.append(&mut body_blocks);
    Ok(out)
}

/// Removes and returns the first `Run::Drawing` found in `blocks`, leaving any
/// sibling content (e.g. introspection tags) in place so it isn't lost. If the
/// host paragraph is left empty after extraction, it is dropped. Returns `None`
/// if no drawing exists (the body has no native image — e.g. a table figure).
fn take_first_drawing(blocks: &mut [Block]) -> Option<Drawing> {
    for block in blocks.iter_mut() {
        if let Block::Para(para) = block {
            // Find the index of the (first) drawing child in this paragraph.
            let pos = para
                .content
                .iter()
                .position(|c| matches!(c, ParaChild::Run(Run::Drawing(_))));
            if let Some(pos) = pos {
                let child = para.content.remove(pos);
                if let ParaChild::Run(Run::Drawing(d)) = child {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// Renders an arbitrary laid-out element to a PNG and embeds it as an inline
/// picture — the generic fallback for any block the dispatch cannot represent
/// natively (custom layout, raw frames, boxes, …).
///
/// INTEGRATION-NEEDED: rasterizing a `Frame`/`Page` to PNG bytes requires
/// `typst-render` (→ `tiny-skia` pixmap → `Pixmap::encode_png`) **and** layout
/// of the element to a `Frame`. Neither `typst-render` nor an image encoder is
/// a dependency of `typst-docx`, and adding one edits `Cargo.toml`, which the
/// mapper phase may not touch. The intended integration is:
///
/// 1. Add `typst-render` (and `typst-layout` if the element must first be laid
///    out) to `crates/typst-docx/Cargo.toml`.
/// 2. Lay the element out to a single-page `Frame`/`PagedDocument` and call
///    `typst_render::render(&page, &RenderOptions { pixel_per_pt, .. })` to get
///    a `tiny_skia::Pixmap`, then `pixmap.encode_png()` for the bytes.
/// 3. Feed those bytes here: embed via `ctx.add_image(&png, "png")`, allocate a
///    `docpr_id`, and build the `Drawing` exactly as [`image`] does, using the
///    frame's point size for the extents.
///
/// Returns the drawing run followed by the frame-recovered text as hidden
/// (`w:vanish`) runs, or an empty `Vec` if the content lays out to nothing
/// (callers then fall back to `warn_ignored`).
pub fn laid_out_fallback(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    laid_out_fallback_with_reason(content, styles, ctx, DecisionReason::RasterFallback)
}

/// Whole-region raster fallback with an explicit capability-planning reason.
pub(crate) fn laid_out_fallback_with_reason(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
    reason: DecisionReason,
) -> SourceResult<Vec<Run>> {
    let Some((rel, size, text)) = ctx.rasterize(content, styles, content.span())? else {
        return Ok(Vec::new());
    };
    ctx.record_content_decision(
        content,
        Representation::Raster,
        reason,
        LossSet::RASTER,
        text.chars().count(),
    );
    Ok(fallback_runs(ctx, rel, size, &text))
}

/// Block-level raster fallback. Tall regions are split into page-sized images;
/// inline/table/positioned callers must use [`laid_out_fallback`] instead.
pub(crate) fn laid_out_block_fallback(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    laid_out_block_fallback_with_reason(
        content,
        styles,
        ctx,
        DecisionReason::RasterFallback,
    )
}

pub(crate) fn laid_out_block_fallback_with_reason(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
    reason: DecisionReason,
) -> SourceResult<Vec<Run>> {
    let Some((tiles, text)) = ctx.rasterize_tiled(content, styles, content.span())?
    else {
        return Ok(Vec::new());
    };
    ctx.record_content_decision(
        content,
        Representation::Raster,
        reason,
        LossSet::RASTER,
        text.chars().count(),
    );
    Ok(fallback_runs_tiled(ctx, tiles, &text))
}

/// Renders a coherent mixed placed canvas from the same converged frame used
/// to classify it, preserving the root's authored logical footprint rather
/// than expanding Word flow to the canvas's cropped ink bounds.
pub(crate) fn coherent_placed_canvas_fallback(
    content: &Content,
    frame: typst_library::layout::Frame,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    let Some((rel, size, text)) = ctx.rasterize_coherent_placed_canvas(content, frame)
    else {
        return Ok(Vec::new());
    };
    Ok(fallback_runs(ctx, rel, size, &text))
}

/// Same as [`laid_out_fallback`], but hands the laid-out frame's introspection
/// tags back to the caller instead of deferring them to the end of the
/// document. Paragraph-level callers use this to keep state/counter updates
/// ordered at their exact position.
pub fn laid_out_fallback_with_tags(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<(Vec<typst_library::introspection::Tag>, Vec<Run>, bool)> {
    let (tags, rasterized, failed) =
        ctx.rasterize_with_tags(content, styles, content.span())?;
    let Some((rel, size, text)) = rasterized else {
        return Ok((tags, Vec::new(), failed));
    };
    ctx.record_content_decision(
        content,
        Representation::Raster,
        DecisionReason::RasterFallback,
        LossSet::RASTER,
        text.chars().count(),
    );
    Ok((tags, fallback_runs(ctx, rel, size, &text), false))
}

/// Builds the drawing + hidden-text run sequence for a rasterized frame. The
/// image carries the exact visual; alongside it, the text recovered from the
/// laid-out frame is kept as HIDDEN runs (`w:vanish`), so the rasterized region
/// stays searchable, selectable, copy-pasteable, and screen-reader accessible
/// instead of being pure dead pixels. Recovered line boundaries are flattened
/// to spaces: a bare `<w:br/>` is layout-visible even beside vanished text and
/// can otherwise manufacture blank pages after a large fallback.
fn fallback_runs(
    ctx: &mut DocxCtx,
    rel: EcoString,
    size: typst_library::layout::Size,
    text: &str,
) -> Vec<Run> {
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");
    let mut runs = Vec::with_capacity(2);
    runs.push(Run::Drawing(Drawing {
        rel,
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu: crate::props::abs_to_emu(size.x),
        h_emu: crate::props::abs_to_emu(size.y),
        source_offset_emu: [0, 0],
        alt: Some(text.replace('\n', " ").into())
            .filter(|s: &EcoString| !s.trim().is_empty()),
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: None,
        pic_clip: PicClip::default(),
    }));
    hidden_text_runs(text, &mut runs);
    runs
}

fn fallback_runs_tiled(
    ctx: &mut DocxCtx,
    tiles: Vec<(EcoString, typst_library::layout::Size)>,
    text: &str,
) -> Vec<Run> {
    let mut runs = Vec::with_capacity(tiles.len() * 2 + 1);
    for (index, (rel, size)) in tiles.into_iter().enumerate() {
        if index > 0 {
            runs.push(Run::PageBreak);
        }
        let docpr_id = ctx.next_drawing_id();
        let name: EcoString = ecow::eco_format!("Picture {docpr_id}");
        runs.push(Run::Drawing(Drawing {
            rel,
            svg_rel: None,
            compatibility_split_ids: None,
            w_emu: crate::props::abs_to_emu(size.x),
            h_emu: crate::props::abs_to_emu(size.y),
            source_offset_emu: [0, 0],
            alt: if index == 0 {
                Some(text.replace('\n', " ").into())
                    .filter(|s: &EcoString| !s.trim().is_empty())
            } else {
                None
            },
            decorative: false,
            docpr_id,
            name,
            anchor: None,
            shape: None,
            group: None,
            pic_clip: PicClip::default(),
        }));
    }
    hidden_text_runs(text, &mut runs);
    runs
}

/// Appends the frame-recovered `text` as hidden (`w:vanish`) runs — the words
/// stay searchable/selectable but take no visual space beside the image.
/// Line separators become ordinary spaces inside the vanished run because
/// `Run::Break { .. }` has no run properties and therefore still consumes layout.
/// Leading/trailing spaces keep a boundary against adjacent visible runs
/// (otherwise a consumer concatenating run text — pandoc, Word's Find,
/// copy-paste — would glue e.g. `urbane` + `Stoicos` into one token).
fn hidden_text_runs(text: &str, out: &mut Vec<Run>) {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.is_empty() {
        return;
    }
    let hidden = RunProps { vanish: true, ..RunProps::default() };
    out.push(Run::Text {
        props: hidden,
        text: format!(" {flattened} ").into(),
    });
}

// ---------------------------------------------------------------------------
// Natively clipped pictures.
// ---------------------------------------------------------------------------

/// Recovers a clipped image container — `#box(radius: .., clip: true)[image]`,
/// the rounded avatar/card idiom — as a real Word picture instead of baking the
/// clip into a raster.
///
/// DrawingML expresses exactly this shape: the picture's `a:prstGeom` is its
/// outline (a `roundRect` whose `adj` guide carries the corner radius — at its
/// maximum, a circular crop), and `a:blipFill/a:srcRect` says which part of the
/// source image shows through it. The image therefore stays the
/// original, full-resolution, replaceable bytes — croppable and re-roundable in
/// Word — rather than a flattened bitmap of the clipped result.
///
/// `None` means "not this shape, rasterize as before". The classification is
/// deliberately narrow: the clip must be a preset outline, the container's
/// entire visible content must be one image, and that image must *cover* the
/// clip (a gap would expose a background a picture frame cannot paint).
pub fn clipped_image(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let (frame, _) =
        ctx.layout_export_frame(child, styles, child.span(), ctx.raster_height)?;
    let Some(frame) = frame else { return Ok(None) };
    let Some(picture) = clipped_picture(&frame) else { return Ok(None) };
    let Some(src_rect) = cover_src_rect(picture.pos, picture.size, picture.frame) else {
        return Ok(None);
    };
    // Only formats Word embeds verbatim can keep their own coordinate system
    // under `a:srcRect`; a re-rendered fallback would be cropped to its own ink
    // and make the source rectangle meaningless.
    let Some((bytes, ext)) = embeddable_bytes(picture.image) else {
        return Ok(None);
    };

    let w_emu = crate::props::abs_to_emu(picture.frame.x);
    let h_emu = crate::props::abs_to_emu(picture.frame.y);
    if w_emu <= 0 || h_emu <= 0 {
        return Ok(None);
    }

    ctx.record_content_decision(
        child,
        Representation::Native,
        DecisionReason::NativeClippedPicture,
        LossSet::default(),
        0,
    );
    ctx.defer_frame_tags(&frame);

    let rel = ctx.add_image(&bytes, &ext);
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel,
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt: picture.image.alt().map(Into::into),
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: None,
        pic_clip: PicClip {
            geom: picture.geom,
            src_rect: (src_rect != [0; 4]).then_some(src_rect),
        },
    })))
}

/// One image seen through a preset clip outline.
struct ClippedPicture<'a> {
    geom: PicGeom,
    /// The clip's extent, which is also the emitted picture's extent.
    frame: Size,
    image: &'a Image,
    /// The image's placement within the clip, in the clip's own coordinates.
    pos: Point,
    size: Size,
}

/// Finds the sole clipped picture in `frame`, descending through the
/// pass-through groups Typst nests around a box's content.
///
/// A `#box(clip: true)` lays out as a group carrying the clip curve; anything
/// else visible beside it (a fill, a border, a second image, text) means the
/// container is not just a framed picture and must keep the raster path.
fn clipped_picture(frame: &Frame) -> Option<ClippedPicture<'_>> {
    let mut found: Option<ClippedPicture<'_>> = None;
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) if found.is_none() => {
                let size = group.frame.size();
                if !is_translation(&group.transform) {
                    return None;
                }
                found = Some(match group.clip.as_ref().map(|c| clip_to_pic_geom(c, size))
                {
                    // A real clip outline: the image inside it is the payload.
                    Some(Some(geom @ PicGeom::RoundRect { .. })) => {
                        let (pos, image, size_of_image) =
                            single_frame_image(&group.frame)?;
                        ClippedPicture {
                            geom,
                            frame: size,
                            image,
                            pos,
                            size: size_of_image,
                        }
                    }
                    // A pass-through wrapper: no clip at all, or the plain
                    // bounding rectangle Typst puts around a box's content,
                    // neither of which removes anything. The real outline, if
                    // there is one, is further in.
                    None | Some(Some(PicGeom::Rect)) => clipped_picture(&group.frame)?,
                    // A clip that is not a preset outline.
                    Some(None) => return None,
                });
            }
            FrameItem::Tag(_) | FrameItem::Link(..) => {}
            _ => return None,
        }
    }
    found
}

/// Whether a group's clip (if any) is just its own bounding rectangle, which
/// removes nothing and so may be walked through.
fn clip_is_bounding_rect(group: &typst_library::layout::GroupItem) -> bool {
    group.clip.as_ref().is_none_or(|clip| *clip == Curve::rect(group.frame.size()))
}

/// Classifies a clip curve as a DrawingML preset picture outline.
fn clip_to_pic_geom(clip: &Curve, size: Size) -> Option<PicGeom> {
    if !size.x.to_pt().is_finite()
        || !size.y.to_pt().is_finite()
        || size.x.to_pt() <= 0.0
        || size.y.to_pt() <= 0.0
    {
        return None;
    }
    if *clip == Curve::rect(size) {
        return Some(PicGeom::Rect);
    }
    let radius = dml::rounded_rect_radius(clip, size)?;
    Some(PicGeom::RoundRect { adj_100k: dml::round_rect_adj(radius, size) })
}

/// The sole image in a frame, in that frame's coordinates. Pass-through groups
/// that only translate are walked into; anything that draws beside the image
/// (text, a second picture, an inked shape) disqualifies the frame.
fn single_frame_image(frame: &Frame) -> Option<(Point, &Image, Size)> {
    let mut found: Option<(Point, &Image, Size)> = None;
    for (pos, item) in frame.items() {
        match item {
            FrameItem::Image(image, size, _) if found.is_none() => {
                found = Some((*pos, image, *size));
            }
            FrameItem::Group(group) if found.is_none() => {
                if !is_translation(&group.transform) || !clip_is_bounding_rect(group) {
                    return None;
                }
                let (inner, image, size) = single_frame_image(&group.frame)?;
                let inner = inner.transform(group.transform);
                found = Some((Point::new(pos.x + inner.x, pos.y + inner.y), image, size));
            }
            FrameItem::Tag(_) | FrameItem::Link(..) => {}
            _ => return None,
        }
    }
    found
}

/// Whether a group transform is a pure translation, the only placement an
/// axis-aligned `pic:pic` can reproduce.
fn is_translation(transform: &typst_library::layout::Transform) -> bool {
    transform.sx.get() == 1.0
        && transform.sy.get() == 1.0
        && transform.kx.get() == 0.0
        && transform.ky.get() == 0.0
}

/// The `a:srcRect` insets `[left, top, right, bottom]` (1/1000 of a percent)
/// that reveal a `frame`-sized window of an image placed at `pos` with `size`.
///
/// `None` when the image does not fully cover the window: the picture frame has
/// no way to paint the exposed background, so those cases keep the raster path.
fn cover_src_rect(pos: Point, size: Size, frame: Size) -> Option<[i32; 4]> {
    let (ix, iy) = (pos.x.to_pt(), pos.y.to_pt());
    let (iw, ih) = (size.x.to_pt(), size.y.to_pt());
    let (fw, fh) = (frame.x.to_pt(), frame.y.to_pt());
    // A tenth of a point of slack absorbs the rounding that layout leaves on an
    // exactly-fitting image without admitting a visible gap.
    const EPS: f64 = 0.05;
    if iw <= EPS || ih <= EPS {
        return None;
    }
    if ix > EPS || iy > EPS || ix + iw < fw - EPS || iy + ih < fh - EPS {
        return None;
    }
    let frac = |amount: f64, span: f64| {
        ((amount.max(0.0) / span) * 100_000.0).round().clamp(0.0, 99_000.0) as i32
    };
    Some([frac(-ix, iw), frac(-iy, ih), frac(ix + iw - fw, iw), frac(iy + ih - fh, ih)])
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Returns the bytes + lowercase extension to embed for an image, or `None` if
/// the format needs rasterization that is not yet available.
///
/// Raster *exchange* formats (PNG/JPEG/GIF) are embedded verbatim — no
/// re-encode, preserving fidelity. SVG is handled by [`image`] before this
/// helper so it can carry both a native SVG part and a PNG fallback. WebP and
/// PDF return `None` and stay on the raster fallback.
fn embeddable_bytes(image: &Image) -> Option<(Vec<u8>, EcoString)> {
    let embeddable = media::embeddable_image_bytes(image)?;
    Some((embeddable.bytes.to_vec(), embeddable.ext.into()))
}

fn svg_image(image: &Image) -> Option<&SvgImage> {
    match image.kind() {
        ImageKind::Svg(svg) => Some(svg),
        _ => None,
    }
}

/// Computes the inline display extents `(cx, cy)` in EMU.
///
/// Resolution order, per axis:
/// 1. an explicit or relative `width`/`height` on the element → resolved
///    against the current lowering container;
/// 2. otherwise the image's intrinsic point-size (`pixels / dpi × 72`), scaled
///    proportionally if the *other* axis was given absolutely (preserving the
///    aspect ratio — Word's `noChangeAspect` lock assumes the extents already
///    match the picture).
///
fn display_extents(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    image: &Image,
    ctx: &DocxCtx,
) -> (i64, i64) {
    // Intrinsic point dimensions from pixels + DPI.
    let dpi = image.dpi().unwrap_or(Image::DEFAULT_DPI).max(1.0);
    let intrinsic_w_pt = image.width() / dpi * 72.0;
    let intrinsic_h_pt = image.height() / dpi * 72.0;
    let aspect = if intrinsic_w_pt > 0.0 { intrinsic_h_pt / intrinsic_w_pt } else { 1.0 };

    // Resolve an explicit/relative width against the current container.
    let abs_w: Option<Abs> = match elem.width.get(styles) {
        Smart::Custom(rel) => Some(rel.resolve(styles).relative_to(ctx.available_width)),
        _ => None,
    };
    // Resolve an explicit/relative height against the current container — the
    // measured cell/row box when this image sits inside one
    // (`shape_height_base`, scoped by `with_shape_height_base`), else the
    // page. Using the page unconditionally turned a poster header's
    // `image(height: 120%)` logo (120% of its actual ~13%-of-page cell) into
    // a multi-thousand-point image, same as `handle_block_box`'s analogous
    // `fixed_height` bug just below the point this mirrors.
    let abs_h: Option<Abs> = match elem.height.get(styles) {
        Sizing::Rel(rel) => Some(
            rel.resolve(styles)
                .relative_to(ctx.shape_height_base.unwrap_or(ctx.available_height)),
        ),
        _ => None,
    };

    let (w_pt, h_pt) = match (abs_w, abs_h) {
        // Both explicit: use as given (may distort — mirrors `fit: stretch`).
        (Some(w), Some(h)) => (w.to_pt(), h.to_pt()),
        // Width only: scale height to preserve aspect ratio.
        (Some(w), None) => (w.to_pt(), w.to_pt() * aspect),
        // Height only: scale width to preserve aspect ratio.
        (None, Some(h)) => {
            let w = if aspect > 0.0 { h.to_pt() / aspect } else { h.to_pt() };
            (w, h.to_pt())
        }
        // Neither: Typst contains the image's natural size inside the current
        // region while preserving its aspect ratio. Bounding both axes matters
        // for portrait images: Word otherwise expands them to the column width
        // and clips most of their height at the page boundary.
        (None, None) => {
            let width_scale = if intrinsic_w_pt > 0.0 {
                ctx.available_width.to_pt() / intrinsic_w_pt
            } else {
                1.0
            };
            let height_scale = if intrinsic_h_pt > 0.0 {
                // Contain against the enclosing box's measured height where
                // there is one (a sized block, a table cell), matching the
                // explicit-height branch above; else the page. Without this an
                // unsized image inside `block(height: ..)` ignores that bound.
                ctx.shape_height_base.unwrap_or(ctx.available_height).to_pt()
                    / intrinsic_h_pt
            } else {
                1.0
            };
            let scale = 1.0_f64.min(width_scale).min(height_scale).max(0.0);
            (intrinsic_w_pt * scale, intrinsic_h_pt * scale)
        }
    };

    // Guard against degenerate zero/negative extents (Word rejects `cx="0"`).
    let cx = ((w_pt * EMU_PER_PT).round() as i64).max(1);
    let cy = ((h_pt * EMU_PER_PT).round() as i64).max(1);
    (cx, cy)
}

/// Attaches a bookmark `(id, name)` bracketing the given blocks.
///
/// The `bookmarkStart` is prepended to the first paragraph and the matching
/// `bookmarkEnd` appended to the last paragraph. If there is no paragraph (the
/// figure produced only tables, or nothing), a fresh empty paragraph carrying a
/// zero-length bookmark is inserted — a legal cross-reference target.
fn attach_bookmark(blocks: &mut Vec<Block>, id: u32, name: EcoString) {
    // Find the first and last paragraph indices.
    let first_para = blocks.iter().position(|b| matches!(b, Block::Para(_)));
    let last_para = blocks.iter().rposition(|b| matches!(b, Block::Para(_)));

    match (first_para, last_para) {
        (Some(first), Some(last)) => {
            if let Block::Para(p) = &mut blocks[first] {
                p.content.insert(0, ParaChild::BookmarkStart { id, name });
            }
            if let Block::Para(p) = &mut blocks[last] {
                p.content.push(ParaChild::BookmarkEnd { id });
            }
        }
        _ => {
            // No paragraph to bracket: emit a zero-length bookmark paragraph at
            // the front so the target still exists.
            blocks.insert(
                0,
                Block::Para(Para {
                    props: ParaProps::default(),
                    content: vec![
                        ParaChild::BookmarkStart { id, name },
                        ParaChild::BookmarkEnd { id },
                    ],
                }),
            );
        }
    }
}

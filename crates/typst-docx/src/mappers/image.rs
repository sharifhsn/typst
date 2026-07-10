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
use typst_library::layout::{Abs, OuterVAlignment, Sizing, VAlignment};
use typst_library::model::{FigureElem, FigureKind, Numbering};
use typst_library::text::TextElem;
use typst_library::visualize::{Image, ImageElem, ImageKind, SvgImage};
use typst_ooxml_core::media;

use crate::ctx::DocxCtx;
use crate::dom::{
    Anchor, AnchorPos, AnchorWrap, Block, Drawing, Field, FieldCacheStatus, FieldDisplay,
    FieldMode, Jc, Para, ParaChild, ParaProps, Run, RunProps, TextBoxWrap,
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
                w_emu: crate::props::abs_to_emu(size.x),
                h_emu: crate::props::abs_to_emu(size.y),
                alt,
                decorative: false,
                docpr_id,
                name,
                anchor: None,
                shape: None,
                group: None,
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
    let (w_emu, h_emu) = display_extents(elem, styles, &decoded);

    // A unique, ≥1 non-visual id (Word repairs on duplicate `wp:docPr` ids).
    let docpr_id = ctx.next_drawing_id();
    let name: EcoString = ecow::eco_format!("Picture {docpr_id}");

    // Alt text → `descr` for accessibility.
    let alt = elem.alt.get_cloned(styles);

    Ok(Run::Drawing(Drawing {
        rel,
        svg_rel: None,
        w_emu,
        h_emu,
        alt,
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: None,
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
    let mut blocks: Vec<Block> = Vec::new();

    // Register a bookmark so cross-references to this figure resolve. We bracket
    // the whole figure (caption + body) with the start/end markers, attaching
    // them to the first and last emitted paragraphs.
    let bookmark = elem.location().map(|loc| ctx.add_bookmark(loc));

    // G9: build the caption with a `SEQ` field for the number (so Word
    // auto-renumbers) instead of a baked-in static counter value.
    let caption = elem.caption.get_cloned(styles);
    let (position, caption_blocks) = match &caption {
        Some(cap) => {
            let position = cap.position.get(styles);
            let runs = caption_runs(elem, cap, styles, ctx)?;
            let props = ParaProps {
                style: Some(CAPTION_STYLE.into()),
                ..Default::default()
            };
            let para = Para {
                props,
                content: runs.into_iter().map(ParaChild::Run).collect(),
            };
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
        let mut body_blocks = blocks?;
        for block in &mut body_blocks {
            if let Block::Para(para) = block
                && para.props.jc.is_none()
            {
                para.props.jc = Some(Jc::Center);
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
    // Best-effort: realizing a standalone caption runs the user's numbering
    // closure, which can fail against the empty first-iteration introspector
    // (see `caption_runs`) — skip the caption rather than abort the export.
    let Ok(realized) = elem.realize(ctx.engine(), styles) else {
        return Ok(Vec::new());
    };
    let runs = ctx.inline_runs(&realized, styles, RunProps::default())?;
    if runs.is_empty() {
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
    let plan = preflight_place(body, styles);

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
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedDrawing,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing(drawing)]);
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
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedTextBox,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing(drawing)]);
    }

    // Lower the body like any block once.
    let mut blocks = ctx.blocks(body, styles)?;

    // A single standalone drawing (a bare image, or a canvas/visual body we
    // rasterized) → anchor it at the place position. Guarded to SOLELY one
    // drawing so a richer body (several lowered blocks) isn't silently
    // truncated to its first drawing.
    let is_solely_one_drawing = matches!(
        blocks.as_slice(),
        [Block::Para(p)] if matches!(p.content.as_slice(), [ParaChild::Run(Run::Drawing(_))])
    );
    if is_solely_one_drawing && let Some(mut drawing) = take_first_drawing(&mut blocks) {
        set_place_anchor(&mut drawing, elem, styles, ctx);
        ctx.record_content_decision(
            &placed,
            Representation::Native,
            DecisionReason::PositionedDrawing,
            LossSet::default(),
            0,
        );
        return Ok(vec![para_drawing(drawing)]);
    }

    // Real block content (figure body + caption, table, text) → flow it in
    // place, keeping the text live, instead of rasterizing the whole body to a
    // flat (text-dead) image. A float is Typst's own "reflow to region
    // top/bottom", a clean semantic match; a non-float positioned overlay
    // loses its exact position this way, but preserving the (usually far more
    // valuable) text beats a positioned-but-dead raster. Genuinely visual
    // placed content — a bare shape, a canvas — lowered to a single drawing
    // above and never reaches here.
    if !blocks.is_empty() {
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
            Ok(vec![Block::Para(Para { props: ParaProps::default(), content })])
        }
        _ => Ok(Vec::new()),
    }
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
fn para_drawing(drawing: Drawing) -> Block {
    Block::Para(Para {
        props: ParaProps::default(),
        content: vec![ParaChild::Run(Run::Drawing(drawing))],
    })
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
            offset: Some(crate::props::abs_to_emu(dy)),
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
) -> AnchorPos {
    if displacement == Abs::zero() {
        return AnchorPos { rel_from, align: Some(align), offset: None };
    }

    let reference_emu = crate::props::abs_to_emu(reference);
    let base = factor.unwrap_or(0.0) * (reference_emu - extent_emu) as f64;
    AnchorPos {
        rel_from,
        align: None,
        offset: Some(base.round() as i64 + crate::props::abs_to_emu(displacement)),
    }
}

/// Builds the caption runs with a `SEQ` field carrying the number (G9).
///
/// When the figure is numbered, the caption is reconstructed as
/// `supplement` + number + `separator` + `body`. A Word `SEQ` field owns the
/// visible number only when its format is provably equivalent to Typst's
/// numbering pattern. Otherwise the exact Typst number stays as text and a
/// hidden `SEQ \h` advances Word's per-kind counter for list-of-figures support.
fn caption_runs(
    elem: &Packed<FigureElem>,
    cap: &Packed<typst_library::model::FigureCaption>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Vec<Run>> {
    // Only emit SEQ for actually-numbered figures; otherwise plain realized text
    // (byte-identical to the previous behaviour for unnumbered captions).
    let Some(numbering) = elem.numbering.get_ref(styles) else {
        let realized = cap.realize(ctx.engine(), styles)?;
        return ctx.inline_runs(&realized, styles, RunProps::default());
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
    let number_runs =
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
                    Ok(number) => {
                        ctx.inline_runs(&number, styles, RunProps::default())?
                    }
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
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        };

    let number_cache_status = if cache_unavailable {
        FieldCacheStatus::Unavailable
    } else if number_runs.is_empty() {
        FieldCacheStatus::ConsumerRequired
    } else {
        FieldCacheStatus::Resolved
    };
    let seq = seq_name(elem, styles);
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

    // Separator (e.g. ": "). Synthesized into the `separator` field by
    // `FigureCaption`'s `Synthesize` impl, so read it off the chain.
    if let Smart::Custom(sep) = cap.separator.get_cloned(styles) {
        runs.extend(ctx.inline_runs(&sep, styles, RunProps::default())?);
    }

    // Caption body.
    runs.extend(ctx.inline_runs(&cap.body, styles, RunProps::default())?);

    Ok(runs)
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
            if let Block::Para(para) = block
                && para.props.jc.is_none()
            {
                para.props.jc = Some(Jc::Center);
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

/// Same as [`laid_out_fallback`], but hands the laid-out frame's introspection
/// tags back to the caller instead of deferring them to the end of the
/// document. Paragraph-level callers use this to keep state/counter updates
/// ordered at their exact position.
pub fn laid_out_fallback_with_tags(
    content: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<(Vec<typst_library::introspection::Tag>, Vec<Run>)> {
    let (tags, rasterized) = ctx.rasterize_with_tags(content, styles, content.span())?;
    let Some((rel, size, text)) = rasterized else {
        return Ok((tags, Vec::new()));
    };
    ctx.record_content_decision(
        content,
        Representation::Raster,
        DecisionReason::RasterFallback,
        LossSet::RASTER,
        text.chars().count(),
    );
    Ok((tags, fallback_runs(ctx, rel, size, &text)))
}

/// Builds the drawing + hidden-text run sequence for a rasterized frame. The
/// image carries the exact visual; alongside it, the text recovered from the
/// laid-out frame is kept as HIDDEN runs (`w:vanish`), so the rasterized region
/// stays searchable, selectable, copy-pasteable, and screen-reader accessible
/// instead of being pure dead pixels. Line breaks in the recovered text become
/// `<w:br/>`s within the hidden run sequence.
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
        w_emu: crate::props::abs_to_emu(size.x),
        h_emu: crate::props::abs_to_emu(size.y),
        alt: Some(text.replace('\n', " ").into())
            .filter(|s: &EcoString| !s.trim().is_empty()),
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: None,
    }));
    hidden_text_runs(text, &mut runs);
    runs
}

/// Appends the frame-recovered `text` as hidden (`w:vanish`) runs — the words
/// stay searchable/selectable but take no visual space beside the image.
/// `\n` line separators become `Run::Break`s. The block is bracketed with
/// hidden spaces so its first/last words keep a boundary against any adjacent
/// visible run (otherwise a consumer concatenating run text — pandoc, Word's
/// Find, copy-paste — would glue e.g. `urbane` + `Stoicos` into one token).
fn hidden_text_runs(text: &str, out: &mut Vec<Run>) {
    if text.trim().is_empty() {
        return;
    }
    let hidden = RunProps { vanish: true, ..RunProps::default() };
    out.push(Run::Text { props: hidden.clone(), text: " ".into() });
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(Run::Break);
        }
        if !line.is_empty() {
            out.push(Run::Text { props: hidden.clone(), text: line.into() });
        }
    }
    out.push(Run::Text { props: hidden.clone(), text: " ".into() });
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
/// 1. an explicit *absolute* `width`/`height` on the element → used directly;
/// 2. otherwise the image's intrinsic point-size (`pixels / dpi × 72`), scaled
///    proportionally if the *other* axis was given absolutely (preserving the
///    aspect ratio — Word's `noChangeAspect` lock assumes the extents already
///    match the picture).
///
/// Relative (`%`) and fractional (`fr`) sizes have no fixed value without
/// layout, so they fall back to the intrinsic size.
fn display_extents(
    elem: &Packed<ImageElem>,
    styles: StyleChain,
    image: &Image,
) -> (i64, i64) {
    // Font size, to resolve any `em` component of an absolute length.
    let font_size = styles.resolve(TextElem::size);

    // Intrinsic point dimensions from pixels + DPI.
    let dpi = image.dpi().unwrap_or(Image::DEFAULT_DPI).max(1.0);
    let intrinsic_w_pt = image.width() / dpi * 72.0;
    let intrinsic_h_pt = image.height() / dpi * 72.0;
    let aspect = if intrinsic_w_pt > 0.0 { intrinsic_h_pt / intrinsic_w_pt } else { 1.0 };

    // Resolve an explicit absolute width, if any.
    let abs_w: Option<Abs> = match elem.width.get(styles) {
        Smart::Custom(rel) if rel.rel.is_zero() => Some(rel.abs.at(font_size)),
        _ => None,
    };
    // Resolve an explicit absolute height, if any.
    let abs_h: Option<Abs> = match elem.height.get(styles) {
        Sizing::Rel(rel) if rel.rel.is_zero() => Some(rel.abs.at(font_size)),
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
        // Neither: intrinsic size.
        (None, None) => (intrinsic_w_pt, intrinsic_h_pt),
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

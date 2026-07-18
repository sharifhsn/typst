//! Vector-shape mapper: decorative shapes (`#rect`, `#square`, `#ellipse`,
//! `#circle`, `#polygon`) → a DrawingML `wps:wsp` *vector* shape instead of a
//! rasterized image.
//!
//! A shape is mapped only when it is purely decorative (no body) and has an
//! explicit, representable size, fill and stroke — solid colours, DrawingML
//! gradients, or raster-backed DrawingML tile fills, no auto/fractional size.
//! Anything else returns `None`, and the caller rasterizes it so the visual is
//! still preserved.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Resolve, Smart, StyleChain};
use typst_library::layout::{Abs, BoxElem, Length, Rel, Sides, Sizing};
use typst_library::visualize::{
    CircleElem, EllipseElem, Paint, PolygonElem, RectElem, SquareElem, Stroke, Tiling,
};
use typst_ooxml_core::dml::{self, TileImage};

use crate::ctx::DocxCtx;
use crate::dom::{
    Block, Drawing, GroupChild, GroupSpec, Para, ParaChild, ParaProps, Run, RunProps,
    ShapeFill, ShapeGeom, ShapeSpec, ShapeStroke, TextBox, TextBoxWrap,
};
use crate::props::{abs_to_emu, color_to_hex};
use crate::report::{DecisionReason, LossSet, Representation};

fn opaque(rgb: [u8; 3]) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], 255]
}

fn rgb(rgba: [u8; 4]) -> [u8; 3] {
    [rgba[0], rgba[1], rgba[2]]
}

/// Maps a shape element to a vector DrawingML shape run, or `None` to rasterize.
pub fn shape(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let width_base = ctx.available_width;
    let height_base = ctx.shape_height_base;
    let Some((w, h, spec)) = build(ctx, child, styles, width_base, height_base) else {
        return Ok(None);
    };
    let (w_emu, h_emu) = (abs_to_emu(w), abs_to_emu(h));
    if w_emu <= 0 || h_emu <= 0 {
        return Ok(None);
    }
    let docpr_id = ctx.next_drawing_id();
    let name = ecow::eco_format!("Shape {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: true,
        docpr_id,
        name,
        anchor: None,
        shape: Some(spec),
        group: None,
    })))
}

use ecow::EcoString;

/// Maps a framed container with a body — a styled `#box(fill|stroke)[text]`, or
/// a `#rect`/`#square` carrying content — to a Word *text box*: a `wps:wsp` shape
/// whose `wps:txbx` holds the real, editable text, instead of rasterizing it to a
/// flat image. Returns `None` when the container is not a text-box candidate (no
/// visible frame, an unrepresentable fill, a body with layout-only
/// introspection, or a body that lays out to nothing), so the caller falls
/// through to its normal rasterize/extract handling.
///
/// The shape's extent is the laid-out size (`ctx.measure`); the inset becomes the
/// text-frame insets; the fill/stroke/rounded-corners become the shape's `spPr`.
/// The text is extracted via [`DocxCtx::blocks`] and its introspection tags are
/// forwarded so cross-references inside the box resolve.
pub fn text_box(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    // A footnote is a separate flowing story. Keep framed content there as
    // ordinary editable runs (the caller's fallback) instead of nesting a WPS
    // text box in `footnotes.xml`. LibreOffice can enter a layout loop when a
    // footnote contains these text boxes and the main story later contains a
    // table; inline raw/code spans are a common real-world trigger.
    if ctx.in_footnote {
        return Ok(None);
    }

    let Some(framed) = framed_container(child, styles) else {
        return Ok(None);
    };
    let Framed {
        body,
        fill_paint,
        stroke,
        rounded,
        inset,
        nonuniform_stroke: _,
    } = framed;

    // Text boxes use the same DrawingML shape fill vocabulary as decorative
    // shapes. Preserve native linear gradients and raster-backed tilings while
    // declining only paints that the shared fill mapper cannot represent.
    let Some(fill) = fill_color(ctx, &fill_paint) else {
        return Ok(None);
    };
    // Need a visible frame: a fill or a (representable) stroke. A bare inline box
    // has neither and keeps its existing handling; a `#rect`/`#square` always has
    // at least the default border.
    if fill.is_none() && stroke.is_none() {
        return Ok(None);
    }
    // Layout-only introspection (a per-line equation label) cannot survive native
    // extraction; such a body must rasterize.
    if !crate::convert::body_extractable(&body) {
        return Ok(None);
    }
    // A footnote is illegal inside a Word text box (the file fails to open), so a
    // footnote-bearing body is not a text-box candidate; the caller keeps it in
    // the main story instead (a shaded paragraph, or frameless inline extraction).
    if crate::convert::body_has_footnote(&body) {
        return Ok(None);
    }
    // Figures/images/tables/math/nested frames are Word-fragile inside a text box,
    // and a counter element inside one is laid out twice (measure + extract),
    // which corrupts cross-reference numbers. Such a body takes the
    // shaded-paragraph path instead, which lays out once and is correct.
    if !crate::convert::body_textbox_safe(&body) {
        return Ok(None);
    }

    // Size the frame from the laid-out container; bail to rasterization if it lays
    // out to nothing usable.
    let Some(size) = ctx.measure(child, styles, child.span())? else {
        return Ok(None);
    };
    let (w_emu, h_emu) = (abs_to_emu(size.x), abs_to_emu(size.y));
    if w_emu <= 0 || h_emu <= 0 {
        return Ok(None);
    }

    // Extract the real text and forward its introspection tags (cites/refs/labels
    // inside the box must still reach the introspector — the text box is opaque
    // to the IR tag-collection walk).
    let mut blocks = ctx.blocks(&body, styles)?;
    crate::mappers::table::collapse_par_spacing(&mut blocks);
    crate::document::collect_tags(&blocks, &mut ctx.deferred_tags);

    let geom = if rounded { ShapeGeom::RoundRect } else { ShapeGeom::Rect };
    let ins = resolve_insets(&inset, styles, size);

    let docpr_id = ctx.next_drawing_id();
    let name = ecow::eco_format!("Text Box {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: Some(ShapeSpec {
            geom,
            fill,
            stroke,
            txbx: Some(TextBox {
                ins,
                blocks,
                wrap: TextBoxWrap::Square,
                autofit: true,
            }),
        }),
        group: None,
    })))
}

/// Maps plain, text-box-safe placed content to an unframed Word text box. This
/// is separate from [`text_box`]: `#place[..]` contributes position but no
/// visible frame, so the shape deliberately has neither fill nor stroke.
pub fn unframed_text_box(
    body: &Content,
    styles: StyleChain,
    wrap: TextBoxWrap,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    if ctx.in_footnote
        || !crate::convert::body_extractable(body)
        || crate::convert::body_has_footnote(body)
    {
        return Ok(None);
    }

    let Some(size) = ctx.measure(body, styles, body.span())? else {
        return Ok(None);
    };
    let (w_emu, h_emu) = (abs_to_emu(size.x), abs_to_emu(size.y));
    if w_emu <= 0 || h_emu <= 0 {
        return Ok(None);
    }

    let mut blocks = ctx.blocks(body, styles)?;
    // Some positioned bodies are intrinsically inline-only. In particular,
    // `text(..)[counter(page).display()]` realizes to a live PAGE field but
    // does not manufacture a block paragraph. Keep using the block pipeline
    // first (it preserves real multi-paragraph structure), then synthesize a
    // single text-box paragraph from the shared inline mapper when there was
    // no block-level output. This is a general extraction boundary, not a
    // page-counter special case: links, styled text, and other legal inline
    // fields follow the same path.
    if blocks.is_empty() {
        let runs = ctx.inline_runs(body, styles, RunProps::default())?;
        if runs.is_empty() {
            return Ok(None);
        }
        blocks.push(Block::Para(Para {
            props: ParaProps::default(),
            content: runs.into_iter().map(ParaChild::Run).collect(),
        }));
    }
    crate::mappers::table::collapse_par_spacing(&mut blocks);
    crate::document::collect_tags(&blocks, &mut ctx.deferred_tags);

    let docpr_id = ctx.next_drawing_id();
    let name = ecow::eco_format!("Placed Text Box {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: false,
        docpr_id,
        name,
        anchor: None,
        shape: Some(ShapeSpec {
            geom: ShapeGeom::Rect,
            fill: None,
            stroke: None,
            txbx: Some(TextBox { ins: [0; 4], blocks, wrap, autofit: true }),
        }),
        group: None,
    })))
}

/// Resolves an *inline* framed container (`#box`/`#rect`/`#square` mid-line) to
/// `(body, run-shading fill, run border)` — the building blocks of boxed inline
/// text via `<w:shd>` + `<w:bdr>`, which (unlike an inline text box) flows
/// correctly within the line in Word. Returns `None` for a non-framed element or
/// a bodyless one; a gradient fill is dropped (no inline gradient form).
#[allow(clippy::type_complexity)]
pub fn inline_frame(
    child: &Content,
    styles: StyleChain,
) -> Option<(Content, Option<[u8; 3]>, Option<crate::dom::ParaBorder>)> {
    let framed = framed_container(child, styles)?;
    // A non-uniform per-side stroke (`box(stroke: (bottom: ..))`, the common
    // "border as a section-title underline" idiom) has no run-level form: the
    // character border (`w:bdr`) built below is inherently uniform around all
    // four sides, so a bottom-only stroke would silently become a full box.
    // Decline so the caller falls through to the block dispatch (a paragraph-
    // wrapped sole child, `convert::paragraph_sole_block_container`) or the
    // rasterize fallback (genuinely mid-line), both of which render it
    // correctly.
    if framed.nonuniform_stroke {
        return None;
    }
    // A gradient fill approximates to its first stop's colour (as the block box
    // path does, COVERAGE.md §7.1h) so a gradient-filled inline box — e.g. a
    // code-line highlight from a listing package — becomes run-shaded live text
    // instead of rasterizing; a tiling has no flat analogue and drops to no
    // shade.
    let fill = match framed.fill_paint {
        Some(Paint::Solid(c)) => Some(color_to_hex(&c)),
        Some(Paint::Gradient(g)) => crate::props::gradient_shade_hex(&g),
        _ => None,
    };
    let bdr = framed.stroke.map(|s| {
        let pt = s.w_emu as f64 / 12700.0;
        crate::dom::ParaBorder {
            style: "single",
            // `w:bdr/@w:sz` is in eighths of a point; keep a visible minimum.
            sz: ((pt * 8.0).round() as u32).max(2),
            space: 0,
            color: rgb(s.color),
        }
    });
    Some((framed.body, fill, bdr))
}

/// The frame properties a `#box`/`#rect`/`#square` with a body contributes to a
/// text box (the `stroke` already resolved to a uniform [`ShapeStroke`]).
struct Framed {
    body: Content,
    fill_paint: Option<Paint>,
    stroke: Option<ShapeStroke>,
    /// Whether the ORIGINAL per-side stroke (before reduction to a uniform
    /// [`ShapeStroke`]) had some sides set and others not — see
    /// [`inline_frame`]'s use of this.
    nonuniform_stroke: bool,
    rounded: bool,
    inset: Sides<Option<Rel<Length>>>,
}

/// Extracts the common [`Framed`] view from a `#box`/`#rect`/`#square` that has a
/// body, or `None` for any other element (or a bodyless one).
fn framed_container(child: &Content, styles: StyleChain) -> Option<Framed> {
    if let Some(e) = child.to_packed::<BoxElem>() {
        let body = e.body.get_cloned(styles)?;
        // A box's stroke has no implicit default: only an explicit side counts.
        let raw_stroke = e.stroke.get_cloned(styles);
        let stroke = sides_stroke_first(&raw_stroke, styles);
        Some(Framed {
            body,
            fill_paint: e.fill.get_cloned(styles),
            stroke,
            nonuniform_stroke: crate::convert::stroke_sides_nonuniform(&raw_stroke),
            rounded: any_radius(&e.radius.get_cloned(styles)),
            inset: e.inset.get_cloned(styles),
        })
    } else if let Some(e) = child.to_packed::<RectElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        let raw_stroke = e.stroke.get_cloned(styles);
        Some(Framed {
            stroke: shape_stroke(raw_stroke.clone(), &fill, styles),
            body,
            fill_paint: fill,
            nonuniform_stroke: matches!(&raw_stroke, Smart::Custom(sides)
                if crate::convert::stroke_sides_nonuniform(sides)),
            rounded: any_radius(&e.radius.get_cloned(styles)),
            inset: e.inset.get_cloned(styles),
        })
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        let raw_stroke = e.stroke.get_cloned(styles);
        Some(Framed {
            stroke: shape_stroke(raw_stroke.clone(), &fill, styles),
            body,
            fill_paint: fill,
            nonuniform_stroke: matches!(&raw_stroke, Smart::Custom(sides)
                if crate::convert::stroke_sides_nonuniform(sides)),
            rounded: any_radius(&e.radius.get_cloned(styles)),
            inset: e.inset.get_cloned(styles),
        })
    } else {
        None
    }
}

/// A `#rect`/`#square` stroke (`Smart` per-side): `Auto` is the Typst default (a
/// 1pt black outline when unfilled, else none); an explicit set uses its first
/// present side.
fn shape_stroke(
    stroke: Smart<Sides<Option<Option<Stroke>>>>,
    fill: &Option<Paint>,
    styles: StyleChain,
) -> Option<ShapeStroke> {
    match stroke {
        Smart::Auto => default_stroke(fill),
        Smart::Custom(sides) => sides_stroke_first(&sides, styles),
    }
}

/// Reduces a per-side stroke to one uniform [`ShapeStroke`] (the first present
/// side), or `None` for no/unrepresentable stroke.
fn sides_stroke_first(
    sides: &Sides<Option<Option<Stroke>>>,
    styles: StyleChain,
) -> Option<ShapeStroke> {
    for side in [&sides.top, &sides.right, &sides.bottom, &sides.left] {
        if let Some(Some(stroke)) = side {
            return resolve_stroke(stroke.clone(), styles);
        }
    }
    None
}

/// Whether any corner has a nonzero radius (→ a `roundRect` frame).
fn any_radius(radius: &typst_library::layout::Corners<Option<Rel<Length>>>) -> bool {
    [radius.top_left, radius.top_right, radius.bottom_left, radius.bottom_right]
        .into_iter()
        .any(|c| c.is_some_and(|r| !r.is_zero()))
}

/// Resolves a container's inset to text-frame insets `[left, top, right, bottom]`
/// in EMU (percentages relative to the laid-out size).
fn resolve_insets(
    inset: &Sides<Option<Rel<Length>>>,
    styles: StyleChain,
    size: typst_library::layout::Size,
) -> [i64; 4] {
    let resolve = |opt: Option<Rel<Length>>, base: Abs| -> i64 {
        opt.map(|r| abs_to_emu(r.resolve(styles).relative_to(base)))
            .unwrap_or(0)
    };
    [
        resolve(inset.left, size.x),
        resolve(inset.top, size.y),
        resolve(inset.right, size.x),
        resolve(inset.bottom, size.y),
    ]
}

/// Builds `(width, height, spec)` for a representable decorative shape. A
/// bodyless painted `#box` is a rectangle too: packages commonly use a tiny
/// fixed-size box as a colour swatch or list marker, and dropping it loses real
/// visual ink even though it has no semantic text. The `size`/`radius`
/// constructor args of `#square`/`#circle` fold into `width`/`height`, so all
/// shape variants read the same two fields.
fn build(
    ctx: &mut DocxCtx,
    child: &Content,
    styles: StyleChain,
    width_base: Abs,
    height_base: Option<Abs>,
) -> Option<(Abs, Abs, ShapeSpec)> {
    if let Some(e) = child.to_packed::<BoxElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let fill_paint = e.fill.get_cloned(styles);
        let raw_stroke = e.stroke.get_cloned(styles);
        // A completely unpainted box is layout geometry, not a decorative
        // drawing. Leave it to the empty-box approximation path so fixed-size
        // spacers do not acquire a selectable Word object.
        if fill_paint.is_none() && raw_stroke.iter().all(|side| side.is_none()) {
            return None;
        }
        let (w, h) = explicit_box_size(
            e.width.get(styles),
            e.height.get(styles),
            styles,
            width_base,
            height_base,
        )?;
        let fill = fill_color(ctx, &fill_paint)?;
        let stroke = sides_stroke_first(&raw_stroke, styles);
        let geom = if any_radius(&e.radius.get_cloned(styles)) {
            ShapeGeom::RoundRect
        } else {
            ShapeGeom::Rect
        };
        Some((w, h, ShapeSpec { geom, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<RectElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(
            e.width.get(styles),
            e.height.get(styles),
            styles,
            width_base,
            height_base,
        )?;
        let fill = fill_color(ctx, e.fill.get_ref(styles))?;
        let stroke =
            sides_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(
            e.width.get(styles),
            e.height.get(styles),
            styles,
            width_base,
            height_base,
        )?;
        let fill = fill_color(ctx, e.fill.get_ref(styles))?;
        let stroke =
            sides_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<EllipseElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(
            e.width.get(styles),
            e.height.get(styles),
            styles,
            width_base,
            height_base,
        )?;
        let fill = fill_color(ctx, e.fill.get_ref(styles))?;
        let stroke =
            single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<CircleElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(
            e.width.get(styles),
            e.height.get(styles),
            styles,
            width_base,
            height_base,
        )?;
        let fill = fill_color(ctx, e.fill.get_ref(styles))?;
        let stroke =
            single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<PolygonElem>() {
        use typst_library::layout::Point;
        // Resolve each vertex like `#rect`'s percentage sizing — a vertex is a
        // `Rel<Length>` per axis, so a percentage component (e.g. `(50%, 0pt)`,
        // common in a full-width decorative motif) resolves against the scoped
        // bases, and an `em` length against the font size. A vertical
        // percentage with no honest height base bails to the fallback chain.
        let verts: Vec<Point> = e
            .vertices
            .iter()
            .map(|v| {
                Some(Point::new(
                    resolve_axis(v.x, styles, Some(width_base))?,
                    resolve_axis(v.y, styles, height_base)?,
                ))
            })
            .collect::<Option<_>>()?;
        if verts.len() < 2 {
            return None;
        }
        // Route through the same `RawSeg`/`normalize_segments` path `#curve`/
        // `#line` use, so a polygon with NEGATIVE vertex coordinates (e.g. a
        // corner wedge drawn upward from its origin, `(1em, -0.5em)`) is shifted
        // to the non-negative `a:custGeom` space and sized by its true bounding
        // box — the old direct `max_x`/`max_y` logic silently bailed
        // (`max_y <= 0`) on any all-non-positive axis, forcing a rasterize.
        let mut raw = Vec::with_capacity(verts.len() + 1);
        for (i, p) in verts.iter().enumerate() {
            raw.push(if i == 0 { dml::RawSeg::Move(*p) } else { dml::RawSeg::Line(*p) });
        }
        raw.push(dml::RawSeg::Close);
        let normalized = dml::normalize_segments(raw)?;
        let (segments, w, h) = (normalized.segments, normalized.w, normalized.h);
        let fill = fill_color(ctx, e.fill.get_ref(styles))?;
        let stroke =
            single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((
            w,
            h,
            ShapeSpec {
                geom: ShapeGeom::Path(segments),
                fill,
                stroke,
                txbx: None,
            },
        ))
    } else {
        None
    }
}

/// Resolves one axis of an explicit shape size. A pure-absolute length (its
/// ratio component is zero after style resolution) needs no base at all. A
/// ratio component resolves against `base` when one is honestly known — the
/// scoped width budget, or the measured cell row box / section text area for
/// heights — and bails (`None`) when it is not: resolving a cell-relative
/// `height: 100%` against the page manufactured full-page ink and forced each
/// such shape onto its own page.
fn resolve_axis(r: Rel<Length>, styles: StyleChain, base: Option<Abs>) -> Option<Abs> {
    use typst_library::foundations::Resolve;
    let rel = r.resolve(styles);
    if rel.rel.is_zero() {
        Some(rel.abs)
    } else {
        base.map(|base| rel.relative_to(base))
    }
}

/// Resolves an explicit `(width, height)` to absolute sizes, or `None` if
/// either is auto or fractional, or carries a ratio with no honest base
/// (see [`resolve_axis`]).
fn explicit_size(
    width: Smart<Rel<Length>>,
    height: Sizing,
    styles: StyleChain,
    width_base: Abs,
    height_base: Option<Abs>,
) -> Option<(Abs, Abs)> {
    let w = match width {
        Smart::Custom(r) => resolve_axis(r, styles, Some(width_base))?,
        _ => return None,
    };
    let h = match height {
        Sizing::Rel(r) => resolve_axis(r, styles, height_base)?,
        _ => return None,
    };
    Some((w, h))
}

/// Resolves a `#box`'s explicit size. Unlike block shapes, Typst stores the
/// inline box width as [`Sizing`] and its height as [`Smart<Rel<Length>>`].
fn explicit_box_size(
    width: Sizing,
    height: Smart<Rel<Length>>,
    styles: StyleChain,
    width_base: Abs,
    height_base: Option<Abs>,
) -> Option<(Abs, Abs)> {
    let w = match width {
        Sizing::Rel(r) => resolve_axis(r, styles, Some(width_base))?,
        _ => return None,
    };
    let h = match height {
        Smart::Custom(r) => resolve_axis(r, styles, height_base)?,
        _ => return None,
    };
    Some((w, h))
}

/// `None` outer = unrepresentable fill → rasterize; inner `None` = no fill.
fn fill_color(ctx: &mut DocxCtx, paint: &Option<Paint>) -> Option<Option<ShapeFill>> {
    match paint {
        None => Some(None),
        Some(Paint::Solid(c)) => Some(Some(ShapeFill::Solid(opaque(color_to_hex(c))))),
        Some(Paint::Gradient(g)) => gradient_fill(g).map(Some),
        Some(Paint::Tiling(tiling)) => tile_fill(ctx, tiling).map(Some),
    }
}

/// Maps a Typst [`Gradient`] to a native DrawingML gradient fill, or `None`
/// (bail to rasterize) for conic gradients.
fn gradient_fill(gradient: &typst_library::visualize::Gradient) -> Option<ShapeFill> {
    dml::gradient_fill(gradient, dml::AlphaMode::Opaque)
}

fn tile_fill(ctx: &mut DocxCtx, tiling: &Tiling) -> Option<ShapeFill> {
    let tile = dml::render_tiling_tile(tiling)?;
    let rel = ctx.add_image(&tile.png, "png");
    Some(tile.fill(TileImage::Rel(rel)))
}

/// Resolves a single optional stroke. `None` outer = unrepresentable (gradient)
/// → rasterize; inner `None` = no stroke. A `Smart::Auto` stroke is the Typst
/// default: a 1pt black outline when there is no fill, else none.
fn single_stroke(
    stroke: Smart<Option<Stroke>>,
    fill: &Option<Paint>,
    styles: StyleChain,
) -> Option<Option<ShapeStroke>> {
    match stroke {
        Smart::Auto => Some(default_stroke(fill)),
        Smart::Custom(None) => Some(None),
        Smart::Custom(Some(s)) => resolve_stroke(s, styles).map(Some),
    }
}

/// A per-side stroke (`#rect`/`#square`) reduced to one uniform stroke (top side).
fn sides_stroke(
    sides: Smart<typst_library::layout::Sides<Option<Option<Stroke>>>>,
    fill: &Option<Paint>,
    styles: StyleChain,
) -> Option<Option<ShapeStroke>> {
    match sides {
        Smart::Auto => Some(default_stroke(fill)),
        Smart::Custom(s) => match s.top.flatten() {
            None => Some(None),
            Some(stroke) => resolve_stroke(stroke, styles).map(Some),
        },
    }
}

fn default_stroke(fill: &Option<Paint>) -> Option<ShapeStroke> {
    if fill.is_some() {
        None
    } else {
        Some(ShapeStroke {
            color: [0, 0, 0, 255],
            w_emu: abs_to_emu(Abs::pt(1.0)),
            cap: "flat",
            dash: None,
        })
    }
}

fn resolve_stroke(stroke: Stroke, styles: StyleChain) -> Option<ShapeStroke> {
    let fx = stroke.resolve(styles).unwrap_or_default();
    match &fx.paint {
        Paint::Solid(c) => Some(ShapeStroke {
            color: opaque(color_to_hex(c)),
            w_emu: abs_to_emu(fx.thickness),
            cap: dml::line_cap_to_ooxml(fx.cap),
            dash: fx.dash.as_ref().map(|d| dml::prst_dash(&d.array, fx.thickness)),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Arbitrary vector paths: `#curve`, a diagonal/endpoint `#line`, and `#move`
// compositions of either.
// ---------------------------------------------------------------------------
//
// `#curve`/`#line` lay out to a single `Shape` frame item (`Geometry::Curve`/
// `Geometry::Line`) whose points are already fully resolved (percentages,
// `auto`-mirrored Bézier control points, relative-to-pen coordinates) by the
// shared layouter — the same code every other export target uses, so this
// reuses it rather than re-deriving that resolution logic. `#curve`'s
// Move/Line/Cubic/Close items map 1:1 onto OOXML `a:custGeom`'s
// moveTo/lnTo/cubicBezTo/close; a `#line` is the simplest case, a single open
// two-point path.
//
// `#move` lays out its ENTIRE body (which may hold several shapes/lines/
// curves — the common way to hand-compose a small diagram without a package
// like CeTZ) via the general recursive layouter, then walks every frame item:
// if the whole composition is native-representable shapes (optionally nested
// one level into plain-translation frame groups — ordinary block-flow
// nesting), it becomes ONE drawing — a single shape if there's only one, or a
// `wpg:wgp` group of several sharing one coordinate space, so their relative
// positioning survives instead of each computing its own independent
// page-anchor. Anything else in the body (text, images, a rotate/scale/skew
// transform) bails to rasterize, preserving the exact pre-existing behaviour
// for everything not in this narrow window.

/// Maps a `#curve` — straight and cubic-Bézier segments — to a native
/// `a:custGeom` vector shape, or `None` to rasterize (a non-solid fill/stroke,
/// or a degenerate/zero-size result).
pub fn curve(
    elem: &typst_library::foundations::Packed<typst_library::visualize::CurveElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let frame = layout_shape_frame(ctx, elem.span(), |engine, locator, region| {
        typst_layout::layout_curve(elem, engine, locator, styles, region)
    })?;
    build_shapes_drawing(ctx, &frame, elem.span())
}

/// Maps a diagonal or explicit-endpoint `#line` to a native open `a:custGeom`
/// path (a horizontal rule is handled earlier, as a paragraph border — see
/// `convert::handle_block_inner`). `None` to rasterize, for the same reasons as
/// [`curve`].
pub fn line(
    elem: &typst_library::foundations::Packed<typst_library::visualize::LineElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let frame = layout_shape_frame(ctx, elem.span(), |engine, locator, region| {
        typst_layout::layout_line(elem, engine, locator, styles, region)
    })?;
    build_shapes_drawing(ctx, &frame, elem.span())
}

/// Maps `#move(dx:, dy:)[body]` to one or more native shapes when its ENTIRE
/// body is a composition of natively-representable shapes/lines/curves — a
/// single shape becomes one drawing; several become a `wpg:wgp` group sharing
/// one coordinate space (recovering, e.g., a hand-drawn diagram built from a
/// few `#move`d primitives — the dominant remaining rasterize cause in real
/// documents; see `#76`'s corpus investigation). `None` (rasterize the whole
/// `#move`, as before) the moment anything in the body isn't a
/// plain-translated native shape — text, an image, or a further
/// rotate/scale/skew.
pub fn move_(
    elem: &typst_library::foundations::Packed<typst_library::layout::MoveElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    use typst_library::layout::{Axes, Rel, Size};

    // Must go through `layout_export_frame` (not a bare `layout_frame` call):
    // it chains in `Target::Paged`, which is what makes bare shape elements
    // (`LineElem`, `CurveElem`, …) show-rule into their `BlockElem` layouters
    // in the first place — under the ambient `Docx` target those show rules
    // aren't registered, so the body's shapes would be dropped by flow
    // collection ("was ignored during paged export") and the frame would come
    // back empty.
    let size = Size::new(ctx.available_width, ctx.raster_height);
    let height = ctx.raster_height;
    let (frame, _) = ctx.layout_export_frame(&elem.body, styles, elem.span(), height)?;
    let Some(mut frame) = frame else {
        return Ok(None);
    };
    // Mirrors `typst_layout::layout_move` exactly (dx/dy resolved against the
    // region, then a visual-only translate of the laid-out body).
    let delta = Axes::new(elem.dx.resolve(styles), elem.dy.resolve(styles))
        .zip_map(size, Rel::relative_to);
    frame.translate_visual(delta.to_point());

    build_shapes_drawing(ctx, &frame, elem.span())
}

/// Maps a bare (not `#move`-wrapped) `#rotate(..)[body]`/`#scale(..)[body]`
/// whose body is a native-representable shape/composition, the same way
/// [`move_`] does for `#move` — the transform doesn't need any special
/// handling of its own here: laying out `child` (the whole rotate/scale
/// element, not just its body) via `layout_export_frame` produces a frame
/// where the rotation/scale already shows up as an ordinary
/// `FrameItem::Group` (exactly the shape [`extract_shapes`]/[`collect_shapes`]
/// already knows how to bake into path coordinates via
/// [`similarity_scale`]), so this is just that same walk with no translate
/// step. `None` (fall through to rasterize) for anything [`build_shapes_drawing`]
/// doesn't recognize (text, an image, a skew/non-uniform scale).
pub fn transformed(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let height = ctx.raster_height;
    let (frame, _) = ctx.layout_export_frame(child, styles, child.span(), height)?;
    let Some(frame) = frame else {
        return Ok(None);
    };
    let run = build_shapes_drawing(ctx, &frame, child.span())?;
    if run.is_some() {
        ctx.defer_frame_tags(&frame);
    }
    Ok(run)
}

/// Recovers a finite source-proven shape-only placement canvas as one shared
/// DrawingML coordinate space. Layout can leave harmless glyph items in an
/// otherwise visual box (for example alignment/baseline scaffolding), so try
/// the richer canvas collector before the strict shape-only extractor.
pub fn placed_shape_canvas(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let height = ctx.raster_height;
    let (frame, _) = ctx.layout_export_frame(child, styles, child.span(), height)?;
    let Some(frame) = frame else { return Ok(None) };
    if let Some(run) = mixed_canvas(ctx, &frame, child.span())? {
        return Ok(Some(run));
    }
    let run = build_shapes_drawing(ctx, &frame, child.span())?;
    if run.is_some() {
        ctx.defer_frame_tags(&frame);
    }
    Ok(run)
}

/// Maps a pure vertical nudge of plain text/inline content — `#move(dy:
/// ..)[body]` with `dx` ~0 and `body` containing no shape/image/nested
/// transform (see [`is_pure_text_body`]) — to real inline runs carrying a
/// `w:position` shift, instead of rasterizing. `None` (fall through to the
/// existing rasterize fallback) whenever `dx` isn't negligible or `body`
/// isn't plain text, since a MIXED composition (some real shape alongside
/// text) would silently lose that shape's own shift if lowered this way — the
/// same conservative bail every other native-shape path in this module takes.
///
/// Word's own contract for `w:position` — raise/lower a run's glyphs without
/// affecting the paragraph's line height — mirrors `#move`'s "translate
/// visually without affecting layout" exactly, which is what makes this
/// substitution safe rather than approximate.
pub fn move_text(
    elem: &typst_library::foundations::Packed<typst_library::layout::MoveElem>,
    styles: StyleChain,
    props: &crate::dom::RunProps,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Vec<Run>>> {
    use typst_library::layout::{Axes, Rel, Size};

    if !is_pure_text_body(&elem.body) {
        return Ok(None);
    }
    // Mirrors `move_`'s own dx/dy resolution (against the raster region size)
    // so a percentage `dx`/`dy` — rare, but the field type (`Rel<Length>`)
    // permits it — resolves consistently between the two paths.
    let size = Size::new(ctx.available_width, ctx.raster_height);
    let delta = Axes::new(elem.dx.resolve(styles), elem.dy.resolve(styles))
        .zip_map(size, Rel::relative_to);
    if delta.x != Abs::zero() {
        return Ok(None);
    }
    let dy = delta.y;
    let mut p = props.clone();
    let existing = p.position_half_pt.unwrap_or(0);
    // `w:position` is upward-positive while `#move`'s `dy` is downward-positive
    // (same convention `TextElem::baseline` already negates — see
    // `resolve_text_props`), so negate; then compose with whatever shift the
    // ambient run properties already carry (e.g. a nested `#move`, or this
    // sitting inside a `#super`/`#sub`).
    let shift = existing.saturating_add((-dy.to_pt() * 2.0).round() as i32);
    p.position_half_pt = Some(shift);

    Ok(Some(ctx.inline_runs(&elem.body, styles, p)?))
}

/// Whether `body` contains nothing but plain inline/text content — no drawn
/// shape, image, or nested transform. Deliberately conservative: a single
/// non-text element found anywhere inside bails (`false`), since [`move_`]
/// (the shape/group composition path) and the whole-container rasterize
/// fallback already try first — a body this check lets through is exactly the
/// content [`move_text`] can safely lower to a `w:position`-shifted run.
fn is_pure_text_body(body: &Content) -> bool {
    use std::ops::ControlFlow;
    use typst_library::layout::{GridElem, MoveElem, RotateElem, ScaleElem};
    use typst_library::model::{FigureElem, TableElem};
    use typst_library::visualize::{CurveElem, ImageElem, LineElem};

    let mut has_non_text = false;
    let _ = body.traverse(&mut |e: Content| {
        if e.is::<LineElem>()
            || e.is::<CurveElem>()
            || e.is::<RectElem>()
            || e.is::<SquareElem>()
            || e.is::<EllipseElem>()
            || e.is::<CircleElem>()
            || e.is::<PolygonElem>()
            || e.is::<ImageElem>()
            || e.is::<MoveElem>()
            || e.is::<RotateElem>()
            || e.is::<ScaleElem>()
            || e.is::<GridElem>()
            || e.is::<TableElem>()
            || e.is::<FigureElem>()
        {
            has_non_text = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    !has_non_text
}

/// Lays out a shape element to a [`typst_library::layout::Frame`] via the
/// shared layouter, sized against the page's content area (mirroring how
/// rasterized content is sized) since `#curve`/`#line` points may be given as
/// percentages of their container.
fn layout_shape_frame(
    ctx: &mut DocxCtx,
    span: typst_syntax::Span,
    layout: impl FnOnce(
        &mut typst_library::engine::Engine,
        typst_library::introspection::Locator,
        typst_library::layout::Region,
    ) -> SourceResult<typst_library::layout::Frame>,
) -> SourceResult<typst_library::layout::Frame> {
    use typst_library::layout::{Axes, Region, Size};
    let region = Region::new(
        Size::new(ctx.available_width, ctx.raster_height),
        Axes::splat(false),
    );
    let locator = ctx.next_locator(span);
    layout(ctx.engine(), locator, region)
}

/// One extracted shape: its raw (un-normalized) path segments — already
/// carrying any ancestor rotation/reflection/uniform-scale baked directly
/// into their coordinates — plus its resolved fill/stroke, in the SHARED
/// coordinate space of whatever frame it was extracted from (so multiple
/// shapes from one frame can be positioned relative to each other).
struct ExtractedShape {
    raw: Vec<dml::RawSeg>,
    fill: Option<ShapeFill>,
    stroke: Option<ShapeStroke>,
}

enum ExtractedCanvasChild {
    Shape(ExtractedShape),
    Text {
        x: Abs,
        y: Abs,
        width: Abs,
        height: Abs,
        text: EcoString,
        props: crate::dom::RunProps,
        rtl: bool,
    },
}

/// Word's DrawingML reader rejects otherwise schema-valid coordinates outside
/// its signed 32-bit implementation range.
// The schema permits larger values, but current desktop Word rejects documents
// containing custom geometries in the billion-EMU range. 100 million EMU is
// still over 7,800pt—far beyond a page—while leaving ample parser headroom.
const WORD_SAFE_POINT_MAX: i64 = 100_000_000;
const WORD_SAFE_POINT_MIN: i64 = -100_000_000;
// A normalized extent can span from the negative point bound to the positive
// point bound even though every source point itself stays inside ±100M.
const WORD_SAFE_COORDINATE_MAX: i64 = 200_000_000;
const WORD_SAFE_COORDINATE_MIN: i64 = -100_000_000;

fn word_safe_coordinates(values: impl IntoIterator<Item = i64>) -> bool {
    values.into_iter().all(|value| {
        (WORD_SAFE_COORDINATE_MIN..=WORD_SAFE_COORDINATE_MAX).contains(&value)
    })
}

/// Compresses only a pathological, out-of-range axis toward the edge farther
/// from the page origin. The near edge stays fixed, so the page-visible part
/// of an enormous line/curve remains in place while Word receives coordinates
/// it can parse. This also avoids attempting an unbounded raster fallback.
fn fit_raw_to_word_coordinates(raw: Vec<dml::RawSeg>) -> (Vec<dml::RawSeg>, bool) {
    use typst_library::layout::{Abs, Point};

    let (min_x, min_y, max_x, max_y) = dml::raw_bounds(&raw);
    let limit_min = WORD_SAFE_POINT_MIN as f64 / 12700.0;
    let limit_max = WORD_SAFE_POINT_MAX as f64 / 12700.0;
    let axis = |value: Abs, min: Abs, max: Abs| {
        let (value, min, max) = (value.to_pt(), min.to_pt(), max.to_pt());
        if min >= limit_min && max <= limit_max {
            return Abs::pt(value);
        }

        let preserve_min = min.abs() <= max.abs();
        let pivot = if preserve_min { min } else { max };
        let span = (max - min).max(f64::EPSILON);
        let available = if preserve_min { limit_max - pivot } else { pivot - limit_min };
        let scale = (available / span).clamp(0.0, 1.0);
        Abs::pt(pivot + (value - pivot) * scale)
    };
    let point = |p: Point| Point::new(axis(p.x, min_x, max_x), axis(p.y, min_y, max_y));

    let changed = min_x.to_pt() < limit_min
        || min_y.to_pt() < limit_min
        || max_x.to_pt() > limit_max
        || max_y.to_pt() > limit_max;
    let fitted = raw
        .into_iter()
        .map(|segment| match segment {
            dml::RawSeg::Move(p) => dml::RawSeg::Move(point(p)),
            dml::RawSeg::Line(p) => dml::RawSeg::Line(point(p)),
            dml::RawSeg::Cubic(c1, c2, end) => {
                dml::RawSeg::Cubic(point(c1), point(c2), point(end))
            }
            dml::RawSeg::Close => dml::RawSeg::Close,
        })
        .collect();
    (fitted, changed)
}

/// Walks every item in `frame`, extracting each native-representable shape —
/// or bailing (`None`) the moment it finds anything that isn't one (text, an
/// image, an unrepresentable fill/stroke, or a transform that isn't a
/// similarity — see [`similarity_scale`]). Recurses through nested
/// `FrameItem::Group`s (ordinary block-flow nesting, or a real
/// `#rotate`/`#scale`/`#move`), accumulating their transforms so a rotated or
/// uniformly-scaled shape/composition is recovered too: since an OOXML
/// `a:custGeom` path is just a flat point list with no inherent orientation,
/// baking the WHOLE accumulated transform directly into each point's
/// coordinates (rather than trying to express the rotation as `a:xfrm rot=`)
/// reuses every bit of the plain-translation machinery unchanged.
fn extract_shapes(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
) -> Option<Vec<ExtractedShape>> {
    use typst_library::layout::Transform;
    let mut out = Vec::new();
    collect_shapes(ctx, frame, Transform::identity(), 1.0, &mut out).then_some(out)
}

fn collect_shapes(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
    acc: typst_library::layout::Transform,
    stroke_scale: f64,
    out: &mut Vec<ExtractedShape>,
) -> bool {
    use typst_library::layout::{FrameItem, Transform};

    for (pos, item) in frame.items() {
        // Matches every exporter's own `handle_frame`/`render_group` walk
        // (e.g. `typst-pdf`'s `handle_group`): each item is first translated
        // by its own position within its parent frame, and a `Group` item's
        // own transform then applies on top of that (`pre_concat`'s "prev
        // happens first" order), before recursing into its sub-frame.
        let item_transform = acc.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Shape(shape, _) => {
                let Some(fill) = resolved_fill(ctx, &shape.fill) else { return false };
                let Some(stroke) = resolved_stroke(&shape.stroke, stroke_scale) else {
                    return false;
                };
                let raw = dml::geometry_to_raw(&shape.geometry, item_transform);
                out.push(ExtractedShape { raw, fill, stroke });
            }
            FrameItem::Group(group) => {
                if group.clip.is_some() {
                    // A clip path inside — not attempted; bail to rasterize.
                    return false;
                }
                let Some(scale) = dml::similarity_scale(&group.transform) else {
                    // A skew or non-uniform scale — no exact flat-stroke-width
                    // representation; the exact "grouped shapes with a
                    // transform" case this pass doesn't attempt (see
                    // COVERAGE.md); bail to rasterize.
                    return false;
                };
                let new_acc = item_transform.pre_concat(group.transform);
                if !collect_shapes(ctx, &group.frame, new_acc, stroke_scale * scale, out)
                {
                    return false;
                }
            }
            FrameItem::Tag(_) => {
                // Introspection-only marker; carries no visual, safe to skip.
            }
            _ => return false, // text, image, link — not a pure shape composition
        }
    }
    true
}

/// Recovers a bounded frame containing both vector geometry and positioned
/// text as one native WordprocessingGroup. Shapes and labels retain the shared
/// coordinate system of the authored canvas; labels become editable text-box
/// children rather than hidden text beside a raster image.
///
/// This deliberately accepts only transformations Word can reproduce without
/// changing glyph layout. Shapes retain the broader similarity-transform path,
/// while text requires a positive, axis-aligned uniform scale. Clips, images,
/// link overlays, text strokes, and non-solid glyph paints make the whole
/// composition fall back atomically.
pub fn mixed_canvas(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
    span: typst_syntax::Span,
) -> SourceResult<Option<Run>> {
    use typst_library::layout::Transform;

    let mut extracted = Vec::new();
    if !collect_canvas_children(ctx, frame, Transform::identity(), 1.0, &mut extracted)
        || extracted.len() < 2
    {
        return Ok(None);
    }

    let size = frame.size();
    let (w_emu, h_emu) = (abs_to_emu(size.x).max(1), abs_to_emu(size.y).max(1));
    if !word_safe_coordinates([w_emu, h_emu]) {
        return Ok(None);
    }

    let mut fitted = false;
    let mut children = Vec::with_capacity(extracted.len());
    for child in extracted {
        let group_child = match child {
            ExtractedCanvasChild::Shape(shape) => {
                let (raw, changed) = fit_raw_to_word_coordinates(shape.raw);
                fitted |= changed;
                let Some(normalized) = dml::normalize_segments(raw) else { continue };
                let x_emu = abs_to_emu(normalized.min_x);
                let y_emu = abs_to_emu(normalized.min_y);
                let child_w_emu = abs_to_emu(normalized.w).max(1);
                let child_h_emu = abs_to_emu(normalized.h).max(1);
                if !word_safe_coordinates([x_emu, y_emu, child_w_emu, child_h_emu]) {
                    return Ok(None);
                }
                GroupChild {
                    x_emu,
                    y_emu,
                    w_emu: child_w_emu,
                    h_emu: child_h_emu,
                    shape: ShapeSpec {
                        geom: ShapeGeom::Path(normalized.segments),
                        fill: shape.fill,
                        stroke: shape.stroke,
                        txbx: None,
                    },
                }
            }
            ExtractedCanvasChild::Text { x, y, width, height, text, props, rtl } => {
                let x_emu = abs_to_emu(x);
                let y_emu = abs_to_emu(y);
                let child_w_emu = abs_to_emu(width).max(1);
                let child_h_emu = abs_to_emu(height).max(1);
                if !word_safe_coordinates([x_emu, y_emu, child_w_emu, child_h_emu]) {
                    return Ok(None);
                }
                let line = crate::props::abs_to_twip(height).max(1);
                GroupChild {
                    x_emu,
                    y_emu,
                    w_emu: child_w_emu,
                    h_emu: child_h_emu,
                    shape: ShapeSpec {
                        geom: ShapeGeom::Rect,
                        fill: None,
                        stroke: None,
                        txbx: Some(TextBox {
                            ins: [0; 4],
                            blocks: vec![crate::dom::Block::Para(crate::dom::Para {
                                props: crate::dom::ParaProps {
                                    bidi: rtl,
                                    spacing: Some(crate::dom::Spacing {
                                        before: Some(0),
                                        after: Some(0),
                                        line: Some(line),
                                        ..Default::default()
                                    }),
                                    ..Default::default()
                                },
                                content: vec![crate::dom::ParaChild::Run(Run::Text {
                                    props,
                                    text,
                                })],
                            })],
                            wrap: TextBoxWrap::None,
                            autofit: false,
                        }),
                    },
                }
            }
        };
        children.push(group_child);
    }
    if children.len() < 2 {
        return Ok(None);
    }

    if fitted {
        ctx.record_span_decision(
            "Word-bounded mixed vector group geometry",
            span,
            Representation::Approximate,
            DecisionReason::WordCoordinateBound,
            LossSet::VISUAL_ONLY,
        );
    }
    ctx.defer_frame_tags(frame);
    let docpr_id = ctx.next_drawing_id();
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu,
        h_emu,
        source_offset_emu: [0, 0],
        alt: None,
        decorative: false,
        docpr_id,
        name: ecow::eco_format!("Canvas {docpr_id}"),
        anchor: None,
        shape: None,
        group: Some(GroupSpec { children }),
    })))
}

fn collect_canvas_children(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
    acc: typst_library::layout::Transform,
    stroke_scale: f64,
    out: &mut Vec<ExtractedCanvasChild>,
) -> bool {
    use typst_library::layout::{Dir, FrameItem, Point, Transform};
    use typst_library::text::FontStyle;

    for (pos, item) in frame.items() {
        let item_transform = acc.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Shape(shape, _) => {
                let Some(fill) = resolved_fill(ctx, &shape.fill) else { return false };
                let Some(stroke) = resolved_stroke(&shape.stroke, stroke_scale) else {
                    return false;
                };
                out.push(ExtractedCanvasChild::Shape(ExtractedShape {
                    raw: dml::geometry_to_raw(&shape.geometry, item_transform),
                    fill,
                    stroke,
                }));
            }
            FrameItem::Text(text) if !text.text.is_empty() => {
                let (sx, sy) = (item_transform.sx.get(), item_transform.sy.get());
                let text_box_safe = text.stroke.is_none()
                    && item_transform.kx.get().abs() <= 1e-9
                    && item_transform.ky.get().abs() <= 1e-9
                    && sx > 0.0
                    && (sx - sy).abs() <= 1e-9;
                if !text_box_safe {
                    if outline_text_item(ctx, text, item_transform, out) {
                        continue;
                    }
                    return false;
                }
                let Paint::Solid(color) = &text.fill else { return false };
                let baseline = Point::zero().transform(item_transform);
                let size = text.size * sx;
                let ascent = text.font.metrics().ascender.at(size);
                let descent = (-text.font.metrics().descender).at(size);
                let height = (ascent + descent).max(size);
                // A small right-side allowance prevents consumer font metrics
                // from wrapping a label that fits exactly in Typst.
                let width = (text.width() * sx + Abs::pt(0.5)).max(Abs::pt(0.5));
                let variant = text.font.font().info().variant;
                let rtl = matches!(text.lang.dir(), Dir::RTL);
                out.push(ExtractedCanvasChild::Text {
                    x: baseline.x,
                    y: baseline.y - ascent,
                    width,
                    height,
                    text: text.text.clone(),
                    props: crate::dom::RunProps {
                        font: Some(text.font.font().info().family.clone().into()),
                        bold: variant.weight.to_number() >= 600,
                        italic: matches!(
                            variant.style,
                            FontStyle::Italic | FontStyle::Oblique
                        ),
                        color: Some(color_to_hex(color)),
                        size_half_pt: Some(crate::props::pt_to_half_pt(size.to_pt())),
                        rtl,
                        cs: rtl,
                        ..Default::default()
                    },
                    rtl,
                });
            }
            FrameItem::Text(_) | FrameItem::Image(_, _, _) | FrameItem::Link(_, _) => {
                return false;
            }
            FrameItem::Group(group) => {
                let Some(scale) = dml::similarity_scale(&group.transform) else {
                    return false;
                };
                let new_acc = item_transform.pre_concat(group.transform);
                if !collect_canvas_children(
                    ctx,
                    &group.frame,
                    new_acc,
                    stroke_scale * scale,
                    out,
                ) {
                    return false;
                }
            }
            FrameItem::Tag(_) => {}
        }
    }
    true
}

/// Converts a text item whose transform cannot be represented by an editable
/// DrawingML text box (most commonly a rotated mathematical arrowhead) into
/// native vector paths. Ordinary labels remain editable text boxes; only the
/// transformed glyphs become geometry.
fn outline_text_item(
    ctx: &mut DocxCtx,
    text: &typst_library::text::TextItem,
    transform: typst_library::layout::Transform,
    out: &mut Vec<ExtractedCanvasChild>,
) -> bool {
    let Some(fill) = resolved_fill(ctx, &Some(text.fill.clone())) else {
        return false;
    };
    let scale = text.size.to_pt() / text.font.units_per_em();
    let mut cursor = typst_library::layout::Point::zero();
    for glyph in &text.glyphs {
        let origin = cursor
            + typst_library::layout::Point::new(
                glyph.x_offset.at(text.size),
                -glyph.y_offset.at(text.size),
            );
        let mut builder = GlyphOutlineBuilder::new(origin, scale, transform);
        let outlined = text
            .font
            .ttf()
            .outline_glyph(ttf_parser::GlyphId(glyph.id), &mut builder);
        if outlined.is_some() && !builder.raw.is_empty() {
            out.push(ExtractedCanvasChild::Shape(ExtractedShape {
                raw: builder.raw,
                fill: fill.clone(),
                stroke: None,
            }));
        }
        cursor += typst_library::layout::Point::new(
            glyph.x_advance.at(text.size),
            -glyph.y_advance.at(text.size),
        );
    }
    true
}

struct GlyphOutlineBuilder {
    raw: Vec<dml::RawSeg>,
    current: typst_library::layout::Point,
    origin: typst_library::layout::Point,
    scale: f64,
    transform: typst_library::layout::Transform,
}

impl GlyphOutlineBuilder {
    fn new(
        origin: typst_library::layout::Point,
        scale: f64,
        transform: typst_library::layout::Transform,
    ) -> Self {
        Self {
            raw: Vec::new(),
            current: typst_library::layout::Point::zero(),
            origin,
            scale,
            transform,
        }
    }

    fn point(&self, x: f32, y: f32) -> typst_library::layout::Point {
        use typst_library::layout::{Abs, Point};
        (self.origin
            + Point::new(
                Abs::pt(f64::from(x) * self.scale),
                Abs::pt(-f64::from(y) * self.scale),
            ))
        .transform(self.transform)
    }
}

impl ttf_parser::OutlineBuilder for GlyphOutlineBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.current = point;
        self.raw.push(dml::RawSeg::Move(point));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.current = point;
        self.raw.push(dml::RawSeg::Line(point));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let end = self.point(x, y);
        let c1 = self.current + (control - self.current) * (2.0 / 3.0);
        let c2 = end + (control - end) * (2.0 / 3.0);
        self.raw.push(dml::RawSeg::Cubic(c1, c2, end));
        self.current = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c1 = self.point(x1, y1);
        let c2 = self.point(x2, y2);
        let end = self.point(x, y);
        self.raw.push(dml::RawSeg::Cubic(c1, c2, end));
        self.current = end;
    }

    fn close(&mut self) {
        self.raw.push(dml::RawSeg::Close);
    }
}

/// Builds the final [`Run::Drawing`] from every shape found in `frame`: `None`
/// if the frame holds anything not natively representable, a single native
/// shape if it holds exactly one, or a `wpg:wgp` group if it holds several.
fn build_shapes_drawing(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
    span: typst_syntax::Span,
) -> SourceResult<Option<Run>> {
    let Some(shapes) = extract_shapes(ctx, frame) else { return Ok(None) };
    if shapes.is_empty() {
        return Ok(None);
    }

    if shapes.len() == 1 {
        let ExtractedShape { raw, fill, stroke } = shapes.into_iter().next().unwrap();
        let (raw, fitted) = fit_raw_to_word_coordinates(raw);
        let Some(normalized) = dml::normalize_segments(raw) else {
            return Ok(None);
        };
        let (segments, min_x, min_y, w, h) = (
            normalized.segments,
            normalized.min_x,
            normalized.min_y,
            normalized.w,
            normalized.h,
        );
        // A perfectly horizontal/vertical line is legitimately degenerate on
        // one axis; floor it to 1 EMU (imperceptible) rather than the 0 Word
        // handles poorly for a drawing extent. `normalize_segments` already
        // bailed when BOTH axes are degenerate (nothing to draw).
        let (w_emu, h_emu) = (abs_to_emu(w).max(1), abs_to_emu(h).max(1));
        let source_offset_emu = [abs_to_emu(min_x), abs_to_emu(min_y)];
        debug_assert!(word_safe_coordinates([
            w_emu,
            h_emu,
            source_offset_emu[0],
            source_offset_emu[1],
        ]));
        let docpr_id = ctx.next_drawing_id();
        if fitted {
            ctx.record_span_decision(
                "Word-bounded vector geometry",
                span,
                Representation::Approximate,
                DecisionReason::WordCoordinateBound,
                LossSet::VISUAL_ONLY,
            );
        }
        let name = ecow::eco_format!("Shape {docpr_id}");
        return Ok(Some(Run::Drawing(Drawing {
            rel: EcoString::new(),
            svg_rel: None,
            compatibility_split_ids: None,
            w_emu,
            h_emu,
            source_offset_emu,
            alt: None,
            decorative: true,
            docpr_id,
            name,
            anchor: None,
            shape: Some(ShapeSpec {
                geom: ShapeGeom::Path(segments),
                fill,
                stroke,
                txbx: None,
            }),
            group: None,
        })));
    }

    // Several shapes: position each relative to the GROUP's own shared origin
    // (the union of every shape's own bounds), so their relative layout — not
    // just each one's own local geometry — is preserved.
    let mut fitted = false;
    let shapes: Vec<_> = shapes
        .into_iter()
        .map(|shape| {
            let (raw, changed) = fit_raw_to_word_coordinates(shape.raw);
            fitted |= changed;
            ExtractedShape { raw, ..shape }
        })
        .collect();
    let bounds: Vec<_> = shapes.iter().map(|s| dml::raw_bounds(&s.raw)).collect();
    let (mut group_min_x, mut group_min_y, mut group_max_x, mut group_max_y) = bounds[0];
    for &(x0, y0, x1, y1) in &bounds[1..] {
        group_min_x = group_min_x.min(x0);
        group_min_y = group_min_y.min(y0);
        group_max_x = group_max_x.max(x1);
        group_max_y = group_max_y.max(y1);
    }
    let (group_w, group_h) = (group_max_x - group_min_x, group_max_y - group_min_y);
    if !group_w.to_pt().is_finite()
        || !group_h.to_pt().is_finite()
        || (group_w.to_pt() <= 0.0 && group_h.to_pt() <= 0.0)
    {
        return Ok(None);
    }
    let (group_w_emu, group_h_emu) =
        (abs_to_emu(group_w).max(1), abs_to_emu(group_h).max(1));
    let group_source_offset_emu = [abs_to_emu(group_min_x), abs_to_emu(group_min_y)];
    if !word_safe_coordinates([
        group_w_emu,
        group_h_emu,
        group_source_offset_emu[0],
        group_source_offset_emu[1],
    ]) {
        return Ok(None);
    }

    let mut children = Vec::with_capacity(shapes.len());
    for (shape, (min_x, min_y, _, _)) in shapes.into_iter().zip(bounds) {
        let ExtractedShape { raw, fill, stroke } = shape;
        let Some(normalized) = dml::normalize_segments(raw) else { continue };
        let (segments, w, h) = (normalized.segments, normalized.w, normalized.h);
        let (x_emu, y_emu) =
            (abs_to_emu(min_x - group_min_x), abs_to_emu(min_y - group_min_y));
        let (w_emu, h_emu) = (abs_to_emu(w).max(1), abs_to_emu(h).max(1));
        if !word_safe_coordinates([x_emu, y_emu, w_emu, h_emu]) {
            return Ok(None);
        }
        children.push(GroupChild {
            x_emu,
            y_emu,
            w_emu,
            h_emu,
            shape: ShapeSpec {
                geom: ShapeGeom::Path(segments),
                fill,
                stroke,
                txbx: None,
            },
        });
    }
    if children.len() < 2 {
        // Every shape but one turned out degenerate after all; not worth a
        // group for a single survivor — fall back to rasterizing rather than
        // re-deriving the single-shape path for this rare edge case.
        return Ok(None);
    }

    let docpr_id = ctx.next_drawing_id();
    if fitted {
        ctx.record_span_decision(
            "Word-bounded vector group geometry",
            span,
            Representation::Approximate,
            DecisionReason::WordCoordinateBound,
            LossSet::VISUAL_ONLY,
        );
    }
    let name = ecow::eco_format!("Group {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        svg_rel: None,
        compatibility_split_ids: None,
        w_emu: group_w_emu,
        h_emu: group_h_emu,
        source_offset_emu: group_source_offset_emu,
        alt: None,
        decorative: true,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: Some(GroupSpec { children }),
    })))
}

/// A resolved (post-layout) fill → its native DrawingML form, or `None` (no
/// fill). Returns the OUTER `None` when the paint has no native OOXML form, so
/// the caller bails to rasterize.
fn resolved_fill(ctx: &mut DocxCtx, fill: &Option<Paint>) -> Option<Option<ShapeFill>> {
    fill_color(ctx, fill)
}

/// A resolved (post-layout) stroke → a uniform [`ShapeStroke`]. Same
/// outer/inner `None` convention as [`resolved_fill`]. `scale` is the
/// cumulative uniform scale of any ancestor group transforms (1.0 outside a
/// composition, or under rotation/translation/reflection alone) — since
/// rotating/translating a stroked path leaves its perceived width unchanged
/// but scaling it does, the flat OOXML line width must scale by the same
/// factor to stay visually correct (see [`similarity_scale`]).
fn resolved_stroke(
    stroke: &Option<typst_library::visualize::FixedStroke>,
    scale: f64,
) -> Option<Option<ShapeStroke>> {
    match stroke {
        None => Some(None),
        Some(fx) => match &fx.paint {
            Paint::Solid(c) => {
                let thickness = fx.thickness * scale;
                Some(Some(ShapeStroke {
                    color: opaque(color_to_hex(c)),
                    w_emu: abs_to_emu(thickness),
                    cap: dml::line_cap_to_ooxml(fx.cap),
                    dash: fx.dash.as_ref().map(|d| dml::prst_dash(&d.array, thickness)),
                }))
            }
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::word_safe_coordinates;

    #[test]
    fn word_safe_coordinates_reject_word_fragile_extremes() {
        assert!(word_safe_coordinates([0, 1, 200_000_000, -100_000_000]));
        assert!(!word_safe_coordinates([200_000_001]));
        assert!(!word_safe_coordinates([-100_000_001]));
    }
}

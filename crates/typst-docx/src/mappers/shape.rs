//! Vector-shape mapper: decorative shapes (`#rect`, `#square`, `#ellipse`,
//! `#circle`, `#polygon`) → a DrawingML `wps:wsp` *vector* shape instead of a
//! rasterized image.
//!
//! A shape is mapped only when it is purely decorative (no body) and has an
//! explicit, representable size, fill and stroke — solid colours, no gradient,
//! no auto/fractional size. Anything else returns `None`, and the caller
//! rasterizes it so the visual is still preserved.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Content, Resolve, Smart, StyleChain};
use typst_library::layout::{Abs, BoxElem, Length, Rel, Sides, Sizing};
use typst_library::visualize::{
    CircleElem, EllipseElem, Paint, PolygonElem, RectElem, SquareElem, Stroke,
};

use crate::ctx::DocxCtx;
use crate::dom::{Drawing, Run, ShapeGeom, ShapeSpec, ShapeStroke, TextBox};
use crate::props::{abs_to_emu, color_to_hex};

/// Maps a shape element to a vector DrawingML shape run, or `None` to rasterize.
pub fn shape(
    child: &Content,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Run>> {
    let Some((w, h, spec)) = build(child, styles) else {
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
        w_emu,
        h_emu,
        alt: None,
        docpr_id,
        name,
        anchor: None,
        shape: Some(spec),
    })))
}

use ecow::EcoString;

/// Maps a framed container with a body — a styled `#box(fill|stroke)[text]`, or
/// a `#rect`/`#square` carrying content — to a Word *text box*: a `wps:wsp` shape
/// whose `wps:txbx` holds the real, editable text, instead of rasterizing it to a
/// flat image. Returns `None` when the container is not a text-box candidate (no
/// visible frame, an unrepresentable gradient fill, a body with layout-only
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
    let Some(framed) = framed_container(child, styles) else {
        return Ok(None);
    };
    let Framed { body, fill_paint, stroke, rounded, inset } = framed;

    // A gradient/tiling fill has no solid-colour text-box form: keep rasterizing
    // it so the visual survives.
    let fill = match fill_paint {
        Some(Paint::Solid(c)) => Some(color_to_hex(&c)),
        Some(_) => return Ok(None),
        None => None,
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
    let blocks = ctx.blocks(&body, styles)?;
    crate::document::collect_tags(&blocks, &mut ctx.deferred_tags);

    let geom = if rounded { ShapeGeom::RoundRect } else { ShapeGeom::Rect };
    let ins = resolve_insets(&inset, styles, size);

    let docpr_id = ctx.next_drawing_id();
    let name = ecow::eco_format!("Text Box {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        w_emu,
        h_emu,
        alt: None,
        docpr_id,
        name,
        anchor: None,
        shape: Some(ShapeSpec {
            geom,
            fill,
            stroke,
            txbx: Some(TextBox { ins, blocks }),
        }),
    })))
}

/// The body of a `#box`/`#rect`/`#square`, or `None` for any other element (or a
/// bodyless one). Used to decide framed-container handling without resolving the
/// full frame.
pub fn framed_body(child: &Content, styles: StyleChain) -> Option<Content> {
    use typst_library::layout::BoxElem;
    use typst_library::visualize::{RectElem, SquareElem};
    if let Some(e) = child.to_packed::<BoxElem>() {
        e.body.get_cloned(styles)
    } else if let Some(e) = child.to_packed::<RectElem>() {
        e.body.get_cloned(styles)
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        e.body.get_cloned(styles)
    } else {
        None
    }
}

/// The frame properties a `#box`/`#rect`/`#square` with a body contributes to a
/// text box (the `stroke` already resolved to a uniform [`ShapeStroke`]).
struct Framed {
    body: Content,
    fill_paint: Option<Paint>,
    stroke: Option<ShapeStroke>,
    rounded: bool,
    inset: Sides<Option<Rel<Length>>>,
}

/// Extracts the common [`Framed`] view from a `#box`/`#rect`/`#square` that has a
/// body, or `None` for any other element (or a bodyless one).
fn framed_container(child: &Content, styles: StyleChain) -> Option<Framed> {
    if let Some(e) = child.to_packed::<BoxElem>() {
        let body = e.body.get_cloned(styles)?;
        // A box's stroke has no implicit default: only an explicit side counts.
        let stroke = sides_stroke_first(&e.stroke.get_cloned(styles), styles);
        Some(Framed {
            body,
            fill_paint: e.fill.get_cloned(styles),
            stroke,
            rounded: any_radius(&e.radius.get_cloned(styles)),
            inset: e.inset.get_cloned(styles),
        })
    } else if let Some(e) = child.to_packed::<RectElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        Some(Framed {
            stroke: shape_stroke(e.stroke.get_cloned(styles), &fill, styles),
            body,
            fill_paint: fill,
            rounded: any_radius(&e.radius.get_cloned(styles)),
            inset: e.inset.get_cloned(styles),
        })
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        let body = e.body.get_cloned(styles)?;
        let fill = e.fill.get_cloned(styles);
        Some(Framed {
            stroke: shape_stroke(e.stroke.get_cloned(styles), &fill, styles),
            body,
            fill_paint: fill,
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
        opt.map(|r| abs_to_emu(r.resolve(styles).relative_to(base))).unwrap_or(0)
    };
    [
        resolve(inset.left, size.x),
        resolve(inset.top, size.y),
        resolve(inset.right, size.x),
        resolve(inset.bottom, size.y),
    ]
}

/// Builds `(width, height, spec)` for a representable decorative shape. The
/// `size`/`radius` constructor args of `#square`/`#circle` fold into
/// `width`/`height`, so all four read the same two fields.
fn build(child: &Content, styles: StyleChain) -> Option<(Abs, Abs, ShapeSpec)> {
    if let Some(e) = child.to_packed::<RectElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = sides_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = sides_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<EllipseElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<CircleElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke, txbx: None }))
    } else if let Some(e) = child.to_packed::<PolygonElem>() {
        let mut pts = Vec::new();
        for v in e.vertices.iter() {
            pts.push((v.x.abs.resolve(styles), v.y.abs.resolve(styles)));
        }
        if pts.len() < 2 {
            return None;
        }
        let max_x = pts.iter().map(|p| p.0).fold(Abs::zero(), Abs::max);
        let max_y = pts.iter().map(|p| p.1).fold(Abs::zero(), Abs::max);
        if max_x.to_pt() <= 0.0 || max_y.to_pt() <= 0.0 {
            return None;
        }
        let emu = pts
            .iter()
            .map(|p| (abs_to_emu(p.0), abs_to_emu(p.1)))
            .collect();
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        let geom = ShapeGeom::Path { points: emu, closed: true };
        Some((max_x, max_y, ShapeSpec { geom, fill, stroke, txbx: None }))
    } else {
        None
    }
}

/// Resolves an explicit `(width, height)` to absolute sizes, or `None` if either
/// is auto, fractional, or percentage-relative.
fn explicit_size(
    width: Smart<Rel<Length>>,
    height: Sizing,
    styles: StyleChain,
) -> Option<(Abs, Abs)> {
    let w = match width {
        Smart::Custom(r) if r.rel.is_zero() => r.abs.resolve(styles),
        _ => return None,
    };
    let h = match height {
        Sizing::Rel(r) if r.rel.is_zero() => r.abs.resolve(styles),
        _ => return None,
    };
    Some((w, h))
}

/// `None` outer = unrepresentable fill (gradient/tiling) → rasterize; inner
/// `None` = no fill.
fn fill_color(paint: &Option<Paint>) -> Option<Option<[u8; 3]>> {
    match paint {
        None => Some(None),
        Some(Paint::Solid(c)) => Some(Some(color_to_hex(c))),
        Some(_) => None,
    }
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
        Some(ShapeStroke { color: [0, 0, 0], w_emu: abs_to_emu(Abs::pt(1.0)) })
    }
}

fn resolve_stroke(stroke: Stroke, styles: StyleChain) -> Option<ShapeStroke> {
    let fx = stroke.resolve(styles).unwrap_or_default();
    match &fx.paint {
        Paint::Solid(c) => {
            Some(ShapeStroke { color: color_to_hex(c), w_emu: abs_to_emu(fx.thickness) })
        }
        _ => None,
    }
}

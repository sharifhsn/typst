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
use typst_library::layout::{Abs, Length, Rel, Sizing};
use typst_library::visualize::{
    CircleElem, EllipseElem, Paint, PolygonElem, RectElem, SquareElem, Stroke,
};

use crate::ctx::DocxCtx;
use crate::dom::{Drawing, Run, ShapeGeom, ShapeSpec, ShapeStroke};
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
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke }))
    } else if let Some(e) = child.to_packed::<SquareElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = sides_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Rect, fill, stroke }))
    } else if let Some(e) = child.to_packed::<EllipseElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke }))
    } else if let Some(e) = child.to_packed::<CircleElem>() {
        if e.body.get_ref(styles).is_some() {
            return None;
        }
        let (w, h) = explicit_size(e.width.get(styles), e.height.get(styles), styles)?;
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        Some((w, h, ShapeSpec { geom: ShapeGeom::Ellipse, fill, stroke }))
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
        Some((max_x, max_y, ShapeSpec { geom, fill, stroke }))
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

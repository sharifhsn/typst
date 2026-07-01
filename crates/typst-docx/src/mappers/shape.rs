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
use crate::dom::{
    Drawing, GroupChild, GroupSpec, PathSegment, Run, ShapeFill, ShapeGeom, ShapeSpec,
    ShapeStroke, TextBox,
};
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
        group: None,
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
        Some(Paint::Solid(c)) => Some(ShapeFill::Solid(color_to_hex(&c))),
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
    let fill = match framed.fill_paint {
        Some(Paint::Solid(c)) => Some(color_to_hex(&c)),
        _ => None,
    };
    let bdr = framed.stroke.map(|s| {
        let pt = s.w_emu as f64 / 12700.0;
        crate::dom::ParaBorder {
            style: "single",
            // `w:bdr/@w:sz` is in eighths of a point; keep a visible minimum.
            sz: ((pt * 8.0).round() as u32).max(2),
            space: 0,
            color: s.color,
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
        let mut segments = Vec::with_capacity(pts.len() + 1);
        for (i, p) in pts.iter().enumerate() {
            let (x, y) = (abs_to_emu(p.0), abs_to_emu(p.1));
            segments.push(if i == 0 { PathSegment::MoveTo(x, y) } else { PathSegment::LineTo(x, y) });
        }
        segments.push(PathSegment::Close);
        let fill = fill_color(e.fill.get_ref(styles))?;
        let stroke = single_stroke(e.stroke.get_cloned(styles), e.fill.get_ref(styles), styles)?;
        let geom = ShapeGeom::Path(segments);
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
fn fill_color(paint: &Option<Paint>) -> Option<Option<ShapeFill>> {
    match paint {
        None => Some(None),
        Some(Paint::Solid(c)) => Some(Some(ShapeFill::Solid(color_to_hex(c)))),
        Some(Paint::Gradient(g)) => linear_gradient_fill(g).map(Some),
        Some(_) => None,
    }
}

/// Maps a Typst [`Gradient`] to a native [`ShapeFill::LinearGradient`], or
/// `None` (bail to rasterize) for a radial/conic gradient — those don't map
/// cleanly onto OOXML's shape-relative `a:path` radial model (center/radius
/// are free-form in Typst but the OOXML form is anchored to the bounding box),
/// so they are scoped out rather than risk a subtly-wrong mapping.
fn linear_gradient_fill(
    gradient: &typst_library::visualize::Gradient,
) -> Option<ShapeFill> {
    use typst_library::visualize::{ColorSpace, Gradient, ProcessColorSpace};
    let Gradient::Linear(lg) = gradient else { return None };
    // A gradient's stops are stored in its own interpolation space (Oklab by
    // default, not sRGB) for correct in-between blending — `Color::to_vec4_u8`
    // reads a colour's CURRENT components verbatim, with no implicit space
    // conversion, so calling it directly on a stop yields the Oklab L/a/b
    // triple reinterpreted as RGB bytes (a plausible-looking but wrong colour).
    // Convert every stop to sRGB first, exactly as the SVG exporter does
    // before hex-encoding a stop (`paint.rs::write_gradients`).
    let srgb = ColorSpace::Process(ProcessColorSpace::Srgb);
    let stops = lg
        .stops
        .iter()
        .map(|(c, pos)| {
            let rgb = c.to_space(&srgb).unwrap_or_else(|_| c.clone());
            ((pos.get() * 100_000.0).round() as u32, color_to_hex(&rgb))
        })
        .collect();
    // OOXML's `a:lin ang` is measured the same way Typst's gradient angle is:
    // 0 = left-to-right, increasing clockwise, in 60,000ths of a degree.
    let angle_60000ths = (lg.angle.to_deg().rem_euclid(360.0) * 60_000.0).round() as i32;
    Some(ShapeFill::LinearGradient { angle_60000ths, stops })
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
            color: [0, 0, 0],
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
            color: color_to_hex(c),
            w_emu: abs_to_emu(fx.thickness),
            cap: line_cap_to_ooxml(fx.cap),
            dash: fx.dash.as_ref().map(|d| prst_dash(&d.array, fx.thickness)),
        }),
        _ => None,
    }
}

/// Maps Typst's [`LineCap`] to OOXML's `a:ln` `cap` attribute.
fn line_cap_to_ooxml(cap: typst_library::visualize::LineCap) -> &'static str {
    use typst_library::visualize::LineCap;
    match cap {
        LineCap::Butt => "flat",
        LineCap::Round => "rnd",
        LineCap::Square => "sq",
    }
}

/// Maps a *resolved* dash array (plain absolute on/off lengths — by this
/// point `DashLength::LineWidth` has already been multiplied out, so the
/// dot-vs-dash distinction `classify_dash` reads directly from the unresolved
/// source stroke isn't available) to the closest OOXML `a:prstDash` preset — a
/// small fixed vocabulary, so an arbitrary dash array is approximated rather
/// than reproduced exactly. An "on" segment at (or barely above) the line's
/// own thickness reads as a dot (`DashLength::LineWidth` resolves to exactly
/// 1x); a longer one reads as a dash. This is inherently lossy — Typst's own
/// `"dashed"` preset (`3pt` on/off, fixed regardless of thickness) and a
/// custom `dash: "dotted"`-like array both just look like "some on/off array"
/// once resolved to plain lengths, so a sufficiently thick `"dashed"` stroke
/// can misclassify as a dot. Kept tight (1.2x) to favor the common case
/// (thin-to-medium strokes) over the rarer thick-dashed edge case.
fn prst_dash(array: &[Abs], thickness: Abs) -> &'static str {
    if array.is_empty() {
        return "solid";
    }
    let (mut has_dot, mut has_dash) = (false, false);
    for on in array.iter().step_by(2) {
        if *on <= thickness * 1.2 {
            has_dot = true;
        } else {
            has_dash = true;
        }
    }
    match (has_dot, has_dash) {
        (true, true) => "dashDot",
        (true, false) => "sysDot",
        _ => "dash",
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
    build_shapes_drawing(ctx, &frame)
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
    build_shapes_drawing(ctx, &frame)
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
    let size = Size::new(ctx.raster_width, ctx.raster_height);
    let height = ctx.raster_height;
    let Some(mut frame) = ctx.layout_export_frame(&elem.body, styles, elem.span(), height)?
    else {
        return Ok(None);
    };
    // Mirrors `typst_layout::layout_move` exactly (dx/dy resolved against the
    // region, then a visual-only translate of the laid-out body).
    let delta = Axes::new(elem.dx.resolve(styles), elem.dy.resolve(styles))
        .zip_map(size, Rel::relative_to);
    frame.translate_visual(delta.to_point());

    build_shapes_drawing(ctx, &frame)
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
    let region = Region::new(Size::new(ctx.raster_width, ctx.raster_height), Axes::splat(false));
    let locator = ctx.next_locator(span);
    layout(ctx.engine(), locator, region)
}

/// One extracted shape: its raw (un-normalized) path segments plus its
/// resolved fill/stroke, in the SHARED coordinate space of whatever frame it
/// was extracted from (so multiple shapes from one frame can be positioned
/// relative to each other).
struct ExtractedShape {
    raw: Vec<RawSeg>,
    fill: Option<ShapeFill>,
    stroke: Option<ShapeStroke>,
}

/// Walks every item in `frame`, extracting each native-representable shape —
/// or bailing (`None`) the moment it finds anything that isn't one (text, an
/// image, an unrepresentable fill/stroke, or a non-translation transform).
/// Recurses one level into plain-translated sub-frames (`FrameItem::Group`
/// with an identity scale/skew — ordinary block-flow nesting), since a
/// `#move`d body composed of several block-level shapes typically nests that
/// way.
fn extract_shapes(frame: &typst_library::layout::Frame) -> Option<Vec<ExtractedShape>> {
    let mut out = Vec::new();
    collect_shapes(frame, typst_library::layout::Point::zero(), &mut out).then_some(out)
}

fn collect_shapes(
    frame: &typst_library::layout::Frame,
    offset: typst_library::layout::Point,
    out: &mut Vec<ExtractedShape>,
) -> bool {
    use typst_library::layout::{FrameItem, Point, Ratio};

    for (pos, item) in frame.items() {
        let pos = offset + *pos;
        match item {
            FrameItem::Shape(shape, _) => {
                let Some(fill) = resolved_fill(&shape.fill) else { return false };
                let Some(stroke) = resolved_stroke(&shape.stroke) else { return false };
                let Some(raw) = geometry_to_raw(&shape.geometry, pos) else { return false };
                out.push(ExtractedShape { raw, fill, stroke });
            }
            FrameItem::Group(group) => {
                let t = &group.transform;
                let is_translation = t.sx == Ratio::one()
                    && t.sy == Ratio::one()
                    && t.kx == Ratio::zero()
                    && t.ky == Ratio::zero();
                if !is_translation || group.clip.is_some() {
                    // A rotate/scale/skew or a clip path inside — the exact
                    // "grouped shapes with a transform" case this pass doesn't
                    // attempt (see COVERAGE.md); bail to rasterize.
                    return false;
                }
                let sub_offset = pos + Point::new(t.tx, t.ty);
                if !collect_shapes(&group.frame, sub_offset, out) {
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

/// A resolved shape [`Geometry`](typst_library::visualize::Geometry) → its
/// [`RawSeg`] path, offset by `pos` (the frame item's own position — see the
/// note on why this matters below), or `None` for `Geometry::Rect` (an
/// axis-aligned rect/square fill — not yet supported in this composed path;
/// such a shape already maps natively when it is the sole top-level element
/// via [`shape`], just not composed with others here).
fn geometry_to_raw(
    geometry: &typst_library::visualize::Geometry,
    pos: typst_library::layout::Point,
) -> Option<Vec<RawSeg>> {
    use typst_library::visualize::Geometry;
    match geometry {
        Geometry::Curve(curve) => Some(raw_segments_from_curve(curve, pos)),
        // `layout_line` pushes its `Shape` at `start.to_point()` (not the
        // origin) and the geometry is only the *delta* from there — so `pos`
        // must be added to both ends, or the line's true start/end (and hence
        // its bounding box) comes out wrong.
        Geometry::Line(delta) => Some(vec![RawSeg::Move(pos), RawSeg::Line(pos + *delta)]),
        Geometry::Rect(_) => None,
    }
}

/// Builds the final [`Run::Drawing`] from every shape found in `frame`: `None`
/// if the frame holds anything not natively representable, a single native
/// shape if it holds exactly one, or a `wpg:wgp` group if it holds several.
fn build_shapes_drawing(
    ctx: &mut DocxCtx,
    frame: &typst_library::layout::Frame,
) -> SourceResult<Option<Run>> {
    let Some(shapes) = extract_shapes(frame) else { return Ok(None) };
    if shapes.is_empty() {
        return Ok(None);
    }

    if shapes.len() == 1 {
        let ExtractedShape { raw, fill, stroke } = shapes.into_iter().next().unwrap();
        let Some((segments, w, h)) = normalize_segments(raw) else { return Ok(None) };
        // A perfectly horizontal/vertical line is legitimately degenerate on
        // one axis; floor it to 1 EMU (imperceptible) rather than the 0 Word
        // handles poorly for a drawing extent. `normalize_segments` already
        // bailed when BOTH axes are degenerate (nothing to draw).
        let (w_emu, h_emu) = (abs_to_emu(w).max(1), abs_to_emu(h).max(1));
        let docpr_id = ctx.next_drawing_id();
        let name = ecow::eco_format!("Shape {docpr_id}");
        return Ok(Some(Run::Drawing(Drawing {
            rel: EcoString::new(),
            w_emu,
            h_emu,
            alt: None,
            docpr_id,
            name,
            anchor: None,
            shape: Some(ShapeSpec { geom: ShapeGeom::Path(segments), fill, stroke, txbx: None }),
            group: None,
        })));
    }

    // Several shapes: position each relative to the GROUP's own shared origin
    // (the union of every shape's own bounds), so their relative layout — not
    // just each one's own local geometry — is preserved.
    let bounds: Vec<_> = shapes.iter().map(|s| raw_bounds(&s.raw)).collect();
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
    let (group_w_emu, group_h_emu) = (abs_to_emu(group_w).max(1), abs_to_emu(group_h).max(1));

    let mut children = Vec::with_capacity(shapes.len());
    for (shape, (min_x, min_y, _, _)) in shapes.into_iter().zip(bounds) {
        let ExtractedShape { raw, fill, stroke } = shape;
        let Some((segments, w, h)) = normalize_segments(raw) else { continue };
        children.push(GroupChild {
            x_emu: abs_to_emu(min_x - group_min_x),
            y_emu: abs_to_emu(min_y - group_min_y),
            w_emu: abs_to_emu(w).max(1),
            h_emu: abs_to_emu(h).max(1),
            shape: ShapeSpec { geom: ShapeGeom::Path(segments), fill, stroke, txbx: None },
        });
    }
    if children.len() < 2 {
        // Every shape but one turned out degenerate after all; not worth a
        // group for a single survivor — fall back to rasterizing rather than
        // re-deriving the single-shape path for this rare edge case.
        return Ok(None);
    }

    let docpr_id = ctx.next_drawing_id();
    let name = ecow::eco_format!("Group {docpr_id}");
    Ok(Some(Run::Drawing(Drawing {
        rel: EcoString::new(),
        w_emu: group_w_emu,
        h_emu: group_h_emu,
        alt: None,
        docpr_id,
        name,
        anchor: None,
        shape: None,
        group: Some(GroupSpec { children }),
    })))
}

/// A resolved (post-layout) fill → its solid colour, or `None` (no fill).
/// Returns the OUTER `None` when the paint is a gradient/tiling/pattern — no
/// flat OOXML form — so the caller bails to rasterize.
fn resolved_fill(fill: &Option<Paint>) -> Option<Option<ShapeFill>> {
    fill_color(fill)
}

/// A resolved (post-layout) stroke → a uniform [`ShapeStroke`]. Same
/// outer/inner `None` convention as [`resolved_fill`].
fn resolved_stroke(
    stroke: &Option<typst_library::visualize::FixedStroke>,
) -> Option<Option<ShapeStroke>> {
    match stroke {
        None => Some(None),
        Some(fx) => match &fx.paint {
            Paint::Solid(c) => Some(Some(ShapeStroke {
                color: color_to_hex(c),
                w_emu: abs_to_emu(fx.thickness),
                cap: line_cap_to_ooxml(fx.cap),
                dash: fx.dash.as_ref().map(|d| prst_dash(&d.array, fx.thickness)),
            })),
            _ => None,
        },
    }
}

/// A curve/line command in the shape's own (possibly negative) coordinate
/// space, before the shift to OOXML's non-negative convention.
enum RawSeg {
    Move(typst_library::layout::Point),
    Line(typst_library::layout::Point),
    Cubic(
        typst_library::layout::Point,
        typst_library::layout::Point,
        typst_library::layout::Point,
    ),
    Close,
}

/// Lowers a resolved [`typst_library::visualize::Curve`] into [`RawSeg`]s,
/// mirroring the SVG/PDF exporters' own item walk (`CurveItem::Move` →
/// `RawSeg::Move`, …) — the direct, 1:1 translation of Typst's Bézier
/// vocabulary into OOXML's. `pos` is the frame item's own position (added to
/// every point; see the caller's note on why this matters).
fn raw_segments_from_curve(
    curve: &typst_library::visualize::Curve,
    pos: typst_library::layout::Point,
) -> Vec<RawSeg> {
    use typst_library::visualize::CurveItem;
    curve
        .0
        .iter()
        .map(|item| match item {
            CurveItem::Move(p) => RawSeg::Move(pos + *p),
            CurveItem::Line(p) => RawSeg::Line(pos + *p),
            CurveItem::Cubic(c1, c2, end) => {
                RawSeg::Cubic(pos + *c1, pos + *c2, pos + *end)
            }
            CurveItem::Close => RawSeg::Close,
        })
        .collect()
}

/// The conservative bounding box of a raw path — see [`normalize_segments`]'s
/// doc comment for why it's the control-point hull, not the tight curve
/// extent. Returns `(min_x, min_y, max_x, max_y)` in the path's own (possibly
/// negative) coordinate space. Shared by [`normalize_segments`] (a single
/// shape's own bounds) and [`build_shapes_drawing`] (each group child's bounds
/// relative to the whole group).
fn raw_bounds(raw: &[RawSeg]) -> (Abs, Abs, Abs, Abs) {
    let mut min_x = Abs::zero();
    let mut min_y = Abs::zero();
    let mut max_x = Abs::zero();
    let mut max_y = Abs::zero();
    let mut expand = |p: typst_library::layout::Point| {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    };
    for seg in raw {
        match seg {
            RawSeg::Move(p) | RawSeg::Line(p) => expand(*p),
            RawSeg::Cubic(c1, c2, end) => {
                expand(*c1);
                expand(*c2);
                expand(*end);
            }
            RawSeg::Close => {}
        }
    }
    (min_x, min_y, max_x, max_y)
}

/// Shifts a path so every coordinate is non-negative (the OOXML `a:custGeom`
/// convention — the path's own local space spans `[0, w] x [0, h]`) and
/// converts to EMU, returning the segments plus the path's true bounding size.
///
/// The bound is conservative rather than the mathematically tight curve
/// extent: for a cubic segment it includes the two control points as well as
/// the endpoints. A cubic Bézier always lies within the convex hull of its 4
/// control points, so this never clips the curve — unlike reusing the laid-out
/// frame's own `size()`, which only tracks the *positive* extent (see
/// `CurveBuilder::expand_bounds` in `typst-layout`) and would silently clip any
/// segment that dips negative.
fn normalize_segments(raw: Vec<RawSeg>) -> Option<(Vec<PathSegment>, Abs, Abs)> {
    let (min_x, min_y, max_x, max_y) = raw_bounds(&raw);
    let (w, h) = (max_x - min_x, max_y - min_y);
    if !w.to_pt().is_finite() || !h.to_pt().is_finite() {
        return None;
    }
    // A perfectly horizontal or vertical `#line` is legitimately degenerate on
    // ONE axis (it is a 1-D stroke, not a 2-D fill region) — floor that axis to
    // a single, visually imperceptible EMU rather than bailing to rasterize
    // (Word accepts a zero-size drawing extent poorly, but not a 1-EMU one). A
    // point (both axes degenerate — a zero-length line) has nothing to draw.
    if w.to_pt() <= 0.0 && h.to_pt() <= 0.0 {
        return None;
    }

    let shift = |p: typst_library::layout::Point| -> (i64, i64) {
        (abs_to_emu(p.x - min_x), abs_to_emu(p.y - min_y))
    };
    let segments = raw
        .into_iter()
        .map(|seg| match seg {
            RawSeg::Move(p) => {
                let (x, y) = shift(p);
                PathSegment::MoveTo(x, y)
            }
            RawSeg::Line(p) => {
                let (x, y) = shift(p);
                PathSegment::LineTo(x, y)
            }
            RawSeg::Cubic(c1, c2, end) => {
                let (c1x, c1y) = shift(c1);
                let (c2x, c2y) = shift(c2);
                let (ex, ey) = shift(end);
                PathSegment::CubicTo(c1x, c1y, c2x, c2y, ex, ey)
            }
            RawSeg::Close => PathSegment::Close,
        })
        .collect();
    Some((segments, w, h))
}

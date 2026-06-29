//! Math mapper: `EquationElem` → a Pandoc `Math` node (LaTeX string), with a
//! rasterize-to-`Image` safety net.
//!
//! Pandoc stores math as an opaque LaTeX *string*
//! (`Math InlineMath "<tex>"` / `Math DisplayMath "<tex>"`). Typst has no
//! Typst-math→LaTeX emitter, so we resolve the equation body to the [`MathItem`]
//! IR — the very same IR the DOCX backend lowers to OMML — via
//! [`resolve_equation`], then walk it into a LaTeX string with
//! [`crate::mappers::math_latex::emit`]. Inline equations become
//! `Math(InlineMath, latex)`; block (display) equations become a `Para` holding
//! a single `Math(DisplayMath, latex)`.
//!
//! **Safety net (no regressions).** The emitter returns
//! [`Unrepresentable`](super::math_latex::Unrepresentable) for any sub-construct
//! it cannot render cleanly — embedded `box(..)`/`External` content, an inner
//! introspection tag (a per-line `#<label>`), an unmapped non-ASCII glyph, an
//! unrepresentable delimiter/accent. In every such case we fall back to the
//! original rasterize-to-`Image` path for that whole equation, so we never emit
//! broken LaTeX and never lose content. A partial emitter that bails to raster
//! is a strict improvement over always-raster.
//!
//! Two correctness obligations carry over from the rasterize design:
//!
//! 1. **Drop the baked equation number.** The writer owns numbering in every
//!    Pandoc mapper. For the LaTeX path this is automatic (we emit only the body
//!    LaTeX, never `(N)`); for the rasterize fallback we lay the equation out
//!    under a style chain that sets `EquationElem::numbering = None` so the
//!    number is not baked into the pixels.
//!
//! 2. **Preserve inner labels/tags for convergence.** An equation body may carry
//!    introspection tags — a per-line `#<eqa>` label, a `#ref` target, a cite —
//!    that the introspector must still see, or the document never converges. The
//!    emitter treats any inner `MathItem::Tag` as unrepresentable and bails to
//!    the rasterize path, whose `ctx.rasterize` harvests the frame tags into
//!    `ctx.deferred_tags` BEFORE its degenerate-size drop. So a labeled inner
//!    line keeps its tag for free, via the fallback.
//!
//! A numbered/labeled equation additionally exposes its `anchor_id` on the
//! emitted node (a `Span` wrapping the inline `Math`, or the block `Para`) so a
//! `#ref` to the equation resolves to a real anchor in the shared id namespace.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, Style, StyleChain};
use typst_library::introspection::Locator;
use typst_library::math::EquationElem;
use typst_library::math::ir::resolve_equation;
use typst_library::routines::Arenas;
use typst_utils::LazyHash;

use crate::ast::{Block, Inline, MathType, empty_attr};
use crate::ctx::PandocCtx;

use super::math_latex;
use super::{rasterize_block, rasterize_inline};

/// An inline equation → an inline `Math(InlineMath, latex)` node, or, if the
/// equation cannot be cleanly emitted as LaTeX, a raster `Image` fallback.
pub fn equation_inline(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    if let Some(latex) = try_latex(elem, styles, ctx)? {
        let math = Inline::Math(MathType::InlineMath, latex);
        // Wrap in an id-carrying `Span` only when locatable, so `#ref(<eq>)`
        // resolves; otherwise emit the bare `Math` node.
        return Ok(vec![anchor_inline(elem, ctx, math)]);
    }

    // Fallback: rasterize (number stripped before layout, tags harvested).
    let reset = unnumbered();
    let styles = styles.chain(&reset);
    let mut inlines = rasterize_inline(elem.pack_ref(), styles, ctx)?;
    if let Some(loc) = elem.location()
        && let Some(Inline::Image(attr, _, _)) = inlines.first_mut()
    {
        attr.0 = ctx.anchor_id(loc).to_string();
    }
    Ok(inlines)
}

/// A block (display) equation → a `Para` holding `Math(DisplayMath, latex)`, or
/// a `Para` holding a raster `Image` if the equation is not cleanly LaTeX.
pub fn equation_block(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    if let Some(latex) = try_latex(elem, styles, ctx)? {
        let math = Inline::Math(MathType::DisplayMath, latex);
        let inline = anchor_inline(elem, ctx, math);
        return Ok(vec![Block::Para(vec![inline])]);
    }

    // Fallback: rasterize the display equation (number stripped, tags harvested).
    let reset = unnumbered();
    let styles = styles.chain(&reset);
    let mut blocks = rasterize_block(elem.pack_ref(), styles, ctx)?;
    if let Some(loc) = elem.location() {
        let id = ctx.anchor_id(loc);
        if let Some(Block::Para(inlines)) = blocks.first_mut()
            && let Some(Inline::Image(attr, _, _)) = inlines.first_mut()
        {
            attr.0 = id.to_string();
        }
    }
    Ok(blocks)
}

/// Resolves the equation body to the math IR and walks it into a LaTeX string,
/// returning `None` if the equation has no location (cannot be resolved) or if
/// any sub-item is unrepresentable (the caller then rasterizes).
fn try_latex(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Option<String>> {
    // An equation without a location cannot be IR-resolved (it would have been
    // assigned one during realization for any real document element); fall back.
    let Some(loc) = elem.location() else {
        return Ok(None);
    };

    // Resolve to the math IR. The IR borrows from `arenas`, which lives only for
    // this call — fine, we serialize to a LaTeX `String` immediately. Mirror the
    // HTML/DOCX backends: `Locator::synthesize` over the element's own location.
    let arenas = Arenas::default();
    let item =
        resolve_equation(elem, ctx.engine(), Locator::synthesize(loc), &arenas, styles)?;

    Ok(math_latex::emit(&item).ok())
}

/// Wraps an inline `Math` node in an id-carrying `Span` when the equation is
/// locatable (so `#ref(<eq>)` resolves to a real anchor), or returns the bare
/// node otherwise. Using a `Span` keeps the node a single inline while attaching
/// the shared-namespace anchor id; pandoc carries the id through to every writer.
fn anchor_inline(
    elem: &Packed<EquationElem>,
    ctx: &PandocCtx,
    math: Inline,
) -> Inline {
    match elem.location() {
        Some(loc) => {
            let mut attr = empty_attr();
            attr.0 = ctx.anchor_id(loc).to_string();
            Inline::Span(attr, vec![math])
        }
        None => math,
    }
}

/// Builds a one-property style forcing `EquationElem::numbering` to `None`, so
/// the rasterize fallback does not bake the equation number into the pixels.
fn unnumbered() -> LazyHash<Style> {
    EquationElem::numbering.set(None).wrap()
}

#[cfg(test)]
mod tests {
    use crate::ast::{Block, Inline, MathType};

    /// The inline-equation clean shape: a `Span` carrying the equation's anchor
    /// id and wrapping a single `Math InlineMath` node. The surrounding
    /// inter-word `Space`s are the caller's responsibility.
    #[test]
    fn inline_equation_is_span_math() {
        let math = Inline::Math(MathType::InlineMath, "a^2 + b^2".into());
        let mut attr = crate::ast::empty_attr();
        attr.0 = "ref-00ff".into();
        let span = Inline::Span(attr, vec![math]);
        let json = serde_json::to_value(&span).unwrap();
        assert_eq!(json["t"], "Span");
        // Span Attr id (c[0][0]) carries the anchor.
        assert_eq!(json["c"][0][0], "ref-00ff");
        // The wrapped node is a Math InlineMath with the LaTeX string.
        assert_eq!(json["c"][1][0]["t"], "Math");
        assert_eq!(json["c"][1][0]["c"][0]["t"], "InlineMath");
        assert_eq!(json["c"][1][0]["c"][1], "a^2 + b^2");
    }

    /// The block-equation clean shape: a `Para` holding a single
    /// `Math DisplayMath` node (no baked `(N)` number — the writer owns it).
    #[test]
    fn block_equation_is_para_display_math() {
        let math = Inline::Math(MathType::DisplayMath, "\\frac{1}{2}".into());
        let para = Block::Para(vec![math]);
        let json = serde_json::to_value(&para).unwrap();
        assert_eq!(json["t"], "Para");
        assert_eq!(json["c"][0]["t"], "Math");
        assert_eq!(json["c"][0]["c"][0]["t"], "DisplayMath");
        assert_eq!(json["c"][0]["c"][1], "\\frac{1}{2}");
        // Exactly one inline (the math) — no trailing number run.
        assert_eq!(json["c"].as_array().unwrap().len(), 1);
    }
}

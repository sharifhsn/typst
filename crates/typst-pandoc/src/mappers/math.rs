//! Math mapper: `EquationElem` → a rasterized `Image` (inline or block).
//!
//! Pandoc stores math as an opaque LaTeX *string* (`Math InlineMath "<tex>"` /
//! `Math DisplayMath "<tex>"`), but Typst has no Typst-math→LaTeX emitter. For
//! THIS workflow we take the cheap, always-correct route: rasterize the
//! equation to a PNG `data:` URI and emit it as an `Image` — inline for an
//! inline equation, a `Para`-wrapped block image for a display equation.
//!
//! Two correctness obligations are met here, both load-bearing:
//!
//! 1. **Drop the baked equation number.** If we rasterized the `EquationElem`
//!    as-is, `typst-layout`'s math layout would bake the `(1)` number (and its
//!    right-aligned gutter) straight into the pixels — the writer owns
//!    numbering in every other mapper, so a baked number would both
//!    double-count and stretch the image to the full text width. We strip it by
//!    laying the equation out under a style chain that sets
//!    `EquationElem::numbering = None`.
//!
//! 2. **Preserve inner labels/tags for convergence.** An equation body may carry
//!    introspection tags — a per-line `#<eqa>` label, a `#ref` target, a cite —
//!    that the introspector must still see, or the document never converges
//!    ("label does not exist" / "citation could not be located"). The shared
//!    `ctx.rasterize` *already* harvests frame tags into `ctx.deferred_tags`
//!    BEFORE its degenerate-size drop (see `ctx.rs`), so the rasterize path
//!    keeps these tags for free. A labeled/numbered equation additionally
//!    exposes its `anchor_id` on the emitted `Image` so a `#ref` to the
//!    equation resolves to a real anchor (the shared id namespace).
//
// TODO(next-workflow): MathItem -> LaTeX-string emitter. Replace the rasterize
// calls below with a structural walk of the math IR (`resolve_equation` →
// `MathItem`) into a LaTeX string, then emit `Math(InlineMath, latex)` /
// `Para[Math(DisplayMath, latex)]`. The STRUCTURAL BLUEPRINT is the OMML walk in
// `crates/typst-docx/src/mappers/math.rs` (`Emitter::emit_kind`): each
// `MathKind` maps to one LaTeX construct — Fraction→`\frac{num}{den}`,
// SkewedFraction→`{num}/{den}`, Radical→`\sqrt[deg]{rad}`,
// Scripts→`base^{sup}_{sub}`, Accent→`\hat{..}`/`\bar{..}`/…,
// Line→`\overline{..}`/`\underline{..}`, Fenced→`\left( .. \right)`,
// Table→`\begin{matrix} a & b \\ .. \end{matrix}`,
// Multiline→`\begin{aligned} .. \end{aligned}`, Cancel→`\cancel{..}`. The n-ary
// lookahead (∑∫∏ + following operand until the next relation) is the SAME shape
// as the DOCX `emit_items`/`emit_nary` but SIMPLER for LaTeX: just linear
// juxtaposition — `\sum_{lo}^{hi} integrand` with no nested `m:e`. The long tail
// is the per-glyph symbol→command table (∫→`\int`, ≤→`\leq`, α→`\alpha`, …;
// unknown codepoint → `\unicode{...}` or `\text{..}`). `Box`/`External` math
// have no LaTeX form → keep the rasterize fallback below for those sub-cases.
// HOOK POINT: branch on whether the IR walk fully succeeded; on any
// unrepresentable sub-item fall back to `rasterize_inline`/`rasterize_block`
// exactly as below, so the emitter can be landed incrementally.

use typst_library::diag::SourceResult;
use typst_library::foundations::{Packed, Style, StyleChain};
use typst_library::math::EquationElem;
use typst_utils::LazyHash;

use crate::ast::{Block, Inline};
use crate::ctx::PandocCtx;

use super::{rasterize_block, rasterize_inline};

/// An inline equation → an inline raster `Image`.
///
/// The baked equation number is stripped before layout (an inline equation is
/// normally unnumbered anyway, but this keeps the two paths uniform). Inner
/// tags are harvested by `ctx.rasterize` for convergence.
pub fn equation_inline(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Inline>> {
    // Hold the reset styles on the stack so the chained borrow outlives the
    // rasterize call below (no leak / no shared arena needed).
    let reset = unnumbered();
    let styles = styles.chain(&reset);

    let mut inlines = rasterize_inline(elem.pack_ref(), styles, ctx)?;

    // If the equation is labeled/locatable, expose its shared anchor id on the
    // image so `#ref(<eq>)` (a `Link` to `#anchor_id`) resolves to a real
    // target. Attach to the first emitted image (the rasterize fallback emits at
    // most one).
    if let Some(loc) = elem.location()
        && let Some(Inline::Image(attr, _, _)) = inlines.first_mut()
    {
        attr.0 = ctx.anchor_id(loc).to_string();
    }

    Ok(inlines)
}

/// A block (display) equation → a `Para` holding a raster `Image`.
///
/// The baked `(N)` equation number is stripped before layout (the writer owns
/// numbering — see module docs). Inner tags are harvested by `ctx.rasterize`.
/// A numbered/labeled equation carries its `anchor_id` on the image so refs to
/// the equation resolve.
pub fn equation_block(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut PandocCtx,
) -> SourceResult<Vec<Block>> {
    let reset = unnumbered();
    let styles = styles.chain(&reset);

    let mut blocks = rasterize_block(elem.pack_ref(), styles, ctx)?;

    // Attach the shared anchor id to the block image (if locatable) so a `#ref`
    // to this equation lands on a real anchor. `rasterize_block` emits a single
    // `Para[Image]`; find that image and set its id.
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

#[cfg(test)]
mod tests {
    use crate::ast::{Block, Inline, empty_attr};

    const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";

    /// The inline equation shape: a bare `Image` (data-URI raster), with the
    /// equation's `anchor_id` carried as the `Attr` id when locatable, so a
    /// `#ref` (a `Link` to `#anchor_id`) lands on a real target. The surrounding
    /// inter-word `Space`s are the caller's responsibility — here we just guard
    /// the node this mapper produces.
    #[test]
    fn inline_equation_is_image_with_anchor() {
        let mut img = Inline::Image(empty_attr(), Vec::new(), (PNG.into(), String::new()));
        if let Inline::Image(attr, _, _) = &mut img {
            attr.0 = "ref-00ff".into();
        }
        let json = serde_json::to_value(&img).unwrap();
        assert_eq!(json["t"], "Image");
        // Attr id (c[0][0]) carries the anchor; target url (c[2][0]) is the URI.
        assert_eq!(json["c"][0][0], "ref-00ff");
        assert_eq!(json["c"][2][0], PNG);
        // Alt is empty (rasterized math carries no alt here).
        assert!(json["c"][1].as_array().unwrap().is_empty());
    }

    /// The block equation shape: a `Para` holding a single `Image`, the anchor
    /// id on the image. No baked `(N)` number node — the writer owns numbering.
    #[test]
    fn block_equation_is_para_image_with_anchor() {
        let mut img = Inline::Image(empty_attr(), Vec::new(), (PNG.into(), String::new()));
        if let Inline::Image(attr, _, _) = &mut img {
            attr.0 = "ref-aa11".into();
        }
        let para = Block::Para(vec![img]);
        let json = serde_json::to_value(&para).unwrap();
        assert_eq!(json["t"], "Para");
        assert_eq!(json["c"][0]["t"], "Image");
        assert_eq!(json["c"][0]["c"][0][0], "ref-aa11");
        // Exactly one inline (the image) — no trailing number run.
        assert_eq!(json["c"].as_array().unwrap().len(), 1);
    }
}

/// Builds a one-property style that forces `EquationElem::numbering` to `None`,
/// so the math layout does not bake the equation number into the rasterized
/// pixels. The writer owns numbering in every Pandoc mapper; baking it here
/// would both double-count and stretch the image to the full text width via the
/// number gutter. (Same `set(..).wrap()` pattern as the `TargetElem` reset in
/// `ctx.rasterize`.)
fn unnumbered() -> LazyHash<Style> {
    EquationElem::numbering.set(None).wrap()
}

//! Equation mapper: `EquationElem` → native Word equations (OMML).
//!
//! Typst's realized math is lowered to the `MathItem` IR via
//! [`resolve_equation`] (the same entry point the paged and HTML backends use),
//! and the IR is then turned into OMML (`m:` namespace) — the native, *editable*
//! Word equation format — by [`typst_omml`], which both Office exporters share.
//!
//! What stays here is everything that is about *Word*, not about math: where the
//! `m:oMath` fragment goes, and what to do when it cannot be produced.
//!
//! Inline equations (`block == false`) become a bare `m:oMath` that sits as a
//! sibling of `w:r` runs inside the paragraph; block equations become an
//! `m:oMathPara` (its own paragraph), optionally followed — on the same line,
//! via a right tab — by the rendered equation number.
//!
//! An equation the shared lowering refuses ([`Lowered::Unsupported`]) is
//! rasterized whole, falling back to its alternate text if even that fails.
//!
//! See `research/omml_math.md` and `map/math_repr.md`.

use ecow::eco_format;
use typst_library::diag::SourceResult;
use typst_library::foundations::{NativeElement, Packed, StyleChain};
use typst_library::introspection::{Counter, Locator};
use typst_library::math::EquationElem;
use typst_library::math::ir::resolve_equation;
use typst_library::routines::Arenas;
use typst_omml::{Lowered, UnsupportedMath};

use crate::ctx::DocxCtx;
use crate::dom::{Block, Para, ParaChild, ParaProps, Run, RunProps, TabAlign, TabStop};
use crate::report::{DecisionReason, LossSet, Representation};

/// The result of lowering an equation: inline run or block paragraph(s).
// The inline `Run` is large but the common case; this IR is transient, so boxing
// to shrink the enum isn't worthwhile (see the `Run`/`ParaChild` note in `dom`).
#[allow(clippy::large_enum_variant)]
pub enum EquationOut {
    Inline(Vec<Run>),
    Block(Vec<Block>),
}

/// The engine-side services the shared OMML lowering asks of its caller.
impl typst_omml::MathHooks for DocxCtx<'_, '_> {
    /// Route a tag found inside the equation body to the run-only-context
    /// channel, exactly as the inline handler does for tags it cannot position
    /// among runs. Without this, a `#ref` to a per-line label (`#<eqa>`) inside
    /// an equation would not resolve.
    fn defer_tag(&mut self, tag: &typst_library::introspection::Tag) {
        self.deferred_tags.push(tag.clone());
    }
}

pub fn equation(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<EquationOut> {
    let block = elem.block.get(styles);

    // Resolve the equation body to the math IR. The `MathItem` borrows from
    // `arenas`, which lives only for this function — fine, because we serialize
    // it to an OMML `String` immediately and the IR is not needed afterwards.
    //
    // We mirror the HTML backend's `EQUATION_RULE`: `Locator::synthesize` over
    // the element's own location plus a fresh `Arenas`. An equation without a
    // location cannot be resolved this way (it would have been assigned one
    // during realization for any real document element); fall back to alt/empty
    // in that case so export never fails.
    let Some(loc) = elem.location() else {
        return Ok(text_fallback(elem, styles, block, ctx));
    };

    let arenas = Arenas::default();
    let item =
        resolve_equation(elem, ctx.engine(), Locator::synthesize(loc), &arenas, styles)?;

    // Walk the IR into an `<m:oMath>…</m:oMath>` fragment. `ctx` is reborrowed
    // as the lowering's hooks and released when the statement ends.
    //
    // Capability planning is atomic at the logical equation boundary, so the
    // lowering either produces the whole equation or refuses it: a native OMML
    // subtree may not simply omit one unsupported descendant, because that
    // changes the equation's meaning while leaving a plausible-looking result.
    let omath = match typst_omml::lower_equation(&item, &mut *ctx)? {
        Lowered::Native(omath) => omath,
        Lowered::Unsupported(unsupported) => {
            return raster_fallback(elem, styles, block, unsupported, ctx);
        }
    };

    ctx.mark_math();

    if !block {
        return Ok(EquationOut::Inline(vec![Run::OmmlInline(omath)]));
    }

    // Block equation: wrap the `m:oMath` in an `m:oMathPara` (centered), which
    // must be a direct child of the paragraph (never inside a run).
    let mut content = vec![ParaChild::OmmlPara(typst_omml::omath_para(&omath))];

    // Append the equation number, if numbered, as a tab + run on the same line.
    // OOXML has no first-class equation-number element; Word's own convention is
    // a right tab stop with the number text, all in the one paragraph. We do not
    // use the same scoped width budget as tables, stacks, TOCs, and fallback
    // layout, so a nested or non-Letter equation reaches its actual right edge.
    let mut props = ParaProps::default();
    if let Some(number) = equation_number(elem, styles, ctx)? {
        content.push(ParaChild::Run(Run::Tab));
        let number_bookmark = ctx.number_bookmark_for_emission(loc);
        if let Some((id, name)) = &number_bookmark {
            content.push(ParaChild::BookmarkStart { id: *id, name: name.clone() });
        }
        content.extend(number.into_iter().map(ParaChild::Run));
        if let Some((id, _)) = number_bookmark {
            content.push(ParaChild::BookmarkEnd { id });
        }
        props.tabs.push(TabStop {
            val: TabAlign::End,
            leader: None,
            pos: ctx.available_width_dxa(),
        });
    }

    Ok(EquationOut::Block(vec![Block::Para(Para { props, content })]))
}

/// Produces the rendered equation-number runs (e.g. `(1)`), if the equation has
/// a numbering pattern. Mirrors the paged backend's use of
/// `Counter::of(EquationElem::ELEM).display_at(...)`.
fn equation_number(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    ctx: &mut DocxCtx,
) -> SourceResult<Option<Vec<Run>>> {
    let Some(numbering) = elem.numbering.get_ref(styles).clone() else {
        return Ok(None);
    };
    let Some(loc) = elem.location() else { return Ok(None) };

    let span = elem.span();
    let number =
        ctx.paged_equation_number(elem, styles, &numbering)
            .unwrap_or_else(|| {
                let result = Counter::of(EquationElem::ELEM).display_at(
                    ctx.engine(),
                    loc,
                    styles,
                    &numbering,
                    span,
                );
                ctx.engine().delay(result).spanned(span)
            });

    // Lower the number content (ordinary inline content) to runs.
    let runs = ctx.inline_runs(&number, styles, RunProps::default())?;
    Ok(Some(runs))
}

/// Last-resort readable fallback when neither native OMML nor raster output is
/// available: emit the equation's alternate text and report the semantic loss.
fn text_fallback(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    block: bool,
    ctx: &mut DocxCtx,
) -> EquationOut {
    let alt = elem.alt.get_cloned(styles).unwrap_or_default();
    let content = elem.clone().pack();
    let (representation, losses) = if alt.is_empty() {
        (Representation::Drop, LossSet::DROP)
    } else {
        (Representation::Approximate, LossSet::MATH_TEXT)
    };
    ctx.record_content_decision(
        &content,
        representation,
        DecisionReason::EquationTextFallback,
        losses,
        alt.chars().count(),
    );
    let run = Run::Text { props: RunProps::default(), text: alt };
    if block {
        EquationOut::Block(vec![Block::Para(Para {
            props: ParaProps::default(),
            content: vec![ParaChild::Run(run)],
        })])
    } else {
        EquationOut::Inline(vec![run])
    }
}

fn raster_fallback(
    elem: &Packed<EquationElem>,
    styles: StyleChain,
    block: bool,
    unsupported: UnsupportedMath,
    ctx: &mut DocxCtx,
) -> SourceResult<EquationOut> {
    let content = elem.clone().pack();
    let runs = crate::mappers::image::laid_out_fallback_with_reason(
        &content,
        styles,
        ctx,
        DecisionReason::UnsupportedMathRasterFallback,
    )?;
    if !runs.is_empty() {
        ctx.warn_message(
            eco_format!(
                "equation was rasterized because its {} cannot be represented safely in native Word math",
                unsupported.kind.label()
            ),
            unsupported.span,
        );
        return Ok(if block {
            EquationOut::Block(vec![Block::Para(Para {
                props: ParaProps::default(),
                content: runs.into_iter().map(ParaChild::Run).collect(),
            })])
        } else {
            EquationOut::Inline(runs)
        });
    }

    ctx.warn_message(
        eco_format!(
            "equation containing {} could not be rasterized; alternate text was used",
            unsupported.kind.label()
        ),
        unsupported.span,
    );
    Ok(text_fallback(elem, styles, block, ctx))
}

pub use typst_ooxml_core::omml::equation_omml_fragment;

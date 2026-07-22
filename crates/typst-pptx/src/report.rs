//! Structured fidelity decisions for PPTX export.
//!
//! Mirrors [`typst_docx`'s fidelity report](../../typst-docx/src/report.rs):
//! every place the exporter falls back to a picture, approximates a paint, or
//! drops content records itself here as a [`Representation`] + [`LossSet`] +
//! [`DecisionReason`], instead of being observable only through
//! `PPTX_DEBUG_RASTER` eprintln lines or this crate's README prose.
//!
//! The identity unit differs from DOCX's on purpose. DOCX walks Typst's
//! *realized content tree* and can key a decision to a stable source
//! [`Location`](typst_library::introspection::Location)/span. This exporter
//! instead walks an already laid-out `PagedDocument` page by page (see the
//! crate root docs) — there is no show-rule tree and no per-node identity to
//! recover after layout. A decision's natural "source" here is therefore the
//! slide it happened on: repeated occurrences of the same reason on the same
//! slide aggregate into one row with an incremented count, the same
//! aggregation behavior DOCX applies to repeated realizations of one region.
//!
//! The variant list below is deliberately smaller than DOCX's ~40 reasons: it
//! is derived directly from this exporter's own fallback sites (frame-walk
//! rasterization, table-transform fallback, tiling/gradient-text
//! approximation, the math OMML/text-fallback pair, and page-background
//! substitution), not copied from DOCX's much larger native-format surface.

use ecow::EcoString;

/// The representation selected for one logical exported region.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Representation {
    /// Editable target-native DrawingML/PresentationML semantics, with no
    /// known fidelity loss.
    Native,
    /// A native representation plus a compatibility fallback (OMML behind an
    /// `mc:AlternateContent` text branch).
    NativeWithFallback,
    /// Editable/native output with a known visual or semantic difference.
    Approximate,
    /// A rendered picture because no native representation was safe or
    /// possible.
    Raster,
    /// No representation was emitted.
    Drop,
}

/// Why a representation was selected.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DecisionReason {
    /// A group's clip or local transform (skew, non-uniform scale, or a clip
    /// shape the render probe could not prove a no-op) has no DrawingML
    /// equivalent, so its whole subtree was rendered to one positioned
    /// picture instead of walked natively.
    UnrepresentableGroupRasterFallback,
    /// Text under a skewed or non-uniformly scaled transform cannot be
    /// expressed as a DrawingML run, so it was rendered to a picture (with
    /// its source text kept alongside as invisible, searchable fallback
    /// text).
    UnrepresentableTextTransformRasterFallback,
    /// A shape's local transform is not a similarity — a skew, a non-uniform
    /// scale, or a reflection — so its path could not be placed as a native
    /// `custGeom`/`prstGeom` and the shape was rendered to a picture.
    ///
    /// Measured, not assumed: no frame walk reaches this today. A
    /// non-similarity always arrives on a *group*, which rasterizes as
    /// [`Self::UnrepresentableGroupRasterFallback`] before the walk descends
    /// to the shape, and composing similarities only ever yields another
    /// similarity. The guard stays because `shape_to_geom` is also callable
    /// on its own, but a deck reporting this reason means the group walk
    /// changed.
    UnmappableShapeTransformRasterFallback,
    /// A shape's path is empty, degenerate (zero width *and* height), or has
    /// non-finite bounds, so there was no `custGeom` to write.
    UnmappableShapeGeometryRasterFallback,
    /// A shape's fill paint has no DrawingML equivalent — an off-center
    /// radial gradient, a conic gradient, or a tiling whose tile could not be
    /// rendered — so the shape was rendered to a picture rather than shipped
    /// with a visibly wrong fill.
    UnmappableShapeFillRasterFallback,
    /// A shape's stroke is painted with a gradient or a tiling, and `a:ln`
    /// carries a solid color only.
    UnmappableShapeStrokeRasterFallback,
    /// A picture's placement is neither a translation nor a rotation with
    /// uniform scale — a skew, a non-uniform scale, or a reflection — so it
    /// cannot be expressed as an `a:xfrm` box plus `rot` and was rendered at
    /// its final on-slide transform instead. Rotated and uniformly scaled
    /// pictures are native (see `Walker::try_emit_rotated_image`); this also
    /// covers an image whose bytes could not be embedded or rendered at all.
    RotatedOrScaledImageRasterFallback,
    /// A table cell region's transform was not an axis-aligned similarity
    /// (rotation, skew, or non-uniform scale), so the whole table — including
    /// cells already captured natively — was rasterized cell by cell instead
    /// of mixing a partial native table with fallback pictures.
    TransformedTableRasterFallback,
    /// A procedural tiling/pattern fill was rendered once to a PNG tile and
    /// placed as a native DrawingML tile fill (`a:tile`) instead of staying a
    /// live, parametric pattern.
    TilingFillRasterizedApproximation,
    /// Text painted with a gradient or tiling fill was approximated with one
    /// representative solid color, because a DrawingML text run can only
    /// carry a single solid color.
    GradientOrTilingTextFillApproximation,
    /// A page fill that is not a solid color or linear gradient (a tiling
    /// pattern, a conic gradient, or an off-center radial gradient) has no
    /// native `p:bg` equivalent, so the slide silently received a plain white
    /// background instead.
    PageBackgroundWhiteFallback,
    /// An equation was emitted as native OMML behind an `mc:AlternateContent`
    /// compatibility switch, with a compact plain-text DrawingML run as the
    /// fallback branch for consumers without the `a14`/OMML extension (older
    /// Office versions, or LibreOffice Impress).
    ///
    /// Two things do not survive even on this, the best path, because the
    /// equation is re-resolved from the introspector after layout rather than
    /// during realization:
    ///
    /// * An ambient `#show` recipe on a math symbol does not fire. A document
    ///   that rewrites, say, every `x` in math gets the unrewritten symbol.
    /// * A `#context` read inside math resolves against default styles, not
    ///   the styles in force where the equation sits.
    ///
    /// The element's own show-set styles *are* re-applied, so display-versus-
    /// inline sizing (and with it the placement of an n-ary operator's limits)
    /// is exact.
    MathOmmlWithTextFallback,
    /// An equation's start/end tag pair produced no discoverable OMML source
    /// or no non-empty geometry bounds, so nothing was emitted for it at all.
    MathSourceUnavailableDrop,
    /// An equation contains a construct with no faithful OMML form (an inline
    /// `box(..)`, or package-built external content), so no math object was
    /// started for it and its laid-out glyphs and rules were painted as
    /// ordinary text runs and shapes instead.
    UnsupportedMathTextFallback,
    /// An equation body could not be resolved to Typst's math IR after layout,
    /// so it took the same painted-text fallback as
    /// [`Self::UnsupportedMathTextFallback`]. The document itself compiled —
    /// only this post-layout re-resolution failed — so the export continues
    /// rather than failing.
    UnresolvableMathTextFallback,
}

/// Independent dimensions in which a representation can lose information.
/// Same shape as `typst_docx::report::LossSet`.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct LossSet {
    pub visual_fidelity: bool,
    pub semantic_structure: bool,
    pub editability: bool,
    pub dynamic_behavior: bool,
    pub accessibility: bool,
    pub portability: bool,
}

impl LossSet {
    /// A whole-region raster fallback: pixels and portability survive, but
    /// native structure, editing, dynamic behavior, and accessibility do not
    /// — even when hidden searchable text is recovered alongside the
    /// picture.
    pub const RASTER: Self = Self {
        visual_fidelity: false,
        semantic_structure: true,
        editability: true,
        dynamic_behavior: true,
        accessibility: true,
        portability: false,
    };

    /// No representation survived at all.
    pub const DROP: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: true,
        dynamic_behavior: true,
        accessibility: true,
        portability: true,
    };

    /// A procedural pattern became a static rendered tile image: the shape
    /// stays native and editable, but the fill is no longer a recomputable
    /// pattern and its "this is a tiling" structure is gone.
    pub const TILE_FILL: Self = Self {
        visual_fidelity: false,
        semantic_structure: true,
        editability: false,
        dynamic_behavior: true,
        accessibility: false,
        portability: false,
    };

    /// A representative solid color stands in for a multi-stop paint: the run
    /// stays live and editable, but its exact appearance and paint kind are
    /// both approximate.
    pub const TEXT_FILL_COLOR: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: false,
        dynamic_behavior: false,
        accessibility: false,
        portability: false,
    };

    /// A wrong-but-native, still-editable background color stands in for an
    /// unrepresentable page fill.
    pub const BACKGROUND_COLOR: Self = Self {
        visual_fidelity: true,
        semantic_structure: false,
        editability: false,
        dynamic_behavior: false,
        accessibility: false,
        portability: false,
    };

    /// Exact and editable, but a consumer without the OMML compatibility
    /// extension recalculates nothing and only ever sees the static text
    /// branch.
    pub const MATH_NATIVE_WITH_FALLBACK: Self = Self {
        visual_fidelity: false,
        semantic_structure: false,
        editability: false,
        dynamic_behavior: true,
        accessibility: false,
        portability: false,
    };

    /// An equation that stayed in the frame walk: its glyphs and rules are
    /// painted as ordinary text runs and shapes. Everything about it being an
    /// equation is gone — it cannot be edited as math, reads to a screen
    /// reader as loose characters, and its glyph positions are now subject to
    /// the text clustering every other run goes through rather than to math
    /// layout.
    pub const MATH_LAID_OUT_TEXT: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: true,
        dynamic_behavior: true,
        accessibility: true,
        portability: false,
    };
}

/// Identifies one exported region for aggregation.
///
/// See the module doc for why this is a slide index rather than a Typst
/// source span: this exporter has no realized-content `Location` to key off
/// of after layout.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct ExportSource {
    /// Zero-based index of the slide the decision was made on.
    pub slide_index: usize,
}

/// One selected representation for a logical exported region.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ExportDecision {
    pub source: ExportSource,
    pub representation: Representation,
    pub reason: DecisionReason,
    pub losses: LossSet,
    /// Repeated instances of the same reason on the same slide are
    /// aggregated here instead of producing duplicate rows.
    pub occurrences: usize,
    /// Live text characters swallowed by a raster fallback, when known.
    pub affected_text_chars: usize,
}

/// Counts of recorded representation decisions.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct RepresentationCounts {
    pub native: usize,
    pub native_with_fallback: usize,
    pub approximate: usize,
    pub raster: usize,
    pub drop: usize,
}

/// One embedded-font decision for the finalized package's font table.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontFact {
    pub family: EcoString,
    /// Human-readable style slot: `"Regular"`, `"Bold"`, `"Italic"`, or
    /// `"Bold Italic"` (PresentationML's four embedded-font style slots).
    pub style: EcoString,
    /// Whether an OpenType-license-permitted program for this exact
    /// family/style was embedded as a PresentationML font part. When
    /// `false`, the license did not grant Installable/Editable embedding (or
    /// the face was a collection, or too small/malformed to parse), and
    /// editable text using this face falls back to whatever the consumer
    /// substitutes.
    pub embedded: bool,
    pub occurrences: usize,
}

/// Structured, queryable evidence about PPTX fidelity decisions.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FidelityReport {
    decisions: Vec<ExportDecision>,
    fonts: Vec<FontFact>,
}

impl FidelityReport {
    /// Recorded representation decisions in first-seen order.
    pub fn decisions(&self) -> &[ExportDecision] {
        &self.decisions
    }

    /// Font families referenced by editable text runs, and whether each was
    /// embedded.
    pub fn fonts(&self) -> &[FontFact] {
        &self.fonts
    }

    /// Aggregate representation counts, including repeated occurrences.
    pub fn counts(&self) -> RepresentationCounts {
        let mut counts = RepresentationCounts::default();
        for decision in &self.decisions {
            let slot = match decision.representation {
                Representation::Native => &mut counts.native,
                Representation::NativeWithFallback => &mut counts.native_with_fallback,
                Representation::Approximate => &mut counts.approximate,
                Representation::Raster => &mut counts.raster,
                Representation::Drop => &mut counts.drop,
            };
            *slot += decision.occurrences;
        }
        counts
    }

    pub(crate) fn record(
        &mut self,
        slide_index: usize,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
        affected_text_chars: usize,
    ) {
        let source = ExportSource { slide_index };
        if let Some(existing) = self.decisions.iter_mut().find(|decision| {
            decision.source == source
                && decision.representation == representation
                && decision.reason == reason
        }) {
            existing.occurrences += 1;
            existing.affected_text_chars += affected_text_chars;
            return;
        }
        self.decisions.push(ExportDecision {
            source,
            representation,
            reason,
            losses,
            occurrences: 1,
            affected_text_chars,
        });
    }

    pub(crate) fn record_font(&mut self, family: &str, style: &str, embedded: bool) {
        if let Some(existing) = self
            .fonts
            .iter_mut()
            .find(|font| font.family == family && font.style == style)
        {
            existing.occurrences += 1;
            existing.embedded |= embedded;
            return;
        }
        self.fonts.push(FontFact {
            family: family.into(),
            style: style.into(),
            embedded,
            occurrences: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_decisions_on_one_slide_are_aggregated() {
        let mut report = FidelityReport::default();
        report.record(
            0,
            Representation::Raster,
            DecisionReason::UnmappableShapeGeometryRasterFallback,
            LossSet::RASTER,
            4,
        );
        report.record(
            0,
            Representation::Raster,
            DecisionReason::UnmappableShapeGeometryRasterFallback,
            LossSet::RASTER,
            7,
        );

        assert_eq!(report.decisions().len(), 1);
        assert_eq!(report.decisions()[0].occurrences, 2);
        assert_eq!(report.decisions()[0].affected_text_chars, 11);
        assert_eq!(report.counts().raster, 2);
    }

    #[test]
    fn decisions_on_different_slides_do_not_aggregate() {
        let mut report = FidelityReport::default();
        report.record(
            0,
            Representation::Raster,
            DecisionReason::UnmappableShapeGeometryRasterFallback,
            LossSet::RASTER,
            0,
        );
        report.record(
            1,
            Representation::Raster,
            DecisionReason::UnmappableShapeGeometryRasterFallback,
            LossSet::RASTER,
            0,
        );

        assert_eq!(report.decisions().len(), 2);
        assert_eq!(report.counts().raster, 2);
    }

    #[test]
    fn different_reasons_on_the_same_slide_stay_distinct() {
        let mut report = FidelityReport::default();
        report.record(
            0,
            Representation::Raster,
            DecisionReason::UnmappableShapeGeometryRasterFallback,
            LossSet::RASTER,
            0,
        );
        report.record(
            0,
            Representation::Approximate,
            DecisionReason::GradientOrTilingTextFillApproximation,
            LossSet::TEXT_FILL_COLOR,
            0,
        );

        assert_eq!(report.decisions().len(), 2);
        let counts = report.counts();
        assert_eq!(counts.raster, 1);
        assert_eq!(counts.approximate, 1);
    }

    #[test]
    fn repeated_font_use_is_aggregated_and_embedded_sticks() {
        let mut report = FidelityReport::default();
        report.record_font("Libertinus Serif", "Regular", false);
        report.record_font("Libertinus Serif", "Regular", true);
        report.record_font("Libertinus Serif", "Regular", false);

        assert_eq!(report.fonts().len(), 1);
        let fact = &report.fonts()[0];
        assert_eq!(fact.occurrences, 3);
        assert!(fact.embedded, "embedded should stick once true");
    }
}

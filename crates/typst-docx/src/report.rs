//! Structured fidelity decisions and suppressed diagnostics for DOCX export.
//!
//! The report deliberately lives beside the typed DOCX IR rather than in the
//! encoder. Representation policy is decided while lowering Typst content; the
//! OOXML serializer should only encode that already-decided plan.

use ecow::EcoString;
use typst_library::diag::SourceDiagnostic;
use typst_library::foundations::Content;
use typst_library::introspection::Location;
use typst_syntax::Span;

/// The representation selected for one logical source region.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Representation {
    /// Editable target-native Word semantics.
    Native,
    /// A native representation plus a compatibility fallback.
    NativeWithFallback,
    /// Editable/native output with a known visual or semantic difference.
    Approximate,
    /// A rendered image because a native representation is unsafe.
    Raster,
    /// No representation was emitted.
    Drop,
}

/// Why a representation was selected.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DecisionReason {
    /// Generic whole-region PNG fallback.
    RasterFallback,
    /// A whole equation was rasterized because one child could not be emitted
    /// safely in native OMML.
    UnsupportedMathRasterFallback,
    /// Native equation emission and rasterization were unavailable, so only
    /// the equation's alternate text survived.
    EquationTextFallback,
    /// A page-relative background or foreground rendered as a PNG.
    PageOverlayRasterFallback,
    /// A block layout callback and its whole-region paged fallback both failed,
    /// so no safe representation could be emitted.
    LayoutCallbackUnavailable,
    /// Native SVG with the PNG branch required by Office compatibility markup.
    SvgWithPngFallback,
    /// The target format cannot express the source feature.
    UnsupportedContent,
    /// A positional link kept its text but lost the interactive target.
    PositionalLinkTarget,
    /// A semantic reference keeps Typst's computed text because Word's `REF`
    /// evaluator cannot reproduce Typst supplements and numbering semantics.
    TypstOwnedReferenceText,
    /// A figure number keeps Typst's computed value because its numbering
    /// pattern or function has no equivalent Word `SEQ` format.
    TypstOwnedFigureNumber,
    /// Typst could not compute a trustworthy cached field result, so the
    /// consumer must provide a best-effort value from the native field code.
    FieldCacheUnavailable,
    /// A page reference was planned as one atomic semantic group: its localized
    /// supplement is Typst-owned text and its numeric value is a live PAGEREF.
    NativePageReference,
    /// A standalone figure caption could not be realized natively, so visible
    /// editable text recovered from paged layout preserves the whole caption.
    StandaloneCaptionTextFallback,
    /// Both native realization and visible-text recovery failed, so a
    /// standalone figure caption was preserved as one rendered region plus
    /// searchable fallback text.
    StandaloneCaptionRasterFallback,
    /// Neither native standalone-caption realization nor its whole-region
    /// fallback produced output.
    StandaloneCaptionUnavailable,
    /// Section content failed to lower, so only its geometry was retained.
    SectionGeometryFallback,
    /// Fractional stack spacing depends on leftover region geometry. DOCX keeps
    /// it as a flexible table track, but body widths remain a flow-model
    /// approximation rather than Typst's measured frame widths.
    FlexibleStackSpacing,
    /// Placed text was preserved as an editable anchored Word text box.
    PositionedTextBox,
    /// Placed visual content was preserved as a native anchored drawing.
    PositionedDrawing,
    /// Rich placed content stayed editable in the main story because it was
    /// unsafe or illegal inside a Word text box, losing exact placement.
    PositionedContentFlowFallback,
    /// Non-native placed content was rasterized as one anchored region so its
    /// position and appearance survived atomically.
    PositionedContentRasterFallback,
    /// Placed content produced neither editable flow nor an anchorable
    /// whole-region fallback.
    PositionedContentUnavailable,
    /// Contextual page furniture varied beyond Word's first/even/default
    /// header model, so one sampled value is repeated rather than pretending
    /// that a page-specific value is parity-stable.
    PageFurnitureSampled,
    /// A resolved table/grid was emitted as native editable `w:tbl` without a
    /// known table-level representation loss.
    NativeTable,
    /// A table/grid stayed native and editable, but its cell paint, border
    /// nuance, or track sizing cannot be reproduced exactly by Word.
    TableGeometryApproximation,
    /// A table/grid had no resolved `CellGrid`, and both native lowering and
    /// whole-region fallback produced no representation.
    TableResolutionUnavailable,
}

/// Independent dimensions in which a representation can lose information.
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
    /// A whole-region raster fallback: designed to keep pixels and portability,
    /// but native structure, editing, dynamic behavior, and rich accessibility
    /// are reduced even when searchable hidden text is recovered.
    pub const RASTER: Self = Self {
        visual_fidelity: false,
        semantic_structure: true,
        editability: true,
        dynamic_behavior: true,
        accessibility: true,
        portability: false,
    };

    /// Content with no emitted representation.
    pub const DROP: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: true,
        dynamic_behavior: true,
        accessibility: true,
        portability: true,
    };

    /// Text survives, but a link's interactive destination does not.
    pub const LINK_TARGET: Self = Self {
        visual_fidelity: false,
        semantic_structure: true,
        editability: false,
        dynamic_behavior: true,
        accessibility: true,
        portability: false,
    };

    /// The emitted value is exact and editable, but intentionally does not
    /// participate in consumer-side recalculation because the target cannot
    /// reproduce the source semantics.
    pub const DYNAMIC_BEHAVIOR: Self = Self {
        visual_fidelity: false,
        semantic_structure: false,
        editability: false,
        dynamic_behavior: true,
        accessibility: false,
        portability: false,
    };

    /// Native/editable content with an unavoidable geometric difference.
    pub const VISUAL_ONLY: Self = Self {
        visual_fidelity: true,
        semantic_structure: false,
        editability: false,
        dynamic_behavior: false,
        accessibility: false,
        portability: false,
    };

    /// Page furniture stays native and editable, but later pages can display a
    /// sampled value and the source's per-page dynamic behavior is frozen.
    pub const PAGE_FURNITURE_SAMPLED: Self = Self {
        visual_fidelity: true,
        semantic_structure: false,
        editability: false,
        dynamic_behavior: true,
        accessibility: false,
        portability: false,
    };

    /// Section geometry survives, but its failed furniture/content does not.
    pub const SECTION_GEOMETRY_ONLY: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: false,
        dynamic_behavior: true,
        accessibility: true,
        portability: false,
    };

    /// Plain alternate text preserves readability and editability but not the
    /// equation's mathematical structure or appearance.
    pub const MATH_TEXT: Self = Self {
        visual_fidelity: true,
        semantic_structure: true,
        editability: false,
        dynamic_behavior: false,
        accessibility: false,
        portability: false,
    };
}

/// Stable-enough identity shared by repeated decisions for one source region.
///
/// Typst locations are preferred when available. The logical id also includes
/// the source span and element name, so detached or location-less content still
/// has a deterministic identity within a compile.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ExportSource {
    pub logical_id: u128,
    pub element: EcoString,
    pub span: Span,
    pub location: Option<Location>,
}

impl ExportSource {
    pub(crate) fn from_content(content: &Content) -> Self {
        Self::new(content.elem().name(), content.span(), content.location())
    }

    pub(crate) fn new(
        element: impl Into<EcoString>,
        span: Span,
        location: Option<Location>,
    ) -> Self {
        let element = element.into();
        // Source spans survive independent paged/DOCX realizations; Typst
        // locations do not. Keep locations as useful evidence, but exclude
        // them from the stable identity unless the source is detached.
        let logical_id = if span.is_detached() {
            typst_utils::hash128(&(span, location, &element))
        } else {
            typst_utils::hash128(&(span, &element))
        };
        Self { logical_id, element, span, location }
    }
}

/// One selected representation for a logical source region.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ExportDecision {
    pub source: ExportSource,
    pub representation: Representation,
    pub reason: DecisionReason,
    pub losses: LossSet,
    /// Repeated realization of the same logical region (for example, repeated
    /// page furniture) is aggregated instead of producing duplicate rows.
    pub occurrences: usize,
    /// Searchable text characters recovered alongside a raster fallback.
    pub affected_text_chars: usize,
    /// Logical semantic regions affected by this decision. This is one for a
    /// first occurrence and grows with aggregated repeated realizations.
    pub affected_semantic_nodes: usize,
}

/// Stage at which a diagnostic was suppressed to keep best-effort export going.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ExportStage {
    DocumentConversion,
    LayoutCallback,
    FallbackLayout,
    SectionLowering,
    FieldPlanning,
    CapabilityPlanning,
}

/// Kind of suppressed diagnostic.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum SuppressedKind {
    Error,
    DelayedError,
    Warning,
    Panic,
}

/// A diagnostic retained in the report even though it did not abort export.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SuppressedDiagnostic {
    pub source: ExportSource,
    pub stage: ExportStage,
    pub kind: SuppressedKind,
    pub diagnostic: SourceDiagnostic,
    pub occurrences: usize,
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

/// Who is allowed to recalculate a dynamic field after export.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FieldOwner {
    Typst,
    Consumer,
}

/// Whether a field's cached result participates in visible document content.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FieldVisibility {
    Visible,
    Hidden,
}

/// Availability of a trustworthy cached result in the finalized field group.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FieldCacheStatus {
    Resolved,
    BestEffort,
    ConsumerRequired,
    Unavailable,
}

/// One stable field group in the finalized DOCX IR.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DynamicFieldFact {
    pub logical_id: u128,
    pub kind: EcoString,
    pub instruction: EcoString,
    pub owner: FieldOwner,
    pub visibility: FieldVisibility,
    pub cache_status: FieldCacheStatus,
    pub occurrences: usize,
}

/// One font family referenced by the finalized DOCX IR.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontFact {
    pub logical_id: u128,
    pub family: EcoString,
    /// Whether the family was resolvable in Typst's font book while exporting.
    /// A missing family can still be a valid portable Word reference, but its
    /// first-open metrics and glyph coverage are consumer-dependent.
    pub available_at_export: bool,
    /// DOCX currently references fonts but does not embed font programs.
    pub embedded: bool,
    pub occurrences: usize,
}

/// Accessibility semantics for one finalized drawing.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DrawingAccessibilityFact {
    pub logical_id: u128,
    pub docpr_id: u32,
    pub name: EcoString,
    pub alternative_text: Option<EcoString>,
    /// Explicit Office decorative intent: assistive technology should skip it.
    pub decorative: bool,
    /// The drawing carries real editable text-box content.
    pub native_text: bool,
}

impl DrawingAccessibilityFact {
    /// True when a non-decorative drawing exposes neither a description nor
    /// native text content.
    pub fn unlabeled(&self) -> bool {
        !self.decorative && self.alternative_text.is_none() && !self.native_text
    }
}

/// Structured, queryable evidence about DOCX fidelity decisions.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FidelityReport {
    decisions: Vec<ExportDecision>,
    suppressed: Vec<SuppressedDiagnostic>,
    dynamic_fields: Vec<DynamicFieldFact>,
    fonts: Vec<FontFact>,
    drawings: Vec<DrawingAccessibilityFact>,
}

impl FidelityReport {
    /// Recorded representation decisions in first-seen document order.
    pub fn decisions(&self) -> &[ExportDecision] {
        &self.decisions
    }

    /// Diagnostics deliberately suppressed by best-effort conversion.
    pub fn suppressed_diagnostics(&self) -> &[SuppressedDiagnostic] {
        &self.suppressed
    }

    /// Dynamic field groups in the finalized typed IR.
    pub fn dynamic_fields(&self) -> &[DynamicFieldFact] {
        &self.dynamic_fields
    }

    /// Font families referenced by defaults, styles, and concrete runs.
    pub fn fonts(&self) -> &[FontFact] {
        &self.fonts
    }

    /// Finalized drawing accessibility facts in document traversal order.
    pub fn drawings(&self) -> &[DrawingAccessibilityFact] {
        &self.drawings
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

    pub(crate) fn record_content(
        &mut self,
        content: &Content,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
        affected_text_chars: usize,
    ) {
        self.record(
            ExportSource::from_content(content),
            representation,
            reason,
            losses,
            affected_text_chars,
        );
    }

    pub(crate) fn record_span(
        &mut self,
        source: ExportSource,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
        affected_text_chars: usize,
    ) {
        self.record(source, representation, reason, losses, affected_text_chars);
    }

    fn record(
        &mut self,
        source: ExportSource,
        representation: Representation,
        reason: DecisionReason,
        losses: LossSet,
        affected_text_chars: usize,
    ) {
        if let Some(existing) = self.decisions.iter_mut().find(|decision| {
            decision.source.logical_id == source.logical_id
                && decision.representation == representation
                && decision.reason == reason
        }) {
            existing.occurrences += 1;
            existing.affected_text_chars += affected_text_chars;
            existing.affected_semantic_nodes += 1;
            return;
        }
        self.decisions.push(ExportDecision {
            source,
            representation,
            reason,
            losses,
            occurrences: 1,
            affected_text_chars,
            affected_semantic_nodes: 1,
        });
    }

    pub(crate) fn suppress_content(
        &mut self,
        content: &Content,
        stage: ExportStage,
        kind: SuppressedKind,
        diagnostic: SourceDiagnostic,
    ) {
        self.suppress(ExportSource::from_content(content), stage, kind, diagnostic);
    }

    pub(crate) fn suppress_span(
        &mut self,
        element: impl Into<EcoString>,
        span: Span,
        location: Option<Location>,
        stage: ExportStage,
        kind: SuppressedKind,
        diagnostic: SourceDiagnostic,
    ) {
        self.suppress(
            ExportSource::new(element, span, location),
            stage,
            kind,
            diagnostic,
        );
    }

    pub(crate) fn record_dynamic_field(
        &mut self,
        snapshot_id: u128,
        instruction: &str,
        owner: FieldOwner,
        visibility: FieldVisibility,
        cache_status: FieldCacheStatus,
    ) {
        let instruction = instruction.trim();
        let kind: EcoString = instruction
            .split_whitespace()
            .next()
            .unwrap_or("UNKNOWN")
            .to_ascii_uppercase()
            .into();
        let logical_id = typst_utils::hash128(&(
            snapshot_id,
            instruction,
            owner,
            visibility,
            cache_status,
        ));
        if let Some(existing) = self
            .dynamic_fields
            .iter_mut()
            .find(|field| field.logical_id == logical_id)
        {
            existing.occurrences += 1;
            return;
        }
        self.dynamic_fields.push(DynamicFieldFact {
            logical_id,
            kind,
            instruction: instruction.into(),
            owner,
            visibility,
            cache_status,
            occurrences: 1,
        });
    }

    pub(crate) fn record_font(
        &mut self,
        snapshot_id: u128,
        family: &str,
        available_at_export: bool,
    ) {
        if family.is_empty() {
            return;
        }
        let logical_id = typst_utils::hash128(&(snapshot_id, family));
        if let Some(existing) =
            self.fonts.iter_mut().find(|font| font.logical_id == logical_id)
        {
            existing.occurrences += 1;
            return;
        }
        self.fonts.push(FontFact {
            logical_id,
            family: family.into(),
            available_at_export,
            embedded: false,
            occurrences: 1,
        });
    }

    pub(crate) fn record_drawing(
        &mut self,
        snapshot_id: u128,
        docpr_id: u32,
        name: &str,
        alternative_text: Option<&str>,
        decorative: bool,
        native_text: bool,
    ) {
        self.drawings.push(DrawingAccessibilityFact {
            logical_id: typst_utils::hash128(&(snapshot_id, docpr_id, name)),
            docpr_id,
            name: name.into(),
            alternative_text: alternative_text.map(Into::into),
            decorative,
            native_text,
        });
    }

    fn suppress(
        &mut self,
        source: ExportSource,
        stage: ExportStage,
        kind: SuppressedKind,
        diagnostic: SourceDiagnostic,
    ) {
        if let Some(existing) = self.suppressed.iter_mut().find(|entry| {
            entry.source.logical_id == source.logical_id
                && entry.stage == stage
                && entry.kind == kind
                && entry.diagnostic == diagnostic
        }) {
            existing.occurrences += 1;
            return;
        }
        self.suppressed.push(SuppressedDiagnostic {
            source,
            stage,
            kind,
            diagnostic,
            occurrences: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_decisions_are_aggregated() {
        let mut report = FidelityReport::default();
        let content: Content =
            typst_library::text::TextElem::new("same source".into()).into();
        report.record_content(
            &content,
            Representation::Raster,
            DecisionReason::RasterFallback,
            LossSet::RASTER,
            4,
        );
        report.record_content(
            &content,
            Representation::Raster,
            DecisionReason::RasterFallback,
            LossSet::RASTER,
            7,
        );

        assert_eq!(report.decisions.len(), 1);
        assert_eq!(report.decisions[0].occurrences, 2);
        assert_eq!(report.decisions[0].affected_text_chars, 11);
        assert_eq!(report.decisions[0].affected_semantic_nodes, 2);
        assert_eq!(report.counts().raster, 2);
    }
}

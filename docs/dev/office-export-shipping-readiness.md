# Office export shipping readiness

Status date: 2026-07-13

This is the current product and validation snapshot after consolidating the DOCX,
PPTX, Pandoc, shared Office, DOCX review, corpus-hardening, and OOXML-math branch
histories onto `codex/office-export`. Performance branches remain separate.

## Executive verdict

- **DOCX:** usable as an experimental preview for broad, mostly semantic documents.
  It is not justified as a universal "any Typst document" exporter yet.
- **PPTX:** produces valid, editable presentations and has broad native coverage,
  but remains an experimental preview. It is not ready for a general fidelity
  guarantee: editable text metrics, complex math, tables, and some transformed or
  mixed-size layouts still have verified gaps.
- **Pandoc:** usable as a semantic interchange target, not as a paged-layout
  preservation target.
- **OOXML math import:** the inverse OMML-to-Typst converter is consolidated, but it
  is an import-side utility rather than part of DOCX/PPTX export.

The safe product wording is: **Typst can export experimental `.docx` and `.pptx`
files with substantial native editability and image fallbacks. Compatibility and
fidelity are document- and consumer-dependent.** Do not claim arbitrary-document
fidelity or production support yet.

## What works now

### DOCX

Native or editable coverage includes text and character styling, headings and Word
styles, nested lists, tables and grids, OMML math, links and bookmarks, references,
footnotes, citations and bibliographies, TOCs, figures and captions, page geometry,
sections, columns, headers and footers, page numbering, many vector shapes, anchored
text and simple tables, SVG with compatibility fallback, and standard raster image
formats. Unsupported visual regions can fall back atomically to PNG instead of
silently dropping a child.

The exporter also has:

- deterministic OPC/ZIP output and relationship/package validation;
- repair-sensitive WordprocessingML sequence validation;
- structured fidelity decisions for native, approximate, raster, and dropped
  content;
- converged paged-layout snapshot data for semantic IDs and page-dependent fields;
- opt-in embedded fidelity metadata (off by default in the CLI);
- an experimental source-safe Word review workflow for enrolled text regions,
  comments, formatting/structural conflict detection, and transactional apply.

### PPTX

One Typst page maps to one slide. Native coverage includes editable text runs,
external and slide links, common vector shapes, solid/linear-gradient slide
backgrounds (including adaptive native stops that preserve Typst's perceptual
interpolation in sRGB Office consumers), PNG/JPEG/GIF, native SVG plus PNG fallback,
conservative editable tables, eligible OMML math with a DrawingML fallback, notes,
slide numbers, and inferred title/body placeholders. Unsupported visual regions can
become positioned pictures while the rest of the slide remains editable.

### Pandoc

The target emits typed Pandoc JSON for headings, paragraphs, inline formatting,
lists, tables, links, footnotes, code, figures, math, citations, and metadata. It can
write a bibliography sidecar and rasterize visual content that has no Pandoc node.

## Verified gaps and shipping blockers

### DOCX

- A fresh, unfiltered public-corpus authority has not been completed on the combined
  revision. The current handoff calls for all 1,408 records with LibreOffice and
  round-trip lanes; the existing v14 aggregate contains only a filtered subset.
- The current corpus handoff still identifies real low-fidelity documents, including
  `presentation/sleiden-lei` (page growth, displaced logo content, missing text, and
  unsupported-content drops).
- Contextual headers/footers that vary beyond Word's first/even/default model freeze
  to a reported page-one approximation.
- Some mapper-specific empty/unsupported paths still need complete loss accounting.
- Complex diagrams, non-OOXML transforms, radial/conic fills, PDF/WebP images, and
  opaque layout callbacks may be rasterized and therefore not editable.
- Fractional page-space distribution and page-coordinate links do not have flowing
  Word equivalents. Accessibility metadata exists but is not an accessibility
  certification.
- Full Office-version XSD validation and current Microsoft Word corpus validation
  remain broader release gates.

### PPTX

- Unsupported or transformed native-table regions can still be accepted by capture
  without a table or whole-region fallback.
- Mixed page sizes are uniformly scaled to fit and centered on PowerPoint's one
  global slide canvas. This preserves content but can introduce letterboxing;
  gradients and other page-relative backgrounds need broader mixed-size coverage.
- Table capture carries native cell fills, strokes, alignment, and per-side text
  insets, but does not yet carry the complete gutter or cell-math contract;
  consumer line-box metrics can still expand automatic row heights.
- Live text regrouping still lacks a complete language and shaping policy. Licensed
  fonts are embedded when their OpenType permissions allow it, but consumer text-box
  metrics can still reflow text.
- Rotated live text retains editable DrawingML rotation and uses rotation-neutral
  bounds, validated at 90, -90, and 45 degrees in LibreOffice; broader
  angle/font/consumer coverage remains a release gate. External and same-deck
  hyperlinks on text, vector shapes, and pictures retain full-object hit areas.
- Math fallback quality varies by consumer, especially in LibreOffice and older
  Office versions.
- Page filtering does not remap every slide-jump link.
- The 2026-07-13 LibreOffice smoke export opened without repair and preserved editable
  content, but visibly wrapped table/list text differently, overlapped a list with a
  following shape, and rendered the inline equation less faithfully. This confirms
  package validity but rejects a general visual-fidelity claim.

### Pandoc

- Paged geometry, exact line breaking, floats, and columns cannot survive the semantic
  AST by design.
- Citation normalization loses some mode, supplement, and grouping distinctions.
- Anchor installation, deep table-cell dangling-link discovery, responsive fallback
  width, and transactional JSON/sidecar writes remain incomplete.

## Validation completed on the combined branch

- DOCX integration: 195 tests passed.
- PPTX integration: 41 tests passed.
- Pandoc integration: 24 tests passed.
- DOCX review round trip: 22 tests passed.
- OOXML math conversion: 27 tests passed.
- Office exporter unit/doc suites: 71 tests passed.
- Strict Clippy across CLI and all Office/Pandoc crates: passed with warnings denied.
- DOCX corpus Python tools: bytecode compilation passed.
- Release CLI build: passed.
- Real release-mode `.docx`, `.pptx`, and `.pandoc` exports: passed.
- DOCX and PPTX ZIP integrity: passed.
- LibreOffice Writer/Impress open and PDF conversion: passed.
- Visual inspection: DOCX smoke output was structurally clean; PPTX exposed the
  fidelity limitations described above.

## Remaining release gates

1. Run the full unfiltered 1,408-document DOCX corpus with package, LibreOffice,
   visual, editability, and round-trip lanes; publish denominators and failure classes.
2. Triage and fix or explicitly disposition the remaining DOCX corpus tail, starting
   from the current handoff's ranked defects.
3. Add an equivalent current PPTX corpus run on the consolidated revision, with both
   visual similarity and native-editability measurements, plus PowerPoint testing.
4. Fix the critical PPTX table-fallback and mixed-page-size issues before widening
   availability beyond preview.
5. Decide the CLI/product contract for fidelity reporting and preview flags, then
   align user-facing documentation and release notes.
6. Run accessibility and target-version checks in Microsoft Word and PowerPoint for
   the supported consumer matrix.

Until those gates pass, ship only behind explicit experimental/preview wording.

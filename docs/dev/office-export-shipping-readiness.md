# Office export shipping readiness

Status date: 2026-07-14

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
- **Pandoc:** retained as known-incomplete historical/development code. It is not
  a supported preview or release gate and should be treated as broken for general
  use.
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
- embedded fidelity metadata by default in the CLI (library callers remain opt-in);
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

The math compatibility fallback uses the captured authored size and compact
Unicode scripts/limits so non-OMML consumers remain readable and editable. It
does not reproduce stacked fractions, radicals, or native limit placement.

### Pandoc

The target emits typed Pandoc JSON for headings, paragraphs, inline formatting,
lists, tables, links, footnotes, code, figures, math, citations, and metadata. It can
write a bibliography sidecar and rasterize visual content that has no Pandoc node.

## Verified gaps and shipping blockers

### DOCX

- A fresh, unfiltered public-corpus authority has not been completed after the
  current TOC, review-export, and tall-raster fixes. The current handoff calls for
  all 1,408 records with LibreOffice and round-trip lanes.
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

- Transformed tables now use a whole-region picture fallback instead of disappearing.
  Other unsupported or partially captured table edge cases still need corpus coverage.
- Mixed page sizes are uniformly scaled to fit and centered on PowerPoint's one
  global slide canvas. This preserves content but can introduce letterboxing;
  gradients and other page-relative backgrounds need broader mixed-size coverage.
- Table capture carries native cell fills, stroke width/dash/cap, alignment, and
  per-side text insets; row and column gutters become editable borderless spacer
  tracks that participate in spans. It does not yet carry the complete cell-math contract,
  and consumer line-box metrics can still expand automatic row heights.
- Live text regrouping still lacks a complete language and shaping policy. Licensed
  fonts are embedded when their OpenType permissions allow it, but consumer text-box
  metrics can still reflow text.
- Rotated live text retains editable DrawingML rotation and uses rotation-neutral
  bounds, validated at 90, -90, and 45 degrees in LibreOffice; broader
  angle/font/consumer coverage remains a release gate. External and same-deck
  hyperlinks on text, vector shapes, and pictures retain full-object hit areas.
- LibreOffice and older Office versions still render math through the compact
  Unicode fallback rather than native stacked OMML; authored sizing and readable
  scripts/limits are preserved, but stacked fractions and radicals are not.
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

- DOCX integration: 212 tests passed.
- PPTX integration: 65 tests passed.
- DOCX review round trip: 22 tests passed.
- OOXML math conversion: 27 tests passed.
- Strict Clippy across the CLI and supported DOCX/PPTX crates: passed with
  warnings denied.
- DOCX corpus Python tools: bytecode compilation passed.
- Release CLI build: passed.
- Real release-mode `.docx` and `.pptx` exports: passed.
- DOCX and PPTX ZIP integrity: passed.
- LibreOffice Writer/Impress open and PDF conversion: passed.
- Fresh CLI exports embed fidelity metadata: validator 2/2 passed with zero
  unverified records.
- Ninety of the 91 historical review-export failures now pass. The remaining
  `paper/tracl` failure occurs during source compilation before export.
- Tall block-level raster fallbacks are split into page-bounded pictures; the
  previously hanging `elegant-culsc` LibreOffice conversion now completes. Inline,
  table, positioned, and math fallbacks remain atomic.
- Filtered PPTX exports remap explicit physical-page links to their retained slide
  numbers and drop links whose target page was omitted.
- Headless visual QA reports exposed the fidelity limitations described above;
  primary-agent review did not inspect rendered images.

## Baseline corpus authority (revision `c07adc99b70f`)

The 2026-07-13 campaign froze the same 1,408-document public corpus at manifest
SHA-256 `9b92ee3092b97c9c600547722ccb2397a5d21a1eea1d224418d1c57d1a9e99af`
and used release-binary SHA-256
`1f061d3130681a63b3ccaad2a15fb3545b0988058a33ae65cdb8341dba877133`.
This authority predates the current DOCX TOC/review fixes and PPTX transformed-table
fallback. Its metrics remain reproducible baseline evidence, not a score for HEAD.

### DOCX

- 1,407/1,408 packages were valid; `paper/tracl` retained its source-owned
  DOCX-target compile failure.
- LibreOffice produced 1,397 scored renders. Seven conversions timed out, one
  conversion failed, and two consumer PDFs could not be rasterized.
- Visual-policy passes: 762; exact page counts: 456; page deltas above one: 635.
- Across the 1,397 scored renders, mean similarity was `0.954462`, median
  `0.965868`, p10 `0.909426`, and minimum `0.308371` (`presentation/sleiden-lei`).
- The last clean v12 authority was materially better: 847 policy passes, 554
  exact page counts, and 554 page deltas above one. A limited three-revision
  follow-up found that `report/kdl` now matches Typst's 13-page reference, but a
  fresh authority is required before classifying the aggregate delta on HEAD.
- At this baseline, review export completed for 1,317 documents and failed for
  91. Eighty-nine failures violated the terminal-paragraph invariant in a
  document table cell, one did so in a header table cell, and `paper/tracl`
  retained its source-owned target failure.
- At this baseline, 1,399 records were `unverified` because its CLI exports omitted
  fidelity metadata required by the corpus classifier. New CLI exports embed that
  metadata by default; the full authority still needs to be rerun under that contract.

Durable results are under `target/docx-public-corpus-run-c07adc9/`.

### PPTX

- All 120 currently compilable presentation templates exported valid OOXML
  packages: zero export errors, invalid packages, or timeouts.
- LibreOffice scored all 120 decks with zero stage failures and zero slide-count
  mismatches: mean `0.992`, median `0.993`, p10 `0.983`, minimum `0.946`.
- Native-text recovery was measurable for 109 decks: mean `0.975`, median
  `1.000`, p10 `0.892`, minimum `0.431`. Eleven decks had no extractable PDF-word
  denominator; none failed export.
- The visual result is slightly below the dated 112-template mean of `0.995`, but
  it covers a larger set on current HEAD and is now the visual authority.

Durable tabular results are `target/pptx-structure-c07adc9.tsv`,
`target/pptx-visual-c07adc9.tsv`, and `target/pptx-nativeness-c07adc9.tsv`.

## Distribution direction

The canonical public branch is `codex/office-export`, now the fork's default
branch. A browser-hosted WASM export surface is the preferred distribution goal;
a native installer is optional. Release archives remain a useful fallback, but
installer polish is not a prerequisite for the next hosted-preview campaign.

The separate `typst-office` demo now imports folders or bounded ZIP projects,
loads project-local fonts, resolves relative modules, and maps vendored packages
from `packages/<namespace>/<name>/<version>/...` into Typst's package namespace.
Its local WASM smoke exports both DOCX and PPTX with relative and `@local` imports.
This proves the browser conversion core, not yet the deployed GitHub Pages path or
arbitrary Typst Universe/proprietary-app project compatibility.

## Remaining release gates

1. Run a fresh 1,408-document DOCX authority on HEAD so the fixed fidelity-manifest
   and review-export contracts, pagination tail, and tiled fallback are scored together.
2. Explain or disposition the remaining DOCX delta from v12 rather than carrying
   the historical aggregate forward as a current regression claim.
3. Retry the historical DOCX consumer/raster failures serially to separate fixed,
   deterministic, and load-sensitive LibreOffice behavior.
4. Add real Microsoft PowerPoint testing to the new 120-deck PPTX authority.
5. Expand PPTX table-fallback and mixed-page-size coverage before widening
   availability beyond preview.
6. Document the CLI/library fidelity-reporting contract and preview status in the
   user-facing release notes.
7. Prove the DOCX/PPTX export path in the intended browser/WASM hosting architecture,
   including fonts, packages, filesystem inputs, memory bounds, and file download.
8. Run accessibility and target-version checks in Microsoft Word and PowerPoint for
   the supported consumer matrix.

Until those gates pass, ship only behind explicit experimental/preview wording.

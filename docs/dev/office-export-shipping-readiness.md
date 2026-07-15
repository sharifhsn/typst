# Office export shipping readiness

Status date: 2026-07-15

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

- The fresh 1,408-document authority completed at `1bf829900`, followed by
  serial consumer retries and an OMML-aware semantic refresh. It produced 1,405
  valid packages, 1,392 successful LibreOffice renders, nine remaining
  consumer failures, and one source-owned DOCX-target error (`paper/tracl`).
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
- Rasterized clipped or transformed groups now retain transparent editable text
  plus accessibility metadata. This preserves search/copy/edit richness without
  competing with the raster picture for visual authority; broader PowerPoint
  save-and-reopen testing remains necessary.
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

- DOCX integration: 220 tests passed.
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
- DOCX paragraph spacing is emitted once per collapsed Typst boundary across body,
  list, table, furniture, footnote, and text-box stories, avoiding consumer-specific
  `before` + `after` summation.
- PPTX raster fallbacks preserve searchable/editable transparent text. On the
  previous worst nativeness deck, `steady-rvl-slides`, recovery increased from
  28/65 to 65/65 words; `clari-docs` and `sdu-touying-simpl` recover about 99.7%
  and 101.0% of reference words respectively while retaining valid packages.
- Headless visual QA reports exposed the fidelity limitations described above;
  primary-agent review did not inspect rendered images.

## Current DOCX corpus authority (revision `1bf8299001ff`)

The 2026-07-15 campaign compiled the frozen 1,408-document corpus with release
binary SHA-256
`66711ff6daa4506f8ceac2fd59e098c8da1b3b74ecb57da449a9e6be80c412a2`.
The subsequent retries reused that exact binary and retained the original run
identity; later checker and exporter fixes on this branch are validated by
focused tests rather than being mislabeled as part of this authority.

- 1,405/1,408 packages were valid. Two very large documents exceeded the
  240-second compile timeout; `paper/tracl` explicitly lacks a DOCX target. Those
  three absent packages are also the checker's `DOCX-E101` records.
- Both timeout cases succeeded when retried serially with a 600-second budget
  (DOCX compilation took about 82 and 134 seconds), classifying them as
  load-sensitive authority-run failures rather than unsupported documents.
- LibreOffice rendered 1,392 documents. Serial retry recovered four cases; eight
  remain timeouts, one remains a deterministic conversion failure, and four
  records failed downstream raster evidence while three were never submitted to
  the consumer because they did not produce a DOCX package.
- Longer isolated retries have opened three of those eight timeout records,
  including a 261-page, 23.9 MB package. The other five still hang for at least
  120 seconds in LibreOffice, including the math-heavy `xenolay` record. They are
  retained as failures in the authority score. ZIP/XML validation and prefix
  bisection point to consumer scalability rather than malformed OOXML, but this
  is a diagnosis rather than proof for every record. The deterministic failure
  is also a valid package; it
  reproducibly triggers LibreOffice's `Unspecified Application Error`. Its
  retained package contains 103 tables, 75 modern text boxes, and 128 drawings.
  Prefix bisection places
  its first trigger at an inline dashed DrawingML line, but merely padding that
  line's one-EMU degenerate dimension does not resolve the full document, and
  deleting that shape run does not either. The LibreOffice failure is cumulative,
  non-local, or has a later independent trigger. A focused rerun with the current
  exporter reconfirmed `DOCX-E202`: the package is valid and review round trip
  passes, but LibreOffice still produces no PDF. Durable evidence is under
  `target/docx-public-corpus-focus-e2021-1b38352/`.
- Focused HEAD validation after that frozen authority adds two explicitly reported
  raster fallbacks for pathological visual canvases. The one-page `raphaelasla`
  shape swarm uses one full-page fallback, retains one page, and scores `0.979955`.
  `gb-ctr` uses 92 dense-canvas fallbacks and scores `0.968293`, but expands from
  164 to 202 pages. Tura uses seven dense-canvas fallbacks and scores `0.975914`,
  but expands from 265 to 351 pages. Those three records now complete LibreOffice
  conversion instead of hanging, though the two long documents retain substantial
  reflow errors.
- A serial 300-second retry also rendered the math-heavy `xenolay` and Alex mathnote
  packages without new raster policies. They score `0.949368` (163 versus 161 pages)
  and `0.977457` (187 versus 145 pages), respectively. Across the five-record
  focused run, packages, LibreOffice rendering, and review round trip passed 5/5;
  visual policy passed only `raphaelasla`. These focused results are not folded
  into the authority totals above. Durable evidence is under
  `target/docx-public-corpus-focus-dense-fixes-1b38352/`.
- Visual-policy passes: 761/1,392 rendered; exact page counts: 457; page deltas
  above one: 631.
- The checker identifies 164 slide-shaped DOCX exports as a separate informational
  lane. Non-slide results account for 1,228 rendered documents, 713 policy passes,
  437 exact page counts, and 515 page deltas above one.
- Review round trip passed for 1,405 documents, failed only with `paper/tracl`, and
  was unavailable for two. It enrolled 520,021 regions across 1,138 documents.
- OMML text is now included in semantic extraction. This fixed false zero-text
  reports for math-heavy documents; semantic coverage remains an advisory, not a
  proof of loss, especially for CJK, raster-heavy, and slide-shaped sources.
- The strict classifier still labels 1,398 records `unverified`, chiefly because
  current Microsoft Word evidence is absent for 1,405, fonts are unavailable for
  749, and licenses are unverified for 518. Completion of the authority means all
  configured lanes ran and retained evidence; it does not turn missing consumer,
  font, or license evidence into a pass.

Durable results are under `target/docx-public-corpus-run-1bf8299-clean/`.

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
  follow-up found that `report/kdl` now matches Typst's 13-page reference. The
  current authority is lower at 761/457/631, but the v12 directory, binary, and
  per-record JSON are no longer retained locally or in Git. Its aggregate is
  therefore historical evidence, not a reproducible comparison authority, and
  exact record-level or causal attribution is no longer possible.
- At this baseline, review export completed for 1,317 documents and failed for
  91. Eighty-nine failures violated the terminal-paragraph invariant in a
  document table cell, one did so in a header table cell, and `paper/tracl`
  retained its source-owned target failure.
- At this baseline, 1,399 records were `unverified` because its CLI exports omitted
  fidelity metadata required by the corpus classifier. The current authority has
  now rerun the full corpus under the embedded-metadata contract.

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
The deployed GitHub Pages path currently serves the browser JS and WASM with the
correct `application/wasm` content type, and the exact CI smoke path passes locally.
Three newer project-import/package/filename commits still need to reach the remote;
arbitrary Typst Universe/proprietary-app project compatibility is not yet proven.

## Remaining release gates

1. Re-run the full consumer lane to establish the focused 5/5 timeout recovery as
   corpus-wide authority, then address the deterministic LibreOffice conversion
   failure and incomplete raster evidence.
2. Run a new authority after the post-`1bf829900` paragraph-spacing and checker
   changes and establish it as the new reproducible baseline; do not use the
   unretained v12 aggregate for causal claims.
3. Add real Microsoft PowerPoint testing to the new 120-deck PPTX authority,
   including save-and-reopen behavior for transparent recovered text.
4. Prove the DOCX/PPTX export path in the intended browser/WASM hosting architecture,
   including fonts, packages, filesystem inputs, memory bounds, and file download.
5. Run accessibility and target-version checks in Microsoft Word and PowerPoint for
   the supported consumer matrix.

Until those gates pass, ship only behind explicit experimental/preview wording.

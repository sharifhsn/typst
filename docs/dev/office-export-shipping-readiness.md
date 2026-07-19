# Office export shipping readiness

Status date: 2026-07-18

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

- The 2026-07-18 pagination campaign (commits `b05bd79` + `ff55d6b`) fixed the
  two dominant page-growth mechanisms — weak page breaks lowered as hard breaks
  (now Word's idempotent `pageBreakBefore`), and relative shape heights resolved
  against the page instead of the measured cell row box. Against the same-day
  pre-fix authority: visual-policy passes 910 → 960, exact page counts 626 →
  656, and total absolute page deviation down 24% (5,130 → 3,908 pages). The
  worst offender (`book/alen-ops99-sleep-guide`) dropped 188 → 125 rendered
  pages against a 106-page reference.
- Known metric caveat from that campaign: LibreOffice's DOCX→PDF conversion
  performs `oddPage` section transitions but does not materialize parity blank
  pages, so chapter-on-odd-page theses (kthesis family, ~10 documents) now
  render *shorter* than their paged references by one page per chapter. The
  old always-hard weak breaks accidentally compensated for this. The export now
  encodes the author's parity intent idiomatically; Microsoft Word honors it.
- Remaining distinct page-inflation families (measured, unfixed): Arabic/RTL
  pure-paragraph line metrics (investigated and diagnosed as consumer-side
  complex-script text-shaping drift outside the exporter's control — see
  memory/commit history, not pursued further), math-raster fallback growth,
  and a diffuse ~15–20% table-row/leading drift.
- Follow-up fix (commit `8c3b53e`): a positioned corner-badge idiom
  (`place(..)[box(width: 1cm)[image(width: 100%), ..]]`, the common
  footer-shield/page-number pattern) silently dropped the box's own
  width/height constraint when its body was flattened to inline runs,
  resolving the nested image's `100%` against the ambient page width instead
  — a 1cm badge became a near-full-page image repeated every page. Fixed by
  scoping the box's own resolved size for that extraction, matching table
  cells. Worst-case fix: `report/lion-ecl` 52 → 10 rendered pages (7-page
  reference). Full-corpus effect: degraded documents 487 → 435, native_good
  454 → 472, visual-policy passes 960 → 963, zero regressions.
- Follow-up fix (commit `e2fc8d8`, first of a 10-document goal): a bare `#box`
  that Typst's realize splits a paragraph around (the ubiquitous
  `show raw.where(block: false)` shaded-inline-code-pill idiom, and similar
  patterns) was never recognized as paragraph-continuing content, so it fell
  to the standalone-text-box block path — an *inline* Word text box does not
  flow, so every code span broke its sentence into a separate paragraph and
  wrapped character-by-character inside a tiny fixed box. Fixed by keeping a
  bare top-level box in its paragraph (guarded so a `#layout(..)`-produced box
  wrapping a real figure/grid/table still takes its dedicated recovery path).
  Worst-case fix: `book/albertarakelyan-rust-handbook` 40 → 22 rendered pages
  (23-page reference), native_degraded → native_good. Full-corpus effect:
  native_good 472 → 473, fallback_visual 303 → 314, degraded documents
  435 → 426; no category regressed in aggregate (no per-document prior-vs-new
  diff retained — the previous authority was auto-pruned before a snapshot
  was saved).
- Follow-up fix (2/10 of the 10-document goal, `dmitro44-osisp_coursework_report`):
  investigated and confirmed as the already-documented diffuse ~15–20%
  leading/table-row drift family (not single-mechanism-fixable) via an
  independent pure-Cyrillic/Times-New-Roman-paragraph repro — no new
  exporter defect, no code change.
- Follow-up fix (3/10, `cv/linked-cv`): two independent bugs compounded into a
  700% page explosion (1 → 8 rendered pages) on a single-page CV template.
  (a) SVG "tech-icon" bodies use the bracketed-markup idiom
  `box(height: size)[#image(bytes(svg), height: 100%)]`; the box→image
  icon-sizing special case only matched a *bare* `ImageElem` argument with
  `height: auto`, so the bracketed body (a realize-inserted `SequenceElem`)
  with its own explicit relative height fell through to the plain-box
  fallback and rasterized each icon at an unbounded, wildly elongated size.
  Fixed by unwrapping `SequenceElem`/`StyledElem` wrappers down to the bare
  image (`unwrap_sole_image`) and widening the height match from
  `Sizing::Auto`-only to any explicit sizing. (b) The template wraps its
  entire body in `#show: doc => { set page(footer: ..); context { doc } }` —
  the standard "establish page geometry once, from a show-everywhere rule"
  idiom. Typst inserts a *boundary* pagebreak exactly where the new page
  style takes effect; the realize scaffolding before it (a handful of
  `TagElem`s, no visible content) still differed in resolved page geometry
  (no footer vs. footer) from the real body that followed, so the section
  resolver gave that empty run its own Word section — and a section
  transition costs a full blank page even with zero paragraphs in it. Fixed
  by `merge_content_empty_sections`, a post-pass that absorbs any
  content-empty section (no forced break, no requested blank pages) into a
  neighbouring section instead of giving it its own transition. Combined:
  8 → 1 rendered pages, exact parity with the gold reference. Full-corpus
  effect: pending gate (both fixes are general — (a) affects any bracketed
  image-in-box icon idiom, (b) affects any document using the common
  "show-everywhere page-setup wrapper" template pattern).
- Follow-up fix (4/10, `cv/imtsuki-resume`): a table-geometry *identity* bug,
  not a sizing bug. The resume factors its "section with a side heading"
  layout into a reusable function wrapping a single-row
  `grid(columns: (label, 1fr), ..)`, called once per section (Employment
  History, Education, Past Internships, Skills) with very different content
  heights. `logical_id` (span + element) is intentionally shared across
  physically distinct occurrences of one call site — that is what lets a
  repeated code-listing gutter widen to its widest occurrence and lets a
  table's own header row merge correctly across a page break — but the row-
  height lookup used `first_table(logical_id)`, which just returns the FIRST
  matching occurrence, so every later call to the shared section-layout
  function inherited the FIRST call's measured row height. A short row
  ("Past Internships") got a ~330pt minimum height stamped onto it from an
  unrelated, much taller row ("Employment History"), opening a huge blank
  gap. Fixed by giving each occurrence an optional introspection `location`
  (distinct per realized element instance, unlike the span-based
  `logical_id`) and preferring an exact location match before falling back to
  the old first-match behavior; the legitimate cross-page header-row merge
  and column-width-widening aggregation (both genuinely span-only by design)
  are untouched. Worst-case fix: `cv/imtsuki-resume` 3 → 1 pages, exact
  parity. Full-corpus effect (LibreOffice-backed 1408-doc gate): native_good
  473 → 484, fallback_visual 314 → 355, native_degraded 130 → 120,
  fallback_degraded 296 → 253 — every category moved toward higher fidelity,
  none regressed; package_ok unchanged at 1283/1286 (same 3 pre-existing
  export_error docs, independently confirmed not caused by any of this
  session's changes).
- Follow-up fix (5/10, `notes/takei-batu-mynote`): a CJK math notes document
  using the `cjk-spacer` package (Latin/CJK kerning) grew 550% (6 → 39
  pages). `cjk-spacer` brackets essentially every inline-math/punctuation
  boundary in a zero-width "ghost" trick built from `hide(..)` and `h(..)`
  calls inside a `context` block. `HElem` was already recognized as
  paragraph-continuing inline content, but `HideElem` (`#hide[..]`) was not —
  a bare top-level `hide(..)` fell to the generic block dispatch, which
  flushes the paragraph being assembled both before and after it, so this
  kerning idiom fragmented ordinary sentences into one paragraph per word.
  Fixed by adding `HideElem` to the paragraph-continuation whitelist (its
  existing inline handling already lowers it to nothing but harvested
  introspection tags, matching Typst's own full-redaction semantics for
  `hide`). Separately hardened paragraph flushing against a related case: a
  lone `SpaceElem` realize leaves behind around a promoted-to-block child
  (`$ x $`, block because of the internal spaces) used to become its own
  spurious blank-line paragraph; dropped instead, matching real layout's
  insignificant-whitespace-at-a-block-boundary behavior. Worst-case fix:
  `notes/takei-batu-mynote` 39 → 8 pages (6-page reference). Full-corpus
  effect: package_ok steady at 1283/1286, category distribution unchanged
  within noise (`cjk-spacer` is not widespread in this corpus, but both
  fixes are general and transformative wherever they apply).
- Follow-up fix (6/10, `poster/simple-research-poster`): an A1 academic
  poster grew 300% (1 → 4 pages) and rendered with an invisible
  white-on-white title (the dark header banner missing) and a giant,
  page-height logo. The template wraps everything in one outer
  `grid(rows: (13%, 83%, 4%), header, body, footer)`, where the header/
  footer bands are each a `block(fill: .., height: 100%)` sized to their
  own grid row, not the page. Two independent call sites resolved that
  `height: 100%` against the page's full available height instead of the
  measured cell/row box (`available_width` in the same function already
  received correct per-cell scoping; only the height axis lacked it):
  `handle_block_box`'s fixed-height-fill detection, and `display_extents`'s
  relative-image-height resolution. Also extended the per-cell scoping
  helper to cover the raster-fallback region a *bodyless* filled block
  (a plain colored divider, no content) rasterizes against, matching an
  existing precedent in the footer-content-conversion code. Worst-case fix:
  simple-research-poster 4 → 3 pages (1-page reference), header/title/logo
  now render correctly. Full-corpus effect: native_good steady at 484,
  fallback_visual 354 → 361, fallback_degraded 254 → 248 (net improvement,
  no regressions); `report/lion-ecl` (an earlier fix) improved as a bonus,
  9 → 8 pages. A separate, unrelated defect remains in this poster (its
  3-column body collapses to 2 physical Word columns) — left for a future
  target.
- Also diagnosed and explicitly set aside: a `slide_shaped_docx`-flagged
  document (a presentation compiled through the DOCX path, mismatched
  category vs. actual content shape) is a structural pairing issue already
  isolated by the checker as its own informational lane, not a per-document
  exporter defect — excluded from north-star candidate selection going
  forward.
- The prior full authority completed at `1bf829900`, followed by
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

- DOCX integration: 233 tests passed.
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
- DOCX font selection follows Typst's declared-family, `covers`, and
  `fallback` contract. Unavailable fallback-disabled text no longer becomes
  consumer-invented visible glyphs, while unavailable declarations remain
  explicit fidelity evidence. On the frozen `gb-ctr` north star this reduces
  LibreOffice pagination from 202 to 173 pages against a 164-page reference and
  raises semantic coverage from 67.4% to 88.7% without regressing the six-document
  sentinel lane.
- Mixed placed canvases are now classified from realized `PlaceElem` frame tags
  at their nearest finite owner instead of a 64-object threshold. On `gb-ctr`,
  all 224 Cetz canvases are atomic: delegated QA confirms that clock,
  fetch/execute, instruction, and external-bus timing diagrams are complete and
  unclipped. Visual score improves from 0.969502 to 0.969851 and searchable text
  coverage from 88.7% to 95.5%. The export is 175 pages versus the 164-page
  reference; two new blank instruction pages and a repeated external-bus
  footnote remain known pagination/state defects.
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
  was also a valid package and reproducibly triggered LibreOffice's
  `Unspecified Application Error`. Its retained package contains 103 tables,
  75 modern text boxes, and 128 drawings. A later reduction supersedes the
  initial dashed-line hypothesis: the decisive trigger was WPS text boxes inside
  footnotes 5 and 6 (inline raw/code spans) combined with the document's later
  table-heavy flow. The unmodified package failed a fresh bundled
  LibreOfficeDev 26.8 PDF conversion after 300 seconds. Keeping framed footnote
  content as editable flowing runs removes all five footnote-story text boxes,
  preserves the note text, and renders the exact thesis to PDF in about four
  seconds. ZIP/XML validation and the 221-test DOCX structural suite pass. The
  frozen pre-fix evidence remains under
  `target/docx-public-corpus-focus-e2021-1b38352/`; this focused HEAD fix is not
  folded into the older full-corpus totals above.
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
- Explicit paragraph leading now uses Typst's resolved font-edge text frame
  instead of adding the nominal font size. With LibreOffice 26.2.4.2 held fixed,
  Alex Mathnote improves from 187 to 171 pages, while Xenolay remains 163 pages
  from a byte-identical DOCX. The earlier reported Xenolay 129-page regression
  came from LibreOfficeDev 26.8 alpha and was incorrectly compared with the
  stable-consumer result. The checker now freezes revision, dirty state, and
  binary identity once per campaign, and refuses resumes whose `soffice`,
  `pdftotext`, or `pdftoppm` identities differ from the original authority. A
  six-document LibreOffice 26.2 guard run passed package, consumer, and review
  lanes 6/6: Mathnote improved 187 to 171 pages, the table/footnote thesis
  improved 212 to 195, Tura retained 293, Xenolay retained 163, gb-ctr retained
  202, and a missing-font C++ guide retained its exact one page. Evidence is in
  `target/docx-public-corpus-focus-leading-grid-stable/`.
- Tura's 420 native DOCX tables were traced to editable layout grids produced by
  roughly 280 code blocks, not to authored semantic tables. Word added symmetric
  grid-cell margins outside the already measured row minimum, expanding the
  265-page reference to 351 pages. Layout-grid rows whose cells already request
  centered alignment and equal top/bottom inset now retain the full measured row
  box and realize that inset through the existing centering instead of counting it
  again as `w:tcMar`. The focused consumer result is 293 pages, score `0.975292`,
  semantic text coverage `0.991067`, and a successful 6,880-region review round
  trip. This removes 58 of 86 excess pages without exact/clipping heights or a
  package-name heuristic. Delegated headless visual QA found no new glyph clipping,
  overlap, or lost row separation in sampled code-heavy pages. Semantic tables
  keep their authored Word cell margins. Durable evidence is under
  `target/docx-public-corpus-focus-centered-grid-stable/`; the older full-corpus
  totals above do not include this focused fix.
- Tura's remaining page growth was then traced to a 381-twip editable code-line
  number cell whose symmetric 84-twip margins left two-digit 9pt monospace
  numbers on LibreOffice's wrap boundary. `w:noWrap` had no effect; 83-twip
  margins still produced 293 pages, while 82 kept the numbers horizontal. The
  mapper now applies that bounded two-twip-per-edge tolerance only to known
  layout-grid cells with centered vertical alignment and equal nonzero
  horizontal insets; semantic tables and asymmetric, zero-inset, or non-centered
  cells remain unchanged. A measured-row-only intermediate rendered at 261
  pages but left lines 65–75 wrapping and split line 69 across a page boundary;
  package inspection traced those to 465 unmeasured centered rows, including all
  28 three-digit gutters. Separating the horizontal tolerance from vertical
  row-box measurement produces 254 pages against the 265-page reference, score
  `0.975362`, semantic coverage `0.991067`, and a successful 6,880-region review
  round trip. Delegated 180-dpi visual QA confirmed that late lines 65–75 remain
  horizontal, line 69 no longer splits across pages, padding and row alignment
  remain intact, and there is no clipping, collision, overlap, or page-furniture
  contact. The same-consumer six-document
  guard again passed package, LibreOffice, and review lanes 6/6; evidence is in
  `target/docx-public-corpus-focus-grid-inline-guard-v2-stable/`.
  A later three-digit case initially remained: lines 100–117 wrapped and line
  109 split across pages because repeated grids shared one logical identity and
  DOCX retained only the first occurrence's two-digit auto-track measurement.
  Typst had already measured the later `(auto, 1fr)` occurrence wider. DOCX now
  takes the widest same-context auto measurement and donates the exact delta
  from fractional tracks, preserving total table width; a bounded two-twip
  Office metric tolerance makes the result call-order independent. Tura now
  emits a 489/8,262-twip split with the same 8,751-twip total and renders 254
  pages, score `0.975426`. Delegated QA confirmed all lines 100–117 and line 109
  are horizontal and intact, adjacent code does not newly wrap, and the earlier
  two-digit sample is unchanged. Focused evidence is under
  `target/docx-public-corpus-focus-tura-auto-width-v4-stable/`.
  The same-consumer six-document guard then passed package, LibreOffice 26.2.4.2,
  and review lanes 6/6 after a serial retry recovered one load-sensitive Tura
  timeout. Mathnote remains 171 pages, the table/footnote thesis 195, Xenolay
  163, gb-ctr 202, and the missing-font C++ guide one. Combined evidence is in
  `target/docx-public-corpus-focus-grid-auto-width-v4-stable/`.
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

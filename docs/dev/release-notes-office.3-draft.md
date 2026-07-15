# v0.15.0-office.3 — draft release notes

> **DRAFT.** Not published. Do not tag or cut a release from this draft yet.
> A fresh post-fix corpus authority and Microsoft Office consumer validation
> remain release blockers; the former TOC page-cache blocker is fixed.

Supersedes `v0.15.0-office.2` (2026-07-04). Like both prior releases, this
remains **experimental preview** software: Typst can export `.docx` and
`.pptx` files with substantial native editability and image fallbacks, but
compatibility and fidelity are document- and consumer-dependent. This is not
an arbitrary-document-fidelity or production-support claim — see
[`docs/dev/office-export-shipping-readiness.md`](office-export-shipping-readiness.md)
for the full current verdict.

## What improved

### Word (.docx)

- Tighter, more Word-like spacing: table cells no longer accumulate extra
  paragraph gaps at cell or group boundaries, list spacing is preserved
  correctly across group boundaries, and native paragraph/table-cell margins
  match the authored document more closely.
- More layout stays native instead of falling back: explicit column breaks,
  sequential multi-column flow, page breaks after column blocks, top-floated
  text boxes, placed-image anchors, and figure captions centered with their
  bodies are now preserved.
- Hyperlinks keep their authored appearance and styling instead of falling
  back to Word's default link formatting.
- Text boxes can now carry gradient fills natively.
- Licensed fonts are embedded when their OpenType permissions allow it.
- Corpus-driven hardening pass: a large batch of real-document fallback and
  classification fixes landed together as part of consolidating the combined
  DOCX branch history (see "Corpus and validation" below).
- Table-of-contents entries now use their physical paged-snapshot positions
  instead of caching page `1` for every entry.
- Review export now emits the required terminal paragraph in table cells; 90 of
  the 91 historical failures now pass, with the remaining template failing
  during source compilation before export.
- Tall block-level picture fallbacks are page-tiled, preventing one reproduced
  LibreOffice import hang while retaining searchable hidden text.

### PowerPoint (.pptx)

- Tables are substantially more native: cell alignment, insets, gutters (as
  editable spacer tracks), border dash styles, and richer per-cell content are
  now preserved instead of falling back.
- Transformed tables use a whole-region picture fallback instead of disappearing.
- Explicit page links are remapped when `--pages` filters the deck; links to omitted
  pages are dropped instead of pointing at a missing slide.
- Text fidelity improvements: native first-line indents, wide paragraph
  leading, single-line bullet layout, text highlights, and authored hyperlink
  styling (including on shapes and pictures, not just text) are now preserved.
- Math: structured (OMML-eligible) math is preserved in the compatibility
  fallback, with further improvements to the compatibility math rendering
  path.
- Rotated live text is positioned from rotation-neutral bounds instead of
  drifting.
- Mixed-size decks are fit and centered onto PowerPoint's single slide canvas
  instead of failing to export.
- Native SVG cover-crop handling, explicit column-break preservation, and
  preserved highlights/inline math inside multi-column layouts.
- Licensed fonts are embedded when their OpenType permissions allow it.

### Shared / cross-format

- Gradient fills preserve Typst's perceptual color interpolation instead of a
  flatter linear approximation, for both DOCX and PPTX.
- OOXML stroke handling now distinguishes dashed strokes from dotted strokes
  instead of collapsing them to the same preset.

### Documentation

- The office-export documentation now points to a single current authority,
  [`docs/dev/office-export-shipping-readiness.md`](office-export-shipping-readiness.md),
  with corpus validation counts and the supported-preview wording kept in
  sync with the latest corpus campaign.
- DOCX fidelity-manifest behavior in the docs now matches the CLI (new CLI
  exports embed it by default; library callers remain opt-in).

## Known limitations

These are carried over verbatim in spirit from the shipping-readiness
document's verified-gaps section — this list is intentionally not exhaustive;
see that document for the complete picture.

**DOCX:**
- A fresh, unfiltered full public-corpus authority after the latest fixes is
  not yet complete; the existing 1,408-record authority is a pre-fix baseline.
- Real low-fidelity documents remain, including `presentation/sleiden-lei`
  (page growth, displaced content, missing text).
- Headers/footers that vary beyond Word's first/even/default model freeze to
  a page-one approximation.
- Complex diagrams, non-OOXML transforms, radial/conic fills, PDF/WebP
  images, and some layout callbacks still rasterize and are not editable.
- Fractional page-space distribution (`1fr` spacing) has no flowing-Word
  equivalent.

**PPTX:**
- Other unsupported or partially captured table edge cases still need broader
  corpus coverage after the transformed-table fallback fix.
- Mixed page sizes are uniformly scaled and centered on one global slide
  canvas, which can introduce letterboxing.
- Table cell-math handling is incomplete, and consumer line-box metrics can
  still expand automatic row heights.
- LibreOffice and older Office versions render math through a compact Unicode
  fallback, not native stacked OMML; stacked fractions and radicals are not
  reproduced there.
- A 2026-07-13 LibreOffice smoke export opened without repair and preserved
  editable content, but wrapped table/list text differently, overlapped a
  list with a following shape, and rendered one inline equation less
  faithfully. This confirms package validity but is not a general
  visual-fidelity claim.

**Pandoc target:** retained as known-incomplete historical/development code.
It is not a supported preview and is not a release gate.

## Validation

Carried forward as a pre-fix baseline from revision
`c07adc99b70f`, 1,408-document public corpus, manifest SHA-256
`9b92ee3092b97c9c600547722ccb2397a5d21a1eea1d224418d1c57d1a9e99af`):

- DOCX: 1,407/1,408 packages valid; mean LibreOffice visual similarity
  `0.954462` across 1,397 scored renders.
- PPTX: all 120 compilable presentation templates exported valid OOXML with
  zero export errors; mean LibreOffice visual similarity `0.992` across all
  120 decks.
- DOCX integration (212), PPTX integration (65), DOCX review round-trip (22),
  and OOXML math conversion (27) test suites passed; strict Clippy passed
  with warnings denied.

See
[`docs/dev/office-export-shipping-readiness.md`](office-export-shipping-readiness.md)
for full detail, including why a fresh authority is required before classifying
the aggregate DOCX pagination delta against v12 on current HEAD.

<details>
<summary>Commit-level detail (`v0.15.0-office.2..HEAD`, docs/CI-only commits omitted from the summary above)</summary>

```
14d36e2 pptx: remap links in filtered exports
9bea579 docx: tile tall raster fallbacks
fe4afc3d9 pptx: preserve transformed table content
42a26b348 docx: fix TOC and review export validation
a0ab69044 docs: record current office corpus authority
c07adc99b docs: narrow the supported export preview
bd1318c72 pptx: preserve table border dash styles
fa5ead00c docs: refresh office export validation counts
1dc375df5 pptx: improve compatibility math fallbacks
06b264e52 pptx: preserve table gutters as spacer tracks
8289be144 docx: collapse internal cell paragraph gaps
62bedeec6 pptx: preserve native table cell insets
1c92c6540 docx: collapse paragraph spacing at cell boundaries
c5a08daa9 office: preserve perceptual gradient interpolation
e3af3d0b9 docx: preserve page breaks after block columns
a029f40ab docs: reflect native pptx object hyperlinks
a0817f4d6 docx: center figure captions with their bodies
e62453ae9 pptx: fit mixed-size pages to the slide canvas
a668a0006 docx: keep list spacing on group boundaries
84b388e50 pptx: position rotated live text from neutral bounds
3157e0f86 docx: align simple furniture to margin bands
bd0f69118 docx: preserve furniture paragraph alignment
81b368272 docx: avoid phantom table-cell paragraphs
927513553 pptx: embed license-permitted fonts
7354a5e1f docx: avoid double-counting table row insets
a841443fc docx: embed license-permitted fonts
8d769424e pptx: preserve rich content in table cells
f18d150c1 docx: preserve authored hyperlink appearance
28118970d docx: preserve explicit column breaks
9f12563e9 pptx: preserve explicit column breaks
ca22402e6 pptx: preserve highlights inside columns
e84969292 pptx: preserve inline math inside columns
1198a438d docx: preserve top float text box flow
80a198ba1 docx: preserve sequential column flow
16879477b pptx: preserve authored hyperlink styling
b71b74dc8 docx: preserve placed image anchors through tags
73a28cba1 docx: preserve explicit hyperlink styling
07bdee05a pptx: preserve native SVG cover crops
3c567eb73 pptx: preserve wide paragraph leading
be3f54037 pptx: preserve native first-line indents
985f8b23b docx: preserve native paragraph spacing
ad7000ea6 ooxml: distinguish dashed strokes from dots
e30dfadfd docx: preserve native table cell margins
52c8378a6 pptx: preserve native text highlights
c0d583d5d Show full escape sequence in symbol list flyouts (#8418)
f272c5a6a docx: preserve gradient fills on editable text boxes
4c66cf47f pptx: preserve native table cell alignment
d8456354e pptx: preserve links on native shapes and pictures
e95c9cf0f pptx: preserve single-line bullet layout
25b558779 pptx: preserve structured math in compatibility fallback
c4ef49535 docs: record Office export shipping readiness
b111ce081 docs: align DOCX fidelity manifest behavior
c3e578334 office: reconcile combined exporter histories
8510faf33 docx: complete corpus classification and fallback hardening
```

Note: `c0d583d5d` (`Show full escape sequence in symbol list flyouts`) is an
unrelated upstream `main` commit pulled in by a merge and is not part of the
office-export feature set.

</details>

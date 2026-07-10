# How this compares to existing Typst → Office tools

Everything below is measured, not asserted. Shared test set: 8 documents from an
818-doc corpus of real published Typst templates — 4 slide decks (calmly-touying and
touying-aqua [Touying], basic-polylux [Polylux], diatypst) and 4 documents (arkheion
paper, academi-notes-gr report, basic-resume CV, classicthesis book). All tools were
run at their latest released versions on the same inputs; typ2docx used its free
`pdf2docx` backend (its higher-quality backends require Adobe Acrobat or Adobe cloud
credentials).

**Fidelity metric:** render the exported file to PDF via LibreOffice headless,
rasterize both it and the gold Typst PDF to fixed-size grayscale strips, score mean
per-pixel agreement in [0, 1] — the same oracle this exporter is validated with
corpus-wide. **Structure metrics** are counted from the raw OOXML. The scoring and
audit scripts are in [`crates/typst-pptx/compare/`](crates/typst-pptx/compare/), so every number here is reproducible.

> **Dated benchmark snapshot (2026-07-03).** These measurements compare the
> revisions and installed renderers used on that date. They are regression
> evidence, not the current feature contract; see
> [`docs/dev/office-export-architecture.md`](docs/dev/office-export-architecture.md)
> for the current issue register.

## PPTX: vs typ2pptx and touying-exporter

**Fidelity (gold-PDF agreement, higher is better):**

| deck | **typst-pptx (this)** | typ2pptx | touying-exporter |
|---|---|---|---|
| calmly-touying | **0.998** | 0.961 | 0.996 (raster) |
| touying-aqua | **0.994** | 0.980 | — |
| basic-polylux | **0.998** | 0.988 | — |
| diatypst | **0.998** | 0.993 | — |

This exporter wins every head-to-head — including against touying-exporter's
*screenshots*, which are pixel-perfect by construction but 0% editable (verified: 13
slides = 13 `<p:pic>` images, zero `<a:t>` text elements).

**Structure:**

| | this | typ2pptx | touying-exporter |
|---|---|---|---|
| live text | yes | yes | **no (images only)** |
| hyperlinks | **206 + 1** (diatypst / touying-aqua) | **0 on all four decks** | 0 |
| gradients / alpha | 13 / 3 | 13 / 3 | — |

**What the gap looks like**
([`compare/calmly-touying-slide2-3way.png`](crates/typst-pptx/compare/calmly-touying-slide2-3way.png)):
on the "Introduction" section divider, this exporter matches the gold PDF's serif
face and position; typ2pptx renders it bold sans and wraps it mid-word
("Introducti / on"). Verified in the XML: the deck's titles are Libertinus Serif
(this exporter emits `typeface="Libertinus Serif"`); typ2pptx emits
`typeface="Arial" b="1"` — and a source read shows why: **font families are
hard-coded** (its `converter.py:82`: regular/bold/italic → Arial, mono → Consolas,
math → Cambria Math). It never recovers the document's real fonts; every deck it
converts is Arial. Arial's wider metrics then overflow the reconstructed text box,
hence the mid-word wrap.

**Why the fidelity gap:** typ2pptx compiles to SVG via typst-ts, then *reconstructs*
text from the rendered output with heuristics (its own source, file:line-verified):
bold/italic guessed from glyph-width ratios (`typst_svg_parser.py:554`), text top
guessed as `baseline − 0.8 × font_size` (`:848`), lines grouped by a 2px baseline
tolerance, and **word spaces inferred from pixel gaps** (`gap > 0.15 × font size` →
insert a space, `converter.py:2334`). Math is never native — simple formulas become
Cambria Math text, complex ones become grouped glyph-outline shapes. Content
failures are frequently silent (`except Exception: pass` around notes, images, and
math-glyph insertion). Its README calls itself "a vibe coding project… many
conversion errors and edge cases remain." This exporter consumes Typst's own
laid-out page frames, so every position, font, size, weight, color, and space is the
compiler's ground truth rather than reconstructing it from SVG. The exporter
still has to choose DrawingML text-box bounds and relies on recipient fonts, so
editable text can reflow differently in PowerPoint or LibreOffice.

**Fairness notes:** typ2pptx handled all four decks including the non-Touying ones,
installs from PyPI in seconds, and runs fast (0.2–0.9 s/deck; this exporter
0.07–0.12 s). It's a reasonable tool; the architectural ceiling is just lower.

## Rasterization is audited, not hidden

A pixel metric can't distinguish live text from a screenshot — a fully rasterized
deck scores ~perfect. So nativeness is audited separately: live `<a:t>` words
against the gold PDF's text layer, across 112 real presentation templates
([`compare/nativeness_audit.py`](crates/typst-pptx/compare/nativeness_audit.py)). Result: **the median
deck preserves 100% of its words as editable text (mean 97.5%)**.

The audit is also how the exporter's biggest content-loss bug was found and fixed:
clipped groups (the defensive `#box(clip: true)` card every slide theme uses) used
to rasterize wholesale, silently baking 27,686 characters of corpus text into
pictures while the pixel score stayed at 0.99. Now a clipped group is rasterized
only when the clip *provably matters*: a rectangular clip nothing overflows is
walked natively, and any other clip shape is render-probed — the frame is rendered
with and without the clip, and byte-identical pixels prove the clip removes nothing.
Swallowed text dropped 69%; what remains is pixel-proven visible clipping, where the
raster is the fidelity-correct choice. `PPTX_DEBUG_RASTER=1` logs every fallback
with its reason and the text characters affected.

## DOCX: vs typ2docx and pandoc

**Reliability first:** pandoc's Typst reader failed **0/4** real templates — it
parses syntax but doesn't evaluate code, so any template with
`#import "@preview/..."` dies immediately. typ2docx crashed on 1/4 (classicthesis:
an internal panic, `extract.rs "project should compile: unable to get the current
date"` — its embedded compiler doesn't wire up datetime; the same file compiles fine
with plain typst). This exporter: 4/4.

**Fidelity (pixels):**

| doc | this | typ2docx (pdf2docx backend) |
|---|---|---|
| academi-notes-gr | 0.968 | **0.992** |
| arkheion | 0.951 | **0.970** |
| basic-resume | 0.944 | **0.964** |
| classicthesis | **0.992** | crash |

**typ2docx wins raw pixels on 3/4 — and that's exactly what you'd expect,** because
it is architecturally a PDF-to-DOCX reconstruction pipeline with native OMML math
spliced in afterward: it renders the PDF and reconstructs a page-frozen facsimile of
it. A facsimile is the easiest thing to score well on a visual diff. The question
for a Word document is what you can *do* with it:

**Structure (arkheion / academi-notes-gr / basic-resume):**

| | this | typ2docx |
|---|---|---|
| `Heading N` style refs (→ navigation pane, live TOC) | **13 / 4 / 0** | 0 / 0 / 0 |
| live fields (REF cross-refs, SEQ, TOC) | **4 / 10 / 0** | 0 / 0 / 0 |
| header/footer parts | **1 / 1 / 0** | 0 / 0 / 0 |
| styles | 35–39 curated | 164 auto-generated |
| OMML math | 6 / 12 / 7 | 4 / 12 / 7 |
| words preserved | 579 / 105 / 429 | 588 / 110 / 430 |

Both preserve the text (word counts match) and both produce native OMML math. The
difference: typ2docx's output is semantically flat — no heading styles, no live
cross-references, headers/footers baked into the body — because only OMML math is
spliced semantically (a source read confirms only `word/document.xml` is merged;
everything else "keeps whatever the PDF converter inferred"). Its math merge is
marker-based and brittle (its issue #76 documents unreplaced markers producing a
file Word can't open with the free backend). This exporter walks Typst's realized
element tree, so headings, lists, tables, footnotes, fields, TOC, and sections are
constructed as native Word objects.

**Dependencies & speed:** typ2docx = Python + ~130 MB of deps (pymupdf, opencv,
saxonche) + pandoc + a typst binary, 1.8–8.4 s/doc; best quality requires Adobe
Acrobat or Adobe cloud credentials. This exporter = the one typst binary you already
have, 0.07–0.12 s/doc, byte-reproducible under `SOURCE_DATE_EPOCH`.

## The one-paragraph version

Existing converters work *backwards from rendered output* — typ2docx reconstructs a
Word file from the PDF, typ2pptx reconstructs slides from SVG with font-guessing
heuristics, touying-exporter screenshots each slide. This exporter works *forwards
from the compiler*: DOCX walks Typst's realized element tree (real heading styles,
live REF/TOC fields, native tables/footnotes/math), and PPTX takes the laid-out
page frames (direct Typst-computed positions, live text, native
shapes/gradients, working hyperlinks).
Measured on the same corpus with the same oracle, it beats typ2pptx on visual
fidelity on every deck tested and is the only PPTX path with working hyperlinks;
typ2docx scores higher on raw pixels (it ships a page facsimile) but has zero
heading styles, zero live fields, and zero header/footer parts in its output — and
pandoc fails outright on any template that imports a package. One binary, no
Python/pandoc/Adobe pipeline, ~0.1 s per document.

## Where the other tools are genuinely ahead

- **typ2docx with the Adobe Acrobat backend** (not benchmarked here — needs
  Acrobat): likely narrows the visual gap, and its facsimile approach is exactly
  right if your goal is "a docx that looks identical" rather than "a docx I can
  edit."
- **touying-exporter**: dead simple, by the Touying authors, and a raster is
  bulletproof — if you only need to *present* through PowerPoint, it's fine.
- Both install via `pip` without compiling a Rust toolchain; until this fork's
  binary releases circulate, `pip install` beats `cargo build --release`.

## Reproducing

The compare scripts live in [`crates/typst-pptx/compare/`](crates/typst-pptx/compare/): `score_files.py` (fidelity),
`audit_structure.py` (OOXML structure counts), `nativeness_audit.py` (corpus
text-recovery + raster-event audit). Each is a `uv run`-able PEP-723 script;
methodology details are in their docstrings. Measured 2026-07-03.

# typst-pptx — PowerPoint export for Typst (community fork)

`typst-pptx` compiles a Typst document straight to a Microsoft PowerPoint
(`.pptx`) presentation: **one Typst page becomes one editable slide**. It opens
without a repair prompt in **Microsoft PowerPoint** and **LibreOffice Impress**.

This is a sibling of the [`typst-docx`](../typst-docx/README.md) exporter, but it
works very differently. Word documents reflow, so DOCX export walks Typst's
*realized element tree* and rebuilds semantic structure (headings, lists,
tables). Slides do **not** reflow — a slide is a fixed canvas — so PPTX export
consumes the already **laid-out** `PagedDocument` and places every element at
its Typst-computed position, the same input the PNG and SVG renderers use.
Geometry is therefore direct rather than reconstructed, while editable text can
still reflow when PowerPoint or Impress substitutes fonts or applies different
text-box metrics.

> **Experimental preview.** Like Typst's own HTML export, this is not part of
> upstream Typst and is not endorsed by the Typst maintainers. Please report
> anything that opens wrong or looks off.

## Usage

```sh
# by output extension…
typst compile deck.typ deck.pptx
# …or with an explicit format
typst compile --format pptx deck.typ
```

`typst-pptx` is happiest with slide-shaped documents — the templates built on
[Touying](https://touying-typ.github.io/) and
[Polylux](https://polylux.dev/), or any `#set page(width:.., height:..)` in a
16:9 / 16:10 / 4:3 / cinema ratio. Export a portrait or A4 document to `.pptx`
and the CLI will suggest `.docx` instead (and vice-versa); it's a soft hint, not
an error.

## What maps natively

- **Text** — every run is a live, editable DrawingML run: font family, size,
  bold/italic, color, letter-spacing, and RTL. Positions use a measured
  first-baseline rule so text lands where Typst placed it. Exact font programs
  used by editable runs are embedded as PresentationML EOT font parts when the
  OpenType license grants Installable or Editable embedding; restricted,
  preview/print-only, bitmap-only, and collection faces are left unembedded.
- **Links** — external URLs and same-deck slide jumps (`#link((page: n))`) on
  editable text, vector shapes, and pictures; object links use the full authored
  hit area.
- **Vector shapes** — `#rect`, `#circle`/`#ellipse`, `#line`, `#curve`,
  `#polygon` become `prstGeom`/`custGeom` shapes with solid, **linear-gradient**,
  and **translucent** (alpha) fills, and native stroke width / dash / cap.
  Perceptual Typst gradients remain editable and use adaptively sampled native
  stops so Office's sRGB interpolation preserves the authored colors.
- **Slide backgrounds** — a page `fill:` of a solid color or a linear gradient
  becomes the slide's `p:bg`, with the same interpolation-preserving stops.
- **Images** — PNG/JPEG/GIF embedded verbatim; media is de-duplicated across
  slides by content hash.
- **SVG images** — embedded as native SVG with a PNG compatibility fallback.
- **Tables** — eligible laid-out tables become editable DrawingML tables with
  native cell fills, stroke width/dash/cap, alignment, per-side text insets,
  and borderless spacer tracks for row/column gutters; broader table styling and
  transformed-table fallback are still incomplete, and consumer line-box
  metrics can expand automatic row heights.
- **Math** — equations are lowered from Typst's resolved math IR by
  `typst-omml` (the same code the DOCX exporter uses) and emitted as OMML inside
  an Office compatibility wrapper, with an authored-size compact Unicode
  DrawingML text fallback for consumers that do not support the native math
  branch. Two things do not survive that route, because the equation is
  re-resolved from the introspector after layout: an ambient `#show` recipe on a
  math symbol does not fire, and a `#context` read inside math resolves against
  default styles. An equation containing an inline `box(..)` is refused whole
  and painted as ordinary text and shapes rather than shipped half-native.
- **Presentation UX** — notes, slide numbers, and inferred title/body
  placeholders are preserved when the source exposes enough structure.

## What falls back to a picture

Anything with no clean PowerPoint equivalent is rasterized to a positioned image
so the visual is preserved: PDF images, CeTZ/fletcher diagrams, complex clips or
skewed groups, radial/conic gradients, and opaque layout callbacks (`#block`
bodies whose content can only be produced by re-running layout). The rest of the
slide stays native and editable.

## Known limitations

Every fidelity fallback this exporter takes now records itself in a
`FidelityReport` — a `Representation`, a `LossSet`, and a `DecisionReason`
naming the specific site. **That report, and the
[support matrix](PPTX_SUPPORT_MATRIX.md) generated alongside it, are the
source of truth**; this list is a prose summary of it and should be re-derived
from `src/report.rs`'s `DecisionReason` variants rather than edited
independently.

Fallbacks the report names (see the matrix for what each loses):

- **Raster fallbacks** — a group with a clip or skew, text under a
  non-uniform transform, a shape whose geometry, fill or stroke DrawingML
  cannot express (each named separately: a degenerate path, a conic or
  off-centre radial gradient, a gradient stroke), a picture placed under a
  skew or non-uniform scale, and a transformed table. Rotated and uniformly
  scaled pictures are *not* in this list: they are native `a:xfrm` boxes with
  a `rot`.
- **Approximations** — gradient/tiling *text* fill collapsed to one solid
  colour (a DrawingML run carries only one), a tiling shape fill rendered to a
  static tile image, and a page fill that is not solid-or-linear falling back
  to plain **white**.
- **Native-with-fallback** — equations, emitted as OMML behind an
  `mc:AlternateContent` switch with a plain-text branch for consumers without
  the extension.
- **Drops** — an equation whose OMML source or geometry could not be
  recovered.

Two limitations are *not* fallback sites, so they are not in the report and
belong here:

- **Editable text layout still varies by consumer.** Standard EOT font parts
  preserve the source face in PowerPoint and current LibreOffice Impress, but
  the applications apply different text-box and line-breaking metrics. A
  narrow editable text box can wrap differently even with the same embedded
  font.
- **Mixed page sizes** cannot be represented: PowerPoint has one global slide
  size. The CLI warns; off-size pages are uniformly scaled to fit and centred
  on the first page's canvas, which can letterbox.

## Validation

Every emitted XML part is namespace-well-formed, and the package is checked
against PowerPoint's stricter-than-LibreOffice schema rules (theme style-matrix
minimums, `custGeom` literal coordinates, no `.rels` content-type overrides — a
regression test guards each). Output is byte-for-byte reproducible under
`SOURCE_DATE_EPOCH`.

Fidelity is measured by rendering both the gold PDF and the exported `.pptx`
(via LibreOffice) to images and scoring their similarity. In the 2026-07-03
112-template snapshot, the mean score was **0.995** (median 0.996), with no
export failures. These are historical regression measurements, not a guarantee
for every Office version, installed-font set, or later exporter revision.

Nativeness is audited separately (a pixel diff can't tell live text from a
screenshot): comparing live `<a:t>` words against the PDF's text layer, the
median deck preserves **100%** of its words as editable text (mean 97.5%).
Rasterization is a last resort in a checkable sense — a clipped group is only
rasterized after a render probe proves the clip visibly alters pixels
(otherwise its children are exported natively), so every picture in the output
is either a source image or a fallback the exporter can prove it needed. Set
`PPTX_DEBUG_RASTER=1` to log every fallback with its reason and the text
characters affected.

Integration tests live in [`../../tests/src/pptx.rs`](../../tests/src/pptx.rs), and
a measured head-to-head against typ2pptx and touying-exporter is in the repo-level
[`COMPARISON.md`](../../COMPARISON.md).

The current pipeline, fidelity model, verified failure modes, and proposed
preflight architecture are documented in
[`../../docs/dev/office-export-architecture.md`](../../docs/dev/office-export-architecture.md).

# typst-pptx — PowerPoint export for Typst (community fork)

`typst-pptx` compiles a Typst document straight to a Microsoft PowerPoint
(`.pptx`) presentation: **one Typst page becomes one editable slide**. It opens
without a repair prompt in **Microsoft PowerPoint** and **LibreOffice Impress**.

This is a sibling of the [`typst-docx`](../typst-docx/README.md) exporter, but it
works very differently. Word documents reflow, so DOCX export walks Typst's
*realized element tree* and rebuilds semantic structure (headings, lists,
tables). Slides do **not** reflow — a slide is a fixed canvas — so PPTX export
consumes the already **laid-out** `PagedDocument` and places every element at
its exact position, the same input the PNG and SVG renderers use. There are no
show rules, no convergence passes, and no engine: positions are exact by
construction.

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
  first-baseline rule so text lands where Typst placed it.
- **Links** — external URLs and same-deck slide jumps (`#link((page: n))`).
- **Vector shapes** — `#rect`, `#circle`/`#ellipse`, `#line`, `#curve`,
  `#polygon` become `prstGeom`/`custGeom` shapes with solid, **linear-gradient**,
  and **translucent** (alpha) fills, and native stroke width / dash / cap.
- **Slide backgrounds** — a page `fill:` of a solid color or a linear gradient
  becomes the slide's `p:bg`.
- **Images** — PNG/JPEG/GIF embedded verbatim; media is de-duplicated across
  slides by content hash.
- **Groups** — rotated/scaled/nested composites map to `p:grpSp` group shapes.

## What falls back to a picture

Anything with no clean PowerPoint equivalent is rasterized to a positioned image
so the visual is preserved exactly: SVG/PDF images, CeTZ/fletcher diagrams,
math, radial/conic gradients, and opaque layout callbacks (`#block` bodies whose
content can only be produced by re-running layout). The rest of the slide stays
native and editable.

## Known limitations

- **Math** renders as a rasterized image, not native PowerPoint equations (OOXML
  math — OMML — is a Word format; PowerPoint uses it only in a limited way).
- **Rotated live text** (`#rotate(90deg)[…]`) may be offset from Typst; the text
  stays editable but its box position is approximate.
- **Hyperlinks on a shape or image** (rather than on text) are dropped; the shape
  still renders.
- **Mixed page sizes** in one document are all scaled to the first page's size
  (PowerPoint has a single global slide size); the CLI warns when this happens.
- **Gradient/tiling *text* fills** are approximated with a representative solid
  color (a run can carry only one color), so the text stays visible.

## Validation

Every emitted XML part is namespace-well-formed, and the package is checked
against PowerPoint's stricter-than-LibreOffice schema rules (theme style-matrix
minimums, `custGeom` literal coordinates, no `.rels` content-type overrides — a
regression test guards each). Output is byte-for-byte reproducible under
`SOURCE_DATE_EPOCH`.

Fidelity is measured by rendering both the gold PDF and the exported `.pptx`
(via LibreOffice) to images and scoring their similarity. Across 112 real
presentation templates the mean score is **0.995** (median 0.996), with no
export failures.

Integration tests live in [`../../tests/src/pptx.rs`](../../tests/src/pptx.rs), and
a measured head-to-head against typ2pptx and touying-exporter is in the repo-level
[`COMPARISON.md`](../../COMPARISON.md).

# typst-pptx-import

Convert PowerPoint presentations (`.pptx`) into Typst source, as
[touying](https://github.com/touying-typ/touying) slides.

The fourth corner of the Office interop set, and the mirror of
[`typst-pptx`](../typst-pptx):

```text
.pptx ─opc::Reader─► xml ─pml::parse─► PmlPackage ─lower─► TypstDoc ─emit─► .typ + assets
         (ooxml-core)         │          (PowerPoint IR)  (mappers) (Typst IR)  (touying)
                         resolve/ (masters, layouts, theme colours)
```

```
cargo run -p typst-pptx-import --example import -- deck.pptx out.typ
cargo run -p typst-pptx-import --example import -- --idiomatic deck.pptx out.typ
```

## Why this is not the Word importer with the nouns changed

> **A `.docx` is a flow; a `.pptx` is a coordinate system.**

Word content is a linear stream that Typst also lays out linearly, so
[`typst-docx-import`](../typst-docx-import)'s job is mostly translation. Every
PowerPoint shape instead carries an absolute `a:off`/`a:ext` in EMU, and there
is no flow to recover. Two consequences shape the whole crate:

**Fidelity is a choice, not a quality.** `Placed` (the default) reproduces
every shape at its authored coordinates with `#place`. It is the only mode
that **round-trips**: `typst-pptx` exports from laid-out frames, so a shape
placed at its original offset comes back out at that same offset.
`--idiomatic` promotes the placeholders PowerPoint itself labelled `title`
into touying headings — nicer to edit, and wrong the moment a designer put a
"body" box somewhere the flow would not.

**Nothing states its own formatting.** A slide's shapes inherit position,
size, font, colour and bullet style from the matching placeholder on their
**layout**, which inherits from the **master**, whose colours are *names* that
only `theme1.xml` resolves. Three levels and a theme, keyed by placeholder
identity rather than by a style name — unlike Word's one-dimensional `basedOn`
walk, and present in 100% of real presentations. Skip it and every slide is in
the wrong font, colour and place.

## Support matrix

Corpus percentages are document frequency across the 542-presentation gate
corpus (LibreOffice `sd/qa`, Apache POI, Tika).

| Symbol | Meaning |
|---|---|
| ✅ | Recovered as a faithful, editable Typst construct |
| ◐ | Mapped, with a stated difference — the report names it |
| ⊘ | Recognised and deliberately not mapped; reported |
| — | No Typst construct exists to carry it |

### Deck structure

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Slide order | ✅ | — | From `p:sldIdLst`, which is the running order. Archive order is not. |
| Slide size | ✅ | — | `p:sldSz` becomes an exact `config-page(width:, height:)`. Stated absolutely rather than as an aspect ratio, because 4:3, 16:9, A4-landscape and custom poster sizes all occur and only the exact size keeps `#place` coordinates meaning what they meant. |
| Slide layouts / masters | ✅ | 100% | Resolved, not reproduced: the chain supplies each placeholder's geometry and text defaults. |
| Theme colours | ✅ | 34.1% | `a:schemeClr` → the master's `p:clrMap` → `theme1.xml`, with `lumMod`/`lumOff`/`shade`/`tint`/`alpha` applied. |
| Speaker notes | ✅ | 13.9% | Emitted as the `<pdfpc-file>` metadata `typst-pptx` reads back, so notes survive the round trip rather than needing a second convention. |
| Hidden slides | ◐ | — | Kept, with a comment saying PowerPoint hid them. Dropping authored content silently is the one thing this crate tries never to do. |
| Slide background | ✅ | — | Slide → layout → master, the same three-level fallback the shapes use. |
| Transitions | — | 13.6% | Typst output is static; there is no timeline. |
| Animations (`p:timing`) | — | 23.2% | Same. |

### Text

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Runs, fonts, size, bold, italic | ✅ | 65.7% | |
| Colour, highlight, tracking, caps | ✅ | — | |
| Super/subscript | ✅ | — | From `@baseline`'s sign. |
| Placeholders (`p:ph`) | ✅ | 36.3% | Matched to the layout by `idx` first and `type` second — the order PowerPoint uses, and the only way to tell two body placeholders apart. |
| Bullets and numbering | ✅ | 8.1% | `a:buChar`/`a:buAutoNum` with the outline level as nesting depth. An authored glyph is reproduced literally; Typst's own `•` is not. `a:buNone` correctly beats an inherited bullet. |
| Alignment, indents, spacing | ✅ | — | `a:lnSpc` percentages become leading relative to the font size; 100% is left to Typst's own default rather than replaced with an approximation of it. |
| Hyperlinks | ✅ | 7.3% | External by URL; same-deck jumps resolve to the target slide's index. |
| Slide-number fields | ✅ | — | Become touying's live counter rather than the cached number. |
| Vertical text (`vert`) | — | — | Typst has no vertical writing mode. |

### Shapes and pictures

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Pictures (PNG/JPEG/GIF/WebP) | ✅ | 21.1% | Extracted to `assets/`, deduplicated, and identified by **magic bytes** rather than by the declared extension — real decks ship `image1.png` holding a JPEG. |
| SVG pictures | ✅ | — | The native `asvg:svgBlip` is preferred over the raster fallback beside it. |
| Picture crop / rounded clip | ✅ | — | `a:srcRect` becomes geometry (oversize and clip), since Typst's `image` has no crop parameter. `roundRect`/`ellipse` clips become a radius. |
| Rectangle, ellipse, rounded rect | ✅ | 56.3% | |
| Custom geometry (`a:custGeom`) | ✅ | 4.0% | `#curve`, command for command, scaled from the path's own coordinate space. |
| Lines and connectors | ◐ | 4.7% | Drawn as a line; the *connection* to the shapes at each end has no Typst counterpart. |
| Solid, gradient fills; strokes and dashes | ✅ | 3.8% | Gradient stops are sorted and extended to span 0..1, which Typst requires and PowerPoint does not provide. |
| Groups | ✅ | 5.6% | The child coordinate space (`a:chOff`/`a:chExt`) is composed properly, including nested groups — a group states both where it sits and what space its children are drawn in, and the two are routinely different. |
| Rotation | ✅ | — | `a:xfrm/@rot` about the box centre, which is `#rotate`'s own default origin. |
| Other presets (stars, arrows, callouts) | ◐ | — | ~180 named presets exist and Typst has four primitives; the rest are drawn as their bounding rectangle **and reported by name**. |
| Flipped shapes (`flipH`/`flipV`) | ◐ | — | Drawn unmirrored; Typst has no reflection on a laid-out box. |
| Picture and pattern fills on a shape | ⊘ | 3.4% | A Typst fill takes a paint, not a picture. A pattern's foreground colour stands in. |
| Shape effects (shadow, glow) | — | 8.7% | No Typst shadow on this branch. |
| 3-D, text warp | — | 2.4% | |

### Tables and other content

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Tables (`a:tbl`) | ✅ | 10.0% | Grid, spans, cell fills and vertical alignment. Merged cells are dropped rather than emitted, since a covered cell holds no content and would widen the row. |
| Table row heights | ◐ | — | PowerPoint's height is a *minimum* that grows with content; a Typst track is exactly its stated size, so honouring it would clip. |
| Charts | ⊘ | 30.7% | The data lives in an embedded workbook part; the chart itself is a live object with no Typst counterpart. Reported by name. |
| SmartArt | ⊘ | 14.1% | A diagram *language*, not a shape. |
| Embedded OLE | ⊘ | 7.7% | A foreign application's document cannot be revived. |
| Audio / video | — | 1.9% | No Typst media element. |

## What is explicitly **not** supported

Not "not yet" — each needs a Typst language feature that does not exist, or a
decoder that does not exist in Rust:

- **Animations and transitions** (23.2% / 13.6% of real decks) — Typst output
  is static. There is no timeline to write to.
- **SmartArt** (14.1%) — PowerPoint stores a rendered fallback picture beside
  the diagram; the diagram itself is a layout language.
- **Charts** (30.7%) — a live object over an embedded workbook.
- **Embedded OLE objects** (7.7%), **audio and video** (1.9%), **3-D**,
  **text warp**.
- **EMF/WMF pictures** — streams of GDI drawing commands rather than images,
  with no mature Rust decoder. Refused by name rather than emitted as an
  `image()` that fails to compile.
- **Vertical writing mode**, **shape shadows**, **picture fills**.

Everything in this list is reported at import time. A silent loss is a bug.

## Gates

- `tools/pptx-import-corpus/corpus.py` — import + compile over 542 real
  presentations, plus text-coverage measurement against the deck's own
  `<a:t>` runs.
- `cargo test -p typst-pptx-import` — hand-built packages, one behaviour each.

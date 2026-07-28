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

```sh
typst import deck.pptx out.typ
typst import deck.pptx out.typ --pptx-fidelity=idiomatic
typst import deck.pptx out.typ --report=import-report.json
```

Extracted media is written beside the `.typ`. The command refuses to
overwrite the source, assets, or report; the crate's `import` example remains
available for API development. The shared OOXML reader rejects archives above
128 MiB and independently bounds expanded data, part sizes, entry count, XML
depth, and external-entity declarations.

## Why this is not the Word importer with the nouns changed

> **A `.docx` is a flow; a `.pptx` is a coordinate system.**

Word content is a linear stream that Typst also lays out linearly, so
[`typst-docx-import`](../typst-docx-import)'s job is mostly translation. Every
PowerPoint shape instead carries an absolute `a:off`/`a:ext` in EMU, and there
is no flow to recover. Two consequences shape the whole crate:

**Fidelity is a choice, not a quality.** `Placed` (the default) reproduces
every shape at its authored coordinates with `#place`, and is the mode to use
when the round trip matters. `--idiomatic` promotes the placeholders
PowerPoint itself labelled `title` into touying headings — nicer to edit, and
wrong the moment a designer put a "body" box somewhere the flow would not.

**What "round-trips" actually means, measured rather than claimed.** Running
`.pptx` → Typst → `.pptx` over the corpus:

| | Result |
|---|---|
| Text | median **100%** of words kept, mean 95.5% |
| Tables | **11/11** kept |
| Pictures | **54/58** kept, of those Typst can decode (85 of the 143 are EMF/WMF) |
| Picture and frame **offsets** | median **100%** within 1pt, mean 76% |
| Text-box offsets | **do not** round-trip exactly — see below |

A picture or drawn shape comes back at the offset it went in at, because
`typst-pptx` exports it from a laid-out frame that `#place` put exactly where
PowerPoint had it. A **text box does not**: the exporter derives a text box's
top from the laid-out first baseline (`box_top = baseline − max_font_size`),
and Typst's first baseline sits at a different offset from the box top than
PowerPoint's does. On a 40pt title the box comes back ~14pt higher. The text,
its formatting and its horizontal position are unaffected; it is the vertical
box origin that shifts, and it shifts because two typesetters disagree about
where a line begins rather than because anything was lost.

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
| Slide layouts / masters | ✅ | 100% | Two jobs. **Resolved** for placeholders: geometry from the matching layout/master shape, and text defaults from the master's `p:txStyles` overlaid by the layout placeholder's own `a:lstStyle` — the middle link that carries "the title on this layout is right-aligned and 54pt". **Reproduced** for decoration: a layout's and master's non-placeholder shapes are drawn behind every slide that uses them, which is where a themed deck's logo and graphics live. `@showMasterSp="0"` suppresses the master's half. |
| Theme colours | ✅ | 34.1% | `a:schemeClr` → the master's `p:clrMap` → `theme1.xml`, with `lumMod`/`lumOff`/`shade`/`tint`/`alpha` applied. |
| Speaker notes | ✅ | 13.9% | Emitted as the `<pdfpc-file>` metadata `typst-pptx` reads back, so notes survive the round trip rather than needing a second convention. |
| Footer / date / slide-number placeholders | ✅ | — | Kept as ordinary placed text. They were skipped at first on the theory that the theme would draw them; nothing does, and one corpus deck's entire visible content is a single footer. |
| Hidden slides | ◐ | — | Kept, with a comment saying PowerPoint hid them. Dropping authored content silently is the one thing this crate tries never to do. |
| Slide background | ✅ | — | Slide → layout → master, the same three-level fallback the shapes use. |
| Title / author | ✅ | — | `docProps/core.xml` → touying's `config-info`. |
| Transitions | — | 13.6% | Typst output is static; there is no timeline. |
| Animations (`p:timing`) | — | 23.2% | Same. |

### Text

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Runs, fonts, size, bold, italic | ✅ | 65.7% | A run states a Latin *and* an East-Asian face (`a:latin`, `a:ea`) and PowerPoint picks per glyph; both are emitted as a Typst `font:` list, which falls back per glyph by the same rule. 10.2% of decks state one — reading only the Latin face gives a CJK deck the wrong font. |
| Colour, highlight, tracking, caps | ✅ | — | |
| Super/subscript | ✅ | — | From `@baseline`'s sign. |
| Placeholders (`p:ph`) | ✅ | 36.3% | Matched to the layout by `idx` first and `type` second — the order PowerPoint uses, and the only way to tell two body placeholders apart. |
| Bullets and numbering | ✅ | 8.1% | `a:buChar`/`a:buAutoNum` with the outline level as nesting depth. `a:buNone` correctly beats an inherited bullet. A **symbol-font** bullet is translated to the Unicode character it depicts — `char="q"` in Wingdings is a hollow square, and reproducing the letter renders a tofu box on every bullet of every themed deck. |
| Alignment, indents, spacing | ✅ | — | `a:lnSpc` percentages become leading relative to the font size; 100% is left to Typst's own default rather than replaced with an approximation of it. |
| Vertical anchoring (`a:bodyPr/@anchor`) | ✅ | — | `ctr`/`b` become `#align(horizon/bottom)` inside the box. A PowerPoint text box is usually taller than its text, so ignoring this top-aligns every centred caption in the deck. |
| Body insets (`lIns`/`tIns`/`rIns`/`bIns`) | ✅ | — | Reserved inside the box, as PowerPoint reserves them. |
| Hyperlinks | ✅ | 7.3% | External by URL; same-deck jumps resolve to the target slide's index. |
| Slide-number fields | ✅ | — | Become touying's live counter rather than the cached number. |
| Underline style | ◐ | 0.2% | Typst draws one plain rule, so a double, wavy or heavy underline arrives as a single line, reported. |
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
| Arrowheads (`a:headEnd`/`a:tailEnd`) | ⊘ | 7.5% | A Typst stroke has no arrowhead. Synthesising one would mean drawing a triangle at an end whose direction is not always known here, so the line is drawn plain and the loss is named. |
| Solid, gradient fills; strokes and dashes | ✅ | 3.8% | Gradient stops are sorted and extended to span 0..1, which Typst requires and PowerPoint does not provide. |
| Groups | ✅ | 5.6% | The child coordinate space (`a:chOff`/`a:chExt`) is composed properly, including nested groups — a group states both where it sits and what space its children are drawn in, and the two are routinely different. |
| Rotation | ✅ | — | `a:xfrm/@rot` about the box centre, which is `#rotate`'s own default origin. |
| Common presets (triangle, diamond, hexagon, arrows, chevron, star, plus…) | ✅ | — | Drawn as real polygons rather than bounding rectangles. Their *adjustment guides* are not read, so a chevron whose notch was dragged is drawn in default proportions. |
| Other presets (callouts, banners, brackets…) | ◐ | — | ~180 named presets exist; the ones with no polygon here are drawn as their bounding rectangle **and reported by name**. |
| Flipped shapes (`flipH`/`flipV`) | ✅ | — | A negative `#scale`. (Reported as unsupported in the first draft of this file — Typst mirrors perfectly well.) |
| Picture and pattern fills on a shape | ⊘ | 3.4% | A Typst fill takes a paint, not a picture. A pattern's foreground colour stands in. |
| Shape effects (shadow, glow) | — | 8.7% | No Typst shadow on this branch. |
| 3-D, text warp | — | 2.4% | |

### Tables and other content

| Feature | | In the wild | Notes |
|---|---|---:|---|
| Tables (`a:tbl`) | ✅ | 10.0% | Grid, spans, cell fills, vertical alignment and **per-cell borders** (`a:lnL`/`lnT`/`lnR`/`lnB`), which 49% of corpus tables state directly. The table's own stroke is `none`: PowerPoint draws a table's edges from its cells and its style, so a table stating no borders has none — emitting Typst's default 1pt grid would put lines on the page that nobody drew. Merged cells are dropped rather than emitted, since a covered cell holds no content and would widen the row. |
| Table styles (`a:tableStyleId`) | ⊘ | — | The other 51%, and **not closable from the file**: 414 of the 487 corpus packages that carry `ppt/tableStyles.xml` define no style in it at all — they are 182-byte stubs naming a built-in that lives inside PowerPoint. Resolving them would mean hard-coding Microsoft's built-in style table, which is an application data dump rather than anything the format supplies. Such a table is drawn **without** borders and says so, rather than with a guessed grid. |
| Table row heights | ◐ | — | PowerPoint's height is a *minimum* that grows with content; a Typst track is exactly its stated size, so honouring it would clip. |
| Charts → data table | ◐ | 3.5% | Typst has no chart element and the live data lives in an embedded workbook — but the chart part **caches** every value it last drew (`c:strCache`, `c:numCache`), and that cache is recovered as a `#table` of categories and series. The plot, its axes and its styling are not drawn, and the report says so. Points are read by their `@idx`, since a cache omits empty points entirely and reading positionally would shift every later value against its category. |
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

### The line between "not yet" and "cannot"

Everything a `.pptx` actually *contains* is now read. What remains is not
unwritten code — it is information that is not in the file, or a Typst feature
that does not exist:

- **Not in the file.** A built-in table style's borders live inside PowerPoint,
  not the package (measured: 85% of `tableStyles.xml` parts are empty stubs). An
  EMF/WMF picture is a stream of GDI drawing commands with no Rust decoder. A
  chart's live data is in an embedded workbook — its *cached* values are
  recovered, which is everything the file itself knows.
- **Not in Typst.** Animations and transitions need a timeline; 3-D needs a
  renderer; shape shadows and vertical writing mode need language features.
  SmartArt needs a diagram layout engine.

Two things are genuine approximations rather than absences, and both are stated
where they occur: a preset shape's *adjustment guides* are not read, so a
dragged chevron gets default proportions; and a text box's vertical origin
shifts on a round trip, because two typesetters disagree about where a line
begins.

## Gates

- `tools/pptx-import-corpus/corpus.py` — import + compile over 542 real
  presentations, plus text-coverage measurement against the deck's own
  `<a:t>` runs.
- `cargo test -p typst-pptx-import` — hand-built packages, one behaviour each.

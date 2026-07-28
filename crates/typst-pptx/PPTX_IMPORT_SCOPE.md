# Historical scoping record: PowerPoint importer (`typst-pptx-import`)

> **Historical design document — implementation has shipped.** This was the
> pre-implementation scope and risk analysis. The current importer lives in
> [`../typst-pptx-import`](../typst-pptx-import); use its
> [README](../typst-pptx-import/README.md) for supported features, command-line
> examples, measured round-trip behaviour, and known limitations. The stages
> below are retained because they explain the architecture, not because they
> are an open roadmap.

What a `.pptx` → Typst converter would have to do, what it could not, and what
order to build it in. Written from a **542-presentation corpus** (LibreOffice
`sd/qa`, Apache POI, Tika) rather than from the specification, so the effort
estimates track what real files contain rather than what OOXML permits.

The sibling [`typst-docx-import`](../typst-docx-import) is the model: two IRs,
a tier-1 literal pass, tier-2 idiomatic passes, and an `ImportReport` that
makes every loss auditable. Most of that architecture transfers directly. What
does **not** transfer is the thing that makes PowerPoint hard, and it is worth
stating before the plan:

> **A `.docx` is a flow; a `.pptx` is a coordinate system.**
> Word content is a linear stream that Typst also lays out linearly, so the
> importer's job is mostly *translation*. PowerPoint content is absolutely
> positioned inside a fixed canvas, and every shape carries an `a:off`/`a:ext`
> in EMU. There is no flow to recover — so the importer either preserves the
> coordinates (and emits a wall of `#place`, which no one would want to edit)
> or infers structure from geometry (and is guessing).

## What the corpus actually contains

Measured across 531 presentations that have at least one slide:

| Construct | Presentations | Note |
|---|---:|---|
| slide layouts | **100.0%** | every file |
| slide masters | **100.0%** | every file |
| text body (`p:txBody`) | 65.7% | |
| placeholder (`p:ph`) | 36.3% | inherits from layout/master |
| theme colour (`a:schemeClr`) | 34.1% | needs `theme1.xml` |
| chart / `p:graphicFrame` | 30.7% | tables, charts and SmartArt all arrive this way |
| animation (`p:timing`) | 23.2% | no Typst equivalent |
| picture (`p:pic`) | 21.1% | |
| SmartArt (`dgm`) | 14.1% | a whole diagram language |
| speaker notes | 13.9% | export already writes these; see stage 5 |
| transition (`p:transition`) | 13.6% | no Typst equivalent |
| table (`a:tbl`) | 10.0% | |
| shape effects (`a:effectLst`) | 8.7% | shadows, glow, reflection |
| bullets | 8.1% | |
| embedded OLE | 7.7% | |
| hyperlink | 7.3% | |
| group shape | 5.6% | |
| connector (`p:cxnSp`) | 4.7% | |
| custom geometry | 4.0% | |
| embedded chart part | 3.8% | |
| gradient fill | 3.8% | |
| image fill | 3.4% | |
| text warp | 2.8% | |
| 3-D | 2.4% | |
| audio/video | 1.9% | |
| pattern fill | 0.4% | |
| OMML math | 0.2% | |

The distribution is the plan. Masters and layouts are in **100%** of files, so
they are not a later refinement — nothing renders correctly without them.
Animations and transitions are in a fifth of files and have no counterpart at
all, so they are report-only from day one.

## The three genuinely hard parts

### 1. Master → layout → slide inheritance (100% of files)

A slide's shapes mostly do **not** carry their own formatting. A placeholder
(`p:ph` with a `type` and `idx`) inherits position, size, font, colour and
bullet style from the matching placeholder in its **slide layout**, which in
turn inherits from the **slide master**, which in turn resolves colours
through `theme1.xml`'s colour map. Three levels plus a theme.

This is the analogue of `resolve/styles.rs`, and it is strictly harder:
Word's chain is one-dimensional (`basedOn`) and keyed by style id, while
PowerPoint's is keyed by *placeholder identity* and inherits geometry as well
as formatting. There is no shortcut — 36.3% of presentations use placeholders
directly, and the masters that back them are in all of them.

**This is the first thing to build and the thing most likely to be
underestimated.**

### 2. Geometry → structure (every file)

Every shape has absolute coordinates. Three possible policies:

- **Preserve** — emit `#place(dx:, dy:)` per shape inside a fixed-size page.
  Visually faithful, completely uneditable, and arguably not "Typst source" in
  any useful sense.
- **Infer** — sort shapes by position, recognise the title placeholder, treat
  the body placeholder's paragraphs as a list, and emit ordinary flow content.
  Editable and idiomatic; wrong whenever a designer positioned things freely.
- **Hybrid** (recommended) — infer for *placeholder* shapes, whose semantic
  role PowerPoint states explicitly (`type="title"`, `"body"`, `"ctrTitle"`),
  and preserve coordinates for everything else. The 36.3% that use
  placeholders get clean output; free-form slides stay faithful.

The hybrid is the only one that is honest about which case it is in, and it
maps onto the tier-1/tier-2 split the DOCX importer already has: tier 1
preserves, tier 2 infers.

### 3. There is no Typst "slide" (every file)

Typst has no slide element; presentations are made with community packages
(`polylux`, `touying`), each with its own API. The importer must choose:

- emit `#set page(width:, height:)` + `#pagebreak()` per slide — self-contained
  and package-free, the same call [`typst-docx-import`](../typst-docx-import)
  makes for charts by default; or
- emit a `polylux`/`touying` skeleton — nicer output, but the emitted source no
  longer compiles without that package, and the choice of package is a taste
  the importer has no business making.

**Recommendation: page-per-slide by default, a package behind an opt-in flag**,
exactly mirroring `ChartStyle::Table` vs `ChartStyle::Plot`.

## What cannot come across at all

Not "not yet" — these have no Typst representation and would need a language
feature first:

| Construct | Corpus | Why |
|---|---:|---|
| Animation (`p:timing`) | 23.2% | Typst output is static; there is no timeline |
| Transition (`p:transition`) | 13.6% | likewise |
| Audio / video | 1.9% | no media element |
| 3-D (`a:scene3d`) | 2.4% | no 3-D renderer |
| SmartArt | 14.1% | a diagram *language*; PowerPoint itself stores a rendered fallback, which is what an importer would take |
| Embedded OLE | 7.7% | same as DOCX — the payload cannot be revived, only its preview |

Each should be reported, not silently dropped — the lesson the DOCX side
learned repeatedly.

## Staged plan

Each stage is independently useful and independently gateable.

**Stage 1 — skeleton + text (the walking skeleton).**
OPC reader (already shared via `typst-ooxml-core`), slide enumeration, one page
per slide, `p:sp` → `p:txBody` → paragraphs and runs with direct formatting
only. No inheritance yet. Gate: every corpus presentation imports and compiles.

**Stage 2 — the inheritance chain.** Master → layout → slide placeholder
resolution plus `theme1.xml` colour mapping. This is where the real work is.
Gate: text in placeholder shapes takes its font, size and colour from the
layout, verified against a LibreOffice render.

**Stage 3 — the shape vocabulary.** Pictures, preset and custom geometry,
fills, lines, group shapes, connectors. Most of this inverts `typst-pptx`'s own
exporter, and the DOCX importer's `mappers::dml_shape` already reads the same
DrawingML — but see the effort note below before assuming it can be shared.

**Stage 4 — tables and charts.** `a:tbl` → `#table` and `p:graphicFrame` charts
→ the same data-table-or-plot treatment the DOCX importer gives them. Both
mappers largely transfer.

**Stage 5 — the periphery.** Speaker notes, hyperlinks, and the report entries
for everything in the "cannot" table above.

Notes have a shape already waiting for them: the exporter *writes*
`notesSlide` parts, and it gets the text from the `<pdfpc-file>` metadata
Touying and the pdfpc integration emit. An importer that emits that same
`#metadata(..) <pdfpc-file>` payload round-trips notes for free and stays
compatible with the presentation packages people already use — inventing a
second convention would break both directions at once.

## Effort, honestly

Stages 1 and 5 are small.

**Stage 3 and 4 are *not* mostly reuse — that estimate was wrong, and it is
recorded here because acting on it would have misdirected the work.** The
claim was that `typst-docx-import`'s `mappers::dml_shape` could be lifted into
`typst-ooxml-core` and shared, making the shape stages cheap. Measured against
the actual file: **3 of its 13 functions, 57 of 691 lines — 8% — are liftable
as they stand** (`preset_vertices`, `preset_dash_name`, `emu_pt`). The other
92% is coupled at *both* ends, to the DOCX importer's Word IR
(`wml::model::Dml*`), its Typst IR (`tdoc::Inline`/`Block`), its `LowerCtx`,
its `ImportReport` and its emit helpers.

That is not an accident of style. A mapper's job *is* to join one IR to
another, so a mapper is coupled to two IRs by definition; only the value
mathematics in the middle — EMU conversion, `a:custGeom` segments → curve
commands, gradient stop reparameterisation, dash run lengths relative to line
width — is portable. And PowerPoint's shapes are `p:sp`, not `wps:wsp`, so the
parse half does not transfer either.

So the "lift it into `typst-ooxml-core` first" prerequisite should **not** be
done as described. Two honest options when the time comes:

1. Define a producer-neutral DrawingML *value* type in `typst-ooxml-core` that
   both importers parse into and both lower from. That is a design task with a
   real payoff, not a lift.
2. Let the PPTX importer duplicate the mapping in its first pass and factor
   afterwards, once two real consumers exist and the seam is visible rather
   than guessed.

Option 2 is the safer default: the seam that looks obvious from one side of a
single implementation is exactly the one this estimate got wrong.

**Stage 2 is the whole project.** Master/layout/theme resolution is where a
naive importer produces text in the wrong font, wrong colour and wrong place on
every slide, and it is the part with no DOCX analogue to lean on.

The prerequisite before any of it: a **PPTX corpus gate** matching
`tools/docx-import-corpus/` — import + compile over the 542 presentations, with
text coverage measured against a LibreOffice render. The DOCX side found its
two worst bugs that way, and both were invisible to unit tests.

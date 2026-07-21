# Typst → PPTX support matrix

Coverage of `typst-pptx`, the PowerPoint exporter. There is no importer — see
[`PPTX_IMPORT_SCOPE.md`](PPTX_IMPORT_SCOPE.md) for what one would take.

**Built the other way round from the DOCX matrix, deliberately.** That one was
written from what the code does, so it could only ever list gaps already known
— and it hid a real one for months (`w:tblStyle` was parsed and then never
consumed, affecting 12.6% of documents). This matrix starts from **what real
files contain**: a 542-presentation corpus (LibreOffice `sd/qa`, Apache POI,
Tika), swept for element frequency, then diffed against the exporter.

Starting from the corpus fixed one failure mode and introduced the opposite
one: the first revision marked four features unsupported that the exporter had
already implemented, because the "diff against the exporter" half was done by
reading the fidelity report's variant list instead of the emitted XML. Every
Export cell below has since been checked by exporting a deck and reading the
part back. **Both halves have to be measured** — the corpus tells you what
matters, only the output tells you what you do.

Two columns, because they answer different questions:

- **Export** — what `typst-pptx` does with the equivalent Typst construct.
- **In the wild** — how often the PowerPoint construct appears in the corpus.
  Only meaningful for the import direction, so it is the *import* priority
  list, not a judgement on the exporter.

| Symbol | Meaning |
|---|---|
| ✅ **Native** | Editable PresentationML/DrawingML, no known loss |
| ◐ **Approximate** | Native, but with a stated difference |
| 🖼 **Raster** | Rendered to a picture; visually faithful, not editable |
| ⊘ **Drop** | Recognized, then discarded |
| — | No Typst construct to export |

---

## 1. Deck structure

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| One page → one slide | ✅ | — | The exporter's whole architecture: it consumes the laid-out `PagedDocument`, so a slide is a page at its final geometry. |
| Slide size | ✅ | — | From the page size. |
| Presentation vs document classifier | ✅ | — | Aspect ratio decides; a document-shaped deck warns. |
| Slide layouts / masters | ◐ | **100.0%** | Export emits one master and one layout, carrying the placeholders the slides bind to but no theme formatting — a real deck's layouts are where its design lives. **Every** real presentation has them, which is why they are the first hard problem for an importer. |
| Speaker notes | ✅ | 13.9% | Real `notesSlide` parts under a `notesMaster`. Typst has no notes element, so the exporter reads the `<pdfpc-file>` metadata Touying and the pdfpc integration already emit (`PptxOptions::speaker_notes` overrides it). Entries are one-based; overlays that produce several entries for one physical slide concatenate in source order. |
| Transitions | — | 13.6% | Static output; no timeline. |
| Animations (`p:timing`) | — | 23.2% | Same. |

## 2. Text

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Text runs, fonts, size, weight, style | ✅ | 65.7% | |
| Solid text colour | ✅ | — | |
| **Gradient / tiling text fill** | ◐ | — | `GradientOrTilingTextFillApproximation` — a DrawingML run carries one solid colour, so a representative one stands in. Loses visual fidelity and paint kind; the run stays live and editable. |
| Placeholders (`p:ph`) | ✅ | 36.3% | Title, body and slide-number placeholders. `text.rs`'s `mark_placeholders` infers the roles from the laid-out slide — largest title-eligible cluster wins the title, the dominant box below it the body — and `package.rs` writes matching `p:ph` shapes into the generated layout, so the binding resolves. |
| Bullets / numbering | ✅ | 8.1% | Native `a:buChar`/`a:buAutoNum` with the paragraph's own indent, recovered from the laid-out marker rather than from a list element (there is none left after layout). |
| Hyperlinks | ✅ | 7.3% | |
| Font embedding | ✅ | — | Recorded per family/style with whether the OpenType licence permitted it (`FontFact`). |
| Text under skew / non-uniform scale | 🖼 | — | `UnrepresentableTextTransformRasterFallback` — rendered, with the source text kept alongside as invisible searchable text. |

## 3. Shapes and drawing

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Preset geometry (rect, ellipse, line) | ✅ | 56.3% | `a:prstGeom`. |
| Custom geometry (`#curve`) | ✅ | 4.0% | `a:custGeom`, command for command. |
| Rounded rectangles | ✅ | — | `roundRect` + adjustment guide. |
| Solid fill, stroke, dash | ✅ | — | |
| Linear / radial gradient | ✅ | 3.8% | |
| **Conic gradient, off-centre radial** | 🖼 | — | `UnmappableShapeFillRasterFallback` — DrawingML cannot express them. |
| **Gradient or tiling stroke** | 🖼 | — | `UnmappableShapeStrokeRasterFallback` — `a:ln` carries a solid colour only. |
| **Degenerate path** | 🖼 | — | `UnmappableShapeGeometryRasterFallback` — a point has no `custGeom`. One that also has no stroke draws nothing and is now skipped silently rather than counted as a raster that emits no picture. |
| **Tiling / pattern fill** | ◐ | 0.4% | `TilingFillRasterizedApproximation` — rendered once to a PNG tile and placed as a native `a:tile`. The shape stays editable; the fill is no longer parametric. |
| Group shapes | ✅ | 5.6% | |
| **Group with clip or skew** | 🖼 | — | `UnrepresentableGroupRasterFallback` — the whole subtree becomes one positioned picture. |
| Connectors (`p:cxnSp`) | ◐ | 4.7% | A bare `line()` becomes a real `p:cxnSp` with `flipH`/`flipV`, so it moves and restyles like a PowerPoint connector — but nothing binds its ends to other shapes (`a:stCxn`/`a:endCxn`), which is the part that makes a connector follow what it connects. Typst has no such relationship to carry. |
| Shape effects (shadow, glow) | — | 8.7% | No Typst shadow on this branch. |
| 3-D | — | 2.4% | |
| Text warp | — | 2.8% | |

## 4. Pictures

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Raster images (PNG/JPEG/GIF) | ✅ | 21.1% | Embedded verbatim, deduplicated. |
| SVG | ✅ | — | Native `asvg:svgBlip` + a required PNG fallback, rendered at the picture's *final* on-slide size so a scaled-up logo is not soft. Note the trade this makes when a scaled SVG stops rasterizing: the package gains the SVG source alongside the PNG (one corpus deck, +94 KB) in exchange for a resolution-independent, editable picture. |
| Rounded-corner clip + crop | ✅ | — | Native `roundRect` + `a:srcRect`, including under rotation and scale — a tilted photo card stays an editable picture. The crop ratios are measured before the transform, so they are unaffected by it. |
| **Rotated / scaled placement** | ✅ | — | Native `a:xfrm` + `rot`. DrawingML spins a box about its own centre and Typst rotates about the item origin, so the two agree on the centre — the exporter places the box there and lets `rot` do the rest. Verified against LibreOffice: centroid agreement within 0.6 px at 72 dpi for rotation, scale, and both together. Across the corpus's 120 presentations, 113 exports are byte-identical to before and the 6 that changed each gained a native picture where a rendered one used to be. |
| **Skewed / reflected placement** | 🖼 | — | `RotatedOrScaledImageRasterFallback`, now confined to what really has no `a:xfrm` form. |
| Image (blip) fill on a shape | — | 3.4% | |

## 5. Tables

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Native table (`a:tbl`) | ✅ | 10.0% | Real editable table, not a picture. |
| Cell fills, borders, spans | ✅ | — | |
| **Transformed table** | 🖼 | — | `TransformedTableRasterFallback` — if the region's transform is not an axis-aligned similarity, the whole table rasterizes cell by cell rather than mixing native cells with pictures. |

## 6. Maths and other content

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| **Equations** | ◐ native+fallback | 0.2% | `MathOmmlWithTextFallback` — native OMML behind `mc:AlternateContent`, with a plain-text branch for consumers without the extension (older Office, LibreOffice Impress). |
| Equation with no recoverable source | ⊘ | — | `MathSourceUnavailableDrop`. |
| Charts | — | 30.7% | Typst has no chart element; the exporter never emits `chartSpace`. (A *graphicFrame* in the wild is a table, chart or SmartArt.) |
| SmartArt | — | 14.1% | A diagram language, not a shape. |
| Embedded OLE | — | 7.7% | |
| Audio / video | — | 1.9% | |

## 7. Page background

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Solid / linear-gradient page fill | ✅ | — | Native `p:bg`. |
| **Anything else** | ◐ | — | `PageBackgroundWhiteFallback` — a tiling, conic or off-centre radial page fill has no `p:bg` equivalent, so the slide gets plain **white**. This is the one fallback that is silently *wrong* rather than merely lossy, which is exactly why it needed reporting. |

---

## The fidelity report

Every row above that names a `DecisionReason` records itself in a
`FidelityReport` mirroring `typst-docx`'s: a `Representation` (Native /
NativeWithFallback / Approximate / Raster / Drop), a `LossSet` of six
independent dimensions, and a `DecisionReason` naming the specific fallback.
Thirteen reasons, each derived from an actual fallback site in this crate
rather than copied from the DOCX list. The two ◐ rows that name no reason —
layouts and connectors — are structural gaps rather than per-region decisions,
and a report is the wrong place to look for them. **That is the report's
built-in blind spot, and the reason this document exists separately from it:**
a fidelity report can only enumerate what goes wrong inside a region it
emitted.

One deliberate difference in the identity unit. DOCX walks Typst's *realized
content tree* and keys each decision to a source `Location`. This exporter
walks an already laid-out `PagedDocument`, where no per-node identity survives
layout — so a decision's source is the **slide index**, and repeats of one
reason on one slide aggregate into a single row with a count.

Real output, from a deck built to trigger several at once:

```
counts: native: 0, native_with_fallback: 1, approximate: 4, raster: 1, drop: 0
slide=0 NativeWithFallback  MathOmmlWithTextFallback
slide=0 Approximate         GradientOrTilingTextFillApproximation
slide=0 Approximate         PageBackgroundWhiteFallback
slide=1 Approximate         TilingFillRasterizedApproximation
slide=1 Raster              UnrepresentableGroupRasterFallback
fonts:  Libertinus Serif Regular embedded=true (×3), Bold embedded=true
```

**One reason is currently unreachable, and says so.**
`UnmappableShapeTransformRasterFallback` cannot fire through the frame walk: a
non-similarity transform always arrives on a *group*, which rasterizes as
`UnrepresentableGroupRasterFallback` before the walk ever descends to the
shape, and composing similarities only yields another similarity. Confirmed by
export, not by reading — `#scale(x: 200%, y: 100%, rect(..))` logs
`kind=group reason=transform`, never `kind=shape`. The guard stays because
`shape_to_geom` is callable on its own; a deck that reports it means the group
walk changed.

## Where the export side should go next

The three recommendations this document originally carried are all resolved:
native picture rotation and the split shape reasons are implemented above, and
the third — "emit placeholders" — was simply **wrong**. It was written from
the report's variant list rather than from the exporter, and the exporter had
been binding title, body and slide-number placeholders all along. Corrected in
the table above; the lesson is the one the DOCX matrix taught in reverse, that
a document derived from one artefact inherits exactly that artefact's blind
spots.

That correction did not stop at one row. Re-checking **every** ⊘ and — in the
table against the code found three more the same way: placeholders, bullets
(`a:buChar` has been emitted all along) and speaker notes (real `notesSlide`
parts, read from the `<pdfpc-file>` metadata Touying already produces) were all
listed as unsupported while working, and connectors were listed as absent when
a `line()` does become a `p:cxnSp`. Each was verified by exporting a deck and
reading the XML back, which is the only check that would have caught them.

What is actually left, ranked:

1. **Layouts that carry design, not just placeholders.** The generated layout
   exists to make the placeholder binding resolve; it holds no theme
   formatting, so a deck opened in PowerPoint and switched to another theme
   keeps every literal colour and size the exporter baked into each slide.
   This is the honest version of the old recommendation 3.
2. **Connector attachment** (`a:stCxn`/`a:endCxn`). The shape is already
   emitted; what is missing is the relationship to the shapes at its ends —
   and Typst has nothing to read it from, so this needs a source-level answer
   first.
3. **Shape effects** (8.7% of real decks): `a:effectLst` is written empty.
   Drop shadows exist on another branch of this fork, so the exporter side is
   ready before the language side is.

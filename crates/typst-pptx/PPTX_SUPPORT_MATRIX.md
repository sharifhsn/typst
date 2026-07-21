# Typst → PPTX support matrix

Coverage of `typst-pptx`, the PowerPoint exporter. There is no importer — see
[`PPTX_IMPORT_SCOPE.md`](PPTX_IMPORT_SCOPE.md) for what one would take.

**Built the other way round from the DOCX matrix, deliberately.** That one was
written from what the code does, so it could only ever list gaps already known
— and it hid a real one for months (`w:tblStyle` was parsed and then never
consumed, affecting 12.6% of documents). This matrix starts from **what real
files contain**: a 542-presentation corpus (LibreOffice `sd/qa`, Apache POI,
Tika), swept for element frequency, then diffed against the exporter.

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
| Slide layouts / masters | ⊘ | **100.0%** | Export emits a minimal master+layout because the format requires them, not to carry formatting. **Every** real presentation has them, which is why they are the first hard problem for an importer. |
| Speaker notes | — | 13.9% | Typst has no notes construct. |
| Transitions | — | 13.6% | Static output; no timeline. |
| Animations (`p:timing`) | — | 23.2% | Same. |

## 2. Text

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Text runs, fonts, size, weight, style | ✅ | 65.7% | |
| Solid text colour | ✅ | — | |
| **Gradient / tiling text fill** | ◐ | — | `GradientOrTilingTextFillApproximation` — a DrawingML run carries one solid colour, so a representative one stands in. Loses visual fidelity and paint kind; the run stays live and editable. |
| Placeholders (`p:ph`) | ⊘ | 36.3% | Export writes free-floating shapes, never placeholder-bound text. |
| Bullets / numbering | ⊘ | 8.1% | |
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
| **Conic gradient, off-centre radial** | 🖼 | — | `UnmappableShapeRasterFallback` — DrawingML cannot express them. |
| **Tiling / pattern fill** | ◐ | 0.4% | `TilingFillRasterizedApproximation` — rendered once to a PNG tile and placed as a native `a:tile`. The shape stays editable; the fill is no longer parametric. |
| Group shapes | ✅ | 5.6% | |
| **Group with clip or skew** | 🖼 | — | `UnrepresentableGroupRasterFallback` — the whole subtree becomes one positioned picture. |
| Connectors (`p:cxnSp`) | — | 4.7% | Typst has no connector element. |
| Shape effects (shadow, glow) | — | 8.7% | No Typst shadow on this branch. |
| 3-D | — | 2.4% | |
| Text warp | — | 2.8% | |

## 4. Pictures

| Feature | Export | In the wild | Notes |
|---|---|---:|---|
| Raster images (PNG/JPEG/GIF) | ✅ | 21.1% | Embedded verbatim, deduplicated. |
| SVG | ✅ | — | Native `asvg:svgBlip` + a required PNG fallback. |
| Rounded-corner clip + crop | ✅ | — | Native `roundRect` + `a:srcRect`. |
| **Rotated / scaled placement** | 🖼 | — | `RotatedOrScaledImageRasterFallback`. PowerPoint *does* have a native `a:xfrm rot`; this exporter simply never sets one, so any non-translation re-renders. **The clearest cheap win on the export side.** |
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

Every row marked ◐, 🖼 or ⊘ above records itself in a `FidelityReport`,
mirroring `typst-docx`'s: a `Representation` (Native / NativeWithFallback /
Approximate / Raster / Drop), a `LossSet` of six independent dimensions, and a
`DecisionReason` naming the specific fallback. Ten reasons, each derived from
an actual fallback site in this crate rather than copied from the DOCX list.

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

**Known limit, disclosed rather than hidden:** `UnmappableShapeRasterFallback`
covers three distinct causes — unmappable geometry, unmappable transform, and
unmappable fill — because `shape_to_geom` collapses them into one `None`
return. Separating them needs that function to return a typed error, which is
a refactor rather than a report change.

## Where the export side should go next

Ranked by value, from the table above:

1. **Native picture rotation.** `RotatedOrScaledImageRasterFallback` fires for
   any non-translation, but `a:xfrm` has a `rot` attribute the exporter never
   sets. Pure gain: rotated images stop being pictures.
2. **Split `UnmappableShapeRasterFallback`** into its three causes, so the
   report says *which* thing was unmappable.
3. **Placeholder-aware output.** Emitting title/body placeholders instead of
   free-floating shapes would make exported decks behave like real ones under
   PowerPoint's own layout and theme switching — 36.3% of real presentations
   rely on them.

# DOCX export — coverage ledger

A map of what has been **deliberately handled** vs **deliberately skipped** (and
*why*), so that a new finding can be quickly classified as a genuine gap or as
already-covered noise. Validated against the 627-document `typst-corpus`
(`bench/docx_batch.py` for validity, `bench/docx_oracle.py` for content
correctness). Steady state: **616/627 export, 0 invalid XML.**

The three feature tiers (Native / Rasterized / Unsupported) and the per-feature
mapping live in the [README](README.md); this file is the *decision record* —
the rationale behind each disposition and the catalogue of noise.

## 1. Rasterization causes — disposition of every element that reaches `ctx.rasterize`

`DOCX_DEBUG_RASTER=1` logs the element behind each rasterization. Corpus-wide,
ranked, each cause is one of:

| Disposition | Elements | Why |
|---|---|---|
| **Recovered → native** | `stack`, `columns`, `box` (plain), `layout`, `grid`, `table`, `list`, `enum`, `terms`, `quote` | Pure layout/flow containers whose body is real content. Lowered natively (see §4). |
| **Legitimately visual** (raster is correct) | `image` (SVG/PDF/WebP), `curve`, `line`, `polygon`, `rect`/`square`/`circle`/`ellipse` (gradient/complex fill), `move`/`rotate`/`scale`/`skew` (transforms), `block` (opaque `BlockBody::Single/MultiLayouter` Rust callbacks — images/shapes/lines), `figure` (drawing bodies), `repeat`, `divider`, `attach` | No OOXML construct can carry the visual; rasterizing preserves it exactly. Labels/refs inside are still harvested (`deferred_tags`). |
| **Text-inside-visual** (belongs in the raster) | `text`, `heading`, `par`, `sequence`, `styled`, `context`, `caption`, `title`, `pad`, `align` when they are the *body of a transform/place/visual container* | The text is positioned by the visual it lives in; extracting it would orphan it from its diagram. |

Solid-fill rectangles/shapes already map to **native** `wps:wsp` DrawingML (not
rasterized); only gradient/tiling/complex fills rasterize.

**Verdict: the recoverable vein is exhausted.** A new "element X rasterizes"
report is only interesting if X is a flow container (not in the visual list) that
loses *text* — measure with the alphabetic-word oracle A/B before acting.

## 2. Real-Word-document survey — attributes matched vs the noise floor

Method: download N real official `.docx` (government / university / IEEE / NIH),
diff their parts + `settings.xml` elements + style IDs + `document.xml` features
against ours by frequency, relax the threshold until only noise remains. Done at
40% (8 docs), 30% (63 docs), and <30% (24 docs).

### Matched (shipped)
Standard parts (`theme1.xml`, `fontTable.xml`, `webSettings.xml`, `endnotes.xml`
stub, per-part `.rels`), full `latentStyles` (370 entries), the implicit default
styles (`DefaultParagraphFont`, `TableNormal`, `NoList`), `Normal`, heading
styles **with linked `HeadingNChar`**, `Header`/`Footer`(+`Char`),
`ListParagraph`, `Caption`, `Quote`, `Hyperlink`/`FollowedHyperlink`,
`PageNumber`, `FootnoteText`/`FootnoteReference`, `TOC1`–`9`, `Bibliography`,
the gallery styles `Title`/`Subtitle`(+`Char`)/`Strong`/`Emphasis`/`TableGrid`;
rich `settings.xml` (compat-15 block, `rsids`, `w14:docId`, `m:mathPr`,
`clrSchemeMapping`, `themeFontLang`, `decimalSymbol`, `shapeDefaults`,
`hdrShapeDefaults`, …); `w14:paraId`/`textId` on every content paragraph;
enriched `app.xml`/`core.xml`.

### Noise floor (deliberately NOT emitted — a new survey hit here is noise)
| Category | Examples | Why skipped |
|---|---|---|
| **Comment styles** | `BalloonText`, `CommentText`, `CommentReference`, `CommentSubject` (+`Char`) | We emit no comments; the styles would be dead. |
| **SharePoint / property binding** | `customXml/item*.xml`, `docProps/custom.xml`, `docVars` | Bind to content controls / server properties we don't have. |
| **`Normal` aliases** | `BodyText*`, `NormalWeb`, `NoSpacing`, `PlainText`, `Default` | Re-skins of `Normal`; we render with direct formatting + `docDefaults`. |
| **Feature-specific built-ins** | `EnvelopeAddress/Return`, `MacroText`, `HTMLPreformatted`, `MessageHeader`, `IndexHeading`, `TOAHeading`, `DocumentMap`, `Index*` | For Word features (mail-merge, web, index, table-of-authorities) with no Typst source. |
| **Multi-level list styles** | `ListBullet2`–`5`, `ListNumber2`–`5`, `ListContinue*`, `List2`–`5` | We drive lists from `numbering.xml` directly. |
| **Template-author custom styles** | `Subhead-Lvl*-Thesis`, `OMBInfo`, `DataField11pt-Single`, `bodytype`, `Style1`, `highlight1`, `apple-converted-space` | One-off styles a specific template defined; not a Word baseline. |
| **Editor-preference settings** | `stylePaneFormatFilter`, `drawingGrid*`, `doNotHyphenateCaps`, `embedSystemFonts`, `evenAndOddHeaders`, `useFELayout`, `stylePaneSortMethod` | UI/East-Asian-layout preferences with no document-fidelity impact. |
| **Test-doc artifacts** | `Heading3`–`9`(+`Char`), header/footer/notes `.rels`, `fldSimple`, `drawing` | We emit these *on demand* (deeper headings, parts with relationships, …); they only look "missing" against a shallow sample. |

Word's other semantic gallery character styles (`SubtleEmphasis`,
`IntenseEmphasis`, `SubtleReference`, `IntenseReference`, `IntenseQuote`,
`BookTitle`) and `NoSpacing`/`TOCHeading` appear in 8–25% of real docs and are
define-only cosmetics; left out as not worth the styles.xml weight (revisit only
if a consumer expects the full gallery).

## 3. Known export failures (the ~2% that error)

All compile to **PDF**; every error originates in **template/package code**, not
in OOXML generation. They assume the paged layout model the flowing target lacks:

| Class | Docs | Root |
|---|---|---|
| Margin notes (`marginalia`/`drafting` panic) | toffee-tufte, parcio-thesis, sos-ugent-style | package `panic!` in a non-paged model |
| Page-number read | shuosc-shu-bachelor-thesis, splines-thesis-starter | `loc.page-numbering()` / page query → `none` |
| Layout-time introspection assumption | versatile-apa, easy-hgb-thesis, gb-ctr | `query(..).first()/.last()/.at(n)` empty/oob before pagination |
| Show-rule-count assertion | ijimai | `assert(used == 1)` over the laid-out doc |
| Cross-ref to a parent-scope float label | wenyuan-campaign | label not in the introspector |
| User `target`-conditional code | tracl | template has no `docx` branch |

Not exporter bugs; the fix belongs in the template (a default-valued access or a
`target` branch). See the README "Templates that assume a paged model".

## 4. This-session content recovery (rasterize → native)

Validated with the oracle A/B on an **alphabetic-only** word count (the
whitespace count is fooled by pandoc `[]` image-placeholder tokens). Cumulative:
**206 docs gained +35,390 real words, 2 docs −3, 0 invalid.**

| Fix | Mechanism | Gain |
|---|---|---|
| `#stack` | vertical → children in order; horizontal → borderless `w:tbl` row | +13.3k / 82 docs |
| `#columns` | flow body as blocks (single-column approximation) | +27k / 46 docs |
| plain `#box` | `box_is_plain` + `body_inline_extractable` → extract runs (block-bodied boxes still raster, keeping figure `SEQ`) | +2.9k / 71 docs |
| `#layout` | invoke the closure with the page content size, lower the result | +3.9k / 58 docs |
| `wrap-content` figure | a frameless `box(grid(figure, text))` lowers the grid-of-figure natively (narrow guard `body_is_wrap_figure` — a designed full-page box still rasterizes) | +215 / 3 docs (office/may "Table 1" caption recovered) |

## 5. Where genuinely-new findings can still come from

The survey and the rasterize-recovery axes are exhausted (everything above).
Remaining signal is **content correctness**, found with `docx_oracle.py pdf`
(refs/cites/nums vs the PDF gold — *very* noisy: the bulk of its flags are
pdftotext artifacts — page-model `page N` refs, CJK mis-reads, multi-column
extraction order, captions inside placeholder-figure rasters. Read the
per-token detail, then **confirm against the docx directly** before trusting a
flag; the cross-reference machinery itself was spot-checked and resolves
correctly). What this surfaced:

- **wrap-content figures** — found + fixed (§4 above).
- cross-reference / caption **numbers** in recovered native content — verified
  correct on the docs checked (preprintx, mousse-notes; flags were noise).
- OMML math fidelity on uncommon constructs, and table edge cases (merges,
  nesting, alignment) in recovered `#grid`/`#stack` — not yet exhaustively
  swept; the next place to look if more is wanted.

**Lesson (load-bearing):** lowering a frameless box's body wholesale regressed a
designed full-page layout box by −148 words (content vanished in the native
re-walk). Any "lower this container natively" change must be guarded to a
specific structural signature and validated with the alphabetic-word oracle A/B
(0 real loss) before shipping — the whitespace word count is fooled by pandoc
`[]` placeholders.

## 6. Official-Word-feature audit (against Word's own ribbon tabs)

Method: walk Word's own tab structure (Home / Insert / Design / Layout /
References / Review / Developer — the taxonomy Microsoft itself organizes the
product around) and check each feature against a Typst source concept, not
just against real-document samples (§2's axis). Two categories of outcome:

### Genuine gaps found and fixed this pass
| Word feature | Typst source | Fix |
|---|---|---|
| Design ▸ Page Color | `set page(fill: solid-color)` | native `w:background` (§ README "Page layout") |
| (no direct ribbon home — an editing default) | `#set text(hyphenate: ..)` / `auto` following justification | `w:autoHyphenation`, Word's own schema position |

Both were silent losses: Typst carried the exact signal, the exporter simply
never read it. Neither needed a design decision — once found, they were a
single style-chain read each (see the `dfa2f2da3` commit for the false start:
the first attempt read the pre-realize root `styles`, which never sees the
body's own `#set` rules — fixed by reading from the *realized* section's style
chain, the same one `PageElem::fill`/`background` already correctly use).

### Confirmed already covered (audit ruled these out, not gaps)
- **Alt text** (`wp:docPr descr`) — already wired from `image(alt: ..)`.
- **Table of Figures** (References ▸ Captions) — already supported via
  `#outline(target: figure)` → a `\c "Figure"` TOC field (`mappers/outline.rs`).
- **Watermark** (Design ▸ Page Background) — already covered by the general
  `set page(background:)` → `behindDoc` header-image mechanism (a rotated,
  semi-transparent watermark is just a rasterized background image, same as
  any other page background).

### Out of scope — no Typst source concept (not exporter gaps)
Word features that have **no corresponding Typst language construct**, so
there is nothing for the exporter to read regardless of effort:
- **Mailings** (mail merge, envelopes, labels) — a Typst compile always
  produces one fixed document; there is no data-source/merge-field pipeline.
- **Review** (Track Changes, Comments, Compare) — no revision/annotation
  concept in Typst source; a compile is a single final render, not a diff.
- **Developer** (content controls — checkbox/dropdown/date-picker/rich-text,
  building blocks, macros) — these exist for round-trip *editable* Word forms;
  a one-shot compile has no interactive-field concept to populate them from.
- **SmartArt, WordArt (text-on-a-path/distorted text), embedded Excel Charts,
  3D Models, Icons gallery** — no Typst diagram/chart/warped-text DSL to draw
  from. (A chart-like visual built from Typst shapes/CeTZ still works via the
  existing rasterize path; there's no *native* OOXML chart object to target.)
- **Drop caps** — Word's `w:framePr dropCap` has no Typst-native trigger (no
  built-in `dropcap` element); a user who fakes one with a large first letter +
  float already gets the correct visual via existing float/text handling.
- **Restrict Editing / document protection, Accessibility Checker itself** —
  the Checker is an *analysis tool* over content we already emit correctly
  (headings, alt text, table structure); protection flags have no source
  trigger. Track Changes/Compare/macros: see above.

## 7. Ambitious forward design: what's left on the table for shapes

This session mapped the *individual-shape* primitives (curve, line, gradient
fill, dash/cap — §4 and the shape commits), then §7.1 (grouped shapes) was
implemented and shipped in a follow-up pass. The remaining items (§7.2-§7.4)
are recorded so a future pass starts from the analysis instead of redoing it.

### 7.1 Grouped shapes (`wpg:wgp`) — IMPLEMENTED
**The problem:** a hand-drawn diagram built from several `#move`d primitives
(rects, lines, circles) — the common way to sketch a simple diagram *without*
a package like CeTZ — rasterized as one flat image, because each shape only
reached the native-shape dispatch when *its own* content was the thing being
lowered; a composition of several shapes inside one `#move` container was
captured whole by `ctx.rasterize` before any individual piece was inspected.

**The fix:** `#move(dx:, dy:)[body]` now walks its laid-out body
(`mappers/shape.rs::extract_shapes`/`collect_shapes`) recursively through
plain-translated `FrameItem::Group` nesting, collecting every
`FrameItem::Shape` whose fill/stroke/geometry are natively representable and
bailing to the old whole-container rasterize the moment it finds anything else
(text, an image, a rotate/scale/skew, a clip). One matched shape lowers to the
same single `wps:wsp` drawing `#curve`/`#line` already produce; several lower
to one `wpg:wgp` group — a single anchored drawing whose `wpg:grpSpPr`
declares the shared local coordinate space (`a:off`/`a:ext` +
`a:chOff`/`a:chExt`, the union of every child's bbox) and each child is a
`wps:wsp` positioned by its own `a:xfrm` within it (`encode.rs::write_wsp`,
factored out of the single-shape path so both share one code path down to the
XML).

**The bug that blocked the first attempt:** laying the body out via a bare
`typst_layout::layout_frame` call (ambient `Docx`-target styles) dropped every
shape silently — Word's shape elements (`LineElem`, `CurveElem`, `RectElem`, …)
only become a `BlockElem` layouter via a show rule
(`LINE_RULE`/`CURVE_RULE`/… in `typst-layout/src/rules.rs`) registered for
`Target::Paged`, *not* `Target::Docx` (DOCX intercepts them natively before
that show rule would ever fire, in the normal top-level dispatch). Re-laying
out raw content under the ambient `Docx` target left those show rules
unregistered, so flow collection (`typst-layout/src/flow/collect.rs`) hit its
"anything else" fallback and warned+dropped each bare shape
("`line` was ignored during paged export"), producing an empty frame. Fixed by
routing through the existing `ctx.layout_export_frame` helper (already used by
the rasterize fallback) instead, which chains in `Target::Paged` before laying
out — the same mechanism that makes bare shapes drawable at all.

**Validated:** corpus-wide, `RASTERIZE: move` occurrences dropped from 2597 to
1587 (-39%) across 627 docs (0 new EXPORT_ERR/INVALID; still the same 11
pre-existing template failures); `presentation/black-angular-frame` alone went
1319->394. Oracle A/B (baseline vs. this change) shows 0 regressions on any
signal. Synthetic multi-shape `#move` cases render pixel-identical between the
gold Typst PDF and the DOCX round-tripped through LibreOffice.

**What's still out of scope (bails to rasterize, same as before):**
- a rotate/scale/skew or a clip anywhere in the `#move`d body (the
  `FrameItem::Group` transform-identity check bails) — grouped shapes *with* a
  transform is the natural next tier (§7.1 follow-up: a `wpg:wgp` child can
  itself carry a rotation via `a:xfrm rot=`, but per-child skew/scale has no
  OOXML equivalent and would need to fall back per-shape);
- axis-aligned `Geometry::Rect` inside a composition (`geometry_to_raw`
  returns `None` for it) — it already maps natively as a *sole* top-level
  shape via the existing rect path, just not yet composed with siblings here;
- any non-shape leaf (text, image) anywhere in the body, which still
  rasterizes the *whole* `#move` — a mixed shape+caption composition doesn't
  partially recover.

### 7.2 Radial gradient (scoped out in §4, see the `6bcb673cf` commit)
OOXML's radial gradient is expressed as an inset (`a:fillToRect`) into the
*shape's own bounding box* — an ellipse whose size is implied by how far the
insets pull in from each edge — not a free-form center + radius the way
Typst's `gradient.radial(center:, radius:)` is. Mapping the common case
(default center, no focal point) needs the inset-to-radius conversion formula
derived and checked against real Word rendering before trusting it; mapping
the general case (off-center, two-circle focal gradients) may not be possible
at all in the OOXML model. Left rasterizing; a future pass should scope to
just the "centered, no focal point" case and verify the inset formula
empirically rather than derive it from the spec alone.

### 7.3 Preset-shape recognition (low priority)
Word ships ~187 preset geometries (`a:prstGeom`'s `prst` enum: arrows, stars,
callouts, flowchart symbols, …). Typst has no source vocabulary for most of
these (no built-in arrow/star/callout element) — a user who wants one already
draws it as a `#polygon`, which we already map to a native (if `custGeom`,
not preset) shape. Detecting "this polygon happens to be axis-aligned and
shaped like a 5-point star" to swap in `prst="star5"` (getting Word's
resizable handle instead of a fixed path) is a nice-to-have with no missing
functionality behind it — not pursued.

### 7.4 Connectors (`cxnSp`), 3D bevels, WordArt — no Typst source signal
`a:ln headEnd`/`tailEnd` (arrowheads) has no Typst stroke field to read from
(Typst strokes have no arrow-mark concept). 3D bevels/shape shadows
(`a:sp3d`/`a:effectLst`) have no Typst *shape*-level source either — the
user's own box-shadow work (a separate branch, `box-shadow`) adds a shadow to
`#box`/`#block`, not to the vector shapes this session covers; wiring that
in, when the branches meet, is a clean, well-scoped follow-up (a solid-color
shadow → native `a:outerShdw`, keeping the rasterize path for anything
`a:effectLst` can't express). WordArt (distorted text on a path) has no Typst
source concept at all (Typst text is never warped to a path) — not pursued.

# DOCX export — coverage ledger

A map of what has been **deliberately handled** vs **deliberately skipped** (and
*why*), so that a new finding can be quickly classified as a genuine gap or as
already-covered noise. The current, checked-in package and semantic validator
lives in [`../../tools/docx-validate/`](../../tools/docx-validate/); its
workflow, report format, smoke corpus, and explicit non-goals are documented in
[`../../docs/dev/docx-validation.md`](../../docs/dev/docx-validation.md).

The older 627-document `typst-corpus` campaign used local
`bench/docx_batch.py` and `bench/docx_oracle.py` scripts that are **not present
in this repository**. Its final recorded snapshot was **616/627 exports and 0
invalid XML packages**. Keep that number as dated regression evidence, not as a
reproducible current release result or a public compatibility percentage.

The four feature tiers (Native / Approximate / Rasterized / Unsupported) and
the per-feature mapping live in the [README](README.md); this file is the
*decision record* —
the rationale behind each disposition and the catalogue of noise.

> **Historical ledger.** Corpus counts and dispositions below describe the
> exporter snapshot in which each audit was run. They are valuable evidence,
> but not a substitute for the current cross-export issue register and design in
> [`../../docs/dev/office-export-architecture.md`](../../docs/dev/office-export-architecture.md),
> or for a fresh run of the checked-in validator above.

## Current rearchitecture foundation

The current branch now carries a structured `FidelityReport` on every
`DocxDocument`. Whole-region raster decisions, native SVG plus PNG compatibility
fallbacks, approximated positional links, dropped content, and suppressed
fallback/layout diagnostics are queryable without parsing warning strings.
Repeated page-furniture decisions aggregate by source identity.

Equation lowering is the first real capability-planning migration: the resolved
math IR is preflighted recursively before OMML emission. Any `MathKind::Box` or
`MathKind::External` descendant selects one whole-equation raster fallback, with
recovered searchable text, so the native emitter can no longer silently omit a
child from otherwise-valid OMML.

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
| **SharePoint / property binding** | `w:dataBinding`, SharePoint property parts, `docVars` | Bind to content controls / server properties we don't have. Custom XML is used for bibliography/fidelity data, and one custom property is used as a Writer-stable fidelity carrier. |
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

## 3. Historical export failures (the old ~2% tail)

This table records the corpus tail that motivated the paged-introspection work
in §§10-11. The page-number, layout-time introspection, and citation/location
classes are now handled by the real paged introspector plus the synthetic
fallback. Current failures should be re-profiled from a fresh corpus run rather
than assumed to match this historical list.

| Class | Docs | Original root |
|---|---|---|
| Margin notes (`marginalia`/`drafting` panic) | toffee-tufte, parcio-thesis, sos-ugent-style | package `panic!` in a non-paged model |
| Page-number read | shuosc-shu-bachelor-thesis, splines-thesis-starter | `loc.page-numbering()` / page query → `none` |
| Layout-time introspection assumption | versatile-apa, easy-hgb-thesis, gb-ctr | `query(..).first()/.last()/.at(n)` empty/oob before pagination |
| Show-rule-count assertion | ijimai | `assert(used == 1)` over the laid-out doc |
| Cross-ref to a parent-scope float label | wenyuan-campaign | label not in the introspector |
| User `target`-conditional code | tracl | template has no `docx` branch |

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

### 7.1a Follow-up pass: rect, rotate/scale, and text-only moves — IMPLEMENTED
Three extensions landed in the same follow-up pass, closing most of §7.1's
"what's still out of scope" list:

**Rect in a composition.** `geometry_to_raw` lowered `Geometry::Curve`/`Line`
to a raw path but bailed (`None`) on `Geometry::Rect`, so a rect mixed with
lines/curves in one `#move` still rasterized the whole thing even though a
*sole* rect already mapped natively via the preset-geometry path. Fixed by
also lowering `Rect` to its 4-corner closed path (`Move`/3×`Line`/`Close`) —
it composes exactly like any other shape once flattened to raw segments.

**Rotate/scale inside a composition.** Originally `collect_shapes` bailed on
any non-identity `FrameItem::Group` transform. Generalized: since an OOXML
`a:custGeom` path is just a flat point list with no inherent orientation,
baking the group's FULL accumulated transform directly into each point
(`Point::transform`, composed via `pre_concat` down the recursion — mirroring
exactly how every exporter's own `handle_group` accumulates transforms) covers
rotation and reflection *exactly* (they preserve length, so a stroke's flat
width is unaffected) and uniform scale *exactly* once the stroke width is also
multiplied by the same factor (`similarity_scale`, a same-column-norm +
orthogonal-columns check on the 2x2 linear part). Skew or non-uniform scale —
no exact single-width stroke representation — still bails to rasterize. No
`a:xfrm rot=` needed at all: the rotated/scaled shape's own path coordinates
already encode the final orientation, reusing 100% of the group/bbox/EMU
machinery §7.1 built. The bug that blocked §7.1's first attempt (bare shapes
needing `Target::Paged` to show-rule into a layouter) doesn't recur here since
`layout_export_frame` is unchanged — this pass only touched the *frame-walk*.

**Text-only `#move` via `w:position`.** The dominant *remaining* rasterize
cause turned out not to be transforms but plain text: real templates use
`#move(dy: Npt)[text(...)]` as a baseline/vertical nudge (`#59`'s corpus dig
found this exact pattern powering slide section titles). Word's own
`w:position` (raise/lower a run's glyphs, in half-points, *without* affecting
the paragraph's line height) is precisely `#move`'s "translate visually
without affecting layout" contract for text — so a `#move` whose `dx` is ~0
and whose ENTIRE body is plain inline content (`mappers/shape.rs::
is_pure_text_body` — a conservative `Content::traverse` bailing on any shape,
image, nested transform, grid, table, or figure anywhere inside) now lowers to
real inline runs (`ctx.inline_runs`) carrying a composed `w:position` shift,
instead of rasterizing. Deliberately conservative: a MIXED body (some real
shape alongside text) still bails whole to rasterize, since embedding it this
way would silently drop the shape's own position.

**Validated:** corpus-wide, `RASTERIZE: move` dropped further from 1587 to
1201 (-24% more; -54% from the pre-§7.1 baseline of 2597), docs affected
41 (from 48). 0 new EXPORT_ERR/INVALID, same 11 pre-existing failures. Oracle
A/B flagged exactly 2 docs (`black-angular-frame` words=0.62,
`touying-simpl-swufe` words=0.98) — both manually confirmed as pure
IMPROVEMENTS, not regressions: pandoc renders an alt-text-less rasterized
image as literal `[]` in plain-text extraction, and in both docs the *only*
diff was `[]` -> the correct section title / "Thank You!" text the old
rasterize path had been silently reducing to an untagged image. Verified
visually in LibreOffice (`black-angular-frame`'s "Configuration" section
divider slide renders correctly, bold, inside its bordered box). Rotation and
uniform-scale-with-stroke were also each verified pixel-identical between the
gold Typst PDF and the LibreOffice-rendered DOCX round-trip on synthetic
cases.

**What's still out of scope (bails to rasterize, same as before):**
- skew or non-uniform scale (X/Y scaled by different factors) anywhere in the
  `#move`d body — no exact single-width flat-stroke representation, so it's
  left rasterizing rather than approximating;
- a clip path anywhere in the composition;
- any non-shape, non-text leaf (an image) anywhere in the body, or a MIXED
  shape+text composition — either still rasterizes the *whole* `#move`;
- a horizontal-only or diagonal (`dx` != 0) text nudge — no clean inline OOXML
  analogue the way a pure vertical nudge has in `w:position`.

### 7.1b Bare (not `#move`-wrapped) `#rotate`/`#scale` — IMPLEMENTED
§7.1a's `collect_shapes` generalization (bake a similarity transform into path
coordinates) is useful even without a `#move` in the mix: `#rotate(..)[shape]`
or `#scale(..)[shape]` used directly still rasterized before this, since
nothing laid out the WHOLE rotate/scale element under `Target::Paged` and ran
it through shape extraction. Fixed with one new function
(`mappers::shape::transformed`) that does exactly that — laying out `child`
(the whole element, not just its body) via `layout_export_frame` produces a
frame where the rotation/scale already shows up as an ordinary
`FrameItem::Group`, so it's the same walk `move_` already does, minus the
translate step — and one new `ctx.rs` dispatch arm gating it on
`child.is::<RotateElem>() || child.is::<ScaleElem>()`. Validated: corpus-wide
`RASTERIZE: rotate` 227->218 (18 docs, down from 20); 0 new EXPORT_ERR/
INVALID, 0 oracle regressions; visually verified pixel-identical (gold PDF vs.
LibreOffice-rendered DOCX) on a bare-rotate + bare-scale synthetic case.

### 7.1c `#place` nested in a framed container — IMPLEMENTED
A broader corpus sweep by rasterize-cause (`DOCX_DEBUG_RASTER`, not just
`move`) found `place` as the single largest remaining category (3624
occurrences, 59 docs) — concentrated in templates using the `codetastic`
package (QR codes/barcodes), which draws every module as its own
`#place(dx:, dy:, square(..))` inside a sized `#box`.

**First attempt (reverted): anchored inline drawing.** `#place` at the top
level dispatches to `mappers::image::place`, which anchors a native
shape/image drawing via `<wp:anchor>`. A `#place` nested inside a framed
container instead reaches `handle_inline` with no dispatch arm for a bare
`PlaceElem` at all, falling through to rasterize. The first fix
(`mappers::image::place_inline`, reusing `place`'s anchor logic) compiled and
stopped rasterizing, but LibreOffice rendered the resulting *nested* `wp:anchor`
as entirely invisible — an anchored drawing inside another container's own
paragraph flow is exotic enough that LibreOffice's renderer doesn't handle it
the way a top-level one does. Reverted rather than ship silently-invisible
content.

**The real fix: don't anchor at all — recompute the whole container as one
shape composition.** The insight that unblocked this: `#place`'s own
*non-floating* layout (the common case; `float: true` is separate, unaffected,
opt-in page/column floating) does not produce any special frame wrapper at
all — `typst-layout/src/flow/distribute.rs::finalize` composites a placed
child into its parent frame via an *ordinary* `output.push_frame(pos, frame)`,
identical to how any other block child is placed. From
[`collect_shapes`](§7.1)'s point of view, a `#box` full of `#place`d shapes is
therefore no different from a `#move`d composition — it's just an ordinary
frame with shape items (or plain-translation `FrameItem::Group`s) in it. So
the SAME [`mappers::shape::transformed`](§7.1b) function (lay the whole thing
out under `Target::Paged`, run `extract_shapes`/`build_shapes_drawing`)
recovers it too, as long as it's handed the container's ENTIRE content rather
than one `#place` at a time — sidestepping the anchor problem entirely, since
the result is one ordinary INLINE (non-floating) drawing, not a floating one.

**Where to intercept, and why it took two attempts to find:** a `#box`/`#rect`
with no fill/stroke/clip of its own carries no visual, so realize flattens it
away entirely — its content reaches the document as the direct body of an
(implicit) paragraph, processed by `inline_runs`/`inline_pchildren`, *not*
`convert_children`'s block dispatch (where a naive fix would look first — and
where an earlier attempt in this same pass added a now-mostly-redundant but
still useful defense-in-depth check, for the case where a `#box`/`#rect`
*does* have a fill/stroke and so keeps its block structure through
`handle_block_box`). The actual fix lives in three places, all gated on a
cheap `Content::traverse` pre-filter (`contains_place`, checking for any
`PlaceElem` anywhere inside) so ordinary paragraphs/boxes without `#place`
pay zero extra cost:
- `ctx.rs::inline_runs`/`inline_pchildren` — the chokepoint that actually
  fires for a plain (no-fill) container's flattened body reached as
  paragraph content;
- `convert.rs::convert_children` and `ctx.rs::handle_inline` — for a
  `#box`/`#rect`/`BlockElem` that *does* keep its own block structure (a fill
  or stroke set) and so is walked as a container in its own right.

**Validated:** the synthetic QR-code repro (`codetastic`, 225 modules) renders
pixel-identical between the gold Typst PDF and the LibreOffice-rendered DOCX
round-trip — as one native `wpg:wgp` group of 225 `wps:wsp` squares, fully
vector/editable, not a raster image. Corpus-wide: `RASTERIZE: place` dropped
3624->1900 (-48%), with the `tuhi-*-vuw` template family (postcard/programme/
course-poster, each embedding a `codetastic` QR code) going to 0 `place`
rasterizations. `black-angular-frame`'s `place` count is unchanged (1184) —
correctly so: its nav-bar's `#place` calls wrap `layout(size => ..)` closures
with `measure()` calls, genuinely opaque content, not a plain shape
composition. 0 new EXPORT_ERR/INVALID, 0 oracle regressions, full test suite
green.

### 7.1d Categorized sweep: `#v`, `#pdf.artifact`, and a content-loss bug — IMPLEMENTED
A full `DOCX_DEBUG_RASTER` sweep by element kind (not just `move`/`place`)
found several more gaps, each investigated to a concrete root cause before
fixing (a couple of dead ends included, recorded honestly):

**Bare `#v()` reaching a run-only context.** A plain (no fill/stroke) box
whose body mixes text and `#v()` (a common pattern for a two-line label —
e.g. `box[#text(..)[name] #v(-.7em) #text(..)[title]]`, used 46 docs
corpus-wide, 484 times alone in the `caidan` flyer template's per-item
macro) gets flattened to inline content, but `handle_inline` had no arm for
`VElem` — it silently vanished (not even a warning: `VElem` was already in
`is_invisible_noop`), crushing the surrounding text together with no gap.
Fixed with the same idea `ParbreakElem` already uses one arm up: approximate
with a line break (`Run::Break`) rather than trying to preserve the exact
spacing, which Word has no run-level primitive for anyway. Corpus-wide
`RASTERIZE: v` (a spurious debug artifact of the old silent-drop path) drops
1103->0.

**`#pdf.artifact[..]` had no handler anywhere.** Zero dispatch arms in
either `handle_inline` or `convert_children` — any accessibility-marked
content (a decorative logo, or a code-listing package like `zebraw` marking
its line-number gutter cells as artifacts) rasterized whole. Fixed by
unwrapping it exactly like the existing `PdfMarkerTag` arm (no DOCX artifact
concept, so just lower the body) in both places. This surfaced one more
layer: `zebraw` wraps `pdf.artifact(grid.cell(..))` — a user-authored
`grid.cell` nested inside the artifact — and Typst's own grid resolution
adds ITS OWN uniform outer `GridCell` wrapper regardless, so unwrapping the
artifact reveals a bare inner `GridCell`/`TableCell` with no meaning outside
its parent grid's cell lattice and no dispatch arm of its own. Added one for
each (unwrap to `.body`, lower like any other wrapper) in both dispatchers.
Validated on the `unofficial-ouc-bachelor-thesis` corpus doc: its zebraw code
listings' line numbers, previously silently dropped, now render (LibreOffice-
verified); a full corpus oracle A/B confirms this as a pure recovery (higher
`nums` signal — more numbers correctly present — not a regression), plus two
`touying`-family presentation templates recovered section-title text via the
same `[]`-placeholder pattern documented in §7.1a, and one previously-EXPORT_ERR
doc (`wenyuan-campaign`, a label-resolution convergence failure) now compiles
successfully as an unplanned side effect.

**A genuine content-loss bug, found along the way (not itself about
rasterization):** `mappers::image::place_body_drawing`'s `take_first_drawing`
unconditionally took the *first* native drawing found in a placed body's
lowered blocks and discarded everything else — silently, with no warning. A
`#place`d body richer than a lone image/shape (rare, but real) would lose
all but its first block. Fixed to only trust that shortcut when the body
produced *exactly* one paragraph holding *exactly* one drawing; anything
richer now rasterizes the whole body instead (preserving all the visual
content, even at the cost of vector fidelity, rather than an arbitrary
silent truncation).

**`visualization/dudi-colorful-slides`'s 540 `RASTERIZE: polygon` — RESOLVED
in §7.1f below** (the diagnosis in this section's first pass was incomplete;
the real root cause and fix are documented there). The `take_first_drawing`
fix above was found while investigating it and is a real, independent win
either way.

### 7.1e Percentage-relative `#rect`/`#square`/`#circle`/`#ellipse` sizing — IMPLEMENTED
The single largest remaining cause of shape rasterization, spread broadly
(98 docs) rather than concentrated in an outlier: `explicit_size` (the
decorative-shape size check in `mappers::shape::build`) required a *purely
absolute* width/height, bailing on any percentage component — but
`width: 100%`/`height: 100%` (fill the container) is an extremely common
real-world pattern for a decorative background rect. A gradient-fill
sibling finding from the same investigation turned out to be a false lead:
`fill_color` already routes `Paint::Gradient` through the existing
`linear_gradient_fill` (built in an earlier session for polygons) — verified
directly, a gradient-filled rect with *absolute* sizing already mapped
natively before this fix. The corpus examples flagged as "gradient doesn't
work" all *also* had percentage sizing; the compound failure was
misattributed to the gradient.

**The fix:** resolve `Rel<Length>` against a real reference size instead of
requiring its ratio component to be zero — the same `ctx.raster_width`/
`raster_height` (the page's own content area) that `#move`'s `dx`/`dy` and
the rotate/scale/composition work already resolve percentages against (§7.1,
§7.1a). A pure-absolute size is unaffected (its ratio component is zero, so
`relative_to` returns the same value regardless of reference); this is a
strict superset of the old behavior, not a change to it.

**Validated:** corpus-wide `RASTERIZE: rect` drops 1555->989 (-36%, 25 more
docs fully clear of it, 98->73 remaining). Full test suite green, corpus
batch 617/627 OK (up from 616 — see below), 0 oracle regressions. Visually
verified pixel-accurate width resolution on a synthetic `width: 100%`/
`width: 50%` case and on a real corpus doc (`modern-ipsy-thesis`).

**Unplanned bonus, twice compounded:** this session's `#pdf.artifact`/
`GridCell` fix (§7.1d) and this percentage-size fix together flip
`layout/wenyuan-campaign` from EXPORT_ERR (a label-resolution convergence
failure) to a clean compile — neither fix targeted that document or that
failure mode; it's a downstream effect of more content resolving natively
instead of needing a introspection-losing rasterize fallback partway through
convergence.

### 7.1f `#polygon` decorative patterns and corner wedges — IMPLEMENTED
The single largest concentrated polygon-rasterize case
(`visualization/dudi-colorful-slides`, 540 of the corpus's 658 `RASTERIZE:
polygon`) plus a broad tail across 8 more docs. Two independent root causes,
both fixed:

**(1) `#place(stack(..polygons))` — a decorative full-bleed motif rasterized
per-polygon.** A slide theme draws a geometric background by pushing dozens of
`#polygon`s into a `#stack`, wrapped in a bare top-level `#place`. The path
was: top-level `#place` -> `mappers::image::place` -> `place_body_drawing` ->
`ctx.blocks(stack)` -> the *stack mapper* lowers a horizontal stack to a
borderless **table row, one cell per child** -> each cell's polygon reaches
`shape`/`build`, bails (see (2)), and rasterizes; then `place_body_drawing`
throws the table away and rasterizes the whole stack anyway. So the 540
per-polygon rasters were wasted work discarded into one whole-stack image —
visually a flat raster of the motif. Fixed by giving `place_body_drawing` the
SAME shape-composition path §7.1c gave `#box(place(..))`: try
`mappers::shape::transformed(body)` first (lay the whole placed body out under
`Target::Paged` — which resolves every polygon's percentage-relative
coordinates against the page — and `build_shapes_drawing` groups them). The
result is one `wpg:wgp` of vector polygons wrapped in the existing top-level
`<wp:anchor>` `#place` already produces. Crucially this is a **top-level**
anchor (unlike the nested-anchor case §7.1c had to abandon as
LibreOffice-invisible) — verified: the striped bands and full-page triangle
grids render correctly, as vector, in the LibreOffice round-trip.

**(2) `#polygon` with negative or percentage vertex coordinates bailed in
`build`.** The direct-polygon path read only each vertex's *absolute*
component (`v.x.abs`, ignoring any percentage) and sized the shape by
`max(0, coords)` — so a polygon whose vertices are all non-positive on an axis
(a corner wedge drawn *upward*, `(1em, -0.5em)`, e.g. `mythographer-5e`'s
sidebar edge triangles) hit `max_y <= 0` and rasterized, and a
percentage-coordinate polygon resolved to a degenerate ~0 size. Fixed by
resolving each vertex against the page reference (like §7.1e's rect sizing)
and routing through the shared `RawSeg`/`normalize_segments` path `#curve`/
`#line` already use, which shifts the whole path into the non-negative
`a:custGeom` space and sizes it by its true bounding box. Corner wedges now
render as native vector shapes at the correct positions (LibreOffice-verified
against gold).

**Validated:** corpus-wide `RASTERIZE: polygon` 658->6 (-99%); dudi 540->0,
plus 7 more docs cleared entirely. The 6 residual (`kzn-ma`) are full-bleed
cover decorations whose vertices reference `page.height`/`page.width`/
`page.margin` — page-geometry the isolated re-layout can't resolve (the same
hard class as the margin-note docs), correctly left rasterizing. Full test
suite green, corpus batch 617/627 OK, 0 INVALID, 0 oracle regressions across
the whole corpus.

### 7.1g Floated `#place` content + standalone figure captions — IMPLEMENTED
The single biggest CONTENT-loss pattern in academic papers: a two-column
template's `show figure: it => place(float: true, scope: "parent")[#it.body
#it.caption]` (and the analogous title-block `place(top+center, float: true,
scope: "parent")[title, authors, abstract, keywords]`) had its entire body
rasterized to one flat image by `place_body_drawing` — because the body isn't
a *single* drawing (it's a figure body + caption, or a whole title block), the
old logic fell to `laid_out_fallback` and flattened the lot. In pandoc text
extraction the whole title block came out as a single `[]`.

**Two fixes:**
1. **Float `#place` → flow as blocks.** `place` now, when the body isn't a
   single native/rasterized drawing, checks `elem.float`: a float is Typst's
   own "remove from normal flow, reflow to the region top/bottom" — *exactly*
   the DOCX figure-flow model — so its blocks are emitted in place (live text),
   not rasterized. A non-float positioned overlay (watermark/decoration) still
   rasterizes-and-anchors, preserving its position. `place` was refactored to
   return `Vec<Block>` (the anchor logic factored into `set_place_anchor`); the
   shape-composition (§7.1f) and single-drawing anchor paths are unchanged.
2. **Standalone `FigureCaption` handler.** A `show figure` rule that emits
   `it.caption` separately from the body leaves a bare `FigureCaption` in the
   flow with no dispatch arm — it rasterized. Now `mappers::image::caption`
   realizes it (`FigureCaption::realize` → "Figure 3: …", number baked as
   static text since a caption divorced from its figure has no live counter
   context) into a `Caption`-styled paragraph. If that target-specific
   realization fails, the current planner retains the diagnostic and recovers
   the complete visible text from paged layout; it no longer returns an empty
   block list.

**Validated:** corpus-wide `RASTERIZE: caption` 132->0, `sequence` 734->586,
`styled` 402->374. But the rasterize-count drop *understates* the win: the
oracle A/B flagged 16 docs, and ALL 16 are pure CONTENT GAINS (every one's
extracted-text length increased) — twelve two-column paper templates (IEEE,
IOP, JACoW, ACM-VGTC, ABNT, …) recovered their entire title block + figure
captions as live, selectable text (e.g. `ioppub` +1103 chars: title, authors
with ORCID, affiliations, the full abstract, keywords — previously one `[]`
image). Zero content losses anywhere. The `nums` sub-flags are the documented
static-number tradeoff (a standalone caption's number is baked text, not a
live `SEQ` field). Visually (LibreOffice) the flowed two-column body renders
correctly, and page count *drops* (`ioppub` 6->5) since the title block no
longer consumes a full rasterized page. Full test suite green, corpus 617/627
OK, 0 INVALID.

### 7.1h Aggressive prefer-text-over-raster: all non-drawing `#place`, gradient/tiling boxes — IMPLEMENTED
A deliberate policy shift: where the exporter previously rasterized
text-bearing content to preserve a *visual* (exact position, a gradient fill),
it now extracts the text and accepts a cosmetic downgrade — because live,
selectable, editable text is almost always worth more than a
positioned-but-dead pixel image. Two changes:

1. **All non-drawing `#place` bodies flow** (§7.1g dropped its `float`-only
   restriction). A non-float, absolutely-positioned `#place` (a CV sidebar, a
   decorative overlay) now flows its blocks in place rather than
   rasterizing-and-anchoring. It loses its exact position (in a tight
   two-column CV the sidebar can overlap the main column), but keeps every word
   live. Genuinely visual placed content (a bare shape/canvas, lowered to a
   single drawing) still anchors, unchanged.
2. **Gradient/tiling-filled boxes extract their content** (`handle_block_box`
   dropped its `representable` fill gate, and the inline `inline_frame` path
   the same). A `#block`/`#rect`/`#box` with a gradient or tiling fill and a
   content body now emits its paragraphs, approximating a gradient by its first
   stop's colour as a solid `w:shd` shade (a tiling drops to no shade). Only a
   genuine *layouter* body (`#block(width => ..)`, an opaque closure with no
   extractable content) still rasterizes. The approximation goes through the
   shared `props::gradient_shade_hex`, which converts the stop to sRGB first —
   a gradient's stops live in its interpolation space (Oklab by default), so
   reading a stop's bytes verbatim would reinterpret the L/a/b triple as RGB
   (a pale lilac coming out bright red).

**Validated — the aggressive bet paid off cleanly.** Corpus-wide: `block`
2588->1142 (-56%), `box` 3212->2733, `sequence` 586->224, and `context`/
`styled`/`hide` all fell out of the top-20 entirely — ~2500 fewer
rasterizations. Oracle A/B flagged 26 docs and EVERY ONE is a content GAIN
(extracted-text length up in all 26 — theorem/definition boxes now
cross-referenceable, gradient callout boxes and positioned sidebars now live
text; e.g. `ostfriesen-layout` +810, `put-thesis` +652, `clean-hda` +457
chars). ZERO content losses across the whole corpus. Corpus batch went 617->
**618** (a previously-failing doc now exports), 0 INVALID, full test suite
green. Visually (LibreOffice): papers/theses render cleanly with live
hyperlinked references and proper TOC/caption text; the one visible cost is
mild column overlap in tight two-column CVs (a `#place` sidebar flowing into
the main column) — content fully intact, just imperfectly positioned, exactly
the accepted trade. The remaining `box`/`block` rasterizations are now almost
entirely genuine layouter closures (`layout(size => ..)` with `measure()`) and
`skew` content — but even *these* need not be text-dead (§7.1i).

### 7.1i The rasterized residue is not opaque: recover its text as hidden runs — IMPLEMENTED
§7.1h called the surviving layouter/`skew`/scaled-diagram rasterizations "the
true opaque residue." That was wrong. A rasterized element still lays out to a
real `Frame`, and that frame's `Text` items carry the exact glyph runs — the
words are right there, we were just throwing the frame away after rendering it
to a PNG. So `ctx.rasterize` now also walks the laid-out frame
(`collect_frame_text` / `frame_to_text`): it accumulates each `Text` item's
string with its position (folding in every `Group` translation), sorts by
reading order (y then x), and reconstructs line breaks from y-clusters and word
spaces from x-gaps. `laid_out_fallback` returns the drawing **plus** that
recovered text as **hidden `w:vanish` runs** in the same paragraph (and sets the
drawing's `descr` alt-text to the space-joined transcription).

The image is byte-for-byte the same PNG — **zero visual regression** — but the
region is no longer dead pixels: the recovered text is available to Word Find,
copy/paste, and indexing through hidden runs and the image description. This is
**not yet a screen-reader claim**: consumers differ in whether they announce
hidden runs, descriptions, or both, so release-level assistive-technology tests
must check for omissions and duplicate announcements. The hidden block is
bracketed with hidden spaces so its
first/last words keep a boundary against adjacent visible runs (without them a
consumer concatenating run text glues e.g. `urbane`+`Stoicos` — the one
tokenization seam the corpus check caught).

This threads through every rasterize call site: the block-level fallbacks and
`handle_layout`/`handle_block_box` (via a `fallback_para` helper wrapping the
runs in one paragraph), the inline box path, `rasterize_fallback`, and the
`#place` body-drawing anchor (drawing + hidden text share the anchored
paragraph). The vector-`image()` path takes just the drawing (an image has no
body text).

**Validated — pure gains, corpus-wide.** Isolated oracle A/B (HEAD vs this
change) flagged 298 docs; a subset-direction check (`is HEAD's text ⊆ the new
text?`) confirmed **HEAD ⊆ NEW everywhere — zero content loss, every flagged doc
is strictly additive**. Total recovered: **+37,328 searchable words (+10.7%)**
across those 298 docs — some more than doubled (`universal-jlu-thesis`
3122->7788, `fh-joanneum-iit-thesis` 2576->5070, `elegant-culsc-record`
359->1241). Corpus batch 618 OK / **0 INVALID**, 68/68 docx tests green,
LibreOffice round-trips cleanly (the `w:vanish` text correctly does not render,
so the visual is unchanged, while staying in the document model for
search and accessibility metadata). Assistive-technology behavior remains
subject to the screen-reader qualification above. The residue that "can't
become text" now does.

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

## 8. Pre-release hardening review

An adversarial, maintainer-grade review of the highest-risk / least-reviewed
surfaces (rasterize + hidden-text recovery, shape composition + DrawingML
encode, dispatch + table + OMML) before the public preview. Four real defects
fixed, one speculative fix reverted, and the crate brought to a clean build
under CI's `-Dwarnings`.

### Fixed
- **Rowspan continuation-cell borders** (`table.rs`): a vertically-merged
  (`rowspan`) cell's placeholder `w:tc` emitted `CellBorders::default()` = all
  `w:val="nil"`, so in a *bordered* table the merged cell's lower rows lost their
  left/right sides and bottom edge (a visibly open box). Now the continuation
  carries the origin's resolved left/right on every row, its bottom on the final
  row only, and no top (interior to the merge). The common-case, most-visible
  find.
- **`SEQ` field-code identifier sanitization** (`mappers/image.rs`): a custom
  `#figure(kind: "…")` name flowed into ` SEQ <name> \* ARABIC ` with only
  spaces escaped, so a `"`/`\`/empty name broke the field grammar and Word
  showed "Error!". The identifier is now restricted to alphanumerics + `_`, with
  a `Figure` fallback.
- **Shape bounding-box tightening** (`mappers/shape.rs::raw_bounds`): the box
  was seeded at the origin, so a path offset from `(0,0)` (a
  `#line(start: (10pt,10pt), …)`, or a group child away from the group origin)
  got an extent stretched back to include the origin — phantom padding, and
  every group child anchored at `a:off=(0,0)` with an oversized extent. Now
  seeded from the first real point: single-shape extents are tight (verified a
  `#line` box shrinking 60×40→50×30pt) and group children carry their true
  offsets. Renders are pixel-identical (the old oversized box drew the shape at
  the right place via an un-shifted path); the new structure is simply the clean
  one Word would author.
- **`move_text` `w:position` overflow** (`mappers/shape.rs`): `existing + shift`
  on `i32` → `saturating_add` (a pathological `dy` no longer panics/wraps).

### Reverted (speculative, net-negative — recorded so it isn't retried)
- **`frame_to_text` super/subscript line-clustering**: a review hypothesis held
  that the newline threshold (`|Δy| > size*0.6`, on the *current* run's size)
  would split a small raised superscript onto its own line in recovered hidden
  text. Changing it to the *max* of the two adjacent sizes was tried, but (a) the
  original never actually mis-split a real superscript — a typical ~0.35em raise
  sits just under `size*0.6` — so there was **zero** measurable benefit, and (b)
  raising the threshold on a big-run→small-run transition *suppressed a genuine*
  line break, gluing `NAME`+`WHAT` into `NAMEWHAT` in one corpus CV
  (`vercanard`, the lone doc the oracle A/B flagged). Reverted to the original
  heuristic; the dead `last_x_end = None` cleanup (a no-op) was kept.

### Clean under `-Dwarnings`
CI sets `RUSTFLAGS="-Dwarnings"`, so seven pre-existing dead-code warnings in the
crate (unused constants/fn, never-read `DocxDocument.bookmarks` and
`MediaPart.rel` fields, a `drop`-of-`Copy`, an ignored `#[must_use]` traverse)
were release-blocking. Each was confirmed truly dead and removed; the crate now
compiles warning-clean.

### Validation
69/69 integration tests; corpus **618 OK / 9 EXPORT_ERR / 0 INVALID** with a
per-doc outcome set byte-identical to before the pass; oracle A/B vs the pre-pass
binary shows no content regressions (the shape/border/field fixes change no
extractable text; `raw_bounds` changes only shape geometry); a multi-shape
composition renders pixel-identical before/after in LibreOffice; output stays
reproducible under `SOURCE_DATE_EPOCH`.

## 9. Adversarial (second-model) review — three design challenges, all upheld

An independent adversarial review (a different model, prompted to challenge
design choices rather than hunt implementation bugs) challenged three
decisions. All three challenges were verified against Typst's own semantics
and upheld; the fixes shipped together.

### 9a. `#hide` must redact, not embed
The exporter deliberately mapped `#hide[..]` to `w:vanish` hidden text
("invisible but searchable") — but Typst *documents* `hide` as a redaction
tool ("neither present visually nor accessible to Assistive Technology"), and
paged export enforces it physically: `Frame::hide` drops every frame item
except introspection tags, so the text simply does not exist in a PDF. Word
reveals `w:vanish` text with a single toggle — an answer key or redacted value
`#hide`-removed by the author would ship recoverable inside the package. Fixed:
the inline arm now harvests only the body's introspection tags (a label or
citation inside hidden content still resolves — the same "traces" paged
export keeps) and emits nothing; deliberately *without* lowering the body,
since lowering has side effects (a footnote or image inside `#hide` would
still register into `footnotes.xml`/`word/media` even with its runs
discarded). The block path already had the right semantics for free (it
rasterizes, and `Frame::hide` empties the frame before anything is read).

**The oracle flags proved the point**: the A/B flagged 16 docs, all word
*decreases* — and a PDF-ground-truth count showed every one was **phantom
text the PDF never displayed** being removed. touying's `#pause`-staged
reveal text now matches the PDF *exactly* (visible copies kept, hidden
duplicates gone: "uncover" PDF 3 / old-docx 5 / new-docx 3), and orange-book
shed 60 copies of a hidden "Main" nav label the PDF shows zero of. The
"regression" direction was fidelity gain.

### 9b. Sections must split on furniture, not just geometry
`resolve_sections` merged consecutive page runs whenever the *geometry*
(size/margins/columns) matched — but Word carries running headers, footers,
and page numbering on `w:sectPr`, so a mid-document `set page(header: ..)` or
`set page(numbering: ..)` change with unchanged geometry was silently merged
away: the new furniture never emitted. `same_geometry` became `same_section`,
comparing every section-scoped property (header/footer/background content by
`hash128`, numbering, number placement, header/footer bands, suppression
flags). `background_color` and `hyphenate` stay excluded — both are
document-wide in OOXML, so splitting cannot express them. Real-template
proof: **classicthesis went from 2 sections / 0 header parts to 7 sections /
3 header parts** — its running heads were entirely missing before; documents
whose sections were already geometry-driven (drupol) are byte-unchanged.

### 9c. `--pages` must error, not silently export everything
The CLI accepted `--pages` for DOCX and ignored it — a data-disclosure
footgun (a user exporting "pages 1–2" ships the whole document). Now a hard
error at config-build time ("a Word document flows continuously and has no
fixed pages to select from"). HTML inherits the same silent behavior
upstream; that is upstream's call to make — a new format should not copy the
footgun.

**Validation:** 72/72 integration tests (4 new: inline + block `#hide`
redaction, header-only and numbering-only section splits); corpus 618/9/0
identical; oracle A/B all-flags-verified-as-gains; `--pages` e2e errors for
docx and still works for PDF; LibreOffice opens the newly multi-section
output.

## 10. Real paged introspection plus the synthetic fallback

DOCX export now computes the standard `PagedDocument` fixed point first and
uses its introspector as the primary source while realizing the editable DOCX
tree. This replaces the old approximation for page reads, positions,
citations/bibliographies, and layout-time queries with the same converged
answers PDF sees. The original synthetic model remains as a fallback for
DOCX-only target branches and locations that genuinely have no paged
equivalent.

The earlier synthetic-only work is still relevant as the fallback layer and as
history for the failures it cleared:

### 10a. Paged-backed page numbers (`DocxIntrospector`)
`page()`/`pages()`/`position()`/`page_numbering()` now delegate to the real
paged introspector first. A walk over the lowered DOCX IR still counts explicit
page breaks (`Run::PageBreak`) and section breaks, assigning every fallback tag
location a (page, section) pair; `page_numbering` resolves against that
section's real `set page(numbering:)`. Header/footer locations are marked
separately and may be aliased back to the corresponding repeated paged-layout
location by tag key, so `here().page()` in page furniture can use paged truth
instead of the final-page fallback.

### 10b. Header/footer introspection-tag harvest
Page-furniture content lives outside the body IR, so a labeled element in a
running header/footer never reached the introspector — and templates *do*
query furniture (`query(<_ght-footer>.after(here())).first()`). `build_section`
now harvests tags from lowered header/footer blocks into `deferred_tags`
(append-at-end is exactly where an `.after()` query wants them; the builder
dedups duplicate locations across sections). Recovered: easy-hgb-thesis.

### 10c. Tolerate failing user numbering closures
A `#set figure(numbering: closure)` that reads introspection (querying
headings, indexing counter components) can fail against the empty
first-iteration introspector — under paged layout that is a *delayed* error
that gets retried, but our figure mapper propagated it as a hard abort on
iteration one, before any introspector was ever built. The caption's cached
number, the list-of-figures entry text, and the standalone-caption realize
are now best-effort (the `SEQ` field remains the live truth in Word).
Recovered: versatile-apa, gb-ctr.

**Result: corpus 618 → 623 OK (4 EXPORT_ERR, <1%), 0 invalid.** 75/75 tests
(3 new: synthetic page ref, footer label query, failing-closure tolerance).
Remaining tail at that point: tracl, sos-ugent-style, toffee-tufte, ijimai —
cleared in §11 below (except tracl).

## 11. The last three: positioned tags, stable shape classification, in-order scaffolding

Three parallel adversarial dives (a different model per doc, each in an
isolated worktree) root-caused the remaining tail; the portable parts were
synthesized here — two of the three prototype patches were partially
**rejected** by corpus gates and reworked (recorded honestly below).

### 11a. Positioned tags + leading-break exclusion (fixes ijimai)
ijimai's `show par` rule guards on `here().page() > 1` *and* position
comparisons against a heading. The §10 page model counted the *leading*
page-setup section break as a physical page (first content on "page 2" →
rule suppressed → the template's use-count assert saw 0), and positions
reported a dummy origin for every location (before §10 that made 10
paragraphs qualify — one flawed model, both wrong counts).
`collect_positioned_tags` now walks the IR once, producing the tag stream,
the page model, *and* a synthetic `PagedPosition` per tag (monotonic y in
block order — an ordering approximation, but strictly better than everything
at origin); leading breaks no longer advance the page; the introspector
stores real positions and `position()` returns `DocumentPosition::Paged`.

### 11b. Structural shape-only classification (fixes toffee-tufte)
The `contains_place → transformed` native-shape shortcut classified a placed
body by its *laid-out frame*: a sidenote mixing a rule line with a
`cite(form: "full")` looked shape-only on iterations where the unresolved
citation rendered empty, and textful once it resolved — the lowering flapped
between iterations, the cite's tag flickered, and the bibliography never
converged ("citation could not be located"). `body_shape_only` /
`placed_bodies_shape_only` now classify *structurally* (only genuinely
shape-composed bodies may take the vector path — false negatives fall back
safely, false positives were the instability), and `transformed` forwards the
consumed frame's introspection tags (previously silently dropped for native
shape groups). **Rejected from the prototype:** two "keep the body live in
flow" arms — the corpus proved they dropped placed letter/CV address-block
content that the rasterize path preserves (briefs −26 words, metronic −49,
inboisu −6, all present in the PDF).

### 11c. In-order scaffolding tags (fixes sos-ugent-style)
The `drafting` package initializes its margin-note state via
`box(place(layout(size => state.update(..))))` and reads it back at each
note's own position. The scaffolding rasterized, so its `state.update` tag
went through `deferred_tags` — appended after the whole body — and every
(earlier) read saw the initial value → the package's own panic. Three pieces:
inline `#layout(size => ..)` closures are now evaluated in place with the
synthetic page size (exactly what the block path has done since the `#layout`
recovery), `rasterize_with_tags` lets paragraph-level callers keep a
rasterized child's frame tags *at its position* (`ParaChild::Tag` exists at
that level; run-level contexts still defer), and an `inline_pchildren` arm
routes inline `#place` — and plain boxes holding one — through that
in-position path. The box case is gated on `contains_visible_text` being
false (pure scaffolding only): rasterizing a text-bearing place-box would
demote live text to an image just to reposition its tags, a downgrade the
word-count oracle structurally cannot see (it reads hidden text too).
**Rejected from the prototype:** a plain-box `inline_pchildren` recursion arm
that re-lowered box bodies at paragraph level — it broke elspub and four
other docs (net −4; the per-line-equation-label re-realization hazard
documented in the box/pad tradeoff notes).

**Result: corpus 626/627 OK (1 EXPORT_ERR: tracl, whose own template code
panics on any target it doesn't know), 0 invalid.** 79 tests (4 new: the two
dive regression tests plus a state-order and a mixed-placed-label test).
Oracle A/B vs the pre-dive binary: all flags triaged as fidelity gains —
rendercv now matches the PDF token-for-token (the old output duplicated its
contact header), tonguetoquill purely gains, and the `nums` class is the §10
flat-"1"-to-synthetic-numbers improvement.

## 12. Ink-bounded rasters — no phantom pages from blank renders

Found by the visual-fidelity oracle at corpus scale (the PDF-vs-DOCX render
comparison): touying decks rendered to ~5× their page count through
LibreOffice (`touying` 31→153 pages), with the worst presentations scoring
0.65–0.75. The dissection: the finite-height rasterize retry renders the FULL
page content region, and content that draws little or nothing in that region
(an `#uncover` step whose payload is currently hidden, a slide-furniture
composition that resolves to a sliver) still shipped as a content-area-sized
PNG — `touying`'s docx carried 7 near-empty PNGs (177–341 bytes) each
displayed at 742×474 pt, every one reserving a page of phantom space.

**The fix (`crop_to_ink`, `ctx.rs`):** render to the pixmap first, compute the
ink bounding box (any nonzero alpha — the render surface is transparent), then:
a fully blank render is **dropped** (its introspection tags were already
harvested, so convergence is untouched); a mostly-blank one is **cropped** to
its ink, with the display size scaled by the same ratio so the visual keeps
its exact physical scale. Ink that (nearly) fills the render — the common
image/diagram case — is kept byte-identical (≥97% threshold on both axes), so
ordinary rasters don't churn.

**The accepted trade:** cropping removes *outer blank margin*, so a lone
decoration positioned deep inside an otherwise-empty rasterized container
loses its offset-within-the-box (the inline image now sits at its natural
size in the flow). Relative positions *between* multiple ink regions are
preserved — the bbox spans all ink. This is the same content-over-phantom-
space policy as the §7.1h prefer-text shift. The one caller that must NOT
crop is the `set page(background:)` path (`rasterize_uncropped`): it
stretches the drawing to the full page, so an ink-cropped corner watermark
would be blown up to full-bleed.

### 12a. Two review-fix cycles the oracle caught (the honest record)

The naive pixel-crop shipped two regressions of its own, both caught by the
word-oracle A/B and fixed before landing:

1. **`#move` content rendered blank → dropped.** `#move` draws its child
   OUTSIDE the frame's own [0, size] box, and the render canvas covered only
   the frame's extent — so the pre-crop code had been shipping thousands of
   fully TRANSPARENT PNGs (a pre-existing invisible-image bug!) whose hidden
   text papered over the visual loss; blank-dropping deleted both (a
   code-heavy book lost 84 of its `[N]` array-index marks). Fix:
   `frame_ink_rect` computes the GEOMETRIC bounds of everything that draws
   and the canvas covers them (`Frame::hard(ink_size)` + `push_frame(-min)`)
   — moved content now renders *for the first time*, and the pixel crop runs
   second. The book recovered byte-exactly, with real renders replacing the
   transparent rectangles.
2. **Glyph-outline text bboxes failed on CJK faces → canvas collapsed.**
   `TextItem::bbox()` is outline-extraction based and can come up empty
   (spaces, some CJK fonts); a frame whose only *measurable* ink was a
   zero-height rule collapsed the canvas to a 1-px strip and the (present!)
   text rendered outside it → blank → dropped, losing hidden text across
   several Chinese theses. Fix: metrics-based text rects
   (ascender/descender/advance — always finite), and non-finite item rects
   are skipped rather than poisoning the union (an unstroked infinite line
   must not erase the whole raster).

Final adjudication across the maximized 818-doc corpus: batch 816/2/0
identical to base; every oracle flag proven to be a gain (recovered
cross-refs, phantom-duplication removal — one book's docx dropped from 2.2× the
PDF's word count to near-parity), byte-neutral, or pandoc `[]`
placeholder-noise from correctly-dropped blank images. Zero real content loss.

## 13. Field ownership: exact Typst semantics without hostile open-time updates

The old field design had two architectural problems. First, every field set one
document-wide `uses_fields` bit, which emitted `w:updateFields`: Word then
recalculated *all* fields even when only pagination/TOC behavior was live.
Second, the `RefElem` mapper was not the final reference path. Ordinary
realization had already converted references into `DirectLinkElem` and then
linked style state, so a mapper-only `REF`/`PAGEREF` policy was bypassed by the
output users actually received.

The replacement is explicit and survives realization:

- `DirectLinkKind::{Reference, PageReferenceSupplement, PageReference, Other}`
  and a source span are carried through the library/layout direct-link rule.
  DOCX keeps normal reference text as Typst-computed clickable hyperlinks. A
  page reference is one semantic group whose localized/custom supplement plus
  nonbreaking space remains a static linked segment while only the numeric
  segment becomes one live `PAGEREF`; multi-run numeric results are coalesced
  instead of duplicating the field. Exact groups enroll as
  `NativePageReference` fidelity decisions.
- `FieldMode::{Static, Live}` records value ownership. Static complex fields
  encode `w:fldLock`; live fields stay consumer-updateable. `FieldDisplay`
  separately records visible versus hidden behavior.
- `FieldCacheStatus::{Resolved, BestEffort, ConsumerRequired, Unavailable}` records cached
  result provenance independently of ownership. Final-IR validation rejects
  impossible combinations. Figure-number evaluation failures now retain their
  exact `FieldPlanning` diagnostics, record a `FieldCacheUnavailable`
  approximation, and remain visible in the public/embedded field inventory
  instead of being erased by `Err(_) => Vec::new()`.
- Figure `SEQ` is visible/live only for a single Typst numbering component that
  exactly matches Word's Arabic/alphabetic/roman format switches. Prefixes,
  suffixes, multiple components, padding, and functions remain exact Typst text
  plus one hidden `SEQ` counter so Word's caption/list ecosystem still counts
  the paragraph.
- LibreOffice 26.2.4.2 exposed that `SEQ \h` alone is not portable: its first
  render appended a visible `1` to `Figure (i)`. Hidden fields now apply
  `w:vanish` to their begin/instruction/separator/cache/end runs, and the Writer
  render returns to exactly `Figure (i)`.
- Native TOCs bake entry text and Typst-computed page-number field caches, stay
  unlocked, and expose Word's ordinary **Update Table** command. TOCs containing
  Typst-only fallback entries are locked because Word cannot rebuild them.
  Neither `w:updateFields` nor `w:dirty` is emitted: real Word validation showed
  both variants trigger disruptive external-field and TOC dialogs on open.

The field-policy kernel was compiled through the real CLI, opened in Microsoft
Word, round-tripped/rendered through LibreOffice, and compared with the Typst
PDF reference. Final Word first-open had no modal dialog and showed a populated
TOC/page cache, `Figure (i)`, a clickable static `Figure i` reference, one live
page reference, and a live footer `PAGE` field. LibreOffice showed the same
caption/reference text on one 170 mm x 120 mm page. A manual select-all/F9
update followed by a real Word save kept the Typst-owned hyperlink and hidden
counter intact while updating live field caches; Writer rendered that Word-saved
round trip with the same visible text. Structural gates now cover all ownership
branches; the DOCX target passes 173 tests and the crate passes 13 unit gates.
The cache-failure kernel also survived a LibreOffice 26.2.4.2 save/reopen: live
`SEQ`/`PAGEREF` instructions remained, `cache="Unavailable"` survived in the
Writer-stable custom property, and the one-page visible/searchable text was
unchanged across the Writer round trip.
An atomic page-reference fixture then compared default `page 1` and custom
`sheet 1` references across Typst PDF, first-open DOCX, a Writer save/reopen,
and a real Word select-all/F9/save. All three extracted texts were identical;
Word's accessibility tree exposed separate linked `page `/`sheet ` segments and
two live `PAGEREF` values before and after the update. Both Word and Writer
saved packages retained the two instructions and visible supplements, and Word
preserved the embedded `NativePageReference` evidence.

## 14. Scoped width ownership and physical grid gutters

The previous table, grid, and horizontal-stack mappers assumed a 9,360-twip
US-Letter text area regardless of the document's page geometry. Equation-number
tabs separately assumed 8,640 twips, while raster fallback already used a
page-derived width. That split authority produced over-wide tables on narrow
pages, under-wide tables after a wider section change, and nested tables sized
as if they were still at document scope. Typst's resolved gutter tracks were
discarded entirely.

`DocxCtx::available_width` is now the single scoped width budget:

- each section installs `page width - left margin - right margin` before its
  body is lowered;
- nested table and horizontal-stack cells lower their bodies under their own
  track width and restore the parent budget even on an error;
- flexible table/grid tracks, horizontal stacks, equation-number tabs, fill
  tabs, outline tabs, relative shapes, and raster layout read that same value;
- column gutters become borderless `w:tc` tracks, row gutters become exact
  spacer rows, and a colspan includes every internal gutter track;
- `CellGrid` internally doubles both axes when either axis has a gutter. The
  DOCX mapper filters normalization-only zero tracks, so a row-only gutter does
  not create a phantom column and a column-only gutter does not create a row.

The width kernel covers narrow and wide sections in one document, flexible
tracks, mixed row/column gutters, axis-only gutters, gutter-aware colspans,
nested tables, horizontal stacks, and equation-number tabs. It was also built
through the real CLI and opened in Microsoft Word: Word's accessibility tree
reported the expected editable 4-row/3-column gutter table plus the nested
table, and the visual view showed both section widths and the 1:2 track ratio.
LibreOffice Writer rendered the same two-page DOCX with the same horizontal
edges, gutter gaps, and nested-cell constraint. Comparison with the Typst PDF
also exposed a separate pre-existing flow issue: fixed `#v` between two table
blocks had no paragraph to carry its spacing. The explicit flow policy in §15
now preserves it rather than hiding the defect inside width mapping.

## 15. Explicit block-flow spacing and stack gaps

`#v` was previously held in a converter-local integer until the next paragraph
appeared. A paragraph could absorb it as `w:spacing/@w:before`, but a table could
not. In `table; #v(10pt); table`, the accumulator skipped the second table and
either moved the gap to a later paragraph or dropped it at the end of the body.

The typed IR now has `Block::FlowSpace { dxa }`. Paragraph-adjacent gaps still
use ordinary paragraph spacing; when the next visible block is a table or TOC,
the converter emits a flow-space paragraph with an exact line height. This is a
representation decision in the lowering layer, not an encoder guess, and works
inside table cells as well as at document scope.

Stacks now preserve their own spacing contract too:

- fixed vertical gaps become `FlowSpace` blocks;
- fixed horizontal gaps become exact borderless table tracks;
- fractional horizontal gaps become flexible tracks instead of disappearing;
- fractional/region-relative stack geometry is marked
  `Approximate/FlexibleStackSpacing` with visual loss in `FidelityReport`.

The structural gates cover a 10 pt gap between two tables, a 6 pt vertical-stack
default, a 12 pt horizontal-stack track, and a retained/reported `1fr` gap. The
two-section fixture was rebuilt and reopened in Word and LibreOffice; the 10 pt
gap is now visible in both consumers and all table content remains editable.

The structural DOCX target passes 158 tests; the crate library test and clippy
with warnings denied also pass.

## 16. Whole-region preflight for placed text and tables

`#place` used to discover its representation while lowering. Separate branches
would independently ask whether the body was shape-only, whether it happened to
fit a text box, or whether the first lowered child was a drawing. That made the
policy implicit and made it easy for a future mapper change to admit a Word-
hostile child into `wps:txbx` or to consume only part of a rich placed body.

`mappers::image::PlacePlan` is now selected before any placed-body lowering:

- `NativeShapeGroup` handles source-structural shape-only compositions;
- `NativeTextBox(TextBoxWrap::None)` handles plain extractable text;
- `NativeTextBox(TextBoxWrap::Square)` handles exactly one root text-only
  table/grid, rejecting nested tables, notes, math, drawings, lists, figures,
  and other consumer-sensitive children;
- `LowerOnce` lowers every other body exactly once, then accepts only a sole
  native drawing as an anchor. Rich editable blocks flow in document order with
  `PositionedContentFlowFallback`; empty/unrecoverable visual regions retain the
  existing whole-region raster path.

This is intentionally a narrow capability admission, not a claim that every
`w:tbl` is safe in every drawing container. A table containing OMML remains on
the explicit fallback path until that combination has its own Word and Writer
evidence. The structural tests assert native `w:tbl` inside `wps:txbx`, absence
of `a:blip`, a native `PositionedTextBox` decision, and a reported fallback for
the table-plus-math case.

The real CLI fixture was compared with its Typst PDF and rendered through
LibreOffice Writer. Microsoft Word opened it without repair, exposed the
anchored object as `Placed Text Box 1` containing a 2-row/2-column table and
individually selectable cells, and accepted an in-cell text replacement while
retaining the native table. This validates the authoring path in addition to
the package structure. The structural DOCX target passes 158 tests; the crate
library test and clippy with warnings denied also pass.

## 17. Explicit preflight for page-varying furniture

Word sections can reference at most three header/footer variants: first page,
even pages, and the default odd-page value. Typst contextual furniture can vary
on every page. The earlier implementation sampled pages 1 through 5 to avoid
misclassifying literal page numbers as a stable odd/even pattern, but when the
samples proved unstable it silently repeated page 1.

`document::FurniturePlan` now owns that whole-region decision before any part
is serialized:

- `Exact` carries native default, first/default, even/default, or
  first/even/default references after proving the sampled signatures stable;
- `Sampled` carries the page-1 region only when pages 2/4 or 3/5 disagree.

The sampled branch stays native and editable, but records
`Approximate/PageFurnitureSampled` with visual and dynamic-behavior losses and
the character count of the emitted region. It also emits a source-located CLI
warning explaining that the page-1 value repeats. This closes the silent-loss
failure mode without pretending the remaining visual limitation is solved.

The structural gates prove that exact first-page and odd/even furniture still
produce the correct `w:titlePg` and reference types, while a five-page literal
page-number header produces one default part and one structured approximation.
A real CLI fixture emitted the warning at the contextual header span; package
inspection confirmed one native `header1.xml` containing the page-1 value.

## 18. Validated, deterministic OPC finalization

The shared `typst-ooxml-core::opc::Package` previously accumulated unchecked
vectors and called `expect` for `start_file`, `write_all`, and ZIP finalization.
A duplicate part, conflicting extension content type, or I/O failure therefore
produced either a corrupt package or a process panic. Relationship XML was
serialized early, leaving finalization unable to prove its targets existed.

Finalization now returns `Result<Vec<u8>, PackageError>` and validates before
writing any bytes:

- part names are unique, relative, non-reserved, and contain no empty,
  traversal, or backslash component;
- default and override content types cannot conflict;
- part-owned relationships remain typed through `Package::add_relationships`;
- every relationship owner exists and every internal target normalizes relative
  to that owner to an existing part; external targets are intentionally exempt;
- relationship mode participates in deduplication, so identical internal and
  external URIs cannot accidentally share one rId;
- content-type overrides and package parts are sorted canonically before ZIP
  emission, while timestamps and permissions remain fixed.

DOCX and PPTX translate `PackageError` into detached export diagnostics instead
of panicking. Seven shared OPC unit tests cover the invariant failures and
insertion-order independence. The complete DOCX package is byte-identical over
repeated exports, all 158 DOCX structural tests and all 41 PPTX structural tests
pass, and clippy is clean with warnings denied across the three Office crates.

## 19. Stable export snapshot and embedded fidelity manifest

Representation decisions previously used a logical ID hashed from source span,
element name, and Typst `Location`. Locations are realization-specific, so the
same source node could receive different IDs in the paged and DOCX universes—the
exact reconciliation failure the architecture is meant to remove.

`ExportSource` now uses source span plus element identity for source-backed
nodes, retaining `Location` as evidence rather than identity. Detached nodes
still include location to avoid collapsing unrelated generated content.
Before lowering, `ExportSnapshot` owns:

- top-level semantic lowering regions and nested located semantic nodes;
- aggregated semantic occurrence counts;
- every paged element with matching source span/element identity, recorded as
  page plus resolved x/y points;
- all converged page sizes and a deterministic snapshot ID.

No arena-backed `Content` or `StyleChain` escapes realization. Two independent
DOCX compilations of a two-page heading fixture produce identical snapshot and
node IDs, with each heading associated to its correct converged page.

The report is no longer process-local. Every DOCX contains the versioned
`customXml/typstFidelity.xml` part and a typed relationship from
`word/document.xml`. The manifest includes snapshot/pages/nodes/positions,
representation counts and decisions, every independent loss dimension,
occurrence and affected-text counts, plus retained suppressed-diagnostic stage,
kind, count, and message. `DocxDocument::fidelity_manifest_xml()` exposes the
same artifact to external tooling. The schema marks enrollment as `partial` so
consumers cannot mistake the current lossy-path coverage for complete native,
font, field, or compatibility accounting.

The real CLI rebuilt the placed-table fixture with the manifest. Microsoft Word
opened the package without repair and retained the editable anchored textbox,
2-row/2-column table, and individual cell accessibility. LibreOffice Writer
rendered the package as one 453.543 x 340.157 pt page. The custom relationship
and manifest therefore open in both primary consumers without perturbing the
native authoring surface. Later Writer save-round-trip testing showed that
opening is not preservation: Writer drops arbitrary `customXml` parts. Section
26 records the redundant carrier added for that case.

## 20. Whole-region table/grid preflight and representative cell paint

The table mapper previously unwrapped `TableElem.grid`, silently returned no
blocks when `GridElem.grid` was absent, and discovered visual limitations only
while encoding cells. Auto/fractional/relative tracks were distributed against
the scoped flowing width without recording that this was not Typst's measured
paged geometry. Gradient/tiling fills returned `None`, so a visually important
cell could silently become unfilled.

`mappers::table::TablePlan` now selects before cell lowering:

- `Native` for a resolved, non-empty grid with fixed absolute tracks and
  representable solid cell/border paint;
- `Approximate` for auto/fractional/relative/em tracks, relative row geometry,
  gradients/tiling/transparency, unsupported stroke dash/cap/join/miter nuance,
  or repeatable footers;
- `Empty` for a source region with no visual cells;
- `Raster` when no resolved grid exists. Whole-region fallback runs before a
  `Drop/TableResolutionUnavailable` decision can be emitted.

Native and approximate paths remain real `w:tbl` structures. The report records
`NativeTable` or `TableGeometryApproximation` against the stable source ID, with
affected searchable characters and semantic-node count. `ExportDecision` now
tracks `affected_semantic_nodes`; repeated source realizations aggregate it just
like occurrence and text counts, and the embedded manifest persists it.

Word cell shading cannot carry gradients or alpha. Instead of deleting the
fill, the mapper converts every stop into sRGB, composites transparency over
Word's default white cell, and averages the stop colors into one representative
solid. Non-RGB internal color spaces are converted before byte extraction—an
early real-fixture pass caught the previous component-space mistake, which had
turned a red/blue gradient into dark red `8E1600`. The corrected representative
is purple `854E9D`.

The PDF/DOCX table kernel uses a 1:2 fractional table with a red-to-blue heading
gradient. Word opened it without repair and exposed a native editable
3-row/2-column table plus individual cells. LibreOffice Writer rendered one page
with the same normalized table width and 1:2 column geometry. The Typst PDF keeps
the full gradient; Word and Writer show the representative purple tone, while
the manifest records `Approximate/TableGeometryApproximation`, 96 affected text
characters, and one semantic node. The structural gate separately proves a
fixed-track solid table enrolls as `Native/NativeTable`.

## 21. Finalized field/font inventory and referenced-relationship validation

Field behavior and font dependence previously existed only in the serialized
markup. That made it impossible for corpus tooling to distinguish a stable
Typst result from a consumer-recalculated field, or to identify an inline-only
font that was absent from `fontTable.xml`.

After all body, table, TOC, drawing/text-box, header/footer, and footnote
lowering, one recursive IR inventory now records:

- every typed field, grouped by stable snapshot ID, normalized instruction,
  `Typst`/`Consumer` update owner, visible/hidden result, and occurrences;
- every referenced default, style, math, and concrete-run font, with stable ID,
  occurrences, and explicit `embedded=false` status.

Both collections are persisted in `customXml/typstFidelity.xml`. The font
collection also drives `word/fontTable.xml`, so a family used only by a local
run no longer disappears from the package's declared fonts. Focused structural
gates cover `TOC`, `PAGEREF`, `SEQ`, and `PAGE` ownership and a document whose
inline monospace font differs from its root serif font.

Shared OPC finalization now closes the next consumer-repair failure class. It
parses every `.xml` part and validates every relationship-namespace `id`,
`embed`, and `link` attribute against the `Rels` set owned by that exact source
part. Malformed XML, a missing relationship ID, or an ID accidentally borrowed
from another part returns a typed `PackageError`. Three new shared gates cover
those cases. All 160 DOCX structural tests, all 41 PPTX structural tests, and
all 11 shared core unit tests pass.

WordprocessingML identifiers have additional format-specific scope rules, so a
finalized-IR validator runs before DOCX serialization. It recursively covers
body content, tables, TOCs, field results, text boxes/groups, headers, footers,
and footnotes, rejecting zero/duplicate drawing IDs, duplicate or unpaired
bookmark IDs and names, dangling footnote references, duplicate numbering IDs,
and missing abstract or paragraph numbering targets. These checks deliberately
remain in `typst-docx`; only format-neutral package mechanics live in the shared
OPC crate. Two crate unit gates cover duplicate drawings and unpaired bookmarks,
and the full structural suite proves every current exporter path satisfies the
validator.

## 22. Heading-to-Normal cascade-safe style deduplication

Word resolves a heading run through direct formatting, `HeadingN`, `Normal`,
and document defaults. The old cleanup treated Normal as if it were always the
run's immediate parent. In a blue Heading1 inside a red Normal document, an
explicit red span survived Heading1 deduplication but was then removed because
it equaled Normal; Word re-inherited blue from Heading1 and changed the span.

`apply_style_inheritance` now strips a Normal/default property from a heading
run only when HeadingN does not define that property. Values matching HeadingN
are still moved into the style, while deviations from HeadingN remain direct
even when they happen to equal Normal. A regression fixture covers font, size,
and color across this exact three-level cascade. The DOCX structural target now
passes 161 tests.

A real CLI fixture was compiled to the Typst PDF reference and DOCX. Package
inspection showed the corrected span retained explicit Libertinus Serif,
11-point size, and `AA0000` color beside the blue 20-point Heading1. LibreOffice
Writer 26.2.4.2 opened and rendered the DOCX as the same single 150 mm x 80 mm
page; the heading, red override, and red body remained visually aligned with the
PDF, and all text stayed searchable.

## 23. Converged paged geometry for native DOCX tables

Flexible table tracks were previously re-solved inside DOCX from the current
flowing-width budget. That fixed hard-coded page widths but still duplicated a
layout algorithm without Typst's content measurements. It also made nested
tables too wide by ignoring the parent cell's physical insets and estimated
page-column gutters independently from paged layout.

Layout already emits hidden `GridCellRegion` tags containing each final cell's
logical coordinates, spans, width, and height. A new format-neutral
`typst-export-common::paged::PagedGeometry` walker captures those tags from the
converged `PagedDocument`, including nested frame transforms, and groups them by
the same source-backed logical ID used by the DOCX snapshot. The CLI passes this
owned sidecar into the independent DOCX realization. `ExportSnapshot` persists
the physical cell facts and the embedded fidelity manifest serializes them.

Table preflight now derives `w:tblGrid` content tracks and physical gutter gaps
from those measurements. Axis-aligned auto/fractional/relative tables with a
complete single-cell sample per column can enroll as `Native/NativeTable`;
measured row heights become non-clipping `atLeast` minima. Unsupported paints,
border nuance, repeated footers, transforms, fully merged columns without a
solvable sample, and callers that do not supply paged geometry remain explicit
approximations.

The structural regression uses a 1:2 fractional table and proves the snapshot,
Word grid, native decision, and manifest all share the converged ratio. Existing
nested-table and page-column gates now compare emitted widths directly with the
paged oracle: this exposed and fixed the old parent-inset and hand-estimated
gutter assumptions. A real auto/1fr/2fr table with a 7 pt gutter and nested 1:2
table was compiled to PDF and DOCX. The manifest enrolled two measured native
tables; LibreOffice Writer 26.2.4.2 rendered the same single 160 mm x 120 mm
page, outer/nested cell boundaries closely matched the PDF, and all cell text
remained searchable and editable. Microsoft Word opened the same package without
a repair prompt; its accessibility tree exposed one native 2-row/5-column outer
table (content columns plus two physical gutters), the nested 1-row/2-column
table, every individual cell, and an `Accessibility: Good to go` result. The
DOCX target passes 162 tests.

## 24. Repair-sensitive WordprocessingML sequence gate

Well-formed XML and complete OPC relationships are necessary but do not prove
that Word will accept the child order of a complex type. Word is particularly
sensitive to property elements emitted after content, table grids emitted after
rows, a section-properties element that does not terminate the body, and
malformed markup-compatibility branch order. These mistakes can survive a plain
XML parser and only appear as a repair prompt in the consumer.

The shared `Package` now exposes a read-only iterator over its accumulated XML
parts. It remains format-neutral: it does not know Word element names or schema
policy. Immediately before generic OPC finalization, `typst-docx::schema`
parses those parts and enforces the repair-sensitive sequences currently emitted
by this exporter:

- at most one leading `w:pPr`, `w:rPr`, `w:tblPr`, `w:trPr`, or `w:tcPr` in
  its owning container;
- at most one `w:tblGrid`, before every `w:tr`;
- a final paragraph in every table cell, preserving Word's editable cell
  terminator;
- at most one final `w:sectPr` in `w:body`;
- one or more `mc:Choice` branches before at most one `mc:Fallback`.

A violation becomes a detached export diagnostic naming the package part,
container, and failed sequence before ZIP bytes are written. Six focused unit
gates cover a valid representative tree and late paragraph properties, late
table grids, reversed or choice-less compatibility branches, and missing cell
terminators. The runtime gate executes across all 162 DOCX structural tests.
Strict clippy passes for `typst-ooxml-core` and `typst-docx`.

The measured-table CLI fixture was rebuilt through this gate. ZIP integrity was
clean, and LibreOffice Writer 26.2.4.2 opened it headlessly and produced one
453.543 x 340.157 pt (160 x 120 mm) PDF page. This is consumer-open evidence for
the representative package, while the earlier interactive Word table evidence
continues to cover editability and accessibility; neither is presented as full
schema validation.

This intentionally does not claim full ECMA-376 validation. The remaining gate
is validation of every emitted part against the chosen Office-version schemas,
plus a maintained allowlist for Microsoft extension namespaces and compatibility
markup; consumer open/save tests remain independent evidence rather than a
substitute for that work.

## 25. CJK/RTL language slots and physical paragraph alignment

The exporter already split text by installed glyph coverage, emitted every
resolved fallback family into `fontTable.xml`, and carried `w:rtl`/`w:cs` on RTL
runs plus `w:bidi` on RTL paragraphs. Two consumer-visible gaps remained:

- `w:lang` populated only the default `w:val` slot. Word has separate
  `w:eastAsia` and `w:bidi` slots for East Asian and complex-script font and
  proofing behavior; it does not reliably infer them from `w:val`.
- Typst resolves horizontal alignment into global physical coordinates, while
  Word's `w:jc="start"`/`"end"` are logical values that reverse under
  `w:bidi`. Mapping physical right directly to logical end placed default RTL
  paragraphs at the left margin in LibreOffice Writer.

Language serialization now mirrors Typst's RTL-language set and writes the
script-specific slot for Japanese, Korean, Chinese, Arabic, Divehi, Persian,
Hebrew, Kashmiri, Punjabi, Pashto, Sindhi, Uyghur, Urdu, and Yiddish. The same
helper drives direct run properties, `docDefaults`, and `themeFontLang`.
Paragraph lowering now translates Typst's physical left/right alignment into
Word's logical start/end after considering the paragraph direction.

One structural regression covers Japanese and Arabic language slots, RTL run
and paragraph properties, and explicit logical-start alignment. The DOCX target
passes 163 tests.

A dated 160 x 120 mm fixture (2026-07-10) combines English, Japanese, Hebrew,
Arabic, and mixed inline text. Typst PDF and Writer DOCX renders both stayed on
one identically sized page. The first render exposed the left-margin RTL bug;
after the fix, Writer and current Word placed the standalone Hebrew and Arabic
paragraphs at the same physical right edge as Typst. `pdftotext` recovered all
four scripts from the Writer PDF. Package inspection showed `w:eastAsia="ja"`,
`w:bidi="he"`/`"ar"`, `w:jc="start"`, and the actual Hiragino Sans, Arial
Hebrew, and Geeza Pro fallback families. Word opened without repair and its
accessibility tree exposed the complete multilingual content as native document
text.

## 26. Missing-font facts and Writer-stable fidelity evidence

The finalized font inventory previously recorded only family, occurrence count,
and `embedded=false`. That made an installed family and an unresolved portable
Word reference indistinguishable even though the latter lets Word or Writer
choose different glyphs, widths, and line breaks from the Typst PDF fallback.

Each `FontFact` now records `available_at_export`. Final inventory checks the
actual `FontBook` after every style/body/table/TOC/text-box/header/footer/
footnote run has been lowered. The embedded manifest emits
`availableAtExport` per family and an occurrence-weighted `missingFonts` count;
the stable font ID remains snapshot plus family so machine availability does not
change semantic identity. A missing family remains in `fontTable.xml` and run
properties as a valid editable Word reference rather than being silently
rewritten to the export machine's fallback.

The same investigation found that Writer 26.2.4.2 removes
`customXml/typstFidelity.xml` during an open/save DOCX round trip. Every new DOCX
therefore also stores the exact canonical manifest text in the standard
`TypstFidelityManifestV1` custom document property at `docProps/custom.xml`,
with a package-root custom-properties relationship. Word and tooling can keep
using the canonical custom-XML part; recovery tools use the property when that
part is absent. This is redundancy, not a second independently generated
report, so the two carriers cannot drift at export time.

Structural regressions cover installed-versus-missing font facts, manifest
counts/attributes, the portable `fontTable` reference, custom-property/root-
relationship presence, and exact equality between the canonical and redundant
payloads. The DOCX target passes 173 tests.

A 2026-07-10 missing-font fixture compiled to one 160 x 120 mm Typst PDF page
and one identically sized Writer page. Both kept searchable text and happened to
choose substitutes with the same line breaks, which is recorded as an observed
consumer result rather than a portability guarantee. Current Word opened
without repair, showed the unavailable requested family in the font UI while
displaying substituted glyphs, exposed native editable text, and reported
`Accessibility: Good to go`.

Writer then saved the DOCX back to DOCX. It removed the canonical custom-XML
part but retained `docProps/custom.xml`; extracting the custom-property value
produced the same 1,730-byte manifest and SHA-256
`511dbf865d506b860da306c5c2bf19ab1b099a18daeced015b586c5a3fcb356d` as the
original canonical part. This proves exact evidence survival for this fixture,
not a blanket guarantee across future Writer versions.

## 27. Explicit drawing accessibility semantics and inventory

> **Scope:** This section records structural metadata and dated observations for
> specific fixtures. It does not establish WCAG, Section 508, EN 301 549, or
> screen-reader conformance for arbitrary exports. Package validation cannot
> substitute for Word's Accessibility Checker, reading-order review, and testing
> with the assistive technology used by the intended audience.

Drawing accessibility previously depended only on optional image alt text.
Bodyless vector art, page backgrounds, text boxes, described pictures, and an
unlabeled meaningful picture could all serialize with the same empty
`wp:docPr@descr`, while page-foreground rasterization discarded recovered text.
That made the exporter unable to distinguish intentional decoration from an
accessibility defect.

`Drawing` now owns explicit `decorative` intent. Bodyless native shapes/groups
and behind-text page backgrounds emit Office 2019's `adec:decorative val="1"`
inside the standard drawing extension list. Native text boxes remain
non-decorative because their editable text is the accessible content. Images
with Typst `alt` retain that description; images without it stay
non-decorative and are reported as unlabeled rather than being falsely hidden
from assistive technology. Recovered text from a page foreground now becomes
its description instead of being dropped. A final-IR invariant rejects a
decorative drawing that also carries alternative or native text.

The finalized fidelity inventory recursively traverses body, tables, TOCs,
field results, nested text boxes/groups, headers, footers, and footnotes. Each
`DrawingAccessibilityFact` records stable ID, `docPr` ID, name, optional alt
text, decorative intent, native-text presence, and computed unlabeled state.
The manifest adds drawing/unlabeled counts and a `<typst:drawings>` collection.
Existing structural tests now cover described and unlabeled SVG pictures,
native text boxes, decorative vector shapes, decorative backgrounds, and a
described foreground; one new invariant unit gate covers contradictory intent.
The DOCX target now passes 173 structural tests and 13 crate unit tests.

A 2026-07-10 mixed fixture produced five drawings: one described SVG, one
unlabeled SVG, one native text box, one decorative orange shape, and one
decorative page background. The embedded report counted exactly five drawings
and one unlabeled drawing. Typst PDF and Writer rendered one identically sized
160 x 120 mm page with all four visible objects and searchable text; Writer's
flow model shortened the text-box height but retained its editable content.

Current Word opened without repair. Its accessibility tree identified the
background and orange shape as `Decorative`, exposed the described image by its
alt text, exposed the callout as a textbox with native text, and left the second
picture as `Picture 2`. Word's Accessibility Assistant reported exactly one
`Missing alt text` issue, matching `unlabeledDrawings="1"`; all other media,
contrast, table, structure, and access categories reported zero issues. This is
the intended correlation between package evidence and the consumer UX.

## 28. Whole-region planning for failing standalone captions

A standalone `FigureCaption` emitted by a custom `show figure` rule previously
used `let Ok(realized) = ... else { return Ok(Vec::new()) }`. A numbering or
supplement closure that failed only under `target() == "docx"` therefore deleted
the entire caption without a warning, fallback, or fidelity fact.

Caption lowering now chooses an explicit plan. Native realization remains an
editable `Caption` paragraph. On failure, every diagnostic is retained at the
`CapabilityPlanning` stage and the complete caption is re-laid out under the
paged target. Recovered text becomes one visible editable paragraph with a
`StandaloneCaptionTextFallback` approximation. Only when no text is recoverable
does the planner try one whole-caption raster; failure of that second fallback
records `StandaloneCaptionUnavailable` as a `Drop` with affected text and a
warning. Introspection tags from the paged recovery remain enrolled.

A real 160 x 100 mm fixture deliberately failed its DOCX numbering closure.
Typst PDF and Writer both extracted exactly `Figure 1: This caption remains
visible and searchable.` followed by editable body text. The DOCX manifest
recorded `Approximate`, `StandaloneCaptionTextFallback`, and 54 affected text
characters. Writer rendered the caption visibly as ordinary text; its flowing
alignment differs from the centered PDF and is therefore reported rather than
presented as exact fidelity.

## 29. Explicit failure provenance for layout regions and TOC page caches

A block `#layout` could fail standalone callback evaluation, fail its paged
whole-region fallback, and then return successfully with no block, warning, or
representation decision. The suppressed errors existed, but the manifest
incorrectly reported zero drops. `handle_layout` now treats that double failure
as `Drop/LayoutCallbackUnavailable`, emits a source warning, and retains both
the `LayoutCallback` and `FallbackLayout` diagnostics.

The real 120 x 80 mm regression fixture is deliberately height-sensitive: the
60 mm PDF content region renders `VISIBLE LAYOUT BODY`, while both DOCX recovery
attempts reject the 80 mm region. The final package keeps the following body
text, reports `drop="1"`, and names the failed region instead of claiming full
fidelity.

TOC page-number cache evaluation also no longer converts a failed counter
display to an apparently resolved page `1`. Its diagnostic is retained at
`FieldPlanning`, the approximation is enrolled, and the live `PAGEREF` carries
the distinct `BestEffort` cache provenance: a visible placeholder exists, but
Word or Writer owns refreshing it. Final-IR invariants require best-effort
fields to remain live and to carry a placeholder.

## 30. Introspection-only placed bodies are not visible representations

Placed-content lowering previously tested only whether `ctx.blocks(body)` was
non-empty. A failed nested layout can leave an ordered `Block::Tag` even though
it emitted no paragraph, table, drawing, or other visible block. The outer
`#place` therefore reported `PositionedContentFlowFallback` and returned without
trying its atomic fallback: introspection survived, but the visible region did
not, and no placed-region drop was recorded.

The place planner now distinguishes introspection scaffolding from rendered
blocks. Tags remain in document order, but a tag-only body proceeds to the
whole-region raster attempt. If that also produces no anchorable drawing, the
planner records `Drop/PositionedContentUnavailable` and emits a source warning
through the shared terminal-region helper.

The real 120 x 80 mm fixture renders `VISIBLE PLACED BODY` in the PDF's 60 mm
content region. Its deliberately failing DOCX callback leaves only `After` in
the Word body; the package now truthfully reports both the nested
`LayoutCallbackUnavailable` and enclosing `PositionedContentUnavailable`
regions, with `drop="2"`, rather than claiming an approximate flowed result.

## 31. Failed inline placement is distinct from empty semantic scaffolding

The paragraph-child path for inline `#place` and `#box(place(..))` correctly
kept harvested state/counter tags in document order, but an empty raster result
still conflated two different outcomes: intentional tag-only scaffolding and a
failed layout attempt that lost visible content.

Fallback layout now returns explicit failure provenance alongside its optional
frame/raster. Inline lowering preserves the tags in both cases, but records
`Drop/InlinePositionedContentUnavailable` only when the layout attempt actually
errored or panicked. A state-only `box(place(layout(..state.update..)))` remains
an intentional semantic operation and does not acquire a false drop.

The real 120 x 80 mm fixture places a height-sensitive body inside an inline
box. Typst PDF contains `VISIBLE INLINE BODY`; the deliberately degraded DOCX
retains `Before` and `After`, emits the source warning, retains the
`FallbackLayout/Error`, and embeds `drop="1"` with
`InlinePositionedContentUnavailable`.

## 32. Snapshot-owned bibliography authority

Bibliography packaging previously queried the DOCX introspector twice after
snapshot construction: once for the lossless BibLaTeX sidecar and once for the
selected Hayagriva entries mapped into Word's `b:Sources` schema. Those parallel
late queries could disagree with the paged visual reference or with each other.

`ExportSnapshot` now captures the paged document's owned BibLaTeX payload and
ordered selected entries before DOCX lowering. Every entry has a stable logical
ID derived from its key and canonical snapshot payload; those IDs participate
in the document snapshot hash. Both `word/typstBibliography.xml` and
`customXml/item1.xml` now derive exclusively from the snapshot, and the fidelity
manifest publishes each entry ID/key pair.

The real fixture cites `beta` before `alpha` while the source file lists
`alpha` first. PDF retains visible `[1]`/`[2]` citation order, the snapshot and
manifest contain stable `alpha`/`beta` entry identities, Word Source Manager
contains exactly the same two tags, and the lossless sidecar contains both
source records. Repeated compilation produces identical entry IDs. A document
without a bibliography has empty snapshot facts and emits neither package view.

## 33. Positioned native canvases preserve flow and source-space geometry

A one-page `badformer` game scene exposed two independent false-native claims.
First, every floating shape used a normal line-height anchor paragraph; 275
legal anchors therefore consumed six pages of document flow. Anchor-only
paragraphs now use an exact one-twip line. Second, native line/curve path
normalization retained width and height but discarded the source-space minimum
coordinate, placing every explicit-endpoint line at `(0, 0)`. `Drawing` now
carries that normalized source offset into its final `wp:positionH/V` anchor.

The same document also uses `set page(fill: black)`. Word's native
`w:background` is retained, and a full-page native DrawingML rectangle is added
behind the header as a compatibility branch for consumers such as headless
LibreOffice that do not print Page Color. No raster media is added.

Real LibreOffice evidence improved from six pages and visual score `0.014820`
to one page and `0.984571`, while all 275 scene/UI drawings remain native and
editable. The full 179-test DOCX suite passes, including exact source-origin,
collapsed-anchor-paragraph, and solid-page-fill compatibility assertions.

Microsoft Word exposed one further boundary that schema/package validation did
not: four decorative lines used billion-EMU source geometry, and the first such
line made Word reject the entire file. Falling through to the generic raster
path attempted an enormous allocation and was killed. Native path extraction
now compresses only axes outside a conservative ±100,000,000 EMU point range,
anchored at the edge nearest the page origin. This keeps visible geometry in
place, preserves editability, and bounds Word's parser input. The fidelity
manifest honestly records these as `Approximate` / `WordCoordinateBound` with
visual-only loss. Focused evidence is one page in both Word and LibreOffice,
with LibreOffice score `0.983091`, 275 drawings, four approximations, zero
rasters, and zero drops.

### 33.1 Large relative raster compatibility branch

Correctly resolving a large width-relative PNG against its current container
exposed a LibreOffice layout loop in `how-to-use-typst-for-paper-ja`: one
842×845 screenshot displayed at about 293×294pt consumed more than four CPU
minutes, while its older oversized extent rendered in roughly two seconds.
Changing DPI metadata, resampling pixels, VML, smaller extents, and horizontal
tiling did not help. The failure depended on one tall inline box.

The final enrollment is deliberately narrow: a raster must have source width
at least 75% relative, intrinsic width and height between 700 and 1000 pixels,
near-square aspect (0.95–1.05), and resolved height between 280 and 310pt. The
exact single DrawingML picture then lives in a `w15` `mc:Choice`. The
compatibility fallback references
the same relationship twice, cropping top and bottom halves into consecutive
bands whose combined dimensions and pixels equal the source picture. Modern
Word therefore keeps one exact editable picture; LibreOffice and older
consumers avoid the pathological box. Only the first band owns alt text and the
second is decorative. Fidelity records `NativeWithFallback` /
`LibreOfficeImageLayoutFallback` with unified-image editability loss.

Focused evidence: LibreOffice conversion completes in about 2.4 seconds,
visual score `0.885345`, and 55 pages (v4 was `0.886529` and 55 pages). Word
opens without repair and selects the modern branch, though its 95-page layout
against a 23-page Typst reference remains a separate pagination defect.
The full v8 corpus enrolls exactly one document/region in this fallback and
returns to the five pre-existing LibreOffice timeouts.

### 33.2 Page overlays retain their logical canvas

`render_frame_to_png(crop_to_ink: false)` still constructed its initial canvas
from the frame's ink bounds. A page background made only from a positioned 1pt
border therefore became a 2-pixel-wide PNG, which `page_overlay_block` then
stretched across the full sheet. `agregyst` rendered as large black/gray page
regions even though the Typst reference was white with a thin border.

Page overlays now expand the laid-out frame to the full logical page box and
use `render_full_frame_to_png`. Ordinary fallback images retain ink cropping.
A regression asserts that a 100pt × 80pt page overlay produces a 200×160 PNG
at the existing 2 px/pt resolution rather than 2×160. Focused LibreOffice
evidence keeps two pages and improves from `0.355880` to `0.944054`, passing
visual policy. Word opens without repair but produces three pages, leaving a
separate one-page consumer-pagination difference.

## 34. Snapshot link edges and stable internal bookmark names

Internal hyperlinks previously called `add_bookmark(Location)` during lowering,
which assigned `_Ref1`, `_Ref2`, and so on in conversion order. The location and
name therefore belonged to the DOCX realization rather than the paged semantic
target, and unrelated earlier allocations could rename every later edge.

`ExportSnapshot` now resolves `LinkElem` destinations against the converged
paged introspector before lowering. Each edge owns a stable source ID and either
an external URL, paged position, or target semantic-node ID; edge identities and
occurrence counts participate in the document snapshot hash and are serialized
in the fidelity manifest. For DOCX locations matched to snapshot nodes,
`add_bookmark` uses `_Typst` plus the target node's 128-bit ID. Unmatched
generated locations retain the sequential compatibility fallback.

The real fixture contains one label link and one external URL. PDF exposes both
link texts; DOCX uses the same `_Typst…` value for `w:bookmarkStart/@w:name` and
`w:hyperlink/@w:anchor`, its relationship targets `https://example.com` in
external mode, and the manifest publishes one node edge and one URL edge.
Repeated compilation produces identical link facts and bookmark names.

## 35. Paged-authoritative page counters and TOC caches

TOC page caches previously called `Counter::display_at` after DOCX lowering.
That replayed custom numbering functions under `Target::Docx`, even when the
PDF had already evaluated them successfully under `Target::Paged`. A function
could therefore produce the correct PDF value but fail during DOCX export,
forcing a `BestEffort` placeholder and a consumer refresh.

Each snapshot node now owns resolved page-counter displays for its matching
paged occurrences. Evaluation runs through a temporary engine whose
introspector is the converged `PagedIntrospector` and whose target style is
explicitly `Paged`; page patterns, updates, and resets are therefore captured
from the visual reference universe. Counter facts participate in the snapshot
hash and appear beneath their semantic node in the fidelity manifest.

TOC cache planning first consults those facts. The real two-page fixture uses a
numbering closure that deliberately errors under `Target::Docx`, roman `i` on
the first page, then switches to Arabic and resets to `1`. PDF and the DOCX TOC
both show `i`/`1`; the manifest records the same values, and the two `PAGEREF`
fields are `Resolved` rather than `BestEffort`. Non-page semantic counters and
pre-enrolled fallback regions remain snapshot migration work.

## 36. Terminal drops distinguish source whitespace from visible text

`record_content_drop` previously counted raw `Content::plain_text()` characters.
For a multiline visual body such as `place(rotate(line(..)))`, that includes the
newlines and indentation surrounding the shape. An unsupported gradient stroke
could therefore become a false `content_loss` result even though no visible text
existed. Drop accounting now trims only the outer source-layout whitespace; real
internal spaces in visible text remain counted.

The shape-only preflight also accepts inert spaces and paragraph separators, so
a multiline rotated solid line reaches the existing native transformed-shape
mapper and emits an anchored DrawingML custom geometry instead of raster media.
An unsupported gradient-stroked variant remains an explicit visual/semantic
drop, but reports zero affected text characters.

A focused rebuild of the three former corpus text-loss records—`storytiles`,
`smorad-um_cisc_7026`, and `vnckey-book-rs`—now classifies all three as
`unverified`, with zero `reported_text_drop` results. Their remaining capability
losses and missing consumer/font/license evidence stay visible.

## 37. Landscape page boundaries avoid double pagination

Run-level `<w:br w:type="page"/>` is not idempotent. When fixed-height slide
content already fills a Word page, the trailing break lands at the top of the
next physical page and advances again, producing a blank page between slides.
For landscape/slide-shaped sections, page boundaries now move onto the following
paragraph as native `<w:pageBreakBefore/>`. If Word has already auto-paginated,
the property leaves the paragraph on that page; otherwise it starts the intended
new page. Non-landscape flowing documents retain their existing break-run path.

The `sleiden-lei` public fixture improves from 19 LibreOffice pages for 10 PDF
pages to 12 pages. The remaining two pages are the independently visible logo
rows from its two fixed-height title stacks; attempted exact-row and whole-stack
raster branches were rejected because LibreOffice either repaginated the row or
failed to paint the body image. The retained fix therefore removes nine proven
blank pages without hiding the remaining degradation.

Image extents also resolve explicit percentage width/height against the current
DOCX container instead of always falling back to intrinsic pixels. A synthetic
200 x 100 pt page verifies that `image(height: 50%)` emits a 50 pt native SVG
extent with its PNG compatibility branch.

## 38. Section page colors do not leak or manufacture slide headers

Word's `w:background` is document-global. A dark cover followed by ordinary
white sections therefore stayed dark even though Typst had reset the page fill.
When existing sections disagree on solid page color, DOCX now omits the global
color and retains section-specific behind-text shapes only for colored runs. A
white/unfilled section following a colored section gets an empty header part to
break Word's header inheritance; an initially white section gets no unnecessary
part. This distinction matters for slide decks with dozens of sections.

`humble-dtu-thesis` improves from `0.403043` to `0.961059` in LibreOffice while
remaining 12 reference pages versus 13 consumer pages. Word opens without
repair and reports 14 pages. `algorithmlecturenotes` improves from `0.351669`
to `0.809159` without changing its separate 20-to-32-page pagination drift.

## 39. Unsupported math raster fallback starts page-bounded

Raster fallback normally lays content out at infinite height so tall figures
are captured whole. For equation-containing regions, that can retain a
document-absolute vertical position: a small border remains at the origin while
the actual equation lands tens of thousands of transparent pixels below it.
The resulting PNGs reached 750 million pixels and drawing extents up to 5.89
billion EMU.

Equation-containing fallbacks now start at the real page height. A secondary
ink guard retries any other infinite-height frame whose geometric axis exceeds
8,000pt, and refuses to render it if the bounded retry is still pathological.
The three affected equation-heavy fixtures now have maximum extents of 10.69M,
70.73M, and 58.15M EMU instead of 5.89B, 157.09M, and 1.38B. Their focused
LibreOffice scores remain high (`0.978066`, `0.961428`, and `0.952929`), and the
largest tutorial exports in 41.18 seconds with an optimized build. A structural
regression places unsupported math after 9,000pt of prior document space and
asserts that its raster extent remains below 100M EMU.

## 40. Page background rasters include the solid page-fill canvas

Typst paints `page(background:)` above `page(fill:)`. Word can retain the solid
fill as native `w:background`, but LibreOffice reverses the relative z-order of
two behind-text header drawings: the compatibility fill rectangle covered the
rasterized background artwork. A successful background raster now composites
the solid fill into its own canvas and suppresses only that competing rectangle;
if rasterization produces nothing, the rectangle remains as the fallback.

Sixteen v11 documents contain this layer combination. The focused
`codealchemy24-resourcebook` result remains one page with full text coverage and
improves from `0.560334` to `0.990632`. A structural regression retains native
`w:background`, one raster background anchor, and no competing Page Color
anchor.

## 41. Auto-sized raster images follow Typst's bounded natural size

For an image with neither width nor height specified, Typst starts from the
pixel/DPI natural size and proportionally bounds it by both axes of the current
layout region. DOCX previously emitted the unbounded intrinsic dimensions. Wide
screenshots therefore consumed whole extra Word pages even though Typst had
scaled them to the text width.

Native raster-image extents now apply the same proportional bound while leaving
explicit width/height behavior unchanged. `besarabegor-discord-guide` keeps all
245 extracted words and 17 native drawings, but improves from 25 pages at
`0.598571` to the reference nine pages at `0.941477`. A synthetic 100 x 400 PNG
on a 200pt square page verifies a 50pt x 200pt native picture extent, proving
that the height bound is applied as well as the width bound. A rejected
width-only counterfactual expanded that picture to 200pt x 800pt and let the
consumer clip most of it.

## 42. Consecutive page breaks and whole-page vertical alignment

A page-style transition is represented by a Word section boundary. Typst's
realized stream contains both the synthetic transition break and any explicit
`pagebreak()` calls at that point. The section splitter previously consumed all
of them, collapsing multiple requested blank pages into one. The first actual
break and the synthetic transition are now represented by the section boundary;
every further consecutive break is retained inside the new section. A focused
three-page fixture verifies one section boundary plus one native page break.

When every non-tag element in a page run resolves to the same vertical
`align(..)` component, `horizon` and `bottom` now map to section-level
`w:vAlign="center"` and `w:vAlign="bottom"`. Mixed page runs keep Word's default
top alignment. This keeps the content native and editable rather than replacing
a designed title page with a raster.

The `kdl-unofficial-template` fixture recovers its second requested blank page:
LibreOffice moves from 10 to 11 pages against Typst's 13, with unchanged
`0.995331` text coverage, a valid package, and a successful 104-region round
trip. LibreOffice retains but does not visually apply `w:vAlign`, so its score
changes only from `0.630572` to `0.630916`. Microsoft Word opens without repair,
applies the native vertical centering on the cover, and reports 12 pages. The
remaining consumer-specific pagination drift stays classified as degraded.

## 43. License-permitted fonts are embedded for portable text metrics

DOCX previously declared referenced font families in `fontTable.xml` but never
stored their programs. A consumer without the Typst font substituted another
family, changing glyph widths and line breaks even when the exporter preserved
the exact paged geometry. The exporter now embeds the regular, bold, italic,
and bold-italic faces it can resolve as deterministic obfuscated OpenType font
parts, relates them from `fontTable.xml`, and records the family as embedded in
the fidelity report. It honors the font's OpenType `OS/2.fsType`: restricted,
preview-and-print-only, bitmap-only, and collection faces remain portable font
references rather than being embedded. Skipping preview-and-print-only programs
preserves the exporter's editable-document contract.

A package regression checks the `w:embedRegular` relationship, `w:fontKey`,
obfuscated-font content type, non-plain stored bytes, and the reversible
ECMA-376 XOR. All 203 DOCX tests pass. In a dated 2026-07-13 three-column
fixture, LibreOffice previously substituted Liberation Serif and wrapped
`Cell A`/`Cell B` inside a 765-twip nested table. The embedded export uses
Libertinus Serif and keeps both cells on one line at the same authored width;
OMML, highlight, external/internal links, bookmark, and outer column geometry
remain native. The consumer still makes those automatic rows taller than
Typst, which is tracked as a separate row-metrics fidelity defect rather than
hidden by widening the table.

## 44. Measured table-row minima do not count cell insets twice

Typst's tagged physical cell height already includes the cell's top and bottom
insets. Word interprets `w:trHeight` as a content-height minimum and then adds
`w:tcMar`, so emitting the full tagged height as `atLeast` counted those insets
twice. Measured rows now subtract the largest non-spanning cell's vertical
insets from the emitted minimum while retaining the full height as the reference
for percentage inset resolution. Explicit fixed row heights are unchanged.

The 2026-07-13 embedded-font fixture now emits a 145-twip expandable minimum
for a 345-twip physical row with 100-twip top and bottom margins. LibreOffice
reduces the two-row table from roughly 120 to 100 pixels without wrapping,
clipping, or disturbing outer columns and rich content. Typst remains roughly
74 pixels because Writer's editable text line box is taller than Typst's glyph
frame; that remaining consumer-metric difference is not concealed with an
exact/clipping row height.

## 45. Trailing semantic tags do not create a second table-cell line

Typst introspection tags survive lowering as internal `Block::Tag` markers but
serialize to no WordprocessingML. The table terminator previously looked only
at the final IR block, so a real paragraph followed by a tag gained another
empty `<w:p>` merely to satisfy Word's requirement that every cell end in a
paragraph. A footnote-only cell therefore occupied two editable line boxes.
The terminator now ignores non-serializing tags when locating the last emitted
block. A package regression verifies one native footnote reference in exactly
one cell paragraph.

In the 2026-07-13 mixed-font stress fixture, LibreOffice reduces the table from
roughly 173 to 142 pixels while preserving the native footnote and expandable
rows. The first row still wraps against Typst's single-line layout. Temporary
50-twip and zero-twip horizontal-margin probes showed that only removing all
padding stopped both wraps, at the cost of text touching the cell border, so no
consumer-specific margin fudge was retained.

## 46. Explicit header/footer horizontal alignment reaches Word paragraphs

A top-level `align(center)`/`align(right)` around page furniture is consumed as
a block-layout wrapper while the isolated header/footer fragment is realized.
Its synthesized paragraph therefore missed the wrapper's style chain and fell
back to Word's left alignment. Furniture lowering now preserves an explicit
outer horizontal alignment on each otherwise-unset top-level paragraph.

Focused package tests cover centered headers and right-aligned footers. In the
mixed-font visual fixture, LibreOffice moves the header from the left body
margin to Typst's horizontal center and centers the footer within a few pixels.
Their remaining vertical offsets (about 21 rendered pixels in opposite
directions) are tracked separately as header/footer band-metric drift. All 206
DOCX integration tests pass and the exporter remains clippy-clean.

## 47. Single-line page furniture uses Word's content-start band distance

Typst defines `header-ascent`/`footer-descent` against a marginal layout region:
headers are bottom-aligned within the top band and footers are top-aligned
within the bottom band. Word's `w:header`/`w:footer` instead measure from the
page edge to the start of the content. Emitting Typst's band boundary directly
therefore shifted both simple lines inward by one line height. For furniture
that lowers to exactly one native paragraph with no forced break, drawing,
math, or field, the exporter now subtracts the largest explicit run size from
the band boundary. Tags, bookmarks, hyperlinks, and semantic strong/emphasis
runs remain eligible; complex and multiline furniture keeps the conservative
boundary.

A focused regression maps a default 11pt line in a 0.5in margin from the
504-twip Typst boundary to a 284-twip Word content-start distance. In the
mixed-font fixture, LibreOffice moves the rich centered header from y≈57 to
y≈35 against Typst y≈36, and the footer from y≈684 to y≈706 against Typst
y≈705, without clipping or moving the body/table. A two-line fixture remains at
504 twips. All 207 DOCX integration tests pass and clippy remains clean.

## 48. List item spacing belongs to the list, not its first paragraph

Typst's list layouter owns the vertical gutter between item frames. Re-realizing
an item body can nevertheless put the surrounding `par.spacing` on its first
paragraph; carrying that directly into Word placed the full gap both before and
after only the first marker. Nested bullets consequently jumped away from their
parent while later siblings stayed tight. Marker-bearing native-numbering and
static-marker paragraphs now discard only before/after values equal to the
inherited paragraph spacing, retaining explicit line-height and other paragraph
properties. A top-level list keeps the normal paragraph gap once, on its final
paragraph, so Word can collapse it with the following block. Nested lists do not
gain that outer boundary gap.

The package regression covers a nested bullet group followed by an enum: the
first/nested/first-enum paragraphs carry no leaked before/after spacing, while
the final paragraph of each top-level group carries the 264-twip default
boundary. In the 2026-07-13 LibreOffice fixture, bullet baselines improve from
roughly 115/169/196 pixels to 89/115/141 against Typst 83/113/143, with native
`w:numPr` and level indents unchanged. LibreOffice renders the following
list-to-list gap about 11 pixels larger than Typst; a package-only 160--180-twip
consumer calibration scores closer, but the exporter retains the authored
264-twip paragraph boundary instead of hard-coding a LibreOffice-only fudge.
All 208 DOCX integration tests pass and clippy remains clean.

## 49. Figure captions retain the figure body's centered alignment

The native figure mapper already centers each in-flow body paragraph, matching
Typst's figure show rule, but emitted the editable caption with only Word's
`Caption` style. That built-in style does not imply horizontal centering, so a
centered image opened with its caption at the left body margin. Caption
paragraphs now carry both the semantic `Caption` style and direct centered
paragraph alignment; SEQ numbering, bookmarks, internal references, and the
editable caption runs are unchanged.

The package regression checks `w:pStyle="Caption"` and `w:jc="center"` on the
same SEQ-bearing paragraph. In the 2026-07-13 image fixture, LibreOffice places
the image and caption at the same x center (about 450 rendered pixels), with the
caption baseline within one pixel of Typst. The image geometry and linked
`See Figure 1.` reference remain unchanged. All 208 DOCX integration tests pass
and clippy remains clean.

## 50. A page break immediately after block columns becomes the restore-section break

Native `#columns(..)` uses a continuous multi-column section followed by an
empty restore-to-page-columns section. An explicit `#pagebreak()` immediately
after the columns therefore became the leading child of that empty section;
the ordinary converter correctly drops section-leading page setup, but in this
case that also erased the authored break. Section resolution now recognizes
that exact boundary and changes the transition out of the column section from
`continuous` to `nextPage` (or the requested odd/even parity). Additional
consecutive breaks remain explicit page breaks inside the restored section, so
blank-page intent is preserved.

The package regression verifies a two-column `w:sectPr`, a following
single-column section with `w:type="nextPage"`, and one surviving `w:br` when
two consecutive breaks are authored. In the 2026-07-13 fixture, LibreOffice
now produces the same two pages as Typst: the short first-page paragraph wraps
to four lines at the same half-page column width, and the following heading
starts at the top of page two. All 209 DOCX integration tests pass and clippy
remains clean.

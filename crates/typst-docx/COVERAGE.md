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
   context) into a `Caption`-styled paragraph.

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
region is no longer dead pixels: the text is searchable (Word Find), selectable,
copy-pasteable, screen-reader accessible (both the hidden runs and the image alt
text), and indexable. The hidden block is bracketed with hidden spaces so its
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
search/accessibility). The residue that "can't become text" now does.

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

## 10. The synthetic page model — clearing the paged-introspection tail

The remaining EXPORT_ERR class was templates that read *paged* introspection a
flowing document doesn't have. Three mechanisms, found by root-causing each of
the failing corpus docs individually (the first two hypotheses — anchor
leniency for unknown locations — turned out to be redundant: the shared
`ElementIntrospector` already treats an unknown `before()`/`count_before()`
anchor as end-of-document; both attempted overrides were removed after an
ablation confirmed they weren't load-bearing):

### 10a. Synthetic page numbers (`DocxIntrospector::set_page_model`)
`page()`/`pages()`/`page_numbering()` returned `None`, so
`@target(form: "page")` and `loc.page-numbering()` hard-failed the export.
Now a walk over the lowered IR (mirroring `collect_tags`) counts explicit
page breaks (`Run::PageBreak`) and section breaks, assigning every tag
location a (page, section) pair; `page_numbering` resolves against that
section's real `set page(numbering:)`. The numbers are exact for
break-structured front matter and a lower bound where text auto-flows —
and most of them surface as *cached field values* that Word recomputes live.
Locations outside the model (rasterize-deferred, header/footer tags) resolve
to the final page, consistent with their append-at-end position. Recovered:
shuosc-shu-bachelor-thesis, splines-thesis-starter. Corpus-wide the oracle
flagged ~20 theses whose TOC/ref page numbers changed — every one previously
showed a flat wrong "1" for all pages; the synthetic values are strictly
closer to the PDF (and respect roman front-matter numbering).

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
Remaining tail: tracl (template's own `target` branch — theirs to fix),
sos-ugent-style (the `drafting` package's own realize-time panic),
toffee-tufte (a citation inside margin-note content is convergence-unstable),
ijimai (a show-rule use-count assertion our sub-realizations distort — the
count moved from 10 to 0 with these changes, still not 1). Each is a bespoke
package-interaction dive, documented here so the next pass starts from the
diagnosis.

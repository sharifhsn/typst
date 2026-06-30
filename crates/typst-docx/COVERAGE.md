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

## 5. Where genuinely-new findings can still come from

The survey and the rasterize-recovery axes are exhausted (everything above).
Remaining signal is **content correctness**, found with `docx_oracle.py pdf`
(refs/cites/nums vs the PDF gold — noisy, read the per-token detail not the
score):

- cross-reference / caption **numbers** in recovered native content (figures,
  tables, equations, sections, citations);
- OMML math fidelity on uncommon constructs;
- table edge cases (merges, nesting, alignment) in recovered `#grid`/`#stack`.

# Typst ↔ DOCX Support Matrix

Comprehensive, source-grounded coverage of the two Word-interop crates:

| Direction | Crate | Path |
|---|---|---|
| **Export** — Typst → DOCX | `typst-docx` | `crates/typst-docx` |
| **Import** — DOCX → Typst | `typst-docx-import` | `crates/typst-docx-import` |

Every row below traces to actual code (not documentation or intent). Enumerated 2026-07-20, and **updated after both gap-closing passes** described in the changelog at the end, against branch `codex/office-export` (which now carries all the Office work — export, import, and PowerPoint — on one branch). A `typst-docx-roundtrip` crate exercises the composition of the two.

## Legend

Each feature carries one rating per direction. Ratings are direction-specific: "Full" export + "None" import is common and expected.

| Symbol | Meaning |
|---|---|
| ✅ **Full** | Emitted as / recovered as a faithful, native, editable construct |
| ◐ **Partial** | Mapped but lossy or approximate — the Notes say exactly what is lost |
| 🖼 **Raster** | *(export only)* Rendered to an embedded image; visually faithful, not editable |
| ⊘ **Dropped** | Recognized, then discarded — either content or a formatting attribute |
| ✗ **None** | No code path touches it; silently ignored / falls through |
| ⛔ **Refused** | *(import only)* Aborts with an error — the document safety boundary |
| — | Not applicable to that direction (the construct has no counterpart) |

A cell may combine a symbol for the *mechanism* with a note for the *loss*. "None" and "Dropped" differ: **None** never had a code path; **Dropped** is a deliberate decision to unwrap/discard.

---

## 1. Document structure

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Page size | ✅ | ✅ | Export derives from the converged paged geometry (`auto` axis → real section size, else A4); import `twip → abs`. |
| Margins (4 body) | ✅ | ✅ | |
| Header/footer edge margins | ✅ | ✅ | Import converts `w:pgMar@header/@footer` (page-edge origin) to Typst's `header-ascent`/`footer-descent` (body-side origin) assuming a single line — the exact inverse of the exporter's own `adjust_furniture_band`. Skipped when the result lands within 1pt of Typst's 30% default. |
| Orientation / landscape | ✅ | ✅ | Export derives from `PageElem::flipped` axis swap. |
| Gutter / mirrored (book) margins | ◐ | ✅ | Export folds document-wide (OOXML has no per-section mirror). Import: `w:mirrorMargins` → `margin: (inside:, outside:)`, the spelling that swaps on facing pages; `w:gutter` folds into the binding-side margin, since Typst has no separate gutter. |
| Text columns | ✅ | ✅ | Export emits `w:cols` num/space/equalWidth incl. mid-flow `#columns` wrapped in continuous sections; import maps **count only** (equal width assumed, per-column widths dropped). |
| Column balancing | ✅ | — | Export sets `w:noColumnBalance`. |
| Sections (multi-section) | ✅ | ✅ | Export derives boundaries from a change in size/orientation/margins/columns/gutter/numbering/line-numbers/header-band **or a content-hash diff**; import splits `w:body` at each `w:sectPr`. |
| Section break type (continuous / nextPage / odd / even) | ✅ | ◐ | Import: `nextColumn` treated as `nextPage`; a continuous break that changes column count forces a page break (reported). |
| Page-number format (`w:pgNumType@fmt`) | ✅ | ◐ | Import maps decimal / lower-upper roman / lower-upper letter; other formats (ordinal, cardinalText…) unmapped → inherits prior. |
| **Page-number restart (`@start`)** | ✅ | ✅ | Round-trips in both directions (recent work — export `w:pgNumType@start` ⇄ import `#counter(page).update(n)`). |
| Page break | ✅ | ✅ | Import drops page/column breaks **inside a container** (table cell / textbox / footnote / header) and reports it. |
| Column break `#colbreak` | ✅ | ✅ | |
| `pagebreak(to: odd/even)` | ✅ | ✅ | |

## 2. Headers & footers

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Explicit header/footer content (paras, tables, images, fields) | ✅ | ✅ | Import namespaces rIds per part to avoid cross-part collision; visually-empty furniture dropped. |
| default / even / odd / first variants | ✅ | ✅ | Import: even/odd gated on `settings.xml w:evenAndOddHeaders`; first-page gated on `w:titlePg`. Export probes pages 1–5 only when content is context-dependent. |
| Suppressed band | ✅ | — | Export: `Smart::Custom(None)` → suppressed. |
| Page-varying furniture (page 3 ≠ page 5) | ◐ | — | Export samples the page-1 furniture and repeats it (reported `PAGE_FURNITURE_SAMPLED`). Word cannot express arbitrary per-page furniture. |
| Synthetic page-number band | ✅ | — | Export synthesizes a page-number paragraph when numbering is set and the band is left `auto`. |
| Tab-stop three-column header layout | ✅ | ◐ | Export emits `w:tabs` with leaders; import collapses `w:tab`/`w:ptab` to a plain tab character (reported). |

## 3. Paragraph formatting

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Alignment (`w:jc`) | ✅ | ✅ | Export flips logical alignment under RTL. |
| Justification | ✅ | ✅ | |
| Indentation (left / right / first-line / hanging) | ✅ | ✅ | Import emits left/right via `#pad` and first-line/hanging via `#par`. |
| Spacing before / after | ✅ | ✅ | Import parses both; the majority pair is hoisted into `#set par(spacing:)` and only deviating paragraphs carry a `#block(above:, below:)`. |
| Line spacing / leading | ✅ | ◐ | Import ignores `w:lineRule` (auto/atLeast/exact), reads twips as absolute leading. |
| keep-with-next / keep-lines | ✅ | ◐ | Import: `w:keepLines` → `#block(breakable: false)`. `w:keepNext` has no Typst counterpart — nothing binds a block to its successor — so it is reported rather than dropped silently. |
| page-break-before | ✅ | ✅ | Import via the break path. |
| Contextual spacing | ✅ | — | |
| Tab stops (+ leader) | ✅ | ◐ | Import → plain tab char. |
| Paragraph shading / background | ✅ | ✅ | Import wraps the paragraph in `#block(fill:, width: 100%)`. |
| Paragraph borders | ✅ (per-side) | ✅ | Export is the only construct that expresses non-uniform strokes. Import reads all four `w:pBdr` sides into `#block(stroke:)`, with `@w:space` as the inset — except the lone-bottom-border-on-an-empty-paragraph idiom, which stays a `#line` rule because that is what it looks like. |

## 4. Run / character formatting

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Bold | ✅ | ✅ | Export picks `Strong` style vs direct `w:b` by provenance; import promotes to `*..*` in idiomatic tier. |
| Italic | ✅ | ✅ | Idiomatic import → `_.._`. |
| Underline | ✅ | ✅ | Both carry color and pattern; import maps `w:u` to a real `#underline(stroke:)` (dash/thickness/paint). Double and wavy have no Typst dash — drawn solid, reported. |
| Strike | ✅ | ✅ | Export: boolean (strike's own color/style dropped). |
| Double-strike (`w:dstrike`) | — | ◐ | Typst has no double strike; import draws a single `#strike` and reports it. |
| Super / subscript | ✅ | ✅ | |
| Text color | ✅ | ✅ | Export: a solid paint is `w:color`; a **gradient** is `w14:textFill/w14:gradFill` with the real stop list, *always* alongside a flat first-stop `w:color` — the w14 element is MCE-ignorable, so a consumer that skips it must still see a sensible colour rather than black (verified: LibreOffice renders the first stop). A tiling text fill has no Word primitive and is reported. Import: solid hex, `auto` → none. |
| Highlight | ✅/◐ | ✅ | Import maps the 16 named colors through the **exporter's own table**, so a marker round-trips to the RGB it started with. |
| Font family | ✅ | ◐ | Export splits runs by per-char font coverage (mirrors shaping) and fills ascii/hAnsi/cs/eastAsia; import reads `@ascii` only. |
| Font size | ✅ | ✅ | Export hoists the common size to docDefaults. |
| Letter-spacing / tracking | ✅ | ✅ | Import: run `w:spacing` → `#text(tracking:)`. |
| Baseline shift | ✅ | — | Import expresses only super/sub via `vertAlign`. |
| Small caps | ✅ | ✅ | |
| All caps (`w:caps`) | ✅ | ✅ | Import: `w:caps` → `#upper`. |
| RTL / bidi | ✅ | ⊘ (by design) | Export emits `w:rtl`/`w:cs`/`w:bidi`. Import deliberately drops the override: Typst resolves bidi from the Unicode text itself, and forcing `dir` onto an inline span panicked Typst's shaper on a real corpus document. |
| Language (`w:lang`) | ✅ | ✅ | Import hoists the document default to `#set text(lang:, region:)` and reduces it off runs. Tags Typst would reject (script subtags, numeric regions) lose the offending half rather than failing the compile. |
| Vanish / hidden | ⊘ | ⊘ | Both drop — for different reasons. Export: `#hide[..]` physically drops frames (redaction; deliberately **not** `w:vanish`, which would leak text). Import: `w:vanish` runs emit nothing (matches Word's non-display). |
| noProof (code/raw) | ✅ | — | |

## 5. Styles

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Paragraph styles | ✅ | ✅ | Export derives Heading-N + a full built-in gallery (Title, Quote, Caption, TOC1–9, Bibliography…). Import resolves effective props. |
| Character styles | ✅ | ✅ | Import layers para-style-run → char-style → direct formatting. |
| `basedOn` resolution | ✅ | ✅ | Import: root-first chain, cycle-safe, cap 32 hops. |
| docDefaults | ✅ | ✅ | Export derives by length-weighted majority vote over body prose; import parses `w:docDefaults`. |
| Linked styles (`w:link`) | ✅ | ✗ | Export pairs each heading char style via `w:link`; import resolves para and char styles independently. |

## 6. Headings & outline

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Heading → styled paragraph | ✅ | ✅ | Export → `w:pStyle="Heading{n}"`; import determines heading-ness from style outlineLvl (via basedOn chain) or a `"Heading N"` name match. |
| Outline level | ✅ | ✅ | Export `w:outlineLvl = level-1` (clamped 8). Import maps levels 1–6 to `=`…`======`, clamps 7–9 down, and treats `outlineLvl=9` as a body-text sentinel (never a heading). |
| keepNext on headings | ✅ | — | |
| Heading numbering | ✅/◐ | ✅ | Export emits live `w:numPr` on the `HeadingN` styles, but only after **replaying Word's numbering** over the real heading sequence and confirming every number matches Typst's; a moved counter, an unmappable numeral system, or a `STYLEREF` caption prefix makes it decline and keep frozen text. Import reads the style-linked `w:numPr` back into `#set heading(numbering:)`. |
| Cross-ref anchor / bookmark | ✅ | ✅ | Export brackets headings in `w:bookmarkStart/End`. Import lowers each `w:bookmarkStart` to a Typst label, hoisted to the end of its block (Typst labels attach backwards); a second bookmark on the same paragraph gets a `#metadata(none)` anchor of its own, since Typst allows one label per element. A label nothing ever emitted is downgraded by the `resolve_labels` pass rather than left dangling. |

## 7. Lists / numbering

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Bullet lists | ✅ | ✅ | Export generates a 9-level `w:abstractNum`; import classifies via `numFmt`. |
| Custom bullet markers | ✅ | ◐ | Export now reads `ListElem::marker` and emits the glyph as `w:lvlText` (non-text markers keep the conventional glyph, reported). Import still emits `-`. |
| Ordered / enum lists | ✅ | ✅ | Import maps per-level `numFmt` into one Typst pattern (`"1.a.i."`), scoped to the list. A level with no Typst counterpart abandons the pattern rather than half-translating it. |
| `start:` | ✅ | ✅ | Import reads `w:start` and the instance's `w:startOverride` (kept per-instance so sibling lists sharing an `abstractNum` don't inherit each other's start). |
| Nesting / levels | ✅ | ✅ | Both cap at 9 levels; import indents 2 spaces/level. |
| `reversed:` / explicit `number:` / closure numbering | ◐ | — | Export bakes the marker as static literal text (won't renumber in Word); import n/a. |
| Numbering restart / `lvlOverride` | ✅ | ✅ | See `start:` above. |
| Term / definition lists | ◐ | — | Export emulates with a bold term + hanging-indent description (no native Word construct). Import: a Word "definition list" arrives as plain paragraphs. |

## 8. Tables

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Grid + column widths | ✅ | ✅ | Export uses measured paged geometry (median) or resolves widths; import maps `w:tblGrid` (zero-width → `auto`). |
| colspan (`w:gridSpan`) | ✅ | ✅ | Import pads short rows to the widest row. |
| **rowspan / vertical merge (`w:vMerge`)** | ✅ | ✅ | Import resolves merge runs into real rowspans via a grid-occupancy walk, dropping the covered cells and discounting the spanned columns when padding short rows. |
| Per-side cell strokes | ✅ | ✅ | Import maps `w:tcBorders` per side; a side Word never stated is left unset so the table's own stroke shows through. |
| Table-level borders | ◐ | ✅ | Export hardcodes a blanket `w:tblBorders` (real fidelity rides per-cell). Import maps it to `table(stroke:)` — which matters most for the `nil` case: a deliberately borderless Word table used to arrive wearing Typst's default 1pt grid. Word states six edges where Typst takes one stroke, so on disagreement the *interior* one wins (it decides how a table reads) and the reconciliation is reported. |
| Stroke thickness / color / dash | ✅/◐ | ✅ | Export emits exact `a:custDash` run lengths, keeping `a:prstDash` only where a preset matches exactly. DrawingML has no dash offset, so a phase that isn't on an even run boundary is dropped (the pattern survives). Import reads `a:ln`'s width, colour and both dash spellings back, and distinguishes an absent `a:ln` from an explicit `a:noFill`. |
| Cell fill / shading | ✅ | ✅ | Export: solid full, gradient → mean-of-stops, tiling dropped. Import: `w:shd@fill` → `table.cell(fill:)`. |
| Horizontal cell alignment | ✅ | ✅ | Comes from the cell's own paragraphs' `w:jc`, through the ordinary paragraph path. |
| Vertical cell alignment | ✅ | ✅ | Import: `w:vAlign` → `table.cell(align:)`. |
| Cell inset / padding | ✅ (per-cell) | ✅ | Import: `w:tcMar` → `table.cell(inset:)`. |
| Header rows | ✅ | ✅ | Import puts every *leading* header row into one `table.header`. |
| Footer rows (`table.footer`) | ⊘ | — | Export renders as ordinary rows (Word has no repeat-at-bottom); flagged approximate. |
| Nested tables | ✅ | ✅ | Import cap depth 24 (DoS guard). |
| Table layout | ◐ | — | Export always emits `w:tblLayout="fixed"`. |
| Table alignment / indent | ✅ | ✅ | Export sources `w:jc` from the style chain and emits `w:tblInd`. Import reads both, resolved against each other the way Word does: an indent is ignored once the table is centred or right-aligned, so only one is ever emitted. |
| Row heights / cantSplit | ✅ | ◐ | Import maps `w:trHeight hRule="exact"` to a `rows:` track size. `atLeast` — Word's default — is a *minimum*, and a Typst track is exactly its stated size, so honoring it would clip any row whose content outgrew Word's floor; those stay content-sized and are reported. `w:cantSplit` has no per-row Typst counterpart and is likewise reported. |

## 9. Images

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| PNG / JPEG / GIF | ✅ | ✅ | Export embeds **verbatim, no re-encode**, deduped by hash; import extracts to `assets/`. |
| WebP | 🖼 | ✅ | Export rasterizes to PNG; import supports it natively. |
| SVG (+ svgz) | ✅ | ✅ | Export emits native Office `asvg:svgBlip` **plus** a required PNG fallback slot; import sniffs and keeps SVG. |
| PDF as image | 🖼 | — | Export rasterizes; import: PDF is not a DOCX image format. |
| BMP / TIFF | — | ⊘ | Typst cannot load these, so no export source; import drops them per-image (unsupported) and continues. |
| **EMF / WMF metafiles** | — | ⊘ | Neither direction supports metafiles. Import drops them per-image (no Rust GDI decoder; documented in README). In one 2,459-doc corpus these were **41% of all media parts**. |
| Format sniffing (lying extensions) | — | ✅ | Import overrides the declared extension by magic bytes (SVG by text sniff). |
| Sizing (EMU / DPI) | ✅ | ✅ | |
| Inline positioning | ✅ | ✅ | An image is inline content in Typst's model, Word's (`w:drawing` lives inside a `w:r`) and Pandoc's alike, so the DOCX/Pandoc targets now group it into its paragraph rather than letting it interrupt one. Before that, a lone image erased the boundary with whatever followed — realize discards the `ParbreakElem` between two blocks — and a run of figures collapsed into a single `w:p` holding every caption. |
| Floating / anchored | ✅ | ◐ | Export → `wp:anchor` (align/offset/wrap). Import keeps the *named* placement (`#align`) but deliberately not the absolute offset or wrap: Word's offsets are page-relative and Typst's `#place` reserves no space, so emitting them would overlap body text. |
| Alt text | ✅ | ✅ | |
| Cropping / corner clip (`a:srcRect`) | ✅ | ✅ | Export emits native `roundRect` + `a:srcRect` (ported from the pptx sibling) instead of rasterizing. Import inverts both: the radius becomes a clipping `#box(radius:)`, and — since Typst's `image` has no crop parameter — the crop becomes geometry, oversizing the image to `W / (1 − l − r)` inside that box and `#place`-ing it by the hidden band so the box doesn't grow. |
| Image inside hyperlink / field | ✅ | ⊘ | Import v1 simplification: skips drawings inside hyperlink/field runs. |

## 10. Math (OMML)

Export walks the resolved `MathItem` IR and emits `m:` OMML directly; a whole equation rasterizes if **any** descendant is unsupported (never a silent partial subtree). Import tokenizes OMML into Typst math source.

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Inline equation | ✅ | ✅ | |
| Block / display | ✅ | ✅ | Import routes `m:oMathPara` to a block equation; an equation sharing its paragraph with prose stays inline. |
| Equation number | ◐ | — | Export: right tab stop + inline runs (+ optional bookmark), no first-class OMML number. |
| Symbols / identifiers / numbers | ✅ | ✅ | Export bakes italic/variant into Plane-1 codepoints + `m:nor`. Import folds Plane-1 math-alphanumerics to base letters, space-separates adjacent letters, reports style loss. |
| Operators / function names | ✅ | ✅ | |
| Fractions (bar / no-bar / skewed) | ✅ | ✅ | Import: skewed → linear. |
| Radicals (sqrt / nth-root) | ✅ | ✅ | |
| Scripts (sup / sub / subsup / pre) | ✅ | ✅ | |
| Over / under limits | ✅ | ✅ | |
| N-ary (∑ ∏ ∫ …) | ✅ | ✅ | Import keeps an unknown operator as a literal symbol + reports. |
| Delimiters / fences / `lr` | ✅ | ✅ | Both escape syntax-significant fences; one-sided fence drops `lr`. |
| Matrices / cases | ✅ | ✅ | Export flattens intra-cell alignment points into one `m:e`; import handles `m:m` / `m:eqArr` / `{`-delimited. |
| Multiline gather / align (`&`) | ✅ / ◐ | — | Export: gather → `m:eqArr`, align → borderless matrix (per-column align approximated). |
| Accents / bars / vectors | ✅ | ✅ | Import: unknown accent → `hat` + reports. |
| Colored math | ◐ | ◐ | Export: solid non-black only (gradients dropped). Import: color/variant styles folded away + reported. |
| Variant styles (bold / blackboard / fraktur / mathrm) | ✅ | ◐ | Export via codepoint remapping (no `m:sty`). Import folds to base letter + reports. |
| `box()` in math | 🖼 | — | Export rasterizes the whole equation; import unwraps `m:box`. |
| Phantom | — | ◐ | Import shows the content (spacing-only invisibility not simulated). |
| `func` (function apply) | ✗ | ✅ | Export juxtaposes upright runs (no `m:func`); import converts `m:func`. |
| Unrecognized / malformed OMML | — | ◐ / ⊘ | Import recurses keeping text (never raw-XML-splices); a malformed equation is dropped, and the parser can excise one bad `m:oMath` to save the document. Depth bomb (>64) flattened to text. |

## 11. Fields

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| PAGE / NUMPAGES | ✅ | ✅ | Export emits live complex fields; import → `#context counter(page).display()` / `.final()`. |
| TOC / list-of-figures/tables | ✅ | ✅ | Export writes a real `TOC` field wrapped in a `w:sdt` content control (entries baked, `updateFields` deliberately omitted). Import → `#outline()` (falls back to cached text when inside a heading). |
| Cross-reference REF / PAGEREF | ✅ | ✅/◐ | Import resolves both against imported bookmarks: `PAGEREF` becomes a genuinely live `counter(page).at(<label>)`, `REF` becomes a live `#link(<label>)` keeping Word's cached text (Typst has no "content of the bookmarked range"). |
| HYPERLINK | ✅ | ✅ | |
| Figure number SEQ | ✅ / ◐ | ◐ | Export: `SEQ` or `STYLEREF`+scoped SEQ, non-equivalent patterns → Typst text + hidden `SEQ`. Import: cached result text. |
| DATE / TIME / STYLEREF / SEQ / other | ✅ (static via core.xml) | ◐ | Import lowers every other field via a generic fallback to its cached result text, reported once per field type. |
| Field cache / ownership model | ✅ | — | Export tracks `FieldMode::Live/Static` + `FieldOwner` + `FieldCacheStatus` in the report. |

## 12. Footnotes & endnotes

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Footnote reference + body | ✅ | ✅ | Export writes `word/footnotes.xml` with its own rels part; import inlines `#footnote[body]` at the reference. |
| Endnotes | ✗ | ◐ | Typst has no endnote element. Export routes all notes to footnotes.xml. Import now leaves a superscript mark in place and collects the bodies, numbered, at the document's end — Word's own placement — rather than scattering them across page feet. |
| Re-reference (`#footnote(<lbl>)` / NOTEREF) | ◐ | — | Export → static `NOTEREF` (avoids renumbering). |
| Nested footnotes | ◐ | ⊘ | Export flattens (nested `w:footnoteReference` is illegal); import drops on cycle/depth. |
| Separators / continuation | ✅ | ⊘ | Export writes separator ids −1/0; import skips the boilerplate note furniture. |

## 13. Hyperlinks, bookmarks, cross-references

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| External hyperlink | ✅ | ✅ | Import: missing target → text kept unlinked + reported. |
| Internal hyperlink (anchor) | ✅ | ✅ | Import resolves the anchor to the imported label → `#link(<label>)`. |
| Bookmarks | ✅ | ✅ | Import emits Typst labels, hoisted to the end of their block (Word writes bookmarks at the start, where a label would attach to the *previous* block). Names are sanitised, `_GoBack` skipped, collisions and duplicates deduped, and extra labels on one block get their own `#metadata(none)` anchor since Typst allows one label per element. |
| Cross-ref to page number | ✅ | ◐ | Export emits a live `PAGEREF` with a Typst-owned supplement; import gives cached text. |
| Positional link (`#link(page/x/y)`) | ◐ / ⊘ | — | Export → per-page synthetic bookmark when possible, else text + flag. |

## 14. Bibliography & citations

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| In-body citation / bibliography text | ✅ | — | **Major asymmetry.** Export keeps fully-realized CSL-formatted output with clickable back-references. Import: a `w:sdt` bibliography is unwrapped and survives only as its **last-rendered plain text**. |
| Native Word Source Manager (`b:Sources`) | ✅ | ✅ | Export writes real `b:Sources/b:Source` in `customXml` (hayagriva ~29 → Word 17 source types, coarse) + a deterministic-GUID datastore. **Import inverts it**: the store — found by *content*, since Word numbers `customXml/itemN.xml` by insertion order — becomes a hayagriva `bibliography.yml` sidecar emitted as an asset, and each `CITATION` field becomes a live `#cite(<b:Tag>)`. The seventeen-into-thirty collapse cannot be undone, so each Word type maps to the hayagriva type it most often came from: **95.4% correct** across 15,241 entries from 1,213 real `.bib` files, with the loss concentrated in `Report`, a near coin-flip between report and thesis (313 vs 261). A Word-authored document does better still, since Word uses types like `JournalArticle` that map back exactly. A citation naming a tag with no matching source keeps Word's cached text — a `#cite` pointing at nothing fails the whole compile. The sidecar is written only when something actually cites it (452 corpus documents carry a store; only **17 hold any sources**). |
| Lossless BibLaTeX sidecar | ✅ | — | Export writes an inert private-namespace `word/typstBibliography.xml` for external tools. |

## 15. Shapes & drawing

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| rect / roundrect | ✅ native | ◐ | Export → `a:prstGeom`. Import reads both spellings: DrawingML `a:prstGeom` → `#rect` with the `roundRect` adjustment recovered as a real `radius:`, and legacy VML → `#rect` with the radius lost. **Position is dropped** either way (drawn inline). |
| circle / ellipse | ✅ native | ◐ | Import from DrawingML `ellipse`/`circle` presets or VML `v:oval`; position dropped. |
| polygon / path / `#curve` | ✅ native | ◐ | Export → `a:custGeom` (moveTo/lnTo/cubicBezTo/close, 1:1). Import inverts exactly that, command for command, into `#curve` — scaling the path's own `a:path@w/@h` coordinate space to the shape's extent. Legacy VML's `v:path`/`v:formulas` mini-language is still declined (it is a different, far messier grammar). |
| line | ✅ native | ◐ | Import keeps length only, direction discarded. Export: horizontal rule → paragraph bottom border instead. |
| Framed box / text box (box or rect with text body) | ✅ native | ◐ | Export → editable `wps:txbx` + legacy VML fallback via `mc:AlternateContent`; declined (→ raster) for footnote/no-frame/centered-figure bodies. Import inlines the content at the anchor, drops position/size + reports. |
| WordArt (`v:textpath`) | — | ◐ | Import → plain text (curved/warped path styling dropped). |
| Grouped shapes | ✅ (wpg group) | ◐ | Import recurses (cap 32), preserves multiple text boxes, drops group geometry. |
| Preset / custom geometries (stars, callouts, connectors) | ✅ (many via custGeom) | ◐ | Anything the exporter wrote as `a:custGeom` comes back as `#curve`. A *named* preset with no Typst counterpart (a star, a callout) still falls back to its bounding `#rect` and is reported — Word states those as a name plus adjustment guides, not as a path. |
| Shape stroke / fill | ✅ (solid + linear gradient) | ◐ | Import reads `a:solidFill`/`a:gradFill` with `a:alpha` folded into the fourth channel, and `a:ln` with either dash spelling. Still `#rrggbb`-only: a theme colour lives in `theme1.xml`, which this importer does not resolve, and such a shape is reported rather than painted a guessed colour. |
| OLE objects (`w:object`) | ✗ | ◐ | Export has no source construct. Import cannot revive an embedded application, but Word renders a **preview picture** beside every embedding, so that is kept (the whole element used to fall through the run parser, taking the preview with it) and the payload is reported *by name* from `o:OLEObject/@ProgID` — "an embedded Excel.Sheet.12 object…". Measured on the wide corpus: 61 documents, 121 embeddings, top producers Equation.3 / Package / Excel.Sheet.12. **95.9% of those previews are EMF/WMF**, which Typst cannot decode, so today the picture itself only lands for the PNG minority; the naming is what carries the other 95.9%. |
| Drop shadow | ✗ | — | No shadow path in DOCX export (distinct box shadows fall to generic raster). |

## 16. Charts & plots

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Native chart object (`c:chart`) | ✗ | — | Typst has no chart element; export never emits `chartSpace`. |
| Typst plot-package output (cetz-plot, lilaq…) | ✅ vector / 🖼 raster | — | Reaches the exporter as `#curve`/`#line`/`#polygon`/`#rect`+text → native shape paths when representable; a whole diagram in one drawing callback → one raster image. |
| DOCX chart → data table | — | ✅ (default) | **Asymmetry.** Import default: `#figure(table(..), caption:)`. The plot itself is not drawn. |
| DOCX chart → plot (opt-in) | — | ◐ | Opt-in `charts: Plot` → `lilaq` (Bar/Line/Scatter/Area only; area fill lost; non-numeric → falls back to table). Adds a pinned `@preview/lilaq:0.6.0` import. |
| Extended charts (`cx:chart`, box-whisker/sunburst/waterfall) | — | ◐ | Import → table; hierarchical category axis keeps finest level only; prefers a fallback picture when Word supplies one. |

## 17. Content controls & wrappers

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| `w:sdt` structured document tags | ✅ (TOC / review regions) | ⊘ | Export uses SDTs for the TOC and optional review-tag regions. Import **unwraps** to `w:sdtContent` — **content preserved**, wrapper semantics dropped (the crate's headline content-loss fix). |
| `mc:AlternateContent` | ✅ (compat tiles / textbox fallback) | ⊘ | Import takes the first honored `mc:Choice` else `mc:Fallback` (prefers the fallback picture for `cx` charts). |
| `w:smartTag` | — | ⊘ | Import unwraps, keeps runs. |
| Ruby / furigana (`w:ruby`) | — | ✅ | Import → an on-demand `#let ruby(base, gloss)` helper (place-above). Reading-less ruby → plain base text. |

## 18. Tracked changes & comments

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Insertions (`w:ins` / `w:moveTo`) | — | ✅ | Rendered as accepted (the text is live) in **both** modes. Under the default `Preserve` the record survives beside it as two invisible anchors bracketing the inserted words — `<ins-N>` … `<ins-N-end>` — carrying author and timestamp, plus the `w:name` that links the two halves of a *move*. Export's "review tags" (`w:sdt` regions) are a different, opt-in concept. |
| Deletions (`w:del` / `w:moveFrom`) | — | ✅ | Renders as accepted — the text is gone from the page — but under `Preserve` the removed runs are lowered **into the anchor's own value**, since there is no live content to bracket. `w:delText` is read only from inside a kept `w:del`, so it can never leak into the live text. A wholly-deleted paragraph keeps its record rather than being dropped as empty: a paragraph holding only `#metadata` renders pixel-identically to no paragraph at all. |
| Format & paragraph-mark changes (`w:rPrChange`, `w:pPrChange`, `w:tblPrChange`, …) | — | ⊘ | Detected and reported, not mapped. These record what the formatting *used to be*, which would need a serialized mirror of Word's run/paragraph properties for something nothing in Typst consumes; a paragraph-mark change is the paragraph boundary itself rather than inline content, so it has nowhere to hang an anchor. Measured on the wide corpus: `w:ins`/`w:del` appear in 71/80 documents, these in 23/18. |
| Comments (`w:comment` + ranges) | ✅ | ✅ | **Round-trips.** Export finds the comment `#metadata` in the tag stream — `Tag::Start` carries the real element content — allocates its own `w:id`, writes `word/comments.xml` with the body lowered through the same machinery footnote bodies use, and brackets the span with `w:commentRangeStart`/`End` + a `w:commentReference` run. Span-vs-point is decided by asking the introspector whether a matching `-end` anchor exists anywhere, never inferred from ordering. A `#metadata` that isn't a comment passes through completely untouched. Import lowers each comment to a labelled **`#metadata`** — the one Typst element that is invisible, carries an arbitrary value, and stays reachable via `#query`. So a comment reaches neither the page nor the bin: rendered output is identical to the comment-free import (verified: the comment text appears 0 times in the rendered PDF), while `#query(<comment-N>)` returns author / initials / date / body, the body kept as **content** so its own formatting survives. A commented *span* becomes two anchors (`<comment-N>` … `<comment-N-end>`) because a Typst label attaches to one element; a point-anchored comment — Word omits the range pair — carries its payload on the `w:commentReference` mark instead. `w:annotationRef` is suppressed like `w:footnoteRef`. A dangling anchor is reported. **Anchors on a heading are hoisted to their own block just before it**: a label binds to the element it follows *except* at the end of a heading, where it binds to the heading instead and the record becomes unreachable (lists, paragraphs and table cells all bind correctly and keep their anchors in place). |

## 19. Colors, fills, gradients, strokes

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| RGB / CMYK / luma / oklab | ✅ | ✅ | Export composites everything to `[u8;3]` hex; import reads rgb hex. |
| Alpha / opacity | ⊘ | — | Export composites translucent colors onto white (Word has no alpha primitive). |
| Linear gradient (shapes) | ✅ native | ✅ | Export → `a:gradFill` (stops sampled); import reads the stop list and `a:lin@ang` back into `gradient.linear`. |
| Radial gradient | ✅ | ✅ | Export emits `a:gradFill` with a reparameterised stop list; import recovers it from `a:path`'s `a:fillToRect`. **Conic** still rasterizes in both directions — no OOXML path sweeps by angle. |
| Gradient text / paragraph / cell fill | ✅ / ◐ | ✗ | Export: **text is now native** (`w14:textFill`, sharing `dml::gradient_fill`'s Oklab stop sampling and angle/focus maths with the shape exporter, so the two cannot drift); block/highlight → first-stop shade; cells → mean-of-stops. |
| Tiling / pattern fill | 🖼 / ⊘ | ✗ | Export: shapes → rasterized PNG tile; cells dropped; **text dropped but now reported** (`tiling text fill`) — no Word primitive tiles glyphs. |
| Solid stroke | ✅ | ◐ | Export → `a:ln` (dash → nearest preset); import: VML hex only. |

## 20. Boxes, blocks, containers *(Typst-source constructs — export-centric)*

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| `#block` (solid fill) | ✅ | ✅ | Export: fixed-height ≤60% page → one-cell table, else shaded/bordered paragraphs. Import maps `w:shd@fill` to `#block(fill:, width: 100%)` — full width, because Word's shading spans the text column rather than hugging the glyphs. |
| `#block` (closure / layouter body) | 🖼 | — | Rasterized with hidden searchable runs. |
| `#box` (framed, with body) | ✅ | ◐ | See §15 text box. Import inlines the body. |
| Radius (rounded corners) | ⊘ (text) / ✅ (bare shape) | — | Shaded-paragraph/textbox corner radius silently lost; a bare shape → `roundRect`. |
| Clip | 🖼 | — | Rasterized (no native clip). |
| Drop shadow | ✗ | — | Not represented. |
| `#pad` | ✅ (horizontal) | — | Vertical padding dropped. |

## 21. Document metadata

| Feature | Export T→D | Import D→T | Notes |
|---|---|---|---|
| Title / author(s) / keywords | ✅ | ✅ | Import reads `docProps/core.xml` → `#set document(..)`, splitting authors on `;` (the separator the exporter writes) and keywords on `,`. `dc:description` has no Typst counterpart and is reported. |
| Dates | ✅ (static) | ✅ | Import maps `dcterms:created` → `datetime(..)`; an out-of-range timestamp is dropped rather than failing the compile. |

## 22. Robustness & safety *(import boundary; export runs a conformance gate)*

Import enforced by the shared OPC reader (`typst-ooxml-core::opc`), surfaced as `ImportError`.

| Condition | Import D→T | Notes |
|---|---|---|
| Not a Word doc (no `word/document.xml`) | ⛔ | `NotAWordDocument`. |
| Malformed `word/document.xml` | ⛔ | One repair attempt, then `Xml` — the only part with no safe fallback. |
| Malformed companion parts (styles/numbering/settings/headers/notes/charts) | degraded | Default + report; never aborts the whole doc. Repair fixes duplicate attributes or excises one bad `m:oMath`. |
| XML bomb (deep nesting) | ⛔ | `max_xml_depth=256` + per-construct caps: tables 24, wrappers 32, textboxes 16, fields 64, VML groups 32, math 64, notes 8. |
| XXE (`<!DOCTYPE>` / `<!ENTITY>`) | ⛔ | Any XML part carrying these is rejected. |
| Zip bomb / oversized | ⛔ | 128 MiB archive / 64 MiB part / 256 MiB expanded / 8192 entries. |
| Corrupt zip | ⛔ | |
| Non-Word OOXML (LibreOffice / Google Docs / WPS) | ✅ | By design — producer-agnostic local-name element matching throughout. |

**Export conformance gate:** before writing, `invariants::validate` checks bookmark uniqueness and field-cache consistency, then `schema::validate_package` checks WordprocessingML child-order (pPr/rPr lead, tblGrid before rows, cell terminal paragraph, sectPr terminates body, mc:Choice before Fallback).

---

## Loss taxonomies

### Import — `ImportReport` (`report.rs`)

Two severities: **Approximate** ("mapped, detail lost") and **Drop** ("content dropped"), deduplicated by `(severity, what, detail)` so each construct reports once. Labels emitted: OMML equation, image, chart, footnote, endnote, text box, WordArt, VML shape, VML line, hyperlink, internal hyperlink, `field {TYPE}`, header/footer, header/footer tab stops, keep with next, embedded object, comment, table borders, table row, table row height, table indent, continuous section, page/column break, plus part-name-keyed malformed-XML drops.

### Export — `FidelityReport` (`report.rs`)

- **Representation** (5): `Native`, `NativeWithFallback`, `Approximate`, `Raster`, `Drop`.
- **LossSet** — 6 boolean dimensions: visual_fidelity, semantic_structure, editability, dynamic_behavior, accessibility, portability. Presets: RASTER, DROP, LINK_TARGET, DYNAMIC_BEHAVIOR, VISUAL_ONLY, PLAIN_TEXT_FALLBACK, PAGE_FURNITURE_SAMPLED, SECTION_GEOMETRY_ONLY, MATH_TEXT.
- **DecisionReason** (~40, non-exhaustive): raster fallbacks, math raster, page-overlay raster, dense-visual/placed-canvas raster, SVG-with-PNG, positional-link target, Typst-owned reference/figure text, field-cache-unavailable, native-page-reference, section-geometry fallback, positioned-content fallbacks, page-furniture-sampled, table-geometry approximation, Word-coordinate-bound, LibreOffice-image-layout, and more.
- Also records SuppressedDiagnostic (caught panics/errors), DynamicFieldFact, FontFact (embedded / available), DrawingAccessibilityFact (alt / decorative / unlabeled).

## Opt-in options & flags

**Import (`opts.rs`):**
- `tier` — `Literal` | `Idiomatic` (**default Idiomatic**): tier-2 passes collapse redundant `#text`, promote `*`/`_`, hoist justification.
- `charts` — `Table` | `Plot` (**default Table**): Plot draws via `lilaq` and adds a pinned package import.
- `assets_dir` — **default `"assets"`**.
- `tracked` — `Preserve` | `Accept` (**default Preserve**): both render the accepted view; `Preserve` additionally keeps each revision's record as invisible `#metadata`. Preserve is the default because it is a strict information superset at *zero* visual cost, and discarding authored text silently is the one thing this importer tries never to do.

**Export (`DocxOptions`):**
- `pretty` — pretty-print XML.
- `embed_fidelity_manifest` — **OFF by default**; when on, persists the versioned manifest to `customXml` + `docProps/custom.xml`. The in-memory report is always computed.
- Review round-trip: `docx_with_review_tags` wraps regions in `w:sdt`; plain `docx` is byte-identical.
- Font embedding: gated on OS/2 embedding permission; embedded as obfuscated `.odttf` parts (skips TTCs and preview-only faces).

## Test & corpus gates

**Import:**
- `corpus.py` — routine gate on **Apache POI's 128 documents**, measuring text coverage vs a LibreOffice render.
- `wide_corpus.py` — breadth gate on **~2,500 documents** (16 producers / 60 languages): does it import, does the output compile.
- `template_conformance.py` — organizational-template property conformance.

**Export:**
- `tools/docx-validate/` + a semantic smoke validator; `COVERAGE.md` is the author's four-tier historical ledger (a dated `bench/docx_batch.py` snapshot of 616/627 exports, 0 invalid packages — explicitly labeled historical, not a current figure).

> Live pass counts are computed by the scripts at run time; the repos commit the corpus sizes and metrics, not fixed thresholds.

---

## Round-trip asymmetries worth knowing

The two directions are **not** inverses. Most of the gaps that used to matter
have since been closed (see the changelog below); what remains:

**Export-rich, import-blind** — export writes it, import cannot read it back:
- Linked styles (`w:link`) — cosmetic: import resolves paragraph and character
  styles independently, which loses the pairing but no formatting. The only
  one left.

**Import-capable, export-absent** — import reads it, export has no counterpart:
- Word charts (`c:chart` / `cx:chart`) — import gives a table or an opt-in
  `lilaq` plot; Typst has no chart element to export.
- Endnotes — import collects them at the document's end; export has no endnote
  source at all.

**Approximate both ways** — mapped, but Typst has no property that means quite
the same thing: `w:cantSplit`, an `atLeast` row height, per-column widths, and
`w:keepNext`. Each is reported rather than silently dropped.

**Symmetric** — round-trips in both directions: core prose and character
formatting (including highlight, caps, tracking, language, underline pattern),
paragraph indents/spacing/shading **and borders**, sections, page geometry
(including **mirrored margins, the binding gutter, and header/footer band
distances**), columns, page-number restarts, `w:keepLines`, lists (format,
start, and authored bullet glyphs), tables (grid, colspan, **rowspan**,
per-side and **table-level** borders, alignment and indent, inset, exact row
heights, header rows), footnotes, hyperlinks (external **and** internal),
bookmarks and cross-references, images (raster + SVG, with **corner clip and
crop**), **DrawingML shapes** (preset and custom geometry, gradients, alpha,
dashes), document metadata, and the full OMML math structure set.

**Neither direction** — EMF/WMF metafiles, and tracked changes as visible
markup (see the changelog for why). An OLE object's *preview* now imports, but
its payload cannot round-trip in either direction.

---

## Changelog — gap-closing pass

Import gained: run formatting (highlight/caps/tracking/language/underline
stroke/double-strike), the full paragraph indent and spacing model plus
shading, document metadata, list numbering formats and starts, table
`vMerge`→rowspan with borders/alignment/inset/multi-row headers, block
equations, bookmarks with live internal links and cross-references, endnotes
collected at the document end, and named float alignment.

Export gained: table alignment and `w:tblInd`, custom bullet markers, native
image corner clip, radial gradients, shape fill alpha, and rounded text-box
corners.

Three things were deliberately **not** done, each for a stated reason:

- **Drop shadows** (export) — Typst has no shadow property on this branch, so
  there is nothing in the realized tree to carry.
- **`STYLEREF`-prefixed captions** (export) — a document using a composite
  `heading.n` caption prefix keeps frozen heading numbers, because converting
  the field to `STYLEREF "Heading N" \w` could not be verified against real
  Word from here. Zero regression; a follow-up needing a real-Word check.
- **Absolute float offsets and text wrap** (import) — Word's offsets are
  page-relative and Typst's `#place` reserves no space, so emitting them would
  overlap body text; true wrap needs a third-party package.
- **Tracked changes as visible markup** — `typst-docx` has no `w:ins`/`w:del`
  path and does not handle `MetadataElem`, so a Typst-side `#ins`/`#del`
  helper would leave no trace for the exporter to find. Round-tripping them
  would need a new cross-crate marker protocol, so import keeps its current
  behaviour: insertions accepted, deletions dropped.

Three real defects were caught by the corpora during this work and fixed:
non-ISO language tags failing the compile outright, a fully-merged table row
emitting a bare `,`, and an inline `dir: rtl` override panicking Typst's
shaper.

### Second pass — the remaining round-trip asymmetries

Everything the exporter already wrote but the importer could not read back.
Each was a guaranteed round-trip loss, and each had its XML shape pinned down
in advance by the code that emits it.

Import gained: DrawingML shapes (`a:custGeom` → `#curve` command-for-command,
presets → `#rect`/`#circle`/`#line`/`#polygon`, with `a:gradFill`, `a:alpha`
and `a:custDash`), image crops (`a:srcRect`), table alignment and indent,
authored bullet glyphs, **table-level borders**, **exact row heights**,
**all four paragraph border sides**, **`w:keepLines`**, **header/footer band
distances**, and **mirrored margins with the binding gutter**. A picture
paragraph's own `w:jc` now places the figure, so a centred image no longer
arrives flush left.

Export gained one structural fix in the same pass: a bare `#image()` followed
by its own caption paragraph used to emit a **single** `w:p` holding both, and
a run of figures collapsed into one paragraph carrying every caption. The
cause was upstream of `typst-docx` — `ImageElem` has no show rule outside the
paged/HTML targets, so it reached paragraph grouping raw and *interrupted*
rather than joining, which erased the only signal distinguishing "lone image,
then a separate block" from "one paragraph split around an inline image".
Treating an image as inline in the grouper (exactly as `LinkElem`/`RefElem`/
`FootnoteElem` already are, and for the same stated reason) fixes both shapes
at the root: three figures now emit six `w:p`, while `Text #image(..) more`
still emits one. The golden-reference suite is untouched by this — paged and
HTML both register their own `IMAGE_RULE`, so an image never reaches the
grouper raw there.

Two Word-verification defects were found by opening the round-tripped files in
real Word — neither of which any automated gate caught, because both tests
exercised the *function* rather than the document shape: a custom bullet
marker read off the style chain instead of the element, and adjacent lists
merged across a `w:numId` change, which silently renumbered roman lists to
arabic.

Still open, and deliberately so:

- **Bibliography round-trip** — export writes real `b:Sources` and `CITATION`
  fields; import parses neither back into `#cite`/`#bibliography`. This needs
  an inverse of the hayagriva → Word-17-types mapping plus a sidecar `.yml`,
  and the direction it would run in is the lossy one.
- **OLE objects (`w:object`)** — neither direction. An embedded application
  cannot be revived, but these almost always carry a `v:shape` preview image,
  so importing *that* would beat today's silent nothing.
- **`w:cantSplit`, `atLeast` row heights, per-column widths, `w:keepNext`** —
  each reported rather than mapped, because Typst has no property that means
  the same thing. See the rows above for the individual reasoning.
- **Comments, conic gradients, gradient text fill** — genuinely blocked in at
  least one direction; conic has no OOXML path that sweeps by angle, and Typst
  has no comment construct for the exporter to find.

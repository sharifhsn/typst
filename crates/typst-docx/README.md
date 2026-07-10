# typst-docx

Native Word (`.docx`) export for Typst. It walks the realized Typst element tree
under a `Target::Docx` and emits idiomatic Office Open XML — real headings,
tables, lists, OMML math, footnotes, fields, and styles — rather than a flat
rendering. Output opens cleanly in Microsoft Word and LibreOffice.

> **Status: experimental / preview.** Like Typst's HTML export, this is a preview
> feature. Structural content (text, tables, math, lists, references) maps to
> native, editable OOXML; graphics that have no OOXML equivalent fall back to an
> embedded image. See the support tables below for exactly what maps where.

## Clean, restylable output

The exporter aims for the document you'd get if a careful person had built it in
Word — not a flattened rendering. It uses Word's **built-in styles** (`Heading 1`,
`List Paragraph`, `Quote`, `Caption`, `Hyperlink`, `TOC 1`–`9`, `Bibliography`),
so the Navigation pane, Styles gallery, and "update style" all work. The
document's predominant **font, size, and language are hoisted into `docDefaults`**
and the body inherits them — each run's formatting carries only what *deviates*
(bold, a different size, a colour). Editing the `Normal` style (or the theme font)
in Word therefore restyles the whole document, and `document.xml` stays compact.
Heading-run deduplication follows Word's `HeadingN -> Normal -> docDefaults`
cascade: a direct deviation equal to Normal is retained when HeadingN defines a
different value, so editing or reopening the document cannot change its meaning.
This makes the docx pleasant both for a human to tweak in Word's GUI and for a
tool to regenerate by editing the (far more compact) Typst source.

The package is also structured like one Word itself saves: it ships the standard
`theme1.xml`, `fontTable.xml` and `webSettings.xml`, a full `settings.xml` with
the modern compatibility block (so Word opens it natively, not in "Compatibility
Mode"), and wraps newer constructs such as text boxes in `mc:AlternateContent`
with a legacy fallback for older consumers.

Shared OPC finalization validates unique and legal part names, content-type
consistency, relationship owners, and every internal relationship target before
writing. Parts and overrides are canonicalized for byte-deterministic output,
and ZIP failures propagate as export diagnostics instead of panicking.
Immediately before that format-neutral finalizer consumes the package, DOCX
parses every accumulated XML part and enforces repair-sensitive WordprocessingML
sequences: paragraph/run properties lead their containers, table properties and
grids precede rows, cells retain an editable terminal paragraph, section
properties terminate the body, and `mc:Choice` branches precede their fallback.
This is a focused consumer-safety gate, not a claim of complete ECMA-376 XSD
validation.

## How content is mapped

Every element falls into one of four user-visible tiers:

| | Tier | Meaning |
|---|---|---|
| ✅ | **Native** | Real, editable OOXML (text runs, `w:tbl`, OMML, fields, …). |
| ⚠️ | **Approximate** | Editable OOXML with a known visual or behavioral difference. |
| 🖼️ | **Rasterized** | Embedded PNG. The visual is preserved exactly, but it is not editable. Used **only** when no OOXML construct can carry the content. |
| ❌ | **Unsupported** | Dropped, with a warning. Reserved for things with no flowing-document equivalent. |

Rasterization is a genuine last resort: it is bounded to unsupported fills,
PDF/WebP images, transforms, and external drawing packages (CeTZ, fletcher, …).
SVG is embedded natively with a PNG compatibility fallback. Everything
structural stays native. Cross-references and labels **inside** rasterized
regions are still harvested, so `@ref` to them resolves and figure numbering
stays consistent.

### Text & inline formatting

| Feature | | Maps to |
|---|:--:|---|
| Bold, italic | ✅ | `w:b`, `w:i` |
| Underline (incl. colour + dash style) | ✅ | `w:u` |
| Strikethrough | ✅ | `w:strike` |
| Superscript / subscript | ✅ | `w:vertAlign` |
| Highlight | ✅ | `w:highlight` / `w:shd` |
| Small caps | ✅ | `w:smallCaps` |
| Text colour, font family, size | ✅ | `w:color`, `w:rFonts`, `w:sz` |
| Language and RTL/CJK text | ✅ | `w:lang` with `w:eastAsia`/`w:bidi` script slots, `w:rtl`, `w:cs`, paragraph `w:bidi`, and direction-aware logical justification |
| Smart quotes | ✅ | resolved to curly quotes |
| `#hide[…]` | ✅ | content is **removed** from the file (redaction semantics, like PDF); only introspection traces (labels) are kept |

### Document structure

| Feature | | Notes |
|---|:--:|---|
| Paragraphs, line breaks, paragraph spacing | ✅ | incl. first-line indent |
| Paragraph break (`parbreak`) | ✅ | splits paragraphs; a line break in run-only contexts |
| Headings | ✅ | `Heading N` styles + numbering + bookmark |
| Bullet / numbered / nested lists | ✅ | `numbering.xml` (depth + full ancestry) |
| Term lists (`/ term: desc`) | ✅ | bold term + definition |
| Block quotes (+ attribution) | ✅ | `Quote` style; multi-paragraph quotes preserved |
| Code / raw blocks | ✅ | monospace + per-token syntax colours + line breaks |
| Outline / table of contents | ✅ | baked entries + page-number caches in a native `TOC` content control; updateable in Word without modal refresh-on-open prompts |
| Bibliography & citations | ✅ | native text + clickable back-references |
| Fixed vertical space (`#v(2cm)`) | ✅ | paragraph spacing when possible; otherwise an exact flow-space paragraph (for example between tables) |
| Vertical / horizontal stacks | ⚠️ | editable flow / borderless table; fixed gaps are exact, fractional gaps remain an explicitly reported width approximation |

### Tables

| Feature | | Notes |
|---|:--:|---|
| Tables | ✅ | `w:tbl` — borders, alignment, cell shading, merged cells, row heights; the CLI carries converged paged cell geometry into `w:tblGrid`, so axis-aligned auto/fractional/relative tracks are native when fully measured; unavailable geometry stays editable and explicitly approximate |
| Layout grids (`#grid`) | ✅ | also `w:tbl` (content stays editable); column and row gutters become physical spacer tracks |
| `stroke: none` cells | ✅ | explicit `w:val="nil"` |
| Gradient/translucent cell fills and non-solid border nuance | ⚠️ | native editable cells with a representative composited solid tone / solid border; the visual difference is recorded before lowering |

### Math (OMML)

| Feature | | |
|---|:--:|---|
| Inline & block equations, numbering | ✅ | `m:oMath` / `m:oMathPara` |
| Fractions, scripts, radicals, accents | ✅ | |
| Matrices, vectors, cases | ✅ | |
| n-ary / big operators (∑ ∫ ∏) | ✅ | |
| Multi-line `&` alignment | ✅ | right/left-justified matrix |
| Coloured math, upright/italic letters | ✅ | |
| Per-line equation labels | 🖼️ | layout-only anchors can't survive native extraction |
| Non-math content inside `$…$` | 🖼️ | whole-equation preflight selects one raster fallback; unsupported descendants are never omitted from partial OMML |

### Links, references & footnotes

| Feature | | |
|---|:--:|---|
| `#link(url)` | ✅ | external hyperlink (blue + underline) |
| `@ref` to heading / figure / equation / labelled element | ✅ | Typst-computed text in a clickable hyperlink → bookmark; Word cannot rewrite it as non-equivalent `REF` text |
| Footnotes | ✅ | `footnotes.xml` |
| `#link` to a page *coordinate* | ❌ | text kept, link dropped |
| `@ref` to a **page number** | ✅ | live `PAGEREF` field with Typst's fixed-point result as its cache; DOCX-only/fallback locations use a synthetic explicit-break model |

Word fields are live document objects, not frozen paint. The IR therefore marks
value ownership explicitly. Normal references are Typst-owned static hyperlinks;
page references and pagination fields are consumer-owned; a TOC is live only
when Word can reconstruct all of its entries (otherwise it is locked around the
baked result). The exporter deliberately omits document-wide `updateFields` and
per-field `dirty` flags because current Word presents disruptive external-field
and TOC dialogs on open. Baked results make first-open output complete, while
Word's normal **Update Table / Update Field** commands remain available after
the user edits the document.

### Figures, images & graphics

| Feature | | Notes |
|---|:--:|---|
| Figures (caption + cross-reference) | ✅ | equivalent single-component `1`/`a`/`A`/`i`/`I` numbering stays a live `SEQ`; richer Typst patterns/functions stay exact text plus a hidden Word counter |
| **PNG / JPEG / GIF** images | ✅ | embedded **verbatim** (no re-encode) |
| **SVG** images | ✅ | native SVG with a PNG compatibility fallback |
| **PDF / WebP** images | 🖼️ | rasterized to PNG |
| Drawing accessibility | ✅ | image `alt` becomes the Word description; text boxes expose native text; bodyless art and page backgrounds are explicitly decorative; unresolved non-decorative images are counted as unlabeled |
| Rect, square, circle, ellipse, polygon (solid **or linear-gradient** fill) | ✅ | **native vector** `wps:wsp` DrawingML — solid → `a:solidFill`, linear gradient → `a:gradFill` |
| Framed text boxes (`#box`/`#rect[text]`) | ✅ | editable `wps:txbx`, or flowing shaded paragraphs |
| Horizontal rules (`#line`) | ✅ | paragraph bottom border |
| Diagonal / endpoint `#line` | ✅ | native open `a:custGeom` path |
| `#curve` (straight + cubic-Bézier segments) | ✅ | native `a:custGeom` — `a:lnTo`/`a:cubicBezTo`/`a:close`, 1:1 with Typst's own Move/Line/Cubic/Close vocabulary |
| Stroke dash pattern + line cap (on the above) | ✅ | `a:prstDash` (approximated to the nearest OOXML preset) + `a:ln cap` |
| `#place(…)` around one representable drawing | ✅ | `wp:anchor` float |
| `#place(…)` around plain text | ✅ | editable/searchable `wps:txbx` in a `wp:anchor`, with source alignment and offsets |
| `#place(…)` around one simple text-only table/grid | ✅ | native editable `w:tbl` inside the anchored text box; validated in Word and Writer |
| `#place(…)` around richer mixed content | ⚠️ | lowered once as a whole region; native single drawings stay anchored, while unsupported mixtures flow in document order with an explicit `PositionedContentFlowFallback` report instead of silently losing content |
| Tiling / pattern fills | ✅ | DrawingML tile fill when the source can be represented |
| Radial / conic gradient fills | 🖼️ | OOXML's radial model cannot represent Typst's free center/radius exactly |
| Selected `#move`, rotation, and uniform scale on representable shapes/text | ✅ | transform is baked into native geometry or a Word text-position primitive |
| Skew, non-uniform scale, or mixed transformed content | 🖼️ | whole-region picture fallback |
| CeTZ / fletcher / canvas drawings, diagrams | 🖼️ | the main rasterize category — the individual shapes/lines/curves such a diagram draws are native *only* when they reach the exporter as standalone top-level elements; a diagram composed of many shapes inside one drawing callback still rasterizes as one image (see COVERAGE.md's forward-design notes for the `wpg:wgp` group-shape idea that would lift this) |

### Page layout

| Feature | | |
|---|:--:|---|
| Page size, orientation, margins | ✅ | `w:sectPr` |
| Columns | ✅ | `w:cols` |
| `#colbreak()` | ✅ | `<w:br w:type="column"/>` |
| Headers / footers | ✅ | native parts with exact static, first-page, and parity-stable variants; contextual values that vary beyond Word's first/even/default model repeat a page-1 sample with an explicit warning and fidelity decision |
| Page numbering | ✅ | `PAGE` field + `pgNumType` |
| `set page(background: image)` | ✅ | full-page `behindDoc` header image |
| `set page(fill: solid-color)` | ✅ | document-level `w:background` (Word's "Page Color") — a gradient/tiling fill is not (yet) representable this way and stays unset |
| Multi-section (geometry changes, landscape appendix) | ✅ | section breaks |
| Document metadata (title / author / date) | ✅ | `core.xml` |
| Hyphenation intent (`#set text(hyphenate: ..)`, or `auto` following justification) | ✅ | `w:autoHyphenation` (Word defaults this OFF, so it must be stated explicitly to preserve the author's intent) |

### Unsupported (dropped, with a warning)

| Feature | Why |
|---|---|
| Fractional spacing (`#v(1fr)`, `#h(1fr)`) | distributes leftover page space — no flowing-document equivalent |
| Margin notes (`drafting` / `marginalia`) | paged-only; the package panics in a flowing model |

Some markers are absorbed without a warning because they carry no lost content:
`#place(float)` flush ordering (we anchor floats in document order rather than
deferring them), and `pdf-marker-tag` accessibility delimiters (the wrapped body
is unwrapped and kept; the structural role is conveyed by native Heading/list
styles).

### Templates that assume a paged model

DOCX export first computes the standard paged fixed point and uses that
introspector while realizing the editable DOCX tree. Page reads, positions,
citations, bibliographies, and layout-time queries therefore generally see the
same answers as PDF export. The exporter still keeps a synthetic explicit-break
fallback for DOCX-only target branches and locations with no paged equivalent.

Remaining failures in paged-only templates are expected when the document itself
does not compile under paged layout, or when target-conditional code deliberately
has no DOCX branch.

## Usage

```
typst compile --format docx document.typ document.docx
```

The target is also selected automatically from a `.docx` output extension.

`DocxDocument::fidelity_report()` exposes structured representation decisions
(`NativeWithFallback`, `Approximate`, `Raster`, and `Drop` as they are enrolled),
independent loss dimensions, affected searchable-text counts, stable source
identities, and diagnostics suppressed by best-effort fallback conversion.
`DocxDocument::export_snapshot()` exposes owned semantic nodes matched to the
converged paged oracle, and `fidelity_manifest_xml()` serializes both records.
Every package persists that versioned manifest at
`customXml/typstFidelity.xml` and duplicates its exact text in the standard
`TypstFidelityManifestV1` custom document property because LibreOffice Writer
drops arbitrary custom-XML parts on save but preserves custom properties. The
finalized IR inventory records every dynamic field's instruction, update owner,
visibility, and occurrence count, plus every referenced font, whether it was
available on the export machine, and its current non-embedded status. The same
font inventory drives `fontTable.xml`, including fonts used only by individual
runs. Drawing facts independently classify described, native-text,
Office-decorative, and unlabeled objects. Complete native-region and
consumer-profile enrollment plus an optional standalone CLI sidecar remain
migration work.

For the cross-export pipeline, fidelity model, verified failure modes, and
proposed preflight architecture, see
[`../../docs/dev/office-export-architecture.md`](../../docs/dev/office-export-architecture.md).

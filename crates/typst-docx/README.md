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
This makes the docx pleasant both for a human to tweak in Word's GUI and for a
tool to regenerate by editing the (far more compact) Typst source.

The package is also structured like one Word itself saves: it ships the standard
`theme1.xml`, `fontTable.xml` and `webSettings.xml`, a full `settings.xml` with
the modern compatibility block (so Word opens it natively, not in "Compatibility
Mode"), and wraps newer constructs such as text boxes in `mc:AlternateContent`
with a legacy fallback for older consumers.

## How content is mapped

Every element falls into one of three tiers:

| | Tier | Meaning |
|---|---|---|
| ✅ | **Native** | Real, editable OOXML (text runs, `w:tbl`, OMML, fields, …). |
| 🖼️ | **Rasterized** | Embedded PNG. The visual is preserved exactly, but it is not editable. Used **only** when no OOXML construct can carry the content. |
| ❌ | **Unsupported** | Dropped, with a warning. Reserved for things with no flowing-document equivalent. |

Rasterization is a genuine last resort: it is bounded to non-solid fills,
vector/SVG/PDF/WebP images, transforms, and external drawing packages (CeTZ,
fletcher, …). Everything structural stays native. Cross-references and labels
**inside** rasterized regions are still harvested, so `@ref` to them resolves and
figure numbering stays consistent.

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
| Smart quotes | ✅ | resolved to curly quotes |
| `#hide[…]` | ✅ | `w:vanish` (hidden but kept; searchable / screen-readable) |

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
| Outline / table of contents | ✅ | `TOC` field in a content control |
| Bibliography & citations | ✅ | native text + clickable back-references |
| Fixed vertical space (`#v(2cm)`) | ✅ | folds into paragraph spacing |

### Tables

| Feature | | Notes |
|---|:--:|---|
| Tables | ✅ | `w:tbl` — borders, alignment, cell shading, merged cells, row heights |
| Layout grids (`#grid`) | ✅ | also `w:tbl` (content stays editable) |
| `stroke: none` cells | ✅ | explicit `w:val="nil"` |

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
| Non-math content inside `$…$` | 🖼️ | |

### Links, references & footnotes

| Feature | | |
|---|:--:|---|
| `#link(url)` | ✅ | external hyperlink (blue + underline) |
| `@ref` to heading / figure / equation / labelled element | ✅ | clickable hyperlink → bookmark |
| Footnotes | ✅ | `footnotes.xml` |
| `#link` to a page *coordinate* | ❌ | text kept, link dropped |
| `@ref` to a **page number** | ❌ | docx has no fixed-page model |

### Figures, images & graphics

| Feature | | Notes |
|---|:--:|---|
| Figures (caption + cross-reference) | ✅ | caption via `SEQ` field + bookmark |
| **PNG / JPEG / GIF** images | ✅ | embedded **verbatim** (no re-encode) |
| **SVG / PDF / WebP** images | 🖼️ | rasterized to PNG (no native Word form) |
| Rect, square, circle, ellipse, polygon (solid fill) | ✅ | **native vector** `wps:wsp` DrawingML |
| Framed text boxes (`#box`/`#rect[text]`) | ✅ | editable `wps:txbx`, or flowing shaded paragraphs |
| Horizontal rules (`#line`) | ✅ | paragraph bottom border |
| `#place(…)` (floating) | ✅ | `wp:anchor` float |
| Gradient / tiling / pattern fills | 🖼️ | no flat OOXML form |
| Curves (`#curve`), diagonal lines | 🖼️ | |
| Transforms (`#rotate`, `#scale`, `#move`, skew) | 🖼️ | |
| CeTZ / fletcher / canvas drawings, diagrams | 🖼️ | the main rasterize category |

### Page layout

| Feature | | |
|---|:--:|---|
| Page size, orientation, margins | ✅ | `w:sectPr` |
| Columns | ✅ | `w:cols` |
| `#colbreak()` | ✅ | `<w:br w:type="column"/>` |
| Headers / footers | ✅ | header/footer parts |
| Page numbering | ✅ | `PAGE` field + `pgNumType` |
| `set page(background: image)` | ✅ | full-page `behindDoc` header image |
| Multi-section (geometry changes, landscape appendix) | ✅ | section breaks |
| Document metadata (title / author / date) | ✅ | `core.xml` |

### Unsupported (dropped, with a warning)

| Feature | Why |
|---|---|
| Fractional spacing (`#v(1fr)`, `#h(1fr)`) | distributes leftover page space — no flowing-document equivalent |
| Margin notes (`drafting` / `marginalia`) | paged-only; the package panics in a flowing model |
| Page-number cross-references | docx has no fixed-page model |

Some markers are absorbed without a warning because they carry no lost content:
`#place(float)` flush ordering (we anchor floats in document order rather than
deferring them), and `pdf-marker-tag` accessibility delimiters (the wrapped body
is unwrapped and kept; the structural role is conveyed by native Heading/list
styles).

### Templates that assume a paged model

A small number of templates (~2% of a 627-document corpus) **fail to compile**
to docx although they compile to PDF. The error always originates in the
template's own code, not in OOXML generation — it assumes the paged layout
model that docx does not have:

- reading a page number that does not exist (`loc.page-numbering()` is `none`,
  `query(..page..)`), the same root as page-number cross-references;
- numbering or `query(...).first()`/`.last()`/`.at(n)` that assumes a
  layout-time introspector state (e.g. "a heading always precedes this figure",
  "every heading has two number components") which only holds once the document
  is laid out into pages;
- asserting a show rule runs an exact number of times across the laid-out
  document.

These are limitations of running a paged-only template through a flowing target,
not exporter defects, and are left to the template author (typically a one-line
guard such as `.at(1, default: 0)` or a `target`-conditional branch).

## Usage

```
typst compile --format docx document.typ document.docx
```

The target is also selected automatically from a `.docx` output extension.

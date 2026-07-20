# DOCX → Typst import

`crates/typst-docx-import` converts an arbitrary Word document into Typst
source. It is the mirror image of `typst-docx`: where that walks a realized
Typst document and lowers it to OOXML, this parses OOXML and *raises* it to
Typst source.

It is distinct from `typst-docx-roundtrip`, which is a **closed** loop — that
merges Word edits back into the Typst source Typst itself produced, via
embedded `typst:v1:` anchors and a sidecar state file, and cannot ingest a
foreign document. This crate is the **open** loop.

## Pipeline

```text
.docx ─opc::Reader─► xml ─wml::parse─► WmlPackage ─lower─► TypstDoc ─emit─► .typ + assets
        (ooxml-core)         │           (Word IR)  (mappers) (Typst IR)  (pretty-print)
                        resolve/ (styles, numbering)              passes/ (tier 2)
```

The load-bearing decision is the **two IRs**. `wml::model` is a faithful,
Word-shaped parse (a deliberate sibling of the exporter's `dom.rs`, minus
export-only concerns); `tdoc` is a Typst-shaped tree that speaks
headings/strong/emph/lists. Lowering into a Typst-shaped IR — rather than
emitting strings straight from the Word IR — is what lets the output be made
idiomatic by *adding passes* instead of rewriting the emitter.

Two tiers:

- **Tier 1 (literal)** — faithful lowering. Every run keeps its explicit
  formatting.
- **Tier 2 (idiomatic, the default)** — `passes/` rewrites the Typst IR:
  collapse styling that merely restates the document default, promote
  bold/italic to `*`/`_`, hoist `#set par`. These walk every block tree via
  `TypstDoc::block_trees_mut()`, so header/footer content is made idiomatic
  alongside the body.

Anything that can't be mapped cleanly is recorded in an `ImportReport` (the
mirror of the exporter's `FidelityReport`) so loss is auditable, never silent.

## Notable subsystems

**Fields** (`mappers/field.rs`). Word has two spellings — the `w:fldSimple`
element and a *flattened* `w:fldChar` begin/separate/end run sequence. Both are
folded back into one logical `RunItem::Field` at parse time, so lowering sees a
field rather than loose punctuation runs. `PAGE`/`NUMPAGES` become live page
counters, `HYPERLINK` becomes `#link`, `TOC` becomes `#outline()`. Every other
field type falls back to its **cached result** — what Word last rendered, i.e.
exactly the text a reader sees — which is the single highest-value behaviour
here: it preserves visible content for field types we will never model.

**Headers and footers** (`mappers/section.rs`). `w:sectPr` references up to
three variants per furniture (default / first / even). Word gates them:
`first` applies only under `w:titlePg`, `even` only under `settings.xml`'s
`w:evenAndOddHeaders`. Typst has one `header:`/`footer:` per page setup, so
active variants collapse into a single `context` block branching on the page
number. Visually-empty placeholder furniture (Word emits these routinely) is
dropped rather than emitted as `header: []`.

*Per-part relationship scoping* is the subtle part: `word/_rels/header1.xml.rels`
numbers its `rId`s independently of `document.xml.rels`, so the same id means
different targets in different parts. Furniture relationships are merged under a
namespaced key (`"word/header1.xml!rId1"`) and that part's drawing/hyperlink ids
are rewritten to match, so downstream resolution needs no special case.

**Content controls.** A `w:sdt` is a wrapper, not content — everything visible
lives in `w:sdtContent`. The parser splices it away at every level (body, table
row, cell, and inline within a paragraph). Ignoring the element silently drops
whatever it wraps, which for some documents is an entire footer.

**Hostile input.** The shared `opc::Reader` enforces zip-bomb, XXE and
XML-nesting limits; the parser additionally caps table, field and `w:sdt`
nesting depth. A malformed or malicious document must produce a clean error,
never a panic or a stack overflow.

## Emitter invariants

The escaper and the emitter both have to respect Typst syntax that Word content
routinely violates:

- `[` / `]` are content-block delimiters; literal brackets (`[100]`) are common
  in prose and must be escaped or they close the enclosing block early.
- `//` opens a line comment, which would swallow the rest of the line — URLs hit
  this constantly.
- `*` / `_` are only read as strong/emph delimiters at a **word boundary**.
  Word applies character formatting across sub-word run boundaries as a matter
  of course, so mid-word spans fall back to `#strong[..]`/`#emph[..]`, which
  always parse. (`*` mid-word is a hard error; `_` degrades silently to literal
  underscores — both are wrong.)
- Images in formats Typst cannot decode (`.wmf`/`.emf` metafiles, TIFF, BMP)
  are dropped with a report note. Referencing one fails the *entire* document
  with "unknown image format", so a single unsupported picture would otherwise
  cost every other page.

## Validation

The acceptance gate is the **Apache POI test-data corpus** — 128 genuinely
real-world `.docx` (real Word and LibreOffice output, deliberately including
fuzzer, truncated, encrypted and torture files). Per document it measures import
status, whether the emitted Typst compiles, and **text coverage** — the word-set
overlap between a LibreOffice render of the source and a Typst render of the
import.

```sh
cargo build --release
cargo build -p typst-docx-import --example import
python3 tools/docx-import-corpus/corpus.py --fetch   # first run only, ~8 MB
python3 tools/docx-import-corpus/corpus.py
```

It exits non-zero if any document that imported produced source that does not
compile — the one outcome that is always a genuine defect. Pass `--baseline
results.json` to diff a run against a saved one.

Text coverage is the importer's primary metric, the counterpart to the
exporter's page-fidelity metric: the first question is not "does it look
identical" but "did the content survive".

Current state:

| metric | value |
| --- | --- |
| import | 111/128 |
| compile | 111/111 |
| text coverage | mean ~98%, median 100%, min 50% |

The 17 non-imports are all *correct refusals*: fuzzer-corrupted archives,
truncated files, password-encrypted documents, an XXE probe, and a 5000-deep
nested-table DoS fixture. Each returns a clean error.

## Wrapper elements

Three OOXML constructs wrap content without contributing any of their own, and
all three are made transparent in one place (`splice_node` in `wml/parse.rs`)
so every walk — body, cell, row, paragraph, field folding — handles them alike:

- **`w:sdt`** (content controls) — spliced away in favour of `w:sdtContent`.
  Word wraps cover pages, date pickers and whole footers in these.
- **`mc:AlternateContent`** — an `mc:Choice`/`mc:Fallback` pair holding *the
  same content twice*. Taking both duplicates every text box; taking neither
  loses it.
- **`w:ruby`** — furigana, whose base and reading are both real sentence text.
- **`w:smartTag`**, **`w:bdo`**, **`w:dir`** — auto-recognition markup and
  bidi overrides, which nest several deep around a single run.
- **`w:ins`** / **`w:moveTo`** — tracked insertions, which *are* part of the
  final text. Deletions (`w:del`/`w:moveFrom`) are dropped instead, so tracked
  changes come in accepted, which is what Word renders by default. There is no
  option for this; there used to be an `accept_tracked_changes` flag that
  nothing read, which is worse than no option at all.

For `mc:AlternateContent` the choice is not automatic. MCE says a consumer
takes an `mc:Choice` only if it supports that choice's requirement. We handle
`wps` text boxes and the `a14`/`w14` drawing extensions better than the legacy
VML fallback beside them, so those Choices win. `cx` (2014 extended charts —
sunburst, box-and-whisker, waterfall) is the exception: Typst can't draw them
and flattening a hierarchical chart into a table misrepresents it, so we take
the `mc:Fallback`, which is a picture of the chart Word already rendered.

## Constructs with no Typst counterpart

Where Typst has no primitive, the importer keeps the *information* and reports
the approximation rather than dropping content:

| Word | imported as |
| --- | --- |
| chart (`c:chartSpace`) | the cached data as a `#figure(table(..))` |
| text box / shape text | `#box[..]` inlined at the anchor, geometry dropped |
| endnote | `#footnote[..]` (Typst has no end-of-document note store) |
| ruby / furigana | a generated `#let ruby(base, gloss)` preamble helper |
| `PAGE`/`NUMPAGES` field | live `#context counter(page)` calls |
| any other field | its cached result — what Word last rendered |

## Emitter constraints worth knowing

Typst markup has three traps that produce source which does not *parse*, and
the escaper handles all three:

- `[` and `]` delimit content blocks. Literal brackets are common in prose.
- `//` opens a line comment, which eats the rest of the line — including the
  closing `]`. Prose URLs hit this constantly.
- `*`/`_` are only read as delimiters at a word boundary. Word applies
  character formatting mid-word routinely, so those spans fall back to
  `#strong[..]`/`#emph[..]`, which always parse.

## Known gaps

- Only the body-level (final) `w:sectPr` is honoured — a multi-section document
  gets that section's page setup and furniture throughout.
- `w:pgMar/@w:header` / `@w:footer` clearances aren't mapped to page margins.
- Tab-stop layout (the "left⇥centre⇥right" header idiom) becomes plain spaced
  text, not a three-column grid.
- A figure or chart anchored on a list item ends the list and starts a new one
  after it: Typst can't place a block between two items of one list.
- Scatter/bubble charts (`c:xVal`/`c:yVal`) aren't extracted; a chart relying
  on live formula references rather than a cached values yields an empty table.
- A drawing nested inside a hyperlink or a field's cached result isn't
  discovered as its paragraph's figure.
- Adjacent runs sharing an identical style are emitted as separate `#text(..)`
  wrappers rather than merged — correct, but more verbose than necessary.

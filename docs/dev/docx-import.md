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
fuzzer, truncated, encrypted and torture files). Runner: `run.py` in the corpus
directory. Per document it measures import status, whether the emitted Typst
compiles, and **text coverage** — the word-set overlap between a LibreOffice
render of the source and a Typst render of the import.

Text coverage is the importer's primary metric, the counterpart to the
exporter's page-fidelity metric: the first question is not "does it look
identical" but "did the content survive".

Current state:

| metric | value |
| --- | --- |
| import | 111/128 |
| compile | 111/111 |
| text coverage | mean ~94%, median 100% |

The 17 non-imports are all *correct refusals*: fuzzer-corrupted archives,
truncated files, password-encrypted documents, an XXE probe, and a 5000-deep
nested-table DoS fixture. Each returns a clean error.

## Known gaps

- Only the body-level (final) `w:sectPr` is honoured — a multi-section document
  gets that section's page setup and furniture throughout.
- `w:pgMar/@w:header` / `@w:footer` clearances aren't mapped to page margins.
- Tab-stop layout (the "left⇥centre⇥right" header idiom) becomes plain spaced
  text, not a three-column grid.
- Charts, VML shapes and text-box content aren't extracted.
- Footnote/endnote *text* is not pulled in (references survive, bodies don't).
- Adjacent runs sharing an identical style are emitted as separate `#text(..)`
  wrappers rather than merged — correct, but more verbose than necessary.

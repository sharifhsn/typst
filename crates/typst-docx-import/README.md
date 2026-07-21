# typst-docx-import

Convert Word documents (`.docx`) into Typst source. This is the mirror image of
[`typst-docx`](../typst-docx): where that walks a realized Typst document and
emits OOXML, this parses arbitrary OOXML — from Word, LibreOffice, Google Docs,
WPS, whatever produced it — and raises it to readable Typst.

> **Status: experimental / preview.** The goal is that the *content* survives
> and the output reads like something a person would write, not that the result
> is a pixel-for-pixel reproduction. Everything the importer can't map cleanly
> is recorded in an `ImportReport`, so the loss is auditable rather than silent.

```sh
cargo run -p typst-docx-import --example import -- in.docx out.typ
cargo run -p typst-docx-import --example import -- --charts=plot in.docx out.typ
```

Some constructs — images, and a recovered bibliography — are written *beside*
the `.typ` as assets, so the emitted source only compiles next to them.

## Two IRs

```text
.docx ─opc::Reader─► xml ─wml::parse─► WmlPackage ─lower─► TypstDoc ─emit─► .typ + assets
        (ooxml-core)         │           (Word IR)  (mappers)  (Typst IR)   (pretty)
                        resolve/ (styles, numbering)               passes/ (tier 2)
```

The load-bearing decision is that there are **two** IRs, not one. `wml::model`
is a faithful, Word-shaped parse; `tdoc` is Typst-shaped. Lowering between them
— rather than emitting strings straight from the Word IR — is what lets the
output be made idiomatic by *adding a pass* instead of rewriting the emitter.
Tier 1 is literal fidelity; tier 2 collapses redundant styling, promotes
bold/italic to `*`/`_`, and hoists a preamble.

## What maps where

| Word | Typst |
| --- | --- |
| heading styles / `w:outlineLvl` 0–8 | `=` … `======` |
| `w:numPr` lists | `-` / `+`, nested |
| `w:tbl` | `#table` with spans, widths, fills |
| images | `#image`, extracted beside the source |
| `w:hyperlink`, `HYPERLINK` field | `#link` |
| `w:sectPr` | `#set page` |
| headers / footers | `header:` / `footer:`, page-class variants as one `context` |
| `w:footnoteReference` | `#footnote[…]` |
| endnotes | a superscript mark, bodies collected and numbered at the document's end — Word's own placement |
| OMML (`m:oMath`) | native Typst maths |
| `w:ruby` | a generated `#let ruby(base, gloss)` helper |
| `PAGE` / `NUMPAGES` fields | live `#context counter(page)` calls |
| `TOC` field | `#outline()` |
| any other field | its cached result — what Word last rendered |
| text boxes, shapes | `#box`, `#rect`/`#circle`/`#line`, inlined at the anchor |
| WordArt (`v:textpath`) | its text, styling dropped |
| charts | the cached data as a `#figure(table(..))`, or a real plot opt-in |
| `w:comment` + ranges | an invisible `#metadata` anchor pair — see below |
| tracked changes | rendered as accepted; the record kept as `#metadata` — see below |
| `b:Sources` + `CITATION` | a hayagriva `bibliography.yml` sidecar + live `#cite(<tag>)` |
| `w:object` (OLE) | the payload can't be revived, but Word's preview picture is kept and the producer is named |
| linked styles (`w:link`) | resolved as one style — a paragraph style inherits the run formatting of its character twin |

Wrapper elements that carry no content of their own — `w:sdt` content
controls, `mc:AlternateContent`, `w:smartTag`, `w:bdo`/`w:dir` — are
transparent. Each of them used to swallow whatever it contained.

### Annotations that must not reach the page

A comment and a tracked change are *annotations*: printing them would change
the document. Dropping them loses real authored information. Both therefore
lower to a labelled `#metadata`, the one Typst element that is invisible,
carries an arbitrary value, and stays reachable through `#query` — so the
rendered output is identical to an import without them, and a `#show` rule can
opt into displaying them.

```typst
#metadata((kind: "comment", author: "Ada", date: "…", body: [Check this.])) <comment-7>
```

Since a label attaches to a single element, a *span* becomes two anchors —
`<comment-7>` … `<comment-7-end>` — bracketing the words it is about. An
insertion works the same way around live text; a **deletion** instead carries
its removed text inside the anchor's own value, because there is nothing left
in the document to bracket.

`ImportOptions::tracked` chooses between `Preserve` (the default) and
`Accept`. Both render identically — insertions shown, deletions hidden, which
is Word's own "all changes accepted" view. They differ only in whether the
record survives beside it.

## Not supported

**Windows metafiles (`.wmf` / `.emf`) are not imported.** They are the single
most common image format in real `.docx` files — 41% of media parts in our
2,459-document corpus, more than PNG and JPEG combined — so this is a real
limitation, not a rounding error. They are dropped with a report note rather
than referenced, because referencing an image Typst cannot decode fails the
*entire* document.

The reason is structural. Typst reads raster images (PNG/JPEG/GIF/WebP) and
vector images (SVG, PDF). A metafile is neither: it is a serialized stream of
GDI drawing commands, so displaying one means implementing a GDI interpreter —
device contexts, object tables, world transforms, clipping regions, font
mapping — plus EMF+, which is a second command set layered on top. There is no
mature Rust decoder, and metafile parsing has a notable history of memory-safety
vulnerabilities, which matters for a crate whose input is untrusted by design.

If a Rust WMF/EMF → SVG converter matures, the seam to plug it into is
`mappers::drawing`, which already sniffs the real format from the leading bytes
and decides support there.

Also unsupported: an OLE embedding's *payload* (only Word's preview picture
survives, and 95.9% of those previews are themselves metafiles), `v:shape`
custom `v:path` geometry, tracked **formatting** changes (`w:rPrChange` /
`w:pPrChange`, which record what the formatting used to be), and
scatter/bubble chart series (`c:xVal`/`c:yVal`).

For the full picture in both directions, see
[`DOCX_SUPPORT_MATRIX.md`](DOCX_SUPPORT_MATRIX.md).

## Testing

Three gates, measuring different things:

```sh
python3 tools/docx-import-corpus/corpus.py               # 128 docs, text-coverage fidelity
python3 tools/docx-import-corpus/wide_corpus.py run      # ~2500 docs, import + compile
```

`corpus.py` is the routine gate — quick, and it measures whether content
survived. `wide_corpus.py` trades fidelity measurement for breadth: sixteen
producers across sixty languages, asking only the two questions whose failures
are always genuine defects. Text coverage is the primary metric here, the
counterpart to the exporter's page fidelity: the first question is not "does it
look identical" but "did the content survive".

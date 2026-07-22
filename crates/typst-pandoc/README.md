# typst-pandoc

> **Known incomplete and unsupported.** This consolidated target is retained for
> development and historical compatibility, but it is not part of the supported
> DOCX/PPTX preview or its release gates. Treat it as broken for general use.

Pandoc JSON AST export for Typst. Unlike Pandoc's syntax reader, this exporter
runs Typst first, so package imports, functions, show rules, and contextual
content are evaluated before the realized document is lowered to Pandoc nodes.

## Usage

```sh
typst compile document.typ document.pandoc
typst compile --format pandoc document.typ

# Run this from the output directory when a relative bibliography sidecar exists.
pandoc --from json --citeproc document.pandoc -o document.docx
```

Headings, paragraphs, inline formatting, lists, tables, links, footnotes, code,
figures, math, citations, and document metadata map to typed Pandoc nodes. Visual
content that has no Pandoc representation is embedded as an image fallback.

When the CLI writes a real output path and the document has a bibliography, it
also writes a sibling `.bib` file and records that filename in Pandoc metadata.
Pandoc resolves the relative name from its process working directory, so invoke
Pandoc from the output directory or rewrite the metadata path.

## Architecture

The pipeline has explicit stages:

1. `document.rs` realizes the Typst document under `Target::Pandoc`.
2. `convert.rs` lowers realized content into the typed AST in `ast.rs`.
3. `mappers/` owns element-specific lowering.
4. `normalize.rs` restructures citations and removes dangling links.
5. `encode.rs` serializes the Pandoc JSON envelope.

Raster fallback lives in `ctx.rs` and uses the shared
`typst-export-common` raster primitives.

## Known limitations

- This is a semantic target, not a paged-layout target. Page positions, floats,
  multi-column geometry, and exact line breaking cannot survive in Pandoc AST.
- Citation normalization currently loses some Typst grouping, mode, and
  supplement distinctions.
- Dangling-link discovery does not yet recurse through every table-cell path.
- Visual fallback width comes from the document's page width minus its margins,
  resolved from the document-level style chain. Two cases are not covered: a
  mid-document `#set page(width:)` is not seen, because this target has no page
  model and only reads the root styles; and `page(width: auto)` keeps a finite
  default, because laying width-relative content out against an infinite
  container width would produce pathologically wide output.
- The JSON and optional bibliography sidecar are separate writes, not one
  transactional output operation.

The shared Office/Pandoc fidelity model, verified issue register, and proposed
preflight architecture live in
[`../../docs/dev/office-export-architecture.md`](../../docs/dev/office-export-architecture.md).

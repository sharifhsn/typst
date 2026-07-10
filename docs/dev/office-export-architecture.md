# Office and interchange export architecture

This document is the architectural source of truth for the experimental DOCX,
PPTX, and Pandoc exporters in this fork. User-facing feature lists belong in
the individual crate READMEs. Historical corpus measurements belong in dated
reports such as `COMPARISON.md` and `crates/typst-docx/COVERAGE.md`.

## Product goal

PDF remains Typst's visual reference output. The additional exporters should
communicate as much of the same document as their target format can carry:

1. preserve the PDF's appearance where practical;
2. preserve native structure and editability where practical;
3. never silently lose content;
4. make every unavoidable degradation inspectable;
5. produce useful files in Microsoft Office and LibreOffice, not merely valid
   XML packages.

Those goals conflict. A page-faithful screenshot is not editable. A native Word
paragraph can reflow because Word and Typst use different shaping, font, and
pagination engines. A PowerPoint text box can be edited, but the recipient's
fonts can change its line breaks. The architecture must represent this trade
explicitly instead of hiding it inside mapper branches.

## Current pipelines

The three exporters intentionally start from different compiler products.

### DOCX: semantics first, paged layout as an oracle

```text
source
  -> ordinary paged compile ---------------------> PagedDocument
  -> Target::Docx realization -> native elements -> DOCX typed IR -> OPC package
                                  ^                    |
                                  |                    v
                                  +-- paged introspector and page sizes
```

Word is a flowing document model, so DOCX walks the realized native element
tree and emits paragraphs, styles, lists, tables, fields, footnotes, sections,
and OMML. The preliminary paged compile supplies converged answers for page
queries, citations, counters, and content-driven page sizes. A synthetic DOCX
page model remains as a fallback for DOCX-only locations.

This hybrid is powerful, but today the two realizations use different location
universes. `DocxIntrospector` contains selector-specific reconciliation rules;
that is a compatibility layer, not a stable long-term identity model.

### PPTX: final geometry first, semantics recovered through tags

```text
source -> PagedDocument -> frame walker -> slide IR -> PresentationML package
                           ^
                           +-- layout tags recover tables, math, columns,
                               slide numbers, and other semantic regions
```

A slide is a fixed canvas, so the final `PagedDocument` is the correct geometric
authority. The exporter walks frame items in painter order and emits editable
text, native geometry, tables, OMML, pictures, links, notes, and placeholders.
Layout tags selectively restore semantics that are no longer visible in flat
frames.

The weak point is that tags currently carry only fragments of the compiler's
resolved knowledge. The exporter still reconstructs paragraphs, spaces,
placeholders, cell styling, and roles from geometry or default styles.

### Pandoc: semantic interchange

```text
source -> Target::Pandoc realization -> Pandoc AST -> JSON
                                             +------> optional BibLaTeX sidecar
```

Pandoc has a linear semantic document model. The exporter walks native elements
into a typed mirror of the Pandoc JSON AST. Visual-only content can be rendered
to a self-contained image fallback. Citations keep both structured keys and a
formatted fallback.

Pandoc is not an OOXML exporter. Shared mechanics such as safe raster fallback
therefore live in `typst-export-common`, while packaging, DrawingML, OMML, and
Office media primitives live in `typst-ooxml-core`.

## Ownership map

| Layer | Crate/module | Owns |
|---|---|---|
| Compiler target selection | `typst-library`, `typst`, `typst-cli` | target, output format, orchestration |
| Format-neutral export mechanics | `typst-export-common` | safe frame rendering and raster geometry |
| OOXML mechanics | `typst-ooxml-core` | OPC, XML, namespaces, media, DrawingML, OMML, units |
| Word lowering | `typst-docx` | semantic realization, Word IR, WordprocessingML |
| PowerPoint lowering | `typst-pptx` | frame walk, semantic recovery, slide IR, PresentationML |
| Interchange lowering | `typst-pandoc` | native-element walk, Pandoc AST and JSON |

The exporter crates should own policy. Common crates should own mechanics. A
common crate must not decide whether a Typst construct is semantically safe to
lower into a particular consumer format.

## Required representation classes

Every subtree or resolved region should end in one explicit class:

| Class | Meaning |
|---|---|
| `Native` | represented with target-native editable semantics |
| `NativeWithFallback` | native representation plus a compatibility fallback |
| `Approximate` | editable/native output with a documented visual or semantic loss |
| `Raster` | rendered from Typst's paged model because native output is unsafe |
| `Drop` | no representation; always requires a diagnostic |

Many older dispatch branches still use `Option` or an empty vector as the
result. DOCX now has the first typed planning/reporting foundation:
`DocxDocument::fidelity_report()` stores `ExportDecision`s with stable source
identity, representation, reason, independent losses, occurrence counts, and
recovered-text counts. It also retains diagnostics deliberately suppressed by
best-effort conversion.

```rust,ignore
struct ExportDecision {
    source: ExportSource,
    representation: Representation,
    reason: DecisionReason,
    losses: LossSet,
    occurrences: usize,
    affected_text_chars: usize,
}
```

`LossSet` should independently record visual fidelity, semantic structure,
editability, dynamic consumer behavior, accessibility, and portability. “Live
text” is not automatically better if it destroys placement. “Pixel perfect” is
not automatically better if it turns a whole slide into one picture.

## Proposed clean design

### 1. Separate format from layout model

The compiler currently has `Target::Docx` and `Target::Pandoc`, while PPTX uses
`Target::Paged`. A future profile should make both axes explicit:

```rust,ignore
struct ExportProfile {
    format: ExportFormat,
    layout: LayoutModel,
    consumer: ConsumerProfile,
    capabilities: CapabilitySet,
}
```

This would prevent target-specific normalization from hiding in generic layout
registration and would let Typst code distinguish a PowerPoint paged export
from PDF without inventing a second layout engine.

### 2. Produce one stable export snapshot

DOCX currently reconciles two realization identities; PPTX reverse-engineers
semantics from frames. The shared compiler product should instead contain:

- the realized semantic tree;
- the converged paged document when requested;
- stable logical IDs shared by semantic nodes, frame tags, counters, links, and
  bibliography entries;
- resolved semantic regions for tables, columns, equations, page furniture,
  and placed content.

DOCX would use semantics as primary and paged geometry as an oracle. PPTX would
use frames as primary and semantic regions as a sidecar. Pandoc would use only
the semantic side.

### 3. Plan before emitting

Each exporter should run a capability-planning pass before serialization. The
planner chooses a representation for a whole logical region. This prevents
partial-success bugs such as emitting the supported children of an equation
while deleting one unsupported child, or accepting a table tag while capturing
no usable table cell.

Planning also makes consumer profiles possible:

- modern Microsoft Word;
- modern Microsoft PowerPoint;
- LibreOffice Writer;
- LibreOffice Impress;
- conservative OOXML fallback.

Open XML markup compatibility is only effective when the consuming application
understands the selected branch. `mc:AlternateContent` should therefore be an
explicit `NativeWithFallback` decision, not simply an encoding detail.

### 4. Emit a fidelity manifest

Debug environment variables are useful during development but insufficient for
users and corpus gates. Every export should be able to report:

- native/approximate/raster/drop counts by reason;
- text characters and semantic nodes captured by raster fallbacks;
- fields that can change when Word updates them;
- fonts referenced but not embedded;
- consumer-specific compatibility branches;
- suppressed layout or conversion errors.

The default CLI can remain quiet for lossless documents. A `--diagnostic-format`
or dedicated export report can expose the details without coupling them to
stderr strings.

DOCX now retains this information in memory on `DocxDocument`. Persisting it as
a CLI-selected JSON/text artifact, enrolling every native region and dynamic
field, and adding consumer/font facts remain open work.

## Consumer rendering realities

### Microsoft Word

WordprocessingML is a flow of paragraphs, runs, tables, stories, and section
properties. Word chooses pagination, line breaking, field results, and font
substitution when it opens or edits the file. Live `REF`, `SEQ`, `TOC`, and
page fields are behavior, not static decoration: updating them can replace the
cached result emitted by Typst.

The exporter must distinguish:

- static Typst-computed text that should remain stable;
- a live Word field whose update semantics are equivalent;
- a live field used only because Word cannot express the Typst result directly.

DOCX now encodes the first two cases as `FieldMode::{Static, Live}` and models
visibility independently with `FieldDisplay`. Normal semantic references remain
Typst-computed hyperlinks; page references are live `PAGEREF` fields. A visible
`SEQ` is live only for a provably equivalent single-component numeral system.
Richer numbering stays exact Typst text and advances an explicitly hidden Word
counter. Static complex fields use `w:fldLock`; hidden fields use both their
field switch and `w:vanish` on every structural/result run because LibreOffice
does not reliably implement Word's `SEQ \h` behavior.

The exporter does not emit document-wide `w:updateFields` or mark TOCs dirty.
Microsoft documents `updateFields` as recalculating all fields, while `fldLock`
prevents recalculation of a specific field. In current Word for macOS, either
`updateFields` or a dirty TOC also produces modal field/TOC dialogs on open.
Instead, TOCs bake entry text and Typst-computed page-number caches, remain live
when fully reconstructible, and expose Word's normal manual **Update Table** UX.
Typst-only fallback TOCs are locked so an update cannot delete entries that Word
cannot reconstruct. See Microsoft's [`updateFields`](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.wordprocessing.updatefieldsonopen?view=openxml-3.0.1),
[`w:dirty`](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.wordprocessing.dirty?view=openxml-3.0.1),
and [`w:fldLock`](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.wordprocessing.fieldchar.fieldlock?view=openxml-3.0.1)
references, plus LibreOffice's [field update guidance](https://help.libreoffice.org/latest/en-US/text/swriter/guide/fields.html).

Font names alone do not preserve metrics. OOXML supports embedded font parts,
but licensing, subsetting, and Office/LibreOffice support require a deliberate
policy. Until then, editable text fidelity must be tested both with and without
the source fonts installed.

### Microsoft PowerPoint

PresentationML has a fixed global slide size and separate slide, layout, master,
theme, notes, picture, table, and shape parts. Geometry can match Typst exactly,
but editable DrawingML text is re-shaped by the recipient. Auto-fit, placeholder
inheritance, language, theme fonts, and “Reset Layout” affect user experience
beyond first-open pixels.

Mixed Typst page sizes require an explicit mapping policy. Uniform fit with
letterboxing preserves geometry; non-uniform scaling fills the slide but
distorts shapes. Cropping silently is never an acceptable implicit policy.

### LibreOffice

LibreOffice can read and write DOCX/PPTX, but its import filters are an
independent implementation. Valid OOXML does not guarantee identical layout.
Active LibreOffice issues document differences in floating-object wrapping,
grouped shape text/rotation, and other layout behavior. LibreOffice rendering is
an essential compatibility target, not a substitute oracle for Microsoft Office.

## Verified architectural issue register

The following are verified from current source. They are not all fixed by the
mechanical cleanup that introduced this document.

### Critical

- PPTX rotated or otherwise unsupported native-table regions can be accepted by
  tag capture without producing a table or a whole-region raster fallback.
- PPTX mixed-size pages are not transformed to the global slide size even
  though current CLI text and documentation claim they are.

### High

- DOCX text-bearing `place` content can stay editable by discarding placement.
- DOCX page-varying furniture can silently freeze to an early sampled value.
- Some DOCX mapper-specific `Option`/empty fallbacks still conflate unsupported
  content and content loss. Central fallback layout, layout-callback, section,
  and delayed conversion failures are now retained in `FidelityReport`.
- DOCX style-deduplication can remove a deliberate heading override when it
  happens to equal the Normal style.
- PPTX table tags do not carry the resolver's final fill, stroke, inset,
  alignment, gutter, or cell-math contract.
- PPTX live text is regrouped heuristically and does not carry a complete font,
  language, shaping, or embedding policy.
- PPTX page filtering remaps speaker notes but not all slide-jump links.
- Pandoc raster fallback width defaults to 450pt despite comments claiming that
  the driver supplies real geometry.
- Pandoc builds an anchor-aware introspector but never installs the generated
  anchor map.
- Pandoc citation restructuring loses modes, supplements, and multi-cite
  grouping even though its formatted fallback remains readable.

### Cross-cutting

- target-specific normalization is partly registered from `typst-layout`, so
  ownership is not visible from exporter crates;
- math lowering has three implementations with different access to styles and
  resolved IR;
- package tests prove XML well-formedness but not all OPC relationship,
  content-type, uniqueness, schema, or consumer-repair invariants;
- current feature matrices and comparison measurements drift behind the code.

### Resolved on the rearchitecture branch

- DOCX math lowering now recursively preflights the resolved `MathItem` tree.
  Any unsupported box/external descendant selects a whole-equation PNG plus
  searchable text before OMML emission; partial native equations can no longer
  omit that child.
- The first typed loss-accounting layer is live: raster, compatibility,
  approximation, drop, and suppressed-diagnostic evidence is attached to the
  in-memory `DocxDocument`. Native enrollment and persisted manifests are not
  yet complete.
- DOCX field ownership is explicit. Reference intent now survives the library's
  `RefElem -> DirectLinkElem -> LinkMarker/style` realization path; the old
  mapper-only policy was bypassed in final output. Normal references remain
  static hyperlinks, page references become one live `PAGEREF` per logical
  reference, and non-equivalent figure numbering stays Typst-owned instead of
  being coerced to Arabic `SEQ` output.
- Native TOCs are baked, unlocked, and manually updateable; Typst-only fallback
  TOCs are locked. No field requests automatic document-open refresh, avoiding
  Word's modal warnings without leaving blank first-open page numbers.
- Hidden sequence counters carry run-level `w:vanish` formatting in addition to
  `SEQ \h`, fixing LibreOffice's otherwise-visible counter result.
- DOCX width ownership is scoped. Every section installs its page-minus-margins
  text width; nested table/stack cells temporarily narrow that budget; tables,
  grids, horizontal stacks, equation-number tabs, fill tabs, outlines, relative
  shapes, and raster fallback consume the same authority. Grid gutters are
  emitted as physical spacer tracks/rows, colspans include internal gutters,
  and normalization-only zero gutters cannot create phantom rows or columns.
- Fixed block-flow spacing is explicit in the DOCX IR. `FlowSpace` bridges gaps
  that have no neighboring paragraph to own `w:spacing` (notably table-to-table
  flow), while stack spacing becomes vertical flow blocks or horizontal table
  tracks. Fractional stack gaps remain editable, are represented as flexible
  tracks, and carry a structured visual-approximation decision.

## Current implementation evidence (2026-07-10)

- The focused Graphify corpus was refreshed from the live worktree: 2,954 code
  nodes, 8,150 extracted edges, and 122 communities. `FidelityReport` is linked
  to `DocxCtx`, `LoweredDocx`, `DocxDocument`, and the public report accessor;
  equation preflight reaches the explicit raster-fallback mapper. The refreshed
  graph also connects `DirectLinkKind` through `LinkElem` to the DOCX paragraph
  lowering path, while `caption_runs` calls the isolated `word_seq_format`
  equivalence classifier. The width graph connects `set_ctx_geometry` and
  `resolve_column_widths` through `DocxCtx`, making the section-to-table
  authority visible instead of implicit. `push_vertical_spacing` reaches the
  encoder through the typed DOM, and `record_flexible_spacing` reaches
  `FidelityReport` through `DocxCtx`.
- `cargo clippy -p typst-docx --all-targets -- -D warnings` passes.
- The complete structural DOCX test target passes 147 tests. New gates cover
  raster/compatibility/approximation classification, a retained suppressed
  layout-callback error, and nested unsupported math choosing one whole-region
  fallback.
- The atomic-math fixture was compiled through the real CLI to PDF and DOCX,
  then the DOCX was rendered through LibreOffice Writer 26.2.4.2. Both outputs
  remained one 160 mm × 90 mm page; the boxed fraction and surrounding equation
  were visible in the Writer PDF. Writer text export retained `1`, `BoxedTerm`,
  and `+ y`. The DOCX XML contained one picture and searchable hidden runs, and
  no partial `<m:oMath>` subtree.
- A dedicated field-policy fixture was opened in Microsoft Word and rendered by
  LibreOffice Writer. Word opened the final package without field/TOC dialogs;
  its accessibility tree and visual render showed a populated TOC, `Figure (i)`,
  a clickable static `Figure i` reference, and one live page reference. Writer
  produced the same visible caption/reference text. An earlier validation pass
  caught LibreOffice rendering `SEQ \h` as a visible extra `1`; the final
  run-level `w:vanish` representation removed it. A subsequent explicit
  select-all/F9 update and Word save preserved the static hyperlink and hidden
  counter while updating TOC/PAGEREF results; the Word-saved package rendered
  with the same text in Writer. Both consumers produced one 170 mm x 120 mm page.
- A two-section width fixture (100 mm and 160 mm text areas) was compared with
  the Typst PDF and opened in both Word and LibreOffice. Flexible 1:2 tracks,
  12 pt column gutters, 8 pt row gutters, and a nested table stayed horizontally
  aligned in both consumers; Word exposed the structures as editable tables.
  The same fixture exposed a missing 10 pt table-to-table gap; after `FlowSpace`
  replaced the paragraph-dependent accumulator, Word and Writer both rendered
  the explicit gap while preserving the editable tables.

## Validation model

No single gate is sufficient.

### Structural gates

- XML namespace well-formedness;
- unique package part names and content-type declarations;
- every internal relationship target exists;
- every referenced relationship ID exists on the owning part;
- unique bookmark, drawing, shape, slide, and relationship IDs;
- OOXML schema validation against the intended Office version;
- byte determinism under `SOURCE_DATE_EPOCH`.

### Semantic/editability gates

- live text coverage;
- native headings, lists, tables, equations, notes, links, citations, and
  navigation objects;
- fields before and after update;
- selection/copy and search behavior;
- alt text and screen-reader order;
- PowerPoint Outline view, Reset Layout, duplicate-slide, and new-slide UX.

### Visual gates

For a small fidelity kernel and the large corpus:

1. compile the Typst PDF reference;
2. export DOCX/PPTX;
3. render through Microsoft Word or PowerPoint;
4. render independently through LibreOffice Writer or Impress;
5. compare page/slide images and extracted text;
6. preserve per-consumer results rather than averaging disagreements away.

The kernel must cover tables, floats, placed text, headers/footers, page fields,
equations, CJK/RTL text, missing fonts, shapes/groups, gradients/tiling,
backgrounds, footnotes, citations, and mixed page sizes.

## Migration sequence

1. **Mechanical ownership cleanup — complete.** Move format-neutral raster mechanics to
   `typst-export-common`; isolate DOCX fallback, PPTX table capture, and Pandoc
   normalization in focused modules. No intended output change.
2. **Loss accounting — in progress.** Typed report and central lossy paths are
   implemented; field ownership and non-equivalent numbering decisions are now
   enrolled. Migrate remaining mapper-specific `Option`/empty fallbacks, native
   regions, and CLI manifest output.
3. **Stable semantic sidecar.** Carry resolved semantic regions and stable IDs
   alongside paged frames.
4. **Capability planning — first slice implemented.** Equations plan atomically;
   extend the same whole-region decision boundary to tables, placed content,
   furniture, fields, and consumer profiles.
5. **Cross-consumer gates.** Make Microsoft Office plus LibreOffice render and
   interaction results part of release evidence.

## Primary references

- [Microsoft: Structure of a WordprocessingML document](https://learn.microsoft.com/en-us/office/open-xml/word/structure-of-a-wordprocessingml-document)
- [Microsoft: Structure of a PresentationML document](https://learn.microsoft.com/en-us/office/open-xml/presentation/structure-of-a-presentationml-document)
- [Microsoft: Introduction to Open XML markup compatibility](https://learn.microsoft.com/en-us/office/open-xml/general/introduction-to-markup-compatibility)
- [Microsoft: Shape AutoFit](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.drawing.shapeautofit)
- [Microsoft: Word field updates](https://learn.microsoft.com/en-us/office/vba/api/word.fields.update)
- [LibreOffice: Using Microsoft Office and LibreOffice](https://help.libreoffice.org/latest/en-US/text/shared/guide/ms_user.html)
- [LibreOffice issue 76022: DOCX floating-object/table wrapping](https://bugs.documentfoundation.org/show_bug.cgi?id=76022)
- [LibreOffice issue 93675: grouped-shape text and rotation](https://bugs.documentfoundation.org/show_bug.cgi?id=93675)

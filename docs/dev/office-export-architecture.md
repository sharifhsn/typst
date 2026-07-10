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

The first DOCX slice now captures an owned `ExportSnapshot` before lowering.
Logical IDs are span/element based rather than target-specific-location based;
top-level lowering regions and nested located semantic nodes aggregate their
semantic occurrences and every matching converged paged position. The snapshot
also owns every converged page size, physical table/grid cell regions recovered
from hidden paged-frame tags, and a deterministic document ID. Counter, link,
bibliography, and fallback-specific resolved payload enrollment remains
incomplete.

DOCX uses semantics as primary and paged geometry as an oracle. PPTX would
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

DOCX now retains this information on `DocxDocument` and persists a versioned XML
manifest inside every package at `customXml/typstFidelity.xml`, related from the
main document part. A public serializer supports external tooling. A finalized
IR inventory enrolls every live/static field with its recalculation owner and
visibility, plus every referenced font with occurrence counts and current
non-embedded status. Complete native-region and consumer-profile enrollment and
an optional CLI-selected standalone JSON/text sidecar remain open work.

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

- DOCX contextual furniture that varies beyond Word's first/even/default model
  still freezes to a page-1 sample. The approximation is now preflighted,
  warned, and reported with affected text instead of being silent; a fully
  resolved per-page representation remains open design work.
- Some DOCX mapper-specific `Option`/empty fallbacks still conflate unsupported
  content and content loss. Central fallback layout, layout-callback, section,
  and delayed conversion failures are now retained in `FidelityReport`.
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
- shared package finalization now proves part/content-type uniqueness and typed
  internal relationship targets; DOCX additionally validates the
  repair-sensitive container sequences it emits, while full Office-version XSD
  validation still needs a broader gate;
- current feature matrices and comparison measurements drift behind the code.

### Resolved on the rearchitecture branch

- DOCX math lowering now recursively preflights the resolved `MathItem` tree.
  Any unsupported box/external descendant selects a whole-equation PNG plus
  searchable text before OMML emission; partial native equations can no longer
  omit that child.
- The first typed loss-accounting layer is live: raster, compatibility,
  approximation, drop, and suppressed-diagnostic evidence is attached to the
  `DocxDocument` and its versioned embedded manifest. Native enrollment is not
  yet complete.
- `ExportSnapshot` now gives semantic nodes and converged paged geometry one
  owned pre-lowering sidecar. Source-backed IDs exclude target-specific
  locations, repeated semantic occurrences aggregate, and matching paged
  positions/page sizes survive into the final document and manifest.
- Tables and grids now cross an explicit `TablePlan` before cell lowering.
  Fixed tracks and axis-aligned auto/fractional/relative tracks with complete
  converged cell measurements enroll as native. Unsupported cell paints, border
  nuance, repeating footers, transformed regions, or unavailable measurements
  enroll as one attributable visual approximation. A missing resolved grid
  selects atomic raster fallback and can only drop after that fallback also
  produces nothing.
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
- Shared OPC finalization rejects duplicate/reserved/traversing part names,
  conflicting default or override content types, missing relationship owners,
  and missing/invalid internal targets. Relationship mode participates in rId
  deduplication, parts/overrides are canonicalized, ZIP failures propagate as
  typed errors, and DOCX/PPTX no longer panic during package finalization.
- Shared OPC finalization parses every XML part and proves that each `r:id`,
  `r:embed`, and `r:link` resolves in the relationship set owned by that exact
  source part. Malformed XML and stale or cross-part relationship IDs fail with
  typed package diagnostics instead of reaching Word's repair path.
- The finalized DOCX IR is inventoried for fields and fonts after all body,
  table, TOC, drawing/text-box, header/footer, and footnote lowering. Field facts
  record instruction kind, Typst-versus-consumer update ownership, visibility,
  and occurrences. Font facts feed both the embedded manifest and
  `fontTable.xml`; font programs are explicitly reported as not embedded.
- A DOCX-specific final-IR gate now rejects zero/duplicate drawing IDs,
  duplicate or unpaired bookmark IDs and names, invalid or dangling footnote
  references, duplicate numbering IDs, and missing abstract/paragraph numbering
  targets before XML serialization. These WordprocessingML rules stay in
  `typst-docx`, while package mechanics remain in `typst-ooxml-core`.
- A second DOCX-owned gate runs over every accumulated XML part immediately
  before OPC finalization. It rejects late or duplicate paragraph/run/table/row/
  cell properties, table grids after rows, cells without a terminal paragraph,
  non-terminal body section properties, and invalid `mc:AlternateContent`
  branch order. Shared OPC exposes only a read-only XML-part view and remains
  free of Word policy.
- Heading inheritance now respects the full `HeadingN -> Normal -> docDefaults`
  cascade. A direct heading deviation that happens to equal Normal is retained
  whenever HeadingN defines that property, preventing Word from silently
  re-inheriting the heading value. Truly inherited properties are still
  deduplicated from runs.

## Current implementation evidence (2026-07-10)

- The focused Graphify corpus was refreshed from the live worktree: 3,214 code
  nodes, 8,887 extracted edges, and 134 communities. `FidelityReport` is linked
  to `DocxCtx`, `LoweredDocx`, `DocxDocument`, and the public report accessor;
  equation preflight reaches the explicit raster-fallback mapper. The refreshed
  graph also connects `DirectLinkKind` through `LinkElem` to the DOCX paragraph
  lowering path, while `caption_runs` calls the isolated `word_seq_format`
  equivalence classifier. The width graph connects `set_ctx_geometry` and
  `resolve_column_widths` through `DocxCtx`, making the section-to-table
  authority visible instead of implicit. `push_vertical_spacing` reaches the
  encoder through the typed DOM, and `record_flexible_spacing` reaches
  `FidelityReport` through `DocxCtx`. A focused placed-content query connects
  `preflight_place` and `PlacePlan` to `place`, table lowering, anchored drawing
  serialization, and the structured report path. A furniture query connects
  `build_furniture_refs` through `preflight_furniture` and `FurniturePlan` to
  `lower_furniture`, native part emission, and `FidelityReport` recording. The
  OPC query connects both public exporters through `Package::add_relationships`
  and `Package::finish` to target validation, canonical ordering,
  `PackageError`, and detached export diagnostics. The snapshot query connects
  `PagedIntrospector` and `matched_positions` through `ExportSnapshot` and
  `DocxDocument` to the public APIs, embedded manifest builder, typed package
  relationship, and final OPC validation. The field/font path connects the
  finalized recursive inventories to `FidelityReport`, the embedded manifest,
  and font-table emission. The measured-table query connects layout's
  `GridCellRegion` through the format-neutral `PagedGeometry` scanner, CLI
  handoff, `ExportSnapshot`, `DocxCtx`, `preflight_table`, and `TablePlan` to
  native `w:tblGrid` lowering, structured decisions, the embedded manifest, and
  the structural oracle-comparison gates.
- `cargo clippy -p typst-docx --all-targets -- -D warnings` passes.
- The complete structural DOCX test target passes 162 tests. New gates cover
  raster/compatibility/approximation classification, a retained suppressed
  layout-callback error, nested unsupported math choosing one whole-region
  fallback, explicit placed-content planning, native anchored tables, and
  complete DOCX byte determinism, stable semantic IDs/paged positions, and the
  embedded manifest relationship/content. Ten shared OPC unit gates cover
  duplicate and invalid parts, content-type conflicts, relative target
  resolution, missing targets, relationship-mode identity, insertion-order
  independence, malformed XML, missing referenced relationship IDs, and
  source-part scoping. The shared DML unit gate also passes.
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
- A style-cascade fixture was compiled through the real CLI to PDF and DOCX.
  The Heading1 style was blue, while one explicit red 11-point serif span
  matched Normal. Final XML retained the direct font, size, and color instead of
  deleting them during deduplication. LibreOffice Writer rendered the same
  single 150 mm x 80 mm page with the blue heading fragment, red override, and
  red body aligned to the Typst PDF; all text remained searchable.
- Placed content now crosses an explicit source-structural `PlacePlan` boundary
  before lowering. Shape-only regions select a native grouped drawing, plain
  text selects an anchored text box, one simple text-only table/grid selects an
  anchored text box containing native `w:tbl`, and richer regions lower exactly
  once before choosing an anchored drawing or reported flow/raster fallback.
  A real placed-table fixture opened in Microsoft Word without repair. Word's
  accessibility tree exposed `Placed Text Box 1`, a 2-row/2-column table, and
  each individual cell; selecting and replacing one cell value succeeded while
  the table remained positioned. LibreOffice Writer rendered the same package
  on the expected 160 mm x 120 mm page with native searchable table text and
  geometry closely matching the Typst PDF reference.
- Page furniture now crosses an explicit `FurniturePlan` boundary. Static,
  first-page, and parity-stable content produces exact native references;
  contextual content that changes between pages 3/5 or 2/4 selects a sampled
  approximation. That branch records `PageFurnitureSampled` with visual and
  dynamic-behavior losses, affected searchable-text characters, and emits a
  source-located CLI warning. A five-page real CLI fixture produced one native
  header part containing the page-1 value and the expected warning instead of
  silently implying exact export.
- The stable-snapshot/manifest package was rebuilt through the real CLI from
  the placed-table fixture. Microsoft Word opened it without repair and still
  exposed the anchored textbox, 2-row/2-column native table, and individual
  cells. LibreOffice Writer rendered one 453.543 x 340.157 pt page. Package
  inspection found the deterministic snapshot ID, page geometry, and native
  `PositionedTextBox` decision in `customXml/typstFidelity.xml`.
- A table-planning fixture compared one PDF gradient/fractional table with the
  native DOCX result. Word opened without repair and exposed an editable
  3-row/2-column table and every cell; Writer rendered one page with matching
  normalized table width and 1:2 columns. The unsupported gradient is now a
  representative purple solid (`854E9D`) rather than a missing fill, and the
  manifest reports `Approximate/TableGeometryApproximation`, 96 affected text
  characters, and one semantic node.
- A measured-table fixture compared an auto/1fr/2fr table, 7 pt gutters, and a
  nested 1:2 table across PDF, DOCX, Word, and Writer. The manifest enrolled two
  measured native tables. Writer preserved the outer and nested boundaries on
  the same 160 mm x 120 mm page; Word opened without repair and exposed the
  outer 2-row/5-track table, nested 1-row/2-column table, every cell, and an
  `Accessibility: Good to go` result. Narrow-cell text can still reflow under
  consumer font metrics, which is expected in Word's editable flow model.

## Validation model

No single gate is sufficient.

### Structural gates

- XML namespace well-formedness;
- unique package part names and content-type declarations;
- every internal relationship target exists;
- every referenced relationship ID exists on the owning part;
- unique bookmark, drawing, shape, slide, and relationship IDs;
- repair-sensitive DOCX child-sequence validation (implemented for the emitted
  paragraph, run, table, cell, body-section, and compatibility-branch subset);
- complete OOXML schema validation against the intended Office version;
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
   implemented; field ownership, non-equivalent numbering decisions, finalized
   dynamic-field facts, and referenced-font facts are enrolled. Migrate
   remaining mapper-specific `Option`/empty fallbacks, native-region and
   consumer-profile facts, and optional standalone CLI manifest output. The
   versioned manifest is already embedded in every DOCX.
3. **Stable semantic sidecar — table geometry slice implemented.**
   `ExportSnapshot` carries stable source-backed IDs, semantic occurrences,
   converged page sizes, matched paged positions, and final table/grid cell
   regions extracted through format-neutral paged-frame scanning. Enroll
   resolved counters, links, bibliography payloads, and fallback regions, and
   reuse more of the sidecar across PPTX/Pandoc.
4. **Capability planning — equations, placed content, furniture, and table slices
   implemented.** Equations plan atomically. Placed content preflights native
   shape groups, plain text boxes, simple native tables, and a lower-once
   fallback. Furniture preflights exact static/first/parity variants versus a
   warned sampled approximation. Tables/grids preflight native fixed geometry,
   reported editable approximations, missing-grid atomic fallback, and measured
   paged geometry for axis-aligned flexible tracks and row minima. Extend the
   same whole-region boundary to field groups and consumer profiles, and solve
   transformed or fully merged table geometry where no single-cell measurement
   is available.
5. **OPC invariants — third slice implemented.** Shared finalization validates
   unique/legal parts, content-type consistency, typed relationship owners and
   internal targets, XML well-formedness, per-source-part relationship ID
   references, deterministic ordering, and propagates ZIP failures. DOCX now
   adds final-IR drawing, bookmark, footnote, and numbering ID/reference gates,
   plus a final-package repair-sensitive child-sequence gate. Continue with
   complete Office-version XSD validation and PPTX-specific ID coverage.
6. **Cross-consumer gates.** Make Microsoft Office plus LibreOffice render and
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

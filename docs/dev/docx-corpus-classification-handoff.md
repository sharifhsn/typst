# DOCX corpus classification handoff

## Objective

Classify every document in the versioned public Typst corpus using observed
PDF/DOCX output, package structure, editability, and Office-consumer rendering.
The result must distinguish successful editable export from visually faithful
raster fallback. A successful compile alone is not a fidelity result.

This work has **not yet been run across the public corpus**. The checked-in
`tools/docx-validate` fixtures are a small release smoke corpus. The interactive
`tools/docx-review-demo` is a workflow demonstration, not corpus evidence.

## Classification taxonomy

Assign one primary class to every corpus entry:

| Class | Required evidence | Meaning |
| --- | --- | --- |
| `native_good` | Export succeeds; package, semantic, and editability gates pass; visual score is within the fixture threshold. | Word-native output works properly and broadly matches the PDF. |
| `native_degraded` | Export and structural gates pass, but visual score, pagination, text flow, fonts, or consumer behavior is outside the accepted threshold. | Editable, but not sufficiently faithful. |
| `fallback_visual` | Export succeeds and the fidelity manifest reports raster or compatibility fallback; rendered output preserves the region acceptably. | Appearance is preserved at the cost of native editability. |
| `fallback_degraded` | Fallback is used and the rendered result is outside the visual or semantic threshold. | The escape hatch worked mechanically but not well enough. |
| `content_loss` | Export succeeds, but expected text, semantic nodes, relationships, accessibility structure, or visible content is missing. | Silent or reported loss; never count as success. |
| `export_error` | Typst cannot produce the DOCX. | Exporter/compiler failure. |
| `package_error` | A DOCX is produced but ZIP, XML, relationship, content-type, or invariant validation fails. | Invalid or repair-prone OOXML. |
| `consumer_error` | Word or LibreOffice cannot open/render the produced package, crashes, repairs it, or times out. | Package is unusable in at least one required consumer. |
| `unverified` | A required gate could not run because a consumer, font, source asset, license, or dependency was unavailable. | No fidelity claim is allowed. |

Use the most severe applicable class. Preserve all secondary findings rather
than forcing them into the primary label. For example, a document can have
`primary_class: native_degraded`, `fallback_regions: 2`, and
`consumer_disagreement: true`.

## Required per-document record

Write one stable JSON record per source document with:

- corpus-relative source path, source hash, corpus revision, and license;
- exporter revision and exact Typst command;
- `primary_class` and machine-readable reason codes;
- compile duration, exit status, and normalized diagnostic;
- PDF and DOCX page counts plus extracted-text coverage;
- package/invariant results and whether Word reported repair;
- native counts for headings, paragraphs, lists, tables, equations, links,
  notes, drawings, descriptions, and other tracked structures;
- fidelity-manifest decisions, grouped by native, approximation, and fallback;
- Word and LibreOffice render results kept separately;
- visual score, page delta, missing fonts, and artifact paths;
- round-trip enrollment counts and any unsupported or conflicting regions;
- `unverified_reasons` when any required evidence is absent.

Do not average Word and LibreOffice into one score. Consumer disagreement is a
first-class result.

## Classification pipeline

1. Freeze the public corpus revision, source hashes, licenses, fonts, packages,
   and external assets. Missing inputs produce `unverified`, not a failed
   fidelity claim.
2. Build the exporter and record its Git revision and tool versions.
3. Compile each source to the reference PDF and DOCX with isolated output,
   bounded time, and captured diagnostics.
4. Run ZIP/XML/relationship/content-type and final-package invariant checks.
5. Extract text and native OOXML structure; parse the embedded Typst fidelity
   manifest to identify approximations, fallbacks, and reported losses.
6. Render the DOCX independently through Microsoft Word and LibreOffice. Record
   repair dialogs, timeouts, page counts, and failures per consumer.
7. Compare each consumer rendering against the Typst PDF and retain page images,
   diff images, extracted text, and metrics.
8. Probe review-state enrollment separately. Round-trip coverage must not be
   inferred from export success.
9. Apply the severity-ordered taxonomy above and emit aggregate reports by
   primary class, reason code, feature family, package, and consumer.

## Existing baseline command

First prove the current small smoke gate before adding the public-corpus runner:

```sh
cargo build -p typst-cli --release && \
  uv run tools/docx-validate/run.py \
    --typst target/release/typst \
    --out target/docx-validation \
    --mode smoke
```

Run its optional visual lane when LibreOffice and Poppler are available:

```sh
uv run tools/docx-validate/run.py \
  --typst target/release/typst \
  --out target/docx-validation-visual \
  --visual
```

The public-corpus runner should extend this harness rather than inventing a
second set of gate semantics. It should write at least:

- `metadata.json` — immutable corpus, exporter, consumer, and environment facts;
- `documents.jsonl` — one complete record per document;
- `summary.json` — counts and rates with explicit denominators;
- `summary.md` — human-readable findings and top failure clusters;
- `artifacts/<document-id>/` — logs, manifests, renders, and diffs.

## Acceptance criteria

- Every corpus source has exactly one terminal primary class.
- No missing consumer or visual result is counted as passing.
- Native success and raster fallback are reported separately.
- Export, package, consumer, semantic, editability, visual, and round-trip
  failures remain independently queryable.
- Aggregate percentages state their denominator and exclude nothing silently.
- Results are reproducible from the recorded corpus and exporter revisions.
- The dashboard reads the generated records; it does not contain hand-authored
  success counts.

## Current product truth

The exporter has broad native mapping and explicit raster/compatibility escape
hatches, but universal public-corpus fidelity has not been established. The
round-trip importer is deliberately narrower: it enrolls unique source-backed
text regions and rejects computed, generated, structural, and formatting edits
that cannot be mapped safely. Therefore the final dashboard needs separate
views for **export fidelity**, **Word editability**, and **Word-to-Typst
round-trip coverage**.

See [`docx-validation.md`](docx-validation.md) for the existing four-gate smoke
model and [`docx-roundtrip.md`](docx-roundtrip.md) for the current review-import
contract and exclusions.

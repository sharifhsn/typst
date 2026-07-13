# DOCX corpus classification handoff

## Objective

Classify every document in the versioned public Typst corpus using observed
PDF/DOCX output, package structure, editability, and Office-consumer rendering.
The result must distinguish successful editable export from visually faithful
raster fallback. A successful compile alone is not a fidelity result.

The checked-in `tools/docx-validate` fixtures remain a small release smoke
corpus, and the interactive `tools/docx-review-demo` remains a workflow
demonstration rather than corpus evidence. The independently frozen public
corpus campaign is tracked in the checkpoint below.

## Current campaign checkpoint (2026-07-13)

The corpus pipeline now has a reproducible freeze step and a resumable evidence
runner in `tools/docx-validate/`. The current local freeze contains 1,408 unique
documents from manifest SHA-256
`9b92ee3092b97c9c600547722ccb2397a5d21a1eea1d224418d1c57d1a9e99af`.
License evidence was detected for 888 documents; missing provenance remains an
explicit `unverified` reason.

The v4 structural run covers all 1,408 documents from the current exporter. It
has one source-template export error (`tracl`, whose own target switch has no
`docx` branch), one bounded DOCX compile timeout, zero invalid OOXML packages,
and zero `content_loss` classifications. The three former nonzero text-drop
records were indentation-only `plain_text` around visual bodies. `storytiles`,
`smorad-um_cisc_7026`, and `vnckey-book-rs` now report zero meaningful dropped
text across both focused and full-corpus reruns. Their visual/semantic drop
decisions remain queryable. Low PDF/DOCX text-extraction overlap remains a
separate diagnostic; it is not treated as proven content loss without stronger
evidence.

LibreOffice v4 enrichment completed for every runnable artifact: 1,399 rendered,
five timed out with explicit 180-second provenance, and four lacked runnable
evidence (`not_run` or `unavailable`). Of the successful renders, 793 passed the
initial visual/page policy and 606 missed it; only five had visual score below
`0.45`, while pagination drift accounted for most policy misses (604 had page
delta greater than one; 520 matched exactly). The worst former outlier
(`badformer`) improved from six pages and score `0.014820` to one page and
`0.984571` by preserving native drawing source origins, collapsing anchor-only
paragraphs, and adding a native page-fill compatibility shape. A subsequent
focused fix maps landscape page boundaries to `pageBreakBefore`, reducing the
worst current outlier (`sleiden-lei`) from 19 to 12 LibreOffice pages; its two
fixed-height title stacks remain degraded and are not claimed as solved.

Microsoft Word probing subsequently found a real consumer incompatibility in
`badformer`: drawing 269 introduced billion-EMU custom geometry, making Word
reject the otherwise package-valid document. Raster fallback was not viable—it
was killed by the operating system after attempting unbounded work. The final
fix compresses only pathological coordinates toward their off-page edge, keeps
all 275 drawings editable/native, and records four explicit `Approximate`
visual-fidelity decisions. Word opens the result without repair as one page;
LibreOffice remains one page with score `0.983091`. The focused result contains
durable Word evidence and structured diagnoses for compiler signals and
DrawingML coordinate outliers under
`target/docx-public-corpus-badformer-word-final/`. No structurally successful
document is promoted to a passing terminal class solely from this focused
proof. Current structural and LibreOffice results live under
`target/docx-public-corpus-run-v4/`; the focused page-boundary proof is under
`target/docx-public-corpus-sleiden-final-pagebreak/`, and the three-document
post-fix proof is under
`target/docx-public-corpus-fixes-v5/`. None is a versioned release claim.

A fresh v8 structural/round-trip pass now covers all 1,408 frozen documents:
1,407 package-valid exports, the same source-controlled `tracl` export failure,
1,407 successful round trips, 1,139 enrolled documents, and 549,345 review
regions. LibreOffice enrichment produced 1,401 successful renders, 834 visual
policy passes, 543 exact page matches, and 564 page deltas above one—improving
v4's 793, 520, and 604 respectively. The earlier v5 pass had six timeouts. Its
new timeout (`how-to-use-typst-for-paper-ja`) was traced to LibreOffice's layout of
one large width-relative inline PNG. Word now keeps the exact single picture in
a `w15` choice; alternate consumers receive two vertically cropped bands with
the same total dimensions. Focused LibreOffice conversion fell from over four
minutes to about 2.4 seconds with score `0.885345` and the prior 55-page count.
The final v8 failure set is back to the five pre-existing deterministic
timeouts; the fixed paper is one of the 1,401 successful renders. The fallback
is explicitly reported as `NativeWithFallback` with unified-image
editability loss. Word opens without repair but lays this presentation out as
95 pages versus the 23-page Typst reference, so its pagination remains an open
degradation rather than a solved document. Authoritative current results live
under `target/docx-public-corpus-run-v8/`; v6/v7 were trigger-audit runs and are
not current release evidence.

The next exact-page visual outlier, `utility/agregyst`, exposed page-overlay
canvas loss: its positioned border rasterized to 2×1191 pixels and was then
stretched over the entire page. Page overlays now use the full-frame renderer.
Focused LibreOffice evidence remains two pages and improves from `0.355880` to
`0.944054` (policy pass). Word opens without repair but reports three pages, so
its one-page pagination delta remains open. Focused artifacts live under
`target/docx-public-corpus-agregyst-full-overlay/`; this fix postdates v8 and
requires the next clean aggregate before becoming corpus-wide authority.

The following focused outlier, `integration/conch`, exposed a separate native
layout omission. Its terminal is a solid-filled `#block(height: 300pt)`; the
exporter retained the text but discarded the fixed height, leaving most of the
light terminal text outside the dark background. Fixed-height solid blocks now
lower to a one-cell native Word table with an `atLeast` row height and real cell
shading. This keeps the nested terminal structure and text editable instead of
rasterizing the region. Focused LibreOffice evidence remains one page, improves
from `0.420908` to `0.795660`, and passes policy. Artifacts live under
`target/docx-public-corpus-focus-background-inheritance-final/`; like the overlay fix,
this awaits the clean aggregate following v8.

`thesis/humble-dtu-thesis` then exposed a document-global page-colour leak.
Its dark cover fill was emitted as `w:background`, so every later page stayed
dark even after Typst returned to the default white fill. When existing page
runs form sections whose colours disagree, the exporter omits the
global page colour, retains native behind-text shapes only for coloured
sections, and emits an empty header part to stop Word from inheriting a prior
dark header into an unfilled section. Focused LibreOffice evidence keeps
the 12-to-13-page delta but improves from `0.403043` to `0.961059` and passes
policy. The proof is under
`target/docx-public-corpus-focus-humble-empty-header/`; all 188 structural DOCX
tests pass after this and the fixed-height-block change.

The same page-colour defect obscured `notes/daniel-ros-algorithmlecturenotes`.
After constraining the fixed-height-cell mapping to sub-page panels (page-sized
slide canvases must not become flowing tables), the section-fill fix improves
its LibreOffice score from `0.351669` to `0.809159` while preserving the prior
20-to-32-page count. The remaining 12-page pagination drift is independent and
open. Final focused proofs are under
`target/docx-public-corpus-focus-humble-empty-header/` and
`target/docx-public-corpus-focus-daniel-empty-header/`.

Microsoft Word opens the focused Conch and Humble documents without repair.
Conch's terminal remains editable native table/text content, but Word reports
two pages versus one in Typst and LibreOffice. Humble's section backgrounds and
inheritance resets are present, but Word reports 14 pages versus 12 in Typst
and 13 in LibreOffice. Both disagreements are retained in durable
`word-consumer.json` sidecars; neither document is promoted to universal native
success from LibreOffice evidence alone. Completed Word documents were closed
after inspection.

Corpus diagnostics now additionally expose stable `DOCX-E...` error codes and
`DOCX-W...` warnings. Each per-document entry retains the original stage detail
and adds a short explanation plus next action; aggregate JSON and Markdown
reports group the same codes for triage. `corpus.py --explain-code E201`
decodes either short or canonical spellings without requiring corpus inputs;
`--triage-run <run> [--code E201]` groups affected document IDs, stages, and
artifact/log paths, with `--json` available for automation. `W301` is the
rendered-but-outside-policy work queue and is re-derived from retained visual
evidence even for older runs. The current v9 LibreOffice enrichment
was launched before this schema and the two post-v8 exporter fixes were built,
so it remains useful as a deterministic comparison run but must be regenerated
before serving as final authority.

The pre-fix v9 enrichment completed with 1,401 successful LibreOffice renders,
the same five deterministic consumer failures, 543 exact page matches, 564 page
deltas above one, and 835 policy passes (the additional pass over v8 is the
Agregyst overlay fix). The subsequent clean v10 run under
`target/docx-public-corpus-run-v10/` added structural, LibreOffice, round-trip,
and stable error-code evidence from one release build.

The complete v10 retry/resume checkpoint restores the full 1,408-record
denominator and incorporates one Word sidecar. It has 1,401 successful
LibreOffice renders, the five historical consumer failures, one deterministic
consumer-PDF rasterization timeout, 835 policy passes, and 543 exact page
matches. The targeted serial retry proved the temporary August timeout was
load-sensitive: it again rendered 153 pages at `0.745940`; `tyrchen-open-books`
also completed its 445-page rasterization at `0.846302`.

Four remaining coordinate warnings then exposed page-absolute positions inside
unsupported-math raster fallbacks. Single-equation images reached 923×24,739
pixels, and one sparse fallback exceeded 750 million pixels / 5.89 billion EMU.
Equation-containing fallback regions now use the real page height on their first
layout instead of infinite height; other pathological infinite-height frames
receive one bounded retry. Focused optimized exports remove warnings from
`physx_book`, `ldiex-lecture-notes`, and `multimodal-tutorial`, reducing their
maximum extents to 10.69M, 70.73M, and 58.15M EMU. LibreOffice renders all three
with scores `0.978066`, `0.961428`, and `0.952929`. The tutorial exports in
41.18 seconds. `probablity_essay` remains warned at 137.10M EMU because its one
fully opaque wide raster is not the transparent-gap defect and is still open.
These changes postdate v10.

The clean v11 aggregate under `target/docx-public-corpus-run-v11/` is the
full-corpus authority before the page-layer and intrinsic-image fixes below. All 1,408 frozen records are
present; 1,407 packages and round trips succeed, 1,139 documents enroll 549,345
review regions across 3,282 files, and the single failure is the same
source-controlled `tracl` export. Serial consumer retries leave 1,401 successful
LibreOffice renders, the four historical timeouts, Minecraft's stable
conversion failure, and one `arnaukl` PDF-rasterization timeout. Visual evidence
contains 835 policy passes, 543 exact page matches, and 566 page deltas above
one. The three unsupported-math coordinate outliers are gone; only the distinct
`probablity_essay` warning remains. The byte-identical Conch artifact retains
its real Word sidecar, so Word evidence remains one success and 1,407 not run.

Every v11 record now carries the exact exporter executable SHA-256
`dcea153df7dbe79173fa88a7c69c9f52d5cb2a68feb67a9228e6dc85d016baca` and the
same original source-state fingerprint. The resume path preserves those
artifact identities and writes the caller's state separately as `last_resume`;
it no longer relabels a reused DOCX with newer unbuilt source. The serial lane
again recovered the load-sensitive August render (153 pages, `0.745969`) and
the 445-page `tyrchen-open-books` raster (`0.846330`), while
`typstraymarcher` recovered to one page at `0.648976`.

Two focused fixes postdate v11. A solid `page(fill:)` compatibility rectangle
could cover a simultaneous rasterized `page(background:)` in LibreOffice.
Background rasters now composite the solid fill into their own canvas and omit
the competing rectangle while retaining native Word `w:background` semantics.
This layer pattern appears in 16 v11 documents; the focused
`codealchemy24-resourcebook` proof improves from `0.560334` to `0.990632` with
one page and full text coverage. Separately, auto-sized native raster images
now follow Typst's natural-size rule and are proportionally bounded by the
current page region. `besarabegor-discord-guide` improves from 25 pages at
`0.598571` to the reference nine pages at `0.941477`, retaining all 245 words
and 17 native editable drawings. Focused artifacts live under
`target/docx-public-corpus-focus-codealchemy-page-layer/` and
`target/docx-public-corpus-focus-discord-intrinsic-image/`. All 192 DOCX tests
and all 22 round-trip tests pass.

The clean v12 aggregate under `target/docx-public-corpus-run-v12/` is now the
full-corpus authority for both fixes. It retains 1,407 valid packages and round
trips, 1,401 successful LibreOffice renders, the same five deterministic
consumer failures, the `arnaukl` rasterization timeout, and the source-owned
`tracl` export failure. Policy passes improve from 835 to 847, exact page
matches from 543 to 554, and page deltas above one fall from 566 to 554. The
last `DOCX-W102` coordinate warning disappears. A width-only counterfactual was
render-equivalent on seven selected landscape fixtures but failed a tall
synthetic case: Typst contains a 100 x 400pt natural image to 50 x 200pt,
whereas width-only Word output expands it to 200 x 800pt and clips most of it.
The retained two-axis rule therefore matches Typst for both landscape and
portrait images.

The next ranked `W301` fixture, `kdl-unofficial-template`, exposed two separate
page-run defects. Consecutive page breaks adjacent to a page-style transition
were all consumed as one Word section boundary; excess breaks now remain as
native page breaks in the new section. Whole-page `horizon`/`bottom` alignment
now maps to native section `w:vAlign` when every non-tag element in the run
agrees. KDL moves from 10 to 11 LibreOffice pages against Typst's 13, with score
`0.630572` to `0.630916`, unchanged `0.995331` text coverage, a valid package,
and a successful 104-region round trip. Real Word opens without repair, applies
the cover's native vertical centering, and reports 12 pages. The focused Word
sidecar is under `target/docx-public-corpus-focus-kdl-vertical-align/`, and the
document was closed immediately after inspection.

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

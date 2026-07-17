# DOCX validation harness

This is the small, checked-in release gate for Typst's DOCX exporter. It is
deliberately fixture-based: pull requests must not depend on a private or
mutable corpus, network access, or a particular office installation.

`run.py` compiles each fixture to PDF and DOCX and writes machine-readable
evidence:

- `metadata.json`: exporter revision, commands, tool versions, platform, and
  manifest/allowlist hashes;
- `results.json`: one package, semantic, editability, and optional visual result
  per fixture;
- compiled artifacts under `artifacts/<fixture>/`.

The always-on gate validates the DOCX ZIP, every XML and relationship part,
required visible text, and structural editability metrics. `--visual` adds a
LibreOffice-to-PDF render comparison. Visual scores are diagnostic thresholds,
not a claim of pixel identity: Word is a flowing layout engine and a page break
may legitimately differ. Retained page PNGs preserve the rendered colors for
human fidelity review; the compact automated layout score converts those color
artifacts to grayscale internally so color and geometry remain separate signals.

The Typst CLI embeds `customXml/typstFidelity.xml` in every DOCX export. The
manifest records exporter-side representation and approximation decisions; it
does not certify Word compatibility or visual equivalence. Library callers
using `typst-docx` directly may opt out with
`DocxOptions::embed_fidelity_manifest = false`, in which case the validator
correctly reports the export as unverified rather than inferring fidelity.

## Run locally

```sh
cargo build -p typst-cli --release && \
  uv run tools/docx-validate/run.py \
    --typst target/release/typst \
    --out target/docx-validation
```

Add `--visual` when `soffice`, `pdftoppm`, and `Pillow` are available. To run a
larger public corpus, pass another manifest with the same schema and keep its
source revision and licensing metadata in that manifest. Do not replace this
small release fixture set with a mutable local corpus.

## Public corpus campaign

The public-corpus lane is deliberately separate from the checked-in smoke gate.
First freeze every source, asset root, repository revision, and available
license signal from the corpus manifest:

```sh
uv run tools/docx-validate/freeze_corpus.py \
  --corpus /path/to/typst-corpus \
  --out target/docx-public-corpus-freeze
```

Then compile and classify the frozen records. The freezer records an explicit
Office target: corpus entries categorized as `presentation` go to PPTX; all
other categories go to DOCX unless a frozen record explicitly overrides
`target_format`. Known category mistakes are curated by exact entry path in
`format-overrides.json`; this avoids guessing from geometry and counting a
poster or landscape document as a deck, while keeping slide decks out of the
degraded Word-document denominator.
Results are durable per source, so `--resume` skips completed compilation and
can add consumer evidence later:

```sh
cargo build -p typst-cli --release && \
  uv run tools/docx-validate/corpus.py \
    --typst target/release/typst \
    --frozen target/docx-public-corpus-freeze/documents.jsonl \
    --out target/docx-public-corpus-run \
    --jobs 8

uv run tools/docx-validate/corpus.py \
  --typst target/release/typst \
  --frozen target/docx-public-corpus-freeze/documents.jsonl \
  --out target/docx-public-corpus-run \
  --jobs 8 --resume --libreoffice
```

The corpus output contains `metadata.json`, `documents.jsonl`, `summary.json`,
`summary.md`, and retained evidence under `artifacts/<document-id>/`. Missing
licenses, fonts, consumers, text extraction, or visual evidence produce
`unverified`; they are never folded into a passing denominator. Use repeated
`--filter <id-substring>` plus `--retry-libreoffice-failures` to isolate a
consumer failure without rerunning unrelated documents. Filtered resume runs
still regenerate authority-wide summaries from every retained result, so a
serial retry cannot replace the aggregate report with its selected subset.

DOCX and PPTX use separate fidelity evidence. DOCX retains its exporter
manifest and Word review round trip. PPTX records live DrawingML text and shape
metrics, slide count, LibreOffice Impress visual comparison, and raster-fallback
events emitted under `PPTX_DEBUG_RASTER=1`; DOCX-only fields are marked not
applicable rather than counted as failures. `summary.json.formats` keeps the
two denominators separate.

PPTX's `live_to_pdf_word_ratio` is a diagnostic ratio, not bounded recall: it
can exceed 1.0 when the editable slide contains searchable compatibility text
that the PDF text layer omits. Use multiset `text_coverage`, raster events, and
the visual score together rather than treating that ratio alone as fidelity.

Documents with redistributable font dependencies can use a checked-in
`font-fixtures/<document-name>/` directory. The corpus runner discovers it
automatically, supplies it to PDF/DOCX/review compilation, exposes it to the
LibreOffice renderer, and records its file list and content digest in each
fresh result. This keeps font-sensitive gold PDFs reproducible instead of
silently accepting a reference that omitted unavailable glyphs.

The exporter revision, dirty-tree fingerprint, and binary hash are captured once
when a campaign starts and reused by every worker. A commit or checkout while a
long run is still processing therefore cannot silently give later records a
different source revision while they continue using the same binary. Resume runs
preserve each compiled artifact's original identity and record the retry identity
separately in `metadata.json` under `last_resume`.

The validation-tool identity is equally authoritative. A resume now refuses to
mix a different `soffice`, `pdftotext`, or `pdftoppm` version into an existing
run directory; start a new `--out` directory after a tool upgrade. Visual scores
and page counts from separate runs are comparable only when their
`metadata.json` tool identities match. In particular, LibreOffice stable and a
LibreOfficeDev alpha can paginate the same byte-identical DOCX differently.

When semantic extraction rules improve, add `--refresh-semantic` to a resume.
It recomputes DOCX/PDF word evidence from retained artifacts without compiling
the corpus again. This currently counts both ordinary Word text and native OMML
math text; missing or timed-out PDF extraction remains explicit `unverified`
evidence rather than aborting the run.

`text_coverage` is the historical multiset-token Jaccard score, not literal
content recall. For a low score, decompose the retained evidence before treating
it as content loss:

```sh
uv run tools/docx-validate/semantic_diff.py \
  target/<run>/artifacts/<document>/reference.pdf \
  target/<run>/artifacts/<document>/document.docx \
  > target/<run>/artifacts/<document>/semantic-diff.json
```

The report separates PDF mathematical-alphanumeric tokenization, last-line
page furniture, token-boundary differences, and normalized letter-character
recall. Keep the JSON beside the authority artifact so the diagnosis remains
reproducible.

For a document whose consumer rendering gains or loses pages, retain a
text-position and flow-spacing report as well:

```sh
uv run tools/docx-validate/pagination_diff.py \
  target/<run>/artifacts/<document>/reference.pdf \
  target/<run>/artifacts/<document>/libreoffice.pdf \
  --docx target/<run>/artifacts/<document>/document.docx \
  --anchor 'Chapter title' \
  --output target/<run>/artifacts/<document>/pagination-diff.json
```

The analyzer maps unique text n-grams between pages, reports cumulative drift
and page density, resolves named anchors in both renderings, and inventories
display-math spacing and explicit page-break mechanisms in the DOCX. It does
not compare page images, so it is suitable for deterministic headless triage.

Each result also contains a `diagnoses` object. Compiler timeouts, non-zero
exits, and terminating signals receive stable reason codes (for example,
`process_killed` + `SIGKILL`) instead of relying on empty stderr. Successful
packages are scanned for Word-fragile DrawingML coordinate outliers, retaining
the affected part, attribute, value, threshold, and maximum coordinate. A real
Word probe can persist `artifacts/<document-id>/word-consumer.json`; subsequent
`--resume` runs validate and incorporate that evidence rather than losing a
manual consumer result.

Each document also has an `errors` array with stable `DOCX-E...` codes
(`DOCX-W...` for warnings). Entries retain the failing stage and raw structured
detail, but add a plain-language summary and suggested next action. Both summary
files aggregate those codes, making common failures triageable before opening
raw stderr logs.

The corpus command is also the canonical diagnostic decoder. These modes do not
require `--typst`, `--frozen`, or `--out`:

```sh
uv run tools/docx-validate/corpus.py --explain-code E201
uv run tools/docx-validate/corpus.py --list-error-codes
uv run tools/docx-validate/corpus.py \
  --triage-run target/docx-public-corpus-run \
  --code E201
```

`--triage-run` resolves a code to every affected document, its failing stage,
artifact directory, stderr path, and source JSONL line. Add `--json` for a
machine-readable result. Short codes such as `E201` and canonical spellings such
as `DOCX-E201` are accepted interchangeably. `W301` is the actionable visual
queue: the consumer produced a render, but its score and/or page delta missed
the run's configured policy. Triage re-derives codes from retained structured
evidence, so new catalog rules also work on completed older runs. `W301`
documents are ordered by lowest score, then largest absolute page drift.

Every freshly exported record also stores the SHA-256 of the exact Typst
executable that produced it. `--resume` preserves that record's original source
fingerprint instead of relabeling a reused DOCX with the caller's current dirty
tree. The run metadata likewise keeps its original identity and records the
latest retry separately under `last_resume`, so serial consumer retries remain
auditable even when exporter work has continued in the checkout.

Visual conversion always writes through a fresh short temporary output
directory, removes stale rendered PDFs and page PNGs before a retry, and moves a
successful PDF into the artifact directory. `pdftoppm` timeout reports name the
reference/consumer rasterization stage. LibreOffice runs use their own process
group so a timeout kills both the shell wrapper and the real office process;
this prevents CPU-bound orphan processes and locked profiles from contaminating
later evidence.

## Allowlist policy

`allowlist.json` is intentionally empty. A future exception must include an
`id`, `fixture`, `gate`, `reason`, an issue URL, and an ISO-8601 `expires`
date. Expired entries do not suppress failures. Allowed failures remain visible
in `results.json` and still make the run `passed_with_allowances`, never simply
green.

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
may legitimately differ.

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

Then compile and classify the frozen records. Results are durable per document,
so `--resume` skips completed compilation and can add consumer evidence later:

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

The exporter revision, dirty-tree fingerprint, and binary hash are captured once
when a campaign starts and reused by every worker. A commit or checkout while a
long run is still processing therefore cannot silently give later records a
different source revision while they continue using the same binary. Resume runs
preserve each compiled artifact's original identity and record the retry identity
separately in `metadata.json` under `last_resume`.

When semantic extraction rules improve, add `--refresh-semantic` to a resume.
It recomputes DOCX/PDF word evidence from retained artifacts without compiling
the corpus again. This currently counts both ordinary Word text and native OMML
math text; missing or timed-out PDF extraction remains explicit `unverified`
evidence rather than aborting the run.

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

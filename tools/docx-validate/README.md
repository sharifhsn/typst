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

## Allowlist policy

`allowlist.json` is intentionally empty. A future exception must include an
`id`, `fixture`, `gate`, `reason`, an issue URL, and an ISO-8601 `expires`
date. Expired entries do not suppress failures. Allowed failures remain visible
in `results.json` and still make the run `passed_with_allowances`, never simply
green.

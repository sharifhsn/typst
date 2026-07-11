# DOCX validation

The DOCX exporter has four separate quality signals. A successful compile is
necessary but does not establish any of the other three.

| Gate | What it proves | Release fixture implementation |
| --- | --- | --- |
| Package | A consumer can read a valid OOXML package. | ZIP integrity, required package part, and namespace-aware parse of every XML and relationship part. |
| Semantic | Required source text survives in both PDF and DOCX. | Expected-text assertions plus a diagnostic PDF/DOCX word-token Jaccard score. |
| Editability | Content remains Word-native rather than being silently flattened. | Minimum counts for paragraphs, Heading styles, tables/header rows, OMML, links, drawings, and descriptions. |
| Visual | A real office consumer broadly renders the DOCX like Typst's PDF. | Optional LibreOffice-to-PDF strip comparison with per-fixture score and page-delta bounds. |

The checked-in smoke corpus is deliberately small and deterministic. Its sources,
expectations, thresholds, and allowlist live in
[`tools/docx-validate/`](../../tools/docx-validate/). It is the pull-request
release gate; it does not replace the broader public corpus, which must be
versioned independently with source revision and license data.

## Run the smoke gate

```sh
cargo build -p typst-cli --release && \
  uv run tools/docx-validate/run.py \
    --typst target/release/typst \
    --out target/docx-validation \
    --mode smoke
```

The output contains `metadata.json`, including the exporter revision and tool
versions, and `results.json`, containing every gate decision. Keep both files
with any release or corpus report.

## Visual lane

The visual lane is deliberately optional because it requires LibreOffice and
Poppler. It runs weekly and may be requested manually in GitHub Actions.

```sh
uv run tools/docx-validate/run.py \
  --typst target/release/typst \
  --out target/docx-validation-visual \
  --visual
```

Visual scoring is not pixel-identity: DOCX is a flowing format and legitimate
pagination differences occur. Thresholds are per fixture, and every exception
must be an expiring, issue-linked entry in `allowlist.json`. An allowance remains
visible in `results.json` as `passed_with_allowances`.

## Accessibility boundary

This gate validates structural prerequisites such as heading styles, table header
rows, alt descriptions, and language-bearing Word output. It is not a substitute
for Word's Accessibility Checker or screen-reader testing. In particular,
raster fallback uses hidden recovered text and image descriptions; a release-level
assistive-technology test must verify that this does not produce duplicate
announcements.

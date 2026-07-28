# PPTX import corpus authority

This directory contains the reproducible breadth and round-trip gates for
`typst-pptx-import`. The fetched presentations and results are deliberately
repo-local but git-ignored so a reboot does not erase the authority and large
third-party fixtures are never redistributed by this repository.

```sh
uv run tools/pptx-import-corpus/corpus.py fetch
cargo build --release
uv run tools/pptx-import-corpus/corpus.py run --quiet
uv run tools/pptx-import-corpus/roundtrip.py --limit 60
```

`fetch` resolves the current Apache POI `trunk`, Apache Tika `main`, and
LibreOffice `master` refs to immutable commits, walks the documented test-data
subtrees, downloads PPTX/PPTM files, de-duplicates Git blobs, hashes every
package, and writes `corpus/manifest.json`. Interrupted downloads remain in
`corpus/.fetch/` and are reused on the next run; the authoritative manifest is
replaced only after a complete fetch.

The breadth gate writes `out/corpus-results.json`; the round-trip gate writes
`out/roundtrip-results.json`. Preserve the manifest and both result files when
recording a release claim. Import/compile success is a structural gate. Text,
shape, and coordinate ratios are useful fidelity signals, not proof of visual
identity in Microsoft PowerPoint.

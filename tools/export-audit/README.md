# Export audit: the corpus-scale harness

## Why this exists — the 2026-07 audit finding

Eight real exporter defects were found in one audit session: silently
flattened matrices, font-relative insets resolved against the wrong font,
36 dead internal links, nondeterministic packages, dead Pandoc anchor ids,
a stranded header relationship, physically overlapping stack children, and
tables of contents that lost their titles. **None were caught by the existing
gates** — not by the 400+ structural tests, not by the 3680-test render
suite, not by the fixture validator. All eight lived in the corpus
cross-product of construct × styling × realization path (`context`-titled
headings, 59%-width stacks, 30 pt insets, bilingual outlines), which no
authored fixture set enumerates.

The tooling that *could* have caught them was scattered across three
disconnected places: a deliberately fixture-only CI gate
([`tools/docx-validate`](../docx-validate)), a corpus bench suite in the
*corpus* repository that nothing here invokes, and seventeen ad-hoc session
scripts that died with the session that wrote them. Meanwhile the ad-hoc
scripts themselves failed eight times — every failure in re-derived corpus
plumbing, every failure producing plausible passing output.

This directory is the consolidation: corpus mechanics written once
(`corpuslib.py`), detectors as pure functions with **planted-defect
canaries** (`detectors.py`), and one CLI (`audit.py`).

## Division of labor

| Layer | Where | Catches | Cost |
|---|---|---|---|
| Structural fixture tests | `tests/src/{docx,pptx,pandoc}.rs` | authored cases, invariants on fixtures, byte determinism | seconds, every CI run |
| Fixture release gate | `tools/docx-validate` | package validity, editability, fixture visuals | minutes, CI |
| **Corpus invariants** | `audit.py invariants` | dead anchors, dangling rels, unresolved numIds/r:ids, `w:` in slides, nondeterminism — at scale | ~1 s/doc |
| **Corpus text fidelity** | `audit.py text` | content that vanishes or mangles between the PDF and the package | ~2 s/doc |
| **Corpus visuals, ranked** | `audit.py visual` → `rank` → `sheet` | gross layout breakage vs the PDF; a *ranker*, not a detector | ~15 s/doc; vision only on the ranked worst |
| **Same-renderer A/B** | `audit.py abdiff` | any real output change between two binaries, page-localized | ~30 s/doc, run on suspects |

### Why two visual instruments (measured, not assumed)

The whole-page mean against the typst PDF **cannot detect a localized
defect**: replaying the overlapping-stack bug (`815bea387`) against its own
fix moved the page mean by 0.003 — inside cross-renderer noise. Two
responses, both kept: pages are also scored by their **worst 4×6 tile**, so
a collapsed panel dominates its tile instead of drowning in the mean; and
`abdiff` renders *both* binaries' output through the same LibreOffice, where
the noise floor is ~zero and any tile that moves is a real change. Replaying
the same mutation, `abdiff` flags exactly one page (worst-tile 0.955, page 4
— the defective panel) out of twelve, and the identical-binary negative
control flags none. The gold-score ranks; `abdiff` detects; the sheet is
what you actually judge with.

Run `selftest` before trusting any run. It plants every known failure shape —
a dead anchor, a stranded relationship, the `numId="0"` sentinel, `w:rPr`
inside a slide, a nondeterministic pair, concatenated table cells, a
smallcaps split run (must NOT fire), a TOC title hiding behind its heading,
a repeated running head (must NOT fire), CJK loss, a drop-cap fragment
(must NOT fire) — and fails loudly if any detector misses or overfires.

## Typical sweeps

```sh
cargo build --release -p typst-cli   # never drive a debug binary at scale
cd tools/export-audit

uv run audit.py selftest
uv run audit.py invariants --binary ../../target/release/typst -n 150
uv run audit.py invariants --binary ../../target/release/typst --kind presentation -n 60
uv run audit.py text --binary ../../target/release/typst -n 150

uv run audit.py visual --binary ../../target/release/typst -n 60
uv run audit.py rank --scores /tmp/export-audit/scores.json -k 15
uv run audit.py sheet --scores /tmp/export-audit/scores.json \
    --binary ../../target/release/typst -k 8
```

Then *look at the sheets* (typst render left, office render right). The
cheap metric only decides where looking is worth it; the looking is the
check. A score is diagnostic, not a verdict — Word reflows legitimately.

For regression use, keep a `scores.json` from the last good revision and
pass it as `--baseline` to `rank`: a *drop* on the same document is signal
even when the absolute score is unremarkable.

## Ground rules the code enforces (each one paid for by a real miss)

- Success means the output file exists. stderr is full of honest warnings.
- Every document gets its own `--root`; a shared root breaks absolute
  imports and forges failures.
- Failures are listed by name; at corpus scale the *changed failure set* is
  the regression signal.
- Text joins runs with no separator inside a paragraph; word sets alone are
  never trusted (occurrence deficits and CJK pairs cover their blind spots).
- Exports run serially, and a `/debug/` binary path warns: a debug-binary
  parallel sweep has taken the host down before.

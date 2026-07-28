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

Two kinds of check live here, and the distinction matters:

- The **canary suite** (`selftest`) is *regression armor for known failure
  classes*. Each canary is a planted defect from a specific past bug plus its
  clean twins; it proves a detector still fires on that exact shape and stays
  quiet on the look-alikes. It is indexed on failures we have already seen.
- The **general layers** reason from the defect space of "lower a laid-out
  document into OOXML," not from any bug list: *referential integrity*
  invariants (every id/anchor/rel resolves, and every id is unique where the
  format demands), *reading-order / formatting / colour* signals that a
  bag-of-words comparison is blind to, a *generative cross-product* that
  manufactures construct × context combinations no author enumerates, and a
  *consumer re-save* that runs the output back through a real reader.

| Layer | Where | Catches | Cost |
|---|---|---|---|
| Structural fixture tests | `tests/src/{docx,pptx,pandoc}.rs` | authored cases, invariants on fixtures, byte determinism | seconds, every CI run |
| Fixture release gate | `tools/docx-validate` | package validity, editability, fixture visuals | minutes, CI |
| **typ_leak scoreboard** | `audit.py leak` | the campaign's objective function: export failures + typst/office page-count divergence, one number with the leaking documents by name; `--baseline` diffs runs (NEW / FIXED / moved) | ~4 s/doc |
| **Corpus invariants** | `audit.py invariants` | dead anchors, dangling rels, unresolved numIds/r:ids, `w:` in slides, **duplicate docPr / bookmark / cNvPr / sldId ids**, **drawings in text boxes/notes (Word refuses to open these)**, nondeterminism — at scale | ~1 s/doc |
| **Corpus text fidelity** | `audit.py text` | content that vanishes or mangles; **scrambled reading order** (`--links` also flags external-link loss). Supports DOCX documents and PPTX slides via `--kind presentation`; the Word run-size-collapse signal is DOCX-only. | ~2 s/doc |
| **Corpus visuals, ranked** | `audit.py visual` → `rank` → `sheet` | gross layout breakage vs the PDF, plus **colour that vanished** (`desaturated`); a *ranker*, not a detector | ~15 s/doc; vision only on the ranked worst |
| **Generative cross-product** | `audit.py generate` | export failure, invariant breaks, and silent sentinel loss over construct × context pairs | ~2 s/pair, one shot |
| **Consumer re-save** | `audit.py resave` | content a real consumer (LibreOffice) silently drops from our output | ~10 s/doc, on ~15 |
| **Same-renderer A/B** | `audit.py abdiff` | any real output change between two binaries, page-localized | ~30 s/doc, run on suspects |

### Which consumer renders our output (`--consumer`)

`visual` and `sheet` take `--consumer soffice` (the default), `--consumer
office`, or an explicit `--consumer word` / `--consumer powerpoint`.
`office` routes DOCX to Microsoft Word and PPTX to Microsoft PowerPoint; the
explicit choices reject the wrong format instead of accidentally sending a
slide deck to Word. LibreOffice is fast and scriptable, but it is a **proxy**:
it is not the consumer either format exists for, and it is markedly more
forgiving than the native Office apps.

That difference is not academic. The very first real-Word run raised *"You
can't put drawing objects into a text box, callout, comment, footnote, or
endnote"* — Word declining to **open** the document at all — on output
LibreOffice had rendered without complaint for over a thousand documents. Four
of 150 corpus documents were affected. The lesson is worth stating plainly: a
clean LibreOffice sweep is evidence about LibreOffice, and a claim about Word
is a hypothesis until Word has actually run. (The same run also *refuted* a
spec-derived belief that Word needs `<w:displayBackgroundShape/>` to show a
page colour. It does not.)

Where a rule can be checked statically, prefer that — `drawings_in_text_boxes`
now runs in the `invariants` layer, needing neither Word nor a render, so the
expensive consumer is for discovery rather than routine gating.

Word and PowerPoint are driven through AppleScript (`osascript`), not headless
binaries, with two consequences worth knowing:

- They are GUI apps. The readiness probe launches the requested app with
  `open -g` so it stays backgrounded, then waits until it answers; a **cold**
  launch may not respond for minutes, so the warm instance is reused.
- A package Office objects to can raise a **modal dialog**, which wedges every
  later conversion. A conversion that times out therefore closes all open
  documents/presentations before giving up — and a timeout is itself a signal
  that the native consumer may be rejecting that package.

Both lanes stage their input and intermediate PDF inside the app's macOS
container, then copy the PDF back to the audit output. This avoids the first-run
`Grant File Access` dialog without granting either app access to the checkout.

Native Office is the slower instrument by some margin: run it on a reduced `-n`.

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
uv run audit.py generate --binary ../../target/release/typst   # construct x context matrix
uv run audit.py invariants --binary ../../target/release/typst -n 150
uv run audit.py invariants --binary ../../target/release/typst --kind presentation -n 60
uv run audit.py text --binary ../../target/release/typst -n 150 --links
uv run audit.py text --binary ../../target/release/typst --kind presentation -n 60 --links

uv run audit.py visual --binary ../../target/release/typst -n 60
uv run audit.py rank --scores ../../target/export-audit/scores.json -k 15   # DESAT marks vanished colour
uv run audit.py sheet --scores ../../target/export-audit/scores.json \
    --binary ../../target/release/typst -k 8

uv run audit.py resave --binary ../../target/release/typst -n 15   # LibreOffice re-save round-trip

# Native-consumer discovery: DOCX goes to Word and presentations go to
# PowerPoint. Keep -n small because both are GUI apps driven over AppleScript.
uv run audit.py visual --binary ../../target/release/typst --consumer office -n 20
uv run audit.py visual --binary ../../target/release/typst --kind presentation \
    --consumer office -n 12
```

`generate` needs no corpus — it manufactures its own inputs and is the one
layer that runs in a single shot. `text --links` adds a `typst query` per
document (drop the flag for the fast path). The default output is durable under
`target/export-audit/` rather than a system temporary directory. The `resave`
and `visual` layers each spawn a consumer per document and are the slow tail;
run them last on a reduced `-n`.

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

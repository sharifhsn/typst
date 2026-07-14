# DOCX public-corpus campaign: next-session handoff

> Status: historical session handoff (superseded by office-export-shipping-readiness.md, 2026-07-13).

Updated 2026-07-13. This is the short operational handoff for continuing the
long-running goal. The detailed historical log remains in
`docs/dev/docx-corpus-classification-handoff.md`.

## Goal and boundaries

Work in `/Users/sharif/Code/typst/.claude/worktrees/docx-public-release`.
Continue classifying the frozen 1,408-document public Typst corpus across
export, OOXML package validity, LibreOffice and Word consumption, semantics,
editability, visual fidelity, and Typst-to-Word-to-Typst round trip. Fix real
exporter defects with rich native mappings where safe and explicit fidelity
fallbacks where not. Keep reports honest. Do not commit, push, or publish unless
separately requested.

Before editing, run `rtk git status --short`; the worktree intentionally has a
large uncommitted campaign diff. Preserve it. All shell commands must follow the
repository `AGENTS.md`/RTK quoting rules. Build and run in one `&&` chain when a
fresh binary is required.

The user specifically requested that any future Ryujinx or Computer Use work be
delegated to a cheap Luna agent. Do not operate those UIs in the primary agent.
The current recovery is headless LibreOffice/pdftoppm work and does not require
Computer Use. A prior cost-harness turn denied delegation; obey the live harness
if it still does.

## Accepted implementation state

The current release binary SHA-256 is
`ce686eb855cf3d6960f1264e898929b68bdc1d66508078fa28e0fadf04216d59`.

Accepted fixes since the clean v12 authority:

- Intrinsic raster images are proportionally bounded by both axes of the current
  layout region. A 100 x 400 portrait-image regression proves the height bound;
  the rejected width-only alternative clipped the image.
- Consecutive explicit page breaks next to a geometry/page-style transition are
  retained after the Word section boundary. `SectionRun.leading_pagebreaks`
  carries the excess breaks and lowering emits native page-break paragraphs.
- Uniform whole-page `horizon` or `bottom` alignment maps to section-level
  `w:vAlign="center"` or `w:vAlign="bottom"`.
- Crucial guardrail: vertical alignment must **not** participate in
  `same_section`. Emitting new sections only for alignment caused a massive
  pagination regression and was reverted. Like background and hyphenation, the
  alignment is emitted only when another real property already creates a
  section.
- `tools/docx-validate/corpus.py` has a stable error catalog and human-facing
  helpers: `--explain-code`, `--list-error-codes`, and
  `--triage-run RUN [--code CODE] [--json]`. `DOCX-W301` ranks successful
  renders that miss visual policy.

Focused results for `book/kdl-unofficial-template`: Typst 13 pages,
LibreOffice 11, Word 12; score `0.630916`; text coverage `0.995331`; valid
package; 104-region round trip. Word opened without repair and visibly applied
native cover centering. The Word document was closed after inspection.

Latest verified gates after these source changes:

- DOCX tests: 194/194 passed.
- Round-trip tests: 22/22 passed.
- `cargo fmt`, Python byte-compilation, and `git diff --check` passed.

No source edits were made after those gates.

## Authority and rejected experiment

`target/docx-public-corpus-run-v12/` is the last completed clean full-corpus
authority: 1,407 valid packages and round trips, 1,401 successful LibreOffice
renders, 847 policy passes, 554 exact page counts, and 554 page deltas above one.
Its stable failures are the source-owned `paper/tracl` export error, four
deterministic LibreOffice timeouts, the Minecraft conversion failure, and the
`thesis/arnaukl-tfg_writing` raster timeout.

`target/docx-public-corpus-run-v13/` is **rejected**. Comparing it with v12
showed 217 changed documents and 167 worsened. Exact page matches fell to 541,
page deltas above one rose to 578, and policy passes fell to 821. Cause: treating
vertical-alignment changes as section boundaries. Do not restore that behavior.

`target/docx-public-corpus-run-v14/` is the clean run of the corrected policy,
but is not yet authoritative because its serial recovery/aggregate is still in
progress. The initial parallel pass had 1,397 LibreOffice successes, 547 exact
page counts, 567 page deltas above one, and 830 policy passes. Those totals are
known to be depressed by load-sensitive consumer failures and must not be cited
as final.

## Completed serial recovery at handoff time

The bounded `/private/tmp/retry-docx-v14-recoverable.sh` process completed while
this handoff was being written. No corpus, LibreOffice, or `pdftoppm` process
remains. The five filtered cases were:

- `book/t-u-r-a-tura-coding-book`
- `uncategorized/augustozanellato-polylux-unipd`
- `uncategorized/raphaelasla-typstraymarcher`
- `book/tyrchen-open-books`
- `thesis/arnaukl-tfg_writing`

All five now have successful LibreOffice consumption. Four have complete visual
results; only the known `thesis/arnaukl-tfg_writing` E204 remains because
`pdftoppm` again timed out after 180 seconds. The filtered report shows no E201,
four visual successes, and five successful round trips.

Because a filtered resume intentionally rewrites the reports for only its
selection, `target/docx-public-corpus-run-v14/documents.jsonl` and
`summary.json` currently contain **five records**, not the full corpus. The
summary's denominator of five is not an aggregate regression. The very next
operation must be the unfiltered resume/aggregation described below.

Confirm no stale process before starting it with:

```sh
rtk zsh -lc 'pgrep -fl "tools/docx-validate/corpus.py|soffice.*docx-public-corpus-run-v14|pdftoppm" || true'
```

If the Python runner disappears while a `soffice` child remains with PPID 1,
that conversion is orphaned; terminate only those exact stale PIDs before
retrying. This happened once with `hanzi-calligraphy` and was cleaned up.

## Word evidence already retained

Two sidecars have been placed in v14:

- `artifacts/book-kdl-unofficial-template/word-consumer.json`
- `artifacts/integration-conch/word-consumer.json`

Conch's v14 DOCX is byte-identical to the inspected v12 DOCX. KDL's formatted
package content matches the inspected corrected focused export except for
nondeterministically generated internal `_Typst...` hyperlink-anchor names.
Do not reopen either document merely to recreate this evidence. A subsequent
full `--resume` pass will incorporate the sidecars into `documents.jsonl` and
the aggregate.

## Exact next actions

1. Run an unfiltered full `--resume --libreoffice --roundtrip` aggregation now,
   so all 1,408 records and both Word sidecars are restored to the reports. Use
   the current release binary; no rebuild is needed unless source changes. A
   suitable command is:

   ```sh
   rtk uv run tools/docx-validate/corpus.py \
     --typst target/release/typst \
     --frozen target/docx-public-corpus-freeze/documents.jsonl \
     --out target/docx-public-corpus-run-v14 \
     --jobs 8 \
     --timeout 240 \
     --resume \
     --libreoffice \
     --roundtrip
   ```

2. Triage the restored full aggregate:

   ```sh
   rtk uv run tools/docx-validate/corpus.py \
     --triage-run target/docx-public-corpus-run-v14
   ```

3. Retry only any newly remaining load-sensitive failures serially. The deterministic
   expected set does not need repeated parallel retries: `fun/hanzi-calligraphy`,
   `presentation/touying-simpl-nudt`, `report/gakusyun-doc`,
   `thesis/unofficial-ouc-bachelor-thesis`, and the Minecraft E202 case.
4. Compare v12 with final v14. Adapt `/private/tmp/compare_v12_v13.py` to load
   `v14`. Verify the six worst v13 regressions returned exactly to v12 behavior,
   KDL remains at 11 LibreOffice pages, and report changed/improved/worsened
   counts plus aggregate policy/page totals.
5. Update both `crates/typst-docx/COVERAGE.md` and the historical handoff with
   the rejected v13 metrics and final v14 authority.
6. Re-rank `DOCX-W301` from final v14 and continue with the next real defect.
   The initial ranking starts with `presentation/sleiden-lei`, then KDL. KDL is
   already diagnosed and improved; Sleiden is the likely next target.

## Sleiden starting evidence

The current v14 record for `presentation/sleiden-lei` has Typst 10 pages versus
LibreOffice 12, score `0.308420`, text coverage `0.831650`, 257 DOCX words versus
287 PDF words, nine tables/drawings, and five `UnsupportedContent` drops. The
first-page comparison shows editable title/header/footer content but a missing
Leiden logo block; page two in Word is largely the logo block displaced onto its
own page. Start by tracing why that native/fallback image or positioned block
enters normal flow and forces pagination. Do not assume the five dynamic drops
are the root cause without checking the realized content and OOXML.

Useful artifacts:

- `target/docx-public-corpus-run-v14/artifacts/presentation-sleiden-lei/`
- `/private/tmp/compare_v12_v13.py`
- `/private/tmp/compare_focus_narrowed.py`
- `/private/tmp/focus-section-align-narrowed.sh`
- `/private/tmp/run-docx-corpus-v13.sh` (despite its name, it targets v14)
- `/private/tmp/retry-docx-v14-recoverable.sh`

There is no `graphify-out/graph.json` in this worktree or repository root, so
Graphify is unavailable here. No commits or publication actions have been made.

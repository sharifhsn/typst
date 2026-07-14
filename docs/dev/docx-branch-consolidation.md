# DOCX branch consolidation

> Status: historical session handoff (superseded by office-export-shipping-readiness.md, 2026-07-13).

This file records the Phase 0 integration baseline for the public DOCX export
work. It is a topology ledger, not a promise that local worktrees can be deleted:
remove a worktree only after its branch is remotely backed up and the integrated
release gates pass.

## Active integration line

`codex/docx-public-release` starts from `codex/office-architecture-cleanup` at
`ccb015ed7`, twenty commits after `office-pandoc` at `4c84579ae`. It merges
`origin/main` at `921bb8318`. The merge deliberately preserves the existing
feature-branch history.

Local rollback refs were created before integration:

- `codex/office-pandoc-backup` at `4c84579ae`
- `codex/office-cleanup-backup` at `ccb015ed7`

These refs are local until explicitly pushed.

## Integrated ancestors

The following tips are exact ancestors of the integration line and are
functionally superseded by it:

- `docx-export` (`e0a6e8cc6`, retained by the `v0.15.0-docx.3` tag)
- `docx-citations-research` (`82de6d1ed`)
- `docx-columns-section` (`2ebb0296e`)
- `docx-header-titlepg` (`a1eeea174`)
- `docx-quick-wins` (`57842cf93`)
- `docx-remaining-bundle` (`739770ed4`)
- `docx-run-styles` (`e674d67a7`)
- `docx-svg-passthrough` (`4e1f7b991`)
- `ooxml-core2` (`29b2e1991`)
- `office-pandoc` (`4c84579ae`)
- `codex/office-architecture-cleanup` (`ccb015ed7`)

Do not retire these worktrees merely from this list. First back up the active
integration branch, run the validation matrix, and confirm that no untracked
research artifacts are needed.

## Preserved independent work

`ooxml-math` (`401270a2e`) is not an ancestor of the integration line. Its two
commits explore OMML-to-Typst import and remain separate because bidirectional
conversion is outside the one-way exporter release phases.

`claude/typst-pandoc` (`dd8c0f5f3`) is also topologically independent, although
the integrated line contains a separate Pandoc implementation. Keep it until a
behavioral parity audit establishes that it is fully superseded.

## Phase 0 gate

After the upstream merge, the focused DOCX suite passed with 173 tests. The
merge also exposed obsolete marker-trait imports in the fork's columns and grid
extensions; those imports were removed to match the current upstream element
macro API. Full workspace, corpus, consumer, and accessibility gates remain the
release authority described by the DOCX validation documentation.

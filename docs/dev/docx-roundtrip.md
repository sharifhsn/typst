# DOCX advisor review round trip

The Phase 4 review workflow is an opt-in, deliberately conservative bridge for
bringing ordinary editorial changes from Word back into the original Typst
project. It is not a generic DOCX-to-Typst converter.

## Export a review package

```sh
typst compile paper.typ paper.docx --docx-review-state
```

This writes `paper.docx` and `paper.docx.typst-review.json`. The DOCX contains
opaque Word content-control tags around source regions that are safe to map.
The JSON state stays with the student: it contains source baselines and exact
project-relative ranges needed for a three-way merge. Do not send the state file
to the reviewer unless sharing the full Typst source is intentional.

A custom state path can be selected with an equals sign:

```sh
typst compile paper.typ paper.docx \
  --docx-review-state=.review/paper.json
```

Ordinary DOCX export remains unchanged when this option is absent.

## Inspect and apply advisor edits

After receiving an edited DOCX, run a dry review first:

```sh
typst review advisor-edited.docx \
  --state paper.docx.typst-review.json \
  --root .
```

The command prints a JSON result for every enrolled region. `ready` means the
Word edit can be applied; `unchanged` means Word did not alter that region; a
`conflict` is never applied implicitly. Save the report with `--report path`.

Apply only a conflict-free plan:

```sh
typst review advisor-edited.docx \
  --state paper.docx.typst-review.json \
  --root . \
  --apply
```

The importer rechecks source hashes immediately before replacing files and
escapes Word text as literal Typst markup. Local edits elsewhere in a file are
allowed when the original source island still has one unambiguous match. An
overlapping or ambiguous local edit becomes a conflict.

The MVP applies at most one changed source file per invocation so replacement
remains atomic. A review spanning edits in multiple Typst files can be inspected
but cannot yet be applied automatically.

## Current supported contract

The first slice enrolls plain, uniquely realized, source-backed headings. It
preserves the heading marker and changes only its authored text. It accepts
Word run splitting and the final view of tracked insertions/deletions.

It intentionally rejects or leaves unenrolled:

- paragraphs, list restructuring, and table edits;
- generated, repeated, package, bibliography, reference, equation, and raster content;
- paragraph insertion/deletion and manual line breaks inside a region;
- missing, duplicated, copied, or foreign content controls;
- ambiguous source relocation or overlapping student edits;
- formatting-only round trips, comments, and arbitrary Word objects.

These exclusions prevent a visually plausible Word edit from silently
flattening Typst code, macros, counters, or templates. Future slices can add
literal paragraphs, list-item text, table-cell text, comments, and formatting
only after each has exact source-segment ownership and conflict tests.

## Security boundary

The DOCX parser treats the returned document as untrusted. It bounds archive and
expanded sizes, reads only the known document part, rejects DTD/entity input,
validates every tagged control, and never extracts ZIP paths. Source paths come
only from the student-retained state and must remain beneath the selected root.

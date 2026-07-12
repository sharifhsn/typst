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
Word comments anchored inside an enrolled region are included in the same JSON
under `comments`, with their Word id, region id, author, initials, date, and
plain comment text. They are preserved as review annotations and are never
injected into executable Typst source.

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

Source-backed regions from project imports and includes are enrolled alongside
the main file. A conflict-free review can update multiple Typst files in one
transaction: every source is validated and prepared first, original files are
hard-linked to same-directory rollback backups, and any replacement failure
restores already-committed files before returning an error.

## Current supported contract

The current slice enrolls plain, uniquely realized, source-backed:

- headings, preserving the heading marker;
- ordinary paragraph text;
- bullet and numbered list-item text, preserving the list marker and numbering;
- each source-backed paragraph in single- or multi-paragraph table cells,
  preserving the table structure and cell formatting.

Literal text may be nested in Typst strong, emphasis, underline, strike,
highlight, small-caps, superscript, or subscript wrappers. The importer replaces
only the innermost literal source island, so those existing Typst styles survive
every Word text-edit cycle.

Each edit replaces only the exact authored source island. Imported heading,
paragraph, and list text uses an identifier-free literal markup expression;
table cells use the same expression inside a content block so the result remains
valid in `table`'s code-mode argument list. All four forms are eligible again on
the next export, so repeated advisor cycles do not degrade coverage. Word run
splitting and the final view of tracked insertions/deletions are accepted.

It intentionally rejects or leaves unenrolled:

- computed text, mixed literal/computed paragraphs, list restructuring, and
  table row/column/cell restructuring;
- generated, repeated, package, bibliography, reference, equation, and raster content;
- paragraph insertion/deletion and manual line breaks inside a region;
- missing, duplicated, copied, or foreign content controls;
- ambiguous source relocation or overlapping student edits;
- formatting-only round trips and arbitrary Word objects. Comments outside an
  enrolled source-backed region remain outside the importer boundary.

These exclusions prevent a visually plausible Word edit from silently
flattening Typst code, macros, counters, or templates. Future slices can add
formatting changes and structural edits only after each has exact source-segment
ownership and conflict tests.

## Security boundary

The DOCX parser treats the returned document as untrusted. It bounds archive and
expanded sizes, reads only the known document part, rejects DTD/entity input,
validates every tagged control, and never extracts ZIP paths. Source paths come
only from the student-retained state and must remain beneath the selected root.

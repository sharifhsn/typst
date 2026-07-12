# DOCX review interface demo

A dependency-free visual simulation of the Typst ↔ Word advisor-review workflow.
It demonstrates the source-backed regions, comments, conflict detection, and
safe apply behavior implemented by the DOCX round-trip crates. It is deliberately
not presented as a live Microsoft Word Online integration.

Run it from the repository root:

```sh
uv run python -m http.server 4173 --directory tools/docx-review-demo
```

Then open <http://127.0.0.1:4173/>.

The demo is entirely local and does not upload documents or contact Microsoft.

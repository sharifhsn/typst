"""One-off: re-run classify()/classify_pptx() over an existing run's
documents.jsonl without redoing compile/consumer/visual/round-trip work,
and regenerate summary.json/summary.md. Use after a classification-policy
change in corpus.py so a full corpus re-run isn't needed to see its effect.

Usage: python reclassify.py <run-dir>
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import corpus


def main() -> int:
    run_dir = Path(sys.argv[1]).resolve()
    documents_path = run_dir / "documents.jsonl"
    records = [
        json.loads(line)
        for line in documents_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    for record in records:
        if record.get("target_format", "docx") == "pptx":
            primary, reasons, unverified = corpus.classify_pptx(record)
        else:
            primary, reasons, unverified = corpus.classify(record)
        record["primary_class"] = primary
        record["reason_codes"] = reasons
        record["unverified_reasons"] = unverified
        artifact_result = (
            run_dir / "artifacts" / corpus.safe_component(record["id"]) / "result.json"
        )
        if artifact_result.is_file():
            artifact_result.write_text(
                json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n",
                encoding="utf-8",
            )
    summary = corpus.write_reports(records, run_dir)
    print(json.dumps(summary["primary_classes"], indent=2))
    print(json.dumps(summary["unverified_reasons"], indent=2))
    print(json.dumps(summary["missing_fonts"], indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

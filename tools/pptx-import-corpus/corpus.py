#!/usr/bin/env python3
"""Acceptance gate for the PPTX → Typst importer.

The counterpart of `tools/docx-import-corpus/corpus.py`, and it asks the same
two questions of every real presentation we can find:

  1. **Does it import?**  A crash or an error is always a defect.
  2. **Does the emitted source compile?**  Source that does not compile is
     worse than no source, because it looks like an answer.

Both failures are unambiguous, which is what makes them worth gating on. A
third question — does it *look* right — needs a renderer and a human, and is
what `--render` is for.

Coverage is measured the way the Word importer measures it: the fraction of
the deck's text that survives into the emitted source. Text is the one thing
whose loss is never acceptable and always detectable.

    python3 corpus.py --corpus /tmp/pptx-corpus/docs
    python3 corpus.py --corpus /tmp/pptx-corpus/docs --filter touying --render
"""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import re
import subprocess
import sys
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
IMPORTER = REPO / "target" / "debug" / "examples" / "import"
TYPST = REPO / "target" / "debug" / "typst"

TEXT_RE = re.compile(rb"<a:t>([^<]*)</a:t>")
# Slide parts only: a master's or layout's prompt text ("Click to edit Master
# title style") is template furniture that no importer should reproduce, and
# counting it would make coverage look worse the more faithful we got.
SLIDE_PART_RE = re.compile(r"^ppt/slides/slide\d+\.xml$")


@dataclass
class Result:
    name: str
    imported: bool
    compiled: bool
    coverage: float | None
    notes: int
    error: str = ""


def deck_text(path: Path) -> str:
    """Every character of slide text in the source deck."""
    out = []
    try:
        with zipfile.ZipFile(path) as z:
            for name in z.namelist():
                if SLIDE_PART_RE.match(name):
                    for m in TEXT_RE.finditer(z.read(name)):
                        out.append(m.group(1).decode("utf8", "replace"))
    except Exception:
        return ""
    return "".join(out)


def normalise(text: str) -> str:
    """Compare on word characters only.

    Typst escaping, list markers and whitespace all differ legitimately
    between the deck and the emitted source; a character-level diff would
    report those as loss and drown the real thing.
    """
    return "".join(ch.lower() for ch in text if ch.isalnum())


def coverage(deck: str, source: str) -> float | None:
    want = normalise(deck)
    if not want:
        return None
    got = normalise(source)
    # Longest-common-subsequence would be exact but quadratic on a megabyte of
    # text. Counting how much of the deck's character multiset survives is
    # close enough to spot a dropped shape and fast enough to run on 542 files.
    from collections import Counter

    a, b = Counter(want), Counter(got)
    kept = sum(min(n, b[ch]) for ch, n in a.items())
    return kept / len(want)


def run_one(path: Path, render: bool, timeout: int) -> Result:
    name = path.stem
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "out.typ"
        try:
            proc = subprocess.run(
                [str(IMPORTER), str(path), str(out)],
                capture_output=True,
                timeout=timeout,
                text=True,
            )
        except subprocess.TimeoutExpired:
            return Result(name, False, False, None, 0, "import timed out")
        if proc.returncode != 0 or not out.exists():
            first = (proc.stderr or "").strip().splitlines()
            return Result(name, False, False, None, 0, first[0] if first else "import failed")

        notes = sum(1 for line in (proc.stderr or "").splitlines() if line.startswith("- ["))
        source = out.read_text(encoding="utf8", errors="replace")
        cov = coverage(deck_text(path), source)

        target = ["--format", "png", str(Path(tmp) / "p-{n}.png")] if render else [
            str(Path(tmp) / "out.pdf")
        ]
        try:
            comp = subprocess.run(
                [str(TYPST), "compile", str(out), *target],
                capture_output=True,
                timeout=timeout,
                text=True,
                cwd=tmp,
            )
        except subprocess.TimeoutExpired:
            return Result(name, True, False, cov, notes, "compile timed out")
        if comp.returncode != 0:
            msg = ""
            for line in (comp.stderr or "").splitlines():
                if line.startswith("error:"):
                    msg = line[len("error:") :].strip()
                    break
            return Result(name, True, False, cov, notes, msg or "compile failed")
        return Result(name, True, True, cov, notes)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", default="/tmp/pptx-corpus/docs")
    ap.add_argument("--filter", default="")
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) - 2))
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--render", action="store_true", help="compile to PNG instead of PDF")
    ap.add_argument("--quiet", action="store_true", help="only print failures")
    args = ap.parse_args()

    for tool in (IMPORTER, TYPST):
        if not tool.exists():
            print(f"missing {tool}; build it first", file=sys.stderr)
            return 2

    root = Path(args.corpus)
    files = sorted(
        p for p in root.iterdir() if p.suffix.lower() in (".pptx", ".pptm") and args.filter in p.name
    )
    if not files:
        print(f"no presentations under {root}", file=sys.stderr)
        return 2

    results: list[Result] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = {pool.submit(run_one, f, args.render, args.timeout): f for f in files}
        for i, fut in enumerate(concurrent.futures.as_completed(futures), 1):
            r = fut.result()
            results.append(r)
            bad = not r.imported or not r.compiled
            if bad or not args.quiet:
                cov = f"{r.coverage:.0%}" if r.coverage is not None else "-"
                status = "ok" if r.compiled else ("COMPILE" if r.imported else "IMPORT")
                print(f"  {r.name[:56]:58s} {status:8s} {cov:>5s} {r.notes:3d}  {r.error[:48]}")
            if i % 100 == 0:
                print(f"  ... {i}/{len(files)}", flush=True)

    results.sort(key=lambda r: r.name)
    imported = sum(r.imported for r in results)
    compiled = sum(r.compiled for r in results)
    covs = [r.coverage for r in results if r.coverage is not None and r.imported]

    print("=" * 74)
    print(f"presentations : {len(results)}")
    print(f"import ok     : {imported}/{len(results)}")
    print(f"compile ok    : {compiled}/{imported}")
    if covs:
        covs.sort()
        mean = sum(covs) / len(covs)
        median = covs[len(covs) // 2]
        print(
            f"text coverage : mean {mean:.1%}  median {median:.1%}  "
            f"min {covs[0]:.1%}  (n={len(covs)})"
        )
    # Compilation failures are the gate: an import that produces broken source
    # is a defect every time, whereas an unreadable package may just be
    # corrupt.
    return 1 if compiled < imported else 0


if __name__ == "__main__":
    raise SystemExit(main())

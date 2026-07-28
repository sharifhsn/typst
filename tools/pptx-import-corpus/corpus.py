#!/usr/bin/env python3
"""Acceptance gate for the PPTX → Typst importer.

The counterpart of `tools/docx-import-corpus/corpus.py`, and it asks the same
two questions of every real presentation we can find:

  1. **Does it import or refuse safely?**  A crash or hang is always a defect;
     an encrypted, malformed, or hostile non-package may be refused cleanly.
  2. **Does the emitted source compile?**  Source that does not compile is
     worse than no source, because it looks like an answer.

The unambiguous release failure is a successful import whose emitted source
does not compile. A third question — does it *look* right — needs a renderer
and a human, and is what `--render` is for.

Coverage is measured the way the Word importer measures it: the fraction of
the deck's text that survives into the emitted source. Text is the one thing
whose loss is never acceptable and always detectable.

    uv run corpus.py fetch
    uv run corpus.py run
    uv run corpus.py run --filter touying --render
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_CORPUS = Path(__file__).resolve().parent / "corpus"
DEFAULT_OUT = Path(__file__).resolve().parent / "out"
TYPST = REPO / "target" / "release" / "typst"

TEXT_RE = re.compile(rb"<a:t>([^<]*)</a:t>")
# Slide parts only: a master's or layout's prompt text ("Click to edit Master
# title style") is template furniture that no importer should reproduce, and
# counting it would make coverage look worse the more faithful we got.
SLIDE_PART_RE = re.compile(r"^ppt/slides/slide\d+\.xml$")

SOURCES = (
    ("poi", "apache/poi", "trunk", "test-data/slideshow"),
    (
        "tika",
        "apache/tika",
        "main",
        "tika-parsers/tika-parsers-standard/tika-parsers-standard-modules/"
        "tika-parser-microsoft-module/src/test/resources/test-documents",
    ),
    ("libreoffice", "LibreOffice/core", "master", "sd/qa/unit/data"),
)

def github_json(url: str) -> dict:
    request = urllib.request.Request(url, headers={"User-Agent": "typst-pptx-corpus"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def resolve_subtree(repo: str, root_tree: str, subtree: str) -> str:
    """Resolve a path component-by-component without requesting a huge root tree."""
    tree_sha = root_tree
    for component in subtree.split("/"):
        tree = github_json(f"https://api.github.com/repos/{repo}/git/trees/{tree_sha}")
        entry = next(
            (
                entry
                for entry in tree.get("tree", [])
                if entry.get("type") == "tree" and entry.get("path") == component
            ),
            None,
        )
        if entry is None:
            raise RuntimeError(f"missing corpus subtree {repo}:{subtree}")
        tree_sha = entry["sha"]
    return tree_sha


def fetch_corpus(dest: Path, jobs: int) -> None:
    """Fetch the public corpus at resolved, immutable Git commits.

    Downloads stage inside the ignored corpus directory, so an interruption
    does not lose completed work and a restart can reuse it. The authoritative
    manifest is replaced only after every candidate has downloaded and hashed.
    """
    dest.mkdir(parents=True, exist_ok=True)
    staging = dest / ".fetch"
    staging.mkdir(exist_ok=True)

    resolved_sources = []
    candidates = []
    seen_blobs = set()
    for label, repo, ref, subtree in SOURCES:
        commit = github_json(
            f"https://api.github.com/repos/{repo}/commits/{urllib.parse.quote(ref)}"
        )
        commit_sha = commit["sha"]
        tree_sha = commit["commit"]["tree"]["sha"]
        subtree_sha = resolve_subtree(repo, tree_sha, subtree)
        tree = github_json(
            f"https://api.github.com/repos/{repo}/git/trees/{subtree_sha}?recursive=1"
        )
        if tree.get("truncated"):
            raise RuntimeError(f"GitHub returned a truncated tree for {repo}@{commit_sha}")
        source_entries = [
            entry
            for entry in tree.get("tree", [])
            if entry.get("type") == "blob"
            and entry.get("path", "").lower().endswith((".pptx", ".pptm"))
        ]
        print(f"{label:<12} {len(source_entries):>5} candidates at {commit_sha[:12]}")
        resolved_sources.append(
            {
                "label": label,
                "repository": repo,
                "requested_ref": ref,
                "commit": commit_sha,
                "subtree": subtree,
                "candidates": len(source_entries),
            }
        )
        for entry in source_entries:
            # Git blob ids are content-addressed, so avoid downloading exact
            # duplicates that occur in more than one upstream test corpus.
            if entry["sha"] in seen_blobs:
                continue
            seen_blobs.add(entry["sha"])
            candidates.append(
                {
                    "label": label,
                    "repo": repo,
                    "commit": commit_sha,
                    "path": f"{subtree}/{entry['path']}",
                    "blob": entry["sha"],
                    "declared_bytes": entry.get("size"),
                }
            )

    def download(candidate: dict) -> dict:
        suffix = Path(candidate["path"]).suffix.lower()
        filename = (
            f"{candidate['label']}-{candidate['blob'][:12]}-"
            f"{Path(candidate['path']).stem}{suffix}"
        )
        staged = staging / filename
        if staged.exists():
            blob = staged.read_bytes()
        else:
            quoted_path = urllib.parse.quote(candidate["path"], safe="/")
            url = (
                f"https://raw.githubusercontent.com/{candidate['repo']}/"
                f"{candidate['commit']}/{quoted_path}"
            )
            request = urllib.request.Request(
                url, headers={"User-Agent": "typst-pptx-corpus"}
            )
            with urllib.request.urlopen(request, timeout=180) as response:
                blob = response.read()
            staged.write_bytes(blob)
        return {
            "file": filename,
            "repository": candidate["repo"],
            "commit": candidate["commit"],
            "path": candidate["path"],
            "git_blob": candidate["blob"],
            "sha256": hashlib.sha256(blob).hexdigest(),
            "bytes": len(blob),
            # One upstream `.pptx` is an encrypted Compound File Binary rather
            # than an OPC ZIP. Keep it as a hostile/unsupported-input fixture;
            # the importer must refuse it cleanly rather than crash.
            "package_kind": "ooxml-zip" if blob.startswith(b"PK") else "non-zip",
        }

    entries = []
    errors = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, jobs)) as pool:
        futures = {pool.submit(download, candidate): candidate for candidate in candidates}
        for index, future in enumerate(concurrent.futures.as_completed(futures), 1):
            candidate = futures[future]
            try:
                entries.append(future.result())
            except Exception as error:
                errors.append(
                    {
                        "repository": candidate["repo"],
                        "commit": candidate["commit"],
                        "path": candidate["path"],
                        "error": str(error),
                    }
                )
                print(f"  ! {candidate['path']}: {error}", file=sys.stderr)
            if index % 100 == 0:
                print(f"  ... downloaded {index}/{len(candidates)}", flush=True)

    if errors:
        (dest / "fetch-errors.json").write_text(json.dumps(errors, indent=2) + "\n")
        raise RuntimeError(
            f"{len(errors)} corpus downloads failed; staged files were retained in {staging}"
        )

    expected = {entry["file"] for entry in entries}
    for entry in entries:
        os.replace(staging / entry["file"], dest / entry["file"])
    for old in dest.iterdir():
        if old.is_file() and old.suffix.lower() in (".pptx", ".pptm") and old.name not in expected:
            old.unlink()
    (dest / "fetch-errors.json").unlink(missing_ok=True)
    try:
        staging.rmdir()
    except OSError:
        pass
    entries.sort(key=lambda entry: (entry["repository"], entry["path"]))
    manifest = {
        "schema": 1,
        "sources": resolved_sources,
        "unique_files": len(entries),
        "ooxml_packages": sum(entry["package_kind"] == "ooxml-zip" for entry in entries),
        "entries": entries,
    }
    (dest / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"kept {len(entries)} unique presentations in {dest}")


@dataclass
class Result:
    name: str
    imported: bool
    compiled: bool
    coverage: float | None
    notes: int
    error: str = ""
    fatal: bool = False


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
                [str(TYPST), "import", str(path), str(out)],
                capture_output=True,
                timeout=timeout,
                text=True,
            )
        except subprocess.TimeoutExpired:
            return Result(name, False, False, None, 0, "import timed out", True)
        if proc.returncode != 0 or not out.exists():
            first = (proc.stderr or "").strip().splitlines()
            error = first[0] if first else "import failed"
            fatal = proc.returncode < 0 or proc.returncode == 0
            return Result(name, False, False, None, 0, error, fatal)

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
            return Result(name, True, False, cov, notes, "compile timed out", True)
        if comp.returncode != 0:
            msg = ""
            for line in (comp.stderr or "").splitlines():
                if line.startswith("error:"):
                    msg = line[len("error:") :].strip()
                    break
            return Result(name, True, False, cov, notes, msg or "compile failed", True)
        return Result(name, True, True, cov, notes)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("command", nargs="?", choices=("run", "fetch"), default="run")
    ap.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--filter", default="")
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) - 2))
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--render", action="store_true", help="compile to PNG instead of PDF")
    ap.add_argument("--quiet", action="store_true", help="only print failures")
    args = ap.parse_args()

    if args.command == "fetch":
        fetch_corpus(args.corpus, args.jobs)
        return 0

    if not TYPST.exists():
        print(f"missing {TYPST}; build it with cargo build --release", file=sys.stderr)
        return 2

    root = args.corpus
    if not root.is_dir():
        print(f"no presentations under {root} (run: corpus.py fetch)", file=sys.stderr)
        return 2
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
                status = (
                    "ok"
                    if r.compiled
                    else "FATAL"
                    if r.fatal
                    else "REFUSED"
                )
                print(f"  {r.name[:56]:58s} {status:8s} {cov:>5s} {r.notes:3d}  {r.error[:48]}")
            if i % 100 == 0:
                print(f"  ... {i}/{len(files)}", flush=True)

    results.sort(key=lambda r: r.name)
    imported = sum(r.imported for r in results)
    compiled = sum(r.compiled for r in results)
    fatal = sum(r.fatal for r in results)
    covs = [r.coverage for r in results if r.coverage is not None and r.imported]

    print("=" * 74)
    print(f"presentations : {len(results)}")
    print(f"import ok     : {imported}/{len(results)}")
    print(f"compile ok    : {compiled}/{imported}")
    print(f"fatal failures: {fatal}")
    if covs:
        covs.sort()
        mean = sum(covs) / len(covs)
        median = covs[len(covs) // 2]
        print(
            f"text coverage : mean {mean:.1%}  median {median:.1%}  "
            f"min {covs[0]:.1%}  (n={len(covs)})"
        )
    args.out.mkdir(parents=True, exist_ok=True)
    results_path = args.out / "corpus-results.json"
    payload = {
        "schema": 1,
        "corpus_manifest": str(args.corpus / "manifest.json"),
        "presentations": len(results),
        "imported": imported,
        "compiled": compiled,
        "fatal_failures": fatal,
        "results": [dataclasses.asdict(result) for result in results],
    }
    results_path.write_text(json.dumps(payload, indent=2) + "\n")
    print(f"wrote {results_path}")
    # Compilation failures are the gate: an import that produces broken source
    # is a defect every time, whereas an unreadable package may just be
    # corrupt.
    return 1 if fatal or compiled < imported else 0


if __name__ == "__main__":
    raise SystemExit(main())

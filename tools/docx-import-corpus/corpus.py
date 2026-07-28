# /// script
# requires-python = ">=3.11"
# ///
"""Run the DOCX → Typst importer over a corpus of real-world Word documents.

This is the importer's acceptance gate, the counterpart to
`tools/docx-validate` on the export side. The metric differs accordingly: the
exporter is judged on page fidelity, the importer on **text coverage** — the
first question is not "does it look identical" but "did the content survive".

For each document it records
  * whether the import succeeds (a clean error on a malformed or hostile
    document is a *correct* outcome, not a failure),
  * whether the emitted Typst source compiles,
  * text coverage: the word-set overlap between a LibreOffice render of the
    source .docx and a Typst render of the import,
  * how many `ImportReport` notes were raised.

The default corpus is Apache POI's `test-data/document` directory — 128
genuinely real-world documents produced by Word and LibreOffice, and
deliberately including fuzzer-corrupted, truncated, encrypted and torture
files. Fetch it with `--fetch`.

Usage:
    uv run corpus.py --fetch                 # download into ./poi-docs (~8 MB)
    uv run corpus.py                         # run everything
    uv run corpus.py --filter headerFooter   # run one document
    uv run corpus.py --baseline out.json     # compare against a saved run

Requires `soffice` (LibreOffice) and `pdftotext` (poppler) on PATH, and a
release Typst binary.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TYPST = REPO / "target" / "release" / "typst"
POI_REPO = "apache/poi"
POI_REF = "trunk"
POI_PATH = "test-data/document"
DEFAULT_CORPUS = REPO / "tools" / "docx-import-corpus" / "poi-docs"
DEFAULT_OUT = REPO / "tools" / "docx-import-corpus" / "poi-out"

# Two or more letters: digits and punctuation are too noisy to compare across
# two different renderers' line breaking and hyphenation.
WORD = re.compile(r"[^\W\d_]{2,}", re.UNICODE)


def words(text: str) -> set[str]:
    return {w.lower() for w in WORD.findall(text)}


def pdf_words(pdf: Path) -> set[str]:
    try:
        r = subprocess.run(["pdftotext", str(pdf), "-"], capture_output=True, timeout=60)
        return words(r.stdout.decode("utf-8", "replace"))
    except Exception:
        return set()


def soffice_pdf(docx: Path, wd: Path, slot: int) -> Path | None:
    """Convert with a private profile so parallel workers don't collide."""
    profile = Path(tempfile.gettempdir()) / f"lo-profile-{slot}"
    try:
        subprocess.run(
            [
                "soffice",
                f"-env:UserInstallation=file://{profile}",
                "--headless",
                "--convert-to",
                "pdf",
                "--outdir",
                str(wd),
                str(docx),
            ],
            capture_output=True,
            timeout=180,
        )
    except Exception:
        return None
    pdf = wd / (docx.stem + ".pdf")
    return pdf if pdf.exists() else None


def run_one(docx: Path, out_dir: Path, slot: int) -> dict:
    wd = out_dir / docx.stem
    wd.mkdir(parents=True, exist_ok=True)
    rec: dict = {
        "name": docx.stem,
        "import": "FAIL",
        "compile": "-",
        "coverage": None,
        "notes": 0,
        "fatal": False,
    }

    typ = wd / "doc.typ"
    try:
        r = subprocess.run(
            [str(TYPST), "import", str(docx), str(typ)],
            capture_output=True,
            timeout=120,
        )
        log = (r.stdout + r.stderr).decode("utf-8", "replace")
        (wd / "import.log").write_text(log)
        if r.returncode == 0 and typ.exists() and typ.stat().st_size > 0:
            rec["import"] = "ok"
            rec["notes"] = sum(1 for line in log.splitlines() if line.startswith("- ["))
        else:
            tail = log.strip().splitlines()
            rec["error"] = tail[-1][:90] if tail else f"exit {r.returncode}"
            rec["fatal"] = r.returncode < 0 or r.returncode == 0
    except subprocess.TimeoutExpired:
        rec["error"] = "import timeout"
        rec["fatal"] = True
        return rec
    if rec["import"] != "ok":
        return rec

    imp_pdf = wd / "imp.pdf"
    try:
        c = subprocess.run(
            [str(TYPST), "compile", "--root", str(wd), str(typ), str(imp_pdf)],
            capture_output=True,
            timeout=180,
        )
        clog = (c.stdout + c.stderr).decode("utf-8", "replace")
        (wd / "compile.log").write_text(clog)
        rec["compile"] = "ok" if c.returncode == 0 else "FAIL"
        if c.returncode != 0:
            rec["fatal"] = True
            errs = [line for line in clog.splitlines() if "error" in line.lower()]
            rec["error"] = (errs[0] if errs else clog.splitlines()[0] if clog else "")[:90]
    except subprocess.TimeoutExpired:
        rec["compile"] = "TIMEOUT"
        rec["fatal"] = True
        return rec

    src_pdf = soffice_pdf(docx, wd, slot)
    if src_pdf:
        source = pdf_words(src_pdf)
        if source and rec["compile"] == "ok":
            imported = pdf_words(imp_pdf)
            rec["coverage"] = round(100.0 * len(source & imported) / len(source))
            rec["missing"] = sorted(source - imported)[:12]
        src_pdf.unlink(missing_ok=True)
    # Keep the .typ and the logs (small, diagnostic); drop the bulky PDF so a
    # full run stays bounded on disk.
    imp_pdf.unlink(missing_ok=True)
    return rec


def fetch_corpus(dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    headers = {"User-Agent": "typst-docx-corpus"}
    commit_request = urllib.request.Request(
        f"https://api.github.com/repos/{POI_REPO}/commits/{POI_REF}", headers=headers
    )
    with urllib.request.urlopen(commit_request, timeout=60) as fh:
        commit = json.load(fh)["sha"]
    contents_request = urllib.request.Request(
        f"https://api.github.com/repos/{POI_REPO}/contents/{POI_PATH}?ref={commit}",
        headers=headers,
    )
    with urllib.request.urlopen(contents_request, timeout=60) as fh:
        entries = json.load(fh)
    docs = [e for e in entries if e["name"].lower().endswith(".docx")]
    print(f"fetching {len(docs)} documents from {POI_REPO}@{commit[:12]} into {dest}")

    def git_blob_sha(data: bytes) -> str:
        header = f"blob {len(data)}\0".encode()
        return hashlib.sha1(header + data).hexdigest()

    def get(entry: dict) -> dict:
        path = dest / entry["name"]
        data = path.read_bytes() if path.exists() else b""
        status = "skip"
        if git_blob_sha(data) != entry["sha"]:
            source_path = urllib.parse.quote(f"{POI_PATH}/{entry['name']}", safe="/")
            request = urllib.request.Request(
                f"https://raw.githubusercontent.com/{POI_REPO}/{commit}/{source_path}",
                headers=headers,
            )
            with urllib.request.urlopen(request, timeout=120) as response:
                data = response.read()
            if git_blob_sha(data) != entry["sha"]:
                raise RuntimeError(f"Git blob hash mismatch for {entry['name']}")
            path.write_bytes(data)
            status = "ok"
        return {
            "file": entry["name"],
            "bytes": len(data),
            "git_blob": entry["sha"],
            "sha256": hashlib.sha256(data).hexdigest(),
            "status": status,
        }

    records = []
    errors = []
    with concurrent.futures.ThreadPoolExecutor(8) as executor:
        future_entries = {executor.submit(get, entry): entry for entry in docs}
        for future in concurrent.futures.as_completed(future_entries):
            entry = future_entries[future]
            try:
                records.append(future.result())
            except Exception as error:
                errors.append(f"{entry['name']}: {error}")

    if errors:
        for error in errors:
            print(f"  ERR {error}", file=sys.stderr)
        raise RuntimeError(f"{len(errors)} corpus downloads failed")

    records.sort(key=lambda record: record["file"])
    fetched = sum(record.pop("status") == "ok" for record in records)
    manifest = {
        "schema": 1,
        "source": {
            "repository": POI_REPO,
            "requested_ref": POI_REF,
            "commit": commit,
            "subtree": POI_PATH,
        },
        "documents": len(records),
        "entries": records,
    }
    (dest / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"  {fetched} downloaded, {len(records) - fetched} already verified")


def summarize(results: list[dict], baseline: list[dict] | None) -> None:
    ok_import = sum(1 for r in results if r["import"] == "ok")
    ok_compile = sum(1 for r in results if r["compile"] == "ok")
    fatal = sum(1 for r in results if r.get("fatal"))
    covs = [r["coverage"] for r in results if r["coverage"] is not None]

    print("\n" + "=" * 78)
    print(f"documents      : {len(results)}")
    print(f"import ok      : {ok_import}/{len(results)}")
    print(f"compile ok     : {ok_compile}/{ok_import}")
    print(f"fatal failures : {fatal}")
    if covs:
        ordered = sorted(covs)
        print(
            f"coverage       : mean {sum(covs) / len(covs):.1f}%  "
            f"median {ordered[len(covs) // 2]}%  min {min(covs)}%  (n={len(covs)})"
        )
        for label, lo, hi in [
            ("100%", 100, 101),
            ("90-99", 90, 100),
            ("50-89", 50, 90),
            ("<50", 0, 50),
        ]:
            print(f"  {label:>6}: {sum(1 for c in covs if lo <= c < hi)}")

    if baseline is None:
        return
    before = {r["name"]: r for r in baseline}
    changed = []
    for r in results:
        b = before.get(r["name"])
        if not b:
            continue
        if b["coverage"] is not None and r["coverage"] is not None:
            if b["coverage"] != r["coverage"]:
                changed.append((r["coverage"] - b["coverage"], r["name"], b["coverage"], r["coverage"]))
        if (b["import"], b["compile"]) != (r["import"], r["compile"]):
            print(
                f"  STATUS {r['name'][:42]:<42} "
                f"{b['import']}/{b['compile']} -> {r['import']}/{r['compile']}"
            )
    if changed:
        print("\ncoverage changes vs baseline:")
        for delta, name, was, now in sorted(changed):
            print(f"  {name[:44]:<44} {was:>3}% -> {now:>3}%  {delta:+d}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS,
                    help=f"corpus directory (default {DEFAULT_CORPUS})")
    ap.add_argument("--out", type=Path, default=None,
                    help=f"work dir (default {DEFAULT_OUT})")
    ap.add_argument("--fetch", action="store_true", help="download the POI corpus first")
    ap.add_argument("--filter", default=None, help="only documents whose name contains this")
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--baseline", type=Path, default=None, help="compare against a saved results.json")
    args = ap.parse_args()

    if args.fetch:
        fetch_corpus(args.corpus)

    if not TYPST.exists():
        print(f"missing {TYPST}\nbuild it with:\n  cargo build --release", file=sys.stderr)
        return 2

    docs = sorted(p for p in args.corpus.glob("*.docx"))
    if args.filter:
        docs = [d for d in docs if args.filter.lower() in d.name.lower()]
    if not docs:
        print(f"no .docx found in {args.corpus} (try --fetch)", file=sys.stderr)
        return 2

    out_dir = args.out or DEFAULT_OUT
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)

    print(f"{'DOCUMENT':<44} {'IMPORT':>6} {'COMPILE':>7} {'COV':>6} {'NOTE':>4}")
    results: list[dict] = []
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as ex:
        futures = [
            ex.submit(run_one, doc, out_dir, i % args.jobs) for i, doc in enumerate(docs)
        ]
        for fut in futures:
            rec = fut.result()
            results.append(rec)
            cov = "-" if rec["coverage"] is None else f"{rec['coverage']}%"
            print(
                f"{rec['name'][:44]:<44} {rec['import']:>6} {rec['compile']:>7} "
                f"{cov:>6} {rec['notes']:>4}  {rec.get('error', '')[:50]}",
                flush=True,
            )

    baseline = json.loads(args.baseline.read_text()) if args.baseline else None
    summarize(results, baseline)

    results_path = out_dir / "results.json"
    results_path.write_text(json.dumps(results, indent=1))
    print(f"\nwrote {results_path}")

    # A non-zero exit means a document that imported produced source that does
    # not compile — the one outcome that is always a genuine defect.
    broken = [r for r in results if r["import"] == "ok" and r["compile"] != "ok"]
    fatal = [r for r in results if r.get("fatal")]
    return 1 if broken or fatal else 0


if __name__ == "__main__":
    raise SystemExit(main())

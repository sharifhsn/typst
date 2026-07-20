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
    python3 corpus.py --fetch                 # download the corpus (~8 MB)
    python3 corpus.py                         # run everything
    python3 corpus.py --filter headerFooter   # run one document
    python3 corpus.py --baseline out.json     # compare against a saved run

Requires `soffice` (LibreOffice) and `pdftotext` (poppler) on PATH, and a
release Typst binary.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TYPST = REPO / "target" / "release" / "typst"
IMPORTER = REPO / "target" / "debug" / "examples" / "import"
POI_API = (
    "https://api.github.com/repos/apache/poi/contents/test-data/document?ref=trunk"
)

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
    }

    typ = wd / "doc.typ"
    try:
        r = subprocess.run(
            [str(IMPORTER), str(docx), str(typ)], capture_output=True, timeout=120
        )
        log = (r.stdout + r.stderr).decode("utf-8", "replace")
        (wd / "import.log").write_text(log)
        if r.returncode == 0 and typ.exists() and typ.stat().st_size > 0:
            rec["import"] = "ok"
            rec["notes"] = sum(1 for line in log.splitlines() if line.startswith("- ["))
        else:
            tail = log.strip().splitlines()
            rec["error"] = tail[-1][:90] if tail else f"exit {r.returncode}"
    except subprocess.TimeoutExpired:
        rec["error"] = "import timeout"
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
            errs = [line for line in clog.splitlines() if "error" in line.lower()]
            rec["error"] = (errs[0] if errs else clog.splitlines()[0] if clog else "")[:90]
    except subprocess.TimeoutExpired:
        rec["compile"] = "TIMEOUT"
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
    with urllib.request.urlopen(POI_API) as fh:
        entries = json.load(fh)
    docs = [e for e in entries if e["name"].lower().endswith(".docx")]
    print(f"fetching {len(docs)} documents into {dest}")

    def get(entry: dict) -> str:
        path = dest / entry["name"]
        if path.exists() and path.stat().st_size == entry["size"]:
            return "skip"
        try:
            urllib.request.urlretrieve(entry["download_url"], path)
            return "ok"
        except Exception as exc:  # noqa: BLE001 - report and continue
            return f"ERR {exc}"

    with concurrent.futures.ThreadPoolExecutor(8) as ex:
        results = list(ex.map(get, docs))
    fetched = sum(1 for r in results if r == "ok")
    errors = [r for r in results if r.startswith("ERR")]
    print(f"  {fetched} downloaded, {len(results) - fetched - len(errors)} already present")
    for err in errors:
        print(f"  {err}")


def summarize(results: list[dict], baseline: list[dict] | None) -> None:
    ok_import = sum(1 for r in results if r["import"] == "ok")
    ok_compile = sum(1 for r in results if r["compile"] == "ok")
    covs = [r["coverage"] for r in results if r["coverage"] is not None]

    print("\n" + "=" * 78)
    print(f"documents      : {len(results)}")
    print(f"import ok      : {ok_import}/{len(results)}")
    print(f"compile ok     : {ok_compile}/{ok_import}")
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
    ap.add_argument("--corpus", type=Path, default=Path("/tmp/docx-poi"))
    ap.add_argument("--out", type=Path, default=None, help="work dir (default <corpus>/out)")
    ap.add_argument("--fetch", action="store_true", help="download the POI corpus first")
    ap.add_argument("--filter", default=None, help="only documents whose name contains this")
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--baseline", type=Path, default=None, help="compare against a saved results.json")
    args = ap.parse_args()

    if args.fetch:
        fetch_corpus(args.corpus)

    if not IMPORTER.exists():
        print(f"missing {IMPORTER}\nbuild it with:", file=sys.stderr)
        print("  cargo build -p typst-docx-import --example import", file=sys.stderr)
        return 2
    if not TYPST.exists():
        print(f"missing {TYPST}\nbuild it with:\n  cargo build --release", file=sys.stderr)
        return 2

    docs = sorted(p for p in args.corpus.glob("*.docx"))
    if args.filter:
        docs = [d for d in docs if args.filter.lower() in d.name.lower()]
    if not docs:
        print(f"no .docx found in {args.corpus} (try --fetch)", file=sys.stderr)
        return 2

    out_dir = args.out or (args.corpus / "out")
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

    results_path = args.corpus / "results.json"
    results_path.write_text(json.dumps(results, indent=1))
    print(f"\nwrote {results_path}")

    # A non-zero exit means a document that imported produced source that does
    # not compile — the one outcome that is always a genuine defect.
    broken = [r for r in results if r["import"] == "ok" and r["compile"] != "ok"]
    return 1 if broken else 0


if __name__ == "__main__":
    raise SystemExit(main())

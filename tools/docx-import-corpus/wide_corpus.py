# /// script
# requires-python = ">=3.11"
# ///
"""Gather and run a *wide* real-world .docx corpus for the importer.

`corpus.py` runs Apache POI's 128 documents and measures text coverage. That
is the right routine gate — it is quick and it measures fidelity. This is the
complement: ~2,500 documents drawn from several projects' test suites, run
only for "does it import, does the output compile".

Those two questions are the ones whose failures are *always* genuine defects,
and they are cheap enough to ask of every document. Breadth is the point: a
single project's suite is biased toward that project's own history, whereas
across LibreOffice, Tika, POI, pandoc and a few library fixtures you get
documents from sixteen different producers — several Word versions including
Mac and Outlook, LibreOffice, Collabora, OpenOffice, WPS, OnlyOffice,
TextMaker — in sixty-odd languages. Most are bug-report attachments, i.e.
documents that already broke somebody's word processor.

    uv run wide_corpus.py fetch          # ~65 MB, deduplicated by content
    uv run wide_corpus.py describe       # producers, features, languages
    uv run wide_corpus.py run            # import + compile over everything

Exits non-zero if any document that imported produced source that does not
compile — the outcome that is always a bug, as opposed to a clean refusal of a
malformed package, which is correct behaviour.
"""

import argparse
import collections
import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DIR = os.path.join(ROOT, "wide-docs")
MAX_TOTAL_BYTES = 3 * 1024**3
MAX_FILE_BYTES = 30 * 1024**2

# (label, repo, ref, subtree). Enumerated recursively; every .docx is taken.
SOURCES = [
    ("lo-sw", "LibreOffice/core", "master", "sw/qa"),
    ("tika", "apache/tika", "main", "tika-parsers"),
    ("poi", "apache/poi", "trunk", "test-data"),
    ("pandoc", "jgm/pandoc", "main", "test"),
    ("mammoth", "mwilliamson/mammoth.js", "master", "test"),
    ("docxjs", "dolanmiu/docx", "master", "demo"),
    ("docx-preview", "VolodymyrBaydalka/docxjs", "master", "tests"),
    ("python-docx", "python-openxml/python-docx", "master", "tests"),
]

FEATURES = {
    "table": r"<w:tbl[ >]",
    "image": r"<a:blip[ >]|<v:imagedata[ >]",
    "header/footer": r"<w:headerReference|<w:footerReference",
    "footnote/endnote": r"<w:footnoteReference|<w:endnoteReference",
    "field": r"<w:fldSimple|<w:fldChar",
    "OMML math": r"<m:oMath[ >]",
    "content control": r"<w:sdt[ >]",
    "tracked change": r"<w:ins[ >]|<w:del[ >]",
    "text box": r"<w:txbxContent[ >]",
    "chart": r"<c:chart[ >]|<cx:chart[ >]",
    "VML": r"<v:(shape|rect|line|oval|group)[ >]",
    "MCE alt-content": r"<mc:AlternateContent[ >]",
    "numbering": r"<w:numPr[ >]",
    "hyperlink": r"<w:hyperlink[ >]",
    "ruby": r"<w:ruby[ >]",
    "smartTag": r"<w:smartTag[ >]",
    "comment": r"<w:commentReference",
    "bookmark": r"<w:bookmarkStart",
    "RTL/bidi": r"<w:bidi[ />]|<w:rtl[ />]",
    "CJK font": r'w:eastAsia="[^"]+"',
    "embedded object": r"<w:object[ >]",
}


def gh(path):
    r = subprocess.run(["gh", "api", path], capture_output=True, timeout=180)
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return None


def resolve_commit(repo, ref):
    commit = gh(f"repos/{repo}/commits/{urllib.parse.quote(ref, safe='')}")
    if not isinstance(commit, dict) or not commit.get("sha"):
        raise RuntimeError(f"could not resolve {repo}@{ref} to an immutable commit")
    return commit["sha"]


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def enumerate_source(label, repo, ref, path):
    parent = os.path.dirname(path) or ""
    siblings = gh(f"repos/{repo}/contents/{parent}?ref={ref}") or []
    sha = next(
        (s.get("sha") for s in siblings
         if isinstance(s, dict) and s.get("path") == path and s.get("type") == "dir"),
        None,
    )
    if sha is None:
        return []
    tree = gh(f"repos/{repo}/git/trees/{sha}?recursive=1") or {}
    if tree.get("truncated"):
        print(f"  ! {label}: tree truncated, listing is partial", file=sys.stderr)
    out = []
    for e in tree.get("tree", []):
        p = e.get("path", "")
        if e.get("type") == "blob" and p.lower().endswith(".docx"):
            if 0 < (e.get("size") or 0) <= MAX_FILE_BYTES:
                out.append((label, repo, ref, f"{path}/{p}"))
    return out


def cmd_fetch(args):
    os.makedirs(args.dir, exist_ok=True)
    found, resolved_sources = [], []
    for label, repo, ref, path in SOURCES:
        commit = resolve_commit(repo, ref)
        items = enumerate_source(label, repo, commit, path)
        print(f"{label:<13} {repo:<32} {len(items):>5} .docx  {commit[:12]}")
        resolved_sources.append({
            "label": label,
            "repository": repo,
            "requested_ref": ref,
            "commit": commit,
            "subtree": path,
            "candidates": len(items),
        })
        found.extend(items)
    print(f"\ncandidates: {len(found)}")

    def get(item):
        _label, repo, ref, path = item
        url = (f"https://raw.githubusercontent.com/{repo}/{ref}/"
               f"{urllib.parse.quote(path)}")
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                return item, r.read()
        except Exception:
            return item, None

    seen, total, kept, manifest = set(), 0, 0, []
    with concurrent.futures.ThreadPoolExecutor(12) as ex:
        for item, blob in ex.map(get, found):
            # A .docx is a zip; anything else is a bad fetch, not a document.
            if not blob or not blob.startswith(b"PK"):
                continue
            digest = hashlib.sha256(blob).hexdigest()
            if digest in seen or total + len(blob) > MAX_TOTAL_BYTES:
                continue
            seen.add(digest)
            label, repo, commit, path = item
            name = f"{label}-{os.path.basename(path)}"[:120]
            dest = os.path.join(args.dir, name)
            write_blob = True
            if os.path.exists(dest) and sha256_file(dest) == digest:
                write_blob = False
            elif os.path.exists(dest):
                n = 1
                while True:
                    candidate = os.path.join(args.dir, f"{name[:-5]}~{n}.docx")
                    if not os.path.exists(candidate):
                        dest = candidate
                        break
                    if sha256_file(candidate) == digest:
                        dest = candidate
                        write_blob = False
                        break
                    n += 1
            if write_blob:
                with open(dest, "wb") as fh:
                    fh.write(blob)
            manifest.append({
                "file": os.path.basename(dest),
                "repository": repo,
                "commit": commit,
                "path": path,
                "sha256": digest,
                "bytes": len(blob),
            })
            total += len(blob)
            kept += 1
    manifest.sort(key=lambda entry: (entry["repository"], entry["path"]))
    authority = {
        "schema": 1,
        "sources": resolved_sources,
        "unique_files": kept,
        "bytes": total,
        "entries": manifest,
    }
    expected_files = {entry["file"] for entry in manifest}
    for old in os.listdir(args.dir):
        if old.lower().endswith(".docx") and old not in expected_files:
            os.remove(os.path.join(args.dir, old))
    with open(os.path.join(args.dir, "manifest.json"), "w") as fh:
        json.dump(authority, fh, indent=1)
        fh.write("\n")
    print(f"kept {kept} unique documents, {total/1024**2:.1f} MB -> {args.dir}")


def cmd_describe(args):
    import zipfile

    files = sorted(f for f in os.listdir(args.dir) if f.lower().endswith(".docx"))
    producers, feats, langs, sizes, broken = (
        collections.Counter(), collections.Counter(), collections.Counter(), [], 0)
    for name in files:
        path = os.path.join(args.dir, name)
        try:
            z = zipfile.ZipFile(path)
            parts = [z.read(n).decode("utf8", "replace") for n in z.namelist()
                     if n.endswith((".xml", ".rels"))]
        except Exception:
            broken += 1
            continue
        sizes.append(os.path.getsize(path))
        xml = "\n".join(parts)
        m = re.search(r"<Application>([^<]+)</Application>", xml)
        producers[re.split(r"[/,]", m.group(1))[0].strip() if m else "(unstated)"] += 1
        for code in set(re.findall(r'w:val="([a-z]{2}-[A-Z]{2})"', xml)):
            langs[code] += 1
        for fname, pat in FEATURES.items():
            if re.search(pat, xml, re.S):
                feats[fname] += 1

    n = len(sizes) or 1
    print(f"documents : {len(files)} ({broken} unreadable as zip)")
    print(f"size      : {sum(sizes)/1024**2:.1f} MB total, "
          f"{sorted(sizes)[len(sizes)//2]/1024:.0f} KB median")
    print(f"\nproducers ({len(producers)}):")
    for app, c in producers.most_common(20):
        print(f"  {app[:50]:<50} {c:>5}")
    print("\nfeature coverage:")
    for fname in FEATURES:
        c = feats[fname]
        print(f"  {fname:<18} {c:>5}  {'#' * min(40, int(40 * c / n))}")
    print(f"\nlanguages ({len(langs)}): " +
          "  ".join(f"{k}:{v}" for k, v in langs.most_common(18)))


def cmd_run(args):
    out = os.path.join(ROOT, "wide-out")
    if os.path.exists(out):
        shutil.rmtree(out)
    os.makedirs(out)
    docs = sorted(os.path.join(args.dir, f) for f in os.listdir(args.dir)
                  if f.lower().endswith(".docx"))
    manifest_path = os.path.join(args.dir, "manifest.json")
    if not os.path.isfile(manifest_path):
        raise RuntimeError(f"missing immutable corpus manifest: {manifest_path}; run fetch")
    with open(manifest_path) as fh:
        manifest = json.load(fh)
    if not isinstance(manifest, dict) or manifest.get("schema") != 1:
        raise RuntimeError("wide corpus manifest has an unsupported schema; re-fetch it")
    expected = {entry["file"]: entry for entry in manifest.get("entries", [])}
    actual = {os.path.basename(path) for path in docs}
    if actual != set(expected):
        missing = sorted(set(expected) - actual)
        extra = sorted(actual - set(expected))
        raise RuntimeError(
            f"wide corpus differs from its manifest: missing={missing[:5]} extra={extra[:5]}"
        )
    for path in docs:
        name = os.path.basename(path)
        if sha256_file(path) != expected[name]["sha256"]:
            raise RuntimeError(f"wide corpus hash mismatch: {name}; re-fetch it")
    if args.filter:
        docs = [d for d in docs if args.filter.lower() in os.path.basename(d).lower()]

    def one(path):
        name = os.path.basename(path)[:-5]
        wd = os.path.join(out, name[:80])
        os.makedirs(wd, exist_ok=True)
        typ = os.path.join(wd, "doc.typ")
        rec = {
            "name": name,
            "import": "FAIL",
            "compile": "-",
            "error": "",
            "fatal": False,
        }
        try:
            r = subprocess.run([args.typst, "import", path, typ], capture_output=True,
                               timeout=120)
        except subprocess.TimeoutExpired:
            rec["error"] = "import TIMEOUT"
            rec["fatal"] = True
            return rec
        if r.returncode != 0:
            log = (r.stdout + r.stderr).decode("utf8", "replace").strip().splitlines()
            rec["error"] = ("CRASH" if r.returncode < 0
                            else (log[-1] if log else f"exit {r.returncode}"))[:110]
            rec["fatal"] = r.returncode < 0
            return rec
        rec["import"] = "ok"
        # An empty result is the right answer for a document with no content.
        if os.path.getsize(typ) == 0:
            rec["compile"] = "ok"
            return rec
        try:
            c = subprocess.run([args.typst, "compile", "--root", wd, typ,
                                os.path.join(wd, "o.pdf")],
                               capture_output=True, timeout=180)
        except subprocess.TimeoutExpired:
            rec["compile"] = "TIMEOUT"
            rec["fatal"] = True
            return rec
        rec["compile"] = "ok" if c.returncode == 0 else "FAIL"
        if c.returncode != 0:
            rec["fatal"] = True
            log = (c.stdout + c.stderr).decode("utf8", "replace")
            errs = [l.strip() for l in log.splitlines() if l.lower().startswith("error")]
            rec["error"] = (errs[0] if errs else "")[:110]
        pdf = os.path.join(wd, "o.pdf")
        if os.path.exists(pdf):
            os.remove(pdf)
        return rec

    results = []
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as ex:
        for i, rec in enumerate(ex.map(one, docs)):
            results.append(rec)
            if rec["import"] != "ok" or rec["compile"] not in ("ok", "-"):
                print(f"  {rec['name'][:56]:<56} {rec['import']:>4}/{rec['compile']:<6} "
                      f"{rec['error'][:58]}", flush=True)
            if (i + 1) % 500 == 0:
                print(f"  ... {i+1}/{len(docs)}", flush=True)

    ok_i = sum(1 for r in results if r["import"] == "ok")
    ok_c = sum(1 for r in results if r["compile"] == "ok")
    print("\n" + "=" * 70)
    print(f"documents   : {len(results)}")
    print(f"import ok   : {ok_i}/{len(results)}")
    print(f"compile ok  : {ok_c}/{ok_i}")
    fatal = sum(1 for r in results if r.get("fatal"))
    print(f"fatal       : {fatal}")
    authority = {
        "schema": 1,
        "typst": {
            "path": args.typst,
            "sha256": sha256_file(args.typst),
        },
        "corpus": {
            "manifest": manifest_path,
            "manifest_sha256": sha256_file(manifest_path),
            "sources": manifest["sources"],
            "unique_files": manifest["unique_files"],
            "selected_files": len(docs),
        },
        "summary": {
            "documents": len(results),
            "import_ok": ok_i,
            "compile_ok": ok_c,
            "fatal": fatal,
        },
        "results": results,
    }
    with open(os.path.join(ROOT, "wide-results.json"), "w") as fh:
        json.dump(authority, fh, indent=1)
        fh.write("\n")
    return 1 if fatal or ok_c < ok_i else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("command", choices=["fetch", "describe", "run"])
    ap.add_argument("--dir", default=DEFAULT_DIR)
    ap.add_argument("--filter")
    ap.add_argument("--jobs", type=int, default=10)
    ap.add_argument("--typst", default=os.path.join(ROOT, "../../target/release/typst"))
    args = ap.parse_args()
    args.typst = os.path.abspath(args.typst)
    return {"fetch": cmd_fetch, "describe": cmd_describe, "run": cmd_run}[
        args.command](args) or 0


if __name__ == "__main__":
    sys.exit(main())

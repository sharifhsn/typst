# /// script
# requires-python = ">=3.11"
# ///
"""Round-trip fidelity, in both directions.

Neither loop can be byte-identical and neither is meant to be — two producers
emit entirely different, equally valid OOXML for the same document. Round-trip
is useful as a *signal*: it exercises the importer and the exporter against
each other, and it catches the class of bug that compiling cleanly cannot,
namely output that is perfectly valid and quietly means something else.

    docx    original.docx -> .typ -> .docx     compares structure
    typst   original.typ  -> .docx -> .typ     compares rendered text + pages

The `typst` direction has the cleaner oracle: both ends are rendered by Typst
itself, so no third renderer's opinion enters the comparison and any
difference is attributable to one of the two legs.

Counting headings is the subtle part, and the reason this doesn't just grep
for Word's built-in style ids: a localized document writes its headings under
a localized style id (`berschrift1` for "Überschrift 1"), or under a bare
numeric id whose *name* is "heading 2". Matching on `w:val="Heading…"` scores
those as zero headings, which makes a faithful round trip look like it is
inventing structure. Heading-ness is resolved the way the importer resolves
it: through the style's name and outline level, transitively along `basedOn`.

    python3 roundtrip.py docx  --dir <docx-dir> [--limit N]
    python3 roundtrip.py typst --dir <typst-repo-dir> [--limit N]
"""

import argparse
import concurrent.futures
import os
import re
import shutil
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.abspath(__file__))
WORD = re.compile(r"[^\W\d_]{2,}", re.UNICODE)


def words(t):
    return set(w.lower() for w in WORD.findall(t))


def run(cmd, timeout=300):
    try:
        r = subprocess.run(cmd, capture_output=True, timeout=timeout)
        return r.returncode, (r.stdout + r.stderr).decode("utf8", "replace")
    except subprocess.TimeoutExpired:
        return 124, "timeout"


def sample(items, limit):
    """Even stride, so a limited run still spans the whole corpus."""
    if limit and len(items) > limit:
        step = len(items) / limit
        return [items[int(i * step)] for i in range(limit)]
    return items


# --- docx -> typst -> docx ----------------------------------------------------

def heading_styles(styles_xml):
    direct, based_on = {}, {}
    for m in re.finditer(r'<w:style\b[^>]*w:styleId="([^"]+)"[^>]*>(.*?)</w:style>',
                         styles_xml, re.S):
        sid, body = m.group(1), m.group(2)
        b = re.search(r'<w:basedOn w:val="([^"]+)"', body)
        if b:
            based_on[sid] = b.group(1)
        lvl = re.search(r'<w:outlineLvl w:val="(\d+)"', body)
        name = re.search(r'<w:name w:val="([^"]+)"', body)
        n = (name.group(1) if name else "").lower().replace(" ", "")
        head = bool((lvl and int(lvl.group(1)) <= 8)
                    or re.fullmatch(r"heading\d", n)
                    or re.fullmatch(r"heading\d", sid.lower()))
        # An explicit outlineLvl of 9 means body text, and overrides the name.
        if lvl and int(lvl.group(1)) == 9:
            head = False
        direct[sid] = head

    def resolve(sid, seen=()):
        if sid in seen:
            return False
        if direct.get(sid):
            return True
        parent = based_on.get(sid)
        return resolve(parent, seen + (sid,)) if parent else False

    return {sid for sid in direct if resolve(sid)}


def structure(docx):
    try:
        z = zipfile.ZipFile(docx)
        doc = z.read("word/document.xml").decode("utf8", "replace")
    except Exception:
        return None
    try:
        styles = z.read("word/styles.xml").decode("utf8", "replace")
    except Exception:
        styles = ""
    heads = heading_styles(styles)
    used = re.findall(r'<w:pStyle w:val="([^"]+)"', doc)
    return {
        "paragraphs": len(re.findall(r"<w:p[ >]", doc)),
        "headings": sum(1 for s in used if s in heads),
        "tables": len(re.findall(r"<w:tbl[ >]", doc)),
        "rows": len(re.findall(r"<w:tr[ >]", doc)),
        "images": len(re.findall(r"<a:blip[ >]|<v:imagedata[ >]", doc)),
    }


def docx_leg(args, src):
    wd = os.path.join(args.out, os.path.basename(src)[:-5][:80])
    os.makedirs(wd, exist_ok=True)
    typ, back = os.path.join(wd, "d.typ"), os.path.join(wd, "back.docx")
    if run([args.importer, src, typ], 120)[0] != 0 or os.path.getsize(typ) == 0:
        return None
    if run([args.typst, "compile", "--root", wd, "--format", "docx", typ, back])[0] != 0:
        return None
    a, b = structure(src), structure(back)
    if os.path.exists(back):
        os.remove(back)
    return (a, b) if a and b else None


def cmd_docx(args):
    docs = sample(sorted(os.path.join(args.dir, f) for f in os.listdir(args.dir)
                         if f.lower().endswith(".docx")), args.limit)
    pairs = []
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as ex:
        for r in ex.map(lambda s: docx_leg(args, s), docs):
            if r:
                pairs.append(r)
    print(f"docx -> typst -> docx, {len(pairs)}/{len(docs)} completed the loop\n")
    print(f"  {'':<11}{'original':>10}{'round-trip':>12}{'ratio':>8}")
    for k in ("paragraphs", "headings", "tables", "rows", "images"):
        a = sum(p[0][k] for p in pairs)
        b = sum(p[1][k] for p in pairs)
        print(f"  {k:<11}{a:>10}{b:>12}{(f'{100*b/a:.0f}%' if a else '-'):>8}")
    return 0


# --- typst -> docx -> typst ---------------------------------------------------

def pdf_info(pdf):
    if not os.path.exists(pdf):
        return set(), 0
    rc, out = run(["pdftotext", pdf, "-"], 90)
    w = words(out) if rc == 0 else set()
    _rc, info = run(["pdfinfo", pdf], 30)
    m = re.search(r"Pages:\s+(\d+)", info)
    return w, int(m.group(1)) if m else 0


def typst_leg(args, src):
    wd = os.path.join(args.out, os.path.basename(os.path.dirname(src))[:80])
    os.makedirs(wd, exist_ok=True)
    root = os.path.dirname(src) or "."
    a_pdf, docx = os.path.join(wd, "a.pdf"), os.path.join(wd, "b.docx")
    typ, c_pdf = os.path.join(wd, "c.typ"), os.path.join(wd, "c.pdf")

    # A source that doesn't build on its own (missing fonts, packages, data)
    # isn't a round-trip result either way.
    if run([args.typst, "compile", "--root", root, src, a_pdf])[0] != 0:
        return ("skip", None, None)
    if run([args.typst, "compile", "--root", root, "--format", "docx", src, docx])[0] != 0:
        return ("export", None, None)
    if run([args.importer, docx, typ], 120)[0] != 0 or os.path.getsize(typ) == 0:
        return ("import", None, None)
    if run([args.typst, "compile", "--root", wd, typ, c_pdf])[0] != 0:
        return ("recompile", None, None)

    A, pa = pdf_info(a_pdf)
    C, pc = pdf_info(c_pdf)
    for f in (a_pdf, c_pdf, docx):
        if os.path.exists(f):
            os.remove(f)
    return ("ok", round(100 * len(A & C) / len(A)) if A else None,
            (pa, pc) if pa else None)


def cmd_typst(args):
    cands = []
    for dirpath, _d, files in os.walk(args.dir):
        typs = [f for f in files if f.endswith(".typ")]
        if not typs:
            continue
        pref = [f for f in typs if f in ("main.typ", "paper.typ", "thesis.typ",
                                         "report.typ", "cv.typ", "index.typ")]
        cands.append(os.path.join(dirpath, (pref or sorted(typs))[0]))
    cands = sample(sorted(cands), args.limit)

    stages, texts, pages = {}, [], []
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as ex:
        for stage, text, pg in ex.map(lambda s: typst_leg(args, s), cands):
            stages[stage] = stages.get(stage, 0) + 1
            if text is not None:
                texts.append(text)
            if pg:
                pages.append(pg)

    usable = sum(v for k, v in stages.items() if k != "skip")
    print(f"typst -> docx -> typst, {stages.get('ok', 0)}/{usable} completed the loop "
          f"({stages.get('skip', 0)} sources didn't build on their own)\n")
    for k, v in sorted(stages.items(), key=lambda kv: -kv[1]):
        if k not in ("ok", "skip"):
            print(f"  failed at {k}: {v}")
    if texts:
        texts.sort()
        print(f"  text      : mean {sum(texts)/len(texts):.1f}%  "
              f"median {texts[len(texts)//2]}%  n={len(texts)}")
    if pages:
        same = sum(1 for a, c in pages if a == c)
        print(f"  pages     : identical {same}/{len(pages)}, "
              f"mean ratio {sum(c/a for a, c in pages)/len(pages):.2f}x")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("direction", choices=["docx", "typst"])
    ap.add_argument("--dir", required=True)
    ap.add_argument("--limit", type=int, default=200)
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--out", default=os.path.join(ROOT, "roundtrip-out"))
    ap.add_argument("--typst", default=os.path.join(ROOT, "../../target/release/typst"))
    ap.add_argument("--importer",
                    default=os.path.join(ROOT, "../../target/debug/examples/import"))
    args = ap.parse_args()
    args.typst = os.path.abspath(args.typst)
    args.importer = os.path.abspath(args.importer)
    if os.path.exists(args.out):
        shutil.rmtree(args.out)
    os.makedirs(args.out)
    return cmd_docx(args) if args.direction == "docx" else cmd_typst(args)


if __name__ == "__main__":
    sys.exit(main())

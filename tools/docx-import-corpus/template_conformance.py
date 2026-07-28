# /// script
# requires-python = ">=3.11"
# ///
"""Does an organizationally-mandated .docx template survive a trip through Typst?

Many organizations *require* a specific Word template — journal and conference
submissions, university theses, regulatory filings. Someone who must deliver
such a file, but would rather author in Typst, needs the properties that make
the document *conformant* to survive the loop. Those are not the text.

So this doesn't measure text coverage. It extracts the properties a template
actually imposes, and diffs them across original -> import -> export:

  page       size and orientation
  margins    including the header/footer clearances
  columns    a two-column call-for-papers layout is a hard requirement
  styles     the named hierarchy — `Heading 2` must still be `Heading 2`
  fonts      the document default and per-style typefaces
  numbering  list/heading numbering formats
  sections   section count, which carries per-section geometry

Reported as three columns: what the template says, what the imported Typst
still expresses, and what comes back out the far side.

    python3 template_conformance.py template1.docx template2.docx ...

Templates are not committed — they belong to the organizations that publish
them. Point this at whichever ones matter to you.
"""

import os
import re
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.abspath(__file__))
TYPST = os.path.abspath(os.path.join(ROOT, "../../target/release/typst"))
OUT = os.path.join(ROOT, "conformance-out")


def part(z, name):
    try:
        return z.read(name).decode("utf8", "replace")
    except Exception:
        return ""


def props(docx):
    try:
        z = zipfile.ZipFile(docx)
    except Exception:
        return None
    doc, styles = part(z, "word/document.xml"), part(z, "word/styles.xml")
    numbering = part(z, "word/numbering.xml")

    pg = re.search(r'<w:pgSz[^>]*w:w="(\d+)"[^>]*w:h="(\d+)"', doc)
    mar = re.search(r"<w:pgMar[^>]*/>", doc)
    margins = {}
    if mar:
        for k in ("top", "right", "bottom", "left", "header", "footer"):
            m = re.search(rf'w:{k}="(-?\d+)"', mar.group(0))
            if m:
                margins[k] = int(m.group(1))
    cols = re.search(r'<w:cols[^>]*w:num="(\d+)"', doc)

    # Style names actually *used* by the body, which is what conformance
    # checkers look at — a style defined but never applied proves nothing.
    used_ids = set(re.findall(r'<w:pStyle w:val="([^"]+)"', doc))
    id_to_name = {}
    for m in re.finditer(r'<w:style\b[^>]*w:styleId="([^"]+)"[^>]*>(.*?)</w:style>',
                         styles, re.S):
        n = re.search(r'<w:name w:val="([^"]+)"', m.group(2))
        id_to_name[m.group(1)] = (n.group(1) if n else m.group(1)).lower()
    used_names = {id_to_name.get(s, s.lower()) for s in used_ids}

    default_font = re.search(r'<w:rFonts[^>]*w:ascii="([^"]+)"', styles)
    fonts = set(re.findall(r'w:ascii="([^"]+)"', styles)) | \
        set(re.findall(r'w:ascii="([^"]+)"', doc))

    return {
        "page": (int(pg.group(1)), int(pg.group(2))) if pg else None,
        "margins": margins,
        "columns": int(cols.group(1)) if cols else 1,
        "styles_used": used_names,
        "headings_used": {s for s in used_names if s.startswith("heading")},
        "default_font": default_font.group(1) if default_font else None,
        "fonts": fonts,
        "numbering_fmts": set(re.findall(r'<w:numFmt w:val="([^"]+)"', numbering)),
        "sections": len(re.findall(r"<w:sectPr[ >]", doc)),
    }


def typst_expresses(typ_path):
    """What the emitted Typst source still states, read from the source."""
    try:
        src = open(typ_path, encoding="utf8", errors="replace").read()
    except Exception:
        return {}
    w = re.search(r"width:\s*([\d.]+)pt", src)
    h = re.search(r"height:\s*([\d.]+)pt", src)
    mar = re.search(r"margin:\s*\(([^)]*)\)", src)
    margins = {}
    if mar:
        for k in ("top", "right", "bottom", "left"):
            m = re.search(rf"{k}:\s*([\d.]+)pt", mar.group(1))
            if m:
                margins[k] = round(float(m.group(1)) * 20)  # pt -> twips
    return {
        "page_pt": (float(w.group(1)), float(h.group(1))) if w and h else None,
        "margins_tw": margins,
        "headings": len(re.findall(r"^=+ ", src, re.M)),
        "columns": int(m.group(1)) if (m := re.search(r"columns:\s*(\d+)", src)) else 1,
        "font_set": bool(re.search(r"#set text\([^)]*font:", src)),
    }


def fmt_set(s, limit=6):
    if not s:
        return "-"
    items = sorted(s)
    shown = ", ".join(items[:limit])
    return shown + (f" (+{len(items)-limit})" if len(items) > limit else "")


def main():
    os.makedirs(OUT, exist_ok=True)
    for src in sorted(sys.argv[1:]):
        name = os.path.basename(src)
        wd = os.path.join(OUT, name[:-5])
        os.makedirs(wd, exist_ok=True)
        typ, back = os.path.join(wd, "t.typ"), os.path.join(wd, "back.docx")

        print(f"\n{'='*78}\n{name}\n{'='*78}")
        a = props(src)
        if not a:
            print("  unreadable"); continue

        r = subprocess.run([TYPST, "import", src, typ], capture_output=True, timeout=180)
        if r.returncode != 0:
            print("  IMPORT FAILED:", (r.stdout + r.stderr).decode()[-120:]); continue
        notes = [l for l in (r.stdout + r.stderr).decode("utf8", "replace").splitlines()
                 if l.startswith("- [")]
        mid = typst_expresses(typ)

        e = subprocess.run([TYPST, "compile", "--root", wd, "--format", "docx", typ, back],
                           capture_output=True, timeout=240)
        if e.returncode != 0:
            print("  EXPORT FAILED:", (e.stdout + e.stderr).decode()[-120:]); continue
        b = props(back)

        def row(label, x, y):
            ok = "OK " if x == y else "DIFF"
            print(f"  {ok} {label:<14} {str(x)[:30]:<32} -> {str(y)[:30]}")

        row("page (twips)", a["page"], b["page"])
        row("margins", {k: a["margins"].get(k) for k in ("top", "left")},
            {k: b["margins"].get(k) for k in ("top", "left")})
        row("columns", a["columns"], b["columns"])
        row("sections", a["sections"], b["sections"])
        # Case-insensitively: Typst lowercases font names, and reporting
        # "Times New Roman -> times new roman" as a difference is the same
        # kind of self-inflicted false positive as counting headings by
        # Word's English style ids.
        row("default font", (a["default_font"] or "").lower(),
            (b["default_font"] or "").lower())
        print(f"       headings used  {fmt_set(a['headings_used'])}")
        print(f"                   -> {fmt_set(b['headings_used'])}")
        print(f"       styles used    {len(a['styles_used'])} -> {len(b['styles_used'])}")
        print(f"       numbering fmts {fmt_set(a['numbering_fmts'])}")
        print(f"                   -> {fmt_set(b['numbering_fmts'])}")
        print(f"       typst states   page={mid.get('page_pt')} "
              f"headings={mid.get('headings')} cols={mid.get('columns')}")
        if notes:
            print(f"       import notes   {len(notes)}")
            for n in notes[:4]:
                print(f"         {n[:88]}")
        if os.path.exists(back):
            os.remove(back)


if __name__ == "__main__":
    main()

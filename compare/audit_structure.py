# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Structural/editability audit of pptx+docx outputs.

For DOCX: does the output behave like a real Word document (flowing paragraphs,
heading styles, native math, live fields) or a page-frozen facsimile
(absolutely-positioned frames / per-line paragraphs / images)?

For PPTX: live text runs vs pictures.

Usage: uv run audit_structure.py <dir> [<dir>...]
"""
import re, sys, zipfile
from pathlib import Path


def count(xml, pat):
    return len(re.findall(pat, xml))


def audit_docx(path):
    z = zipfile.ZipFile(path)
    doc = z.read("word/document.xml").decode("utf8", "replace")
    names = z.namelist()
    styles = z.read("word/styles.xml").decode("utf8", "replace") if "word/styles.xml" in names else ""
    words = len(re.findall(r"[A-Za-zͰ-Ͽ]{2,}", " ".join(re.findall(r"<w:t[^>]*>([^<]*)</w:t>", doc))))
    return {
        "paragraphs": count(doc, r"<w:p[ >]"),
        "words": words,
        "heading_style_refs": count(doc, r'w:val="Heading\d'),
        "framePr(abs-pos)": count(doc, r"<w:framePr"),
        "textboxes": count(doc, r"<w:txbxContent|<wps:txbx"),
        "images": count(doc, r"<pic:pic|<w:drawing"),
        "omml_math": count(doc, r"<m:oMath[ >]"),
        "fields(REF/TOC/SEQ)": count(doc, r'w:instr|<w:instrText'),
        "hyperlinks": count(doc, r"<w:hyperlink"),
        "sectPr": count(doc, r"<w:sectPr"),
        "hdr_ftr_parts": sum(1 for n in names if re.match(r"word/(header|footer)\d+\.xml", n)),
        "footnotes": count(z.read("word/footnotes.xml").decode("utf8","replace"), r'<w:footnote w:id="[2-9]') if "word/footnotes.xml" in names else 0,
        "numbering_part": int("word/numbering.xml" in names),
        "styles_count": count(styles, r"<w:style "),
    }


def audit_pptx(path):
    z = zipfile.ZipFile(path)
    slides = [n for n in z.namelist() if re.match(r"ppt/slides/slide\d+\.xml", n)]
    xml = " ".join(z.read(s).decode("utf8", "replace") for s in slides)
    words = len(re.findall(r"[A-Za-zͰ-Ͽ]{2,}", " ".join(re.findall(r"<a:t>([^<]*)</a:t>", xml))))
    return {
        "slides": len(slides),
        "live_text_runs": count(xml, r"<a:t>"),
        "words": words,
        "pictures": count(xml, r"<p:pic[ >]"),
        "native_shapes": count(xml, r"<a:custGeom|<a:prstGeom"),
        "gradients": count(xml, r"<a:gradFill"),
        "alpha": count(xml, r"<a:alpha "),
        "hyperlinks": count(xml, r"<a:hlinkClick"),
        "groups": count(xml, r"<p:grpSp>"),
    }


def main():
    for d in sys.argv[1:]:
        d = Path(d).resolve()
        print(f"\n## {d.name}")
        for f in sorted(list(d.glob("*.docx")) + list(d.glob("*.pptx"))):
            try:
                info = audit_docx(f) if f.suffix == ".docx" else audit_pptx(f)
            except Exception as e:
                print(f"{f.name}: ERROR {e}")
                continue
            kv = " ".join(f"{k}={v}" for k, v in info.items())
            print(f"{f.name}: {kv}")


if __name__ == "__main__":
    main()

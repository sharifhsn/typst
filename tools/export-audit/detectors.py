"""Pure detectors over extracted package data, each with planted-defect canaries.

Every detector is a pure function: package parts (or already-extracted text)
in, findings out. No filesystem, no subprocess. That split exists so the
detectors can be proven against planted defects without compiling anything —
`audit.py selftest` runs every canary below and refuses to report green if a
planted defect goes undetected or a clean twin fires.

The rule that motivates the canaries: a checker that can only report success
is not a gate. Eight scanner bugs in the 2026-07 sessions each produced
plausible passing output; every failure mode named there is a canary here.
"""

from __future__ import annotations

import re
from typing import Any, Callable

from corpuslib import cjk_pair_counts, word_counts

# ---------------------------------------------------------------------------
# DOCX invariants
# ---------------------------------------------------------------------------

_ANCHOR = re.compile(rb'<w:hyperlink(?![^>]*r:id=)[^>]*w:anchor="([^"]+)"')
_BOOKMARK = re.compile(rb'<w:bookmarkStart[^>]*w:name="([^"]+)"')
_INSTR = re.compile(rb"<w:instrText[^>]*>([^<]*)</w:instrText>")
_FLDSIMPLE = re.compile(rb'<w:fldSimple[^>]*w:instr="([^"]*)"')
_FIELD_REF = re.compile(rb"\s*(?:REF|PAGEREF|NOTEREF)\s+([^\s\\&]+)")
_RESERVED = (b"_GoBack", b"_Toc")


def dead_anchors(parts: dict[str, bytes]) -> list[str]:
    """Internal link targets with no bookmark anywhere in word/*.xml.

    Mirrors tests/src/docx.rs `assert_no_dangling_anchors`, at corpus scale:
    a `w:anchor` without `r:id`, or a REF/PAGEREF/NOTEREF field argument,
    must name a `w:bookmarkStart` in the same package.
    """
    body = b"".join(
        d for n, d in parts.items() if n.startswith("word/") and n.endswith(".xml")
    )
    names = set(_BOOKMARK.findall(body))
    refs: set[bytes] = set(_ANCHOR.findall(body))
    for instr in _INSTR.findall(body) + _FLDSIMPLE.findall(body):
        m = _FIELD_REF.match(instr)
        if m:
            refs.add(m.group(1))
    return sorted(
        r.decode("utf-8", "replace")
        for r in refs
        if r not in names and not any(r.startswith(p) for p in _RESERVED)
    )


_REL = re.compile(rb'<Relationship\b[^>]*/?>')
_ATTR = {
    "id": re.compile(rb'Id="([^"]*)"'),
    "target": re.compile(rb'Target="([^"]*)"'),
    "mode": re.compile(rb'TargetMode="([^"]*)"'),
}


def dangling_rels(parts: dict[str, bytes]) -> list[str]:
    """Non-external relationship targets that resolve to no part in the zip.

    The `header4.xml` shape: a section bails, its already-allocated
    relationships stay behind, and Word's repair dialog greets the user.
    """
    from corpuslib import resolve_part_path

    bad = []
    for rels_name, data in parts.items():
        if not rels_name.endswith(".rels"):
            continue
        # word/_rels/document.xml.rels -> word/document.xml; _rels/.rels -> ""
        head, _, tail = rels_name.rpartition("_rels/")
        source = head.rstrip("/") + "/" + tail[: -len(".rels")] if head else ""
        source = source.strip("/") if tail != ".rels" else ""
        for rel in _REL.findall(data):
            mode = _ATTR["mode"].search(rel)
            if mode and b"External" in mode.group(1):
                continue
            t = _ATTR["target"].search(rel)
            if not t:
                continue
            target = t.group(1).decode("utf-8", "replace")
            resolved = resolve_part_path(source, target)
            if resolved not in parts:
                bad.append(f"{rels_name} -> {target}")
    return sorted(bad)


_NUM_REF = re.compile(rb'<w:numId w:val="([^"]+)"')
_NUM_DEF = re.compile(rb'<w:num w:numId="([^"]+)"')


def unresolved_numids(parts: dict[str, bytes]) -> list[str]:
    """numId references with no definition. `val="0"` is OOXML's reserved
    "no numbering" sentinel and is legitimately undefined — a scanner that
    does not know the sentinel reports 43 false defects per 400 documents."""
    defs = set(_NUM_DEF.findall(parts.get("word/numbering.xml", b"")))
    refs: set[bytes] = set()
    for n, d in parts.items():
        if n.startswith("word/") and n.endswith(".xml") and "numbering" not in n:
            refs.update(_NUM_REF.findall(d))
    return sorted(
        r.decode() for r in refs - defs if r != b"0"
    )


# ---------------------------------------------------------------------------
# PPTX invariants
# ---------------------------------------------------------------------------

_W_ELEMENT = re.compile(rb"<w:[A-Za-z]")


def wordprocessing_in_slides(parts: dict[str, bytes]) -> list[str]:
    """Wordprocessing markup inside a slide part. DrawingML run properties
    are `a:rPr`; `w:` anything in a slide is schema-alien and its prefix is
    not even declared there."""
    return sorted(
        n
        for n, d in parts.items()
        if n.startswith("ppt/slides/") and n.endswith(".xml") and _W_ELEMENT.search(d)
    )


_RID_REF = re.compile(rb'r:(?:id|embed|link)="([^"]+)"')
_RID_DEF = re.compile(rb'Id="([^"]+)"')


def unresolved_rids(parts: dict[str, bytes]) -> list[str]:
    """r:id/r:embed/r:link referenced by a part but absent from its own rels."""
    bad = []
    for n, d in parts.items():
        if not n.endswith(".xml") or n.endswith(".rels"):
            continue
        head, _, base = n.rpartition("/")
        rels = f"{head}/_rels/{base}.rels" if head else f"_rels/{base}.rels"
        defined = set(_RID_DEF.findall(parts.get(rels, b"")))
        for rid in set(_RID_REF.findall(d)) - defined:
            bad.append(f"{n} -> {rid.decode('utf-8', 'replace')}")
    return sorted(bad)


# ---------------------------------------------------------------------------
# Shared invariants
# ---------------------------------------------------------------------------


def nondeterminism(parts_a: dict[str, bytes], parts_b: dict[str, bytes]) -> list[str]:
    """Part names that differ between two exports of the same input."""
    names = sorted(set(parts_a) | set(parts_b))
    return [n for n in names if parts_a.get(n) != parts_b.get(n)]


# ---------------------------------------------------------------------------
# Text fidelity (pure over extracted strings)
# ---------------------------------------------------------------------------


def _fragment_of(word: str, by_length: dict[int, list[str]]) -> bool:
    """A pdftotext edge-slice of a real docx word (drop cap, letter spacing):
    the PDF token is a prefix/suffix of a docx word at most two chars longer.
    The bound forgives a lost initial without forgiving `boost` inside
    `boosthandlingclimbstallspeed`."""
    for extra in (1, 2):
        for candidate in by_length.get(len(word) + extra, ()):
            if candidate.endswith(word) or candidate.startswith(word):
                return True
    return False


def text_loss(pdf_text: str, body_text: str, furniture_text: str) -> dict:
    """What the PDF says that the .docx does not.

    Three signals, because each alone has a proven blind spot:
      missing        — word-set difference (misses anything repeated),
      deficit        — occurrence shortfall (catches a TOC title hiding
                       behind the heading that still has it); furniture is
                       exempt since a running head repeats per page in the
                       PDF and is stored once in the .docx, correctly,
      cjk_pair_loss  — adjacent-CJK-pair shortfall (a checker that skips CJK
                       lets a Chinese document lose a chapter and score clean).
    """
    everything = body_text + " " + furniture_text
    pdf_words = word_counts(pdf_text)
    doc_words = word_counts(everything)
    by_length: dict[int, list[str]] = {}
    for w in doc_words:
        by_length.setdefault(len(w), []).append(w)

    missing = sorted(
        w
        for w in set(pdf_words) - set(doc_words)
        if not _fragment_of(w, by_length)
    )
    body_words = word_counts(body_text)
    furn_words = word_counts(furniture_text)
    deficit = {
        w: pdf_words[w] - body_words[w]
        for w in pdf_words
        if w in body_words
        and body_words[w] < pdf_words[w]
        and furn_words.get(w, 0) == 0
    }
    pdf_pairs = cjk_pair_counts(pdf_text)
    doc_pairs = cjk_pair_counts(everything)
    cjk_loss = sorted(set(pdf_pairs) - set(doc_pairs))
    return {"missing": missing, "deficit": deficit, "cjk_pair_loss": cjk_loss}


# ---------------------------------------------------------------------------
# Canaries: (name, detector, args, should_fire)
# ---------------------------------------------------------------------------

_DOC_OK = {
    "word/document.xml": (
        b'<w:hyperlink w:anchor="_Typstaa"><w:t>x</w:t></w:hyperlink>'
        b'<w:bookmarkStart w:name="_Typstaa"/>'
        b'<w:fldSimple w:instr=" PAGEREF _Typstaa "/>'
    ),
}
_DOC_DEAD = {
    "word/document.xml": b'<w:hyperlink w:anchor="_Typstbb"><w:t>x</w:t></w:hyperlink>',
}
_DOC_EXTERNAL_FRAGMENT = {
    # An anchor next to r:id names a fragment in the TARGET document.
    "word/document.xml": b'<w:hyperlink r:id="rId9" w:anchor="section2"/>',
}
_RELS_OK = {
    "word/document.xml": b"<w:document/>",
    "word/header1.xml": b"<w:hdr/>",
    "word/_rels/document.xml.rels": (
        b'<Relationships><Relationship Id="rId1" Target="header1.xml"/>'
        b'<Relationship Id="rId2" Target="https://x" TargetMode="External"/>'
        b"</Relationships>"
    ),
}
_RELS_DEAD = {
    "word/document.xml": b"<w:document/>",
    "word/_rels/document.xml.rels": (
        b'<Relationships><Relationship Id="rId1" Target="header4.xml"/></Relationships>'
    ),
}
_NUM_OK = {
    "word/document.xml": b'<w:numId w:val="1"/><w:numId w:val="0"/>',
    "word/numbering.xml": b'<w:num w:numId="1"/>',
}
_NUM_DEAD = {
    "word/document.xml": b'<w:numId w:val="7"/>',
    "word/numbering.xml": b'<w:num w:numId="1"/>',
}
_SLIDE_OK = {"ppt/slides/slide1.xml": b"<a:rPr><a:solidFill/></a:rPr>"}
_SLIDE_W = {"ppt/slides/slide1.xml": b"<m:r><w:rPr><w:color w:val=\"FF0000\"/></w:rPr></m:r>"}
_RID_OK = {
    "ppt/slides/slide1.xml": b'<a:blip r:embed="rId2"/>',
    "ppt/slides/_rels/slide1.xml.rels": b'<Relationship Id="rId2" Target="../media/i.png"/>',
}
_RID_DEAD = {"ppt/slides/slide1.xml": b'<a:blip r:embed="rId2"/>'}

# The table is deliberately heterogeneous — each entry pairs a detector with
# its own argument shape — so it is typed as fully dynamic.
CANARIES: list[tuple[str, Callable[..., Any], tuple[Any, ...], bool]] = [
    ("dead_anchors fires on planted dead link", dead_anchors, (_DOC_DEAD,), True),
    ("dead_anchors clean on resolved link+field", dead_anchors, (_DOC_OK,), False),
    ("dead_anchors ignores external fragment", dead_anchors, (_DOC_EXTERNAL_FRAGMENT,), False),
    ("dangling_rels fires on missing part", dangling_rels, (_RELS_DEAD,), True),
    ("dangling_rels clean incl. external", dangling_rels, (_RELS_OK,), False),
    ("numids fires on undefined id", unresolved_numids, (_NUM_DEAD,), True),
    ("numids forgives the val=0 sentinel", unresolved_numids, (_NUM_OK,), False),
    ("w-in-slide fires on w:rPr", wordprocessing_in_slides, (_SLIDE_W,), True),
    ("w-in-slide clean on a:rPr", wordprocessing_in_slides, (_SLIDE_OK,), False),
    ("rids fires on missing rels entry", unresolved_rids, (_RID_DEAD,), True),
    ("rids clean when defined", unresolved_rids, (_RID_OK,), False),
    (
        "nondeterminism names the moving part",
        nondeterminism,
        ({"a.xml": b"1", "b.xml": b"x"}, {"a.xml": b"2", "b.xml": b"x"}),
        True,
    ),
    (
        "nondeterminism clean on identical",
        nondeterminism,
        ({"a.xml": b"1"}, {"a.xml": b"1"}),
        False,
    ),
    # Text canaries replay the session's real failure shapes.
    (
        "text: concatenated cells are missing words",
        text_loss,
        ("Boost Handling Climb", "boosthandlingclimb", ""),
        True,
    ),
    (
        "text: smallcaps split runs are NOT loss",
        text_loss,
        ("JOBBER inspector", "Jobber inspector", ""),
        False,
    ),
    (
        "text: a TOC title hiding behind its heading is a deficit",
        text_loss,
        ("Introduction body Introduction toc", "Introduction body", ""),
        True,
    ),
    (
        "text: a running head repeated per page is NOT a deficit",
        text_loss,
        ("Runhead Runhead Runhead body", "body", "Runhead"),
        False,
    ),
    (
        "text: CJK chapter loss is caught despite word-skip",
        text_loss,
        ("绪论与方法", "", ""),
        True,
    ),
    (
        "text: drop-cap fragment is forgiven",
        text_loss,
        ("hapter", "chapter", ""),
        False,
    ),
]


def text_fired(result: dict) -> bool:
    return bool(result["missing"] or result["deficit"] or result["cjk_pair_loss"])


def run_canaries() -> list[str]:
    """Every canary, returning failure descriptions (empty = all proven)."""
    failures = []
    for name, fn, args, should_fire in CANARIES:
        result = fn(*args)
        fired = text_fired(result) if fn is text_loss else bool(result)
        if fired != should_fire:
            failures.append(
                f"{name}: expected {'FIRE' if should_fire else 'clean'},"
                f" got {result!r}"
            )
    return failures

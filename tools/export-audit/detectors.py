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

import collections
import difflib
import html
import re
from typing import Any, Callable

from corpuslib import cjk_pair_counts, norm, word_counts

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
# Uniqueness of element ids (both formats)
# ---------------------------------------------------------------------------

_DOCPR = re.compile(rb'<wp:docPr\b[^>]*\bid="([^"]+)"')
_BOOKMARK_NAME = re.compile(rb'<w:bookmarkStart\b[^>]*\bw:name="([^"]+)"')
_CNVPR = re.compile(rb'<p:cNvPr\b[^>]*\bid="([^"]+)"')
_SLDID = re.compile(rb'<p:sldId\b[^>]*\bid="([^"]+)"')


def _dups_within(values: list[bytes]) -> list[bytes]:
    counts = collections.Counter(values)
    return sorted(v for v, c in counts.items() if c > 1)


def duplicate_ids(parts: dict[str, bytes]) -> list[str]:
    """Element ids whose duplication makes a consumer reject or repair the file.

    Four uniqueness rules, each scoped exactly where the format demands it —
    a scanner that widens the scope forges defects (docPr ids legitimately
    repeat ACROSS parts; cNvPr ids legitimately repeat across slides):

      docPr id     — unique WITHIN one wordprocessing part; a repeat inside a
                     single header/footer/document part is Word's repair-dialog
                     trigger (a picture placed twice keeping one id),
      bookmark name— unique ACROSS all wordprocessing parts; a second
                     `w:bookmarkStart w:name=` makes every cross-reference to
                     that name ambiguous,
      cNvPr id     — unique WITHIN one slide,
      sldId id     — unique within presentation.xml's slide-id list.
    """
    bad = []
    seen_bookmarks: collections.Counter = collections.Counter()
    for n, d in parts.items():
        if n.startswith("word/") and n.endswith(".xml"):
            for v in _dups_within(_DOCPR.findall(d)):
                bad.append(f"{n}: docPr id {v.decode()}")
            for name in _BOOKMARK_NAME.findall(d):
                if not any(name.startswith(p) for p in _RESERVED):
                    seen_bookmarks[name] += 1
        elif n.startswith("ppt/slides/") and n.endswith(".xml"):
            for v in _dups_within(_CNVPR.findall(d)):
                bad.append(f"{n}: cNvPr id {v.decode()}")
    for name, c in seen_bookmarks.items():
        if c > 1:
            bad.append(f"bookmark name {name.decode()} x{c}")
    for v in _dups_within(_SLDID.findall(parts.get("ppt/presentation.xml", b""))):
        bad.append(f"ppt/presentation.xml: sldId {v.decode()}")
    return sorted(bad)


# ---------------------------------------------------------------------------
# External-link survival (advisory; a show rule may legitimately drop a link)
# ---------------------------------------------------------------------------


def external_link_survival(dests: list[str], parts: dict[str, bytes]) -> list[str]:
    """Source http(s) link destinations that reach neither an external
    relationship Target nor a HYPERLINK field in the package. Advisory: a
    show rule can legitimately transform or remove a link, so this reports
    a count to look at, never a hard failure."""
    haystack = []
    for n, d in parts.items():
        if n.endswith(".rels"):
            for rel in _REL.findall(d):
                mode = _ATTR["mode"].search(rel)
                if mode and b"External" in mode.group(1):
                    t = _ATTR["target"].search(rel)
                    if t:
                        haystack.append(t.group(1).decode("utf-8", "replace"))
        elif n.endswith(".xml"):
            for instr in _INSTR.findall(d) + _FLDSIMPLE.findall(d):
                if b"HYPERLINK" in instr:
                    haystack.append(instr.decode("utf-8", "replace"))
    # A rel Target with an `&` is stored XML-escaped (`&amp;`); the source dest
    # is not. Unescape the haystack so a query-string URL is not reported lost
    # purely because of entity encoding.
    hay = html.unescape("\n".join(haystack))
    return sorted(u for u in dict.fromkeys(dests) if u not in hay)


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


def text_loss_from_parts(parts: dict[str, bytes], pdf_text: str) -> dict:
    """`text_loss`, but extracting the docx text the way a real run does.

    A canary here exercises the whole extraction path (`docx_text`'s
    `w:t`/`m:t`/`a:t` reading), not just the comparison, so the class of miss
    that inflated a full-corpus sweep — a word present only inside exported
    maths reading as lost — cannot silently return.
    """
    from corpuslib import docx_text

    body, furniture = docx_text(parts)
    return text_loss(pdf_text, body, furniture)


# Reading-order signal. The bags above (word sets, occurrence counts) are
# order-blind: a perfectly scrambled paragraph passes every one of them. This
# compares the two word STREAMS in order.
_SEQ_TOK = re.compile(r"[^\W\d_]{2,}", re.UNICODE)
# Empirical floor over 30 clean corpus documents was 0.539, and every document
# under 0.80 was a multi-column/CV layout whose geometric read order in the PDF
# legitimately differs from the linearized docx flow (altacv, ratio 0.763, has
# zero text loss). The flag sits UNDER that floor so it only fires on a stream
# more scrambled than any clean document — including heavy multi-column ones.
SEQ_FLAG = 0.50


def _seq_words(text: str, cap: int = 4000) -> list[str]:
    return _SEQ_TOK.findall(norm(text))[:cap]


def sequence_similarity(pdf_text: str, body_text: str, cap: int = 4000) -> float:
    """difflib ratio of the PDF word stream against the docx body word stream.

    1.0 is identical order; a fully reversed stream tends toward 0; a benign
    local transposition stays near 1. Lists are capped at `cap` words so the
    O(n^2) matcher stays cheap on book-length documents. Empty on either side
    means there is nothing to order, which is not a defect -> 1.0."""
    a = _seq_words(pdf_text, cap)
    b = _seq_words(body_text, cap)
    if not a or not b:
        return 1.0
    return round(difflib.SequenceMatcher(None, a, b, autojunk=False).ratio(), 4)


def formatting_collapse(height_classes: int, sz_values: int) -> bool:
    """A coarse formatting-survival proxy over two already-extracted counts:
    the number of distinct rounded word-height classes in the PDF and the
    number of distinct `w:sz` values in the docx body. Three or more visual
    text sizes flattened to a single size in the package is a collapse. Kept
    conservative (>=3 vs <=1) so a document that merely lacks run-level sizing
    for a legitimate reason does not trip it."""
    return height_classes >= 3 and sz_values <= 1


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

# Duplicate ids. The dup must be scoped exactly: WITHIN a part for docPr/cNvPr,
# ACROSS parts for bookmark names. The clean twins are the false-positive
# shapes — the same id legitimately reused in a different part / slide.
_DOCPR_DUP = {"word/footer1.xml": b'<wp:docPr id="7"/><wp:docPr id="7"/>'}
_DOCPR_CROSSPART_OK = {
    "word/document.xml": b'<wp:docPr id="7"/>',
    "word/header1.xml": b'<wp:docPr id="7"/>',
}
_BOOKMARK_DUP = {
    "word/document.xml": b'<w:bookmarkStart w:name="ref"/>',
    "word/footnotes.xml": b'<w:bookmarkStart w:name="ref"/>',
}
_BOOKMARK_OK = {
    "word/document.xml": b'<w:bookmarkStart w:name="a"/><w:bookmarkStart w:name="_GoBack"/>',
    "word/footnotes.xml": b'<w:bookmarkStart w:name="b"/><w:bookmarkStart w:name="_GoBack"/>',
}
_CNVPR_DUP = {"ppt/slides/slide1.xml": b'<p:cNvPr id="3"/><p:cNvPr id="3"/>'}
_CNVPR_CROSSSLIDE_OK = {
    "ppt/slides/slide1.xml": b'<p:cNvPr id="3"/>',
    "ppt/slides/slide2.xml": b'<p:cNvPr id="3"/>',
}
_SLDID_DUP = {"ppt/presentation.xml": b'<p:sldId id="256"/><p:sldId id="256"/>'}

_LINK_REL_OK = {
    "word/document.xml": b"<w:hyperlink r:id=\"rId5\"/>",
    "word/_rels/document.xml.rels": (
        b'<Relationship Id="rId5" Target="https://x.com/a" TargetMode="External"/>'
    ),
}
_LINK_FIELD_OK = {
    "word/document.xml": b'<w:instrText> HYPERLINK "https://y.com/b" </w:instrText>',
}
# A query-string dest whose rel Target is XML-escaped must still count as
# survived (the &amp; false positive found on ilm in the 2026-07 shakeout).
_LINK_ESCAPED_OK = {
    "word/_rels/document.xml.rels": (
        b'<Relationship Id="rId3" Target="https://w.com/p?a=1&amp;b=2"'
        b' TargetMode="External"/>'
    ),
}
_LINK_DEAD = {"word/document.xml": b"<w:p>no link here</w:p>"}

# Reading-order fixtures.
_SEQ_A = "alpha bravo charlie delta echo foxtrot golf hotel india juliet"
_SEQ_SWAP = "alpha bravo charlie delta foxtrot echo golf hotel india juliet"
_SEQ_REV = " ".join(reversed(_SEQ_A.split()))

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
    (
        "text: a word only in exported math is not 'missing'",
        text_loss_from_parts,
        ({"word/document.xml": b"<m:oMath><m:r><m:t>integer</m:t></m:r></m:oMath>"},
         "integer"),
        False,
    ),
    (
        "text: a word only in drawing text is not 'missing'",
        text_loss_from_parts,
        ({"word/document.xml": b"<a:t>diagram</a:t>"}, "diagram"),
        False,
    ),
    # Duplicate ids: fire on an in-scope dup, clean on the reused-elsewhere twin.
    ("dupids fires on docPr repeat in one part", duplicate_ids, (_DOCPR_DUP,), True),
    ("dupids clean on docPr reused across parts", duplicate_ids, (_DOCPR_CROSSPART_OK,), False),
    ("dupids fires on bookmark name across parts", duplicate_ids, (_BOOKMARK_DUP,), True),
    ("dupids clean on unique names + shared _GoBack", duplicate_ids, (_BOOKMARK_OK,), False),
    ("dupids fires on cNvPr repeat in one slide", duplicate_ids, (_CNVPR_DUP,), True),
    ("dupids clean on cNvPr reused across slides", duplicate_ids, (_CNVPR_CROSSSLIDE_OK,), False),
    ("dupids fires on sldId repeat", duplicate_ids, (_SLDID_DUP,), True),
    # External-link survival: clean when the dest reaches a rel or a field.
    ("links clean when dest in external rel", external_link_survival, (["https://x.com/a"], _LINK_REL_OK), False),
    ("links clean when dest in HYPERLINK field", external_link_survival, (["https://y.com/b"], _LINK_FIELD_OK), False),
    ("links clean when rel Target is XML-escaped", external_link_survival, (["https://w.com/p?a=1&b=2"], _LINK_ESCAPED_OK), False),
    ("links fires on a stranded dest", external_link_survival, (["https://z.com/gone"], _LINK_DEAD), True),
    # Reading order: reversed fires, identical/local-swap clean.
    ("sequence fires on reversed stream", sequence_similarity, (_SEQ_A, _SEQ_REV), True),
    ("sequence clean on identical stream", sequence_similarity, (_SEQ_A, _SEQ_A), False),
    ("sequence clean on a benign adjacent swap", sequence_similarity, (_SEQ_A, _SEQ_SWAP), False),
    # Formatting survival over two extracted counts.
    ("formatting fires when 3+ heights collapse to one sz", formatting_collapse, (3, 1), True),
    ("formatting fires when many heights and no sz", formatting_collapse, (5, 0), True),
    ("formatting clean when two sizes survive", formatting_collapse, (3, 2), False),
    ("formatting clean when few heights to begin with", formatting_collapse, (2, 1), False),
]


def text_fired(result: dict) -> bool:
    return bool(result["missing"] or result["deficit"] or result["cjk_pair_loss"])


def _fired(fn: Callable[..., Any], result: Any) -> bool:
    """Did this detector fire? Each result shape has its own truthiness: the
    text bag is fired iff any of its three lists is nonempty; a similarity
    ratio is fired iff it falls under the flag; everything else is a list or a
    bool whose ordinary truthiness is the answer."""
    if fn is text_loss or fn is text_loss_from_parts:
        return text_fired(result)
    if fn is sequence_similarity:
        return result < SEQ_FLAG
    return bool(result)


def run_canaries() -> list[str]:
    """Every canary, returning failure descriptions (empty = all proven)."""
    failures = []
    for name, fn, args, should_fire in CANARIES:
        result = fn(*args)
        if _fired(fn, result) != should_fire:
            failures.append(
                f"{name}: expected {'FIRE' if should_fire else 'clean'},"
                f" got {result!r}"
            )
    return failures

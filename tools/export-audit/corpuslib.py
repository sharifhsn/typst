"""Corpus mechanics for the export audit, written once.

Every scanner that found a real exporter bug in the 2026-07 audit sessions
also *contained* a bug at some point — and every one of those bugs was in
re-derived plumbing (roots, failure classification, run joining), never in
the comparison logic. This module owns the plumbing so it can only be wrong
in one place, and records the specific lessons as behavior:

  - A corpus document needs its OWN --root (the repo dir for repos/, the
    template dir for registry/). One corpus-wide root breaks absolute
    imports and misreported 36 documents as export failures.
  - An export failed iff the OUTPUT FILE does not exist. stderr is full of
    legitimate warnings (unknown fonts, page-varying furniture) and judging
    by it misclassified 29 successful exports.
  - Failures are reported BY NAME. At corpus scale, a changed failure set is
    the regression signal; a bare count hides it.
  - Word runs join with NO separator inside a paragraph; only structural
    boundaries separate. Joining `<w:t>` values with spaces invented a
    missing word for every smallcaps/drop-cap in the corpus.
  - Debug binaries running in parallel took the host down (13 GB resident).
    Exports here are serial, and a debug binary path warns loudly.
"""

from __future__ import annotations

import collections
import html
import re
import subprocess
import sys
import unicodedata
import zipfile
from pathlib import Path

CORPUS = Path.home() / "Code/typst-corpus"
MANIFEST = CORPUS / "_meta/manifest.tsv"

# Text runs interleaved with the structural boundaries that are real word
# separators. Everything else between two runs joins with no gap.
#
# Three text elements are read, not one: `w:t` (prose), `m:t` (OMML math), and
# `a:t` (DrawingML text in a shape/text box). Reading only `w:t` made every word
# that appears solely inside an exported equation — `integer`, `where`, half a
# maths paper's vocabulary — look lost; group 1 is the tag, group 2 the text,
# tied by a backreference so an open tag matches only its own close.
FLOW = re.compile(
    rb"<(w:t|m:t|a:t)[^>]*>([^<]*)</\1>"
    rb"|<w:(?:br|cr|tab)\b[^>]*/?>"
    rb"|</(?:w:(?:p|tc|tr|tbl|sdt|hyperlink|drawing)|a:p)>"
    rb"|</m:oMath>"
)
ALT = re.compile(rb'descr="([^"]*)"')
WORD = re.compile(r"[^\W\d_]{5,}", re.UNICODE)
CJK_CLASS = "぀-ヿ㐀-䶿一-鿿豈-﫿가-힯"
CJK = re.compile(f"[{CJK_CLASS}]")
CJK_RUN = re.compile(f"[{CJK_CLASS}]+")
HDRFTR = re.compile(r"word/(header|footer)\d*\.xml$")
SLIDE = re.compile(r"ppt/slides/slide(\d+)\.xml$")


def docs(limit: int, kind: str = "document", filt: str = "") -> list[Path]:
    """Compilable corpus entries. kind: 'document', 'presentation', 'all'."""
    out = []
    for line in MANIFEST.read_text().splitlines():
        f = line.split("\t")
        if len(f) <= 6 or f[0] != "yes":
            continue
        is_deck = f[3] == "presentation"
        if kind == "document" and is_deck:
            continue
        if kind == "presentation" and not is_deck:
            continue
        if filt and filt not in f[6]:
            continue
        out.append(Path(f[6]))
        if len(out) >= limit:
            break
    return out


def root_for(src: Path) -> Path:
    """The document's own root: repo dir for repos/, template dir otherwise."""
    if src.parts[0] == "repos":
        return CORPUS / src.parts[0] / src.parts[1]
    return CORPUS / src.parent


def check_binary(binary: str) -> str:
    binary = str(Path(binary).absolute())
    if "/debug/" in binary:
        print(
            "WARNING: driving a DEBUG typst binary. Debug exports are slow and"
            " memory-hungry enough to take the host down at corpus scale;"
            " build with --release.",
            file=sys.stderr,
        )
    return binary


def export(binary: str, src: Path, fmt: str, out: Path, timeout: int = 180) -> bool:
    """One serial export. Success is the output file existing, nothing else."""
    # Export commands run from the corpus checkout so their relative source
    # paths resolve. Anchor the destination before changing cwd; otherwise a
    # caller-provided relative --out is interpreted inside the corpus while
    # the existence check below still looks in the caller's working directory.
    out = out.absolute()
    out.unlink(missing_ok=True)
    cmd = [binary, "compile", "--root", str(root_for(src))]
    if fmt != "pdf":
        cmd += ["--format", fmt]
    cmd += [str(src), str(out)]
    try:
        subprocess.run(cmd, capture_output=True, cwd=CORPUS, timeout=timeout)
    except subprocess.TimeoutExpired:
        return False
    return out.exists()


def read_parts(path: Path) -> dict[str, bytes]:
    with zipfile.ZipFile(path) as z:
        return {n: z.read(n) for n in z.namelist()}


def part_text(data: bytes) -> str:
    """One part's text, joined the way a reader sees it."""
    out = []
    for m in FLOW.finditer(data):
        # Group 1 is the matched text tag (w:t/m:t/a:t); group 2 is its text.
        # A boundary token captures neither, so it ends the current word.
        run = m.group(2)
        out.append(html.unescape(run.decode("utf-8", "replace")) if run else " ")
    # Alt text is a documented fallback, not a loss; an attribute never
    # continues a run.
    for m in ALT.findall(data):
        out.append(" " + html.unescape(m.decode("utf-8", "replace")) + " ")
    out.append(" ")
    return "".join(out)


def docx_text(parts: dict[str, bytes]) -> tuple[str, str]:
    """(body-and-notes text, running header/footer text)."""
    body, furniture = [], []
    for n, data in parts.items():
        if not (n.startswith("word/") and n.endswith(".xml")):
            continue
        (furniture if HDRFTR.match(n) else body).append(part_text(data))
    return "".join(body), "".join(furniture)


def pptx_text(parts: dict[str, bytes]) -> str:
    """Text visible on slides, in numeric slide order.

    Layouts, masters, themes, and notes are deliberately excluded: they do
    not appear as slide-body text in the PDF used by the audit and would turn
    inherited placeholder text or speaker notes into false survival.
    """
    slides = []
    for name, data in parts.items():
        if match := SLIDE.fullmatch(name):
            slides.append((int(match.group(1)), data))
    return "".join(part_text(data) for _, data in sorted(slides))


def norm(s: str) -> str:
    s = unicodedata.normalize("NFKD", s)
    return "".join(c for c in s if not unicodedata.combining(c)).lower()


def word_counts(text: str) -> collections.Counter:
    return collections.Counter(
        w for w in WORD.findall(norm(text)) if not CJK.search(w)
    )


def cjk_pair_counts(text: str) -> collections.Counter:
    """Adjacent CJK character pairs: segmentation-independent, loss-sensitive."""
    counts: collections.Counter = collections.Counter()
    for run in CJK_RUN.findall(text):
        if len(run) == 1:
            counts[run] += 1
        else:
            counts.update(run[i : i + 2] for i in range(len(run) - 1))
    return counts


def resolve_part_path(source_part: str, target: str) -> str:
    """Resolve a relationship target against its source part, per OPC rules."""
    if target.startswith("/"):
        raw = target[1:]
    else:
        base = source_part.rsplit("/", 1)[0] if "/" in source_part else ""
        raw = f"{base}/{target}" if base else target
    segs: list[str] = []
    for seg in raw.split("/"):
        if seg in ("", "."):
            continue
        if seg == "..":
            if segs:
                segs.pop()
        else:
            segs.append(seg)
    return "/".join(segs)

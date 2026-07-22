#!/usr/bin/env python3
"""Round-trip gate: `.pptx` → Typst → `.pptx`.

The corpus gate asks whether a real deck imports and compiles. This asks the
question that only the *pair* of crates can answer: **does what the importer
reads survive what the exporter writes?**

It matters because the two crates are each other's inverse and nothing else
checks that. A corpus tests a crate against the world; a round trip tests the
pair against each other, which is the only way to catch a construct both
handle but no real document happens to contain.

Three measurements, cheapest first:

  1. **Text.**  Every `<a:t>` run in the source deck should appear in the
     re-exported deck. Text loss is never acceptable and always detectable.
  2. **Shape count.**  Pictures and tables that go in should come out.
  3. **Geometry.**  Where a shape sat, in EMU. A shape that survives at the
     wrong coordinates is a different bug from one that vanishes, and the
     `Placed` fidelity mode exists precisely to make this comparable.

    python3 roundtrip.py --corpus /tmp/pptx-corpus/docs --limit 60
"""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import re
import subprocess
import sys
import tempfile
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
IMPORTER = REPO / "target" / "debug" / "examples" / "import"
TYPST = REPO / "target" / "debug" / "typst"

TEXT_RE = re.compile(rb"<a:t>([^<]*)</a:t>")
SLIDE_RE = re.compile(r"^ppt/slides/slide\d+\.xml$")
OFF_RE = re.compile(rb'<a:off x="(-?\d+)" y="(-?\d+)"/>')
PIC_RE = re.compile(rb"<p:pic>")
TBL_RE = re.compile(rb"<a:tbl>")


@dataclass
class Deck:
    text: str = ""
    slides: int = 0
    pics: int = 0
    tables: int = 0
    offsets: list[tuple[int, int]] = field(default_factory=list)
    #: Offsets belonging to pictures and drawn shapes, kept apart from text
    #: boxes because the two round-trip differently and averaging them hides
    #: which.
    graphic_offsets: list[tuple[int, int]] = field(default_factory=list)
    #: Metafile media parts. Typst cannot decode EMF/WMF at all, so a picture
    #: backed by one is a *documented* loss rather than a round-trip failure —
    #: counting it as one buries the real signal (82 of 132 pictures across
    #: the first 60 decks are metafiles).
    metafiles: int = 0


def read(path: Path) -> Deck:
    d = Deck()
    with zipfile.ZipFile(path) as z:
        d.metafiles = sum(1 for n in z.namelist() if n.lower().endswith((".emf", ".wmf")))
        for name in sorted(z.namelist()):
            if not SLIDE_RE.match(name):
                continue
            d.slides += 1
            blob = z.read(name)
            # Joined with a space, not concatenated: PowerPoint splits a
            # sentence across `<a:t>` runs at every formatting change, so
            # gluing them fuses "Red" "Color" into one token that matches
            # nothing. That alone reported four healthy decks as 0% text.
            d.text += " ".join(
                m.group(1).decode("utf8", "replace") for m in TEXT_RE.finditer(blob)
            )
            d.text += " "
            d.pics += len(PIC_RE.findall(blob))
            d.tables += len(TBL_RE.findall(blob))
            d.offsets += [(int(a), int(b)) for a, b in OFF_RE.findall(blob)]
            for m in re.finditer(rb"<p:(pic|graphicFrame)>.*?</p:\1>", blob, re.S):
                d.graphic_offsets += [
                    (int(a), int(b)) for a, b in OFF_RE.findall(m.group(0))
                ]
    return d


def words(text: str) -> list[str]:
    return re.findall(r"\w+", text.lower())


@dataclass
class Row:
    name: str
    ok: bool
    text_kept: float | None
    slides: tuple[int, int]
    pics: tuple[int, int]
    tables: tuple[int, int]
    geom_within_1pt: float | None
    geom_graphics: float | None = None
    metafiles: int = 0
    error: str = ""


def run_one(path: Path, timeout: int) -> Row:
    name = path.stem
    with tempfile.TemporaryDirectory() as tmp:
        tmpd = Path(tmp)
        typ, out = tmpd / "rt.typ", tmpd / "rt.pptx"
        proc = subprocess.run(
            [str(IMPORTER), str(path), str(typ)], capture_output=True, timeout=timeout, text=True
        )
        if proc.returncode != 0:
            return Row(name, False, None, (0, 0), (0, 0), (0, 0), None, None, 0, "import failed")
        comp = subprocess.run(
            [str(TYPST), "compile", "--format", "pptx", str(typ), str(out)],
            capture_output=True,
            timeout=timeout,
            text=True,
            cwd=tmp,
        )
        if comp.returncode != 0:
            first = next(
                (l for l in (comp.stderr or "").splitlines() if l.startswith("error:")), ""
            )
            return Row(name, False, None, (0, 0), (0, 0), (0, 0), None, None, 0, first[:60])

        try:
            a, b = read(path), read(out)
        except Exception as e:  # a package we wrote must be readable
            return Row(name, False, None, (0, 0), (0, 0), (0, 0), None, None, 0, f"reread: {e}")

        wa, wb = words(a.text), words(b.text)
        from collections import Counter

        ca, cb = Counter(wa), Counter(wb)
        kept = sum(min(n, cb[w]) for w, n in ca.items())
        text_kept = kept / len(wa) if wa else None

        # Geometry: for each source offset, is there an offset in the output
        # within a point? Set-based rather than positional, because the
        # exporter is free to reorder shapes within a slide.
        near = 0
        pool = list(b.offsets)
        for x, y in a.offsets:
            for i, (px, py) in enumerate(pool):
                if abs(px - x) <= 12700 and abs(py - y) <= 12700:
                    near += 1
                    pool.pop(i)
                    break
        geom = near / len(a.offsets) if a.offsets else None

        near_g = 0
        pool = list(b.graphic_offsets)
        for x, y in a.graphic_offsets:
            for i, (px, py) in enumerate(pool):
                if abs(px - x) <= 12700 and abs(py - y) <= 12700:
                    near_g += 1
                    pool.pop(i)
                    break
        geom_g = near_g / len(a.graphic_offsets) if a.graphic_offsets else None

        return Row(
            name,
            True,
            text_kept,
            (a.slides, b.slides),
            (a.pics, b.pics),
            (a.tables, b.tables),
            geom,
            geom_g,
            a.metafiles,
        )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", default="/tmp/pptx-corpus/docs")
    ap.add_argument("--filter", default="")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 4) - 2))
    ap.add_argument("--timeout", type=int, default=180)
    args = ap.parse_args()

    files = sorted(
        p
        for p in Path(args.corpus).iterdir()
        if p.suffix.lower() in (".pptx", ".pptm") and args.filter in p.name
    )
    if args.limit:
        files = files[: args.limit]
    if not files:
        print("no presentations", file=sys.stderr)
        return 2

    rows: list[Row] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futs = {pool.submit(run_one, f, args.timeout): f for f in files}
        for fut in concurrent.futures.as_completed(futs):
            try:
                r = fut.result()
            except Exception as e:
                print(f"  {futs[fut].stem[:48]:50s} ERROR {e}")
                continue
            rows.append(r)
            if not r.ok:
                print(f"  {r.name[:48]:50s} FAIL   {r.error}")
            elif (r.text_kept is not None and r.text_kept < 0.98) or (
                r.geom_within_1pt is not None and r.geom_within_1pt < 0.9
            ):
                # `None` is "the source had none of this", which is not a
                # loss. Printing it as 0% turned four text-free decks into
                # four phantom failures on the first run.
                text = "  n/a" if r.text_kept is None else f"{r.text_kept:5.0%}"
                geom = "  n/a" if r.geom_within_1pt is None else f"{r.geom_within_1pt:5.0%}"
                print(
                    f"  {r.name[:48]:50s} text {text} geom {geom} "
                    f"pic {r.pics[0]}->{r.pics[1]} tbl {r.tables[0]}->{r.tables[1]}"
                )

    good = [r for r in rows if r.ok]
    print("=" * 72)
    print(f"decks           : {len(rows)}")
    print(f"round-tripped   : {len(good)}/{len(rows)}")
    if good:
        texts = sorted(r.text_kept for r in good if r.text_kept is not None)
        geoms = sorted(r.geom_within_1pt for r in good if r.geom_within_1pt is not None)
        if texts:
            print(
                f"text kept       : mean {sum(texts)/len(texts):.1%}  "
                f"median {texts[len(texts)//2]:.1%}  min {texts[0]:.1%}"
            )
        if geoms:
            print(
                f"all offsets ≤1pt : mean {sum(geoms)/len(geoms):.1%}  "
                f"median {geoms[len(geoms)//2]:.1%}"
            )
        # Only over decks with no metafiles. A picture correctly refused as
        # EMF still contributes a source offset that can never be matched, so
        # including those decks measures the documented gap rather than the
        # geometry.
        gg = sorted(
            r.geom_graphics
            for r in good
            if r.geom_graphics is not None and r.metafiles == 0
        )
        if gg:
            print(
                f"  pictures/frames : mean {sum(gg)/len(gg):.1%}  "
                f"median {gg[len(gg)//2]:.1%}  "
                f"(n={len(gg)}, metafile-free decks only)"
            )
        kept_pics = sum(r.pics[1] for r in good)
        src_pics = sum(r.pics[0] for r in good)
        meta = sum(r.metafiles for r in good)
        tbls = sum(r.tables[1] for r in good), sum(r.tables[0] for r in good)
        reachable = max(src_pics - meta, 0)
        # Can exceed the source count, and legitimately: a layout's and
        # master's own pictures are drawn behind every slide that uses them,
        # so one shared logo becomes one picture per slide.
        print(
            f"pictures        : {kept_pics} out of {src_pics} on the source slides "
            f"({meta} are EMF/WMF Typst cannot decode; {reachable} reachable; "
            f"inherited decoration adds more)"
        )
        print(f"tables          : {tbls[0]}/{tbls[1]} kept")
    return 0 if len(good) == len(rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())

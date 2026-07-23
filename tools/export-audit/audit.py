# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Corpus-scale export audit: invariants, text fidelity, and ranked visuals.

The fixture gate (tools/docx-validate, tests/src/*.rs) proves authored cases
under authored conditions. Every exporter bug found in the 2026-07 audit
sessions lived outside that space — in the corpus cross-product of construct,
styling, and realization path. This tool sweeps that space in three layers,
cheapest first, and spends expensive attention (vision) only where cheap
signals point:

  selftest    prove every detector against planted defects (run this first;
              a checker that can only report success is not a gate)
  invariants  referential integrity + determinism over real exports
  text        PDF-vs-package text fidelity (occurrence- and CJK-aware)
  visual      LibreOffice-render similarity scores per document -> scores.json
  rank        order scores worst-first, optionally against a baseline
  sheet       contact sheets (typst | office render) for the worst pages,
              sized for a human or vision-model spot check

Usage:
  uv run audit.py selftest
  uv run audit.py invariants --binary target/release/typst --kind document -n 150
  uv run audit.py text       --binary target/release/typst -n 150
  uv run audit.py visual     --binary target/release/typst --kind presentation -n 40
  uv run audit.py rank  --scores /tmp/export-audit/scores.json [--baseline old.json]
  uv run audit.py sheet --scores /tmp/export-audit/scores.json --binary ... -k 8
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import detectors
from corpuslib import check_binary, docs, docx_text, export, read_parts

SOFFICE = "/opt/homebrew/bin/soffice"


def _pages(pdf: Path, out_prefix: Path, max_pages: int) -> list[Path]:
    subprocess.run(
        ["pdftoppm", "-png", "-gray", "-r", "60", "-f", "1", "-l", str(max_pages),
         str(pdf), str(out_prefix)],
        capture_output=True, timeout=300,
    )
    return sorted(out_prefix.parent.glob(out_prefix.name + "-*.png"))


def _thumb(png: Path):
    from PIL import Image

    with Image.open(png) as im:
        return list(im.convert("L").resize((96, 128)).tobytes())


def _agreement(a: list[int], b: list[int]) -> float:
    return 1.0 - sum(abs(x - y) for x, y in zip(a, b)) / (255.0 * len(a))


def _worst_tile(a: list[int], b: list[int], cols: int = 4, rows: int = 6) -> float:
    """Agreement of the WORST tile of a 4x6 grid over the 96x128 thumbs.

    A whole-page mean dilutes a localized defect ~12,000:1 — measured: the
    overlapping-stack bug moved the page mean by 0.003, indistinguishable
    from rasterization noise. A collapsed panel dominates its own tile.
    """
    w, h = 96, 128
    tw, th = w // cols, h // rows
    worst = 1.0
    for ty in range(rows):
        for tx in range(cols):
            ta, tb = [], []
            for y in range(ty * th, (ty + 1) * th):
                row = y * w + tx * tw
                ta.extend(a[row : row + tw])
                tb.extend(b[row : row + tw])
            worst = min(worst, _agreement(ta, tb))
    return worst


def cmd_selftest(_args) -> int:
    failures = detectors.run_canaries()
    for f in failures:
        print(f"CANARY FAILED: {f}")
    print(f"{len(detectors.CANARIES) - len(failures)}/{len(detectors.CANARIES)} canaries pass")
    return 1 if failures else 0


def cmd_invariants(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    is_deck = args.kind == "presentation"
    fmt, ext = ("pptx", "pptx") if is_deck else ("docx", "docx")
    failures, findings = [], {}

    for src in docs(args.n, args.kind, args.filter):
        a, b = work / f"a.{ext}", work / f"b.{ext}"
        if not (export(binary, src, fmt, a) and export(binary, src, fmt, b)):
            failures.append(str(src))
            continue
        pa, pb = read_parts(a), read_parts(b)
        found = {}
        if moved := detectors.nondeterminism(pa, pb):
            found["nondeterministic_parts"] = moved
        if is_deck:
            if w := detectors.wordprocessing_in_slides(pa):
                found["wordprocessing_in_slides"] = w
            if r := detectors.unresolved_rids(pa):
                found["unresolved_rids"] = r
        else:
            if d := detectors.dead_anchors(pa):
                found["dead_anchors"] = d
            if r := detectors.dangling_rels(pa):
                found["dangling_rels"] = r
            if n := detectors.unresolved_numids(pa):
                found["unresolved_numids"] = n
        if found:
            findings[str(src)] = found

    _report_run(failures, findings, "invariant findings")
    return 1 if findings else 0


def cmd_text(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    failures, findings = [], {}

    for src in docs(args.n, "document", args.filter):
        pdf, docx = work / "t.pdf", work / "t.docx"
        if not (export(binary, src, "pdf", pdf) and export(binary, src, "docx", docx)):
            failures.append(str(src))
            continue
        t = subprocess.run(["pdftotext", str(pdf), "-"], capture_output=True, timeout=120)
        if t.returncode != 0:
            failures.append(f"{src} (pdftotext)")
            continue
        body, furniture = docx_text(read_parts(docx))
        loss = detectors.text_loss(t.stdout.decode("utf-8", "replace"), body, furniture)
        if detectors.text_fired(loss):
            findings[str(src)] = {
                "missing": loss["missing"][:8],
                "deficit_tokens": sum(loss["deficit"].values()),
                "cjk_pairs_lost": len(loss["cjk_pair_loss"]),
            }

    _report_run(failures, findings, "text-loss findings")
    return 0


def cmd_visual(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    is_deck = args.kind == "presentation"
    fmt = "pptx" if is_deck else "docx"
    window = 0 if is_deck else 2  # decks are one page per slide by design
    scores, failures = {}, []

    for src in docs(args.n, args.kind, args.filter):
        gold_pdf, office = work / "g.pdf", work / f"o.{fmt}"
        if not (export(binary, src, "pdf", gold_pdf) and export(binary, src, fmt, office)):
            failures.append(str(src))
            continue
        for stale in work.glob("*.png"):
            stale.unlink()
        subprocess.run(
            [SOFFICE, "--headless", "--convert-to", "pdf", "--outdir", str(work),
             str(office)],
            capture_output=True, timeout=600,
        )
        conv = work / f"o.pdf"
        if not conv.exists():
            failures.append(f"{src} (soffice)")
            continue
        gold = [_thumb(p) for p in _pages(gold_pdf, work / "gold", args.pages)]
        got = [_thumb(p) for p in _pages(conv, work / "got", args.pages)]
        if not gold or not got:
            failures.append(f"{src} (raster)")
            continue
        per_page, per_tile = [], []
        for i, page in enumerate(got):
            lo, hi = max(0, i - window), min(len(gold), i + window + 1)
            best, best_j = 0.0, None
            for j in range(lo, hi):
                a = _agreement(page, gold[j])
                if a > best:
                    best, best_j = a, j
            per_page.append(round(best, 4))
            per_tile.append(
                round(_worst_tile(page, gold[best_j]), 4) if best_j is not None else 0.0
            )
        scores[str(src)] = {
            "score": round(sum(per_page) / len(per_page), 4),
            "tile": min(per_tile) if per_tile else 0.0,
            "pages": [len(got), len(gold)],
            "per_page": per_page,
            "per_tile": per_tile,
        }

    out = Path(args.out) / "scores.json"
    out.write_text(json.dumps(scores, indent=1))
    _report_run(failures, {}, "")
    print(f"{len(scores)} documents scored -> {out}")
    return 0


def cmd_abdiff(args) -> int:
    """Same document, two binaries, one renderer: the regression instrument.

    Gold-vs-office scores carry a cross-renderer noise floor that hides
    localized defects even from tiles. Rendering BOTH binaries' output
    through the same LibreOffice removes that floor: any page whose
    worst tile moves is a real output change, and the sheet shows it.
    """
    a_bin, b_bin = check_binary(args.binary_a), check_binary(args.binary_b)
    fmt = "pptx" if args.kind == "presentation" else "docx"
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    flagged = 0

    for src in docs(args.n, args.kind, args.filter):
        thumbs = {}
        for label, binary in (("A", a_bin), ("B", b_bin)):
            office = work / f"{label}.{fmt}"
            if not export(binary, src, fmt, office):
                thumbs = {}
                break
            subprocess.run([SOFFICE, "--headless", "--convert-to", "pdf",
                            "--outdir", str(work), str(office)],
                           capture_output=True, timeout=600)
            pdf = work / f"{label}.pdf"
            if not pdf.exists():
                thumbs = {}
                break
            for stale in work.glob(f"{label}t-*.png"):
                stale.unlink()
            thumbs[label] = [
                _thumb(p) for p in _pages(pdf, work / f"{label}t", args.pages)
            ]
        if not thumbs:
            print(f"  SKIP (export/convert failed): {src}")
            continue
        pages_a, pages_b = thumbs["A"], thumbs["B"]
        if len(pages_a) != len(pages_b):
            flagged += 1
            print(f"  PAGE COUNT {len(pages_a)} -> {len(pages_b)}: {src}")
            continue
        for i, (pa, pb) in enumerate(zip(pages_a, pages_b)):
            tile = _worst_tile(pa, pb)
            if tile < args.threshold:
                flagged += 1
                sheet = work / f"abdiff_{Path(src).stem}_p{i + 1}.png"
                for side in ("A", "B"):
                    subprocess.run(["pdftoppm", "-png", "-r", "90",
                                    "-f", str(i + 1), "-l", str(i + 1),
                                    str(work / f"{side}.pdf"),
                                    str(work / f"{side}big")],
                                   capture_output=True, timeout=300)
                halves = sorted(work.glob("Abig-*.png")) + sorted(
                    work.glob("Bbig-*.png"))
                subprocess.run(["magick", *map(str, halves), "+append",
                                str(sheet)], capture_output=True)
                for h in halves:
                    h.unlink()
                print(f"  CHANGED p{i + 1} worst-tile={tile:.4f}: {src}"
                      f"  -> {sheet.name}")
    print(f"{flagged} changed pages/documents flagged"
          f" (threshold {args.threshold})")
    return 1 if flagged else 0


def cmd_rank(args) -> int:
    scores = json.loads(Path(args.scores).read_text())
    baseline = json.loads(Path(args.baseline).read_text()) if args.baseline else {}
    rows = []
    for doc, s in scores.items():
        drop = (baseline[doc]["score"] - s["score"]) if doc in baseline else None
        worst = min(range(len(s["per_page"])), key=lambda i: s["per_page"][i])
        rows.append((drop if drop is not None else -s["score"], doc, s, drop, worst))
    rows.sort(reverse=True)
    print(f"{'score':>6} {'drop':>6} {'pages':>7}  worst-page  document")
    for _, doc, s, drop, worst in rows[: args.k]:
        d = f"{drop:+.3f}" if drop is not None else "     -"
        print(f"{s['score']:6.3f} {d:>6} {s['pages'][0]:3}/{s['pages'][1]:<3} "
              f"p{worst + 1}={s['per_page'][worst]:.3f}  {doc}")
    return 0


def cmd_sheet(args) -> int:
    scores = json.loads(Path(args.scores).read_text())
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    worst_docs = sorted(scores, key=lambda d: scores[d]["score"])[: args.k]
    fmt = "pptx" if args.kind == "presentation" else "docx"

    for idx, doc in enumerate(worst_docs):
        src = Path(doc)
        per = scores[doc]["per_page"]
        page = min(range(len(per)), key=lambda i: per[i]) + 1
        gold_pdf, office = work / "g.pdf", work / f"o.{fmt}"
        if not (export(binary, src, "pdf", gold_pdf) and export(binary, src, fmt, office)):
            continue
        subprocess.run([SOFFICE, "--headless", "--convert-to", "pdf",
                        "--outdir", str(work), str(office)],
                       capture_output=True, timeout=600)
        for side, pdf in (("L", gold_pdf), ("R", work / "o.pdf")):
            subprocess.run(["pdftoppm", "-png", "-r", "90", "-f", str(page),
                            "-l", str(page), str(pdf), str(work / side)],
                           capture_output=True, timeout=300)
        halves = sorted(work.glob("L-*.png")) + sorted(work.glob("R-*.png"))
        sheet = work / f"sheet_{idx:02}_{src.stem}_p{page}.png"
        subprocess.run(["magick", *map(str, halves), "+append", str(sheet)],
                       capture_output=True)
        for h in halves:
            h.unlink()
        print(f"  {sheet.name}  (score {per[page - 1]:.3f}: typst left, {fmt} right)")
    return 0


def _report_run(failures: list[str], findings: dict, label: str) -> None:
    if failures:
        print(f"EXPORT FAILURES ({len(failures)}):")
        for f in failures:
            print(f"  {f}")
    if label:
        print(f"{len(findings)} documents with {label}")
        for doc, found in findings.items():
            print(f"  {doc}")
            for kind, items in found.items():
                shown = items if isinstance(items, int) else items[:4]
                print(f"      {kind}: {shown}")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--binary", help="ABSOLUTE path to a release typst binary")
    common.add_argument("-n", type=int, default=100)
    common.add_argument("--kind", default="document",
                        choices=["document", "presentation"])
    common.add_argument("--filter", default="")
    common.add_argument("--out", default="/tmp/export-audit")
    sub.add_parser("selftest", parents=[common]).set_defaults(fn=cmd_selftest)
    sub.add_parser("invariants", parents=[common]).set_defaults(fn=cmd_invariants)
    sub.add_parser("text", parents=[common]).set_defaults(fn=cmd_text)
    vis = sub.add_parser("visual", parents=[common])
    vis.add_argument("--pages", type=int, default=10)
    vis.set_defaults(fn=cmd_visual)
    ab = sub.add_parser("abdiff", parents=[common])
    ab.add_argument("--binary-a", required=True, help="baseline typst binary")
    ab.add_argument("--binary-b", required=True, help="candidate typst binary")
    ab.add_argument("--pages", type=int, default=12)
    ab.add_argument("--threshold", type=float, default=0.985)
    ab.set_defaults(fn=cmd_abdiff)
    rank = sub.add_parser("rank", parents=[common])
    rank.add_argument("--scores", required=True)
    rank.add_argument("--baseline")
    rank.add_argument("-k", type=int, default=20)
    rank.set_defaults(fn=cmd_rank)
    sheet = sub.add_parser("sheet", parents=[common])
    sheet.add_argument("--scores", required=True)
    sheet.add_argument("-k", type=int, default=8)
    sheet.set_defaults(fn=cmd_sheet)
    args = p.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    raise SystemExit(main())

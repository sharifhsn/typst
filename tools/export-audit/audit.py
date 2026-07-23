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
import re
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import detectors
from corpuslib import check_binary, docs, docx_text, export, read_parts, root_for

SOFFICE = "/opt/homebrew/bin/soffice"
WORD_APP = "Microsoft Word"

_WORDBOX = re.compile(
    rb'<word xMin="[0-9.]+" yMin="([0-9.]+)" xMax="[0-9.]+" yMax="([0-9.]+)"'
)
_SZ = re.compile(rb'<w:sz w:val="([0-9]+)"')


def _pdf_height_classes(pdf: Path) -> int:
    """Distinct rounded word-height classes in the PDF, from pdftotext -bbox."""
    r = subprocess.run(
        ["pdftotext", "-bbox", str(pdf), "-"], capture_output=True, timeout=120
    )
    heights = {
        round(float(m.group(2)) - float(m.group(1)))
        for m in _WORDBOX.finditer(r.stdout)
    }
    return len({h for h in heights if h > 0})


def _body_sz_count(parts: dict[str, bytes]) -> int:
    """Distinct `w:sz` values in the docx body flow. styles.xml is excluded:
    a style-defined size is not run-level survival, and counting it would mask
    a body whose every run collapsed to the default size."""
    vals: set[bytes] = set()
    for n, d in parts.items():
        if n.startswith("word/") and n.endswith(".xml") and n != "word/styles.xml":
            vals.update(_SZ.findall(d))
    return len(vals)


def _source_link_dests(binary: str, src: Path) -> list[str] | None:
    """http(s) link destinations declared in the source, or None if the query
    could not be run (so the caller can tell 'no links' from 'no answer')."""
    r = subprocess.run(
        [binary, "query", "--root", str(root_for(src)), str(src),
         "link", "--field", "dest", "--format", "json"],
        capture_output=True, cwd=str(Path.home() / "Code/typst-corpus"), timeout=120,
    )
    try:
        dests = json.loads(r.stdout.decode("utf-8", "replace") or "[]")
    except json.JSONDecodeError:
        return None
    return [d for d in dests if isinstance(d, str) and d.startswith(("http://", "https://"))]


def _pages(pdf: Path, out_prefix: Path, max_pages: int) -> list[Path]:
    # Rendered in colour (not -gray): the thumb greyscales itself, but the
    # saturation signal needs the colour to survive to this point.
    subprocess.run(
        ["pdftoppm", "-png", "-r", "60", "-f", "1", "-l", str(max_pages),
         str(pdf), str(out_prefix)],
        capture_output=True, timeout=300,
    )
    return sorted(out_prefix.parent.glob(out_prefix.name + "-*.png"))


def _soffice_pdf(office: Path, work: Path, timeout: int = 300) -> Path | None:
    """Convert one Office file to PDF through LibreOffice, guarding the hang.

    A single soffice invocation can wedge on a pathological document. An
    unguarded `TimeoutExpired` there raised straight through the sweep loop
    and destroyed every score already computed — a whole run lost to one bad
    document. Here a timeout, a crash, or a missing output is a per-document
    failure the caller records and steps past, exactly like a failed export."""
    out = work / (office.stem + ".pdf")
    out.unlink(missing_ok=True)
    try:
        subprocess.run(
            [SOFFICE, "--headless", "--convert-to", "pdf", "--outdir", str(work),
             str(office)],
            capture_output=True, timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        # The launcher dies on timeout but the soffice.bin daemon it forked
        # survives, still chewing the pathological document and holding the
        # profile lock — which then hangs every conversion after it. Reaping it
        # is what turns one bad document into one skip instead of a dead sweep.
        subprocess.run(["pkill", "-9", "-f", "soffice.bin"], capture_output=True)
        return None
    return out if out.exists() else None


def _word_ready(timeout: int = 40) -> bool:
    """Brings Word up in the background and waits until it answers.

    A *cold* launch does not answer AppleScript for a long time — an unguarded
    conversion against a cold Word sat for over two minutes and produced
    nothing. Launch detached with `-g` (so it never steals focus), then poll a
    cheap query until it responds; every later conversion reuses that warm
    instance.
    """
    subprocess.run(["open", "-g", "-a", WORD_APP], capture_output=True)
    deadline = timeout
    while deadline > 0:
        r = subprocess.run(
            ["osascript", "-e", f'tell application "{WORD_APP}" to return name of it'],
            capture_output=True, timeout=30,
        )
        if r.returncode == 0 and b"Word" in r.stdout:
            return True
        deadline -= 2
        subprocess.run(["sleep", "2"], capture_output=True)
    return False


def _word_pdf(office: Path, work: Path, timeout: int = 180) -> Path | None:
    """Convert one document to PDF through *real* Microsoft Word.

    The same contract as [`_soffice_pdf`]: a hang, a refusal, or a missing
    output is a per-document failure the caller records and steps past. Word is
    the format's actual consumer, so this is ground truth where LibreOffice is
    only a proxy — but it is also a GUI app being driven, so it is the slower
    of the two and belongs on a reduced document set.

    `open` does not bind a usable document reference in Word for Mac (it fails
    with `-2753`), hence the separate `active document`.
    """
    out = work / (office.stem + "_word.pdf")
    out.unlink(missing_ok=True)
    script = (
        f'tell application "{WORD_APP}"\n'
        f'  open POSIX file "{office}"\n'
        f"  set theDoc to active document\n"
        f'  save as theDoc file name "{out}" file format format PDF\n'
        f"  close theDoc saving no\n"
        f"end tell\n"
    )
    try:
        subprocess.run(
            ["osascript", "-"], input=script.encode(), capture_output=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        # Leave no modal document behind to wedge the next conversion.
        subprocess.run(
            ["osascript", "-e",
             f'tell application "{WORD_APP}" to close every document saving no'],
            capture_output=True, timeout=60,
        )
        return None
    return out if out.exists() else None


def _consumer_pdf(office: Path, work: Path, consumer: str) -> Path | None:
    """Render one Office file through the requested consumer."""
    return _word_pdf(office, work) if consumer == "word" else _soffice_pdf(office, work)


def _thumb(png: Path):
    from PIL import Image

    with Image.open(png) as im:
        return list(im.convert("L").resize((96, 128)).tobytes())


def _saturation(png: Path) -> float:
    """Mean HSV saturation of a page, 0..1, at low res. Grayscale text on a
    white ground sits near 0; a filled colour panel or gradient lifts it. The
    metric is magnitude-only, so LibreOffice's hue shifts do not move it —
    only colour genuinely vanishing does."""
    from PIL import Image

    with Image.open(png) as im:
        s = im.convert("HSV").resize((48, 64)).getchannel("S")
        px = s.tobytes()
    return sum(px) / (255.0 * len(px)) if px else 0.0


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
        if dup := detectors.duplicate_ids(pa):
            found["duplicate_ids"] = dup
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
            if t := detectors.drawings_in_text_boxes(pa):
                found["drawings_in_text_boxes"] = t
        if found:
            findings[str(src)] = found

    _report_run(failures, findings, "invariant findings")
    return 1 if findings else 0


def cmd_text(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    failures, findings = [], {}
    ratios: list[float] = []
    link_docs = link_lost = 0

    for src in docs(args.n, "document", args.filter):
        pdf, docx = work / "t.pdf", work / "t.docx"
        if not (export(binary, src, "pdf", pdf) and export(binary, src, "docx", docx)):
            failures.append(str(src))
            continue
        t = subprocess.run(["pdftotext", str(pdf), "-"], capture_output=True, timeout=120)
        if t.returncode != 0:
            failures.append(f"{src} (pdftotext)")
            continue
        pdf_text = t.stdout.decode("utf-8", "replace")
        parts = read_parts(docx)
        body, furniture = docx_text(parts)
        loss = detectors.text_loss(pdf_text, body, furniture)
        seq = detectors.sequence_similarity(pdf_text, body)
        ratios.append(seq)
        collapsed = detectors.formatting_collapse(
            _pdf_height_classes(pdf), _body_sz_count(parts)
        )
        found: dict = {}
        if detectors.text_fired(loss):
            found["missing"] = loss["missing"][:8]
            found["deficit_tokens"] = sum(loss["deficit"].values())
            found["cjk_pairs_lost"] = len(loss["cjk_pair_loss"])
        if seq < detectors.SEQ_FLAG:
            found["reading_order"] = seq
        if collapsed:
            found["formatting_collapsed"] = True
        if args.links:
            dests = _source_link_dests(binary, src)
            if dests:
                link_docs += 1
                lost = detectors.external_link_survival(dests, parts)
                if lost:
                    link_lost += 1
                    found["links_lost"] = lost[:6]
        if found:
            findings[str(src)] = found

    _report_run(failures, findings, "text/order/formatting findings")
    if ratios:
        qs = statistics.quantiles(ratios, n=20) if len(ratios) >= 2 else [ratios[0]]
        print(
            f"sequence ratio over {len(ratios)} docs: "
            f"min={min(ratios):.3f} p5={qs[0]:.3f} "
            f"median={statistics.median(ratios):.3f} max={max(ratios):.3f} "
            f"(flag < {detectors.SEQ_FLAG})"
        )
    if args.links:
        print(f"external links: {link_docs} docs had http(s) links,"
              f" {link_lost} lost at least one (advisory)")
    return 0


def cmd_visual(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    is_deck = args.kind == "presentation"
    fmt = "pptx" if is_deck else "docx"
    window = 0 if is_deck else 2  # decks are one page per slide by design
    scores, failures = {}, []
    out = Path(args.out) / "scores.json"
    if args.consumer == "word" and not _word_ready():
        print("Word did not become scriptable; aborting", file=sys.stderr)
        return 1

    for src in docs(args.n, args.kind, args.filter):
        gold_pdf, office = work / "g.pdf", work / f"o.{fmt}"
        if not (export(binary, src, "pdf", gold_pdf) and export(binary, src, fmt, office)):
            failures.append(str(src))
            continue
        for stale in work.glob("*.png"):
            stale.unlink()
        conv = _consumer_pdf(office, work, args.consumer)
        if conv is None:
            failures.append(f"{src} ({args.consumer})")
            continue
        gold_pngs = _pages(gold_pdf, work / "gold", args.pages)
        got_pngs = _pages(conv, work / "got", args.pages)
        gold = [_thumb(p) for p in gold_pngs]
        got = [_thumb(p) for p in got_pngs]
        if not gold or not got:
            failures.append(f"{src} (raster)")
            continue
        gold_sat = [_saturation(p) for p in gold_pngs]
        got_sat = [_saturation(p) for p in got_pngs]
        per_page, per_tile, desat = [], [], False
        for i, page in enumerate(got):
            lo, hi = max(0, i - window), min(len(gold), i + window + 1)
            best, best_j = 0.0, None
            for j in range(lo, hi):
                a = _agreement(page, gold[j])
                if a > best:
                    best, best_j = a, j
            per_page.append(round(best, 4))
            if best_j is not None:
                per_tile.append(round(_worst_tile(page, gold[best_j]), 4))
                # Colour present in the typst render but gone from the office
                # render: the exporter stripped it. A near-zero office mean is
                # the vanishing case; LibreOffice's hue shifts keep the mean up.
                if gold_sat[best_j] > 0.05 and got_sat[i] < 0.01:
                    desat = True
            else:
                per_tile.append(0.0)
        scores[str(src)] = {
            "score": round(sum(per_page) / len(per_page), 4),
            "tile": min(per_tile) if per_tile else 0.0,
            "desaturated": desat,
            "pages": [len(got), len(gold)],
            "per_page": per_page,
            "per_tile": per_tile,
        }
        # Written every document, not once at the end: a soffice hang on doc N
        # must not throw away the N-1 scores already earned.
        out.write_text(json.dumps(scores, indent=1))

    out.write_text(json.dumps(scores, indent=1))
    _report_run(failures, {}, "")
    print(f"{len(scores)} documents scored -> {out}")
    return 0


# ---------------------------------------------------------------------------
# Generative cross-product: construct x styling-context x realization-path.
# The bugs a corpus finds only accidentally live in these combinations. Both
# lists are data: a new construct or context is one line, and its sentinel
# (zqxjkw<i>) is derived from its row index so the read-back stays automatic.
# ---------------------------------------------------------------------------

# Each src embeds %%S%%, replaced at generation with the construct's sentinel.
CONSTRUCTS: list[dict] = [
    {"name": "table_spans_fills", "src":
        "#table(columns: 3, fill: (x, y) => if y == 0 { luma(220) } else { none },\n"
        "  table.cell(colspan: 2)[%%S%% wide], [top],\n  [a], [b], [c])"},
    {"name": "nested_list", "src":
        "- %%S%% one\n  - two\n    - three\n- four"},
    {"name": "math_block_inline", "src":
        "%%S%% mixes inline $a^2 + b^2 = c^2$ with a block:\n"
        "$ sum_(i=1)^n i = frac(n (n + 1), 2) $"},
    {"name": "figure_caption_ref", "src":
        "#figure(rect(width: 3cm, height: 1cm)[box], caption: [%%S%% caption]) <genfig>\n"
        "Referenced in @genfig."},
    {"name": "footnote", "src":
        "Body text %%S%% with a footnote.#footnote[The footnote body.]"},
    {"name": "link_and_ref", "src":
        "#set heading(numbering: \"1.\")\n"
        "An #link(\"https://example.com/%%S%%\")[%%S%% external link].\n"
        "= Target section <gensec>\nJump to @gensec."},
    {"name": "heading_numbering", "src":
        "#set heading(numbering: \"1.1\")\n= %%S%% first\n== nested"},
    {"name": "columns", "src":
        "#columns(2)[\n  %%S%% column text long enough to flow across both balanced"
        " columns and keep going for a while so the break is real.\n]"},
    {"name": "place_rotate_scale", "src":
        "#place(top + right)[%%S%% placed]\n#rotate(18deg)[rotated %%S%%]\n"
        "#scale(130%)[scaled body]"},
    {"name": "grid_frac_sized", "src":
        "#grid(columns: (2cm, 1fr, 1fr),\n  [%%S%%], [b], [c],\n  [d], [e], [f])"},
    {"name": "bib_cite", "src":
        "Background claim %%S%% @genkey.\n#bibliography(\"refs.bib\")"},
    {"name": "raw_block", "src":
        "```rust\nfn %%S%%() -> u32 { 42 }\n```"},
    {"name": "smallcaps_styled", "src":
        "#smallcaps[%%S%% small caps] then #text(fill: red, weight: \"bold\")[bold red]"
        " and #underline[underlined]."},
]

# A context wraps the construct body in a styling context / realization path.
# `lang_de` sets the German locale; the constructs are deliberately CJK-free,
# which is the "CJK-font-free guard" — no glyph here needs a font we lack.
CONTEXTS: list[tuple[str, Any]] = [
    ("default", lambda b: b),
    ("text30", lambda b: "#set text(size: 30pt)\n" + b),
    ("lang_de", lambda b: "#set text(lang: \"de\")\n" + b),
    ("context", lambda b: "#context [\n" + b + "\n]"),
    ("show_block", lambda b: "#show: it => block(fill: luma(240), inset: 6pt, it)\n" + b),
    ("twocol", lambda b: "#set page(columns: 2)\n" + b),
    ("smallpage", lambda b: "#set page(width: 9cm, height: 12cm, margin: 0.8cm)\n" + b),
]

_REFS_BIB = (
    "@article{genkey, title={A Study of Testing}, author={Ada Author},"
    " year={2020}, journal={Journal of Testing}}\n"
)


def _gen_export(binary: str, typ: Path, fmt: str, out: Path, root: Path) -> bool:
    out.unlink(missing_ok=True)
    subprocess.run(
        [binary, "compile", "--root", str(root), "--format", fmt, str(typ), str(out)],
        capture_output=True, timeout=120,
    )
    return out.exists()


def _docx_findings(parts: dict[str, bytes]) -> list[str]:
    out = []
    if detectors.dead_anchors(parts):
        out.append("dead_anchors")
    if detectors.dangling_rels(parts):
        out.append("dangling_rels")
    if detectors.unresolved_numids(parts):
        out.append("numids")
    if detectors.duplicate_ids(parts):
        out.append("dup_ids")
    return out


def _pptx_findings(parts: dict[str, bytes]) -> list[str]:
    out = []
    if detectors.wordprocessing_in_slides(parts):
        out.append("w_in_slide")
    if detectors.unresolved_rids(parts):
        out.append("unresolved_rids")
    if detectors.duplicate_ids(parts):
        out.append("dup_ids")
    return out


def cmd_generate(args) -> int:
    binary = check_binary(args.binary)
    work = Path(args.out) / "generate"
    work.mkdir(parents=True, exist_ok=True)
    (work / "refs.bib").write_text(_REFS_BIB)

    matrix: dict[tuple[str, str], list[str]] = {}
    for i, construct in enumerate(CONSTRUCTS):
        sentinel = f"zqxjkw{i}"
        body = construct["src"].replace("%%S%%", sentinel)
        for ctx_name, wrap in CONTEXTS:
            if ctx_name in construct.get("skip", set()):
                matrix[(construct["name"], ctx_name)] = ["skipped"]
                continue
            typ = work / "gen.typ"
            typ.write_text(wrap(body) + "\n")
            found: list[str] = []
            docx, pptx = work / "gen.docx", work / "gen.pptx"
            if _gen_export(binary, typ, "docx", docx, work):
                parts = read_parts(docx)
                found += _docx_findings(parts)
                bodytext, furn = docx_text(parts)
                # Scoped to word/ deliberately: a sentinel surviving only in
                # the customXml/docProps fidelity sidecars is not delivered to
                # a reader, so that must NOT count as survival.
                word_bytes = b"".join(
                    d for pn, d in parts.items() if pn.startswith("word/")
                )
                if sentinel not in (bodytext + furn) and sentinel.encode() not in word_bytes:
                    found.append("sentinel_missing")
            else:
                found.append("docx_fail")
            if _gen_export(binary, typ, "pptx", pptx, work):
                found += _pptx_findings(read_parts(pptx))
            else:
                found.append("pptx_fail")
            matrix[(construct["name"], ctx_name)] = found

    # Grid: rows = constructs, cols = contexts, cell '.' ok / 'F' finding.
    col_abbr = [c[0][:6] for c in CONTEXTS]
    name_w = max(len(c["name"]) for c in CONSTRUCTS)
    print(" " * (name_w + 2) + " ".join(f"{a:>6}" for a in col_abbr))
    total_findings = 0
    detail: list[str] = []
    for construct in CONSTRUCTS:
        cells = []
        for ctx_name, _ in CONTEXTS:
            f = matrix[(construct["name"], ctx_name)]
            if not f:
                cells.append(f"{'.':>6}")
            elif f == ["skipped"]:
                cells.append(f"{'-':>6}")
            else:
                cells.append(f"{'F':>6}")
                total_findings += 1
                detail.append(f"{construct['name']} x {ctx_name}: {f}")
        print(f"{construct['name']:<{name_w}}  " + " ".join(cells))
    print(f"\n{len(CONSTRUCTS)}x{len(CONTEXTS)} = "
          f"{len(CONSTRUCTS) * len(CONTEXTS)} cells, {total_findings} with findings")
    for line in detail:
        print(f"  {line}")
    return 1 if total_findings else 0


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
            pdf = _soffice_pdf(office, work)
            if pdf is None:
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
    print(f"{'score':>6} {'drop':>6} {'pages':>7} {'col':>5}  worst-page  document")
    for _, doc, s, drop, worst in rows[: args.k]:
        d = f"{drop:+.3f}" if drop is not None else "     -"
        col = "DESAT" if s.get("desaturated") else ""
        print(f"{s['score']:6.3f} {d:>6} {s['pages'][0]:3}/{s['pages'][1]:<3} "
              f"{col:>5}  p{worst + 1}={s['per_page'][worst]:.3f}  {doc}")
    return 0


def cmd_sheet(args) -> int:
    scores = json.loads(Path(args.scores).read_text())
    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    worst_docs = sorted(scores, key=lambda d: scores[d]["score"])[: args.k]
    fmt = "pptx" if args.kind == "presentation" else "docx"
    if args.consumer == "word" and not _word_ready():
        print("Word did not become scriptable; aborting", file=sys.stderr)
        return 1

    for idx, doc in enumerate(worst_docs):
        src = Path(doc)
        per = scores[doc]["per_page"]
        page = min(range(len(per)), key=lambda i: per[i]) + 1
        gold_pdf, office = work / "g.pdf", work / f"o.{fmt}"
        if not (export(binary, src, "pdf", gold_pdf) and export(binary, src, fmt, office)):
            continue
        rendered = _consumer_pdf(office, work, args.consumer)
        if rendered is None:
            continue
        for side, pdf in (("L", gold_pdf), ("R", rendered)):
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


def cmd_resave(args) -> int:
    """Re-save OUR docx through LibreOffice, a real consumer, and compare word
    counts. A large drop means we emitted something the consumer silently
    dropped -- the nearest proxy to "Word repairs it" available without Word.
    Advisory: LibreOffice reflows and may itself gain or lose a few words."""
    from corpuslib import word_counts

    binary = check_binary(args.binary)
    work = Path(args.out); work.mkdir(parents=True, exist_ok=True)
    redir = work / "resaved"; redir.mkdir(exist_ok=True)
    failures, findings = [], {}

    for src in docs(args.n, "document", args.filter):
        ours = work / "orig.docx"
        if not export(binary, src, "docx", ours):
            failures.append(str(src)); continue
        for stale in redir.glob("*.docx"):
            stale.unlink()
        try:
            subprocess.run(
                [SOFFICE, "--headless", "--convert-to", "docx:MS Word 2007 XML",
                 "--outdir", str(redir), str(ours)],
                capture_output=True, timeout=300,
            )
        except subprocess.TimeoutExpired:
            failures.append(f"{src} (soffice timeout)"); continue
        redone = redir / "orig.docx"
        if not redone.exists():
            failures.append(f"{src} (soffice)"); continue
        ob, of = docx_text(read_parts(ours))
        rb, rf = docx_text(read_parts(redone))
        ours_wc = sum(word_counts(ob + " " + of).values())
        redo_wc = sum(word_counts(rb + " " + rf).values())
        if ours_wc == 0:
            continue
        loss = (ours_wc - redo_wc) / ours_wc
        if loss > args.loss:
            findings[str(src)] = {
                "our_words": ours_wc, "resaved_words": redo_wc,
                "loss_pct": round(loss * 100, 1),
            }

    _report_run(failures, findings, "resave word-loss findings")
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
                # A finding value is a scalar (count, ratio, bool) or a list
                # of names; only the latter is sliced.
                shown = items[:4] if isinstance(items, list) else items
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
    common.add_argument(
        "--consumer", default="soffice", choices=["soffice", "word"],
        help="which reader renders our output: LibreOffice (fast, a proxy) or"
             " real Microsoft Word (ground truth, slower — use a reduced -n)",
    )
    sub.add_parser("selftest", parents=[common]).set_defaults(fn=cmd_selftest)
    sub.add_parser("invariants", parents=[common]).set_defaults(fn=cmd_invariants)
    txt = sub.add_parser("text", parents=[common])
    txt.add_argument("--links", action="store_true",
                     help="also check external-link survival (a typst query per doc)")
    txt.set_defaults(fn=cmd_text)
    sub.add_parser("generate", parents=[common]).set_defaults(fn=cmd_generate)
    res = sub.add_parser("resave", parents=[common])
    res.add_argument("--loss", type=float, default=0.10,
                     help="flag when >loss fraction of words drop on re-save")
    res.set_defaults(fn=cmd_resave)
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

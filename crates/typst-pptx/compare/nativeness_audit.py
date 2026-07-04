# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Corpus-wide nativeness audit for the PPTX exporter.

For every presentation in the manifest subset:
  1. export .pptx with PPTX_DEBUG_RASTER=1, capturing raster events
  2. compile the gold PDF and pdftotext it
  3. compute: live words in <a:t> vs words in the PDF (text-recovery ratio),
     picture count, raster events by kind/reason, text chars swallowed

Usage: uv run nativeness_audit.py <typst-bin> <pres_manifest.tsv> <out.tsv>
"""
import os, re, subprocess, sys, tempfile, zipfile
from collections import Counter
from pathlib import Path

CORPUS = Path("/Users/sharif/Code/typst-corpus")
WORD = re.compile(r"[^\W\d_]{2,}", re.UNICODE)


def words(text):
    return len(WORD.findall(text))


def pptx_stats(path):
    z = zipfile.ZipFile(path)
    slides = [n for n in z.namelist() if re.match(r"ppt/slides/slide\d+\.xml", n)]
    xml = " ".join(z.read(s).decode("utf8", "replace") for s in slides)
    live = words(" ".join(re.findall(r"<a:t>([^<]*)</a:t>", xml)))
    pics = len(re.findall(r"<p:pic[ >]", xml))
    return len(slides), live, pics


def main():
    typst, manifest, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
    env = dict(os.environ, SOURCE_DATE_EPOCH="0", PPTX_DEBUG_RASTER="1")
    rows, agg_reasons, agg_chars = [], Counter(), Counter()
    entries = []
    for line in Path(manifest).read_text().splitlines():
        f = line.split("\t")
        if len(f) >= 7 and f[0] == "yes":
            entries.append((f[3], f[4], CORPUS / f[5], CORPUS / f[6]))
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        for i, (cat, name, root, entry) in enumerate(entries, 1):
            pptx = tmp / f"{name}.pptx"
            pdf = tmp / f"{name}.pdf"
            r = subprocess.run(
                [typst, "compile", "--root", str(root), str(entry), str(pptx)],
                capture_output=True, text=True, timeout=300, env=env,
            )
            if r.returncode != 0 or not pptx.exists():
                rows.append((cat, name, "EXPORT_FAIL", "", "", "", "", ""))
                continue
            events = re.findall(
                r"RASTERIZE kind=(\S+) reason=(\S+) text_chars=(\d+)", r.stderr)
            subprocess.run(
                [typst, "compile", "--root", str(root), str(entry), str(pdf)],
                capture_output=True, timeout=300, env=env,
            )
            txt = tmp / f"{name}.txt"
            subprocess.run(["pdftotext", str(pdf), str(txt)], capture_output=True)
            gold_words = words(txt.read_text(errors="replace")) if txt.exists() else 0
            slides, live, pics = pptx_stats(pptx)
            swallowed = sum(int(c) for _, _, c in events)
            reasons = Counter(f"{k}/{re_}" for k, re_, _ in events)
            for key, n in reasons.items():
                agg_reasons[key] += n
            for k, re_, c in events:
                agg_chars[f"{k}/{re_}"] += int(c)
            ratio = f"{live / gold_words:.3f}" if gold_words else "n/a"
            rows.append((cat, name, slides, gold_words, live, ratio, pics,
                         ";".join(f"{k}={v}" for k, v in sorted(reasons.items())),
                         swallowed))
            if i % 20 == 0:
                print(f"  {i}/{len(entries)}", flush=True)
    with open(out_path, "w") as fh:
        fh.write("cat\tname\tslides\tpdf_words\tlive_words\trecovery\tpics\traster_events\tchars_swallowed\n")
        for row in rows:
            fh.write("\t".join(str(x) for x in row) + "\n")
    print("\n=== AGGREGATE raster events (kind/reason: count, text_chars swallowed) ===")
    for key in sorted(agg_reasons):
        print(f"  {key}: {agg_reasons[key]} events, {agg_chars[key]} text chars")
    ok = [r for r in rows if r[2] != "EXPORT_FAIL" and r[5] != "n/a"]
    ratios = sorted(float(r[5]) for r in ok)
    if ratios:
        n = len(ratios)
        print(f"\n=== TEXT RECOVERY over {n} decks ===")
        print(f"  mean={sum(ratios)/n:.3f} median={ratios[n//2]:.3f} "
              f"p10={ratios[n//10]:.3f} min={ratios[0]:.3f}")
        print("  worst 12:")
        for r in sorted(ok, key=lambda r: float(r[5]))[:12]:
            print(f"    {r[5]}  {r[0]}/{r[1]}  pdf={r[3]} live={r[4]} pics={r[6]} chars_swallowed={r[8]}")
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main()

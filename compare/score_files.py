# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow", "numpy"]
# ///
"""Score pre-made office files (pptx/docx) against gold PDFs.

Same metric as typst-corpus/bench/visual_oracle.py: LibreOffice-render the
office file to PDF, rasterize both PDFs to a fixed-size grayscale strip,
mean per-pixel agreement in [0,1].

Usage: uv run score_files.py <candidate-dir> [--gold DIR] [--label NAME]
Gold PDFs are read from <gold>/<stem>.pdf (default: ./inputs next to this script).
"""
import subprocess, sys, tempfile
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
SOFF = "/opt/homebrew/bin/soffice"
STRIP_W, STRIP_H = 96, 2048
DPI = 60


def pdf_strip(pdf_path, workdir):
    prefix = workdir / "pg"
    for old in workdir.glob("pg*.png"):
        old.unlink()
    r = subprocess.run(
        ["pdftoppm", "-png", "-gray", "-r", str(DPI), str(pdf_path), str(prefix)],
        capture_output=True, timeout=300,
    )
    pages = sorted(workdir.glob("pg*.png"))
    if r.returncode != 0 or not pages:
        return None, 0
    imgs = []
    for p in pages:
        im = Image.open(p).convert("L")
        h = max(1, round(im.height * STRIP_W / im.width))
        imgs.append(np.asarray(im.resize((STRIP_W, h)), dtype=np.float32))
    strip = np.concatenate(imgs, axis=0)
    strip_img = Image.fromarray(strip.astype(np.uint8)).resize((STRIP_W, STRIP_H))
    return np.asarray(strip_img, dtype=np.float32), len(pages)


def office_to_pdf(office_path, outdir, profile):
    r = subprocess.run(
        [SOFF, "--headless", f"-env:UserInstallation=file://{profile}",
         "--convert-to", "pdf", "--outdir", str(outdir), str(office_path)],
        capture_output=True, timeout=180,
    )
    pdf = outdir / (office_path.stem + ".pdf")
    return pdf if pdf.exists() else None


def main():
    cand_dir = Path(sys.argv[1]).resolve()
    label = sys.argv[sys.argv.index("--label") + 1] if "--label" in sys.argv else cand_dir.name
    gold_dir = Path(sys.argv[sys.argv.index("--gold") + 1]).resolve() if "--gold" in sys.argv else HERE / "inputs"
    rows = []
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        for cand in sorted(list(cand_dir.glob("*.pptx")) + list(cand_dir.glob("*.docx"))):
            gold_pdf = gold_dir / (cand.stem + ".pdf")
            if not gold_pdf.exists():
                rows.append((cand.stem, "NO_GOLD", "", ""))
                continue
            conv = tmp / cand.stem
            conv.mkdir(exist_ok=True)
            cand_pdf = office_to_pdf(cand, conv, tmp / f"prof-{cand.stem}")
            if cand_pdf is None:
                rows.append((cand.stem, "CONVERT_FAIL", "", ""))
                continue
            gw = tmp / f"g-{cand.stem}"; gw.mkdir(exist_ok=True)
            cw = tmp / f"c-{cand.stem}"; cw.mkdir(exist_ok=True)
            gold, gp = pdf_strip(gold_pdf, gw)
            got, cp = pdf_strip(cand_pdf, cw)
            if gold is None or got is None:
                rows.append((cand.stem, "RASTER_FAIL", "", ""))
                continue
            score = float(1.0 - np.abs(gold - got).mean() / 255.0)
            rows.append((cand.stem, f"{score:.4f}", gp, cp))
    print(f"# {label}")
    for name, score, gp, cp in rows:
        print(f"{name}\t{score}\tpages {gp}->{cp}")


if __name__ == "__main__":
    main()

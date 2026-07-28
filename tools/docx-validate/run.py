# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Reproducible fixture validation for Typst's DOCX exporter.

The default gate is intentionally portable: compile a checked-in Typst fixture,
validate its OOXML package, verify expected semantic text, and count native OOXML
constructs that make a Word file editable. ``--visual`` is optional because it
requires a locally installed LibreOffice and Poppler.
"""

from __future__ import annotations

import argparse
import html
import hashlib
import json
import os
import platform
import re
import signal
import shutil
import subprocess
import sys
import tempfile
import zipfile
from collections import Counter
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any
from xml.etree import ElementTree


HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
WORD_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
MATH_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"
TEXT_TAGS = {f"{{{WORD_NS}}}t", f"{{{MATH_NS}}}t"}
TEXT_RE = re.compile(r"[^\W\d_]{2,}", re.UNICODE)
FIELD_INSTRUCTION = re.compile(
    r'<w:instrText(?:\s[^>]*)?>(.*?)</w:instrText>'
    r'|<w:fldSimple\b[^>]*\bw:instr="([^"]*)"',
    re.DOTALL,
)


def internal_hyperlink_field_count(document: str) -> int:
    """Count internal links stored as Word ``REF``/``PAGEREF`` fields.

    Word represents an internal link as a field instruction with ``\\h``;
    unlike an external relationship it need not use a ``w:hyperlink`` element.
    Keep this distinct from the literal-element count so external hyperlinks
    retain their existing editability check.
    """
    count = 0
    for instr_text, simple_instr in FIELD_INSTRUCTION.findall(document):
        instruction = html.unescape(instr_text or simple_instr)
        if re.match(r"\s*(?:PAGE)?REF\b", instruction, re.IGNORECASE) and re.search(
            r"\\h\b", instruction, re.IGNORECASE
        ):
            count += 1
    return count


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_version(command: list[str]) -> str | None:
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=20)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None
    text = (result.stdout or result.stderr).strip().splitlines()
    return text[0] if text else None


def git_revision() -> str | None:
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO, text=True, timeout=20
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError, subprocess.TimeoutExpired):
        return None


def run_command(
    command: list[str], *, cwd: Path | None = None, timeout: int = 180,
    extra_env: dict[str, str] | None = None,
) -> dict[str, Any]:
    started = datetime.now(timezone.utc)
    env = dict(os.environ, SOURCE_DATE_EPOCH="0")
    if extra_env:
        env.update(extra_env)
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        stdout, stderr = process.communicate(timeout=timeout)
        return {
            "command": command,
            "exit_code": process.returncode,
            "stdout": stdout[-4000:],
            "stderr": stderr[-4000:],
            "started_at": started.isoformat(),
        }
    except subprocess.TimeoutExpired:
        # `soffice` is commonly a shell wrapper that spawns the real office
        # process. Killing only the wrapper leaks a CPU-bound child and leaves
        # its temporary profile locked, so terminate the whole process group.
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        return {
            "command": command,
            "exit_code": None,
            "stdout": stdout[-4000:],
            "stderr": stderr[-4000:],
            "started_at": started.isoformat(),
            "timeout": timeout,
        }
    except OSError as error:
        return {
            "command": command,
            "exit_code": 127,
            "stdout": "",
            "stderr": str(error),
            "started_at": started.isoformat(),
        }


def word_text(parts: dict[str, bytes]) -> str:
    texts: list[str] = []
    for name, data in sorted(parts.items()):
        if not name.endswith(".xml") or not name.startswith("word/"):
            continue
        try:
            root = ElementTree.fromstring(data)
        except ElementTree.ParseError:
            continue
        texts.extend(node.text or "" for node in root.iter() if node.tag in TEXT_TAGS)
    return " ".join(texts)


def package_check(docx_path: Path) -> tuple[dict[str, Any], dict[str, bytes]]:
    result: dict[str, Any] = {"ok": False, "errors": [], "parts": 0}
    try:
        with zipfile.ZipFile(docx_path) as package:
            corrupt = package.testzip()
            if corrupt:
                result["errors"].append(f"corrupt ZIP entry: {corrupt}")
            names = package.namelist()
            if "word/document.xml" not in names:
                result["errors"].append("missing word/document.xml")
            parts = {name: package.read(name) for name in names}
    except (OSError, zipfile.BadZipFile) as error:
        result["errors"].append(f"not a readable ZIP: {error}")
        return result, {}

    for name, data in parts.items():
        if not (name.endswith(".xml") or name.endswith(".rels")):
            continue
        try:
            ElementTree.fromstring(data)
        except ElementTree.ParseError as error:
            result["errors"].append(f"invalid XML {name}: {error}")
    result["parts"] = len(parts)
    result["ok"] = not result["errors"]
    return result, parts


def editability_metrics(parts: dict[str, bytes]) -> dict[str, int]:
    document = parts.get("word/document.xml", b"").decode("utf-8", "replace")
    footnotes = parts.get("word/footnotes.xml", b"").decode("utf-8", "replace")
    endnotes = parts.get("word/endnotes.xml", b"").decode("utf-8", "replace")
    literal_hyperlinks = len(re.findall(r"<w:hyperlink(?: |>)", document))
    internal_hyperlink_fields = internal_hyperlink_field_count(document)
    return {
        "paragraphs": len(re.findall(r"<w:p(?: |>)", document)),
        "heading_styles": len(re.findall(r'w:pStyle w:val="Heading[1-9]"', document)),
        "lists": len(re.findall(r"<w:numPr(?: |>)", document)),
        "tables": len(re.findall(r"<w:tbl(?: |>)", document)),
        "table_headers": len(re.findall(r"<w:tblHeader(?: |/|>)", document)),
        "omml_math": len(re.findall(r"<m:oMath(?: |>)", document)),
        # A literal w:hyperlink is still counted for external links. REF and
        # PAGEREF fields with \\h are the native Word representation for
        # internal links, so the aggregate reflects either editable form.
        "hyperlinks": literal_hyperlinks + internal_hyperlink_fields,
        "literal_hyperlinks": literal_hyperlinks,
        "internal_hyperlink_fields": internal_hyperlink_fields,
        "footnotes": len(re.findall(r'<w:footnote w:id="(?:[0-9]|[1-9][0-9]+)"', footnotes)),
        "endnotes": len(re.findall(r'<w:endnote w:id="(?:[0-9]|[1-9][0-9]+)"', endnotes)),
        "drawings": len(re.findall(r"<w:drawing(?: |>)", document)),
        "alt_descriptions": len(re.findall(r"\bdescr=", document)),
        "content_controls": len(re.findall(r"<w:sdt(?: |>)", document)),
        "hidden_runs": len(re.findall(r"<w:vanish(?: |/|>)", document)),
    }


def normalized(value: str) -> str:
    return re.sub(r"\s+", " ", value.casefold()).strip()


def token_jaccard(left: str, right: str) -> float | None:
    if not left or not right:
        return None
    a, b = Counter(TEXT_RE.findall(left.casefold())), Counter(TEXT_RE.findall(right.casefold()))
    union = sum((a | b).values())
    return sum((a & b).values()) / union if union else 1.0


def pdf_text(pdf_path: Path) -> tuple[str | None, str | None]:
    if not shutil.which("pdftotext"):
        return None, "pdftotext unavailable"
    command = run_command(["pdftotext", "-nopgbrk", "-q", str(pdf_path), "-"], timeout=120)
    if command["exit_code"] != 0:
        return None, command["stderr"] or "pdftotext failed"
    return command["stdout"], None


def rasterize_pages(
    pdf_path: Path, directory: Path, prefix: str
) -> tuple[list[Path], str | None]:
    """Render PDF pages for both automated scoring and human visual review.

    Keep the page artifacts in color. The layout score intentionally converts
    them to grayscale in ``strip`` below, but grayscale source artifacts make
    it impossible to inspect color fidelity independently of layout fidelity.
    """
    if not shutil.which("pdftoppm"):
        return [], "pdftoppm unavailable"
    for stale in directory.glob(f"{prefix}-*.png"):
        stale.unlink(missing_ok=True)
    result = run_command(
        ["pdftoppm", "-png", "-r", "60", str(pdf_path), str(directory / prefix)],
        timeout=180,
    )
    pages = sorted(directory.glob(f"{prefix}-*.png"))
    if result.get("timeout"):
        return (
            [],
            f"pdftoppm timed out after {result['timeout']}s while rendering "
            f"{pdf_path.name}",
        )
    if result["exit_code"] != 0 or not pages:
        return [], result["stderr"] or "pdftoppm failed"
    return pages, None


def visual_check(
    pdf_path: Path, docx_path: Path, directory: Path, *,
    font_paths: list[Path] | None = None,
) -> dict[str, Any]:
    soffice = shutil.which("soffice")
    if not soffice:
        return {"status": "unavailable", "reason": "soffice unavailable"}
    rendered = directory / f"{docx_path.stem}.pdf"
    rendered.unlink(missing_ok=True)
    profile = Path(tempfile.mkdtemp(prefix="typst-docx-validator-lo-"))
    conversion_dir = Path(tempfile.mkdtemp(prefix="typst-docx-validator-out-"))
    converted = conversion_dir / f"{docx_path.stem}.pdf"
    try:
        conversion = run_command(
            [
                soffice,
                "--headless",
                f"-env:UserInstallation=file://{profile}",
                "--convert-to",
                "pdf",
                "--outdir",
                str(conversion_dir),
                str(docx_path),
            ],
            timeout=180,
            extra_env={
                "SAL_FONTPATH": os.pathsep.join(str(path) for path in font_paths or [])
            } if font_paths else None,
        )
        if conversion["exit_code"] == 0 and converted.is_file():
            shutil.move(converted, rendered)
    finally:
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(conversion_dir, ignore_errors=True)
    if conversion.get("timeout"):
        return {
            "status": "failed",
            "reason": f"LibreOffice conversion timed out after {conversion['timeout']}s",
            "consumer_timeout": conversion["timeout"],
        }
    if conversion["exit_code"] != 0 or not rendered.exists():
        return {"status": "failed", "reason": conversion["stderr"] or "LibreOffice produced no PDF"}

    gold_pages, gold_error = rasterize_pages(pdf_path, directory, "gold-page")
    docx_pages, docx_error = rasterize_pages(rendered, directory, "docx-page")
    if gold_error:
        return {
            "status": "unavailable",
            "stage": "reference_pdf_rasterization",
            "reason": gold_error,
        }
    if docx_error:
        return {
            "status": "unavailable",
            "stage": "consumer_pdf_rasterization",
            "reason": docx_error,
        }

    from PIL import Image, ImageChops, ImageOps

    def strip(pages: list[Path]) -> Image.Image:
        width, height = 96, 2048
        images = []
        for page in pages:
            image = ImageOps.grayscale(Image.open(page))
            scaled_height = max(1, round(image.height * width / image.width))
            images.append(image.resize((width, scaled_height)))
        joined = Image.new("L", (width, sum(image.height for image in images)), color=255)
        cursor = 0
        for image in images:
            joined.paste(image, (0, cursor))
            cursor += image.height
        return joined.resize((width, height))

    gold, rendered_strip = strip(gold_pages), strip(docx_pages)
    difference = ImageChops.difference(gold, rendered_strip)
    score = 1.0 - sum(difference.get_flattened_data()) / (
        255 * difference.width * difference.height
    )
    return {
        "status": "ok",
        "score": round(score, 6),
        "gold_pages": len(gold_pages),
        "docx_pages": len(docx_pages),
    }


def allowance_for(
    allowances: list[dict[str, Any]], fixture: str, gate: str
) -> dict[str, Any] | None:
    today = date.today().isoformat()
    for allowance in allowances:
        if allowance.get("fixture") != fixture or allowance.get("gate") != gate:
            continue
        if allowance.get("expires", "") < today:
            continue
        return allowance
    return None


def validate_fixture(
    fixture: dict[str, Any],
    args: argparse.Namespace,
    allowances: list[dict[str, Any]],
    output: Path,
    source_root: Path,
) -> dict[str, Any]:
    fixture_id = fixture["id"]
    source = source_root / fixture["entry"]
    artifact_dir = output / "artifacts" / fixture_id
    artifact_dir.mkdir(parents=True, exist_ok=True)
    docx_path, pdf_path = artifact_dir / f"{fixture_id}.docx", artifact_dir / f"{fixture_id}.pdf"
    root = source.parent
    try:
        source_label = str(source.relative_to(REPO))
    except ValueError:
        source_label = str(source)
    docx_compile = run_command(
        [args.typst, "compile", "--root", str(root), "--format", "docx", str(source), str(docx_path)]
    )
    pdf_compile = run_command(
        [args.typst, "compile", "--root", str(root), "--format", "pdf", str(source), str(pdf_path)]
    )
    result: dict[str, Any] = {
        "id": fixture_id,
        "source": source_label,
        "compile": {"docx": docx_compile, "pdf": pdf_compile},
        "gates": {},
        "status": "passed",
    }
    failures: list[str] = []
    if docx_compile["exit_code"] != 0 or not docx_path.exists():
        failures.append("compile-docx")
        result["gates"]["package"] = {"ok": False, "errors": ["DOCX compilation failed"]}
        result["status"] = "failed"
        result["failures"] = failures
        return result

    package, parts = package_check(docx_path)
    result["gates"]["package"] = package
    if not package["ok"]:
        failures.append("package")

    docx_text = word_text(parts)
    expected = fixture.get("expected_text", [])
    absent_docx = [text for text in expected if normalized(text) not in normalized(docx_text)]
    semantic: dict[str, Any] = {
        "docx_expected_text_missing": absent_docx,
        "docx_word_count": len(TEXT_RE.findall(docx_text)),
    }
    pdf_output, pdf_error = (None, "PDF compilation failed")
    if pdf_compile["exit_code"] == 0 and pdf_path.exists():
        pdf_output, pdf_error = pdf_text(pdf_path)
    if pdf_output is not None:
        semantic["pdf_expected_text_missing"] = [
            text for text in expected if normalized(text) not in normalized(pdf_output)
        ]
        semantic["word_jaccard"] = token_jaccard(pdf_output, docx_text)
    else:
        semantic["pdf_check"] = "unavailable"
        semantic["pdf_reason"] = pdf_error
    semantic["ok"] = not semantic["docx_expected_text_missing"] and not semantic.get(
        "pdf_expected_text_missing", []
    )
    result["gates"]["semantic"] = semantic
    if not semantic["ok"]:
        failures.append("semantic")

    metrics = editability_metrics(parts)
    minimums = fixture.get("minimums", {})
    missing_metrics = {
        metric: {"expected_at_least": expected_minimum, "actual": metrics.get(metric, 0)}
        for metric, expected_minimum in minimums.items()
        if metrics.get(metric, 0) < expected_minimum
    }
    editability = {"metrics": metrics, "minimums_missing": missing_metrics, "ok": not missing_metrics}
    result["gates"]["editability"] = editability
    if not editability["ok"]:
        failures.append("editability")

    if args.visual:
        visual = visual_check(pdf_path, docx_path, artifact_dir) if pdf_path.exists() else {
            "status": "failed", "reason": "PDF compilation failed"
        }
        if visual.get("status") == "ok":
            policy = fixture.get("visual", {})
            below_score = visual["score"] < policy.get("minimum_score", 0.0)
            page_delta = abs(visual["gold_pages"] - visual["docx_pages"])
            too_many_pages = page_delta > policy.get("max_page_delta", 1_000_000)
            visual["ok"] = not below_score and not too_many_pages
            visual["page_delta"] = page_delta
            if not visual["ok"]:
                failures.append("visual")
        elif visual.get("status") == "failed":
            visual["ok"] = False
            failures.append("visual")
        else:
            # A requested gate without evidence is not a pass. Keep this
            # distinct from a fidelity failure so corpus reports can classify
            # the document as unverified instead of degraded.
            visual["ok"] = False
            visual["unverified"] = True
            failures.append("visual-unverified")
        result["gates"]["visual"] = visual

    allowed: list[dict[str, Any]] = []
    unresolved: list[str] = []
    for failure in failures:
        allowance = allowance_for(allowances, fixture_id, failure)
        if allowance:
            allowed.append({"gate": failure, "allowance": allowance})
        else:
            unresolved.append(failure)
    result["allowed_failures"] = allowed
    result["failures"] = unresolved
    if any(failure.endswith("-unverified") for failure in unresolved):
        result["status"] = "unverified"
    else:
        result["status"] = (
            "failed" if unresolved else ("passed_with_allowances" if allowed else "passed")
        )
    return result


def validate_allowlist(allowlist: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    for index, allowance in enumerate(allowlist.get("allowances", [])):
        required = ("id", "fixture", "gate", "reason", "issue", "expires")
        absent = [key for key in required if not allowance.get(key)]
        if absent:
            errors.append(f"allowance {index} missing {', '.join(absent)}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--typst", required=True, help="Path to the typst CLI binary")
    parser.add_argument("--manifest", type=Path, default=HERE / "manifest.json")
    parser.add_argument("--allowlist", type=Path, default=HERE / "allowlist.json")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--mode", choices=("smoke", "full"), default="smoke")
    parser.add_argument("--visual", action="store_true", help="Run the optional LibreOffice visual lane")
    args = parser.parse_args()
    args.typst = str(Path(args.typst).resolve())
    if not Path(args.typst).is_file():
        parser.error(f"typst binary not found: {args.typst}")

    manifest = json.loads(args.manifest.read_text())
    allowlist = json.loads(args.allowlist.read_text())
    allowlist_errors = validate_allowlist(allowlist)
    args.out.mkdir(parents=True, exist_ok=True)
    metadata = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "mode": args.mode,
        "visual_requested": args.visual,
        "repo_revision": git_revision(),
        "manifest": str(args.manifest),
        "manifest_sha256": sha256(args.manifest),
        "allowlist": str(args.allowlist),
        "allowlist_sha256": sha256(args.allowlist),
        "platform": platform.platform(),
        "python": sys.version,
        "tools": {
            "typst": command_version([args.typst, "--version"]),
            "soffice": command_version([shutil.which("soffice") or "soffice", "--version"]),
            "pandoc": command_version(["pandoc", "--version"]),
            "pdftotext": command_version(["pdftotext", "-v"]),
            "pdftoppm": command_version(["pdftoppm", "-v"]),
        },
    }
    (args.out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")

    results = [
        validate_fixture(
            fixture,
            args,
            allowlist.get("allowances", []),
            args.out,
            args.manifest.parent,
        )
        for fixture in manifest["fixtures"]
    ]
    failed = [result["id"] for result in results if result["status"] == "failed"]
    unverified = [result["id"] for result in results if result["status"] == "unverified"]
    report = {
        "schema_version": 1,
        "metadata_file": "metadata.json",
        "allowlist_errors": allowlist_errors,
        "fixtures": results,
        "summary": {
            "total": len(results),
            "failed": len(failed),
            "unverified": len(unverified),
            "passed": sum(result["status"] == "passed" for result in results),
            "passed_with_allowances": sum(
                result["status"] == "passed_with_allowances" for result in results
            ),
        },
    }
    (args.out / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["summary"], sort_keys=True))
    return 1 if failed or unverified or allowlist_errors else 0


if __name__ == "__main__":
    raise SystemExit(main())

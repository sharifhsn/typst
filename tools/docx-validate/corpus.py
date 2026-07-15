# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow"]
# ///
"""Run the frozen public corpus through the DOCX evidence pipeline."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from xml.etree import ElementTree

import run as validator


FIDELITY_NS = "https://typst.app/schema/2026/fidelity"
NS = {"typst": FIDELITY_NS}
FONT_WARNING = re.compile(r"unknown font family: ([^\n]+)", re.IGNORECASE)
SLIDE_SHAPED_WARNING = "document has slide-shaped pages but is being exported to DOCX"
DRAWING_COORDINATE = re.compile(
    rb'(?P<attribute>\b(?:cx|cy|x|y|w|h))="(?P<value>-?\d+)"'
    rb'|<wp:posOffset>(?P<offset>-?\d+)</wp:posOffset>'
)
SUSPICIOUS_DRAWING_COORDINATE_EMU = 125_000_000


def bounded_output(value: str | None, limit: int = 12000) -> str:
    value = value or ""
    if len(value) <= limit:
        return value
    half = limit // 2
    return value[:half] + "\n...[middle truncated]...\n" + value[-half:]


def exporter_state() -> dict[str, Any]:
    status = subprocess.run(
        ["git", "status", "--porcelain=v1", "-z"], capture_output=True, check=False
    ).stdout
    diff = subprocess.run(
        ["git", "diff", "--binary", "HEAD"], capture_output=True, check=False
    ).stdout
    digest = hashlib.sha256()
    digest.update(diff)
    entries = [entry for entry in status.split(b"\0") if entry]
    untracked: list[str] = []
    for entry in entries:
        if not entry.startswith(b"?? "):
            continue
        path = entry[3:].decode("utf-8", "surrogateescape")
        candidate = Path(path)
        if candidate.is_file():
            untracked.append(path)
    for path in sorted(untracked):
        digest.update(path.encode("utf-8", "surrogateescape"))
        digest.update(Path(path).read_bytes())
    return {
        "revision": validator.git_revision(),
        "dirty": bool(entries),
        "status_sha256": hashlib.sha256(status).hexdigest(),
        "diff_and_untracked_sha256": digest.hexdigest(),
        "untracked_files": untracked,
    }


def safe_component(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip("-") or "document"


def command(
    argv: list[str], *, timeout: int, cwd: Path | None = None
) -> dict[str, Any]:
    started = datetime.now(timezone.utc)
    before = time.monotonic()
    env = dict(os.environ, SOURCE_DATE_EPOCH="0")
    try:
        result = subprocess.run(
            argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
        )
        return {
            "command": argv,
            "exit_code": result.returncode,
            "duration_seconds": round(time.monotonic() - before, 6),
            "stdout": bounded_output(result.stdout),
            "stderr": bounded_output(result.stderr),
            "started_at": started.isoformat(),
        }
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout.decode("utf-8", "replace") if isinstance(error.stdout, bytes) else error.stdout
        stderr = error.stderr.decode("utf-8", "replace") if isinstance(error.stderr, bytes) else error.stderr
        return {
            "command": argv,
            "exit_code": None,
            "duration_seconds": round(time.monotonic() - before, 6),
            "stdout": bounded_output(stdout),
            "stderr": bounded_output(stderr),
            "started_at": started.isoformat(),
            "timeout": timeout,
        }


def extract_pdf_text(path: Path) -> tuple[str | None, str | None]:
    if not shutil.which("pdftotext"):
        return None, "pdftotext unavailable"
    try:
        result = subprocess.run(
            ["pdftotext", "-nopgbrk", "-q", str(path), "-"],
            capture_output=True,
            timeout=180,
        )
    except subprocess.TimeoutExpired:
        return None, "pdftotext timed out after 180 seconds"
    except OSError as error:
        return None, f"pdftotext failed: {error}"
    if result.returncode != 0:
        return None, result.stderr.decode("utf-8", "replace")[-4000:]
    return result.stdout.decode("utf-8", "replace"), None


def pdf_page_count(path: Path) -> tuple[int | None, str | None]:
    if not shutil.which("pdfinfo"):
        return None, "pdfinfo unavailable"
    result = subprocess.run(["pdfinfo", str(path)], capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        return None, (result.stderr or "pdfinfo failed")[-4000:]
    match = re.search(r"^Pages:\s+(\d+)$", result.stdout, re.MULTILINE)
    return (int(match.group(1)), None) if match else (None, "pdfinfo omitted page count")


def package_page_count(parts: dict[str, bytes]) -> int | None:
    data = parts.get("docProps/app.xml")
    if not data:
        return None
    try:
        root = ElementTree.fromstring(data)
    except ElementTree.ParseError:
        return None
    node = next((item for item in root.iter() if item.tag.endswith("}Pages") or item.tag == "Pages"), None)
    return int(node.text) if node is not None and (node.text or "").isdigit() else None


def normalized_diagnostic(result: dict[str, Any]) -> str | None:
    text = result.get("stderr", "").strip()
    if not text:
        return None
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    errors = [line.strip() for line in text.splitlines() if line.lstrip().startswith("error:")]
    if errors:
        return " | ".join(errors)[:2000]
    text = re.sub(r"\s+", " ", text)
    return text[:2000]


def format_advisories(result: dict[str, Any]) -> list[str]:
    """Classify source/export mode mismatches that skew readiness metrics."""
    stderr = result.get("stderr") or ""
    return ["slide_shaped_docx"] if SLIDE_SHAPED_WARNING in stderr else []


def compile_diagnosis(result: dict[str, Any]) -> dict[str, Any]:
    """Turn subprocess mechanics into stable, actionable reason codes."""
    exit_code = result.get("exit_code")
    if exit_code is None:
        return {
            "code": "process_timeout",
            "timeout_seconds": result.get("timeout"),
            "summary": "compiler exceeded its configured timeout",
        }
    if exit_code < 0:
        signal = -exit_code
        names = {6: "SIGABRT", 9: "SIGKILL", 11: "SIGSEGV", 15: "SIGTERM"}
        return {
            "code": "process_killed" if signal == 9 else "process_signaled",
            "signal": signal,
            "signal_name": names.get(signal),
            "summary": (
                "compiler was killed; inspect memory pressure or unbounded work"
                if signal == 9
                else "compiler terminated from an operating-system signal"
            ),
        }
    if exit_code > 0:
        return {
            "code": "compiler_error",
            "exit_code": exit_code,
            "summary": normalized_diagnostic(result) or "compiler exited with an error",
        }
    return {"code": "ok"}


ERROR_CATALOG: dict[str, dict[str, str]] = {
    "DOCX-E001": {
        "name": "compile_timeout",
        "summary": "Typst did not finish before the compile timeout.",
        "next": "Reproduce the source alone; inspect unbounded layout work and memory use.",
    },
    "DOCX-E002": {
        "name": "compile_killed",
        "summary": "The operating system killed the Typst compiler.",
        "next": "Check memory pressure first, then reproduce with resource monitoring enabled.",
    },
    "DOCX-E003": {
        "name": "compile_signaled",
        "summary": "Typst terminated because of an operating-system signal.",
        "next": "Reproduce the source alone and inspect the signal, crash output, and backtrace.",
    },
    "DOCX-E004": {
        "name": "compile_error",
        "summary": "Typst reported a source or exporter error.",
        "next": "Read normalized_diagnostic and the artifact's docx.stderr.log.",
    },
    "DOCX-E101": {
        "name": "package_missing_or_invalid",
        "summary": "No DOCX package was produced, or the package is corrupt or contains invalid XML.",
        "next": "If the package is absent, inspect the compile diagnostic; otherwise inspect package.errors and unzip the named part.",
    },
    "DOCX-W102": {
        "name": "drawing_coordinate_outlier",
        "summary": "Drawing coordinates exceed the Word-safe diagnostic threshold.",
        "next": "Inspect diagnoses.docx_package.examples and bound or approximate the geometry.",
    },
    "DOCX-E201": {
        "name": "libreoffice_timeout",
        "summary": "LibreOffice did not finish converting the DOCX.",
        "next": "Reproduce this document alone; inspect layout loops, image sizes, and process memory.",
    },
    "DOCX-E202": {
        "name": "libreoffice_conversion_failed",
        "summary": "LibreOffice failed to produce a PDF from the DOCX.",
        "next": "Open the DOCX directly and inspect visual.reason plus the consumer logs.",
    },
    "DOCX-E203": {
        "name": "reference_rasterization_failed",
        "summary": "The reference PDF could not be rasterized for comparison.",
        "next": "Inspect visual.reason and run pdftoppm on the reference PDF alone.",
    },
    "DOCX-E204": {
        "name": "consumer_rasterization_failed",
        "summary": "The consumer-rendered PDF could not be rasterized for comparison.",
        "next": "Inspect visual.reason and run pdftoppm on the rendered PDF alone.",
    },
    "DOCX-W301": {
        "name": "visual_policy_miss",
        "summary": "The DOCX rendered successfully but missed the configured visual policy.",
        "next": "Compare the retained gold/docx page PNGs; rank by score and absolute page delta.",
    },
}


def canonical_error_code(value: str) -> str:
    """Accept the short code humans type while keeping one stored spelling."""
    code = value.strip().upper()
    if not code.startswith("DOCX-"):
        code = f"DOCX-{code}"
    if code not in ERROR_CATALOG:
        choices = ", ".join(ERROR_CATALOG)
        raise ValueError(f"unknown DOCX diagnostic code {value!r}; known codes: {choices}")
    return code


def triage_run(run: Path, requested_codes: list[str]) -> dict[str, Any]:
    """Group a completed corpus run by diagnostic code and retain evidence paths."""
    documents_path = run / "documents.jsonl"
    if not documents_path.is_file():
        raise ValueError(f"corpus run has no documents.jsonl: {run}")
    wanted = {canonical_error_code(code) for code in requested_codes}
    grouped: dict[str, list[dict[str, Any]]] = {code: [] for code in wanted}
    for line_number, line in enumerate(documents_path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"invalid JSON at {documents_path}:{line_number}: {error}") from error
        # Re-derive codes from retained structured evidence so newly added
        # catalog rules also work against an older completed run.
        for error in error_entries(record):
            code = error.get("code")
            if code not in ERROR_CATALOG or (wanted and code not in wanted):
                continue
            artifact = Path(record.get("artifacts", {}).get("directory", run / "artifacts" / record["id"]))
            grouped.setdefault(code, []).append({
                "id": record["id"],
                "primary_class": record.get("primary_class"),
                "stage": error.get("stage"),
                "detail": error.get("detail"),
                "artifact_directory": str(artifact),
                "docx_stderr": str(artifact / "docx.stderr.log"),
                "record": f"{documents_path}:{line_number}",
            })
    for code, documents in grouped.items():
        if code == "DOCX-W301":
            documents.sort(key=lambda item: (
                item["detail"].get("score") is None,
                item["detail"].get("score") or 0.0,
                -abs(item["detail"].get("page_delta") or 0),
                item["id"],
            ))
        else:
            documents.sort(key=lambda item: item["id"])
    return {
        "schema_version": 1,
        "run": str(run),
        "diagnostics": [
            {
                "code": code,
                **ERROR_CATALOG[code],
                "count": len(documents),
                "documents": documents,
            }
            for code, documents in sorted(grouped.items())
            if documents or code in wanted
        ],
    }


def print_diagnostic_reference(codes: list[str], as_json: bool) -> None:
    canonical = [canonical_error_code(code) for code in codes] if codes else list(ERROR_CATALOG)
    entries = [{"code": code, **ERROR_CATALOG[code]} for code in canonical]
    if as_json:
        print(json.dumps(entries, indent=2))
        return
    for entry in entries:
        print(f"{entry['code']} ({entry['name']}): {entry['summary']}")
        print(f"  Next: {entry['next']}")


def error_entries(record: dict[str, Any]) -> list[dict[str, Any]]:
    """Return stable codes while retaining stage-specific raw evidence."""
    found: list[tuple[str, str, Any]] = []
    compile_code = record.get("diagnoses", {}).get("docx_compile", {}).get("code")
    compile_codes = {
        "process_timeout": "DOCX-E001",
        "process_killed": "DOCX-E002",
        "process_signaled": "DOCX-E003",
        "compiler_error": "DOCX-E004",
    }
    if compile_code in compile_codes:
        found.append((compile_codes[compile_code], "compile", record["diagnoses"]["docx_compile"]))
    package = record.get("package", {})
    if not package.get("ok", False):
        package_errors = package.get("errors", [])
        package_missing = compile_code in compile_codes or any(
            "no package" in str(error).lower() for error in package_errors
        )
        detail = (
            {"kind": "missing", "errors": package_errors}
            if package_missing
            else {"kind": "invalid", "errors": package_errors}
        )
        found.append(("DOCX-E101", "package", detail))
    package_code = record.get("diagnoses", {}).get("docx_package", {}).get("code")
    if package_code == "drawing_coordinate_outlier":
        found.append(("DOCX-W102", "package", record["diagnoses"]["docx_package"]))

    visual = record.get("visual", {})
    if visual.get("status") in {"failed", "unavailable"}:
        reason = str(visual.get("reason", ""))
        if "LibreOffice" in reason and "timed out" in reason:
            code = "DOCX-E201"
        elif visual.get("stage") == "reference_pdf_rasterization":
            code = "DOCX-E203"
        elif visual.get("stage") == "consumer_pdf_rasterization":
            code = "DOCX-E204"
        else:
            code = "DOCX-E202"
        found.append((code, "visual", {"status": visual.get("status"), "reason": reason}))
    elif visual.get("status") == "ok" and visual.get("ok") is False:
        found.append((
            "DOCX-W301",
            "visual",
            {
                key: visual.get(key)
                for key in ("score", "gold_pages", "docx_pages", "page_delta")
            },
        ))

    return [
        {"code": code, "stage": stage, **ERROR_CATALOG[code], "detail": detail}
        for code, stage, detail in found
    ]


def docx_diagnosis(parts: dict[str, bytes]) -> dict[str, Any]:
    """Find Word-fragile OOXML constructs that package validation permits."""
    outliers: list[dict[str, Any]] = []
    maximum = 0
    for part, data in parts.items():
        if not part.startswith("word/") or not part.endswith(".xml"):
            continue
        for match in DRAWING_COORDINATE.finditer(data):
            raw = match.group("value") or match.group("offset")
            value = int(raw)
            maximum = max(maximum, abs(value))
            if abs(value) <= SUSPICIOUS_DRAWING_COORDINATE_EMU:
                continue
            if len(outliers) < 20:
                outliers.append(
                    {
                        "part": part,
                        "attribute": (
                            match.group("attribute").decode()
                            if match.group("attribute")
                            else "wp:posOffset"
                        ),
                        "value_emu": value,
                    }
                )
    return {
        "code": "drawing_coordinate_outlier" if outliers else "ok",
        "threshold_emu": SUSPICIOUS_DRAWING_COORDINATE_EMU,
        "maximum_absolute_coordinate_emu": maximum,
        "examples": outliers,
        "summary": (
            "extreme DrawingML coordinates may make Microsoft Word reject the package"
            if outliers
            else None
        ),
    }


def word_evidence(artifact: Path) -> dict[str, Any]:
    """Load a durable result produced by the real Microsoft Word probe lane."""
    path = artifact / "word-consumer.json"
    if not path.is_file():
        return {"status": "not_run", "repair_reported": None}
    try:
        evidence = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return {"status": "failed", "reason": f"invalid Word evidence: {error}"}
    if evidence.get("status") not in {"ok", "failed", "unavailable"}:
        return {"status": "failed", "reason": "invalid Word evidence status"}
    evidence["evidence_file"] = str(path)
    return evidence


def parse_fidelity(parts: dict[str, bytes]) -> dict[str, Any]:
    data = parts.get("customXml/typstFidelity.xml")
    if not data:
        return {"status": "missing", "counts": {}, "decisions": []}
    try:
        root = ElementTree.fromstring(data)
    except ElementTree.ParseError as error:
        return {"status": "invalid", "error": str(error), "counts": {}, "decisions": []}
    counts_node = root.find("typst:counts", NS)
    counts = dict(counts_node.attrib) if counts_node is not None else {}
    decisions = [dict(node.attrib) for node in root.findall("typst:decisions/typst:decision", NS)]
    grouped = Counter(decision.get("representation", "Unknown") for decision in decisions)
    losses = {
        key: sum(
            int(decision.get("occurrences", "0"))
            for decision in decisions
            if decision.get(key) == "true"
        )
        for key in ("visual", "semantic", "editability", "dynamic", "accessibility", "portability")
    }
    return {
        "status": "ok",
        "enrollment": root.attrib.get("enrollment"),
        "snapshot_id": root.attrib.get("snapshotId"),
        "counts": counts,
        "decisions_by_representation": dict(sorted(grouped.items())),
        "loss_occurrences": losses,
        "decisions": decisions,
    }


def classify(record: dict[str, Any]) -> tuple[str, list[str], list[str]]:
    reasons: list[str] = []
    unverified: list[str] = []
    docx_compile = record["compile"]["docx"]
    if docx_compile["exit_code"] is None:
        return "unverified", [], ["docx_compile_timeout"]
    if docx_compile["exit_code"] != 0:
        return "export_error", ["docx_compile_failed"], []
    if not record["package"]["ok"]:
        return "package_error", ["invalid_ooxml_package"], []

    reasons.extend(record.get("format_advisories", []))

    fidelity = record["fidelity"]
    if fidelity["status"] != "ok":
        reasons.append("fidelity_manifest_missing_or_invalid")
    semantic_losses = fidelity.get("loss_occurrences", {}).get("semantic", 0)
    dropped_text = sum(
        int(item.get("affectedTextChars", "0"))
        for item in fidelity.get("decisions", [])
        if item.get("representation") == "Drop"
    )
    coverage = record["semantic"].get("text_coverage")
    if dropped_text > 0:
        if dropped_text:
            reasons.append("reported_text_drop")
        return "content_loss", reasons, []
    if coverage is not None and coverage < 0.5:
        reasons.append("text_coverage_below_threshold")
    if semantic_losses:
        reasons.append("reported_semantic_loss")

    if not record["license"]["verified"]:
        unverified.append("license_unverified")
    if record["compile"]["pdf"]["exit_code"] != 0:
        unverified.append("reference_pdf_unavailable")
    if coverage is None:
        unverified.append("text_coverage_unavailable")
    if record["missing_fonts"]:
        unverified.append("fonts_unavailable")
    for consumer in ("word", "libreoffice"):
        state = record["consumers"][consumer].get("status")
        if state == "failed":
            return "consumer_error", [f"{consumer}_consumer_failed"], unverified
        if state != "ok":
            unverified.append(f"{consumer}_consumer_unavailable")
    if record["visual"].get("status") != "ok":
        unverified.append("visual_comparison_unavailable")
    if record["round_trip"].get("status") != "ok":
        unverified.append("round_trip_unavailable")
    if unverified:
        return "unverified", reasons, sorted(set(unverified))

    visual_ok = bool(record["visual"].get("ok"))
    fallbacks = int(fidelity.get("counts", {}).get("raster", "0")) + int(
        fidelity.get("counts", {}).get("nativeWithFallback", "0")
    )
    if fallbacks:
        return (
            "fallback_visual" if visual_ok else "fallback_degraded",
            reasons + ["fidelity_fallback_present"],
            [],
        )
    return ("native_good" if visual_ok else "native_degraded", reasons, [])


def roundtrip_probe(
    source: Path,
    root: Path,
    artifact: Path,
    args: argparse.Namespace,
) -> dict[str, Any]:
    review_docx = artifact / "review.docx"
    state_path = artifact / "review.typst-review.json"
    report_path = artifact / "review-report.json"
    compile_result = command(
        [
            args.typst,
            "compile",
            "--root",
            str(root),
            "--format",
            "docx",
            str(source),
            str(review_docx),
            f"--docx-review-state={state_path}",
        ],
        timeout=args.timeout,
    )
    (artifact / "review.stderr.log").write_text(
        compile_result.get("stderr", ""), encoding="utf-8"
    )
    if (
        compile_result.get("exit_code") != 0
        and "no source regions eligible for DOCX review"
        in compile_result.get("stderr", "")
    ):
        return {
            "status": "ok",
            "outcome": "not_enrolled",
            "stage": "complete",
            "reason": "no uniquely realized source-backed regions",
            "compile": compile_result,
            "enrollment": {"regions": 0, "files": 0, "stories": 0, "by_kind": {}},
            "report": {
                "statuses": {},
                "comments": 0,
                "baseline_stories": 0,
                "word_stories": 0,
            },
            "artifacts": {
                "docx": str(review_docx),
                "state": str(state_path),
                "report": str(report_path),
            },
        }
    if compile_result.get("exit_code") != 0 or not state_path.is_file():
        return {
            "status": "failed" if compile_result.get("exit_code") is not None else "unavailable",
            "stage": "review_export",
            "reason": normalized_diagnostic(compile_result) or "review export produced no state",
            "compile": compile_result,
            "artifacts": {
                "docx": str(review_docx),
                "state": str(state_path),
                "report": str(report_path),
            },
        }

    try:
        state = json.loads(state_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return {
            "status": "failed",
            "stage": "state_parse",
            "reason": str(error),
            "compile": compile_result,
        }

    review_result = command(
        [
            args.typst,
            "review",
            str(review_docx),
            "--state",
            str(state_path),
            "--root",
            str(root),
            "--report",
            str(report_path),
        ],
        timeout=args.timeout,
    )
    if review_result.get("exit_code") != 0 or not report_path.is_file():
        return {
            "status": "failed" if review_result.get("exit_code") is not None else "unavailable",
            "stage": "unchanged_review",
            "reason": normalized_diagnostic(review_result) or "review produced no report",
            "compile": compile_result,
            "review": review_result,
            "enrollment": {
                "regions": len(state.get("regions", [])),
                "files": len(state.get("files", [])),
                "stories": len(state.get("stories", [])),
                "by_kind": dict(Counter(region.get("kind", "unknown") for region in state.get("regions", []))),
            },
            "artifacts": {
                "docx": str(review_docx),
                "state": str(state_path),
                "report": str(report_path),
            },
        }

    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return {
            "status": "failed",
            "stage": "report_parse",
            "reason": str(error),
            "compile": compile_result,
            "review": review_result,
        }
    statuses = Counter(
        status if isinstance(status := region.get("status"), str) else next(iter(status), "unknown")
        for region in report.get("regions", [])
    )
    unchanged = statuses.get("unchanged", 0)
    region_count = len(state.get("regions", []))
    return {
        "status": "ok" if unchanged == region_count else "failed",
        "outcome": "validated" if unchanged == region_count else "mismatch",
        "stage": "complete",
        "reason": None if unchanged == region_count else "unchanged review did not preserve every enrolled region",
        "compile": compile_result,
        "review": review_result,
        "enrollment": {
            "regions": region_count,
            "files": len(state.get("files", [])),
            "stories": len(state.get("stories", [])),
            "by_kind": dict(sorted(Counter(region.get("kind", "unknown") for region in state.get("regions", [])).items())),
        },
        "report": {
            "statuses": dict(sorted(statuses.items())),
            "comments": len(report.get("comments", [])),
            "baseline_stories": len(report.get("baseline_stories", [])),
            "word_stories": len(report.get("word_stories", [])),
        },
        "artifacts": {
            "docx": str(review_docx),
            "state": str(state_path),
            "report": str(report_path),
        },
    }


def validate_document(
    frozen: dict[str, Any], args: argparse.Namespace, corpus_root: Path
) -> dict[str, Any]:
    artifact = args.out / "artifacts" / safe_component(frozen["id"])
    artifact.mkdir(parents=True, exist_ok=True)
    result_file = artifact / "result.json"
    if args.resume and result_file.is_file():
        record = json.loads(result_file.read_text(encoding="utf-8"))
        # A resumed record normally reuses its already compiled DOCX. Preserve
        # the source fingerprint that produced that artifact instead of falsely
        # relabeling it with whatever dirty tree happens to invoke the retry.
        record.setdefault("exporter_state", args.exporter_state)
        record.setdefault("exporter_binary_sha256", args.exporter_binary_sha256)
        record["consumers"]["word"] = word_evidence(artifact)
        docx_path = Path(record["artifacts"]["docx"])
        _, diagnostic_parts = (
            validator.package_check(docx_path) if docx_path.is_file() else ({}, {})
        )
        record["diagnoses"] = {
            "docx_compile": compile_diagnosis(record["compile"]["docx"]),
            "docx_package": docx_diagnosis(diagnostic_parts),
        }
        record["format_advisories"] = format_advisories(record["compile"]["docx"])
        if args.refresh_semantic:
            pdf_artifact = record.get("artifacts", {}).get("pdf")
            pdf_path = Path(pdf_artifact) if pdf_artifact else None
            docx_text = validator.word_text(diagnostic_parts)
            pdf_text, pdf_text_error = (
                extract_pdf_text(pdf_path)
                if pdf_path is not None and pdf_path.is_file()
                else (None, "PDF unavailable")
            )
            record["semantic"] = {
                "docx_word_count": len(validator.TEXT_RE.findall(docx_text)),
                "pdf_word_count": len(validator.TEXT_RE.findall(pdf_text or "")),
                "text_coverage": (
                    validator.token_jaccard(pdf_text, docx_text)
                    if pdf_text is not None
                    else None
                ),
                "pdf_text_error": pdf_text_error,
            }
        libreoffice_status = record["consumers"]["libreoffice"].get("status")
        should_run_libreoffice = libreoffice_status == "not_run" or (
            args.retry_libreoffice_failures and libreoffice_status in {"failed", "unavailable"}
        )
        if args.libreoffice and should_run_libreoffice:
            docx = Path(record["artifacts"]["docx"])
            pdf = Path(record["artifacts"]["pdf"])
            if docx.is_file() and pdf.is_file():
                visual = validator.visual_check(pdf, docx, artifact)
                if visual.get("status") == "ok":
                    visual["page_delta"] = abs(visual["gold_pages"] - visual["docx_pages"])
                    visual["ok"] = (
                        visual["score"] >= args.minimum_visual_score
                        and visual["page_delta"] <= args.max_page_delta
                    )
                    record["consumers"]["libreoffice"] = {
                        "status": "ok",
                        "repair_reported": False,
                        "rendered": True,
                    }
                    record["pages"]["libreoffice_render"] = visual["docx_pages"]
                else:
                    record["consumers"]["libreoffice"] = {
                        "status": visual.get("status", "failed"),
                        "reason": visual.get("reason"),
                    }
                record["visual"] = visual
        roundtrip_status = record.get("round_trip", {}).get("status", "not_probed")
        should_run_roundtrip = roundtrip_status == "not_probed" or (
            args.retry_roundtrip_failures and roundtrip_status in {"failed", "unavailable"}
        )
        if args.roundtrip and should_run_roundtrip:
            root = corpus_root / frozen["root"]
            source = corpus_root / frozen["entry"]
            record["round_trip"] = roundtrip_probe(source, root, artifact, args)
        primary, reasons, unverified = classify(record)
        record["primary_class"] = primary
        record["reason_codes"] = reasons
        record["unverified_reasons"] = unverified
        record["errors"] = error_entries(record)
        result_file.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
        return record

    root = corpus_root / frozen["root"]
    source = corpus_root / frozen["entry"]
    docx = artifact / "document.docx"
    pdf = artifact / "reference.pdf"
    docx_compile = command(
        [args.typst, "compile", "--root", str(root), "--format", "docx", str(source), str(docx)],
        timeout=args.timeout,
    )
    pdf_compile = command(
        [args.typst, "compile", "--root", str(root), "--format", "pdf", str(source), str(pdf)],
        timeout=args.timeout,
    )
    (artifact / "docx.stderr.log").write_text(docx_compile["stderr"], encoding="utf-8")
    (artifact / "pdf.stderr.log").write_text(pdf_compile["stderr"], encoding="utf-8")

    package, parts = validator.package_check(docx) if docx.is_file() else (
        {"ok": False, "errors": ["DOCX compilation produced no package"], "parts": 0},
        {},
    )
    docx_text = validator.word_text(parts)
    pdf_text, pdf_text_error = extract_pdf_text(pdf) if pdf.is_file() else (None, "PDF unavailable")
    fidelity = parse_fidelity(parts)
    metrics = validator.editability_metrics(parts)
    sdt_count = metrics["content_controls"]
    coverage = validator.token_jaccard(pdf_text, docx_text) if pdf_text is not None else None
    missing_fonts = sorted(set(FONT_WARNING.findall(docx_compile["stderr"] + pdf_compile["stderr"])))
    pdf_pages, pdf_page_error = pdf_page_count(pdf) if pdf.is_file() else (None, "PDF unavailable")

    libreoffice: dict[str, Any] = {"status": "not_run"}
    visual: dict[str, Any] = {"status": "not_run"}
    if args.libreoffice and docx.is_file() and pdf.is_file():
        visual = validator.visual_check(pdf, docx, artifact)
        if visual.get("status") == "ok":
            visual["page_delta"] = abs(visual["gold_pages"] - visual["docx_pages"])
            visual["ok"] = visual["score"] >= args.minimum_visual_score and visual["page_delta"] <= args.max_page_delta
            libreoffice = {"status": "ok", "repair_reported": False, "rendered": True}
        else:
            libreoffice = {"status": visual.get("status", "failed"), "reason": visual.get("reason")}

    record: dict[str, Any] = {
        **frozen,
        "exporter_revision": validator.git_revision(),
        "exporter_state": args.exporter_state,
        "exporter_binary_sha256": args.exporter_binary_sha256,
        "compile": {"docx": docx_compile, "pdf": pdf_compile},
        "normalized_diagnostic": normalized_diagnostic(docx_compile),
        "format_advisories": format_advisories(docx_compile),
        "diagnoses": {
            "docx_compile": compile_diagnosis(docx_compile),
            "docx_package": docx_diagnosis(parts),
        },
        "package": package,
        "semantic": {
            "docx_word_count": len(validator.TEXT_RE.findall(docx_text)),
            "pdf_word_count": len(validator.TEXT_RE.findall(pdf_text or "")),
            "text_coverage": coverage,
            "pdf_text_error": pdf_text_error,
        },
        "pages": {
            "pdf": pdf_pages,
            "pdf_error": pdf_page_error,
            # Extended properties are exporter metadata, not a consumer layout
            # result. Keep the value for diagnostics but never substitute it
            # for Word/LibreOffice page counts.
            "docx_metadata": package_page_count(parts),
            "word_render": None,
            "libreoffice_render": visual.get("docx_pages") if visual.get("status") == "ok" else None,
        },
        "editability": metrics,
        "fidelity": fidelity,
        "consumers": {
            "word": word_evidence(artifact),
            "libreoffice": libreoffice,
        },
        "visual": visual,
        "missing_fonts": missing_fonts,
        "round_trip": roundtrip_probe(source, root, artifact, args) if args.roundtrip else {
            "enrollment": fidelity.get("enrollment"),
            "content_controls": sdt_count,
            "unsupported_or_conflicting_regions": None,
            "status": "not_probed",
        },
        "artifacts": {"directory": str(artifact), "docx": str(docx), "pdf": str(pdf)},
    }
    primary, reasons, unverified = classify(record)
    record["primary_class"] = primary
    record["reason_codes"] = reasons
    record["unverified_reasons"] = unverified
    record["errors"] = error_entries(record)
    result_file.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
    return record


def write_reports(records: list[dict[str, Any]], out: Path) -> dict[str, Any]:
    with (out / "documents.jsonl").open("w", encoding="utf-8") as stream:
        for record in records:
            stream.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
    classes = Counter(record["primary_class"] for record in records)
    reasons = Counter(reason for record in records for reason in record["reason_codes"])
    unverified = Counter(reason for record in records for reason in record["unverified_reasons"])
    error_counts = Counter(
        error["code"] for record in records for error in record.get("errors", [])
    )
    total = len(records)
    consumer_statuses = {
        consumer: dict(sorted(Counter(
            record["consumers"][consumer].get("status", "missing") for record in records
        ).items()))
        for consumer in ("word", "libreoffice")
    }
    visual_ok = [record for record in records if record["visual"].get("status") == "ok"]
    slide_shaped = [
        record
        for record in records
        if "slide_shaped_docx" in record.get("format_advisories", [])
    ]
    slide_ids = {record["id"] for record in slide_shaped}
    non_slide_visual_ok = [record for record in visual_ok if record["id"] not in slide_ids]
    slide_visual_ok = [record for record in visual_ok if record["id"] in slide_ids]
    advisory_counts = Counter(
        advisory for record in records for advisory in record.get("format_advisories", [])
    )
    roundtrip_statuses = Counter(
        record.get("round_trip", {}).get("status", "missing") for record in records
    )
    roundtrip_kinds: Counter[str] = Counter()
    roundtrip_regions = 0
    roundtrip_files = 0
    roundtrip_stories = 0
    roundtrip_enrolled_documents = 0
    for record in records:
        enrollment = record.get("round_trip", {}).get("enrollment", {})
        if not isinstance(enrollment, dict):
            continue
        regions = int(enrollment.get("regions", 0))
        roundtrip_regions += regions
        roundtrip_files += int(enrollment.get("files", 0))
        roundtrip_stories += int(enrollment.get("stories", 0))
        roundtrip_enrolled_documents += regions > 0
        roundtrip_kinds.update(enrollment.get("by_kind", {}))
    fidelity_counts: Counter[str] = Counter()
    editability_totals: Counter[str] = Counter()
    for record in records:
        fidelity_counts.update({
            key: int(value)
            for key, value in record.get("fidelity", {}).get("counts", {}).items()
            if str(value).isdigit()
        })
        editability_totals.update(record.get("editability", {}))
    summary = {
        "schema_version": 1,
        "total": total,
        "primary_classes": dict(sorted(classes.items())),
        "primary_class_rates": {key: {"count": value, "denominator": total, "rate": value / total if total else 0.0} for key, value in sorted(classes.items())},
        "reason_codes": dict(reasons.most_common()),
        "unverified_reasons": dict(unverified.most_common()),
        "format_advisories": dict(advisory_counts.most_common()),
        "error_codes": {
            code: {"count": count, **ERROR_CATALOG[code]}
            for code, count in error_counts.most_common()
        },
        "evidence": {
            "licenses_verified": sum(record.get("license", {}).get("verified", False) for record in records),
            "package_ok": sum(record.get("package", {}).get("ok", False) for record in records),
            "consumer_statuses": consumer_statuses,
            "semantic_text_coverage_available": sum(record.get("semantic", {}).get("text_coverage") is not None for record in records),
            "visual": {
                "status_ok": len(visual_ok),
                "policy_pass": sum(bool(record["visual"].get("ok")) for record in visual_ok),
                "exact_page_count": sum(record["visual"].get("page_delta") == 0 for record in visual_ok),
                "page_delta_over_one": sum((record["visual"].get("page_delta") or 0) > 1 for record in visual_ok),
                "non_slide": {
                    "status_ok": len(non_slide_visual_ok),
                    "policy_pass": sum(
                        bool(record["visual"].get("ok")) for record in non_slide_visual_ok
                    ),
                    "exact_page_count": sum(
                        record["visual"].get("page_delta") == 0
                        for record in non_slide_visual_ok
                    ),
                    "page_delta_over_one": sum(
                        (record["visual"].get("page_delta") or 0) > 1
                        for record in non_slide_visual_ok
                    ),
                },
                "slide_shaped_docx": {
                    "documents": len(slide_shaped),
                    "status_ok": len(slide_visual_ok),
                    "policy_pass": sum(
                        bool(record["visual"].get("ok")) for record in slide_visual_ok
                    ),
                },
            },
            "editability_totals": dict(sorted(editability_totals.items())),
            "fidelity_counts": dict(sorted(fidelity_counts.items())),
            "round_trip": {
                "statuses": dict(sorted(roundtrip_statuses.items())),
                "enrolled_documents": roundtrip_enrolled_documents,
                "regions": roundtrip_regions,
                "files": roundtrip_files,
                "stories": roundtrip_stories,
                "by_kind": dict(sorted(roundtrip_kinds.items())),
            },
        },
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    lines = ["# DOCX public corpus results", "", f"Denominator: **{total} documents**", "", "## Primary classes", ""]
    lines.extend(f"- `{key}`: {value}/{total} ({value / total:.2%})" for key, value in sorted(classes.items()))
    lines.extend(["", "## Top reason codes", ""])
    lines.extend(f"- `{key}`: {value}" for key, value in reasons.most_common(20))
    lines.extend(["", "## Unverified evidence", ""])
    lines.extend(f"- `{key}`: {value}" for key, value in unverified.most_common())
    lines.extend(["", "## Format advisories", ""])
    if advisory_counts:
        lines.extend(f"- `{key}`: {value}" for key, value in advisory_counts.most_common())
    else:
        lines.append("- None.")
    lines.extend(["", "## Error-code triage", ""])
    if error_counts:
        lines.extend(
            f"- `{code}` ({count}): {ERROR_CATALOG[code]['summary']} Next: {ERROR_CATALOG[code]['next']}"
            for code, count in error_counts.most_common()
        )
    else:
        lines.append("- No coded errors or warnings.")
    lines.extend(["", "## Evidence lanes", ""])
    lines.append(f"- Package-valid documents: {summary['evidence']['package_ok']}/{total}")
    lines.append(f"- License-verified documents: {summary['evidence']['licenses_verified']}/{total}")
    for consumer, statuses in consumer_statuses.items():
        lines.append(f"- `{consumer}` statuses: {json.dumps(statuses, sort_keys=True)}")
    lines.append(
        f"- Visual policy pass: {summary['evidence']['visual']['policy_pass']}/{summary['evidence']['visual']['status_ok']} rendered"
    )
    non_slide = summary["evidence"]["visual"]["non_slide"]
    slides = summary["evidence"]["visual"]["slide_shaped_docx"]
    lines.append(
        f"- Non-slide visual policy pass: {non_slide['policy_pass']}/{non_slide['status_ok']} rendered"
    )
    lines.append(
        f"- Slide-shaped DOCX (informational): {slides['policy_pass']}/{slides['status_ok']} rendered across {slides['documents']} documents"
    )
    lines.extend(["", "## Round-trip evidence", ""])
    round_trip = summary["evidence"]["round_trip"]
    lines.append(f"- Statuses: `{json.dumps(round_trip['statuses'], sort_keys=True)}`")
    lines.append(f"- Enrolled documents: {round_trip['enrolled_documents']}/{total}")
    lines.append(f"- Regions: {round_trip['regions']} across {round_trip['files']} source files and {round_trip['stories']} Word stories")
    lines.append(f"- Region kinds: `{json.dumps(round_trip['by_kind'], sort_keys=True)}`")
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--typst")
    parser.add_argument("--frozen", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument(
        "--refresh-semantic",
        action="store_true",
        help="recompute semantic text evidence from retained DOCX/PDF artifacts",
    )
    parser.add_argument("--libreoffice", action="store_true")
    parser.add_argument("--retry-libreoffice-failures", action="store_true")
    parser.add_argument("--roundtrip", action="store_true")
    parser.add_argument("--retry-roundtrip-failures", action="store_true")
    parser.add_argument("--filter", action="append", default=[])
    parser.add_argument("--minimum-visual-score", type=float, default=0.45)
    parser.add_argument("--max-page-delta", type=int, default=1)
    parser.add_argument(
        "--explain-code",
        action="append",
        default=[],
        metavar="CODE",
        help="explain a diagnostic such as E201 or DOCX-W102 without running the corpus",
    )
    parser.add_argument(
        "--list-error-codes",
        action="store_true",
        help="list every stable DOCX corpus diagnostic without running the corpus",
    )
    parser.add_argument(
        "--triage-run",
        type=Path,
        help="group a completed run's documents and evidence paths by diagnostic code",
    )
    parser.add_argument(
        "--code",
        action="append",
        default=[],
        help="limit --triage-run to a code such as E201 (repeatable)",
    )
    parser.add_argument("--json", action="store_true", help="emit diagnostic helper output as JSON")
    args = parser.parse_args()
    try:
        if args.explain_code or args.list_error_codes:
            print_diagnostic_reference(args.explain_code, args.json)
            return 0
        if args.triage_run:
            result = triage_run(args.triage_run.resolve(), args.code)
            if args.json:
                print(json.dumps(result, indent=2))
            else:
                for diagnostic in result["diagnostics"]:
                    print(f"{diagnostic['code']} ({diagnostic['count']}): {diagnostic['summary']}")
                    print(f"  Next: {diagnostic['next']}")
                    for document in diagnostic["documents"]:
                        print(
                            f"  - {document['id']} [{document['stage']}]: "
                            f"{document['artifact_directory']}"
                        )
            return 0
    except ValueError as error:
        parser.error(str(error))
    missing = [name for name in ("typst", "frozen", "out") if getattr(args, name) is None]
    if missing:
        parser.error(
            "a corpus run requires "
            + ", ".join(f"--{name}" for name in missing)
            + "; use --explain-code, --list-error-codes, or --triage-run for diagnostics only"
        )
    args.typst = str(Path(args.typst).resolve())
    args.frozen = args.frozen.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    args.exporter_state = exporter_state()
    args.exporter_binary_sha256 = validator.sha256(Path(args.typst))
    frozen_metadata = json.loads((args.frozen.parent / "metadata.json").read_text())
    corpus_root = Path(frozen_metadata["corpus_root"])
    all_documents = [
        json.loads(line) for line in args.frozen.read_text(encoding="utf-8").splitlines()
    ]
    documents = all_documents
    if args.filter:
        documents = [
            item for item in documents if any(pattern in item["id"] for pattern in args.filter)
        ]
    if args.limit:
        documents = documents[: args.limit]
    generated_at = datetime.now(timezone.utc).isoformat()
    invocation_metadata = {
        "schema_version": 1,
        "generated_at": generated_at,
        "frozen_documents": str(args.frozen),
        "frozen_documents_sha256": validator.sha256(args.frozen),
        "exporter_revision": validator.git_revision(),
        "exporter_state": args.exporter_state,
        "typst_version": validator.command_version([args.typst, "--version"]),
        "typst_sha256": args.exporter_binary_sha256,
        "platform": platform.platform(),
        "python": sys.version,
        "jobs": args.jobs,
        "timeout": args.timeout,
        "libreoffice_requested": args.libreoffice,
        "roundtrip_requested": args.roundtrip,
        "tools": {
            "soffice": validator.command_version([shutil.which("soffice") or "soffice", "--version"]),
            "pdftotext": validator.command_version(["pdftotext", "-v"]),
            "pdftoppm": validator.command_version(["pdftoppm", "-v"]),
        },
    }
    metadata_path = args.out / "metadata.json"
    if args.resume and metadata_path.is_file():
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        # Keep the original run identity. A resume may only enrich or retry
        # existing artifacts, and must not make them appear freshly compiled by
        # the current source tree or executable.
        metadata.setdefault("typst_sha256", args.exporter_binary_sha256)
        metadata["last_resume"] = {
            "generated_at": generated_at,
            "exporter_revision": invocation_metadata["exporter_revision"],
            "exporter_state": args.exporter_state,
            "typst_version": invocation_metadata["typst_version"],
            "typst_sha256": args.exporter_binary_sha256,
            "jobs": args.jobs,
            "filters": args.filter,
            "refresh_semantic": args.refresh_semantic,
            "retry_libreoffice_failures": args.retry_libreoffice_failures,
            "retry_roundtrip_failures": args.retry_roundtrip_failures,
        }
    else:
        metadata = invocation_metadata
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n")

    records_by_id: dict[str, dict[str, Any]] = {}
    with ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(validate_document, item, args, corpus_root): item for item in documents}
        for completed, future in enumerate(as_completed(futures), 1):
            record = future.result()
            records_by_id[record["id"]] = record
            if completed % 25 == 0 or record["primary_class"] not in {"unverified", "native_good", "fallback_visual"}:
                print(f'[{completed}/{len(documents)}] {record["primary_class"]:18} {record["id"]}', file=sys.stderr, flush=True)
    report_documents = documents
    if args.resume and args.filter:
        # A filtered retry updates only the selected artifacts, but its summary
        # remains an authority-wide report. Otherwise every serial retry would
        # replace the run's aggregate evidence with a misleading tiny subset.
        report_documents = []
        for item in all_documents:
            if item["id"] in records_by_id:
                report_documents.append(item)
                continue
            result_file = args.out / "artifacts" / safe_component(item["id"]) / "result.json"
            if result_file.is_file():
                records_by_id[item["id"]] = json.loads(result_file.read_text(encoding="utf-8"))
                report_documents.append(item)
    records = [records_by_id[item["id"]] for item in report_documents]
    summary = write_reports(records, args.out)
    print(json.dumps(summary, sort_keys=True))
    return 1 if summary["primary_classes"].get("export_error", 0) or summary["primary_classes"].get("package_error", 0) else 0


if __name__ == "__main__":
    raise SystemExit(main())

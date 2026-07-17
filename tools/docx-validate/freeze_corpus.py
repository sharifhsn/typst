#!/usr/bin/env python3
"""Freeze a typst-corpus TSV into reproducible per-document JSON records."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tomllib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


IGNORED_PARTS = {".git", "target", "node_modules", "__pycache__"}
LICENSE_NAMES = ("LICENSE", "LICENSE.md", "LICENSE.txt", "COPYING", "COPYING.md")
FORMAT_OVERRIDES_PATH = Path(__file__).resolve().parent / "format-overrides.json"


def format_overrides() -> dict[str, str]:
    if not FORMAT_OVERRIDES_PATH.is_file():
        return {}
    values = json.loads(FORMAT_OVERRIDES_PATH.read_text(encoding="utf-8"))
    invalid = {
        key: value for key, value in values.items() if value not in {"docx", "pptx"}
    }
    if invalid:
        raise ValueError(f"invalid Office format overrides: {invalid}")
    return values


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_sha256(root: Path) -> str:
    digest = hashlib.sha256()
    files = sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and not IGNORED_PARTS.intersection(path.relative_to(root).parts)
    )
    for path in files:
        relative = path.relative_to(root).as_posix().encode()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(bytes.fromhex(sha256(path)))
    return digest.hexdigest()


def git_facts(path: Path) -> dict[str, Any]:
    def git(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", "-C", str(path), *args], text=True, capture_output=True, check=False
        )

    top = git("rev-parse", "--show-toplevel")
    if top.returncode != 0:
        return {"revision": None, "repository_root": None, "dirty": None}
    revision = git("rev-parse", "HEAD")
    status = git("status", "--porcelain", "--untracked-files=no")
    return {
        "revision": revision.stdout.strip() if revision.returncode == 0 else None,
        "repository_root": str(Path(top.stdout.strip()).resolve()),
        "dirty": bool(status.stdout.strip()) if status.returncode == 0 else None,
    }


def repository_candidate(corpus: Path, root: Path) -> Path:
    relative = root.relative_to(corpus)
    if relative.parts[0] == "registry":
        return corpus / "registry"
    if len(relative.parts) >= 2 and relative.parts[0] in {"repos", "local-private"}:
        return corpus.joinpath(*relative.parts[:2])
    return root


def git_tree_id(repository: Path, root: Path, revision: str | None) -> str | None:
    if not revision:
        return None
    relative = root.relative_to(repository)
    object_name = revision if not relative.parts else f"{revision}:{relative.as_posix()}"
    result = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", object_name],
        text=True,
        capture_output=True,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def license_facts(root: Path, entry: Path) -> dict[str, Any]:
    candidates = [entry.parent, *entry.parents[:6]]
    package_file = next(
        (parent / "typst.toml" for parent in candidates if (parent / "typst.toml").is_file()),
        None,
    )
    declared = None
    if package_file:
        try:
            package = tomllib.loads(package_file.read_text(encoding="utf-8"))["package"]
            declared = package.get("license")
        except (KeyError, OSError, tomllib.TOMLDecodeError):
            pass

    license_file = next(
        (
            parent / name
            for parent in [root, *root.parents[:3]]
            for name in LICENSE_NAMES
            if (parent / name).is_file()
        ),
        None,
    )
    return {
        "declared": declared,
        "file": str(license_file) if license_file else None,
        "file_sha256": sha256(license_file) if license_file else None,
        "verified": bool(declared or license_file),
    }


def load_rows(corpus: Path, manifest: Path) -> list[dict[str, str]]:
    seen: set[tuple[str, str, str, str]] = set()
    rows = []
    for line_number, line in enumerate(manifest.read_text(encoding="utf-8").splitlines(), 1):
        fields = line.split("\t")
        if len(fields) < 7 or fields[0] != "yes":
            continue
        category, name, root_text, entry_text = fields[3:7]
        key = (category, name, root_text, entry_text)
        if key in seen:
            continue
        seen.add(key)
        rows.append(
            {
                "category": category,
                "name": name,
                "root": root_text,
                "entry": entry_text,
                "line": str(line_number),
            }
        )
    return rows


def document_id(row: dict[str, str]) -> str:
    """Return a readable ID that remains unique when names repeat."""
    entry_hash = hashlib.sha256(row["entry"].encode()).hexdigest()[:12]
    return f'{row["category"]}/{row["name"]}--{entry_hash}'


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    corpus = args.corpus.resolve()
    manifest = (args.manifest or corpus / "_meta/manifest.tsv").resolve()
    rows = load_rows(corpus, manifest)
    args.out.mkdir(parents=True, exist_ok=True)

    root_cache: dict[Path, dict[str, Any]] = {}
    repository_cache: dict[Path, dict[str, Any]] = {}
    target_overrides = format_overrides()
    documents = []
    for row in rows:
        root = (corpus / row["root"]).resolve()
        entry = (corpus / row["entry"]).resolve()
        if root not in root_cache:
            repository = repository_candidate(corpus, root)
            git = repository_cache.setdefault(repository, git_facts(repository))
            tracked_and_clean = bool(git["revision"] and git["dirty"] is False)
            root_cache[root] = {
                "tree_sha256": None if tracked_and_clean else tree_sha256(root),
                "git_tree_id": git_tree_id(repository, root, git["revision"]),
                "git": git,
                "license": license_facts(root, entry),
            }
        facts = root_cache[root]
        documents.append(
            {
                "id": document_id(row),
                "category": row["category"],
                "target_format": (
                    target_overrides.get(row["entry"])
                    or ("pptx" if row["category"] == "presentation" else "docx")
                ),
                "name": row["name"],
                "root": row["root"],
                "entry": row["entry"],
                "source_sha256": sha256(entry),
                "root_tree_sha256": facts["tree_sha256"],
                "root_git_tree_id": facts["git_tree_id"],
                "corpus_revision": facts["git"]["revision"],
                "repository_root": facts["git"]["repository_root"],
                "repository_dirty": facts["git"]["dirty"],
                "license": facts["license"],
            }
        )

    metadata = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "corpus_root": str(corpus),
        "source_manifest": str(manifest),
        "source_manifest_sha256": sha256(manifest),
        "documents": len(documents),
        "unique_roots": len(root_cache),
        "licensed_documents": sum(item["license"]["verified"] for item in documents),
    }
    (args.out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    with (args.out / "documents.jsonl").open("w", encoding="utf-8") as stream:
        for document in documents:
            stream.write(json.dumps(document, ensure_ascii=False, sort_keys=True) + "\n")
    print(json.dumps(metadata, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

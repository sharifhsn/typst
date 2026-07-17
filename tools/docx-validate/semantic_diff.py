#!/usr/bin/env python3
"""Explain PDF/DOCX semantic-token differences for an authority artifact."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import unicodedata
import zipfile
from collections import Counter
from pathlib import Path
from xml.etree import ElementTree


WORD_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
MATH_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"
TEXT_TAGS = {f"{{{WORD_NS}}}t", f"{{{MATH_NS}}}t"}
TEXT_RE = re.compile(r"[^\W\d_]{2,}", re.UNICODE)


def tokens(text: str) -> list[str]:
    return [token.casefold() for token in TEXT_RE.findall(text)]


def canonical_tokens(text: str, *, minimum_length: int = 2) -> list[str]:
    normalized = unicodedata.normalize("NFKC", text).casefold()
    pattern = re.compile(rf"[^\W\d_]{{{minimum_length},}}", re.UNICODE)
    return pattern.findall(normalized)


def is_mathematical_token(token: str) -> bool:
    return any("MATHEMATICAL" in unicodedata.name(char, "") for char in token)


def canonical_letters(text: str) -> Counter[str]:
    normalized = unicodedata.normalize("NFKC", text).casefold()
    return Counter(char for char in normalized if char.isalpha())


def pdf_pages(path: Path, *, layout: bool = False) -> list[str]:
    options = ["-q"]
    if layout:
        options.append("-layout")
    result = subprocess.run(
        ["pdftotext", *options, str(path), "-"],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.split("\f")


def docx_parts(path: Path) -> dict[str, str]:
    by_part: dict[str, str] = {}
    with zipfile.ZipFile(path) as package:
        for name in sorted(package.namelist()):
            if not name.startswith("word/") or not name.endswith(".xml"):
                continue
            try:
                root = ElementTree.fromstring(package.read(name))
            except ElementTree.ParseError:
                continue
            text = " ".join(node.text or "" for node in root.iter() if node.tag in TEXT_TAGS)
            if text:
                by_part[name] = text
    return by_part


def ranked(counter: Counter[str], limit: int) -> list[dict[str, int | str]]:
    return [{"token": token, "occurrences": count} for token, count in counter.most_common(limit)]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("pdf", type=Path)
    parser.add_argument("docx", type=Path)
    parser.add_argument("--limit", type=int, default=100)
    args = parser.parse_args()

    pages = pdf_pages(args.pdf)
    layout_pages = pdf_pages(args.pdf, layout=True)
    page_tokens = [tokens(page) for page in pages]
    pdf = Counter(token for page in page_tokens for token in page)
    parts = docx_parts(args.docx)
    docx = Counter(token for text in parts.values() for token in tokens(text))
    matched = pdf & docx
    pdf_only = pdf - docx
    docx_only = docx - pdf
    union = pdf | docx

    missing_by_page = []
    for index, values in enumerate(page_tokens, start=1):
        page = Counter(values)
        missing = page & pdf_only
        if missing:
            missing_by_page.append(
                {
                    "page": index,
                    "occurrences": sum(missing.values()),
                    "distinct": len(missing),
                    "top": ranked(missing, 12),
                }
            )

    pdf_text = " ".join(pages)
    docx_text = " ".join(parts.values())
    canonical_pdf = Counter(canonical_tokens(pdf_text))
    canonical_docx = Counter(canonical_tokens(docx_text))
    canonical_matched = canonical_pdf & canonical_docx
    canonical_union = canonical_pdf | canonical_docx
    single_pdf = Counter(canonical_tokens(pdf_text, minimum_length=1))
    single_docx = Counter(canonical_tokens(docx_text, minimum_length=1))
    single_matched = single_pdf & single_docx
    single_union = single_pdf | single_docx
    letter_pdf = canonical_letters(pdf_text)
    letter_docx = canonical_letters(docx_text)
    letter_matched = letter_pdf & letter_docx
    letter_union = letter_pdf | letter_docx
    mathematical_pdf_only = Counter(
        {token: count for token, count in pdf_only.items() if is_mathematical_token(token)}
    )
    plain_pdf_only = pdf_only - mathematical_pdf_only
    footer = Counter()
    footer_lines: list[str] = []
    for page in layout_pages:
        lines = [line.strip() for line in page.splitlines() if line.strip()]
        if lines:
            footer_lines.append(lines[-1])
            footer.update(tokens(lines[-1]))
    footer_pdf_only = pdf_only & footer
    other_pdf_only = pdf_only - mathematical_pdf_only - footer_pdf_only
    footer_letters = canonical_letters(" ".join(footer_lines))
    footer_letter_pdf_only = (letter_pdf - letter_docx) & footer_letters
    other_letter_pdf_only = letter_pdf - letter_docx - footer_letter_pdf_only

    output = {
        "counts": {
            "pdf": sum(pdf.values()),
            "docx": sum(docx.values()),
            "matched": sum(matched.values()),
            "pdf_only": sum(pdf_only.values()),
            "docx_only": sum(docx_only.values()),
            "recall": sum(matched.values()) / sum(pdf.values()) if pdf else None,
            "precision": sum(matched.values()) / sum(docx.values()) if docx else None,
            "multiset_jaccard": sum(matched.values()) / sum(union.values()) if union else None,
        },
        "canonical_nfkc_counts": {
            "pdf": sum(canonical_pdf.values()),
            "docx": sum(canonical_docx.values()),
            "matched": sum(canonical_matched.values()),
            "pdf_only": sum((canonical_pdf - canonical_docx).values()),
            "docx_only": sum((canonical_docx - canonical_pdf).values()),
            "recall": sum(canonical_matched.values()) / sum(canonical_pdf.values()),
            "precision": sum(canonical_matched.values()) / sum(canonical_docx.values()),
            "multiset_jaccard": sum(canonical_matched.values()) / sum(canonical_union.values()),
        },
        "canonical_nfkc_single_letter_counts": {
            "pdf": sum(single_pdf.values()),
            "docx": sum(single_docx.values()),
            "matched": sum(single_matched.values()),
            "pdf_only": sum((single_pdf - single_docx).values()),
            "docx_only": sum((single_docx - single_pdf).values()),
            "recall": sum(single_matched.values()) / sum(single_pdf.values()),
            "precision": sum(single_matched.values()) / sum(single_docx.values()),
            "multiset_jaccard": sum(single_matched.values()) / sum(single_union.values()),
        },
        "canonical_letter_character_counts": {
            "pdf": sum(letter_pdf.values()),
            "docx": sum(letter_docx.values()),
            "matched": sum(letter_matched.values()),
            "pdf_only": sum((letter_pdf - letter_docx).values()),
            "docx_only": sum((letter_docx - letter_pdf).values()),
            "recall": sum(letter_matched.values()) / sum(letter_pdf.values()),
            "precision": sum(letter_matched.values()) / sum(letter_docx.values()),
            "multiset_jaccard": sum(letter_matched.values()) / sum(letter_union.values()),
            "pdf_only_found_on_last_page_lines": sum(footer_letter_pdf_only.values()),
            "other_pdf_only": sum(other_letter_pdf_only.values()),
        },
        "raw_pdf_only_classification": {
            "mathematical_styled_occurrences": sum(mathematical_pdf_only.values()),
            "last_line_footer_occurrences": sum(footer_pdf_only.values()),
            "plain_or_mixed_occurrences": sum(plain_pdf_only.values()),
            "other_after_math_and_footer": sum(
                other_pdf_only.values()
            ),
        },
        "docx_parts": {name: len(tokens(text)) for name, text in parts.items()},
        "top_pdf_only": ranked(pdf_only, args.limit),
        "top_plain_or_mixed_pdf_only": ranked(plain_pdf_only, args.limit),
        "top_last_line_footer_pdf_only": ranked(footer_pdf_only, args.limit),
        "top_other_pdf_only": ranked(other_pdf_only, args.limit),
        "other_pdf_only_letters": ranked(other_letter_pdf_only, args.limit),
        "top_docx_only": ranked(docx_only, args.limit),
        "pages_with_most_pdf_only": sorted(
            missing_by_page, key=lambda item: int(item["occurrences"]), reverse=True
        )[: args.limit],
    }
    print(json.dumps(output, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()

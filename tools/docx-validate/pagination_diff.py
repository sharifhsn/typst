#!/usr/bin/env python3
"""Compare where shared text lands across two paginated PDF renderings."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import unicodedata
import zipfile
from collections import Counter, defaultdict
from pathlib import Path
from xml.etree import ElementTree


TOKEN_RE = re.compile(r"[^\W\d_]{2,}", re.UNICODE)
WORD_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
MATH_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"
NS = {"w": WORD_NS, "m": MATH_NS}


def extract_pages(path: Path) -> list[str]:
    result = subprocess.run(
        ["pdftotext", "-q", "-layout", str(path), "-"],
        check=True,
        capture_output=True,
        text=True,
    )
    pages = result.stdout.split("\f")
    if pages and not pages[-1].strip():
        pages.pop()
    return pages


def tokens(text: str) -> list[str]:
    normalized = unicodedata.normalize("NFKC", text).casefold()
    return TOKEN_RE.findall(normalized)


def visible_lines(page: str) -> list[str]:
    return [line.strip() for line in page.splitlines() if line.strip()]


def ngram_locations(pages: list[list[str]], width: int) -> dict[tuple[str, ...], tuple[int, int]]:
    locations: dict[tuple[str, ...], list[tuple[int, int]]] = defaultdict(list)
    for page, values in enumerate(pages, start=1):
        for offset in range(max(0, len(values) - width + 1)):
            locations[tuple(values[offset : offset + width])].append((page, offset))
    return {gram: found[0] for gram, found in locations.items() if len(found) == 1}


def page_mappings(
    left: list[list[str]], right: list[list[str]], width: int
) -> list[dict[str, int | str | None]]:
    left_grams = ngram_locations(left, width)
    right_grams = ngram_locations(right, width)
    candidates: dict[int, list[tuple[int, int, tuple[str, ...]]]] = defaultdict(list)
    for gram, (left_page, left_offset) in left_grams.items():
        if gram in right_grams:
            right_page, _ = right_grams[gram]
            candidates[left_page].append((left_offset, right_page, gram))

    mappings = []
    for page in range(1, len(left) + 1):
        options = sorted(candidates.get(page, []), key=lambda item: item[0])
        if not options:
            mappings.append(
                {"reference_page": page, "consumer_page": None, "delta": None, "anchor": None}
            )
            continue
        offset, consumer_page, gram = options[0]
        mappings.append(
            {
                "reference_page": page,
                "consumer_page": consumer_page,
                "delta": consumer_page - page,
                "reference_token_offset": offset,
                "anchor": " ".join(gram),
            }
        )
    return mappings


def phrase_pages(pages: list[str], phrase: str) -> list[int]:
    needle = " ".join(tokens(phrase))
    found = []
    for index, page in enumerate(pages, start=1):
        if needle in " ".join(tokens(page)):
            found.append(index)
    return found


def page_stats(pages: list[str]) -> list[dict[str, int]]:
    return [
        {"page": index, "tokens": len(tokens(page)), "lines": len(visible_lines(page))}
        for index, page in enumerate(pages, start=1)
    ]


def spacing_before(paragraph: ElementTree.Element) -> int:
    spacing = paragraph.find("w:pPr/w:spacing", NS)
    if spacing is None:
        return 0
    return int(spacing.attrib.get(f"{{{WORD_NS}}}before", "0"))


def docx_flow(path: Path) -> dict[str, object]:
    with zipfile.ZipFile(path) as package:
        root = ElementTree.fromstring(package.read("word/document.xml"))
    paragraphs = list(root.iter(f"{{{WORD_NS}}}p"))
    math_indexes = [
        index
        for index, paragraph in enumerate(paragraphs)
        if paragraph.find("m:oMathPara", NS) is not None
    ]
    math_before = Counter(spacing_before(paragraphs[index]) for index in math_indexes)
    following_before = Counter(
        spacing_before(paragraphs[index + 1])
        for index in math_indexes
        if index + 1 < len(paragraphs)
    )
    page_break_before = sum(
        paragraph.find("w:pPr/w:pageBreakBefore", NS) is not None for paragraph in paragraphs
    )
    explicit_page_breaks = len(root.findall(".//w:br[@w:type='page']", NS))
    return {
        "paragraphs": len(paragraphs),
        "display_math_paragraphs": len(math_indexes),
        "display_math_before_twips": {
            str(amount): count for amount, count in sorted(math_before.items())
        },
        "paragraph_after_display_math_before_twips": {
            str(amount): count for amount, count in sorted(following_before.items())
        },
        "display_math_total_before_points": sum(
            spacing_before(paragraphs[index]) for index in math_indexes
        )
        / 20,
        "following_paragraph_total_before_points": sum(
            spacing_before(paragraphs[index + 1])
            for index in math_indexes
            if index + 1 < len(paragraphs)
        )
        / 20,
        "page_break_before_paragraphs": page_break_before,
        "explicit_page_breaks": explicit_page_breaks,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("reference", type=Path)
    parser.add_argument("consumer", type=Path)
    parser.add_argument("--docx", type=Path)
    parser.add_argument("--anchor", action="append", default=[])
    parser.add_argument("--ngram-width", type=int, default=6)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    reference_pages = extract_pages(args.reference)
    consumer_pages = extract_pages(args.consumer)
    reference_tokens = [tokens(page) for page in reference_pages]
    consumer_tokens = [tokens(page) for page in consumer_pages]
    mappings = page_mappings(reference_tokens, consumer_tokens, args.ngram_width)
    mapped = [item for item in mappings if item["consumer_page"] is not None]
    deltas = Counter(int(item["delta"]) for item in mapped)

    output = {
        "summary": {
            "reference_pages": len(reference_pages),
            "consumer_pages": len(consumer_pages),
            "page_delta": len(consumer_pages) - len(reference_pages),
            "mapped_reference_pages": len(mapped),
            "reference_tokens": sum(len(values) for values in reference_tokens),
            "consumer_tokens": sum(len(values) for values in consumer_tokens),
            "median_tokens_per_page_reference": sorted(map(len, reference_tokens))[
                len(reference_tokens) // 2
            ],
            "median_tokens_per_page_consumer": sorted(map(len, consumer_tokens))[
                len(consumer_tokens) // 2
            ],
        },
        "delta_histogram": {str(delta): count for delta, count in sorted(deltas.items())},
        "anchors": {
            phrase: {
                "reference_pages": phrase_pages(reference_pages, phrase),
                "consumer_pages": phrase_pages(consumer_pages, phrase),
            }
            for phrase in args.anchor
        },
        "reference_page_mappings": mappings,
        "reference_page_stats": page_stats(reference_pages),
        "consumer_page_stats": page_stats(consumer_pages),
        "lowest_density_consumer_pages": sorted(
            page_stats(consumer_pages), key=lambda item: (item["tokens"], item["lines"])
        )[:30],
    }
    if args.docx:
        output["docx_flow"] = docx_flow(args.docx)
    encoded = json.dumps(output, indent=2, ensure_ascii=False)
    if args.output:
        args.output.write_text(encoded + "\n", encoding="utf-8")
    else:
        print(encoded)


if __name__ == "__main__":
    main()

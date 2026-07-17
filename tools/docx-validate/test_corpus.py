import argparse
import contextlib
import io
import json
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import corpus


class FidelityManifestTests(unittest.TestCase):
    def test_parse_fidelity_groups_occurrences_by_element_and_reason(self) -> None:
        xml = b"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<typst:fidelity xmlns:typst="https://typst.app/schema/2026/fidelity"
  enrollment="complete" snapshotId="test">
  <typst:counts native="0" nativeWithFallback="0" approximate="0"
    raster="0" drop="3" dynamicFields="0" referencedFonts="0"
    missingFonts="0" drawings="0" unlabeledDrawings="0" measuredTables="0"/>
  <typst:decisions>
    <typst:decision sourceId="a" element="box" representation="Drop"
      reason="UnsupportedContent" occurrences="2" affectedTextChars="0"
      affectedSemanticNodes="2" visual="true" semantic="true"
      editability="true" dynamic="true" accessibility="true" portability="true"/>
    <typst:decision sourceId="b" element="place" representation="Drop"
      reason="UnsupportedContent" occurrences="1" affectedTextChars="0"
      affectedSemanticNodes="1" visual="true" semantic="true"
      editability="true" dynamic="true" accessibility="true" portability="true"/>
  </typst:decisions>
</typst:fidelity>"""

        parsed = corpus.parse_fidelity({"customXml/typstFidelity.xml": xml})

        self.assertEqual(parsed["occurrences_by_element"], {"box": 2, "place": 1})
        self.assertEqual(
            parsed["occurrences_by_reason_and_element"],
            {"UnsupportedContent:box": 2, "UnsupportedContent:place": 1},
        )


class OfficeFormatTests(unittest.TestCase):
    def test_presentation_category_routes_to_pptx(self) -> None:
        self.assertEqual(corpus.target_format({"category": "presentation"}), "pptx")
        self.assertEqual(corpus.target_format({"category": "report"}), "docx")

    def test_explicit_target_format_overrides_category(self) -> None:
        self.assertEqual(
            corpus.target_format({"category": "report", "target_format": "pptx"}),
            "pptx",
        )

    def test_curated_entry_override_repairs_mislabeled_category(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            overrides = Path(directory) / "overrides.json"
            overrides.write_text('{"repos/deck/main.typ":"pptx"}', encoding="utf-8")
            with mock.patch.object(corpus, "FORMAT_OVERRIDES_PATH", overrides):
                self.assertEqual(
                    corpus.target_format({
                        "category": "uncategorized",
                        "entry": "repos/deck/main.typ",
                    }),
                    "pptx",
                )

    def test_pptx_package_text_metrics_and_raster_audit(self) -> None:
        slide = b"""<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
 <p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Hello deck</a:t></a:r></a:p>
 </p:txBody></p:sp><p:pic/><a:tbl/></p:spTree></p:cSld></p:sld>"""
        presentation = b"""<?xml version="1.0"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"""
        content_types = b"""<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "deck.pptx"
            with zipfile.ZipFile(path, "w") as package:
                package.writestr("[Content_Types].xml", content_types)
                package.writestr("ppt/presentation.xml", presentation)
                package.writestr("ppt/slides/slide1.xml", slide)
            result, parts = corpus.pptx_package_check(path)

        self.assertTrue(result["ok"])
        self.assertEqual(corpus.pptx_text(parts), "Hello deck")
        metrics = corpus.pptx_editability_metrics(parts)
        self.assertEqual(metrics["slides"], 1)
        self.assertEqual(metrics["pictures"], 1)
        fidelity = corpus.pptx_fidelity({
            "stderr": "RASTERIZE kind=group reason=clip text_chars=12\n"
        })
        self.assertEqual(fidelity["raster_events"], 1)
        self.assertEqual(fidelity["rasterized_text_chars"], 12)


class ExporterIdentityTests(unittest.TestCase):
    def test_main_captures_one_identity_for_all_workers(self) -> None:
        state = {
            "revision": "captured-revision",
            "dirty": False,
            "status_sha256": "status",
            "diff_and_untracked_sha256": "diff",
            "untracked_files": [],
        }
        seen = []

        def validate(frozen, args, _corpus_root):
            seen.append(corpus.frozen_exporter_identity(args))
            return {"id": frozen["id"], "primary_class": "unverified"}

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            frozen = root / "freeze" / "documents.jsonl"
            frozen.parent.mkdir()
            frozen.write_text('{"id":"one"}\n{"id":"two"}\n', encoding="utf-8")
            (frozen.parent / "metadata.json").write_text(
                json.dumps({"corpus_root": str(root / "corpus")}), encoding="utf-8"
            )
            typst = root / "typst"
            typst.write_bytes(b"binary")
            out = root / "out"
            argv = [
                "corpus.py",
                "--typst",
                str(typst),
                "--frozen",
                str(frozen),
                "--out",
                str(out),
                "--jobs",
                "2",
            ]
            state_probe = mock.Mock(return_value=state)
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(corpus, "exporter_state", state_probe),
                mock.patch.object(corpus, "validate_document", side_effect=validate),
                mock.patch.object(
                    corpus,
                    "write_reports",
                    return_value={"primary_classes": {}},
                ),
                mock.patch.object(corpus.validator, "sha256", return_value="binary-sha"),
                mock.patch.object(
                    corpus.validator, "command_version", return_value="test-version"
                ),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(corpus.main(), 0)

            state_probe.assert_called_once_with()
            self.assertEqual(len(seen), 2)
            self.assertTrue(all(identity == seen[0] for identity in seen))
            self.assertEqual(seen[0]["exporter_revision"], "captured-revision")
            self.assertEqual(seen[0]["exporter_binary_sha256"], "binary-sha")
            metadata = json.loads((out / "metadata.json").read_text(encoding="utf-8"))
            self.assertEqual(metadata["exporter_revision"], "captured-revision")
            self.assertEqual(metadata["exporter_state"], state)


class FontFixtureTests(unittest.TestCase):
    def test_document_font_paths_resolves_named_checked_in_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "gb-ctr"
            fixture.mkdir()
            with mock.patch.object(corpus, "FONT_FIXTURE_ROOT", root):
                self.assertEqual(
                    corpus.document_font_paths({"name": "gb-ctr"}),
                    [fixture.resolve()],
                )

    def test_document_font_paths_is_empty_without_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(corpus, "FONT_FIXTURE_ROOT", Path(directory)):
                self.assertEqual(corpus.document_font_paths({"name": "other"}), [])

    def test_font_fixture_evidence_changes_with_font_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            font = root / "Example.ttf"
            font.write_bytes(b"first")
            first = corpus.font_fixture_evidence([root])
            font.write_bytes(b"second")
            second = corpus.font_fixture_evidence([root])

        self.assertNotEqual(first[0]["sha256"], second[0]["sha256"])
        self.assertEqual(first[0]["files"], ["Example.ttf"])

    def test_frozen_identity_never_reloads_git_state(self) -> None:
        state = {
            "revision": "captured-revision",
            "dirty": False,
            "status_sha256": "status",
            "diff_and_untracked_sha256": "diff",
            "untracked_files": [],
        }
        args = argparse.Namespace(
            exporter_revision=state["revision"],
            exporter_state=state,
            exporter_binary_sha256="captured-binary",
        )

        with mock.patch.object(
            corpus.validator,
            "git_revision",
            side_effect=AssertionError("worker reloaded live Git state"),
        ):
            first = corpus.frozen_exporter_identity(args)
            second = corpus.frozen_exporter_identity(args)

        expected = {
            "exporter_revision": "captured-revision",
            "exporter_state": state,
            "exporter_binary_sha256": "captured-binary",
        }
        self.assertEqual(first, expected)
        self.assertEqual(second, expected)

    def test_resume_defaults_share_the_same_frozen_identity(self) -> None:
        original = {
            "exporter_revision": "original-revision",
            "exporter_state": {"revision": "original-revision"},
            "exporter_binary_sha256": "original-binary",
        }
        current = argparse.Namespace(
            exporter_revision="resume-revision",
            exporter_state={"revision": "resume-revision"},
            exporter_binary_sha256="resume-binary",
        )

        for key, value in corpus.frozen_exporter_identity(current).items():
            original.setdefault(key, value)

        self.assertEqual(original["exporter_revision"], "original-revision")
        self.assertEqual(original["exporter_state"]["revision"], "original-revision")
        self.assertEqual(original["exporter_binary_sha256"], "original-binary")

    def test_resume_rejects_changed_consumer_tools(self) -> None:
        original = {
            "soffice": "LibreOffice 26.2.4.2",
            "pdftotext": "pdftotext 26.06.0",
            "pdftoppm": "pdftoppm 26.06.0",
        }
        current = dict(original)
        current["soffice"] = "LibreOfficeDev 26.8.0.0.alpha0"

        with self.assertRaisesRegex(
            ValueError,
            r"soffice: 'LibreOffice 26\.2\.4\.2' -> "
            r"'LibreOfficeDev 26\.8\.0\.0\.alpha0'.*new --out",
        ):
            corpus.require_same_resume_tools(original, current)

    def test_resume_accepts_identical_consumer_tools(self) -> None:
        tools = {
            "soffice": "LibreOffice 26.2.4.2",
            "pdftotext": "pdftotext 26.06.0",
            "pdftoppm": "pdftoppm 26.06.0",
        }
        corpus.require_same_resume_tools(tools, dict(tools))


if __name__ == "__main__":
    unittest.main()

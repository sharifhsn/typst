import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run as validator


class PdfRasterizationTests(unittest.TestCase):
    def test_rasterized_pages_preserve_color_for_visual_review(self) -> None:
        commands: list[list[str]] = []

        def render(command: list[str], *, timeout: int) -> dict[str, object]:
            commands.append(command)
            Path(command[-1] + "-1.png").touch()
            return {"exit_code": 0, "stderr": "", "timeout": None}

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf = root / "input.pdf"
            pdf.touch()
            with (
                mock.patch.object(validator.shutil, "which", return_value="pdftoppm"),
                mock.patch.object(validator, "run_command", side_effect=render),
            ):
                pages, error = validator.rasterize_pages(pdf, root, "page")

        self.assertIsNone(error)
        self.assertEqual([page.name for page in pages], ["page-1.png"])
        self.assertEqual(commands[0][:4], ["pdftoppm", "-png", "-r", "60"])
        self.assertNotIn("-gray", commands[0])


class HyperlinkMetricsTests(unittest.TestCase):
    def test_internal_ref_field_with_hyperlink_switch_counts(self) -> None:
        parts = {
            "word/document.xml": (
                b'<w:document><w:body><w:p><w:fldSimple '
                b'w:instr=" REF validator-heading \\h "/></w:p></w:body></w:document>'
            )
        }

        metrics = validator.editability_metrics(parts)

        self.assertEqual(metrics["hyperlinks"], 1)
        self.assertEqual(metrics["literal_hyperlinks"], 0)
        self.assertEqual(metrics["internal_hyperlink_fields"], 1)

    def test_pageref_and_literal_external_hyperlink_both_count(self) -> None:
        parts = {
            "word/document.xml": (
                b'<w:document><w:body><w:p><w:hyperlink w:id="rId1"/>'
                b'<w:r><w:instrText xml:space="preserve"> PAGEREF page-target \\h '
                b'</w:instrText></w:r></w:p></w:body></w:document>'
            )
        }

        metrics = validator.editability_metrics(parts)

        self.assertEqual(metrics["hyperlinks"], 2)
        self.assertEqual(metrics["literal_hyperlinks"], 1)
        self.assertEqual(metrics["internal_hyperlink_fields"], 1)

    def test_ref_without_hyperlink_switch_is_not_counted_as_link(self) -> None:
        self.assertEqual(validator.internal_hyperlink_field_count(
            '<w:instrText> REF validator-heading </w:instrText>'
        ), 0)


if __name__ == "__main__":
    unittest.main()

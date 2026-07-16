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


if __name__ == "__main__":
    unittest.main()

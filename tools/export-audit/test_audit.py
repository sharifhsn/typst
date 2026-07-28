import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import audit


class PresentationTextCommandTests(unittest.TestCase):
    def test_presentation_text_lane_exports_pptx_and_reads_slide_text(self) -> None:
        args = SimpleNamespace(
            binary="typst", out="", n=1, kind="presentation", filter="", links=False
        )
        source = Path("decks/example.typ")
        completed = SimpleNamespace(returncode=0, stdout=b"slide words")

        with tempfile.TemporaryDirectory() as directory:
            args.out = directory
            with (
                mock.patch.object(audit, "check_binary", return_value="typst"),
                mock.patch.object(audit, "docs", return_value=[source]) as documents,
                mock.patch.object(audit, "export", return_value=True) as export,
                mock.patch.object(audit.subprocess, "run", return_value=completed),
                mock.patch.object(audit, "read_parts", return_value={"slide": b""}),
                mock.patch.object(audit, "pptx_text", return_value="slide words") as pptx_text,
                mock.patch.object(audit, "docx_text", side_effect=AssertionError("wrong extractor")),
            ):
                self.assertEqual(audit.cmd_text(args), 0)

        documents.assert_called_once_with(1, "presentation", "")
        self.assertEqual([call.args[2] for call in export.call_args_list], ["pdf", "pptx"])
        pptx_text.assert_called_once()


if __name__ == "__main__":
    unittest.main()

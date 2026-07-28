import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import corpuslib


class PptxTextTests(unittest.TestCase):
    def test_reads_only_slide_body_text_in_numeric_order(self) -> None:
        parts = {
            "ppt/slides/slide10.xml": b"<a:t>ten</a:t>",
            "ppt/slides/slide2.xml": b"<a:t>two</a:t>",
            "ppt/slides/slide1.xml": b"<a:t>one</a:t>",
            "ppt/slideLayouts/slideLayout1.xml": b"<a:t>layout placeholder</a:t>",
            "ppt/notesSlides/notesSlide1.xml": b"<a:t>speaker notes</a:t>",
        }

        self.assertEqual(corpuslib.pptx_text(parts), "one two ten ")

    def test_slide_paragraphs_are_word_boundaries(self) -> None:
        parts = {
            "ppt/slides/slide1.xml": b"<a:t>first</a:t></a:p><a:t>second</a:t>",
        }

        self.assertEqual(corpuslib.pptx_text(parts), "first second ")


class ExportPathTests(unittest.TestCase):
    def test_relative_output_is_anchored_before_corpus_cwd(self) -> None:
        relative = Path("target/export-audit-test/relative.docx")
        absolute = relative.absolute()
        absolute.parent.mkdir(parents=True, exist_ok=True)
        absolute.unlink(missing_ok=True)

        def fake_run(command, **kwargs):
            self.assertEqual(kwargs["cwd"], corpuslib.CORPUS)
            self.assertEqual(Path(command[-1]), absolute)
            absolute.touch()

        try:
            with patch.object(corpuslib.subprocess, "run", side_effect=fake_run):
                self.assertTrue(
                    corpuslib.export("/bin/typst", Path("sample.typ"), "docx", relative)
                )
        finally:
            absolute.unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()

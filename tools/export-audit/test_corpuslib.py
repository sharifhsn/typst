import sys
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()

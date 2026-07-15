import argparse
import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import corpus


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


if __name__ == "__main__":
    unittest.main()

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "stamp-game-build.py"
SPEC = importlib.util.spec_from_file_location("stamp_game_build", SCRIPT)
assert SPEC and SPEC.loader
STAMP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAMP)

SHA = "0123456789abcdef0123456789abcdef01234567"


class StampGameBuildTests(unittest.TestCase):
    def project(self, root: Path, setting: str = 'config/build_commit="development"') -> Path:
        project = root / "project.godot"
        project.write_text(f'[application]\nconfig/name="Spurfire"\n{setting}\n', encoding="utf-8")
        return project

    def test_stamps_exact_commit_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            project = self.project(Path(directory))
            STAMP.stamp(project, SHA)
            first = project.read_text(encoding="utf-8")
            STAMP.stamp(project, SHA)
            self.assertEqual(project.read_text(encoding="utf-8"), first)
            self.assertIn(f'config/build_commit="{SHA}"', first)
            self.assertNotIn("development", first)

    def test_rejects_noncanonical_sha_without_mutating(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            project = self.project(Path(directory))
            original = project.read_bytes()
            for invalid in ("development", "A" * 40, "0" * 39, "0" * 41):
                with self.subTest(invalid=invalid), self.assertRaises(STAMP.StampError):
                    STAMP.stamp(project, invalid)
                self.assertEqual(project.read_bytes(), original)

    def test_rejects_missing_or_duplicate_setting(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing = self.project(root, 'config/version="0.2.0"')
            with self.assertRaises(STAMP.StampError):
                STAMP.stamp(missing, SHA)
            duplicate = self.project(
                root,
                'config/build_commit="development"\nconfig/build_commit="development"',
            )
            with self.assertRaises(STAMP.StampError):
                STAMP.stamp(duplicate, SHA)


if __name__ == "__main__":
    unittest.main()

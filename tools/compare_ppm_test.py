#!/usr/bin/env python3
"""Unit tests for compare_ppm.py."""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_ppm


class ComparePpmTests(unittest.TestCase):
    def write_ppm(self, path: Path, pixels: bytes, width: int = 2, height: int = 1) -> None:
        compare_ppm.write_ppm(path, width, height, pixels)

    def run_tool(self, *args: str) -> tuple[int, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = compare_ppm.main(list(args))
        return code, stdout.getvalue()

    def test_identical_file_comparison_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            a = Path(tmp) / "a.ppm"
            b = Path(tmp) / "b.ppm"
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(a, pixels)
            self.write_ppm(b, pixels)

            code, output = self.run_tool(str(a), str(b))

            self.assertEqual(code, 0)
            self.assertIn("changed_pixels: 0", output)
            self.assertIn("max_channel_delta: 0", output)

    def test_different_file_comparison_fails_and_writes_diff(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            a = Path(tmp) / "a.ppm"
            b = Path(tmp) / "b.ppm"
            diff = Path(tmp) / "diff.ppm"
            self.write_ppm(a, bytes([0, 0, 0, 255, 128, 64]))
            self.write_ppm(b, bytes([0, 0, 0, 0, 128, 64]))

            code, output = self.run_tool(str(a), str(b), "--write-diff", str(diff))

            self.assertEqual(code, 1)
            self.assertIn("changed_pixels: 1", output)
            self.assertIn("max_channel_delta: 255", output)
            width, height, pixels = compare_ppm.read_ppm(diff)
            self.assertEqual((width, height), (2, 1))
            self.assertEqual(pixels, bytes([0, 0, 0, 255, 0, 0]))

    def test_directory_comparison_uses_sequence_frames_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            actual = Path(tmp) / "actual"
            reference = Path(tmp) / "reference"
            actual.mkdir()
            reference.mkdir()
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000001.ppm", pixels)
            self.write_ppm(actual / "frame-000001-diff.ppm", bytes([255, 0, 0, 0, 0, 0]))

            code, output = self.run_tool(str(actual), str(reference))

            self.assertEqual(code, 0)
            self.assertIn("frames: 1", output)
            self.assertNotIn("frame-000001-diff.ppm", output)

    def test_directory_comparison_reports_missing_frames(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            actual = Path(tmp) / "actual"
            reference = Path(tmp) / "reference"
            actual.mkdir()
            reference.mkdir()
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000002.ppm", pixels)

            code, _ = self.run_tool(str(actual), str(reference))

            self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()

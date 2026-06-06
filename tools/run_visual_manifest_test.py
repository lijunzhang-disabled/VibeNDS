#!/usr/bin/env python3
"""Unit tests for run_visual_manifest.py."""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_ppm
import run_visual_manifest


class VisualManifestTests(unittest.TestCase):
    def write_ppm(self, path: Path, pixels: bytes) -> None:
        compare_ppm.write_ppm(path, 2, 1, pixels)

    def write_metadata(
        self,
        path: Path,
        *,
        kind: str,
        frame_files: list[str] | None = None,
        screen_gap: int = 0,
    ) -> None:
        path.write_text(
            json.dumps(
                {
                    "format": "nds-frontend-capture-v1",
                    "kind": kind,
                    "rom": "/tmp/test.nds",
                    "rom_size": 1024,
                    "rom_title": "TEST ROM",
                    "gamecode": "TEST",
                    "header_crc_valid": True,
                    "capture_frames": 1,
                    "capture_interval": 1,
                    "screen_gap": screen_gap,
                    "screen_width": 256,
                    "screen_height": 192,
                    "output_width": 256,
                    "output_height": 384 + screen_gap,
                    "frame_files": frame_files or [],
                }
            )
        )

    def run_manifest(self, manifest: Path) -> tuple[int, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = run_visual_manifest.main([str(manifest)])
        return code, stdout.getvalue()

    def test_file_case_passes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actual = root / "actual.ppm"
            reference = root / "reference.ppm"
            manifest = root / "manifest.json"
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual, pixels)
            self.write_ppm(reference, pixels)
            self.write_metadata(actual.with_suffix(".json"), kind="single")
            self.write_metadata(reference.with_suffix(".json"), kind="single")
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "same-frame",
                                "actual": str(actual),
                                "reference": str(reference),
                            }
                        ]
                    }
                )
            )

            code, output = self.run_manifest(manifest)

            self.assertEqual(code, 0)
            self.assertIn("PASS same-frame", output)
            self.assertIn("summary: cases=1 failures=0", output)

    def test_file_case_failure_exits_nonzero_and_writes_diff(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actual = root / "actual.ppm"
            reference = root / "reference.ppm"
            diff = root / "diff.ppm"
            manifest = root / "manifest.json"
            self.write_ppm(actual, bytes([0, 0, 0, 255, 128, 64]))
            self.write_ppm(reference, bytes([0, 0, 0, 0, 128, 64]))
            self.write_metadata(actual.with_suffix(".json"), kind="single")
            self.write_metadata(reference.with_suffix(".json"), kind="single")
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "different-frame",
                                "actual": str(actual),
                                "reference": str(reference),
                                "write_diff": str(diff),
                            }
                        ]
                    }
                )
            )

            code, output = self.run_manifest(manifest)

            self.assertEqual(code, 1)
            self.assertIn("FAIL different-frame", output)
            self.assertTrue(diff.exists())

    def test_directory_case_passes_and_writes_diff_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actual = root / "actual"
            reference = root / "reference"
            diff = root / "diff"
            manifest = root / "manifest.json"
            actual.mkdir()
            reference.mkdir()
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000001.ppm", pixels)
            self.write_metadata(
                actual / "capture-metadata.json",
                kind="sequence",
                frame_files=["frame-000001.ppm"],
            )
            self.write_metadata(
                reference / "capture-metadata.json",
                kind="sequence",
                frame_files=["frame-000001.ppm"],
            )
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "same-sequence",
                                "actual": str(actual),
                                "reference": str(reference),
                                "write_diff": str(diff),
                            }
                        ]
                    }
                )
            )

            code, output = self.run_manifest(manifest)

            self.assertEqual(code, 0)
            self.assertIn("PASS same-sequence", output)
            self.assertTrue((diff / "frame-000001.ppm").exists())

    def test_directory_metadata_mismatch_is_rejected_before_diff(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actual = root / "actual"
            reference = root / "reference"
            manifest = root / "manifest.json"
            actual.mkdir()
            reference.mkdir()
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000001.ppm", pixels)
            self.write_metadata(
                actual / "capture-metadata.json",
                kind="sequence",
                frame_files=["frame-000001.ppm"],
                screen_gap=0,
            )
            self.write_metadata(
                reference / "capture-metadata.json",
                kind="sequence",
                frame_files=["frame-000001.ppm"],
                screen_gap=8,
            )
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "bad-metadata",
                                "actual": str(actual),
                                "reference": str(reference),
                            }
                        ]
                    }
                )
            )

            code, _ = self.run_manifest(manifest)

            self.assertEqual(code, 2)

    def test_ignore_metadata_allows_legacy_capture_dirs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            actual = root / "actual"
            reference = root / "reference"
            manifest = root / "manifest.json"
            actual.mkdir()
            reference.mkdir()
            pixels = bytes([0, 0, 0, 255, 128, 64])
            self.write_ppm(actual / "frame-000001.ppm", pixels)
            self.write_ppm(reference / "frame-000001.ppm", pixels)
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "legacy",
                                "actual": str(actual),
                                "reference": str(reference),
                                "ignore_metadata": True,
                            }
                        ]
                    }
                )
            )

            code, output = self.run_manifest(manifest)

            self.assertEqual(code, 0)
            self.assertIn("PASS legacy", output)

    def test_manifest_rejects_negative_thresholds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "cases": [
                            {
                                "name": "bad",
                                "actual": "a.ppm",
                                "reference": "b.ppm",
                                "max_changed_pixels": -1,
                            }
                        ]
                    }
                )
            )

            code, _ = self.run_manifest(manifest)

            self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()

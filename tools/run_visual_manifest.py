#!/usr/bin/env python3
"""Run manifest-driven PPM visual comparisons.

The manifest is JSON so the runner stays dependency-free:

{
  "cases": [
    {
      "name": "heartgold-title",
      "actual": "/tmp/current-seq",
      "reference": "/tmp/reference-seq",
      "pixel_threshold": 0,
      "max_changed_pixels": 0,
      "max_channel_delta": 0,
      "write_diff": "/tmp/heartgold-title-diff"
    }
  ]
}
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_ppm


@dataclass(frozen=True)
class Case:
    name: str
    actual: Path
    reference: Path
    pixel_threshold: int = 0
    max_changed_pixels: int = 0
    max_channel_delta: int = 0
    write_diff: Path | None = None
    ignore_metadata: bool = False


class ManifestError(Exception):
    pass


def _int_field(raw: dict[str, Any], name: str, default: int = 0) -> int:
    value = raw.get(name, default)
    if not isinstance(value, int) or value < 0:
        raise ManifestError(f"{name} must be a non-negative integer")
    return value


def _path_field(raw: dict[str, Any], name: str) -> Path:
    value = raw.get(name)
    if not isinstance(value, str) or not value:
        raise ManifestError(f"{name} must be a non-empty string")
    return Path(value)


def _bool_field(raw: dict[str, Any], name: str, default: bool = False) -> bool:
    value = raw.get(name, default)
    if not isinstance(value, bool):
        raise ManifestError(f"{name} must be a boolean")
    return value


def parse_case(raw: Any, index: int) -> Case:
    if not isinstance(raw, dict):
        raise ManifestError(f"case {index} must be an object")
    name = raw.get("name", f"case-{index}")
    if not isinstance(name, str) or not name:
        raise ManifestError(f"case {index} name must be a non-empty string")
    write_diff_raw = raw.get("write_diff")
    if write_diff_raw is not None and not isinstance(write_diff_raw, str):
        raise ManifestError(f"case {name}: write_diff must be a string when present")
    return Case(
        name=name,
        actual=_path_field(raw, "actual"),
        reference=_path_field(raw, "reference"),
        pixel_threshold=_int_field(raw, "pixel_threshold"),
        max_changed_pixels=_int_field(raw, "max_changed_pixels"),
        max_channel_delta=_int_field(raw, "max_channel_delta"),
        write_diff=Path(write_diff_raw) if write_diff_raw else None,
        ignore_metadata=_bool_field(raw, "ignore_metadata"),
    )


def load_manifest(path: Path) -> list[Case]:
    try:
        raw = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        raise ManifestError(str(e)) from e
    if not isinstance(raw, dict):
        raise ManifestError("manifest root must be an object")
    raw_cases = raw.get("cases")
    if not isinstance(raw_cases, list) or not raw_cases:
        raise ManifestError("manifest must contain a non-empty cases array")
    return [parse_case(case, i) for i, case in enumerate(raw_cases)]


def metadata_path_for_capture(path: Path) -> Path:
    if path.is_dir():
        return path / "capture-metadata.json"
    return path.with_suffix(".json")


def load_capture_metadata(path: Path) -> dict[str, Any]:
    metadata_path = metadata_path_for_capture(path)
    try:
        raw = json.loads(metadata_path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        raise ManifestError(f"{metadata_path}: {e}") from e
    if not isinstance(raw, dict):
        raise ManifestError(f"{metadata_path}: metadata root must be an object")
    return raw


def validate_capture_metadata(case: Case, sequence: bool) -> None:
    if case.ignore_metadata:
        return
    actual = load_capture_metadata(case.actual)
    reference = load_capture_metadata(case.reference)
    expected_kind = "sequence" if sequence else "single"
    keys = [
        "format",
        "kind",
        "rom_size",
        "rom_title",
        "gamecode",
        "header_crc_valid",
        "capture_frames",
        "capture_interval",
        "screen_gap",
        "screen_width",
        "screen_height",
        "output_width",
        "output_height",
        "frame_files",
    ]
    for key in keys:
        if actual.get(key) != reference.get(key):
            raise ManifestError(
                f"{case.name}: capture metadata mismatch for {key}: "
                f"actual={actual.get(key)!r} reference={reference.get(key)!r}"
            )
    if actual.get("format") != "nds-frontend-capture-v1":
        raise ManifestError(f"{case.name}: unsupported metadata format {actual.get('format')!r}")
    if actual.get("kind") != expected_kind:
        raise ManifestError(
            f"{case.name}: expected metadata kind {expected_kind!r}, got {actual.get('kind')!r}"
        )


def compare_directory_case(case: Case) -> tuple[bool, list[compare_ppm.ImageDiff]]:
    validate_capture_metadata(case, sequence=True)
    actual_files = compare_ppm.ppm_files_by_name(case.actual)
    reference_files = compare_ppm.ppm_files_by_name(case.reference)
    names = sorted(set(actual_files) & set(reference_files))
    missing_actual = sorted(set(reference_files) - set(actual_files))
    missing_reference = sorted(set(actual_files) - set(reference_files))
    if missing_actual:
        raise ManifestError(f"{case.name}: missing actual frames: {', '.join(missing_actual)}")
    if missing_reference:
        raise ManifestError(
            f"{case.name}: missing reference frames: {', '.join(missing_reference)}"
        )
    if not names:
        raise ManifestError(f"{case.name}: no matching sequence frames")

    results = [
        compare_ppm.compare_ppm_files(
            actual_files[name], reference_files[name], case.pixel_threshold
        )
        for name in names
    ]
    if case.write_diff:
        case.write_diff.mkdir(parents=True, exist_ok=True)
        for result in results:
            compare_ppm.write_ppm(
                case.write_diff / result.name, result.width, result.height, result.diff_pixels
            )
    failed = any(
        compare_ppm.should_fail(
            result, case.max_changed_pixels, case.max_channel_delta
        )
        for result in results
    )
    return failed, results


def compare_file_case(case: Case) -> tuple[bool, list[compare_ppm.ImageDiff]]:
    validate_capture_metadata(case, sequence=False)
    result = compare_ppm.compare_ppm_files(
        case.actual, case.reference, case.pixel_threshold
    )
    if case.write_diff:
        compare_ppm.write_ppm(
            case.write_diff, result.width, result.height, result.diff_pixels
        )
    failed = compare_ppm.should_fail(
        result, case.max_changed_pixels, case.max_channel_delta
    )
    return failed, [result]


def print_case_summary(case: Case, failed: bool, results: list[compare_ppm.ImageDiff]) -> None:
    changed_pixels = sum(result.changed_pixels for result in results)
    changed_channels = sum(result.changed_channels for result in results)
    pixels = sum(result.pixels for result in results)
    max_delta = max((result.max_channel_delta for result in results), default=0)
    worst_rmse = max((result.rmse for result in results), default=0.0)
    changed_pct = changed_pixels * 100.0 / pixels if pixels else 0.0
    status = "FAIL" if failed else "PASS"
    print(
        f"{status} {case.name}: frames={len(results)} changed_pixels={changed_pixels} "
        f"({changed_pct:.4f}%) changed_channels={changed_channels} "
        f"max_channel_delta={max_delta} worst_rmse={worst_rmse:.4f}"
    )


def run_case(case: Case) -> bool:
    if case.actual.is_dir() or case.reference.is_dir():
        if not case.actual.is_dir() or not case.reference.is_dir():
            raise ManifestError(
                f"{case.name}: actual and reference must both be files or both be directories"
            )
        failed, results = compare_directory_case(case)
    else:
        failed, results = compare_file_case(case)
    print_case_summary(case, failed, results)
    return failed


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Run JSON visual comparison manifests.")
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args(argv)

    try:
        cases = load_manifest(args.manifest)
        failures = 0
        for case in cases:
            if run_case(case):
                failures += 1
    except (OSError, compare_ppm.PpmError, ManifestError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    print(f"summary: cases={len(cases)} failures={failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

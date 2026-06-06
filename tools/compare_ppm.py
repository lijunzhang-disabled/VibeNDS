#!/usr/bin/env python3
"""Compare binary PPM (P6) captures.

This is intentionally dependency-free so it can be used on captures emitted by
`nds-frontend --capture-ppm` or `--capture-dir` without installing image
libraries. It compares either two files or two capture directories, reports
channel/pixel deltas, and can optionally write amplified diff PPMs for visual
inspection.
"""

from __future__ import annotations

import argparse
import math
import re
import sys
from dataclasses import dataclass
from pathlib import Path


class PpmError(Exception):
    pass


@dataclass(frozen=True)
class ImageDiff:
    name: str
    width: int
    height: int
    pixels: int
    changed_pixels: int
    changed_channels: int
    max_channel_delta: int
    rmse: float
    diff_pixels: bytes


def _read_token(data: bytes, pos: int) -> tuple[str, int]:
    n = len(data)
    while pos < n:
        b = data[pos]
        if b == ord("#"):
            while pos < n and data[pos] not in b"\r\n":
                pos += 1
        elif chr(b).isspace():
            pos += 1
        else:
            break
    if pos >= n:
        raise PpmError("unexpected end of PPM header")

    start = pos
    while pos < n and not chr(data[pos]).isspace() and data[pos] != ord("#"):
        pos += 1
    return data[start:pos].decode("ascii"), pos


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    data = path.read_bytes()
    pos = 0
    magic, pos = _read_token(data, pos)
    if magic != "P6":
        raise PpmError(f"{path}: expected P6 PPM, got {magic!r}")

    width_s, pos = _read_token(data, pos)
    height_s, pos = _read_token(data, pos)
    maxval_s, pos = _read_token(data, pos)
    try:
        width = int(width_s)
        height = int(height_s)
        maxval = int(maxval_s)
    except ValueError as e:
        raise PpmError(f"{path}: invalid PPM dimensions/header") from e
    if width <= 0 or height <= 0:
        raise PpmError(f"{path}: invalid PPM size {width}x{height}")
    if maxval != 255:
        raise PpmError(f"{path}: only maxval 255 PPMs are supported, got {maxval}")

    if pos >= len(data) or not chr(data[pos]).isspace():
        raise PpmError(f"{path}: missing whitespace after PPM header")
    pos += 1

    expected = width * height * 3
    pixels = data[pos:]
    if len(pixels) != expected:
        raise PpmError(
            f"{path}: expected {expected} pixel bytes for {width}x{height}, got {len(pixels)}"
        )
    return width, height, pixels


def write_ppm(path: Path, width: int, height: int, pixels: bytes) -> None:
    path.write_bytes(f"P6\n{width} {height}\n255\n".encode("ascii") + pixels)


def compare_pixels(
    actual: bytes,
    reference: bytes,
    pixel_threshold: int,
) -> tuple[int, int, int, int, float, bytes]:
    max_channel_delta = 0
    changed_channels = 0
    changed_pixels = 0
    sum_sq = 0
    diff = bytearray(len(actual))

    for pixel_start in range(0, len(actual), 3):
        pixel_max = 0
        for channel in range(3):
            i = pixel_start + channel
            delta = abs(actual[i] - reference[i])
            pixel_max = max(pixel_max, delta)
            max_channel_delta = max(max_channel_delta, delta)
            if delta != 0:
                changed_channels += 1
            sum_sq += delta * delta
            diff[i] = min(255, delta * 8)
        if pixel_max > pixel_threshold:
            changed_pixels += 1

    pixel_count = len(actual) // 3
    rmse = math.sqrt(sum_sq / len(actual)) if actual else 0.0
    return pixel_count, changed_pixels, changed_channels, max_channel_delta, rmse, bytes(diff)


def compare_ppm_files(actual: Path, reference: Path, pixel_threshold: int) -> ImageDiff:
    actual_w, actual_h, actual_pixels = read_ppm(actual)
    reference_w, reference_h, reference_pixels = read_ppm(reference)
    if (actual_w, actual_h) != (reference_w, reference_h):
        raise PpmError(
            "image dimensions differ: "
            f"actual={actual_w}x{actual_h} reference={reference_w}x{reference_h}"
        )

    pixel_count, changed_pixels, changed_channels, max_delta, rmse, diff = compare_pixels(
        actual_pixels, reference_pixels, pixel_threshold
    )
    return ImageDiff(
        name=actual.name,
        width=actual_w,
        height=actual_h,
        pixels=pixel_count,
        changed_pixels=changed_pixels,
        changed_channels=changed_channels,
        max_channel_delta=max_delta,
        rmse=rmse,
        diff_pixels=diff,
    )


def print_single_result(result: ImageDiff) -> None:
    changed_pct = (result.changed_pixels * 100.0 / result.pixels) if result.pixels else 0.0
    print(f"size: {result.width}x{result.height}")
    print(f"pixels: {result.pixels}")
    print(f"changed_pixels: {result.changed_pixels} ({changed_pct:.4f}%)")
    print(f"changed_channels: {result.changed_channels}")
    print(f"max_channel_delta: {result.max_channel_delta}")
    print(f"rmse: {result.rmse:.4f}")


def print_sequence_results(results: list[ImageDiff]) -> None:
    print("frame,changed_pixels,changed_pct,changed_channels,max_channel_delta,rmse")
    total_pixels = 0
    total_changed_pixels = 0
    total_changed_channels = 0
    total_squared_error = 0.0
    max_delta = 0

    for result in results:
        changed_pct = (result.changed_pixels * 100.0 / result.pixels) if result.pixels else 0.0
        print(
            f"{result.name},{result.changed_pixels},{changed_pct:.4f},"
            f"{result.changed_channels},{result.max_channel_delta},{result.rmse:.4f}"
        )
        total_pixels += result.pixels
        total_changed_pixels += result.changed_pixels
        total_changed_channels += result.changed_channels
        total_squared_error += result.rmse * result.rmse * result.pixels * 3
        max_delta = max(max_delta, result.max_channel_delta)

    channel_count = total_pixels * 3
    aggregate_rmse = math.sqrt(total_squared_error / channel_count) if channel_count else 0.0
    changed_pct = (
        total_changed_pixels * 100.0 / total_pixels if total_pixels else 0.0
    )
    print("summary:")
    print(f"frames: {len(results)}")
    print(f"pixels: {total_pixels}")
    print(f"changed_pixels: {total_changed_pixels} ({changed_pct:.4f}%)")
    print(f"changed_channels: {total_changed_channels}")
    print(f"max_channel_delta: {max_delta}")
    print(f"rmse: {aggregate_rmse:.4f}")


def ppm_files_by_name(path: Path) -> dict[str, Path]:
    frame_name = re.compile(r"^frame-\d{6}\.ppm$")
    return {
        p.name: p
        for p in sorted(path.glob("frame-*.ppm"))
        if p.is_file() and frame_name.match(p.name)
    }


def should_fail(result: ImageDiff, max_changed_pixels: int, max_channel_delta: int) -> bool:
    return (
        result.changed_pixels > max_changed_pixels
        or result.max_channel_delta > max_channel_delta
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Compare P6 PPM capture files or capture directories."
    )
    parser.add_argument("actual", type=Path)
    parser.add_argument("reference", type=Path)
    parser.add_argument(
        "--pixel-threshold",
        type=int,
        default=0,
        help="count a pixel as changed only if any channel delta exceeds this value",
    )
    parser.add_argument(
        "--max-changed-pixels",
        type=int,
        default=0,
        help="exit nonzero if changed pixels exceed this count",
    )
    parser.add_argument(
        "--max-channel-delta",
        type=int,
        default=0,
        help="exit nonzero if any single channel delta exceeds this value",
    )
    parser.add_argument(
        "--write-diff",
        type=Path,
        help="write an amplified diff PPM, or a diff directory for directory comparisons",
    )
    args = parser.parse_args(argv)

    if args.pixel_threshold < 0 or args.max_changed_pixels < 0 or args.max_channel_delta < 0:
        parser.error("thresholds must be non-negative")

    if args.actual.is_dir() or args.reference.is_dir():
        if not args.actual.is_dir() or not args.reference.is_dir():
            print("error: actual and reference must both be files or both be directories", file=sys.stderr)
            return 2

        actual_files = ppm_files_by_name(args.actual)
        reference_files = ppm_files_by_name(args.reference)
        names = sorted(set(actual_files) & set(reference_files))
        missing_actual = sorted(set(reference_files) - set(actual_files))
        missing_reference = sorted(set(actual_files) - set(reference_files))
        if missing_actual or missing_reference:
            if missing_actual:
                print(f"error: missing actual frames: {', '.join(missing_actual)}", file=sys.stderr)
            if missing_reference:
                print(
                    f"error: missing reference frames: {', '.join(missing_reference)}",
                    file=sys.stderr,
                )
            return 2
        if not names:
            print("error: no matching .ppm frames found", file=sys.stderr)
            return 2

        try:
            results = [
                compare_ppm_files(actual_files[name], reference_files[name], args.pixel_threshold)
                for name in names
            ]
        except (OSError, PpmError) as e:
            print(f"error: {e}", file=sys.stderr)
            return 2

        if args.write_diff:
            try:
                args.write_diff.mkdir(parents=True, exist_ok=True)
                for result in results:
                    write_ppm(
                        args.write_diff / result.name,
                        result.width,
                        result.height,
                        result.diff_pixels,
                    )
            except OSError as e:
                print(f"error: {e}", file=sys.stderr)
                return 2

        print_sequence_results(results)
        return 1 if any(
            should_fail(result, args.max_changed_pixels, args.max_channel_delta)
            for result in results
        ) else 0

    try:
        result = compare_ppm_files(args.actual, args.reference, args.pixel_threshold)
    except (OSError, PpmError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    if args.write_diff:
        try:
            write_ppm(args.write_diff, result.width, result.height, result.diff_pixels)
        except OSError as e:
            print(f"error: {e}", file=sys.stderr)
            return 2

    print_single_result(result)
    return 1 if should_fail(result, args.max_changed_pixels, args.max_channel_delta) else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

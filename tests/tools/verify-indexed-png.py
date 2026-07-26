#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 James Seymour-Lock / Project Prism
"""Verify pngquant smoke outputs as complete, non-interlaced indexed PNGs."""

from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def fail(path: Path, message: str) -> None:
    raise ValueError(f"{path}: {message}")


def verify(path: Path) -> None:
    data = path.read_bytes()
    if not data.startswith(PNG_SIGNATURE):
        fail(path, "missing PNG signature")

    offset = len(PNG_SIGNATURE)
    chunks: list[tuple[bytes, bytes]] = []
    while offset < len(data):
        if len(data) - offset < 12:
            fail(path, "truncated chunk framing")
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4 : offset + 8]
        payload_start = offset + 8
        payload_end = payload_start + length
        crc_end = payload_end + 4
        if crc_end > len(data):
            fail(path, f"truncated {kind!r} chunk")
        payload = data[payload_start:payload_end]
        expected_crc = struct.unpack_from(">I", data, payload_end)[0]
        actual_crc = zlib.crc32(kind + payload) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            fail(path, f"bad CRC for {kind.decode('ascii', 'replace')}")
        chunks.append((kind, payload))
        offset = crc_end

    if offset != len(data) or not chunks or chunks[0][0] != b"IHDR":
        fail(path, "IHDR must be the first complete chunk")
    if chunks[-1] != (b"IEND", b""):
        fail(path, "IEND must be the final zero-length chunk")

    ihdr = chunks[0][1]
    if len(ihdr) != 13:
        fail(path, "IHDR must have 13 bytes")
    width, height, bit_depth, color_type, compression, filter_method, interlace = struct.unpack(
        ">IIBBBBB", ihdr
    )
    if not width or not height:
        fail(path, "zero image dimension")
    if color_type != 3 or bit_depth not in (1, 2, 4, 8):
        fail(path, "not indexed-color PNG data")
    if compression != 0 or filter_method != 0 or interlace != 0:
        fail(path, "unsupported PNG method or interlacing")

    names = [kind for kind, _ in chunks]
    if names.count(b"IHDR") != 1 or names.count(b"IEND") != 1:
        fail(path, "duplicate mandatory PNG chunk")
    try:
        palette_index = names.index(b"PLTE")
        first_idat = names.index(b"IDAT")
    except ValueError:
        fail(path, "indexed PNG needs PLTE and IDAT")
    palette = chunks[palette_index][1]
    if not (3 <= len(palette) <= 768 and len(palette) % 3 == 0):
        fail(path, "invalid PLTE length")
    if palette_index > first_idat:
        fail(path, "PLTE must precede IDAT")

    compressed = b"".join(payload for kind, payload in chunks if kind == b"IDAT")
    try:
        scanlines = zlib.decompress(compressed)
    except zlib.error as error:
        fail(path, f"invalid IDAT deflate stream: {error}")
    row_bytes = (width * bit_depth + 7) // 8
    expected_size = height * (row_bytes + 1)
    if len(scanlines) != expected_size:
        fail(path, f"decoded scanlines have {len(scanlines)} bytes, expected {expected_size}")
    for row in range(height):
        if scanlines[row * (row_bytes + 1)] > 4:
            fail(path, f"invalid filter byte in row {row}")

    print(f"{path}: indexed PNG {width}x{height}, {bit_depth}-bit palette, {len(palette) // 3} entries")


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: verify-indexed-png.py OUTPUT.png [... ]", file=sys.stderr)
        return 2
    try:
        for raw_path in argv:
            verify(Path(raw_path))
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

#!/usr/bin/env python3
"""Deterministic edge-case PNG corpus generator (T-0201, v0.4 release hardening).

Builds a fixed set of *release-binary* edge-case fixtures for the robustness
gate in `tests/edge_corpus.rs`. Unlike the ch17 §31 adversarial corpus
(`tests/adversarial/`, T-0110) — which probes the in-process `decode_png`
library API against malformed input — this corpus is driven through the
compiled `pngprism` binary end to end and covers BOTH:

  * VALID edge geometries/formats that must produce a clean, correct indexed
    PNG (exit 0): 1x1, 1xN, Nx1, 16-bit, Adam7-interlaced, fully-transparent,
    single-color, palette-with-short-tRNS, gray+alpha, 2-color palette; and
  * MALFORMED inputs that must produce a clean nonzero-exit error, never a
    panic or a hang: random bytes, truncated streams, bad CRC, 0x0 dims,
    absurd (pixel-capped-before-allocation) IHDR dims, empty file.

Python standard library only (`struct`, `zlib`) — same convention as the
`parity/` and adversarial generators. This is a generator, not a fuzzer:
re-running reproduces byte-identical output. `--check` verifies the committed
corpus matches this generator byte-for-byte; the Rust suite and the release
gate both call it in `--check` mode so the fixtures can never silently drift.

A committed `manifest.tsv` records, per fixture, the expected outcome class
(`valid`/`bad`) and, for valid fixtures, the source width/height the output
must preserve — so the Rust test needs no PNG decoder of the input itself.

Usage:
    python3 tests/edge/generate_edge_corpus.py [--check]
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS_DIR = HERE / "corpus"
MANIFEST = HERE / "manifest.tsv"

SIG = b"\x89PNG\r\n\x1a\n"

# Adam7 interlace passes: (x0, y0, dx, dy). Mirrors src/png.rs ADAM7_PASSES.
ADAM7 = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
]


def chunk(kind: bytes, payload: bytes) -> bytes:
    assert len(kind) == 4
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    )


def ihdr_payload(
    width: int,
    height: int,
    bit_depth: int,
    color_type: int,
    interlace: int = 0,
) -> bytes:
    return struct.pack(
        ">IIBBBBB", width, height, bit_depth, color_type, 0, 0, interlace
    )


def png(
    ihdr: bytes,
    idat: bytes,
    *,
    plte: bytes | None = None,
    trns: bytes | None = None,
) -> bytes:
    out = bytearray(SIG)
    out += chunk(b"IHDR", ihdr)
    if plte is not None:
        out += chunk(b"PLTE", plte)
    if trns is not None:
        out += chunk(b"tRNS", trns)
    out += chunk(b"IDAT", idat)
    out += chunk(b"IEND", b"")
    return bytes(out)


def rgb_pixel(x: int, y: int) -> bytes:
    """A deterministic, structured RGB triple (varies per position)."""
    return bytes([(x * 37 + y * 11) & 0xFF, (x * 5 + 90) & 0xFF, (y * 19 + 3) & 0xFF])


# --- non-interlaced filter-0 encoders --------------------------------------


def rgb8_scanlines(width: int, height: int) -> bytes:
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter type 0
        for x in range(width):
            rows += rgb_pixel(x, y)
    return bytes(rows)


def solid_rgb8_scanlines(width: int, height: int, rgb: tuple[int, int, int]) -> bytes:
    rows = bytearray()
    row = bytes(rgb) * width
    for _ in range(height):
        rows.append(0)
        rows += row
    return bytes(rows)


def rgba8_scanlines(
    width: int, height: int, rgba: tuple[int, int, int, int]
) -> bytes:
    rows = bytearray()
    row = bytes(rgba) * width
    for _ in range(height):
        rows.append(0)
        rows += row
    return bytes(rows)


def rgb16_scanlines(width: int, height: int) -> bytes:
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            for chan in range(3):
                v = ((x * 4099 + y * 2053 + chan * 8191) & 0xFFFF)
                rows += v.to_bytes(2, "big")
    return bytes(rows)


def gray16_scanlines(width: int, height: int) -> bytes:
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            v = ((x * 5501 + y * 3301) & 0xFFFF)
            rows += v.to_bytes(2, "big")
    return bytes(rows)


def grayalpha8_scanlines(width: int, height: int) -> bytes:
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows += bytes([(x * 23 + y * 7) & 0xFF, (255 - ((x + y) * 9)) & 0xFF])
    return bytes(rows)


def palette8_scanlines(width: int, height: int, num_entries: int) -> bytes:
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.append((x + y) % num_entries)
    return bytes(rows)


# --- Adam7 interlaced filter-0 encoder -------------------------------------


def adam7_rgb8_scanlines(width: int, height: int) -> bytes:
    """Filter-0 Adam7 interlaced scanlines for an 8-bit RGB image."""
    out = bytearray()
    for (x0, y0, dx, dy) in ADAM7:
        pw = (width - x0 + dx - 1) // dx if width > x0 else 0
        ph = (height - y0 + dy - 1) // dy if height > y0 else 0
        if pw == 0 or ph == 0:
            continue
        for py in range(ph):
            out.append(0)  # filter type 0
            y = y0 + py * dy
            for px in range(pw):
                x = x0 + px * dx
                out += rgb_pixel(x, y)
    return bytes(out)


# --- fixtures ---------------------------------------------------------------


def valid_fixtures() -> dict[str, tuple[bytes, int, int]]:
    """name -> (bytes, width, height). All must decode + quantize cleanly."""
    out: dict[str, tuple[bytes, int, int]] = {}

    out["valid-1x1-rgb.png"] = (
        png(ihdr_payload(1, 1, 8, 2), zlib.compress(rgb8_scanlines(1, 1), 9)),
        1,
        1,
    )
    out["valid-1xN-rgb.png"] = (
        png(ihdr_payload(1, 32, 8, 2), zlib.compress(rgb8_scanlines(1, 32), 9)),
        1,
        32,
    )
    out["valid-Nx1-rgb.png"] = (
        png(ihdr_payload(32, 1, 8, 2), zlib.compress(rgb8_scanlines(32, 1), 9)),
        32,
        1,
    )
    out["valid-16bit-rgb.png"] = (
        png(ihdr_payload(6, 5, 16, 2), zlib.compress(rgb16_scanlines(6, 5), 9)),
        6,
        5,
    )
    out["valid-16bit-gray.png"] = (
        png(ihdr_payload(7, 4, 16, 0), zlib.compress(gray16_scanlines(7, 4), 9)),
        7,
        4,
    )
    out["valid-interlaced-rgb.png"] = (
        png(
            ihdr_payload(8, 8, 8, 2, interlace=1),
            zlib.compress(adam7_rgb8_scanlines(8, 8), 9),
        ),
        8,
        8,
    )
    out["valid-fully-transparent-rgba.png"] = (
        png(
            ihdr_payload(6, 6, 8, 6),
            zlib.compress(rgba8_scanlines(6, 6, (17, 34, 51, 0)), 9),
        ),
        6,
        6,
    )
    out["valid-single-color-rgb.png"] = (
        png(
            ihdr_payload(12, 9, 8, 2),
            zlib.compress(solid_rgb8_scanlines(12, 9, (200, 40, 90)), 9),
        ),
        12,
        9,
    )
    # Palette with tRNS SHORTER than the palette: a spec-valid construction —
    # entries beyond the tRNS length default to alpha 255.
    plte4 = bytes([10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])  # 4 entries
    out["valid-palette-short-trns.png"] = (
        png(
            ihdr_payload(8, 8, 8, 3),
            zlib.compress(palette8_scanlines(8, 8, 4), 9),
            plte=plte4,
            trns=bytes([0, 128]),  # only entries 0,1 carry alpha; 2,3 -> 255
        ),
        8,
        8,
    )
    out["valid-gray-alpha.png"] = (
        png(
            ihdr_payload(5, 5, 8, 4),
            zlib.compress(grayalpha8_scanlines(5, 5), 9),
        ),
        5,
        5,
    )
    plte2 = bytes([0, 0, 0, 255, 255, 255])  # 2 entries
    out["valid-2color-palette.png"] = (
        png(
            ihdr_payload(10, 10, 8, 3),
            zlib.compress(palette8_scanlines(10, 10, 2), 9),
            plte=plte2,
        ),
        10,
        10,
    )
    return out


def bad_fixtures() -> dict[str, bytes]:
    """name -> bytes. Every one must produce a clean nonzero exit (no panic,
    no hang)."""
    out: dict[str, bytes] = {}

    # Deterministic pseudo-random bytes (fixed LCG seed) with NO PNG signature.
    def lcg_bytes(seed: int, n: int) -> bytes:
        buf = bytearray()
        s = seed & 0xFFFFFFFF
        for _ in range(n):
            s = (1103515245 * s + 12345) & 0xFFFFFFFF
            buf.append((s >> 16) & 0xFF)
        return bytes(buf)

    out["bad-random-bytes.png"] = lcg_bytes(0xC0FFEE, 512)
    # Valid signature followed by deterministic garbage (exercises the chunk
    # framing / CRC / data-error path rather than the signature check).
    out["bad-random-after-signature.png"] = SIG + lcg_bytes(0xBADBEEF, 504)

    out["bad-empty.png"] = b""

    # A genuinely valid small PNG, then several truncations of it.
    good = png(ihdr_payload(4, 4, 8, 2), zlib.compress(rgb8_scanlines(4, 4), 9))
    out["bad-truncated-after-signature.png"] = good[: len(SIG)]
    out["bad-truncated-mid-idat.png"] = good[: len(good) - 12]

    # Bad CRC: flip one byte inside the IHDR CRC (last byte of the IHDR chunk).
    ihdr_end = len(SIG) + 8 + 13 + 4  # sig + len+kind + payload + crc
    corrupt = bytearray(good)
    corrupt[ihdr_end - 1] ^= 0xFF
    out["bad-crc-ihdr.png"] = bytes(corrupt)

    # 0x0 dimensions (rejected in parse_ihdr before any allocation).
    out["bad-zero-dims.png"] = png(
        ihdr_payload(0, 0, 8, 2), zlib.compress(b"", 9)
    )

    # Absurd IHDR dims with a tiny IDAT: the decoder computes the expected
    # scanline byte count (u128) and rejects on the length check BEFORE it ever
    # allocates a width*height pixel buffer — the pixel-cap-before-allocation
    # guarantee. Must fail fast, not OOM or hang.
    out["bad-absurd-dims-width.png"] = png(
        ihdr_payload(0x7FFFFFFF, 1, 8, 2), zlib.compress(b"", 9)
    )
    out["bad-absurd-dims-both.png"] = png(
        ihdr_payload(0x7FFFFFFF, 0x7FFFFFFF, 8, 2), zlib.compress(b"", 9)
    )

    return out


def all_files() -> tuple[dict[str, bytes], list[tuple[str, str, int, int]]]:
    files: dict[str, bytes] = {}
    manifest: list[tuple[str, str, int, int]] = []
    for name, (data, w, h) in sorted(valid_fixtures().items()):
        files[name] = data
        manifest.append((name, "valid", w, h))
    for name, data in sorted(bad_fixtures().items()):
        files[name] = data
        manifest.append((name, "bad", 0, 0))
    manifest.sort()
    return files, manifest


def render_manifest(rows: list[tuple[str, str, int, int]]) -> str:
    lines = ["# name\tclass\twidth\theight  (T-0201 edge corpus; generated)"]
    for name, cls, w, h in rows:
        lines.append(f"{name}\t{cls}\t{w}\t{h}")
    return "\n".join(lines) + "\n"


def write_all(check: bool) -> int:
    CORPUS_DIR.mkdir(parents=True, exist_ok=True)
    files, manifest = all_files()
    manifest_text = render_manifest(manifest)

    if check:
        mismatches = []
        for name, data in sorted(files.items()):
            path = CORPUS_DIR / name
            if not path.is_file() or path.read_bytes() != data:
                mismatches.append(name)
        if not MANIFEST.is_file() or MANIFEST.read_text() != manifest_text:
            mismatches.append("manifest.tsv")
        # Stray fixtures (removed from the generator but left on disk) are drift.
        expected_names = set(files)
        for path in CORPUS_DIR.glob("*.png"):
            if path.name not in expected_names:
                mismatches.append(f"{path.name} (stray)")
        if mismatches:
            print(
                f"CHECK FAILED: {len(mismatches)} item(s) differ from generator:",
                file=sys.stderr,
            )
            for name in sorted(mismatches):
                print(f"  {name}", file=sys.stderr)
            return 1
        print(f"CHECK OK: {len(files)} fixtures + manifest match byte-for-byte.")
        return 0

    for name, data in sorted(files.items()):
        (CORPUS_DIR / name).write_bytes(data)
    MANIFEST.write_text(manifest_text)
    print(f"wrote {len(files)} fixtures + manifest.tsv to {CORPUS_DIR}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed corpus matches this generator; write nothing",
    )
    args = parser.parse_args()
    return write_all(check=args.check)


if __name__ == "__main__":
    raise SystemExit(main())

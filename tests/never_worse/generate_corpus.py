#!/usr/bin/env python3
"""Deterministic never-worse differential fixtures (T-0210, item 1).

Three PNG inputs, each chosen to exercise a distinct EXCEPTION clause of the
never-worse output guarantee (not the happy path): for each, the engine's
encoded indexed-PNG output is >= the input file's bytes, so the CLI must
emit the input bytes verbatim and report the fallback. The differential
test (`never_worse.rs` / `test_prism_quant.py`) asserts BOTH implementations
trip the gate identically on these shared fixtures.

  tiny.png            — a 1x1 truecolor+alpha PNG. PNG per-file overhead
                        (IHDR + PLTE + a palette-image IDAT + IEND) dominates
                        a 1-pixel image, so the indexed re-encode cannot be
                        smaller. (Clause: tiny input.)
  incompressible.png  — a 4x4 truecolor+alpha field of high-entropy
                        deterministic noise: 16 distinct colors in 16 pixels,
                        so the field does not compress and an indexed re-encode
                        (palette + indices + overhead) is >= the input.
                        (Clause: incompressible input.)
  already-palette.png — a FROZEN engine artifact: an already-optimized indexed
                        (color_type 3) PNG the engine itself emitted, so
                        re-quantizing + re-emitting it is idempotent (encoded
                        output == input, the gate trips). This is the honest
                        "already a palette image" case — a hand-crafted indexed
                        PNG is not, because the engine can out-pack a naive
                        stdlib encoding. It is committed as bytes and NOT
                        rebuilt by this generator; `--check` structurally
                        verifies it is a valid indexed PNG, and the differential
                        test is its behavioral guard (both impls must trip on
                        it). (Clause: already-palette input.)

The two synthesized fixtures are stdlib only (zlib for the exact IDAT);
`--check` re-derives their bytes and diffs them against the committed files
(drift guard), matching the adversarial / edge / fuzz generators' contract.
Their bytes are a frozen INPUT contract independent of the engine under test.

Usage:
  python3 generate_corpus.py            # (re)write corpus/*.png
  python3 generate_corpus.py --check    # verify committed bytes match
"""
from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS = HERE / "corpus"

PNG_SIG = b"\x89PNG\r\n\x1a\n"


def _chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def _idat(scanlines: bytes) -> bytes:
    # Level-9 single-stream IDAT (matches the engine's DEFLATE params family;
    # the exact bytes need not match — these are INPUT fixtures).
    return _chunk(b"IDAT", zlib.compress(scanlines, 9))


def _png_truecolor_alpha(width: int, height: int, pixels: list[tuple[int, int, int, int]]) -> bytes:
    """color_type 6 (RGBA), bit depth 8, filter 0 on every scanline."""
    assert len(pixels) == width * height
    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter: None
        for x in range(width):
            r, g, b, a = pixels[y * width + x]
            raw += bytes((r, g, b, a))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return PNG_SIG + _chunk(b"IHDR", ihdr) + _idat(bytes(raw)) + _chunk(b"IEND", b"")


def _png_indexed(width: int, height: int, palette: list[tuple[int, int, int]], indices: list[int]) -> bytes:
    """color_type 3 (indexed), bit depth 8, filter 0 on every scanline."""
    assert len(indices) == width * height
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            raw.append(indices[y * width + x])
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 3, 0, 0, 0)
    plte = b"".join(struct.pack(">BBB", *c) for c in palette)
    return (
        PNG_SIG
        + _chunk(b"IHDR", ihdr)
        + _chunk(b"PLTE", plte)
        + _idat(bytes(raw))
        + _chunk(b"IEND", b"")
    )


def _lcg(seed: int):
    """Deterministic stdlib PRNG (numerically reproducible everywhere)."""
    state = seed & 0xFFFFFFFF
    while True:
        state = (1664525 * state + 1013904223) & 0xFFFFFFFF
        yield state


def build_tiny() -> bytes:
    return _png_truecolor_alpha(1, 1, [(10, 20, 30, 255)])


def build_incompressible() -> bytes:
    # 4x4 = 16 RGBA pixels of deterministic high-entropy noise (16 distinct
    # colors). The field does not compress, and a palette + index re-encode is
    # >= this already-small truecolor input, so the never-worse gate trips.
    rng = _lcg(0xC0FFEE)
    pixels = []
    for _ in range(4 * 4):
        r = next(rng) & 0xFF
        g = (next(rng) >> 3) & 0xFF
        b = (next(rng) >> 5) & 0xFF
        a = 255 if (next(rng) & 3) else 128
        pixels.append((r, g, b, a))
    return _png_truecolor_alpha(4, 4, pixels)


# Synthesized, stdlib-rebuildable fixtures (drift-guarded by --check).
SYNTH_FIXTURES = {
    "tiny.png": build_tiny,
    "incompressible.png": build_incompressible,
}
# Frozen engine artifact: rebuilt only via the engine (see docstring), so it
# is byte-committed and structurally checked here, never regenerated.
FROZEN_INDEXED = "already-palette.png"


def _is_indexed_png(data: bytes) -> bool:
    if data[:8] != PNG_SIG or data[12:16] != b"IHDR":
        return False
    # IHDR color type is the 25th byte of the IHDR data (offset 8+8+9).
    return data[8 + 8 + 9] == 3


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="verify committed bytes")
    args = ap.parse_args()

    CORPUS.mkdir(parents=True, exist_ok=True)
    drift = []
    for name, builder in SYNTH_FIXTURES.items():
        want = builder()
        path = CORPUS / name
        if args.check:
            have = path.read_bytes() if path.exists() else b""
            if have != want:
                drift.append(name)
        else:
            path.write_bytes(want)
            print(f"WROTE {path} ({len(want)} bytes)")

    frozen = CORPUS / FROZEN_INDEXED
    if not frozen.exists():
        print(f"MISSING frozen engine artifact: {frozen}", file=sys.stderr)
        return 1
    if not _is_indexed_png(frozen.read_bytes()):
        print(f"NOT an indexed PNG: {frozen}", file=sys.stderr)
        drift.append(FROZEN_INDEXED)

    if args.check:
        if drift:
            print(f"DRIFT: {drift}", file=sys.stderr)
            return 1
        print(
            f"OK: {len(SYNTH_FIXTURES)} synthesized fixtures match committed "
            f"bytes; {FROZEN_INDEXED} is a valid indexed PNG"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""T-0212 reproducer generator: the T-0207 inflate-amplification finding.

Builds ONE deterministic PNG file that triggers the decompression-
amplification property T-0207 observed and its reviewer (claude-sonnet-46)
independently reproduced: `inflate` in `src/png.rs` used to materialise the
FULL IDAT output before checking it against the IHDR-declared expected
scanline-byte total, so a maximally-compressible IDAT could inflate to
~1000x its compressed size before being cleanly rejected.

Recipe (verbatim from the T-0207 task file's review section, reproduced
independently here rather than copied byte-for-byte from any script the
implementer or reviewer used):
  - IHDR: 8x8, bit depth 8, color type 2 (RGB8) -> expected scanline bytes
    = height * (1 + width*3) = 8 * (1 + 24) = 200.
  - IDAT: zlib level-9 compression of 64 MiB of zero bytes (NOT 200 bytes
    of real scanline data -- the whole point is the compressed stream
    claims to decode to far more than IHDR's 200-byte budget).
  - No PLTE/tRNS; single IDAT chunk; well-formed IEND.

This is deliberately NOT a valid image (its decompressed content does not
correspond to 8x8 real scanline data) -- it must always end in a clean
`data_error`, on both the pre-fix and post-fix decoder. What changes with
the T-0212 fix is how much memory / how many scratch-buffer bytes the
decoder materialises before it reports that error, not whether it errors.

Python standard library only (`struct`, `zlib`), same convention as
`tests/edge/generate_edge_corpus.py` / `tests/adversarial/generate_corpus.py`.
Deterministic: re-running reproduces a byte-identical file. `--check`
verifies the committed fixture matches this generator byte-for-byte.

Usage:
    python3 tests/amplification/generate_repro.py [--check]
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS_DIR = HERE / "corpus"
FIXTURE = CORPUS_DIR / "bomb-8x8-rgb8-64mib-zeros.png"

SIG = b"\x89PNG\r\n\x1a\n"

WIDTH = 8
HEIGHT = 8
BIT_DEPTH = 8
COLOR_TYPE = 2  # RGB8
EXPECTED_SCANLINE_BYTES = HEIGHT * (1 + WIDTH * 3)  # 200
ZERO_PAYLOAD_BYTES = 64 * 1024 * 1024  # 64 MiB


def chunk(kind: bytes, payload: bytes) -> bytes:
    assert len(kind) == 4
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    )


def build() -> bytes:
    ihdr = struct.pack(
        ">IIBBBBB", WIDTH, HEIGHT, BIT_DEPTH, COLOR_TYPE, 0, 0, 0
    )
    # The IDAT payload is a valid zlib stream (correct Adler-32 trailer,
    # correct final-block framing) that decompresses to 64 MiB of zero
    # bytes -- nothing to do with real 8x8 scanline data. That is what
    # makes it "maximally compressible": 64 MiB of zeros compresses to a
    # few tens of KiB at level 9.
    idat = zlib.compress(b"\x00" * ZERO_PAYLOAD_BYTES, 9)
    out = bytearray(SIG)
    out += chunk(b"IHDR", ihdr)
    out += chunk(b"IDAT", idat)
    out += chunk(b"IEND", b"")
    return bytes(out)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    data = build()
    print(
        f"built {len(data)}-byte PNG: IHDR {WIDTH}x{HEIGHT} color_type={COLOR_TYPE} "
        f"expected_scanline_bytes={EXPECTED_SCANLINE_BYTES} "
        f"idat_decompresses_to={ZERO_PAYLOAD_BYTES} "
        f"ratio={ZERO_PAYLOAD_BYTES / EXPECTED_SCANLINE_BYTES:.1f}x",
        file=sys.stderr,
    )

    if args.check:
        if not FIXTURE.is_file():
            print(f"CHECK FAILED: {FIXTURE} missing", file=sys.stderr)
            return 1
        existing = FIXTURE.read_bytes()
        if existing != data:
            print(
                f"CHECK FAILED: {FIXTURE} does not match generator output "
                f"({len(existing)} vs {len(data)} bytes)",
                file=sys.stderr,
            )
            return 1
        print("CHECK OK: fixture matches generator byte-for-byte.", file=sys.stderr)
        return 0

    CORPUS_DIR.mkdir(parents=True, exist_ok=True)
    FIXTURE.write_bytes(data)
    print(f"wrote {FIXTURE}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

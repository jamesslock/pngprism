#!/usr/bin/env python3
"""Deterministic adversarial/malformed-PNG corpus generator (T-0110, ch17 §31).

Builds a fixed set of structurally-invalid PNG fixtures plus one small valid
`seed.png` (used by the Rust suite's truncation and byte-flip sweeps at test
time, so those sweeps do not need thousands of committed variant files — the
sweep offsets themselves are a deterministic range/loop, not runtime
randomness). Python standard library only (`struct`, `zlib`), mirroring the
`parity/` and `datasets/conformance/pngsuite/verify.py` convention.

Every fixture here targets a SPECIFIC declared rejection reason in
`src/png.rs::decode_png` (see the filename), so a reviewer can map each file
to the check it exercises. This is a generator, not a fuzzer: re-running it
reproduces byte-identical output (`--check` verifies this).

Usage:
    python3 tests/adversarial/generate_corpus.py [--check]
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORPUS_DIR = HERE / "corpus"

SIG = b"\x89PNG\r\n\x1a\n"


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
    compression: int = 0,
    filter_method: int = 0,
    interlace: int = 0,
) -> bytes:
    return struct.pack(
        ">IIBBBBB", width, height, bit_depth, color_type, compression, filter_method, interlace
    )


def solid_scanlines(width: int, height: int, bit_depth: int, color_type: int, sample: int) -> bytes:
    """Filter-0 scanlines of one repeated sample value per channel."""
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]
    if bit_depth == 8:
        row = bytes([sample] * (width * channels))
    elif bit_depth == 16:
        row = (sample & 0xFFFF).to_bytes(2, "big") * (width * channels)
    else:
        # Sub-byte depths: pack `sample` (already masked to bit_depth) MSB
        # first, one channel implied (gray/palette only at these depths).
        per_byte = 8 // bit_depth
        mask = (1 << bit_depth) - 1
        s = sample & mask
        bits = []
        for _ in range(width * channels):
            bits.append(s)
        row = bytearray()
        for i in range(0, len(bits), per_byte):
            group = bits[i : i + per_byte]
            byte = 0
            shift = 8 - bit_depth
            for v in group:
                byte |= (v & mask) << shift
                shift -= bit_depth
            row.append(byte)
        row = bytes(row)
    return b"".join(b"\x00" + row for _ in range(height))


def build(
    *,
    width: int = 2,
    height: int = 2,
    bit_depth: int = 8,
    color_type: int = 2,
    compression: int = 0,
    filter_method: int = 0,
    interlace: int = 0,
    plte: bytes | None = None,
    trns: bytes | None = None,
    gama: bytes | None = None,
    iccp: bytes | None = None,
    extra_chunks: tuple[tuple[bytes, bytes], ...] = (),
    idat_payloads: list[bytes] | None = None,
    scanlines: bytes | None = None,
    include_ihdr: bool = True,
    ihdr_payload_override: bytes | None = None,
    include_iend: bool = True,
    trailing: bytes = b"",
    ihdr_first: bool = True,
    duplicate_ihdr: bool = False,
) -> bytes:
    """General-purpose malformed-PNG builder. Every default produces a
    genuinely valid tiny RGB image; each fixture flips exactly one knob away
    from valid so the failure is attributable."""
    out = bytearray(SIG)

    ihdr = ihdr_payload_override
    if ihdr is None:
        ihdr = ihdr_payload(width, height, bit_depth, color_type, compression, filter_method, interlace)
    ihdr_chunk = chunk(b"IHDR", ihdr)

    if idat_payloads is None:
        if scanlines is None:
            scanlines = solid_scanlines(width, height, bit_depth, color_type, sample=1)
        idat_payloads = [zlib.compress(scanlines, 9)]

    body = bytearray()
    if plte is not None:
        body += chunk(b"PLTE", plte)
    if gama is not None:
        body += chunk(b"gAMA", gama)
    if iccp is not None:
        body += chunk(b"iCCP", iccp)
    if trns is not None:
        body += chunk(b"tRNS", trns)
    for kind, payload in extra_chunks:
        body += chunk(kind, payload)
    for payload in idat_payloads:
        body += chunk(b"IDAT", payload)

    if ihdr_first:
        if include_ihdr:
            out += ihdr_chunk
        if duplicate_ihdr:
            out += ihdr_chunk
        out += body
    else:
        # Deliberately put a non-IHDR chunk first, THEN IHDR.
        out += body
        if include_ihdr:
            out += ihdr_chunk

    if include_iend:
        out += chunk(b"IEND", b"")
    out += trailing
    return bytes(out)


def valid_seed() -> bytes:
    """A genuinely valid tiny RGBA image: 4x4, color type 6, bit depth 8,
    non-interlaced, one gAMA ancillary chunk, a distinct byte per row so
    truncation/byte-flip offsets land in meaningfully different content."""
    width, height, color_type, bit_depth = 4, 4, 6, 8
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter type 0
        for x in range(width):
            rows += bytes([(x * 17 + y * 41) & 0xFF, (x * 53) & 0xFF, (y * 29) & 0xFF, 255 - ((x + y) & 0xFF)])
    idat = zlib.compress(bytes(rows), 9)
    out = bytearray(SIG)
    out += chunk(b"IHDR", ihdr_payload(width, height, bit_depth, color_type))
    out += chunk(b"gAMA", struct.pack(">I", 45455))
    out += chunk(b"IDAT", idat)
    out += chunk(b"IEND", b"")
    return bytes(out)


def corrupt_first_filter_byte(png: bytes, bad_filter: int) -> bytes:
    """Re-decompress the IDAT of a filter-0-only `build()` output, force the
    first scanline's filter-type byte to `bad_filter`, and re-emit with a
    fresh CRC (used for the invalid-row-filter-type fixture)."""
    # Locate + decompress the sole IDAT chunk.
    offset = len(SIG)
    idat_payload = None
    chunks = []
    while offset < len(png):
        length = struct.unpack(">I", png[offset : offset + 4])[0]
        kind = png[offset + 4 : offset + 8]
        payload = png[offset + 8 : offset + 8 + length]
        chunks.append((kind, payload))
        if kind == b"IDAT":
            idat_payload = payload
        offset += 12 + length
    assert idat_payload is not None
    raw = bytearray(zlib.decompress(idat_payload))
    raw[0] = bad_filter
    new_idat = zlib.compress(bytes(raw), 9)
    out = bytearray(SIG)
    for kind, payload in chunks:
        if kind == b"IDAT":
            out += chunk(b"IDAT", new_idat)
        elif kind != b"IEND":
            out += chunk(kind, payload)
    out += chunk(b"IEND", b"")
    return bytes(out)


def fixtures() -> dict[str, bytes]:
    out: dict[str, bytes] = {}

    # --- IHDR-level structural defects (parse_ihdr) -------------------------
    out["bad-ihdr-zero-width.png"] = build(width=0)
    out["bad-ihdr-zero-height.png"] = build(height=0)
    out["bad-ihdr-oversized-width.png"] = build(
        ihdr_payload_override=ihdr_payload(0xFFFFFFF0, 2, 8, 2),
        idat_payloads=[zlib.compress(b"", 9)],
    )
    out["bad-ihdr-oversized-height.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 0xFFFFFFF0, 8, 2),
        idat_payloads=[zlib.compress(b"", 9)],
    )
    out["bad-ihdr-unsupported-color-type-1.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 8, 1)
    )
    out["bad-ihdr-unsupported-color-type-5.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 8, 5)
    )
    out["bad-ihdr-unsupported-color-type-7.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 8, 7)
    )
    out["bad-ihdr-illegal-depth-rgb-1.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 1, 2)
    )
    out["bad-ihdr-illegal-depth-rgb-4.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 4, 2)
    )
    out["bad-ihdr-illegal-depth-palette-16.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 16, 3),
        plte=bytes([0, 0, 0, 255, 255, 255]),
    )
    out["bad-ihdr-illegal-depth-gray-3.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 3, 0)
    )
    out["bad-ihdr-illegal-depth-rgba-1.png"] = build(
        ihdr_payload_override=ihdr_payload(2, 2, 1, 6)
    )
    out["bad-ihdr-unsupported-compression.png"] = build(compression=1)
    out["bad-ihdr-unsupported-filter-method.png"] = build(filter_method=1)
    out["bad-ihdr-unsupported-interlace.png"] = build(interlace=2)
    out["bad-ihdr-wrong-length-short.png"] = build(ihdr_payload_override=ihdr_payload(2, 2, 8, 2)[:-1])
    out["bad-ihdr-wrong-length-long.png"] = build(ihdr_payload_override=ihdr_payload(2, 2, 8, 2) + b"\x00")
    out["bad-ihdr-duplicate.png"] = build(duplicate_ihdr=True)
    out["bad-ihdr-not-first-chunk.png"] = build(ihdr_first=False, gama=struct.pack(">I", 45455))

    # --- Chunk-framing / structural defects (parse_chunks) ------------------
    out["bad-structure-missing-signature.png"] = b"not a png file at all, just garbage bytes\x00\x01\x02"
    out["bad-structure-empty-file.png"] = b""
    out["bad-structure-signature-only.png"] = SIG
    out["bad-structure-unknown-critical-chunk.png"] = build(extra_chunks=((b"FRAK", b"x"),))
    out["bad-structure-missing-idat.png"] = build(idat_payloads=[])
    out["bad-structure-missing-iend.png"] = build(include_iend=False)
    out["bad-structure-trailing-garbage.png"] = build(trailing=b"\x00\x01\x02\x03")
    out["bad-structure-truncated-framing.png"] = build(include_iend=False) + b"\x00\x00\x00"
    out["bad-structure-truncated-chunk-body.png"] = build(include_iend=False) + struct.pack(
        ">I", 500
    ) + b"IDAT" + b"short"

    # --- PLTE / tRNS content defects (parse_plte / parse_trns) --------------
    out["bad-plte-not-multiple-of-3.png"] = build(
        color_type=3, bit_depth=8, plte=bytes([0, 0, 0, 255]), idat_payloads=[zlib.compress(b"", 9)]
    )
    out["bad-plte-zero-entries.png"] = build(
        color_type=3, bit_depth=8, plte=b"", idat_payloads=[zlib.compress(b"", 9)]
    )
    out["bad-plte-too-many-entries.png"] = build(
        color_type=3, bit_depth=8, plte=bytes([1, 2, 3]) * 257, idat_payloads=[zlib.compress(b"", 9)]
    )
    out["bad-plte-missing-for-palette-image.png"] = build(color_type=3, bit_depth=8, idat_payloads=[zlib.compress(b"", 9)])
    out["bad-plte-duplicate.png"] = build(
        color_type=3,
        bit_depth=8,
        plte=bytes([0, 0, 0, 255, 255, 255]),
        extra_chunks=(("PLTE".encode(), bytes([0, 0, 0, 255, 255, 255])),),
        idat_payloads=[zlib.compress(b"", 9)],
    )
    out["bad-trns-longer-than-plte.png"] = build(
        color_type=3,
        bit_depth=8,
        plte=bytes([0, 0, 0, 255, 255, 255]),
        trns=bytes([255, 255, 255]),
        idat_payloads=[zlib.compress(b"", 9)],
    )
    out["bad-trns-duplicate.png"] = build(
        color_type=3,
        bit_depth=8,
        plte=bytes([0, 0, 0, 255, 255, 255]),
        trns=bytes([255]),
        extra_chunks=(("tRNS".encode(), bytes([128])),),
        idat_payloads=[zlib.compress(b"", 9)],
    )
    out["bad-trns-gray-wrong-length.png"] = build(color_type=0, bit_depth=8, trns=bytes([0, 0, 0]))
    out["bad-trns-truecolor-wrong-length.png"] = build(color_type=2, bit_depth=8, trns=bytes([0, 0, 0, 0]))

    # --- gAMA / iCCP content defects -----------------------------------------
    out["bad-gama-wrong-length.png"] = build(gama=struct.pack(">I", 1)[:-1])
    out["bad-iccp-missing-terminator.png"] = build(iccp=b"no-null-terminator-here")
    out["bad-iccp-unsupported-compression-method.png"] = build(
        iccp=b"name\x00\x01" + zlib.compress(b"fake", 6)
    )

    # --- IDAT / inflate-stage defects (post-inflate reachability) -----------
    valid_min = build(width=2, height=2, color_type=2, bit_depth=8)
    out["bad-idat-invalid-deflate-stream.png"] = build(
        idat_payloads=[b"\xff\xff\xff\xff garbage not zlib at all"]
    )
    good_stream = zlib.compress(solid_scanlines(2, 2, 8, 2, 1), 9)
    out["bad-idat-truncated-deflate-stream.png"] = build(idat_payloads=[good_stream[: len(good_stream) // 2]])
    out["bad-idat-trailing-data-after-stream.png"] = build(idat_payloads=[good_stream + b"\x00\x00\x00\x00"])
    out["bad-idat-decoded-length-mismatch.png"] = build(
        width=4, height=4, idat_payloads=[good_stream]  # stream sized for 2x2, header says 4x4
    )

    # --- Post-inflate pixel-level defects ------------------------------------
    out["bad-pixel-palette-index-out-of-range.png"] = build(
        color_type=3,
        bit_depth=8,
        width=1,
        height=1,
        plte=bytes([0, 0, 0]),  # 1 entry
        scanlines=b"\x00\x05",  # index 5, only entry 0 exists
        idat_payloads=None,
    )
    out["bad-pixel-invalid-row-filter-type.png"] = corrupt_first_filter_byte(
        build(width=2, height=2, color_type=2, bit_depth=8), bad_filter=250
    )

    return out


def write_all(check: bool) -> int:
    CORPUS_DIR.mkdir(parents=True, exist_ok=True)
    seed = valid_seed()
    all_files = dict(fixtures())
    all_files["seed.png"] = seed

    mismatches = []
    for name, data in sorted(all_files.items()):
        path = CORPUS_DIR / name
        if check:
            if not path.is_file() or path.read_bytes() != data:
                mismatches.append(name)
        else:
            path.write_bytes(data)

    if check:
        if mismatches:
            print(f"CHECK FAILED: {len(mismatches)} file(s) differ from generator output:", file=sys.stderr)
            for name in mismatches:
                print(f"  {name}", file=sys.stderr)
            return 1
        print(f"CHECK OK: {len(all_files)} files match the generator byte-for-byte.")
        return 0

    print(f"wrote {len(all_files)} files to {CORPUS_DIR}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="verify the committed corpus matches this generator, write nothing"
    )
    args = parser.parse_args()
    return write_all(check=args.check)


if __name__ == "__main__":
    raise SystemExit(main())

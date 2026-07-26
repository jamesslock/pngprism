#!/usr/bin/env python3
"""Deterministic structure-aware seed corpus for the T-0207 fuzz targets.

Random-byte fuzzing of a PNG decoder is low-value: virtually every random
buffer dies at the 8-byte signature check, so the fuzzer never reaches the
chunk parser, the inflate seam, the defilter loop, or the quantizer. This
generator instead builds a *structure-aware* seed corpus — the starting
population libFuzzer mutates from — out of two ingredients the task mandates:

  1. REAL corpus files: every committed fixture from the two existing hand-
     built corpora (`tests/edge/corpus/`, T-0201 — valid + malformed edge
     geometries; and `tests/adversarial/corpus/`, T-0110 ch17 §31 — one
     malformed fixture per declared `decode_png` rejection reason), plus the
     shared `seed.png`. These are already-valid PNG *structure* the fuzzer can
     splice and perturb, so mutations land deep in the decoder rather than
     bouncing off the signature.

  2. CHUNK-LEVEL structural mutations of those real files: splice / reorder /
     resize / duplicate / drop whole PNG chunks, and perturb IHDR fields
     (dimensions, bit depth, colour type, interlace). Every mutation is a
     deterministic function of a fixed seed file + a fixed rule, so re-running
     reproduces byte-identical output (no runtime randomness — same discipline
     as the `tests/edge` and `tests/adversarial` generators). libFuzzer's own
     mutator supplies the randomness at fuzz time; this corpus only has to
     seed it with structurally diverse, decoder-reaching inputs.

The same corpus seeds both fuzz targets (`decode_png`, `quantize_pipeline`).

Stdlib only (`struct`, `zlib`, `hashlib`, `pathlib`). Usage:
    python3 fuzz/generate_seed_corpus.py            # (re)write the corpus
    python3 fuzz/generate_seed_corpus.py --check    # verify committed == generated
"""

from __future__ import annotations

import argparse
import hashlib
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
CORPUS_DECODE = HERE / "corpus" / "decode_png"
CORPUS_PIPELINE = HERE / "corpus" / "quantize_pipeline"

# Real hand-built corpora to draw structure from (paths relative to the crate).
SOURCE_DIRS = [
    CRATE / "tests" / "edge" / "corpus",
    CRATE / "tests" / "adversarial" / "corpus",
]

SIG = b"\x89PNG\r\n\x1a\n"


# --- PNG chunk model (parse an existing PNG into a chunk list) ---------------


def split_chunks(data: bytes) -> tuple[bytes, list[tuple[bytes, bytes]]] | None:
    """Return (signature, [(kind, payload), ...]) or None if `data` is not a
    well-enough-formed PNG chunk stream to decompose (a bad seed we keep as-is
    rather than mutate structurally)."""
    if len(data) < 8 or data[:8] != SIG:
        return None
    chunks: list[tuple[bytes, bytes]] = []
    off = 8
    while off + 8 <= len(data):
        (length,) = struct.unpack(">I", data[off : off + 4])
        kind = data[off + 4 : off + 8]
        body_start = off + 8
        body_end = body_start + length
        crc_end = body_end + 4
        if crc_end > len(data):
            return None  # truncated framing: keep verbatim, don't decompose
        payload = data[body_start:body_end]
        chunks.append((kind, payload))
        off = crc_end
        if kind == b"IEND":
            break
    if not chunks or chunks[0][0] != b"IHDR":
        return None
    return SIG, chunks


def build_chunk(kind: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    )


def assemble(chunks: list[tuple[bytes, bytes]], *, fix_crc: bool = True) -> bytes:
    out = bytearray(SIG)
    for kind, payload in chunks:
        if fix_crc:
            out += build_chunk(kind, payload)
        else:
            # Emit with a deliberately WRONG crc (all-zero) to seed the CRC
            # rejection path without libFuzzer having to guess a valid CRC.
            out += struct.pack(">I", len(payload)) + kind + payload + b"\x00\x00\x00\x00"
    return bytes(out)


# --- deterministic structural mutations -------------------------------------
#
# Each takes a decomposed (sig, chunks) and returns bytes (or None to skip when
# the mutation does not apply to this seed). Named so filenames are stable.


def mut_reorder_first_ancillary(chunks):
    """Swap the first two non-IHDR/IEND chunks (chunk reorder)."""
    mid = [i for i, (k, _) in enumerate(chunks) if k not in (b"IHDR", b"IEND")]
    if len(mid) < 2:
        return None
    c = list(chunks)
    i, j = mid[0], mid[1]
    c[i], c[j] = c[j], c[i]
    return assemble(c)


def mut_duplicate_idat(chunks):
    """Duplicate the first IDAT chunk in place (chunk duplication)."""
    for idx, (k, p) in enumerate(chunks):
        if k == b"IDAT":
            c = list(chunks)
            c.insert(idx + 1, (b"IDAT", p))
            return assemble(c)
    return None


def mut_drop_idat(chunks):
    """Drop every IDAT (chunk deletion -> missing-image-data path)."""
    c = [(k, p) for (k, p) in chunks if k != b"IDAT"]
    if len(c) == len(chunks):
        return None
    return assemble(c)


def mut_resize_ihdr_giant(chunks):
    """Rewrite IHDR width/height to a near-u32::MAX 'gigapixel' pair while
    leaving the tiny original IDAT untouched (chunk field resize -> the
    pixel-cap-before-allocation path)."""
    c = list(chunks)
    kind, payload = c[0]
    if kind != b"IHDR" or len(payload) != 13:
        return None
    body = bytearray(payload)
    body[0:4] = struct.pack(">I", 0x7FFFFFFF)
    body[4:8] = struct.pack(">I", 0x7FFFFFFF)
    c[0] = (b"IHDR", bytes(body))
    return assemble(c)


def mut_ihdr_bit_depth_zero(chunks):
    """Zero IHDR bit depth (invalid-depth path)."""
    c = list(chunks)
    kind, payload = c[0]
    if kind != b"IHDR" or len(payload) != 13:
        return None
    body = bytearray(payload)
    body[8] = 0
    c[0] = (b"IHDR", bytes(body))
    return assemble(c)


def mut_ihdr_color_type_bogus(chunks):
    """Set IHDR colour type to an unsupported value (5)."""
    c = list(chunks)
    kind, payload = c[0]
    if kind != b"IHDR" or len(payload) != 13:
        return None
    body = bytearray(payload)
    body[9] = 5
    c[0] = (b"IHDR", bytes(body))
    return assemble(c)


def mut_ihdr_toggle_interlace(chunks):
    """Flip the IHDR interlace flag (re-routes non-interlaced seeds through the
    Adam7 pass geometry, whose scanline count will then mismatch the IDAT ->
    the length-check path; and vice-versa)."""
    c = list(chunks)
    kind, payload = c[0]
    if kind != b"IHDR" or len(payload) != 13:
        return None
    body = bytearray(payload)
    body[12] = 0 if body[12] else 1
    c[0] = (b"IHDR", bytes(body))
    return assemble(c)


def mut_truncate_last_idat_payload(chunks):
    """Halve the last IDAT payload (truncated deflate stream)."""
    idats = [i for i, (k, _) in enumerate(chunks) if k == b"IDAT"]
    if not idats:
        return None
    c = list(chunks)
    idx = idats[-1]
    k, p = c[idx]
    c[idx] = (k, p[: len(p) // 2])
    return assemble(c)


def mut_bad_crc_on_ihdr(chunks):
    """Emit with all-zero CRCs (CRC-mismatch path) — the whole file, so the
    first chunk already trips it."""
    return assemble(chunks, fix_crc=False)


def mut_inject_unknown_critical(chunks):
    """Inject an unknown *critical* chunk (uppercase 4th letter) after IHDR."""
    c = list(chunks)
    c.insert(1, (b"cRIT", b"\x00\x01\x02\x03"))
    return assemble(c)


def mut_inject_unknown_ancillary(chunks):
    """Inject an unknown *ancillary* chunk (lowercase) after IHDR — must be
    ignored, so this seeds a valid-but-embellished structure."""
    c = list(chunks)
    c.insert(1, (b"prIv", b"hello-fuzz"))
    return assemble(c)


MUTATIONS = [
    ("reorder", mut_reorder_first_ancillary),
    ("dup-idat", mut_duplicate_idat),
    ("drop-idat", mut_drop_idat),
    ("giant-dims", mut_resize_ihdr_giant),
    ("depth0", mut_ihdr_bit_depth_zero),
    ("ctype5", mut_ihdr_color_type_bogus),
    ("interlace-flip", mut_ihdr_toggle_interlace),
    ("trunc-idat", mut_truncate_last_idat_payload),
    ("badcrc", mut_bad_crc_on_ihdr),
    ("crit-chunk", mut_inject_unknown_critical),
    ("anc-chunk", mut_inject_unknown_ancillary),
]


# --- corpus assembly --------------------------------------------------------


def short_hash(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:12]


def collect() -> dict[str, bytes]:
    """name -> bytes. Names are content-addressed so the set is stable and
    duplicate structures collapse to one entry (libFuzzer wants a deduplicated
    seed set)."""
    files: dict[str, bytes] = {}

    def add(prefix: str, data: bytes) -> None:
        if not data:
            return
        files[f"{prefix}-{short_hash(data)}"] = data

    for src in SOURCE_DIRS:
        if not src.is_dir():
            continue
        tag = src.parent.name  # "edge" or "adversarial"
        for path in sorted(src.glob("*.png")):
            data = path.read_bytes()
            add(f"seed-{tag}", data)
            decomposed = split_chunks(data)
            if decomposed is None:
                continue
            _sig, chunks = decomposed
            for mut_name, fn in MUTATIONS:
                try:
                    out = fn(list(chunks))
                except Exception:  # a mutation that does not apply -> skip
                    out = None
                if out is not None and out != data:
                    add(f"mut-{mut_name}", out)

    return files


def write_dir(target: Path, files: dict[str, bytes], check: bool) -> list[str]:
    """Write (or --check) `files` into `target`. Returns a list of mismatch
    descriptions (empty == OK)."""
    mismatches: list[str] = []
    if check:
        for name, data in files.items():
            path = target / name
            if not path.is_file() or path.read_bytes() != data:
                mismatches.append(f"{target.name}/{name}")
        expected = set(files)
        for path in target.glob("*"):
            if path.name == ".gitkeep":
                continue
            if path.name not in expected:
                mismatches.append(f"{target.name}/{path.name} (stray)")
        return mismatches
    target.mkdir(parents=True, exist_ok=True)
    # Clear stray files first (keep the dir reproducible).
    for path in target.glob("*"):
        if path.name != ".gitkeep" and path.name not in files:
            path.unlink()
    for name, data in files.items():
        (target / name).write_bytes(data)
    return mismatches


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed corpus matches this generator; write nothing",
    )
    args = parser.parse_args()

    files = collect()
    mismatches: list[str] = []
    for target in (CORPUS_DECODE, CORPUS_PIPELINE):
        mismatches += write_dir(target, files, args.check)

    if args.check:
        if mismatches:
            print(
                f"CHECK FAILED: {len(mismatches)} item(s) differ from generator:",
                file=sys.stderr,
            )
            for m in sorted(mismatches):
                print(f"  {m}", file=sys.stderr)
            return 1
        print(f"CHECK OK: {len(files)} seeds x 2 targets match byte-for-byte.")
        return 0

    print(f"wrote {len(files)} seeds into each of decode_png/ and quantize_pipeline/")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

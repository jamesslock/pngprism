#!/usr/bin/env python3
"""Arbitrary-PNG decoder and deterministic indexed-PNG writer for the Prism M1 harness.

``decode_png`` turns any spec-valid PNG into canonical RGBA8 pixels;
``write_indexed_png`` emits a byte-deterministic color-type-3 PNG. Standard
library only. All malformed input fails with :class:`PngError` — no traceback
ever escapes into the harness controller.

Declared conversion and policy decisions (normative for this module):

1.  Chunk discipline: the 8-byte signature is required; every chunk's CRC-32
    (over chunk type + payload) is validated; IHDR must be the first chunk and
    appear exactly once; IEND must be the final zero-length chunk with no
    trailing bytes after it; unknown critical chunks are rejected; unknown
    ancillary chunks are ignored; multiple IDAT chunks are concatenated in
    order of appearance.
2.  IHDR constraints: width and height must be at least 1; compression method
    must be 0; filter method must be 0; interlace method must be 0 (none) or
    1 (Adam7) — any other value is an error; only color types 0/2/3/4/6 with
    their spec-valid bit depths are accepted (gray 1/2/4/8/16, truecolor
    8/16, palette 1/2/4/8, gray+alpha 8/16, RGBA 8/16).
3.  gAMA and iCCP are recorded in ``properties`` but NEVER applied: this
    module performs no color management; sample values pass through
    numerically untouched.
4.  Defiltering: the filter unit (bytes-per-pixel) is
    ``max(1, ceil(channels * bit_depth / 8))`` — sub-byte depths filter at
    1 bpp; each scanline is ``ceil(width * bits_per_pixel / 8)`` bytes; all
    five filter types (0 None, 1 Sub, 2 Up, 3 Average, 4 Paeth) reconstruct
    mod 256; the Paeth tie-break order is left, up, up-left (spec order).
5.  Sub-byte samples (bit depths 1/2/4) unpack MSB-first within each byte;
    row padding bits are discarded.
6.  16-bit samples convert to 8-bit by ``(v * 255 + 32767) // 65535``
    (declared rounding), applied to every channel including alpha.
7.  Sub-byte grayscale samples scale to 8-bit by ``v * 255 // (2^d - 1)``,
    which is exact integer arithmetic (1-bit: x255, 2-bit: x85, 4-bit: x17).
8.  tRNS policy: for gray and truecolor images the colorkey is matched on
    the native-depth SOURCE sample values BEFORE any rounding, and yields
    binary alpha (0 on match, 255 otherwise). For palette images the alpha
    of each used entry comes from tRNS, defaulting to 255 when tRNS is
    absent or shorter than the referenced index. tRNS on color types 4/6 is
    spec-invalid and is recorded as absent and never applied.
9.  Palette indices greater than or equal to the PLTE entry count are a
    hard error (PngError), never a silent clamp.
10. Adam7 interlace: the seven passes at origins/strides (0,0,8,8),
    (4,0,8,8), (0,4,4,8), (2,0,4,4), (0,2,2,4), (1,0,2,2), (0,1,1,2) are
    each defiltered independently with their own row stride and bpp; passes
    empty for the image geometry carry no data and are skipped.
11. The concatenated IDAT zlib stream must decompress cleanly to EOF with
    no trailing bytes, and the decompressed size must equal the expected
    scanline byte count exactly.
12. Output pixels are ``(r, g, b, a)`` tuples of ints 0..255, row-major
    from the top-left, exactly ``width * height`` of them.
13. Writer policy: color type 3, bit depth 8, non-interlaced, every row
    filter 0, a single IDAT chunk, ``zlib.compress(data, 9)``, PLTE with RGB
    triples, tRNS emitted iff any palette alpha < 255 and trimmed to the
    last alpha < 255 entry, no ancillary chunks, correct CRCs. Output is
    deterministic by construction (no timestamps, no randomness).
14. Resource admission: compressed input, each dimension, total pixels, and
    aggregate decoded scanline bytes have explicit absolute ceilings shared
    with the Rust port. IHDR-derived ceilings are checked before IDAT parsing,
    inflation, or canonical-pixel allocation. These bounds constrain requests;
    they are not an allocation-success promise under arbitrary host pressure.
"""

from __future__ import annotations

import binascii
import os
import struct
import zlib
from dataclasses import dataclass
from os import PathLike
from typing import Any, Sequence

__all__ = [
    "PngError",
    "PngResourceError",
    "DecodedImage",
    "decode_png",
    "read_png_file",
    "write_indexed_png",
    "PNG_SIGNATURE",
    "MAX_INPUT_BYTES",
    "MAX_DIMENSION",
    "MAX_BYTES_PER_PIXEL",
    "MAX_PIXELS",
    "MAX_DECODED_SCANLINE_BYTES",
    "set_max_pixels",
    "active_max_pixels",
]


PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# Decoder resource policy, mirrored byte-for-byte by src/png.rs. The DEFAULT
# pixel ceiling is 64 Mi-pixels (covers 50-64 MP cameras + 8K); a single
# conversion may override it up or down via ``set_max_pixels`` (``--max-pixels``).
# MAX_DIMENSION (fixed) rejects pathological skinny images; the compressed-input
# ceiling (256 MiB, fixed) bounds retained input. The decoded-scanline ceiling
# is DERIVED as (active pixel ceiling) x MAX_BYTES_PER_PIXEL, so the single
# ``--max-pixels`` lever scales pixel and scanline admission together. These are
# admission ceilings, not a promise that allocation cannot fail under host
# memory pressure; the working-set rationale + memory measurements live in
# lib/prism-quant/docs/resource-limits.md.
MAX_INPUT_BYTES = 256 * 1024 * 1024
MAX_DIMENSION = 32_768
# The widest native PNG pixel is 16-bit RGBA = 8 bytes.
MAX_BYTES_PER_PIXEL = 8
# DEFAULT pixel ceiling (overridable per-invocation via set_max_pixels).
MAX_PIXELS = 64 * 1024 * 1024
# DEFAULT decoded-scanline ceiling (512 MiB); the enforced value scales with
# the active pixel ceiling the same way.
MAX_DECODED_SCANLINE_BYTES = MAX_PIXELS * MAX_BYTES_PER_PIXEL

# Process-wide active pixel ceiling. Defaults to MAX_PIXELS; the CLI's
# ``--max-pixels N`` overrides it once, before any decode. Every decode reads
# this same value, so source admission and the pipeline's own
# self-verification re-decode honor one coherent ceiling.
_active_max_pixels = MAX_PIXELS


def set_max_pixels(limit: int) -> None:
    """Override the active pixel ceiling (``--max-pixels N``).

    Intended to be set once at startup, before decoding. ``limit`` must be
    >= 1 (the CLI rejects 0/negative/non-numeric before calling). Raising it
    above available RAM is the caller's choice to own: the no-OOM guarantee
    holds only at or below the active ceiling.
    """

    global _active_max_pixels
    _active_max_pixels = limit


def active_max_pixels() -> int:
    """The pixel ceiling the next decode will enforce.

    The ``--max-pixels`` override if one was set, else the ``MAX_PIXELS``
    default. The enforced decoded-scanline ceiling is derived from this value
    x ``MAX_BYTES_PER_PIXEL``.
    """

    return _active_max_pixels

_CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}
_VALID_DEPTHS = {
    0: (1, 2, 4, 8, 16),
    2: (8, 16),
    3: (1, 2, 4, 8),
    4: (8, 16),
    6: (8, 16),
}
_ADAM7_PASSES = (
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
)


class PngError(Exception):
    """Clean, expected failure for malformed or unsupported PNG input/output."""


class PngResourceError(PngError):
    """A PNG crossed a declared decoder admission ceiling."""


def _input_limit_message() -> str:
    return (
        "resource limit exceeded: compressed PNG input exceeds "
        f"{MAX_INPUT_BYTES} bytes"
    )


def read_png_file(path: str | PathLike[str]) -> bytes:
    """Read one PNG through the absolute compressed-input ceiling.

    ``fstat`` gives regular files a zero-payload fast rejection. The bounded
    ``read(MAX + 1)`` remains authoritative if a file grows after that check
    or if the descriptor is non-regular.
    """

    with open(path, "rb") as stream:
        declared_size = os.fstat(stream.fileno()).st_size
        if declared_size > MAX_INPUT_BYTES:
            raise PngResourceError(_input_limit_message())
        # Do not pass MAX+1 to one `read`: CPython may reserve the requested
        # size even for a tiny regular file. Start from descriptor size + one,
        # then continue in bounded chunks so growth/special files cannot evade
        # the cap and ordinary inputs allocate only their actual size.
        chunks = [stream.read(min(declared_size + 1, MAX_INPUT_BYTES + 1))]
        total = len(chunks[0])
        while chunks[-1] and total <= MAX_INPUT_BYTES:
            remaining = MAX_INPUT_BYTES + 1 - total
            piece = stream.read(min(1024 * 1024, remaining))
            if not piece:
                break
            chunks.append(piece)
            total += len(piece)
        if total > MAX_INPUT_BYTES:
            raise PngResourceError(_input_limit_message())
        return b"".join(chunks)


@dataclass(frozen=True)
class DecodedImage:
    """Canonical decode result: RGBA8 pixels row-major from the top-left."""

    width: int
    height: int
    pixels: tuple[tuple[int, int, int, int], ...]
    properties: dict[str, Any]


@dataclass(frozen=True)
class _Header:
    width: int
    height: int
    bit_depth: int
    color_type: int
    interlaced: bool


def _parse_ihdr(payload: bytes) -> _Header:
    if len(payload) != 13:
        raise PngError("IHDR chunk must be exactly 13 bytes")
    (
        width,
        height,
        bit_depth,
        color_type,
        compression,
        filter_method,
        interlace,
    ) = struct.unpack(">IIBBBBB", payload)
    if width == 0 or height == 0:
        raise PngError("image dimensions must be at least 1x1")
    if color_type not in _CHANNELS:
        raise PngError(f"unsupported color type {color_type}")
    if bit_depth not in _VALID_DEPTHS[color_type]:
        raise PngError(f"invalid bit depth {bit_depth} for color type {color_type}")
    if compression != 0:
        raise PngError(f"unsupported compression method {compression}")
    if filter_method != 0:
        raise PngError(f"unsupported filter method {filter_method}")
    if interlace not in (0, 1):
        raise PngError(f"unsupported interlace method {interlace}")
    header = _Header(width, height, bit_depth, color_type, interlace == 1)
    _validate_header_resource_limits(header)
    return header


def _parse_iccp(payload: bytes) -> dict[str, Any]:
    nul = payload.find(b"\x00")
    if nul <= 0:
        raise PngError("malformed iCCP chunk: missing profile name terminator")
    if nul + 2 > len(payload) or payload[nul + 1] != 0:
        raise PngError("unsupported iCCP compression method")
    return {
        "name": payload[:nul].decode("latin-1"),
        "profile_bytes": len(payload) - (nul + 2),
    }


def _parse_chunks(raw: bytes) -> tuple[_Header, bytes | None, bytes | None, int | None, dict[str, Any] | None, list[bytes]]:
    offset = len(PNG_SIGNATURE)
    header: _Header | None = None
    plte_payload: bytes | None = None
    trns_payload: bytes | None = None
    gama: int | None = None
    iccp: dict[str, Any] | None = None
    idat_parts: list[bytes] = []
    first = True
    while True:
        if offset == len(raw):
            raise PngError("missing IEND chunk")
        if len(raw) - offset < 12:
            raise PngError("truncated chunk framing")
        (length,) = struct.unpack_from(">I", raw, offset)
        kind = raw[offset + 4 : offset + 8]
        data_end = offset + 8 + length
        crc_end = data_end + 4
        if crc_end > len(raw):
            raise PngError(f"truncated {kind!r} chunk")
        payload = raw[offset + 8 : data_end]
        (expected_crc,) = struct.unpack_from(">I", raw, data_end)
        actual_crc = binascii.crc32(kind + payload) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise PngError(f"CRC mismatch in {kind!r} chunk")
        if first:
            if kind != b"IHDR":
                raise PngError("first chunk must be IHDR")
            first = False
        if kind == b"IHDR":
            if header is not None:
                raise PngError("duplicate IHDR chunk")
            header = _parse_ihdr(payload)
        elif kind == b"PLTE":
            if plte_payload is not None:
                raise PngError("duplicate PLTE chunk")
            plte_payload = payload
        elif kind == b"tRNS":
            if trns_payload is not None:
                raise PngError("duplicate tRNS chunk")
            trns_payload = payload
        elif kind == b"gAMA":
            if len(payload) != 4:
                raise PngError("gAMA chunk must be 4 bytes")
            (gama,) = struct.unpack(">I", payload)
        elif kind == b"iCCP":
            iccp = _parse_iccp(payload)
        elif kind == b"IDAT":
            idat_parts.append(payload)
        elif kind == b"IEND":
            if length != 0:
                raise PngError("IEND chunk must be empty")
            if crc_end != len(raw):
                raise PngError("trailing garbage after IEND chunk")
            break
        elif not (kind[0] & 0x20):
            raise PngError(f"unknown critical chunk {kind!r}")
        # Unknown ancillary chunk: ignored by policy.
        offset = crc_end
    if header is None:  # pragma: no cover - unreachable: first chunk must be IHDR
        raise PngError("missing IHDR chunk")
    return header, plte_payload, trns_payload, gama, iccp, idat_parts


def _parse_plte(payload: bytes) -> list[tuple[int, int, int]]:
    if len(payload) % 3 != 0:
        raise PngError("PLTE length must be a multiple of 3")
    entries = len(payload) // 3
    if not 1 <= entries <= 256:
        raise PngError(f"PLTE entry count {entries} out of range")
    return [(payload[i * 3], payload[i * 3 + 1], payload[i * 3 + 2]) for i in range(entries)]


def _parse_trns(
    payload: bytes, header: _Header, plte: list[tuple[int, int, int]] | None
) -> int | tuple[int, ...] | None:
    color_type = header.color_type
    if color_type in (4, 6):
        # Spec-invalid placement: recorded as absent, never applied.
        return None
    if color_type == 3:
        if plte is not None and len(payload) > len(plte):
            raise PngError("tRNS longer than PLTE")
        return tuple(payload)
    if color_type == 0:
        if len(payload) != 2:
            raise PngError("grayscale tRNS must be 2 bytes")
        (value,) = struct.unpack(">H", payload)
        return value
    if len(payload) != 6:
        raise PngError("truecolor tRNS must be 6 bytes")
    return struct.unpack(">HHH", payload)


def _pass_geometry(header: _Header) -> list[tuple[int, int, int, int, int, int]]:
    """Non-empty passes as (x0, y0, dx, dy, pass_width, pass_height)."""
    passes = _ADAM7_PASSES if header.interlaced else ((0, 0, 1, 1),)
    geometry: list[tuple[int, int, int, int, int, int]] = []
    for x0, y0, dx, dy in passes:
        pw = (header.width - x0 + dx - 1) // dx if header.width > x0 else 0
        ph = (header.height - y0 + dy - 1) // dy if header.height > y0 else 0
        if pw and ph:
            geometry.append((x0, y0, dx, dy, pw, ph))
    return geometry


def _validate_header_resource_limits(header: _Header) -> None:
    """Reject every IHDR-derived ceiling before parsing reaches IDAT.

    The pixel (and derived scanline) ceilings are read from the process-wide
    active value (``active_max_pixels``, set once by ``--max-pixels``); the
    dimension ceiling is fixed. Delegates to the pure ceiling arithmetic so it
    is testable without mutating the global.
    """

    _validate_header_resource_limits_with(header, active_max_pixels())


def _validate_header_resource_limits_with(header: _Header, max_pixels: int) -> None:
    """Pure admission check against an explicit pixel ceiling ``max_pixels``.

    The scanline ceiling is derived (``max_pixels`` x ``MAX_BYTES_PER_PIXEL``);
    the dimension ceiling is fixed.
    """

    if header.width > MAX_DIMENSION or header.height > MAX_DIMENSION:
        raise PngResourceError(
            "resource limit exceeded: image dimensions "
            f"{header.width}x{header.height} exceed per-dimension maximum "
            f"{MAX_DIMENSION}"
        )
    pixel_count = header.width * header.height
    if pixel_count > max_pixels:
        raise PngResourceError(
            f"resource limit exceeded: image has {pixel_count} pixels; "
            f"maximum is {max_pixels}"
        )

    channels = _CHANNELS[header.color_type]
    bits_per_pixel = channels * header.bit_depth
    decoded_bytes = sum(
        ph * (1 + (pw * bits_per_pixel + 7) // 8)
        for (_, _, _, _, pw, ph) in _pass_geometry(header)
    )
    max_decoded_scanline_bytes = max_pixels * MAX_BYTES_PER_PIXEL
    if decoded_bytes > max_decoded_scanline_bytes:
        raise PngResourceError(
            "resource limit exceeded: decoded scanlines require "
            f"{decoded_bytes} bytes; maximum is {max_decoded_scanline_bytes}"
        )


def _inflate(parts: list[bytes], expected: int) -> bytes:
    decomp = zlib.decompressobj()
    # Bounded scratch growth (T-0212): feed `decompress()` a
    # `max_length`-bounded piece per call instead of materializing the
    # entire IDAT output in one unbounded call. Once cumulative output has
    # already reached the IHDR-declared `expected` total, any FURTHER
    # output byte is conclusive proof the stream decodes to more than
    # IHDR promised, so decoding aborts the instant that byte appears
    # rather than after the (potentially enormous) stream fully drains.
    # SCRATCH_CAP mirrors the Rust port's `src/png.rs::inflate` cap so a
    # well-formed decode still finishes in effectively one call; T-0207
    # found the unbounded predecessor of this function could materialize
    # ~1000x the compressed IDAT size before its length check ever ran.
    SCRATCH_CAP = 8 * 1024 * 1024
    pending = b"".join(parts)
    chunks: list[bytes] = []
    total = 0
    try:
        while not decomp.eof:
            remaining = expected - total
            want = min(remaining, SCRATCH_CAP) if remaining > 0 else 1
            input_len_before = len(pending)
            piece = decomp.decompress(pending, want)
            pending = decomp.unconsumed_tail
            consumed = input_len_before - len(pending)
            if piece:
                if remaining <= 0:
                    raise PngError(
                        f"decoded more than {expected} scanline bytes "
                        "(deflate stream exceeds IHDR-declared size)"
                    )
                chunks.append(piece)
                total += len(piece)
            elif consumed == 0:
                # zlib documents progress (input consumed or output
                # produced) while input remains; this guard only rules out
                # an infinite loop and is unreachable per zlib docs —
                # mirrors the Rust port's identical guard.
                raise PngError("truncated IDAT deflate stream")
        data = b"".join(chunks) + decomp.flush()
    except zlib.error as exc:
        raise PngError(f"invalid IDAT deflate stream: {exc}") from exc
    if not decomp.eof:
        raise PngError("truncated IDAT deflate stream")
    if decomp.unused_data:
        raise PngError("trailing data after IDAT deflate stream")
    if len(data) != expected:
        raise PngError(f"decoded {len(data)} scanline bytes, expected {expected}")
    return data


def _defilter(data: bytes, offset: int, rows_count: int, row_bytes: int, bpp: int) -> tuple[list[bytearray], int]:
    rows: list[bytearray] = []
    prev = bytearray(row_bytes)
    pos = offset
    for _ in range(rows_count):
        filter_type = data[pos]
        pos += 1
        line = data[pos : pos + row_bytes]
        pos += row_bytes
        if filter_type == 0:
            recon = bytearray(line)
        elif filter_type == 1:
            recon = bytearray(line)
            for i in range(bpp, row_bytes):
                recon[i] = (recon[i] + recon[i - bpp]) & 0xFF
        elif filter_type == 2:
            recon = bytearray(row_bytes)
            for i in range(row_bytes):
                recon[i] = (line[i] + prev[i]) & 0xFF
        elif filter_type == 3:
            recon = bytearray(row_bytes)
            for i in range(row_bytes):
                left = recon[i - bpp] if i >= bpp else 0
                recon[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif filter_type == 4:
            recon = bytearray(row_bytes)
            for i in range(row_bytes):
                left = recon[i - bpp] if i >= bpp else 0
                up = prev[i]
                upper_left = prev[i - bpp] if i >= bpp else 0
                estimate = left + up - upper_left
                dist_left = estimate - left if estimate >= left else left - estimate
                dist_up = estimate - up if estimate >= up else up - estimate
                dist_ul = estimate - upper_left if estimate >= upper_left else upper_left - estimate
                if dist_left <= dist_up and dist_left <= dist_ul:
                    predictor = left
                elif dist_up <= dist_ul:
                    predictor = up
                else:
                    predictor = upper_left
                recon[i] = (line[i] + predictor) & 0xFF
        else:
            raise PngError(f"invalid row filter type {filter_type}")
        rows.append(recon)
        prev = recon
    return rows, pos


def _row_samples(row: bytearray, count: int, bit_depth: int) -> Any:
    if bit_depth == 16:
        return struct.unpack_from(f">{count}H", row)
    if bit_depth == 8:
        return row
    per_byte = 8 // bit_depth
    mask = (1 << bit_depth) - 1
    samples: list[int] = []
    for byte in row:
        shift = 8 - bit_depth
        for _ in range(per_byte):
            samples.append((byte >> shift) & mask)
            shift -= bit_depth
    del samples[count:]
    return samples


def _round16(value: int) -> int:
    return (value * 255 + 32767) // 65535


def _convert_row(
    samples: Any,
    pass_width: int,
    header: _Header,
    plte: list[tuple[int, int, int]] | None,
    trns: int | tuple[int, ...] | None,
    gray_scale: int,
) -> list[tuple[int, int, int, int]]:
    color_type = header.color_type
    bit_depth = header.bit_depth
    out: list[tuple[int, int, int, int]] = []
    if color_type == 0:
        for i in range(pass_width):
            value = samples[i]
            if bit_depth == 16:
                gray = _round16(value)
            elif bit_depth == 8:
                gray = value
            else:
                gray = value * gray_scale
            alpha = 0 if (trns is not None and value == trns) else 255
            out.append((gray, gray, gray, alpha))
    elif color_type == 3:
        assert plte is not None  # guaranteed: palette images require PLTE
        palette_size = len(plte)
        for i in range(pass_width):
            index = samples[i]
            if index >= palette_size:
                raise PngError(f"palette index {index} out of range ({palette_size} entries)")
            red, green, blue = plte[index]
            alpha = trns[index] if (trns is not None and index < len(trns)) else 255
            out.append((red, green, blue, alpha))
    elif color_type == 2:
        for i in range(pass_width):
            red = samples[i * 3]
            green = samples[i * 3 + 1]
            blue = samples[i * 3 + 2]
            alpha = 0 if (trns is not None and (red, green, blue) == trns) else 255
            if bit_depth == 16:
                out.append((_round16(red), _round16(green), _round16(blue), alpha))
            else:
                out.append((red, green, blue, alpha))
    elif color_type == 4:
        for i in range(pass_width):
            value = samples[i * 2]
            alpha = samples[i * 2 + 1]
            if bit_depth == 16:
                out.append((_round16(value), _round16(value), _round16(value), _round16(alpha)))
            else:
                out.append((value, value, value, alpha))
    else:
        for i in range(pass_width):
            red = samples[i * 4]
            green = samples[i * 4 + 1]
            blue = samples[i * 4 + 2]
            alpha = samples[i * 4 + 3]
            if bit_depth == 16:
                out.append((_round16(red), _round16(green), _round16(blue), _round16(alpha)))
            else:
                out.append((red, green, blue, alpha))
    return out


def _conversions_applied(header: _Header, trns: int | tuple[int, ...] | None) -> list[str]:
    color_type = header.color_type
    conversions: list[str] = []
    if color_type == 0:
        conversions.append("gray:replicate-sample-to-rgb")
        if header.bit_depth < 8:
            conversions.append("gray-sub-byte-scale:v*255//(2^bit_depth-1)")
    elif color_type == 2:
        conversions.append("truecolor:alpha-255")
    elif color_type == 3:
        conversions.append("palette:plte-lookup")
    elif color_type == 4:
        conversions.append("gray-alpha:replicate-gray-to-rgb;alpha-passthrough")
    else:
        conversions.append("rgba:passthrough")
    if header.bit_depth == 16:
        conversions.append("16-to-8-rounding:(v*255+32767)//65535")
    if trns is not None:
        if color_type in (0, 2):
            conversions.append("trns:colorkey-binary-alpha(native-source-sample-match)")
        elif color_type == 3:
            conversions.append("trns:palette-entry-alpha(default-255)")
    if header.interlaced:
        conversions.append("adam7:seven-pass-reassembly")
    return conversions


def decode_png(raw: bytes | bytearray) -> DecodedImage:
    """Decode an arbitrary PNG byte string to canonical RGBA8 pixels.

    Raises :class:`PngError` for any malformed or unsupported input.
    """

    if not isinstance(raw, (bytes, bytearray)):
        raise PngError("decode_png expects a bytes-like object")
    if len(raw) > MAX_INPUT_BYTES:
        raise PngResourceError(_input_limit_message())
    data_in = bytes(raw)
    if len(data_in) < len(PNG_SIGNATURE) or data_in[: len(PNG_SIGNATURE)] != PNG_SIGNATURE:
        raise PngError("missing PNG signature")

    header, plte_payload, trns_payload, gama, iccp, idat_parts = _parse_chunks(data_in)
    if not idat_parts:
        raise PngError("missing IDAT chunk")
    plte = _parse_plte(plte_payload) if plte_payload is not None else None
    if header.color_type == 3 and plte is None:
        raise PngError("palette image missing PLTE chunk")
    trns = _parse_trns(trns_payload, header, plte) if trns_payload is not None else None

    channels = _CHANNELS[header.color_type]
    bits_per_pixel = channels * header.bit_depth
    bpp = max(1, (bits_per_pixel + 7) // 8)
    geometry = _pass_geometry(header)
    expected = sum(ph * (1 + (pw * bits_per_pixel + 7) // 8) for (_, _, _, _, pw, ph) in geometry)
    data = _inflate(idat_parts, expected)

    gray_scale = 255 // ((1 << header.bit_depth) - 1) if header.bit_depth < 8 else 0
    pixels: list[tuple[int, int, int, int] | None] = [None] * (header.width * header.height)
    offset = 0
    for x0, y0, dx, dy, pass_width, pass_height in geometry:
        row_bytes = (pass_width * bits_per_pixel + 7) // 8
        rows, offset = _defilter(data, offset, pass_height, row_bytes, bpp)
        sample_count = pass_width * channels
        for row_index in range(pass_height):
            samples = _row_samples(rows[row_index], sample_count, header.bit_depth)
            converted = _convert_row(samples, pass_width, header, plte, trns, gray_scale)
            base = (y0 + row_index * dy) * header.width + x0
            for column, pixel in enumerate(converted):
                pixels[base + column * dx] = pixel
    if offset != len(data):  # pragma: no cover - _inflate pins the exact length
        raise PngError("scanline data left unconsumed")
    if any(pixel is None for pixel in pixels):  # pragma: no cover - geometry covers the image
        raise PngError("interlace passes did not cover the whole image")

    properties: dict[str, Any] = {
        "color_type": header.color_type,
        "bit_depth": header.bit_depth,
        "interlaced": header.interlaced,
        "plte": plte,
        "trns": trns,
        "gama": gama,
        "iccp": iccp,
        "conversions": _conversions_applied(header, trns),
    }
    return DecodedImage(
        header.width,
        header.height,
        tuple(pixels),  # type: ignore[arg-type]
        properties,
    )


def _emit_chunk(kind: bytes, payload: bytes) -> bytes:
    crc = binascii.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)


def write_indexed_png(
    width: int,
    height: int,
    palette: list[tuple[int, int, int, int]],
    indices: Sequence[int],
) -> bytes:
    """Serialize an indexed image as a deterministic color-type-3 PNG.

    Bit depth 8, non-interlaced, every row filter 0, single IDAT, zlib level
    9, tRNS only when some palette alpha is below 255 (trimmed to the last
    such entry), no ancillary chunks. Raises :class:`PngError` on invalid
    arguments.
    """

    if isinstance(width, bool) or isinstance(height, bool) or not isinstance(width, int) or not isinstance(height, int):
        raise PngError("width and height must be integers")
    if width < 1 or height < 1:
        raise PngError("width and height must be at least 1")
    if not isinstance(palette, (list, tuple)) or not 1 <= len(palette) <= 256:
        raise PngError("palette must contain 1..256 entries")
    normalized_palette: list[tuple[int, int, int, int]] = []
    for entry in palette:
        if not isinstance(entry, (list, tuple)) or len(entry) != 4:
            raise PngError("palette entries must be (r, g, b, a) tuples")
        red, green, blue, alpha = entry
        for channel in (red, green, blue, alpha):
            if isinstance(channel, bool) or not isinstance(channel, int) or not 0 <= channel <= 255:
                raise PngError("palette channels must be integers in 0..255")
        normalized_palette.append((red, green, blue, alpha))
    if len(indices) != width * height:
        raise PngError(f"expected {width * height} indices, got {len(indices)}")
    index_list: list[int] = []
    for index in indices:
        if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index < len(normalized_palette):
            raise PngError(f"palette index {index!r} out of range")
        index_list.append(index)

    scanlines = bytearray()
    for y in range(height):
        scanlines.append(0)
        scanlines.extend(index_list[y * width : (y + 1) * width])
    compressed = zlib.compress(bytes(scanlines), 9)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 3, 0, 0, 0)
    plte_payload = bytes(channel for (red, green, blue, _) in normalized_palette for channel in (red, green, blue))
    last_transparent = -1
    for position, (_, _, _, alpha) in enumerate(normalized_palette):
        if alpha < 255:
            last_transparent = position

    out = bytearray(PNG_SIGNATURE)
    out += _emit_chunk(b"IHDR", ihdr)
    out += _emit_chunk(b"PLTE", plte_payload)
    if last_transparent >= 0:
        trns_payload = bytes(alpha for (_, _, _, alpha) in normalized_palette[: last_transparent + 1])
        out += _emit_chunk(b"tRNS", trns_payload)
    out += _emit_chunk(b"IDAT", compressed)
    out += _emit_chunk(b"IEND", b"")
    return bytes(out)

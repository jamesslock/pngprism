#!/usr/bin/env python3
"""Deterministic indexed-PNG packing search for prism-quant (T-0069).

This module is a lossless packing stage.  It never changes the decoded RGBA
candidate supplied by its palette and index map.  It cleans the palette,
tries deterministic palette permutations and PNG row-filter strategies,
compares complete PNG byte strings, and retains the smallest artifact.

``search_version="v1"`` preserves the original five-order/six-filter search.
``search_version="v2"`` adds the bounded chapter-19 A5 search: nine declared
seed orders, a per-row trial-compression filter heuristic, actual-byte local
palette/filter refinement, and (in maximum mode) final-byte comparison across
three pinned-zopflipng finalists.  V2's decoded image is frozen throughout.

``mode="fast"`` uses only the Python standard library's zlib encoder.
``mode="max"`` (with ``"zopfli"`` accepted as an explicit alias) invokes the
repository's pinned ``zopflipng`` black-box subprocess.  Missing tools,
timeouts, nonzero exits, missing output, invalid PNG, and pixel changes are
explicit :class:`PackError` failures; maximum mode never silently falls back.

Clean-room derivation boundary: PNG serialization/filter behavior follows the
W3C PNG specification and Project Prism book chapters 05/06/17/19.  The
palette-order heuristics are independently authored Project Prism work.  No
libimagequant/pngquant source was consulted.  The optional external Zopfli
baseline is the Apache-2.0 black box pinned under ``benchmarks/baselines``.
"""

from __future__ import annotations

import binascii
import hashlib
import os
import shutil
import struct
import subprocess
import tempfile
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

import m1_png

Pixel = tuple[int, int, int, int]

FILTER_NAMES = ("none", "sub", "up", "average", "paeth")
FILTER_STRATEGIES = FILTER_NAMES + ("residual",)
ORDER_STRATEGIES = (
    "identity",
    "alpha-first",
    "frequency",
    "color-locality",
    "spatial-adjacency",
)
V2_ORDER_STRATEGIES = ORDER_STRATEGIES + (
    "alpha-frequency",
    "alpha-color-locality",
    "alpha-spatial-adjacency",
    "packed-frequency",
)
V2_FILTER_STRATEGIES = FILTER_STRATEGIES + ("trial-zlib",)

# Ch19 A5 requires a declared encode-count and no-improvement stop.  These are
# algorithm constants, not caller-tunable defaults, so identical inputs always
# receive the same search budget.  The seed portfolio costs 9 * 7 = 63 complete
# artifacts.  Palette moves and row changes share the remaining 33 slots.
V2_MAX_PRE_OPTIMIZER_VARIANTS = 96
V2_LOCAL_MOVE_LIMIT = 20
V2_NO_IMPROVEMENT_LIMIT = 12
V2_ROW_CHANGE_LIMIT = 16
V2_ZOPFLI_FINALIST_LIMIT = 3
V2_ZOPFLI_ARGUMENTS = ("-m",)

ZOPFLI_PINNED_COMMIT = "ccf9f0588d4a4509cb1040310ec122243e670ee6"
ZOPFLI_LICENSE = "Apache-2.0"
_PRISM_ROOT = Path(__file__).resolve().parents[2]
_IN_TREE_ZOPFLIPNG = (
    _PRISM_ROOT / "benchmarks" / "baselines" / "zopfli" / "work" / "zopfli" / "zopflipng"
)


def _resolve_zopflipng() -> Path:
    """Locate `zopflipng`: PRISM_ZOPFLIPNG, else the in-tree pinned build, else PATH.

    **This order is load-bearing and must stay identical to the Rust port's**
    (`pack.rs::default_zopflipng` / `resolve_zopflipng`). The two implementations
    shell out to zopflipng independently; if they resolve to DIFFERENT binaries
    their bytes may diverge and the parity gates would be comparing two different
    programs rather than two implementations of one.

    Rung 1 (`PRISM_ZOPFLIPNG`) is new here. The parity and benchmark harnesses
    have always set that variable, but only the Rust side read it — this module
    used the in-tree path unconditionally. In-tree the two coincide, so the gap
    never showed up; it would have the moment anyone pinned a non-default build.

    Rung 2 keeps the in-tree pinned build ABOVE PATH so an ad-hoc in-tree run
    with no environment set uses the vendored pinned zopflipng on both sides,
    not whatever the host happens to have installed. It is existence-checked, so
    outside the research tree (a vendored copy of this oracle shipped alongside
    the crate) resolution simply continues to PATH instead of naming a file that
    cannot exist.

    Falls back to the bare name `zopflipng` when nothing resolves, so the caller's
    existing "not an executable file" diagnostic is unchanged in shape.
    """
    override = os.environ.get("PRISM_ZOPFLIPNG")
    if override:
        return Path(override)
    if _IN_TREE_ZOPFLIPNG.is_file():
        return _IN_TREE_ZOPFLIPNG
    found = shutil.which("zopflipng")
    return Path(found) if found else Path("zopflipng")


DEFAULT_ZOPFLIPNG = _resolve_zopflipng()


class PackError(ValueError):
    """Clean failure for invalid input, emission, or external optimization."""


@dataclass(frozen=True)
class CleanupEvidence:
    input_entries: int
    used_entries: int
    output_entries: int
    unused_removed: int
    duplicate_rgba_removed: int


@dataclass(frozen=True)
class PackingVariantEvidence:
    """Facts about one complete pre-optimizer PNG artifact."""

    order_strategy: str
    filter_strategy: str
    bit_depth: int
    row_filters: tuple[int, ...]
    filter_histogram: tuple[int, int, int, int, int]
    palette_entries: int
    palette_rgba_sha256: str
    plte_data_bytes: int
    trns_data_bytes: int
    idat_data_bytes: int
    png_bytes: int
    sha256: str
    pixel_identical: bool


@dataclass(frozen=True)
class OptimizerEvidence:
    """Separates the selected pre-optimizer artifact from observed output."""

    mode: str
    invoked: bool
    binary_path: str | None
    binary_sha256: str | None
    pinned_commit: str | None
    license_expression: str | None
    argv_template: tuple[str, ...] | None
    input_bytes: int
    input_sha256: str
    output_bytes: int
    output_sha256: str
    output_color_type: int
    output_bit_depth: int
    output_palette_entries: int | None
    output_palette_sha256: str | None
    output_plte_data_bytes: int
    output_trns_data_bytes: int
    output_idat_data_bytes: int
    output_filter_histogram: tuple[int, int, int, int, int] | None
    pixel_identical: bool


@dataclass(frozen=True)
class PackingSearchEvidence:
    """Declared budget and observed work for one deterministic search."""

    version: str
    seed_orders: tuple[str, ...]
    seed_filter_strategies: tuple[str, ...]
    max_pre_optimizer_variants: int
    pre_optimizer_variants_encoded: int
    local_move_limit: int
    local_moves_tested: int
    no_improvement_limit: int
    row_change_limit: int
    row_changes_tested: int
    zopflipng_finalist_limit: int
    zopflipng_candidates_tested: int
    selection_boundary: str


@dataclass(frozen=True)
class PackResult:
    """Final artifact plus pre/post evidence and the chosen pre-pack mapping."""

    data: bytes
    palette: tuple[Pixel, ...]
    indices: tuple[int, ...]
    cleanup: CleanupEvidence
    selected_pre_optimizer: PackingVariantEvidence
    pre_optimizer_portfolio: tuple[PackingVariantEvidence, ...]
    optimizer: OptimizerEvidence
    optimizer_portfolio: tuple[OptimizerEvidence, ...]
    search: PackingSearchEvidence


@dataclass(frozen=True)
class _Variant:
    evidence: PackingVariantEvidence
    data: bytes
    palette: tuple[Pixel, ...]
    indices: tuple[int, ...]


def _normalize_inputs(
    width: int,
    height: int,
    palette: Sequence[Pixel],
    indices: Sequence[int],
) -> tuple[tuple[Pixel, ...], tuple[int, ...]]:
    if (
        isinstance(width, bool)
        or isinstance(height, bool)
        or not isinstance(width, int)
        or not isinstance(height, int)
        or width < 1
        or height < 1
    ):
        raise PackError("width and height must be integers >= 1")
    if not isinstance(palette, (list, tuple)) or not 1 <= len(palette) <= 256:
        raise PackError("palette must contain 1..256 entries")
    normalized_palette: list[Pixel] = []
    for entry in palette:
        if not isinstance(entry, (list, tuple)) or len(entry) != 4:
            raise PackError("palette entries must be RGBA four-tuples")
        channels: list[int] = []
        for channel in entry:
            if isinstance(channel, bool) or not isinstance(channel, int) or not 0 <= channel <= 255:
                raise PackError("palette channels must be integers in 0..255")
            channels.append(channel)
        normalized_palette.append(tuple(channels))  # type: ignore[arg-type]
    if len(indices) != width * height:
        raise PackError(f"expected {width * height} indices, got {len(indices)}")
    normalized_indices: list[int] = []
    for index in indices:
        if (
            isinstance(index, bool)
            or not isinstance(index, int)
            or not 0 <= index < len(normalized_palette)
        ):
            raise PackError(f"palette index {index!r} out of range")
        normalized_indices.append(index)
    return tuple(normalized_palette), tuple(normalized_indices)


def cleanup_palette(
    palette: Sequence[Pixel], indices: Sequence[int]
) -> tuple[tuple[Pixel, ...], tuple[int, ...], CleanupEvidence]:
    """Remove unused and duplicate RGBA entries while preserving pixels.

    Used input entries are visited in original index order.  Identical RGBA
    values collapse to their first used representative, producing a stable
    canonical mapping independent of dictionary iteration order.
    """

    if not palette:
        raise PackError("palette must not be empty")
    used = sorted(set(indices))
    if not used:
        raise PackError("at least one palette index is required")
    if used[-1] >= len(palette) or used[0] < 0:
        raise PackError("palette index out of range during cleanup")
    output: list[Pixel] = []
    rgba_to_output: dict[Pixel, int] = {}
    old_to_output: dict[int, int] = {}
    duplicates = 0
    for old_index in used:
        raw_entry = palette[old_index]
        if not isinstance(raw_entry, (list, tuple)) or len(raw_entry) != 4:
            raise PackError("palette entries must be RGBA four-tuples")
        channels: list[int] = []
        for channel in raw_entry:
            if (
                isinstance(channel, bool)
                or not isinstance(channel, int)
                or not 0 <= channel <= 255
            ):
                raise PackError("palette channels must be integers in 0..255")
            channels.append(channel)
        entry: Pixel = tuple(channels)  # type: ignore[assignment]
        new_index = rgba_to_output.get(entry)
        if new_index is None:
            new_index = len(output)
            rgba_to_output[entry] = new_index
            output.append(entry)
        else:
            duplicates += 1
        old_to_output[old_index] = new_index
    remapped = tuple(old_to_output[index] for index in indices)
    evidence = CleanupEvidence(
        input_entries=len(palette),
        used_entries=len(used),
        output_entries=len(output),
        unused_removed=len(palette) - len(used),
        duplicate_rgba_removed=duplicates,
    )
    return tuple(output), remapped, evidence


def minimum_bit_depth(palette_entries: int) -> int:
    """Smallest legal PNG indexed bit depth for ``palette_entries``."""

    if isinstance(palette_entries, bool) or not isinstance(palette_entries, int):
        raise PackError("palette entry count must be an integer")
    if not 1 <= palette_entries <= 256:
        raise PackError("palette entry count must be in 1..256")
    if palette_entries <= 2:
        return 1
    if palette_entries <= 4:
        return 2
    if palette_entries <= 16:
        return 4
    return 8


def pack_index_row(indices: Sequence[int], bit_depth: int) -> bytes:
    """Pack one indexed scanline MSB-first with deterministic zero padding."""

    if bit_depth not in (1, 2, 4, 8):
        raise PackError("indexed bit depth must be 1, 2, 4, or 8")
    limit = 1 << bit_depth
    for index in indices:
        if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index < limit:
            raise PackError(f"index {index!r} does not fit bit depth {bit_depth}")
    if bit_depth == 8:
        return bytes(indices)
    output = bytearray((len(indices) * bit_depth + 7) // 8)
    for position, index in enumerate(indices):
        bit_offset = position * bit_depth
        shift = 8 - bit_depth - (bit_offset % 8)
        output[bit_offset // 8] |= index << shift
    return bytes(output)


def _paeth(left: int, up: int, upper_left: int) -> int:
    estimate = left + up - upper_left
    distance_left = abs(estimate - left)
    distance_up = abs(estimate - up)
    distance_upper_left = abs(estimate - upper_left)
    if distance_left <= distance_up and distance_left <= distance_upper_left:
        return left
    if distance_up <= distance_upper_left:
        return up
    return upper_left


def filter_row(row: bytes, previous: bytes | None, filter_type: int, bpp: int = 1) -> bytes:
    """Apply one PNG filter to serialized bytes, modulo 256."""

    if filter_type not in range(5):
        raise PackError("filter type must be 0..4")
    if isinstance(bpp, bool) or not isinstance(bpp, int) or bpp < 1:
        raise PackError("bpp must be an integer >= 1")
    prior = bytes(len(row)) if previous is None else previous
    if len(prior) != len(row):
        raise PackError("previous row length differs from current row")
    output = bytearray(len(row))
    for offset, value in enumerate(row):
        left = row[offset - bpp] if offset >= bpp else 0
        up = prior[offset]
        upper_left = prior[offset - bpp] if offset >= bpp else 0
        if filter_type == 0:
            predictor = 0
        elif filter_type == 1:
            predictor = left
        elif filter_type == 2:
            predictor = up
        elif filter_type == 3:
            predictor = (left + up) // 2
        else:
            predictor = _paeth(left, up, upper_left)
        output[offset] = (value - predictor) & 0xFF
    return bytes(output)


def _residual_score(filtered: bytes) -> int:
    return sum(value if value < 128 else 256 - value for value in filtered)


def select_row_filters(
    rows: Sequence[bytes], strategy: str
) -> tuple[bytes, tuple[int, ...]]:
    """Serialize filtered rows for a fixed strategy or signed-residual rule."""

    if strategy not in FILTER_STRATEGIES:
        raise PackError(f"unknown filter strategy: {strategy}")
    output = bytearray()
    choices: list[int] = []
    previous: bytes | None = None
    for row in rows:
        if strategy == "residual":
            candidates = tuple(filter_row(row, previous, kind, 1) for kind in range(5))
            chosen = min(range(5), key=lambda kind: (_residual_score(candidates[kind]), kind))
            filtered = candidates[chosen]
        else:
            chosen = FILTER_NAMES.index(strategy)
            filtered = filter_row(row, previous, chosen, 1)
        output.append(chosen)
        output.extend(filtered)
        choices.append(chosen)
        previous = row
    return bytes(output), tuple(choices)


def _serialize_row_filter_choices(
    rows: Sequence[bytes], choices: Sequence[int]
) -> tuple[bytes, tuple[int, ...]]:
    if len(rows) != len(choices):
        raise PackError("row-filter choice count differs from image height")
    output = bytearray()
    previous: bytes | None = None
    normalized: list[int] = []
    for row, choice in zip(rows, choices):
        if isinstance(choice, bool) or not isinstance(choice, int) or choice not in range(5):
            raise PackError("row-filter choices must be integers in 0..4")
        output.append(choice)
        output.extend(filter_row(row, previous, choice, 1))
        normalized.append(choice)
        previous = row
    return bytes(output), tuple(normalized)


def _trial_compression_row_filters(
    rows: Sequence[bytes],
) -> tuple[bytes, tuple[int, ...]]:
    """Choose each row by a copied zlib state and deterministic sync probe.

    The copied compressor is flushed only for scoring; the retained state sees
    the chosen row without an inserted flush boundary.  This implements ch05
    §8.3's compressor-state trial heuristic while the resulting whole artifact
    is still judged by its actual final bytes.
    """

    compressor = zlib.compressobj(level=9)
    output = bytearray()
    choices: list[int] = []
    previous: bytes | None = None
    for row in rows:
        candidates = tuple(filter_row(row, previous, kind, 1) for kind in range(5))
        records = tuple(bytes((kind,)) + candidates[kind] for kind in range(5))
        costs: list[int] = []
        for record in records:
            probe = compressor.copy()
            costs.append(len(probe.compress(record) + probe.flush(zlib.Z_SYNC_FLUSH)))
        chosen = min(range(5), key=lambda kind: (costs[kind], kind))
        compressor.compress(records[chosen])
        output.extend(records[chosen])
        choices.append(chosen)
        previous = row
    return bytes(output), tuple(choices)


def _frequency(indices: Sequence[int], count: int) -> tuple[int, ...]:
    frequencies = [0] * count
    for index in indices:
        frequencies[index] += 1
    return tuple(frequencies)


def _spatial_order(
    palette: Sequence[Pixel],
    indices: Sequence[int],
    width: int,
    height: int,
    members: Sequence[int] | None = None,
) -> list[int]:
    count = len(palette)
    frequency = _frequency(indices, count)
    adjacency = [[0] * count for _ in range(count)]
    for y in range(height):
        base = y * width
        for x in range(width):
            here = indices[base + x]
            if x + 1 < width:
                other = indices[base + x + 1]
                if here != other:
                    adjacency[here][other] += 1
                    adjacency[other][here] += 1
            if y + 1 < height:
                other = indices[base + width + x]
                if here != other:
                    adjacency[here][other] += 1
                    adjacency[other][here] += 1
    candidates = tuple(range(count)) if members is None else tuple(members)
    if not candidates:
        return []
    start = min(candidates, key=lambda index: (-frequency[index], palette[index], index))
    order = [start]
    remaining = set(candidates)
    remaining.remove(start)
    while remaining:
        last = order[-1]
        chosen = min(
            remaining,
            key=lambda index: (
                -adjacency[last][index],
                -sum(adjacency[placed][index] for placed in order),
                -frequency[index],
                palette[index],
                index,
            ),
        )
        order.append(chosen)
        remaining.remove(chosen)
    return order


def _color_locality_order(
    palette: Sequence[Pixel], members: Sequence[int] | None = None
) -> list[int]:
    """Deterministic nearest-neighbor path through straight RGBA bytes."""

    count = len(palette)
    candidates = tuple(range(count)) if members is None else tuple(members)
    if not candidates:
        return []
    start = min(candidates, key=lambda index: (palette[index], index))
    order = [start]
    remaining = set(candidates)
    remaining.remove(start)
    while remaining:
        last = palette[order[-1]]

        def key(index: int) -> tuple[int, Pixel, int]:
            entry = palette[index]
            distance = sum((left - right) ** 2 for left, right in zip(last, entry))
            return distance, entry, index

        chosen = min(remaining, key=key)
        order.append(chosen)
        remaining.remove(chosen)
    return order


def _alpha_partitions(palette: Sequence[Pixel]) -> tuple[tuple[int, ...], tuple[int, ...]]:
    """Return nonopaque then opaque members for a compact legal tRNS prefix."""

    return (
        tuple(index for index, entry in enumerate(palette) if entry[3] < 255),
        tuple(index for index, entry in enumerate(palette) if entry[3] == 255),
    )


def _packed_frequency_order(
    palette: Sequence[Pixel], indices: Sequence[int], width: int, height: int
) -> list[int]:
    """Group frequent co-occurring indices into deterministic packed-byte bands.

    Ch06 §9.5 calls for optimizing complete packed bytes below eight-bit index
    depth.  This bounded seed assigns the most frequent remaining entry at each
    packed-byte boundary, then fills that byte's remaining numeric slots by
    observed within-byte co-occurrence.  At eight bits it deliberately reduces
    to frequency order.
    """

    count = len(palette)
    frequency = _frequency(indices, count)
    per_byte = 8 // minimum_bit_depth(count)
    if per_byte == 1:
        return sorted(range(count), key=lambda index: (-frequency[index], index))
    cooccurrence = [[0] * count for _ in range(count)]
    for y in range(height):
        row = indices[y * width : (y + 1) * width]
        for start in range(0, width, per_byte):
            group = row[start : start + per_byte]
            for left_position, left in enumerate(group):
                for right in group[left_position + 1 :]:
                    if left != right:
                        cooccurrence[left][right] += 1
                        cooccurrence[right][left] += 1
    order: list[int] = []
    remaining = set(range(count))
    while remaining:
        within_byte = len(order) % per_byte
        if within_byte == 0:
            chosen = min(
                remaining,
                key=lambda index: (-frequency[index], palette[index], index),
            )
        else:
            current_group = order[-within_byte:]
            chosen = min(
                remaining,
                key=lambda index: (
                    -sum(cooccurrence[placed][index] for placed in current_group),
                    -frequency[index],
                    palette[index],
                    index,
                ),
            )
        order.append(chosen)
        remaining.remove(chosen)
    return order


def permute_palette(
    palette: Sequence[Pixel],
    indices: Sequence[int],
    width: int,
    height: int,
    strategy: str,
) -> tuple[tuple[Pixel, ...], tuple[int, ...]]:
    """Apply a deterministic bijective palette order and remap indices."""

    if strategy not in V2_ORDER_STRATEGIES:
        raise PackError(f"unknown palette-order strategy: {strategy}")
    count = len(palette)
    if count < 1:
        raise PackError("palette must not be empty")
    if len(indices) != width * height:
        raise PackError("index count does not match dimensions")
    frequency = _frequency(indices, count)
    if strategy == "identity":
        order = list(range(count))
    elif strategy == "alpha-first":
        order = sorted(
            range(count),
            key=lambda index: (palette[index][3] == 255, palette[index][3], index),
        )
    elif strategy == "frequency":
        order = sorted(range(count), key=lambda index: (-frequency[index], index))
    elif strategy == "color-locality":
        order = _color_locality_order(palette)
    elif strategy == "spatial-adjacency":
        order = _spatial_order(palette, indices, width, height)
    elif strategy == "alpha-frequency":
        nonopaque, opaque = _alpha_partitions(palette)
        order = sorted(nonopaque, key=lambda index: (-frequency[index], palette[index][3], index))
        order += sorted(opaque, key=lambda index: (-frequency[index], index))
    elif strategy == "alpha-color-locality":
        nonopaque, opaque = _alpha_partitions(palette)
        order = _color_locality_order(palette, nonopaque)
        order += _color_locality_order(palette, opaque)
    elif strategy == "alpha-spatial-adjacency":
        nonopaque, opaque = _alpha_partitions(palette)
        order = _spatial_order(palette, indices, width, height, nonopaque)
        order += _spatial_order(palette, indices, width, height, opaque)
    else:
        order = _packed_frequency_order(palette, indices, width, height)
    inverse = [0] * count
    for new_index, old_index in enumerate(order):
        inverse[old_index] = new_index
    return tuple(palette[index] for index in order), tuple(inverse[index] for index in indices)


def _chunk(kind: bytes, payload: bytes) -> bytes:
    crc = binascii.crc32(kind + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)


def _encode_variant(
    width: int,
    height: int,
    palette: tuple[Pixel, ...],
    indices: tuple[int, ...],
    order_strategy: str,
    filter_strategy: str,
    row_filter_choices: Sequence[int] | None = None,
) -> _Variant:
    bit_depth = minimum_bit_depth(len(palette))
    rows = tuple(
        pack_index_row(indices[y * width : (y + 1) * width], bit_depth)
        for y in range(height)
    )
    if row_filter_choices is not None:
        scanlines, row_filters = _serialize_row_filter_choices(rows, row_filter_choices)
    elif filter_strategy == "trial-zlib":
        scanlines, row_filters = _trial_compression_row_filters(rows)
    else:
        scanlines, row_filters = select_row_filters(rows, filter_strategy)
    compressed = zlib.compress(scanlines, 9)
    ihdr = struct.pack(">IIBBBBB", width, height, bit_depth, 3, 0, 0, 0)
    plte = bytes(channel for red, green, blue, _ in palette for channel in (red, green, blue))
    last_nonopaque = max(
        (index for index, entry in enumerate(palette) if entry[3] < 255),
        default=-1,
    )
    trns = (
        bytes(entry[3] for entry in palette[: last_nonopaque + 1])
        if last_nonopaque >= 0
        else b""
    )
    output = bytearray(m1_png.PNG_SIGNATURE)
    output += _chunk(b"IHDR", ihdr)
    output += _chunk(b"PLTE", plte)
    if trns:
        output += _chunk(b"tRNS", trns)
    output += _chunk(b"IDAT", compressed)
    output += _chunk(b"IEND", b"")
    data = bytes(output)
    try:
        decoded = m1_png.decode_png(data)
    except m1_png.PngError as error:
        raise PackError(f"independent decode of packing variant failed: {error}") from error
    expected_pixels = tuple(palette[index] for index in indices)
    if (decoded.width, decoded.height) != (width, height):
        raise PackError("packing variant decoded with different dimensions")
    if decoded.pixels != expected_pixels:
        raise PackError("packing variant failed decoded-pixel identity verification")
    histogram = tuple(row_filters.count(kind) for kind in range(5))
    evidence = PackingVariantEvidence(
        order_strategy=order_strategy,
        filter_strategy=filter_strategy,
        bit_depth=bit_depth,
        row_filters=row_filters,
        filter_histogram=histogram,  # type: ignore[arg-type]
        palette_entries=len(palette),
        palette_rgba_sha256=hashlib.sha256(
            bytes(channel for entry in palette for channel in entry)
        ).hexdigest(),
        plte_data_bytes=len(plte),
        trns_data_bytes=len(trns),
        idat_data_bytes=len(compressed),
        png_bytes=len(data),
        sha256=hashlib.sha256(data).hexdigest(),
        pixel_identical=True,
    )
    return _Variant(evidence, data, palette, indices)


def _spread_positions(length: int, count: int) -> tuple[int, ...]:
    """Select up to ``count`` stable positions spanning ``range(length)``."""

    if length <= 0 or count <= 0:
        return ()
    if length <= count:
        return tuple(range(length))
    return tuple((step * (length - 1)) // (count - 1) for step in range(count))


def _apply_position_order(
    palette: tuple[Pixel, ...],
    indices: tuple[int, ...],
    position_order: Sequence[int],
) -> tuple[tuple[Pixel, ...], tuple[int, ...]]:
    count = len(palette)
    if tuple(sorted(position_order)) != tuple(range(count)):
        raise PackError("local palette move is not bijective")
    inverse = [0] * count
    for new_index, old_index in enumerate(position_order):
        inverse[old_index] = new_index
    return (
        tuple(palette[index] for index in position_order),
        tuple(inverse[index] for index in indices),
    )


def _local_position_moves(palette: tuple[Pixel, ...]) -> tuple[tuple[str, tuple[int, ...]], ...]:
    """Bounded A5 neighborhood: swaps, insertions, blocks, alpha-safe swaps."""

    count = len(palette)
    if count < 2:
        return ()
    identity = tuple(range(count))
    moves: list[tuple[str, tuple[int, ...]]] = []
    seen = {identity}

    def add(label: str, order: list[int]) -> None:
        candidate = tuple(order)
        if candidate not in seen and len(moves) < V2_LOCAL_MOVE_LIMIT:
            seen.add(candidate)
            moves.append((label, candidate))

    for position in _spread_positions(count - 1, 7):
        order = list(identity)
        order[position], order[position + 1] = order[position + 1], order[position]
        add(f"swap-{position}-{position + 1}", order)

    distance = max(2, count // 7)
    for source in _spread_positions(count, 6):
        target = source + distance if source + distance < count else max(0, source - distance)
        order = list(identity)
        entry = order.pop(source)
        order.insert(target, entry)
        add(f"insert-{source}-{target}", order)

    if count >= 4:
        for start in _spread_positions(count - 3, 4):
            order = list(identity)
            order[start : start + 4] = order[start + 2 : start + 4] + order[start : start + 2]
            add(f"block2-{start}-{start + 2}", order)

    opacity_groups = (
        tuple(index for index, entry in enumerate(palette) if entry[3] < 255),
        tuple(index for index, entry in enumerate(palette) if entry[3] == 255),
    )
    for group in opacity_groups:
        for offset in _spread_positions(len(group) - 2, 3):
            left = group[offset]
            right = group[offset + 2]
            order = list(identity)
            order[left], order[right] = order[right], order[left]
            add(f"alpha-preserving-swap-{left}-{right}", order)
    return tuple(moves)


def _build_v2_variants(
    width: int,
    height: int,
    palette: tuple[Pixel, ...],
    indices: tuple[int, ...],
) -> tuple[list[_Variant], int, int]:
    """Run the fixed-budget chapter-19 A5 pre-optimizer search."""

    variants: list[_Variant] = []
    for order_strategy in V2_ORDER_STRATEGIES:
        ordered_palette, ordered_indices = permute_palette(
            palette, indices, width, height, order_strategy
        )
        for filter_strategy in V2_FILTER_STRATEGIES:
            variants.append(
                _encode_variant(
                    width,
                    height,
                    ordered_palette,
                    ordered_indices,
                    order_strategy,
                    filter_strategy,
                )
            )

    current = min(enumerate(variants), key=lambda pair: (len(pair[1].data), pair[0]))[1]
    local_moves_tested = 0
    consecutive_without_improvement = 0
    for move_name, position_order in _local_position_moves(current.palette):
        if len(variants) >= V2_MAX_PRE_OPTIMIZER_VARIANTS:
            break
        moved_palette, moved_indices = _apply_position_order(
            current.palette, current.indices, position_order
        )
        candidate = _encode_variant(
            width,
            height,
            moved_palette,
            moved_indices,
            f"local-{move_name}",
            "row-carry",
            current.evidence.row_filters,
        )
        variants.append(candidate)
        local_moves_tested += 1
        if len(candidate.data) < len(current.data):
            current = candidate
            consecutive_without_improvement = 0
        else:
            consecutive_without_improvement += 1

        # A5 calls for periodically retesting alternate row assignments as the
        # order changes.  Two complete-artifact checks every eight moves keep
        # that interaction inside the same fixed encode budget.
        if local_moves_tested % 8 == 0:
            for strategy in ("residual", "trial-zlib"):
                if len(variants) >= V2_MAX_PRE_OPTIMIZER_VARIANTS:
                    break
                alternate = _encode_variant(
                    width,
                    height,
                    current.palette,
                    current.indices,
                    current.evidence.order_strategy,
                    strategy,
                )
                variants.append(alternate)
                if len(alternate.data) < len(current.data):
                    current = alternate
                    consecutive_without_improvement = 0
        if consecutive_without_improvement >= V2_NO_IMPROVEMENT_LIMIT:
            break

    current = min(enumerate(variants), key=lambda pair: (len(pair[1].data), pair[0]))[1]
    row_changes_tested = 0
    row_choices = list(current.evidence.row_filters)
    row_budget = min(
        V2_ROW_CHANGE_LIMIT,
        V2_MAX_PRE_OPTIMIZER_VARIANTS - len(variants),
    )
    rows_to_try = max(0, row_budget // 4)
    row_positions = _spread_positions(height, rows_to_try)
    for row_position in row_positions:
        original_choice = row_choices[row_position]
        for filter_type in range(5):
            if filter_type == original_choice or row_changes_tested >= row_budget:
                continue
            candidate_choices = list(row_choices)
            candidate_choices[row_position] = filter_type
            candidate = _encode_variant(
                width,
                height,
                current.palette,
                current.indices,
                current.evidence.order_strategy,
                "row-search",
                candidate_choices,
            )
            variants.append(candidate)
            row_changes_tested += 1
            if len(candidate.data) < len(current.data):
                current = candidate
                row_choices = candidate_choices
    if len(variants) > V2_MAX_PRE_OPTIMIZER_VARIANTS:
        raise PackError("internal: v2 packing search exceeded its declared budget")
    return variants, local_moves_tested, row_changes_tested


def _assert_pixel_identity(reference: bytes, candidate: bytes) -> m1_png.DecodedImage:
    try:
        expected = m1_png.decode_png(reference)
        observed = m1_png.decode_png(candidate)
    except m1_png.PngError as error:
        raise PackError(f"independent decode failed: {error}") from error
    if (expected.width, expected.height) != (observed.width, observed.height):
        raise PackError("optimizer changed decoded dimensions")
    if expected.pixels != observed.pixels:
        for offset, (left, right) in enumerate(zip(expected.pixels, observed.pixels)):
            if left != right:
                x = offset % expected.width
                y = offset // expected.width
                raise PackError(
                    f"optimizer changed decoded pixel ({x},{y}): {left} != {right}"
                )
        raise PackError("optimizer changed decoded pixel count")
    return observed


def _observed_artifact_facts(
    data: bytes, decoded: m1_png.DecodedImage
) -> tuple[int | None, str | None, int, int, int, tuple[int, int, int, int, int] | None]:
    """Inspect representation facts from the actual final PNG bytes.

    Zopfli is an external PNG optimizer and may rewrite palette order, color
    type, bit depth, or filters.  Pre-optimizer choices therefore cannot be
    presented as final-output facts.  This parser runs only after the
    independent decoder accepted the datastream; it records final PLTE/tRNS/
    IDAT payloads and, for the emitted non-interlaced form, row-filter counts.
    """
    offset = len(m1_png.PNG_SIGNATURE)
    plte = b""
    trns = b""
    idat_parts: list[bytes] = []
    while offset < len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4 : offset + 8]
        payload = data[offset + 8 : offset + 8 + length]
        if kind == b"PLTE":
            plte = payload
        elif kind == b"tRNS":
            trns = payload
        elif kind == b"IDAT":
            idat_parts.append(payload)
        offset += 12 + length
    palette_entries = len(plte) // 3 if plte else None
    if plte:
        rgba_palette = bytearray()
        for index in range(len(plte) // 3):
            rgba_palette.extend(plte[index * 3 : index * 3 + 3])
            rgba_palette.append(trns[index] if index < len(trns) else 255)
        palette_hash = hashlib.sha256(bytes(rgba_palette)).hexdigest()
    else:
        palette_hash = None
    filter_histogram: tuple[int, int, int, int, int] | None = None
    if not decoded.properties["interlaced"]:
        channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[decoded.properties["color_type"]]
        row_bytes = (decoded.width * channels * decoded.properties["bit_depth"] + 7) // 8
        scanlines = zlib.decompress(b"".join(idat_parts))
        stride = 1 + row_bytes
        choices = tuple(scanlines[row * stride] for row in range(decoded.height))
        if len(scanlines) != decoded.height * stride or any(choice > 4 for choice in choices):
            raise PackError("final PNG has inconsistent non-interlaced filter bytes")
        filter_histogram = tuple(choices.count(kind) for kind in range(5))  # type: ignore[assignment]
    return (
        palette_entries,
        palette_hash,
        len(plte),
        len(trns),
        sum(len(part) for part in idat_parts),
        filter_histogram,
    )


def _retruncate_trns(data: bytes, decoded: m1_png.DecodedImage) -> bytes | None:
    """Trim a palette tRNS chunk's genuinely-opaque trailing bytes, if any.

    E-0018 (T-0123, ``finalist-trace.json``) demonstrated that pack v2's
    alpha-aware order strategies reach the zopfli finalist stage at the
    truncation-optimal tRNS length (payload trimmed to
    ``last_nonopaque_index + 1``, exactly as ``m1_png.write_indexed_png``
    and ``_encode_variant`` above both compute), while the pinned
    ``zopflipng`` binary's own output has a longer tRNS (dice 221->247,
    globe 159->201, alphaball 129->145). That trace still stands. What does
    NOT stand (T-0132 remeasurement, cross-reviewed and reconciled; see
    ``experiments/E-0018-trns-truncation/README.md``'s T-0132 correction
    note and ``T-0132-post-zopfli-retruncation.md``) is the inference that
    those extra bytes are recoverable padding: zopflipng does not preserve
    the caller's palette order, and on every unit checked its output tRNS
    is already minimal for the order it actually converged to -- the
    trailing bytes are genuinely non-opaque entries in zopflipng's chosen
    order, not padding, and there is nothing to trim.

    This function is therefore a conservative, currently-inert safety net,
    not a recovered-bytes optimization: per the PNG spec (11.2.3), palette
    entries beyond the tRNS payload are implicitly fully opaque (255)
    whether the byte is physically present or not, so IF a chunk's own
    trailing bytes ever are genuinely opaque padding (not observed in the
    corpus measured for T-0132, but not ruled out for some future
    zopflipng version, order, or image), trimming them is a legal,
    pixel-preserving rewrite. This performs exactly that trim as a raw
    chunk edit (payload slice, updated length, recomputed CRC) and returns
    the rewritten bytes, or ``None`` when there is nothing to shrink (color
    type != 3, no tRNS chunk, an entirely-opaque tRNS, or a tRNS already at
    its truncation optimum -- the case measured for every unit tested so
    far). This function only performs the byte rewrite -- callers MUST
    independently re-verify decode identity before adopting the result.
    """
    if decoded.properties["color_type"] != 3:
        return None
    offset = len(m1_png.PNG_SIGNATURE)
    trns_offset: int | None = None
    trns_length = 0
    trns_payload = b""
    while offset < len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4 : offset + 8]
        if kind == b"tRNS":
            trns_offset = offset
            trns_length = length
            trns_payload = data[offset + 8 : offset + 8 + length]
            break
        offset += 12 + length
    if trns_offset is None or not trns_payload:
        return None
    last_nonopaque = max(
        (index for index, alpha in enumerate(trns_payload) if alpha < 255),
        default=-1,
    )
    new_length = last_nonopaque + 1
    if new_length <= 0 or new_length >= trns_length:
        return None
    new_payload = trns_payload[:new_length]
    new_chunk = (
        struct.pack(">I", new_length)
        + b"tRNS"
        + new_payload
        + struct.pack(">I", binascii.crc32(b"tRNS" + new_payload) & 0xFFFFFFFF)
    )
    return data[:trns_offset] + new_chunk + data[trns_offset + 12 + trns_length :]


def _run_zopflipng(
    input_png: bytes,
    binary: Path,
    timeout_seconds: float,
    extra_arguments: Sequence[str] = (),
) -> tuple[bytes, OptimizerEvidence]:
    if timeout_seconds <= 0:
        raise PackError("zopflipng timeout must be positive")
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise PackError(f"zopflipng is not an executable file: {binary}")
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="prism-pack-zopfli-") as temporary:
        temporary_path = Path(temporary)
        source = temporary_path / "input.png"
        output = temporary_path / "output.png"
        source.write_bytes(input_png)
        argv = [str(binary), "-y", *extra_arguments, str(source), str(output)]
        try:
            completed = subprocess.run(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=timeout_seconds,
                check=False,
                shell=False,
            )
        except subprocess.TimeoutExpired as error:
            raise PackError(f"zopflipng timed out after {timeout_seconds:g}s") from error
        except OSError as error:
            raise PackError(f"cannot execute zopflipng: {error}") from error
        if completed.returncode != 0:
            diagnostic = (completed.stderr or completed.stdout).strip().splitlines()
            detail = diagnostic[0] if diagnostic else "no diagnostic output"
            raise PackError(f"zopflipng exited {completed.returncode}: {detail}")
        if not output.is_file():
            raise PackError("zopflipng succeeded without creating output")
        optimized = output.read_bytes()
    observed = _assert_pixel_identity(input_png, optimized)
    rewritten = _retruncate_trns(optimized, observed)
    if rewritten is not None:
        try:
            rewritten_observed = _assert_pixel_identity(input_png, rewritten)
        except PackError:
            # Legal-rewrite guard: keep the un-truncated zopflipng artifact
            # if the re-truncated bytes fail their own independent
            # decode-identity check.  Never adopt an unverified rewrite.
            pass
        else:
            optimized = rewritten
            observed = rewritten_observed
    (
        palette_entries,
        palette_hash,
        plte_bytes,
        trns_bytes,
        idat_bytes,
        filter_histogram,
    ) = _observed_artifact_facts(optimized, observed)
    evidence = OptimizerEvidence(
        mode="zopfli",
        invoked=True,
        binary_path=str(binary),
        binary_sha256=binary_hash,
        pinned_commit=ZOPFLI_PINNED_COMMIT,
        license_expression=ZOPFLI_LICENSE,
        argv_template=("zopflipng", "-y", *extra_arguments, "INPUT", "OUTPUT"),
        input_bytes=len(input_png),
        input_sha256=hashlib.sha256(input_png).hexdigest(),
        output_bytes=len(optimized),
        output_sha256=hashlib.sha256(optimized).hexdigest(),
        output_color_type=observed.properties["color_type"],
        output_bit_depth=observed.properties["bit_depth"],
        output_palette_entries=palette_entries,
        output_palette_sha256=palette_hash,
        output_plte_data_bytes=plte_bytes,
        output_trns_data_bytes=trns_bytes,
        output_idat_data_bytes=idat_bytes,
        output_filter_histogram=filter_histogram,
        pixel_identical=True,
    )
    return optimized, evidence


def pack_indexed_png(
    width: int,
    height: int,
    palette: Sequence[Pixel],
    indices: Sequence[int],
    *,
    mode: str = "fast",
    search_version: str = "v1",
    order_strategies: Sequence[str] = ORDER_STRATEGIES,
    filter_strategies: Sequence[str] = FILTER_STRATEGIES,
    zopflipng_path: str | os.PathLike[str] | None = None,
    timeout_seconds: float = 120.0,
) -> PackResult:
    """Search complete indexed-PNG artifacts and return the smallest.

    Palette cleanup is mandatory and pixel-preserving.  V1 encodes every
    requested order/filter combination and retains its historical behavior.
    V2 uses the declared module-level budget and fixed candidate family; custom
    V1 portfolios are therefore rejected with V2.  Selection always uses
    complete artifact bytes, with generation order as the deterministic tie.
    Maximum V2 mode independently optimizes up to three unique finalists and
    selects by the observed complete zopflipng output bytes.
    """

    normalized_palette, normalized_indices = _normalize_inputs(
        width, height, palette, indices
    )
    clean_palette, clean_indices, cleanup = cleanup_palette(
        normalized_palette, normalized_indices
    )
    if any(
        normalized_palette[old] != clean_palette[new]
        for old, new in zip(normalized_indices, clean_indices)
    ):
        raise PackError("palette cleanup changed decoded pixels")
    if mode not in ("fast", "max", "zopfli"):
        raise PackError("mode must be 'fast', 'max', or 'zopfli'")
    if search_version not in ("v1", "v2"):
        raise PackError("search_version must be 'v1' or 'v2'")
    if not order_strategies or not filter_strategies:
        raise PackError("packing portfolios must not be empty")
    if search_version == "v2" and (
        tuple(order_strategies) != ORDER_STRATEGIES
        or tuple(filter_strategies) != FILTER_STRATEGIES
    ):
        raise PackError("v2 uses its fixed declared portfolio; custom portfolios are v1-only")

    local_moves_tested = 0
    row_changes_tested = 0
    if search_version == "v1":
        variants: list[_Variant] = []
        for order_strategy in order_strategies:
            ordered_palette, ordered_indices = permute_palette(
                clean_palette,
                clean_indices,
                width,
                height,
                order_strategy,
            )
            for filter_strategy in filter_strategies:
                variants.append(
                    _encode_variant(
                        width,
                        height,
                        ordered_palette,
                        ordered_indices,
                        order_strategy,
                        filter_strategy,
                    )
                )
        seed_orders = tuple(order_strategies)
        seed_filters = tuple(filter_strategies)
        max_variants = len(seed_orders) * len(seed_filters)
        local_limit = 0
        no_improvement_limit = 0
        row_change_limit = 0
        zopflipng_limit = 1
    else:
        variants, local_moves_tested, row_changes_tested = _build_v2_variants(
            width, height, clean_palette, clean_indices
        )
        seed_orders = V2_ORDER_STRATEGIES
        seed_filters = V2_FILTER_STRATEGIES
        max_variants = V2_MAX_PRE_OPTIMIZER_VARIANTS
        local_limit = V2_LOCAL_MOVE_LIMIT
        no_improvement_limit = V2_NO_IMPROVEMENT_LIMIT
        row_change_limit = V2_ROW_CHANGE_LIMIT
        zopflipng_limit = V2_ZOPFLI_FINALIST_LIMIT

    selected = min(enumerate(variants), key=lambda pair: (len(pair[1].data), pair[0]))[1]
    optimizer_portfolio: list[OptimizerEvidence] = []
    if mode in ("max", "zopfli"):
        binary = Path(zopflipng_path) if zopflipng_path is not None else DEFAULT_ZOPFLIPNG
        ranked = sorted(
            enumerate(variants),
            key=lambda pair: (len(pair[1].data), pair[0]),
        )
        finalists: list[_Variant] = []
        finalist_palette_hashes: set[str] = set()
        for _position, variant in ranked:
            palette_digest = variant.evidence.palette_rgba_sha256
            if palette_digest in finalist_palette_hashes:
                continue
            finalist_palette_hashes.add(palette_digest)
            finalists.append(variant)
            if len(finalists) >= zopflipng_limit:
                break
        optimized_candidates: list[tuple[_Variant, bytes, OptimizerEvidence]] = []
        for finalist in finalists:
            optimized_data, observed_optimizer = _run_zopflipng(
                finalist.data,
                binary.resolve(),
                timeout_seconds,
                V2_ZOPFLI_ARGUMENTS if search_version == "v2" else (),
            )
            optimizer_portfolio.append(observed_optimizer)
            optimized_candidates.append((finalist, optimized_data, observed_optimizer))
        selected, final_data, optimizer = min(
            enumerate(optimized_candidates),
            key=lambda pair: (len(pair[1][1]), pair[0]),
        )[1]
        selection_boundary = "complete pinned-zopflipng PNG bytes"
    else:
        decoded = m1_png.decode_png(selected.data)
        digest = hashlib.sha256(selected.data).hexdigest()
        (
            palette_entries,
            palette_hash,
            plte_bytes,
            trns_bytes,
            idat_bytes,
            filter_histogram,
        ) = _observed_artifact_facts(selected.data, decoded)
        optimizer = OptimizerEvidence(
            mode="fast",
            invoked=False,
            binary_path=None,
            binary_sha256=None,
            pinned_commit=None,
            license_expression=None,
            argv_template=None,
            input_bytes=len(selected.data),
            input_sha256=digest,
            output_bytes=len(selected.data),
            output_sha256=digest,
            output_color_type=decoded.properties["color_type"],
            output_bit_depth=decoded.properties["bit_depth"],
            output_palette_entries=palette_entries,
            output_palette_sha256=palette_hash,
            output_plte_data_bytes=plte_bytes,
            output_trns_data_bytes=trns_bytes,
            output_idat_data_bytes=idat_bytes,
            output_filter_histogram=filter_histogram,
            pixel_identical=True,
        )
        final_data = selected.data
        optimizer_portfolio.append(optimizer)
        selection_boundary = "complete stdlib-zlib PNG bytes"
    search = PackingSearchEvidence(
        version=search_version,
        seed_orders=seed_orders,
        seed_filter_strategies=seed_filters,
        max_pre_optimizer_variants=max_variants,
        pre_optimizer_variants_encoded=len(variants),
        local_move_limit=local_limit,
        local_moves_tested=local_moves_tested,
        no_improvement_limit=no_improvement_limit,
        row_change_limit=row_change_limit,
        row_changes_tested=row_changes_tested,
        zopflipng_finalist_limit=zopflipng_limit,
        zopflipng_candidates_tested=len(optimizer_portfolio) if optimizer.invoked else 0,
        selection_boundary=selection_boundary,
    )
    return PackResult(
        data=final_data,
        palette=selected.palette,
        indices=selected.indices,
        cleanup=cleanup,
        selected_pre_optimizer=selected.evidence,
        pre_optimizer_portfolio=tuple(variant.evidence for variant in variants),
        optimizer=optimizer,
        optimizer_portfolio=tuple(optimizer_portfolio),
        search=search,
    )


__all__ = [
    "CleanupEvidence",
    "DEFAULT_ZOPFLIPNG",
    "FILTER_NAMES",
    "FILTER_STRATEGIES",
    "ORDER_STRATEGIES",
    "OptimizerEvidence",
    "PackError",
    "PackResult",
    "PackingSearchEvidence",
    "PackingVariantEvidence",
    "V2_FILTER_STRATEGIES",
    "V2_LOCAL_MOVE_LIMIT",
    "V2_MAX_PRE_OPTIMIZER_VARIANTS",
    "V2_NO_IMPROVEMENT_LIMIT",
    "V2_ORDER_STRATEGIES",
    "V2_ROW_CHANGE_LIMIT",
    "V2_ZOPFLI_FINALIST_LIMIT",
    "V2_ZOPFLI_ARGUMENTS",
    "ZOPFLI_LICENSE",
    "ZOPFLI_PINNED_COMMIT",
    "cleanup_palette",
    "filter_row",
    "minimum_bit_depth",
    "pack_index_row",
    "pack_indexed_png",
    "permute_palette",
    "select_row_filters",
]

"""pngprism 0.5.0 (reference oracle; module file kept as prism_quant.py) —
quantizer core with integrated dither and pack seams.

**Label: 0.5.0, unproven, metric-validated only.** The pipeline frame
is the T-0067 six-seam skeleton and the core remains the reviewed v0.1
algorithm of T-0068.  T-0094 only composes the reviewed T-0069/T-0080/T-0085
dither policies and T-0069/T-0083 pack searches around that unchanged core.
No quality claim is made or implied beyond the measured task evidence.

Pipeline frame (book ch17 §4.5 / ch19, six declared seams):

    decode -> sample -> palette init -> refinement -> remap -> emit

- decode: the lab's arbitrary-PNG path (m1_png).
- sample: identity in v0.1 — every pixel participates (declared;
  weighted/subsampled policies are a later stage's business).
- palette init: sparse factorized RGB/alpha initialization (ch19 §5
  contract A2): an alpha ladder with mandatory exact locks (ch19 §4 A1
  step 2) + weighted 1-D Lloyd interior levels; alpha-mass-weighted
  deterministic farthest-point RGB representatives with weighted Lloyd
  polish; observed-pair co-occurrence instantiation, mass-ranked and
  capped, with nearest-pair repair at remap.
- refinement: k-means-style joint Lloyd in premultiplied space (book
  ch11 §1-2) over a deterministic bin sample, under the declared
  convergence bounds (fixed-point stop or REFINE_MAX_ITERS).
- remap: nearest-entry assignment under the declared alpha-zone rules.
- emit: deterministic indexed PNG with tRNS alpha handling
  (m1_png.write_indexed_png).

v0.1 core declared rules (T-0068; every choice cites its contract):

1. ALPHA-AWARE DISTANCE. Squared Euclidean over premultiplied RGBA on the
   65025 scale: d(p,q) = sum_c(a_p*c_p - a_q*c_q)^2 + (255*a_p - 255*a_q)^2
   for c in {r,g,b}. This is exactly the numerator of the M1 harness
   metric ``premultiplied_linear_rgba_mse`` (in-repo original, T-0053),
   so refinement minimizes the measured objective. Premultiplied color
   collapses hidden RGB at alpha zero (book ch07: "arbitrary RGB at alpha
   zero collapses to zero premultiplied color"; ch07 §11: scale
   foreground color distance by alpha / compare premultiplied
   coordinates). Alpha is a first-class channel — the program's founding
   technical position.

2. OCCUPANCY-WEIGHTED CENTROIDS, NEVER GRID CENTERS (BINDING). Every bin
   carries actual member-pixel sums (count, sum_r/g/b/a, sum_ar/ag/ab);
   every palette value is the count-weighted mean of its actual members,
   rounded half-up. Consequence, pinned in tests: any source with <= N
   distinct colors quantized at --colors N is PIXEL-EXACT (exact path:
   distinct colors <= cap -> the palette IS the distinct color set).
   Channel extremes 0 and 255 are reachable palette values because the
   mean of a uniform-extreme cluster IS the extreme (BINDING).

3. ALPHA ZONES (BINDING alpha-extremes rule). Assignment is zoned: bins
   whose pixels are all a==0 use only the a==0 entry; all-a==255 bins use
   only a==255 entries; interior-alpha bins use only interior entries.
   Endpoint-isolated alpha binning (a==0 and a==255 always occupy their
   own alpha bins) makes the zone constraint per-pixel exact, so alpha==0
   stays 0 and alpha==255 stays 255, always — no fully-transparent pixel
   can gain visibility (compositing correctness; E-0001 motivation).
   Exactly one a==0 palette entry exists, policy-locked and never
   drifting. a==255 entries are lock-pinned at alpha 255 through centroid
   updates. Capacity degradation is one-directional: when a non-
   transparent zone has no entry (cap below the zone count), its bins map
   to the nearest non-transparent entry (visibility may be lost in a
   degenerate --colors 1 case; visibility is NEVER gained by a
   fully-transparent pixel).

4. HIDDEN-RGB POLICY HOOKS. The a==0 entry's RGB follows the declared
   policy: "canonicalize-black" (RGB=(0,0,0)) or "preserve-mean"
   (count-weighted mean of the hidden member RGB). The default remains
   "canonicalize-black". E-0001 bounded the evidence: it was the only tested
   byte-saver (median -3.7% with oxipng on affected images) but doubled fringe
   exposure under straight-channel resampling; every tested policy was harmless
   under correct premultiplied-linear resampling. Those findings are
   corpus-bounded, not a universal product-policy claim. "extend"/"hybrid"
   hooks remain deferred to follow-up work including E-0008.

CLI:

    prism-quant <in.png> <out.png> [--colors N] [--hidden-rgb-policy P]
        [--colors-search MIN..MAX@QUALITY]
        [--color-space srgb|oklab]
        [--adaptive-default off|on|guarded]
        [--dither off|on] [--dither-strength S]
        [--dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise]
        [--pack none|fast|max] [--pack-search v1|v2]

Exit statuses: 0 success; 2 usage; 3 data error (undecodable input,
invalid --colors/--hidden-rgb-policy); 5 input I/O error; 70 internal.
Errors go to stderr as one line of plain text; stdout stays empty on
failure.

CLEAN-ROOM (binding): no GPL-derived code. This implementation uses ONLY
in-repo original work plus public papers; the ch12 libimagequant GPL
source study was research context only and was NEVER consulted. Method
references (also logged in the T-0068 task evidence log for the T-0073
provenance record):

- Lloyd, "Least squares quantization in PCM", IEEE Trans. Information
  Theory, 1982 (public paper) -> 1-D alpha ladder + joint refinement.
- Gonzalez, "Clustering to minimize the maximum intercluster cost",
  Theoretical Computer Science, 1985 (public paper) -> deterministic
  farthest-point RGB seeding (deterministic k-means++ family, book ch11
  §8; Arthur & Vassilvitskii 2007 remains a declared forward lineage).
- book/07-color-spaces-for-transparent-quantization.md (in-repo) ->
  premultiplied distance feature space.
- book/11-neuquant-kmeans-and-local-refinement.md (in-repo) -> weighted
  Lloyd refinement, declared stop rules, empty-entry handling.
- book/19-novel-algorithm-portfolio.md A1/A2/A7 (in-repo contracts) ->
  alpha locks/ladder, sparse factorized init, refinement stop rules.
- book/04-hidden-rgb-matting-and-halos.md + experiments/E-0001 (in-repo)
  -> hidden-RGB policy vocabulary and the bounded evidence for the default.
- m1_metrics.premultiplied_linear_rgba_mse (in-repo original, T-0053) ->
  the exact objective the distance minimizes.
- experiments/E-0016-perceptual-distance (in-repo clean-room study) +
  Björn Ottosson's published 2021 Oklab matrices -> the opt-in Oklab
  assignment/refinement/remap feature space. The sRGB default is unchanged.
"""

from __future__ import annotations

import json
import math
import os
import secrets
import stat
import struct
import sys
import zlib
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

import m1_png

VERSION = "0.5.0"
LABEL = "0.5.0, unproven, metric-validated only"
# The ONE version source for the ``--version`` CLI flag (T-0210). Aliases
# VERSION so the pipeline identity and the flag never drift apart. The Rust
# port sources its own ``--version`` from CARGO_PKG_VERSION; as of the pngprism
# rename (T-0213) both implementations emit ``pngprism 0.5.0`` — the prior
# pinned version drift is resolved (see lib/prism-quant/docs/cli-contract.md).
__version__ = VERSION

DEFAULT_COLORS = 256
MAX_COLORS = 256
# T-0190 implements the round-5 pre-committed-rule selection: omission uses
# guarded adaptive-unit dithering. The two historical explicit policies keep
# their frozen meanings: ``off`` is the legacy no-dither path and ``on`` is
# unguarded adaptive-unit. The new ``guarded`` policy disables adaptive dither
# only when E-0032's classifier feature ``opaque_frac`` is 0.0000: the
# fully-opaque source-pixel fraction rounded to four decimals (Option A).
ADAPTIVE_DEFAULT_POLICIES = ("off", "on", "guarded")
DEFAULT_ADAPTIVE_DEFAULT = "guarded"
DEFAULT_DITHER = False
DEFAULT_DITHER_STRENGTH = (1, 1)
DEFAULT_DITHER_POLICY = "uniform"
DITHER_POLICIES = (
    "uniform",
    "adaptive",
    "region",
    "adaptive-unit",
    "luma-bluenoise",
)
DEFAULT_PACK_MODE = "none"
PACK_MODES = ("none", "fast", "max")
DEFAULT_PACK_SEARCH = "v1"
PACK_SEARCHES = ("v1", "v2")
DEFAULT_COLOR_SPACE = "srgb"
COLOR_SPACES = ("srgb", "oklab")

# --- E-0036/T-0192 byte-only pack-seam extensions ---------------------------
# E-0040 adopts ARM-S and ARM-R by omission on pack=none; ARM-M remains off.
# Explicit booleans preserve the E-0036 surface byte-for-byte. ``None`` means
# omitted so fast/max can retain the documented gating: omitted seams resolve
# off there, while an explicit on remains the frozen usage error. Each enabled
# seam trials quality-invariant, byte-only techniques and keeps the SMALLEST
# stream that re-decodes pixel-identical to the baseline. See E-0036 and E-0040.
#   ARM-S palette-sort trials:  --pack-seam-palette-sort off|on
#   ARM-M memLevel race:        --pack-seam-memlevel     off|on
#   ARM-R reduction-ladder:     --pack-seam-reduction    off|on
DEFAULT_PACK_SEAM_PALETTE_SORT = True
DEFAULT_PACK_SEAM_MEMLEVEL = False
DEFAULT_PACK_SEAM_REDUCTION = True

# --- E-0037/T-0189 fewest-colors-at-quality search --------------------------
# Opt-in ``--colors-search MIN..MAX@QUALITY``: pick the SMALLEST palette size
# in [MIN, MAX] whose internal premultiplied-RGBA MSE meets the frozen
# quality-to-MSE ceiling for QUALITY, else fall back to the cap MAX
# (never-worse). FROZEN by E-0037 (experiments/E-0037-fewest-colors/
# PROPOSAL.md). When the flag is omitted the whole pipeline is byte-identical
# to the pre-E-0037 path. Curve shape from
# lab/design-notes/dither-pack-and-qa-techniques.md §6.
COLORS_SEARCH_CURVE_W = 0.45
COLORS_SEARCH_CURVE_LOW_Q_FUDGE = 0.0008
COLORS_SEARCH_MIN_QUALITY = 0.0
COLORS_SEARCH_MAX_QUALITY = 100.0

# --- v0.1 core declared constants (T-0068) ---------------------------------

# Exact-color histogram while distinct colors stay at or below this limit;
# above it, a fine preclip (16 levels/channel, alpha endpoint-isolated)
# bounds the working set. Bin representatives are ALWAYS occupancy-weighted
# means of actual member pixels, never bin centers.
EXACT_BIN_LIMIT = 32768
PRECLIP_LEVELS = 16

# Refinement works on a deterministic stride sample of the sorted bins when
# there are more than this many (declared); final remap covers ALL bins.
REFINE_SAMPLE_CAP = 4096

# Sparse factorized init budgets (ch19 A2): the RGB rep count is
# ceil(colors / zoned alpha levels), floored at RGB_REP_MAX, so pairs can
# fill the palette when the ladder is short.
RGB_REP_MAX = 32
ALPHA_LADDER_INTERIOR_MAX = 8
RGB_FIT_ITERS = 4
ALPHA_LADDER_MAX_ITERS = 32

# Joint refinement convergence bound (book ch11: declared "maximum
# iteration count"; the fixed-point stop usually fires first).
REFINE_MAX_ITERS = 8

# Hidden-RGB policy hook values (ch04 / E-0001 vocabulary). E-0001 bounds the
# evidence for the retained byte-priority default; see the module header.
HIDDEN_RGB_POLICIES = ("canonicalize-black", "preserve-mean")
DEFAULT_HIDDEN_RGB_POLICY = "canonicalize-black"

_ZONE_TRANSPARENT = 0
_ZONE_INTERIOR = 1
_ZONE_OPAQUE = 2


class PrismQuantError(Exception):
    """A clean CLI data/usage failure (never a traceback)."""


@dataclass(frozen=True)
class StageNotes:
    """The declared per-stage observations of one pipeline execution.

    The first four fields are the T-0067 harness-consumed surface; the
    v0.1 core adds observability fields the harness ignores.
    """

    sampled_pixels: int
    initial_bins: int
    refined_palette_entries: int
    alpha_note: str
    exact_path: bool = False
    palette_init_pairs: int = 0
    refinement_iterations: int = 0
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY


@dataclass(frozen=True)
class _Bin:
    """One histogram bin: its key plus actual member-pixel sums (never
    grid centers). The key is the exact color (exact histogram) or the
    preclip tuple (bounded fallback). Zone is shared by ALL member pixels
    by construction (endpoint-isolated alpha binning)."""

    key: tuple[int, int, int, int]
    count: int
    sum_r: int
    sum_g: int
    sum_b: int
    sum_a: int
    sum_ar: int
    sum_ag: int
    sum_ab: int
    zone: int


@dataclass(frozen=True)
class PaletteInit:
    """Palette-init seam output: histogram bins, the instantiated sparse
    factorized pairs, the alpha ladder, and histogram kind flags."""

    bins: list[_Bin]
    palette: list[tuple[int, int, int, int]]
    ladder: list[int]
    exact: bool  # True when the histogram used exact colors (no preclip)
    exact_path: bool  # True when distinct colors <= cap (pixel-exact)


def _round_half_up(numerator: int, denominator: int) -> int:
    """Exact floor(numerator/denominator + 1/2) on nonnegative ints."""
    return (2 * numerator + denominator) // (2 * denominator)


def _zone_of(alpha: int) -> int:
    if alpha == 0:
        return _ZONE_TRANSPARENT
    if alpha == 255:
        return _ZONE_OPAQUE
    return _ZONE_INTERIOR


def _alpha_bin(alpha: int, levels: int = PRECLIP_LEVELS) -> int:
    """Endpoint-isolating alpha bin: a==0 -> 0, a==255 -> levels-1, and
    interior values spread over 1..levels-2. Bins are zone-homogeneous,
    which makes zone constraints per-pixel exact (T-0068)."""
    if alpha == 0:
        return 0
    if alpha == 255:
        return levels - 1
    return 1 + (alpha - 1) * (levels - 2) // 254


def _pack_rgba(value: tuple[int, int, int, int]) -> int:
    return (value[0] << 24) | (value[1] << 16) | (value[2] << 8) | value[3]


def premultiplied_distance_sq(
    p: tuple[int, int, int, int], q: tuple[int, int, int, int]
) -> int:
    """Declared alpha-aware distance: squared Euclidean over premultiplied
    RGBA on the 65025 scale (exact numerator of the M1 harness metric
    premultiplied_linear_rgba_mse). Hidden RGB collapses at alpha zero."""
    dr = p[3] * p[0] - q[3] * q[0]
    dg = p[3] * p[1] - q[3] * q[1]
    db = p[3] * p[2] - q[3] * q[2]
    da = 255 * (p[3] - q[3])
    return dr * dr + dg * dg + db * db + da * da


# --- E-0016 opt-in Oklab assignment/refinement/remap -----------------------


def _srgb8_to_linear(value: int) -> float:
    encoded = value / 255.0
    if encoded <= 0.04045:
        return encoded / 12.92
    return ((encoded + 0.055) / 1.055) ** 2.4


def _linear_to_srgb8(value: float) -> int:
    value = min(1.0, max(0.0, value))
    if value <= 0.0031308:
        encoded = 12.92 * value
    else:
        encoded = 1.055 * (value ** (1.0 / 2.4)) - 0.055
    return min(255, max(0, math.floor(encoded * 255.0 + 0.5)))


def _cbrt(value: float) -> float:
    if value == 0.0:
        return 0.0
    return math.copysign(abs(value) ** (1.0 / 3.0), value)


def srgb8_to_oklab(rgb: tuple[int, int, int]) -> tuple[float, float, float]:
    """Published 2021 linear-sRGB to Oklab transform (Ottosson)."""
    red = _srgb8_to_linear(rgb[0])
    green = _srgb8_to_linear(rgb[1])
    blue = _srgb8_to_linear(rgb[2])
    light = 0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue
    medium = 0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue
    short = 0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue
    light_root = _cbrt(light)
    medium_root = _cbrt(medium)
    short_root = _cbrt(short)
    return (
        0.2104542553 * light_root
        + 0.7936177850 * medium_root
        - 0.0040720468 * short_root,
        1.9779984951 * light_root
        - 2.4285922050 * medium_root
        + 0.4505937099 * short_root,
        0.0259040371 * light_root
        + 0.7827717662 * medium_root
        - 0.8086757660 * short_root,
    )


def oklab_to_srgb8(lab: tuple[float, float, float]) -> tuple[int, int, int]:
    """Published Oklab inverse, followed by the standard sRGB transfer."""
    lightness, axis_a, axis_b = lab
    light_root = lightness + 0.3963377774 * axis_a + 0.2158037573 * axis_b
    medium_root = lightness - 0.1055613458 * axis_a - 0.0638541728 * axis_b
    short_root = lightness - 0.0894841775 * axis_a - 1.2914855480 * axis_b
    light = light_root * light_root * light_root
    medium = medium_root * medium_root * medium_root
    short = short_root * short_root * short_root
    return (
        _linear_to_srgb8(
            +4.0767416621 * light - 3.3077115913 * medium + 0.2309699292 * short
        ),
        _linear_to_srgb8(
            -1.2684380046 * light + 2.6097574011 * medium - 0.3413193965 * short
        ),
        _linear_to_srgb8(
            -0.0041960863 * light - 0.7034186147 * medium + 1.7076147010 * short
        ),
    )


def premultiplied_oklab_feature(
    pixel: tuple[int, int, int, int],
) -> tuple[float, float, float, float]:
    """E-0016's alpha-aware Oklab feature: (A*L, A*a, A*b, A)."""
    alpha = pixel[3] / 255.0
    if alpha == 0.0:
        return (0.0, 0.0, 0.0, 0.0)
    lightness, axis_a, axis_b = srgb8_to_oklab(pixel[:3])
    return (alpha * lightness, alpha * axis_a, alpha * axis_b, alpha)


def _pixel_bin_key(
    pixel: tuple[int, int, int, int], exact: bool
) -> tuple[int, int, int, int]:
    if exact:
        return pixel
    levels = PRECLIP_LEVELS
    red, green, blue, alpha = pixel
    return (
        red * levels // 256,
        green * levels // 256,
        blue * levels // 256,
        _alpha_bin(alpha, levels),
    )


@dataclass(frozen=True)
class _OklabFeatureBin:
    count: int
    sums: tuple[float, float, float, float]

    @property
    def mean(self) -> tuple[float, float, float, float]:
        return tuple(value / self.count for value in self.sums)  # type: ignore[return-value]


def _oklab_feature_bins(
    pixels: Sequence[tuple[int, int, int, int]], init: PaletteInit
) -> dict[tuple[int, int, int, int], _OklabFeatureBin]:
    accumulators: dict[tuple[int, int, int, int], list[float]] = {}
    for pixel in pixels:
        key = _pixel_bin_key(pixel, init.exact)
        accumulator = accumulators.setdefault(key, [0.0, 0.0, 0.0, 0.0, 0.0])
        feature = premultiplied_oklab_feature(pixel)
        accumulator[0] += 1.0
        for index in range(4):
            accumulator[index + 1] += feature[index]
    result = {
        key: _OklabFeatureBin(int(values[0]), tuple(values[1:]))
        for key, values in accumulators.items()
    }
    if set(result) != {item.key for item in init.bins}:
        raise PrismQuantError("internal: Oklab bins differ from initializer")
    for item in init.bins:
        if result[item.key].count != item.count:
            raise PrismQuantError("internal: Oklab bin count mismatch")
    return result


def _oklab_distance_sq(
    left: tuple[float, float, float, float],
    right: tuple[float, float, float, float],
) -> float:
    return sum((left[index] - right[index]) ** 2 for index in range(4))


def _nearest_oklab_entry(
    feature: tuple[float, float, float, float],
    zone: int,
    entries: list[tuple[float, float, float, float]],
    entry_zones: list[int],
) -> int:
    best = -1
    best_distance: float | None = None
    for index, entry in enumerate(entries):
        if entry_zones[index] != zone:
            continue
        distance = _oklab_distance_sq(feature, entry)
        if best_distance is None or distance < best_distance:
            best = index
            best_distance = distance
    if best >= 0:
        return best
    if zone == _ZONE_TRANSPARENT:
        raise PrismQuantError("internal: transparent bin without transparent entry")
    fallback = -1
    fallback_distance: float | None = None
    for index, entry in enumerate(entries):
        if entry_zones[index] == _ZONE_TRANSPARENT and len(entries) > 1:
            continue
        distance = _oklab_distance_sq(feature, entry)
        if fallback_distance is None or distance < fallback_distance:
            fallback = index
            fallback_distance = distance
    if fallback < 0:
        raise PrismQuantError("internal: empty palette")
    return fallback


def _oklab_centroid_from_feature_sums(
    count: int,
    sum_alpha_u8: int,
    sum_alpha_lightness: float,
    sum_alpha_axis_a: float,
    sum_alpha_axis_b: float,
) -> tuple[int, int, int, int]:
    alpha = _round_half_up(sum_alpha_u8, count)
    if alpha == 0 or sum_alpha_u8 == 0:
        return (0, 0, 0, 0)
    sum_alpha_normalized = sum_alpha_u8 / 255.0
    red, green, blue = oklab_to_srgb8(
        (
            sum_alpha_lightness / sum_alpha_normalized,
            sum_alpha_axis_a / sum_alpha_normalized,
            sum_alpha_axis_b / sum_alpha_normalized,
        )
    )
    return (red, green, blue, alpha)


def _oklab_single_bin_centroid(
    item: _Bin, feature: _OklabFeatureBin
) -> tuple[int, int, int, int]:
    candidate = _oklab_centroid_from_feature_sums(
        item.count,
        item.sum_a,
        feature.sums[0],
        feature.sums[1],
        feature.sums[2],
    )
    if item.zone == _ZONE_OPAQUE:
        candidate = (candidate[0], candidate[1], candidate[2], 255)
    return candidate


def _refine_oklab(
    init: PaletteInit,
    feature_by_key: dict[tuple[int, int, int, int], _OklabFeatureBin],
) -> tuple[list[tuple[int, int, int, int]], int]:
    palette = list(init.palette)
    if not palette or not init.bins or init.exact_path:
        return palette, 0
    sample = _refine_sample(init.bins)
    sample_features = [feature_by_key[item.key].mean for item in sample]
    entry_zones = [_zone_of(entry[3]) for entry in palette]
    iterations = 0
    for iteration in range(1, REFINE_MAX_ITERS + 1):
        iterations = iteration
        entry_features = [premultiplied_oklab_feature(entry) for entry in palette]
        assignments = [
            _nearest_oklab_entry(
                sample_features[index], item.zone, entry_features, entry_zones
            )
            for index, item in enumerate(sample)
        ]
        # count, sum-alpha-u8, sum(A*L), sum(A*a), sum(A*b)
        accumulators: list[list[float]] = [[0.0] * 5 for _ in palette]
        for index, item in enumerate(sample):
            target = accumulators[assignments[index]]
            feature = feature_by_key[item.key]
            target[0] += item.count
            target[1] += item.sum_a
            target[2] += feature.sums[0]
            target[3] += feature.sums[1]
            target[4] += feature.sums[2]

        worst: dict[int, tuple[float, tuple[int, int, int, int]]] = {}
        for index, item in enumerate(sample):
            distance = _oklab_distance_sq(
                sample_features[index], entry_features[assignments[index]]
            )
            candidate = _oklab_single_bin_centroid(
                item, feature_by_key[item.key]
            )
            current = worst.get(item.zone)
            if current is None or distance > current[0] or (
                distance == current[0]
                and _pack_rgba(candidate) < _pack_rgba(current[1])
            ):
                worst[item.zone] = (distance, candidate)

        new_palette: list[tuple[int, int, int, int]] = []
        new_zones: list[int] = []
        moved = False
        zone_counts: dict[int, int] = {}
        for zone in entry_zones:
            zone_counts[zone] = zone_counts.get(zone, 0) + 1
        for index, entry in enumerate(palette):
            zone = entry_zones[index]
            sums = accumulators[index]
            if zone == _ZONE_TRANSPARENT:
                new_palette.append(entry)
                new_zones.append(zone)
                continue
            if int(sums[0]) == 0:
                candidate = worst.get(zone)
                if (
                    candidate is None or candidate[0] == 0.0
                ) and zone_counts[zone] > 1:
                    zone_counts[zone] -= 1
                    moved = True
                    continue
                if candidate is None or candidate[0] == 0.0:
                    new_palette.append(entry)
                    new_zones.append(zone)
                    continue
                new_palette.append(candidate[1])
                new_zones.append(zone)
                moved = True
                continue
            updated = _oklab_centroid_from_feature_sums(
                int(sums[0]), int(sums[1]), sums[2], sums[3], sums[4]
            )
            if zone == _ZONE_OPAQUE:
                updated = (updated[0], updated[1], updated[2], 255)
            new_palette.append(updated)
            new_zones.append(zone)
            if updated != entry:
                moved = True
        palette = new_palette
        entry_zones = new_zones
        if not moved:
            break
    return palette, iterations


def _remap_oklab(
    pixels: Sequence[tuple[int, int, int, int]],
    init: PaletteInit,
    palette: list[tuple[int, int, int, int]],
    feature_by_key: dict[tuple[int, int, int, int], _OklabFeatureBin],
) -> list[int]:
    if not palette:
        return []
    entry_features = [premultiplied_oklab_feature(entry) for entry in palette]
    entry_zones = [_zone_of(entry[3]) for entry in palette]
    assignment = {
        item.key: _nearest_oklab_entry(
            feature_by_key[item.key].mean, item.zone, entry_features, entry_zones
        )
        for item in init.bins
    }
    return [assignment[_pixel_bin_key(pixel, init.exact)] for pixel in pixels]


def _build_bins(pixels: Sequence[tuple[int, int, int, int]]) -> tuple[list[_Bin], bool]:
    """Histogram pass. Exact distinct colors while they fit
    EXACT_BIN_LIMIT, else the declared fine preclip; every bin carries
    actual member sums so representatives are occupancy-weighted means.
    Returns (bins sorted by key, exact flag)."""
    exact_table: dict[tuple[int, int, int, int], list[int]] = {}
    preclip_table: dict[tuple[int, int, int, int], list[int]] = {}
    preclip_mode = False
    levels = PRECLIP_LEVELS
    for r, g, b, a in pixels:
        if not preclip_mode:
            key = (r, g, b, a)
            sums = exact_table.get(key)
            if sums is not None:
                sums[0] += 1
                sums[1] += r
                sums[2] += g
                sums[3] += b
                sums[4] += a
                sums[5] += a * r
                sums[6] += a * g
                sums[7] += a * b
                continue
            if len(exact_table) < EXACT_BIN_LIMIT:
                exact_table[key] = [1, r, g, b, a, a * r, a * g, a * b]
                continue
            # Overflow: convert the exact table to the bounded preclip.
            preclip_mode = True
            for (er, eg, eb, ea), es in exact_table.items():
                pkey = (
                    er * levels // 256,
                    eg * levels // 256,
                    eb * levels // 256,
                    _alpha_bin(ea, levels),
                )
                acc = preclip_table.setdefault(pkey, [0] * 8)
                for i in range(8):
                    acc[i] += es[i]
            exact_table = {}
        pkey = (
            r * levels // 256,
            g * levels // 256,
            b * levels // 256,
            _alpha_bin(a, levels),
        )
        acc = preclip_table.get(pkey)
        if acc is None:
            preclip_table[pkey] = [1, r, g, b, a, a * r, a * g, a * b]
        else:
            acc[0] += 1
            acc[1] += r
            acc[2] += g
            acc[3] += b
            acc[4] += a
            acc[5] += a * r
            acc[6] += a * g
            acc[7] += a * b
    table = preclip_table if preclip_mode else exact_table
    bins: list[_Bin] = []
    for key in sorted(table):
        s = table[key]
        mean_a = _round_half_up(s[4], s[0])
        bins.append(
            _Bin(
                key=key,
                count=s[0],
                sum_r=s[1],
                sum_g=s[2],
                sum_b=s[3],
                sum_a=s[4],
                sum_ar=s[5],
                sum_ag=s[6],
                sum_ab=s[7],
                zone=_zone_of(mean_a),
            )
        )
    return bins, not preclip_mode


def _bin_mean_color(b: _Bin) -> tuple[int, int, int, int]:
    """The bin's occupancy-weighted representative (mean of members)."""
    return (
        _round_half_up(b.sum_r, b.count),
        _round_half_up(b.sum_g, b.count),
        _round_half_up(b.sum_b, b.count),
        _round_half_up(b.sum_a, b.count),
    )


def _bin_premult_mean(b: _Bin) -> tuple[int, int, int, int]:
    """The bin's rounded premultiplied mean (assignment distance input)."""
    return (
        _round_half_up(b.sum_ar, b.count),
        _round_half_up(b.sum_ag, b.count),
        _round_half_up(b.sum_ab, b.count),
        _round_half_up(255 * b.sum_a, b.count),
    )


def _centroid(
    count: int, sum_a: int, sum_ar: int, sum_ag: int, sum_ab: int
) -> tuple[int, int, int, int]:
    """Occupancy-weighted palette value from member sums: alpha is the
    count-weighted mean; RGB un-premultiplies the premultiplied mean by it
    (sum_ar/sum_a <= 255 always, since per-pixel a*r <= 255*a). Exact
    integer rounding half-up. a*==0 is the caller's policy case."""
    a_star = _round_half_up(sum_a, count)
    if a_star == 0:
        return (0, 0, 0, 0)
    return (
        _round_half_up(sum_ar, sum_a),
        _round_half_up(sum_ag, sum_a),
        _round_half_up(sum_ab, sum_a),
        a_star,
    )


def _alpha_ladder(bins: list[_Bin]) -> list[int]:
    """ch19 A1-lite ladder: mandatory exact locks {0, 255} where present,
    plus interior levels from weighted 1-D Lloyd over the 256 alpha
    buckets, seeded at weighted quantiles (declared bound
    ALPHA_LADDER_MAX_ITERS; 1-D Lloyd converges well before it)."""
    mass = [0] * 256
    for b in bins:
        mass[_round_half_up(b.sum_a, b.count)] += b.count
    ladder: list[int] = []
    if mass[0]:
        ladder.append(0)
    if mass[255]:
        ladder.append(255)
    interior = [a for a in range(1, 255) if mass[a]]
    if interior:
        k = min(ALPHA_LADDER_INTERIOR_MAX, len(interior))
        total = sum(mass[a] for a in interior)
        # Weighted-quantile seeds: the alpha values splitting the interior
        # mass into k equal-weight bands (deterministic).
        seeds: list[int] = []
        cumulative = 0
        target = 1
        for a in interior:
            cumulative += mass[a]
            while target <= k and cumulative * 2 * k >= (2 * target - 1) * total:
                if not seeds or seeds[-1] != a:
                    seeds.append(a)
                target += 1
        levels = seeds
        for _ in range(ALPHA_LADDER_MAX_ITERS):
            groups: list[list[int]] = [[] for _ in levels]
            for a in interior:
                best = 0
                best_d = abs(a - levels[0])
                for j in range(1, len(levels)):
                    d = abs(a - levels[j])
                    if d < best_d:  # ties keep the lower level
                        best = j
                        best_d = d
                groups[best].append(a)
            updated: list[int] = []
            for j, group in enumerate(groups):
                if not group:
                    updated.append(levels[j])
                    continue
                numerator = sum(a * mass[a] for a in group)
                denominator = sum(mass[a] for a in group)
                updated.append(_round_half_up(numerator, denominator))
            updated = sorted(set(updated))
            if updated == levels:
                break
            levels = updated
        ladder.extend(levels)
    return sorted(set(ladder))


def _refine_sample(bins: list[_Bin]) -> list[_Bin]:
    """Deterministic stride sample of the sorted bins (declared
    REFINE_SAMPLE_CAP); final remap always covers ALL bins."""
    if len(bins) <= REFINE_SAMPLE_CAP:
        return bins
    stride = -(-len(bins) // REFINE_SAMPLE_CAP)  # ceil division
    return bins[::stride]


def _fit_rgb_reps(
    sample: list[_Bin], cap: int, zoned_levels: int
) -> list[tuple[int, int, int]]:
    """Alpha-mass-weighted RGB representatives over the refinement sample:
    deterministic farthest-point seeding (Gonzalez 1985; deterministic
    k-means++ family, book ch11 §8) plus weighted Lloyd polish (ch11 §2).
    Weight is the bin's total alpha mass sum_a, so fully-transparent
    pixels never claim palette capacity for hidden RGB (ch07/ch04).

    ``zoned_levels`` is the number of non-transparent alpha ladder levels;
    the rep budget is ceil(cap / zoned_levels) (floor RGB_REP_MAX) so the
    factorized pairs can actually fill the palette when the ladder is
    short (an opaque source has ONE level and needs ~cap reps)."""
    items: list[tuple[tuple[int, int, int], int]] = []  # (mean rgb, weight)
    for b in sample:
        if b.zone == _ZONE_TRANSPARENT or b.sum_a == 0:
            continue
        mean = _bin_mean_color(b)
        items.append(((mean[0], mean[1], mean[2]), b.sum_a))
    if not items:
        return [(0, 0, 0)]
    budget = max(RGB_REP_MAX, -(-cap // max(1, zoned_levels)))
    k = min(budget, cap, len(items))
    packed = [(v[0] << 16) | (v[1] << 8) | v[2] for v, _ in items]
    # Seed 0: maximum alpha mass (ties -> lowest packed RGB).
    first = min(range(len(items)), key=lambda i: (-items[i][1], packed[i]))
    seeds = [items[first][0]]
    # Incremental farthest-point: cur_d2[i] = squared distance from item i
    # to its nearest seed so far.
    cur_d2 = [None] * len(items)
    for i, (value, _) in enumerate(items):
        s = seeds[0]
        cur_d2[i] = (value[0] - s[0]) ** 2 + (value[1] - s[1]) ** 2 + (value[2] - s[2]) ** 2
    while len(seeds) < k:
        best = min(
            range(len(items)),
            key=lambda i: (-(items[i][1] * cur_d2[i]), packed[i]),
        )
        if items[best][1] * cur_d2[best] == 0:
            break  # every weighted distinct color is already seeded
        s = items[best][0]
        seeds.append(s)
        for i, (value, _) in enumerate(items):
            d2 = (value[0] - s[0]) ** 2 + (value[1] - s[1]) ** 2 + (value[2] - s[2]) ** 2
            if d2 < cur_d2[i]:
                cur_d2[i] = d2
    reps = seeds
    # Weighted Lloyd polish (declared RGB_FIT_ITERS bound).
    for _ in range(RGB_FIT_ITERS):
        acc = [[0, 0, 0, 0] for _ in reps]  # weight, wr, wg, wb
        for (value, weight) in items:
            best = 0
            s = reps[0]
            best_d = (value[0] - s[0]) ** 2 + (value[1] - s[1]) ** 2 + (value[2] - s[2]) ** 2
            for j in range(1, len(reps)):
                s = reps[j]
                d2 = (value[0] - s[0]) ** 2 + (value[1] - s[1]) ** 2 + (value[2] - s[2]) ** 2
                if d2 < best_d:
                    best = j
                    best_d = d2
            acc[best][0] += weight
            acc[best][1] += weight * value[0]
            acc[best][2] += weight * value[1]
            acc[best][3] += weight * value[2]
        moved = False
        new_reps = []
        for j, (weight, wr, wg, wb) in enumerate(acc):
            if weight == 0:
                new_reps.append(reps[j])
                continue
            updated = (
                _round_half_up(wr, weight),
                _round_half_up(wg, weight),
                _round_half_up(wb, weight),
            )
            if updated != reps[j]:
                moved = True
            new_reps.append(updated)
        reps = new_reps
        if not moved:
            break
    return reps


def _fill_palette_by_weighted_residual(
    bins: list[_Bin],
    palette: list[tuple[int, int, int, int]],
    colors: int,
) -> list[tuple[int, int, int, int]]:
    """Fill unused capacity with deterministic, zone-safe residual seeds.

    The sparse factorized initializer can instantiate fewer observed pairs
    than the requested cap.  Preserve those seeds, then repeatedly add the
    occupancy-weighted mean of the bin with maximum ``count * nearest_d2``
    inside its alpha zone.  Nearest distances are updated incrementally after
    each append, which is equivalent to recomputing them from the expanded
    palette.  Transparent mass remains represented by its single policy-locked
    entry; duplicate RGBA seeds are never appended.
    """
    result = list(palette)
    if len(result) >= colors:
        return result
    sample = _refine_sample(bins)
    means = [_bin_mean_color(b) for b in sample]
    sample_premult = [_bin_premult_mean(b) for b in sample]
    palette_values = set(result)
    entries_premult = [_entry_premult(entry) for entry in result]
    entry_zones = [_zone_of(entry[3]) for entry in result]

    nearest: list[int | None] = []
    for b, point in zip(sample, sample_premult):
        if b.zone == _ZONE_TRANSPARENT:
            nearest.append(0)
            continue
        best: int | None = None
        for entry, zone in zip(entries_premult, entry_zones):
            if zone != b.zone:
                continue
            dr = point[0] - entry[0]
            dg = point[1] - entry[1]
            db = point[2] - entry[2]
            da = point[3] - entry[3]
            d2 = dr * dr + dg * dg + db * db + da * da
            if best is None or d2 < best:
                best = d2
        nearest.append(best)

    while len(result) < colors:
        eligible = [
            i
            for i, b in enumerate(sample)
            if b.zone != _ZONE_TRANSPARENT
            and means[i] not in palette_values
            and nearest[i] is not None
            and nearest[i] > 0
        ]
        if not eligible:
            break
        selected = min(
            eligible,
            key=lambda i: (
                -(sample[i].count * int(nearest[i])),
                _pack_rgba(means[i]),
                sample[i].zone,
            ),
        )
        seed = means[selected]
        seed_zone = sample[selected].zone
        seed_premult = _entry_premult(seed)
        result.append(seed)
        palette_values.add(seed)
        for i, (b, point) in enumerate(zip(sample, sample_premult)):
            if b.zone != seed_zone:
                continue
            dr = point[0] - seed_premult[0]
            dg = point[1] - seed_premult[1]
            db = point[2] - seed_premult[2]
            da = point[3] - seed_premult[3]
            d2 = dr * dr + dg * dg + db * db + da * da
            if nearest[i] is None or d2 < nearest[i]:
                nearest[i] = d2
    return result


def stage_sample(pixels: Sequence[tuple[int, int, int, int]]) -> list[tuple[int, int, int, int]]:
    """v0.1 sampling seam: identity — every pixel participates."""
    return list(pixels)


def stage_palette_init(
    pixels: Sequence[tuple[int, int, int, int]],
    colors: int,
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY,
) -> PaletteInit:
    """v0.1 palette-initialization seam: sparse factorized RGB/alpha init
    (ch19 §5 contract A2): alpha ladder with exact locks + alpha-mass-
    weighted RGB reps + observed-pair instantiation, mass-ranked and
    capped. All values occupancy-weighted (never grid centers)."""
    if hidden_rgb_policy not in HIDDEN_RGB_POLICIES:
        raise PrismQuantError(f"unknown hidden-rgb-policy: {hidden_rgb_policy}")
    bins, exact = _build_bins(pixels)
    if not bins:
        return PaletteInit(bins=[], palette=[], ladder=[], exact=True, exact_path=True)

    # Exact path (BINDING): distinct colors <= cap -> the palette IS the
    # distinct color set; pixel-exact by construction.
    if exact and len(bins) <= colors:
        palette = [_bin_mean_color(b) for b in bins]
        return PaletteInit(
            bins=bins,
            palette=palette,
            ladder=[],
            exact=exact,
            exact_path=True,
        )

    ladder = _alpha_ladder(bins)
    zoned_levels = sum(1 for level in ladder if _zone_of(level) != _ZONE_TRANSPARENT)
    reps = _fit_rgb_reps(_refine_sample(bins), colors, max(1, zoned_levels))

    # Observed co-occurrence (A2 step 3): map each bin to its nearest RGB
    # rep (straight-RGB Euclidean, ties -> lowest index) and nearest
    # ladder level inside its zone; accumulate member sums per pair.
    pair_acc: dict[tuple[int, int], list[int]] = {}
    pair_mass: dict[tuple[int, int], int] = {}
    for b in bins:
        mean = _bin_mean_color(b)
        if b.zone == _ZONE_TRANSPARENT:
            slot = (-1, 0)  # the single policy-locked a==0 entry
        else:
            best_rep = 0
            s = reps[0]
            best_d = (mean[0] - s[0]) ** 2 + (mean[1] - s[1]) ** 2 + (mean[2] - s[2]) ** 2
            for j in range(1, len(reps)):
                s = reps[j]
                d2 = (mean[0] - s[0]) ** 2 + (mean[1] - s[1]) ** 2 + (mean[2] - s[2]) ** 2
                if d2 < best_d:
                    best_rep = j
                    best_d = d2
            level = None
            for candidate in ladder:
                if _zone_of(candidate) != b.zone:
                    continue
                if level is None or abs(candidate - mean[3]) < abs(level - mean[3]):
                    level = candidate
            slot = (best_rep, level if level is not None else mean[3])
        acc = pair_acc.get(slot)
        if acc is None:
            pair_acc[slot] = acc = [0, 0, 0, 0, 0]
            pair_mass[slot] = 0
        acc[0] += b.count
        acc[1] += b.sum_a
        acc[2] += b.sum_ar
        acc[3] += b.sum_ag
        acc[4] += b.sum_ab
        pair_mass[slot] += b.count

    # Instantiate at most the joint cap (A2 step 5): the a==0 entry is
    # always instantiated when transparent mass exists; then each PRESENT
    # zone reserves its heaviest pair (BINDING: a zone with mass must keep
    # at least one entry so remap never leaves the zone); the remaining
    # pairs rank by mass (ties -> lowest packed initial value). Pairs left
    # uninstantiated are repaired at remap (nearest entry, A2 step 6).
    palette: list[tuple[int, int, int, int]] = []
    instantiated: set[tuple[int, int]] = set()
    transparent_slot = (-1, 0)
    if transparent_slot in pair_acc:
        instantiated.add(transparent_slot)
        if hidden_rgb_policy == "preserve-mean":
            sums = [0, 0, 0]
            total = 0
            for b in bins:
                if b.zone == _ZONE_TRANSPARENT:
                    sums[0] += b.sum_r
                    sums[1] += b.sum_g
                    sums[2] += b.sum_b
                    total += b.count
            palette.append(
                (
                    _round_half_up(sums[0], total),
                    _round_half_up(sums[1], total),
                    _round_half_up(sums[2], total),
                    0,
                )
            )
        else:
            palette.append((0, 0, 0, 0))
    present_zones = sorted({b.zone for b in bins})
    for zone in present_zones:
        if zone == _ZONE_TRANSPARENT or len(palette) >= colors:
            continue
        zoned = [
            slot
            for slot in pair_acc
            if slot not in instantiated and _zone_of(slot[1]) == zone
        ]
        if not zoned:
            continue
        heaviest = min(
            zoned, key=lambda slot: (-pair_mass[slot], _pack_rgba(_centroid(*pair_acc[slot])))
        )
        instantiated.add(heaviest)
        palette.append(_centroid(*pair_acc[heaviest]))
    ranked = sorted(
        (slot for slot in pair_acc if slot not in instantiated),
        key=lambda slot: (-pair_mass[slot], _pack_rgba(_centroid(*pair_acc[slot]))),
    )
    for slot in ranked:
        if len(palette) >= colors:
            break
        palette.append(_centroid(*pair_acc[slot]))
    palette = _fill_palette_by_weighted_residual(bins, palette, colors)
    return PaletteInit(
        bins=bins,
        palette=palette,
        ladder=ladder,
        exact=exact,
        exact_path=False,
    )


def _entry_premult(entry: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    return (
        entry[3] * entry[0],
        entry[3] * entry[1],
        entry[3] * entry[2],
        255 * entry[3],
    )


def _nearest_entry(
    premult: tuple[int, int, int, int],
    zone: int,
    entries_premult: list[tuple[int, int, int, int]],
    entry_zones: list[int],
) -> int:
    """Nearest palette index in premultiplied space, restricted to the
    bin's alpha zone (BINDING); ties -> lowest index. One-directional
    capacity degradation: a zone with no entry falls back to the nearest
    non-transparent entry (the a==0 entry only when it is the ONLY entry),
    so a fully-transparent pixel can never gain visibility."""
    best = -1
    best_d: int | None = None
    for j, ep in enumerate(entries_premult):
        if entry_zones[j] != zone:
            continue
        dr = premult[0] - ep[0]
        dg = premult[1] - ep[1]
        db = premult[2] - ep[2]
        da = premult[3] - ep[3]
        d2 = dr * dr + dg * dg + db * db + da * da
        if best_d is None or d2 < best_d:
            best = j
            best_d = d2
    if best >= 0:
        return best
    if zone == _ZONE_TRANSPARENT:
        raise PrismQuantError("internal: transparent bin without a transparent entry")
    fallback: int | None = None
    fallback_d: int | None = None
    for j, ep in enumerate(entries_premult):
        if entry_zones[j] == _ZONE_TRANSPARENT and len(entries_premult) > 1:
            continue
        dr = premult[0] - ep[0]
        dg = premult[1] - ep[1]
        db = premult[2] - ep[2]
        da = premult[3] - ep[3]
        d2 = dr * dr + dg * dg + db * db + da * da
        if fallback_d is None or d2 < fallback_d:
            fallback = j
            fallback_d = d2
    if fallback is None:
        raise PrismQuantError("internal: empty palette")
    return fallback


def stage_refinement(
    init: PaletteInit,
    colors: int,
) -> tuple[list[tuple[int, int, int, int]], int]:
    """v0.1 refinement seam: k-means-style joint Lloyd in premultiplied
    space (book ch11 §1-2; ch19 A7 stop-rule family). Occupancy-weighted
    centroid updates; zone-constrained assignment; fixed-point stop or
    REFINE_MAX_ITERS; empty entries re-seed to the worst-served sample bin
    of their zone. Returns (palette, iterations_run)."""
    palette = list(init.palette)
    if not palette or not init.bins or init.exact_path:
        return palette, 0
    sample = _refine_sample(init.bins)
    sample_premult = [_bin_premult_mean(b) for b in sample]
    entry_zones = [_zone_of(entry[3]) for entry in palette]
    iterations = 0
    for iteration in range(1, REFINE_MAX_ITERS + 1):
        iterations = iteration
        entries_premult = [_entry_premult(entry) for entry in palette]
        assign = [
            _nearest_entry(sample_premult[i], b.zone, entries_premult, entry_zones)
            for i, b in enumerate(sample)
        ]
        acc = [[0, 0, 0, 0, 0] for _ in palette]
        for i, b in enumerate(sample):
            j = assign[i]
            acc[j][0] += b.count
            acc[j][1] += b.sum_a
            acc[j][2] += b.sum_ar
            acc[j][3] += b.sum_ag
            acc[j][4] += b.sum_ab
        # Worst-served sample bin per zone (for empty-entry re-seeding):
        # score = rounded squared distance to its assigned entry; ties ->
        # lower packed bin mean.
        worst: dict[int, tuple[int, tuple[int, int, int, int]]] = {}
        for i, b in enumerate(sample):
            j = assign[i]
            ep = entries_premult[j]
            sp = sample_premult[i]
            dr = sp[0] - ep[0]
            dg = sp[1] - ep[1]
            db = sp[2] - ep[2]
            da = sp[3] - ep[3]
            d2 = dr * dr + dg * dg + db * db + da * da
            current = worst.get(b.zone)
            mean = _bin_mean_color(b)
            if current is None or d2 > current[0] or (
                d2 == current[0] and _pack_rgba(mean) < _pack_rgba(current[1])
            ):
                worst[b.zone] = (d2, mean)
        new_palette: list[tuple[int, int, int, int]] = []
        new_zones: list[int] = []
        moved = False
        zone_counts: dict[int, int] = {}
        for zone in entry_zones:
            zone_counts[zone] = zone_counts.get(zone, 0) + 1
        for j, entry in enumerate(palette):
            zone = entry_zones[j]
            sums = acc[j]
            if zone == _ZONE_TRANSPARENT:
                new_palette.append(entry)  # policy-locked; never drifts
                new_zones.append(zone)
                continue
            if sums[0] == 0:
                candidate = worst.get(zone)
                # Never drop a zone's last entry (BINDING: remap must
                # always find an entry in every zone that still has bins).
                if (candidate is None or candidate[0] == 0) and zone_counts.get(zone, 0) > 1:
                    zone_counts[zone] -= 1
                    moved = True  # zone perfectly fit: drop the spare entry
                    continue
                if candidate is None or candidate[0] == 0:
                    new_palette.append(entry)
                    new_zones.append(zone)
                    continue
                new_palette.append(candidate[1])
                new_zones.append(zone)
                moved = True
                continue
            updated = _centroid(sums[0], sums[1], sums[2], sums[3], sums[4])
            if zone == _ZONE_OPAQUE:
                updated = (updated[0], updated[1], updated[2], 255)  # lock pin
            new_palette.append(updated)
            new_zones.append(zone)
            if updated != entry:
                moved = True
        palette = new_palette
        entry_zones = new_zones
        if not moved:
            break
    return palette, iterations


def stage_remap(
    pixels: Sequence[tuple[int, int, int, int]],
    init: PaletteInit,
    palette: list[tuple[int, int, int, int]],
) -> list[int]:
    """v0.1 remapping seam: EVERY histogram bin (including any pair left
    uninstantiated — the A2 nearest-repair rule) maps to its nearest entry
    within its alpha zone; pixels map through their bin key."""
    if not palette:
        return []
    entries_premult = [_entry_premult(entry) for entry in palette]
    entry_zones = [_zone_of(entry[3]) for entry in palette]
    assignment: dict[tuple[int, int, int, int], int] = {}
    for b in init.bins:
        assignment[b.key] = _nearest_entry(
            _bin_premult_mean(b), b.zone, entries_premult, entry_zones
        )
    indices: list[int] = []
    if init.exact:
        for r, g, b, a in pixels:
            indices.append(assignment[(r, g, b, a)])
    else:
        levels = PRECLIP_LEVELS
        for r, g, b, a in pixels:
            key = (
                r * levels // 256,
                g * levels // 256,
                b * levels // 256,
                _alpha_bin(a, levels),
            )
            indices.append(assignment[key])
    return indices


def stage_emit(
    width: int,
    height: int,
    palette: list[tuple[int, int, int, int]],
    indices: Sequence[int],
) -> bytes:
    """v0.1 emission seam: deterministic indexed PNG (tRNS when needed)."""
    return m1_png.write_indexed_png(width, height, palette, indices)


# --- E-0036 byte-only pack-seam extensions (T-0188) -------------------------
#
# All helpers below are inert unless a --pack-seam-* flag is on. Every emission
# they produce re-decodes pixel-identical to the baseline stage_emit output
# (the emitted PNG carries exactly the palette[index] pixel sequence, because
# every trial is a bijective palette permutation and/or a pure byte re-pack),
# so no quality claim is needed. The baseline stage_emit bytes are ALWAYS one
# of the candidates, so selection is never-worse than the frozen default.


def _seam_remap_by_order(
    palette: Sequence[tuple[int, int, int, int]],
    indices: Sequence[int],
    order: Sequence[int],
) -> tuple[list[tuple[int, int, int, int]], list[int]]:
    """Apply a bijective palette permutation ``order`` (new position -> old
    index) and consistently remap indices. Pixel sequence is invariant."""
    count = len(palette)
    inverse = [0] * count
    for new_index, old_index in enumerate(order):
        inverse[old_index] = new_index
    new_palette = [palette[old_index] for old_index in order]
    new_indices = [inverse[index] for index in indices]
    return new_palette, new_indices


def _seam_order_popularity(
    palette: Sequence[tuple[int, int, int, int]], indices: Sequence[int]
) -> list[int]:
    """Popularity order: most frequent index first (ties -> lower old index)."""
    frequency = [0] * len(palette)
    for index in indices:
        frequency[index] += 1
    return sorted(range(len(palette)), key=lambda i: (-frequency[i], i))


def _seam_order_luminance(
    palette: Sequence[tuple[int, int, int, int]]
) -> list[int]:
    """Luminance order: Rec.601 integer luma ascending (ties -> old index)."""
    def luma(i: int) -> int:
        red, green, blue, _ = palette[i]
        return 299 * red + 587 * green + 114 * blue

    return sorted(range(len(palette)), key=lambda i: (luma(i), i))


def _seam_order_channel_major(
    palette: Sequence[tuple[int, int, int, int]]
) -> list[int]:
    """Channel-major order: RGBA tuple ascending (ties -> old index)."""
    return sorted(range(len(palette)), key=lambda i: (palette[i], i))


def _seam_order_transparent_front(
    palette: Sequence[tuple[int, int, int, int]]
) -> list[int] | None:
    """ARM-R single-transparent-color tRNS rung: when EXACTLY one palette
    entry is non-opaque and it is fully transparent (alpha==0) — i.e. one
    transparent color, no partial alpha — move it to index 0 so the emitted
    tRNS payload trims to a single byte. Returns None when inapplicable."""
    nonopaque = [i for i, entry in enumerate(palette) if entry[3] < 255]
    if len(nonopaque) != 1 or palette[nonopaque[0]][3] != 0:
        return None
    transparent = nonopaque[0]
    return [transparent] + [i for i in range(len(palette)) if i != transparent]


def _seam_emit_config(
    width: int,
    height: int,
    palette: Sequence[tuple[int, int, int, int]],
    indices: Sequence[int],
    *,
    bit_depth: int,
    mem_level: int,
) -> bytes:
    """Emit a color-type-3 PNG mirroring m1_png.write_indexed_png exactly,
    parameterized by index bit depth and DEFLATE memLevel. bit_depth 8 +
    mem_level 8 reproduces the baseline stage_emit bytes byte-for-byte
    (verified: zlib.compressobj(9, DEFLATED, 15, 8, Z_DEFAULT_STRATEGY) ==
    zlib.compress(., 9))."""
    import prism_pack  # read-only reuse of pack_index_row / minimum_bit_depth

    scanlines = bytearray()
    for y in range(height):
        row = list(indices[y * width : (y + 1) * width])
        scanlines.append(0)  # filter type 0 (None), as in the baseline emit
        scanlines.extend(prism_pack.pack_index_row(row, bit_depth))
    if mem_level == 8:
        compressed = zlib.compress(bytes(scanlines), 9)
    else:
        compressor = zlib.compressobj(
            9, zlib.DEFLATED, 15, mem_level, zlib.Z_DEFAULT_STRATEGY
        )
        compressed = compressor.compress(bytes(scanlines)) + compressor.flush()

    ihdr = struct.pack(">IIBBBBB", width, height, bit_depth, 3, 0, 0, 0)
    plte_payload = bytes(
        channel for (red, green, blue, _) in palette for channel in (red, green, blue)
    )
    last_transparent = -1
    for position, (_, _, _, alpha) in enumerate(palette):
        if alpha < 255:
            last_transparent = position

    out = bytearray(m1_png.PNG_SIGNATURE)
    out += m1_png._emit_chunk(b"IHDR", ihdr)
    out += m1_png._emit_chunk(b"PLTE", plte_payload)
    if last_transparent >= 0:
        trns_payload = bytes(alpha for (_, _, _, alpha) in palette[: last_transparent + 1])
        out += m1_png._emit_chunk(b"tRNS", trns_payload)
    out += m1_png._emit_chunk(b"IDAT", compressed)
    out += m1_png._emit_chunk(b"IEND", b"")
    return bytes(out)


def _seam_emit(
    width: int,
    height: int,
    palette: list[tuple[int, int, int, int]],
    indices: Sequence[int],
    *,
    palette_sort: bool,
    memlevel_race: bool,
    reduction: bool,
) -> tuple[bytes, dict[str, Any]]:
    """Trial the enabled byte-only pack-seam techniques and return the
    SMALLEST stream that re-decodes pixel-identical to the baseline, plus an
    evidence dict. The baseline (identity order, 8-bit, memLevel 8) is always
    a candidate, so the result is never larger than stage_emit."""
    import prism_pack

    count = len(palette)
    expected_pixels = tuple(palette[index] for index in indices)

    # Candidate palette orderings (new position -> old index). Identity is the
    # baseline order and is always present.
    orders: list[tuple[str, list[int]]] = [("identity", list(range(count)))]
    if palette_sort:
        orders.append(("popularity", _seam_order_popularity(palette, indices)))
        orders.append(("luminance", _seam_order_luminance(palette)))
        orders.append(("channel-major", _seam_order_channel_major(palette)))
    if reduction:
        front = _seam_order_transparent_front(palette)
        if front is not None:
            orders.append(("trns-front", front))

    # Candidate index bit depths (ARM-R reduction rungs). 8 is the baseline.
    depths = [8]
    if reduction:
        min_depth = prism_pack.minimum_bit_depth(count)
        if min_depth < 8:
            depths.append(min_depth)

    # Candidate DEFLATE memLevels (ARM-M race). 8 is the baseline.
    mems = [8]
    if memlevel_race:
        mems.append(5)

    # Deterministic tie-break ranks (applied only among size-ties):
    #   mem 5 preferred over 8   -> ARM-M "deterministic tie-break to 5"
    #   identity order preferred -> ARM-S/ARM-R keep baseline order on a tie
    #   depth 8 preferred        -> reduction rungs kept ONLY when strictly smaller
    order_rank = {name: rank for rank, (name, _) in enumerate(orders)}

    best: tuple[tuple[int, int, int, int], bytes] | None = None
    best_meta: dict[str, Any] = {}
    trials = 0
    for order_name, order in orders:
        permuted_palette, permuted_indices = _seam_remap_by_order(
            palette, indices, order
        )
        for bit_depth in depths:
            # A reduced bit depth requires the packed indices to fit; by
            # construction indices < count <= 2**minimum_bit_depth, so the
            # only reduced depth offered already fits.
            for mem_level in mems:
                data = _seam_emit_config(
                    width,
                    height,
                    permuted_palette,
                    permuted_indices,
                    bit_depth=bit_depth,
                    mem_level=mem_level,
                )
                # Independent decoded-pixel identity gate (per trial).
                decoded = m1_png.decode_png(data)
                if (decoded.width, decoded.height) != (width, height):
                    raise PrismQuantError("internal: seam trial changed dimensions")
                if tuple(decoded.pixels) != expected_pixels:
                    raise PrismQuantError(
                        "internal: seam trial failed decoded-pixel identity"
                    )
                trials += 1
                key = (
                    len(data),
                    0 if mem_level == 5 else 1,
                    order_rank[order_name],
                    0 if bit_depth == 8 else 1,
                )
                if best is None or key < best[0]:
                    best = (key, data)
                    best_meta = {
                        "order": order_name,
                        "bit_depth": bit_depth,
                        "mem_level": mem_level,
                    }
    assert best is not None
    best_meta["trials"] = trials
    return best[1], best_meta


# --- E-0037/T-0189 fewest-colors-at-quality search --------------------------


def colors_search_mse_ceiling(quality: float) -> float:
    """Frozen monotone quality->premultiplied-RGBA-MSE ceiling (E-0037).

    Strictly decreasing on ``quality`` in [0, 100]: a higher quality target
    maps to a tighter (smaller) internal-MSE ceiling. Shape from
    lab/design-notes/dither-pack-and-qa-techniques.md §6."""
    q = float(quality)
    q_term = 2.5 / (210.0 + q) ** 1.2 * (100.1 - q) / 100.0
    return COLORS_SEARCH_CURVE_W * (COLORS_SEARCH_CURVE_LOW_Q_FUDGE + q_term)


def premultiplied_rgba_mse(
    reference: Sequence[tuple[int, int, int, int]],
    candidate: Sequence[tuple[int, int, int, int]],
) -> float:
    """The internal search objective: the premultiplied-linear-RGBA MSE that
    ``m1_metrics.compute_metrics`` reports (T-0053), returned as a float in
    [0, 1]. Integer accumulation is exact; only the final normalisation is
    floating point."""
    total = 0
    n = len(reference)
    if n == 0:
        raise PrismQuantError("internal: empty pixel sequence in colors-search")
    for (r0, g0, b0, a0), (r1, g1, b1, a1) in zip(reference, candidate):
        pr = a0 * r0 - a1 * r1
        pg = a0 * g0 - a1 * g1
        pb = a0 * b0 - a1 * b1
        pa = 255 * (a0 - a1)
        total += pr * pr + pg * pg + pb * pb + pa * pa
    return total / (n * 4 * 255 * 255 * 255 * 255)


def parse_colors_search(spec: str) -> tuple[int, int, float]:
    """Parse ``MIN..MAX@QUALITY`` -> (min_colors, max_colors, quality).

    Raises ``PrismQuantError`` on any malformed field (the CLI maps that to a
    usage error)."""
    at_parts = spec.split("@")
    if len(at_parts) != 2:
        raise PrismQuantError("--colors-search must be MIN..MAX@QUALITY")
    range_part, quality_part = at_parts
    bounds = range_part.split("..")
    if len(bounds) != 2:
        raise PrismQuantError("--colors-search range must be MIN..MAX")
    try:
        lo = int(bounds[0], 10)
        hi = int(bounds[1], 10)
    except ValueError:
        raise PrismQuantError("--colors-search MIN/MAX must be integers")
    try:
        quality = float(quality_part)
    except ValueError:
        raise PrismQuantError("--colors-search QUALITY must be a number")
    _validate_colors_search_bounds(lo, hi, quality)
    return lo, hi, quality


def _validate_colors_search_bounds(lo: int, hi: int, quality: float) -> None:
    if lo < 1 or hi > MAX_COLORS or lo > hi:
        raise PrismQuantError(
            f"--colors-search requires 1 <= MIN <= MAX <= {MAX_COLORS}"
        )
    if not (COLORS_SEARCH_MIN_QUALITY <= quality <= COLORS_SEARCH_MAX_QUALITY):
        raise PrismQuantError("--colors-search QUALITY must be in 0..100")


def run_colors_search(
    source: m1_png.DecodedImage,
    min_colors: int,
    max_colors: int,
    quality: float,
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY,
    color_space: str = DEFAULT_COLOR_SPACE,
) -> dict[str, Any]:
    """Find the smallest palette size in [min_colors, max_colors] whose
    internal premultiplied-RGBA MSE is at or below the frozen ceiling for
    ``quality``. Candidate MSE is not monotonic in palette size, so every size
    in the bounded (at most 256-entry) range is evaluated exactly once. If no
    size meets the ceiling, ``chosen`` = ``max_colors`` and ``fallback`` = True
    (never-worse: emit the cap, i.e. the unmodified fixed-cap path). See
    E-0037 §5."""
    _validate_colors_search_bounds(min_colors, max_colors, quality)
    ceiling = colors_search_mse_ceiling(quality)
    cache: dict[int, float] = {}

    def mse_at(n: int) -> float:
        if n not in cache:
            palette, indices, _notes = quantize_candidate(
                source, n, hidden_rgb_policy, color_space
            )
            remapped = [palette[index] for index in indices]
            cache[n] = premultiplied_rgba_mse(source.pixels, remapped)
        return cache[n]

    passing_colors = [
        colors
        for colors in range(min_colors, max_colors + 1)
        if mse_at(colors) <= ceiling
    ]
    cap_mse = cache[max_colors]
    if passing_colors:
        chosen = passing_colors[0]
        fallback = False
    else:
        chosen = max_colors
        fallback = True
    return {
        "min_colors": min_colors,
        "max_colors": max_colors,
        "quality": quality,
        "ceiling": ceiling,
        "chosen_colors": chosen,
        "chosen_mse": cache[chosen],
        "cap_mse": cap_mse,
        "fallback": fallback,
        "search_method": "exhaustive",
        "evaluated_count": len(cache),
        "passing_colors": passing_colors,
        "evaluations": dict(sorted(cache.items())),
    }


def quantize_candidate(
    source: m1_png.DecodedImage,
    colors: int,
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY,
    color_space: str = DEFAULT_COLOR_SPACE,
) -> tuple[list[tuple[int, int, int, int]], list[int], StageNotes]:
    """Run the core with the selected assignment/refinement/remap space."""
    if color_space not in COLOR_SPACES:
        raise PrismQuantError(f"unknown color-space: {color_space}")
    sampled = stage_sample(source.pixels)
    init = stage_palette_init(sampled, colors, hidden_rgb_policy)
    if color_space == "srgb":
        palette, iterations = stage_refinement(init, colors)
        indices = stage_remap(sampled, init, palette)
    else:
        feature_by_key = _oklab_feature_bins(sampled, init)
        palette, iterations = _refine_oklab(init, feature_by_key)
        indices = _remap_oklab(sampled, init, palette, feature_by_key)
    nonopaque = sum(1 for pixel in source.pixels if pixel[3] < 255)
    alpha_note = (
        "alpha preserved via tRNS (extremes exact; interior quantized)"
        if nonopaque
        else "source fully opaque; no tRNS emitted"
    )
    notes = StageNotes(
        sampled_pixels=len(sampled),
        initial_bins=len(init.bins),
        refined_palette_entries=len(palette),
        alpha_note=alpha_note,
        exact_path=init.exact_path,
        palette_init_pairs=len(init.palette),
        refinement_iterations=iterations,
        hidden_rgb_policy=hidden_rgb_policy,
    )
    return palette, indices, notes


def quantize_image(
    source: m1_png.DecodedImage,
    colors: int,
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY,
    color_space: str = DEFAULT_COLOR_SPACE,
) -> tuple[bytes, list[tuple[int, int, int, int]], StageNotes]:
    """Run the six-stage v0.1 pipeline over one decoded image."""
    palette, indices, notes = quantize_candidate(
        source, colors, hidden_rgb_policy, color_space
    )
    output = stage_emit(source.width, source.height, palette, indices)
    return output, palette, notes


def quantize_png(
    in_path: Path,
    out_path: Path,
    colors: int,
    hidden_rgb_policy: str = DEFAULT_HIDDEN_RGB_POLICY,
    *,
    color_space: str = DEFAULT_COLOR_SPACE,
    adaptive_default: str | bool = DEFAULT_ADAPTIVE_DEFAULT,
    dither: bool = DEFAULT_DITHER,
    dither_strength: tuple[int, int] = DEFAULT_DITHER_STRENGTH,
    dither_strength_explicit: bool = False,
    dither_policy: str = DEFAULT_DITHER_POLICY,
    pack_mode: str = DEFAULT_PACK_MODE,
    pack_search: str = DEFAULT_PACK_SEARCH,
    pack_seam_palette_sort: bool | None = None,
    pack_seam_memlevel: bool | None = None,
    pack_seam_reduction: bool | None = None,
    colors_search: tuple[int, int, float] | None = None,
    source_bytes: bytes | None = None,
) -> dict[str, Any]:
    """Decode, run the v0.1 core, opt into dither/pack, and self-verify.

    E-0037: when ``colors_search`` is given as ``(min, max, quality)`` the
    effective ``colors`` is the fewest-colors-at-quality search result (the
    passed ``colors`` is overridden); when it is ``None`` the pipeline is
    byte-identical to the pre-E-0037 path."""
    if isinstance(adaptive_default, bool):
        # Preserve the pre-T-0190 programmatic surface while the CLI exposes
        # the three named policies. True retains frozen unguarded-on meaning.
        adaptive_default_policy = "on" if adaptive_default else "off"
    else:
        adaptive_default_policy = adaptive_default
    if adaptive_default_policy not in ADAPTIVE_DEFAULT_POLICIES:
        raise PrismQuantError(
            "--adaptive-default must be one of "
            + ", ".join(ADAPTIVE_DEFAULT_POLICIES)
        )
    if adaptive_default_policy in ("on", "guarded"):
        if (
            dither != DEFAULT_DITHER
            or dither_strength != DEFAULT_DITHER_STRENGTH
            or dither_strength_explicit
            or dither_policy != DEFAULT_DITHER_POLICY
        ):
            raise PrismQuantError(
                f"--adaptive-default {adaptive_default_policy} is not composable "
                "with explicit dither options"
            )
        dither = True
        dither_policy = "adaptive-unit"
    if colors_search is not None:
        _validate_colors_search_bounds(*colors_search)
    if colors < 1 or colors > MAX_COLORS:
        raise PrismQuantError(f"--colors must be in 1..{MAX_COLORS}")
    if hidden_rgb_policy not in HIDDEN_RGB_POLICIES:
        raise PrismQuantError(
            f"--hidden-rgb-policy must be one of {', '.join(HIDDEN_RGB_POLICIES)}"
        )
    if color_space not in COLOR_SPACES:
        raise PrismQuantError(
            f"--color-space must be one of {', '.join(COLOR_SPACES)}"
        )
    if dither_policy not in DITHER_POLICIES:
        raise PrismQuantError(
            f"--dither-policy must be one of {', '.join(DITHER_POLICIES)}"
        )
    if pack_mode not in PACK_MODES:
        raise PrismQuantError(f"--pack must be one of {', '.join(PACK_MODES)}")
    if pack_search not in PACK_SEARCHES:
        raise PrismQuantError(
            f"--pack-search must be one of {', '.join(PACK_SEARCHES)}"
        )
    requested_pack_seams = (
        pack_seam_palette_sort,
        pack_seam_memlevel,
        pack_seam_reduction,
    )
    if pack_mode != "none" and any(value is True for value in requested_pack_seams):
        raise PrismQuantError(
            "--pack-seam-* flags apply to the pack=none emission path only "
            "(--pack fast/max runs its own byte search)"
        )
    pack_seam_explicit = any(value is not None for value in requested_pack_seams)
    if pack_mode == "none" and not pack_seam_explicit:
        pack_seam_palette_sort = (
            DEFAULT_PACK_SEAM_PALETTE_SORT
        )
        pack_seam_memlevel = (
            DEFAULT_PACK_SEAM_MEMLEVEL
        )
        pack_seam_reduction = (
            DEFAULT_PACK_SEAM_REDUCTION
        )
    elif pack_mode == "none":
        # Frozen-flag rule: once an invocation names any E-0036 seam flag,
        # unspecified peer flags retain their E-0036 default-off behavior.
        # Thus every pre-adoption explicit invocation keeps its exact bytes.
        pack_seam_palette_sort = bool(pack_seam_palette_sort)
        pack_seam_memlevel = bool(pack_seam_memlevel)
        pack_seam_reduction = bool(pack_seam_reduction)
    else:
        # E-0040 retains the composition gate. Omission must not make a normal
        # --pack fast/max invocation fail merely because S/R default on for the
        # separate pack=none emission surface.
        pack_seam_palette_sort = False
        pack_seam_memlevel = False
        pack_seam_reduction = False
    pack_seam_on = (
        pack_seam_palette_sort or pack_seam_memlevel or pack_seam_reduction
    )
    if (
        not isinstance(dither_strength, tuple)
        or len(dither_strength) != 2
        or any(
            isinstance(value, bool) or not isinstance(value, int)
            for value in dither_strength
        )
        or dither_strength[0] < 0
        or dither_strength[1] <= 0
        or dither_strength[0] > dither_strength[1]
    ):
        raise PrismQuantError("--dither-strength must be an exact ratio in 0..1")
    if dither_policy in ("adaptive", "region", "luma-bluenoise") and not dither:
        raise PrismQuantError(
            f"--dither-policy {dither_policy} requires --dither on"
        )
    if dither_policy in ("adaptive", "region"):
        if dither_strength != (1, 1):
            raise PrismQuantError(
                "--dither-strength is not composable with "
                f"--dither-policy {dither_policy} (policy supplies exact strengths)"
            )
    if source_bytes is None:
        try:
            raw = m1_png.read_png_file(in_path)
        except OSError as exc:
            raise PrismQuantError(f"io_error: cannot read {in_path}: {exc}") from exc
        except m1_png.PngResourceError as exc:
            raise PrismQuantError(f"data_error: cannot decode {in_path}: {exc}") from exc
    else:
        # The CLI retains these exact bytes for never-worse fallback. Reusing
        # that snapshot binds decode and publication to one source identity
        # while avoiding a second MAX_INPUT_BYTES-sized allocation.
        raw = source_bytes
    try:
        source = m1_png.decode_png(raw)
    except m1_png.PngError as exc:
        raise PrismQuantError(f"data_error: cannot decode {in_path}: {exc}") from exc
    colors_search_trace: dict[str, Any] | None = None
    if colors_search is not None:
        # E-0037: resolve the effective palette size by the fewest-colors
        # search, then run the otherwise-unchanged pipeline at that size (so
        # the searched output is byte-identical to an explicit --colors chosen
        # invocation).
        colors_search_trace = run_colors_search(
            source, *colors_search, hidden_rgb_policy, color_space
        )
        colors = colors_search_trace["chosen_colors"]
    palette, indices, notes = quantize_candidate(
        source, colors, hidden_rgb_policy, color_space
    )
    adaptive_default_guard_fired = False
    adaptive_default_opaque_count = sum(pixel[3] == 255 for pixel in source.pixels)
    adaptive_default_opaque_frac = round(
        adaptive_default_opaque_count / len(source.pixels), 4
    )
    adaptive_unit_classes = None
    if adaptive_default_policy == "guarded":
        # Reuse the existing classifier work required by adaptive-unit. The
        # accepted Option-A predicate is E-0032's four-decimal opaque_frac
        # feature. Classify even on the guarded-off branch so the policy does
        # not acquire a second structural-analysis path.
        import prism_dither

        adaptive_unit_classes = prism_dither.classify_regions(
            source.pixels, source.width, source.height, palette
        )
        adaptive_default_guard_fired = adaptive_default_opaque_frac == 0.0
        if adaptive_default_guard_fired:
            dither = False
    dither_algorithm = "none"
    dither_effective_strength = (0, 1) if not dither else dither_strength
    if dither:
        import prism_dither

        try:
            if dither_policy == "luma-bluenoise":
                # Call the promoted E-0017 implementation directly. This is a
                # threshold-mask dither, not a Floyd-Steinberg region-hook
                # variant; --dither-strength scales its mask amplitude.
                remap = prism_dither.luma_bluenoise_remap(
                    source.pixels,
                    source.width,
                    source.height,
                    palette,
                    colors=colors,
                    strength=dither_strength,
                )
            elif dither_policy == "adaptive":
                region_hook = prism_dither.adaptive_strength_hook(
                    source.pixels, source.width, source.height, palette
                )
            elif dither_policy == "region":
                region_hook, _classes = prism_dither.region_policy_hook(
                    source.pixels, source.width, source.height, palette
                )
            elif dither_policy == "adaptive-unit":
                if dither_strength_explicit:
                    dither_effective_strength = dither_strength
                else:
                    classes = adaptive_unit_classes
                    if classes is None:
                        classes = prism_dither.classify_regions(
                            source.pixels, source.width, source.height, palette
                        )
                    dither_effective_strength = prism_dither._unit_strength_from_classes(
                        classes
                    )
                region_hook = (
                    None
                    if dither_effective_strength == (1, 1)
                    else prism_dither._uniform_strength_hook(dither_effective_strength)
                )
            else:
                # The exact T-0080 path is reused, including the historical
                # region_hook=None full-strength fast path.
                region_hook = (
                    None
                    if dither_strength == (1, 1)
                    else prism_dither._uniform_strength_hook(dither_strength)
                )
            if dither_policy != "luma-bluenoise":
                remap = prism_dither.floyd_steinberg(
                    source.pixels,
                    source.width,
                    source.height,
                    palette,
                    region_hook=region_hook,
                )
        except prism_dither.DitherCliError as exc:
            # Preserve the promoted remapper's declared io/internal prefix so
            # this CLI maps mask-load failures to the same public exit class.
            raise PrismQuantError(str(exc)) from exc
        except ValueError as exc:
            raise PrismQuantError(f"data_error: cannot dither candidate: {exc}") from exc
        indices = list(remap.indices)
        dither_algorithm = remap.evidence.algorithm
    pack_seam_meta: dict[str, Any] = {}
    if pack_mode == "none":
        if pack_seam_on:
            output, pack_seam_meta = _seam_emit(
                source.width,
                source.height,
                palette,
                indices,
                palette_sort=pack_seam_palette_sort,
                memlevel_race=pack_seam_memlevel,
                reduction=pack_seam_reduction,
            )
        else:
            output = stage_emit(source.width, source.height, palette, indices)
    else:
        import prism_pack

        try:
            packed = prism_pack.pack_indexed_png(
                source.width,
                source.height,
                palette,
                indices,
                mode=pack_mode,
                search_version=pack_search,
            )
        except prism_pack.PackError as exc:
            raise PrismQuantError(f"data_error: {exc}") from exc
        output = packed.data
    # Self-verification: the emitted bytes must re-decode under the
    # independent decoder to the declared palette and dimensions.
    check = m1_png.decode_png(output)
    if (check.width, check.height) != (source.width, source.height):
        raise PrismQuantError("internal: emitted dimensions differ from source")
    if len(check.properties["plte"] or ()) > colors:
        raise PrismQuantError("internal: emitted palette exceeds --colors")
    expected_pixels = tuple(palette[index] for index in indices)
    if tuple(check.pixels) != expected_pixels:
        raise PrismQuantError("internal: emitted pixels differ from remap candidate")
    try:
        out_path.write_bytes(output)
    except OSError as exc:
        raise PrismQuantError(f"io_error: cannot write {out_path}: {exc}") from exc
    return {
        "version": VERSION,
        "label": LABEL,
        "colors": colors,
        "colors_search": colors_search_trace,
        "hidden_rgb_policy": hidden_rgb_policy,
        "color_space": color_space,
        "adaptive_default": adaptive_default_policy,
        "adaptive_default_guard_fired": adaptive_default_guard_fired,
        "adaptive_default_opaque_count": adaptive_default_opaque_count,
        "adaptive_default_opaque_frac": adaptive_default_opaque_frac,
        "dither": "on" if dither else "off",
        "dither_strength": dither_strength,
        "dither_effective_strength": dither_effective_strength,
        "dither_policy": dither_policy,
        "dither_algorithm": dither_algorithm,
        "pack_mode": pack_mode,
        "pack_search": pack_search,
        "pack_seam_palette_sort": "on" if pack_seam_palette_sort else "off",
        "pack_seam_memlevel": "on" if pack_seam_memlevel else "off",
        "pack_seam_reduction": "on" if pack_seam_reduction else "off",
        "pack_seam": pack_seam_meta,
        "source_bytes": len(raw),
        "output_bytes": len(output),
        "palette_entries": len(check.properties["plte"] or ()),
        "stages": {
            "sampled_pixels": notes.sampled_pixels,
            "initial_bins": notes.initial_bins,
            "refined_palette_entries": notes.refined_palette_entries,
            "alpha_note": notes.alpha_note,
        },
    }


HELP = """\
usage: pngprism <in.png> <out.png> [options]

Quantize a PNG to an indexed PNG. On success the engine's encoded output is
written to <out.png>; if that output would be >= the input file's bytes, the
input bytes are emitted verbatim instead (the never-worse guarantee).

positional arguments:
  <in.png>                    source PNG to quantize
  <out.png>                   destination indexed PNG

options:
  --colors N                  palette-size ceiling (1..=256; default 256)
  --colors-search MIN..MAX@QUALITY   fewest-colors-at-quality search
  --hidden-rgb-policy P        fully-transparent RGB policy
  --color-space srgb|oklab    quantization color space
  --adaptive-default off|on|guarded   adaptive-unit dither default policy
  --dither off|on             enable error-diffusion dither
  --dither-strength S          exact dither strength ratio in 0..1
  --dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise
  --pack none|fast|max        lossless indexed-PNG packing search
  --pack-search v1|v2         packing search variant
  --pack-seam-palette-sort off|on
  --pack-seam-memlevel off|on
  --pack-seam-reduction off|on
  --max-pixels N              pixel admission ceiling (>=1; default 67108864 =
                              64 Mi-pixel); overrides up or down, hard bound
  --report json               emit a machine-readable JSON report on stdout
  --version                   print the version and exit
  --help                      print this help and exit

exit codes: 0 success, 2 usage error, 3 data error, 5 input I/O error,
70 internal error. See lib/prism-quant/docs/cli-contract.md for the policy.
"""


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    # ``--help``/``--version`` short-circuit anywhere in argv (GNU convention),
    # help winning when both are present. The version string has ONE source:
    # the module ``__version__`` (aliasing VERSION). See docs/cli-contract.md.
    if "--help" in args:
        sys.stdout.write(HELP)
        return 0
    if "--version" in args:
        print(f"pngprism {__version__}")
        return 0
    colors = DEFAULT_COLORS
    report_json = False
    colors_explicit = False
    colors_search: tuple[int, int, float] | None = None
    hidden_rgb_policy = DEFAULT_HIDDEN_RGB_POLICY
    color_space = DEFAULT_COLOR_SPACE
    adaptive_default = DEFAULT_ADAPTIVE_DEFAULT
    dither = DEFAULT_DITHER
    dither_explicit = False
    dither_strength = DEFAULT_DITHER_STRENGTH
    dither_strength_explicit = False
    dither_policy = DEFAULT_DITHER_POLICY
    dither_policy_explicit = False
    pack_mode = DEFAULT_PACK_MODE
    pack_search = DEFAULT_PACK_SEARCH
    pack_search_explicit = False
    # ``None`` records omission. quantize_png resolves it to the adopted S/R
    # defaults on pack=none and to the documented gated-off state on fast/max.
    pack_seam_palette_sort: bool | None = None
    pack_seam_memlevel: bool | None = None
    pack_seam_reduction: bool | None = None
    max_pixels: int | None = None
    positional: list[str] = []
    index = 0
    while index < len(args):
        token = args[index]
        if token == "--colors":
            if index + 1 >= len(args):
                print("usage_error: --colors needs a value", file=sys.stderr)
                return 2
            try:
                colors = int(args[index + 1], 10)
            except ValueError:
                print("usage_error: --colors must be an integer", file=sys.stderr)
                return 2
            colors_explicit = True
            index += 2
        elif token == "--colors-search":
            if index + 1 >= len(args):
                print("usage_error: --colors-search needs a value", file=sys.stderr)
                return 2
            try:
                colors_search = parse_colors_search(args[index + 1])
            except PrismQuantError as exc:
                print(f"usage_error: {exc}", file=sys.stderr)
                return 2
            index += 2
        elif token == "--hidden-rgb-policy":
            if index + 1 >= len(args):
                print("usage_error: --hidden-rgb-policy needs a value", file=sys.stderr)
                return 2
            hidden_rgb_policy = args[index + 1]
            index += 2
        elif token == "--color-space":
            if index + 1 >= len(args):
                print("usage_error: --color-space needs a value", file=sys.stderr)
                return 2
            color_space = args[index + 1]
            if color_space not in COLOR_SPACES:
                print(
                    "usage_error: --color-space must be srgb or oklab",
                    file=sys.stderr,
                )
                return 2
            index += 2
        elif token == "--adaptive-default":
            if index + 1 >= len(args):
                print("usage_error: --adaptive-default needs a value", file=sys.stderr)
                return 2
            value = args[index + 1]
            if value not in ADAPTIVE_DEFAULT_POLICIES:
                print(
                    "usage_error: --adaptive-default must be off, on, or guarded",
                    file=sys.stderr,
                )
                return 2
            adaptive_default = value
            index += 2
        elif token == "--dither":
            if index + 1 >= len(args):
                print("usage_error: --dither needs a value", file=sys.stderr)
                return 2
            value = args[index + 1]
            if value not in ("off", "on"):
                print("usage_error: --dither must be off or on", file=sys.stderr)
                return 2
            dither = value == "on"
            dither_explicit = True
            index += 2
        elif token == "--dither-strength":
            if index + 1 >= len(args):
                print("usage_error: --dither-strength needs a value", file=sys.stderr)
                return 2
            import prism_dither

            try:
                dither_strength = prism_dither._parse_dither_strength(args[index + 1])
            except prism_dither.DitherCliError as exc:
                print(str(exc), file=sys.stderr)
                return exc.status
            dither_strength_explicit = True
            index += 2
        elif token == "--dither-policy":
            if index + 1 >= len(args):
                print("usage_error: --dither-policy needs a value", file=sys.stderr)
                return 2
            dither_policy = args[index + 1]
            dither_policy_explicit = True
            if dither_policy not in DITHER_POLICIES:
                print(
                    "usage_error: --dither-policy must be uniform, adaptive, region, "
                    "adaptive-unit, or luma-bluenoise",
                    file=sys.stderr,
                )
                return 2
            index += 2
        elif token == "--pack":
            if index + 1 >= len(args):
                print("usage_error: --pack needs a value", file=sys.stderr)
                return 2
            pack_mode = args[index + 1]
            if pack_mode not in PACK_MODES:
                print("usage_error: --pack must be none, fast, or max", file=sys.stderr)
                return 2
            index += 2
        elif token == "--pack-search":
            if index + 1 >= len(args):
                print("usage_error: --pack-search needs a value", file=sys.stderr)
                return 2
            pack_search = args[index + 1]
            if pack_search not in PACK_SEARCHES:
                print("usage_error: --pack-search must be v1 or v2", file=sys.stderr)
                return 2
            pack_search_explicit = True
            index += 2
        elif token in (
            "--pack-seam-palette-sort",
            "--pack-seam-memlevel",
            "--pack-seam-reduction",
        ):
            if index + 1 >= len(args):
                print(f"usage_error: {token} needs a value", file=sys.stderr)
                return 2
            value = args[index + 1]
            if value not in ("off", "on"):
                print(f"usage_error: {token} must be off or on", file=sys.stderr)
                return 2
            enabled = value == "on"
            if token == "--pack-seam-palette-sort":
                pack_seam_palette_sort = enabled
            elif token == "--pack-seam-memlevel":
                pack_seam_memlevel = enabled
            else:
                pack_seam_reduction = enabled
            index += 2
        elif token == "--max-pixels":
            if index + 1 >= len(args):
                print("usage_error: --max-pixels needs a value", file=sys.stderr)
                return 2
            try:
                value = int(args[index + 1], 10)
            except ValueError:
                print("usage_error: --max-pixels must be an integer", file=sys.stderr)
                return 2
            if value < 1:
                print(
                    "usage_error: --max-pixels must be a positive integer",
                    file=sys.stderr,
                )
                return 2
            max_pixels = value
            index += 2
        elif token == "--report":
            if index + 1 >= len(args):
                print("usage_error: --report needs a value", file=sys.stderr)
                return 2
            if args[index + 1] != "json":
                print("usage_error: --report must be json", file=sys.stderr)
                return 2
            report_json = True
            index += 2
        elif token.startswith("-"):
            print(f"usage_error: unknown option {token}", file=sys.stderr)
            return 2
        else:
            positional.append(token)
            index += 1
    if colors_search is not None and colors_explicit:
        print(
            "usage_error: --colors-search is mutually exclusive with --colors",
            file=sys.stderr,
        )
        return 2
    if (
        adaptive_default in ("on", "guarded")
        and (dither_explicit or dither_strength_explicit or dither_policy_explicit)
    ):
        print(
            f"usage_error: --adaptive-default {adaptive_default} is not composable "
            "with explicit dither options",
            file=sys.stderr,
        )
        return 2
    if pack_search_explicit and pack_mode == "none":
        print("usage_error: --pack-search requires --pack fast or max", file=sys.stderr)
        return 2
    if any(
        value is True
        for value in (
            pack_seam_palette_sort,
            pack_seam_memlevel,
            pack_seam_reduction,
        )
    ) and pack_mode != "none":
        print(
            "usage_error: --pack-seam-* flags apply to the pack=none emission "
            "path only (--pack fast/max runs its own byte search)",
            file=sys.stderr,
        )
        return 2
    if dither_policy in ("adaptive", "region", "luma-bluenoise") and not dither:
        print(
            f"usage_error: --dither-policy {dither_policy} requires --dither on",
            file=sys.stderr,
        )
        return 2
    if dither_policy in ("adaptive", "region") and dither_strength != (1, 1):
        print(
            "usage_error: --dither-strength is not composable with "
            f"--dither-policy {dither_policy} (policy supplies exact strengths)",
            file=sys.stderr,
        )
        return 2
    if len(positional) != 2:
        print(
            f"usage: pngprism <in.png> <out.png> [--colors N] "
            f"[--colors-search MIN..MAX@QUALITY] "
            f"[--hidden-rgb-policy P] [--color-space srgb|oklab] "
            f"[--adaptive-default off|on|guarded] "
            f"[--dither off|on] [--dither-strength S] "
            f"[--dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise] "
            f"[--pack none|fast|max] "
            f"[--pack-search v1|v2] "
            f"[--pack-seam-palette-sort off|on] "
            f"[--pack-seam-memlevel off|on] "
            f"[--pack-seam-reduction off|on] [--max-pixels N]  ({LABEL})",
            file=sys.stderr,
        )
        return 2
    # Apply the pixel-ceiling override (``--max-pixels``) once, before any
    # decode. A HARD admission bound checked at IHDR (before inflation/
    # allocation) by every decode: source admission and the pipeline's own
    # self-verification re-decode alike. Omission leaves the 64 Mi-pixel default
    # (m1_png.MAX_PIXELS). A user raising it above their RAM owns that: the
    # no-OOM guarantee holds at or below the active ceiling.
    if max_pixels is not None:
        m1_png.set_max_pixels(max_pixels)
    input_path = Path(positional[0])
    output_path = Path(positional[1])
    staged_output: Path | None = None
    try:
        # Retain the original before any candidate write. Candidate generation
        # targets a fresh sibling file, so exact-path, hardlink, and symlink
        # aliases cannot truncate the source before the never-worse decision.
        input_bytes = m1_png.read_png_file(input_path)
        try:
            destination_stat = output_path.lstat()
        except FileNotFoundError:
            destination_mode = None
        else:
            if stat.S_ISREG(destination_stat.st_mode):
                destination_mode = stat.S_IMODE(destination_stat.st_mode)
            elif stat.S_ISLNK(destination_stat.st_mode):
                try:
                    target_stat = output_path.stat()
                except FileNotFoundError:
                    destination_mode = None
                else:
                    if not stat.S_ISREG(target_stat.st_mode):
                        raise OSError("symlink target is not a regular file")
                    destination_mode = stat.S_IMODE(target_stat.st_mode)
            else:
                destination_mode = None
        # os.open(..., 0o666) gives a fresh output the same umask-governed mode
        # as ordinary Path.write_bytes, unlike mkstemp's forced 0o600.
        for _ in range(128):
            candidate = output_path.parent / (
                f".pngprism-{os.getpid()}-{secrets.token_hex(8)}.tmp"
            )
            try:
                fd = os.open(
                    candidate,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                    0o666,
                )
            except FileExistsError:
                continue
            staged_output = candidate
            try:
                if destination_mode is not None:
                    # Match the final confidentiality/read exposure before any
                    # pixels are written, adding only owner-write temporarily.
                    os.fchmod(fd, destination_mode | stat.S_IWUSR)
                os.close(fd)
            except OSError:
                with suppress(OSError):
                    staged_output.unlink()
                staged_output = None
                raise
            break
        if staged_output is None:
            raise OSError("could not reserve a temporary output")
    except m1_png.PngResourceError as exc:
        print(f"data_error: cannot decode {input_path}: {exc}", file=sys.stderr)
        return 3
    except OSError as exc:
        print(f"io_error: {exc}", file=sys.stderr)
        return 5
    try:
        summary = quantize_png(
            input_path,
            staged_output,
            colors,
            hidden_rgb_policy,
            color_space=color_space,
            adaptive_default=adaptive_default,
            dither=dither,
            dither_strength=dither_strength,
            dither_strength_explicit=dither_strength_explicit,
            dither_policy=dither_policy,
            pack_mode=pack_mode,
            pack_search=pack_search,
            pack_seam_palette_sort=pack_seam_palette_sort,
            pack_seam_memlevel=pack_seam_memlevel,
            pack_seam_reduction=pack_seam_reduction,
            colors_search=colors_search,
            source_bytes=input_bytes,
        )
        # Never-worse output guarantee (T-0210, item 1): select final bytes in
        # staging, then publish with one same-directory atomic replacement.
        # quantize_png itself stays unchanged, preserving library semantics and
        # every encoded byte stream.
        final_output_bytes = summary["output_bytes"]
        never_worse_fallback = False
        if summary["output_bytes"] >= summary["source_bytes"]:
            staged_output.write_bytes(input_bytes)
            final_output_bytes = len(input_bytes)
            never_worse_fallback = True
        if destination_mode is not None:
            os.chmod(staged_output, destination_mode, follow_symlinks=False)
        os.replace(staged_output, output_path)
    except PrismQuantError as exc:
        message = str(exc)
        if message.startswith("io_error"):
            print(message, file=sys.stderr)
            return 5
        if message.startswith("internal"):
            print(message, file=sys.stderr)
            return 70
        print(message, file=sys.stderr)
        return 3
    except OSError as exc:
        print(f"io_error: cannot write {output_path}: {exc}", file=sys.stderr)
        return 5
    finally:
        if staged_output is not None:
            # Cleanup is best-effort and must never replace the intended CLI
            # status/diagnostic if another process races the temp entry.
            with suppress(OSError):
                staged_output.unlink()
    candidate = "input-verbatim" if never_worse_fallback else "encoded"
    if report_json:
        # Compact, stable-key-order JSON matching the Rust port's hand-written
        # form (separators=(",", ":")). Deliberately omits the version string
        # so the two impls' reports are byte-identical despite version drift.
        # ``palette_size`` is the engine candidate's palette even on a
        # never-worse fallback (the candidate was built, then discarded).
        report = {
            "schema_version": "prism.cli.report/1",
            "bytes_in": summary["source_bytes"],
            "bytes_out": final_output_bytes,
            "palette_size": summary["palette_entries"],
            "candidate": candidate,
            "guard": adaptive_default if isinstance(adaptive_default, str) else (
                "on" if adaptive_default else "off"
            ),
            "never_worse_fallback": never_worse_fallback,
        }
        print(json.dumps(report, separators=(",", ":")))
    else:
        print(
            "pngprism {version}: {in_bytes} -> {out_bytes} bytes, "
            "{palette} palette entries ({alpha})".format(
                version=summary["version"],
                in_bytes=summary["source_bytes"],
                out_bytes=final_output_bytes,
                palette=summary["palette_entries"],
                alpha=summary["stages"]["alpha_note"],
            )
        )
        if never_worse_fallback:
            print(
                "never-worse: encoded output ({encoded} bytes) >= input "
                "({input} bytes); emitted input verbatim".format(
                    encoded=summary["output_bytes"],
                    input=summary["source_bytes"],
                )
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

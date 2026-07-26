#!/usr/bin/env python3
"""Alpha-boundary-safe Floyd--Steinberg remapping for prism-quant.

This module is an independently authored clean-room implementation of the
classical Floyd--Steinberg diffusion weights described in:

    Robert W. Floyd and Louis Steinberg, "An Adaptive Algorithm for Spatial
    Grey Scale", Proceedings of the Society for Information Display 17(2),
    1976, pp. 75--77.

The alpha and region rules are original Project Prism work derived from book
chapters 05--09 and 19, not from an encoder implementation.  In particular:

* error is transported in integer premultiplied-RGBA feature space;
* alpha-zero, interior-alpha, and alpha-255 are hard assignment and transport
  zones, so diffusion cannot cross a source alpha boundary;
* alpha-zero source pixels can select only alpha-zero palette entries and
  alpha-255 pixels can select only alpha-255 entries;
* region/barrier/strength hooks are deterministic plumbing.  The default
  (v1) behavior is the unchanged T-0069 stub: one global region, no
  barriers, full strength.  The opt-in E-0010 policies fill those hooks with
  either the accepted continuous e/(e+g) strength
  (``--dither-policy adaptive``), the frozen ch19 A6 region table
  (``--dither-policy region``), the E-0014 per-unit predictor
  (``--dither-policy adaptive-unit``: one global strength B/(B+N) per unit,
  the unit-level lift of e/(e+g); an explicit ``--dither-strength`` still wins;
  T-0161 introduced the ``--adaptive-default`` switch and T-0190 later adopted
  ``guarded`` as its omission default; explicit ``off`` and ``on`` retain their
  frozen pre-flip behavior),
  or the E-0017 luma-weighted void-and-cluster blue-noise MASK dither
  (``--dither-policy luma-bluenoise``: not error diffusion at all but a seeded
  Ulichney-1993 blue-noise threshold mask with chroma-attenuated luma
  weighting; composes with ``--dither-strength`` as the mask amplitude);
* residual that cannot cross a boundary is discarded, not renormalized;
* arithmetic is integer-only and palette-distance ties choose the lowest
  palette index.

No libimagequant/pngquant source or Project Prism book chapter 12 was used.
"""

from __future__ import annotations

import hashlib
import json
import sys
from dataclasses import asdict, dataclass
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Callable, Hashable, Sequence

Pixel = tuple[int, int, int, int]
Feature = tuple[int, int, int, int]

_KERNEL_FORWARD = ((1, 0, 7), (-1, 1, 3), (0, 1, 5), (1, 1, 1))
_KERNEL_DENOMINATOR = 16


@dataclass(frozen=True)
class RegionDirective:
    """Caller-supplied future E-0010 transport evidence for one pixel.

    ``region_id`` creates a barrier when neighboring values differ.
    ``barrier`` prevents both incoming and outgoing transport. ``strength`` is
    an exact nonnegative rational multiplier applied to outgoing residual.
    """

    region_id: Hashable = "global"
    barrier: bool = False
    strength_numerator: int = 1
    strength_denominator: int = 1
    policy_version: str = "region-hook-stub-v0"


RegionHook = Callable[[int, int, Pixel], RegionDirective]

_REFERENCE_DIR = Path(__file__).resolve().parent
_PRISM_ROOT = _REFERENCE_DIR.parents[1]

# Reviewer-rerunnable scorecard inputs. The cone and squirrel paths are the
# concrete committed sources corresponding to James's named visual targets;
# the 16-level ramp is the task verification gate's explicit family probe.
SCORECARD_TARGETS = (
    (
        "synthetic-hue-ramp",
        "benchmarks/synthetic-corpus/corpus/alpha-ramp-hue-radial-64/source.png",
        "James visual target: synthetic hue ramp",
    ),
    (
        "lightmask-cone",
        "datasets/collections/kenney-packs/light-masks/Transparent/cone_a.png",
        "James visual target: Kenney transparent lightmask cone",
    ),
    (
        "squirrel-cutout",
        "datasets/pilot-v0/packages/distributable-D/"
        "open-images300-squirrel-0022bffa9abfb554-rgba.png",
        "James visual target: T-0047 reviewed Open Images squirrel cutout",
    ),
    (
        "synthetic-alpha-ramp-16",
        "benchmarks/synthetic-corpus/corpus/alpha-ramp-quadratic-v-16/source.png",
        "T-0069 verification target: 16-level gradient family",
    ),
)


@dataclass(frozen=True)
class DitherEvidence:
    algorithm: str
    scan: str
    kernel: str
    arithmetic: str
    error_space: str
    clipping: str
    boundary_rule: str
    region_policy_versions: tuple[str, ...]
    strength_pairs: tuple[tuple[int, int], ...]
    transported_edges: int
    discarded_boundary_edges: int
    changed_assignments_vs_no_dither: int

    def to_dict(self) -> dict[str, object]:
        return asdict(self)


@dataclass(frozen=True)
class DitherResult:
    indices: tuple[int, ...]
    pixels: tuple[Pixel, ...]
    evidence: DitherEvidence


def _check_pixel(value: object, where: str) -> Pixel:
    if not isinstance(value, (tuple, list)) or len(value) != 4:
        raise ValueError(f"{where}: expected an RGBA tuple")
    channels: list[int] = []
    for position, channel in enumerate(value):
        if isinstance(channel, bool) or not isinstance(channel, int):
            raise ValueError(f"{where}[{position}]: channel must be an integer")
        if channel < 0 or channel > 255:
            raise ValueError(f"{where}[{position}]: channel outside 0..255")
        channels.append(channel)
    return (channels[0], channels[1], channels[2], channels[3])


def _validate(
    pixels: Sequence[Pixel], width: int, height: int, palette: Sequence[Pixel]
) -> tuple[list[Pixel], list[Pixel]]:
    if isinstance(width, bool) or not isinstance(width, int) or width < 1:
        raise ValueError("width must be an integer >= 1")
    if isinstance(height, bool) or not isinstance(height, int) or height < 1:
        raise ValueError("height must be an integer >= 1")
    if len(pixels) != width * height:
        raise ValueError(f"expected {width * height} pixels, got {len(pixels)}")
    if not 1 <= len(palette) <= 256:
        raise ValueError("palette must contain 1..256 entries")
    return (
        [_check_pixel(pixel, f"pixel {index}") for index, pixel in enumerate(pixels)],
        [_check_pixel(entry, f"palette {index}") for index, entry in enumerate(palette)],
    )


def _feature(pixel: Pixel) -> Feature:
    red, green, blue, alpha = pixel
    return (red * alpha, green * alpha, blue * alpha, 255 * alpha)


def _alpha_zone(alpha: int) -> int:
    if alpha == 0:
        return 0
    if alpha == 255:
        return 2
    return 1


def _round_div_signed(numerator: int, denominator: int) -> int:
    """Round a signed rational to nearest, with half away from zero."""
    if denominator <= 0:
        raise ValueError("denominator must be positive")
    if numerator < 0:
        return -((-numerator + denominator // 2) // denominator)
    return (numerator + denominator // 2) // denominator


def _directive(hook: RegionHook | None, x: int, y: int, pixel: Pixel) -> RegionDirective:
    value = RegionDirective() if hook is None else hook(x, y, pixel)
    if not isinstance(value, RegionDirective):
        raise ValueError("region_hook must return RegionDirective")
    if isinstance(value.strength_numerator, bool) or not isinstance(value.strength_numerator, int):
        raise ValueError("region strength numerator must be an integer")
    if isinstance(value.strength_denominator, bool) or not isinstance(value.strength_denominator, int):
        raise ValueError("region strength denominator must be an integer")
    if value.strength_numerator < 0 or value.strength_denominator <= 0:
        raise ValueError("region strength must be a nonnegative rational")
    if not value.policy_version:
        raise ValueError("region policy_version must be nonempty")
    hash(value.region_id)
    return value


def _eligible_by_zone(palette: Sequence[Pixel]) -> dict[int, tuple[int, ...]]:
    eligible = {
        zone: tuple(index for index, entry in enumerate(palette) if _alpha_zone(entry[3]) == zone)
        for zone in (0, 1, 2)
    }
    return eligible


def _nearest_index_and_distance_sq(
    feature: Feature, palette_features: Sequence[Feature], eligible: Sequence[int]
) -> tuple[int, int]:
    if not eligible:
        raise ValueError("palette has no entry in the source pixel's alpha zone")
    f0, f1, f2, f3 = feature
    best_index = eligible[0]
    p0, p1, p2, p3 = palette_features[best_index]
    d0 = f0 - p0
    d1 = f1 - p1
    d2 = f2 - p2
    d3 = f3 - p3
    best_distance = d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
    # ``eligible`` is always ascending. Updating only for a strict distance
    # improvement therefore makes the lowest-index tie rule explicit without
    # allocating a tuple/generator for every palette comparison.
    for index in eligible[1:]:
        p0, p1, p2, p3 = palette_features[index]
        d0 = f0 - p0
        d1 = f1 - p1
        d2 = f2 - p2
        d3 = f3 - p3
        distance = d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
        if distance < best_distance:
            best_index = index
            best_distance = distance
    return best_index, best_distance


def _nearest_index(
    feature: Feature, palette_features: Sequence[Feature], eligible: Sequence[int]
) -> int:
    return _nearest_index_and_distance_sq(feature, palette_features, eligible)[0]


def nearest_remap(
    pixels: Sequence[Pixel],
    width: int,
    height: int,
    palette: Sequence[Pixel],
) -> DitherResult:
    """Direct alpha-zone-constrained nearest remap: the no-dither baseline."""
    source, entries = _validate(pixels, width, height, palette)
    palette_features = [_feature(entry) for entry in entries]
    eligible = _eligible_by_zone(entries)
    indices = tuple(
        _nearest_index(_feature(pixel), palette_features, eligible[_alpha_zone(pixel[3])])
        for pixel in source
    )
    return DitherResult(
        indices=indices,
        pixels=tuple(entries[index] for index in indices),
        evidence=DitherEvidence(
            algorithm="none",
            scan="row-major",
            kernel="none",
            arithmetic="integer-exact",
            error_space="premultiplied-rgba8-numerator",
            clipping="not-applicable",
            boundary_rule="alpha-zone assignment constraint",
            region_policy_versions=("none",),
            strength_pairs=((0, 1),),
            transported_edges=0,
            discarded_boundary_edges=0,
            changed_assignments_vs_no_dither=0,
        ),
    )


def floyd_steinberg(
    pixels: Sequence[Pixel],
    width: int,
    height: int,
    palette: Sequence[Pixel],
    *,
    serpentine: bool = True,
    region_hook: RegionHook | None = None,
) -> DitherResult:
    """Apply alpha-boundary-safe Floyd--Steinberg error diffusion.

    Residual transport uses the classical 7/16, 3/16, 5/16, 1/16 kernel,
    mirrored on reverse serpentine rows.  A transport edge is legal only when
    source alpha zones and caller region ids match and neither endpoint is a
    barrier.  Illegal residual is discarded; legal weights are not
    renormalized.  This choice protects structure at the cost of local bias.
    """
    source, entries = _validate(pixels, width, height, palette)
    source_features = [_feature(pixel) for pixel in source]
    palette_features = [_feature(entry) for entry in entries]
    eligible = _eligible_by_zone(entries)
    zones = [_alpha_zone(pixel[3]) for pixel in source]
    directives = [
        _directive(region_hook, x, y, source[y * width + x])
        for y in range(height)
        for x in range(width)
    ]
    # Four feature-coordinate residual buffers. Values remain integers in the
    # native feature scale 0..65025; each weighted contribution is rounded
    # half away from zero at the transport edge.
    residual = [[0, 0, 0, 0] for _ in source]
    indices = [-1] * len(source)
    transported_edges = 0
    discarded_edges = 0

    for y in range(height):
        reverse = serpentine and y % 2 == 1
        x_values = range(width - 1, -1, -1) if reverse else range(width)
        for x in x_values:
            position = y * width + x
            adjusted = tuple(
                min(65025, max(0, source_features[position][channel] + residual[position][channel]))
                for channel in range(4)
            )
            chosen = _nearest_index(adjusted, palette_features, eligible[zones[position]])
            indices[position] = chosen
            error = tuple(adjusted[channel] - palette_features[chosen][channel] for channel in range(4))
            directive = directives[position]
            for dx_forward, dy, weight in _KERNEL_FORWARD:
                dx = -dx_forward if reverse else dx_forward
                nx = x + dx
                ny = y + dy
                if nx < 0 or nx >= width or ny < 0 or ny >= height:
                    continue
                neighbor = ny * width + nx
                target = directives[neighbor]
                legal = (
                    zones[position] == zones[neighbor]
                    and directive.region_id == target.region_id
                    and not directive.barrier
                    and not target.barrier
                )
                if not legal:
                    discarded_edges += 1
                    continue
                denominator = _KERNEL_DENOMINATOR * directive.strength_denominator
                numerator_scale = weight * directive.strength_numerator
                for channel in range(4):
                    residual[neighbor][channel] += _round_div_signed(
                        error[channel] * numerator_scale, denominator
                    )
                transported_edges += 1

    no_dither = nearest_remap(source, width, height, entries)
    output_indices = tuple(indices)
    versions = tuple(sorted({directive.policy_version for directive in directives}))
    strengths = tuple(
        sorted({(directive.strength_numerator, directive.strength_denominator) for directive in directives})
    )
    return DitherResult(
        indices=output_indices,
        pixels=tuple(entries[index] for index in output_indices),
        evidence=DitherEvidence(
            algorithm="floyd-steinberg-1976-alpha-boundary-v0",
            scan="serpentine" if serpentine else "raster",
            kernel="7/16,3/16,5/16,1/16",
            arithmetic="signed integer; per-edge half-away-from-zero",
            error_space="premultiplied-rgba8-numerator (r*a,g*a,b*a,255*a)",
            clipping="adjusted feature coordinates clamped to 0..65025",
            boundary_rule="discard across image edge, alpha zone, region id, or barrier; no renormalization",
            region_policy_versions=versions,
            strength_pairs=strengths,
            transported_edges=transported_edges,
            discarded_boundary_edges=discarded_edges,
            changed_assignments_vs_no_dither=sum(
                left != right for left, right in zip(output_indices, no_dither.indices)
            ),
        ),
    )


# --- E-0010 region classification (book ch19 section 9, A6) ------------------
#
# Opt-in deterministic region policy behind ``--dither-policy region``.  The
# default ``uniform`` policy is the unchanged v1 behavior.  Classification is
# integer-exact, one deterministic pass plus one confluent flat-fill, and is
# derived from the source pixels and the shared unit palette only (ch19 A6
# step 1: deterministic, versioned region probabilities and barriers; the
# "probabilities" of this lab policy are hard integer-evidence labels with an
# explicit ``uncertain`` class in the table, not semantic truth).
#
# Region classes (ch19 A6 candidate classes; ch09 section 12 example policy):
#   transparent      alpha zone 0; no hidden-RGB dither (ch09 section 8.6)
#   protected-exact  source color exactly reproduced by the palette; dither
#                    can only add noise (ch19 A6 "protected region"; ch09
#                    proposal F exact-region barriers)
#   flat             every existing 4-neighbor has identical RGBA, flooded to
#                    the whole identical-color component (ch09 section 3:
#                    no dither is best for flat/exact-color regions)
#   hard-edge        alpha-zone change, or an isolated discontinuity: the
#                    largest adjacent premultiplied step >= _EDGE_STEP_MIN
#                    and >= _EDGE_STEP_RATIO x the largest step continuing
#                    on EITHER outer side of the jump (scale-free: a uniform
#                    ramp never triggers, a step edge always does; ch09
#                    section 8.2 silhouette crawl)
#   texture          sign-incoherent local activity in the premultiplied sum
#                    (ch09 section 12: source noise masks quantization)
#   gradient-opaque  coherent sub-edge activity in alpha zone 2 (ch09
#                    section 12: smooth gradient -> stronger diffusion)
#   gradient-alpha   coherent activity in alpha zone 1, alpha >=
#                    _SHADOW_ALPHA_MAX (broad alpha gradient)
#   soft-shadow      coherent activity in alpha zone 1, alpha <
#                    _SHADOW_ALPHA_MAX (ch09 section 8.4 shadow breakup;
#                    section 12: soft shadow -> alpha-safe adaptive dither)
#   uncertain        table entry for future classifiers; the v1 decision tree
#                    is total and never emits it (conservative no-dither)
#
# Frozen policy table (ch19 A6 steps 2-4): two region ids.  ``none`` pixels
# never dither and are isolated from residual by the region-id transport
# rule; ``dither`` pixels share one id so error transports smoothly across
# class boundaries inside the dithered family, scaled by the SENDER's
# strength (the declared transition rule; ch09 section 12 requires smooth
# transitions).  Barriers realize ch19 A6 step 4 (no diffusion across
# protected and strong alpha boundaries); the alpha-zone rule independently
# blocks all zone-mismatched transport.

REGION_POLICY_VERSION = "e0010-region-policy-v1"

_EDGE_STEP_MIN = 1020  # 4 premultiplied byte steps at full alpha (4 * 255)
_EDGE_STEP_RATIO = 4
_SHADOW_ALPHA_MAX = 64

REGION_CLASSES = (
    "transparent",
    "protected-exact",
    "flat",
    "hard-edge",
    "texture",
    "gradient-opaque",
    "gradient-alpha",
    "soft-shadow",
    "uncertain",
)

# class -> (region_id, barrier, strength_numerator, strength_denominator)
REGION_CLASS_TABLE: dict[str, tuple[str, bool, int, int]] = {
    "transparent": ("none", False, 0, 1),
    "protected-exact": ("none", True, 0, 1),
    "flat": ("none", False, 0, 1),
    "hard-edge": ("none", True, 0, 1),
    "texture": ("none", False, 0, 1),
    "gradient-opaque": ("dither", False, 1, 1),
    "gradient-alpha": ("dither", False, 3, 4),
    "soft-shadow": ("dither", False, 1, 2),
    "uncertain": ("none", False, 0, 1),
}

_NEIGHBOR_DELTAS = ((-1, 0), (1, 0), (0, -1), (0, 1))


# Lifted without policy changes from the accepted E-0010 V3 experiment arm:
# experiments/E-0010-region-conditioned-dithering/code/variants.py
# (implementation 141430f3; measured evidence 9ab7eb50).  Keeping the exact
# integer ratio here makes the measured policy a first-class lab seam while
# leaving it opt-in; the v0.1/default no-dither path is unchanged (T-0094).
ADAPTIVE_POLICY_VERSION = "e0010-adaptive-eg-v1"


def _squared_local_gradient(
    features: Sequence[Feature], width: int, height: int
) -> list[int]:
    """Return each pixel's largest squared feature step to a 4-neighbor."""
    gradient = [0] * len(features)
    for y in range(height):
        for x in range(width):
            position = y * width + x
            feature = features[position]
            best = 0
            for dx, dy in _NEIGHBOR_DELTAS:
                nx = x + dx
                ny = y + dy
                if 0 <= nx < width and 0 <= ny < height:
                    other = features[ny * width + nx]
                    distance = sum(
                        (feature[channel] - other[channel]) ** 2
                        for channel in range(4)
                    )
                    if distance > best:
                        best = distance
            gradient[position] = best
    return gradient


def adaptive_strength_hook(
    pixels: Sequence[Pixel], width: int, height: int, palette: Sequence[Pixel]
) -> RegionHook:
    """Build E-0010's global continuous strength policy e/(e+g).

    ``e`` is nearest-entry squared premultiplied error and ``g`` is the
    largest squared premultiplied step to an existing 4-neighbor.  Exact
    matches use 0/1; every other pixel retains the unreduced exact ratio from
    accepted experiment arm V3.  This is the frozen E-0010 instantiation of
    ch09 section 13, not a fitted model or a new default.
    """
    source, entries = _validate(pixels, width, height, palette)
    features = [_feature(pixel) for pixel in source]
    palette_features = [_feature(entry) for entry in entries]
    eligible = _eligible_by_zone(entries)
    errors = [
        _nearest_index_and_distance_sq(
            feature, palette_features, eligible[_alpha_zone(pixel[3])]
        )[1]
        for pixel, feature in zip(source, features)
    ]
    gradient = _squared_local_gradient(features, width, height)
    directives = []
    for error, local_gradient in zip(errors, gradient):
        total = error + local_gradient
        numerator, denominator = (0, 1) if total == 0 else (error, total)
        directives.append(
            RegionDirective(
                strength_numerator=numerator,
                strength_denominator=denominator,
                policy_version=ADAPTIVE_POLICY_VERSION,
            )
        )
    table = tuple(directives)

    def hook(x: int, y: int, _pixel: Pixel) -> RegionDirective:
        return table[y * width + x]

    return hook


def classify_regions(
    pixels: Sequence[Pixel], width: int, height: int, palette: Sequence[Pixel]
) -> tuple[str, ...]:
    """Classify every pixel into a frozen ch19 A6 region class.

    Deterministic and integer-exact; the result depends only on the source
    pixels, geometry, and the shared palette.  Returns one class label per
    pixel, row-major; every label is a member of ``REGION_CLASSES``.
    """
    source, entries = _validate(pixels, width, height, palette)
    palette_features = [_feature(entry) for entry in entries]
    eligible = _eligible_by_zone(entries)
    features = [_feature(pixel) for pixel in source]
    zones = [_alpha_zone(pixel[3]) for pixel in source]
    count = len(source)
    classes: list[str | None] = [None] * count

    # Pass 1: transparent support and palette-exact (protected) pixels.
    for position in range(count):
        if zones[position] == 0:
            classes[position] = "transparent"
            continue
        _, distance = _nearest_index_and_distance_sq(
            features[position], palette_features, eligible[zones[position]]
        )
        if distance == 0:
            classes[position] = "protected-exact"

    # Pass 2: flat seeds (all existing 4-neighbors identical), then flood the
    # whole identical-color component.  The fixed point is confluent: the
    # resulting set is independent of the fill order.
    stack: list[int] = []
    for y in range(height):
        for x in range(width):
            position = y * width + x
            if classes[position] is not None:
                continue
            pixel = source[position]
            if all(
                not (0 <= x + dx < width and 0 <= y + dy < height)
                or source[(y + dy) * width + (x + dx)] == pixel
                for dx, dy in _NEIGHBOR_DELTAS
            ):
                classes[position] = "flat"
                stack.append(position)
    while stack:
        position = stack.pop()
        x = position % width
        y = position // width
        for dx, dy in _NEIGHBOR_DELTAS:
            nx = x + dx
            ny = y + dy
            if 0 <= nx < width and 0 <= ny < height:
                neighbor = ny * width + nx
                if classes[neighbor] is None and source[neighbor] == source[position]:
                    classes[neighbor] = "flat"
                    stack.append(neighbor)

    # Pass 3: hard edges, texture, and the gradient/shadow family.
    for y in range(height):
        for x in range(width):
            position = y * width + x
            if classes[position] is not None:
                continue
            feature = features[position]
            largest = 0
            largest_neighbor = -1
            zone_boundary = False
            for dx, dy in _NEIGHBOR_DELTAS:
                nx = x + dx
                ny = y + dy
                if not (0 <= nx < width and 0 <= ny < height):
                    continue
                neighbor = ny * width + nx
                if zones[neighbor] != zones[position]:
                    zone_boundary = True
                other = features[neighbor]
                step = max(
                    abs(feature[0] - other[0]),
                    abs(feature[1] - other[1]),
                    abs(feature[2] - other[2]),
                    abs(feature[3] - other[3]),
                )
                if step > largest:
                    # Strict improvement: the first argmax in _NEIGHBOR_DELTAS
                    # order wins ties, keeping the continuation probe stable.
                    largest = step
                    largest_neighbor = neighbor
            if zone_boundary:
                classes[position] = "hard-edge"
                continue
            if largest >= _EDGE_STEP_MIN:
                # Isolated-discontinuity test: an edge step is large AND the
                # neighborhood on BOTH remaining sides is calm — the largest
                # step continuing past this pixel (excluding the argmax
                # neighbor) or past that neighbor (excluding this pixel).
                # A uniform ramp fails this test (the step continues at the
                # same magnitude), so smooth-but-steep gradients are never
                # edges; a true step edge is calm on both outer sides.
                continuation = 0
                for dx, dy in _NEIGHBOR_DELTAS:
                    nx = x + dx
                    ny = y + dy
                    if not (0 <= nx < width and 0 <= ny < height):
                        continue
                    neighbor = ny * width + nx
                    if neighbor == largest_neighbor:
                        continue
                    other = features[neighbor]
                    step = max(
                        abs(feature[0] - other[0]),
                        abs(feature[1] - other[1]),
                        abs(feature[2] - other[2]),
                        abs(feature[3] - other[3]),
                    )
                    if step > continuation:
                        continuation = step
                qx = largest_neighbor % width
                qy = largest_neighbor // width
                q_feature = features[largest_neighbor]
                for dx, dy in _NEIGHBOR_DELTAS:
                    nx = qx + dx
                    ny = qy + dy
                    if not (0 <= nx < width and 0 <= ny < height):
                        continue
                    neighbor = ny * width + nx
                    if neighbor == position:
                        continue
                    other = features[neighbor]
                    step = max(
                        abs(q_feature[0] - other[0]),
                        abs(q_feature[1] - other[1]),
                        abs(q_feature[2] - other[2]),
                        abs(q_feature[3] - other[3]),
                    )
                    if step > continuation:
                        continuation = step
                if continuation * _EDGE_STEP_RATIO <= largest:
                    classes[position] = "hard-edge"
                    continue
            # Texture: sign-incoherent local activity of the premultiplied
            # channel sum in the nearest valid 2x2 window (anchored at the
            # pixel, clamped one cell in from the right/bottom border so
            # border pixels reuse their adjacent valid window).
            incoherent = False
            if width >= 2 and height >= 2:
                anchor_x = min(x, width - 2)
                anchor_y = min(y, height - 2)
                base = anchor_y * width + anchor_x
                here = features[base]
                here = here[0] + here[1] + here[2]
                right = features[base + 1]
                right = right[0] + right[1] + right[2]
                below = features[base + width]
                below = below[0] + below[1] + below[2]
                diagonal = features[base + width + 1]
                diagonal = diagonal[0] + diagonal[1] + diagonal[2]
                dx_top = right - here
                dx_bottom = diagonal - below
                dy_left = below - here
                dy_right = diagonal - right
                if (dx_top != 0 and dx_bottom != 0 and (dx_top < 0) != (dx_bottom < 0)) or (
                    dy_left != 0 and dy_right != 0 and (dy_left < 0) != (dy_right < 0)
                ):
                    incoherent = True
            if incoherent:
                classes[position] = "texture"
            elif zones[position] == 2:
                classes[position] = "gradient-opaque"
            elif source[position][3] < _SHADOW_ALPHA_MAX:
                classes[position] = "soft-shadow"
            else:
                classes[position] = "gradient-alpha"
    return tuple(classes)  # type: ignore[return-value]


# --- E-0014 per-unit adaptive strength (T-0161/T-0190 default lineage) -------
#
# The unit-level lift of E-0010's per-pixel e/(e+g): a single global
# Floyd--Steinberg strength for the whole unit, ``B / (B + N)``, where B is the
# want-diffusion pixel mass and N is the resist-diffusion pixel mass, both read
# off the SAME frozen E-0010 region classifier (classify_regions,
# e0010-region-policy-v1).  No new classifier and no new thresholds are
# introduced; the 3-way partition below is DERIVED from the sign of each class's
# frozen REGION_CLASS_TABLE strength (>0 -> banding/want; ==0 and
# non-barrier/non-transparent/non-protected -> grain/resist; else neutral).
# Frozen a priori in experiments/E-0014-per-unit-dither-strength/plan-v1.json
# (T-0107) before any deck measurement; validated held-out on the T-0071 deck.
# Exposed explicitly through T-0161's default-candidate switch. T-0190 adopted
# the guarded policy as the omission default; explicit ``off`` remains the
# frozen no-dither behavior and explicit ``on`` the frozen unguarded behavior.

ADAPTIVE_UNIT_POLICY_VERSION = "e0014-adaptive-unit-v1"

# Classes whose frozen REGION_CLASS_TABLE strength is > 0 (1/1, 3/4, 1/2):
# smooth, bandable regions where diffusion helps.
_ADAPTIVE_UNIT_BANDING_CLASSES = ("gradient-opaque", "gradient-alpha", "soft-shadow")
# Non-transparent, non-protected, non-edge classes the frozen table nonetheless
# leaves at strength 0: texture (high-frequency activity that masks banding and
# where added grain clashes) and flat (exact/uniform regions where dither only
# adds visible grain).
_ADAPTIVE_UNIT_GRAIN_CLASSES = ("texture", "flat")


def _unit_strength_from_classes(classes: Sequence[str]) -> tuple[int, int]:
    """Return the reduced B/(B+N) per-unit strength from a class map."""
    counts = region_class_counts(classes)
    banding = sum(counts[label] for label in _ADAPTIVE_UNIT_BANDING_CLASSES)
    total = banding + sum(counts[label] for label in _ADAPTIVE_UNIT_GRAIN_CLASSES)
    if total == 0:
        return (0, 1)
    from math import gcd

    divisor = gcd(banding, total)
    return (banding // divisor, total // divisor)


def predict_unit_strength(
    pixels: Sequence[Pixel], width: int, height: int, palette: Sequence[Pixel]
) -> tuple[int, int]:
    """Predict a single global dither strength for the unit as reduced B/(B+N).

    Deterministic and exact.  ``B`` is the frozen E-0010 gradient-family pixel
    mass (want diffusion) and ``B + N`` adds the texture+flat mass (resist
    diffusion); transparent, palette-exact, and hard-edge pixels are neutral.
    Palette-conditioned exactly like ``--dither-policy region`` (a gradient the
    palette reproduces exactly does not band and correctly predicts 0).
    """
    classes = classify_regions(pixels, width, height, palette)
    return _unit_strength_from_classes(classes)


def region_hook_from_classes(classes: Sequence[str], width: int) -> RegionHook:
    """Build the frozen-table directive hook for a classified region map."""
    if len(classes) % width != 0:
        raise ValueError("class map length must be a multiple of width")
    directives: list[RegionDirective] = []
    for label in classes:
        if label not in REGION_CLASS_TABLE:
            raise ValueError(f"unknown region class: {label}")
        region_id, barrier, numerator, denominator = REGION_CLASS_TABLE[label]
        directives.append(
            RegionDirective(
                region_id=region_id,
                barrier=barrier,
                strength_numerator=numerator,
                strength_denominator=denominator,
                policy_version=REGION_POLICY_VERSION,
            )
        )
    table = tuple(directives)

    def hook(x: int, y: int, _pixel: Pixel) -> RegionDirective:
        return table[y * width + x]

    return hook


def region_class_counts(classes: Sequence[str]) -> dict[str, int]:
    """Deterministic per-class pixel counts, keyed in REGION_CLASSES order."""
    counts = {label: 0 for label in REGION_CLASSES}
    for label in classes:
        counts[label] += 1
    return counts


def region_policy_hook(
    pixels: Sequence[Pixel], width: int, height: int, palette: Sequence[Pixel]
) -> tuple[RegionHook, tuple[str, ...]]:
    """Classify the source and return (hook, class map) for the region path."""
    classes = classify_regions(pixels, width, height, palette)
    return region_hook_from_classes(classes, width), classes


class DitherCliError(Exception):
    """A stable CLI failure carrying the declared process status."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status


# --- E-0017 luma-weighted blue-noise mask dither -----------------------------
#
# Opt-in ``--dither-policy luma-bluenoise`` (T-0139, James-approved promotion of
# the accepted E-0017 arm).  This is NOT error diffusion: it is a void-and-
# cluster blue-noise THRESHOLD MASK dither (Ulichney 1993) with luma-weighted
# chroma attenuation.  On the round-2 gate-failing units E-0017 measured roughly
# HALF the E-0013 grain proxy at matched banding versus Floyd--Steinberg (dice:
# FS grain 110.3 -> luma 57.9 at banding ~0.5), ~byte-neutral on photographs and
# about half blue-noise's DEFLATE penalty elsewhere.  Opt-in only; never a
# default (a separate approved lane, T-0138/E-0020, studies defaults).
#
# The three seeded 64x64 masks are E-0017's committed, sha256-pinned artifacts
# (experiments/E-0017-bluenoise-dither/masks/), produced by that experiment's
# clean-room void-and-cluster generator from the Ulichney 1993 primary source --
# no third-party dither/halftone code (E-0017 plan-v1 primary_source_ledger).
# The luma-weighting remap below is a faithful, numpy-free pure-Python port of
# that experiment's dither_arms.bluenoise_remap(mode="luma"); it reuses this
# module's own alpha-zone / premultiplied-feature / nearest helpers, so its
# indices are bit-identical to the experiment arm on a shared palette
# (cross-checked in the promotion evidence, promotion/equivalence_check.py).

LUMA_BLUENOISE_POLICY_VERSION = "e0017-luma-bluenoise-v1"

_BLUENOISE_MASK_SIZE = 64
_BLUENOISE_LUMA_WEIGHT = 0.75
_BLUENOISE_CHROMA_WEIGHT = 0.25
_BLUENOISE_MASK_DIR = (
    _PRISM_ROOT / "experiments" / "E-0017-bluenoise-dither" / "masks"
)
# Frozen E-0017 masks-manifest.json: (channel, seed, filename, file sha256).
# The load is self-verifying against these constants, so a corrupted or
# substituted mask fails loudly rather than silently changing the dither.
_BLUENOISE_MASKS = (
    ("r", 20260719, "bluenoise-64-seed20260719-r.json",
     "8ee801878fd37cc52fbb2993fa4d7c5b4ace02f2fccc04a0c28dabf13111b0d8"),
    ("g", 20260720, "bluenoise-64-seed20260720-g.json",
     "80aba5e8dc5cbef7b1c04acfc3e3b0d6193375a74ef007cf8a26d604ae2522cc"),
    ("b", 20260721, "bluenoise-64-seed20260721-b.json",
     "cb2706b65c956f52369fd05ccb0c73fef52774c185cb39fffa7ff8dc79258139"),
)

_bluenoise_mask_cache: dict[str, tuple[int, ...]] | None = None


def _load_bluenoise_masks() -> dict[str, tuple[int, ...]]:
    """Load E-0017's three committed void-and-cluster masks (memoized).

    Each mask file is sha256-verified against the frozen E-0017 hash before use.
    Returns the row-major rank permutation per channel (length 64*64), each a
    strict permutation of 0..4095.
    """
    global _bluenoise_mask_cache
    if _bluenoise_mask_cache is not None:
        return _bluenoise_mask_cache
    total = _BLUENOISE_MASK_SIZE * _BLUENOISE_MASK_SIZE
    masks: dict[str, tuple[int, ...]] = {}
    for channel, seed, filename, expected_sha in _BLUENOISE_MASKS:
        path = _BLUENOISE_MASK_DIR / filename
        try:
            raw = path.read_bytes()
        except OSError as error:
            raise DitherCliError(
                5, f"io_error: cannot read E-0017 blue-noise mask {path}: {error}"
            ) from error
        actual_sha = hashlib.sha256(raw).hexdigest()
        if actual_sha != expected_sha:
            raise DitherCliError(
                70,
                f"internal: E-0017 mask {filename} sha256 {actual_sha} != "
                f"frozen {expected_sha}",
            )
        record = json.loads(raw)
        ranks = record.get("ranks_row_major")
        if record.get("seed") != seed or not isinstance(ranks, list) or len(ranks) != total:
            raise DitherCliError(
                70, f"internal: E-0017 mask {filename} shape/seed mismatch"
            )
        masks[channel] = tuple(int(v) for v in ranks)
    _bluenoise_mask_cache = masks
    return masks


def _amplitude_a0(colors: int) -> float:
    """Blue-noise amplitude A0 = 255 / colors**(1/3) (E-0017 arms b/c)."""
    return 255.0 / (colors ** (1.0 / 3.0))


def luma_bluenoise_remap(
    pixels: Sequence[Pixel],
    width: int,
    height: int,
    palette: Sequence[Pixel],
    *,
    colors: int,
    strength: tuple[int, int] = (1, 1),
) -> DitherResult:
    """Luma-weighted void-and-cluster blue-noise mask dither (E-0017 arm c).

    Per pixel/channel k, full noise n_k = (m_k - 0.5) * A0 with independent
    R/G/B masks (m_k = (rank + 0.5) / 4096).  Luma-weighting attenuates the
    chroma component:  delta_k = s * (0.25*n_k + 0.75*nL), nL=(n_r+n_g+n_b)/3.
    The adjusted premultiplied feature is round-half-to-even((pixel_k+delta_k)
    * alpha) clamped to 0..65025; the alpha feature (255*a) is never perturbed;
    the alpha-zone nearest lookup and lowest-index tie rule match the FS path.
    A mask dither transports no error, so no residual crosses any boundary.
    """
    source, entries = _validate(pixels, width, height, palette)
    if colors < 1 or colors > 256:
        raise ValueError("colors must be in 1..256")
    strength_num, strength_den = strength
    if strength_den <= 0 or strength_num < 0:
        raise ValueError("strength must be a nonnegative rational")
    palette_features = [_feature(entry) for entry in entries]
    eligible = _eligible_by_zone(entries)
    masks = _load_bluenoise_masks()
    mask_r, mask_g, mask_b = masks["r"], masks["g"], masks["b"]
    size = _BLUENOISE_MASK_SIZE
    total = size * size
    a0 = _amplitude_a0(colors)
    scale = strength_num / strength_den
    chroma_w = _BLUENOISE_CHROMA_WEIGHT
    luma_w = _BLUENOISE_LUMA_WEIGHT
    indices = [-1] * len(source)
    for y in range(height):
        row = (y % size) * size
        for x in range(width):
            position = y * width + x
            red, green, blue, alpha = source[position]
            tile = row + (x % size)
            n_r = ((mask_r[tile] + 0.5) / total - 0.5) * a0
            n_g = ((mask_g[tile] + 0.5) / total - 0.5) * a0
            n_b = ((mask_b[tile] + 0.5) / total - 0.5) * a0
            n_l = (n_r + n_g + n_b) / 3.0
            delta_r = scale * (chroma_w * n_r + luma_w * n_l)
            delta_g = scale * (chroma_w * n_g + luma_w * n_l)
            delta_b = scale * (chroma_w * n_b + luma_w * n_l)
            alpha_f = float(alpha)
            adjusted = (
                min(65025, max(0, round((red + delta_r) * alpha_f))),
                min(65025, max(0, round((green + delta_g) * alpha_f))),
                min(65025, max(0, round((blue + delta_b) * alpha_f))),
                255 * alpha,
            )
            indices[position] = _nearest_index(
                adjusted, palette_features, eligible[_alpha_zone(alpha)]
            )
    output_indices = tuple(indices)
    no_dither = nearest_remap(source, width, height, entries)
    return DitherResult(
        indices=output_indices,
        pixels=tuple(entries[index] for index in output_indices),
        evidence=DitherEvidence(
            algorithm="ulichney-1993-void-and-cluster-luma-weighted-v0",
            scan="mask-threshold",
            kernel="void-and-cluster-64 (Ulichney 1993); luma 0.75 / chroma 0.25",
            arithmetic="float64 amplitude; round-half-to-even premultiplied feature",
            error_space="premultiplied-rgba8-numerator (r*a,g*a,b*a,255*a)",
            clipping="adjusted feature coordinates clamped to 0..65025",
            boundary_rule=(
                "mask dither; no error transport; alpha-zone assignment constraint"
            ),
            region_policy_versions=(LUMA_BLUENOISE_POLICY_VERSION,),
            strength_pairs=((strength_num, strength_den),),
            transported_edges=0,
            discarded_boundary_edges=0,
            changed_assignments_vs_no_dither=sum(
                left != right for left, right in zip(output_indices, no_dither.indices)
            ),
        ),
    )


def _external_metric_observations(source_path: Path, output_path: Path) -> dict[str, object]:
    """Call the existing M1 metric-tool adapter; never fork its subprocess brain."""
    import m1_run

    observations: dict[str, object] = {}
    for name, binary in (
        ("ssimulacra2", m1_run.SSIMULACRA2_BINARY),
        ("butteraugli", m1_run.BUTTERAUGLI_BINARY),
    ):
        if not binary.is_file():
            observations[name] = {"status": "skipped", "reason": f"missing binary: {binary}"}
            continue
        try:
            observations[name] = {
                "status": "measured",
                "raw": m1_run._score_metric_tool(binary, source_path, output_path),
            }
        except m1_run.M1RunError as error:
            observations[name] = {"status": "skipped", "reason": str(error)}
    return observations


def _parse_dither_strength(value: str) -> tuple[int, int]:
    """Parse a finite decimal strength exactly and return its reduced ratio."""
    try:
        strength = Decimal(value)
    except InvalidOperation as error:
        raise DitherCliError(
            2, "usage_error: --dither-strength must be a decimal in 0..1"
        ) from error
    if not strength.is_finite() or strength < 0 or strength > 1:
        raise DitherCliError(
            2, "usage_error: --dither-strength must be a decimal in 0..1"
        )
    return strength.as_integer_ratio()


def _uniform_strength_hook(
    strength: tuple[int, int],
) -> RegionHook:
    numerator, denominator = strength

    def hook(_x: int, _y: int, _pixel: Pixel) -> RegionDirective:
        return RegionDirective(
            strength_numerator=numerator,
            strength_denominator=denominator,
            policy_version="cli-uniform-strength-v0",
        )

    return hook


def build_candidate(
    source_path: Path,
    *,
    colors: int,
    adaptive_default: bool = False,
    dither: bool,
    dither_strength: tuple[int, int] = (1, 1),
    dither_strength_explicit: bool = False,
    dither_policy: str = "uniform",
    pack_mode: str,
    zopflipng_path: Path | None = None,
) -> tuple[bytes, dict[str, object]]:
    """Run the real core palette, this remap stage, packing, and M1 metrics."""
    import m1_metrics
    import m1_png
    import prism_pack
    import prism_quant

    if adaptive_default:
        if (
            dither is not True
            or dither_strength != (1, 1)
            or dither_strength_explicit
            or dither_policy != "uniform"
        ):
            raise DitherCliError(
                2,
                "usage_error: --adaptive-default on is not composable with "
                "explicit dither options",
            )
        dither_policy = "adaptive-unit"
    if colors < 1 or colors > 256:
        raise DitherCliError(3, "data_error: --colors must be in 1..256")
    if dither_policy not in (
        "uniform", "adaptive", "region", "adaptive-unit", "luma-bluenoise"
    ):
        raise DitherCliError(
            2,
            "usage_error: --dither-policy must be uniform, adaptive, region, "
            "adaptive-unit, or luma-bluenoise",
        )
    if dither_policy in ("adaptive", "region", "adaptive-unit", "luma-bluenoise"):
        if not dither:
            raise DitherCliError(
                2, f"usage_error: --dither-policy {dither_policy} requires --dither on"
            )
    if dither_policy in ("adaptive", "region"):
        if dither_strength != (1, 1):
            raise DitherCliError(
                2,
                "usage_error: --dither-strength is not composable with "
                f"--dither-policy {dither_policy} (policy supplies exact strengths)",
            )
    region_classes: tuple[str, ...] | None = None
    adaptive_unit_strength: tuple[int, int] | None = None
    adaptive_unit_classes: tuple[str, ...] | None = None
    try:
        source_bytes = source_path.read_bytes()
    except OSError as error:
        raise DitherCliError(5, f"io_error: cannot read {source_path}: {error}") from error
    try:
        source = m1_png.decode_png(source_bytes)
        _core_output, palette, core_notes = prism_quant.quantize_image(source, colors)
        if dither and dither_policy == "luma-bluenoise":
            # E-0017 promotion (opt-in): void-and-cluster blue-noise MASK dither
            # with luma-weighting.  Not error diffusion -- bypasses the FS
            # region-hook path entirely; composes with --dither-strength as the
            # mask amplitude (default full).
            remap = luma_bluenoise_remap(
                source.pixels,
                source.width,
                source.height,
                palette,
                colors=colors,
                strength=dither_strength,
            )
        elif dither:
            if dither_policy == "region":
                # The opt-in E-0010 seam fill: classify once against the shared
                # palette, then feed the frozen-table directives through the
                # pre-existing deterministic region hook.
                region_hook, region_classes = region_policy_hook(
                    source.pixels, source.width, source.height, palette
                )
            elif dither_policy == "adaptive":
                region_hook = adaptive_strength_hook(
                    source.pixels, source.width, source.height, palette
                )
            elif dither_policy == "adaptive-unit":
                # E-0014: the per-unit predictor sets the default global
                # strength; an explicit --dither-strength still WINS.  The
                # predicted (or explicit) per-unit-constant strength is applied
                # through the SAME uniform-strength hook as the uniform path, so
                # adaptive-unit is byte-identical to uniform at that strength.
                if dither_strength_explicit:
                    adaptive_unit_strength = dither_strength
                else:
                    adaptive_unit_classes = classify_regions(
                        source.pixels, source.width, source.height, palette
                    )
                    adaptive_unit_strength = _unit_strength_from_classes(
                        adaptive_unit_classes
                    )
                region_hook = (
                    None
                    if adaptive_unit_strength == (1, 1)
                    else _uniform_strength_hook(adaptive_unit_strength)
                )
            else:
                # Keep the historical full-strength path byte-for-byte untouched.
                # Other strengths use only the pre-existing deterministic region hook.
                region_hook = (
                    None
                    if dither_strength == (1, 1)
                    else _uniform_strength_hook(dither_strength)
                )
            remap = floyd_steinberg(
                source.pixels,
                source.width,
                source.height,
                palette,
                region_hook=region_hook,
            )
        else:
            remap = nearest_remap(
                source.pixels, source.width, source.height, palette
            )
        packed = prism_pack.pack_indexed_png(
            source.width,
            source.height,
            palette,
            remap.indices,
            mode=pack_mode,
            zopflipng_path=zopflipng_path,
        )
        observed = m1_png.decode_png(packed.data)
    except prism_quant.PrismQuantError as error:
        status = 70 if str(error).startswith("internal") else 3
        label = "internal" if status == 70 else "data_error"
        raise DitherCliError(status, f"{label}: {error}") from error
    except (m1_png.PngError, prism_pack.PackError, ValueError) as error:
        raise DitherCliError(3, f"data_error: {error}") from error
    if observed.pixels != remap.pixels:
        raise DitherCliError(70, "internal: packed output differs from remap candidate")
    metrics = m1_metrics.compute_metrics(source.pixels, observed.pixels, width=source.width)
    transparent_source_count = sum(pixel[3] == 0 for pixel in source.pixels)
    opaque_source_count = sum(pixel[3] == 255 for pixel in source.pixels)
    transparent_output_mismatch_count = sum(
        source_pixel[3] == 0 and output_pixel[3] != 0
        for source_pixel, output_pixel in zip(source.pixels, observed.pixels)
    )
    opaque_output_mismatch_count = sum(
        source_pixel[3] == 255 and output_pixel[3] != 255
        for source_pixel, output_pixel in zip(source.pixels, observed.pixels)
    )
    pack_evidence = {
        "cleanup": asdict(packed.cleanup),
        "selected_pre_optimizer": asdict(packed.selected_pre_optimizer),
        "pre_optimizer_portfolio": [asdict(item) for item in packed.pre_optimizer_portfolio],
        "optimizer": asdict(packed.optimizer),
    }
    try:
        evidence_source_path = str(source_path.relative_to(_PRISM_ROOT))
    except ValueError:
        evidence_source_path = str(source_path)
    evidence: dict[str, object] = {
        "schema_version": "prism.lab.prism-dither-candidate/0",
        "evidence_label": "pilot scorecard; not a quality/default claim",
        "source_path": evidence_source_path,
        "source_sha256": hashlib.sha256(source_bytes).hexdigest(),
        "source_bytes": len(source_bytes),
        "width": source.width,
        "height": source.height,
        "colors": colors,
        "quantizer_version": prism_quant.VERSION,
        "quantizer_stage_notes": asdict(core_notes),
        "adaptive_default": adaptive_default,
        "dither_enabled": dither,
        "dither_strength": {
            "numerator": dither_strength[0],
            "denominator": dither_strength[1],
        },
        "dither": remap.evidence.to_dict(),
        "pack_mode": pack_mode,
        "pack": pack_evidence,
        "output_sha256": hashlib.sha256(packed.data).hexdigest(),
        "output_bytes": len(packed.data),
        "metrics": metrics,
        "structural_checks": {
            "support_unchanged": metrics["support_xor_count"] == 0,
            "no_new_speckles": metrics["new_speckle_count"] == 0,
            "component_count_unchanged": (
                metrics["reference_component_count"] == metrics["candidate_component_count"]
            ),
            "silhouette_iou_exact": metrics["silhouette_iou"] == {"numerator": 1, "denominator": 1},
            "transparent_source_count": transparent_source_count,
            "transparent_output_mismatch_count": transparent_output_mismatch_count,
            "opaque_source_count": opaque_source_count,
            "opaque_output_mismatch_count": opaque_output_mismatch_count,
        },
    }
    if region_classes is not None:
        # Region-path evidence is additive-only so the v1/uniform evidence
        # shape (and the T-0080 sweep's byte-identity) is preserved exactly.
        evidence["region"] = {
            "dither_policy": dither_policy,
            "policy_version": REGION_POLICY_VERSION,
            "class_counts": region_class_counts(region_classes),
            "classes_sha256": hashlib.sha256(
                ",".join(region_classes).encode("utf-8")
            ).hexdigest(),
            "edge_step_min": _EDGE_STEP_MIN,
            "edge_step_ratio": _EDGE_STEP_RATIO,
            "shadow_alpha_max": _SHADOW_ALPHA_MAX,
        }
    elif dither and dither_policy == "adaptive":
        evidence["adaptive"] = {
            "dither_policy": dither_policy,
            "policy_version": ADAPTIVE_POLICY_VERSION,
            "formula": "e/(e+g)",
        }
    elif dither and dither_policy == "adaptive-unit" and adaptive_unit_strength is not None:
        # Additive-only, mirroring the region/adaptive evidence seams so the
        # v1/uniform evidence shape stays byte-stable.
        adaptive_unit_evidence: dict[str, object] = {
            "dither_policy": dither_policy,
            "policy_version": ADAPTIVE_UNIT_POLICY_VERSION,
            "formula": "B/(B+N)",
            "strength": {
                "numerator": adaptive_unit_strength[0],
                "denominator": adaptive_unit_strength[1],
            },
            "explicit_strength_override": bool(dither_strength_explicit),
        }
        if adaptive_unit_classes is not None:
            counts = region_class_counts(adaptive_unit_classes)
            adaptive_unit_evidence["banding_mass_B"] = sum(
                counts[label] for label in _ADAPTIVE_UNIT_BANDING_CLASSES
            )
            adaptive_unit_evidence["grain_mass_N"] = sum(
                counts[label] for label in _ADAPTIVE_UNIT_GRAIN_CLASSES
            )
            adaptive_unit_evidence["class_counts"] = counts
            adaptive_unit_evidence["region_policy_version"] = REGION_POLICY_VERSION
        evidence["adaptive_unit"] = adaptive_unit_evidence
    elif dither and dither_policy == "luma-bluenoise":
        # Additive-only (E-0017 promotion), mirroring the region/adaptive seams
        # so the v1/uniform evidence shape stays byte-stable.
        evidence["luma_bluenoise"] = {
            "dither_policy": dither_policy,
            "policy_version": LUMA_BLUENOISE_POLICY_VERSION,
            "algorithm": "ulichney-1993-void-and-cluster-luma-weighted",
            "primary_source": (
                "Robert Ulichney, The Void-and-Cluster Method for Dither Array "
                "Generation, Proc. SPIE 1913, 1993"
            ),
            "mask_size": _BLUENOISE_MASK_SIZE,
            "mask_seeds": {ch: seed for ch, seed, _f, _s in _BLUENOISE_MASKS},
            "amplitude_a0": _amplitude_a0(colors),
            "luma_weight": _BLUENOISE_LUMA_WEIGHT,
            "chroma_weight": _BLUENOISE_CHROMA_WEIGHT,
            "strength": {
                "numerator": dither_strength[0],
                "denominator": dither_strength[1],
            },
        }
    return packed.data, evidence


def write_candidate(
    source_path: Path,
    output_path: Path,
    *,
    colors: int,
    adaptive_default: bool = False,
    dither: bool,
    dither_strength: tuple[int, int] = (1, 1),
    dither_strength_explicit: bool = False,
    dither_policy: str = "uniform",
    pack_mode: str,
    zopflipng_path: Path | None = None,
    evidence_path: Path | None = None,
    measure_external: bool = False,
) -> dict[str, object]:
    data, evidence = build_candidate(
        source_path,
        colors=colors,
        adaptive_default=adaptive_default,
        dither=dither,
        dither_strength=dither_strength,
        dither_strength_explicit=dither_strength_explicit,
        dither_policy=dither_policy,
        pack_mode=pack_mode,
        zopflipng_path=zopflipng_path,
    )
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_bytes(data)
        if measure_external:
            evidence["external_metrics"] = _external_metric_observations(
                source_path, output_path
            )
        if evidence_path is not None:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(evidence, sort_keys=True, separators=(",", ":")) + "\n",
                encoding="utf-8",
            )
    except OSError as error:
        raise DitherCliError(5, f"io_error: cannot write output: {error}") from error
    return evidence


def generate_scorecard(
    output_dir: Path,
    *,
    colors: int,
    dither_strength: tuple[int, int] = (1, 1),
    pack_mode: str,
    zopflipng_path: Path | None = None,
) -> dict[str, object]:
    """Generate deterministic before/after artifacts for the frozen target set."""
    output_dir.mkdir(parents=True, exist_ok=True)
    pairs: list[dict[str, object]] = []
    for target_id, relative_path, selection_note in SCORECARD_TARGETS:
        source_path = _PRISM_ROOT / relative_path
        variants: dict[str, dict[str, object]] = {}
        for label, enabled in (("off", False), ("on", True)):
            artifact_path = output_dir / f"{target_id}.dither-{label}.png"
            evidence_path = output_dir / f"{target_id}.dither-{label}.json"
            variants[label] = write_candidate(
                source_path,
                artifact_path,
                colors=colors,
                dither=enabled,
                dither_strength=dither_strength,
                pack_mode=pack_mode,
                zopflipng_path=zopflipng_path,
                evidence_path=evidence_path,
                measure_external=True,
            )
        off = variants["off"]
        on = variants["on"]
        pairs.append(
            {
                "target_id": target_id,
                "source_path": relative_path,
                "selection_note": selection_note,
                "dither_off_evidence": f"{target_id}.dither-off.json",
                "dither_on_evidence": f"{target_id}.dither-on.json",
                "dither_off_artifact": f"{target_id}.dither-off.png",
                "dither_on_artifact": f"{target_id}.dither-on.png",
                "dither_off_bytes": off["output_bytes"],
                "dither_on_bytes": on["output_bytes"],
                "size_delta_on_minus_off": int(on["output_bytes"]) - int(off["output_bytes"]),
                "dither_changed_assignments": on["dither"]["changed_assignments_vs_no_dither"],  # type: ignore[index]
                "dither_off_metrics": off["metrics"],
                "dither_on_metrics": on["metrics"],
                "off_structural_checks": off["structural_checks"],
                "on_structural_checks": on["structural_checks"],
                "off_external_metrics": off.get("external_metrics", {}),
                "on_external_metrics": on.get("external_metrics", {}),
            }
        )
    scorecard = {
        "schema_version": "prism.lab.prism-dither-scorecard/0",
        "evidence_label": "pilot comparison; raw measurements, not a quality/default claim",
        "recipe": {
            "colors": colors,
            "dither_strength": {
                "numerator": dither_strength[0],
                "denominator": dither_strength[1],
            },
            "pack_mode": pack_mode,
            "targets": [target[0] for target in SCORECARD_TARGETS],
            "dither_variants": ["off", "floyd-steinberg-1976-alpha-boundary-v0"],
        },
        "interpretation_boundary": (
            "Dither may worsen scalar metrics and bytes while reducing visible banding; "
            "this scorecard records both directions and makes no preference claim."
        ),
        "pairs": pairs,
    }
    (output_dir / "scorecard.json").write_text(
        json.dumps(scorecard, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    return scorecard


def _parse_cli(args: Sequence[str]) -> tuple[str, list[str], dict[str, object]]:
    if not args:
        raise DitherCliError(2, "usage: prism_dither.py encode|scorecard ...")
    command = args[0]
    positional: list[str] = []
    options: dict[str, object] = {
        "colors": 64,
        "adaptive_default": False,
        "dither": True,
        "dither_strength": (1, 1),
        "dither_strength_explicit": False,
        "dither_policy": "uniform",
        "pack_mode": "fast",
        "zopflipng_path": None,
        "evidence_path": None,
        "measure_external": False,
    }
    index = 1
    while index < len(args):
        token = args[index]
        if token in (
            "--colors",
            "--adaptive-default",
            "--dither",
            "--dither-strength",
            "--dither-policy",
            "--pack",
            "--zopflipng",
            "--evidence",
        ):
            if index + 1 >= len(args):
                raise DitherCliError(2, f"usage_error: {token} needs a value")
            value = args[index + 1]
            if token == "--colors":
                try:
                    options["colors"] = int(value, 10)
                except ValueError as error:
                    raise DitherCliError(2, "usage_error: --colors must be an integer") from error
            elif token == "--adaptive-default":
                if value not in ("on", "off"):
                    raise DitherCliError(
                        2, "usage_error: --adaptive-default must be on or off"
                    )
                options["adaptive_default"] = value == "on"
            elif token == "--dither":
                if value not in ("on", "off"):
                    raise DitherCliError(2, "usage_error: --dither must be on or off")
                options["dither"] = value == "on"
            elif token == "--dither-strength":
                options["dither_strength"] = _parse_dither_strength(value)
                options["dither_strength_explicit"] = True
            elif token == "--dither-policy":
                if value not in (
                    "uniform", "adaptive", "region", "adaptive-unit", "luma-bluenoise"
                ):
                    raise DitherCliError(
                        2,
                        "usage_error: --dither-policy must be uniform, adaptive, "
                        "region, adaptive-unit, or luma-bluenoise",
                    )
                options["dither_policy"] = value
            elif token == "--pack":
                if value not in ("fast", "max", "zopfli"):
                    raise DitherCliError(2, "usage_error: --pack must be fast or max")
                options["pack_mode"] = value
            elif token == "--zopflipng":
                options["zopflipng_path"] = Path(value)
            else:
                options["evidence_path"] = Path(value)
            index += 2
        elif token == "--external-metrics":
            options["measure_external"] = True
            index += 1
        elif token.startswith("-"):
            raise DitherCliError(2, f"usage_error: unknown option {token}")
        else:
            positional.append(token)
            index += 1
    return command, positional, options


def main(argv: Sequence[str] | None = None) -> int:
    try:
        command, positional, options = _parse_cli(
            list(sys.argv[1:] if argv is None else argv)
        )
        if command == "encode":
            if len(positional) != 2:
                raise DitherCliError(
                    2,
                    "usage: prism_dither.py encode INPUT OUTPUT "
                    "[--adaptive-default on|off] "
                    "[--dither on|off] [--dither-strength S] "
                    "[--dither-policy uniform|adaptive|region|adaptive-unit"
                    "|luma-bluenoise] "
                    "[--colors N] [--pack fast|max]",
                )
            evidence = write_candidate(
                Path(positional[0]),
                Path(positional[1]),
                colors=int(options["colors"]),
                adaptive_default=bool(options["adaptive_default"]),
                dither=bool(options["dither"]),
                dither_strength=options["dither_strength"],  # type: ignore[arg-type]
                dither_strength_explicit=bool(options["dither_strength_explicit"]),
                dither_policy=str(options["dither_policy"]),
                pack_mode=str(options["pack_mode"]),
                zopflipng_path=options["zopflipng_path"],  # type: ignore[arg-type]
                evidence_path=options["evidence_path"],  # type: ignore[arg-type]
                measure_external=bool(options["measure_external"]),
            )
            policy_evidence = evidence.get(
                "region",
                evidence.get(
                    "adaptive",
                    evidence.get("adaptive_unit", evidence.get("luma_bluenoise")),
                ),
            )
            policy_note = (
                f"; policy={policy_evidence['dither_policy']}"
                if isinstance(policy_evidence, dict)
                else ""
            )
            print(
                f"prism-dither: {evidence['source_bytes']} -> {evidence['output_bytes']} bytes; "
                f"dither={'on' if evidence['dither_enabled'] else 'off'}; "
                f"strength={evidence['dither_strength']['numerator']}/"
                f"{evidence['dither_strength']['denominator']}; pack={evidence['pack_mode']}"
                f"{policy_note}"
            )
            return 0
        if command == "scorecard":
            if len(positional) != 1:
                raise DitherCliError(
                    2,
                    "usage: prism_dither.py scorecard OUTPUT_DIR [--colors N] "
                    "[--dither-strength S] [--pack fast|max]",
                )
            scorecard = generate_scorecard(
                Path(positional[0]),
                colors=int(options["colors"]),
                dither_strength=options["dither_strength"],  # type: ignore[arg-type]
                pack_mode=str(options["pack_mode"]),
                zopflipng_path=options["zopflipng_path"],  # type: ignore[arg-type]
            )
            print(f"prism-dither scorecard: {len(scorecard['pairs'])} before/after pairs")
            return 0
        raise DitherCliError(2, f"usage_error: unknown command {command}")
    except DitherCliError as error:
        print(str(error), file=sys.stderr)
        return error.status
    except OSError as error:
        print(f"io_error: {error}", file=sys.stderr)
        return 5
    except Exception as error:  # pragma: no cover - defensive CLI boundary
        print(f"internal: {error}", file=sys.stderr)
        return 70


if __name__ == "__main__":
    raise SystemExit(main())

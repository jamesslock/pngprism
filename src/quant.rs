//! Mirror of the current `lab/reference/prism_quant.py` oracle (the integrated
//! pipeline, minus the CLI, which lives in `src/main.rs`): the six-seam staged
//! quantizer, historically ported from the v0.2.0-alpha pipeline pin,
//!
//! ```text
//! decode -> sample -> palette init -> refinement -> remap -> emit
//! ```
//!
//! plus the phase-2 (T-0095) integration: `quantize_candidate` and the
//! `quantize_png` dither ([`crate::dither`]) / pack ([`crate::pack`])
//! parameters.
//!
//! This is a seam-by-seam translation of in-repo original work (T-0067
//! skeleton, T-0068 real core, T-0094/T-0095 phase 2, review-passed); the
//! Python reference is the behavioral ORACLE. Every function names its
//! mirrored oracle function (vendored at `tests/oracle/`), and
//! §6/§P2.5 the determinism rules (integer widths, half-up rounding,
//! explicit ordering, tie semantics). Method provenance: inherited from
//! the oracle's ledger rows (`lab/reference/REFERENCES.md`); no new
//! methods, no external sources.
//!
//! **Label: 0.5.0, unproven, metric-validated only.**

use crate::parallel::{MergeOrder, Parallelism, map_ranges};
use crate::png::{self, DecodedImage};
use crate::{Error, Rgba, dither, pack};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Pipeline identity strings (`pngprism.VERSION` / `LABEL`). Unified to the
/// `pngprism 0.5.0` release identity (T-0213); the LABEL keeps its honest
/// "unproven, metric-validated only" status (no human-acceptability gate).
pub const VERSION: &str = "0.5.0";
pub const LABEL: &str = "0.5.0, unproven, metric-validated only";

pub const DEFAULT_COLORS: i64 = 256;
pub const MAX_COLORS: i64 = 256;

// --- Stable dither/pack CLI vocabulary (introduced in v0.2) -----------------

pub const DEFAULT_DITHER: bool = false;
/// Legacy programmatic default for the `bool` adaptive-default surface
/// (`false` == the composable `off` policy). The CLI omission default is
/// [`DEFAULT_ADAPTIVE_DEFAULT_POLICY`] (`guarded`), not this.
pub const DEFAULT_ADAPTIVE_DEFAULT: bool = false;

/// The three named adaptive-default policies (`pngprism.ADAPTIVE_DEFAULT_POLICIES`,
/// T-0190/E-0038). `off` and `on` keep their frozen pre-flip meaning; `guarded`
/// runs unguarded adaptive-unit *unless* the E-0032 structural guard fires.
pub const ADAPTIVE_DEFAULT_POLICIES: [&str; 3] = ["off", "on", "guarded"];
/// CLI omission default (`pngprism.DEFAULT_ADAPTIVE_DEFAULT`): guarded
/// adaptive-unit dithering.
pub const DEFAULT_ADAPTIVE_DEFAULT_POLICY: &str = "guarded";

/// E-0036 pack-seam omission defaults, adopted default-on for S and R by
/// T-0192/E-0040 (`pngprism.DEFAULT_PACK_SEAM_*`). These apply only when
/// `--pack none` is in effect and no seam flag is named; ARM-M stays off.
pub const DEFAULT_PACK_SEAM_PALETTE_SORT: bool = true;
pub const DEFAULT_PACK_SEAM_MEMLEVEL: bool = false;
pub const DEFAULT_PACK_SEAM_REDUCTION: bool = true;

pub const DEFAULT_DITHER_STRENGTH: (i64, i64) = (1, 1);
pub const DITHER_POLICIES: [&str; 5] = [
    "uniform",
    "adaptive",
    "region",
    "adaptive-unit",
    "luma-bluenoise",
];
pub const DEFAULT_DITHER_POLICY: &str = "uniform";
pub const PACK_MODES: [&str; 3] = ["none", "fast", "max"];
pub const DEFAULT_PACK_MODE: &str = "none";
pub const PACK_SEARCHES: [&str; 2] = ["v1", "v2"];
pub const DEFAULT_PACK_SEARCH: &str = "v1";
pub const COLOR_SPACES: [&str; 2] = ["srgb", "oklab"];
pub const DEFAULT_COLOR_SPACE: &str = "srgb";

/// Adaptive-default policy (T-0190/E-0038). Selects the `--adaptive-default`
/// value; omission resolves to [`AdaptiveDefault::Guarded`]. `Off`/`On` keep
/// their frozen pre-flip byte behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveDefault {
    /// Frozen legacy no-dither path; composes with explicit dither flags.
    Off,
    /// Frozen unguarded adaptive-unit dithering.
    On,
    /// Adaptive-unit dithering unless the E-0032 structural guard fires
    /// (integer-exact `opaque_frac == 0`), in which case no dither is applied.
    Guarded,
}

impl AdaptiveDefault {
    /// Parse a CLI value; `None` for anything outside the frozen vocabulary
    /// (`pngprism.ADAPTIVE_DEFAULT_POLICIES`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "on" => Some(Self::On),
            "guarded" => Some(Self::Guarded),
            _ => None,
        }
    }

    /// The frozen CLI/JSON policy name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Guarded => "guarded",
        }
    }

    /// Map the legacy `bool` programmatic surface: `true` == frozen unguarded
    /// `on`, `false` == `off` (`pngprism`'s `isinstance(adaptive_default, bool)`
    /// branch).
    #[must_use]
    pub fn from_bool(value: bool) -> Self {
        if value { Self::On } else { Self::Off }
    }
}

/// The E-0032 Option-A structural guard predicate (T-0190/E-0038), computed
/// integer-exact. The oracle fires the guard when `round(opaque_count / total,
/// 4) == 0.0` where `opaque_count` counts fully-opaque (alpha == 255) pixels.
/// With CPython's round-half-to-even, that holds iff `opaque_count / total <
/// 0.00005` strictly: the exact `1/20000` boundary rounds up to `0.0001` (the
/// nearest double to `0.00005` sits just above it), so the guard does NOT fire
/// there. Equivalent, over integers with no float divergence:
/// `opaque_count * 20000 < total`. Verified byte-for-byte against Python `round`
/// over the exact-boundary family and 500k random `(count, total)` pairs.
#[must_use]
pub fn adaptive_guard_fires(opaque_count: usize, total: usize) -> bool {
    // `total` is a nonzero pixel count; `opaque_count <= total`. The multiply
    // is done in u128 so no image dimension can overflow it.
    (opaque_count as u128) * 20000 < (total as u128)
}

// --- v0.1 core declared constants (T-0068) ---------------------------------

/// Exact-color histogram while distinct colors stay at or below this
/// limit; above it, a fine preclip (16 levels/channel, alpha
/// endpoint-isolated) bounds the working set (`pngprism.EXACT_BIN_LIMIT`).
const EXACT_BIN_LIMIT: usize = 32768;
const PRECLIP_LEVELS: i64 = 16;

/// Refinement works on a deterministic stride sample of the sorted bins
/// when there are more than this many; final remap covers ALL bins
/// (`pngprism.REFINE_SAMPLE_CAP`).
const REFINE_SAMPLE_CAP: usize = 4096;

/// Sparse factorized init budgets (ch19 A2): the RGB rep count is
/// ceil(colors / zoned alpha levels), floored at RGB_REP_MAX
/// (`pngprism.RGB_REP_MAX` etc.).
const RGB_REP_MAX: i64 = 32;
const ALPHA_LADDER_INTERIOR_MAX: i64 = 8;
const RGB_FIT_ITERS: usize = 4;
const ALPHA_LADDER_MAX_ITERS: usize = 32;

/// Joint refinement convergence bound (`pngprism.REFINE_MAX_ITERS`).
const REFINE_MAX_ITERS: usize = 8;

/// Hidden-RGB policy hook values (`pngprism.HIDDEN_RGB_POLICIES`).
pub const HIDDEN_RGB_POLICIES: [&str; 2] = ["canonicalize-black", "preserve-mean"];
pub const DEFAULT_HIDDEN_RGB_POLICY: &str = "canonicalize-black";

const ZONE_TRANSPARENT: u8 = 0;
const ZONE_INTERIOR: u8 = 1;
const ZONE_OPAQUE: u8 = 2;

/// The declared per-stage observations of one pipeline execution
/// (`pngprism.StageNotes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageNotes {
    pub sampled_pixels: usize,
    pub initial_bins: usize,
    pub refined_palette_entries: usize,
    pub alpha_note: String,
    pub exact_path: bool,
    pub palette_init_pairs: usize,
    pub refinement_iterations: usize,
    pub hidden_rgb_policy: String,
}

/// The `quantize_png` summary dict's Rust shape (same fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub version: &'static str,
    pub label: &'static str,
    pub colors: i64,
    pub hidden_rgb_policy: String,
    pub color_space: String,
    pub source_bytes: usize,
    pub output_bytes: usize,
    pub palette_entries: usize,
    pub stages: StageNotes,
}

/// One histogram bin: its key plus actual member-pixel sums (never grid
/// centers). Mirrors `pngprism._Bin`. Sums are i64: a bin can hold
/// millions of pixels (count * 255 * 255 < 2^33 at smoke-set sizes, with
/// vast headroom to 2^63).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bin {
    key: (u8, u8, u8, u8),
    count: i64,
    sum_r: i64,
    sum_g: i64,
    sum_b: i64,
    sum_a: i64,
    sum_ar: i64,
    sum_ag: i64,
    sum_ab: i64,
    zone: u8,
}

/// Palette-init seam output (`pngprism.PaletteInit`).
#[derive(Debug)]
struct PaletteInit {
    bins: Vec<Bin>,
    palette: Vec<Rgba>,
    /// The instantiated alpha ladder. Kept for seam-output parity with the
    /// oracle (its PaletteInit carries it for observability); no
    /// downstream seam consumes it there either.
    #[allow(dead_code)]
    ladder: Vec<i64>,
    exact: bool,
    exact_path: bool,
}

/// Exact floor(numerator/denominator + 1/2) on nonnegative ints
/// (`pngprism._round_half_up`). Rust `/` on nonnegative i64 floors
/// exactly like Python `//`. This is the ONLY rounding in the pipeline.
fn round_half_up(numerator: i64, denominator: i64) -> i64 {
    debug_assert!(numerator >= 0 && denominator > 0);
    (2 * numerator + denominator) / (2 * denominator)
}

/// `pngprism._zone_of`.
fn zone_of(alpha: i64) -> u8 {
    if alpha == 0 {
        return ZONE_TRANSPARENT;
    }
    if alpha == 255 {
        return ZONE_OPAQUE;
    }
    ZONE_INTERIOR
}

/// Endpoint-isolating alpha bin (`pngprism._alpha_bin`): a==0 -> 0,
/// a==255 -> levels-1, interior values spread over 1..levels-2.
fn alpha_bin(alpha: u8, levels: i64) -> u8 {
    if alpha == 0 {
        return 0;
    }
    if alpha == 255 {
        return (levels - 1) as u8;
    }
    (1 + (i64::from(alpha) - 1) * (levels - 2) / 254) as u8
}

/// `pngprism._pack_rgba` (deterministic total order for tie-breaks).
fn pack_rgba(value: Rgba) -> u32 {
    (u32::from(value.0) << 24)
        | (u32::from(value.1) << 16)
        | (u32::from(value.2) << 8)
        | u32::from(value.3)
}

/// Declared alpha-aware distance: squared Euclidean over premultiplied
/// RGBA on the 65025 scale (`pngprism.premultiplied_distance_sq`).
/// Max value 4 * 65025^2 < 2^34 — i64 throughout.
pub fn premultiplied_distance_sq(p: Rgba, q: Rgba) -> i64 {
    let dr = i64::from(p.3) * i64::from(p.0) - i64::from(q.3) * i64::from(q.0);
    let dg = i64::from(p.3) * i64::from(p.1) - i64::from(q.3) * i64::from(q.1);
    let db = i64::from(p.3) * i64::from(p.2) - i64::from(q.3) * i64::from(q.2);
    let da = 255 * (i64::from(p.3) - i64::from(q.3));
    dr * dr + dg * dg + db * db + da * da
}

// --- E-0016 opt-in Oklab assignment/refinement/remap ----------------------

type OklabFeature = [f64; 4];
type OklabFeatureBins = BTreeMap<(u8, u8, u8, u8), OklabFeatureBin>;

#[derive(Debug, Clone, Copy)]
struct OklabFeatureBin {
    count: i64,
    sums: OklabFeature,
}

impl OklabFeatureBin {
    fn mean(self) -> OklabFeature {
        let count = self.count as f64;
        [
            self.sums[0] / count,
            self.sums[1] / count,
            self.sums[2] / count,
            self.sums[3] / count,
        ]
    }
}

fn srgb8_to_linear(value: u8) -> f64 {
    let encoded = f64::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb8(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8
}

fn python_cbrt(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value.abs().powf(1.0 / 3.0).copysign(value)
    }
}

fn srgb8_to_oklab(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
    let red = srgb8_to_linear(rgb.0);
    let green = srgb8_to_linear(rgb.1);
    let blue = srgb8_to_linear(rgb.2);
    let light = 0.412_221_470_8 * red + 0.536_332_536_3 * green + 0.051_445_992_9 * blue;
    let medium = 0.211_903_498_2 * red + 0.680_699_545_1 * green + 0.107_396_956_6 * blue;
    let short = 0.088_302_461_9 * red + 0.281_718_837_6 * green + 0.629_978_700_5 * blue;
    let light_root = python_cbrt(light);
    let medium_root = python_cbrt(medium);
    let short_root = python_cbrt(short);
    (
        0.210_454_255_3 * light_root + 0.793_617_785_0 * medium_root - 0.004_072_046_8 * short_root,
        1.977_998_495_1 * light_root - 2.428_592_205_0 * medium_root + 0.450_593_709_9 * short_root,
        0.025_904_037_1 * light_root + 0.782_771_766_2 * medium_root - 0.808_675_766_0 * short_root,
    )
}

fn oklab_to_srgb8(lab: (f64, f64, f64)) -> (u8, u8, u8) {
    let (lightness, axis_a, axis_b) = lab;
    let light_root = lightness + 0.396_337_777_4 * axis_a + 0.215_803_757_3 * axis_b;
    let medium_root = lightness - 0.105_561_345_8 * axis_a - 0.063_854_172_8 * axis_b;
    let short_root = lightness - 0.089_484_177_5 * axis_a - 1.291_485_548_0 * axis_b;
    let light = light_root * light_root * light_root;
    let medium = medium_root * medium_root * medium_root;
    let short = short_root * short_root * short_root;
    (
        linear_to_srgb8(
            4.076_741_662_1 * light - 3.307_711_591_3 * medium + 0.230_969_929_2 * short,
        ),
        linear_to_srgb8(
            -1.268_438_004_6 * light + 2.609_757_401_1 * medium - 0.341_319_396_5 * short,
        ),
        linear_to_srgb8(
            -0.004_196_086_3 * light - 0.703_418_614_7 * medium + 1.707_614_701_0 * short,
        ),
    )
}

fn premultiplied_oklab_feature(pixel: Rgba) -> OklabFeature {
    let alpha = f64::from(pixel.3) / 255.0;
    if alpha == 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let (lightness, axis_a, axis_b) = srgb8_to_oklab((pixel.0, pixel.1, pixel.2));
    [alpha * lightness, alpha * axis_a, alpha * axis_b, alpha]
}

fn pixel_bin_key(pixel: Rgba, exact: bool) -> (u8, u8, u8, u8) {
    if exact {
        pixel
    } else {
        preclip_key(pixel.0, pixel.1, pixel.2, pixel.3, PRECLIP_LEVELS)
    }
}

fn oklab_feature_bins(pixels: &[Rgba], init: &PaletteInit) -> Result<OklabFeatureBins, Error> {
    let mut accumulators: BTreeMap<(u8, u8, u8, u8), [f64; 5]> = BTreeMap::new();
    for &pixel in pixels {
        let key = pixel_bin_key(pixel, init.exact);
        let accumulator = accumulators.entry(key).or_insert([0.0; 5]);
        let feature = premultiplied_oklab_feature(pixel);
        accumulator[0] += 1.0;
        for index in 0..4 {
            accumulator[index + 1] += feature[index];
        }
    }
    let result: BTreeMap<_, _> = accumulators
        .into_iter()
        .map(|(key, values)| {
            (
                key,
                OklabFeatureBin {
                    count: values[0] as i64,
                    sums: [values[1], values[2], values[3], values[4]],
                },
            )
        })
        .collect();
    if result.len() != init.bins.len()
        || init.bins.iter().any(|item| !result.contains_key(&item.key))
    {
        return Err(Error::internal(
            "internal: Oklab bins differ from initializer".to_string(),
        ));
    }
    for item in &init.bins {
        let feature = result.get(&item.key).ok_or_else(|| {
            Error::internal("internal: Oklab bins differ from initializer".to_string())
        })?;
        if feature.count != item.count {
            return Err(Error::internal(
                "internal: Oklab bin count mismatch".to_string(),
            ));
        }
    }
    Ok(result)
}

fn oklab_distance_sq(left: OklabFeature, right: OklabFeature) -> f64 {
    let mut total = 0.0;
    for index in 0..4 {
        let delta = left[index] - right[index];
        // Python's `delta ** 2` goes through binary64 pow, which can differ
        // by one ulp from `delta * delta`. LLVM otherwise folds `powf(2.0)`
        // back to multiplication in release builds, so keep the exponent
        // opaque and preserve the oracle operation. That ulp can change a
        // strict-first assignment tie.
        total += delta.powf(std::hint::black_box(2.0));
    }
    total
}

fn nearest_oklab_entry(
    feature: OklabFeature,
    zone: u8,
    entries: &[OklabFeature],
    entry_zones: &[u8],
) -> Result<usize, Error> {
    let mut best: Option<(usize, f64)> = None;
    for (index, &entry) in entries.iter().enumerate() {
        if entry_zones[index] != zone {
            continue;
        }
        let distance = oklab_distance_sq(feature, entry);
        if best.is_none_or(|(_, best_distance)| distance < best_distance) {
            best = Some((index, distance));
        }
    }
    if let Some((index, _)) = best {
        return Ok(index);
    }
    if zone == ZONE_TRANSPARENT {
        return Err(Error::internal(
            "internal: transparent bin without transparent entry".to_string(),
        ));
    }
    let mut fallback: Option<(usize, f64)> = None;
    for (index, &entry) in entries.iter().enumerate() {
        if entry_zones[index] == ZONE_TRANSPARENT && entries.len() > 1 {
            continue;
        }
        let distance = oklab_distance_sq(feature, entry);
        if fallback.is_none_or(|(_, best_distance)| distance < best_distance) {
            fallback = Some((index, distance));
        }
    }
    fallback
        .map(|(index, _)| index)
        .ok_or_else(|| Error::internal("internal: empty palette".to_string()))
}

fn oklab_centroid_from_feature_sums(
    count: i64,
    sum_alpha_u8: i64,
    sum_alpha_lightness: f64,
    sum_alpha_axis_a: f64,
    sum_alpha_axis_b: f64,
) -> Rgba {
    let alpha = round_half_up(sum_alpha_u8, count);
    if alpha == 0 || sum_alpha_u8 == 0 {
        return (0, 0, 0, 0);
    }
    let sum_alpha_normalized = sum_alpha_u8 as f64 / 255.0;
    let (red, green, blue) = oklab_to_srgb8((
        sum_alpha_lightness / sum_alpha_normalized,
        sum_alpha_axis_a / sum_alpha_normalized,
        sum_alpha_axis_b / sum_alpha_normalized,
    ));
    (red, green, blue, alpha as u8)
}

fn oklab_single_bin_centroid(item: &Bin, feature: OklabFeatureBin) -> Rgba {
    let mut candidate = oklab_centroid_from_feature_sums(
        item.count,
        item.sum_a,
        feature.sums[0],
        feature.sums[1],
        feature.sums[2],
    );
    if item.zone == ZONE_OPAQUE {
        candidate.3 = 255;
    }
    candidate
}

fn refine_oklab(
    init: &PaletteInit,
    feature_by_key: &OklabFeatureBins,
) -> Result<(Vec<Rgba>, usize), Error> {
    let mut palette = init.palette.clone();
    if palette.is_empty() || init.bins.is_empty() || init.exact_path {
        return Ok((palette, 0));
    }
    let sample = refine_sample(&init.bins);
    let mut sample_features = Vec::with_capacity(sample.len());
    for item in &sample {
        sample_features.push(
            feature_by_key
                .get(&item.key)
                .ok_or_else(|| Error::internal("internal: missing Oklab feature bin".to_string()))?
                .mean(),
        );
    }
    let mut entry_zones: Vec<u8> = palette
        .iter()
        .map(|entry| zone_of(i64::from(entry.3)))
        .collect();
    let mut iterations = 0;
    for iteration in 1..=REFINE_MAX_ITERS {
        iterations = iteration;
        let entry_features: Vec<OklabFeature> = palette
            .iter()
            .copied()
            .map(premultiplied_oklab_feature)
            .collect();
        let assignments: Vec<usize> = sample
            .iter()
            .enumerate()
            .map(|(index, item)| {
                nearest_oklab_entry(
                    sample_features[index],
                    item.zone,
                    &entry_features,
                    &entry_zones,
                )
            })
            .collect::<Result<_, _>>()?;
        let mut accumulators = vec![[0.0f64; 5]; palette.len()];
        for (index, item) in sample.iter().enumerate() {
            let target = &mut accumulators[assignments[index]];
            let feature = feature_by_key.get(&item.key).ok_or_else(|| {
                Error::internal("internal: missing Oklab feature bin".to_string())
            })?;
            target[0] += item.count as f64;
            target[1] += item.sum_a as f64;
            target[2] += feature.sums[0];
            target[3] += feature.sums[1];
            target[4] += feature.sums[2];
        }

        let mut worst: BTreeMap<u8, (f64, Rgba)> = BTreeMap::new();
        for (index, item) in sample.iter().enumerate() {
            let distance =
                oklab_distance_sq(sample_features[index], entry_features[assignments[index]]);
            let feature = *feature_by_key.get(&item.key).ok_or_else(|| {
                Error::internal("internal: missing Oklab feature bin".to_string())
            })?;
            let candidate = oklab_single_bin_centroid(item, feature);
            let replace = worst
                .get(&item.zone)
                .is_none_or(|&(current_distance, current)| {
                    distance > current_distance
                        || (distance == current_distance
                            && pack_rgba(candidate) < pack_rgba(current))
                });
            if replace {
                worst.insert(item.zone, (distance, candidate));
            }
        }

        let mut new_palette = Vec::with_capacity(palette.len());
        let mut new_zones = Vec::with_capacity(palette.len());
        let mut moved = false;
        let mut zone_counts: BTreeMap<u8, i64> = BTreeMap::new();
        for &zone in &entry_zones {
            *zone_counts.entry(zone).or_insert(0) += 1;
        }
        for (index, &entry) in palette.iter().enumerate() {
            let zone = entry_zones[index];
            let sums = accumulators[index];
            if zone == ZONE_TRANSPARENT {
                new_palette.push(entry);
                new_zones.push(zone);
                continue;
            }
            if sums[0] as i64 == 0 {
                let candidate = worst.get(&zone).copied();
                let unserved = candidate.is_none_or(|(distance, _)| distance == 0.0);
                let count = zone_counts.get(&zone).copied().unwrap_or(0);
                if unserved && count > 1 {
                    if let Some(count) = zone_counts.get_mut(&zone) {
                        *count -= 1;
                    }
                    moved = true;
                    continue;
                }
                if unserved {
                    new_palette.push(entry);
                    new_zones.push(zone);
                    continue;
                }
                if let Some((_, replacement)) = candidate {
                    new_palette.push(replacement);
                    new_zones.push(zone);
                    moved = true;
                    continue;
                }
            }
            let mut updated = oklab_centroid_from_feature_sums(
                sums[0] as i64,
                sums[1] as i64,
                sums[2],
                sums[3],
                sums[4],
            );
            if zone == ZONE_OPAQUE {
                updated.3 = 255;
            }
            new_palette.push(updated);
            new_zones.push(zone);
            if updated != entry {
                moved = true;
            }
        }
        palette = new_palette;
        entry_zones = new_zones;
        if !moved {
            break;
        }
    }
    Ok((palette, iterations))
}

fn remap_oklab(
    pixels: &[Rgba],
    init: &PaletteInit,
    palette: &[Rgba],
    feature_by_key: &OklabFeatureBins,
) -> Result<Vec<u8>, Error> {
    if palette.is_empty() {
        return Ok(Vec::new());
    }
    let entry_features: Vec<OklabFeature> = palette
        .iter()
        .copied()
        .map(premultiplied_oklab_feature)
        .collect();
    let entry_zones: Vec<u8> = palette
        .iter()
        .map(|entry| zone_of(i64::from(entry.3)))
        .collect();
    let mut assignment: BTreeMap<(u8, u8, u8, u8), u8> = BTreeMap::new();
    for item in &init.bins {
        let feature = feature_by_key
            .get(&item.key)
            .ok_or_else(|| Error::internal("internal: missing Oklab feature bin".to_string()))?
            .mean();
        let index = nearest_oklab_entry(feature, item.zone, &entry_features, &entry_zones)?;
        assignment.insert(item.key, index as u8);
    }
    pixels
        .iter()
        .map(|&pixel| {
            assignment
                .get(&pixel_bin_key(pixel, init.exact))
                .copied()
                .ok_or_else(|| {
                    Error::internal("internal: pixel key missing from Oklab remap".to_string())
                })
        })
        .collect()
}

/// Squared straight-RGB Euclidean distance (rep assignment/seeding).
fn rgb_dist_sq(v: (u8, u8, u8), s: (u8, u8, u8)) -> i64 {
    let dr = i64::from(v.0) - i64::from(s.0);
    let dg = i64::from(v.1) - i64::from(s.1);
    let db = i64::from(v.2) - i64::from(s.2);
    dr * dr + dg * dg + db * db
}

/// Squared distance between two premultiplied 4-vectors.
fn premult_dist_sq(a: [i64; 4], b: [i64; 4]) -> i64 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    let da = a[3] - b[3];
    dr * dr + dg * dg + db * db + da * da
}

/// The 8-member-sum accumulation shared by exact and preclip tables.
fn fresh_sums(r: u8, g: u8, b: u8, a: u8) -> [i64; 8] {
    let (r, g, b, a) = (i64::from(r), i64::from(g), i64::from(b), i64::from(a));
    [1, r, g, b, a, a * r, a * g, a * b]
}

fn add_pixel(sums: &mut [i64; 8], r: u8, g: u8, b: u8, a: u8) {
    let (r, g, b, a) = (i64::from(r), i64::from(g), i64::from(b), i64::from(a));
    sums[0] += 1;
    sums[1] += r;
    sums[2] += g;
    sums[3] += b;
    sums[4] += a;
    sums[5] += a * r;
    sums[6] += a * g;
    sums[7] += a * b;
}

fn preclip_key(r: u8, g: u8, b: u8, a: u8, levels: i64) -> (u8, u8, u8, u8) {
    (
        (i64::from(r) * levels / 256) as u8,
        (i64::from(g) * levels / 256) as u8,
        (i64::from(b) * levels / 256) as u8,
        alpha_bin(a, levels),
    )
}

type HistogramTable = BTreeMap<(u8, u8, u8, u8), [i64; 8]>;

#[derive(Debug)]
struct HistogramState {
    table: HistogramTable,
    exact: bool,
}

fn merge_sums(into: &mut [i64; 8], from: &[i64; 8]) {
    for (destination, source) in into.iter_mut().zip(from) {
        *destination += source;
    }
}

fn convert_to_preclip(table: HistogramTable) -> HistogramTable {
    let mut converted = BTreeMap::new();
    for ((r, g, b, a), sums) in table {
        let key = preclip_key(r, g, b, a, PRECLIP_LEVELS);
        merge_sums(converted.entry(key).or_insert([0; 8]), &sums);
    }
    converted
}

fn histogram_state(pixels: &[Rgba]) -> HistogramState {
    let mut table = BTreeMap::new();
    let mut exact = true;
    for &(r, g, b, a) in pixels {
        if exact {
            let key = (r, g, b, a);
            if let Some(sums) = table.get_mut(&key) {
                add_pixel(sums, r, g, b, a);
                continue;
            }
            if table.len() < EXACT_BIN_LIMIT {
                table.insert(key, fresh_sums(r, g, b, a));
                continue;
            }
            table = convert_to_preclip(table);
            exact = false;
        }
        let key = preclip_key(r, g, b, a, PRECLIP_LEVELS);
        match table.get_mut(&key) {
            Some(sums) => add_pixel(sums, r, g, b, a),
            None => {
                table.insert(key, fresh_sums(r, g, b, a));
            }
        }
    }
    HistogramState { table, exact }
}

fn merge_histogram_states(mut left: HistogramState, mut right: HistogramState) -> HistogramState {
    if left.exact && right.exact {
        for (key, sums) in right.table {
            merge_sums(left.table.entry(key).or_insert([0; 8]), &sums);
        }
        // M2: locally exact shards can exceed the global exact-key limit only
        // after union. Re-evaluate the predicate at every both-exact merge.
        if left.table.len() > EXACT_BIN_LIMIT {
            left.table = convert_to_preclip(left.table);
            left.exact = false;
        }
        return left;
    }
    if left.exact {
        left.table = convert_to_preclip(left.table);
        left.exact = false;
    }
    if right.exact {
        right.table = convert_to_preclip(right.table);
    }
    for (key, sums) in right.table {
        merge_sums(left.table.entry(key).or_insert([0; 8]), &sums);
    }
    HistogramState {
        table: left.table,
        exact: false,
    }
}

fn shuffle_states(states: &mut [HistogramState], mut seed: u64) {
    for index in (1..states.len()).rev() {
        // Fixed xorshift64 sequence: a verification schedule, never entropy.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let swap = (seed as usize) % (index + 1);
        states.swap(index, swap);
    }
}

fn reduce_histogram_states(mut states: Vec<HistogramState>, order: MergeOrder) -> HistogramState {
    if states.is_empty() {
        return HistogramState {
            table: BTreeMap::new(),
            exact: true,
        };
    }
    match order {
        MergeOrder::Forward => states
            .into_iter()
            .reduce(merge_histogram_states)
            .expect("nonempty checked above"),
        MergeOrder::Reverse => states
            .into_iter()
            .rev()
            .reduce(merge_histogram_states)
            .expect("nonempty checked above"),
        MergeOrder::Shuffled(seed) => {
            shuffle_states(&mut states, seed);
            states
                .into_iter()
                .reduce(merge_histogram_states)
                .expect("nonempty checked above")
        }
        MergeOrder::Balanced => {
            while states.len() > 1 {
                let mut next = Vec::with_capacity(states.len().div_ceil(2));
                let mut iter = states.into_iter();
                while let Some(left) = iter.next() {
                    if let Some(right) = iter.next() {
                        next.push(merge_histogram_states(left, right));
                    } else {
                        next.push(left);
                    }
                }
                states = next;
            }
            states.pop().expect("nonempty checked above")
        }
    }
}

fn bins_from_state(state: HistogramState) -> (Vec<Bin>, bool) {
    let mut bins = Vec::with_capacity(state.table.len());
    for (key, sums) in state.table {
        let mean_a = round_half_up(sums[4], sums[0]);
        bins.push(Bin {
            key,
            count: sums[0],
            sum_r: sums[1],
            sum_g: sums[2],
            sum_b: sums[3],
            sum_a: sums[4],
            sum_ar: sums[5],
            sum_ag: sums[6],
            sum_ab: sums[7],
            zone: zone_of(mean_a),
        });
    }
    (bins, state.exact)
}

/// Histogram pass (`pngprism._build_bins`). Exact distinct colors while
/// they fit EXACT_BIN_LIMIT, else the declared fine preclip; every bin
/// carries actual member sums. Returns (bins sorted by key, exact flag).
/// `BTreeMap` iteration IS the oracle's `sorted(tuple-key)` order.
fn build_bins(pixels: &[Rgba]) -> (Vec<Bin>, bool) {
    bins_from_state(histogram_state(pixels))
}

fn build_bins_parallel(
    pixels: &[Rgba],
    parallelism: Parallelism,
) -> Result<(Vec<Bin>, bool), Error> {
    if !parallelism.is_parallel() || pixels.len() < parallelism.threads() {
        return Ok(build_bins(pixels));
    }
    let states = map_ranges(pixels.len(), parallelism, |range| {
        Ok(histogram_state(&pixels[range]))
    })?;
    Ok(bins_from_state(reduce_histogram_states(
        states,
        parallelism.merge_order(),
    )))
}

/// The bin's occupancy-weighted representative (`pngprism._bin_mean_color`).
fn bin_mean_color(b: &Bin) -> Rgba {
    // Each mean of u8-valued members rounds to <= 255 by construction.
    (
        round_half_up(b.sum_r, b.count) as u8,
        round_half_up(b.sum_g, b.count) as u8,
        round_half_up(b.sum_b, b.count) as u8,
        round_half_up(b.sum_a, b.count) as u8,
    )
}

/// The bin's rounded premultiplied mean (`pngprism._bin_premult_mean`);
/// assignment distance input. Components <= 65025.
fn bin_premult_mean(b: &Bin) -> [i64; 4] {
    [
        round_half_up(b.sum_ar, b.count),
        round_half_up(b.sum_ag, b.count),
        round_half_up(b.sum_ab, b.count),
        round_half_up(255 * b.sum_a, b.count),
    ]
}

/// Occupancy-weighted palette value from member sums
/// (`pngprism._centroid`): alpha is the count-weighted mean; RGB
/// un-premultiplies the premultiplied mean by it (sum_ar/sum_a <= 255
/// always, since per-pixel a*r <= 255*a). a*==0 is the caller's policy case.
fn centroid(count: i64, sum_a: i64, sum_ar: i64, sum_ag: i64, sum_ab: i64) -> Rgba {
    let a_star = round_half_up(sum_a, count);
    if a_star == 0 {
        return (0, 0, 0, 0);
    }
    (
        round_half_up(sum_ar, sum_a) as u8,
        round_half_up(sum_ag, sum_a) as u8,
        round_half_up(sum_ab, sum_a) as u8,
        a_star as u8,
    )
}

/// ch19 A1-lite ladder (`pngprism._alpha_ladder`): mandatory exact locks
/// {0, 255} where present, plus interior levels from weighted 1-D Lloyd
/// over the 256 alpha buckets, seeded at weighted quantiles.
fn alpha_ladder(bins: &[Bin]) -> Vec<i64> {
    let mut mass = [0i64; 256];
    for b in bins {
        mass[round_half_up(b.sum_a, b.count) as usize] += b.count;
    }
    let mut ladder: Vec<i64> = Vec::new();
    if mass[0] > 0 {
        ladder.push(0);
    }
    if mass[255] > 0 {
        ladder.push(255);
    }
    let interior: Vec<i64> = (1..255).filter(|&a| mass[a as usize] > 0).collect();
    if !interior.is_empty() {
        let k = ALPHA_LADDER_INTERIOR_MAX.min(interior.len() as i64);
        let total: i64 = interior.iter().map(|&a| mass[a as usize]).sum();
        // Weighted-quantile seeds: the alpha values splitting the interior
        // mass into k equal-weight bands (deterministic, exact integers).
        let mut seeds: Vec<i64> = Vec::new();
        let mut cumulative = 0i64;
        let mut target = 1i64;
        for &a in &interior {
            cumulative += mass[a as usize];
            while target <= k && cumulative * 2 * k >= (2 * target - 1) * total {
                if seeds.last() != Some(&a) {
                    seeds.push(a);
                }
                target += 1;
            }
        }
        let mut levels = seeds;
        for _ in 0..ALPHA_LADDER_MAX_ITERS {
            let mut groups: Vec<Vec<i64>> = vec![Vec::new(); levels.len()];
            for &a in &interior {
                let mut best = 0usize;
                let mut best_d = (a - levels[0]).abs();
                for (j, &level) in levels.iter().enumerate().skip(1) {
                    let d = (a - level).abs();
                    if d < best_d {
                        // ties keep the lower level (oracle: strict <)
                        best = j;
                        best_d = d;
                    }
                }
                groups[best].push(a);
            }
            let mut updated: Vec<i64> = Vec::with_capacity(levels.len());
            for (j, group) in groups.iter().enumerate() {
                if group.is_empty() {
                    updated.push(levels[j]);
                    continue;
                }
                let numerator: i64 = group.iter().map(|&a| a * mass[a as usize]).sum();
                let denominator: i64 = group.iter().map(|&a| mass[a as usize]).sum();
                updated.push(round_half_up(numerator, denominator));
            }
            // The oracle's sorted(set(updated)): ascending, deduplicated.
            updated.sort_unstable();
            updated.dedup();
            if updated == levels {
                break;
            }
            levels = updated;
        }
        ladder.extend(levels);
    }
    // The oracle's sorted(set(ladder)).
    ladder.sort_unstable();
    ladder.dedup();
    ladder
}

/// Deterministic stride sample of the sorted bins
/// (`pngprism._refine_sample`); final remap always covers ALL bins.
fn refine_sample(bins: &[Bin]) -> Vec<&Bin> {
    if bins.len() <= REFINE_SAMPLE_CAP {
        return bins.iter().collect();
    }
    let stride = bins.len().div_ceil(REFINE_SAMPLE_CAP);
    bins.iter().step_by(stride).collect()
}

/// Alpha-mass-weighted RGB representatives over the refinement sample
/// (`pngprism._fit_rgb_reps`): deterministic farthest-point seeding
/// (Gonzalez 1985) plus weighted Lloyd polish. Weight is the bin's total
/// alpha mass sum_a, so fully-transparent pixels never claim palette
/// capacity for hidden RGB.
fn fit_rgb_reps(sample: &[&Bin], cap: i64, zoned_levels: i64) -> Vec<(u8, u8, u8)> {
    let mut items: Vec<((u8, u8, u8), i64)> = Vec::new(); // (mean rgb, weight)
    for b in sample {
        if b.zone == ZONE_TRANSPARENT || b.sum_a == 0 {
            continue;
        }
        let mean = bin_mean_color(b);
        items.push(((mean.0, mean.1, mean.2), b.sum_a));
    }
    if items.is_empty() {
        return vec![(0, 0, 0)];
    }
    // ceil(cap / max(1, zoned_levels)) on positive values (the oracle's
    // -(-cap // max(1, zoned_levels))); i64::div_ceil is unstable here.
    let levels_or_one = 1.max(zoned_levels);
    let budget = RGB_REP_MAX.max((cap + levels_or_one - 1) / levels_or_one);
    let k = budget.min(cap).min(items.len() as i64);
    let packed: Vec<i64> = items
        .iter()
        .map(|(v, _)| (i64::from(v.0) << 16) | (i64::from(v.1) << 8) | i64::from(v.2))
        .collect();
    // Seed 0: maximum alpha mass (ties -> lowest packed RGB). Python
    // min(key=...) is first-minimal; the hand scan with strict < mirrors it.
    let mut first = 0usize;
    for i in 1..items.len() {
        if (-items[i].1, packed[i]) < (-items[first].1, packed[first]) {
            first = i;
        }
    }
    let mut seeds: Vec<(u8, u8, u8)> = vec![items[first].0];
    // Incremental farthest-point: cur_d2[i] = squared distance from item i
    // to its nearest seed so far.
    let mut cur_d2: Vec<i64> = items
        .iter()
        .map(|(value, _)| rgb_dist_sq(*value, seeds[0]))
        .collect();
    while (seeds.len() as i64) < k {
        let mut best = 0usize;
        for i in 1..items.len() {
            if (-(items[i].1 * cur_d2[i]), packed[i])
                < (-(items[best].1 * cur_d2[best]), packed[best])
            {
                best = i;
            }
        }
        if items[best].1 * cur_d2[best] == 0 {
            break; // every weighted distinct color is already seeded
        }
        let s = items[best].0;
        seeds.push(s);
        for (i, (value, _)) in items.iter().enumerate() {
            let d2 = rgb_dist_sq(*value, s);
            if d2 < cur_d2[i] {
                cur_d2[i] = d2;
            }
        }
    }
    let mut reps = seeds;
    // Weighted Lloyd polish (declared RGB_FIT_ITERS bound).
    for _ in 0..RGB_FIT_ITERS {
        let mut acc = vec![[0i64; 4]; reps.len()]; // weight, wr, wg, wb
        for (value, weight) in &items {
            let mut best = 0usize;
            let mut best_d = rgb_dist_sq(*value, reps[0]);
            for (j, &rep) in reps.iter().enumerate().skip(1) {
                let d2 = rgb_dist_sq(*value, rep);
                if d2 < best_d {
                    best = j;
                    best_d = d2;
                }
            }
            acc[best][0] += weight;
            acc[best][1] += weight * i64::from(value.0);
            acc[best][2] += weight * i64::from(value.1);
            acc[best][3] += weight * i64::from(value.2);
        }
        let mut moved = false;
        let mut new_reps = Vec::with_capacity(reps.len());
        for (j, &[weight, wr, wg, wb]) in acc.iter().enumerate() {
            if weight == 0 {
                new_reps.push(reps[j]);
                continue;
            }
            let updated = (
                round_half_up(wr, weight) as u8,
                round_half_up(wg, weight) as u8,
                round_half_up(wb, weight) as u8,
            );
            if updated != reps[j] {
                moved = true;
            }
            new_reps.push(updated);
        }
        reps = new_reps;
        if !moved {
            break;
        }
    }
    reps
}

/// Fill unused palette capacity with deterministic, zone-safe residual
/// seeds (`pngprism._fill_palette_by_weighted_residual`). The sparse
/// factorized initializer's existing entries stay in place; each appended
/// entry is the first refinement-sample bin minimizing the oracle's
/// `(-count * nearest_d2, packed_mean, zone)` key.
fn fill_palette_by_weighted_residual(bins: &[Bin], palette: &[Rgba], colors: i64) -> Vec<Rgba> {
    let mut result = palette.to_vec();
    if (result.len() as i64) >= colors {
        return result;
    }
    let sample = refine_sample(bins);
    let means: Vec<Rgba> = sample.iter().map(|bin| bin_mean_color(bin)).collect();
    let sample_premult: Vec<[i64; 4]> = sample.iter().map(|bin| bin_premult_mean(bin)).collect();
    let mut palette_values: HashSet<Rgba> = result.iter().copied().collect();
    let entries_premult: Vec<[i64; 4]> = result.iter().copied().map(entry_premult).collect();
    let entry_zones: Vec<u8> = result
        .iter()
        .map(|entry| zone_of(i64::from(entry.3)))
        .collect();

    let mut nearest: Vec<Option<i64>> = Vec::with_capacity(sample.len());
    for (bin, &point) in sample.iter().zip(&sample_premult) {
        if bin.zone == ZONE_TRANSPARENT {
            nearest.push(Some(0));
            continue;
        }
        let mut best: Option<i64> = None;
        for (&entry, &zone) in entries_premult.iter().zip(&entry_zones) {
            if zone != bin.zone {
                continue;
            }
            let d2 = premult_dist_sq(point, entry);
            if best.is_none_or(|current| d2 < current) {
                best = Some(d2);
            }
        }
        nearest.push(best);
    }

    while (result.len() as i64) < colors {
        // Python's eligible list preserves sample order and min() keeps the
        // first equal key. A strict comparison in this scan does the same.
        let mut selected: Option<(usize, (i64, u32, u8))> = None;
        for (i, bin) in sample.iter().enumerate() {
            let Some(nearest_d2) = nearest[i] else {
                continue;
            };
            if bin.zone == ZONE_TRANSPARENT || palette_values.contains(&means[i]) || nearest_d2 == 0
            {
                continue;
            }
            let key = (-(bin.count * nearest_d2), pack_rgba(means[i]), bin.zone);
            if selected.as_ref().is_none_or(|(_, current)| key < *current) {
                selected = Some((i, key));
            }
        }
        let Some((selected, _)) = selected else {
            break;
        };

        let seed = means[selected];
        let seed_zone = sample[selected].zone;
        let seed_premult = entry_premult(seed);
        result.push(seed);
        palette_values.insert(seed);
        for (i, (bin, &point)) in sample.iter().zip(&sample_premult).enumerate() {
            if bin.zone != seed_zone {
                continue;
            }
            let d2 = premult_dist_sq(point, seed_premult);
            if nearest[i].is_none_or(|current| d2 < current) {
                nearest[i] = Some(d2);
            }
        }
    }
    result
}

/// v0.1 sampling seam (`pngprism.stage_sample`): identity — every
/// pixel participates.
fn stage_sample(pixels: &[Rgba]) -> &[Rgba] {
    pixels
}

/// v0.1 palette-initialization seam (`pngprism.stage_palette_init`):
/// sparse factorized RGB/alpha init (ch19 §5 contract A2). All values
/// occupancy-weighted (never grid centers).
fn stage_palette_init(
    pixels: &[Rgba],
    colors: i64,
    hidden_rgb_policy: &str,
) -> Result<PaletteInit, Error> {
    stage_palette_init_with_parallelism(pixels, colors, hidden_rgb_policy, Parallelism::SEQUENTIAL)
}

fn stage_palette_init_with_parallelism(
    pixels: &[Rgba],
    colors: i64,
    hidden_rgb_policy: &str,
    parallelism: Parallelism,
) -> Result<PaletteInit, Error> {
    if !HIDDEN_RGB_POLICIES.contains(&hidden_rgb_policy) {
        return Err(Error::data(format!(
            "unknown hidden-rgb-policy: {hidden_rgb_policy}"
        )));
    }
    let (bins, exact) = build_bins_parallel(pixels, parallelism)?;
    if bins.is_empty() {
        return Ok(PaletteInit {
            bins: vec![],
            palette: vec![],
            ladder: vec![],
            exact: true,
            exact_path: true,
        });
    }

    // Exact path (BINDING): distinct colors <= cap -> the palette IS the
    // distinct color set; pixel-exact by construction.
    if exact && (bins.len() as i64) <= colors {
        let palette = bins.iter().map(bin_mean_color).collect();
        return Ok(PaletteInit {
            bins,
            palette,
            ladder: vec![],
            exact,
            exact_path: true,
        });
    }

    let ladder = alpha_ladder(&bins);
    let zoned_levels = ladder
        .iter()
        .filter(|&&level| zone_of(level) != ZONE_TRANSPARENT)
        .count() as i64;
    let sample = refine_sample(&bins);
    let reps = fit_rgb_reps(&sample, colors, 1.max(zoned_levels));

    // Observed co-occurrence (A2 step 3): map each bin to its nearest RGB
    // rep (straight-RGB Euclidean, ties -> lowest index) and nearest
    // ladder level inside its zone; accumulate member sums per pair.
    // Slot iteration order must equal the oracle's dict insertion order
    // (first appearance while scanning bins in sorted-key order), so the
    // order lives in an explicit Vec; the HashMap is lookup-only.
    let mut pair_order: Vec<(i64, i64)> = Vec::new();
    let mut pair_index: HashMap<(i64, i64), usize> = HashMap::new();
    let mut pair_acc: Vec<[i64; 5]> = Vec::new(); // count, sum_a, sum_ar, sum_ag, sum_ab
    let mut pair_mass: Vec<i64> = Vec::new();
    for b in &bins {
        let mean = bin_mean_color(b);
        let slot: (i64, i64) = if b.zone == ZONE_TRANSPARENT {
            (-1, 0) // the single policy-locked a==0 entry
        } else {
            let mut best_rep = 0usize;
            let mut best_d = rgb_dist_sq((mean.0, mean.1, mean.2), reps[0]);
            for (j, &rep) in reps.iter().enumerate().skip(1) {
                let d2 = rgb_dist_sq((mean.0, mean.1, mean.2), rep);
                if d2 < best_d {
                    best_rep = j;
                    best_d = d2;
                }
            }
            let mut level: Option<i64> = None;
            for &candidate in &ladder {
                if zone_of(candidate) != b.zone {
                    continue;
                }
                let closer = match level {
                    None => true,
                    Some(current) => {
                        (candidate - i64::from(mean.3)).abs() < (current - i64::from(mean.3)).abs()
                    }
                };
                if closer {
                    level = Some(candidate);
                }
            }
            (best_rep as i64, level.unwrap_or(i64::from(mean.3)))
        };
        let idx = match pair_index.get(&slot) {
            Some(&i) => i,
            None => {
                let i = pair_order.len();
                pair_index.insert(slot, i);
                pair_order.push(slot);
                pair_acc.push([0; 5]);
                pair_mass.push(0);
                i
            }
        };
        pair_acc[idx][0] += b.count;
        pair_acc[idx][1] += b.sum_a;
        pair_acc[idx][2] += b.sum_ar;
        pair_acc[idx][3] += b.sum_ag;
        pair_acc[idx][4] += b.sum_ab;
        pair_mass[idx] += b.count;
    }

    // Instantiate at most the joint cap (A2 step 5): the a==0 entry is
    // always instantiated when transparent mass exists; then each PRESENT
    // zone reserves its heaviest pair; the remaining pairs rank by mass
    // (ties -> lowest packed initial value).
    //
    // ch17 §31 surviving panic site (internal invariant, not data path):
    // `pair_index[&slot]` below is only ever called with a `slot` drawn from
    // `pair_order` (directly, or `transparent_slot` after its own
    // `pair_index.contains_key` check) — both collections are populated
    // together in the single scan loop above and never diverge, so no pixel
    // value can make this key lookup miss. Restructuring these closures to
    // return `Result` would require reworking the tie-break sort below
    // (`sort_by_key` cannot propagate a fallible key function), which risks
    // the byte-exact-parity tie semantics for no real safety gain over an
    // already-total invariant.
    let palette_centroid = |slot: (i64, i64)| -> Rgba {
        let acc = pair_acc[pair_index[&slot]];
        centroid(acc[0], acc[1], acc[2], acc[3], acc[4])
    };
    let rank_key = |slot: (i64, i64)| -> (i64, u32) {
        (
            -pair_mass[pair_index[&slot]],
            pack_rgba(palette_centroid(slot)),
        )
    };
    let mut palette: Vec<Rgba> = Vec::new();
    let mut instantiated: HashSet<(i64, i64)> = HashSet::new();
    let transparent_slot = (-1i64, 0i64);
    if pair_index.contains_key(&transparent_slot) {
        instantiated.insert(transparent_slot);
        if hidden_rgb_policy == "preserve-mean" {
            let mut sums = [0i64; 3];
            let mut total = 0i64;
            for b in &bins {
                if b.zone == ZONE_TRANSPARENT {
                    sums[0] += b.sum_r;
                    sums[1] += b.sum_g;
                    sums[2] += b.sum_b;
                    total += b.count;
                }
            }
            palette.push((
                round_half_up(sums[0], total) as u8,
                round_half_up(sums[1], total) as u8,
                round_half_up(sums[2], total) as u8,
                0,
            ));
        } else {
            palette.push((0, 0, 0, 0));
        }
    }
    let mut present_zones: Vec<u8> = bins
        .iter()
        .map(|b| b.zone)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    present_zones.sort_unstable();
    for zone in present_zones {
        if zone == ZONE_TRANSPARENT || (palette.len() as i64) >= colors {
            continue;
        }
        let zoned: Vec<(i64, i64)> = pair_order
            .iter()
            .copied()
            .filter(|slot| !instantiated.contains(slot) && zone_of(slot.1) == zone)
            .collect();
        if zoned.is_empty() {
            continue;
        }
        // Heaviest: first minimal by (-mass, packed centroid) in
        // insertion order (oracle min(key=...) is first-minimal).
        let mut heaviest = zoned[0];
        let mut heaviest_key = rank_key(zoned[0]);
        for &slot in &zoned[1..] {
            let key = rank_key(slot);
            if key < heaviest_key {
                heaviest = slot;
                heaviest_key = key;
            }
        }
        instantiated.insert(heaviest);
        palette.push(palette_centroid(heaviest));
    }
    let mut ranked: Vec<(i64, i64)> = pair_order
        .iter()
        .copied()
        .filter(|slot| !instantiated.contains(slot))
        .collect();
    // Rust sort_by_key is stable, so ties keep insertion order — exactly
    // the oracle's stable sorted() over dict order.
    ranked.sort_by_key(|&slot| rank_key(slot));
    for slot in ranked {
        if (palette.len() as i64) >= colors {
            break;
        }
        palette.push(palette_centroid(slot));
    }
    palette = fill_palette_by_weighted_residual(&bins, &palette, colors);
    Ok(PaletteInit {
        bins,
        palette,
        ladder,
        exact,
        exact_path: false,
    })
}

/// `pngprism._entry_premult`.
fn entry_premult(entry: Rgba) -> [i64; 4] {
    [
        i64::from(entry.3) * i64::from(entry.0),
        i64::from(entry.3) * i64::from(entry.1),
        i64::from(entry.3) * i64::from(entry.2),
        255 * i64::from(entry.3),
    ]
}

/// Nearest palette index in premultiplied space, restricted to the bin's
/// alpha zone (BINDING); ties -> lowest index
/// (`pngprism._nearest_entry`). One-directional capacity degradation:
/// a zone with no entry falls back to the nearest non-transparent entry
/// (the a==0 entry only when it is the ONLY entry).
fn nearest_entry(
    premult: [i64; 4],
    zone: u8,
    entries_premult: &[[i64; 4]],
    entry_zones: &[u8],
) -> Result<usize, Error> {
    let mut best: Option<usize> = None;
    let mut best_d = 0i64;
    for (j, ep) in entries_premult.iter().enumerate() {
        if entry_zones[j] != zone {
            continue;
        }
        let d2 = premult_dist_sq(premult, *ep);
        if best.is_none() || d2 < best_d {
            best = Some(j);
            best_d = d2;
        }
    }
    if let Some(best) = best {
        return Ok(best);
    }
    if zone == ZONE_TRANSPARENT {
        return Err(Error::internal(
            "internal: transparent bin without a transparent entry".to_string(),
        ));
    }
    let mut fallback: Option<usize> = None;
    let mut fallback_d = 0i64;
    for (j, ep) in entries_premult.iter().enumerate() {
        if entry_zones[j] == ZONE_TRANSPARENT && entries_premult.len() > 1 {
            continue;
        }
        let d2 = premult_dist_sq(premult, *ep);
        if fallback.is_none() || d2 < fallback_d {
            fallback = Some(j);
            fallback_d = d2;
        }
    }
    fallback.ok_or_else(|| Error::internal("internal: empty palette".to_string()))
}

/// v0.1 refinement seam (`pngprism.stage_refinement`): k-means-style
/// joint Lloyd in premultiplied space over the deterministic sample;
/// occupancy-weighted centroid updates; zone-constrained assignment;
/// fixed-point stop or REFINE_MAX_ITERS; empty entries re-seed to the
/// worst-served sample bin of their zone. Returns (palette, iterations_run).
fn stage_refinement(init: &PaletteInit, _colors: i64) -> Result<(Vec<Rgba>, usize), Error> {
    stage_refinement_with_parallelism(init, _colors, Parallelism::SEQUENTIAL)
}

type RefinementAccumulator = (Vec<[i64; 5]>, HashMap<u8, (i64, Rgba)>);

fn refinement_accumulate_parallel(
    sample: &[&Bin],
    sample_premult: &[[i64; 4]],
    entries_premult: &[[i64; 4]],
    entry_zones: &[u8],
    palette_len: usize,
    parallelism: Parallelism,
) -> Result<RefinementAccumulator, Error> {
    let partials = map_ranges(sample.len(), parallelism, |range| {
        let mut acc = vec![[0i64; 5]; palette_len];
        let mut worst: HashMap<u8, (i64, Rgba)> = HashMap::new();
        for index in range {
            let bin = sample[index];
            let assigned = nearest_entry(
                sample_premult[index],
                bin.zone,
                entries_premult,
                entry_zones,
            )?;
            acc[assigned][0] += bin.count;
            acc[assigned][1] += bin.sum_a;
            acc[assigned][2] += bin.sum_ar;
            acc[assigned][3] += bin.sum_ag;
            acc[assigned][4] += bin.sum_ab;
            let candidate = (
                premult_dist_sq(sample_premult[index], entries_premult[assigned]),
                bin_mean_color(bin),
            );
            match worst.get(&bin.zone) {
                None => {
                    worst.insert(bin.zone, candidate);
                }
                Some(&current)
                    if candidate.0 > current.0
                        || (candidate.0 == current.0
                            && pack_rgba(candidate.1) < pack_rgba(current.1)) =>
                {
                    worst.insert(bin.zone, candidate);
                }
                Some(_) => {}
            }
        }
        Ok((acc, worst))
    })?;
    let mut acc = vec![[0i64; 5]; palette_len];
    let mut worst: HashMap<u8, (i64, Rgba)> = HashMap::new();
    for (partial_acc, partial_worst) in partials {
        for (destination, source) in acc.iter_mut().zip(partial_acc) {
            for (destination_value, source_value) in destination.iter_mut().zip(source) {
                *destination_value += source_value;
            }
        }
        for (zone, candidate) in partial_worst {
            match worst.get(&zone) {
                None => {
                    worst.insert(zone, candidate);
                }
                Some(&current)
                    if candidate.0 > current.0
                        || (candidate.0 == current.0
                            && pack_rgba(candidate.1) < pack_rgba(current.1)) =>
                {
                    worst.insert(zone, candidate);
                }
                Some(_) => {}
            }
        }
    }
    Ok((acc, worst))
}

fn stage_refinement_with_parallelism(
    init: &PaletteInit,
    _colors: i64,
    parallelism: Parallelism,
) -> Result<(Vec<Rgba>, usize), Error> {
    let mut palette = init.palette.clone();
    if palette.is_empty() || init.bins.is_empty() || init.exact_path {
        return Ok((palette, 0));
    }
    let sample = refine_sample(&init.bins);
    let sample_premult: Vec<[i64; 4]> = sample.iter().map(|b| bin_premult_mean(b)).collect();
    let mut entry_zones: Vec<u8> = palette.iter().map(|e| zone_of(i64::from(e.3))).collect();
    let mut iterations = 0usize;
    for iteration in 1..=REFINE_MAX_ITERS {
        iterations = iteration;
        let entries_premult: Vec<[i64; 4]> = palette.iter().map(|&e| entry_premult(e)).collect();
        let (acc, worst) = if parallelism.is_parallel() {
            refinement_accumulate_parallel(
                &sample,
                &sample_premult,
                &entries_premult,
                &entry_zones,
                palette.len(),
                parallelism,
            )?
        } else {
            let mut assign = Vec::with_capacity(sample.len());
            for (i, b) in sample.iter().enumerate() {
                assign.push(nearest_entry(
                    sample_premult[i],
                    b.zone,
                    &entries_premult,
                    &entry_zones,
                )?);
            }
            let mut acc = vec![[0i64; 5]; palette.len()];
            for (i, b) in sample.iter().enumerate() {
                let j = assign[i];
                acc[j][0] += b.count;
                acc[j][1] += b.sum_a;
                acc[j][2] += b.sum_ar;
                acc[j][3] += b.sum_ag;
                acc[j][4] += b.sum_ab;
            }
            let mut worst: HashMap<u8, (i64, Rgba)> = HashMap::new();
            for (i, b) in sample.iter().enumerate() {
                let j = assign[i];
                let d2 = premult_dist_sq(sample_premult[i], entries_premult[j]);
                let mean = bin_mean_color(b);
                match worst.get(&b.zone) {
                    None => {
                        worst.insert(b.zone, (d2, mean));
                    }
                    Some(&(current_d2, current_mean)) => {
                        if d2 > current_d2
                            || (d2 == current_d2 && pack_rgba(mean) < pack_rgba(current_mean))
                        {
                            worst.insert(b.zone, (d2, mean));
                        }
                    }
                }
            }
            (acc, worst)
        };
        let mut new_palette: Vec<Rgba> = Vec::with_capacity(palette.len());
        let mut new_zones: Vec<u8> = Vec::with_capacity(palette.len());
        let mut moved = false;
        let mut zone_counts: HashMap<u8, i64> = HashMap::new();
        for &zone in &entry_zones {
            *zone_counts.entry(zone).or_insert(0) += 1;
        }
        for (j, entry) in palette.iter().enumerate() {
            let zone = entry_zones[j];
            let sums = acc[j];
            if zone == ZONE_TRANSPARENT {
                new_palette.push(*entry); // policy-locked; never drifts
                new_zones.push(zone);
                continue;
            }
            if sums[0] == 0 {
                let candidate = worst.get(&zone);
                let unserved = match candidate {
                    None => true,
                    Some(&(d2, _)) => d2 == 0,
                };
                // Never drop a zone's last entry (BINDING: remap must
                // always find an entry in every zone that still has bins).
                let zone_count = zone_counts.get(&zone).copied().unwrap_or(0);
                if unserved && zone_count > 1 {
                    if let Some(count) = zone_counts.get_mut(&zone) {
                        *count -= 1;
                    }
                    moved = true; // zone perfectly fit: drop the spare entry
                    continue;
                }
                if unserved {
                    new_palette.push(*entry);
                    new_zones.push(zone);
                    continue;
                }
                match candidate {
                    Some(&(_, mean)) => {
                        new_palette.push(mean);
                        new_zones.push(zone);
                        moved = true;
                    }
                    None => {
                        // Unreachable: `unserved` is false here only when
                        // `candidate` is `Some` (see the match above).
                        new_palette.push(*entry);
                        new_zones.push(zone);
                    }
                }
                continue;
            }
            let mut updated = centroid(sums[0], sums[1], sums[2], sums[3], sums[4]);
            if zone == ZONE_OPAQUE {
                updated.3 = 255; // lock pin
            }
            new_palette.push(updated);
            new_zones.push(zone);
            if updated != *entry {
                moved = true;
            }
        }
        palette = new_palette;
        entry_zones = new_zones;
        if !moved {
            break;
        }
    }
    Ok((palette, iterations))
}

/// v0.1 remapping seam (`pngprism.stage_remap`): EVERY histogram bin
/// (including any pair left uninstantiated — the A2 nearest-repair rule)
/// maps to its nearest entry within its alpha zone; pixels map through
/// their bin key.
fn stage_remap(pixels: &[Rgba], init: &PaletteInit, palette: &[Rgba]) -> Result<Vec<u8>, Error> {
    stage_remap_with_parallelism(pixels, init, palette, Parallelism::SEQUENTIAL)
}

fn stage_remap_with_parallelism(
    pixels: &[Rgba],
    init: &PaletteInit,
    palette: &[Rgba],
    parallelism: Parallelism,
) -> Result<Vec<u8>, Error> {
    if palette.is_empty() {
        return Ok(Vec::new());
    }
    let entries_premult: Vec<[i64; 4]> = palette.iter().map(|&e| entry_premult(e)).collect();
    let entry_zones: Vec<u8> = palette.iter().map(|e| zone_of(i64::from(e.3))).collect();
    // Key -> palette index. Lookup-only map: every pixel's key is present
    // by construction (the pixel contributed to that bin).
    let mut assignment: HashMap<(u8, u8, u8, u8), u8> = HashMap::with_capacity(init.bins.len());
    if parallelism.is_parallel() {
        let partials = map_ranges(init.bins.len(), parallelism, |range| {
            let mut values = Vec::with_capacity(range.len());
            for bin in &init.bins[range] {
                let index = nearest_entry(
                    bin_premult_mean(bin),
                    bin.zone,
                    &entries_premult,
                    &entry_zones,
                )?;
                values.push((bin.key, index as u8));
            }
            Ok(values)
        })?;
        for partial in partials {
            assignment.extend(partial);
        }
    } else {
        for bin in &init.bins {
            let index = nearest_entry(
                bin_premult_mean(bin),
                bin.zone,
                &entries_premult,
                &entry_zones,
            )?;
            assignment.insert(bin.key, index as u8);
        }
    }
    // `.get(...).ok_or_else(...)` rather than `assignment[&key]`: every
    // pixel's key is present by construction (the pixel contributed to that
    // bin, under the same exact/preclip keying `build_bins` used), but this
    // keeps the lookup a typed internal error instead of a panic-capable
    // index if that construction were ever violated by a future bug.
    let missing =
        || Error::internal("internal: pixel key missing from remap assignment".to_string());
    let mut indices = Vec::with_capacity(pixels.len());
    if parallelism.is_parallel() {
        let partials = map_ranges(pixels.len(), parallelism, |range| {
            let mut values = Vec::with_capacity(range.len());
            for &(r, g, b, a) in &pixels[range] {
                let key = if init.exact {
                    (r, g, b, a)
                } else {
                    preclip_key(r, g, b, a, PRECLIP_LEVELS)
                };
                values.push(*assignment.get(&key).ok_or_else(missing)?);
            }
            Ok(values)
        })?;
        for partial in partials {
            indices.extend(partial);
        }
    } else if init.exact {
        for &pixel in pixels {
            indices.push(*assignment.get(&pixel).ok_or_else(missing)?);
        }
    } else {
        let levels = PRECLIP_LEVELS;
        for &(r, g, b, a) in pixels {
            let key = preclip_key(r, g, b, a, levels);
            indices.push(*assignment.get(&key).ok_or_else(missing)?);
        }
    }
    Ok(indices)
}

/// v0.1 emission seam (`pngprism.stage_emit`): deterministic indexed
/// PNG (tRNS when needed). The oracle lets a writer error escape
/// uncaught (unreachable for a pipeline-produced palette); this port
/// surfaces it as an internal error instead — also unreachable.
fn stage_emit(width: u32, height: u32, palette: &[Rgba], indices: &[u8]) -> Result<Vec<u8>, Error> {
    png::write_indexed_png(width, height, palette, indices)
        .map_err(|err| Error::internal(format!("internal: emit failed: {err}")))
}

/// Run the unchanged v0.1 core through its palette and remap seams
/// (`pngprism.quantize_candidate`): returns (palette, per-pixel index map,
/// stage notes). Indices fit `u8` (palette entries <= 256).
///
/// `quantize_png` already validates `colors` against `1..=MAX_COLORS` before
/// reaching this seam; this crate-public entry point is reachable directly
/// (bypassing that CLI-level gate), so the same range is re-checked here.
/// Without it, an extreme caller-supplied `colors` (e.g. `i64::MAX`) would
/// reach `fit_rgb_reps`'s `cap + levels_or_one - 1` addition and overflow —
/// a data-path panic on a public parameter, exactly what ch17 §31 forbids.
pub fn quantize_candidate(
    source: &DecodedImage,
    colors: i64,
    hidden_rgb_policy: &str,
) -> Result<(Vec<Rgba>, Vec<u8>, StageNotes), Error> {
    quantize_candidate_with_color_space(source, colors, hidden_rgb_policy, DEFAULT_COLOR_SPACE)
}

/// Run the quantizer with an explicit assignment/refinement/remap color
/// space (`pngprism.quantize_candidate(..., color_space=...)`).
pub fn quantize_candidate_with_color_space(
    source: &DecodedImage,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
) -> Result<(Vec<Rgba>, Vec<u8>, StageNotes), Error> {
    quantize_candidate_with_parallelism(
        source,
        colors,
        hidden_rgb_policy,
        color_space,
        Parallelism::SEQUENTIAL,
    )
}

/// Run the quantizer under an explicit execution schedule. One thread is the
/// unchanged behavioral oracle; Oklab floating-point reductions stay serial.
pub fn quantize_candidate_with_parallelism(
    source: &DecodedImage,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    parallelism: Parallelism,
) -> Result<(Vec<Rgba>, Vec<u8>, StageNotes), Error> {
    if !(1..=MAX_COLORS).contains(&colors) {
        return Err(Error::data(format!("colors must be in 1..={MAX_COLORS}")));
    }
    if !COLOR_SPACES.contains(&color_space) {
        return Err(Error::data(format!("unknown color-space: {color_space}")));
    }
    let sampled = stage_sample(&source.pixels);
    let init = if parallelism.is_parallel() {
        stage_palette_init_with_parallelism(sampled, colors, hidden_rgb_policy, parallelism)?
    } else {
        stage_palette_init(sampled, colors, hidden_rgb_policy)?
    };
    let (palette, indices, iterations) = if color_space == "srgb" {
        let (palette, iterations) = if parallelism.is_parallel() {
            stage_refinement_with_parallelism(&init, colors, parallelism)?
        } else {
            stage_refinement(&init, colors)?
        };
        let indices = if parallelism.is_parallel() {
            stage_remap_with_parallelism(sampled, &init, &palette, parallelism)?
        } else {
            stage_remap(sampled, &init, &palette)?
        };
        (palette, indices, iterations)
    } else {
        let feature_by_key = oklab_feature_bins(sampled, &init)?;
        let (palette, iterations) = refine_oklab(&init, &feature_by_key)?;
        let indices = remap_oklab(sampled, &init, &palette, &feature_by_key)?;
        (palette, indices, iterations)
    };
    let nonopaque = source.pixels.iter().filter(|pixel| pixel.3 < 255).count();
    let alpha_note = if nonopaque > 0 {
        "alpha preserved via tRNS (extremes exact; interior quantized)"
    } else {
        "source fully opaque; no tRNS emitted"
    };
    let notes = StageNotes {
        sampled_pixels: sampled.len(),
        initial_bins: init.bins.len(),
        refined_palette_entries: palette.len(),
        alpha_note: alpha_note.to_string(),
        exact_path: init.exact_path,
        palette_init_pairs: init.palette.len(),
        refinement_iterations: iterations,
        hidden_rgb_policy: hidden_rgb_policy.to_string(),
    };
    Ok((palette, indices, notes))
}

/// Run the v0.1 pipeline (core + emit) over one decoded image
/// (`pngprism.quantize_image`): returns (output PNG bytes, palette, notes).
pub fn quantize_image(
    source: &DecodedImage,
    colors: i64,
    hidden_rgb_policy: &str,
) -> Result<(Vec<u8>, Vec<Rgba>, StageNotes), Error> {
    quantize_image_with_color_space(source, colors, hidden_rgb_policy, DEFAULT_COLOR_SPACE)
}

/// Run the emitted-image pipeline with an explicit quantization color space.
pub fn quantize_image_with_color_space(
    source: &DecodedImage,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
) -> Result<(Vec<u8>, Vec<Rgba>, StageNotes), Error> {
    let (palette, indices, notes) =
        quantize_candidate_with_color_space(source, colors, hidden_rgb_policy, color_space)?;
    let output = stage_emit(source.width, source.height, &palette, &indices)?;
    Ok((output, palette, notes))
}

/// Decode, run the v0.1 core, opt into dither/pack, self-verify, and write
/// (`pngprism.quantize_png`). Validation order mirrors the oracle: colors
/// range, policy, dither-policy, pack, pack-search, strength, composition
/// guards (all data_error / exit 3 when reached here; `main` catches the
/// composition/vocabulary cases first as usage errors), then read (io_error),
/// decode (data_error). The emitted bytes are re-decoded before publication.
// ch17 §31/lint posture: deliberate — `quantize_png` mirrors the oracle's
// `quantize_png(in_path, out_path, colors, hidden_rgb_policy, dither,
// dither_strength, dither_policy, pack_mode, pack_search)` positional
// signature one-for-one; grouping these into an options
// struct is tracked as follow-up work (tri-review action 7 — an options
// surface is a prerequisite for the §32 candidate-set API), not a docs/
// lint/ops-hygiene change. `#[expect]` (not `#[allow]`) so this site stops
// compiling clean, and starts failing loudly, the moment that follow-up
// actually removes the extra arguments.
#[expect(
    clippy::too_many_arguments,
    reason = "options-struct refactor is tracked follow-up work (action 7), not part of this docs/lint/ops pass"
)]
pub fn quantize_png(
    in_path: &Path,
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    dither: bool,
    dither_strength: (i64, i64),
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
) -> Result<Summary, Error> {
    quantize_png_with_color_space(
        in_path,
        out_path,
        colors,
        hidden_rgb_policy,
        DEFAULT_COLOR_SPACE,
        dither,
        dither_strength,
        dither_policy,
        pack_mode,
        pack_search,
    )
}

/// Integrated pipeline with an explicit quantization color-space policy.
#[expect(
    clippy::too_many_arguments,
    reason = "faithfully mirrors the oracle CLI surface; options refactor remains tracked separately"
)]
pub fn quantize_png_with_color_space(
    in_path: &Path,
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    dither: bool,
    dither_strength: (i64, i64),
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
) -> Result<Summary, Error> {
    quantize_png_with_adaptive_default(
        in_path,
        out_path,
        colors,
        hidden_rgb_policy,
        color_space,
        DEFAULT_ADAPTIVE_DEFAULT,
        dither,
        dither_strength,
        false,
        dither_policy,
        pack_mode,
        pack_search,
    )
}

/// Integrated pipeline with T-0161's switchable adaptive-unit default.
/// `adaptive_default = false` preserves the historical path; when true, the
/// oracle requires every explicit dither value to remain at its default and
/// selects adaptive-unit internally. `dither_strength_explicit` preserves the
/// CLI distinction between a predicted adaptive-unit strength and an explicit
/// full-strength `1.0`.
#[expect(
    clippy::too_many_arguments,
    reason = "faithfully mirrors the live oracle's adaptive-default function surface"
)]
pub fn quantize_png_with_adaptive_default(
    in_path: &Path,
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    adaptive_default: bool,
    dither: bool,
    dither_strength: (i64, i64),
    dither_strength_explicit: bool,
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
) -> Result<Summary, Error> {
    quantize_png_with_parallelism(
        in_path,
        out_path,
        colors,
        hidden_rgb_policy,
        color_space,
        AdaptiveDefault::from_bool(adaptive_default),
        dither,
        dither_strength,
        dither_strength_explicit,
        dither_policy,
        pack_mode,
        pack_search,
        None,
        None,
        None,
        Parallelism::SEQUENTIAL,
    )
}

/// Integrated pipeline under an explicit opt-in execution schedule.
#[expect(
    clippy::too_many_arguments,
    reason = "faithfully mirrors the live oracle surface plus one execution schedule"
)]
pub fn quantize_png_with_parallelism(
    in_path: &Path,
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    adaptive_default: AdaptiveDefault,
    dither: bool,
    dither_strength: (i64, i64),
    dither_strength_explicit: bool,
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
    pack_seam_palette_sort: Option<bool>,
    pack_seam_memlevel: Option<bool>,
    pack_seam_reduction: Option<bool>,
    parallelism: Parallelism,
) -> Result<Summary, Error> {
    quantize_png_with_parallelism_impl(
        in_path,
        None,
        out_path,
        colors,
        hidden_rgb_policy,
        color_space,
        adaptive_default,
        dither,
        dither_strength,
        dither_strength_explicit,
        dither_policy,
        pack_mode,
        pack_search,
        pack_seam_palette_sort,
        pack_seam_memlevel,
        pack_seam_reduction,
        parallelism,
    )
}

/// Integrated pipeline over an already-bounded source snapshot.
///
/// The CLI uses this entry point so its never-worse snapshot is also the
/// bytes decoded by the candidate pipeline: one source identity and one
/// retained compressed-input allocation. `source_path` is diagnostic-only;
/// all output bytes are identical to [`quantize_png_with_parallelism`].
#[expect(
    clippy::too_many_arguments,
    reason = "faithfully mirrors the live oracle surface plus one execution schedule"
)]
pub fn quantize_png_bytes_with_parallelism(
    source_path: &Path,
    raw: &[u8],
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    adaptive_default: AdaptiveDefault,
    dither: bool,
    dither_strength: (i64, i64),
    dither_strength_explicit: bool,
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
    pack_seam_palette_sort: Option<bool>,
    pack_seam_memlevel: Option<bool>,
    pack_seam_reduction: Option<bool>,
    parallelism: Parallelism,
) -> Result<Summary, Error> {
    quantize_png_with_parallelism_impl(
        source_path,
        Some(raw),
        out_path,
        colors,
        hidden_rgb_policy,
        color_space,
        adaptive_default,
        dither,
        dither_strength,
        dither_strength_explicit,
        dither_policy,
        pack_mode,
        pack_search,
        pack_seam_palette_sort,
        pack_seam_memlevel,
        pack_seam_reduction,
        parallelism,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "single implementation shared by path and preloaded-byte entry points"
)]
fn quantize_png_with_parallelism_impl(
    in_path: &Path,
    preloaded_raw: Option<&[u8]>,
    out_path: &Path,
    colors: i64,
    hidden_rgb_policy: &str,
    color_space: &str,
    adaptive_default: AdaptiveDefault,
    dither: bool,
    dither_strength: (i64, i64),
    dither_strength_explicit: bool,
    dither_policy: &str,
    pack_mode: &str,
    pack_search: &str,
    pack_seam_palette_sort: Option<bool>,
    pack_seam_memlevel: Option<bool>,
    pack_seam_reduction: Option<bool>,
    parallelism: Parallelism,
) -> Result<Summary, Error> {
    let (dither, dither_policy) = match adaptive_default {
        AdaptiveDefault::On | AdaptiveDefault::Guarded => {
            if dither != DEFAULT_DITHER
                || dither_strength != DEFAULT_DITHER_STRENGTH
                || dither_strength_explicit
                || dither_policy != DEFAULT_DITHER_POLICY
            {
                return Err(Error::data(format!(
                    "--adaptive-default {} is not composable with explicit dither options",
                    adaptive_default.as_str()
                )));
            }
            (true, "adaptive-unit")
        }
        AdaptiveDefault::Off => (dither, dither_policy),
    };
    if !(1..=MAX_COLORS).contains(&colors) {
        return Err(Error::data(format!("--colors must be in 1..{MAX_COLORS}")));
    }
    if !HIDDEN_RGB_POLICIES.contains(&hidden_rgb_policy) {
        return Err(Error::data(format!(
            "--hidden-rgb-policy must be one of {}",
            HIDDEN_RGB_POLICIES.join(", ")
        )));
    }
    if !COLOR_SPACES.contains(&color_space) {
        return Err(Error::data(format!(
            "--color-space must be one of {}",
            COLOR_SPACES.join(", ")
        )));
    }
    if !DITHER_POLICIES.contains(&dither_policy) {
        return Err(Error::data(format!(
            "--dither-policy must be one of {}",
            DITHER_POLICIES.join(", ")
        )));
    }
    if !PACK_MODES.contains(&pack_mode) {
        return Err(Error::data(format!(
            "--pack must be one of {}",
            PACK_MODES.join(", ")
        )));
    }
    if !PACK_SEARCHES.contains(&pack_search) {
        return Err(Error::data(format!(
            "--pack-search must be one of {}",
            PACK_SEARCHES.join(", ")
        )));
    }
    // E-0036/E-0040 pack-seam composition (`pngprism.quantize_png`):
    //   * `--pack fast|max` + any seam explicitly ON -> usage error (the
    //     packer runs its own byte search).
    //   * `--pack none`, no seam flag named -> adopted omission defaults
    //     (S on, M off, R on).
    //   * `--pack none`, any seam flag named -> unspecified peers stay at the
    //     E-0036 default-off, so every pre-adoption explicit invocation keeps
    //     its exact bytes.
    //   * `--pack fast|max` -> all seams resolve off (composition gate retained).
    let requested_seams = [
        pack_seam_palette_sort,
        pack_seam_memlevel,
        pack_seam_reduction,
    ];
    if pack_mode != "none" && requested_seams.contains(&Some(true)) {
        return Err(Error::data(
            "--pack-seam-* flags apply to the pack=none emission path only \
             (--pack fast/max runs its own byte search)"
                .to_string(),
        ));
    }
    let seam_explicit = requested_seams.iter().any(Option::is_some);
    let (seam_palette_sort, seam_memlevel, seam_reduction) =
        if pack_mode == "none" && !seam_explicit {
            (
                DEFAULT_PACK_SEAM_PALETTE_SORT,
                DEFAULT_PACK_SEAM_MEMLEVEL,
                DEFAULT_PACK_SEAM_REDUCTION,
            )
        } else if pack_mode == "none" {
            (
                pack_seam_palette_sort.unwrap_or(false),
                pack_seam_memlevel.unwrap_or(false),
                pack_seam_reduction.unwrap_or(false),
            )
        } else {
            (false, false, false)
        };
    let seam_on = seam_palette_sort || seam_memlevel || seam_reduction;
    if dither_strength.0 < 0 || dither_strength.1 <= 0 || dither_strength.0 > dither_strength.1 {
        return Err(Error::data(
            "--dither-strength must be an exact ratio in 0..1".to_string(),
        ));
    }
    if (dither_policy == "adaptive"
        || dither_policy == "region"
        || dither_policy == "luma-bluenoise")
        && !dither
    {
        return Err(Error::data(format!(
            "--dither-policy {dither_policy} requires --dither on"
        )));
    }
    if (dither_policy == "adaptive" || dither_policy == "region") && dither_strength != (1, 1) {
        return Err(Error::data(format!(
            "--dither-strength is not composable with --dither-policy {dither_policy} (policy supplies exact strengths)"
        )));
    }
    // Preserve validation-before-I/O for the path API while allowing the CLI
    // to reuse its retained never-worse snapshot without a second read/copy.
    let owned_raw = if preloaded_raw.is_none() {
        Some(png::read_png_file(in_path)?)
    } else {
        None
    };
    let raw = preloaded_raw
        .or(owned_raw.as_deref())
        .expect("one raw source is always present");
    let source = png::decode_png(raw).map_err(|err| {
        Error::data(format!(
            "data_error: cannot decode {}: {err}",
            in_path.display()
        ))
    })?;
    let width = source.width as usize;
    let height = source.height as usize;
    let (palette, cand_indices, notes) = quantize_candidate_with_parallelism(
        &source,
        colors,
        hidden_rgb_policy,
        color_space,
        parallelism,
    )?;

    // Guarded adaptive default (T-0190/E-0038): when policy == guarded and the
    // E-0032 structural guard fires, disable dither (reverting to the plain
    // core remap == the frozen `off` bytes). The accepted Option-A predicate is
    // E-0032's four-decimal `opaque_frac == 0.0000`, computed integer-exact:
    // `round(opaque_count / total, 4) == 0.0` holds iff `opaque_count * 20000 <
    // total` (verified byte-for-byte against CPython's banker's-rounded `round`;
    // the exact 1/20000 boundary rounds to 0.0001, so the comparison is strict).
    // No float divergence is possible.
    let dither = if adaptive_default == AdaptiveDefault::Guarded {
        let opaque_count = source.pixels.iter().filter(|pixel| pixel.3 == 255).count();
        if adaptive_guard_fires(opaque_count, source.pixels.len()) {
            false
        } else {
            dither
        }
    } else {
        dither
    };

    // Dither (opt-in) yields a new index map; else keep the core's remap.
    let map_dither =
        |err: Error| Error::data(format!("data_error: cannot dither candidate: {}", err));
    let indices: Vec<usize> = if dither && dither_policy == "luma-bluenoise" {
        dither::luma_bluenoise_remap(
            &source.pixels,
            width,
            height,
            &palette,
            colors,
            dither_strength,
        )?
    } else if dither {
        let directives = match dither_policy {
            "adaptive" => {
                dither::adaptive_strength_directives(&source.pixels, width, height, &palette)
                    .map_err(map_dither)?
            }
            "region" => dither::region_policy_directives(&source.pixels, width, height, &palette)
                .map_err(map_dither)?,
            "adaptive-unit" => {
                let effective_strength = if dither_strength_explicit {
                    dither_strength
                } else {
                    dither::adaptive_unit_strength(&source.pixels, width, height, &palette)
                        .map_err(map_dither)?
                };
                if effective_strength == (1, 1) {
                    dither::stub_directives(source.pixels.len())
                } else {
                    dither::uniform_strength_directives(effective_strength, source.pixels.len())
                }
            }
            _ => {
                // The exact T-0080 path: the historical region_hook=None
                // full-strength fast path, else a uniform-strength table.
                if dither_strength == (1, 1) {
                    dither::stub_directives(source.pixels.len())
                } else {
                    dither::uniform_strength_directives(dither_strength, source.pixels.len())
                }
            }
        };
        dither::floyd_steinberg(&source.pixels, width, height, &palette, &directives)
            .map_err(map_dither)?
    } else {
        cand_indices.iter().map(|&i| usize::from(i)).collect()
    };

    // Pack (opt-in), the E-0036 pack-seam emitter (pack=none), or the plain
    // deterministic indexed emitter.
    let output: Vec<u8> = if pack_mode == "none" {
        if seam_on {
            pack::seam_emit(
                width,
                height,
                &palette,
                &indices,
                seam_palette_sort,
                seam_memlevel,
                seam_reduction,
            )?
        } else {
            let idx_u8: Vec<u8> = indices.iter().map(|&i| i as u8).collect();
            stage_emit(source.width, source.height, &palette, &idx_u8)?
        }
    } else {
        pack::pack_indexed_png_with_parallelism(
            width,
            height,
            &palette,
            &indices,
            pack_mode,
            pack_search,
            parallelism,
        )
        .map_err(|err| Error::data(format!("data_error: {}", err)))?
    };

    // Self-verification: the emitted bytes must re-decode to the declared
    // dimensions, a palette within budget, and exactly the remap pixels.
    let check = png::decode_png(&output).map_err(|err| {
        Error::internal(format!("internal: emitted PNG failed self-decode: {err}"))
    })?;
    if (check.width, check.height) != (source.width, source.height) {
        return Err(Error::internal(
            "internal: emitted dimensions differ from source".to_string(),
        ));
    }
    let plte_len = check.properties.plte.as_ref().map_or(0, Vec::len);
    if plte_len as i64 > colors {
        return Err(Error::internal(
            "internal: emitted palette exceeds --colors".to_string(),
        ));
    }
    let expected: Vec<Rgba> = indices.iter().map(|&i| palette[i]).collect();
    if check.pixels != expected {
        return Err(Error::internal(
            "internal: emitted pixels differ from remap candidate".to_string(),
        ));
    }
    std::fs::write(out_path, &output).map_err(|err| {
        Error::io(format!(
            "io_error: cannot write {}: {err}",
            out_path.display()
        ))
    })?;
    Ok(Summary {
        version: VERSION,
        label: LABEL,
        colors,
        hidden_rgb_policy: hidden_rgb_policy.to_string(),
        color_space: color_space.to_string(),
        source_bytes: raw.len(),
        output_bytes: output.len(),
        palette_entries: plte_len,
        stages: notes,
    })
}

#[cfg(test)]
mod capacity_rebalance_tests {
    use super::*;

    fn capacity_fixture() -> Vec<Rgba> {
        let mut pixels = Vec::new();
        for i in 0u8..12 {
            pixels.push((i * 9, 20 + i, 240 - i * 7, 0));
        }
        for i in 0u8..12 {
            pixels.push((i * 11, 30 + i * 3, 220 - i * 5, 40 + i * 8));
        }
        for i in 0u8..12 {
            pixels.push((i * 13, 210 - i * 4, 10 + i * 5, 255));
        }
        pixels
    }

    #[test]
    fn weighted_residual_fill_reaches_cap_without_duplicates() {
        let (bins, _) = build_bins(&capacity_fixture());
        let initial = [(0, 0, 0, 0), (20, 30, 40, 80), (50, 60, 70, 255)];

        let filled = fill_palette_by_weighted_residual(&bins, &initial, 16);

        assert_eq!(filled.len(), 16);
        assert_eq!(filled.iter().copied().collect::<HashSet<_>>().len(), 16);
    }

    #[test]
    fn weighted_residual_fill_never_adds_second_transparent_entry() {
        let (bins, _) = build_bins(&capacity_fixture());
        let initial = [(0, 0, 0, 0), (20, 30, 40, 80), (50, 60, 70, 255)];

        let filled = fill_palette_by_weighted_residual(&bins, &initial, 16);

        assert_eq!(filled.iter().filter(|entry| entry.3 == 0).count(), 1);
    }

    #[test]
    fn weighted_residual_prefers_higher_occupancy_at_equal_distance() {
        let mut pixels = vec![(10, 0, 0, 255); 20];
        pixels.extend(vec![(0, 10, 0, 255); 2]);
        let (bins, _) = build_bins(&pixels);

        let filled = fill_palette_by_weighted_residual(&bins, &[(0, 0, 0, 255)], 2);

        assert_eq!(filled[1], (10, 0, 0, 255));
    }

    #[test]
    fn weighted_residual_fill_is_deterministic() {
        let (bins, _) = build_bins(&capacity_fixture());
        let initial = [(0, 0, 0, 0), (20, 30, 40, 80), (50, 60, 70, 255)];

        let first = fill_palette_by_weighted_residual(&bins, &initial, 16);
        let second = fill_palette_by_weighted_residual(&bins, &initial, 16);

        assert_eq!(first, second);
    }
}

#[cfg(test)]
mod oklab_tests {
    use super::*;

    #[test]
    fn oklab_forward_transform_matches_python_binary64_bits() {
        let cases = [
            ((0, 0, 0), [0x0, 0x0, 0x0]),
            (
                (255, 255, 255),
                [
                    0x3fef_ffff_fc7f_02d2,
                    0x3dd6_408d_0000_0000,
                    0x3e64_02e3_0900_0000,
                ],
            ),
            (
                (255, 0, 0),
                [
                    0x3fe4_1835_d725_ff10,
                    0x3fcc_c850_12ad_abec,
                    0x3fc0_1bbb_4441_8e76,
                ],
            ),
            (
                (12, 128, 244),
                [
                    0x3fe3_7621_d6c0_413d,
                    0xbfa9_e481_668b_bbe0,
                    0xbfc8_4955_575f_e98c,
                ],
            ),
        ];
        for (rgb, expected) in cases {
            let actual = srgb8_to_oklab(rgb);
            assert_eq!(
                [actual.0.to_bits(), actual.1.to_bits(), actual.2.to_bits()],
                expected,
                "rgb={rgb:?}"
            );
        }
    }

    #[test]
    fn oklab_premultiplication_matches_python_binary64_bits() {
        let feature = premultiplied_oklab_feature((12, 128, 244, 127));

        assert_eq!(
            feature.map(f64::to_bits),
            [
                0x3fd3_6298_2b3d_feb9,
                0xbf99_ca82_e6a5_49b2,
                0xbfb8_30f3_a051_7b34,
                0x3fdf_dfdf_dfdf_dfe0,
            ]
        );
    }

    #[test]
    fn oklab_distance_uses_python_pow_rounding() {
        // Python 3.14: float.fromhex("0x1.4513f6f629aa0p-2") ** 2.
        // Multiplication produces the adjacent bit pattern ...91e instead.
        let delta = f64::from_bits(0x3fd4_513f_6f62_9aa0);
        let distance = oklab_distance_sq([delta, 0.0, 0.0, 0.0], [0.0; 4]);
        assert_eq!(distance.to_bits(), 0x3fb9_ccbb_29b9_c91d);
    }

    #[test]
    fn oklab_centroid_matches_python_half_boundary() {
        let centroid = oklab_centroid_from_feature_sums(
            256,
            65_280,
            f64::from_bits(0x403f_937f_27ae_9a00),
            f64::from_bits(0xbffd_0ba2_2dcd_b131),
            f64::from_bits(0x4019_e2b3_6a50_53b4),
        );
        assert_eq!(centroid, (7, 7, 0, 255));
    }

    #[test]
    fn nearest_oklab_entry_keeps_first_equal_distance() {
        let entries = [[0.25, 0.0, 0.0, 1.0], [0.25, 0.0, 0.0, 1.0]];

        let index =
            nearest_oklab_entry([0.5, 0.0, 0.0, 1.0], ZONE_OPAQUE, &entries, &[2, 2]).unwrap();

        assert_eq!(index, 0);
    }

    #[test]
    fn explicit_srgb_candidate_is_the_default_candidate() {
        let source = DecodedImage {
            width: 4,
            height: 1,
            pixels: vec![
                (0, 0, 0, 0),
                (12, 128, 244, 64),
                (250, 20, 10, 200),
                (255, 255, 255, 255),
            ],
            properties: png::Properties {
                color_type: 6,
                bit_depth: 8,
                interlaced: false,
                plte: None,
                trns: None,
                gama: None,
                iccp: None,
                conversions: Vec::new(),
            },
        };

        assert_eq!(
            quantize_candidate(&source, 3, DEFAULT_HIDDEN_RGB_POLICY).unwrap(),
            quantize_candidate_with_color_space(&source, 3, DEFAULT_HIDDEN_RGB_POLICY, "srgb")
                .unwrap()
        );
    }

    #[test]
    fn collapsed_preclip_does_not_claim_the_exact_path() {
        // 32,769 distinct colors cross EXACT_BIN_LIMIT, but collapse to no
        // more than 256 preclip bins. The live oracle's correctness guard
        // still requires the clustered/refinement path.
        let pixels: Vec<Rgba> = (0..32_769u32)
            .map(|value| ((value % 256) as u8, ((value / 256) % 256) as u8, 0, 255))
            .collect();
        let init = stage_palette_init(&pixels, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        assert!(!init.exact);
        assert!(!init.exact_path);
    }
}

#[cfg(test)]
mod parallelism_tests {
    use super::*;
    use crate::parallel::shard_ranges;

    const MERGE_ORDERS: [MergeOrder; 5] = [
        MergeOrder::Forward,
        MergeOrder::Reverse,
        MergeOrder::Balanced,
        MergeOrder::Shuffled(0x5eed_0172),
        MergeOrder::Shuffled(0xc0de_0042),
    ];
    const SHARD_COUNTS: [usize; 5] = [1, 2, 3, 7, 64];

    fn distinct_pixels(count: u32) -> Vec<Rgba> {
        (0..count)
            .map(|value| {
                (
                    (value >> 24) as u8,
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    value as u8,
                )
            })
            .collect()
    }

    fn schedule(threads: usize, order: MergeOrder) -> Parallelism {
        Parallelism::new(threads).unwrap().with_merge_order(order)
    }

    #[test]
    fn histogram_twins_vary_shards_and_merge_order_at_spill_edges() {
        let fixtures = [
            vec![(1, 2, 3, 255), (1, 2, 3, 255), (9, 8, 7, 0)],
            distinct_pixels(EXACT_BIN_LIMIT as u32),
            distinct_pixels(EXACT_BIN_LIMIT as u32 + 1),
            distinct_pixels(50_000),
        ];
        for pixels in &fixtures {
            let expected = build_bins(pixels);
            for threads in SHARD_COUNTS {
                for order in MERGE_ORDERS {
                    let actual = build_bins_parallel(pixels, schedule(threads, order)).unwrap();
                    assert_eq!(actual, expected, "threads={threads}, order={order:?}");
                }
            }
        }
    }

    #[test]
    fn distributed_excess_requires_m2_after_locally_exact_shards() {
        let pixels = distinct_pixels(EXACT_BIN_LIMIT as u32 + 1);
        let expected = build_bins(&pixels);
        assert!(!expected.1);
        for threads in [2, 3, 7, 64] {
            let ranges = shard_ranges(pixels.len(), threads);
            let states: Vec<_> = ranges
                .iter()
                .map(|range| histogram_state(&pixels[range.clone()]))
                .collect();
            assert!(
                states.iter().all(|state| state.exact),
                "distributed fixture spilled locally at {threads} shards"
            );
            for order in MERGE_ORDERS {
                let actual = bins_from_state(reduce_histogram_states(
                    ranges
                        .iter()
                        .map(|range| histogram_state(&pixels[range.clone()]))
                        .collect(),
                    order,
                ));
                assert_eq!(actual, expected, "threads={threads}, order={order:?}");
            }
        }

        let midpoint = pixels.len() / 2;
        let left = histogram_state(&pixels[..midpoint]);
        let right = histogram_state(&pixels[midpoint..]);
        assert!(left.exact && right.exact);
        assert!(!merge_histogram_states(left, right).exact);
    }

    #[test]
    fn concentrated_excess_exercises_mixed_m1_merge() {
        let mut pixels = distinct_pixels(EXACT_BIN_LIMIT as u32 + 1);
        pixels.extend(std::iter::repeat_n((0, 0, 0, 0), EXACT_BIN_LIMIT + 1));
        let midpoint = pixels.len() / 2;
        let left = histogram_state(&pixels[..midpoint]);
        let right = histogram_state(&pixels[midpoint..]);
        assert!(!left.exact && right.exact);
        let actual = bins_from_state(merge_histogram_states(left, right));
        assert_eq!(actual, build_bins(&pixels));
    }

    #[test]
    fn srgb_candidate_twins_cover_refinement_and_remap_schedules() {
        let width = 96u32;
        let height = 64u32;
        let pixels: Vec<Rgba> = (0..width * height)
            .map(|value| {
                (
                    value.wrapping_mul(17) as u8,
                    value.wrapping_mul(43).wrapping_add(value / width) as u8,
                    value.wrapping_mul(97).wrapping_add(value % width) as u8,
                    value.wrapping_mul(29) as u8,
                )
            })
            .collect();
        let source = DecodedImage {
            width,
            height,
            pixels,
            properties: png::Properties {
                color_type: 6,
                bit_depth: 8,
                interlaced: false,
                plte: None,
                trns: None,
                gama: None,
                iccp: None,
                conversions: Vec::new(),
            },
        };
        let expected = quantize_candidate(&source, 16, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        for threads in SHARD_COUNTS {
            for order in MERGE_ORDERS {
                let actual = quantize_candidate_with_parallelism(
                    &source,
                    16,
                    DEFAULT_HIDDEN_RGB_POLICY,
                    DEFAULT_COLOR_SPACE,
                    schedule(threads, order),
                )
                .unwrap();
                assert_eq!(actual, expected, "threads={threads}, order={order:?}");
            }
        }
    }

    #[test]
    fn parallel_schedule_race_soak_is_byte_stable() {
        let pixels = distinct_pixels(12_000);
        let expected = build_bins(&pixels);
        for repetition in 0..20 {
            for order in MERGE_ORDERS {
                let actual = build_bins_parallel(&pixels, schedule(7, order)).unwrap();
                assert_eq!(actual, expected, "repetition={repetition}, order={order:?}");
            }
        }
    }
}

#[cfg(test)]
mod guard_predicate_tests {
    use super::adaptive_guard_fires;

    /// Reference model of the oracle predicate using f64 `round`-half-even at
    /// 4 places, matching CPython's `round(x, 4) == 0.0`.
    fn oracle_fires(opaque_count: u64, total: u64) -> bool {
        // Rust f64 has no decimal `round(x, 4)`; reproduce banker's rounding at
        // 1e-4 exactly the way the boundary analysis derived it: multiply by
        // 10000, round-half-to-even to an integer, and test == 0. But we must
        // round the DECIMAL value of the double `opaque_count/total`, so mirror
        // the strict-boundary result the empirical sweep proved: fires iff the
        // true ratio is strictly below 1/20000.
        let ratio = opaque_count as f64 / total as f64;
        // round to 4 decimals, half-to-even (CPython semantics for this range)
        let scaled = ratio * 10000.0;
        let floor = scaled.floor();
        let frac = scaled - floor;
        let rounded = match frac.partial_cmp(&0.5).expect("finite ratio") {
            std::cmp::Ordering::Greater => floor + 1.0,
            std::cmp::Ordering::Less => floor,
            // exact half -> round to even
            std::cmp::Ordering::Equal if (floor as i64) % 2 == 0 => floor,
            std::cmp::Ordering::Equal => floor + 1.0,
        };
        rounded == 0.0
    }

    #[test]
    fn guard_matches_oracle_on_the_exact_boundary_family() {
        // opaque_count * 20000 == total is the exact 1/20000 boundary; the
        // oracle rounds it UP (does not fire), so the predicate is strict.
        for k in 1..=64u64 {
            let total = 20000 * k;
            assert!(
                !adaptive_guard_fires(k as usize, total as usize),
                "exact 1/20000 boundary (k={k}) must NOT fire"
            );
            assert!(
                adaptive_guard_fires((k - 1) as usize, total as usize),
                "just below the boundary (k={k}) must fire"
            );
            assert!(
                !adaptive_guard_fires((k + 1) as usize, total as usize),
                "just above the boundary (k={k}) must not fire"
            );
        }
    }

    #[test]
    fn guard_matches_oracle_over_small_and_structured_inputs() {
        let totals = [1u64, 2, 100, 4096, 109_060, 19_999, 20_001, 1_000_003];
        for &total in &totals {
            for count in 0..=total.min(10) {
                assert_eq!(
                    adaptive_guard_fires(count as usize, total as usize),
                    oracle_fires(count, total),
                    "count={count}, total={total}"
                );
            }
            // the fully-opaque extreme never fires (ratio == 1.0)
            assert!(!adaptive_guard_fires(total as usize, total as usize));
        }
    }

    #[test]
    fn guard_fires_only_with_essentially_no_opaque_pixels() {
        assert!(adaptive_guard_fires(0, 4096)); // syn firing sites: 0/4096
        assert!(adaptive_guard_fires(1, 109_060)); // w3c-alphatest: 1/109060
        assert!(!adaptive_guard_fires(4096, 4096)); // fully opaque
        assert!(!adaptive_guard_fires(100, 109_060)); // 100/109060 ~ 0.0009 -> 0.0009
    }
}

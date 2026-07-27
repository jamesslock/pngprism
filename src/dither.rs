//! Mirror of `lab/reference/prism_dither.py` @ `8754f90c`: alpha-boundary-safe
//! Floyd–Steinberg remapping plus the opt-in E-0010 policies (adaptive
//! `e/(e+g)` and the frozen ch19 A6 region table). Integer-only; error is
//! transported in premultiplied-RGBA feature space; diffusion never crosses a
//! source alpha boundary, a region-id boundary, or a barrier, and residual
//! that cannot cross is discarded (never renormalized). Palette-distance ties
//! choose the lowest index.
//!
//! This is a seam-by-seam translation of in-repo original work (the classical
//! Floyd–Steinberg 1976 kernel; the alpha/region rules are original Project
//! Prism work per the oracle header). The Python reference is the behavioral
//! ORACLE (vendored at `tests/oracle/`); the port's determinism
//! rules. Method provenance is inherited from the oracle ledger.
//!
//! The integrated `pngprism` CLI consumes only the produced INDEX map
//! (`floyd_steinberg` / `nearest_remap`) and the policy hooks; the oracle's
//! evidence dataclasses are not part of any emitted artifact and are not
//! reconstructed here.
//!
//! The source pin above records the port's historical origin; this module is
//! part of the current pngprism surface.
//!
//! **Label: 0.5.0, unproven, metric-validated only.**

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::{Error, Rgba, sha256};

/// Premultiplied-RGBA feature coordinate `(r*a, g*a, b*a, 255*a)`, all in
/// `0..=65025`. `i64` throughout (`prism_dither._feature`).
type Feature = [i64; 4];

/// Region id (`RegionDirective.region_id`). The oracle allows any hashable,
/// but the only producers are the four builders, which emit exactly these
/// three values; equality is the only operation used (transport legality).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionId {
    /// The default / uniform-strength id (`"global"`).
    Global,
    /// Non-dithered classes (`"none"`).
    None,
    /// The single dithered family (`"dither"`).
    Dither,
}

/// Caller-supplied per-pixel transport directive (`prism_dither.RegionDirective`).
/// `strength_*` is an exact nonnegative rational applied to outgoing residual.
/// (`policy_version` is oracle observability only — it never affects output
/// bytes — so it is not carried here.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionDirective {
    pub region_id: RegionId,
    pub barrier: bool,
    pub strength_numerator: i64,
    pub strength_denominator: i64,
}

impl RegionDirective {
    /// `RegionDirective()` — the T-0069 stub default (one global region, no
    /// barrier, full strength) used by the `region_hook=None` fast path.
    fn stub() -> Self {
        RegionDirective {
            region_id: RegionId::Global,
            barrier: false,
            strength_numerator: 1,
            strength_denominator: 1,
        }
    }
}

/// `7/16, 3/16, 5/16, 1/16` forward kernel (`prism_dither._KERNEL_FORWARD`);
/// each entry is `(dx_forward, dy, weight)`.
const KERNEL_FORWARD: [(i64, i64, i64); 4] = [(1, 0, 7), (-1, 1, 3), (0, 1, 5), (1, 1, 1)];
const KERNEL_DENOMINATOR: i64 = 16;

// --- E-0010 region policy constants (ch19 A6; `prism_dither` lines 415-446) --

const EDGE_STEP_MIN: i64 = 1020; // 4 premultiplied byte steps at full alpha
const EDGE_STEP_RATIO: i64 = 4;
const SHADOW_ALPHA_MAX: i64 = 64;

const NEIGHBOR_DELTAS: [(i64, i64); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

/// `prism_dither._feature`.
fn feature(pixel: Rgba) -> Feature {
    let (r, g, b, a) = (
        i64::from(pixel.0),
        i64::from(pixel.1),
        i64::from(pixel.2),
        i64::from(pixel.3),
    );
    [r * a, g * a, b * a, 255 * a]
}

/// `prism_dither._alpha_zone`.
fn alpha_zone(alpha: u8) -> usize {
    if alpha == 0 {
        0
    } else if alpha == 255 {
        2
    } else {
        1
    }
}

/// Round a signed rational to nearest, half AWAY from zero
/// (`prism_dither._round_div_signed`). Precondition `denominator > 0`
/// (validated at every call site: `16 * strength_denominator`, and
/// `strength_denominator > 0`). Distinct from the pipeline's nonnegative
/// `round_half_up`; residual is signed, so this rounding is used here.
fn round_div_signed(numerator: i64, denominator: i64) -> i64 {
    debug_assert!(denominator > 0);
    if numerator < 0 {
        -(((-numerator) + denominator / 2) / denominator)
    } else {
        (numerator + denominator / 2) / denominator
    }
}

/// Guard the `pixels.len() == width * height` invariant every
/// dimension-indexed function below relies on. These functions are public
/// crate API called directly with caller-supplied `width`/`height`/`pixels`
/// (not gated behind `png::decode_png`, which only guarantees the invariant
/// for its own callers): a mismatched triple would otherwise reach an
/// out-of-bounds row/column index and panic — exactly the ch17 §31 data-path
/// failure mode this returns a typed error for instead. `checked_mul` also
/// covers a `width * height` product overflowing `usize` for adversarial
/// dimensions, rather than panicking on the multiplication itself.
fn check_pixel_dimensions(pixels_len: usize, width: usize, height: usize) -> Result<(), Error> {
    match width.checked_mul(height) {
        Some(expected) if expected == pixels_len => Ok(()),
        Some(expected) => Err(Error::data(format!(
            "pixel count {pixels_len} does not match width*height {expected} ({width}x{height})"
        ))),
        None => Err(Error::data(format!(
            "width*height overflows for {width}x{height}"
        ))),
    }
}

/// Palette indices eligible in each alpha zone, ascending
/// (`prism_dither._eligible_by_zone`).
fn eligible_by_zone(palette: &[Rgba]) -> [Vec<usize>; 3] {
    let mut zones: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (index, entry) in palette.iter().enumerate() {
        zones[alpha_zone(entry.3)].push(index);
    }
    zones
}

/// Nearest palette index (in feature space) restricted to `eligible`, with the
/// squared distance (`prism_dither._nearest_index_and_distance_sq`). `eligible`
/// is ascending and only a STRICT improvement replaces, so ties keep the
/// lowest index. Max squared distance `4 * 65025^2 < 2^34` — `i64`.
fn nearest_index_and_distance_sq(
    feat: Feature,
    palette_features: &[Feature],
    eligible: &[usize],
) -> Result<(usize, i64), Error> {
    let &first = eligible.first().ok_or_else(|| {
        Error::data("palette has no entry in the source pixel's alpha zone".to_string())
    })?;
    let dist = |p: Feature| -> i64 {
        let d0 = feat[0] - p[0];
        let d1 = feat[1] - p[1];
        let d2 = feat[2] - p[2];
        let d3 = feat[3] - p[3];
        d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
    };
    let mut best_index = first;
    let mut best_distance = dist(palette_features[first]);
    for &index in &eligible[1..] {
        let distance = dist(palette_features[index]);
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    Ok((best_index, best_distance))
}

fn nearest_index(
    feat: Feature,
    palette_features: &[Feature],
    eligible: &[usize],
) -> Result<usize, Error> {
    Ok(nearest_index_and_distance_sq(feat, palette_features, eligible)?.0)
}

// --- E-0017 luma-weighted blue-noise mask dither --------------------------

const BLUENOISE_MASK_SIZE: usize = 64;
const BLUENOISE_LUMA_WEIGHT: f64 = 0.75;
const BLUENOISE_CHROMA_WEIGHT: f64 = 0.25;

#[derive(Clone, Copy)]
struct BlueNoiseMaskSpec {
    seed: u32,
    filename: &'static str,
    sha256: &'static str,
}

const BLUENOISE_MASK_SPECS: [BlueNoiseMaskSpec; 3] = [
    BlueNoiseMaskSpec {
        seed: 20_260_719,
        filename: "bluenoise-64-seed20260719-r.json",
        sha256: "8ee801878fd37cc52fbb2993fa4d7c5b4ace02f2fccc04a0c28dabf13111b0d8",
    },
    BlueNoiseMaskSpec {
        seed: 20_260_720,
        filename: "bluenoise-64-seed20260720-g.json",
        sha256: "80aba5e8dc5cbef7b1c04acfc3e3b0d6193375a74ef007cf8a26d604ae2522cc",
    },
    BlueNoiseMaskSpec {
        seed: 20_260_721,
        filename: "bluenoise-64-seed20260721-b.json",
        sha256: "cb2706b65c956f52369fd05ccb0c73fef52774c185cb39fffa7ff8dc79258139",
    },
];

struct BlueNoiseMasks {
    channels: [Vec<u16>; 3],
}

static BLUENOISE_MASK_CACHE: OnceLock<BlueNoiseMasks> = OnceLock::new();

/// The frozen E-0017 mask payloads, embedded in the binary at compile time from
/// `src/assets/bluenoise/` (copies of the E-0017 generator's output; see that
/// directory's `README.md` for provenance).
///
/// These are EMBEDDED rather than read from `../../experiments/…` at runtime so
/// the crate is self-contained: a consumer who has only the crate — a crates.io
/// download, a vendored copy, a `cargo install` — still gets a working dither
/// stage. The SHA-256 pins in `BLUENOISE_MASK_SPECS` are still verified on
/// first use (see `load_bluenoise_masks_from_bytes`), so the integrity contract
/// is unchanged; only the byte SOURCE moved from the filesystem into `.rodata`.
/// Order matches `BLUENOISE_MASK_SPECS` (r, g, b).
const BLUENOISE_MASK_BYTES: [&[u8]; 3] = [
    include_bytes!("assets/bluenoise/bluenoise-64-seed20260719-r.json"),
    include_bytes!("assets/bluenoise/bluenoise-64-seed20260720-g.json"),
    include_bytes!("assets/bluenoise/bluenoise-64-seed20260721-b.json"),
];

fn find_after<'a>(data: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    data.windows(needle.len())
        .position(|window| window == needle)
        .map(|index| &data[index + needle.len()..])
}

fn parse_mask(raw: &[u8], spec: BlueNoiseMaskSpec) -> Result<Vec<u16>, Error> {
    let invalid = || {
        Error::internal(format!(
            "internal: E-0017 mask {} shape/seed mismatch",
            spec.filename
        ))
    };
    let seed_tail = find_after(raw, b"\"seed\":").ok_or_else(invalid)?;
    let seed_end = seed_tail
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(seed_tail.len());
    let seed = std::str::from_utf8(&seed_tail[..seed_end])
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    let ranks_tail = find_after(raw, b"\"ranks_row_major\":[").ok_or_else(invalid)?;
    let ranks_end = ranks_tail
        .iter()
        .position(|&byte| byte == b']')
        .ok_or_else(invalid)?;
    let ranks_text = std::str::from_utf8(&ranks_tail[..ranks_end]).map_err(|_| invalid())?;
    let ranks: Vec<u16> = ranks_text
        .split(',')
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid())?;
    if seed != spec.seed || ranks.len() != BLUENOISE_MASK_SIZE * BLUENOISE_MASK_SIZE {
        return Err(invalid());
    }
    Ok(ranks)
}

fn load_bluenoise_masks_from_bytes(sources: &[&[u8]; 3]) -> Result<BlueNoiseMasks, Error> {
    let mut channels = [Vec::new(), Vec::new(), Vec::new()];
    for (index, spec) in BLUENOISE_MASK_SPECS.iter().copied().enumerate() {
        let raw = sources[index];
        let actual_sha = sha256::hex(raw);
        if actual_sha != spec.sha256 {
            return Err(Error::internal(format!(
                "internal: E-0017 mask {} sha256 {} != frozen {}",
                spec.filename, actual_sha, spec.sha256
            )));
        }
        channels[index] = parse_mask(raw, spec)?;
    }
    Ok(BlueNoiseMasks { channels })
}

fn load_bluenoise_masks() -> Result<&'static BlueNoiseMasks, Error> {
    if let Some(masks) = BLUENOISE_MASK_CACHE.get() {
        return Ok(masks);
    }
    let loaded = load_bluenoise_masks_from_bytes(&BLUENOISE_MASK_BYTES)?;
    let _ = BLUENOISE_MASK_CACHE.set(loaded);
    BLUENOISE_MASK_CACHE.get().ok_or_else(|| {
        Error::internal("internal: E-0017 blue-noise mask cache unavailable".to_string())
    })
}

fn bluenoise_amplitude(colors: i64) -> f64 {
    255.0 / (colors as f64).powf(1.0 / 3.0)
}

/// E-0017's luma-weighted void-and-cluster blue-noise remap
/// (`prism_dither.luma_bluenoise_remap`). The committed 64×64 rank masks are
/// SHA-256 verified before their first use. Float expressions preserve the
/// oracle's operation order and adjusted features use ties-to-even rounding.
pub fn luma_bluenoise_remap(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
    colors: i64,
    strength: (i64, i64),
) -> Result<Vec<usize>, Error> {
    check_pixel_dimensions(pixels.len(), width, height)?;
    if !(1..=256).contains(&colors) {
        return Err(Error::data("colors must be in 1..256".to_string()));
    }
    if strength.1 <= 0 || strength.0 < 0 {
        return Err(Error::data(
            "strength must be a nonnegative rational".to_string(),
        ));
    }
    let palette_features: Vec<Feature> = palette.iter().map(|&entry| feature(entry)).collect();
    let eligible = eligible_by_zone(palette);
    let masks = load_bluenoise_masks()?;
    let [mask_r, mask_g, mask_b] = &masks.channels;
    let total = (BLUENOISE_MASK_SIZE * BLUENOISE_MASK_SIZE) as f64;
    let amplitude = bluenoise_amplitude(colors);
    let scale = strength.0 as f64 / strength.1 as f64;
    let mut indices = vec![0usize; pixels.len()];
    for y in 0..height {
        let row = (y % BLUENOISE_MASK_SIZE) * BLUENOISE_MASK_SIZE;
        for x in 0..width {
            let position = y * width + x;
            let (red, green, blue, alpha) = pixels[position];
            let tile = row + (x % BLUENOISE_MASK_SIZE);
            let noise_r = ((f64::from(mask_r[tile]) + 0.5) / total - 0.5) * amplitude;
            let noise_g = ((f64::from(mask_g[tile]) + 0.5) / total - 0.5) * amplitude;
            let noise_b = ((f64::from(mask_b[tile]) + 0.5) / total - 0.5) * amplitude;
            let noise_luma = (noise_r + noise_g + noise_b) / 3.0;
            let delta_r =
                scale * (BLUENOISE_CHROMA_WEIGHT * noise_r + BLUENOISE_LUMA_WEIGHT * noise_luma);
            let delta_g =
                scale * (BLUENOISE_CHROMA_WEIGHT * noise_g + BLUENOISE_LUMA_WEIGHT * noise_luma);
            let delta_b =
                scale * (BLUENOISE_CHROMA_WEIGHT * noise_b + BLUENOISE_LUMA_WEIGHT * noise_luma);
            let alpha_float = f64::from(alpha);
            let adjusted = [
                ((f64::from(red) + delta_r) * alpha_float)
                    .round_ties_even()
                    .clamp(0.0, 65_025.0) as i64,
                ((f64::from(green) + delta_g) * alpha_float)
                    .round_ties_even()
                    .clamp(0.0, 65_025.0) as i64,
                ((f64::from(blue) + delta_b) * alpha_float)
                    .round_ties_even()
                    .clamp(0.0, 65_025.0) as i64,
                255 * i64::from(alpha),
            ];
            indices[position] =
                nearest_index(adjusted, &palette_features, &eligible[alpha_zone(alpha)])?;
        }
    }
    Ok(indices)
}

/// Direct alpha-zone-constrained nearest remap: the no-dither baseline
/// (`prism_dither.nearest_remap`). Returns the per-pixel palette index map.
pub fn nearest_remap(
    pixels: &[Rgba],
    _width: usize,
    _height: usize,
    palette: &[Rgba],
) -> Result<Vec<usize>, Error> {
    let palette_features: Vec<Feature> = palette.iter().map(|&e| feature(e)).collect();
    let eligible = eligible_by_zone(palette);
    let mut indices = Vec::with_capacity(pixels.len());
    for &pixel in pixels {
        indices.push(nearest_index(
            feature(pixel),
            &palette_features,
            &eligible[alpha_zone(pixel.3)],
        )?);
    }
    Ok(indices)
}

/// Alpha-boundary-safe Floyd–Steinberg error diffusion
/// (`prism_dither.floyd_steinberg`, `serpentine=True`). `region_directives` is
/// the per-pixel directive table (row-major, length `width*height`); the
/// `region_hook=None` fast path passes a table of stub directives. Returns the
/// per-pixel palette index map.
pub fn floyd_steinberg(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
    region_directives: &[RegionDirective],
) -> Result<Vec<usize>, Error> {
    check_pixel_dimensions(pixels.len(), width, height)?;
    if region_directives.len() != pixels.len() {
        return Err(Error::data(format!(
            "region directive count {} does not match pixel count {}",
            region_directives.len(),
            pixels.len()
        )));
    }
    // `RegionDirective`'s fields are all `pub`, so a caller can hand-build
    // one with a non-positive `strength_denominator` without going through
    // `uniform_strength_directives` (which only ever receives an
    // already-`quantize_png`-validated ratio). `round_div_signed`'s
    // `denominator` is `KERNEL_DENOMINATOR * strength_denominator`: zero
    // denominator is an unconditional (release-mode-too) integer division
    // panic, and a negative one silently inverts the rounding. Both are a
    // ch17 §31 data-path failure on a directly public struct; reject up
    // front instead of reaching the division.
    if let Some(bad) = region_directives
        .iter()
        .find(|directive| directive.strength_denominator <= 0)
    {
        return Err(Error::data(format!(
            "region directive strength_denominator must be positive, got {}",
            bad.strength_denominator
        )));
    }
    let source_features: Vec<Feature> = pixels.iter().map(|&p| feature(p)).collect();
    let palette_features: Vec<Feature> = palette.iter().map(|&e| feature(e)).collect();
    let eligible = eligible_by_zone(palette);
    let zones: Vec<usize> = pixels.iter().map(|p| alpha_zone(p.3)).collect();

    let mut residual = vec![[0i64; 4]; pixels.len()];
    let mut indices = vec![0usize; pixels.len()];
    let w = width as i64;
    let h = height as i64;

    for y in 0..height {
        let reverse = y % 2 == 1; // serpentine
        // x order: reverse rows scan right-to-left.
        let xs: Vec<usize> = if reverse {
            (0..width).rev().collect()
        } else {
            (0..width).collect()
        };
        for x in xs {
            let position = y * width + x;
            let mut adjusted = [0i64; 4];
            for c in 0..4 {
                adjusted[c] =
                    (source_features[position][c] + residual[position][c]).clamp(0, 65025);
            }
            let chosen = nearest_index(adjusted, &palette_features, &eligible[zones[position]])?;
            indices[position] = chosen;
            let mut error = [0i64; 4];
            for c in 0..4 {
                error[c] = adjusted[c] - palette_features[chosen][c];
            }
            let directive = region_directives[position];
            for (dx_forward, dy, weight) in KERNEL_FORWARD {
                let dx = if reverse { -dx_forward } else { dx_forward };
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx < 0 || nx >= w || ny < 0 || ny >= h {
                    continue;
                }
                let neighbor = (ny * w + nx) as usize;
                let target = region_directives[neighbor];
                let legal = zones[position] == zones[neighbor]
                    && directive.region_id == target.region_id
                    && !directive.barrier
                    && !target.barrier;
                if !legal {
                    continue;
                }
                let denominator = KERNEL_DENOMINATOR * directive.strength_denominator;
                let numerator_scale = weight * directive.strength_numerator;
                for c in 0..4 {
                    residual[neighbor][c] +=
                        round_div_signed(error[c] * numerator_scale, denominator);
                }
            }
        }
    }
    Ok(indices)
}

// --- Uniform strength -------------------------------------------------------

/// `prism_dither._uniform_strength_hook`: one directive for every pixel with
/// the given strength (region id `Global`, no barrier).
pub fn uniform_strength_directives(
    strength: (i64, i64),
    pixel_count: usize,
) -> Vec<RegionDirective> {
    vec![
        RegionDirective {
            region_id: RegionId::Global,
            barrier: false,
            strength_numerator: strength.0,
            strength_denominator: strength.1,
        };
        pixel_count
    ]
}

/// The `region_hook=None` stub table (one global full-strength directive per
/// pixel). Byte-equivalent to `uniform_strength_directives((1,1), n)`.
pub fn stub_directives(pixel_count: usize) -> Vec<RegionDirective> {
    vec![RegionDirective::stub(); pixel_count]
}

// --- Adaptive policy (E-0010 `e/(e+g)`, `prism_dither` lines 449-520) --------

/// Each pixel's largest squared feature step to a 4-neighbor
/// (`prism_dither._squared_local_gradient`). Max per-channel step 65025, so a
/// full squared step `< 2^34` — `i64`.
fn squared_local_gradient(features: &[Feature], width: usize, height: usize) -> Vec<i64> {
    let mut gradient = vec![0i64; features.len()];
    let w = width as i64;
    let h = height as i64;
    for y in 0..height {
        for x in 0..width {
            let position = y * width + x;
            let feat = features[position];
            let mut best = 0i64;
            for (dx, dy) in NEIGHBOR_DELTAS {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx >= 0 && nx < w && ny >= 0 && ny < h {
                    let other = features[(ny * w + nx) as usize];
                    let mut distance = 0i64;
                    for c in 0..4 {
                        let d = feat[c] - other[c];
                        distance += d * d;
                    }
                    if distance > best {
                        best = distance;
                    }
                }
            }
            gradient[position] = best;
        }
    }
    gradient
}

/// E-0010's global continuous strength policy `e/(e+g)`
/// (`prism_dither.adaptive_strength_hook`): `e` is nearest-entry squared
/// premultiplied error, `g` the largest squared step to a 4-neighbor. Exact
/// matches use `0/1`; every other pixel keeps the unreduced exact ratio.
/// Returns the per-pixel directive table (region id `Global`).
pub fn adaptive_strength_directives(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
) -> Result<Vec<RegionDirective>, Error> {
    check_pixel_dimensions(pixels.len(), width, height)?;
    let features: Vec<Feature> = pixels.iter().map(|&p| feature(p)).collect();
    let palette_features: Vec<Feature> = palette.iter().map(|&e| feature(e)).collect();
    let eligible = eligible_by_zone(palette);
    let mut errors = Vec::with_capacity(pixels.len());
    for (pixel, &feat) in pixels.iter().zip(features.iter()) {
        let (_, distance) =
            nearest_index_and_distance_sq(feat, &palette_features, &eligible[alpha_zone(pixel.3)])?;
        errors.push(distance);
    }
    let gradient = squared_local_gradient(&features, width, height);
    let mut directives = Vec::with_capacity(pixels.len());
    for (error, local_gradient) in errors.into_iter().zip(gradient) {
        let total = error + local_gradient;
        let (numerator, denominator) = if total == 0 { (0, 1) } else { (error, total) };
        directives.push(RegionDirective {
            region_id: RegionId::Global,
            barrier: false,
            strength_numerator: numerator,
            strength_denominator: denominator,
        });
    }
    Ok(directives)
}

// --- Region policy (E-0010 ch19 A6, `prism_dither` lines 421-738) ------------

/// A frozen region class (`prism_dither.REGION_CLASSES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionClass {
    Transparent,
    ProtectedExact,
    Flat,
    HardEdge,
    Texture,
    GradientOpaque,
    GradientAlpha,
    SoftShadow,
    // `Uncertain` is a table entry for future classifiers; the v1 decision
    // tree is total and never emits it, so it is unreachable here.
}

/// `class -> (region_id, barrier, strength_numerator, strength_denominator)`
/// (`prism_dither.REGION_CLASS_TABLE`).
fn region_class_directive(class: RegionClass) -> RegionDirective {
    let (region_id, barrier, num, den) = match class {
        RegionClass::Transparent => (RegionId::None, false, 0, 1),
        RegionClass::ProtectedExact => (RegionId::None, true, 0, 1),
        RegionClass::Flat => (RegionId::None, false, 0, 1),
        RegionClass::HardEdge => (RegionId::None, true, 0, 1),
        RegionClass::Texture => (RegionId::None, false, 0, 1),
        RegionClass::GradientOpaque => (RegionId::Dither, false, 1, 1),
        RegionClass::GradientAlpha => (RegionId::Dither, false, 3, 4),
        RegionClass::SoftShadow => (RegionId::Dither, false, 1, 2),
    };
    RegionDirective {
        region_id,
        barrier,
        strength_numerator: num,
        strength_denominator: den,
    }
}

/// Classify every pixel into a frozen ch19 A6 region class
/// (`prism_dither.classify_regions`). Deterministic, integer-exact, three
/// passes plus a confluent flat flood-fill.
fn classify_regions(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
) -> Result<Vec<RegionClass>, Error> {
    check_pixel_dimensions(pixels.len(), width, height)?;
    let palette_features: Vec<Feature> = palette.iter().map(|&e| feature(e)).collect();
    let eligible = eligible_by_zone(palette);
    let features: Vec<Feature> = pixels.iter().map(|&p| feature(p)).collect();
    let zones: Vec<usize> = pixels.iter().map(|p| alpha_zone(p.3)).collect();
    let count = pixels.len();
    let mut classes: Vec<Option<RegionClass>> = vec![None; count];
    let w = width as i64;
    let h = height as i64;

    // Pass 1: transparent support and palette-exact (protected) pixels.
    //
    // Only whether SOME eligible palette entry exactly matches the pixel's
    // feature vector is needed here (the matched entry's index is discarded
    // below), so this is a set-membership test rather than a full linear
    // nearest-distance scan over the (up to 256-entry) eligible list per
    // pixel. A squared 4D distance of 0 holds iff the two feature vectors are
    // pointwise equal (a sum of squares is 0 iff every term is 0), so
    // "exists eligible entry at distance 0" and "feature vector is in the
    // eligible set" are the same predicate. Same classification result,
    // O(1)-average membership instead of O(palette size) per pixel.
    let zone_exact_features: [HashSet<Feature>; 3] = std::array::from_fn(|zone| {
        eligible[zone]
            .iter()
            .map(|&index| palette_features[index])
            .collect()
    });
    for position in 0..count {
        let zone = zones[position];
        if zone == 0 {
            classes[position] = Some(RegionClass::Transparent);
            continue;
        }
        if eligible[zone].is_empty() {
            return Err(Error::data(
                "palette has no entry in the source pixel's alpha zone".to_string(),
            ));
        }
        if zone_exact_features[zone].contains(&features[position]) {
            classes[position] = Some(RegionClass::ProtectedExact);
        }
    }

    // Pass 2: flat seeds (all existing 4-neighbors identical), then flood the
    // identical-color component. Confluent — order-independent.
    let mut stack: Vec<usize> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let position = y * width + x;
            if classes[position].is_some() {
                continue;
            }
            let pixel = pixels[position];
            let mut all_same = true;
            for (dx, dy) in NEIGHBOR_DELTAS {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx >= 0 && nx < w && ny >= 0 && ny < h && pixels[(ny * w + nx) as usize] != pixel
                {
                    all_same = false;
                    break;
                }
            }
            if all_same {
                classes[position] = Some(RegionClass::Flat);
                stack.push(position);
            }
        }
    }
    while let Some(position) = stack.pop() {
        let x = (position % width) as i64;
        let y = (position / width) as i64;
        for (dx, dy) in NEIGHBOR_DELTAS {
            let nx = x + dx;
            let ny = y + dy;
            if nx >= 0 && nx < w && ny >= 0 && ny < h {
                let neighbor = (ny * w + nx) as usize;
                if classes[neighbor].is_none() && pixels[neighbor] == pixels[position] {
                    classes[neighbor] = Some(RegionClass::Flat);
                    stack.push(neighbor);
                }
            }
        }
    }

    // Pass 3: hard edges, texture, and the gradient/shadow family.
    let step_max = |a: Feature, b: Feature| -> i64 {
        (a[0] - b[0])
            .abs()
            .max((a[1] - b[1]).abs())
            .max((a[2] - b[2]).abs())
            .max((a[3] - b[3]).abs())
    };
    for y in 0..height {
        for x in 0..width {
            let position = y * width + x;
            if classes[position].is_some() {
                continue;
            }
            let feat = features[position];
            let mut largest = 0i64;
            let mut largest_neighbor: i64 = -1;
            let mut zone_boundary = false;
            for (dx, dy) in NEIGHBOR_DELTAS {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if !(nx >= 0 && nx < w && ny >= 0 && ny < h) {
                    continue;
                }
                let neighbor = (ny * w + nx) as usize;
                if zones[neighbor] != zones[position] {
                    zone_boundary = true;
                }
                let step = step_max(feat, features[neighbor]);
                if step > largest {
                    // Strict improvement: first argmax in NEIGHBOR_DELTAS order.
                    largest = step;
                    largest_neighbor = neighbor as i64;
                }
            }
            if zone_boundary {
                classes[position] = Some(RegionClass::HardEdge);
                continue;
            }
            if largest >= EDGE_STEP_MIN {
                // Isolated-discontinuity test: the largest step continuing past
                // this pixel (excluding the argmax neighbor) or past that
                // neighbor (excluding this pixel).
                let mut continuation = 0i64;
                for (dx, dy) in NEIGHBOR_DELTAS {
                    let nx = x as i64 + dx;
                    let ny = y as i64 + dy;
                    if !(nx >= 0 && nx < w && ny >= 0 && ny < h) {
                        continue;
                    }
                    let neighbor = ny * w + nx;
                    if neighbor == largest_neighbor {
                        continue;
                    }
                    let step = step_max(feat, features[neighbor as usize]);
                    if step > continuation {
                        continuation = step;
                    }
                }
                let qx = largest_neighbor % w;
                let qy = largest_neighbor / w;
                let q_feature = features[largest_neighbor as usize];
                for (dx, dy) in NEIGHBOR_DELTAS {
                    let nx = qx + dx;
                    let ny = qy + dy;
                    if !(nx >= 0 && nx < w && ny >= 0 && ny < h) {
                        continue;
                    }
                    let neighbor = ny * w + nx;
                    if neighbor == position as i64 {
                        continue;
                    }
                    let step = step_max(q_feature, features[neighbor as usize]);
                    if step > continuation {
                        continuation = step;
                    }
                }
                if continuation * EDGE_STEP_RATIO <= largest {
                    classes[position] = Some(RegionClass::HardEdge);
                    continue;
                }
            }
            // Texture: sign-incoherent activity of the premultiplied channel
            // sum in the nearest valid 2x2 window (clamped one cell in from the
            // right/bottom border).
            let mut incoherent = false;
            if width >= 2 && height >= 2 {
                let anchor_x = x.min(width - 2);
                let anchor_y = y.min(height - 2);
                let base = anchor_y * width + anchor_x;
                let sum3 = |f: Feature| f[0] + f[1] + f[2];
                let here = sum3(features[base]);
                let right = sum3(features[base + 1]);
                let below = sum3(features[base + width]);
                let diagonal = sum3(features[base + width + 1]);
                let dx_top = right - here;
                let dx_bottom = diagonal - below;
                let dy_left = below - here;
                let dy_right = diagonal - right;
                if (dx_top != 0 && dx_bottom != 0 && (dx_top < 0) != (dx_bottom < 0))
                    || (dy_left != 0 && dy_right != 0 && (dy_left < 0) != (dy_right < 0))
                {
                    incoherent = true;
                }
            }
            classes[position] = Some(if incoherent {
                RegionClass::Texture
            } else if zones[position] == 2 {
                RegionClass::GradientOpaque
            } else if i64::from(pixels[position].3) < SHADOW_ALPHA_MAX {
                RegionClass::SoftShadow
            } else {
                RegionClass::GradientAlpha
            });
        }
    }

    // Every pixel is classified by construction (the v1 tree is total).
    let mut resolved = Vec::with_capacity(count);
    for class in classes {
        resolved.push(
            class.ok_or_else(|| Error::internal("internal: unclassified pixel".to_string()))?,
        );
    }
    Ok(resolved)
}

/// Return E-0014's reduced per-unit `B / (B + N)` strength from the frozen
/// E-0010 region classes (`prism_dither._unit_strength_from_classes`).
fn unit_strength_from_classes(classes: &[RegionClass]) -> (i64, i64) {
    let banding = classes
        .iter()
        .filter(|class| {
            matches!(
                class,
                RegionClass::GradientOpaque | RegionClass::GradientAlpha | RegionClass::SoftShadow
            )
        })
        .count();
    let grain = classes
        .iter()
        .filter(|class| matches!(class, RegionClass::Texture | RegionClass::Flat))
        .count();
    let total = banding + grain;
    if total == 0 {
        return (0, 1);
    }
    let divisor = gcd(banding as u128, total as u128) as usize;
    // A resident pixel slice cannot approach i64::MAX elements on supported
    // targets; these casts preserve the oracle's exact integer ratio.
    ((banding / divisor) as i64, (total / divisor) as i64)
}

/// Predict E-0014's single adaptive-unit strength for an image and palette
/// (`prism_dither.predict_unit_strength`). Classification, class partition,
/// reduction, and ordering are shared with the live Python oracle.
pub fn adaptive_unit_strength(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
) -> Result<(i64, i64), Error> {
    let classes = classify_regions(pixels, width, height, palette)?;
    Ok(unit_strength_from_classes(&classes))
}

/// Classify the source and return the per-pixel directive table for the region
/// path (`prism_dither.region_policy_hook` composed with
/// `region_hook_from_classes`).
pub fn region_policy_directives(
    pixels: &[Rgba],
    width: usize,
    height: usize,
    palette: &[Rgba],
) -> Result<Vec<RegionDirective>, Error> {
    let classes = classify_regions(pixels, width, height, palette)?;
    Ok(classes.into_iter().map(region_class_directive).collect())
}

// --- Exact-rational strength parse (`prism_dither._parse_dither_strength`) ----

/// Parse a finite decimal strength exactly and return its reduced ratio, the
/// way `Decimal(value).as_integer_ratio()` does — WITHOUT floating point.
/// Accepts an optional sign, integer and/or fraction digits, and a decimal
/// exponent (`e`/`E`); rejects NaN/Infinity and anything outside `[0, 1]`.
/// Mirrors `prism_dither._parse_dither_strength` (usage error, exit 2).
pub fn parse_dither_strength(value: &str) -> Result<(i64, i64), Error> {
    let err =
        || Error::usage("usage_error: --dither-strength must be a decimal in 0..1".to_string());
    // Python's Decimal(str) strips surrounding ASCII whitespace.
    let text =
        value.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    if text.is_empty() {
        return Err(err());
    }
    let mut rest = text;
    let negative = if let Some(r) = rest.strip_prefix('-') {
        rest = r;
        true
    } else if let Some(r) = rest.strip_prefix('+') {
        rest = r;
        false
    } else {
        false
    };
    // Reject NaN / Infinity (not finite).
    let lower = rest.to_ascii_lowercase();
    if lower.starts_with("nan")
        || lower.starts_with("inf")
        || lower.starts_with("snan")
        || rest.is_empty()
    {
        return Err(err());
    }
    // Split mantissa and exponent.
    let (mantissa, exponent): (&str, i64) = match rest.find(['e', 'E']) {
        Some(pos) => {
            let exp_str = &rest[pos + 1..];
            let exp = parse_signed_int(exp_str).ok_or_else(err)?;
            (&rest[..pos], exp)
        }
        None => (rest, 0),
    };
    // Split integer and fraction digits.
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(pos) => (&mantissa[..pos], &mantissa[pos + 1..]),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(err());
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(err());
    }
    // Value = (int_part . frac_part) * 10^exponent, exact.
    let digits: String = format!("{int_part}{frac_part}");
    let numerator_mag: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse::<i128>().map_err(|_| err())?
    };
    // Scale exponent: fraction contributes 10^-len(frac); exponent adds.
    let scale = exponent - frac_part.len() as i64;
    // numerator_mag * 10^scale, as an exact fraction num/den (den a power of 10).
    let (mut num, mut den): (i128, i128) = if scale >= 0 {
        (
            numerator_mag
                .checked_mul(pow10(scale as u32).ok_or_else(err)?)
                .ok_or_else(err)?,
            1,
        )
    } else {
        (numerator_mag, pow10((-scale) as u32).ok_or_else(err)?)
    };
    if negative {
        num = -num;
    }
    // Reduce to lowest terms with positive denominator (as_integer_ratio).
    if num == 0 {
        num = 0;
        den = 1;
    } else {
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()) as i128;
        num /= g;
        den /= g;
    }
    // Range check 0 <= value <= 1.
    if num < 0 || num > den {
        return Err(err());
    }
    let numerator = i64::try_from(num).map_err(|_| err())?;
    let denominator = i64::try_from(den).map_err(|_| err())?;
    Ok((numerator, denominator))
}

fn parse_signed_int(text: &str) -> Option<i64> {
    let mut rest = text;
    let mut negative = false;
    if let Some(r) = rest.strip_prefix('-') {
        rest = r;
        negative = true;
    } else if let Some(r) = rest.strip_prefix('+') {
        rest = r;
    }
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let magnitude: i64 = rest.parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

fn pow10(exp: u32) -> Option<i128> {
    // Bounded: strengths are in [0,1]; anything needing a huge power is
    // out of range and will be rejected by the caller's checks anyway.
    if exp > 38 {
        return None;
    }
    10i128.checked_pow(exp)
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strength_exact_ratios() {
        assert_eq!(parse_dither_strength("0"), Ok((0, 1)));
        assert_eq!(parse_dither_strength("1"), Ok((1, 1)));
        assert_eq!(parse_dither_strength("1.0"), Ok((1, 1)));
        assert_eq!(parse_dither_strength("0.5"), Ok((1, 2)));
        assert_eq!(parse_dither_strength("0.25"), Ok((1, 4)));
        assert_eq!(parse_dither_strength("0.125"), Ok((1, 8)));
        assert_eq!(parse_dither_strength("0.30"), Ok((3, 10)));
        assert_eq!(parse_dither_strength("0.750"), Ok((3, 4)));
        assert_eq!(parse_dither_strength("2.5e-1"), Ok((1, 4))); // 0.25
        assert_eq!(parse_dither_strength(" 0.5 "), Ok((1, 2))); // Decimal strips ws
    }

    #[test]
    fn parse_strength_rejects_out_of_range_and_garbage() {
        for bad in ["1.5", "-0.5", "2", "nope", "", "nan", "inf", "1e3"] {
            assert!(
                parse_dither_strength(bad).is_err(),
                "expected reject: {bad:?}"
            );
        }
    }

    #[test]
    fn round_div_signed_half_away_from_zero() {
        assert_eq!(round_div_signed(8, 16), 1); // 0.5 -> away -> 1
        assert_eq!(round_div_signed(-8, 16), -1);
        assert_eq!(round_div_signed(7, 16), 0);
        assert_eq!(round_div_signed(-7, 16), 0);
        assert_eq!(round_div_signed(24, 16), 2); // 1.5 -> 2
        assert_eq!(round_div_signed(-24, 16), -2);
        assert_eq!(round_div_signed(0, 16), 0);
    }

    #[test]
    fn nearest_remap_respects_alpha_zones() {
        // Palette: transparent, interior, opaque. A transparent pixel must map
        // to the transparent entry; an opaque pixel to the opaque entry.
        let palette = vec![(0, 0, 0, 0), (10, 20, 30, 128), (200, 100, 50, 255)];
        let pixels = vec![(9, 9, 9, 0), (250, 250, 250, 255), (11, 21, 31, 128)];
        let indices = nearest_remap(&pixels, 3, 1, &palette).unwrap();
        assert_eq!(indices, vec![0, 2, 1]);
    }

    #[test]
    fn floyd_steinberg_is_deterministic() {
        let palette = vec![(0, 0, 0, 255), (255, 255, 255, 255)];
        let pixels: Vec<Rgba> = (0..64)
            .map(|i| ((i * 4) as u8, (i * 4) as u8, (i * 4) as u8, 255))
            .collect();
        let directives = stub_directives(pixels.len());
        let a = floyd_steinberg(&pixels, 8, 8, &palette, &directives).unwrap();
        let b = floyd_steinberg(&pixels, 8, 8, &palette, &directives).unwrap();
        assert_eq!(a, b);
        // Every index is a valid palette position.
        assert!(a.iter().all(|&i| i < palette.len()));
    }

    #[test]
    fn stub_and_uniform_full_strength_are_byte_equivalent() {
        // region_hook=None fast path == uniform (1,1): same transport, same map.
        let palette = vec![(0, 0, 0, 255), (128, 128, 128, 255), (255, 255, 255, 255)];
        let pixels: Vec<Rgba> = (0..36).map(|i| ((i * 7) as u8, 0, 0, 255)).collect();
        let stub =
            floyd_steinberg(&pixels, 6, 6, &palette, &stub_directives(pixels.len())).unwrap();
        let uniform = floyd_steinberg(
            &pixels,
            6,
            6,
            &palette,
            &uniform_strength_directives((1, 1), pixels.len()),
        )
        .unwrap();
        assert_eq!(stub, uniform);
    }

    #[test]
    fn adaptive_unit_strength_reduces_banding_over_banding_plus_grain() {
        let classes = [
            RegionClass::GradientOpaque,
            RegionClass::GradientAlpha,
            RegionClass::SoftShadow,
            RegionClass::Texture,
            RegionClass::Flat,
            RegionClass::HardEdge,
            RegionClass::ProtectedExact,
            RegionClass::Transparent,
        ];

        assert_eq!(unit_strength_from_classes(&classes), (3, 5));
    }

    #[test]
    fn adaptive_unit_strength_is_zero_when_all_classes_are_neutral() {
        let classes = [
            RegionClass::HardEdge,
            RegionClass::ProtectedExact,
            RegionClass::Transparent,
        ];

        assert_eq!(unit_strength_from_classes(&classes), (0, 1));
    }

    #[test]
    fn committed_bluenoise_masks_pass_frozen_sha256_pins() {
        let masks = load_bluenoise_masks_from_bytes(&BLUENOISE_MASK_BYTES).unwrap();

        assert_eq!(
            masks
                .channels
                .map(|ranks| (ranks.len(), ranks[0], ranks[4095])),
            [(4096, 199, 673), (4096, 683, 3115), (4096, 753, 291)]
        );
    }

    #[test]
    fn modified_bluenoise_mask_fails_before_parsing() {
        let mut tampered = BLUENOISE_MASK_BYTES[0].to_vec();
        tampered[0] ^= 1;
        let sources: [&[u8]; 3] = [&tampered, BLUENOISE_MASK_BYTES[1], BLUENOISE_MASK_BYTES[2]];

        let error = load_bluenoise_masks_from_bytes(&sources).err().unwrap();

        assert_eq!(error.kind(), crate::Kind::Internal);
        assert!(error.message().contains("sha256"));
    }

    /// The embedded payloads must stay byte-identical to the E-0017 generator
    /// output the pins were frozen against. `load_bluenoise_masks_from_bytes`
    /// already checks this on the happy path, but this asserts it directly so a
    /// bad re-vendoring names the offending channel instead of surfacing as a
    /// generic load failure.
    #[test]
    fn embedded_bluenoise_payloads_match_frozen_pins() {
        for (index, spec) in BLUENOISE_MASK_SPECS.iter().copied().enumerate() {
            assert_eq!(
                sha256::hex(BLUENOISE_MASK_BYTES[index]),
                spec.sha256,
                "embedded {} drifted from its frozen pin",
                spec.filename
            );
        }
    }

    #[test]
    fn luma_bluenoise_indices_match_python_oracle_vector() {
        let pixels: Vec<Rgba> = [
            40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190,
        ]
        .into_iter()
        .map(|value| (value, value, value, 255))
        .collect();
        let palette = [
            (0, 0, 0, 255),
            (85, 85, 85, 255),
            (170, 170, 170, 255),
            (255, 255, 255, 255),
        ];

        let indices = luma_bluenoise_remap(&pixels, 8, 2, &palette, 4, (1, 1)).unwrap();

        assert_eq!(indices, [0, 1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 2, 2, 2, 2, 2]);
    }

    #[test]
    fn zero_strength_luma_bluenoise_is_nearest_remap() {
        let pixels: Vec<Rgba> = [
            40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190,
        ]
        .into_iter()
        .map(|value| (value, value, value, 255))
        .collect();
        let palette = [
            (0, 0, 0, 255),
            (85, 85, 85, 255),
            (170, 170, 170, 255),
            (255, 255, 255, 255),
        ];

        assert_eq!(
            luma_bluenoise_remap(&pixels, 8, 2, &palette, 4, (0, 1)).unwrap(),
            nearest_remap(&pixels, 8, 2, &palette).unwrap()
        );
    }
}

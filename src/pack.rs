//! Mirror of `lab/reference/prism_pack.py` @ `25ca55d3`: the deterministic
//! lossless indexed-PNG packing search. It never changes the decoded RGBA
//! candidate; it cleans the palette, tries deterministic palette permutations
//! and PNG row-filter strategies, compares complete PNG byte strings, and
//! retains the smallest artifact. `v1` is the original 5-order/6-filter
//! portfolio; `v2` is the bounded ch19 A5 search (9 orders, a trial-zlib
//! per-row filter heuristic, actual-byte local palette/filter refinement, and
//! up to three pinned-`zopflipng` finalists in maximum mode).
//!
//! Seam-by-seam translation of in-repo original work; the Python reference is
//! the behavioral ORACLE (vendored at `tests/oracle/`, digest-pinned).
//! `zopflipng` is invoked as a subprocess and is a black-box tool, not a linked
//! dependency — it does PNG-level work (row-filter search, color-type
//! reduction), so a deflate-only library is not a substitute for it. It is an
//! OPTIONAL external tool needed only by `--pack max`; the default pack mode is
//! `none` (`DEFAULT_PACK_MODE`), so the common path never looks for it.
//! `default_zopflipng` resolves it from `PRISM_ZOPFLIPNG`, else `PATH`; parity
//! and benchmark harnesses always set the former, pinning the SAME binary the
//! oracle uses so published byte figures reproduce exactly. Every invocation is
//! bounded by a timeout (`PRISM_ZOPFLIPNG_TIMEOUT_SECS`, default 120s —
//! `run_zopflipng`), ops hygiene per tri-review kimi F10.
//!
//! **Scope note (parity boundary):** the integrated `pngprism` CLI emits
//! only the final PNG bytes (and derives its summary from re-decoding them),
//! never the oracle's evidence dataclasses. This port therefore reproduces the
//! byte-producing and control-flow logic — palette cleanup, encoding, the
//! search, finalist selection, and the decoded-pixel-identity guards — but not
//! the evidence facts (`_observed_artifact_facts`, per-variant SHA/histogram
//! records) that never reach an artifact. Max-mode finalist de-duplication
//! keys on the palette RGBA bytes directly (the same equivalence classes as
//! the oracle's `palette_rgba_sha256`), so no hashing dependency is needed.
//!
//! The source pin above records the port's historical origin; this module is
//! part of the current pngprism surface.
//!
//! **Label: 0.5.0, unproven, metric-validated only.**

use crate::png::{self, PNG_SIGNATURE};
use crate::{Error, Rgba};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The `zprobe` FFI shim for the `trial-zlib` heuristic.
mod zprobe;

/// Test-only access to the real pack FFI stream for the cross-language gate.
#[cfg(feature = "zlib-ffi-harness")]
#[doc(hidden)]
pub mod zlib_ffi_harness {
    use super::zprobe;

    /// A live level-9 stream using the same implementation as trial-zlib.
    pub struct Deflater(zprobe::Deflater);

    impl Deflater {
        /// Initialize a retained stream.
        pub fn new() -> Result<Self, i32> {
            zprobe::Deflater::new().map(Self)
        }

        /// Copy the retained stream with zlib's `deflateCopy`.
        #[must_use]
        pub fn copy(&self) -> Self {
            Self(self.0.copy())
        }

        /// Capture one `deflate(Z_NO_FLUSH)` call.
        pub fn compress(&mut self, data: &[u8]) -> Vec<u8> {
            self.0.compress_capture(data)
        }

        /// Capture one `deflate(Z_SYNC_FLUSH)` call.
        pub fn flush_sync(&mut self) -> Vec<u8> {
            self.0.flush_sync_capture()
        }

        /// Capture `deflate(Z_FINISH)` through stream end.
        pub fn finish(&mut self) -> Vec<u8> {
            self.0.finish_capture()
        }
    }

    /// Return the linked zlib runtime version.
    #[must_use]
    pub fn runtime_version() -> String {
        zprobe::runtime_version()
    }
}

/// The version string of the zlib actually linked into this process
/// (`zlibVersion()`, queried at runtime — ops hygiene, tri-review kimi
/// F10). PNG output bytes never embed this; it exists so a caller
/// (parity harness, provenance record) can capture and log the exact
/// linked zlib identity as evidence alongside emitted artifacts,
/// answering fable finding #3 ("byte-determinism is machine-contingent
/// on system zlib" — the `-sys` crate pin governs the Rust binding, not
/// the C library it dynamically links).
#[must_use]
pub fn zlib_runtime_version() -> String {
    zprobe::runtime_version()
}

// --- Declared portfolios and V2 budget (`prism_pack` lines 45-71) -----------

/// Filter names by index (`prism_pack.FILTER_NAMES`): none/sub/up/average/paeth.
const FILTER_NAMES: [&str; 5] = ["none", "sub", "up", "average", "paeth"];
/// `prism_pack.FILTER_STRATEGIES` (adds the signed-residual rule).
const FILTER_STRATEGIES: [&str; 6] = ["none", "sub", "up", "average", "paeth", "residual"];
/// `prism_pack.ORDER_STRATEGIES` (v1 portfolio).
const ORDER_STRATEGIES: [&str; 5] = [
    "identity",
    "alpha-first",
    "frequency",
    "color-locality",
    "spatial-adjacency",
];
/// `prism_pack.V2_ORDER_STRATEGIES`.
const V2_ORDER_STRATEGIES: [&str; 9] = [
    "identity",
    "alpha-first",
    "frequency",
    "color-locality",
    "spatial-adjacency",
    "alpha-frequency",
    "alpha-color-locality",
    "alpha-spatial-adjacency",
    "packed-frequency",
];
/// `prism_pack.V2_FILTER_STRATEGIES` (adds trial-zlib).
const V2_FILTER_STRATEGIES: [&str; 7] = [
    "none",
    "sub",
    "up",
    "average",
    "paeth",
    "residual",
    "trial-zlib",
];

const V2_MAX_PRE_OPTIMIZER_VARIANTS: usize = 96;
const V2_LOCAL_MOVE_LIMIT: usize = 20;
const V2_NO_IMPROVEMENT_LIMIT: usize = 12;
const V2_ROW_CHANGE_LIMIT: usize = 16;
const V2_ZOPFLI_FINALIST_LIMIT: usize = 3;
const V2_ZOPFLI_ARGUMENTS: [&str; 1] = ["-m"];

/// Where `--pack max` looks for the Apache-2.0 `zopflipng` it shells out to,
/// in order:
///
/// 1. **`PRISM_ZOPFLIPNG`** — an explicit path, used verbatim. Every parity and
///    benchmark harness sets this, pinning the exact build the Python oracle
///    used, so byte-for-byte reproduction never depends on host discovery.
/// 2. **The research tree's vendored pinned build**, when this crate is checked
///    out inside the Prism monorepo — existence-checked, so it simply does not
///    apply anywhere else.
/// 3. **`zopflipng` on `PATH`** — the install a crate consumer actually has
///    (`brew install zopfli`, distro package, hand build).
///
/// **This order is load-bearing and must stay identical to the Python oracle's**
/// (`prism_pack.DEFAULT_ZOPFLIPNG`). The two implementations shell out to
/// zopflipng independently; if they resolve to DIFFERENT binaries their bytes
/// may diverge and the parity gates would be comparing two different programs.
/// The in-tree rung sits ABOVE `PATH` for exactly that reason: an ad-hoc in-tree
/// run with no env set must use the vendored pinned build on both sides, not
/// whatever the host happens to have installed.
///
/// The old behavior was an UNCONDITIONAL in-tree path, which for anyone holding
/// just the crate named a file that could not exist — turning a missing optional
/// tool into an error citing a nonsensical location. Existence-checking it keeps
/// in-tree behavior byte-identical while letting resolution continue elsewhere.
///
/// **Reproduction caveat:** a `PATH`-discovered zopflipng is not necessarily
/// the pinned build. Output stays lossless regardless — `run_zopflipng` decodes
/// and pixel-compares every result — but exact byte reproduction of published
/// figures requires `PRISM_ZOPFLIPNG`, which is why the harnesses set it rather
/// than relying on this lookup.
#[doc(hidden)]
pub fn default_zopflipng() -> Option<PathBuf> {
    resolve_zopflipng(
        std::env::var_os("PRISM_ZOPFLIPNG"),
        std::env::var_os("PATH"),
        |candidate| candidate.is_file(),
    )
}

/// The vendored pinned `zopflipng` inside the Prism research tree, if this crate
/// is checked out there. Mirrors `prism_pack._PRISM_ROOT / "benchmarks" / ...`.
fn in_tree_zopflipng() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../benchmarks/baselines/zopfli/work/zopfli/zopflipng"
    ))
}

/// The resolution policy of [`default_zopflipng`], with the process environment
/// and the filesystem passed in so it is testable without mutating either
/// (env mutation is `unsafe` in edition 2024 and races across test threads).
fn resolve_zopflipng(
    override_var: Option<std::ffi::OsString>,
    path_var: Option<std::ffi::OsString>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    // An explicit override is used verbatim, existence unchecked: if the caller
    // named a binary and got it wrong, `run_zopflipng` must say so about THAT
    // path rather than silently falling through to some other zopflipng.
    if let Some(path) = override_var
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    // In-tree pinned build BEFORE PATH — see `default_zopflipng`: the Rust and
    // Python implementations must resolve to the SAME binary or the parity
    // gates compare two different programs.
    let in_tree = in_tree_zopflipng();
    if exists(&in_tree) {
        return Some(in_tree);
    }
    std::env::split_paths(&path_var?)
        // An empty PATH entry means "the current directory" to the shell; do not
        // honor that here — it would let a stray ./zopflipng hijack the search.
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("zopflipng"))
        .find(|candidate| exists(candidate))
}

/// One complete pre-optimizer artifact plus the facts later stages consume
/// (`prism_pack._Variant` + the evidence fields that drive control flow: the
/// row-filter choices reused by local moves, and the palette RGBA bytes used
/// for finalist de-duplication).
#[derive(Debug, Clone)]
struct Variant {
    data: Vec<u8>,
    palette: Vec<Rgba>,
    indices: Vec<usize>,
    row_filters: Vec<usize>,
    palette_rgba: Vec<u8>,
}

// --- Input normalization and palette cleanup (`prism_pack` lines 181-273) -----

/// `prism_pack._normalize_inputs`: the range checks that can fail on degenerate
/// input (Rust's types already enforce the channel/bit constraints).
fn normalize_inputs(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
) -> Result<(), Error> {
    if width < 1 || height < 1 {
        return Err(Error::data(
            "width and height must be integers >= 1".to_string(),
        ));
    }
    if palette.is_empty() || palette.len() > 256 {
        return Err(Error::data(
            "palette must contain 1..256 entries".to_string(),
        ));
    }
    if indices.len() != width * height {
        return Err(Error::data(format!(
            "expected {} indices, got {}",
            width * height,
            indices.len()
        )));
    }
    for &index in indices {
        if index >= palette.len() {
            return Err(Error::data(format!("palette index {index} out of range")));
        }
    }
    Ok(())
}

/// Remove unused and duplicate RGBA entries while preserving pixels
/// (`prism_pack.cleanup_palette`). Used entries are visited in original index
/// order; identical RGBA collapses to the first used representative.
fn cleanup_palette(palette: &[Rgba], indices: &[usize]) -> Result<(Vec<Rgba>, Vec<usize>), Error> {
    if palette.is_empty() {
        return Err(Error::data("palette must not be empty".to_string()));
    }
    let mut used: Vec<usize> = indices.to_vec();
    used.sort_unstable();
    used.dedup();
    let Some(&max_used) = used.last() else {
        return Err(Error::data(
            "at least one palette index is required".to_string(),
        ));
    };
    if max_used >= palette.len() {
        return Err(Error::data(
            "palette index out of range during cleanup".to_string(),
        ));
    }
    let mut output: Vec<Rgba> = Vec::new();
    // rgba -> output index; first-appearance order among `used`.
    let mut rgba_to_output: Vec<(Rgba, usize)> = Vec::new();
    let mut old_to_output: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for &old_index in &used {
        let entry = palette[old_index];
        let new_index = match rgba_to_output.iter().find(|(rgba, _)| *rgba == entry) {
            Some((_, idx)) => *idx,
            None => {
                let idx = output.len();
                rgba_to_output.push((entry, idx));
                output.push(entry);
                idx
            }
        };
        old_to_output.insert(old_index, new_index);
    }
    let remapped: Vec<usize> = indices.iter().map(|i| old_to_output[i]).collect();
    Ok((output, remapped))
}

/// `prism_pack.minimum_bit_depth`.
fn minimum_bit_depth(palette_entries: usize) -> Result<usize, Error> {
    if !(1..=256).contains(&palette_entries) {
        return Err(Error::data(
            "palette entry count must be in 1..256".to_string(),
        ));
    }
    Ok(if palette_entries <= 2 {
        1
    } else if palette_entries <= 4 {
        2
    } else if palette_entries <= 16 {
        4
    } else {
        8
    })
}

/// Pack one indexed scanline MSB-first with deterministic zero padding
/// (`prism_pack.pack_index_row`).
fn pack_index_row(indices: &[usize], bit_depth: usize) -> Vec<u8> {
    if bit_depth == 8 {
        return indices.iter().map(|&i| i as u8).collect();
    }
    let mut output = vec![0u8; (indices.len() * bit_depth).div_ceil(8)];
    for (position, &index) in indices.iter().enumerate() {
        let bit_offset = position * bit_depth;
        let shift = 8 - bit_depth - (bit_offset % 8);
        output[bit_offset / 8] |= (index as u8) << shift;
    }
    output
}

// --- PNG row filters (`prism_pack` lines 311-426) ---------------------------

/// `prism_pack._paeth`.
fn paeth(left: i64, up: i64, upper_left: i64) -> i64 {
    let estimate = left + up - upper_left;
    let dl = (estimate - left).abs();
    let du = (estimate - up).abs();
    let dul = (estimate - upper_left).abs();
    if dl <= du && dl <= dul {
        left
    } else if du <= dul {
        up
    } else {
        upper_left
    }
}

/// Apply one PNG filter to serialized bytes, modulo 256 (`prism_pack.filter_row`,
/// `bpp=1`). `previous` is the prior UNFILTERED row.
fn filter_row(row: &[u8], previous: Option<&[u8]>, filter_type: usize) -> Vec<u8> {
    let bpp = 1usize;
    let prior: &[u8] = previous.unwrap_or(row); // length-matched; zeros used below
    let zero_prior = previous.is_none();
    let mut output = vec![0u8; row.len()];
    for offset in 0..row.len() {
        let left = if offset >= bpp {
            i64::from(row[offset - bpp])
        } else {
            0
        };
        let up = if zero_prior {
            0
        } else {
            i64::from(prior[offset])
        };
        let upper_left = if offset >= bpp && !zero_prior {
            i64::from(prior[offset - bpp])
        } else {
            0
        };
        let value = i64::from(row[offset]);
        let predictor = match filter_type {
            0 => 0,
            1 => left,
            2 => up,
            3 => (left + up) / 2,
            _ => paeth(left, up, upper_left),
        };
        output[offset] = ((value - predictor) & 0xFF) as u8;
    }
    output
}

/// `prism_pack._residual_score`.
fn residual_score(filtered: &[u8]) -> i64 {
    filtered
        .iter()
        .map(|&v| {
            if v < 128 {
                i64::from(v)
            } else {
                256 - i64::from(v)
            }
        })
        .sum()
}

/// Serialize filtered rows for a fixed strategy or the signed-residual rule
/// (`prism_pack.select_row_filters`). Returns (scanlines, per-row choices).
fn select_row_filters(rows: &[Vec<u8>], strategy: &str) -> Result<(Vec<u8>, Vec<usize>), Error> {
    if !FILTER_STRATEGIES.contains(&strategy) {
        return Err(Error::data(format!("unknown filter strategy: {strategy}")));
    }
    let mut output = Vec::new();
    let mut choices = Vec::with_capacity(rows.len());
    let mut previous: Option<&[u8]> = None;
    for row in rows {
        let (chosen, filtered) = if strategy == "residual" {
            let candidates: Vec<Vec<u8>> = (0..5).map(|k| filter_row(row, previous, k)).collect();
            let mut best = 0usize;
            let mut best_key = (residual_score(&candidates[0]), 0usize);
            for (k, cand) in candidates.iter().enumerate().skip(1) {
                let key = (residual_score(cand), k);
                if key < best_key {
                    best = k;
                    best_key = key;
                }
            }
            (best, candidates[best].clone())
        } else {
            // ch17 §31 surviving panic site (internal invariant, not data
            // path): reached only when `strategy != "residual"`, and the
            // function-entry check above already rejected anything not in
            // `FILTER_STRATEGIES` (= `FILTER_NAMES` + `"residual"`), so
            // `strategy` is guaranteed present in `FILTER_NAMES` here —
            // `strategy` is a fixed loop-invariant, never per-row data.
            let chosen = FILTER_NAMES.iter().position(|&n| n == strategy).unwrap();
            (chosen, filter_row(row, previous, chosen))
        };
        output.push(chosen as u8);
        output.extend_from_slice(&filtered);
        choices.push(chosen);
        previous = Some(row);
    }
    Ok((output, choices))
}

/// Serialize rows under caller-supplied per-row filter choices
/// (`prism_pack._serialize_row_filter_choices`).
fn serialize_row_filter_choices(
    rows: &[Vec<u8>],
    choices: &[usize],
) -> Result<(Vec<u8>, Vec<usize>), Error> {
    if rows.len() != choices.len() {
        return Err(Error::data(
            "row-filter choice count differs from image height".to_string(),
        ));
    }
    let mut output = Vec::new();
    let mut normalized = Vec::with_capacity(rows.len());
    let mut previous: Option<&[u8]> = None;
    for (row, &choice) in rows.iter().zip(choices.iter()) {
        if choice > 4 {
            return Err(Error::data(
                "row-filter choices must be integers in 0..4".to_string(),
            ));
        }
        output.push(choice as u8);
        output.extend_from_slice(&filter_row(row, previous, choice));
        normalized.push(choice);
        previous = Some(row);
    }
    Ok((output, normalized))
}

/// Choose each row by a copied zlib state and deterministic sync probe
/// (`prism_pack._trial_compression_row_filters`). The copied compressor is
/// flushed only for scoring; the retained state sees the chosen row without an
/// inserted flush boundary. STOP-spike-verified byte-identical to Python's
/// `zlib.compressobj(9)`.
fn trial_compression_row_filters(rows: &[Vec<u8>]) -> Result<(Vec<u8>, Vec<usize>), Error> {
    let mut compressor = zprobe::Deflater::new()
        .map_err(|e| Error::data(format!("trial-zlib deflate init failed: {e}")))?;
    let mut output = Vec::new();
    let mut choices = Vec::with_capacity(rows.len());
    let mut previous: Option<&[u8]> = None;
    for row in rows {
        let candidates: Vec<Vec<u8>> = (0..5).map(|k| filter_row(row, previous, k)).collect();
        let records: Vec<Vec<u8>> = (0..5)
            .map(|k| {
                let mut rec = Vec::with_capacity(candidates[k].len() + 1);
                rec.push(k as u8);
                rec.extend_from_slice(&candidates[k]);
                rec
            })
            .collect();
        let mut costs = [0usize; 5];
        for k in 0..5 {
            let mut probe = compressor.copy();
            let c1 = probe.compress(&records[k]);
            let c2 = probe.flush_sync();
            costs[k] = c1 + c2;
        }
        let mut chosen = 0usize;
        let mut best_key = (costs[0], 0usize);
        for (k, &cost) in costs.iter().enumerate().skip(1) {
            let key = (cost, k);
            if key < best_key {
                chosen = k;
                best_key = key;
            }
        }
        compressor.compress(&records[chosen]); // advance retained state
        output.extend_from_slice(&records[chosen]);
        choices.push(chosen);
        previous = Some(row);
    }
    Ok((output, choices))
}

// --- Palette-order heuristics (`prism_pack` lines 429-619) -------------------

/// `prism_pack._frequency`.
fn frequency(indices: &[usize], count: usize) -> Vec<i64> {
    let mut freq = vec![0i64; count];
    for &index in indices {
        freq[index] += 1;
    }
    freq
}

/// `prism_pack._spatial_order`.
fn spatial_order(
    palette: &[Rgba],
    indices: &[usize],
    width: usize,
    height: usize,
    members: Option<&[usize]>,
) -> Vec<usize> {
    let count = palette.len();
    let freq = frequency(indices, count);
    let mut adjacency = vec![vec![0i64; count]; count];
    for y in 0..height {
        let base = y * width;
        for x in 0..width {
            let here = indices[base + x];
            if x + 1 < width {
                let other = indices[base + x + 1];
                if here != other {
                    adjacency[here][other] += 1;
                    adjacency[other][here] += 1;
                }
            }
            if y + 1 < height {
                let other = indices[base + width + x];
                if here != other {
                    adjacency[here][other] += 1;
                    adjacency[other][here] += 1;
                }
            }
        }
    }
    let candidates: Vec<usize> = members
        .map(|m| m.to_vec())
        .unwrap_or_else(|| (0..count).collect());
    if candidates.is_empty() {
        return Vec::new();
    }
    // start = min by (-freq, palette, index)
    //
    // ch17 §31 surviving panic sites (internal invariant, not data path):
    // `min_by(...).unwrap()` is reached only after the `candidates.is_empty()`
    // early return above, so `candidates` (and thus the iterator) is
    // non-empty here; `order.last().unwrap()` below is safe because `order`
    // is seeded with `[start]` and only ever grows.
    let start = *candidates
        .iter()
        .min_by(|&&a, &&b| (-freq[a], palette[a], a).cmp(&(-freq[b], palette[b], b)))
        .unwrap();
    let mut order = vec![start];
    let mut remaining: Vec<usize> = candidates.into_iter().filter(|&i| i != start).collect();
    while !remaining.is_empty() {
        let last = *order.last().unwrap();
        let mut best_pos = 0usize;
        let mut best_key = spatial_key(&adjacency, &order, &freq, palette, last, remaining[0]);
        for (pos, &index) in remaining.iter().enumerate().skip(1) {
            let key = spatial_key(&adjacency, &order, &freq, palette, last, index);
            if key < best_key {
                best_pos = pos;
                best_key = key;
            }
        }
        order.push(remaining[best_pos]);
        remaining.remove(best_pos);
    }
    order
}

fn spatial_key(
    adjacency: &[Vec<i64>],
    order: &[usize],
    freq: &[i64],
    palette: &[Rgba],
    last: usize,
    index: usize,
) -> (i64, i64, i64, Rgba, usize) {
    let placed_sum: i64 = order.iter().map(|&placed| adjacency[placed][index]).sum();
    (
        -adjacency[last][index],
        -placed_sum,
        -freq[index],
        palette[index],
        index,
    )
}

/// `prism_pack._color_locality_order`.
fn color_locality_order(palette: &[Rgba], members: Option<&[usize]>) -> Vec<usize> {
    let count = palette.len();
    let candidates: Vec<usize> = members
        .map(|m| m.to_vec())
        .unwrap_or_else(|| (0..count).collect());
    if candidates.is_empty() {
        return Vec::new();
    }
    // ch17 §31 surviving panic sites (internal invariant, not data path):
    // same reasoning as `spatial_order` above — `candidates` is non-empty
    // here (the `is_empty()` early return already handled that case), and
    // `order` always holds at least `[start]`.
    let start = *candidates
        .iter()
        .min_by(|&&a, &&b| (palette[a], a).cmp(&(palette[b], b)))
        .unwrap();
    let mut order = vec![start];
    let mut remaining: Vec<usize> = candidates.into_iter().filter(|&i| i != start).collect();
    while !remaining.is_empty() {
        let last = palette[*order.last().unwrap()];
        let key = |index: usize| -> (i64, Rgba, usize) {
            let entry = palette[index];
            let distance = rgba_dist_sq(last, entry);
            (distance, entry, index)
        };
        let mut best_pos = 0usize;
        let mut best_key = key(remaining[0]);
        for (pos, &index) in remaining.iter().enumerate().skip(1) {
            let k = key(index);
            if k < best_key {
                best_pos = pos;
                best_key = k;
            }
        }
        order.push(remaining[best_pos]);
        remaining.remove(best_pos);
    }
    order
}

fn rgba_dist_sq(a: Rgba, b: Rgba) -> i64 {
    let d0 = i64::from(a.0) - i64::from(b.0);
    let d1 = i64::from(a.1) - i64::from(b.1);
    let d2 = i64::from(a.2) - i64::from(b.2);
    let d3 = i64::from(a.3) - i64::from(b.3);
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
}

/// `prism_pack._alpha_partitions`: (nonopaque, opaque) member indices.
fn alpha_partitions(palette: &[Rgba]) -> (Vec<usize>, Vec<usize>) {
    let nonopaque = (0..palette.len()).filter(|&i| palette[i].3 < 255).collect();
    let opaque = (0..palette.len())
        .filter(|&i| palette[i].3 == 255)
        .collect();
    (nonopaque, opaque)
}

/// `prism_pack._packed_frequency_order`.
fn packed_frequency_order(
    palette: &[Rgba],
    indices: &[usize],
    width: usize,
    height: usize,
) -> Result<Vec<usize>, Error> {
    let count = palette.len();
    let freq = frequency(indices, count);
    let per_byte = 8 / minimum_bit_depth(count)?;
    if per_byte == 1 {
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|&a, &b| (-freq[a], a).cmp(&(-freq[b], b)));
        return Ok(order);
    }
    let mut cooccurrence = vec![vec![0i64; count]; count];
    for y in 0..height {
        let row = &indices[y * width..(y + 1) * width];
        let mut start = 0;
        while start < width {
            let group = &row[start..(start + per_byte).min(width)];
            for left_position in 0..group.len() {
                for right_position in (left_position + 1)..group.len() {
                    let left = group[left_position];
                    let right = group[right_position];
                    if left != right {
                        cooccurrence[left][right] += 1;
                        cooccurrence[right][left] += 1;
                    }
                }
            }
            start += per_byte;
        }
    }
    let mut order: Vec<usize> = Vec::new();
    let mut remaining: Vec<usize> = (0..count).collect();
    while !remaining.is_empty() {
        let within_byte = order.len() % per_byte;
        let best_pos = if within_byte == 0 {
            let mut best = 0usize;
            let mut best_key = (-freq[remaining[0]], palette[remaining[0]], remaining[0]);
            for (pos, &index) in remaining.iter().enumerate().skip(1) {
                let key = (-freq[index], palette[index], index);
                if key < best_key {
                    best = pos;
                    best_key = key;
                }
            }
            best
        } else {
            let current_group = &order[order.len() - within_byte..];
            let key = |index: usize| -> (i64, Rgba, usize) {
                let cooc: i64 = current_group
                    .iter()
                    .map(|&placed| cooccurrence[placed][index])
                    .sum();
                (-cooc, palette[index], index)
            };
            let mut best = 0usize;
            let mut best_key = key(remaining[0]);
            for (pos, &index) in remaining.iter().enumerate().skip(1) {
                let k = key(index);
                if k < best_key {
                    best = pos;
                    best_key = k;
                }
            }
            best
        };
        order.push(remaining[best_pos]);
        remaining.remove(best_pos);
    }
    Ok(order)
}

/// Apply a deterministic bijective palette order and remap indices
/// (`prism_pack.permute_palette`).
fn permute_palette(
    palette: &[Rgba],
    indices: &[usize],
    width: usize,
    height: usize,
    strategy: &str,
) -> Result<(Vec<Rgba>, Vec<usize>), Error> {
    if !V2_ORDER_STRATEGIES.contains(&strategy) {
        return Err(Error::data(format!(
            "unknown palette-order strategy: {strategy}"
        )));
    }
    let count = palette.len();
    if count < 1 {
        return Err(Error::data("palette must not be empty".to_string()));
    }
    if indices.len() != width * height {
        return Err(Error::data(
            "index count does not match dimensions".to_string(),
        ));
    }
    let freq = frequency(indices, count);
    let order: Vec<usize> = match strategy {
        "identity" => (0..count).collect(),
        "alpha-first" => {
            let mut o: Vec<usize> = (0..count).collect();
            o.sort_by(|&a, &b| {
                (palette[a].3 == 255, palette[a].3, a).cmp(&(palette[b].3 == 255, palette[b].3, b))
            });
            o
        }
        "frequency" => {
            let mut o: Vec<usize> = (0..count).collect();
            o.sort_by(|&a, &b| (-freq[a], a).cmp(&(-freq[b], b)));
            o
        }
        "color-locality" => color_locality_order(palette, None),
        "spatial-adjacency" => spatial_order(palette, indices, width, height, None),
        "alpha-frequency" => {
            let (nonopaque, opaque) = alpha_partitions(palette);
            let mut o = nonopaque.clone();
            o.sort_by(|&a, &b| (-freq[a], palette[a].3, a).cmp(&(-freq[b], palette[b].3, b)));
            let mut op = opaque.clone();
            op.sort_by(|&a, &b| (-freq[a], a).cmp(&(-freq[b], b)));
            o.extend(op);
            o
        }
        "alpha-color-locality" => {
            let (nonopaque, opaque) = alpha_partitions(palette);
            let mut o = color_locality_order(palette, Some(&nonopaque));
            o.extend(color_locality_order(palette, Some(&opaque)));
            o
        }
        "alpha-spatial-adjacency" => {
            let (nonopaque, opaque) = alpha_partitions(palette);
            let mut o = spatial_order(palette, indices, width, height, Some(&nonopaque));
            o.extend(spatial_order(
                palette,
                indices,
                width,
                height,
                Some(&opaque),
            ));
            o
        }
        _ => packed_frequency_order(palette, indices, width, height)?,
    };
    let mut inverse = vec![0usize; count];
    for (new_index, &old_index) in order.iter().enumerate() {
        inverse[old_index] = new_index;
    }
    let new_palette: Vec<Rgba> = order.iter().map(|&i| palette[i]).collect();
    let new_indices: Vec<usize> = indices.iter().map(|&i| inverse[i]).collect();
    Ok((new_palette, new_indices))
}

// --- Variant encoding (`prism_pack` lines 622-694) --------------------------

/// `prism_pack._chunk` (reuses `png::emit_chunk`).
fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    png::emit_chunk(kind, payload)
}

/// zlib level-9 stream identical to Python `zlib.compress(data, 9)` (the
/// phase-1-validated `flate2` `zlib` backend).
fn zlib_compress9(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
    encoder.write_all(data).expect("write to Vec");
    encoder.finish().expect("finish zlib stream")
}

// --- E-0036 pack seams (`pngprism._seam_*`; T-0188 spec, T-0192 adoption) -
//
// The pack=none emission path can trial a handful of byte-only techniques and
// keep the smallest stream that re-decodes pixel-identical to the baseline:
//   ARM-S palette-sort trials (popularity / luminance / channel-major orders),
//   ARM-R reduction rungs (minimum-bit-depth repack + transparent-front tRNS
//   trim), ARM-M memLevel race (memLevel 5 alongside the baseline 8).
// The baseline (identity order, 8-bit, memLevel 8) is always a candidate, so
// the result is never larger than `stage_emit`. Composition is resolved by the
// caller (`quant::quantize_png_with_parallelism`): default-on for S and R when
// `--pack none` and no seam flag is named, frozen-off for unnamed peers once
// any seam flag is explicit, and all-off under `--pack fast|max`.

/// `pngprism._seam_remap_by_order`: apply a bijective palette permutation
/// `order` (new position -> old index) and consistently remap indices. The
/// per-pixel color sequence is invariant.
fn seam_remap_by_order(
    palette: &[Rgba],
    indices: &[usize],
    order: &[usize],
) -> (Vec<Rgba>, Vec<usize>) {
    let count = palette.len();
    let mut inverse = vec![0usize; count];
    for (new_index, &old_index) in order.iter().enumerate() {
        inverse[old_index] = new_index;
    }
    let new_palette: Vec<Rgba> = order.iter().map(|&old| palette[old]).collect();
    let new_indices: Vec<usize> = indices.iter().map(|&i| inverse[i]).collect();
    (new_palette, new_indices)
}

/// `pngprism._seam_order_popularity`: most-frequent index first
/// (ties -> lower old index).
fn seam_order_popularity(palette: &[Rgba], indices: &[usize]) -> Vec<usize> {
    let mut frequency = vec![0usize; palette.len()];
    for &index in indices {
        frequency[index] += 1;
    }
    let mut order: Vec<usize> = (0..palette.len()).collect();
    // key = (-frequency[i], i) ascending: frequency descending, index ascending.
    order.sort_by(|&a, &b| frequency[b].cmp(&frequency[a]).then(a.cmp(&b)));
    order
}

/// `pngprism._seam_order_luminance`: Rec.601 integer luma ascending
/// (ties -> old index).
fn seam_order_luminance(palette: &[Rgba]) -> Vec<usize> {
    let luma = |i: usize| -> i64 {
        let (red, green, blue, _) = palette[i];
        299 * i64::from(red) + 587 * i64::from(green) + 114 * i64::from(blue)
    };
    let mut order: Vec<usize> = (0..palette.len()).collect();
    order.sort_by(|&a, &b| luma(a).cmp(&luma(b)).then(a.cmp(&b)));
    order
}

/// `pngprism._seam_order_channel_major`: RGBA tuple ascending
/// (ties -> old index). Rust tuple `Ord` is lexicographic, matching Python's
/// tuple comparison.
fn seam_order_channel_major(palette: &[Rgba]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..palette.len()).collect();
    order.sort_by(|&a, &b| palette[a].cmp(&palette[b]).then(a.cmp(&b)));
    order
}

/// `pngprism._seam_order_transparent_front`: when EXACTLY one palette entry
/// is non-opaque and it is fully transparent (alpha == 0), move it to index 0
/// so the emitted tRNS payload trims to a single byte. Returns `None` when
/// inapplicable.
fn seam_order_transparent_front(palette: &[Rgba]) -> Option<Vec<usize>> {
    let nonopaque: Vec<usize> = (0..palette.len()).filter(|&i| palette[i].3 < 255).collect();
    if nonopaque.len() != 1 || palette[nonopaque[0]].3 != 0 {
        return None;
    }
    let transparent = nonopaque[0];
    let mut order = Vec::with_capacity(palette.len());
    order.push(transparent);
    order.extend((0..palette.len()).filter(|&i| i != transparent));
    Some(order)
}

/// `pngprism._seam_emit_config`: emit a color-type-3 PNG mirroring
/// `png::write_indexed_png`, parameterized by index bit depth and DEFLATE
/// memLevel. `bit_depth = 8` + `mem_level = 8` reproduces the baseline
/// `stage_emit` bytes byte-for-byte (memLevel 8 == `zlib.compress(., 9)`).
fn seam_emit_config(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
    bit_depth: usize,
    mem_level: i32,
) -> Vec<u8> {
    let mut scanlines = Vec::new();
    for y in 0..height {
        scanlines.push(0u8); // filter type 0 (None), as in the baseline emit
        let row = &indices[y * width..(y + 1) * width];
        scanlines.extend_from_slice(&pack_index_row(row, bit_depth));
    }
    let compressed = if mem_level == 8 {
        zlib_compress9(&scanlines)
    } else {
        zprobe::deflate_level9_memlevel(&scanlines, mem_level)
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(bit_depth as u8);
    ihdr.extend_from_slice(&[3, 0, 0, 0]); // color type 3, compression/filter/interlace 0
    let mut plte = Vec::with_capacity(palette.len() * 3);
    for &(red, green, blue, _) in palette {
        plte.push(red);
        plte.push(green);
        plte.push(blue);
    }
    let mut last_transparent: Option<usize> = None;
    for (position, &(_, _, _, alpha)) in palette.iter().enumerate() {
        if alpha < 255 {
            last_transparent = Some(position);
        }
    }
    let mut out = Vec::from(PNG_SIGNATURE);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"PLTE", &plte));
    if let Some(last) = last_transparent {
        let trns: Vec<u8> = palette[..=last].iter().map(|&(_, _, _, a)| a).collect();
        out.extend_from_slice(&chunk(b"tRNS", &trns));
    }
    out.extend_from_slice(&chunk(b"IDAT", &compressed));
    out.extend_from_slice(&chunk(b"IEND", b""));
    out
}

/// Deterministic seam tie-break key: `(len, mem5?0:1, order_rank, depth8?0:1)`;
/// the lexicographically smallest key wins (mirrors the oracle's `best[0]`).
type SeamKey = (usize, u8, usize, u8);

/// `pngprism._seam_emit`: trial the enabled byte-only pack-seam techniques
/// and return the SMALLEST stream that re-decodes pixel-identical to the
/// baseline, mirroring the oracle's deterministic tie-break exactly.
pub(crate) fn seam_emit(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
    palette_sort: bool,
    memlevel_race: bool,
    reduction: bool,
) -> Result<Vec<u8>, Error> {
    let count = palette.len();
    let expected: Vec<Rgba> = indices.iter().map(|&i| palette[i]).collect();

    // Candidate palette orderings (new position -> old index). Identity is the
    // baseline order and is always present (rank 0).
    let mut orders: Vec<(&'static str, Vec<usize>)> = vec![("identity", (0..count).collect())];
    if palette_sort {
        orders.push(("popularity", seam_order_popularity(palette, indices)));
        orders.push(("luminance", seam_order_luminance(palette)));
        orders.push(("channel-major", seam_order_channel_major(palette)));
    }
    if reduction {
        if let Some(front) = seam_order_transparent_front(palette) {
            orders.push(("trns-front", front));
        }
    }

    // Candidate index bit depths (ARM-R reduction rungs); 8 is the baseline.
    let mut depths = vec![8usize];
    if reduction {
        let min_depth = minimum_bit_depth(count)?;
        if min_depth < 8 {
            depths.push(min_depth);
        }
    }

    // Candidate DEFLATE memLevels (ARM-M race); 8 is the baseline.
    let mut mems = vec![8i32];
    if memlevel_race {
        mems.push(5);
    }

    // Deterministic tie-break key (applied only among size-ties), lower wins:
    //   (len, mem5?0:1, order_rank, depth8?0:1). mem 5 preferred over 8, then
    // lower order rank (identity first), then depth 8. `<` is strict, so the
    // first trial (identity/8/8) is the baseline and only a strictly smaller
    // key replaces it — the oracle's `best is None or key < best[0]`.
    let mut best: Option<(SeamKey, Vec<u8>)> = None;
    for (rank, (_order_name, order)) in orders.iter().enumerate() {
        let (permuted_palette, permuted_indices) = seam_remap_by_order(palette, indices, order);
        for &bit_depth in &depths {
            for &mem_level in &mems {
                let data = seam_emit_config(
                    width,
                    height,
                    &permuted_palette,
                    &permuted_indices,
                    bit_depth,
                    mem_level,
                );
                // Independent decoded-pixel identity gate (per trial).
                let decoded = png::decode_png(&data).map_err(|e| {
                    Error::internal(format!("internal: seam trial failed self-decode: {e}"))
                })?;
                if (decoded.width as usize, decoded.height as usize) != (width, height) {
                    return Err(Error::internal(
                        "internal: seam trial changed dimensions".to_string(),
                    ));
                }
                if decoded.pixels != expected {
                    return Err(Error::internal(
                        "internal: seam trial failed decoded-pixel identity".to_string(),
                    ));
                }
                let key = (
                    data.len(),
                    if mem_level == 5 { 0u8 } else { 1 },
                    rank,
                    if bit_depth == 8 { 0u8 } else { 1 },
                );
                if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
                    best = Some((key, data));
                }
            }
        }
    }
    Ok(best
        .expect("seam_emit always evaluates the identity/8-bit/memLevel-8 baseline")
        .1)
}

/// Build one complete pre-optimizer PNG and verify decoded-pixel identity
/// (`prism_pack._encode_variant`).
fn encode_variant(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
    row_filter_choices: Option<&[usize]>,
    filter_strategy: &str,
) -> Result<Variant, Error> {
    let bit_depth = minimum_bit_depth(palette.len())?;
    let rows: Vec<Vec<u8>> = (0..height)
        .map(|y| pack_index_row(&indices[y * width..(y + 1) * width], bit_depth))
        .collect();
    let (scanlines, row_filters) = if let Some(choices) = row_filter_choices {
        serialize_row_filter_choices(&rows, choices)?
    } else if filter_strategy == "trial-zlib" {
        trial_compression_row_filters(&rows)?
    } else {
        select_row_filters(&rows, filter_strategy)?
    };
    let compressed = zlib_compress9(&scanlines);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(bit_depth as u8);
    ihdr.extend_from_slice(&[3, 0, 0, 0]); // color type 3, compression 0, filter 0, interlace 0
    let mut plte = Vec::with_capacity(palette.len() * 3);
    for &(r, g, b, _) in palette {
        plte.push(r);
        plte.push(g);
        plte.push(b);
    }
    // last non-opaque index; tRNS covers palette[..=last_nonopaque] alphas.
    let last_nonopaque: i64 = palette
        .iter()
        .enumerate()
        .filter(|(_, e)| e.3 < 255)
        .map(|(i, _)| i as i64)
        .max()
        .unwrap_or(-1);
    let trns: Vec<u8> = if last_nonopaque >= 0 {
        palette[..=(last_nonopaque as usize)]
            .iter()
            .map(|e| e.3)
            .collect()
    } else {
        Vec::new()
    };
    let mut output = Vec::from(PNG_SIGNATURE);
    output.extend_from_slice(&chunk(b"IHDR", &ihdr));
    output.extend_from_slice(&chunk(b"PLTE", &plte));
    if !trns.is_empty() {
        output.extend_from_slice(&chunk(b"tRNS", &trns));
    }
    output.extend_from_slice(&chunk(b"IDAT", &compressed));
    output.extend_from_slice(&chunk(b"IEND", b""));

    // Independent decode + pixel-identity verification.
    let decoded = png::decode_png(&output)
        .map_err(|e| Error::data(format!("independent decode of packing variant failed: {e}")))?;
    if (decoded.width as usize, decoded.height as usize) != (width, height) {
        return Err(Error::data(
            "packing variant decoded with different dimensions".to_string(),
        ));
    }
    let expected: Vec<Rgba> = indices.iter().map(|&i| palette[i]).collect();
    if decoded.pixels != expected {
        return Err(Error::data(
            "packing variant failed decoded-pixel identity verification".to_string(),
        ));
    }
    let palette_rgba: Vec<u8> = palette
        .iter()
        .flat_map(|&(r, g, b, a)| [r, g, b, a])
        .collect();
    Ok(Variant {
        data: output,
        palette: palette.to_vec(),
        indices: indices.to_vec(),
        row_filters,
        palette_rgba,
    })
}

// --- V2 local search (`prism_pack` lines 697-878) ---------------------------

/// `prism_pack._spread_positions`.
fn spread_positions(length: i64, count: i64) -> Vec<usize> {
    if length <= 0 || count <= 0 {
        return Vec::new();
    }
    if length <= count {
        return (0..length as usize).collect();
    }
    (0..count)
        .map(|step| ((step * (length - 1)) / (count - 1)) as usize)
        .collect()
}

/// `prism_pack._apply_position_order`.
fn apply_position_order(
    palette: &[Rgba],
    indices: &[usize],
    position_order: &[usize],
) -> Result<(Vec<Rgba>, Vec<usize>), Error> {
    let count = palette.len();
    let mut sorted = position_order.to_vec();
    sorted.sort_unstable();
    if sorted != (0..count).collect::<Vec<_>>() {
        return Err(Error::data(
            "local palette move is not bijective".to_string(),
        ));
    }
    let mut inverse = vec![0usize; count];
    for (new_index, &old_index) in position_order.iter().enumerate() {
        inverse[old_index] = new_index;
    }
    let new_palette: Vec<Rgba> = position_order.iter().map(|&i| palette[i]).collect();
    let new_indices: Vec<usize> = indices.iter().map(|&i| inverse[i]).collect();
    Ok((new_palette, new_indices))
}

/// Bounded A5 neighborhood: swaps, insertions, blocks, alpha-safe swaps
/// (`prism_pack._local_position_moves`). Returns the ordered move list (labels
/// are oracle observability only; the ORDER and the permutations are binding).
fn local_position_moves(palette: &[Rgba]) -> Vec<Vec<usize>> {
    let count = palette.len();
    if count < 2 {
        return Vec::new();
    }
    let identity: Vec<usize> = (0..count).collect();
    let mut moves: Vec<Vec<usize>> = Vec::new();
    let mut seen: Vec<Vec<usize>> = vec![identity.clone()];
    let add = |order: Vec<usize>, moves: &mut Vec<Vec<usize>>, seen: &mut Vec<Vec<usize>>| {
        if !seen.contains(&order) && moves.len() < V2_LOCAL_MOVE_LIMIT {
            seen.push(order.clone());
            moves.push(order);
        }
    };

    for position in spread_positions((count - 1) as i64, 7) {
        let mut order = identity.clone();
        order.swap(position, position + 1);
        add(order, &mut moves, &mut seen);
    }

    let distance = std::cmp::max(2, count / 7);
    for source in spread_positions(count as i64, 6) {
        let target = if source + distance < count {
            source + distance
        } else {
            source.saturating_sub(distance)
        };
        let mut order = identity.clone();
        let entry = order.remove(source);
        order.insert(target, entry);
        add(order, &mut moves, &mut seen);
    }

    if count >= 4 {
        for start in spread_positions((count - 3) as i64, 4) {
            let mut order = identity.clone();
            // order[start:start+4] = order[start+2:start+4] + order[start:start+2]
            let block: Vec<usize> = order[start + 2..start + 4]
                .iter()
                .chain(order[start..start + 2].iter())
                .copied()
                .collect();
            order[start..start + 4].copy_from_slice(&block);
            add(order, &mut moves, &mut seen);
        }
    }

    let (nonopaque, opaque) = alpha_partitions(palette);
    for group in [nonopaque, opaque] {
        if group.len() >= 2 {
            for offset in spread_positions((group.len() as i64) - 2, 3) {
                let left = group[offset];
                let right = group[offset + 2];
                let mut order = identity.clone();
                order.swap(left, right);
                add(order, &mut moves, &mut seen);
            }
        }
    }
    moves
}

/// Run the fixed-budget ch19 A5 pre-optimizer search (`prism_pack._build_v2_variants`).
fn build_v2_variants(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
) -> Result<Vec<Variant>, Error> {
    let mut variants: Vec<Variant> = Vec::new();
    for order_strategy in V2_ORDER_STRATEGIES {
        let (ordered_palette, ordered_indices) =
            permute_palette(palette, indices, width, height, order_strategy)?;
        for filter_strategy in V2_FILTER_STRATEGIES {
            variants.push(encode_variant(
                width,
                height,
                &ordered_palette,
                &ordered_indices,
                None,
                filter_strategy,
            )?);
        }
    }

    let mut current = min_variant_index(&variants);
    let mut current_palette = variants[current].palette.clone();
    let mut current_indices = variants[current].indices.clone();
    let mut current_row_filters = variants[current].row_filters.clone();
    let mut local_moves_tested = 0usize;
    let mut consecutive_without_improvement = 0usize;

    for position_order in local_position_moves(&current_palette) {
        if variants.len() >= V2_MAX_PRE_OPTIMIZER_VARIANTS {
            break;
        }
        let (moved_palette, moved_indices) =
            apply_position_order(&current_palette, &current_indices, &position_order)?;
        let candidate = encode_variant(
            width,
            height,
            &moved_palette,
            &moved_indices,
            Some(&current_row_filters),
            "row-carry",
        )?;
        let candidate_len = candidate.data.len();
        let candidate_palette = candidate.palette.clone();
        let candidate_indices = candidate.indices.clone();
        let candidate_row_filters = candidate.row_filters.clone();
        variants.push(candidate);
        local_moves_tested += 1;
        if candidate_len < variants[current].data.len() {
            current = variants.len() - 1;
            current_palette = candidate_palette;
            current_indices = candidate_indices;
            current_row_filters = candidate_row_filters;
            consecutive_without_improvement = 0;
        } else {
            consecutive_without_improvement += 1;
        }

        // Alternate row-assignment retest every 8 moves.
        if local_moves_tested.is_multiple_of(8) {
            for strategy in ["residual", "trial-zlib"] {
                if variants.len() >= V2_MAX_PRE_OPTIMIZER_VARIANTS {
                    break;
                }
                let alternate = encode_variant(
                    width,
                    height,
                    &current_palette,
                    &current_indices,
                    None,
                    strategy,
                )?;
                let alt_len = alternate.data.len();
                let alt_row_filters = alternate.row_filters.clone();
                variants.push(alternate);
                if alt_len < variants[current].data.len() {
                    current = variants.len() - 1;
                    current_row_filters = alt_row_filters;
                    consecutive_without_improvement = 0;
                }
            }
        }
        if consecutive_without_improvement >= V2_NO_IMPROVEMENT_LIMIT {
            break;
        }
    }

    current = min_variant_index(&variants);
    let row_current_palette = variants[current].palette.clone();
    let row_current_indices = variants[current].indices.clone();
    let mut row_choices = variants[current].row_filters.clone();
    let mut row_changes_tested = 0usize;
    let row_budget = std::cmp::min(
        V2_ROW_CHANGE_LIMIT,
        V2_MAX_PRE_OPTIMIZER_VARIANTS - variants.len(),
    );
    let rows_to_try = row_budget / 4;
    let row_positions = spread_positions(height as i64, rows_to_try as i64);
    for row_position in row_positions {
        let original_choice = row_choices[row_position];
        for filter_type in 0..5 {
            if filter_type == original_choice || row_changes_tested >= row_budget {
                continue;
            }
            let mut candidate_choices = row_choices.clone();
            candidate_choices[row_position] = filter_type;
            let candidate = encode_variant(
                width,
                height,
                &row_current_palette,
                &row_current_indices,
                Some(&candidate_choices),
                "row-search",
            )?;
            let candidate_len = candidate.data.len();
            variants.push(candidate);
            row_changes_tested += 1;
            if candidate_len < variants[current].data.len() {
                current = variants.len() - 1;
                row_choices = candidate_choices;
            }
        }
    }
    if variants.len() > V2_MAX_PRE_OPTIMIZER_VARIANTS {
        return Err(Error::internal(
            "internal: v2 packing search exceeded its declared budget".to_string(),
        ));
    }
    Ok(variants)
}

/// First-minimal variant by (data length, generation index).
fn min_variant_index(variants: &[Variant]) -> usize {
    let mut best = 0usize;
    for i in 1..variants.len() {
        if variants[i].data.len() < variants[best].data.len() {
            best = i;
        }
    }
    best
}

// --- zopflipng subprocess (`prism_pack` lines 881-1026) ---------------------

/// Decode both PNGs and assert decoded-pixel identity (`prism_pack._assert_pixel_identity`).
fn assert_pixel_identity(reference: &[u8], candidate: &[u8]) -> Result<(), Error> {
    let expected = png::decode_png(reference)
        .map_err(|e| Error::data(format!("independent decode failed: {e}")))?;
    let observed = png::decode_png(candidate)
        .map_err(|e| Error::data(format!("independent decode failed: {e}")))?;
    if (expected.width, expected.height) != (observed.width, observed.height) {
        return Err(Error::data(
            "optimizer changed decoded dimensions".to_string(),
        ));
    }
    if expected.pixels != observed.pixels {
        return Err(Error::data("optimizer changed decoded pixels".to_string()));
    }
    Ok(())
}

/// Hard ceiling on one `zopflipng` invocation (ops hygiene, tri-review
/// kimi F10): without a bound, a hung or pathologically slow subprocess
/// blocks the pack search forever on a single candidate. Generous by
/// default — legitimate `-m` max-mode runs on large images are already
/// slow by design. `PRISM_ZOPFLIPNG_TIMEOUT_SECS` overrides it (same
/// env-override convention as `PRISM_ZOPFLIPNG`); non-positive or
/// unparseable values fall back to the default rather than erroring.
const ZOPFLI_DEFAULT_TIMEOUT_SECS: u64 = 120;

fn zopfli_timeout() -> std::time::Duration {
    let secs = std::env::var("PRISM_ZOPFLIPNG_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .unwrap_or(ZOPFLI_DEFAULT_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Spawn `cmd`, wait up to `timeout`, and kill (+ reap) it on expiry.
/// Drains stdout/stderr on their own threads while polling for exit —
/// the same strategy `std::process::Command::output` uses internally —
/// so the child cannot deadlock writing to a full pipe while this
/// thread is busy polling rather than reading.
///
/// How long to wait for a drain thread to report EOF after the child we
/// spawned has already exited (or been killed+reaped). Bounded on
/// purpose — see the safety note on `drain_async` below.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Fixed size of the single, reused chunk buffer `drain_async` reads
/// into. Never grown, never accumulated — see the doc comment below for
/// why that matters.
const DRAIN_CHUNK_SIZE: usize = 8192;

/// Spawn a reader thread that drains `pipe` in fixed-size chunks
/// (discarding each one immediately) until EOF, a read error, or
/// `abandon` is observed set, and signals completion on the returned
/// channel. Also returns the `JoinHandle` for tests that need to prove
/// the thread actually exits; production callers (`spawn_with_timeout`)
/// must discard it — see point 1 below.
///
/// Safety/correctness note (regression, T-0135 cross-review
/// `openai-gpt5-39`, two rounds):
///
/// 1. **Never `JoinHandle::join()` this thread from `spawn_with_timeout`.**
///    A real `zopflipng` override (or, in principle, the real binary)
///    can spawn a descendant that inherits the piped stdout/stderr write
///    ends; a pipe's read side does not see EOF while ANY process still
///    holds the write end open, so once our direct child is
///    killed+reaped, a blocking read on that pipe does not return on its
///    own — it can block until the SURVIVING DESCENDANT itself exits,
///    which can be forever (round 1: observed reparented to PID 1,
///    outliving the configured timeout indefinitely). The caller signals
///    `abandon` and waits on the completion channel with a bounded
///    `recv_timeout` instead of joining.
/// 2. **Never accumulate an unbounded buffer.** The round-1 fix still
///    called `read_to_end` into a private `Vec` — harmless for a
///    quiet/finite descendant, but a CONTINUOUSLY WRITING one (round-2
///    finding) keeps that `Vec` growing for as long as the thread is
///    abandoned-but-alive, since the `tx.send` at the end is never
///    reached. Reading into one small, reused, fixed-size buffer
///    (`DRAIN_CHUNK_SIZE`) and discarding each chunk immediately bounds
///    this thread's own memory use to that one buffer, independent of
///    runtime or total bytes written — strictly stronger than a
///    byte-count cap-then-discard scheme (no cap bookkeeping needed: we
///    never hold more than one chunk at a time regardless of the total).
/// 3. **The thread must actually exit, releasing its owned fd.** `pipe`
///    is OWNED by this thread (moved in); the underlying fd closes via
///    ordinary `Drop` the instant the thread function returns — there is
///    exactly one owner and therefore no double-close race, unlike an
///    external force-close from the caller's thread (which would target
///    the same fd number a blocked syscall is using — a real fd-reuse
///    hazard the caller does not need to take on). `abandon` is checked
///    between chunks so the loop notices promptly and returns for a
///    WRITING descendant (each `read` call returns quickly, since more
///    data keeps arriving) — exactly the case this fix's regression
///    tests exercise. A descendant that stops writing but keeps the pipe
///    open without closing it is a narrower, separate case an
///    between-chunks check cannot bound (the blocking `read` call itself
///    would not return); that would need non-blocking I/O or
///    process-group teardown, is not exercised by either reviewed
///    reproduction, and is tracked as follow-up rather than fixed here.
fn drain_async<R: std::io::Read + Send + 'static>(
    mut pipe: R,
    abandon: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (std::sync::mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; DRAIN_CHUNK_SIZE];
        loop {
            if abandon.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break, // EOF, or the pipe/fd went away.
                Ok(_) => {}              // Discard this chunk; read the next one.
            }
        }
        // Dropping `pipe` here (end of scope) closes its owned fd.
        let _ = tx.send(());
    });
    (rx, handle)
}

fn spawn_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let abandon = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut child = cmd.spawn()?;
    let stdout_done = child
        .stdout
        .take()
        .map(|pipe| drain_async(pipe, std::sync::Arc::clone(&abandon)));
    let stderr_done = child
        .stderr
        .take()
        .map(|pipe| drain_async(pipe, std::sync::Arc::clone(&abandon)));

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    // Bounded wait for drain (never an unbounded `join()` — see
    // `drain_async`'s doc comment): the normal case (no pipe-inheriting
    // descendant) reports back in microseconds once the direct child's
    // own fds close. Past the grace period, signal `abandon` — shared by
    // both streams, so one timeout is enough to tell both drain threads
    // to stop — and move on without joining either `JoinHandle`.
    let mut drained = true;
    if let Some((rx, _handle)) = stdout_done {
        drained &= rx.recv_timeout(DRAIN_GRACE).is_ok();
    }
    if let Some((rx, _handle)) = stderr_done {
        drained &= rx.recv_timeout(DRAIN_GRACE).is_ok();
    }
    if !drained {
        abandon.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(status)
}

/// Run the pinned `zopflipng` on `input_png`, verify pixel identity, return the
/// optimized bytes (`prism_pack._run_zopflipng`).
fn run_zopflipng(
    input_png: &[u8],
    binary: &Path,
    extra_arguments: &[&str],
) -> Result<Vec<u8>, Error> {
    use std::process::{Command, Stdio};
    if !binary.is_file() {
        return Err(Error::data(format!(
            "zopflipng is not an executable file: {}",
            binary.display()
        )));
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(format!("prism-pack-zopfli-{unique}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::data(format!("cannot create zopfli temp dir: {e}")))?;
    let source = dir.join("input.png");
    let output = dir.join("output.png");
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&dir);
    };
    if let Err(e) = std::fs::write(&source, input_png) {
        cleanup();
        return Err(Error::data(format!("cannot write zopfli input: {e}")));
    }
    let mut cmd = Command::new(binary);
    cmd.arg("-y");
    for arg in extra_arguments {
        cmd.arg(arg);
    }
    cmd.arg(&source)
        .arg(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let timeout = zopfli_timeout();
    let result = spawn_with_timeout(cmd, timeout);
    let status = match result {
        Ok(Some(status)) => status,
        Ok(None) => {
            cleanup();
            return Err(Error::data(format!(
                "zopflipng timed out after {}s",
                timeout.as_secs()
            )));
        }
        Err(e) => {
            cleanup();
            return Err(Error::data(format!("cannot execute zopflipng: {e}")));
        }
    };
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        cleanup();
        return Err(Error::data(format!("zopflipng exited {code}")));
    }
    if !output.is_file() {
        cleanup();
        return Err(Error::data(
            "zopflipng succeeded without creating output".to_string(),
        ));
    }
    let optimized = match std::fs::read(&output) {
        Ok(bytes) => bytes,
        Err(e) => {
            cleanup();
            return Err(Error::data(format!("cannot read zopfli output: {e}")));
        }
    };
    cleanup();
    assert_pixel_identity(input_png, &optimized)?;
    Ok(optimized)
}

// --- Top-level pack (`prism_pack.pack_indexed_png`) -------------------------

/// Search complete indexed-PNG artifacts and return the smallest bytes. `mode`
/// is `"fast"` or `"max"`; `search_version` is `"v1"` or `"v2"`. Mirrors
/// `prism_pack.pack_indexed_png` restricted to the integrated CLI's usage
/// (fixed default portfolios; no custom portfolio path).
pub fn pack_indexed_png(
    width: usize,
    height: usize,
    palette: &[Rgba],
    indices: &[usize],
    mode: &str,
    search_version: &str,
) -> Result<Vec<u8>, Error> {
    normalize_inputs(width, height, palette, indices)?;
    let (clean_palette, clean_indices) = cleanup_palette(palette, indices)?;
    // Cleanup must preserve decoded pixels.
    for (&old, &new) in indices.iter().zip(clean_indices.iter()) {
        if palette[old] != clean_palette[new] {
            return Err(Error::data(
                "palette cleanup changed decoded pixels".to_string(),
            ));
        }
    }
    if mode != "fast" && mode != "max" {
        return Err(Error::data(
            "mode must be 'fast', 'max', or 'zopfli'".to_string(),
        ));
    }
    if search_version != "v1" && search_version != "v2" {
        return Err(Error::data(
            "search_version must be 'v1' or 'v2'".to_string(),
        ));
    }

    let variants: Vec<Variant> = if search_version == "v1" {
        let mut v = Vec::new();
        for order_strategy in ORDER_STRATEGIES {
            let (ordered_palette, ordered_indices) = permute_palette(
                &clean_palette,
                &clean_indices,
                width,
                height,
                order_strategy,
            )?;
            for filter_strategy in FILTER_STRATEGIES {
                v.push(encode_variant(
                    width,
                    height,
                    &ordered_palette,
                    &ordered_indices,
                    None,
                    filter_strategy,
                )?);
            }
        }
        v
    } else {
        build_v2_variants(width, height, &clean_palette, &clean_indices)?
    };

    let selected = min_variant_index(&variants);

    if mode == "max" {
        let binary = default_zopflipng().ok_or_else(|| {
            Error::data(
                "--pack max needs the `zopflipng` binary, which was not found on PATH. \
                 Install it (macOS: `brew install zopfli`; most distros package it as \
                 `zopfli`), or set PRISM_ZOPFLIPNG to its path. Other --pack modes \
                 (none, fast) do not need it."
                    .to_string(),
            )
        })?;
        // ranked by (len, gen index) — enumerate order is generation order.
        let mut ranked: Vec<usize> = (0..variants.len()).collect();
        ranked.sort_by(|&a, &b| (variants[a].data.len(), a).cmp(&(variants[b].data.len(), b)));
        let zopfli_limit = if search_version == "v2" {
            V2_ZOPFLI_FINALIST_LIMIT
        } else {
            1
        };
        let mut finalists: Vec<usize> = Vec::new();
        let mut seen_palettes: Vec<Vec<u8>> = Vec::new();
        for &idx in &ranked {
            let digest = &variants[idx].palette_rgba;
            if seen_palettes.iter().any(|p| p == digest) {
                continue;
            }
            seen_palettes.push(digest.clone());
            finalists.push(idx);
            if finalists.len() >= zopfli_limit {
                break;
            }
        }
        let extra: Vec<&str> = if search_version == "v2" {
            V2_ZOPFLI_ARGUMENTS.to_vec()
        } else {
            Vec::new()
        };
        let mut optimized: Vec<Vec<u8>> = Vec::new();
        for &idx in &finalists {
            optimized.push(run_zopflipng(&variants[idx].data, &binary, &extra)?);
        }
        // min by (optimized len, finalist index)
        let mut best = 0usize;
        for i in 1..optimized.len() {
            if optimized[i].len() < optimized[best].len() {
                best = i;
            }
        }
        Ok(optimized[best].clone())
    } else {
        Ok(variants[selected].data.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- zopflipng resolution (PRISM_ZOPFLIPNG, else PATH) ------------------

    fn os(value: &str) -> Option<std::ffi::OsString> {
        Some(std::ffi::OsString::from(value))
    }

    /// An `exists` probe that denies the in-tree pinned build, so a test can
    /// exercise the PATH rung regardless of whether this checkout actually has
    /// the vendored zopfli build present.
    fn no_in_tree(found: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let found: Vec<PathBuf> = found.iter().map(PathBuf::from).collect();
        move |candidate: &Path| found.iter().any(|f| f == candidate)
    }

    #[test]
    fn zopflipng_override_wins_and_is_not_existence_checked() {
        // A wrong explicit path must survive resolution so `run_zopflipng`
        // reports THAT path, rather than silently using a different binary.
        let resolved = resolve_zopflipng(os("/opt/pinned/zopflipng"), os("/usr/bin"), |_| true);

        assert_eq!(resolved, Some(PathBuf::from("/opt/pinned/zopflipng")));
    }

    #[test]
    fn zopflipng_prefers_the_in_tree_pinned_build_over_path() {
        // Load-bearing: the Python oracle resolves in this same order, and the
        // two must shell out to the SAME binary or parity compares two programs.
        let resolved = resolve_zopflipng(None, os("/a"), |_| true);

        assert_eq!(resolved, Some(in_tree_zopflipng()));
    }

    #[test]
    fn zopflipng_falls_back_to_path_in_order() {
        let resolved = resolve_zopflipng(
            None,
            os("/a:/b:/c"),
            no_in_tree(&["/b/zopflipng", "/c/zopflipng"]),
        );

        assert_eq!(resolved, Some(PathBuf::from("/b/zopflipng")));
    }

    #[test]
    fn zopflipng_resolution_fails_when_absent_everywhere() {
        assert_eq!(resolve_zopflipng(None, os("/a:/b"), |_| false), None);
        assert_eq!(resolve_zopflipng(None, None, no_in_tree(&[])), None);
    }

    #[test]
    fn zopflipng_empty_override_defers_to_path() {
        // An unset-but-exported `PRISM_ZOPFLIPNG=` must not resolve to "".
        let resolved = resolve_zopflipng(os(""), os("/a"), no_in_tree(&["/a/zopflipng"]));

        assert_eq!(resolved, Some(PathBuf::from("/a/zopflipng")));
    }

    #[test]
    fn zopflipng_ignores_empty_path_entries() {
        // A leading empty PATH entry means "." to the shell; honoring it would
        // let a stray ./zopflipng hijack the search.
        let resolved =
            resolve_zopflipng(None, os(":/b"), no_in_tree(&["/b/zopflipng", "/zopflipng"]));

        assert_eq!(resolved, Some(PathBuf::from("/b/zopflipng")));
    }

    /// The seam identity/8-bit/memLevel-8 trial must reproduce the plain
    /// `write_indexed_png` (stage_emit) bytes exactly, and the full seam search
    /// must never exceed that baseline (the never-worse guarantee), while
    /// re-decoding pixel-identically.
    #[test]
    fn seam_emit_is_never_larger_than_the_baseline() {
        // A 4x4 image over a 4-entry palette (one fully transparent): exercises
        // palette-sort, reduction rungs, transparent-front, and memLevel race.
        let palette: Vec<Rgba> = vec![
            (10, 20, 30, 0),    // fully transparent
            (200, 10, 10, 255), // opaque red
            (10, 200, 10, 255), // opaque green
            (10, 10, 200, 255), // opaque blue
        ];
        let indices: Vec<usize> = vec![
            0, 1, 2, 3, //
            1, 1, 2, 2, //
            3, 3, 0, 0, //
            2, 1, 0, 3,
        ];
        let baseline = {
            let idx_u8: Vec<u8> = indices.iter().map(|&i| i as u8).collect();
            crate::png::write_indexed_png(4, 4, &palette, &idx_u8).unwrap()
        };
        // identity/8/8 baseline (no seams) via seam_emit_config directly.
        let identity = seam_emit_config(4, 4, &palette, &indices, 8, 8);
        assert_eq!(
            identity, baseline,
            "seam identity/8-bit/memLevel-8 must equal write_indexed_png"
        );
        for (ps, ml, rd) in [
            (true, false, true),
            (true, true, true),
            (false, false, true),
            (true, false, false),
            (false, true, false),
        ] {
            let out = seam_emit(4, 4, &palette, &indices, ps, ml, rd).unwrap();
            assert!(
                out.len() <= baseline.len(),
                "seam ({ps},{ml},{rd}) = {} B must not exceed baseline {} B",
                out.len(),
                baseline.len()
            );
            let decoded = png::decode_png(&out).unwrap();
            let expected: Vec<Rgba> = indices.iter().map(|&i| palette[i]).collect();
            assert_eq!(decoded.pixels, expected, "seam trial must decode-identical");
        }
    }

    /// The transparent-front reduction rung trims tRNS to a single byte when
    /// exactly one entry is fully transparent — it must be selected only when
    /// it does not enlarge the stream.
    #[test]
    fn seam_transparent_front_is_applicable_and_safe() {
        let palette: Vec<Rgba> = vec![
            (0, 0, 0, 255),
            (1, 1, 1, 255),
            (9, 9, 9, 0), // sole fully-transparent entry, NOT at index 0
        ];
        let order = seam_order_transparent_front(&palette).expect("applicable");
        assert_eq!(order[0], 2, "transparent entry moves to index 0");
        // Two transparent entries -> inapplicable.
        let palette2: Vec<Rgba> = vec![(0, 0, 0, 0), (1, 1, 1, 0), (2, 2, 2, 255)];
        assert!(seam_order_transparent_front(&palette2).is_none());
        // Partial alpha (not fully transparent) -> inapplicable.
        let palette3: Vec<Rgba> = vec![(0, 0, 0, 128), (2, 2, 2, 255)];
        assert!(seam_order_transparent_front(&palette3).is_none());
    }

    #[test]
    fn bit_depth_thresholds() {
        assert_eq!(minimum_bit_depth(1).unwrap(), 1);
        assert_eq!(minimum_bit_depth(2).unwrap(), 1);
        assert_eq!(minimum_bit_depth(3).unwrap(), 2);
        assert_eq!(minimum_bit_depth(4).unwrap(), 2);
        assert_eq!(minimum_bit_depth(5).unwrap(), 4);
        assert_eq!(minimum_bit_depth(16).unwrap(), 4);
        assert_eq!(minimum_bit_depth(17).unwrap(), 8);
        assert_eq!(minimum_bit_depth(256).unwrap(), 8);
        assert!(minimum_bit_depth(0).is_err());
        assert!(minimum_bit_depth(257).is_err());
    }

    #[test]
    fn index_row_packs_msb_first() {
        assert_eq!(
            pack_index_row(&[1, 0, 1, 1, 0, 0, 0, 0], 1),
            vec![0b1011_0000]
        );
        assert_eq!(pack_index_row(&[1, 2], 4), vec![0x12]);
        assert_eq!(pack_index_row(&[3, 0, 1], 2), vec![0b11_00_01_00]);
        assert_eq!(pack_index_row(&[5, 255, 0], 8), vec![5, 255, 0]);
        // partial final byte is zero-padded.
        assert_eq!(pack_index_row(&[1], 1), vec![0b1000_0000]);
    }

    #[test]
    fn paeth_predictor() {
        assert_eq!(paeth(0, 0, 0), 0);
        assert_eq!(paeth(10, 20, 15), 15); // estimate 15; equidistant -> upper_left path check
        assert_eq!(paeth(255, 0, 0), 255);
    }

    #[test]
    fn filter_none_is_identity_first_row() {
        let row = vec![5, 9, 200, 1];
        assert_eq!(filter_row(&row, None, 0), row);
    }

    #[test]
    fn cleanup_removes_unused_and_duplicate_entries() {
        // palette entries 0 and 2 are identical RGBA; 3 is unused.
        let palette = vec![
            (1, 2, 3, 255),
            (9, 9, 9, 255),
            (1, 2, 3, 255),
            (7, 7, 7, 255),
        ];
        let indices = vec![0usize, 2, 1, 0];
        let (clean, remapped) = cleanup_palette(&palette, &indices).unwrap();
        // 0 and 2 collapse to one entry; 3 dropped -> 2 output entries.
        assert_eq!(clean, vec![(1, 2, 3, 255), (9, 9, 9, 255)]);
        assert_eq!(remapped, vec![0, 0, 1, 0]);
        // pixels preserved.
        for (&old, &new) in indices.iter().zip(remapped.iter()) {
            assert_eq!(palette[old], clean[new]);
        }
    }

    #[test]
    fn trial_zlib_matches_frozen_python_oracle() {
        // STOP-spike property pinned in-crate: the trial-zlib
        // filter choices reproduce Python zlib.compressobj(9) exactly. Frozen
        // vector generated from prism_pack._trial_compression_row_filters.
        let rows: Vec<Vec<u8>> = vec![
            vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![10, 20, 30, 40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            vec![5, 5, 5, 5, 5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 0, 0],
            vec![
                0, 1, 4, 9, 16, 25, 36, 49, 64, 81, 100, 121, 144, 169, 196, 225,
            ],
            vec![
                77, 202, 24, 37, 48, 187, 29, 109, 19, 44, 222, 214, 35, 123, 46, 217,
            ],
            vec![114, 31, 203, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![113, 23, 68, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![
                214, 73, 60, 157, 92, 52, 96, 190, 49, 32, 30, 105, 254, 218, 160, 238,
            ],
            vec![
                153, 127, 92, 124, 41, 153, 253, 175, 229, 147, 37, 60, 214, 84, 175, 77,
            ],
            vec![
                20, 39, 160, 174, 179, 254, 233, 35, 47, 138, 242, 33, 31, 158, 228, 145,
            ],
        ];
        let (scanlines, choices) = trial_compression_row_filters(&rows).unwrap();
        assert_eq!(choices, vec![0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(scanlines.len(), 187);
    }

    #[test]
    fn pack_v1_fast_produces_decodable_identity() {
        // 2x2 indexed image, small palette; fast v1 output must decode to the
        // same pixels.
        let palette = vec![(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 128)];
        let indices = vec![0usize, 1, 2, 0];
        let data = pack_indexed_png(2, 2, &palette, &indices, "fast", "v1").unwrap();
        let decoded = png::decode_png(&data).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 2);
        let expected: Vec<Rgba> = indices.iter().map(|&i| palette[i]).collect();
        assert_eq!(decoded.pixels, expected);
    }

    #[test]
    fn zlib_runtime_version_is_observable() {
        // The public evidence accessor (ops hygiene, tri-review kimi F10):
        // a caller can retrieve the exact linked zlib identity rather than
        // trusting a hardcoded assumption.
        assert!(!zlib_runtime_version().is_empty());
    }

    #[test]
    fn spawn_with_timeout_returns_status_when_process_exits_in_time() {
        let mut cmd = std::process::Command::new("/bin/echo");
        cmd.arg("hi")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let status = spawn_with_timeout(cmd, std::time::Duration::from_secs(5))
            .expect("spawn should succeed")
            .expect("process should exit before the timeout");
        assert!(status.success());
    }

    #[test]
    fn spawn_with_timeout_kills_a_process_that_outlives_the_deadline() {
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let started = std::time::Instant::now();
        let status = spawn_with_timeout(cmd, std::time::Duration::from_millis(200))
            .expect("spawn should succeed");
        // Timed out (killed): no exit status, and we did not wait anywhere
        // near the process's own 30s sleep.
        assert!(status.is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn spawn_with_timeout_returns_promptly_even_if_a_descendant_keeps_the_pipe_open() {
        // Regression (T-0135 cross-review, openai-gpt5-39): a real
        // zopflipng override can spawn a long-lived descendant that
        // inherits the piped stdout/stderr write ends. The direct child
        // (the shell) is killed+reaped at the deadline, but the
        // backgrounded grandchild survives (reparented to PID 1 on this
        // OS), keeping the pipe's write end open. A naive
        // `JoinHandle::join()` on the drain threads would block until
        // that grandchild itself exits (up to its own 30s sleep, or
        // forever for a truly long-lived process) instead of respecting
        // the configured timeout.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 30 & sleep 30")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let started = std::time::Instant::now();
        let status = spawn_with_timeout(cmd, std::time::Duration::from_secs(1))
            .expect("spawn should succeed");
        assert!(
            status.is_none(),
            "expected the 1s timeout to fire (no exit status)"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "spawn_with_timeout took {:?}, expected well under 3s despite \
             the surviving descendant holding the pipe open",
            started.elapsed()
        );
    }

    #[test]
    fn spawn_with_timeout_bounds_memory_and_time_against_a_continuous_writer() {
        // Regression (T-0135 cross-review round 2, openai-gpt5-39): the
        // round-1 fix stopped the CALLER from blocking forever, but the
        // abandoned drain thread still called `read_to_end` into a
        // private `Vec` — a descendant that writes CONTINUOUSLY (not
        // just a silent `sleep`, like round 1's repro) keeps that Vec
        // growing for as long as the thread stays alive. `yes` is the
        // canonical continuous writer AND is well known to terminate
        // cleanly on SIGPIPE the moment nobody reads its output anymore
        // (unlike a raw infinite shell loop), so this cannot leak an
        // unkillable background process even if the assertions below
        // somehow failed.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("yes continuous-writer-payload & sleep 30")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let started = std::time::Instant::now();
        let status = spawn_with_timeout(cmd, std::time::Duration::from_secs(1))
            .expect("spawn should succeed");
        assert!(
            status.is_none(),
            "expected the 1s timeout to fire (no exit status)"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "spawn_with_timeout took {:?} against a continuous writer, expected well under 3s",
            started.elapsed()
        );
        // Bounded memory is a structural property of `drain_async` (one
        // fixed `DRAIN_CHUNK_SIZE` buffer, never accumulated) rather than
        // something to sample via RSS here; the dedicated
        // `drain_async`-level test below proves the thread that would
        // otherwise keep growing such a buffer actually exits.
    }

    #[test]
    fn drain_async_exits_promptly_after_abandon_against_a_continuous_writer() {
        // Companion to the test above: proves `drain_async`'s OWN thread
        // — not just `spawn_with_timeout`'s willingness to stop waiting
        // on it — terminates (and so releases its owned fd via `Drop`)
        // once `abandon` is set, even while a descendant is actively,
        // continuously writing.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        let mut child = std::process::Command::new("yes")
            .arg("drain-async-continuous-writer-payload")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn yes");
        let stdout = child.stdout.take().expect("piped stdout");
        let abandon = Arc::new(AtomicBool::new(false));
        let (done_rx, handle) = drain_async(stdout, Arc::clone(&abandon));

        // Let it genuinely drain real, continuously-produced bytes for a
        // moment — proves this is an active writer, not an idle pipe.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            done_rx.try_recv().is_err(),
            "a continuous writer should not report EOF on its own"
        );

        abandon.store(true, Ordering::Relaxed);

        // Bounded join: run the actual `.join()` on a helper thread so a
        // regression that reintroduces an unconditional block fails this
        // test instead of hanging the test binary.
        let (join_tx, join_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = join_tx.send(());
        });
        assert!(
            join_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok(),
            "drain_async's thread did not exit within 2s of being abandoned \
             while a descendant kept writing"
        );

        // We own `child` (`yes`) directly here (no shell indirection), so
        // clean it up deterministically rather than relying on SIGPIPE.
        let _ = child.kill();
        let _ = child.wait();
    }
}

//! pngprism 0.5.0 (crate renamed from `prism-quant`, T-0213) — Rust port of
//! the reviewed Python reference quantizer (`lab/reference/prism_quant.py`
//! integrated CLI, ported from the v0.2.0-alpha pipeline pin,
//! T-0067/T-0068 phase 1 + T-0094/T-0095 phase 2, review-passed) and its
//! PNG substrate (`lab/reference/m1_png.py`).
//!
//! `quant::LABEL` retains the literal quality disclaimer "0.5.0, unproven,
//! metric-validated only". That label denies an unearned human-acceptability
//! claim; it does not mean the Rust port is still awaiting its historical
//! T-0193 parity work. This crate was developed as a seam-by-seam translation
//! of in-repo original work (T-0082/T-0095) and is gated against the Python
//! oracle (see `PORT-PLAN.md`; the parity harnesses themselves live lab-side,
//! in the `pngprism-lab` repository). There is no API-stability
//! or public-release claim.
//! Method provenance is inherited from the oracle's ledger
//! (`lab/reference/REFERENCES.md`); the clean-room quarantine binds (no
//! libimagequant source, no `book/12-*`). Current engineering status is
//! machine-derived in `benchmarks/release-eng-v05/current-gate-summary.json`;
//! older T-0212/T-0213 records must not be projected onto a repaired head.
//!
//! Module map (oracle -> crate):
//! - `m1_png.py` -> [`png`] (decode any spec-valid PNG to canonical
//!   RGBA8; deterministic indexed-PNG writer).
//! - `prism_quant.py` (pipeline) -> [`quant`] (six seams: decode ->
//!   sample -> palette init -> refinement -> remap -> emit; plus the
//!   integrated v0.2 dither/pack parameters and `quantize_candidate`).
//! - `prism_dither.py` -> [`dither`] (alpha-boundary-safe Floyd-Steinberg,
//!   the no-dither baseline, exact-rational strength parsing, and the
//!   E-0010 adaptive/region policies).
//! - `prism_pack.py` -> [`pack`] (deterministic lossless indexed-PNG
//!   packing search: v1 portfolio, the bounded v2 search, and the
//!   `zprobe` FFI shim for the trial-zlib row-filter heuristic).
//! - `prism_quant.py` (CLI) -> `src/main.rs` (`pngprism` binary).
//!
//! Tests cover in-module seams plus CLI semantics, atomic never-worse
//! publication, absolute resource caps, edge/adversarial/fuzz regressions,
//! PNG corpora, and quantizer binding. Counts are intentionally not copied
//! here; the gate summary derives inventory and the release runner determines
//! pass/fail. Speed evidence is historical-pin scoped under
//! `benchmarks/perf-v05/`; `benches/stage_boundaries.rs` is dev-only.

pub mod dither;
mod error;
pub mod pack;
pub mod parallel;
pub mod png;
pub mod quant;
// Reachable from integration tests (`tests/oracle_pins.rs` pins the vendored
// Python oracle by digest) but deliberately NOT advertised: this is a minimal
// internal SHA-256, not an API we intend to support. Widening access is the
// parity rule's prescribed move — the alternative was a second copy of the
// implementation living in the test tree.
#[doc(hidden)]
pub mod sha256;

/// One canonical RGBA8 pixel, mirroring the oracle's `(r, g, b, a)` tuple.
pub type Rgba = (u8, u8, u8, u8);

pub use error::{Error, Kind};
pub use parallel::{MAX_THREADS, MergeOrder, Parallelism};

pub use quant::{
    ADAPTIVE_DEFAULT_POLICIES, AdaptiveDefault, COLOR_SPACES, DEFAULT_ADAPTIVE_DEFAULT,
    DEFAULT_ADAPTIVE_DEFAULT_POLICY, DEFAULT_COLOR_SPACE, DEFAULT_COLORS, DEFAULT_DITHER,
    DEFAULT_DITHER_POLICY, DEFAULT_DITHER_STRENGTH, DEFAULT_HIDDEN_RGB_POLICY, DEFAULT_PACK_MODE,
    DEFAULT_PACK_SEAM_MEMLEVEL, DEFAULT_PACK_SEAM_PALETTE_SORT, DEFAULT_PACK_SEAM_REDUCTION,
    DEFAULT_PACK_SEARCH, DITHER_POLICIES, HIDDEN_RGB_POLICIES, LABEL, MAX_COLORS, PACK_MODES,
    PACK_SEARCHES, StageNotes, Summary, VERSION, premultiplied_distance_sq, quantize_candidate,
    quantize_candidate_with_color_space, quantize_candidate_with_parallelism, quantize_image,
    quantize_image_with_color_space, quantize_png, quantize_png_bytes_with_parallelism,
    quantize_png_with_adaptive_default, quantize_png_with_color_space,
    quantize_png_with_parallelism,
};

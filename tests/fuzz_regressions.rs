//! T-0207 crash-regression gate: replay every input the fuzzer ever flagged
//! (`fuzz/crash-regressions/`) against BOTH fuzz-target code paths — the bare
//! decoder and the full decode -> quantize -> emit -> re-decode pipeline — on
//! the pinned RELEASE toolchain, so a fixed crash can never silently regress.
//!
//! This mirrors the two `fuzz/fuzz_targets/*.rs` entry points exactly, minus
//! libFuzzer, so `cargo test` alone (no nightly) is a sufficient guard. The
//! directory starts EMPTY — that is the correct state when the fuzz window
//! reproduced no crash. Each finding is minimised (`cargo fuzz tmin`) and
//! dropped in as one file; from then on it is replayed here forever.
//!
//! Contract asserted per replayed input: neither path panics (a panic is the
//! §31 violation these regressions exist to pin). A returned `Err`/early
//! return is a correct outcome — the input was malformed by construction.

use pngprism::png;
use pngprism::quant;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

const MAX_PIXELS: usize = 1 << 16; // mirror quantize_pipeline.rs guard.

fn crash_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz/crash-regressions")
}

/// Every committed crash-regression input (skips the README/.gitkeep markers).
fn crash_inputs() -> Vec<PathBuf> {
    let dir = crash_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n != ".gitkeep" && n != "README.md"
        })
        .collect();
    paths.sort();
    paths
}

/// The `decode_png` fuzz target's body.
fn replay_decode_target(data: &[u8]) {
    let _ = png::decode_png(data);
}

/// The `quantize_pipeline` fuzz target's body (kept in lockstep with
/// `fuzz/fuzz_targets/quantize_pipeline.rs`).
fn replay_pipeline_target(data: &[u8]) {
    let Ok(image) = png::decode_png(data) else {
        return;
    };
    if image.pixels.len() > MAX_PIXELS {
        return;
    }
    let seed = data.first().copied().unwrap_or(0);
    let colors = i64::from(seed) % quant::MAX_COLORS + 1;
    let Ok((palette, indices, _notes)) =
        quant::quantize_candidate(&image, colors, quant::DEFAULT_HIDDEN_RGB_POLICY)
    else {
        return;
    };
    if let Ok(encoded) = png::write_indexed_png(image.width, image.height, &palette, &indices) {
        let _ = png::decode_png(&encoded);
    }
}

#[test]
fn crash_regressions_never_panic_on_either_target() {
    let inputs = crash_inputs();
    let mut failures = Vec::new();
    for path in &inputs {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        for (target, body) in [
            ("decode_png", replay_decode_target as fn(&[u8])),
            ("quantize_pipeline", replay_pipeline_target as fn(&[u8])),
        ] {
            if panic::catch_unwind(AssertUnwindSafe(|| body(&bytes))).is_err() {
                failures.push(format!("{name}: PANIC on target {target}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "crash-regression panics ({} input(s) replayed):\n{}",
        inputs.len(),
        failures.join("\n")
    );
    // Visible with `--nocapture`; empty is the correct state with no findings.
    eprintln!(
        "crash-regressions: replayed {} committed input(s) against 2 targets, no panics",
        inputs.len()
    );
}

/// Guards that the regression directory itself is present (a deleted dir would
/// make the replay vacuously pass).
#[test]
fn crash_regression_dir_exists() {
    assert!(
        crash_dir().is_dir(),
        "crash-regressions dir missing: {}",
        crash_dir().display()
    );
}

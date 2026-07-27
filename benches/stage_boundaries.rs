//! T-0111 stage-boundary micro-benchmarks for `prism-quant` (Rust crate).
//!
//! Criterion is a dev-dependency only (never ships; ch17 §30's dev-dep
//! clause, ledgered in `REFERENCES.md`). These benches exist so future
//! Thread-4 optimization tasks can cite a stage-level delta ("quantize got
//! N% faster") instead of only a whole-pipeline number — the whole-pipeline
//! wall-time/RSS baseline lives in `benchmarks/speed-tracking/`, not here.
//!
//! Scope: quantize (`quantize_candidate`), dither (`floyd_steinberg`), and
//! pack (`pack_indexed_png`, "fast" mode). Pack's "max" mode shells out to
//! the external `zopflipng` binary per invocation and is deliberately
//! EXCLUDED from criterion sampling (repeated subprocess spawns would
//! dominate the measurement and don't reflect in-process cost); the
//! full-pipeline harness still exercises `--pack max` end-to-end.
//!
//! Fixture: in the research tree, the prism benchmark dice PNG (the program's
//! standing "accountability image",
//! `datasets/collections/prism-benchmark-image/`), decoded once per benchmark
//! group (decode itself is not a stage under test here — it is covered by the
//! whole-pipeline harness and by this crate's own parity/unit tests).
//! That image is CC BY-SA 3.0 and is NOT shipped with the crate, so outside the
//! tree these benches need `PNGPRISM_BENCH_IMAGE` and otherwise skip — see
//! `fixture_path`.
//!
//! Run: `cargo bench` from the crate root.

use criterion::{Criterion, criterion_group, criterion_main};
use pngprism::dither::{floyd_steinberg, stub_directives};
use pngprism::pack::pack_indexed_png;
use pngprism::png::{DecodedImage, decode_png};
use pngprism::quant::quantize_candidate;
use std::hint::black_box;
use std::path::PathBuf;

const COLORS: i64 = 256;
const HIDDEN_RGB_POLICY: &str = "canonicalize-black";

/// The bench corpus image, resolved in order:
///
/// 1. **`PNGPRISM_BENCH_IMAGE`** — an explicit path to any PNG.
/// 2. The **Prism research tree's** copy, when this crate is checked out inside
///    it (`benches/` -> `lib/prism-quant/` -> `lib/` -> `project-prism/`),
///    existence-checked so it simply does not apply elsewhere.
///
/// `None` when neither resolves — the benches then SKIP with a message rather
/// than fail, so `cargo bench` stays green for a consumer who has only the
/// crate.
///
/// The program's accountability image (the Wikimedia dice render) is
/// deliberately **not vendored** into this crate: it is **CC BY-SA 3.0**, and
/// share-alike attribution obligations on a redistributed asset do not belong
/// in an MIT-or-Apache crate (ADR-0033 §2). It stays lab-side; anyone else
/// points `PNGPRISM_BENCH_IMAGE` at a PNG of their choosing. Note that timings
/// are only comparable across runs that used the SAME image.
///
/// Returns the path AND the label that goes into every benchmark id. The label
/// tracks the actual fixture: `dice` only for the in-tree accountability image
/// (keeping T-0111's historical ids comparable), otherwise the supplied file's
/// stem. A benchmark named `.../dice/...` measured on some other PNG would be a
/// claim detached from its evidence (ADR-0025), and criterion would silently
/// compare the two as if they were one series.
fn fixture_path() -> Option<(PathBuf, String)> {
    if let Some(explicit) = std::env::var_os("PNGPRISM_BENCH_IMAGE")
        && !explicit.is_empty()
    {
        let path = PathBuf::from(explicit);
        let label = path.file_stem().map_or_else(
            || "custom".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        return Some((path, label));
    }
    // The lab checkout, resolved the same way the test suites resolve it:
    // PRISM_LAB_DIR if set, else the conventional sibling, confirmed by its
    // .prism-root marker.
    //
    // This used to be `CARGO_MANIFEST_DIR/../../datasets/...`, correct while the
    // crate sat at `research/project-prism/lib/prism-quant` inside the monorepo.
    // After the split it resolves outside any checkout and can never exist, so
    // `cargo bench` skipped every benchmark while still exiting zero — and these
    // benchmarks exist specifically so optimization work can cite a stage-level
    // delta. A performance harness that silently measures nothing is worse than
    // none, because it looks like evidence.
    let lab = std::env::var_os("PRISM_LAB_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(|p| p.join("pngprism-lab"))
        },
        |dir| Some(PathBuf::from(dir)),
    )?;
    if !lab.join(".prism-root").is_file() {
        return None;
    }
    let in_tree = lab.join(
        "datasets/collections/prism-benchmark-image/files/PNG_transparency_demonstration_1.png",
    );
    in_tree.is_file().then(|| (in_tree, "dice".to_string()))
}

/// Read + decode the fixture, or `None` (having explained the skip once) when
/// no image is available.
fn load_fixture() -> Option<(DecodedImage, String)> {
    static EXPLAINED: std::sync::Once = std::sync::Once::new();
    let Some((path, label)) = fixture_path() else {
        // Strict mode turns a skip into a failure, for automation that is
        // SUPPOSED to have a fixture. This benchmark silently measured nothing
        // from the repository split until 2026-07-27 while still exiting zero,
        // which is the failure mode a performance harness can least afford: it
        // looks like evidence. Same switch the test suites honour.
        assert!(
            std::env::var_os("PRISM_REQUIRE_LAB").is_none(),
            "PRISM_REQUIRE_LAB is set but no bench fixture was found — set \
             PNGPRISM_BENCH_IMAGE or PRISM_LAB_DIR, or clone pngprism-lab \
             beside this crate. Refusing to report a benchmark that measured \
             nothing."
        );
        EXPLAINED.call_once(|| {
            eprintln!(
                "stage_boundaries: SKIPPED — no bench image. Set PNGPRISM_BENCH_IMAGE \
                 to a PNG to run these benchmarks. (The program's own accountability \
                 image is CC BY-SA 3.0 and is not shipped with this crate.)"
            );
        });
        return None;
    };
    let raw =
        std::fs::read(&path).unwrap_or_else(|e| panic!("read bench image {}: {e}", path.display()));
    Some((decode_png(&raw).expect("decode bench image"), label))
}

fn bench_quantize(c: &mut Criterion) {
    let Some((decoded, label)) = load_fixture() else {
        return;
    };

    c.bench_function(&format!("quantize_candidate/{label}/colors=256"), |b| {
        b.iter(|| {
            quantize_candidate(
                black_box(&decoded),
                black_box(COLORS),
                black_box(HIDDEN_RGB_POLICY),
            )
            .expect("quantize_candidate")
        })
    });
}

fn bench_dither(c: &mut Criterion) {
    let Some((decoded, label)) = load_fixture() else {
        return;
    };
    let (palette, _indices, _notes) = quantize_candidate(&decoded, COLORS, HIDDEN_RGB_POLICY)
        .expect("quantize_candidate (setup for dither bench)");
    let directives = stub_directives(decoded.pixels.len());

    c.bench_function(&format!("floyd_steinberg/{label}/colors=256"), |b| {
        b.iter(|| {
            floyd_steinberg(
                black_box(&decoded.pixels),
                black_box(decoded.width as usize),
                black_box(decoded.height as usize),
                black_box(&palette),
                black_box(&directives),
            )
            .expect("floyd_steinberg")
        })
    });
}

fn bench_pack(c: &mut Criterion) {
    let Some((decoded, label)) = load_fixture() else {
        return;
    };
    let (palette, indices, _notes) = quantize_candidate(&decoded, COLORS, HIDDEN_RGB_POLICY)
        .expect("quantize_candidate (setup for pack bench)");
    let indices_usize: Vec<usize> = indices.iter().map(|&i| i as usize).collect();

    let mut group = c.benchmark_group(format!("pack_indexed_png/{label}/colors=256"));
    // Measured on this fixture: fast/v1 ~2.9s/iter, fast/v2 ~15.5s/iter (T-0111
    // evidence log) — criterion's 100-sample default would need ~1550s for v2
    // alone. Drop to the criterion-minimum sample_size (10) and raise
    // measurement_time so the estimate doesn't undershoot and warn/retry;
    // still statistically meaningful (10 samples), and `cargo bench` finishes
    // in low single-digit minutes instead of tens of minutes.
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(200));
    for search_version in ["v1", "v2"] {
        group.bench_function(format!("fast/{search_version}"), |b| {
            b.iter(|| {
                pack_indexed_png(
                    black_box(decoded.width as usize),
                    black_box(decoded.height as usize),
                    black_box(&palette),
                    black_box(&indices_usize),
                    black_box("fast"),
                    black_box(search_version),
                )
                .expect("pack_indexed_png (fast)")
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_quantize, bench_dither, bench_pack);
criterion_main!(benches);

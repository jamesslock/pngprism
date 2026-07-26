//! T-0207 caps-as-tests: the decoder's resource caps, enforced on the pinned
//! RELEASE toolchain (no nightly, no libFuzzer) so they gate every `cargo
//! test` run — complementing the fuzz targets, which merely *search* for a
//! cap violation, with committed *proofs* that the caps hold.
//!
//! Five caps are asserted:
//!
//!   * COMPRESSED INPUT / DIMENSION / PIXEL / DECODED-SCANLINE caps — explicit
//!     absolute ceilings reject before an unbounded path read, inflation, or
//!     canonical-pixel allocation. The IHDR tests use independently built,
//!     spec-valid streams whose IDAT expands to exactly the declared geometry;
//!     they are not malformed short-stream shortcuts. The input-size fixture
//!     is a valid sparse PNG, so the test crosses 256 MiB without allocating or
//!     physically writing a 256 MiB buffer.
//!
//!   * TIME cap (bounded work) — every one of the 623 structure-aware fuzz
//!     seeds must resolve (to `Ok` or a typed `Err`) within a generous
//!     per-input wall-clock bound and without panicking. A decoder with an
//!     unbounded loop or a hang would blow the bound; this replays the whole
//!     committed seed corpus as a no-hang / no-panic gate on the release code.
//!
//!   * AMPLIFICATION cap (T-0212, fixes the T-0207 finding below) —
//!     `inflate` (`src/png.rs`) no longer materialises the full IDAT output
//!     before its length check: once cumulative decoded bytes reach the
//!     IHDR-declared `expected` total, any further output byte is
//!     conclusive proof the stream is oversized and is rejected immediately
//!     (a typed `data_error`, still never a panic/OOB — the §31 contract
//!     holds). Asserted here as a tight wall-clock bound on the
//!     `tests/amplification/` reproducer (an IHDR declaring 200 expected
//!     scanline bytes whose IDAT decompresses to 64 MiB) — pre-fix this
//!     input took long enough to fully materialize ~67 MB of scratch that a
//!     tight ceiling would have caught it; post-fix it resolves in
//!     microseconds. Peak-RSS before/after numbers (not something a `cargo
//!     test` process can assert on itself) are measured externally via
//!     `/usr/bin/time -l` and recorded in `tests/amplification/README.md`
//!     and the T-0212 task file's Evidence log.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use pngprism::Kind;
use pngprism::png::{
    MAX_DECODED_SCANLINE_BYTES, MAX_DIMENSION, MAX_INPUT_BYTES, MAX_PIXELS, decode_png,
    read_png_file,
};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn edge_corpus() -> PathBuf {
    crate_dir().join("tests/edge/corpus")
}

fn fuzz_seed_dir() -> PathBuf {
    crate_dir().join("fuzz/corpus/decode_png")
}

fn amplification_corpus() -> PathBuf {
    crate_dir().join("tests/amplification/corpus")
}

/// Decode `bytes` inside a panic guard, returning the elapsed time and whether
/// it panicked (a panic is always a cap/robustness violation).
fn timed_decode(bytes: &[u8]) -> (Duration, bool, bool) {
    let start = Instant::now();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| decode_png(bytes)));
    let elapsed = start.elapsed();
    match outcome {
        Ok(Ok(_)) => (elapsed, false, true),   // decoded cleanly
        Ok(Err(_)) => (elapsed, false, false), // typed error
        Err(_) => (elapsed, true, false),      // PANIC
    }
}

/// A generous ceiling for a *single* decode of a small fuzz seed. Real decodes
/// finish in microseconds; this only converts a hang/unbounded loop into a
/// reported failure. Kept well above any plausible scheduling jitter on a
/// shared CI box.
const PER_INPUT_CEILING: Duration = Duration::from_secs(5);

/// The giant-dims cap must fire essentially instantly — it rejects before any
/// allocation/inflation. A hard multi-hundred-ms bound is still orders of
/// magnitude above the real cost (~microseconds) yet far below what allocating
/// or inflating the claimed ~2^62 bytes could ever take.
const GIANT_DIMS_CEILING: Duration = Duration::from_millis(500);

/// The amplification cap (T-0212) must also fire essentially instantly: the
/// fix rejects on the first output byte beyond the IHDR-declared total,
/// which needs at most a couple of small `decompress` calls. Generous
/// relative to the true cost (microseconds, per `tests/amplification/
/// README.md`) but far below what fully materializing 64 MiB of decoded
/// output (the pre-fix behavior) would take.
const AMPLIFICATION_CEILING: Duration = Duration::from_millis(500);

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

fn append_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc_input = Vec::with_capacity(4 + payload.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Build a non-interlaced, filter-0 PNG whose zlib stream expands to exactly
/// the IHDR-declared scanline count. Compression is fed from one 64 KiB block,
/// so even the 256 MiB decoded-limit probe has only O(compressed-size) output
/// and O(64 KiB) source storage.
fn valid_compressible_png(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
    let channels = match color_type {
        0 | 3 => 1u64,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => panic!("unsupported test color type"),
    };
    let row_bytes = (u64::from(width) * channels * u64::from(bit_depth)).div_ceil(8);
    let decoded_bytes = u64::from(height) * (1 + row_bytes);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    let zeroes = [0u8; 64 * 1024];
    let mut remaining = decoded_bytes;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(zeroes.len() as u64)).unwrap();
        encoder.write_all(&zeroes[..count]).unwrap();
        remaining -= count as u64;
    }
    let idat = encoder.finish().unwrap();

    let mut png = Vec::with_capacity(idat.len() + 57);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &ihdr);
    append_chunk(&mut png, b"IDAT", &idat);
    append_chunk(&mut png, b"IEND", &[]);
    png
}

/// Create a structurally valid PNG one large ancillary chunk beyond the
/// compressed-input ceiling. The payload is an APFS sparse hole. Its CRC is a
/// precomputed pin for `b"vpAg" + (256 MiB of zeroes)`; the assertion binds
/// that pin to the production limit if policy ever changes.
fn write_sparse_oversize_png(path: &Path) {
    assert_eq!(MAX_INPUT_BYTES, 256 * 1024 * 1024);
    let mut file = File::create(path).unwrap();
    file.write_all(b"\x89PNG\r\n\x1a\n").unwrap();

    let mut ihdr_chunk = Vec::new();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    append_chunk(&mut ihdr_chunk, b"IHDR", &ihdr);
    file.write_all(&ihdr_chunk).unwrap();

    file.write_all(&(MAX_INPUT_BYTES as u32).to_be_bytes())
        .unwrap();
    file.write_all(b"vpAg").unwrap();
    file.seek(SeekFrom::Current(MAX_INPUT_BYTES as i64))
        .unwrap();
    file.write_all(&0xedc6_24feu32.to_be_bytes()).unwrap();

    let tail = valid_compressible_png(1, 1, 8, 0);
    // Skip the duplicate signature + IHDR and retain this builder's valid
    // IDAT/IEND tail.
    file.write_all(&tail[33..]).unwrap();
    file.flush().unwrap();
    assert!(file.metadata().unwrap().len() > MAX_INPUT_BYTES as u64);
}

#[test]
fn absolute_resource_ceilings_reject_valid_exact_streams_before_heavy_work() {
    let cases = [
        (
            "dimension",
            valid_compressible_png(MAX_DIMENSION + 1, 1, 1, 0),
            format!(
                "resource limit exceeded: image dimensions {}x1 exceed per-dimension maximum {MAX_DIMENSION}",
                MAX_DIMENSION + 1
            ),
        ),
        (
            // 8192*8193 = 67,117,056 > 64 Mi default (67,108,864). Low bit
            // depth so the scanline ceiling is far away — this rejects on
            // PIXELS. Both dims <= 32,768, so the dimension ceiling is not hit.
            "pixels",
            valid_compressible_png(8192, 8193, 1, 0),
            format!(
                "resource limit exceeded: image has {} pixels; maximum is {MAX_PIXELS}",
                8192u64 * 8193
            ),
        ),
        (
            // 8192*8192 = 67,108,864 == the 64 Mi pixel ceiling exactly (so it
            // PASSES the strict `>` pixel check), but as 16-bit RGBA its filtered
            // scanlines are 8192*(1 + 8192*8) = 536,879,104 > 512 MiB derived
            // ceiling — it rejects on DECODED-SCANLINES, the same filter-byte
            // margin the 32 Mi/256 MiB pairing relied on, re-derived at 64 Mi.
            "decoded-scanlines",
            valid_compressible_png(8192, 8192, 16, 6),
            format!(
                "resource limit exceeded: decoded scanlines require {} bytes; maximum is {MAX_DECODED_SCANLINE_BYTES}",
                8192u128 * (1 + 8192u128 * 8)
            ),
        ),
    ];

    let temp = std::env::temp_dir().join(format!("pngprism-caps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir(&temp).unwrap();
    let python_cli = crate_dir().join("tests/oracle/prism_quant.py");

    for (label, bytes, expected) in cases {
        assert!(
            bytes.len() < 2 * 1024 * 1024,
            "{label}: fixture must stay highly compressed, got {} bytes",
            bytes.len()
        );
        let start = Instant::now();
        let error = decode_png(&bytes).expect_err("over-limit fixture must reject");
        assert_eq!(error.kind(), Kind::Data);
        assert_eq!(error.message(), expected);
        assert!(
            start.elapsed() < GIANT_DIMS_CEILING,
            "{label}: preflight did not reject promptly"
        );

        let input = temp.join(format!("{label}.png"));
        let rust_out = temp.join(format!("{label}-rust.png"));
        let python_out = temp.join(format!("{label}-python.png"));
        std::fs::write(&input, &bytes).unwrap();
        let rust = Command::new(env!("CARGO_BIN_EXE_pngprism"))
            .args([input.as_os_str(), rust_out.as_os_str()])
            .output()
            .unwrap();
        let python = Command::new("python3")
            .args([
                python_cli.as_os_str(),
                input.as_os_str(),
                python_out.as_os_str(),
            ])
            .output()
            .unwrap();
        assert_eq!(rust.status.code(), Some(3), "{label}: {rust:?}");
        assert_eq!(python.status.code(), Some(3), "{label}: {python:?}");
        assert_eq!(rust.stderr, python.stderr, "{label}: diagnostic parity");
        assert!(!rust_out.exists());
        assert!(!python_out.exists());
    }
    std::fs::remove_dir_all(&temp).unwrap();
}

/// Run both CLIs (Rust binary + Python oracle) on the same input and args,
/// returning their process outputs for parity assertions.
fn run_both_clis(
    input: &Path,
    rust_out: &Path,
    python_out: &Path,
    extra: &[&str],
) -> (std::process::Output, std::process::Output) {
    let python_cli = crate_dir().join("tests/oracle/prism_quant.py");
    let mut rust_args: Vec<&std::ffi::OsStr> = vec![input.as_os_str(), rust_out.as_os_str()];
    rust_args.extend(extra.iter().map(std::ffi::OsStr::new));
    let rust = Command::new(env!("CARGO_BIN_EXE_pngprism"))
        .args(&rust_args)
        .output()
        .unwrap();
    let mut py_args: Vec<&std::ffi::OsStr> = vec![
        python_cli.as_os_str(),
        input.as_os_str(),
        python_out.as_os_str(),
    ];
    py_args.extend(extra.iter().map(std::ffi::OsStr::new));
    let python = Command::new("python3").args(&py_args).output().unwrap();
    (rust, python)
}

/// The `--max-pixels` knob is a HARD admission bound, honored identically by
/// both implementations (parity rule), overriding the default ceiling up OR
/// down. Tested with a SMALL image by moving the ceiling around it — never by
/// encoding a real multi-gigabyte image (the default-reject/flag-admit of a
/// genuinely >64 Mi image is proven at the header-validate level in
/// `src/png.rs` / `test_m1_png.py`, without allocating it).
#[test]
fn max_pixels_flag_admission_knob_is_a_hard_bound_both_impls() {
    let temp = std::env::temp_dir().join(format!("pngprism-maxpx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir(&temp).unwrap();

    // A small, valid 100-pixel (10x10) RGB image the DEFAULT ceiling admits.
    let bytes = valid_compressible_png(10, 10, 8, 2);
    let input = temp.join("small.png");
    std::fs::write(&input, &bytes).unwrap();

    // DOWN: ceiling below the pixel count -> both reject (exit 3, data error),
    // byte-identical diagnostic, no output written. This is an image the
    // default would ACCEPT, forced to reject by lowering the ceiling.
    let ro = temp.join("down-rust.png");
    let po = temp.join("down-python.png");
    let (rust, python) = run_both_clis(&input, &ro, &po, &["--max-pixels", "99"]);
    assert_eq!(rust.status.code(), Some(3), "down: rust {rust:?}");
    assert_eq!(python.status.code(), Some(3), "down: python {python:?}");
    assert_eq!(
        String::from_utf8_lossy(&rust.stderr),
        format!(
            "data_error: cannot decode {}: resource limit exceeded: image has 100 pixels; maximum is 99\n",
            input.display()
        )
    );
    assert_eq!(rust.stderr, python.stderr, "down: diagnostic parity");
    assert!(!ro.exists() && !po.exists());

    // AT: ceiling exactly equal to the pixel count -> both admit (strict `>`).
    let ro = temp.join("at-rust.png");
    let po = temp.join("at-python.png");
    let (rust, python) = run_both_clis(&input, &ro, &po, &["--max-pixels", "100"]);
    assert_eq!(rust.status.code(), Some(0), "at: rust {rust:?}");
    assert_eq!(python.status.code(), Some(0), "at: python {python:?}");

    // UP: ceiling far above the pixel count -> both admit; the flag is honored
    // (and does not disturb acceptance) in the raise direction too.
    let ro = temp.join("up-rust.png");
    let po = temp.join("up-python.png");
    let (rust, python) = run_both_clis(&input, &ro, &po, &["--max-pixels", "1000000"]);
    assert_eq!(rust.status.code(), Some(0), "up: rust {rust:?}");
    assert_eq!(python.status.code(), Some(0), "up: python {python:?}");

    std::fs::remove_dir_all(&temp).unwrap();
}

/// Invalid `--max-pixels` values are rejected identically by both impls:
/// non-numeric, zero, and negative are all usage errors (exit 2), with a
/// byte-identical one-line diagnostic and empty stdout.
#[test]
fn max_pixels_flag_invalid_values_reject_identically_both_impls() {
    let temp = std::env::temp_dir().join(format!("pngprism-maxpx-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir(&temp).unwrap();
    let input = temp.join("small.png");
    std::fs::write(&input, valid_compressible_png(4, 4, 8, 2)).unwrap();

    let cases: [(&[&str], &str); 4] = [
        (
            &["--max-pixels", "0"],
            "usage_error: --max-pixels must be a positive integer\n",
        ),
        (
            &["--max-pixels", "-1"],
            "usage_error: --max-pixels must be a positive integer\n",
        ),
        (
            &["--max-pixels", "abc"],
            "usage_error: --max-pixels must be an integer\n",
        ),
        (
            &["--max-pixels"],
            "usage_error: --max-pixels needs a value\n",
        ),
    ];
    for (extra, expected) in cases {
        let ro = temp.join("bad-rust.png");
        let po = temp.join("bad-python.png");
        let (rust, python) = run_both_clis(&input, &ro, &po, extra);
        assert_eq!(rust.status.code(), Some(2), "{extra:?}: rust {rust:?}");
        assert_eq!(
            python.status.code(),
            Some(2),
            "{extra:?}: python {python:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&rust.stderr),
            expected,
            "{extra:?}: rust text"
        );
        assert_eq!(rust.stderr, python.stderr, "{extra:?}: diagnostic parity");
        assert!(rust.stdout.is_empty() && python.stdout.is_empty());
        assert!(!ro.exists() && !po.exists());
    }
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
fn compressed_input_cap_rejects_valid_sparse_png_before_payload_read() {
    let temp = std::env::temp_dir().join(format!(
        "pngprism-input-cap-{}-{}.png",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_file(&temp);
    write_sparse_oversize_png(&temp);

    let start = Instant::now();
    let error = read_png_file(&temp).expect_err("oversize file must reject");
    assert_eq!(error.kind(), Kind::Data);
    assert_eq!(
        error.message(),
        format!(
            "data_error: cannot decode {}: resource limit exceeded: compressed PNG input exceeds {MAX_INPUT_BYTES} bytes",
            temp.display()
        )
    );
    assert!(
        start.elapsed() < GIANT_DIMS_CEILING,
        "metadata preflight should not read the sparse payload"
    );
    std::fs::remove_file(&temp).unwrap();
}

#[test]
fn allocation_cap_rejects_absurd_ihdr_dims_before_allocating() {
    // The two hand-built T-0201 giant-dims edge fixtures.
    let fixtures = ["bad-absurd-dims-both.png", "bad-absurd-dims-width.png"];
    let mut failures = Vec::new();
    for name in fixtures {
        let path = edge_corpus().join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        // Repeat a few times so the assertion isn't a single-sample fluke.
        for iter in 0..8 {
            let (elapsed, panicked, ok) = timed_decode(&bytes);
            if panicked {
                failures.push(format!("{name}: PANIC on iter {iter}"));
            }
            if ok {
                failures.push(format!("{name}: decoded cleanly (must reject absurd dims)"));
            }
            if elapsed > GIANT_DIMS_CEILING {
                failures.push(format!(
                    "{name}: took {elapsed:?} (> {GIANT_DIMS_CEILING:?}) on iter {iter} — \
                     cap did not fire before allocation/inflation"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn allocation_cap_holds_under_mutation_across_all_giant_dims_seeds() {
    // Every giant-dims mutation the seed generator derived from a real source
    // fixture (mut-giant-dims-*). Each must reject fast without allocating.
    let dir = fuzz_seed_dir();
    let mut seeds: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("mut-giant-dims-"))
        })
        .collect();
    seeds.sort();
    assert!(
        seeds.len() >= 40,
        "expected the generator to derive many giant-dims mutations, found {}",
        seeds.len()
    );

    let mut failures = Vec::new();
    for path in &seeds {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        let (elapsed, panicked, _ok) = timed_decode(&bytes);
        // NB: a giant-dims mutation applied to an already-malformed seed may
        // reject for a different reason first; we only require fast + no-panic.
        if panicked {
            failures.push(format!("{name}: PANIC"));
        }
        if elapsed > GIANT_DIMS_CEILING {
            failures.push(format!(
                "{name}: took {elapsed:?} (> {GIANT_DIMS_CEILING:?}) — allocation cap may not \
                 hold under this mutation"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn amplification_cap_rejects_oversized_deflate_stream_fast() {
    // T-0212: the T-0207 finding. An 8x8 RGB8 IHDR declares 200 expected
    // scanline bytes; the IDAT decompresses to 64 MiB. Pre-fix this took
    // long enough (materializing ~67 MB across repeated SCRATCH_CAP-sized
    // scratch grows) that this ceiling would have failed; post-fix it must
    // reject on the very first output byte beyond 200.
    let path = amplification_corpus().join("bomb-8x8-rgb8-64mib-zeros.png");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read reproducer: {e}"));
    let mut failures = Vec::new();
    for iter in 0..8 {
        let (elapsed, panicked, ok) = timed_decode(&bytes);
        if panicked {
            failures.push(format!("PANIC on iter {iter}"));
        }
        if ok {
            failures.push(format!(
                "iter {iter}: decoded cleanly (must reject an oversized deflate stream)"
            ));
        }
        if elapsed > AMPLIFICATION_CEILING {
            failures.push(format!(
                "iter {iter}: took {elapsed:?} (> {AMPLIFICATION_CEILING:?}) — amplification \
                 cap did not fire before materializing the oversized stream"
            ));
        }
    }
    // Also pin that the failure is the NEW bounded-overflow error, not the
    // old post-hoc "decoded <huge> scanline bytes, expected 200" message
    // (which would only appear after full materialization).
    match decode_png(&bytes) {
        Err(err) => assert_eq!(
            err.message(),
            "decoded more than 200 scanline bytes (deflate stream exceeds IHDR-declared size)"
        ),
        Ok(_) => panic!("expected a data_error, got a clean decode"),
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn time_cap_whole_seed_corpus_decodes_without_hang_or_panic() {
    let dir = fuzz_seed_dir();
    let mut seeds: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_none() || p.is_file())
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != Some(".gitkeep"))
        .collect();
    seeds.sort();
    assert!(
        seeds.len() >= 500,
        "expected the full structure-aware seed corpus (>=500), found {}",
        seeds.len()
    );

    let mut failures = Vec::new();
    let mut worst = Duration::ZERO;
    let mut worst_name = String::new();
    for path in &seeds {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        let (elapsed, panicked, _ok) = timed_decode(&bytes);
        if panicked {
            failures.push(format!("{name}: PANIC"));
        }
        if elapsed > PER_INPUT_CEILING {
            failures.push(format!(
                "{name}: took {elapsed:?} (> {PER_INPUT_CEILING:?}) — possible hang/unbounded loop"
            ));
        }
        if elapsed > worst {
            worst = elapsed;
            worst_name = name;
        }
    }
    // Surface the slowest seed for the record (visible with `--nocapture`).
    eprintln!(
        "time-cap: {} seeds, slowest {worst:?} ({worst_name})",
        seeds.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Sanity that the fixtures this suite depends on actually exist (a moved
/// corpus would otherwise make the caps tests vacuously pass).
#[test]
fn cap_fixtures_are_present() {
    for name in ["bad-absurd-dims-both.png", "bad-absurd-dims-width.png"] {
        assert!(
            edge_corpus().join(name).is_file(),
            "missing giant-dims fixture {name}"
        );
    }
    assert!(
        Path::new(&fuzz_seed_dir()).is_dir(),
        "missing fuzz seed corpus dir"
    );
    assert!(
        amplification_corpus()
            .join("bomb-8x8-rgb8-64mib-zeros.png")
            .is_file(),
        "missing T-0212 amplification reproducer fixture"
    );
}

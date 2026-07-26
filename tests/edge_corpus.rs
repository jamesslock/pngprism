//! T-0201 release-hardening robustness gate: drive the compiled
//! `pngprism` binary end to end over the committed edge-case corpus
//! (`tests/edge/corpus/`, produced by the deterministic
//! `tests/edge/generate_edge_corpus.py`) and assert, in BOUNDED time, that:
//!
//!   * every VALID edge fixture (1x1, 1xN, Nx1, 16-bit, Adam7-interlaced,
//!     fully-transparent, single-color, palette-with-short-tRNS, gray+alpha,
//!     2-color palette) exits 0 with an empty stderr and writes a
//!     well-formed indexed PNG of the source's own dimensions — and does so
//!     deterministically (twin run byte-identical); and
//!   * every MALFORMED fixture (random bytes, truncated streams, bad CRC,
//!     0x0 dims, absurd pixel-capped-before-allocation IHDR dims, empty file)
//!     exits with a clean, declared nonzero status (never a signal/abort —
//!     `status.code()` is always `Some`, so a panic-abort would be caught),
//!     writes nothing to stdout, and prints a one-line diagnostic to stderr.
//!
//! This complements `tests/adversarial_suite.rs` (T-0110), which probes the
//! in-process `decode_png`/`quant`/`dither`/`pack` library API. This gate is
//! the *binary-level* proof: the exact artifact a release ships, exercised as
//! a subprocess with real file paths, a per-case wall-clock timeout, and the
//! CLI's stdout/stderr discipline enforced.
//!
//! Dependency-free by construction (no test-time Python): the committed
//! fixtures + `manifest.tsv` are read directly. The generator's `--check`
//! mode is the separate anti-drift guard for the reviewer.

use pngprism::png;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Generous per-case wall-clock ceiling. Every fixture completes in well under
/// a second in practice (the absurd-dims cases fail at the decode length check
/// before any allocation); this bound exists only to convert a hypothetical
/// hang into a reported failure rather than a stuck suite. The child is our own
/// spawned process, so killing it on timeout is safe and self-contained.
const PER_CASE_TIMEOUT: Duration = Duration::from_secs(60);

const DECLARED_EXIT_CODES: [i32; 4] = [2, 3, 5, 70];

fn edge_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/edge")
}

struct Case {
    name: String,
    class: String,
    width: u32,
    height: u32,
}

fn read_manifest() -> Vec<Case> {
    let text = std::fs::read_to_string(edge_dir().join("manifest.tsv"))
        .expect("read tests/edge/manifest.tsv");
    let mut cases = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let name = cols.next().expect("name column").to_string();
        let class = cols.next().expect("class column").to_string();
        let width = cols
            .next()
            .expect("width column")
            .parse()
            .expect("width u32");
        let height = cols
            .next()
            .expect("height column")
            .parse()
            .expect("height u32");
        cases.push(Case {
            name,
            class,
            width,
            height,
        });
    }
    cases
}

struct RunOutcome {
    /// `Some(code)` for a normal exit; `None` if the process was terminated by
    /// a signal (crash/abort) or had to be killed for exceeding the timeout.
    code: Option<i32>,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run the binary with `in`/`out` paths, capturing stdout/stderr to files
/// (so a large write can never deadlock a pipe) and enforcing a wall-clock
/// timeout by polling. On timeout the child — which we spawned — is killed.
fn run_binary(input: &Path, output: &Path, scratch: &Path, tag: &str) -> RunOutcome {
    let out_file = scratch.join(format!("{tag}.stdout"));
    let err_file = scratch.join(format!("{tag}.stderr"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_pngprism"))
        .arg(input)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&out_file).expect("create stdout file"),
        ))
        .stderr(Stdio::from(
            File::create(&err_file).expect("create stderr file"),
        ))
        .spawn()
        .expect("spawn pngprism");

    let deadline = Instant::now() + PER_CASE_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break Some(status),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    let stdout = std::fs::read(&out_file).unwrap_or_default();
    let stderr = std::fs::read(&err_file).unwrap_or_default();
    RunOutcome {
        code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
    }
}

#[test]
fn edge_corpus_binary_is_robust_in_bounded_time() {
    let cases = read_manifest();
    let valid_count = cases.iter().filter(|c| c.class == "valid").count();
    let bad_count = cases.iter().filter(|c| c.class == "bad").count();
    assert!(
        valid_count >= 11 && bad_count >= 9,
        "edge manifest looks truncated: {valid_count} valid + {bad_count} bad \
         (expected >= 11 valid + >= 9 bad); did the generator run?"
    );

    let scratch =
        std::env::temp_dir().join(format!("prism-quant-edge-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        let input = edge_dir().join("corpus").join(&case.name);
        let output = scratch.join(format!("out-{}", case.name));
        let _ = std::fs::remove_file(&output);
        let run = run_binary(&input, &output, &scratch, &case.name);

        if run.timed_out {
            failures.push(format!(
                "{}: HANG — exceeded {:?}",
                case.name, PER_CASE_TIMEOUT
            ));
            continue;
        }
        // A `None` code with no timeout means the process died by signal — an
        // abort/segfault/panic-abort. That is the single worst outcome.
        let Some(code) = run.code else {
            failures.push(format!(
                "{}: terminated by signal (crash/abort), not a clean exit",
                case.name
            ));
            continue;
        };

        if case.class == "valid" {
            if code != 0 {
                failures.push(format!(
                    "{}: valid fixture exited {code} (stderr: {})",
                    case.name,
                    String::from_utf8_lossy(&run.stderr).trim()
                ));
                continue;
            }
            if !run.stderr.is_empty() {
                failures.push(format!(
                    "{}: valid fixture wrote to stderr: {}",
                    case.name,
                    String::from_utf8_lossy(&run.stderr).trim()
                ));
            }
            if run.stdout.is_empty() {
                failures.push(format!(
                    "{}: valid fixture wrote no summary line",
                    case.name
                ));
            }
            match std::fs::read(&output) {
                Ok(bytes) => match png::decode_png(&bytes) {
                    Ok(image) => {
                        // The output is the engine's indexed candidate
                        // (color_type 3), OR — under the never-worse guarantee
                        // (T-0210), for these tiny fixtures where indexing adds
                        // net overhead — the input bytes emitted verbatim (any
                        // color type, but still a decodable PNG of the same
                        // dimensions). Both are contract-valid.
                        let input_bytes = std::fs::read(&input).unwrap_or_default();
                        let never_worse_fallback = bytes == input_bytes;
                        if !never_worse_fallback && image.properties.color_type != 3 {
                            failures.push(format!(
                                "{}: output color_type {} (expected 3 indexed, or input verbatim under never-worse)",
                                case.name, image.properties.color_type
                            ));
                        }
                        if (image.width, image.height) != (case.width, case.height) {
                            failures.push(format!(
                                "{}: output {}x{} != source {}x{}",
                                case.name, image.width, image.height, case.width, case.height
                            ));
                        }
                    }
                    Err(err) => failures.push(format!(
                        "{}: output is not a decodable PNG: {}",
                        case.name,
                        err.message()
                    )),
                },
                Err(err) => failures.push(format!("{}: no output file written: {err}", case.name)),
            }
            // Determinism: a twin run must be byte-identical.
            let twin = scratch.join(format!("twin-{}", case.name));
            let twin_run = run_binary(&input, &twin, &scratch, &format!("twin-{}", case.name));
            if twin_run.code == Some(0) {
                let a = std::fs::read(&output).unwrap_or_default();
                let b = std::fs::read(&twin).unwrap_or_default();
                if a != b {
                    failures.push(format!("{}: twin run not byte-identical", case.name));
                }
            } else {
                failures.push(format!("{}: twin run did not exit 0", case.name));
            }
        } else {
            // Malformed: a clean, declared nonzero exit; nothing on stdout; a
            // diagnostic on stderr.
            if code == 0 {
                failures.push(format!(
                    "{}: malformed fixture exited 0 (should be a clean error)",
                    case.name
                ));
                continue;
            }
            if !DECLARED_EXIT_CODES.contains(&code) {
                failures.push(format!(
                    "{}: exit code {code} is not one of the declared statuses {DECLARED_EXIT_CODES:?}",
                    case.name
                ));
            }
            if !run.stdout.is_empty() {
                failures.push(format!(
                    "{}: malformed fixture wrote {} byte(s) to stdout (must be empty on failure)",
                    case.name,
                    run.stdout.len()
                ));
            }
            if run.stderr.is_empty() {
                failures.push(format!(
                    "{}: malformed fixture produced no stderr diagnostic",
                    case.name
                ));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "edge-corpus robustness violations ({} of {} fixtures):\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

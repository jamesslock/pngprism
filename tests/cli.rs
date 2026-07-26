//! CLI contract tests, ported from the oracle suite
//! `lab/reference/test_prism_quant.py` (PrismQuantCliTests): exit
//! statuses (0 success, 2 usage, 3 data, 5 io), one-line diagnostics on
//! stderr, stdout empty on failure, the success summary line.

use pngprism::png;
use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "common/smoke.rs"]
mod smoke;

/// A smoke-set image by id, or the first available one when `id_wanted` is
/// `None`. Every id these tests name explicitly is one of the 24 vendored CC0
/// images, so this is infallible — except `w3c-alphatest`, which is lab-only
/// and is reached through [`smoke::resolve_id`] directly by the one test that
/// needs it, so that test can skip rather than panic (ADR-0033 §2 escape 7).
fn smoke_path(id_wanted: Option<&str>) -> PathBuf {
    match id_wanted {
        Some(id) => smoke::resolve_id(id).unwrap_or_else(|| {
            panic!("smoke item {id:?} is not available here; if it is lab-only, the test must skip")
        }),
        None => smoke::available().swap_remove(0).1,
    }
}

/// Whether `--pack max` can run here: it shells out to `zopflipng`, an OPTIONAL
/// external tool (the default pack mode is `none`, so the common path never
/// looks for it).
///
/// This asks the CRATE's own resolver rather than re-implementing the lookup.
/// There are already two implementations of that policy held in lockstep — the
/// Rust one and the Python oracle's — and a third, living in the tests and
/// quietly disagreeing with both, is exactly the forked brain the parity rule
/// exists to prevent. (An earlier version of this helper checked only
/// `PRISM_ZOPFLIPNG` and `PATH`, and would have skipped in-tree runs where the
/// vendored pinned build was resolvable.) Widening access beats copying, the
/// same call made for `sha256`.
///
/// Tests that need it SKIP when it is absent rather than fail: a consumer who
/// installed the crate and not an optional third-party binary has not broken
/// anything, and a red suite would say they had. CI installs it, so the path is
/// still exercised on every push.
fn zopflipng_available() -> bool {
    pngprism::pack::default_zopflipng().is_some()
}

fn skip_no_zopflipng(test: &str) {
    eprintln!(
        "{test}: SKIPPED the --pack max cases — zopflipng not found. It is an \
         optional external tool; set PRISM_ZOPFLIPNG or install it \
         (`brew install zopfli`, or the `zopfli` package on most distros)."
    );
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pngprism"))
        .args(args)
        .output()
        .expect("run pngprism")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("pngprism-cli-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_success_and_labels() {
    let tmp = TempDir::new("success");
    let out = tmp.path("out.png");
    let item = smoke_path(None);
    let completed = run_cli(&[
        item.to_str().expect("utf8 path"),
        out.as_str(),
        "--colors",
        "128",
    ]);
    assert_eq!(
        completed.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&completed.stderr)
    );
    let stdout = String::from_utf8_lossy(&completed.stdout);
    assert!(stdout.contains("0.5.0"), "stdout: {stdout}");
    assert!(completed.stderr.is_empty());
    let check = png::decode_png(&std::fs::read(&out).expect("read output")).expect("decode output");
    assert_eq!(check.properties.color_type, 3);
    assert!(check.properties.plte.as_ref().map_or(0, Vec::len) <= 128);
}

#[test]
fn cli_hidden_rgb_policy_option() {
    let tmp = TempDir::new("policy");
    let out = tmp.path("out.png");
    let item = smoke_path(Some("syn-hidden-rgb-random"));
    let item = item.to_str().expect("utf8 path");
    for policy in ["canonicalize-black", "preserve-mean"] {
        let completed = run_cli(&[item, out.as_str(), "--hidden-rgb-policy", policy]);
        assert_eq!(
            completed.status.code(),
            Some(0),
            "policy {policy}: stderr {}",
            String::from_utf8_lossy(&completed.stderr)
        );
    }
    let bad = run_cli(&[item, out.as_str(), "--hidden-rgb-policy", "nope"]);
    assert_eq!(bad.status.code(), Some(3));
}

#[test]
fn cli_usage_errors() {
    assert_eq!(run_cli(&[]).status.code(), Some(2));
    assert_eq!(run_cli(&["one.png"]).status.code(), Some(2));
    assert_eq!(run_cli(&["a", "b", "--colors"]).status.code(), Some(2));
    assert_eq!(run_cli(&["a", "b", "--nope", "3"]).status.code(), Some(2));
    assert_eq!(
        run_cli(&["a", "b", "--hidden-rgb-policy"]).status.code(),
        Some(2)
    );
    for flags in [
        &["--threads"][..],
        &["--threads", "0"],
        &["--threads", "-1"],
        &["--threads", "257"],
        &["--threads", "nope"],
        &["--parallel-merge-order"],
        &["--parallel-merge-order", "random"],
        &["--parallel-merge-order", "shuffle:not-a-seed"],
    ] {
        let mut args = vec!["a", "b"];
        args.extend_from_slice(flags);
        assert_eq!(
            run_cli(&args).status.code(),
            Some(2),
            "expected usage error for {flags:?}"
        );
    }
}

#[test]
fn cli_parallel_schedules_are_byte_identical_to_default() {
    let tmp = TempDir::new("parallel");
    let item = smoke_path(Some("syn-alpha-ramp-hue-radial-64"));
    let item = item.to_str().expect("utf8 path");
    let serial = tmp.path("serial.png");
    let completed = run_cli(&[item, serial.as_str(), "--colors", "16"]);
    assert_eq!(completed.status.code(), Some(0));
    let expected = std::fs::read(&serial).expect("read serial output");

    for (threads, order) in [
        ("1", "forward"),
        ("2", "forward"),
        ("3", "reverse"),
        ("7", "balanced"),
        ("64", "shuffle:1592594802"),
    ] {
        let output = tmp.path(&format!("parallel-{threads}-{order}.png"));
        let completed = run_cli(&[
            item,
            output.as_str(),
            "--colors",
            "16",
            "--threads",
            threads,
            "--parallel-merge-order",
            order,
        ]);
        assert_eq!(
            completed.status.code(),
            Some(0),
            "threads={threads}, order={order}: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        assert_eq!(
            std::fs::read(output).expect("read parallel output"),
            expected,
            "threads={threads}, order={order}"
        );
    }
}

#[test]
fn cli_v02_dither_pack_success() {
    // Every v0.2 dither/pack path exits 0 and the output decodes to an indexed
    // PNG. (Byte-parity vs the oracle is the parity sweep's job; this only
    // pins the CLI contract and self-consistency.)
    let tmp = TempDir::new("v02");
    let out = tmp.path("out.png");
    let item = smoke_path(Some("syn-alpha-ramp-hue-radial-64"));
    let item = item.to_str().expect("utf8 path");
    // T-0193: the omission default is now `guarded` (T-0190/E-0038), which is
    // not composable with explicit dither flags. Every set that supplies an
    // explicit dither flag therefore selects `--adaptive-default off` first to
    // reach the intended explicit-dither path — the same honest update the
    // Python oracle's CLI tests took (E-0038 report §Test-expectation changes).
    let flag_sets: &[&[&str]] = &[
        &["--adaptive-default", "off"],
        &["--adaptive-default", "on"],
        &["--adaptive-default", "guarded"],
        &["--adaptive-default", "off", "--dither", "on"],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-strength",
            "0.5",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-strength",
            "0",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither-policy",
            "adaptive-unit",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "adaptive",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "region",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "adaptive-unit",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "adaptive-unit",
            "--dither-strength",
            "0.5",
        ],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "luma-bluenoise",
            "--dither-strength",
            "0.5",
        ],
        &["--color-space", "oklab"],
        &["--pack", "fast", "--pack-search", "v1"],
        &["--pack", "fast", "--pack-search", "v2"],
        &["--pack", "max", "--pack-search", "v1"],
        &["--pack", "max", "--pack-search", "v2"],
        // pack=none seam surface (adopted omission + explicit forms).
        &[
            "--pack-seam-palette-sort",
            "off",
            "--pack-seam-memlevel",
            "off",
            "--pack-seam-reduction",
            "off",
        ],
        &[
            "--pack-seam-palette-sort",
            "on",
            "--pack-seam-memlevel",
            "on",
            "--pack-seam-reduction",
            "on",
        ],
        &["--pack-seam-reduction", "on"],
        &[
            "--adaptive-default",
            "off",
            "--dither",
            "on",
            "--dither-policy",
            "region",
            "--pack",
            "max",
            "--pack-search",
            "v2",
        ],
    ];
    let have_zopflipng = zopflipng_available();
    if !have_zopflipng {
        skip_no_zopflipng("cli_v02_dither_pack_success");
    }
    for flags in flag_sets {
        // Only the `--pack max` sets need the optional binary; every other set
        // still runs, so absence costs those cases and nothing else.
        if !have_zopflipng && flags.contains(&"max") {
            continue;
        }
        let mut args = vec![item, out.as_str(), "--colors", "16"];
        args.extend_from_slice(flags);
        let completed = run_cli(&args);
        assert_eq!(
            completed.status.code(),
            Some(0),
            "flags {flags:?}: stderr {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        let check =
            png::decode_png(&std::fs::read(&out).expect("read output")).expect("decode output");
        assert_eq!(check.properties.color_type, 3, "flags {flags:?}");
    }
}

#[test]
fn cli_v02_usage_errors() {
    // The oracle's main() catches these as usage errors (exit 2) before
    // quantize_png. Vocabulary/composition mismatches on the v0.2 flags.
    let item = smoke_path(None);
    let item = item.to_str().expect("utf8 path");
    let cases: &[&[&str]] = &[
        &["--adaptive-default"],                                     // needs value
        &["--adaptive-default", "maybe"],                            // not on/off
        &["--dither"],                                               // needs value
        &["--dither", "maybe"],                                      // not on/off
        &["--dither-strength"],                                      // needs value
        &["--dither", "on", "--dither-strength", "1.5"],             // out of range
        &["--dither", "on", "--dither-strength", "nope"],            // not a decimal
        &["--dither-policy", "sideways"],                            // not in set
        &["--color-space"],                                          // needs value
        &["--color-space", "xyz"],                                   // not in set
        &["--pack", "medium"],                                       // not in set
        &["--pack-search", "v3"],                                    // not in set
        &["--pack-search", "v2"],                                    // requires --pack != none
        &["--dither-policy", "adaptive"],                            // requires --dither on
        &["--dither-policy", "luma-bluenoise"],                      // requires --dither on
        &["--adaptive-default", "on", "--dither", "off"], // switch forbids explicit dither
        &["--adaptive-default", "on", "--dither-strength", "1.0"], // switch forbids explicit strength, even the default value
        &["--adaptive-default", "on", "--dither-policy", "uniform"], // switch forbids explicit policy, even the default value
        &[
            "--dither",
            "on",
            "--dither-policy",
            "region",
            "--dither-strength",
            "0.5",
        ], // not composable
    ];
    for extra in cases {
        let mut args = vec![item, "/tmp/prism-should-not-write.png"];
        args.extend_from_slice(extra);
        assert_eq!(
            run_cli(&args).status.code(),
            Some(2),
            "expected usage error for {extra:?}"
        );
    }
}

#[test]
fn cli_adaptive_default_policies_match_their_explicit_forms() {
    // T-0193/E-0038 semantics on a NON-guard-firing unit (this alpha ramp has
    // fully-opaque pixels, so `opaque_frac != 0` and the guard does not fire):
    //   * omission == explicit `--adaptive-default guarded`;
    //   * guarded (guard not firing) == `--adaptive-default on`
    //     == the explicit unguarded adaptive-unit path;
    //   * `--adaptive-default on` != `--adaptive-default off`.
    let tmp = TempDir::new("adaptive-default");
    let item = smoke_path(Some("syn-alpha-ramp-hue-radial-64"));
    let item = item.to_str().expect("utf8 path");
    let omission = tmp.path("omission.png");
    let guarded = tmp.path("guarded.png");
    let switch_off = tmp.path("switch-off.png");
    let switch_on = tmp.path("switch-on.png");
    let explicit_policy = tmp.path("explicit-policy.png");

    for (output, flags) in [
        (omission.as_str(), vec!["--colors", "16"]),
        (
            guarded.as_str(),
            vec!["--colors", "16", "--adaptive-default", "guarded"],
        ),
        (
            switch_off.as_str(),
            vec!["--colors", "16", "--adaptive-default", "off"],
        ),
        (
            switch_on.as_str(),
            vec!["--colors", "16", "--adaptive-default", "on"],
        ),
        (
            explicit_policy.as_str(),
            vec![
                "--colors",
                "16",
                "--adaptive-default",
                "off",
                "--dither",
                "on",
                "--dither-policy",
                "adaptive-unit",
            ],
        ),
    ] {
        let mut args = vec![item, output];
        args.extend(flags);
        let completed = run_cli(&args);
        assert_eq!(
            completed.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
    }

    let omission_bytes = std::fs::read(&omission).expect("omission output");
    let guarded_bytes = std::fs::read(&guarded).expect("guarded output");
    let switch_off_bytes = std::fs::read(&switch_off).expect("switch-off output");
    let switch_on_bytes = std::fs::read(&switch_on).expect("switch-on output");
    let explicit_bytes = std::fs::read(&explicit_policy).expect("explicit-policy output");

    assert_eq!(
        omission_bytes, guarded_bytes,
        "omission must equal explicit --adaptive-default guarded"
    );
    assert_eq!(
        guarded_bytes, switch_on_bytes,
        "guarded (guard not firing) must equal --adaptive-default on"
    );
    assert_eq!(
        switch_on_bytes, explicit_bytes,
        "--adaptive-default on must equal the explicit adaptive-unit path"
    );
    assert_ne!(
        switch_on_bytes, switch_off_bytes,
        "on and off must differ on a dithering unit"
    );
}

#[test]
fn cli_v02_determinism() {
    // Twin run: identical flags -> byte-identical output.
    let tmp = TempDir::new("determinism");
    let out_a = tmp.path("a.png");
    let out_b = tmp.path("b.png");
    // The twin run is `--pack max` end to end, so without zopflipng there is
    // nothing to compare; skipped whole rather than silently weakened.
    if !zopflipng_available() {
        skip_no_zopflipng("cli_v02_determinism");
        return;
    }
    let item = smoke_path(Some("syn-hidden-rgb-random"));
    let item = item.to_str().expect("utf8 path");
    let flags = [
        "--colors",
        "32",
        // Explicit dither flags require selecting the frozen `off` policy first
        // now that omission defaults to guarded (T-0190/E-0038).
        "--adaptive-default",
        "off",
        "--dither",
        "on",
        "--dither-policy",
        "region",
        "--pack",
        "max",
        "--pack-search",
        "v2",
    ];
    for out in [out_a.as_str(), out_b.as_str()] {
        let mut args = vec![item, out];
        args.extend_from_slice(&flags);
        assert_eq!(run_cli(&args).status.code(), Some(0));
    }
    assert_eq!(
        std::fs::read(&out_a).expect("a"),
        std::fs::read(&out_b).expect("b"),
        "twin run must be byte-identical"
    );
}

#[test]
fn cli_guard_fires_on_fully_transparent_unit() {
    // T-0190/E-0038: w3c-alphatest is a known guard-firing site (its rounded
    // opaque_frac is 0). Under the guarded default the guard disables dither,
    // so omission == explicit `off` and differs from unguarded `on`.
    let tmp = TempDir::new("guard-fire");
    // No substitute: the premise is this specific image's rounded opaque_frac
    // of 0. It is a W3C demo page file with no redistribution grant, so outside
    // the research tree the test skips rather than assert the same thing about
    // some other picture.
    let Some(item) = smoke::resolve_id("w3c-alphatest") else {
        smoke::skip_lab_only("cli_guard_fires_on_fully_transparent_unit", "w3c-alphatest");
        return;
    };
    let item = item.to_str().expect("utf8 path");
    let omission = tmp.path("omission.png");
    let guarded = tmp.path("guarded.png");
    let off = tmp.path("off.png");
    let on = tmp.path("on.png");
    for (output, flags) in [
        (omission.as_str(), vec![]),
        (guarded.as_str(), vec!["--adaptive-default", "guarded"]),
        (off.as_str(), vec!["--adaptive-default", "off"]),
        (on.as_str(), vec!["--adaptive-default", "on"]),
    ] {
        let mut args = vec![item, output];
        args.extend(flags);
        assert_eq!(
            run_cli(&args).status.code(),
            Some(0),
            "flags for {output} failed"
        );
    }
    let omission_bytes = std::fs::read(&omission).expect("omission");
    let guarded_bytes = std::fs::read(&guarded).expect("guarded");
    let off_bytes = std::fs::read(&off).expect("off");
    let on_bytes = std::fs::read(&on).expect("on");
    assert_eq!(
        omission_bytes, guarded_bytes,
        "omission == explicit guarded"
    );
    assert_eq!(
        guarded_bytes, off_bytes,
        "guard fired: guarded must revert to the frozen off bytes"
    );
    assert_ne!(
        off_bytes, on_bytes,
        "unguarded on must differ from off on this unit"
    );
}

#[test]
fn cli_pack_seam_default_on_is_never_larger_and_frozen() {
    // T-0192/E-0040: on pack=none, omission enables S+R (M off). The result is
    // never larger than the all-seams-off baseline, and omission is byte-equal
    // to the explicit `--pack-seam-palette-sort on --pack-seam-reduction on`
    // form (with memlevel omitted -> off).
    let tmp = TempDir::new("seam-default");
    let item = smoke_path(Some("syn-hidden-rgb-random"));
    let item = item.to_str().expect("utf8 path");
    let omission = tmp.path("omission.png");
    let all_off = tmp.path("all-off.png");
    let explicit_sr = tmp.path("explicit-sr.png");
    for (output, flags) in [
        (omission.as_str(), vec!["--colors", "256"]),
        (
            all_off.as_str(),
            vec![
                "--colors",
                "256",
                "--pack-seam-palette-sort",
                "off",
                "--pack-seam-memlevel",
                "off",
                "--pack-seam-reduction",
                "off",
            ],
        ),
        (
            explicit_sr.as_str(),
            vec![
                "--colors",
                "256",
                "--pack-seam-palette-sort",
                "on",
                "--pack-seam-reduction",
                "on",
            ],
        ),
    ] {
        let mut args = vec![item, output];
        args.extend(flags);
        assert_eq!(
            run_cli(&args).status.code(),
            Some(0),
            "flags for {output} failed"
        );
    }
    let omission_bytes = std::fs::read(&omission).expect("omission");
    let all_off_bytes = std::fs::read(&all_off).expect("all-off");
    let explicit_sr_bytes = std::fs::read(&explicit_sr).expect("explicit-sr");
    assert_eq!(
        omission_bytes, explicit_sr_bytes,
        "omission must equal explicit S-on + R-on (M off)"
    );
    assert!(
        omission_bytes.len() <= all_off_bytes.len(),
        "seam-on omission ({} B) must never exceed the all-off baseline ({} B)",
        omission_bytes.len(),
        all_off_bytes.len()
    );
}

#[test]
fn cli_pack_seam_flags_are_pack_none_only() {
    // T-0192/E-0040: explicit seam-ON with --pack fast|max is a frozen usage
    // error; explicit seam-OFF is allowed (it changes nothing there).
    let item = smoke_path(Some("syn-hidden-rgb-random"));
    let item = item.to_str().expect("utf8 path");
    let out = "/tmp/prism-seam-should-not-write.png";
    for mode in ["fast", "max"] {
        for seam in [
            "--pack-seam-palette-sort",
            "--pack-seam-memlevel",
            "--pack-seam-reduction",
        ] {
            let on = run_cli(&[item, out, "--pack", mode, seam, "on"]);
            assert_eq!(
                on.status.code(),
                Some(2),
                "explicit {seam} on with --pack {mode} must be a usage error"
            );
        }
    }
    // explicit off composes silently with a packer.
    let tmp = TempDir::new("seam-off-pack");
    let ok_out = tmp.path("ok.png");
    let ok = run_cli(&[
        item,
        ok_out.as_str(),
        "--pack",
        "fast",
        "--pack-seam-palette-sort",
        "off",
    ]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "explicit seam-off + pack is fine"
    );
}

#[test]
fn cli_pack_seam_usage_vocabulary_errors() {
    let item = smoke_path(None);
    let item = item.to_str().expect("utf8 path");
    let out = "/tmp/prism-seam-vocab-should-not-write.png";
    for extra in [
        &["--pack-seam-palette-sort"][..],
        &["--pack-seam-palette-sort", "maybe"],
        &["--pack-seam-memlevel", "yes"],
        &["--pack-seam-reduction"],
        &["--adaptive-default", "maybe"],
    ] {
        let mut args = vec![item, out];
        args.extend_from_slice(extra);
        assert_eq!(
            run_cli(&args).status.code(),
            Some(2),
            "expected usage error for {extra:?}"
        );
    }
}

#[test]
fn cli_data_and_io_errors() {
    let tmp = TempDir::new("errors");
    let bad = tmp.path("bad.png");
    std::fs::write(&bad, b"not a png").expect("write bad png");
    let out = tmp.path("out.png");
    assert_eq!(
        run_cli(&[bad.as_str(), out.as_str()]).status.code(),
        Some(3)
    );
    assert_eq!(
        run_cli(&[bad.as_str(), out.as_str(), "--colors", "0"])
            .status
            .code(),
        Some(3)
    );
    let missing = tmp.path("missing.png");
    assert_eq!(
        run_cli(&[missing.as_str(), out.as_str()]).status.code(),
        Some(5)
    );
}

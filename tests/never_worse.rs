//! T-0210 CLI-contract tests (Rust side of the dual implementation):
//! the never-worse output guarantee, the `--version` / `--help` surface, the
//! `--report json` schema, and the pinned exit-code contract. The Python
//! reference carries the byte-for-byte twin of every check (differential gate:
//! `parity/T-0210_cli_contract.py`; doc: `docs/cli-contract.md`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common/smoke.rs"]
mod smoke;

/// The digest of the golden corpus's first unit, which is also the byte-identical
/// vendored copy at `tests/never_worse/corpus/golden-first-unit.png`. Pinned so
/// the two cannot drift: whichever one [`normal_source`] hands back, it is
/// provably the same image, and these tests' size assertions therefore mean the
/// same thing in the research tree and outside it.
const GOLDEN_FIRST_UNIT_SHA256: &str =
    "7a75a47ef0d7d74a3df8c6ada0373c97ea59b1fc66a31c12aa7c22eb9be4c72b";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/never_worse/corpus")
        .join(name)
}

/// A real corpus image (default emission is comfortably smaller than input):
/// the golden corpus's first unit in the research tree, else the vendored copy
/// (ADR-0033 §2 — the image is Prism's own CC0 synthetic corpus, §6). Both are
/// checked against [`GOLDEN_FIRST_UNIT_SHA256`] before use.
fn normal_source() -> PathBuf {
    let path = smoke::repo_root()
        .map(|root| root.join("research/project-prism/benchmarks/golden-corpus/manifest.json"))
        .filter(|manifest| manifest.is_file())
        .map_or_else(
            || fixture("golden-first-unit.png"),
            |manifest| {
                let text = std::fs::read_to_string(&manifest).expect("golden manifest");
                // Cheap hand parse of the first row's "source" (avoid a serde dep).
                let key = "\"source\":";
                let start = text.find(key).expect("source key") + key.len();
                let rest = &text[start..];
                let open = rest.find('"').expect("open quote") + 1;
                let close = rest[open..].find('"').expect("close quote") + open;
                smoke::repo_root()
                    .expect("in the research tree")
                    .join(&rest[open..close])
            },
        );
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(
        pngprism::sha256::hex(&bytes),
        GOLDEN_FIRST_UNIT_SHA256,
        "{} is not the pinned golden first unit",
        path.display()
    );
    path
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
        let dir = std::env::temp_dir().join(format!("pngprism-nw-{}-{tag}", std::process::id()));
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

/// Read one string/number/bool value for `key` out of a flat compact JSON
/// object. The report is a single line with no nesting, so this is enough.
fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":");
    let start = json.find(&needle).expect("key present") + needle.len();
    let rest = &json[start..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"').expect("string close");
        &stripped[..end]
    } else {
        let end = rest.find([',', '}']).expect("value close");
        &rest[..end]
    }
}

#[test]
fn version_flag_prints_crate_version_and_exits_zero() {
    let out = run_cli(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    // ONE version source: the crate version (CARGO_PKG_VERSION).
    assert_eq!(
        stdout.trim(),
        format!("pngprism {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_flag_lists_the_flag_surface_and_exits_zero() {
    let out = run_cli(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    for flag in [
        "--colors",
        "--hidden-rgb-policy",
        "--color-space",
        "--adaptive-default",
        "--dither",
        "--dither-strength",
        "--dither-policy",
        "--pack",
        "--pack-search",
        "--pack-seam-palette-sort",
        "--pack-seam-memlevel",
        "--pack-seam-reduction",
        "--threads",
        "--parallel-merge-order",
        "--report",
        "--version",
        "--help",
    ] {
        assert!(stdout.contains(flag), "help missing {flag}");
    }
    assert!(out.stderr.is_empty());
}

#[test]
fn help_wins_when_both_help_and_version_present() {
    let out = run_cli(&["--version", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.starts_with("usage: pngprism"));
}

#[test]
fn report_json_schema_on_a_normal_unit() {
    let tmp = TempDir::new("report");
    let out = run_cli(&[
        normal_source().to_str().expect("utf8"),
        &tmp.path("o.png"),
        "--colors",
        "32",
        "--report",
        "json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let line = stdout.trim();
    // Stable key order + exact schema id.
    assert!(
        line.starts_with("{\"schema_version\":\"prism.cli.report/1\","),
        "{line}"
    );
    assert!(line.ends_with('}'));
    assert_eq!(json_field(line, "candidate"), "encoded");
    assert_eq!(json_field(line, "never_worse_fallback"), "false");
    assert_eq!(json_field(line, "guard"), "guarded");
    // bytes_out < bytes_in on a normal unit.
    let bin: u64 = json_field(line, "bytes_in").parse().expect("int");
    let bout: u64 = json_field(line, "bytes_out").parse().expect("int");
    assert!(bout < bin, "expected smaller output: {bout} < {bin}");
}

#[test]
fn never_worse_trips_on_every_exception_fixture() {
    for name in ["tiny.png", "already-palette.png", "incompressible.png"] {
        let tmp = TempDir::new("nw");
        let src = fixture(name);
        let out_path = tmp.path("out.png");
        let out = run_cli(&[src.to_str().expect("utf8"), &out_path, "--report", "json"]);
        assert_eq!(out.status.code(), Some(0), "{name}");
        let line = String::from_utf8(out.stdout).expect("utf8");
        let line = line.trim();
        assert_eq!(
            json_field(line, "never_worse_fallback"),
            "true",
            "{name}: {line}"
        );
        assert_eq!(json_field(line, "candidate"), "input-verbatim", "{name}");
        // The emitted file is the INPUT bytes verbatim.
        let input = std::fs::read(&src).expect("read input");
        let emitted = std::fs::read(&out_path).expect("read output");
        assert_eq!(emitted, input, "{name}: output must equal input bytes");
        // And the report's bytes reconcile.
        assert_eq!(
            json_field(line, "bytes_out").parse::<usize>().expect("int"),
            input.len(),
            "{name}"
        );
    }
}

fn assert_alias_fallback_preserves_source(source: &str, output: &str) {
    let original = std::fs::read(source).expect("read original");
    let out = run_cli(&[source, output, "--report", "json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8(out.stdout).expect("utf8 report");
    assert_eq!(json_field(report.trim(), "never_worse_fallback"), "true");
    assert_eq!(std::fs::read(source).expect("read source"), original);
    assert_eq!(std::fs::read(output).expect("read output"), original);
}

#[test]
fn never_worse_is_safe_when_input_and_output_are_the_same_path() {
    let tmp = TempDir::new("same-path");
    let path = tmp.path("image.png");
    std::fs::copy(fixture("tiny.png"), &path).expect("copy fixture");
    assert_alias_fallback_preserves_source(&path, &path);
}

#[test]
fn never_worse_is_safe_when_output_is_a_hardlink_to_input() {
    let tmp = TempDir::new("hardlink");
    let source = tmp.path("source.png");
    let output = tmp.path("output.png");
    std::fs::copy(fixture("tiny.png"), &source).expect("copy fixture");
    std::fs::hard_link(&source, &output).expect("create hardlink alias");
    assert_alias_fallback_preserves_source(&source, &output);
}

#[cfg(unix)]
#[test]
fn never_worse_is_safe_when_output_is_a_symlink_to_input() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let tmp = TempDir::new("symlink");
    let source = tmp.path("source.png");
    let output = tmp.path("output.png");
    std::fs::copy(fixture("tiny.png"), &source).expect("copy fixture");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))
        .expect("make source private");
    symlink(&source, &output).expect("create symlink alias");
    assert_alias_fallback_preserves_source(&source, &output);
    assert_eq!(file_mode(&output), 0o600);
    assert!(
        !std::fs::symlink_metadata(&output)
            .expect("output metadata")
            .file_type()
            .is_symlink(),
        "atomic publication replaces the destination entry, not its target"
    );
}

#[cfg(unix)]
#[test]
fn publication_rejects_a_symlink_to_a_non_file() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new("symlink-directory");
    let directory = tmp.0.join("target-directory");
    std::fs::create_dir(&directory).expect("create target directory");
    let output = tmp.path("output.png");
    symlink(&directory, &output).expect("create symlink");
    let run = run_cli(&[fixture("tiny.png").to_str().expect("utf8 source"), &output]);
    assert_eq!(run.status.code(), Some(5));
    assert!(
        std::fs::symlink_metadata(&output)
            .expect("symlink remains")
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
fn file_mode(path: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .expect("file metadata")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(unix)]
fn assert_publication_mode(source: &Path, output: &str, expected_mode: u32, fallback: bool) {
    let source = source.to_str().expect("utf8 source");
    let mut args = vec![source, output];
    if !fallback {
        args.extend(["--colors", "32"]);
    }
    args.extend(["--report", "json"]);
    let run = run_cli(&args);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = String::from_utf8(run.stdout).expect("utf8 report");
    assert_eq!(
        json_field(report.trim(), "never_worse_fallback"),
        if fallback { "true" } else { "false" }
    );
    assert_eq!(file_mode(output), expected_mode);
}

#[cfg(unix)]
#[test]
fn fresh_encoded_output_uses_ordinary_umask_governed_mode() {
    let tmp = TempDir::new("fresh-encoded-mode");
    let output = tmp.path("output.png");
    let control = tmp.path("ordinary-write");
    std::fs::write(&control, b"control").expect("ordinary create");
    assert_publication_mode(&normal_source(), &output, file_mode(&control), false);
}

#[cfg(unix)]
#[test]
fn fresh_fallback_output_uses_ordinary_umask_governed_mode() {
    let tmp = TempDir::new("fresh-fallback-mode");
    let output = tmp.path("output.png");
    let control = tmp.path("ordinary-write");
    std::fs::write(&control, b"control").expect("ordinary create");
    assert_publication_mode(&fixture("tiny.png"), &output, file_mode(&control), true);
}

#[cfg(unix)]
#[test]
fn encoded_publication_preserves_existing_destination_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("existing-encoded-mode");
    let output = tmp.path("output.png");
    std::fs::write(&output, b"old output").expect("existing output");
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o640))
        .expect("set output mode");
    assert_publication_mode(&normal_source(), &output, 0o640, false);
}

#[cfg(unix)]
#[test]
fn fallback_publication_preserves_existing_destination_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new("existing-fallback-mode");
    let output = tmp.path("output.png");
    std::fs::write(&output, b"old output").expect("existing output");
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o604))
        .expect("set output mode");
    assert_publication_mode(&fixture("tiny.png"), &output, 0o604, true);
}

#[cfg(unix)]
#[test]
fn publication_accepts_a_name_max_destination_basename() {
    let tmp = TempDir::new("name-max");
    let output = tmp.path(&"o".repeat(255));
    let run = run_cli(&[
        normal_source().to_str().expect("utf8 source"),
        &output,
        "--colors",
        "32",
        "--report",
        "json",
    ]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(Path::new(&output).is_file());
}

#[test]
fn never_worse_human_note_on_fallback() {
    let tmp = TempDir::new("note");
    let out = run_cli(&[
        fixture("tiny.png").to_str().expect("utf8"),
        &tmp.path("o.png"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.contains("never-worse:"),
        "missing human note: {stdout}"
    );
    assert!(stdout.contains("emitted input verbatim"));
}

#[test]
fn exit_codes_match_the_documented_contract() {
    let tmp = TempDir::new("exit");
    let real = normal_source();
    let real = real.to_str().expect("utf8");
    let malformed = tmp.path("bad.png");
    std::fs::write(&malformed, b"\x89PNG\r\n\x1a\nnope").expect("write malformed");
    let missing = tmp.path("missing.png");

    // (args, expected exit code) — docs/cli-contract.md.
    let ok = tmp.path("ok.png");
    let u1 = tmp.path("u1.png");
    let u2 = tmp.path("u2.png");
    let u3 = tmp.path("u3.png");
    let u4 = tmp.path("u4.png");
    let u5 = tmp.path("u5.png");
    let u6 = tmp.path("u6.png");
    let cases: &[(&[&str], i32)] = &[
        (&["--help"], 0),
        (&["--version"], 0),
        (&[real, &ok, "--colors", "16"], 0),
        (&[], 2),
        (&[real], 2),
        (&[real, &u1, "--colors"], 2),
        (&[real, &u2, "--nope", "1"], 2),
        (&[real, &u3, "--report", "yaml"], 2),
        (&[real, &u4, "--colors", "0"], 3),
        (&[&malformed, &u5], 3),
        (&[&missing, &u6], 5),
    ];
    for (args, want) in cases {
        let got = run_cli(args).status.code();
        assert_eq!(
            got,
            Some(*want),
            "args {args:?} expected exit {want}, got {got:?}"
        );
    }
}

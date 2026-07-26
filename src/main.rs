//! `pngprism` — CLI mirror of `lab/reference/prism_quant.py`'s pipeline.
//! `main`: bounded option parsing, declared exit statuses (0 success,
//! 2 usage, 3 data error, 5 input I/O error, 70 internal), one-line
//! diagnostics on stderr, stdout empty on failure.
//!
//! Usage: `pngprism <in.png> <out.png> [--colors N]
//! [--hidden-rgb-policy P] [--color-space srgb|oklab]
//! [--adaptive-default off|on|guarded]
//! [--dither off|on] [--dither-strength S]
//! [--dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise]
//! [--pack none|fast|max]
//! [--pack-search v1|v2]
//! [--pack-seam-palette-sort off|on] [--pack-seam-memlevel off|on]
//! [--pack-seam-reduction off|on] [--threads N]
//! [--parallel-merge-order balanced|forward|reverse|shuffle:SEED]
//! [--report json] [--version] [--help]`
//!
//! T-0210 adds the production CLI contract: the never-worse output guarantee
//! (emit the input bytes verbatim when the encoded output would be >= the
//! input), `--version`/`--help`, and the `--report json` machine-readable
//! report. The contract + semver policy live in `docs/cli-contract.md`.

use prism_quant::dither::parse_dither_strength;
use prism_quant::{
    AdaptiveDefault, COLOR_SPACES, DEFAULT_COLOR_SPACE, DEFAULT_COLORS, DEFAULT_DITHER,
    DEFAULT_DITHER_POLICY, DEFAULT_DITHER_STRENGTH, DEFAULT_HIDDEN_RGB_POLICY, DEFAULT_PACK_MODE,
    DEFAULT_PACK_SEARCH, DITHER_POLICIES, Error, Kind, LABEL, MAX_THREADS, MergeOrder, PACK_MODES,
    PACK_SEARCHES, Parallelism, quantize_png_bytes_with_parallelism,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

/// `--help` text. Lists every flag this binary accepts (one `--flag` token
/// each) so the flag surface is machine-enumerable from `--help` output —
/// the dual-implementation flag-parity contract (T-0210, `docs/cli-contract.md`)
/// compares this set against the Python reference's. Flags present here but
/// not in the reference (`--threads`, `--parallel-merge-order`) and vice
/// versa (`--colors-search`) are the documented, pinned divergences.
const HELP: &str = "\
usage: pngprism <in.png> <out.png> [options]

Quantize a PNG to an indexed PNG. On success the engine's encoded output is
written to <out.png>; if that output would be >= the input file's bytes, the
input bytes are emitted verbatim instead (the never-worse guarantee).

positional arguments:
  <in.png>                    source PNG to quantize
  <out.png>                   destination indexed PNG

options:
  --colors N                  palette-size ceiling (1..=256; default 256)
  --hidden-rgb-policy P        fully-transparent RGB policy
  --color-space srgb|oklab    quantization color space
  --adaptive-default off|on|guarded   adaptive-unit dither default policy
  --dither off|on             enable error-diffusion dither
  --dither-strength S          exact dither strength ratio in 0..1
  --dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise
  --pack none|fast|max        lossless indexed-PNG packing search
  --pack-search v1|v2         packing search variant
  --pack-seam-palette-sort off|on
  --pack-seam-memlevel off|on
  --pack-seam-reduction off|on
  --max-pixels N              pixel admission ceiling (>=1; default 67108864 =
                              64 Mi-pixel); overrides up or down, hard bound
  --threads N                 stage-parallel worker count (1..; Rust-only)
  --parallel-merge-order balanced|forward|reverse|shuffle:SEED  (Rust-only)
  --report json               emit a machine-readable JSON report on stdout
  --version                   print the version and exit
  --help                      print this help and exit

exit codes: 0 success, 2 usage error, 3 data error, 5 input I/O error,
70 internal error. See docs/cli-contract.md for the stability policy.
";

fn exit_code(kind: Kind) -> u8 {
    match kind {
        Kind::Io => 5,
        Kind::Data => 3,
        Kind::Internal => 70,
        Kind::Usage => 2,
    }
}

fn report_error(error: &Error) -> ExitCode {
    eprintln!("{error}");
    ExitCode::from(exit_code(error.kind()))
}

/// A sibling temporary file used to keep candidate generation separate from
/// final publication. `Drop` removes an unpublished candidate on every error
/// path; `publish` is one same-directory rename on platforms where rename
/// replaces an existing destination atomically.
struct StagedOutput {
    path: PathBuf,
    destination_permissions: Option<std::fs::Permissions>,
    published: bool,
}

impl StagedOutput {
    fn create(destination: &Path) -> Result<Self, Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        // Preserve a regular destination's mode. For a symlink, borrow mode
        // only from a regular target so private source bytes never sit in a
        // broader-mode staging file; publication still replaces the symlink
        // directory entry rather than writing through it.
        let destination_permissions = match std::fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.file_type().is_file() => Some(metadata.permissions()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                match std::fs::metadata(destination) {
                    Ok(target) if target.file_type().is_file() => Some(target.permissions()),
                    Ok(_) => {
                        return Err(Error::new(
                            Kind::Io,
                            format!(
                                "io_error: cannot write {}: symlink target is not a regular file",
                                destination.display()
                            ),
                        ));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(err) => {
                        return Err(Error::new(
                            Kind::Io,
                            format!("io_error: cannot write {}: {err}", destination.display()),
                        ));
                    }
                }
            }
            Ok(_) => None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(Error::new(
                    Kind::Io,
                    format!("io_error: cannot write {}: {err}", destination.display()),
                ));
            }
        };
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        for _ in 0..128 {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            // Destination-independent basename: the destination itself may
            // already consume NAME_MAX (commonly 255 bytes).
            let path = parent.join(format!(".pngprism-{}-{unique}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    if let Some(permissions) = destination_permissions.as_ref() {
                        let mut staging_permissions = permissions.clone();
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            staging_permissions =
                                std::fs::Permissions::from_mode(staging_permissions.mode() | 0o200);
                        }
                        #[cfg(not(unix))]
                        staging_permissions.set_readonly(false);
                        if let Err(err) = file.set_permissions(staging_permissions) {
                            drop(file);
                            let _ = std::fs::remove_file(&path);
                            return Err(Error::new(
                                Kind::Io,
                                format!("io_error: cannot write {}: {err}", destination.display()),
                            ));
                        }
                    }
                    drop(file);
                    return Ok(Self {
                        path,
                        destination_permissions,
                        published: false,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(Error::new(
                        Kind::Io,
                        format!("io_error: cannot write {}: {err}", destination.display()),
                    ));
                }
            }
        }
        Err(Error::new(
            Kind::Io,
            format!(
                "io_error: cannot write {}: could not reserve a temporary output",
                destination.display()
            ),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(mut self, destination: &Path) -> Result<(), Error> {
        if let Some(permissions) = self.destination_permissions.take() {
            std::fs::set_permissions(&self.path, permissions).map_err(|err| {
                Error::new(
                    Kind::Io,
                    format!("io_error: cannot write {}: {err}", destination.display()),
                )
            })?;
        }
        std::fs::rename(&self.path, destination).map_err(|err| {
            Error::new(
                Kind::Io,
                format!("io_error: cannot write {}: {err}", destination.display()),
            )
        })?;
        self.published = true;
        Ok(())
    }
}

impl Drop for StagedOutput {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Unicode 16.0 `Nd` zero code points from `DerivedNumericType.txt`.
/// Every decimal digit block has ten consecutive code points; adjacent
/// blocks therefore each appear here independently.
const UNICODE_16_DECIMAL_ZEROES: [u32; 76] = [
    0x0030, 0x0660, 0x06f0, 0x07c0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66, 0x0ce6,
    0x0d66, 0x0de6, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80,
    0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
    0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
    0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
    0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
    0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

fn unicode_decimal_digit(character: char) -> Option<u8> {
    let codepoint = u32::from(character);
    let block = UNICODE_16_DECIMAL_ZEROES
        .partition_point(|&zero| zero <= codepoint)
        .checked_sub(1)?;
    let offset = codepoint - UNICODE_16_DECIMAL_ZEROES[block];
    (offset < 10).then_some(offset as u8)
}

/// Parse an integer the way Python's `int(str, 10)` does (the CLI
/// contract): ASCII-whitespace trim, one optional sign, Unicode 16.0 decimal
/// digits, and PEP-515 single underscores between digits. Returns `None` on
/// invalid syntax (the oracle's ValueError path). Values outside i64 saturate,
/// preserving the oracle's observable behavior (out-of-range -> exit 3, not
/// 2).
fn python_int(text: &str) -> Option<i64> {
    let trimmed =
        text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    let digits = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    // PEP 515: underscores are accepted only between decimal digits. Fold
    // directly into a bounded magnitude so an arbitrarily long argv value
    // cannot trigger a proportional allocation or integer-parse overflow.
    let magnitude_limit = i64::MAX as u64 + 1;
    let mut magnitude = 0u64;
    let mut saw_digit = false;
    let mut previous_was_digit = false;
    for character in digits.chars() {
        if character == '_' {
            if !previous_was_digit {
                return None;
            }
            previous_was_digit = false;
            continue;
        }
        let digit = unicode_decimal_digit(character)?;
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(u64::from(digit))
            .min(magnitude_limit);
        saw_digit = true;
        previous_was_digit = true;
    }
    if !saw_digit || !previous_was_digit {
        return None;
    }
    let negative = trimmed.starts_with('-');
    let magnitude = i128::from(magnitude);
    let signed = if negative { -magnitude } else { magnitude };
    Some(signed.clamp(i64::MIN.into(), i64::MAX.into()) as i64)
}

fn parse_merge_order(text: &str) -> Option<MergeOrder> {
    match text {
        "balanced" => Some(MergeOrder::Balanced),
        "forward" => Some(MergeOrder::Forward),
        "reverse" => Some(MergeOrder::Reverse),
        _ => text
            .strip_prefix("shuffle:")
            .and_then(|seed| seed.parse::<u64>().ok())
            .map(MergeOrder::Shuffled),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--help`/`--version` short-circuit anywhere in argv (GNU convention),
    // help winning when both are present. The version string has ONE source
    // per impl: here it is the crate version (`CARGO_PKG_VERSION`, i.e. the
    // `[package] version` in Cargo.toml). See docs/cli-contract.md.
    if args.iter().any(|arg| arg == "--help") {
        print!("{HELP}");
        return ExitCode::from(0);
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("pngprism {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::from(0);
    }
    let mut colors = DEFAULT_COLORS;
    let mut hidden_rgb_policy = DEFAULT_HIDDEN_RGB_POLICY.to_string();
    let mut color_space = DEFAULT_COLOR_SPACE.to_string();
    // Omission resolves to the guarded adaptive-unit default (T-0190/E-0038).
    let mut adaptive_default = AdaptiveDefault::Guarded;
    let mut pack_seam_palette_sort: Option<bool> = None;
    let mut pack_seam_memlevel: Option<bool> = None;
    let mut pack_seam_reduction: Option<bool> = None;
    let mut dither = DEFAULT_DITHER;
    let mut dither_explicit = false;
    let mut dither_strength = DEFAULT_DITHER_STRENGTH;
    let mut dither_strength_explicit = false;
    let mut dither_policy = DEFAULT_DITHER_POLICY.to_string();
    let mut dither_policy_explicit = false;
    let mut pack_mode = DEFAULT_PACK_MODE.to_string();
    let mut pack_search = DEFAULT_PACK_SEARCH.to_string();
    let mut pack_search_explicit = false;
    let mut threads = 1usize;
    let mut merge_order = MergeOrder::Balanced;
    let mut max_pixels: Option<u64> = None;
    let mut report_json = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = args[index].as_str();
        match token {
            "--colors" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --colors needs a value");
                    return ExitCode::from(2);
                }
                match python_int(&args[index + 1]) {
                    Some(value) => colors = value,
                    None => {
                        eprintln!("usage_error: --colors must be an integer");
                        return ExitCode::from(2);
                    }
                }
                index += 2;
            }
            "--hidden-rgb-policy" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --hidden-rgb-policy needs a value");
                    return ExitCode::from(2);
                }
                hidden_rgb_policy = args[index + 1].clone();
                index += 2;
            }
            "--color-space" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --color-space needs a value");
                    return ExitCode::from(2);
                }
                color_space = args[index + 1].clone();
                if !COLOR_SPACES.contains(&color_space.as_str()) {
                    eprintln!("usage_error: --color-space must be srgb or oklab");
                    return ExitCode::from(2);
                }
                index += 2;
            }
            "--adaptive-default" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --adaptive-default needs a value");
                    return ExitCode::from(2);
                }
                match AdaptiveDefault::parse(args[index + 1].as_str()) {
                    Some(value) => adaptive_default = value,
                    None => {
                        eprintln!("usage_error: --adaptive-default must be off, on, or guarded");
                        return ExitCode::from(2);
                    }
                }
                index += 2;
            }
            "--pack-seam-palette-sort" | "--pack-seam-memlevel" | "--pack-seam-reduction" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: {token} needs a value");
                    return ExitCode::from(2);
                }
                let value = args[index + 1].as_str();
                let enabled = match value {
                    "off" => false,
                    "on" => true,
                    _ => {
                        eprintln!("usage_error: {token} must be off or on");
                        return ExitCode::from(2);
                    }
                };
                match token {
                    "--pack-seam-palette-sort" => pack_seam_palette_sort = Some(enabled),
                    "--pack-seam-memlevel" => pack_seam_memlevel = Some(enabled),
                    _ => pack_seam_reduction = Some(enabled),
                }
                index += 2;
            }
            "--dither" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --dither needs a value");
                    return ExitCode::from(2);
                }
                let value = args[index + 1].as_str();
                if value != "off" && value != "on" {
                    eprintln!("usage_error: --dither must be off or on");
                    return ExitCode::from(2);
                }
                dither = value == "on";
                dither_explicit = true;
                index += 2;
            }
            "--dither-strength" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --dither-strength needs a value");
                    return ExitCode::from(2);
                }
                match parse_dither_strength(&args[index + 1]) {
                    Ok(ratio) => dither_strength = ratio,
                    Err(err) => {
                        return report_error(&err);
                    }
                }
                dither_strength_explicit = true;
                index += 2;
            }
            "--dither-policy" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --dither-policy needs a value");
                    return ExitCode::from(2);
                }
                dither_policy = args[index + 1].clone();
                dither_policy_explicit = true;
                if !DITHER_POLICIES.contains(&dither_policy.as_str()) {
                    eprintln!(
                        "usage_error: --dither-policy must be uniform, adaptive, region, adaptive-unit, or luma-bluenoise"
                    );
                    return ExitCode::from(2);
                }
                index += 2;
            }
            "--pack" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --pack needs a value");
                    return ExitCode::from(2);
                }
                pack_mode = args[index + 1].clone();
                if !PACK_MODES.contains(&pack_mode.as_str()) {
                    eprintln!("usage_error: --pack must be none, fast, or max");
                    return ExitCode::from(2);
                }
                index += 2;
            }
            "--pack-search" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --pack-search needs a value");
                    return ExitCode::from(2);
                }
                pack_search = args[index + 1].clone();
                if !PACK_SEARCHES.contains(&pack_search.as_str()) {
                    eprintln!("usage_error: --pack-search must be v1 or v2");
                    return ExitCode::from(2);
                }
                pack_search_explicit = true;
                index += 2;
            }
            "--threads" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --threads needs a value");
                    return ExitCode::from(2);
                }
                match python_int(&args[index + 1])
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|&value| (1..=MAX_THREADS).contains(&value))
                {
                    Some(value) => threads = value,
                    None => {
                        eprintln!("usage_error: --threads must be an integer in 1..={MAX_THREADS}");
                        return ExitCode::from(2);
                    }
                }
                index += 2;
            }
            "--parallel-merge-order" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --parallel-merge-order needs a value");
                    return ExitCode::from(2);
                }
                match parse_merge_order(&args[index + 1]) {
                    Some(value) => merge_order = value,
                    None => {
                        eprintln!(
                            "usage_error: --parallel-merge-order must be balanced, forward, reverse, or shuffle:SEED"
                        );
                        return ExitCode::from(2);
                    }
                }
                index += 2;
            }
            "--max-pixels" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --max-pixels needs a value");
                    return ExitCode::from(2);
                }
                match python_int(&args[index + 1]) {
                    Some(value) if value >= 1 => max_pixels = Some(value as u64),
                    Some(_) => {
                        eprintln!("usage_error: --max-pixels must be a positive integer");
                        return ExitCode::from(2);
                    }
                    None => {
                        eprintln!("usage_error: --max-pixels must be an integer");
                        return ExitCode::from(2);
                    }
                }
                index += 2;
            }
            "--report" => {
                if index + 1 >= args.len() {
                    eprintln!("usage_error: --report needs a value");
                    return ExitCode::from(2);
                }
                if args[index + 1].as_str() != "json" {
                    eprintln!("usage_error: --report must be json");
                    return ExitCode::from(2);
                }
                report_json = true;
                index += 2;
            }
            _ if token.starts_with('-') => {
                eprintln!("usage_error: unknown option {token}");
                return ExitCode::from(2);
            }
            _ => {
                positional.push(token);
                index += 1;
            }
        }
    }
    if matches!(
        adaptive_default,
        AdaptiveDefault::On | AdaptiveDefault::Guarded
    ) && (dither_explicit || dither_strength_explicit || dither_policy_explicit)
    {
        eprintln!(
            "usage_error: --adaptive-default {} is not composable with explicit dither options",
            adaptive_default.as_str()
        );
        return ExitCode::from(2);
    }
    if pack_search_explicit && pack_mode == "none" {
        eprintln!("usage_error: --pack-search requires --pack fast or max");
        return ExitCode::from(2);
    }
    if pack_mode != "none"
        && [
            pack_seam_palette_sort,
            pack_seam_memlevel,
            pack_seam_reduction,
        ]
        .contains(&Some(true))
    {
        eprintln!(
            "usage_error: --pack-seam-* flags apply to the pack=none emission path only \
             (--pack fast/max runs its own byte search)"
        );
        return ExitCode::from(2);
    }
    if (dither_policy == "adaptive"
        || dither_policy == "region"
        || dither_policy == "luma-bluenoise")
        && !dither
    {
        eprintln!("usage_error: --dither-policy {dither_policy} requires --dither on");
        return ExitCode::from(2);
    }
    if (dither_policy == "adaptive" || dither_policy == "region")
        && dither_strength != DEFAULT_DITHER_STRENGTH
    {
        eprintln!(
            "usage_error: --dither-strength is not composable with --dither-policy {dither_policy} (policy supplies exact strengths)"
        );
        return ExitCode::from(2);
    }
    if positional.len() != 2 {
        eprintln!(
            "usage: pngprism <in.png> <out.png> [--colors N] \
             [--hidden-rgb-policy P] [--color-space srgb|oklab] \
             [--adaptive-default off|on|guarded] \
             [--dither off|on] [--dither-strength S] \
             [--dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise] \
             [--pack none|fast|max] \
             [--pack-search v1|v2] \
             [--pack-seam-palette-sort off|on] [--pack-seam-memlevel off|on] \
             [--pack-seam-reduction off|on] [--max-pixels N] [--threads N] \
             [--parallel-merge-order balanced|forward|reverse|shuffle:SEED]  ({LABEL})"
        );
        return ExitCode::from(2);
    }
    let parallelism = match Parallelism::new(threads) {
        Ok(value) => value.with_merge_order(merge_order),
        Err(error) => return report_error(&error),
    };
    // Apply the pixel-ceiling override (`--max-pixels`) once, before any
    // decode. It is a HARD admission bound checked at IHDR (before inflation/
    // allocation) by every decode in the process — source admission and the
    // pipeline's own self-verification/pack re-decodes alike. Omission leaves
    // the 64 Mi-pixel default (png::MAX_PIXELS). A user raising it above their
    // RAM owns that: the no-OOM guarantee holds at or below the active ceiling.
    if let Some(limit) = max_pixels {
        prism_quant::png::set_max_pixels(limit);
    }
    let input_path = Path::new(positional[0]);
    let output_path = Path::new(positional[1]);
    // Snapshot the source before candidate generation. More importantly, the
    // library writes only to a fresh sibling path, so an identical path, a
    // hardlink, or a symlink can never let candidate publication truncate the
    // source before the never-worse decision is made.
    let input_bytes = match prism_quant::png::read_png_file(input_path) {
        Ok(bytes) => bytes,
        Err(err) => return report_error(&err),
    };
    let staged_output = match StagedOutput::create(output_path) {
        Ok(staged) => staged,
        Err(err) => return report_error(&err),
    };
    let summary = match quantize_png_bytes_with_parallelism(
        input_path,
        &input_bytes,
        staged_output.path(),
        colors,
        &hidden_rgb_policy,
        &color_space,
        adaptive_default,
        dither,
        dither_strength,
        dither_strength_explicit,
        &dither_policy,
        &pack_mode,
        &pack_search,
        pack_seam_palette_sort,
        pack_seam_memlevel,
        pack_seam_reduction,
        parallelism,
    ) {
        Ok(summary) => summary,
        Err(err) => {
            return report_error(&err);
        }
    };
    // Never-worse output guarantee (T-0210, item 1): select the final bytes in
    // the sibling staging file before one atomic destination replacement.
    // Implemented ONCE here at the CLI layer (the parity rule): the library
    // `quantize_png*` functions are unchanged, so encoded bytes and library
    // publication semantics remain byte-for-byte untouched.
    let mut final_output_bytes = summary.output_bytes;
    let mut never_worse_fallback = false;
    if summary.output_bytes >= summary.source_bytes {
        if let Err(err) = std::fs::write(staged_output.path(), &input_bytes) {
            return report_error(&Error::new(
                Kind::Io,
                format!("io_error: cannot write {}: {err}", output_path.display()),
            ));
        }
        final_output_bytes = input_bytes.len();
        never_worse_fallback = true;
    }
    if let Err(err) = staged_output.publish(output_path) {
        return report_error(&err);
    }
    let candidate = if never_worse_fallback {
        "input-verbatim"
    } else {
        "encoded"
    };
    if report_json {
        // Compact, stable-key-order JSON matching Python's
        // json.dumps(separators=(",", ":")). Deliberately omits the version
        // string so the two impls' reports are byte-identical despite their
        // pinned-version drift (crate 0.1.0 / pipeline v0.2 vs oracle v0.3).
        // `palette_size` is the engine candidate's palette even on a
        // never-worse fallback (the candidate was built, then discarded).
        println!(
            "{{\"schema_version\":\"prism.cli.report/1\",\
             \"bytes_in\":{bytes_in},\
             \"bytes_out\":{bytes_out},\
             \"palette_size\":{palette},\
             \"candidate\":\"{candidate}\",\
             \"guard\":\"{guard}\",\
             \"never_worse_fallback\":{fallback}}}",
            bytes_in = summary.source_bytes,
            bytes_out = final_output_bytes,
            palette = summary.palette_entries,
            candidate = candidate,
            guard = adaptive_default.as_str(),
            fallback = never_worse_fallback,
        );
    } else {
        println!(
            "pngprism {version}: {in_bytes} -> {out_bytes} bytes, \
             {palette} palette entries ({alpha})",
            version = summary.version,
            in_bytes = summary.source_bytes,
            out_bytes = final_output_bytes,
            palette = summary.palette_entries,
            alpha = summary.stages.alpha_note,
        );
        if never_worse_fallback {
            println!(
                "never-worse: encoded output ({encoded} bytes) >= input ({input} bytes); \
                 emitted input verbatim",
                encoded = summary.output_bytes,
                input = summary.source_bytes,
            );
        }
    }
    ExitCode::from(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kind_exit_code_mapping_is_exhaustive() {
        let mappings = [
            (Kind::Io, 5),
            (Kind::Data, 3),
            (Kind::Internal, 70),
            (Kind::Usage, 2),
        ];

        for (kind, expected) in mappings {
            assert_eq!(exit_code(kind), expected, "wrong exit code for {kind:?}");
        }
    }

    #[test]
    fn merge_order_parser_is_explicit_and_deterministic() {
        assert_eq!(parse_merge_order("balanced"), Some(MergeOrder::Balanced));
        assert_eq!(parse_merge_order("forward"), Some(MergeOrder::Forward));
        assert_eq!(parse_merge_order("reverse"), Some(MergeOrder::Reverse));
        assert_eq!(
            parse_merge_order("shuffle:123"),
            Some(MergeOrder::Shuffled(123))
        );
        assert_eq!(parse_merge_order("shuffle:"), None);
        assert_eq!(parse_merge_order("random"), None);
    }

    #[test]
    fn python_integer_parser_accepts_every_unicode_16_decimal_block() {
        assert_eq!(UNICODE_16_DECIMAL_ZEROES.len(), 76);
        for zero in UNICODE_16_DECIMAL_ZEROES {
            let zero = char::from_u32(zero).expect("valid Unicode scalar");
            let nine = char::from_u32(u32::from(zero) + 9).expect("valid Unicode scalar");
            assert_eq!(python_int(&format!("+{zero}_{nine}")), Some(9));
        }
    }

    #[test]
    fn python_integer_parser_matches_decimal_syntax_and_overflow_classes() {
        assert_eq!(python_int(" \t+1_2\x0c"), Some(12));
        assert_eq!(python_int("١٢"), Some(12));
        assert_eq!(python_int("１２"), Some(12));
        assert_eq!(python_int("𝟘𝟡"), Some(9));
        assert_eq!(python_int("1_٢"), Some(12));
        assert_eq!(python_int("²"), None);
        assert_eq!(python_int("Ⅻ"), None);
        assert_eq!(python_int("一"), None);
        assert_eq!(python_int("١__٢"), None);
        assert_eq!(python_int("١_"), None);
        assert_eq!(python_int("_١"), None);
        assert_eq!(python_int("999999999999999999999999"), Some(i64::MAX));
        assert_eq!(python_int("-999999999999999999999999"), Some(i64::MIN));
    }

    #[cfg(unix)]
    #[test]
    fn staged_output_applies_confidential_mode_before_candidate_write() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("pngprism-staged-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("create temp dir");
        let destination = dir.join("output.png");
        std::fs::write(&destination, b"old").expect("create destination");
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o400))
            .expect("set destination mode");

        let staged = StagedOutput::create(&destination).expect("create staged output");
        let staged_mode = std::fs::metadata(staged.path())
            .expect("staged metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(staged_mode, 0o600, "owner-write is the only added bit");
        drop(staged);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }
}

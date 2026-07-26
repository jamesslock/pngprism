//! Shared resolution of the 47-image M1 smoke set for the integration suites.
//!
//! One resolver, four consumers (`cli.rs`, `quant_binding.rs`, `png_corpus.rs`,
//! `never_worse.rs`). Each of those used to carry its own `repo_root()` +
//! manifest parser; with availability now varying by *file* (ADR-0033 §2,
//! escape 7) that duplication would be four places for the same rule to drift.
//!
//! **The rule.** Resolution order for every row is:
//!
//! 1. The **in-tree original** at its repo-relative manifest path, when this
//!    crate is checked out inside the Prism research tree — so in-tree runs
//!    exercise all 47 files exactly as before and no accepted evidence moves.
//! 2. The **vendored copy** at `tests/smoke/<id>.png`, for the 24 rows whose
//!    licenses permit redistribution (`tests/smoke/README.md`).
//! 3. **Unavailable** — the row is skipped and *counted*. A suite that silently
//!    tested half of what its name implies is exactly the claims defect this
//!    program exists to avoid (ADR-0025), so callers report the count.

#![allow(dead_code)] // each test binary compiles this module and uses a subset

use std::path::{Path, PathBuf};

/// One row of `tests/smoke_manifest.tsv` (the pinned extract of
/// `benchmarks/m1-smoke-set/manifest.json`).
pub struct SmokeRow {
    pub id: String,
    /// Repo-relative path to the in-tree original.
    pub path: String,
    pub color_type: String,
    pub bit_depth: u8,
    pub interlaced: bool,
    pub plte: bool,
    pub trns: bool,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
}

/// The manifest covers this many files; asserted so a truncated manifest is a
/// failure rather than a quietly smaller test run.
pub const SMOKE_SET_SIZE: usize = 47;

/// The rows carrying an explicit redistribution grant (CC0), vendored under
/// `tests/smoke/`. Kept as a literal list, not derived from the path prefix, so
/// that adding a row under a CC0-looking directory does not silently enrol it:
/// vendoring is a licensing decision and this is where it is recorded.
pub const VENDORED_IDS: [&str; 24] = [
    "kenney-flag-1bit",
    "kenney-flag-2bit",
    "kenney-flag-4bit",
    "kenney-lightmask-1bit-binary",
    "kenney-lightmask-256level",
    "kenney-lightmask-4bit-9level",
    "kenney-lightmask-rgba-opaque",
    "kenney-miniforest-isometric",
    "kenney-platformer-preview-large",
    "kenney-platformer-sprite",
    "kenney-retro-texture-cutout",
    "kenney-retro-texture-opaque",
    "syn-aa-circle-subpixel",
    "syn-aa-glyph-strokes",
    "syn-alpha-ramp-hue-radial-64",
    "syn-alpha-ramp-linear-8",
    "syn-dither-shallow-alpha",
    "syn-hidden-rgb-edge-extended",
    "syn-hidden-rgb-random",
    "syn-matte-white-dark",
    "syn-palette-few-colors-many-alpha",
    "syn-shadow-inner-glow",
    "syn-shadow-low-alpha-tail",
    "syn-thin-hairlines",
];

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The repo root, when this crate is checked out at
/// `research/project-prism/lib/prism-quant` inside it. `None` anywhere else —
/// the four ancestors still *exist* as a path, they just don't mean anything,
/// which is why every caller goes through [`resolve`] rather than joining onto
/// this directly.
pub fn repo_root() -> Option<PathBuf> {
    let candidate = crate_dir().ancestors().nth(4)?;
    candidate
        .join("research/project-prism/lib/prism-quant/Cargo.toml")
        .is_file()
        .then(|| candidate.to_path_buf())
}

/// True when running inside the Prism research tree, where all 47 files and the
/// lab corpora are present.
pub fn in_research_tree() -> bool {
    repo_root().is_some()
}

pub fn vendored_dir() -> PathBuf {
    crate_dir().join("tests/smoke")
}

pub fn is_vendored(id: &str) -> bool {
    VENDORED_IDS.contains(&id)
}

/// Every row of the pinned manifest, in file order.
pub fn rows() -> Vec<SmokeRow> {
    let manifest_path = crate_dir().join("tests/smoke_manifest.tsv");
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", manifest_path.display()));
    let rows: Vec<SmokeRow> = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 10, "manifest row must have 10 columns: {line}");
            for flag in [cols[4], cols[5], cols[6]] {
                assert!(
                    flag == "true" || flag == "false",
                    "bad boolean {flag:?} in: {line}"
                );
            }
            SmokeRow {
                id: cols[0].to_string(),
                path: cols[1].to_string(),
                color_type: cols[2].to_string(),
                bit_depth: cols[3].parse().expect("bit_depth column is numeric"),
                interlaced: cols[4] == "true",
                plte: cols[5] == "true",
                trns: cols[6] == "true",
                width: cols[7].parse().expect("width column is numeric"),
                height: cols[8].parse().expect("height column is numeric"),
                sha256: cols[9].to_string(),
            }
        })
        .collect();
    assert_eq!(
        rows.len(),
        SMOKE_SET_SIZE,
        "the smoke manifest must cover {SMOKE_SET_SIZE} files"
    );
    rows
}

/// The readable file for a row, or `None` when it is one of the 23 lab-only
/// images and we are outside the research tree. See the module docs for the
/// order and why the in-tree original wins.
pub fn resolve(row: &SmokeRow) -> Option<PathBuf> {
    if let Some(root) = repo_root() {
        let in_tree = root.join(&row.path);
        if in_tree.is_file() {
            return Some(in_tree);
        }
    }
    let vendored = vendored_dir().join(format!("{}.png", row.id));
    vendored.is_file().then_some(vendored)
}

/// The file for one named row. Panics if the id is not in the manifest at all
/// (a typo in a test), returns `None` only for a genuinely unavailable image.
pub fn resolve_id(id: &str) -> Option<PathBuf> {
    let rows = rows();
    let row = rows
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("no smoke row with id {id:?}"));
    resolve(row)
}

/// Every row that can actually be read here, paired with its file.
pub fn available() -> Vec<(SmokeRow, PathBuf)> {
    let found: Vec<(SmokeRow, PathBuf)> = rows()
        .into_iter()
        .filter_map(|row| resolve(&row).map(|path| (row, path)))
        .collect();
    assert!(
        found.len() >= VENDORED_IDS.len(),
        "only {} smoke images resolved; the {} vendored copies under tests/smoke/ \
         must always be available — check that directory rather than the corpus",
        found.len(),
        VENDORED_IDS.len()
    );
    found
}

/// Print what a full-set iteration actually covered. Callers that iterate the
/// smoke set MUST call this: outside the research tree the run is 24/47, and
/// that difference has to be visible in the output, not inferred from a green
/// tick.
pub fn report_coverage(suite: &str, ran: usize) {
    if ran == SMOKE_SET_SIZE {
        return;
    }
    eprintln!(
        "{suite}: ran {ran}/{SMOKE_SET_SIZE} smoke images. The other \
         {} are lab-only (no redistribution grant) and are not shipped with \
         this crate — see tests/smoke/README.md.",
        SMOKE_SET_SIZE - ran
    );
}

/// Announce a test skipped for want of a lab-only image, so the reason is in the
/// output rather than looking like the test passed on it.
pub fn skip_lab_only(test: &str, id: &str) {
    eprintln!(
        "{test}: SKIPPED — needs the lab-only image {id:?}, which carries no \
         redistribution grant and is not shipped with this crate \
         (tests/smoke/README.md)."
    );
}

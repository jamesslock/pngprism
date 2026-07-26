//! Integrity of the vendored smoke-set subset (`tests/smoke/`).
//!
//! The other suites *use* those 24 images; this one proves they are the images
//! they claim to be. Same shape as `oracle_pins.rs`, for the same reason: a
//! vendored copy is a fork waiting to happen unless something checks it.
//!
//! 1. **Always** — the directory holds exactly the expected ids, and every
//!    file's SHA-256 equals its `smoke_manifest.tsv` pin.
//! 2. **In the research tree** — each in-tree original also matches that same
//!    pin, which is what makes copy and original provably identical without
//!    comparing them to each other.

#[path = "common/smoke.rs"]
mod smoke;

/// How many smoke images the suites actually got, asserted rather than printed.
///
/// The other suites `eprintln!` their coverage, which `cargo test` swallows on
/// a passing run — so on its own that note is invisible exactly when someone is
/// trusting the green tick. This makes the count a checked claim: 47 in the
/// research tree, 24 outside it, and a mismatch is a failure naming both
/// numbers. (The printed lines are still worth having under `--nocapture`.)
#[test]
fn available_image_count_matches_the_environment() {
    let available = smoke::available().len();
    let (expected, where_) = if smoke::in_research_tree() {
        (smoke::SMOKE_SET_SIZE, "inside the research tree")
    } else {
        (smoke::VENDORED_IDS.len(), "outside the research tree")
    };
    assert_eq!(
        available, expected,
        "{where_}, {expected} smoke images should resolve but {available} did"
    );
}

/// The vendored directory is exactly `VENDORED_IDS` — no missing file (a suite
/// would skip and quietly lose coverage) and no extra one (an image shipped
/// without a recorded licensing decision).
#[test]
fn vendored_directory_holds_exactly_the_licensed_subset() {
    let dir = smoke::vendored_dir();
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("directory entry readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .map(|path| {
            path.file_stem()
                .expect("file stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();

    let mut expected: Vec<String> = smoke::VENDORED_IDS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    expected.sort();

    assert_eq!(
        found, expected,
        "tests/smoke/ must contain exactly the ids in VENDORED_IDS — adding an \
         image here is a licensing decision and belongs in that list and in \
         tests/smoke/README.md"
    );
}

#[test]
fn vendored_copies_match_their_manifest_digests() {
    for row in smoke::rows() {
        if !smoke::is_vendored(&row.id) {
            continue;
        }
        let path = smoke::vendored_dir().join(format!("{}.png", row.id));
        let bytes =
            std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        assert_eq!(
            pngprism::sha256::hex(&bytes),
            row.sha256,
            "{}: the vendored copy drifted from its smoke_manifest.tsv pin",
            row.id
        );
    }
}

/// In-tree, the originals must match the same pins. Together with the check
/// above this establishes copy == original transitively, and it catches corpus
/// drift under `datasets/` and `benchmarks/` that would otherwise only surface
/// as a confusing property mismatch elsewhere.
#[test]
fn in_tree_originals_match_the_same_digests() {
    if !smoke::in_research_tree() {
        eprintln!(
            "smoke_vendor: SKIPPED the in-tree original check — no lab checkout \
             found. The vendored-copy pins still ran. Set PRISM_REQUIRE_LAB=1 to \
             make this a failure instead."
        );
        return;
    }
    let mut checked = 0;
    for row in smoke::rows() {
        let path = smoke::resolve_recorded(&row.path).expect("lab checkout, just confirmed");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("{}: read {}: {err}", row.id, path.display()));
        assert_eq!(
            pngprism::sha256::hex(&bytes),
            row.sha256,
            "{}: the in-tree corpus file drifted from its smoke_manifest.tsv pin",
            row.id
        );
        checked += 1;
    }
    assert_eq!(checked, smoke::SMOKE_SET_SIZE);
}

//! Integrity of the vendored Python oracle (`tests/oracle/`).
//!
//! `tests/caps.rs` asserts the Rust port agrees with the Python oracle. That is
//! only meaningful if the oracle it runs is *the* oracle — so this suite pins
//! the vendored copies, in the two ways that can actually go wrong:
//!
//! 1. **Someone edits the vendored copy.** Each file's SHA-256 must equal its
//!    frozen pin.
//! 2. **The lab oracle moves and the vendored copy doesn't.** Inside the
//!    research tree, each file must be byte-identical to its `lab/reference/`
//!    original. This is the anti-fork guard (ADR-0025's parity rule, ADR-0033
//!    §2): drift between the lab oracle and the shipped oracle becomes a test
//!    failure the moment it is introduced, rather than a silent divergence
//!    discovered after the repo split.
//!
//! Check 2 skips outside the research tree — a consumer holding only the crate
//! has nothing to compare against, and check 1 still fully protects them.
//! See `tests/oracle/README.md` for why only four of the twelve modules are
//! vendored.

use std::path::{Path, PathBuf};

/// `(filename, sha256)` — update in the SAME commit that re-copies the file.
const ORACLE_PINS: [(&str, &str); 4] = [
    (
        "prism_quant.py",
        "5525f72702f31054c0343562b3c40c415ff13097d21b08ebe83acdaa2fdcb5af",
    ),
    (
        "prism_dither.py",
        "cf1bb02b24b4d4acfcbffd2d6c6927c1c44c6e122f9065262c201bdc195ec8aa",
    ),
    (
        "prism_pack.py",
        "46bae1168cd698bcf3cad75062c6a0be623b1f59da25583e7b5a40bedbef32f5",
    ),
    (
        "m1_png.py",
        "ad57130b51cdf3427e9520219bc591a03bc738575b386c8c6825515a6270c26c",
    ),
];

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle")
}

/// The lab's original, when this crate is checked out inside the research tree.
fn lab_reference_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/reference");
    dir.is_dir().then_some(dir)
}

#[test]
fn vendored_oracle_matches_its_frozen_pins() {
    for (name, expected) in ORACLE_PINS {
        let path = oracle_dir().join(name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read vendored oracle {}: {e}", path.display()));
        let actual = pngprism::sha256::hex(&bytes);
        assert_eq!(
            actual, expected,
            "{name} drifted from its frozen pin — if this edit is intended, \
             change it in lab/reference/ first, re-copy, and update ORACLE_PINS"
        );
    }
}

#[test]
fn vendored_oracle_is_byte_identical_to_the_lab_original() {
    let Some(lab) = lab_reference_dir() else {
        eprintln!(
            "oracle_pins: SKIPPED the anti-fork check — lab/reference/ is not \
             present (crate is outside the research tree). The frozen-pin check \
             still ran."
        );
        return;
    };
    for (name, _) in ORACLE_PINS {
        let vendored = std::fs::read(oracle_dir().join(name)).expect("read vendored oracle");
        let original =
            std::fs::read(lab.join(name)).unwrap_or_else(|e| panic!("read lab oracle {name}: {e}"));
        assert!(
            vendored == original,
            "{name}: the vendored oracle has FORKED from lab/reference/{name}. \
             The parity suite would be validating a copy, not the oracle. \
             Re-copy the file and update ORACLE_PINS."
        );
    }
}

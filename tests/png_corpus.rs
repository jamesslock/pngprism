//! Corpus integration tests, ported from `lab/reference/test_m1_png.py`'s
//! `SmokeSetTests` (driven by the pinned `tests/smoke_manifest.tsv` extract
//! of `benchmarks/m1-smoke-set/manifest.json`), `PngSuitePairTests` (the
//! basi/basn Adam7 oracle pairs), and the external-verifier writer test.
//! The fixture corpora under `benchmarks/` and `datasets/` are read-only
//! inputs; the only write is the verifier temp file (fresh temp dir,
//! removed afterwards).

use prism_quant::png::{decode_png, write_indexed_png};
use std::path::{Path, PathBuf};

#[path = "common/smoke.rs"]
mod smoke;

use smoke::SmokeRow;

/// The oracle test module's `COLOR_TYPE_NAMES`.
fn color_type_name(color_type: u8) -> &'static str {
    match color_type {
        0 => "gray",
        2 => "rgb",
        3 => "palette",
        4 => "gray+alpha",
        6 => "rgba",
        other => panic!("unexpected color type {other}"),
    }
}

/// Port of `SmokeSetTests.test_manifest_covers_47_files` +
/// `test_properties_match_manifest`: every available smoke-set file decodes and
/// matches its manifest properties (width, height, bit depth, color-type name,
/// interlaced flag, PLTE/tRNS presence), with `pixels.len() == w * h`.
///
/// The manifest must still cover all 47 rows — that assertion lives in
/// `smoke::rows()` and is not weakened by availability. What varies is how many
/// of those rows have a readable file here: 47 in the research tree, 24 outside
/// it, reported either way.
#[test]
fn smoke_set_matches_manifest() {
    let available = smoke::available();
    for (row, path) in &available {
        let id = row.id.as_str();
        let raw = std::fs::read(path)
            .unwrap_or_else(|err| panic!("{id}: read {}: {err}", path.display()));
        let image = decode_png(&raw).unwrap_or_else(|err| panic!("{id}: decode failed: {err}"));
        assert_eq!(image.width, row.width, "{id}: width");
        assert_eq!(image.height, row.height, "{id}: height");
        assert_eq!(
            image.pixels.len(),
            row.width as usize * row.height as usize,
            "{id}: pixel count"
        );
        assert_eq!(image.properties.bit_depth, row.bit_depth, "{id}: bit_depth");
        assert_eq!(
            color_type_name(image.properties.color_type),
            row.color_type,
            "{id}: color_type"
        );
        assert_eq!(
            image.properties.interlaced, row.interlaced,
            "{id}: interlaced"
        );
        assert_eq!(
            image.properties.plte.is_some(),
            row.plte,
            "{id}: plte presence"
        );
        assert_eq!(
            image.properties.trns.is_some(),
            row.trns,
            "{id}: trns presence"
        );
    }
    smoke::report_coverage("smoke_set_matches_manifest", available.len());
}

/// Port of `SmokeSetTests.test_interlaced_files_decode_deterministically`:
/// the smoke set's two interlaced files decode to identical pixels and
/// properties on a repeated decode.
///
/// Both interlaced images are libpng-legacy files with no redistribution grant,
/// so outside the research tree this test has nothing to run on and skips. The
/// Adam7 decode path is not left unexercised by that: `pngsuite_basi_matches_basn`
/// below drives it over the vendored PngSuite `basi*` set.
#[test]
fn interlaced_files_decode_deterministically() {
    let rows = smoke::rows();
    let interlaced: Vec<&SmokeRow> = rows.iter().filter(|row| row.interlaced).collect();
    let mut ids: Vec<&str> = interlaced.iter().map(|row| row.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["legacy-8passes-interlaced", "legacy-magnolia-interlaced"]
    );
    for row in interlaced {
        let Some(path) = smoke::resolve(row) else {
            smoke::skip_lab_only("interlaced_files_decode_deterministically", &row.id);
            continue;
        };
        let raw = std::fs::read(&path)
            .unwrap_or_else(|err| panic!("{}: read {}: {err}", row.id, path.display()));
        let first = decode_png(&raw).unwrap_or_else(|err| panic!("{}: {err}", row.id));
        let second = decode_png(&raw).unwrap_or_else(|err| panic!("{}: {err}", row.id));
        assert_eq!(first.pixels, second.pixels, "{}: pixels", row.id);
        assert_eq!(
            first.properties, second.properties,
            "{}: properties",
            row.id
        );
    }
}

fn pngsuite_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pngsuite")
}

/// `sorted(PNGSUITE_DIR.glob("basi*.png"))` — sorted by file name.
fn glob_prefixed_pngs(prefix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(pngsuite_dir())
        .expect("PngSuite corpus directory exists")
        .map(|entry| entry.expect("directory entry readable").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".png"))
        })
        .collect();
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    paths
}

/// Port of `PngSuitePairTests.test_pair_inventory`: exactly 15 basi files,
/// each with its basn twin.
#[test]
fn pngsuite_pair_inventory() {
    let basi_paths = glob_prefixed_pngs("basi");
    assert_eq!(basi_paths.len(), 15);
    for basi in &basi_paths {
        let name = basi
            .file_name()
            .expect("file name")
            .to_str()
            .expect("utf-8 name");
        let twin = basi.with_file_name(format!("basn{}", &name[4..]));
        assert!(twin.is_file(), "missing basn twin for {name}");
    }
}

/// Port of `PngSuitePairTests.test_interlaced_matches_non_interlaced_twin`:
/// the Adam7 correctness proof — each interlaced basi file decodes to the
/// same dimensions and pixels as its non-interlaced basn twin.
#[test]
fn pngsuite_interlaced_matches_non_interlaced_twin() {
    for basi in glob_prefixed_pngs("basi") {
        let name = basi
            .file_name()
            .expect("file name")
            .to_str()
            .expect("utf-8 name");
        let basn = basi.with_file_name(format!("basn{}", &name[4..]));
        let interlaced =
            decode_png(&std::fs::read(&basi).expect("read basi")).expect("decode basi");
        let plain = decode_png(&std::fs::read(&basn).expect("read basn")).expect("decode basn");
        assert!(interlaced.properties.interlaced, "{name}: interlaced flag");
        assert!(!plain.properties.interlaced, "{name}: twin interlaced flag");
        assert_eq!(
            (interlaced.width, interlaced.height),
            (plain.width, plain.height),
            "{name}: dimensions"
        );
        assert_eq!(interlaced.pixels, plain.pixels, "{name}: pixels");
    }
}

/// Port of `PngSuitePairTests.test_basn_names_match_decoded_format`: the
/// basn name encoding (`BASN_FORMAT_BY_KIND` + bit depth) matches the
/// decoded format, and every basn file is non-interlaced.
#[test]
fn pngsuite_basn_names_match_decoded_format() {
    let basn_paths = glob_prefixed_pngs("basn");
    assert_eq!(basn_paths.len(), 15);
    for basn in &basn_paths {
        let name = basn
            .file_name()
            .expect("file name")
            .to_str()
            .expect("utf-8 name");
        let expected_color_type = match &name[4..6] {
            "0g" => 0,
            "2c" => 2,
            "3p" => 3,
            "4a" => 4,
            "6a" => 6,
            kind => panic!("unexpected basn kind {kind:?} in {name}"),
        };
        let expected_bit_depth: u8 = name[6..8].parse().expect("bit depth in name");
        let image = decode_png(&std::fs::read(basn).expect("read basn")).expect("decode basn");
        assert_eq!(
            image.properties.color_type, expected_color_type,
            "{name}: color_type"
        );
        assert_eq!(
            image.properties.bit_depth, expected_bit_depth,
            "{name}: bit_depth"
        );
        assert!(!image.properties.interlaced, "{name}: interlaced");
    }
}

/// Port of `IndexedWriterTests.test_external_verifier_accepts_output`: the
/// writer's output is accepted by the vendored stdlib verifier script
/// (`tests/tools/verify-indexed-png.py`).
#[test]
fn external_verifier_accepts_writer_output() {
    let palette: Vec<(u8, u8, u8, u8)> = (0..256u32)
        .step_by(17)
        // x <= 255, so every channel expression fits u8 exactly.
        .map(|x| (x as u8, (255 - x) as u8, ((x * 3) % 256) as u8, x as u8))
        .collect();
    let (width, height) = (17u32, 11u32);
    let palette_len = palette.len();
    let indices: Vec<u8> = (0..height as usize)
        .flat_map(|y| {
            (0..width as usize).map(move |x| ((x + y * width as usize) % palette_len) as u8)
        })
        .collect();
    let blob = write_indexed_png(width, height, &palette, &indices)
        .expect("writer accepts valid arguments");

    let dir = std::env::temp_dir().join(format!("prism-quant-png-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fresh temp dir");
    let file = dir.join("indexed.png");
    std::fs::write(&file, &blob).expect("write temp png");
    // Vendored to tests/tools/ (ADR-0033 §2): despite living under the
    // libimagequant baseline directory in the lab, this is Prism's own
    // stdlib-only PNG chunk validator — it contains no libimagequant code and
    // carries no GPL obligation.
    let verifier = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tools/verify-indexed-png.py");
    let output = std::process::Command::new("python3")
        .arg(&verifier)
        .arg(&file)
        .output()
        .expect("run python3 verify-indexed-png.py");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "verify-indexed-png.py rejected the writer output: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

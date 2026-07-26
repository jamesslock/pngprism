//! Typed-error taxonomy and oracle-verbatim message regressions.

use prism_quant::dither;
use prism_quant::pack;
use prism_quant::png::{self, DecodedImage, Properties};
use prism_quant::quant;
use prism_quant::{Kind, Rgba};
use std::path::Path;

fn tiny_source() -> DecodedImage {
    DecodedImage {
        width: 1,
        height: 1,
        pixels: vec![(0, 0, 0, 255)],
        properties: Properties {
            color_type: 6,
            bit_depth: 8,
            interlaced: false,
            plte: None,
            trns: None,
            gama: None,
            iccp: None,
            conversions: Vec::new(),
        },
    }
}

#[test]
fn png_failure_is_data_with_verbatim_message() {
    let error = png::decode_png(b"").expect_err("empty input must fail");

    assert_eq!(
        (error.kind(), error.message()),
        (Kind::Data, "missing PNG signature")
    );
}

#[test]
fn dither_failure_is_data_with_verbatim_message() {
    let pixels: [Rgba; 1] = [(1, 2, 3, 0)];
    let opaque_palette: [Rgba; 1] = [(0, 0, 0, 255)];
    let error = dither::nearest_remap(&pixels, 1, 1, &opaque_palette)
        .expect_err("a missing alpha zone must fail");

    assert_eq!(
        (error.kind(), error.message()),
        (
            Kind::Data,
            "palette has no entry in the source pixel's alpha zone"
        )
    );
}

#[test]
fn strength_failure_is_usage_with_verbatim_message() {
    let error = dither::parse_dither_strength("nope").expect_err("garbage strength must fail");

    assert_eq!(
        (error.kind(), error.message()),
        (
            Kind::Usage,
            "usage_error: --dither-strength must be a decimal in 0..1"
        )
    );
}

#[test]
fn pack_failure_is_data_with_verbatim_message() {
    let error = pack::pack_indexed_png(0, 1, &[(0, 0, 0, 255)], &[0], "fast", "v1")
        .expect_err("zero width must fail");

    assert_eq!(
        (error.kind(), error.message()),
        (Kind::Data, "width and height must be integers >= 1")
    );
}

#[test]
fn quant_failure_is_data_with_verbatim_message() {
    let error = quant::quantize_candidate(&tiny_source(), 0, quant::DEFAULT_HIDDEN_RGB_POLICY)
        .expect_err("zero colors must fail");

    assert_eq!(
        (error.kind(), error.message()),
        (Kind::Data, "colors must be in 1..=256")
    );
}

#[test]
fn quant_io_failure_preserves_context_message() {
    let input = Path::new("/nonexistent/path/that/should/not/exist/prism-error-kind.png");
    let output = Path::new("/tmp/prism-error-kind-unused.png");
    let source_message = std::fs::read(input)
        .expect_err("fixture path must not exist")
        .to_string();
    let error = quant::quantize_png(
        input,
        output,
        quant::DEFAULT_COLORS,
        quant::DEFAULT_HIDDEN_RGB_POLICY,
        quant::DEFAULT_DITHER,
        quant::DEFAULT_DITHER_STRENGTH,
        quant::DEFAULT_DITHER_POLICY,
        quant::DEFAULT_PACK_MODE,
        quant::DEFAULT_PACK_SEARCH,
    )
    .expect_err("missing input must fail");
    let expected = format!(
        "io_error: cannot read {}: {source_message}",
        input.display()
    );

    assert_eq!(
        (error.kind(), error.message()),
        (Kind::Io, expected.as_str())
    );
}

#[test]
fn adaptive_default_library_composition_failure_is_data() {
    let error = quant::quantize_png_with_adaptive_default(
        Path::new("unused-input.png"),
        Path::new("unused-output.png"),
        quant::DEFAULT_COLORS,
        quant::DEFAULT_HIDDEN_RGB_POLICY,
        quant::DEFAULT_COLOR_SPACE,
        true,
        true,
        quant::DEFAULT_DITHER_STRENGTH,
        false,
        quant::DEFAULT_DITHER_POLICY,
        quant::DEFAULT_PACK_MODE,
        quant::DEFAULT_PACK_SEARCH,
    )
    .expect_err("adaptive default must reject explicit dither state");

    assert_eq!(
        (error.kind(), error.message()),
        (
            Kind::Data,
            "--adaptive-default on is not composable with explicit dither options"
        )
    );
}

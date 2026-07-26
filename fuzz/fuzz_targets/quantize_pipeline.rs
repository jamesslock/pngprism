#![no_main]
//! T-0207 fuzz target B: the full in-memory pipeline past the decoder —
//! `decode_png` -> `quant::quantize_candidate` (palette init + refinement +
//! remap, exercising `quant` and `dither`) -> `png::write_indexed_png` (the
//! deterministic indexed-PNG writer + zlib emit) -> `decode_png` of the
//! writer's own output (a round-trip that must itself never panic).
//!
//! Where target A hammers the decoder, target B proves the *downstream* seams
//! uphold the same ch17 §31 no-panic / no-OOB contract when fed whatever
//! structurally-valid `DecodedImage` a mutated PNG happens to decode to
//! (unusual palettes, all-transparent images, 1xN strips, 16-bit sources,
//! interlaced reassembly, ...).
//!
//! Two harness-only guards (NOT library changes) keep each exec fast and
//! bounded so libFuzzer's own `-timeout`/`-rss_limit_mb` flags flag genuine
//! regressions rather than a legitimately huge synthesized image:
//!   * only quantize images within a modest pixel budget, and
//!   * derive the colour count from the input bytes across the valid 1..=256
//!     range (structure-aware coverage of the palette-capacity code paths).

use libfuzzer_sys::fuzz_target;
use prism_quant::png;
use prism_quant::quant;

/// Cap on pixels we quantize per exec. Decoding is already bounded; this only
/// stops a legitimately large decoded image from dominating the fuzz budget.
const MAX_PIXELS: usize = 1 << 16; // 65_536

fuzz_target!(|data: &[u8]| {
    let Ok(image) = png::decode_png(data) else {
        return; // malformed input: target A owns the decoder contract.
    };
    if image.pixels.len() > MAX_PIXELS {
        return;
    }

    // Structure-aware colour count in the valid inclusive range 1..=256,
    // steered by the input so the mutator explores palette capacities.
    let seed = data.first().copied().unwrap_or(0);
    let colors = i64::from(seed) % quant::MAX_COLORS + 1; // 1..=256

    let Ok((palette, indices, _notes)) =
        quant::quantize_candidate(&image, colors, quant::DEFAULT_HIDDEN_RGB_POLICY)
    else {
        return;
    };

    // Emit and re-decode: the writer + zlib emit + a second decode pass must
    // all uphold the no-panic contract on quantizer-produced data.
    if let Ok(encoded) = png::write_indexed_png(image.width, image.height, &palette, &indices) {
        let _ = png::decode_png(&encoded);
    }
});

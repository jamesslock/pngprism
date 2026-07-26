#![no_main]
//! T-0207 fuzz target A: the primary attack surface — `png::decode_png`, the
//! arbitrary-PNG -> canonical RGBA8 decoder. Every byte of a caller-supplied
//! PNG flows through here: signature check, chunk framing + CRC, IHDR
//! validation, PLTE/tRNS/gAMA/iCCP parsing, the concatenated-IDAT inflate
//! seam (bounded 8 MiB scratch), Adam7 pass geometry, the defilter loop, and
//! the sample -> RGBA8 conversion.
//!
//! The contract under test (ch17 §31 / T-0110 / T-0201): for ANY input,
//! `decode_png` returns `Ok` or a typed `Err` — never a panic, an abort, or
//! an out-of-bounds access. libFuzzer + AddressSanitizer turn any panic/abort
//! or OOB into a crash artifact. The bounded-scratch inflate + the
//! pixel-cap-before-allocation length check keep the giant-IHDR seeds
//! (`mut-giant-dims-*`) from ever allocating the claimed buffer, so RSS stays
//! bounded under mutation (verify with `-rss_limit_mb`).
//!
//! Structure-aware only: the seed corpus (`fuzz/corpus/decode_png/`) is built
//! by `fuzz/generate_seed_corpus.py` from real PNGs + chunk-level mutations,
//! and `fuzz/dictionaries/png.dict` supplies PNG's magic tokens — so the
//! mutator reaches deep decoder states instead of dying at the signature.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The result is intentionally discarded: we assert only the no-panic /
    // no-OOB / bounded-work contract. A returned `Err` is a correct outcome.
    let _ = prism_quant::png::decode_png(data);
});

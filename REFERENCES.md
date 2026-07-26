# pngprism (Rust crate) — derivation and dependency ledger

**Evidence label: derivation ledger, not a quality or novelty claim.**
This crate (`lib/prism-quant/` in the lab monorepo; the repository root when
published standalone) is a Rust port of the reviewed Python
oracles and inherits their method provenance; this file records (1)
the oracle source rows the port derives from and (2) the crate's external
dependencies with licenses and exact pins, per the T-0082 task card.

## 1. Oracle source (in-repo original work — no clean-room issue)

The port is a seam-by-seam translation of OUR OWN reviewed code. The
method-level derivation ledger for the algorithm itself lives with the
oracle and is NOT duplicated here:

| Ported component | Oracle file | Oracle provenance ledger |
| --- | --- | --- |
| Whole crate (six-seam pipeline, core algorithm, hidden-RGB policies, PLTE/tRNS emission) | `lab/reference/prism_quant.py` (pngprism-lab: `lab/reference/prism_quant.py`) v0.1.1-alpha (T-0067/T-0068, review-passed) | `lab/reference/REFERENCES.md` (pngprism-lab: `lab/reference/REFERENCES.md`) rows: six-stage frame; premultiplied distance; alpha ladder; farthest-point seeding; sparse pair instantiation; zoned joint Lloyd; hidden-RGB policies (Lloyd 1982 and Gonzalez 1985 public-paper lineages recorded there) |
| `src/png.rs` (decode + indexed writer) | `lab/reference/m1_png.py` (pngprism-lab: `lab/reference/m1_png.py`) | ledger rows: PNG decode to canonical RGBA8; deterministic indexed-PNG emission ([W3C PNG Specification, Third Edition](https://www.w3.org/TR/png-3/); independently authored standard-library implementation) |
| Oklab opt-in (`src/quant.rs`, `--color-space oklab`) | `lab/reference/prism_quant.py` (pngprism-lab: `lab/reference/prism_quant.py`) at accepted commit `1bc2c339` (T-0131); independently derived in E-0016 | Björn Ottosson, “A perceptual color space for image processing,” published 2020-12-23, matrices updated 2021-01-25, [primary post](https://bottosson.github.io/posts/oklab/) (accessed 2026-07-19). Used the published linear-sRGB/LMS/Oklab forward and inverse matrices and D65 transform. The displayed implementation is public domain or, alternatively, MIT licensed. The premultiplied-alpha extension is the in-repo E-0016 construction, not an Ottosson claim. |
| Luma-weighted blue-noise opt-in (`src/dither.rs`, `--dither-policy luma-bluenoise`) | `lab/reference/prism_dither.py` (pngprism-lab: `lab/reference/prism_dither.py`) at accepted commit `768ca92a` (T-0139); E-0017 committed masks | Robert Ulichney, “The Void-and-Cluster Method for Dither Array Generation,” *Human Vision, Visual Processing, and Digital Display IV*, Proc. SPIE 1913, 1993. Paper-level method source for the independently authored E-0017 mask generator; no third-party dither code was consulted. The luma-weighted chroma attenuation and premultiplied-alpha application are in-repo E-0017 constructions. |
| Mask byte verification (`src/sha256.rs`) | Python oracle's `hashlib.sha256` checks in `prism_dither.py` at `768ca92a` | NIST FIPS PUB 180-4, Secure Hash Standard (SHS), SHA-256. Independently implemented one-shot verifier; no new crate dependency. Pinned by standard vectors and the three committed E-0017 mask digests. |

Clean-room quarantine binds the port exactly as it bound the oracles: no
libimagequant source, no `book/12-*`. The port adds no methods beyond the
accepted in-repo oracle paths and no external algorithm sources beyond this
table and §2.

The committed E-0017 mask byte pins consumed at runtime are:

- R, seed 20260719: `8ee801878fd37cc52fbb2993fa4d7c5b4ace02f2fccc04a0c28dabf13111b0d8`
- G, seed 20260720: `80aba5e8dc5cbef7b1c04acfc3e3b0d6193375a74ef007cf8a26d604ae2522cc`
- B, seed 20260721: `cb2706b65c956f52369fd05ccb0c73fef52774c185cb39fffa7ff8dc79258139`

## 2. External crate dependencies

Direct dependencies:

| Crate | Version pin | License | Why it is needed |
| --- | --- | --- | --- |
| [`flate2`](https://crates.io/crates/flate2) | `=1.1.9` (Cargo.toml), `default-features = false, features = ["zlib"]` | MIT OR Apache-2.0 | zlib-format inflate (PNG decode) and deflate level 9 (PNG emit). The **`zlib` C-library backend is mandatory**: `write_indexed_png`/pack emission must emit bytes identical to Python's `zlib.compress(data, 9)`, and deflate output is backend-dependent. miniz_oxide is NOT linked. Byte-identity was spike-verified before adoption (empty/tiny/repetitive/random/1 MiB/real scanline payloads, all identical; T-0082 Evidence log). |
| [`libz-sys`](https://crates.io/crates/libz-sys) | `=1.1.20` (Cargo.toml), crate checksum `d2d16453e800a8cf6dd2fc3eb4bc99b786a9b90c663b8559a5b1a041bf89e472` (Cargo.lock), `default-features = false`, features `libc`, `static`, `stock-zlib` | MIT OR Apache-2.0 (binding); zlib license (bundled C source) | **Phase 2 (T-0095), static-vendored integration (T-0176).** Direct FFI (`deflateInit2_`/`deflateCopy`/`deflate`/`deflateEnd`) to the SAME C zlib `flate2` uses. Required for the `trial-zlib` row-filter heuristic (`prism_pack._trial_compression_row_filters`), which selects each PNG row's filter by the length of a stateful `zlib.compressobj(9)` `copy()` + `Z_SYNC_FLUSH` probe — `deflateCopy` semantics `flate2` does not expose. Version 1.1.20 bundles stock zlib **1.3.1**; `static` makes its build script bypass host discovery and compile that source. The bundled source files are byte-identical to the overlapping files in upstream `zlib-1.3.1.tar.gz`, pinned at SHA-256 `9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23` in E-0023. T-0151 proved single-call level-9 equivalence before integration; T-0176 closes the multi-call/copy/sync-flush pack-path gap and re-runs the complete parity surface. |

Transitive/runtime-resolved dependencies (from Cargo.lock, recorded for
completeness; none are used directly):

| Crate | Locked version | License | Role |
| --- | --- | --- | --- |
| [`libc`](https://crates.io/crates/libc) | 0.2.186 | MIT OR Apache-2.0 | libz-sys FFI type aliases (`z_stream` fields). |
| [`crc32fast`](https://crates.io/crates/crc32fast) | 1.5.0 | MIT OR Apache-2.0 | flate2 internal checksum. (The crate's own PNG chunk CRC-32 is hand-rolled in `src/png.rs`, pinned by golden vectors — no dependency.) |
| [`cfg-if`](https://crates.io/crates/cfg-if) | 1.0.4 | MIT OR Apache-2.0 | crc32fast build support. |

Build-time-only dependencies of libz-sys (`cc` 1.2.67, `pkg-config`
0.3.33, `vcpkg` 0.2.15, `find-msvc-tools` 0.1.9, `shlex` 2.0.1) support its
build script; they are not linked into the binary. With the required `static`
feature, the build script does not probe `pkg-config` or the host SDK for zlib:
it compiles the bundled stock zlib 1.3.1 C sources into the Rust artifact.

**External black-box tool (not a linked dependency):** maximum-mode packing
shells out to the pinned Apache-2.0 `zopflipng`
(`benchmarks/baselines/zopfli/work/zopfli/zopflipng`, commit
`ccf9f0588d4a4509cb1040310ec122243e670ee6`) — the SAME binary the oracle
invokes — as a subprocess. It is confirmed deterministic (identical output
bytes across repeated runs), so max-mode byte-parity reduces to feeding it
identical pre-optimizer bytes.

## 2.1 Dev-dependencies (never ship)

Admitted under ch17 §30's dev-dep clause (`book/17-prism-engine-architecture.md`
§30): "Dev-dependencies that never ship (benchmark/test harnesses) require
only (4)" — exact-version pin + committed lockfile + ledger row with
license and role. No null-hypothesis test, measured-need writeup, or
byte-parity spike applies (criterion touches no emit path and is never
linked into `prism-quant-rs`).

| Crate | Version pin | License | Why it is needed |
| --- | --- | --- | --- |
| [`criterion`](https://crates.io/crates/criterion) | `=0.8.2` (Cargo.toml `[dev-dependencies]`) | Apache-2.0 OR MIT | Stage-boundary micro-benchmarks (T-0111, `benches/stage_boundaries.rs`): statistically-sound wall-time sampling for `quantize_candidate`/`floyd_steinberg`/`pack_indexed_png` so later Thread-4 optimization tasks can cite a stage-level delta, not just a whole-pipeline number. `cargo bench` only; `[[bench]] harness = false` (criterion supplies its own runner). Default features (`cargo_bench_support`, `plotters`, `rayon`) accepted as-is — dev-only, never shipped, no effect on `prism-quant-rs`'s own single-threaded execution. |

Its transitive dependency tree (dozens of crates: `plotters`, `rayon`,
`serde_json`, `tinytemplate`, `regex`, etc.) is recorded in full in
`Cargo.lock`, the source of truth for the locked graph; not enumerated
here individually since none are direct or emit-path-adjacent — the
binding facts are the direct pin, the license, and the role, all above.

## 3. Standing rule

Any new external crate added in later phases must be pinned (`=x.y.z`),
locked, and ledgered here in the same commit series, with a byte-parity
justification if it touches the emit path.

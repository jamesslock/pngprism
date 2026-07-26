# pngprism fuzz harness (T-0207)

Structure-aware, coverage-guided fuzzing of the `prism-quant` PNG decoder and
its downstream quantize/emit pipeline, hardening the crate against malformed
and adversarial input for the v0.5 release (Theme E-16). Builds on the
accepted T-0201 edge gate and the T-0110 ch17 §31 no-panic contract.

## What is fuzzed

Two [`libfuzzer-sys`] targets (`fuzz_targets/`):

| target | surface |
| --- | --- |
| `decode_png` | the primary attack surface: `png::decode_png` (signature, chunk framing + CRC, IHDR validation, PLTE/tRNS/gAMA/iCCP, the bounded-scratch inflate seam, Adam7 geometry, defilter, sample→RGBA8) |
| `quantize_pipeline` | `decode_png` → `quant::quantize_candidate` → `png::write_indexed_png` → re-decode round-trip (the quant + dither + writer + zlib seams on decoder-produced images) |

The contract under test: for **any** input, each path returns `Ok` or a typed
`Err` — never a panic, an abort, or an out-of-bounds access. libFuzzer +
AddressSanitizer turn any violation into a crash artifact.

## Structure-aware, not random-byte

Random-byte fuzzing of a PNG decoder is low-value — nearly every random buffer
dies at the 8-byte signature check, so mutation never reaches the parser.
Instead:

- **Seed corpus** (`corpus/decode_png/`, `corpus/quantize_pipeline/`) is built
  by `generate_seed_corpus.py` from the **real** committed fixtures
  (`../tests/edge/corpus`, `../tests/adversarial/corpus`) plus deterministic
  **chunk-level mutations** of them (splice / reorder / resize / duplicate /
  drop chunks; perturb IHDR dims, depth, colour type, interlace). Regenerate
  or verify with:

  ```bash
  python3 fuzz/generate_seed_corpus.py           # (re)write
  python3 fuzz/generate_seed_corpus.py --check    # byte-exact drift check
  ```

- **Dictionary** (`dictionaries/png.dict`) supplies PNG's magic tokens
  (signature, chunk codes, IHDR field values, zlib headers) so the mutator
  reaches deep decoder states.

## Dual toolchain (READ THIS)

cargo-fuzz drives libFuzzer through `-Z` flags (`-Zsanitizer=address`,
`-Zbuild-std`) that **only nightly** accepts. The release crate one directory
up stays pinned to **stable 1.96.1** (`../rust-toolchain.toml`) and must not
move. The nightly requirement is contained entirely in this directory by
`fuzz/rust-toolchain.toml` (`channel = "nightly"`).

This crate is a **separate package and its own workspace root** (see
`Cargo.toml`), so `cargo build/test --manifest-path ../Cargo.toml` — the
release build — never sees it. Adding/using this harness leaves the release
binary byte-identical.

### Running it on this dev box (Homebrew cargo, not a rustup proxy)

`/opt/homebrew/bin/cargo` is a plain Homebrew binary, so `cargo +nightly` and
the `rust-toolchain.toml` override are **inert** for it. Select nightly
explicitly by putting the nightly bin dir on the **front** of `PATH` (this
makes the nightly cargo/rustc pair the active one), with `cargo-fuzz` also on
`PATH`:

```bash
NBIN="$(dirname "$(rustup which --toolchain nightly rustc)")"
export PATH="$NBIN:$HOME/.cargo/bin:$PATH"        # nightly cargo/rustc + cargo-fuzz
cd "$(git rev-parse --show-toplevel)"   # the crate root

# One-time setup, if missing:
#   rustup toolchain install nightly --profile minimal --component rust-src
#   cargo install cargo-fuzz --locked

cargo fuzz build                                   # builds both targets w/ ASAN

# Run a target. Use an EPHEMERAL writable corpus (corpus-work/, gitignored) as
# the primary dir and the committed seed corpus as a read-only seed source, so
# libFuzzer's coverage discoveries never mutate the reproducible seed corpus.
cargo fuzz run decode_png \
  fuzz/corpus-work/decode_png fuzz/corpus/decode_png -- \
  -dict=fuzz/dictionaries/png.dict -rss_limit_mb=2048 -max_total_time=600
```

On a rustup-proxied machine, `cargo +nightly fuzz run …` works directly.

### ASAN / arm64 macOS status

AddressSanitizer + `-Zbuild-std` compiles, links, and runs on
`aarch64-apple-darwin` under this nightly (verified 2026-07-21). cargo-fuzz's
default sanitizer is `address`; `-rss_limit_mb` additionally bounds memory so a
giant-IHDR mutation that regressed the pixel cap would be caught as an OOM.

## Findings → permanent regressions

Any input libFuzzer flags is minimised (`cargo fuzz tmin <target> <artifact>`)
and committed to `crash-regressions/`, which `../tests/fuzz_regressions.rs`
replays against both targets on the **release** toolchain in plain `cargo
test` — no nightly needed to prove a fix stays fixed. The directory is empty
when the window reproduced no crash (the current state).

## What fuzzing cannot claim

Absence of a crash in a bounded window is not a proof of total absence.
Coverage-guided fuzzing explores reachable states heuristically; it does not
enumerate them. See the T-0207 evidence log for the exact CPU-time run, the
coverage reached, and the documented deflate-amplification observation (a
maximally-compressible IDAT inflates fully before the length check rejects it —
a bounded typed `data_error`, not a §31 violation).

[`libfuzzer-sys`]: https://docs.rs/libfuzzer-sys

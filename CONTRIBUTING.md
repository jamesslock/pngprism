# Contributing to pngprism

Contributions are welcome. Two things about this crate are unusual enough that
knowing them first will save you a rejected pull request.

## 1. Output bytes are part of the contract

The point of this crate is that the same input, flags and build produce the
same bytes, on any machine, every time. People pin those digests.

So **a change that alters the bytes produced for an existing flag combination
is a breaking change** — including one that makes files *smaller*. That is not
a patch release here; it invalidates every pinned digest downstream.

If your change alters output:

- Say so explicitly in the pull request, with which inputs and flags change.
- Expect it to land behind a new flag or a minor version bump, not silently.
- A genuine improvement is welcome. It just has to arrive announced.

If you are unsure whether your change alters output, run the test suite — the
golden and parity tests are designed to tell you.

## 2. The Python reference is the oracle, not a legacy artifact

`tests/oracle/` holds a Python implementation of the same pipeline, vendored
and digest-pinned. `tests/caps.rs` runs it as a subprocess and asserts the Rust
code agrees with it byte for byte.

**It is the authority on behavior.** If Rust and Python disagree, the default
assumption is that the Rust is wrong. A change that makes the suite pass by
editing the oracle will not be accepted unless the oracle itself is the thing
being fixed — and then both change together, with the pins in
`tests/oracle_pins.rs` updated in the same commit.

This exists because a port whose reference has drifted away is no longer a port
of anything. Please don't route around it.

## Getting set up

```bash
git clone https://github.com/jamesslock/pngprism
cd pngprism
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Rust 1.87+ (edition 2024). Python 3 must be on `PATH` or the oracle parity
suite cannot run — if it is missing you will see that suite fail rather than
skip, which is deliberate.

**`zopflipng` is optional.** It is only needed by `--pack max`; without it
those specific test cases skip and everything else runs. Install it with
`brew install zopfli` or your distro's `zopfli` package if you are touching the
packing code, since otherwise you will not be exercising it.

## What CI checks

Every push runs, on macOS and Linux: the full suite in debug and release, doc
tests, clippy and rustfmt as errors, rustdoc as errors, and `cargo-deny`
(licenses, advisories, bans, sources). All of it must be green.

## Adding test fixtures

Test images are only added if their license permits redistribution, and that is
checked rather than assumed — see [`PROVENANCE.md`](PROVENANCE.md) for the
per-fixture record. If you want to add an image, say where it came from and
under what terms. "It's widely used" is not a license.

## Scope

Reasonable: correctness fixes, performance work that does not change output,
portability, documentation, better error messages, test coverage.

Ask first: new quantization or dithering algorithms, changes to default
behavior, new dependencies. Not because they are unwelcome, but because this
crate carries a measurement discipline behind it — defaults here were chosen
against a benchmark corpus and, in some cases, blind human evaluation, and a
change to them needs to answer the same question those did.

Out of scope: anything that makes the crate non-deterministic, and anything
that requires linking GPL code (the crate is MIT OR Apache-2.0, and its
independence from `pngquant`/`libimagequant` is deliberate — see
[`PROVENANCE.md`](PROVENANCE.md)).

## Reporting bugs

A bug report is most useful with the input file, the exact flags, what you
expected, and what happened. If the input cannot be shared, the output of
`--report json` plus the image's dimensions, color type and bit depth is
usually enough to start.

Crashes and panics are always bugs: the contract is that any input, however
malformed, produces either valid output or a typed error — never a panic.

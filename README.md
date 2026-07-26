# pngprism

[![CI](https://github.com/jamesslock/pngprism/actions/workflows/ci.yml/badge.svg)](https://github.com/jamesslock/pngprism/actions/workflows/ci.yml)

A deterministic PNG palette quantizer, ditherer, and lossless indexed-PNG
packer, in Rust. It does the same job as `pngquant`: take a truecolor or RGBA
PNG and produce a smaller, valid palette PNG.

Two things make it unusual. **Output is bit-reproducible** — the same input,
flags and build always produce the same bytes, on any machine. And **the
quality claims below are bounded by what was actually measured**, including the
cases where it loses.

```bash
pngprism input.png output.png --colors 256
```

## Status

**Version 0.5.0, pre-1.0.** Usable — the CLI is contract-tested and output is
verified lossless — but the API is not stable yet, and 0.1–0.4 were internal to
the research program this came out of.

**Versioning is explicit about one unusual thing:** output bytes are part of
the contract. A change that alters the bytes produced for an existing flag
combination is *breaking* and takes a minor bump, even if the files only get
smaller. See [`CHANGELOG.md`](CHANGELOG.md) for the policy and the release
history.

## Install

Requires Rust 1.87+ (edition 2024).

```bash
cargo add pngprism        # library
cargo install pngprism    # CLI
```

Or grab a prebuilt binary for macOS or Linux from the
[releases page](https://github.com/jamesslock/pngprism/releases). To build from
source:

```bash
git clone https://github.com/jamesslock/pngprism
cd pngprism
cargo build --locked --release      # → target/release/pngprism
```

**Optional:** `--pack max` shells out to [`zopflipng`](https://github.com/google/zopfli)
(Apache-2.0) for the strongest lossless re-pack. Everything else works without
it — the default pack mode is `none`.

```bash
brew install zopfli          # macOS
sudo apt install zopfli      # Debian/Ubuntu
```

It is found on `PATH`, or via `PRISM_ZOPFLIPNG=/path/to/zopflipng`. If it is
missing, `--pack max` exits 3 and says so; no other mode is affected.

## Usage

```bash
pngprism <in.png> <out.png> [options]
```

```bash
# 256 colors, default settings
pngprism photo.png out.png

# fewer colors, perceptual color space
pngprism logo.png out.png --colors 64 --color-space oklab

# strongest lossless packing (needs zopflipng)
pngprism ui.png out.png --pack max --pack-search v2

# reproducible parallelism — byte-identical to the serial path
pngprism big.png out.png --threads 8

# machine-readable result
pngprism in.png out.png --report json
```

On success it writes one summary line to stdout and nothing to stderr. On any
failure stdout is empty and stderr carries a single diagnostic line.

### Options

| Flag | Values | Default |
|---|---|---|
| `--colors` | `1`–`256` | `256` |
| `--color-space` | `srgb`, `oklab` | `srgb` |
| `--adaptive-default` | `off`, `on`, `guarded` | `guarded` |
| `--dither` | `off`, `on` | policy-driven |
| `--dither-strength` | `0`–`1` | — |
| `--dither-policy` | `uniform`, `adaptive`, `region`, `adaptive-unit`, `luma-bluenoise` | `uniform` |
| `--pack` | `none`, `fast`, `max` | `none` |
| `--pack-search` | `v1`, `v2` | — |
| `--pack-seam-palette-sort` / `--pack-seam-memlevel` / `--pack-seam-reduction` | `off`, `on` | sort/reduction on for `--pack none` |
| `--hidden-rgb-policy` | policy name | `canonicalize-black` |
| `--threads` | `1`–`256` | `1` |
| `--parallel-merge-order` | `balanced`, `forward`, `reverse`, `shuffle:SEED` | `balanced` |
| `--report` | `json` | — |
| `--help`, `--version` | — | — |

Some combinations are rejected rather than silently resolved: explicit dither
flags with `--adaptive-default on|guarded`, `--pack-search` with `--pack none`,
`--pack-seam-*` with `--pack fast|max`, a non-`uniform` dither policy without
`--dither on`, and `--dither-strength` with an adaptive or region policy.

### Exit codes

| Code | Meaning |
|---:|---|
| 0 | success |
| 2 | usage error — bad option, missing value, wrong positional count, forbidden flag combination |
| 3 | data error — malformed PNG, `--colors` out of range, unknown policy value |
| 5 | input I/O error — path missing or unreadable |
| 70 | internal error — a violated invariant; unreachable on well-formed input |

Full contract: [`docs/cli-contract.md`](docs/cli-contract.md).

## When not to use this

**If the output does not have to be a PNG, a transform codec will almost
certainly beat it.** Palette PNG has a rate-distortion ceiling that no
quantizer can move. Checked against a multi-format compression harness across
six content classes, pngprism produced the best palette-PNG candidate every
time — and WebP, AVIF or JPEG XL still won on bytes in **all six**:

| content | best PNG (pngprism) | best transform codec |
|---|---|---|
| flat-color line art, 7 colors | 533 B | 434 B WebP, **304 B JPEG XL** |
| UI cutout, soft edges | 57.6 KB | **15–18 KB** WebP/AVIF |
| synthetic gradient ramp | 1,952 B | **614 B** WebP |
| illustration asset | 9.2 KB | **3.4 KB** AVIF |
| natural photograph | 47.7 KB | **44 KB** AVIF |
| photographic sunrise | 610 KB | **113–191 KB** AVIF |

The gap runs from ~19–43% on flat line art to 3–5× on photographs. One
inversion is worth knowing: on flat-color line art, lossless AVIF is *worse*
than every PNG-family candidate (+52%).

So reach for pngprism when the container must be PNG — a hard format
constraint, a genuinely lossless requirement, or a pipeline with no
transform-codec decoder. When the format is open, it usually is not the right
tool, and this README would rather say so than sell you something.

*(Six unblinded data points, not a controlled study.)*

## How it compares to pngquant

All figures come from an internal 48-image, 8-class benchmark corpus, measured
against **pngquant's own best-effort configuration** — `--speed 1` plus a
lossless `zopflipng` post-pass, not its `--speed 3` default. Comparing against
a competitor's fastest setting would have been an easier and less honest test.

**Bytes.** At a matched color count, pngprism wins 24, ties 16, loses 5
(3 no-compare); at a fixed 256-color palette, wins 25, ties 16, loses 5
(2 no-compare). Median output is **0.52×** the source size in both. Wins
outnumber losses roughly 5:1.

**The lead is not universal**, and where it fails is specific: five images flip
to a pngquant win at max effort, including a photographic Kodak image, an alpha
sphere, a 16-bit vase render, and the project's own benchmark dice image. A
further 32 image×engine cells that were strict wins against pngquant's default
become exact *ties* against its best-effort arm.

**Quality at a matched byte rate.** With the opt-in Oklab color space
(`--color-space oklab --pack max --pack-search v2`) matched to pngquant at
equal byte budgets (median residual 0.049%) across the 24-image Kodak set:
pngprism wins **16/24** on ssimulacra2 (median Δ +2.41, Cliff's d 0.33,
Wilcoxon p=0.030) and **17/24** on butteraugli (median Δ +1.57, d 0.42,
p=0.00061). Two images lose substantially (−11.2 and −7.6 ssimulacra2).

Most of that advantage is the **Oklab space itself**, not the quantizer — in
isolation Oklab beats sRGB on 23/24. In the default sRGB space, pngprism sits
at rough parity with pngquant rather than ahead of it. Oklab is opt-in.

**The hardest case, stated in full:** the 256-color dice/grain class. Three
blind human sessions were run on it. The first two **failed** — no pngprism
candidate reached pngquant's acceptability, 0/3 even for the strongest
metric survivors, and metric wins plainly did not transfer to human judgement.
A third session, on a newer dithered candidate, **passed** at parity with the
pngquant reference.

That third result is **n=1**: one rated unit, and its own analysis labels it
*"pilot instrument input, not an experimental result."* So the honest position
is neither of the tidy ones: this class is not established as
production-quality, and the single positive result is too small to lean on.
Treat metric wins here with particular suspicion until a properly powered
session says otherwise.

**Speed.** No current speed claim is published. Earlier measurements exist but
were taken against older default surfaces, and promoting a stale number to a
current one is the kind of thing this README exists not to do.

## Determinism

The contract is: **same input bytes + same flags + same build → same output
bytes.** It is held to an 815-image corpus (790 synthetic edge cases, the
24-image Kodak set, one large photograph) on three axes:

- **Run-to-run** — 815/815 byte-identical across repeat runs.
- **Producer independence** — the Rust implementation and an independent Python
  reference agree byte-for-byte on all 815, despite different zlib linkage
  (statically vendored 1.3.1 vs host 1.2.12).
- **Reproducibility** — an independent re-run reproduces all 815 output digests
  and all 815 source digests, with the aggregate corpus digest matching
  bit-for-bit.

`--threads N` is byte-identical to the serial path regardless of merge order:
parallelism changes the schedule, never the output.

Every `zopflipng` result is decoded and pixel-compared before it is accepted,
so `--pack max` output is verified lossless rather than assumed to be.

One caveat for exact byte reproduction: a `zopflipng` discovered on `PATH` is
not necessarily the build these figures were measured with. Output stays
lossless either way, but byte-exact reproduction of `--pack max` numbers needs
the same binary.

## How it works

Four stages, each independently testable:

1. **Decode** — an in-crate PNG decoder (all color types, bit depths, Adam7
   interlacing), with explicit ceilings on compressed size, dimensions, total
   pixels and decoded scanline bytes so malformed input fails cleanly instead
   of exhausting memory.
2. **Quantize** — occupancy-weighted k-means in sRGB or Oklab, with exact
   reproduction guaranteed when the source has no more distinct colors than the
   palette budget, and alpha extremes (0 and 255) preserved exactly.
3. **Dither** — Floyd–Steinberg with selectable policies, including a
   blue-noise mask variant. The default is unit-adaptive with a guard that
   disables dithering for images with no fully-opaque pixels.
4. **Pack** — a lossless indexed-PNG writer with row-filter and palette-order
   search, and an optional `zopflipng` post-pass. Never emits a file larger
   than its input: if no candidate wins, the input is passed through verbatim.

## Testing

```bash
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

194 tests across 13 suites, on macOS and Linux in CI. What they cover:

- **Differential parity against the Python reference** — the behavioral oracle
  is vendored at `tests/oracle/` and digest-pinned, so the two implementations
  are compared on every run rather than assumed to still agree.
- **PNG conformance** — the full PngSuite corpus, including Adam7 pairs where
  each interlaced image must decode identically to its non-interlaced twin.
- **Adversarial and edge corpora** — malformed streams, bad CRCs, absurd
  headers, a decompression bomb, plus 1,246 fuzz seeds. The contract is that
  any input yields `Ok` or a typed error, never a panic.
- **Never-worse** — output is never larger than input, on fixtures chosen to
  make that hard.

Two fixture sets are deliberately smaller here than in development, for
licensing reasons: the smoke corpus ships the 24 images that carry a
redistribution grant (of 47), and the benchmark image is not included. The
suite *asserts* which case it is rather than quietly running less —
`tests/smoke_vendor.rs` fails if the count is wrong.

`tests/amplification/` contains a deliberate decompression bomb: 65 KB that
inflates to 64 MiB. It is committed on purpose — it is the regression test for
the bound that rejects it — and may trip scanners on clone.

## Provenance

pngprism is a port of a Python reference implementation written for a
compression research program, and the two are held byte-identical by the
differential suite above. It contains no code derived from pngquant or
libimagequant, which are used only as black-box measurement baselines, never
linked or distributed.

The full research history — experiment reports, the raw measurement data
behind the figures above, and human-study records — is not public, because it
contains third-party corpora that cannot be redistributed. The consequence is
stated plainly: the benchmark numbers here come with their method and their
failure cases, but you are taking them on trust. Everything the repository
itself claims — determinism, losslessness, the CLI contract, parity with the
reference — you can run and check.

Details: [`PROVENANCE.md`](PROVENANCE.md) ·
[`REFERENCES.md`](REFERENCES.md) ·
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)

## License

**MIT OR Apache-2.0**, at your option —
[`LICENSE-MIT`](LICENSE-MIT), [`LICENSE-APACHE`](LICENSE-APACHE).

Third-party obligations are enumerated in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md), which must ship with any
binary distribution. The statically vendored zlib 1.3.1 is Zlib-licensed;
`zopflipng` is Apache-2.0 and invoked as a subprocess, neither linked nor
distributed here.

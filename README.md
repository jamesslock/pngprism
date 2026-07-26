# pngprism

_(crate renamed from `prism-quant`; the library import name and the
`lab/reference/prism_quant.py` oracle keep the lab name — T-0213.)_

A deterministic PNG palette quantizer, ditherer, and lossless indexed-PNG
packer, written in Rust. It targets the same job pngquant does — turn a
truecolor/RGBA PNG into a small, valid palette PNG — and this document
compares the two honestly, with every number traced to a committed, reviewed
record.

**This is an unreleased research-program artifact.** The current command and
package identity is `pngprism 0.5.0`, but `Cargo.toml` deliberately retains
`publish = false`; there is no crates.io release or API-stability promise.
The v0.5 Rust and Python implementations share the surface documented below,
and their current matrix construction is machine-checked at 2,452 cells.

### Where the evidence lives

This repository is the **crate**: the implementation, its own test suite, and
the documents that describe its contract. The **evidence** behind every measured
claim below — the experiment reports, the golden corpus, the parity matrix, the
release gates, the human-study records — lives in Project Prism's research lab,
the private **`pngprism-lab`** repository (ADR-0033).

So: any path in this document under `experiments/`, `benchmarks/`, `datasets/`,
`lab/`, `coordination/`, `parity/` or `reviews/`, and `state.md`, names a file in
**that** repository, not this one. They are cited rather than reproduced because
the lab carries vendored third-party trees and corpora whose redistribution
terms are unresolved — the reason the split exists.

What that means for a reader here: you can run and verify everything this
repository claims *about itself* (see "What `cargo test` here actually covers"),
and you are taking the lab-side measurements on the strength of a record you
cannot currently open. That is stated rather than blurred. A green `cargo test`
here is not the release gate, and this document does not imply it is.

The lab's canonical `current-gate-summary.json` also records that public release
and public-lab export remain **BLOCKED** on outstanding human decisions.

## What it is

- Palette PNG quantization at up to 256 colors, sRGB by default, with an
  Oklab-perceptual color space available opt-in (`--color-space oklab`).
- **Guarded adaptive dithering by default** — omitting `--adaptive-default`
  runs unit-adaptive dithering; the guard turns it OFF only for images with
  no fully-opaque pixels (`opaque_frac == 0.0000`) — i.e. wholly-transparent
  or alpha-dominated sources, per E-0038's firing table
  (`experiments/E-0038-guard-a-flip/REPORT.md`). Explicit
  `--adaptive-default off|on` still selects the frozen legacy no-dither or
  frozen unguarded-adaptive path, byte-for-byte as before. This is the
  output of a pre-registered, James-run human decision rule, not an
  engineering default picked in isolation — see "Quality" below.
- **S/R pack seams on for the plain pack path** (`--pack none`) —
  palette-sort trials (ARM-S) and reduction-ladder rungs (ARM-R) are tried
  by default when neither the seam flags nor `pack=none` is stated
  explicitly; a memLevel race (ARM-M) was measured to make no difference
  and stays off. `--pack fast|max` are unaffected (seams there resolve off;
  the max packer already owns its own palette/row-filter search). Explicit
  seam flags always override, and once any is named, unspecified peer
  seams keep their off default.
- **Opt-in, byte-exact parallelism** (`--threads N`) — per-stage sharding
  bitwise identical to the single-threaded oracle across the full parity
  matrix, independent of merge order; default remains serial.
- **Static-vendored zlib 1.3.1** — no host `libz` dependency (`otool -L`
  shows zero host-libz matches on a `--locked` release build); DEFLATE
  parameters (level 9, windowBits 15, memLevel 8, default strategy) are
  fixed so output is reproducible across machines.
- **Deterministic output**: same input bytes + same flags + same engine
  commit always produce the same output bytes — see "Determinism" below.

## Honest benchmarks

Every number below is read from a committed, reviewed report — the path is
given so you can check it yourself. None of these numbers is a claim that
prism is "better" in general; each is scoped to its stated corpus, metric,
and configuration.

### Bytes — prism vs pngquant's own best effort

`experiments/E-0034-best-effort-incumbent/REPORT.md` (accepted) ran prism's
v0.3.x flagship configurations against pngquant's own **best-effort**
arm (`--speed 1` + a lossless `zopflipng` post-pass, not pngquant's
`--speed 3` default) on the standing 48-unit, 8-class scorecard:

- **Matched-color palette**: prism wins 24, ties 16, loses 5, no-compares 3
  (median bytes/source ratio **0.52**) — wins outnumber losses **roughly 5:1**.

  (The source report states these ratios as exact rationals — `21289/40937`,
  `1657704281/3188419182` — because it medians with `Fraction` arithmetic, which
  is why its numerals run to ten digits for a 48-unit scorecard. Both reduce to
  0.52. Quoted as decimals here: an unreducible ten-digit fraction reads as a
  byte total, and a reader cannot open the report to find out that it is not
  one.)
- **Fixed 256-color palette**: prism wins 25, ties 16, loses 5, no-compares 2
  (median bytes/source ratio **0.52**) — same ~5:1 lead.
- **The lead is not universal.** At max-effort, 5 named units flip from a
  prism win to a pngquant win: `kodak-kodim01`, `legacy-alphaball`,
  `legacy-skyvase-16bit`, `prism-benchmark-dice`, `w3c-alphatest` — including
  the program's own benchmark-image unit. A further 32 unit×engine cells
  that were strict wins against pngquant's default (`--speed 3`) become
  exact ties against the best-effort arm. Full named tables are in the
  report.

`experiments/E-0040-seam-adoption/REPORT.md` (accepted) measured the S/R
pack-seam default adoption itself, never-worse by construction and
decode-identical everywhere it changed anything:

- Full 815-unit corpus: 506 units smaller, **0 larger**, total −9,054 bytes.
- Fixed 48-unit scorecard: 33 units smaller, **0 larger**, total −2,445
  bytes.

### Speed — historical measured pins, not a current-HEAD claim

`benchmarks/speed-v04/run/report.md` (accepted, T-0199) is a **speed table,
not a quality claim** — quoted here with its own framing intact because
that caveat is load-bearing:

> This is a speed table, not a quality claim. The four cells do not
> produce interchangeable output... Nothing here licenses a
> "prism is better/worse than pngquant" statement; it licenses only
> "prism-default processes N MPix/s under these pinned conditions."

The historical matched-primary comparison (`pngprism` defaults vs
`pngquant --speed 3`, both single-threaded) on the report's contention-robust
user-CPU basis, 72 units (48-unit scorecard + 24 Kodak):

| | prism-default | pngquant-s3 |
|---|---:|---:|
| median MPix/s, all 72 | 2.8 | 2.3 |
| median MPix/s, Kodak-24 only | 3.4 | 3.4 (parity) |

> **Pin boundary:** T-0199's crate source (`e46655fa`) predates the guarded
> adaptive default and default-on pack seams. Its 2.8 MPix/s number therefore
> describes the old `--adaptive-default off` + seam-off surface, not current
> defaults. T-0209's replacement measurement was accepted and measured its own
> pin (`39a2714d`) at 0.919 median user-CPU MPix/s over 72 units and 0.839 on
> Kodak-24. Those values come from
> `benchmarks/perf-v05/budgets/budgets.json`, not copied prose. That pin also
> predates reviewed implementation head `6bae3b54`, so the gate summary labels
> it `HISTORICAL_PIN_ONLY`: this README makes no current-HEAD speed claim.

The run was measured under sustained multi-lane contention (1-minute
loadavg 14.7–31.4, median 26.1, on a 10-CPU host); wall-clock numbers are
flagged contention-affected in the source report and are not repeated
here — only the user-CPU basis is quoted. `--threads 8` is included in the
source report for context, not as a matched comparison, and was **not**
faster under that load.

### Quality — at matched byte rate, and at the human bar

`experiments/E-0033-matched-rate-parity/REPORT.md` (accepted) matched prism's
**opt-in Oklab color space** (`--colors 256 --color-space oklab --pack max
--pack-search v2`) to pinned pngquant 4.0.0 at equal byte budgets (median
residual 0.049%, max 0.199%) over the full 24-image Kodak photographic set:

- ssimulacra2: prism wins **16/24**, loses 8 (median delta +2.41, Cliff's d
  0.33, Wilcoxon p=0.030).
- butteraugli: prism wins **17/24**, loses 7 (median delta +1.57, Cliff's d
  0.42, p=0.00061).
- The win is a **majority effect, not universal**: `kodim23` (−11.2
  ssimulacra2) and `kodim15` (−7.6) lose substantially to pngquant. The
  advantage is attributable in large part to the Oklab space itself
  (isolated Oklab-vs-sRGB win 23/24 on ssimulacra2); prism-sRGB alone sits
  at parity with pngquant, not ahead of it.
- **This is an opt-in flag, not the default surface** — the default color
  space is sRGB.

**Human-gate honesty.** Objective metrics are not the acceptability bar;
James's blind pilot sessions are (`state.md`, round-4 session
`20260720T101740-james-r4-e119906d` and round-5 session
`20260720T200815-james-r5-a80e20f6`). Two results matter for this release:

- The guarded-default flip described above was accepted by round-5's
  pre-registered rule (round-4 found one regression, `w3c-alphatest`;
  round-5 re-tested it blind, neither accepted it, so by the pre-committed
  rule the flip ships **with a guard** that reverts exactly that unit).
- **The 256-color dice/grain class is NOT at human parity with pngquant.**
  Round-4's G1 gate failed (neither weak survivor reached pngquant
  acceptability), and round-5's R1 gate failed again, 0/3, even for the
  strongest proxy-metric survivors — "the proxy-vs-human gap on dice remains
  the standing frontier" (`state.md`, round-5 entry). Do not read the byte
  or metric numbers above as evidence this class is shippable-quality; it
  is not, by the program's own human gate.

## Limitations — when NOT to use palette PNG at all

Palette-PNG quantization has a rate-distortion ceiling that no amount of
quantizer engineering moves: on photographic or smooth-gradient content,
transform codecs (WebP, AVIF, JPEG XL) reach the same or better perceived
quality at a fraction of the bytes. This is not a prism-specific weakness —
it is measured directly, unblinded, by cross-checking prism's own output
against a companion multi-format compression harness's automatic
recommendation across six content classes (`state.md`, entries from
2026-07-21 ~02:15Z through ~03:40Z):

| content class | outcome |
|---|---|
| flat-color line art (a 763-byte icon, 7 colors) | closest case, but still a loss: prism holds the best-palette-PNG slot at 533 B, but lossless WebP (434 B) and lossless JPEG XL (304 B, ~43% ahead) both beat it. The one inversion: lossless AVIF is *worse* than every PNG-family candidate here (+52%) — the one class where a transform codec underperforms |
| a UI cutout with soft edges | WebP/AVIF (15–18 KB) vs prism's best-palette-PNG (57.6 KB) — transform codecs ~3.8x smaller |
| a synthetic gradient ramp | WebP (614 B) vs prism's best-palette-PNG (1,952 B) — transform codec ~3.2x smaller |
| an illustration-style asset | AVIF (3.4 KB) vs prism's best-palette-PNG (9.2 KB) — transform codec ~2.7x smaller |
| a natural photograph (dice test image) | AVIF (44 KB) vs prism's best-palette-PNG (~47.7 KB); James's own unblinded eye separately preferred a 23 KB AVIF candidate over a 56 KB pngquant candidate — a full quality tier at half the bytes |
| a photographic sunrise image | AVIF (113–191 KB) vs prism's best-palette-PNG (610 KB) — transform codecs 3–5x smaller |

Across all six anchors, prism held the **best-palette-PNG-candidate** slot
every time it was tried (6/6) — but a transform codec beat that best-PNG
candidate on overall bytes in **all six** cases too, from a modest
~19–43% on flat-color line art up to 3–5x on photographic content.
**Practical guidance:** reach for prism (or pngquant) only when the
output must literally be a PNG (a container constraint, a genuinely
lossless requirement, or a pipeline without a transform-codec decoder);
when format choice is open, a transform codec wins on bytes at
equivalent-or-better quality on every class checked so far, including the
flat-color case. This finding is exploratory (six unblinded product-side
data points, not a controlled study) and motivates v0.5 work on automatic
classification — it is not itself a gated experiment.

## Determinism guarantees

The frozen determinism contract is **same input bytes + same flags + same
engine commit → same output bytes**. Its accepted record covers an 815-unit
corpus spanning
790 synthetic edge cases, the 24-image Kodak set, and one large real image
(`benchmarks/golden-corpus/MANIFEST.md`, accepted T-0177):

- **Twin (run-to-run determinism):** 815/815 byte-identical across two
  runs of the same producer.
- **Cross (producer independence):** the Rust crate and the independent
  Python reference oracle (different zlib linkage — vendored 1.3.1 vs host
  1.2.12) agree byte-for-byte on all 815/815 outputs.
- **Verify (reproducibility from committed inputs):** an independent
  re-run reproduces all 815 output digests and all 815 source digests
  against the committed manifest, and the aggregate corpus digest
  (`6c28532be994d732a0c2bf3bd37faad9fbc573a0b448431c52589e6db754d9b5`)
  matches bit-for-bit.

These are the frozen expectations and accepted historical evidence. Whether
the reviewed repaired head has freshly reproduced them is stated only by the
machine summary's `reviewed_implementation_head_cross_run` field; historical
counts are never promoted to a current full-gate claim.

Reproduced lab-side in one command, from the root of the `pngprism-lab`
repository:

```bash
benchmarks/golden-corpus/regenerate.sh --cross
```

That is **not** runnable from this repository — the golden corpus is one of
the fixtures that stays lab-side. It is quoted so the check is nameable and
auditable by someone who has that repository, not as an exercise for a reader
who does not.

This freeze pins the **legacy path** (`--adaptive-default off`, all pack
seams explicitly off) so it stays valid regardless of default-surface
changes; it is a reproducibility pin, not an acceptability claim
(`benchmarks/golden-corpus/README.md`).

## Build / install

```bash
cargo build --locked --release   # binary at target/release/pngprism
cargo test --locked               # debug test suite
cargo test --release --locked     # release test suite
cargo clippy --locked --all-targets -- -D warnings
```

Do not copy test totals into prose. The canonical gate summary derives the Rust
inventory from `cargo test --locked -- --list --format terse` and the Python
inventory from `unittest` discovery. Those are inventory counts, not pass
claims; `lab/ci/run-prism-quant-gates.sh` is the pass/fail authority.

**wasm32.** The default build (library + `pngprism` binary) compiles
for `wasm32-unknown-unknown` with the static-vendored zlib
(`experiments/E-0023-zlib-vendoring/INTEGRATION-RESULTS.md`) — this is
**build-only evidence**; the artifact was not executed, so runtime parity
on wasm32 is not claimed. A separate, earlier spike
(`lib/prism-quant-wasm-spike/`, T-0130) explored a pure-Rust-deflate-backend
adapter for the default quantize path only; it is spike-grade, does not
modify this crate, and is not shipped.

### `zopflipng` — an optional external tool

`--pack max` shells out to **`zopflipng`** (Apache-2.0), the only external
binary this crate needs. It is **optional**: the default pack mode is `none`
and `--pack fast` never invokes it, so a plain install runs without it.

It is resolved from **`PRISM_ZOPFLIPNG`** (an explicit path, used verbatim),
else from **`zopflipng` on `PATH`** (macOS: `brew install zopfli`; most distros
package it as `zopfli`). If neither resolves, `--pack max` exits **3** naming
both remedies; no other mode is affected.

(Inside the Prism research tree one rung sits between those two: the vendored
pinned build at `benchmarks/baselines/zopfli/…`, existence-checked, so an ad-hoc
lab run uses the pinned binary rather than whatever the host has installed. It
resolves to nothing outside that tree and does not apply to you if you are
holding only this crate.)

**Reproducing published figures:** a `PATH`-discovered zopflipng is not
necessarily the pinned build this program's numbers were measured against.
Output is lossless either way — every zopflipng result is decoded and
pixel-compared before it is accepted — but exact **byte** reproduction requires
`PRISM_ZOPFLIPNG` pointed at the pinned Apache-2.0 build, which is what the
parity and benchmark harnesses set. The quality figures under
"Quality — at matched byte rate" above are `--pack max` measurements and
therefore carry this requirement.

`PRISM_ZOPFLIPNG_TIMEOUT_SECS` overrides zopflipng's 120s per-invocation
timeout. Exit statuses: 0 success, 2 usage error, 3 data error, 5 input I/O
error, 70 internal error.

## CLI surface audit (v0.5)

This audit documents the `pngprism` command-line surface exactly as it
behaves (T-0201/T-0210, release hardening). It is a description of the current
binary, cross-checked against the CLI it mirrors
(`lab/reference/prism_quant.py::main`), not a change to it. The invocation
form is `pngprism <in.png> <out.png> [options]` — exactly two positional
paths are required; any other count prints the usage banner to stderr and
exits 2.

### Exit codes

| Code | Class | When |
|---:|---|---|
| 0 | success | output written; a one-line summary printed to **stdout** |
| 2 | usage error | bad option syntax: unknown option, missing option value, invalid enum value, wrong positional count, or a forbidden flag **composition** (see below) |
| 3 | data error | malformed/undecodable input PNG; a `--colors` value outside `1..=256` (e.g. `0`, negative); an unknown `--hidden-rgb-policy` (validated in the pipeline, not the parser) |
| 5 | input I/O error | input path missing or unreadable |
| 70 | internal error | a violated implementation invariant — unreachable on well-formed input |

Exit-code mapping is unit-tested exhaustively (`src/main.rs`,
`error_kind_exit_code_mapping_is_exhaustive`) and the 0/2/3/5 statuses are
end-to-end tested (`tests/cli.rs`, `tests/edge_corpus.rs`).

### Flags

| Flag | Accepted values | Invalid value → |
|---|---|---|
| `--colors` | integer, parsed like Python `int(str, 10)` (sign, PEP-515 underscores) | non-integer → **2**; in-range-syntax but out of `1..=256` → **3** |
| `--hidden-rgb-policy` | policy name | unknown → **3** (pipeline-validated) |
| `--color-space` | `srgb` \| `oklab` | **2** |
| `--adaptive-default` | `off` \| `on` \| `guarded` | **2** |
| `--dither` | `off` \| `on` | **2** |
| `--dither-strength` | decimal in `0..1` | **2** |
| `--dither-policy` | `uniform` \| `adaptive` \| `region` \| `adaptive-unit` \| `luma-bluenoise` | **2** |
| `--pack` | `none` \| `fast` \| `max` | **2** |
| `--pack-search` | `v1` \| `v2` | **2** |
| `--pack-seam-palette-sort` / `--pack-seam-memlevel` / `--pack-seam-reduction` | `off` \| `on` | **2** |
| `--threads` | integer in `1..=MAX_THREADS` | **2** |
| `--parallel-merge-order` | `balanced` \| `forward` \| `reverse` \| `shuffle:SEED` | **2** |
| any `-`-prefixed token not listed | — | unknown option → **2** |

Forbidden **compositions** (all exit **2**, checked after parsing):
`--adaptive-default on\|guarded` with any explicit dither flag;
`--pack-search` with `--pack none`; `--pack-seam-*` **on** with `--pack
fast\|max`; a non-`uniform` `--dither-policy` without `--dither on`;
`--dither-strength` with `--dither-policy adaptive\|region`.

### stdout / stderr discipline

On **success**, the summary line (`pngprism <version>: <in> -> <out>
bytes, <n> palette entries (<alpha-note>)`) is the only thing written, to
**stdout**, and stderr is empty. On **any failure**, stdout is empty and a
single diagnostic line is written to **stderr**. This is enforced end-to-end
by `tests/edge_corpus.rs` (stdout empty on every malformed input; stderr empty
on every valid one) and `tests/cli.rs`.

### `--help` / `--version`

Both implementations provide these flags and exit 0 without requiring input
or output paths. `--help` wins when both occur; `--version` prints exactly
`pngprism 0.5.0`. They short-circuit anywhere in the argument vector. The
shared semantic cases are enforced by `parity/T-0210_cli_contract.py`; see
[`docs/cli-contract.md`](docs/cli-contract.md) for the binding contract.

### Robustness

The release binary is exercised over a committed, deterministically-generated
edge-case corpus (`tests/edge/`, generator
`tests/edge/generate_edge_corpus.py`, gate `tests/edge_corpus.rs`): valid
edge geometries/formats (1x1, 1xN, Nx1, 16-bit, Adam7-interlaced,
fully-transparent, single-color, palette-with-short-tRNS, gray+alpha, 2-color
palette) produce a correct indexed PNG of the source's dimensions, and
malformed inputs (random bytes, truncated streams, bad CRC, 0x0 dims, absurd
IHDR dims, empty file) produce a clean declared nonzero exit — never a panic,
signal, or hang, within a bounded per-case wall-clock ceiling. Explicit
compressed-input, per-dimension, total-pixel, and decoded-scanline ceilings are
enforced before unbounded reads, inflation, or canonical-pixel allocation.
Spec-valid exact-stream and sparse-file regressions live in `tests/caps.rs` and
the Python oracle suite. The constants, aggregate working-set rationale, and
the deliberately limited (not "OOM-proof on any host") claim are documented in
[`docs/resource-limits.md`](docs/resource-limits.md). This sits alongside the
in-process ch17 §31 no-panic adversarial suite (`tests/adversarial_suite.rs`,
T-0110).

## Rust/Python parity status

T-0193 completed the v0.4 port; T-0210/T-0212 then established the v0.5 CLI
and matrix instruments. The current `parity_v05.py` gate is success-only: both
producers and both twin runs must exit 0 and emit identical bytes. Matrix
construction for reviewed implementation head `6bae3b54` is 2,452 cells and
passes. The old `parity/T-0212-parity-v05-full.json` predates the repair wave;
the current post-repair execution is retained separately at
`benchmarks/release-eng-v05/evidence/6bae3b54/v05-full.json`. The canonical
summary revalidates that tracked evidence before deriving its current `PASS`;
copied prose does not. This engineering status is separate from—and cannot
satisfy—the blocked public-release and public-export decision gates.

## What `cargo test` here actually covers

The suite is self-contained — it builds and passes with **zero reads outside
this directory**, which is checked by copying the crate out of the research tree
and running it there, not by inspecting paths. Two corpora are deliberately
smaller here than in the lab, both for licensing reasons, and the difference is
asserted rather than assumed (`tests/smoke_vendor.rs`):

| Fixture | In the lab | Here |
| --- | --- | --- |
| M1 smoke set | 47 images | **24** — the CC0 subset (`tests/smoke/README.md`); Kodak, libpng-legacy, PMT, the-light, W3C and pilot images carry no redistribution grant |
| `benches/stage_boundaries.rs` fixture | the CC BY-SA 3.0 accountability image | **skips** unless you set `PNGPRISM_BENCH_IMAGE` |

Everything else runs in full: the PngSuite conformance corpus, the adversarial
and edge corpora, the fuzz crash regressions, and the differential parity suite
against the vendored Python oracle (`tests/oracle/`, digest-pinned).

**One fixture will look alarming to a scanner.**
`tests/amplification/corpus/bomb-8x8-rgb8-64mib-zeros.png` is a deliberate
decompression bomb: 65 KB of PNG that inflates to 64 MiB (335,544:1). It is the
reproducer for the T-0212 inflate-amplification bound and is exercised by the
test suite. Antivirus and dependency scanners may flag it on clone; it is
committed on purpose, and the bound it proves is the reason the decoder refuses
it.

**The full release gate is not in this repository.** The 2,452-cell parity
matrix, the 815-unit golden corpus, the supply-chain audit and the human-study
evidence live lab-side and are referenced above; a green `cargo test` here means
the crate's own suite passed, not that the release gate did.

## Provenance

This crate and every number above are produced under Project Prism's
evidence discipline (`coordination/PROTOCOL.md`): pre-registered
experiments, frozen-before-results commits, cross-reviewed acceptance, and
committed reproduction commands. No claim in this document is an
acceptability judgment beyond what a named gated human session (round-4,
round-5) actually decided. For the full evidence trail — every accepted
experiment, every claim's supporting review — see `experiments/README.md`
and `state.md` in the private **`pngprism-lab`** repository (see "Where the
evidence lives" above; they are not in this one).

## License

**`MIT OR Apache-2.0`**, at your option — the Rust-ecosystem standard dual
permissive license. Full texts: [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE). You may use, modify, distribute and sell
software built on this crate, including in closed-source and commercial work,
provided the copyright and license notice is retained.

Third-party obligations are separate and are enumerated in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md), which reproduces the union
of **distributed** dependency license texts (MIT, Apache-2.0, Zlib) and **must
ship alongside any binary distribution**. Dev-only dependencies are not
distributed and are deliberately excluded from it — so the `Unicode-3.0` that
appears in a full `cargo deny` license census is not reproduced there, because
nothing carrying it ships. Two notes on things this
crate uses without linking: the statically vendored **zlib 1.3.1** is
Zlib-licensed, and **`zopflipng`** (Apache-2.0, optional, `--pack max` only) is
invoked as a subprocess and is neither linked nor distributed here.

The GPL-licensed `pngquant`/`libimagequant` appears in this program only as a
black-box measurement baseline in the research lab — never linked, never
distributed with the crate — and therefore places no constraint on this
license. See [`PROVENANCE.md`](PROVENANCE.md).

The license covers the **code**. It is not a warranty of the measured claims
above, which carry their own evidence and their own stated bounds.

# pngprism CLI contract (T-0210, Theme E-19; renamed from prism-quant, T-0213)

This document is the **stability contract** for the `pngprism` command-line
surface. It is the single source of truth for the exit codes, the flag
surface, the never-worse output guarantee, and the `--report json` schema, and
it states the semver policy that governs how each may change.

Two implementations honor this contract byte-for-byte where the parity rule
requires it (one behavior, two implementations — never two behaviors):

- **Rust** — this crate (`pngprism` package, `pngprism` binary,
  `src/main.rs`; the library target keeps the import name `prism_quant`).
- **Python reference oracle** — `lab/reference/prism_quant.py`.

The contract is enforced by tests in **both** implementations
(`tests/never_worse.rs`, `test_prism_quant.py::PrismQuantCliContractTests`) and
by the differential gate `parity/T-0210_cli_contract.py`, which re-derives the
flag surface, the never-worse behavior, the report bytes, and the exit codes
from both binaries and requires them to agree.

## 1. Never-worse output guarantee

The tool is **never worse than doing nothing**. The CLI retains the original
input bytes before any destination publication, builds the encoded indexed-PNG
candidate separately, and chooses the final bytes in memory. **If the encoded
output is `>=` the input file's bytes, the chosen artifact is the retained input
verbatim.** It then publishes exactly once through an atomic same-directory
replace and reports the fallback honestly (a `--report json` flag and, in human
mode, a one-line `never-worse:` note). A tie (`==`) resolves to the input,
because the original is the trustworthy artifact. The sequence is safe when
input and output are the same path, hardlink aliases, or symlink aliases; all
three cases are differential regression tests in Rust and Python.

This is implemented **once per implementation, at the CLI layer** (`main`), not
in the library `quantize_png*` functions — so the quantization pipeline and
therefore the golden-corpus outputs are unchanged. All 815 golden-corpus units
have encoded output strictly smaller than their inputs (min margin 2599 bytes;
measured, `parity/T-0210-never-worse-golden.json`), so the gate is provably
**inert** on the golden corpus and the frozen digests are untouched.

The exception clauses (where the gate fires) are pinned by shared differential
fixtures in `tests/never_worse/corpus/`: `tiny.png` (1-pixel; overhead
dominates), `already-palette.png` (an already-optimized indexed PNG; re-encode
is idempotent), and `incompressible.png` (a small high-entropy field that does
not compress).

## 2. Exit codes

The union of exit codes either implementation can return. These are pinned by
tests in both implementations.

| Code | Name           | Meaning                                                   |
|-----:|----------------|-----------------------------------------------------------|
| `0`  | success        | Output written (encoded candidate, or input verbatim under the never-worse fallback). |
| `2`  | usage error    | Invalid CLI syntax/composition: unknown flag, missing flag value, bad enum value, wrong positional count, non-composable flags. Diagnostic on stderr, stdout empty. |
| `3`  | data error     | Malformed/unsupported/invalid input data: undecodable PNG, out-of-range `--colors`, invalid `--hidden-rgb-policy`, etc. |
| `5`  | input I/O error| The input file could not be read (missing/permission), or the output could not be written. |
| `70` | internal error | A violated implementation invariant (self-check failure). Should never occur on valid input. |

Notes:
- A **missing input file** is `5` (I/O), not `3` — the file could not be read
  at all. A file that exists but is not a valid PNG is `3` (data).
- On any non-zero exit, stdout is empty; the one-line diagnostic is on stderr.

## 3. Flag surface

Shared flags (identical meaning in both implementations):

```
--colors N                  --hidden-rgb-policy P       --color-space srgb|oklab
--adaptive-default off|on|guarded                       --dither off|on
--dither-strength S         --dither-policy uniform|adaptive|region|adaptive-unit|luma-bluenoise
--pack none|fast|max        --pack-search v1|v2
--pack-seam-palette-sort off|on   --pack-seam-memlevel off|on   --pack-seam-reduction off|on
--max-pixels N              --report json               --version                   --help
```

`--max-pixels N` sets the decoder's pixel admission ceiling (default
67,108,864 = 64 Mi-pixel; `N` must be an integer ≥ 1). It overrides the default
up or down and is a HARD BOUND checked at IHDR before allocation — see
`docs/resource-limits.md`. Invalid values (`0`, negative, non-numeric, missing)
are usage errors (exit `2`) in both implementations with a byte-identical
diagnostic. The derived decoded-scanline ceiling scales with it, so this single
lever governs both admission tests.

**Documented, pinned feature asymmetries** (both implementations emit 0.5.0,
but the Python research surface and Rust parallel port have intentionally
different opt-in extensions):

- **Python-only:** `--colors-search MIN..MAX@QUALITY` (fewest-colors search;
  not ported to Rust).
- **Rust-only:** `--threads N`, `--parallel-merge-order …` (the port's
  stage-parallel surface; the oracle is single-threaded by construction).

The differential gate asserts the two flag sets' symmetric difference is
**exactly** `{--colors-search, --threads, --parallel-merge-order}` and that
`{--report, --version, --help}` are present in both — a canonical set
comparison, not an expected-list walk, so a new flag added to one impl and not
the other fails the gate.

The shared dither-policy compositions are behavioral contract, not just help
text:

- `uniform` is valid with dither off or on.
- `adaptive` and `region` require `--dither on`, and their policy-supplied
  strengths are not composable with a non-unit `--dither-strength`.
- `adaptive-unit` is valid with dither off or omitted; in that state the policy
  is inert. With dither on, an explicit strength wins over its predicted unit
  strength.
- `luma-bluenoise` requires `--dither on`. Its `--dither-strength` scales the
  promoted E-0017 threshold-mask amplitude; it is not a Floyd-Steinberg
  region-hook variant.

The integer grammar for shared integer flags follows Python `int(text, 10)` on
the supported surface: leading/trailing ASCII whitespace, one optional ASCII
sign, Unicode 16.0 `Nd` decimal digits (including mixed digit scripts), and
PEP-515 single underscores between digits. Unicode numeric characters outside
`Nd` (for example superscripts, Roman numerals, and ideographs) are rejected as
syntax errors. A syntactically valid integer outside a flag's value range is a
data/range error rather than a syntax error. The Rust implementation carries
the complete Unicode 16.0 decimal-zero table instead of narrowing the Python
oracle to ASCII.

`--version` / `--help` short-circuit anywhere in argv and exit `0`; `--help`
wins when both are present. The **version string has one source per impl**:
Rust prints `CARGO_PKG_VERSION` (the crate `[package] version`); Python prints
the module `__version__` (which aliases `VERSION`). As of the pngprism rename
and version unification (T-0213), both sources are set to `0.5.0`, so **both
implementations emit the identical string `pngprism 0.5.0`** — the earlier,
documented pin drift (Rust `prism-quant-rs 0.1.0` vs Python
`prism-quant v0.3.0-alpha`) is resolved. The semver policy below applies from
`pngprism 0.5.0` forward. (The underlying pipeline pin history is unchanged and
recorded in `../PORT-PLAN.md`; `0.5.0` is the release/CLI version, not a claim
that the two impls re-converged on a new pipeline.) The flag-parity contract
governs the flag **surface**, not the version **value**.

## 4. `--report json` schema

`--report json` writes a single-line, compact JSON object to stdout (and
suppresses the human summary line). It is byte-identical between the two
implementations on shared inputs (both emit
`json.dumps(separators=(",", ":"))`-equivalent bytes with a trailing newline),
and it round-trips through `python -m json.tool`.

Schema id: **`prism.cli.report/1`**. Keys, in this stable order:

| Key                    | Type   | Meaning |
|------------------------|--------|---------|
| `schema_version`       | string | Always `"prism.cli.report/1"`. |
| `bytes_in`             | int    | Input file size in bytes. |
| `bytes_out`            | int    | Size of the file actually written to the destination (the encoded candidate, or the input under the never-worse fallback). |
| `palette_size`         | int    | Palette entries of the engine's encoded candidate. Reported even under a never-worse fallback (the candidate was built, then discarded in favor of the input). |
| `candidate`            | string | `"encoded"` (the engine's output was kept) or `"input-verbatim"` (never-worse fallback). |
| `guard`                | string | The resolved `--adaptive-default` policy: `"off"`, `"on"`, or `"guarded"`. (This is the contract-level guard state; the per-image structural-guard firing is an internal pipeline detail not surfaced here.) |
| `never_worse_fallback` | bool   | `true` iff the never-worse gate fired (equivalently `candidate == "input-verbatim"`). |

The version string is deliberately **absent** from the report so the two
implementations' reports are byte-identical (this held across the earlier
version drift and continues to hold now that both emit `pngprism 0.5.0`).

## 5. Semver policy

Versioning applies to the CLI contract described here. "The tool's behavior"
means the exit codes, the flag surface, the never-worse guarantee, and the
report schema.

**PATCH** (`x.y.Z`) — no observable contract change:
- Bug fixes that do not change exit codes, accepted flags, report keys, or the
  never-worse decision.
- Diagnostic message wording changes (messages are not part of the contract;
  callers route on exit codes, not text).
- Smaller encoded output (the never-worse decision may change for a given
  input as the encoder improves — that is allowed and honest, and never
  produces a larger file).

**MINOR** (`x.Y.0`) — backward-compatible additions:
- New flags with a safe default (omission preserves prior behavior).
- New keys **appended** to the report object (existing keys keep their name,
  type, and position; consumers must ignore unknown keys).
- A new exit code for a genuinely new failure category that no prior valid
  invocation could reach.

**MAJOR** (`X.0.0`) — breaking changes:
- Removing or renaming a flag, or changing an existing flag's meaning,
  accepted values, or default.
- Removing/renaming a report key, changing a key's type, changing key order,
  or bumping `schema_version` (`prism.cli.report/2`).
- Reassigning an exit code's meaning, or changing which code a given error
  class returns.
- Weakening the never-worse guarantee (e.g. ever emitting output larger than
  the input).

Bumping `schema_version` is the explicit, machine-detectable signal of a
breaking report change; consumers should check it.

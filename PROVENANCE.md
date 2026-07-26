# pngprism — provenance & clean-room record

**Evidence label: engineering provenance record.** Not a quality claim, a
novelty claim, a license opinion, or legal advice. Every claim below is
traceable to a committed record (a git commit, a parity result JSON, or an
in-repo document); nothing here is asserted from memory. This is the crate-level
record; the deeper derivation-boundary record is
`../../lab/reference/PROVENANCE.md` (pngprism-lab: `lab/reference/PROVENANCE.md`), which
this document cross-references rather than restates.

## 1. Identity

- Crate/package name: **`pngprism`** (crates.io identity; renamed from
  `prism-quant` in T-0213, 2026-07-21, name decided by James).
- Binary: **`pngprism`**. Library import name kept as `prism_quant` (the
  code-internal module identity, the Rust analogue of the reference module
  filename `prism_quant.py`; see
  `../../benchmarks/release-eng-v05/RENAME-ENUMERATION.md` (pngprism-lab: `benchmarks/release-eng-v05/RENAME-ENUMERATION.md`)
  decision D1).
- Version identity: **0.5.0**; both implementations emit `pngprism 0.5.0`.
  This is not a claim that a public release exists: the crate remains
  `publish = false` and the public-release readiness record is blocked. The
  rename changed **zero output bytes** — proven by the
  golden corpus re-verification (§3): `corpus_digest`
  `6c28532be994d732a0c2bf3bd37faad9fbc573a0b448431c52589e6db754d9b5`
  bit-for-bit before and after.

## 2. What the crate implements, and what it was NOT derived from

`pngprism` is a **seam-by-seam Rust port of Project Prism's own reviewed Python
reference** — the deterministic PNG quantize → dither → pack pipeline in
`../../lab/reference/prism_quant.py` (pngprism-lab: `lab/reference/prism_quant.py`) (and
its PNG substrate `m1_png.py`, dither `prism_dither.py`, pack `prism_pack.py`).
The port is parity work: no new algorithms, gated on byte-identical output
against the Python oracle (`PORT-PLAN.md`).

The reference itself is implemented from **Project Prism's pre-existing
contracts, independently authored lab methods, primary papers, and permissively
licensed sources** (`../../lab/reference/REFERENCES.md` (pngprism-lab: `lab/reference/REFERENCES.md`)).
Clean-room boundary, per `../../lab/reference/PROVENANCE.md`:

- **No GPL code translated.** The binding port task prohibited translation of
  GPL code; the accepted T-0067 review reported it found **no GPL-derived
  expression** in the skeleton (commits `30b18a01…`, `c514f7d7…`).
- **pngquant / libimagequant is a black-box subprocess baseline only** — GPL-3.0
  research-use boundary, invoked as a subprocess, never linked, imported, or
  called through an API (pinned `pngquant` `4efbd189…`, libimagequant
  `9388d269…`; black-box protocol committed).
- **The libimagequant source study `book/12-libimagequant-source-study.md` is
  quarantined** from implementation — research context, never an implementation
  reference.

No pngquant or libimagequant source was consulted to author `pngprism`'s
methods. The evidence trail is this repository's own history: the port lands in
`lib/prism-quant/` (the lab monorepo path) across the T-0082/T-0095 phases,
each seam gated on parity
against the pre-existing reference (`PORT-PLAN.md`).

## 3. Dual-implementation proof chain

Two independent implementations (Rust `pngprism`, Python `prism_quant.py`) are
held byte-identical where the parity rule requires it. The records below are
committed historical milestones; they are not silently promoted to proof of a
later implementation head:

| Record | Scope | Result |
|---|---|---|
| `parity/T-0166-RESULT.json` | initial port parity sweep (oracle `a89d6610…` vs port `23736a90…`) | **ALL IDENTICAL** — 0 divergences, 0 CLI failures |
| `parity/T-0193-differential.json` | v0.4 surface, 80 cases × 5 schedules | **400/400 identical** |
| `parity/T-0210-cli-contract.json` | CLI contract differential (flag surface, never-worse, report bytes, exit codes) | **ALL PASS** |
| `parity/T-0212-parity-v05-full.json` | v0.5 full matrix, 2452 cells × twin runs | **2452/2452 identical**, twin-divergent 0 |
| golden corpus (§below) | T-0177 815-unit freeze, Rust≡Python | **815/815 byte-identical** |

**Historical post-rename re-verification (T-0213):** the golden corpus
regenerate VERIFY + CROSS re-ran through the renamed `pngprism` binary —
TWIN 815/815 run-identical, CROSS 815/815 Rust≡Python byte-identical, VERIFY
815/815 outputs + 815/815 source digests match the committed manifest,
`corpus_digest` `6c28532b…d9b5` exact and the T-0210 differential gate returned
ALL PASS. The attempted post-rename stratified rerun did not finish; the
completed T-0212 stratified/full records predate that rename and the later
repair wave. Evidence and the original scoping are in
`../../benchmarks/release-eng-v05/RESULTS.md` (pngprism-lab: `benchmarks/release-eng-v05/RESULTS.md`).

The machine-generated
`current-gate-summary.json` (pngprism-lab: `benchmarks/release-eng-v05/current-gate-summary.json`)
pins the reviewed findings-1-through-10 implementation at `6bae3b54`. It
validates the current 2,452-cell matrix construction and is the sole status
source for a full parity run and golden CROSS at that head. The underlying
post-repair artifacts, including all per-cell records, are retained at
`../../benchmarks/release-eng-v05/evidence/6bae3b54/`; summary verification
reopens them and re-derives the official execution record. This prose does not
mirror the mutable value. Public-release and public-export decisions remain
separate, blocked gates regardless of an engineering pass.

## 4. Vendored zlib provenance

The PNG DEFLATE backend is **statically vendored stock zlib 1.3.1**, so IDAT
output is byte-identical to Python's `zlib.compress(data, 9)`:

- Source: **`libz-sys =1.1.20`**, features `libc, static, stock-zlib` — compiles
  its bundled stock zlib and bypasses host discovery (the byte-determinism
  contract). `flate2 =1.1.9` (`default-features=false`, `features=["zlib"]`) is
  routed onto that same C backend (the pure-Rust `miniz_oxide` backend is
  deliberately excluded).
- zlib version **1.3.1** (`ZLIB_VERSION "1.3.1"`, `VERNUM 0x1310`).
- Tarball sha256
  `9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23`, from
  `https://zlib.net/fossils/zlib-1.3.1.tar.gz`; in-repo pin
  `experiments/E-0023-zlib-vendoring/vendor/zlib-1.3.1.tar.gz`. (Recorded in
  `../../benchmarks/golden-corpus/regenerate.py::load_pin`.)
- CVE posture: `docs/security-zlib-cve-posture.md` (T-0208 headline: no known
  zlib CVE reachable through the crate's call surface).

zlib is distributed under the **zlib license** (permissive); its text is
included in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

## 5. Toolchain pin

- Crate MSRV floor: `rust-version = "1.87"` (`Cargo.toml`).
- Tooling pin: `rust-toolchain.toml` channel **1.96.1** (clippy/rustfmt), the
  version fmt/clippy output was verified against (T-0149).
- Edition 2024. Target: macOS/arm64 (the product ships Apple-Silicon-only).

## 6. Repository history

This repository begins with a **single initial commit**. That is deliberate, and
it is not the whole record.

The crate was developed inside Project Prism's research monorepo across roughly
3,000 commits, and that history is preserved in full — in the private
**`pngprism-lab`** repository, which is the same tree filtered to the research
program. Nothing was discarded; it was not *published*, because those commits
carry vendored third-party source trees and corpora whose redistribution terms
are unresolved, and a public object store is permanent: one filtering miss
cannot be taken back. The crate is pre-1.0 with `publish = false`, so no
downstream bisect is being broken by this (ADR-0033 §4).

What this means when reading the rest of this record: claims that cite a `T-####` task,
an `E-####` experiment, a commit hash, or a path under `experiments/`,
`benchmarks/`, `datasets/` or `lab/` are citing that private repository. They are
real, dated, and cross-reviewed records; they are simply not ones you can open
from here. Where a claim can be checked from this repository alone, it says so.

The initial commit's tree is generated from the monorepo by
`lab/ci/assemble-public-crate.sh` (lab-side): git-tracked crate files only,
minus the lab-side harnesses and rationale documents, with `../../` markdown
links rewritten into citations. "What is public" is therefore a rule that can be
re-run and reviewed, not a recollection of which files someone copied.

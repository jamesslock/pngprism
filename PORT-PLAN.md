# pngprism Rust port — phase 1 plan (T-0082, written as `prism-quant`)

**Label: v0.1-alpha parity work — no performance claims, no new algorithms,
no API stability claims.** This document is the binding plan for the first
phase of the production Rust library, per James's recorded language
decision ("ok sounds like rust is best here for our lib at least",
`state.md` 2026-07-18, "Language decision — prism-quant library"). It was
committed before any code, as required by the T-0082 task card.

## 1. What we are porting (the oracle)

The oracle is our own reviewed Python reference implementation — in-repo
original work, so there is no clean-room issue with OUR code. The clean-
room quarantine still binds in full: no libimagequant source, no
`book/12-*`. Method provenance is inherited unchanged from the oracle's
ledger rows (`lab/reference/REFERENCES.md`); the crate headers cite the
oracle files and those rows as the source.

- **`lab/reference/prism_quant.py` v0.1.1-alpha** (T-0067 skeleton,
  T-0068 real core, review-passed): the six-seam pipeline
  `decode -> sample -> palette init -> refinement -> remap -> emit`,
  bins/exact-path, alpha ladder with 0/255 locks, farthest-point seeding,
  zoned Lloyd refinement, hidden-RGB policies, PLTE/tRNS emission.
- **`lab/reference/m1_png.py`**: the arbitrary-PNG decoder
  (`decode_png`, canonical RGBA8) and the deterministic indexed writer
  (`write_indexed_png`). Ported in full, not to a smoke-set subset —
  the decoder is one coherent spec and partial ports invite silent
  behavioral drift on inputs the smoke set does not cover.
- **Oracle tests** (`test_prism_quant.py`, `test_m1_png.py`): ported to
  Rust (binding tests, hand vectors, PngSuite oracle pairs, malformed-
  input containment, CLI contract tests) so the Rust suite pins the same
  behavior the Python suite pins.

The Python reference is the ORACLE in the strong sense: where Rust and
Python disagree, the Python is presumed correct and the Rust is wrong.
If parity work ever reveals the PYTHON behavior to be the bug, the port
STOPS at that stage and the finding is recorded in the task Evidence
log — no silent divergence.

## 2. Phase 1 scope

**Includes (this phase):**

- Crate scaffold at `lib/prism-quant/` (this directory), edition
  matching the lab runner (`edition = "2024"` in
  `lab/runner/Cargo.toml`), default rustfmt settings, clippy
  `-D warnings --all-targets`, `cargo test` in debug AND release with
  `--locked` — the same gates as `lab/test-all.sh`.
- Full port of the quantizer core and both PNG directions (below).
- Binary `prism-quant-rs <in.png> <out.png> --colors N
  [--hidden-rgb-policy P]` mirroring the Python CLI: same defaults
  (256 colors, `canonicalize-black`), same exit statuses (0 success,
  2 usage, 3 data error, 5 input I/O error, 70 internal), same one-line
  diagnostics discipline (stderr only; stdout empty on failure), same
  success summary line shape.
- Unit/integration tests ported from the oracle suites.
- The parity harness (§4).

**Defers (explicitly not this phase):**

- Dither (`prism_dither.py`) and pack/order search (`prism_pack.py`) —
  phase 2.
- C-ABI surface, NEON/SIMD, rayon/multithreading — later phases.
  Phase 1 is single-threaded, determinism-first.
- Any optimization that risks behavior drift (arena allocators, hash
  tweaks, approximate math): not now.

## 3. Module map (Python seam -> Rust module)

The crate layout mirrors the two oracle files one-to-one so the reviewer
can diff seam-by-seam. Every public function keeps the oracle's name in
Rust snake_case and cites its oracle line range in a doc comment.

| Oracle (Python) | Rust (this crate) |
| --- | --- |
| `m1_png.decode_png` + chunk/CRC/inflate/defilter/sample-convert helpers | `src/png.rs` (`decode_png`, `PngError`, `DecodedImage`, private helpers mirroring `_parse_chunks`, `_defilter`, `_row_samples`, `_convert_row`, `_pass_geometry`, Adam7 passes) |
| `m1_png.write_indexed_png` + `_emit_chunk` + CRC-32 | `src/png.rs` (`write_indexed_png`, private `emit_chunk`, table CRC-32) |
| zlib inflate/deflate (Python stdlib `zlib`) | `flate2` crate, **zlib (C library) backend only** — see §5 |
| `prism_quant` constants (`EXACT_BIN_LIMIT`, `PRECLIP_LEVELS`, `REFINE_SAMPLE_CAP`, `RGB_REP_MAX`, `ALPHA_LADDER_INTERIOR_MAX`, `RGB_FIT_ITERS`, `ALPHA_LADDER_MAX_ITERS`, `REFINE_MAX_ITERS`, policies, zones) | `src/quant.rs` (same names, same values, doc-cited) |
| `premultiplied_distance_sq`, `_round_half_up`, `_zone_of`, `_alpha_bin`, `_pack_rgba` | `src/quant.rs` (same names) |
| `_Bin`, `_build_bins` (exact histogram -> bounded preclip overflow, member sums, sorted-by-key) | `src/quant.rs` (`Bin`, `build_bins`; `BTreeMap` gives the oracle's `sorted(tuple-key)` order by construction) |
| `_bin_mean_color`, `_bin_premult_mean`, `_centroid` | `src/quant.rs` (same names) |
| `_alpha_ladder` (0/255 locks, weighted-quantile seeds, 1-D Lloyd) | `src/quant.rs` (`alpha_ladder`) |
| `_refine_sample` (deterministic stride) | `src/quant.rs` (`refine_sample`) |
| `_fit_rgb_reps` (farthest-point seeding + weighted Lloyd polish) | `src/quant.rs` (`fit_rgb_reps`) |
| `stage_sample` (identity), `stage_palette_init` (A2 pair co-occurrence, per-zone reservation, mass-ranked cap, hidden-RGB policies) | `src/quant.rs` (`stage_sample`, `stage_palette_init`, `PaletteInit`) |
| `_entry_premult`, `_nearest_entry` (zoned, one-directional degradation), `stage_refinement` (zoned joint Lloyd, worst-served re-seed, never-drop-last-zone-entry) | `src/quant.rs` (same names) |
| `stage_remap` (all bins, exact/preclip pixel keying) | `src/quant.rs` (`stage_remap`) |
| `stage_emit` | `src/quant.rs` (thin wrapper over `png::write_indexed_png`) |
| `quantize_image`, `quantize_png` (incl. post-emit self-verification re-decode), `StageNotes`, `PrismQuantError` | `src/lib.rs` re-exporting from `src/quant.rs` (`quantize_image`, `quantize_png`, `StageNotes`, `Error`) |
| `main` (bounded option parsing, exit statuses, summary line) | `src/main.rs` |
| `test_m1_png.py` hand vectors + malformed-input containment | `src/png.rs` `#[cfg(test)]` + `tests/png_corpus.rs` (smoke-set manifest checks, PngSuite basi/basn oracle pairs) |
| `test_prism_quant.py` binding/distance/CLI tests | `tests/quant_binding.rs`, `tests/cli.rs` |
| (no Python counterpart — the acceptance heart) | `parity/parity_sweep.py` (§4) |

Non-goals for the map: no Rust API invented beyond what the oracle
exposes; no renames that break traceability; no `HashMap` iteration
anywhere (see determinism rules, §6).

## 4. The parity discipline (acceptance heart)

**Gate:** byte-identical output PNGs, oracle CLI vs `prism-quant-rs`,
over the full matrix:

- 47-image smoke set (`benchmarks/m1-smoke-set/manifest.json`, read via
  its repo-relative paths) + the benchmark dice
  (`datasets/collections/prism-benchmark-image/files/PNG_transparency_demonstration_1.png`
  — 800x600 RGBA, ~86k distinct colors, so it exercises the preclip
  overflow path the smoke set's photographic items also hit);
- `--colors 256` AND `--colors 16`;
- both hidden-RGB policies (`canonicalize-black`, `preserve-mean`).

48 images x 2 color counts x 2 policies = **192 paired runs**. The
harness (`parity/parity_sweep.py`, stdlib-only, matching lab tooling
conventions) builds the Rust binary once, runs both CLIs into fresh
temp dirs, and compares output bytes pairwise (SHA-256 recorded in the
report; byte-compare is authoritative). It exits nonzero listing every
divergence. It also runs the Rust side TWICE (twin run) and requires
run-to-run byte identity — the determinism clause. A clean sweep is
`192/192 byte-identical + 192/192 twin-identical`; the summary lines go
into the T-0082 Evidence log verbatim.

**Discipline rules:**

1. Any divergence is a finding: fix the Rust, or — if the Python
   behavior is the bug — STOP, record in the Evidence log, do not
   silently diverge.
2. The harness never edits anything under `benchmarks/` or `datasets/`;
   it only reads. Outputs go to a fresh temp dir per sweep.
3. Exit statuses and stderr discipline are checked on a small error
   matrix too (usage/data/io), but exact OS-error message suffixes are
   platform text and are NOT gated (declared cosmetic divergence; the
   `io_error:`/`data_error:` prefixes and codes are gated).

## 5. Dependencies (the only judgment call)

PNG emission must be byte-identical to Python's
`zlib.compress(scanlines, 9)`. Deflate output is implementation- and
backend-dependent, so the ONLY safe choice is the same algorithm the
oracle uses: the C zlib library (Python 3.14 here reports zlib
1.2.12, runtime == compiled). Plan:

- `flate2` with `default-features = false, features = ["zlib"]` —
  the stock-C-zlib backend (miniz_oxide is NOT linked; its deflate
  output is not guaranteed identical). Pinned `=x.y.z` in Cargo.toml
  and locked in Cargo.lock, matching the lab runner's pinning style
  (`sha2 = "=0.10.9"`).
- **Spike before committing to it (step 0 of implementation):** a
  throwaway comparison of `flate2` level-9 zlib output vs Python
  `zlib.compress(data, 9)` over a battery of buffers (empty, tiny,
  repetitive, random, and real scanline payloads from smoke items).
  If bytes differ, the fallback is linking the same Homebrew zlib
  1.2.12 the oracle's Python uses (libz-sys pkg-config override);
  if THAT cannot be made identical, this plan stops and the finding
  goes to the board before any further code.
- CRC-32 (PNG chunk CRCs) is hand-rolled (small table implementation,
  pinned by golden vectors) — one less dependency.
- Inflate (decode) is exact under any correct implementation, so the
  same `flate2` backend is used for decode without parity risk.
- Every external crate gets a row in this crate's `REFERENCES.md`
  (license + exact pin + why needed) in the same commit series. The
  crate's `REFERENCES.md` lives here (not in `lab/reference/`, which
  T-0082's scope declares read/invoke-only) and cross-references the
  oracle ledger rows it inherits method provenance from.

No other dependencies: CLI parsing is hand-rolled (mirrors the Python
parser exactly, including `int()`-compatible `--colors` parsing:
ASCII-whitespace trim, optional sign, PEP-515 single underscores —
pathological but part of the CLI contract), errors are this crate's own
`Error` type.

## 6. Determinism-first translation rules (the audit checklist)

These are the Python-semantics traps, each with its Rust rule. The
reviewer should be able to grep for every one.

1. **Integer widths.** Python ints are unbounded. All pixel sums,
   masses, and accumulator arithmetic use `i64`; distances and squared
   norms use `i64` (max |premultiplied diff| = 65025, so a full
   four-channel squared distance <= 1.7e10; weighted seeding keys
   `weight * cur_d2` <= ~6e13 at smoke-set sizes — both far inside
   i64; headroom documented at each site). No `as` truncation from
   wider to narrower types except at the final, range-proven palette
   byte casts, each marked with a comment citing the bound.
2. **Rounding.** The oracle rounds half-up via
   `_round_half_up(n, d) = (2n + d) // (2d)` on NONNEGATIVE ints.
   Rust `/` on nonnegative integers floors identically. The function is
   ported verbatim with the nonnegativity precondition documented and
   debug-asserted; it is the ONLY rounding in the port (Rust's
   `.round()` and float math are banned from the pipeline — there is
   no floating point anywhere in the oracle's math, so there is none
   here).
3. **Division.** Python `//` floors; on the oracle's nonnegative
   domains Rust integer division truncates identically. Every divisor
   site in the oracle is nonnegative-by-construction; the port keeps
   those types signed (`i64`) so a future bug can never silently wrap
   to a huge unsigned value.
4. **Dict ordering.** The oracle relies on three orderings, all made
   explicit:
   - `sorted(table)` over tuple keys -> `BTreeMap<(u8,u8,u8,u8), _>`
     iteration (identical lexicographic order).
   - Insertion order (`pair_acc` first-appearance order feeds the
     `heaviest` pick and the stable `ranked` sort) -> an insertion
     `Vec` of slots plus a `HashMap` for accumulation; iteration only
     ever walks the Vec. `HashMap` iteration order is randomized in
     Rust and is NEVER used.
   - `sorted(...)` stability (`ranked` ties keep insertion order) ->
     Rust `sort_by`/`sort_by_key` are stable; the sort input is the
     insertion-ordered Vec, so tie order matches.
5. **Tie semantics.** Python `min(key=...)` returns the FIRST minimal
   element; the port uses hand-rolled scans with strict `<`
   replacement (or `min_by` with a comparator that never reports Equal
   early — hand-rolled is clearer and is what the oracle does).
   Documented at: ladder nearest-level (`d < best_d` keeps the lower
   level), farthest-point seed picks (`(-mass*d2, packed)` first
   minimal), rep assignment, `_nearest_entry` (`d2 < best_d` keeps the
   lowest index), worst-served tracking (`>` distance, or `==` and
   lower packed mean), `heaviest`/`ranked` slot ordering.
6. **Early-exit and loop bounds.** `while len(seeds) < k` with the
   `weight*cur_d2 == 0` break, the `moved` fixed-point stops, the
   ladder `updated == levels` stop, and iteration counters (including
   the 1-based `iterations` value that lands in StageNotes) are ported
   control-flow-verbatim.
7. **Single-threaded.** No threads, no rayon, no parallelism anywhere
   in phase 1; the binary is one thread plus the OS.
8. **No randomness, no timestamps, no environment dependence.** Same
   input bytes + same flags => same output bytes, on any machine, in
   any locale (the only float-free, allocation-order-free guarantee
   that makes the twin-run gate meaningful).
9. **Panic-freedom at the data boundary.** Every malformed-input path
   returns the ported `PngError`/`PrismQuantError` equivalent (exit 3),
   never a panic; the malformed-input oracle tests are ported to prove
   it. Internal invariants use `debug_assert` where the oracle uses
   comments/`assert` that cannot fire on valid flow.

## 7. Implementation order

0. Scaffold + zlib byte-parity spike (§5). STOP-gate.
1. `src/png.rs` (decode + emit + CRC + flate2 glue) with its unit
   tests, reviewed against `m1_png.py` line by line.
2. `src/quant.rs` core port with its unit tests, reviewed against
   `prism_quant.py` line by line.
3. `src/main.rs` CLI + CLI contract tests.
4. `parity/parity_sweep.py` + the 192-pair sweep; divergences handled
   per §4.
5. Gates: `cargo fmt --check`, `cargo clippy --locked --all-targets --
   -D warnings`, `cargo test --locked` (debug + release). Twin-run
   determinism. Evidence log. `task.sh done`.

Sub-agent use (protocol §4b): file-partitioned executors may draft
individual modules against this plan (e.g. `png.rs` while the lead does
`quant.rs`); they never commit — the lead reviews every diff against
the oracle and commits under `[T-0082][kimi-k2-2]`. Verification is
non-delegable: the lead re-runs every gate personally before `done`.

## 8. File inventory this phase intends to create

```
lib/prism-quant/
  PORT-PLAN.md          (this file)
  README.md             (crate purpose + v0.1-alpha label + pointer here)
  REFERENCES.md         (crate dependency ledger + inherited provenance)
  Cargo.toml            (lib + [[bin]] prism-quant-rs, edition 2024)
  Cargo.lock            (committed, --locked gates)
  .gitignore            (/target)
  src/lib.rs            (public surface, crate docs)
  src/png.rs            (m1_png.py mirror)
  src/quant.rs          (prism_quant.py mirror, minus CLI)
  src/main.rs           (prism_quant.py main mirror)
  tests/png_corpus.rs   (smoke manifest + PngSuite pairs)
  tests/quant_binding.rs(binding/distance/hidden-rgb/degenerate tests)
  tests/cli.rs          (exit codes, usage, summary line)
  parity/parity_sweep.py(the byte-parity gate, stdlib-only)
  parity/README.md      (how to run, what clean means)
```

Everything is inside the T-0082 scope (`lib/prism-quant/` only).
`lab/` is read/invoke-only; `benchmarks/` and `datasets/` are read-only
inputs to the harness; nothing else in the repo is touched.

---

# prism-quant Rust port — phase 2 plan (T-0095)

**Label: still v0.2-alpha parity work — no performance claims, no new
algorithms, no API stability.** This section is the binding plan for phase
2, committed **before any phase-2 code** as required by the T-0095 task
card. It extends the phase-1 discipline (Python is the ORACLE; byte-parity
is the acceptance heart; every §6 determinism rule still binds) to the
dither and pack surfaces. Nothing above is changed or re-opened.

## P2.1 What we are porting (the oracle), with pinned SHAs

The v0.2 integration (T-0094) folded dither and pack into the **main
`prism_quant.py` CLI**, so there is now ONE integrated surface to mirror.
Exact oracle SHAs ported (pinned; a later oracle edit invalidates parity
and must be re-verified):

- **`lab/reference/prism_quant.py` @ `8754f90c`** (v0.2.0-alpha): the
  integrated CLI and `quantize_png` orchestration — the v0.1 six-seam core
  (already ported in phase 1, unchanged) plus the opt-in dither and pack
  stages and the new flag set.
- **`lab/reference/prism_dither.py` @ `8754f90c`**: alpha-boundary-safe
  Floyd–Steinberg (`floyd_steinberg`), the no-dither baseline
  (`nearest_remap`), the exact-rational strength parse
  (`_parse_dither_strength`, T-0080), the E-0010 policies — region
  classifier (`classify_regions` + `REGION_CLASS_TABLE`, T-0085) and the
  lifted adaptive `e/(e+g)` hook (`adaptive_strength_hook`, T-0094).
- **`lab/reference/prism_pack.py` @ `25ca55d3`**: the lossless packing
  search — v1 portfolio and v2 bounded ch19 A5 search (T-0083), with
  `zopflipng` as an external subprocess to the SAME pinned binary.

Method provenance is inherited from the oracle ledgers (`prism_dither.py`
cites Floyd–Steinberg 1976 + ch05–09/19; `prism_pack.py` cites W3C PNG +
ch05/06/17/19; the E-0010 rows are in `lab/reference/REFERENCES.md`). The
port adds no new methods. Clean-room quarantine binds unchanged (no
libimagequant source, no `book/12-*`).

**Oracle-is-truth clause (unchanged):** where Rust and Python disagree the
Python is presumed correct and the port STOPS at that seam with the finding
recorded in the T-0095 Evidence log — no silent divergence.

**Amendment (T-0135, 2026-07-19) — pin vs. live oracle drift.** The pin
above (`8754f90c`) is the SHA this port was translated against; the live
`lab/reference/prism_quant.py` has since moved through `a32c060f`
("pin meaningful pack v2 outputs" — a `Summary.palette_entries`
derivation fix confined to the summary dict, never PNG output bytes; not
yet mirrored here) to `013f64cc` (T-0114's v0.3 palette-capacity
rebalance + T-0115's version bump, `v0.2.0-alpha` -> `v0.3.0-alpha`).
The T-0114/T-0115 delta is a **known, accepted parity gap** — it is
tracked, not silent: it is the sole cause of `parity/parity_sweep.py`
reporting fewer than 428/428 byte-identical cells on this pin (the
remainder is unaffected and stays byte-identical). Absorbing it is
separate, larger work (a new phase), out of scope for this crate's
declared `v0.2.0-alpha` label and for docs/lint/ops hygiene passes like
T-0135.

**Amendment (T-0137, 2026-07-19) — capacity pin refresh.** The Rust
artifact-producing default path is now refreshed through live-oracle commit
`1bc2c339` (`prism_quant.py` blob
`d043d5a2da0f7e3065322d9153799675574f1316`): T-0114's deterministic
same-alpha-zone occupancy-weighted residual seeding is mirrored in
`quant.rs`. T-0131's later Oklab path is opt-in and outside P2.2; its `srgb`
default preserves the pinned artifact path. T-0115's version/label bump and
T-0094's summary-only `palette_entries` derivation remain metadata differences
outside PNG bytes. The unchanged P2.2 matrix is again required to report
428/428 byte-identical cells and 856/856 run-identical twins against that live
pin; the prior 358/428 result is retained above as the pre-port baseline, not
current parity.

## P2.2 The declared parity matrix (acceptance heart) — declared FIRST

The gate remains **byte-identical output PNGs, oracle CLI
(`prism_quant.py`) vs `prism-quant-rs`**, now over the integrated flag
surface, run TWICE (twin) with run-to-run byte identity. Because the
integrated CLI exposes `--pack-search v1|v2` directly, v2 is reached
through the CLI — no separate driver. `--pack max` shells out to the same
pinned `zopflipng` on both sides (its output is deterministic — verified:
identical SHA across repeated runs), so max-mode parity reduces to feeding
zopflipng identical pre-optimizer bytes.

Images: `S48` = the 47-image M1 smoke set + the benchmark dice (the
phase-1 set). `S10` = a fixed representative subset declared in
`parity/parity_sweep.py` (`PHASE2_SUBSET`): fully-opaque photographic,
alpha cutout, synthetic hue/alpha ramp, flat-color icon, few-color, and
the many-color dice — spanning every alpha zone, the exact/preclip split,
and both small/large palettes. `S4` ⊂ `S10` for the costliest cells.

| # | Purpose | Images | colors | flags swept | cells |
| --- | --- | --- | --- | --- | --- |
| M1 | phase-1 regression (kept) | S48 | 256, 16 | `--dither off --pack none` (defaults) × both hidden-rgb policies | 192 |
| M2 | dither surface (pack isolated off) | S10 | 256, 16 | `--dither on` × strength ∈ {`1.0`,`0.5`,`0.25`,`0`} (uniform) + `--dither-policy adaptive` + `--dither-policy region`; plus `--dither off` baseline | 10·2·7 = 140 |
| M3 | pack surface (dither off) | S10 | 256, 16 | `--pack fast --pack-search v1`, `fast v2`, `max v1`, `max v2` | 10·2·4 = 80 |
| M4 | dither×pack interaction | S4 | 256 | `region + max v2`, `adaptive + fast v2`, `strength 0.5 + max v1`, `strength 0.25 + fast v1` | 4·4 = 16 |
| M5 | error/usage contract | 1 | — | bad `--dither`, `--dither-strength` out of range/non-decimal, `adaptive`+`--dither off`, `strength`+`adaptive`, `--pack-search` with `--pack none`, unknown option | codes only |

Total data cells: **428 paired runs × 2 sides × 2 runs**. A clean sweep is
`428/428 byte-identical + 428/428 twin-identical` on the data matrix, and
exit-code/stderr-prefix identity on M5 (exact OS-error suffixes remain
declared-cosmetic, as in phase 1). Strength values map to exact ratios via
`Decimal.as_integer_ratio`: `1.0`→(1,1) (the historical `region_hook=None`
fast path), `0.5`→(1,2), `0.25`→(1,4), `0`→(0,1). The summary lines go
verbatim into the T-0095 Evidence log.

## P2.3 Module map (Python seam → Rust module)

New modules mirror the two new oracle files one-to-one; `quant.rs`/`png.rs`
gain small, additive seams. Every public item keeps the oracle name in
snake_case and cites its oracle line range.

| Oracle (Python) | Rust (this crate) |
| --- | --- |
| `prism_dither._feature`, `_alpha_zone`, `_round_div_signed` (signed half-away-from-zero), `_eligible_by_zone`, `_nearest_index_and_distance_sq`, `_nearest_index` | `src/dither.rs` (same names) |
| `prism_dither.RegionDirective`, `_directive`, `nearest_remap`, `floyd_steinberg` (serpentine, `_KERNEL_FORWARD`, boundary/zone/region/barrier legality, no renormalization) | `src/dither.rs` (`RegionDirective`, `nearest_remap`, `floyd_steinberg`) |
| `prism_dither` E-0010: `_squared_local_gradient`, `adaptive_strength_hook`, `classify_regions` (3-pass, confluent flat-flood), `region_hook_from_classes`, `region_policy_hook`, `REGION_CLASS_TABLE`, `_EDGE_STEP_MIN/_RATIO`, `_SHADOW_ALPHA_MAX` | `src/dither.rs` (same names/constants; policy hooks return a per-pixel `Vec<RegionDirective>`) |
| `prism_dither._parse_dither_strength` (Decimal → reduced integer ratio), `_uniform_strength_hook` | `src/dither.rs` (`parse_dither_strength`; exact-decimal parse mirrored without floats — see P2.5) |
| `prism_pack._normalize_inputs`, `cleanup_palette`, `minimum_bit_depth`, `pack_index_row` (MSB-first bit packing), `_paeth`, `filter_row`, `_residual_score`, `select_row_filters`, `_serialize_row_filter_choices`, `_frequency` | `src/pack.rs` (same names) |
| `prism_pack._trial_compression_row_filters` (stateful `zlib.compressobj(9)` + `copy()` + `Z_SYNC_FLUSH` probe) | `src/pack.rs` (`trial_compression_row_filters`) over the `zprobe` FFI shim (P2.4) |
| `prism_pack` order heuristics: `_spatial_order`, `_color_locality_order`, `_alpha_partitions`, `_packed_frequency_order`, `permute_palette` (9 V2 orders) | `src/pack.rs` (same names) |
| `prism_pack._chunk`, `_encode_variant`, `_spread_positions`, `_apply_position_order`, `_local_position_moves`, `_build_v2_variants` (budget 96/20/12/16) | `src/pack.rs` (same names; `_chunk` reuses `png::crc32`) |
| `prism_pack._assert_pixel_identity`, `_observed_artifact_facts`, `_run_zopflipng` (subprocess), `pack_indexed_png` (v1/v2 × fast/max, finalist selection) | `src/pack.rs` (same names; decode via `png::decode_png`) |
| `prism_quant.quantize_candidate` (palette+indices+notes), `quantize_png` (dither/pack params, self-verify incl. `emitted pixels == remap`) | `src/quant.rs` (`quantize_candidate`; `quantize_png` extended with `DitherOptions`/`PackOptions`) |
| `prism_quant.main` (v0.2 flags, exit codes, summary) | `src/main.rs` (extended parser) |
| `test_prism_dither.py`, `test_prism_pack.py`, v0.2 `test_prism_quant.py` CLI tests | in-module `#[cfg(test)]` units beside `src/dither.rs`/`src/pack.rs` (not separate `tests/dither_binding.rs`/`tests/pack_binding.rs` files as originally planned here — see the P2.7 amendment below), `tests/cli.rs` (extended) |

No `HashMap` iteration anywhere; the pack order heuristics build explicit
`Vec` orders and `min(key=…)` becomes a hand scan / stable sort over a
tuple key exactly as in §6.5.

## P2.4 The one dependency judgment call: `trial-zlib` and `libz-sys`

`_trial_compression_row_filters` chooses each row's PNG filter by the
LENGTH of a `zlib.compressobj(level=9)` probe: `copy()` the running
compressor, `compress(record)`, `flush(Z_SYNC_FLUSH)`, measure. This is
stateful `deflateCopy` behavior that `flate2` does not expose. The only
faithful reproduction is the same C zlib via `deflateInit2_` /
`deflateCopy` / `deflate(Z_NO_FLUSH|Z_SYNC_FLUSH)`.

**Decision:** promote **`libz-sys = "=1.1.29"`** (already the exact zlib
`flate2`'s `zlib` feature links, so ONE backend stays linked) to a DIRECT
pinned dependency, and add a tiny `unsafe` FFI shim `zprobe` mirroring
`compressobj(9)` (`deflateInit2_(9, Z_DEFLATED, 15, 8,
Z_DEFAULT_STRATEGY)`), `copy` (`deflateCopy`), `compress`
(`deflate(Z_NO_FLUSH)`), and `flush` (`deflate(Z_SYNC_FLUSH)`), measuring
produced bytes. Ledgered in `REFERENCES.md` as a direct dep.

**STOP-gate spike (step 0, DONE — result recorded in the Evidence log):** a
throwaway `libz-sys` probe vs Python `zlib.compressobj(9)` over a 66-row
battery (empty/tiny/repetitive/random/near-constant "dithering-like" rows)
produced **byte-identical probe lengths AND identical filter choices, all
66/66**. So `trial-zlib` filter selection matches, and since the final IDAT
is the separate phase-1-validated `zlib.compress(scanlines, 9)`
(`flate2` level 9), whole-artifact bytes match. Had it diverged, the
declared fallback (unchanged from §5) was overriding to the same Homebrew
zlib the oracle's Python links; it was not needed. This property is also
pinned as an in-crate test (`trial_zlib_matches_frozen_python_oracle`,
`src/pack.rs` — not a separate `tests/pack_binding.rs` file as originally
planned here; see the P2.7 amendment) against a small frozen
Python-derived vector, so a future zlib bump cannot silently break it.

Everything else is float-free integer work needing no new dependency.
`zopflipng` is invoked as a subprocess to the SAME pinned binary the oracle
uses (`benchmarks/baselines/zopfli/work/zopfli/zopflipng`, arm64,
confirmed deterministic); it is a black-box tool, not a linked dependency.

## P2.5 Determinism-first additions (extends §6; the audit checklist)

Every §6 rule still binds. Phase-2-specific traps:

1. **Signed rounding.** `_round_div_signed(n, d)` rounds half-AWAY-from-zero
   on a signed numerator (`d>0`): `n<0 → -((-n + d//2)//d)`, else
   `(n + d//2)//d`. Ported verbatim as `round_div_signed(i64,i64)->i64`
   with the `d>0` precondition debug-asserted. This is DISTINCT from the
   pipeline's nonnegative `round_half_up`; the residual transport uses it
   because error is signed. No floats.
2. **Exact-rational strength.** `_parse_dither_strength` uses
   `Decimal(str).as_integer_ratio()`. The port parses the decimal string
   itself (sign/int/frac/exponent) into an exact `(i64,i64)` ratio and
   reduces by `gcd` — NO `f64`. Rejects non-finite / out-of-`[0,1]` with
   the oracle's `usage_error` (exit 2). Composition guards (adaptive/region
   forbid non-`(1,1)` strength; adaptive/region require `--dither on`) are
   mirrored in both `main` and `quantize_png`, matching the oracle's
   double-check.
3. **Feature clamp.** `floyd_steinberg` clamps adjusted features to
   `0..=65025` (`min(65025,max(0,·))`) BEFORE nearest-entry; `i64`.
4. **Serpentine + tie order.** Reverse rows mirror `dx`; nearest-entry ties
   keep the lowest palette index via ascending `eligible` + strict `<`
   (mirrors `_nearest_index_and_distance_sq`). Residual is discarded, never
   renormalized, on any illegal edge (image border, zone mismatch, region-id
   mismatch, barrier).
5. **Pack ordering ties.** Every `min(key=(…tuple…))` becomes a hand scan
   with strict `<` over a Rust tuple whose element order matches Python
   (RGBA tuples compare lexicographically as `(u8,u8,u8,u8)`; frequencies
   are negated `i64`). `sorted(...)` → stable `sort_by_key` over the same
   tuple. Selection `min(enumerate(variants), key=(len,idx))` → first
   minimal by `(len, generation_index)`.
6. **Bit packing.** `pack_index_row` writes indices MSB-first with
   deterministic zero padding for depths 1/2/4 and raw bytes at depth 8;
   ported bit-for-bit.
7. **zopflipng subprocess.** Same argv shape (`-y [-m] INPUT OUTPUT`),
   `stdin` closed, temp files, nonzero-exit / missing-output / decode /
   pixel-change → the ported `PackError` (data_error, exit 3); max mode
   never silently falls back. Finalist set = up to 3 distinct-`palette_rgba_sha256`
   pre-optimizer variants ranked by `(len, idx)`; final by
   `(optimized_len, idx)`.

## P2.6 Implementation order

0. **[DONE] `libz-sys` `trial-zlib` STOP spike** (P2.4) — PASS.
1. `src/quant.rs`: add `quantize_candidate` (palette+indices+notes) and
   thread the v0.2 dither/pack params through `quantize_png`; keep the
   existing no-dither/pack-none path byte-identical (M1 regression).
2. `src/dither.rs`: `nearest_remap`, `floyd_steinberg`, strength parse,
   uniform/adaptive/region policies — reviewed line-by-line vs the oracle.
3. `src/pack.rs`: emission + filter/order search + `zprobe` FFI + v1/v2 +
   zopflipng — reviewed line-by-line vs the oracle.
4. `src/main.rs`: v0.2 flag parser, exit codes, summary line.
5. Tests: in-module unit tests beside `src/dither.rs`/`src/pack.rs`,
   extended `tests/cli.rs` — ported from the oracle suites (landed as
   in-crate `#[cfg(test)]` modules rather than the separate
   `tests/dither_binding.rs`/`tests/pack_binding.rs` files this step
   originally named; see the P2.7 amendment).
6. `parity/parity_sweep.py`: extend to the P2.2 matrix; run the sweep;
   handle any divergence per P2.1.
7. Gates: `cargo fmt --check`, `clippy --locked --all-targets -- -D
   warnings`, `test --locked` (debug + release), twin-run parity. Evidence
   log. `task.sh done`.

Sub-agent use (protocol §4b): file-partitioned executors may draft a module
against this plan inside `lib/prism-quant/`; they never commit and never
leave scope. Verification is non-delegable — the lead re-runs every gate
and the full parity sweep personally before `done`.

## P2.7 File inventory this phase intends to create/extend

```
lib/prism-quant/
  PORT-PLAN.md          (extended: this phase-2 section)
  REFERENCES.md         (extended: libz-sys promoted to a direct dep row)
  Cargo.toml            (add libz-sys =1.1.29; new modules)
  Cargo.lock            (unchanged pins; libz-sys already present)
  src/lib.rs            (export dither, pack)
  src/dither.rs         (NEW — prism_dither.py mirror)
  src/pack.rs           (NEW — prism_pack.py mirror + zprobe FFI)
  src/quant.rs          (extend — quantize_candidate + v0.2 quantize_png)
  src/main.rs           (extend — v0.2 CLI)
  tests/dither_binding.rs (NEW), tests/pack_binding.rs (NEW)
  tests/cli.rs          (extend — v0.2 flags + error matrix)
  parity/parity_sweep.py (extend — P2.2 matrix)
  parity/README.md      (extend — what the phase-2 matrix means)
```

All inside the T-0095 scope (`lib/prism-quant/` only). `lab/` is
read/invoke-only; `benchmarks/` and `datasets/` are read-only harness
inputs; nothing else in the repo is touched.

**Amendment (T-0135, 2026-07-19) — as landed, not as planned.** The two
test files this inventory named never materialized; the oracle test
suites landed instead as in-module `#[cfg(test)]` units beside
`src/dither.rs` and `src/pack.rs` (plus `tests/adversarial_suite.rs`,
`tests/error_kind.rs`, `tests/png_corpus.rs`, and `tests/quant_binding.rs`
added later, outside this phase-2 inventory). The frozen trial-zlib
vector this plan expected in `tests/pack_binding.rs` lives instead as
`trial_zlib_matches_frozen_python_oracle` in `src/pack.rs`'s test module.
This section is left otherwise unchanged as the historical plan; it is
not a re-plan.

---

# v0.3.x opt-in parity amendment (T-0153)

**Label: faithful Rust ports of two accepted opt-ins — no default-policy
change and no new algorithm claim.** This amendment adds the accepted Oklab
quantization-space path and luma-weighted blue-noise dither path while keeping
the established sRGB/default artifact surface unchanged.

## P3.1 Pinned live oracles and artifacts

- **Oklab:** `lab/reference/prism_quant.py` at accepted commit `1bc2c339`
  (T-0131). The Rust port mirrors the E-0016 Ottosson-derived transform,
  premultiplied `(A*L,A*a,A*b,A)` feature, source-pixel assignment and
  refinement, centroid inversion, and lowest-index tie rules.
- **Luma blue-noise:** `lab/reference/prism_dither.py` at accepted commit
  `768ca92a` (T-0139). That accepted policy is not exposed by the integrated
  Python quantizer CLI, so its parity cells invoke the real standalone
  `prism_dither.py encode` oracle; the harness does not invent an adapter or
  duplicate its mechanism.
- **Masks:** the port consumes the three committed E-0017 64x64 JSON masks in
  place and verifies their bytes before parsing: R
  `8ee801878fd37cc52fbb2993fa4d7c5b4ace02f2fccc04a0c28dabf13111b0d8`, G
  `80aba5e8dc5cbef7b1c04acfc3e3b0d6193375a74ef007cf8a26d604ae2522cc`, B
  `cb2706b65c956f52369fd05ccb0c73fef52774c185cb39fffa7ff8dc79258139`.
  Masks are loaded and verified, never regenerated. Only successful loads are
  cached, so a transient read or verification failure is not hidden.

The oracle-is-truth and clean-room rules from P2.1 remain binding. The primary
method sources are ledgered in `REFERENCES.md`.

## P3.2 Extended parity matrix

The existing P2.2 matrix remains an independently reported **default** surface:
428 cells, with 428/428 Python-vs-Rust byte identity and 856/856 Rust twin-run
identity required.

| Surface | Construction | cells |
| --- | --- | ---: |
| default | Existing P2.2 M1-M4 matrix, byte-for-byte unchanged | 428 |
| Oklab | Literal mirror of all 428 default cells with `--color-space oklab`; live `prism_quant.py` oracle | 428 |
| luma-bluenoise | S10 x colors {16,256} x strength {1.0,0.5,0.25,0} x pack {fast-v1,none}, plus S4 x colors 256 x max-v1 at full strength | 84 |

The extended gate is therefore **940 unique paired cells** and **1,880 Rust
twin comparisons**. The two new surfaces contribute 512 paired cells and
1,024 twin comparisons. `parity_sweep.py` reports each surface separately as
well as the aggregate, preventing a new opt-in result from concealing a
default regression. The error matrix also covers invalid/missing color-space
values and the luma policy's `--dither on` requirement.

## P3.3 Module and semantics map

| Python seam | Rust seam |
| --- | --- |
| `prism_quant._srgb8_to_linear`, `_linear_to_srgb8`, `_oklab_from_srgb8`, `_srgb8_from_oklab`, `_oklab_feature`, `_build_oklab_features`, `_oklab_distance_sq`, `_oklab_centroid`, `_stage_refinement_oklab`, `_stage_remap_oklab` | `src/quant.rs`, preserving binary64 operation order, constants, `pow` behavior, source traversal, stable ordering, and strict-first tie replacement |
| `prism_quant --color-space {srgb,oklab}` | `src/main.rs` plus additive `*_with_color_space` library entry points; existing entry points remain sRGB-compatible wrappers |
| `prism_dither._load_bluenoise_masks`, `_bankers_round`, `luma_bluenoise_remap` | `src/dither.rs`, including exact file SHA-256 verification, 64x64 tiling, luma/chroma blend, alpha-zone eligibility, and lowest-index tie behavior |
| Python `hashlib.sha256` used by mask loading | dependency-free one-shot SHA-256 in `src/sha256.rs`, pinned by FIPS vectors and mask-digest tests |
| `prism_dither --dither-policy luma-bluenoise` | `src/main.rs` / `src/quant.rs`; strength remains composable and dither must be on |

Oklab uses Rust `f64::powf` rather than a native `cbrt`, avoids fused
multiply-adds, and retains the Python expression order so the frozen binary64
vectors agree bit-for-bit. Squared distance also uses runtime `powf(2.0)` with
an optimization barrier: Python's `delta ** 2` can differ by one ulp from
`delta * delta`, and LLVM otherwise folds the former into the latter in release
builds. Feature maps use `BTreeMap`; assignments scan in
ascending palette order and replace only on strict `<`; equal worst-served
distances use the lower packed RGBA key. Luma mask scaling uses
`round_ties_even` to reproduce Python's banker rounding (not Rust's ordinary
half-away-from-zero `round`). The existing exact-rational dither-strength
parser remains the source of amplitude ratios. The live oracle's exact-path
guard is also preserved: a preclip histogram that collapses to the color cap
must still take refinement; only an originally exact histogram may claim the
pixel-exact early return.

---

# v0.3.x adaptive-default parity amendment (T-0166)

**Label: faithful port of the accepted T-0161 switch/config surface; the
switch remains off by default and all established artifacts remain frozen.**

## P4.1 Pinned live oracle and pre-port crate

- **Live Python oracle:** `lab/reference/prism_quant.py` and
  `lab/reference/prism_dither.py` at accepted implementation commit
  `a89d6610c43629834375352c0c1df1b0b67c8678` (T-0161). The port mirrors
  `--adaptive-default off|on`, the explicit `adaptive-unit` policy, the
  explicit-strength bit, and E-0014's reduced per-unit `B / (B + N)` policy.
- **Pre-port Rust baseline:** commit
  `23736a9079dbbfb6894c26d3f633eddf7eef7d9f` (accepted T-0153 crate state;
  byte-identical to the crate at the T-0166 claim). The parity harness builds
  this tree in an isolated temporary directory and compares every established
  cell directly; it never checks out or rewrites the shared working tree.

The oracle remains truth. The E-0014 policy reuses the existing faithful Rust
port of E-0010 `classify_regions`; it adds only the oracle's class partition,
integer counts, `gcd` reduction, and uniform-strength dispatch. Float
semantics, traversal order, strict-first ties, pack ordering, and quantizer
math are unchanged.

## P4.2 Switch and composition semantics

- Omission and `--adaptive-default off` preserve the legacy default.
- `--adaptive-default on` is accepted only when no dither flag was explicitly
  supplied. It enables dither and selects predicted `adaptive-unit`.
- Explicit `--dither on --dither-policy adaptive-unit` predicts the same
  per-unit strength unless `--dither-strength` was explicitly supplied.
  Explicit `1.0` is therefore observably distinct from an omitted strength.
- The predicted ratio counts `gradient-opaque`, `gradient-alpha`, and
  `soft-shadow` as banding mass; `texture` and `flat` as grain mass; all other
  frozen classes are neutral. No counted mass yields `0/1`; otherwise the
  result is reduced by `gcd`, exactly as the oracle.
- Existing Rust entry points remain source-compatible wrappers with the switch
  off. `quantize_png_with_adaptive_default` carries the new boolean and
  explicit-strength bit without changing the old function signatures.

## P4.3 Extended parity matrix

The accepted T-0153 matrix remains intact as a separately reported 940-cell
prefix. T-0166 runs that prefix under explicit switch-off, compares every cell
to no-flag Python/Rust aliases, and directly compares Rust to the pinned
pre-port binary.

| Surface | Construction | cells |
| --- | --- | ---: |
| established default | Existing P2 M1-M4 under explicit switch-off | 428 |
| established Oklab | Existing P3 Oklab mirror under explicit switch-off | 428 |
| established luma-bluenoise | Existing P3 isolated surface under explicit switch-off | 84 |
| adaptive-default | S48 x colors {16,256} x both hidden-RGB policies, plus S10 x colors x four non-none pack modes | 272 |
| adaptive-default Oklab | Literal mirror of all adaptive-default cells | 272 |
| explicit adaptive-unit strength | S10 x colors {16,256} x strength {1.0,0.5,0.25,0} | 80 |

The extended gate is **1,564 unique paired cells** and **3,128 twin
comparisons**. It additionally performs 940 direct pre-port comparisons and
2,968 alias comparisons: no-flag versus explicit-off across the established
prefix, and switch-on versus explicit predicted adaptive-unit across the 544
switch cells. Surface-specific headlines prevent a new path from hiding an
established regression.

## P4.4 Verification gates

1. `python3 parity/parity_sweep.py` — all 1,564 pairs, 3,128 twins, 940
   pre-port comparisons, and 2,968 aliases byte-identical.
2. `cargo test --locked` and `cargo test --release --locked`.
3. `cargo clippy --locked --all-targets --all-features -- -D warnings`.
4. `cargo fmt --check`.

---

# v0.4.0 opt-in byte-exact parallelism amendment (T-0172)

**Accepted after James's v0.4.0 approval on 2026-07-20.** This amendment
replaces §6.7/P2.5's blanket single-threaded posture only for the stages named
below. The one-thread path remains the behavioral oracle and the default.
Parallel execution is an implementation schedule, never a new image recipe:
given the same source and image options, every accepted schedule MUST emit the
same PNG bytes, summary, and typed error as the one-thread path.

The proof basis is `docs/parallelism-readiness.md` (T-0134 v2). Its corrected
M2 post-merge global-spill rule and merge-order-varying differential design
are binding here; this amendment promotes those reviewed proofs rather than
re-deriving them.

## P5.1 Public schedule contract

- `--threads N` is the CLI opt-in; omission means `N=1`. `N` is a positive
  integer and is recorded as execution metadata, not artifact identity.
- Existing library entry points remain one-thread wrappers. An additive
  schedule-aware entry point accepts a bounded nonzero thread count. It does
  not silently read host CPU count or an environment variable.
- Merge order is a deterministic verification control, not a quality knob.
  The production default is a balanced tree. Differential gates additionally
  exercise forward, reverse, and two named seeded shuffle orders. A seed is
  explicit execution metadata; no entropy source, timestamp, or hash-map
  iteration may select a schedule.
- `N=1` executes the pre-amendment functions directly. It is not a one-worker
  emulation, so the oracle stays available for twins and benchmarks.
- Worker panic or join failure maps to `internal_error`; data-path failures are
  collected into index-addressed slots and returned in sequential scan order,
  preserving error kind and message. Parallel work may not weaken ch17 §31.
- `std::thread::scope` is the first implementation. No production dependency
  is added; rayon remains subject to ch17 §30.

## P5.2 Stage allowlist and honest negatives

Only these independently proven regions may use `N>1`:

| Stage | Parallel unit | Required deterministic assembly |
| --- | --- | --- |
| sRGB histogram build | contiguous pixel shards | keyed integer merge into `BTreeMap`; M1 mixed-mode conversion plus **M2 union distinct-count spill recheck at every both-exact merge** |
| sRGB refinement assignment/accumulation | sample shards inside one Lloyd iteration | integer accumulator sum, total-order worst winner, then the existing sequential palette update and iteration chain |
| sRGB remap | bin shards, then pixel shards | first-error scan and index-addressed output |
| Adam7 decode | independent passes | precomputed pass offsets and disjoint destination indices; rows inside a pass stay sequential |
| pack v1/v2 independent portfolio sweeps and max-mode finalists | generation-index slots | ranking, dedup, caps, local search, and winner folds stay sequential |

The integer histogram is shared by both color spaces, and pure per-pixel Oklab
remap may use index-addressed parallel mapping. Oklab feature-bin sums and
Oklab refinement remain sequential because their binary64 additions are not
associative; T-0134's integer-commutative proof does not cover them. Also
still sequential are zlib inflate, rows within an Adam7 pass, scanned
Floyd–Steinberg, palette-order greedy walks, v2 local/row-change search,
trial-zlib row-filter selection, farthest-point seeding, Lloyd iteration
chains, and order-defined finalist folds. A future schedule for any negative
requires its own declared proof and gate before code.

Global decisions are made only from fully merged state: exact/preclip mode
uses the merged union's distinct-key count (M2); `refine_sample` stride uses
the final sorted bin count; the exact-path guard consumes the merged
histogram; variant and finalist caps consume generation-index order.

## P5.3 Differential acceptance gate

The implementation does not land on a speed result. It lands only after:

1. Stage twins compare one-thread and parallel internal values bit-for-bit.
   Histogram twins cover exact, 32,768-key edge, 32,769-key distributed-M2,
   32,769-key concentrated-M1, and preclip fixtures. For each applicable
   fixture they sweep shard counts `{1,2,3,7,64}` and merge orders forward,
   reverse, balanced, and two recorded seeded shuffles. The distributed M2
   fixture asserts both child inputs are exact before the union crosses the
   threshold. Tie-stress, degenerate-shape, Adam7, and error twins cover the
   other enabled stages. A 20-repetition race soak requires mutual identity.
2. A Rust parallel-vs-one-thread outer gate covers **all 1,564 P4.3 cells**.
   Its committed configuration list varies both shard count and merge order;
   every configuration must report 1,564/1,564 identical and 0 divergent.
3. The unchanged Python-vs-Rust gate runs once on the one-thread oracle:
   1,564/1,564 paired, 3,128/3,128 twins, 940/940 pre-port, and 2,968/2,968
   aliases byte-identical. This prevents a mutually identical Rust regression.
4. Debug tests, release tests, clippy `-D warnings`, and rustfmt remain green.

Any divergent cell is a hard fail. No speedup can waive it. Error twins require
the same kind and message; any deliberately different first-error rule would
require another contract amendment rather than an implementation note.

## P5.4 Freeze-before-measure speed row

Performance is measured only after the amendment, implementation, fixtures,
and differential gates are committed. The speed record commits raw
wall-clock samples and their direct derivation, source hash, exact invocation,
release binary identity, macOS/arm64 host identity, and
`hw.perflevel0.logicalcpu`. It compares `--threads 1` against that P-core count
on the same representative large input with warm-up and alternating order,
reports median wall time and median-ratio speedup, and records an honest
negative if the parallel path does not win. Byte identity is rechecked for
every timed output; timing never substitutes for P5.3.

# v0.4 default-surface parity amendment (T-0193)

The final v0.4 reference surface (`lab/reference/prism_quant.py`, pinned at
commit `d62b4592`, T-0192/E-0040 — byte-identical default surface to HEAD; the
live tree only adds T-0189's opt-in fewest-colors flag, which this amendment
does not exercise) changes two omission defaults. This amendment brings the
crate to byte parity with them.

## P6.1 Adopted semantics (mirrors the pinned oracle)

- **Guarded adaptive default (T-0190/E-0038).** `--adaptive-default` gains a
  third value, `guarded`, and OMISSION resolves to it. `off` and `on` keep
  their frozen bytes (`off` = legacy no-dither; `on` = unguarded adaptive-unit).
  `guarded` runs adaptive-unit UNLESS the E-0032 Option-A structural guard
  fires, then it disables dither (reverting to the `off` bytes). The guard
  predicate is E-0032's four-decimal `opaque_frac == 0` over fully-opaque
  (alpha == 255) source pixels, computed **integer-exact**:
  `opaque_count * 20000 < total_pixels`. This is byte-faithful to CPython's
  `round(opaque_count / total, 4) == 0.0`: the exact `1/20000` boundary rounds
  UP to `0.0001` under round-half-to-even (the nearest double to `0.00005` sits
  just above it), so the comparison is strict `<`. Verified against Python
  `round` over the exact-boundary family and 500k random pairs; no float is
  used on the data path. The three known firing sites
  (`syn-shadow-inner-glow`, `syn-dither-shallow-alpha`, `w3c-alphatest`) are in
  the parity matrix.
- **Pack seams default-on (T-0192/E-0040).** Three `--pack-seam-*` flags
  (`palette-sort` = ARM-S, `memlevel` = ARM-M, `reduction` = ARM-R). On
  `--pack none` with no seam flag named, S and R default ON and M OFF; naming
  any seam flag freezes unspecified peers to E-0036 off; `--pack fast|max`
  resolves all seams off, and an explicit seam-ON with `fast|max` is a frozen
  status-2 usage error. Seam emission (`pack::seam_emit`) trials the enabled
  byte-only techniques and keeps the smallest stream that re-decodes
  pixel-identical to the baseline (identity order, 8-bit, memLevel 8), so it is
  never larger than `stage_emit`. The ARM-M memLevel-5 candidate uses a new
  one-shot `deflate_level9_memlevel` over the same vendored static zlib
  (`zprobe.rs`); memLevel 8 keeps delegating to the proven `zlib_compress9`.

## P6.2 Parity matrix and gates (acceptance heart)

- `parity/parity_v04.py parity` — the full v0.4 surface against the pinned
  oracle: adaptive-default {omit, off, on, guarded} x pack {none, fast, max} x
  seams {omit, all-off, all-on, S, R, M} x colors {256, 16}, twin runs, at
  `--rust-threads default` and `--threads 1` (T-0176 precedent). ALL IDENTICAL
  required; per-config tallies and >=3 archived frozen-flag reproductions
  emitted to `parity/T-0193-parity-*.json`.
- `parity/parity_v04.py differential` — Rust `--threads 1` vs parallel
  schedules (thread counts x merge orders) over a stratified subset that
  includes the guard-firing units (`parity/T-0193-differential.json`).
- `parity/parity_sweep.py` — the T-0166 frozen-explicit-flag regression, now
  pinned to the frozen surface (`--adaptive-default off` plus, on the pack=none
  quant path, explicit `--pack-seam-* off`) so it still reproduces the pre-port
  binary; the obsolete omission-alias (its `omission == off` invariant died
  with the flip) is retired and replaced by the adopted-surface coverage above.
- Crate gates unchanged: debug + release `cargo test`, clippy `-D warnings`,
  rustfmt, and a recorded `wasm32-unknown-unknown` release build
  (`parity/T-0193-wasm-build.json`). New unit tests cover the guard predicate
  (boundary-exact), the seam never-worse invariant + transparent-front rung,
  and the memLevel-8 FFI equivalence; new CLI tests cover guarded firing,
  seam default-on/never-larger, and the pack-seam usage errors.

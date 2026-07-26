# Parallelism readiness — per-stage byte-exact parallelizability (T-0134)

> **Status:** T-0134's v2 proof source was promoted by T-0172 into the
> binding v0.4 contract in `PORT-PLAN.md` §P5 and ch17 §33. This document
> retains the reviewed proof and its historical line citations; the binding
> schedule/API/gate text now lives in those amendment sections.

**Provenance.** Seeded by the T-0119 tri-review: kimi F7
(`reviews/2026-07-19/kimi.md:642`) and its §2.4 "Parallelism readiness —
better than the plan assumes" (`reviews/2026-07-19/kimi.md:332-362`),
adopted by the tri-review synthesis (`reviews/2026-07-19/SYNTHESIS.md:90-92`:
"the crate's integer-commutative discipline makes several stages provably
byte-exact-parallelizable; currently recorded nowhere").

**Revision history.**
- *v1 (2026-07-19, kimi-k3-5):* pinned to committed tree `adb6c71d`
  (T-0110). Cross-review by openai-gpt5-37: **FAIL**, two findings —
  (R1) the `build_bins` shard-merge proof omitted the case where every
  shard is locally under `EXACT_BIN_LIMIT` but the *global* distinct-key
  count exceeds it (the merge never re-applied the exact→preclip
  transition, so a sharded run could wrongly stay in exact mode);
  (R2) the differential gate varied shard counts but not *merge order*,
  which a fixed-order merge implementation could hide bugs behind.
- *v2 (2026-07-19, kimi-k3-5, this revision):* §3.1 merge rule corrected
  (rule M2 — post-merge global-spill recomputation; the claim survives,
  now with a named required condition), a global-threshold audit of all
  neighboring proofs added (§3.6), the differential gate now varies merge
  order explicitly (§5.2) and pins the distributed-excess fixture (§5.4),
  and **every line cite re-verified against the new committed tree
  `d6e60ca8`** (T-0133's error-kind refactor, landed and review-passed at
  `cb119f23` — the working tree is clean; cites are what you read today).

**Citation basis.** All `file:line` cites are the committed tree as of
`d6e60ca8` (post-T-0133; T-0110's error-path fixes included), re-read
2026-07-19 at board HEAD `cb119f23`. Relative to v1's `adb6c71d` pin the
refactor shifted lines by up to ~50 (pack.rs `encode_variant` 770→722);
function names, control flow, and arithmetic are unchanged (T-0133's
review evidence: stderr byte-identical vs the pre-refactor binary).

## 1. What "byte-exact-parallelizable" means here

A stage is **byte-exact-parallelizable** iff a parallel schedule exists
whose output bytes equal the sequential implementation's for every input.
Three properties, all already present in this crate, do the heavy lifting:

- **(P1) Integer-commutative accumulation.** Every histogram/sum in the
  crate is `i64` `+=` (`add_pixel`, quant.rs:206-216). Addition of
  integers is associative and commutative, so a reduction's result is
  independent of operand order — the property float pipelines never get
  (kimi §2.4, kimi.md:335-337).
- **(P2) Pure per-element maps with deterministic tie-breaks.** Selection
  scans use strict `<` improvement, i.e. first-minimal wins and ties keep
  the lowest index (`nearest_entry`, quant.rs:755-794, strict `<` at 768;
  `nearest_index_and_distance_sq`, dither.rs:150-168). Each element's
  result is a pure function of the element and read-only tables — no
  scan-state.
- **(P3) Deterministic result assembly.** Bins emit in `BTreeMap`
  sorted-key order (quant.rs:273-288; the map *is* the oracle's
  `sorted(tuple-key)`, doc at 230); per-pixel results write into
  index-addressed slots; variant ranking carries explicit
  generation-index tie-breaks (pack.rs:1211-1212). Assembly order is
  therefore recoverable from content, not from scheduling luck.

The standing determinism rules that make these auditable are PORT-PLAN
§6 (tie-break inventory, §6.5 at PORT-PLAN.md:210-218) and §6.8
(no randomness/timestamps/env dependence, PORT-PLAN.md:226-229).

**Warning shape for all proofs below:** the one thing P1–P3 do *not*
give for free is a **global threshold or counter evaluated mid-scan**
(the v1 gap, R1). Wherever the sequential code makes a control-flow
decision from a global quantity (distinct-key count, sample stride,
iteration count, variant budget, finalist cap), a sharded schedule must
re-evaluate that predicate on the *merged* global state, not infer it
from shard-local state. §3.6 audits every instance.

## 2. Per-stage verdicts

| # | Stage (entry point) | Verdict | Basis |
|---|---|---|---|
| A | Histogram build (`build_bins`) | **Parallel-safe, conditioned on merge rule M2** (post-merge global-spill check) | P1 + keyed merge; §3.1 |
| B | Remap (`stage_remap`) | **Parallel-safe** (trivial) | P2 + P3; §3.2 |
| C | Refinement assignment + accumulation (per `stage_refinement` iteration) | **Parallel-safe per iteration**; the fixed-point iteration chain is sequential | P1 + P2; §3.3 |
| D | Adam7 pass decode (`decode_png` pass loop over `defilter`) | **Parallel-safe across passes (≤7-way)**; rows within a pass are not free | P3 + disjoint writes; §3.4 |
| E | Pack variant evaluation (portfolio sweeps + zopfli finalists) | **Parallel-safe for the sweeps**; the v2 local search is not | purity + indexed slots; §3.5 |

## 3. Proof sketches

### 3.1 Stage A — histogram build: `build_bins` (quant.rs:231-290)

**Claim.** Sharded histograms + keyed merge produce bins byte-identical
to the sequential scan, including the exact→preclip transition —
**provided the merge re-applies the global spill predicate (rule M2
below).** Without M2 the claim is false (R1): 32,769 globally distinct
colors distributed across ≥ 2 shards so every shard holds ≤ 32,768
would leave every shard in exact mode, no operand would ever be
preclip, and a naive merge would return exact bins where the
sequential scan returns preclip bins.

**Evidence.** Per pixel, the scan updates one of two
`BTreeMap<(u8,u8,u8,u8), [i64;8]>` tables via `add_pixel`
(quant.rs:236-245 exact, 260-266 preclip; `add_pixel` body 206-216 is
eight commutative `i64` `+=`). Mode starts exact and flips when a new
distinct key would exceed `EXACT_BIN_LIMIT` (32768, quant.rs:47;
insert-while-`len < LIMIT` at 243, flip at 250): the exact table is
then converted into preclip keys and merged (quant.rs:247-258), with
the in-source comment "the merge is pure keyed accumulation, so
iteration order is irrelevant" (248-249). Emission is sorted-key order
with per-bin derived fields computed from the sums (quant.rs:273-288).

**The parallel schedule and its merge rules.** Each shard maintains
(mode, table) under the *same* local spill rule as the sequential scan
(a shard can only spill if the global distinct count exceeds the limit,
since shard distinct ≤ global distinct). Merging two shard states:

- **M1 (mixed).** If either operand is preclip, convert any exact
  operand to preclip first — per-key pure conversion, the same one the
  sequential code applies at quant.rs:251-257 — then merge keyed sums.
  (Correct mode: a preclip operand means its sub-multiset already
  exceeded the limit, so the union certainly does.)
- **M2 (both exact — the R1 fix).** Merge the two exact tables by
  keyed-sum addition into the union table, then **re-apply the spill
  predicate to the union**: if the union's key count >
  `EXACT_BIN_LIMIT`, convert the union to preclip exactly as at
  quant.rs:251-257. The check is on the *merged union's* key count, not
  the sum of operand sizes — heavy key overlap can make the union
  smaller than the sum.

**Proof sketch.** Invariant: after any sequence of shard scans and
M1/M2 merges, each node's state equals (distinct(sub-multiset) >
`EXACT_BIN_LIMIT`, mode-appropriate keyed sums over the sub-multiset).
Base: a shard scan is the sequential algorithm on its sub-multiset, so
the invariant holds (mode flips exactly when local distinct crosses the
limit; table content is keyed sums, order-free by P1). Step: given two
nodes satisfying the invariant, (i) if either is preclip, the merged
distinct count exceeds the limit, M1 converts the exact side (a pure
per-key map) and keyed-sum merge yields preclip sums over the union —
invariant preserved; (ii) if both are exact, the union keyed-sum table
is exact sums over the union multiset, and M2's union-count check
decides mode exactly as the sequential predicate does, converting
losslessly when it fires — invariant preserved. Conversion commutes
with keyed-sum merge because `preclip_key` (quant.rs:218-225) is a
pure function of the key and sums add. By induction the root state is a
pure function of the *whole pixel multiset* — independent of shard
boundaries, shard count, and merge order/tree. The sequential result is
the same function of the same multiset (mode: flip-at-crossing, quant
.rs:243-250; table: keyed sums, converted at most once, 251-258).
Emission order comes from the `BTreeMap` (P3), not the merge. ∎

**Caveats for implementers.** (a) `HashMap`-based shard tables are fine
internally, but the merge must land in a `BTreeMap` (or be sorted by
key) before emission; `HashMap` iteration order is randomized and would
violate P3 if it leaked into bin order (PORT-PLAN §6 item 4,
PORT-PLAN.md:202-209, bans this outright). (b) The M2 check must fire
on the union *key count*, re-evaluated at every both-exact merge — a
distributed excess can cross the limit at any level of the merge tree.
(c) Getting mode wrong is doubly expensive: the exact-path decision
`bins.len() <= colors` (quant.rs:554-563) consumes the histogram
immediately downstream, so a wrong mode changes the entire pipeline,
not just the histogram.

### 3.2 Stage B — remap: `stage_remap` (quant.rs:926-959)

**Claim.** Trivially parallel over bins, then over pixels.

**Proof sketch.** The per-bin loop inserts `bin.key → nearest_entry(...)`
into `assignment` (quant.rs:934-938). `nearest_entry` is a pure function
of the bin's premultiplied mean, its zone, and the read-only palette
tables (755-794; strict `<` first-minimal at 768, deterministic zone
fallback at 776-793) — P2. Map construction order is irrelevant because
the map is lookup-only afterwards (932-933). The per-pixel loop
(946-957) is a pure table lookup per pixel (exact key at 948-949,
preclip key at 952-955) pushing into an index-addressed `Vec` — shard
pixels, write into indexed slots, done (P3). Error paths
(`missing`-key typed error, 944-945; transparent-without-entry,
776-779) are per-element; a parallel version must declare its
error-equivalence rule — see §5. ∎

### 3.3 Stage C — refinement assignment: `stage_refinement` (quant.rs:801-920)

**Claim.** Within one Lloyd iteration, assignment (813-821), sum
accumulation (823-830), and worst-served tracking (834-851) are
parallel-safe. The iteration chain itself (810-918) is inherently
sequential; the per-entry update scan (859-912) stays sequential and is
negligible (≤256 entries).

**Proof sketch.** The sample is a deterministic stride over sorted bins
(`refine_sample`, quant.rs:408-414, fed from `init.bins` at 806) — a
pure function of the *fully merged* histogram. **Threshold-audit note
(R1 class):** the stride is `ceil(len / REFINE_SAMPLE_CAP)` over the
global bin count (cap 4096, quant.rs:53) — a parallel `build_bins` must
be fully merged *before* sampling; per-shard sampling with local
strides would select a different sample and diverge silently. Per
iteration: `assign[i] = nearest_entry(sample[i], …)` over read-only
`entries_premult` (812-821) — per-sample pure (P2). `acc[j] += …`
(823-830) is commutative keyed accumulation (P1). `worst` is a per-zone
fold selecting max `d2` with a total tie-break (`d2 >` or `d2 ==` and
lower packed mean, 839-850) — max over a totally-ordered key is
order-independent, so shard-local winners merge by the same comparison.
The centroid update per entry (903-911) is pure given `acc`; the
`moved` flag (909, 915) is a per-entry boolean OR-reduced — logical OR
is commutative, so parallel per-entry evaluation preserves the stop
condition exactly. The blocking sequential structure is the fixed-point
loop itself: iteration N+1 reads iteration N's palette (812), and the
iteration *count* lands in `StageNotes` (quant.rs:919, 1005) and the
CLI summary, so truncating or reordering iterations would change
observable output. Within the per-entry update scan, `zone_counts` is
decremented as entries are dropped (875-879) — a scan-state over
palette order; leave it sequential (≤256 iterations of trivial work). ∎

### 3.4 Stage D — Adam7 pass decode: `decode_png` pass loop (png.rs:887-909) over `defilter` (png.rs:562-640)

**Claim.** Passes decode independently: up to 7-way parallel for Adam7
input (1-way for non-interlaced), byte-exact. Rows *within* a pass are
sequential (see §4).

**Proof sketch.** Pass geometry is a pure function of the header
(`pass_geometry`, png.rs:459-484; `ADAM7_PASSES` vs single pass at
460-464, empty passes dropped at 479-481). The only inter-pass
dependency in the loop is the `offset` cursor into the inflated buffer
(886, 891) — but each pass's byte count is `ph * (1 + row_bytes(pw))`,
an arithmetic function of geometry alone (the same terms summed for the
expected-size check at 868-874), so all pass offsets are computable up
front with no data dependence. Each pass's `defilter` call starts from
a fresh zero `prev` row (570) and touches only its own slice.
Conversion (`row_samples`, `convert_row`) is a pure per-row function of
the row and the parsed PLTE/tRNS/header tables (png.rs:647, 693-700).
Writes are disjoint by construction: pass pixel sets partition the
image (`pixels[base + column * dx]`, 900-907; the coverage check at
914-918 asserts the partition is total). Disjoint writes + pure
per-pass compute + arithmetic offsets ⇒ a per-pass parallel schedule
reproduces the same pixel vector (P3). (Cross-review verified the
offset arithmetic and the disjoint writes directly.) ∎

### 3.5 Stage E — pack variant evaluation (pack.rs)

**Claim.** The v1 portfolio sweep (pack.rs:1180-1201), the v2 initial
sweep (917-930), and the max-mode zopfli finalist evaluations
(1237-1239) are parallel-safe. The v2 local search and row-change
search are **not** (§4); the finalist ranking/dedup/cap is a
deliberately sequential fold (below).

**Proof sketch.** `encode_variant` (pack.rs:722-805) is a pure function
of its arguments — palette/indices/row-filters/strategy in, PNG bytes +
self-check out (the independent decode + pixel-identity verification,
779-793) — with no shared mutable state between calls (P2). Both sweeps
push results in a fixed generation order (nested loops over declared
strategy arrays: `ORDER_STRATEGIES` 45-51 / `FILTER_STRATEGIES` 43;
`V2_ORDER_STRATEGIES` 53-63 / `V2_FILTER_STRATEGIES` 65-73). Generation
order is load-bearing downstream: `min_variant_index` is first-minimal
by length, ties resolved by scan order = generation index
(1043-1052); max-mode ranking sorts by `(len, generation index)`
(1211-1212); finalist dedup keeps first occurrence in that ranked order
(1218-1230). A parallel sweep must therefore write results into
**pre-indexed slots** (variants[i]), never push-on-completion — then
every downstream tie-break sees identical input (P3).
**Threshold-audit note (R1 class):** finalist selection is a
first-N-distinct fold with a global cap (`V2_ZOPFLI_FINALIST_LIMIT` = 3,
pack.rs:79, enforced at 1224-1229) over the ranked order — its result
*is* order-defined, so it stays a sequential scan over ≤ 96 variants
(trivial cost); only the per-finalist zopfli evaluations parallelize.
The zopfli finalists are independent subprocesses with per-call unique
temp dirs (pid + `AtomicU64` counter, pack.rs:1086-1092 — already
concurrency-safe) and the winner is first-minimal by optimized length
(1241-1246). ∎

### 3.6 Global-threshold audit (the R1 error class, swept across every proof)

Every global count/threshold/counter that a sharded schedule could
evaluate on shard-local state instead of merged state, and its
disposition:

| Global quantity | Where | Disposition |
|---|---|---|
| `EXACT_BIN_LIMIT` distinct-key spill | quant.rs:243-250 | **M2** — re-evaluated on merged union at every both-exact merge (§3.1) |
| `refine_sample` stride `ceil(len/4096)` | quant.rs:408-414, cap at 53 | Sample only from the **fully merged** sorted bins; per-shard sampling forbidden (§3.3) |
| Exact-path decision `bins.len() <= colors` | quant.rs:554-563 | Sequential consumer of the merged histogram; correct given M2 (§3.1 caveat c) |
| Refinement `moved` stop flag | quant.rs:909, 915 | OR-reduction over per-entry flags — commutative, safe (§3.3) |
| `zone_counts` drop guard | quant.rs:875-879 | Scan-state over palette order — stays sequential (§3.3) |
| Iteration count → `StageNotes` | quant.rs:810-918, 1005 | Fixed-point chain — sequential by design (§3.3) |
| v2 variant budget + early-exit counters | pack.rs:75, 936-937, 993-995, 1034-1039 | Inside the named-sequential local search (§4) — no parallel rule needed |
| Finalist dedup + cap (first-N-distinct) | pack.rs:79, 1218-1230 | Order-defined fold over ≤96 items — stays sequential (§3.5) |
| Adam7 pass offsets (prefix sums) | png.rs:868-874, 886-891 | Arithmetic in geometry, no data dependence — precomputable (§3.4) |
| `min_variant_index` / zopfli winner scans | pack.rs:1043-1052, 1241-1246 | First-minimal folds over indexed slots — deterministic given §3.5's slot discipline |

## 4. The named NOT-parallelizable stages (blocking operation)

| Stage | Blocking operation | Nature |
|---|---|---|
| `inflate` (png.rs:500-557) | zlib/deflate stream state — each block's decode depends on the rolling dictionary/bitstream position | Inherently sequential entropy decode. (Parallel-inflate tricks change nothing observable but are out of scope here.) |
| `defilter` row chain *within* a pass (png.rs:572-638) | `prev` row carry (570, 637) for Up/Avg/Paeth; intra-row left recurrence `recon[i - bpp]` for Sub/Avg/Paeth (589-591, 604, 614) | Sequential per pass (cross-review verified both dependencies directly). A diagonal wavefront is byte-exact-possible (each row reads only the row above) but is real engineering, not free — kimi §2.4 lists it as such (kimi.md:354-356). |
| `floyd_steinberg` (dither.rs:254-302) | Serpentine residual transport: each cell reads `residual[position]` (264-269) accumulated from producers' `+=` writes (292-297); `chosen` (269) feeds the error of later cells | NOT order-independent as scanned. **Provably byte-exact under a DAG-exact schedule**: the producer set per cell is *static* (geometry + zone/barrier legality, dither.rs:276-290, all known before any value is computed), residual contributions commute (integer `+=`), and `clamp(source + residual)` is deterministic in the final sum — so any schedule computing each cell after all its producers (diagonal wavefront with handoff channels) reproduces every value. Possible, not free (kimi.md:349-353). |
| Palette-order greedy walks: `spatial_order` (pack.rs:457-473), `color_locality_order` (511-533), `packed_frequency_order` walk (586-…) | Each pick depends on all prior picks: `placed_sum` over the placed set (`spatial_key`, pack.rs:484) / the `last` entry (514-518) | Inherently sequential greedy. (Their *inputs* — `frequency` 401-407, `adjacency` 419-439, `cooccurrence` 567-585 — are commutative histograms and parallel-safe.) |
| v2 local search: position moves (`build_v2_variants`, pack.rs:939-996) and row-change search (998-1033) | Hill-climb state: each candidate reuses `current_row_filters` (950) and improves against `variants[current]` (959-967); row search mutates `row_choices` on improvement (1028-1031); the early-exit counters (936-937, 993-995) make even the *set of variants generated* path-dependent | The search trajectory IS the output. Sequential by design. |
| Trial-zlib row-filter selection (`trial_compression_row_filters`, pack.rs:358-396) | Rolling deflate stream state: each row's five candidate costs are measured on a **copy of the live compressor** after all previous rows (`compressor.copy()`, 376-379), and the chosen record advances the retained compressor | Sequential zlib state chain; row k's costs are a function of rows 0..k (cross-review verified). (Contrast `select_row_filters`' non-zlib strategies, 287-323: those read only `previous` — an input row — and are per-row parallel-safe.) |
| `fit_rgb_reps` farthest-point seeding (quant.rs:457-477) | Greedy: each new seed updates `cur_d2` for all items (471-476); pick N+1 depends on seeds 0..N | Inherently sequential. (Its Lloyd polish, 480-518, is per-iteration parallel-safe like §3.3.) |
| `alpha_ladder` 1-D Lloyd (quant.rs:365-397) | Fixed-point iteration chain (`updated == levels` stop, 393-395) | Sequential across iterations; each iteration's assignment/centroid pass is parallel-safe but tiny (≤256 buckets) — not a target. |
| `classify_regions` pass 2 flood fill (dither.rs:499-513) | Graph traversal shape | Result is **confluent** (in-source doc, dither.rs:473-474): the Flat set is the union of identical-color 4-connected components containing a seed — order-independent and reproducible by parallel union-find. Sequential *implementation*, parallel-safe *result*. Passes 1 (457-471) and 3 (515-629) are per-pixel pure (P2). |

Also recorded: `stage_palette_init`'s pair accumulation
(quant.rs:583-631) is commutative **but** the `pair_order`
first-appearance insertion order (576-578; this insertion-order
contract is itself documented in PORT-PLAN §6 item 4,
PORT-PLAN.md:202-209) feeds the stable-sort tie-break at 722-724
(`ranked.sort_by_key`, stability documented at 722-723). A parallel
version must reconstruct first-appearance-in-sorted-key order per slot
(e.g. merge keeping the minimum bin-ordinal per slot) — a stated merge
rule, not a blocker, since bins are scanned in sorted-key order and the
slot's first appearance is a deterministic function of the bin set.

## 5. Differential-test gating design (the gate any threading PR must pass)

No parallel implementation lands without its differential evidence
committed **with it** (the PORT-PLAN declared-first discipline, §P2.2 at
PORT-PLAN.md:323, applied to schedules). Layers, cheapest first:

1. **Stage-level twin tests (in-module, `#[cfg(test)]`).** The parallel
   stage is invoked beside the existing sequential function on identical
   inputs; outputs compared for **bit equality** (bins: key+all 8 sums+
   zone; palettes: entry sequence; index maps: full `Vec`; PNG bytes:
   full buffer). Requires the sequential path to remain available — new
   code selects schedule by parameter/feature, never deletes the
   reference path.
2. **Shard-count × merge-order sweep.** Each twin test runs shard
   counts **1, 2, 3, 7, 64** (odd and oversubscribed counts expose
   remainder/boundary bugs), and for **each** shard count runs merge
   schedules: **forward** (shard 0..n in order), **reverse** (n-1..0),
   **balanced pairwise tree**, and **≥2 deterministic shuffled
   permutations** (seeded, seeds recorded in the test). Bit equality is
   required across every (shard count × merge schedule) pair — R2:
   shard sweeps plus repetition do *not* exercise merge order when an
   implementation merges in a fixed order, and M2-class bugs are
   precisely merge-order/-shape sensitive. Shard count 1 is the
   degenerate case by design: there is no merge (all schedules
   coincide), the single shard is the sequential scan itself — it
   cross-checks the parallel harness against the sequential reference
   (and, for the 32,769-distinct fixture in §5.4, exercises the local
   spill path, never M2).
3. **Race soak.** The parallel configuration runs **20 repetitions**
   per fixture/schedule; all outputs must be mutually byte-identical.
   Catches nondeterministic schedules that a single lucky run hides.
4. **Fixture classes** (all already generatable in-tree):
   - exact-path images (≤256 distinct colors — exercises the exact table
     and the ≤colors early return, quant.rs:554-563);
   - preclip-forcing images (>32768 distinct colors from a
     deterministic PRNG-free generator, e.g. structured gradients —
     crosses the exact→preclip transition, quant.rs:247-258);
   - transition-edge images (exactly 32768 and 32769 distinct colors).
     **The 32769 fixture must exist in two constructions** (R1):
     *distributed* — unique colors round-robined across the image so
     that at every swept shard count **≥ 2** each shard's local
     distinct count stays ≤ 32768 and only the merged union crosses
     the limit (the only construction that exercises M2 — one whose
     excess lands in a single shard never tests the both-exact union
     check) — and *concentrated* — the excess inside one shard of a
     ≥ 2 split (exercises M1's convert-then-merge). The ≥ 2 scoping
     on the distributed construction is load-bearing: at shard count
     1 the single shard *is* the global table, necessarily holds all
     32,769 distinct keys, and spills locally, so a 1-shard run
     exercises the sequential spill path (the baseline the sweep
     compares against, §5.2), never M2. The M2 assertion must check
     the *preconditions*, not just the output bytes: before the tested
     merge, assert both child states are exact-mode (no local spill
     occurred) — an output-only check cannot distinguish a genuine M2
     union conversion from an accidental local spill masking as one;
   - Adam7-interlaced PNGs (PngSuite `basi*` set, already vendored) vs
     their non-interlaced twins — byte-equal pixels across schedules;
   - tie-stress synthetics: equal-distance palette candidates,
     equal-mass pair slots (exercises first-minimal scans,
     quant.rs:768, and first-appearance/stable-sort semantics,
     quant.rs:576-578, 722-724);
   - degenerate shapes: 1×1, 1×N, N×1, empty-region dither directive
     mixes (barrier on/off per region — static-DAG coverage for any FS
     wavefront work).
5. **Error-path equivalence.** On invalid inputs the parallel build
   must return an error of the **same kind and message** (post-T-0133
   the crate carries typed error kinds — `Error::internal`/`data`/`io`
   etc. — so "same class" is mechanically checkable); byte-comparison
   applies to the success path. First-error vs any-error must be
   *declared* in the implementation note (the sequential code returns
   the first error in scan order; a parallel map may surface a
   different legitimate error — a declared, review-visible divergence,
   not a silent one).
6. **Pipeline-level outer gate.** The existing self-checks stay on:
   `encode_variant`'s independent decode + pixel-identity check
   (pack.rs:779-793) and `quantize_png`'s re-decode verification
   (quant.rs:1137-1158). The oracle parity sweep
   (`parity/parity_sweep.py`; 428/428 recorded by T-0110's evidence) is
   re-run over the parallel build as the final gate — parallel-vs-
   sequential Rust twins catch schedule bugs; the sweep catches
   semantic drift.
7. **Dependency discipline.** First implementations use `std::thread`
   scoped threads over shards (no new dependency). `rayon` is a §30
   dependency-admission decision (book ch17 §30; kimi F7, kimi.md:642)
   and is not assumed by any proof above.

## 6. Contract interactions (what a future amendment must touch)

- `PORT-PLAN.md` §6.7 (single-threaded rule, PORT-PLAN.md:224-225) must
  be amended per-stage, citing this note's proofs — the note is the
  evidence base, not the authorization.
- ch17 §31 (no-panic at the data boundary): thread joins add a new
  panic surface (a panicking worker poisons the join); any
  implementation must map worker failure to the stage's typed error,
  preserving the inventory's discipline.
- §32 candidate-set API (kimi F2/F3 context): stage-level parallelism
  is orthogonal to and compatible with candidate-level parallelism; do
  not conflate the two in one amendment.

## 7. Bottom line

**Provably parallel-safe today (proofs above, differential gate §5
designed):** histogram build (`build_bins`, sharded tables + keyed
merge, **conditioned on merge rule M2** — the post-merge global-spill
recomputation added after cross-review found the v1 proof incomplete,
§3.1); remap (`stage_remap`, per-bin then per-pixel, §3.2); refinement
assignment + accumulation within each Lloyd iteration (§3.3); Adam7
decode across passes (≤7-way, §3.4); pack portfolio sweeps and zopfli
finalist evaluation with indexed-slot assembly (§3.5); plus the
commutative histogram inputs of the palette-order heuristics and the
per-pixel region-classification passes 1/3 (§4). Every global threshold
these stages touch is audited in §3.6.

**Not parallel-safe (named with their blocking operation):** zlib
`inflate` (stream state); `defilter` rows within a pass (`prev` carry +
intra-row recurrence); `floyd_steinberg` as scanned (serpentine
residual transport — byte-exact only under a DAG-exact wavefront, real
engineering); the greedy palette-order walks (`placed_sum`/`last`
dependence); the v2 local and row-change searches (hill-climb
trajectory is the output); trial-zlib row-filter selection (rolling
deflate state); farthest-point seeding and both Lloyd fixed-point
chains (iteration dependence); and the order-defined finalist
first-N-distinct fold (sequential by design, trivial cost).

## 8. T-0172 v0.4 implementation outcome

T-0172 enabled the smallest proved useful subset: integer histogram shards
with M1/M2 keyed merges, integer sRGB assignment/accumulation inside each
refinement iteration, and sRGB bin/pixel remap. The fixed-point iteration
chain and palette update remain serial. Oklab feature construction,
refinement, and remap remain serial as one unit so no binary64 reduction or
parallel-only feature-table seam is introduced. Adam7 pass decode and pack
portfolio/finalist evaluation were not enabled in this amendment; their
proofs remain available for a later measured implementation. Dither, zlib,
palette-order, local-search, and finalist-fold negatives remain unchanged.

The committed outer gate compared each one-thread output with five schedules
over the 1,564-cell P4.3 matrix:

| Threads | Histogram merge order | Identical | Divergent |
|---:|---|---:|---:|
| 2 | forward | 1,564/1,564 | 0 |
| 3 | reverse | 1,564/1,564 | 0 |
| 7 | balanced | 1,564/1,564 | 0 |
| 8 | shuffle seed 1,592,594,802 | 1,564/1,564 | 0 |
| 8 | shuffle seed 3,235,823,838 | 1,564/1,564 | 0 |
| **Total** | **five schedules** | **7,820/7,820** | **0** |

The independent Python-oracle gate remained 1,564/1,564 paired,
3,128/3,128 deterministic twins, 940/940 pre-port, and 2,968/2,968 aliases,
with zero divergences. Debug and release each passed 137 tests; clippy with
`-D warnings` and rustfmt were clean.

Performance was measured only after the implementation and gates were frozen
at commit `f4598419e8d76533a4363477fae11de08860455e`. On the Apple M1 Max host's
8 logical P-cores, the 1,600×1,600 pilot alpha-mask fixture measured a
0.1475444165 s one-thread median and 0.0852479590 s 8-thread median: a
**1.7307677302× median-ratio speedup**. The row used three warm-ups per
configuration and 20 timed pairs in alternating order; all 40 timed outputs
had SHA-256 `8501f2ff8590827ba45f14945414703fef8deb4df618764ef23d020edf40ef3b`.
Raw samples, source and binary hashes, exact invocation, host identity, and
the direct derivation are committed in `parity/T-0172-speed.json`.

# PNG resource admission policy

`pngprism` rejects a PNG when any one of these ceilings is crossed. Rust
(`src/png.rs`) and the Python reference (`lab/reference/m1_png.py`) use the same
integer constants and the same error text.

| Resource | Ceiling (default) | Configurable? | Rejection point |
|---|---:|---|---|
| Compressed input file / byte slice | 268,435,456 bytes (256 MiB) | fixed | Descriptor size before payload read, plus an authoritative `limit + 1` bounded read; first instruction of slice decode |
| Width or height | 32,768 pixels | fixed | IHDR parse |
| Total pixels | 67,108,864 pixels (64 Mi-pixels) | `--max-pixels N` | IHDR parse, before IDAT parsing/inflation |
| Aggregate filtered scanlines | 536,870,912 bytes (512 MiB) | derived from pixel ceiling | IHDR parse, before IDAT parsing/inflation |

The default pixel ceiling is 64 Mi-pixels (2× the historical 32 Mi): it covers
50–64 MP cameras and 8K, at a measured ~9 GB peak footprint (see below). The
separate **fixed** dimension ceiling rejects pathological skinny images and is
the absolute allocation backstop — a 64 Mi-pixel square is 8192×8192, so
32,768/side remains a valid independent guard, and the widest admissible image
(32,768 × 32,768 = 1 Gi-pixel) bounds allocation even when `--max-pixels` is
raised very high.

The scanline ceiling exists because PNG's accepted native formats range from
one-bit gray to 16-bit RGBA (8 bytes/pixel), and Adam7 adds a filter byte to
every pass row. It is **derived** as (active pixel ceiling) × 8 bytes/pixel —
the widest native pixel — so a 16-bit-RGBA image admitted by the pixel ceiling
is admitted by the scanline ceiling too, up to the per-row filter-byte margin.
Because it is derived, the single `--max-pixels` lever scales both admission
tests together; the default 512 MiB is exactly 64 Mi × 8. (The historical
256 MiB scanline ceiling was likewise 32 Mi × 8 — this re-derivation preserves
that coupling.)

All limit arithmetic is checked before inflation or canonical-pixel
allocation. File reads are bounded on the open descriptor, not just by an
advisory path metadata check. The CLI retains one bounded source snapshot for
both candidate decode and never-worse fallback; it does not reread a second
256 MiB copy.

## Configuring the pixel ceiling (`--max-pixels N`)

`--max-pixels N` overrides the default pixel ceiling **up or down** for one
invocation. It remains a HARD BOUND: `N` is checked at IHDR before any
inflation or allocation, by every decode in the process (source admission and
the pipeline's own self-verification re-decode alike, since the override is a
process-wide value read at decode time). The derived scanline ceiling scales
with it (`N` × 8 bytes/pixel), so the single lever governs both tests.

- A 64 GB-Mac user can raise it (e.g. `--max-pixels 134217728` for a 128 MP
  panorama); a memory-constrained user can lower it below the default.
- Invalid values are **usage errors (exit 2)** in both implementations, with a
  byte-identical diagnostic: `0`, negative, and non-numeric are rejected
  (`--max-pixels must be a positive integer` / `must be an integer`), as is a
  missing value (`--max-pixels needs a value`). This differs from `--colors`,
  where an out-of-range integer is a data error (exit 3); for `--max-pixels`,
  ≤ 0 is a usage error because it can never name a valid ceiling.
- Astronomically large values are accepted (Rust clamps to `i64::MAX`; Python's
  arbitrary-precision `int` keeps the exact value) — the difference is not
  observable, because the fixed 32,768/side dimension ceiling caps any real
  image at 1 Gi-pixel, so any `--max-pixels` above that is equally non-binding.
- **A user who raises `--max-pixels` past their available RAM owns that
  outcome.** The no-OOM guarantee holds at or below the *active* ceiling; it
  does not promise that an arbitrarily raised ceiling fits in host memory (see
  the bounded-guarantee statement below).

## Measured memory cost of the default ceiling (64 MP encode)

To validate the 64 Mi-pixel default against the 16 GiB Apple-Silicon baseline,
a real 8192×8192 (67,108,864 px = exactly 64 Mi) 8-bit RGB source was encoded
with the release binary under `/usr/bin/time -l` (single conversion, arm64,
32 GiB host). Values are the representative (highest stable) of three runs:

| Metric | Bytes | ≈ | Per-pixel |
|---|---:|---:|---:|
| maximum resident set size | 9,578,283,008 | 8.92 GiB | 142.7 B/px |
| peak memory footprint | 9,449,102,488 | 8.80 GiB | 140.8 B/px |

Both figures land at **~9 GiB**: resident memory is essentially equal to the
peak footprint here (not a fraction of it), and both **match the T-0209 model
of ~142 B/px** almost exactly (142.7 / 140.8 measured). The three RSS runs were
tightly clustered (9,577.4–9,578.3 MB); an earlier single low RSS reading
(5.26 GiB) was a measurement outlier and has been corrected. Both are within the
~9 GB prediction and below a 12 GB concern threshold, so the 64 Mi default is
safe on the common 16 GiB configuration. Wall clock ~51 s.

The honest number a user budgets against for a 64 MP default encode is
therefore **~8.9 GiB of real resident memory** (9.58 GB) — workable on
16 GiB, but not the ~5 GiB the earlier row implied. This is the cost users opt
into at the default ceiling; raising `--max-pixels` scales it roughly linearly
in pixel count.

## Working-set interpretation

These are admission ceilings, not a claim that peak resident memory equals one
of the table values. The Rust decoder deliberately retains the source while
parsing copied chunk payloads, joining IDAT, materializing filtered scanlines,
defiltering rows, and building canonical pixels. Near several simultaneous
ceilings, decoder peak can be on the order of 1–2 GiB before later quantizer,
dither, and pack vectors are counted. The Python oracle has much larger object
overhead: a canonical RGBA tuple plus its outer-sequence slot is roughly tens
of bytes per pixel, so a 64 Mi-pixel lab decode can consume many GiB before
quantizer working storage.

Accordingly, the default ceiling is intended for a single conversion on the
project's 16 GiB-or-larger Apple-Silicon development baseline (the measured
64 MP peak footprint above is ~9 GB). The Python implementation remains a lab
oracle, not the memory-efficient shipping path. Concurrent conversions — and
any `--max-pixels` value raised above the default — require the operator to
budget their aggregate RSS.

The ceilings remove attacker-controlled *unbounded* allocation requests. They
cannot guarantee successful allocation under arbitrary host memory pressure,
nor do they prove that every admitted combination has a fixed small RSS.

**Bounded no-OOM guarantee.** The tool does not OOM on input **at or below the
active pixel ceiling** — the 64 Mi-pixel default, or whatever `--max-pixels`
sets. It is NOT a claim of "no OOM on any input": a user who raises
`--max-pixels` past their host memory owns that outcome, and an allocation
failure outside these declared bounds is not converted into a false "safe for
any input" claim.

## Gate coverage

`tests/caps.rs` and `lab/reference/test_m1_png.py` independently construct
spec-valid, exactly sized, highly compressible streams just beyond the
dimension, pixel (64 Mi), and decoded-scanline (512 MiB) ceilings. They assert
early typed-data rejection and Rust/Python CLI diagnostic parity. The
compressed-input test uses a spec-valid sparse PNG beyond 256 MiB, proving
descriptor preflight without allocating or physically writing a 256 MiB test
buffer. Pure preflight tests also pin that equality at the new policy
boundaries remains admitted (8192×8192 at the pixel boundary; 8192×8191 16-bit
RGBA at the scanline boundary).

The `--max-pixels` knob is covered without encoding a real multi-GB image: the
pure header-validation arithmetic is driven up and down directly (over/under an
explicit ceiling, both impls), and the CLI end-to-end is exercised with a small
image by moving the ceiling around it (`tests/caps.rs`
`max_pixels_flag_admission_knob_is_a_hard_bound_both_impls` +
`max_pixels_flag_invalid_values_reject_identically_both_impls`;
`test_prism_quant.py` `PrismQuantCliContractTests`). Both implementations reject
invalid `--max-pixels` values (0/negative/non-numeric/missing) identically at
exit 2. The default-reject / raised-admit of a genuinely >64 Mi image is proven
at the header-validate level (no allocation).

# T-0212 — inflate amplification reproducer

`generate_repro.py` builds `corpus/bomb-8x8-rgb8-64mib-zeros.png`
deterministically: an 8x8 IHDR (color type 2 / RGB8, so IHDR declares
`expected scanline bytes = 200`) whose single IDAT chunk is a valid zlib
stream that decompresses to 64 MiB of zero bytes (ratio ~335544x on the
*claimed* budget; ~1028x on the *compressed IDAT size* — matches the
T-0207 finding's "65 KiB IDAT, 1028:1" framing). `--check` verifies the
committed fixture matches the generator byte-for-byte.

This is the T-0207 finding (independently reproduced by its reviewer,
claude-sonnet-46): `inflate` in `src/png.rs` materialised the FULL IDAT
output before checking it against IHDR's expected total, so this input
would inflate to ~67 MB RSS before a clean `data_error` fired. Not a
panic/OOB — the §31 no-panic contract held throughout — but a needless
resource-amplification property T-0212 bounds.

## Before (pre-T-0212 fix, at the freeze commit)

<!-- The freeze commit predates this repository's history; see PROVENANCE.md. -->

```
$ /usr/bin/time -l ./target/release/pngprism \
    tests/amplification/corpus/bomb-8x8-rgb8-64mib-zeros.png /tmp/out-repro.png
data_error: cannot decode tests/amplification/corpus/bomb-8x8-rgb8-64mib-zeros.png: decoded 67108864 scanline bytes, expected 200
        0.57 real         0.14 user         0.00 sys
            69337088  maximum resident set size   (66.1 MB)
            68567424  peak memory footprint        (65.4 MB)
exit=3
```

## After (post-T-0212 fix)

```
$ /usr/bin/time -l ./target/release/pngprism \
    tests/amplification/corpus/bomb-8x8-rgb8-64mib-zeros.png /tmp/out-repro-after.png
data_error: cannot decode tests/amplification/corpus/bomb-8x8-rgb8-64mib-zeros.png: decoded more than 200 scanline bytes (deflate stream exceeds IHDR-declared size)
        0.25 real         0.00 user         0.00 sys
             2146304  maximum resident set size   (2.05 MB)
             1327416  peak memory footprint        (1.27 MB)
exit=3
```

66.1 MB -> 2.05 MB maximum RSS (~32x reduction; the residual ~2 MB is
process/runtime baseline, not decode-path allocation). The fix: `inflate`
in `src/png.rs` now drops its scratch-buffer request to a 1-byte probe
(`OVERFLOW_PROBE`) once cumulative decoded output has already reached the
IHDR-declared `expected` total, instead of continuing to request
SCRATCH_CAP-sized (8 MiB) chunks. Any single byte of output beyond
`expected` is conclusive proof the stream is oversized (nothing more needs
to be discovered), so decode fails the instant that byte appears. The
error message also changed for this specific case (from the post-hoc
"decoded 67108864 scanline bytes, expected 200" to
"decoded more than 200 scanline bytes (deflate stream exceeds
IHDR-declared size)", since the exact final count is no longer
materialized to report) — no test pinned the old string for this path
(grepped for "67108864" repo-wide: only descriptive/doc mentions, not test
assertions). The one test that DOES pin a similar-looking message,
`decompressed_length_mismatch_rejected` ("decoded 4 scanline bytes,
expected 7"), exercises the *truncated* (less-than-expected) case, which
this fix does not touch — verified unaffected.

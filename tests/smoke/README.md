# The vendored smoke-set subset

The 24 images from the 47-image M1 smoke set (`tests/smoke_manifest.tsv`) whose
licenses permit redistribution, copied here so the CLI, quantizer-binding and
corpus suites run outside the original research tree.

Each file is named `<manifest id>.png` and is a **byte-identical** copy of its
in-tree original — `MANIFEST.sha256` carries the digests, and they are the same
digests already pinned in the `sha256` column of `tests/smoke_manifest.tsv`.
`tests/smoke_vendor.rs` checks that both ways round (see below).

## Why 24 and not 47

| Source | Files | Rights | Here |
| --- | --- | --- | --- |
| synthetic corpus | 12 | **CC0-1.0** — the project's own generated corpus | **Yes** |
| `datasets/collections/kenney-packs/` | 12 | **CC0 1.0** — per-pack `License.txt`, identical grant in all five packs | **Yes** |
| `datasets/collections/libpng-org-legacy-test-images/` | 9 | No license located; bare copyright notices only (tier U-internal) | No |
| `datasets/pilot-v0/packages/` | 5 | Per-item admission records, not a blanket redistribution grant | No |
| `datasets/benchmark/kodak/` | 4 | No primary license text; the hosting page's "unrestricted usage" is recorded as the **host's inference**, not a Kodak grant (tier R-internal) | No |
| `datasets/collections/the-light-16m-colors/`, `pmt-colorspace-gamma-tests/`, `w3c-png-alpha-test-pages/` | 5 | No license located (tier U-internal) | No |

The rights column is not a summary written here — it is what
`datasets/README.md` records for each set, from the T-0005 rights taxonomy. A
set is vendored only on an explicit grant. "Probably fine" is not a grant, and
the four Kodak images are the clearest case: widely redistributed, no license we
could locate.

## What runs where

- **In the research tree**, every suite resolves the **in-tree original** first,
  so all 47 images are exercised exactly as before. Nothing about accepted
  evidence changes.
- **Outside it**, the 24 here are used and the other 23 are skipped. The count
  is **asserted**, not merely printed: `available_image_count_matches_the_environment`
  in `tests/smoke_vendor.rs` checks 47 in-tree / 24 outside and fails naming both
  numbers. Printing alone was the rejected first cut — `cargo test` swallows
  stderr on a passing run, so the note vanished exactly when someone was trusting
  the green tick. The suites still print their coverage for anyone running with
  `--nocapture`. A suite that quietly tested half of what its name implies is the
  claims defect this project exists to avoid.

## Integrity

`tests/smoke_vendor.rs` enforces, always: this directory contains **exactly**
the 24 expected ids, and every file's SHA-256 matches its `smoke_manifest.tsv`
pin. In the research tree it additionally checks each in-tree original against
the same pin — so a copy here and its original cannot drift apart without a test
failure, in either direction.

To change the set: update `tests/smoke_manifest.tsv` and re-copy, in one commit.

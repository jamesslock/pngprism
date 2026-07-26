# The vendored Python oracle

Verbatim copies of the four `lab/reference/` modules that the `pngprism` CLI
path executes. `tests/caps.rs` runs this oracle as a subprocess and asserts the
Rust port agrees with it.

## Why these files are here

`pngprism` is a Rust port whose **behavioral oracle is Python**. Under the
project's parity rule, a copied implementation without its differential test is
not acceptable — so when the crate is extracted to its own repository
the oracle has to travel with it. Otherwise `tests/caps.rs` dies at
the repository boundary and we have forked a brain across a repo split, which is
the exact failure the rule exists to prevent.

## Why only four files

The full static import closure of `prism_quant.py` is 12 modules (528 KB), but
**the CLI path executes only these four**. The other eight
(`m1_evidence`, `m1_metrics`, `m1_quantizer`, `m1_report`, `m1_run`,
`run_bundle_v0`, `canonical_json_v0`, `identity_v0`) are lab evidence and
reporting plumbing, reachable only through `prism_dither.build_candidate` /
`generate_scorecard` — scorecard entry points the CLI never calls, which import
their dependencies lazily inside the function body.

Verified by executing this four-file set standalone, outside the research tree,
across the CLI surface (default, `--dither`, `--dither-policy luma-bluenoise`,
`--pack fast`, `--pack max --pack-search v2`, `--color-space oklab`,
`--max-pixels` valid and invalid): no `ModuleNotFoundError`, all exits are
legitimate CLI outcomes.

**Consequence to know:** `prism_dither.py` is vendored *whole*, so it still
contains those scorecard entry points, and calling them here would fail on the
missing lazy imports. That is deliberate. Stripping them would make this copy
differ from the lab's, and then the parity test would be validating a **fork**
rather than the oracle — trading a real correctness guarantee for cosmetics.

## These are verbatim — do not edit them

`oracle_pins.rs` enforces this two ways:

1. **Always:** each file's SHA-256 must equal its frozen pin, so an accidental
   edit to the vendored copy fails the suite.
2. **In the research tree only:** each file must be byte-identical to its
   `lab/reference/` original. Outside the tree that check skips (there is
   nothing to compare against). This is the anti-fork guard — it makes drift
   between the lab oracle and the shipped oracle a test failure at the moment it
   is introduced.

To update the oracle: change it in `lab/reference/`, re-copy here, and update
the pins in `oracle_pins.rs` in the same commit.

| File | SHA-256 |
| --- | --- |
| `prism_quant.py` | `5525f72702f31054c0343562b3c40c415ff13097d21b08ebe83acdaa2fdcb5af` |
| `prism_dither.py` | `cf1bb02b24b4d4acfcbffd2d6c6927c1c44c6e122f9065262c201bdc195ec8aa` |
| `prism_pack.py` | `46bae1168cd698bcf3cad75062c6a0be623b1f59da25583e7b5a40bedbef32f5` |
| `m1_png.py` | `ad57130b51cdf3427e9520219bc591a03bc738575b386c8c6825515a6270c26c` |

Python puts a script's own directory on `sys.path`, so running
`python3 tests/oracle/prism_quant.py …` resolves the three siblings with no
`PYTHONPATH` setup.

License: these are Project Prism's own work and carry the crate's
`MIT OR Apache-2.0` terms.

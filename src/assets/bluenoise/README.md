# Embedded E-0017 blue-noise dither masks

The three 64×64 void-and-cluster rank masks consumed by
`dither::luma_bluenoise_remap`, embedded into the binary via `include_bytes!`
(see `BLUENOISE_MASK_BYTES` in `../../dither.rs`).

## Why these live here

They were previously read from a research-tree path
at **runtime**, via a `CARGO_MANIFEST_DIR`-relative path that escaped the crate
directory. That works only inside the Prism research tree; any consumer holding
just the crate — a crates.io download, a vendored copy, `cargo install` — would
fail on the first dither call. Embedding makes the crate self-contained.

The integrity contract is unchanged: the SHA-256 pins in
`BLUENOISE_MASK_SPECS` are still verified before first use, now against the
embedded bytes rather than the file contents. `embedded_bluenoise_payloads_match_frozen_pins`
asserts the same thing directly so a bad re-vendoring names the offending
channel.

## Provenance

Produced by a void-and-cluster mask generator written for this project (method
after Ulichney 1993; see [`REFERENCES.md`](../../../REFERENCES.md)). These files
are verbatim copies of that generator's frozen output — do not hand-edit them.
Regenerate, re-vendor, and update the digest pins in `dither.rs` in the same
change, or the integrity check will reject them.

- **Method:** Robert Ulichney, *The Void-and-Cluster Method for Dither Array
  Generation*, Proc. SPIE 1913, 1993. The method is published prior art; the
  mask files themselves are in-repo original work (our generator, our seeds).
- **Parameters** (from `masks-manifest.json`): size 64, kernel radius 5,
  sigma 1.5, initial ones fraction 0.1, weight scale 1024.
- **Per-channel seeds:** r = 20260719, g = 20260720, b = 20260721.

| File | Channel | Seed | SHA-256 |
| --- | --- | --- | --- |
| `bluenoise-64-seed20260719-r.json` | r | 20260719 | `8ee801878fd37cc52fbb2993fa4d7c5b4ace02f2fccc04a0c28dabf13111b0d8` |
| `bluenoise-64-seed20260720-g.json` | g | 20260720 | `80aba5e8dc5cbef7b1c04acfc3e3b0d6193375a74ef007cf8a26d604ae2522cc` |
| `bluenoise-64-seed20260721-b.json` | b | 20260721 | `cb2706b65c956f52369fd05ccb0c73fef52774c185cb39fffa7ff8dc79258139` |

`masks-manifest.json` is carried alongside for provenance and is **not**
embedded or parsed by the crate.

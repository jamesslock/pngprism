# Changelog

Notable changes to pngprism. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Versioning policy

pngprism is **pre-1.0**, and follows Cargo's pre-1.0 semver convention:

- **`0.x.0`** — may change the library API, the CLI surface, or **output
  bytes**. Treat a minor bump as potentially breaking.
- **`0.x.y`** — bug fixes and additions that change neither the API nor the
  bytes produced for a given input and flag set.

**Output bytes are part of the contract.** This crate's defining property is
that the same input, flags and build produce the same bytes. A change that
alters output for an existing flag combination is therefore a **breaking**
change and gets a minor bump, even if it only makes files smaller. Such changes
are called out under a `Changed (output bytes)` heading so anyone pinning
digests can find them without reading the whole entry.

A 1.0 release is not scheduled. It would mean committing to API stability, and
that is a promise worth making only once the API has been used by someone other
than its author.

## Unreleased

### Performance

No output bytes change — stated first, because on this crate that is the
question a performance entry raises. Byte-identity is verified across thread
counts and on every reference conversion.

- The `--pack max` zopflipng finalists and the v2 packing search's variant
  generation now run across threads, bounded by `--threads`. Results are
  reassembled in generation order before any tie-break sees them.
- `classify_regions` no longer runs a full linear nearest-distance scan per
  pixel to answer an exact-match question: 4-20% off a default conversion,
  depending on the image.
- New `pack_indexed_png_with_parallelism`. `pack_indexed_png` is unchanged and
  remains sequential — the plain entry point does not spawn threads behind a
  caller's back.

### Added

- `SECURITY.md` — how to report a vulnerability privately, what is in scope,
  and which versions are supported. It ships in the crate tarball, so it
  reaches crates.io with this release rather than only being visible on GitHub.
- A measured comparison grid in `README.md` — four images through pngprism and
  pngquant at both default and maximum effort, with output sizes, digests and
  observed times. The images live in `docs/gallery/` and are excluded from the
  crate tarball; the README references them by URL. Generated in the research
  lab per release, not hand-written.

## 0.5.0 — first public release

The first version published outside the research program it was written for.
The version number reflects that development history rather than a sequence of
public releases; 0.1 through 0.4 were internal.

### Added

- PNG palette quantization to 1–256 colors, in sRGB or the opt-in Oklab
  perceptual color space (`--color-space oklab`).
- Floyd–Steinberg dithering with selectable policies, including a blue-noise
  mask variant, and a guarded adaptive default.
- A lossless indexed-PNG packer with row-filter and palette-order search, plus
  an optional `zopflipng` post-pass (`--pack max`).
- Opt-in parallelism (`--threads N`) that is byte-identical to the serial path
  regardless of merge order.
- `--report json` for machine-readable results.
- A never-worse guarantee: output is never larger than input; if no candidate
  wins, the input is passed through verbatim.
- Explicit resource ceilings on compressed size, dimensions, total pixels and
  decoded scanline bytes, so malformed input fails cleanly rather than
  exhausting memory.

### Notes for this release

- The library import name changed from `prism_quant` to `pngprism` before
  publication, so the package, library, binary and CLI strings are all one
  name. This affects no published version, because there is none before this.
- `zopflipng` is an optional external tool, needed only by `--pack max`. The
  test suite skips those cases when it is absent rather than failing.
- Two test fixture sets are smaller than the ones used in development, for
  licensing reasons; the suite asserts which case it is running under. See
  `PROVENANCE.md`.

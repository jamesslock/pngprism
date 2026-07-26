# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Use GitHub's private vulnerability reporting on this repository:
[Report a vulnerability](https://github.com/jamesslock/pngprism/security/advisories/new).
It creates a private thread visible only to the maintainer, and it is the
preferred channel because it needs no email address from either side.

This is a single-maintainer project. Expect an acknowledgement within about a
week rather than within hours, and no guaranteed fix window — that is a
limitation worth stating plainly rather than promising a response time that
would not be honoured. If a report is credible and I cannot fix it promptly, I
will say so and coordinate disclosure rather than leave it silent.

## Supported versions

pngprism is pre-1.0. Only the latest published `0.x` line receives fixes;
there are no long-term support branches. Older versions are not patched, and
yanking is used rather than backporting when a released version is unsafe.

## What is in scope

This crate's job is to read PNG files, which in practice means reading files
someone else produced. Its stated contract is that **any input, however
malformed, produces either valid output or a typed error — never a panic.** A
counterexample to that is a bug, and is in scope here:

- A panic, abort, or crash reachable from a decoded input.
- Memory-safety failures, including anything found in the vendored zlib path.
- Unbounded memory or CPU consumption not stopped by the configured resource
  ceiling, where a modest input drives disproportionate allocation or runtime.
- Writing outside the intended destination path, or clobbering a file the
  documented publication rules say should be left alone.
- Any way to make the library execute code from its input.

A crash on malformed input is treated as a security bug even without a
demonstrated exploit, because the contract promises it cannot happen.

## What is out of scope

- Producing output larger than expected, or worse compression than a
  competitor. That is a quality bug — open a normal issue.
- Findings against `zopflipng`, `pngquant`, or any other external binary this
  project measures against or invokes as a subprocess. Report those upstream.
  Note that `zopflipng` is optional and is run as a subprocess, never linked.
- Dependency advisories already visible to `cargo deny` — CI checks those on a
  schedule, so a report adds nothing unless the crate's own use of the
  dependency is what makes it exploitable.
- Anything requiring an attacker who already controls the machine running
  pngprism.

## Testing this yourself

The repository carries two fuzz targets under `fuzz/`. If you are hunting for
crashes, they are the intended entry point:

```bash
cargo +nightly fuzz run decode_png         # the parser, on arbitrary bytes
cargo +nightly fuzz run quantize_pipeline  # the full quantize/dither/pack path
```

A reproducing input file is by far the most useful thing to attach to a report.
If the input cannot be shared, the image's dimensions, colour type and bit
depth plus the exact flags used are usually enough to reconstruct the case.

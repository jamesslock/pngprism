<!--
Thanks for contributing. The checkboxes below are not ceremony — each one maps
to a property this crate is expected to hold, and the first one is the reason
this template exists at all. See CONTRIBUTING.md for the reasoning.
-->

## What this changes, and why

<!-- If it fixes an issue, link it. If it changes behaviour, say what a user
     would notice. -->

## Output bytes

This crate's defining property is that the same input, flags and build produce
the same bytes. People pin those digests, so changing them breaks them.

- [ ] **This change does not alter the bytes produced for any existing flag
      combination.**
- [ ] This change **does** alter output bytes. Details below.

If it does alter output, fill this in:

- Which inputs and flag combinations change:
- Why the new bytes are correct:
- [ ] Recorded in `CHANGELOG.md` under a `Changed (output bytes)` heading
- [ ] Takes a minor version bump, not a patch

A change that makes files *smaller* is still a breaking change here.

## Checks

- [ ] `cargo test --locked --all-targets` passes
- [ ] `cargo clippy --locked --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all -- --check` is clean

## The Python oracle

`tests/oracle/` is the authority on behaviour, not a legacy artifact.

- [ ] Not touched.
- [ ] Touched — and `tests/oracle_pins.rs` is updated in the same commit, with
      an explanation of why the oracle itself was the thing that was wrong.

## Publication surface

- [ ] This PR adds no new top-level file or directory.
- [ ] It adds one, and `.github/publication-surface.txt` is updated to match.
      This repository is public and permanent, so that line is the record of a
      deliberate publication decision.

## Anything reviewers should know

<!-- Known limitations, follow-up work, things you were unsure about. Saying
     "I wasn't sure about X" is more useful than leaving it to be discovered. -->

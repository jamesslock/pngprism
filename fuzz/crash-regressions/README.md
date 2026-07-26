# Permanent crash-regression corpus (T-0207).
#
# Any input libFuzzer flags (panic / OOB / timeout) is MINIMISED with
# `cargo fuzz tmin` and committed here, one file per finding, so `cargo test`
# (see ../tests/fuzz_regressions.rs) replays it forever against BOTH the
# decoder and the full pipeline on the pinned RELEASE toolchain — no nightly,
# no libFuzzer needed to prove a fix stays fixed. Empty is the correct state
# when no finding was reproduced.

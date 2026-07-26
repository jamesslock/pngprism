//! Minimal `unsafe` FFI shim reproducing Python's `zlib.compressobj(level=9)`
//! for the `trial-zlib` row-filter heuristic (`prism_pack._trial_compression_row_filters`).
//!
//! The heuristic chooses each PNG row's filter by the LENGTH of a stateful
//! deflate probe: `copy()` the running compressor, `compress(record)`,
//! `flush(Z_SYNC_FLUSH)`, measure. `flate2` does not expose `deflateCopy`, so
//! this shim calls the same C zlib (`libz-sys` — the exact backend `flate2`'s
//! `zlib` feature already links, so one zlib stays linked). The
//! parameterization matches `compressobj(9)`: `deflateInit2_(level=9,
//! method=Z_DEFLATED, windowBits=15, memLevel=8, strategy=Z_DEFAULT_STRATEGY)`.
//!
//! STOP-spike-verified byte-identical to Python over a 66-row battery (probe
//! lengths AND filter choices; `PORT-PLAN.md` §P2.4). The property is also
//! pinned as an in-crate test.
//!
//! Safety: the `z_stream` is held as `MaybeUninit` and only ever touched
//! through raw pointers (never materialized as a value with the invalid
//! null function-pointer fields), so no undefined behavior arises; zlib reads
//! the zeroed `zalloc`/`zfree` as "use the default allocator".

use libz_sys::{
    Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH, Z_NO_FLUSH, Z_OK, Z_STREAM_END, Z_SYNC_FLUSH,
    deflate, deflateCopy, deflateEnd, deflateInit2_, z_stream, zlibVersion,
};
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::os::raw::c_int;

const CHUNK: usize = 1 << 16;

/// The actual linked zlib's own version string (`zlibVersion()`), queried
/// at runtime rather than assumed. Ops hygiene (tri-review kimi F10):
/// the shim previously hardcoded a literal `"1.2.12\0"` for the version
/// argument `deflateInit2_` uses to sanity-check header/library agreement
/// (zlib itself only checks the leading major-version digit plus the
/// `z_stream` struct size — both are process-invariant here — so the
/// hardcoded literal never actually caught anything; it just risked
/// silently drifting from whatever's really linked). Querying the
/// running library instead can never go stale, and this same value is
/// what any evidence/provenance harness should record alongside emitted
/// bytes for fable finding #3 ("byte-determinism is machine-contingent
/// on system zlib") — see `runtime_version_is_observable` below.
///
/// Safety: `zlibVersion()` returns a pointer to a static string owned by
/// the linked zlib itself, valid for the process lifetime; never freed.
pub(crate) fn runtime_version() -> String {
    let ptr = unsafe { zlibVersion() };
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// A running level-9 deflate compressor mirroring `zlib.compressobj(9)`.
pub struct Deflater {
    stream: Box<MaybeUninit<z_stream>>,
}

impl Deflater {
    /// `zlib.compressobj(level=9)` (default method/wbits/memLevel/strategy).
    pub fn new() -> Result<Self, i32> {
        let mut boxed = Box::new(MaybeUninit::<z_stream>::zeroed());
        let ptr = boxed.as_mut_ptr();
        // Pass the actually-linked library's own version pointer (queried,
        // not a hardcoded literal) as the version-check argument zlib's
        // `deflateInit2_` compares against itself.
        let r = unsafe {
            deflateInit2_(
                ptr,
                9,
                Z_DEFLATED,
                15,
                8,
                Z_DEFAULT_STRATEGY,
                zlibVersion(),
                std::mem::size_of::<z_stream>() as c_int,
            )
        };
        if r != Z_OK {
            return Err(r);
        }
        Ok(Deflater { stream: boxed })
    }

    /// `compressor.copy()` — `deflateCopy` into a fresh stream.
    pub fn copy(&self) -> Deflater {
        let mut boxed = Box::new(MaybeUninit::<z_stream>::zeroed());
        let dest = boxed.as_mut_ptr();
        let src = self.stream.as_ptr() as *mut z_stream;
        let r = unsafe { deflateCopy(dest, src) };
        // ch17 §31 surviving panic site (internal invariant, not data path):
        // `self` is always a live stream this module itself initialized via
        // `deflateInit2_`, so `deflateCopy` can only fail on allocation
        // failure (Z_MEM_ERROR) — never on row/PNG content, which this
        // function never inspects.
        assert_eq!(r, Z_OK, "deflateCopy failed: {r}");
        Deflater { stream: boxed }
    }

    /// Run one `deflate` call and return the number of output bytes produced.
    /// Production callers retain only the length; the differential harness can
    /// capture the same call's bytes without maintaining a second FFI path.
    fn run(&mut self, input: &[u8], flush: c_int, mut captured: Option<&mut Vec<u8>>) -> usize {
        let ptr = self.stream.as_mut_ptr();
        let mut produced = 0usize;
        let mut buf = vec![0u8; CHUNK];
        unsafe {
            (*ptr).next_in = input.as_ptr() as *mut _;
            (*ptr).avail_in = input.len() as u32;
            loop {
                (*ptr).next_out = buf.as_mut_ptr();
                (*ptr).avail_out = CHUNK as u32;
                let r = deflate(ptr, flush);
                // ch17 §31 surviving panic site (internal invariant, not data
                // path, debug-only): `deflate`'s return code depends on
                // stream/parameter validity, never on the row BYTE VALUES
                // (arbitrary 0..=255 content is exactly what deflate is
                // built to accept), so no adversarial pixel/row content can
                // trigger this.
                debug_assert!(r >= 0, "deflate returned error {r}");
                let written = CHUNK - (*ptr).avail_out as usize;
                produced += written;
                if let Some(output) = captured.as_deref_mut() {
                    output.extend_from_slice(&buf[..written]);
                }
                if flush == Z_FINISH {
                    if r == Z_STREAM_END {
                        break;
                    }
                } else if flush == Z_NO_FLUSH {
                    // Stop once input is consumed and output is not saturated.
                    if (*ptr).avail_in == 0 && (*ptr).avail_out != 0 {
                        break;
                    }
                } else {
                    // Sync flush complete once output is not saturated.
                    if (*ptr).avail_out != 0 {
                        break;
                    }
                }
            }
        }
        produced
    }

    /// `probe.compress(data)` / retained-state advance — `deflate(Z_NO_FLUSH)`.
    pub fn compress(&mut self, data: &[u8]) -> usize {
        self.run(data, Z_NO_FLUSH, None)
    }

    /// `probe.flush(Z_SYNC_FLUSH)`.
    pub fn flush_sync(&mut self) -> usize {
        self.run(&[], Z_SYNC_FLUSH, None)
    }

    /// Capture `compress(data)` bytes for the differential FFI harness.
    #[cfg(feature = "zlib-ffi-harness")]
    pub fn compress_capture(&mut self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        self.run(data, Z_NO_FLUSH, Some(&mut output));
        output
    }

    /// Capture `flush(Z_SYNC_FLUSH)` bytes for the differential FFI harness.
    #[cfg(feature = "zlib-ffi-harness")]
    pub fn flush_sync_capture(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        self.run(&[], Z_SYNC_FLUSH, Some(&mut output));
        output
    }

    /// Capture `flush(Z_FINISH)` bytes for the differential FFI harness.
    #[cfg(feature = "zlib-ffi-harness")]
    pub fn finish_capture(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        self.run(&[], Z_FINISH, Some(&mut output));
        output
    }
}

impl Drop for Deflater {
    fn drop(&mut self) {
        unsafe {
            deflateEnd(self.stream.as_mut_ptr());
        }
    }
}

/// One-shot level-9 deflate at an explicit `memLevel`, mirroring Python's
/// `zlib.compressobj(9, zlib.DEFLATED, 15, mem_level, Z_DEFAULT_STRATEGY)`
/// followed by `compress(data) + flush()` (equivalently `compress(data,
/// Z_FINISH)`). The E-0036 ARM-M pack seam (`_seam_emit_config`) needs
/// `memLevel = 5` alongside the baseline `memLevel = 8`; `flate2` exposes no
/// `memLevel` knob, so this reuses the same linked C zlib the trial-zlib probe
/// already depends on. `mem_level = 8` reproduces `zlib.compress(data, 9)`
/// byte-for-byte (pinned by test below), so callers keep using the proven
/// `zlib_compress9` for the baseline and reach here only for `mem_level != 8`.
///
/// Safety: identical discipline to [`Deflater`] — the `z_stream` lives as a
/// zeroed `MaybeUninit` touched only through raw pointers; `deflate` accepts
/// arbitrary 0..=255 content, so no input can trip the internal invariants.
pub(crate) fn deflate_level9_memlevel(data: &[u8], mem_level: c_int) -> Vec<u8> {
    let mut boxed = Box::new(MaybeUninit::<z_stream>::zeroed());
    let ptr = boxed.as_mut_ptr();
    let init = unsafe {
        deflateInit2_(
            ptr,
            9,
            Z_DEFLATED,
            15,
            mem_level,
            Z_DEFAULT_STRATEGY,
            zlibVersion(),
            std::mem::size_of::<z_stream>() as c_int,
        )
    };
    // Internal invariant (not the data path): parameters are compile-time
    // constants plus a caller-fixed memLevel in zlib's valid 1..=9 range, so
    // init can only fail on allocation exhaustion.
    assert_eq!(init, Z_OK, "deflateInit2_ failed: {init}");
    let mut output = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    unsafe {
        (*ptr).next_in = data.as_ptr() as *mut _;
        (*ptr).avail_in = data.len() as u32;
        loop {
            (*ptr).next_out = buf.as_mut_ptr();
            (*ptr).avail_out = CHUNK as u32;
            let r = deflate(ptr, Z_FINISH);
            debug_assert!(r >= 0, "deflate returned error {r}");
            let written = CHUNK - (*ptr).avail_out as usize;
            output.extend_from_slice(&buf[..written]);
            if r == Z_STREAM_END {
                break;
            }
        }
        deflateEnd(ptr);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queried runtime zlib version is a well-formed, non-empty
    /// dotted-digit string on every machine this crate runs on — the
    /// evidence-recording property `runtime_version()` exists to serve
    /// (fable finding #3): a caller can always retrieve and log the
    /// EXACT linked zlib identity next to emitted output, rather than
    /// trusting a hardcoded assumption that could silently go stale.
    #[test]
    fn runtime_version_is_observable() {
        let version = runtime_version();
        assert!(
            !version.is_empty(),
            "zlibVersion() returned an empty string"
        );
        assert!(
            version.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "expected a dotted-digit zlib version, got {version:?}"
        );
    }

    #[test]
    fn deflater_init_succeeds_with_the_queried_version() {
        // Exercises the same `deflateInit2_` call `trial_compression_row_
        // filters` depends on, now passing the queried version pointer
        // instead of the previously-hardcoded literal.
        Deflater::new().expect("deflateInit2_ should succeed on this host");
    }

    /// `deflate_level9_memlevel(data, 8)` must reproduce `flate2`'s proven
    /// level-9 (`memLevel=8`) stream byte-for-byte — the baseline the E-0036
    /// seam path keeps delegating to `zlib_compress9`. If this ever diverges,
    /// the seam identity/8/8 trial would stop matching `stage_emit`.
    #[test]
    fn memlevel8_matches_flate2_level9() {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        for sample in [
            &b""[..],
            &b"a"[..],
            &b"the quick brown fox jumps over the lazy dog"[..],
            &vec![0u8; 4096][..],
            &(0..=255u8).cycle().take(9000).collect::<Vec<u8>>()[..],
        ] {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(9));
            enc.write_all(sample).unwrap();
            let flate = enc.finish().unwrap();
            let ffi = deflate_level9_memlevel(sample, 8);
            assert_eq!(
                flate, ffi,
                "memLevel-8 FFI stream must equal flate2 level 9"
            );
        }
    }
}

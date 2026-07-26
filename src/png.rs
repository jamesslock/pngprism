//! Mirror of `lab/reference/m1_png.py` (in-repo original work, T-0053):
//! arbitrary-PNG decoder to canonical RGBA8 pixels + deterministic
//! indexed-PNG writer.
//!
//! This is a seam-by-seam faithful translation of the Python oracle: every
//! declared policy in the oracle's module docstring applies unchanged, every
//! error message is verbatim, and all arithmetic is exact integer math (no
//! floating point anywhere). Private helpers carry the oracle's `_`-prefixed
//! names in snake_case (`_parse_chunks` -> `parse_chunks`, ...).
//!
//! Malformed input never panics: every input-derived length is checked
//! before slicing and every failure surfaces as [`Error`]. Where the
//! oracle relies on a bare `IndexError` being unreachable (the `_inflate`
//! exact-length pin), the invariant is kept and the indexing sites are
//! `debug_assert!`-documented instead.

use crate::{Error, Rgba};
use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// The 8-byte PNG signature (`m1_png.PNG_SIGNATURE`).
pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Maximum bytes retained for one compressed PNG input (256 MiB).
///
/// Path-based callers use [`read_png_file`] so this ceiling is enforced on
/// the open descriptor before an unbounded read. Slice callers are checked at
/// the first instruction in [`decode_png`].
pub const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;

/// Maximum accepted width or height. This bounds pathological skinny images
/// independently of their total pixel count. It is a FIXED ceiling (not
/// scaled by `--max-pixels`): a 64 Mi-pixel square is 8192x8192, so 32,768/side
/// stays a valid independent skinny-image guard, and the widest admissible
/// image (32,768 x 32,768 = 1 Gi-pixel) is the absolute allocation backstop
/// even when `--max-pixels` is raised very high.
pub const MAX_DIMENSION: u32 = 32_768;

/// The widest native PNG pixel is 16-bit RGBA = 8 bytes. The decoded-scanline
/// ceiling is derived as (active pixel ceiling) x this constant, so a single
/// `--max-pixels` lever scales pixel and scanline admission together.
pub const MAX_BYTES_PER_PIXEL: u128 = 8;

/// Default maximum canonical pixel count (64 Mi-pixels; 2x the historical
/// 32 Mi ceiling — covers 50-64 MP cameras and 8K, ~9 GiB peak at the
/// T-0209-measured ~142 B/px on the 16 GiB Apple-Silicon baseline). This is
/// the DEFAULT; a single invocation may override it up or down via
/// [`set_max_pixels`] (`--max-pixels N`). The value actually enforced by a
/// decode is [`active_max_pixels`].
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Default aggregate filtered-scanline ceiling: [`MAX_PIXELS`] x the widest
/// native bytes/pixel (512 MiB), including one filter byte per row/pass. The
/// value enforced by a decode is derived from [`active_max_pixels`] the same
/// way, so it scales with `--max-pixels`.
pub const MAX_DECODED_SCANLINE_BYTES: u128 = MAX_PIXELS as u128 * MAX_BYTES_PER_PIXEL;

/// Process-wide active pixel ceiling. Defaults to [`MAX_PIXELS`]; the CLI's
/// `--max-pixels N` overrides it exactly once, before any decode, via
/// [`set_max_pixels`]. Every decode in the process reads this same value, so
/// the source-admission decode and the pipeline's own self-verification /
/// pack re-decodes all honor one coherent ceiling.
static ACTIVE_MAX_PIXELS: AtomicU64 = AtomicU64::new(MAX_PIXELS);

/// Override the active pixel ceiling (`--max-pixels N`). Intended to be set
/// once at startup, before decoding; all subsequent decodes in the process
/// admit up to `limit` pixels. `limit` must be >= 1 (the CLI rejects
/// 0/negative/non-numeric before calling this). Raising it above available
/// RAM is the caller's choice to own — the no-OOM guarantee holds only at or
/// below the active ceiling (see `docs/resource-limits.md`).
pub fn set_max_pixels(limit: u64) {
    ACTIVE_MAX_PIXELS.store(limit, Ordering::Relaxed);
}

/// The pixel ceiling the next decode will enforce: the `--max-pixels` override
/// if one was set, else the [`MAX_PIXELS`] default. The decoded-scanline
/// ceiling the decode enforces is derived from this value x
/// [`MAX_BYTES_PER_PIXEL`], so the single `--max-pixels` lever scales both
/// admission tests coherently (a 16-bit-RGBA image admitted by the pixel
/// ceiling is admitted by the scanline ceiling too, up to the per-row
/// filter-byte margin).
pub fn active_max_pixels() -> u64 {
    ACTIVE_MAX_PIXELS.load(Ordering::Relaxed)
}

fn input_limit_message() -> String {
    format!("resource limit exceeded: compressed PNG input exceeds {MAX_INPUT_BYTES} bytes")
}

/// Read a PNG from `path` without ever accepting more than
/// [`MAX_INPUT_BYTES`].
///
/// Descriptor metadata gives regular files a zero-payload fast rejection;
/// `Read::take(MAX + 1)` remains the authority so a growing file or a special
/// file cannot bypass the bound. Resource failures are stable `Data` errors;
/// open/read failures remain `Io` errors.
pub fn read_png_file(path: &Path) -> Result<Vec<u8>, Error> {
    let file = std::fs::File::open(path)
        .map_err(|err| Error::io(format!("io_error: cannot read {}: {err}", path.display())))?;
    let metadata = file
        .metadata()
        .map_err(|err| Error::io(format!("io_error: cannot read {}: {err}", path.display())))?;
    if metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(Error::data(format!(
            "data_error: cannot decode {}: {}",
            path.display(),
            input_limit_message()
        )));
    }

    // The metadata length is advisory. Cap both reservation and the actual
    // read so a concurrent grow or a non-regular input stays bounded.
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_INPUT_BYTES)
        .min(MAX_INPUT_BYTES);
    let mut raw = Vec::with_capacity(capacity.saturating_add(1));
    let mut bounded = file.take((MAX_INPUT_BYTES as u64) + 1);
    bounded
        .read_to_end(&mut raw)
        .map_err(|err| Error::io(format!("io_error: cannot read {}: {err}", path.display())))?;
    if raw.len() > MAX_INPUT_BYTES {
        return Err(Error::data(format!(
            "data_error: cannot decode {}: {}",
            path.display(),
            input_limit_message()
        )));
    }
    Ok(raw)
}

/// tRNS payload interpretation (`m1_png._parse_trns`): native-depth gray
/// colorkey, native-depth RGB colorkey, or per-palette-entry alpha bytes.
/// Color types 4/6 record tRNS as absent (spec-invalid placement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trns {
    Gray(u16),
    Rgb(u16, u16, u16),
    Palette(Vec<u8>),
}

/// iCCP chunk summary (`m1_png._parse_iccp`): profile name (latin-1) and
/// the compressed profile byte count. Never applied (no color management).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iccp {
    pub name: String,
    pub profile_bytes: usize,
}

/// Decoded-image sidecar facts (`m1_png.DecodedImage.properties`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Properties {
    pub color_type: u8,
    pub bit_depth: u8,
    pub interlaced: bool,
    pub plte: Option<Vec<(u8, u8, u8)>>,
    pub trns: Option<Trns>,
    pub gama: Option<u32>,
    pub iccp: Option<Iccp>,
    /// The oracle's `conversions` strings, verbatim.
    pub conversions: Vec<String>,
}

/// Canonical decode result: RGBA8 pixels row-major from the top-left,
/// exactly `width * height` of them (`m1_png.DecodedImage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
    pub properties: Properties,
}

/// `m1_png._CHANNELS`: samples per pixel for the accepted color types.
fn channels(color_type: u8) -> Option<u8> {
    match color_type {
        0 => Some(1),
        2 => Some(3),
        3 => Some(1),
        4 => Some(2),
        6 => Some(4),
        _ => None,
    }
}

/// `m1_png._VALID_DEPTHS`: spec-valid bit depths per color type.
fn valid_depths(color_type: u8) -> &'static [u8] {
    match color_type {
        0 => &[1, 2, 4, 8, 16],
        2 => &[8, 16],
        3 => &[1, 2, 4, 8],
        4 => &[8, 16],
        6 => &[8, 16],
        _ => &[],
    }
}

/// `m1_png._ADAM7_PASSES`: (x0, y0, dx, dy) of the seven interlace passes.
const ADAM7_PASSES: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// `m1_png._Header`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlaced: bool,
}

/// Render a 4-byte chunk kind the way Python's `{kind!r}` renders a bytes
/// object: `b'IHDR'`, with `\t` `\n` `\r` short escapes, `\\` and the active
/// quote backslash-escaped, every other non-printable byte as `\xNN`
/// (lowercase), and double quotes when the payload holds a single quote but
/// no double quote (CPython `bytes_repr`).
fn py_bytes_repr(bytes: &[u8]) -> String {
    let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from("b");
    out.push(quote);
    for &byte in bytes {
        if byte == quote as u8 || byte == b'\\' {
            out.push('\\');
            out.push(char::from(byte));
        } else if byte == b'\t' {
            out.push_str("\\t");
        } else if byte == b'\n' {
            out.push_str("\\n");
        } else if byte == b'\r' {
            out.push_str("\\r");
        } else if !(0x20..=0x7e).contains(&byte) {
            out.push_str(&format!("\\x{byte:02x}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out.push(quote);
    out
}

/// CRC-32 table (poly 0xEDB88320 reflected), built at compile time.
const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_crc_table();

/// One CRC-32 step over `bytes`, continuing the running register `state`
/// (init 0xFFFFFFFF, xorout 0xFFFFFFFF applied by the callers) — identical
/// arithmetic to `binascii.crc32`/`zlib.crc32`.
fn crc32_update(state: u32, bytes: &[u8]) -> u32 {
    let mut c = state;
    for &byte in bytes {
        c = CRC_TABLE[((c ^ u32::from(byte)) & 0xFF) as usize] ^ (c >> 8);
    }
    c
}

/// `binascii.crc32(bytes) & 0xFFFFFFFF`. Production chains `crc32_update`
/// through `chunk_crc`; this single-slice form exists for the golden pins.
#[cfg(test)]
fn crc32(bytes: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, bytes) ^ 0xFFFF_FFFF
}

/// `binascii.crc32(kind + payload) & 0xFFFFFFFF` without concatenating.
fn chunk_crc(kind: &[u8], payload: &[u8]) -> u32 {
    crc32_update(crc32_update(0xFFFF_FFFF, kind), payload) ^ 0xFFFF_FFFF
}

/// Mirrors `m1_png._parse_ihdr` (oracle lines 115-139).
fn parse_ihdr(payload: &[u8]) -> Result<Header, Error> {
    if payload.len() != 13 {
        return Err(Error::data(
            "IHDR chunk must be exactly 13 bytes".to_string(),
        ));
    }
    // All indexing below is in range: the 13-byte length was checked above.
    let width = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let height = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let bit_depth = payload[8];
    let color_type = payload[9];
    let compression = payload[10];
    let filter_method = payload[11];
    let interlace = payload[12];
    if width == 0 || height == 0 {
        return Err(Error::data(
            "image dimensions must be at least 1x1".to_string(),
        ));
    }
    if channels(color_type).is_none() {
        return Err(Error::data(format!("unsupported color type {color_type}")));
    }
    if !valid_depths(color_type).contains(&bit_depth) {
        return Err(Error::data(format!(
            "invalid bit depth {bit_depth} for color type {color_type}"
        )));
    }
    if compression != 0 {
        return Err(Error::data(format!(
            "unsupported compression method {compression}"
        )));
    }
    if filter_method != 0 {
        return Err(Error::data(format!(
            "unsupported filter method {filter_method}"
        )));
    }
    if interlace != 0 && interlace != 1 {
        return Err(Error::data(format!(
            "unsupported interlace method {interlace}"
        )));
    }
    let header = Header {
        width,
        height,
        bit_depth,
        color_type,
        interlaced: interlace == 1,
    };
    validate_header_resource_limits(&header)?;
    Ok(header)
}

/// Mirrors `m1_png._parse_iccp` (oracle lines 142-151). The latin-1 decode
/// maps each byte to the same code point.
fn parse_iccp(payload: &[u8]) -> Result<Iccp, Error> {
    let nul = match payload.iter().position(|&b| b == 0) {
        Some(n) if n > 0 => n,
        _ => {
            return Err(Error::data(
                "malformed iCCP chunk: missing profile name terminator".to_string(),
            ));
        }
    };
    // `nul + 1` is in bounds whenever `nul + 2 <= payload.len()` (checked
    // first; `||` short-circuits exactly like the oracle).
    if nul + 2 > payload.len() || payload[nul + 1] != 0 {
        return Err(Error::data(
            "unsupported iCCP compression method".to_string(),
        ));
    }
    let name = payload[..nul].iter().map(|&b| char::from(b)).collect();
    Ok(Iccp {
        name,
        profile_bytes: payload.len() - (nul + 2),
    })
}

/// The `m1_png._parse_chunks` return tuple as named fields.
struct ParsedChunks {
    header: Header,
    plte_payload: Option<Vec<u8>>,
    trns_payload: Option<Vec<u8>>,
    gama: Option<u32>,
    iccp: Option<Iccp>,
    idat_parts: Vec<Vec<u8>>,
}

/// Mirrors `m1_png._parse_chunks` (oracle lines 154-215): full chunk
/// discipline — CRC of every chunk validated before dispatch, IHDR first and
/// exactly once, IEND final with no trailing bytes, unknown critical chunks
/// rejected, unknown ancillary chunks ignored, IDATs concatenated in order.
fn parse_chunks(raw: &[u8]) -> Result<ParsedChunks, Error> {
    let mut offset = PNG_SIGNATURE.len();
    let mut header: Option<Header> = None;
    let mut plte_payload: Option<Vec<u8>> = None;
    let mut trns_payload: Option<Vec<u8>> = None;
    let mut gama: Option<u32> = None;
    let mut iccp: Option<Iccp> = None;
    let mut idat_parts: Vec<Vec<u8>> = Vec::new();
    let mut first = true;
    loop {
        if offset == raw.len() {
            return Err(Error::data("missing IEND chunk".to_string()));
        }
        if raw.len() - offset < 12 {
            return Err(Error::data("truncated chunk framing".to_string()));
        }
        // In range: `raw.len() - offset >= 12` was checked above.
        let length = u32::from_be_bytes([
            raw[offset],
            raw[offset + 1],
            raw[offset + 2],
            raw[offset + 3],
        ]);
        let kind: [u8; 4] = [
            raw[offset + 4],
            raw[offset + 5],
            raw[offset + 6],
            raw[offset + 7],
        ];
        // u64 offset arithmetic: a bogus 32-bit length can never wrap.
        let data_end = offset as u64 + 8 + u64::from(length);
        let crc_end = data_end + 4;
        if crc_end > raw.len() as u64 {
            return Err(Error::data(format!(
                "truncated {} chunk",
                py_bytes_repr(&kind)
            )));
        }
        // In range: `crc_end <= raw.len()` was checked above.
        let data_end = data_end as usize;
        let crc_end = crc_end as usize;
        let payload = &raw[offset + 8..data_end];
        let expected_crc = u32::from_be_bytes([
            raw[data_end],
            raw[data_end + 1],
            raw[data_end + 2],
            raw[data_end + 3],
        ]);
        let actual_crc = chunk_crc(&kind, payload);
        if actual_crc != expected_crc {
            return Err(Error::data(format!(
                "CRC mismatch in {} chunk",
                py_bytes_repr(&kind)
            )));
        }
        if first {
            if kind != *b"IHDR" {
                return Err(Error::data("first chunk must be IHDR".to_string()));
            }
            first = false;
        }
        if kind == *b"IHDR" {
            if header.is_some() {
                return Err(Error::data("duplicate IHDR chunk".to_string()));
            }
            header = Some(parse_ihdr(payload)?);
        } else if kind == *b"PLTE" {
            if plte_payload.is_some() {
                return Err(Error::data("duplicate PLTE chunk".to_string()));
            }
            plte_payload = Some(payload.to_vec());
        } else if kind == *b"tRNS" {
            if trns_payload.is_some() {
                return Err(Error::data("duplicate tRNS chunk".to_string()));
            }
            trns_payload = Some(payload.to_vec());
        } else if kind == *b"gAMA" {
            if payload.len() != 4 {
                return Err(Error::data("gAMA chunk must be 4 bytes".to_string()));
            }
            gama = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        } else if kind == *b"iCCP" {
            iccp = Some(parse_iccp(payload)?);
        } else if kind == *b"IDAT" {
            idat_parts.push(payload.to_vec());
        } else if kind == *b"IEND" {
            if length != 0 {
                return Err(Error::data("IEND chunk must be empty".to_string()));
            }
            if crc_end != raw.len() {
                return Err(Error::data("trailing garbage after IEND chunk".to_string()));
            }
            break;
        } else if kind[0] & 0x20 == 0 {
            return Err(Error::data(format!(
                "unknown critical chunk {}",
                py_bytes_repr(&kind)
            )));
        }
        // Unknown ancillary chunk: ignored by policy.
        offset = crc_end;
    }
    let header = match header {
        Some(header) => header,
        // Unreachable: the first chunk must be IHDR (oracle: pragma no cover).
        None => return Err(Error::data("missing IHDR chunk".to_string())),
    };
    Ok(ParsedChunks {
        header,
        plte_payload,
        trns_payload,
        gama,
        iccp,
        idat_parts,
    })
}

/// Mirrors `m1_png._parse_plte` (oracle lines 218-224).
fn parse_plte(payload: &[u8]) -> Result<Vec<(u8, u8, u8)>, Error> {
    if !payload.len().is_multiple_of(3) {
        return Err(Error::data(
            "PLTE length must be a multiple of 3".to_string(),
        ));
    }
    let entries = payload.len() / 3;
    if !(1..=256).contains(&entries) {
        return Err(Error::data(format!(
            "PLTE entry count {entries} out of range"
        )));
    }
    Ok(payload
        .chunks_exact(3)
        .map(|triple| (triple[0], triple[1], triple[2]))
        .collect())
}

/// Mirrors `m1_png._parse_trns` (oracle lines 227-245). Color types 4/6 are
/// spec-invalid placements: recorded as absent, never applied.
fn parse_trns(
    payload: &[u8],
    header: &Header,
    plte: Option<&[(u8, u8, u8)]>,
) -> Result<Option<Trns>, Error> {
    let color_type = header.color_type;
    if color_type == 4 || color_type == 6 {
        // Spec-invalid placement: recorded as absent, never applied.
        return Ok(None);
    }
    if color_type == 3 {
        if let Some(palette) = plte
            && payload.len() > palette.len()
        {
            return Err(Error::data("tRNS longer than PLTE".to_string()));
        }
        return Ok(Some(Trns::Palette(payload.to_vec())));
    }
    if color_type == 0 {
        if payload.len() != 2 {
            return Err(Error::data("grayscale tRNS must be 2 bytes".to_string()));
        }
        let value = u16::from_be_bytes([payload[0], payload[1]]);
        return Ok(Some(Trns::Gray(value)));
    }
    if payload.len() != 6 {
        return Err(Error::data("truecolor tRNS must be 6 bytes".to_string()));
    }
    Ok(Some(Trns::Rgb(
        u16::from_be_bytes([payload[0], payload[1]]),
        u16::from_be_bytes([payload[2], payload[3]]),
        u16::from_be_bytes([payload[4], payload[5]]),
    )))
}

/// Mirrors `m1_png._pass_geometry` (oracle lines 248-257): non-empty passes
/// as (x0, y0, dx, dy, pass_width, pass_height).
fn pass_geometry(header: &Header) -> Vec<(u32, u32, u32, u32, u32, u32)> {
    let passes: &[(u32, u32, u32, u32)] = if header.interlaced {
        &ADAM7_PASSES
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut geometry = Vec::new();
    for &(x0, y0, dx, dy) in passes {
        // u64 math: dimensions near u32::MAX cannot overflow the stride term.
        // The quotient is <= the dimension, so the u32 casts are exact.
        let pw = if header.width > x0 {
            (u64::from(header.width) - u64::from(x0)).div_ceil(u64::from(dx)) as u32
        } else {
            0
        };
        let ph = if header.height > y0 {
            (u64::from(header.height) - u64::from(y0)).div_ceil(u64::from(dy)) as u32
        } else {
            0
        };
        if pw != 0 && ph != 0 {
            geometry.push((x0, y0, dx, dy, pw, ph));
        }
    }
    geometry
}

/// Validate all IHDR-derived allocation ceilings before chunk parsing reaches
/// IDAT, and therefore before inflation or canonical-pixel allocation. The
/// pixel (and derived scanline) ceilings are read from the process-wide active
/// value ([`active_max_pixels`], set once by `--max-pixels`); the dimension
/// ceiling is fixed. Delegates to [`validate_header_resource_limits_with`] so
/// the pure ceiling arithmetic is unit-testable without mutating the global.
fn validate_header_resource_limits(header: &Header) -> Result<(), Error> {
    validate_header_resource_limits_with(header, active_max_pixels())
}

/// The pure admission check against an explicit pixel ceiling `max_pixels`.
/// The scanline ceiling is derived (`max_pixels` x [`MAX_BYTES_PER_PIXEL`]);
/// the dimension ceiling is fixed. Separated from the global-reading wrapper so
/// tests can drive the knob deterministically.
fn validate_header_resource_limits_with(header: &Header, max_pixels: u64) -> Result<(), Error> {
    if header.width > MAX_DIMENSION || header.height > MAX_DIMENSION {
        return Err(Error::data(format!(
            "resource limit exceeded: image dimensions {}x{} exceed per-dimension maximum {MAX_DIMENSION}",
            header.width, header.height
        )));
    }

    let pixel_count = u64::from(header.width)
        .checked_mul(u64::from(header.height))
        .ok_or_else(|| Error::data("resource limit exceeded: pixel count overflow".to_string()))?;
    if pixel_count > max_pixels {
        return Err(Error::data(format!(
            "resource limit exceeded: image has {pixel_count} pixels; maximum is {max_pixels}"
        )));
    }

    // Header validation above guarantees a known channel count. u128 plus
    // checked operations keeps this computation total even if the public
    // dimension policy is raised later without auditing integer widths.
    let channel_count = channels(header.color_type).expect("validated color type");
    let bits_per_pixel = u128::from(channel_count) * u128::from(header.bit_depth);
    let mut decoded_bytes = 0u128;
    for &(_, _, _, _, pass_width, pass_height) in &pass_geometry(header) {
        let row_bits = u128::from(pass_width)
            .checked_mul(bits_per_pixel)
            .ok_or_else(|| {
                Error::data("resource limit exceeded: decoded scanline size overflow".to_string())
            })?;
        let row_bytes = row_bits.div_ceil(8);
        let pass_bytes = u128::from(pass_height)
            .checked_mul(row_bytes.checked_add(1).ok_or_else(|| {
                Error::data("resource limit exceeded: decoded scanline size overflow".to_string())
            })?)
            .ok_or_else(|| {
                Error::data("resource limit exceeded: decoded scanline size overflow".to_string())
            })?;
        decoded_bytes = decoded_bytes.checked_add(pass_bytes).ok_or_else(|| {
            Error::data("resource limit exceeded: decoded scanline size overflow".to_string())
        })?;
    }
    let max_decoded_scanline_bytes = u128::from(max_pixels) * MAX_BYTES_PER_PIXEL;
    if decoded_bytes > max_decoded_scanline_bytes {
        return Err(Error::data(format!(
            "resource limit exceeded: decoded scanlines require {decoded_bytes} bytes; maximum is {max_decoded_scanline_bytes}"
        )));
    }
    Ok(())
}

/// Mirrors `m1_png._inflate` (oracle lines 260-273): the concatenated IDAT
/// zlib stream must decompress cleanly to EOF with no trailing bytes, and the
/// decompressed size must equal `expected` exactly.
///
/// flate2 mapping of the oracle's `zlib.decompressobj()` acceptance
/// semantics: `Decompress::new(true)` (zlib wrapper); the joined parts are
/// fed through a grow-as-needed output loop with `FlushDecompress::None` —
/// `Status::StreamEnd` is `decomp.eof`, `Status::BufError` with output space
/// still offered means the input ran out before the stream ended (the
/// oracle's `not decomp.eof` branch), `total_in < joined.len()` at StreamEnd
/// is the oracle's `decomp.unused_data` branch, and `DecompressError` is the
/// oracle's `zlib.error` branch (message suffix is backend text and may
/// differ from CPython's, per the port spec). Check order matches the
/// oracle: stream error, then truncated, then trailing, then length.
fn inflate(parts: &[Vec<u8>], expected: u128) -> Result<Vec<u8>, Error> {
    let mut joined = Vec::with_capacity(parts.iter().map(Vec::len).sum::<usize>());
    for part in parts {
        joined.extend_from_slice(part);
    }
    let mut decomp = Decompress::new(true);
    let mut out: Vec<u8> = Vec::new();
    // Bounded scratch growth: the first pass aims at the expected size, so
    // well-formed input finishes in one call, but an absurd expected size
    // (gigapixel IHDR) never pre-allocates — it fails at StreamEnd/BufError
    // after a bounded scratch instead.
    //
    // T-0212 fix: once `out_at` has already reached the IHDR-declared
    // `expected` total, `remaining` is 0 and the scratch offered drops to a
    // single byte rather than growing in further SCRATCH_CAP (8 MiB) steps.
    // A legitimate stream still finishes correctly from here — deflate
    // sometimes needs one more zero-output call to observe `StreamEnd` on a
    // trailing empty final block — but any stream that tries to produce so
    // much as ONE more byte than `expected` is thereby proven oversized
    // (§31: still a typed error, never a panic), so it is rejected the
    // instant that byte appears instead of after full materialization. This
    // is the T-0207 finding: pre-fix, a maximally-compressible IDAT could
    // reach ~67 MB RSS before the post-loop length check ever ran; post-fix
    // the excess materialized is bounded to O(1) bytes, not O(SCRATCH_CAP)
    // per iteration. See `tests/amplification/` for the reproducer + the
    // before/after RSS measurement.
    const SCRATCH_CAP: usize = 8 * 1024 * 1024;
    const OVERFLOW_PROBE: usize = 1;
    loop {
        let in_at = decomp.total_in() as usize;
        let out_at = decomp.total_out() as usize;
        debug_assert_eq!(out_at, out.len());
        let remaining = expected.saturating_sub(out_at as u128);
        let scratch = if remaining == 0 {
            OVERFLOW_PROBE
        } else {
            remaining.min(SCRATCH_CAP as u128) as usize
        };
        out.resize(out_at + scratch, 0);
        let result = decomp.decompress(&joined[in_at..], &mut out[out_at..], FlushDecompress::None);
        let produced = decomp.total_out() as usize - out_at;
        out.truncate(out_at + produced);
        if remaining == 0 && produced > 0 {
            // Conclusive: the stream decodes to more than IHDR promised.
            // No need to discover exactly how much more — stop here so the
            // amplification is bounded to this one small probe, not a full
            // materialization of the oversized stream.
            return Err(Error::data(format!(
                "decoded more than {expected} scanline bytes (deflate stream exceeds IHDR-declared size)"
            )));
        }
        match result {
            Ok(Status::StreamEnd) => break,
            Ok(Status::Ok) => {
                if produced == 0 && decomp.total_in() as usize == in_at {
                    // zlib documents progress on Z_OK; this guard only rules
                    // out an infinite loop and is unreachable per zlib docs.
                    return Err(Error::data("truncated IDAT deflate stream".to_string()));
                }
            }
            Ok(Status::BufError) => {
                // Scratch offered is always non-empty, so "no progress
                // possible" means the input ran out before the stream ended.
                return Err(Error::data("truncated IDAT deflate stream".to_string()));
            }
            Err(exc) => {
                return Err(Error::data(format!("invalid IDAT deflate stream: {exc}")));
            }
        }
    }
    if decomp.total_in() != joined.len() as u64 {
        return Err(Error::data(
            "trailing data after IDAT deflate stream".to_string(),
        ));
    }
    if out.len() as u128 != expected {
        return Err(Error::data(format!(
            "decoded {} scanline bytes, expected {expected}",
            out.len()
        )));
    }
    Ok(out)
}

/// Mirrors `m1_png._defilter` (oracle lines 276-321): each pass is
/// defiltered independently; all five filter types reconstruct mod 256; the
/// Paeth tie-break order is left, up, up-left (spec order).
fn defilter(
    data: &[u8],
    offset: usize,
    rows_count: usize,
    row_bytes: usize,
    bpp: usize,
) -> Result<(Vec<Vec<u8>>, usize), Error> {
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(rows_count);
    let mut prev = vec![0u8; row_bytes];
    let mut pos = offset;
    for _ in 0..rows_count {
        // Both bounds are pinned by `inflate`'s exact-length check (the sum
        // of the per-pass `1 + row_bytes` terms equals `data.len()`); the
        // checked accesses keep any violation panic-free on a path the oracle
        // would reach only via a bare IndexError.
        let Some(&filter_type) = data.get(pos) else {
            return Err(Error::data("scanline data exhausted mid-pass".to_string()));
        };
        pos += 1;
        let Some(line) = data.get(pos..pos + row_bytes) else {
            return Err(Error::data("scanline data exhausted mid-pass".to_string()));
        };
        pos += row_bytes;
        let recon: Vec<u8> = match filter_type {
            0 => line.to_vec(),
            1 => {
                let mut recon = line.to_vec();
                for i in bpp..row_bytes {
                    recon[i] = recon[i].wrapping_add(recon[i - bpp]);
                }
                recon
            }
            2 => {
                let mut recon = vec![0u8; row_bytes];
                for i in 0..row_bytes {
                    recon[i] = line[i].wrapping_add(prev[i]);
                }
                recon
            }
            3 => {
                let mut recon = vec![0u8; row_bytes];
                for i in 0..row_bytes {
                    let left = if i >= bpp { recon[i - bpp] } else { 0 };
                    // (left + prev) >> 1 <= 255, so the u8 cast is exact.
                    let average = ((u16::from(left) + u16::from(prev[i])) >> 1) as u8;
                    recon[i] = line[i].wrapping_add(average);
                }
                recon
            }
            4 => {
                let mut recon = vec![0u8; row_bytes];
                for i in 0..row_bytes {
                    let left = if i >= bpp { recon[i - bpp] } else { 0 };
                    let up = prev[i];
                    let upper_left = if i >= bpp { prev[i - bpp] } else { 0 };
                    let estimate = i32::from(left) + i32::from(up) - i32::from(upper_left);
                    let dist_left = (estimate - i32::from(left)).abs();
                    let dist_up = (estimate - i32::from(up)).abs();
                    let dist_ul = (estimate - i32::from(upper_left)).abs();
                    // Spec tie order: left, then up, then up-left.
                    let predictor = if dist_left <= dist_up && dist_left <= dist_ul {
                        left
                    } else if dist_up <= dist_ul {
                        up
                    } else {
                        upper_left
                    };
                    recon[i] = line[i].wrapping_add(predictor);
                }
                recon
            }
            other => return Err(Error::data(format!("invalid row filter type {other}"))),
        };
        rows.push(recon);
        // Mirror of the oracle's `prev = recon` (rows owns its own copy).
        prev = rows[rows.len() - 1].clone();
    }
    Ok((rows, pos))
}

/// Mirrors `m1_png._row_samples` (oracle lines 324-338): 16-bit big-endian
/// pairs, 8-bit bytes, or sub-byte MSB-first unpacking with row-padding
/// discard. The oracle's polymorphic return collapses to `Vec<u16>` (8-bit
/// and sub-byte samples widen exactly), which preserves every downstream
/// comparison and arithmetic result, including native-sample tRNS matching.
fn row_samples(row: &[u8], count: usize, bit_depth: u8) -> Vec<u16> {
    // Length invariant: `row` holds exactly ceil(count * bit_depth / 8) bytes
    // (the defilter row stride, itself pinned by `inflate`'s exact-length
    // check). `take`/`chunks_exact`/`truncate` keep this panic-free even if
    // the invariant were ever broken.
    match bit_depth {
        16 => {
            debug_assert!(row.len() >= count * 2);
            row.chunks_exact(2)
                .take(count)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect()
        }
        8 => {
            debug_assert!(row.len() >= count);
            row.iter().take(count).map(|&b| u16::from(b)).collect()
        }
        _ => {
            let per_byte = 8 / bit_depth;
            let mask = (1u16 << bit_depth) - 1;
            let mut samples: Vec<u16> = Vec::with_capacity(row.len() * usize::from(per_byte));
            for &byte in row {
                let mut shift = 8 - bit_depth;
                for _ in 0..per_byte {
                    samples.push((u16::from(byte) >> shift) & mask);
                    // Mirrors the oracle's `shift -= bit_depth`; the wrapped
                    // value after the last sample of a byte is never read.
                    shift = shift.wrapping_sub(bit_depth);
                }
            }
            samples.truncate(count);
            samples
        }
    }
}

/// Mirrors `m1_png._round16` (oracle lines 341-342): the declared 16-to-8
/// rounding `(v * 255 + 32767) // 65535`, applied to every channel.
fn round16(value: u16) -> u8 {
    // <= (65535 * 255 + 32767) / 65535 == 255, so the u8 cast is exact.
    ((u32::from(value) * 255 + 32767) / 65535) as u8
}

/// Mirrors `m1_png._convert_row` (oracle lines 345-405): one defiltered row
/// of native-depth samples becomes `pass_width` RGBA8 pixels under the
/// declared per-color-type conversions and tRNS policies.
fn convert_row(
    samples: &[u16],
    pass_width: usize,
    header: &Header,
    plte: Option<&[(u8, u8, u8)]>,
    trns: Option<&Trns>,
    gray_scale: u16,
) -> Result<Vec<Rgba>, Error> {
    // Sample-count invariant: `samples.len() == pass_width * channels` (row
    // stride pinned by `inflate`); indexing below cannot panic on any input.
    debug_assert_eq!(
        samples.len(),
        pass_width * usize::from(channels(header.color_type).expect("validated color type"))
    );
    let color_type = header.color_type;
    let bit_depth = header.bit_depth;
    let mut out: Vec<Rgba> = Vec::with_capacity(pass_width);
    if color_type == 0 {
        for &value in samples.iter().take(pass_width) {
            let gray = if bit_depth == 16 {
                round16(value)
            } else if bit_depth == 8 {
                // 8-bit samples are widened bytes: 0..=255.
                value as u8
            } else {
                // value <= 2^d - 1 and gray_scale == 255 / (2^d - 1), so the
                // product is <= 255 and the u8 cast is exact.
                (value * gray_scale) as u8
            };
            let alpha = if matches!(trns, Some(Trns::Gray(key)) if value == *key) {
                0
            } else {
                255
            };
            out.push((gray, gray, gray, alpha));
        }
    } else if color_type == 3 {
        let palette = plte.expect("guaranteed: palette images require PLTE");
        let palette_size = palette.len();
        for &sample in samples.iter().take(pass_width) {
            let index = usize::from(sample);
            if index >= palette_size {
                return Err(Error::data(format!(
                    "palette index {index} out of range ({palette_size} entries)"
                )));
            }
            let (red, green, blue) = palette[index];
            let alpha = match trns {
                Some(Trns::Palette(alphas)) if index < alphas.len() => alphas[index],
                _ => 255,
            };
            out.push((red, green, blue, alpha));
        }
    } else if color_type == 2 {
        for i in 0..pass_width {
            let red = samples[i * 3];
            let green = samples[i * 3 + 1];
            let blue = samples[i * 3 + 2];
            let alpha = if matches!(trns, Some(Trns::Rgb(r, g, b)) if (red, green, blue) == (*r, *g, *b))
            {
                0
            } else {
                255
            };
            if bit_depth == 16 {
                out.push((round16(red), round16(green), round16(blue), alpha));
            } else {
                // 8-bit samples are widened bytes: 0..=255.
                out.push((red as u8, green as u8, blue as u8, alpha));
            }
        }
    } else if color_type == 4 {
        for i in 0..pass_width {
            let value = samples[i * 2];
            let alpha = samples[i * 2 + 1];
            if bit_depth == 16 {
                out.push((
                    round16(value),
                    round16(value),
                    round16(value),
                    round16(alpha),
                ));
            } else {
                // 8-bit samples are widened bytes: 0..=255.
                out.push((value as u8, value as u8, value as u8, alpha as u8));
            }
        }
    } else {
        for i in 0..pass_width {
            let red = samples[i * 4];
            let green = samples[i * 4 + 1];
            let blue = samples[i * 4 + 2];
            let alpha = samples[i * 4 + 3];
            if bit_depth == 16 {
                out.push((round16(red), round16(green), round16(blue), round16(alpha)));
            } else {
                // 8-bit samples are widened bytes: 0..=255.
                out.push((red as u8, green as u8, blue as u8, alpha as u8));
            }
        }
    }
    Ok(out)
}

/// Mirrors `m1_png._conversions_applied` (oracle lines 408-432): the
/// declared conversion strings, verbatim and in the oracle's order.
fn conversions_applied(header: &Header, trns: Option<&Trns>) -> Vec<String> {
    let color_type = header.color_type;
    let mut conversions: Vec<String> = Vec::new();
    if color_type == 0 {
        conversions.push("gray:replicate-sample-to-rgb".to_string());
        if header.bit_depth < 8 {
            conversions.push("gray-sub-byte-scale:v*255//(2^bit_depth-1)".to_string());
        }
    } else if color_type == 2 {
        conversions.push("truecolor:alpha-255".to_string());
    } else if color_type == 3 {
        conversions.push("palette:plte-lookup".to_string());
    } else if color_type == 4 {
        conversions.push("gray-alpha:replicate-gray-to-rgb;alpha-passthrough".to_string());
    } else {
        conversions.push("rgba:passthrough".to_string());
    }
    if header.bit_depth == 16 {
        conversions.push("16-to-8-rounding:(v*255+32767)//65535".to_string());
    }
    if trns.is_some() {
        if color_type == 0 || color_type == 2 {
            conversions.push("trns:colorkey-binary-alpha(native-source-sample-match)".to_string());
        } else if color_type == 3 {
            conversions.push("trns:palette-entry-alpha(default-255)".to_string());
        }
    }
    if header.interlaced {
        conversions.push("adam7:seven-pass-reassembly".to_string());
    }
    conversions
}

/// Decode an arbitrary spec-valid PNG byte string to canonical RGBA8
/// pixels. Every malformed or unsupported input fails with [`Error`].
///
/// Mirrors `m1_png.decode_png` (oracle lines 435-495; all 14 declared
/// policy decisions in the oracle's module docstring apply unchanged).
pub fn decode_png(raw: &[u8]) -> Result<DecodedImage, Error> {
    if raw.len() > MAX_INPUT_BYTES {
        return Err(Error::data(input_limit_message()));
    }
    if raw.len() < PNG_SIGNATURE.len() || raw[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err(Error::data("missing PNG signature".to_string()));
    }

    let parsed = parse_chunks(raw)?;
    if parsed.idat_parts.is_empty() {
        return Err(Error::data("missing IDAT chunk".to_string()));
    }
    let plte = match &parsed.plte_payload {
        Some(payload) => Some(parse_plte(payload)?),
        None => None,
    };
    let header = parsed.header;
    if header.color_type == 3 && plte.is_none() {
        return Err(Error::data("palette image missing PLTE chunk".to_string()));
    }
    let trns = match &parsed.trns_payload {
        Some(payload) => parse_trns(payload, &header, plte.as_deref())?,
        None => None,
    };

    // Validated by `parse_ihdr`, so the color type always has a channel count.
    let channel_count = channels(header.color_type).expect("validated color type");
    let bits_per_pixel = u32::from(channel_count) * u32::from(header.bit_depth);
    // `(bits_per_pixel + 7) // 8` floors in the oracle; plain `/` floors
    // identically on this nonnegative domain (ceil would inflate bpp).
    let bpp = bits_per_pixel.div_ceil(8).max(1);
    let geometry = pass_geometry(&header);
    // This repeats the checked preflight arithmetic only to derive the exact
    // inflater target. `parse_ihdr` has already proven it is within the
    // absolute decoded-scanline ceiling.
    let expected: u128 = geometry
        .iter()
        .map(|&(_, _, _, _, pw, ph)| {
            let row_bytes = (u128::from(pw) * u128::from(bits_per_pixel)).div_ceil(8);
            u128::from(ph) * (1 + row_bytes)
        })
        .sum();
    let data = inflate(&parsed.idat_parts, expected)?;

    let gray_scale: u16 = if header.bit_depth < 8 {
        255 / ((1u16 << header.bit_depth) - 1)
    } else {
        0
    };
    // `parse_ihdr` proved the product is within the active pixel ceiling, and
    // the FIXED per-dimension ceiling (`MAX_DIMENSION`) bounds it at 32,768^2 =
    // 1 Gi-pixel regardless of any `--max-pixels` override, so it fits usize on
    // every supported target and bounds this allocation before it is made.
    let pixel_count = usize::try_from(u64::from(header.width) * u64::from(header.height))
        .expect("dimension ceiling keeps the pixel product within usize");
    let mut pixels: Vec<Option<Rgba>> = vec![None; pixel_count];
    let mut offset = 0usize;
    for &(x0, y0, dx, dy, pass_width, pass_height) in &geometry {
        let row_bytes = (pass_width as usize * bits_per_pixel as usize).div_ceil(8);
        let (rows, new_offset) =
            defilter(&data, offset, pass_height as usize, row_bytes, bpp as usize)?;
        offset = new_offset;
        let sample_count = pass_width as usize * usize::from(channel_count);
        for (row_index, row) in rows.iter().enumerate() {
            let samples = row_samples(row, sample_count, header.bit_depth);
            let converted = convert_row(
                &samples,
                pass_width as usize,
                &header,
                plte.as_deref(),
                trns.as_ref(),
                gray_scale,
            )?;
            let base =
                (y0 as usize + row_index * dy as usize) * header.width as usize + x0 as usize;
            for (column, pixel) in converted.into_iter().enumerate() {
                pixels[base + column * dx as usize] = Some(pixel);
            }
        }
    }
    if offset != data.len() {
        // Unreachable: `inflate` pins the exact length (oracle: pragma no cover).
        return Err(Error::data("scanline data left unconsumed".to_string()));
    }
    if pixels.iter().any(Option::is_none) {
        // Unreachable: the pass geometry covers the image (oracle: pragma no cover).
        return Err(Error::data(
            "interlace passes did not cover the whole image".to_string(),
        ));
    }
    let pixels: Vec<Rgba> = pixels
        .into_iter()
        .map(|pixel| pixel.expect("coverage checked above"))
        .collect();

    let conversions = conversions_applied(&header, trns.as_ref());
    let properties = Properties {
        color_type: header.color_type,
        bit_depth: header.bit_depth,
        interlaced: header.interlaced,
        plte,
        trns,
        gama: parsed.gama,
        iccp: parsed.iccp,
        conversions,
    };
    Ok(DecodedImage {
        width: header.width,
        height: header.height,
        pixels,
        properties,
    })
}

/// Mirrors `m1_png._emit_chunk` (oracle lines 498-500). Also reused by
/// `pack.rs` as `prism_pack._chunk` (same length+kind+payload+CRC-32 shape).
pub(crate) fn emit_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    // Writer payloads are bounded by validated inputs (PLTE <= 768 bytes,
    // IDAT = compressed scanlines of a width*height-indexed image).
    let length = u32::try_from(payload.len()).expect("chunk payload length fits u32");
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out.extend_from_slice(&chunk_crc(kind, payload).to_be_bytes());
    out
}

/// Serialize an indexed image as a deterministic color-type-3 PNG: bit
/// depth 8, non-interlaced, every row filter 0, single IDAT, zlib level 9,
/// tRNS only when some palette alpha is below 255 (trimmed to the last
/// such entry), no ancillary chunks.
///
/// Mirrors `m1_png.write_indexed_png` (oracle lines 503-561), including its
/// argument validation (the oracle's Python type checks have no Rust
/// analogue: `u32`/`&[Rgba]`/`&[u8]` are already int-typed, 4-tupled, and
/// 0..=255). Deflate is flate2's `ZlibEncoder` at level 9, spike-verified
/// byte-identical to Python's `zlib.compress(data, 9)`.
pub fn write_indexed_png(
    width: u32,
    height: u32,
    palette: &[Rgba],
    indices: &[u8],
) -> Result<Vec<u8>, Error> {
    if width == 0 || height == 0 {
        return Err(Error::data(
            "width and height must be at least 1".to_string(),
        ));
    }
    if palette.is_empty() || palette.len() > 256 {
        return Err(Error::data(
            "palette must contain 1..256 entries".to_string(),
        ));
    }
    // u64: width * height of validated u32 dims can exceed u32 but never u64.
    let pixel_count = u64::from(width) * u64::from(height);
    if indices.len() as u64 != pixel_count {
        return Err(Error::data(format!(
            "expected {pixel_count} indices, got {}",
            indices.len()
        )));
    }
    for &index in indices {
        if usize::from(index) >= palette.len() {
            return Err(Error::data(format!("palette index {index} out of range")));
        }
    }

    let row_width = width as usize;
    let mut scanlines = Vec::with_capacity(indices.len() + height as usize);
    for y in 0..height as usize {
        scanlines.push(0);
        // In range: `indices.len() == width * height` was validated above.
        scanlines.extend_from_slice(&indices[y * row_width..(y + 1) * row_width]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
    encoder
        .write_all(&scanlines)
        .expect("writing to a Vec cannot fail");
    let compressed = encoder
        .finish()
        .expect("zlib deflate of in-memory data cannot fail");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);

    let mut plte_payload = Vec::with_capacity(palette.len() * 3);
    for &(red, green, blue, _) in palette {
        plte_payload.extend_from_slice(&[red, green, blue]);
    }

    let mut last_transparent: Option<usize> = None;
    for (position, &(_, _, _, alpha)) in palette.iter().enumerate() {
        if alpha < 255 {
            last_transparent = Some(position);
        }
    }

    let mut out = PNG_SIGNATURE.to_vec();
    out.extend_from_slice(&emit_chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&emit_chunk(b"PLTE", &plte_payload));
    if let Some(last) = last_transparent {
        let trns_payload: Vec<u8> = palette[..=last]
            .iter()
            .map(|&(_, _, _, alpha)| alpha)
            .collect();
        out.extend_from_slice(&emit_chunk(b"tRNS", &trns_payload));
    }
    out.extend_from_slice(&emit_chunk(b"IDAT", &compressed));
    out.extend_from_slice(&emit_chunk(b"IEND", b""));
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! Port of `lab/reference/test_m1_png.py`'s `HandVectorTests`,
    //! `IndexedWriterTests`, and `MalformedPngTests`, plus CRC-32 goldens and
    //! the Python-bytes-repr helper pins. The smoke-set, PngSuite, and
    //! external-verifier tests live in `tests/png_corpus.rs`.
    use super::*;

    /// Port of the oracle test file's `_chunk` (test-local on purpose: the
    /// builder must stay independent of the writer under test).
    fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let crc = chunk_crc(kind, payload);
        let mut out = Vec::with_capacity(12 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    /// `zlib.compress(data, level)` via the same backend the writer uses.
    fn zlib_compress(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
        encoder.write_all(data).expect("write to Vec");
        encoder.finish().expect("finish zlib stream")
    }

    /// Port of the oracle test file's `_hand_png`: an independent minimal
    /// PNG builder for hand vectors (filter 0 rows, single IDAT, level 9).
    /// It never calls `write_indexed_png`.
    #[allow(clippy::too_many_arguments)]
    fn hand_png(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        scanlines: &[u8],
        plte: Option<&[u8]>,
        trns: Option<&[u8]>,
        interlace: u8,
        extra_chunks: &[(&[u8; 4], &[u8])],
    ) -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, interlace]);
        let mut blob = PNG_SIGNATURE.to_vec();
        blob.extend_from_slice(&chunk(b"IHDR", &ihdr));
        for (kind, payload) in extra_chunks {
            blob.extend_from_slice(&chunk(kind, payload));
        }
        if let Some(payload) = plte {
            blob.extend_from_slice(&chunk(b"PLTE", payload));
        }
        if let Some(payload) = trns {
            blob.extend_from_slice(&chunk(b"tRNS", payload));
        }
        blob.extend_from_slice(&chunk(b"IDAT", &zlib_compress(scanlines, 9)));
        blob.extend_from_slice(&chunk(b"IEND", b""));
        blob
    }

    /// Port of the oracle test file's `_chunk_names`.
    fn chunk_names(blob: &[u8]) -> Vec<[u8; 4]> {
        let mut names = Vec::new();
        let mut offset = PNG_SIGNATURE.len();
        while offset < blob.len() {
            let length = u32::from_be_bytes(blob[offset..offset + 4].try_into().unwrap()) as usize;
            names.push(blob[offset + 4..offset + 8].try_into().unwrap());
            offset += 12 + length;
        }
        names
    }

    /// Port of the oracle test file's `_chunk_payload`.
    fn chunk_payload(blob: &[u8], wanted: &[u8; 4]) -> Vec<u8> {
        let mut offset = PNG_SIGNATURE.len();
        while offset < blob.len() {
            let length = u32::from_be_bytes(blob[offset..offset + 4].try_into().unwrap()) as usize;
            if &blob[offset + 4..offset + 8] == wanted {
                return blob[offset + 8..offset + 8 + length].to_vec();
            }
            offset += 12 + length;
        }
        panic!("chunk {wanted:?} not found");
    }

    fn expect_png_error(blob: &[u8], expected: &str) {
        match decode_png(blob) {
            Err(err) => assert_eq!(err.message(), expected),
            Ok(image) => panic!(
                "expected Error {expected:?}, got a clean {}x{} decode",
                image.width, image.height
            ),
        }
    }

    #[test]
    fn crc32_goldens() {
        // binascii.crc32 / zlib.crc32 reference values.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn py_bytes_repr_matches_python() {
        assert_eq!(py_bytes_repr(b"IDAT"), "b'IDAT'");
        assert_eq!(
            py_bytes_repr(&[0x00, b'A', 0x7f, 0xff]),
            "b'\\x00A\\x7f\\xff'"
        );
        assert_eq!(py_bytes_repr(b"\t\n\r\\"), "b'\\t\\n\\r\\\\'");
        assert_eq!(py_bytes_repr(b"'"), "b\"'\"");
        assert_eq!(py_bytes_repr(b"'\""), "b'\\'\"'");
    }

    // ---- HandVectorTests ----

    #[test]
    fn unpack_1bit_msb_first() {
        let scanlines = [0x00, 0b1011_0010, 0b1100_0000];
        let image = decode_png(&hand_png(10, 1, 1, 0, &scanlines, None, None, 0, &[])).unwrap();
        let grays = [255, 0, 255, 255, 0, 0, 255, 0, 255, 255];
        let expected: Vec<Rgba> = grays.iter().map(|&g| (g, g, g, 255)).collect();
        assert_eq!(image.pixels, expected);
    }

    #[test]
    fn unpack_2bit_msb_first_with_padding() {
        let scanlines = [0x00, 0b0001_1011, 0b1000_0000];
        let image = decode_png(&hand_png(5, 1, 2, 0, &scanlines, None, None, 0, &[])).unwrap();
        let grays = [0, 85, 170, 255, 170];
        let expected: Vec<Rgba> = grays.iter().map(|&g| (g, g, g, 255)).collect();
        assert_eq!(image.pixels, expected);
    }

    #[test]
    fn unpack_4bit_msb_first_with_padding() {
        let scanlines = [0x00, 0x1F, 0x80];
        let image = decode_png(&hand_png(3, 1, 4, 0, &scanlines, None, None, 0, &[])).unwrap();
        let grays = [17, 255, 136];
        let expected: Vec<Rgba> = grays.iter().map(|&g| (g, g, g, 255)).collect();
        assert_eq!(image.pixels, expected);
    }

    #[test]
    fn palette_4bit_indices() {
        let plte = [10, 20, 30, 40, 50, 60];
        let scanlines = [0x00, 0x01, 0x10];
        let image =
            decode_png(&hand_png(3, 1, 4, 3, &scanlines, Some(&plte), None, 0, &[])).unwrap();
        assert_eq!(
            image.pixels,
            vec![(10, 20, 30, 255), (40, 50, 60, 255), (40, 50, 60, 255)]
        );
    }

    #[test]
    fn round16_boundaries() {
        let mut scanlines = vec![0x00];
        for value in [0u16, 65535, 32768] {
            scanlines.extend_from_slice(&value.to_be_bytes());
        }
        let image = decode_png(&hand_png(3, 1, 16, 0, &scanlines, None, None, 0, &[])).unwrap();
        assert_eq!(
            image.pixels,
            vec![(0, 0, 0, 255), (255, 255, 255, 255), (128, 128, 128, 255)]
        );
    }

    #[test]
    fn gray_trns_binary_alpha() {
        let scanlines = [0x00, 7, 9];
        let trns = 7u16.to_be_bytes();
        let image =
            decode_png(&hand_png(2, 1, 8, 0, &scanlines, None, Some(&trns), 0, &[])).unwrap();
        assert_eq!(image.pixels, vec![(7, 7, 7, 0), (9, 9, 9, 255)]);
        assert_eq!(image.properties.trns, Some(Trns::Gray(7)));
    }

    #[test]
    fn gray_trns_matches_source_sample_before_rounding() {
        // Both 32896 and 32768 round to 128; only the exact source match is keyed.
        let mut scanlines = vec![0x00];
        for value in [32896u16, 32768] {
            scanlines.extend_from_slice(&value.to_be_bytes());
        }
        let trns = 32896u16.to_be_bytes();
        let image = decode_png(&hand_png(
            2,
            1,
            16,
            0,
            &scanlines,
            None,
            Some(&trns),
            0,
            &[],
        ))
        .unwrap();
        assert_eq!(image.pixels, vec![(128, 128, 128, 0), (128, 128, 128, 255)]);
    }

    #[test]
    fn truecolor_trns_binary_alpha() {
        let scanlines = [0x00, 255, 0, 255, 1, 2, 3];
        let mut trns = Vec::new();
        for value in [255u16, 0, 255] {
            trns.extend_from_slice(&value.to_be_bytes());
        }
        let image =
            decode_png(&hand_png(2, 1, 8, 2, &scanlines, None, Some(&trns), 0, &[])).unwrap();
        assert_eq!(image.pixels, vec![(255, 0, 255, 0), (1, 2, 3, 255)]);
        assert_eq!(image.properties.trns, Some(Trns::Rgb(255, 0, 255)));
    }

    #[test]
    fn truecolor16_trns_matches_source_samples() {
        let mut scanlines = vec![0x00];
        for value in [65535u16, 0, 65535, 32768, 0, 0] {
            scanlines.extend_from_slice(&value.to_be_bytes());
        }
        let mut trns = Vec::new();
        for value in [65535u16, 0, 65535] {
            trns.extend_from_slice(&value.to_be_bytes());
        }
        let image = decode_png(&hand_png(
            2,
            1,
            16,
            2,
            &scanlines,
            None,
            Some(&trns),
            0,
            &[],
        ))
        .unwrap();
        assert_eq!(image.pixels, vec![(255, 0, 255, 0), (128, 0, 0, 255)]);
    }

    #[test]
    fn palette_without_trns_is_fully_opaque() {
        let plte = [10, 20, 30, 40, 50, 60];
        let scanlines = [0x00, 0, 1];
        let image =
            decode_png(&hand_png(2, 1, 8, 3, &scanlines, Some(&plte), None, 0, &[])).unwrap();
        assert_eq!(image.pixels, vec![(10, 20, 30, 255), (40, 50, 60, 255)]);
        assert_eq!(image.properties.trns, None);
    }

    #[test]
    fn palette_trns_defaults_to_255_beyond_trns_length() {
        let plte = [10, 20, 30, 40, 50, 60, 70, 80, 90];
        let scanlines = [0x00, 0, 1, 2];
        let trns = [0, 128];
        let image = decode_png(&hand_png(
            3,
            1,
            8,
            3,
            &scanlines,
            Some(&plte),
            Some(&trns),
            0,
            &[],
        ))
        .unwrap();
        assert_eq!(
            image.pixels,
            vec![(10, 20, 30, 0), (40, 50, 60, 128), (70, 80, 90, 255)]
        );
    }

    #[test]
    fn gama_and_iccp_recorded_never_applied() {
        let mut iccp_payload = b"test-profile\x00\x00".to_vec();
        iccp_payload.extend_from_slice(&zlib_compress(b"fake-icc", 6));
        let gama_payload = 45455u32.to_be_bytes();
        let extra: [(&[u8; 4], &[u8]); 2] = [(b"gAMA", &gama_payload), (b"iCCP", &iccp_payload)];
        let scanlines = [0x00, 100, 150, 200];
        let image = decode_png(&hand_png(1, 1, 8, 2, &scanlines, None, None, 0, &extra)).unwrap();
        assert_eq!(image.properties.gama, Some(45455));
        assert_eq!(
            image.properties.iccp,
            Some(Iccp {
                name: "test-profile".to_string(),
                profile_bytes: iccp_payload.len() - 14,
            })
        );
        assert_eq!(image.pixels, vec![(100, 150, 200, 255)]);
    }

    #[test]
    fn unknown_ancillary_chunk_is_ignored() {
        let scanlines = [0x00, 42];
        let extra: [(&[u8; 4], &[u8]); 1] = [(b"vpAg", b"whatever")];
        let image = decode_png(&hand_png(1, 1, 8, 0, &scanlines, None, None, 0, &extra)).unwrap();
        assert_eq!(image.pixels, vec![(42, 42, 42, 255)]);
    }

    // ---- IndexedWriterTests ----

    const PALETTE: [Rgba; 4] = [
        (255, 0, 0, 255),
        (0, 255, 0, 128),
        (0, 0, 255, 0),
        (255, 255, 0, 255),
    ];

    #[test]
    fn round_trip_preserves_palette_and_indices() {
        let (width, height) = (5u32, 3u32);
        let indices: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).map(move |x| ((x + y) % 4) as u8))
            .collect();
        let blob = write_indexed_png(width, height, &PALETTE, &indices).unwrap();
        let image = decode_png(&blob).unwrap();
        assert_eq!((image.width, image.height), (width, height));
        assert_eq!(image.properties.color_type, 3);
        assert_eq!(image.properties.bit_depth, 8);
        assert!(!image.properties.interlaced);
        assert_eq!(
            image.properties.plte,
            Some(
                PALETTE
                    .iter()
                    .map(|&(r, g, b, _)| (r, g, b))
                    .collect::<Vec<_>>()
            )
        );
        assert_eq!(
            image.properties.trns,
            Some(Trns::Palette(vec![255, 128, 0]))
        );
        let expected: Vec<Rgba> = indices.iter().map(|&i| PALETTE[usize::from(i)]).collect();
        assert_eq!(image.pixels, expected);
    }

    #[test]
    fn all_opaque_palette_emits_no_trns() {
        let palette: [Rgba; 3] = [(1, 2, 3, 255), (4, 5, 6, 255), (7, 8, 9, 255)];
        let blob = write_indexed_png(2, 2, &palette, &[0, 1, 2, 0]).unwrap();
        assert!(!chunk_names(&blob).contains(b"tRNS"));
        let image = decode_png(&blob).unwrap();
        assert_eq!(image.properties.trns, None);
        assert!(image.pixels.iter().all(|pixel| pixel.3 == 255));
    }

    #[test]
    fn mid_trns_trims_trailing_opaque_entries() {
        let palette: [Rgba; 4] = [
            (0, 0, 0, 0),
            (10, 10, 10, 200),
            (20, 20, 20, 255),
            (30, 30, 30, 255),
        ];
        let blob = write_indexed_png(2, 2, &palette, &[0, 1, 2, 3]).unwrap();
        assert_eq!(chunk_payload(&blob, b"tRNS"), vec![0, 200]);
    }

    #[test]
    fn output_is_deterministic() {
        let indices: Vec<u8> = (0..4)
            .flat_map(|y| (0..4).map(move |x| ((x * y) % 4) as u8))
            .collect();
        let first = write_indexed_png(4, 4, &PALETTE, &indices).unwrap();
        let second = write_indexed_png(4, 4, &PALETTE, &indices).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn chunk_layout() {
        let blob = write_indexed_png(1, 1, &[(9, 9, 9, 255)], &[0]).unwrap();
        assert_eq!(
            chunk_names(&blob),
            vec![*b"IHDR", *b"PLTE", *b"IDAT", *b"IEND"]
        );
    }

    #[test]
    fn writer_rejects_invalid_arguments() {
        // The oracle's `write_indexed_png(1, 1, [(1, 2, 3, 256)], [0])` case
        // has no Rust analogue: palette channels are `u8` by type.
        let empty: [Rgba; 0] = [];
        let cases: Vec<(Result<Vec<u8>, Error>, &str)> = vec![
            (
                write_indexed_png(0, 1, &PALETTE, &[0]),
                "width and height must be at least 1",
            ),
            (
                write_indexed_png(1, 1, &empty, &[0]),
                "palette must contain 1..256 entries",
            ),
            (
                write_indexed_png(2, 1, &PALETTE, &[0]),
                "expected 2 indices, got 1",
            ),
            (
                write_indexed_png(1, 1, &PALETTE, &[4]),
                "palette index 4 out of range",
            ),
        ];
        for (result, expected) in cases {
            match result {
                Err(err) => assert_eq!(err.message(), expected),
                Ok(_) => panic!("expected Error {expected:?}"),
            }
        }
    }

    // ---- MalformedPngTests ----

    /// Port of the oracle test class's `VALID` blob: 2x2 RGB8, filter 0 rows.
    fn valid_blob() -> Vec<u8> {
        let mut scanlines = vec![0x00];
        scanlines.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        scanlines.push(0);
        scanlines.extend_from_slice(&[7, 8, 9, 10, 11, 12]);
        hand_png(2, 2, 8, 2, &scanlines, None, None, 0, &[])
    }

    fn idat_offset(blob: &[u8]) -> usize {
        blob.windows(4)
            .position(|window| window == b"IDAT")
            .unwrap()
    }

    #[test]
    fn bad_signature() {
        let valid = valid_blob();
        let mut broken = b"\x89PNX".to_vec();
        broken.extend_from_slice(&valid[4..]);
        expect_png_error(&broken, "missing PNG signature");
        expect_png_error(b"", "missing PNG signature");
        expect_png_error(b"\x89PNG", "missing PNG signature");
    }

    #[test]
    fn bad_crc() {
        let valid = valid_blob();
        let mut blob = valid.clone();
        let idat_at = idat_offset(&valid);
        blob[idat_at + 4] ^= 0xFF;
        expect_png_error(&blob, "CRC mismatch in b'IDAT' chunk");
    }

    #[test]
    fn truncated_idat() {
        let valid = valid_blob();
        let idat_at = idat_offset(&valid);
        // idat_at + 6 leaves 10 bytes from the IDAT length field — fewer
        // than the 12-byte minimum frame, so the framing check fires (the
        // oracle raises the same message at this truncation point).
        expect_png_error(&valid[..idat_at + 6], "truncated chunk framing");
        // A deeper cut keeps a full frame header but truncates the payload,
        // hitting the oracle's `truncated {kind!r} chunk` site instead.
        expect_png_error(&valid[..idat_at + 10], "truncated b'IDAT' chunk");
    }

    #[test]
    fn truncated_deflate_stream() {
        let mut scanlines = vec![0x00];
        scanlines.extend_from_slice(&[1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3]);
        let full = zlib_compress(&scanlines, 9);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut blob = PNG_SIGNATURE.to_vec();
        blob.extend_from_slice(&chunk(b"IHDR", &ihdr));
        blob.extend_from_slice(&chunk(b"IDAT", &full[..full.len() / 2]));
        blob.extend_from_slice(&chunk(b"IEND", b""));
        expect_png_error(&blob, "truncated IDAT deflate stream");
    }

    #[test]
    fn trailing_garbage_after_iend() {
        let mut blob = valid_blob();
        blob.push(0);
        expect_png_error(&blob, "trailing garbage after IEND chunk");
    }

    #[test]
    fn interlace_method_2_rejected() {
        let scanlines = [0x00, 1, 2, 3, 4, 5, 6];
        let blob = hand_png(2, 1, 8, 2, &scanlines, None, None, 2, &[]);
        expect_png_error(&blob, "unsupported interlace method 2");
    }

    #[test]
    fn unknown_critical_chunk_rejected() {
        let scanlines = [0x00, 1, 2, 3];
        let extra: [(&[u8; 4], &[u8]); 1] = [(b"CRIT", b"boom")];
        let blob = hand_png(1, 1, 8, 2, &scanlines, None, None, 0, &extra);
        expect_png_error(&blob, "unknown critical chunk b'CRIT'");
    }

    #[test]
    fn decompressed_length_mismatch_rejected() {
        let scanlines = [0x00, 1, 2, 3];
        let blob = hand_png(2, 1, 8, 2, &scanlines, None, None, 0, &[]);
        expect_png_error(&blob, "decoded 4 scanline bytes, expected 7");
    }

    #[test]
    fn invalid_filter_type_rejected() {
        let scanlines = [0x09, 1, 2, 3];
        let blob = hand_png(1, 1, 8, 2, &scanlines, None, None, 0, &[]);
        expect_png_error(&blob, "invalid row filter type 9");
    }

    #[test]
    fn missing_plte_rejected_for_palette() {
        let blob = hand_png(1, 1, 8, 3, &[0x00, 0x00], None, None, 0, &[]);
        expect_png_error(&blob, "palette image missing PLTE chunk");
    }

    #[test]
    fn palette_index_out_of_range_rejected() {
        let plte = [10, 20, 30];
        let blob = hand_png(1, 1, 8, 3, &[0x00, 0x01], Some(&plte), None, 0, &[]);
        expect_png_error(&blob, "palette index 1 out of range (1 entries)");
    }

    #[test]
    fn resource_limit_boundaries_are_inclusive_without_allocating_images() {
        // Exercise the pure IHDR preflight directly at the DEFAULT (64 Mi)
        // ceilings: equality remains valid, but no test needs to inflate or
        // allocate a 64 Mi-pixel image.
        for header in [
            // Skinny-image dimension boundary (fixed, independent of pixels).
            Header {
                width: MAX_DIMENSION,
                height: 1,
                bit_depth: 1,
                color_type: 0,
                interlaced: false,
            },
            // Pixel boundary: exactly 64 Mi-pixels (8192*8192), low bit depth
            // so the scanline ceiling is far away.
            Header {
                width: 8192,
                height: 8192,
                bit_depth: 1,
                color_type: 0,
                interlaced: false,
            },
            // Scanline boundary: 16-bit RGBA just under the 512 MiB derived
            // scanline ceiling — 8192*8191 pixels < 64 Mi, and
            // 8191*(1 + 8192*8) = 536,813,567 <= 536,870,912.
            Header {
                width: 8192,
                height: 8191,
                bit_depth: 16,
                color_type: 6,
                interlaced: false,
            },
        ] {
            // Use the pure ceiling variant (explicit default) so this test
            // never reads the mutable global and cannot race the setter test.
            validate_header_resource_limits_with(&header, MAX_PIXELS)
                .expect("boundary header remains admitted");
        }
    }

    #[test]
    fn max_pixels_override_is_a_hard_bound_up_and_down() {
        // Drive the pure ceiling arithmetic directly (no global mutation, so it
        // cannot race concurrent default-relying decode tests): the
        // `--max-pixels` knob is `validate_header_resource_limits_with`'s
        // explicit ceiling. The global-wrapper path is exercised end-to-end via
        // the per-process CLI subprocesses in `tests/caps.rs`.
        let small = Header {
            width: 64,
            height: 64,
            bit_depth: 8,
            color_type: 2,
            interlaced: false,
        }; // 4096 pixels
        // Lower the ceiling below the small image -> rejected, reporting the
        // active ceiling (not the default constant).
        let err = validate_header_resource_limits_with(&small, 4095)
            .expect_err("4096-pixel image must reject under a 4095 ceiling");
        assert_eq!(err.kind(), crate::Kind::Data);
        assert_eq!(
            err.message(),
            "resource limit exceeded: image has 4096 pixels; maximum is 4095"
        );
        // Exactly at the ceiling -> admitted (the check is strictly `>`).
        validate_header_resource_limits_with(&small, 4096)
            .expect("4096 pixels admitted at a 4096 ceiling");
        // An image the DEFAULT (64 Mi) rejects, admitted once the ceiling is
        // raised above its pixel count; the derived scanline ceiling scales up
        // with it (134 Mi x 8 = 1 GiB, and this 1-bit image needs far less).
        let big = Header {
            width: 16384,
            height: 8192,
            bit_depth: 1,
            color_type: 0,
            interlaced: false,
        }; // 134,217,728 pixels (> 64 Mi default), low bit depth
        validate_header_resource_limits_with(&big, MAX_PIXELS)
            .expect_err("134 Mi-pixel image must reject under the 64 Mi default");
        validate_header_resource_limits_with(&big, 134_217_728)
            .expect("raised ceiling admits the larger image (scanlines scale with it)");
    }

    #[test]
    fn active_max_pixels_defaults_to_the_constant() {
        // The default (no `--max-pixels`) must be the 64 Mi constant. Read-only,
        // so it never races the concurrent decode tests. The global setter path
        // is exercised end-to-end per-process by `tests/caps.rs`.
        assert_eq!(active_max_pixels(), MAX_PIXELS);
        assert_eq!(MAX_PIXELS, 64 * 1024 * 1024);
    }
}

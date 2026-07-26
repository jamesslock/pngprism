//! Line-protocol driver for the pack zlib FFI differential gate.

use pngprism::pack::zlib_ffi_harness::{self, Deflater};
use std::error::Error;
use std::io::{self, BufRead};

fn decode_hex(text: &str) -> io::Result<Vec<u8>> {
    if text == "-" {
        return Ok(Vec::new());
    }
    if !text.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hex input must contain byte pairs",
        ));
    }
    (0..text.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&text[offset..offset + 2], 16).map_err(|error| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid hex: {error}"))
            })
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "-".to_string();
    }
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn one_argument<'a>(
    command: &str,
    parts: &mut impl Iterator<Item = &'a str>,
) -> io::Result<&'a str> {
    let argument = parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{command} requires one argument"),
        )
    })?;
    if parts.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{command} accepts exactly one argument"),
        ));
    }
    Ok(argument)
}

fn main() -> Result<(), Box<dyn Error>> {
    let stdin = io::stdin();
    let mut retained = Deflater::new()
        .map_err(|code| io::Error::other(format!("deflateInit2_ failed: {code}")))?;

    for line in stdin.lock().lines() {
        let line = line?;
        let mut parts = line.split_whitespace();
        let command = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command line"))?;
        match command {
            "V" => {
                if parts.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "V accepts no arguments",
                    )
                    .into());
                }
                println!("V {}", zlib_ffi_harness::runtime_version());
            }
            "P" => {
                let data = decode_hex(one_argument(command, &mut parts)?)?;
                let mut probe = retained.copy();
                let compressed = probe.compress(&data);
                let flushed = probe.flush_sync();
                println!("P {} {}", encode_hex(&compressed), encode_hex(&flushed));
            }
            "R" => {
                let data = decode_hex(one_argument(command, &mut parts)?)?;
                let compressed = retained.compress(&data);
                println!("R {}", encode_hex(&compressed));
            }
            "F" => {
                if parts.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "F accepts no arguments",
                    )
                    .into());
                }
                let mut probe = retained.copy();
                println!("F {}", encode_hex(&probe.finish()));
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown command: {other}"),
                )
                .into());
            }
        }
    }
    Ok(())
}

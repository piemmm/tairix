//! The `data:` URL a picture is embedded in an SVG document as: RFC 2397's
//! syntax, its payload in RFC 4648 base64 or percent-encoded.
//!
//! Reading takes a URL from an untrusted document, so the bytes it decodes
//! are bounded before they are held and every one is checked; a URL that is
//! not a data URL is no picture this reader can reach. The media type it
//! states is not believed: a picture is known by its content.

use alloc::vec::Vec;
use core::fmt::{self, Write};

use crate::DrawingError;

/// The bytes `url` carries, no more than `most` of them, or `None` for a URL
/// that is not a data URL.
///
/// # Errors
///
/// [`DrawingError::Invalid`] for a malformed data URL,
/// [`DrawingError::TooComplex`] for one carrying more than `most` bytes, and
/// [`DrawingError::OutOfMemory`] when they cannot be held.
pub(crate) fn decode(url: &str, most: usize) -> Result<Option<Vec<u8>>, DrawingError> {
    let url = url.trim();
    let Some(rest) = strip_prefix_ignoring_case(url, "data:") else {
        return Ok(None);
    };
    let (header, payload) = rest.split_once(',').ok_or(DrawingError::Invalid)?;
    let bytes = if strip_suffix_ignoring_case(header, ";base64").is_some() {
        decode_base64(payload, most)?
    } else {
        decode_percent(payload, most)?
    };
    Ok(Some(bytes))
}

/// Write `bytes` as a data URL of `media` with a base64 payload.
pub(crate) fn write(out: &mut dyn Write, media: &str, bytes: &[u8]) -> fmt::Result {
    write!(out, "data:{media};base64,")?;
    write_base64(out, bytes)
}

/// The base64 alphabet, RFC 4648 §4.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The input bytes encoded at once into a run on the stack.
const RUN: usize = 3 * 1024;

/// Write `bytes` in base64, padded, a run at a time.
fn write_base64(out: &mut dyn Write, bytes: &[u8]) -> fmt::Result {
    let mut run = [0_u8; RUN / 3 * 4];
    for chunk in bytes.chunks(RUN) {
        let mut len = 0;
        for group in chunk.chunks(3) {
            let word = (u32::from(group[0]) << 16)
                | (u32::from(group.get(1).copied().unwrap_or(0)) << 8)
                | u32::from(group.get(2).copied().unwrap_or(0));
            let digits = [18, 12, 6, 0].map(|shift| ALPHABET[((word >> shift) & 63) as usize]);
            let kept = group.len() + 1;
            for (at, digit) in digits.iter().enumerate() {
                run[len + at] = if at < kept { *digit } else { b'=' };
            }
            len += 4;
        }
        // Every digit is ASCII.
        out.write_str(core::str::from_utf8(&run[..len]).map_err(|_| fmt::Error)?)?;
    }
    Ok(())
}

/// The six bits a base64 digit stands for.
const fn sextet(digit: u8) -> Option<u32> {
    let value = match digit {
        b'A'..=b'Z' => digit - b'A',
        b'a'..=b'z' => digit - b'a' + 26,
        b'0'..=b'9' => digit - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(value as u32)
}

/// The bytes a base64 `payload` spells, ASCII white space between its digits
/// ignored and its padding optional.
fn decode_base64(payload: &str, most: usize) -> Result<Vec<u8>, DrawingError> {
    let digits = payload
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .take_while(|&byte| byte != b'=');
    let count = digits.clone().count();
    let decoded = count / 4 * 3 + (count % 4).saturating_sub(1);
    if count % 4 == 1 {
        return Err(DrawingError::Invalid);
    }
    if decoded > most {
        return Err(DrawingError::TooComplex);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(decoded)
        .map_err(|_| DrawingError::OutOfMemory)?;
    let (mut word, mut held) = (0_u32, 0_u32);
    for digit in digits {
        word = (word << 6) | sextet(digit).ok_or(DrawingError::Invalid)?;
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(((word >> held) & 0xff) as u8);
        }
    }
    // Past the digits there may be only padding and white space.
    let tail = payload
        .bytes()
        .skip_while(|&byte| byte != b'=')
        .all(|byte| byte == b'=' || byte.is_ascii_whitespace());
    if !tail {
        return Err(DrawingError::Invalid);
    }
    Ok(bytes)
}

/// The bytes a percent-encoded `payload` spells.
fn decode_percent(payload: &str, most: usize) -> Result<Vec<u8>, DrawingError> {
    let raw = payload.as_bytes();
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(raw.len().min(most))
        .map_err(|_| DrawingError::OutOfMemory)?;
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        let value = if byte == b'%' {
            let high = raw.get(at + 1).and_then(|&digit| hex(digit));
            let low = raw.get(at + 2).and_then(|&digit| hex(digit));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(DrawingError::Invalid);
            };
            at += 3;
            (high << 4) | low
        } else {
            at += 1;
            byte
        };
        if bytes.len() == most {
            return Err(DrawingError::TooComplex);
        }
        bytes.push(value);
    }
    Ok(bytes)
}

/// The value of one hex digit.
const fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn strip_prefix_ignoring_case<'t>(text: &'t str, prefix: &str) -> Option<&'t str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn strip_suffix_ignoring_case<'t>(text: &'t str, suffix: &str) -> Option<&'t str> {
    let cut = text.len().checked_sub(suffix.len())?;
    let tail = text.get(cut..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &text[..cut])
}

#[cfg(test)]
#[path = "data_url_tests.rs"]
mod tests;

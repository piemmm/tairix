//! A JPEG file's marker segments, as the structure report lists them.

use alloc::string::String;
use core::fmt::Write as _;

use super::{
    read_marker, read_segment, APP0, APP1, APP14, APP15, COM, DAC, DHP, DHT, DNL, DQT, DRI, EOI,
    EXP, RST0, RST7, SOF0, SOF1, SOF10, SOF11, SOF13, SOF14, SOF15, SOF2, SOF3, SOF5, SOF6, SOF7,
    SOF9, SOI, SOS,
};
use crate::inspect::dpi_of_ppm;
use crate::orientation::EXIF_IDENTIFIER;
use tairix_util::report::{clean, Builder};

/// The one marker besides the restarts that carries no length: temporary
/// private use in arithmetic coding.
const TEM: u8 = 0x01;

/// The identifier an `APP1` segment carrying XMP opens with.
const XMP_IDENTIFIER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The identifier an `APP2` segment carrying a piece of an ICC profile opens
/// with.
const ICC_IDENTIFIER: &[u8] = b"ICC_PROFILE\0";

/// Report `bytes`, a JPEG file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    out.heading(format_args!("JPEG image, {} bytes", bytes.len()));
    let mut at = 0;
    while at < bytes.len() && !out.full() {
        let Ok((marker, after)) = read_marker(bytes, at) else {
            out.stop(at, "no marker where one must be");
            return;
        };
        match marker {
            SOI => {
                out.record(at, "SOI", after - at, format_args!("start of image"));
                at = after;
            }
            EOI => {
                out.record(at, "EOI", after - at, format_args!("end of image"));
                at = after;
                break;
            }
            RST0..=RST7 | TEM => {
                out.record(
                    at,
                    "marker",
                    after - at,
                    format_args!("{marker:#04x}, standing alone"),
                );
                at = after;
            }
            _ => {
                let Ok((payload, end)) = read_segment(bytes, after) else {
                    out.stop(at, "a segment runs past the end of the file");
                    return;
                };
                segment(marker, (at, end - at), payload, out);
                at = end;
                if marker == SOS {
                    at = scan_end(bytes, at, out);
                }
            }
        }
    }
    if at < bytes.len() {
        out.record(
            at,
            "trailing",
            bytes.len() - at,
            format_args!("bytes after the image end"),
        );
    }
}

/// Where the entropy-coded data opening at `at` ends — at the first marker
/// that is neither a stuffed zero nor a restart — listing how long it ran.
fn scan_end(bytes: &[u8], at: usize, out: &mut Builder) -> usize {
    let mut cursor = at;
    let mut restarts = 0usize;
    while let Some(found) = bytes
        .get(cursor..)
        .and_then(|rest| rest.iter().position(|byte| *byte == 0xFF))
    {
        let marker_at = cursor + found;
        match bytes.get(marker_at + 1) {
            Some(0x00 | 0xFF) => cursor = marker_at + 1,
            Some(RST0..=RST7) => {
                restarts += 1;
                cursor = marker_at + 2;
            }
            Some(_) => {
                data(at, marker_at, restarts, out);
                return marker_at;
            }
            None => break,
        }
    }
    data(at, bytes.len(), restarts, out);
    bytes.len()
}

/// List the entropy-coded data from `at` to `end`.
fn data(at: usize, end: usize, restarts: usize, out: &mut Builder) {
    if restarts == 0 {
        out.record(at, "data", end - at, format_args!("entropy-coded data"));
    } else {
        out.record(
            at,
            "data",
            end - at,
            format_args!("entropy-coded data, {restarts} restarts"),
        );
    }
}

/// List the segment `marker` at `at`, `total` bytes with its marker and
/// length, holding `payload`.
fn segment(marker: u8, (at, total): (usize, usize), payload: &[u8], out: &mut Builder) {
    let mut name = String::new();
    let mut detail = String::new();
    describe(marker, payload, (&mut name, &mut detail));
    out.record(at, &name, total, format_args!("{detail}"));
    match marker {
        APP1 => {
            if let Some(tiff) = payload.strip_prefix(EXIF_IDENTIFIER) {
                crate::exif::report(tiff, 1, out);
            }
        }
        COM => {
            out.fact(1, format_args!("\u{201c}{}\u{201d}", clean(payload)));
        }
        _ => {}
    }
}

/// The segment's name and what it declares.
fn describe(marker: u8, payload: &[u8], (name, detail): (&mut String, &mut String)) {
    let said = match marker {
        SOF0 | SOF1 | SOF2 | SOF3 | SOF5 | SOF6 | SOF7 | SOF9 | SOF10 | SOF11 | SOF13 | SOF14
        | SOF15 => {
            let _ = write!(name, "SOF{}", marker - SOF0);
            frame(marker, payload, detail)
        }
        DHT => {
            name.push_str("DHT");
            tables(payload, detail, "Huffman table")
        }
        DQT => {
            name.push_str("DQT");
            tables(payload, detail, "quantisation table")
        }
        DRI => {
            name.push_str("DRI");
            payload.first_chunk::<2>().is_some_and(|raw| {
                write!(detail, "restart every {} MCUs", u16::from_be_bytes(*raw)).is_ok()
            })
        }
        SOS => {
            name.push_str("SOS");
            payload
                .first()
                .is_some_and(|count| write!(detail, "start of scan, {count} components").is_ok())
        }
        DAC => {
            name.push_str("DAC");
            detail.write_str("arithmetic coding conditions").is_ok()
        }
        DNL => {
            name.push_str("DNL");
            detail.write_str("number of lines").is_ok()
        }
        DHP | EXP => {
            name.push_str(if marker == DHP { "DHP" } else { "EXP" });
            detail.write_str("hierarchical progression").is_ok()
        }
        APP0..=APP15 => {
            let _ = write!(name, "APP{}", marker - APP0);
            application(marker, payload, detail)
        }
        COM => {
            name.push_str("COM");
            detail.write_str("comment").is_ok()
        }
        _ => {
            let _ = write!(name, "{marker:#04x}");
            detail
                .write_str("a marker this report does not read")
                .is_ok()
        }
    };
    if !said {
        detail.clear();
        let _ = write!(detail, "malformed, {} bytes", payload.len());
    }
}

/// What a frame header declares: the coding process, precision, size and
/// each component's sampling.
fn frame(marker: u8, payload: &[u8], detail: &mut String) -> bool {
    let &[precision, h0, h1, w0, w1, count, ref components @ ..] = payload else {
        return false;
    };
    let process = match marker {
        SOF0 => "baseline",
        SOF1 => "extended sequential",
        SOF2 => "progressive",
        SOF3 => "lossless",
        SOF9 => "extended sequential, arithmetic",
        SOF10 => "progressive, arithmetic",
        SOF11 => "lossless, arithmetic",
        _ => "hierarchical",
    };
    let width = u16::from_be_bytes([w0, w1]);
    let height = u16::from_be_bytes([h0, h1]);
    let _ = write!(
        detail,
        "{process}, {precision}-bit, {width} \u{d7} {height}, {count} components"
    );
    for component in components
        .as_chunks::<3>()
        .0
        .iter()
        .take(usize::from(count))
    {
        let _ = write!(
            detail,
            "; {}: {}\u{d7}{}",
            component[0],
            component[1] >> 4,
            component[1] & 0x0F
        );
    }
    true
}

/// How many tables a `DHT` or `DQT` segment carries, by their identifiers.
fn tables(payload: &[u8], detail: &mut String, what: &str) -> bool {
    let _ = write!(detail, "{what}s, {} bytes", payload.len());
    true
}

/// What an application segment is, by the identifier it opens with.
fn application(marker: u8, payload: &[u8], detail: &mut String) -> bool {
    if marker == APP0 && payload.starts_with(b"JFIF\0") {
        return jfif(payload, detail);
    }
    if marker == APP0 && payload.starts_with(b"JFXX\0") {
        return detail.write_str("JFIF extension (thumbnail)").is_ok();
    }
    if marker == APP1 && payload.starts_with(EXIF_IDENTIFIER) {
        return detail.write_str("EXIF").is_ok();
    }
    if marker == APP1 && payload.starts_with(XMP_IDENTIFIER) {
        return write!(
            detail,
            "XMP metadata, {} bytes",
            payload.len() - XMP_IDENTIFIER.len()
        )
        .is_ok();
    }
    if marker == APP0 + 2 && payload.starts_with(ICC_IDENTIFIER) {
        let (Some(&part), Some(&parts)) = (
            payload.get(ICC_IDENTIFIER.len()),
            payload.get(ICC_IDENTIFIER.len() + 1),
        ) else {
            return false;
        };
        return write!(detail, "ICC profile, part {part} of {parts}").is_ok();
    }
    if marker == APP14 && payload.starts_with(b"Adobe") {
        let transform = match payload.get(11) {
            Some(0) => "RGB or CMYK",
            Some(1) => "YCbCr",
            Some(2) => "YCCK",
            _ => "unknown transform",
        };
        return write!(detail, "Adobe, {transform}").is_ok();
    }
    let identifier = payload.split(|byte| *byte == 0).next().unwrap_or_default();
    write!(
        detail,
        "application data \u{201c}{}\u{201d}",
        clean(identifier)
    )
    .is_ok()
}

/// What a JFIF header states: its version and density.
fn jfif(payload: &[u8], detail: &mut String) -> bool {
    let (Some(&major), Some(&minor), Some(&unit), Some(x), Some(y)) = (
        payload.get(5),
        payload.get(6),
        payload.get(7),
        crate::be_u16(payload, 8),
        crate::be_u16(payload, 10),
    ) else {
        return false;
    };
    let _ = write!(detail, "JFIF {major}.{minor:02}, ");
    let _ = match unit {
        1 => write!(detail, "{x} \u{d7} {y} dpi"),
        2 => write!(
            detail,
            "{x} \u{d7} {y} dots a centimetre ({} \u{d7} {} dpi)",
            dpi_of_ppm(u32::from(x) * 100),
            dpi_of_ppm(u32::from(y) * 100)
        ),
        _ => write!(detail, "pixel aspect {x}:{y}"),
    };
    true
}

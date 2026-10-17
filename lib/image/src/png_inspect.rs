//! A PNG file's chunks, as the structure report lists them.

use alloc::string::String;
use core::fmt::Write as _;

use super::{
    chunk_crc, ColourType, IDAT, IEND, IHDR, PHYS, PHYS_ASPECT, PHYS_METRE, PLTE, SIGNATURE, TRNS,
};
use crate::inspect::{clean, dpi_of_ppm, Builder};

/// The chunk types an animated PNG's frames are carried in, and the image
/// data they continue, which a report lists as one run each.
const FDAT: [u8; 4] = *b"fdAT";

/// Report `bytes`, a PNG file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    if !bytes.starts_with(&SIGNATURE) {
        out.stop(0, "no PNG signature");
        return;
    }
    out.heading(format_args!("PNG image, {} bytes", bytes.len()));
    out.record(0, "signature", SIGNATURE.len(), format_args!("PNG"));
    let mut at = SIGNATURE.len();
    let mut run: Option<Run> = None;
    while at < bytes.len() && !out.full() {
        let Some(header) = bytes.get(at..at + 8) else {
            out.stop(at, "a chunk header is cut short");
            return;
        };
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = [header[4], header[5], header[6], header[7]];
        let payload_at = at + 8;
        let Some(payload) = usize::try_from(length)
            .ok()
            .and_then(|length| bytes.get(payload_at..payload_at.checked_add(length)?))
        else {
            flush(&mut run, out);
            out.stop(at, "a chunk runs past the end of the file");
            return;
        };
        let end = payload_at + payload.len();
        let crc_ok = bytes.get(end..end + 4).map(|crc| {
            u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]) == chunk_crc(kind, payload)
        });
        let total = 12 + payload.len();
        if kind == IDAT || kind == FDAT {
            match &mut run {
                Some(open) if open.kind == kind => open.add(total, crc_ok == Some(true)),
                _ => {
                    flush(&mut run, out);
                    run = Some(Run::new(kind, at, total, crc_ok == Some(true)));
                }
            }
        } else {
            flush(&mut run, out);
            chunk(kind, (at, total), payload, out);
            if crc_ok != Some(true) {
                out.fact(0, format_args!("its CRC does not match its contents"));
            }
        }
        at = end + 4;
        if kind == IEND {
            break;
        }
    }
    flush(&mut run, out);
    if at < bytes.len() {
        out.record(
            at,
            "trailing",
            bytes.len() - at,
            format_args!("bytes after the image end"),
        );
    }
}

/// A run of image-data chunks, listed as one line.
struct Run {
    kind: [u8; 4],
    at: usize,
    chunks: usize,
    bytes: usize,
    bad_crcs: usize,
}

impl Run {
    fn new(kind: [u8; 4], at: usize, bytes: usize, crc_ok: bool) -> Self {
        Self {
            kind,
            at,
            chunks: 1,
            bytes,
            bad_crcs: usize::from(!crc_ok),
        }
    }

    fn add(&mut self, bytes: usize, crc_ok: bool) {
        self.chunks += 1;
        self.bytes += bytes;
        self.bad_crcs += usize::from(!crc_ok);
    }
}

/// List the run that was open, if any.
fn flush(run: &mut Option<Run>, out: &mut Builder) {
    let Some(run) = run.take() else {
        return;
    };
    let name = String::from_utf8_lossy(&run.kind);
    let what = if run.kind == IDAT {
        "image data"
    } else {
        "frame data"
    };
    let chunks = if run.chunks == 1 { "chunk" } else { "chunks" };
    out.record(
        run.at,
        &name,
        run.bytes,
        format_args!("{what}, {} {chunks}", run.chunks),
    );
    if run.bad_crcs > 0 {
        out.fact(0, format_args!("{} of them fail their CRC", run.bad_crcs));
    }
}

/// List the chunk `kind` at `at`, `total` bytes long with its header and CRC.
fn chunk(kind: [u8; 4], (at, total): (usize, usize), payload: &[u8], out: &mut Builder) {
    let mut name = String::new();
    crate::inspect::push_clean(&mut name, &kind, 8);
    let mut detail = String::new();
    describe(kind, payload, &mut detail);
    out.record(at, &name, total, format_args!("{detail}"));
    match &kind {
        b"eXIf" => crate::exif::report(payload, 1, out),
        b"tEXt" | b"zTXt" | b"iTXt" => text(kind, payload, out),
        _ => {}
    }
}

/// What the chunk `kind` holding `payload` declares, written onto `detail`.
fn describe(kind: [u8; 4], payload: &[u8], detail: &mut String) {
    let be = |at| crate::be_u32(payload, at);
    let said = match kind {
        IHDR => ihdr(payload, detail),
        PLTE => write!(detail, "palette, {} colours", payload.len() / 3).is_ok(),
        TRNS => write!(detail, "transparency, {} bytes", payload.len()).is_ok(),
        PHYS => match (be(0), be(4), payload.get(8)) {
            (Some(x), Some(y), Some(&PHYS_METRE)) => write!(
                detail,
                "density {x} \u{d7} {y} pixels a metre ({} \u{d7} {} dpi)",
                dpi_of_ppm(x),
                dpi_of_ppm(y)
            )
            .is_ok(),
            (Some(x), Some(y), Some(&PHYS_ASPECT)) => {
                write!(detail, "pixel aspect {x}:{y}").is_ok()
            }
            _ => false,
        },
        IEND => detail.write_str("image end").is_ok(),
        _ => match &kind {
            b"gAMA" => be(0).is_some_and(|gamma| {
                write!(detail, "gamma {}.{:05}", gamma / 100_000, gamma % 100_000).is_ok()
            }),
            b"sRGB" => payload.first().is_some_and(|intent| {
                let intent = match intent {
                    0 => "perceptual",
                    1 => "relative colorimetric",
                    2 => "saturation",
                    3 => "absolute colorimetric",
                    _ => "unknown intent",
                };
                write!(detail, "sRGB colour space, {intent}").is_ok()
            }),
            b"iCCP" => {
                let name = payload.split(|byte| *byte == 0).next().unwrap_or_default();
                write!(
                    detail,
                    "ICC profile \u{201c}{}\u{201d}, {} compressed bytes",
                    clean(name),
                    payload.len().saturating_sub(name.len() + 2)
                )
                .is_ok()
            }
            b"cHRM" => detail.write_str("chromaticities").is_ok(),
            b"bKGD" => detail.write_str("background colour").is_ok(),
            b"sBIT" => write!(
                detail,
                "significant bits {}",
                crate::inspect::hex(payload, 4)
            )
            .is_ok(),
            b"tIME" => time(payload, detail),
            b"tEXt" => detail.write_str("text").is_ok(),
            b"zTXt" => detail.write_str("compressed text").is_ok(),
            b"iTXt" => detail.write_str("international text").is_ok(),
            b"eXIf" => detail.write_str("EXIF").is_ok(),
            b"acTL" => match (be(0), be(4)) {
                (Some(frames), Some(0)) => {
                    write!(detail, "animation, {frames} frames, looping for ever").is_ok()
                }
                (Some(frames), Some(plays)) => {
                    write!(detail, "animation, {frames} frames, played {plays} times").is_ok()
                }
                _ => false,
            },
            b"fcTL" => frame_control(payload, detail),
            _ => {
                let class = if kind[0] & 0x20 == 0 {
                    "critical"
                } else {
                    "ancillary"
                };
                write!(detail, "{class} chunk this report does not read").is_ok()
            }
        },
    };
    if !said {
        detail.clear();
        let _ = write!(detail, "malformed, {} bytes", payload.len());
    }
}

/// What an `IHDR` payload declares.
fn ihdr(payload: &[u8], detail: &mut String) -> bool {
    let (Some(width), Some(height), Some(&[depth, colour, compression, filter, interlace])) = (
        crate::be_u32(payload, 0),
        crate::be_u32(payload, 4),
        payload.get(8..13).and_then(|tail| tail.first_chunk::<5>()),
    ) else {
        return false;
    };
    let model = match ColourType::from_byte(colour) {
        Ok(ColourType::Grey) => "greyscale",
        Ok(ColourType::Truecolour) => "RGB",
        Ok(ColourType::Indexed) => "indexed colour",
        Ok(ColourType::GreyAlpha) => "greyscale with alpha",
        Ok(ColourType::Rgba) => "RGB with alpha",
        Err(_) => "an unknown colour type",
    };
    let interlace = match interlace {
        0 => "not interlaced",
        1 => "Adam7 interlaced",
        _ => "unknown interlace",
    };
    write!(
        detail,
        "header: {width} \u{d7} {height}, {depth}-bit {model}, {interlace}{}",
        if compression == 0 && filter == 0 {
            ""
        } else {
            ", unknown compression or filter"
        }
    )
    .is_ok()
}

/// What a `tIME` payload states: when the picture was last changed.
fn time(payload: &[u8], detail: &mut String) -> bool {
    let (Some(year), Some(&[month, day, hour, minute, second])) = (
        payload
            .first_chunk::<2>()
            .map(|year| u16::from_be_bytes(*year)),
        payload.get(2..7).and_then(|tail| tail.first_chunk::<5>()),
    ) else {
        return false;
    };
    write!(
        detail,
        "last changed {year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC"
    )
    .is_ok()
}

/// What an `fcTL` payload says of the frame it opens.
fn frame_control(payload: &[u8], detail: &mut String) -> bool {
    let be = |at| crate::be_u32(payload, at);
    let (Some(sequence), Some(width), Some(height), Some(x), Some(y)) =
        (be(0), be(4), be(8), be(12), be(16))
    else {
        return false;
    };
    let (Some(delay), Some(denominator)) = (
        payload
            .get(20..22)
            .map(|n| u16::from_be_bytes([n[0], n[1]])),
        payload
            .get(22..24)
            .map(|n| u16::from_be_bytes([n[0], n[1]])),
    ) else {
        return false;
    };
    let denominator = if denominator == 0 { 100 } else { denominator };
    write!(
        detail,
        "frame {sequence}: {width} \u{d7} {height} at ({x}, {y}), shown {delay}/{denominator} s"
    )
    .is_ok()
}

/// A text chunk's keyword and, where it is stored plainly, its text.
fn text(kind: [u8; 4], payload: &[u8], out: &mut Builder) {
    let mut fields = payload.splitn(2, |byte| *byte == 0);
    let keyword = fields.next().unwrap_or_default();
    let rest = fields.next().unwrap_or_default();
    match &kind {
        b"tEXt" => {
            out.fact(
                1,
                format_args!("{}: \u{201c}{}\u{201d}", clean(keyword), clean(rest)),
            );
        }
        b"iTXt" => {
            let compressed = rest.first().is_some_and(|flag| *flag != 0);
            let mut parts = rest
                .get(2..)
                .unwrap_or_default()
                .splitn(3, |byte| *byte == 0);
            let language = parts.next().unwrap_or_default();
            let _translated = parts.next();
            let body = parts.next().unwrap_or_default();
            if compressed {
                out.fact(
                    1,
                    format_args!("{}: compressed, {} bytes", clean(keyword), body.len()),
                );
            } else {
                out.fact(
                    1,
                    format_args!(
                        "{} [{}]: \u{201c}{}\u{201d}",
                        clean(keyword),
                        clean(language),
                        clean(body)
                    ),
                );
            }
        }
        _ => {
            out.fact(
                1,
                format_args!(
                    "{}: compressed, {} bytes",
                    clean(keyword),
                    rest.len().saturating_sub(1)
                ),
            );
        }
    }
}

//! A WEBP file's RIFF chunks, as the structure report lists them.

use alloc::string::String;
use core::fmt::Write as _;

use super::{
    le_u24, Chunk, Chunks, ALPHA, ANIMATION, ANIMATION_FRAME, CHUNK_HEADER, EXTENDED, VP8_LOSSLESS,
    VP8_LOSSY,
};
use tairix_util::report::{push_clean, Builder};

/// Report `bytes`, a WEBP file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let Ok(mut chunks) = Chunks::open(bytes) else {
        out.stop(0, "no whole RIFF WEBP form");
        return;
    };
    out.heading(format_args!("WEBP image, {} bytes", bytes.len()));
    out.record(0, "RIFF", 12, format_args!("form WEBP"));
    walk(&mut chunks, 0, out);
}

/// List every chunk `chunks` walks, `depth` levels in.
fn walk(chunks: &mut Chunks<'_>, depth: usize, out: &mut Builder) {
    let mut frames = 0usize;
    while !out.full() {
        let header_at = chunks.base + chunks.pos;
        let chunk = match chunks.next() {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return,
            Err(_) => {
                out.stop(header_at, "a chunk runs past the end of its region");
                return;
            }
        };
        let mut name = String::new();
        push_clean(&mut name, &chunk.id, 8);
        let mut detail = String::new();
        describe(&chunk, &mut detail);
        let total = CHUNK_HEADER + chunk.payload.len();
        if depth == 0 {
            out.record(header_at, &name, total, format_args!("{detail}"));
        } else {
            out.fact(
                depth,
                format_args!("{name} at {header_at:#x}, {total} bytes: {detail}"),
            );
        }
        match &chunk.id {
            b"EXIF" => crate::exif::report(strip_exif_identifier(chunk.payload), depth + 1, out),
            // A frame holds a picture's chunks, never another frame, so only
            // the form's own frames are opened: nesting cannot run deep.
            id if *id == ANIMATION_FRAME && depth == 0 => {
                frames += 1;
                if let Some(nested) = chunk.payload.get(16..) {
                    let mut inner = Chunks::nested(nested, chunk.at + 16);
                    out.fact(depth + 1, format_args!("frame {frames}"));
                    walk(&mut inner, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// An EXIF chunk's TIFF header, past the `Exif\0\0` some writers put before it.
fn strip_exif_identifier(payload: &[u8]) -> &[u8] {
    payload
        .strip_prefix(crate::orientation::EXIF_IDENTIFIER)
        .unwrap_or(payload)
}

/// What `chunk` declares.
fn describe(chunk: &Chunk<'_>, detail: &mut String) {
    let payload = chunk.payload;
    let said = match chunk.id {
        VP8_LOSSY => lossy(payload, detail),
        VP8_LOSSLESS => lossless(payload, detail),
        EXTENDED => extended(payload, detail),
        ALPHA => payload.first().is_some_and(|header| {
            let compression = if header & 0x03 == 1 {
                "lossless"
            } else {
                "uncompressed"
            };
            write!(
                detail,
                "alpha plane, {compression}, filter {}",
                (header >> 2) & 3
            )
            .is_ok()
        }),
        ANIMATION => match (crate::le_u32(payload, 0), crate::le_u16(payload, 4)) {
            (Some(background), Some(0)) => write!(
                detail,
                "animation, background {background:#010x}, looping for ever"
            )
            .is_ok(),
            (Some(background), Some(loops)) => write!(
                detail,
                "animation, background {background:#010x}, played {loops} times"
            )
            .is_ok(),
            _ => false,
        },
        ANIMATION_FRAME => frame(payload, detail),
        _ => match &chunk.id {
            b"ICCP" => write!(detail, "ICC colour profile, {} bytes", payload.len()).is_ok(),
            b"EXIF" => detail.write_str("EXIF").is_ok(),
            b"XMP " => write!(detail, "XMP metadata, {} bytes", payload.len()).is_ok(),
            _ => detail
                .write_str("a chunk this report does not read")
                .is_ok(),
        },
    };
    if !said {
        detail.clear();
        let _ = write!(detail, "malformed, {} bytes", payload.len());
    }
}

/// What a lossy bitstream's frame header declares.
fn lossy(payload: &[u8], detail: &mut String) -> bool {
    let (Some(width), Some(height)) = (crate::le_u16(payload, 6), crate::le_u16(payload, 8)) else {
        return false;
    };
    let key = payload.first().is_some_and(|tag| tag & 1 == 0);
    write!(
        detail,
        "lossy bitstream, {} \u{d7} {}{}",
        width & 0x3FFF,
        height & 0x3FFF,
        if key { "" } else { ", not a key frame" }
    )
    .is_ok()
}

/// What a lossless bitstream's header declares.
fn lossless(payload: &[u8], detail: &mut String) -> bool {
    let (Some(&0x2F), Some(bits)) = (payload.first(), crate::le_u32(payload, 1)) else {
        return false;
    };
    write!(
        detail,
        "lossless bitstream, {} \u{d7} {}{}",
        (bits & 0x3FFF) + 1,
        ((bits >> 14) & 0x3FFF) + 1,
        if bits & (1 << 28) != 0 {
            ", with alpha"
        } else {
            ""
        }
    )
    .is_ok()
}

/// What the extended header declares: its features and its canvas.
fn extended(payload: &[u8], detail: &mut String) -> bool {
    let Some(&flags) = payload.first() else {
        return false;
    };
    if payload.len() < 10 {
        return false;
    }
    let _ = write!(
        detail,
        "extended: canvas {} \u{d7} {}",
        le_u24(payload, 4) + 1,
        le_u24(payload, 7) + 1
    );
    for (bit, feature) in [
        (0x20, "ICC profile"),
        (0x10, "alpha"),
        (0x08, "EXIF"),
        (0x04, "XMP"),
        (0x02, "animation"),
    ] {
        if flags & bit != 0 {
            let _ = write!(detail, ", {feature}");
        }
    }
    true
}

/// What an animation frame's header declares.
fn frame(payload: &[u8], detail: &mut String) -> bool {
    if payload.len() < 16 {
        return false;
    }
    let flags = payload[15];
    write!(
        detail,
        "frame {} \u{d7} {} at ({}, {}), shown {} ms, {}, {}",
        le_u24(payload, 6) + 1,
        le_u24(payload, 9) + 1,
        le_u24(payload, 0) * 2,
        le_u24(payload, 3) * 2,
        le_u24(payload, 12),
        if flags & 0x02 == 0 {
            "blended"
        } else {
            "not blended"
        },
        if flags & 0x01 == 0 {
            "left in place"
        } else {
            "cleared after"
        }
    )
    .is_ok()
}

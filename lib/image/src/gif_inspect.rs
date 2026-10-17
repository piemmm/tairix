//! A GIF file's blocks, as the structure report lists them.

use alloc::string::String;
use core::fmt::Write as _;

use super::{
    palette_len, word, Reader, APPLICATION_ID_LEN, EXTENSION_INTRODUCER, IMAGE_DESCRIPTOR_LEN,
    IMAGE_SEPARATOR, LABEL_APPLICATION, LABEL_GRAPHIC_CONTROL, LABEL_PLAIN_TEXT, MAGIC,
    NETSCAPE_ID, SCREEN_DESCRIPTOR_LEN, TRAILER, VERSIONS,
};
use crate::inspect::{clean, Builder};

/// The comment extension's label.
const LABEL_COMMENT: u8 = 0xFE;

/// Report `bytes`, a GIF file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let header_len = MAGIC.len() + 3;
    let (Some(signature), Some(descriptor)) = (
        bytes.get(..header_len),
        bytes.get(header_len..header_len + SCREEN_DESCRIPTOR_LEN),
    ) else {
        out.stop(0, "the header is cut short");
        return;
    };
    let version = &signature[MAGIC.len()..];
    if !signature.starts_with(&MAGIC) || !VERSIONS.iter().any(|known| known == version) {
        out.stop(0, "no GIF signature");
        return;
    }
    out.heading(format_args!("GIF image, {} bytes", bytes.len()));
    out.record(
        0,
        "header",
        header_len,
        format_args!("GIF{}", String::from_utf8_lossy(version)),
    );
    let packed = descriptor[4];
    out.record(
        header_len,
        "screen",
        SCREEN_DESCRIPTOR_LEN,
        format_args!(
            "logical screen {} \u{d7} {}, background colour {}, aspect byte {}",
            word(descriptor, 0),
            word(descriptor, 2),
            descriptor[5],
            descriptor[6]
        ),
    );
    let mut at = header_len + SCREEN_DESCRIPTOR_LEN;
    if packed & 0x80 != 0 {
        let len = palette_len(packed);
        out.record(
            at,
            "colours",
            len,
            format_args!("global colour table, {} colours", len / 3),
        );
        at += len;
    }
    let mut frames = 0usize;
    while !out.full() {
        let Some(&introducer) = bytes.get(at) else {
            out.stop(at, "the file ends before its trailer");
            return;
        };
        let next = match introducer {
            EXTENSION_INTRODUCER => extension(bytes, at, out),
            IMAGE_SEPARATOR => {
                frames += 1;
                image(bytes, at, frames, out)
            }
            TRAILER => {
                out.record(
                    at,
                    "trailer",
                    1,
                    format_args!("end of the file, {frames} frames"),
                );
                at += 1;
                break;
            }
            _ => {
                out.stop(at, "a block of no known kind");
                return;
            }
        };
        let Some(next) = next else {
            out.stop(at, "a block runs past the end of the file");
            return;
        };
        at = next;
    }
    if at < bytes.len() && !out.full() {
        out.record(
            at,
            "trailing",
            bytes.len() - at,
            format_args!("bytes after the trailer"),
        );
    }
}

/// List the extension at `at`, answering where the next block begins.
fn extension(bytes: &[u8], at: usize, out: &mut Builder) -> Option<usize> {
    let mut reader = Reader::new(bytes, at + 1);
    let label = reader.byte().ok()?;
    let first = reader.pos;
    let mut detail = String::new();
    match label {
        LABEL_GRAPHIC_CONTROL => {
            let size = reader.byte().ok()?;
            let block = reader.take(usize::from(size)).ok()?;
            let packed = *block.first()?;
            let disposal = match (packed >> 2) & 7 {
                0 => "no disposal",
                1 => "left in place",
                2 => "cleared to the background",
                3 => "restored to the previous",
                _ => "an unknown disposal",
            };
            let _ = write!(
                detail,
                "graphic control: shown {} \u{d7} 10 ms, {disposal}",
                word(block, 1)
            );
            if packed & 1 != 0 {
                let _ = write!(detail, ", colour {} transparent", block.get(3)?);
            }
        }
        LABEL_COMMENT => {
            detail.push_str("comment");
        }
        LABEL_APPLICATION => {
            let size = reader.byte().ok()?;
            let id = reader.take(usize::from(size)).ok()?;
            if id.len() == usize::from(APPLICATION_ID_LEN) && id == NETSCAPE_ID {
                detail.push_str("application NETSCAPE2.0: animation loop");
            } else {
                let _ = write!(detail, "application \u{201c}{}\u{201d}", clean(id));
            }
        }
        LABEL_PLAIN_TEXT => detail.push_str("plain text"),
        _ => {
            let _ = write!(detail, "extension {label:#04x}");
        }
    }
    let mut text = alloc::vec::Vec::new();
    reader.pos = first;
    if label == LABEL_COMMENT {
        loop {
            let len = reader.byte().ok()?;
            if len == 0 {
                break;
            }
            let block = reader.take(usize::from(len)).ok()?;
            if text.len() < crate::inspect::MAX_TEXT_SHOWN {
                text.extend_from_slice(block);
            }
        }
    } else {
        reader.skip_sub_blocks().ok()?;
    }
    out.record(at, "extension", reader.pos - at, format_args!("{detail}"));
    if label == LABEL_COMMENT {
        out.fact(1, format_args!("\u{201c}{}\u{201d}", clean(&text)));
    }
    Some(reader.pos)
}

/// List frame `index`'s image at `at`, answering where the next block
/// begins.
fn image(bytes: &[u8], at: usize, index: usize, out: &mut Builder) -> Option<usize> {
    let mut reader = Reader::new(bytes, at + 1);
    let descriptor = reader.take(IMAGE_DESCRIPTOR_LEN).ok()?;
    let packed = descriptor[8];
    let local = if packed & 0x80 == 0 {
        0
    } else {
        let len = palette_len(packed);
        reader.take(len).ok()?;
        len / 3
    };
    let code_size = reader.byte().ok()?;
    let data = reader.pos;
    reader.skip_sub_blocks().ok()?;
    let mut detail = alloc::format!(
        "frame {index}: {} \u{d7} {} at ({}, {}), {}minimum code size {code_size}",
        word(descriptor, 4),
        word(descriptor, 6),
        word(descriptor, 0),
        word(descriptor, 2),
        if packed & 0x40 == 0 {
            ""
        } else {
            "interlaced, "
        }
    );
    if local > 0 {
        let _ = write!(detail, ", local colour table of {local}");
    }
    let _ = write!(detail, ", {} bytes of data", reader.pos - data);
    out.record(at, "image", reader.pos - at, format_args!("{detail}"));
    Some(reader.pos)
}

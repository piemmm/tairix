//! An icon or cursor file's directory and pictures, as the structure report
//! lists them.

use super::{
    field, CUR_SIGNATURE, DIRECTORY_LEN, ENTRY_BYTES_AT, ENTRY_LEN, ENTRY_OFFSET_AT, ICO_SIGNATURE,
};
use crate::inspect::Builder;

/// Report `bytes`, an icon or cursor file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let cursor = bytes.starts_with(&CUR_SIGNATURE);
    if !cursor && !bytes.starts_with(&ICO_SIGNATURE) {
        out.stop(0, "no icon or cursor signature");
        return;
    }
    let Some(count) = crate::le_u16(bytes, 4) else {
        out.stop(0, "the directory is cut short");
        return;
    };
    let kind = if cursor { "cursor" } else { "icon" };
    out.heading(format_args!("Windows {kind} file, {} bytes", bytes.len()));
    out.record(
        0,
        "directory",
        DIRECTORY_LEN,
        format_args!("{count} pictures"),
    );
    for index in 0..usize::from(count) {
        if out.full() {
            return;
        }
        let at = DIRECTORY_LEN + index * ENTRY_LEN;
        let Some(entry) = bytes.get(at..at + ENTRY_LEN) else {
            out.stop(at, "the directory runs past the end of the file");
            return;
        };
        let side = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
        let (first, second) = (
            crate::le_u16(entry, 4).unwrap_or_default(),
            crate::le_u16(entry, 6).unwrap_or_default(),
        );
        let fields = if cursor {
            alloc::format!("hot spot ({first}, {second})")
        } else {
            alloc::format!("{second} bits a pixel")
        };
        let (Ok(length), Ok(offset)) = (
            field(bytes, at + ENTRY_BYTES_AT),
            field(bytes, at + ENTRY_OFFSET_AT),
        ) else {
            return;
        };
        out.record(
            at,
            "entry",
            ENTRY_LEN,
            format_args!(
                "picture {}: {} \u{d7} {}, {fields}, {length} bytes at {offset:#x}",
                index + 1,
                side(entry[0]),
                side(entry[1])
            ),
        );
    }
    for index in 0..usize::from(count) {
        if out.full() {
            return;
        }
        let at = DIRECTORY_LEN + index * ENTRY_LEN;
        let (Ok(length), Ok(offset)) = (
            field(bytes, at + ENTRY_BYTES_AT),
            field(bytes, at + ENTRY_OFFSET_AT),
        ) else {
            return;
        };
        let Some(picture) = offset
            .checked_add(length)
            .and_then(|end| bytes.get(offset..end))
        else {
            out.stop(offset, "a picture runs past the end of the file");
            continue;
        };
        if picture.starts_with(&crate::PNG_SIGNATURE) {
            out.record(
                offset,
                "PNG",
                length,
                format_args!("picture {} as a whole PNG file", index + 1),
            );
        } else if let Some(header) = crate::bmp::inspect::dib_header(picture, offset, out) {
            let rest = length.saturating_sub(header);
            out.fact(
                1,
                format_args!("then {rest} bytes of colours, pixels and mask"),
            );
        }
    }
}

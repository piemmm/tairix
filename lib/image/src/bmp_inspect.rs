//! A BMP file's headers, colour table and pixels, as the structure report
//! lists them.

use core::fmt::Write as _;

use super::{
    BI_ALPHABITFIELDS, BI_BITFIELDS, BI_RGB, BI_RLE4, BI_RLE8, COLOUR_SPACE_AT, CORE_HEADER_LEN,
    DENSITY_AT, FILE_HEADER_LEN, FILE_HEADER_PIXEL_OFFSET_AT, INFO_HEADER_LEN, LCS_CALIBRATED_RGB,
    LCS_SRGB, LCS_WINDOWS_COLOR_SPACE, MAGIC, MASK_AT, V2_HEADER_LEN, V3_HEADER_LEN, V4_HEADER_LEN,
    V5_HEADER_LEN,
};
use crate::inspect::{dpi_of_ppm, Builder};

/// Report `bytes`, a BMP file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let (Some(size), Some(pixels)) = (
        crate::le_u32(bytes, 2),
        crate::le_u32(bytes, FILE_HEADER_PIXEL_OFFSET_AT),
    ) else {
        out.stop(0, "the file header is cut short");
        return;
    };
    if !bytes.starts_with(&MAGIC) {
        out.stop(0, "no BMP signature");
        return;
    }
    out.heading(format_args!("BMP image, {} bytes", bytes.len()));
    out.record(
        0,
        "file header",
        FILE_HEADER_LEN,
        format_args!("states {size} bytes, pixels at {pixels:#x}"),
    );
    let dib = bytes.get(FILE_HEADER_LEN..).unwrap_or_default();
    let Some(length) = dib_header(dib, FILE_HEADER_LEN, out) else {
        return;
    };
    let pixels = usize::try_from(pixels).unwrap_or(usize::MAX);
    let table = FILE_HEADER_LEN + length;
    if pixels > table && pixels <= bytes.len() {
        out.record(
            table,
            "colours",
            pixels - table,
            format_args!("colour table or masks"),
        );
    }
    if pixels < bytes.len() {
        out.record(
            pixels,
            "pixels",
            bytes.len() - pixels,
            format_args!("pixel array"),
        );
    } else {
        out.stop(pixels, "the pixel array lies past the end of the file");
    }
}

/// List the device-independent bitmap header opening `dib`, which lies at
/// `at` in its file; answers its length. Shared with icon entries, whose
/// pictures are the same header and pixels without the file header.
pub(crate) fn dib_header(dib: &[u8], at: usize, out: &mut Builder) -> Option<usize> {
    let Some(length) = crate::le_u32(dib, 0) else {
        out.stop(at, "the bitmap header is cut short");
        return None;
    };
    let name = match length {
        CORE_HEADER_LEN => "BITMAPCOREHEADER",
        INFO_HEADER_LEN => "BITMAPINFOHEADER",
        V2_HEADER_LEN => "BITMAPV2INFOHEADER",
        V3_HEADER_LEN => "BITMAPV3INFOHEADER",
        V4_HEADER_LEN => "BITMAPV4HEADER",
        V5_HEADER_LEN => "BITMAPV5HEADER",
        _ => "a bitmap header of no known size",
    };
    let held = usize::try_from(length)
        .ok()
        .filter(|length| *length <= dib.len());
    let Some(held) = held else {
        out.stop(at, "the bitmap header runs past the end of the file");
        return None;
    };
    let mut detail = alloc::string::String::from(name);
    if length == CORE_HEADER_LEN {
        let (Some(width), Some(height), Some(bits)) = (
            crate::le_u16(dib, 4),
            crate::le_u16(dib, 6),
            crate::le_u16(dib, 10),
        ) else {
            return None;
        };
        let _ = write!(detail, ": {width} \u{d7} {height}, {bits} bits a pixel");
        out.record(at, "bitmap", held, format_args!("{detail}"));
        return Some(held);
    }
    let read = |offset| crate::le_u32(dib, offset);
    let (Some(width), Some(height), Some(bits), Some(compression)) =
        (read(4), read(8), crate::le_u16(dib, 14), read(16))
    else {
        out.record(at, "bitmap", held, format_args!("{detail}, cut short"));
        return Some(held);
    };
    let height = i32::from_ne_bytes(height.to_ne_bytes());
    let order = if height < 0 {
        "top row first"
    } else {
        "bottom row first"
    };
    let encoding = match compression {
        BI_RGB => "uncompressed",
        BI_RLE8 => "8-bit run-length",
        BI_RLE4 => "4-bit run-length",
        BI_BITFIELDS => "bit fields",
        BI_ALPHABITFIELDS => "bit fields with alpha",
        4 => "embedded JPEG",
        5 => "embedded PNG",
        _ => "an unknown encoding",
    };
    let _ = write!(
        detail,
        ": {width} \u{d7} {}, {order}, {bits} bits a pixel, {encoding}",
        height.unsigned_abs()
    );
    out.record(at, "bitmap", held, format_args!("{detail}"));
    facts(dib, length, out);
    Some(held)
}

/// What an info header and its later versions state beyond the geometry:
/// density, colour counts, channel masks and colour space.
fn facts(dib: &[u8], length: u32, out: &mut Builder) {
    let read = |offset| crate::le_u32(dib, offset);
    if let (Some(x), Some(y)) = (read(DENSITY_AT[0]), read(DENSITY_AT[1])) {
        if x > 0 || y > 0 {
            out.fact(
                1,
                format_args!(
                    "density {x} \u{d7} {y} pixels a metre ({} \u{d7} {} dpi)",
                    dpi_of_ppm(x),
                    dpi_of_ppm(y)
                ),
            );
        }
    }
    if let (Some(used), Some(important)) = (read(32), read(36)) {
        if used > 0 || important > 0 {
            out.fact(
                1,
                format_args!("{used} colours used, {important} important"),
            );
        }
    }
    if length >= V2_HEADER_LEN {
        let masks = MASK_AT.map(read);
        if let [Some(r), Some(g), Some(b), alpha] = masks {
            out.fact(
                1,
                format_args!(
                    "masks red {r:#010x}, green {g:#010x}, blue {b:#010x}, alpha {:#010x}",
                    alpha.unwrap_or(0)
                ),
            );
        }
    }
    if length >= V4_HEADER_LEN {
        let space = match read(COLOUR_SPACE_AT) {
            Some(LCS_CALIBRATED_RGB) => "calibrated RGB",
            Some(LCS_SRGB) => "sRGB",
            Some(LCS_WINDOWS_COLOR_SPACE) => "the system's colour space",
            Some(0x4C49_4E4B) => "a linked colour profile",
            Some(0x4D42_4544) => "an embedded colour profile",
            _ => "an unknown colour space",
        };
        out.fact(1, format_args!("colour space: {space}"));
    }
}

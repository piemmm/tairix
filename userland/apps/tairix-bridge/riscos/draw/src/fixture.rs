//! Draw files made to order, for this crate's tests and the Font Manager's,
//! which render them through DrawFile.

use alloc::vec::Vec;

use tairix_drawing::riscos::format::{
    BARE_HEADER, FONT_TABLE, JPEG, MAGIC, MAJOR_VERSION, OBJECT_HEADER, OBJECT_SIZE, TEXT,
    TEXT_AREA, TEXT_COLUMN, TRANSFORMED_TEXT,
};

/// The creator a made file names, padded to its twelve bytes.
const CREATOR: &[u8; 12] = b"TAIRiX      ";

/// The text area column the probes lay out and paint in: 100 points to 200
/// both ways, in draw units.
pub const COLUMN: [i32; 4] = [64_000, 64_000, 128_000, 128_000];

/// A Draw file of `objects`, its box `bounds`.
#[must_use]
pub fn file(bounds: [i32; 4], objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(MAJOR_VERSION.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend_from_slice(CREATOR);
    bytes.extend(bounds.iter().flat_map(|v| v.to_le_bytes()));
    for object in objects {
        bytes.extend_from_slice(object);
    }
    bytes
}

/// `values` as a file holds them, each a little-endian word.
#[must_use]
pub fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// A font table naming each of `fonts` by its number.
#[must_use]
pub fn font_table(fonts: &[(u8, &[u8])]) -> Vec<u8> {
    let mut bytes = alloc::vec![0; BARE_HEADER];
    for &(number, name) in fonts {
        bytes.push(number);
        bytes.extend_from_slice(name);
        bytes.push(0);
    }
    framed(FONT_TABLE, bytes)
}

/// A text, or a transformed one where `own` gives its matrix and flags: its
/// box, colours, font, sizes and base point in draw units, and `string`.
#[must_use]
pub fn text(text: &Text<'_>, own: Option<([i32; 6], u32)>) -> Vec<u8> {
    let mut fields: Vec<u32> = alloc::vec![0, 0];
    fields.extend(text.bounds.map(i32::cast_unsigned));
    if let Some((matrix, flags)) = own {
        fields.extend(matrix.map(i32::cast_unsigned));
        fields.push(flags);
    }
    fields.extend([text.foreground, text.background, u32::from(text.font)]);
    fields.extend([text.sizes.0, text.sizes.1, text.at.0, text.at.1].map(i32::cast_unsigned));
    let mut bytes = words(&fields);
    bytes.extend_from_slice(text.string);
    bytes.push(0);
    framed(
        if own.is_some() {
            TRANSFORMED_TEXT
        } else {
            TEXT
        },
        bytes,
    )
}

/// What a text object is made of.
#[derive(Clone, Copy, Debug)]
pub struct Text<'a> {
    /// Its box, in draw units.
    pub bounds: [i32; 4],
    /// Its colours, as RGB words.
    pub foreground: u32,
    /// What it is anti-aliased against.
    pub background: u32,
    /// Its font's number in the font table, nought for the system font.
    pub font: u8,
    /// Its width and height, in draw units.
    pub sizes: (i32, i32),
    /// Its base point.
    pub at: (i32, i32),
    /// Its characters.
    pub string: &'a [u8],
}

/// `bytes`, whose first two words are left for them, made an object of
/// type `kind`: padded to a word, its type and size written.
fn framed(kind: u32, mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    bytes[..OBJECT_SIZE].copy_from_slice(&kind.to_le_bytes());
    bytes[OBJECT_SIZE..BARE_HEADER].copy_from_slice(&size.to_le_bytes());
    bytes
}

/// A text area object: its box, a column object for each of `columns`, its
/// colours and `text`, terminated and padded to a word.
#[must_use]
pub fn text_area(
    bounds: [i32; 4],
    columns: &[[i32; 4]],
    foreground: u32,
    background: u32,
    text: &[u8],
) -> Vec<u8> {
    let column_size = u32::try_from(OBJECT_HEADER).unwrap_or(u32::MAX);
    let mut fields: Vec<u32> = alloc::vec![0, 0];
    fields.extend(bounds.map(i32::cast_unsigned));
    for column in columns {
        fields.extend([TEXT_COLUMN, column_size]);
        fields.extend(column.map(i32::cast_unsigned));
    }
    fields.extend([0, 0, 0, foreground, background]);
    let mut bytes = words(&fields);
    bytes.extend_from_slice(text);
    bytes.push(0);
    framed(TEXT_AREA, bytes)
}

/// A JPEG object: its box, its width and height in draw units, its dots to
/// the inch across and up, its matrix, and `data`, padded to a word.
#[must_use]
pub fn jpeg(
    bounds: [i32; 4],
    sizes: [u32; 2],
    dpi: [u32; 2],
    matrix: [i32; 6],
    data: &[u8],
) -> Vec<u8> {
    let mut fields: Vec<u32> = alloc::vec![0, 0];
    fields.extend(bounds.map(i32::cast_unsigned));
    fields.extend(sizes);
    fields.extend(dpi);
    fields.extend(matrix.map(i32::cast_unsigned));
    fields.push(u32::try_from(data.len()).unwrap_or(u32::MAX));
    let mut bytes = words(&fields);
    bytes.extend_from_slice(data);
    framed(JPEG, bytes)
}

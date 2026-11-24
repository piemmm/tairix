//! The Draw file format's one definition (RISC OS PRM volume 4, appendix
//! E): its header, its object types, their framing and the fields of those
//! RISC OS draws, the path elements the Draw module reads, a path style's
//! fields and the stroke the format fixes, with the walk every reader of a
//! file's objects shares.

use tairix_util::endian::le_u32;

/// What every Draw file begins with.
pub const MAGIC: &[u8; 4] = b"Draw";

/// The format's major version, the one RISC OS writes and reads.
pub const MAJOR_VERSION: u32 = 201;

/// Where the header holds its major version.
pub const HEADER_VERSION: usize = MAGIC.len();

/// The header's length, in bytes: the magic, the two versions, the program's
/// name and the drawing's bounds.
pub const HEADER: usize = HEADER_BOUNDS + 2 * POINT;

/// Where the header holds its bounds.
pub const HEADER_BOUNDS: usize = 24;

/// A point, in bytes: its x, then its y, each a signed word.
pub const POINT: usize = 8;

/// Where an object holds its size, after its type.
pub const OBJECT_SIZE: usize = 4;

/// The header of an object with no bounds, a font table, in bytes: its type
/// and its size.
pub const BARE_HEADER: usize = OBJECT_SIZE + 4;

/// Where an object's header holds its bounds, after its type and size: its
/// low corner, then its high one.
pub const OBJECT_BOUNDS: usize = BARE_HEADER;

/// An object's header, in bytes: its type, its size and its bounds.
pub const OBJECT_HEADER: usize = OBJECT_BOUNDS + 2 * POINT;

/// A transformed object's matrix, in bytes: four multipliers of
/// [`FRACTION_BITS`] bits of fraction, then a translation in draw units.
pub const MATRIX: usize = 24;

/// The bits of fraction a matrix's multipliers hold.
pub const FRACTION_BITS: u32 = 16;

/// One, as a matrix's multipliers hold it.
pub const FIXED_ONE: i32 = 1 << FRACTION_BITS;

/// The matrix that leaves every point where it is, as its six words.
pub const IDENTITY: [u32; 6] = {
    let one = FIXED_ONE.cast_unsigned();
    [one, 0, 0, one, 0, 0]
};

/// The six words of the matrix at `at` of `bytes`, each `None` where `bytes`
/// end before it.
#[must_use]
pub fn matrix_words(bytes: &[u8], at: usize) -> [Option<u32>; 6] {
    core::array::from_fn(|index| le_u32(bytes, at.checked_add(4 * index)?))
}

/// The colour word that stands for no colour at all.
pub const NO_COLOUR: u32 = 0xFFFF_FFFF;

/// The font table: font numbers and their names.
pub const FONT_TABLE: u32 = 0;
/// A line of text.
pub const TEXT: u32 = 1;
/// A path, filled and stroked.
pub const PATH: u32 = 2;
/// A sprite, filling its bounds.
pub const SPRITE: u32 = 5;
/// A named group of objects.
pub const GROUP: u32 = 6;
/// An object wrapped with a program's tag and data.
pub const TAGGED: u32 = 7;
/// Text flowed through columns.
pub const TEXT_AREA: u32 = 9;
/// A text area's column.
pub const TEXT_COLUMN: u32 = 10;
/// The paper and the editor's settings.
pub const OPTIONS: u32 = 11;
/// A line of text through a matrix.
pub const TRANSFORMED_TEXT: u32 = 12;
/// A sprite through a matrix.
pub const TRANSFORMED_SPRITE: u32 = 13;
/// A JPEG image.
pub const JPEG: u32 = 16;

/// The path's end: the Draw module's path format, which a path object
/// holds, codes each element in its first word's low byte.
pub const END: u32 = 0;
/// A word follows: the address the path goes on at.
pub const CONTINUATION: u32 = 1;
/// A subpath begins.
pub const MOVE: u32 = 2;
/// A move starting a subpath that adds nothing to the winding number.
pub const MOVE_UNWOUND: u32 = 3;
/// The sub-path closed, its closing edge left undrawn.
pub const CLOSE_WITH_GAP: u32 = 4;
/// The sub-path closed by a line.
pub const CLOSE: u32 = 5;
/// A cubic Bézier curve: two handles, then its end.
pub const CURVE: u32 = 6;
/// A segment edging a fill that a stroke leaves undrawn.
pub const GAP: u32 = 7;
/// A straight line.
pub const LINE: u32 = 8;

/// The bits of an element word its code is in; the rest are the client's.
pub const TAG: u32 = 0xFF;

/// Where an element's points begin, after the word its code is in; a
/// continuation's address is there too.
pub const ELEMENT_POINTS: usize = 4;

/// How many points follow the code of element `tag` — three for a curve, one
/// for a move, a gap or a line, none for a close or the end — or `None` for a
/// continuation, which an address follows, and a code that names no element.
#[must_use]
pub const fn element_points(tag: u32) -> Option<usize> {
    match tag {
        END | CLOSE_WITH_GAP | CLOSE => Some(0),
        MOVE | MOVE_UNWOUND | GAP | LINE => Some(1),
        CURVE => Some(3),
        _ => None,
    }
}

/// The bytes element `tag` takes: its code's word and its points.
#[must_use]
pub const fn element_bytes(tag: u32) -> Option<usize> {
    match element_points(tag) {
        Some(points) => Some(ELEMENT_POINTS + POINT * points),
        None => None,
    }
}

/// The two bits a path style holds a join or a cap in.
pub const CODE: u32 = 0b11;

/// A mitred join, as a path style's low two bits and the Draw module's cap
/// and join block hold one.
pub const JOIN_MITRE: u32 = 0;
/// A round join.
pub const JOIN_ROUND: u32 = 1;
/// A bevelled join.
pub const JOIN_BEVEL: u32 = 2;

/// A butt cap, as a path style and the Draw module's block hold each end's.
pub const CAP_BUTT: u32 = 0;
/// A round cap.
pub const CAP_ROUND: u32 = 1;
/// A square cap.
pub const CAP_SQUARE: u32 = 2;
/// A triangular cap.
pub const CAP_TRIANGLE: u32 = 3;

/// Where a path style holds the cap at its end.
pub const END_CAP_SHIFT: u32 = 2;
/// Where a path style holds the cap at its start.
pub const START_CAP_SHIFT: u32 = 4;
/// The path is filled by the even-odd rule, not the non-zero one.
pub const WINDING_EVEN_ODD: u32 = 1 << 6;
/// A dash pattern follows the style.
pub const DASHED: u32 = 1 << 7;
/// Where a path style holds a triangle cap's width.
pub const TRIANGLE_WIDTH_SHIFT: u32 = 16;
/// Where a path style holds a triangle cap's length.
pub const TRIANGLE_LENGTH_SHIFT: u32 = 24;

/// The bits a path style holds each of a triangle cap's sizes in, each in
/// sixteenths of the line's width.
pub const TRIANGLE_SIZE: u32 = 0xFF;

/// The parts of its line's width a triangle cap's sizes are counted in.
pub const TRIANGLE_UNIT: u32 = 16;

/// The mitre limit every path is stroked with, in line widths: PostScript's
/// default, which the format fixes.
pub const MITRE_LIMIT: u32 = 10;

/// Where a path object holds its fill colour; its outline colour, the
/// outline's width and its style follow, a word each.
pub const PATH_FILL: usize = OBJECT_HEADER;
/// Where a path object holds its outline colour.
pub const PATH_OUTLINE: usize = PATH_FILL + 4;
/// Where a path object holds its outline's width, in draw units.
pub const PATH_WIDTH: usize = PATH_OUTLINE + 4;
/// Where a path object holds its style.
pub const PATH_STYLE: usize = PATH_WIDTH + 4;
/// Where a path object holds its dash pattern, where its style is
/// [`DASHED`]; its path follows the pattern, or begins here without one.
pub const PATH_DASHES: usize = PATH_STYLE + 4;

/// Where a dash pattern holds how many lengths it has, after the distance
/// into the pattern a stroke starts at.
pub const DASH_COUNT: usize = 4;
/// Where a dash pattern's lengths begin, a word each.
pub const DASH_LENGTHS: usize = DASH_COUNT + 4;

/// The bytes a dash pattern of `count` lengths takes, or `None` past a
/// `usize`.
#[must_use]
pub fn dash_pattern_bytes(count: usize) -> Option<usize> {
    count.checked_mul(4)?.checked_add(DASH_LENGTHS)
}

/// A group's name, padded with spaces, in bytes.
pub const GROUP_NAME: usize = 12;

/// Where a group's objects begin, after its name.
pub const GROUP_OBJECTS: usize = OBJECT_HEADER + GROUP_NAME;

/// Where a tagged object's object begins, after its tag; the tag's data
/// follows the object.
pub const TAGGED_OBJECT: usize = OBJECT_HEADER + 4;

/// Where a text holds its colour; its background colour, its style, its sizes
/// and its base point follow, then its characters, ended by a zero. A
/// transformed text holds each [`TRANSFORMED_TEXT_FIELDS`] further on.
pub const TEXT_COLOUR: usize = OBJECT_HEADER;
/// Where a text holds the colour it is anti-aliased against.
pub const TEXT_BACKGROUND: usize = TEXT_COLOUR + 4;
/// Where a text holds its style, its font's number in [`FONT_NUMBER`].
pub const TEXT_STYLE: usize = TEXT_BACKGROUND + 4;
/// Where a text holds its sizes, across then up, in draw units.
pub const TEXT_SIZES: usize = TEXT_STYLE + 4;
/// Where a text holds its base point, its baseline's start.
pub const TEXT_BASE: usize = TEXT_SIZES + 8;
/// Where a text's characters begin.
pub const TEXT_CHARACTERS: usize = TEXT_BASE + POINT;

/// The bits of a text style its font number is in.
pub const FONT_NUMBER: u32 = 0xFF;

/// Where a transformed text holds its flags, after its matrix.
pub const TRANSFORMED_TEXT_FLAGS: usize = OBJECT_HEADER + MATRIX;
/// What a transformed text holds ahead of a text's fields: its matrix and
/// its flags.
pub const TRANSFORMED_TEXT_FIELDS: usize = MATRIX + 4;
/// A transformed text's flag that kerns it.
pub const KERNED: u32 = 1;
/// A transformed text's flag that sets it from right to left.
pub const RIGHT_TO_LEFT: u32 = 1 << 1;

/// Where a JPEG object holds its width and height, in draw units; its dots
/// to the inch across and up follow, then its matrix.
pub const JPEG_SIZES: usize = OBJECT_HEADER;
/// Where a JPEG object holds its matrix.
pub const JPEG_MATRIX: usize = JPEG_SIZES + 16;
/// Where a JPEG object holds its data's length, after its matrix.
pub const JPEG_LENGTH: usize = JPEG_MATRIX + MATRIX;
/// Where a JPEG object's data begins: the JPEG itself.
pub const JPEG_DATA: usize = JPEG_LENGTH + 4;

/// One object of a file: its type, where it starts in the file, and all its
/// bytes, header and all.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Object<'a> {
    /// Its type.
    pub kind: u32,
    /// Its offset in the file.
    pub at: usize,
    /// Its bytes, its header first.
    pub bytes: &'a [u8],
}

/// An object whose size is less than its type and size words, is no whole
/// number of words, or runs past what holds it: where it starts in the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Misframed {
    /// Its offset in the file.
    pub at: usize,
}

/// The objects one after another in a run of a file's bytes, each held to
/// the run; a misframed one ends the walk.
#[derive(Clone, Debug)]
pub struct Objects<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Objects<'a> {
    /// The objects of `bytes`, which start `at` bytes into the file.
    #[must_use]
    pub const fn new(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at }
    }
}

impl<'a> Iterator for Objects<'a> {
    type Item = Result<Object<'a>, Misframed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }
        let framed = le_u32(self.bytes, 0)
            .zip(le_u32(self.bytes, OBJECT_SIZE))
            .and_then(|(kind, size)| {
                let size = usize::try_from(size).ok()?;
                (size >= BARE_HEADER && size % 4 == 0 && size <= self.bytes.len())
                    .then_some((kind, size))
            });
        let at = self.at;
        let Some((kind, size)) = framed else {
            self.bytes = &[];
            return Some(Err(Misframed { at }));
        };
        let (bytes, rest) = self.bytes.split_at(size);
        self.bytes = rest;
        self.at += size;
        Some(Ok(Object { kind, at, bytes }))
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn objects_are_walked_where_they_are_in_the_file() {
        let bytes = words(&[PATH, 12, 7, GROUP, 8]);
        let walked: Vec<_> = Objects::new(&bytes, 40).collect();
        assert_eq!(walked.len(), 2);
        assert_eq!(
            walked[0],
            Ok(Object {
                kind: PATH,
                at: 40,
                bytes: &bytes[..12]
            })
        );
        assert_eq!(
            walked[1],
            Ok(Object {
                kind: GROUP,
                at: 52,
                bytes: &bytes[12..]
            })
        );
    }

    #[test]
    fn a_misframed_object_ends_the_walk_naming_where_it_is() {
        for size in [0, 4, 9, 13, 400] {
            let bytes = words(&[PATH, 12, 7, TEXT, size, 0]);
            let walked: Vec<_> = Objects::new(&bytes, 40).collect();
            assert_eq!(walked.len(), 2, "{size}");
            assert_eq!(walked[1], Err(Misframed { at: 52 }), "{size}");
        }
    }

    #[test]
    fn the_fields_lie_where_the_format_puts_them() {
        assert_eq!((HEADER, OBJECT_HEADER, OBJECT_BOUNDS), (40, 24, 8));
        assert_eq!((TEXT_STYLE, TEXT_CHARACTERS), (32, 52));
        assert_eq!((TRANSFORMED_TEXT_FLAGS, TRANSFORMED_TEXT_FIELDS), (48, 28));
        assert_eq!((PATH_DASHES, JPEG_MATRIX, JPEG_DATA), (40, 40, 68));
    }

    #[test]
    fn each_element_takes_its_code_and_its_points() {
        for (tag, bytes) in [
            (END, 4),
            (MOVE, 12),
            (MOVE_UNWOUND, 12),
            (CLOSE_WITH_GAP, 4),
            (CLOSE, 4),
            (CURVE, 28),
            (GAP, 12),
            (LINE, 12),
        ] {
            assert_eq!(element_bytes(tag), Some(bytes), "{tag}");
        }
        for tag in [CONTINUATION, LINE + 1, TAG] {
            assert_eq!(element_points(tag), None, "{tag}");
            assert_eq!(element_bytes(tag), None, "{tag}");
        }
    }

    #[test]
    fn a_matrix_word_past_the_bytes_is_none() {
        let bytes = words(&[0, 1, 2, 3, 4, 5, 6]);
        let read = |at| matrix_words(&bytes, at);
        assert_eq!(read(4), [1, 2, 3, 4, 5, 6].map(Some));
        assert_eq!(read(12), [Some(3), Some(4), Some(5), Some(6), None, None]);
        assert_eq!(read(usize::MAX), [None; 6]);
        assert_eq!(IDENTITY, [0x1_0000, 0, 0, 0x1_0000, 0, 0]);
    }

    #[test]
    fn a_dash_pattern_is_its_header_and_a_word_a_length() {
        assert_eq!(dash_pattern_bytes(0), Some(8));
        assert_eq!(dash_pattern_bytes(3), Some(20));
        assert_eq!(dash_pattern_bytes(usize::MAX / 4), None);
        assert_eq!(dash_pattern_bytes(usize::MAX), None);
    }
}

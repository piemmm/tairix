//! The Draw file format's one definition (RISC OS PRM volume 4, appendix
//! E): its header, its object types and their framing, the path elements
//! the Draw module reads, and a path style's fields, with the walk every
//! reader of a file's objects shares.

/// What every Draw file begins with.
pub const MAGIC: &[u8; 4] = b"Draw";

/// The format's major version, the one RISC OS writes and reads.
pub const MAJOR_VERSION: u32 = 201;

/// The header's length, in bytes: the magic, the two versions, the program's
/// name and the drawing's bounds.
pub const HEADER: usize = 40;

/// Where the header holds its bounds.
pub const HEADER_BOUNDS: usize = 24;

/// An object's header, in bytes: its type, its size and its bounds.
pub const OBJECT_HEADER: usize = 24;

/// The header of an object with no bounds — a font table, the options — in
/// bytes: its type and its size.
pub const BARE_HEADER: usize = 8;

/// A transformed object's matrix, in bytes: four entries of sixteen bits of
/// fraction, then a translation in draw units.
pub const MATRIX: usize = 24;

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

/// A path object's fields before its path: fill and outline colours, the
/// outline's width and the style, then a dash pattern where the style says.
pub const PATH_FIELDS: usize = 16;

/// A group's name, padded with spaces, in bytes.
pub const GROUP_NAME: usize = 12;

/// The bits of a text style its font number is in.
pub const FONT_NUMBER: u32 = 0xFF;

/// The word at `at` in `bytes`.
#[must_use]
pub fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let four = bytes.get(at..end)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

/// The signed word at `at` in `bytes`.
#[must_use]
pub fn int(bytes: &[u8], at: usize) -> Option<i32> {
    word(bytes, at).map(u32::cast_signed)
}

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
        let framed = word(self.bytes, 0)
            .zip(word(self.bytes, 4))
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
    fn a_word_past_the_bytes_is_none() {
        assert_eq!(word(&[1, 2, 3], 0), None);
        assert_eq!(word(&[1, 0, 0, 0], usize::MAX), None);
        assert_eq!(int(&[0xFF; 4], 0), Some(-1));
    }
}

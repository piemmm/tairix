//! The classic hex dump's shape, as every hex view in the system draws it:
//! sixteen bytes a row in two groups of eight, an offset column as wide as
//! the largest offset it shows and never narrower than eight digits, and an
//! ASCII column that shows only what prints.

/// Bytes in one row.
pub const BYTES_PER_ROW: usize = 16;

/// The narrowest offset column: an ordinary file's row fits an 80-column
/// screen, and only a file past 4 GiB widens it.
const MIN_OFFSET_DIGITS: usize = 8;

/// Hex digits the offset column takes to show every offset up to `largest`.
#[must_use]
pub const fn offset_digits(largest: u64) -> usize {
    let significant = (u64::BITS - largest.leading_zeros()).div_ceil(4) as usize;
    if significant > MIN_OFFSET_DIGITS {
        significant
    } else {
        MIN_OFFSET_DIGITS
    }
}

/// What the ASCII column shows for `byte`: itself when it prints, a dot
/// otherwise, so a byte from a file never reaches a display as a control.
#[must_use]
pub const fn ascii_of(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        byte as char
    } else {
        '.'
    }
}

#[cfg(test)]
mod tests {
    use super::{ascii_of, offset_digits};

    #[test]
    fn the_offset_column_widens_only_past_eight_digits() {
        assert_eq!(offset_digits(0), 8);
        assert_eq!(offset_digits(0xffff_ffff), 8);
        assert_eq!(offset_digits(0x1_0000_0000), 9);
        assert_eq!(offset_digits(u64::MAX), 16);
    }

    #[test]
    fn only_what_prints_is_shown() {
        assert_eq!(ascii_of(b'A'), 'A');
        assert_eq!(ascii_of(b' '), ' ');
        assert_eq!(ascii_of(b'~'), '~');
        for byte in [0u8, b'\n', 0x1b, 0x7f, 0x80, 0xff] {
            assert_eq!(ascii_of(byte), '.', "{byte:#x}");
        }
    }
}

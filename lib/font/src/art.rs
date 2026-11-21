//! Glyph art: a bitmap character set drawn as text, a row of `#` for a dot
//! and `.` for none, read as the program compiles so a malformed row is a
//! build error.

/// `row`'s `width` dots as a byte, the leftmost the highest of its low
/// `width` bits.
///
/// # Panics
///
/// Where `width` is more than eight, or `row` is not `width` characters each
/// `#` or `.`: at compile time, for a table built as the program compiles.
#[must_use]
pub const fn dots(row: &[u8], width: usize) -> u8 {
    assert!(width <= 8, "a row of glyph art is at most eight dots");
    assert!(
        row.len() == width,
        "a row of glyph art is its glyph's width"
    );
    let mut bits = 0u8;
    let mut at = 0;
    while at < width {
        bits <<= 1;
        match row[at] {
            b'#' => bits |= 1,
            b'.' => {}
            _ => panic!("glyph art draws a dot as # and none as ."),
        }
        at += 1;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::dots;

    #[test]
    fn the_leftmost_dot_is_the_highest_of_the_row_s_bits() {
        assert_eq!(dots(b"#....", 5), 0x10);
        assert_eq!(dots(b"....#", 5), 0x01);
        assert_eq!(dots(b"#.#.#..#", 8), 0xA9);
        assert_eq!(dots(b"########", 8), 0xFF);
        assert_eq!(dots(b"", 0), 0);
    }

    #[test]
    #[should_panic(expected = "glyph art draws a dot as # and none as .")]
    fn anything_but_a_dot_or_a_gap_is_refused() {
        let _ = dots(b"#x..", 4);
    }

    #[test]
    #[should_panic(expected = "a row of glyph art is its glyph's width")]
    fn a_row_of_another_width_is_refused() {
        let _ = dots(b"###", 4);
    }

    #[test]
    #[should_panic(expected = "a row of glyph art is at most eight dots")]
    fn a_row_wider_than_a_byte_is_refused() {
        let _ = dots(b"#########", 9);
    }
}

//! The canvas's slots as the dots of a straight RGBA8 frame.

use tairix_crt::screen::DOT;

/// A slot's colour: its foreground over its background by its coverage, in
/// thirds, each of the eight colours bit 0 red, bit 1 green and bit 2 blue.
#[must_use]
pub const fn colour(slot: u8) -> [u8; DOT] {
    let (foreground, background, coverage) = (slot & 7, (slot >> 3) & 7, slot >> 6);
    [
        channel(foreground, background, coverage, 1),
        channel(foreground, background, coverage, 2),
        channel(foreground, background, coverage, 4),
        0xFF,
    ]
}

/// One channel's level: a third of full for each third of the slot the
/// colour holding `bit` covers.
const fn channel(foreground: u8, background: u8, coverage: u8, bit: u8) -> u8 {
    let thirds = (foreground & bit) / bit * coverage + (background & bit) / bit * (3 - coverage);
    thirds * 85
}

/// Every slot's colour, by its byte.
#[must_use]
pub const fn palette() -> [[u8; DOT]; 256] {
    let mut table = [[0; DOT]; 256];
    let mut slot: u8 = 0;
    loop {
        table[slot as usize] = colour(slot);
        if slot == u8::MAX {
            return table;
        }
        slot += 1;
    }
}

/// The colour of every slot of `slots` into `dots`, a dot a slot.
pub fn expand(palette: &[[u8; DOT]; 256], slots: &[u8], dots: &mut [u8]) {
    for (dot, &slot) in dots.as_chunks_mut::<DOT>().0.iter_mut().zip(slots) {
        *dot = palette[usize::from(slot)];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::canvas::encode;

    #[test]
    fn a_slot_mixes_its_colours_by_its_coverage() {
        assert_eq!(
            colour(encode(7, 0, 3)),
            [255, 255, 255, 255],
            "white, wholly covered"
        );
        assert_eq!(
            colour(encode(7, 0, 0)),
            [0, 0, 0, 255],
            "uncovered, the black behind"
        );
        assert_eq!(colour(encode(1, 4, 3)), [255, 0, 0, 255], "red over blue");
        assert_eq!(colour(encode(1, 4, 1)), [85, 0, 170, 255], "a third red");
        assert_eq!(
            colour(encode(6, 0, 2)),
            [0, 170, 170, 255],
            "two thirds cyan"
        );
    }

    #[test]
    fn a_row_of_slots_expands_dot_for_dot() {
        let table = palette();
        let slots = [encode(2, 0, 3), encode(0, 5, 0)];
        let mut dots = [0u8; 2 * DOT];
        expand(&table, &slots, &mut dots);
        assert_eq!(dots, [0, 255, 0, 255, 255, 0, 255, 255]);
    }
}

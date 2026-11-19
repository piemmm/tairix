//! The canvas's slots as the dots of a straight RGBA8 frame, as each model's
//! canvas holds them.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_crt::screen::{Screen, DOT};

use crate::machine::c64::palette::{self as c64_palette, Luma};
use crate::machine::Model;

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

/// Every slot's colour, by its byte, as `model`'s canvas holds them: an
/// Acorn's slots mix two of its eight colours by their coverage, a C64's
/// name one of the VIC-II's sixteen.
#[must_use]
pub fn palette_of(model: Model) -> Box<[[u8; DOT]; 256]> {
    match model {
        Model::ModelB | Model::Electron => Box::new(palette()),
        Model::C64 => {
            let colours = c64_palette::palette(Luma::Later);
            Box::new(core::array::from_fn(|slot| {
                let [red, green, blue] = colours[slot % colours.len()];
                [red, green, blue, 0xFF]
            }))
        }
    }
}

/// Every Acorn slot's colour, by its byte.
const fn palette() -> [[u8; DOT]; 256] {
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

/// Changed rows of slots expanded onto a screen in order, a row alike the
/// one expanded before it copied from that row: a model drawing each line
/// twice expands it once.
#[derive(Debug, Default)]
pub struct Expansion {
    held: Vec<u8>,
    last: Option<u32>,
}

impl Expansion {
    /// A fresh run of rows: what was expanded before is not looked at, as
    /// the screen may have changed since.
    pub const fn restart(&mut self) {
        self.last = None;
    }

    /// Row `row` of `screen` showing `slots` through `palette`.
    pub fn row(&mut self, palette: &[[u8; DOT]; 256], screen: &mut Screen, row: u32, slots: &[u8]) {
        if let Some(from) = self.last.filter(|_| self.held == slots) {
            if screen.copy_row(from, row) {
                self.last = Some(row);
                return;
            }
        }
        self.last = None;
        let Some(dots) = screen.row_mut(row) else {
            return;
        };
        expand(palette, slots, dots);
        self.held.clear();
        if self.held.try_reserve(slots.len()).is_ok() {
            self.held.extend_from_slice(slots);
            self.last = Some(row);
        }
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
    fn a_commodore_s_slots_are_its_sixteen_colours() {
        let table = palette_of(Model::C64);
        assert_eq!(table[0], [0, 0, 0, 0xFF], "black");
        assert_eq!(table[1], [0xFF, 0xFF, 0xFF, 0xFF], "white");
        let colours = c64_palette::palette(Luma::Later);
        assert_eq!(table[6][..3], colours[6], "blue");
        assert_eq!(*palette_of(Model::ModelB), palette(), "an Acorn's mix");
    }

    #[test]
    fn a_row_alike_the_last_is_copied_and_any_other_expanded() {
        let table = palette();
        let black = [[0, 0, 0, 0xFF]; 256];
        let (red, blue) = (encode(1, 0, 3), encode(4, 0, 3));
        let (r, b) = ([255, 0, 0, 255], [0, 0, 255, 255]);
        let row_of = |screen: &Screen, row: usize| screen.pixels()[8 * row..8 * row + 8].to_vec();
        let mut screen = Screen::new();
        assert!(screen.reshape(2, 4));
        let mut expansion = Expansion::default();
        expansion.row(&table, &mut screen, 0, &[red, blue]);
        // Copied, so a palette that would draw it black is not looked at.
        expansion.row(&black, &mut screen, 1, &[red, blue]);
        expansion.row(&table, &mut screen, 2, &[blue, blue]);
        assert_eq!(row_of(&screen, 1), [r, b].concat(), "copied");
        assert_eq!(row_of(&screen, 2), [b, b].concat(), "expanded");
        screen.row_mut(2).unwrap().fill(0);
        expansion.restart();
        expansion.row(&table, &mut screen, 3, &[blue, blue]);
        assert_eq!(row_of(&screen, 3), [b, b].concat(), "a fresh run expands");
        expansion.row(&table, &mut screen, 4, &[blue, blue]);
        screen.row_mut(3).unwrap().fill(0);
        expansion.row(&table, &mut screen, 1, &[blue, blue]);
        assert_eq!(
            row_of(&screen, 1),
            [b, b].concat(),
            "nothing held past the frame"
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

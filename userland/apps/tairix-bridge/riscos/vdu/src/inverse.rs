//! The 32K inverse table RISC OS 5 builds for a palette, which ColourTrans's
//! table from a direct-colour source, `InverseTable` and `BlendTable` all take:
//! each fifteen-bit colour's palette entry nearest by the sum of its
//! channels' differences in five-bit steps, the first of equally near, then
//! each entry's own colour taking the last entry that holds it. The default
//! 256-colour palette, whose table RISC OS 5 keeps prebuilt, takes its
//! nearest by the default loadings instead, the closest rule to that table.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::len32;
use tairix_vdu::palette::default_256;

use crate::colour::eight_of_five;
use crate::colourtrans::matching::Loadings;
use crate::palette::EIGHT_BIT_COLOURS;

/// The colours a table covers.
pub(crate) const CELLS: usize = 1 << 15;
/// The marker either side of a 32K table's address where a translation
/// table hands one over.
pub(crate) const GUARD: u32 = u32::from_le_bytes(*b"32K.");
/// The levels of a five-bit channel.
const LEVELS: usize = 32;
/// A key no entry reaches: nearness in the high byte, the entry below.
const UNREACHED: u16 = u16::MAX;
/// One five-bit step further, in a key.
const STEP: u16 = 1 << 8;
/// The tints of the default 256-colour palette, and the levels of each of a
/// colour's channels at one tint: its nibble four times its level, plus the
/// tint.
const TINTS: usize = 4;
const TINT_LEVELS: usize = 4;

/// `colour`'s fifteen-bit form: each channel's top five bits, red lowest.
pub(crate) const fn fifteen(colour: Rgb) -> usize {
    cell([colour.r >> 3, colour.g >> 3, colour.b >> 3])
}

/// The cell of five-bit red, green and blue `five`, red lowest.
pub(crate) const fn cell(five: [u8; 3]) -> usize {
    five[0] as usize | (five[1] as usize) << 5 | (five[2] as usize) << 10
}

/// The table for `colours`, at most 256 of them; none where there is no
/// room.
pub(crate) fn table(colours: &[Rgb]) -> Option<Vec<u8>> {
    if is_default_256(colours) {
        return default_table();
    }
    let mut keys = Vec::new();
    keys.try_reserve_exact(CELLS).ok()?;
    keys.resize(CELLS, UNREACHED);
    for (index, &colour) in (0..=u8::MAX).zip(colours) {
        let cell = &mut keys[fifteen(colour)];
        *cell = (*cell).min(u16::from(index));
    }
    for stride in [1, LEVELS, LEVELS * LEVELS] {
        sweep(&mut keys, stride);
    }
    let mut table = Vec::new();
    table.try_reserve_exact(CELLS).ok()?;
    table.extend(keys.iter().map(|key| key.to_le_bytes()[0]));
    for (index, &colour) in (0..=u8::MAX).zip(colours) {
        table[fifteen(colour)] = index;
    }
    Some(table)
}

/// A table kept for the colours it was built for, so that it is built again
/// only once they differ.
#[derive(Default)]
pub(crate) struct Kept {
    colours: Vec<Rgb>,
    table: Option<Rc<Vec<u8>>>,
}

impl Kept {
    /// The table for `colours`, and whether it was built afresh for them;
    /// none where there is no room.
    pub(crate) fn table(&mut self, colours: &[Rgb]) -> Option<(Rc<Vec<u8>>, bool)> {
        if let Some(table) = self.table.as_ref().filter(|_| self.colours == colours) {
            return Some((Rc::clone(table), false));
        }
        self.table = None;
        let built = Rc::new(table(colours)?);
        self.colours.clear();
        self.colours.try_reserve_exact(colours.len()).ok()?;
        self.colours.extend_from_slice(colours);
        self.table = Some(Rc::clone(&built));
        Some((built, true))
    }

    /// Its table given up, to be built again when next asked for.
    pub(crate) fn forget(&mut self) {
        self.table = None;
    }
}

/// Each line of 32 cells along `stride` given the nearest key either way
/// along it: the sum of differences splits by channel, so three sweeps give
/// the nearest over all three.
fn sweep(keys: &mut [u16], stride: usize) {
    let lines = (0..CELLS).filter(|cell| (cell / stride).is_multiple_of(LEVELS));
    for start in lines {
        let mut carried = UNREACHED;
        for step in 0..LEVELS {
            let cell = &mut keys[start + step * stride];
            carried = (*cell).min(carried.saturating_add(STEP));
            *cell = carried;
        }
        carried = UNREACHED;
        for step in (0..LEVELS).rev() {
            let cell = &mut keys[start + step * stride];
            carried = (*cell).min(carried.saturating_add(STEP));
            *cell = carried;
        }
    }
}

/// Whether `colours` are the default 256-colour palette's, exactly.
fn is_default_256(colours: &[Rgb]) -> bool {
    colours.len() == EIGHT_BIT_COLOURS
        && colours
            .iter()
            .zip(0..=u8::MAX)
            .all(|(colour, pixel)| colour.to_array() == default_256(pixel))
}

/// The default 256-colour palette's table: each cell, its channels widened
/// to eight bits, matched by the default loadings, the first of equally
/// near. A colour of the palette is a tint its channels share and a level of
/// each, and at one tint each channel's nearest level is its own alone, so
/// the match is the nearest of the four tints' nearest colours.
fn default_table() -> Option<Vec<u8>> {
    let mut colour_of = [[[[0u8; TINT_LEVELS]; TINT_LEVELS]; TINT_LEVELS]; TINTS];
    for pixel in 0..=u8::MAX {
        let [r, g, b] = default_256(pixel).map(|channel| usize::from(channel >> 4));
        colour_of[r % TINTS][r / TINTS][g / TINTS][b / TINTS] = pixel;
    }
    let nearest: [[(usize, u32); LEVELS]; TINTS] = core::array::from_fn(|tint| {
        core::array::from_fn(|five| nearest_level(eight_of_five(len32(five)), tint))
    });
    let [red, green, blue] = Loadings::DEFAULT.channels().map(u32::from);
    let mut table = Vec::new();
    table.try_reserve_exact(CELLS).ok()?;
    for cell in 0..CELLS {
        let fives = [0, 5, 10].map(|shift| (cell >> shift) % LEVELS);
        let mut best: Option<(u32, u8)> = None;
        for (tint, nearest) in nearest.iter().enumerate() {
            let [r, g, b] = fives.map(|five| nearest[five]);
            let error = red * r.1 + green * g.1 + blue * b.1;
            let pixel = colour_of[tint][r.0][g.0][b.0];
            if best.is_none_or(|nearer| (error, pixel) < nearer) {
                best = Some((error, pixel));
            }
        }
        table.push(best.map_or(0, |(_, pixel)| pixel));
    }
    Some(table)
}

/// The level of a default 256-colour channel at `tint` nearest `wanted`,
/// and the square of how far it lies.
fn nearest_level(wanted: u32, tint: usize) -> (usize, u32) {
    (0..TINT_LEVELS)
        .map(|level| {
            let shown = 0x11 * len32(TINTS * level + tint);
            (level, wanted.abs_diff(shown).pow(2))
        })
        .min_by_key(|&(_, distance)| distance)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;

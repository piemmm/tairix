//! The ECF pattern `ColourTrans_SetGCOL` makes for a colour a palettised
//! mode can only approximate. In a two-colour mode the brighter colour takes
//! as many of sixteen cells, in Bayer's order, as the colour's luma holds
//! fifteenths. In a deeper one each channel is on in as many of a 2×2 tile's
//! cells as its level among RISC OS 5's five gives, laid out as RISC OS 5
//! lays a channel alone, and each cell is the colour nearest its channels.

use tairix_colour::Rgb;

use super::matching::{Loadings, Look};
use super::palettes::Against;

// When each cell of a two-colour pattern turns on: Bayer's 4×4 order.
use tairix_raster::dither::BAYER_4 as BAYER;

/// The luma of a two-colour pattern, in 256ths, each channel's weight.
const LUMA: [u32; 3] = [77, 151, 28];
/// A cell's worth of luma, and the luma short of the first, in 256ths.
const LUMA_PER_CELL: u32 = 15 * 256;
const LUMA_SHORT: u32 = 112;

/// A tile's cells a channel is on in, by how many of four, row by row.
const TILES: [[[bool; 2]; 2]; 5] = [
    [[false, false], [false, false]],
    [[false, true], [false, false]],
    [[true, false], [false, true]],
    [[true, true], [false, true]],
    [[true, true], [true, true]],
];

/// The pattern for `wanted` in the mode `against` describes: its eight rows,
/// each a word of pixels. None for a direct-colour mode, which has none.
pub(crate) fn pattern(against: &Against, wanted: Rgb, loadings: Loadings) -> Option<[u32; 8]> {
    let (log2_bpp, colours) = against.palette()?;
    if log2_bpp == 0 {
        return Some(two_colour(colours, wanted));
    }
    let bits = 1 << log2_bpp;
    let channels = wanted.to_array();
    let mut spans = [(0u8, 0u8, 0usize); 3];
    for (channel, (span, &value)) in spans.iter_mut().zip(&channels).enumerate() {
        let (low, high) = levels_around(colours, channel, value);
        *span = (low, high, cells_on(low, high, value));
    }
    let mut tile = [[0u32; 2]; 2];
    for (y, row) in tile.iter_mut().enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            let [r, g, b] = spans.map(|(low, high, on)| if TILES[on][y][x] { high } else { low });
            *cell = against.colour_number(Rgb::new(r, g, b), loadings, Look::Nearest);
        }
    }
    Some(core::array::from_fn(|row| {
        (0..32 / bits).fold(0, |word, i| word | tile[row % 2][i % 2] << (i * bits))
    }))
}

/// The two-colour pattern: the brighter colour's cells by `wanted`'s luma.
fn two_colour(colours: &[Rgb], wanted: Rgb) -> [u32; 8] {
    let luma = |colour: Rgb| {
        LUMA.iter()
            .zip(colour.to_array())
            .map(|(&weight, value)| weight * u32::from(value))
            .sum::<u32>()
    };
    let lit = (luma(wanted).saturating_sub(LUMA_SHORT) / LUMA_PER_CELL).min(16);
    let (dark, bright) = match colours {
        [first, second, ..] if luma(*first) > luma(*second) => (1, 0),
        _ => (0, 1),
    };
    core::array::from_fn(|row| {
        (0..32).fold(0, |word, x| {
            let pixel = if u32::from(BAYER[row % 4][x % 4]) < lit {
                bright
            } else {
                dark
            };
            word | pixel << x
        })
    })
}

/// The levels of `channel` among `colours` either side of `value`: equal
/// where it is one, the nearest where it is past them all.
fn levels_around(colours: &[Rgb], channel: usize, value: u8) -> (u8, u8) {
    let mut low = None;
    let mut high = None;
    for colour in colours {
        let level = colour.to_array()[channel];
        if level <= value && low.is_none_or(|l| level > l) {
            low = Some(level);
        }
        if level >= value && high.is_none_or(|h| level < h) {
            high = Some(level);
        }
    }
    match (low, high) {
        (Some(low), Some(high)) => (low, high),
        (Some(level), None) | (None, Some(level)) => (level, level),
        (None, None) => (value, value),
    }
}

/// How many of a tile's four cells take `high` for `value` between `low`
/// and `high`: one more at each of three, five, seven and nine twelfths of
/// the span, RISC OS 5's levels. None where every level lies to one side.
fn cells_on(low: u8, high: u8, value: u8) -> usize {
    let span = u32::from(high.saturating_sub(low));
    let into = 12 * u32::from(value.saturating_sub(low));
    (0..4u32)
        .filter(|&k| span != 0 && into >= span * (2 * k + 3))
        .count()
}

#[cfg(test)]
mod tests;

//! Choosing a palette for a picture: its own colours where it has few
//! enough, else median cut over a histogram of them.

use alloc::vec::Vec;

use tairix_image::Rgba8;
use tairix_util::fallible;

use crate::canvas::{Canvas, OutOfMemory};

/// Bits of each channel the histogram keeps.
const BITS: u32 = 5;

/// Levels of each channel the histogram keeps.
const LEVELS: usize = 1 << BITS;

/// Opacity from which a pixel counts as shown rather than masked out.
pub const OPAQUE_FROM: u8 = 128;

/// A box of the histogram: an inclusive range of levels on each channel.
#[derive(Copy, Clone, Debug)]
struct Cell {
    low: [usize; 3],
    high: [usize; 3],
    count: u64,
}

fn bin(colour: Rgba8) -> usize {
    let level = |channel: u8| usize::from(channel >> (8 - BITS));
    (level(colour[0]) * LEVELS + level(colour[1])) * LEVELS + level(colour[2])
}

/// A palette of at most `colours` entries for the shown pixels of `canvas`,
/// every entry opaque: its own colours when it has no more than that, else
/// the averages of the boxes median cut divides them into. A picture with
/// nothing shown answers one black entry.
///
/// # Errors
///
/// [`OutOfMemory`] when the histogram cannot be had.
pub fn palette_for(canvas: &Canvas, colours: usize) -> Result<Vec<Rgba8>, OutOfMemory> {
    let colours = colours.clamp(1, 256);
    let mut counts = fallible::filled(LEVELS * LEVELS * LEVELS, 0u64).ok_or(OutOfMemory)?;
    let mut distinct = Distinct::new(colours)?;
    // One pass gathers both: the colours themselves, while there are few
    // enough to keep, and the histogram median cut needs once there are not.
    each_shown(canvas, |colour| {
        counts[bin(colour)] += 1;
        distinct.see(colour);
    })?;
    if let Some(exact) = distinct.palette()? {
        return Ok(exact);
    }
    let mut cells: Vec<Cell> = Vec::new();
    cells.try_reserve_exact(colours).map_err(|_| OutOfMemory)?;
    let whole = Cell {
        low: [0; 3],
        high: [LEVELS - 1; 3],
        count: counts.iter().sum(),
    };
    cells.push(shrink(whole, &counts));
    while cells.len() < colours {
        let Some(widest) = cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.count > 1 && cell.low != cell.high)
            .max_by_key(|(index, cell)| (cell.count, core::cmp::Reverse(*index)))
            .map(|(index, _)| index)
        else {
            break;
        };
        let (a, b) = split(cells[widest], &counts);
        cells[widest] = a;
        cells.push(b);
    }
    let mut palette = Vec::new();
    palette
        .try_reserve_exact(cells.len())
        .map_err(|_| OutOfMemory)?;
    palette.extend(cells.iter().map(|cell| average(cell, &counts)));
    Ok(palette)
}

/// The colours a picture shows, in the order first met, kept while there
/// are no more than a limit.
struct Distinct {
    sorted: Vec<[u8; 3]>,
    order: Vec<[u8; 3]>,
    limit: usize,
    last: Option<[u8; 3]>,
    over: bool,
}

impl Distinct {
    fn new(limit: usize) -> Result<Self, OutOfMemory> {
        let room =
            |vec: &mut Vec<[u8; 3]>| vec.try_reserve_exact(limit + 1).map_err(|_| OutOfMemory);
        let (mut sorted, mut order) = (Vec::new(), Vec::new());
        room(&mut sorted)?;
        room(&mut order)?;
        Ok(Self {
            sorted,
            order,
            limit,
            last: None,
            over: false,
        })
    }

    fn see(&mut self, colour: Rgba8) {
        let rgb = [colour[0], colour[1], colour[2]];
        if self.over || self.last == Some(rgb) {
            return;
        }
        self.last = Some(rgb);
        if let Err(at) = self.sorted.binary_search(&rgb) {
            if self.sorted.len() == self.limit {
                self.over = true;
                return;
            }
            self.sorted.insert(at, rgb);
            self.order.push(rgb);
        }
    }

    /// Every colour seen, opaque, or `None` past the limit; one black entry
    /// for a picture that showed none.
    fn palette(self) -> Result<Option<Vec<Rgba8>>, OutOfMemory> {
        if self.over {
            return Ok(None);
        }
        let mut palette = Vec::new();
        palette
            .try_reserve_exact(self.order.len().max(1))
            .map_err(|_| OutOfMemory)?;
        palette.extend(self.order.iter().map(|&[r, g, b]| [r, g, b, 255]));
        if palette.is_empty() {
            palette.push([0, 0, 0, 255]);
        }
        Ok(Some(palette))
    }
}

/// Hand every shown pixel's colour, opaque, to `seen`, row by row.
///
/// # Errors
///
/// [`OutOfMemory`] when the row cannot be had: nothing was seen, which is
/// not a picture showing nothing.
fn each_shown(canvas: &Canvas, mut seen: impl FnMut(Rgba8)) -> Result<(), OutOfMemory> {
    let mut row = alloc::vec::Vec::new();
    if !fallible::grow_to(&mut row, canvas.width() as usize, [0u8; 4]) {
        return Err(OutOfMemory);
    }
    for y in 0..canvas.height() {
        canvas.row_colours(y, 0, &mut row);
        for colour in &row {
            if colour[3] >= OPAQUE_FROM {
                seen(*colour);
            }
        }
    }
    Ok(())
}

/// `cell` shrunk to the levels its colours actually reach.
fn shrink(cell: Cell, counts: &[u64]) -> Cell {
    let mut low = cell.high;
    let mut high = cell.low;
    for r in cell.low[0]..=cell.high[0] {
        for g in cell.low[1]..=cell.high[1] {
            for b in cell.low[2]..=cell.high[2] {
                if counts[(r * LEVELS + g) * LEVELS + b] > 0 {
                    for (axis, level) in [r, g, b].into_iter().enumerate() {
                        low[axis] = low[axis].min(level);
                        high[axis] = high[axis].max(level);
                    }
                }
            }
        }
    }
    if cell.count == 0 {
        return cell;
    }
    Cell {
        low,
        high,
        count: cell.count,
    }
}

/// `cell` cut across its longest side where half its colours lie either
/// side, its colours counted along that side once.
fn split(cell: Cell, counts: &[u64]) -> (Cell, Cell) {
    let axis = (0..3)
        .max_by_key(|&axis| (cell.high[axis] - cell.low[axis], core::cmp::Reverse(axis)))
        .unwrap_or(0);
    let mut along = [0u64; LEVELS];
    for r in cell.low[0]..=cell.high[0] {
        for g in cell.low[1]..=cell.high[1] {
            for b in cell.low[2]..=cell.high[2] {
                along[[r, g, b][axis]] += counts[(r * LEVELS + g) * LEVELS + b];
            }
        }
    }
    let mut running = 0u64;
    let mut cut = cell.low[axis];
    for (level, &count) in (cell.low[axis]..).zip(&along[cell.low[axis]..cell.high[axis]]) {
        running += count;
        cut = level;
        if running * 2 >= cell.count {
            break;
        }
    }
    let below: u64 = along[cell.low[axis]..=cut].iter().sum();
    let mut first = cell;
    first.high[axis] = cut;
    first.count = below;
    let mut second = cell;
    second.low[axis] = cut + 1;
    second.count = cell.count - below;
    (shrink(first, counts), shrink(second, counts))
}

/// The mean colour of `cell`'s pixels, each level taken at its middle.
fn average(cell: &Cell, counts: &[u64]) -> Rgba8 {
    let mut sums = [0u64; 3];
    let mut total = 0u64;
    for r in cell.low[0]..=cell.high[0] {
        for g in cell.low[1]..=cell.high[1] {
            for b in cell.low[2]..=cell.high[2] {
                let count = counts[(r * LEVELS + g) * LEVELS + b];
                for (sum, level) in sums.iter_mut().zip([r, g, b]) {
                    *sum += count * middle(level);
                }
                total += count;
            }
        }
    }
    if total == 0 {
        return [0, 0, 0, 255];
    }
    let channel = |sum: u64| u8::try_from((sum + total / 2) / total).unwrap_or(u8::MAX);
    [channel(sums[0]), channel(sums[1]), channel(sums[2]), 255]
}

fn middle(level: usize) -> u64 {
    let step = 1u64 << (8 - BITS);
    level as u64 * step + step / 2
}

#[cfg(test)]
#[path = "quantize_tests.rs"]
mod tests;

//! The frost: `lib/raster`'s half-resolution blur as four passes, each its
//! arithmetic to the bit.
//!
//! [`reduce`] averages the rectangle two by two into cells, [`blur`] box-blurs
//! the cells along one axis a pass, [`spread`] lays every pixel back from the
//! two cells nearest its centre along each axis, and [`mix`] mixes the blur
//! over a surface at each pixel's coverage: `tairix_raster::halved_reference`
//! and `Surface::frost_halved_from`, cell for cell and pixel for pixel. Every
//! sample past the rectangle's edges replicates them.

use tairix_spirv::{Admitted, BuildError, Function, Id, Op};

use crate::kit::{Kit, Pass, Place};

/// Add each of `word`'s channels to its sum in `sums`, locals of the pass.
fn accumulate(
    f: &mut Function<'_>,
    kit: &Kit,
    sums: [Id; 4],
    word: Id,
    weight: Option<Id>,
) -> Result<(), BuildError> {
    let channels = kit.channels(f, word)?;
    for (sum, channel) in sums.into_iter().zip(channels) {
        let held = f.op(Op::LOAD, kit.uint, &[sum])?;
        let part = match weight {
            Some(weight) => kit.op(f, Op::I_MUL, channel, weight)?,
            None => channel,
        };
        let added = kit.op(f, Op::I_ADD, held, part)?;
        f.emit(Op::STORE, &[sum, added])?;
    }
    Ok(())
}

/// Four `Function`-class sums, each started at nought.
fn sums(f: &mut Function<'_>, kit: &Kit) -> Result<[Id; 4], BuildError> {
    let pointer = f
        .module()
        .pointer(tairix_spirv::enumerant::StorageClass::FUNCTION, kit.uint)?;
    let zero = f.module().uint(0)?;
    let mut sums = [zero; 4];
    for sum in &mut sums {
        *sum = f.local(pointer)?;
        f.emit(Op::STORE, &[*sum, zero])?;
    }
    Ok(sums)
}

/// The pixel whose channels are `sums` plus `rounding`, over `divisor`.
fn mean(
    f: &mut Function<'_>,
    kit: &Kit,
    sums: [Id; 4],
    rounding: Id,
    divisor: Id,
) -> Result<Id, BuildError> {
    let mut channels = sums;
    for (channel, sum) in channels.iter_mut().zip(sums) {
        let held = f.op(Op::LOAD, kit.uint, &[sum])?;
        let rounded = kit.op(f, Op::I_ADD, held, rounding)?;
        *channel = kit.op(f, Op::U_DIV, rounded, divisor)?;
    }
    kit.pack(f, channels)
}

/// The cells of a rectangle of a surface: each the rounded mean of its two
/// by two pixels, edges replicated.
pub mod reduce {
    use super::{accumulate, mean, sums, Admitted, BuildError, Op, Pass};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/frost_reduce.spv");

    /// The binding of the surface the rectangle lies in, read.
    pub const PIXELS: u32 = 0;
    /// The binding of the cells, `across` × `down` words a row at a time,
    /// written.
    pub const CELLS: u32 = 1;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const WIDTH: u32 = 3;
    const HEIGHT: u32 = 4;
    const ACROSS: u32 = 5;
    const DOWN: u32 = 6;
    /// The push constants' words.
    pub const WORDS: usize = 7;

    /// What a reduction is given: the surface's row in words, and the
    /// rectangle's top-left and size within it.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface.
        pub stride: u32,
        /// The rectangle's leftmost column.
        pub left: u32,
        /// The rectangle's top row.
        pub top: u32,
        /// The rectangle's width, at least one.
        pub width: u32,
        /// The rectangle's height, at least one.
        pub height: u32,
    }

    impl Push {
        /// The cells the rectangle reduces to, across and down.
        #[must_use]
        pub const fn cells(&self) -> (u32, u32) {
            (self.width.div_ceil(2), self.height.div_ceil(2))
        }

        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let (across, down) = self.cells();
            let mut words = [0; WORDS];
            words[STRIDE as usize] = self.stride;
            words[LEFT as usize] = self.left;
            words[TOP as usize] = self.top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[ACROSS as usize] = across;
            words[DOWN as usize] = down;
            words
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let (pixels, cells) = (pass.buffer(PIXELS)?, pass.buffer(CELLS)?);
        pass.finish((ACROSS, DOWN), |f, kit, at| {
            let surface = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let one = f.module().uint(1)?;
            let width = kit.field(f, WIDTH)?;
            let height = kit.field(f, HEIGHT)?;
            let last = (
                kit.op(f, Op::I_SUB, width, one)?,
                kit.op(f, Op::I_SUB, height, one)?,
            );
            let origin = (
                kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, at.x, 1)?,
                kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, at.y, 1)?,
            );
            let sums = sums(f, kit)?;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let x = kit.op_by(f, Op::I_ADD, origin.0, dx)?;
                let y = kit.op_by(f, Op::I_ADD, origin.1, dy)?;
                let sample = (kit.min(f, x, last.0)?, kit.min(f, y, last.1)?);
                let index = kit.index(f, surface, sample)?;
                let word = kit.load(f, pixels, index)?;
                accumulate(f, kit, sums, word, None)?;
            }
            let (two, four) = (f.module().uint(2)?, f.module().uint(4)?);
            let cell = mean(f, kit, sums, two, four)?;
            let across = kit.field(f, ACROSS)?;
            let row = kit.op(f, Op::I_MUL, at.y, across)?;
            let index = kit.op(f, Op::I_ADD, row, at.x)?;
            kit.store(f, cells, index, cell)
        })
    }
}

/// One axis of the box blur over the cells: each the rounded mean of the
/// `2 · reach + 1` cells about it along the axis, edges replicated.
pub mod blur {
    use super::{accumulate, mean, sums, Admitted, BuildError, Op, Pass};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/frost_blur.spv");

    /// The binding of the cells blurred, read.
    pub const SOURCE: u32 = 0;
    /// The binding of the cells written.
    pub const TARGET: u32 = 1;

    const ACROSS: u32 = 0;
    const DOWN: u32 = 1;
    const REACH: u32 = 2;
    const VERTICAL: u32 = 3;
    /// The push constants' words.
    pub const WORDS: usize = 4;

    /// What a blur pass is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Cells a row, and the cells the pass writes across.
        pub across: u32,
        /// Rows of cells.
        pub down: u32,
        /// How many cells either side each mean takes: half the frost's
        /// radius.
        pub reach: u32,
        /// Whether the pass runs down the columns rather than along the rows.
        pub vertical: bool,
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let mut words = [0; WORDS];
            words[ACROSS as usize] = self.across;
            words[DOWN as usize] = self.down;
            words[REACH as usize] = self.reach;
            words[VERTICAL as usize] = self.vertical as u32;
            words
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let (source, target) = (pass.buffer(SOURCE)?, pass.buffer(TARGET)?);
        pass.finish((ACROSS, DOWN), |f, kit, at| {
            let (across, down) = (kit.field(f, ACROSS)?, kit.field(f, DOWN)?);
            let reach = kit.field(f, REACH)?;
            let zero = f.module().uint(0)?;
            let one = f.module().uint(1)?;
            let vertical = kit.field(f, VERTICAL)?;
            let downward = f.op(Op::I_NOT_EQUAL, kit.boolean, &[vertical, zero])?;
            let along = f.op(Op::SELECT, kit.uint, &[downward, at.y, at.x])?;
            let extent = f.op(Op::SELECT, kit.uint, &[downward, down, across])?;
            let last = kit.op(f, Op::I_SUB, extent, one)?;
            let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, reach, 1)?;
            let count = kit.op(f, Op::I_ADD, doubled, one)?;
            let sums = sums(f, kit)?;
            let counter = {
                let pointer = f
                    .module()
                    .pointer(tairix_spirv::enumerant::StorageClass::FUNCTION, kit.uint)?;
                f.local(pointer)?
            };
            f.emit(Op::STORE, &[counter, zero])?;
            f.while_loop(
                |f| {
                    let step = f.op(Op::LOAD, kit.uint, &[counter])?;
                    f.op(Op::U_LESS_THAN, kit.boolean, &[step, count])
                },
                |f| {
                    let step = f.op(Op::LOAD, kit.uint, &[counter])?;
                    let ahead = kit.op(f, Op::I_ADD, along, step)?;
                    let short = f.op(Op::U_LESS_THAN, kit.boolean, &[ahead, reach])?;
                    let back = kit.op(f, Op::I_SUB, ahead, reach)?;
                    let behind = f.op(Op::SELECT, kit.uint, &[short, zero, back])?;
                    let taken = kit.min(f, behind, last)?;
                    let x = f.op(Op::SELECT, kit.uint, &[downward, at.x, taken])?;
                    let y = f.op(Op::SELECT, kit.uint, &[downward, taken, at.y])?;
                    let row = kit.op(f, Op::I_MUL, y, across)?;
                    let index = kit.op(f, Op::I_ADD, row, x)?;
                    let word = kit.load(f, source, index)?;
                    accumulate(f, kit, sums, word, None)
                },
                |f| {
                    let step = f.op(Op::LOAD, kit.uint, &[counter])?;
                    let next = kit.op(f, Op::I_ADD, step, one)?;
                    f.emit(Op::STORE, &[counter, next])
                },
            )?;
            let half = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, count, 1)?;
            let cell = mean(f, kit, sums, half, count)?;
            let row = kit.op(f, Op::I_MUL, at.y, across)?;
            let index = kit.op(f, Op::I_ADD, row, at.x)?;
            kit.store(f, target, index, cell)
        })
    }
}

/// Every pixel of the rectangle laid back from the blurred cells: the
/// bilinear sample a quarter of a cell up and left of its centre, weighted
/// in sixteenths and rounded.
pub mod spread {
    use super::{accumulate, mean, sums, Admitted, BuildError, Function, Id, Kit, Op, Pass};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/frost_spread.spv");

    /// The binding of the blurred cells, read.
    pub const CELLS: u32 = 0;
    /// The binding of the surface the rectangle is written into.
    pub const PIXELS: u32 = 1;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const WIDTH: u32 = 3;
    const HEIGHT: u32 = 4;
    const ACROSS: u32 = 5;
    const DOWN: u32 = 6;
    /// The push constants' words.
    pub const WORDS: usize = 7;

    /// What a spread is given: the rectangle as its reduction was.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface written.
        pub stride: u32,
        /// The rectangle's leftmost column in it.
        pub left: u32,
        /// The rectangle's top row in it.
        pub top: u32,
        /// The rectangle's width.
        pub width: u32,
        /// The rectangle's height.
        pub height: u32,
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let mut words = [0; WORDS];
            words[STRIDE as usize] = self.stride;
            words[LEFT as usize] = self.left;
            words[TOP as usize] = self.top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[ACROSS as usize] = self.width.div_ceil(2);
            words[DOWN as usize] = self.height.div_ceil(2);
            words
        }
    }

    /// The two cells a pixel `at` of an axis `extent` cells long samples
    /// along it, and their weights in quarters.
    ///
    /// In quarters of a cell the point sampled lies at `2 · at − 1`, so
    /// `2 · at + 3` is that plus a whole cell, never negative: its quotient
    /// by four is the second cell and its remainder the second's weight.
    fn taps(
        f: &mut Function<'_>,
        kit: &Kit,
        at: Id,
        extent: Id,
    ) -> Result<[(Id, Id); 2], BuildError> {
        let one = f.module().uint(1)?;
        let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, at, 1)?;
        let quarters = kit.op_by(f, Op::I_ADD, doubled, 3)?;
        let second = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, quarters, 2)?;
        let toward = kit.op_by(f, Op::BITWISE_AND, quarters, 3)?;
        let last = kit.op(f, Op::I_SUB, extent, one)?;
        let past_first = f.glsl(tairix_spirv::GlslStd450::U_MAX, kit.uint, &[second, one])?;
        let first = kit.op(f, Op::I_SUB, past_first, one)?;
        let four = f.module().uint(4)?;
        let away = kit.op(f, Op::I_SUB, four, toward)?;
        Ok([
            (kit.min(f, first, last)?, away),
            (kit.min(f, second, last)?, toward),
        ])
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let (cells, pixels) = (pass.buffer(CELLS)?, pass.buffer(PIXELS)?);
        pass.finish((WIDTH, HEIGHT), |f, kit, at| {
            let (across, down) = (kit.field(f, ACROSS)?, kit.field(f, DOWN)?);
            let columns = taps(f, kit, at.x, across)?;
            let rows = taps(f, kit, at.y, down)?;
            let sums = sums(f, kit)?;
            for (row, row_weight) in rows {
                let start = kit.op(f, Op::I_MUL, row, across)?;
                for (column, column_weight) in columns {
                    let weight = kit.op(f, Op::I_MUL, row_weight, column_weight)?;
                    let index = kit.op(f, Op::I_ADD, start, column)?;
                    let word = kit.load(f, cells, index)?;
                    accumulate(f, kit, sums, word, Some(weight))?;
                }
            }
            let (eight, sixteen) = (f.module().uint(8)?, f.module().uint(16)?);
            let pixel = mean(f, kit, sums, eight, sixteen)?;
            let surface = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let index = kit.index(f, surface, (at.x, at.y))?;
            kit.store(f, pixels, index, pixel)
        })
    }
}

/// The blur mixed over a surface's own pixels at each pixel's coverage:
/// `255` takes the blur, `0` keeps the pixel, rounded at the surface's
/// ordered dither.
pub mod mix {
    use super::{Admitted, BuildError, Op, Pass, Place};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/frost_mix.spv");

    /// The binding of the surface mixed over, read and written.
    pub const PIXELS: u32 = 0;
    /// The binding of the blur, a surface of its own.
    pub const BLUR: u32 = 1;
    /// The binding of the coverage, a byte a pixel, rows `coverage_stride`
    /// bytes apart, four bytes a word with the first in its low byte.
    pub const COVERAGE: u32 = 2;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const WIDTH: u32 = 3;
    const HEIGHT: u32 = 4;
    const BLUR_STRIDE: u32 = 5;
    const BLUR_LEFT: u32 = 6;
    const BLUR_TOP: u32 = 7;
    const COVERAGE_STRIDE: u32 = 8;
    const DITHERED: u32 = 9;
    /// The push constants' words.
    pub const WORDS: usize = 10;

    /// What a mix is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface mixed over.
        pub stride: u32,
        /// The rectangle's leftmost column on it: where its dither starts.
        pub left: u32,
        /// The rectangle's top row on it.
        pub top: u32,
        /// The rectangle's width.
        pub width: u32,
        /// The rectangle's height.
        pub height: u32,
        /// Words a row of the blur.
        pub blur_stride: u32,
        /// The rectangle's leftmost column in the blur.
        pub blur_left: u32,
        /// The rectangle's top row in the blur.
        pub blur_top: u32,
        /// Bytes a row of the coverage, which starts at the rectangle's
        /// top-left.
        pub coverage_stride: u32,
        /// Whether to round at the surface's ordered dither rather than to
        /// nearest.
        pub dithered: bool,
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let mut words = [0; WORDS];
            words[STRIDE as usize] = self.stride;
            words[LEFT as usize] = self.left;
            words[TOP as usize] = self.top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[BLUR_STRIDE as usize] = self.blur_stride;
            words[BLUR_LEFT as usize] = self.blur_left;
            words[BLUR_TOP as usize] = self.blur_top;
            words[COVERAGE_STRIDE as usize] = self.coverage_stride;
            words[DITHERED as usize] = self.dithered as u32;
            words
        }
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let pixels = pass.buffer(PIXELS)?;
        let (blur, coverage) = (pass.buffer(BLUR)?, pass.buffer(COVERAGE)?);
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let surface = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let at = kit.index(f, surface, (x, y))?;
            let from = kit.load(f, pixels, at)?;
            let field = (
                kit.field(f, BLUR_STRIDE)?,
                kit.field(f, BLUR_LEFT)?,
                kit.field(f, BLUR_TOP)?,
            );
            let blur_at = kit.index(f, field, (x, y))?;
            let to = kit.load(f, blur, blur_at)?;
            let stride = kit.field(f, COVERAGE_STRIDE)?;
            let row = kit.op(f, Op::I_MUL, y, stride)?;
            let byte = kit.op(f, Op::I_ADD, row, x)?;
            let word_at = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, byte, 2)?;
            let word = kit.load(f, coverage, word_at)?;
            let within = kit.op_by(f, Op::BITWISE_AND, byte, 3)?;
            let shift = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, within, 3)?;
            let shifted = kit.op(f, Op::SHIFT_RIGHT_LOGICAL, word, shift)?;
            let weight = kit.op_by(f, Op::BITWISE_AND, shifted, 0xff)?;
            let (left, top) = (surface.1, surface.2);
            let column = kit.op(f, Op::I_ADD, left, x)?;
            let line = kit.op(f, Op::I_ADD, top, y)?;
            let dithered = kit.field(f, DITHERED)?;
            let bias = kit.bias(f, dithered, (column, line))?;
            let full = f.module().uint(255)?;
            let keep = kit.op(f, Op::I_SUB, full, weight)?;
            let froms = kit.channels(f, from)?;
            let tos = kit.channels(f, to)?;
            let mut mixed = froms;
            for ((channel, from), to) in mixed.iter_mut().zip(froms).zip(tos) {
                let kept = kit.op(f, Op::I_MUL, from, keep)?;
                let taken = kit.op(f, Op::I_MUL, to, weight)?;
                let sum = kit.op(f, Op::I_ADD, kept, taken)?;
                *channel = kit.div255(f, sum, bias)?;
            }
            let blended = kit.pack(f, mixed)?;
            let none = f.module().uint(0)?;
            let all = kit.equal(f, weight, full)?;
            let nothing = kit.equal(f, weight, none)?;
            let partial = f.op(Op::SELECT, kit.uint, &[all, to, blended])?;
            let result = f.op(Op::SELECT, kit.uint, &[nothing, from, partial])?;
            kit.store(f, pixels, at, result)
        })
    }
}

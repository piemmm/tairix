//! The compositor's own passes: a source laid over a surface, a rectangle
//! copied, a rectangle filled, pixels encoded into a display's byte order,
//! and a picture resampled into a window of another —
//! `tairix_raster::blend_span`, a row copy, a row fill,
//! `ChannelOrder::encode_run` and `Surface::resample_window_into`, pixel for
//! pixel.

use tairix_spirv::{Admitted, BuildError, Op};

use crate::kit::{Pass, Place};

/// A source composited over a surface, each source pixel scaled by `factor`
/// on its way in and both rounded at the surface's ordered dither: a
/// transparent source pixel leaves the surface as it was.
pub mod over {
    use super::{Admitted, BuildError, Op, Pass, Place};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/over.spv");

    /// The binding of the surface laid over, read and written.
    pub const TARGET: u32 = 0;
    /// The binding of the source laid over it.
    pub const SOURCE: u32 = 1;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const SOURCE_STRIDE: u32 = 3;
    const SOURCE_LEFT: u32 = 4;
    const SOURCE_TOP: u32 = 5;
    const WIDTH: u32 = 6;
    const HEIGHT: u32 = 7;
    const FACTOR: u32 = 8;
    const DITHERED: u32 = 9;
    /// The push constants' words.
    pub const WORDS: usize = 10;

    /// What an over is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface.
        pub stride: u32,
        /// The rectangle's leftmost column on the surface: where its dither
        /// starts.
        pub left: u32,
        /// The rectangle's top row on the surface.
        pub top: u32,
        /// Words a row of the source.
        pub source_stride: u32,
        /// The rectangle's leftmost column in the source.
        pub source_left: u32,
        /// The rectangle's top row in the source.
        pub source_top: u32,
        /// The rectangle's width.
        pub width: u32,
        /// The rectangle's height.
        pub height: u32,
        /// The share of each source pixel laid, in 255ths.
        pub factor: u8,
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
            words[SOURCE_STRIDE as usize] = self.source_stride;
            words[SOURCE_LEFT as usize] = self.source_left;
            words[SOURCE_TOP as usize] = self.source_top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[FACTOR as usize] = self.factor as u32;
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
        let (target, source) = (pass.buffer(TARGET)?, pass.buffer(SOURCE)?);
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let from = (
                kit.field(f, SOURCE_STRIDE)?,
                kit.field(f, SOURCE_LEFT)?,
                kit.field(f, SOURCE_TOP)?,
            );
            let source_at = kit.index(f, from, (x, y))?;
            let laid = kit.load(f, source, source_at)?;
            let alpha = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, laid, 24)?;
            let none = f.module().uint(0)?;
            let clear = kit.equal(f, alpha, none)?;
            f.if_then(clear, |f| f.emit(Op::RETURN, &[]))?;
            let surface = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let at = kit.index(f, surface, (x, y))?;
            let under = kit.load(f, target, at)?;
            let column = kit.op(f, Op::I_ADD, surface.1, x)?;
            let line = kit.op(f, Op::I_ADD, surface.2, y)?;
            let dithered = kit.field(f, DITHERED)?;
            let bias = kit.bias(f, dithered, (column, line))?;
            let factor = kit.field(f, FACTOR)?;
            let scaled = kit.scale(f, laid, factor, bias)?;
            let result = kit.over(f, scaled, under, bias)?;
            kit.store(f, target, at, result)
        })
    }
}

/// A rectangle of one surface copied into another.
pub mod copy {
    use super::{Admitted, BuildError, Pass, Place};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/copy.spv");

    /// The binding of the surface written.
    pub const TARGET: u32 = 0;
    /// The binding of the surface read.
    pub const SOURCE: u32 = 1;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const SOURCE_STRIDE: u32 = 3;
    const SOURCE_LEFT: u32 = 4;
    const SOURCE_TOP: u32 = 5;
    const WIDTH: u32 = 6;
    const HEIGHT: u32 = 7;
    /// The push constants' words.
    pub const WORDS: usize = 8;

    /// What a copy is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface written.
        pub stride: u32,
        /// The rectangle's leftmost column in it.
        pub left: u32,
        /// The rectangle's top row in it.
        pub top: u32,
        /// Words a row of the surface read.
        pub source_stride: u32,
        /// The rectangle's leftmost column in it.
        pub source_left: u32,
        /// The rectangle's top row in it.
        pub source_top: u32,
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
            words[SOURCE_STRIDE as usize] = self.source_stride;
            words[SOURCE_LEFT as usize] = self.source_left;
            words[SOURCE_TOP as usize] = self.source_top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
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
        let (target, source) = (pass.buffer(TARGET)?, pass.buffer(SOURCE)?);
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let from = (
                kit.field(f, SOURCE_STRIDE)?,
                kit.field(f, SOURCE_LEFT)?,
                kit.field(f, SOURCE_TOP)?,
            );
            let read = kit.index(f, from, (x, y))?;
            let word = kit.load(f, source, read)?;
            let to = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let written = kit.index(f, to, (x, y))?;
            kit.store(f, target, written, word)
        })
    }
}

/// A rectangle of a surface filled with one pixel.
pub mod fill {
    use super::{Admitted, BuildError, Pass, Place};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/fill.spv");

    /// The binding of the surface filled.
    pub const TARGET: u32 = 0;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const WIDTH: u32 = 3;
    const HEIGHT: u32 = 4;
    const PIXEL: u32 = 5;
    /// The push constants' words.
    pub const WORDS: usize = 6;

    /// What a fill is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the surface.
        pub stride: u32,
        /// The rectangle's leftmost column.
        pub left: u32,
        /// The rectangle's top row.
        pub top: u32,
        /// The rectangle's width.
        pub width: u32,
        /// The rectangle's height.
        pub height: u32,
        /// The pixel laid, red in its low byte.
        pub pixel: u32,
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
            words[PIXEL as usize] = self.pixel;
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
        let target = pass.buffer(TARGET)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let to = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let at = kit.index(f, to, (x, y))?;
            let pixel = kit.field(f, PIXEL)?;
            kit.store(f, target, at, pixel)
        })
    }
}

/// Pixels encoded into a display's byte order: each output byte one channel
/// of its pixel, as `order` names.
pub mod encode {
    use super::{Admitted, BuildError, Op, Pass, Place};

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/encode.spv");

    /// The binding of the frame written, a word a pixel.
    pub const TARGET: u32 = 0;
    /// The binding of the pixels encoded.
    pub const SOURCE: u32 = 1;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const SOURCE_STRIDE: u32 = 3;
    const SOURCE_LEFT: u32 = 4;
    const SOURCE_TOP: u32 = 5;
    const WIDTH: u32 = 6;
    const HEIGHT: u32 = 7;
    const ORDER: u32 = 8;
    /// The push constants' words.
    pub const WORDS: usize = 9;

    /// What an encode is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the frame.
        pub stride: u32,
        /// The rectangle's leftmost column in it.
        pub left: u32,
        /// The rectangle's top row in it.
        pub top: u32,
        /// Words a row of the pixels.
        pub source_stride: u32,
        /// The rectangle's leftmost column in them.
        pub source_left: u32,
        /// The rectangle's top row in them.
        pub source_top: u32,
        /// The rectangle's width.
        pub width: u32,
        /// The rectangle's height.
        pub height: u32,
        /// For each output byte, the channel it takes (`0` red to `3`
        /// alpha), the first byte's lowest: the encoding of the pixel whose
        /// channels are `0`, `1`, `2` and `3`, read as a little-endian word.
        pub order: u32,
    }

    impl Push {
        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let mut words = [0; WORDS];
            words[STRIDE as usize] = self.stride;
            words[LEFT as usize] = self.left;
            words[TOP as usize] = self.top;
            words[SOURCE_STRIDE as usize] = self.source_stride;
            words[SOURCE_LEFT as usize] = self.source_left;
            words[SOURCE_TOP as usize] = self.source_top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[ORDER as usize] = self.order;
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
        let (target, source) = (pass.buffer(TARGET)?, pass.buffer(SOURCE)?);
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let from = (
                kit.field(f, SOURCE_STRIDE)?,
                kit.field(f, SOURCE_LEFT)?,
                kit.field(f, SOURCE_TOP)?,
            );
            let read = kit.index(f, from, (x, y))?;
            let pixel = kit.load(f, source, read)?;
            let order = kit.field(f, ORDER)?;
            let mut bytes = [pixel; 4];
            for (byte, slot) in (0..4).zip(&mut bytes) {
                let named = kit.op_by(f, Op::SHIFT_RIGHT_LOGICAL, order, byte * 8)?;
                let channel = kit.op_by(f, Op::BITWISE_AND, named, 3)?;
                let shift = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, channel, 3)?;
                let shifted = kit.op(f, Op::SHIFT_RIGHT_LOGICAL, pixel, shift)?;
                *slot = kit.op_by(f, Op::BITWISE_AND, shifted, 0xff)?;
            }
            let encoded = kit.pack(f, bytes)?;
            let to = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let written = kit.index(f, to, (x, y))?;
            kit.store(f, target, written, encoded)
        })
    }
}

/// A picture resampled into a window of a larger or smaller one, as
/// `Surface::resample_window_into` filters it: through the taps of the
/// window's `tairix_raster::ResamplePlan`, which [`taps`](resample::taps)
/// lays out for the pass.
pub mod resample {
    use alloc::vec::Vec;

    use tairix_raster::{Region, ResamplePlan, RESAMPLE_SHIFT, ROUND_NEAREST};
    use tairix_util::fallible;

    use super::{Admitted, BuildError, Op, Pass, Place};
    use crate::kit::Refused;

    /// The module as committed.
    pub const SPIRV: &[u8] = include_bytes!("../spv/resample.spv");

    /// The binding of the picture written.
    pub const TARGET: u32 = 0;
    /// The binding of the picture read.
    pub const SOURCE: u32 = 1;
    /// The binding of the window's taps, [`taps`]' words.
    pub const PLAN: u32 = 2;

    const STRIDE: u32 = 0;
    const LEFT: u32 = 1;
    const TOP: u32 = 2;
    const WIDTH: u32 = 3;
    const HEIGHT: u32 = 4;
    const SOURCE_STRIDE: u32 = 5;
    const ACROSS: u32 = 6;
    const DOWN: u32 = 7;
    const COPY: u32 = 8;
    /// The push constants' words.
    pub const WORDS: usize = 9;

    /// What a resample is given.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Push {
        /// Words a row of the picture written.
        pub stride: u32,
        /// The window's leftmost column in it.
        pub left: u32,
        /// The window's top row in it.
        pub top: u32,
        /// The window's width.
        pub width: u32,
        /// The window's height.
        pub height: u32,
        /// Words a row of the picture read.
        pub source_stride: u32,
        /// Taps each column of the window reads.
        pub across: u32,
        /// Taps each row of the window reads.
        pub down: u32,
        /// Whether the plan copies rather than filters.
        pub copy: bool,
    }

    impl Push {
        /// The push for `plan`, the plan of `window`, writing a picture
        /// `stride` words a row from one `source_stride` words a row.
        ///
        /// # Errors
        ///
        /// [`Refused::Extent`] for a plan whose taps a sample cannot be
        /// counted in a word.
        pub fn new(
            plan: &ResamplePlan,
            window: Region,
            (stride, source_stride): (u32, u32),
        ) -> Result<Self, Refused> {
            let count = |taps: usize| u32::try_from(taps).map_err(|_| Refused::Extent);
            Ok(Self {
                stride,
                left: window.x,
                top: window.y,
                width: window.width,
                height: window.height,
                source_stride,
                across: count(plan.columns().per_sample())?,
                down: count(plan.rows().per_sample())?,
                copy: plan.is_copy(),
            })
        }

        /// The push constants, in the module's order.
        #[must_use]
        pub const fn words(&self) -> [u32; WORDS] {
            let mut words = [0; WORDS];
            words[STRIDE as usize] = self.stride;
            words[LEFT as usize] = self.left;
            words[TOP as usize] = self.top;
            words[WIDTH as usize] = self.width;
            words[HEIGHT as usize] = self.height;
            words[SOURCE_STRIDE as usize] = self.source_stride;
            words[ACROSS as usize] = self.across;
            words[DOWN as usize] = self.down;
            words[COPY as usize] = self.copy as u32;
            words
        }
    }

    /// `plan`'s taps as the pass reads them: every column's, then every
    /// row's, each the source pixel it reads and its weight.
    ///
    /// # Errors
    ///
    /// [`Refused::OutOfMemory`] when the allocator refuses the words.
    pub fn taps(plan: &ResamplePlan) -> Result<Vec<u32>, Refused> {
        let (columns, rows) = (plan.columns(), plan.rows());
        let count = columns.taps().count() + rows.taps().count();
        let mut words = Vec::new();
        if !fallible::reserve(&mut words, count.saturating_mul(2)) {
            return Err(Refused::OutOfMemory);
        }
        for (source, weight) in columns.taps().chain(rows.taps()) {
            words.extend_from_slice(&[source, weight.cast_unsigned()]);
        }
        Ok(words)
    }

    /// The module, as its source writes it.
    ///
    /// # Errors
    ///
    /// The builder's refusal.
    pub fn build() -> Result<Admitted, BuildError> {
        let mut pass = Pass::new(WORDS)?;
        let (target, source) = (pass.buffer(TARGET)?, pass.buffer(SOURCE)?);
        let plan = pass.buffer(PLAN)?;
        pass.finish((WIDTH, HEIGHT), |f, kit, Place { x, y }| {
            let (across, down) = (kit.field(f, ACROSS)?, kit.field(f, DOWN)?);
            let width = kit.field(f, WIDTH)?;
            let source_stride = kit.field(f, SOURCE_STRIDE)?;
            let column_taps = kit.op(f, Op::I_MUL, x, across)?;
            let column_base = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, column_taps, 1)?;
            let all_columns = kit.op(f, Op::I_MUL, width, across)?;
            let row_taps = kit.op(f, Op::I_MUL, y, down)?;
            let row_tap = kit.op(f, Op::I_ADD, all_columns, row_taps)?;
            let row_base = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, row_tap, 1)?;
            let to = (
                kit.field(f, STRIDE)?,
                kit.field(f, LEFT)?,
                kit.field(f, TOP)?,
            );
            let written = kit.index(f, to, (x, y))?;
            let zero = f.module().uint(0)?;
            let copy = kit.field(f, COPY)?;
            let copies = kit.test(f, Op::I_NOT_EQUAL, copy, zero)?;
            f.if_then(copies, |f| {
                let column = kit.load(f, plan, column_base)?;
                let row = kit.load(f, plan, row_base)?;
                let start = kit.op(f, Op::I_MUL, row, source_stride)?;
                let read = kit.op(f, Op::I_ADD, start, column)?;
                let pixel = kit.load(f, source, read)?;
                kit.store(f, target, written, pixel)?;
                f.emit(Op::RETURN, &[])
            })?;
            let half = f.module().uint(1 << (RESAMPLE_SHIFT - 1))?;
            let sums = kit.sums(f)?;
            let along = kit.sums(f)?;
            kit.repeat(f, down, |f, step| {
                let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, step, 1)?;
                let tap = kit.op(f, Op::I_ADD, row_base, doubled)?;
                let row = kit.load(f, plan, tap)?;
                let next = kit.op_by(f, Op::I_ADD, tap, 1)?;
                let row_weight = kit.load(f, plan, next)?;
                let start = kit.op(f, Op::I_MUL, row, source_stride)?;
                for sum in along {
                    f.emit(Op::STORE, &[sum, zero])?;
                }
                kit.repeat(f, across, |f, step| {
                    let doubled = kit.op_by(f, Op::SHIFT_LEFT_LOGICAL, step, 1)?;
                    let tap = kit.op(f, Op::I_ADD, column_base, doubled)?;
                    let column = kit.load(f, plan, tap)?;
                    let next = kit.op_by(f, Op::I_ADD, tap, 1)?;
                    let weight = kit.load(f, plan, next)?;
                    let read = kit.op(f, Op::I_ADD, start, column)?;
                    let pixel = kit.load(f, source, read)?;
                    let channels = kit.channels(f, pixel)?;
                    for (sum, channel) in along.into_iter().zip(channels) {
                        let scaled = kit.op_by(f, Op::I_MUL, channel, 255)?;
                        let weighed = kit.op(f, Op::I_MUL, scaled, weight)?;
                        let held = f.op(Op::LOAD, kit.uint, &[sum])?;
                        let added = kit.op(f, Op::I_ADD, held, weighed)?;
                        f.emit(Op::STORE, &[sum, added])?;
                    }
                    Ok(())
                })?;
                for (sum, along) in sums.into_iter().zip(along) {
                    let filtered = f.op(Op::LOAD, kit.uint, &[along])?;
                    let rounded = kit.op(f, Op::I_ADD, filtered, half)?;
                    let descaled =
                        kit.op_by(f, Op::SHIFT_RIGHT_ARITHMETIC, rounded, RESAMPLE_SHIFT)?;
                    let weighed = kit.op(f, Op::I_MUL, descaled, row_weight)?;
                    let held = f.op(Op::LOAD, kit.uint, &[sum])?;
                    let added = kit.op(f, Op::I_ADD, held, weighed)?;
                    f.emit(Op::STORE, &[sum, added])?;
                }
                Ok(())
            })?;
            let mut levels = [zero; 4];
            for (level, sum) in levels.iter_mut().zip(sums) {
                let filtered = f.op(Op::LOAD, kit.uint, &[sum])?;
                let rounded = kit.op(f, Op::I_ADD, filtered, half)?;
                let descaled = kit.op_by(f, Op::SHIFT_RIGHT_ARITHMETIC, rounded, RESAMPLE_SHIFT)?;
                let negative = kit.test(f, Op::S_LESS_THAN, descaled, zero)?;
                let floored = kit.select(f, negative, zero, descaled)?;
                let nearest = kit.op_by(f, Op::I_ADD, floored, ROUND_NEAREST)?;
                *level = kit.op_by(f, Op::U_DIV, nearest, 255)?;
            }
            let full = f.module().uint(255)?;
            let alpha = kit.min(f, levels[3], full)?;
            let mut channels = [alpha; 4];
            for (channel, level) in channels.iter_mut().zip(levels).take(3) {
                *channel = kit.min(f, level, alpha)?;
            }
            let pixel = kit.pack(f, channels)?;
            kit.store(f, target, written, pixel)
        })
    }
}

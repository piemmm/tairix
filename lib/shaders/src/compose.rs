//! The compositor's own passes: a source laid over a surface, a rectangle
//! copied, a rectangle filled, and pixels encoded into a display's byte
//! order — `tairix_raster::blend_span`, a row copy, a row fill and
//! `ChannelOrder::encode_run`, pixel for pixel.

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
            let (none, full) = (f.module().uint(0)?, f.module().uint(255)?);
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
            let channels = kit.channels(f, laid)?;
            let mut scaled = channels;
            for (scaled, channel) in scaled.iter_mut().zip(channels) {
                let product = kit.op(f, Op::I_MUL, channel, factor)?;
                *scaled = kit.div255(f, product, bias)?;
            }
            let whole = kit.equal(f, factor, full)?;
            let scaled_word = kit.pack(f, scaled)?;
            let scaled_word = f.op(Op::SELECT, kit.uint, &[whole, laid, scaled_word])?;
            let source = kit.channels(f, scaled_word)?;
            let inverse = kit.op(f, Op::I_SUB, full, source[3])?;
            let beneath = kit.channels(f, under)?;
            let mut blended = source;
            for ((blend, source), beneath) in blended.iter_mut().zip(source).zip(beneath) {
                let product = kit.op(f, Op::I_MUL, beneath, inverse)?;
                let kept = kit.div255(f, product, bias)?;
                let sum = kit.op(f, Op::I_ADD, source, kept)?;
                *blend = kit.min(f, sum, full)?;
            }
            let opaque = kit.equal(f, source[3], full)?;
            let blended_word = kit.pack(f, blended)?;
            let result = f.op(Op::SELECT, kit.uint, &[opaque, scaled_word, blended_word])?;
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

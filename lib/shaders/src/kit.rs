//! What every pass of the set is written with: its storage buffers of
//! pixels, its push constants, its invocation's place, and the per-channel
//! arithmetic of `lib/raster`'s premultiplied pixels.
//!
//! A pixel is one word, red in its low byte and alpha in its high one: the
//! order `tairix_raster::Pixel` lies in memory. Every pass runs over a grid
//! of [`GROUP`]-square workgroups, one invocation a pixel or cell.

use tairix_raster::{
    DitherRow, LUMA_SHIFT, LUMA_WEIGHTS, ROUND_NEAREST, ROUND_RECT_SCALE, ROUND_RECT_SUBSAMPLES,
};
use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, StorageClass,
};
use tairix_spirv::{Admitted, Arg, BuildError, Builder, Function, GlslStd450, Id, Op, Version};
use tairix_util::fallible;

/// The side of a pass's square workgroup, in invocations.
pub const GROUP: u32 = 8;

/// The largest corner radius a pass rounds by: it keeps the squares of the
/// supersampled distances, and their sum, in a word.
pub const MAX_PASS_RADIUS: u32 = 4095;

/// The furthest from the frame's origin a caster's edge may lie: it keeps
/// every offset a pass takes from it in a signed word.
pub const MAX_PASS_COORDINATE: i32 = 1 << 29;

/// Why a pass's description was refused: it names geometry past a bound that
/// keeps the pass's arithmetic exact in 32-bit words, or its words could not
/// be had.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refused {
    /// A corner radius past [`MAX_PASS_RADIUS`].
    Radius,
    /// An extent, offset or stride past `tairix_raster::MAX_DRAWING_EXTENT`,
    /// or a run of pixels past `tairix_raster::MAX_SURFACE_PIXELS` words.
    Extent,
    /// A caster's edge further than [`MAX_PASS_COORDINATE`] from the frame's
    /// origin.
    Coordinate,
    /// The allocator refused the description's words.
    OutOfMemory,
}

/// Whether every one of `values` is at most `tairix_raster::MAX_DRAWING_EXTENT`.
pub(crate) fn within_extent(values: &[u32]) -> bool {
    values
        .iter()
        .all(|&value| value <= tairix_raster::MAX_DRAWING_EXTENT)
}

/// Whether a run of pixels `rows` of `stride` words from word `base` lies
/// within what a surface may hold, so every index into it stays in a word.
pub(crate) fn within_surface(base: u32, stride: u32, rows: u32) -> bool {
    let most = u64::try_from(tairix_raster::MAX_SURFACE_PIXELS).unwrap_or(u64::MAX);
    u64::from(base) <= most && u64::from(rows) * u64::from(stride) <= most
}

/// The word `pixel` lies in memory as: red in its low byte, alpha in its high.
pub(crate) const fn pixel_word(pixel: tairix_raster::Pixel) -> u32 {
    u32::from_le_bytes([pixel.r, pixel.g, pixel.b, pixel.a])
}

/// The workgroups a pass over `width` × `height` pixels or cells dispatches.
#[must_use]
pub const fn groups(width: u32, height: u32) -> [u32; 3] {
    [width.div_ceil(GROUP), height.div_ceil(GROUP), 1]
}

/// The oldest version every backend takes: the Vulkan 1.1 baseline, whose
/// entry points name their inputs and outputs alone, never their resources.
const VERSION: Version = Version { major: 1, minor: 3 };

/// A pass being written: its module and the ids its body reads.
pub(crate) struct Pass {
    b: Builder,
    kit: Kit,
}

/// The ids a pass's body reads its push constants, buffers, place and dither
/// through.
#[derive(Copy, Clone)]
pub(crate) struct Kit {
    pub(crate) uint: Id,
    pub(crate) boolean: Id,
    word: Id,
    field: Id,
    global: Id,
    v3uint: Id,
    push: Id,
    dither: Id,
}

/// One invocation's place in its pass: its column and row.
#[derive(Copy, Clone)]
pub(crate) struct Place {
    pub(crate) x: Id,
    pub(crate) y: Id,
}

impl Pass {
    /// A pass whose push constants are `fields` words.
    pub(crate) fn new(fields: usize) -> Result<Self, BuildError> {
        let mut b = Builder::new(VERSION, 0)?;
        b.capability(Capability::SHADER)?;
        let (uint, boolean) = (b.int(32, false)?, b.boolean()?);
        let v3uint = b.vector(uint, 3)?;
        let input = b.pointer(StorageClass::INPUT, v3uint)?;
        let global = b.variable(input, StorageClass::INPUT)?;
        b.decorate(
            global,
            Decoration::BUILT_IN,
            &[BuiltIn::GLOBAL_INVOCATION_ID.0],
        )?;
        let word = b.pointer(StorageClass::STORAGE_BUFFER, uint)?;
        let field = b.pointer(StorageClass::PUSH_CONSTANT, uint)?;
        let members = fallible::filled(fields, uint).ok_or(BuildError::OutOfMemory)?;
        let block = b.structure(&members)?;
        b.decorate(block, Decoration::BLOCK, &[])?;
        for (member, offset) in (0..).zip((0..).step_by(4)).take(fields) {
            b.member_decorate(block, member, Decoration::OFFSET, &[offset])?;
        }
        let block_ptr = b.pointer(StorageClass::PUSH_CONSTANT, block)?;
        let push = b.variable(block_ptr, StorageClass::PUSH_CONSTANT)?;
        let dither = dither_table(&mut b, uint)?;
        Ok(Self {
            b,
            kit: Kit {
                uint,
                boolean,
                word,
                field,
                global,
                v3uint,
                push,
                dither,
            },
        })
    }

    /// A storage buffer of words at set 0, `binding`.
    pub(crate) fn buffer(&mut self, binding: u32) -> Result<Id, BuildError> {
        let b = &mut self.b;
        let array = b.runtime_array(self.kit.uint)?;
        b.decorate(array, Decoration::ARRAY_STRIDE, &[4])?;
        let block = b.structure(&[array])?;
        b.decorate(block, Decoration::BLOCK, &[])?;
        b.member_decorate(block, 0, Decoration::OFFSET, &[0])?;
        let block_ptr = b.pointer(StorageClass::STORAGE_BUFFER, block)?;
        let buffer = b.variable(block_ptr, StorageClass::STORAGE_BUFFER)?;
        b.decorate(buffer, Decoration::DESCRIPTOR_SET, &[0])?;
        b.decorate(buffer, Decoration::BINDING, &[binding])?;
        Ok(buffer)
    }

    /// The pass whose `main` is `body`, given each invocation's place within
    /// the `width` × `height` push fields name; an invocation outside does
    /// nothing.
    pub(crate) fn finish<B>(
        self,
        (width, height): (u32, u32),
        body: B,
    ) -> Result<Admitted, BuildError>
    where
        B: FnOnce(&mut Function<'_>, &Kit, Place) -> Result<(), BuildError>,
    {
        let Self { mut b, kit } = self;
        let (void, main) = (b.void()?, b.id()?);
        b.define(main, void, &[], |f, _| {
            let id = f.op(Op::LOAD, kit.v3uint, &[kit.global])?;
            let place = Place {
                x: f.op_args(
                    Op::COMPOSITE_EXTRACT,
                    kit.uint,
                    &[Arg::Id(id), Arg::Word(0)],
                )?,
                y: f.op_args(
                    Op::COMPOSITE_EXTRACT,
                    kit.uint,
                    &[Arg::Id(id), Arg::Word(1)],
                )?,
            };
            let (across, down) = (kit.field(f, width)?, kit.field(f, height)?);
            let right = f.op(Op::U_GREATER_THAN_EQUAL, kit.boolean, &[place.x, across])?;
            let below = f.op(Op::U_GREATER_THAN_EQUAL, kit.boolean, &[place.y, down])?;
            let outside = f.op(Op::LOGICAL_OR, kit.boolean, &[right, below])?;
            f.if_then(outside, |f| f.emit(Op::RETURN, &[]))?;
            body(f, &kit, place)?;
            f.emit(Op::RETURN, &[])
        })?;
        b.entry_point(ExecutionModel::GL_COMPUTE, main, "main", &[kit.global])?;
        b.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[GROUP, GROUP, 1])?;
        b.finish()
    }
}

impl Kit {
    /// Push field `index`.
    pub(crate) fn field(&self, f: &mut Function<'_>, index: u32) -> Result<Id, BuildError> {
        let at = f.module().uint(index)?;
        let pointer = f.op(Op::ACCESS_CHAIN, self.field, &[self.push, at])?;
        f.op(Op::LOAD, self.uint, &[pointer])
    }

    /// The word at `index` of `buffer`.
    pub(crate) fn load(
        &self,
        f: &mut Function<'_>,
        buffer: Id,
        index: Id,
    ) -> Result<Id, BuildError> {
        let pointer = self.word_at(f, buffer, index)?;
        f.op(Op::LOAD, self.uint, &[pointer])
    }

    /// A `Function`-class word started at `initial`.
    pub(crate) fn local(&self, f: &mut Function<'_>, initial: Id) -> Result<Id, BuildError> {
        let pointer = f.module().pointer(StorageClass::FUNCTION, self.uint)?;
        let local = f.local(pointer)?;
        f.emit(Op::STORE, &[local, initial])?;
        Ok(local)
    }

    /// Four `Function`-class words, each started at nought.
    pub(crate) fn sums(&self, f: &mut Function<'_>) -> Result<[Id; 4], BuildError> {
        let zero = f.module().uint(0)?;
        let mut sums = [zero; 4];
        for sum in &mut sums {
            *sum = self.local(f, zero)?;
        }
        Ok(sums)
    }

    /// Run `body` once for each step from nought up to `count`, handing it
    /// the step.
    pub(crate) fn repeat<B>(
        &self,
        f: &mut Function<'_>,
        count: Id,
        body: B,
    ) -> Result<(), BuildError>
    where
        B: FnOnce(&mut Function<'_>, Id) -> Result<(), BuildError>,
    {
        let (zero, one) = (f.module().uint(0)?, f.module().uint(1)?);
        let counter = self.local(f, zero)?;
        f.while_loop(
            |f| {
                let step = f.op(Op::LOAD, self.uint, &[counter])?;
                f.op(Op::U_LESS_THAN, self.boolean, &[step, count])
            },
            |f| {
                let step = f.op(Op::LOAD, self.uint, &[counter])?;
                body(f, step)
            },
            |f| {
                let step = f.op(Op::LOAD, self.uint, &[counter])?;
                let next = self.op(f, Op::I_ADD, step, one)?;
                f.emit(Op::STORE, &[counter, next])
            },
        )
    }

    /// The word at the constant `index` of `buffer`.
    pub(crate) fn word(
        &self,
        f: &mut Function<'_>,
        buffer: Id,
        index: u32,
    ) -> Result<Id, BuildError> {
        let at = f.module().uint(index)?;
        self.load(f, buffer, at)
    }

    /// Store `value` at `index` of `buffer`.
    pub(crate) fn store(
        &self,
        f: &mut Function<'_>,
        buffer: Id,
        index: Id,
        value: Id,
    ) -> Result<(), BuildError> {
        let pointer = self.word_at(f, buffer, index)?;
        f.emit(Op::STORE, &[pointer, value])
    }

    fn word_at(&self, f: &mut Function<'_>, buffer: Id, index: Id) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        f.op(Op::ACCESS_CHAIN, self.word, &[buffer, zero, index])
    }

    /// `a op b`, both words.
    pub(crate) fn op(&self, f: &mut Function<'_>, op: Op, a: Id, b: Id) -> Result<Id, BuildError> {
        f.op(op, self.uint, &[a, b])
    }

    /// `a op constant`.
    pub(crate) fn op_by(
        &self,
        f: &mut Function<'_>,
        op: Op,
        a: Id,
        constant: u32,
    ) -> Result<Id, BuildError> {
        let constant = f.module().uint(constant)?;
        self.op(f, op, a, constant)
    }

    /// The lesser of `a` and `b`.
    pub(crate) fn min(&self, f: &mut Function<'_>, a: Id, b: Id) -> Result<Id, BuildError> {
        f.glsl(GlslStd450::U_MIN, self.uint, &[a, b])
    }

    /// The word index of the pixel at (`x`, `y`) of a surface `stride` words
    /// a row, the pass's (0, 0) at its (`left`, `top`).
    pub(crate) fn index(
        &self,
        f: &mut Function<'_>,
        (stride, left, top): (Id, Id, Id),
        (x, y): (Id, Id),
    ) -> Result<Id, BuildError> {
        let row = self.op(f, Op::I_ADD, top, y)?;
        let column = self.op(f, Op::I_ADD, left, x)?;
        let start = self.op(f, Op::I_MUL, row, stride)?;
        self.op(f, Op::I_ADD, start, column)
    }

    /// The four channels of the pixel `word`: red, green, blue and alpha.
    pub(crate) fn channels(&self, f: &mut Function<'_>, word: Id) -> Result<[Id; 4], BuildError> {
        let mut channels = [word; 4];
        for (channel, slot) in (0..4).zip(&mut channels) {
            let shifted = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, word, channel * 8)?;
            *slot = self.op_by(f, Op::BITWISE_AND, shifted, 0xff)?;
        }
        Ok(channels)
    }

    /// The pixel whose channels are `channels`, each below 256.
    pub(crate) fn pack(&self, f: &mut Function<'_>, channels: [Id; 4]) -> Result<Id, BuildError> {
        let [red, green, blue, alpha] = channels;
        let green = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, green, 8)?;
        let blue = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, blue, 16)?;
        let alpha = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, alpha, 24)?;
        let low = self.op(f, Op::BITWISE_OR, red, green)?;
        let high = self.op(f, Op::BITWISE_OR, blue, alpha)?;
        self.op(f, Op::BITWISE_OR, low, high)
    }

    /// `tairix_raster::div255_biased`: `value + bias` over 255, rounded down
    /// and held below 256.
    pub(crate) fn div255(
        &self,
        f: &mut Function<'_>,
        value: Id,
        bias: Id,
    ) -> Result<Id, BuildError> {
        let biased = self.op(f, Op::I_ADD, value, bias)?;
        let quotient = self.op_by(f, Op::U_DIV, biased, 255)?;
        let most = f.module().uint(255)?;
        self.min(f, quotient, most)
    }

    /// The rounding bias surface pixel (`x`, `y`) takes: its place in
    /// `tairix_raster::DitherRow`'s pattern where `dithered` is nonzero, and
    /// [`ROUND_NEAREST`] where it is nought.
    pub(crate) fn bias(
        &self,
        f: &mut Function<'_>,
        dithered: Id,
        (x, y): (Id, Id),
    ) -> Result<Id, BuildError> {
        let row = self.op_by(f, Op::BITWISE_AND, y, 7)?;
        let column = self.op_by(f, Op::BITWISE_AND, x, 7)?;
        let start = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, row, 3)?;
        let at = self.op(f, Op::I_ADD, start, column)?;
        let entry = f.module().pointer(StorageClass::PRIVATE, self.uint)?;
        let pointer = f.op(Op::ACCESS_CHAIN, entry, &[self.dither, at])?;
        let patterned = f.op(Op::LOAD, self.uint, &[pointer])?;
        let zero = f.module().uint(0)?;
        let on = f.op(Op::I_NOT_EQUAL, self.boolean, &[dithered, zero])?;
        let nearest = f.module().uint(ROUND_NEAREST)?;
        f.op(Op::SELECT, self.uint, &[on, patterned, nearest])
    }

    /// Whether `a` and `b` are the same word.
    pub(crate) fn equal(&self, f: &mut Function<'_>, a: Id, b: Id) -> Result<Id, BuildError> {
        self.test(f, Op::I_EQUAL, a, b)
    }

    /// Whether `a op b` holds, for an integer comparison `op`; a signed one
    /// reads both words as two's complement.
    pub(crate) fn test(
        &self,
        f: &mut Function<'_>,
        op: Op,
        a: Id,
        b: Id,
    ) -> Result<Id, BuildError> {
        f.op(op, self.boolean, &[a, b])
    }

    /// `a` where `condition` holds, else `b`.
    pub(crate) fn select(
        &self,
        f: &mut Function<'_>,
        condition: Id,
        a: Id,
        b: Id,
    ) -> Result<Id, BuildError> {
        f.op(Op::SELECT, self.uint, &[condition, a, b])
    }

    /// `a − b`, or nought where `b` is the greater.
    pub(crate) fn saturating_sub(
        &self,
        f: &mut Function<'_>,
        a: Id,
        b: Id,
    ) -> Result<Id, BuildError> {
        let above = self.test(f, Op::U_GREATER_THAN, a, b)?;
        let difference = self.op(f, Op::I_SUB, a, b)?;
        let zero = f.module().uint(0)?;
        self.select(f, above, difference, zero)
    }

    /// `tairix_raster::Pixel::scale_alpha_biased`: every channel of the pixel
    /// `word` scaled by `factor`/255, rounding at `bias`, and `word` itself at
    /// a whole factor.
    pub(crate) fn scale(
        &self,
        f: &mut Function<'_>,
        word: Id,
        factor: Id,
        bias: Id,
    ) -> Result<Id, BuildError> {
        let channels = self.channels(f, word)?;
        let mut scaled = channels;
        for (scaled, channel) in scaled.iter_mut().zip(channels) {
            let product = self.op(f, Op::I_MUL, channel, factor)?;
            *scaled = self.div255(f, product, bias)?;
        }
        let packed = self.pack(f, scaled)?;
        let full = f.module().uint(255)?;
        let whole = self.equal(f, factor, full)?;
        self.select(f, whole, word, packed)
    }

    /// `tairix_raster::Pixel::over_biased`: the pixel `source` over the pixel
    /// `under`, the share of `under` it leaves rounding at `bias`, and
    /// `source` itself where it is opaque.
    pub(crate) fn over(
        &self,
        f: &mut Function<'_>,
        source: Id,
        under: Id,
        bias: Id,
    ) -> Result<Id, BuildError> {
        let laid = self.channels(f, source)?;
        let full = f.module().uint(255)?;
        let inverse = self.op(f, Op::I_SUB, full, laid[3])?;
        let beneath = self.channels(f, under)?;
        let mut blended = laid;
        for ((blend, laid), beneath) in blended.iter_mut().zip(laid).zip(beneath) {
            let product = self.op(f, Op::I_MUL, beneath, inverse)?;
            let kept = self.div255(f, product, bias)?;
            let sum = self.op(f, Op::I_ADD, laid, kept)?;
            *blend = self.min(f, sum, full)?;
        }
        let packed = self.pack(f, blended)?;
        let opaque = self.equal(f, laid[3], full)?;
        self.select(f, opaque, source, packed)
    }

    /// `tairix_raster`'s mixer: `weight`/255 of the way from the pixel `from`
    /// to the pixel `to`, each channel rounding at `bias`, and either end
    /// exactly at a weight of nought or 255.
    pub(crate) fn mix(
        &self,
        f: &mut Function<'_>,
        (from, to): (Id, Id),
        weight: Id,
        bias: Id,
    ) -> Result<Id, BuildError> {
        let full = f.module().uint(255)?;
        let keep = self.op(f, Op::I_SUB, full, weight)?;
        let (starts, ends) = (self.channels(f, from)?, self.channels(f, to)?);
        let mut mixed = starts;
        for ((slot, start), end) in mixed.iter_mut().zip(starts).zip(ends) {
            let kept = self.op(f, Op::I_MUL, start, keep)?;
            let taken = self.op(f, Op::I_MUL, end, weight)?;
            let sum = self.op(f, Op::I_ADD, kept, taken)?;
            *slot = self.div255(f, sum, bias)?;
        }
        let packed = self.pack(f, mixed)?;
        let none = f.module().uint(0)?;
        let whole = self.equal(f, weight, full)?;
        let unmoved = self.equal(f, weight, none)?;
        let packed = self.select(f, whole, to, packed)?;
        self.select(f, unmoved, from, packed)
    }

    /// `tairix_raster`'s wash: the straight colour `straight` (its alpha byte
    /// unused) laid over the pixel `under` at alpha `weight`, both terms
    /// rounded together once at `bias`.
    pub(crate) fn wash(
        &self,
        f: &mut Function<'_>,
        (straight, weight): (Id, Id),
        under: Id,
        bias: Id,
    ) -> Result<Id, BuildError> {
        let full = f.module().uint(255)?;
        let inverse = self.op(f, Op::I_SUB, full, weight)?;
        let mut colour = self.channels(f, straight)?;
        colour[3] = full;
        let beneath = self.channels(f, under)?;
        let mut washed = colour;
        for ((slot, ink), beneath) in washed.iter_mut().zip(colour).zip(beneath) {
            let laid = self.op(f, Op::I_MUL, ink, weight)?;
            let kept = self.op(f, Op::I_MUL, beneath, inverse)?;
            let sum = self.op(f, Op::I_ADD, laid, kept)?;
            *slot = self.div255(f, sum, bias)?;
        }
        self.pack(f, washed)
    }

    /// `tairix_raster`'s `scale8`: `a · b / 255` rounded to nearest, as the
    /// vector kernels' sixteen-bit form works it.
    pub(crate) fn scale8(&self, f: &mut Function<'_>, a: Id, b: Id) -> Result<Id, BuildError> {
        let product = self.op(f, Op::I_MUL, a, b)?;
        let biased = self.op_by(f, Op::I_ADD, product, ROUND_NEAREST)?;
        let high = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, biased, 8)?;
        let sum = self.op(f, Op::I_ADD, biased, high)?;
        let sum = self.op_by(f, Op::I_ADD, sum, 1)?;
        self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, sum, 8)
    }

    /// The straight colour `step` of `last` of the way from `from` to `to`,
    /// each channel interpolated and floored as `tairix_raster`'s vertical
    /// gradient does, and `from` itself where `last` is nought.
    pub(crate) fn ramp(
        &self,
        f: &mut Function<'_>,
        (from, to): (Id, Id),
        (step, last): (Id, Id),
    ) -> Result<Id, BuildError> {
        let none = f.module().uint(0)?;
        let one = f.module().uint(1)?;
        let flat = self.equal(f, last, none)?;
        let divisor = self.select(f, flat, one, last)?;
        let left = self.op(f, Op::I_SUB, last, step)?;
        let (starts, ends) = (self.channels(f, from)?, self.channels(f, to)?);
        let mut ramped = starts;
        for ((slot, start), end) in ramped.iter_mut().zip(starts).zip(ends) {
            let early = self.op(f, Op::I_MUL, start, left)?;
            let late = self.op(f, Op::I_MUL, end, step)?;
            let sum = self.op(f, Op::I_ADD, early, late)?;
            *slot = self.op(f, Op::U_DIV, sum, divisor)?;
        }
        let packed = self.pack(f, ramped)?;
        self.select(f, flat, from, packed)
    }

    /// `tairix_raster::Pixel::desaturate`: every colour channel of `word`
    /// pulled toward its BT.601 luma, keeping `saturation`/255 of the colour,
    /// its alpha untouched, and `word` itself at a whole saturation.
    pub(crate) fn desaturate(
        &self,
        f: &mut Function<'_>,
        word: Id,
        saturation: Id,
    ) -> Result<Id, BuildError> {
        let channels = self.channels(f, word)?;
        let mut luma = f.module().uint(1 << (LUMA_SHIFT - 1))?;
        for (channel, weight) in channels.into_iter().zip(LUMA_WEIGHTS) {
            let weighed = self.op_by(f, Op::I_MUL, channel, weight)?;
            luma = self.op(f, Op::I_ADD, luma, weighed)?;
        }
        let luma = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, luma, LUMA_SHIFT)?;
        let full = f.module().uint(255)?;
        let grey = self.op(f, Op::I_SUB, full, saturation)?;
        let toward_grey = self.op(f, Op::I_MUL, luma, grey)?;
        let nearest = f.module().uint(ROUND_NEAREST)?;
        let mut toned = channels;
        for (slot, channel) in toned.iter_mut().zip(channels).take(3) {
            let kept = self.op(f, Op::I_MUL, channel, saturation)?;
            let sum = self.op(f, Op::I_ADD, toward_grey, kept)?;
            *slot = self.div255(f, sum, nearest)?;
        }
        let packed = self.pack(f, toned)?;
        let whole = self.equal(f, saturation, full)?;
        self.select(f, whole, word, packed)
    }

    /// The integer square root of `value · 2^(2 · halves)`, for a `value`
    /// below 2³⁰ and at most eight `halves`: the root taken two bits at a
    /// time from the top, so no remainder or trial passes a word.
    pub(crate) fn isqrt_scaled(
        &self,
        f: &mut Function<'_>,
        value: Id,
        halves: u32,
    ) -> Result<Id, BuildError> {
        let none = f.module().uint(0)?;
        let (mut remainder, mut root) = (none, none);
        for pair in (0..15 + halves).rev() {
            let digits = match pair.checked_sub(halves) {
                Some(at) => {
                    let shifted = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, value, 2 * at)?;
                    self.op_by(f, Op::BITWISE_AND, shifted, 3)?
                }
                None => none,
            };
            let carried = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, remainder, 2)?;
            remainder = self.op(f, Op::BITWISE_OR, carried, digits)?;
            let doubled = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, root, 2)?;
            let trial = self.op_by(f, Op::BITWISE_OR, doubled, 1)?;
            let fits = self.test(f, Op::U_GREATER_THAN_EQUAL, remainder, trial)?;
            let taken = self.op(f, Op::I_SUB, remainder, trial)?;
            remainder = self.select(f, fits, taken, remainder)?;
            let shifted = self.op_by(f, Op::SHIFT_LEFT_LOGICAL, root, 1)?;
            let raised = self.op_by(f, Op::BITWISE_OR, shifted, 1)?;
            root = self.select(f, fits, raised, shifted)?;
        }
        Ok(root)
    }

    /// `tairix_raster::round_rect_coverage` of pixel (`x`, `y`) of a
    /// `width` × `height` rectangle rounded by `radius`.
    ///
    /// Exact for a radius of at most [`MAX_PASS_RADIUS`] and coordinates and
    /// extents below 2²⁹: a sub-sample's distance on either axis is held one
    /// past the scaled radius, which leaves it outside as before and keeps
    /// both squares and their sum in a word.
    pub(crate) fn coverage(
        &self,
        f: &mut Function<'_>,
        (x, y): (Id, Id),
        (width, height, radius): (Id, Id, Id),
    ) -> Result<Id, BuildError> {
        let half_width = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, width, 1)?;
        let half_height = self.op_by(f, Op::SHIFT_RIGHT_LOGICAL, height, 1)?;
        let narrower = self.min(f, radius, half_width)?;
        let rounded = self.min(f, narrower, half_height)?;
        let right_band = self.op(f, Op::I_SUB, width, rounded)?;
        let bottom_band = self.op(f, Op::I_SUB, height, rounded)?;
        let left = self.test(f, Op::U_LESS_THAN, x, rounded)?;
        let right = self.test(f, Op::U_GREATER_THAN_EQUAL, x, right_band)?;
        let top = self.test(f, Op::U_LESS_THAN, y, rounded)?;
        let bottom = self.test(f, Op::U_GREATER_THAN_EQUAL, y, bottom_band)?;
        let column = f.op(Op::LOGICAL_OR, self.boolean, &[left, right])?;
        let row = f.op(Op::LOGICAL_OR, self.boolean, &[top, bottom])?;
        let corner = f.op(Op::LOGICAL_AND, self.boolean, &[column, row])?;
        let none = f.module().uint(0)?;
        let curved = self.test(f, Op::I_NOT_EQUAL, rounded, none)?;
        let corner = f.op(Op::LOGICAL_AND, self.boolean, &[corner, curved])?;
        let full = f.module().uint(255)?;
        let level = self.local(f, full)?;
        // Only a corner's pixels can be less than covered, and they are few,
        // so the sub-samples are counted for them alone.
        f.if_then(corner, |f| {
            let scaled = self.op_by(f, Op::I_MUL, rounded, ROUND_RECT_SCALE)?;
            let reach = self.op(f, Op::I_MUL, scaled, scaled)?;
            let beyond = self.op_by(f, Op::I_ADD, scaled, 1)?;
            let wide = self.op_by(f, Op::I_MUL, width, ROUND_RECT_SCALE)?;
            let tall = self.op_by(f, Op::I_MUL, height, ROUND_RECT_SCALE)?;
            let inset_right = self.op(f, Op::I_SUB, wide, scaled)?;
            let inset_bottom = self.op(f, Op::I_SUB, tall, scaled)?;
            let across = self.op_by(f, Op::I_MUL, x, ROUND_RECT_SCALE)?;
            let down = self.op_by(f, Op::I_MUL, y, ROUND_RECT_SCALE)?;
            let mut distances = [[across; 2]; ROUND_RECT_SUBSAMPLES as usize];
            for (sample, slot) in (0..ROUND_RECT_SUBSAMPLES).zip(&mut distances) {
                for (start, (low, high), out) in [
                    (across, (scaled, inset_right), 0),
                    (down, (scaled, inset_bottom), 1),
                ] {
                    let at = self.op_by(f, Op::I_ADD, start, 2 * sample + 1)?;
                    let before = self.saturating_sub(f, low, at)?;
                    let after = self.saturating_sub(f, at, high)?;
                    let distance = self.op(f, Op::I_ADD, before, after)?;
                    let held = self.min(f, distance, beyond)?;
                    slot[out] = self.op(f, Op::I_MUL, held, held)?;
                }
            }
            let one = f.module().uint(1)?;
            let mut inside = none;
            for [_, dy] in distances {
                for [dx, _] in distances {
                    let square = self.op(f, Op::I_ADD, dx, dy)?;
                    let within = self.test(f, Op::U_LESS_THAN_EQUAL, square, reach)?;
                    let counted = self.select(f, within, one, none)?;
                    inside = self.op(f, Op::I_ADD, inside, counted)?;
                }
            }
            let total = ROUND_RECT_SUBSAMPLES * ROUND_RECT_SUBSAMPLES;
            let weighed = self.op_by(f, Op::I_MUL, inside, 255)?;
            let rounding = self.op_by(f, Op::I_ADD, weighed, total / 2)?;
            let counted = self.op_by(f, Op::U_DIV, rounding, total)?;
            let counted = self.min(f, counted, full)?;
            f.emit(Op::STORE, &[level, counted])
        })?;
        f.op(Op::LOAD, self.uint, &[level])
    }
}

/// `tairix_raster::DitherRow`'s biases for the eight rows and eight columns
/// its pattern repeats over, row by row, as a table the pass reads.
fn dither_table(b: &mut Builder, uint: Id) -> Result<Id, BuildError> {
    let mut entries = [uint; 64];
    for (slot, at) in entries.iter_mut().zip(0u32..) {
        *slot = b.uint(DitherRow::at(at / 8).bias(at % 8))?;
    }
    let length = b.uint(64)?;
    let table = b.array(uint, length)?;
    let pointer = b.pointer(StorageClass::PRIVATE, table)?;
    let values = b.constant_composite(table, &entries)?;
    b.declare(
        Op::VARIABLE,
        Some(pointer),
        &[Arg::Word(StorageClass::PRIVATE.0), Arg::Id(values)],
    )
}

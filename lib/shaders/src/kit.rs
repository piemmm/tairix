//! What every pass of the set is written with: its storage buffers of
//! pixels, its push constants, its invocation's place, and the per-channel
//! arithmetic of `lib/raster`'s premultiplied pixels.
//!
//! A pixel is one word, red in its low byte and alpha in its high one: the
//! order `tairix_raster::Pixel` lies in memory. Every pass runs over a grid
//! of [`GROUP`]-square workgroups, one invocation a pixel or cell.

use tairix_raster::{DitherRow, ROUND_NEAREST};
use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, StorageClass,
};
use tairix_spirv::{Admitted, Arg, BuildError, Builder, Function, GlslStd450, Id, Op, Version};
use tairix_util::fallible;

/// The side of a pass's square workgroup, in invocations.
pub const GROUP: u32 = 8;

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
        f.op(Op::I_EQUAL, self.boolean, &[a, b])
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

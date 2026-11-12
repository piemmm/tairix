//! The intermediate representation: one function of SSA values in blocks
//! with parameters. Every operation is total, so no input value traps:
//! division by zero gives zero and its remainder the dividend, a signed
//! division that overflows wraps, and a shift or rotate counts modulo the
//! value's width.

use alloc::vec::Vec;

/// The arguments a call or a function takes at most: what every target's
/// argument registers hold.
pub const MAX_ARGS: usize = 6;

/// A value's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// A 32-bit integer.
    I32,
    /// A 64-bit integer, which a pointer is.
    I64,
}

impl Type {
    /// Its width in bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::I32 => 32,
            Self::I64 => 64,
        }
    }

    /// `raw` as a value of this type: a 32-bit value's upper bits clear.
    #[must_use]
    pub const fn wrap(self, raw: u64) -> u64 {
        match self {
            Self::I32 => raw & 0xFFFF_FFFF,
            Self::I64 => raw,
        }
    }
}

/// How many bytes a load or store moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Width {
    /// One.
    W8,
    /// Two.
    W16,
    /// Four.
    W32,
    /// Eight.
    W64,
}

impl Width {
    /// Its bytes.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::W8 => 1,
            Self::W16 => 2,
            Self::W32 => 4,
            Self::W64 => 8,
        }
    }

    /// Its bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bytes() * 8
    }
}

/// A value: the result of one instruction, or a block's parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Value(pub(crate) u32);

impl Value {
    /// Its index among the function's values.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A block of the function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Block(pub(crate) u32);

impl Block {
    /// The entry block, whose parameters are the function's arguments.
    pub const ENTRY: Self = Self(0);

    /// Its index among the function's blocks; the entry block is the first.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An operation of one operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// Two's-complement negation.
    Neg,
    /// Every bit inverted.
    Not,
    /// The low 8 bits, zero-extended.
    Uext8,
    /// The low 16 bits, zero-extended.
    Uext16,
    /// The low 8 bits, sign-extended.
    Sext8,
    /// The low 16 bits, sign-extended.
    Sext16,
}

/// An operation of two operands of one type, giving that type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// Wrapping addition.
    Add,
    /// Wrapping subtraction.
    Sub,
    /// Wrapping multiplication: the product's low half.
    Mul,
    /// Bitwise and.
    And,
    /// Bitwise or.
    Or,
    /// Bitwise exclusive or.
    Xor,
    /// Shift left by the second operand modulo the width.
    Shl,
    /// Logical shift right by the second operand modulo the width.
    Shr,
    /// Arithmetic shift right by the second operand modulo the width.
    Sar,
    /// Rotate left by the second operand modulo the width.
    Rotl,
    /// Rotate right by the second operand modulo the width.
    Rotr,
    /// Unsigned division; by zero, zero.
    Udiv,
    /// Signed division, toward zero; by zero, zero; the one overflow wraps.
    Sdiv,
    /// Unsigned remainder; by zero, the dividend.
    Urem,
    /// Signed remainder, of the dividend's sign; by zero, the dividend.
    Srem,
}

impl BinOp {
    /// Whether its operands may be swapped.
    #[must_use]
    pub const fn commutes(self) -> bool {
        matches!(
            self,
            Self::Add | Self::Mul | Self::And | Self::Or | Self::Xor
        )
    }
}

/// A comparison, giving `1` where it holds and `0` where not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cond {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Signed less than.
    Slt,
    /// Signed less than or equal.
    Sle,
    /// Signed greater than.
    Sgt,
    /// Signed greater than or equal.
    Sge,
    /// Unsigned less than.
    Ult,
    /// Unsigned less than or equal.
    Ule,
    /// Unsigned greater than.
    Ugt,
    /// Unsigned greater than or equal.
    Uge,
}

impl Cond {
    /// The comparison that holds exactly where this one does not.
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::Slt => Self::Sge,
            Self::Sle => Self::Sgt,
            Self::Sgt => Self::Sle,
            Self::Sge => Self::Slt,
            Self::Ult => Self::Uge,
            Self::Ule => Self::Ugt,
            Self::Ugt => Self::Ule,
            Self::Uge => Self::Ult,
        }
    }

    /// The comparison of the operands swapped.
    #[must_use]
    pub const fn swapped(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Ne => Self::Ne,
            Self::Slt => Self::Sgt,
            Self::Sle => Self::Sge,
            Self::Sgt => Self::Slt,
            Self::Sge => Self::Sle,
            Self::Ult => Self::Ugt,
            Self::Ule => Self::Uge,
            Self::Ugt => Self::Ult,
            Self::Uge => Self::Ule,
        }
    }
}

/// One instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inst {
    /// A constant of `ty`: `imm` wrapped to the type.
    Const {
        /// Its type.
        ty: Type,
        /// Its bits.
        imm: u64,
    },
    /// `op a`.
    Unary {
        /// The operation.
        op: UnOp,
        /// The operand.
        a: Value,
    },
    /// `a op b`.
    Binary {
        /// The operation.
        op: BinOp,
        /// The first operand.
        a: Value,
        /// The second.
        b: Value,
    },
    /// `1` where `a cond b` holds, else `0`, as an [`Type::I32`].
    Icmp {
        /// The comparison.
        cond: Cond,
        /// The first operand.
        a: Value,
        /// The second.
        b: Value,
    },
    /// `a` where `cond` is not zero, else `b`.
    Select {
        /// An [`Type::I32`] choosing.
        cond: Value,
        /// The value where it is not zero.
        a: Value,
        /// The value where it is.
        b: Value,
    },
    /// An [`Type::I32`] widened to [`Type::I64`].
    Extend {
        /// Whether its sign is kept, else its upper half is zero.
        signed: bool,
        /// The value.
        a: Value,
    },
    /// An [`Type::I64`]'s low half.
    Reduce {
        /// The value.
        a: Value,
    },
    /// `width` bytes at `base + offset`, extended to `ty`.
    Load {
        /// The bytes moved.
        width: Width,
        /// Whether the bytes are sign-extended, else zero-extended.
        signed: bool,
        /// The result's type, which `width` does not exceed.
        ty: Type,
        /// An [`Type::I64`] address.
        base: Value,
        /// Added to it.
        offset: i32,
    },
    /// `value`'s low `width` bytes stored at `base + offset`.
    Store {
        /// The bytes moved.
        width: Width,
        /// The value.
        value: Value,
        /// An [`Type::I64`] address.
        base: Value,
        /// Added to it.
        offset: i32,
    },
    /// The helper at `index` of the table at `table` called with `args`:
    /// what it answers, of type `ret`, if it answers.
    Call {
        /// An [`Type::I64`] address of the table of helper addresses.
        table: Value,
        /// The helper's place in it.
        index: u32,
        /// Its arguments, at most [`MAX_ARGS`].
        args: Vec<Value>,
        /// What it returns.
        ret: Option<Type>,
    },
}

impl Inst {
    /// The values it reads, in order.
    pub fn uses(&self) -> impl Iterator<Item = Value> + '_ {
        let (fixed, rest): ([Option<Value>; 3], &[Value]) = match self {
            Self::Const { .. } => ([None; 3], &[]),
            Self::Unary { a, .. } | Self::Extend { a, .. } | Self::Reduce { a } => {
                ([Some(*a), None, None], &[])
            }
            Self::Binary { a, b, .. } | Self::Icmp { a, b, .. } => {
                ([Some(*a), Some(*b), None], &[])
            }
            Self::Select { cond, a, b } => ([Some(*cond), Some(*a), Some(*b)], &[]),
            Self::Load { base, .. } => ([Some(*base), None, None], &[]),
            Self::Store { value, base, .. } => ([Some(*value), Some(*base), None], &[]),
            Self::Call { table, args, .. } => ([Some(*table), None, None], args),
        };
        fixed.into_iter().flatten().chain(rest.iter().copied())
    }

    /// Whether it calls a helper, which every caller-saved register outlives
    /// only in memory.
    #[must_use]
    pub const fn is_call(&self) -> bool {
        matches!(self, Self::Call { .. })
    }
}

/// Where control goes at a block's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// To `to`, its parameters given `args`.
    Jump {
        /// The block.
        to: Block,
        /// Its parameters' values.
        args: Vec<Value>,
    },
    /// To `then` where the [`Type::I32`] `cond` is not zero, else to `other`,
    /// each given its arguments.
    Branch {
        /// What decides.
        cond: Value,
        /// The block where it is not zero.
        then: Block,
        /// Its parameters' values.
        then_args: Vec<Value>,
        /// The block where it is.
        other: Block,
        /// Its parameters' values.
        other_args: Vec<Value>,
    },
    /// Out of the function, answering `value` where it answers one.
    Return {
        /// What it answers.
        value: Option<Value>,
    },
}

impl Terminator {
    /// The values it reads.
    pub fn uses(&self) -> impl Iterator<Item = Value> + '_ {
        let (cond, args, more): (Option<Value>, &[Value], &[Value]) = match self {
            Self::Jump { args, .. } => (None, args, &[]),
            Self::Branch {
                cond,
                then_args,
                other_args,
                ..
            } => (Some(*cond), then_args, other_args),
            Self::Return { value } => (*value, &[], &[]),
        };
        cond.into_iter()
            .chain(args.iter().copied())
            .chain(more.iter().copied())
    }

    /// The blocks it may go to, each with the arguments it gives.
    pub fn successors(&self) -> impl Iterator<Item = (Block, &[Value])> + '_ {
        let (first, second) = match self {
            Self::Jump { to, args } => (Some((*to, args.as_slice())), None),
            Self::Branch {
                then,
                then_args,
                other,
                other_args,
                ..
            } => (
                Some((*then, then_args.as_slice())),
                Some((*other, other_args.as_slice())),
            ),
            Self::Return { .. } => (None, None),
        };
        first.into_iter().chain(second)
    }
}

/// One block: its parameters, its instructions and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockData {
    /// Its parameters, values defined as it is entered.
    pub params: Vec<Value>,
    /// Its instructions, each with the value it defines, if any.
    pub insts: Vec<(Option<Value>, Inst)>,
    /// Where it goes.
    pub term: Terminator,
}

/// A function: its blocks, the first entered with the function's arguments
/// as its parameters, and every value's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub(crate) blocks: Vec<BlockData>,
    pub(crate) types: Vec<Type>,
    pub(crate) ret: Option<Type>,
}

impl Function {
    /// Its blocks, the entry first.
    #[must_use]
    pub fn blocks(&self) -> &[BlockData] {
        &self.blocks
    }

    /// The block `block`.
    #[must_use]
    pub fn block(&self, block: Block) -> &BlockData {
        &self.blocks[block.index()]
    }

    /// The type of `value`.
    #[must_use]
    pub fn type_of(&self, value: Value) -> Type {
        self.types[value.index()]
    }

    /// How many values it defines.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.types.len()
    }

    /// What it returns.
    #[must_use]
    pub const fn ret(&self) -> Option<Type> {
        self.ret
    }

    /// Its parameters: the entry block's.
    #[must_use]
    pub fn params(&self) -> &[Value] {
        self.blocks
            .first()
            .map_or(&[], |entry| entry.params.as_slice())
    }
}

/// Builds a [`Function`] block by block. A block is begun, filled with
/// instructions, and ended by its terminator; the function is finished once
/// every block it made is ended. A misuse — an instruction with no block
/// begun, or put in a block already ended — is kept and answered by
/// [`Builder::finish`].
#[derive(Debug)]
pub struct Builder {
    blocks: Vec<Option<BlockData>>,
    /// Each block's parameters and the instructions given it so far.
    open: Vec<Open>,
    types: Vec<Type>,
    ret: Option<Type>,
    current: Option<Block>,
    misused: Option<BuildError>,
}

/// A block being built.
#[derive(Debug, Default)]
struct Open {
    params: Vec<Value>,
    insts: Vec<(Option<Value>, Inst)>,
}

/// Why a [`Builder`] could not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// A block made was never ended.
    Unended(Block),
    /// An instruction was given with no block begun.
    NoBlock,
    /// An instruction or terminator was given to a block already ended.
    Ended(Block),
    /// More values or blocks were made than 32 bits number.
    TooLarge,
}

impl Builder {
    /// A function answering `ret`, its entry block's parameters of `params`;
    /// the entry block is begun.
    #[must_use]
    pub fn new(params: &[Type], ret: Option<Type>) -> Self {
        let mut builder = Self {
            blocks: Vec::new(),
            open: Vec::new(),
            types: Vec::new(),
            ret,
            current: None,
            misused: None,
        };
        let entry = builder.block(params);
        builder.switch_to(entry);
        builder
    }

    /// A new block with parameters of `params`, not yet begun.
    pub fn block(&mut self, params: &[Type]) -> Block {
        let block = Block(self.number(self.blocks.len()));
        let params = params.iter().map(|&ty| self.value(ty)).collect();
        self.blocks.push(None);
        self.open.push(Open {
            params,
            insts: Vec::new(),
        });
        block
    }

    /// `count` as a 32-bit number; where it is not one, the misuse kept and
    /// a number the finished function never holds.
    fn number(&mut self, count: usize) -> u32 {
        u32::try_from(count).unwrap_or_else(|_| {
            self.misused.get_or_insert(BuildError::TooLarge);
            u32::MAX
        })
    }

    /// The parameters of `block`.
    #[must_use]
    pub fn params(&self, block: Block) -> &[Value] {
        &self.open[block.index()].params
    }

    /// Fill `block` from now on.
    pub fn switch_to(&mut self, block: Block) {
        self.current = Some(block);
    }

    fn value(&mut self, ty: Type) -> Value {
        let value = Value(self.number(self.types.len()));
        self.types.push(ty);
        value
    }

    /// The block being filled, or the misuse kept where none is.
    fn filling(&mut self) -> Option<Block> {
        match self.current {
            Some(block) if self.blocks[block.index()].is_some() => {
                self.misused.get_or_insert(BuildError::Ended(block));
                None
            }
            Some(block) => Some(block),
            None => {
                self.misused.get_or_insert(BuildError::NoBlock);
                None
            }
        }
    }

    fn push(&mut self, defines: Option<Type>, inst: Inst) -> Option<Value> {
        let value = defines.map(|ty| self.value(ty));
        if let Some(block) = self.filling() {
            self.open[block.index()].insts.push((value, inst));
        }
        value
    }

    fn define(&mut self, ty: Type, inst: Inst) -> Value {
        let value = self.value(ty);
        if let Some(block) = self.filling() {
            self.open[block.index()].insts.push((Some(value), inst));
        }
        value
    }

    /// The type of `value`, as made.
    #[must_use]
    pub fn type_of(&self, value: Value) -> Type {
        self.types.get(value.index()).copied().unwrap_or(Type::I64)
    }

    /// A constant.
    pub fn iconst(&mut self, ty: Type, imm: u64) -> Value {
        self.define(
            ty,
            Inst::Const {
                ty,
                imm: ty.wrap(imm),
            },
        )
    }

    /// `op a`.
    pub fn unary(&mut self, op: UnOp, a: Value) -> Value {
        let ty = self.type_of(a);
        self.define(ty, Inst::Unary { op, a })
    }

    /// `a op b`.
    pub fn binary(&mut self, op: BinOp, a: Value, b: Value) -> Value {
        let ty = self.type_of(a);
        self.define(ty, Inst::Binary { op, a, b })
    }

    /// `1` where `a cond b`, else `0`.
    pub fn icmp(&mut self, cond: Cond, a: Value, b: Value) -> Value {
        self.define(Type::I32, Inst::Icmp { cond, a, b })
    }

    /// `a` where `cond` is not zero, else `b`.
    pub fn select(&mut self, cond: Value, a: Value, b: Value) -> Value {
        let ty = self.type_of(a);
        self.define(ty, Inst::Select { cond, a, b })
    }

    /// `a` widened to 64 bits.
    pub fn extend(&mut self, signed: bool, a: Value) -> Value {
        self.define(Type::I64, Inst::Extend { signed, a })
    }

    /// `a`'s low 32 bits.
    pub fn reduce(&mut self, a: Value) -> Value {
        self.define(Type::I32, Inst::Reduce { a })
    }

    /// `width` bytes at `base + offset`, extended to `ty`.
    pub fn load(
        &mut self,
        ty: Type,
        width: Width,
        signed: bool,
        base: Value,
        offset: i32,
    ) -> Value {
        self.define(
            ty,
            Inst::Load {
                width,
                signed,
                ty,
                base,
                offset,
            },
        )
    }

    /// `value`'s low `width` bytes stored at `base + offset`.
    pub fn store(&mut self, width: Width, value: Value, base: Value, offset: i32) {
        self.push(
            None,
            Inst::Store {
                width,
                value,
                base,
                offset,
            },
        );
    }

    /// Helper `index` of the table at `table` called with `args`.
    pub fn call(
        &mut self,
        table: Value,
        index: u32,
        args: &[Value],
        ret: Option<Type>,
    ) -> Option<Value> {
        self.push(
            ret,
            Inst::Call {
                table,
                index,
                args: args.to_vec(),
                ret,
            },
        )
    }

    fn end(&mut self, term: Terminator) {
        if let Some(block) = self.filling() {
            let Open { params, insts } = core::mem::take(&mut self.open[block.index()]);
            self.blocks[block.index()] = Some(BlockData {
                params,
                insts,
                term,
            });
        }
        self.current = None;
    }

    /// End the block with a jump to `to`.
    pub fn jump(&mut self, to: Block, args: &[Value]) {
        self.end(Terminator::Jump {
            to,
            args: args.to_vec(),
        });
    }

    /// End the block with a branch on `cond`.
    pub fn branch(&mut self, cond: Value, then: (Block, &[Value]), other: (Block, &[Value])) {
        self.end(Terminator::Branch {
            cond,
            then: then.0,
            then_args: then.1.to_vec(),
            other: other.0,
            other_args: other.1.to_vec(),
        });
    }

    /// End the block with a return.
    pub fn ret(&mut self, value: Option<Value>) {
        self.end(Terminator::Return { value });
    }

    /// The function, once every block is ended.
    ///
    /// # Errors
    ///
    /// The first misuse, else the first block left unended.
    pub fn finish(self) -> Result<Function, BuildError> {
        if let Some(misused) = self.misused {
            return Err(misused);
        }
        // Every block was numbered without a misuse, so each index fits.
        let blocks = self
            .blocks
            .into_iter()
            .zip(0u32..)
            .map(|(block, n)| block.ok_or(BuildError::Unended(Block(n))))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Function {
            blocks,
            types: self.types,
            ret: self.ret,
        })
    }
}

#[cfg(test)]
#[path = "ir_tests.rs"]
mod tests;

//! The program a pipeline's entry point is lowered to, and the tile of lanes
//! runs: instructions over registers that are either a plane of a word per
//! lane or one word the tile shares, blocks in structured order, and the
//! memory objects its pointers are rooted in.

use alloc::vec::Vec;

use tairix_spirv::Component;

/// The lanes a tile runs at once: the width of a register plane and of a
/// lane mask.
pub(crate) const LANES: usize = 64;

/// A register: the first of the consecutive words a value occupies, either
/// planes of a word per lane or words the tile shares.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Reg(u32);

impl Reg {
    const UNIFORM: u32 = 1 << 31;

    pub(crate) const fn varying(slot: u32) -> Self {
        Self(slot)
    }

    pub(crate) const fn uniform(slot: u32) -> Self {
        Self(slot | Self::UNIFORM)
    }

    pub(crate) const fn is_uniform(self) -> bool {
        self.0 & Self::UNIFORM != 0
    }

    pub(crate) const fn slot(self) -> u32 {
        self.0 & !Self::UNIFORM
    }

    /// The register `words` words further on.
    pub(crate) const fn at(self, words: u32) -> Self {
        Self(self.0 + words)
    }
}

/// How a lane holds one scalar: a word, or two for a 64-bit one, the low
/// word first. A 16-bit value is in its word's low half, the high half
/// clear; a Boolean is one or zero.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Num {
    Bool,
    I16,
    I32,
    I64,
    F16,
    F32,
    F64,
}

impl Num {
    pub(crate) const fn words(self) -> u32 {
        match self {
            Self::I64 | Self::F64 => 2,
            _ => 1,
        }
    }

    pub(crate) const fn bits(self) -> u32 {
        match self {
            Self::Bool => 1,
            Self::I16 | Self::F16 => 16,
            Self::I32 | Self::F32 => 32,
            Self::I64 | Self::F64 => 64,
        }
    }

    pub(crate) const fn is_float(self) -> bool {
        matches!(self, Self::F16 | Self::F32 | Self::F64)
    }
}

/// A component-wise operation of one operand.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op1 {
    SNegate,
    Not,
    FNegate,
    BitReverse,
    BitCount,
    LogicalNot,
    IsNan,
    IsInf,
    Round,
    RoundEven,
    Trunc,
    FAbs,
    SAbs,
    FSign,
    SSign,
    Floor,
    Ceil,
    Fract,
    Radians,
    Degrees,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Exp,
    Log,
    Exp2,
    Log2,
    Sqrt,
    InverseSqrt,
    FindILsb,
    FindSMsb,
    FindUMsb,
}

/// A component-wise operation of two operands of one type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op2 {
    IAdd,
    ISub,
    IMul,
    SDiv,
    UDiv,
    SRem,
    SMod,
    UMod,
    BitAnd,
    BitOr,
    BitXor,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FRem,
    FMod,
    LogicalAnd,
    LogicalOr,
    LogicalEqual,
    LogicalNotEqual,
    Atan2,
    Pow,
    FMin,
    FMax,
    NMin,
    NMax,
    UMin,
    UMax,
    SMin,
    SMax,
    Step,
}

/// A component-wise operation of three operands of one type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op3 {
    FClamp,
    NClamp,
    UClamp,
    SClamp,
    FMix,
    SmoothStep,
    Fma,
}

/// A shift of each component by the matching component of a count.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Shift {
    Left,
    RightLogical,
    RightArithmetic,
}

/// A component-wise comparison, answering Booleans.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Cmp {
    IEqual,
    INotEqual,
    UGreater,
    UGreaterEqual,
    ULess,
    ULessEqual,
    SGreater,
    SGreaterEqual,
    SLess,
    SLessEqual,
    /// A float comparison: true where `ordered` and neither operand is `NaN`
    /// and the relation holds, or where unordered and either is `NaN` or it
    /// holds.
    Float {
        relation: Relation,
        ordered: bool,
    },
}

/// The relation a float comparison tests.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Relation {
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

/// A component-wise conversion from one scalar to another.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Conv {
    /// A float to an unsigned integer, saturating, `NaN` as zero.
    FloatToUnsigned,
    /// A float to a signed integer, saturating, `NaN` as zero.
    FloatToSigned,
    SignedToFloat,
    UnsignedToFloat,
    /// An integer zero-extended or truncated.
    Unsigned,
    /// An integer sign-extended or truncated.
    Signed,
    /// A float rounded to nearest to another width.
    Float,
    /// A float rounded toward zero to another width: a conversion an
    /// admitted `FPRoundingMode RTZ` decorates.
    FloatTowardZero,
    /// A single rounded to a half and back, flushing what no normal half
    /// holds.
    QuantizeToF16,
}

/// Operations over whole vectors and matrices of floats.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Linear {
    /// `a` scaled by the scalar `b`, over `count` components.
    Scale {
        count: u32,
    },
    /// The dot product of two vectors of `n`.
    Dot {
        n: u32,
    },
    /// A `rows`-tall, `cols`-wide matrix `a` times the vector `b`.
    MatrixVector {
        rows: u32,
        cols: u32,
    },
    /// The `rows`-long vector `a` times the `rows`-tall, `cols`-wide matrix
    /// `b`.
    VectorMatrix {
        rows: u32,
        cols: u32,
    },
    /// The `rows` by `inner` matrix `a` times the `inner` by `cols` matrix
    /// `b`.
    MatrixMatrix {
        rows: u32,
        inner: u32,
        cols: u32,
    },
    /// The `rows`-long column `a` times the `cols`-long row `b`.
    Outer {
        rows: u32,
        cols: u32,
    },
    /// The transpose of a `rows`-tall, `cols`-wide matrix.
    Transpose {
        rows: u32,
        cols: u32,
    },
    /// The determinant of a square matrix of side `n`.
    Determinant {
        n: u32,
    },
    /// The inverse of a square matrix of side `n`.
    Inverse {
        n: u32,
    },
    Length {
        n: u32,
    },
    Distance {
        n: u32,
    },
    Normalize {
        n: u32,
    },
    Cross,
    /// `a` facing away from `c` as `b` says: `FaceForward(N, I, Nref)`.
    FaceForward {
        n: u32,
    },
    /// `a` reflected about the normal `b`.
    Reflect {
        n: u32,
    },
    /// `a` refracted through the normal `b` at the ratio `c`, a scalar of
    /// `eta`.
    Refract {
        n: u32,
        eta: Num,
    },
}

/// A packing of a vector into one scalar, or the reverse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Pack {
    Snorm4x8,
    Unorm4x8,
    Snorm2x16,
    Unorm2x16,
    Half2x16,
    UnpackSnorm4x8,
    UnpackUnorm4x8,
    UnpackSnorm2x16,
    UnpackUnorm2x16,
    UnpackHalf2x16,
    /// `count` 16-bit components, a word apiece, into half as many words.
    Halves {
        count: u32,
    },
    /// The reverse of [`Self::Halves`].
    UnpackHalves {
        count: u32,
    },
}

/// An operation answering two results, each `count` components.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Wide {
    /// The sum and its carry.
    AddCarry,
    /// The difference and its borrow.
    SubBorrow,
    /// The low and high halves of the unsigned product.
    UMulExtended,
    /// The low and high halves of the signed product.
    SMulExtended,
    /// A float's fraction and its whole part, both of its sign.
    Modf,
    /// A float's significand in `[0.5, 1)` and its 32-bit exponent.
    Frexp,
}

/// An operation on the bits of an integer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum BitField {
    Insert,
    SExtract,
    UExtract,
}

/// A value an invocation is given from where its tile placed it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Builtin {
    HelperInvocation,
    LocalInvocationId,
    GlobalInvocationId,
    WorkgroupId,
    NumWorkgroups,
    LocalInvocationIndex,
    WorkgroupSize,
    SubgroupSize,
    SubgroupLocalInvocationId,
    NumSubgroups,
    SubgroupId,
    SubgroupEqMask,
    SubgroupGeMask,
    SubgroupGtMask,
    SubgroupLeMask,
    SubgroupLtMask,
}

/// An atomic read-modify-write.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Atomic {
    Load,
    Store,
    Exchange,
    CompareExchange,
    Increment,
    Decrement,
    Add,
    Sub,
    SMin,
    UMin,
    SMax,
    UMax,
    And,
    Or,
    Xor,
}

/// What a subgroup operation computes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Group {
    Elect,
    All,
    Any,
    AllEqual {
        num: Num,
        count: u32,
    },
    /// The value of the lane `b` names, `words` words.
    Broadcast {
        words: u32,
    },
    BroadcastFirst {
        words: u32,
    },
    Ballot,
    InverseBallot,
    BallotBitExtract,
    BallotBitCount(Scan),
    BallotFindLsb,
    BallotFindMsb,
    /// A fold of `count` components of `num`.
    Fold {
        op: Fold,
        num: Num,
        count: u32,
        scan: Scan,
    },
}

/// Which lanes a subgroup fold folds for each lane.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Scan {
    /// Every active lane.
    Reduce,
    /// The active lanes up to and including this one.
    Inclusive,
    /// The active lanes before this one.
    Exclusive,
}

/// The operation a subgroup fold folds with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Fold {
    IAdd,
    IMul,
    SMin,
    SMax,
    UMin,
    UMax,
    FAdd,
    FMul,
    FMin,
    FMax,
    BitAnd,
    BitOr,
    BitXor,
    LogicalAnd,
    LogicalOr,
    LogicalXor,
}

/// A derivative taken across a quad.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Derivative {
    X,
    Y,
    Width,
}

/// How a memory access is ordered against others.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Order {
    Relaxed,
    Acquire,
    Release,
    AcquireRelease,
    SeqCst,
}

/// One step of an access chain from a pointer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    /// `bytes` further on.
    Bytes(u32),
    /// `index` (a scalar of `num`, signed) times `stride` bytes further on.
    Index { index: Reg, num: Num, stride: u32 },
    /// The descriptor `index` names, of an arrayed binding.
    Element { index: Reg, num: Num },
}

/// One scalar a load or store moves between a value and memory: the value's
/// word it starts at, its byte from the pointer, and its width in bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Scalar {
    pub(crate) word: u32,
    pub(crate) byte: u32,
    pub(crate) bits: u8,
}

/// How a load or store lays a value out in memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Plan {
    /// As the registers hold it: `words` consecutive words.
    Words(u32),
    /// Scalar by scalar, as a buffer's explicit layout places them: a range
    /// of [`Program::scalars`].
    Scalars(u32, u32),
}

/// A memory object a pointer is rooted in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Root {
    /// Words of every lane's own, `words` long from word `first`.
    Private { first: u32, words: u32 },
    /// Words every lane of a tile shares, the same values in every lane:
    /// what a uniform built-in holds.
    Shared { first: u32, words: u32 },
    /// Words a workgroup's invocations share, from word `first`.
    Workgroup { first: u32, words: u32 },
    /// One of the pipeline's bindings, by its place in [`Program::bindings`].
    Binding(u32),
    /// The push constants.
    PushConstants,
}

/// A binding the program reaches, which a dispatch resolves through its
/// bind sets.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Binding {
    pub(crate) set: u32,
    pub(crate) number: u32,
    pub(crate) count: u32,
    /// Where its first descriptor lies among its set's, once the pipeline's
    /// layout places it.
    pub(crate) first: u32,
}

/// An image operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Image {
    pub(crate) op: ImageOp,
    /// The image's binding, and the sampler's where one is used.
    pub(crate) binding: u32,
    pub(crate) sampler: Option<u32>,
    /// The descriptor element of each: one word, or two for a sampled image.
    pub(crate) handle: Reg,
    pub(crate) coord: Reg,
    /// How many components the coordinate has, its layer included.
    pub(crate) coords: u8,
    /// The coordinate's scalar: a float, or a signed or unsigned integer.
    pub(crate) coord_num: Num,
    pub(crate) operands: ImageOperands,
    /// What the image's texels read as.
    pub(crate) component: Component,
    pub(crate) dst: Reg,
}

/// What an image operation computes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ImageOp {
    /// A filtered sample; `projective` divides the coordinate by its last
    /// component, `dref` compares against the reference, and the level of
    /// detail is implicit unless a `Lod` or `Grad` operand gives it.
    Sample {
        projective: bool,
        dref: bool,
    },
    Fetch,
    Gather {
        component: Option<Reg>,
        dref: bool,
    },
    Read,
    /// A texel of `components` words written; the rest are zero.
    Write {
        texel: Reg,
        components: u8,
    },
    QuerySizeLod {
        lod: Reg,
    },
    QuerySize,
    QueryLevels,
    QuerySamples,
    QueryLod,
}

/// The optional operands of an image operation, each a register where
/// given.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageOperands {
    pub(crate) dref: Option<Reg>,
    pub(crate) bias: Option<Reg>,
    pub(crate) lod: Option<Reg>,
    /// The derivatives along x then y, each `coords - layered` floats.
    pub(crate) grad: Option<(Reg, Reg)>,
    pub(crate) offset: Option<Reg>,
    /// Four offsets of a gather, each two signed integers.
    pub(crate) offsets: Option<Reg>,
    pub(crate) sample: Option<Reg>,
}

// A program holds up to the instruction budget of these, each as large as
// the largest variant.
const _: () = assert!(size_of::<Inst>() <= 40);

/// One lowered instruction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Inst {
    Copy {
        dst: Reg,
        src: Reg,
        words: u32,
    },
    Zero {
        dst: Reg,
        words: u32,
    },
    Map1 {
        op: Op1,
        num: Num,
        count: u32,
        dst: Reg,
        a: Reg,
    },
    Map2 {
        op: Op2,
        num: Num,
        count: u32,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Map3 {
        op: Op3,
        num: Num,
        count: u32,
        dst: Reg,
        a: Reg,
        b: Reg,
        c: Reg,
    },
    Shift {
        op: Shift,
        num: Num,
        by: Num,
        count: u32,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Compare {
        op: Cmp,
        num: Num,
        count: u32,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Convert {
        op: Conv,
        from: Num,
        to: Num,
        count: u32,
        dst: Reg,
        a: Reg,
    },
    /// `a` where `cond` holds and `b` where it does not: a Boolean for the
    /// whole value where `per` is `words`, else one per `per`-word component.
    Select {
        dst: Reg,
        cond: Reg,
        a: Reg,
        b: Reg,
        words: u32,
        per: u32,
    },
    AnyAll {
        all: bool,
        count: u32,
        dst: Reg,
        a: Reg,
    },
    Linear {
        op: Linear,
        num: Num,
        dst: Reg,
        a: Reg,
        b: Reg,
        c: Reg,
    },
    Pack {
        op: Pack,
        dst: Reg,
        a: Reg,
    },
    /// Two results of `count` components of `num`: the first at `dst`, the
    /// second at `second`.
    Wide {
        op: Wide,
        num: Num,
        count: u32,
        dst: Reg,
        second: Reg,
        a: Reg,
        b: Reg,
    },
    /// `base` and, to insert, `insert`, over bits `offset..offset + bits`,
    /// the two scalars of `by`.
    BitField {
        op: BitField,
        num: Num,
        by: Num,
        count: u32,
        dst: Reg,
        base: Reg,
        insert: Reg,
        offset: Reg,
        bits: Reg,
    },
    /// `a` scaled by two to the integer `b`'s power.
    Ldexp {
        num: Num,
        by: Num,
        count: u32,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// Component `index` (an integer of `by`) of the `count`-component
    /// vector `vector`, each `per` words; zero past its end.
    Extract {
        dst: Reg,
        vector: Reg,
        index: Reg,
        by: Num,
        count: u32,
        per: u32,
    },
    /// `vector` with component `index` replaced by `value`, unchanged past
    /// its end.
    Insert {
        dst: Reg,
        vector: Reg,
        value: Reg,
        index: Reg,
        by: Num,
        count: u32,
        per: u32,
    },
    /// The pointer `base` walked through `steps`.
    Chain {
        dst: Reg,
        base: Reg,
        steps: (u32, u32),
    },
    Load {
        dst: Reg,
        root: Root,
        ptr: Reg,
        plan: u32,
    },
    Store {
        src: Reg,
        root: Root,
        ptr: Reg,
        plan: u32,
    },
    /// The elements of the runtime array closing the block `ptr` points to:
    /// past the block's `offset` bytes, each `stride` long.
    ArrayLength {
        dst: Reg,
        root: Root,
        ptr: Reg,
        offset: u32,
        stride: u32,
    },
    Atomic {
        op: Atomic,
        order: Order,
        root: Root,
        ptr: Reg,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// A pointer to the texel `coord` names in sample `sample` of the
    /// storage image `image`.
    TexelPointer {
        dst: Reg,
        image: u32,
        handle: Reg,
        coord: Reg,
        coords: u8,
        sample: Reg,
    },
    /// The image operation `Program::images` holds at this index.
    Image(u32),
    Group {
        op: Group,
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Derivative {
        op: Derivative,
        fine: bool,
        num: Num,
        count: u32,
        dst: Reg,
        a: Reg,
    },
    Fence(Order),
}

impl Inst {
    /// What one lane's run of the instruction costs, in words it reads,
    /// computes or writes, and never less than one: what the work meter is
    /// charged, so a deadline is checked in proportion to time spent.
    pub(crate) fn work(&self, program: &Program) -> u64 {
        let words = match *self {
            Self::Copy { words, .. } | Self::Zero { words, .. } | Self::Select { words, .. } => {
                words
            }
            Self::Map1 { num, count, .. }
            | Self::Map2 { num, count, .. }
            | Self::Map3 { num, count, .. }
            | Self::Shift { num, count, .. }
            | Self::Compare { num, count, .. }
            | Self::Wide { num, count, .. }
            | Self::BitField { num, count, .. }
            | Self::Ldexp { num, count, .. }
            | Self::Derivative { num, count, .. } => count.saturating_mul(num.words()),
            Self::Convert {
                from, to, count, ..
            } => count.saturating_mul(from.words().max(to.words())),
            Self::AnyAll { count, .. } => count,
            Self::Linear { op, num, .. } => op.work().saturating_mul(num.words()),
            Self::Pack { op, .. } => op.work(),
            Self::Extract { count, per, .. } | Self::Insert { count, per, .. } => {
                count.saturating_mul(per)
            }
            Self::Chain {
                steps: (first, last),
                ..
            } => last.saturating_sub(first),
            Self::Load { plan, .. } | Self::Store { plan, .. } => {
                program.plans.get(plan as usize).map_or(1, Plan::words)
            }
            Self::Image(at) => program
                .images
                .get(at as usize)
                .map_or(1, |image| image.op.work()),
            Self::Group { op, .. } => op.work(),
            Self::ArrayLength { .. }
            | Self::Atomic { .. }
            | Self::TexelPointer { .. }
            | Self::Fence(_) => 1,
        };
        u64::from(words.max(1))
    }
}

impl Plan {
    /// The words a load or store of this plan moves.
    const fn words(&self) -> u32 {
        match *self {
            Self::Words(words) => words,
            Self::Scalars(first, last) => last.saturating_sub(first),
        }
    }
}

impl Linear {
    /// The products and sums one lane's run takes, at least: a determinant
    /// and an inverse by cofactors bounded by the side's cube and fourth
    /// power.
    const fn work(self) -> u32 {
        match self {
            Self::Scale { count: n }
            | Self::Dot { n }
            | Self::Length { n }
            | Self::Distance { n }
            | Self::Normalize { n } => n,
            Self::MatrixVector { rows, cols }
            | Self::VectorMatrix { rows, cols }
            | Self::Outer { rows, cols }
            | Self::Transpose { rows, cols } => rows.saturating_mul(cols),
            Self::MatrixMatrix { rows, inner, cols } => {
                rows.saturating_mul(inner).saturating_mul(cols)
            }
            Self::Determinant { n } => n.saturating_mul(n).saturating_mul(n),
            Self::Inverse { n } => n.saturating_mul(n).saturating_mul(n).saturating_mul(n),
            Self::Cross => 6,
            Self::FaceForward { n } | Self::Reflect { n } | Self::Refract { n, .. } => {
                n.saturating_mul(3)
            }
        }
    }
}

impl Pack {
    const fn work(self) -> u32 {
        match self {
            Self::Halves { count } | Self::UnpackHalves { count } => count,
            _ => 4,
        }
    }
}

impl ImageOp {
    /// The texel words one lane's run decodes at most: a filtered sample
    /// reads eight texels at each of two levels.
    const fn work(self) -> u32 {
        match self {
            Self::Sample { .. } | Self::Gather { .. } | Self::QueryLod => 64,
            Self::Fetch | Self::Read | Self::Write { .. } => 8,
            Self::QuerySizeLod { .. }
            | Self::QuerySize
            | Self::QueryLevels
            | Self::QuerySamples => 4,
        }
    }
}

impl Group {
    const fn work(self) -> u32 {
        match self {
            Self::AllEqual { num, count } | Self::Fold { num, count, .. } => {
                count.saturating_mul(num.words())
            }
            Self::Broadcast { words } | Self::BroadcastFirst { words } => words,
            Self::Ballot
            | Self::InverseBallot
            | Self::BallotBitExtract
            | Self::BallotBitCount(_)
            | Self::BallotFindLsb
            | Self::BallotFindMsb => 4,
            Self::Elect | Self::All | Self::Any => 1,
        }
    }
}

/// Where a block's lanes go next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Term {
    Branch(u32),
    Conditional {
        cond: Reg,
        then: u32,
        other: u32,
    },
    /// The selector's case among `cases`, else `default`.
    Switch {
        selector: Reg,
        wide: bool,
        cases: (u32, u32),
        default: u32,
    },
    /// The lanes end: their entry point returned or reached what cannot be.
    Exit,
    /// The lanes are discarded: a fragment's kill or termination.
    Kill,
    /// A barrier across the workgroup's tiles, then the edge.
    Barrier(u32),
}

/// One edge a terminator takes: the block it reaches and the phi copies it
/// makes on the way.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Edge {
    pub(crate) target: u32,
    pub(crate) copies: (u32, u32),
}

/// One copy an edge makes, in order: a phi's value from its incoming one,
/// cycles broken through the scratch registers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Move {
    pub(crate) dst: Reg,
    pub(crate) src: Reg,
    pub(crate) words: u32,
}

/// A block: its instructions and its terminator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Block {
    pub(crate) insts: (u32, u32),
    pub(crate) term: Term,
}

/// A built-in a tile writes into its lanes' memory before it runs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Given {
    pub(crate) which: Builtin,
    pub(crate) root: Root,
}

/// A built-in a vertex or fragment stage's fixed function feeds its
/// invocations or takes from them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Fixed {
    VertexIndex,
    InstanceIndex,
    Position,
    PointSize,
    FragCoord,
    FrontFacing,
    PointCoord,
    SampleMask,
    FragDepth,
}

/// What a port carries: a fixed-function built-in, or a location's run of
/// components under a fragment output's blend index.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Carries {
    Fixed(Fixed),
    Located {
        location: u32,
        component: u32,
        index: u32,
    },
}

/// A value a vertex or fragment invocation is fed or gives back, `words`
/// words of each lane's private memory from word `first`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Port {
    pub(crate) carries: Carries,
    pub(crate) first: u32,
    pub(crate) words: u32,
}

/// Whether a workgroup of `invocations` takes tiles of its own: one of more
/// than half a tile does, and smaller ones share a tile.
pub(crate) const fn owns_tiles(invocations: usize) -> bool {
    invocations > LANES / 2
}

/// A lowered entry point.
#[derive(Debug)]
pub(crate) struct Program {
    pub(crate) insts: Vec<Inst>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) moves: Vec<Move>,
    /// Each switch case's literal and the edge it takes.
    pub(crate) cases: Vec<(u64, u32)>,
    pub(crate) steps: Vec<Step>,
    /// Each image instruction's operation, apart so every instruction is
    /// not as large as the largest.
    pub(crate) images: Vec<Image>,
    pub(crate) plans: Vec<Plan>,
    pub(crate) scalars: Vec<Scalar>,
    pub(crate) bindings: Vec<Binding>,
    /// The descriptors its bindings read, all together.
    pub(crate) descriptors: u64,
    pub(crate) given: Vec<Given>,
    /// A vertex or fragment stage's ports: what its fixed function feeds
    /// before the tile runs, and what it reads back after.
    pub(crate) inputs: Vec<Port>,
    pub(crate) outputs: Vec<Port>,
    /// The constants the uniform registers start with, from slot zero.
    pub(crate) constants: Vec<u32>,
    /// How many words of planes, uniform registers, private memory, shared
    /// memory and workgroup memory a tile and its workgroup use.
    pub(crate) varying_words: u32,
    pub(crate) uniform_words: u32,
    pub(crate) private_words: u32,
    pub(crate) shared_words: u32,
    pub(crate) workgroup_words: u32,
    /// The planes an edge's copies go through to break a cycle.
    pub(crate) scratch: Reg,
    pub(crate) workgroup_size: [u32; 3],
    /// Whether a block ends at a barrier, where a workgroup's tiles wait on
    /// one another, so each keeps its own memory meanwhile.
    pub(crate) barriers: bool,
}

/// A program's tables free as long as they are large, so no fair lock is held
/// as they go.
#[cfg(test)]
impl Drop for Program {
    fn drop(&mut self) {
        crate::fair::held::note_drop();
    }
}

impl Program {
    /// An empty program for workgroups of `workgroup_size`, its scratch
    /// planes at `scratch` until registers are allocated.
    pub(crate) const fn new(workgroup_size: [u32; 3], scratch: Reg) -> Self {
        Self {
            insts: Vec::new(),
            blocks: Vec::new(),
            edges: Vec::new(),
            moves: Vec::new(),
            cases: Vec::new(),
            steps: Vec::new(),
            images: Vec::new(),
            plans: Vec::new(),
            scalars: Vec::new(),
            bindings: Vec::new(),
            descriptors: 0,
            given: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            constants: Vec::new(),
            varying_words: 0,
            uniform_words: 0,
            private_words: 0,
            shared_words: 0,
            workgroup_words: 0,
            scratch,
            workgroup_size,
            barriers: false,
        }
    }

    pub(crate) fn inst_range(&self, range: (u32, u32)) -> &[Inst] {
        self.insts
            .get(range.0 as usize..range.1 as usize)
            .unwrap_or(&[])
    }

    /// Its tables given back what they grew past their entries, as it is held
    /// and charged for as long as its pipeline lives.
    pub(crate) fn trim(&mut self) {
        use tairix_util::fallible::trim;
        trim(&mut self.insts);
        trim(&mut self.blocks);
        trim(&mut self.edges);
        trim(&mut self.moves);
        trim(&mut self.cases);
        trim(&mut self.steps);
        trim(&mut self.images);
        trim(&mut self.plans);
        trim(&mut self.scalars);
        trim(&mut self.bindings);
        trim(&mut self.given);
        trim(&mut self.inputs);
        trim(&mut self.outputs);
        trim(&mut self.constants);
    }

    /// The bytes of memory it holds past its own size.
    pub(crate) fn held_bytes(&self) -> u64 {
        use tairix_util::fallible::held;
        [
            held(&self.insts),
            held(&self.blocks),
            held(&self.edges),
            held(&self.moves),
            held(&self.cases),
            held(&self.steps),
            held(&self.images),
            held(&self.plans),
            held(&self.scalars),
            held(&self.bindings),
            held(&self.given),
            held(&self.inputs),
            held(&self.outputs),
            held(&self.constants),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

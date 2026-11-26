//! The corpus's structure: composites and the copies between them, memory
//! through variables, control flow, atomics, barriers across a workgroup,
//! and the subgroup instructions over a subgroup of one.

use tairix_spirv::enumerant::{
    BuiltIn, Capability, GroupOperation, MemorySemantics, Scope, StorageClass,
};
use tairix_spirv::{Arg, BuildError, Function, Id, Op, Version};

use super::{exact, local, member, op, Family, Held, Io};

pub(super) const FAMILIES: &[Family] = &[
    Family::compute("the corpus's composites", &[], composites),
    Family::compute("the corpus's logical copy", &[], logical).at(Version { major: 1, minor: 4 }),
    Family::compute("the corpus's memory", &[], memory),
    Family::compute("the corpus's control flow", &[], control),
    Family::compute("the corpus's atomics", &[], atomics),
    Family::compute("the corpus's barriers", &[], barriers).across(4),
    Family::compute(
        "the corpus's subgroups",
        &[
            Capability::GROUP_NON_UNIFORM,
            Capability::GROUP_NON_UNIFORM_VOTE,
            Capability::GROUP_NON_UNIFORM_BALLOT,
            Capability::GROUP_NON_UNIFORM_ARITHMETIC,
        ],
        subgroups,
    ),
];

/// Shuffles, dynamic extracts and inserts, the copies of an object, and
/// structures and arrays built, changed and taken apart.
fn composites(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let v4 = kit.uvec(4);
    let (v, w) = (
        io.uints(f, &[11, 22, 33, 44])?,
        io.uints(f, &[55, 66, 77, 88])?,
    );
    let shuffled = f.op_args(
        Op::VECTOR_SHUFFLE,
        v4,
        &[
            Arg::Id(v),
            Arg::Id(w),
            Arg::Word(7),
            Arg::Word(0),
            Arg::Word(5),
            Arg::Word(2),
        ],
    )?;
    io.out(f, exact("VectorShuffle"), (shuffled, v4))?;
    let (index, value) = (io.word(f, 2)?, io.word(f, 99)?);
    op(
        f,
        io,
        exact("VectorExtractDynamic"),
        (Op::VECTOR_EXTRACT_DYNAMIC, kit.uint, &[v, index]),
    )?;
    op(
        f,
        io,
        exact("VectorInsertDynamic"),
        (Op::VECTOR_INSERT_DYNAMIC, v4, &[v, value, index]),
    )?;
    op(
        f,
        io,
        exact("CopyObject"),
        (Op::COPY_OBJECT, v4, &[shuffled]),
    )?;
    let v2 = kit.vec(2);
    let pair = f.module().structure(&[kit.uint, v2])?;
    let reals = io.floats(f, &[1.5, -2.0])?;
    let built = f.op(Op::COMPOSITE_CONSTRUCT, pair, &[value, reals])?;
    let replacement = io.float(f, 8.25)?;
    let changed = f.op_args(
        Op::COMPOSITE_INSERT,
        pair,
        &[
            Arg::Id(replacement),
            Arg::Id(built),
            Arg::Word(1),
            Arg::Word(0),
        ],
    )?;
    let count = member(f, changed, kit.uint, 0)?;
    io.out(f, exact("CompositeInsert"), (count, kit.uint))?;
    let inner = member(f, changed, v2, 1)?;
    io.out(f, exact("CompositeInsert"), (inner, v2))?;
    let three = f.module().uint(3)?;
    let array = f.module().array(kit.uint, three)?;
    let listed = f.op(Op::COMPOSITE_CONSTRUCT, array, &[value, index, count])?;
    f.emit(Op::NOP, &[])?;
    let middle = member(f, listed, kit.uint, 1)?;
    io.out(f, exact("CompositeExtract"), (middle, kit.uint))
}

/// A structure copied to another of the same members.
fn logical(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let first = f.module().structure(&[kit.uint, kit.float])?;
    let second = f.module().structure(&[kit.uint, kit.float])?;
    let (count, real) = (io.word(f, 0x0123_4567)?, io.float(f, -3.5)?);
    let made = f.op(Op::COMPOSITE_CONSTRUCT, first, &[count, real])?;
    let copied = f.op(Op::COPY_LOGICAL, second, &[made])?;
    let kept = member(f, copied, kit.uint, 0)?;
    io.out(f, exact("CopyLogical"), (kept, kit.uint))?;
    let kept = member(f, copied, kit.float, 1)?;
    io.out(f, exact("CopyLogical"), (kept, kit.float))
}

/// An array stored whole, copied, changed through an index and loaded; and
/// the length of the buffer's runtime array.
fn memory(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let four = f.module().uint(4)?;
    let array = f.module().array(kit.uint, four)?;
    let (source, target) = (local(f, array)?, local(f, array)?);
    let parts = [
        io.word(f, 5)?,
        io.word(f, 6)?,
        io.word(f, 7)?,
        io.word(f, 8)?,
    ];
    let whole = f.op(Op::COMPOSITE_CONSTRUCT, array, &parts)?;
    f.emit(Op::STORE, &[source, whole])?;
    f.emit(Op::COPY_MEMORY, &[target, source])?;
    let element = f.module().pointer(StorageClass::FUNCTION, kit.uint)?;
    let (index, value) = (io.word(f, 3)?, io.word(f, 42)?);
    let changed = f.op(Op::IN_BOUNDS_ACCESS_CHAIN, element, &[target, index])?;
    f.emit(Op::STORE, &[changed, value])?;
    let copied = f.op(Op::LOAD, array, &[target])?;
    for at in 0..4 {
        let part = member(f, copied, kit.uint, at)?;
        io.out(f, exact("CopyMemory"), (part, kit.uint))?;
    }
    let start = io.word(f, 0)?;
    let first = f.op(Op::ACCESS_CHAIN, element, &[source, start])?;
    op(f, io, exact("AccessChain"), (Op::LOAD, kit.uint, &[first]))?;
    let length = f.op_args(
        Op::ARRAY_LENGTH,
        kit.uint,
        &[Arg::Id(kit.words), Arg::Word(0)],
    )?;
    io.out(f, exact("ArrayLength"), (length, kit.uint))
}

/// A call answered by a value, a selection merged by a phi, a switch, a
/// loop, and a branch never taken that is unreachable.
fn control(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let helper = f.module().id()?;
    // One more than three times the sum of its parameters.
    f.module()
        .define(helper, kit.uint, &[kit.uint], |g, params| {
            let (three, one) = (g.module().uint(3)?, g.module().uint(1)?);
            let mut answer = one;
            for &param in params {
                let tripled = g.op(Op::I_MUL, kit.uint, &[param, three])?;
                answer = g.op(Op::I_ADD, kit.uint, &[answer, tripled])?;
            }
            g.emit(Op::RETURN_VALUE, &[answer])
        })?;
    let input = io.word(f, 7)?;
    op(
        f,
        io,
        exact("FunctionCall"),
        (Op::FUNCTION_CALL, kit.uint, &[helper, input]),
    )?;
    let (flag, hundred) = (io.boolean(f, true)?, io.word(f, 100)?);
    let (taken, other, merged) = (f.label()?, f.label()?, f.label()?);
    f.emit_args(Op::SELECTION_MERGE, &[Arg::Id(merged), Arg::Word(0)])?;
    f.emit(Op::BRANCH_CONDITIONAL, &[flag, taken, other])?;
    f.begin(taken)?;
    let raised = f.op(Op::I_ADD, kit.uint, &[input, hundred])?;
    f.emit(Op::BRANCH, &[merged])?;
    f.begin(other)?;
    let lowered = f.op(Op::I_SUB, kit.uint, &[hundred, input])?;
    f.emit(Op::BRANCH, &[merged])?;
    f.begin(merged)?;
    op(
        f,
        io,
        exact("Phi"),
        (Op::PHI, kit.uint, &[raised, taken, lowered, other]),
    )?;
    switch(f, io, input)?;
    looped(f, io)?;
    let never = io.boolean(f, false)?;
    let (dead, live) = (f.label()?, f.label()?);
    f.emit_args(Op::SELECTION_MERGE, &[Arg::Id(live), Arg::Word(0)])?;
    f.emit(Op::BRANCH_CONDITIONAL, &[never, dead, live])?;
    f.begin(dead)?;
    f.emit(Op::UNREACHABLE, &[])?;
    f.begin(live)
}

/// A switch on an input word to one of three cases or its default, each
/// adding its own amount to `base`.
fn switch(f: &mut Function<'_>, io: &mut Io, base: Id) -> Result<(), BuildError> {
    let kit = io.kit;
    let selector = io.word(f, 2)?;
    let (one, two, five, default, merged) =
        (f.label()?, f.label()?, f.label()?, f.label()?, f.label()?);
    f.emit_args(Op::SELECTION_MERGE, &[Arg::Id(merged), Arg::Word(0)])?;
    f.emit_args(
        Op::SWITCH,
        &[
            Arg::Id(selector),
            Arg::Id(default),
            Arg::Word(1),
            Arg::Id(one),
            Arg::Word(2),
            Arg::Id(two),
            Arg::Word(5),
            Arg::Id(five),
        ],
    )?;
    // Every pair is written below, each arm's value then its label.
    let mut incoming = [[selector; 2]; 4];
    let arms = [(one, 10), (two, 20), (five, 50), (default, 90)];
    for (pair, (label, amount)) in incoming.iter_mut().zip(arms) {
        f.begin(label)?;
        let step = f.module().uint(amount)?;
        let value = f.op(Op::I_ADD, kit.uint, &[base, step])?;
        f.emit(Op::BRANCH, &[merged])?;
        *pair = [value, label];
    }
    f.begin(merged)?;
    op(
        f,
        io,
        exact("Switch"),
        (Op::PHI, kit.uint, incoming.as_flattened()),
    )
}

/// The sum of one to an input word, counted in a loop.
fn looped(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let (counter, total) = (local(f, kit.uint)?, local(f, kit.uint)?);
    let (last, one, zero) = (io.word(f, 10)?, f.module().uint(1)?, f.module().uint(0)?);
    f.emit(Op::STORE, &[counter, one])?;
    f.emit(Op::STORE, &[total, zero])?;
    f.while_loop(
        |f| {
            let at = f.op(Op::LOAD, kit.uint, &[counter])?;
            f.op(Op::U_LESS_THAN_EQUAL, kit.boolean, &[at, last])
        },
        |f| {
            let (at, sum) = (
                f.op(Op::LOAD, kit.uint, &[counter])?,
                f.op(Op::LOAD, kit.uint, &[total])?,
            );
            let added = f.op(Op::I_ADD, kit.uint, &[sum, at])?;
            f.emit(Op::STORE, &[total, added])
        },
        |f| {
            let at = f.op(Op::LOAD, kit.uint, &[counter])?;
            let next = f.op(Op::I_ADD, kit.uint, &[at, one])?;
            f.emit(Op::STORE, &[counter, next])
        },
    )?;
    op(f, io, exact("LoopMerge"), (Op::LOAD, kit.uint, &[total]))
}

/// Each atomic on a word of its own: what it answered, and the word it
/// left.
fn atomics(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let device = f.module().uint(Scope::DEVICE.0)?;
    let relaxed = f.module().uint(MemorySemantics::RELAXED.0)?;
    for (case, code, start, operand) in [
        ("AtomicExchange", Op::ATOMIC_EXCHANGE, 40, 2),
        ("AtomicIAdd", Op::ATOMIC_I_ADD, 40, 2),
        ("AtomicISub", Op::ATOMIC_I_SUB, 40, 3),
        ("AtomicSMin", Op::ATOMIC_S_MIN, (-5i32).cast_unsigned(), 7),
        ("AtomicUMin", Op::ATOMIC_U_MIN, (-5i32).cast_unsigned(), 7),
        ("AtomicSMax", Op::ATOMIC_S_MAX, (-5i32).cast_unsigned(), 7),
        ("AtomicUMax", Op::ATOMIC_U_MAX, (-5i32).cast_unsigned(), 7),
        ("AtomicAnd", Op::ATOMIC_AND, 0b1100, 0b1010),
        ("AtomicOr", Op::ATOMIC_OR, 0b1100, 0b1010),
        ("AtomicXor", Op::ATOMIC_XOR, 0b1100, 0b1010),
    ] {
        let slot = io.slot(f, case, start)?;
        let value = io.word(f, operand)?;
        op(
            f,
            io,
            exact(case),
            (code, kit.uint, &[slot, device, relaxed, value]),
        )?;
    }
    for (case, code) in [
        ("AtomicIIncrement", Op::ATOMIC_I_INCREMENT),
        ("AtomicIDecrement", Op::ATOMIC_I_DECREMENT),
    ] {
        let slot = io.slot(f, case, 9)?;
        op(
            f,
            io,
            exact(case),
            (code, kit.uint, &[slot, device, relaxed]),
        )?;
    }
    for comparator in [17, 18] {
        let slot = io.slot(f, "AtomicCompareExchange", 17)?;
        let (value, comparator) = (io.word(f, 23)?, io.word(f, comparator)?);
        op(
            f,
            io,
            exact("AtomicCompareExchange"),
            (
                Op::ATOMIC_COMPARE_EXCHANGE,
                kit.uint,
                &[slot, device, relaxed, relaxed, value, comparator],
            ),
        )?;
    }
    let slot = io.slot(f, "AtomicStore", 0)?;
    let value = io.word(f, 31_337)?;
    f.emit(Op::ATOMIC_STORE, &[slot, device, relaxed, value])?;
    op(
        f,
        io,
        exact("AtomicLoad"),
        (Op::ATOMIC_LOAD, kit.uint, &[slot, device, relaxed]),
    )
}

/// Each invocation's word of workgroup memory, read by its neighbour across
/// a barrier.
fn barriers(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let lane = io.built_in(f, (BuiltIn::LOCAL_INVOCATION_INDEX, kit.uint))?;
    let lanes = f.module().uint(io.lanes())?;
    let shared_ty = f.module().array(kit.uint, lanes)?;
    let shared = io.global(f, (shared_ty, StorageClass::WORKGROUP))?;
    let element = f.module().pointer(StorageClass::WORKGROUP, kit.uint)?;
    let (scale, offset) = (io.word(f, 3)?, io.word(f, 1)?);
    let scaled = f.op(Op::I_MUL, kit.uint, &[lane, scale])?;
    let value = f.op(Op::I_ADD, kit.uint, &[scaled, offset])?;
    let own = f.op(Op::ACCESS_CHAIN, element, &[shared, lane])?;
    f.emit(Op::STORE, &[own, value])?;
    let workgroup = f.module().uint(Scope::WORKGROUP.0)?;
    let shared_memory = MemorySemantics::ACQUIRE_RELEASE.0 | MemorySemantics::WORKGROUP_MEMORY.0;
    let ordered = f.module().uint(shared_memory)?;
    f.emit(Op::CONTROL_BARRIER, &[workgroup, workgroup, ordered])?;
    let one = f.module().uint(1)?;
    let after = f.op(Op::I_ADD, kit.uint, &[lane, one])?;
    let neighbour = f.op(Op::U_MOD, kit.uint, &[after, lanes])?;
    let theirs = f.op(Op::ACCESS_CHAIN, element, &[shared, neighbour])?;
    let seen = f.op(Op::LOAD, kit.uint, &[theirs])?;
    let device = f.module().uint(Scope::DEVICE.0)?;
    let buffer_memory = MemorySemantics::ACQUIRE_RELEASE.0 | MemorySemantics::UNIFORM_MEMORY.0;
    let released = f.module().uint(buffer_memory)?;
    f.emit(Op::MEMORY_BARRIER, &[device, released])?;
    io.out_lanes(f, exact("ControlBarrier"), (seen, kit.uint), lane)
}

/// The subgroup instructions over the one invocation of the kernel's
/// workgroup, the first of its subgroup: each answers what one lane makes of
/// its own value.
fn subgroups(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let subgroup = f.module().uint(Scope::SUBGROUP.0)?;
    let lane = io.built_in(f, (BuiltIn::SUBGROUP_LOCAL_INVOCATION_ID, kit.uint))?;
    io.out(f, exact("SubgroupLocalInvocationId"), (lane, kit.uint))?;
    op(
        f,
        io,
        exact("GroupNonUniformElect"),
        (Op::GROUP_NON_UNIFORM_ELECT, kit.boolean, &[subgroup]),
    )?;
    let (yes, no) = (io.boolean(f, true)?, io.boolean(f, false)?);
    op(
        f,
        io,
        exact("GroupNonUniformAll"),
        (Op::GROUP_NON_UNIFORM_ALL, kit.boolean, &[subgroup, yes]),
    )?;
    op(
        f,
        io,
        exact("GroupNonUniformAny"),
        (Op::GROUP_NON_UNIFORM_ANY, kit.boolean, &[subgroup, no]),
    )?;
    let value = io.word(f, 0x1234_5678)?;
    op(
        f,
        io,
        exact("GroupNonUniformAllEqual"),
        (
            Op::GROUP_NON_UNIFORM_ALL_EQUAL,
            kit.boolean,
            &[subgroup, value],
        ),
    )?;
    let first = f.module().uint(0)?;
    op(
        f,
        io,
        exact("GroupNonUniformBroadcast"),
        (
            Op::GROUP_NON_UNIFORM_BROADCAST,
            kit.uint,
            &[subgroup, value, first],
        ),
    )?;
    op(
        f,
        io,
        exact("GroupNonUniformBroadcastFirst"),
        (
            Op::GROUP_NON_UNIFORM_BROADCAST_FIRST,
            kit.uint,
            &[subgroup, value],
        ),
    )?;
    ballots(f, io, (subgroup, yes, lane))?;
    reductions(f, io, subgroup)
}

/// A ballot of every lane, read back only through what its lane alone
/// makes of it.
fn ballots(
    f: &mut Function<'_>,
    io: &mut Io,
    (subgroup, yes, lane): (Id, Id, Id),
) -> Result<(), BuildError> {
    let kit = io.kit;
    let v4 = kit.uvec(4);
    let ballot = f.op(Op::GROUP_NON_UNIFORM_BALLOT, v4, &[subgroup, yes])?;
    op(
        f,
        io,
        exact("GroupNonUniformInverseBallot"),
        (
            Op::GROUP_NON_UNIFORM_INVERSE_BALLOT,
            kit.boolean,
            &[subgroup, ballot],
        ),
    )?;
    op(
        f,
        io,
        exact("GroupNonUniformBallotBitExtract"),
        (
            Op::GROUP_NON_UNIFORM_BALLOT_BIT_EXTRACT,
            kit.boolean,
            &[subgroup, ballot, lane],
        ),
    )?;
    for scan in [
        GroupOperation::REDUCE,
        GroupOperation::INCLUSIVE_SCAN,
        GroupOperation::EXCLUSIVE_SCAN,
    ] {
        let counted = f.op_args(
            Op::GROUP_NON_UNIFORM_BALLOT_BIT_COUNT,
            kit.uint,
            &[Arg::Id(subgroup), Arg::Word(scan.0), Arg::Id(ballot)],
        )?;
        io.out(
            f,
            exact("GroupNonUniformBallotBitCount"),
            (counted, kit.uint),
        )?;
    }
    for (case, code) in [
        (
            "GroupNonUniformBallotFindLSB",
            Op::GROUP_NON_UNIFORM_BALLOT_FIND_LSB,
        ),
        (
            "GroupNonUniformBallotFindMSB",
            Op::GROUP_NON_UNIFORM_BALLOT_FIND_MSB,
        ),
    ] {
        let found = f.op(code, kit.uint, &[subgroup, ballot])?;
        op(
            f,
            io,
            exact(case),
            (Op::I_EQUAL, kit.boolean, &[found, lane]),
        )?;
    }
    Ok(())
}

/// What a reduction reduces.
#[derive(Clone, Copy)]
enum Over {
    Word,
    Signed,
    Real,
    Truth,
}

/// The arithmetic reductions, and what each reduces.
const REDUCTIONS: [(&str, Op, Over); 16] = [
    (
        "GroupNonUniformIAdd",
        Op::GROUP_NON_UNIFORM_I_ADD,
        Over::Word,
    ),
    (
        "GroupNonUniformIMul",
        Op::GROUP_NON_UNIFORM_I_MUL,
        Over::Word,
    ),
    (
        "GroupNonUniformSMin",
        Op::GROUP_NON_UNIFORM_S_MIN,
        Over::Signed,
    ),
    (
        "GroupNonUniformSMax",
        Op::GROUP_NON_UNIFORM_S_MAX,
        Over::Signed,
    ),
    (
        "GroupNonUniformUMin",
        Op::GROUP_NON_UNIFORM_U_MIN,
        Over::Word,
    ),
    (
        "GroupNonUniformUMax",
        Op::GROUP_NON_UNIFORM_U_MAX,
        Over::Word,
    ),
    (
        "GroupNonUniformBitwiseAnd",
        Op::GROUP_NON_UNIFORM_BITWISE_AND,
        Over::Word,
    ),
    (
        "GroupNonUniformBitwiseOr",
        Op::GROUP_NON_UNIFORM_BITWISE_OR,
        Over::Word,
    ),
    (
        "GroupNonUniformBitwiseXor",
        Op::GROUP_NON_UNIFORM_BITWISE_XOR,
        Over::Word,
    ),
    (
        "GroupNonUniformFAdd",
        Op::GROUP_NON_UNIFORM_F_ADD,
        Over::Real,
    ),
    (
        "GroupNonUniformFMul",
        Op::GROUP_NON_UNIFORM_F_MUL,
        Over::Real,
    ),
    (
        "GroupNonUniformFMin",
        Op::GROUP_NON_UNIFORM_F_MIN,
        Over::Real,
    ),
    (
        "GroupNonUniformFMax",
        Op::GROUP_NON_UNIFORM_F_MAX,
        Over::Real,
    ),
    (
        "GroupNonUniformLogicalAnd",
        Op::GROUP_NON_UNIFORM_LOGICAL_AND,
        Over::Truth,
    ),
    (
        "GroupNonUniformLogicalOr",
        Op::GROUP_NON_UNIFORM_LOGICAL_OR,
        Over::Truth,
    ),
    (
        "GroupNonUniformLogicalXor",
        Op::GROUP_NON_UNIFORM_LOGICAL_XOR,
        Over::Truth,
    ),
];

/// The group operations a reduction runs as.
const SCANS: [GroupOperation; 3] = [
    GroupOperation::REDUCE,
    GroupOperation::INCLUSIVE_SCAN,
    GroupOperation::EXCLUSIVE_SCAN,
];

/// Every arithmetic reduction of one value as a reduction and an inclusive
/// and an exclusive scan.
fn reductions(f: &mut Function<'_>, io: &mut Io, subgroup: Id) -> Result<(), BuildError> {
    let kit = io.kit;
    let word = (kit.uint, io.word(f, 0x8000_0003)?);
    let signed = (kit.int, io.int(f, -77)?);
    let real = (kit.float, io.float(f, 2.5)?);
    let truth = (kit.boolean, io.boolean(f, true)?);
    for (case, code, over) in REDUCTIONS {
        let (ty, value) = match over {
            Over::Word => word,
            Over::Signed => signed,
            Over::Real => real,
            Over::Truth => truth,
        };
        // A sum's identity, which one lane's exclusive scan answers, is a
        // zero whose sign the device chooses.
        let held = if matches!(over, Over::Real) {
            Held::Ulps(0)
        } else {
            Held::Exact
        };
        // One lane's exclusive scan is the identity: an infinity for these.
        let unbounded = code == Op::GROUP_NON_UNIFORM_F_MIN || code == Op::GROUP_NON_UNIFORM_F_MAX;
        let scans = if unbounded { &SCANS[..2] } else { &SCANS[..] };
        for scan in scans {
            let answer = f.op_args(
                code,
                ty,
                &[Arg::Id(subgroup), Arg::Word(scan.0), Arg::Id(value)],
            )?;
            io.out(f, (case, held), (answer, ty))?;
        }
    }
    Ok(())
}

//! Subgroup operations over a tile of 64 lanes, two subgroups of 32: votes,
//! broadcasts, ballots, folds and scans, the subgroup built-ins, and the
//! lanes a loop and an early return leave together at their merges.

use alloc::string::String;
use alloc::vec::Vec;

use super::{f, Case, Scalar, Ty};

const CAPS: &str = "OpCapability GroupNonUniform\nOpCapability GroupNonUniformVote\nOpCapability GroupNonUniformBallot\nOpCapability GroupNonUniformArithmetic";
const SCOPE: &str = "%subgroup = OpConstant %uint 3";
const WIDTH: usize = 32;
const LANES: usize = 64;

/// A case of every lane, reading `input(lane)` and expected to write
/// `output(lane)`.
fn every(
    inputs: &[Ty],
    output: Ty,
    body: &str,
    input: impl Fn(usize) -> Vec<u32>,
    expected: impl Fn(usize) -> Vec<u32>,
) -> Case {
    (0..LANES).fold(
        Case::new(inputs, output, body).caps(CAPS).decls(SCOPE),
        |c, lane| c.lane(&input(lane), &expected(lane)),
    )
}

/// The lanes of `lane`'s subgroup.
fn subgroup(lane: usize) -> core::ops::Range<usize> {
    let first = lane - lane % WIDTH;
    first..first + WIDTH
}

fn word(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

pub(super) fn cases() -> Vec<Case> {
    let mut cases = votes();
    cases.extend(ballots());
    cases.extend(folds());
    cases.extend(reconvergence());
    cases
}

fn votes() -> Vec<Case> {
    use Ty::{Bool, U32};
    alloc::vec![
        every(
            &[U32],
            Bool,
            "%r = OpGroupNonUniformElect %bool %subgroup",
            |_| alloc::vec![0],
            |l| alloc::vec![u32::from(l % WIDTH == 0)]
        ),
        every(
            &[Bool],
            Bool,
            "%r = OpGroupNonUniformAll %bool %subgroup %a",
            |l| alloc::vec![u32::from(l < 40)],
            |l| alloc::vec![u32::from(l < WIDTH)]
        ),
        every(
            &[Bool],
            Bool,
            "%r = OpGroupNonUniformAny %bool %subgroup %a",
            |l| alloc::vec![u32::from(l == 50)],
            |l| alloc::vec![u32::from(l >= WIDTH)]
        ),
        every(
            &[U32],
            Bool,
            "%r = OpGroupNonUniformAllEqual %bool %subgroup %a",
            |l| alloc::vec![word(l / WIDTH)],
            |_| alloc::vec![1]
        ),
        every(
            &[U32],
            Bool,
            "%r = OpGroupNonUniformAllEqual %bool %subgroup %a",
            |l| alloc::vec![word(l)],
            |_| alloc::vec![0]
        ),
        every(
            &[U32],
            U32,
            "%r = OpGroupNonUniformBroadcast %uint %subgroup %a %subgroup",
            |l| alloc::vec![word(l * 10)],
            |l| alloc::vec![word((subgroup(l).start + 3) * 10)]
        ),
        every(
            &[U32],
            U32,
            "%r = OpGroupNonUniformBroadcastFirst %uint %subgroup %a",
            |l| alloc::vec![word(l * 10)],
            |l| alloc::vec![word(subgroup(l).start * 10)]
        ),
    ]
}

fn ballots() -> Vec<Case> {
    use Ty::{Bool, U32};
    let v4 = Ty::V(Scalar::U32, 4);
    let ballot = |mask: u32| alloc::vec![mask, 0, 0, 0];
    alloc::vec![
        every(
            &[Bool],
            v4,
            "%r = OpGroupNonUniformBallot %v4uint %subgroup %a",
            |l| alloc::vec![u32::from(l % 2 == 0)],
            |_| ballot(0x5555_5555)
        ),
        every(
            &[v4],
            Bool,
            "%r = OpGroupNonUniformInverseBallot %bool %subgroup %a",
            |_| ballot(0xffff),
            |l| alloc::vec![u32::from(l % WIDTH < 16)]
        ),
        every(
            &[v4, U32],
            Bool,
            "%r = OpGroupNonUniformBallotBitExtract %bool %subgroup %a %b",
            |l| {
                let mut v = ballot(0xf0);
                v.push(word(l % WIDTH));
                v
            },
            |l| alloc::vec![u32::from((4..8).contains(&(l % WIDTH)))]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotBitCount %uint %subgroup Reduce %a",
            |_| ballot(u32::MAX),
            |_| alloc::vec![32]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotBitCount %uint %subgroup InclusiveScan %a",
            |_| ballot(u32::MAX),
            |l| alloc::vec![word(l % WIDTH + 1)]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotBitCount %uint %subgroup ExclusiveScan %a",
            |_| ballot(u32::MAX),
            |l| alloc::vec![word(l % WIDTH)]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotFindLSB %uint %subgroup %a",
            |_| ballot(0x8),
            |_| alloc::vec![3]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotFindMSB %uint %subgroup %a",
            |_| alloc::vec![0x10, 0, 0, 0],
            |_| alloc::vec![4]
        ),
        // Only the bits of a subgroup's own lanes count, however many the
        // ballot sets past them.
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotBitCount %uint %subgroup Reduce %a",
            |_| alloc::vec![u32::MAX; 4],
            |_| alloc::vec![word(WIDTH)]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotFindMSB %uint %subgroup %a",
            |_| alloc::vec![1, 1, 0, 0],
            |_| alloc::vec![0]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotFindMSB %uint %subgroup %a",
            |_| alloc::vec![u32::MAX; 4],
            |_| alloc::vec![word(WIDTH - 1)]
        ),
        every(
            &[v4],
            U32,
            "%r = OpGroupNonUniformBallotFindLSB %uint %subgroup %a",
            |_| alloc::vec![0, 1, 0, 0],
            |_| alloc::vec![u32::MAX]
        ),
        every(
            &[v4, U32],
            Bool,
            "%r = OpGroupNonUniformBallotBitExtract %bool %subgroup %a %b",
            |l| alloc::vec![u32::MAX, u32::MAX, 0, 0, word(WIDTH + l % WIDTH)],
            |_| alloc::vec![0]
        ),
    ]
}

/// A fold over each subgroup of `value(lane)`: all of it, the scan up to and
/// including each lane, and the scan before it.
fn fold(
    op: &str,
    ty: Ty,
    value: impl Fn(usize) -> u32,
    step: impl Fn(u32, u32) -> u32,
    identity: u32,
) -> Vec<Case> {
    let name = ty.name();
    ["Reduce", "InclusiveScan", "ExclusiveScan"]
        .into_iter()
        .map(|scan| {
            let body = alloc::format!("%r = OpGroupNonUniform{op} {name} %subgroup {scan} %a");
            every(
                &[ty],
                ty,
                &body,
                |l| alloc::vec![value(l)],
                |l| {
                    let lanes = subgroup(l);
                    let upto = match scan {
                        "Reduce" => lanes.end,
                        "InclusiveScan" => l + 1,
                        _ => l,
                    };
                    alloc::vec![(lanes.start..upto).fold(identity, |acc, k| step(acc, value(k)))]
                },
            )
        })
        .collect()
}

fn folds() -> Vec<Case> {
    use Ty::{Bool, F32, I32, U32};
    let small = |l: usize| f32::from(u8::try_from(l % 7 + 1).expect("below eight"));
    let signed = |l: usize| word(l).wrapping_mul(0x9e37_79b9);
    let mut cases = Vec::new();
    cases.extend(fold("IAdd", U32, word, u32::wrapping_add, 0));
    cases.extend(fold(
        "IMul",
        U32,
        |l| if l % WIDTH == 5 { 3 } else { 1 },
        u32::wrapping_mul,
        1,
    ));
    cases.extend(fold(
        "SMin",
        I32,
        signed,
        |a, b| a.cast_signed().min(b.cast_signed()).cast_unsigned(),
        0x7fff_ffff,
    ));
    cases.extend(fold(
        "SMax",
        I32,
        signed,
        |a, b| a.cast_signed().max(b.cast_signed()).cast_unsigned(),
        0x8000_0000,
    ));
    cases.extend(fold("UMin", U32, signed, u32::min, u32::MAX));
    cases.extend(fold("UMax", U32, signed, u32::max, 0));
    cases.extend(fold("BitwiseAnd", U32, signed, |a, b| a & b, u32::MAX));
    cases.extend(fold("BitwiseOr", U32, signed, |a, b| a | b, 0));
    cases.extend(fold("BitwiseXor", U32, signed, |a, b| a ^ b, 0));
    cases.extend(fold(
        "FAdd",
        F32,
        |l| f(small(l)),
        |a, b| f(f32::from_bits(a) + f32::from_bits(b)),
        f(0.0),
    ));
    cases.extend(fold(
        "FMul",
        F32,
        |l| f(if l % 3 == 0 { 2.0 } else { 1.0 }),
        |a, b| f(f32::from_bits(a) * f32::from_bits(b)),
        f(1.0),
    ));
    let low = |a: u32, b: u32| {
        if f32::from_bits(b) < f32::from_bits(a) {
            b
        } else {
            a
        }
    };
    let high = |a: u32, b: u32| {
        if f32::from_bits(a) < f32::from_bits(b) {
            b
        } else {
            a
        }
    };
    cases.extend(fold("FMin", F32, |l| f(small(l)), low, f(f32::INFINITY)));
    cases.extend(fold(
        "FMax",
        F32,
        |l| f(small(l)),
        high,
        f(f32::NEG_INFINITY),
    ));
    cases.extend(fold(
        "LogicalAnd",
        Bool,
        |l| u32::from(l % WIDTH != 9),
        |a, b| a & b,
        1,
    ));
    cases.extend(fold(
        "LogicalOr",
        Bool,
        |l| u32::from(l % WIDTH == 9),
        |a, b| a | b,
        0,
    ));
    cases.extend(fold(
        "LogicalXor",
        Bool,
        |l| u32::from(l % 3 == 0),
        |a, b| a ^ b,
        0,
    ));
    cases
}

/// The subgroup built-ins, read from their variables.
const BUILTINS: &str = r"%uint_in = OpTypePointer Input %uint
%v4uint_in = OpTypePointer Input %v4uint
%size_var = OpVariable %uint_in Input
%local_var = OpVariable %uint_in Input
%count_var = OpVariable %uint_in Input
%id_var = OpVariable %uint_in Input
%lt_var = OpVariable %v4uint_in Input
%subgroup = OpConstant %uint 3";

const BUILTIN_DECORATIONS: &str = "OpDecorate %size_var BuiltIn SubgroupSize\nOpDecorate %local_var BuiltIn SubgroupLocalInvocationId\nOpDecorate %count_var BuiltIn NumSubgroups\nOpDecorate %id_var BuiltIn SubgroupId\nOpDecorate %lt_var BuiltIn SubgroupLtMask";

fn reconvergence() -> Vec<Case> {
    use Ty::U32;
    let v4 = Ty::V(Scalar::U32, 4);
    let builtins = (0..LANES).fold(
        Case::new(
            &[U32],
            v4,
            "%s = OpLoad %uint %size_var\n%l = OpLoad %uint %local_var\n%n = OpLoad %uint %count_var\n%i = OpLoad %uint %id_var\n%lt = OpLoad %v4uint %lt_var\n%lt0 = OpCompositeExtract %uint %lt 0\n%li = OpIAdd %uint %l %lt0\n%r = OpCompositeConstruct %v4uint %s %li %n %i",
        )
        .caps(CAPS)
        .decls(BUILTINS)
        .annotate(BUILTIN_DECORATIONS)
        .interface("%size_var %local_var %count_var %id_var %lt_var"),
        |c, lane| {
            let within = lane % WIDTH;
            let below = if within == 0 { 0 } else { (1u32 << within) - 1 };
            c.lane(&[0], &[32, word(within).wrapping_add(below), 2, word(lane / WIDTH)])
        },
    );
    // Each lane loops `lane % 4` times, counting the lanes with it at each
    // iteration: none laps another, so iteration `i` holds exactly the lanes
    // with more than `i` iterations.
    let lapping = (0..LANES).fold(
        Case::new(
            &[U32],
            U32,
            "OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %zero %entry %ni %cont\n%seen = OpPhi %uint %zero %entry %nseen %cont\nOpLoopMerge %exit %cont None\nOpBranch %body\n%body = OpLabel\n%more = OpULessThan %bool %i %a\nOpBranchConditional %more %step %exit\n%step = OpLabel\n%together = OpGroupNonUniformBallot %v4uint %subgroup %true\n%count = OpGroupNonUniformBallotBitCount %uint %subgroup Reduce %together\n%nseen = OpIAdd %uint %seen %count\nOpBranch %cont\n%cont = OpLabel\n%ni = OpIAdd %uint %i %one\nOpBranch %head\n%exit = OpLabel\n%r = OpCopyObject %uint %seen",
        )
        .caps(CAPS)
        .decls("%subgroup = OpConstant %uint 3\n%true = OpConstantTrue %bool"),
        |c, lane| {
            let trips = lane % 4;
            let seen: usize = (0..trips).map(|i| (0..WIDTH).filter(|k| k % 4 > i).count()).sum();
            c.lane(&[word(trips)], &[word(seen)])
        },
    );
    // Lanes returning early from a call's loop wait at its end: past the
    // call every lane is together again.
    let early = (0..LANES).fold(
        Case::new(
            &[U32],
            U32,
            "%ignored = OpFunctionCall %uint %leave %a\n%all = OpGroupNonUniformBallot %v4uint %subgroup %true\n%r = OpGroupNonUniformBallotBitCount %uint %subgroup Reduce %all",
        )
        .caps(CAPS)
        .decls(LEAVE),
        |c, lane| c.lane(&[word(lane % 5)], &[32]),
    );
    alloc::vec![builtins, lapping, early]
}

/// A function looping until its count, returning from inside the loop.
const LEAVE: &str = r"%subgroup = OpConstant %uint 3
%true = OpConstantTrue %bool
%leave_fn = OpTypeFunction %uint %uint
%leave = OpFunction %uint None %leave_fn
%to = OpFunctionParameter %uint
%leave_entry = OpLabel
OpBranch %leave_head
%leave_head = OpLabel
%j = OpPhi %uint %zero %leave_entry %nj %leave_cont
OpLoopMerge %leave_merge %leave_cont None
OpBranch %leave_body
%leave_body = OpLabel
%reached = OpUGreaterThanEqual %bool %j %to
OpSelectionMerge %leave_go None
OpBranchConditional %reached %leave_out %leave_go
%leave_out = OpLabel
OpReturnValue %j
%leave_go = OpLabel
OpBranch %leave_cont
%leave_cont = OpLabel
%nj = OpIAdd %uint %j %one
OpBranch %leave_head
%leave_merge = OpLabel
OpReturnValue %zero
OpFunctionEnd";

/// The shaders this family runs beside its cases.
pub(super) fn bodies() -> Vec<String> {
    Vec::new()
}

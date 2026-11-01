//! Arithmetic, bitwise, shift, comparison, logical and selection
//! instructions, the operations with two results, bit fields, and vector and
//! matrix products.

use alloc::vec::Vec;

use super::{d, f, Case, Scalar, Ty};

const NAN: u32 = 0x7fc0_0000;

pub(super) fn cases() -> Vec<Case> {
    let mut cases = integer();
    cases.extend(float());
    cases.extend(comparisons());
    cases.extend(logical());
    cases.extend(wide_and_narrow());
    cases.extend(fields());
    cases.extend(products());
    cases
}

fn integer() -> Vec<Case> {
    use Ty::{I32, U32};
    let two = &[U32, U32];
    let signed = &[I32, I32];
    alloc::vec![
        Case::new(two, U32, "%r = OpIAdd %uint %a %b")
            .lane(&[3, 4], &[7])
            .lane(&[u32::MAX, 1], &[0]),
        Case::new(two, U32, "%r = OpISub %uint %a %b").lane(&[3, 5], &[0xffff_fffe]),
        Case::new(two, U32, "%r = OpIMul %uint %a %b")
            .lane(&[7, 6], &[42])
            .lane(&[0x1_0000, 0x1_0000], &[0]),
        Case::new(signed, I32, "%r = OpSDiv %int %a %b")
            .lane(&[7, (-2i32).cast_unsigned()], &[(-3i32).cast_unsigned()])
            .lane(&[0x8000_0000, u32::MAX], &[0x8000_0000])
            .lane(&[5, 0], &[0]),
        Case::new(two, U32, "%r = OpUDiv %uint %a %b")
            .lane(&[7, 2], &[3])
            .lane(&[7, 0], &[0]),
        Case::new(signed, I32, "%r = OpSRem %int %a %b")
            .lane(&[7, (-2i32).cast_unsigned()], &[1])
            .lane(&[(-7i32).cast_unsigned(), 2], &[u32::MAX])
            .lane(&[3, 0], &[0]),
        Case::new(signed, I32, "%r = OpSMod %int %a %b")
            .lane(&[(-7i32).cast_unsigned(), 2], &[1])
            .lane(&[7, (-2i32).cast_unsigned()], &[u32::MAX])
            .lane(&[3, 0], &[0]),
        Case::new(two, U32, "%r = OpUMod %uint %a %b")
            .lane(&[7, 3], &[1])
            .lane(&[7, 0], &[0]),
        Case::new(two, U32, "%r = OpBitwiseAnd %uint %a %b").lane(&[0xf0f0, 0xff00], &[0xf000]),
        Case::new(two, U32, "%r = OpBitwiseOr %uint %a %b").lane(&[0xf0f0, 0xff00], &[0xfff0]),
        Case::new(two, U32, "%r = OpBitwiseXor %uint %a %b").lane(&[0xf0f0, 0xff00], &[0x0ff0]),
        Case::new(&[I32], I32, "%r = OpSNegate %int %a")
            .lane(&[5], &[(-5i32).cast_unsigned()])
            .lane(&[0x8000_0000], &[0x8000_0000]),
        Case::new(&[U32], U32, "%r = OpNot %uint %a").lane(&[0], &[u32::MAX]),
        Case::new(two, U32, "%r = OpShiftLeftLogical %uint %a %b")
            .lane(&[1, 31], &[0x8000_0000])
            .lane(&[1, 32], &[0]),
        Case::new(two, U32, "%r = OpShiftRightLogical %uint %a %b")
            .lane(&[0x8000_0000, 31], &[1])
            .lane(&[0x8000_0000, 99], &[0]),
        Case::new(&[I32, U32], I32, "%r = OpShiftRightArithmetic %int %a %b")
            .lane(&[0x8000_0000, 31], &[u32::MAX])
            .lane(&[0x8000_0000, 40], &[0]),
    ]
}

fn float() -> Vec<Case> {
    use Ty::F32;
    let two = &[F32, F32];
    alloc::vec![
        Case::new(two, F32, "%r = OpFAdd %float %a %b").lane(&[f(1.5), f(2.25)], &[f(3.75)]),
        Case::new(two, F32, "%r = OpFSub %float %a %b").lane(&[f(1.0), f(0.5)], &[f(0.5)]),
        Case::new(two, F32, "%r = OpFMul %float %a %b").lane(&[f(3.0), f(0.5)], &[f(1.5)]),
        Case::new(two, F32, "%r = OpFDiv %float %a %b")
            .lane(&[f(1.0), f(4.0)], &[f(0.25)])
            .lane(&[f(1.0), f(0.0)], &[f(f32::INFINITY)]),
        Case::new(two, F32, "%r = OpFRem %float %a %b")
            .lane(&[f(5.5), f(2.0)], &[f(1.5)])
            .lane(&[f(-5.5), f(2.0)], &[f(-1.5)]),
        Case::new(two, F32, "%r = OpFMod %float %a %b")
            .lane(&[f(-5.5), f(2.0)], &[f(0.5)])
            .lane(&[f(5.5), f(-2.0)], &[f(-0.5)]),
        Case::new(&[F32], F32, "%r = OpFNegate %float %a").lane(&[f(1.0)], &[f(-1.0)]),
        Case::new(&[F32], Ty::Bool, "%r = OpIsNan %bool %a")
            .lane(&[NAN], &[1])
            .lane(&[f(1.0)], &[0]),
        Case::new(&[Ty::F64], Ty::Bool, "%r = OpIsInf %bool %a")
            .lane(&d(f64::INFINITY), &[1])
            .lane(&d(2.0), &[0]),
        Case::new(&[F32], F32, "%r = OpQuantizeToF16 %float %a")
            .lane(&[f(1e-6)], &[0])
            .lane(&[f(70000.0)], &[f(f32::INFINITY)]),
    ]
}

#[expect(clippy::too_many_lines, reason = "one case per comparison instruction")]
fn comparisons() -> Vec<Case> {
    use Ty::{Bool, F32, I32, U32};
    let ints = &[U32, U32];
    let signed = &[I32, I32];
    let floats = &[F32, F32];
    let int_case = |body, lanes: &[([u32; 2], u32)]| {
        lanes
            .iter()
            .fold(Case::new(ints, Bool, body), |c, (i, o)| c.lane(i, &[*o]))
    };
    let signed_case = |body, lanes: &[([u32; 2], u32)]| {
        lanes
            .iter()
            .fold(Case::new(signed, Bool, body), |c, (i, o)| c.lane(i, &[*o]))
    };
    let float_case = |body, lanes: &[([u32; 2], u32)]| {
        lanes
            .iter()
            .fold(Case::new(floats, Bool, body), |c, (i, o)| c.lane(i, &[*o]))
    };
    let m1 = u32::MAX;
    let (one, two) = (f(1.0), f(2.0));
    alloc::vec![
        int_case("%r = OpIEqual %bool %a %b", &[([3, 3], 1), ([3, 4], 0)]),
        int_case("%r = OpINotEqual %bool %a %b", &[([3, 4], 1), ([3, 3], 0)]),
        int_case(
            "%r = OpUGreaterThan %bool %a %b",
            &[([m1, 1], 1), ([1, 1], 0)]
        ),
        int_case(
            "%r = OpUGreaterThanEqual %bool %a %b",
            &[([1, 1], 1), ([0, 1], 0)]
        ),
        int_case(
            "%r = OpULessThan %bool %a %b",
            &[([1, m1], 1), ([m1, 1], 0)]
        ),
        int_case(
            "%r = OpULessThanEqual %bool %a %b",
            &[([1, 1], 1), ([2, 1], 0)]
        ),
        signed_case(
            "%r = OpSGreaterThan %bool %a %b",
            &[([m1, 1], 0), ([1, m1], 1)]
        ),
        signed_case(
            "%r = OpSGreaterThanEqual %bool %a %b",
            &[([m1, m1], 1), ([m1, 0], 0)]
        ),
        signed_case(
            "%r = OpSLessThan %bool %a %b",
            &[([m1, 1], 1), ([1, m1], 0)]
        ),
        signed_case(
            "%r = OpSLessThanEqual %bool %a %b",
            &[([m1, m1], 1), ([0, m1], 0)]
        ),
        float_case(
            "%r = OpFOrdEqual %bool %a %b",
            &[([one, one], 1), ([NAN, NAN], 0)]
        ),
        float_case(
            "%r = OpFUnordEqual %bool %a %b",
            &[([NAN, one], 1), ([one, two], 0)]
        ),
        float_case(
            "%r = OpFOrdNotEqual %bool %a %b",
            &[([one, two], 1), ([NAN, one], 0)]
        ),
        float_case(
            "%r = OpFUnordNotEqual %bool %a %b",
            &[([NAN, one], 1), ([one, one], 0)]
        ),
        float_case(
            "%r = OpFOrdLessThan %bool %a %b",
            &[([one, two], 1), ([NAN, two], 0)]
        ),
        float_case(
            "%r = OpFUnordLessThan %bool %a %b",
            &[([NAN, two], 1), ([two, one], 0)]
        ),
        float_case(
            "%r = OpFOrdGreaterThan %bool %a %b",
            &[([two, one], 1), ([NAN, one], 0)]
        ),
        float_case(
            "%r = OpFUnordGreaterThan %bool %a %b",
            &[([NAN, one], 1), ([one, two], 0)]
        ),
        float_case(
            "%r = OpFOrdLessThanEqual %bool %a %b",
            &[([one, one], 1), ([two, NAN], 0)]
        ),
        float_case(
            "%r = OpFUnordLessThanEqual %bool %a %b",
            &[([two, NAN], 1), ([two, one], 0)]
        ),
        float_case(
            "%r = OpFOrdGreaterThanEqual %bool %a %b",
            &[([one, one], 1), ([NAN, one], 0)]
        ),
        float_case(
            "%r = OpFUnordGreaterThanEqual %bool %a %b",
            &[([NAN, one], 1), ([one, two], 0)]
        ),
    ]
}

fn logical() -> Vec<Case> {
    use Ty::{Bool, U32};
    let two = &[Bool, Bool];
    let table = |body| {
        [(0, 0), (0, 1), (1, 0), (1, 1)]
            .iter()
            .fold(Case::new(two, Bool, body), |c, &(a, b)| {
                c.lane(&[a, b], &[0])
            })
    };
    let with = |body, expected: [u32; 4]| {
        let mut case = table(body);
        for (lane, &e) in case.lanes.iter_mut().zip(&expected) {
            lane.1 = alloc::vec![e];
        }
        case
    };
    let v4 = Ty::V(Scalar::Bool, 4);
    alloc::vec![
        with("%r = OpLogicalEqual %bool %a %b", [1, 0, 0, 1]),
        with("%r = OpLogicalNotEqual %bool %a %b", [0, 1, 1, 0]),
        with("%r = OpLogicalOr %bool %a %b", [0, 1, 1, 1]),
        with("%r = OpLogicalAnd %bool %a %b", [0, 0, 0, 1]),
        Case::new(&[Bool], Bool, "%r = OpLogicalNot %bool %a")
            .lane(&[1], &[0])
            .lane(&[0], &[1]),
        Case::new(&[v4], Bool, "%r = OpAny %bool %a")
            .lane(&[0, 0, 1, 0], &[1])
            .lane(&[0; 4], &[0]),
        Case::new(&[v4], Bool, "%r = OpAll %bool %a")
            .lane(&[1; 4], &[1])
            .lane(&[1, 1, 0, 1], &[0]),
        Case::new(&[Bool, U32, U32], U32, "%r = OpSelect %uint %a %b %c")
            .lane(&[1, 5, 6], &[5])
            .lane(&[0, 5, 6], &[6]),
        Case::new(
            &[
                Ty::V(Scalar::Bool, 2),
                Ty::V(Scalar::U32, 2),
                Ty::V(Scalar::U32, 2)
            ],
            Ty::V(Scalar::U32, 2),
            "%r = OpSelect %v2uint %a %b %c",
        )
        .lane(&[1, 0, 10, 11, 20, 21], &[10, 21]),
    ]
}

fn wide_and_narrow() -> Vec<Case> {
    use Ty::{F16, F64, I64, U16, U64};
    alloc::vec![
        Case::new(&[U64, U64], U64, "%r = OpIAdd %ulong %a %b")
            .lane(&[u32::MAX, u32::MAX, 1, 0], &[0, 0])
            .lane(&[u32::MAX, 0, 1, 0], &[0, 1]),
        Case::new(&[I64, I64], I64, "%r = OpIMul %long %a %b")
            .lane(&[0x1_0000, 0, 0x1_0000, 0], &[0, 1])
            .lane(&[u32::MAX, u32::MAX, 2, 0], &[0xffff_fffe, u32::MAX]),
        Case::new(&[U16, U16], U16, "%r = OpIAdd %ushort %a %b").lane(&[0xffff, 2], &[1]),
        Case::new(&[F16, F16], F16, "%r = OpFAdd %half %a %b").lane(&[0x3c00, 0x3c00], &[0x4000]),
        Case::new(&[F64, F64], F64, "%r = OpFAdd %double %a %b").lane(
            &[d(0.1)[0], d(0.1)[1], d(0.2)[0], d(0.2)[1]],
            &d(0.300_000_000_000_000_04),
        ),
    ]
}

fn fields() -> Vec<Case> {
    use Ty::{I32, U32};
    let pair = Ty::Pair("pair", Scalar::U32, 1);
    let ipair = Ty::Pair("ipair", Scalar::I32, 1);
    let decls = "%pair = OpTypeStruct %uint %uint";
    let idecls = "%ipair = OpTypeStruct %int %int";
    alloc::vec![
        Case::new(&[U32, U32], pair, "%r = OpIAddCarry %pair %a %b")
            .decls(decls)
            .lane(&[u32::MAX, 2], &[1, 1])
            .lane(&[1, 2], &[3, 0]),
        Case::new(&[U32, U32], pair, "%r = OpISubBorrow %pair %a %b")
            .decls(decls)
            .lane(&[1, 2], &[u32::MAX, 1])
            .lane(&[3, 2], &[1, 0]),
        Case::new(&[U32, U32], pair, "%r = OpUMulExtended %pair %a %b")
            .decls(decls)
            .lane(&[0x1_0000, 0x1_0000], &[0, 1]),
        Case::new(&[I32, I32], ipair, "%r = OpSMulExtended %ipair %a %b")
            .decls(idecls)
            .lane(&[u32::MAX, 2], &[0xffff_fffe, u32::MAX]),
        Case::new(
            &[U32, U32, U32, U32],
            U32,
            "%r = OpBitFieldInsert %uint %a %b %c %d"
        )
        .lane(&[0xffff_0000, 0xab, 4, 8], &[0xffff_0ab0])
        .lane(&[0x1234, 0xff, 30, 8], &[0xc000_1234]),
        Case::new(
            &[I32, U32, U32],
            I32,
            "%r = OpBitFieldSExtract %int %a %b %c"
        )
        .lane(&[0xf000, 12, 4], &[u32::MAX])
        .lane(&[0x7000, 12, 4], &[7]),
        Case::new(
            &[U32, U32, U32],
            U32,
            "%r = OpBitFieldUExtract %uint %a %b %c"
        )
        .lane(&[0xf000, 12, 4], &[0xf])
        .lane(&[0xf000, 12, 0], &[0]),
        Case::new(&[U32], U32, "%r = OpBitReverse %uint %a").lane(&[1], &[0x8000_0000]),
        Case::new(&[U32], U32, "%r = OpBitCount %uint %a").lane(&[0xf0f0], &[8]),
    ]
}

fn products() -> Vec<Case> {
    let v2 = Ty::V(Scalar::F32, 2);
    let v4 = Ty::V(Scalar::F32, 4);
    let matrix = "%m2 = OpTypeMatrix %v2float 2";
    // A matrix of columns `%a` and `%b`, and one of a result's columns spread
    // as a vector of four.
    let flatten = "%r0 = OpCompositeExtract %v2float %rm 0\n%r1 = OpCompositeExtract %v2float %rm 1\n%r = OpCompositeConstruct %v4float %r0 %r1";
    let product = |body: &'static str| Case::new(&[v2, v2, v2], v2, body).decls(matrix);
    let columns = [f(1.0), f(2.0), f(3.0), f(4.0)];
    let with = |mut case: Case, extra: [u32; 2], out: [u32; 2]| {
        let mut input = columns.to_vec();
        input.extend_from_slice(&extra);
        case = case.lane(&input, &out);
        case
    };
    alloc::vec![
        Case::new(&[v2, v2], Ty::F32, "%r = OpDot %float %a %b")
            .lane(&[f(1.0), f(2.0), f(3.0), f(4.0)], &[f(11.0)]),
        Case::new(
            &[v2, Ty::F32],
            v2,
            "%r = OpVectorTimesScalar %v2float %a %b"
        )
        .lane(&[f(1.0), f(2.0), f(3.0)], &[f(3.0), f(6.0)]),
        // [1 3; 2 4] * (5, 6) = (23, 34).
        with(
            product("%m = OpCompositeConstruct %m2 %a %b\n%r = OpMatrixTimesVector %v2float %m %c"),
            [f(5.0), f(6.0)],
            [f(23.0), f(34.0)],
        ),
        // (5, 6) * [1 3; 2 4] = (17, 39).
        with(
            product("%m = OpCompositeConstruct %m2 %a %b\n%r = OpVectorTimesMatrix %v2float %c %m"),
            [f(5.0), f(6.0)],
            [f(17.0), f(39.0)],
        ),
        Case::new(
            &[v2, v2],
            v4,
            "%m = OpCompositeConstruct %m2 %a %b\n%rm = OpMatrixTimesScalar %m2 %m %two"
        )
        .decls("%m2 = OpTypeMatrix %v2float 2\n%two = OpConstant %float 2.0")
        .lane(&columns, &[f(2.0), f(4.0), f(6.0), f(8.0)]),
        // [1 3; 2 4] squared = [7 15; 10 22].
        Case::new(
            &[v2, v2],
            v4,
            "%m = OpCompositeConstruct %m2 %a %b\n%rm = OpMatrixTimesMatrix %m2 %m %m"
        )
        .decls(matrix)
        .lane(&columns, &[f(7.0), f(10.0), f(15.0), f(22.0)]),
        Case::new(&[v2, v2], v4, "%rm = OpOuterProduct %m2 %a %b")
            .decls(matrix)
            .lane(&columns, &[f(3.0), f(6.0), f(4.0), f(8.0)]),
        Case::new(
            &[v2, v2],
            v4,
            "%m = OpCompositeConstruct %m2 %a %b\n%rm = OpTranspose %m2 %m"
        )
        .decls(matrix)
        .lane(&columns, &[f(1.0), f(3.0), f(2.0), f(4.0)]),
    ]
    .into_iter()
    .map(|mut case| {
        if case.output == v4 {
            case.body.push('\n');
            case.body.push_str(flatten);
        }
        case
    })
    .collect()
}

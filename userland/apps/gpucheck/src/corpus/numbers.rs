//! The corpus's numbers: integer and float arithmetic, comparisons,
//! conversions and bit fields, the extended arithmetic, the matrices, the
//! packings, the widths past 32 bits and short of them, and every
//! `GLSL.std.450` instruction, each held to the precision Vulkan states for
//! it.

use tairix_spirv::enumerant::Capability;
use tairix_spirv::{BuildError, Function, GlslStd450, Id, Op};

use super::{local, member, op, rows, Family, Held, Io};

pub(super) const FAMILIES: &[Family] = &[
    Family::compute("the corpus's integer arithmetic", &[], integer_arithmetic),
    Family::compute("the corpus's integer comparisons", &[], integer_comparisons),
    Family::compute("the corpus's float arithmetic", &[], float_arithmetic),
    Family::compute("the corpus's float functions", &[], float_functions),
    Family::compute("the corpus's transcendentals", &[], transcendentals),
    Family::compute("the corpus's geometry", &[], geometry),
    Family::compute("the corpus's matrices", &[], matrices),
    Family::compute("the corpus's packings", &[], packings),
    Family::compute(
        "the corpus's wide numbers",
        &[Capability::INT64, Capability::FLOAT64],
        wide,
    ),
    Family::compute(
        "the corpus's narrow numbers",
        &[Capability::INT16, Capability::FLOAT16],
        narrow,
    ),
];

/// The values packed as normalised integers, four and two to a word: none
/// scales to a tie, which rounds in a direction the device chooses.
pub(super) const SNORM_4X8: [f32; 4] = [0.25, -0.75, 1.0, -1.0];
pub(super) const UNORM_4X8: [f32; 4] = [0.2, 0.4, 0.6, 1.0];
pub(super) const SNORM_2X16: [f32; 2] = [0.2, -0.6];
pub(super) const UNORM_2X16: [f32; 2] = [0.2, 0.8];

/// Two halves unpacked, 1.5 and -1.0: normal numbers, whose bits every
/// device unpacks alike.
pub(super) const HALVES: u32 = 0xbc00_3e00;

/// A word converted to a single: within a single's precision, so no
/// rounding mode tells devices apart.
pub(super) const CONVERTED: u32 = 0x9e37_7900;

/// One `GLSL.std.450` instruction a family writes the answer of: its case,
/// how it is held, the instruction, its result's type, and its operands.
type GlslRow<'a> = (&'static str, Held, GlslStd450, Id, &'a [Id]);

/// Each of `glsl_rows`, written in turn.
fn glsl(f: &mut Function<'_>, io: &mut Io, glsl_rows: &[GlslRow<'_>]) -> Result<(), BuildError> {
    for &(case, held, inst, ty, operands) in glsl_rows {
        let value = f.glsl(inst, ty, operands)?;
        io.out(f, (case, held), (value, ty))?;
    }
    Ok(())
}

/// Integer arithmetic, shifts and bit fields, and the extended arithmetic.
fn integer_arithmetic(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let (uint, int) = (io.kit.uint, io.kit.int);
    let (word, small) = (io.word(f, 0x9e37_79b9)?, io.word(f, 7)?);
    let (signed, divisor) = (io.int(f, -1_234_567)?, io.int(f, 89)?);
    let (amount, offset, count) = (io.word(f, 5)?, io.word(f, 4)?, io.word(f, 12)?);
    let (both, divided) = ([word, small], [signed, divisor]);
    rows(
        f,
        io,
        Held::Exact,
        &[
            ("IAdd", Op::I_ADD, uint, &both),
            ("ISub", Op::I_SUB, uint, &both),
            ("IMul", Op::I_MUL, uint, &both),
            ("UDiv", Op::U_DIV, uint, &both),
            ("UMod", Op::U_MOD, uint, &both),
            ("BitwiseOr", Op::BITWISE_OR, uint, &both),
            ("BitwiseXor", Op::BITWISE_XOR, uint, &both),
            ("BitwiseAnd", Op::BITWISE_AND, uint, &both),
            ("SDiv", Op::S_DIV, int, &divided),
            ("SRem", Op::S_REM, int, &divided),
            ("SMod", Op::S_MOD, int, &divided),
            ("SNegate", Op::S_NEGATE, int, &[signed]),
            ("Not", Op::NOT, uint, &[word]),
            (
                "ShiftRightLogical",
                Op::SHIFT_RIGHT_LOGICAL,
                uint,
                &[word, amount],
            ),
            (
                "ShiftRightArithmetic",
                Op::SHIFT_RIGHT_ARITHMETIC,
                int,
                &[signed, amount],
            ),
            (
                "ShiftLeftLogical",
                Op::SHIFT_LEFT_LOGICAL,
                uint,
                &[word, amount],
            ),
            (
                "BitFieldInsert",
                Op::BIT_FIELD_INSERT,
                uint,
                &[word, small, offset, count],
            ),
            (
                "BitFieldSExtract",
                Op::BIT_FIELD_S_EXTRACT,
                int,
                &[signed, offset, count],
            ),
            (
                "BitFieldUExtract",
                Op::BIT_FIELD_U_EXTRACT,
                uint,
                &[word, offset, count],
            ),
            ("BitReverse", Op::BIT_REVERSE, uint, &[word]),
            ("BitCount", Op::BIT_COUNT, uint, &[word]),
        ],
    )?;
    let pair = f.module().structure(&[uint, uint])?;
    let signed_pair = f.module().structure(&[int, int])?;
    for (case, code, (ty, half), operands) in [
        ("IAddCarry", Op::I_ADD_CARRY, (pair, uint), [word, word]),
        ("ISubBorrow", Op::I_SUB_BORROW, (pair, uint), [small, word]),
        (
            "UMulExtended",
            Op::U_MUL_EXTENDED,
            (pair, uint),
            [word, word],
        ),
        (
            "SMulExtended",
            Op::S_MUL_EXTENDED,
            (signed_pair, int),
            [signed, signed],
        ),
    ] {
        let answered = f.op(code, ty, &operands)?;
        for index in 0..2 {
            let part = member(f, answered, half, index)?;
            io.out(f, (case, Held::Exact), (part, half))?;
        }
    }
    Ok(())
}

/// Integer comparisons, the logical instructions, a selection, and the
/// integer `GLSL.std.450` instructions.
fn integer_comparisons(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let (uint, int, boolean) = (io.kit.uint, io.kit.int, io.kit.boolean);
    let (word, small) = (io.word(f, 0x9e37_79b9)?, io.word(f, 7)?);
    let (signed, other) = (io.int(f, -1_234_567)?, io.int(f, 89)?);
    let (truth, lie) = (io.boolean(f, true)?, io.boolean(f, false)?);
    let some = io.booleans(f, &[false, true, false])?;
    let (both, signs, truths) = ([word, small], [signed, other], [truth, lie]);
    rows(
        f,
        io,
        Held::Exact,
        &[
            ("IEqual", Op::I_EQUAL, boolean, &both),
            ("INotEqual", Op::I_NOT_EQUAL, boolean, &both),
            ("UGreaterThan", Op::U_GREATER_THAN, boolean, &both),
            (
                "UGreaterThanEqual",
                Op::U_GREATER_THAN_EQUAL,
                boolean,
                &both,
            ),
            ("ULessThan", Op::U_LESS_THAN, boolean, &both),
            ("ULessThanEqual", Op::U_LESS_THAN_EQUAL, boolean, &both),
            ("SGreaterThan", Op::S_GREATER_THAN, boolean, &signs),
            (
                "SGreaterThanEqual",
                Op::S_GREATER_THAN_EQUAL,
                boolean,
                &signs,
            ),
            ("SLessThan", Op::S_LESS_THAN, boolean, &signs),
            ("SLessThanEqual", Op::S_LESS_THAN_EQUAL, boolean, &signs),
            ("LogicalEqual", Op::LOGICAL_EQUAL, boolean, &truths),
            ("LogicalNotEqual", Op::LOGICAL_NOT_EQUAL, boolean, &truths),
            ("LogicalOr", Op::LOGICAL_OR, boolean, &truths),
            ("LogicalAnd", Op::LOGICAL_AND, boolean, &truths),
            ("LogicalNot", Op::LOGICAL_NOT, boolean, &[truth]),
            ("Select", Op::SELECT, uint, &[lie, word, small]),
            ("Any", Op::ANY, boolean, &[some]),
            ("All", Op::ALL, boolean, &[some]),
        ],
    )?;
    let (low, high) = (io.word(f, 100)?, io.word(f, 1_000)?);
    let (least, most) = (io.int(f, -5)?, io.int(f, 5)?);
    let exact = Held::Exact;
    glsl(
        f,
        io,
        &[
            ("SAbs", exact, GlslStd450::S_ABS, int, &[signed]),
            ("SSign", exact, GlslStd450::S_SIGN, int, &[signed]),
            ("UMin", exact, GlslStd450::U_MIN, uint, &both),
            ("UMax", exact, GlslStd450::U_MAX, uint, &both),
            ("SMin", exact, GlslStd450::S_MIN, int, &signs),
            ("SMax", exact, GlslStd450::S_MAX, int, &signs),
            (
                "UClamp",
                exact,
                GlslStd450::U_CLAMP,
                uint,
                &[word, low, high],
            ),
            (
                "SClamp",
                exact,
                GlslStd450::S_CLAMP,
                int,
                &[signed, least, most],
            ),
            ("FindILsb", exact, GlslStd450::FIND_I_LSB, uint, &[word]),
            ("FindSMsb", exact, GlslStd450::FIND_S_MSB, int, &[signed]),
            ("FindUMsb", exact, GlslStd450::FIND_U_MSB, uint, &[word]),
        ],
    )
}

/// Arithmetic, comparisons, classes and conversions of singles; a quotient
/// and the remainders to the bounds Vulkan states for them.
fn float_arithmetic(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let (float, boolean) = (kit.float, kit.boolean);
    let (real, negative) = (io.float(f, 1.75)?, io.float(f, -0.375)?);
    let vector = io.floats(f, &[0.5, -2.0, 4.25])?;
    let (above, below) = (io.float(f, 7.75)?, io.float(f, -7.75)?);
    let (word, signed) = (io.word(f, CONVERTED)?, io.int(f, -1_234_567)?);
    let both = [real, negative];
    rows(
        f,
        io,
        Held::Exact,
        &[
            ("FAdd", Op::F_ADD, float, &both),
            ("FSub", Op::F_SUB, float, &both),
            ("FMul", Op::F_MUL, float, &both),
            ("FNegate", Op::F_NEGATE, float, &[real]),
            (
                "VectorTimesScalar",
                Op::VECTOR_TIMES_SCALAR,
                kit.vec(3),
                &[vector, real],
            ),
            ("Dot", Op::DOT, float, &[vector, vector]),
            ("FOrdEqual", Op::F_ORD_EQUAL, boolean, &both),
            ("FUnordEqual", Op::F_UNORD_EQUAL, boolean, &both),
            ("FOrdNotEqual", Op::F_ORD_NOT_EQUAL, boolean, &both),
            ("FUnordNotEqual", Op::F_UNORD_NOT_EQUAL, boolean, &both),
            ("FOrdLessThan", Op::F_ORD_LESS_THAN, boolean, &both),
            ("FUnordLessThan", Op::F_UNORD_LESS_THAN, boolean, &both),
            ("FOrdGreaterThan", Op::F_ORD_GREATER_THAN, boolean, &both),
            (
                "FUnordGreaterThan",
                Op::F_UNORD_GREATER_THAN,
                boolean,
                &both,
            ),
            (
                "FOrdLessThanEqual",
                Op::F_ORD_LESS_THAN_EQUAL,
                boolean,
                &both,
            ),
            (
                "FUnordLessThanEqual",
                Op::F_UNORD_LESS_THAN_EQUAL,
                boolean,
                &both,
            ),
            (
                "FOrdGreaterThanEqual",
                Op::F_ORD_GREATER_THAN_EQUAL,
                boolean,
                &both,
            ),
            (
                "FUnordGreaterThanEqual",
                Op::F_UNORD_GREATER_THAN_EQUAL,
                boolean,
                &both,
            ),
            ("IsNan", Op::IS_NAN, boolean, &[real]),
            ("IsInf", Op::IS_INF, boolean, &[real]),
            ("ConvertFToU", Op::CONVERT_F_TO_U, kit.uint, &[above]),
            ("ConvertFToS", Op::CONVERT_F_TO_S, kit.int, &[below]),
            ("ConvertUToF", Op::CONVERT_U_TO_F, float, &[word]),
            ("ConvertSToF", Op::CONVERT_S_TO_F, float, &[signed]),
            ("QuantizeToF16", Op::QUANTIZE_TO_F16, float, &[real]),
            ("Bitcast", Op::BITCAST, kit.uint, &[negative]),
        ],
    )?;
    for (case, code, units) in [
        ("FDiv", Op::F_DIV, 3),
        ("FRem", Op::F_REM, 1),
        ("FMod", Op::F_MOD, 1),
    ] {
        op(f, io, (case, Held::Ulps(units)), (code, float, &both))?;
    }
    Ok(())
}

/// The `GLSL.std.450` instructions of singles defined exactly or inherited
/// from exact ones, and the forms answering through a variable or a
/// structure.
fn float_functions(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let (float, int) = (io.kit.float, io.kit.int);
    let (real, negative, three) = (io.float(f, 1.75)?, io.float(f, -0.375)?, io.float(f, 3.0)?);
    let (above, below) = (io.float(f, 7.75)?, io.float(f, -7.75)?);
    let (tie, edge, half) = (io.float(f, 2.5)?, io.float(f, 0.0)?, io.float(f, 0.5)?);
    let power = io.int(f, 3)?;
    let (exact, both) = (Held::Exact, [real, negative]);
    glsl(
        f,
        io,
        &[
            ("Round", exact, GlslStd450::ROUND, float, &[real]),
            ("RoundEven", exact, GlslStd450::ROUND_EVEN, float, &[tie]),
            ("Trunc", exact, GlslStd450::TRUNC, float, &[below]),
            ("FAbs", exact, GlslStd450::F_ABS, float, &[negative]),
            ("FSign", exact, GlslStd450::F_SIGN, float, &[negative]),
            ("Floor", exact, GlslStd450::FLOOR, float, &[below]),
            ("Ceil", exact, GlslStd450::CEIL, float, &[below]),
            ("Fract", exact, GlslStd450::FRACT, float, &[real]),
            (
                "Radians",
                Held::Ulps(2),
                GlslStd450::RADIANS,
                float,
                &[three],
            ),
            (
                "Degrees",
                Held::Ulps(2),
                GlslStd450::DEGREES,
                float,
                &[three],
            ),
            ("FMin", exact, GlslStd450::F_MIN, float, &both),
            ("FMax", exact, GlslStd450::F_MAX, float, &both),
            ("NMin", exact, GlslStd450::N_MIN, float, &both),
            ("NMax", exact, GlslStd450::N_MAX, float, &both),
            (
                "FClamp",
                exact,
                GlslStd450::F_CLAMP,
                float,
                &[three, negative, real],
            ),
            (
                "NClamp",
                exact,
                GlslStd450::N_CLAMP,
                float,
                &[three, negative, real],
            ),
            ("Step", exact, GlslStd450::STEP, float, &[edge, negative]),
            (
                "FMix",
                Held::Ulps(1),
                GlslStd450::F_MIX,
                float,
                &[real, three, half],
            ),
            (
                "SmoothStep",
                Held::Ulps(8),
                GlslStd450::SMOOTH_STEP,
                float,
                &[negative, three, real],
            ),
            (
                "Fma",
                exact,
                GlslStd450::FMA,
                float,
                &[real, negative, three],
            ),
            ("Ldexp", exact, GlslStd450::LDEXP, float, &[negative, power]),
        ],
    )?;
    for (case, inst, (of, kept)) in [
        ("Modf", GlslStd450::MODF, (below, float)),
        ("Frexp", GlslStd450::FREXP, (above, int)),
    ] {
        let place = local(f, kept)?;
        glsl(f, io, &[(case, exact, inst, float, &[of, place])])?;
        op(f, io, (case, exact), (Op::LOAD, kept, &[place]))?;
    }
    for (case, inst, (of, second)) in [
        ("ModfStruct", GlslStd450::MODF_STRUCT, (below, float)),
        ("FrexpStruct", GlslStd450::FREXP_STRUCT, (above, int)),
    ] {
        let ty = f.module().structure(&[float, second])?;
        let answered = f.glsl(inst, ty, &[of])?;
        for (index, part_ty) in (0..).zip([float, second]) {
            let part = member(f, answered, part_ty, index)?;
            io.out(f, (case, exact), (part, part_ty))?;
        }
    }
    Ok(())
}

/// The `GLSL.std.450` instructions Vulkan holds to an error rather than to
/// their exact answer, each at an argument within the range that error is
/// stated over.
fn transcendentals(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let float = io.kit.float;
    let (angle, ratio, two) = (io.float(f, 0.5)?, io.float(f, 0.25)?, io.float(f, 2.0)?);
    let (growth, near_one, three) = (io.float(f, 1.25)?, io.float(f, 1.75)?, io.float(f, 3.0)?);
    // Sine and cosine to an absolute error of 2^-11 over [-pi, pi]; what is
    // inherited from them, from the exponential or from the logarithm, to
    // the bounds those carry through the formula Vulkan inherits it from.
    glsl(
        f,
        io,
        &[
            ("Sin", Held::Within(11), GlslStd450::SIN, float, &[angle]),
            ("Cos", Held::Within(11), GlslStd450::COS, float, &[angle]),
            ("Tan", Held::Within(9), GlslStd450::TAN, float, &[angle]),
            ("Asin", Held::Ulps(4096), GlslStd450::ASIN, float, &[ratio]),
            ("Acos", Held::Ulps(4096), GlslStd450::ACOS, float, &[ratio]),
            ("Atan", Held::Ulps(4096), GlslStd450::ATAN, float, &[ratio]),
            ("Sinh", Held::Within(10), GlslStd450::SINH, float, &[angle]),
            ("Cosh", Held::Within(10), GlslStd450::COSH, float, &[angle]),
            ("Tanh", Held::Within(10), GlslStd450::TANH, float, &[angle]),
            (
                "Asinh",
                Held::Within(10),
                GlslStd450::ASINH,
                float,
                &[angle],
            ),
            ("Acosh", Held::Within(10), GlslStd450::ACOSH, float, &[two]),
            (
                "Atanh",
                Held::Within(10),
                GlslStd450::ATANH,
                float,
                &[ratio],
            ),
            ("Exp", Held::Ulps(6), GlslStd450::EXP, float, &[growth]),
            ("Exp2", Held::Ulps(6), GlslStd450::EXP2, float, &[growth]),
            ("Log", Held::Within(21), GlslStd450::LOG, float, &[near_one]),
            ("Log2", Held::Ulps(3), GlslStd450::LOG2, float, &[three]),
            ("Sqrt", Held::Ulps(6), GlslStd450::SQRT, float, &[two]),
            (
                "InverseSqrt",
                Held::Ulps(2),
                GlslStd450::INVERSE_SQRT,
                float,
                &[two],
            ),
            (
                "Atan2",
                Held::Ulps(4096),
                GlslStd450::ATAN2,
                float,
                &[ratio, two],
            ),
            (
                "Pow",
                Held::Ulps(64),
                GlslStd450::POW,
                float,
                &[two, growth],
            ),
        ],
    )
}

/// Lengths and distances, the cross product, normalisation, facing,
/// reflection and refraction, each to the bound Vulkan's formula for it
/// carries.
fn geometry(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let (float, triple) = (io.kit.float, io.kit.vec(3));
    let first = io.floats(f, &[1.0, -2.0, 2.0])?;
    let second = io.floats(f, &[0.5, 0.25, -1.0])?;
    let eta = io.float(f, 0.75)?;
    let incident = f.glsl(GlslStd450::NORMALIZE, triple, &[first])?;
    let normal = f.glsl(GlslStd450::NORMALIZE, triple, &[second])?;
    let both = [first, second];
    glsl(
        f,
        io,
        &[
            ("Length", Held::Ulps(8), GlslStd450::LENGTH, float, &[first]),
            (
                "Distance",
                Held::Ulps(8),
                GlslStd450::DISTANCE,
                float,
                &both,
            ),
            ("Cross", Held::Exact, GlslStd450::CROSS, triple, &both),
            (
                "Normalize",
                Held::Ulps(16),
                GlslStd450::NORMALIZE,
                triple,
                &[first],
            ),
            (
                "FaceForward",
                Held::Exact,
                GlslStd450::FACE_FORWARD,
                triple,
                &[first, second, first],
            ),
            ("Reflect", Held::Ulps(2), GlslStd450::REFLECT, triple, &both),
            (
                "Refract",
                Held::Within(16),
                GlslStd450::REFRACT,
                triple,
                &[incident, normal, eta],
            ),
        ],
    )
}

/// The matrix instructions, over small exact values whose every product and
/// sum is exact; an inverse, which divides, to the bound its division
/// carries.
fn matrices(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let (pair, triple) = (kit.vec(2), kit.vec(3));
    let square = f.module().matrix(triple, 3)?;
    let small = f.module().matrix(pair, 2)?;
    let tall = f.module().matrix(pair, 3)?;
    let wide = f.module().matrix(triple, 2)?;
    let columns = [
        io.floats(f, &[2.0, -1.0, 0.5])?,
        io.floats(f, &[0.0, 3.0, 1.0])?,
        io.floats(f, &[1.0, 0.25, -2.0])?,
    ];
    let [first, second, third] = columns;
    let matrix = f.op(Op::COMPOSITE_CONSTRUCT, square, &columns)?;
    let halves = [io.floats(f, &[4.0, 1.0])?, io.floats(f, &[2.0, 3.0])?];
    let invertible = f.op(Op::COMPOSITE_CONSTRUCT, small, &halves)?;
    let scale = io.float(f, -1.5)?;
    let exact = Held::Exact;
    let scaled = f.op(Op::MATRIX_TIMES_SCALAR, square, &[matrix, scale])?;
    out_matrix(f, io, ("MatrixTimesScalar", exact), (scaled, 3, triple))?;
    rows(
        f,
        io,
        exact,
        &[
            (
                "VectorTimesMatrix",
                Op::VECTOR_TIMES_MATRIX,
                triple,
                &[third, matrix],
            ),
            (
                "MatrixTimesVector",
                Op::MATRIX_TIMES_VECTOR,
                triple,
                &[matrix, second],
            ),
        ],
    )?;
    let product = f.op(Op::MATRIX_TIMES_MATRIX, square, &[matrix, matrix])?;
    out_matrix(f, io, ("MatrixTimesMatrix", exact), (product, 3, triple))?;
    let [left, _] = halves;
    let outer = f.op(Op::OUTER_PRODUCT, tall, &[left, first])?;
    out_matrix(f, io, ("OuterProduct", exact), (outer, 3, pair))?;
    let transposed = f.op(Op::TRANSPOSE, wide, &[outer])?;
    out_matrix(f, io, ("Transpose", exact), (transposed, 2, triple))?;
    glsl(
        f,
        io,
        &[(
            "Determinant",
            exact,
            GlslStd450::DETERMINANT,
            kit.float,
            &[matrix],
        )],
    )?;
    let inverse = f.glsl(GlslStd450::MATRIX_INVERSE, small, &[invertible])?;
    out_matrix(f, io, ("MatrixInverse", Held::Ulps(8)), (inverse, 2, pair))
}

/// The packings, at values no rounding direction tells apart, and their
/// unpackings, which divide; the halves unpacked are normal numbers.
fn packings(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let uint = io.kit.uint;
    let signed = io.floats(f, &SNORM_4X8)?;
    let unsigned = io.floats(f, &UNORM_4X8)?;
    let signed_pair = io.floats(f, &SNORM_2X16)?;
    let unsigned_pair = io.floats(f, &UNORM_2X16)?;
    let halves = io.floats(f, &[1.5, -0.25])?;
    let (word, halves_word) = (io.word(f, 0x8001_7fff)?, io.word(f, HALVES)?);
    let (exact, near) = (Held::Exact, Held::Ulps(4));
    let (pair, quad) = (io.kit.vec(2), io.kit.vec(4));
    glsl(
        f,
        io,
        &[
            (
                "PackSnorm4x8",
                exact,
                GlslStd450::PACK_SNORM4X8,
                uint,
                &[signed],
            ),
            (
                "PackUnorm4x8",
                exact,
                GlslStd450::PACK_UNORM4X8,
                uint,
                &[unsigned],
            ),
            (
                "PackSnorm2x16",
                exact,
                GlslStd450::PACK_SNORM2X16,
                uint,
                &[signed_pair],
            ),
            (
                "PackUnorm2x16",
                exact,
                GlslStd450::PACK_UNORM2X16,
                uint,
                &[unsigned_pair],
            ),
            (
                "PackHalf2x16",
                exact,
                GlslStd450::PACK_HALF2X16,
                uint,
                &[halves],
            ),
            (
                "UnpackSnorm2x16",
                near,
                GlslStd450::UNPACK_SNORM2X16,
                pair,
                &[word],
            ),
            (
                "UnpackUnorm2x16",
                near,
                GlslStd450::UNPACK_UNORM2X16,
                pair,
                &[word],
            ),
            (
                "UnpackHalf2x16",
                exact,
                GlslStd450::UNPACK_HALF2X16,
                pair,
                &[halves_word],
            ),
            (
                "UnpackSnorm4x8",
                near,
                GlslStd450::UNPACK_SNORM4X8,
                quad,
                &[word],
            ),
            (
                "UnpackUnorm4x8",
                near,
                GlslStd450::UNPACK_UNORM4X8,
                quad,
                &[word],
            ),
        ],
    )
}

/// Write each column of `matrix`, of `columns` columns of `column`.
fn out_matrix(
    f: &mut Function<'_>,
    io: &mut Io,
    (case, held): (&'static str, Held),
    (matrix, columns, column): (Id, u32, Id),
) -> Result<(), BuildError> {
    for index in 0..columns {
        let part = member(f, matrix, column, index)?;
        io.out(f, (case, held), (part, column))?;
    }
    Ok(())
}

/// The 64-bit words of `value`, written as `case`'s answer.
fn out_wide(
    f: &mut Function<'_>,
    io: &mut Io,
    case: &'static str,
    value: Id,
) -> Result<(), BuildError> {
    let pair = io.kit.uvec(2);
    let split = f.op(Op::BITCAST, pair, &[value])?;
    io.out(f, (case, Held::Exact), (split, pair))
}

/// The 64-bit widths: conversions to them, their arithmetic, and the
/// double's two words.
fn wide(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let module = f.module();
    let (ulong, long, double) = (
        module.int(64, false)?,
        module.int(64, true)?,
        module.float(64)?,
    );
    let (word, signed, real) = (
        io.word(f, 0x9e37_79b9)?,
        io.int(f, -1_234_567)?,
        io.float(f, 1.75)?,
    );
    let widened = f.op(Op::U_CONVERT, ulong, &[word])?;
    out_wide(f, io, "UConvert", widened)?;
    let summed = f.op(Op::I_ADD, ulong, &[widened, widened])?;
    out_wide(f, io, "IAdd", summed)?;
    let lengthened = f.op(Op::S_CONVERT, long, &[signed])?;
    out_wide(f, io, "SConvert", lengthened)?;
    let precise = f.op(Op::F_CONVERT, double, &[real])?;
    out_wide(f, io, "FConvert", precise)?;
    let doubled = f.op(Op::F_ADD, double, &[precise, precise])?;
    out_wide(f, io, "FAdd", doubled)?;
    let pair = io.kit.uvec(2);
    let unpacked = f.glsl(GlslStd450::UNPACK_DOUBLE2X32, pair, &[doubled])?;
    io.out(f, ("UnpackDouble2x32", Held::Exact), (unpacked, pair))?;
    let rebuilt = f.glsl(GlslStd450::PACK_DOUBLE2X32, double, &[unpacked])?;
    out_wide(f, io, "PackDouble2x32", rebuilt)
}

/// The 16-bit widths: conversions to them and back, and their arithmetic.
fn narrow(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let module = f.module();
    let (ushort, short, half) = (
        module.int(16, false)?,
        module.int(16, true)?,
        module.float(16)?,
    );
    let (word, signed, real) = (
        io.word(f, 0x1234_9abc)?,
        io.int(f, -12_345)?,
        io.float(f, 1.75)?,
    );
    let kit = io.kit;
    let narrowed = f.op(Op::U_CONVERT, ushort, &[word])?;
    let summed = f.op(Op::I_ADD, ushort, &[narrowed, narrowed])?;
    let shortened = f.op(Op::S_CONVERT, short, &[signed])?;
    let halved = f.op(Op::F_CONVERT, half, &[real])?;
    let added = f.op(Op::F_ADD, half, &[halved, halved])?;
    rows(
        f,
        io,
        Held::Exact,
        &[
            ("UConvert", Op::U_CONVERT, kit.uint, &[summed]),
            ("SConvert", Op::S_CONVERT, kit.int, &[shortened]),
            ("FConvert", Op::F_CONVERT, kit.float, &[added]),
        ],
    )
}

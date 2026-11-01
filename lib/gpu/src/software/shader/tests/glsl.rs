//! The `GLSL.std.450` instructions: exact where the result is, and the
//! transcendental functions within two units in the last place of the
//! host's own.

use alloc::vec::Vec;

use super::{d, f, Case, Scalar, Ty};

/// `body` applying instruction `name` to `%a`, and to `%b` and `%c` where
/// given.
fn ext(result: &str, name: &str, args: &str) -> alloc::string::String {
    alloc::format!("%r = OpExtInst {result} %glsl {name} {args}")
}

fn unary(name: &str, cases: &[(f32, f32)]) -> Case {
    cases.iter().fold(
        Case::new(&[Ty::F32], Ty::F32, &ext("%float", name, "%a")),
        |c, &(x, y)| c.lane(&[f(x)], &[f(y)]),
    )
}

pub(super) fn cases() -> Vec<Case> {
    let mut cases = exact();
    cases.extend(transcendental());
    cases.extend(integer());
    cases.extend(splits());
    cases.extend(packing());
    cases.extend(geometry());
    cases
}

fn exact() -> Vec<Case> {
    use Ty::F32;
    let two = &[F32, F32];
    let three = &[F32, F32, F32];
    let nan = 0x7fc0_0000;
    alloc::vec![
        unary("Round", &[(2.5, 3.0), (-2.5, -2.0), (1.2, 1.0)]),
        unary("RoundEven", &[(2.5, 2.0), (3.5, 4.0)]),
        unary("Trunc", &[(-2.7, -2.0)]),
        unary("FAbs", &[(-3.0, 3.0)]),
        unary("FSign", &[(-2.0, -1.0), (0.0, 0.0), (5.0, 1.0)]),
        unary("Floor", &[(-1.5, -2.0)]),
        unary("Ceil", &[(-1.5, -1.0)]),
        unary("Fract", &[(1.75, 0.75), (-0.25, 0.75)]),
        unary("Exp2", &[(3.0, 8.0)]),
        unary("Log2", &[(8.0, 3.0)]),
        unary("Sqrt", &[(4.0, 2.0)]),
        unary("InverseSqrt", &[(4.0, 0.5)]),
        Case::new(two, F32, &ext("%float", "FMin", "%a %b")).lane(&[f(1.0), f(2.0)], &[f(1.0)]),
        Case::new(two, F32, &ext("%float", "FMax", "%a %b")).lane(&[f(1.0), f(2.0)], &[f(2.0)]),
        Case::new(two, F32, &ext("%float", "NMin", "%a %b")).lane(&[nan, f(1.0)], &[f(1.0)]),
        Case::new(two, F32, &ext("%float", "NMax", "%a %b")).lane(&[nan, f(1.0)], &[f(1.0)]),
        Case::new(two, F32, &ext("%float", "Step", "%a %b"))
            .lane(&[f(1.0), f(0.5)], &[f(0.0)])
            .lane(&[f(1.0), f(1.5)], &[f(1.0)]),
        Case::new(two, F32, &ext("%float", "Pow", "%a %b")).lane(&[f(2.0), f(10.0)], &[f(1024.0)]),
        Case::new(three, F32, &ext("%float", "FClamp", "%a %b %c"))
            .lane(&[f(5.0), f(0.0), f(1.0)], &[f(1.0)]),
        Case::new(three, F32, &ext("%float", "NClamp", "%a %b %c"))
            .lane(&[nan, f(0.0), f(1.0)], &[f(0.0)]),
        Case::new(three, F32, &ext("%float", "FMix", "%a %b %c"))
            .lane(&[f(0.0), f(10.0), f(0.25)], &[f(2.5)]),
        Case::new(three, F32, &ext("%float", "SmoothStep", "%a %b %c"))
            .lane(&[f(0.0), f(1.0), f(0.5)], &[f(0.5)]),
        Case::new(three, F32, &ext("%float", "Fma", "%a %b %c"))
            .lane(&[f(2.0), f(3.0), f(4.0)], &[f(10.0)]),
        Case::new(&[F32, Ty::I32], F32, &ext("%float", "Ldexp", "%a %b"))
            .lane(&[f(0.75), 3], &[f(6.0)]),
    ]
}

/// A function's name and the points it is held to: each input, and the bits
/// of its value correctly rounded to a single, computed apart from any libm
/// so the reference is the same on every host and under an interpreter.
type Reference = (&'static str, &'static [(f32, u32)]);

fn transcendental() -> Vec<Case> {
    use core::f32::consts::{E, PI};
    let references: [Reference; 20] = [
        ("Radians", &[(180.0, 0x4049_0fdb), (45.0, 0x3f49_0fdb)]),
        ("Degrees", &[(PI, 0x4334_0000), (1.0, 0x4265_2ee1)]),
        (
            "Sin",
            &[
                (0.5, 0x3ef5_7744),
                (-2.0, 0xbf68_c7b7),
                (100.0, 0xbf01_a12e),
            ],
        ),
        ("Cos", &[(0.5, 0x3f60_a940), (3.0, 0xbf7d_7026)]),
        ("Tan", &[(0.5, 0x3f0b_da7b), (-1.2, 0xc024_9e23)]),
        ("Asin", &[(0.5, 0x3f06_0a92), (-0.25, 0xbe81_5f4e)]),
        ("Acos", &[(0.5, 0x3f86_0a92), (-0.25, 0x3fe9_67ae)]),
        ("Atan", &[(0.5, 0x3eed_6338), (10.0, 0x3fbc_4de9)]),
        ("Sinh", &[(0.5, 0x3f05_6680), (-2.0, 0xc068_1e7b)]),
        ("Cosh", &[(0.5, 0x3f90_560c), (2.0, 0x4070_c7d0)]),
        ("Tanh", &[(0.5, 0x3eec_9a9f), (-3.0, 0xbf7e_bbe9)]),
        ("Asinh", &[(0.5, 0x3ef6_6165), (40.0, 0x408c_3ad8)]),
        ("Acosh", &[(1.5, 0x3f76_6165), (40.0, 0x408c_3848)]),
        ("Atanh", &[(0.5, 0x3f0c_9f54), (-0.25, 0xbe82_c578)]),
        ("Exp", &[(1.0, 0x402d_f854), (-3.5, 0x3cf7_6081)]),
        ("Log", &[(E, 0x3f7f_ffff), (0.1, 0xc013_5d8e)]),
        ("Exp2", &[(0.5, 0x3fb5_04f3), (-7.25, 0x3bd7_44fd)]),
        ("Log2", &[(10.0, 0x4054_9a78), (0.3, 0xbfde_54e3)]),
        ("Sqrt", &[(2.0, 0x3fb5_04f3), (0.1, 0x3ea1_e89b)]),
        ("InverseSqrt", &[(2.0, 0x3f35_04f3), (0.1, 0x404a_62c2)]),
    ];
    let mut cases: Vec<Case> = references
        .iter()
        .map(|&(name, points)| {
            points
                .iter()
                .fold(
                    Case::new(&[Ty::F32], Ty::F32, &ext("%float", name, "%a")),
                    |c, &(x, value)| c.lane(&[f(x)], &[value]),
                )
                .within(2)
        })
        .collect();
    cases.push(
        Case::new(
            &[Ty::F32, Ty::F32],
            Ty::F32,
            &ext("%float", "Atan2", "%a %b"),
        )
        .lane(&[f(1.0), f(-1.0)], &[0x4016_cbe4])
        .within(2),
    );
    cases
}

fn integer() -> Vec<Case> {
    use Ty::{I32, U32};
    let m1 = u32::MAX;
    alloc::vec![
        Case::new(&[I32], I32, &ext("%int", "SAbs", "%a"))
            .lane(&[(-5i32).cast_unsigned()], &[5])
            .lane(&[0x8000_0000], &[0x8000_0000]),
        Case::new(&[I32], I32, &ext("%int", "SSign", "%a"))
            .lane(&[(-7i32).cast_unsigned()], &[m1])
            .lane(&[0], &[0])
            .lane(&[9], &[1]),
        Case::new(&[U32, U32], U32, &ext("%uint", "UMin", "%a %b")).lane(&[m1, 1], &[1]),
        Case::new(&[I32, I32], I32, &ext("%int", "SMin", "%a %b")).lane(&[m1, 1], &[m1]),
        Case::new(&[U32, U32], U32, &ext("%uint", "UMax", "%a %b")).lane(&[m1, 1], &[m1]),
        Case::new(&[I32, I32], I32, &ext("%int", "SMax", "%a %b")).lane(&[m1, 1], &[1]),
        Case::new(&[U32, U32, U32], U32, &ext("%uint", "UClamp", "%a %b %c"))
            .lane(&[5, 1, 3], &[3]),
        Case::new(&[I32, I32, I32], I32, &ext("%int", "SClamp", "%a %b %c")).lane(
            &[(-5i32).cast_unsigned(), (-2i32).cast_unsigned(), 2],
            &[(-2i32).cast_unsigned()]
        ),
        Case::new(&[U32], U32, &ext("%uint", "FindILsb", "%a"))
            .lane(&[8], &[3])
            .lane(&[0], &[m1]),
        Case::new(&[I32], I32, &ext("%int", "FindSMsb", "%a"))
            .lane(&[m1], &[m1])
            .lane(&[0x10], &[4])
            .lane(&[(-17i32).cast_unsigned()], &[4]),
        Case::new(&[U32], U32, &ext("%uint", "FindUMsb", "%a"))
            .lane(&[0x8000_0000], &[31])
            .lane(&[0], &[m1]),
    ]
}

fn splits() -> Vec<Case> {
    use Ty::F32;
    let v2 = Ty::V(Scalar::F32, 2);
    let private = "%float_private = OpTypePointer Private %float\n%int_private = OpTypePointer Private %int\n%whole = OpVariable %float_private Private\n%exponent = OpVariable %int_private Private";
    alloc::vec![
        Case::new(&[F32], v2, "%f = OpExtInst %float %glsl Modf %a %whole\n%w = OpLoad %float %whole\n%r = OpCompositeConstruct %v2float %f %w")
            .decls(private)
            .interface("%whole %exponent")
            .lane(&[f(3.75)], &[f(0.75), f(3.0)]),
        Case::new(&[F32], Ty::Pair("modf", Scalar::F32, 1), "%r = OpExtInst %modf %glsl ModfStruct %a")
            .decls("%modf = OpTypeStruct %float %float")
            .lane(&[f(-3.75)], &[f(-0.75), f(-3.0)]),
        Case::new(&[F32], Ty::V(Scalar::U32, 2), "%s = OpExtInst %float %glsl Frexp %a %exponent\n%e = OpLoad %int %exponent\n%su = OpBitcast %uint %s\n%eu = OpBitcast %uint %e\n%r = OpCompositeConstruct %v2uint %su %eu")
            .decls(private)
            .interface("%whole %exponent")
            .lane(&[f(8.0)], &[f(0.5), 4]),
        Case::new(&[F32], Ty::V(Scalar::U32, 2), "%s = OpExtInst %frexp %glsl FrexpStruct %a\n%m = OpCompositeExtract %float %s 0\n%e = OpCompositeExtract %int %s 1\n%mu = OpBitcast %uint %m\n%eu = OpBitcast %uint %e\n%r = OpCompositeConstruct %v2uint %mu %eu")
            .decls("%frexp = OpTypeStruct %float %int")
            .lane(&[f(3.0)], &[f(0.75), 2]),
    ]
}

fn packing() -> Vec<Case> {
    use Ty::U32;
    let v4 = Ty::V(Scalar::F32, 4);
    let v2 = Ty::V(Scalar::F32, 2);
    let v2u = Ty::V(Scalar::U32, 2);
    alloc::vec![
        Case::new(&[v4], U32, &ext("%uint", "PackSnorm4x8", "%a"))
            .lane(&[f(1.0), f(-1.0), f(0.5), f(0.0)], &[0x0040_817f]),
        Case::new(&[v4], U32, &ext("%uint", "PackUnorm4x8", "%a"))
            .lane(&[f(1.0), f(0.0), f(0.5), f(0.25)], &[0x4080_00ff]),
        Case::new(&[v2], U32, &ext("%uint", "PackSnorm2x16", "%a"))
            .lane(&[f(1.0), f(-1.0)], &[0x8001_7fff]),
        Case::new(&[v2], U32, &ext("%uint", "PackUnorm2x16", "%a"))
            .lane(&[f(1.0), f(0.0)], &[0x0000_ffff]),
        Case::new(&[v2], U32, &ext("%uint", "PackHalf2x16", "%a"))
            .lane(&[f(1.0), f(2.0)], &[0x4000_3c00]),
        Case::new(&[v2u], Ty::F64, &ext("%double", "PackDouble2x32", "%a")).lane(&d(1.0), &d(1.0)),
        Case::new(&[U32], v2, &ext("%v2float", "UnpackSnorm2x16", "%a"))
            .lane(&[0x8001_7fff], &[f(1.0), f(-1.0)]),
        Case::new(&[U32], v2, &ext("%v2float", "UnpackUnorm2x16", "%a"))
            .lane(&[0x0000_ffff], &[f(1.0), f(0.0)]),
        Case::new(&[U32], v2, &ext("%v2float", "UnpackHalf2x16", "%a"))
            .lane(&[0x4000_3c00], &[f(1.0), f(2.0)]),
        Case::new(&[U32], v4, &ext("%v4float", "UnpackSnorm4x8", "%a"))
            .lane(&[0x0040_817f], &[f(1.0), f(-1.0), f(64.0 / 127.0), f(0.0)]),
        Case::new(&[U32], v4, &ext("%v4float", "UnpackUnorm4x8", "%a")).lane(
            &[0x4080_00ff],
            &[f(1.0), f(0.0), f(128.0 / 255.0), f(64.0 / 255.0)]
        ),
        Case::new(&[Ty::F64], v2u, &ext("%v2uint", "UnpackDouble2x32", "%a"))
            .lane(&d(1.0), &d(1.0)),
    ]
}

fn geometry() -> Vec<Case> {
    use Ty::F32;
    let v2 = Ty::V(Scalar::F32, 2);
    let v3 = Ty::V(Scalar::F32, 3);
    let v4 = Ty::V(Scalar::F32, 4);
    let matrix = "%m2 = OpTypeMatrix %v2float 2";
    let columns = [f(1.0), f(3.0), f(2.0), f(4.0)];
    alloc::vec![
        Case::new(&[v2], F32, &ext("%float", "Length", "%a")).lane(&[f(3.0), f(4.0)], &[f(5.0)]),
        Case::new(&[v2, v2], F32, &ext("%float", "Distance", "%a %b")).lane(&[f(1.0), f(1.0), f(4.0), f(5.0)], &[f(5.0)]),
        Case::new(&[v3, v3], v3, &ext("%v3float", "Cross", "%a %b"))
            .lane(&[f(1.0), 0, 0, 0, f(1.0), 0], &[0, 0, f(1.0)]),
        Case::new(&[v2], v2, &ext("%v2float", "Normalize", "%a")).lane(&[f(3.0), f(4.0)], &[f(0.6), f(0.8)]).within(1),
        Case::new(&[v2, v2, v2], v2, &ext("%v2float", "FaceForward", "%a %b %c"))
            .lane(&[0, f(1.0), 0, f(-1.0), 0, f(1.0)], &[0, f(1.0)])
            .lane(&[0, f(1.0), 0, f(1.0), 0, f(1.0)], &[f(-0.0), f(-1.0)]),
        Case::new(&[v2, v2], v2, &ext("%v2float", "Reflect", "%a %b")).lane(&[f(1.0), f(-1.0), 0, f(1.0)], &[f(1.0), f(1.0)]),
        Case::new(&[v2, v2, F32], v2, &ext("%v2float", "Refract", "%a %b %c"))
            .lane(&[0, f(-1.0), 0, f(1.0), f(1.0)], &[0, f(-1.0)])
            .lane(&[f(1.0), f(-0.1), 0, f(1.0), f(2.0)], &[0, 0]),
        // det [1 2; 3 4] = -2, its columns (1, 3) and (2, 4).
        Case::new(&[v2, v2], F32, "%m = OpCompositeConstruct %m2 %a %b\n%r = OpExtInst %float %glsl Determinant %m")
            .decls(matrix)
            .lane(&columns, &[f(-2.0)]),
        // [1 2; 3 4]^-1 = [-2 1; 1.5 -0.5].
        Case::new(&[v2, v2], v4, "%m = OpCompositeConstruct %m2 %a %b\n%i = OpExtInst %m2 %glsl MatrixInverse %m\n%i0 = OpCompositeExtract %v2float %i 0\n%i1 = OpCompositeExtract %v2float %i 1\n%r = OpCompositeConstruct %v4float %i0 %i1")
            .decls(matrix)
            .lane(&columns, &[f(-2.0), f(1.5), f(1.0), f(-0.5)]),
    ]
}

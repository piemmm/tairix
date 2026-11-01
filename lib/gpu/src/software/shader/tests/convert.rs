//! Conversions between scalars, and bit casts between types of one width.

use alloc::vec::Vec;

use super::{d, f, Case, Scalar, Ty};

pub(super) fn cases() -> Vec<Case> {
    use Ty::{F16, F32, F64, I16, I32, I64, U32, U64};
    let nan = 0x7fc0_0000;
    alloc::vec![
        Case::new(&[F32], U32, "%r = OpConvertFToU %uint %a")
            .lane(&[f(3.7)], &[3])
            .lane(&[f(-1.0)], &[0])
            .lane(&[nan], &[0])
            .lane(&[f(5e9)], &[u32::MAX]),
        Case::new(&[F32], I32, "%r = OpConvertFToS %int %a")
            .lane(&[f(-3.7)], &[(-3i32).cast_unsigned()])
            .lane(&[f(3e9)], &[0x7fff_ffff])
            .lane(&[f(-3e9)], &[0x8000_0000])
            .lane(&[nan], &[0]),
        Case::new(&[I32], F32, "%r = OpConvertSToF %float %a")
            .lane(&[(-2i32).cast_unsigned()], &[f(-2.0)]),
        Case::new(&[U32], F32, "%r = OpConvertUToF %float %a").lane(&[u32::MAX], &[0x4f80_0000]),
        Case::new(&[U32], U64, "%r = OpUConvert %ulong %a").lane(&[u32::MAX], &[u32::MAX, 0]),
        Case::new(&[U64], U32, "%r = OpUConvert %uint %a").lane(&[5, 7], &[5]),
        Case::new(&[I32], I64, "%r = OpSConvert %long %a").lane(&[u32::MAX], &[u32::MAX, u32::MAX]),
        Case::new(&[I16], I32, "%r = OpSConvert %int %a").lane(&[0x8000], &[0xffff_8000]),
        Case::new(&[F32], F64, "%r = OpFConvert %double %a").lane(&[f(1.5)], &d(1.5)),
        Case::new(&[F64], F32, "%r = OpFConvert %float %a").lane(&d(0.1), &[f(0.1)]),
        // Just past a half's tie rounds away from it: rounded once.
        Case::new(&[F64], F16, "%r = OpFConvert %half %a")
            .lane(&d(1.000_488_281_250_000_2), &[0x3c01]),
        Case::new(&[Ty::V(Scalar::F16, 2)], U32, "%r = OpBitcast %uint %a")
            .lane(&[0x3c00, 0x4000], &[0x4000_3c00]),
        Case::new(&[U32], Ty::V(Scalar::U16, 2), "%r = OpBitcast %v2ushort %a")
            .lane(&[0x4000_3c00], &[0x3c00, 0x4000]),
        Case::new(&[I64], Ty::V(Scalar::U32, 2), "%r = OpBitcast %v2uint %a")
            .lane(&[1, 2], &[1, 2]),
    ]
}

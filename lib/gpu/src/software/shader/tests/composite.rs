//! Composites taken apart and put together: constructed, extracted from and
//! inserted into by literal paths, shuffled, indexed by a lane's own value,
//! copied logically, and undefined values.

use alloc::vec::Vec;

use super::{f, Case, Scalar, Ty};

pub(super) fn cases() -> Vec<Case> {
    use Ty::{F32, U32};
    let v4 = Ty::V(Scalar::U32, 4);
    let v2 = Ty::V(Scalar::U32, 2);
    let nested = "%inner = OpTypeStruct %uint %v2uint\n%array = OpTypeArray %inner %c2\n%outer = OpTypeStruct %float %array";
    alloc::vec![
        // A structure of an array of structures, built then taken apart.
        Case::new(
            &[U32, v2, U32, v2, F32],
            U32,
            "%i0 = OpCompositeConstruct %inner %a %b\n%i1 = OpCompositeConstruct %inner %c %d\n%arr = OpCompositeConstruct %array %i0 %i1\n%o = OpCompositeConstruct %outer %e %arr\n%r = OpCompositeExtract %uint %o 1 1 1 0",
        )
        .decls(nested)
        .lane(&[1, 2, 3, 4, 5, 6, f(7.0)], &[5]),
        Case::new(
            &[U32, v2, U32],
            v2,
            "%i0 = OpCompositeConstruct %inner %a %b\n%i1 = OpCompositeInsert %inner %c %i0 1 0\n%r = OpCompositeExtract %v2uint %i1 1",
        )
        .decls("%inner = OpTypeStruct %uint %v2uint")
        .lane(&[1, 2, 3, 9], &[9, 3]),
        Case::new(&[v4, v4], v4, "%r = OpVectorShuffle %v4uint %a %b 7 0 0xFFFFFFFF 5")
            .lane(&[1, 2, 3, 4, 5, 6, 7, 8], &[8, 1, 0, 6]),
        Case::new(&[v4, U32], U32, "%r = OpVectorExtractDynamic %uint %a %b")
            .lane(&[10, 11, 12, 13, 2], &[12])
            .lane(&[10, 11, 12, 13, 4], &[0]),
        Case::new(&[v4, U32, U32], v4, "%r = OpVectorInsertDynamic %v4uint %a %b %c")
            .lane(&[10, 11, 12, 13, 99, 1], &[10, 99, 12, 13])
            .lane(&[10, 11, 12, 13, 99, 7], &[10, 11, 12, 13]),
        Case::new(
            &[U32, F32],
            F32,
            "%s = OpCompositeConstruct %first %a %b\n%t = OpCopyLogical %second %s\n%r = OpCompositeExtract %float %t 1",
        )
        .decls("%first = OpTypeStruct %uint %float\n%second = OpTypeStruct %uint %float")
        .lane(&[1, f(2.5)], &[f(2.5)]),
        Case::new(&[U32], v2, "%u = OpUndef %v2uint\n%r = OpCompositeInsert %v2uint %a %u 1")
            .lane(&[7], &[0, 7]),
        Case::new(&[U32], U32, "%r = OpCopyObject %uint %a").lane(&[5], &[5]),
    ]
}

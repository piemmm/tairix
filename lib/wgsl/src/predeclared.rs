//! The names the language declares before any module does: types, the
//! generators that make types from template lists, enumerants, and the
//! built-in functions — each found by binary search over one sorted table.
//! A module's own declaration of a name shadows it.

use crate::types::{Access, Dim, Scalar, Space, TexelFormat, TEXEL_FORMATS};

/// A predeclared type that needs no template list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Named {
    Scalar(Scalar),
    /// A vector alias: `vec3f`.
    Vector(u8, Scalar),
    /// A matrix alias: `mat3x4h`.
    Matrix(u8, u8, Scalar),
    Sampler {
        comparison: bool,
    },
    External,
    Depth(Dim),
    DepthMultisampled,
}

/// A predeclared type generator, completed by a template list.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Generator {
    Vector(u8),
    Matrix(u8, u8),
    Array,
    Atomic,
    Pointer,
    Sampled(Dim),
    Multisampled,
    Storage(Dim),
}

/// A built-in function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Builtin {
    Bitcast,
    All,
    Any,
    Select,
    ArrayLength,
    Abs,
    Acos,
    Acosh,
    Asin,
    Asinh,
    Atan,
    Atanh,
    Atan2,
    Ceil,
    Clamp,
    Cos,
    Cosh,
    CountLeadingZeros,
    CountOneBits,
    CountTrailingZeros,
    Cross,
    Degrees,
    Determinant,
    Distance,
    Dot,
    Dot4U8Packed,
    Dot4I8Packed,
    Exp,
    Exp2,
    ExtractBits,
    FaceForward,
    FirstLeadingBit,
    FirstTrailingBit,
    Floor,
    Fma,
    Fract,
    Frexp,
    InsertBits,
    InverseSqrt,
    Ldexp,
    Length,
    Log,
    Log2,
    Max,
    Min,
    Mix,
    Modf,
    Normalize,
    Pow,
    QuantizeToF16,
    Radians,
    Reflect,
    Refract,
    ReverseBits,
    Round,
    Saturate,
    Sign,
    Sin,
    Sinh,
    Smoothstep,
    Sqrt,
    Step,
    Tan,
    Tanh,
    Transpose,
    Trunc,
    Dpdx,
    DpdxCoarse,
    DpdxFine,
    Dpdy,
    DpdyCoarse,
    DpdyFine,
    Fwidth,
    FwidthCoarse,
    FwidthFine,
    TextureDimensions,
    TextureGather,
    TextureGatherCompare,
    TextureLoad,
    TextureNumLayers,
    TextureNumLevels,
    TextureNumSamples,
    TextureSample,
    TextureSampleBias,
    TextureSampleCompare,
    TextureSampleCompareLevel,
    TextureSampleGrad,
    TextureSampleLevel,
    TextureSampleBaseClampToEdge,
    TextureStore,
    AtomicLoad,
    AtomicStore,
    AtomicAdd,
    AtomicSub,
    AtomicMax,
    AtomicMin,
    AtomicAnd,
    AtomicOr,
    AtomicXor,
    AtomicExchange,
    AtomicCompareExchangeWeak,
    Pack4x8Snorm,
    Pack4x8Unorm,
    Pack4xI8,
    Pack4xU8,
    Pack4xI8Clamp,
    Pack4xU8Clamp,
    Pack2x16Snorm,
    Pack2x16Unorm,
    Pack2x16Float,
    Unpack4x8Snorm,
    Unpack4x8Unorm,
    Unpack4xI8,
    Unpack4xU8,
    Unpack2x16Snorm,
    Unpack2x16Unorm,
    Unpack2x16Float,
    StorageBarrier,
    TextureBarrier,
    WorkgroupBarrier,
    WorkgroupUniformLoad,
    /// A function of an extension TAIRiX does not offer, refused by name
    /// where it is called.
    Refused(Withheld),
}

/// An extension TAIRiX does not offer whose functions WGSL predeclares: the
/// subgroup and quad operations, the 64-bit atomics' min and max, and the
/// buffer views.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Withheld {
    AtomicVec2uMinMax,
    BufferView,
    Subgroups,
}

impl Withheld {
    /// The extension's name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::AtomicVec2uMinMax => "atomic_vec2u_min_max",
            Self::BufferView => "buffer_view",
            Self::Subgroups => "subgroups",
        }
    }
}

/// What a predeclared name denotes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Predeclared {
    Type(Named),
    Generator(Generator),
    Builtin(Builtin),
    Space(Space),
    Access(Access),
    /// A texel format, and whether it needs `texture_formats_tier1`.
    Format(TexelFormat, bool),
}

/// Every predeclared name but the texel formats, ascending by byte.
const NAMES: [(&str, Predeclared); 229] = {
    use Builtin as B;
    use Generator as G;
    use Named as N;
    use Predeclared::{Access as A, Builtin as F, Generator as Gen, Space as S, Type as T};
    use Scalar::{Bool, F16, F32, I32, U32};
    [
        ("abs", F(B::Abs)),
        ("acos", F(B::Acos)),
        ("acosh", F(B::Acosh)),
        ("all", F(B::All)),
        ("any", F(B::Any)),
        ("array", Gen(G::Array)),
        ("arrayLength", F(B::ArrayLength)),
        ("asin", F(B::Asin)),
        ("asinh", F(B::Asinh)),
        ("atan", F(B::Atan)),
        ("atan2", F(B::Atan2)),
        ("atanh", F(B::Atanh)),
        ("atomic", Gen(G::Atomic)),
        ("atomicAdd", F(B::AtomicAdd)),
        ("atomicAnd", F(B::AtomicAnd)),
        ("atomicCompareExchangeWeak", F(B::AtomicCompareExchangeWeak)),
        ("atomicExchange", F(B::AtomicExchange)),
        ("atomicLoad", F(B::AtomicLoad)),
        ("atomicMax", F(B::AtomicMax)),
        ("atomicMin", F(B::AtomicMin)),
        ("atomicOr", F(B::AtomicOr)),
        ("atomicStore", F(B::AtomicStore)),
        ("atomicStoreMax", F(B::Refused(Withheld::AtomicVec2uMinMax))),
        ("atomicStoreMin", F(B::Refused(Withheld::AtomicVec2uMinMax))),
        ("atomicSub", F(B::AtomicSub)),
        ("atomicXor", F(B::AtomicXor)),
        ("bitcast", F(B::Bitcast)),
        ("bool", T(N::Scalar(Bool))),
        ("bufferArrayView", F(B::Refused(Withheld::BufferView))),
        ("bufferLength", F(B::Refused(Withheld::BufferView))),
        ("bufferView", F(B::Refused(Withheld::BufferView))),
        ("ceil", F(B::Ceil)),
        ("clamp", F(B::Clamp)),
        ("cos", F(B::Cos)),
        ("cosh", F(B::Cosh)),
        ("countLeadingZeros", F(B::CountLeadingZeros)),
        ("countOneBits", F(B::CountOneBits)),
        ("countTrailingZeros", F(B::CountTrailingZeros)),
        ("cross", F(B::Cross)),
        ("degrees", F(B::Degrees)),
        ("determinant", F(B::Determinant)),
        ("distance", F(B::Distance)),
        ("dot", F(B::Dot)),
        ("dot4I8Packed", F(B::Dot4I8Packed)),
        ("dot4U8Packed", F(B::Dot4U8Packed)),
        ("dpdx", F(B::Dpdx)),
        ("dpdxCoarse", F(B::DpdxCoarse)),
        ("dpdxFine", F(B::DpdxFine)),
        ("dpdy", F(B::Dpdy)),
        ("dpdyCoarse", F(B::DpdyCoarse)),
        ("dpdyFine", F(B::DpdyFine)),
        ("exp", F(B::Exp)),
        ("exp2", F(B::Exp2)),
        ("extractBits", F(B::ExtractBits)),
        ("f16", T(N::Scalar(F16))),
        ("f32", T(N::Scalar(F32))),
        ("faceForward", F(B::FaceForward)),
        ("firstLeadingBit", F(B::FirstLeadingBit)),
        ("firstTrailingBit", F(B::FirstTrailingBit)),
        ("floor", F(B::Floor)),
        ("fma", F(B::Fma)),
        ("fract", F(B::Fract)),
        ("frexp", F(B::Frexp)),
        ("function", S(Space::Function)),
        ("fwidth", F(B::Fwidth)),
        ("fwidthCoarse", F(B::FwidthCoarse)),
        ("fwidthFine", F(B::FwidthFine)),
        ("i32", T(N::Scalar(I32))),
        ("immediate", S(Space::Immediate)),
        ("insertBits", F(B::InsertBits)),
        ("inverseSqrt", F(B::InverseSqrt)),
        ("ldexp", F(B::Ldexp)),
        ("length", F(B::Length)),
        ("log", F(B::Log)),
        ("log2", F(B::Log2)),
        ("mat2x2", Gen(G::Matrix(2, 2))),
        ("mat2x2f", T(N::Matrix(2, 2, F32))),
        ("mat2x2h", T(N::Matrix(2, 2, F16))),
        ("mat2x3", Gen(G::Matrix(2, 3))),
        ("mat2x3f", T(N::Matrix(2, 3, F32))),
        ("mat2x3h", T(N::Matrix(2, 3, F16))),
        ("mat2x4", Gen(G::Matrix(2, 4))),
        ("mat2x4f", T(N::Matrix(2, 4, F32))),
        ("mat2x4h", T(N::Matrix(2, 4, F16))),
        ("mat3x2", Gen(G::Matrix(3, 2))),
        ("mat3x2f", T(N::Matrix(3, 2, F32))),
        ("mat3x2h", T(N::Matrix(3, 2, F16))),
        ("mat3x3", Gen(G::Matrix(3, 3))),
        ("mat3x3f", T(N::Matrix(3, 3, F32))),
        ("mat3x3h", T(N::Matrix(3, 3, F16))),
        ("mat3x4", Gen(G::Matrix(3, 4))),
        ("mat3x4f", T(N::Matrix(3, 4, F32))),
        ("mat3x4h", T(N::Matrix(3, 4, F16))),
        ("mat4x2", Gen(G::Matrix(4, 2))),
        ("mat4x2f", T(N::Matrix(4, 2, F32))),
        ("mat4x2h", T(N::Matrix(4, 2, F16))),
        ("mat4x3", Gen(G::Matrix(4, 3))),
        ("mat4x3f", T(N::Matrix(4, 3, F32))),
        ("mat4x3h", T(N::Matrix(4, 3, F16))),
        ("mat4x4", Gen(G::Matrix(4, 4))),
        ("mat4x4f", T(N::Matrix(4, 4, F32))),
        ("mat4x4h", T(N::Matrix(4, 4, F16))),
        ("max", F(B::Max)),
        ("min", F(B::Min)),
        ("mix", F(B::Mix)),
        ("modf", F(B::Modf)),
        ("normalize", F(B::Normalize)),
        ("pack2x16float", F(B::Pack2x16Float)),
        ("pack2x16snorm", F(B::Pack2x16Snorm)),
        ("pack2x16unorm", F(B::Pack2x16Unorm)),
        ("pack4x8snorm", F(B::Pack4x8Snorm)),
        ("pack4x8unorm", F(B::Pack4x8Unorm)),
        ("pack4xI8", F(B::Pack4xI8)),
        ("pack4xI8Clamp", F(B::Pack4xI8Clamp)),
        ("pack4xU8", F(B::Pack4xU8)),
        ("pack4xU8Clamp", F(B::Pack4xU8Clamp)),
        ("pow", F(B::Pow)),
        ("private", S(Space::Private)),
        ("ptr", Gen(G::Pointer)),
        ("quadBroadcast", F(B::Refused(Withheld::Subgroups))),
        ("quadSwapDiagonal", F(B::Refused(Withheld::Subgroups))),
        ("quadSwapX", F(B::Refused(Withheld::Subgroups))),
        ("quadSwapY", F(B::Refused(Withheld::Subgroups))),
        ("quantizeToF16", F(B::QuantizeToF16)),
        ("radians", F(B::Radians)),
        ("read", A(Access::Read)),
        ("read_write", A(Access::ReadWrite)),
        ("reflect", F(B::Reflect)),
        ("refract", F(B::Refract)),
        ("reverseBits", F(B::ReverseBits)),
        ("round", F(B::Round)),
        ("sampler", T(N::Sampler { comparison: false })),
        ("sampler_comparison", T(N::Sampler { comparison: true })),
        ("saturate", F(B::Saturate)),
        ("select", F(B::Select)),
        ("sign", F(B::Sign)),
        ("sin", F(B::Sin)),
        ("sinh", F(B::Sinh)),
        ("smoothstep", F(B::Smoothstep)),
        ("sqrt", F(B::Sqrt)),
        ("step", F(B::Step)),
        ("storage", S(Space::Storage)),
        ("storageBarrier", F(B::StorageBarrier)),
        ("subgroupAdd", F(B::Refused(Withheld::Subgroups))),
        ("subgroupAll", F(B::Refused(Withheld::Subgroups))),
        ("subgroupAnd", F(B::Refused(Withheld::Subgroups))),
        ("subgroupAny", F(B::Refused(Withheld::Subgroups))),
        ("subgroupBallot", F(B::Refused(Withheld::Subgroups))),
        ("subgroupBroadcast", F(B::Refused(Withheld::Subgroups))),
        ("subgroupBroadcastFirst", F(B::Refused(Withheld::Subgroups))),
        ("subgroupElect", F(B::Refused(Withheld::Subgroups))),
        ("subgroupExclusiveAdd", F(B::Refused(Withheld::Subgroups))),
        ("subgroupExclusiveMul", F(B::Refused(Withheld::Subgroups))),
        ("subgroupInclusiveAdd", F(B::Refused(Withheld::Subgroups))),
        ("subgroupInclusiveMul", F(B::Refused(Withheld::Subgroups))),
        ("subgroupMax", F(B::Refused(Withheld::Subgroups))),
        ("subgroupMin", F(B::Refused(Withheld::Subgroups))),
        ("subgroupMul", F(B::Refused(Withheld::Subgroups))),
        ("subgroupOr", F(B::Refused(Withheld::Subgroups))),
        ("subgroupShuffle", F(B::Refused(Withheld::Subgroups))),
        ("subgroupShuffleDown", F(B::Refused(Withheld::Subgroups))),
        ("subgroupShuffleUp", F(B::Refused(Withheld::Subgroups))),
        ("subgroupShuffleXor", F(B::Refused(Withheld::Subgroups))),
        ("subgroupXor", F(B::Refused(Withheld::Subgroups))),
        ("tan", F(B::Tan)),
        ("tanh", F(B::Tanh)),
        ("textureBarrier", F(B::TextureBarrier)),
        ("textureDimensions", F(B::TextureDimensions)),
        ("textureGather", F(B::TextureGather)),
        ("textureGatherCompare", F(B::TextureGatherCompare)),
        ("textureLoad", F(B::TextureLoad)),
        ("textureNumLayers", F(B::TextureNumLayers)),
        ("textureNumLevels", F(B::TextureNumLevels)),
        ("textureNumSamples", F(B::TextureNumSamples)),
        ("textureSample", F(B::TextureSample)),
        (
            "textureSampleBaseClampToEdge",
            F(B::TextureSampleBaseClampToEdge),
        ),
        ("textureSampleBias", F(B::TextureSampleBias)),
        ("textureSampleCompare", F(B::TextureSampleCompare)),
        ("textureSampleCompareLevel", F(B::TextureSampleCompareLevel)),
        ("textureSampleGrad", F(B::TextureSampleGrad)),
        ("textureSampleLevel", F(B::TextureSampleLevel)),
        ("textureStore", F(B::TextureStore)),
        ("texture_1d", Gen(G::Sampled(Dim::D1))),
        ("texture_2d", Gen(G::Sampled(Dim::D2))),
        ("texture_2d_array", Gen(G::Sampled(Dim::D2Array))),
        ("texture_3d", Gen(G::Sampled(Dim::D3))),
        ("texture_cube", Gen(G::Sampled(Dim::Cube))),
        ("texture_cube_array", Gen(G::Sampled(Dim::CubeArray))),
        ("texture_depth_2d", T(N::Depth(Dim::D2))),
        ("texture_depth_2d_array", T(N::Depth(Dim::D2Array))),
        ("texture_depth_cube", T(N::Depth(Dim::Cube))),
        ("texture_depth_cube_array", T(N::Depth(Dim::CubeArray))),
        ("texture_depth_multisampled_2d", T(N::DepthMultisampled)),
        ("texture_external", T(N::External)),
        ("texture_multisampled_2d", Gen(G::Multisampled)),
        ("texture_storage_1d", Gen(G::Storage(Dim::D1))),
        ("texture_storage_2d", Gen(G::Storage(Dim::D2))),
        ("texture_storage_2d_array", Gen(G::Storage(Dim::D2Array))),
        ("texture_storage_3d", Gen(G::Storage(Dim::D3))),
        ("transpose", F(B::Transpose)),
        ("trunc", F(B::Trunc)),
        ("u32", T(N::Scalar(U32))),
        ("uniform", S(Space::Uniform)),
        ("unpack2x16float", F(B::Unpack2x16Float)),
        ("unpack2x16snorm", F(B::Unpack2x16Snorm)),
        ("unpack2x16unorm", F(B::Unpack2x16Unorm)),
        ("unpack4x8snorm", F(B::Unpack4x8Snorm)),
        ("unpack4x8unorm", F(B::Unpack4x8Unorm)),
        ("unpack4xI8", F(B::Unpack4xI8)),
        ("unpack4xU8", F(B::Unpack4xU8)),
        ("vec2", Gen(G::Vector(2))),
        ("vec2f", T(N::Vector(2, F32))),
        ("vec2h", T(N::Vector(2, F16))),
        ("vec2i", T(N::Vector(2, I32))),
        ("vec2u", T(N::Vector(2, U32))),
        ("vec3", Gen(G::Vector(3))),
        ("vec3f", T(N::Vector(3, F32))),
        ("vec3h", T(N::Vector(3, F16))),
        ("vec3i", T(N::Vector(3, I32))),
        ("vec3u", T(N::Vector(3, U32))),
        ("vec4", Gen(G::Vector(4))),
        ("vec4f", T(N::Vector(4, F32))),
        ("vec4h", T(N::Vector(4, F16))),
        ("vec4i", T(N::Vector(4, I32))),
        ("vec4u", T(N::Vector(4, U32))),
        ("workgroup", S(Space::Workgroup)),
        ("workgroupBarrier", F(B::WorkgroupBarrier)),
        ("workgroupUniformLoad", F(B::WorkgroupUniformLoad)),
        ("write", A(Access::Write)),
    ]
};

/// What the predeclared name `name` denotes, if it is one.
pub(crate) fn lookup(name: &str) -> Option<Predeclared> {
    if let Ok(found) = NAMES.binary_search_by(|(n, _)| n.cmp(&name)) {
        return Some(NAMES[found].1);
    }
    TEXEL_FORMATS
        .binary_search_by(|(n, _, _)| n.cmp(&name))
        .ok()
        .map(|found| {
            let (_, format, tier1) = TEXEL_FORMATS[found];
            Predeclared::Format(format, tier1)
        })
}

#[cfg(test)]
mod tests {
    use super::{lookup, Builtin, Predeclared, Withheld, NAMES};
    use crate::types::TEXEL_FORMATS;

    #[test]
    fn the_tables_are_sorted_for_their_searches() {
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(TEXEL_FORMATS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn every_name_is_found_as_itself() {
        for (name, denotes) in NAMES {
            assert_eq!(lookup(name), Some(denotes), "{name}");
        }
        for (name, format, tier1) in TEXEL_FORMATS {
            assert_eq!(lookup(name), Some(Predeclared::Format(format, tier1)));
            assert_eq!(format.name(), name);
        }
        assert_eq!(
            lookup("subgroupAdd"),
            Some(Predeclared::Builtin(Builtin::Refused(Withheld::Subgroups)))
        );
        assert_eq!(lookup("handle"), None, "no source spells the handle space");
        assert_eq!(lookup("vec5"), None);
    }
}

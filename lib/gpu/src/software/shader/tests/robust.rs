//! Accesses past every kind of resource: a storage and a uniform buffer
//! bound over less than their block, texel buffers, a storage image viewed
//! at one level and an array at one layer, a sampled image viewed at one
//! level, push constants, and workgroup, private and function arrays. Each
//! reads zero past what it reaches and writes nothing there. An array of
//! descriptors is indexed only by constants within it, which its layout
//! covers, so none is reached past.

use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{BindingKind, Component, ImageShape};

use super::image::{dispatch, filled, texels, view, Bound};
use crate::device_tests::{context, device, memory, over};
use crate::{
    Aspects, BufferViewDesc, Context, Extent3d, Format, ImageDesc, ImageDimension, ImageRange,
    ImageUsage, ImageViewDesc, Resource, ViewDimension,
};

const MODULE: &str = r#"OpCapability Shader
OpCapability ImageBuffer
OpCapability SampledBuffer
OpCapability StorageImageExtendedFormats
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %out %data %ubo %stexels %utexels %level %layer %sampled %push %wg %wguard %pv %pguard
OpExecutionMode %main LocalSize 16 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %data DescriptorSet 0
OpDecorate %data Binding 1
OpDecorate %ubo DescriptorSet 0
OpDecorate %ubo Binding 2
OpDecorate %stexels DescriptorSet 0
OpDecorate %stexels Binding 3
OpDecorate %utexels DescriptorSet 0
OpDecorate %utexels Binding 4
OpDecorate %level DescriptorSet 0
OpDecorate %level Binding 5
OpDecorate %layer DescriptorSet 0
OpDecorate %layer Binding 6
OpDecorate %sampled DescriptorSet 0
OpDecorate %sampled Binding 7
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %data_block Block
OpMemberDecorate %data_block 0 Offset 0
OpDecorate %sixteen_words ArrayStride 4
OpDecorate %ubo_block Block
OpMemberDecorate %ubo_block 0 Offset 0
OpDecorate %four_vecs ArrayStride 16
OpDecorate %push_block Block
OpMemberDecorate %push_block 0 Offset 0
OpDecorate %four_words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%v2int = OpTypeVector %int 2
%v3int = OpTypeVector %int 3
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%c0 = OpConstant %uint 0
%c2 = OpConstant %uint 2
%c4 = OpConstant %uint 4
%c16 = OpConstant %uint 16
%c100 = OpConstant %uint 100
%c300 = OpConstant %uint 300
%c600 = OpConstant %uint 600
%c700 = OpConstant %uint 700
%c800 = OpConstant %uint 800
%c900 = OpConstant %uint 900
%c1100 = OpConstant %uint 1100
%c1200 = OpConstant %uint 1200
%izero = OpConstant %int 0
%workgroup = OpConstant %uint 2
%shared_sem = OpConstant %uint 264
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%sixteen_words = OpTypeArray %uint %c16
%data_block = OpTypeStruct %sixteen_words
%data_ptr = OpTypePointer StorageBuffer %data_block
%four_vecs = OpTypeArray %v4uint %c4
%ubo_block = OpTypeStruct %four_vecs
%ubo_ptr = OpTypePointer Uniform %ubo_block
%uniform_word = OpTypePointer Uniform %uint
%storage_texels_t = OpTypeImage %uint Buffer 0 0 0 2 R32ui
%storage_texels_ptr = OpTypePointer UniformConstant %storage_texels_t
%uniform_texels_t = OpTypeImage %uint Buffer 0 0 0 1 Unknown
%uniform_texels_ptr = OpTypePointer UniformConstant %uniform_texels_t
%storage_2d = OpTypeImage %uint 2D 0 0 0 2 R32ui
%storage_2d_ptr = OpTypePointer UniformConstant %storage_2d
%storage_array = OpTypeImage %uint 2D 0 1 0 2 R32ui
%storage_array_ptr = OpTypePointer UniformConstant %storage_array
%sampled_2d = OpTypeImage %uint 2D 0 0 0 1 Unknown
%sampled_2d_ptr = OpTypePointer UniformConstant %sampled_2d
%four_words = OpTypeArray %uint %c4
%push_block = OpTypeStruct %four_words
%push_ptr = OpTypePointer PushConstant %push_block
%push_word = OpTypePointer PushConstant %uint
%four_plain = OpTypeArray %uint %c4
%wg_ptr = OpTypePointer Workgroup %four_plain
%wg_word = OpTypePointer Workgroup %uint
%private_ptr = OpTypePointer Private %four_plain
%private_word = OpTypePointer Private %uint
%function_ptr = OpTypePointer Function %four_plain
%function_word = OpTypePointer Function %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%out = OpVariable %block_ptr StorageBuffer
%data = OpVariable %data_ptr StorageBuffer
%ubo = OpVariable %ubo_ptr Uniform
%stexels = OpVariable %storage_texels_ptr UniformConstant
%utexels = OpVariable %uniform_texels_ptr UniformConstant
%level = OpVariable %storage_2d_ptr UniformConstant
%layer = OpVariable %storage_array_ptr UniformConstant
%sampled = OpVariable %sampled_2d_ptr UniformConstant
%push = OpVariable %push_ptr PushConstant
%wg = OpVariable %wg_ptr Workgroup
%wguard = OpVariable %wg_ptr Workgroup
%pv = OpVariable %private_ptr Private
%pguard = OpVariable %private_ptr Private
%main = OpFunction %void None %fn
%entry = OpLabel
%fv = OpVariable %function_ptr Function
%fguard = OpVariable %function_ptr Function
%g = OpLoad %v3uint %gid
%i = OpCompositeExtract %uint %g 0
%ii = OpBitcast %int %i
%x = OpUMod %uint %i %c4
%y = OpUDiv %uint %i %c4
%xi = OpBitcast %int %x
%yi = OpBitcast %int %y
%at2 = OpCompositeConstruct %v2int %xi %yi
%x2 = OpUMod %uint %i %c2
%half = OpUDiv %uint %i %c2
%y2 = OpUMod %uint %half %c2
%x2i = OpBitcast %int %x2
%y2i = OpBitcast %int %y2
%at3 = OpCompositeConstruct %v3int %x2i %y2i %yi
%v100 = OpIAdd %uint %i %c100
%dp = OpAccessChain %word_ptr %data %c0 %i
OpStore %dp %v100
%d = OpLoad %uint %dp
%up = OpAccessChain %uniform_word %ubo %c0 %y %x
%u = OpLoad %uint %up
%st = OpLoad %storage_texels_t %stexels
%v300 = OpIAdd %uint %i %c300
%v300v = OpCompositeConstruct %v4uint %v300 %c0 %c0 %c0
OpImageWrite %st %ii %v300v
%str = OpImageRead %v4uint %st %ii
%s = OpCompositeExtract %uint %str 0
%ut = OpLoad %uniform_texels_t %utexels
%utr = OpImageFetch %v4uint %ut %ii
%t = OpCompositeExtract %uint %utr 0
%li = OpLoad %storage_2d %level
%v600 = OpIAdd %uint %i %c600
%v600v = OpCompositeConstruct %v4uint %v600 %c0 %c0 %c0
OpImageWrite %li %at2 %v600v
%lr = OpImageRead %v4uint %li %at2
%l = OpCompositeExtract %uint %lr 0
%ai = OpLoad %storage_array %layer
%v700 = OpIAdd %uint %i %c700
%v700v = OpCompositeConstruct %v4uint %v700 %c0 %c0 %c0
OpImageWrite %ai %at3 %v700v
%ar = OpImageRead %v4uint %ai %at3
%a = OpCompositeExtract %uint %ar 0
%si = OpLoad %sampled_2d %sampled
%sr = OpImageFetch %v4uint %si %at2 Lod %izero
%f = OpCompositeExtract %uint %sr 0
%pp = OpAccessChain %push_word %push %c0 %i
%p = OpLoad %uint %pp
%v900 = OpIAdd %uint %x %c900
%ggp = OpAccessChain %wg_word %wguard %x
OpStore %ggp %v900
%gp = OpAccessChain %wg_word %wg %i
%v800 = OpIAdd %uint %i %c800
OpStore %gp %v800
OpControlBarrier %workgroup %workgroup %shared_sem
%wgv = OpLoad %uint %gp
%wgg = OpLoad %uint %ggp
%pgp = OpAccessChain %private_word %pguard %x
OpStore %pgp %v900
%ppv = OpAccessChain %private_word %pv %i
%v1100 = OpIAdd %uint %i %c1100
OpStore %ppv %v1100
%pvv = OpLoad %uint %ppv
%pgv = OpLoad %uint %pgp
%fgp = OpAccessChain %function_word %fguard %x
OpStore %fgp %v900
%fpv = OpAccessChain %function_word %fv %i
%v1200 = OpIAdd %uint %i %c1200
OpStore %fpv %v1200
%fvv = OpLoad %uint %fpv
%fgv = OpLoad %uint %fgp
%base = OpIMul %uint %i %stride
{stores}
OpReturn
OpFunctionEnd
"#;

/// What each lane writes, in order.
const READ: [&str; 14] = [
    "%d", "%u", "%s", "%t", "%l", "%a", "%f", "%p", "%wgv", "%wgg", "%pvv", "%pgv", "%fvv", "%fgv",
];

const SENTINEL: u32 = 0xa5a5_a5a5;

/// Where in the one allocation each buffer range starts.
const DATA: u64 = 256;
const UNIFORM: u64 = 512;
const STORAGE_TEXELS: u64 = 768;
const UNIFORM_TEXELS: u64 = 832;

/// The module, each lane storing what [`READ`] names in words of its own.
fn module() -> alloc::string::String {
    let mut constants = alloc::format!("%stride = OpConstant %uint {}\n", READ.len());
    let mut stores = alloc::string::String::new();
    for (k, value) in READ.iter().enumerate() {
        let _ = writeln!(constants, "%k{k} = OpConstant %uint {k}");
        let _ = writeln!(
            stores,
            "%o{k} = OpIAdd %uint %base %k{k}\n%q{k} = OpAccessChain %word_ptr %out %c0 %o{k}\nOpStore %q{k} {value}"
        );
    }
    MODULE
        .replacen(
            "%izero = OpConstant",
            &alloc::format!("{constants}%izero = OpConstant"),
            1,
        )
        .replacen("{stores}", &stores, 1)
}

/// Lane `i`'s words, as the robust rules say.
fn expected(i: u32) -> [u32; READ.len()] {
    let (x, y) = (i % 4, i / 4);
    let below = |limit: u32, value: u32| if i < limit { value } else { 0 };
    let in_level = x < 2 && y < 2;
    [
        below(8, 100 + i),
        below(8, 2000 + i),
        below(4, 300 + i),
        below(4, 500 + i),
        if in_level { 600 + i } else { 0 },
        below(4, 700 + i),
        if in_level { 3000 + x + 2 * y } else { 0 },
        below(4, 4000 + i),
        below(4, 800 + i),
        900 + x,
        below(4, 1100 + i),
        900 + x,
        below(4, 1200 + i),
        900 + x,
    ]
}

fn uint_shape(dim: Dim, arrayed: bool, format: ImageFormat) -> ImageShape {
    ImageShape {
        dim,
        arrayed,
        multisampled: false,
        component: Component::Uint,
        format,
    }
}

/// The images the shader reaches past: a storage image of three levels and
/// an array of three layers, each viewed at one, and a sampled image of two
/// levels viewed at its second.
struct Images {
    levelled: ImageDesc,
    layered: ImageDesc,
    levels: crate::Image,
    layers: crate::Image,
    level_view: crate::ImageView,
    layer_view: crate::ImageView,
    sampled_view: crate::ImageView,
    _held: [(crate::Allocation, Option<crate::Image>); 3],
}

fn images(context: &Context) -> Images {
    let levelled = ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::R32Uint,
        extent: Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: 3,
        array_layers: 1,
        samples: 1,
        usage: ImageUsage::STORAGE,
    };
    let layered = ImageDesc {
        extent: Extent3d {
            width: 2,
            height: 2,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 3,
        ..levelled
    };
    let sampled_desc = ImageDesc {
        mip_levels: 2,
        usage: ImageUsage::SAMPLED,
        ..levelled
    };
    let (level_memory, levels) = filled(context, &levelled, Aspects::COLOR, |_, _, _, _| SENTINEL);
    let (layer_memory, layers) = filled(context, &layered, Aspects::COLOR, |_, _, _, _| SENTINEL);
    let (sampled_memory, sampled) =
        filled(context, &sampled_desc, Aspects::COLOR, |level, _, x, y| {
            if level == 1 {
                3000 + x + 2 * y
            } else {
                SENTINEL
            }
        });
    let narrow = |dimension, base_level, base_layer| ImageViewDesc {
        format: Format::R32Uint,
        dimension,
        range: ImageRange {
            aspects: Aspects::COLOR,
            base_level,
            levels: 1,
            base_layer,
            layers: 1,
        },
    };
    Images {
        level_view: view(context, &levels, &narrow(ViewDimension::D2, 1, 0)),
        layer_view: view(context, &layers, &narrow(ViewDimension::D2Array, 0, 1)),
        sampled_view: view(context, &sampled, &narrow(ViewDimension::D2, 1, 0)),
        levelled,
        layered,
        levels,
        layers,
        _held: [
            (level_memory, None),
            (layer_memory, None),
            (sampled_memory, Some(sampled)),
        ],
    }
}

/// The words of `allocation`, `count` of them.
fn read_words(allocation: &crate::Allocation, count: usize) -> Vec<u32> {
    let mut bytes = alloc::vec![0u8; count * 4];
    allocation.read(0, &mut bytes).expect("readable");
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

/// `len` words of `words` from byte `offset` set to `first` on.
fn fill(words: &mut [u32], (offset, len): (u64, usize), first: u32) {
    let at = usize::try_from(offset / 4).expect("within the allocation");
    for (slot, value) in words.iter_mut().skip(at).take(len).zip(first..) {
        *slot = value;
    }
}

#[test]
fn an_access_past_every_kind_of_resource_reads_zero_and_writes_nothing() {
    let (device, _) = device();
    let context = context(&device);
    let held = memory(&context, 2048);
    let mut initial: Vec<u32> = alloc::vec![SENTINEL; 512];
    fill(&mut initial, (UNIFORM, 8), 2000);
    fill(&mut initial, (UNIFORM_TEXELS, 4), 500);
    let bytes: Vec<u8> = initial.iter().flat_map(|w| w.to_le_bytes()).collect();
    held.write(0, &bytes).expect("writable");
    let whole = over(&context, &held, 0, 2048);
    let texel_view = |offset| {
        let desc = BufferViewDesc {
            offset,
            size: 16,
            format: Format::R32Uint,
        };
        context
            .create_buffer_view(&whole, &desc)
            .expect("a texel buffer")
    };
    let (storage_texels, uniform_texels) = (texel_view(STORAGE_TEXELS), texel_view(UNIFORM_TEXELS));
    let images = images(&context);
    let range = |offset, size| Resource::Buffer {
        buffer: &whole,
        offset,
        size,
    };
    let push: Vec<u8> = (4000u32..4004).flat_map(u32::to_le_bytes).collect();
    let texel_buffer = |format| uint_shape(Dim::BUFFER, false, format);
    let out = dispatch(
        &context,
        &module(),
        &[
            Bound(BindingKind::StorageBuffer, alloc::vec![range(DATA, 32)]),
            Bound(BindingKind::UniformBuffer, alloc::vec![range(UNIFORM, 32)]),
            Bound(
                BindingKind::StorageTexelBuffer(texel_buffer(ImageFormat::R32UI)),
                alloc::vec![Resource::TexelBuffer(&storage_texels)],
            ),
            Bound(
                BindingKind::UniformTexelBuffer(texel_buffer(ImageFormat::UNKNOWN)),
                alloc::vec![Resource::TexelBuffer(&uniform_texels)],
            ),
            Bound(
                BindingKind::StorageImage(uint_shape(Dim::DIM_2D, false, ImageFormat::R32UI)),
                alloc::vec![Resource::Image(&images.level_view)],
            ),
            Bound(
                BindingKind::StorageImage(uint_shape(Dim::DIM_2D, true, ImageFormat::R32UI)),
                alloc::vec![Resource::Image(&images.layer_view)],
            ),
            Bound(
                BindingKind::SampledImage(uint_shape(Dim::DIM_2D, false, ImageFormat::UNKNOWN)),
                alloc::vec![Resource::Image(&images.sampled_view)],
            ),
        ],
        &push,
        16 * READ.len(),
    );
    for (lane, read) in (0..16u32).zip(out.chunks(READ.len())) {
        assert_eq!(read, expected(lane), "lane {lane} read {READ:?}");
    }

    let mut landed = initial;
    fill(&mut landed, (DATA, 8), 100);
    fill(&mut landed, (STORAGE_TEXELS, 4), 300);
    assert_eq!(
        read_words(&held, 512),
        landed,
        "nothing written past a range or a texel buffer"
    );
    let mut level = alloc::vec![SENTINEL; 16];
    level.extend([600, 601, 604, 605, SENTINEL]);
    let written = texels(&context, &images.levels, &images.levelled, Aspects::COLOR);
    assert_eq!(written, level, "only level 1 written, and only within it");
    let mut layer = alloc::vec![SENTINEL; 4];
    layer.extend([700, 701, 702, 703]);
    layer.extend([SENTINEL; 4]);
    let written = texels(&context, &images.layers, &images.layered, Aspects::COLOR);
    assert_eq!(written, layer, "only layer 1 written");
}

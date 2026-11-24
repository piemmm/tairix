use super::assemble::{admit, with};
use super::tests::FRAGMENT;
use crate::{SampleForms, Sampling, Stage};

/// A compute shader sampling a float image through one sampler in every
/// explicit form, and a depth image through another with a comparison.
const SEPARATE: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %img %depth %plain %cmp %out
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %img DescriptorSet 0
OpDecorate %img Binding 0
OpDecorate %plain DescriptorSet 0
OpDecorate %plain Binding 1
OpDecorate %cmp DescriptorSet 0
OpDecorate %cmp Binding 2
OpDecorate %depth DescriptorSet 0
OpDecorate %depth Binding 3
OpDecorate %out DescriptorSet 1
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%int = OpTypeInt 32 1
%v2float = OpTypeVector %float 2
%v3float = OpTypeVector %float 3
%v4float = OpTypeVector %float 4
%v2int = OpTypeVector %int 2
%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%depth_t = OpTypeImage %float 2D 1 0 0 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%depth_ptr = OpTypePointer UniformConstant %depth_t
%img = OpVariable %image_ptr UniformConstant
%depth = OpVariable %depth_ptr UniformConstant
%sampler_t = OpTypeSampler
%sampler_ptr = OpTypePointer UniformConstant %sampler_t
%plain = OpVariable %sampler_ptr UniformConstant
%cmp = OpVariable %sampler_ptr UniformConstant
%si_t = OpTypeSampledImage %image_t
%sd_t = OpTypeSampledImage %depth_t
%block = OpTypeStruct %v4float
%block_ptr = OpTypePointer StorageBuffer %block
%out = OpVariable %block_ptr StorageBuffer
%v4ptr = OpTypePointer StorageBuffer %v4float
%izero = OpConstant %int 0
%ione = OpConstant %int 1
%fzero = OpConstant %float 0
%fone = OpConstant %float 1
%at = OpConstantComposite %v2float %fzero %fzero
%at3 = OpConstantComposite %v3float %fzero %fzero %fone
%offset = OpConstantComposite %v2int %ione %izero
%main = OpFunction %void None %fn
%entry = OpLabel
%i = OpLoad %image_t %img
%d = OpLoad %depth_t %depth
%p = OpLoad %sampler_t %plain
%c = OpLoad %sampler_t %cmp
%ip = OpSampledImage %si_t %i %p
%dc = OpSampledImage %sd_t %d %c
%a = OpImageSampleExplicitLod %v4float %ip %at Lod %fzero
%b = OpImageSampleExplicitLod %v4float %ip %at Lod|ConstOffset %fzero %offset
%g = OpImageGather %v4float %ip %at %izero
%j = OpImageSampleProjExplicitLod %v4float %ip %at3 Lod %fzero
%r = OpImageSampleDrefExplicitLod %float %dc %at %fone Lod %fzero
%ab = OpFAdd %v4float %a %b
%gj = OpFAdd %v4float %g %j
%sum = OpFAdd %v4float %ab %gj
%rs = OpVectorTimesScalar %v4float %sum %r
%slot = OpAccessChain %v4ptr %out %izero
OpStore %slot %rs
OpReturn
OpFunctionEnd
"#;

fn samplings(text: &str, stage: Stage) -> alloc::vec::Vec<Sampling> {
    let admitted = admit(text).expect("admitted");
    let entry = admitted
        .entry_point("main", stage)
        .expect("the entry point");
    entry.samplings().to_vec()
}

#[test]
fn each_image_sampled_through_a_separate_sampler_is_recorded_once_with_every_form() {
    let read = SampleForms::READ
        .with(SampleForms::OFFSET)
        .with(SampleForms::GATHER)
        .with(SampleForms::PROJECTIVE);
    assert_eq!(
        samplings(SEPARATE, Stage::Compute),
        [
            Sampling {
                image: (0, 0),
                sampler: (0, 1),
                forms: read,
            },
            Sampling {
                image: (0, 3),
                sampler: (0, 2),
                forms: SampleForms::COMPARE,
            },
        ]
    );
}

#[test]
fn a_combined_image_sampler_is_its_own_sampler() {
    assert_eq!(
        samplings(FRAGMENT, Stage::Fragment),
        [Sampling {
            image: (0, 1),
            sampler: (0, 1),
            forms: SampleForms::READ.with(SampleForms::IMPLICIT),
        }]
    );
}

#[test]
fn a_bias_and_a_level_of_detail_query_are_recorded_as_implicit_sampling() {
    let module = with(
        FRAGMENT,
        "OpCapability Shader",
        "OpCapability Shader\nOpCapability ImageQuery",
    );
    let module = with(
        &module,
        "%main = OpFunction",
        "%bias = OpConstant %float 0.5\n%main = OpFunction",
    );
    let module = with(
        &module,
        "%s = OpImageSampleImplicitLod %v4 %t %c",
        "%s = OpImageSampleImplicitLod %v4 %t %c Bias %bias\n%lod = OpImageQueryLod %v2 %t %c",
    );
    assert_eq!(
        samplings(&module, Stage::Fragment),
        [Sampling {
            image: (0, 1),
            sampler: (0, 1),
            forms: SampleForms::READ
                .with(SampleForms::IMPLICIT)
                .with(SampleForms::BIAS),
        }]
    );
}

#[test]
fn a_fetch_or_a_query_samples_nothing() {
    let module = with(
        SEPARATE,
        "%a = OpImageSampleExplicitLod %v4float %ip %at Lod %fzero",
        "%a = OpImageFetch %v4float %i %offset Lod %izero",
    );
    let module = with(
        &module,
        "%b = OpImageSampleExplicitLod %v4float %ip %at Lod|ConstOffset %fzero %offset",
        "%b = OpImageFetch %v4float %i %offset Lod %izero",
    );
    let module = with(
        &module,
        "%g = OpImageGather %v4float %ip %at %izero",
        "%g = OpCopyObject %v4float %a",
    );
    let module = with(
        &module,
        "%j = OpImageSampleProjExplicitLod %v4float %ip %at3 Lod %fzero",
        "%j = OpCopyObject %v4float %b",
    );
    let module = with(
        &module,
        "%r = OpImageSampleDrefExplicitLod %float %dc %at %fone Lod %fzero",
        "%r = OpCopyObject %float %fone",
    );
    assert_eq!(samplings(&module, Stage::Compute), []);
}

/// A compute shader sampling two images, each through its own sampler, in a
/// function the images and samplers are passed to, and a third through a
/// function that names its own.
const CALLED: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %img %img2 %img3 %s1 %s2 %s3 %out
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %img DescriptorSet 0
OpDecorate %img Binding 0
OpDecorate %s1 DescriptorSet 0
OpDecorate %s1 Binding 1
OpDecorate %img2 DescriptorSet 0
OpDecorate %img2 Binding 2
OpDecorate %s2 DescriptorSet 0
OpDecorate %s2 Binding 3
OpDecorate %img3 DescriptorSet 2
OpDecorate %img3 Binding 0
OpDecorate %s3 DescriptorSet 2
OpDecorate %s3 Binding 1
OpDecorate %out DescriptorSet 1
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%int = OpTypeInt 32 1
%v2float = OpTypeVector %float 2
%v4float = OpTypeVector %float 4
%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%img = OpVariable %image_ptr UniformConstant
%img2 = OpVariable %image_ptr UniformConstant
%img3 = OpVariable %image_ptr UniformConstant
%sampler_t = OpTypeSampler
%sampler_ptr = OpTypePointer UniformConstant %sampler_t
%s1 = OpVariable %sampler_ptr UniformConstant
%s2 = OpVariable %sampler_ptr UniformConstant
%s3 = OpVariable %sampler_ptr UniformConstant
%si_t = OpTypeSampledImage %image_t
%block = OpTypeStruct %v4float
%block_ptr = OpTypePointer StorageBuffer %block
%out = OpVariable %block_ptr StorageBuffer
%v4ptr = OpTypePointer StorageBuffer %v4float
%izero = OpConstant %int 0
%fzero = OpConstant %float 0
%at = OpConstantComposite %v2float %fzero %fzero
%fn_sample = OpTypeFunction %v4float %image_ptr %sampler_ptr
%fn_own = OpTypeFunction %v4float
%sample = OpFunction %v4float None %fn_sample
%pi = OpFunctionParameter %image_ptr
%ps = OpFunctionParameter %sampler_ptr
%sl = OpLabel
%li = OpLoad %image_t %pi
%ls = OpLoad %sampler_t %ps
%si = OpSampledImage %si_t %li %ls
%v = OpImageSampleExplicitLod %v4float %si %at Lod %fzero
OpReturnValue %v
OpFunctionEnd
%own = OpFunction %v4float None %fn_own
%ol = OpLabel
%oi = OpLoad %image_t %img3
%os = OpLoad %sampler_t %s3
%osi = OpSampledImage %si_t %oi %os
%ov = OpImageGather %v4float %osi %at %izero
OpReturnValue %ov
OpFunctionEnd
%main = OpFunction %void None %fn
%entry = OpLabel
%x = OpFunctionCall %v4float %sample %img %s1
%y = OpFunctionCall %v4float %sample %img2 %s2
%z = OpFunctionCall %v4float %own
%xy = OpFAdd %v4float %x %y
%xyz = OpFAdd %v4float %xy %z
%slot = OpAccessChain %v4ptr %out %izero
OpStore %slot %xyz
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_callees_sampling_through_its_parameters_is_charged_to_what_each_call_passes() {
    let sampling = |image, sampler, forms| Sampling {
        image,
        sampler,
        forms,
    };
    assert_eq!(
        samplings(CALLED, Stage::Compute),
        [
            sampling((0, 0), (0, 1), SampleForms::READ),
            sampling((0, 2), (0, 3), SampleForms::READ),
            sampling((2, 0), (2, 1), SampleForms::READ.with(SampleForms::GATHER)),
        ]
    );
}

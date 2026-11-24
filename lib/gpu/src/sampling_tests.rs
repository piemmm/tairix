//! What a pipeline's entry points sample, held to their layout's sample and
//! sampler types.

use alloc::format;
use alloc::string::String;

use tairix_spirv::assemble::admit;
use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, Component, ImageShape, SampleForms, Stages};

use super::{DescriptorType, LayoutBinding, SampleType, SamplerType};
use crate::device_tests::{context, device};
use crate::{ComputePipelineDesc, Context, Error, PushConstants};

const READ_FLOAT: &str = "%v = OpImageSampleExplicitLod %v4float %sampled %at Lod %fzero";
const READ_UINT: &str = "%v = OpImageSampleExplicitLod %v4uint %sampled %at Lod %fzero";
const COMPARE: &str = "%v = OpImageSampleDrefExplicitLod %float %sampled %at %fzero Lod %fzero";
const OFFSET: &str =
    "%v = OpImageSampleExplicitLod %v4float %sampled %at Lod|ConstOffset %fzero %offset";
const GATHER: &str = "%v = OpImageGather %v4float %sampled %at %izero";
const PROJECTIVE: &str = "%v = OpImageSampleProjExplicitLod %v4float %sampled %at3 Lod %fzero";

/// A compute shader whose `%sampled` is `sampled` and which makes `%v` from
/// it by `sample`, `declarations` declaring its image and sampler.
fn shader(declarations: &str, sampled: &str, sample: &str) -> String {
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %img %smp
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %img DescriptorSet 0
OpDecorate %img Binding 0
OpDecorate %smp DescriptorSet 0
OpDecorate %smp Binding 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%v2float = OpTypeVector %float 2
%v3float = OpTypeVector %float 3
%v4float = OpTypeVector %float 4
%v4uint = OpTypeVector %uint 4
%v2int = OpTypeVector %int 2
{declarations}
%fzero = OpConstant %float 0
%fone = OpConstant %float 1
%izero = OpConstant %int 0
%ione = OpConstant %int 1
%at = OpConstantComposite %v2float %fzero %fzero
%at3 = OpConstantComposite %v3float %fzero %fzero %fone
%offset = OpConstantComposite %v2int %ione %izero
%main = OpFunction %void None %fn
%entry = OpLabel
{sampled}
{sample}
OpReturn
OpFunctionEnd
"#
    )
}

/// The image at binding 0, of `texel` and declared depth where `depth`,
/// sampled through the sampler at binding 1 by `sample`.
fn separate(texel: &str, depth: bool, sample: &str) -> String {
    let declarations = format!(
        "%image_t = OpTypeImage {texel} 2D {} 0 0 1 Unknown\n%image_ptr = OpTypePointer UniformConstant %image_t\n%img = OpVariable %image_ptr UniformConstant\n%sampler_t = OpTypeSampler\n%sampler_ptr = OpTypePointer UniformConstant %sampler_t\n%smp = OpVariable %sampler_ptr UniformConstant\n%si_t = OpTypeSampledImage %image_t",
        u32::from(depth)
    );
    let sampled =
        "%i = OpLoad %image_t %img\n%s = OpLoad %sampler_t %smp\n%sampled = OpSampledImage %si_t %i %s";
    shader(&declarations, sampled, sample)
}

/// A combined image sampler of `texel` at binding 0, sampled by `sample`;
/// the sampler at binding 1 is never sampled through.
fn combined(texel: &str, sample: &str) -> String {
    let declarations = format!(
        "%image_t = OpTypeImage {texel} 2D 0 0 0 1 Unknown\n%si_t = OpTypeSampledImage %image_t\n%si_ptr = OpTypePointer UniformConstant %si_t\n%img = OpVariable %si_ptr UniformConstant\n%sampler_t = OpTypeSampler\n%sampler_ptr = OpTypePointer UniformConstant %sampler_t\n%smp = OpVariable %sampler_ptr UniformConstant"
    );
    let sampled = "%sampled = OpLoad %si_t %img\n%unused = OpLoad %sampler_t %smp";
    shader(&declarations, sampled, sample)
}

const fn shape(component: Component) -> ImageShape {
    ImageShape {
        dim: Dim::DIM_2D,
        arrayed: false,
        multisampled: false,
        component,
        format: ImageFormat::UNKNOWN,
    }
}

const fn binding(binding: u32, kind: DescriptorType) -> LayoutBinding {
    LayoutBinding {
        binding,
        kind,
        count: 1,
        access: Access::READ,
        stages: Stages::COMPUTE,
    }
}

/// A compute pipeline of `text`'s `main` over one set of `bindings`.
fn pipeline(context: &Context, text: &str, bindings: &[LayoutBinding]) -> Result<(), Error> {
    let module = context
        .create_shader_module(admit(text).unwrap_or_else(|r| panic!("{r:?} in\n{text}")))
        .expect("a module");
    let set = context.create_bind_set_layout(bindings).expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set], PushConstants::NONE)
        .expect("a pipeline layout");
    context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .map(drop)
}

/// Each of `cases` — how a module samples, the image's sample type, the
/// sampler's type, whether the pipeline is made, and what it is — over an
/// image of `texel`, declared depth where `depth`, at binding 0 and a
/// separate sampler at binding 1.
fn separate_cases(texel: &str, depth: bool, cases: &[(&str, SampleType, SamplerType, bool, &str)]) {
    let (device, _) = device();
    let context = context(&device);
    let component = if texel == "%uint" {
        Component::Uint
    } else {
        Component::Float
    };
    for &(sample, image, sampler, made, what) in cases {
        let bindings = [
            binding(0, DescriptorType::SampledImage(shape(component), image)),
            binding(1, DescriptorType::Sampler(sampler)),
        ];
        let outcome = if made {
            Ok(())
        } else {
            Err(Error::Sampling((0, 0), (0, 1)))
        };
        assert_eq!(
            pipeline(&context, &separate(texel, depth, sample), &bindings),
            outcome,
            "{what}"
        );
    }
}

#[test]
fn a_separate_sampler_reads_float_colour_only_as_its_type_takes() {
    use SampleType as I;
    use SamplerType as S;
    separate_cases(
        "%float",
        false,
        &[
            (READ_FLOAT, I::Float, S::Filtering, true, "a filtered read"),
            (
                READ_FLOAT,
                I::UnfilterableFloat,
                S::NonFiltering,
                true,
                "a nearest read",
            ),
            (
                GATHER,
                I::UnfilterableFloat,
                S::NonFiltering,
                true,
                "a gather",
            ),
            (
                READ_FLOAT,
                I::UnfilterableFloat,
                S::Filtering,
                false,
                "filtering the unfiltered",
            ),
            (
                READ_FLOAT,
                I::Float,
                S::Comparison,
                false,
                "a read through a comparison",
            ),
            (
                READ_FLOAT,
                I::Float,
                S::Integer,
                false,
                "floats through an integer sampler",
            ),
        ],
    );
}

#[test]
fn a_separate_sampler_compares_or_reads_depth_as_its_type_takes() {
    use SampleType as I;
    use SamplerType as S;
    separate_cases(
        "%float",
        true,
        &[
            (
                COMPARE,
                I::Depth,
                S::Comparison,
                true,
                "a filtered comparison",
            ),
            (
                COMPARE,
                I::UnfilterableDepth,
                S::NonFilteringComparison,
                true,
                "a nearest comparison",
            ),
            (
                COMPARE,
                I::UnfilterableDepth,
                S::Comparison,
                false,
                "filtering depth unfiltered",
            ),
            (
                COMPARE,
                I::Depth,
                S::Filtering,
                false,
                "a comparison through a read",
            ),
            (READ_FLOAT, I::Depth, S::NonFiltering, true, "depth read"),
        ],
    );
}

#[test]
fn a_separate_sampler_reads_integers_only_through_an_integer_type() {
    use SampleType as I;
    use SamplerType as S;
    separate_cases(
        "%uint",
        false,
        &[
            (READ_UINT, I::Uint, S::Integer, true, "integers"),
            (
                READ_UINT,
                I::Uint,
                S::NonFiltering,
                false,
                "integers through a float sampler",
            ),
        ],
    );
}

#[test]
fn an_unnormalised_sampler_reads_at_an_explicit_level_with_no_offset_gather_or_projection() {
    let (device, _) = device();
    let context = context(&device);
    let bindings = [
        binding(
            0,
            DescriptorType::CombinedImageSampler(
                shape(Component::Float),
                SampleType::UnfilterableFloat,
                SamplerType::Unnormalized,
            ),
        ),
        binding(1, DescriptorType::Sampler(SamplerType::NonFiltering)),
    ];
    let refused = Err(Error::Sampling((0, 0), (0, 0)));
    for (sample, outcome, what) in [
        (READ_FLOAT, Ok(()), "an explicit read"),
        (OFFSET, refused, "an offset"),
        (GATHER, refused, "a gather"),
        (PROJECTIVE, refused, "a projection"),
    ] {
        assert_eq!(
            pipeline(&context, &combined("%float", sample), &bindings),
            outcome,
            "{what}"
        );
    }
}

#[test]
fn no_sampler_takes_both_comparisons_and_reads_and_an_unnormalised_one_no_derived_level() {
    let both = SampleForms::READ.with(SampleForms::COMPARE);
    for sampler in [
        SamplerType::Filtering,
        SamplerType::NonFiltering,
        SamplerType::Integer,
        SamplerType::Comparison,
        SamplerType::NonFilteringComparison,
        SamplerType::Unnormalized,
    ] {
        assert!(!sampler.takes(both), "{sampler:?}");
    }
    for forms in [SampleForms::IMPLICIT, SampleForms::BIAS] {
        assert!(!SamplerType::Unnormalized.takes(SampleForms::READ.with(forms)));
        assert!(SamplerType::Filtering.takes(SampleForms::READ.with(forms)));
    }
}

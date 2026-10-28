use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use super::allow::{class, Enabled, CAPABILITIES, EXTENSIONS};
use super::assemble::{admit, admit_version, assemble, refused, with, V1_6};
use crate::enumerant::{AddressingModel, Capability, Dim, ImageFormat};
use crate::grammar;
use crate::tables::{GLSL_STD_450, INSTRUCTIONS};
use crate::{
    Access, Binding, BindingKind, Component, Disagreement, ImageShape, Layout, Rule, Stage,
    Varying, Version, WorkgroupSize,
};
use crate::{Kind, Module, Op};

/// A compute shader doubling each float of a storage buffer.
pub(crate) const COMPUTE: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf
OpExecutionMode %main LocalSize 8 8 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %floats ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%in_v3 = OpTypePointer Input %v3uint
%gid = OpVariable %in_v3 Input
%float = OpTypeFloat 32
%floats = OpTypeRuntimeArray %float
%block = OpTypeStruct %floats
%sb_block = OpTypePointer StorageBuffer %block
%buf = OpVariable %sb_block StorageBuffer
%sb_float = OpTypePointer StorageBuffer %float
%zero = OpConstant %uint 0
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
%p = OpAccessChain %sb_float %buf %zero %x
%v = OpLoad %float %p
%w = OpFAdd %float %v %v
OpStore %p %w
OpReturn
OpFunctionEnd
"#;

/// A fragment shader sampling a texture at its interpolated coordinate.
pub(crate) const FRAGMENT: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %uv %colour %tex
OpExecutionMode %main OriginUpperLeft
OpDecorate %uv Location 0
OpDecorate %colour Location 0
OpDecorate %tex DescriptorSet 0
OpDecorate %tex Binding 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v2 = OpTypeVector %float 2
%v4 = OpTypeVector %float 4
%in_v2 = OpTypePointer Input %v2
%uv = OpVariable %in_v2 Input
%out_v4 = OpTypePointer Output %v4
%colour = OpVariable %out_v4 Output
%image = OpTypeImage %float 2D 0 0 0 1 Unknown
%sampled = OpTypeSampledImage %image
%uc_sampled = OpTypePointer UniformConstant %sampled
%tex = OpVariable %uc_sampled UniformConstant
%main = OpFunction %void None %fn
%entry = OpLabel
%t = OpLoad %sampled %tex
%c = OpLoad %v2 %uv
%s = OpImageSampleImplicitLod %v4 %t %c
%d = OpDPdx %v4 %s
OpStore %colour %d
OpReturn
OpFunctionEnd
"#;

/// A vertex shader passing its position through the built-in block.
pub(crate) const VERTEX: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %position %out %vertex
OpDecorate %position Location 0
OpDecorate %per_vertex Block
OpMemberDecorate %per_vertex 0 BuiltIn Position
OpMemberDecorate %per_vertex 1 BuiltIn PointSize
OpDecorate %vertex BuiltIn VertexIndex
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%int = OpTypeInt 32 1
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%position = OpVariable %in_v4 Input
%in_int = OpTypePointer Input %int
%vertex = OpVariable %in_int Input
%per_vertex = OpTypeStruct %v4 %float
%out_pv = OpTypePointer Output %per_vertex
%out = OpVariable %out_pv Output
%out_v4 = OpTypePointer Output %v4
%zero = OpConstant %int 0
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpLoad %v4 %position
%slot = OpAccessChain %out_v4 %out %zero
OpStore %slot %p
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_compute_shader_is_admitted_with_its_storage_buffer() {
    let admitted = admit(COMPUTE).expect("admitted");
    let [entry] = admitted.entry_points() else {
        panic!("one entry point");
    };
    assert_eq!(entry.name(), "main");
    assert_eq!(entry.stage(), Stage::Compute);
    assert_eq!(entry.workgroup(), Some(WorkgroupSize::Fixed([8, 8, 1])));
    assert_eq!(
        entry.bindings(),
        &[Binding {
            set: 0,
            binding: 0,
            kind: BindingKind::StorageBuffer,
            count: 1,
            access: Access::READ.with(Access::WRITE),
        }]
    );
    assert_eq!(entry.push_constants(), None);
    assert!(entry.inputs().is_empty() && entry.outputs().is_empty());
    assert!(admitted.entry_point("main", Stage::Compute).is_some());
    assert!(admitted.entry_point("main", Stage::Fragment).is_none());
}

#[test]
fn a_fragment_shader_is_admitted_with_its_texture_and_locations() {
    let admitted = admit(FRAGMENT).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Fragment)
        .expect("the entry point");
    let shape = ImageShape {
        dim: Dim::DIM_2D,
        arrayed: false,
        multisampled: false,
        component: Component::Float,
        format: ImageFormat::UNKNOWN,
    };
    assert_eq!(
        entry.bindings(),
        &[Binding {
            set: 0,
            binding: 1,
            kind: BindingKind::CombinedImageSampler(shape),
            count: 1,
            access: Access::READ,
        }]
    );
    let varying = |components| Varying {
        location: 0,
        component: 0,
        components,
        scalar: Component::Float,
        width: 32,
        index: 0,
    };
    assert_eq!(entry.inputs(), &[varying(2)]);
    assert_eq!(entry.outputs(), &[varying(4)]);
}

#[test]
fn a_vertex_shader_is_admitted_with_its_built_in_block() {
    let admitted = admit(VERTEX).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Vertex)
        .expect("the entry point");
    assert_eq!(entry.inputs().len(), 1);
    assert!(entry.outputs().is_empty());
}

fn layout(bindings: &[Binding]) -> Layout<'_> {
    Layout {
        bindings,
        push_constants: 0,
    }
}

#[test]
fn a_layout_providing_each_binding_agrees_and_one_missing_it_does_not() {
    let admitted = admit(COMPUTE).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Compute)
        .expect("the entry point");
    let buffer = Binding {
        set: 0,
        binding: 0,
        kind: BindingKind::StorageBuffer,
        count: 1,
        access: Access::READ.with(Access::WRITE),
    };
    assert_eq!(entry.agree(&layout(&[buffer])), Ok(()));
    assert_eq!(entry.agree(&layout(&[])), Err(Disagreement::Missing(0, 0)));
    let read_only = Binding {
        access: Access::READ,
        ..buffer
    };
    assert_eq!(
        entry.agree(&layout(&[read_only])),
        Err(Disagreement::Access(0, 0))
    );
    let uniform = Binding {
        kind: BindingKind::UniformBuffer,
        ..buffer
    };
    assert_eq!(
        entry.agree(&layout(&[uniform])),
        Err(Disagreement::Kind(0, 0))
    );
    let other = Binding {
        binding: 1,
        ..buffer
    };
    assert_eq!(
        entry.agree(&layout(&[other, buffer])),
        Err(Disagreement::Unordered)
    );
}

#[test]
fn physical_addressing_is_refused_whether_or_not_its_capability_is_declared() {
    let module = with(COMPUTE, "Logical GLSL450", "Physical64 GLSL450");
    assert_eq!(
        refused(&module),
        Rule::EnumerantNotEnabled(Kind::AddressingModel, AddressingModel::PHYSICAL64.0)
    );
    let module = with(
        &module,
        "OpCapability Shader",
        "OpCapability Shader\nOpCapability Addresses",
    );
    assert_eq!(refused(&module), Rule::Capability(Capability::ADDRESSES));
}

#[test]
fn the_capability_allow_list_is_ordered_and_closed_under_what_each_implies() {
    assert!(CAPABILITIES.windows(2).all(|w| w[0] < w[1]));
    for capability in CAPABILITIES {
        let implied = grammar::enumerant(Kind::Capability, capability.0).expect("recognised");
        for also in implied.gate.capabilities {
            assert!(
                CAPABILITIES.contains(also),
                "{capability:?} implies {also:?}"
            );
        }
    }
}

/// A module declaring every admitted capability and extension, as the
/// widest set the validator enables.
fn everything() -> Module {
    let mut text = String::new();
    for capability in CAPABILITIES {
        let _ = writeln!(text, "OpCapability {}", capability.0);
    }
    for extension in EXTENSIONS {
        let _ = writeln!(text, "OpExtension \"{extension}\"");
    }
    Module::decode(&assemble(&text, V1_6)).expect("decoded")
}

#[test]
fn every_instruction_the_allow_list_enables_is_checked_or_refused_by_name() {
    let module = everything();
    let enabled = Enabled::collect(&module).expect("collected");
    let unchecked: Vec<Op> = INSTRUCTIONS
        .iter()
        .filter(|spec| enabled_spec(&enabled, spec) && class(spec.op).is_none())
        .map(|spec| spec.op)
        .collect();
    assert_eq!(
        unchecked,
        [
            Op::DECORATION_GROUP,
            Op::GROUP_DECORATE,
            Op::GROUP_MEMBER_DECORATE,
            Op::PTR_EQUAL,
            Op::PTR_NOT_EQUAL,
            Op::DECORATE_STRING,
            Op::MEMBER_DECORATE_STRING,
        ]
    );
}

fn enabled_spec(enabled: &Enabled, spec: &grammar::Spec) -> bool {
    enabled.enables(&spec.gate)
}

#[test]
fn every_glsl_instruction_the_allow_list_enables_has_a_rule_but_the_removed_imix() {
    let module = everything();
    let enabled = Enabled::collect(&module).expect("collected");
    for inst in &GLSL_STD_450 {
        let admitted = inst.capabilities.iter().all(|&c| enabled.has(c));
        let ruled = super::glsl::rule_of(inst.number).is_some();
        assert_eq!(
            ruled,
            admitted && inst.number != 47,
            "GLSL.std.450 {}",
            inst.number
        );
    }
}

#[test]
fn a_capability_outside_the_allow_list_is_refused() {
    let module = with(
        COMPUTE,
        "OpCapability Shader",
        "OpCapability Shader\nOpCapability Int8",
    );
    assert!(matches!(refused(&module), Rule::Capability(_)));
}

#[test]
fn a_version_before_the_vulkan_one_one_baseline_is_refused() {
    let old = admit_version(COMPUTE, Version { major: 1, minor: 2 }).expect_err("refused");
    assert_eq!(old.rule, Rule::Version);
}

#[test]
fn an_operand_of_the_wrong_type_is_refused() {
    let module = with(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%w = OpFAdd %float %v %x",
    );
    assert!(matches!(refused(&module), Rule::OperandType(_)));
}

#[test]
fn a_conditional_branch_without_a_merge_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "%cond = OpFOrdLessThan %bool %v %w\nOpBranchConditional %cond %yes %no\n%yes = OpLabel\nOpStore %p %w\nOpReturn\n%no = OpLabel\nOpReturn",
    );
    let module = with(
        &module,
        "%float = OpTypeFloat 32",
        "%float = OpTypeFloat 32\n%bool = OpTypeBool",
    );
    assert_eq!(refused(&module), Rule::Merge);
}

//! Modules the validator must admit: each a shape front ends emit, so a
//! false refusal is found here rather than in a consumer.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::assemble::{admit, admit_version};
use crate::{Access, BindingKind, Component, Stage, Varying, Version};

/// The declarations a storage-buffer compute shader shares; `{body}` is the
/// function's body after its first label.
fn compute(extra_types: &str, body: &str) -> String {
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %uints ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%uints = OpTypeRuntimeArray %uint
%block = OpTypeStruct %uints
%sb_block = OpTypePointer StorageBuffer %block
%buf = OpVariable %sb_block StorageBuffer
%sb_uint = OpTypePointer StorageBuffer %uint
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%three = OpConstant %uint 3
%ten = OpConstant %uint 10
{extra_types}
%main = OpFunction %void None %fn
%entry = OpLabel
{body}
OpFunctionEnd
"#
    )
}

#[test]
fn a_counted_loop_with_a_phi_a_break_and_a_continue_block_is_admitted() {
    let module = compute(
        "",
        r"
OpBranch %header
%header = OpLabel
%i = OpPhi %uint %zero %entry %next %continue
OpLoopMerge %merge %continue None
OpBranch %cond
%cond = OpLabel
%lt = OpULessThan %bool %i %ten
OpBranchConditional %lt %body %merge
%body = OpLabel
%is3 = OpIEqual %bool %i %three
OpSelectionMerge %after None
OpBranchConditional %is3 %break %after
%break = OpLabel
OpBranch %merge
%after = OpLabel
%p = OpAccessChain %sb_uint %buf %zero %i
OpStore %p %i
OpBranch %continue
%continue = OpLabel
%next = OpIAdd %uint %i %one
OpBranch %header
%merge = OpLabel
OpReturn",
    );
    admit(&module).expect("admitted");
}

#[test]
fn a_do_while_loop_whose_back_edge_block_branches_to_the_merge_is_admitted() {
    let module = compute(
        "",
        r"
OpBranch %header
%header = OpLabel
%i = OpPhi %uint %zero %entry %next %latch
OpLoopMerge %merge %latch None
OpBranch %latch
%latch = OpLabel
%next = OpIAdd %uint %i %one
%again = OpULessThan %bool %next %ten
OpBranchConditional %again %header %merge
%merge = OpLabel
OpReturn",
    );
    admit(&module).expect("admitted");
}

#[test]
fn a_switch_whose_first_case_falls_through_to_the_next_is_admitted() {
    let module = compute(
        "",
        r"
%p = OpAccessChain %sb_uint %buf %zero %zero
%sel = OpLoad %uint %p
OpSelectionMerge %merge None
OpSwitch %sel %default 0 %case0 1 %case1
%case0 = OpLabel
OpStore %p %one
OpBranch %case1
%case1 = OpLabel
OpStore %p %three
OpBranch %merge
%default = OpLabel
OpBranch %merge
%merge = OpLabel
OpReturn",
    );
    admit(&module).expect("admitted");
}

#[test]
fn a_call_through_a_pointer_parameter_charges_the_callers_variable() {
    let module = compute(
        r"
%fn_uint = OpTypePointer Function %uint
%helper_type = OpTypeFunction %uint %fn_uint",
        r"
%local = OpVariable %fn_uint Function
OpStore %local %three
%r = OpFunctionCall %uint %helper %local
%p = OpAccessChain %sb_uint %buf %zero %zero
OpStore %p %r
OpReturn
OpFunctionEnd
%helper = OpFunction %uint None %helper_type
%param = OpFunctionParameter %fn_uint
%h = OpLabel
%v = OpLoad %uint %param
%w = OpIAdd %uint %v %one
OpStore %param %w
OpReturnValue %w",
    );
    admit(&module).expect("admitted");
}

#[test]
fn atomics_barriers_and_workgroup_memory_are_admitted_in_compute() {
    let module = compute(
        r"
%wg_uint = OpTypePointer Workgroup %uint
%shared = OpVariable %wg_uint Workgroup
%device = OpConstant %uint 1
%workgroup = OpConstant %uint 2
%relaxed = OpConstant %uint 0
%release_workgroup = OpConstant %uint 0x108",
        r"
%p = OpAccessChain %sb_uint %buf %zero %zero
%old = OpAtomicIAdd %uint %p %device %relaxed %one
OpStore %shared %old
OpControlBarrier %workgroup %workgroup %release_workgroup
%seen = OpLoad %uint %shared
OpAtomicStore %p %device %relaxed %seen
OpReturn",
    )
    .replace("\"main\" %buf", "\"main\" %buf %shared");
    admit(&module).expect("admitted");
}

#[test]
fn a_subgroup_reduction_is_admitted_with_its_capabilities() {
    let module = compute(
        "%subgroup = OpConstant %uint 3",
        r"
%p = OpAccessChain %sb_uint %buf %zero %zero
%v = OpLoad %uint %p
%sum = OpGroupNonUniformIAdd %uint %subgroup Reduce %v
OpStore %p %sum
OpReturn",
    )
    .replace(
        "OpCapability Shader",
        "OpCapability Shader\nOpCapability GroupNonUniform\nOpCapability GroupNonUniformArithmetic",
    );
    admit(&module).expect("admitted");
}

#[test]
fn a_specialization_constant_and_its_operation_are_admitted() {
    let module = compute(
        r"
%spec = OpSpecConstant %uint 4
%sum = OpSpecConstantOp %uint IAdd %spec %one",
        r"
%p = OpAccessChain %sb_uint %buf %zero %zero
OpStore %p %sum
OpReturn",
    )
    .replace(
        "OpDecorate %block Block",
        "OpDecorate %block Block\nOpDecorate %spec SpecId 0",
    );
    admit(&module).expect("admitted");
}

#[test]
fn a_uniform_block_laid_out_by_std140_and_push_constants_are_admitted() {
    let module = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %colour %ubo %push
OpExecutionMode %main OriginUpperLeft
OpDecorate %colour Location 0
OpDecorate %ubo DescriptorSet 0
OpDecorate %ubo Binding 2
OpDecorate %uniforms Block
OpMemberDecorate %uniforms 0 Offset 0
OpMemberDecorate %uniforms 0 ColMajor
OpMemberDecorate %uniforms 0 MatrixStride 16
OpMemberDecorate %uniforms 1 Offset 64
OpMemberDecorate %uniforms 2 Offset 76
OpMemberDecorate %uniforms 3 Offset 80
OpDecorate %v4s ArrayStride 16
OpDecorate %constants Block
OpMemberDecorate %constants 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%int = OpTypeInt 32 1
%v3 = OpTypeVector %float 3
%v4 = OpTypeVector %float 4
%m4 = OpTypeMatrix %v4 4
%two = OpConstant %int 2
%v4s = OpTypeArray %v4 %two
%uniforms = OpTypeStruct %m4 %v3 %float %v4s
%u_uniforms = OpTypePointer Uniform %uniforms
%ubo = OpVariable %u_uniforms Uniform
%u_v4 = OpTypePointer Uniform %v4
%constants = OpTypeStruct %v4
%pc_constants = OpTypePointer PushConstant %constants
%push = OpVariable %pc_constants PushConstant
%pc_v4 = OpTypePointer PushConstant %v4
%out_v4 = OpTypePointer Output %v4
%colour = OpVariable %out_v4 Output
%three = OpConstant %int 3
%zero = OpConstant %int 0
%one = OpConstant %int 1
%main = OpFunction %void None %fn
%entry = OpLabel
%a = OpAccessChain %u_v4 %ubo %three %one
%b = OpAccessChain %pc_v4 %push %zero
%x = OpLoad %v4 %a
%y = OpLoad %v4 %b
%z = OpFMul %v4 %x %y
OpStore %colour %z
OpReturn
OpFunctionEnd
"#;
    let admitted = admit(module).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Fragment)
        .expect("the entry point");
    assert_eq!(entry.push_constants(), Some(16));
    let [binding] = entry.bindings() else {
        panic!("one binding");
    };
    assert_eq!(
        (binding.kind, binding.access),
        (BindingKind::UniformBuffer, Access::READ)
    );
}

#[test]
fn a_storage_image_written_by_a_compute_shader_is_charged_a_write_only() {
    let module = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %out
OpExecutionMode %main LocalSize 8 8 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %out NonReadable
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%v2int = OpTypeVector %int 2
%v3uint = OpTypeVector %uint 3
%v2uint = OpTypeVector %uint 2
%v4 = OpTypeVector %float 4
%in_v3 = OpTypePointer Input %v3uint
%gid = OpVariable %in_v3 Input
%image = OpTypeImage %float 2D 0 0 0 2 Rgba8
%uc_image = OpTypePointer UniformConstant %image
%out = OpVariable %uc_image UniformConstant
%half = OpConstant %float 0x3f000000
%grey = OpConstantComposite %v4 %half %half %half %half
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%xy = OpVectorShuffle %v2uint %id %id 0 1
%coord = OpBitcast %v2int %xy
%img = OpLoad %image %out
OpImageWrite %img %coord %grey
OpReturn
OpFunctionEnd
"#;
    let admitted = admit(module).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Compute)
        .expect("the entry point");
    let [binding] = entry.bindings() else {
        panic!("one binding");
    };
    assert!(matches!(binding.kind, BindingKind::StorageImage(_)));
    assert_eq!(binding.access, Access::WRITE);
}

#[test]
fn glsl_std_450_and_a_vertex_matrix_input_are_admitted() {
    let module = r#"
OpCapability Shader
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %transform %weight %out
OpDecorate %transform Location 0
OpDecorate %weight Location 4
OpDecorate %per_vertex Block
OpMemberDecorate %per_vertex 0 BuiltIn Position
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%int = OpTypeInt 32 1
%v4 = OpTypeVector %float 4
%m4 = OpTypeMatrix %v4 4
%in_m4 = OpTypePointer Input %m4
%transform = OpVariable %in_m4 Input
%in_float = OpTypePointer Input %float
%weight = OpVariable %in_float Input
%per_vertex = OpTypeStruct %v4
%out_pv = OpTypePointer Output %per_vertex
%out = OpVariable %out_pv Output
%out_v4 = OpTypePointer Output %v4
%zero = OpConstant %int 0
%main = OpFunction %void None %fn
%entry = OpLabel
%m = OpLoad %m4 %transform
%w = OpLoad %float %weight
%root = OpExtInst %float %glsl Sqrt %w
%column = OpCompositeExtract %v4 %m 3
%scaled = OpVectorTimesScalar %v4 %column %root
%unit = OpExtInst %v4 %glsl Normalize %scaled
%slot = OpAccessChain %out_v4 %out %zero
OpStore %slot %unit
OpReturn
OpFunctionEnd
"#;
    let admitted = admit(module).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Vertex)
        .expect("the entry point");
    let locations: Vec<u32> = entry.inputs().iter().map(|v| v.location).collect();
    assert_eq!(locations, [0, 1, 2, 3, 4]);
}

#[test]
fn a_flat_integer_input_and_a_written_depth_are_admitted_in_a_fragment_shader() {
    let module = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %id %depth
OpExecutionMode %main OriginUpperLeft
OpExecutionMode %main DepthReplacing
OpDecorate %id Location 1
OpDecorate %id Flat
OpDecorate %depth BuiltIn FragDepth
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%uint = OpTypeInt 32 0
%bool = OpTypeBool
%in_uint = OpTypePointer Input %uint
%id = OpVariable %in_uint Input
%out_float = OpTypePointer Output %float
%depth = OpVariable %out_float Output
%zero = OpConstant %uint 0
%near = OpConstant %float 0
%main = OpFunction %void None %fn
%entry = OpLabel
%v = OpLoad %uint %id
%none = OpIEqual %bool %v %zero
OpSelectionMerge %merge None
OpBranchConditional %none %discard %merge
%discard = OpLabel
OpTerminateInvocation
%merge = OpLabel
OpStore %depth %near
OpReturn
OpFunctionEnd
"#;
    let admitted = admit(module).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Fragment)
        .expect("the entry point");
    assert_eq!(
        entry.inputs(),
        &[Varying {
            location: 1,
            component: 0,
            components: 1,
            scalar: Component::Uint,
            width: 32,
            index: 0,
        }]
    );
}

#[test]
fn a_one_point_three_module_lists_only_its_inputs_and_outputs() {
    let module = super::tests::COMPUTE.replace("\"main\" %gid %buf", "\"main\" %gid");
    admit_version(&module, Version { major: 1, minor: 3 }).expect("admitted");
}

#[test]
fn glslangs_vertex_shader_is_admitted_with_its_latent_clip_and_cull_distances() {
    for minor in [3, 6] {
        admit_version(
            super::refusal_tests::GLSLANG_VERTEX,
            Version { major: 1, minor },
        )
        .expect("admitted");
    }
}

#[test]
fn a_matrix_in_a_uniform_block_needs_only_its_columns_alignment() {
    let module = |stride: u32| {
        format!(
            r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %colour %ubo
OpExecutionMode %main OriginUpperLeft
OpDecorate %colour Location 0
OpDecorate %ubo DescriptorSet 0
OpDecorate %ubo Binding 0
OpDecorate %uniforms Block
OpMemberDecorate %uniforms 0 Offset 0
OpMemberDecorate %uniforms 0 ColMajor
OpMemberDecorate %uniforms 0 MatrixStride {stride}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v2 = OpTypeVector %float 2
%v4 = OpTypeVector %float 4
%m2 = OpTypeMatrix %v2 2
%uniforms = OpTypeStruct %m2
%u_uniforms = OpTypePointer Uniform %uniforms
%ubo = OpVariable %u_uniforms Uniform
%u_v2 = OpTypePointer Uniform %v2
%out_v4 = OpTypePointer Output %v4
%colour = OpVariable %out_v4 Output
%int = OpTypeInt 32 1
%zero = OpConstant %int 0
%main = OpFunction %void None %fn
%entry = OpLabel
%a = OpAccessChain %u_v2 %ubo %zero %zero
%x = OpLoad %v2 %a
%c = OpVectorShuffle %v4 %x %x 0 1 0 1
OpStore %colour %c
OpReturn
OpFunctionEnd
"#
        )
    };
    admit(&module(8)).expect("admitted");
    admit(&module(16)).expect("admitted");
    assert_eq!(
        admit(&module(4)).map(|_| ()).map_err(|r| r.rule),
        Err(crate::Rule::ExplicitLayout)
    );
}

/// A storage buffer of `class` whose two members are arrays of two `mat2`s,
/// laid out by `strides`: each member's matrix stride, and the arrays'.
fn matrix_arrays(class: &str, strides: (u32, u32, u32)) -> String {
    let (first, second, array) = strides;
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpDecorate %m2s ArrayStride {array}
OpMemberDecorate %block 0 Offset 0
OpMemberDecorate %block 0 ColMajor
OpMemberDecorate %block 0 MatrixStride {first}
OpMemberDecorate %block 1 Offset 256
OpMemberDecorate %block 1 ColMajor
OpMemberDecorate %block 1 MatrixStride {second}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%two = OpConstant %uint 2
%zero = OpConstant %uint 0
%float = OpTypeFloat 32
%v2 = OpTypeVector %float 2
%m2 = OpTypeMatrix %v2 2
%m2s = OpTypeArray %m2 %two
%block = OpTypeStruct %m2s %m2s
%ptr_block = OpTypePointer {class} %block
%buf = OpVariable %ptr_block {class}
%ptr_v2 = OpTypePointer {class} %v2
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpAccessChain %ptr_v2 %buf %zero %zero %zero
%v = OpLoad %v2 %p
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn arrays_of_matrices_are_laid_out_for_each_members_matrix() {
    admit(&matrix_arrays("StorageBuffer", (8, 8, 16))).expect("admitted");
    admit(&matrix_arrays("Uniform", (8, 8, 16))).expect("admitted");
    let refused = |module: &str| admit(module).map(|_| ()).map_err(|r| r.rule);
    let layout = Err(crate::Rule::ExplicitLayout);
    // A matrix overlapping the next element of its array.
    assert_eq!(refused(&matrix_arrays("StorageBuffer", (8, 8, 8))), layout);
    // An array stride a uniform block's rounding to sixteen does not divide.
    assert_eq!(refused(&matrix_arrays("Uniform", (8, 8, 8))), layout);
    // The second member's wider matrix no longer fits the array the first
    // one's laid out.
    assert_eq!(
        refused(&matrix_arrays("StorageBuffer", (8, 16, 16))),
        layout
    );
}

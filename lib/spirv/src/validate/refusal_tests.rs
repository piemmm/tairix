//! The hostile corpus: each rule broken in an otherwise valid module, and
//! the refusal it earns — never a module.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use super::assemble::{admit_version, refused, with};
use super::tests::{COMPUTE, FRAGMENT, VERTEX};
use crate::enumerant::{Capability, Decoration, Dim, ExecutionMode};
use crate::{Kind, Limit, Op, Rule, Version};

/// `COMPUTE` with `line` added after the line holding `anchor`.
fn after(text: &str, anchor: &str, line: &str) -> String {
    with(text, anchor, &format!("{anchor}\n{line}"))
}

#[test]
fn a_capability_not_admitted_names_itself() {
    let module = after(COMPUTE, "OpCapability Shader", "OpCapability Int8");
    assert_eq!(refused(&module), Rule::Capability(Capability::INT8));
}

#[test]
fn an_extension_not_admitted_is_refused() {
    let module = after(
        COMPUTE,
        "OpCapability Shader",
        "OpExtension \"SPV_KHR_variable_pointers\"",
    );
    assert_eq!(refused(&module), Rule::Extension);
}

#[test]
fn an_instruction_newer_than_the_module_is_refused() {
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%c = OpCopyLogical %block %w",
    );
    let old = admit_version(&module, Version { major: 1, minor: 3 }).expect_err("refused");
    assert_eq!(old.rule, Rule::InstructionNotEnabled(Op::COPY_LOGICAL));
}

#[test]
fn an_enumerant_no_declared_capability_enables_is_refused() {
    let module = after(
        COMPUTE,
        "%float = OpTypeFloat 32",
        "%line = OpTypeImage %float 1D 0 0 0 1 Unknown",
    );
    assert_eq!(
        refused(&module),
        Rule::EnumerantNotEnabled(Kind::Dim, Dim::DIM_1D.0)
    );
}

#[test]
fn pointer_comparison_and_decoration_groups_are_refused_by_name() {
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%same = OpPtrEqual %bool %p %p",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert_eq!(refused(&module), Rule::NotAdmitted(Op::PTR_EQUAL));
    let module = after(
        COMPUTE,
        "OpDecorate %block Block",
        "%group = OpDecorationGroup",
    );
    assert_eq!(refused(&module), Rule::NotAdmitted(Op::DECORATION_GROUP));
}

#[test]
fn the_simple_memory_model_is_refused() {
    let module = with(COMPUTE, "Logical GLSL450", "Logical Simple");
    assert_eq!(refused(&module), Rule::MemoryModel);
}

#[test]
fn instructions_out_of_section_order_are_refused() {
    let module = after(
        COMPUTE,
        "%void = OpTypeVoid",
        "OpDecorate %uint RelaxedPrecision",
    );
    assert_eq!(refused(&module), Rule::Layout);
    let module = after(
        COMPUTE,
        "OpMemoryModel Logical GLSL450",
        "OpMemoryModel Logical GLSL450",
    );
    assert_eq!(refused(&module), Rule::Layout);
    let module = after(
        COMPUTE,
        "OpExecutionMode %main LocalSize 8 8 1",
        "OpSourceContinued \"more\"",
    );
    assert_eq!(refused(&module), Rule::Layout);
}

#[test]
fn an_execution_mode_naming_anything_but_an_entry_point_is_refused() {
    let module = after(
        COMPUTE,
        "OpExecutionMode %main LocalSize 8 8 1",
        "OpExecutionMode %fn LocalSize 8 8 1",
    );
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::LOCAL_SIZE)
    );
    let helper = "OpFunctionEnd\n%helper = OpFunction %void None %fn\n%helper_entry = OpLabel\nOpReturn\nOpFunctionEnd";
    let module = with(COMPUTE, "OpFunctionEnd", helper);
    super::assemble::admit(&module).expect("admitted");
    let module = after(
        &module,
        "OpExecutionMode %main LocalSize 8 8 1",
        "OpExecutionMode %helper LocalSize 8 8 1",
    );
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::LOCAL_SIZE)
    );
}

#[test]
fn more_execution_modes_than_the_bound_on_one_entry_point_are_refused() {
    let anchor = "OpExecutionMode %main LocalSize 8 8 1";
    let module = after(
        COMPUTE,
        anchor,
        &format!("{anchor}\n").repeat(crate::MAX_MODES),
    );
    assert_eq!(refused(&module), Rule::Limit(Limit::Modes));
}

#[test]
fn a_module_with_no_entry_point_is_refused() {
    let module = with(
        COMPUTE,
        "OpEntryPoint GLCompute %main \"main\" %gid %buf\n",
        "",
    );
    let module = with(&module, "OpExecutionMode %main LocalSize 8 8 1\n", "");
    assert_eq!(refused(&module), Rule::NoEntryPoint);
}

#[test]
fn a_string_at_the_bound_in_characters_is_admitted_and_past_it_refused() {
    let named = |name: String| {
        after(
            COMPUTE,
            "OpExecutionMode %main LocalSize 8 8 1",
            &format!("OpName %main \"{name}\""),
        )
    };
    // Two bytes a character, so its bytes alone are past the bound.
    let wide = "\u{e9}".repeat(crate::MAX_STRING);
    super::assemble::admit(&named(wide)).expect("admitted");
    assert_eq!(
        refused(&named("n".repeat(crate::MAX_STRING + 1))),
        Rule::Limit(Limit::String)
    );
}

#[test]
fn an_access_chain_or_extraction_past_the_index_bound_is_refused() {
    let indexes = " %zero".repeat(crate::MAX_INDEXES + 1);
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        &format!("%q = OpAccessChain %sb_float %buf{indexes}"),
    );
    assert_eq!(refused(&module), Rule::Limit(Limit::Indexes));
    let literals = " 0".repeat(crate::MAX_INDEXES + 1);
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        &format!("%e = OpCompositeExtract %uint %id{literals}"),
    );
    assert_eq!(refused(&module), Rule::Limit(Limit::Indexes));
}

#[test]
fn a_location_past_the_bound_is_refused() {
    let last = crate::MAX_LOCATIONS - 1;
    let module = with(
        FRAGMENT,
        "OpDecorate %uv Location 0",
        &format!("OpDecorate %uv Location {last}"),
    );
    super::assemble::admit(&module).expect("admitted");
    let module = with(
        FRAGMENT,
        "OpDecorate %uv Location 0",
        &format!("OpDecorate %uv Location {}", last + 1),
    );
    assert_eq!(refused(&module), Rule::Limit(Limit::Locations));
}

#[test]
fn an_input_array_reaching_past_the_location_bound_is_refused() {
    let array = |length: u32| {
        let module = with(FRAGMENT, "%main \"main\" %uv", "%main \"main\" %many %uv");
        let module = after(
            &module,
            "OpDecorate %uv Location 0",
            "OpDecorate %many Location 1",
        );
        let declared = format!(
            "%uint = OpTypeInt 32 0\n%length = OpConstant %uint {length}\n%v4s = OpTypeArray %v4 %length\n%in_v4s = OpTypePointer Input %v4s\n%many = OpVariable %in_v4s Input"
        );
        after(&module, "%uv = OpVariable %in_v2 Input", &declared)
    };
    super::assemble::admit(&array(crate::MAX_LOCATIONS - 1)).expect("admitted");
    assert_eq!(
        refused(&array(crate::MAX_LOCATIONS)),
        Rule::Limit(Limit::Locations)
    );
}

#[test]
fn a_type_nested_to_the_bound_is_admitted_and_past_it_refused() {
    let nested = |levels: usize| {
        let mut types = String::new();
        let mut inner = String::from("%float");
        for level in 0..levels {
            let _ = writeln!(types, "%nest{level} = OpTypeArray {inner} %two");
            inner = format!("%nest{level}");
        }
        after(
            COMPUTE,
            "%zero = OpConstant %uint 0",
            &format!("%two = OpConstant %uint 2\n{types}"),
        )
    };
    let bound = usize::from(crate::MAX_TYPE_DEPTH);
    super::assemble::admit(&nested(bound)).expect("admitted");
    assert_eq!(refused(&nested(bound + 1)), Rule::Limit(Limit::TypeDepth));
}

#[test]
fn an_id_nothing_defines_is_refused() {
    let module = with(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%w = OpFAdd %float %v %nothing",
    );
    assert!(matches!(refused(&module), Rule::Undefined(_)));
}

#[test]
fn a_constant_naming_a_later_one_is_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%pair = OpConstantComposite %v3uint %zero %later %zero\n%later = OpConstant %uint 9",
    );
    assert!(matches!(refused(&module), Rule::ForwardReference(_)));
}

#[test]
fn a_use_its_definition_does_not_dominate_is_refused() {
    let module = with(
        COMPUTE,
        "%w = OpFAdd %float %v %v\nOpStore %p %w\nOpReturn",
        "%c = OpFOrdLessThan %bool %v %v\nOpSelectionMerge %m None\nOpBranchConditional %c %a %b\n%a = OpLabel\n%w = OpFAdd %float %v %v\nOpBranch %m\n%b = OpLabel\nOpStore %p %w\nOpBranch %m\n%m = OpLabel\nOpReturn",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert!(matches!(refused(&module), Rule::NotDominated(_)));
}

#[test]
fn malformed_and_duplicate_types_are_refused() {
    let module = after(
        COMPUTE,
        "%float = OpTypeFloat 32",
        "%v5 = OpTypeVector %float 5",
    );
    assert_eq!(refused(&module), Rule::Type);
    let module = after(COMPUTE, "%uint = OpTypeInt 32 0", "%again = OpTypeInt 32 0");
    assert_eq!(refused(&module), Rule::DuplicateType);
    let module = after(COMPUTE, "%float = OpTypeFloat 32", "%half = OpTypeFloat 16");
    assert_eq!(refused(&module), Rule::Type);
}

#[test]
fn an_array_sized_by_a_specialization_constant_or_by_zero_is_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%n = OpSpecConstant %uint 4\n%a = OpTypeArray %float %n",
    );
    assert_eq!(refused(&module), Rule::ArrayLength);
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%a = OpTypeArray %float %zero",
    );
    assert_eq!(refused(&module), Rule::ArrayLength);
}

#[test]
fn a_run_time_array_before_a_structures_last_member_is_refused() {
    let module = with(
        COMPUTE,
        "%block = OpTypeStruct %floats",
        "%block = OpTypeStruct %floats %float",
    );
    assert_eq!(refused(&module), Rule::RuntimeArray);
}

#[test]
fn a_structure_holding_an_image_is_refused() {
    let module = after(
        COMPUTE,
        "%float = OpTypeFloat 32",
        "%image = OpTypeImage %float 2D 0 0 0 1 Unknown\n%holder = OpTypeStruct %image",
    );
    assert_eq!(refused(&module), Rule::Contents);
}

#[test]
fn a_composite_constant_of_the_wrong_count_and_a_float_specialization_are_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%pair = OpConstantComposite %v3uint %zero %zero",
    );
    assert_eq!(refused(&module), Rule::Constant);
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%f = OpSpecConstant %float 0\n%g = OpSpecConstantOp %float FAdd %f %f",
    );
    assert_eq!(refused(&module), Rule::SpecConstantOp);
}

#[test]
fn decorations_on_what_they_cannot_decorate_are_refused() {
    let module = after(
        COMPUTE,
        "OpDecorate %block Block",
        "OpDecorate %buf Offset 0",
    );
    assert_eq!(refused(&module), Rule::DecorationTarget(Decoration::OFFSET));
    // The grammar ends `BufferBlock` at 1.3; there it is refused by name.
    let module = with(
        COMPUTE,
        "OpDecorate %block Block",
        "OpDecorate %block BufferBlock",
    );
    assert_eq!(
        refused(&module),
        Rule::EnumerantNotEnabled(Kind::Decoration, Decoration::BUFFER_BLOCK.0)
    );
    let module = with(&module, "\"main\" %gid %buf", "\"main\" %gid");
    let old = admit_version(&module, Version { major: 1, minor: 3 }).expect_err("refused");
    assert_eq!(old.rule, Rule::DecorationTarget(Decoration::BUFFER_BLOCK));
    let module = after(
        COMPUTE,
        "OpDecorate %buf Binding 0",
        "OpDecorate %buf Binding 1",
    );
    assert_eq!(
        refused(&module),
        Rule::DuplicateDecoration(Decoration::BINDING)
    );
}

#[test]
fn a_misaligned_or_missing_explicit_layout_is_refused() {
    let module = with(
        COMPUTE,
        "OpMemberDecorate %block 0 Offset 0",
        "OpMemberDecorate %block 0 Offset 2",
    );
    assert_eq!(refused(&module), Rule::ExplicitLayout);
    let module = with(COMPUTE, "OpDecorate %floats ArrayStride 4\n", "");
    assert_eq!(refused(&module), Rule::ExplicitLayout);
}

#[test]
fn variables_of_the_wrong_class_contents_or_initializer_are_refused() {
    let module = with(
        COMPUTE,
        "%buf = OpVariable %sb_block StorageBuffer",
        "%buf = OpVariable %sb_block Uniform",
    );
    assert!(matches!(refused(&module), Rule::StorageClass(_)));
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%pv = OpTypePointer Private %uint\n%init = OpVariable %pv Private %float",
    );
    assert_eq!(refused(&module), Rule::Initializer);
}

#[test]
fn a_pointer_held_in_memory_is_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%pp = OpTypePointer Private %sb_float\n%held = OpVariable %pp Private",
    );
    assert_eq!(refused(&module), Rule::Type);
}

#[test]
fn an_access_chain_through_a_structure_by_a_variable_index_is_refused() {
    let module = with(
        COMPUTE,
        "%p = OpAccessChain %sb_float %buf %zero %x",
        "%p = OpAccessChain %sb_float %buf %x %x",
    );
    assert_eq!(refused(&module), Rule::AccessChain);
}

#[test]
fn a_store_to_an_input_or_a_non_writable_buffer_is_refused() {
    let module = after(COMPUTE, "%id = OpLoad %v3uint %gid", "OpStore %gid %id");
    assert_eq!(refused(&module), Rule::NotWritable);
    let module = after(
        COMPUTE,
        "OpDecorate %buf Binding 0",
        "OpDecorate %buf NonWritable",
    );
    assert_eq!(refused(&module), Rule::NotWritable);
}

#[test]
fn an_image_copied_as_an_object_is_refused() {
    let module = after(
        FRAGMENT,
        "%t = OpLoad %sampled %tex",
        "%copy = OpCopyObject %sampled %t",
    );
    assert_eq!(refused(&module), Rule::OpaqueUse);
}

#[test]
fn a_result_type_the_instruction_cannot_make_is_refused() {
    let module = with(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%w = OpIAdd %float %v %v",
    );
    assert_eq!(refused(&module), Rule::ResultType);
}

#[test]
fn explicit_lod_sampling_without_a_level_is_refused() {
    let module = with(
        FRAGMENT,
        "%s = OpImageSampleImplicitLod %v4 %t %c",
        "%s = OpImageSampleExplicitLod %v4 %t %c None",
    );
    assert_eq!(refused(&module), Rule::ImageOperands);
}

#[test]
fn a_branch_to_the_entry_block_or_out_of_the_function_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "OpStore %p %w\nOpBranch %entry",
    );
    assert_eq!(refused(&module), Rule::BranchTarget);
}

#[test]
fn a_phi_whose_parents_are_not_its_predecessors_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "OpBranch %next\n%next = OpLabel\n%phi = OpPhi %float %w %next\nOpStore %p %phi\nOpReturn",
    );
    assert_eq!(refused(&module), Rule::Phi);
}

#[test]
fn a_block_from_which_no_return_is_reachable_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "OpBranch %spin\n%spin = OpLabel\nOpBranch %spin",
    );
    assert_eq!(refused(&module), Rule::NoExit);
}

#[test]
fn a_back_edge_to_a_block_that_is_no_loop_header_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "%c = OpFOrdLessThan %bool %v %w\nOpBranch %spin\n%spin = OpLabel\nOpSelectionMerge %out None\nOpBranchConditional %c %spin %out\n%out = OpLabel\nOpReturn",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert_eq!(refused(&module), Rule::BackEdge);
}

#[test]
fn two_headers_declaring_one_merge_block_are_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        "%c = OpFOrdLessThan %bool %v %w\nOpSelectionMerge %m None\nOpBranchConditional %c %a %m\n%a = OpLabel\nOpSelectionMerge %m None\nOpBranchConditional %c %b %m\n%b = OpLabel\nOpBranch %m\n%m = OpLabel\nOpReturn",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert_eq!(refused(&module), Rule::MergeBlock);
}

#[test]
fn a_break_out_of_two_loops_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        r"%c = OpFOrdLessThan %bool %v %w
OpBranch %outer
%outer = OpLabel
OpLoopMerge %outer_merge %outer_continue None
OpBranch %inner
%inner = OpLabel
OpLoopMerge %inner_merge %inner_continue None
OpBranchConditional %c %inner_body %outer_merge
%inner_body = OpLabel
OpBranch %inner_continue
%inner_continue = OpLabel
OpBranch %inner
%inner_merge = OpLabel
OpBranch %outer_continue
%outer_continue = OpLabel
OpBranch %outer
%outer_merge = OpLabel
OpReturn",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert_eq!(refused(&module), Rule::ConstructExit);
}

#[test]
fn a_switch_falling_through_against_its_target_order_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn",
        r"OpSelectionMerge %m None
OpSwitch %x %m 0 %case0 1 %case1
%case1 = OpLabel
OpBranch %case0
%case0 = OpLabel
OpBranch %m
%m = OpLabel
OpReturn",
    );
    assert_eq!(refused(&module), Rule::Fallthrough);
}

#[test]
fn recursion_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn\nOpFunctionEnd",
        "OpStore %p %w\n%r = OpFunctionCall %void %again\nOpReturn\nOpFunctionEnd\n%again = OpFunction %void None %fn\n%b = OpLabel\n%s = OpFunctionCall %void %again\nOpReturn\nOpFunctionEnd",
    );
    assert_eq!(refused(&module), Rule::Recursion);
}

#[test]
fn a_return_without_a_value_from_a_function_returning_one_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn\nOpFunctionEnd",
        "OpStore %p %w\nOpReturn\nOpFunctionEnd\n%helper = OpFunction %float None %fn_float\n%b = OpLabel\nOpReturn\nOpFunctionEnd",
    );
    let module = after(
        &module,
        "%float = OpTypeFloat 32",
        "%fn_float = OpTypeFunction %float",
    );
    assert_eq!(refused(&module), Rule::Return);
}

#[test]
fn a_call_of_the_wrong_arity_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn\nOpFunctionEnd",
        "%r = OpFunctionCall %void %helper %w\nOpStore %p %w\nOpReturn\nOpFunctionEnd\n%helper = OpFunction %void None %fn\n%b = OpLabel\nOpReturn\nOpFunctionEnd",
    );
    assert_eq!(refused(&module), Rule::Call);
}

#[test]
fn a_derivative_in_a_compute_shader_is_refused() {
    let module = after(COMPUTE, "%w = OpFAdd %float %v %v", "%d = OpDPdx %float %w");
    assert_eq!(refused(&module), Rule::ExecutionModel);
}

#[test]
fn a_compute_shader_without_its_workgroup_size_is_refused() {
    let module = with(COMPUTE, "OpExecutionMode %main LocalSize 8 8 1\n", "");
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::LOCAL_SIZE)
    );
}

#[test]
fn a_fragment_shader_with_the_lower_left_origin_is_refused() {
    let module = with(
        FRAGMENT,
        "OpExecutionMode %main OriginUpperLeft",
        "OpExecutionMode %main OriginLowerLeft",
    );
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::ORIGIN_LOWER_LEFT)
    );
}

#[test]
fn a_global_the_interface_does_not_list_is_refused() {
    let module = with(COMPUTE, "\"main\" %gid %buf", "\"main\" %gid");
    assert_eq!(refused(&module), Rule::Interface);
}

#[test]
fn a_position_that_is_no_four_component_vector_is_refused() {
    let module = with(
        VERTEX,
        "OpMemberDecorate %per_vertex 1 BuiltIn PointSize",
        "OpMemberDecorate %per_vertex 1 BuiltIn FragCoord",
    );
    assert_eq!(refused(&module), Rule::BuiltIn);
    let module = with(
        VERTEX,
        "OpMemberDecorate %per_vertex 0 BuiltIn Position",
        "OpMemberDecorate %per_vertex 0 BuiltIn PointSize",
    );
    assert_eq!(refused(&module), Rule::BuiltIn);
}

#[test]
fn an_input_without_a_location_or_two_overlapping_are_refused() {
    let module = with(FRAGMENT, "OpDecorate %uv Location 0\n", "");
    assert_eq!(refused(&module), Rule::Location);
    let module = after(
        FRAGMENT,
        "%uv = OpVariable %in_v2 Input",
        "%also = OpVariable %in_v2 Input",
    );
    let module = with(&module, "\"main\" %uv", "\"main\" %uv %also");
    let module = after(
        &module,
        "OpDecorate %uv Location 0",
        "OpDecorate %also Location 0",
    );
    assert_eq!(refused(&module), Rule::Location);
}

#[test]
fn a_resource_without_its_set_is_refused() {
    let module = with(COMPUTE, "OpDecorate %buf DescriptorSet 0\n", "");
    assert_eq!(refused(&module), Rule::Resource);
}

#[test]
fn a_value_of_another_function_is_refused() {
    let module = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn\nOpFunctionEnd",
        "OpStore %p %w\nOpReturn\nOpFunctionEnd\n%helper = OpFunction %float None %fn_float\n%b = OpLabel\nOpReturnValue %w\nOpFunctionEnd",
    );
    let module = after(
        &module,
        "%float = OpTypeFloat 32",
        "%fn_float = OpTypeFunction %float",
    );
    assert!(matches!(refused(&module), Rule::OtherFunction(_)));
}

#[test]
fn a_sampled_image_used_outside_its_block_is_refused() {
    let module = with(
        FRAGMENT,
        "%s = OpImageSampleImplicitLod %v4 %t %c",
        "OpBranch %next\n%next = OpLabel\n%s = OpImageSampleImplicitLod %v4 %t %c",
    );
    let module = with(&module, "%t = OpLoad %sampled %tex", "%i = OpLoad %image %tex_image\n%sm = OpLoad %sampler %smp\n%t = OpSampledImage %sampled %i %sm");
    let module = with(&module, "%tex = OpVariable %uc_sampled UniformConstant", "%uc_image = OpTypePointer UniformConstant %image\n%tex_image = OpVariable %uc_image UniformConstant\n%sampler = OpTypeSampler\n%uc_sampler = OpTypePointer UniformConstant %sampler\n%smp = OpVariable %uc_sampler UniformConstant");
    let module = with(
        &module,
        "\"main\" %uv %colour %tex",
        "\"main\" %uv %colour %tex_image %smp",
    );
    let module = with(&module, "OpDecorate %tex DescriptorSet 0\nOpDecorate %tex Binding 1", "OpDecorate %tex_image DescriptorSet 0\nOpDecorate %tex_image Binding 1\nOpDecorate %smp DescriptorSet 0\nOpDecorate %smp Binding 2");
    assert_eq!(refused(&module), Rule::OpaqueUse);
}

/// A compute shader reading and writing a 32-bit integer storage image.
const STORAGE_IMAGE: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %img_var
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %img_var DescriptorSet 0
OpDecorate %img_var Binding 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%v2int = OpTypeVector %int 2
%v4uint = OpTypeVector %uint 4
%image = OpTypeImage %uint 2D 0 0 0 2 R32ui
%uc_image = OpTypePointer UniformConstant %image
%img_var = OpVariable %uc_image UniformConstant
%izero = OpConstant %int 0
%origin = OpConstantComposite %v2int %izero %izero
%main = OpFunction %void None %fn
%entry = OpLabel
%img = OpLoad %image %img_var
%texel = OpImageRead %v4uint %img %origin
OpImageWrite %img %origin %texel
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_read_of_an_image_declared_unreadable_is_refused() {
    super::assemble::admit(STORAGE_IMAGE).expect("admitted");
    let module = after(
        STORAGE_IMAGE,
        "OpDecorate %img_var Binding 0",
        "OpDecorate %img_var NonReadable",
    );
    assert_eq!(refused(&module), Rule::NotReadable);
}

#[test]
fn a_format_less_write_without_its_capability_is_refused() {
    let module = with(
        STORAGE_IMAGE,
        "%image = OpTypeImage %uint 2D 0 0 0 2 R32ui",
        "%image = OpTypeImage %uint 2D 0 0 0 2 Unknown",
    );
    let module = with(&module, "%texel = OpImageRead %v4uint %img %origin\n", "");
    let module = with(
        &module,
        "OpImageWrite %img %origin %texel",
        "OpImageWrite %img %origin %izero",
    );
    assert!(matches!(refused(&module), Rule::OperandType(_)));
    let module = after(
        &module,
        "OpCapability Shader",
        "OpCapability StorageImageWriteWithoutFormat",
    );
    assert!(
        matches!(refused(&module), Rule::OperandType(_)),
        "the texel is a signed integer"
    );
}

#[test]
fn atomic_semantics_outside_the_vulkan_set_and_an_atomic_on_a_float_are_refused() {
    let atomic = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%u = OpAccessChain %sb_uint %ubuf %zero %x\n%old = OpAtomicIAdd %uint %u %device %semantics %x",
    );
    let atomic = after(
        &atomic,
        "%zero = OpConstant %uint 0",
        "%device = OpConstant %uint 1\n%semantics = OpConstant %uint 0x208\n%sb_uint = OpTypePointer StorageBuffer %uint\n%uints = OpTypeRuntimeArray %uint\n%ublock = OpTypeStruct %uints\n%sb_ublock = OpTypePointer StorageBuffer %ublock\n%ubuf = OpVariable %sb_ublock StorageBuffer",
    );
    let atomic = with(&atomic, "\"main\" %gid %buf", "\"main\" %gid %buf %ubuf");
    let atomic = after(&atomic, "OpDecorate %block Block", "OpDecorate %ublock Block\nOpMemberDecorate %ublock 0 Offset 0\nOpDecorate %uints ArrayStride 4\nOpDecorate %ubuf DescriptorSet 0\nOpDecorate %ubuf Binding 1");
    assert_eq!(refused(&atomic), Rule::Semantics);
    let relaxed = with(
        &atomic,
        "%semantics = OpConstant %uint 0x208",
        "%semantics = OpConstant %uint 0",
    );
    super::assemble::admit(&relaxed).expect("admitted");
    let float = after(
        &relaxed,
        "%w = OpFAdd %float %v %v",
        "%f = OpAtomicExchange %float %p %device %semantics %v",
    );
    assert!(matches!(refused(&float), Rule::OperandType(_)));
}

#[test]
fn workgroup_memory_in_a_fragment_shader_is_refused() {
    let module = after(
        FRAGMENT,
        "%colour = OpVariable %out_v4 Output",
        "%wg_v4 = OpTypePointer Workgroup %v4\n%shared = OpVariable %wg_v4 Workgroup",
    );
    let module = after(&module, "%d = OpDPdx %v4 %s", "OpStore %shared %d");
    let module = with(
        &module,
        "\"main\" %uv %colour %tex",
        "\"main\" %uv %colour %tex %shared",
    );
    assert_eq!(refused(&module), Rule::ExecutionModel);
}

#[test]
fn two_resources_at_one_binding_are_refused() {
    let module = after(
        COMPUTE,
        "%buf = OpVariable %sb_block StorageBuffer",
        "%twin = OpVariable %sb_block StorageBuffer",
    );
    let module = after(
        &module,
        "OpDecorate %buf Binding 0",
        "OpDecorate %twin DescriptorSet 0\nOpDecorate %twin Binding 0",
    );
    let module = with(&module, "\"main\" %gid %buf", "\"main\" %gid %buf %twin");
    let module = after(
        &module,
        "%v = OpLoad %float %p",
        "%q = OpAccessChain %sb_float %twin %zero %x\n%t = OpLoad %float %q",
    );
    assert_eq!(refused(&module), Rule::Resource);
}

#[test]
fn an_entry_points_function_called_or_named_twice_is_refused() {
    let called = with(
        COMPUTE,
        "OpStore %p %w\nOpReturn\nOpFunctionEnd",
        "OpStore %p %w\nOpReturn\nOpFunctionEnd\n%caller = OpFunction %void None %fn\n%b = OpLabel\n%r = OpFunctionCall %void %main\nOpReturn\nOpFunctionEnd",
    );
    assert_eq!(refused(&called), Rule::EntryPoint);
    let twice = after(
        COMPUTE,
        "OpEntryPoint GLCompute %main \"main\" %gid %buf",
        "OpEntryPoint GLCompute %main \"main\" %gid %buf",
    );
    assert_eq!(refused(&twice), Rule::EntryPoint);
}

#[test]
fn entry_points_past_the_bound_are_refused() {
    let mut points = String::new();
    for n in 0..=crate::MAX_ENTRY_POINTS {
        let _ = writeln!(points, "OpEntryPoint GLCompute %main \"main{n}\" %gid %buf");
    }
    let module = with(
        COMPUTE,
        "OpEntryPoint GLCompute %main \"main\" %gid %buf\n",
        &points,
    );
    assert_eq!(refused(&module), Rule::Limit(Limit::EntryPoints));
}

#[test]
fn a_selection_between_pointers_is_refused() {
    let module = after(
        COMPUTE,
        "%v = OpLoad %float %p",
        "%c = OpFOrdLessThan %bool %v %v\n%either = OpSelect %sb_float %c %p %p",
    );
    let module = after(&module, "%float = OpTypeFloat 32", "%bool = OpTypeBool");
    assert_eq!(refused(&module), Rule::LogicalPointer);
}

#[test]
fn a_constant_index_past_an_arrays_length_is_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%two = OpConstant %uint 2\n%pair = OpTypeArray %float %two\n%fn_pair = OpTypePointer Function %pair\n%fn_float = OpTypePointer Function %float",
    );
    let module = with(
        &module,
        "%entry = OpLabel",
        "%entry = OpLabel\n%local = OpVariable %fn_pair Function",
    );
    let module = after(
        &module,
        "%w = OpFAdd %float %v %v",
        "%past = OpAccessChain %fn_float %local %two",
    );
    assert_eq!(refused(&module), Rule::AccessChain);
}

#[test]
fn an_integer_fragment_input_that_is_not_flat_is_refused() {
    let module = after(
        FRAGMENT,
        "%v4 = OpTypeVector %float 4",
        "%uint = OpTypeInt 32 0\n%in_uint = OpTypePointer Input %uint",
    );
    let module = after(
        &module,
        "%uv = OpVariable %in_v2 Input",
        "%id = OpVariable %in_uint Input",
    );
    let module = after(
        &module,
        "OpDecorate %uv Location 0",
        "OpDecorate %id Location 1",
    );
    let module = with(
        &module,
        "\"main\" %uv %colour %tex",
        "\"main\" %uv %colour %tex %id",
    );
    assert_eq!(refused(&module), Rule::DecorationTarget(Decoration::FLAT));
    let flat = after(&module, "OpDecorate %id Location 1", "OpDecorate %id Flat");
    super::assemble::admit(&flat).expect("admitted");
}

#[test]
fn a_written_depth_without_depth_replacing_is_refused() {
    let module = after(FRAGMENT, "%colour = OpVariable %out_v4 Output", "%out_float = OpTypePointer Output %float\n%depth = OpVariable %out_float Output\n%near = OpConstant %float 0");
    let module = after(
        &module,
        "OpDecorate %colour Location 0",
        "OpDecorate %depth BuiltIn FragDepth",
    );
    let module = after(&module, "OpStore %colour %d", "OpStore %depth %near");
    let module = with(
        &module,
        "\"main\" %uv %colour %tex",
        "\"main\" %uv %colour %tex %depth",
    );
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::DEPTH_REPLACING)
    );
}

#[test]
fn a_bit_count_of_a_64_bit_integer_is_refused() {
    let module = after(COMPUTE, "OpCapability Shader", "OpCapability Int64");
    let module = after(
        &module,
        "%uint = OpTypeInt 32 0",
        "%ulong = OpTypeInt 64 0\n%big = OpConstant %ulong 1 0",
    );
    let module = after(
        &module,
        "%w = OpFAdd %float %v %v",
        "%bits = OpBitCount %ulong %big",
    );
    assert!(matches!(refused(&module), Rule::OperandType(_)));
}

#[test]
fn a_workgroup_variable_with_an_initializer_is_refused() {
    let module = after(
        COMPUTE,
        "%zero = OpConstant %uint 0",
        "%wg_uint = OpTypePointer Workgroup %uint\n%shared = OpVariable %wg_uint Workgroup %zero",
    );
    let module = with(&module, "\"main\" %gid %buf", "\"main\" %gid %buf %shared");
    assert_eq!(refused(&module), Rule::Initializer);
}

#[test]
fn a_workgroup_size_given_by_id_is_refused() {
    let module = with(
        COMPUTE,
        "OpExecutionMode %main LocalSize 8 8 1",
        "OpExecutionModeId %main LocalSizeId %eight %eight %one",
    );
    let module = after(
        &module,
        "%zero = OpConstant %uint 0",
        "%eight = OpConstant %uint 8\n%one = OpConstant %uint 1",
    );
    assert_eq!(
        refused(&module),
        Rule::ExecutionMode(ExecutionMode::LOCAL_SIZE_ID)
    );
}

#[test]
fn a_computed_offset_on_a_sample_is_refused() {
    let module = after(FRAGMENT, "%v4 = OpTypeVector %float 4", "%int = OpTypeInt 32 1\n%v2int = OpTypeVector %int 2\n%izero = OpConstant %int 0\n%offset = OpConstantComposite %v2int %izero %izero");
    let module = with(
        &module,
        "%s = OpImageSampleImplicitLod %v4 %t %c",
        "%s = OpImageSampleImplicitLod %v4 %t %c ConstOffset %offset",
    );
    super::assemble::admit(&module).expect("admitted");
    let computed = with(&module, "ConstOffset %offset", "0x10 %offset");
    assert_eq!(
        refused(&computed),
        Rule::EnumerantNotEnabled(Kind::ImageOperands, 0x10)
    );
    let computed = after(
        &computed,
        "OpCapability Shader",
        "OpCapability ImageGatherExtended",
    );
    assert_eq!(refused(&computed), Rule::ImageOperands);
}

#[test]
fn a_uniform_array_strided_tighter_than_std140_is_refused() {
    let module = with(
        COMPUTE,
        "%buf = OpVariable %sb_block StorageBuffer",
        "%buf = OpVariable %u_block Uniform",
    );
    let module = with(
        &module,
        "%sb_block = OpTypePointer StorageBuffer %block",
        "%u_block = OpTypePointer Uniform %block\n%sb_block = OpTypePointer StorageBuffer %block",
    );
    let module = with(
        &module,
        "%floats = OpTypeRuntimeArray %float",
        "%four = OpConstant %uint 4\n%floats = OpTypeArray %float %four",
    );
    let module = with(&module, "%zero = OpConstant %uint 0\n", "");
    let module = with(
        &module,
        "%uint = OpTypeInt 32 0",
        "%uint = OpTypeInt 32 0\n%zero = OpConstant %uint 0",
    );
    assert_eq!(refused(&module), Rule::ExplicitLayout);
}

#[test]
fn interpolation_on_a_vertex_input_is_refused() {
    let module = after(
        VERTEX,
        "OpDecorate %position Location 0",
        "OpDecorate %position Flat",
    );
    assert_eq!(refused(&module), Rule::DecorationTarget(Decoration::FLAT));
}

#[test]
fn an_array_length_naming_the_last_member_as_the_largest_word_is_refused() {
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%n = OpArrayLength %uint %buf 0xffffffff",
    );
    assert!(matches!(refused(&module), Rule::OperandType(_)));
    let module = after(
        COMPUTE,
        "%w = OpFAdd %float %v %v",
        "%n = OpArrayLength %uint %buf 0",
    );
    super::assemble::admit(&module).expect("admitted");
}

/// A fragment shader sampling one of four textures at `index`, through
/// `%base`, which `reach` makes of the array variable `%texs`.
fn texture_array(reach: &str, index: &str) -> String {
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %idx %colour %texs
OpExecutionMode %main OriginUpperLeft
OpDecorate %idx Location 0
OpDecorate %idx Flat
OpDecorate %colour Location 0
OpDecorate %texs DescriptorSet 0
OpDecorate %texs Binding 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%uint = OpTypeInt 32 0
%v2 = OpTypeVector %float 2
%v4 = OpTypeVector %float 4
%zero = OpConstant %uint 0
%four = OpConstant %uint 4
%f0 = OpConstant %float 0
%xy = OpConstantComposite %v2 %f0 %f0
%image = OpTypeImage %float 2D 0 0 0 1 Unknown
%sampled = OpTypeSampledImage %image
%arr = OpTypeArray %sampled %four
%uc_arr = OpTypePointer UniformConstant %arr
%uc_s = OpTypePointer UniformConstant %sampled
%texs = OpVariable %uc_arr UniformConstant
%in_uint = OpTypePointer Input %uint
%idx = OpVariable %in_uint Input
%out_v4 = OpTypePointer Output %v4
%colour = OpVariable %out_v4 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%i = OpLoad %uint %idx
{reach}
%p = OpAccessChain %uc_s %base {index}
%t = OpLoad %sampled %p
%s = OpImageSampleExplicitLod %v4 %t %xy Lod %f0
OpStore %colour %s
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn an_array_of_descriptors_is_indexed_by_a_constant_however_it_is_reached() {
    for reach in [
        "%base = OpCopyObject %uc_arr %texs",
        "%base = OpAccessChain %uc_arr %texs",
    ] {
        super::assemble::admit(&texture_array(reach, "%zero")).expect("admitted");
        assert_eq!(refused(&texture_array(reach, "%i")), Rule::AccessChain);
    }
}

/// A compute shader whose `%set` stores through its parameter, called with
/// the private variable `%priv`.
const WRITES_THROUGH: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %priv
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%one = OpConstant %uint 1
%pp = OpTypePointer Private %uint
%priv = OpVariable %pp Private %one
%set_t = OpTypeFunction %void %pp
%main = OpFunction %void None %fn
%entry = OpLabel
%r = OpFunctionCall %void %set %priv
OpReturn
OpFunctionEnd
%set = OpFunction %void None %set_t
%param = OpFunctionParameter %pp
%b = OpLabel
OpStore %param %one
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_write_through_a_parameter_is_refused_where_its_argument_may_not_be_written() {
    super::assemble::admit(WRITES_THROUGH).expect("admitted");
    let module = after(
        WRITES_THROUGH,
        "OpExecutionMode %main LocalSize 1 1 1",
        "OpDecorate %priv NonWritable",
    );
    assert_eq!(refused(&module), Rule::NotWritable);
    let module = after(
        WRITES_THROUGH,
        "OpExecutionMode %main LocalSize 1 1 1",
        "OpDecorate %param NonWritable",
    );
    assert_eq!(refused(&module), Rule::NotWritable);
}

/// A compute shader whose `%touch` applies `access` to the texels of the
/// storage image `%target`, decorated `decoration`, passed to it.
fn texels_through(decoration: &str, access: &str) -> String {
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %target
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %target DescriptorSet 0
OpDecorate %target Binding 0
{decoration}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%v2int = OpTypeVector %int 2
%v4 = OpTypeVector %float 4
%image = OpTypeImage %float 2D 0 0 0 2 Rgba8
%uc_image = OpTypePointer UniformConstant %image
%target = OpVariable %uc_image UniformConstant
%touch_t = OpTypeFunction %void %uc_image
%zero = OpConstant %int 0
%origin = OpConstantComposite %v2int %zero %zero
%half = OpConstant %float 0x3f000000
%grey = OpConstantComposite %v4 %half %half %half %half
%main = OpFunction %void None %fn
%entry = OpLabel
%r = OpFunctionCall %void %touch %target
OpReturn
OpFunctionEnd
%touch = OpFunction %void None %touch_t
%param = OpFunctionParameter %uc_image
%b = OpLabel
%img = OpLoad %image %param
{access}
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn texels_reached_through_a_parameter_are_refused_where_their_image_forbids_it() {
    let read = "%texel = OpImageRead %v4 %img %origin";
    let write = "OpImageWrite %img %origin %grey";
    let admitted = super::assemble::admit(&texels_through("", write)).expect("admitted");
    let binding = admitted
        .entry_points()
        .first()
        .and_then(|e| e.bindings().first())
        .map(|b| b.access);
    assert_eq!(binding, Some(crate::Access::WRITE));
    assert_eq!(
        refused(&texels_through("OpDecorate %target NonReadable", read)),
        Rule::NotReadable
    );
    assert_eq!(
        refused(&texels_through("OpDecorate %target NonWritable", write)),
        Rule::NotWritable
    );
}

#[test]
fn a_subgroup_operation_on_a_width_that_needs_a_device_feature_is_refused() {
    let caps = "OpCapability Shader\nOpCapability Int64\nOpCapability Float16\nOpCapability GroupNonUniform\nOpCapability GroupNonUniformArithmetic\nOpCapability GroupNonUniformBallot";
    let module = with(COMPUTE, "OpCapability Shader", caps);
    let module = after(&module, "%float = OpTypeFloat 32", "%long = OpTypeInt 64 0\n%half = OpTypeFloat 16\n%subgroup = OpConstant %uint 3\n%five = OpConstant %long 5 0\n%hc = OpConstant %half 0x3c00");
    let wide = after(
        &module,
        "%w = OpFAdd %float %v %v",
        "%sum = OpGroupNonUniformIAdd %long %subgroup Reduce %five",
    );
    assert_eq!(refused(&wide), Rule::ResultType);
    let narrow = after(
        &module,
        "%w = OpFAdd %float %v %v",
        "%first = OpGroupNonUniformBroadcastFirst %half %subgroup %hc",
    );
    assert_eq!(refused(&narrow), Rule::ResultType);
    let word = after(
        &module,
        "%w = OpFAdd %float %v %v",
        "%sum = OpGroupNonUniformIAdd %uint %subgroup Reduce %zero",
    );
    super::assemble::admit(&word).expect("admitted");
}

#[test]
fn a_use_in_an_unreachable_block_ahead_of_its_definition_is_refused() {
    let dead = |body: &str| {
        with(
            COMPUTE,
            "OpReturn\nOpFunctionEnd",
            &format!("OpReturn\n%dead = OpLabel\n{body}\nOpReturn\nOpFunctionEnd"),
        )
    };
    super::assemble::admit(&dead("%a = OpIAdd %uint %x %x\n%b = OpIAdd %uint %a %x"))
        .expect("admitted");
    let rule = refused(&dead("%a = OpIAdd %uint %b %x\n%b = OpIAdd %uint %a %x"));
    assert!(matches!(rule, Rule::ForwardReference(_)), "{rule:?}");
}

#[test]
fn an_image_offset_outside_the_range_every_device_takes_is_refused() {
    let offset = |x: &str, y: &str| {
        let constants = format!("%int = OpTypeInt 32 1\n%v2int = OpTypeVector %int 2\n%ox = OpConstant %int {x}\n%oy = OpConstant %int {y}\n%off = OpConstantComposite %v2int %ox %oy");
        let module = after(FRAGMENT, "%v4 = OpTypeVector %float 4", &constants);
        with(
            &module,
            "%s = OpImageSampleImplicitLod %v4 %t %c",
            "%s = OpImageSampleImplicitLod %v4 %t %c ConstOffset %off",
        )
    };
    super::assemble::admit(&offset("7", "0xfffffff8")).expect("admitted");
    assert!(matches!(refused(&offset("8", "0")), Rule::OperandType(_)));
    assert!(matches!(
        refused(&offset("0", "0xfffffff7")),
        Rule::OperandType(_)
    ));
    let special = with(
        &offset("1", "1"),
        "%ox = OpConstant",
        "%ox = OpSpecConstant",
    );
    let special = with(
        &special,
        "%off = OpConstantComposite",
        "%off = OpSpecConstantComposite",
    );
    assert!(matches!(refused(&special), Rule::OperandType(_)));
}

#[test]
fn a_long_function_type_declared_twice_is_refused_whatever_sorts_between() {
    let seven = " %uint".repeat(7);
    let types = format!(
        "%first = OpTypeFunction %void{seven} %uint\n%between = OpTypeFunction %void{seven} %float\n%again = OpTypeFunction %void{seven} %uint"
    );
    let module = after(COMPUTE, "%float = OpTypeFloat 32", &types);
    assert_eq!(refused(&module), Rule::DuplicateType);
}

/// What glslang emits for a vertex shader writing `gl_Position`: a
/// `gl_PerVertex` block declaring clip and cull distances it never uses.
pub(crate) const GLSLANG_VERTEX: &str = r#"
OpCapability Shader
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %_ %pos
OpName %main "main"
OpName %gl_PerVertex "gl_PerVertex"
OpMemberName %gl_PerVertex 0 "gl_Position"
OpMemberName %gl_PerVertex 1 "gl_PointSize"
OpMemberName %gl_PerVertex 2 "gl_ClipDistance"
OpMemberName %gl_PerVertex 3 "gl_CullDistance"
OpName %_ ""
OpName %pos "pos"
OpMemberDecorate %gl_PerVertex 0 BuiltIn Position
OpMemberDecorate %gl_PerVertex 1 BuiltIn PointSize
OpMemberDecorate %gl_PerVertex 2 BuiltIn ClipDistance
OpMemberDecorate %gl_PerVertex 3 BuiltIn CullDistance
OpDecorate %gl_PerVertex Block
OpDecorate %pos Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4float = OpTypeVector %float 4
%uint = OpTypeInt 32 0
%uint_1 = OpConstant %uint 1
%_arr_float_uint_1 = OpTypeArray %float %uint_1
%gl_PerVertex = OpTypeStruct %v4float %float %_arr_float_uint_1 %_arr_float_uint_1
%_ptr_Output_gl_PerVertex = OpTypePointer Output %gl_PerVertex
%_ = OpVariable %_ptr_Output_gl_PerVertex Output
%int = OpTypeInt 32 1
%int_0 = OpConstant %int 0
%int_2 = OpConstant %int 2
%_ptr_Input_v4float = OpTypePointer Input %v4float
%pos = OpVariable %_ptr_Input_v4float Input
%_ptr_Output_v4float = OpTypePointer Output %v4float
%_ptr_Output_float = OpTypePointer Output %float
%main = OpFunction %void None %fn
%entry = OpLabel
%18 = OpLoad %v4float %pos
%20 = OpAccessChain %_ptr_Output_v4float %_ %int_0
OpStore %20 %18
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_latent_clip_or_cull_distance_is_refused_once_it_is_read_or_written() {
    let clip = after(GLSLANG_VERTEX, "OpStore %20 %18", "%c = OpAccessChain %_ptr_Output_float %_ %int_2 %int_0\n%x = OpCompositeExtract %float %18 0\nOpStore %c %x");
    assert_eq!(refused(&clip), Rule::EnumerantNotEnabled(Kind::BuiltIn, 3));
    let whole = after(
        GLSLANG_VERTEX,
        "OpStore %20 %18",
        "%u = OpUndef %gl_PerVertex\nOpStore %_ %u",
    );
    assert_eq!(refused(&whole), Rule::EnumerantNotEnabled(Kind::BuiltIn, 3));
    let alone = with(GLSLANG_VERTEX, "OpMemberDecorate %gl_PerVertex 3 BuiltIn CullDistance\n", "OpMemberDecorate %gl_PerVertex 3 BuiltIn CullDistance\nOpDecorate %pos BuiltIn ClipDistance\n");
    assert_eq!(refused(&alone), Rule::EnumerantNotEnabled(Kind::BuiltIn, 3));
}

#[test]
fn a_blend_index_other_than_dual_source_blendings_two_is_refused() {
    let dual = after(
        FRAGMENT,
        "OpDecorate %colour Location 0",
        "OpDecorate %colour Index 1",
    );
    super::assemble::admit(&dual).expect("admitted");
    let wide = after(
        FRAGMENT,
        "OpDecorate %colour Location 0",
        "OpDecorate %colour Index 2",
    );
    assert_eq!(refused(&wide), Rule::DecorationTarget(Decoration::INDEX));
}

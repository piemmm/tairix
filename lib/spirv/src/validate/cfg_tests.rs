//! Structured control flow: the shapes front ends emit admitted, and each
//! way out of or into a construct the rules forbid refused.

use alloc::format;
use alloc::string::String;

use super::assemble::{admit, refused};
use crate::Rule;

/// A compute shader whose body after its entry label is `body`, with a
/// condition `%c` and a selector `%x` to branch on.
fn shader(body: &str) -> String {
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
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%block = OpTypeStruct %uint
%sb_block = OpTypePointer StorageBuffer %block
%buf = OpVariable %sb_block StorageBuffer
%sb_uint = OpTypePointer StorageBuffer %uint
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpAccessChain %sb_uint %buf %zero
%x = OpLoad %uint %p
%c = OpIEqual %bool %x %zero
{body}
OpFunctionEnd
"#
    )
}

#[test]
fn nested_loops_each_with_its_own_continue_and_break_are_admitted() {
    admit(&shader(
        r"OpBranch %outer
%outer = OpLabel
OpLoopMerge %outer_merge %outer_continue None
OpBranch %inner
%inner = OpLabel
OpLoopMerge %inner_merge %inner_continue None
OpBranchConditional %c %inner_merge %inner_body
%inner_body = OpLabel
OpStore %p %one
OpBranch %inner_continue
%inner_continue = OpLabel
OpBranch %inner
%inner_merge = OpLabel
OpBranchConditional %c %outer_merge %outer_continue
%outer_continue = OpLabel
OpBranch %outer
%outer_merge = OpLabel
OpReturn",
    ))
    .expect("admitted");
}

#[test]
fn a_case_breaking_to_its_loops_merge_from_inside_a_switch_is_admitted() {
    admit(&shader(
        r"OpBranch %header
%header = OpLabel
OpLoopMerge %merge %continue None
OpBranch %body
%body = OpLabel
OpSelectionMerge %after None
OpSwitch %x %after 1 %leave 2 %stay
%leave = OpLabel
OpBranch %merge
%stay = OpLabel
OpStore %p %one
OpBranch %after
%after = OpLabel
OpBranch %continue
%continue = OpLabel
OpBranchConditional %c %header %merge
%merge = OpLabel
OpReturn",
    ))
    .expect("admitted");
}

#[test]
fn an_early_return_and_a_selection_in_the_continue_construct_are_admitted() {
    admit(&shader(
        r"OpBranch %header
%header = OpLabel
OpLoopMerge %merge %continue None
OpBranch %body
%body = OpLabel
OpSelectionMerge %after None
OpBranchConditional %c %done %after
%done = OpLabel
OpReturn
%after = OpLabel
OpBranch %continue
%continue = OpLabel
OpSelectionMerge %latch None
OpBranchConditional %c %bump %latch
%bump = OpLabel
OpStore %p %one
OpBranch %latch
%latch = OpLabel
OpBranchConditional %c %header %merge
%merge = OpLabel
OpReturn",
    ))
    .expect("admitted");
}

#[test]
fn an_unreachable_merge_and_a_dead_block_after_a_return_are_admitted() {
    admit(&shader(
        r"OpSelectionMerge %merge None
OpBranchConditional %c %yes %no
%yes = OpLabel
OpReturn
%dead = OpLabel
OpBranch %merge
%no = OpLabel
OpReturn
%merge = OpLabel
OpUnreachable",
    ))
    .expect("admitted");
}

#[test]
fn a_single_block_loop_is_admitted() {
    admit(&shader(
        r"OpBranch %spin
%spin = OpLabel
OpLoopMerge %merge %spin None
OpBranchConditional %c %spin %merge
%merge = OpLabel
OpReturn",
    ))
    .expect("admitted");
}

#[test]
fn a_switch_falling_through_into_its_default_is_admitted() {
    admit(&shader(
        r"OpSelectionMerge %merge None
OpSwitch %x %default 4 %four
%four = OpLabel
OpStore %p %one
OpBranch %default
%default = OpLabel
OpBranch %merge
%merge = OpLabel
OpReturn",
    ))
    .expect("admitted");
}

#[test]
fn a_branch_back_into_a_selection_from_past_its_merge_is_refused() {
    let rule = refused(&shader(
        r"OpSelectionMerge %merge None
OpBranchConditional %c %a %b
%a = OpLabel
OpBranch %merge
%merge = OpLabel
OpBranch %b
%b = OpLabel
OpReturn",
    ));
    assert_eq!(rule, Rule::ConstructEntry);
}

#[test]
fn a_selection_merge_before_an_unconditional_branch_is_refused() {
    let rule = refused(&shader(
        r"OpSelectionMerge %merge None
OpBranch %merge
%merge = OpLabel
OpReturn",
    ));
    assert_eq!(rule, Rule::Merge);
}

#[test]
fn a_loop_whose_continue_target_is_its_merge_is_refused() {
    let rule = refused(&shader(
        r"OpBranch %header
%header = OpLabel
OpLoopMerge %merge %merge None
OpBranch %merge
%merge = OpLabel
OpReturn",
    ));
    assert_eq!(rule, Rule::ContinueTarget);
}

#[test]
fn a_switch_target_that_breaks_its_loop_directly_is_refused() {
    let rule = refused(&shader(
        r"OpBranch %header
%header = OpLabel
OpLoopMerge %merge %continue None
OpBranch %body
%body = OpLabel
OpSelectionMerge %after None
OpSwitch %x %after 1 %merge
%after = OpLabel
OpBranch %continue
%continue = OpLabel
OpBranch %header
%merge = OpLabel
OpReturn",
    ));
    assert_eq!(rule, Rule::Fallthrough);
}

#[test]
fn a_phi_in_the_first_block_is_refused() {
    let module = shader("OpReturn").replace(
        "%entry = OpLabel\n",
        "%entry = OpLabel\n%none = OpPhi %uint\n",
    );
    assert_eq!(refused(&module), Rule::Phi);
}

#[test]
fn two_literals_of_one_switch_alike_are_refused() {
    let rule = refused(&shader(
        r"OpSelectionMerge %merge None
OpSwitch %x %merge 3 %a 3 %b
%a = OpLabel
OpBranch %merge
%b = OpLabel
OpBranch %merge
%merge = OpLabel
OpReturn",
    ));
    assert_eq!(rule, Rule::OperandValue(2));
}

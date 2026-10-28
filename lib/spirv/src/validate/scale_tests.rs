//! Modules as large or as deep as the bounds allow: each admitted in time
//! linear in its size, and one past each bound refused.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use super::assemble::{admit, refused};
use crate::{
    Limit, Rule, MAX_BINDINGS, MAX_CALL_DEPTH, MAX_GLOBALS, MAX_INDEXES, MAX_LOCALS, MAX_MEMBERS,
    MAX_NESTING,
};

/// The parts of a compute shader a test supplies.
#[derive(Default)]
struct Parts<'a> {
    /// Ids its entry point lists, each with a leading space.
    interface: &'a str,
    decorations: &'a str,
    declarations: &'a str,
    body: &'a str,
    functions: &'a str,
}

/// A compute shader with `parts`, declaring a condition `%c`, a 32-bit
/// unsigned `%uint` and its zero `%zero`.
fn module(parts: &Parts<'_>) -> String {
    let Parts {
        interface,
        decorations,
        declarations,
        body,
        functions,
    } = parts;
    format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"{interface}
OpExecutionMode %main LocalSize 1 1 1
{decorations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%c = OpConstantTrue %bool
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
{declarations}
%main = OpFunction %void None %fn
%entry = OpLabel
{body}
OpFunctionEnd
{functions}
"#
    )
}

/// A compute shader whose function body is `body`, with further functions
/// `functions`.
fn shader(body: &str, functions: &str) -> String {
    module(&Parts {
        body,
        functions,
        ..Parts::default()
    })
}

/// `count` lines, the `n`th `line(n)`.
fn lines(count: usize, line: impl Fn(usize) -> String) -> String {
    let mut text = String::new();
    for n in 0..count {
        text.push_str(&line(n));
        text.push('\n');
    }
    text
}

/// `count` selections one after another, each closing before the next opens:
/// a dominator tree as deep as the function is long.
fn sequential(count: usize) -> String {
    let mut body = String::new();
    for n in 0..count {
        let _ = write!(
            body,
            "OpSelectionMerge %m{n} None\nOpBranchConditional %c %t{n} %m{n}\n%t{n} = OpLabel\nOpBranch %m{n}\n%m{n} = OpLabel\n"
        );
    }
    body.push_str("OpReturn");
    shader(&body, "")
}

/// `depth` selections each inside the last.
fn nested(depth: usize) -> String {
    let mut body = String::new();
    for n in 0..depth {
        let _ = write!(
            body,
            "OpSelectionMerge %m{n} None\nOpBranchConditional %c %t{n} %m{n}\n%t{n} = OpLabel\n"
        );
    }
    body.push_str("OpBranch %close\n%close = OpLabel\n");
    for n in (0..depth).rev() {
        let _ = write!(body, "OpBranch %m{n}\n%m{n} = OpLabel\n");
    }
    body.push_str("OpReturn");
    shader(&body, "")
}

/// An entry point calling down a chain of `length` functions in all.
fn calls(length: usize) -> String {
    let mut functions = String::new();
    for n in 1..length {
        let callee = if n + 1 < length {
            format!("%r{n} = OpFunctionCall %void %f{}\n", n + 1)
        } else {
            String::new()
        };
        let _ = write!(
            functions,
            "%f{n} = OpFunction %void None %fn\n%b{n} = OpLabel\n{callee}OpReturn\nOpFunctionEnd\n"
        );
    }
    let first = if length > 1 {
        "%r0 = OpFunctionCall %void %f1\n"
    } else {
        ""
    };
    shader(&format!("{first}OpReturn"), &functions)
}

#[test]
fn a_hundred_thousand_selections_in_sequence_are_admitted() {
    admit(&sequential(100_000)).expect("admitted");
}

#[test]
fn nesting_to_the_bound_is_admitted_and_past_it_refused() {
    admit(&nested(MAX_NESTING)).expect("admitted");
    assert_eq!(
        refused(&nested(MAX_NESTING + 1)),
        Rule::Limit(Limit::Nesting)
    );
}

#[test]
fn a_call_chain_to_the_bound_is_admitted_and_past_it_refused() {
    admit(&calls(MAX_CALL_DEPTH)).expect("admitted");
    assert_eq!(
        refused(&calls(MAX_CALL_DEPTH + 1)),
        Rule::Limit(Limit::CallDepth)
    );
}

/// A compute shader loading `count` samplers, each at its own binding.
fn samplers(count: usize) -> String {
    let mut interface = String::new();
    for n in 0..count {
        let _ = write!(interface, " %s{n}");
    }
    let decorations = lines(count, |n| {
        format!("OpDecorate %s{n} DescriptorSet 0\nOpDecorate %s{n} Binding {n}")
    });
    let declarations = format!(
        "%sampler = OpTypeSampler\n%uc_sampler = OpTypePointer UniformConstant %sampler\n{}",
        lines(count, |n| format!(
            "%s{n} = OpVariable %uc_sampler UniformConstant"
        ))
    );
    let body = format!(
        "{}OpReturn",
        lines(count, |n| format!("%l{n} = OpLoad %sampler %s{n}"))
    );
    module(&Parts {
        interface: &interface,
        decorations: &decorations,
        declarations: &declarations,
        body: &body,
        functions: "",
    })
}

#[test]
fn bindings_to_the_bound_are_admitted_and_past_it_refused() {
    let admitted = admit(&samplers(MAX_BINDINGS)).expect("admitted");
    let used = admitted.entry_points().first().map(|e| e.bindings().len());
    assert_eq!(used, Some(MAX_BINDINGS));
    assert_eq!(
        refused(&samplers(MAX_BINDINGS + 1)),
        Rule::Limit(Limit::Bindings)
    );
}

#[test]
fn globals_to_the_bound_are_admitted_and_past_it_refused() {
    let globals = |count: usize| {
        let declarations = format!(
            "%private_uint = OpTypePointer Private %uint\n{}",
            lines(count, |n| format!(
                "%g{n} = OpVariable %private_uint Private"
            ))
        );
        module(&Parts {
            declarations: &declarations,
            body: "OpReturn",
            ..Parts::default()
        })
    };
    admit(&globals(MAX_GLOBALS)).expect("admitted");
    assert_eq!(
        refused(&globals(MAX_GLOBALS + 1)),
        Rule::Limit(Limit::Globals)
    );
}

#[test]
fn locals_to_the_bound_are_admitted_and_past_it_refused() {
    let locals = |count: usize| {
        let body = format!(
            "{}OpReturn",
            lines(count, |n| format!(
                "%l{n} = OpVariable %function_uint Function"
            ))
        );
        module(&Parts {
            declarations: "%function_uint = OpTypePointer Function %uint",
            body: &body,
            ..Parts::default()
        })
    };
    admit(&locals(MAX_LOCALS)).expect("admitted");
    assert_eq!(refused(&locals(MAX_LOCALS + 1)), Rule::Limit(Limit::Locals));
}

#[test]
fn members_and_cases_to_the_bound_are_admitted_and_past_it_refused() {
    let structure = |count: usize| {
        let members: String = (0..count).map(|_| " %uint").collect();
        module(&Parts {
            declarations: &format!("%wide = OpTypeStruct{members}"),
            body: "OpReturn",
            ..Parts::default()
        })
    };
    let switch = |count: usize| {
        let mut cases = String::new();
        for n in 0..count {
            let _ = write!(cases, " {n} %merge");
        }
        shader(
            &format!("OpSelectionMerge %merge None\nOpSwitch %zero %merge{cases}\n%merge = OpLabel\nOpReturn"),
            "",
        )
    };
    admit(&structure(MAX_MEMBERS)).expect("admitted");
    admit(&switch(MAX_MEMBERS)).expect("admitted");
    assert_eq!(
        refused(&structure(MAX_MEMBERS + 1)),
        Rule::Limit(Limit::Members)
    );
    assert_eq!(
        refused(&switch(MAX_MEMBERS + 1)),
        Rule::Limit(Limit::Members)
    );
}

#[test]
fn parameters_and_arguments_to_the_bound_are_admitted_and_past_it_refused() {
    let wide = |count: usize| {
        let parameters: String = (0..count).map(|_| " %uint").collect();
        let arguments: String = (0..count).map(|_| " %zero").collect();
        let functions = format!(
            "%wide = OpFunction %void None %wide_fn\n{}%wide_entry = OpLabel\nOpReturn\nOpFunctionEnd",
            lines(count, |n| format!("%p{n} = OpFunctionParameter %uint"))
        );
        module(&Parts {
            declarations: &format!("%wide_fn = OpTypeFunction %void{parameters}"),
            body: &format!("%r = OpFunctionCall %void %wide{arguments}\nOpReturn"),
            functions: &functions,
            ..Parts::default()
        })
    };
    admit(&wide(MAX_INDEXES)).expect("admitted");
    assert_eq!(refused(&wide(MAX_INDEXES + 1)), Rule::Limit(Limit::Indexes));
}

/// How many of each repeated piece a hostile-shape test builds: enough that
/// work super-linear in it would take minutes, few enough under miri.
const fn scaled(full: usize, miri: usize) -> usize {
    if cfg!(miri) {
        miri
    } else {
        full
    }
}

#[test]
fn phis_after_a_switch_whose_every_case_is_its_merge_cost_one_predecessor_each() {
    let cases = scaled(MAX_MEMBERS, 8);
    let mut targets = String::new();
    for n in 0..cases {
        let _ = write!(targets, " {n} %merge");
    }
    let phis = lines(scaled(20_000, 8), |n| {
        format!("%p{n} = OpPhi %uint %zero %entry")
    });
    let body = format!("OpSelectionMerge %merge None\nOpSwitch %zero %merge{targets}\n%merge = OpLabel\n{phis}OpReturn");
    admit(&shader(&body, "")).expect("admitted");
}

#[test]
fn an_extension_declared_many_times_costs_nothing_per_instruction_it_enables() {
    let declared = "OpExtension \"SPV_KHR_16bit_storage\"\n".repeat(scaled(20_000, 4));
    let mut body = String::new();
    for n in 0..scaled(20_000, 4) {
        let _ = write!(body, "OpSelectionMerge %m{n} None\nOpBranchConditional %c %t{n} %m{n}\n%t{n} = OpLabel\nOpTerminateInvocation\n%m{n} = OpLabel\n");
    }
    let module = format!(
        "OpCapability Shader\n{declared}OpExtension \"SPV_KHR_terminate_invocation\"\nOpMemoryModel Logical GLSL450\nOpEntryPoint Fragment %main \"main\"\nOpExecutionMode %main OriginUpperLeft\n%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%bool = OpTypeBool\n%c = OpConstantTrue %bool\n%main = OpFunction %void None %fn\n%entry = OpLabel\n{body}OpReturn\nOpFunctionEnd\n"
    );
    super::assemble::admit_version(&module, crate::Version { major: 1, minor: 5 })
        .expect("admitted");
}

/// A vertex shader whose outputs are `variables` of one block of `members`
/// four-component members, each decorated by `member(n)`.
fn output_blocks(variables: usize, members: usize, member: impl Fn(usize) -> String) -> String {
    let mut interface = String::new();
    for n in 0..variables {
        let _ = write!(interface, " %o{n}");
    }
    let mut block = String::new();
    for _ in 0..members {
        block.push_str(" %v4");
    }
    format!(
        "OpCapability Shader\nOpMemoryModel Logical GLSL450\nOpEntryPoint Vertex %main \"main\"{interface}\nOpDecorate %s Block\n{}%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%float = OpTypeFloat 32\n%v4 = OpTypeVector %float 4\n%s = OpTypeStruct{block}\n%out_s = OpTypePointer Output %s\n{}%main = OpFunction %void None %fn\n%entry = OpLabel\nOpReturn\nOpFunctionEnd\n",
        lines(members, member),
        lines(variables, |n| format!("%o{n} = OpVariable %out_s Output")),
    )
}

#[test]
fn many_outputs_of_a_wide_block_are_refused_at_their_first_overlap() {
    let (variables, members) = (scaled(300, 2), scaled(MAX_MEMBERS, 4));
    let built_ins = output_blocks(variables, members, |n| {
        format!("OpMemberDecorate %s {n} BuiltIn Position")
    });
    assert_eq!(refused(&built_ins), Rule::BuiltIn);
    let located = output_blocks(variables, members, |n| {
        format!("OpMemberDecorate %s {n} Location 0")
    });
    assert_eq!(refused(&located), Rule::Location);
}

#[test]
fn every_break_out_of_deep_nesting_is_judged_without_walking_it() {
    let depth = scaled(MAX_NESTING - 2, 16);
    let breaks = scaled(50_000, 8);
    let mut body =
        String::from("OpBranch %h\n%h = OpLabel\nOpLoopMerge %lm %lc None\nOpBranch %s0\n");
    for i in 0..depth {
        let _ = write!(
            body,
            "%s{i} = OpLabel\nOpSelectionMerge %sm{i} None\nOpBranchConditional %c %s{} %sm{i}\n",
            i + 1
        );
    }
    let _ = write!(body, "%s{depth} = OpLabel\nOpBranch %x0\n");
    for e in 0..breaks {
        let _ = write!(
            body,
            "%x{e} = OpLabel\nOpBranchConditional %c %lm %x{}\n",
            e + 1
        );
    }
    let _ = write!(body, "%x{breaks} = OpLabel\nOpBranch %sm{}\n", depth - 1);
    for i in (1..depth).rev() {
        let _ = write!(body, "%sm{i} = OpLabel\nOpBranch %sm{}\n", i - 1);
    }
    body.push_str(
        "%sm0 = OpLabel\nOpBranch %lc\n%lc = OpLabel\nOpBranch %h\n%lm = OpLabel\nOpReturn",
    );
    admit(&shader(&body, "")).expect("admitted");
}

#[test]
fn every_block_inside_deeply_nested_continue_constructs_is_checked_against_the_innermost() {
    let depth = scaled(MAX_NESTING / 2 - 1, 8);
    let blocks = scaled(50_000, 8);
    let mut body = String::from("OpBranch %h0\n");
    for k in 0..depth {
        let _ = write!(body, "%h{k} = OpLabel\nOpLoopMerge %m{k} %ct{k} None\nOpBranchConditional %c %ct{k} %m{k}\n%ct{k} = OpLabel\n");
        if k + 1 < depth {
            let _ = writeln!(body, "OpBranch %h{}", k + 1);
        } else {
            body.push_str("OpBranch %x0\n");
        }
    }
    for e in 0..blocks {
        let _ = write!(body, "%x{e} = OpLabel\nOpBranch %x{}\n", e + 1);
    }
    let _ = write!(
        body,
        "%x{blocks} = OpLabel\nOpBranchConditional %c %h{} %m{}\n",
        depth - 1,
        depth - 1
    );
    for k in (0..depth - 1).rev() {
        let _ = write!(body, "%m{} = OpLabel\nOpBranch %be{k}\n%be{k} = OpLabel\nOpBranchConditional %c %h{k} %m{k}\n", k + 1);
    }
    body.push_str("%m0 = OpLabel\nOpReturn");
    admit(&shader(&body, "")).expect("admitted");
}

#[test]
fn members_that_are_deep_arrays_of_matrices_are_laid_out_once_per_array() {
    let (depth, members) = (scaled(200, 4), scaled(8_000, 4));
    let mut decorations = String::from(
        "OpDecorate %buf DescriptorSet 0\nOpDecorate %buf Binding 0\nOpDecorate %block Block\n",
    );
    for k in 1..=depth {
        let _ = writeln!(decorations, "OpDecorate %a{k} ArrayStride 64");
    }
    for i in 0..members {
        let _ = write!(decorations, "OpMemberDecorate %block {i} Offset {}\nOpMemberDecorate %block {i} ColMajor\nOpMemberDecorate %block {i} MatrixStride 16\n", i * 64);
    }
    let mut declarations = String::from("%one = OpConstant %uint 1\n%float = OpTypeFloat 32\n%v4 = OpTypeVector %float 4\n%a0 = OpTypeMatrix %v4 4\n%sb_v4 = OpTypePointer StorageBuffer %v4\n");
    for k in 1..=depth {
        let _ = writeln!(declarations, "%a{k} = OpTypeArray %a{} %one", k - 1);
    }
    declarations.push_str("%block = OpTypeStruct");
    for _ in 0..members {
        let _ = write!(declarations, " %a{depth}");
    }
    declarations.push_str("\n%sb_block = OpTypePointer StorageBuffer %block\n%buf = OpVariable %sb_block StorageBuffer");
    let body = format!(
        "%p = OpAccessChain %sb_v4 %buf{} %zero\n%v = OpLoad %v4 %p\nOpReturn",
        " %zero".repeat(depth + 1)
    );
    let module = module(&Parts {
        interface: " %buf",
        decorations: &decorations,
        declarations: &declarations,
        body: &body,
        functions: "",
    });
    admit(&module).expect("admitted");
}

#[test]
fn many_entry_points_sharing_deep_array_outputs_cost_each_output_once_each() {
    let (entries, outputs, depth) = (
        scaled(crate::MAX_ENTRY_POINTS, 2),
        scaled(60, 2),
        scaled(250, 4),
    );
    let mut text = String::from("OpCapability Shader\nOpMemoryModel Logical GLSL450\n");
    let mut listed = String::new();
    for o in 0..outputs {
        let _ = write!(listed, " %o{o}");
    }
    for e in 0..entries {
        let _ = writeln!(text, "OpEntryPoint Vertex %e{e} \"e{e}\"{listed}");
    }
    text.push_str(&lines(outputs, |o| {
        format!("OpDecorate %o{o} Location {o}")
    }));
    text.push_str("%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%uint = OpTypeInt 32 0\n%one = OpConstant %uint 1\n%d0 = OpTypeFloat 32\n");
    text.push_str(&lines(depth, |k| {
        format!("%d{} = OpTypeArray %d{k} %one", k + 1)
    }));
    let _ = writeln!(text, "%out_d = OpTypePointer Output %d{depth}");
    text.push_str(&lines(outputs, |o| {
        format!("%o{o} = OpVariable %out_d Output")
    }));
    text.push_str(&lines(entries, |e| {
        format!("%e{e} = OpFunction %void None %fn\n%eb{e} = OpLabel\nOpReturn\nOpFunctionEnd")
    }));
    admit(&text).expect("admitted");
}

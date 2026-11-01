use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use tairix_spirv::assemble::admit;
use tairix_spirv::Stage;

use super::{lower, Fault, MAX_CONSTANT_WORDS};
use crate::software::shader::ir::{Conv, Inst, Program};

/// The program the entry point `name` of `text` lowers to: a compute
/// entry point's in its own workgroups, any other's one invocation apiece.
fn lowered(text: &str, name: &str) -> Result<Program, Fault> {
    let admitted = admit(text).unwrap_or_else(|refusal| panic!("{refusal:?} in\n{text}"));
    let index = admitted
        .entry_points()
        .iter()
        .position(|e| e.name() == name)
        .expect("the entry point");
    let entry = &admitted.entry_points()[index];
    let specialized = admitted.specialize(&[]).expect("no constants to set");
    let size = if entry.stage() == Stage::Compute {
        specialized.workgroup_size(index).expect("a size")
    } else {
        [1, 1, 1]
    };
    lower(&admitted, entry, &specialized, size)
}

/// A compute shader `main` whose types and constants are `decls` and whose
/// entry block's body is `body`.
fn compute(decls: &str, body: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
{decls}
%main = OpFunction %void None %fn
%entry = OpLabel
{body}
OpFunctionEnd
"#
    )
}

#[test]
fn a_switch_whose_labels_all_name_one_block_makes_that_edge_once() {
    const LABELS: usize = 1000;
    const PHIS: usize = 20;
    let mut decls = String::new();
    for k in 0..PHIS {
        let _ = writeln!(decls, "%v{k} = OpConstant %uint {}", k + 2);
    }
    let mut body = String::from("OpSelectionMerge %m None\nOpSwitch %zero %m");
    for label in 0..LABELS {
        let _ = write!(body, " {} %x", label + 1);
    }
    body.push_str("\n%x = OpLabel\n");
    for k in 0..PHIS {
        let _ = writeln!(body, "%p{k} = OpPhi %uint %v{k} %entry");
    }
    body.push_str("OpBranch %m\n%m = OpLabel\nOpReturn");
    let program = lowered(&compute(&decls, &body), "main").expect("lowered");
    assert_eq!(
        program.moves.len(),
        PHIS,
        "one edge's copies, not one per label"
    );
}

#[test]
fn zero_values_share_one_run_of_constants() {
    const UNDEFINED: usize = 1000;
    const WORDS: usize = 4096;
    let decls = format!("%c{WORDS} = OpConstant %uint {WORDS}\n%big = OpTypeArray %uint %c{WORDS}");
    let mut body = String::new();
    for k in 0..UNDEFINED {
        let _ = writeln!(body, "%u{k} = OpUndef %big");
    }
    body.push_str("OpReturn");
    let program = lowered(&compute(&decls, &body), "main").expect("lowered");
    assert!(
        program.constants.len() < 2 * WORDS + 64,
        "{} words of constants for {UNDEFINED} zero values of {WORDS} words",
        program.constants.len()
    );
}

/// Declarations of a 65,536-word array type `%a65536` and a constant
/// `%k4096` of its element type, built up sixteen at a time.
fn tower() -> String {
    let mut decls = String::from("%c16 = OpConstant %uint 16\n");
    let mut element = String::from("%uint");
    let mut part = String::from("%one");
    for words in [16, 256, 4096, 65536] {
        let _ = writeln!(decls, "%a{words} = OpTypeArray {element} %c16");
        if words < 65536 {
            let parts = format!(" {part}").repeat(16);
            let _ = writeln!(decls, "%k{words} = OpConstantComposite %a{words}{parts}");
            part = format!("%k{words}");
        }
        element = format!("%a{words}");
    }
    decls
}

#[test]
fn constants_past_their_budget_are_refused() {
    let each = 65536;
    let fit = MAX_CONSTANT_WORDS / each - 1;
    let program = |count: usize| {
        let mut decls = tower();
        let mut body = String::new();
        for k in 0..count {
            let parts = " %k4096".repeat(16);
            let _ = writeln!(decls, "%big{k} = OpConstantComposite %a65536{parts}");
            let _ = writeln!(body, "%e{k} = OpCompositeExtract %uint %big{k} 0 0 0 0");
        }
        body.push_str("OpReturn");
        lowered(&compute(&decls, &body), "main")
    };
    program(fit).expect("within the budget");
    assert_eq!(program(fit + 2).err(), Some(Fault::Budget));
}

#[test]
fn a_constant_structure_of_the_most_members_is_laid_out_in_one_walk() {
    const MEMBERS: usize = 16383;
    let members = " %uint".repeat(MEMBERS);
    let parts = " %one".repeat(MEMBERS);
    let decls = format!("%wide = OpTypeStruct{members}\n%all = OpConstantComposite %wide{parts}");
    let body = format!(
        "%last = OpCompositeExtract %uint %all {}\nOpReturn",
        MEMBERS - 1
    );
    lowered(&compute(&decls, &body), "main").expect("lowered within its member reads");
}

#[test]
fn another_entry_points_variables_are_not_lowered_or_counted() {
    let text = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %paint "paint" %colour
OpEntryPoint GLCompute %first "first" %shared_first
OpEntryPoint GLCompute %second "second" %shared_second
OpExecutionMode %paint OriginUpperLeft
OpExecutionMode %first LocalSize 1 1 1
OpExecutionMode %second LocalSize 1 1 1
OpDecorate %colour Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v4float = OpTypeVector %float 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%fzero = OpConstant %float 0
%black = OpConstantComposite %v4float %fzero %fzero %fzero %fzero
%c6144 = OpConstant %uint 6144
%words = OpTypeArray %uint %c6144
%out_v4 = OpTypePointer Output %v4float
%wg_words = OpTypePointer Workgroup %words
%wg_uint = OpTypePointer Workgroup %uint
%colour = OpVariable %out_v4 Output
%shared_first = OpVariable %wg_words Workgroup
%shared_second = OpVariable %wg_words Workgroup
%paint = OpFunction %void None %fn
%paint_entry = OpLabel
OpStore %colour %black
OpReturn
OpFunctionEnd
%first = OpFunction %void None %fn
%first_entry = OpLabel
%a = OpAccessChain %wg_uint %shared_first %zero
OpStore %a %one
OpReturn
OpFunctionEnd
%second = OpFunction %void None %fn
%second_entry = OpLabel
%b = OpAccessChain %wg_uint %shared_second %zero
OpStore %b %one
OpReturn
OpFunctionEnd
"#;
    for name in ["first", "second"] {
        let program = lowered(text, name).expect("lowered beside a fragment output");
        assert_eq!(
            program.workgroup_words, 6144,
            "{name} counts its own memory only"
        );
    }
}

#[test]
fn a_level_of_detail_query_reads_its_coordinate() {
    let text = r#"OpCapability Shader
OpCapability ImageQuery
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %tex %out
OpExecutionMode %main OriginUpperLeft
OpDecorate %tex DescriptorSet 0
OpDecorate %tex Binding 0
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 1
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%half = OpConstant %float 0.5
%centre = OpConstantComposite %v2float %half %half
%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%tex = OpVariable %si_ptr UniformConstant
%block = OpTypeStruct %v2float
%block_ptr = OpTypePointer StorageBuffer %block
%v2_ptr = OpTypePointer StorageBuffer %v2float
%out = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%s = OpLoad %si_t %tex
%lod = OpImageQueryLod %v2float %s %centre
%p = OpAccessChain %v2_ptr %out %zero
OpStore %p %lod
OpReturn
OpFunctionEnd
"#;
    let program = lowered(text, "main").expect("lowered");
    let query = program
        .images
        .iter()
        .find(|image| image.op == crate::software::shader::ir::ImageOp::QueryLod)
        .expect("the query");
    assert_ne!(query.coord, query.handle, "the coordinate, not the image");
    assert_eq!(query.coords, 2);
}

#[test]
fn constants_sharing_parts_of_no_words_are_laid_out_without_walking_them() {
    let mut decls = String::from(
        "%empty = OpTypeStruct\n%c16 = OpConstant %uint 16\n%k0 = OpConstantNull %empty\n",
    );
    let mut element = String::from("%empty");
    for level in 1..=8 {
        let _ = writeln!(decls, "%a{level} = OpTypeArray {element} %c16");
        let parts = format!(" %k{}", level - 1).repeat(16);
        let _ = writeln!(decls, "%k{level} = OpConstantComposite %a{level}{parts}");
        element = format!("%a{level}");
    }
    decls.push_str("%t0 = OpTypeStruct %uint %a8\n%held = OpConstantComposite %t0 %one %k8\n");
    let body = "%x = OpCopyObject %a8 %k8\n%y = OpCompositeExtract %uint %held 0\nOpReturn";
    lowered(&compute(&decls, body), "main").expect("4 billion parts of nothing, never walked");
}

/// Declarations of a chain `%t{depth}` of one-element arrays of `%uint`,
/// its constant `%v{depth}`, and an array `%mid` of 4096 of them.
fn chain(depth: usize) -> String {
    let mut decls = String::from("%c4096 = OpConstant %uint 4096\n%v0 = OpConstant %uint 7\n");
    for level in 1..=depth {
        let below = if level == 1 {
            String::from("%uint")
        } else {
            format!("%t{}", level - 1)
        };
        let _ = writeln!(
            decls,
            "%t{level} = OpTypeArray {below} %one\n%v{level} = OpConstantComposite %t{level} %v{}",
            level - 1
        );
    }
    let _ = writeln!(decls, "%mid = OpTypeArray %t{depth} %c4096");
    decls
}

#[test]
fn the_parts_of_constants_laid_out_are_held_to_their_budget() {
    let program = |count: usize| {
        let mut decls = chain(250);
        let mut body = String::new();
        let parts = " %v250".repeat(4096);
        for k in 0..count {
            let _ = writeln!(decls, "%m{k} = OpConstantComposite %mid{parts}");
            let _ = writeln!(body, "%x{k} = OpCopyObject %mid %m{k}");
        }
        body.push_str("OpReturn");
        lowered(&compute(&decls, &body), "main")
    };
    program(4).expect("a million parts of each of four");
    assert_eq!(
        program(17).err(),
        Some(Fault::Budget),
        "past sixteen million parts, though only 70 thousand words"
    );
}

#[test]
fn values_of_no_words_build_nothing_however_often_they_are_inlined() {
    let members = " %empty".repeat(16383);
    let decls = format!("%empty = OpTypeStruct\n%wide = OpTypeStruct{members}\n%e = OpConstantNull %empty\n%wfn = OpTypeFunction %wide");
    let parts = " %e".repeat(16383);
    let mut body = String::new();
    for k in 0..300 {
        let _ = writeln!(body, "%r{k} = OpFunctionCall %wide %build");
    }
    body.push_str("OpReturn\nOpFunctionEnd\n%build = OpFunction %wide None %wfn\n%b = OpLabel\n");
    let _ = write!(
        body,
        "%made = OpCompositeConstruct %wide{parts}\nOpReturnValue %made"
    );
    lowered(&compute(&decls, &body), "main").expect("five million copies of nothing, never made");
}

#[test]
fn a_call_s_result_of_no_words_is_a_value_like_any_other() {
    let decls = "%empty = OpTypeStruct\n%efn = OpTypeFunction %empty\n%e = OpConstantNull %empty\n%local = OpTypePointer Function %empty";
    let body = "%v = OpVariable %local Function\n%r = OpFunctionCall %empty %make\n%c = OpCopyObject %empty %r\nOpStore %v %r\nOpReturn\nOpFunctionEnd\n%make = OpFunction %empty None %efn\n%m = OpLabel\nOpReturnValue %e";
    lowered(&compute(decls, body), "main").expect("lowered");
}

#[test]
fn calls_one_after_another_share_their_callees_private_words() {
    let decls = "%c256 = OpConstant %uint 256\n%big = OpTypeArray %uint %c256\n%local = OpTypePointer Function %big\n%local_word = OpTypePointer Function %uint";
    let mut body = String::new();
    for k in 0..16 {
        let _ = writeln!(body, "%r{k} = OpFunctionCall %void %helper");
    }
    body.push_str("OpReturn\nOpFunctionEnd\n%helper = OpFunction %void None %fn\n%h = OpLabel\n%a = OpVariable %local Function\n%p = OpAccessChain %local_word %a %one\nOpStore %p %one\nOpReturn");
    let program = lowered(&compute(decls, &body), "main").expect("lowered");
    assert_eq!(
        program.private_words, 256,
        "one call's array at a time, not sixteen"
    );
}

#[test]
fn every_zero_value_shares_the_one_run_whatever_its_type() {
    let mut decls = String::from("%c65536 = OpConstant %uint 65536\n");
    let mut body = String::new();
    for k in 0..64 {
        // Each a type of its own, so no two nulls are the same constant.
        let _ = writeln!(
            decls,
            "%a{k} = OpTypeArray %uint %c65536\n%n{k} = OpConstantNull %a{k}"
        );
        let _ = writeln!(body, "%x{k} = OpCopyObject %a{k} %n{k}");
    }
    body.push_str("OpReturn");
    let program = lowered(&compute(&decls, &body), "main").expect("lowered");
    assert!(
        program.constants.len() < 2 * 65536 + 64,
        "{} words for 64 zero values of 65536 words",
        program.constants.len()
    );
}

#[test]
fn a_type_s_explicit_layout_is_planned_once_however_often_it_is_loaded() {
    let mut loads = String::new();
    for k in 0..256 {
        let _ = writeln!(loads, "%l{k} = OpLoad %block %buf");
    }
    let text = format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpMemberDecorate %block 1 Offset 16
OpMemberDecorate %inner 0 Offset 0
OpMemberDecorate %inner 1 Offset 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%inner = OpTypeStruct %uint %uint
%block = OpTypeStruct %inner %inner
%block_ptr = OpTypePointer StorageBuffer %block
%buf = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
{loads}OpReturn
OpFunctionEnd
"#
    );
    let program = lowered(&text, "main").expect("lowered");
    assert_eq!(program.scalars.len(), 4, "one plan's four scalars");
}

#[test]
fn the_operands_each_inlined_instance_walks_are_held_to_their_budget() {
    let program = |calls: usize| {
        let decls = "%empty = OpTypeStruct\n%c65532 = OpConstant %uint 65532\n%row = OpTypeArray %empty %c65532\n%e = OpConstantNull %empty";
        let parts = " %e".repeat(65532);
        let mut body = String::new();
        for k in 0..calls {
            let _ = writeln!(body, "%r{k} = OpFunctionCall %void %leaf");
        }
        body.push_str("OpReturn\nOpFunctionEnd\n%leaf = OpFunction %void None %fn\n%l = OpLabel\n");
        let _ = write!(body, "%built = OpCompositeConstruct %row{parts}\nOpReturn");
        lowered(&compute(decls, &body), "main")
    };
    program(100).expect("six and a half million operands of nothing");
    assert_eq!(
        program(130).err(),
        Some(Fault::Budget),
        "past eight million, though each builds nothing"
    );
}

#[test]
fn a_lowered_program_holds_no_room_past_its_entries() {
    // Enough of every kind of entry that a table grown by doubling would
    // hold room past its last.
    let mut decls = String::from("%fn_uint = OpTypePointer Function %uint\n");
    let mut body = String::from("%x = OpVariable %fn_uint Function\nOpStore %x %zero\n");
    for k in 0..37 {
        let _ = writeln!(decls, "%c{k} = OpConstant %uint {}", k + 2);
        let _ = writeln!(
            body,
            "%l{k} = OpLoad %uint %x\n%s{k} = OpIAdd %uint %l{k} %c{k}\nOpStore %x %s{k}"
        );
    }
    body.push_str("OpReturn\n");
    let program = lowered(&compute(&decls, &body), "main").expect("lowered");
    let tables = [
        (
            "instructions",
            program.insts.len(),
            program.insts.capacity(),
        ),
        ("blocks", program.blocks.len(), program.blocks.capacity()),
        ("edges", program.edges.len(), program.edges.capacity()),
        ("moves", program.moves.len(), program.moves.capacity()),
        ("cases", program.cases.len(), program.cases.capacity()),
        ("steps", program.steps.len(), program.steps.capacity()),
        ("images", program.images.len(), program.images.capacity()),
        ("plans", program.plans.len(), program.plans.capacity()),
        ("scalars", program.scalars.len(), program.scalars.capacity()),
        (
            "bindings",
            program.bindings.len(),
            program.bindings.capacity(),
        ),
        ("given", program.given.len(), program.given.capacity()),
        (
            "constants",
            program.constants.len(),
            program.constants.capacity(),
        ),
    ];
    assert!(program.insts.len() > 37, "the stores and loads lowered");
    for (table, len, capacity) in tables {
        assert_eq!(len, capacity, "the program's {table}");
    }
}

/// A compute shader `main` over a storage buffer of items, each a word and a
/// pair of words, whose entry block's body is `body`.
fn over_items(body: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf
OpExecutionMode %main LocalSize 64 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpMemberDecorate %item 0 Offset 0
OpMemberDecorate %item 1 Offset 8
OpDecorate %items ArrayStride 16
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v2uint = OpTypeVector %uint 2
%v3uint = OpTypeVector %uint 3
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%item = OpTypeStruct %uint %v2uint
%items = OpTypeRuntimeArray %item
%block = OpTypeStruct %items
%block_ptr = OpTypePointer StorageBuffer %block
%uint_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
{body}
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn a_chain_takes_no_step_of_no_bytes_and_one_for_bytes_in_a_row() {
    // `buf.items[x].pair.y`: member 0, an index, member 1, component 1.
    let text = over_items(
        "%p = OpAccessChain %uint_ptr %buf %zero %x %one %one\n%v = OpLoad %uint %p\nOpStore %p %v",
    );
    let program = lowered(&text, "main").expect("lowered");
    let steps = program
        .insts
        .iter()
        .find_map(|inst| match inst {
            crate::software::shader::ir::Inst::Chain {
                steps: (first, last),
                ..
            } => program.steps.get(*first as usize..*last as usize),
            _ => None,
        })
        .expect("the chain");
    assert!(
        matches!(
            steps,
            [
                crate::software::shader::ir::Step::Index { stride: 16, .. },
                crate::software::shader::ir::Step::Bytes(12)
            ]
        ),
        "the index, then the pair's offset and the component's as one: {steps:?}"
    );
}

#[test]
fn chains_folding_to_one_pointer_share_its_constant() {
    let once = "%p = OpAccessChain %uint_ptr %buf %zero %zero %one %one\n%v = OpLoad %uint %p\nOpStore %p %v";
    let twice = format!(
        "{once}\n%q = OpAccessChain %uint_ptr %buf %zero %zero %one %one\n%w = OpLoad %uint %q\nOpStore %q %w"
    );
    let constants = |body: &str| {
        let program = lowered(&over_items(body), "main").expect("lowered");
        let chained = program
            .insts
            .iter()
            .any(|inst| matches!(inst, crate::software::shader::ir::Inst::Chain { .. }));
        assert!(!chained, "each chain folded to a constant");
        program.constants.len()
    };
    assert_eq!(constants(&twice), constants(once));
}

#[test]
fn loads_of_one_type_laid_out_alike_share_one_plan() {
    // A local, and a matrix a buffer's member lays out by its own stride.
    let module = |loads: usize| {
        let mut body = String::new();
        for k in 0..loads {
            let _ = writeln!(
                body,
                "%l{k} = OpLoad %uint %local\n%pm{k} = OpAccessChain %mat_ptr %mbuf %zero\n%m{k} = OpLoad %mat2 %pm{k}"
            );
        }
        format!(
            r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %mbuf
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %mbuf DescriptorSet 0
OpDecorate %mbuf Binding 0
OpDecorate %mblock Block
OpMemberDecorate %mblock 0 Offset 0
OpMemberDecorate %mblock 0 ColMajor
OpMemberDecorate %mblock 0 MatrixStride 16
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%mat2 = OpTypeMatrix %v2float 2
%zero = OpConstant %uint 0
%mblock = OpTypeStruct %mat2
%mblock_ptr = OpTypePointer StorageBuffer %mblock
%mat_ptr = OpTypePointer StorageBuffer %mat2
%fn_uint = OpTypePointer Function %uint
%mbuf = OpVariable %mblock_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %fn_uint Function
OpStore %local %zero
{body}OpReturn
OpFunctionEnd
"#
        )
    };
    let tables = |loads: usize| {
        let program = lowered(&module(loads), "main").expect("lowered");
        (program.plans.len(), program.scalars.len())
    };
    assert_eq!(tables(3), tables(1), "each load's plan made once");
}

/// A compute shader reading its invocation's id, a vector of three words
/// per lane, as `body` says, its declarations beside `decls`.
fn over_ids(decls: &str, body: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf
OpExecutionMode %main LocalSize 64 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v2uint = OpTypeVector %uint 2
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
{decls}
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
{body}
%at = OpAccessChain %word_ptr %buf %zero %zero
OpStore %at %result
OpReturn
OpFunctionEnd
"#
    )
}

/// The words each copy `program` keeps moves, in order.
fn copies(program: &Program) -> alloc::vec::Vec<u32> {
    program
        .insts
        .iter()
        .filter_map(|inst| match inst {
            crate::software::shader::ir::Inst::Copy { words, .. } => Some(*words),
            _ => None,
        })
        .collect()
}

#[test]
fn components_and_runs_of_them_are_read_where_they_lie() {
    let read = |body: &str| {
        let text = over_ids("", body);
        copies(&lowered(&text, "main").expect("lowered"))
    };
    let components = "%x = OpCompositeExtract %uint %id 0\n%y = OpCompositeExtract %uint %id 2\n%result = OpIAdd %uint %x %y";
    assert_eq!(read(components), [], "components read in place");
    let run = "%yz = OpVectorShuffle %v2uint %id %id 1 2\n%result = OpCompositeExtract %uint %yz 1";
    assert_eq!(read(run), [], "a run of one source");
    let overlapping = "%xy = OpVectorShuffle %v2uint %id %id 0 1\n%yz = OpVectorShuffle %v2uint %id %id 1 2\n%v = OpCompositeConstruct %v4uint %xy %yz\n%result = OpCompositeExtract %uint %v 3";
    assert_eq!(
        read(overlapping),
        [2, 2],
        "one register's parts, not in order"
    );
    let wrapped =
        "%zx = OpVectorShuffle %v2uint %id %id 2 3\n%result = OpCompositeExtract %uint %zx 1";
    assert_eq!(read(wrapped), [1, 1], "a run never wraps round its source");
    let across = "%twice = OpIAdd %v3uint %id %id\n%xy = OpVectorShuffle %v2uint %id %twice 0 4\n%result = OpCompositeExtract %uint %xy 1";
    assert_eq!(
        read(across),
        [1, 1],
        "a run never crosses from one source to the next"
    );
    let swapped =
        "%zy = OpVectorShuffle %v2uint %id %id 2 1\n%result = OpCompositeExtract %uint %zy 0";
    assert_eq!(read(swapped), [1, 1], "no run");
    let spread =
        "%xxy = OpVectorShuffle %v3uint %id %id 0 0 1\n%result = OpCompositeExtract %uint %xxy 2";
    assert_eq!(read(spread), [1, 2], "a pick, then a run");
    let rebuilt = "%x = OpCompositeExtract %uint %id 0\n%y = OpCompositeExtract %uint %id 1\n%z = OpCompositeExtract %uint %id 2\n%v = OpCompositeConstruct %v3uint %x %y %z\n%result = OpCompositeExtract %uint %v 2";
    assert_eq!(read(rebuilt), [], "rebuilt from its own words in order");
}

#[test]
fn a_part_of_a_large_composite_is_copied_out() {
    let decls = "%c32 = OpConstant %uint 32\n%arr = OpTypeArray %uint %c32\n%fn_arr = OpTypePointer Function %arr";
    let body = "%x = OpCompositeExtract %uint %id 0\n%filled = OpCompositeConstruct %arr";
    let mut body = String::from(body);
    for _ in 0..32 {
        body.push_str(" %x");
    }
    body.push_str("\n%result = OpCompositeExtract %uint %filled 7");
    let program = lowered(&over_ids(decls, &body), "main").expect("lowered");
    assert_eq!(
        copies(&program),
        [1; 33],
        "the construction's own words, then the one part copied out"
    );
}

#[test]
fn an_insertion_into_a_value_it_outlives_takes_its_place() {
    let chain = "%a = OpCompositeExtract %uint %id 0\n%b = OpCompositeExtract %uint %id 1\n%v0 = OpCompositeConstruct %v4uint %a %a %b %b\n%v1 = OpCompositeInsert %v4uint %b %v0 0\n%v2 = OpCompositeInsert %v4uint %a %v1 1\n%v3 = OpCompositeInsert %v4uint %b %v2 2\n%v4 = OpCompositeInsert %v4uint %a %v3 3\n%result = OpCompositeExtract %uint %v4 1";
    let program = lowered(&over_ids("", chain), "main").expect("lowered");
    // The first value's parts, the second read after the first as a run,
    // then each insertion's own word.
    assert_eq!(copies(&program), [1, 2, 1, 1, 1, 1, 1]);
    let kept_too = chain.replace(
        "%result = OpCompositeExtract %uint %v4 1",
        "%back = OpCompositeExtract %uint %v1 0\n%front = OpCompositeExtract %uint %v4 1\n%result = OpIAdd %uint %back %front",
    );
    let program = lowered(&over_ids("", &kept_too), "main").expect("lowered");
    assert_eq!(
        copies(&program).iter().filter(|&&words| words == 4).count(),
        1,
        "a value still read is copied before it is changed"
    );
}

/// A float narrowing decorated `FPRoundingMode RTZ` lowers to the toward-zero
/// conversion, and one decorated `RTE`, or not at all, to nearest.
#[test]
fn a_narrowing_rounds_as_its_rounding_mode_says() {
    let text = |decoration: &str| {
        format!(
            r#"OpCapability Shader
OpCapability Float16
OpCapability StorageBuffer16BitAccess
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf %src
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %src DescriptorSet 0
OpDecorate %src Binding 1
OpDecorate %block Block
OpDecorate %floats Block
OpMemberDecorate %block 0 Offset 0
OpMemberDecorate %floats 0 Offset 0
OpDecorate %halves ArrayStride 2
{decoration}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%half = OpTypeFloat 16
%halves = OpTypeRuntimeArray %half
%block = OpTypeStruct %halves
%floats = OpTypeStruct %float
%sb_block = OpTypePointer StorageBuffer %block
%sb_floats = OpTypePointer StorageBuffer %floats
%buf = OpVariable %sb_block StorageBuffer
%src = OpVariable %sb_floats StorageBuffer
%sb_half = OpTypePointer StorageBuffer %half
%sb_float = OpTypePointer StorageBuffer %float
%zero = OpConstant %uint 0
%main = OpFunction %void None %fn
%entry = OpLabel
%q = OpAccessChain %sb_float %src %zero
%x = OpLoad %float %q
%p = OpAccessChain %sb_half %buf %zero %zero
%narrow = OpFConvert %half %x
OpStore %p %narrow
OpReturn
OpFunctionEnd
"#
        )
    };
    let conversion = |decoration: &str| {
        lowered(&text(decoration), "main")
            .expect("lowered")
            .insts
            .iter()
            .find_map(|inst| match inst {
                Inst::Convert { op, .. } => Some(*op),
                _ => None,
            })
    };
    assert_eq!(
        conversion("OpDecorate %narrow FPRoundingMode RTZ"),
        Some(Conv::FloatTowardZero)
    );
    assert_eq!(
        conversion("OpDecorate %narrow FPRoundingMode RTE"),
        Some(Conv::Float)
    );
    assert_eq!(conversion(""), Some(Conv::Float));
}

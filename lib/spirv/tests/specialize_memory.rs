//! What specializing a module holds at its peak: an insertion builds its
//! deepest level first and lists nothing on the way, so what it holds is what
//! it keeps, however deep into a null composite it goes.

use std::fmt::Write as _;

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_spirv::assemble::admit;
use tairix_spirv::{Admitted, Constant, Op, SpecError, Specialized};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn inserting_deep_into_a_null_composite_holds_only_the_level_it_keeps() {
    const LEVELS: usize = 20;
    const LENGTH: u32 = 1 << 21;
    let mut decls = format!("%len = OpConstant %uint {LENGTH}\n%one = OpConstant %uint 1\n");
    let mut element = String::from("%uint");
    for level in 1..=LEVELS {
        let _ = writeln!(decls, "%a{level} = OpTypeArray {element} %len");
        element = format!("%a{level}");
    }
    let _ = writeln!(
        decls,
        "%null = OpConstantNull %a{LEVELS}\n%deep = OpSpecConstantOp %a{LEVELS} CompositeInsert %one %null{}",
        " 0".repeat(LEVELS)
    );
    let text = format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
{decls}%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#
    );
    let admitted = admit(&text).unwrap_or_else(|refusal| panic!("{refusal:?}"));
    let (specialized, metering) = metered(|| admitted.specialize(&[]));
    assert_eq!(specialized.err(), Some(SpecError::Budget));
    // One level of nulls is kept before the budget refuses the next.
    let level = LENGTH as usize * size_of::<u32>();
    assert!(
        metering.peak < level + (level >> 2),
        "{} MiB held inserting {LEVELS} levels into {LENGTH} nulls",
        metering.peak >> 20
    );
}

#[test]
fn evaluating_operations_allocates_only_as_the_kept_tables_grow() {
    const CHAIN: usize = 4096;
    let mut decls = String::from(
        "%s0 = OpSpecConstant %uint 1\n%v0 = OpSpecConstantComposite %uvec4 %s0 %c2 %c3 %c4\n",
    );
    for i in 1..=CHAIN {
        let p = i - 1;
        let _ = writeln!(
            decls,
            "%s{i} = OpSpecConstantOp %uint IAdd %s{p} %s0\n\
             %v{i} = OpSpecConstantOp %uvec4 IMul %v{p} %v0\n\
             %w{i} = OpSpecConstantOp %uvec4 VectorShuffle %v{i} %v0 0 5 2 7\n\
             %x{i} = OpSpecConstantOp %uvec4 CompositeInsert %s{i} %w{i} 2"
        );
    }
    let text = format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%uvec4 = OpTypeVector %uint 4
%c2 = OpConstant %uint 2
%c3 = OpConstant %uint 3
%c4 = OpConstant %uint 4
{decls}%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#
    );
    let admitted = admit(&text).unwrap_or_else(|refusal| panic!("{refusal:?}"));
    let (specialized, metering) = metered(|| admitted.specialize(&[]));
    let specialized = specialized.expect("specialized");
    let powered = (0..=CHAIN).fold(1u64, |n, _| n.wrapping_mul(3) & u64::from(u32::MAX));
    assert_eq!(last_vector(&admitted, &specialized, 1), [1, 2, powered, 4]);
    assert_eq!(
        last_vector(&admitted, &specialized, 0),
        [1, 2, CHAIN as u64 + 1, 4]
    );
    assert!(
        metering.allocations < 128,
        "{} allocations evaluating {CHAIN} rounds of operations",
        metering.allocations
    );
}

/// The components of the operation `back` places before the last one.
fn last_vector(admitted: &Admitted, specialized: &Specialized, back: usize) -> [u64; 4] {
    let module = admitted.module();
    let id = module
        .instructions()
        .iter()
        .filter(|i| i.op() == Op::SPEC_CONSTANT_OP)
        .nth_back(back)
        .and_then(|i| module.result(i))
        .expect("an operation");
    let constant = specialized.constant(admitted, id).expect("a constant");
    let Some(Constant::Composite(parts)) = specialized.value(constant) else {
        panic!("a vector");
    };
    let mut components = [0; 4];
    for (component, &part) in components.iter_mut().zip(parts) {
        let Some(Constant::Scalar(bits)) = specialized.value(part) else {
            panic!("a scalar");
        };
        *component = bits;
    }
    components
}

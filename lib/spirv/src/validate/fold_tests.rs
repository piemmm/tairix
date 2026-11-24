//! The fold held to the evaluation it writes out: every constant of a folded
//! module evaluates as the original's specialization did, nothing of the
//! specialization is left, and the folded module is admitted.

use alloc::vec::Vec;

use super::FoldError;
use crate::assemble::admit;
use crate::enumerant::Decoration;
use crate::{Admitted, Constant, ConstantId, Id, Op, SpecValue, Specialized, Value};

/// Every kind of specialization constant: scalars of each width, a Boolean,
/// a composite, total operations over scalars and vectors, and the
/// workgroup size.
const SPECIALIZED: &str = r#"
OpCapability Shader
OpCapability Int64
OpCapability Int16
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpDecorate %wide SpecId 1
OpDecorate %narrow SpecId 2
OpDecorate %flag SpecId 3
OpDecorate %x SpecId 4
OpDecorate %kept SpecId 5
OpDecorate %size BuiltIn WorkgroupSize
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%ulong = OpTypeInt 64 0
%short = OpTypeInt 16 1
%v3uint = OpTypeVector %uint 3
%v2int = OpTypeVector %int 2
%wide = OpSpecConstant %ulong 5
%narrow = OpSpecConstant %short 3
%flag = OpSpecConstantTrue %bool
%x = OpSpecConstant %uint 2
%kept = OpSpecConstant %int -7
%one = OpConstant %uint 1
%zero = OpConstant %uint 0
%size = OpSpecConstantComposite %v3uint %x %one %one
%quotient = OpSpecConstantOp %uint UDiv %one %zero
%sum = OpSpecConstantOp %uint IAdd %x %one
%pair = OpSpecConstantComposite %v2int %kept %kept
%twice = OpSpecConstantOp %v2int IAdd %pair %pair
%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#;

/// The values the test specializes with: one past 32 bits, a negative
/// narrow integer, a false Boolean and a workgroup width; `%kept` keeps its
/// default.
const VALUES: [SpecValue; 4] = [
    SpecValue {
        id: 1,
        bits: 1 << 40,
    },
    SpecValue {
        id: 2,
        bits: 0xfffe,
    },
    SpecValue { id: 3, bits: 0 },
    SpecValue { id: 4, bits: 8 },
];

/// Whether `a` of `left` and `b` of `right` are the same value, constituent
/// by constituent.
fn same(left: (&Specialized, ConstantId), right: (&Specialized, ConstantId)) -> bool {
    match (left.0.value(left.1), right.0.value(right.1)) {
        (Some(Constant::Scalar(a)), Some(Constant::Scalar(b))) => a == b,
        (Some(Constant::Null), Some(Constant::Null)) => true,
        (Some(Constant::Composite(a)), Some(Constant::Composite(b))) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(&a, &b)| same((left.0, a), (right.0, b)))
        }
        _ => false,
    }
}

/// The ids every constant of `admitted` is declared under.
fn constant_ids(admitted: &Admitted) -> Vec<Id> {
    let module = admitted.module();
    module
        .instructions()
        .iter()
        .take_while(|instruction| instruction.op() != Op::FUNCTION)
        .filter(|instruction| {
            matches!(
                instruction.op(),
                Op::CONSTANT
                    | Op::CONSTANT_TRUE
                    | Op::CONSTANT_FALSE
                    | Op::CONSTANT_COMPOSITE
                    | Op::SPEC_CONSTANT
                    | Op::SPEC_CONSTANT_TRUE
                    | Op::SPEC_CONSTANT_FALSE
                    | Op::SPEC_CONSTANT_COMPOSITE
                    | Op::SPEC_CONSTANT_OP
            )
        })
        .filter_map(|instruction| module.result(instruction))
        .collect()
}

#[test]
fn every_constant_folds_to_the_value_its_specialization_evaluated() {
    let admitted = admit(SPECIALIZED).expect("admitted");
    let specialized = admitted.specialize(&VALUES).expect("specialized");
    let folded = admitted.fold(&specialized).expect("folded");
    let refolded = folded.specialize(&[]).expect("specialized again");
    for id in constant_ids(&admitted) {
        let was = specialized.constant(&admitted, id).expect("evaluated");
        let is = refolded.constant(&folded, id).expect("still declared");
        assert!(same((&specialized, was), (&refolded, is)), "{id:?} changed");
    }
    assert_eq!(refolded.workgroup_size(0), Some([8, 1, 1]));
    assert_eq!(specialized.workgroup_size(0), Some([8, 1, 1]));
}

#[test]
fn nothing_of_the_specialization_is_left_in_a_folded_module() {
    let admitted = admit(SPECIALIZED).expect("admitted");
    let folded = admitted
        .fold(&admitted.specialize(&VALUES).expect("specialized"))
        .expect("folded");
    let module = folded.module();
    for instruction in module.instructions() {
        let op = instruction.op();
        assert!(
            !matches!(
                op,
                Op::SPEC_CONSTANT
                    | Op::SPEC_CONSTANT_TRUE
                    | Op::SPEC_CONSTANT_FALSE
                    | Op::SPEC_CONSTANT_COMPOSITE
                    | Op::SPEC_CONSTANT_OP
            ),
            "{op:?} left"
        );
        let names_spec_id = op == Op::DECORATE
            && module.operands(instruction).get(1).map(|o| module.value(o))
                == Some(Value::Word(Decoration::SPEC_ID.0));
        assert!(!names_spec_id, "a SpecId left");
    }
}

#[test]
fn a_narrow_signed_constant_is_sign_extended_and_a_wide_one_written_whole() {
    let admitted = admit(SPECIALIZED).expect("admitted");
    let folded = admitted
        .fold(&admitted.specialize(&VALUES).expect("specialized"))
        .expect("folded");
    let module = folded.module();
    let literal_of = |name: Id| {
        let instruction = module.definition(name).expect("declared");
        assert_eq!(instruction.op(), Op::CONSTANT);
        module
            .operands(instruction)
            .get(2)
            .map(|o| module.value(o))
            .expect("a literal")
    };
    let id_of = |index: usize| constant_ids(&admitted)[index];
    assert_eq!(literal_of(id_of(0)), Value::Number(1 << 40));
    // Its word, sign-extended past its sixteen bits.
    assert_eq!(literal_of(id_of(1)), Value::Number(0xffff_fffe));
    let flag = module.definition(id_of(2)).expect("declared");
    assert_eq!(flag.op(), Op::CONSTANT_FALSE);
}

#[test]
fn a_module_with_no_specialization_folds_to_itself() {
    let plain = SPECIALIZED
        .lines()
        .filter(|line| !line.contains("SpecId") && !line.contains("OpSpecConstant"))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("OpDecorate %size BuiltIn WorkgroupSize", "");
    let text = plain.replace(
        "OpEntryPoint GLCompute %main \"main\"",
        "OpEntryPoint GLCompute %main \"main\"\nOpExecutionMode %main LocalSize 4 1 1",
    );
    let admitted = admit(&text).expect("admitted");
    let folded = admitted
        .fold(&admitted.specialize(&[]).expect("specialized"))
        .expect("folded");
    assert_eq!(folded.module().bytes(), admitted.module().bytes());
}

#[test]
fn a_specialization_of_another_module_is_refused() {
    let admitted = admit(SPECIALIZED).expect("admitted");
    let other = admit(
        "OpCapability Shader\nOpMemoryModel Logical GLSL450\nOpEntryPoint GLCompute %main \"main\"\nOpExecutionMode %main LocalSize 1 1 1\n%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%main = OpFunction %void None %fn\n%entry = OpLabel\nOpReturn\nOpFunctionEnd\n",
    )
    .expect("admitted");
    let theirs = other.specialize(&[]).expect("specialized");
    assert_eq!(admitted.fold(&theirs).err(), Some(FoldError::Malformed));
}

#[test]
fn every_value_a_folded_module_could_read_unwritten_is_null() {
    let admitted = admit(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %private %shared
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v2 = OpTypeVector %uint 2
%outside = OpUndef %uint
%private_ptr = OpTypePointer Private %v2
%private = OpVariable %private_ptr Private
%shared_ptr = OpTypePointer Workgroup %uint
%shared = OpVariable %shared_ptr Workgroup
%local_ptr = OpTypePointer Function %uint
%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %local_ptr Function
%inside = OpUndef %v2
OpReturn
OpFunctionEnd
"#,
    )
    .expect("admitted");
    let folded = admitted
        .fold(&admitted.specialize(&[]).expect("specialized"))
        .expect("folded");
    let module = folded.module();
    let op_of = |id: Id| module.definition(id).expect("defined").op();
    let id_of = |name: &str| {
        admitted
            .module()
            .instructions()
            .iter()
            .filter_map(|instruction| admitted.module().result(instruction))
            .nth(match name {
                "outside" => 4,
                "private" => 6,
                "shared" => 8,
                "local" => 12,
                _ => 13,
            })
            .expect("declared")
    };
    assert_eq!(op_of(id_of("outside")), Op::CONSTANT_NULL);
    assert_eq!(op_of(id_of("inside")), Op::COPY_OBJECT);
    for variable in ["private", "shared", "local"] {
        let instruction = module.definition(id_of(variable)).expect("declared");
        let initializer = match module.operands(instruction).get(3).map(|o| module.value(o)) {
            Some(Value::Id(initializer)) => initializer,
            other => panic!("{variable} has no initializer: {other:?}"),
        };
        assert_eq!(op_of(initializer), Op::CONSTANT_NULL, "{variable}");
    }
    assert!(module
        .instructions()
        .iter()
        .all(|instruction| instruction.op() != Op::UNDEF));
}

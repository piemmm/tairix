//! Specialization's budget held without wrapping, and every operation's value.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use super::{fits, MAX_SPECIALIZED};
use crate::assemble::admit;
use crate::{Admitted, Constant, Op, SpecError, Specialized};

#[test]
fn the_budget_is_held_however_large_a_count_is() {
    assert!(fits(0, MAX_SPECIALIZED - 1));
    assert!(!fits(0, MAX_SPECIALIZED));
    assert!(!fits(1, MAX_SPECIALIZED - 1));
    assert!(!fits(MAX_SPECIALIZED, 0));
    assert!(!fits(MAX_SPECIALIZED + 5, 0));
    assert!(!fits(3, usize::MAX), "no sum to wrap");
}

/// A compute shader whose declarations are `decls`.
fn module(decls: &str) -> String {
    format!(
        r#"OpCapability Shader
OpCapability Int64
OpCapability Float16
OpCapability Float64
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%ulong = OpTypeInt 64 0
%half = OpTypeFloat 16
%float = OpTypeFloat 32
%double = OpTypeFloat 64
%v2uint = OpTypeVector %uint 2
%v2bool = OpTypeVector %bool 2
{decls}
%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn an_insertion_into_a_null_of_the_longest_array_is_refused() {
    let text = module(
        "%most = OpConstant %uint 4294967295\n%one = OpConstant %uint 1\n%huge = OpTypeArray %uint %most\n%null = OpConstantNull %huge\n%planted = OpSpecConstantOp %huge CompositeInsert %one %null 0",
    );
    let admitted = admit(&text).expect("admitted");
    assert_eq!(admitted.specialize(&[]).err(), Some(SpecError::Budget));
}

/// The value of every `OpSpecConstantOp`, in order: its scalar's bits, or
/// each of its vector's.
fn results(admitted: &Admitted, specialized: &Specialized) -> Vec<Vec<u64>> {
    let module = admitted.module();
    module
        .instructions()
        .iter()
        .filter(|i| i.op() == Op::SPEC_CONSTANT_OP)
        .map(|i| {
            let id = module.result(i).expect("a result");
            let constant = specialized.constant(admitted, id).expect("a constant");
            let scalar = |part| match specialized.value(part) {
                Some(Constant::Scalar(bits)) => bits,
                other => panic!("not a scalar: {other:?}"),
            };
            match specialized.value(constant) {
                Some(Constant::Scalar(bits)) => alloc::vec![bits],
                Some(Constant::Composite(parts)) => parts.iter().copied().map(scalar).collect(),
                other => panic!("not a value: {other:?}"),
            }
        })
        .collect()
}

#[test]
fn every_operation_computes_its_value() {
    let cases: [(&str, &[u64]); 31] = [
        ("%ulong UConvert %a", &[0xF0F0_0F0F]),
        ("%uint UConvert %wide", &[5]),
        ("%float FConvert %h", &[0x3FC0_0000]),
        ("%double FConvert %f", &[0x3FF8_0000_0000_0000]),
        ("%float FConvert %tenth", &[0x3DCC_CCCD]),
        ("%uint Not %a", &[0x0F0F_F0F0]),
        ("%uint ISub %three %a", &[0x0F0F_F0F4]),
        ("%uint ShiftRightLogical %a %three", &[0x1E1E_01E1]),
        ("%uint ShiftRightLogical %a %past", &[0]),
        ("%uint BitwiseOr %a %b", &[0xFFF0_FF0F]),
        ("%uint BitwiseXor %a %b", &[0xFF00_FF00]),
        ("%uint BitwiseAnd %a %b", &[0x00F0_000F]),
        ("%bool LogicalOr %yes %no", &[1]),
        ("%bool LogicalAnd %yes %no", &[0]),
        ("%bool LogicalNot %no", &[1]),
        ("%bool LogicalEqual %yes %no", &[0]),
        ("%bool LogicalNotEqual %yes %no", &[1]),
        ("%bool IEqual %a %a", &[1]),
        ("%bool INotEqual %a %b", &[1]),
        ("%bool ULessThan %b %a", &[1]),
        ("%bool UGreaterThan %b %a", &[0]),
        ("%bool ULessThanEqual %a %a", &[1]),
        ("%bool UGreaterThanEqual %b %a", &[0]),
        ("%bool SGreaterThan %minus %three_signed", &[0]),
        ("%bool SLessThanEqual %minus %three_signed", &[1]),
        ("%bool SGreaterThanEqual %three_signed %minus", &[1]),
        ("%v2uint Select %yes %left %right", &[3, 0xF0F0_0F0F]),
        ("%v2uint Select %no %left %right", &[0x0FF0_F00F, 33]),
        ("%v2uint Select %mixed %left %right", &[3, 33]),
        ("%v2uint IAdd %left %right", &[0x0FF0_F012, 0xF0F0_0F30]),
        ("%v2uint ShiftLeftLogical %left %right", &[0, 0]),
    ];
    let mut decls = String::from(
        "%a = OpConstant %uint 0xF0F00F0F\n%b = OpConstant %uint 0x0FF0F00F\n%three = OpConstant %uint 3\n%past = OpConstant %uint 33\n%minus = OpConstant %int -5\n%three_signed = OpConstant %int 3\n%yes = OpConstantTrue %bool\n%no = OpConstantFalse %bool\n%wide = OpConstant %ulong 0x100000005\n%h = OpConstant %half 1.5\n%f = OpConstant %float 1.5\n%tenth = OpConstant %double 0.1\n%left = OpConstantComposite %v2uint %three %a\n%right = OpConstantComposite %v2uint %b %past\n%mixed = OpConstantComposite %v2bool %yes %no\n",
    );
    for (at, (operation, _)) in cases.iter().enumerate() {
        let (ty, rest) = operation.split_once(' ').expect("a type");
        let _ = writeln!(decls, "%op{at} = OpSpecConstantOp {ty} {rest}");
    }
    let admitted = admit(&module(&decls)).unwrap_or_else(|refusal| panic!("{refusal:?}"));
    let specialized = admitted.specialize(&[]).expect("specialized");
    for ((operation, expected), got) in cases.iter().zip(results(&admitted, &specialized)) {
        assert_eq!(got, *expected, "{operation}");
    }
}

/// A float conversion between constants takes each width's one quiet `NaN`
/// for every `NaN`, as the same conversion run in a shader does, rather than
/// whichever sign and payload a host cast keeps.
#[test]
fn a_constant_nan_converts_to_its_widths_one_quiet_nan() {
    let cases: [(&str, u64); 5] = [
        ("%half FConvert %payload", 0x7e00),
        ("%half FConvert %negative", 0x7e00),
        ("%double FConvert %payload", 0x7ff8_0000_0000_0000),
        ("%float FConvert %half_payload", 0x7fc0_0000),
        ("%float FConvert %double_payload", 0x7fc0_0000),
    ];
    let mut decls = String::from(
        "%payload = OpConstant %float 0x7FC02000\n%negative = OpConstant %float 0xFFC00000\n%half_payload = OpConstant %half 0x7E01\n%double_payload = OpConstant %double 0xFFF8000000000042\n",
    );
    for (at, (operation, _)) in cases.iter().enumerate() {
        let (ty, rest) = operation.split_once(' ').expect("a type");
        let _ = writeln!(decls, "%op{at} = OpSpecConstantOp {ty} {rest}");
    }
    let admitted = admit(&module(&decls)).unwrap_or_else(|refusal| panic!("{refusal:?}"));
    let specialized = admitted.specialize(&[]).expect("specialized");
    for ((operation, expected), got) in cases.iter().zip(results(&admitted, &specialized)) {
        assert_eq!(got, [*expected], "{operation}");
    }
}

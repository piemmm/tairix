//! The lowering view and specialization, read from assembled modules.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use super::assemble::admit;
use crate::enumerant::{BuiltIn, Decoration, Dim, StorageClass};
use crate::{Admitted, Constant, Id, Op, Scalar, SpecError, SpecValue, Specialized, Type};

/// A compute shader whose declarations are `declarations`, with a storage
/// buffer of a structure laid out by decorations, as every test below reads.
fn module(annotations: &str, declarations: &str) -> String {
    format!(
        r#"
OpCapability Shader
OpCapability Int64
OpCapability Float16
OpCapability Float64
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf
OpExecutionMode %main LocalSize 4 2 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpMemberDecorate %block 1 Offset 16
OpMemberDecorate %block 1 MatrixStride 16
OpMemberDecorate %block 1 RowMajor
OpMemberDecorate %block 2 Offset 48
OpDecorate %floats ArrayStride 4
{annotations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%ulong = OpTypeInt 64 0
%float = OpTypeFloat 32
%half = OpTypeFloat 16
%v2float = OpTypeVector %float 2
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%mat2 = OpTypeMatrix %v2float 2
%floats = OpTypeRuntimeArray %float
%block = OpTypeStruct %v4uint %mat2 %floats
%sb_block = OpTypePointer StorageBuffer %block
%buf = OpVariable %sb_block StorageBuffer
{declarations}
%main = OpFunction %void None %fn
%entry = OpLabel
OpReturn
OpFunctionEnd
"#
    )
}

fn id_named(admitted: &Admitted, op: Op, nth: usize) -> Id {
    let module = admitted.module();
    module
        .instructions()
        .iter()
        .filter(|i| i.op() == op)
        .nth(nth)
        .and_then(|i| module.result(i))
        .expect("the instruction")
}

#[test]
fn a_structures_members_carry_the_layout_their_decorations_give() {
    let admitted = admit(&module("", "")).expect("admitted");
    let block = admitted
        .type_id(id_named(&admitted, Op::TYPE_STRUCT, 0))
        .expect("a type");
    assert_eq!(admitted.ty(block), Some(Type::Struct { members: 3 }));
    let first = admitted.member(block, 0).expect("a member");
    assert_eq!(first.offset, Some(0));
    assert!(matches!(
        admitted.ty(first.ty),
        Some(Type::Vector {
            scalar: Scalar::Int {
                width: 32,
                signed: false
            },
            count: 4
        })
    ));
    let matrix = admitted.member(block, 1).expect("a member");
    assert_eq!(
        (matrix.offset, matrix.matrix_stride, matrix.row_major),
        (Some(16), Some(16), true)
    );
    let Some(Type::Matrix {
        columns: 2,
        rows: 2,
        column,
        ..
    }) = admitted.ty(matrix.ty)
    else {
        panic!("a matrix");
    };
    assert!(matches!(
        admitted.ty(column),
        Some(Type::Vector { count: 2, .. })
    ));
    let runtime = admitted.member(block, 2).expect("a member");
    let Some(Type::RuntimeArray { element }) = admitted.ty(runtime.ty) else {
        panic!("a runtime array");
    };
    assert_eq!(admitted.array_stride(runtime.ty), Some(4));
    assert_eq!(
        admitted.ty(element),
        Some(Type::Scalar(Scalar::Float { width: 32 }))
    );
    assert_eq!(admitted.member(block, 3), None);
}

#[test]
fn a_members_layout_is_read_whole_among_its_other_decorations() {
    let admitted = admit(&module(
        "OpMemberDecorate %block 1 NonWritable\nOpMemberDecorate %block 0 NonWritable",
        "",
    ))
    .expect("admitted");
    let block = admitted
        .type_id(id_named(&admitted, Op::TYPE_STRUCT, 0))
        .expect("a type");
    let first = admitted.member(block, 0).expect("a member");
    assert_eq!(
        (first.offset, first.matrix_stride, first.row_major),
        (Some(0), None, false)
    );
    let matrix = admitted.member(block, 1).expect("a member");
    assert_eq!(
        (matrix.offset, matrix.matrix_stride, matrix.row_major),
        (Some(16), Some(16), true)
    );
    assert_eq!(admitted.member_type(block, 1), Some(matrix.ty));
    assert_eq!(admitted.member_type(block, 3), None);
    let uint = admitted
        .type_id(id_named(&admitted, Op::TYPE_INT, 0))
        .expect("a type");
    assert_eq!(admitted.member_type(uint, 0), None);
    assert_eq!(admitted.type_id(id_named(&admitted, Op::VARIABLE, 0)), None);
}

#[test]
fn the_tables_an_admitted_module_keeps_hold_no_room_to_grow() {
    let (mut annotations, mut declarations) = (String::new(), String::new());
    for i in 0..40 {
        let _ = writeln!(annotations, "OpDecorate %k{i} SpecId {i}");
        let _ = writeln!(
            declarations,
            "%k{i} = OpSpecConstant %uint {i}\n%n{i} = OpConstant %uint {}\n%arr{i} = OpTypeArray %uint %n{i}",
            i + 1
        );
    }
    let admitted = admit(&module(&annotations, &declarations)).expect("admitted");
    let module = admitted.module();
    let kept = &admitted.kept;
    let room = [
        (
            "instructions",
            module.instructions.capacity() - module.instructions.len(),
        ),
        (
            "operands",
            module.operands.capacity() - module.operands.len(),
        ),
        (
            "definitions",
            module.definitions.capacity() - module.definitions.len(),
        ),
        ("types", kept.types.capacity() - kept.types.len()),
        ("lists", kept.lists.capacity() - kept.lists.len()),
        (
            "decorations",
            kept.decorations.capacity() - kept.decorations.len(),
        ),
        (
            "types by id",
            kept.types_by_id.capacity() - kept.types_by_id.len(),
        ),
    ];
    assert!(room.iter().all(|&(_, spare)| spare == 0), "{room:?}");
}

#[test]
fn a_variables_decorations_and_a_pointers_class_are_read() {
    let admitted = admit(&module("", "")).expect("admitted");
    let buf = id_named(&admitted, Op::VARIABLE, 0);
    assert_eq!(admitted.decoration(buf, None, Decoration::BINDING), Some(0));
    assert_eq!(admitted.decoration(buf, None, Decoration::LOCATION), None);
    let variable = admitted.module().definition(buf).expect("defined");
    let pointer = admitted.result_type(variable).expect("a type");
    assert!(matches!(
        admitted.ty(pointer),
        Some(Type::Pointer {
            class: StorageClass::STORAGE_BUFFER,
            ..
        })
    ));
    let function = admitted
        .type_id(id_named(&admitted, Op::TYPE_FUNCTION, 0))
        .expect("a type");
    assert!(matches!(
        admitted.ty(function),
        Some(Type::Function { parameters: 0, .. })
    ));
    assert_eq!(admitted.parameter(function, 0), None);
    let _ = (BuiltIn::POSITION, Dim::DIM_2D);
}

fn scalar(specialized: &Specialized, admitted: &Admitted, id: Id) -> u64 {
    let constant = specialized.constant(admitted, id).expect("a constant");
    match specialized.value(constant) {
        Some(Constant::Scalar(bits)) => bits,
        Some(Constant::Null) => 0,
        other => panic!("not a scalar: {other:?}"),
    }
}

/// The value of the `nth` `OpSpecConstantOp`.
fn op_result(admitted: &Admitted, specialized: &Specialized, nth: usize) -> u64 {
    scalar(
        specialized,
        admitted,
        id_named(admitted, Op::SPEC_CONSTANT_OP, nth),
    )
}

#[test]
fn a_specialization_constant_takes_its_value_or_keeps_its_default() {
    let text = module(
        "OpDecorate %count SpecId 3\nOpDecorate %flag SpecId 4",
        "%count = OpSpecConstant %uint 7\n%flag = OpSpecConstantTrue %bool\n%twice = OpSpecConstantOp %uint IAdd %count %count",
    );
    let admitted = admit(&text).expect("admitted");
    let count = id_named(&admitted, Op::SPEC_CONSTANT, 0);
    let flag = id_named(&admitted, Op::SPEC_CONSTANT_TRUE, 0);
    let defaults = admitted.specialize(&[]).expect("specialized");
    assert_eq!(scalar(&defaults, &admitted, count), 7);
    assert_eq!(scalar(&defaults, &admitted, flag), 1);
    assert_eq!(op_result(&admitted, &defaults, 0), 14);
    let given = admitted
        .specialize(&[SpecValue { id: 3, bits: 20 }, SpecValue { id: 4, bits: 0 }])
        .expect("specialized");
    assert_eq!(scalar(&given, &admitted, count), 20);
    assert_eq!(scalar(&given, &admitted, flag), 0);
    assert_eq!(op_result(&admitted, &given, 0), 40);
    assert_eq!(
        admitted.specialize(&[SpecValue { id: 9, bits: 1 }]).err(),
        Some(SpecError::UnknownId(9))
    );
    assert_eq!(
        admitted
            .specialize(&[SpecValue {
                id: 3,
                bits: 1 << 32
            }])
            .err(),
        Some(SpecError::Width(3))
    );
    assert_eq!(
        admitted
            .specialize(&[SpecValue { id: 3, bits: 1 }, SpecValue { id: 3, bits: 2 }])
            .err(),
        Some(SpecError::Duplicate(3))
    );
    assert_eq!(
        admitted.specialize(&[SpecValue { id: 4, bits: 2 }]).err(),
        Some(SpecError::Width(4)),
        "a Boolean is one or zero"
    );
}

#[test]
fn every_operation_is_total_over_its_edge_values() {
    let text = module(
        "OpDecorate %zero SpecId 0",
        r"
%zero = OpSpecConstant %uint 0
%seven = OpConstant %uint 7
%min = OpConstant %int 0x80000000
%minus = OpConstant %int -1
%big = OpConstant %uint 40
%by_zero = OpSpecConstantOp %uint UDiv %seven %zero
%rem_zero = OpSpecConstantOp %uint UMod %seven %zero
%min_div = OpSpecConstantOp %int SDiv %min %minus
%min_rem = OpSpecConstantOp %int SRem %min %minus
%past = OpSpecConstantOp %uint ShiftLeftLogical %seven %big
%past_right = OpSpecConstantOp %int ShiftRightArithmetic %min %big
%neg = OpSpecConstantOp %int SNegate %min
%smod = OpSpecConstantOp %int SMod %minus %seven
%less = OpSpecConstantOp %bool SLessThan %min %minus
%pick = OpSpecConstantOp %uint Select %less %seven %big
%wide = OpSpecConstantOp %ulong SConvert %min
",
    );
    let admitted = admit(&text).expect("admitted");
    let specialized = admitted.specialize(&[]).expect("specialized");
    let results: [u64; 10] = core::array::from_fn(|n| op_result(&admitted, &specialized, n));
    assert_eq!(results[0], 0, "a division by zero is zero");
    assert_eq!(results[1], 0, "a remainder by zero is zero");
    assert_eq!(results[2], 0x8000_0000, "MIN / -1 is MIN");
    assert_eq!(results[3], 0);
    assert_eq!(results[4], 0, "a shift past the width is zero");
    assert_eq!(results[5], 0);
    assert_eq!(results[6], 0x8000_0000, "MIN negates to itself");
    assert_eq!(results[7], 6, "SMod takes the divisor's sign");
    assert_eq!(results[8], 1);
    assert_eq!(results[9], 7);
    assert_eq!(
        op_result(&admitted, &specialized, 10),
        0xffff_ffff_8000_0000
    );
}

#[test]
fn composites_are_shuffled_extracted_and_inserted_by_reference() {
    let text = module(
        "OpDecorate %n SpecId 1",
        r"
%n = OpSpecConstant %uint 5
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%vec = OpSpecConstantComposite %v3uint %n %one %two
%swapped = OpSpecConstantOp %v3uint VectorShuffle %vec %vec 2 0 0xFFFFFFFF
%first = OpSpecConstantOp %uint CompositeExtract %swapped 1
%undefined = OpSpecConstantOp %uint CompositeExtract %swapped 2
%arr4 = OpTypeArray %v3uint %two
%fixed = OpConstantComposite %v3uint %two %one %one
%both = OpConstantComposite %arr4 %fixed %fixed
%nulls = OpConstantNull %arr4
%planted = OpSpecConstantOp %arr4 CompositeInsert %two %nulls 1 2
%back = OpSpecConstantOp %uint CompositeExtract %planted 1 2
%untouched = OpSpecConstantOp %uint CompositeExtract %planted 0 2
%deep = OpSpecConstantOp %uint CompositeExtract %both 1 0
",
    );
    let admitted = admit(&text).expect("admitted");
    let specialized = admitted
        .specialize(&[SpecValue { id: 1, bits: 9 }])
        .expect("specialized");
    assert_eq!(
        op_result(&admitted, &specialized, 1),
        9,
        "component 0 of the original"
    );
    assert_eq!(
        op_result(&admitted, &specialized, 2),
        0,
        "an undefined component is zero"
    );
    assert_eq!(
        op_result(&admitted, &specialized, 4),
        2,
        "the inserted value"
    );
    assert_eq!(
        op_result(&admitted, &specialized, 5),
        0,
        "the rest of a null array"
    );
    assert_eq!(
        op_result(&admitted, &specialized, 6),
        2,
        "through a constant array"
    );
}

#[test]
fn a_half_quantised_flushes_what_no_normal_half_holds() {
    let text = module(
        "",
        r"
%tiny = OpConstant %float 1e-6
%many = OpConstant %float 70000.0
%third = OpConstant %float 0.3333333
%q_tiny = OpSpecConstantOp %float QuantizeToF16 %tiny
%q_many = OpSpecConstantOp %float QuantizeToF16 %many
%q_third = OpSpecConstantOp %float QuantizeToF16 %third
",
    );
    let admitted = admit(&text).expect("admitted");
    let specialized = admitted.specialize(&[]).expect("specialized");
    assert_eq!(op_result(&admitted, &specialized, 0), 0);
    assert_eq!(
        op_result(&admitted, &specialized, 1),
        u64::from(f32::INFINITY.to_bits())
    );
    let third = f32::from_bits(u32::try_from(op_result(&admitted, &specialized, 2)).unwrap_or(0));
    assert!(
        (third - 0.333_251_95).abs() < 1e-9,
        "{third} is the nearest half"
    );
}

#[test]
fn a_double_converted_to_a_half_is_rounded_once() {
    let text = module(
        "",
        r"
%double = OpTypeFloat 64
%past = OpConstant %double 1.0004882812500002
%h = OpSpecConstantOp %half FConvert %past
",
    );
    let admitted = admit(&text).expect("admitted");
    let specialized = admitted.specialize(&[]).expect("specialized");
    assert_eq!(
        op_result(&admitted, &specialized, 0),
        0x3c01,
        "just past a tie rounds away from it"
    );
}

#[test]
fn a_workgroup_size_is_the_specialized_constant_and_never_zero() {
    let text = module(
        "OpDecorate %size BuiltIn WorkgroupSize\nOpDecorate %x SpecId 7",
        "%x = OpSpecConstant %uint 16\n%one = OpConstant %uint 1\n%size = OpSpecConstantComposite %v3uint %x %one %one",
    );
    let admitted = admit(&text).expect("admitted");
    assert_eq!(
        admitted
            .specialize(&[])
            .expect("specialized")
            .workgroup_size(0),
        Some([16, 1, 1])
    );
    assert_eq!(
        admitted
            .specialize(&[SpecValue { id: 7, bits: 64 }])
            .expect("specialized")
            .workgroup_size(0),
        Some([64, 1, 1])
    );
    assert_eq!(
        admitted.specialize(&[SpecValue { id: 7, bits: 0 }]).err(),
        Some(SpecError::Workgroup)
    );
    let fixed = admit(&module("", "")).expect("admitted");
    assert_eq!(
        fixed
            .specialize(&[])
            .expect("specialized")
            .workgroup_size(0),
        Some([4, 2, 1])
    );
}

#[test]
fn an_admitted_module_holds_at_least_its_words_and_more_for_more_of_them() {
    let small = admit(&module("", "")).expect("admitted");
    let mut constants = String::new();
    for k in 0..1000 {
        let _ = writeln!(constants, "%c{k} = OpConstant %uint {k}");
    }
    let large = admit(&module("", &constants)).expect("admitted");
    assert!(small.held_bytes() >= small.module().bytes().len() as u64);
    assert!(
        large.held_bytes() >= small.held_bytes() + 1000 * 16,
        "a thousand constants, sixteen bytes of words apiece and their tables"
    );
}

#[test]
fn a_composite_is_selected_whole_by_its_specialized_condition() {
    let text = module(
        "OpDecorate %flag SpecId 5",
        r"
%pair = OpTypeStruct %uint %uint
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%a = OpConstantComposite %pair %one %two
%b = OpConstantComposite %pair %two %one
%flag = OpSpecConstantTrue %bool
%chosen = OpSpecConstantOp %pair Select %flag %a %b
",
    );
    let admitted = admit(&text).expect("admitted");
    let named = |op, at| id_named(&admitted, op, at);
    let (a, b) = (
        named(Op::CONSTANT_COMPOSITE, 0),
        named(Op::CONSTANT_COMPOSITE, 1),
    );
    let chosen = named(Op::SPEC_CONSTANT_OP, 0);
    for (bits, picked) in [(1, a), (0, b)] {
        let specialized = admitted
            .specialize(&[SpecValue { id: 5, bits }])
            .expect("specialized");
        assert_eq!(
            specialized.constant(&admitted, chosen),
            specialized.constant(&admitted, picked),
            "the condition {bits} picks its composite whole"
        );
    }
}

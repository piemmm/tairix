use alloc::vec::Vec;

use super::assemble;
use crate::enumerant::{Decoration, ImageOperands};
use crate::{Module, Op, Version};

/// What an operand holds, its id and text left out.
#[derive(Debug, Eq, PartialEq)]
enum Read {
    Id,
    Word(u32),
    Number(u64),
    Text,
}

fn words_of(text: &str, op: Op) -> Vec<Read> {
    let bytes = assemble(text, Version::NEWEST);
    let module = Module::decode(&bytes).expect("decoded");
    let instruction = module
        .instructions()
        .iter()
        .find(|i| i.op() == op)
        .expect("the instruction");
    module
        .operands(instruction)
        .iter()
        .map(|o| match module.value(o) {
            crate::Value::Id(_) => Read::Id,
            crate::Value::Word(word) => Read::Word(word),
            crate::Value::Number(number) => Read::Number(number),
            crate::Value::Text(_) => Read::Text,
        })
        .collect()
}

#[test]
fn a_result_is_placed_after_its_type() {
    let bytes = assemble(
        "OpCapability Shader\n%t = OpTypeInt 32 0\n%c = OpConstant %t 7\n",
        Version::NEWEST,
    );
    let module = Module::decode(&bytes).expect("decoded");
    let constant = module.instructions().get(2).expect("a constant");
    assert_eq!(
        module.result_type(constant),
        module.result(module.instructions().get(1).expect("a type"))
    );
}

#[test]
fn an_enumerant_is_read_as_the_kind_its_operand_takes() {
    let decorated = words_of("OpDecorate %x Offset 4", Op::DECORATE);
    assert_eq!(decorated[1], Read::Word(Decoration::OFFSET.0));
    assert_eq!(decorated[2], Read::Word(4));
    let sampled = words_of(
        "%r = OpImageSampleExplicitLod %v %si %c Lod|Offset %lod %off",
        Op::IMAGE_SAMPLE_EXPLICIT_LOD,
    );
    assert_eq!(
        sampled[4],
        Read::Word(ImageOperands::LOD.0 | ImageOperands::OFFSET.0)
    );
    assert_eq!(sampled.len(), 7, "each set bit brings its parameter");
}

#[test]
fn a_literal_is_as_wide_as_its_constant_type() {
    let long = words_of(
        "%l = OpTypeInt 64 0\n%c = OpConstant %l 0x1_0000_0002",
        Op::CONSTANT,
    );
    assert_eq!(long[2], Read::Number(0x1_0000_0002));
    let double = words_of("%d = OpTypeFloat 64\n%c = OpConstant %d 1.5", Op::CONSTANT);
    assert_eq!(double[2], Read::Number(1.5f64.to_bits()));
    let float = words_of("%f = OpTypeFloat 32\n%c = OpConstant %f -2.5", Op::CONSTANT);
    assert_eq!(float[2], Read::Number(u64::from((-2.5f32).to_bits())));
    let small = words_of("%h = OpTypeFloat 16\n%c = OpConstant %h 0.5", Op::CONSTANT);
    assert_eq!(small[2], Read::Number(0x3800));
    // Rounded to a single first, this lands on a half's tie and then on 1.0.
    let past_a_tie = words_of(
        "%h = OpTypeFloat 16\n%c = OpConstant %h 1.0004882812500002",
        Op::CONSTANT,
    );
    assert_eq!(
        past_a_tie[2],
        Read::Number(0x3c01),
        "a half literal rounded once"
    );
}

#[test]
fn a_switch_literal_is_as_wide_as_its_selector() {
    let text = "%l = OpTypeInt 64 0\n%s = OpUndef %l\nOpSwitch %s %d 5 %a 0x1_0000_0000 %b";
    let words = words_of(text, Op::SWITCH);
    assert_eq!(words[2], Read::Number(5));
    assert_eq!(words[4], Read::Number(1 << 32));
}

#[test]
fn an_extended_instruction_and_a_nested_operation_are_named() {
    let ext = words_of(
        "%glsl = OpExtInstImport \"GLSL.std.450\"\n%r = OpExtInst %f %glsl Sqrt %x",
        Op::EXT_INST,
    );
    assert_eq!(ext[3], Read::Word(31));
    let nested = words_of("%r = OpSpecConstantOp %u IAdd %a %b", Op::SPEC_CONSTANT_OP);
    assert_eq!(nested[2], Read::Word(u32::from(Op::I_ADD.code())));
    assert_eq!(nested.len(), 5);
}

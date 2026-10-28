//! A test-only assembler: SPIR-V's assembly text to a module, through the
//! encoder, so a test reads as the shader it is.
//!
//! One instruction a line, `;` comments. `%name` is an id, assigned on first
//! sight so a forward reference is written as one; `"text"` a string; a
//! decimal or `0x` number a word; any other word an enumerant by its grammar
//! name, as [`enumerant`] knows it. `%result = OpName …` places the result
//! after the result type where the instruction has one.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::enumerant::{
    AddressingModel, BuiltIn, Capability, Decoration, Dim, ExecutionMode, ExecutionModel,
    GroupOperation, ImageFormat, ImageOperands, MemoryModel, StorageClass,
};
use crate::grammar;
use crate::tables::INSTRUCTIONS;
use crate::{Admitted, Arg, Encoder, Id, Module, Refusal, Rule, Version};

/// The value of the enumerant `name`, for the names the tests write.
fn enumerant(name: &str) -> Option<u32> {
    Some(match name {
        "None" => 0,
        "Shader" => Capability::SHADER.0,
        "Matrix" => Capability::MATRIX.0,
        "Int64" => Capability::INT64.0,
        "Float64" => Capability::FLOAT64.0,
        "Int16" => Capability::INT16.0,
        "Float16" => Capability::FLOAT16.0,
        "Int8" => Capability::INT8.0,
        "Kernel" => Capability::KERNEL.0,
        "Geometry" => Capability::GEOMETRY.0,
        "Addresses" => Capability::ADDRESSES.0,
        "ImageQuery" => Capability::IMAGE_QUERY.0,
        "ImageGatherExtended" => Capability::IMAGE_GATHER_EXTENDED.0,
        "Sampled1D" => Capability::SAMPLED1D.0,
        "GroupNonUniform" => Capability::GROUP_NON_UNIFORM.0,
        "GroupNonUniformArithmetic" => Capability::GROUP_NON_UNIFORM_ARITHMETIC.0,
        "GroupNonUniformBallot" => Capability::GROUP_NON_UNIFORM_BALLOT.0,
        "StorageBuffer16BitAccess" => Capability::STORAGE_BUFFER16_BIT_ACCESS.0,
        "StorageImageWriteWithoutFormat" => Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT.0,
        "Logical" => AddressingModel::LOGICAL.0,
        "Physical64" => AddressingModel::PHYSICAL64.0,
        "GLSL450" => MemoryModel::GLSL450.0,
        "Simple" => MemoryModel::SIMPLE.0,
        "Vertex" => ExecutionModel::VERTEX.0,
        "Fragment" => ExecutionModel::FRAGMENT.0,
        "GLCompute" => ExecutionModel::GL_COMPUTE.0,
        "LocalSize" => ExecutionMode::LOCAL_SIZE.0,
        "LocalSizeId" => ExecutionMode::LOCAL_SIZE_ID.0,
        "OriginUpperLeft" => ExecutionMode::ORIGIN_UPPER_LEFT.0,
        "OriginLowerLeft" => ExecutionMode::ORIGIN_LOWER_LEFT.0,
        "DepthReplacing" => ExecutionMode::DEPTH_REPLACING.0,
        "EarlyFragmentTests" => ExecutionMode::EARLY_FRAGMENT_TESTS.0,
        "UniformConstant" => StorageClass::UNIFORM_CONSTANT.0,
        "Input" => StorageClass::INPUT.0,
        "Uniform" => StorageClass::UNIFORM.0,
        "Output" => StorageClass::OUTPUT.0,
        "Workgroup" => StorageClass::WORKGROUP.0,
        "Private" => StorageClass::PRIVATE.0,
        "Function" => StorageClass::FUNCTION.0,
        "PushConstant" => StorageClass::PUSH_CONSTANT.0,
        "StorageBuffer" => StorageClass::STORAGE_BUFFER.0,
        "Block" => Decoration::BLOCK.0,
        "BufferBlock" => Decoration::BUFFER_BLOCK.0,
        "ArrayStride" => Decoration::ARRAY_STRIDE.0,
        "MatrixStride" => Decoration::MATRIX_STRIDE.0,
        "ColMajor" => Decoration::COL_MAJOR.0,
        "RowMajor" => Decoration::ROW_MAJOR.0,
        "Offset" => Decoration::OFFSET.0,
        "BuiltIn" => Decoration::BUILT_IN.0,
        "Location" => Decoration::LOCATION.0,
        "Component" => Decoration::COMPONENT.0,
        "Flat" => Decoration::FLAT.0,
        "Index" => Decoration::INDEX.0,
        "DescriptorSet" => Decoration::DESCRIPTOR_SET.0,
        "Binding" => Decoration::BINDING.0,
        "NonWritable" => Decoration::NON_WRITABLE.0,
        "NonReadable" => Decoration::NON_READABLE.0,
        "SpecId" => Decoration::SPEC_ID.0,
        "RelaxedPrecision" => Decoration::RELAXED_PRECISION.0,
        "NoContraction" => Decoration::NO_CONTRACTION.0,
        "GlobalInvocationId" => BuiltIn::GLOBAL_INVOCATION_ID.0,
        "LocalInvocationIndex" => BuiltIn::LOCAL_INVOCATION_INDEX.0,
        "WorkgroupSize" => BuiltIn::WORKGROUP_SIZE.0,
        "Position" => BuiltIn::POSITION.0,
        "PointSize" => BuiltIn::POINT_SIZE.0,
        "ClipDistance" => BuiltIn::CLIP_DISTANCE.0,
        "CullDistance" => BuiltIn::CULL_DISTANCE.0,
        "VertexIndex" => BuiltIn::VERTEX_INDEX.0,
        "FragCoord" => BuiltIn::FRAG_COORD.0,
        "FragDepth" => BuiltIn::FRAG_DEPTH.0,
        "1D" => Dim::DIM_1D.0,
        "2D" => Dim::DIM_2D.0,
        "3D" => Dim::DIM_3D.0,
        "Cube" => Dim::CUBE.0,
        "Buffer" => Dim::BUFFER.0,
        "Unknown" => ImageFormat::UNKNOWN.0,
        "Rgba8" => ImageFormat::RGBA8.0,
        "R32ui" => ImageFormat::R32UI.0,
        "R32f" => ImageFormat::R32F.0,
        "Lod" => ImageOperands::LOD.0,
        "Bias" => ImageOperands::BIAS.0,
        "ConstOffset" => ImageOperands::CONST_OFFSET.0,
        "Reduce" => GroupOperation::REDUCE.0,
        "InclusiveScan" => GroupOperation::INCLUSIVE_SCAN.0,
        // `GLSL.std.450` instruction numbers.
        "Sqrt" => 31,
        "Modf" => 35,
        "IMix" => 47,
        "FMix" => 46,
        "Fma" => 50,
        "Normalize" => 69,
        "InterpolateAtCentroid" => 76,
        _ => return None,
    })
}

/// The newest version, which the tests write unless they say otherwise.
pub(crate) const V1_6: Version = Version { major: 1, minor: 6 };

/// Assemble `text` as a module of `version`.
pub(crate) fn assemble(text: &str, version: Version) -> Vec<u8> {
    let mut encoder = Encoder::new(version, 0).expect("a header");
    let specs: BTreeMap<&str, &grammar::Spec> =
        INSTRUCTIONS.iter().map(|spec| (spec.name, spec)).collect();
    let mut names: BTreeMap<String, Id> = BTreeMap::new();
    let mut id = |encoder: &mut Encoder, name: &str| -> Id {
        if let Some(&id) = names.get(name) {
            return id;
        }
        let id = encoder.next_id().expect("an id");
        names.insert(name.into(), id);
        id
    };
    for line in text.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let tokens = tokenize(line);
        let (result, tokens) = match tokens.as_slice() {
            [result, "=", rest @ ..] if result.starts_with('%') => (Some(*result), rest),
            all => (None, all),
        };
        let (name, operands) = tokens.split_first().expect("an opcode");
        let spec = specs
            .get(name)
            .unwrap_or_else(|| panic!("unknown instruction {name}"));
        let mut words: Vec<Arg<'_>> = Vec::new();
        let ids: Vec<Option<Id>> = operands
            .iter()
            .map(|token| token.strip_prefix('%').map(|n| id(&mut encoder, n)))
            .collect();
        let result_id = result.map(|r| id(&mut encoder, r.trim_start_matches('%')));
        let mut operand_args = operands.iter().zip(&ids).map(|(token, id)| match id {
            Some(id) => Arg::Id(*id),
            None if token.starts_with('"') => Arg::Text(token.trim_matches('"')),
            None => Arg::Word(
                number(token)
                    .or_else(|| enumerant(token))
                    .or_else(|| opcode(token))
                    .unwrap_or_else(|| panic!("unknown operand {token}")),
            ),
        });
        if let Some(result_id) = result_id {
            if spec.has_result_type() {
                words.push(operand_args.next().expect("a result type"));
            }
            words.push(Arg::Id(result_id));
        }
        words.extend(operand_args);
        encoder.emit(spec.op, &words).expect("an instruction");
    }
    encoder.finish()
}

/// The opcode `name`, an instruction's name without its `Op`, names: the
/// operation an `OpSpecConstantOp` performs.
fn opcode(name: &str) -> Option<u32> {
    INSTRUCTIONS
        .iter()
        .find(|spec| spec.name.strip_prefix("Op") == Some(name))
        .map(|spec| u32::from(spec.op.code()))
}

fn number(token: &str) -> Option<u32> {
    match token.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => token
            .parse::<u32>()
            .ok()
            .or_else(|| token.parse::<i32>().ok().map(i32::cast_unsigned)),
    }
}

/// The line's tokens, a quoted string kept whole.
fn tokenize(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let end = if rest.starts_with('"') {
            rest.get(1..)
                .and_then(|r| r.find('"'))
                .map_or(rest.len(), |e| e + 2)
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        tokens.push(rest.get(..end).unwrap_or(rest));
        rest = rest.get(end..).unwrap_or("").trim_start();
    }
    tokens
}

/// Assemble and validate `text`.
pub(crate) fn admit(text: &str) -> Result<Admitted, Refusal> {
    admit_version(text, V1_6)
}

pub(crate) fn admit_version(text: &str, version: Version) -> Result<Admitted, Refusal> {
    let module = Module::decode(&assemble(text, version)).expect("a module the decoder takes");
    Admitted::new(module)
}

/// The rule `text` is refused by.
pub(crate) fn refused(text: &str) -> Rule {
    admit(text).expect_err("refused").rule
}

/// `text` with `from` replaced by `to`, which must occur in it.
pub(crate) fn with(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from:?} is not in the module");
    text.replacen(from, to, 1)
}

#[test]
fn the_assembler_places_a_result_after_its_type() {
    let bytes = assemble(
        "OpCapability Shader\n%t = OpTypeInt 32 0\n%c = OpConstant %t 7\n",
        V1_6,
    );
    let module = Module::decode(&bytes).expect("decoded");
    let constant = module.instructions().get(2).expect("a constant");
    assert_eq!(
        module.result_type(constant),
        module.result(module.instructions().get(1).expect("a type"))
    );
    assert!(grammar::instruction(constant.op()).is_some());
}

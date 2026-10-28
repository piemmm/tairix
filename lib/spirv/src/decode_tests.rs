use alloc::vec::Vec;

use super::decode;
use crate::enumerant::{
    AddressingModel, BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, MemoryAccess,
    MemoryModel, Scope, StorageClass,
};
use crate::{Arg, DecodeError, EncodeError, Encoder, Fault, Id, Kind, Module, Op, Value, Version};

const V1_0: Version = Version { major: 1, minor: 0 };

const SHADER: u32 = Capability::SHADER.0;
const LOGICAL: u32 = AddressingModel::LOGICAL.0;
const GLSL450: u32 = MemoryModel::GLSL450.0;
const GL_COMPUTE: u32 = ExecutionModel::GL_COMPUTE.0;
const LOCAL_SIZE: u32 = ExecutionMode::LOCAL_SIZE.0;
const SPEC_ID: u32 = Decoration::SPEC_ID.0;
const BUILT_IN: u32 = Decoration::BUILT_IN.0;
const GLOBAL_INVOCATION_ID: u32 = BuiltIn::GLOBAL_INVOCATION_ID.0;
const FUNCTION_STORAGE: u32 = StorageClass::FUNCTION.0;
const MEMORY_ACCESS_ALIGNED: u32 = MemoryAccess::ALIGNED.0;
/// `GLSL.std.450`'s `Round`.
const ROUND: u32 = 1;

fn encode(build: impl FnOnce(&mut Encoder) -> Result<(), EncodeError>) -> Vec<u8> {
    let mut encoder = Encoder::new(V1_0, 0).expect("a header");
    build(&mut encoder).expect("written");
    encoder.finish()
}

fn fault(bytes: &[u8]) -> Fault {
    Module::decode(bytes).expect_err("refused").fault
}

/// The ids of [`shader`], in the order it issues them.
struct Ids {
    glsl: Id,
    void: Id,
    function: Id,
    int: Id,
    long: Id,
    float: Id,
    seven: Id,
    big: Id,
    half: Id,
    spec: Id,
    sum: Id,
    pointer: Id,
    main: Id,
    entry: Id,
    local: Id,
    loaded: Id,
    rounded: Id,
    case: Id,
    wide_case: Id,
    merge: Id,
    wide_merge: Id,
}

/// A compute shader using every shape the decoder reads: strings, enumerants
/// with parameters, a bit set with one, constants of each width, a nested
/// operation, an extended instruction, and switches on 32- and 64-bit
/// selectors.
fn shader() -> (Vec<u8>, Ids) {
    let mut ids = None;
    let bytes = encode(|e| {
        let i = issue(e)?;
        declarations(e, &i)?;
        body(e, &i)?;
        ids = Some(i);
        Ok(())
    });
    (bytes, ids.expect("issued"))
}

fn issue(e: &mut Encoder) -> Result<Ids, EncodeError> {
    let mut id = || e.next_id();
    Ok(Ids {
        glsl: id()?,
        void: id()?,
        function: id()?,
        int: id()?,
        long: id()?,
        float: id()?,
        seven: id()?,
        big: id()?,
        half: id()?,
        spec: id()?,
        sum: id()?,
        pointer: id()?,
        main: id()?,
        entry: id()?,
        local: id()?,
        loaded: id()?,
        rounded: id()?,
        case: id()?,
        wide_case: id()?,
        merge: id()?,
        wide_merge: id()?,
    })
}

fn declarations(e: &mut Encoder, i: &Ids) -> Result<(), EncodeError> {
    e.emit(Op::CAPABILITY, &[Arg::Word(SHADER)])?;
    e.emit(
        Op::EXT_INST_IMPORT,
        &[Arg::Id(i.glsl), Arg::Text("GLSL.std.450")],
    )?;
    e.emit(Op::MEMORY_MODEL, &[Arg::Word(LOGICAL), Arg::Word(GLSL450)])?;
    e.emit(
        Op::ENTRY_POINT,
        &[Arg::Word(GL_COMPUTE), Arg::Id(i.main), Arg::Text("main")],
    )?;
    e.emit(
        Op::EXECUTION_MODE,
        &[
            Arg::Id(i.main),
            Arg::Word(LOCAL_SIZE),
            Arg::Word(8),
            Arg::Word(8),
            Arg::Word(1),
        ],
    )?;
    e.emit(Op::NAME, &[Arg::Id(i.main), Arg::Text("main0")])?;
    e.emit(
        Op::DECORATE,
        &[Arg::Id(i.spec), Arg::Word(SPEC_ID), Arg::Word(7)],
    )?;
    e.emit(
        Op::DECORATE,
        &[
            Arg::Id(i.local),
            Arg::Word(BUILT_IN),
            Arg::Word(GLOBAL_INVOCATION_ID),
        ],
    )?;
    e.emit(Op::TYPE_VOID, &[Arg::Id(i.void)])?;
    e.emit(Op::TYPE_FUNCTION, &[Arg::Id(i.function), Arg::Id(i.void)])?;
    e.emit(Op::TYPE_INT, &[Arg::Id(i.int), Arg::Word(32), Arg::Word(1)])?;
    e.emit(
        Op::TYPE_INT,
        &[Arg::Id(i.long), Arg::Word(64), Arg::Word(0)],
    )?;
    e.emit(Op::TYPE_FLOAT, &[Arg::Id(i.float), Arg::Word(32)])?;
    e.emit(
        Op::CONSTANT,
        &[Arg::Id(i.int), Arg::Id(i.seven), Arg::Word(7)],
    )?;
    e.emit(
        Op::CONSTANT,
        &[Arg::Id(i.long), Arg::Id(i.big), Arg::Wide(0x1_0000_0002)],
    )?;
    e.emit(
        Op::CONSTANT,
        &[Arg::Id(i.float), Arg::Id(i.half), Arg::Word(0x3f00_0000)],
    )?;
    e.emit(
        Op::SPEC_CONSTANT,
        &[Arg::Id(i.int), Arg::Id(i.spec), Arg::Word(3)],
    )?;
    e.emit(
        Op::SPEC_CONSTANT_OP,
        &[
            Arg::Id(i.int),
            Arg::Id(i.sum),
            Arg::Word(u32::from(Op::I_ADD.code())),
            Arg::Id(i.seven),
            Arg::Id(i.spec),
        ],
    )?;
    e.emit(
        Op::TYPE_POINTER,
        &[
            Arg::Id(i.pointer),
            Arg::Word(FUNCTION_STORAGE),
            Arg::Id(i.float),
        ],
    )
}

fn body(e: &mut Encoder, i: &Ids) -> Result<(), EncodeError> {
    e.emit(
        Op::FUNCTION,
        &[
            Arg::Id(i.void),
            Arg::Id(i.main),
            Arg::Word(0),
            Arg::Id(i.function),
        ],
    )?;
    e.emit(Op::LABEL, &[Arg::Id(i.entry)])?;
    e.emit(
        Op::VARIABLE,
        &[
            Arg::Id(i.pointer),
            Arg::Id(i.local),
            Arg::Word(FUNCTION_STORAGE),
        ],
    )?;
    e.emit(
        Op::LOAD,
        &[
            Arg::Id(i.float),
            Arg::Id(i.loaded),
            Arg::Id(i.local),
            Arg::Word(MEMORY_ACCESS_ALIGNED),
            Arg::Word(4),
        ],
    )?;
    e.emit(
        Op::EXT_INST,
        &[
            Arg::Id(i.float),
            Arg::Id(i.rounded),
            Arg::Id(i.glsl),
            Arg::Word(ROUND),
            Arg::Id(i.loaded),
        ],
    )?;
    e.emit(Op::SELECTION_MERGE, &[Arg::Id(i.merge), Arg::Word(0)])?;
    e.emit(
        Op::SWITCH,
        &[
            Arg::Id(i.seven),
            Arg::Id(i.merge),
            Arg::Word(1),
            Arg::Id(i.case),
            Arg::Word(2),
            Arg::Id(i.merge),
        ],
    )?;
    e.emit(Op::LABEL, &[Arg::Id(i.case)])?;
    e.emit(Op::BRANCH, &[Arg::Id(i.merge)])?;
    e.emit(Op::LABEL, &[Arg::Id(i.merge)])?;
    e.emit(Op::SELECTION_MERGE, &[Arg::Id(i.wide_merge), Arg::Word(0)])?;
    e.emit(
        Op::SWITCH,
        &[
            Arg::Id(i.big),
            Arg::Id(i.wide_merge),
            Arg::Wide(0x1_0000_0002),
            Arg::Id(i.wide_case),
        ],
    )?;
    e.emit(Op::LABEL, &[Arg::Id(i.wide_case)])?;
    e.emit(Op::BRANCH, &[Arg::Id(i.wide_merge)])?;
    e.emit(Op::LABEL, &[Arg::Id(i.wide_merge)])?;
    e.emit(Op::RETURN, &[])?;
    e.emit(Op::FUNCTION_END, &[])
}

fn instruction(module: &Module, op: Op, nth: usize) -> crate::Instruction {
    *module
        .instructions()
        .iter()
        .filter(|i| i.op() == op)
        .nth(nth)
        .expect("present")
}

fn values<'m>(module: &'m Module, instruction: &crate::Instruction) -> Vec<Value<'m>> {
    module
        .operands(instruction)
        .iter()
        .map(|operand| module.value(operand))
        .collect()
}

#[test]
fn a_module_decodes_to_its_instructions_operands_and_definitions() {
    let (bytes, i) = shader();
    let module = Module::decode(&bytes).expect("a valid module");
    assert_eq!(module.version(), V1_0);
    assert_eq!(module.bound(), i.wide_merge.get() + 1);
    assert_eq!(module.instructions().len(), 36);
    let entry = instruction(&module, Op::ENTRY_POINT, 0);
    assert_eq!(
        values(&module, &entry),
        [
            Value::Word(GL_COMPUTE),
            Value::Id(i.main),
            Value::Text("main")
        ]
    );
    let mode = instruction(&module, Op::EXECUTION_MODE, 0);
    assert_eq!(
        values(&module, &mode),
        [
            Value::Id(i.main),
            Value::Word(LOCAL_SIZE),
            Value::Word(8),
            Value::Word(8),
            Value::Word(1)
        ]
    );
    let kinds: Vec<Kind> = module.operands(&mode).iter().map(|o| o.kind()).collect();
    assert_eq!(
        kinds,
        [
            Kind::IdRef,
            Kind::ExecutionMode,
            Kind::LiteralInteger,
            Kind::LiteralInteger,
            Kind::LiteralInteger
        ]
    );
    let builtin = instruction(&module, Op::DECORATE, 1);
    assert_eq!(module.operands(&builtin)[2].kind(), Kind::BuiltIn);
    let wide = instruction(&module, Op::CONSTANT, 1);
    assert_eq!(values(&module, &wide)[2], Value::Number(0x1_0000_0002));
    let nested = instruction(&module, Op::SPEC_CONSTANT_OP, 0);
    assert_eq!(
        values(&module, &nested),
        [
            Value::Id(i.int),
            Value::Id(i.sum),
            Value::Word(u32::from(Op::I_ADD.code())),
            Value::Id(i.seven),
            Value::Id(i.spec)
        ]
    );
    let load = instruction(&module, Op::LOAD, 0);
    assert_eq!(
        &values(&module, &load)[3..],
        &[Value::Word(MEMORY_ACCESS_ALIGNED), Value::Word(4)]
    );
    let narrow = instruction(&module, Op::SWITCH, 0);
    assert_eq!(
        values(&module, &narrow),
        [
            Value::Id(i.seven),
            Value::Id(i.merge),
            Value::Number(1),
            Value::Id(i.case),
            Value::Number(2),
            Value::Id(i.merge)
        ]
    );
    let wide_switch = instruction(&module, Op::SWITCH, 1);
    assert_eq!(
        values(&module, &wide_switch)[2],
        Value::Number(0x1_0000_0002)
    );
    let int = module.definition(i.int).expect("defined");
    assert_eq!(int.op(), Op::TYPE_INT);
    assert_eq!(module.result(int), Some(i.int));
    let loaded = module.definition(i.loaded).expect("defined");
    assert_eq!(module.result_type(loaded), Some(i.float));
    assert_eq!(module.result_type(int), None);
    assert!(module
        .definition(Id::new(module.bound()).expect("nonzero"))
        .is_none());
}

#[test]
fn the_operand_table_is_reserved_once_at_the_bound_its_words_give() {
    let (bytes, _) = shader();
    let module = Module::decode(&bytes).expect("a valid module");
    let most = bytes.len() / 4 - super::HEADER_WORDS - module.instructions().len();
    assert_eq!(
        module.operands.capacity(),
        most,
        "no growth past the one reservation"
    );
    assert!(module.operands.len() <= most);
}

#[test]
fn a_module_reencodes_to_exactly_its_bytes() {
    let (bytes, _) = shader();
    let module = Module::decode(&bytes).expect("a valid module");
    let mut encoder = Encoder::rewriting(&module).expect("a header");
    for instruction in module.instructions() {
        encoder.copy(&module, instruction).expect("copied");
    }
    assert_eq!(encoder.finish(), bytes);
    assert_eq!(module.bytes(), bytes);
}

#[test]
fn a_module_of_no_instructions_reencodes_with_its_bound() {
    for bound in [0, 1, 40] {
        let mut header = alloc::vec::Vec::new();
        for word in [super::MAGIC, 0x0001_0000, 9, bound, 0] {
            header.extend_from_slice(&u32::to_le_bytes(word));
        }
        let module = Module::decode(&header).expect("a header alone is a module");
        assert_eq!(module.bound(), bound);
        let mut rewritten = Encoder::rewriting(&module).expect("a header");
        assert_eq!(rewritten.next_id().map(Id::get), Ok(bound.max(1)));
        let fresh = Encoder::rewriting(&module).expect("a header");
        assert_eq!(fresh.finish(), header, "bound {bound}");
    }
}

#[test]
fn both_byte_orders_decode_to_the_same_little_endian_module() {
    let (bytes, _) = shader();
    let swapped: Vec<u8> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, b, c, d]| [d, c, b, a])
        .collect();
    let module = Module::decode(&swapped).expect("a big-endian module");
    assert_eq!(module.bytes(), bytes);
}

#[test]
fn a_header_outside_the_format_is_refused() {
    let (bytes, _) = shader();
    let with = |word: usize, value: u32| {
        let mut changed = bytes.clone();
        changed[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
        fault(&changed)
    };
    assert_eq!(with(0, 0x0723_0204), Fault::Magic);
    assert_eq!(with(1, 0x0002_0000), Fault::Version);
    assert_eq!(with(1, 0x0001_0700), Fault::Version);
    assert_eq!(with(1, 0x0101_0000), Fault::Version);
    assert_eq!(with(4, 1), Fault::Schema);
    assert_eq!(fault(&bytes[..bytes.len() - 1]), Fault::NotWords);
    assert_eq!(fault(&bytes[..16]), Fault::NotWords);
    assert_eq!(fault(&[]), Fault::NotWords);
    assert_eq!(
        fault(&alloc::vec![0; crate::MAX_MODULE_BYTES + 4]),
        Fault::TooLarge
    );
}

/// The shader with the instruction `op` at position `nth` replaced by `words`.
fn replacing(op: Op, nth: usize, words: &[u32]) -> Vec<u8> {
    let (bytes, _) = shader();
    let module = Module::decode(&bytes).expect("a valid module");
    let target = instruction(&module, op, nth);
    let mut out = Vec::new();
    for instruction in module.instructions() {
        if *instruction == target {
            for word in words {
                out.extend_from_slice(&word.to_le_bytes());
            }
        } else {
            out.extend_from_slice(module.instruction_bytes(instruction));
        }
    }
    let mut header = bytes[..20].to_vec();
    header.extend_from_slice(&out);
    header
}

fn first_word(count: usize, op: Op) -> u32 {
    u32::try_from(count).expect("short") << 16 | u32::from(op.code())
}

#[test]
fn an_instruction_running_past_the_end_or_of_no_words_is_refused() {
    let mut bytes = shader().0;
    bytes.extend_from_slice(&first_word(4, Op::NOP).to_le_bytes());
    assert_eq!(fault(&bytes), Fault::WordCount);
    let mut bytes = shader().0;
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(fault(&bytes), Fault::WordCount);
}

#[test]
fn an_opcode_the_grammar_does_not_recognise_is_refused() {
    let mut bytes = shader().0;
    bytes.extend_from_slice(&first_word(1, Op(0xffff)).to_le_bytes());
    assert_eq!(fault(&bytes), Fault::UnknownOpcode);
    let vendor = Op(5571);
    assert!(
        vendor.name().is_none(),
        "a vendor-only instruction is unrecognised"
    );
    let mut bytes = shader().0;
    bytes.extend_from_slice(&first_word(1, vendor).to_le_bytes());
    assert_eq!(fault(&bytes), Fault::UnknownOpcode);
}

#[test]
fn a_core_instruction_a_vendor_extension_first_brought_is_recognised() {
    let bytes = encode(|e| {
        let ty = e.next_id()?;
        let scope = e.next_id()?;
        e.emit(Op::CAPABILITY, &[Arg::Word(SHADER)])?;
        e.emit(Op::MEMORY_MODEL, &[Arg::Word(LOGICAL), Arg::Word(GLSL450)])?;
        e.emit(
            Op::DECORATE_ID,
            &[
                Arg::Id(scope),
                Arg::Word(Decoration::UNIFORM_ID.0),
                Arg::Id(scope),
            ],
        )?;
        e.emit(Op::TYPE_INT, &[Arg::Id(ty), Arg::Word(32), Arg::Word(0)])?;
        e.emit(
            Op::CONSTANT,
            &[Arg::Id(ty), Arg::Id(scope), Arg::Word(Scope::SUBGROUP.0)],
        )
    });
    let module = Module::decode(&bytes).expect("decoded");
    assert_eq!(
        module.instructions().get(2).map(|i| i.op()),
        Some(Op::DECORATE_ID)
    );
}

#[test]
fn operands_missing_or_left_over_are_refused() {
    let int = shader().1.int.get();
    let short = replacing(Op::TYPE_INT, 0, &[first_word(3, Op::TYPE_INT), int, 32]);
    assert_eq!(fault(&short), Fault::MissingOperand);
    let long = replacing(
        Op::TYPE_INT,
        0,
        &[first_word(5, Op::TYPE_INT), int, 32, 1, 9],
    );
    assert_eq!(fault(&long), Fault::TrailingWords);
    let unresulted = replacing(Op::TYPE_VOID, 0, &[first_word(1, Op::TYPE_VOID)]);
    assert_eq!(fault(&unresulted), Fault::MissingOperand);
}

#[test]
fn an_id_of_zero_or_past_the_bound_is_refused() {
    let (bytes, i) = shader();
    let bound = Module::decode(&bytes).expect("valid").bound();
    for bad in [0, bound, u32::MAX] {
        let changed = replacing(
            Op::TYPE_FUNCTION,
            0,
            &[first_word(3, Op::TYPE_FUNCTION), i.function.get(), bad],
        );
        assert_eq!(fault(&changed), Fault::IdOutOfBound, "{bad}");
        let defined = replacing(Op::TYPE_VOID, 0, &[first_word(2, Op::TYPE_VOID), bad]);
        assert_eq!(fault(&defined), Fault::IdOutOfBound, "{bad}");
    }
}

#[test]
fn a_value_or_bit_its_kind_lacks_is_refused() {
    let (_, i) = shader();
    let mode = replacing(
        Op::EXECUTION_MODE,
        0,
        &[first_word(3, Op::EXECUTION_MODE), i.main.get(), 0x7fff_ffff],
    );
    assert_eq!(fault(&mode), Fault::UnknownEnumerant);
    let bits = replacing(
        Op::LOAD,
        0,
        &[
            first_word(5, Op::LOAD),
            i.float.get(),
            i.loaded.get(),
            i.local.get(),
            1 << 30,
        ],
    );
    assert_eq!(fault(&bits), Fault::UnknownEnumerant);
    let parameterless = replacing(
        Op::LOAD,
        0,
        &[
            first_word(5, Op::LOAD),
            i.float.get(),
            i.loaded.get(),
            i.local.get(),
            MEMORY_ACCESS_ALIGNED,
        ],
    );
    assert_eq!(fault(&parameterless), Fault::MissingOperand);
}

fn name(text: &[u8]) -> Vec<u8> {
    let (_, i) = shader();
    let mut words = alloc::vec![0, i.main.get()];
    for chunk in text.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    words[0] = first_word(words.len(), Op::NAME);
    replacing(Op::NAME, 0, &words)
}

#[test]
fn a_string_must_be_terminated_zero_padded_and_utf8() {
    assert!(
        Module::decode(&name(b"main\0\0\0\0")).is_ok(),
        "a whole word of terminator"
    );
    assert!(Module::decode(&name(b"abc\0")).is_ok());
    assert_eq!(fault(&name(b"main")), Fault::BadString);
    assert_eq!(fault(&name(b"ab\0x")), Fault::BadString);
    assert_eq!(fault(&name(b"\xff\xfe\0\0")), Fault::BadString);
}

#[test]
fn a_literal_must_be_as_wide_as_its_type() {
    let (_, i) = shader();
    let narrow_long = replacing(
        Op::CONSTANT,
        1,
        &[first_word(4, Op::CONSTANT), i.long.get(), i.big.get(), 2],
    );
    assert_eq!(fault(&narrow_long), Fault::MissingOperand);
    let wide_int = replacing(
        Op::CONSTANT,
        0,
        &[
            first_word(5, Op::CONSTANT),
            i.int.get(),
            i.seven.get(),
            7,
            0,
        ],
    );
    assert_eq!(fault(&wide_int), Fault::TrailingWords);
    let of_void = replacing(
        Op::CONSTANT,
        0,
        &[first_word(4, Op::CONSTANT), i.void.get(), i.seven.get(), 7],
    );
    assert_eq!(fault(&of_void), Fault::NumericWidth);
    let wide_switch = replacing(
        Op::SWITCH,
        0,
        &[
            first_word(6, Op::SWITCH),
            i.seven.get(),
            i.merge.get(),
            1,
            0,
            i.case.get(),
        ],
    );
    assert_eq!(
        fault(&wide_switch),
        Fault::IdOutOfBound,
        "a 32-bit selector's case is one word"
    );
}

#[test]
fn a_literal_s_type_must_be_defined_before_it() {
    let (_, i) = shader();
    let forward = replacing(
        Op::CONSTANT,
        0,
        &[
            first_word(4, Op::CONSTANT),
            i.pointer.get(),
            i.seven.get(),
            7,
        ],
    );
    assert_eq!(fault(&forward), Fault::Unresolved);
    let selector = replacing(
        Op::SWITCH,
        0,
        &[
            first_word(5, Op::SWITCH),
            i.wide_merge.get(),
            i.merge.get(),
            1,
            i.case.get(),
        ],
    );
    assert_eq!(fault(&selector), Fault::Unresolved);
    let caseless = replacing(
        Op::SWITCH,
        0,
        &[first_word(3, Op::SWITCH), i.wide_merge.get(), i.merge.get()],
    );
    assert!(
        Module::decode(&caseless).is_ok(),
        "a switch with no cases reads no literal, so needs no selector width"
    );
}

#[test]
fn a_nested_operation_must_be_one_of_fixed_shape() {
    let (_, i) = shader();
    let unknown = replacing(
        Op::SPEC_CONSTANT_OP,
        0,
        &[
            first_word(4, Op::SPEC_CONSTANT_OP),
            i.int.get(),
            i.sum.get(),
            0xffff,
        ],
    );
    assert_eq!(fault(&unknown), Fault::UnknownOpcode);
    let recursive = replacing(
        Op::SPEC_CONSTANT_OP,
        0,
        &[
            first_word(6, Op::SPEC_CONSTANT_OP),
            i.int.get(),
            i.sum.get(),
            u32::from(Op::SPEC_CONSTANT_OP.code()),
            u32::from(Op::SPEC_CONSTANT_OP.code()),
            0,
        ],
    );
    assert_eq!(fault(&recursive), Fault::UnknownOpcode);
}

#[test]
fn an_extended_instruction_must_name_a_known_set_instruction_and_arity() {
    let (_, i) = shader();
    let mut set_name = alloc::vec![first_word(6, Op::EXT_INST_IMPORT), i.glsl.get()];
    for chunk in b"GLSL.std.451".chunks(4) {
        set_name.push(u32::from_le_bytes(chunk.try_into().expect("whole")));
    }
    set_name.push(0);
    assert_eq!(
        fault(&replacing(Op::EXT_INST_IMPORT, 0, &set_name)),
        Fault::UnknownExtInstSet
    );
    let ext = |number: u32, operands: &[u32], set: u32| {
        let mut words = alloc::vec![0, i.float.get(), i.rounded.get(), set, number];
        words.extend_from_slice(operands);
        words[0] = first_word(words.len(), Op::EXT_INST);
        replacing(Op::EXT_INST, 0, &words)
    };
    assert_eq!(
        fault(&ext(999, &[i.loaded.get()], i.glsl.get())),
        Fault::UnknownExtInst
    );
    assert_eq!(fault(&ext(ROUND, &[], i.glsl.get())), Fault::ExtInstArity);
    assert_eq!(
        fault(&ext(ROUND, &[i.loaded.get(), i.loaded.get()], i.glsl.get())),
        Fault::ExtInstArity
    );
    assert_eq!(
        fault(&ext(ROUND, &[i.loaded.get()], i.int.get())),
        Fault::UnknownExtInstSet
    );
}

#[test]
fn an_id_defined_twice_is_refused() {
    let (_, i) = shader();
    let twice = replacing(
        Op::TYPE_FLOAT,
        0,
        &[first_word(3, Op::TYPE_FLOAT), i.int.get(), 32],
    );
    let refused = Module::decode(&twice).expect_err("refused");
    assert_eq!(refused.fault, Fault::DuplicateId);
}

#[test]
fn more_instructions_than_the_bound_allows_are_refused() {
    let (bytes, _) = shader();
    let count = Module::decode(&bytes).expect("valid").instructions().len();
    assert!(decode(&bytes, count).is_ok());
    let refused = decode(&bytes, count - 1).expect_err("one too many");
    assert_eq!(refused.fault, Fault::TooManyInstructions);
}

#[test]
fn a_refusal_names_the_word_at_fault() {
    let (bytes, _) = shader();
    let mut changed = bytes.clone();
    let at = changed.len() / 4;
    changed.extend_from_slice(&first_word(1, Op(0xffff)).to_le_bytes());
    assert_eq!(
        Module::decode(&changed).err(),
        Some(DecodeError {
            word: u32::try_from(at).expect("small"),
            fault: Fault::UnknownOpcode
        })
    );
}

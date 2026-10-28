//! What the validator admits: the capability and extension allow-lists, what
//! an instruction or enumerant needs to be enabled, and where each admitted
//! instruction may stand (`plans/SHADER.md` §2.1).

use alloc::vec::Vec;

use super::body::{Reduce, SampleForm, Typing};
use super::{push, reserve, Refusal, Rule};
use crate::enumerant::{BuiltIn, Capability};
use crate::grammar::{self, Gate, Shape};
use crate::{Instruction, Kind, Module, Op, Value, Version};

/// The admitted capabilities, by ascending value; closed under what each
/// implicitly declares.
pub(crate) const CAPABILITIES: [Capability; 25] = [
    Capability::MATRIX,
    Capability::SHADER,
    Capability::FLOAT16,
    Capability::FLOAT64,
    Capability::INT64,
    Capability::INT16,
    Capability::IMAGE_GATHER_EXTENDED,
    Capability::IMAGE_CUBE_ARRAY,
    Capability::SAMPLED1D,
    Capability::IMAGE1D,
    Capability::SAMPLED_CUBE_ARRAY,
    Capability::SAMPLED_BUFFER,
    Capability::IMAGE_BUFFER,
    Capability::STORAGE_IMAGE_EXTENDED_FORMATS,
    Capability::IMAGE_QUERY,
    Capability::STORAGE_IMAGE_READ_WITHOUT_FORMAT,
    Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT,
    Capability::GROUP_NON_UNIFORM,
    Capability::GROUP_NON_UNIFORM_VOTE,
    Capability::GROUP_NON_UNIFORM_ARITHMETIC,
    Capability::GROUP_NON_UNIFORM_BALLOT,
    Capability::STORAGE_BUFFER16_BIT_ACCESS,
    Capability::UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS,
    Capability::STORAGE_PUSH_CONSTANT16,
    Capability::STORAGE_INPUT_OUTPUT16,
];

/// The admitted extensions: the two that restate the 1.3 core, and the one
/// that brings `OpTerminateInvocation` before 1.6.
pub(crate) const EXTENSIONS: [&str; 3] = [
    "SPV_KHR_16bit_storage",
    "SPV_KHR_storage_buffer_storage_class",
    "SPV_KHR_terminate_invocation",
];

/// The oldest version admitted: the Vulkan 1.1 baseline.
pub(crate) const OLDEST: Version = Version { major: 1, minor: 3 };

/// The built-ins a block may declare without their capabilities, as latent
/// members nothing reads or writes: glslang declares both in every
/// `gl_PerVertex`.
const LATENT: [BuiltIn; 2] = [BuiltIn::CLIP_DISTANCE, BuiltIn::CULL_DISTANCE];

/// What a module enables: its version, its declared extensions, and its
/// capabilities with all they implicitly declare.
pub(crate) struct Enabled {
    pub(crate) version: Version,
    capabilities: Vec<Capability>,
    /// Which of [`EXTENSIONS`] are declared, by position.
    extensions: [bool; EXTENSIONS.len()],
}

impl Enabled {
    /// Collect the leading `OpCapability` and `OpExtension` instructions,
    /// refusing any outside the allow-lists.
    pub(crate) fn collect(module: &Module) -> Result<Self, Refusal> {
        let mut enabled = Self {
            version: module.version(),
            capabilities: Vec::new(),
            extensions: [false; EXTENSIONS.len()],
        };
        for instruction in module.instructions() {
            match instruction.op() {
                Op::CAPABILITY => {
                    let capability = Capability(word(module, instruction, 0));
                    if CAPABILITIES.binary_search(&capability).is_err() {
                        return Err(Rule::Capability(capability).at(instruction));
                    }
                    enabled.insert(capability)?;
                }
                Op::EXTENSION => {
                    let name = text(module, instruction, 0);
                    let declared = EXTENSIONS
                        .iter()
                        .zip(&mut enabled.extensions)
                        .find(|(&admitted, _)| admitted == name)
                        .ok_or(Rule::Extension.at(instruction))?;
                    *declared.1 = true;
                }
                _ => break,
            }
        }
        Ok(enabled)
    }

    /// Add `capability` and, transitively, what it implicitly declares.
    fn insert(&mut self, capability: Capability) -> Result<(), Refusal> {
        if self.has(capability) {
            return Ok(());
        }
        let mut pending = Vec::new();
        push(&mut pending, capability)?;
        while let Some(next) = pending.pop() {
            let Err(at) = self.capabilities.binary_search(&next) else {
                continue;
            };
            reserve(&mut self.capabilities, 1)?;
            self.capabilities.insert(at, next);
            let implied = grammar::enumerant(Kind::Capability, next.0)
                .map_or(&[][..], |e| e.gate.capabilities);
            for &also in implied {
                push(&mut pending, also)?;
            }
        }
        Ok(())
    }

    /// Whether `built_in` is a latent block member: one whose capability is
    /// not declared.
    pub(crate) fn latent(&self, built_in: BuiltIn) -> bool {
        LATENT.contains(&built_in)
            && !grammar::enumerant(Kind::BuiltIn, built_in.0).is_some_and(|e| self.enables(&e.gate))
    }

    /// Whether `capability` is declared or implicitly declared.
    pub(crate) fn has(&self, capability: Capability) -> bool {
        self.capabilities.binary_search(&capability).is_ok()
    }

    /// The capabilities, declared and implied, by ascending value.
    pub(crate) fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Whether the module's version and extensions provide what `gate`
    /// names: present by `last`, and core by now or brought by an extension.
    fn in_version(&self, gate: &Gate) -> bool {
        gate.last.is_none_or(|last| self.version <= last)
            && (gate.version.is_some_and(|core| self.version >= core)
                || gate.extensions.iter().any(|&e| self.declared(e)))
    }

    /// Whether the module declares `extension`.
    fn declared(&self, extension: &str) -> bool {
        EXTENSIONS
            .iter()
            .zip(self.extensions)
            .any(|(&admitted, on)| on && admitted == extension)
    }

    /// Whether `gate` is satisfied.
    pub(crate) fn enables(&self, gate: &Gate) -> bool {
        self.in_version(gate)
            && (gate.capabilities.is_empty() || gate.capabilities.iter().any(|&c| self.has(c)))
    }

    /// Refuse `instruction` unless it, and every enumerant among its
    /// operands, is enabled.
    pub(crate) fn check(&self, module: &Module, instruction: &Instruction) -> Result<(), Refusal> {
        let op = instruction.op();
        if !grammar::instruction(op).is_some_and(|spec| self.enables(&spec.gate)) {
            return Err(Rule::InstructionNotEnabled(op).at(instruction));
        }
        for operand in module.operands(instruction) {
            let kind = operand.kind();
            let shape = kind.shape();
            if !matches!(shape, Shape::Enum | Shape::Bits | Shape::SpecOp) {
                continue;
            }
            let Value::Word(value) = module.value(operand) else {
                continue;
            };
            let enabled = match shape {
                Shape::Enum => grammar::enumerant(kind, value).is_some_and(|e| {
                    if kind == Kind::Capability {
                        self.in_version(&e.gate)
                    } else {
                        self.enables(&e.gate)
                            || op == Op::MEMBER_DECORATE
                                && kind == Kind::BuiltIn
                                && LATENT.contains(&BuiltIn(value))
                    }
                }),
                Shape::Bits => self.bits_enabled(kind, value),
                Shape::SpecOp => u16::try_from(value)
                    .ok()
                    .and_then(|code| grammar::instruction(Op(code)))
                    .is_some_and(|spec| self.enables(&spec.gate)),
                _ => true,
            };
            if !enabled {
                return Err(Rule::EnumerantNotEnabled(kind, value).at(instruction));
            }
        }
        Ok(())
    }

    /// Whether every bit of `mask`, a set of `kind`, is enabled; the empty
    /// set stands for the kind's zero enumerant.
    fn bits_enabled(&self, kind: Kind, mask: u32) -> bool {
        let enabled =
            |value| grammar::enumerant(kind, value).is_some_and(|e| self.enables(&e.gate));
        if mask == 0 {
            return enabled(0);
        }
        (0..32)
            .map(|bit| 1u32 << bit)
            .filter(|bit| mask & bit != 0)
            .all(enabled)
    }
}

/// The word of `instruction`'s operand `index`; zero if it has none there.
pub(crate) fn word(module: &Module, instruction: &Instruction, index: usize) -> u32 {
    match module
        .operands(instruction)
        .get(index)
        .map(|o| module.value(o))
    {
        Some(Value::Word(word)) => word,
        _ => 0,
    }
}

/// The string `instruction` holds at operand `index`; empty if none.
pub(crate) fn text<'m>(module: &'m Module, instruction: &Instruction, index: usize) -> &'m str {
    match module
        .operands(instruction)
        .get(index)
        .map(|o| module.value(o))
    {
        Some(Value::Text(text)) => text,
        _ => "",
    }
}

/// The section of a module an instruction stands in; the order is the
/// module's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Section {
    Capability,
    Extension,
    Import,
    MemoryModel,
    EntryPoint,
    ExecutionMode,
    Source,
    Name,
    Processed,
    Annotation,
    Declaration,
    Function,
}

/// Where an admitted instruction may stand, and how it is checked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Class {
    /// Only in the section named.
    Section(Section),
    /// A debug location: anywhere from the declarations on.
    Line,
    /// A type declaration.
    Type,
    /// A constant or specialization constant.
    Constant,
    /// A variable: global among the declarations, `Function` at the head of
    /// a function's first block.
    Variable,
    /// An undefined value, among the declarations or in a block.
    Undef,
    /// `OpFunction`.
    Function,
    /// `OpFunctionParameter`.
    Parameter,
    /// `OpFunctionEnd`.
    FunctionEnd,
    /// `OpLabel`.
    Label,
    /// An instruction of a block, checked as `Typing` says.
    Body(Typing),
}

/// Where `op` may stand, or `None` if it is refused wherever it appears:
/// decoration groups (deprecated), string decorations (no admitted decoration
/// takes one), pointer comparison (logical pointers have no identity to
/// compare), and the extended instruction taking forward references (it
/// exists for non-semantic sets, none of which is recognised).
#[expect(
    clippy::too_many_lines,
    reason = "one arm per admitted instruction keeps the admitted set a single table"
)]
pub(crate) fn class(op: Op) -> Option<Class> {
    use Section as S;
    use Typing as T;
    let class = match op {
        Op::CAPABILITY => Class::Section(S::Capability),
        Op::EXTENSION => Class::Section(S::Extension),
        Op::EXT_INST_IMPORT => Class::Section(S::Import),
        Op::MEMORY_MODEL => Class::Section(S::MemoryModel),
        Op::ENTRY_POINT => Class::Section(S::EntryPoint),
        Op::EXECUTION_MODE | Op::EXECUTION_MODE_ID => Class::Section(S::ExecutionMode),
        Op::STRING | Op::SOURCE_EXTENSION | Op::SOURCE | Op::SOURCE_CONTINUED => {
            Class::Section(S::Source)
        }
        Op::NAME | Op::MEMBER_NAME => Class::Section(S::Name),
        Op::MODULE_PROCESSED => Class::Section(S::Processed),
        Op::DECORATE | Op::MEMBER_DECORATE | Op::DECORATE_ID => Class::Section(S::Annotation),
        Op::LINE | Op::NO_LINE => Class::Line,
        Op::TYPE_VOID
        | Op::TYPE_BOOL
        | Op::TYPE_INT
        | Op::TYPE_FLOAT
        | Op::TYPE_VECTOR
        | Op::TYPE_MATRIX
        | Op::TYPE_IMAGE
        | Op::TYPE_SAMPLER
        | Op::TYPE_SAMPLED_IMAGE
        | Op::TYPE_ARRAY
        | Op::TYPE_RUNTIME_ARRAY
        | Op::TYPE_STRUCT
        | Op::TYPE_POINTER
        | Op::TYPE_FUNCTION => Class::Type,
        Op::CONSTANT_TRUE
        | Op::CONSTANT_FALSE
        | Op::CONSTANT
        | Op::CONSTANT_COMPOSITE
        | Op::CONSTANT_NULL
        | Op::SPEC_CONSTANT_TRUE
        | Op::SPEC_CONSTANT_FALSE
        | Op::SPEC_CONSTANT
        | Op::SPEC_CONSTANT_COMPOSITE
        | Op::SPEC_CONSTANT_OP => Class::Constant,
        Op::VARIABLE => Class::Variable,
        Op::UNDEF => Class::Undef,
        Op::FUNCTION => Class::Function,
        Op::FUNCTION_PARAMETER => Class::Parameter,
        Op::FUNCTION_END => Class::FunctionEnd,
        Op::LABEL => Class::Label,
        Op::NOP => Class::Body(T::Nop),
        Op::I_ADD
        | Op::I_SUB
        | Op::I_MUL
        | Op::S_DIV
        | Op::S_REM
        | Op::S_MOD
        | Op::BITWISE_OR
        | Op::BITWISE_XOR
        | Op::BITWISE_AND => Class::Body(T::IntArith),
        Op::U_DIV | Op::U_MOD => Class::Body(T::UnsignedArith),
        Op::F_ADD | Op::F_SUB | Op::F_MUL | Op::F_DIV | Op::F_REM | Op::F_MOD => {
            Class::Body(T::FloatArith)
        }
        Op::S_NEGATE | Op::NOT => Class::Body(T::IntUnary),
        Op::F_NEGATE => Class::Body(T::FloatUnary),
        Op::SHIFT_RIGHT_LOGICAL | Op::SHIFT_RIGHT_ARITHMETIC | Op::SHIFT_LEFT_LOGICAL => {
            Class::Body(T::Shift)
        }
        Op::I_EQUAL
        | Op::I_NOT_EQUAL
        | Op::U_GREATER_THAN
        | Op::S_GREATER_THAN
        | Op::U_GREATER_THAN_EQUAL
        | Op::S_GREATER_THAN_EQUAL
        | Op::U_LESS_THAN
        | Op::S_LESS_THAN
        | Op::U_LESS_THAN_EQUAL
        | Op::S_LESS_THAN_EQUAL => Class::Body(T::IntCompare),
        Op::F_ORD_EQUAL
        | Op::F_UNORD_EQUAL
        | Op::F_ORD_NOT_EQUAL
        | Op::F_UNORD_NOT_EQUAL
        | Op::F_ORD_LESS_THAN
        | Op::F_UNORD_LESS_THAN
        | Op::F_ORD_GREATER_THAN
        | Op::F_UNORD_GREATER_THAN
        | Op::F_ORD_LESS_THAN_EQUAL
        | Op::F_UNORD_LESS_THAN_EQUAL
        | Op::F_ORD_GREATER_THAN_EQUAL
        | Op::F_UNORD_GREATER_THAN_EQUAL => Class::Body(T::FloatCompare),
        Op::LOGICAL_EQUAL | Op::LOGICAL_NOT_EQUAL | Op::LOGICAL_OR | Op::LOGICAL_AND => {
            Class::Body(T::Logical)
        }
        Op::LOGICAL_NOT => Class::Body(T::LogicalNot),
        Op::IS_NAN | Op::IS_INF => Class::Body(T::FloatClass),
        Op::ANY | Op::ALL => Class::Body(T::AnyAll),
        Op::SELECT => Class::Body(T::Select),
        Op::CONVERT_F_TO_U => Class::Body(T::FloatToInt { unsigned: true }),
        Op::CONVERT_F_TO_S => Class::Body(T::FloatToInt { unsigned: false }),
        Op::CONVERT_S_TO_F | Op::CONVERT_U_TO_F => Class::Body(T::IntToFloat),
        Op::U_CONVERT => Class::Body(T::IntWidth { unsigned: true }),
        Op::S_CONVERT => Class::Body(T::IntWidth { unsigned: false }),
        Op::F_CONVERT => Class::Body(T::FloatWidth),
        Op::QUANTIZE_TO_F16 => Class::Body(T::QuantizeToF16),
        Op::BITCAST => Class::Body(T::Bitcast),
        Op::VECTOR_EXTRACT_DYNAMIC => Class::Body(T::VectorExtractDynamic),
        Op::VECTOR_INSERT_DYNAMIC => Class::Body(T::VectorInsertDynamic),
        Op::VECTOR_SHUFFLE => Class::Body(T::VectorShuffle),
        Op::COMPOSITE_CONSTRUCT => Class::Body(T::CompositeConstruct),
        Op::COMPOSITE_EXTRACT => Class::Body(T::CompositeExtract),
        Op::COMPOSITE_INSERT => Class::Body(T::CompositeInsert),
        Op::COPY_OBJECT => Class::Body(T::CopyObject),
        Op::TRANSPOSE => Class::Body(T::Transpose),
        Op::COPY_LOGICAL => Class::Body(T::CopyLogical),
        Op::VECTOR_TIMES_SCALAR => Class::Body(T::VectorTimesScalar),
        Op::MATRIX_TIMES_SCALAR => Class::Body(T::MatrixTimesScalar),
        Op::VECTOR_TIMES_MATRIX => Class::Body(T::VectorTimesMatrix),
        Op::MATRIX_TIMES_VECTOR => Class::Body(T::MatrixTimesVector),
        Op::MATRIX_TIMES_MATRIX => Class::Body(T::MatrixTimesMatrix),
        Op::OUTER_PRODUCT => Class::Body(T::OuterProduct),
        Op::DOT => Class::Body(T::Dot),
        Op::I_ADD_CARRY | Op::I_SUB_BORROW | Op::U_MUL_EXTENDED => {
            Class::Body(T::Extended { unsigned: true })
        }
        Op::S_MUL_EXTENDED => Class::Body(T::Extended { unsigned: false }),
        Op::BIT_FIELD_INSERT => Class::Body(T::BitFieldInsert),
        Op::BIT_FIELD_S_EXTRACT | Op::BIT_FIELD_U_EXTRACT => Class::Body(T::BitFieldExtract),
        Op::BIT_REVERSE => Class::Body(T::BitReverse),
        Op::BIT_COUNT => Class::Body(T::BitCount),
        Op::D_PDX | Op::D_PDY | Op::FWIDTH => Class::Body(T::Derivative),
        Op::PHI => Class::Body(T::Phi),
        Op::LOOP_MERGE => Class::Body(T::LoopMerge),
        Op::SELECTION_MERGE => Class::Body(T::SelectionMerge),
        Op::BRANCH => Class::Body(T::Branch),
        Op::BRANCH_CONDITIONAL => Class::Body(T::BranchConditional),
        Op::SWITCH => Class::Body(T::Switch),
        Op::KILL => Class::Body(T::Kill),
        Op::RETURN => Class::Body(T::Return),
        Op::RETURN_VALUE => Class::Body(T::ReturnValue),
        Op::UNREACHABLE => Class::Body(T::Unreachable),
        Op::TERMINATE_INVOCATION => Class::Body(T::TerminateInvocation),
        Op::FUNCTION_CALL => Class::Body(T::FunctionCall),
        Op::LOAD => Class::Body(T::Load),
        Op::STORE => Class::Body(T::Store),
        Op::COPY_MEMORY => Class::Body(T::CopyMemory),
        Op::ACCESS_CHAIN | Op::IN_BOUNDS_ACCESS_CHAIN => Class::Body(T::AccessChain),
        Op::ARRAY_LENGTH => Class::Body(T::ArrayLength),
        Op::IMAGE_TEXEL_POINTER => Class::Body(T::ImageTexelPointer),
        Op::SAMPLED_IMAGE => Class::Body(T::SampledImage),
        Op::IMAGE_SAMPLE_IMPLICIT_LOD => Class::Body(T::Sample(SampleForm::PLAIN)),
        Op::IMAGE_SAMPLE_EXPLICIT_LOD => Class::Body(T::Sample(SampleForm::EXPLICIT)),
        Op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD => Class::Body(T::Sample(SampleForm::DREF)),
        Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD => Class::Body(T::Sample(SampleForm::DREF_EXPLICIT)),
        Op::IMAGE_SAMPLE_PROJ_IMPLICIT_LOD => Class::Body(T::Sample(SampleForm::PROJ)),
        Op::IMAGE_SAMPLE_PROJ_EXPLICIT_LOD => Class::Body(T::Sample(SampleForm::PROJ_EXPLICIT)),
        Op::IMAGE_SAMPLE_PROJ_DREF_IMPLICIT_LOD => Class::Body(T::Sample(SampleForm::PROJ_DREF)),
        Op::IMAGE_SAMPLE_PROJ_DREF_EXPLICIT_LOD => {
            Class::Body(T::Sample(SampleForm::PROJ_DREF_EXPLICIT))
        }
        Op::IMAGE_FETCH => Class::Body(T::Fetch),
        Op::IMAGE_GATHER => Class::Body(T::Gather { dref: false }),
        Op::IMAGE_DREF_GATHER => Class::Body(T::Gather { dref: true }),
        Op::IMAGE_READ => Class::Body(T::ImageRead),
        Op::IMAGE_WRITE => Class::Body(T::ImageWrite),
        Op::IMAGE => Class::Body(T::Image),
        Op::IMAGE_QUERY_SIZE_LOD => Class::Body(T::QuerySizeLod),
        Op::IMAGE_QUERY_SIZE => Class::Body(T::QuerySize),
        Op::IMAGE_QUERY_LOD => Class::Body(T::QueryLod),
        Op::IMAGE_QUERY_LEVELS => Class::Body(T::QueryLevels),
        Op::IMAGE_QUERY_SAMPLES => Class::Body(T::QuerySamples),
        Op::ATOMIC_LOAD => Class::Body(T::AtomicLoad),
        Op::ATOMIC_STORE => Class::Body(T::AtomicStore),
        Op::ATOMIC_EXCHANGE => Class::Body(T::AtomicExchange),
        Op::ATOMIC_COMPARE_EXCHANGE => Class::Body(T::AtomicCompareExchange),
        Op::ATOMIC_I_INCREMENT | Op::ATOMIC_I_DECREMENT => Class::Body(T::AtomicStep),
        Op::ATOMIC_I_ADD
        | Op::ATOMIC_I_SUB
        | Op::ATOMIC_S_MIN
        | Op::ATOMIC_U_MIN
        | Op::ATOMIC_S_MAX
        | Op::ATOMIC_U_MAX
        | Op::ATOMIC_AND
        | Op::ATOMIC_OR
        | Op::ATOMIC_XOR => Class::Body(T::AtomicArith),
        Op::CONTROL_BARRIER => Class::Body(T::ControlBarrier),
        Op::MEMORY_BARRIER => Class::Body(T::MemoryBarrier),
        Op::GROUP_NON_UNIFORM_ELECT => Class::Body(T::Elect),
        Op::GROUP_NON_UNIFORM_ALL | Op::GROUP_NON_UNIFORM_ANY => Class::Body(T::Vote),
        Op::GROUP_NON_UNIFORM_ALL_EQUAL => Class::Body(T::AllEqual),
        Op::GROUP_NON_UNIFORM_BROADCAST => Class::Body(T::Broadcast),
        Op::GROUP_NON_UNIFORM_BROADCAST_FIRST => Class::Body(T::BroadcastFirst),
        Op::GROUP_NON_UNIFORM_BALLOT => Class::Body(T::Ballot),
        Op::GROUP_NON_UNIFORM_INVERSE_BALLOT => Class::Body(T::InverseBallot),
        Op::GROUP_NON_UNIFORM_BALLOT_BIT_EXTRACT => Class::Body(T::BallotBitExtract),
        Op::GROUP_NON_UNIFORM_BALLOT_BIT_COUNT => Class::Body(T::BallotBitCount),
        Op::GROUP_NON_UNIFORM_BALLOT_FIND_LSB | Op::GROUP_NON_UNIFORM_BALLOT_FIND_MSB => {
            Class::Body(T::BallotFind)
        }
        Op::GROUP_NON_UNIFORM_I_ADD
        | Op::GROUP_NON_UNIFORM_I_MUL
        | Op::GROUP_NON_UNIFORM_S_MIN
        | Op::GROUP_NON_UNIFORM_S_MAX
        | Op::GROUP_NON_UNIFORM_BITWISE_AND
        | Op::GROUP_NON_UNIFORM_BITWISE_OR
        | Op::GROUP_NON_UNIFORM_BITWISE_XOR => Class::Body(T::GroupReduce(Reduce::Int)),
        Op::GROUP_NON_UNIFORM_U_MIN | Op::GROUP_NON_UNIFORM_U_MAX => {
            Class::Body(T::GroupReduce(Reduce::Unsigned))
        }
        Op::GROUP_NON_UNIFORM_F_ADD
        | Op::GROUP_NON_UNIFORM_F_MUL
        | Op::GROUP_NON_UNIFORM_F_MIN
        | Op::GROUP_NON_UNIFORM_F_MAX => Class::Body(T::GroupReduce(Reduce::Float)),
        Op::GROUP_NON_UNIFORM_LOGICAL_AND
        | Op::GROUP_NON_UNIFORM_LOGICAL_OR
        | Op::GROUP_NON_UNIFORM_LOGICAL_XOR => Class::Body(T::GroupReduce(Reduce::Logical)),
        Op::EXT_INST => Class::Body(T::ExtInst),
        _ => return None,
    };
    Some(class)
}

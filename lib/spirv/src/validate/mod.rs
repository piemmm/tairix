//! The validator (`plans/SHADER.md` SH2); what an admitted module guarantees
//! is the crate documentation's.

pub(crate) mod allow;
mod binding;
mod body;
mod cfg;
mod declare;
mod decorate;
mod dominance;
mod entry;
mod fold;
pub(crate) mod glsl;
mod group;
mod guard;
mod image;
mod interface;
mod layout;
mod memory;
mod ops;
mod scan;
mod specialize;
mod types;
mod view;

#[cfg(test)]
mod admit_tests;
#[cfg(test)]
mod assemble;
#[cfg(test)]
mod cfg_tests;
#[cfg(test)]
mod refusal_tests;
#[cfg(test)]
mod sampling_tests;
#[cfg(test)]
mod scale_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod view_tests;

use alloc::vec::Vec;
use core::fmt;

pub use allow::CAPABILITIES;
pub use binding::{
    Access, Binding, BindingKind, Component, Disagreement, ImageShape, Layout, SampleForms,
    Sampling, Stages,
};
pub use entry::{EntryPoint, Interpolation, InterpolationSampling, Stage, Varying, WorkgroupSize};
pub use fold::FoldError;
pub use guard::{GuardError, Guarded};
pub use specialize::{Constant, ConstantId, SpecError, SpecValue, Specialized, MAX_SPECIALIZED};
pub use types::{MAX_MATRIX_COLUMNS, MAX_VECTOR_COMPONENTS};
pub use view::{ImageType, Member, Scalar, Type, TypeId};

use crate::enumerant::{Capability, Decoration, ExecutionMode, StorageClass};
use tairix_util::fallible::held;

use crate::{Id, Instruction, Kind, Module, Op};

/// The most entry points a module may declare: each one's interface is
/// analysed over the whole module.
pub const MAX_ENTRY_POINTS: usize = 64;

/// The deepest chain of calls from an entry point.
pub const MAX_CALL_DEPTH: usize = 255;

/// The most resource bindings one entry point may use.
pub const MAX_BINDINGS: usize = 1024;

/// The most pairs of an image and a sampler it is sampled through that one
/// function makes, its callees' included: one for each binding an entry point
/// may use.
pub const MAX_SAMPLINGS: usize = MAX_BINDINGS;

/// The most samplings the validator looks at in one module following them
/// through calls and into entry points: what bounds that work, which grows
/// with calls and entry points rather than with the module's size.
pub const MAX_SAMPLING_WORK: usize = 1 << 20;

/// The most interface locations a stage's inputs or outputs may span.
pub const MAX_LOCATIONS: u32 = 64;

/// The deepest nesting of composite types.
pub const MAX_TYPE_DEPTH: u16 = 255;

/// The deepest nesting of structured control-flow constructs.
pub const MAX_NESTING: usize = 1023;

/// The most members a structure may have, and pairs a switch.
pub const MAX_MEMBERS: usize = 16_383;

/// The most indexes an access chain, extraction or insertion may take, and
/// parameters a function or arguments a call.
pub const MAX_INDEXES: usize = 255;

/// The most characters in a string.
pub const MAX_STRING: usize = 65_535;

/// The most variables of storage classes other than `Function` a module may
/// declare.
pub const MAX_GLOBALS: usize = 65_535;

/// The most variables of the `Function` storage class a module may declare.
pub const MAX_LOCALS: usize = 524_287;

/// The most execution modes an entry point may set.
pub const MAX_MODES: usize = 255;

/// A module the validator admitted, with what each of its entry points needs
/// and the tables its meaning was resolved into: what every backend lowers
/// from, so no backend reads it a second way.
#[derive(Debug)]
pub struct Admitted {
    module: Module,
    capabilities: Vec<Capability>,
    entry_points: Vec<EntryPoint>,
    kept: view::Kept,
}

impl Admitted {
    /// Validate `module`.
    ///
    /// # Errors
    ///
    /// The instruction the module was refused at and the rule it broke.
    pub fn new(module: Module) -> Result<Self, Refusal> {
        let (capabilities, entry_points, kept) = Validator::run(&module)?;
        Ok(Self {
            module,
            capabilities,
            entry_points,
            kept,
        })
    }

    /// The module.
    #[must_use]
    pub const fn module(&self) -> &Module {
        &self.module
    }

    /// The module's bytes, little-endian, given up with everything else it
    /// holds: what a compiler hands on without copying what it admitted.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.module.bytes
    }

    /// The capabilities it declares, and those they implicitly declare, by
    /// ascending value: what a device must support to run it.
    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Its entry points, in declaration order.
    #[must_use]
    pub fn entry_points(&self) -> &[EntryPoint] {
        &self.entry_points
    }

    /// The bytes of memory it holds past its own size: the module's words
    /// and tables, and every table the validator kept for it.
    #[must_use]
    pub fn held_bytes(&self) -> u64 {
        let entries = self
            .entry_points
            .iter()
            .fold(held(&self.entry_points), |total, e| {
                total.saturating_add(e.held_bytes())
            });
        [
            self.module.held_bytes(),
            held(&self.capabilities),
            entries,
            self.kept.held_bytes(),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }

    /// The entry point named `name` for `stage`.
    #[must_use]
    pub fn entry_point(&self, name: &str, stage: Stage) -> Option<&EntryPoint> {
        self.entry_points
            .iter()
            .find(|e| e.stage() == stage && e.name() == name)
    }
}

/// Why a module was refused, and the first word of the instruction it was
/// refused at; zero where the module as a whole is at fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    /// The word at fault, counted from the module's first.
    pub word: u32,
    /// What was wrong.
    pub rule: Rule,
}

/// A fixed bound a module exceeded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Limit {
    /// [`MAX_ENTRY_POINTS`].
    EntryPoints,
    /// [`MAX_CALL_DEPTH`].
    CallDepth,
    /// [`MAX_BINDINGS`].
    Bindings,
    /// [`MAX_SAMPLINGS`].
    Samplings,
    /// [`MAX_SAMPLING_WORK`].
    SamplingWork,
    /// [`MAX_LOCATIONS`].
    Locations,
    /// [`MAX_TYPE_DEPTH`].
    TypeDepth,
    /// [`MAX_NESTING`].
    Nesting,
    /// [`MAX_MEMBERS`].
    Members,
    /// [`MAX_INDEXES`].
    Indexes,
    /// [`MAX_STRING`].
    String,
    /// [`MAX_GLOBALS`].
    Globals,
    /// [`MAX_LOCALS`].
    Locals,
    /// [`MAX_MODES`].
    Modes,
}

/// The rule a module broke.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Rule {
    /// A version older than the oldest admitted.
    Version,
    /// A declared capability outside the allow-list.
    Capability(Capability),
    /// A declared extension outside the allow-list.
    Extension,
    /// An instruction no declared capability, the version or a declared
    /// extension enables.
    InstructionNotEnabled(Op),
    /// An enumerant no declared capability, the version or a declared
    /// extension enables.
    EnumerantNotEnabled(Kind, u32),
    /// An instruction the validator refuses wherever it appears.
    NotAdmitted(Op),
    /// An addressing model other than `Logical`.
    AddressingModel,
    /// A memory model other than `GLSL450`.
    MemoryModel,
    /// An instruction out of the module's section order, a missing or second
    /// memory model, or an instruction outside the place it belongs.
    Layout,
    /// No entry point.
    NoEntryPoint,
    /// A fixed bound exceeded.
    Limit(Limit),
    /// The memory the validation needs could not be had.
    OutOfMemory,

    /// An id used where nothing defines it.
    Undefined(Id),
    /// An id used before its definition where that is not allowed.
    ForwardReference(Id),
    /// An id local to another function.
    OtherFunction(Id),
    /// An id that names the wrong sort of thing for its operand: a type for a
    /// value, a value for a label, and so on.
    WrongKind(Id),
    /// A use its definition does not dominate.
    NotDominated(Id),

    /// A type that is not well formed.
    Type,
    /// A second declaration of a non-aggregate, non-pointer type.
    DuplicateType,
    /// An array length that is not a 32-bit integer constant of at least one.
    ArrayLength,
    /// A run-time array anywhere but the last member of a storage buffer.
    RuntimeArray,
    /// A type holding something its storage class or use may not: a pointer
    /// in memory, a Boolean where it would be visible, an opaque type in a
    /// structure, a width without its capability.
    Contents,

    /// A constant whose type or constituents are wrong.
    Constant,
    /// A specialization constant's operation not admitted, or its operands
    /// wrongly typed.
    SpecConstantOp,

    /// A decoration on something it cannot decorate, or with a value it may
    /// not take.
    DecorationTarget(Decoration),
    /// One decoration twice on the same target or member.
    DuplicateDecoration(Decoration),
    /// Two decorations that exclude each other.
    ConflictingDecorations(Decoration),
    /// A buffer's explicit layout missing, misaligned or overlapping.
    ExplicitLayout,

    /// A variable's storage class not admitted, or not its pointer type's.
    StorageClass(StorageClass),
    /// A variable whose type its storage class does not allow.
    VariableType,
    /// An initializer not admitted, or of the wrong type.
    Initializer,
    /// A pointer where a logical pointer may not be: stored, selected,
    /// returned, compared, or consumed by an instruction that does not take
    /// one.
    LogicalPointer,
    /// An access chain whose indexes do not walk its base's type to its
    /// result's.
    AccessChain,
    /// A write through a pointer to memory that may not be written.
    NotWritable,
    /// A read through a pointer to memory that may not be read.
    NotReadable,
    /// An image, sampler or sampled image used other than by the
    /// instructions that operate on it.
    OpaqueUse,

    /// An operand whose type the instruction does not take; the index counts
    /// the instruction's operands, its result type and result included.
    OperandType(u8),
    /// A result type the instruction cannot produce.
    ResultType,
    /// A literal or constant operand out of the range the instruction takes.
    OperandValue(u8),
    /// A scope or memory-semantics operand not admitted.
    Semantics,
    /// Image operands not admitted with the instruction or the image.
    ImageOperands,

    /// A block not opened by a label or not closed by a terminator, a
    /// function with no blocks, or a body outside a function.
    Block,
    /// A branch to something not a label of the function, or to its entry
    /// block.
    BranchTarget,
    /// Blocks out of dominance order.
    BlockOrder,
    /// An `OpPhi` misplaced, or whose parents are not its block's
    /// predecessors.
    Phi,
    /// A merge instruction misplaced, or a conditional branch or switch
    /// without one.
    Merge,
    /// A merge block declared by two headers, or not strictly dominated by
    /// its header.
    MergeBlock,
    /// A loop's continue target wrongly placed against its header, merge and
    /// back-edge block.
    ContinueTarget,
    /// A back edge to a block that is not a loop header, or a loop header
    /// with other than one back edge.
    BackEdge,
    /// A branch leaving a construct other than to a place it may leave to.
    ConstructExit,
    /// A branch entering a construct other than at its header.
    ConstructEntry,
    /// A switch's cases branching to each other other than as its targets'
    /// order allows.
    Fallthrough,
    /// A structurally reachable block from which no function termination is
    /// reachable.
    NoExit,

    /// A call whose arguments do not match the callee's parameters.
    Call,
    /// Recursion in the call graph.
    Recursion,
    /// A return of the wrong type, or none from a function returning one.
    Return,

    /// An entry point's function, model or name not admitted.
    EntryPoint,
    /// An instruction its entry point's execution model does not allow.
    ExecutionModel,
    /// An execution mode not admitted for its entry point, set twice, or one
    /// the model requires missing.
    ExecutionMode(ExecutionMode),
    /// A global variable an entry point uses that its interface does not
    /// list, or an interface entry that is not a global variable.
    Interface,
    /// A built-in of the wrong type, storage class or model, or used twice.
    BuiltIn,
    /// Interface locations or components missing, misassigned or
    /// overlapping.
    Location,
    /// A resource variable without its set and binding, of a type no
    /// descriptor holds, or sharing its binding.
    Resource,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "word {}: {}", self.word, self.rule)
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EntryPoints => "entry points",
            Self::CallDepth => "the depth of its calls",
            Self::Bindings => "an entry point's bindings",
            Self::Samplings => "a function's samplings",
            Self::SamplingWork => "the samplings followed through it",
            Self::Locations => "an interface's locations",
            Self::TypeDepth => "the nesting of its types",
            Self::Nesting => "the nesting of its constructs",
            Self::Members => "a structure's members",
            Self::Indexes => "an instruction's indexes",
            Self::String => "a string's length",
            Self::Globals => "global variables",
            Self::Locals => "function variables",
            Self::Modes => "an entry point's execution modes",
        })
    }
}

impl fmt::Display for Rule {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per rule keeps the rules one table"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Version => f.write_str("a version older than the oldest admitted"),
            Self::Capability(capability) => {
                write!(f, "capability {}, which is not admitted", capability.0)
            }
            Self::Extension => f.write_str("an extension that is not admitted"),
            Self::InstructionNotEnabled(op) => {
                write!(f, "{op:?}, which nothing the module declares enables")
            }
            Self::EnumerantNotEnabled(kind, value) => {
                write!(
                    f,
                    "{kind:?} {value}, which nothing the module declares enables"
                )
            }
            Self::NotAdmitted(op) => write!(f, "{op:?}, which is never admitted"),
            Self::AddressingModel => f.write_str("an addressing model other than Logical"),
            Self::MemoryModel => f.write_str("a memory model other than GLSL450"),
            Self::Layout => f.write_str("an instruction outside the place it belongs"),
            Self::NoEntryPoint => f.write_str("no entry point"),
            Self::Limit(limit) => write!(f, "past the bound on {limit}"),
            Self::OutOfMemory => f.write_str("the memory validating it needs could not be had"),
            Self::Undefined(id) => write!(f, "%{} used where nothing defines it", id.get()),
            Self::ForwardReference(id) => write!(f, "%{} used before its definition", id.get()),
            Self::OtherFunction(id) => write!(f, "%{} local to another function", id.get()),
            Self::WrongKind(id) => {
                write!(
                    f,
                    "%{} naming the wrong sort of thing for its operand",
                    id.get()
                )
            }
            Self::NotDominated(id) => {
                write!(f, "a use of %{} its definition does not dominate", id.get())
            }
            Self::Type => f.write_str("a type that is not well formed"),
            Self::DuplicateType => f.write_str("a type declared twice"),
            Self::ArrayLength => {
                f.write_str("an array length that is not an integer constant of at least one")
            }
            Self::RuntimeArray => {
                f.write_str("a run-time array other than ending a storage buffer")
            }
            Self::Contents => f.write_str("a type holding what its storage class or use may not"),
            Self::Constant => f.write_str("a constant of the wrong type or constituents"),
            Self::SpecConstantOp => {
                f.write_str("a specialization constant's operation not admitted, or mistyped")
            }
            Self::DecorationTarget(decoration) => {
                write!(f, "decoration {} on what it cannot decorate", decoration.0)
            }
            Self::DuplicateDecoration(decoration) => {
                write!(f, "decoration {} twice on one target", decoration.0)
            }
            Self::ConflictingDecorations(decoration) => {
                write!(f, "decoration {} beside one it excludes", decoration.0)
            }
            Self::ExplicitLayout => {
                f.write_str("a buffer's layout missing, misaligned or overlapping")
            }
            Self::StorageClass(class) => write!(f, "storage class {}, not admitted there", class.0),
            Self::VariableType => {
                f.write_str("a variable of a type its storage class does not allow")
            }
            Self::Initializer => f.write_str("an initializer not admitted, or of the wrong type"),
            Self::LogicalPointer => f.write_str("a pointer where a logical pointer may not be"),
            Self::AccessChain => f.write_str("an access chain whose indexes do not walk its type"),
            Self::NotWritable => f.write_str("a write to memory that may not be written"),
            Self::NotReadable => f.write_str("a read of memory that may not be read"),
            Self::OpaqueUse => f.write_str("an image or sampler used other than to operate on it"),
            Self::OperandType(at) => {
                write!(f, "operand {at} of a type the instruction does not take")
            }
            Self::ResultType => f.write_str("a result type the instruction cannot produce"),
            Self::OperandValue(at) => {
                write!(f, "operand {at} out of the range the instruction takes")
            }
            Self::Semantics => f.write_str("a scope or memory semantics not admitted"),
            Self::ImageOperands => {
                f.write_str("image operands not admitted with the instruction or image")
            }
            Self::Block => {
                f.write_str("a block not opened by a label or not closed by a terminator")
            }
            Self::BranchTarget => {
                f.write_str("a branch to no label of the function, or to its entry")
            }
            Self::BlockOrder => f.write_str("blocks out of dominance order"),
            Self::Phi => {
                f.write_str("an OpPhi misplaced, or whose parents are not its predecessors")
            }
            Self::Merge => f.write_str("a merge instruction misplaced, or a branch missing one"),
            Self::MergeBlock => {
                f.write_str("a merge block declared twice, or not dominated by its header")
            }
            Self::ContinueTarget => f.write_str("a loop's continue target wrongly placed"),
            Self::BackEdge => f.write_str("a back edge to no loop header, or a loop without one"),
            Self::ConstructExit => {
                f.write_str("a branch leaving a construct other than where it may")
            }
            Self::ConstructEntry => {
                f.write_str("a branch entering a construct other than at its header")
            }
            Self::Fallthrough => f.write_str("a switch's cases falling through out of their order"),
            Self::NoExit => f.write_str("a block from which no return is reachable"),
            Self::Call => f.write_str("a call whose arguments do not match its callee"),
            Self::Recursion => f.write_str("recursion"),
            Self::Return => f.write_str("a return of the wrong type, or none"),
            Self::EntryPoint => {
                f.write_str("an entry point's function, model or name not admitted")
            }
            Self::ExecutionModel => {
                f.write_str("an instruction its entry point's model does not allow")
            }
            Self::ExecutionMode(mode) => {
                write!(
                    f,
                    "execution mode {} not admitted, repeated, or missing",
                    mode.0
                )
            }
            Self::Interface => {
                f.write_str("a global variable its entry point uses but does not list")
            }
            Self::BuiltIn => {
                f.write_str("a built-in of the wrong type, class or model, or used twice")
            }
            Self::Location => {
                f.write_str("interface locations missing, misassigned or overlapping")
            }
            Self::Resource => f.write_str("a resource without its set and binding, or sharing one"),
        }
    }
}

impl Rule {
    pub(crate) const fn at(self, instruction: &Instruction) -> Refusal {
        Refusal {
            word: instruction.at,
            rule: self,
        }
    }

    pub(crate) const fn module(self) -> Refusal {
        Refusal {
            word: 0,
            rule: self,
        }
    }
}

/// A sentinel for "none" in the validator's index tables.
pub(crate) const NONE: u32 = u32::MAX;

/// What a module-scope id names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum What {
    Type,
    Constant,
    SpecConstant,
    Undef,
    Variable,
    Function,
    Parameter,
    Label,
    Value,
    Import,
    String,
}

/// What the validator knows of one defined id, by its definition slot.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Def {
    pub(crate) what: What,
    /// A type's own index, or a value's type's; [`NONE`] otherwise.
    pub(crate) ty: u32,
    /// The function an id is local to, by index; [`NONE`] at module scope.
    pub(crate) scope: u32,
}

pub(crate) use scan::Validator;

/// Reserve room for `additional` more items, failing closed.
pub(crate) fn reserve<T>(list: &mut Vec<T>, additional: usize) -> Result<(), Refusal> {
    list.try_reserve(additional)
        .map_err(|_| Rule::OutOfMemory.module())
}

/// A list of exactly `capacity` reserved items.
pub(crate) fn with_capacity<T>(capacity: usize) -> Result<Vec<T>, Refusal> {
    let mut list = Vec::new();
    list.try_reserve_exact(capacity)
        .map_err(|_| Rule::OutOfMemory.module())?;
    Ok(list)
}

/// Make `list` exactly `len` copies of `value`, in the memory it holds where
/// that suffices, failing closed.
pub(crate) fn refill<T: Clone>(list: &mut Vec<T>, len: usize, value: T) -> Result<(), Refusal> {
    list.clear();
    reserve(list, len)?;
    list.resize(len, value);
    Ok(())
}

/// Push `item`, reserving first so growth fails closed.
pub(crate) fn push<T>(list: &mut Vec<T>, item: T) -> Result<(), Refusal> {
    reserve(list, 1)?;
    list.push(item);
    Ok(())
}

/// Collect `items`, every allocation failing closed.
pub(crate) fn collect<T>(items: impl Iterator<Item = T>) -> Result<Vec<T>, Refusal> {
    let mut list = with_capacity(items.size_hint().0)?;
    for item in items {
        push(&mut list, item)?;
    }
    Ok(list)
}

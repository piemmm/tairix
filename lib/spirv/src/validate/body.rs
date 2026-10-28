//! Function bodies: every instruction's operands scoped to its function and
//! dominated by their definitions, the pointer and opaque-value rules, and
//! each instruction typed by its family; with what each function does that
//! its entry points' checks need.

use alloc::vec::Vec;

use super::allow::{class, Class};
pub(crate) use super::binding::Access;
use super::cfg::{Cfg, Scratch};
use super::types::{Holds, Ty};
use super::{push, Refusal, Rule, Validator, What, MAX_INDEXES, NONE};
use crate::enumerant::{FunctionControl, StorageClass};
use crate::{Id, Instruction, Op, Operand, Value};

/// How an instruction of a block is checked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Typing {
    Nop,
    IntArith,
    UnsignedArith,
    FloatArith,
    IntUnary,
    FloatUnary,
    Shift,
    IntCompare,
    FloatCompare,
    Logical,
    LogicalNot,
    FloatClass,
    AnyAll,
    Select,
    FloatToInt { unsigned: bool },
    IntToFloat,
    IntWidth { unsigned: bool },
    FloatWidth,
    QuantizeToF16,
    Bitcast,
    VectorExtractDynamic,
    VectorInsertDynamic,
    VectorShuffle,
    CompositeConstruct,
    CompositeExtract,
    CompositeInsert,
    CopyObject,
    Transpose,
    CopyLogical,
    VectorTimesScalar,
    MatrixTimesScalar,
    VectorTimesMatrix,
    MatrixTimesVector,
    MatrixTimesMatrix,
    OuterProduct,
    Dot,
    Extended { unsigned: bool },
    BitFieldInsert,
    BitFieldExtract,
    BitReverse,
    BitCount,
    Derivative,
    Phi,
    LoopMerge,
    SelectionMerge,
    Branch,
    BranchConditional,
    Switch,
    Kill,
    Return,
    ReturnValue,
    Unreachable,
    TerminateInvocation,
    FunctionCall,
    Load,
    Store,
    CopyMemory,
    AccessChain,
    ArrayLength,
    ImageTexelPointer,
    SampledImage,
    Sample(SampleForm),
    Fetch,
    Gather { dref: bool },
    ImageRead,
    ImageWrite,
    Image,
    QuerySizeLod,
    QuerySize,
    QueryLod,
    QueryLevels,
    QuerySamples,
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    AtomicCompareExchange,
    AtomicStep,
    AtomicArith,
    ControlBarrier,
    MemoryBarrier,
    Elect,
    Vote,
    AllEqual,
    Broadcast,
    BroadcastFirst,
    Ballot,
    InverseBallot,
    BallotBitExtract,
    BallotBitCount,
    BallotFind,
    GroupReduce(Reduce),
    ExtInst,
}

/// Which sampling instruction: depth-comparing, projective, explicit-LOD.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct SampleForm {
    pub(crate) dref: bool,
    pub(crate) proj: bool,
    pub(crate) explicit: bool,
}

impl SampleForm {
    const fn new(dref: bool, proj: bool, explicit: bool) -> Self {
        Self {
            dref,
            proj,
            explicit,
        }
    }

    pub(crate) const PLAIN: Self = Self::new(false, false, false);
    pub(crate) const EXPLICIT: Self = Self::new(false, false, true);
    pub(crate) const DREF: Self = Self::new(true, false, false);
    pub(crate) const DREF_EXPLICIT: Self = Self::new(true, false, true);
    pub(crate) const PROJ: Self = Self::new(false, true, false);
    pub(crate) const PROJ_EXPLICIT: Self = Self::new(false, true, true);
    pub(crate) const PROJ_DREF: Self = Self::new(true, true, false);
    pub(crate) const PROJ_DREF_EXPLICIT: Self = Self::new(true, true, true);
}

/// A group reduction's value kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reduce {
    Int,
    Unsigned,
    Float,
    Logical,
}

impl Typing {
    /// Whether it ends a block.
    pub(crate) const fn terminates(self) -> bool {
        matches!(
            self,
            Self::Branch
                | Self::BranchConditional
                | Self::Switch
                | Self::Kill
                | Self::Return
                | Self::ReturnValue
                | Self::Unreachable
                | Self::TerminateInvocation
        )
    }

    /// Whether its result may be a logical pointer.
    const fn makes_pointer(self) -> bool {
        matches!(
            self,
            Self::AccessChain | Self::ImageTexelPointer | Self::CopyObject
        )
    }

    /// Whether it may take a logical pointer as an operand.
    const fn takes_pointer(self) -> bool {
        matches!(
            self,
            Self::Load
                | Self::Store
                | Self::AccessChain
                | Self::FunctionCall
                | Self::ImageTexelPointer
                | Self::CopyMemory
                | Self::CopyObject
                | Self::ArrayLength
                | Self::AtomicLoad
                | Self::AtomicStore
                | Self::AtomicExchange
                | Self::AtomicCompareExchange
                | Self::AtomicStep
                | Self::AtomicArith
                | Self::ExtInst
        )
    }

    /// Whether it may make an image, sampler or sampled image.
    const fn makes_opaque(self) -> bool {
        matches!(self, Self::Load | Self::SampledImage | Self::Image)
    }

    /// Whether it may take an image, sampler or sampled image.
    const fn takes_opaque(self) -> bool {
        matches!(
            self,
            Self::SampledImage
                | Self::Sample(_)
                | Self::Fetch
                | Self::Gather { .. }
                | Self::ImageRead
                | Self::ImageWrite
                | Self::Image
                | Self::QuerySizeLod
                | Self::QuerySize
                | Self::QueryLod
                | Self::QueryLevels
                | Self::QuerySamples
        )
    }

    /// Whether it may move or convert 16-bit data the module may only
    /// store, not compute with.
    const fn moves_narrow(self) -> bool {
        matches!(
            self,
            Self::Load
                | Self::Store
                | Self::CopyObject
                | Self::AccessChain
                | Self::ArrayLength
                | Self::IntWidth { .. }
                | Self::FloatWidth
        )
    }
}

/// An instruction's operands after its result type and result, for typing.
#[derive(Copy, Clone)]
pub(crate) struct Args<'a> {
    pub(crate) op: Op,
    /// The result type's index, or [`NONE`].
    pub(crate) result_type: u32,
    /// The id the instruction defines.
    pub(crate) result: Option<Id>,
    pub(crate) operands: &'a [Operand],
    /// How many operands precede `operands`: a refusal counts from the
    /// instruction's first.
    pub(crate) offset: u8,
}

impl Args<'_> {
    /// The refusal naming operand `n`.
    pub(crate) fn wrong(&self, n: usize) -> Rule {
        Rule::OperandType(
            self.offset
                .saturating_add(u8::try_from(n).unwrap_or(u8::MAX)),
        )
    }

    pub(crate) fn len(&self) -> usize {
        self.operands.len()
    }
}

tairix_util::flags! {
    /// What a function's instructions need of the execution model that
    /// runs them.
    pub(crate) struct Needs(u8) {
        /// Only a fragment shader may run it: a derivative, an implicit
        /// level of detail, a discard.
        const FRAGMENT = 1;
        /// Only a compute shader may: a workgroup-scope barrier.
        const COMPUTE = 1 << 1;
    }
}

/// What one function's body does that its entry points' checks need.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    /// The functions it calls, by index, deduplicated.
    pub(crate) calls: Vec<u32>,
    /// Each global variable it names and how it accesses it, by slot.
    pub(crate) globals: Vec<(u32, Access)>,
    /// How it accesses the memory each pointer parameter points to.
    pub(crate) parameters: Vec<Access>,
    /// Each pointer argument of each call it makes.
    pub(crate) arguments: Vec<Argument>,
    pub(crate) needs: Needs,
}

/// A pointer passed to a function: what the callee does through it is
/// charged to its root, and refused where the root forbids it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Argument {
    pub(crate) callee: u32,
    pub(crate) parameter: u32,
    pub(crate) root: Root,
    /// Whether it points to images or samplers, whose variables are never
    /// written: an access through it is to their texels.
    pub(crate) texels: bool,
    /// The call's first word.
    pub(crate) word: u32,
}

/// A pointer value's root: the memory object declaration it points into,
/// by slot, and what the path to it forbids.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Root {
    pub(crate) slot: u32,
    pub(crate) forbid: Access,
}

/// The state one function's body is checked in.
pub(crate) struct Body<'c> {
    pub(crate) function: u32,
    pub(crate) cfg: &'c Cfg,
    /// The local block being checked.
    pub(crate) block: u32,
    pub(crate) facts: Facts,
    /// An `OpPhi`'s parents, kept for the next.
    pub(crate) parents: Vec<u32>,
}

impl Validator<'_> {
    /// Check function `index`: its control flow, its signature and every
    /// instruction of its body.
    pub(crate) fn function(&mut self, index: usize) -> Result<(), Refusal> {
        let mut scratch = core::mem::take(&mut self.scratch);
        let checked = self.function_in(index, &mut scratch);
        self.scratch = scratch;
        checked
    }

    /// Check function `index` in `scratch`, the tables the previous one left.
    fn function_in(&mut self, index: usize, scratch: &mut Scratch) -> Result<(), Refusal> {
        self.cfg(index, scratch)?;
        let f = self
            .functions
            .get(index)
            .cloned()
            .ok_or(Rule::Block.module())?;
        let function = u32::try_from(index).unwrap_or(NONE);
        let mut body = Body {
            function,
            cfg: &scratch.cfg,
            block: 0,
            facts: Facts::default(),
            parents: core::mem::take(&mut scratch.parents),
        };
        let parameters = self.signature(f.at, f.parameters)?;
        body.facts.parameters = super::with_capacity(parameters.len())?;
        body.facts.parameters.resize(parameters.len(), Access::NONE);
        for (block, b) in (f.blocks.start..f.blocks.end).enumerate() {
            let Some(b) = self.blocks.get(b as usize).copied() else {
                continue;
            };
            body.block = u32::try_from(block).unwrap_or(NONE);
            for index in b.at + 1..=b.terminator {
                let instruction = self.instruction(index);
                self.instruction_in(&mut body, index, instruction)
                    .map_err(|rule| rule.at(instruction))?;
            }
        }
        body.facts.calls.sort_unstable();
        body.facts.calls.dedup();
        body.facts.globals.sort_unstable_by_key(|&(slot, _)| slot);
        fold(&mut body.facts.globals);
        let facts = body.facts;
        scratch.parents = body.parents;
        if let Some(slot) = self.facts.get_mut(index) {
            *slot = facts;
        }
        Ok(())
    }

    /// `OpFunction` and its parameters against its function type; the
    /// parameter type indexes.
    fn signature(&mut self, at: u32, parameters: u32) -> Result<Vec<u32>, Refusal> {
        let function = self.instruction(at);
        let refuse = |rule: Rule| rule.at(function);
        let operands = self.m.operands(function);
        let result = self
            .m
            .result_type(function)
            .ok_or(refuse(Rule::ResultType))?;
        let result = self.type_index(result).map_err(refuse)?;
        let control = match operands.get(2).map(|o| self.m.value(o)) {
            Some(Value::Word(mask)) => FunctionControl(mask),
            _ => return Err(refuse(Rule::Layout)),
        };
        let inline = FunctionControl::INLINE.0 | FunctionControl::DONT_INLINE.0;
        if control.0 & inline == inline {
            return Err(refuse(Rule::OperandValue(2)));
        }
        let Some(Value::Id(ty)) = operands.get(3).map(|o| self.m.value(o)) else {
            return Err(refuse(Rule::Type));
        };
        let ty = self.type_index(ty).map_err(refuse)?;
        let Ty::Function {
            result: returns,
            first,
            count,
        } = self.ty(ty).ty
        else {
            return Err(refuse(Rule::OperandType(3)));
        };
        if returns != result {
            return Err(refuse(Rule::ResultType));
        }
        if count != parameters {
            return Err(refuse(Rule::Call));
        }
        let types = super::collect(self.list(first, count).iter().copied())?;
        for (n, &expected) in types.iter().enumerate() {
            let index = at + 1 + u32::try_from(n).unwrap_or(NONE);
            let parameter = self.instruction(index);
            let ty = self
                .m
                .result_type(parameter)
                .ok_or(Rule::ResultType)
                .and_then(|t| self.type_index(t))
                .map_err(|r| r.at(parameter))?;
            if ty != expected {
                return Err(Rule::ResultType.at(parameter));
            }
            if let (Some(id), Ty::Pointer { class, .. }) =
                (self.m.result(parameter), self.ty(expected).ty)
            {
                let root = self.variable_root(id, class);
                self.set_root(id, root);
            }
        }
        Ok(types)
    }

    /// Check one instruction of the block `body` is in.
    fn instruction_in(
        &mut self,
        body: &mut Body<'_>,
        index: u32,
        instruction: &Instruction,
    ) -> Result<(), Rule> {
        let op = instruction.op();
        match class(op) {
            Some(Class::Line) => self.line(instruction).map_err(|r| r.rule),
            Some(Class::Variable) => self.local_variable(instruction),
            Some(Class::Undef) => self.undef_type(instruction).map(|_| ()),
            Some(Class::Body(typing)) => {
                let result = self.m.result(instruction);
                self.uses(body, index, instruction, typing, result)?;
                let result_type = match self.m.result_type(instruction) {
                    Some(ty) => self.type_index(ty)?,
                    None => NONE,
                };
                let skip = usize::from(result_type != NONE) + usize::from(result.is_some());
                let args = Args {
                    op,
                    result_type,
                    result,
                    operands: self.m.operands(instruction).get(skip..).unwrap_or(&[]),
                    offset: u8::try_from(skip).unwrap_or(u8::MAX),
                };
                self.pointer_and_opaque(typing, &args)?;
                if !typing.moves_narrow() && !self.narrow_args(&args) {
                    return Err(Rule::Contents);
                }
                self.check_body(body, instruction, typing, &args)
            }
            _ => Err(Rule::Layout),
        }
    }

    /// Each id operand of `instruction` is defined in its function or at
    /// module scope, and dominated by its definition; a global variable it
    /// names is recorded as used.
    fn uses(
        &self,
        body: &mut Body<'_>,
        index: u32,
        instruction: &Instruction,
        typing: Typing,
        result: Option<Id>,
    ) -> Result<(), Rule> {
        let phi = typing == Typing::Phi;
        for (position, operand) in self.m.operands(instruction).iter().enumerate() {
            let Value::Id(id) = self.m.value(operand) else {
                continue;
            };
            if Some(id) == result {
                continue;
            }
            let def = self.def(id)?;
            // A rounded conversion's only use is as a store's object.
            if self.rounded
                && self.decorated(id, crate::enumerant::Decoration::FP_ROUNDING_MODE)
                && !(typing == Typing::Store && position == 1)
            {
                return Err(Rule::DecorationTarget(
                    crate::enumerant::Decoration::FP_ROUNDING_MODE,
                ));
            }
            if def.scope == NONE {
                if def.what == What::Variable {
                    let slot = self
                        .slot(id)
                        .map_or(NONE, |s| u32::try_from(s).unwrap_or(NONE));
                    push(&mut body.facts.globals, (slot, Access::NONE)).map_err(|r| r.rule)?;
                }
                continue;
            }
            if def.scope != body.function {
                return Err(Rule::OtherFunction(id));
            }
            // Labels are the control-flow checks'; a phi's values are checked
            // against its parents.
            if def.what == What::Label
                || (phi && position % 2 == 0 && position >= 2)
                || def.what == What::Parameter
            {
                continue;
            }
            self.dominates_use(body, id, index)?;
            // A sampled image is consumed in the block that makes it.
            let defined = self.defining_index(id)?;
            if self.instruction(defined).op() == Op::SAMPLED_IMAGE
                && self.block_at(body.function, defined) != Some(body.block)
            {
                return Err(Rule::OpaqueUse);
            }
        }
        Ok(())
    }

    /// Refuse a use of local `id` at instruction `index` its definition does
    /// not dominate, or in a block the entry does not reach, does not precede.
    pub(crate) fn dominates_use(&self, body: &Body<'_>, id: Id, index: u32) -> Result<(), Rule> {
        let defined = self.defining_index(id)?;
        if !body.cfg.dom.reaches(body.block) {
            return if defined < index {
                Ok(())
            } else {
                Err(Rule::ForwardReference(id))
            };
        }
        let Some(block) = self.block_at(body.function, defined) else {
            return Ok(());
        };
        let ok = if block == body.block {
            defined < index
        } else {
            body.cfg.dom.dominates(block, body.block)
        };
        if ok {
            Ok(())
        } else {
            Err(Rule::NotDominated(id))
        }
    }

    /// The local block of function `function` instruction `index` is in.
    pub(crate) fn block_at(&self, function: u32, index: u32) -> Option<u32> {
        let f = self.functions.get(function as usize)?;
        let blocks = self
            .blocks
            .get(f.blocks.start as usize..f.blocks.end as usize)?;
        let after = blocks.partition_point(|b| b.at <= index);
        let block = after.checked_sub(1)?;
        (blocks.get(block)?.terminator >= index).then(|| u32::try_from(block).unwrap_or(NONE))
    }

    /// The logical-pointer and opaque-value rules: which instructions may
    /// make or take a pointer, an image, a sampler or a sampled image.
    fn pointer_and_opaque(&self, typing: Typing, args: &Args<'_>) -> Result<(), Rule> {
        if args.result_type != NONE {
            let info = self.ty(args.result_type);
            if matches!(info.ty, Ty::Pointer { .. }) && !typing.makes_pointer() {
                return Err(Rule::LogicalPointer);
            }
            if info.holds.any(Holds::POINTER) && !matches!(info.ty, Ty::Pointer { .. }) {
                return Err(Rule::LogicalPointer);
            }
            if info.holds.any(Holds::OPAQUE) && !typing.makes_opaque() {
                return Err(Rule::OpaqueUse);
            }
        }
        for operand in args.operands {
            let Value::Id(id) = self.m.value(operand) else {
                continue;
            };
            let Ok(ty) = self.value_type(id) else {
                continue;
            };
            let info = self.ty(ty);
            let pointer = matches!(info.ty, Ty::Pointer { .. });
            if pointer && !typing.takes_pointer() {
                return Err(Rule::LogicalPointer);
            }
            if info.holds.any(Holds::OPAQUE) && !pointer && !typing.takes_opaque() {
                return Err(Rule::OpaqueUse);
            }
        }
        Ok(())
    }

    /// Whether every value `args` names, and its result, is of a width the
    /// module may compute with.
    fn narrow_args(&self, args: &Args<'_>) -> bool {
        let operand_ok = |operand: &Operand| match self.m.value(operand) {
            Value::Id(id) => !matches!(self.value_type(id), Ok(ty) if !self.narrow_computable(ty)),
            _ => true,
        };
        (args.result_type == NONE || self.narrow_computable(args.result_type))
            && args.operands.iter().all(operand_ok)
    }

    /// Record `root` as `id`'s, a pointer or an image loaded through one.
    pub(crate) fn set_root(&mut self, id: Id, root: Root) {
        if let Some(slot) = self.slot(id) {
            if let Some(entry) = self.roots.get_mut(slot) {
                *entry = root;
            }
        }
    }

    /// The root recorded for `id`.
    pub(crate) fn root(&self, id: Id) -> Option<Root> {
        let root = *self.roots.get(self.slot(id)?)?;
        (root.slot != NONE).then_some(root)
    }

    /// Record an access through a pointer rooted at `root`.
    pub(crate) fn access(
        &self,
        body: &mut Body<'_>,
        root: Root,
        access: Access,
    ) -> Result<(), Rule> {
        if root.forbid.bits() & access.bits() != 0 {
            return Err(if access.contains(Access::WRITE) {
                Rule::NotWritable
            } else {
                Rule::NotReadable
            });
        }
        let def = self
            .defs
            .get(root.slot as usize)
            .copied()
            .ok_or(Rule::LogicalPointer)?;
        match def.what {
            What::Variable if def.scope == NONE => {
                push(&mut body.facts.globals, (root.slot, access)).map_err(|r| r.rule)
            }
            What::Parameter => {
                let parameter = self.parameter_index(body.function, root.slot);
                if let Some(slot) = parameter.and_then(|p| body.facts.parameters.get_mut(p)) {
                    *slot = slot.with(access);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Which parameter of function `function` the id in `slot` is.
    fn parameter_index(&self, function: u32, slot: u32) -> Option<usize> {
        let f = self.functions.get(function as usize)?;
        let &(_, index) = self.m.definitions.get(slot as usize)?;
        let n = index.checked_sub(f.at + 1)?;
        (n < f.parameters).then_some(n as usize)
    }

    /// Check one admitted instruction of a block by its family.
    #[expect(clippy::too_many_lines, reason = "one arm per typing family")]
    fn check_body(
        &mut self,
        body: &mut Body<'_>,
        instruction: &Instruction,
        typing: Typing,
        args: &Args<'_>,
    ) -> Result<(), Rule> {
        match typing {
            Typing::Nop
            | Typing::LoopMerge
            | Typing::SelectionMerge
            | Typing::Branch
            | Typing::Unreachable => Ok(()),
            Typing::Phi => self.phi(body, args),
            Typing::BranchConditional => {
                let condition = self.arg_type(args, 0)?;
                if !matches!(
                    self.ty(condition).ty,
                    Ty::Scalar(super::types::Scalar::Bool)
                ) {
                    return Err(args.wrong(0));
                }
                Ok(())
            }
            Typing::Switch => {
                let selector = self.arg_type(args, 0)?;
                if !matches!(self.ty(selector).ty, Ty::Scalar(s) if s.is_int()) {
                    return Err(args.wrong(0));
                }
                Ok(())
            }
            Typing::Kill | Typing::TerminateInvocation => {
                body.facts.needs = body.facts.needs.with(Needs::FRAGMENT);
                Ok(())
            }
            Typing::Return => {
                let f = self
                    .functions
                    .get(body.function as usize)
                    .ok_or(Rule::Return)?;
                let returns = self.function_result(f.at)?;
                if matches!(self.ty(returns).ty, Ty::Void) {
                    Ok(())
                } else {
                    Err(Rule::Return)
                }
            }
            Typing::ReturnValue => {
                let f = self
                    .functions
                    .get(body.function as usize)
                    .ok_or(Rule::Return)?;
                let returns = self.function_result(f.at)?;
                if matches!(self.ty(returns).ty, Ty::Void) || self.arg_type(args, 0)? != returns {
                    return Err(Rule::Return);
                }
                Ok(())
            }
            Typing::FunctionCall => self.call(body, instruction, args),
            Typing::Load
            | Typing::Store
            | Typing::CopyMemory
            | Typing::AccessChain
            | Typing::ArrayLength
            | Typing::ImageTexelPointer
            | Typing::AtomicLoad
            | Typing::AtomicStore
            | Typing::AtomicExchange
            | Typing::AtomicCompareExchange
            | Typing::AtomicStep
            | Typing::AtomicArith
            | Typing::ControlBarrier
            | Typing::MemoryBarrier => self.memory(body, typing, args),
            Typing::SampledImage
            | Typing::Sample(_)
            | Typing::Fetch
            | Typing::Gather { .. }
            | Typing::ImageRead
            | Typing::ImageWrite
            | Typing::Image
            | Typing::QuerySizeLod
            | Typing::QuerySize
            | Typing::QueryLod
            | Typing::QueryLevels
            | Typing::QuerySamples => self.image(body, typing, args),
            Typing::Elect
            | Typing::Vote
            | Typing::AllEqual
            | Typing::Broadcast
            | Typing::BroadcastFirst
            | Typing::Ballot
            | Typing::InverseBallot
            | Typing::BallotBitExtract
            | Typing::BallotBitCount
            | Typing::BallotFind
            | Typing::GroupReduce(_) => self.group(typing, args),
            Typing::ExtInst => self.ext_inst(body, instruction, args),
            Typing::Derivative => {
                body.facts.needs = body.facts.needs.with(Needs::FRAGMENT);
                self.type_operation(typing, args)
            }
            Typing::CopyObject => {
                self.type_operation(typing, args)?;
                if let (Some(result), Ok(operand)) = (args.result, self.arg_id(args, 0)) {
                    if let Some(root) = self.root(operand) {
                        self.set_root(result, root);
                    }
                }
                Ok(())
            }
            _ => self.type_operation(typing, args),
        }
    }

    /// The result type of the function `OpFunction` at `at` declares.
    fn function_result(&self, at: u32) -> Result<u32, Rule> {
        self.m
            .result_type(self.instruction(at))
            .ok_or(Rule::Return)
            .and_then(|t| self.type_index(t))
    }

    /// The id operand `n` of `args` names.
    pub(crate) fn arg_id(&self, args: &Args<'_>, n: usize) -> Result<Id, Rule> {
        match args.operands.get(n).map(|o| self.m.value(o)) {
            Some(Value::Id(id)) => Ok(id),
            _ => Err(args.wrong(n)),
        }
    }

    /// The literal word operand `n` of `args` holds.
    pub(crate) fn arg_word(&self, args: &Args<'_>, n: usize) -> Result<u32, Rule> {
        match args.operands.get(n).map(|o| self.m.value(o)) {
            Some(Value::Word(word)) => Ok(word),
            _ => Err(args.wrong(n)),
        }
    }

    /// The type of the value operand `n` of `args` names.
    pub(crate) fn arg_type(&self, args: &Args<'_>, n: usize) -> Result<u32, Rule> {
        let id = self.arg_id(args, n)?;
        self.value_type(id).map_err(|_| args.wrong(n))
    }

    /// An `OpPhi`: a value of its type for each predecessor of its block,
    /// exactly once, each defined where it dominates that predecessor.
    fn phi(&self, body: &mut Body<'_>, args: &Args<'_>) -> Result<(), Rule> {
        let ty = args.result_type;
        if ty == NONE || matches!(self.ty(ty).ty, Ty::Void) {
            return Err(Rule::ResultType);
        }
        let pairs = args.len() / 2;
        if !args.len().is_multiple_of(2) {
            return Err(Rule::Phi);
        }
        let predecessors = body.cfg.predecessors.successors(body.block);
        if pairs != predecessors.len() {
            return Err(Rule::Phi);
        }
        let parents = &mut body.parents;
        parents.clear();
        super::reserve(parents, pairs).map_err(|r| r.rule)?;
        for pair in 0..pairs {
            let value = self.arg_id(args, pair * 2)?;
            if self.value_type(value).map_err(|_| args.wrong(pair * 2))? != ty {
                return Err(args.wrong(pair * 2));
            }
            let parent = self.arg_id(args, pair * 2 + 1)?;
            let parent = body.cfg.block(parent).ok_or(Rule::Phi)?;
            parents.push(parent);
            let def = self.def(value)?;
            if def.scope == body.function
                && def.what != What::Parameter
                && body.cfg.dom.reaches(parent)
            {
                let defined = self.defining_index(value)?;
                let block = self
                    .block_at(body.function, defined)
                    .ok_or(Rule::NotDominated(value))?;
                if !body.cfg.dom.dominates(block, parent) {
                    return Err(Rule::NotDominated(value));
                }
            }
        }
        parents.sort_unstable();
        if parents.as_slice() != predecessors {
            return Err(Rule::Phi);
        }
        Ok(())
    }

    /// An `OpFunctionCall`: a defined function not an entry point's, given
    /// exactly its parameters' types, pointers among them naming memory
    /// object declarations.
    fn call(
        &self,
        body: &mut Body<'_>,
        instruction: &Instruction,
        args: &Args<'_>,
    ) -> Result<(), Rule> {
        let callee = self.arg_id(args, 0)?;
        let def = self.def(callee)?;
        if def.what != What::Function {
            return Err(args.wrong(0));
        }
        let function = self.function_of(callee).ok_or(args.wrong(0))?;
        let at = self.functions.get(function).map_or(NONE, |f| f.at);
        let ty = match self
            .m
            .operands(self.instruction(at))
            .get(3)
            .map(|o| self.m.value(o))
        {
            Some(Value::Id(ty)) => self.type_index(ty)?,
            _ => return Err(args.wrong(0)),
        };
        let Ty::Function {
            result,
            first,
            count,
        } = self.ty(ty).ty
        else {
            return Err(args.wrong(0));
        };
        if args.result_type != result {
            return Err(Rule::ResultType);
        }
        let arguments = args.len() - 1;
        if arguments > MAX_INDEXES {
            return Err(Rule::Limit(super::Limit::Indexes));
        }
        if arguments != count as usize {
            return Err(Rule::Call);
        }
        let callee_index = u32::try_from(function).unwrap_or(NONE);
        for (n, &expected) in self.list(first, count).iter().enumerate() {
            let argument = self.arg_id(args, n + 1)?;
            if self.value_type(argument).map_err(|_| args.wrong(n + 1))? != expected {
                return Err(Rule::Call);
            }
            if let Ty::Pointer { class, pointee } = self.ty(expected).ty {
                let root = self.root(argument).ok_or(Rule::LogicalPointer)?;
                if !self.memory_object(argument, root, class, pointee) {
                    return Err(Rule::LogicalPointer);
                }
                let argument = Argument {
                    callee: callee_index,
                    parameter: u32::try_from(n).unwrap_or(NONE),
                    root,
                    texels: class == StorageClass::UNIFORM_CONSTANT,
                    word: instruction.at,
                };
                push(&mut body.facts.arguments, argument).map_err(|r| r.rule)?;
            }
        }
        push(&mut body.facts.calls, callee_index).map_err(|r| r.rule)
    }

    /// Whether `argument`, a pointer rooted at `root`, may be passed to a
    /// function: a memory object declaration itself, or an element of an
    /// array of images or samplers one declares.
    fn memory_object(&self, argument: Id, root: Root, class: StorageClass, pointee: u32) -> bool {
        let named = self
            .m
            .definitions
            .get(root.slot as usize)
            .is_some_and(|&(id, _)| id == argument);
        let element = class == StorageClass::UNIFORM_CONSTANT
            && matches!(self.ty(pointee).ty, Ty::Image(_) | Ty::Sampler);
        named || element
    }
}

/// Fold sorted `(slot, access)` entries into one per slot.
pub(crate) fn fold(list: &mut Vec<(u32, Access)>) {
    let mut write = 0;
    for read in 0..list.len() {
        let (slot, access) = list[read];
        if write > 0 && list[write - 1].0 == slot {
            list[write - 1].1 = list[write - 1].1.with(access);
        } else {
            list[write] = (slot, access);
            write += 1;
        }
    }
    list.truncate(write);
}

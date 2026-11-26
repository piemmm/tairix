//! The guard pass (`plans/SHADER.md` SH6): an admitted module as a device
//! that bounds only buffers and images is given it, every dynamic index into
//! an array, matrix or vector of other memory held within the level it steps
//! into — a read past it zero, a write past it nothing — as the software
//! device holds every access; a dynamic vector component likewise.
//!
//! Each such index is compared with what it reaches of its level, and the
//! access made through it clamped to the last element it reaches. A load's
//! value is selected to its type's null; a store, an atomic, and a composite
//! load before 1.4, whose `OpSelect` takes no composite, are predicated, their
//! block split around them. The result is admitted again and confirmed by
//! [`Guarded::new`], which trusts nothing of the pass.

use alloc::vec::Vec;

use super::allow::{class, Class, V1_4};
use super::fold::literal;
use super::memory::reach;
use super::types::{Holds, Scalar as Resolved, Ty};
use super::view::{Scalar, Type, TypeId};
use super::{Admitted, Refusal, Rule};
use crate::decode::decode_owned;
use crate::encode::{Arg, Encoder};
use crate::enumerant::{Capability, MemoryAccess, StorageClass};
use crate::{EncodeError, Fault, Id, Instruction, Module, Op, Operand, Value, MAX_INSTRUCTIONS};

/// Why a module could not be guarded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GuardError {
    /// The memory guarding needs could not be had.
    OutOfMemory,
    /// The guarded module would pass a module's bounds: its bytes, its
    /// instructions or ids, or one of the validator's.
    TooLarge,
    /// The validator refused the guarded module: the pass's fault.
    Refused(Refusal),
    /// The pass could not write the module, or left an index unbounded that
    /// [`Guarded::new`] found: its fault.
    Unguarded,
}

impl From<EncodeError> for GuardError {
    fn from(error: EncodeError) -> Self {
        match error {
            EncodeError::OutOfMemory => Self::OutOfMemory,
            EncodeError::TooLarge | EncodeError::TooLong | EncodeError::IdsExhausted => {
                Self::TooLarge
            }
            EncodeError::IdNotIssued | EncodeError::InteriorNul => Self::Unguarded,
        }
    }
}

/// An admitted module every dynamic index of which, into a level of memory no
/// device bounds, is held within that level.
#[derive(Debug)]
pub struct Guarded(Admitted);

impl Guarded {
    /// `admitted`, where every dynamic index into an array, matrix or vector of
    /// function, private, workgroup, push-constant or interface memory, and
    /// every dynamic vector component, is clamped to the last element it
    /// reaches; `None` where one is not.
    #[must_use]
    pub fn new(admitted: Admitted) -> Option<Self> {
        bounds_every_index(&admitted).then_some(Self(admitted))
    }

    /// The module.
    #[must_use]
    pub const fn admitted(&self) -> &Admitted {
        &self.0
    }
}

impl Admitted {
    /// This module guarded: as it stands where nothing in it needs the pass,
    /// rewritten and admitted again where something does.
    ///
    /// # Errors
    ///
    /// [`GuardError::TooLarge`] for a guarded module past a module's bounds,
    /// [`GuardError::OutOfMemory`] for memory the host refuses, and
    /// [`GuardError::Refused`] or [`GuardError::Unguarded`] for the pass's
    /// own fault.
    pub fn guarded(self) -> Result<Guarded, GuardError> {
        let written = {
            let mut pass = Pass::new(&self)?;
            if pass.survey()? {
                Some(pass.write()?)
            } else {
                None
            }
        };
        let Some(bytes) = written else {
            return Guarded::new(self).ok_or(GuardError::Unguarded);
        };
        drop(self);
        let module = decode_owned(bytes, MAX_INSTRUCTIONS).map_err(|error| match error.fault {
            Fault::TooLarge | Fault::TooManyInstructions => GuardError::TooLarge,
            _ => GuardError::Unguarded,
        })?;
        let admitted = Self::new(module).map_err(|refusal| match refusal.rule {
            Rule::Limit(_) => GuardError::TooLarge,
            Rule::OutOfMemory => GuardError::OutOfMemory,
            _ => GuardError::Refused(refusal),
        })?;
        Guarded::new(admitted).ok_or(GuardError::Unguarded)
    }
}

/// What the pass does with one instruction.
#[derive(Copy, Clone, Debug)]
enum Plan {
    Keep,
    /// An access chain off a reached pointer, or stepping by the dynamic
    /// indexes [`Pass::steps`] holds into bounded levels.
    Chain,
    /// A copy of a reached pointer.
    Alias,
    /// A load through a reached pointer.
    Load(Load),
    /// A store through a reached pointer.
    Store,
    /// An atomic through a reached pointer.
    Atomic,
    /// A copy between pointers either of which is reached: a load of the
    /// pointee, guarded where its source is, then a store, predicated where
    /// its target is.
    Memory {
        load: Option<Load>,
        store: bool,
        pointee: Id,
    },
    /// A dynamic vector extraction or insertion by an index that may be past
    /// the vector.
    Vector(Index),
}

/// How a load through a reached pointer reads zero past its level.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Load {
    /// Its value selected by the condition.
    Select,
    /// Its value, a vector of this many components, selected by the condition
    /// splatted: before 1.4 a vector is selected a component at a time.
    Splat(u32),
    /// Predicated, its value merged with null.
    Split,
}

/// An index the pass bounds.
#[derive(Copy, Clone, Debug)]
struct Index {
    id: Id,
    /// Its type, and that type's width.
    ty: Id,
    width: u32,
    /// The length of the level it steps into.
    length: u32,
}

/// A dynamic index of an access chain into a bounded level.
#[derive(Copy, Clone, Debug)]
struct Step {
    /// Its operand, among the chain's.
    operand: usize,
    index: Index,
}

/// A type the pass names: one the module declares, or one it declares.
#[derive(Copy, Clone, Debug)]
struct Held {
    id: Id,
    fresh: bool,
}

/// A block the pass splits.
#[derive(Copy, Clone, Debug)]
struct Split {
    label: Id,
    /// The label its last segment takes, which a successor's `OpPhi` names in
    /// its place.
    last: Id,
    /// How many accesses split it.
    count: u32,
    /// Its `OpLoopMerge`'s position where it is a loop header: that stays at
    /// its head, the rest of it moving to a block of its own, as the merge
    /// must stand in the block a back edge returns to.
    merge: Option<u32>,
}

/// A guard under way: what its survey found, then the module being written.
struct Pass<'a> {
    admitted: &'a Admitted,
    module: &'a Module,
    out: Encoder,
    /// Each instruction defining a pointer a bounded index reaches, by
    /// ascending position.
    reached: Vec<u32>,
    /// Each block split, by ascending label once surveyed.
    splits: Vec<Split>,
    boolean: Option<Held>,
    /// A 32-bit integer type, a 16-bit index is widened to.
    wide: Option<Held>,
    /// Boolean vectors of two, three and four components.
    vectors: [Option<Held>; 3],
    /// Each constant declared, by its type and value.
    constants: Vec<(Id, u64, Option<Id>)>,
    /// Each null declared, by its type.
    nulls: Vec<(Id, Option<Id>)>,
    /// The arithmetic capabilities a value the pass selects or merges needs
    /// that the module does not declare.
    capabilities: Vec<Capability>,
    /// The dynamic bounded indexes of the access chain at hand.
    steps: Vec<Step>,
    /// The operands of the instruction being written.
    args: Vec<Arg<'a>>,
    /// The condition each reached pointer is within its levels on, by its
    /// definition's ascending position.
    conditions: Vec<(u32, Id)>,
    /// The block being written where it is split, the splits it has left, the
    /// segment being written, and whether its head waits to be moved.
    block: Option<Split>,
    left: u32,
    segment: Option<Id>,
    head: bool,
}

impl<'a> Pass<'a> {
    fn new(admitted: &'a Admitted) -> Result<Self, GuardError> {
        Ok(Self {
            admitted,
            module: admitted.module(),
            out: Encoder::rewriting(admitted.module())?,
            reached: Vec::new(),
            splits: Vec::new(),
            boolean: None,
            wide: None,
            vectors: [None; 3],
            constants: Vec::new(),
            nulls: Vec::new(),
            capabilities: Vec::new(),
            steps: Vec::new(),
            args: Vec::new(),
            conditions: Vec::new(),
            block: None,
            left: 0,
            segment: None,
            head: false,
        })
    }

    /// Find what the pass must do, declaring the ids of every type, label,
    /// constant and null it will name: whether it must do anything.
    fn survey(&mut self) -> Result<bool, GuardError> {
        let module = self.module;
        let mut any = false;
        let mut block: Option<(Id, u32, Option<u32>)> = None;
        for (position, instruction) in (0u32..).zip(module.instructions()) {
            match instruction.op() {
                Op::LABEL => {
                    block = module.result(instruction).map(|label| (label, 0, None));
                    continue;
                }
                Op::LOOP_MERGE => {
                    if let Some((_, _, merge)) = block.as_mut() {
                        *merge = Some(position);
                    }
                    continue;
                }
                op if terminates(op) => {
                    if let Some((label, count, merge)) = block.take().filter(|b| b.1 > 0) {
                        let last = self.out.next_id()?;
                        push(
                            &mut self.splits,
                            Split {
                                label,
                                last,
                                count,
                                merge,
                            },
                        )?;
                    }
                    continue;
                }
                _ => {}
            }
            let splits = match self.plan(instruction)? {
                Plan::Keep => continue,
                Plan::Chain => {
                    for at in 0..self.steps.len() {
                        let step = self.steps.get(at).copied().ok_or(GuardError::Unguarded)?;
                        self.need_clamp(step.index)?;
                    }
                    push(&mut self.reached, position)?;
                    0
                }
                Plan::Alias => {
                    push(&mut self.reached, position)?;
                    0
                }
                Plan::Load(form) => {
                    self.need_load(operand_id(module, instruction, 0)?, form)?;
                    u32::from(form == Load::Split)
                }
                Plan::Store => 1,
                Plan::Atomic => {
                    if let Some(ty) = module.result_type(instruction) {
                        self.need_null(ty)?;
                    }
                    1
                }
                Plan::Memory {
                    load,
                    store,
                    pointee,
                } => {
                    if let Some(form) = load {
                        self.need_load(pointee, form)?;
                    }
                    u32::from(store) + u32::from(load == Some(Load::Split))
                }
                Plan::Vector(index) => {
                    self.need_clamp(index)?;
                    if instruction.op() == Op::VECTOR_EXTRACT_DYNAMIC {
                        self.need_null(operand_id(module, instruction, 0)?)?;
                    }
                    0
                }
            };
            any = true;
            if splits > 0 {
                let (_, count, _) = block.as_mut().ok_or(GuardError::Unguarded)?;
                *count += splits;
            }
        }
        self.settle()?;
        Ok(any)
    }

    /// Order what the survey found and give each constant and null its id.
    fn settle(&mut self) -> Result<(), GuardError> {
        self.splits.sort_unstable_by_key(|split| split.label);
        self.constants
            .sort_unstable_by_key(|&(ty, value, _)| (ty, value));
        self.constants
            .dedup_by_key(|&mut (ty, value, _)| (ty, value));
        self.nulls.sort_unstable_by_key(|&(ty, _)| ty);
        self.nulls.dedup_by_key(|&mut (ty, _)| ty);
        for (_, _, id) in &mut self.constants {
            *id = Some(self.out.next_id()?);
        }
        for (_, id) in &mut self.nulls {
            *id = Some(self.out.next_id()?);
        }
        Ok(())
    }

    /// What the pass does with `instruction`, the dynamic bounded indexes of
    /// an access chain left in [`Self::steps`].
    fn plan(&mut self, instruction: &Instruction) -> Result<Plan, GuardError> {
        let module = self.module;
        let id = |n| operand_id(module, instruction, n);
        Ok(match instruction.op() {
            Op::ACCESS_CHAIN | Op::IN_BOUNDS_ACCESS_CHAIN => {
                self.find_steps(instruction)?;
                if self.steps.is_empty() && !self.is_reached(id(2)?) {
                    Plan::Keep
                } else {
                    Plan::Chain
                }
            }
            Op::COPY_OBJECT => {
                if self.is_reached(id(2)?) {
                    Plan::Alias
                } else {
                    Plan::Keep
                }
            }
            Op::LOAD => {
                if self.is_reached(id(2)?) {
                    Plan::Load(self.form(id(0)?)?)
                } else {
                    Plan::Keep
                }
            }
            Op::STORE => {
                if self.is_reached(id(0)?) {
                    Plan::Store
                } else {
                    Plan::Keep
                }
            }
            op if atomic(op) => {
                let pointer = if module.result_type(instruction).is_some() {
                    2
                } else {
                    0
                };
                if self.is_reached(id(pointer)?) {
                    Plan::Atomic
                } else {
                    Plan::Keep
                }
            }
            Op::COPY_MEMORY => {
                let (target, source) = (id(0)?, id(1)?);
                let (store, load) = (self.is_reached(target), self.is_reached(source));
                if !store && !load {
                    return Ok(Plan::Keep);
                }
                let (_, pointee) = pointer(self.admitted, source).ok_or(GuardError::Unguarded)?;
                let pointee = self.declaration(pointee)?;
                Plan::Memory {
                    load: if load {
                        Some(self.form(pointee)?)
                    } else {
                        None
                    },
                    store,
                    pointee,
                }
            }
            Op::VECTOR_EXTRACT_DYNAMIC => self.vector_index(id(2)?, id(3)?)?,
            Op::VECTOR_INSERT_DYNAMIC => self.vector_index(id(2)?, id(4)?)?,
            _ => Plan::Keep,
        })
    }

    /// Fill [`Self::steps`] with the dynamic indexes of the access chain
    /// `chain` into levels the pass bounds.
    fn find_steps(&mut self, chain: &Instruction) -> Result<(), GuardError> {
        self.steps.clear();
        let (admitted, module) = (self.admitted, self.module);
        let (class, mut ty) =
            pointer(admitted, operand_id(module, chain, 2)?).ok_or(GuardError::Unguarded)?;
        if !guarded_class(class) {
            return Ok(());
        }
        for (operand, index) in module.operands(chain).iter().enumerate().skip(3) {
            let Value::Id(index) = module.value(index) else {
                return Err(GuardError::Unguarded);
            };
            let (length, next) = level(admitted, ty, index).ok_or(GuardError::Unguarded)?;
            if let Some(length) = length.filter(|_| constant(admitted, index).is_none()) {
                let (ty, width) = integer(admitted, index).ok_or(GuardError::Unguarded)?;
                let index = Index {
                    id: index,
                    ty,
                    width,
                    length,
                };
                push(&mut self.steps, Step { operand, index })?;
            }
            if let Some(next) = next {
                ty = next;
            }
        }
        Ok(())
    }

    /// A dynamic vector operation's plan: the vector `vector` indexed by
    /// `index`, kept where `index` is a constant within it.
    fn vector_index(&self, vector: Id, index: Id) -> Result<Plan, GuardError> {
        let admitted = self.admitted;
        let Some(Type::Vector { count, .. }) =
            value_type(admitted, vector).and_then(|t| admitted.ty(t))
        else {
            return Err(GuardError::Unguarded);
        };
        if constant(admitted, index).is_some_and(|(value, width)| value < reach(count, width)) {
            return Ok(Plan::Keep);
        }
        let (ty, width) = integer(admitted, index).ok_or(GuardError::Unguarded)?;
        Ok(Plan::Vector(Index {
            id: index,
            ty,
            width,
            length: count,
        }))
    }

    /// How a load of `ty` through a reached pointer reads zero past its level.
    fn form(&self, ty: Id) -> Result<Load, GuardError> {
        let modern = self.module.version() >= V1_4;
        let of = self.admitted.type_id(ty).ok_or(GuardError::Unguarded)?;
        Ok(match self.admitted.ty(of).ok_or(GuardError::Unguarded)? {
            Type::Vector { count, .. } if !modern => Load::Splat(count),
            Type::Array { .. } | Type::Struct { .. } | Type::Matrix { .. } if !modern => {
                Load::Split
            }
            _ => Load::Select,
        })
    }

    /// Whether `id` is a pointer a bounded index reaches.
    fn is_reached(&self, id: Id) -> bool {
        self.module
            .position(id)
            .and_then(|at| u32::try_from(at).ok())
            .is_some_and(|at| self.reached.binary_search(&at).is_ok())
    }

    /// The id declaring the type `ty`.
    fn declaration(&self, ty: TypeId) -> Result<Id, GuardError> {
        self.admitted
            .kept
            .types
            .get(ty.index() as usize)
            .and_then(|info| info.id)
            .ok_or(GuardError::Unguarded)
    }

    fn need_load(&mut self, ty: Id, form: Load) -> Result<(), GuardError> {
        self.need_null(ty)?;
        if let Load::Splat(count) = form {
            self.need_vector(count)?;
        }
        Ok(())
    }

    /// Declare what clamping `index` names: the Boolean type, a 32-bit
    /// integer type for a narrower index, and the bound and last element.
    fn need_clamp(&mut self, index: Index) -> Result<(), GuardError> {
        self.need_bool()?;
        let (ty, width) = if index.width == 16 {
            (self.need_wide()?, 32)
        } else {
            (index.ty, index.width)
        };
        let bound = reach(index.length, width);
        let last = bound.checked_sub(1).ok_or(GuardError::Unguarded)?;
        for value in [bound, last] {
            push(&mut self.constants, (ty, value, None))?;
        }
        Ok(())
    }

    /// Declare the null of `ty`, and the arithmetic capability for each
    /// 16-bit type it holds the module does not declare: a value selected or
    /// merged is computed with, where a load alone only moves it.
    fn need_null(&mut self, ty: Id) -> Result<(), GuardError> {
        let holds = self
            .admitted
            .type_id(ty)
            .and_then(|of| self.admitted.kept.types.get(of.index() as usize))
            .map(|info| info.holds)
            .ok_or(GuardError::Unguarded)?;
        for (narrow, capability) in [
            (Holds::INT16, Capability::INT16),
            (Holds::FLOAT16, Capability::FLOAT16),
        ] {
            if holds.contains(narrow)
                && self
                    .admitted
                    .capabilities()
                    .binary_search(&capability)
                    .is_err()
                && !self.capabilities.contains(&capability)
            {
                push(&mut self.capabilities, capability)?;
            }
        }
        push(&mut self.nulls, (ty, None))
    }

    fn need_bool(&mut self) -> Result<Id, GuardError> {
        if let Some(held) = self.boolean {
            return Ok(held.id);
        }
        let held = self.held(|ty| matches!(ty, Ty::Scalar(Resolved::Bool)))?;
        self.boolean = Some(held);
        Ok(held.id)
    }

    fn need_wide(&mut self) -> Result<Id, GuardError> {
        if let Some(held) = self.wide {
            return Ok(held.id);
        }
        let held = match self.find(int32(false)).or_else(|| self.find(int32(true))) {
            Some(id) => Held { id, fresh: false },
            None => Held {
                id: self.out.next_id()?,
                fresh: true,
            },
        };
        self.wide = Some(held);
        Ok(held.id)
    }

    fn need_vector(&mut self, count: u32) -> Result<(), GuardError> {
        let slot = vector_slot(count)?;
        if self.vectors.get(slot).copied().flatten().is_some() {
            return Ok(());
        }
        let fresh_bool = {
            self.need_bool()?;
            self.boolean.is_some_and(|held| held.fresh)
        };
        let held = if fresh_bool {
            Held {
                id: self.out.next_id()?,
                fresh: true,
            }
        } else {
            self.held(
                |ty| matches!(ty, Ty::Vector { scalar: Resolved::Bool, count: c } if *c == count),
            )?
        };
        if let Some(entry) = self.vectors.get_mut(slot) {
            *entry = Some(held);
        }
        Ok(())
    }

    /// The type the module declares that `wanted` takes, or a fresh id for
    /// one the pass declares.
    fn held(&mut self, wanted: impl Fn(&Ty) -> bool) -> Result<Held, GuardError> {
        Ok(match self.find(wanted) {
            Some(id) => Held { id, fresh: false },
            None => Held {
                id: self.out.next_id()?,
                fresh: true,
            },
        })
    }

    fn find(&self, wanted: impl Fn(&Ty) -> bool) -> Option<Id> {
        self.admitted
            .kept
            .types
            .iter()
            .find(|info| info.id.is_some() && wanted(&info.ty))
            .and_then(|info| info.id)
    }

    /// Write the guarded module.
    fn write(mut self) -> Result<Vec<u8>, GuardError> {
        let module = self.module;
        let (mut capabilities, mut declared) = (false, false);
        for (position, instruction) in (0u32..).zip(module.instructions()) {
            let op = instruction.op();
            if op != Op::CAPABILITY && !capabilities {
                capabilities = true;
                for &capability in &self.capabilities {
                    self.out.emit(Op::CAPABILITY, &[Arg::Word(capability.0)])?;
                }
            }
            if op == Op::FUNCTION && !declared {
                declared = true;
                self.declare()?;
            }
            self.instruction(position, instruction)?;
        }
        Ok(self.out.finish())
    }

    /// Declare every type, constant and null the pass names that the module
    /// does not.
    fn declare(&mut self) -> Result<(), GuardError> {
        let boolean = self.boolean.map(|held| held.id);
        if let Some(Held { id, fresh: true }) = self.boolean {
            self.out.emit(Op::TYPE_BOOL, &[Arg::Id(id)])?;
        }
        if let Some(Held { id, fresh: true }) = self.wide {
            self.out
                .emit(Op::TYPE_INT, &[Arg::Id(id), Arg::Word(32), Arg::Word(0)])?;
        }
        for (count, held) in (2u32..).zip(self.vectors) {
            if let (Some(Held { id, fresh: true }), Some(boolean)) = (held, boolean) {
                self.out.emit(
                    Op::TYPE_VECTOR,
                    &[Arg::Id(id), Arg::Id(boolean), Arg::Word(count)],
                )?;
            }
        }
        for &(ty, value, id) in &self.constants {
            let id = id.ok_or(GuardError::Unguarded)?;
            let scalar = if self.wide.is_some_and(|held| held.fresh && held.id == ty) {
                Scalar::Int {
                    width: 32,
                    signed: false,
                }
            } else {
                match self
                    .admitted
                    .type_id(ty)
                    .and_then(|of| self.admitted.ty(of))
                {
                    Some(Type::Scalar(scalar)) => scalar,
                    _ => return Err(GuardError::Unguarded),
                }
            };
            let value = literal(scalar, value).ok_or(GuardError::Unguarded)?;
            self.out
                .emit(Op::CONSTANT, &[Arg::Id(ty), Arg::Id(id), value])?;
        }
        for &(ty, id) in &self.nulls {
            let id = id.ok_or(GuardError::Unguarded)?;
            self.out
                .emit(Op::CONSTANT_NULL, &[Arg::Id(ty), Arg::Id(id)])?;
        }
        Ok(())
    }

    fn instruction(&mut self, position: u32, instruction: &Instruction) -> Result<(), GuardError> {
        let module = self.module;
        let op = instruction.op();
        match op {
            Op::LABEL => return self.label(instruction),
            Op::PHI => return self.phi(instruction),
            Op::LINE | Op::NO_LINE => return Ok(self.out.copy(module, instruction)?),
            Op::LOOP_MERGE if self.block.is_some_and(|split| split.merge.is_some()) => {
                return Ok(());
            }
            _ => {}
        }
        if self.head {
            self.head = false;
            self.move_head()?;
        }
        if terminates(op) && self.left != 0 {
            return Err(GuardError::Unguarded);
        }
        match self.plan(instruction)? {
            Plan::Keep => Ok(self.out.copy(module, instruction)?),
            Plan::Chain => self.chain(position, instruction),
            Plan::Alias => {
                let condition = self.condition(operand_id(module, instruction, 2)?)?;
                self.out.copy(module, instruction)?;
                push(&mut self.conditions, (position, condition))
            }
            Plan::Load(form) => {
                let pointer = operand_id(module, instruction, 2)?;
                let condition = self.condition(pointer)?;
                let loaded = (
                    operand_id(module, instruction, 0)?,
                    operand_id(module, instruction, 1)?,
                    pointer,
                );
                let masks = module.operands(instruction).get(3..).unwrap_or(&[]);
                self.guarded_load(form, loaded, condition, masks)
            }
            Plan::Store => {
                let condition = self.condition(operand_id(module, instruction, 0)?)?;
                let (_, merge) = self.open(condition)?;
                self.out.copy(module, instruction)?;
                self.close(merge).map(drop)
            }
            Plan::Atomic => self.atomic(instruction),
            Plan::Memory {
                load,
                store,
                pointee,
            } => self.memory(instruction, (load, store), pointee),
            Plan::Vector(index) => self.vector(instruction, index),
        }
    }

    fn label(&mut self, instruction: &Instruction) -> Result<(), GuardError> {
        let label = self
            .module
            .result(instruction)
            .ok_or(GuardError::Unguarded)?;
        self.block = self.split_of(label);
        self.left = self.block.map_or(0, |split| split.count);
        self.head = self.block.is_some_and(|split| split.merge.is_some());
        self.segment = Some(label);
        Ok(self.out.copy(self.module, instruction)?)
    }

    fn split_of(&self, label: Id) -> Option<Split> {
        let at = self
            .splits
            .binary_search_by_key(&label, |split| split.label)
            .ok()?;
        self.splits.get(at).copied()
    }

    /// An `OpPhi`, each parent split named by its last segment.
    fn phi(&mut self, instruction: &Instruction) -> Result<(), GuardError> {
        let module = self.module;
        let operands = module.operands(instruction);
        let parent = |n: usize, operand: &Operand| match module.value(operand) {
            Value::Id(label) if n >= 3 && n % 2 == 1 => Some(label),
            _ => None,
        };
        let moved = operands.iter().enumerate().any(|(n, operand)| {
            parent(n, operand).is_some_and(|label| self.split_of(label).is_some())
        });
        if !moved {
            return Ok(self.out.copy(module, instruction)?);
        }
        self.args.clear();
        reserve(&mut self.args, operands.len())?;
        for (n, operand) in operands.iter().enumerate() {
            let arg = match parent(n, operand).and_then(|label| self.split_of(label)) {
                Some(split) => Arg::Id(split.last),
                None => arg_of(module, *operand),
            };
            self.args.push(arg);
        }
        Ok(self.out.emit(Op::PHI, &self.args)?)
    }

    /// Move a split loop header's body to a block of its own, its merge
    /// instruction staying at its head; a loop that was its own continue
    /// target continues at the block its body moves to.
    fn move_head(&mut self) -> Result<(), GuardError> {
        let module = self.module;
        let split = self.block.ok_or(GuardError::Unguarded)?;
        let merge = split
            .merge
            .and_then(|at| module.instructions().get(at as usize))
            .ok_or(GuardError::Unguarded)?;
        let body = self.out.next_id()?;
        self.args.clear();
        reserve(&mut self.args, module.operands(merge).len())?;
        for (n, operand) in module.operands(merge).iter().enumerate() {
            self.args.push(match arg_of(module, *operand) {
                Arg::Id(target) if n == 1 && target == split.label => Arg::Id(body),
                arg => arg,
            });
        }
        self.out.emit(Op::LOOP_MERGE, &self.args)?;
        self.out.emit(Op::BRANCH, &[Arg::Id(body)])?;
        self.out.emit(Op::LABEL, &[Arg::Id(body)])?;
        self.segment = Some(body);
        Ok(())
    }

    /// The condition the reached pointer `pointer` is within its levels on.
    fn condition(&self, pointer: Id) -> Result<Id, GuardError> {
        let at = self
            .module
            .position(pointer)
            .and_then(|at| u32::try_from(at).ok())
            .ok_or(GuardError::Unguarded)?;
        let found = self
            .conditions
            .binary_search_by_key(&at, |&(position, _)| position)
            .map_err(|_| GuardError::Unguarded)?;
        self.conditions
            .get(found)
            .map(|&(_, condition)| condition)
            .ok_or(GuardError::Unguarded)
    }

    /// An access chain, its dynamic bounded indexes clamped, and the
    /// condition it is within its levels on.
    fn chain(&mut self, position: u32, chain: &Instruction) -> Result<(), GuardError> {
        let module = self.module;
        let base = operand_id(module, chain, 2)?;
        let mut condition = if self.is_reached(base) {
            Some(self.condition(base)?)
        } else {
            None
        };
        for at in 0..self.steps.len() {
            let step = self.steps.get(at).copied().ok_or(GuardError::Unguarded)?;
            let (within, clamped) = self.clamp(step.index)?;
            condition = Some(match condition {
                Some(earlier) => self.and(earlier, within)?,
                None => within,
            });
            if let Some(entry) = self.steps.get_mut(at) {
                entry.index.id = clamped;
            }
        }
        let operands = module.operands(chain);
        self.args.clear();
        reserve(&mut self.args, operands.len())?;
        let mut steps = self.steps.iter().peekable();
        for (n, operand) in operands.iter().enumerate() {
            let arg = match steps.next_if(|step| step.operand == n) {
                Some(step) => Arg::Id(step.index.id),
                None => arg_of(module, *operand),
            };
            self.args.push(arg);
        }
        self.out.emit(chain.op(), &self.args)?;
        push(
            &mut self.conditions,
            (position, condition.ok_or(GuardError::Unguarded)?),
        )
    }

    /// `index` compared with what it reaches of its level, and clamped to the
    /// last element it reaches: the comparison and the clamped index. A
    /// 16-bit index is widened first, sign-extended as the signed count an
    /// access chain reads.
    fn clamp(&mut self, index: Index) -> Result<(Id, Id), GuardError> {
        let (value, ty, width) = if index.width == 16 {
            let wide = self.wide.map(|held| held.id).ok_or(GuardError::Unguarded)?;
            let widened = self.out.next_id()?;
            self.out.emit(
                Op::S_CONVERT,
                &[Arg::Id(wide), Arg::Id(widened), Arg::Id(index.id)],
            )?;
            (widened, wide, 32)
        } else {
            (index.id, index.ty, index.width)
        };
        let bound = reach(index.length, width);
        let last = bound.checked_sub(1).ok_or(GuardError::Unguarded)?;
        let (bound, last) = (self.constant(ty, bound)?, self.constant(ty, last)?);
        let boolean = self
            .boolean
            .map(|held| held.id)
            .ok_or(GuardError::Unguarded)?;
        let within = self.out.next_id()?;
        self.out.emit(
            Op::U_LESS_THAN,
            &[
                Arg::Id(boolean),
                Arg::Id(within),
                Arg::Id(value),
                Arg::Id(bound),
            ],
        )?;
        let clamped = self.out.next_id()?;
        self.out.emit(
            Op::SELECT,
            &[
                Arg::Id(ty),
                Arg::Id(clamped),
                Arg::Id(within),
                Arg::Id(value),
                Arg::Id(last),
            ],
        )?;
        Ok((within, clamped))
    }

    fn and(&mut self, a: Id, b: Id) -> Result<Id, GuardError> {
        let boolean = self
            .boolean
            .map(|held| held.id)
            .ok_or(GuardError::Unguarded)?;
        let both = self.out.next_id()?;
        self.out.emit(
            Op::LOGICAL_AND,
            &[Arg::Id(boolean), Arg::Id(both), Arg::Id(a), Arg::Id(b)],
        )?;
        Ok(both)
    }

    fn constant(&self, ty: Id, value: u64) -> Result<Id, GuardError> {
        let at = self
            .constants
            .binary_search_by_key(&(ty, value), |&(of, v, _)| (of, v))
            .map_err(|_| GuardError::Unguarded)?;
        self.constants
            .get(at)
            .and_then(|&(_, _, id)| id)
            .ok_or(GuardError::Unguarded)
    }

    fn null(&self, ty: Id) -> Result<Id, GuardError> {
        let at = self
            .nulls
            .binary_search_by_key(&ty, |&(of, _)| of)
            .map_err(|_| GuardError::Unguarded)?;
        self.nulls
            .get(at)
            .and_then(|&(_, id)| id)
            .ok_or(GuardError::Unguarded)
    }

    /// A load of `ty` into `result` through `pointer`, within its levels on
    /// `condition`, with the memory operands `masks`: zero past them.
    fn guarded_load(
        &mut self,
        form: Load,
        (ty, result, pointer): (Id, Id, Id),
        condition: Id,
        masks: &'a [Operand],
    ) -> Result<(), GuardError> {
        let null = self.null(ty)?;
        let loaded = self.out.next_id()?;
        if form == Load::Split {
            let (then, merge) = self.open(condition)?;
            self.load((ty, loaded, pointer), masks)?;
            let before = self.close(merge)?;
            let merged = [ty, result, loaded, then, null, before].map(Arg::Id);
            return Ok(self.out.emit(Op::PHI, &merged)?);
        }
        self.load((ty, loaded, pointer), masks)?;
        let chooser = match form {
            Load::Splat(count) => self.splat(condition, count)?,
            Load::Select | Load::Split => condition,
        };
        let selected = [ty, result, chooser, loaded, null].map(Arg::Id);
        Ok(self.out.emit(Op::SELECT, &selected)?)
    }

    fn load(
        &mut self,
        (ty, result, pointer): (Id, Id, Id),
        masks: &'a [Operand],
    ) -> Result<(), GuardError> {
        let module = self.module;
        self.args.clear();
        reserve(&mut self.args, masks.len() + 3)?;
        self.args.extend([ty, result, pointer].map(Arg::Id));
        self.args
            .extend(masks.iter().map(|mask| arg_of(module, *mask)));
        Ok(self.out.emit(Op::LOAD, &self.args)?)
    }

    /// `condition` as a Boolean vector of `count` components.
    fn splat(&mut self, condition: Id, count: u32) -> Result<Id, GuardError> {
        let ty = self
            .vectors
            .get(vector_slot(count)?)
            .copied()
            .flatten()
            .ok_or(GuardError::Unguarded)?
            .id;
        let splat = self.out.next_id()?;
        let args = [ty, splat, condition, condition, condition, condition].map(Arg::Id);
        let used = args
            .get(..vector_slot(count)? + 4)
            .ok_or(GuardError::Unguarded)?;
        self.out.emit(Op::COMPOSITE_CONSTRUCT, used)?;
        Ok(splat)
    }

    /// Open a construct predicated on `condition`: the segment so far ends in
    /// a branch to its then-block, which is opened. Answers the then-block's
    /// label and the merge's, the block's last segment's for its last split.
    fn open(&mut self, condition: Id) -> Result<(Id, Id), GuardError> {
        let split = self.block.ok_or(GuardError::Unguarded)?;
        self.left = self.left.checked_sub(1).ok_or(GuardError::Unguarded)?;
        let merge = if self.left == 0 {
            split.last
        } else {
            self.out.next_id()?
        };
        let then = self.out.next_id()?;
        self.out
            .emit(Op::SELECTION_MERGE, &[Arg::Id(merge), Arg::Word(0)])?;
        self.out.emit(
            Op::BRANCH_CONDITIONAL,
            &[Arg::Id(condition), Arg::Id(then), Arg::Id(merge)],
        )?;
        self.out.emit(Op::LABEL, &[Arg::Id(then)])?;
        Ok((then, merge))
    }

    /// Close the construct [`Self::open`] began at `merge`, which is opened:
    /// answers the segment that branched to it.
    fn close(&mut self, merge: Id) -> Result<Id, GuardError> {
        self.out.emit(Op::BRANCH, &[Arg::Id(merge)])?;
        self.out.emit(Op::LABEL, &[Arg::Id(merge)])?;
        self.segment.replace(merge).ok_or(GuardError::Unguarded)
    }

    /// An atomic through a reached pointer, predicated; its value, where it
    /// has one, merged with zero.
    fn atomic(&mut self, instruction: &Instruction) -> Result<(), GuardError> {
        let module = self.module;
        let result_type = module.result_type(instruction);
        let pointer = operand_id(
            module,
            instruction,
            if result_type.is_some() { 2 } else { 0 },
        )?;
        let condition = self.condition(pointer)?;
        let (then, merge) = self.open(condition)?;
        let Some(ty) = result_type else {
            self.out.copy(module, instruction)?;
            return self.close(merge).map(drop);
        };
        let result = module.result(instruction).ok_or(GuardError::Unguarded)?;
        let fresh = self.out.next_id()?;
        let operands = module.operands(instruction);
        self.args.clear();
        reserve(&mut self.args, operands.len())?;
        for (n, operand) in operands.iter().enumerate() {
            self.args.push(if n == 1 {
                Arg::Id(fresh)
            } else {
                arg_of(module, *operand)
            });
        }
        self.out.emit(instruction.op(), &self.args)?;
        let before = self.close(merge)?;
        let null = self.null(ty)?;
        let merged = [ty, result, fresh, then, null, before].map(Arg::Id);
        Ok(self.out.emit(Op::PHI, &merged)?)
    }

    /// A copy between pointers either of which is reached, as a load of
    /// `pointee` then a store, each guarded where its pointer is reached.
    fn memory(
        &mut self,
        instruction: &Instruction,
        (load, store): (Option<Load>, bool),
        pointee: Id,
    ) -> Result<(), GuardError> {
        let module = self.module;
        let (target, source) = (
            operand_id(module, instruction, 0)?,
            operand_id(module, instruction, 1)?,
        );
        let (to, from) = masks(module, module.operands(instruction).get(2..).unwrap_or(&[]))?;
        let value = self.out.next_id()?;
        match load {
            Some(form) => {
                let condition = self.condition(source)?;
                self.guarded_load(form, (pointee, value, source), condition, from)?;
            }
            None => self.load((pointee, value, source), from)?,
        }
        let merge = if store {
            let condition = self.condition(target)?;
            Some(self.open(condition)?.1)
        } else {
            None
        };
        self.args.clear();
        reserve(&mut self.args, to.len() + 2)?;
        self.args.extend([target, value].map(Arg::Id));
        self.args
            .extend(to.iter().map(|mask| arg_of(module, *mask)));
        self.out.emit(Op::STORE, &self.args)?;
        match merge {
            Some(merge) => self.close(merge).map(drop),
            None => Ok(()),
        }
    }

    /// A dynamic vector extraction past its vector zero, an insertion past it
    /// the vector unchanged.
    fn vector(&mut self, instruction: &Instruction, index: Index) -> Result<(), GuardError> {
        let module = self.module;
        let id = |n| operand_id(module, instruction, n);
        let (ty, result, vector) = (id(0)?, id(1)?, id(2)?);
        let (within, clamped) = self.clamp(index)?;
        if instruction.op() == Op::VECTOR_EXTRACT_DYNAMIC {
            let extracted = self.out.next_id()?;
            let extract = [ty, extracted, vector, clamped].map(Arg::Id);
            self.out.emit(Op::VECTOR_EXTRACT_DYNAMIC, &extract)?;
            let selected = [ty, result, within, extracted, self.null(ty)?].map(Arg::Id);
            return Ok(self.out.emit(Op::SELECT, &selected)?);
        }
        let component = component_type(module, ty).ok_or(GuardError::Unguarded)?;
        let (held, kept) = (self.out.next_id()?, self.out.next_id()?);
        let extract = [component, held, vector, clamped].map(Arg::Id);
        self.out.emit(Op::VECTOR_EXTRACT_DYNAMIC, &extract)?;
        let selected = [component, kept, within, id(3)?, held].map(Arg::Id);
        self.out.emit(Op::SELECT, &selected)?;
        let insert = [ty, result, vector, kept, clamped].map(Arg::Id);
        Ok(self.out.emit(Op::VECTOR_INSERT_DYNAMIC, &insert)?)
    }
}

/// Whether every dynamic index of `admitted` into a bounded level, and every
/// dynamic vector component, is clamped to the last element it reaches.
fn bounds_every_index(admitted: &Admitted) -> bool {
    let module = admitted.module();
    module.instructions().iter().all(|instruction| {
        let id = |n| operand_id(module, instruction, n).ok();
        match instruction.op() {
            Op::ACCESS_CHAIN | Op::IN_BOUNDS_ACCESS_CHAIN => chain_bounded(admitted, instruction),
            Op::VECTOR_EXTRACT_DYNAMIC => vector_bounded(admitted, id(2), id(3)),
            Op::VECTOR_INSERT_DYNAMIC => vector_bounded(admitted, id(2), id(4)),
            _ => true,
        }
    })
}

fn chain_bounded(admitted: &Admitted, chain: &Instruction) -> bool {
    let module = admitted.module();
    let Some((class, mut ty)) = operand_id(module, chain, 2)
        .ok()
        .and_then(|base| pointer(admitted, base))
    else {
        return false;
    };
    if !guarded_class(class) {
        return true;
    }
    for operand in module.operands(chain).get(3..).unwrap_or(&[]) {
        let Value::Id(index) = module.value(operand) else {
            return false;
        };
        let Some((length, next)) = level(admitted, ty, index) else {
            return false;
        };
        if length.is_some_and(|length| !clamped(admitted, index, length)) {
            return false;
        }
        if let Some(next) = next {
            ty = next;
        }
    }
    true
}

fn vector_bounded(admitted: &Admitted, vector: Option<Id>, index: Option<Id>) -> bool {
    let (Some(vector), Some(index)) = (vector, index) else {
        return false;
    };
    match value_type(admitted, vector).and_then(|ty| admitted.ty(ty)) {
        Some(Type::Vector { count, .. }) => clamped(admitted, index, count),
        _ => false,
    }
}

/// Whether `index` is within a level of `length` whatever it is computed
/// from: a constant within what it reaches, or a value selected below what it
/// reaches by a comparison with it, else the last element.
fn clamped(admitted: &Admitted, index: Id, length: u32) -> bool {
    if let Some((value, width)) = constant(admitted, index) {
        return value < reach(length, width);
    }
    let module = admitted.module();
    let Some(select) = module
        .definition(index)
        .filter(|instruction| instruction.op() == Op::SELECT)
    else {
        return false;
    };
    let id = |instruction: &Instruction, n: usize| operand_id(module, instruction, n).ok();
    let (Some(within), Some(value), Some(last)) = (id(select, 2), id(select, 3), id(select, 4))
    else {
        return false;
    };
    let Some(compare) = module
        .definition(within)
        .filter(|instruction| instruction.op() == Op::U_LESS_THAN)
    else {
        return false;
    };
    let (Some(compared), Some(bound)) = (id(compare, 2), id(compare, 3)) else {
        return false;
    };
    match (constant(admitted, bound), constant(admitted, last)) {
        (Some((bound, width)), Some((last, _))) => {
            compared == value && bound == reach(length, width) && last.checked_add(1) == Some(bound)
        }
        _ => false,
    }
}

/// Whether the pass bounds indexes into memory of `class`: what no device
/// bounds by the resource bound, as it does a buffer, an image and a
/// descriptor.
const fn guarded_class(class: StorageClass) -> bool {
    matches!(
        class,
        StorageClass::FUNCTION
            | StorageClass::PRIVATE
            | StorageClass::WORKGROUP
            | StorageClass::PUSH_CONSTANT
            | StorageClass::INPUT
            | StorageClass::OUTPUT
    )
}

/// Stepping into `ty` by `index`: the length of the level a dynamic index is
/// bounded by — an array's, a matrix's columns, a vector's components — and
/// the type stepped to, where there is one; `None` for a type an access chain
/// may not step into here.
fn level(admitted: &Admitted, ty: TypeId, index: Id) -> Option<(Option<u32>, Option<TypeId>)> {
    Some(match admitted.ty(ty)? {
        Type::Array { element, length } => (Some(length), Some(element)),
        Type::Matrix {
            column, columns, ..
        } => (Some(columns), Some(column)),
        Type::Vector { count, .. } => (Some(count), None),
        Type::Struct { .. } => {
            let (member, _) = constant(admitted, index)?;
            let member = u32::try_from(member).ok()?;
            (None, Some(admitted.member_type(ty, member)?))
        }
        _ => return None,
    })
}

/// The value of the integer `OpConstant` `id`, within its width, and the
/// width.
fn constant(admitted: &Admitted, id: Id) -> Option<(u64, u32)> {
    let module = admitted.module();
    let instruction = module
        .definition(id)
        .filter(|instruction| instruction.op() == Op::CONSTANT)?;
    let Type::Scalar(Scalar::Int { width, .. }) = admitted.ty(admitted.result_type(instruction)?)?
    else {
        return None;
    };
    let Value::Number(value) = module.value(module.operands(instruction).get(2)?) else {
        return None;
    };
    let mask = u64::MAX
        .checked_shr(64u32.saturating_sub(width))
        .unwrap_or(0);
    Some((value & mask, width))
}

/// The type of the value `id`.
fn value_type(admitted: &Admitted, id: Id) -> Option<TypeId> {
    let module = admitted.module();
    admitted.type_id(module.result_type(module.definition(id)?)?)
}

/// The integer type of the value `id`, and its width.
fn integer(admitted: &Admitted, id: Id) -> Option<(Id, u32)> {
    let module = admitted.module();
    let ty = module.result_type(module.definition(id)?)?;
    match admitted.ty(admitted.type_id(ty)?)? {
        Type::Scalar(Scalar::Int { width, .. }) => Some((ty, width)),
        _ => None,
    }
}

/// The storage class and pointee of the pointer `id`.
fn pointer(admitted: &Admitted, id: Id) -> Option<(StorageClass, TypeId)> {
    match admitted.ty(value_type(admitted, id)?)? {
        Type::Pointer { class, pointee } => Some((class, pointee)),
        _ => None,
    }
}

/// The component type the vector type `ty` declares.
fn component_type(module: &Module, ty: Id) -> Option<Id> {
    let vector = module.definition(ty)?;
    match module.value(module.operands(vector).get(1)?) {
        Value::Id(component) => Some(component),
        _ => None,
    }
}

/// The id operand `n` of `instruction` names.
fn operand_id(module: &Module, instruction: &Instruction, n: usize) -> Result<Id, GuardError> {
    match module
        .operands(instruction)
        .get(n)
        .map(|operand| module.value(operand))
    {
        Some(Value::Id(id)) => Ok(id),
        _ => Err(GuardError::Unguarded),
    }
}

/// `operand` written again as it stands.
fn arg_of(module: &Module, operand: Operand) -> Arg<'_> {
    match module.value(&operand) {
        Value::Id(id) => Arg::Id(id),
        Value::Word(word) => Arg::Word(word),
        Value::Number(number) => match u32::try_from(number) {
            Ok(word) if operand.words == 1 => Arg::Word(word),
            _ => Arg::Wide(number),
        },
        Value::Text(text) => Arg::Text(text),
    }
}

/// A copy's memory operands, as its target's and its source's: its first
/// mask with its parameter, then its second where it has one, else the first
/// again.
fn masks<'m>(
    module: &Module,
    operands: &'m [Operand],
) -> Result<(&'m [Operand], &'m [Operand]), GuardError> {
    let Some(first) = operands.first() else {
        return Ok((&[], &[]));
    };
    let Value::Word(mask) = module.value(first) else {
        return Err(GuardError::Unguarded);
    };
    let end = if mask & MemoryAccess::ALIGNED.0 != 0 {
        2
    } else {
        1
    };
    let (target, rest) = operands
        .split_at_checked(end)
        .ok_or(GuardError::Unguarded)?;
    Ok((target, if rest.is_empty() { target } else { rest }))
}

/// Whether a type is the 32-bit integer of `signed`.
fn int32(signed: bool) -> impl Fn(&Ty) -> bool {
    move |ty| matches!(ty, Ty::Scalar(Resolved::Int { width: 32, signed: s }) if *s == signed)
}

/// Whether `op` ends a block.
fn terminates(op: Op) -> bool {
    matches!(class(op), Some(Class::Body(typing)) if typing.terminates())
}

/// Whether `op` is an atomic the pass may predicate.
const fn atomic(op: Op) -> bool {
    matches!(
        op,
        Op::ATOMIC_LOAD
            | Op::ATOMIC_STORE
            | Op::ATOMIC_EXCHANGE
            | Op::ATOMIC_COMPARE_EXCHANGE
            | Op::ATOMIC_I_INCREMENT
            | Op::ATOMIC_I_DECREMENT
            | Op::ATOMIC_I_ADD
            | Op::ATOMIC_I_SUB
            | Op::ATOMIC_S_MIN
            | Op::ATOMIC_U_MIN
            | Op::ATOMIC_S_MAX
            | Op::ATOMIC_U_MAX
            | Op::ATOMIC_AND
            | Op::ATOMIC_OR
            | Op::ATOMIC_XOR
    )
}

/// Where the Boolean vector of `count` components is kept.
const fn vector_slot(count: u32) -> Result<usize, GuardError> {
    match count {
        2 => Ok(0),
        3 => Ok(1),
        4 => Ok(2),
        _ => Err(GuardError::Unguarded),
    }
}

fn push<T>(list: &mut Vec<T>, item: T) -> Result<(), GuardError> {
    reserve(list, 1)?;
    list.push(item);
    Ok(())
}

fn reserve<T>(list: &mut Vec<T>, additional: usize) -> Result<(), GuardError> {
    list.try_reserve(additional)
        .map_err(|_| GuardError::OutOfMemory)
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;

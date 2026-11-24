//! Folding (`plans/SHADER.md` SH8): an admitted module as a device that
//! compiles modules of its own is given it — its specialization evaluated
//! into it, so no device evaluates one a second way, and every value it could
//! read before writing defined as zero, as on the software device, so no
//! device reads memory another program left.
//!
//! Every specialization constant becomes the constant it was evaluated to,
//! with the id it had, and its `SpecId` goes with it; a composite whose
//! constituents the evaluation made has them declared first, under fresh ids.
//! An `OpUndef` becomes its type's null, and a private, workgroup or function
//! variable with no initializer is given its type's null. Nothing else
//! changes, and the module is admitted again: the fold changes no meaning, so
//! a refusal is the fold's fault.

use alloc::vec::Vec;

use super::specialize::{Constant, ConstantId, Specialized};
use super::view::{Scalar, Type};
use super::{Admitted, Refusal};
use crate::encode::{Arg, Encoder};
use crate::enumerant::{Decoration, StorageClass};
use crate::{EncodeError, Id, Instruction, Module, Op, Value};

/// Why a module could not be folded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FoldError {
    /// The memory folding needs could not be had.
    OutOfMemory,
    /// The folded module would pass the largest module, an instruction's
    /// words, or the ids a module holds.
    TooLarge,
    /// The specialization is not of this module.
    Malformed,
    /// The validator refused the folded module.
    Refused(Refusal),
}

impl From<EncodeError> for FoldError {
    fn from(error: EncodeError) -> Self {
        match error {
            EncodeError::OutOfMemory => Self::OutOfMemory,
            EncodeError::TooLarge | EncodeError::TooLong | EncodeError::IdsExhausted => {
                Self::TooLarge
            }
            EncodeError::IdNotIssued | EncodeError::InteriorNul => Self::Malformed,
        }
    }
}

impl Admitted {
    /// This module with `specialized`, a specialization of it, evaluated
    /// into it, admitted again.
    ///
    /// # Errors
    ///
    /// [`FoldError::Malformed`] for a specialization of another module,
    /// [`FoldError::TooLarge`] for a folded module past a module's bounds,
    /// [`FoldError::OutOfMemory`] for memory the host refuses, and
    /// [`FoldError::Refused`] for a folded module the validator refuses.
    pub fn fold(&self, specialized: &Specialized) -> Result<Self, FoldError> {
        let mut folder = Folder {
            admitted: self,
            specialized,
            out: Encoder::rewriting(self.module())?,
            defined: Vec::new(),
            nulls: Vec::new(),
            pending: Vec::new(),
            in_functions: false,
        };
        folder
            .defined
            .try_reserve_exact(specialized.constants())
            .map_err(|_| FoldError::OutOfMemory)?;
        folder.defined.resize(specialized.constants(), None);
        for instruction in self.module().instructions() {
            folder.instruction(instruction)?;
        }
        let module = Module::decode(&folder.out.finish()).map_err(|_| FoldError::Malformed)?;
        Self::new(module).map_err(FoldError::Refused)
    }
}

/// A fold under way.
struct Folder<'a> {
    admitted: &'a Admitted,
    specialized: &'a Specialized,
    out: Encoder,
    /// Each evaluated constant's id in the folded module, and its type's,
    /// once it has one.
    defined: Vec<Option<(Id, Id)>>,
    /// Each type's null constant that a composite needed, by the type's id.
    nulls: Vec<(Id, Id)>,
    /// Constituents waiting on their own, the next to look at last.
    pending: Vec<(ConstantId, Id)>,
    /// Whether the functions have begun, past where a constant may be
    /// declared.
    in_functions: bool,
}

impl<'a> Folder<'a> {
    fn instruction(&mut self, instruction: &Instruction) -> Result<(), FoldError> {
        let module = self.admitted.module();
        match instruction.op() {
            Op::DECORATE if names_spec_id(module, instruction) => Ok(()),
            Op::FUNCTION if !self.in_functions => {
                self.in_functions = true;
                self.function_nulls()?;
                Ok(self.out.copy(module, instruction)?)
            }
            Op::UNDEF => self.undef(instruction),
            Op::VARIABLE => self.variable(instruction),
            Op::SPEC_CONSTANT_TRUE
            | Op::SPEC_CONSTANT_FALSE
            | Op::SPEC_CONSTANT
            | Op::SPEC_CONSTANT_COMPOSITE
            | Op::SPEC_CONSTANT_OP => {
                let (id, ty, constant) = self.declared(instruction)?;
                self.constituents(constant, ty)?;
                self.define(constant, ty, id)?;
                self.note(constant, ty, id)
            }
            Op::CONSTANT_TRUE | Op::CONSTANT_FALSE | Op::CONSTANT | Op::CONSTANT_COMPOSITE => {
                self.out.copy(module, instruction)?;
                let (id, ty, constant) = self.declared(instruction)?;
                self.note(constant, ty, id)
            }
            _ => Ok(self.out.copy(module, instruction)?),
        }
    }

    /// An `OpUndef` as its type's null: declared as it, among the constants,
    /// or copied from it in a function; one of a type with no null as it is.
    fn undef(&mut self, instruction: &Instruction) -> Result<(), FoldError> {
        let module = self.admitted.module();
        let (Some(id), Some(ty)) = (module.result(instruction), module.result_type(instruction))
        else {
            return Err(FoldError::Malformed);
        };
        if !self.nullable(ty)? {
            return Ok(self.out.copy(module, instruction)?);
        }
        if self.in_functions {
            let null = self.null_of(ty).ok_or(FoldError::Malformed)?;
            self.out
                .emit(Op::COPY_OBJECT, &[Arg::Id(ty), Arg::Id(id), Arg::Id(null)])?;
        } else {
            self.out
                .emit(Op::CONSTANT_NULL, &[Arg::Id(ty), Arg::Id(id)])?;
            self.remember_null(ty, id)?;
        }
        Ok(())
    }

    /// A variable, given its type's null where it has no initializer and
    /// is private, workgroup or a function's own.
    fn variable(&mut self, instruction: &Instruction) -> Result<(), FoldError> {
        let module = self.admitted.module();
        let operands = module.operands(instruction);
        let class = match operands.get(2).map(|o| module.value(o)) {
            Some(Value::Word(class)) => StorageClass(class),
            _ => return Err(FoldError::Malformed),
        };
        let held = matches!(
            class,
            StorageClass::PRIVATE | StorageClass::WORKGROUP | StorageClass::FUNCTION
        );
        let pointee = self.pointee(instruction)?;
        if operands.len() > 3 || !held || !self.nullable(pointee)? {
            return Ok(self.out.copy(module, instruction)?);
        }
        let null = match self.null_of(pointee) {
            Some(null) => null,
            None if !self.in_functions => self.declare_null(pointee)?,
            None => return Err(FoldError::Malformed),
        };
        let (Some(id), Some(ty)) = (module.result(instruction), module.result_type(instruction))
        else {
            return Err(FoldError::Malformed);
        };
        self.out.emit(
            Op::VARIABLE,
            &[Arg::Id(ty), Arg::Id(id), Arg::Word(class.0), Arg::Id(null)],
        )?;
        Ok(())
    }

    /// The type a variable's pointer type points to.
    fn pointee(&self, variable: &Instruction) -> Result<Id, FoldError> {
        let admitted = self.admitted;
        let pointer = admitted
            .module()
            .result_type(variable)
            .and_then(|ty| admitted.type_id(ty))
            .ok_or(FoldError::Malformed)?;
        let Some(Type::Pointer { pointee, .. }) = admitted.ty(pointer) else {
            return Err(FoldError::Malformed);
        };
        self.declaration(pointee)
    }

    /// Declare, ahead of the functions, the null of every type a function
    /// needs one of: an `OpUndef`'s, and a local variable's with no
    /// initializer.
    fn function_nulls(&mut self) -> Result<(), FoldError> {
        let module = self.admitted.module();
        for instruction in module
            .instructions()
            .iter()
            .skip_while(|instruction| instruction.op() != Op::FUNCTION)
        {
            let ty = match instruction.op() {
                Op::UNDEF => module.result_type(instruction),
                Op::VARIABLE if module.operands(instruction).len() == 3 => {
                    Some(self.pointee(instruction)?)
                }
                _ => None,
            };
            if let Some(ty) = ty {
                if self.null_of(ty).is_none() && self.nullable(ty)? {
                    self.declare_null(ty)?;
                }
            }
        }
        Ok(())
    }

    /// Whether a value of `ty` has a null: data, never a pointer or an
    /// opaque handle.
    fn nullable(&self, ty: Id) -> Result<bool, FoldError> {
        let of = self.admitted.type_id(ty).ok_or(FoldError::Malformed)?;
        Ok(matches!(
            self.admitted.ty(of).ok_or(FoldError::Malformed)?,
            Type::Scalar(_)
                | Type::Vector { .. }
                | Type::Matrix { .. }
                | Type::Array { .. }
                | Type::Struct { .. }
        ))
    }

    /// The null of `ty` declared so far.
    fn null_of(&self, ty: Id) -> Option<Id> {
        let at = self.nulls.binary_search_by_key(&ty, |&(of, _)| of).ok()?;
        self.nulls.get(at).map(|&(_, id)| id)
    }

    /// Declare the null of `ty` under a fresh id.
    fn declare_null(&mut self, ty: Id) -> Result<Id, FoldError> {
        let id = self.out.next_id()?;
        self.out
            .emit(Op::CONSTANT_NULL, &[Arg::Id(ty), Arg::Id(id)])?;
        self.remember_null(ty, id)?;
        Ok(id)
    }

    /// Remember `id` as the null of `ty`, where none is yet.
    fn remember_null(&mut self, ty: Id, id: Id) -> Result<(), FoldError> {
        if let Err(at) = self.nulls.binary_search_by_key(&ty, |&(of, _)| of) {
            self.nulls
                .try_reserve(1)
                .map_err(|_| FoldError::OutOfMemory)?;
            self.nulls.insert(at, (ty, id));
        }
        Ok(())
    }

    /// The id declaring the type `ty`.
    fn declaration(&self, ty: crate::TypeId) -> Result<Id, FoldError> {
        self.admitted
            .kept
            .types
            .get(ty.index() as usize)
            .and_then(|info| info.id)
            .ok_or(FoldError::Malformed)
    }

    /// The id, type and evaluated constant `instruction` declares.
    fn declared(&self, instruction: &Instruction) -> Result<(Id, Id, ConstantId), FoldError> {
        let module = self.admitted.module();
        let id = module.result(instruction).ok_or(FoldError::Malformed)?;
        let ty = module
            .result_type(instruction)
            .ok_or(FoldError::Malformed)?;
        let constant = self
            .specialized
            .constant(self.admitted, id)
            .ok_or(FoldError::Malformed)?;
        Ok((id, ty, constant))
    }

    fn value(&self, constant: ConstantId) -> Result<Constant<'a>, FoldError> {
        self.specialized.value(constant).ok_or(FoldError::Malformed)
    }

    /// The id in the folded module holding `constant` as a value of `ty`,
    /// once it has one.
    fn known(&self, constant: ConstantId, ty: Id) -> Result<Option<Id>, FoldError> {
        Ok(match self.value(constant)? {
            Constant::Null => self.null_of(ty),
            _ => self
                .defined
                .get(constant.index())
                .copied()
                .flatten()
                .filter(|&(_, of)| of == ty)
                .map(|(id, _)| id),
        })
    }

    /// Remember that `id` holds `constant` as a value of `ty`.
    fn note(&mut self, constant: ConstantId, ty: Id, id: Id) -> Result<(), FoldError> {
        if matches!(self.value(constant)?, Constant::Null) {
            self.remember_null(ty, id)?;
        } else if let Some(slot) = self.defined.get_mut(constant.index()) {
            slot.get_or_insert((id, ty));
        }
        Ok(())
    }

    /// Declare, under fresh ids, every constituent of `constant`, a value of
    /// `ty`, the folded module has no id for, each after its own.
    fn constituents(&mut self, constant: ConstantId, ty: Id) -> Result<(), FoldError> {
        self.pending.clear();
        self.unknown_parts(constant, ty)?;
        while let Some(&(part, part_ty)) = self.pending.last() {
            if self.known(part, part_ty)?.is_some() {
                self.pending.pop();
                continue;
            }
            if self.unknown_parts(part, part_ty)? > 0 {
                continue;
            }
            self.pending.pop();
            let id = self.out.next_id()?;
            self.define(part, part_ty, id)?;
            self.note(part, part_ty, id)?;
        }
        Ok(())
    }

    /// Queue each constituent of `constant`, a value of `ty`, the folded
    /// module has no id for, answering how many.
    fn unknown_parts(&mut self, constant: ConstantId, ty: Id) -> Result<usize, FoldError> {
        let Constant::Composite(parts) = self.value(constant)? else {
            return Ok(0);
        };
        let mut queued = 0;
        for (index, &part) in (0u32..).zip(parts) {
            let part_ty = self.constituent_type(ty, index)?;
            if self.known(part, part_ty)?.is_none() {
                self.pending
                    .try_reserve(1)
                    .map_err(|_| FoldError::OutOfMemory)?;
                self.pending.push((part, part_ty));
                queued += 1;
            }
        }
        Ok(queued)
    }

    /// The type of constituent `index` of a composite of `ty`.
    fn constituent_type(&self, ty: Id, index: u32) -> Result<Id, FoldError> {
        let admitted = self.admitted;
        let of = admitted.type_id(ty).ok_or(FoldError::Malformed)?;
        let inner = match admitted.ty(of).ok_or(FoldError::Malformed)? {
            Type::Vector { .. } => {
                let module = admitted.module();
                let vector = module.definition(ty).ok_or(FoldError::Malformed)?;
                return match module.operands(vector).get(1).map(|o| module.value(o)) {
                    Some(Value::Id(component)) => Ok(component),
                    _ => Err(FoldError::Malformed),
                };
            }
            Type::Matrix { column, .. } => column,
            Type::Array { element, .. } => element,
            Type::Struct { .. } => admitted
                .member_type(of, index)
                .ok_or(FoldError::Malformed)?,
            _ => return Err(FoldError::Malformed),
        };
        self.declaration(inner)
    }

    /// Declare `constant`, a value of `ty`, as `id`, its constituents known.
    fn define(&mut self, constant: ConstantId, ty: Id, id: Id) -> Result<(), FoldError> {
        let declared = [Arg::Id(ty), Arg::Id(id)];
        match self.value(constant)? {
            Constant::Null => self.out.emit(Op::CONSTANT_NULL, &declared)?,
            Constant::Scalar(bits) => {
                let of = self.admitted.type_id(ty).ok_or(FoldError::Malformed)?;
                let Some(Type::Scalar(scalar)) = self.admitted.ty(of) else {
                    return Err(FoldError::Malformed);
                };
                match literal(scalar, bits) {
                    None => {
                        let op = if bits == 0 {
                            Op::CONSTANT_FALSE
                        } else {
                            Op::CONSTANT_TRUE
                        };
                        self.out.emit(op, &declared)?;
                    }
                    Some(value) => self
                        .out
                        .emit(Op::CONSTANT, &[declared[0], declared[1], value])?,
                }
            }
            Constant::Composite(parts) => {
                let mut args = Vec::new();
                args.try_reserve_exact(parts.len().saturating_add(2))
                    .map_err(|_| FoldError::OutOfMemory)?;
                args.extend(declared);
                for (index, &part) in (0u32..).zip(parts) {
                    let part_ty = self.constituent_type(ty, index)?;
                    let held = self.known(part, part_ty)?.ok_or(FoldError::Malformed)?;
                    args.push(Arg::Id(held));
                }
                self.out.emit(Op::CONSTANT_COMPOSITE, &args)?;
            }
        }
        Ok(())
    }
}

/// A scalar's literal operand, `None` for a Boolean: as wide as its type, a
/// signed integer narrower than a word sign-extended into it.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a literal a word wide is its value's low word"
)]
fn literal(scalar: Scalar, bits: u64) -> Option<Arg<'static>> {
    let (width, signed) = match scalar {
        Scalar::Bool => return None,
        Scalar::Int { width, signed } => (width, signed),
        Scalar::Float { width } => (width, false),
    };
    Some(if width > 32 {
        Arg::Wide(bits)
    } else if signed && width < 32 {
        let shift = 64 - width;
        let extended = ((bits << shift).cast_signed() >> shift).cast_unsigned();
        Arg::Word(extended as u32)
    } else {
        Arg::Word(bits as u32)
    })
}

/// Whether `instruction`, an `OpDecorate`, decorates with `SpecId`.
fn names_spec_id(module: &Module, instruction: &Instruction) -> bool {
    matches!(
        module.operands(instruction).get(1).map(|o| module.value(o)),
        Some(Value::Word(decoration)) if decoration == Decoration::SPEC_ID.0
    )
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;

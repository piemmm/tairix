//! Places: a memory view as the whole object it lies within — a variable,
//! or a pointer parameter naming one — and the indexes from it, so a
//! pointer is only ever made where it is used; loads and stores through
//! one, a value rebuilt where its memory holds its type in another form.

use alloc::vec::Vec;

use tairix_spirv::enumerant::StorageClass;
use tairix_spirv::{Id, Op};

use super::function::Writer;
use super::{built, Flavour, Lower};
use crate::diag::{reserve, Outcome, Stop};
use crate::eval::Values;
use crate::types::{Count, Space, Ty, TyId};

/// What a place's memory is reached from: a module-scope variable, or an
/// object of `ty` in `space` a variant is given a pointer to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Root {
    Global(u32),
    Object { ty: TyId, space: Space },
}

/// One index of a place: a constant one, or a `u32` value.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Index {
    Const(u32),
    Dynamic(Id),
}

/// A memory view.
#[derive(Debug)]
pub(crate) struct Place {
    /// A pointer to the whole object.
    pub(crate) base: Id,
    pub(crate) root: Root,
    pub(crate) space: Space,
    /// Whether a block wraps the object, its first index then stepping in.
    pub(crate) wrapped: bool,
    /// The indexes from the object to the memory viewed.
    pub(crate) indexes: Vec<Index>,
    /// The type of the memory viewed.
    pub(crate) ty: TyId,
}

/// The largest array rebuilt element by element in line; a larger one is
/// rebuilt by a loop through memory.
const UNROLLED: u32 = 64;

impl Place {
    /// The place `index` steps to, of type `ty`.
    pub(crate) fn step(&self, index: Index, ty: TyId) -> Outcome<Self> {
        let mut next = self.with_room(1)?;
        next.indexes.push(index);
        next.ty = ty;
        Ok(next)
    }

    /// The same view.
    pub(crate) fn duplicate(&self) -> Outcome<Self> {
        self.with_room(0)
    }

    fn with_room(&self, extra: usize) -> Outcome<Self> {
        let mut indexes = Vec::new();
        reserve(&mut indexes, self.indexes.len() + extra)?;
        indexes.extend_from_slice(&self.indexes);
        Ok(Self {
            base: self.base,
            root: self.root,
            space: self.space,
            wrapped: self.wrapped,
            indexes,
            ty: self.ty,
        })
    }
}

impl Writer<'_, '_, '_> {
    /// A pointer to the memory `place` views: its base where it views the
    /// whole of an unwrapped object, else an access chain.
    pub(crate) fn pointer_of(&mut self, place: &Place) -> Outcome<Id> {
        if place.indexes.is_empty() && !place.wrapped {
            return Ok(place.base);
        }
        let pointer = self.pointer_type(place.space, place.ty)?;
        let mut operands: Vec<Id> = Vec::new();
        reserve(&mut operands, place.indexes.len() + 2)?;
        operands.push(place.base);
        if place.wrapped {
            operands.push(self.lower().uint(0)?);
        }
        for &index in &place.indexes {
            operands.push(match index {
                Index::Const(n) => self.lower().uint(n)?,
                Index::Dynamic(id) => id,
            });
        }
        self.f
            .op(Op::ACCESS_CHAIN, pointer, &operands)
            .map_err(built)
    }

    /// The pointer type to memory of `ty` in `space`, in the form that
    /// space holds it.
    pub(crate) fn pointer_type(&mut self, space: Space, ty: TyId) -> Outcome<Id> {
        let mut lower = self.lower();
        let flavour = lower.memory_flavour(space, ty)?;
        let pointee = lower.ty(ty, flavour)?;
        lower.pointer(Lower::class(space), pointee)
    }

    /// The value `place` holds, in its values' form.
    pub(crate) fn load(&mut self, place: &Place) -> Outcome<Id> {
        let pointer = self.pointer_of(place)?;
        let (memory, value) = {
            let mut lower = self.lower();
            (
                lower.memory_flavour(place.space, place.ty)?,
                lower.value_flavour(place.ty)?,
            )
        };
        let ty = self.lower().ty(place.ty, memory)?;
        let loaded = self.f.op(Op::LOAD, ty, &[pointer]).map_err(built)?;
        self.reflavour(loaded, place.ty, memory, value)
    }

    /// Store `value`, in its values' form, to `place`: a store to storage
    /// memory masked once the invocation has discarded.
    pub(crate) fn store(&mut self, place: &Place, value: Id) -> Outcome<()> {
        let (memory, from) = {
            let mut lower = self.lower();
            (
                lower.memory_flavour(place.space, place.ty)?,
                lower.value_flavour(place.ty)?,
            )
        };
        let value = self.reflavour(value, place.ty, from, memory)?;
        let pointer = self.pointer_of(place)?;
        self.unless_discarded(place.space == Space::Storage, |w| {
            w.f.emit(Op::STORE, &[pointer, value]).map_err(built)
        })
    }

    /// `value`, of type `ty` in `from`'s form, rebuilt in `to`'s: member by
    /// member, and element by element, through memory for a long array.
    pub(crate) fn reflavour(
        &mut self,
        value: Id,
        ty: TyId,
        from: Flavour,
        to: Flavour,
    ) -> Outcome<Id> {
        let (from, to) = {
            let mut lower = self.lower();
            (lower.canonical(ty, from)?, lower.canonical(ty, to)?)
        };
        if from == to {
            return Ok(value);
        }
        let target = self.lower().ty(ty, to)?;
        match self.c.types.get(ty) {
            Ty::Struct(structure) => {
                let count = self.c.types.members_of(structure).len();
                let mut parts = Vec::new();
                reserve(&mut parts, count)?;
                for index in 0..count {
                    let member = self.c.types.members_of(structure)[index].ty;
                    let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
                    let part = self.extract(value, (member, from), &[index])?;
                    parts.push(self.reflavour(part, member, from, to)?);
                }
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, target, &parts)
                    .map_err(built)
            }
            Ty::Array { element, count } => {
                let n = self.c.count_value(count).ok_or(Stop::Internal)?;
                if n > UNROLLED || matches!(count, Count::Runtime) {
                    return self.reflavour_through_memory(value, (element, n), (ty, from, to));
                }
                let mut parts = Vec::new();
                reserve(&mut parts, n as usize)?;
                for index in 0..n {
                    let part = self.extract(value, (element, from), &[index])?;
                    parts.push(self.reflavour(part, element, from, to)?);
                }
                self.f
                    .op(Op::COMPOSITE_CONSTRUCT, target, &parts)
                    .map_err(built)
            }
            _ => Ok(value),
        }
    }

    /// Part `indexes` of `value`, of type `ty` in `flavour`'s form.
    pub(crate) fn extract(
        &mut self,
        value: Id,
        (ty, flavour): (TyId, Flavour),
        indexes: &[u32],
    ) -> Outcome<Id> {
        let part_ty = self.lower().ty(ty, flavour)?;
        let mut args: Vec<tairix_spirv::Arg<'_>> = Vec::new();
        reserve(&mut args, indexes.len() + 1)?;
        args.push(tairix_spirv::Arg::Id(value));
        args.extend(indexes.iter().map(|&i| tairix_spirv::Arg::Word(i)));
        self.f
            .op_args(Op::COMPOSITE_EXTRACT, part_ty, &args)
            .map_err(built)
    }

    /// A long array rebuilt by a loop: stored whole in one form, copied
    /// element by element, rebuilt, into the other, and read back.
    fn reflavour_through_memory(
        &mut self,
        value: Id,
        (element, n): (TyId, u32),
        (ty, from, to): (TyId, Flavour, Flavour),
    ) -> Outcome<Id> {
        let source_ty = self.lower().ty(ty, from)?;
        let target_ty = self.lower().ty(ty, to)?;
        let source_pointer = self.lower().pointer(StorageClass::FUNCTION, source_ty)?;
        let target_pointer = self.lower().pointer(StorageClass::FUNCTION, target_ty)?;
        let source = self.f.local(source_pointer).map_err(built)?;
        let target = self.f.local(target_pointer).map_err(built)?;
        self.f.emit(Op::STORE, &[source, value]).map_err(built)?;
        let from_element = self.lower().ty(element, from)?;
        let to_element = self.lower().ty(element, to)?;
        let from_pointer = self.lower().pointer(StorageClass::FUNCTION, from_element)?;
        let to_pointer = self.lower().pointer(StorageClass::FUNCTION, to_element)?;
        let (zero, limit, one) = (
            self.lower().uint(0)?,
            self.lower().uint(n)?,
            self.lower().uint(1)?,
        );
        self.count((zero, limit, one), |w, index| {
            let at =
                w.f.op(Op::ACCESS_CHAIN, from_pointer, &[source, index])
                    .map_err(built)?;
            let part = w.f.op(Op::LOAD, from_element, &[at]).map_err(built)?;
            let rebuilt = w.reflavour(part, element, from, to)?;
            let into =
                w.f.op(Op::ACCESS_CHAIN, to_pointer, &[target, index])
                    .map_err(built)?;
            w.f.emit(Op::STORE, &[into, rebuilt]).map_err(built)
        })?;
        self.f.op(Op::LOAD, target_ty, &[target]).map_err(built)
    }

    /// The type of component `index` of a value of `ty`.
    pub(crate) fn component_type(&mut self, ty: TyId, index: u32) -> Outcome<TyId> {
        Values::component_type(&mut self.c.types, ty, index)
    }
}

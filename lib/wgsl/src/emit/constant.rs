//! Constants: a const- or override-expression's value written as the
//! module's constant of its type, each interned by the builder.

use alloc::vec::Vec;

use tairix_spirv::Id;

use super::{built, Lower};
use crate::diag::{reserve, Outcome, Stop};
use crate::eval::{Value, ValueId, Values};
use crate::types::TyId;

impl Lower<'_, '_> {
    /// The constant holding `value`, of type `ty`, in its values' form.
    pub(crate) fn constant(&mut self, value: ValueId, ty: TyId) -> Outcome<Id> {
        let spirv = self.ty(ty, super::Flavour::Value)?;
        match self.c.values.get(value) {
            Value::Bool(b) => self.b.constant_bool(spirv, b).map_err(built),
            Value::I32(n) => self.b.constant(spirv, n.cast_unsigned()).map_err(built),
            Value::U32(n) => self.b.constant(spirv, n).map_err(built),
            Value::F32(x) => self.b.constant(spirv, x.to_bits()).map_err(built),
            Value::F16(bits) => self.b.constant(spirv, u32::from(bits)).map_err(built),
            Value::Zero(_) => self.b.constant_null(spirv).map_err(built),
            Value::Composite { .. } => {
                let mut parts = Vec::new();
                self.c
                    .values
                    .components(&mut self.c.types, (value, ty), &mut parts)?;
                let mut ids = Vec::new();
                reserve(&mut ids, parts.len())?;
                for (index, &part) in parts.iter().enumerate() {
                    let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
                    let part_ty = Values::component_type(&mut self.c.types, ty, index)?;
                    ids.push(self.constant(part, part_ty)?);
                }
                self.b.constant_composite(spirv, &ids).map_err(built)
            }
            Value::AbstractInt(_) | Value::AbstractFloat(_) => Err(Stop::Internal),
        }
    }

    /// The `u32` constant `n`.
    pub(crate) fn uint(&mut self, n: u32) -> Outcome<Id> {
        self.b.uint(n).map_err(built)
    }

    /// The `bool` constant `b`.
    pub(crate) fn boolean(&mut self, b: bool) -> Outcome<Id> {
        let ty = self.b.boolean().map_err(built)?;
        self.b.constant_bool(ty, b).map_err(built)
    }
}

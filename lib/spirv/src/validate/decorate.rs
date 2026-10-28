//! What each decoration may decorate, checked once every id's kind and type
//! is known: the target and member it names, the class of the memory it
//! qualifies, the specialization ids unique, and no two decorations that
//! exclude each other on one target.

use alloc::vec::Vec;

use super::declare::Deco;
use super::types::{Holds, Scalar, Ty};
use super::{push, Refusal, Rule, Validator, What, NONE};
use crate::enumerant::{Decoration, StorageClass};
use crate::{Op, Version};

impl Validator<'_> {
    /// Check every decoration's target.
    pub(crate) fn decorations_checked(&self) -> Result<(), Refusal> {
        let mut spec_ids: Vec<u32> = Vec::new();
        for deco in &self.decorations {
            let instruction = self.instruction(deco.at);
            let refused = Rule::DecorationTarget(deco.decoration).at(instruction);
            let ok = if deco.member == NONE {
                self.whole(deco).map_err(|r| r.at(instruction))?
            } else {
                self.member(deco).map_err(|r| r.at(instruction))?
            };
            if !ok {
                return Err(refused);
            }
            if deco.decoration == Decoration::SPEC_ID {
                push(&mut spec_ids, self.parameter(deco, 0))?;
            }
        }
        spec_ids.sort_unstable();
        if spec_ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(Rule::DuplicateDecoration(Decoration::SPEC_ID).module());
        }
        for group in self
            .decorations
            .chunk_by(|a, b| (a.target, a.member) == (b.target, b.member))
        {
            let has = |d: Decoration| group.iter().any(|g| g.decoration == d);
            for (a, b) in [
                (Decoration::ROW_MAJOR, Decoration::COL_MAJOR),
                (Decoration::FLAT, Decoration::NO_PERSPECTIVE),
                (Decoration::RESTRICT, Decoration::ALIASED),
                (Decoration::BUILT_IN, Decoration::LOCATION),
                (Decoration::BUILT_IN, Decoration::COMPONENT),
            ] {
                if has(a) && has(b) {
                    let at = group.first().map_or(NONE, |g| g.at);
                    return Err(Rule::ConflictingDecorations(b).at(self.instruction(at)));
                }
            }
        }
        Ok(())
    }

    /// Whether `deco`, on a whole id, decorates what it may.
    fn whole(&self, deco: &Deco) -> Result<bool, Rule> {
        let def = self.def(deco.target)?;
        let defining = self.defining(deco.target)?;
        let op = defining.op();
        let variable = def.what == What::Variable;
        let class = if variable {
            Some(self.class_of(deco.target)?)
        } else {
            None
        };
        let interface = matches!(class, Some(StorageClass::INPUT | StorageClass::OUTPUT));
        let pointee = match def.ty {
            NONE => None,
            ty => match self.ty(ty).ty {
                Ty::Pointer { pointee, .. } => Some(pointee),
                _ => None,
            },
        };
        Ok(match deco.decoration {
            Decoration::BLOCK => {
                def.what == What::Type && matches!(self.ty(def.ty).ty, Ty::Struct { .. })
            }
            Decoration::ARRAY_STRIDE => {
                def.what == What::Type
                    && matches!(
                        self.ty(def.ty).ty,
                        Ty::Array { .. } | Ty::RuntimeArray { .. }
                    )
            }
            Decoration::SPEC_ID => matches!(
                op,
                Op::SPEC_CONSTANT | Op::SPEC_CONSTANT_TRUE | Op::SPEC_CONSTANT_FALSE
            ),
            Decoration::BINDING | Decoration::DESCRIPTOR_SET => matches!(
                class,
                Some(
                    StorageClass::UNIFORM_CONSTANT
                        | StorageClass::UNIFORM
                        | StorageClass::STORAGE_BUFFER
                )
            ),
            Decoration::INDEX => class == Some(StorageClass::OUTPUT),
            Decoration::RESTRICT | Decoration::ALIASED => {
                (variable || def.what == What::Parameter) && pointee.is_some()
            }
            Decoration::UNIFORM | Decoration::UNIFORM_ID => def.what == What::Value,
            Decoration::FP_ROUNDING_MODE => {
                op == Op::F_CONVERT
                    && matches!(
                        self.components(def.ty),
                        Some((Scalar::Float { width: 16 }, _))
                    )
            }
            Decoration::NO_CONTRACTION => def.what == What::Value && arithmetic(op),
            Decoration::NO_SIGNED_WRAP => {
                matches!(
                    op,
                    Op::I_ADD | Op::I_SUB | Op::I_MUL | Op::SHIFT_LEFT_LOGICAL | Op::S_NEGATE
                )
            }
            Decoration::NO_UNSIGNED_WRAP => {
                matches!(
                    op,
                    Op::I_ADD | Op::I_SUB | Op::I_MUL | Op::SHIFT_LEFT_LOGICAL
                )
            }
            Decoration::BUILT_IN => {
                interface
                    || (matches!(def.what, What::Constant | What::SpecConstant)
                        && self.uvec3_like(def.ty)
                        && matches!(op, Op::CONSTANT_COMPOSITE | Op::SPEC_CONSTANT_COMPOSITE)
                        && Some(deco.target) == self.workgroup_constant()
                        && self.workgroup_values(deco.target))
            }
            Decoration::NO_PERSPECTIVE
            | Decoration::FLAT
            | Decoration::CENTROID
            | Decoration::INVARIANT
            | Decoration::LOCATION
            | Decoration::COMPONENT => interface,
            Decoration::VOLATILE
            | Decoration::COHERENT
            | Decoration::NON_WRITABLE
            | Decoration::NON_READABLE => self.qualifies(deco.decoration, def, pointee),
            Decoration::RELAXED_PRECISION => self.relaxed(def.what, def.ty),
            _ => false,
        })
    }

    /// Whether the memory qualifier `decoration` may decorate the variable or
    /// parameter `def`: one pointing to a storage buffer or storage image,
    /// or from 1.4 to private or function memory as `NonWritable`.
    fn qualifies(&self, decoration: Decoration, def: super::Def, pointee: Option<u32>) -> bool {
        let qualified_class = |class: StorageClass| match class {
            StorageClass::STORAGE_BUFFER => true,
            StorageClass::UNIFORM_CONSTANT => pointee.is_some_and(|p| self.storage_image(p)),
            StorageClass::PRIVATE | StorageClass::FUNCTION => {
                decoration == Decoration::NON_WRITABLE
                    && self.enabled.version >= (Version { major: 1, minor: 4 })
            }
            _ => false,
        };
        let pointer_class = match def.ty {
            NONE => None,
            ty => match self.ty(ty).ty {
                Ty::Pointer { class, .. } => Some(class),
                _ => None,
            },
        };
        matches!(def.what, What::Variable | What::Parameter)
            && pointer_class.is_some_and(qualified_class)
    }

    /// Whether `deco`, on a structure member, decorates what it may.
    fn member(&self, deco: &Deco) -> Result<bool, Rule> {
        let ty = self.type_index(deco.target)?;
        let Ty::Struct { first, count } = self.ty(ty).ty else {
            return Ok(false);
        };
        let Some(&member) = self.list(first, count).get(deco.member as usize) else {
            return Ok(false);
        };
        let matrix = matches!(self.ty(self.innermost(member)).ty, Ty::Matrix { .. });
        Ok(match deco.decoration {
            Decoration::OFFSET
            | Decoration::BUILT_IN
            | Decoration::NO_PERSPECTIVE
            | Decoration::FLAT
            | Decoration::CENTROID
            | Decoration::INVARIANT
            | Decoration::LOCATION
            | Decoration::COMPONENT
            | Decoration::VOLATILE
            | Decoration::COHERENT
            | Decoration::NON_WRITABLE
            | Decoration::NON_READABLE => true,
            Decoration::ROW_MAJOR | Decoration::COL_MAJOR | Decoration::MATRIX_STRIDE => matrix,
            Decoration::RELAXED_PRECISION => self.relaxed(What::Value, member),
            _ => false,
        })
    }

    /// Whether `ty` is, or is an array of, a storage image.
    fn storage_image(&self, ty: u32) -> bool {
        matches!(self.ty(self.innermost(ty)).ty, Ty::Image(i) if i.sampled == 2)
    }

    /// Whether `RelaxedPrecision` may decorate something that is `what` of
    /// type `ty`: a value or memory of 32-bit numbers, Booleans, or images,
    /// or a function returning one.
    fn relaxed(&self, what: What, ty: u32) -> bool {
        if what == What::Function {
            return true;
        }
        if !matches!(
            what,
            What::Value | What::Variable | What::Parameter | What::Constant
        ) || ty == NONE
        {
            return false;
        }
        let ty = match self.ty(ty).ty {
            Ty::Pointer { pointee, .. } => pointee,
            _ => ty,
        };
        let bottom = self.innermost(ty);
        let info = self.ty(bottom);
        match info.ty {
            Ty::Image(_) | Ty::SampledImage { .. } => true,
            _ => {
                self.scalar_of(bottom)
                    .is_some_and(|s| s.is_bool() || s.width() == 32)
                    && !info.holds.any(Holds::POINTER.with(Holds::OPAQUE))
            }
        }
    }

    /// Whether the `WorkgroupSize` constant, where its value is fixed, has
    /// no zero dimension.
    fn workgroup_values(&self, id: crate::Id) -> bool {
        let Ok(instruction) = self.defining(id) else {
            return false;
        };
        if instruction.op() != Op::CONSTANT_COMPOSITE {
            return true;
        }
        self.m
            .operands(instruction)
            .get(2..)
            .unwrap_or(&[])
            .iter()
            .all(|operand| match self.m.value(operand) {
                crate::Value::Id(dimension) => self
                    .integer_constant(dimension)
                    .is_some_and(|(v, _)| v != 0),
                _ => false,
            })
    }
}

/// Whether `op` is an arithmetic instruction `NoContraction` may decorate.
const fn arithmetic(op: Op) -> bool {
    matches!(
        op,
        Op::S_NEGATE
            | Op::F_NEGATE
            | Op::I_ADD
            | Op::F_ADD
            | Op::I_SUB
            | Op::F_SUB
            | Op::I_MUL
            | Op::F_MUL
            | Op::U_DIV
            | Op::S_DIV
            | Op::F_DIV
            | Op::U_MOD
            | Op::S_REM
            | Op::S_MOD
            | Op::F_REM
            | Op::F_MOD
            | Op::VECTOR_TIMES_SCALAR
            | Op::MATRIX_TIMES_SCALAR
            | Op::VECTOR_TIMES_MATRIX
            | Op::MATRIX_TIMES_VECTOR
            | Op::MATRIX_TIMES_MATRIX
            | Op::OUTER_PRODUCT
            | Op::DOT
            | Op::I_ADD_CARRY
            | Op::I_SUB_BORROW
            | Op::U_MUL_EXTENDED
            | Op::S_MUL_EXTENDED
    )
}

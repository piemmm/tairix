//! Module-scope variables, each written where a function first names it: a
//! buffer as a block — its store type wrapped, unless that type is a
//! structure ending in a runtime-sized array, which only a storage buffer
//! can hold and which is then the block itself — a resource bound at its
//! group and binding, a private variable initialised, and the flag
//! `discard` sets.

use tairix_spirv::enumerant::{Capability, Decoration, StorageClass};
use tairix_spirv::{Arg, Id, Op};

use super::{built, GlobalVar, Lower};
use crate::check::decl::DeclInfo;
use crate::diag::{Outcome, Stop};
use crate::types::{Access, Count, Holds, Scalar, Space, Texture, Ty, TyId};

impl Lower<'_, '_> {
    /// Module-scope variable `decl`, written once.
    pub(crate) fn global(&mut self, decl: u32) -> Outcome<GlobalVar> {
        if let Some(Some(written)) = self.t.globals.get(decl as usize) {
            return Ok(*written);
        }
        let Some(&DeclInfo::Global(global)) = self.c.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        let class = Self::class(global.space);
        let flavour = self.memory_flavour(global.space, global.store)?;
        let store = self.ty(global.store, flavour)?;
        let buffer = global.space.is_host_laid();
        let wrapped = buffer && !self.ends_in_runtime_array(global.store);
        let held = if wrapped {
            let block = self.b.structure(&[store]).map_err(built)?;
            self.b
                .member_decorate(block, 0, Decoration::OFFSET, &[0])
                .map_err(built)?;
            self.matrix_layout(block, 0, global.store)?;
            self.b
                .decorate(block, Decoration::BLOCK, &[])
                .map_err(built)?;
            block
        } else {
            if buffer {
                self.b
                    .decorate(store, Decoration::BLOCK, &[])
                    .map_err(built)?;
            }
            store
        };
        if buffer && self.holds_f16(global.store) {
            let capability = match global.space {
                Space::Uniform => Capability::UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS,
                Space::Storage => Capability::STORAGE_BUFFER16_BIT_ACCESS,
                _ => Capability::STORAGE_PUSH_CONSTANT16,
            };
            self.b.capability(capability).map_err(built)?;
        }
        let pointer = self.pointer(class, held)?;
        let id = match (global.space, global.init) {
            (Space::Private, init) => {
                let value = match init {
                    Some(init) => {
                        let info = self.c.info(init);
                        let value = info.value_id().ok_or(Stop::Internal)?;
                        self.constant(value, info.converted)?
                    }
                    None => self.b.constant_null(store).map_err(built)?,
                };
                self.b
                    .declare(
                        Op::VARIABLE,
                        Some(pointer),
                        &[Arg::Word(class.0), Arg::Id(value)],
                    )
                    .map_err(built)?
            }
            _ => self.b.variable(pointer, class).map_err(built)?,
        };
        if let Some((group, binding)) = global.binding {
            self.b
                .decorate(id, Decoration::DESCRIPTOR_SET, &[group])
                .map_err(built)?;
            self.b
                .decorate(id, Decoration::BINDING, &[binding])
                .map_err(built)?;
        }
        self.qualify(id, global.space, global.access, global.store)?;
        let written = GlobalVar { id, wrapped };
        *crate::diag::slot(&mut self.t.globals, decl as usize, None)? = Some(written);
        Ok(written)
    }

    /// The access a storage buffer or storage texture is not given, stated
    /// on its variable: a read-only one is not written, a write-only one
    /// not read.
    fn qualify(&mut self, variable: Id, space: Space, access: Access, store: TyId) -> Outcome<()> {
        let access = match (space, self.c.types.get(store)) {
            (Space::Storage, _) => access,
            (Space::Handle, Ty::Texture(Texture::Storage { access, .. })) => access,
            _ => return Ok(()),
        };
        if !access.writes() {
            self.b
                .decorate(variable, Decoration::NON_WRITABLE, &[])
                .map_err(built)?;
        }
        if !access.reads() {
            self.b
                .decorate(variable, Decoration::NON_READABLE, &[])
                .map_err(built)?;
        }
        Ok(())
    }

    /// Whether `ty` is a structure whose last member is a runtime-sized
    /// array.
    fn ends_in_runtime_array(&self, ty: TyId) -> bool {
        let Ty::Struct(structure) = self.c.types.get(ty) else {
            return false;
        };
        self.c
            .types
            .members_of(structure)
            .last()
            .is_some_and(|member| {
                matches!(
                    self.c.types.get(member.ty),
                    Ty::Array {
                        count: Count::Runtime,
                        ..
                    }
                )
            })
    }

    /// Whether `ty` holds an `f16`, which a buffer or the stage interface
    /// holds only under a 16-bit storage capability.
    pub(crate) fn holds_f16(&self, ty: TyId) -> bool {
        self.c.types.holds(ty, Holds::F16)
    }

    /// The flag a `discard` sets, written once: private, initially false.
    pub(crate) fn discard_flag(&mut self) -> Outcome<Id> {
        if let Some(flag) = self.t.discarded {
            return Ok(flag);
        }
        let bool_ty = self.scalar(Scalar::Bool)?;
        let pointer = self.pointer(StorageClass::PRIVATE, bool_ty)?;
        let no = self.boolean(false)?;
        let flag = self
            .b
            .declare(
                Op::VARIABLE,
                Some(pointer),
                &[Arg::Word(StorageClass::PRIVATE.0), Arg::Id(no)],
            )
            .map_err(built)?;
        self.t.discarded = Some(flag);
        Ok(flag)
    }
}

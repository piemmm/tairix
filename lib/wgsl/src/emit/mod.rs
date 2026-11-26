//! The module: SPIR-V 1.3 written through `lib/spirv`'s builder for every
//! declaration, function and entry point an entry point reaches, and handed
//! back only once the builder's validator admits it (`plans/SHADER.md`
//! SH5.3).
//!
//! A value of a type some buffer holds takes that type's laid-out form, as
//! does function and private memory of it, so a buffer is read and written
//! with no conversion; workgroup memory, which takes no explicit layout,
//! holds the plain form, and a value crossing into or out of it is rebuilt.
//! A function taking a pointer is written once for each root and access
//! shape its callers pass, so every pointer a call carries is a whole memory
//! object, as logical addressing requires.

mod builtin;
mod constant;
mod entry;
mod expr;
mod function;
mod global;
mod place;
mod types;

use alloc::vec::Vec;

use tairix_collections::OrderedMap;
use tairix_spirv::enumerant::{Capability, StorageClass};
use tairix_spirv::{BuildError, Builder, Id, Version};

use crate::check::Checker;
use crate::diag::{error, Code, Outcome, Span, Stop};
use crate::types::{Space, TyId};
use crate::MAX_OUTPUT_BYTES;

pub(crate) use place::{Index, Place, Root};

/// The most variants of functions a module is written with: each function
/// once per shape of pointer its callers pass.
pub(crate) const MAX_VARIANTS: usize = 4096;

/// The form a type is written in: laid out as a buffer holds it, plain as
/// workgroup memory does, or as values hold it — a buffer's types laid out,
/// any other plain but for the members some buffer holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Flavour {
    Plain,
    Laid,
    Value,
}

/// How a pointer argument reaches its memory, for a variant's key: the
/// root object and the steps taken into it — a structure member's index,
/// or an element whose index the variant takes as a value.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Shape {
    pub(crate) root: Root,
    pub(crate) steps: Vec<Step>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Step {
    Member(u32),
    Element,
}

impl Shape {
    pub(crate) fn duplicate(&self) -> Outcome<Self> {
        let mut steps = Vec::new();
        crate::diag::reserve(&mut steps, self.steps.len())?;
        steps.extend_from_slice(&self.steps);
        Ok(Self {
            root: self.root,
            steps,
        })
    }
}

/// A function variant: its declaration and the shape of each pointer
/// parameter, in order.
pub(crate) type VariantKey = (u32, Vec<Shape>);

/// A module-scope variable as written: its variable, and whether a block
/// wraps its store type.
#[derive(Copy, Clone, Debug)]
pub(crate) struct GlobalVar {
    pub(crate) id: Id,
    pub(crate) wrapped: bool,
}

/// The emitter's state over one checked module: the checker, the tables
/// the module is written from, and the builder.
pub(crate) struct Emitter<'e, 'a> {
    pub(crate) c: &'e mut Checker<'a>,
    pub(crate) t: Tables,
    pub(crate) b: Builder,
}

/// A view of the checker, the tables and a builder that types, constants
/// and variables are written through, from module scope or from within a
/// function's body alike.
pub(crate) struct Lower<'x, 'a> {
    pub(crate) c: &'x mut Checker<'a>,
    pub(crate) t: &'x mut Tables,
    pub(crate) b: &'x mut Builder,
}

/// What the module is written from, besides the checker.
#[derive(Default)]
pub(crate) struct Tables {
    /// Each type written, in each form.
    pub(crate) types: OrderedMap<(TyId, Flavour), Id>,
    /// Whether some buffer holds each type, so its values are laid out.
    pub(crate) buffered: Vec<bool>,
    /// Whether each type holds a type some buffer holds: 0 not yet known,
    /// 1 no, 2 yes.
    pub(crate) holds_buffered: Vec<u8>,
    /// Each module-scope variable written, by declaration.
    pub(crate) globals: Vec<Option<GlobalVar>>,
    /// Each function variant and its id.
    pub(crate) variants: OrderedMap<VariantKey, Id>,
    /// The variants asked for and not yet written.
    pub(crate) pending: Vec<(VariantKey, Id)>,
    /// The flag a `discard` sets, where the module discards.
    pub(crate) discarded: Option<Id>,
}

/// The module `builder` wrote, laid out, admitted, and handed back as its
/// bytes; it is let go of before it is validated by whoever holds the
/// checker it was written from, so validation never holds both.
pub(crate) fn finish(builder: Builder) -> Outcome<Vec<u8>> {
    within_output(&builder)?;
    let bytes = builder.finish().map_err(refused)?.into_bytes();
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

/// The checked module written through a builder, not yet laid out.
pub(crate) fn write(checker: &mut Checker<'_>) -> Outcome<Builder> {
    let builder = Builder::new(Version { major: 1, minor: 3 }, 0).map_err(built)?;
    let decls = checker.tree.decls.len();
    let mut globals = Vec::new();
    crate::diag::reserve(&mut globals, decls)?;
    globals.resize(decls, None);
    let mut emitter = Emitter {
        c: checker,
        t: Tables {
            globals,
            ..Tables::default()
        },
        b: builder,
    };
    emitter.b.capability(Capability::SHADER).map_err(built)?;
    emitter.lower().mark_buffered()?;
    emitter.entry_points()?;
    emitter.drain()?;
    Ok(emitter.b)
}

/// A builder failure while writing: memory refused, or a defect in the
/// front end's emission, for what it writes is held to be admissible.
pub(crate) fn built(error: BuildError) -> Stop {
    match error {
        BuildError::OutOfMemory => Stop::OutOfMemory,
        _ => Stop::Internal,
    }
}

impl From<BuildError> for Stop {
    fn from(error: BuildError) -> Self {
        built(error)
    }
}

/// The validator's refusal of the finished module: past one of its fixed
/// bounds a limit of the module's, any other a defect in the emission.
fn refused(failure: BuildError) -> Stop {
    use tairix_spirv::Rule;
    match failure {
        BuildError::Refused(refusal) => match refusal.rule {
            Rule::Limit(limit) => error(Code::ModuleTooLarge, Span::default(), |m| {
                use core::fmt::Write as _;
                write!(
                    m,
                    "the module is past a bound its SPIR-V is held to: {limit:?}"
                )
            }),
            _ => Stop::Internal,
        },
        other => built(other),
    }
}

impl<'a> Emitter<'_, 'a> {
    /// The lowering view over the module's builder.
    pub(crate) fn lower(&mut self) -> Lower<'_, 'a> {
        Lower {
            c: self.c,
            t: &mut self.t,
            b: &mut self.b,
        }
    }

    /// Write every variant asked for, each of which may ask for more.
    fn drain(&mut self) -> Outcome<()> {
        while let Some((key, id)) = self.t.pending.pop() {
            self.function(&key, id)?;
            within_output(&self.b)?;
        }
        Ok(())
    }
}

/// Refuse a module already past [`MAX_OUTPUT_BYTES`], so a source that
/// writes too much is stopped while it writes rather than once it is whole.
pub(crate) fn within_output(builder: &Builder) -> Outcome<()> {
    if builder.written() > MAX_OUTPUT_BYTES {
        return Err(too_large());
    }
    Ok(())
}

fn too_large() -> Stop {
    error(Code::ModuleTooLarge, Span::default(), |m| {
        use core::fmt::Write as _;
        write!(
            m,
            "the module written is larger than {MAX_OUTPUT_BYTES} bytes"
        )
    })
}

impl Lower<'_, '_> {
    /// The storage class memory of `space` is written in.
    pub(crate) const fn class(space: Space) -> StorageClass {
        match space {
            Space::Function => StorageClass::FUNCTION,
            Space::Private => StorageClass::PRIVATE,
            Space::Workgroup => StorageClass::WORKGROUP,
            Space::Uniform => StorageClass::UNIFORM,
            Space::Storage => StorageClass::STORAGE_BUFFER,
            Space::Handle => StorageClass::UNIFORM_CONSTANT,
            Space::Immediate => StorageClass::PUSH_CONSTANT,
        }
    }

    /// The one form `flavour` names for `ty`: a buffer's types are values
    /// in their laid-out form, and a type holding none of them has one
    /// form for values and workgroup memory alike.
    pub(crate) fn canonical(&mut self, ty: TyId, flavour: Flavour) -> Outcome<Flavour> {
        Ok(match flavour {
            Flavour::Value if self.t.buffered.get(ty.0 as usize).copied().unwrap_or(false) => {
                Flavour::Laid
            }
            Flavour::Value if !self.holds_buffered(ty)? => Flavour::Plain,
            other => other,
        })
    }

    /// Whether `ty` or a type within it is held by some buffer.
    fn holds_buffered(&mut self, ty: TyId) -> Outcome<bool> {
        use crate::types::Ty;
        match self.t.holds_buffered.get(ty.0 as usize).copied() {
            Some(1) => return Ok(false),
            Some(2) => return Ok(true),
            _ => {}
        }
        let mut holds = self.t.buffered.get(ty.0 as usize).copied().unwrap_or(false);
        if !holds {
            match self.c.types.get(ty) {
                Ty::Array { element, .. } => holds = self.holds_buffered(element)?,
                Ty::Struct(structure) => {
                    for index in 0..self.c.types.members_of(structure).len() {
                        let member = self.c.types.members_of(structure)[index].ty;
                        if self.holds_buffered(member)? {
                            holds = true;
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        *crate::diag::slot(&mut self.t.holds_buffered, ty.0 as usize, 0)? =
            if holds { 2 } else { 1 };
        Ok(holds)
    }

    /// The form values of `ty` take.
    pub(crate) fn value_flavour(&mut self, ty: TyId) -> Outcome<Flavour> {
        self.canonical(ty, Flavour::Value)
    }

    /// The form memory of `space` holds `ty` in: a buffer's laid out,
    /// workgroup memory's plain, function and private memory as values are.
    pub(crate) fn memory_flavour(&mut self, space: Space, ty: TyId) -> Outcome<Flavour> {
        match space {
            _ if space.is_host_laid() => Ok(Flavour::Laid),
            Space::Workgroup | Space::Handle => Ok(Flavour::Plain),
            _ => self.value_flavour(ty),
        }
    }

    /// Mark every type a buffer holds, the types within it among them.
    pub(crate) fn mark_buffered(&mut self) -> Outcome<()> {
        use crate::check::decl::DeclInfo;
        use crate::types::Ty;
        let mut pending: Vec<TyId> = Vec::new();
        for info in &self.c.decls {
            if let DeclInfo::Global(global) = info {
                if global.space.is_host_laid() {
                    crate::diag::push(&mut pending, global.store)?;
                }
            }
        }
        while let Some(ty) = pending.pop() {
            let slot = crate::diag::slot(&mut self.t.buffered, ty.0 as usize, false)?;
            if *slot {
                continue;
            }
            *slot = true;
            match self.c.types.get(ty) {
                Ty::Array { element, .. } => crate::diag::push(&mut pending, element)?,
                Ty::Struct(structure) => {
                    for index in 0..self.c.types.members_of(structure).len() {
                        let member = self.c.types.members_of(structure)[index].ty;
                        crate::diag::push(&mut pending, member)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The id of the variant `key`, asked for and queued where new.
    pub(crate) fn variant(&mut self, key: VariantKey) -> Outcome<Id> {
        if let Some(&id) = self.t.variants.get(&key) {
            return Ok(id);
        }
        if self.t.variants.len() >= MAX_VARIANTS {
            let span = self.c.decl_name(key.0);
            return Err(error(Code::ModuleTooLarge, span, |m| {
                use core::fmt::Write as _;
                write!(
                    m,
                    "functions are called with more than {MAX_VARIANTS} shapes of pointer"
                )
            }));
        }
        let id = self.b.id().map_err(built)?;
        let mut shapes = Vec::new();
        crate::diag::reserve(&mut shapes, key.1.len())?;
        for shape in &key.1 {
            shapes.push(shape.duplicate()?);
        }
        crate::diag::push(&mut self.t.pending, ((key.0, shapes), id))?;
        self.t
            .variants
            .try_insert(key, id)
            .map_err(|_| Stop::OutOfMemory)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests;

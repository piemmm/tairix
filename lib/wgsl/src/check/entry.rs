//! Entry points: what each statically accesses, the stage restrictions
//! and resource rules over that, and its stage interface — built-in values
//! of its stage, direction and type, user-defined values at distinct
//! locations interpolated as their type requires.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::decl::{BuiltinValue, DeclInfo, EntryStage, Global, InterpolationKind, Io, MemberAttrs};
use super::graph::Graph;
use super::{Checker, NONE};
use crate::diag::{error, push, reserve, Code, Outcome, Span, Stop};
use crate::syntax::DeclKind;
use crate::types::{Access, Scalar, Space, Texture, Ty, TyId};

/// The most bytes of private variables one entry point reaches: the
/// specification's minimum limit.
pub(crate) const MAX_PRIVATE_BYTES: u64 = 8192;

/// The most bytes of workgroup variables one entry point reaches.
pub(crate) const MAX_WORKGROUP_BYTES: u64 = 16384;

/// The most bytes of the immediate variable an entry point reaches.
pub(crate) const MAX_IMMEDIATE_BYTES: u64 = 64;

/// An entry point: its function's declaration, its stage, and every
/// module-scope declaration it statically accesses, ascending.
pub(crate) struct EntryPoint {
    pub(crate) decl: u32,
    pub(crate) stage: EntryStage,
    pub(crate) reached: Vec<u32>,
    /// Its workgroup size, once overrides take their values.
    pub(crate) workgroup: [u32; 3],
}

/// Which way a stage value flows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Direction {
    In,
    Out,
}

/// A stage's mask among the stages a construct may be used in.
const fn mask(stage: EntryStage) -> u8 {
    match stage {
        EntryStage::Vertex => 1,
        EntryStage::Fragment => 2,
        EntryStage::Compute => 4,
    }
}

const fn stage_name(stage: EntryStage) -> &'static str {
    match stage {
        EntryStage::Vertex => "vertex",
        EntryStage::Fragment => "fragment",
        EntryStage::Compute => "compute",
    }
}

/// The type a built-in value has where `stage` reads or writes it, as
/// `direction`; none where that stage and direction do not have it.
fn builtin_type(
    value: BuiltinValue,
    stage: EntryStage,
    direction: Direction,
) -> Option<(u8, Scalar)> {
    use BuiltinValue as V;
    use Direction::{In, Out};
    use EntryStage::{Compute, Fragment, Vertex};
    match (value, stage, direction) {
        (V::VertexIndex | V::InstanceIndex, Vertex, In)
        | (V::SampleIndex, Fragment, In)
        | (V::SampleMask, Fragment, In | Out)
        | (V::LocalInvocationIndex | V::GlobalInvocationIndex | V::WorkgroupIndex, Compute, In) => {
            Some((1, Scalar::U32))
        }
        (V::Position, Vertex, Out) | (V::Position, Fragment, In) => Some((4, Scalar::F32)),
        (V::FrontFacing, Fragment, In) => Some((1, Scalar::Bool)),
        (V::FragDepth, Fragment, Out) => Some((1, Scalar::F32)),
        (
            V::LocalInvocationId | V::GlobalInvocationId | V::WorkgroupId | V::NumWorkgroups,
            Compute,
            In,
        ) => Some((3, Scalar::U32)),
        _ => None,
    }
}

/// One datum of a stage interface: how it meets the stage, its type, and
/// where it is declared.
#[derive(Copy, Clone)]
struct Datum {
    io: Io,
    ty: TyId,
    span: Span,
}

impl<'a> Checker<'a> {
    /// Every function and declaration each entry point reaches, with the
    /// rules its stage puts on them.
    pub(crate) fn entry_points(&mut self) -> Outcome<()> {
        let count = self.tree.decls.len();
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        reserve(&mut pairs, self.header_pairs.len() + self.references.len())?;
        pairs.extend_from_slice(&self.header_pairs);
        pairs.extend_from_slice(&self.references);
        let reach = Graph::from_edges(count, &mut pairs)?;
        let mut seen: Vec<bool> = Vec::new();
        reserve(&mut seen, count)?;
        seen.resize(count, false);
        let mut stack = Vec::new();
        let tree: &'a crate::syntax::Tree = self.tree;
        for (index, decl) in tree.decls.iter().enumerate() {
            let DeclKind::Fn(function) = decl.kind else {
                continue;
            };
            let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            let Some(&DeclInfo::Function(signature)) = self.decls.get(index as usize) else {
                return Err(Stop::Internal);
            };
            let Some(stage) = signature.stage else {
                continue;
            };
            seen.fill(false);
            reach.reach(index, &mut seen, &mut stack)?;
            let mut reached = Vec::new();
            for (decl, &hit) in seen.iter().enumerate() {
                if hit {
                    push(
                        &mut reached,
                        u32::try_from(decl).map_err(|_| Stop::OutOfMemory)?,
                    )?;
                }
            }
            self.stage_restrictions(stage, function.name, &seen)?;
            self.resources(stage, function.name, &reached)?;
            self.interface(index, stage, function.name)?;
            push(
                &mut self.entries,
                EntryPoint {
                    decl: index,
                    stage,
                    reached,
                    workgroup: [1; 3],
                },
            )?;
        }
        Ok(())
    }

    /// Refuse a construct only other stages may use, in a function the
    /// entry point reaches.
    fn stage_restrictions(&self, stage: EntryStage, entry: Span, seen: &[bool]) -> Outcome<()> {
        for used in &self.stage_uses {
            let reached = seen.get(used.function as usize).copied().unwrap_or(false);
            if reached && used.stages & mask(stage) == 0 {
                return Err(error(Code::Interface, used.span, |m| {
                    write!(m, "this is not for the {} stage, which ", stage_name(stage))?;
                    m.quote(self.spell(entry))?;
                    m.write_str(" is an entry point of")
                }));
            }
        }
        Ok(())
    }

    /// The module-scope variables an entry point reaches, held to its
    /// stage: workgroup memory only in compute, nothing a vertex shader
    /// writes, one immediate variable, distinct bindings, and the private
    /// and immediate byte limits.
    fn resources(&mut self, stage: EntryStage, entry: Span, reached: &[u32]) -> Outcome<()> {
        let mut bindings: Vec<(u32, u32, u32)> = Vec::new();
        let mut immediates = 0usize;
        let mut private_bytes = 0u64;
        for &decl in reached {
            let Some(&DeclInfo::Global(global)) = self.decls.get(decl as usize) else {
                continue;
            };
            let span = self.decl_name(decl);
            let refuse = |what: &str| {
                Err(error(Code::Interface, span, |m| {
                    m.write_str(what)?;
                    write!(m, ", which the {} entry point ", stage_name(stage))?;
                    m.quote(self.spell(entry))?;
                    m.write_str(" reaches")
                }))
            };
            match global {
                Global {
                    space: Space::Workgroup,
                    ..
                } if stage != EntryStage::Compute => {
                    return refuse("workgroup memory is for compute")
                }
                Global {
                    space: Space::Storage,
                    access: Access::ReadWrite,
                    ..
                } if stage == EntryStage::Vertex => {
                    return refuse("a vertex shader does not write a storage buffer")
                }
                Global {
                    space: Space::Handle,
                    store,
                    ..
                } if stage == EntryStage::Vertex
                    && matches!(
                        self.types.get(store),
                        Ty::Texture(Texture::Storage {
                            access: Access::Write | Access::ReadWrite,
                            ..
                        })
                    ) =>
                {
                    return refuse("a vertex shader does not write a storage texture")
                }
                _ => {}
            }
            match global.space {
                Space::Private => {
                    private_bytes = private_bytes.saturating_add(self.layout(global.store)?.size);
                }
                Space::Immediate => {
                    immediates += 1;
                    if immediates > 1 {
                        return refuse("an entry point reaches at most one immediate variable");
                    }
                    let size = self.layout(global.store)?.size;
                    if size > MAX_IMMEDIATE_BYTES {
                        return Err(error(Code::MemoryLimit, span, |m| {
                            write!(m, "an immediate variable takes {size} bytes, past {MAX_IMMEDIATE_BYTES}")
                        }));
                    }
                }
                _ => {}
            }
            if let Some((group, binding)) = global.binding {
                push(&mut bindings, (group, binding, decl))?;
            }
        }
        if private_bytes > MAX_PRIVATE_BYTES {
            return Err(error(Code::MemoryLimit, entry, |m| {
                write!(m, "this entry point's private variables take {private_bytes} bytes, past {MAX_PRIVATE_BYTES}")
            }));
        }
        bindings.sort_unstable();
        if let Some(pair) = bindings
            .windows(2)
            .find(|p| (p[0].0, p[0].1) == (p[1].0, p[1].1))
        {
            let (group, binding, decl) = pair[1];
            return Err(error(Code::Interface, self.decl_name(decl), |m| {
                write!(m, "two resources the entry point reaches take @group({group}) @binding({binding})")
            }));
        }
        Ok(())
    }

    /// The name a module-scope declaration declares, or its span.
    pub(crate) fn decl_name(&self, decl: u32) -> Span {
        self.tree
            .decls
            .get(decl as usize)
            .map_or_else(Span::default, |d| super::declared_name(d).unwrap_or(d.span))
    }

    /// An entry point's parameters and result as its stage takes them.
    fn interface(&mut self, decl: u32, stage: EntryStage, entry: Span) -> Outcome<()> {
        let Some(&DeclInfo::Function(signature)) = self.decls.get(decl as usize) else {
            return Err(Stop::Internal);
        };
        let mut inputs: Vec<Datum> = Vec::new();
        for place in signature.params.0..signature.params.1 {
            let param = self
                .params
                .get(place as usize)
                .copied()
                .ok_or(Stop::Internal)?;
            self.data(param.io, param.ty, param.span, &mut inputs)?;
        }
        let mut outputs: Vec<Datum> = Vec::new();
        if let Some(result) = signature.result {
            if stage == EntryStage::Compute {
                return Err(error(Code::Interface, entry, |m| {
                    m.write_str("a compute entry point returns nothing")
                }));
            }
            self.data(signature.result_io, result, entry, &mut outputs)?;
        }
        self.stage_data(stage, Direction::In, &inputs)?;
        self.stage_data(stage, Direction::Out, &outputs)?;
        if stage == EntryStage::Vertex {
            let positioned = outputs.iter().any(|d| {
                matches!(
                    d.io,
                    Io::Builtin {
                        value: BuiltinValue::Position,
                        ..
                    }
                )
            });
            if !positioned {
                return Err(error(Code::Interface, entry, |m| {
                    m.write_str("a vertex entry point returns the position built-in")
                }));
            }
        }
        Ok(())
    }

    /// The data one parameter or result brings to a stage interface: itself,
    /// or each member of a structure of stage values.
    fn data(&mut self, io: Io, ty: TyId, span: Span, out: &mut Vec<Datum>) -> Outcome<()> {
        if io != Io::None {
            return push(out, Datum { io, ty, span });
        }
        let Ty::Struct(structure) = self.types.get(ty) else {
            return Err(error(Code::Interface, span, |m| {
                m.write_str("an entry point's parameter or result is a built-in, at a location, or a structure of them")
            }));
        };
        let count =
            u32::try_from(self.types.members_of(structure).len()).map_err(|_| Stop::OutOfMemory)?;
        for index in 0..count {
            let member = self.types.members_of(structure)[index as usize];
            let attrs: Option<MemberAttrs> = self.member_attrs_of(structure, index);
            let io = attrs.map_or(Io::None, |a| a.io);
            if io == Io::None {
                return Err(error(Code::Interface, member.name, |m| {
                    m.write_str("each member of a stage structure is a built-in or at a location")
                }));
            }
            push(
                out,
                Datum {
                    io,
                    ty: member.ty,
                    span: member.name,
                },
            )?;
        }
        Ok(())
    }

    /// One direction of a stage's interface: each built-in its stage's in
    /// that direction, of its type, given once; each location given once,
    /// none in compute; an integer between stages interpolated flat; a
    /// blend source only among a fragment's outputs.
    fn stage_data(
        &mut self,
        stage: EntryStage,
        direction: Direction,
        data: &[Datum],
    ) -> Outcome<()> {
        let mut builtins: Vec<(BuiltinValue, Span)> = Vec::new();
        let mut locations: Vec<(u32, u32, Span)> = Vec::new();
        for datum in data {
            match datum.io {
                Io::Builtin { value, .. } => {
                    let wanted = builtin_type(value, stage, direction);
                    let found = match self.types.get(datum.ty) {
                        Ty::Scalar(s) => Some((1, s)),
                        Ty::Vector(n, s) => Some((n, s)),
                        _ => None,
                    };
                    let Some(wanted) = wanted else {
                        return Err(error(Code::Interface, datum.span, |m| {
                            write!(
                                m,
                                "this built-in is not an {} of the {} stage",
                                if direction == Direction::In {
                                    "input"
                                } else {
                                    "output"
                                },
                                stage_name(stage)
                            )
                        }));
                    };
                    if found != Some(wanted) {
                        let expected = match wanted {
                            (1, scalar) => self.types.scalar(scalar)?,
                            (n, scalar) => self.intern(Ty::Vector(n, scalar))?,
                        };
                        return Err(self.mismatch(datum.span, expected, datum.ty));
                    }
                    push(&mut builtins, (value, datum.span))?;
                }
                Io::Location {
                    location,
                    interpolation,
                    blend_src,
                } => {
                    if stage == EntryStage::Compute {
                        return Err(error(Code::Interface, datum.span, |m| {
                            m.write_str("a compute entry point has no values at locations")
                        }));
                    }
                    let fragment_out = stage == EntryStage::Fragment && direction == Direction::Out;
                    if blend_src.is_some() && !fragment_out {
                        return Err(error(Code::Interface, datum.span, |m| {
                            m.write_str("@blend_src belongs among a fragment shader's outputs")
                        }));
                    }
                    let between = matches!(
                        (stage, direction),
                        (EntryStage::Vertex, Direction::Out)
                            | (EntryStage::Fragment, Direction::In)
                    );
                    let integer = self
                        .types
                        .scalar_of(datum.ty)
                        .is_some_and(Scalar::is_integer);
                    let flat = interpolation.is_some_and(|i| i.kind == InterpolationKind::Flat);
                    if between && integer && !flat {
                        return Err(error(Code::Interface, datum.span, |m| {
                            m.write_str("an integer passed between stages is @interpolate(flat)")
                        }));
                    }
                    push(
                        &mut locations,
                        (location, blend_src.unwrap_or(NONE), datum.span),
                    )?;
                }
                Io::None => return Err(Stop::Internal),
            }
        }
        builtins.sort_unstable_by_key(|&(value, _)| value);
        if let Some(pair) = builtins.windows(2).find(|p| p[0].0 == p[1].0) {
            return Err(error(Code::Interface, pair[1].1, |m| {
                m.write_str("a built-in is given twice in one direction")
            }));
        }
        locations.sort_unstable_by_key(|&(location, blend, _)| (location, blend));
        if let Some(pair) = locations
            .windows(2)
            .find(|p| (p[0].0, p[0].1) == (p[1].0, p[1].1))
        {
            return Err(error(Code::Interface, pair[1].2, |m| {
                write!(
                    m,
                    "@location({}) is given twice in one direction",
                    pair[1].0
                )
            }));
        }
        Ok(())
    }
}

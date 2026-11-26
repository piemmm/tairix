//! Memory layout: each type's alignment and size, a structure's member
//! offsets and an array's stride, as the specification defines them, and
//! the constraints each address space puts on them.

use core::fmt::Write as _;

use super::Checker;
use crate::diag::{error, push, reserve, slot, Code, Outcome, Span, Stop};
use crate::types::{Count, Scalar, Space, Ty, TyId};

/// The largest offset or stride a module's layout holds: SPIR-V spells
/// each in 32 bits.
const MAX_OFFSET: u64 = u32::MAX as u64;

/// A type's alignment and size in bytes; a runtime-sized array's size is
/// its stride, the least it holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub(crate) align: u32,
    pub(crate) size: u64,
}

/// A structure's layout: its own, and the run of `member_offsets` holding
/// each member's offset.
#[derive(Copy, Clone, Debug)]
pub(crate) struct StructLayout {
    pub(crate) layout: Layout,
    pub(crate) offsets: u32,
}

/// The constraints checked of a structure so far, as bits: the storage
/// constraints 1, the uniform ones 2.
const CHECKED_STORAGE: u8 = 1;
const CHECKED_UNIFORM: u8 = 2;

const fn round_up(align: u64, n: u64) -> u64 {
    n.div_ceil(align) * align
}

impl Checker<'_> {
    /// The layout of `ty`; an override-sized array's once overrides take
    /// their values.
    pub(crate) fn layout(&mut self, ty: TyId) -> Outcome<Layout> {
        let scalar_layout = |s: Scalar| {
            let bytes = if s == Scalar::F16 { 2 } else { 4 };
            Layout {
                align: bytes,
                size: u64::from(bytes),
            }
        };
        Ok(match self.types.get(ty) {
            Ty::Scalar(s) | Ty::Atomic(s) => scalar_layout(s),
            Ty::Vector(n, s) => {
                let element = scalar_layout(s);
                let lanes = if n == 2 { 2 } else { 4 };
                Layout {
                    align: element.align * lanes,
                    size: element.size * u64::from(n),
                }
            }
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => {
                let column_ty = self.intern(Ty::Vector(rows, scalar))?;
                let column = self.layout(column_ty)?;
                let stride = round_up(u64::from(column.align), column.size);
                Layout {
                    align: column.align,
                    size: stride * u64::from(columns),
                }
            }
            Ty::Array { element, count } => {
                let of = self.layout(element)?;
                let stride = round_up(u64::from(of.align), of.size);
                // An override-sized count is laid out only once known.
                let count = match count {
                    Count::Runtime => 1,
                    counted => u64::from(self.count_value(counted).ok_or(Stop::Internal)?),
                };
                Layout {
                    align: of.align,
                    size: stride.saturating_mul(count),
                }
            }
            Ty::Struct(structure) => self.struct_layout(structure)?.layout,
            Ty::Pointer { .. }
            | Ty::Reference { .. }
            | Ty::Sampler { .. }
            | Ty::Texture(_)
            | Ty::Void => return Err(Stop::Internal),
        })
    }

    /// An array's element stride.
    pub(crate) fn stride(&mut self, element: TyId) -> Outcome<u64> {
        let of = self.layout(element)?;
        Ok(round_up(u64::from(of.align), of.size))
    }

    /// The value an override-sized count takes once overrides take theirs.
    pub(crate) fn count_value(&self, count: Count) -> Option<u32> {
        match count {
            Count::Fixed(n) => Some(n),
            Count::Runtime => None,
            Count::Override(decl) => match self.decls.get(decl as usize) {
                Some(super::decl::DeclInfo::Override(o)) => o
                    .value
                    .and_then(|v| self.values.integer(v))
                    .and_then(|n| u32::try_from(n).ok()),
                _ => None,
            },
            Count::Expression(expr) => self
                .info(crate::syntax::ExprId(expr))
                .value_id()
                .and_then(|v| self.values.integer(v))
                .and_then(|n| u32::try_from(n).ok()),
        }
    }

    /// A structure's layout, worked out once: each member at the least
    /// offset its alignment admits past the one before, sized by `@size`
    /// where it has one.
    pub(crate) fn struct_layout(&mut self, structure: u32) -> Outcome<StructLayout> {
        if let Some(Some(known)) = self.struct_layouts.get(structure as usize) {
            return Ok(*known);
        }
        let members: alloc::vec::Vec<_> = {
            let list = self.types.members_of(structure);
            let mut copy = alloc::vec::Vec::new();
            reserve(&mut copy, list.len())?;
            copy.extend_from_slice(list);
            copy
        };
        let offsets = u32::try_from(self.member_offsets.len()).map_err(|_| Stop::OutOfMemory)?;
        reserve(&mut self.member_offsets, members.len())?;
        let mut align = 1u32;
        let mut end = 0u64;
        for (index, member) in members.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
            let natural = self.layout(member.ty)?;
            let attrs = self.member_attrs_of(structure, index);
            let member_align = attrs.and_then(|a| a.align).unwrap_or(natural.align);
            let member_size = attrs.and_then(|a| a.size).map_or(natural.size, u64::from);
            if member_size < natural.size {
                return Err(error(Code::Layout, member.name, |m| {
                    write!(
                        m,
                        "@size({member_size}) is less than its type's size, {}",
                        natural.size
                    )
                }));
            }
            let offset = round_up(u64::from(member_align), end);
            if offset > MAX_OFFSET {
                return Err(error(Code::Layout, member.name, |m| {
                    write!(m, "a member's offset, {offset}, is past 32 bits")
                }));
            }
            let offset = u32::try_from(offset).map_err(|_| Stop::Internal)?;
            self.member_offsets.push(offset);
            end = u64::from(offset).saturating_add(member_size);
            align = align.max(member_align);
        }
        let layout = Layout {
            align,
            size: round_up(u64::from(align), end),
        };
        let known = StructLayout { layout, offsets };
        *slot(&mut self.struct_layouts, structure as usize, None)? = Some(known);
        Ok(known)
    }

    /// The offset of a structure's member `index`.
    pub(crate) fn member_offset(&mut self, structure: u32, index: u32) -> Outcome<u32> {
        let known = self.struct_layout(structure)?;
        self.member_offsets
            .get((known.offsets + index) as usize)
            .copied()
            .ok_or(Stop::Internal)
    }

    /// Refuse a store type whose layout `space` does not take: every
    /// member's offset a multiple of its type's required alignment, and in
    /// uniform memory every array's stride a multiple of 16 and a nested
    /// structure's room rounded to 16. A space no host shares lays out
    /// nothing, and any variable's types meet the storage constraints.
    pub(crate) fn laid_out(&mut self, store: TyId, space: Space, span: Span) -> Outcome<()> {
        if space == Space::Handle {
            return Ok(());
        }
        let uniform = space == Space::Uniform;
        let mut pending: alloc::vec::Vec<TyId> = alloc::vec::Vec::new();
        push(&mut pending, store)?;
        while let Some(ty) = pending.pop() {
            match self.types.get(ty) {
                Ty::Array { element, .. } => {
                    let stride = self.stride(element)?;
                    if stride > MAX_OFFSET {
                        return Err(error(Code::Layout, span, |m| {
                            write!(m, "an array's stride, {stride}, is past 32 bits")
                        }));
                    }
                    if uniform && stride % 16 != 0 {
                        return Err(error(Code::Layout, span, |m| {
                            write!(
                                m,
                                "a uniform array's stride is a multiple of 16, not {stride}"
                            )
                        }));
                    }
                    push(&mut pending, element)?;
                }
                Ty::Struct(structure) => {
                    let bit = if uniform {
                        CHECKED_UNIFORM
                    } else {
                        CHECKED_STORAGE
                    };
                    let checked = self
                        .struct_checked
                        .get(structure as usize)
                        .copied()
                        .unwrap_or(0);
                    if checked & bit != 0 {
                        continue;
                    }
                    self.struct_constraints(structure, uniform, span)?;
                    *slot(&mut self.struct_checked, structure as usize, 0)? |= bit;
                    for index in 0..self.types.members_of(structure).len() {
                        let member = self.types.members_of(structure)[index].ty;
                        push(&mut pending, member)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// One structure's members held to an address space's offsets.
    fn struct_constraints(&mut self, structure: u32, uniform: bool, span: Span) -> Outcome<()> {
        let count =
            u32::try_from(self.types.members_of(structure).len()).map_err(|_| Stop::OutOfMemory)?;
        for index in 0..count {
            let member = self.types.members_of(structure)[index as usize];
            let natural = self.layout(member.ty)?;
            let composite = matches!(self.types.get(member.ty), Ty::Array { .. } | Ty::Struct(_));
            let required = if uniform && composite {
                round_up(16, u64::from(natural.align))
            } else {
                u64::from(natural.align)
            };
            let offset = u64::from(self.member_offset(structure, index)?);
            let at = if member.name == Span::default() {
                span
            } else {
                member.name
            };
            if offset % required != 0 {
                return Err(error(Code::Layout, at, |m| {
                    write!(
                        m,
                        "this member's offset, {offset}, is not a multiple of {required}"
                    )
                }));
            }
            if uniform && matches!(self.types.get(member.ty), Ty::Struct(_)) && index + 1 < count {
                let next = u64::from(self.member_offset(structure, index + 1)?);
                let room = round_up(16, natural.size);
                if next - offset < room {
                    return Err(error(Code::Layout, at, |m| {
                        write!(
                            m,
                            "a uniform structure member takes {room} bytes before the next member"
                        )
                    }));
                }
            }
        }
        Ok(())
    }
}

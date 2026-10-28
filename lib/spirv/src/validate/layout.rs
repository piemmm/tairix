//! Explicit layout: the offsets and strides a buffer's types carry, checked
//! against the Vulkan standard buffer layout of the storage class they are
//! used in — extended alignment for a uniform block, base alignment for a
//! storage buffer or push constants.
//!
//! Each type's extent is computed once per rule set: a structure used by many
//! others is not walked again for each.

use super::types::{Scalar, Ty};
use alloc::vec::Vec;

use super::{reserve, Rule, Validator, NONE};
use crate::enumerant::{Capability, Decoration, StorageClass};

/// Which alignment a storage class lays its members out by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Rules {
    /// Base alignment: storage buffers and push constants.
    Base = 0,
    /// Extended alignment, every array and structure rounded up to 16
    /// bytes: uniform buffers.
    Extended = 1,
}

impl Rules {
    /// The alignment an array or structure of members aligned `align` has.
    const fn extended(self, align: u64) -> u64 {
        match self {
            Self::Base => align,
            Self::Extended => align.next_multiple_of(16),
        }
    }
}

/// How a type occupies a buffer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Extent {
    /// Its size in bytes, or `None` for one ending in a run-time array.
    pub(crate) size: Option<u64>,
    pub(crate) align: u64,
    /// A structure, array or matrix: the next member must not start in the
    /// padding up to its alignment.
    padded: bool,
}

/// A matrix member's stride and order, which its member decorations give.
#[derive(Copy, Clone, Debug)]
struct Matrix {
    stride: u64,
    row_major: bool,
}

/// An array of matrices laid out, its matrix aside: its extent, and the
/// stride of the array directly holding the matrix, which the matrix must
/// fit.
#[derive(Copy, Clone, Debug)]
struct Chain {
    extent: Extent,
    holder_stride: u64,
}

/// A matrix's alignment, which is a vector's: four to thirty-two bytes.
#[derive(Copy, Clone, Debug)]
struct AlignClass(usize);

impl AlignClass {
    fn of(align: u64) -> Result<Self, Rule> {
        match align {
            4 => Ok(Self(0)),
            8 => Ok(Self(1)),
            16 => Ok(Self(2)),
            32 => Ok(Self(3)),
            _ => Err(Rule::ExplicitLayout),
        }
    }
}

/// Every extent computed: a slot per type and rule set, filled only for the
/// types a buffer holds, so the cache follows what is laid out rather than
/// every type declared.
#[derive(Debug, Default)]
pub(crate) struct Layouts {
    slots: Vec<[u32; 2]>,
    extents: Vec<Extent>,
    chain_slots: Vec<u32>,
    /// Each array of matrices' chain, by rule set and alignment class.
    chains: Vec<[Option<Chain>; 8]>,
}

impl Layouts {
    fn extent(&self, ty: u32, rules: Rules) -> Option<Extent> {
        let slot = *self.slots.get(ty as usize)?.get(rules as usize)?;
        self.extents.get(slot as usize).copied()
    }

    /// Keep `extent`, the slots grown to cover every one of `types`.
    fn keep_extent(
        &mut self,
        types: usize,
        ty: u32,
        rules: Rules,
        extent: Extent,
    ) -> Result<(), Rule> {
        grow(&mut self.slots, types, [NONE; 2])?;
        let slot = u32::try_from(self.extents.len()).map_err(|_| Rule::OutOfMemory)?;
        super::push(&mut self.extents, extent).map_err(|r| r.rule)?;
        if let Some(entry) = self
            .slots
            .get_mut(ty as usize)
            .and_then(|e| e.get_mut(rules as usize))
        {
            *entry = slot;
        }
        Ok(())
    }

    fn chain(&self, ty: u32, rules: Rules, class: AlignClass) -> Option<Chain> {
        let slot = *self.chain_slots.get(ty as usize)?;
        *self
            .chains
            .get(slot as usize)?
            .get(rules as usize * 4 + class.0)?
    }

    fn keep_chain(
        &mut self,
        types: usize,
        ty: u32,
        rules: Rules,
        class: AlignClass,
        chain: Chain,
    ) -> Result<(), Rule> {
        grow(&mut self.chain_slots, types, NONE)?;
        let Some(slot) = self.chain_slots.get_mut(ty as usize) else {
            return Err(Rule::ExplicitLayout);
        };
        if *slot == NONE {
            *slot = u32::try_from(self.chains.len()).map_err(|_| Rule::OutOfMemory)?;
            super::push(&mut self.chains, [None; 8]).map_err(|r| r.rule)?;
        }
        if let Some(entry) = self
            .chains
            .get_mut(*slot as usize)
            .and_then(|c| c.get_mut(rules as usize * 4 + class.0))
        {
            *entry = Some(chain);
        }
        Ok(())
    }
}

/// Grow `list` to `len` entries of `value`, failing closed.
fn grow<T: Copy>(list: &mut Vec<T>, len: usize, value: T) -> Result<(), Rule> {
    let missing = len.saturating_sub(list.len());
    if missing > 0 {
        reserve(list, missing).map_err(|r| r.rule)?;
        list.resize(len, value);
    }
    Ok(())
}

/// One member of a structure as laid out.
#[derive(Copy, Clone, Debug)]
struct Placed {
    offset: u64,
    extent: Extent,
    vector: Option<u64>,
}

impl Validator<'_> {
    /// Check the layout of `block`, the type a buffer of `class` holds.
    pub(crate) fn buffer_layout(
        &mut self,
        block: u32,
        class: StorageClass,
    ) -> Result<Extent, Rule> {
        let rules = if class == StorageClass::UNIFORM {
            Rules::Extended
        } else {
            Rules::Base
        };
        self.extent(block, rules, None)
    }

    fn extent(&mut self, ty: u32, rules: Rules, matrix: Option<Matrix>) -> Result<Extent, Rule> {
        if let Some(matrix) = matrix {
            return self.matrix_extent(ty, rules, matrix);
        }
        if let Some(extent) = self.layouts.extent(ty, rules) {
            return Ok(extent);
        }
        let extended = |align: u64| rules.extended(align);
        let extent = match self.ty(ty).ty {
            Ty::Scalar(Scalar::Bool)
            | Ty::Vector {
                scalar: Scalar::Bool,
                ..
            } => return Err(Rule::VariableType),
            Ty::Scalar(scalar) => {
                let bytes = u64::from(scalar.width() / 8);
                Extent {
                    size: Some(bytes),
                    align: bytes,
                    padded: false,
                }
            }
            Ty::Vector { scalar, count } => {
                let bytes = u64::from(scalar.width() / 8);
                Extent {
                    size: Some(bytes * u64::from(count)),
                    align: vector_align(bytes, count),
                    padded: false,
                }
            }
            Ty::Array { element, length } => {
                let stride = self.array_stride(ty)?;
                let element = self.extent(element, rules, None)?;
                let align = extended(element.align);
                Self::element_fits(stride, element, align)?;
                Extent {
                    size: stride.checked_mul(u64::from(length)),
                    align,
                    padded: true,
                }
                .checked()?
            }
            Ty::RuntimeArray { element } => {
                let stride = self.array_stride(ty)?;
                let element = self.extent(element, rules, None)?;
                let align = extended(element.align);
                Self::element_fits(stride, element, align)?;
                Extent {
                    size: None,
                    align,
                    padded: true,
                }
            }
            Ty::Struct { first, count } => self.structure_extent(ty, first, count, rules)?,
            // A matrix is laid out by the member holding it.
            Ty::Matrix { .. } => return Err(Rule::ExplicitLayout),
            _ => return Err(Rule::VariableType),
        };
        if extent.align == 0 {
            return Err(Rule::ExplicitLayout);
        }
        self.layouts
            .keep_extent(self.types.len(), ty, rules, extent)?;
        Ok(extent)
    }

    /// The extent of `ty`, a matrix or an array of them, laid out by a
    /// member as `matrix`. The arrays are laid out once for each alignment
    /// a matrix may have; only whether the matrix fits the innermost array's
    /// stride is the member's own.
    fn matrix_extent(&mut self, ty: u32, rules: Rules, matrix: Matrix) -> Result<Extent, Rule> {
        let bottom = self.innermost(ty);
        let Ty::Matrix {
            columns,
            rows,
            scalar,
            ..
        } = self.ty(bottom).ty
        else {
            return Err(Rule::ExplicitLayout);
        };
        let (components, vectors) = if matrix.row_major {
            (columns, rows)
        } else {
            (rows, columns)
        };
        let bytes = u64::from(scalar.width() / 8);
        // A matrix's extended alignment is its base alignment: only arrays
        // and structures round up.
        let align = vector_align(bytes, components);
        let vector = bytes * u64::from(components);
        if !matrix.stride.is_multiple_of(align) || matrix.stride < vector {
            return Err(Rule::ExplicitLayout);
        }
        let own = Extent {
            size: matrix.stride.checked_mul(u64::from(vectors)),
            align,
            padded: true,
        }
        .checked()?;
        if bottom == ty {
            return Ok(own);
        }
        let chain = self.chain(ty, rules, align)?;
        if own.size.is_some_and(|size| chain.holder_stride < size) {
            return Err(Rule::ExplicitLayout);
        }
        Ok(chain.extent)
    }

    /// Array of matrices `ty` laid out for a matrix of alignment `align`:
    /// every stride fitting its element but the innermost's, which can only
    /// be judged with the matrix's size.
    fn chain(&mut self, ty: u32, rules: Rules, align: u64) -> Result<Chain, Rule> {
        let class = AlignClass::of(align)?;
        if let Some(chain) = self.layouts.chain(ty, rules, class) {
            return Ok(chain);
        }
        let (element, length) = match self.ty(ty).ty {
            Ty::Array { element, length } => (element, Some(length)),
            Ty::RuntimeArray { element } => (element, None),
            _ => return Err(Rule::ExplicitLayout),
        };
        let stride = self.array_stride(ty)?;
        let (own_align, holder_stride) = if matches!(self.ty(element).ty, Ty::Matrix { .. }) {
            let own_align = rules.extended(align);
            if !stride.is_multiple_of(own_align) {
                return Err(Rule::ExplicitLayout);
            }
            (own_align, stride)
        } else {
            let inner = self.chain(element, rules, align)?;
            let own_align = rules.extended(inner.extent.align);
            Self::element_fits(stride, inner.extent, own_align)?;
            (own_align, inner.holder_stride)
        };
        let extent = Extent {
            size: length.and_then(|l| stride.checked_mul(u64::from(l))),
            align: own_align,
            padded: true,
        };
        if length.is_some() {
            extent.checked()?;
        }
        let chain = Chain {
            extent,
            holder_stride,
        };
        self.layouts
            .keep_chain(self.types.len(), ty, rules, class, chain)?;
        Ok(chain)
    }

    /// An array's stride, which it must declare.
    fn array_stride(&self, ty: u32) -> Result<u64, Rule> {
        self.ty(ty)
            .id
            .and_then(|id| self.decoration_value_of(id, NONE, Decoration::ARRAY_STRIDE))
            .map(u64::from)
            .ok_or(Rule::ExplicitLayout)
    }

    /// Refuse a stride that overlaps an element of fixed size or breaks its
    /// alignment.
    fn element_fits(stride: u64, element: Extent, align: u64) -> Result<(), Rule> {
        let size = element.size.ok_or(Rule::RuntimeArray)?;
        if stride < size || !stride.is_multiple_of(align) {
            return Err(Rule::ExplicitLayout);
        }
        Ok(())
    }

    fn structure_extent(
        &mut self,
        ty: u32,
        first: u32,
        count: u32,
        rules: Rules,
    ) -> Result<Extent, Rule> {
        let id = self.ty(ty).id.ok_or(Rule::ExplicitLayout)?;
        let mut placed = super::with_capacity::<Placed>(count as usize).map_err(|r| r.rule)?;
        let mut align = 0;
        for member in 0..count {
            let member_ty = self
                .list(first, count)
                .get(member as usize)
                .copied()
                .ok_or(Rule::ExplicitLayout)?;
            let offset = self
                .decoration_value_of(id, member, Decoration::OFFSET)
                .map(u64::from)
                .ok_or(Rule::ExplicitLayout)?;
            let matrix = self.member_matrix(id, member, member_ty)?;
            let extent = self.extent(member_ty, rules, matrix)?;
            if offset % extent.align != 0 {
                return Err(Rule::ExplicitLayout);
            }
            align = align.max(extent.align);
            let vector = match self.ty(member_ty).ty {
                Ty::Vector { .. } => extent.size,
                _ => None,
            };
            placed.push(Placed {
                offset,
                extent,
                vector,
            });
        }
        placed.sort_unstable_by_key(|p| p.offset);
        let mut end: Option<u64> = Some(0);
        let mut previous: Option<Placed> = None;
        for (index, member) in placed.iter().enumerate() {
            let size = member.extent.size;
            if size.is_none() && index + 1 != placed.len() {
                return Err(Rule::RuntimeArray);
            }
            if let Some(previous) = previous {
                let previous_end = previous
                    .offset
                    .checked_add(previous.extent.size.ok_or(Rule::RuntimeArray)?)
                    .ok_or(Rule::ExplicitLayout)?;
                let floor = if previous.extent.padded {
                    previous_end.next_multiple_of(previous.extent.align)
                } else {
                    previous_end
                };
                if member.offset < floor {
                    return Err(Rule::ExplicitLayout);
                }
            }
            if let Some(bytes) = member.vector {
                let straddles = if bytes <= 16 {
                    member.offset / 16 != (member.offset + bytes - 1) / 16
                } else {
                    member.offset % 16 != 0
                };
                if straddles {
                    return Err(Rule::ExplicitLayout);
                }
            }
            end = match size {
                Some(size) => member.offset.checked_add(size),
                None => None,
            };
            if size.is_some() && end.is_none() {
                return Err(Rule::ExplicitLayout);
            }
            previous = Some(*member);
        }
        if count == 0 {
            align = if self.storage16()
                || self.enabled.has(Capability::INT16)
                || self.enabled.has(Capability::FLOAT16)
            {
                2
            } else {
                4
            };
        }
        Ok(Extent {
            size: end,
            align: rules.extended(align),
            padded: true,
        })
    }

    /// The matrix layout a member's decorations give: required and
    /// complete for a matrix or an array of them, absent otherwise.
    fn member_matrix(&self, id: crate::Id, member: u32, ty: u32) -> Result<Option<Matrix>, Rule> {
        let stride = self.decoration_value_of(id, member, Decoration::MATRIX_STRIDE);
        let row = self
            .decoration_of(id, member, Decoration::ROW_MAJOR)
            .is_some();
        let column = self
            .decoration_of(id, member, Decoration::COL_MAJOR)
            .is_some();
        let matrix = matches!(self.ty(self.innermost(ty)).ty, Ty::Matrix { .. });
        match (matrix, stride, row, column) {
            (true, Some(stride), true, false) | (true, Some(stride), false, true) => {
                Ok(Some(Matrix {
                    stride: u64::from(stride),
                    row_major: row,
                }))
            }
            (false, None, false, false) => Ok(None),
            _ => Err(Rule::ExplicitLayout),
        }
    }

    /// The type an array, however nested, holds at its bottom.
    pub(crate) fn innermost(&self, ty: u32) -> u32 {
        self.ty(ty).innermost
    }
}

impl Extent {
    /// Refuse a size that overflowed.
    const fn checked(self) -> Result<Self, Rule> {
        if self.size.is_none() {
            return Err(Rule::ExplicitLayout);
        }
        Ok(self)
    }
}

/// A vector's base alignment: twice its scalar's for two components, four
/// times for three or four.
const fn vector_align(bytes: u64, count: u32) -> u64 {
    if count == 2 {
        bytes * 2
    } else {
        bytes * 4
    }
}

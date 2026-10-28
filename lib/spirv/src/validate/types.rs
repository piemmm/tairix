//! Types: each declaration checked as it is met, its depth and what it holds
//! computed from the types it is built of, and the questions the rest of the
//! validator asks of one.
//!
//! No admitted instruction forward-declares a pointer, so every type is built
//! of types declared before it: the type graph has no cycles, and depth and
//! contents are computed in the one pass that meets each declaration.

use super::allow::word;
use super::{push, Refusal, Rule, What, MAX_INDEXES, MAX_MEMBERS, MAX_TYPE_DEPTH, NONE};
use super::{Limit, Validator};
use crate::enumerant::{BuiltIn, Capability, Decoration, Dim, ImageFormat, StorageClass};
use crate::{Id, Instruction, Op, Value};

tairix_util::flags! {
    /// What a type is or holds, at any depth.
    pub(crate) struct Holds(u32) {
        /// A Boolean.
        const BOOL = 1;
        /// A pointer.
        const POINTER = 1 << 1;
        /// A run-time array.
        const RUNTIME_ARRAY = 1 << 2;
        /// An image, sampler or sampled image.
        const OPAQUE = 1 << 3;
        /// A 16-bit integer.
        const INT16 = 1 << 4;
        /// A 16-bit float.
        const FLOAT16 = 1 << 5;
        /// A 64-bit integer.
        const INT64 = 1 << 6;
        /// A 64-bit float.
        const FLOAT64 = 1 << 7;
        /// An `Offset`, `ArrayStride` or `MatrixStride` decoration.
        const LAYOUT = 1 << 8;
        /// A `Block` structure.
        const BLOCK = 1 << 9;
        /// A structure whose members are built-ins.
        const BUILT_IN = 1 << 10;
        /// A matrix.
        const MATRIX = 1 << 11;
        /// A structure member decorated `Volatile`, `Coherent`,
        /// `NonWritable` or `NonReadable`.
        const QUALIFIED = 1 << 12;
        /// A structure with a latent built-in member, which nothing may read
        /// or write.
        const LATENT = 1 << 13;
    }
}

impl Holds {
    pub(crate) const fn any(self, of: Self) -> bool {
        self.bits() & of.bits() != 0
    }

    /// The 16-bit widths.
    pub(crate) const NARROW: Self = Self::INT16.with(Self::FLOAT16);
}

/// A scalar's kind and width.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Scalar {
    Bool,
    Int { width: u32, signed: bool },
    Float { width: u32 },
}

impl Scalar {
    pub(crate) const fn width(self) -> u32 {
        match self {
            Self::Bool => 0,
            Self::Int { width, .. } | Self::Float { width } => width,
        }
    }

    pub(crate) const fn is_int(self) -> bool {
        matches!(self, Self::Int { .. })
    }

    pub(crate) const fn is_float(self) -> bool {
        matches!(self, Self::Float { .. })
    }

    pub(crate) const fn is_bool(self) -> bool {
        matches!(self, Self::Bool)
    }

    pub(crate) const fn is_unsigned(self) -> bool {
        matches!(self, Self::Int { signed: false, .. })
    }
}

/// An image type's operands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Image {
    /// The sampled type's index.
    pub(crate) sampled_type: u32,
    pub(crate) dim: Dim,
    pub(crate) depth: u32,
    pub(crate) arrayed: bool,
    pub(crate) multisampled: bool,
    /// 1 for a sampled image, 2 for a storage image.
    pub(crate) sampled: u32,
    pub(crate) format: ImageFormat,
}

impl Image {
    /// Whether `other` is the same image type but perhaps for depth, which
    /// the Vulkan environment ignores.
    pub(crate) fn matches(&self, other: &Self) -> bool {
        Self { depth: 0, ..*self } == Self { depth: 0, ..*other }
    }

    /// How many coordinates address a texel, not counting the array layer.
    pub(crate) const fn coordinates(&self) -> u32 {
        match self.dim {
            Dim::DIM_1D | Dim::BUFFER => 1,
            Dim::DIM_3D | Dim::CUBE => 3,
            _ => 2,
        }
    }
}

/// What a type declaration declares, its operands resolved to type indexes.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Ty {
    Void,
    Scalar(Scalar),
    Vector {
        scalar: Scalar,
        count: u32,
    },
    Matrix {
        column: u32,
        columns: u32,
        rows: u32,
        scalar: Scalar,
    },
    Image(Image),
    Sampler,
    SampledImage {
        image: u32,
    },
    Array {
        element: u32,
        length: u32,
    },
    RuntimeArray {
        element: u32,
    },
    /// Members in [`Validator::lists`].
    Struct {
        first: u32,
        count: u32,
    },
    Pointer {
        class: StorageClass,
        pointee: u32,
    },
    /// Parameters in [`Validator::lists`].
    Function {
        result: u32,
        first: u32,
        count: u32,
    },
}

/// One declared type.
#[derive(Copy, Clone, Debug)]
pub(crate) struct TypeInfo {
    /// `None` only for the stand-in an out-of-range index reads as.
    pub(crate) id: Option<Id>,
    pub(crate) ty: Ty,
    /// Composite nesting: zero for a scalar.
    pub(crate) depth: u16,
    pub(crate) holds: Holds,
    /// The type an array, however nested, holds at its bottom; any other
    /// type's own index.
    pub(crate) innermost: u32,
}

/// The key two declarations of one non-aggregate, non-pointer type share.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct TypeKey {
    op: Op,
    words: [u32; 8],
}

impl Validator<'_> {
    pub(crate) fn ty(&self, index: u32) -> &TypeInfo {
        const VOID: TypeInfo = TypeInfo {
            id: None,
            ty: Ty::Void,
            depth: 0,
            holds: Holds::NONE,
            innermost: NONE,
        };
        self.types.get(index as usize).unwrap_or(&VOID)
    }

    /// The members of a structure or the parameters of a function type.
    pub(crate) fn list(&self, first: u32, count: u32) -> &[u32] {
        let start = first as usize;
        self.lists.get(start..start + count as usize).unwrap_or(&[])
    }

    /// The type index `id` names.
    pub(crate) fn type_index(&self, id: Id) -> Result<u32, Rule> {
        let def = self.def(id)?;
        if def.what != What::Type || def.ty == NONE {
            return Err(Rule::WrongKind(id));
        }
        Ok(def.ty)
    }

    /// The type of the value `id` names.
    pub(crate) fn value_type(&self, id: Id) -> Result<u32, Rule> {
        let def = self.def(id)?;
        let value = matches!(
            def.what,
            What::Constant
                | What::SpecConstant
                | What::Undef
                | What::Variable
                | What::Parameter
                | What::Value
        );
        if !value || def.ty == NONE {
            return Err(Rule::WrongKind(id));
        }
        Ok(def.ty)
    }

    /// The scalar of a scalar or vector type, and its component count.
    pub(crate) fn components(&self, ty: u32) -> Option<(Scalar, u32)> {
        match self.ty(ty).ty {
            Ty::Scalar(scalar) => Some((scalar, 1)),
            Ty::Vector { scalar, count } => Some((scalar, count)),
            _ => None,
        }
    }

    /// The scalar of a scalar, vector or matrix type.
    pub(crate) fn scalar_of(&self, ty: u32) -> Option<Scalar> {
        match self.ty(ty).ty {
            Ty::Scalar(scalar) | Ty::Vector { scalar, .. } | Ty::Matrix { scalar, .. } => {
                Some(scalar)
            }
            _ => None,
        }
    }

    /// The constituent types of a composite and how many there are; for a
    /// structure, each member's.
    pub(crate) fn constituent(&self, ty: u32, index: u32) -> Option<u32> {
        match self.ty(ty).ty {
            Ty::Vector { scalar, count } if index < count => self.scalar_index(scalar),
            Ty::Matrix {
                column, columns, ..
            } if index < columns => Some(column),
            Ty::Array { element, length } if index < length => Some(element),
            Ty::Struct { first, count } if index < count => {
                self.list(first, count).get(index as usize).copied()
            }
            _ => None,
        }
    }

    /// How many constituents a composite has.
    pub(crate) fn constituents(&self, ty: u32) -> Option<u32> {
        match self.ty(ty).ty {
            Ty::Vector { count, .. } | Ty::Struct { count, .. } => Some(count),
            Ty::Matrix { columns, .. } => Some(columns),
            Ty::Array { length, .. } => Some(length),
            _ => None,
        }
    }

    /// The index of the scalar type `scalar` is declared as.
    pub(crate) fn scalar_index(&self, scalar: Scalar) -> Option<u32> {
        self.scalars
            .iter()
            .find(|(s, _)| *s == scalar)
            .map(|&(_, index)| index)
    }

    /// The instruction at `index`, a type declaration, checked and entered.
    pub(crate) fn declare_type(
        &mut self,
        index: u32,
        instruction: &Instruction,
    ) -> Result<(), Refusal> {
        let at = |rule: Rule| rule.at(instruction);
        let id = self.m.result(instruction).ok_or(at(Rule::Type))?;
        let (ty, depth, holds) = self.build(instruction).map_err(at)?;
        if depth > MAX_TYPE_DEPTH {
            return Err(at(Rule::Limit(Limit::TypeDepth)));
        }
        let holds = holds.with(self.decorated_layout(id));
        let type_index = super::scan::len_u32(&self.types);
        let innermost = match ty {
            Ty::Array { element, .. } | Ty::RuntimeArray { element } => self.ty(element).innermost,
            _ => type_index,
        };
        push(
            &mut self.types,
            TypeInfo {
                id: Some(id),
                ty,
                depth,
                holds,
                innermost,
            },
        )?;
        if let Some(slot) = self.slot(id) {
            if let Some(def) = self.defs.get_mut(slot) {
                def.ty = type_index;
            }
        }
        // The first declaration of each scalar is the one a vector's
        // components resolve to; a second is a duplicate and refused below.
        if let Ty::Scalar(scalar) = ty {
            if self.scalar_index(scalar).is_none() {
                push(&mut self.scalars, (scalar, type_index))?;
            }
        }
        if let Some(key) = self.key(instruction) {
            push(&mut self.keys, (key, index))?;
        }
        Ok(())
    }

    /// The key a non-aggregate, non-pointer declaration is told apart by.
    fn key(&self, instruction: &Instruction) -> Option<TypeKey> {
        let op = instruction.op();
        if matches!(
            op,
            Op::TYPE_ARRAY | Op::TYPE_RUNTIME_ARRAY | Op::TYPE_STRUCT | Op::TYPE_POINTER
        ) {
            return None;
        }
        let operands = self.m.operands(instruction);
        let mut words = [NONE; 8];
        for (slot, operand) in words.iter_mut().zip(operands.iter().skip(1)) {
            *slot = match self.m.value(operand) {
                Value::Id(id) => id.get(),
                Value::Word(word) => word,
                _ => NONE,
            };
        }
        // A function type longer than the key is told apart by its full
        // parameter list in `duplicate_types`.
        Some(TypeKey { op, words })
    }

    /// Refuse a second declaration of a non-aggregate, non-pointer type.
    pub(crate) fn duplicate_types(&mut self) -> Result<(), Refusal> {
        let mut keys = core::mem::take(&mut self.keys);
        keys.sort_unstable();
        let m = self.m;
        let words = |at: u32| {
            m.operands(self.instruction(at))
                .iter()
                .skip(1)
                .map(|o| match m.value(o) {
                    Value::Id(id) => id.get(),
                    Value::Word(word) => word,
                    _ => NONE,
                })
        };
        // A key holds every word but a long function type's, so declarations
        // sharing one are told apart by their whole operand lists.
        for run in keys.chunk_by_mut(|(a, _), (b, _)| a == b) {
            if run.len() < 2 {
                continue;
            }
            run.sort_unstable_by(|&(_, a), &(_, b)| words(a).cmp(words(b)).then(a.cmp(&b)));
            for pair in run.windows(2) {
                if let [(_, first), (_, second)] = *pair {
                    if words(first).eq(words(second)) {
                        return Err(Rule::DuplicateType.at(self.instruction(second)));
                    }
                }
            }
        }
        Ok(())
    }

    /// Resolve and check a type declaration.
    fn build(&mut self, instruction: &Instruction) -> Result<(Ty, u16, Holds), Rule> {
        let m = self.m;
        let operands = m.operands(instruction);
        let id_at = |position: usize| match operands.get(position).map(|o| m.value(o)) {
            Some(Value::Id(id)) => Ok(id),
            _ => Err(Rule::Type),
        };
        let literal = |position: usize| word(m, instruction, position);
        match instruction.op() {
            Op::TYPE_VOID => Ok((Ty::Void, 0, Holds::NONE)),
            Op::TYPE_BOOL | Op::TYPE_INT | Op::TYPE_FLOAT | Op::TYPE_VECTOR | Op::TYPE_MATRIX => {
                self.numeric_type(instruction)
            }
            Op::TYPE_IMAGE => {
                if operands.len() > 8 {
                    return Err(Rule::Type);
                }
                let image = self.image_type(
                    self.type_index(id_at(1)?)?,
                    [
                        literal(2),
                        literal(3),
                        literal(4),
                        literal(5),
                        literal(6),
                        literal(7),
                    ],
                )?;
                Ok((Ty::Image(image), 0, Holds::OPAQUE))
            }
            Op::TYPE_SAMPLER => Ok((Ty::Sampler, 0, Holds::OPAQUE)),
            Op::TYPE_SAMPLED_IMAGE => {
                let image = self.type_index(id_at(1)?)?;
                let Ty::Image(i) = self.ty(image).ty else {
                    return Err(Rule::Type);
                };
                if i.sampled != 1 || i.dim == Dim::BUFFER {
                    return Err(Rule::Type);
                }
                Ok((Ty::SampledImage { image }, 0, Holds::OPAQUE))
            }
            Op::TYPE_ARRAY => {
                let element = self.element(id_at(1)?)?;
                let length = self.array_length(id_at(2)?)?;
                let info = self.ty(element);
                Ok((
                    Ty::Array { element, length },
                    info.depth.saturating_add(1),
                    info.holds,
                ))
            }
            Op::TYPE_RUNTIME_ARRAY => {
                let element = self.element(id_at(1)?)?;
                let info = self.ty(element);
                Ok((
                    Ty::RuntimeArray { element },
                    info.depth.saturating_add(1),
                    info.holds.with(Holds::RUNTIME_ARRAY),
                ))
            }
            Op::TYPE_STRUCT => self.structure(instruction),
            Op::TYPE_POINTER => {
                let class = StorageClass(literal(1));
                let pointee = self.type_index(id_at(2)?)?;
                let info = self.ty(pointee);
                if matches!(info.ty, Ty::Function { .. } | Ty::Pointer { .. }) {
                    return Err(Rule::Type);
                }
                if !admitted_class(class) {
                    return Err(Rule::StorageClass(class));
                }
                Ok((Ty::Pointer { class, pointee }, 0, Holds::POINTER))
            }
            Op::TYPE_FUNCTION => self.function_type(instruction),
            _ => Err(Rule::Type),
        }
    }

    /// A Boolean, integer or float, or a vector or matrix of them, the
    /// widths beyond 32 bits only by their capabilities.
    fn numeric_type(&self, instruction: &Instruction) -> Result<(Ty, u16, Holds), Rule> {
        let m = self.m;
        let operands = m.operands(instruction);
        let id_at = |position: usize| match operands.get(position).map(|o| m.value(o)) {
            Some(Value::Id(id)) => Ok(id),
            _ => Err(Rule::Type),
        };
        let literal = |position: usize| word(m, instruction, position);
        let scalar = |s: Scalar, holds: Holds| -> Result<(Ty, u16, Holds), Rule> {
            Ok((Ty::Scalar(s), 0, holds))
        };
        match instruction.op() {
            Op::TYPE_BOOL => scalar(Scalar::Bool, Holds::BOOL),
            Op::TYPE_INT => {
                let width = literal(1);
                let signed = match literal(2) {
                    0 => false,
                    1 => true,
                    _ => return Err(Rule::Type),
                };
                let holds = match width {
                    32 => Holds::NONE,
                    64 if self.enabled.has(Capability::INT64) => Holds::INT64,
                    16 if self.enabled.has(Capability::INT16) || self.storage16() => Holds::INT16,
                    _ => return Err(Rule::Type),
                };
                scalar(Scalar::Int { width, signed }, holds)
            }
            Op::TYPE_FLOAT => {
                if operands.len() > 2 {
                    return Err(Rule::Type);
                }
                let width = literal(1);
                let holds = match width {
                    32 => Holds::NONE,
                    64 if self.enabled.has(Capability::FLOAT64) => Holds::FLOAT64,
                    16 if self.enabled.has(Capability::FLOAT16) || self.storage16() => {
                        Holds::FLOAT16
                    }
                    _ => return Err(Rule::Type),
                };
                scalar(Scalar::Float { width }, holds)
            }
            Op::TYPE_VECTOR => {
                let component = self.type_index(id_at(1)?)?;
                let count = literal(2);
                let Ty::Scalar(s) = self.ty(component).ty else {
                    return Err(Rule::Type);
                };
                if !(2..=4).contains(&count) {
                    return Err(Rule::Type);
                }
                Ok((Ty::Vector { scalar: s, count }, 1, self.ty(component).holds))
            }
            Op::TYPE_MATRIX => {
                let column = self.type_index(id_at(1)?)?;
                let columns = literal(2);
                let Ty::Vector { scalar: s, count } = self.ty(column).ty else {
                    return Err(Rule::Type);
                };
                if !s.is_float() || !(2..=4).contains(&columns) {
                    return Err(Rule::Type);
                }
                let holds = self.ty(column).holds.with(Holds::MATRIX);
                let ty = Ty::Matrix {
                    column,
                    columns,
                    rows: count,
                    scalar: s,
                };
                Ok((ty, 2, holds))
            }
            _ => Err(Rule::Type),
        }
    }

    /// Whether any 16-bit storage capability is declared, which lets a
    /// module declare 16-bit types it only moves.
    pub(crate) fn storage16(&self) -> bool {
        [
            Capability::STORAGE_BUFFER16_BIT_ACCESS,
            Capability::UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS,
            Capability::STORAGE_PUSH_CONSTANT16,
            Capability::STORAGE_INPUT_OUTPUT16,
        ]
        .into_iter()
        .any(|c| self.enabled.has(c))
    }

    /// Check an image type's operands: `[dim, depth, arrayed, ms, sampled,
    /// format]`.
    fn image_type(&self, sampled_type: u32, operands: [u32; 6]) -> Result<Image, Rule> {
        let [dim, depth, arrayed, ms, sampled, format] = operands;
        let dim = Dim(dim);
        let format = ImageFormat(format);
        let Ty::Scalar(scalar) = self.ty(sampled_type).ty else {
            return Err(Rule::Type);
        };
        if scalar.width() != 32 || scalar.is_bool() {
            return Err(Rule::Type);
        }
        if depth > 2 || arrayed > 1 || ms > 1 || !matches!(sampled, 1 | 2) {
            return Err(Rule::Type);
        }
        let (arrayed, multisampled) = (arrayed == 1, ms == 1);
        let has = |c| self.enabled.has(c);
        let storage = sampled == 2;
        let dim_ok = match dim {
            Dim::DIM_1D => has(if storage {
                Capability::IMAGE1D
            } else {
                Capability::SAMPLED1D
            }),
            Dim::DIM_2D => true,
            Dim::DIM_3D | Dim::CUBE => !multisampled,
            Dim::BUFFER => {
                !arrayed
                    && !multisampled
                    && has(if storage {
                        Capability::IMAGE_BUFFER
                    } else {
                        Capability::SAMPLED_BUFFER
                    })
            }
            _ => false,
        };
        let arrayed_ok = !arrayed
            || match dim {
                Dim::DIM_1D | Dim::DIM_2D => true,
                Dim::CUBE => has(if storage {
                    Capability::IMAGE_CUBE_ARRAY
                } else {
                    Capability::SAMPLED_CUBE_ARRAY
                }),
                _ => false,
            };
        // Multisampled storage images need capabilities not admitted.
        let multisampled_ok = !multisampled || (!storage && dim == Dim::DIM_2D);
        if !dim_ok || !arrayed_ok || !multisampled_ok || !format_matches(format, scalar) {
            return Err(Rule::Type);
        }
        Ok(Image {
            sampled_type,
            dim,
            depth,
            arrayed,
            multisampled,
            sampled,
            format,
        })
    }

    /// An array's element type: not void, a function, a run-time array, or
    /// a structure ending in one.
    fn element(&self, id: Id) -> Result<u32, Rule> {
        let element = self.type_index(id)?;
        let info = self.ty(element);
        if matches!(info.ty, Ty::Void | Ty::Function { .. })
            || info.holds.contains(Holds::RUNTIME_ARRAY)
        {
            return Err(Rule::Type);
        }
        Ok(element)
    }

    /// An array's length: a non-specialization integer constant of at least
    /// one that fits in 32 bits.
    fn array_length(&self, id: Id) -> Result<u32, Rule> {
        let def = self.def(id)?;
        if def.what != What::Constant {
            return Err(Rule::ArrayLength);
        }
        let (value, scalar) = self.integer_constant(id).ok_or(Rule::ArrayLength)?;
        let signed_negative = matches!(scalar, Scalar::Int { signed: true, .. })
            && value >> (scalar.width() - 1) & 1 == 1;
        match u32::try_from(value) {
            Ok(length) if length >= 1 && !signed_negative => Ok(length),
            _ => Err(Rule::ArrayLength),
        }
    }

    /// The value of an `OpConstant` of integer type, zero-extended from its
    /// width, and its scalar.
    pub(crate) fn integer_constant(&self, id: Id) -> Option<(u64, Scalar)> {
        let instruction = self.defining(id).ok()?;
        if instruction.op() != Op::CONSTANT {
            return None;
        }
        let ty = self.value_type(id).ok()?;
        let Ty::Scalar(scalar @ Scalar::Int { width, .. }) = self.ty(ty).ty else {
            return None;
        };
        let operands = self.m.operands(instruction);
        let Value::Number(value) = self.m.value(operands.get(2)?) else {
            return None;
        };
        let mask = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        Some((value & mask, scalar))
    }

    fn structure(&mut self, instruction: &Instruction) -> Result<(Ty, u16, Holds), Rule> {
        let m = self.m;
        let operands = m.operands(instruction);
        let members = operands.len().saturating_sub(1);
        if members > MAX_MEMBERS {
            return Err(Rule::Limit(Limit::Members));
        }
        let first = super::scan::len_u32(&self.lists);
        let mut depth = 0;
        let mut holds = Holds::NONE;
        let id = m.result(instruction).ok_or(Rule::Type)?;
        let block = self.decorated(id, super::declare::BLOCK);
        let mut built_ins = 0;
        for (position, operand) in operands.iter().enumerate().skip(1) {
            let Value::Id(member_id) = m.value(operand) else {
                return Err(Rule::Type);
            };
            let member = self.type_index(member_id)?;
            let info = self.ty(member);
            if matches!(info.ty, Ty::Void | Ty::Function { .. }) || info.holds.any(Holds::OPAQUE) {
                return Err(Rule::Contents);
            }
            let last = position == operands.len() - 1;
            if info.holds.contains(Holds::RUNTIME_ARRAY)
                && (!last || !matches!(info.ty, Ty::RuntimeArray { .. }))
            {
                return Err(Rule::RuntimeArray);
            }
            if info.holds.any(Holds::BLOCK.with(Holds::BUILT_IN)) {
                return Err(Rule::Contents);
            }
            depth = depth.max(info.depth);
            holds = holds.with(info.holds);
            let index = u32::try_from(position - 1).unwrap_or(NONE);
            if let Some(built_in) = self.decoration_value_of(id, index, Decoration::BUILT_IN) {
                built_ins += 1;
                if self.enabled.latent(BuiltIn(built_in)) {
                    holds = holds.with(Holds::LATENT);
                }
            }
            push(&mut self.lists, member).map_err(|r| r.rule)?;
        }
        if built_ins != 0 {
            if built_ins != members {
                return Err(Rule::BuiltIn);
            }
            holds = holds.with(Holds::BUILT_IN);
        }
        if block {
            holds = holds.with(Holds::BLOCK);
        }
        let qualified = self.decorations_of(id).iter().any(|d| {
            d.member != NONE
                && matches!(
                    d.decoration,
                    Decoration::VOLATILE
                        | Decoration::COHERENT
                        | Decoration::NON_WRITABLE
                        | Decoration::NON_READABLE
                )
        });
        if qualified {
            holds = holds.with(Holds::QUALIFIED);
        }
        let count = u32::try_from(members).unwrap_or(NONE);
        Ok((Ty::Struct { first, count }, depth.saturating_add(1), holds))
    }

    fn function_type(&mut self, instruction: &Instruction) -> Result<(Ty, u16, Holds), Rule> {
        let m = self.m;
        let operands = m.operands(instruction);
        let Some(Value::Id(result)) = operands.get(1).map(|o| m.value(o)) else {
            return Err(Rule::Type);
        };
        let result = self.type_index(result)?;
        if matches!(
            self.ty(result).ty,
            Ty::Function { .. } | Ty::Pointer { .. } | Ty::RuntimeArray { .. }
        ) || self
            .ty(result)
            .holds
            .any(Holds::OPAQUE.with(Holds::POINTER))
        {
            return Err(Rule::Type);
        }
        let count = operands.len().saturating_sub(2);
        if count > MAX_INDEXES {
            return Err(Rule::Limit(Limit::Indexes));
        }
        let first = super::scan::len_u32(&self.lists);
        for operand in operands.iter().skip(2) {
            let Value::Id(parameter) = m.value(operand) else {
                return Err(Rule::Type);
            };
            let parameter = self.type_index(parameter)?;
            let info = self.ty(parameter);
            let admitted = match info.ty {
                Ty::Void | Ty::Function { .. } | Ty::RuntimeArray { .. } => false,
                Ty::Pointer { class, .. } => pointer_argument_class(class),
                _ => !info.holds.any(Holds::OPAQUE.with(Holds::POINTER)),
            };
            if !admitted {
                return Err(Rule::Type);
            }
            push(&mut self.lists, parameter).map_err(|r| r.rule)?;
        }
        let count = u32::try_from(count).unwrap_or(NONE);
        Ok((
            Ty::Function {
                result,
                first,
                count,
            },
            0,
            Holds::NONE,
        ))
    }
}

/// The storage classes admitted: those of a vertex, fragment or compute
/// shader's own memory and of the resources bound to it, and the `Image`
/// class an atomic's texel pointer has.
pub(crate) const fn admitted_class(class: StorageClass) -> bool {
    matches!(
        class,
        StorageClass::UNIFORM_CONSTANT
            | StorageClass::INPUT
            | StorageClass::UNIFORM
            | StorageClass::OUTPUT
            | StorageClass::WORKGROUP
            | StorageClass::PRIVATE
            | StorageClass::FUNCTION
            | StorageClass::PUSH_CONSTANT
            | StorageClass::IMAGE
            | StorageClass::STORAGE_BUFFER
    )
}

/// The storage classes a pointer passed to a function may point into.
pub(crate) const fn pointer_argument_class(class: StorageClass) -> bool {
    matches!(
        class,
        StorageClass::UNIFORM_CONSTANT
            | StorageClass::FUNCTION
            | StorageClass::PRIVATE
            | StorageClass::WORKGROUP
    )
}

/// Whether `format` reads and writes as `scalar`, the image's sampled type.
const fn format_matches(format: ImageFormat, scalar: Scalar) -> bool {
    match format {
        ImageFormat::UNKNOWN => true,
        ImageFormat::RGBA32F
        | ImageFormat::RG32F
        | ImageFormat::R32F
        | ImageFormat::RGBA16F
        | ImageFormat::RG16F
        | ImageFormat::R16F
        | ImageFormat::RGBA16
        | ImageFormat::RG16
        | ImageFormat::R16
        | ImageFormat::RGBA16_SNORM
        | ImageFormat::RG16_SNORM
        | ImageFormat::R16_SNORM
        | ImageFormat::RGB10_A2
        | ImageFormat::R11F_G11F_B10F
        | ImageFormat::RGBA8
        | ImageFormat::RG8
        | ImageFormat::R8
        | ImageFormat::RGBA8_SNORM
        | ImageFormat::RG8_SNORM
        | ImageFormat::R8_SNORM => matches!(scalar, Scalar::Float { width: 32 }),
        ImageFormat::RGBA32I
        | ImageFormat::RG32I
        | ImageFormat::R32I
        | ImageFormat::RGBA16I
        | ImageFormat::RG16I
        | ImageFormat::R16I
        | ImageFormat::RGBA8I
        | ImageFormat::RG8I
        | ImageFormat::R8I => matches!(
            scalar,
            Scalar::Int {
                width: 32,
                signed: true
            }
        ),
        ImageFormat::RGBA32UI
        | ImageFormat::RG32UI
        | ImageFormat::R32UI
        | ImageFormat::RGBA16UI
        | ImageFormat::RG16UI
        | ImageFormat::R16UI
        | ImageFormat::RGB10A2UI
        | ImageFormat::RGBA8UI
        | ImageFormat::RG8UI
        | ImageFormat::R8UI => matches!(
            scalar,
            Scalar::Int {
                width: 32,
                signed: false
            }
        ),
        _ => false,
    }
}

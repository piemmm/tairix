//! The lowering view (`plans/SHADER.md` SH8): an admitted module's types,
//! their explicit layout and its decorations as the validator resolved them,
//! kept so a backend lowers from the one reading rather than making its own.

use alloc::vec::Vec;

use tairix_util::fallible::held;

use super::declare::Deco;
use super::types::{Image, Scalar as Resolved, Ty, TypeInfo};
use super::{Admitted, NONE};
use crate::enumerant::{BuiltIn, Decoration, Dim, ImageFormat, StorageClass};
use crate::{Id, Instruction};

/// What validation resolved and an admitted module keeps.
#[derive(Debug, Default)]
pub(crate) struct Kept {
    pub(crate) types: Vec<TypeInfo>,
    /// Structure members and function parameters, as type indexes.
    pub(crate) lists: Vec<u32>,
    pub(crate) decorations: Vec<Deco>,
    /// Each type declared, by its id in order, with its type index.
    pub(crate) types_by_id: Vec<(Id, u32)>,
}

impl Kept {
    pub(crate) fn held_bytes(&self) -> u64 {
        [
            held(&self.types),
            held(&self.lists),
            held(&self.decorations),
            held(&self.types_by_id),
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// A type of an admitted module: its place in the module's type table.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TypeId(u32);

impl TypeId {
    /// Its place in the type table, from zero in declaration order.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A scalar type.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Scalar {
    /// A Boolean.
    Bool,
    /// An integer of `width` bits.
    Int {
        /// Its width in bits.
        width: u32,
        /// Whether it is signed.
        signed: bool,
    },
    /// A float of `width` bits.
    Float {
        /// Its width in bits.
        width: u32,
    },
}

impl From<Resolved> for Scalar {
    fn from(scalar: Resolved) -> Self {
        match scalar {
            Resolved::Bool => Self::Bool,
            Resolved::Int { width, signed } => Self::Int { width, signed },
            Resolved::Float { width } => Self::Float { width },
        }
    }
}

/// An image type's operands.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageType {
    /// The type its texels are read as.
    pub sampled: TypeId,
    /// Its dimensionality.
    pub dim: Dim,
    /// Its depth operand: 0 not a depth image, 1 one, 2 not said.
    pub depth: u32,
    /// Whether it is an array of layers.
    pub arrayed: bool,
    /// Whether it is multisampled.
    pub multisampled: bool,
    /// Whether it is read and written by texel rather than sampled.
    pub storage: bool,
    /// Its storage format; `ImageFormat::UNKNOWN` where any.
    pub format: ImageFormat,
}

/// What a type is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Type {
    /// No value.
    Void,
    /// A scalar.
    Scalar(Scalar),
    /// A vector of `count` scalars.
    Vector {
        /// Its components' type.
        scalar: Scalar,
        /// How many there are.
        count: u32,
    },
    /// A matrix of `columns` column vectors.
    Matrix {
        /// Its columns' type.
        column: TypeId,
        /// How many columns.
        columns: u32,
        /// How many rows.
        rows: u32,
        /// Its components' type.
        scalar: Scalar,
    },
    /// An image.
    Image(ImageType),
    /// A sampler.
    Sampler,
    /// An image and the sampler it is sampled with.
    SampledImage {
        /// The image's type.
        image: TypeId,
    },
    /// An array of fixed length.
    Array {
        /// Its elements' type.
        element: TypeId,
        /// How many.
        length: u32,
    },
    /// An array as long as the buffer holding it.
    RuntimeArray {
        /// Its elements' type.
        element: TypeId,
    },
    /// A structure of `members` members ([`Admitted::member`]).
    Struct {
        /// How many members.
        members: u32,
    },
    /// A logical pointer.
    Pointer {
        /// The storage class it points into.
        class: StorageClass,
        /// What it points at.
        pointee: TypeId,
    },
    /// A function's signature, its parameters by [`Admitted::parameter`].
    Function {
        /// What it returns.
        result: TypeId,
        /// How many parameters.
        parameters: u32,
    },
}

/// A structure member as a buffer lays it out.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Member {
    /// Its type.
    pub ty: TypeId,
    /// Its `Offset`, where it has one.
    pub offset: Option<u32>,
    /// The `MatrixStride` of the matrices it holds, however deep in arrays.
    pub matrix_stride: Option<u32>,
    /// Whether those matrices are `RowMajor`.
    pub row_major: bool,
    /// The built-in it is, where it is one.
    pub built_in: Option<BuiltIn>,
}

impl Admitted {
    /// The type `id` declares, if it declares one.
    #[must_use]
    pub fn type_id(&self, id: Id) -> Option<TypeId> {
        let types = &self.kept.types_by_id;
        let at = types
            .binary_search_by_key(&id, |&(declared, _)| declared)
            .ok()?;
        types.get(at).map(|&(_, index)| TypeId(index))
    }

    /// The type a value of `instruction`, one of the module's, has.
    #[must_use]
    pub fn result_type(&self, instruction: &Instruction) -> Option<TypeId> {
        self.type_id(self.module().result_type(instruction)?)
    }

    /// What `ty` is.
    #[must_use]
    pub fn ty(&self, ty: TypeId) -> Option<Type> {
        let info = self.kept.types.get(ty.0 as usize)?;
        Some(match info.ty {
            Ty::Void => Type::Void,
            Ty::Scalar(scalar) => Type::Scalar(scalar.into()),
            Ty::Vector { scalar, count } => Type::Vector {
                scalar: scalar.into(),
                count,
            },
            Ty::Matrix {
                column,
                columns,
                rows,
                scalar,
            } => Type::Matrix {
                column: TypeId(column),
                columns,
                rows,
                scalar: scalar.into(),
            },
            Ty::Image(image) => Type::Image(image_type(&image)),
            Ty::Sampler => Type::Sampler,
            Ty::SampledImage { image } => Type::SampledImage {
                image: TypeId(image),
            },
            Ty::Array { element, length } => Type::Array {
                element: TypeId(element),
                length,
            },
            Ty::RuntimeArray { element } => Type::RuntimeArray {
                element: TypeId(element),
            },
            Ty::Struct { count, .. } => Type::Struct { members: count },
            Ty::Pointer { class, pointee } => Type::Pointer {
                class,
                pointee: TypeId(pointee),
            },
            Ty::Function { result, count, .. } => Type::Function {
                result: TypeId(result),
                parameters: count,
            },
        })
    }

    /// Member `index` of the structure `ty`, its decorations read in one
    /// pass over the run of them the member has.
    #[must_use]
    pub fn member(&self, ty: TypeId, index: u32) -> Option<Member> {
        let id = self.kept.types.get(ty.0 as usize)?.id?;
        let mut member = Member {
            ty: self.member_type(ty, index)?,
            offset: None,
            matrix_stride: None,
            row_major: false,
            built_in: None,
        };
        let decorations = &self.kept.decorations;
        let first = decorations.partition_point(|d| (d.target, d.member) < (id, index));
        for deco in decorations
            .iter()
            .skip(first)
            .take_while(|d| (d.target, d.member) == (id, index))
        {
            match deco.decoration {
                Decoration::OFFSET => member.offset = self.param(deco),
                Decoration::MATRIX_STRIDE => member.matrix_stride = self.param(deco),
                Decoration::ROW_MAJOR => member.row_major = true,
                Decoration::BUILT_IN => member.built_in = self.param(deco).map(BuiltIn),
                _ => {}
            }
        }
        Some(member)
    }

    /// The type of member `index` of the structure `ty`.
    #[must_use]
    pub fn member_type(&self, ty: TypeId, index: u32) -> Option<TypeId> {
        let Ty::Struct { first, count } = self.kept.types.get(ty.0 as usize)?.ty else {
            return None;
        };
        (index < count)
            .then(|| self.kept.lists.get((first + index) as usize).copied())
            .flatten()
            .map(TypeId)
    }

    /// Parameter `index` of the function type `ty`.
    #[must_use]
    pub fn parameter(&self, ty: TypeId, index: u32) -> Option<TypeId> {
        let info = self.kept.types.get(ty.0 as usize)?;
        let Ty::Function { first, count, .. } = info.ty else {
            return None;
        };
        (index < count)
            .then(|| self.kept.lists.get((first + index) as usize).copied())
            .flatten()
            .map(TypeId)
    }

    /// The `ArrayStride` of the array type `ty`, where it has one.
    #[must_use]
    pub fn array_stride(&self, ty: TypeId) -> Option<u32> {
        let id = self.kept.types.get(ty.0 as usize)?.id?;
        self.decoration(id, None, Decoration::ARRAY_STRIDE)
    }

    /// The first parameter of `decoration` on `target`, or on its member
    /// `member`, where it is decorated so; zero for a decoration that takes
    /// none.
    #[must_use]
    pub fn decoration(
        &self,
        target: Id,
        member: Option<u32>,
        decoration: Decoration,
    ) -> Option<u32> {
        self.param(self.deco(target, member, decoration)?)
    }

    /// The first parameter `deco` carries, zero where it takes none.
    fn param(&self, deco: &Deco) -> Option<u32> {
        if deco.param == NONE {
            return Some(0);
        }
        let instruction = self.module().instructions().get(deco.at as usize)?;
        let operand = self
            .module()
            .operands(instruction)
            .get((deco.param) as usize)?;
        match self.module().value(operand) {
            crate::Value::Word(word) => Some(word),
            crate::Value::Id(id) => Some(id.get()),
            _ => None,
        }
    }

    fn deco(&self, target: Id, member: Option<u32>, decoration: Decoration) -> Option<&Deco> {
        let key = (target, member.unwrap_or(NONE), decoration.0);
        let decorations = &self.kept.decorations;
        decorations
            .binary_search_by_key(&key, |d| (d.target, d.member, d.decoration.0))
            .ok()
            .and_then(|at| decorations.get(at))
    }
}

fn image_type(image: &Image) -> ImageType {
    ImageType {
        sampled: TypeId(image.sampled_type),
        dim: image.dim,
        depth: image.depth,
        arrayed: image.arrayed,
        multisampled: image.multisampled,
        storage: image.sampled == 2,
        format: image.format,
    }
}

//! The types a module's values and memory have, each held once in a table
//! and named by its index there, so two types are the same exactly when
//! their indexes are.

use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use tairix_collections::OrderedMap;

use crate::diag::{push, Message, Outcome, Stop};

/// A scalar type.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Scalar {
    Bool,
    AbstractInt,
    AbstractFloat,
    I32,
    U32,
    F32,
    F16,
}

impl Scalar {
    pub(crate) const fn is_abstract(self) -> bool {
        matches!(self, Self::AbstractInt | Self::AbstractFloat)
    }

    pub(crate) const fn is_integer(self) -> bool {
        matches!(self, Self::AbstractInt | Self::I32 | Self::U32)
    }

    pub(crate) const fn is_float(self) -> bool {
        matches!(self, Self::AbstractFloat | Self::F32 | Self::F16)
    }

    pub(crate) const fn is_numeric(self) -> bool {
        !matches!(self, Self::Bool)
    }

    pub(crate) const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::AbstractInt | Self::I32 | Self::AbstractFloat | Self::F32 | Self::F16
        )
    }

    /// The concrete scalar an abstract one becomes where nothing asks
    /// otherwise: `i32` for an integer, `f32` for a float.
    pub(crate) const fn concrete(self) -> Self {
        match self {
            Self::AbstractInt => Self::I32,
            Self::AbstractFloat => Self::F32,
            other => other,
        }
    }

    /// The rank of converting `self` to `to` automatically: zero for none,
    /// `None` where it does not convert (the specification's conversion rank
    /// table).
    pub(crate) const fn rank_to(self, to: Self) -> Option<u32> {
        if self as u8 == to as u8 {
            return Some(0);
        }
        match (self, to) {
            (Self::AbstractFloat, Self::F32) => Some(1),
            (Self::AbstractFloat, Self::F16) => Some(2),
            (Self::AbstractInt, Self::I32) => Some(3),
            (Self::AbstractInt, Self::U32) => Some(4),
            (Self::AbstractInt, Self::AbstractFloat) => Some(5),
            (Self::AbstractInt, Self::F32) => Some(6),
            (Self::AbstractInt, Self::F16) => Some(7),
            _ => None,
        }
    }

    /// Its spelling.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::AbstractInt => "abstract-int",
            Self::AbstractFloat => "abstract-float",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::F32 => "f32",
            Self::F16 => "f16",
        }
    }

    /// Its width in bytes; an abstract scalar's is its 64-bit evaluation
    /// width, for it is held in no memory.
    pub(crate) const fn bytes(self) -> u32 {
        match self {
            Self::F16 => 2,
            Self::Bool | Self::I32 | Self::U32 | Self::F32 => 4,
            Self::AbstractInt | Self::AbstractFloat => 8,
        }
    }
}

/// An address space.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Space {
    Function,
    Private,
    Workgroup,
    Uniform,
    Storage,
    Handle,
    Immediate,
}

impl Space {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Private => "private",
            Self::Workgroup => "workgroup",
            Self::Uniform => "uniform",
            Self::Storage => "storage",
            Self::Handle => "handle",
            Self::Immediate => "immediate",
        }
    }

    /// The access mode a declaration in it takes where it names none.
    pub(crate) const fn default_access(self) -> Access {
        match self {
            Self::Function | Self::Private | Self::Workgroup => Access::ReadWrite,
            Self::Uniform | Self::Storage | Self::Handle | Self::Immediate => Access::Read,
        }
    }

    /// Whether a buffer laid out for the host is held in it.
    pub(crate) const fn is_host_laid(self) -> bool {
        matches!(self, Self::Uniform | Self::Storage | Self::Immediate)
    }
}

/// An access mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Access {
    Read,
    Write,
    ReadWrite,
}

impl Access {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "read_write",
        }
    }

    pub(crate) const fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    pub(crate) const fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// A texture's dimensionality, with whether it is arrayed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Dim {
    D1,
    D2,
    D2Array,
    D3,
    Cube,
    CubeArray,
}

impl Dim {
    /// The components of a coordinate within it, its array index aside.
    pub(crate) const fn coordinates(self) -> u8 {
        match self {
            Self::D1 => 1,
            Self::D2 | Self::D2Array => 2,
            Self::D3 | Self::Cube | Self::CubeArray => 3,
        }
    }

    pub(crate) const fn is_arrayed(self) -> bool {
        matches!(self, Self::D2Array | Self::CubeArray)
    }

    pub(crate) const fn is_cube(self) -> bool {
        matches!(self, Self::Cube | Self::CubeArray)
    }
}

/// A storage texture's texel format.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum TexelFormat {
    Rgba8Unorm,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Rgba16Uint,
    Rgba16Sint,
    Rgba16Float,
    R32Uint,
    R32Sint,
    R32Float,
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    Bgra8Unorm,
    Rgba16Unorm,
    Rgba16Snorm,
    Rg8Unorm,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    Rg16Unorm,
    Rg16Snorm,
    Rg16Uint,
    Rg16Sint,
    Rg16Float,
    R8Unorm,
    R8Snorm,
    R8Uint,
    R8Sint,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R16Float,
    Rgb10a2Unorm,
    Rgb10a2Uint,
    Rg11b10Ufloat,
}

/// The texel formats, by spelling: whether each needs
/// `texture_formats_tier1`.
pub(crate) const TEXEL_FORMATS: [(&str, TexelFormat, bool); 40] = [
    ("bgra8unorm", TexelFormat::Bgra8Unorm, false),
    ("r16float", TexelFormat::R16Float, true),
    ("r16sint", TexelFormat::R16Sint, true),
    ("r16snorm", TexelFormat::R16Snorm, true),
    ("r16uint", TexelFormat::R16Uint, true),
    ("r16unorm", TexelFormat::R16Unorm, true),
    ("r32float", TexelFormat::R32Float, false),
    ("r32sint", TexelFormat::R32Sint, false),
    ("r32uint", TexelFormat::R32Uint, false),
    ("r8sint", TexelFormat::R8Sint, true),
    ("r8snorm", TexelFormat::R8Snorm, true),
    ("r8uint", TexelFormat::R8Uint, true),
    ("r8unorm", TexelFormat::R8Unorm, true),
    ("rg11b10ufloat", TexelFormat::Rg11b10Ufloat, true),
    ("rg16float", TexelFormat::Rg16Float, true),
    ("rg16sint", TexelFormat::Rg16Sint, true),
    ("rg16snorm", TexelFormat::Rg16Snorm, true),
    ("rg16uint", TexelFormat::Rg16Uint, true),
    ("rg16unorm", TexelFormat::Rg16Unorm, true),
    ("rg32float", TexelFormat::Rg32Float, false),
    ("rg32sint", TexelFormat::Rg32Sint, false),
    ("rg32uint", TexelFormat::Rg32Uint, false),
    ("rg8sint", TexelFormat::Rg8Sint, true),
    ("rg8snorm", TexelFormat::Rg8Snorm, true),
    ("rg8uint", TexelFormat::Rg8Uint, true),
    ("rg8unorm", TexelFormat::Rg8Unorm, true),
    ("rgb10a2uint", TexelFormat::Rgb10a2Uint, true),
    ("rgb10a2unorm", TexelFormat::Rgb10a2Unorm, true),
    ("rgba16float", TexelFormat::Rgba16Float, false),
    ("rgba16sint", TexelFormat::Rgba16Sint, false),
    ("rgba16snorm", TexelFormat::Rgba16Snorm, true),
    ("rgba16uint", TexelFormat::Rgba16Uint, false),
    ("rgba16unorm", TexelFormat::Rgba16Unorm, true),
    ("rgba32float", TexelFormat::Rgba32Float, false),
    ("rgba32sint", TexelFormat::Rgba32Sint, false),
    ("rgba32uint", TexelFormat::Rgba32Uint, false),
    ("rgba8sint", TexelFormat::Rgba8Sint, false),
    ("rgba8snorm", TexelFormat::Rgba8Snorm, false),
    ("rgba8uint", TexelFormat::Rgba8Uint, false),
    ("rgba8unorm", TexelFormat::Rgba8Unorm, false),
];

impl TexelFormat {
    /// Its spelling.
    pub(crate) fn name(self) -> &'static str {
        TEXEL_FORMATS
            .iter()
            .find(|(_, format, _)| *format == self)
            .map_or("", |(name, _, _)| name)
    }

    /// The scalar its channels are read as: `f32`, `i32` or `u32`.
    pub(crate) const fn channel(self) -> Scalar {
        match self {
            Self::Rgba8Uint
            | Self::Rgba16Uint
            | Self::R32Uint
            | Self::Rg32Uint
            | Self::Rgba32Uint
            | Self::Rg8Uint
            | Self::Rg16Uint
            | Self::R8Uint
            | Self::R16Uint
            | Self::Rgb10a2Uint => Scalar::U32,
            Self::Rgba8Sint
            | Self::Rgba16Sint
            | Self::R32Sint
            | Self::Rg32Sint
            | Self::Rgba32Sint
            | Self::Rg8Sint
            | Self::Rg16Sint
            | Self::R8Sint
            | Self::R16Sint => Scalar::I32,
            _ => Scalar::F32,
        }
    }
}

/// A texture type.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Texture {
    /// A sampled texture of `f32`, `i32` or `u32` texels.
    Sampled { dim: Dim, texel: Scalar },
    /// A two-dimensional multisampled texture.
    Multisampled { texel: Scalar },
    /// A depth texture: 2D, 2D array, cube or cube array.
    Depth { dim: Dim },
    /// A two-dimensional multisampled depth texture.
    DepthMultisampled,
    /// A storage texture: 1D, 2D, 2D array or 3D.
    Storage {
        dim: Dim,
        format: TexelFormat,
        access: Access,
    },
    /// An external texture, read as a 2D image of float texels.
    External,
}

impl Texture {
    pub(crate) const fn dim(self) -> Dim {
        match self {
            Self::Sampled { dim, .. } | Self::Depth { dim } | Self::Storage { dim, .. } => dim,
            Self::Multisampled { .. } | Self::DepthMultisampled | Self::External => Dim::D2,
        }
    }
}

/// An array's element count.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Count {
    /// A const-expression's value.
    Fixed(u32),
    /// An override's identifier, by its declaration: two such arrays are one
    /// type only where they name the same override. Its value is known once
    /// overrides take theirs.
    Override(u32),
    /// Another override-expression, by its expression: every such array is
    /// a type of its own.
    Expression(u32),
    /// As many as its buffer holds.
    Runtime,
}

/// A type.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Ty {
    Scalar(Scalar),
    Vector(u8, Scalar),
    Matrix {
        columns: u8,
        rows: u8,
        scalar: Scalar,
    },
    Atomic(Scalar),
    Array {
        element: TyId,
        count: Count,
    },
    /// A structure, by its index in [`Types::structs`].
    Struct(u32),
    Pointer {
        space: Space,
        store: TyId,
        access: Access,
    },
    /// A memory view: what a variable's name or an access through one
    /// denotes before the load rule reads it.
    Reference {
        space: Space,
        store: TyId,
        access: Access,
    },
    Sampler {
        comparison: bool,
    },
    Texture(Texture),
    /// What a call of a function with no return type gives: no value.
    Void,
}

/// A type, by its index among the module's.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct TyId(pub(crate) u32);

/// Who declared a structure: the source, or a built-in function whose
/// result it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Origin {
    /// The module, by its declaration's index.
    Declared(u32),
    /// `frexp`'s result for a fraction of type `fraction`, its exponent of
    /// the matching integer shape.
    Frexp(TyId),
    /// `modf`'s result for type `of`.
    Modf(TyId),
    /// `atomicCompareExchangeWeak`'s result for an atomic of `scalar`.
    CompareExchange(Scalar),
}

/// A structure's member.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Member {
    /// Its name, as a span of the source; a built-in's members are named by
    /// [`Origin`].
    pub(crate) name: crate::diag::Span,
    pub(crate) ty: TyId,
}

/// A structure: who declared it, its name where the source gave it one, and
/// its members, into [`Types::members`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Structure {
    pub(crate) origin: Origin,
    pub(crate) name: crate::diag::Span,
    pub(crate) members: (u32, u32),
}

/// A kind of type another may hold anywhere within it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Holds(u8);

impl Holds {
    pub(crate) const ABSTRACT: Self = Self(1);
    pub(crate) const ATOMIC: Self = Self(1 << 1);
    pub(crate) const ARRAY: Self = Self(1 << 2);
    pub(crate) const RUNTIME_ARRAY: Self = Self(1 << 3);
    pub(crate) const OVERRIDE_ARRAY: Self = Self(1 << 4);
    pub(crate) const BOOL: Self = Self(1 << 5);
    pub(crate) const F16: Self = Self(1 << 6);
}

/// What a type admits, each only where every type within it does.
const CONSTRUCTIBLE: u8 = 1;
const STORABLE: u8 = 1 << 1;
const HOST_SHAREABLE: u8 = 1 << 2;
const CREATION_FIXED: u8 = 1 << 3;
const FIXED: u8 = 1 << 4;

/// A type's traits, worked out from its components' when it is interned,
/// so no question about a type walks the types within it.
#[derive(Copy, Clone, Debug, Default)]
struct Traits {
    holds: u8,
    admits: u8,
    depth: u32,
}

/// The names of a built-in result structure's members, by its origin.
pub(crate) const fn result_members(origin: Origin) -> Option<[&'static str; 2]> {
    match origin {
        Origin::Declared(_) => None,
        Origin::Frexp(_) => Some(["fract", "exp"]),
        Origin::Modf(_) => Some(["fract", "whole"]),
        Origin::CompareExchange(_) => Some(["old_value", "exchanged"]),
    }
}

/// The module's types.
#[derive(Default)]
pub(crate) struct Types {
    list: Vec<Ty>,
    traits: Vec<Traits>,
    index: OrderedMap<Ty, TyId>,
    pub(crate) structs: Vec<Structure>,
    by_origin: OrderedMap<Origin, u32>,
    pub(crate) members: Vec<Member>,
    /// The source spelling each override-sized count was first written
    /// with, for messages.
    count_spellings: OrderedMap<Count, crate::diag::Span>,
}

impl Types {
    /// The type `ty`, interned.
    pub(crate) fn intern(&mut self, ty: Ty) -> Outcome<TyId> {
        if let Some(&id) = self.index.get(&ty) {
            return Ok(id);
        }
        let id = TyId(u32::try_from(self.list.len()).map_err(|_| Stop::OutOfMemory)?);
        let traits = self.traits_of(ty);
        crate::diag::reserve(&mut self.list, 1)?;
        crate::diag::reserve(&mut self.traits, 1)?;
        self.index
            .try_insert(ty, id)
            .map_err(|_| Stop::OutOfMemory)?;
        self.list.push(ty);
        self.traits.push(traits);
        Ok(id)
    }

    pub(crate) fn scalar(&mut self, scalar: Scalar) -> Outcome<TyId> {
        self.intern(Ty::Scalar(scalar))
    }

    /// The type `id` names; a scalar `bool` for an index no type has, which
    /// no caller passes.
    pub(crate) fn get(&self, id: TyId) -> Ty {
        self.list
            .get(id.0 as usize)
            .copied()
            .unwrap_or(Ty::Scalar(Scalar::Bool))
    }

    fn traits(&self, id: TyId) -> Traits {
        self.traits.get(id.0 as usize).copied().unwrap_or_default()
    }

    fn traits_of(&self, ty: Ty) -> Traits {
        let numeric = |s: Scalar, depth| {
            let mut holds = 0;
            if s.is_abstract() {
                holds |= Holds::ABSTRACT.0;
            }
            if s == Scalar::Bool {
                holds |= Holds::BOOL.0;
            }
            if s == Scalar::F16 {
                holds |= Holds::F16.0;
            }
            let mut admits = CONSTRUCTIBLE | CREATION_FIXED | FIXED;
            if !s.is_abstract() {
                admits |= STORABLE;
                if s.is_numeric() {
                    admits |= HOST_SHAREABLE;
                }
            }
            Traits {
                holds,
                admits,
                depth,
            }
        };
        match ty {
            Ty::Scalar(s) => numeric(s, 0),
            Ty::Vector(_, s) => numeric(s, 1),
            Ty::Matrix { scalar, .. } => numeric(scalar, 2),
            Ty::Atomic(_) => Traits {
                holds: Holds::ATOMIC.0,
                admits: STORABLE | HOST_SHAREABLE | CREATION_FIXED | FIXED,
                depth: 0,
            },
            Ty::Array { element, count } => {
                let of = self.traits(element);
                let mut holds = of.holds | Holds::ARRAY.0;
                let mut admits = of.admits & (STORABLE | HOST_SHAREABLE);
                let element_fixed = of.admits & CREATION_FIXED != 0;
                match count {
                    Count::Fixed(_) => {
                        admits |= of.admits & (CONSTRUCTIBLE | CREATION_FIXED);
                        if element_fixed {
                            admits |= FIXED;
                        }
                    }
                    Count::Override(..) | Count::Expression(..) => {
                        holds |= Holds::OVERRIDE_ARRAY.0;
                        if element_fixed {
                            admits |= FIXED;
                        }
                    }
                    Count::Runtime => holds |= Holds::RUNTIME_ARRAY.0,
                }
                Traits {
                    holds,
                    admits,
                    depth: of.depth.saturating_add(1),
                }
            }
            Ty::Struct(s) => {
                let mut traits = Traits {
                    holds: 0,
                    admits: CONSTRUCTIBLE | STORABLE | HOST_SHAREABLE | CREATION_FIXED | FIXED,
                    depth: 0,
                };
                for member in self.members_of(s) {
                    let of = self.traits(member.ty);
                    traits.holds |= of.holds;
                    traits.admits &= of.admits;
                    traits.depth = traits.depth.max(of.depth);
                }
                traits.depth = traits.depth.saturating_add(1);
                traits
            }
            Ty::Sampler { .. } | Ty::Texture(_) => Traits {
                holds: 0,
                admits: STORABLE,
                depth: 0,
            },
            Ty::Pointer { .. } | Ty::Reference { .. } | Ty::Void => Traits::default(),
        }
    }

    /// A structure's members.
    pub(crate) fn members_of(&self, structure: u32) -> &[Member] {
        self.structs
            .get(structure as usize)
            .and_then(|s| self.members.get(s.members.0 as usize..s.members.1 as usize))
            .unwrap_or(&[])
    }

    /// Record `span` as how the override-sized count `count` is spelled,
    /// unless it already has a spelling.
    pub(crate) fn spell_count(&mut self, count: Count, span: crate::diag::Span) -> Outcome<()> {
        if self.count_spellings.get(&count).is_none() {
            self.count_spellings
                .try_insert(count, span)
                .map_err(|_| Stop::OutOfMemory)?;
        }
        Ok(())
    }

    /// The structure `origin` declared, named `name` and holding `members`,
    /// a type of its own: the one origin always answers the one type.
    pub(crate) fn structure(
        &mut self,
        origin: Origin,
        name: crate::diag::Span,
        members: &[Member],
    ) -> Outcome<TyId> {
        if let Some(&index) = self.by_origin.get(&origin) {
            return self.intern(Ty::Struct(index));
        }
        let first = u32::try_from(self.members.len()).map_err(|_| Stop::OutOfMemory)?;
        crate::diag::reserve(&mut self.members, members.len())?;
        self.members.extend_from_slice(members);
        let last = u32::try_from(self.members.len()).map_err(|_| Stop::OutOfMemory)?;
        let index = u32::try_from(self.structs.len()).map_err(|_| Stop::OutOfMemory)?;
        push(
            &mut self.structs,
            Structure {
                origin,
                name,
                members: (first, last),
            },
        )?;
        self.by_origin
            .try_insert(origin, index)
            .map_err(|_| Stop::OutOfMemory)?;
        self.intern(Ty::Struct(index))
    }

    /// The result structure of a built-in, by its origin: `frexp`'s
    /// fraction and exponent, `modf`'s fraction and whole part, or
    /// `atomicCompareExchangeWeak`'s old value and whether it exchanged.
    pub(crate) fn result(&mut self, origin: Origin) -> Outcome<TyId> {
        let unnamed = crate::diag::Span::default();
        let (first, second) = match origin {
            Origin::Frexp(of) => {
                let exponent = self.map_scalars(of, |s| match s {
                    Scalar::AbstractFloat => Scalar::AbstractInt,
                    _ => Scalar::I32,
                })?;
                (of, exponent)
            }
            Origin::Modf(of) => (of, of),
            Origin::CompareExchange(scalar) => (self.scalar(scalar)?, self.scalar(Scalar::Bool)?),
            Origin::Declared(_) => return Err(Stop::Internal),
        };
        let members = [
            Member {
                name: unnamed,
                ty: first,
            },
            Member {
                name: unnamed,
                ty: second,
            },
        ];
        self.structure(origin, unnamed, &members)
    }

    /// The origin of a structure type.
    pub(crate) fn origin_of(&self, id: TyId) -> Option<Origin> {
        match self.get(id) {
            Ty::Struct(s) => self.structs.get(s as usize).map(|s| s.origin),
            _ => None,
        }
    }

    /// The scalar of a scalar, vector, matrix or atomic type.
    pub(crate) fn scalar_of(&self, id: TyId) -> Option<Scalar> {
        match self.get(id) {
            Ty::Scalar(s) | Ty::Vector(_, s) | Ty::Atomic(s) => Some(s),
            Ty::Matrix { scalar, .. } => Some(scalar),
            _ => None,
        }
    }

    /// The type a reference's or pointer's memory holds.
    pub(crate) fn store_of(&self, id: TyId) -> Option<TyId> {
        match self.get(id) {
            Ty::Reference { store, .. } | Ty::Pointer { store, .. } => Some(store),
            _ => None,
        }
    }

    /// Whether `id` or a type within it is of the kind `kind`.
    pub(crate) fn holds(&self, id: TyId, kind: Holds) -> bool {
        self.traits(id).holds & kind.0 != 0
    }

    /// Whether it holds an abstract scalar anywhere.
    pub(crate) fn is_abstract(&self, id: TyId) -> bool {
        self.holds(id, Holds::ABSTRACT)
    }

    /// `id` with each scalar replaced by `map`'s answer for it: the shape
    /// kept, so a vector stays a vector of its count, and a built-in result
    /// structure becomes the result for the mapped type.
    pub(crate) fn map_scalars(&mut self, id: TyId, map: fn(Scalar) -> Scalar) -> Outcome<TyId> {
        match self.get(id) {
            Ty::Scalar(s) => self.intern(Ty::Scalar(map(s))),
            Ty::Vector(n, s) => self.intern(Ty::Vector(n, map(s))),
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => self.intern(Ty::Matrix {
                columns,
                rows,
                scalar: map(scalar),
            }),
            Ty::Array { element, count } => {
                let element = self.map_scalars(element, map)?;
                self.intern(Ty::Array { element, count })
            }
            Ty::Struct(_) => match self.origin_of(id) {
                Some(Origin::Frexp(of)) => {
                    let of = self.map_scalars(of, map)?;
                    self.result(Origin::Frexp(of))
                }
                Some(Origin::Modf(of)) => {
                    let of = self.map_scalars(of, map)?;
                    self.result(Origin::Modf(of))
                }
                _ => Ok(id),
            },
            _ => Ok(id),
        }
    }

    /// The concrete type an abstract one becomes where nothing asks
    /// otherwise.
    pub(crate) fn concrete(&mut self, id: TyId) -> Outcome<TyId> {
        if self.is_abstract(id) {
            self.map_scalars(id, Scalar::concrete)
        } else {
            Ok(id)
        }
    }

    /// The rank of converting a value of `from` to `to` automatically, the
    /// specification's table lifted through vectors, matrices, arrays and
    /// built-in result structures of one shape; `None` where it does not
    /// convert.
    pub(crate) fn rank(&self, from: TyId, to: TyId) -> Option<u32> {
        if from == to {
            return Some(0);
        }
        match (self.get(from), self.get(to)) {
            (Ty::Scalar(a), Ty::Scalar(b)) => a.rank_to(b),
            (Ty::Vector(n, a), Ty::Vector(m, b)) if n == m => a.rank_to(b),
            (
                Ty::Matrix {
                    columns,
                    rows,
                    scalar: from_scalar,
                },
                Ty::Matrix {
                    columns: to_columns,
                    rows: to_rows,
                    scalar: to_scalar,
                },
            ) if columns == to_columns
                && rows == to_rows
                && from_scalar.is_float()
                && to_scalar.is_float() =>
            {
                from_scalar.rank_to(to_scalar)
            }
            (
                Ty::Array {
                    element: a,
                    count: n,
                },
                Ty::Array {
                    element: b,
                    count: m,
                },
            ) if n == m => self.rank(a, b),
            (Ty::Struct(_), Ty::Struct(_)) => match (self.origin_of(from)?, self.origin_of(to)?) {
                (Origin::Frexp(a), Origin::Frexp(b)) | (Origin::Modf(a), Origin::Modf(b)) => {
                    self.rank(a, b)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether a value of it may be constructed: no runtime- or
    /// override-sized array, atomic, pointer, reference or opaque type
    /// within it.
    pub(crate) fn is_constructible(&self, id: TyId) -> bool {
        self.traits(id).admits & CONSTRUCTIBLE != 0
    }

    /// Whether it may be stored in memory: a concrete scalar, vector,
    /// matrix, atomic, array or structure, or a texture or sampler.
    pub(crate) fn is_storable(&self, id: TyId) -> bool {
        self.traits(id).admits & STORABLE != 0
    }

    /// Whether the host may lay it out: a concrete numeric scalar, vector or
    /// matrix, an atomic, or an array or structure of them.
    pub(crate) fn is_host_shareable(&self, id: TyId) -> bool {
        self.traits(id).admits & HOST_SHAREABLE != 0
    }

    /// Whether its size is fixed when the module is made: no runtime-sized
    /// or override-sized array within it.
    pub(crate) fn has_creation_fixed_footprint(&self, id: TyId) -> bool {
        self.traits(id).admits & CREATION_FIXED != 0
    }

    /// Whether its size is fixed once overrides take their values: no
    /// runtime-sized array within it.
    pub(crate) fn has_fixed_footprint(&self, id: TyId) -> bool {
        self.traits(id).admits & FIXED != 0
    }

    /// The nesting depth of a composite type: zero for a scalar or opaque
    /// type, one for a vector, two for a matrix, one more than its deepest
    /// part for an array or structure.
    pub(crate) fn depth(&self, id: TyId) -> u32 {
        self.traits(id).depth
    }
    /// Write `id` as WGSL spells it, a declared structure by its name in
    /// `text`.
    pub(crate) fn write(&self, m: &mut Message, text: &str, id: TyId) -> fmt::Result {
        match self.get(id) {
            Ty::Scalar(s) => m.write_str(s.name()),
            Ty::Vector(n, s) => write!(m, "vec{n}<{}>", s.name()),
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => write!(m, "mat{columns}x{rows}<{}>", scalar.name()),
            Ty::Atomic(s) => write!(m, "atomic<{}>", s.name()),
            Ty::Array { element, count } => {
                m.write_str("array<")?;
                self.write(m, text, element)?;
                match count {
                    Count::Runtime => m.write_str(">"),
                    Count::Fixed(n) => write!(m, ", {n}>"),
                    counted => {
                        let span = self
                            .count_spellings
                            .get(&counted)
                            .copied()
                            .unwrap_or_default();
                        m.write_str(", ")?;
                        m.write_str(
                            text.get(span.start as usize..span.end as usize)
                                .unwrap_or("?"),
                        )?;
                        m.write_str(">")
                    }
                }
            }
            Ty::Struct(index) => match self.structs.get(index as usize) {
                Some(&Structure {
                    origin: Origin::Declared(_),
                    name,
                    ..
                }) => {
                    let name = text
                        .get(name.start as usize..name.end as usize)
                        .unwrap_or("struct");
                    m.quote(name)
                }
                Some(&Structure {
                    origin: Origin::Frexp(of),
                    ..
                }) => {
                    m.write_str("__frexp_result_")?;
                    self.write_suffix(m, of)
                }
                Some(&Structure {
                    origin: Origin::Modf(of),
                    ..
                }) => {
                    m.write_str("__modf_result_")?;
                    self.write_suffix(m, of)
                }
                Some(&Structure {
                    origin: Origin::CompareExchange(s),
                    ..
                }) => write!(m, "__atomic_compare_exchange_result_{}", s.name()),
                None => m.write_str("struct"),
            },
            Ty::Pointer {
                space,
                store,
                access,
            }
            | Ty::Reference {
                space,
                store,
                access,
            } => {
                let pointer = matches!(self.get(id), Ty::Pointer { .. });
                m.write_str(if pointer { "ptr<" } else { "ref<" })?;
                write!(m, "{}, ", space.name())?;
                self.write(m, text, store)?;
                write!(m, ", {}>", access.name())
            }
            Ty::Sampler { comparison } => m.write_str(if comparison {
                "sampler_comparison"
            } else {
                "sampler"
            }),
            Ty::Texture(texture) => write_texture(m, texture),
            Ty::Void => m.write_str("no value"),
        }
    }

    /// A built-in result structure's suffix: its fraction's shape and
    /// scalar.
    fn write_suffix(&self, m: &mut Message, of: TyId) -> fmt::Result {
        match self.get(of) {
            Ty::Vector(n, s) => write!(m, "vec{n}_{}", abstract_name(s)),
            Ty::Scalar(s) => m.write_str(abstract_name(s)),
            _ => Ok(()),
        }
    }
}

/// A scalar as a built-in result structure's name spells it.
const fn abstract_name(scalar: Scalar) -> &'static str {
    match scalar {
        Scalar::AbstractFloat => "abstract",
        other => other.name(),
    }
}

fn write_texture(m: &mut Message, texture: Texture) -> fmt::Result {
    let dim = |dim: Dim| match dim {
        Dim::D1 => "1d",
        Dim::D2 => "2d",
        Dim::D2Array => "2d_array",
        Dim::D3 => "3d",
        Dim::Cube => "cube",
        Dim::CubeArray => "cube_array",
    };
    match texture {
        Texture::Sampled { dim: d, texel } => write!(m, "texture_{}<{}>", dim(d), texel.name()),
        Texture::Multisampled { texel } => {
            write!(m, "texture_multisampled_2d<{}>", texel.name())
        }
        Texture::Depth { dim: d } => write!(m, "texture_depth_{}", dim(d)),
        Texture::DepthMultisampled => m.write_str("texture_depth_multisampled_2d"),
        Texture::Storage {
            dim: d,
            format,
            access,
        } => write!(
            m,
            "texture_storage_{}<{}, {}>",
            dim(d),
            format.name(),
            access.name()
        ),
        Texture::External => m.write_str("texture_external"),
    }
}

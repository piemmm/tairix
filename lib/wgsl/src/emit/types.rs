//! Types: each WGSL type written once in each form it is held in — a
//! buffer's laid out, with its offsets, strides and matrix layout, or plain —
//! and each capability a type needs declared where it is first written.

use tairix_spirv::enumerant::{Capability, Decoration, Dim as SpirvDim, ImageFormat, StorageClass};
use tairix_spirv::{Id, ImageType};

use super::{built, Flavour, Lower};
use crate::diag::{Outcome, Stop};
use crate::types::{Count, Dim, Scalar, TexelFormat, Texture, Ty, TyId};

impl Lower<'_, '_> {
    /// The SPIR-V type of `ty` in `flavour`: only an array or structure
    /// differs between its forms.
    pub(crate) fn ty(&mut self, ty: TyId, flavour: Flavour) -> Outcome<Id> {
        let shape = self.c.types.get(ty);
        let flavour = if matches!(shape, Ty::Array { .. } | Ty::Struct(_)) {
            self.canonical(ty, flavour)?
        } else {
            Flavour::Plain
        };
        if let Some(&id) = self.t.types.get(&(ty, flavour)) {
            return Ok(id);
        }
        let id = match shape {
            Ty::Scalar(s) | Ty::Atomic(s) => self.scalar(s)?,
            Ty::Vector(n, s) => {
                let component = self.scalar(s)?;
                self.b.vector(component, u32::from(n)).map_err(built)?
            }
            Ty::Matrix {
                columns,
                rows,
                scalar,
            } => {
                let column = self.vector_of(rows, scalar)?;
                self.b.matrix(column, u32::from(columns)).map_err(built)?
            }
            Ty::Array { element, count } => self.array(element, count, flavour)?,
            Ty::Struct(structure) => self.structure(structure, flavour)?,
            Ty::Sampler { .. } => self.b.sampler().map_err(built)?,
            Ty::Texture(texture) => self.image(texture)?,
            Ty::Void => self.b.void().map_err(built)?,
            Ty::Pointer { .. } | Ty::Reference { .. } => return Err(Stop::Internal),
        };
        self.t
            .types
            .try_insert((ty, flavour), id)
            .map_err(|_| Stop::OutOfMemory)?;
        Ok(id)
    }

    /// A scalar type, its 16-bit float's capability declared.
    pub(crate) fn scalar(&mut self, scalar: Scalar) -> Outcome<Id> {
        match scalar {
            Scalar::Bool => self.b.boolean(),
            Scalar::I32 => self.b.int(32, true),
            Scalar::U32 => self.b.int(32, false),
            Scalar::F32 => self.b.float(32),
            Scalar::F16 => {
                self.b.capability(Capability::FLOAT16).map_err(built)?;
                self.b.float(16)
            }
            Scalar::AbstractInt | Scalar::AbstractFloat => return Err(Stop::Internal),
        }
        .map_err(built)
    }

    /// The vector of `n` `scalar`s.
    pub(crate) fn vector_of(&mut self, n: u8, scalar: Scalar) -> Outcome<Id> {
        let component = self.scalar(scalar)?;
        self.b.vector(component, u32::from(n)).map_err(built)
    }

    /// The pointer to `pointee` in `class`.
    pub(crate) fn pointer(&mut self, class: StorageClass, pointee: Id) -> Outcome<Id> {
        self.b.pointer(class, pointee).map_err(built)
    }

    fn array(&mut self, element: TyId, count: Count, flavour: Flavour) -> Outcome<Id> {
        let of = self.ty(element, flavour)?;
        let array = match count {
            Count::Runtime => self.b.runtime_array(of).map_err(built)?,
            counted => {
                let n = self.c.count_value(counted).ok_or(Stop::Internal)?;
                let length = self.b.uint(n).map_err(built)?;
                self.b.array(of, length).map_err(built)?
            }
        };
        if flavour == Flavour::Laid {
            let stride = u32::try_from(self.c.stride(element)?).map_err(|_| Stop::Internal)?;
            self.b
                .decorate(array, Decoration::ARRAY_STRIDE, &[stride])
                .map_err(built)?;
        }
        Ok(array)
    }

    fn structure(&mut self, structure: u32, flavour: Flavour) -> Outcome<Id> {
        let count = self.c.types.members_of(structure).len();
        let mut members = alloc::vec::Vec::new();
        crate::diag::reserve(&mut members, count)?;
        for index in 0..count {
            let member = self.c.types.members_of(structure)[index].ty;
            members.push(self.ty(member, flavour)?);
        }
        let id = self.b.structure(&members).map_err(built)?;
        if flavour == Flavour::Laid {
            for index in 0..count {
                let place = u32::try_from(index).map_err(|_| Stop::OutOfMemory)?;
                let offset = self.c.member_offset(structure, place)?;
                self.b
                    .member_decorate(id, place, Decoration::OFFSET, &[offset])
                    .map_err(built)?;
                let member = self.c.types.members_of(structure)[index].ty;
                self.matrix_layout(id, place, member)?;
            }
        }
        Ok(id)
    }

    /// Decorate member `place` of `structure` with the layout of the
    /// matrices its type `ty` holds, an array's elements' among them.
    pub(crate) fn matrix_layout(&mut self, structure: Id, place: u32, ty: TyId) -> Outcome<()> {
        let mut innermost = ty;
        while let Ty::Array { element, .. } = self.c.types.get(innermost) {
            innermost = element;
        }
        let Ty::Matrix { rows, scalar, .. } = self.c.types.get(innermost) else {
            return Ok(());
        };
        let column = self.c.types.intern(Ty::Vector(rows, scalar))?;
        let stride = u32::try_from(self.c.stride(column)?).map_err(|_| Stop::Internal)?;
        self.b
            .member_decorate(structure, place, Decoration::COL_MAJOR, &[])
            .map_err(built)?;
        self.b
            .member_decorate(structure, place, Decoration::MATRIX_STRIDE, &[stride])
            .map_err(built)
    }

    /// The image type a texture is, with the capabilities its shape needs.
    fn image(&mut self, texture: Texture) -> Outcome<Id> {
        let (dim, texel, depth, multisampled, storage, format) = match texture {
            Texture::Sampled { dim, texel } => (dim, texel, 0, false, false, ImageFormat::UNKNOWN),
            Texture::Multisampled { texel } => {
                (Dim::D2, texel, 0, true, false, ImageFormat::UNKNOWN)
            }
            Texture::Depth { dim } => (dim, Scalar::F32, 1, false, false, ImageFormat::UNKNOWN),
            Texture::DepthMultisampled => {
                (Dim::D2, Scalar::F32, 1, true, false, ImageFormat::UNKNOWN)
            }
            Texture::Storage { dim, format, .. } => (
                dim,
                format.channel(),
                0,
                false,
                true,
                self.image_format(format)?,
            ),
            Texture::External => (Dim::D2, Scalar::F32, 0, false, false, ImageFormat::UNKNOWN),
        };
        let spirv_dim = match dim {
            Dim::D1 => {
                let capability = if storage {
                    Capability::IMAGE1D
                } else {
                    Capability::SAMPLED1D
                };
                self.b.capability(capability).map_err(built)?;
                SpirvDim::DIM_1D
            }
            Dim::D2 | Dim::D2Array => SpirvDim::DIM_2D,
            Dim::D3 => SpirvDim::DIM_3D,
            Dim::Cube => SpirvDim::CUBE,
            Dim::CubeArray => {
                self.b
                    .capability(Capability::SAMPLED_CUBE_ARRAY)
                    .map_err(built)?;
                SpirvDim::CUBE
            }
        };
        let sampled = self.scalar(texel)?;
        self.b
            .image(&ImageType {
                sampled,
                dim: spirv_dim,
                depth,
                arrayed: dim.is_arrayed(),
                multisampled,
                storage,
                format,
            })
            .map_err(built)
    }

    /// The SPIR-V format of a storage texel format, `bgra8unorm`'s none: its
    /// memory order is the device's to swizzle, read and written without a
    /// stated format.
    fn image_format(&mut self, format: TexelFormat) -> Outcome<ImageFormat> {
        use ImageFormat as F;
        use TexelFormat as T;
        let (spirv, extended) = match format {
            T::Rgba8Unorm => (F::RGBA8, false),
            T::Rgba8Snorm => (F::RGBA8_SNORM, false),
            T::Rgba8Uint => (F::RGBA8UI, false),
            T::Rgba8Sint => (F::RGBA8I, false),
            T::Rgba16Uint => (F::RGBA16UI, false),
            T::Rgba16Sint => (F::RGBA16I, false),
            T::Rgba16Float => (F::RGBA16F, false),
            T::R32Uint => (F::R32UI, false),
            T::R32Sint => (F::R32I, false),
            T::R32Float => (F::R32F, false),
            T::Rg32Uint => (F::RG32UI, true),
            T::Rg32Sint => (F::RG32I, true),
            T::Rg32Float => (F::RG32F, true),
            T::Rgba32Uint => (F::RGBA32UI, false),
            T::Rgba32Sint => (F::RGBA32I, false),
            T::Rgba32Float => (F::RGBA32F, false),
            T::Bgra8Unorm => {
                for capability in [
                    Capability::STORAGE_IMAGE_READ_WITHOUT_FORMAT,
                    Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT,
                ] {
                    self.b.capability(capability).map_err(built)?;
                }
                (F::UNKNOWN, false)
            }
            T::Rgba16Unorm => (F::RGBA16, true),
            T::Rgba16Snorm => (F::RGBA16_SNORM, true),
            T::Rg8Unorm => (F::RG8, true),
            T::Rg8Snorm => (F::RG8_SNORM, true),
            T::Rg8Uint => (F::RG8UI, true),
            T::Rg8Sint => (F::RG8I, true),
            T::Rg16Unorm => (F::RG16, true),
            T::Rg16Snorm => (F::RG16_SNORM, true),
            T::Rg16Uint => (F::RG16UI, true),
            T::Rg16Sint => (F::RG16I, true),
            T::Rg16Float => (F::RG16F, true),
            T::R8Unorm => (F::R8, true),
            T::R8Snorm => (F::R8_SNORM, true),
            T::R8Uint => (F::R8UI, true),
            T::R8Sint => (F::R8I, true),
            T::R16Unorm => (F::R16, true),
            T::R16Snorm => (F::R16_SNORM, true),
            T::R16Uint => (F::R16UI, true),
            T::R16Sint => (F::R16I, true),
            T::R16Float => (F::R16F, true),
            T::Rgb10a2Unorm => (F::RGB10_A2, true),
            T::Rgb10a2Uint => (F::RGB10A2UI, true),
            T::Rg11b10Ufloat => (F::R11F_G11F_B10F, true),
        };
        if extended {
            self.b
                .capability(Capability::STORAGE_IMAGE_EXTENDED_FORMATS)
                .map_err(built)?;
        }
        Ok(spirv)
    }
}

//! Images, their views, the parts of them a command names, and the checks
//! that hold every one within what it names.

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Component, ImageShape};

use crate::format::{Aspects, Format, FormatFeatures};
use crate::info::{BufferUsage, DeviceInfo};
use crate::Error;

tairix_util::flags! {
    /// What an image may be used for.
    pub struct ImageUsage(u32) {
        /// The source of a copy.
        const TRANSFER_SRC = 1;
        /// The destination of a copy or a clear.
        const TRANSFER_DST = 1 << 1;
        /// Sampled, or read by texel, through a view.
        const SAMPLED = 1 << 2;
        /// Read and written by texel through a view.
        const STORAGE = 1 << 3;
        /// Rendered to as a colour attachment.
        const COLOR_ATTACHMENT = 1 << 4;
        /// Rendered to as a depth or stencil attachment.
        const DEPTH_STENCIL_ATTACHMENT = 1 << 5;
    }
}

impl ImageUsage {
    /// Every usage an image may declare.
    pub const ALL: Self = Self::TRANSFER_SRC
        .with(Self::TRANSFER_DST)
        .with(Self::SAMPLED)
        .with(Self::STORAGE)
        .with(Self::COLOR_ATTACHMENT)
        .with(Self::DEPTH_STENCIL_ATTACHMENT);
}

/// How many axes an image has.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ImageDimension {
    /// A row of texels.
    D1,
    /// A rectangle of texels.
    D2,
    /// A box of texels.
    D3,
}

/// A size in texels.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Extent3d {
    /// Across.
    pub width: u32,
    /// Down.
    pub height: u32,
    /// Deep: one for an image of fewer than three axes.
    pub depth: u32,
}

/// A texel's place.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Origin3d {
    /// Across.
    pub x: u32,
    /// Down.
    pub y: u32,
    /// Deep.
    pub z: u32,
}

/// How an image is made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImageDesc {
    /// How many axes it has.
    pub dimension: ImageDimension,
    /// What each texel holds.
    pub format: Format,
    /// Its size at its first mip level.
    pub extent: Extent3d,
    /// How many mip levels it has, each half the size of the one before.
    pub mip_levels: u32,
    /// How many layers it has: one for a three-dimensional image.
    pub array_layers: u32,
    /// How many samples each texel holds: one, or four.
    pub samples: u32,
    /// What it may be used for.
    pub usage: ImageUsage,
}

/// The size of mip level `level` of an image `extent` in size, a three-axis
/// image halving its depth as it does the other two.
#[must_use]
pub const fn level_extent(extent: Extent3d, dimension: ImageDimension, level: u32) -> Extent3d {
    Extent3d {
        width: halved(extent.width, level),
        height: halved(extent.height, level),
        depth: match dimension {
            ImageDimension::D3 => halved(extent.depth, level),
            ImageDimension::D1 | ImageDimension::D2 => extent.depth,
        },
    }
}

/// `size` halved `level` times, and never below one.
const fn halved(size: u32, level: u32) -> u32 {
    let shifted = if level >= 32 { 0 } else { size >> level };
    if shifted == 0 {
        1
    } else {
        shifted
    }
}

/// How many mip levels an image `extent` in size can have: down to one
/// texel along its longest axis.
const fn full_chain(extent: Extent3d, dimension: ImageDimension) -> u32 {
    let longest = match dimension {
        ImageDimension::D1 => extent.width,
        ImageDimension::D2 => max(extent.width, extent.height),
        ImageDimension::D3 => max(max(extent.width, extent.height), extent.depth),
    };
    32 - longest.leading_zeros()
}

const fn max(a: u32, b: u32) -> u32 {
    if a > b {
        a
    } else {
        b
    }
}

impl ImageDesc {
    /// Refuse an image `device` does not make: an empty size or usage, a
    /// shape its dimension does not have, a size past the device's, more mip
    /// levels than its size halves through, a sample count other than one or
    /// four or four for what cannot hold them, or a usage the device does not
    /// state of its format.
    pub(crate) fn check(&self, device: &DeviceInfo) -> Result<(), Error> {
        let limits = &device.limits;
        let Extent3d {
            width,
            height,
            depth,
        } = self.extent;
        if width == 0
            || height == 0
            || depth == 0
            || self.mip_levels == 0
            || self.array_layers == 0
            || self.usage == ImageUsage::NONE
        {
            return Err(Error::Empty);
        }
        if !ImageUsage::ALL.contains(self.usage) {
            return Err(Error::Usage);
        }
        let (shaped, largest) = match self.dimension {
            ImageDimension::D1 => (height == 1 && depth == 1, limits.max_image_dimension_1d),
            ImageDimension::D2 => (depth == 1, limits.max_image_dimension_2d),
            ImageDimension::D3 => (self.array_layers == 1, limits.max_image_dimension_3d),
        };
        if !shaped {
            return Err(Error::Shape);
        }
        if width > largest
            || height > largest
            || depth > largest
            || self.array_layers > limits.max_image_array_layers
            || self.mip_levels > full_chain(self.extent, self.dimension)
        {
            return Err(Error::TooLarge);
        }
        let features = device.formats.get(self.format);
        match self.samples {
            1 => {}
            4 if features.contains(FormatFeatures::MULTISAMPLED)
                && self.dimension == ImageDimension::D2
                && self.mip_levels == 1
                && !self.usage.contains(ImageUsage::STORAGE)
                && self.usage.intersects(
                    ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::DEPTH_STENCIL_ATTACHMENT),
                ) => {}
            _ => return Err(Error::Samples),
        }
        let offered = [
            (ImageUsage::SAMPLED, FormatFeatures::SAMPLED),
            (ImageUsage::STORAGE, FormatFeatures::STORAGE),
            (
                ImageUsage::COLOR_ATTACHMENT,
                FormatFeatures::COLOR_ATTACHMENT,
            ),
            (
                ImageUsage::DEPTH_STENCIL_ATTACHMENT,
                FormatFeatures::DEPTH_STENCIL,
            ),
        ]
        .iter()
        .all(|&(usage, feature)| !self.usage.contains(usage) || features.contains(feature));
        if offered {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }

    /// The size of mip level `level`.
    #[must_use]
    pub const fn level_extent(&self, level: u32) -> Extent3d {
        level_extent(self.extent, self.dimension, level)
    }
}

/// How a view sees its image.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ViewDimension {
    /// A row.
    D1,
    /// Rows, one a layer.
    D1Array,
    /// A rectangle.
    D2,
    /// Rectangles, one a layer.
    D2Array,
    /// Six square layers as the faces of a cube.
    Cube,
    /// Six square layers a cube, of as many cubes as the layers make.
    CubeArray,
    /// A box.
    D3,
}

impl ViewDimension {
    /// The dimension and arrayedness a module declares for an image it sees
    /// through a view of this dimension.
    #[must_use]
    pub const fn module_dim(self) -> (Dim, bool) {
        match self {
            Self::D1 => (Dim::DIM_1D, false),
            Self::D1Array => (Dim::DIM_1D, true),
            Self::D2 => (Dim::DIM_2D, false),
            Self::D2Array => (Dim::DIM_2D, true),
            Self::Cube => (Dim::CUBE, false),
            Self::CubeArray => (Dim::CUBE, true),
            Self::D3 => (Dim::DIM_3D, false),
        }
    }
}

/// The mip levels and layers of an image.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageRange {
    /// The planes: colour, depth, stencil, or depth and stencil.
    pub aspects: Aspects,
    /// The first mip level.
    pub base_level: u32,
    /// How many mip levels.
    pub levels: u32,
    /// The first layer.
    pub base_layer: u32,
    /// How many layers.
    pub layers: u32,
}

impl ImageRange {
    /// Refuse a range of no aspect, level or layer, or one an image of `desc`
    /// does not hold.
    pub(crate) fn check(&self, desc: &ImageDesc) -> Result<(), Error> {
        if self.levels == 0 || self.layers == 0 || self.aspects == Aspects::NONE {
            return Err(Error::Empty);
        }
        if !desc.format.aspects().contains(self.aspects) {
            return Err(Error::Aspect);
        }
        let levels = self.base_level.checked_add(self.levels);
        let layers = self.base_layer.checked_add(self.layers);
        if levels.is_none_or(|end| end > desc.mip_levels)
            || layers.is_none_or(|end| end > desc.array_layers)
        {
            return Err(Error::OutOfRange);
        }
        Ok(())
    }
}

/// How a view is made.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageViewDesc {
    /// The format it reads its image as: the image's own, or its sRGB twin
    /// ([`Format::srgb_twin`]).
    pub format: Format,
    /// How it sees the image.
    pub dimension: ViewDimension,
    /// The planes, mip levels and layers it sees.
    pub range: ImageRange,
}

impl ImageViewDesc {
    /// Refuse a view an image of `desc` cannot be seen through: a format
    /// other than its own or its twin, a range it does not hold, a dimension
    /// its own does not take, a layer count the dimension does not, a cube of
    /// faces that are not square, or a multisampled image seen as anything but
    /// rectangles.
    pub(crate) fn check(&self, desc: &ImageDesc) -> Result<(), Error> {
        if self.format != desc.format && desc.format.srgb_twin() != Some(self.format) {
            return Err(Error::Shape);
        }
        self.range.check(desc)?;
        let layers = self.range.layers;
        let square = desc.extent.width == desc.extent.height;
        let shaped = match (self.dimension, desc.dimension) {
            (ViewDimension::D1, ImageDimension::D1)
            | (ViewDimension::D2, ImageDimension::D2)
            | (ViewDimension::D3, ImageDimension::D3) => layers == 1,
            (ViewDimension::D1Array, ImageDimension::D1)
            | (ViewDimension::D2Array, ImageDimension::D2) => true,
            (ViewDimension::Cube, ImageDimension::D2) => layers == 6 && square,
            (ViewDimension::CubeArray, ImageDimension::D2) => layers.is_multiple_of(6) && square,
            _ => false,
        };
        let sampled = desc.samples == 1
            || matches!(self.dimension, ViewDimension::D2 | ViewDimension::D2Array);
        if shaped && sampled {
            Ok(())
        } else {
            Err(Error::Shape)
        }
    }

    /// The shape a module declares for an image it sees through this view of
    /// an image of `desc`, with the storage format its texels are read and
    /// written as.
    pub(crate) fn shape(&self, desc: &ImageDesc) -> ImageShape {
        let (dim, arrayed) = self.dimension.module_dim();
        let component = if self.range.aspects == Aspects::STENCIL {
            Component::Uint
        } else {
            self.format.component()
        };
        ImageShape {
            dim,
            arrayed,
            multisampled: desc.samples > 1,
            component,
            format: self.format.storage_format().unwrap_or(ImageFormat::UNKNOWN),
        }
    }
}

/// How a texel buffer is made over a buffer.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BufferViewDesc {
    /// Where it starts in the buffer: a multiple of
    /// [`Limits::storage_buffer_alignment`](crate::Limits::storage_buffer_alignment).
    pub offset: u64,
    /// Its size in bytes: whole texels.
    pub size: u64,
    /// The colour format its texels are read as.
    pub format: Format,
}

impl BufferViewDesc {
    /// Refuse a texel buffer over a buffer `size` bytes long made for `usage`:
    /// a format the device does not state for the texel-buffer use the buffer
    /// was made for, a range misaligned, of part of a texel, past the buffer or
    /// past the device's element count.
    pub(crate) fn check(
        &self,
        size: u64,
        usage: BufferUsage,
        device: &DeviceInfo,
    ) -> Result<(), Error> {
        let features = device.formats.get(self.format);
        let uniform = usage.contains(BufferUsage::UNIFORM_TEXEL)
            && features.contains(FormatFeatures::UNIFORM_TEXEL_BUFFER);
        let storage = usage.contains(BufferUsage::STORAGE_TEXEL)
            && features.contains(FormatFeatures::STORAGE_TEXEL_BUFFER);
        if !usage.intersects(BufferUsage::UNIFORM_TEXEL.with(BufferUsage::STORAGE_TEXEL)) {
            return Err(Error::Usage);
        }
        if !(uniform || storage) {
            return Err(Error::Unsupported);
        }
        let texel = u64::from(
            self.format
                .copy_bytes(Aspects::COLOR)
                .ok_or(Error::Aspect)?,
        );
        if self.size == 0 {
            return Err(Error::Empty);
        }
        if !self
            .offset
            .is_multiple_of(device.limits.storage_buffer_alignment)
            || !self.size.is_multiple_of(texel)
        {
            return Err(Error::Misaligned);
        }
        if self
            .offset
            .checked_add(self.size)
            .is_none_or(|end| end > size)
        {
            return Err(Error::OutOfRange);
        }
        if self.size / texel > device.limits.max_texel_buffer_elements {
            return Err(Error::TooLarge);
        }
        Ok(())
    }

    /// The shape a module declares for a texel buffer it reads through this
    /// view.
    pub(crate) fn shape(&self) -> ImageShape {
        ImageShape {
            dim: Dim::BUFFER,
            arrayed: false,
            multisampled: false,
            component: self.format.component(),
            format: self.format.storage_format().unwrap_or(ImageFormat::UNKNOWN),
        }
    }
}

/// One mip level and some layers of an image, in one aspect: what a copy
/// moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageSubresource {
    /// The one plane: colour, depth or stencil.
    pub aspect: Aspects,
    /// The mip level.
    pub level: u32,
    /// The first layer.
    pub base_layer: u32,
    /// How many layers.
    pub layers: u32,
}

impl ImageSubresource {
    /// The range it is.
    const fn range(self) -> ImageRange {
        ImageRange {
            aspects: self.aspect,
            base_level: self.level,
            levels: 1,
            base_layer: self.base_layer,
            layers: self.layers,
        }
    }

    /// Refuse a part an image of `desc` does not hold, more than one plane,
    /// or a region `origin` and `extent` describe past its level's edge.
    pub(crate) fn check_region(
        &self,
        desc: &ImageDesc,
        origin: Origin3d,
        extent: Extent3d,
    ) -> Result<(), Error> {
        self.range().check(desc)?;
        if desc.format.copy_bytes(self.aspect).is_none() {
            return Err(Error::Aspect);
        }
        if extent.width == 0 || extent.height == 0 || extent.depth == 0 {
            return Err(Error::Empty);
        }
        let level = desc.level_extent(self.level);
        let within = |start: u32, length: u32, edge: u32| {
            start.checked_add(length).is_some_and(|end| end <= edge)
        };
        if within(origin.x, extent.width, level.width)
            && within(origin.y, extent.height, level.height)
            && within(origin.z, extent.depth, level.depth)
        {
            Ok(())
        } else {
            Err(Error::OutOfRange)
        }
    }
}

/// One region a copy between a buffer and an image moves.
///
/// In the buffer, each row of texels is `bytes_per_row` bytes after the one
/// before, and each slice or layer `rows_per_image` rows after the one before.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BufferImageCopy {
    /// Where the region's first texel lies in the buffer.
    pub buffer_offset: u64,
    /// The bytes from one row to the next: at least a row of the region, and
    /// a whole number of texels.
    pub bytes_per_row: u32,
    /// The rows from one slice or layer to the next: at least the region's.
    pub rows_per_image: u32,
    /// The image's level, layers and plane.
    pub image: ImageSubresource,
    /// Where the region starts in the level.
    pub origin: Origin3d,
    /// Its size.
    pub extent: Extent3d,
}

impl BufferImageCopy {
    /// Refuse a region past either side, a buffer layout that does not hold
    /// it, or a multisampled image; answer the bytes the buffer side spans
    /// from its first texel.
    pub(crate) fn check(&self, desc: &ImageDesc) -> Result<u64, Error> {
        if desc.samples != 1 {
            return Err(Error::Samples);
        }
        self.image.check_region(desc, self.origin, self.extent)?;
        let texel = desc
            .format
            .copy_bytes(self.image.aspect)
            .ok_or(Error::Aspect)?;
        let row = u64::from(self.extent.width) * u64::from(texel);
        let per_row = u64::from(self.bytes_per_row);
        if per_row < row
            || !per_row.is_multiple_of(u64::from(texel))
            || self.rows_per_image < self.extent.height
            || !self.buffer_offset.is_multiple_of(u64::from(texel))
        {
            return Err(Error::Misaligned);
        }
        let slices = u64::from(self.extent.depth) * u64::from(self.image.layers);
        let per_slice = per_row * u64::from(self.rows_per_image);
        (slices - 1)
            .checked_mul(per_slice)
            .and_then(|before| before.checked_add(u64::from(self.extent.height - 1) * per_row))
            .and_then(|before| before.checked_add(row))
            .ok_or(Error::OutOfRange)
    }
}

/// One region a copy between two images moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageCopy {
    /// The source's level, layers and plane.
    pub src: ImageSubresource,
    /// Where the region starts in the source.
    pub src_origin: Origin3d,
    /// The destination's level, layers and plane.
    pub dst: ImageSubresource,
    /// Where the region lands in the destination.
    pub dst_origin: Origin3d,
    /// Its size.
    pub extent: Extent3d,
}

impl ImageCopy {
    /// Refuse a region past either image, of a plane either lacks, or
    /// between different layer counts.
    pub(crate) fn check(&self, src: &ImageDesc, dst: &ImageDesc) -> Result<(), Error> {
        if self.src.layers != self.dst.layers || self.src.aspect != self.dst.aspect {
            return Err(Error::Shape);
        }
        self.src.check_region(src, self.src_origin, self.extent)?;
        self.dst.check_region(dst, self.dst_origin, self.extent)
    }
}

/// What a clear writes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ClearValue {
    /// A colour of a float, unsigned normalised or signed normalised format.
    Float([f32; 4]),
    /// A colour of a signed integer format.
    Sint([i32; 4]),
    /// A colour of an unsigned integer format.
    Uint([u32; 4]),
    /// A depth, in `0.0..=1.0`, and a stencil value, either written only
    /// where the clear's range names its aspect.
    DepthStencil {
        /// The depth.
        depth: f32,
        /// The stencil value.
        stencil: u8,
    },
}

impl ClearValue {
    /// Refuse a value of another kind than `format` holds, or a depth outside
    /// `0.0..=1.0`.
    pub(crate) fn check(&self, format: Format) -> Result<(), Error> {
        let colour = format.aspects() == Aspects::COLOR;
        let fits = match *self {
            Self::Float(_) => colour && format.component() == Component::Float,
            Self::Sint(_) => colour && format.component() == Component::Sint,
            Self::Uint(_) => colour && format.component() == Component::Uint,
            Self::DepthStencil { depth, .. } => !colour && (0.0..=1.0).contains(&depth),
        };
        if fits {
            Ok(())
        } else {
            Err(Error::Shape)
        }
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;

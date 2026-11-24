//! The renderer's images, views, samplers and texel buffers: each made only
//! as the host's valid usage allows, anything else fatal to its context; an
//! image's texels kept in its memory as this host lays them; and every copy,
//! clear and layout change an image takes carried out there. A clear writes
//! the reference's encoding of its value, and a dispatch reaches images
//! through the reference's copies of them.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use tairix_gpu::{
    AddressMode, AllocationDesc, Aspects, BorderColor, BufferDesc, BufferImageCopy, BufferUsage,
    BufferViewDesc, ClearValue, CompareOp, ContextDesc, DescriptorType, Extent3d, Filter, Format,
    FormatFeatures, ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage,
    ImageViewDesc, LayoutBinding, Origin3d, SampleType, SamplerDesc, SamplerType, Submission,
    ViewDimension,
};
use tairix_spirv::{Access, BindingKind, Component};

use super::{Entries, Host, HostState, Object, Offered, Words, SECOND};
use crate::three_d::Context;
use crate::venus::encode::{
    BufferImageRegion, ImageRegion, Layers, LayoutEntry, SamplerInfo, Subresources, ViewInfo,
    Written,
};
use crate::venus::info::host_format;
use crate::venus::reply::{HostFormatFeatures, HostImageLimits};
use crate::venus::testing::{format_reply, image_format_reply, Grammar};
use crate::venus::{
    api_version, aspect, descriptor, extension, format_feature, image, queue, Command, Structure,
};

/// What every image's memory must be aligned to, wider than a buffer's.
const IMAGE_ALIGNMENT: u64 = 512;

/// Where each plane's level starts in an image: a copy's start is whole
/// texels and, for depth or stencil, whole words.
const BLOCK_ALIGNMENT: u64 = 16;

/// The host's alignment of a texel buffer's offset and its most texels, as
/// its limits state them.
const TEXEL_OFFSET_ALIGNMENT: u64 = 16;
const MAX_TEXELS: u64 = 1 << 27;

/// What an image's texels read as once it leaves the undefined layout.
const UNDEFINED_TEXEL: u8 = 0x5a;

/// The place of `samplerAnisotropy` among the core features.
const SAMPLER_ANISOTROPY: usize = 19;

/// The commands this module carries out.
pub(super) const COMMANDS: [i32; 18] = [
    Command::GetPhysicalDeviceFormatProperties2 as i32,
    Command::GetPhysicalDeviceImageFormatProperties2 as i32,
    Command::CreateImage as i32,
    Command::GetDeviceImageMemoryRequirements as i32,
    Command::GetImageMemoryRequirements2 as i32,
    Command::BindImageMemory2 as i32,
    Command::CreateImageView as i32,
    Command::CreateSampler as i32,
    Command::CreateBufferView as i32,
    Command::CmdCopyBufferToImage as i32,
    Command::CmdCopyImageToBuffer as i32,
    Command::CmdCopyImage as i32,
    Command::CmdClearColorImage as i32,
    Command::CmdClearDepthStencilImage as i32,
    Command::DestroyImage as i32,
    Command::DestroyImageView as i32,
    Command::DestroySampler as i32,
    Command::DestroyBufferView as i32,
];

/// What a command carried out answers.
pub(super) enum Reply {
    /// Nothing.
    Silent,
    /// This reply, written at the context's reply position.
    Written(Vec<u8>),
}

/// An image as the renderer keeps it.
#[derive(Clone, Debug)]
pub(super) struct HostImage {
    pub(super) flags: u32,
    pub(super) kind: i32,
    pub(super) format: i32,
    pub(super) extent: [u32; 3],
    pub(super) levels: u32,
    pub(super) layers: u32,
    /// One, or four: a target whose samples this host keeps only within the
    /// rendering instance drawing them.
    pub(super) samples: u32,
    pub(super) usage: u32,
    /// The formats its views may read it as, where it lists them.
    pub(super) view_formats: Vec<i32>,
    /// Its memory and where in it, once bound.
    pub(super) bound: Option<(u64, u64)>,
    /// Whether it has left the undefined layout for the general one.
    pub(super) general: bool,
}

/// A texel buffer as the renderer keeps it.
#[derive(Copy, Clone, Debug)]
pub(super) struct HostTexels {
    pub(super) buffer: u64,
    pub(super) format: i32,
    pub(super) offset: u64,
    pub(super) range: u64,
}

/// An image command a command buffer recorded.
pub(super) enum ImageCommand {
    /// The image taken from the undefined layout to the general one.
    Kept(u64),
    ToImage {
        buffer: u64,
        image: u64,
        regions: Vec<BufferImageRegion>,
    },
    ToBuffer {
        image: u64,
        buffer: u64,
        regions: Vec<BufferImageRegion>,
    },
    Copy {
        src: u64,
        dst: u64,
        regions: Vec<ImageRegion>,
    },
    ClearColour {
        image: u64,
        tag: u32,
        bits: [u32; 4],
        ranges: Vec<Subresources>,
    },
    ClearDepth {
        image: u64,
        depth: f32,
        stencil: u32,
        ranges: Vec<Subresources>,
    },
}

/// The seam's format the host's `format` is, as the driver names formats.
pub(super) fn seam_format(format: i32) -> Option<Format> {
    Format::ALL
        .into_iter()
        .find(|&seam| host_format(seam) == format)
}

/// The host's planes, by its aspect bit, with the seam's.
const PLANES: [(u32, Aspects); 3] = [
    (aspect::COLOR, Aspects::COLOR),
    (aspect::DEPTH, Aspects::DEPTH),
    (aspect::STENCIL, Aspects::STENCIL),
];

/// The seam's planes of the host's `aspects`.
fn seam_aspects(aspects: u32) -> Aspects {
    PLANES
        .iter()
        .filter(|&&(host, _)| aspects & host != 0)
        .fold(Aspects::NONE, |all, &(_, seam)| all.with(seam))
}

/// What this host offers each format: everything the seam's encoding of it
/// means, but a depth of 24 bits, of which it renders no depth or stencil.
pub(super) fn offered_formats() -> BTreeMap<i32, HostFormatFeatures> {
    Format::ALL
        .into_iter()
        .filter(|&format| format != Format::Depth24PlusStencil8)
        .map(|format| (host_format(format), offers(format)))
        .collect()
}

/// Whether a host offering `offered` states Vulkan 1.3's format features,
/// to an instance of that version.
fn states_format_features(offered: &Offered) -> bool {
    offered.api_version >= api_version(3)
        || offered
            .extensions
            .iter()
            .any(|name| name == extension::FORMAT_FEATURE_FLAGS2)
}

/// What a Vulkan 1.3 host offers `format`: what the seam means of it, its
/// depth compared wherever it is sampled.
fn offers(format: Format) -> HostFormatFeatures {
    let features = format.features();
    let has = |feature| features.contains(feature);
    let mut optimal = format_feature::TRANSFER_SRC | format_feature::TRANSFER_DST;
    for (seam, host) in [
        (FormatFeatures::SAMPLED, format_feature::SAMPLED_IMAGE),
        (
            FormatFeatures::FILTERED,
            format_feature::SAMPLED_IMAGE_FILTER_LINEAR,
        ),
        (FormatFeatures::STORAGE, format_feature::STORAGE_IMAGE),
        (
            FormatFeatures::STORAGE_READ_WITHOUT_FORMAT,
            format_feature::STORAGE_READ_WITHOUT_FORMAT,
        ),
        (
            FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT,
            format_feature::STORAGE_WRITE_WITHOUT_FORMAT,
        ),
        (
            FormatFeatures::COLOR_ATTACHMENT,
            format_feature::COLOR_ATTACHMENT,
        ),
        (
            FormatFeatures::BLENDED,
            format_feature::COLOR_ATTACHMENT_BLEND,
        ),
        (
            FormatFeatures::DEPTH_STENCIL,
            format_feature::DEPTH_STENCIL_ATTACHMENT,
        ),
    ] {
        if has(seam) {
            optimal |= host;
        }
    }
    if format.aspects().contains(Aspects::DEPTH) && has(FormatFeatures::SAMPLED) {
        optimal |= format_feature::SAMPLED_IMAGE_DEPTH_COMPARISON;
    }
    let mut buffer = 0;
    for (seam, host) in [
        (
            FormatFeatures::UNIFORM_TEXEL_BUFFER,
            format_feature::UNIFORM_TEXEL_BUFFER,
        ),
        (
            FormatFeatures::STORAGE_TEXEL_BUFFER,
            format_feature::STORAGE_TEXEL_BUFFER,
        ),
        (
            FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT,
            format_feature::STORAGE_READ_WITHOUT_FORMAT,
        ),
        (
            FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT,
            format_feature::STORAGE_WRITE_WITHOUT_FORMAT,
        ),
        (FormatFeatures::VERTEX_INPUT, format_feature::VERTEX_BUFFER),
    ] {
        if has(seam) {
            buffer |= host;
        }
    }
    HostFormatFeatures { optimal, buffer }
}

/// Each image use and the format feature it needs.
const USES: [(u32, u64); 6] = [
    (image::TRANSFER_SRC, format_feature::TRANSFER_SRC),
    (image::TRANSFER_DST, format_feature::TRANSFER_DST),
    (image::SAMPLED, format_feature::SAMPLED_IMAGE),
    (image::STORAGE, format_feature::STORAGE_IMAGE),
    (image::COLOR_ATTACHMENT, format_feature::COLOR_ATTACHMENT),
    (
        image::DEPTH_STENCIL_ATTACHMENT,
        format_feature::DEPTH_STENCIL_ATTACHMENT,
    ),
];

/// Whether `features` serve every use `usage` names, and it names only uses.
fn serves(features: u64, usage: u32) -> bool {
    let named = USES.iter().fold(0, |all, &(usage, _)| all | usage);
    usage & !named == 0
        && USES
            .iter()
            .all(|&(asked, needed)| usage & asked == 0 || features & needed == needed)
}

/// The most an image of `format`, `kind`, `usage` and `flags` may be, where
/// the host makes one.
fn most(
    offered: &Offered,
    (format, kind, usage, flags): (i32, i32, u32, u32),
) -> Option<HostImageLimits> {
    let features = offered.formats.get(&format)?.optimal;
    let flagged = flags & !(image::MUTABLE_FORMAT | image::CUBE_COMPATIBLE) == 0
        && (flags & image::CUBE_COMPATIBLE == 0
            || (kind == 1 && !offered.uncubed.contains(&format)));
    (usage != 0 && serves(features, usage) && flagged).then_some(())?;
    let side = offered.image_extent;
    let (extent, layers) = match kind {
        0 => ([side, 1, 1], 2048),
        1 => ([side, side, 1], 2048),
        2 => ([side.min(2048); 3], 1),
        _ => return None,
    };
    // Four samples of a two-dimensional target no shader stores to, of a
    // format the seam multisamples.
    let target = usage & (image::COLOR_ATTACHMENT | image::DEPTH_STENCIL_ATTACHMENT) != 0;
    let four = kind == 1
        && target
        && usage & image::STORAGE == 0
        && seam_format(format)
            .is_some_and(|seam| seam.features().contains(FormatFeatures::MULTISAMPLED));
    Some(HostImageLimits {
        extent,
        levels: u32::BITS - side.leading_zeros(),
        layers,
        samples: if four { 1 | 4 } else { 1 },
    })
}

/// Whether formats `a` and `b` are of one texel size, as a view of one may
/// read an image of the other.
fn compatible(a: i32, b: i32) -> bool {
    let bytes = |format| seam_format(format)?.copy_bytes(Aspects::COLOR);
    bytes(a).is_some() && bytes(a) == bytes(b)
}

impl HostImage {
    /// Its planes: the host's aspect bit and the bytes a texel takes in it.
    fn planes(&self) -> Option<Vec<(u32, u32)>> {
        let seam = seam_format(self.format)?;
        PLANES
            .iter()
            .filter(|&&(_, plane)| seam.aspects().contains(plane))
            .map(|&(host, plane)| Some((host, seam.copy_bytes(plane)?)))
            .collect()
    }

    /// The host's aspect bits of every plane it holds.
    fn aspects(&self) -> Option<u32> {
        Some(
            self.planes()?
                .iter()
                .fold(0, |all, &(plane, _)| all | plane),
        )
    }

    /// The bytes a texel of plane `aspect` takes, where it holds that one
    /// plane.
    fn plane_bytes(&self, aspect: u32) -> Option<u32> {
        self.planes()?
            .into_iter()
            .find(|&(plane, _)| plane == aspect)
            .map(|(_, bytes)| bytes)
    }

    /// The extent of level `level`.
    fn level(&self, level: u32) -> [u32; 3] {
        let halved = |side: u32| side.checked_shr(level).unwrap_or(0).max(1);
        let [width, height, depth] = self.extent;
        match self.kind {
            0 => [halved(width), 1, 1],
            1 => [halved(width), halved(height), 1],
            _ => [halved(width), halved(height), halved(depth)],
        }
    }

    /// Each plane's levels as they lie: its aspect, the level, where it
    /// starts and the bytes a texel takes.
    fn blocks(&self) -> Option<Vec<(u32, u32, u64, u32)>> {
        let mut blocks = Vec::new();
        let mut at = 0u64;
        for (plane, bytes) in self.planes()? {
            for level in 0..self.levels {
                let [width, height, depth] = self.level(level);
                blocks.push((plane, level, at, bytes));
                let texels = u64::from(width)
                    * u64::from(height)
                    * u64::from(depth)
                    * u64::from(self.layers);
                at += (texels * u64::from(bytes)).next_multiple_of(BLOCK_ALIGNMENT);
            }
        }
        Some(blocks)
    }

    /// The bytes it takes in its memory.
    fn size(&self) -> Option<u64> {
        let (_, level, at, bytes) = *self.blocks()?.last()?;
        let [width, height, depth] = self.level(level);
        let last = u64::from(width)
            * u64::from(height)
            * u64::from(depth)
            * u64::from(self.layers)
            * u64::from(bytes);
        Some(at + last.next_multiple_of(BLOCK_ALIGNMENT))
    }

    /// Where texel `[x, y, z]` of layer `layer` of plane `aspect`'s level
    /// `level` lies from the image's start.
    fn texel_at(&self, aspect: u32, level: u32, layer: u32, [x, y, z]: [u32; 3]) -> Option<u64> {
        let &(_, _, start, bytes) = self
            .blocks()?
            .iter()
            .find(|&&(plane, at_level, _, _)| plane == aspect && at_level == level)?;
        let [width, height, depth] = self.level(level);
        let slice = u64::from(width) * u64::from(height);
        let texel = (u64::from(layer) * u64::from(depth) + u64::from(z)) * slice
            + u64::from(y) * u64::from(width)
            + u64::from(x);
        Some(start + texel * u64::from(bytes))
    }

    /// Every plane, level and layer of it.
    pub(super) fn whole(&self) -> Option<Subresources> {
        Some(Subresources {
            aspects: self.aspects()?,
            base_level: 0,
            levels: self.levels,
            base_layer: 0,
            layers: self.layers,
        })
    }

    /// Where a copy's region of `layers` starting `at` and `extent` large
    /// starts in its level, where it holds the region.
    fn within(&self, layers: &Layers, at: [i32; 3], extent: [u32; 3]) -> Option<[u32; 3]> {
        self.plane_bytes(layers.aspect)?;
        let held = layers.level < self.levels
            && layers.count >= 1
            && layers.base_layer.checked_add(layers.count)? <= self.layers;
        held.then_some(())?;
        let level = self.level(layers.level);
        let mut origin = [0; 3];
        for axis in 0..3 {
            let start = u32::try_from(at[axis]).ok()?;
            (extent[axis] >= 1 && start.checked_add(extent[axis])? <= level[axis]).then_some(())?;
            origin[axis] = start;
        }
        Some(origin)
    }

    /// Each row a copy between it and a buffer of `buffer_size` bytes moves:
    /// where it lies in the buffer, where in the image, and its bytes.
    fn rows(&self, region: &BufferImageRegion, buffer_size: u64) -> Option<Vec<(u64, u64, usize)>> {
        let layers = &region.layers;
        let bytes = self.plane_bytes(layers.aspect)?;
        let start = if layers.aspect == aspect::COLOR {
            u64::from(bytes)
        } else {
            4
        };
        let origin = self.within(layers, region.at, region.extent)?;
        let [width, height, depth] = region.extent;
        let row_length = if region.row_length == 0 {
            width
        } else {
            region.row_length
        };
        let image_height = if region.image_height == 0 {
            height
        } else {
            region.image_height
        };
        let laid_out =
            region.offset.is_multiple_of(start) && row_length >= width && image_height >= height;
        laid_out.then_some(())?;
        let row = u64::from(width) * u64::from(bytes);
        let mut rows = Vec::new();
        for layer in 0..layers.count {
            for z in 0..depth {
                for y in 0..height {
                    let buffer_row = (u64::from(layer) * u64::from(depth) + u64::from(z))
                        * u64::from(image_height)
                        + u64::from(y);
                    let buffer_at = region
                        .offset
                        .checked_add(buffer_row * u64::from(row_length) * u64::from(bytes))?;
                    (buffer_at.checked_add(row)? <= buffer_size).then_some(())?;
                    let image_at = self.texel_at(
                        layers.aspect,
                        layers.level,
                        layers.base_layer + layer,
                        [origin[0], origin[1] + y, origin[2] + z],
                    )?;
                    rows.push((buffer_at, image_at, usize::try_from(row).ok()?));
                }
            }
        }
        Some(rows)
    }

    /// Each row a copy of `region` from it into `to` moves: where it lies in
    /// each, and its bytes.
    fn rows_into(&self, to: &Self, region: &ImageRegion) -> Option<Vec<(u64, u64, usize)>> {
        let ((src, src_at), (dst, dst_at)) = (region.src, region.dst);
        let bytes = self.plane_bytes(src.aspect)?;
        (to.plane_bytes(dst.aspect)? == bytes && src.count == dst.count).then_some(())?;
        let read = self.within(&src, src_at, region.extent)?;
        let written = to.within(&dst, dst_at, region.extent)?;
        let [width, height, depth] = region.extent;
        let len = usize::try_from(u64::from(width) * u64::from(bytes)).ok()?;
        let mut rows = Vec::new();
        for layer in 0..src.count {
            for z in 0..depth {
                for y in 0..height {
                    let read_at = self.texel_at(
                        src.aspect,
                        src.level,
                        src.base_layer + layer,
                        [read[0], read[1] + y, read[2] + z],
                    )?;
                    let written_at = to.texel_at(
                        dst.aspect,
                        dst.level,
                        dst.base_layer + layer,
                        [written[0], written[1] + y, written[2] + z],
                    )?;
                    rows.push((read_at, written_at, len));
                }
            }
        }
        Some(rows)
    }

    /// Refuse what the host's valid usage does not let it make.
    /// Whether its format, kind, uses, flags and listed formats were asked
    /// of the host: Vulkan holds an image to the limits a query with an
    /// equivalent chain answered.
    fn queried(&self, state: &HostState) -> Option<()> {
        let asked = (self.format, self.kind, self.usage, self.flags);
        state
            .queried
            .iter()
            .any(|(format, kind, usage, flags, listed)| {
                (*format, *kind, *usage, *flags) == asked && *listed == self.view_formats
            })
            .then_some(())
    }

    fn valid(&self, offered: &Offered) -> Option<()> {
        let most = most(offered, (self.format, self.kind, self.usage, self.flags))?;
        let [width, height, depth] = self.extent;
        let shaped = match self.kind {
            0 => height == 1 && depth == 1,
            1 => depth == 1,
            _ => self.layers == 1,
        };
        let within = self
            .extent
            .iter()
            .zip(&most.extent)
            .all(|(&side, &most)| (1..=most).contains(&side))
            && (1..=most.layers).contains(&self.layers)
            && (1..=u32::BITS - width.max(height).max(depth).leading_zeros())
                .contains(&self.levels);
        let cube =
            self.flags & image::CUBE_COMPATIBLE == 0 || (width == height && self.layers >= 6);
        let listed = match self.view_formats.as_slice() {
            [] => true,
            [only] => *only == self.format,
            formats => {
                self.flags & image::MUTABLE_FORMAT != 0
                    && formats.contains(&self.format)
                    && formats
                        .iter()
                        .all(|&viewed| compatible(self.format, viewed))
            }
        };
        let sampled = most.samples & self.samples != 0
            && (self.samples == 1 || (self.kind == 1 && self.levels == 1));
        (shaped && within && cube && listed && sampled && self.size().is_some()).then_some(())
    }
}

/// A plane, a level and a run of layers, as a copy names them.
fn layers(words: &mut Words<'_>) -> Option<Layers> {
    Some(Layers {
        aspect: words.u32()?,
        level: words.u32()?,
        base_layer: words.u32()?,
        count: words.u32()?,
    })
}

fn at(words: &mut Words<'_>) -> Option<[i32; 3]> {
    Some([words.i32()?, words.i32()?, words.i32()?])
}

fn extent(words: &mut Words<'_>) -> Option<[u32; 3]> {
    Some([words.u32()?, words.u32()?, words.u32()?])
}

fn subresources(words: &mut Words<'_>) -> Option<Subresources> {
    Some(Subresources {
        aspects: words.u32()?,
        base_level: words.u32()?,
        levels: words.u32()?,
        base_layer: words.u32()?,
        layers: words.u32()?,
    })
}

/// The image a create info describes, read from its type to its first
/// layout.
fn described(words: &mut Words<'_>) -> Option<HostImage> {
    (words.i32()? == Structure::ImageCreateInfo as i32).then_some(())?;
    let view_formats = listed_formats(words)?;
    let flags = words.u32()?;
    let (kind, format) = (words.i32()?, words.i32()?);
    let extent = extent(words)?;
    let (levels, layers) = (words.u32()?, words.u32()?);
    let samples = words.u32()?;
    (words.i32()? == image::OPTIMAL).then_some(())?;
    let usage = words.u32()?;
    // Exclusive, naming no queue family.
    (words.i32()? == 0).then_some(())?;
    words.word(0)?;
    words.is(0)?;
    (words.i32()? == image::UNDEFINED).then_some(())?;
    Some(HostImage {
        flags,
        kind,
        format,
        extent,
        levels,
        layers,
        samples,
        usage,
        view_formats,
        bound: None,
        general: false,
    })
}

/// The formats a structure's chain lists an image as read as: none where it
/// chains nothing.
fn listed_formats(words: &mut Words<'_>) -> Option<Vec<i32>> {
    match words.chain()?.as_slice() {
        [] => Some(Vec::new()),
        [list] if *list == Structure::ImageFormatListCreateInfo as i32 => {
            let listed = counted(words, Words::i32)?;
            (!listed.is_empty() && listed.len() <= 8).then_some(listed)
        }
        _ => None,
    }
}

/// The reply of `command` asking what `image` needs of its memory.
fn requirements(command: i32, image: &HostImage) -> Option<Reply> {
    let size = image.size()?.next_multiple_of(IMAGE_ALIGNMENT);
    let reply = Grammar::default()
        .i32(command)
        .u64(1)
        .structure(Structure::MemoryRequirements2)
        .u64(size)
        .u64(IMAGE_ALIGNMENT)
        .u32(0b11);
    Some(Reply::Written(reply.0))
}

/// A count, then that many items each `item` reads.
fn counted<'w, T>(
    words: &mut Words<'w>,
    item: impl Fn(&mut Words<'w>) -> Option<T>,
) -> Option<Vec<T>> {
    let count = words.u32()?;
    words.is(u64::from(count))?;
    (0..count).map(|_| item(words)).collect()
}

/// The layout an image is used in, which must be the general one.
fn general(words: &mut Words<'_>) -> Option<()> {
    (words.i32()? == image::GENERAL).then_some(())
}

fn buffer_image_region(words: &mut Words<'_>) -> Option<BufferImageRegion> {
    Some(BufferImageRegion {
        offset: words.u64()?,
        row_length: words.u32()?,
        image_height: words.u32()?,
        layers: layers(words)?,
        at: at(words)?,
        extent: extent(words)?,
    })
}

/// Whether the family runs graphics work: the only one that clears depth and
/// stencil, or copies them to and from buffers.
fn runs_graphics(offered: &Offered) -> Option<()> {
    (offered.family_flags & queue::GRAPHICS != 0).then_some(())
}

/// Whether the family copies the plane of each of `regions`.
fn planes_copied(offered: &Offered, regions: &[BufferImageRegion]) -> Option<()> {
    if regions
        .iter()
        .all(|region| region.layers.aspect == aspect::COLOR)
    {
        return Some(());
    }
    runs_graphics(offered)
}

fn image_region(words: &mut Words<'_>) -> Option<ImageRegion> {
    Some(ImageRegion {
        src: (layers(words)?, at(words)?),
        dst: (layers(words)?, at(words)?),
        extent: extent(words)?,
    })
}

impl Host {
    fn image(state: &HostState, context: Context, id: u64) -> Option<&HostImage> {
        match state.contexts.get(context)?.objects.get(&id)? {
            Object::Image(image) => Some(image),
            _ => None,
        }
    }

    fn image_mut(state: &mut HostState, context: Context, id: u64) -> Option<&mut HostImage> {
        match state.contexts.get_mut(context)?.objects.get_mut(&id)? {
            Object::Image(image) => Some(image),
            _ => None,
        }
    }

    /// Image `id`, bound and in the general layout, made for `usage`, and
    /// where it lies.
    fn usable(
        state: &HostState,
        context: Context,
        id: u64,
        usage: u32,
    ) -> Option<(HostImage, (u64, u64))> {
        let image = Self::image(state, context, id)?;
        let place = image.bound?;
        (image.general && image.usage & usage == usage).then_some((image.clone(), place))
    }

    /// The bytes of buffers `command` reads or writes: each buffer, where
    /// the bytes start, how many, and whether written.
    pub(super) fn image_buffers(
        state: &HostState,
        context: Context,
        command: &ImageCommand,
    ) -> Option<Vec<(u64, u64, u64, bool)>> {
        let (buffer, image, regions, write) = match command {
            ImageCommand::ToImage {
                buffer,
                image,
                regions,
            } => (*buffer, *image, regions, false),
            ImageCommand::ToBuffer {
                image,
                buffer,
                regions,
            } => (*buffer, *image, regions, true),
            _ => return Some(Vec::new()),
        };
        let host = Self::image(state, context, image)?;
        let (_, size) = Self::bound(state, context, buffer)?;
        let mut spans = Vec::new();
        for region in regions {
            for (at, _, len) in host.rows(region, size)? {
                spans.push((buffer, at, u64::try_from(len).ok()?, write));
            }
        }
        Some(spans)
    }

    /// Carry out `command`, one of [`COMMANDS`] read up to its flags: `None`
    /// off the host's grammar or valid usage, else what it answers.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command the backend sends"
    )]
    pub(super) fn run_image_command(
        state: &mut HostState,
        context: Context,
        words: &mut Words<'_>,
        command: i32,
    ) -> Option<Reply> {
        let made = |command: Command, id: u64| Some(Reply::Written(Self::made(command, id)));
        match command {
            c if c == Command::GetPhysicalDeviceFormatProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                let format = words.i32()?;
                words.is(1)?;
                (words.i32()? == Structure::FormatProperties2 as i32).then_some(())?;
                let chained = match words.u64()? {
                    0 => false,
                    1 => {
                        words.structure(Structure::FormatProperties3)?;
                        true
                    }
                    _ => return None,
                };
                // Vulkan 1.3's format features are asked only of a host
                // stating them.
                (!chained || states_format_features(&state.offered)).then_some(())?;
                let offered = state.offered.formats.get(&format).copied();
                Some(Reply::Written(
                    format_reply(offered.unwrap_or_default(), chained).0,
                ))
            }
            c if c == Command::GetPhysicalDeviceImageFormatProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::PhysicalDeviceImageFormatInfo2 as i32).then_some(())?;
                let listed = listed_formats(words)?;
                let (format, kind) = (words.i32()?, words.i32()?);
                (words.i32()? == image::OPTIMAL).then_some(())?;
                let (usage, flags) = (words.u32()?, words.u32()?);
                words.is(1)?;
                words.structure(Structure::ImageFormatProperties2)?;
                state.queried.push((format, kind, usage, flags, listed));
                let reply = match most(&state.offered, (format, kind, usage, flags)) {
                    Some(most) => image_format_reply(0, most),
                    // VK_ERROR_FORMAT_NOT_SUPPORTED, the properties zeroed.
                    None => image_format_reply(
                        -11,
                        HostImageLimits {
                            extent: [0; 3],
                            levels: 0,
                            layers: 0,
                            samples: 0,
                        },
                    ),
                };
                Some(Reply::Written(reply.0))
            }
            c if c == Command::CreateImage as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                let image = described(words)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                image.valid(&state.offered)?;
                image.queried(state)?;
                Self::insert(state, context, id, Object::Image(image))?;
                made(Command::CreateImage, id)
            }
            // A host before Vulkan 1.3 has no such entry point.
            c if c == Command::GetDeviceImageMemoryRequirements as i32
                && state.offered.api_version >= api_version(3) =>
            {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::DeviceImageMemoryRequirements)?;
                words.is(1)?;
                let image = described(words)?;
                // No plane of a disjoint image.
                (words.i32()? == 0).then_some(())?;
                words.is(1)?;
                words.structure(Structure::MemoryRequirements2)?;
                image.valid(&state.offered)?;
                image.queried(state)?;
                state.described += 1;
                requirements(command, &image)
            }
            c if c == Command::GetImageMemoryRequirements2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::ImageMemoryRequirementsInfo2)?;
                let id = words.u64()?;
                words.is(1)?;
                words.structure(Structure::MemoryRequirements2)?;
                requirements(command, Self::image(state, context, id)?)
            }
            c if c == Command::BindImageMemory2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.word(1)?;
                words.is(1)?;
                words.structure(Structure::BindImageMemoryInfo)?;
                let (id, memory, offset) = (words.u64()?, words.u64()?, words.u64()?);
                let len = u64::try_from(Self::memory_len(state, context, memory)?).ok()?;
                let image = Self::image_mut(state, context, id)?;
                let fits = image.bound.is_none()
                    && offset.is_multiple_of(IMAGE_ALIGNMENT)
                    && offset.checked_add(image.size()?)? <= len;
                fits.then_some(())?;
                image.bound = Some((memory, offset));
                Some(Reply::Written(Grammar::default().i32(command).i32(0).0))
            }
            c if c == Command::CreateImageView as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::ImageViewCreateInfo as i32).then_some(())?;
                (words.chain()? == [Structure::ImageViewUsageCreateInfo as i32]).then_some(())?;
                let usage = words.u32()?;
                words.word(0)?;
                let image = words.u64()?;
                let (kind, format) = (words.i32()?, words.i32()?);
                for _ in 0..4 {
                    // Each channel its own.
                    (words.i32()? == 0).then_some(())?;
                }
                let range = subresources(words)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                let view = ViewInfo {
                    image,
                    kind,
                    format,
                    range,
                    usage,
                };
                Self::viewable(state, context, &view)?;
                Self::insert(state, context, id, Object::View(view))?;
                if state.misbehaves == super::Misbehaviour::GarbleViews {
                    return made(Command::CreateImage, id);
                }
                made(Command::CreateImageView, id)
            }
            c if c == Command::CreateSampler as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::SamplerCreateInfo)?;
                words.word(0)?;
                let filters = [words.i32()?, words.i32()?, words.i32()?];
                let address = [words.i32()?, words.i32()?, words.i32()?];
                let lod_bias = words.f32()?;
                let anisotropic = words.boolean()?;
                let anisotropy = words.f32()?;
                let compares = words.boolean()?;
                let compare = words.i32()?;
                let lod = [words.f32()?, words.f32()?];
                let border = words.i32()?;
                let unnormalized = words.boolean()?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                let sampler = SamplerInfo {
                    filters,
                    address,
                    lod_bias,
                    anisotropy: anisotropic.then_some(anisotropy),
                    compare: compares.then_some(compare),
                    lod,
                    border,
                    unnormalized,
                };
                Self::samplable(state, &sampler)?;
                // The integer borders are the odd ones.
                let integer = sampler.border % 2 == 1;
                if integer && state.misbehaves == super::Misbehaviour::RefuseIntegerBorders {
                    let refused = Grammar::default().i32(command).i32(-1).u64(1).u64(id);
                    return Some(Reply::Written(refused.0));
                }
                Self::insert(state, context, id, Object::Sampler(sampler))?;
                made(Command::CreateSampler, id)
            }
            c if c == Command::CreateBufferView as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::BufferViewCreateInfo)?;
                words.word(0)?;
                let buffer = words.u64()?;
                let format = words.i32()?;
                let (offset, range) = (words.u64()?, words.u64()?);
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                let (_, size) = Self::bound(state, context, buffer)?;
                let offered = state.offered.formats.get(&format)?.buffer;
                let texel = u64::from(seam_format(format)?.copy_bytes(Aspects::COLOR)?);
                // The buffer is made for both texel uses, so the format offers
                // both.
                let texels =
                    format_feature::UNIFORM_TEXEL_BUFFER | format_feature::STORAGE_TEXEL_BUFFER;
                let valid = offered & texels == texels
                    && offset.is_multiple_of(TEXEL_OFFSET_ALIGNMENT)
                    && range > 0
                    && range.is_multiple_of(texel)
                    && range / texel <= MAX_TEXELS
                    && offset.checked_add(range)? <= size;
                valid.then_some(())?;
                let held = HostTexels {
                    buffer,
                    format,
                    offset,
                    range,
                };
                Self::insert(state, context, id, Object::Texels(held))?;
                made(Command::CreateBufferView, id)
            }
            c if c == Command::CmdCopyBufferToImage as i32 => {
                let commands = words.u64()?;
                let (buffer, image) = (words.u64()?, words.u64()?);
                general(words)?;
                let regions = counted(words, buffer_image_region)?;
                planes_copied(&state.offered, &regions)?;
                let copy = ImageCommand::ToImage {
                    buffer,
                    image,
                    regions,
                };
                Self::record(state, context, commands, super::Recorded::Image(copy))?;
                Some(Reply::Silent)
            }
            c if c == Command::CmdCopyImageToBuffer as i32 => {
                let commands = words.u64()?;
                let image = words.u64()?;
                general(words)?;
                let buffer = words.u64()?;
                let regions = counted(words, buffer_image_region)?;
                planes_copied(&state.offered, &regions)?;
                let copy = ImageCommand::ToBuffer {
                    image,
                    buffer,
                    regions,
                };
                Self::record(state, context, commands, super::Recorded::Image(copy))?;
                Some(Reply::Silent)
            }
            c if c == Command::CmdCopyImage as i32 => {
                let commands = words.u64()?;
                let src = words.u64()?;
                general(words)?;
                let dst = words.u64()?;
                general(words)?;
                let regions = counted(words, image_region)?;
                let copy = ImageCommand::Copy { src, dst, regions };
                Self::record(state, context, commands, super::Recorded::Image(copy))?;
                Some(Reply::Silent)
            }
            c if c == Command::CmdClearColorImage as i32 => {
                let commands = words.u64()?;
                let image = words.u64()?;
                general(words)?;
                words.is(1)?;
                let tag = words.u32()?;
                words.is(4)?;
                let bits = [words.u32()?, words.u32()?, words.u32()?, words.u32()?];
                let ranges = counted(words, subresources)?;
                // Vulkan leaves an integer value its channels cannot hold
                // undefined.
                let format = seam_format(Self::image(state, context, image)?.format)?;
                let held = |value: ClearValue| (value.held(format) == value).then_some(());
                match tag {
                    1 => held(ClearValue::Sint(bits.map(u32::cast_signed)))?,
                    2 => held(ClearValue::Uint(bits))?,
                    _ => {}
                }
                let clear = ImageCommand::ClearColour {
                    image,
                    tag,
                    bits,
                    ranges,
                };
                Self::record(state, context, commands, super::Recorded::Image(clear))?;
                Some(Reply::Silent)
            }
            c if c == Command::CmdClearDepthStencilImage as i32 => {
                runs_graphics(&state.offered)?;
                let commands = words.u64()?;
                let image = words.u64()?;
                general(words)?;
                words.is(1)?;
                let (depth, stencil) = (words.f32()?, words.u32()?);
                let ranges = counted(words, subresources)?;
                let clear = ImageCommand::ClearDepth {
                    image,
                    depth,
                    stencil,
                    ranges,
                };
                Self::record(state, context, commands, super::Recorded::Image(clear))?;
                Some(Reply::Silent)
            }
            c if c == Command::DestroyImage as i32
                || c == Command::DestroyImageView as i32
                || c == Command::DestroySampler as i32
                || c == Command::DestroyBufferView as i32 =>
            {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                let objects = &mut state.contexts.get_mut(context)?.objects;
                let kind = match objects.get(&id)? {
                    Object::Image(_) => Command::DestroyImage,
                    Object::View(_) => Command::DestroyImageView,
                    Object::Sampler(_) => Command::DestroySampler,
                    Object::Texels(_) => Command::DestroyBufferView,
                    _ => return None,
                };
                (kind as i32 == c).then_some(())?;
                objects.remove(&id);
                Some(Reply::Silent)
            }
            _ => None,
        }
    }

    /// Refuse a view the host's valid usage does not let `view` be.
    fn viewable(state: &HostState, context: Context, view: &ViewInfo) -> Option<()> {
        let image = Self::image(state, context, view.image)?;
        let offered = state.offered.formats.get(&view.format)?.optimal;
        let viewed = image::SAMPLED
            | image::STORAGE
            | image::COLOR_ATTACHMENT
            | image::DEPTH_STENCIL_ATTACHMENT;
        let usage = view.usage != 0
            && view.usage & !viewed == 0
            && view.usage & !image.usage == 0
            && serves(offered, view.usage);
        let format = view.format == image.format
            || (image.flags & image::MUTABLE_FORMAT != 0
                && (image.view_formats.is_empty() || image.view_formats.contains(&view.format))
                && compatible(image.format, view.format));
        let range = &view.range;
        let held = range.aspects != 0
            && range.aspects & !image.aspects()? == 0
            && range.levels >= 1
            && range.base_level.checked_add(range.levels)? <= image.levels
            && range.layers >= 1
            && range.base_layer.checked_add(range.layers)? <= image.layers;
        let cube = image.flags & image::CUBE_COMPATIBLE != 0;
        let shaped = match (image.kind, view.kind) {
            (0, 0) | (1, 1) => range.layers == 1,
            (0, 4) | (1, 5) | (2, 2) => true,
            (1, 3) => cube && range.layers == 6,
            (1, 6) => cube && range.layers.is_multiple_of(6),
            _ => false,
        };
        (usage && format && held && shaped).then_some(())
    }

    /// Refuse a sampler the host's valid usage does not let `sampler` be,
    /// on the device as it was made.
    fn samplable(state: &HostState, sampler: &SamplerInfo) -> Option<()> {
        let enabled = state.enabled.as_ref()?;
        let mirror = enabled
            .extensions
            .iter()
            .any(|name| name == extension::SAMPLER_MIRROR_CLAMP_TO_EDGE);
        let anisotropic = enabled
            .features
            .first()
            .and_then(|(_, core)| core.get(SAMPLER_ANISOTROPY))
            .copied()
            .unwrap_or(false);
        let filters = sampler
            .filters
            .iter()
            .all(|&filter| (0..=1).contains(&filter));
        let modes = sampler
            .address
            .iter()
            .all(|&mode| (0..=3).contains(&mode) || (mode == 4 && mirror));
        let bias = sampler.lod_bias.is_finite() && sampler.lod_bias.abs() <= 16.0;
        let anisotropy = sampler
            .anisotropy
            .is_none_or(|most| anisotropic && (1.0..=16.0).contains(&most));
        let compare = sampler.compare.is_none_or(|op| (0..=7).contains(&op));
        let lod = sampler.lod[0] <= sampler.lod[1];
        let border = (0..=5).contains(&sampler.border);
        let unnormalized = !sampler.unnormalized
            || (sampler.filters[0] == sampler.filters[1]
                && sampler.filters[2] == 0
                && sampler.lod.iter().all(|&lod| lod == 0.0)
                && sampler.anisotropy.is_none()
                && sampler.compare.is_none()
                && sampler.address[..2]
                    .iter()
                    .all(|&mode| mode == 2 || mode == 3));
        (filters && modes && bias && anisotropy && compare && lod && border && unnormalized)
            .then_some(())
    }

    /// Refuse a descriptor of type `kind` the host's valid usage does not let
    /// `written` be.
    pub(super) fn descriptor_valid(
        state: &HostState,
        context: Context,
        kind: u32,
        written: Written,
    ) -> Option<()> {
        let objects = &state.contexts.get(context)?.objects;
        let sampler = |id: u64| matches!(objects.get(&id), Some(Object::Sampler(_)));
        let view = |id: u64, usage: u32| match objects.get(&id) {
            Some(Object::View(view)) => {
                // A view of depth and stencil reads one of them.
                let one = view.range.aspects.is_power_of_two();
                (view.usage & usage == usage && one).then_some(())
            }
            _ => None,
        };
        match (kind, written) {
            (descriptor::SAMPLER, Written::Image { sampler: id, .. }) => sampler(id).then_some(()),
            (
                descriptor::COMBINED_IMAGE_SAMPLER,
                Written::Image {
                    sampler: id,
                    view: viewed,
                },
            ) => {
                sampler(id).then_some(())?;
                view(viewed, image::SAMPLED)
            }
            (descriptor::SAMPLED_IMAGE, Written::Image { view: viewed, .. }) => {
                view(viewed, image::SAMPLED)
            }
            (descriptor::STORAGE_IMAGE, Written::Image { view: viewed, .. }) => {
                view(viewed, image::STORAGE)
            }
            (
                descriptor::UNIFORM_TEXEL_BUFFER | descriptor::STORAGE_TEXEL_BUFFER,
                Written::Texels(id),
            ) => {
                let Some(Object::Texels(texels)) = objects.get(&id) else {
                    return None;
                };
                let needed = if kind == descriptor::UNIFORM_TEXEL_BUFFER {
                    format_feature::UNIFORM_TEXEL_BUFFER
                } else {
                    format_feature::STORAGE_TEXEL_BUFFER
                };
                let offered = state.offered.formats.get(&texels.format)?.buffer;
                (offered & needed == needed).then_some(())
            }
            (
                descriptor::UNIFORM_BUFFER | descriptor::STORAGE_BUFFER,
                Written::Buffer {
                    buffer,
                    offset,
                    range,
                },
            ) => {
                let (_, size) = Self::bound(state, context, buffer)?;
                (range > 0 && offset.checked_add(range)? <= size).then_some(())
            }
            _ => None,
        }
    }

    /// The image barrier a pipeline barrier holds: one taking a whole image
    /// from the undefined layout to the general one it is kept in.
    pub(super) fn image_barrier(
        state: &HostState,
        context: Context,
        words: &mut Words<'_>,
    ) -> Option<ImageCommand> {
        words.structure(Structure::ImageMemoryBarrier)?;
        let (_src_access, _dst_access) = (words.u32()?, words.u32()?);
        let (old, new) = (words.i32()?, words.i32()?);
        let families = [words.u32()?, words.u32()?];
        let id = words.u64()?;
        let range = subresources(words)?;
        let whole = Self::image(state, context, id)?.whole()?;
        let kept = old == image::UNDEFINED
            && new == image::GENERAL
            && families == [u32::MAX; 2]
            && range == whole;
        kept.then_some(ImageCommand::Kept(id))
    }

    /// Carry out an image command as the device does.
    pub(super) fn execute_image(
        &self,
        state: &mut HostState,
        context: Context,
        command: &ImageCommand,
    ) -> Option<()> {
        match command {
            ImageCommand::Kept(id) => self.keep(state, context, *id),
            ImageCommand::ToImage {
                buffer,
                image,
                regions,
            } => self.between_buffer(state, context, (*buffer, *image, true), regions),
            ImageCommand::ToBuffer {
                image,
                buffer,
                regions,
            } => self.between_buffer(state, context, (*buffer, *image, false), regions),
            ImageCommand::Copy { src, dst, regions } => {
                self.between_images(state, context, (*src, *dst), regions)
            }
            ImageCommand::ClearColour {
                image,
                tag,
                bits,
                ranges,
            } => {
                let (value, read_as) = match tag {
                    0 => (
                        ClearValue::Float(bits.map(f32::from_bits)),
                        Component::Float,
                    ),
                    1 => (
                        ClearValue::Sint(bits.map(u32::cast_signed)),
                        Component::Sint,
                    ),
                    2 => (ClearValue::Uint(*bits), Component::Uint),
                    _ => return None,
                };
                self.clear(state, context, *image, (value, Some(read_as)), ranges)
            }
            ImageCommand::ClearDepth {
                image,
                depth,
                stencil,
                ranges,
            } => {
                (0.0..=1.0).contains(depth).then_some(())?;
                let value = ClearValue::DepthStencil {
                    depth: *depth,
                    stencil: u8::try_from(*stencil).ok()?,
                };
                self.clear(state, context, *image, (value, None), ranges)
            }
        }
    }

    /// Take image `id` from the undefined layout to the general one, its
    /// texels anything until written.
    fn keep(&self, state: &mut HostState, context: Context, id: u64) -> Option<()> {
        let image = Self::image(state, context, id)?;
        let (memory, offset) = image.bound?;
        let len = usize::try_from(image.size()?).ok()?;
        (!image.general).then_some(())?;
        self.write_memory(state, context, memory, offset, &vec![UNDEFINED_TEXEL; len]);
        Self::image_mut(state, context, id)?.general = true;
        Some(())
    }

    /// Copy `regions` between buffer `buffer` and image `image`: into the
    /// image where `into_image`, else out of it.
    fn between_buffer(
        &self,
        state: &mut HostState,
        context: Context,
        (buffer, image, into_image): (u64, u64, bool),
        regions: &[BufferImageRegion],
    ) -> Option<()> {
        let (memory, size) = Self::bound(state, context, buffer)?;
        let usage = if into_image {
            image::TRANSFER_DST
        } else {
            image::TRANSFER_SRC
        };
        let (held, (image_memory, offset)) = Self::usable(state, context, image, usage)?;
        (held.samples == 1).then_some(())?;
        for region in regions {
            for (buffer_at, image_at, len) in held.rows(region, size)? {
                let (in_buffer, in_image) =
                    ((memory, buffer_at), (image_memory, offset + image_at));
                if into_image {
                    self.move_bytes(state, context, (in_buffer, in_image), len);
                } else {
                    self.move_bytes(state, context, (in_image, in_buffer), len);
                }
            }
        }
        Some(())
    }

    /// Copy `regions` from image `src` to image `dst`.
    fn between_images(
        &self,
        state: &mut HostState,
        context: Context,
        (src, dst): (u64, u64),
        regions: &[ImageRegion],
    ) -> Option<()> {
        let (from, (from_memory, from_offset)) =
            Self::usable(state, context, src, image::TRANSFER_SRC)?;
        let (to, (to_memory, to_offset)) = Self::usable(state, context, dst, image::TRANSFER_DST)?;
        (from.samples == 1 && to.samples == 1).then_some(())?;
        for region in regions {
            for (read_at, written_at, len) in from.rows_into(&to, region)? {
                let read = (from_memory, from_offset + read_at);
                let written = (to_memory, to_offset + written_at);
                self.move_bytes(state, context, (read, written), len);
            }
        }
        Some(())
    }

    /// Copy `len` bytes from one memory's place to another's.
    fn move_bytes(
        &self,
        state: &mut HostState,
        context: Context,
        ((from, read_at), (to, written_at)): ((u64, u64), (u64, u64)),
        len: usize,
    ) {
        let mut row = vec![0; len];
        self.read_memory(state, context, from, read_at, &mut row);
        self.write_memory(state, context, to, written_at, &row);
    }

    /// Write `value` to every texel `ranges` name of image `id`: a colour
    /// of an image read as `read_as`, or with none, a depth and stencil.
    fn clear(
        &self,
        state: &mut HostState,
        context: Context,
        id: u64,
        (value, read_as): (ClearValue, Option<Component>),
        ranges: &[Subresources],
    ) -> Option<()> {
        let (target, place) = Self::usable(state, context, id, image::TRANSFER_DST)?;
        let format = seam_format(target.format)?;
        let cleared = match read_as {
            Some(component) => {
                format.aspects() == Aspects::COLOR && format.component() == component
            }
            None => format.aspects().intersects(Aspects::DEPTH_STENCIL),
        };
        cleared.then_some(())?;
        // No sample of a multisampled image outlives the instance drawing it.
        if target.samples != 1 {
            return Some(());
        }
        let texel = self.encoded(format, value)?;
        self.fill(state, context, (&target, place), ranges, &texel)
    }

    /// Write `texel`'s plane of each plane `ranges` name to every texel of
    /// them in `image`.
    fn fill(
        &self,
        state: &mut HostState,
        context: Context,
        (image, (memory, offset)): (&HostImage, (u64, u64)),
        ranges: &[Subresources],
        texel: &[(u32, Vec<u8>)],
    ) -> Option<()> {
        for range in ranges {
            let held = range.aspects != 0
                && range.aspects & !image.aspects()? == 0
                && range.levels >= 1
                && range.base_level.checked_add(range.levels)? <= image.levels
                && range.layers >= 1
                && range.base_layer.checked_add(range.layers)? <= image.layers;
            held.then_some(())?;
            for (plane, bytes) in texel {
                if range.aspects & plane == 0 {
                    continue;
                }
                for level in range.base_level..range.base_level + range.levels {
                    let [width, height, depth] = image.level(level);
                    let row: Vec<u8> = bytes.repeat(usize::try_from(width).ok()?);
                    for layer in range.base_layer..range.base_layer + range.layers {
                        for z in 0..depth {
                            for y in 0..height {
                                let at = image.texel_at(*plane, level, layer, [0, y, z])?;
                                self.write_memory(state, context, memory, offset + at, &row);
                            }
                        }
                    }
                }
            }
        }
        Some(())
    }

    /// Each plane of a texel of `format` holding `value`, as the reference
    /// encodes it.
    fn encoded(&self, format: Format, value: ClearValue) -> Option<Vec<(u32, Vec<u8>)>> {
        let gpu = self
            .reference
            .create_context(&ContextDesc::default())
            .ok()?;
        let one = Extent3d {
            width: 1,
            height: 1,
            depth: 1,
        };
        let desc = ImageDesc {
            dimension: ImageDimension::D2,
            format,
            extent: one,
            mip_levels: 1,
            array_layers: 1,
            samples: 1,
            usage: ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST),
        };
        let (_memory, image) = reference_image(&gpu, &desc)?;
        let planes: Vec<(u32, Aspects, u32)> = PLANES
            .iter()
            .filter(|&&(_, plane)| format.aspects().contains(plane))
            .map(|&(host, plane)| Some((host, plane, format.copy_bytes(plane)?)))
            .collect::<Option<_>>()?;
        let (staging_memory, staging) = reference_buffer(&gpu, BLOCK_ALIGNMENT * 3)?;
        let regions: Vec<BufferImageCopy> = planes
            .iter()
            .zip(0u64..)
            .map(|(&(_, plane, bytes), at)| BufferImageCopy {
                buffer_offset: at * BLOCK_ALIGNMENT,
                bytes_per_row: bytes,
                rows_per_image: 1,
                image: ImageSubresource {
                    aspect: plane,
                    level: 0,
                    base_layer: 0,
                    layers: 1,
                },
                origin: Origin3d::default(),
                extent: one,
            })
            .collect();
        let mut commands = gpu.command_buffer();
        let range = ImageRange {
            aspects: format.aspects(),
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        };
        commands.clear_image(&image, value, &[range]).ok()?;
        commands
            .copy_image_to_buffer(&image, &staging, &regions)
            .ok()?;
        run(&gpu, &commands)?;
        planes
            .iter()
            .zip(0u64..)
            .map(|(&(host, _, bytes), at)| {
                let mut texel = vec![0; usize::try_from(bytes).ok()?];
                staging_memory.read(at * BLOCK_ALIGNMENT, &mut texel).ok()?;
                Some((host, texel))
            })
            .collect()
    }

    /// The reference's copy of image `id`, its texels as this host's memory
    /// holds them.
    pub(super) fn mirror(
        &self,
        state: &HostState,
        context: Context,
        gpu: &tairix_gpu::Context,
        id: u64,
    ) -> Option<Mirrored> {
        let held = Self::image(state, context, id)?.clone();
        let place = held.bound?;
        held.general.then_some(())?;
        let mut usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
        for (host, seam) in [
            (image::SAMPLED, ImageUsage::SAMPLED),
            (image::STORAGE, ImageUsage::STORAGE),
            (image::COLOR_ATTACHMENT, ImageUsage::COLOR_ATTACHMENT),
            (
                image::DEPTH_STENCIL_ATTACHMENT,
                ImageUsage::DEPTH_STENCIL_ATTACHMENT,
            ),
        ] {
            if held.usage & host != 0 {
                usage = usage.with(seam);
            }
        }
        let multisampled = held.samples != 1;
        if multisampled {
            // The reference copies no multisampled image through a buffer.
            usage = usage
                .without(ImageUsage::TRANSFER_SRC)
                .without(ImageUsage::TRANSFER_DST);
        }
        let [width, height, depth] = held.extent;
        let desc = ImageDesc {
            dimension: match held.kind {
                0 => ImageDimension::D1,
                1 => ImageDimension::D2,
                _ => ImageDimension::D3,
            },
            format: seam_format(held.format)?,
            extent: Extent3d {
                width,
                height,
                depth,
            },
            mip_levels: held.levels,
            array_layers: held.layers,
            samples: held.samples,
            usage,
        };
        let (memory, image) = reference_image(gpu, &desc)?;
        let size = if multisampled { 0 } else { held.size()? };
        let mut texels = vec![0; usize::try_from(size).ok()?];
        self.read_memory(state, context, place.0, place.1, &mut texels);
        let (staging_memory, staging) = reference_buffer(gpu, size.max(4))?;
        staging_memory.write(0, &texels).ok()?;
        let blocks = if multisampled {
            Vec::new()
        } else {
            held.blocks()?
        };
        let regions = blocks
            .into_iter()
            .map(|(plane, level, at, bytes)| {
                let [width, height, depth] = held.level(level);
                BufferImageCopy {
                    buffer_offset: at,
                    bytes_per_row: width * bytes,
                    rows_per_image: height,
                    image: ImageSubresource {
                        aspect: seam_aspects(plane),
                        level,
                        base_layer: 0,
                        layers: held.layers,
                    },
                    origin: Origin3d::default(),
                    extent: Extent3d {
                        width,
                        height,
                        depth,
                    },
                }
            })
            .collect();
        Some(Mirrored {
            image,
            staging: (staging_memory, staging),
            len: texels.len(),
            regions,
            place,
            _memory: memory,
        })
    }

    /// The reference's view of host view `view` over its copy `image`.
    pub(super) fn mirrored_view(
        gpu: &tairix_gpu::Context,
        view: &ViewInfo,
        image: &tairix_gpu::Image,
    ) -> Option<tairix_gpu::ImageView> {
        let dimension = match view.kind {
            0 => ViewDimension::D1,
            1 => ViewDimension::D2,
            2 => ViewDimension::D3,
            3 => ViewDimension::Cube,
            4 => ViewDimension::D1Array,
            5 => ViewDimension::D2Array,
            6 => ViewDimension::CubeArray,
            _ => return None,
        };
        let range = &view.range;
        let desc = ImageViewDesc {
            format: seam_format(view.format)?,
            dimension,
            range: ImageRange {
                aspects: seam_aspects(range.aspects),
                base_level: range.base_level,
                levels: range.levels,
                base_layer: range.base_layer,
                layers: range.layers,
            },
        };
        gpu.create_image_view(image, &desc).ok()
    }
}

/// The reference's copy of a host image: the image, the buffer its texels
/// cross in laid out as this host lays them and their bytes, the copies that
/// move them each way, and where the host keeps it.
pub(super) struct Mirrored {
    pub(super) image: tairix_gpu::Image,
    pub(super) staging: (tairix_gpu::Allocation, tairix_gpu::Buffer),
    pub(super) len: usize,
    pub(super) regions: Vec<BufferImageCopy>,
    pub(super) place: (u64, u64),
    _memory: tairix_gpu::Allocation,
}

/// The reference's sampler of host sampler `sampler`.
pub(super) fn mirrored_sampler(
    gpu: &tairix_gpu::Context,
    sampler: &SamplerInfo,
) -> Option<tairix_gpu::Sampler> {
    let filter = |host: i32| match host {
        0 => Some(Filter::Nearest),
        1 => Some(Filter::Linear),
        _ => None,
    };
    let address = |host: i32| {
        Some(match host {
            0 => AddressMode::Repeat,
            1 => AddressMode::MirroredRepeat,
            2 => AddressMode::ClampToEdge,
            3 => AddressMode::ClampToBorder,
            4 => AddressMode::MirrorClampToEdge,
            _ => return None,
        })
    };
    let compare = |host: i32| {
        Some(match host {
            0 => CompareOp::Never,
            1 => CompareOp::Less,
            2 => CompareOp::Equal,
            3 => CompareOp::LessOrEqual,
            4 => CompareOp::Greater,
            5 => CompareOp::NotEqual,
            6 => CompareOp::GreaterOrEqual,
            7 => CompareOp::Always,
            _ => return None,
        })
    };
    let border = match sampler.border / 2 {
        0 => BorderColor::TransparentBlack,
        1 => BorderColor::OpaqueBlack,
        2 => BorderColor::OpaqueWhite,
        _ => return None,
    };
    let anisotropy = match sampler.anisotropy {
        Some(most) => u32::try_from(tairix_util::mathf::round_i32(f64::from(most))).ok()?,
        None => 1,
    };
    let desc = SamplerDesc {
        mag_filter: filter(sampler.filters[0])?,
        min_filter: filter(sampler.filters[1])?,
        mipmap_filter: filter(sampler.filters[2])?,
        address: [
            address(sampler.address[0])?,
            address(sampler.address[1])?,
            address(sampler.address[2])?,
        ],
        lod_bias: sampler.lod_bias,
        lod_min: sampler.lod[0],
        lod_max: sampler.lod[1],
        max_anisotropy: anisotropy,
        compare: match sampler.compare {
            Some(op) => Some(compare(op)?),
            None => None,
        },
        border,
        unnormalized: sampler.unnormalized,
    };
    gpu.create_sampler(&desc).ok()
}

/// The reference's texel buffer of host texel buffer `texels` over `buffer`.
pub(super) fn mirrored_texels(
    gpu: &tairix_gpu::Context,
    buffer: &tairix_gpu::Buffer,
    texels: &HostTexels,
) -> Option<tairix_gpu::BufferView> {
    let desc = BufferViewDesc {
        offset: texels.offset,
        size: texels.range,
        format: seam_format(texels.format)?,
    };
    gpu.create_buffer_view(buffer, &desc).ok()
}

/// What a view of `format` seeing `aspects` is read as, on `info`'s device.
fn sample_type(info: &tairix_gpu::DeviceInfo, format: Format, aspects: Aspects) -> SampleType {
    let filtered = info.formats.get(format).contains(FormatFeatures::FILTERED);
    if aspects == Aspects::STENCIL {
        return SampleType::Uint;
    }
    if aspects == Aspects::DEPTH {
        return if filtered {
            SampleType::Depth
        } else {
            SampleType::UnfilterableDepth
        };
    }
    match format.component() {
        Component::Sint => SampleType::Sint,
        Component::Uint => SampleType::Uint,
        Component::Float if filtered => SampleType::Float,
        Component::Float => SampleType::UnfilterableFloat,
    }
}

/// What `sampler` is as a binding's sampler, sampling texels of `sample`
/// where a module pairs it with an image.
fn sampler_type(sampler: &SamplerInfo, sample: Option<SampleType>) -> SamplerType {
    let nearest = sampler.filters.iter().all(|&filter| filter == 0);
    if sampler.unnormalized {
        SamplerType::Unnormalized
    } else if sampler.compare.is_some() {
        if nearest {
            SamplerType::NonFilteringComparison
        } else {
            SamplerType::Comparison
        }
    } else if matches!(sample, Some(SampleType::Sint | SampleType::Uint)) {
        SamplerType::Integer
    } else if matches!(
        sample,
        Some(SampleType::UnfilterableFloat | SampleType::UnfilterableDepth)
    ) {
        SamplerType::NonFiltering
    } else {
        SamplerType::Filtering
    }
}

/// The reference's binding of `entry`, binding `at` of `program`, holding
/// what `written` holds there: an image sampled as its view reads it, and a
/// sampler as the image a module samples through it is read. A binding the
/// module does not declare is a buffer's, as the host's layout names it.
pub(super) fn reference_binding(
    state: &HostState,
    context: Context,
    (info, entries): (&tairix_gpu::DeviceInfo, &Entries<'_>),
    (at, entry): ((u32, u32), &LayoutEntry),
    written: &BTreeMap<(u32, u32), Written>,
) -> Option<LayoutBinding> {
    let objects = &state.contexts.get(context)?.objects;
    let points = entries
        .iter()
        .map(|&(module, name, stage)| module.entry_point(name, stage))
        .collect::<Option<Vec<_>>>()?;
    // Declared alike by each stage reaching it, each access its own.
    let mut declared = points.iter().flat_map(|point| {
        point
            .bindings()
            .iter()
            .filter(move |binding| (binding.set, binding.binding) == at)
    });
    let first = declared.next();
    let reached =
        first.map(|first| declared.fold(first.access, |access, other| access.with(other.access)));
    let declared = first;
    let view_of = |at: (u32, u32)| match written.get(&at)? {
        Written::Image { view, .. } => match objects.get(view)? {
            Object::View(view) => Some(view),
            _ => None,
        },
        _ => None,
    };
    let sampler_of = |at: (u32, u32)| match written.get(&at)? {
        Written::Image { sampler, .. } => match objects.get(sampler)? {
            Object::Sampler(sampler) => Some(sampler),
            _ => None,
        },
        _ => None,
    };
    let read_as = |at: (u32, u32)| {
        let view = view_of(at)?;
        Some(sample_type(
            info,
            seam_format(view.format)?,
            seam_aspects(view.range.aspects),
        ))
    };
    let (kind, access) = match declared {
        Some(declared) => {
            let kind = match declared.kind {
                BindingKind::StorageImage(shape) => DescriptorType::StorageImage(shape),
                BindingKind::UniformTexelBuffer(shape) => DescriptorType::UniformTexelBuffer(shape),
                BindingKind::StorageTexelBuffer(shape) => DescriptorType::StorageTexelBuffer(shape),
                BindingKind::UniformBuffer => DescriptorType::UniformBuffer,
                BindingKind::StorageBuffer => DescriptorType::StorageBuffer,
                BindingKind::SampledImage(shape) => {
                    DescriptorType::SampledImage(shape, read_as(at)?)
                }
                BindingKind::CombinedImageSampler(shape) => {
                    let sample = read_as(at)?;
                    let sampler = sampler_type(sampler_of(at)?, Some(sample));
                    DescriptorType::CombinedImageSampler(shape, sample, sampler)
                }
                BindingKind::Sampler => {
                    let paired = points
                        .iter()
                        .flat_map(|point| point.samplings())
                        .find(|sampling| sampling.sampler == at)
                        .and_then(|sampling| read_as(sampling.image));
                    DescriptorType::Sampler(sampler_type(sampler_of(at)?, paired))
                }
            };
            (kind, reached.unwrap_or(declared.access))
        }
        None => match entry.kind {
            descriptor::STORAGE_BUFFER => (
                DescriptorType::StorageBuffer,
                Access::READ.with(Access::WRITE),
            ),
            descriptor::UNIFORM_BUFFER => (DescriptorType::UniformBuffer, Access::READ),
            _ => return None,
        },
    };
    Some(LayoutBinding {
        binding: entry.binding,
        kind,
        count: entry.count,
        access,
        stages: super::stages_of(entry.stages)?,
    })
}

/// An image of `desc` in its own memory on the reference.
fn reference_image(
    gpu: &tairix_gpu::Context,
    desc: &ImageDesc,
) -> Option<(tairix_gpu::Allocation, tairix_gpu::Image)> {
    let required = gpu.image_requirements(desc).ok()?;
    let memory = gpu
        .allocate(&AllocationDesc {
            size: required.size,
            memory_type: required.memory_types.trailing_zeros(),
        })
        .ok()?;
    let image = gpu.create_image(&memory, 0, desc).ok()?;
    Some((memory, image))
}

/// A buffer of `size` bytes for every use, in its own memory on the
/// reference.
pub(super) fn reference_buffer(
    gpu: &tairix_gpu::Context,
    size: u64,
) -> Option<(tairix_gpu::Allocation, tairix_gpu::Buffer)> {
    let memory = gpu
        .allocate(&AllocationDesc {
            size,
            memory_type: 0,
        })
        .ok()?;
    let buffer = gpu
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage: BufferUsage::ALL,
            },
        )
        .ok()?;
    Some((memory, buffer))
}

/// Run `commands` on the reference and wait for them.
pub(super) fn run(gpu: &tairix_gpu::Context, commands: &tairix_gpu::CommandBuffer) -> Option<()> {
    let fence = gpu
        .queue(0, 0)
        .ok()?
        .submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline: SECOND,
        })
        .ok()?;
    fence.wait(SECOND).ok()
}

/// The image objects the renderer holds, across every context.
pub(super) fn image_objects(state: &HostState) -> usize {
    state
        .contexts
        .iter()
        .flat_map(|(_, host)| host.objects.values())
        .filter(|object| {
            matches!(
                object,
                Object::Image(_) | Object::View(_) | Object::Sampler(_) | Object::Texels(_)
            )
        })
        .count()
}

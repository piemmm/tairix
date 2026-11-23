//! Images of every shape a description may take, made or refused as the
//! device's rules say, and the copies between them and buffers and between
//! images, and their clears, held to a texel reference: each image's texels
//! by level, layer and place, and each buffer's bytes, both exact. Every
//! image a round makes is read back whole as the round ends.

use std::sync::Arc;
use std::vec::Vec;

use tairix_abi::Duration64;
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, Aspects, BufferDesc, BufferImageCopy, BufferUsage, ClearValue, CommandBuffer,
    ContextDesc, DeviceInfo, Error, Extent3d, Format, FormatFeatures, ImageCopy, ImageDesc,
    ImageDimension, ImageRange, ImageSubresource, ImageUsage, Origin3d, Submission, SubmitError,
};

use super::buffers::{self, Shape};
use super::common::Make;
use super::{below, index_of};

/// Rounds per sweep. Miri interprets every operation, so it drives a handful.
pub(super) const ROUNDS: u32 = if cfg!(miri) { 2 } else { 200 };

/// Submissions per round.
const SUBMISSIONS_PER_ROUND: u32 = if cfg!(miri) { 3 } else { 30 };

/// Descriptions a round asks the device's verdict of.
const DESCRIPTIONS_PER_ROUND: usize = if cfg!(miri) { 6 } else { 40 };

/// The formats of the images the transfers move: integer colours, whose
/// texels a copy moves byte for byte and a clear writes channel by channel,
/// each keeping a value's low bits.
const FORMATS: [Format; 4] = [
    Format::R32Uint,
    Format::Rg32Uint,
    Format::Rgba32Uint,
    Format::Rgba8Uint,
];

/// The bits of the channels of `format`, one of [`FORMATS`].
const fn channel_bits(format: Format) -> u32 {
    match format {
        Format::Rgba8Uint => 8,
        _ => 32,
    }
}

/// What the reference knows of an image: how it was made, where it lies, and
/// each texel of each level, layer by layer, slice by slice and row by row;
/// a multisampled image's texels no copy can bring back, so none.
struct Held {
    desc: ImageDesc,
    memory: usize,
    span: (u64, u64),
    texel: usize,
    levels: Levels,
}

impl Held {
    /// The byte of `levels[level]` texel `(x, y, z)` of `layer` starts at.
    fn at(&self, level: u32, layer: u32, (x, y, z): (u32, u32, u32)) -> usize {
        let extent = self.desc.level_extent(level);
        let depth = extent.depth as usize;
        let slice = layer as usize * depth + z as usize;
        ((slice * extent.height as usize + y as usize) * extent.width as usize + x as usize)
            * self.texel
    }
}

/// The least and the most bytes an image of `desc` can take: its texels
/// packed, and with every row padded to a word and every layer of every
/// level to sixteen bytes.
fn size_bounds(desc: &ImageDesc) -> (u128, u128) {
    let texel = u128::from(desc.format.copy_bytes(desc.format.aspects()).unwrap_or(8));
    let texel = texel.max(u128::from(
        desc.format.copy_bytes(Aspects::DEPTH).unwrap_or(0)
            + desc.format.copy_bytes(Aspects::STENCIL).unwrap_or(0),
    ));
    let (mut least, mut most) = (0u128, 0u128);
    for level in 0..desc.mip_levels.min(32) {
        let extent = desc.level_extent(level);
        let rows = u128::from(extent.height) * u128::from(extent.depth);
        let layers = u128::from(desc.array_layers);
        let packed = u128::from(extent.width) * texel * u128::from(desc.samples) * rows * layers;
        least += packed;
        most += packed + rows * layers * 3 + layers * 15;
    }
    (least, most)
}

/// What the device answers of a description: [`ImageDesc`]'s rules, written
/// out again. `None` where the answer turns on padding the reference does not
/// know: a storage image at the edge of what one may take.
fn admits(info: &DeviceInfo, desc: &ImageDesc) -> Option<Result<(), Error>> {
    let Extent3d {
        width,
        height,
        depth,
    } = desc.extent;
    if [width, height, depth, desc.mip_levels, desc.array_layers].contains(&0)
        || desc.usage == ImageUsage::NONE
    {
        return Some(Err(Error::Empty));
    }
    if !ImageUsage::ALL.contains(desc.usage) {
        return Some(Err(Error::Usage));
    }
    let limits = &info.limits;
    let (shaped, largest, longest) = match desc.dimension {
        ImageDimension::D1 => (
            height == 1 && depth == 1,
            limits.max_image_dimension_1d,
            width,
        ),
        ImageDimension::D2 => (depth == 1, limits.max_image_dimension_2d, width.max(height)),
        ImageDimension::D3 => (
            desc.array_layers == 1,
            limits.max_image_dimension_3d,
            width.max(height).max(depth),
        ),
    };
    if !shaped {
        return Some(Err(Error::Shape));
    }
    if width > largest
        || height > largest
        || depth > largest
        || desc.array_layers > limits.max_image_array_layers
        || desc.mip_levels > 32 - longest.leading_zeros()
    {
        return Some(Err(Error::TooLarge));
    }
    let features = info.formats.get(desc.format);
    let attachment = ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::DEPTH_STENCIL_ATTACHMENT);
    let samples_held = match desc.samples {
        1 => true,
        4 => {
            features.contains(FormatFeatures::MULTISAMPLED)
                && desc.dimension == ImageDimension::D2
                && desc.mip_levels == 1
                && !desc.usage.contains(ImageUsage::STORAGE)
                && desc.usage.intersects(attachment)
        }
        _ => false,
    };
    if !samples_held {
        return Some(Err(Error::Samples));
    }
    let uses = [
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
    ];
    if !uses
        .iter()
        .all(|&(usage, feature)| !desc.usage.contains(usage) || features.contains(feature))
    {
        return Some(Err(Error::Unsupported));
    }
    if desc.usage.contains(ImageUsage::STORAGE) {
        let (least, most) = size_bounds(desc);
        let bound = u128::from(limits.max_storage_image_bytes);
        if least > bound {
            return Some(Err(Error::TooLarge));
        }
        if most > bound {
            return None;
        }
    }
    Some(Ok(()))
}

/// A size along one axis: nought, one, past the device's, or small.
fn draw_side(rng: &mut Prng, largest: u32) -> u32 {
    match rng.below(12) {
        0 => 0,
        1 => largest,
        2 => largest + 1,
        3 => u32::MAX,
        _ => 1 + below(rng, 6),
    }
}

/// A description of any shape, of any format, for the device's verdict.
fn draw_any_desc(rng: &mut Prng, info: &DeviceInfo) -> ImageDesc {
    let dimension = *rng.pick(&[ImageDimension::D1, ImageDimension::D2, ImageDimension::D3]);
    let limits = &info.limits;
    let largest = match dimension {
        ImageDimension::D1 => limits.max_image_dimension_1d,
        ImageDimension::D2 => limits.max_image_dimension_2d,
        ImageDimension::D3 => limits.max_image_dimension_3d,
    };
    let mut axis = |used: bool| {
        if used || rng.below(6) == 0 {
            draw_side(rng, largest)
        } else {
            1
        }
    };
    let extent = Extent3d {
        width: axis(true),
        height: axis(dimension != ImageDimension::D1),
        depth: axis(dimension == ImageDimension::D3),
    };
    let array_layers = if dimension == ImageDimension::D3 && rng.below(4) != 0 {
        1
    } else {
        draw_side(rng, limits.max_image_array_layers)
    };
    let usage = if rng.below(16) == 0 {
        ImageUsage::from_bits(ImageUsage::ALL.bits() + 1)
    } else {
        ImageUsage::from_bits(rng.next_u32() & ImageUsage::ALL.bits())
    };
    ImageDesc {
        dimension,
        format: *rng.pick(&Format::ALL),
        extent,
        mip_levels: *rng.pick(&[0, 1, 1, 2, 3, 5, 40]),
        array_layers,
        samples: *rng.pick(&[1, 1, 1, 4, 4, 2, 0]),
        usage,
    }
}

/// A small single-sample image of one of [`FORMATS`] the transfers move:
/// made for both, three times in four.
fn draw_moved_desc(rng: &mut Prng) -> ImageDesc {
    let dimension = *rng.pick(&[ImageDimension::D1, ImageDimension::D2, ImageDimension::D3]);
    let mut side = || 1 + below(rng, 6);
    let extent = match dimension {
        ImageDimension::D1 => Extent3d {
            width: side(),
            height: 1,
            depth: 1,
        },
        ImageDimension::D2 => Extent3d {
            width: side(),
            height: side(),
            depth: 1,
        },
        ImageDimension::D3 => Extent3d {
            width: side(),
            height: side(),
            depth: side(),
        },
    };
    let longest = extent.width.max(extent.height).max(extent.depth);
    let chain = 32 - longest.leading_zeros();
    let both = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
    let usage = *rng.pick(&[
        both,
        both,
        both,
        ImageUsage::TRANSFER_SRC,
        ImageUsage::TRANSFER_DST,
    ]);
    ImageDesc {
        dimension,
        format: *rng.pick(&FORMATS),
        extent,
        mip_levels: 1 + below(rng, chain),
        array_layers: if dimension == ImageDimension::D3 {
            1
        } else {
            1 + below(rng, 3)
        },
        samples: 1,
        usage: usage.with(ImageUsage::SAMPLED),
    }
}

/// A command over a round's images and buffers.
enum Moved {
    ToImage {
        buffer: usize,
        image: usize,
        regions: Vec<BufferImageCopy>,
    },
    ToBuffer {
        image: usize,
        buffer: usize,
        regions: Vec<BufferImageCopy>,
    },
    Between {
        src: usize,
        dst: usize,
        regions: Vec<ImageCopy>,
    },
    Clear {
        image: usize,
        value: ClearValue,
        ranges: Vec<ImageRange>,
    },
    Buffers(buffers::Recorded),
}

/// A part of `held`'s level drawn to lie inside it mostly, across or past it
/// otherwise.
fn draw_part(rng: &mut Prng, held: &Held) -> (ImageSubresource, Origin3d, Extent3d) {
    let desc = &held.desc;
    let level = if rng.below(10) == 0 {
        desc.mip_levels
    } else {
        below(rng, desc.mip_levels)
    };
    let edge = desc.level_extent(level.min(desc.mip_levels - 1));
    let (base_layer, layers) = match rng.below(10) {
        0 => (0, 0),
        1 => (desc.array_layers, 1),
        2 => (u32::MAX, 2),
        _ => {
            let base = below(rng, desc.array_layers);
            (base, 1 + below(rng, desc.array_layers - base))
        }
    };
    let aspect = if rng.below(16) == 0 {
        Aspects::DEPTH
    } else {
        Aspects::COLOR
    };
    let mut span = |edge: u32| -> (u32, u32) {
        match rng.below(12) {
            0 => (0, 0),
            1 => (edge, 1),
            2 => (u32::MAX, 2),
            3 => (0, edge + 1),
            _ => {
                let start = below(rng, edge);
                (start, 1 + below(rng, edge - start))
            }
        }
    };
    let (x, width) = span(edge.width);
    let (y, height) = span(edge.height);
    let (z, depth) = span(edge.depth);
    (
        ImageSubresource {
            aspect,
            level,
            base_layer,
            layers,
        },
        Origin3d { x, y, z },
        Extent3d {
            width,
            height,
            depth,
        },
    )
}

/// A region between buffer `buffer` and `held`, laid out in the buffer as a
/// region of its size would be, padded, short or off a texel at times.
fn draw_region(rng: &mut Prng, held: &Held, buffer: &Shape) -> BufferImageCopy {
    let (image, origin, extent) = draw_part(rng, held);
    let texel = held.texel as u64;
    let row = u64::from(extent.width) * texel;
    let bytes_per_row = match rng.below(8) {
        0 => row.saturating_sub(texel),
        1 => row + 1,
        2 => row + texel * (1 + rng.below(3) as u64),
        _ => row,
    };
    let rows_per_image = match rng.below(8) {
        0 => extent.height.saturating_sub(1),
        1 => extent.height + 1 + below(rng, 2),
        _ => extent.height,
    };
    let buffer_offset = match rng.below(8) {
        0 => buffer.size,
        1 => texel / 2,
        _ => texel * rng.below(index_of(buffer.size / texel / 2) + 1) as u64,
    };
    BufferImageCopy {
        buffer_offset,
        bytes_per_row: u32::try_from(bytes_per_row).unwrap_or(u32::MAX),
        rows_per_image,
        image,
        origin,
        extent,
    }
}

/// [`ImageSubresource`]'s region rules, written out again: the part within
/// the image, of one plane it holds, of something.
fn part_within(
    desc: &ImageDesc,
    part: &ImageSubresource,
    origin: Origin3d,
    extent: Extent3d,
) -> Result<(), Error> {
    range_within(
        desc,
        &ImageRange {
            aspects: part.aspect,
            base_level: part.level,
            levels: 1,
            base_layer: part.base_layer,
            layers: part.layers,
        },
    )?;
    if desc.format.copy_bytes(part.aspect).is_none() {
        return Err(Error::Aspect);
    }
    if extent.width == 0 || extent.height == 0 || extent.depth == 0 {
        return Err(Error::Empty);
    }
    let edge = desc.level_extent(part.level);
    let inside =
        |start: u32, length: u32, end: u32| start.checked_add(length).is_some_and(|e| e <= end);
    if inside(origin.x, extent.width, edge.width)
        && inside(origin.y, extent.height, edge.height)
        && inside(origin.z, extent.depth, edge.depth)
    {
        Ok(())
    } else {
        Err(Error::OutOfRange)
    }
}

/// [`ImageRange`]'s rules, written out again.
fn range_within(desc: &ImageDesc, range: &ImageRange) -> Result<(), Error> {
    if range.levels == 0 || range.layers == 0 || range.aspects == Aspects::NONE {
        return Err(Error::Empty);
    }
    if !desc.format.aspects().contains(range.aspects) {
        return Err(Error::Aspect);
    }
    let levels = range.base_level.checked_add(range.levels);
    let layers = range.base_layer.checked_add(range.layers);
    if levels.is_none_or(|end| end > desc.mip_levels)
        || layers.is_none_or(|end| end > desc.array_layers)
    {
        return Err(Error::OutOfRange);
    }
    Ok(())
}

/// [`BufferImageCopy`]'s rules, written out again: the bytes its buffer side
/// spans from its first texel.
fn region_span(desc: &ImageDesc, region: &BufferImageCopy) -> Result<u64, Error> {
    if desc.samples != 1 {
        return Err(Error::Samples);
    }
    part_within(desc, &region.image, region.origin, region.extent)?;
    let texel = u64::from(
        desc.format
            .copy_bytes(region.image.aspect)
            .ok_or(Error::Aspect)?,
    );
    let row = u64::from(region.extent.width) * texel;
    let per_row = u64::from(region.bytes_per_row);
    if per_row < row
        || per_row % texel != 0
        || region.rows_per_image < region.extent.height
        || !region.buffer_offset.is_multiple_of(texel)
    {
        return Err(Error::Misaligned);
    }
    let slices = u64::from(region.extent.depth) * u64::from(region.image.layers);
    let per_slice = per_row * u64::from(region.rows_per_image);
    (slices - 1)
        .checked_mul(per_slice)
        .and_then(|before| before.checked_add(u64::from(region.extent.height - 1) * per_row))
        .and_then(|before| before.checked_add(row))
        .ok_or(Error::OutOfRange)
}

/// Whether two parts of one image share a texel.
fn parts_meet(
    a: (&ImageSubresource, Origin3d, Extent3d),
    b: (&ImageSubresource, Origin3d, Extent3d),
) -> bool {
    let along = |a: u32, a_len: u32, b: u32, b_len: u32| {
        u64::from(a) < u64::from(b) + u64::from(b_len)
            && u64::from(b) < u64::from(a) + u64::from(a_len)
    };
    a.0.level == b.0.level
        && a.0.aspect.intersects(b.0.aspect)
        && along(a.0.base_layer, a.0.layers, b.0.base_layer, b.0.layers)
        && along(a.1.x, a.2.width, b.1.x, b.2.width)
        && along(a.1.y, a.2.height, b.1.y, b.2.height)
        && along(a.1.z, a.2.depth, b.1.z, b.2.depth)
}

fn usable_image(held: &Held, usage: ImageUsage) -> Result<(), Error> {
    if held.desc.usage.contains(usage) {
        Ok(())
    } else {
        Err(Error::Usage)
    }
}

fn usable_buffer(shape: &Shape, usage: BufferUsage) -> Result<(), Error> {
    if shape.usage.contains(usage) {
        Ok(())
    } else {
        Err(Error::Usage)
    }
}

/// Each texel a region between a buffer and an image moves: its byte in the
/// buffer, counted from the region's first, and its level, layer and place.
fn region_texels(region: &BufferImageCopy, texel: u64) -> Vec<(u64, u32, (u32, u32, u32))> {
    let mut moved = Vec::new();
    let Extent3d {
        width,
        height,
        depth,
    } = region.extent;
    for layer in 0..region.image.layers {
        for z in 0..depth {
            let slice = u64::from(layer * depth + z);
            for y in 0..height {
                for x in 0..width {
                    let byte = (slice * u64::from(region.rows_per_image) + u64::from(y))
                        * u64::from(region.bytes_per_row)
                        + u64::from(x) * texel;
                    let place = (
                        region.origin.x + x,
                        region.origin.y + y,
                        region.origin.z + z,
                    );
                    moved.push((byte, region.image.base_layer + layer, place));
                }
            }
        }
    }
    moved
}

/// An image's texels by level, where a copy can bring them back.
type Levels = Option<Vec<Vec<u8>>>;

/// Each texel a copy of `regions` between `shape` and `held` moves: its byte
/// in the buffer's memory, its level, and its byte among the level's texels.
fn moved_texels<'r>(
    shape: &'r Shape,
    held: &'r Held,
    regions: &'r [BufferImageCopy],
) -> impl Iterator<Item = (usize, usize, usize)> + 'r {
    regions.iter().flat_map(move |region| {
        region_texels(region, held.texel as u64)
            .into_iter()
            .map(move |(byte, layer, place)| {
                (
                    index_of(shape.offset + region.buffer_offset + byte),
                    region.image.level as usize,
                    held.at(region.image.level, layer, place),
                )
            })
    })
}

/// Each region of a copy between `shape` and `held` within both: the span
/// each takes of the buffer.
fn buffer_regions(
    shape: &Shape,
    held: &Held,
    regions: &[BufferImageCopy],
) -> Result<Vec<(u64, u64)>, Error> {
    if regions.is_empty() {
        return Err(Error::Empty);
    }
    let mut spans = Vec::new();
    for region in regions {
        let span = region_span(&held.desc, region)?;
        if region
            .buffer_offset
            .checked_add(span)
            .is_none_or(|end| end > shape.size)
        {
            return Err(Error::OutOfRange);
        }
        spans.push((region.buffer_offset, span));
    }
    Ok(spans)
}

/// A clear's rules, written out again, and its texels landed in `levels`:
/// each channel of [`FORMATS`]' kind keeping the value's low bits.
fn clear(
    held: &Held,
    levels: &mut Levels,
    value: ClearValue,
    ranges: &[ImageRange],
) -> Result<(), Error> {
    usable_image(held, ImageUsage::TRANSFER_DST)?;
    if ranges.is_empty() {
        return Err(Error::Empty);
    }
    let ClearValue::Uint(channels) = value else {
        return Err(Error::Shape);
    };
    for range in ranges {
        range_within(&held.desc, range)?;
    }
    let bytes = channel_bits(held.desc.format) as usize / 8;
    let texel: Vec<u8> = channels
        .iter()
        .take(held.texel / bytes)
        .flat_map(|&channel| channel.to_le_bytes().into_iter().take(bytes))
        .collect();
    let Some(stored) = levels else {
        return Ok(());
    };
    for range in ranges {
        for level in range.base_level..range.base_level + range.levels {
            let extent = held.desc.level_extent(level);
            let per_layer = index_of(
                u64::from(extent.width) * u64::from(extent.height) * u64::from(extent.depth),
            ) * held.texel;
            let first = range.base_layer as usize * per_layer;
            let cleared =
                &mut stored[level as usize][first..first + range.layers as usize * per_layer];
            for slot in cleared.chunks_exact_mut(held.texel) {
                slot.copy_from_slice(&texel);
            }
        }
    }
    Ok(())
}

/// A round's device, its images and buffers, and the reference beside them.
struct Round {
    info: DeviceInfo,
    context: tairix_gpu::Context,
    queue: tairix_gpu::Queue,
    allocations: Vec<tairix_gpu::Allocation>,
    buffers: Vec<tairix_gpu::Buffer>,
    shapes: Vec<Shape>,
    memories: Vec<Vec<u8>>,
    images: Vec<tairix_gpu::Image>,
    held: Vec<Held>,
    /// A buffer over all of the first image's memory, refused wherever it
    /// meets that image, and the image it aliases.
    alias: Option<usize>,
}

impl Round {
    fn new(rng: &mut Prng, make: Make<'_>) -> Self {
        let device = make(Arc::new(tairix_parallel::Serial));
        let info = device.info().clone();
        let context = device
            .create_context(&ContextDesc::default())
            .expect("a context");
        let queue = context.queue(0, 0).expect("the transfer queue");
        let mut round = Self {
            info,
            context,
            queue,
            allocations: Vec::new(),
            buffers: Vec::new(),
            shapes: Vec::new(),
            memories: Vec::new(),
            images: Vec::new(),
            held: Vec::new(),
            alias: None,
        };
        for _ in 0..=rng.below(2) {
            let memory = round.allocate(16 * (64 + rng.below(192) as u64));
            let whole = round.memories[memory].len() as u64;
            round.add_buffer(memory, 0, whole, BufferUsage::ALL);
            let align = round.info.limits.buffer_alignment;
            for _ in 0..rng.below(3) {
                let offset = align * rng.below(index_of(whole / align / 2)) as u64;
                let size = 4 + rng.below(index_of(whole - offset - 4)) as u64;
                let usage = *rng.pick(&[
                    BufferUsage::ALL,
                    BufferUsage::TRANSFER_SRC,
                    BufferUsage::TRANSFER_DST,
                ]);
                round.add_buffer(memory, offset, size, usage);
            }
        }
        for _ in 0..=rng.below(3) {
            let desc = draw_moved_desc(rng);
            round.place(rng, &desc);
        }
        if rng.below(3) == 0 {
            let four = ImageDesc {
                dimension: ImageDimension::D2,
                format: Format::R32Uint,
                extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                mip_levels: 1,
                array_layers: 1,
                samples: 4,
                usage: ImageUsage::COLOR_ATTACHMENT
                    .with(ImageUsage::SAMPLED)
                    .with(ImageUsage::TRANSFER_SRC)
                    .with(ImageUsage::TRANSFER_DST),
            };
            round.place(rng, &four);
        }
        if rng.below(2) == 0 {
            let first = &round.held[0];
            let (memory, (_, end)) = (first.memory, first.span);
            round.alias = Some(round.shapes.len());
            round.add_buffer(memory, 0, end, BufferUsage::ALL);
        }
        round
    }

    fn allocate(&mut self, size: u64) -> usize {
        let allocation = self
            .context
            .allocate(&AllocationDesc {
                size,
                memory_type: 0,
            })
            .expect("memory");
        self.allocations.push(allocation);
        self.memories.push(vec![0; index_of(size)]);
        self.allocations.len() - 1
    }

    fn add_buffer(&mut self, memory: usize, offset: u64, size: u64, usage: BufferUsage) {
        let buffer = self
            .context
            .create_buffer(
                &self.allocations[memory],
                &BufferDesc {
                    offset,
                    size,
                    usage,
                },
            )
            .expect("a buffer that fits");
        self.buffers.push(buffer);
        self.shapes.push(Shape {
            memory,
            offset,
            size,
            usage,
        });
    }

    /// Make an image of `desc` in memory of its own, a second image sharing
    /// that memory past it at times, after asking for it at an offset off its
    /// alignment and at one past the memory.
    fn place(&mut self, rng: &mut Prng, desc: &ImageDesc) {
        let needs = self
            .context
            .image_requirements(desc)
            .expect("a small image is made");
        let twice = self.held.len() < 3 && rng.below(4) == 0;
        let room = needs.size.next_multiple_of(needs.alignment);
        let memory = self.allocate(if twice { 2 * room } else { room });
        let allocation = &self.allocations[memory];
        let misaligned = (needs.alignment > 1).then_some((needs.alignment / 2, Error::Misaligned));
        let past = (
            allocation.size() - room + needs.alignment,
            Error::OutOfRange,
        );
        for (offset, error) in misaligned.into_iter().chain([past]) {
            assert_eq!(
                self.context.create_image(allocation, offset, desc).err(),
                Some(error),
                "an image placed at {offset}"
            );
        }
        let offsets: &[u64] = if twice { &[0, room] } else { &[0] };
        for &offset in offsets {
            let image = self
                .context
                .create_image(&self.allocations[memory], offset, desc)
                .expect("an image placed within its memory");
            let texel = desc.format.copy_bytes(Aspects::COLOR).expect("a colour") as usize;
            let levels = (desc.samples == 1).then(|| {
                (0..desc.mip_levels)
                    .map(|level| {
                        let extent = desc.level_extent(level);
                        vec![
                            0u8;
                            extent.width as usize
                                * extent.height as usize
                                * extent.depth as usize
                                * desc.array_layers as usize
                                * texel
                        ]
                    })
                    .collect()
            });
            self.images.push(image);
            self.held.push(Held {
                desc: *desc,
                memory,
                span: (offset, offset + needs.size),
                texel,
                levels,
            });
        }
    }

    /// Whether buffer `buffer` and image `image` share a byte.
    fn aliases(&self, buffer: usize, image: usize) -> bool {
        let (shape, held) = (&self.shapes[buffer], &self.held[image]);
        shape.memory == held.memory
            && shape.offset < held.span.1
            && held.span.0 < shape.offset + shape.size
    }

    /// The buffers a command may name, all but the alias; the alias for its
    /// image now and then.
    fn draw_buffer(&self, rng: &mut Prng, image: usize) -> usize {
        match self.alias {
            Some(alias) if image == 0 && rng.below(3) == 0 => alias,
            Some(alias) => {
                let others = alias;
                rng.below(others)
            }
            None => rng.below(self.shapes.len()),
        }
    }

    fn draw_command(&self, rng: &mut Prng) -> Moved {
        let image = rng.below(self.held.len());
        let held = &self.held[image];
        match rng.below(6) {
            0 | 1 => {
                let buffer = self.draw_buffer(rng, image);
                let regions = (0..=rng.below(2))
                    .map(|_| draw_region(rng, held, &self.shapes[buffer]))
                    .collect();
                if rng.below(2) == 0 {
                    Moved::ToImage {
                        buffer,
                        image,
                        regions,
                    }
                } else {
                    Moved::ToBuffer {
                        image,
                        buffer,
                        regions,
                    }
                }
            }
            2 => {
                let dst = if rng.below(4) == 0 {
                    image
                } else {
                    rng.below(self.held.len())
                };
                let regions = (0..=rng.below(2))
                    .map(|_| {
                        let (src, src_origin, extent) = draw_part(rng, held);
                        let (mut dst_part, dst_origin, _) = draw_part(rng, &self.held[dst]);
                        if rng.below(4) != 0 {
                            dst_part.layers = src.layers;
                            dst_part.aspect = src.aspect;
                        }
                        ImageCopy {
                            src,
                            src_origin,
                            dst: dst_part,
                            dst_origin,
                            extent,
                        }
                    })
                    .collect();
                Moved::Between {
                    src: image,
                    dst,
                    regions,
                }
            }
            3 => {
                let value = match rng.below(8) {
                    0 => ClearValue::Float([1.0; 4]),
                    1 => ClearValue::Sint([1; 4]),
                    _ => ClearValue::Uint([
                        rng.next_u32(),
                        rng.next_u32(),
                        rng.next_u32(),
                        rng.next_u32(),
                    ]),
                };
                let ranges = (0..=rng.below(2))
                    .map(|_| {
                        let (part, _, _) = draw_part(rng, held);
                        ImageRange {
                            aspects: part.aspect,
                            base_level: part.level,
                            levels: if rng.below(3) == 0 { 2 } else { 1 },
                            base_layer: part.base_layer,
                            layers: part.layers,
                        }
                    })
                    .collect();
                Moved::Clear {
                    image,
                    value,
                    ranges,
                }
            }
            _ => {
                let plain = self.alias.unwrap_or(self.shapes.len());
                Moved::Buffers(buffers::draw_command(rng, &self.shapes[..plain]))
            }
        }
    }

    fn record(&self, commands: &mut CommandBuffer, moved: &Moved) {
        let outcome = match moved {
            Moved::ToImage {
                buffer,
                image,
                regions,
            } => {
                commands.copy_buffer_to_image(&self.buffers[*buffer], &self.images[*image], regions)
            }
            Moved::ToBuffer {
                image,
                buffer,
                regions,
            } => {
                commands.copy_image_to_buffer(&self.images[*image], &self.buffers[*buffer], regions)
            }
            Moved::Between { src, dst, regions } => {
                commands.copy_image(&self.images[*src], &self.images[*dst], regions)
            }
            Moved::Clear {
                image,
                value,
                ranges,
            } => commands.clear_image(&self.images[*image], *value, ranges),
            Moved::Buffers(recorded) => {
                let buffers: Vec<&tairix_gpu::Buffer> = self.buffers.iter().collect();
                buffers::record(commands, &buffers, recorded);
                Ok(())
            }
        };
        assert_eq!(outcome, Ok(()), "every drawn command records");
    }

    /// Whether the reference admits `moved`, and if it does, apply it to
    /// `memories` and `levels`.
    fn judge(
        &self,
        (memories, levels): (&mut [Vec<u8>], &mut [Levels]),
        moved: &Moved,
    ) -> Result<(), Error> {
        match moved {
            Moved::ToImage {
                buffer,
                image,
                regions,
            } => {
                let (shape, held) = (&self.shapes[*buffer], &self.held[*image]);
                usable_buffer(shape, BufferUsage::TRANSFER_SRC)?;
                usable_image(held, ImageUsage::TRANSFER_DST)?;
                buffer_regions(shape, held, regions)?;
                let twice = regions.iter().enumerate().any(|(at, a)| {
                    regions[at + 1..].iter().any(|b| {
                        parts_meet(
                            (&a.image, a.origin, a.extent),
                            (&b.image, b.origin, b.extent),
                        )
                    })
                });
                if self.aliases(*buffer, *image) || twice {
                    return Err(Error::Overlap);
                }
                if let Some(stored) = &mut levels[*image] {
                    for (from, level, at) in moved_texels(shape, held, regions) {
                        stored[level][at..at + held.texel]
                            .copy_from_slice(&memories[shape.memory][from..from + held.texel]);
                    }
                }
                Ok(())
            }
            Moved::ToBuffer {
                image,
                buffer,
                regions,
            } => {
                let (shape, held) = (&self.shapes[*buffer], &self.held[*image]);
                usable_buffer(shape, BufferUsage::TRANSFER_DST)?;
                usable_image(held, ImageUsage::TRANSFER_SRC)?;
                let spans = buffer_regions(shape, held, regions)?;
                let twice = spans.iter().enumerate().any(|(at, &(a, a_len))| {
                    spans[at + 1..]
                        .iter()
                        .any(|&(b, b_len)| a < b + b_len && b < a + a_len)
                });
                if self.aliases(*buffer, *image) || twice {
                    return Err(Error::Overlap);
                }
                let stored = levels[*image].as_ref().expect("a single-sample image");
                for (to, level, at) in moved_texels(shape, held, regions) {
                    memories[shape.memory][to..to + held.texel]
                        .copy_from_slice(&stored[level][at..at + held.texel]);
                }
                Ok(())
            }
            Moved::Between { src, dst, regions } => {
                self.judge_between((*src, *dst), regions)?;
                self.copy_between(levels, (*src, *dst), regions);
                Ok(())
            }
            Moved::Clear {
                image,
                value,
                ranges,
            } => clear(&self.held[*image], &mut levels[*image], *value, ranges),
            Moved::Buffers(recorded) => {
                let planned = buffers::judge(memories, &self.shapes, recorded)?;
                buffers::land(memories, planned);
                Ok(())
            }
        }
    }

    /// [`ImageCopy`]'s rules and an image copy's, written out again.
    fn judge_between(
        &self,
        (src, dst): (usize, usize),
        regions: &[ImageCopy],
    ) -> Result<(), Error> {
        let (from, into) = (&self.held[src], &self.held[dst]);
        usable_image(from, ImageUsage::TRANSFER_SRC)?;
        usable_image(into, ImageUsage::TRANSFER_DST)?;
        if regions.is_empty() {
            return Err(Error::Empty);
        }
        if from.desc.samples != into.desc.samples {
            return Err(Error::Samples);
        }
        for copy in regions {
            if copy.src.layers != copy.dst.layers || copy.src.aspect != copy.dst.aspect {
                return Err(Error::Shape);
            }
            part_within(&from.desc, &copy.src, copy.src_origin, copy.extent)?;
            part_within(&into.desc, &copy.dst, copy.dst_origin, copy.extent)?;
            let aspect = copy.src.aspect;
            let sized = from.desc.format.copy_bytes(aspect) == into.desc.format.copy_bytes(aspect);
            if !sized || (aspect != Aspects::COLOR && from.desc.format != into.desc.format) {
                return Err(Error::Shape);
            }
        }
        let written = regions
            .iter()
            .map(|c| ((&c.dst, c.dst_origin, c.extent), true));
        let read = regions
            .iter()
            .filter(|_| src == dst)
            .map(|c| ((&c.src, c.src_origin, c.extent), false));
        let parts: Vec<_> = written.chain(read).collect();
        let overlapping = parts.iter().enumerate().any(|(at, &(a, a_written))| {
            parts[at + 1..]
                .iter()
                .any(|&(b, b_written)| (a_written || b_written) && parts_meet(a, b))
        });
        let shared = src != dst
            && from.memory == into.memory
            && from.span.0 < into.span.1
            && into.span.0 < from.span.1;
        if overlapping || shared {
            return Err(Error::Overlap);
        }
        Ok(())
    }

    fn copy_between(
        &self,
        levels: &mut [Levels],
        (src, dst): (usize, usize),
        regions: &[ImageCopy],
    ) {
        let (from, into) = (&self.held[src], &self.held[dst]);
        let mut moved = Vec::new();
        if let Some(stored) = &levels[src] {
            for copy in regions {
                for layer in 0..copy.src.layers {
                    for z in 0..copy.extent.depth {
                        for y in 0..copy.extent.height {
                            for x in 0..copy.extent.width {
                                let read = from.at(
                                    copy.src.level,
                                    copy.src.base_layer + layer,
                                    (
                                        copy.src_origin.x + x,
                                        copy.src_origin.y + y,
                                        copy.src_origin.z + z,
                                    ),
                                );
                                let write = into.at(
                                    copy.dst.level,
                                    copy.dst.base_layer + layer,
                                    (
                                        copy.dst_origin.x + x,
                                        copy.dst_origin.y + y,
                                        copy.dst_origin.z + z,
                                    ),
                                );
                                let bytes = stored[copy.src.level as usize]
                                    [read..read + from.texel]
                                    .to_vec();
                                moved.push((copy.dst.level as usize, write, bytes));
                            }
                        }
                    }
                }
            }
        }
        if let Some(stored) = &mut levels[dst] {
            for (level, at, bytes) in moved {
                stored[level][at..at + bytes.len()].copy_from_slice(&bytes);
            }
        }
    }

    /// Submit `drawn` to the device and the reference, require them to agree
    /// on the verdict and on every buffer's bytes, and answer the verdict.
    fn exchange(&mut self, drawn: &[Moved]) -> Result<(), SubmitError> {
        let mut commands = self.context.command_buffer();
        for moved in drawn {
            self.record(&mut commands, moved);
        }
        let mut memories = self.memories.clone();
        let mut levels: Vec<_> = self.held.iter().map(|held| held.levels.clone()).collect();
        let mut expected = Ok(());
        for (index, moved) in drawn.iter().enumerate() {
            if let Err(error) = self.judge((&mut memories, &mut levels), moved) {
                expected = Err(SubmitError {
                    command: Some(index),
                    error,
                });
                break;
            }
        }
        let outcome = self.queue.submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline: Duration64::from_secs(1),
        });
        match (outcome, expected) {
            (Ok(fence), Ok(())) => {
                fence.wait(Duration64::ZERO).expect("completed");
                self.memories = memories;
                for (held, levels) in self.held.iter_mut().zip(levels) {
                    held.levels = levels;
                }
            }
            (Err(got), Err(want)) => assert_eq!(got, want),
            (got, want) => {
                panic!("the device answered {got:?} where the reference answered {want:?}")
            }
        }
        for (memory, (allocation, reference)) in
            self.allocations.iter().zip(&self.memories).enumerate()
        {
            // An image's texels lie as the device lays them: read back whole.
            if self.held.iter().any(|held| held.memory == memory) {
                continue;
            }
            let mut seen = vec![0u8; reference.len()];
            allocation.read(0, &mut seen).expect("readable");
            assert!(
                &seen == reference,
                "a buffer's memory as the reference holds it"
            );
        }
        expected
    }

    /// Read every texel of every single-sample image made for copying out,
    /// and hold each to the reference.
    fn read_back(&self) {
        for (image, held) in self.images.iter().zip(&self.held) {
            let Some(levels) = &held.levels else {
                continue;
            };
            if !held.desc.usage.contains(ImageUsage::TRANSFER_SRC) {
                continue;
            }
            let total: usize = levels.iter().map(Vec::len).sum();
            let size = (total as u64).next_multiple_of(16);
            let memory = self
                .context
                .allocate(&AllocationDesc {
                    size,
                    memory_type: 0,
                })
                .expect("memory");
            let buffer = self
                .context
                .create_buffer(
                    &memory,
                    &BufferDesc {
                        offset: 0,
                        size,
                        usage: BufferUsage::TRANSFER_DST,
                    },
                )
                .expect("a buffer");
            let mut offset = 0u64;
            let regions: Vec<BufferImageCopy> = (0..held.desc.mip_levels)
                .map(|level| {
                    let extent = held.desc.level_extent(level);
                    let region = BufferImageCopy {
                        buffer_offset: offset,
                        bytes_per_row: extent.width
                            * u32::try_from(held.texel).expect("a texel's bytes"),
                        rows_per_image: extent.height,
                        image: ImageSubresource {
                            aspect: Aspects::COLOR,
                            level,
                            base_layer: 0,
                            layers: held.desc.array_layers,
                        },
                        origin: Origin3d::default(),
                        extent,
                    };
                    offset += levels[level as usize].len() as u64;
                    region
                })
                .collect();
            let mut commands = self.context.command_buffer();
            commands
                .copy_image_to_buffer(image, &buffer, &regions)
                .expect("recorded");
            self.queue
                .submit(&Submission {
                    command_buffers: &[&commands],
                    waits: &[],
                    signals: &[],
                    deadline: Duration64::from_secs(1),
                })
                .expect("a whole image copied out")
                .wait(Duration64::ZERO)
                .expect("completed");
            let mut seen = vec![0u8; total];
            memory.read(0, &mut seen).expect("readable");
            let expected: Vec<u8> = levels.iter().flatten().copied().collect();
            assert!(
                seen == expected,
                "every texel of {:?} as the reference holds it",
                held.desc
            );
        }
    }
}

pub(super) fn run_round(rng: &mut Prng, make: Make<'_>) {
    let mut round = Round::new(rng, make);
    for _ in 0..DESCRIPTIONS_PER_ROUND {
        let desc = draw_any_desc(rng, &round.info);
        let Some(expected) = admits(&round.info, &desc) else {
            continue;
        };
        let answered = round.context.image_requirements(&desc).map(|_| ());
        assert_eq!(answered, expected, "the device's verdict of {desc:?}");
    }
    for _ in 0..SUBMISSIONS_PER_ROUND {
        let drawn: Vec<Moved> = (0..=rng.below(3))
            .map(|_| round.draw_command(rng))
            .collect();
        let _ = round.exchange(&drawn);
    }
    round.read_back();
}

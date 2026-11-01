//! Texels moved on the software device: copies between buffers and images
//! and between images, and clears, row by row through each image's layout.

use crate::command::Op;
use crate::format::{Aspects, Format};
use crate::image::{BufferImageCopy, ClearValue, ImageCopy, ImageRange};
use crate::memory::{read_bytes, write_bytes, Memory};
use crate::model::BufferState;
use crate::resource::ImageState;
use crate::software::texel::{encode, Plane, Srgb};
use crate::software::Meter;
use crate::transfer::Direction;

/// How a run of a transfer ended.
pub(crate) enum Moved {
    Done,
    /// Stopped past its deadline.
    Overran,
    /// Reached past its memory, which its checks proved it cannot.
    Fault,
}

/// The words of one memory, or of two different ones, a transfer reads from
/// and writes to.
enum Pair<'a> {
    One(&'a mut [u32]),
    Two(&'a [u32], &'a mut [u32]),
}

impl Pair<'_> {
    /// Copy `len` bytes from byte `from` of the source to byte `to` of the
    /// destination; the two do not overlap when in one memory.
    fn copy(&mut self, from: usize, to: usize, len: usize) -> Option<()> {
        // A plane of a texel lies within one word each side: moved by mask.
        if len <= 4 && from % 4 + len <= 4 && to % 4 + len <= 4 {
            let source = match self {
                Self::One(words) => words.get(from / 4),
                Self::Two(src, _) => src.get(from / 4),
            };
            let mask = [0, 0xff, 0xffff, 0x00ff_ffff, u32::MAX].get(len).copied()?;
            let bytes = (source.copied()? >> (8 * (from % 4))) & mask;
            let target = match self {
                Self::One(words) => words.get_mut(to / 4),
                Self::Two(_, dst) => dst.get_mut(to / 4),
            }?;
            let shift = 8 * (to % 4);
            *target = (*target & !(mask << shift)) | (bytes << shift);
            return Some(());
        }
        let whole = from.is_multiple_of(4) && to.is_multiple_of(4) && len.is_multiple_of(4);
        match self {
            Self::One(words) if whole => {
                let range = from / 4..(from + len) / 4;
                if range.end > words.len() || (to + len) / 4 > words.len() {
                    return None;
                }
                words.copy_within(range, to / 4);
            }
            Self::Two(src, dst) if whole => {
                let source = src.get(from / 4..(from + len) / 4)?;
                dst.get_mut(to / 4..(to + len) / 4)?.copy_from_slice(source);
            }
            _ => {
                let mut staged = [0u8; 256];
                let mut done = 0;
                while done < len {
                    let step = (len - done).min(staged.len());
                    let chunk = staged.get_mut(..step)?;
                    match self {
                        Self::One(words) => {
                            read_bytes(words, from + done, chunk)?;
                            write_bytes(words, to + done, chunk)?;
                        }
                        Self::Two(src, dst) => {
                            read_bytes(src, from + done, chunk)?;
                            write_bytes(dst, to + done, chunk)?;
                        }
                    }
                    done += step;
                }
            }
        }
        Some(())
    }
}

/// Lock `src` and `dst`, one memory or two, and run `body` over their words.
fn locked<R>(src: &Memory, dst: &Memory, body: impl FnOnce(Pair<'_>) -> R) -> R {
    if core::ptr::eq(src, dst) {
        let mut words = src.lock();
        body(Pair::One(&mut words))
    } else {
        let (from, mut to) = Memory::lock_pair(src, dst);
        body(Pair::Two(&from, &mut to))
    }
}

fn index(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

/// Where in a texel of `format` a copy of `aspect` reads and writes its
/// bytes, and how many: the whole texel, or one plane of depth and stencil.
fn plane_of(format: Format, aspect: Aspects) -> Option<(usize, usize)> {
    let copied = format.copy_bytes(aspect)? as usize;
    let whole = format.texel_bytes() as usize;
    if copied == whole {
        return Some((0, whole));
    }
    match aspect {
        a if a == Aspects::STENCIL => Some((4, 1)),
        _ => Some((0, 4)),
    }
}

/// A copy between a buffer and an image.
pub(crate) fn buffer_image(
    buffer: &BufferState,
    image: &ImageState,
    regions: &[BufferImageCopy],
    direction: Direction,
    meter: &mut Meter<'_>,
) -> Moved {
    let (src, dst) = match direction {
        Direction::ToImage => (&*buffer.memory, &*image.memory),
        Direction::ToBuffer => (&*image.memory, &*buffer.memory),
    };
    locked(src, dst, |mut pair| {
        for region in regions {
            let Some(moved) = buffer_region(&mut pair, buffer, image, region, direction, meter)
            else {
                return Moved::Fault;
            };
            if !moved {
                return Moved::Overran;
            }
        }
        Moved::Done
    })
}

/// One region of a buffer-image copy: `None` past its memory, `Some(false)`
/// past its deadline.
fn buffer_region(
    pair: &mut Pair<'_>,
    buffer: &BufferState,
    image: &ImageState,
    region: &BufferImageCopy,
    direction: Direction,
    meter: &mut Meter<'_>,
) -> Option<bool> {
    let format = image.desc.format;
    let (within, plane) = plane_of(format, region.image.aspect)?;
    let texel = format.texel_bytes() as usize;
    let width = region.extent.width as usize;
    let per_row = u64::from(region.bytes_per_row);
    let per_slice = per_row * u64::from(region.rows_per_image);
    let depth = u64::from(region.extent.depth);
    for layer in 0..region.image.layers {
        for z in 0..region.extent.depth {
            let slice = (u64::from(layer) * depth + u64::from(z)) * per_slice;
            for y in 0..region.extent.height {
                if !meter.proceed(0) {
                    return Some(false);
                }
                let in_buffer =
                    buffer.offset + region.buffer_offset + slice + u64::from(y) * per_row;
                let at = [region.origin.x, region.origin.y + y, region.origin.z + z];
                let in_image = image.offset
                    + image.layout.texel_offset(
                        region.image.level,
                        region.image.base_layer + layer,
                        at,
                        0,
                    );
                let (in_buffer, in_image) = (index(in_buffer)?, index(in_image)?);
                if plane == texel {
                    let len = width * texel;
                    match direction {
                        Direction::ToImage => pair.copy(in_buffer, in_image, len)?,
                        Direction::ToBuffer => pair.copy(in_image, in_buffer, len)?,
                    }
                } else {
                    for x in 0..width {
                        let texel_at = in_image + x * texel + within;
                        let buffer_at = in_buffer + x * plane;
                        match direction {
                            Direction::ToImage => pair.copy(buffer_at, texel_at, plane)?,
                            Direction::ToBuffer => pair.copy(texel_at, buffer_at, plane)?,
                        }
                    }
                }
                meter.charge((width * texel) as u64);
            }
        }
    }
    Some(true)
}

/// A copy between images.
pub(crate) fn image_image(
    src: &ImageState,
    dst: &ImageState,
    regions: &[ImageCopy],
    meter: &mut Meter<'_>,
) -> Moved {
    locked(&src.memory, &dst.memory, |mut pair| {
        for region in regions {
            let Some(moved) = image_region(&mut pair, src, dst, region, meter) else {
                return Moved::Fault;
            };
            if !moved {
                return Moved::Overran;
            }
        }
        Moved::Done
    })
}

fn image_region(
    pair: &mut Pair<'_>,
    src: &ImageState,
    dst: &ImageState,
    region: &ImageCopy,
    meter: &mut Meter<'_>,
) -> Option<bool> {
    let (src_within, plane) = plane_of(src.desc.format, region.src.aspect)?;
    let (dst_within, _) = plane_of(dst.desc.format, region.dst.aspect)?;
    let src_unit = src.desc.format.texel_bytes() as usize;
    let dst_unit = dst.desc.format.texel_bytes() as usize;
    let whole = plane == src_unit && src_unit == dst_unit;
    let samples = src.desc.samples as usize;
    let units = region.extent.width as usize * samples;
    for layer in 0..region.src.layers {
        for z in 0..region.extent.depth {
            for y in 0..region.extent.height {
                if !meter.proceed(0) {
                    return Some(false);
                }
                let from = [
                    region.src_origin.x,
                    region.src_origin.y + y,
                    region.src_origin.z + z,
                ];
                let to = [
                    region.dst_origin.x,
                    region.dst_origin.y + y,
                    region.dst_origin.z + z,
                ];
                let from = src.offset
                    + src.layout.texel_offset(
                        region.src.level,
                        region.src.base_layer + layer,
                        from,
                        0,
                    );
                let to = dst.offset
                    + dst.layout.texel_offset(
                        region.dst.level,
                        region.dst.base_layer + layer,
                        to,
                        0,
                    );
                let (from, to) = (index(from)?, index(to)?);
                if whole {
                    pair.copy(from, to, units * src_unit)?;
                } else {
                    for unit in 0..units {
                        pair.copy(
                            from + unit * src_unit + src_within,
                            to + unit * dst_unit + dst_within,
                            plane,
                        )?;
                    }
                }
                meter.charge((units * src_unit) as u64);
            }
        }
    }
    Some(true)
}

/// The planes a clear of `value` over `aspects` of `format` writes, and the
/// raw texel each holds.
fn cleared(
    format: Format,
    aspects: Aspects,
    value: ClearValue,
    srgb: &Srgb,
) -> [(Plane, [u32; 4]); 2] {
    let none = (Plane::Colour, [0; 4]);
    match value {
        ClearValue::DepthStencil { depth, stencil } => {
            let depth_plane = aspects.contains(Aspects::DEPTH).then(|| {
                (
                    Plane::Depth,
                    encode(format, Plane::Depth, [depth.to_bits(), 0, 0, 0], srgb),
                )
            });
            let stencil_plane = aspects.contains(Aspects::STENCIL).then(|| {
                (
                    Plane::Stencil,
                    encode(format, Plane::Stencil, [u32::from(stencil), 0, 0, 0], srgb),
                )
            });
            [depth_plane.unwrap_or(none), stencil_plane.unwrap_or(none)]
        }
        ClearValue::Float(rgba) => [
            (
                Plane::Colour,
                encode(format, Plane::Colour, rgba.map(f32::to_bits), srgb),
            ),
            none,
        ],
        ClearValue::Sint(rgba) => [
            (
                Plane::Colour,
                encode(format, Plane::Colour, rgba.map(i32::cast_unsigned), srgb),
            ),
            none,
        ],
        ClearValue::Uint(rgba) => [
            (Plane::Colour, encode(format, Plane::Colour, rgba, srgb)),
            none,
        ],
    }
}

/// A clear of `ranges` of an image.
pub(crate) fn clear(
    image: &ImageState,
    value: ClearValue,
    ranges: &[ImageRange],
    srgb: &Srgb,
    meter: &mut Meter<'_>,
) -> Moved {
    let mut words = image.memory.lock();
    for range in ranges {
        let Some(cleared) = clear_range(&mut words, image, value, range, srgb, meter) else {
            return Moved::Fault;
        };
        if !cleared {
            return Moved::Overran;
        }
    }
    Moved::Done
}

fn clear_range(
    words: &mut [u32],
    image: &ImageState,
    value: ClearValue,
    range: &ImageRange,
    srgb: &Srgb,
    meter: &mut Meter<'_>,
) -> Option<bool> {
    let format = image.desc.format;
    let planes = cleared(format, range.aspects, value, srgb);
    let texel = format.texel_bytes() as usize;
    let whole = !matches!(format.aspects(), a if a == Aspects::DEPTH_STENCIL)
        || range.aspects == Aspects::DEPTH_STENCIL;
    let pattern = texel_pattern(texel, planes)?;
    // The bits of a depth-stencil texel's two words a one-plane clear keeps:
    // depth is the first word, stencil the second's low byte.
    let kept = if range.aspects == Aspects::DEPTH {
        [0, u32::MAX, 0, u32::MAX]
    } else {
        [u32::MAX, !0xff, u32::MAX, !0xff]
    };
    for level in range.base_level..range.base_level + range.levels {
        let placed = image.layout.levels.get(level as usize)?;
        let row_bytes = placed.extent.width as usize * texel * image.desc.samples as usize;
        for layer in range.base_layer..range.base_layer + range.layers {
            for z in 0..placed.extent.depth {
                for y in 0..placed.extent.height {
                    if !meter.proceed(0) {
                        return Some(false);
                    }
                    let at = image.offset + image.layout.texel_offset(level, layer, [0, y, z], 0);
                    let row = row_words(words, index(at)?, row_bytes)?;
                    if whole {
                        fill_row(row, &pattern)?;
                    } else {
                        keep_row(row, &pattern, &kept);
                    }
                    meter.charge(row_bytes as u64);
                }
            }
        }
    }
    Some(true)
}

/// The raw texel's bytes, the planes of depth and stencil merged.
fn raw_bytes(raw: [u32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(raw) {
        *chunk = word.to_le_bytes();
    }
    bytes
}

/// Four words of repeated texels: what a row of `texel`-byte texels holding
/// the merged planes reads as, word by word. `None` for a texel the four
/// words do not hold a whole number of.
fn texel_pattern(texel: usize, planes: [(Plane, [u32; 4]); 2]) -> Option<[u32; 4]> {
    if texel == 0 || 16 % texel != 0 {
        return None;
    }
    let mut merged = [0u32; 4];
    for (_, raw) in planes {
        for (into, word) in merged.iter_mut().zip(raw) {
            *into |= word;
        }
    }
    let bytes = raw_bytes(merged);
    let one = bytes.get(..texel)?;
    let mut repeated = [0u8; 16];
    for chunk in repeated.chunks_exact_mut(texel) {
        chunk.copy_from_slice(one);
    }
    let mut pattern = [0u32; 4];
    for (word, chunk) in pattern.iter_mut().zip(repeated.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*chunk);
    }
    Some(pattern)
}

/// The words of a row of `len` bytes at byte `row`, which starts on a word
/// and a texel; the words past the last texel but within the row's padding
/// are taken too.
fn row_words(words: &mut [u32], row: usize, len: usize) -> Option<&mut [u32]> {
    if !row.is_multiple_of(4) {
        return None;
    }
    let start = row / 4;
    words.get_mut(start..start + len.div_ceil(4))
}

/// Fill a row with `pattern`'s texels: their sizes divide its four words.
fn fill_row(row: &mut [u32], pattern: &[u32; 4]) -> Option<()> {
    let (whole, tail) = row.as_chunks_mut::<4>();
    whole.fill(*pattern);
    tail.copy_from_slice(pattern.get(..tail.len())?);
    Some(())
}

/// Write `pattern`'s texels into a row but for the bits `kept` keeps.
fn keep_row(row: &mut [u32], pattern: &[u32; 4], kept: &[u32; 4]) {
    let mut set = [0; 4];
    for ((into, &word), &keep) in set.iter_mut().zip(pattern).zip(kept) {
        *into = word & !keep;
    }
    let (whole, tail) = row.as_chunks_mut::<4>();
    for words in whole.iter_mut().map(<[u32; 4]>::as_mut_slice).chain([tail]) {
        for ((word, &keep), &into) in words.iter_mut().zip(kept).zip(&set) {
            *word = (*word & keep) | into;
        }
    }
}

/// Run one transfer `op`.
pub(crate) fn apply(op: &Op, srgb: &Srgb, meter: &mut Meter<'_>) -> Option<Moved> {
    match op {
        Op::BufferImage {
            buffer,
            image,
            regions,
            direction,
        } => Some(buffer_image(buffer, image, regions, *direction, meter)),
        Op::CopyImage { src, dst, regions } => Some(image_image(src, dst, regions, meter)),
        Op::ClearImage {
            image,
            value,
            ranges,
        } => Some(clear(image, *value, ranges, srgb, meter)),
        _ => None,
    }
}

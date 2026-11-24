//! The checks a submission makes of the commands that move texels: copies
//! between buffers and images and between images, and clears.
//!
//! A copy that writes a texel twice, or reads what it writes, is refused, as a
//! buffer copy is: what lands would depend on the order the regions run in.
//! Where two objects share a memory, one that overlaps the other in it is
//! taken to touch all of it, so no alias is ever both read and written.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use crate::backend::Backend;
use crate::command::{buffer, Op};
use crate::format::Aspects;
use crate::image::ImageUsage;
use crate::image::{BufferImageCopy, ClearValue, Extent3d, ImageCopy, ImageRange, Origin3d};
use crate::info::BufferUsage;
use crate::memory::overlaps;
use crate::model::{BufferKey, BufferState, ContextState, ImageKey};
use crate::resource::ImageState;
use crate::{Error, MAX_COPY_REGIONS};

/// Which way a copy between a buffer and an image moves texels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Direction {
    /// From the buffer into the image.
    ToImage,
    /// From the image into the buffer.
    ToBuffer,
}

/// The image `key` names, made for `usage`.
pub(crate) fn image<B: Backend>(
    context: &ContextState<B>,
    key: ImageKey,
    usage: ImageUsage,
) -> Result<&Arc<ImageState<B>>, Error> {
    let image = context.images.table.get(key).ok_or(Error::Stale)?;
    if image.desc.usage.contains(usage) {
        Ok(image)
    } else {
        Err(Error::Usage)
    }
}

/// Refuse an empty list or one past [`MAX_COPY_REGIONS`].
fn bounded<T>(regions: &[T]) -> Result<(), Error> {
    if regions.is_empty() {
        return Err(Error::Empty);
    }
    if regions.len() > MAX_COPY_REGIONS {
        return Err(Error::TooMany);
    }
    Ok(())
}

fn copied<T: Copy>(regions: &[T]) -> Result<Vec<T>, Error> {
    tairix_util::fallible::copied(regions).ok_or(Error::OutOfMemory)
}

/// The bytes of its memory an image's texels may lie in.
const fn bytes_of<B: Backend>(image: &ImageState<B>) -> Range<u64> {
    image.offset..image.offset.saturating_add(image.size)
}

/// Whether a buffer's range and an image share a byte of one memory.
fn aliases<B: Backend>(buffer: &BufferState<B>, image: &ImageState<B>) -> bool {
    Arc::ptr_eq(&buffer.memory, &image.memory)
        && overlaps(
            &(buffer.offset..buffer.offset.saturating_add(buffer.size)),
            &bytes_of(image),
        )
}

/// One region of an image a copy or a clear touches.
#[derive(Copy, Clone, Debug)]
struct Region {
    level: u32,
    layers: (u32, u32),
    origin: Origin3d,
    extent: Extent3d,
    aspect: Aspects,
}

/// An axis a region spans along.
#[derive(Copy, Clone, Debug)]
enum Axis {
    X,
    Y,
    Z,
    Layers,
}

impl Region {
    /// Where it starts along `axis`, and where it ends.
    fn span(&self, axis: Axis) -> (u64, u64) {
        let (start, len) = match axis {
            Axis::X => (self.origin.x, self.extent.width),
            Axis::Y => (self.origin.y, self.extent.height),
            Axis::Z => (self.origin.z, self.extent.depth),
            Axis::Layers => self.layers,
        };
        (u64::from(start), u64::from(start) + u64::from(len))
    }

    fn meets(&self, other: &Self) -> bool {
        let along = |a: u32, a_len: u32, b: u32, b_len: u32| {
            let span = |start: u32, len: u32| u64::from(start)..u64::from(start) + u64::from(len);
            overlaps(&span(a, a_len), &span(b, b_len))
        };
        self.level == other.level
            && self.aspect.intersects(other.aspect)
            && along(self.layers.0, self.layers.1, other.layers.0, other.layers.1)
            && along(
                self.origin.x,
                self.extent.width,
                other.origin.x,
                other.extent.width,
            )
            && along(
                self.origin.y,
                self.extent.height,
                other.origin.y,
                other.extent.height,
            )
            && along(
                self.origin.z,
                self.extent.depth,
                other.origin.z,
                other.extent.depth,
            )
    }
}

/// Whether two of `regions`, each marked whether it is written, share a
/// texel that one of them writes: swept along the axis the fewest pairs
/// share, in order of level and start, so a region is tested only against
/// those it meets along that axis, and regions split along any one axis
/// cost no pair at all.
fn overlap(regions: &mut [(Region, bool)]) -> bool {
    let axis = [Axis::X, Axis::Y, Axis::Z, Axis::Layers]
        .into_iter()
        .min_by_key(|&axis| sharing(regions, axis))
        .unwrap_or(Axis::X);
    sort_along(regions, axis);
    for (at, (region, written)) in regions.iter().enumerate() {
        let (_, end) = region.span(axis);
        for (other, other_written) in regions.iter().skip(at + 1) {
            if other.level != region.level || other.span(axis).0 >= end {
                break;
            }
            if (*written || *other_written) && region.meets(other) {
                return true;
            }
        }
    }
    false
}

fn sort_along(regions: &mut [(Region, bool)], axis: Axis) {
    regions.sort_unstable_by_key(|(region, _)| (region.level, region.span(axis).0));
}

/// How many pairs of `regions` of one level meet along `axis`.
fn sharing(regions: &mut [(Region, bool)], axis: Axis) -> u64 {
    sort_along(regions, axis);
    let mut pairs = 0u64;
    for (at, (region, _)) in regions.iter().enumerate() {
        let (_, end) = region.span(axis);
        let rest = regions.get(at + 1..).unwrap_or(&[]);
        // Of one level, sorted by start: those starting before it ends lead.
        let met = rest
            .partition_point(|(other, _)| other.level == region.level && other.span(axis).0 < end);
        pairs = pairs.saturating_add(met as u64);
    }
    pairs
}

/// Whether two of the byte spans `(offset, length)` share a byte.
fn spans_overlap(spans: &mut [(u64, u64)]) -> bool {
    spans.sort_unstable();
    let mut end = 0;
    spans.iter().any(|&(offset, length)| {
        let met = offset < end;
        end = end.max(offset.saturating_add(length));
        met
    })
}

/// Whether a copy of `copies` into an image writes a texel twice: decided
/// once as it is recorded, as it depends on nothing else.
pub(crate) fn to_image_overlaps(copies: &[BufferImageCopy]) -> Result<bool, Error> {
    let mut regions = tairix_util::fallible::collected(
        copies.len(),
        copies.iter().map(|copy| (of_copy(copy), true)),
    )
    .ok_or(Error::OutOfMemory)?;
    Ok(overlap(&mut regions))
}

/// Whether a copy of `copies` out of an image of `desc` writes a byte of its
/// buffer twice; one whose regions `desc` refuses is refused for that when
/// submitted, so it is not.
pub(crate) fn to_buffer_overlaps(
    copies: &[BufferImageCopy],
    desc: &crate::image::ImageDesc,
) -> Result<bool, Error> {
    let mut spans = Vec::new();
    spans
        .try_reserve_exact(copies.len())
        .map_err(|_| Error::OutOfMemory)?;
    for copy in copies {
        let Ok(span) = copy.check(desc) else {
            return Ok(false);
        };
        spans.push((copy.buffer_offset, span));
    }
    Ok(spans_overlap(&mut spans))
}

/// Whether a copy of `copies` between images writes a texel twice or, where
/// they are the `same` image, reads one it writes.
pub(crate) fn image_copy_overlaps(copies: &[ImageCopy], same: bool) -> Result<bool, Error> {
    let written = copies.iter().map(|c| {
        let region = Region {
            level: c.dst.level,
            layers: (c.dst.base_layer, c.dst.layers),
            origin: c.dst_origin,
            extent: c.extent,
            aspect: c.dst.aspect,
        };
        (region, true)
    });
    let read = copies.iter().filter(|_| same).map(|c| {
        let region = Region {
            level: c.src.level,
            layers: (c.src.base_layer, c.src.layers),
            origin: c.src_origin,
            extent: c.extent,
            aspect: c.src.aspect,
        };
        (region, false)
    });
    let count = copies.len().saturating_mul(1 + usize::from(same));
    let mut regions =
        tairix_util::fallible::collected(count, written.chain(read)).ok_or(Error::OutOfMemory)?;
    Ok(overlap(&mut regions))
}

const fn of_copy(copy: &BufferImageCopy) -> Region {
    Region {
        level: copy.image.level,
        layers: (copy.image.base_layer, copy.image.layers),
        origin: copy.origin,
        extent: copy.extent,
        aspect: copy.image.aspect,
    }
}

/// A copy between `buffer` and `image` checked whole: every region within
/// both, none overlapping another where it writes, which was decided as it
/// was recorded, and the two not sharing memory.
pub(crate) fn buffer_image<B: Backend>(
    context: &ContextState<B>,
    (buffer_key, image_key): (BufferKey, ImageKey),
    (copies, overlapping): (&[BufferImageCopy], bool),
    direction: Direction,
) -> Result<Op<B>, Error> {
    let (buffer_use, image_use) = match direction {
        Direction::ToImage => (BufferUsage::TRANSFER_SRC, ImageUsage::TRANSFER_DST),
        Direction::ToBuffer => (BufferUsage::TRANSFER_DST, ImageUsage::TRANSFER_SRC),
    };
    let held = buffer(context, buffer_key, buffer_use)?;
    let image = image(context, image_key, image_use)?;
    bounded(copies)?;
    for copy in copies {
        let span = copy.check(&image.desc)?;
        if copy
            .buffer_offset
            .checked_add(span)
            .is_none_or(|end| end > held.size)
        {
            return Err(Error::OutOfRange);
        }
    }
    if aliases(held, image) || overlapping {
        return Err(Error::Overlap);
    }
    Ok(Op::BufferImage {
        buffer: Arc::clone(held),
        image: Arc::clone(image),
        regions: copied(copies)?,
        direction,
    })
}

/// A copy between two images checked whole: every region within both, of a
/// plane each holds and texels of one size, of one sample count, and none
/// written twice or read where written, which was decided as it was
/// recorded.
pub(crate) fn image_image<B: Backend>(
    context: &ContextState<B>,
    (src_key, dst_key): (ImageKey, ImageKey),
    (copies, overlapping): (&[ImageCopy], bool),
) -> Result<Op<B>, Error> {
    let src = image(context, src_key, ImageUsage::TRANSFER_SRC)?;
    let dst = image(context, dst_key, ImageUsage::TRANSFER_DST)?;
    bounded(copies)?;
    if src.desc.samples != dst.desc.samples {
        return Err(Error::Samples);
    }
    for copy in copies {
        copy.check(&src.desc, &dst.desc)?;
        let aspect = copy.src.aspect;
        let sized = src.desc.format.copy_bytes(aspect) == dst.desc.format.copy_bytes(aspect);
        let colour = aspect == Aspects::COLOR;
        if !sized || (!colour && src.desc.format != dst.desc.format) {
            return Err(Error::Shape);
        }
    }
    let shared = !Arc::ptr_eq(src, dst)
        && Arc::ptr_eq(&src.memory, &dst.memory)
        && overlaps(&bytes_of(src), &bytes_of(dst));
    if overlapping || shared {
        return Err(Error::Overlap);
    }
    Ok(Op::CopyImage {
        src: Arc::clone(src),
        dst: Arc::clone(dst),
        regions: copied(copies)?,
    })
}

/// A clear checked whole: a value of the kind the format holds, every range
/// within the image.
pub(crate) fn clear<B: Backend>(
    context: &ContextState<B>,
    image_key: ImageKey,
    value: ClearValue,
    ranges: &[ImageRange],
) -> Result<Op<B>, Error> {
    let image = image(context, image_key, ImageUsage::TRANSFER_DST)?;
    bounded(ranges)?;
    value.check(image.desc.format)?;
    for range in ranges {
        range.check(&image.desc)?;
    }
    Ok(Op::ClearImage {
        image: Arc::clone(image),
        value,
        ranges: copied(ranges)?,
    })
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;

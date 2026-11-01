//! What a context makes over its memory besides buffers — images, their
//! views, texel buffers — and samplers, each a state its handle, the bind sets
//! naming it and queued work share, released when the last lets it go.

use alloc::sync::Arc;

use tairix_spirv::enumerant::Capability;

use crate::image::{BufferViewDesc, ImageDesc, ImageUsage, ImageViewDesc, ViewDimension};
use crate::info::DeviceInfo;
use crate::memory::{Charge, Memory};
use crate::model::{
    shared_bytes, AllocationKey, BufferKey, BufferState, BufferViewKey, ContextKey, ImageKey,
    Model, SamplerKey, ViewKey,
};
use crate::sampler::SamplerDesc;
use crate::software::layout::ImageLayout;
use crate::Error;

/// An image: its memory, where in it the image starts, and its layout there.
#[derive(Debug)]
pub(crate) struct ImageState {
    pub(crate) memory: Arc<Memory>,
    pub(crate) offset: u64,
    pub(crate) desc: ImageDesc,
    pub(crate) layout: ImageLayout,
    _charge: Charge,
}

/// A view of an image.
#[derive(Debug)]
pub(crate) struct ViewState {
    pub(crate) image: Arc<ImageState>,
    pub(crate) desc: ImageViewDesc,
    _charge: Charge,
}

/// A texel buffer over a buffer.
#[derive(Debug)]
pub(crate) struct BufferViewState {
    pub(crate) buffer: Arc<BufferState>,
    pub(crate) desc: BufferViewDesc,
    _charge: Charge,
}

/// A sampler.
#[derive(Debug)]
pub(crate) struct SamplerState {
    pub(crate) desc: SamplerDesc,
    _charge: Charge,
}

/// What an image needs of the memory it is placed in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MemoryRequirements {
    /// The bytes it spans.
    pub size: u64,
    /// What its offset into an allocation must be a multiple of: a power of
    /// two.
    pub alignment: u64,
    /// The memory types it may be placed in: bit `i` for type `i`.
    pub memory_types: u32,
}

/// How an image of `desc` is laid out, once the device of `info` admits it:
/// an image made for storage no larger than an invocation's offsets reach.
fn image_layout(info: &DeviceInfo, desc: &ImageDesc) -> Result<ImageLayout, Error> {
    desc.check(info)?;
    let layout = ImageLayout::of(desc)?;
    if desc.usage.contains(ImageUsage::STORAGE) && layout.size > info.limits.max_storage_image_bytes
    {
        return Err(Error::TooLarge);
    }
    Ok(layout)
}

/// What an image of `desc` needs of its memory, or why the device of `info`
/// makes none: a question of the device's description alone, so asked without
/// its lock.
pub(crate) fn image_requirements(
    info: &DeviceInfo,
    desc: &ImageDesc,
) -> Result<MemoryRequirements, Error> {
    let layout = image_layout(info, desc)?;
    let types = info.memory_types.len();
    let memory_types = u32::try_from((1u64 << types) - 1).map_err(|_| Error::Malformed)?;
    Ok(MemoryRequirements {
        size: layout.size,
        alignment: ImageLayout::alignment(),
        memory_types,
    })
}

impl Model {
    /// An image of `desc` placed at `offset` in the allocation.
    pub(crate) fn create_image(
        &mut self,
        key: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error> {
        let layout = image_layout(self.info(), desc)?;
        if !offset.is_multiple_of(ImageLayout::alignment()) {
            return Err(Error::Misaligned);
        }
        let memory = {
            let held = self
                .live(key)?
                .allocations
                .get(allocation)
                .ok_or(Error::Stale)?;
            if offset
                .checked_add(layout.size)
                .is_none_or(|end| end > held.size)
            {
                return Err(Error::OutOfRange);
            }
            Arc::clone(&held.memory)
        };
        let bytes = shared_bytes::<ImageState>();
        self.file_charged(
            key,
            |context| &mut context.images,
            bytes,
            |charge| {
                crate::shared(ImageState {
                    memory,
                    offset,
                    desc: *desc,
                    layout,
                    _charge: charge,
                })
            },
        )
    }

    /// A view of the image.
    pub(crate) fn create_view(
        &mut self,
        key: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<ViewKey, Error> {
        let cubes = self.info().runs(Capability::SAMPLED_CUBE_ARRAY);
        if desc.dimension == ViewDimension::CubeArray && !cubes {
            return Err(Error::Unsupported);
        }
        let image = {
            let image = self.live(key)?.images.get(image).ok_or(Error::Stale)?;
            desc.check(&image.desc)?;
            Arc::clone(image)
        };
        let bytes = shared_bytes::<ViewState>();
        self.file_charged(
            key,
            |context| &mut context.views,
            bytes,
            |charge| {
                crate::shared(ViewState {
                    image,
                    desc: *desc,
                    _charge: charge,
                })
            },
        )
    }

    /// A texel buffer over the buffer.
    pub(crate) fn create_buffer_view(
        &mut self,
        key: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<BufferViewKey, Error> {
        let buffer = {
            let buffer = self.live(key)?.buffers.get(buffer).ok_or(Error::Stale)?;
            desc.check(buffer.size, buffer.usage, self.info())?;
            Arc::clone(buffer)
        };
        let bytes = shared_bytes::<BufferViewState>();
        self.file_charged(
            key,
            |context| &mut context.buffer_views,
            bytes,
            |charge| {
                crate::shared(BufferViewState {
                    buffer,
                    desc: *desc,
                    _charge: charge,
                })
            },
        )
    }

    /// A sampler.
    pub(crate) fn create_sampler(
        &mut self,
        key: ContextKey,
        desc: &SamplerDesc,
    ) -> Result<SamplerKey, Error> {
        desc.check(self.info())?;
        let bytes = shared_bytes::<SamplerState>();
        self.file_charged(
            key,
            |context| &mut context.samplers,
            bytes,
            |charge| {
                crate::shared(SamplerState {
                    desc: *desc,
                    _charge: charge,
                })
            },
        )
    }
}

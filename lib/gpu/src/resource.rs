//! What a context makes over its memory besides buffers — images, their
//! views, texel buffers — and samplers, each a state its handle, the bind sets
//! naming it and queued work share, released when the last lets it go.

use alloc::sync::Arc;

use tairix_spirv::enumerant::Capability;

use crate::backend::Backend;
use crate::device::Shared;
use crate::image::{BufferViewDesc, ImageDesc, ImageUsage, ImageViewDesc, ViewDimension};
use crate::memory::Charge;
use crate::model::{
    shared_bytes, AllocationKey, BufferKey, BufferState, BufferViewKey, ContextKey, ImageKey,
    Model, SamplerKey, ViewKey,
};
use crate::sampler::SamplerDesc;
use crate::Error;

/// An image: its memory, where in it the image starts and the bytes it
/// takes there, and how its backend lays its texels out.
#[derive(Debug)]
pub(crate) struct ImageState<B: Backend> {
    pub(crate) memory: Arc<B::Memory>,
    pub(crate) offset: u64,
    /// Its requirements' size: where in its memory its texels may lie.
    pub(crate) size: u64,
    pub(crate) desc: ImageDesc,
    pub(crate) form: B::Image,
    _charge: Charge,
}

/// A view of an image.
#[derive(Debug)]
pub(crate) struct ViewState<B: Backend> {
    pub(crate) image: Arc<ImageState<B>>,
    pub(crate) desc: ImageViewDesc,
    _charge: Charge,
}

/// A texel buffer over a buffer.
#[derive(Debug)]
pub(crate) struct BufferViewState<B: Backend> {
    pub(crate) buffer: Arc<BufferState<B>>,
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

impl<B: Backend> Shared<B> {
    /// How an image of `desc` lies in memory and what it needs of it, or why
    /// the device makes none: an image made for storage no larger than an
    /// invocation's offsets reach. Asked without the device's lock.
    pub(crate) fn image_form(
        &self,
        desc: &ImageDesc,
    ) -> Result<(B::Image, MemoryRequirements), Error> {
        desc.check(&self.info)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let (form, requirements) = self.backend.image(&self.info, desc)?;
        if desc.usage.contains(ImageUsage::STORAGE)
            && requirements.size > self.info.limits.max_storage_image_bytes
        {
            return Err(Error::TooLarge);
        }
        Ok((form, requirements))
    }

    /// An image of `desc` placed at `offset` in the context's allocation.
    pub(crate) fn create_image(
        &self,
        context: ContextKey,
        (allocation, offset): (AllocationKey, u64),
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error> {
        let (form, requirements) = self.image_form(desc)?;
        self.model
            .lock()
            .create_image(context, (allocation, offset), desc, (form, requirements))
    }
}

impl<B: Backend> Model<B> {
    /// An image of `desc` laid out as `form` asks, placed at `offset` in the
    /// allocation: one of the memory types its requirements name, at an
    /// offset of their alignment, and holding their size.
    fn create_image(
        &mut self,
        key: ContextKey,
        (allocation, offset): (AllocationKey, u64),
        desc: &ImageDesc,
        (form, requirements): (B::Image, MemoryRequirements),
    ) -> Result<ImageKey, Error> {
        if !offset.is_multiple_of(requirements.alignment) {
            return Err(Error::Misaligned);
        }
        let memory = {
            let held = self
                .live(key)?
                .allocations
                .get(allocation)
                .ok_or(Error::Stale)?;
            let placeable = 1u32
                .checked_shl(held.memory_type)
                .is_some_and(|bit| requirements.memory_types & bit != 0);
            if !placeable {
                return Err(Error::Usage);
            }
            if offset
                .checked_add(requirements.size)
                .is_none_or(|end| end > held.size)
            {
                return Err(Error::OutOfRange);
            }
            Arc::clone(&held.memory)
        };
        let bytes = shared_bytes::<ImageState<B>>();
        self.file_charged(
            key,
            |context| &mut context.images,
            bytes,
            |charge| {
                crate::shared(ImageState {
                    memory,
                    offset,
                    size: requirements.size,
                    desc: *desc,
                    form,
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
        let bytes = shared_bytes::<ViewState<B>>();
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
        let bytes = shared_bytes::<BufferViewState<B>>();
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

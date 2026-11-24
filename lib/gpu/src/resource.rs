//! What a context makes over its memory besides buffers — images, their
//! views, texel buffers — and samplers, each a state its handle, the bind sets
//! naming it and queued work share, released when the last lets it go. Each
//! is looked up under the device's lock, made by the backend without it, as a
//! device may be waited on for it, and filed under it.

use alloc::sync::Arc;

use tairix_spirv::enumerant::Capability;

use crate::backend::Backend;
use crate::device::Shared;
use crate::image::{
    BufferViewDesc, ImageDesc, ImageUsage, ImageViewDesc, ViewDimension, VIEW_USES,
};
use crate::memory::Charge;
use crate::model::{
    shared_bytes, AllocationKey, BufferKey, BufferState, BufferViewKey, ContextKey, ImageKey,
    Model, PickPlanned, SamplerKey, ViewKey,
};
use crate::sampler::SamplerDesc;
use crate::Error;

/// An image: its memory, where in it the image starts and the bytes it
/// takes there, and the image as its backend holds it.
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
    pub(crate) form: B::View,
    _charge: Charge,
}

/// A texel buffer over a buffer.
#[derive(Debug)]
pub(crate) struct BufferViewState<B: Backend> {
    pub(crate) buffer: Arc<BufferState<B>>,
    pub(crate) desc: BufferViewDesc,
    pub(crate) form: B::BufferView,
    _charge: Charge,
}

/// A sampler.
#[derive(Debug)]
pub(crate) struct SamplerState<B: Backend> {
    pub(crate) desc: SamplerDesc,
    pub(crate) form: B::Sampler,
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

/// Where an image is asked to lie: in the context's device, in the
/// allocation's memory of its type and size.
struct Placement<B: Backend> {
    device: Arc<B::Context>,
    memory: Arc<B::Memory>,
    memory_type: u32,
    size: u64,
}

impl<B: Backend> Placement<B> {
    /// Refuse an image of `requirements` at `offset`: off their alignment,
    /// in a memory type they do not name, or past the allocation.
    fn admits(&self, offset: u64, requirements: &MemoryRequirements) -> Result<(), Error> {
        if !offset.is_multiple_of(requirements.alignment) {
            return Err(Error::Misaligned);
        }
        let placeable = 1u32
            .checked_shl(self.memory_type)
            .is_some_and(|bit| requirements.memory_types & bit != 0);
        if !placeable {
            return Err(Error::Usage);
        }
        if offset
            .checked_add(requirements.size)
            .is_none_or(|end| end > self.size)
        {
            return Err(Error::OutOfRange);
        }
        Ok(())
    }
}

impl<B: Backend> Shared<B> {
    /// An image of `desc` in `device` and what it needs of its memory, or
    /// why the device makes none: an image made for storage no larger than
    /// an invocation's offsets reach.
    pub(crate) fn image_form(
        &self,
        device: &Arc<B::Context>,
        desc: &ImageDesc,
    ) -> Result<(B::Image, MemoryRequirements), Error> {
        desc.check(&self.info)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let (form, requirements) = self.backend.image(device, &self.info, desc)?;
        if desc.usage.contains(ImageUsage::STORAGE)
            && requirements.size > self.info.limits.max_storage_image_bytes
        {
            return Err(Error::TooLarge);
        }
        Ok((form, requirements))
    }

    /// What an image of `desc` needs of the memory it is placed in, as the
    /// context's device answers.
    pub(crate) fn image_requirements(
        &self,
        context: ContextKey,
        desc: &ImageDesc,
    ) -> Result<MemoryRequirements, Error> {
        let device = self.model.lock().device(context)?;
        self.image_form(&device, desc)
            .map(|(_, requirements)| requirements)
    }

    /// An image of `desc` placed at `offset` in the context's allocation:
    /// one of the memory types its requirements name, at an offset of their
    /// alignment, and holding their size.
    pub(crate) fn create_image(
        &self,
        context: ContextKey,
        (allocation, offset): (AllocationKey, u64),
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error> {
        let images: PickPlanned<B, ImageKey, Arc<ImageState<B>>> = |context| &mut context.images;
        let placement = self.model.lock().placement(context, allocation)?;
        let (form, requirements) = self.image_form(&placement.device, desc)?;
        placement.admits(offset, &requirements)?;
        // Placing zeroes what the image lies over, so nothing is refused after.
        let charge = self
            .model
            .lock()
            .reserve((context, images), shared_bytes::<ImageState<B>>())?;
        let made = crate::shared(ImageState {
            memory: Arc::clone(&placement.memory),
            offset,
            size: requirements.size,
            desc: *desc,
            form,
            _charge: charge,
        })
        .and_then(|state| {
            self.backend
                .place(&state.form, &placement.memory, offset, requirements.size)?;
            Ok(state)
        });
        let filed = match made {
            Ok(state) => self.model.lock().file_reserved((context, images), state),
            Err(error) => {
                self.model.lock().unreserve((context, images));
                Err(error)
            }
        };
        // What filing refused goes with the lock let go: it may hold the
        // last of a memory.
        drop(placement);
        filed
    }

    /// A view of the image.
    pub(crate) fn create_view(
        &self,
        context: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<ViewKey, Error> {
        let image = self.model.lock().viewed(context, image, desc)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let form = self.backend.view(&image.form, &image.desc, desc)?;
        let mut parts = Some((image, form));
        let filed = self.model.lock().file_charged(
            context,
            |context| &mut context.views,
            shared_bytes::<ViewState<B>>(),
            |charge| {
                let (image, form) = parts.take().ok_or(Error::Malformed)?;
                crate::shared(ViewState {
                    image,
                    desc: *desc,
                    form,
                    _charge: charge,
                })
            },
        );
        drop(parts);
        filed
    }

    /// A texel buffer over the buffer.
    pub(crate) fn create_buffer_view(
        &self,
        context: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<BufferViewKey, Error> {
        let buffer = self.model.lock().texelled(context, buffer, desc)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let form = self
            .backend
            .buffer_view(&buffer.memory, buffer.offset + desc.offset, desc)?;
        let mut parts = Some((buffer, form));
        let filed = self.model.lock().file_charged(
            context,
            |context| &mut context.buffer_views,
            shared_bytes::<BufferViewState<B>>(),
            |charge| {
                let (buffer, form) = parts.take().ok_or(Error::Malformed)?;
                crate::shared(BufferViewState {
                    buffer,
                    desc: *desc,
                    form,
                    _charge: charge,
                })
            },
        );
        drop(parts);
        filed
    }

    /// A sampler.
    pub(crate) fn create_sampler(
        &self,
        context: ContextKey,
        desc: &SamplerDesc,
    ) -> Result<SamplerKey, Error> {
        desc.check(&self.info)?;
        let device = self.model.lock().device(context)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let mut form = Some(self.backend.sampler(&device, desc)?);
        let filed = self.model.lock().file_charged(
            context,
            |context| &mut context.samplers,
            shared_bytes::<SamplerState<B>>(),
            |charge| {
                crate::shared(SamplerState {
                    desc: *desc,
                    form: form.take().ok_or(Error::Malformed)?,
                    _charge: charge,
                })
            },
        );
        drop((form, device));
        filed
    }
}

impl<B: Backend> Model<B> {
    /// Where in the context's allocation an image is asked to lie.
    fn placement(&self, key: ContextKey, allocation: AllocationKey) -> Result<Placement<B>, Error> {
        let context = self.live(key)?;
        let held = context.allocations.get(allocation).ok_or(Error::Stale)?;
        Ok(Placement {
            device: Arc::clone(&context.device),
            memory: Arc::clone(&held.memory),
            memory_type: held.memory_type,
            size: held.size,
        })
    }

    /// The image a view of `desc` is asked of, which it sees, its format
    /// offering one of the uses its image was made for that a view serves.
    fn viewed(
        &self,
        key: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<Arc<ImageState<B>>, Error> {
        let cubes = self.info().runs(Capability::SAMPLED_CUBE_ARRAY);
        if desc.dimension == ViewDimension::CubeArray && !cubes {
            return Err(Error::Unsupported);
        }
        let image = self
            .live(key)?
            .images
            .table
            .get(image)
            .ok_or(Error::Stale)?;
        desc.check(&image.desc)?;
        let offered = self.info().formats.get(desc.format);
        let serves = VIEW_USES
            .iter()
            .any(|&(usage, feature)| image.desc.usage.contains(usage) && offered.contains(feature));
        if !serves {
            return Err(Error::Unsupported);
        }
        Ok(Arc::clone(image))
    }

    /// The buffer a texel buffer of `desc` is asked over, which holds it.
    fn texelled(
        &self,
        key: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<Arc<BufferState<B>>, Error> {
        let buffer = self.live(key)?.buffers.get(buffer).ok_or(Error::Stale)?;
        desc.check(buffer.size, buffer.usage, self.info())?;
        Ok(Arc::clone(buffer))
    }
}

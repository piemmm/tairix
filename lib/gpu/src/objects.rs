//! The handles of the objects a context makes over its memory besides
//! buffers: images, their views, texel buffers and samplers.

use alloc::sync::Arc;
use core::fmt;

use crate::command::Command;
use crate::device::{owned, Allocation, Buffer, CommandBuffer, Context, Shared};
use crate::image::{
    BufferImageCopy, BufferViewDesc, ClearValue, ImageCopy, ImageDesc, ImageRange, ImageViewDesc,
};
use crate::model::{BufferViewKey, ContextKey, ImageKey, SamplerKey, ViewKey};
use crate::resource::MemoryRequirements;
use crate::sampler::SamplerDesc;
use crate::{Error, MAX_COPY_REGIONS};

impl CommandBuffer {
    fn named_image(&self, image: &Image) -> Result<ImageKey, Error> {
        owned(&image.shared, image.context, &self.shared, self.context)?;
        Ok(image.key)
    }

    /// Copy `regions` of `src` into `dst`'s texels. No texel may be written
    /// twice, and the two may not share memory.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context, and
    /// [`Error::TooMany`] past [`MAX_COPY_REGIONS`] regions.
    pub fn copy_buffer_to_image(
        &mut self,
        src: &Buffer,
        dst: &Image,
        regions: &[BufferImageCopy],
    ) -> Result<(), Error> {
        let (src, dst) = (self.named(src)?, self.named_image(dst)?);
        let regions = listed(regions)?;
        let overlapping = crate::transfer::to_image_overlaps(&regions)?;
        self.record(Command::BufferToImage {
            src,
            dst,
            regions,
            overlapping,
        })
    }

    /// Copy `regions` of `src`'s texels into `dst`. No byte may be written
    /// twice, and the two may not share memory.
    ///
    /// # Errors
    ///
    /// As [`Self::copy_buffer_to_image`].
    pub fn copy_image_to_buffer(
        &mut self,
        src: &Image,
        dst: &Buffer,
        regions: &[BufferImageCopy],
    ) -> Result<(), Error> {
        let desc = *src.desc();
        let (src, dst) = (self.named_image(src)?, self.named(dst)?);
        let regions = listed(regions)?;
        let overlapping = crate::transfer::to_buffer_overlaps(&regions, &desc)?;
        self.record(Command::ImageToBuffer {
            src,
            dst,
            regions,
            overlapping,
        })
    }

    /// Copy `regions` of `src`'s texels into `dst`'s, of one texel size. No
    /// texel may be written twice, nor, within one image, both read and
    /// written.
    ///
    /// # Errors
    ///
    /// As [`Self::copy_buffer_to_image`].
    pub fn copy_image(
        &mut self,
        src: &Image,
        dst: &Image,
        regions: &[ImageCopy],
    ) -> Result<(), Error> {
        let (src, dst) = (self.named_image(src)?, self.named_image(dst)?);
        let regions = listed(regions)?;
        let overlapping = crate::transfer::image_copy_overlaps(&regions, src == dst)?;
        self.record(Command::CopyImage {
            src,
            dst,
            regions,
            overlapping,
        })
    }

    /// Write `value` to every texel of `ranges` of `image`, in each aspect a
    /// range names.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an image of another context, and
    /// [`Error::TooMany`] past [`MAX_COPY_REGIONS`] ranges.
    pub fn clear_image(
        &mut self,
        image: &Image,
        value: ClearValue,
        ranges: &[ImageRange],
    ) -> Result<(), Error> {
        let image = self.named_image(image)?;
        let ranges = listed(ranges)?;
        self.record(Command::ClearImage {
            image,
            value,
            ranges,
        })
    }
}

/// `items` copied for a command to keep, at most [`MAX_COPY_REGIONS`].
fn listed<T: Copy>(items: &[T]) -> Result<alloc::vec::Vec<T>, Error> {
    if items.len() > MAX_COPY_REGIONS {
        return Err(Error::TooMany);
    }
    tairix_util::fallible::collected(items.len(), items.iter().copied()).ok_or(Error::OutOfMemory)
}

impl Context {
    /// What an image of `desc` needs of the memory it is placed in.
    ///
    /// # Errors
    ///
    /// Why the device makes no such image: [`Error::Empty`], [`Error::Shape`],
    /// [`Error::TooLarge`], [`Error::Samples`], [`Error::Usage`] or
    /// [`Error::Unsupported`].
    pub fn image_requirements(&self, desc: &ImageDesc) -> Result<MemoryRequirements, Error> {
        crate::resource::image_requirements(&self.shared.info, desc)
    }

    /// An image placed `offset` bytes into one of this context's allocations.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's allocation, what
    /// [`Self::image_requirements`] refuses, [`Error::Misaligned`] for an
    /// offset off the image's alignment and [`Error::OutOfRange`] for an image
    /// the allocation does not hold there.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_image(
        &self,
        allocation: &Allocation,
        offset: u64,
        desc: &ImageDesc,
    ) -> Result<Image, Error> {
        owned(
            &allocation.shared,
            allocation.context,
            &self.shared,
            self.key,
        )?;
        let key = self
            .shared
            .model
            .lock()
            .create_image(self.key, allocation.key, offset, desc)?;
        Ok(Image {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            desc: *desc,
        })
    }

    /// A view of one of this context's images.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's image, [`Error::Shape`],
    /// [`Error::Aspect`], [`Error::Empty`] or [`Error::OutOfRange`] for a view
    /// the image cannot be seen through, and [`Error::Unsupported`] for an
    /// array of cubes on a device that samples none.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_image_view(
        &self,
        image: &Image,
        desc: &ImageViewDesc,
    ) -> Result<ImageView, Error> {
        owned(&image.shared, image.context, &self.shared, self.key)?;
        let key = self
            .shared
            .model
            .lock()
            .create_view(self.key, image.key, desc)?;
        Ok(ImageView {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            desc: *desc,
        })
    }

    /// A texel buffer over one of this context's buffers.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's buffer, [`Error::Usage`] for
    /// one made for no texel-buffer use, [`Error::Unsupported`] for a format
    /// the device does not state for the use, and [`Error::Empty`],
    /// [`Error::Misaligned`], [`Error::OutOfRange`] or [`Error::TooLarge`] for
    /// a range the buffer cannot hold as asked.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_buffer_view(
        &self,
        buffer: &Buffer,
        desc: &BufferViewDesc,
    ) -> Result<BufferView, Error> {
        owned(&buffer.shared, buffer.context, &self.shared, self.key)?;
        let key = self
            .shared
            .model
            .lock()
            .create_buffer_view(self.key, buffer.key, desc)?;
        Ok(BufferView {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            desc: *desc,
        })
    }

    /// A sampler.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfRange`] or [`Error::Unsupported`] for one the device does
    /// not make.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_sampler(&self, desc: &SamplerDesc) -> Result<Sampler, Error> {
        let key = self.shared.model.lock().create_sampler(self.key, desc)?;
        Ok(Sampler {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            desc: *desc,
        })
    }
}

/// An image: typed, sized texels in memory laid out as the device lays them.
pub struct Image {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: ImageKey,
    desc: ImageDesc,
}

impl Image {
    /// How it was made.
    #[must_use]
    pub const fn desc(&self) -> &ImageDesc {
        &self.desc
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.images, self.key);
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

/// A view of an image: some of its levels, layers and planes, seen in one
/// dimension and format.
pub struct ImageView {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: ViewKey,
    desc: ImageViewDesc,
}

impl ImageView {
    /// How it was made.
    #[must_use]
    pub const fn desc(&self) -> &ImageViewDesc {
        &self.desc
    }
}

impl Drop for ImageView {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.views, self.key);
    }
}

impl fmt::Debug for ImageView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageView")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

/// A texel buffer: a buffer's range read, or read and written, as texels of a
/// format.
pub struct BufferView {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: BufferViewKey,
    desc: BufferViewDesc,
}

impl BufferView {
    /// How it was made.
    #[must_use]
    pub const fn desc(&self) -> &BufferViewDesc {
        &self.desc
    }
}

impl Drop for BufferView {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.buffer_views, self.key);
    }
}

impl fmt::Debug for BufferView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferView")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

/// How an image is sampled.
pub struct Sampler {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: SamplerKey,
    desc: SamplerDesc,
}

impl Sampler {
    /// How it was made.
    #[must_use]
    pub const fn desc(&self) -> &SamplerDesc {
        &self.desc
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.shared
            .release(self.context, |context| &mut context.samplers, self.key);
    }
}

impl fmt::Debug for Sampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sampler")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

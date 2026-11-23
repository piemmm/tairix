//! The device-neutral GPU seam: the memory and submission model every backend
//! implements, and the software device that implements it on every target
//! (`plans/GPU.md`).
//!
//! # The model
//!
//! * A [`Context`] is the unit of isolation. Its allocations, buffers,
//!   timelines and queues are its own, and a handle of another context or
//!   device is refused wherever it is named ([`Error::Foreign`]): sharing
//!   memory is an explicit [`Export`] another context imports.
//! * Memory is typed. A [`DeviceInfo`] enumerates its heaps and the memory
//!   types over them; an [`Allocation`] is made in one type and charged to its
//!   context's limit and its heap's size, refused past either, and then
//!   refused if the memory cannot be had.
//! * A [`Buffer`] is a span of an allocation made for stated uses, and is used
//!   for nothing else. An [`Image`] is typed texels placed in an allocation in a
//!   layout of the device's own, seen through an [`ImageView`]; a
//!   [`BufferView`] is a texel buffer over a buffer.
//! * Work is recorded into a [`CommandBuffer`] and handed to a [`Queue`] in a
//!   [`Submission`], which is checked whole before anything is queued: every
//!   buffer it names is its context's and live, every range is within what it
//!   names, every use is one the buffer was made for, every signal advances
//!   its timeline. A refused submission changes nothing but the room its
//!   queues made ahead of it, and nothing is clamped into range.
//! * A queue runs its work in the order submitted. A [`Timeline`] orders work
//!   across queues and with the CPU: a value is reached only once every signal
//!   up to it has landed. A [`Fence`] is one submission's completion.
//! * Every submission carries a deadline. One that runs past it loses its
//!   context ([`Loss::Overran`]); every other context's work goes on. A lost
//!   context refuses all work and only releases what it holds.
//!
//! # The software device
//!
//! [`Device::software`] runs every queue's work on the CPU, on a worker thread
//! of its own, or where none can be started on the thread whose act made the
//! work ready — the submission, the signal, or the completion before it. It
//! states every capability, feature and format use the seam names; it is the
//! reference the hardware backends are held to, and the device a program has
//! when the machine offers no other.
//!
//! # Threads
//!
//! Every handle may be used from any thread. A wait parks on the timeline it
//! waits for and is woken by what advances it; nothing spins.

#![no_std]
#![feature(allocator_api)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Bit for bit on every target: no float max or min, which may pick either zero.
#![deny(clippy::disallowed_methods)]

extern crate alloc;

#[cfg(test)]
extern crate std;

/// A handle to an object a context files: its device's host, its context and
/// its key, the object let go as `$object` when the handle is dropped.
macro_rules! handle {
    ($(#[$meta:meta])* $name:ident, $key:ident, $object:ident) => {
        $(#[$meta])*
        pub struct $name {
            pub(crate) host: alloc::sync::Arc<dyn crate::host::Host>,
            pub(crate) context: crate::model::ContextKey,
            pub(crate) key: crate::model::$key,
        }

        impl Drop for $name {
            fn drop(&mut self) {
                self.host
                    .release(self.context, crate::host::Object::$object(self.key));
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
}

mod backend;
mod binding;
mod command;
mod device;
mod error;
mod fair;
mod format;
mod hardware;
mod host;
mod image;
mod info;
mod memory;
mod model;
mod objects;
mod pass;
mod pipeline;
mod remote;
mod render;
mod resource;
mod room;
mod rt;
mod sampler;
mod serve;
mod software;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
mod transfer;
mod wire;

pub use binding::{
    BindSet, BindSetEntry, BindSetLayout, LayoutBinding, PipelineLayout, PushConstants, Resource,
    ShaderModule,
};
pub use command::{CopyRegion, CopySpan};
pub use device::{
    Allocation, AllocationDesc, Buffer, BufferDesc, CommandBuffer, Context, ContextDesc, Device,
    Export, Fence, Queue, Submission, Timeline,
};
pub use error::{Error, Loss, SubmitError};
pub use format::{Aspects, Format, FormatFeatures};
pub use hardware::{Commands, Hardware, Job, JobCommand};
pub use image::{
    level_extent, BufferImageCopy, BufferViewDesc, ClearValue, Extent3d, ImageCopy, ImageDesc,
    ImageDimension, ImageRange, ImageSubresource, ImageUsage, ImageViewDesc, Origin3d,
    ViewDimension,
};
pub use info::{
    BufferUsage, DeviceInfo, Features, FormatTable, Limits, MemoryHeap, MemoryProperties,
    MemoryType, QueueCapabilities, QueueFamily, MAX_CAPABILITIES, MAX_MEMORY_HEAPS,
    MAX_MEMORY_TYPES, MAX_QUEUES_PER_FAMILY, MAX_QUEUE_FAMILIES, MAX_SUBGROUP_SIZE,
};
pub use memory::MemoryUse;
pub use objects::{BufferView, Image, ImageView, Sampler};
pub use pass::{
    ColourAttachment, DepthStencilAttachment, Draw, Drawer, IndexFormat, Indices, Indirect, Load,
    Pass, Rect, Viewport, DRAW_RECORD_BYTES, INDEXED_DRAW_RECORD_BYTES, MAX_DRAW_VERTEX_BUFFERS,
    MAX_PASS_ATTACHMENTS,
};
pub use pipeline::{ComputePipeline, ComputePipelineDesc, MAX_DISPATCH_SETS, MAX_PUSH_BYTES};
pub use render::{
    Blend, BlendFactor, BlendOp, ColourMask, ColourTarget, CullMode, DepthBias, DepthStencilState,
    FrontFace, LogicOp, Multisample, Rasterization, RenderPipeline, RenderPipelineDesc, StageDesc,
    StencilFaceState, StencilOp, Topology, VertexAttribute, VertexBuffer, VertexStep,
    MAX_VERTEX_ATTRIBUTES,
};
pub use resource::MemoryRequirements;
pub use rt::{serve, MAX_SERVE_WORKERS};
pub use sampler::{AddressMode, BorderColor, CompareOp, Filter, SamplerDesc, MAX_LOD_BIAS};
pub use software::{NoThreads, RuntimeThreads, SoftwareDesc, Spawn, DEFAULT_SUBGROUP_SIZE};

/// The most command buffers one submission carries.
pub const MAX_SUBMISSION_BUFFERS: usize = 64;

/// The most commands one submission carries, across its command buffers.
pub const MAX_SUBMISSION_COMMANDS: usize = 65_536;

/// The most timeline values one submission waits for, and the most it sets.
pub const MAX_SUBMISSION_POINTS: usize = 64;

/// The most regions one copy moves.
pub const MAX_COPY_REGIONS: usize = 1024;

/// The most bytes one update writes.
pub const MAX_UPDATE_BYTES: usize = 65_536;

/// `value` shared, or a refusal where the memory cannot be had.
pub(crate) fn shared<T>(value: T) -> Result<alloc::sync::Arc<T>, Error> {
    alloc::sync::Arc::try_new(value).map_err(|_| Error::OutOfMemory)
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod device_tests;

#[cfg(test)]
#[path = "image_device_tests.rs"]
mod image_device_tests;

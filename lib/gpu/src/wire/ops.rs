//! Each `gpu-v1` op's request and reply bodies, defined once for both ends.
//!
//! A body is the op's fixed fields; what is longer lies in the client's
//! region, named by a [`RegionSpan`] the body carries.

use tairix_abi::gpu::{RegionSpan, GPU_EXPORT_TOKEN_LEN};
use tairix_abi::Duration64;

use super::{put_key, take_key, Reader, Wire, Writer};
use crate::binding::Named;
use crate::host::Object;
use crate::model::{
    AllocationKey, BindSetKey, BindSetLayoutKey, BufferKey, BufferViewKey, ComputePipelineKey,
    ImageKey, PipelineLayoutKey, RenderPipelineKey, SamplerKey, ShaderModuleKey, TimelineKey,
    ViewKey,
};
use crate::{
    AllocationDesc, BufferDesc, BufferViewDesc, Error, ImageDesc, ImageViewDesc,
    MemoryRequirements, MemoryUse, PushConstants, SamplerDesc,
};
use tairix_collections::{Key, SlotKey};

impl Wire for RegionSpan {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.offset.put(writer)?;
        self.len.put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            offset: Wire::take(reader)?,
            len: Wire::take(reader)?,
        })
    }
}

/// A key of the model as one word: checked against the context's tables
/// where it arrives.
macro_rules! wire_key {
    ($($key:ty),*) => {$(
        impl Wire for $key {
            fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
                put_key(writer, *self)
            }

            fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
                take_key(reader)
            }
        }
    )*};
}

wire_key!(
    AllocationKey,
    BufferKey,
    TimelineKey,
    ImageKey,
    ViewKey,
    BufferViewKey,
    SamplerKey,
    BindSetLayoutKey,
    BindSetKey,
    PipelineLayoutKey,
    ShaderModuleKey,
    ComputePipelineKey,
    RenderPipelineKey
);

/// A context as the runtime named it to the client.
pub(crate) type ContextId = u64;

/// An export as the runtime named it: random, so not guessed.
pub(crate) type ExportToken = [u8; GPU_EXPORT_TOKEN_LEN];

impl Wire for Object {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        let (kind, bits): (u8, SlotKey) = match *self {
            Self::Allocation(key) => (0, key.slot()),
            Self::Buffer(key) => (1, key.slot()),
            Self::Timeline(key) => (2, key.slot()),
            Self::Image(key) => (3, key.slot()),
            Self::View(key) => (4, key.slot()),
            Self::BufferView(key) => (5, key.slot()),
            Self::Sampler(key) => (6, key.slot()),
            Self::BindSetLayout(key) => (7, key.slot()),
            Self::BindSet(key) => (8, key.slot()),
            Self::PipelineLayout(key) => (9, key.slot()),
            Self::ShaderModule(key) => (10, key.slot()),
            Self::ComputePipeline(key) => (11, key.slot()),
            Self::RenderPipeline(key) => (12, key.slot()),
        };
        kind.put(writer)?;
        bits.to_bits().put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let kind = u8::take(reader)?;
        let slot = SlotKey::from_bits(u64::take(reader)?).ok_or(Error::Malformed)?;
        Ok(match kind {
            0 => Self::Allocation(Key::from_slot(slot)),
            1 => Self::Buffer(Key::from_slot(slot)),
            2 => Self::Timeline(Key::from_slot(slot)),
            3 => Self::Image(Key::from_slot(slot)),
            4 => Self::View(Key::from_slot(slot)),
            5 => Self::BufferView(Key::from_slot(slot)),
            6 => Self::Sampler(Key::from_slot(slot)),
            7 => Self::BindSetLayout(Key::from_slot(slot)),
            8 => Self::BindSet(Key::from_slot(slot)),
            9 => Self::PipelineLayout(Key::from_slot(slot)),
            10 => Self::ShaderModule(Key::from_slot(slot)),
            11 => Self::ComputePipeline(Key::from_slot(slot)),
            12 => Self::RenderPipeline(Key::from_slot(slot)),
            _ => return Err(Error::Malformed),
        })
    }
}

/// A struct, its fields in order.
macro_rules! body {
    ($(#[$meta:meta])* $name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq)]
        pub(crate) struct $name {
            $(pub(crate) $field: $ty,)*
        }

        impl Wire for $name {
            fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
                $(self.$field.put(writer)?;)*
                Ok(())
            }

            fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
                Ok(Self { $($field: Wire::take(reader)?),* })
            }
        }
    };
}

body!(
    /// [`GpuOp::Open`](tairix_abi::gpu::GpuOp::Open): the grant of the
    /// client's region.
    Open { grant: u64 }
);
body!(
    /// An op naming nothing but a span of the region.
    Spanned { span: RegionSpan }
);
body!(
    /// What a span of the region answers: how many of its bytes were
    /// written.
    Written { len: u32 }
);
body!(
    /// [`GpuOp::CreateContext`](tairix_abi::gpu::GpuOp::CreateContext).
    CreateContext {
        memory_limit: Option<u64>
    }
);
body!(
    /// An op naming a context alone.
    OfContext { context: ContextId }
);
body!(
    /// [`GpuOp::MemoryUsed`](tairix_abi::gpu::GpuOp::MemoryUsed)'s answer.
    Used { used: MemoryUse }
);
body!(
    /// [`GpuOp::Release`](tairix_abi::gpu::GpuOp::Release).
    Release {
        context: ContextId,
        object: Object
    }
);
body!(
    /// [`GpuOp::Allocate`](tairix_abi::gpu::GpuOp::Allocate).
    Allocate {
        context: ContextId,
        desc: AllocationDesc
    }
);
body!(
    /// [`GpuOp::Write`](tairix_abi::gpu::GpuOp::Write) and
    /// [`GpuOp::Read`](tairix_abi::gpu::GpuOp::Read): the span's bytes, at
    /// `offset` into the allocation.
    HostAccess {
        context: ContextId,
        allocation: AllocationKey,
        offset: u64,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::Attach`](tairix_abi::gpu::GpuOp::Attach): the grant of a
    /// region of the client's own, and how many of its bytes it attaches.
    Attach { grant: u64, len: u64 }
);
body!(
    /// [`GpuOp::Attach`](tairix_abi::gpu::GpuOp::Attach)'s answer and
    /// [`GpuOp::Detach`](tairix_abi::gpu::GpuOp::Detach): an attachment of
    /// the client's.
    Attached { attachment: u32 }
);
body!(
    /// [`GpuOp::ReadInto`](tairix_abi::gpu::GpuOp::ReadInto) and
    /// [`GpuOp::WriteFrom`](tairix_abi::gpu::GpuOp::WriteFrom): `len` bytes,
    /// at `offset` into the allocation and `at` into the attachment.
    Transfer {
        context: ContextId,
        allocation: AllocationKey,
        offset: u64,
        attachment: u32,
        at: u64,
        len: u64
    }
);
body!(
    /// [`GpuOp::Export`](tairix_abi::gpu::GpuOp::Export).
    Export {
        context: ContextId,
        allocation: AllocationKey
    }
);
body!(
    /// [`GpuOp::Export`](tairix_abi::gpu::GpuOp::Export)'s answer and
    /// [`GpuOp::DropExport`](tairix_abi::gpu::GpuOp::DropExport).
    Token { token: ExportToken }
);
body!(
    /// [`GpuOp::Import`](tairix_abi::gpu::GpuOp::Import).
    Import {
        context: ContextId,
        token: ExportToken
    }
);
body!(
    /// [`GpuOp::Import`](tairix_abi::gpu::GpuOp::Import)'s answer.
    Imported {
        allocation: AllocationKey,
        desc: AllocationDesc
    }
);
body!(
    /// [`GpuOp::CreateBuffer`](tairix_abi::gpu::GpuOp::CreateBuffer).
    CreateBuffer {
        context: ContextId,
        allocation: AllocationKey,
        desc: BufferDesc
    }
);
body!(
    /// [`GpuOp::CreateTimeline`](tairix_abi::gpu::GpuOp::CreateTimeline).
    CreateTimeline {
        context: ContextId,
        initial: u64
    }
);
body!(
    /// An op naming a timeline of a context.
    OfTimeline {
        context: ContextId,
        timeline: TimelineKey
    }
);
body!(
    /// [`GpuOp::Signal`](tairix_abi::gpu::GpuOp::Signal) and
    /// [`GpuOp::Reached`](tairix_abi::gpu::GpuOp::Reached).
    AtValue {
        context: ContextId,
        timeline: TimelineKey,
        value: u64
    }
);
body!(
    /// [`GpuOp::Wait`](tairix_abi::gpu::GpuOp::Wait).
    Wait {
        context: ContextId,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64
    }
);
body!(
    /// A value answered.
    Value { value: u64 }
);
body!(
    /// A yes or no answered.
    Truth { truth: bool }
);
body!(
    /// [`GpuOp::Upload`](tairix_abi::gpu::GpuOp::Upload): commands in the
    /// span, `last` where they end a command buffer.
    Upload {
        context: ContextId,
        last: bool,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::Submit`](tairix_abi::gpu::GpuOp::Submit): its waits and
    /// signals in the span, each a list of timelines and values.
    Submit {
        context: ContextId,
        queue: u32,
        deadline: Duration64,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::Submit`](tairix_abi::gpu::GpuOp::Submit)'s answer: its
    /// fence.
    Fence {
        timeline: TimelineKey,
        value: u64
    }
);
body!(
    /// An op of a context whose description lies in the span.
    Described {
        context: ContextId,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::CreateBindSet`](tairix_abi::gpu::GpuOp::CreateBindSet).
    CreateBindSet {
        context: ContextId,
        layout: BindSetLayoutKey,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::CreatePipelineLayout`](tairix_abi::gpu::GpuOp::CreatePipelineLayout).
    CreatePipelineLayout {
        context: ContextId,
        push_constants: PushConstants,
        span: RegionSpan
    }
);
body!(
    /// [`GpuOp::ImageRequirements`](tairix_abi::gpu::GpuOp::ImageRequirements).
    ImageRequirements {
        context: ContextId,
        desc: ImageDesc
    }
);
body!(
    /// [`GpuOp::ImageRequirements`](tairix_abi::gpu::GpuOp::ImageRequirements)'s
    /// answer.
    Requirements {
        requirements: MemoryRequirements
    }
);
body!(
    /// [`GpuOp::CreateImage`](tairix_abi::gpu::GpuOp::CreateImage).
    CreateImage {
        context: ContextId,
        allocation: AllocationKey,
        offset: u64,
        desc: ImageDesc
    }
);
body!(
    /// [`GpuOp::CreateView`](tairix_abi::gpu::GpuOp::CreateView).
    CreateView {
        context: ContextId,
        image: ImageKey,
        desc: ImageViewDesc
    }
);
body!(
    /// [`GpuOp::CreateBufferView`](tairix_abi::gpu::GpuOp::CreateBufferView).
    CreateBufferView {
        context: ContextId,
        buffer: BufferKey,
        desc: BufferViewDesc
    }
);
body!(
    /// [`GpuOp::CreateSampler`](tairix_abi::gpu::GpuOp::CreateSampler).
    CreateSampler {
        context: ContextId,
        desc: SamplerDesc
    }
);
body!(
    /// [`GpuOp::CreateComputePipeline`](tairix_abi::gpu::GpuOp::CreateComputePipeline):
    /// its entry point's name and specialization in the span.
    CreateComputePipeline {
        context: ContextId,
        module: ShaderModuleKey,
        layout: PipelineLayoutKey,
        span: RegionSpan
    }
);
body!(
    /// A key answered.
    Made { key: u64 }
);

impl Made {
    pub(crate) fn of<K: Key>(key: K) -> Self {
        Self {
            key: key.slot().to_bits(),
        }
    }

    pub(crate) fn key<K: Key>(self) -> Result<K, Error> {
        SlotKey::from_bits(self.key)
            .map(K::from_slot)
            .ok_or(Error::Malformed)
    }
}

impl Wire for Named {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        match *self {
            Self::Sampler(sampler) => {
                0u8.put(writer)?;
                sampler.put(writer)
            }
            Self::Image(view) => {
                1u8.put(writer)?;
                view.put(writer)
            }
            Self::ImageSampler(view, sampler) => {
                2u8.put(writer)?;
                view.put(writer)?;
                sampler.put(writer)
            }
            Self::TexelBuffer(view) => {
                3u8.put(writer)?;
                view.put(writer)
            }
            Self::Buffer {
                buffer,
                offset,
                size,
            } => {
                4u8.put(writer)?;
                buffer.put(writer)?;
                offset.put(writer)?;
                size.put(writer)
            }
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::take(reader)? {
            0 => Self::Sampler(Wire::take(reader)?),
            1 => Self::Image(Wire::take(reader)?),
            2 => Self::ImageSampler(Wire::take(reader)?, Wire::take(reader)?),
            3 => Self::TexelBuffer(Wire::take(reader)?),
            4 => Self::Buffer {
                buffer: Wire::take(reader)?,
                offset: Wire::take(reader)?,
                size: Wire::take(reader)?,
            },
            _ => return Err(Error::Malformed),
        })
    }
}

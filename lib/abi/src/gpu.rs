//! `gpu-v1`: the channel a GPU's driver serves its device on, to every
//! process holding [`CapabilityId::GPU`](crate::CapabilityId::GPU).
//!
//! A request is an 8-byte header — [`GPU_REQUEST_MAGIC`], [`GPU_VERSION_V1`]
//! and its [`GpuOp`] — then the op's body; a reply is a status word, the op it
//! answers and its body. What an op carries beyond a few words — a
//! description, a module, bytes written or read, commands — lies in the one
//! region the client created and granted at [`GpuOp::Open`], named in the body
//! by a [`RegionSpan`]: the server copies a span out before it reads it, and
//! writes what it answers into one. The bodies are the seam's descriptions,
//! encoded by `lib/gpu`; this module is the frame every one of them shares.
//!
//! The channel is a per-node device channel ([`GPU_CHANNEL_ENDPOINTS`]),
//! bound only by its node's driver, and only where a DMA translation unit
//! translates the node or the platform's firmware declares none;
//! restricted-sender on [`CapabilityId::GPU`](crate::CapabilityId::GPU), and
//! published as a child node carrying [`GPU_NODE_COMPATIBLE`].

use crate::hwtree::NodeEndpointBlock;
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::Errno;

/// The per-node endpoint block a GPU's driver binds its device channel in.
pub const GPU_CHANNEL_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"GP");

/// The match key of the node a GPU's driver publishes its channel under.
pub const GPU_NODE_COMPATIBLE: &[u8] = b"tairix,gpu";

/// Every request's first word.
pub const GPU_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"GPU1");

/// The protocol version this module speaks.
pub const GPU_VERSION_V1: u16 = 1;

/// Request header: magic (4), version (2), op (2).
pub const GPU_HEADER_LEN: usize = 8;

/// Reply header: status (4), op (2), reserved (2). A success names the op it
/// answers, so no reply reads as another op's.
pub const GPU_REPLY_HEADER_LEN: usize = 8;

/// The longest body a request or reply carries in its frame; anything longer
/// goes through the region.
pub const GPU_MAX_BODY: usize = 256;

/// The longest request frame.
pub const GPU_MAX_REQUEST: usize = GPU_HEADER_LEN + GPU_MAX_BODY;

/// The longest reply frame.
pub const GPU_MAX_REPLY: usize = GPU_REPLY_HEADER_LEN + GPU_MAX_BODY;

/// The smallest region a client may grant: room for any one command but a
/// pass at the seam's bounds, which a pass's draws are split across.
pub const GPU_REGION_MIN: usize = 128 << 10;

/// The largest region a client may grant: a containment bound on what one
/// client makes its server map and copy.
pub const GPU_REGION_MAX: usize = 16 << 20;

/// The most contexts one client holds on one served device at once: a
/// containment bound, not a capacity.
pub const MAX_CLIENT_CONTEXTS: usize = 64;

/// The most export tokens one client holds at once: a containment bound on
/// the record the server keeps of each.
pub const MAX_CLIENT_EXPORTS: usize = 4096;

/// The bytes of an export token: random, so no client imports memory it was
/// not handed a token for.
pub const GPU_EXPORT_TOKEN_LEN: usize = 16;

/// High tag of a GPU driver's desk port.
const DESK_ENDPOINT_TAG: u64 = 0x4744_0000_0000_0000;

/// The port a GPU's driver binds for its own threads to rouse the loop that
/// keeps its device, from its own kernel-attested `pid`. The id is
/// unreserved, and the driver admits only its own process to it; `pid` takes
/// the 40 bits [`crate::PID_MAX`] spans, so no pid reaches the tag.
#[must_use]
pub const fn desk_endpoint_for(pid: u64) -> u64 {
    DESK_ENDPOINT_TAG | (pid & crate::PID_MAX)
}

/// What a request asks of a served device: one op for each method of the
/// seam.
#[repr(u16)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum GpuOp {
    /// Map the client's region, and watch the client's exit.
    Open = 1,
    /// Let go of everything the client holds.
    Close = 2,
    /// The device's description, written into the region.
    Info = 3,
    /// A context.
    CreateContext = 4,
    /// Let a context go.
    DestroyContext = 5,
    /// Whether a context still takes work.
    Status = 6,
    /// What a context's memory limit is spent on.
    MemoryUsed = 7,
    /// Let an object of a context go.
    Release = 8,
    /// Memory.
    Allocate = 9,
    /// Write host-visible memory from the region.
    Write = 10,
    /// Read host-visible memory into the region.
    Read = 11,
    /// An export token for an allocation's memory.
    Export = 12,
    /// File memory an export token names in a context.
    Import = 13,
    /// Let an export token go.
    DropExport = 14,
    /// A buffer over an allocation.
    CreateBuffer = 15,
    /// A timeline.
    CreateTimeline = 16,
    /// Where a timeline stands.
    TimelineValue = 17,
    /// Set a timeline from the CPU.
    Signal = 18,
    /// Answered once a timeline reaches a value, its context is lost or
    /// destroyed, or the timeout passes.
    Wait = 19,
    /// Whether a timeline has reached a value.
    Reached = 20,
    /// Commands for a context's next submission, in region-sized parts.
    Upload = 21,
    /// Check and queue the commands uploaded since the last submission.
    Submit = 22,
    /// Let go the commands uploaded since the last submission.
    Discard = 23,
    /// A bind set layout.
    CreateBindSetLayout = 24,
    /// A bind set.
    CreateBindSet = 25,
    /// A pipeline layout.
    CreatePipelineLayout = 26,
    /// A shader module, its words in the region, admitted again on arrival.
    CreateShaderModule = 27,
    /// What an image needs of the memory it is placed in.
    ImageRequirements = 28,
    /// An image.
    CreateImage = 29,
    /// A view of an image.
    CreateView = 30,
    /// A texel buffer over a buffer.
    CreateBufferView = 31,
    /// A sampler.
    CreateSampler = 32,
    /// A compute pipeline.
    CreateComputePipeline = 33,
    /// A render pipeline.
    CreateRenderPipeline = 34,
}

impl GpuOp {
    /// Every op, in order.
    pub const ALL: [Self; 34] = [
        Self::Open,
        Self::Close,
        Self::Info,
        Self::CreateContext,
        Self::DestroyContext,
        Self::Status,
        Self::MemoryUsed,
        Self::Release,
        Self::Allocate,
        Self::Write,
        Self::Read,
        Self::Export,
        Self::Import,
        Self::DropExport,
        Self::CreateBuffer,
        Self::CreateTimeline,
        Self::TimelineValue,
        Self::Signal,
        Self::Wait,
        Self::Reached,
        Self::Upload,
        Self::Submit,
        Self::Discard,
        Self::CreateBindSetLayout,
        Self::CreateBindSet,
        Self::CreatePipelineLayout,
        Self::CreateShaderModule,
        Self::ImageRequirements,
        Self::CreateImage,
        Self::CreateView,
        Self::CreateBufferView,
        Self::CreateSampler,
        Self::CreateComputePipeline,
        Self::CreateRenderPipeline,
    ];

    /// The op `raw` names.
    #[must_use]
    pub fn from_raw(raw: u16) -> Option<Self> {
        let index = usize::from(raw).checked_sub(1)?;
        Self::ALL.get(index).copied()
    }

    /// Its number on the wire.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self as u16
    }
}

/// A request: its op and its body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GpuRequest<'a> {
    /// What it asks.
    pub op: GpuOp,
    /// The op's body, at most [`GPU_MAX_BODY`] bytes.
    pub body: &'a [u8],
}

impl<'a> GpuRequest<'a> {
    /// Read a request frame.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a frame short of its header,
    /// [`Errno::BadMagic`] for another protocol's,
    /// [`Errno::AbiVersionUnsupported`] for another version's,
    /// [`Errno::OutOfRange`] for an op there is not, and
    /// [`Errno::LengthOutOfRange`] for a body past [`GPU_MAX_BODY`].
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < GPU_HEADER_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != GPU_REQUEST_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != GPU_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = GpuOp::from_raw(read_u16(bytes, 6)).ok_or(Errno::OutOfRange)?;
        let body = &bytes[GPU_HEADER_LEN..];
        if body.len() > GPU_MAX_BODY {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(Self { op, body })
    }

    /// Write the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a body past [`GPU_MAX_BODY`], and
    /// [`Errno::BufferTooSmall`] for an `out` short of the frame.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        if self.body.len() > GPU_MAX_BODY {
            return Err(Errno::LengthOutOfRange);
        }
        let len = GPU_HEADER_LEN + self.body.len();
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        put_u32(frame, 0, GPU_REQUEST_MAGIC);
        put_u16(frame, 4, GPU_VERSION_V1);
        put_u16(frame, 6, self.op.raw());
        frame[GPU_HEADER_LEN..].copy_from_slice(self.body);
        Ok(len)
    }
}

/// What a reply's status word says.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GpuStatus {
    /// What was asked; the body is the op's answer.
    Ok,
    /// The frame refused before the device was asked: a malformed request, a
    /// region not granted, a client past its bounds.
    Refused(Errno),
    /// The device refused what was asked, for the [`GpuError`] named; the body
    /// carries its detail.
    Device(GpuError),
}

impl GpuStatus {
    fn word(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Refused(errno) => -errno.as_i32(),
            Self::Device(error) => i32::from(error.raw()),
        }
    }

    fn of_word(word: i32) -> Result<Self, Errno> {
        if word == 0 {
            return Ok(Self::Ok);
        }
        if word < 0 {
            return Errno::try_from_status(word)
                .map(Self::Refused)
                .ok_or(Errno::BadMagic);
        }
        u16::try_from(word)
            .ok()
            .and_then(GpuError::from_raw)
            .map(Self::Device)
            .ok_or(Errno::BadMagic)
    }
}

/// Write a reply to `op` into `out`, answering its length.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a body past [`GPU_MAX_BODY`], and
/// [`Errno::BufferTooSmall`] for an `out` short of the frame.
pub fn encode_reply(
    op: GpuOp,
    status: GpuStatus,
    body: &[u8],
    out: &mut [u8],
) -> Result<usize, Errno> {
    if body.len() > GPU_MAX_BODY {
        return Err(Errno::LengthOutOfRange);
    }
    let len = GPU_REPLY_HEADER_LEN + body.len();
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    put_i32(frame, 0, status.word());
    put_u16(frame, 4, op.raw());
    put_u16(frame, 6, 0);
    frame[GPU_REPLY_HEADER_LEN..].copy_from_slice(body);
    Ok(len)
}

/// Read a reply to `op`: its status and its body.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] for a frame short of its header,
/// [`Errno::BadMagic`] for a status no reply carries, a reply naming another
/// op or a reserved field set, and [`Errno::LengthOutOfRange`] for a body
/// past [`GPU_MAX_BODY`].
pub fn decode_reply(bytes: &[u8], op: GpuOp) -> Result<(GpuStatus, &[u8]), Errno> {
    if bytes.len() < GPU_REPLY_HEADER_LEN {
        return Err(Errno::BufferTooSmall);
    }
    let status = GpuStatus::of_word(read_i32(bytes, 0))?;
    if read_u16(bytes, 4) != op.raw() || read_u16(bytes, 6) != 0 {
        return Err(Errno::BadMagic);
    }
    let body = &bytes[GPU_REPLY_HEADER_LEN..];
    if body.len() > GPU_MAX_BODY {
        return Err(Errno::LengthOutOfRange);
    }
    Ok((status, body))
}

/// Why a device refused what a request asked: one code for each of the
/// seam's errors, its detail — a loss, a disagreement, a refused command —
/// in the reply's body.
#[repr(u16)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum GpuError {
    /// The context was lost.
    Lost = 1,
    /// The context was destroyed.
    Destroyed = 2,
    /// An object of another context or device.
    Foreign = 3,
    /// An object since released.
    Stale = 4,
    /// The memory could not be had.
    OutOfMemory = 5,
    /// Past the context's memory limit.
    OverLimit = 6,
    /// Past its heap's size.
    HeapFull = 7,
    /// A memory type the device does not have.
    NoSuchMemoryType = 8,
    /// A queue the device does not have.
    NoSuchQueue = 9,
    /// Past a bound of the device or the seam.
    TooLarge = 10,
    /// Of nothing.
    Empty = 11,
    /// Off its alignment.
    Misaligned = 12,
    /// Past what it names.
    OutOfRange = 13,
    /// A use it was not made for.
    Usage = 14,
    /// Memory the CPU cannot reach.
    NotHostVisible = 15,
    /// Ranges that meet where they may not.
    Overlap = 16,
    /// A signal that would not advance its timeline.
    TimelineOrder = 17,
    /// A deadline of zero or past the device's longest.
    Deadline = 18,
    /// More than a bound allows.
    TooMany = 19,
    /// The timeout passed first.
    TimedOut = 20,
    /// A description that is not one.
    Malformed = 21,
    /// A shape it does not take.
    Shape = 22,
    /// A sample count it does not take.
    Samples = 23,
    /// An aspect it does not have.
    Aspect = 24,
    /// What the device does not offer.
    Unsupported = 25,
    /// Bindings that do not fit their layout.
    Bindings = 26,
    /// An entry point the module does not have.
    EntryPoint = 27,
    /// A layout that does not provide what an entry point uses.
    Disagreement = 28,
    /// Specialization values the module refuses.
    Specialization = 29,
    /// Stages whose interfaces do not meet.
    Interface = 30,
    /// A sampling a layout's sample and sampler types do not take.
    Sampling = 31,
}

impl GpuError {
    /// Every code, in order.
    pub const ALL: [Self; 31] = [
        Self::Lost,
        Self::Destroyed,
        Self::Foreign,
        Self::Stale,
        Self::OutOfMemory,
        Self::OverLimit,
        Self::HeapFull,
        Self::NoSuchMemoryType,
        Self::NoSuchQueue,
        Self::TooLarge,
        Self::Empty,
        Self::Misaligned,
        Self::OutOfRange,
        Self::Usage,
        Self::NotHostVisible,
        Self::Overlap,
        Self::TimelineOrder,
        Self::Deadline,
        Self::TooMany,
        Self::TimedOut,
        Self::Malformed,
        Self::Shape,
        Self::Samples,
        Self::Aspect,
        Self::Unsupported,
        Self::Bindings,
        Self::EntryPoint,
        Self::Disagreement,
        Self::Specialization,
        Self::Interface,
        Self::Sampling,
    ];

    /// The code `raw` names.
    #[must_use]
    pub fn from_raw(raw: u16) -> Option<Self> {
        let index = usize::from(raw).checked_sub(1)?;
        Self::ALL.get(index).copied()
    }

    /// Its number on the wire.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self as u16
    }
}

/// A span of the client's region: `len` bytes at `offset`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RegionSpan {
    /// Where it starts.
    pub offset: u32,
    /// Its bytes.
    pub len: u32,
}

impl RegionSpan {
    /// Its bytes on the wire.
    pub const WIRE_LEN: usize = 8;

    /// The span as a range of a region of `region_len` bytes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a span past the region.
    pub fn within(self, region_len: usize) -> Result<core::ops::Range<usize>, Errno> {
        let start = usize::try_from(self.offset).map_err(|_| Errno::OutOfRange)?;
        let len = usize::try_from(self.len).map_err(|_| Errno::OutOfRange)?;
        let end = start.checked_add(len).ok_or(Errno::OutOfRange)?;
        if end > region_len {
            return Err(Errno::OutOfRange);
        }
        Ok(start..end)
    }

    /// Write it at the start of `out`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for an `out` shorter than [`Self::WIRE_LEN`].
    pub fn encode(self, out: &mut [u8]) -> Result<(), Errno> {
        let frame = out.get_mut(..Self::WIRE_LEN).ok_or(Errno::BufferTooSmall)?;
        put_u32(frame, 0, self.offset);
        put_u32(frame, 4, self.len);
        Ok(())
    }

    /// Read one from the start of `bytes`.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for `bytes` shorter than [`Self::WIRE_LEN`].
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        Ok(Self {
            offset: read_u32(bytes, 0),
            len: read_u32(bytes, 4),
        })
    }
}

/// One served GPU, as the System Information API lists it: the endpoint of
/// its channel, and the hardware-tree node it is the device channel of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GpuDeviceDescriptor {
    /// The channel a client opens the device on.
    pub endpoint: u64,
    /// The node of the device the channel serves.
    pub node: u32,
}

impl GpuDeviceDescriptor {
    /// Its bytes on the wire: the endpoint, the node, four reserved.
    pub const WIRE_LEN: usize = 16;

    /// Its wire bytes.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0; Self::WIRE_LEN];
        put_u64(&mut out, 0, self.endpoint);
        put_u32(&mut out, 8, self.node);
        out
    }

    /// One read from its wire bytes.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for bytes short of a record, and
    /// [`Errno::BadMagic`] for a reserved field set.
    pub fn from_le_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 12) != 0 {
            return Err(Errno::BadMagic);
        }
        Ok(Self {
            endpoint: read_u64(bytes, 0),
            node: read_u32(bytes, 8),
        })
    }
}

#[cfg(test)]
#[path = "gpu_tests.rs"]
mod tests;

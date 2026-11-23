//! A device a driver serves, as a client process reaches it: a [`Host`]
//! whose every call is a `gpu-v1` request on the device's channel.
//!
//! Bytes go through the client's one region, which a request holds from
//! writing them to reading its answer. Whatever is longer than the region —
//! memory written or read, a command buffer — crosses in parts; a pass longer
//! than the region crosses as its first draws and then the rest. A module
//! crosses whole, so the region is what bounds one.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::gpu::{
    decode_reply, GpuOp, GpuRequest, GpuStatus, RegionSpan, GPU_MAX_BODY, GPU_MAX_REPLY,
    GPU_MAX_REQUEST,
};
use tairix_abi::{Duration64, Errno};
use tairix_collections::Key;
use tairix_rt::sync::Mutex;
use tairix_spirv::{Admitted, SpecValue};

use crate::binding::{LayoutBinding, PushConstants, SetRequest};
use crate::command::{Command, Recorded};
use crate::device::{AllocationDesc, BufferDesc};
use crate::host::{ExportId, Host, Object};
use crate::image::{BufferViewDesc, ImageDesc, ImageViewDesc};
use crate::info::DeviceInfo;
use crate::memory::{Charge, Held, MemoryUse};
use crate::model::{
    AllocationKey, BindSetKey, BindSetLayoutKey, BufferKey, BufferViewKey, ComputePipelineKey,
    ContextKey, ImageKey, Imported, PipelineLayoutKey, RenderPipelineKey, Request, SamplerKey,
    ShaderModuleKey, TimelineKey, ViewKey,
};
use crate::render::{put_render, RenderRequest};
use crate::resource::MemoryRequirements;
use crate::sampler::SamplerDesc;
use crate::wire::ops::{self, ContextId};
use crate::wire::{
    put_command, put_first_draws, put_more_draws, put_name, take_error, take_info,
    take_submit_error, Reader, Wire, Writer,
};
use crate::{Error, Loss, SubmitError};

/// The channel a served device is reached on, and the client's region.
pub(crate) trait Link: Send + Sync {
    /// Send `request`, answering the length of its reply in `reply`. The call
    /// waits for its answer, a wait's for as long as the wait does.
    fn call(&self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
    /// The region's bytes.
    fn region_len(&self) -> usize;
    /// Copy `data` into the region at `at`.
    fn write_region(&self, at: usize, data: &[u8]);
    /// Copy `out.len()` bytes at `at` out of the region.
    fn read_region(&self, at: usize, out: &mut [u8]);
}

/// A served device's channel and the client's region.
struct Channel {
    link: Box<dyn Link>,
    /// Held by a request carrying bytes in the region, from writing them to
    /// reading its answer.
    region: Mutex<()>,
}

/// A served device, reached over its channel.
pub(crate) struct Remote {
    channel: Channel,
    info: DeviceInfo,
}

/// A device's answer to a request, or why there was none.
enum Fault {
    /// The device refused what was asked.
    Device(Error),
    /// The channel or the frame refused: the device cannot be reached.
    Channel(Errno),
}

impl From<Error> for Fault {
    fn from(error: Error) -> Self {
        Self::Device(error)
    }
}

/// What the seam calls a device that cannot be reached: lost, as its
/// contexts are.
const UNREACHABLE: Error = Error::Lost(Loss::Fault);

impl Fault {
    fn error(self) -> Error {
        match self {
            Self::Device(error) => error,
            Self::Channel(Errno::OutOfMemory) => Error::OutOfMemory,
            Self::Channel(_) => UNREACHABLE,
        }
    }
}

impl Remote {
    /// Open the device the link reaches, its region granted as `grant`, and
    /// learn what it is.
    ///
    /// # Errors
    ///
    /// The channel's refusal: a caller without the capability the channel
    /// demands is refused by the kernel before the device reads a byte.
    pub(crate) fn open(link: Box<dyn Link>, grant: u64) -> Result<Self, Errno> {
        let refusal = |fault: Fault| match fault {
            Fault::Channel(errno) => errno,
            Fault::Device(Error::OutOfMemory) => Errno::OutOfMemory,
            Fault::Device(_) => Errno::BadMagic,
        };
        let channel = Channel {
            link,
            region: Mutex::new(()),
        };
        channel
            .exchange::<Empty>(GpuOp::Open, &ops::Open { grant })
            .map_err(refusal)?;
        let info = {
            let span = channel.whole_region().map_err(refusal)?;
            let _held = channel.region.lock();
            let ops::Written { len } = channel
                .exchange(GpuOp::Info, &ops::Spanned { span })
                .map_err(refusal)?;
            let len = usize::try_from(len).map_err(|_| Errno::BadMagic)?;
            if len > channel.link.region_len() {
                return Err(Errno::BadMagic);
            }
            let mut bytes = tairix_util::fallible::zeroed(len).ok_or(Errno::OutOfMemory)?;
            channel.link.read_region(0, &mut bytes);
            let mut reader = Reader::new(&bytes);
            let info = take_info(&mut reader).map_err(|_| Errno::BadMagic)?;
            reader.finish().map_err(|_| Errno::BadMagic)?;
            info
        };
        Ok(Self { channel, info })
    }
}

impl Channel {
    fn whole_region(&self) -> Result<RegionSpan, Fault> {
        let len =
            u32::try_from(self.link.region_len()).map_err(|_| Fault::Channel(Errno::BadMagic))?;
        Ok(RegionSpan { offset: 0, len })
    }

    /// Send `op` with `body`, reading its reply's body as `T`.
    fn exchange<T: Wire>(&self, op: GpuOp, body: &impl Wire) -> Result<T, Fault> {
        let reply = self.send(op, body)?;
        let (status, detail) = decode_reply(&reply, op).map_err(Fault::Channel)?;
        let mut reader = Reader::new(detail);
        match status {
            GpuStatus::Ok => {
                let answer = T::take(&mut reader).map_err(|_| Fault::Channel(Errno::BadMagic))?;
                reader
                    .finish()
                    .map_err(|_| Fault::Channel(Errno::BadMagic))?;
                Ok(answer)
            }
            GpuStatus::Refused(errno) => Err(Fault::Channel(errno)),
            GpuStatus::Device(code) => Err(Fault::Device(
                take_error(code, &mut reader).map_err(|_| Fault::Channel(Errno::BadMagic))?,
            )),
        }
    }

    fn send(&self, op: GpuOp, body: &impl Wire) -> Result<Vec<u8>, Fault> {
        let mut bytes = Vec::new();
        body.put(&mut Writer::new(&mut bytes))?;
        if bytes.len() > GPU_MAX_BODY {
            return Err(Fault::Device(Error::TooLarge));
        }
        let mut frame = [0u8; GPU_MAX_REQUEST];
        let len = GpuRequest { op, body: &bytes }
            .encode(&mut frame)
            .map_err(Fault::Channel)?;
        let mut reply = [0u8; GPU_MAX_REPLY];
        let answered = self
            .link
            .call(&frame[..len], &mut reply)
            .map_err(Fault::Channel)?;
        reply
            .get(..answered)
            .map(<[u8]>::to_vec)
            .ok_or(Fault::Channel(Errno::BadMagic))
    }

    fn call<T: Wire>(&self, op: GpuOp, body: &impl Wire) -> Result<T, Error> {
        self.exchange(op, body).map_err(Fault::error)
    }

    /// `bytes` written into the region, then `op` sent with the body `body`
    /// makes of their span, under one hold of the region.
    fn call_with<T: Wire, B: Wire>(
        &self,
        op: GpuOp,
        bytes: &[u8],
        body: impl FnOnce(RegionSpan) -> B,
    ) -> Result<T, Error> {
        let len = u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
        if bytes.len() > self.link.region_len() {
            return Err(Error::TooLarge);
        }
        let _held = self.region.lock();
        self.link.write_region(0, bytes);
        self.call(op, &body(RegionSpan { offset: 0, len }))
    }

    /// The bytes `write` encodes, written into the region for `op`.
    fn described<T: Wire, B: Wire>(
        &self,
        op: GpuOp,
        write: impl FnOnce(&mut Writer<'_>) -> Result<(), Error>,
        body: impl FnOnce(RegionSpan) -> B,
    ) -> Result<T, Error> {
        let mut bytes = Vec::new();
        write(&mut Writer::new(&mut bytes))?;
        self.call_with(op, &bytes, body)
    }

    /// Upload `commands`, a command buffer, in region-sized parts.
    fn upload(&self, context: ContextId, commands: &[Command]) -> Result<(), Error> {
        let room = self
            .link
            .region_len()
            .checked_sub(size_of::<u32>())
            .ok_or(Error::TooLarge)?;
        let mut chunk = Chunk::default();
        for command in commands {
            let mut one = Vec::new();
            put_command(&mut Writer::new(&mut one), command)?;
            if !chunk.fits(&one, room) {
                self.send_chunk(context, &mut chunk, false)?;
            }
            if chunk.fits(&one, room) {
                chunk.push(&one)?;
                continue;
            }
            // Only a pass outgrows an empty region: it goes as its first
            // draws, then the rest.
            let Command::Pass(pass) = command else {
                return Err(Error::TooLarge);
            };
            let draws = pass.draws();
            let mut sent = 0;
            while sent < draws.len() || sent == 0 {
                let taken = most_draws(room, draws.get(sent..).unwrap_or(&[]), |count| {
                    let mut part = Vec::new();
                    let writer = &mut Writer::new(&mut part);
                    if sent == 0 {
                        put_first_draws(writer, pass, count)?;
                    } else {
                        put_more_draws(writer, draws.get(sent..sent + count).unwrap_or(&[]))?;
                    }
                    Ok(part)
                })?;
                chunk.push(&taken.1)?;
                sent += taken.0;
                self.send_chunk(context, &mut chunk, false)?;
                if draws.is_empty() {
                    break;
                }
            }
        }
        self.send_chunk(context, &mut chunk, true)
    }

    fn send_chunk(&self, context: ContextId, chunk: &mut Chunk, last: bool) -> Result<(), Error> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve(size_of::<u32>() + chunk.bytes.len())
            .map_err(|_| Error::OutOfMemory)?;
        bytes.extend_from_slice(&chunk.count.to_le_bytes());
        bytes.extend_from_slice(&chunk.bytes);
        let sent = self.call_with::<Empty, _>(GpuOp::Upload, &bytes, |span| ops::Upload {
            context,
            last,
            span,
        });
        *chunk = Chunk::default();
        sent.map(|Empty| ())
    }
}

/// Commands being gathered into one upload.
#[derive(Default)]
struct Chunk {
    count: u32,
    bytes: Vec<u8>,
}

impl Chunk {
    fn fits(&self, one: &[u8], room: usize) -> bool {
        self.bytes.len().saturating_add(one.len()) <= room
    }

    fn push(&mut self, one: &[u8]) -> Result<(), Error> {
        self.bytes
            .try_reserve(one.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.bytes.extend_from_slice(one);
        self.count = self.count.checked_add(1).ok_or(Error::TooMany)?;
        Ok(())
    }
}

/// The most of `draws` whose part, as `encode` makes it, fits `room`, and
/// that part; at least one draw where there are any.
fn most_draws<T>(
    room: usize,
    draws: &[T],
    encode: impl Fn(usize) -> Result<Vec<u8>, Error>,
) -> Result<(usize, Vec<u8>), Error> {
    let (mut fits, mut over) = (0, draws.len() + 1);
    let mut best = encode(0)?;
    if best.len() > room {
        return Err(Error::TooLarge);
    }
    while fits + 1 < over {
        let middle = fits + (over - fits) / 2;
        let part = encode(middle)?;
        if part.len() <= room {
            fits = middle;
            best = part;
        } else {
            over = middle;
        }
    }
    if fits == 0 && !draws.is_empty() {
        return Err(Error::TooLarge);
    }
    Ok((fits, best))
}

/// A body of nothing.
pub(crate) struct Empty;

impl Wire for Empty {
    fn put(&self, _: &mut Writer<'_>) -> Result<(), Error> {
        Ok(())
    }

    fn take(_: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self)
    }
}

fn id(context: ContextKey) -> ContextId {
    context.slot().to_bits()
}

impl Host for Remote {
    #[cfg(test)]
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn create_context(&self, memory_limit: Option<u64>) -> Result<ContextKey, Error> {
        let ops::OfContext { context } = self
            .channel
            .call(GpuOp::CreateContext, &ops::CreateContext { memory_limit })?;
        tairix_collections::SlotKey::from_bits(context)
            .map(ContextKey::from_slot)
            .ok_or(UNREACHABLE)
    }

    fn destroy_context(&self, context: ContextKey) {
        let _ = self.channel.call::<Empty>(
            GpuOp::DestroyContext,
            &ops::OfContext {
                context: id(context),
            },
        );
    }

    fn status(&self, context: ContextKey) -> Result<(), Error> {
        self.channel
            .call::<Empty>(
                GpuOp::Status,
                &ops::OfContext {
                    context: id(context),
                },
            )
            .map(|Empty| ())
    }

    fn memory_used(&self, context: ContextKey) -> MemoryUse {
        self.channel
            .call::<ops::Used>(
                GpuOp::MemoryUsed,
                &ops::OfContext {
                    context: id(context),
                },
            )
            .map(|used| used.used)
            .unwrap_or_default()
    }

    fn release(&self, context: ContextKey, object: Object) {
        let _ = self.channel.call::<Empty>(
            GpuOp::Release,
            &ops::Release {
                context: id(context),
                object,
            },
        );
    }

    fn allocate(&self, context: ContextKey, desc: &AllocationDesc) -> Result<AllocationKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::Allocate,
                &ops::Allocate {
                    context: id(context),
                    desc: *desc,
                },
            )?
            .key()
    }

    fn write(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        let part = self.channel.link.region_len();
        let mut at = offset;
        for piece in data.chunks(part.max(1)) {
            self.channel
                .call_with::<Empty, _>(GpuOp::Write, piece, |span| ops::HostAccess {
                    context: id(context),
                    allocation,
                    offset: at,
                    span,
                })?;
            at = at
                .checked_add(piece.len() as u64)
                .ok_or(Error::OutOfRange)?;
        }
        Ok(())
    }

    fn read(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        out: &mut [u8],
    ) -> Result<(), Error> {
        let part = self.channel.link.region_len();
        let mut at = offset;
        for piece in out.chunks_mut(part.max(1)) {
            let len = u32::try_from(piece.len()).map_err(|_| Error::TooLarge)?;
            let _held = self.channel.region.lock();
            self.channel.call::<Empty>(
                GpuOp::Read,
                &ops::HostAccess {
                    context: id(context),
                    allocation,
                    offset: at,
                    span: RegionSpan { offset: 0, len },
                },
            )?;
            self.channel.link.read_region(0, piece);
            at = at
                .checked_add(piece.len() as u64)
                .ok_or(Error::OutOfRange)?;
        }
        Ok(())
    }

    fn export(&self, context: ContextKey, allocation: AllocationKey) -> Result<ExportId, Error> {
        let ops::Token { token } = self.channel.call(
            GpuOp::Export,
            &ops::Export {
                context: id(context),
                allocation,
            },
        )?;
        Ok(ExportId::Token(token))
    }

    fn import(&self, context: ContextKey, export: ExportId) -> Result<Imported, Error> {
        let ExportId::Token(token) = export else {
            return Err(Error::Foreign);
        };
        let ops::Imported { allocation, desc } = self.channel.call(
            GpuOp::Import,
            &ops::Import {
                context: id(context),
                token,
            },
        )?;
        Ok(Imported { allocation, desc })
    }

    fn drop_export(&self, export: ExportId) {
        if let ExportId::Token(token) = export {
            let _ = self
                .channel
                .call::<Empty>(GpuOp::DropExport, &ops::Token { token });
        }
    }

    fn create_buffer(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        desc: &BufferDesc,
    ) -> Result<BufferKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateBuffer,
                &ops::CreateBuffer {
                    context: id(context),
                    allocation,
                    desc: *desc,
                },
            )?
            .key()
    }

    fn create_timeline(&self, context: ContextKey, initial: u64) -> Result<TimelineKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateTimeline,
                &ops::CreateTimeline {
                    context: id(context),
                    initial,
                },
            )?
            .key()
    }

    fn timeline_value(&self, context: ContextKey, timeline: TimelineKey) -> Result<u64, Error> {
        let ops::Value { value } = self.channel.call(
            GpuOp::TimelineValue,
            &ops::OfTimeline {
                context: id(context),
                timeline,
            },
        )?;
        Ok(value)
    }

    fn signal(&self, context: ContextKey, timeline: TimelineKey, value: u64) -> Result<(), Error> {
        self.channel
            .call::<Empty>(
                GpuOp::Signal,
                &ops::AtValue {
                    context: id(context),
                    timeline,
                    value,
                },
            )
            .map(|Empty| ())
    }

    fn wait(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64,
    ) -> Result<(), Error> {
        self.channel
            .call::<Empty>(
                GpuOp::Wait,
                &ops::Wait {
                    context: id(context),
                    timeline,
                    value,
                    timeout,
                },
            )
            .map(|Empty| ())
    }

    fn reached(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<bool, Error> {
        let ops::Truth { truth } = self.channel.call(
            GpuOp::Reached,
            &ops::AtValue {
                context: id(context),
                timeline,
                value,
            },
        )?;
        Ok(truth)
    }

    fn submit(
        &self,
        context: ContextKey,
        request: &Request<'_>,
    ) -> Result<(TimelineKey, u64), SubmitError> {
        let context = id(context);
        for buffer in request.buffers {
            if let Err(error) = self.channel.upload(context, buffer) {
                let _ = self
                    .channel
                    .call::<Empty>(GpuOp::Discard, &ops::OfContext { context });
                return Err(error.into());
            }
        }
        let mut points = Vec::new();
        let writer = &mut Writer::new(&mut points);
        let put = |writer: &mut Writer<'_>, &(timeline, value): &(TimelineKey, u64)| {
            timeline.put(writer)?;
            value.put(writer)
        };
        writer.list(request.waits, put)?;
        writer.list(request.signals, put)?;
        let queue = u32::try_from(request.queue).map_err(|_| Error::NoSuchQueue)?;
        let deadline = Duration64::from_nanos(request.span);
        let len = u32::try_from(points.len()).map_err(|_| Error::TooLarge)?;
        let _held = self.channel.region.lock();
        self.channel.link.write_region(0, &points);
        let body = ops::Submit {
            context,
            queue,
            deadline,
            span: RegionSpan { offset: 0, len },
        };
        let reply = self
            .channel
            .send(GpuOp::Submit, &body)
            .map_err(Fault::error)?;
        let (status, detail) = decode_reply(&reply, GpuOp::Submit).map_err(|_| UNREACHABLE)?;
        let mut reader = Reader::new(detail);
        match status {
            GpuStatus::Ok => {
                let fence = ops::Fence::take(&mut reader).map_err(|_| UNREACHABLE)?;
                Ok((fence.timeline, fence.value))
            }
            GpuStatus::Refused(errno) => Err(Fault::Channel(errno).error().into()),
            GpuStatus::Device(code) => {
                Err(take_submit_error(code, &mut reader).map_err(|_| UNREACHABLE)?)
            }
        }
    }

    fn record_update(&self, _: ContextKey, bytes: Vec<u8>) -> Result<Arc<Recorded>, Error> {
        // The bytes are this process's own until the submission carries them;
        // the device charges them then.
        crate::shared(Recorded::new(bytes, Charge::take(&[], 0, Held::Host)?))
    }

    fn create_bind_set_layout(
        &self,
        context: ContextKey,
        bindings: &[LayoutBinding],
    ) -> Result<BindSetLayoutKey, Error> {
        self.channel
            .described::<ops::Made, _>(
                GpuOp::CreateBindSetLayout,
                |writer| writer.list(bindings, |writer, binding| binding.put(writer)),
                |span| ops::Described {
                    context: id(context),
                    span,
                },
            )?
            .key()
    }

    fn create_bind_set(
        &self,
        context: ContextKey,
        request: &SetRequest<'_>,
    ) -> Result<BindSetKey, Error> {
        self.channel
            .described::<ops::Made, _>(
                GpuOp::CreateBindSet,
                |writer| {
                    writer.list(request.entries, |writer, &(binding, count)| {
                        binding.put(writer)?;
                        u32::try_from(count)
                            .map_err(|_| Error::TooMany)?
                            .put(writer)
                    })?;
                    writer.list(request.resources, |writer, resource| resource.put(writer))
                },
                |span| ops::CreateBindSet {
                    context: id(context),
                    layout: request.layout,
                    span,
                },
            )?
            .key()
    }

    fn create_pipeline_layout(
        &self,
        context: ContextKey,
        sets: &[BindSetLayoutKey],
        push_constants: PushConstants,
    ) -> Result<PipelineLayoutKey, Error> {
        self.channel
            .described::<ops::Made, _>(
                GpuOp::CreatePipelineLayout,
                |writer| writer.list(sets, |writer, set| set.put(writer)),
                |span| ops::CreatePipelineLayout {
                    context: id(context),
                    push_constants,
                    span,
                },
            )?
            .key()
    }

    fn create_shader_module(
        &self,
        context: ContextKey,
        module: &Arc<Admitted>,
    ) -> Result<ShaderModuleKey, Error> {
        self.channel
            .call_with::<ops::Made, _>(
                GpuOp::CreateShaderModule,
                module.module().bytes(),
                |span| ops::Described {
                    context: id(context),
                    span,
                },
            )?
            .key()
    }

    fn image_requirements(&self, desc: &ImageDesc) -> Result<MemoryRequirements, Error> {
        let ops::Requirements { requirements } = self.channel.call(
            GpuOp::ImageRequirements,
            &ops::ImageRequirements { desc: *desc },
        )?;
        Ok(requirements)
    }

    fn create_image(
        &self,
        context: ContextKey,
        (allocation, offset): (AllocationKey, u64),
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateImage,
                &ops::CreateImage {
                    context: id(context),
                    allocation,
                    offset,
                    desc: *desc,
                },
            )?
            .key()
    }

    fn create_view(
        &self,
        context: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<ViewKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateView,
                &ops::CreateView {
                    context: id(context),
                    image,
                    desc: *desc,
                },
            )?
            .key()
    }

    fn create_buffer_view(
        &self,
        context: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<BufferViewKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateBufferView,
                &ops::CreateBufferView {
                    context: id(context),
                    buffer,
                    desc: *desc,
                },
            )?
            .key()
    }

    fn create_sampler(&self, context: ContextKey, desc: &SamplerDesc) -> Result<SamplerKey, Error> {
        self.channel
            .call::<ops::Made>(
                GpuOp::CreateSampler,
                &ops::CreateSampler {
                    context: id(context),
                    desc: *desc,
                },
            )?
            .key()
    }

    fn create_compute_pipeline(
        &self,
        context: ContextKey,
        (module, entry, specialization): (ShaderModuleKey, &str, &[SpecValue]),
        layout: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey, Error> {
        self.channel
            .described::<ops::Made, _>(
                GpuOp::CreateComputePipeline,
                |writer| {
                    put_name(writer, entry)?;
                    writer.list(specialization, |writer, value| value.put(writer))
                },
                |span| ops::CreateComputePipeline {
                    context: id(context),
                    module,
                    layout,
                    span,
                },
            )?
            .key()
    }

    fn create_render_pipeline(
        &self,
        context: ContextKey,
        request: &RenderRequest<'_>,
    ) -> Result<RenderPipelineKey, Error> {
        self.channel
            .described::<ops::Made, _>(
                GpuOp::CreateRenderPipeline,
                |writer| put_render(writer, request),
                |span| ops::Described {
                    context: id(context),
                    span,
                },
            )?
            .key()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.channel.call::<Empty>(GpuOp::Close, &Empty);
    }
}

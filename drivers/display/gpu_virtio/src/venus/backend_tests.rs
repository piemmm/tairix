//! The Venus backend held to a host of the test's own: a display end that
//! carries out the desk's orders on a device it keeps, and a renderer that
//! decodes every stream the backend sends, keeps the objects it names, runs
//! its submissions on its memory, and writes its replies — failing a context
//! fatally, as the real renderer does, on any command off its grammar. The
//! backend is driven only through the GPU seam.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar as StdCondvar, Mutex as StdMutex};
use std::thread::JoinHandle;

use tairix_abi::driver::RegionCaching;
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_collections::SlotMap;
use tairix_gpu::{
    AllocationDesc, BufferDesc, BufferUsage, ContextDesc, CopyRegion, Device, Error,
    MemoryProperties, QueueCapabilities, Spawn, Submission, MAX_UPDATE_BYTES,
};
use tairix_rt::sync::Mutex;
use tairix_virtio::{MockRegion, Part, SharedMemory};

use super::{device, Admits, ContextState, Then};
use crate::three_d::{Blob, Context};
use crate::venus::desk::{Answer, Desk, Order, Rouse};
use crate::venus::testing::{limits, properties_reply, Grammar, Still};
use crate::venus::{Command, Structure, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES};

const REGION_BYTES: usize = 16 << 20;
const SECOND: Duration64 = Duration64::from_secs(1);

/// What the renderer reads a stream with: `None` off the grammar.
struct Words<'a>(&'a [u8]);

impl Words<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(u32::cast_signed)
    }

    fn u64(&mut self) -> Option<u64> {
        let bytes = self.take(8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    /// A word that must be `want`.
    fn is(&mut self, want: u64) -> Option<()> {
        (self.u64()? == want).then_some(())
    }

    fn structure(&mut self, structure: Structure) -> Option<()> {
        (self.i32()? == structure as i32).then_some(())?;
        self.is(0)
    }
}

/// A command a command buffer recorded.
enum Recorded {
    Copy(u64, u64, Vec<(u64, u64, u64)>),
    Fill(u64, u64, u64, u32),
    Update(u64, u64, Vec<u8>),
}

enum Object {
    Instance,
    Physical,
    Device,
    Queue,
    Pool,
    Commands(Vec<Recorded>),
    Buffer { size: u64, memory: Option<u64> },
    Memory { bytes: Vec<u8> },
}

struct HostContext {
    reply: Option<(u32, u64, u64)>,
    reply_at: u64,
    objects: BTreeMap<u64, Object>,
    dead: bool,
}

struct HostBlob {
    context: Context,
    blob_id: u64,
    size: u64,
    /// Where it is mapped in the region.
    at: Option<u64>,
    /// Its bytes while unmapped, a context's own memory's held by the memory.
    own: Vec<u8>,
}

/// The ways the host is made to misbehave.
#[derive(Copy, Clone, Default, Eq, PartialEq)]
enum Misbehaviour {
    #[default]
    Nothing,
    /// Submissions refused.
    RefuseSubmits,
    /// Submissions answered without being run, as a render server answers
    /// the fences of a worker that died.
    IgnoreWork,
    /// Unmaps refused once the driver's window is gone, the blob left mapped.
    RefuseUnmaps,
    /// Maps carried out but answered with what cannot be read, as an answer
    /// without its fence is: the blob may be mapped, and its part is kept.
    DoubtMaps,
}

#[derive(Default)]
struct HostState {
    contexts: SlotMap<Context, HostContext>,
    blobs: SlotMap<Blob, HostBlob>,
    /// How the host departs from a well-behaved one, if it does.
    misbehaves: Misbehaviour,
    unmaps_refused: usize,
    /// Memory freed while a blob still made it host memory of the device.
    freed_exported: bool,
}

/// The host: its device, its renderer, and the region its blobs map into.
struct Host {
    region: Arc<MockRegion>,
    shared: Arc<Mutex<SharedMemory>>,
    state: StdMutex<HostState>,
    /// Told each time an order has been carried out.
    changed: StdCondvar,
}

impl Host {
    fn carry_out(&self, order: Order) -> Answer {
        let mut state = self.state.lock().expect("unpoisoned");
        match order {
            Order::MakeContext => {
                let context = HostContext {
                    reply: None,
                    reply_at: 0,
                    objects: BTreeMap::new(),
                    dead: false,
                };
                Answer::Context(state.contexts.try_insert(context).expect("room"))
            }
            Order::EndContext(context) => match state.contexts.remove(context) {
                Some(_) => Answer::Done,
                None => Answer::Refused(tairix_abi::DriverError::NotFound, None),
            },
            Order::MakeBlob(context, spec) => {
                let fits = spec.blob_id == 0
                    || state.contexts.get(context).is_some_and(|host| {
                        matches!(host.objects.get(&spec.blob_id),
                            Some(Object::Memory { bytes }) if bytes.len() as u64 == spec.size)
                    });
                if !fits {
                    return Answer::Refused(tairix_abi::DriverError::OutOfRange, None);
                }
                let blob = HostBlob {
                    context,
                    blob_id: spec.blob_id,
                    size: spec.size,
                    at: None,
                    own: vec![0; usize::try_from(spec.size).expect("small")],
                };
                Answer::Blob(state.blobs.try_insert(blob).expect("room"))
            }
            Order::EndBlob(blob) => match state.blobs.get(blob) {
                Some(held) if held.at.is_none() => {
                    state.blobs.remove(blob);
                    Answer::Done
                }
                _ => Answer::Refused(tairix_abi::DriverError::Busy, None),
            },
            Order::MapBlob(blob, part) => self.map(&mut state, blob, part),
            Order::UnmapBlob(blob, part) => self.unmap(&mut state, blob, part),
            Order::Submit {
                context, payload, ..
            } => {
                if state.misbehaves == Misbehaviour::RefuseSubmits {
                    return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
                }
                if state.misbehaves != Misbehaviour::IgnoreWork {
                    self.run_payload(&mut state, context, &payload);
                }
                Answer::Done
            }
        }
    }

    /// Map `blob` at `part`, as the display end and the device together do.
    fn map(&self, state: &mut HostState, blob: Blob, mut part: Part) -> Answer {
        let at = part.offset();
        let Some(bytes) = Self::blob_bytes(state, blob) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        self.region
            .write(usize::try_from(at).expect("small"), &bytes)
            .expect("in the region");
        if let Some(held) = state.blobs.get_mut(blob) {
            held.at = Some(at);
        }
        if state.misbehaves == Misbehaviour::DoubtMaps {
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
        }
        let _ = self
            .shared
            .lock()
            .map(&mut part, self.region.as_ref(), RegionCaching::WriteBack);
        Answer::Mapped(part)
    }

    /// Unmap `blob` from `part`, the driver's window first.
    fn unmap(&self, state: &mut HostState, blob: Blob, mut part: Part) -> Answer {
        if self
            .shared
            .lock()
            .unmap(&mut part, self.region.as_ref())
            .is_err()
        {
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, Some(part));
        }
        if state.misbehaves == Misbehaviour::RefuseUnmaps {
            // Still mapped, so the part is not lent back.
            state.unmaps_refused += 1;
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
        }
        let Some(held) = state.blobs.get(blob) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        let (Some(at), size) = (held.at, held.size) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        let mut bytes = vec![0; usize::try_from(size).expect("small")];
        self.region
            .read(usize::try_from(at).expect("small"), &mut bytes)
            .expect("in the region");
        Self::put_blob_bytes(state, blob, bytes);
        if let Some(held) = state.blobs.get_mut(blob) {
            held.at = None;
        }
        Answer::Unmapped(part)
    }

    /// A blob's bytes, wherever they are held.
    fn blob_bytes(state: &HostState, blob: Blob) -> Option<Vec<u8>> {
        let held = state.blobs.get(blob)?;
        if held.blob_id == 0 {
            return Some(held.own.clone());
        }
        match state
            .contexts
            .get(held.context)?
            .objects
            .get(&held.blob_id)?
        {
            Object::Memory { bytes } => Some(bytes.clone()),
            _ => None,
        }
    }

    fn put_blob_bytes(state: &mut HostState, blob: Blob, bytes: Vec<u8>) {
        let Some(held) = state.blobs.get_mut(blob) else {
            return;
        };
        if held.blob_id == 0 {
            held.own = bytes;
            return;
        }
        let (context, id) = (held.context, held.blob_id);
        if let Some(Object::Memory { bytes: held }) = state
            .contexts
            .get_mut(context)
            .and_then(|host| host.objects.get_mut(&id))
        {
            *held = bytes;
        }
    }

    /// Where memory `id` of `context` lies in the region, where its blob is
    /// mapped.
    fn mapped_at(state: &HostState, context: Context, id: u64) -> Option<u64> {
        state
            .blobs
            .iter()
            .find(|(_, held)| held.context == context && held.blob_id == id)
            .and_then(|(_, held)| held.at)
    }

    fn read_memory(&self, state: &HostState, context: Context, id: u64, at: u64, out: &mut [u8]) {
        if let Some(base) = Self::mapped_at(state, context, id) {
            self.region
                .read(usize::try_from(base + at).expect("small"), out)
                .expect("in the region");
        } else if let Some(Object::Memory { bytes }) = state
            .contexts
            .get(context)
            .and_then(|host| host.objects.get(&id))
        {
            let start = usize::try_from(at).expect("small");
            out.copy_from_slice(&bytes[start..start + out.len()]);
        }
    }

    fn write_memory(&self, state: &mut HostState, context: Context, id: u64, at: u64, data: &[u8]) {
        if let Some(base) = Self::mapped_at(state, context, id) {
            self.region
                .write(usize::try_from(base + at).expect("small"), data)
                .expect("in the region");
        } else if let Some(Object::Memory { bytes }) = state
            .contexts
            .get_mut(context)
            .and_then(|host| host.objects.get_mut(&id))
        {
            let start = usize::try_from(at).expect("small");
            bytes[start..start + data.len()].copy_from_slice(data);
        }
    }

    /// The commands an order carried: the reply stream set and sought, and
    /// streams run where they lie.
    fn run_payload(&self, state: &mut HostState, context: Context, payload: &[u8]) {
        let mut words = Words(payload);
        while !words.0.is_empty() {
            let ran = (|| {
                let command = words.i32()?;
                words.u32()?;
                match command {
                    c if c == Command::SetReplyCommandStreamMesa as i32 => {
                        words.is(1)?;
                        let span = (words.u32()?, words.u64()?, words.u64()?);
                        let host = state.contexts.get_mut(context)?;
                        host.reply = Some(span);
                        host.reply_at = 0;
                    }
                    c if c == Command::SeekReplyCommandStreamMesa as i32 => {
                        let position = words.u64()?;
                        state.contexts.get_mut(context)?.reply_at = position;
                    }
                    c if c == Command::ExecuteCommandStreamsMesa as i32 => {
                        let count = words.u32()?;
                        words.is(u64::from(count))?;
                        let mut spans = Vec::new();
                        for _ in 0..count {
                            spans.push((words.u32()?, words.u64()?, words.u64()?));
                        }
                        words.is(0)?;
                        (words.u32()? == 0).then_some(())?;
                        words.is(0)?;
                        (words.u32()? == 0).then_some(())?;
                        for (resource, offset, size) in spans {
                            let stream = self.resource_bytes(state, resource, offset, size)?;
                            self.run_stream(state, context, &stream);
                        }
                    }
                    _ => return None,
                }
                Some(())
            })();
            if ran.is_none() {
                if let Some(host) = state.contexts.get_mut(context) {
                    host.dead = true;
                }
                return;
            }
        }
    }

    fn resource_bytes(
        &self,
        state: &HostState,
        resource: u32,
        offset: u64,
        size: u64,
    ) -> Option<Vec<u8>> {
        let (_, held) = state
            .blobs
            .iter()
            .find(|(blob, _)| blob.resource_id() == Some(resource))?;
        let at = held.at?;
        let mut bytes = vec![0; usize::try_from(size).ok()?];
        self.region
            .read(usize::try_from(at + offset).ok()?, &mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Write `reply` at the context's reply position.
    fn reply(&self, state: &mut HostState, context: Context, reply: &[u8]) -> Option<()> {
        let host = state.contexts.get_mut(context)?;
        let (resource, base, size) = host.reply?;
        let at = host.reply_at;
        if at + reply.len() as u64 > size {
            return None;
        }
        host.reply_at += reply.len() as u64;
        let (_, held) = state
            .blobs
            .iter()
            .find(|(blob, _)| blob.resource_id() == Some(resource))?;
        let mapped = held.at?;
        self.region
            .write(usize::try_from(mapped + base + at).ok()?, reply)
            .ok()
    }

    fn run_stream(&self, state: &mut HostState, context: Context, stream: &[u8]) {
        let mut words = Words(stream);
        while !words.0.is_empty() {
            if state.contexts.get(context).is_none_or(|host| host.dead) {
                return;
            }
            if self.run_command(state, context, &mut words).is_none() {
                if let Some(host) = state.contexts.get_mut(context) {
                    host.dead = true;
                }
                return;
            }
        }
    }

    fn insert(state: &mut HostState, context: Context, id: u64, object: Object) -> Option<()> {
        let host = state.contexts.get_mut(context)?;
        (id != 0 && !host.objects.contains_key(&id)).then_some(())?;
        host.objects.insert(id, object);
        Some(())
    }

    fn has(state: &HostState, context: Context, id: u64) -> Option<()> {
        state
            .contexts
            .get(context)?
            .objects
            .contains_key(&id)
            .then_some(())
    }

    fn made(command: Command, id: u64) -> Vec<u8> {
        Grammar::default()
            .i32(command as i32)
            .i32(0)
            .u64(1)
            .u64(id)
            .0
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command the backend sends"
    )]
    fn run_command(
        &self,
        state: &mut HostState,
        context: Context,
        words: &mut Words<'_>,
    ) -> Option<()> {
        let command = words.i32()?;
        let reply = words.u32()? & 1 == 1;
        let answer: Option<Vec<u8>> = match command {
            c if c == Command::CreateInstance as i32 => {
                words.is(1)?;
                words.structure(Structure::InstanceCreateInfo)?;
                words.u32()?;
                words.is(1)?;
                words.structure(Structure::ApplicationInfo)?;
                words.is(0)?;
                words.u32()?;
                words.is(0)?;
                words.u32()?;
                words.u32()?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Instance)?;
                Some(Self::made(Command::CreateInstance, id))
            }
            c if c == Command::EnumeratePhysicalDevices as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                let count = words.u32()?;
                let named = words.u64()?;
                (named == u64::from(count)).then_some(())?;
                let mut reply = Grammar::default()
                    .i32(Command::EnumeratePhysicalDevices as i32)
                    .i32(0)
                    .u64(1)
                    .u32(1);
                if named == 0 {
                    reply = reply.u64(0);
                } else {
                    let id = words.u64()?;
                    for _ in 1..named {
                        words.u64()?;
                    }
                    Self::insert(state, context, id, Object::Physical)?;
                    reply = reply.u64(1).u64(id);
                }
                Some(reply.0)
            }
            c if c == Command::GetPhysicalDeviceProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::PhysicalDeviceProperties2 as i32).then_some(())?;
                words.is(1)?;
                (words.i32()? == Structure::PhysicalDeviceSubgroupProperties as i32)
                    .then_some(())?;
                words.is(1)?;
                words.structure(Structure::PhysicalDeviceMaintenance3Properties)?;
                Some(properties_reply(limits).0)
            }
            c if c == Command::GetPhysicalDeviceQueueFamilyProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                let capacity = words.u32()?;
                words.is(u64::from(capacity))?;
                for _ in 0..capacity {
                    words.structure(Structure::QueueFamilyProperties2)?;
                }
                let read = u64::from(capacity.min(1));
                let mut reply = Grammar::default().i32(command).u64(1).u32(1).u64(read);
                for _ in 0..read {
                    reply = reply
                        .structure(Structure::QueueFamilyProperties2)
                        .u32(7)
                        .u32(2)
                        .words(4);
                }
                Some(reply.0)
            }
            c if c == Command::GetPhysicalDeviceMemoryProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::PhysicalDeviceMemoryProperties2)?;
                words.is(MAX_MEMORY_TYPES as u64)?;
                words.is(MAX_MEMORY_HEAPS as u64)?;
                let mut reply = Grammar::default()
                    .i32(command)
                    .u64(1)
                    .structure(Structure::PhysicalDeviceMemoryProperties2)
                    .u32(2)
                    .u64(MAX_MEMORY_TYPES as u64);
                for index in 0..MAX_MEMORY_TYPES {
                    reply = match index {
                        0 => reply.u32(1).u32(0),
                        1 => reply.u32(0xe).u32(0),
                        _ => reply.u32(0).u32(0),
                    };
                }
                reply = reply.u32(1).u64(MAX_MEMORY_HEAPS as u64);
                for index in 0..MAX_MEMORY_HEAPS {
                    reply = if index == 0 {
                        reply.u64(1 << 30).u32(1)
                    } else {
                        reply.u64(0).u32(0)
                    };
                }
                Some(reply.0)
            }
            c if c == Command::CreateDevice as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::DeviceCreateInfo)?;
                words.u32()?;
                (words.u32()? == 1).then_some(())?;
                words.is(1)?;
                words.structure(Structure::DeviceQueueCreateInfo)?;
                words.u32()?;
                words.u32()?;
                let queues = words.u32()?;
                words.is(u64::from(queues))?;
                for _ in 0..queues {
                    words.u32()?;
                }
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Device)?;
                Some(Self::made(Command::CreateDevice, id))
            }
            c if c == Command::GetDeviceQueue2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::DeviceQueueInfo2 as i32).then_some(())?;
                words.is(1)?;
                words.structure(Structure::DeviceQueueTimelineInfoMesa)?;
                let ring = words.u32()?;
                (1..=63).contains(&ring).then_some(())?;
                words.u32()?;
                words.u32()?;
                words.u32()?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Queue)?;
                None
            }
            c if c == Command::CreateCommandPool as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::CommandPoolCreateInfo)?;
                words.u32()?;
                words.u32()?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Pool)?;
                Some(Self::made(Command::CreateCommandPool, id))
            }
            c if c == Command::AllocateCommandBuffers as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::CommandBufferAllocateInfo)?;
                Self::has(state, context, words.u64()?)?;
                (words.i32()? == 0).then_some(())?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut reply = Grammar::default().i32(command).i32(0).u64(u64::from(count));
                for _ in 0..count {
                    let id = words.u64()?;
                    Self::insert(state, context, id, Object::Commands(Vec::new()))?;
                    reply = reply.u64(id);
                }
                Some(reply.0)
            }
            c if c == Command::CreateBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::BufferCreateInfo)?;
                words.u32()?;
                let size = words.u64()?;
                words.u32()?;
                words.i32()?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Buffer { size, memory: None })?;
                Some(Self::made(Command::CreateBuffer, id))
            }
            c if c == Command::GetBufferMemoryRequirements2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::BufferMemoryRequirementsInfo2)?;
                let id = words.u64()?;
                words.is(1)?;
                words.structure(Structure::MemoryRequirements2)?;
                let Some(Object::Buffer { size, .. }) =
                    state.contexts.get(context)?.objects.get(&id)
                else {
                    return None;
                };
                Some(
                    Grammar::default()
                        .i32(command)
                        .u64(1)
                        .structure(Structure::MemoryRequirements2)
                        .u64(size.next_multiple_of(256))
                        .u64(256)
                        .u32(0b11)
                        .0,
                )
            }
            c if c == Command::AllocateMemory as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::MemoryAllocateInfo)?;
                let size = words.u64()?;
                (words.u32()? < 2).then_some(())?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                // What the host held before: the backend zeroes it.
                let bytes = vec![0xa5; usize::try_from(size).ok()?];
                Self::insert(state, context, id, Object::Memory { bytes })?;
                Some(Self::made(Command::AllocateMemory, id))
            }
            c if c == Command::BindBufferMemory2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                (words.u32()? == 1).then_some(())?;
                words.is(1)?;
                words.structure(Structure::BindBufferMemoryInfo)?;
                let (buffer, memory, offset) = (words.u64()?, words.u64()?, words.u64()?);
                (offset == 0).then_some(())?;
                Self::has(state, context, memory)?;
                let Some(Object::Buffer { memory: bound, .. }) =
                    state.contexts.get_mut(context)?.objects.get_mut(&buffer)
                else {
                    return None;
                };
                *bound = Some(memory);
                Some(Grammar::default().i32(command).i32(0).0)
            }
            c if c == Command::BeginCommandBuffer as i32 => {
                let id = words.u64()?;
                words.is(1)?;
                words.structure(Structure::CommandBufferBeginInfo)?;
                words.u32()?;
                words.is(0)?;
                let Some(Object::Commands(recorded)) =
                    state.contexts.get_mut(context)?.objects.get_mut(&id)
                else {
                    return None;
                };
                recorded.clear();
                None
            }
            c if c == Command::EndCommandBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                None
            }
            c if c == Command::CmdCopyBuffer as i32 => {
                let id = words.u64()?;
                let (src, dst) = (words.u64()?, words.u64()?);
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut regions = Vec::new();
                for _ in 0..count {
                    regions.push((words.u64()?, words.u64()?, words.u64()?));
                }
                Self::record(state, context, id, Recorded::Copy(src, dst, regions))?;
                None
            }
            c if c == Command::CmdFillBuffer as i32 => {
                let id = words.u64()?;
                let fill = Recorded::Fill(words.u64()?, words.u64()?, words.u64()?, words.u32()?);
                Self::record(state, context, id, fill)?;
                None
            }
            c if c == Command::CmdUpdateBuffer as i32 => {
                let id = words.u64()?;
                let (dst, offset, size) = (words.u64()?, words.u64()?, words.u64()?);
                words.is(size)?;
                let data = words.take(usize::try_from(size).ok()?)?.to_vec();
                Self::record(state, context, id, Recorded::Update(dst, offset, data))?;
                None
            }
            c if c == Command::CmdPipelineBarrier as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.u32()?;
                words.u32()?;
                words.u32()?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                for _ in 0..count {
                    words.structure(Structure::MemoryBarrier)?;
                    words.u32()?;
                    words.u32()?;
                }
                for _ in 0..2 {
                    (words.u32()? == 0).then_some(())?;
                    words.is(0)?;
                }
                None
            }
            c if c == Command::QueueSubmit as i32 => {
                Self::has(state, context, words.u64()?)?;
                (words.u32()? == 1).then_some(())?;
                words.is(1)?;
                words.structure(Structure::SubmitInfo)?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut buffers = Vec::new();
                for _ in 0..count {
                    buffers.push(words.u64()?);
                }
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                for id in buffers {
                    self.execute(state, context, id)?;
                }
                None
            }
            c if c == Command::DestroyBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                state.contexts.get_mut(context)?.objects.remove(&id)?;
                None
            }
            c if c == Command::FreeMemory as i32 => {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                if Self::mapped_at(state, context, id).is_some()
                    || state
                        .blobs
                        .iter()
                        .any(|(_, held)| held.context == context && held.blob_id == id)
                {
                    state.freed_exported = true;
                }
                state.contexts.get_mut(context)?.objects.remove(&id)?;
                None
            }
            _ => return None,
        };
        match (reply, answer) {
            (true, Some(answer)) => self.reply(state, context, &answer),
            (false, _) => Some(()),
            // A reply asked of a command that writes none.
            (true, None) => None,
        }
    }

    fn record(state: &mut HostState, context: Context, id: u64, command: Recorded) -> Option<()> {
        let Some(Object::Commands(recorded)) =
            state.contexts.get_mut(context)?.objects.get_mut(&id)
        else {
            return None;
        };
        recorded.push(command);
        Some(())
    }

    /// The memory a buffer is bound to, and its size.
    fn bound(state: &HostState, context: Context, buffer: u64) -> Option<(u64, u64)> {
        match state.contexts.get(context)?.objects.get(&buffer)? {
            Object::Buffer { size, memory } => Some(((*memory)?, *size)),
            _ => None,
        }
    }

    fn execute(&self, state: &mut HostState, context: Context, id: u64) -> Option<()> {
        let Some(Object::Commands(recorded)) = state.contexts.get_mut(context)?.objects.remove(&id)
        else {
            return None;
        };
        for command in &recorded {
            match command {
                Recorded::Copy(src, dst, regions) => {
                    let (from, _) = Self::bound(state, context, *src)?;
                    let (to, _) = Self::bound(state, context, *dst)?;
                    for &(at, into, len) in regions {
                        let mut bytes = vec![0; usize::try_from(len).ok()?];
                        self.read_memory(state, context, from, at, &mut bytes);
                        self.write_memory(state, context, to, into, &bytes);
                    }
                }
                Recorded::Fill(dst, offset, size, word) => {
                    let (to, buffer) = Self::bound(state, context, *dst)?;
                    let size = if *size == u64::MAX {
                        (buffer - offset) & !3
                    } else {
                        *size
                    };
                    let bytes: Vec<u8> = (0..size / 4).flat_map(|_| word.to_le_bytes()).collect();
                    self.write_memory(state, context, to, *offset, &bytes);
                }
                Recorded::Update(dst, offset, data) => {
                    let (to, _) = Self::bound(state, context, *dst)?;
                    self.write_memory(state, context, to, *offset, data);
                }
            }
        }
        state
            .contexts
            .get_mut(context)?
            .objects
            .insert(id, Object::Commands(recorded));
        Some(())
    }
}

/// Rouses the test's display end.
struct Roused(Arc<(StdMutex<bool>, StdCondvar)>);

impl Rouse for Roused {
    fn rouse(&self) -> bool {
        *self.0 .0.lock().expect("unpoisoned") = true;
        self.0 .1.notify_all();
        true
    }
}

/// Threads of the test's own.
#[derive(Default)]
struct Threads(StdMutex<Vec<JoinHandle<()>>>);

impl Spawn for Threads {
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> bool {
        self.0
            .lock()
            .expect("unpoisoned")
            .push(std::thread::spawn(body));
        true
    }
}

/// Real time, so a job never answered times out instead of holding the test.
struct Wall(std::time::Instant);

impl MonotonicClock for Wall {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Longer than any answer a correct run waits for.
const ANSWERED: Duration64 = Duration64::from_secs(10);

/// A host, the desk to it, and the display end serving it on a thread.
struct Rig {
    host: Arc<Host>,
    desk: Arc<Desk<Then>>,
    threads: Threads,
    stop: Arc<AtomicBool>,
    roused: Arc<(StdMutex<bool>, StdCondvar)>,
    display: Option<JoinHandle<()>>,
}

impl Rig {
    fn new() -> Self {
        let whole = HwResource::memory_region(0x10_0000_0000, REGION_BYTES as u64, 1);
        let host = Arc::new(Host {
            region: Arc::new(MockRegion::new(REGION_BYTES)),
            shared: Arc::new(Mutex::new(SharedMemory::new(whole).expect("a region"))),
            state: StdMutex::new(HostState::default()),
            changed: StdCondvar::new(),
        });
        let roused = Arc::new((StdMutex::new(false), StdCondvar::new()));
        let desk = Arc::new(Desk::new(Box::new(Roused(Arc::clone(&roused)))));
        let stop = Arc::new(AtomicBool::new(false));
        let display = {
            let (host, desk, stop, roused) = (
                Arc::clone(&host),
                Arc::clone(&desk),
                Arc::clone(&stop),
                Arc::clone(&roused),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    {
                        let (flag, wake) = &*roused;
                        let mut set = flag.lock().expect("unpoisoned");
                        while !*set && !stop.load(Ordering::SeqCst) {
                            set = wake.wait(set).expect("unpoisoned");
                        }
                        *set = false;
                    }
                    desk.begin_turn();
                    while let Some((tag, order)) = desk.take() {
                        let answer = host.carry_out(order);
                        host.changed.notify_all();
                        desk.answer(tag, answer);
                    }
                }
            })
        };
        Self {
            host,
            desk,
            threads: Threads::default(),
            stop,
            roused,
            display: Some(display),
        }
    }

    fn device(&self) -> Device {
        self.device_on(Arc::new(Still))
    }

    fn device_on(&self, clock: Arc<dyn MonotonicClock + Send + Sync>) -> Device {
        device(
            Arc::clone(&self.desk),
            Arc::clone(&self.host.shared),
            &self.threads,
            clock,
        )
        .expect("a device")
    }

    fn misbehave(&self, how: Misbehaviour) {
        self.host.state.lock().expect("unpoisoned").misbehaves = how;
    }

    /// Wait until the host's state is as `until` says, failing past a budget
    /// no correct run comes near.
    fn wait_for(&self, until: impl Fn(&HostState) -> bool) {
        let state = self.host.state.lock().expect("unpoisoned");
        let (state, _) = self
            .host
            .changed
            .wait_timeout_while(state, std::time::Duration::from_secs(10), |state| {
                !until(state)
            })
            .expect("unpoisoned");
        assert!(until(&state), "the host never came to the state awaited");
    }

    /// Answer everything still with the desk, then stop.
    fn finish(mut self) -> Arc<Host> {
        self.desk.close();
        for thread in self.threads.0.lock().expect("unpoisoned").drain(..) {
            thread.join().expect("the answering thread ended");
        }
        {
            // Under the flag's lock, so the display end cannot miss it between
            // its check and its wait.
            let (flag, wake) = &*self.roused;
            let _held = flag.lock().expect("unpoisoned");
            self.stop.store(true, Ordering::SeqCst);
            wake.notify_all();
        }
        if let Some(display) = self.display.take() {
            display.join().expect("the display end ended");
        }
        Arc::clone(&self.host)
    }
}

fn context(device: &Device) -> tairix_gpu::Context {
    device
        .create_context(&ContextDesc::default())
        .expect("a context")
}

fn buffer(
    context: &tairix_gpu::Context,
    memory_type: u32,
    size: u64,
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    let memory = context
        .allocate(&AllocationDesc { size, memory_type })
        .expect("memory");
    let usage = BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST);
    let buffer = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage,
            },
        )
        .expect("a buffer");
    (memory, buffer)
}

fn submit(
    context: &tairix_gpu::Context,
    commands: &tairix_gpu::CommandBuffer,
) -> Result<(), Error> {
    submit_by(context, commands, SECOND)
}

fn submit_by(
    context: &tairix_gpu::Context,
    commands: &tairix_gpu::CommandBuffer,
    deadline: Duration64,
) -> Result<(), Error> {
    let fence = context.queue(0, 0).expect("a queue").submit(&Submission {
        command_buffers: &[commands],
        waits: &[],
        signals: &[],
        deadline,
    });
    fence.map_err(|refused| refused.error)?.wait(deadline)
}

/// That `kept` still runs work once another context of its device is lost.
fn goes_on(kept: &tairix_gpu::Context) {
    let (memory, target) = buffer(kept, 1, 64);
    let mut fill = kept.command_buffer();
    fill.fill_buffer(&target, 0, 64, 0x0303_0303)
        .expect("recorded");
    submit_by(kept, &fill, ANSWERED).expect("another context's work goes on");
    let mut seen = [0u8; 4];
    memory.read(0, &mut seen).expect("read");
    assert_eq!(seen, [3; 4]);
}

/// That everything the host made for the backend was let go.
fn let_go(host: &Host) {
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 0, "every blob ended");
    assert!(
        !state.freed_exported,
        "no memory freed while a blob made it the device's"
    );
    assert_eq!(host.region.mapped(), 0, "no part left mapped");
    assert_eq!(
        host.shared
            .lock()
            .reserve(REGION_BYTES as u64)
            .map(|part| part.size())
            .ok(),
        Some(REGION_BYTES as u64),
        "every part given back"
    );
}

#[test]
fn the_device_states_coherent_memory_and_transfers_alone_and_its_probe_is_let_go() {
    let rig = Rig::new();
    let device = rig.device();
    let info = device.info();
    assert_eq!(info.queue_families.len(), 1);
    assert_eq!(
        info.queue_families[0].capabilities,
        QueueCapabilities::TRANSFER
    );
    assert_eq!(info.queue_families[0].queues, 2);
    let stated: Vec<MemoryProperties> = info
        .memory_types
        .iter()
        .map(|kind| kind.properties)
        .collect();
    assert_eq!(
        stated,
        [
            MemoryProperties::DEVICE_LOCAL,
            MemoryProperties::HOST_VISIBLE
                .with(MemoryProperties::HOST_COHERENT)
                .with(MemoryProperties::HOST_CACHED)
        ]
    );
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn host_visible_memory_is_zeroed_written_and_read_through_the_driver() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (memory, whole) = buffer(&context, 1, 4096 + 12);
    let mut seen = vec![0xffu8; 4096 + 12];
    memory.read(0, &mut seen).expect("read");
    assert!(
        seen.iter().all(|&byte| byte == 0),
        "zeroed: the host's bytes never reach the client"
    );
    memory.write(100, &[1, 2, 3, 4]).expect("written");
    let mut back = [0u8; 4];
    memory.read(100, &mut back).expect("read");
    assert_eq!(back, [1, 2, 3, 4]);
    drop((memory, whole, context, device));
    let_go(&rig.finish());
}

#[test]
fn copies_fills_and_updates_land_through_device_local_memory() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (visible, staging) = buffer(&context, 1, 256);
    let (local, deep) = buffer(&context, 0, 256);
    visible.write(0, &[7; 64]).expect("written");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(
            &staging,
            &deep,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 0,
                size: 64,
            }],
        )
        .expect("recorded");
    commands
        .fill_buffer(&deep, 64, 64, 0x0101_0101)
        .expect("recorded");
    commands
        .update_buffer(&deep, 128, &[9; 16])
        .expect("recorded");
    commands
        .copy_buffer(
            &deep,
            &staging,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 0,
                size: 256,
            }],
        )
        .expect("recorded");
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; 256];
    visible.read(0, &mut seen).expect("read");
    assert_eq!(&seen[..64], &[7; 64]);
    assert_eq!(&seen[64..128], &[1; 64]);
    assert_eq!(&seen[128..144], &[9; 16]);
    assert!(
        seen[144..].iter().all(|&byte| byte == 0),
        "the device-local memory was zeroed"
    );
    drop((commands, visible, staging, local, deep, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_longer_than_a_part_travels_in_parts_and_lands_whole() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let size = 4 * MAX_UPDATE_BYTES as u64;
    let (memory, target) = buffer(&context, 1, size);
    let mut commands = context.command_buffer();
    for part in 0..4u8 {
        let offset = u64::from(part) * MAX_UPDATE_BYTES as u64;
        commands
            .update_buffer(&target, offset, &vec![part + 1; MAX_UPDATE_BYTES])
            .expect("recorded");
    }
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    for (part, chunk) in seen.chunks(MAX_UPDATE_BYTES).enumerate() {
        assert!(
            chunk.iter().all(|&byte| usize::from(byte) == part + 1),
            "part {part} landed"
        );
    }
    drop((commands, memory, target, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_the_host_refuses_loses_its_context_alone_and_everything_is_let_go() {
    let rig = Rig::new();
    let device = rig.device();
    let refused = context(&device);
    let kept = context(&device);
    let (memory, target) = buffer(&refused, 1, 64);
    let mut commands = refused.command_buffer();
    commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
    rig.misbehave(Misbehaviour::RefuseSubmits);
    assert!(matches!(submit(&refused, &commands), Err(Error::Lost(_))));
    assert!(matches!(refused.status(), Err(Error::Lost(_))));
    rig.misbehave(Misbehaviour::Nothing);
    let (other, aim) = buffer(&kept, 1, 64);
    let mut fill = kept.command_buffer();
    fill.fill_buffer(&aim, 0, 64, 0x0202_0202)
        .expect("recorded");
    submit(&kept, &fill).expect("another context's work goes on");
    let mut seen = [0u8; 4];
    other.read(0, &mut seen).expect("read");
    assert_eq!(seen, [2; 4]);
    drop((
        commands, memory, target, refused, fill, other, aim, kept, device,
    ));
    let_go(&rig.finish());
}

#[test]
fn a_lost_context_answers_the_work_it_still_holds_and_ends_while_held() {
    let rig = Rig::new();
    let device = rig.device_on(Arc::new(Wall(std::time::Instant::now())));
    let kept = context(&device);
    let refused = context(&device);
    // More parts than the job area holds, so the last still waits when the
    // first is refused.
    let size = 4 * MAX_UPDATE_BYTES as u64;
    let (memory, target) = buffer(&refused, 1, size);
    let mut commands = refused.command_buffer();
    for part in 0..4u8 {
        let offset = u64::from(part) * MAX_UPDATE_BYTES as u64;
        commands
            .update_buffer(&target, offset, &vec![part; MAX_UPDATE_BYTES])
            .expect("recorded");
    }
    rig.misbehave(Misbehaviour::RefuseSubmits);
    let answered = submit_by(&refused, &commands, ANSWERED);
    rig.misbehave(Misbehaviour::Nothing);
    assert!(matches!(answered, Err(Error::Lost(_))), "{answered:?}");
    rig.wait_for(|state| state.contexts.iter().count() == 1);
    goes_on(&kept);
    drop((commands, memory, target, refused, kept, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_whose_number_never_lands_loses_its_context_alone() {
    let rig = Rig::new();
    let device = rig.device_on(Arc::new(Wall(std::time::Instant::now())));
    let kept = context(&device);
    let silent = context(&device);
    let (memory, target) = buffer(&silent, 1, 64);
    let mut commands = silent.command_buffer();
    commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
    rig.misbehave(Misbehaviour::IgnoreWork);
    let answered = submit_by(&silent, &commands, ANSWERED);
    rig.misbehave(Misbehaviour::Nothing);
    assert!(matches!(answered, Err(Error::Lost(_))), "{answered:?}");
    rig.wait_for(|state| state.contexts.iter().count() == 1);
    goes_on(&kept);
    drop((commands, memory, target, silent, kept, device));
    let_go(&rig.finish());
}

#[test]
fn a_blob_the_device_would_not_unmap_keeps_its_part_and_its_memory() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (memory, whole) = buffer(&context, 1, 4096);
    rig.misbehave(Misbehaviour::RefuseUnmaps);
    drop((memory, whole));
    rig.wait_for(|state| state.unmaps_refused == 1);
    rig.misbehave(Misbehaviour::Nothing);
    drop((context, device));
    let host = rig.finish();
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 1, "the blob still mapped stays");
    assert!(!state.freed_exported, "its memory is never freed under it");
    assert_eq!(host.region.mapped(), 0, "no window left mapped");
    assert!(
        host.shared.lock().reserve(REGION_BYTES as u64).is_err(),
        "the part it may still hold is kept from every other"
    );
}

#[test]
fn memory_whose_blob_may_be_mapped_is_kept_with_it() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    rig.misbehave(Misbehaviour::DoubtMaps);
    let refused = context.allocate(&AllocationDesc {
        size: 4096,
        memory_type: 1,
    });
    rig.misbehave(Misbehaviour::Nothing);
    assert!(refused.is_err(), "memory not mapped is not answered");
    drop((context, device));
    let host = rig.finish();
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 1, "the blob it may map stays");
    assert!(!state.freed_exported, "its memory is never freed under it");
}

#[test]
fn nothing_joins_the_queue_of_a_context_lost_or_ended() {
    let mut state = ContextState {
        next_id: 1,
        spans: tairix_collections::RangeMap::new(),
        free_commands: Vec::new(),
        pending: alloc::collections::VecDeque::new(),
        in_flight: 0,
        next_seq: 0,
        lost: false,
        ended: false,
    };
    assert_eq!(state.admits(), Admits::Room);
    state.lost = true;
    assert_eq!(state.admits(), Admits::Gone);
    state.lost = false;
    state.ended = true;
    assert_eq!(state.admits(), Admits::Gone);
}

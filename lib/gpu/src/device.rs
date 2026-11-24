//! The seam: a device, the contexts made on it, and what a context holds and
//! runs.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_collections::Key;
use tairix_inline::ArrayVec;

use crate::backend::Backend;
use crate::command::{Command, CopyRegion};
use crate::fair::{Fair, FairGuard};
use crate::host::{owned, ExportId, Host, Object, Served};
use crate::info::{BufferUsage, DeviceInfo};
use crate::memory::MemoryUse;
use crate::model::{
    AllocationKey, BufferKey, ContextKey, Model, Pick, Request, TimelineKey, Woken,
};
use crate::{
    Error, SubmitError, MAX_COPY_REGIONS, MAX_SUBMISSION_BUFFERS, MAX_SUBMISSION_COMMANDS,
    MAX_SUBMISSION_POINTS, MAX_UPDATE_BYTES,
};

/// What every handle of one device shares: the model, its description, the
/// clock its deadlines are read from, and the backend its work runs on.
pub(crate) struct Shared<B: Backend> {
    /// Had in turn, so no thread that locks it again at once keeps another
    /// out.
    pub(crate) model: Fair<Model<B>>,
    pub(crate) info: DeviceInfo,
    pub(crate) clock: Arc<dyn MonotonicClock + Send + Sync>,
    pub(crate) backend: B,
}

/// The software device behind `host`, for tests that look into it.
#[cfg(test)]
pub(crate) fn as_software(host: &Arc<dyn Host>) -> &Shared<crate::software::Software> {
    host.as_any().downcast_ref().expect("a software device")
}

impl<B: Backend> Shared<B> {
    /// Take `item` out of the context's table `pick` names, letting it go
    /// once the device's lock is: it may hold the last of a memory.
    pub(crate) fn take<K: Key, V>(&self, context: ContextKey, pick: Pick<B, K, V>, item: K) {
        let released = self.model.lock().release(context, pick, item);
        drop(released);
    }

    /// Have the work a change just made ready run, as the backend runs it.
    pub(crate) fn drive<'a>(&'a self, model: FairGuard<'a, Model<B>>) {
        if model.has_ready() {
            B::drive(self, model);
        }
    }
}

/// Wake every waiter `woken` holds, emptying it.
pub(crate) fn notify(woken: &mut Woken) {
    for wake in woken.iter() {
        wake.notify_all();
    }
    woken.clear();
}

/// A device work is made on: a GPU, or the CPU standing in for one.
pub struct Device {
    pub(crate) host: Arc<dyn Host>,
    /// The device itself, where it is in this process: what a runtime
    /// serving it to other processes reaches.
    pub(crate) served: Option<Arc<dyn Served>>,
}

impl Device {
    /// The device `shared` holds, in this process.
    pub(crate) fn local<B: Backend>(shared: Arc<Shared<B>>) -> Self {
        Self {
            host: Arc::clone(&shared) as Arc<dyn Host>,
            served: Some(shared),
        }
    }

    /// What the device is.
    #[must_use]
    pub fn info(&self) -> &DeviceInfo {
        self.host.info()
    }

    /// A context: its own memory, objects and queues, which no other context
    /// can reach.
    ///
    /// # Errors
    ///
    /// [`Error::OverLimit`] where its own state would pass its limit, then
    /// [`Error::HeapFull`] where that and the device's room for it would pass
    /// the heap, and [`Error::OutOfMemory`] where the host cannot hold it. The
    /// room the device made for it ahead of a refusal stays, charged to the
    /// heap, for the next.
    pub fn create_context(&self, desc: &ContextDesc) -> Result<Context, Error> {
        let key = self.host.create_context(desc.memory_limit)?;
        Ok(Context {
            host: Arc::clone(&self.host),
            key,
        })
    }
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("info", self.host.info())
            .finish_non_exhaustive()
    }
}

/// How a context is made.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextDesc {
    /// The most bytes it may take, as [`Context`] says what is charged, or,
    /// when `None`, as much as the device's heaps hold.
    pub memory_limit: Option<u64>,
}

/// The unit of isolation: memory, objects and queues no other context can
/// name. Dropping it releases all of them; handles it made then refuse every
/// request with [`Error::Destroyed`].
///
/// # Memory
///
/// Every request that makes the device hold memory — an allocation, an
/// object, room in one of the context's tables or queues, a recorded update,
/// a submission — is charged to the context's limit. It is refused with
/// [`Error::OverLimit`] past the limit, [`Error::HeapFull`] past its heap's
/// size, and [`Error::OutOfMemory`] where, within both, the host refuses the
/// memory; a lost context refuses it with [`Error::Lost`], a released one
/// with [`Error::Destroyed`]. A refusal changes nothing but the room a table
/// or queue made ahead of it, which stays, charged, for the next.
pub struct Context {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) key: ContextKey,
}

impl Context {
    /// Whether the context still takes work.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once it is lost.
    pub fn status(&self) -> Result<(), Error> {
        self.host.status(self.key)
    }

    /// What its memory limit is spent on: its allocations, and the host
    /// memory its own state, its objects, its tables' room and its queued
    /// work keep on the device, each for as long as anything holds it.
    #[must_use]
    pub fn memory_used(&self) -> MemoryUse {
        self.host.memory_used(self.key)
    }

    /// `desc.size` zeroed bytes of memory type `desc.memory_type`.
    ///
    /// # Errors
    ///
    /// [`Error::OverLimit`] past the context's limit, [`Error::HeapFull`] past
    /// the heap's size, and [`Error::OutOfMemory`] when, within both, the
    /// memory cannot be had; [`Error::Empty`], [`Error::TooLarge`] and
    /// [`Error::NoSuchMemoryType`] for what no device could allocate.
    pub fn allocate(&self, desc: &AllocationDesc) -> Result<Allocation, Error> {
        let key = self.host.allocate(self.key, desc)?;
        Ok(Allocation {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
            desc: *desc,
        })
    }

    /// Take into this context memory another context of the same device
    /// exported.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for memory of another device, and those of every
    /// request that makes memory ([`Context#memory`]).
    pub fn import(&self, export: Export) -> Result<Allocation, Error> {
        if !Arc::ptr_eq(&self.host, &export.host) {
            return Err(Error::Foreign);
        }
        // The export is spent whatever the answer: a refusal lets its memory
        // go with it.
        let imported = self.host.import(self.key, export.key);
        drop(export);
        let imported = imported?;
        Ok(Allocation {
            host: Arc::clone(&self.host),
            context: self.key,
            key: imported.allocation,
            desc: imported.desc,
        })
    }

    /// A buffer over part of one of this context's allocations.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's allocation; [`Error::Empty`],
    /// [`Error::Usage`], [`Error::Misaligned`] and [`Error::OutOfRange`] for
    /// a buffer the allocation cannot hold as asked; and those of every
    /// request that makes memory ([`Context#memory`]).
    pub fn create_buffer(
        &self,
        allocation: &Allocation,
        desc: &BufferDesc,
    ) -> Result<Buffer, Error> {
        owned(&allocation.host, allocation.context, &self.host, self.key)?;
        let key = self.host.create_buffer(self.key, allocation.key, desc)?;
        Ok(Buffer {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
            size: desc.size,
            usage: desc.usage,
        })
    }

    /// A timeline starting at `initial`.
    ///
    /// # Errors
    ///
    /// Those of every request that makes memory ([`Context#memory`]).
    pub fn create_timeline(&self, initial: u64) -> Result<Timeline, Error> {
        let key = self.host.create_timeline(self.key, initial)?;
        Ok(Timeline {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
        })
    }

    /// Queue `index` of family `family`.
    ///
    /// # Errors
    ///
    /// [`Error::NoSuchQueue`] for one the device does not have.
    pub fn queue(&self, family: u32, index: u32) -> Result<Queue, Error> {
        let slot = self.host.info().queue_slot(family, index)?;
        Ok(Queue {
            host: Arc::clone(&self.host),
            context: self.key,
            slot,
            family,
            index,
        })
    }

    /// An empty command buffer for this context's queues.
    #[must_use]
    pub fn command_buffer(&self) -> CommandBuffer {
        CommandBuffer {
            host: Arc::clone(&self.host),
            context: self.key,
            commands: Vec::new(),
            counted: 0,
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.host.destroy_context(self.key);
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// How an allocation is made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocationDesc {
    /// Its size in bytes.
    pub size: u64,
    /// Its memory type, an index into [`DeviceInfo::memory_types`].
    pub memory_type: u32,
}

/// Memory of one memory type, held by one context, and released when this
/// and everything made over it is.
pub struct Allocation {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) context: ContextKey,
    pub(crate) key: AllocationKey,
    desc: AllocationDesc,
}

impl Allocation {
    /// Its size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.desc.size
    }

    /// Its memory type.
    #[must_use]
    pub const fn memory_type(&self) -> u32 {
        self.desc.memory_type
    }

    /// Write `data` at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// [`Error::NotHostVisible`] for memory the CPU cannot reach, and
    /// [`Error::OutOfRange`] past its end.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.host.write(self.context, self.key, offset, data)
    }

    /// Read `out.len()` bytes at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        self.host.read(self.context, self.key, offset, out)
    }

    /// A token another context of this device imports the memory through.
    /// The memory stays charged to this context, and the token to its host
    /// memory, until it is imported or let go.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] on a device whose contexts reach none of one
    /// another's memory ([`Features::SHARED_MEMORY`](crate::Features)),
    /// [`Error::Lost`] once the context is lost, and those of every request
    /// that makes memory ([`Context#memory`]).
    pub fn export(&self) -> Result<Export, Error> {
        let key = self.host.export(self.context, self.key)?;
        Ok(Export {
            host: Arc::clone(&self.host),
            key,
            desc: self.desc,
        })
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.host
            .release(self.context, Object::Allocation(self.key));
    }
}

impl fmt::Debug for Allocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Allocation")
            .field("size", &self.desc.size)
            .field("memory_type", &self.desc.memory_type)
            .finish_non_exhaustive()
    }
}

/// An allocation's memory on its way to another context, which
/// [`Context::import`] takes it into. It holds the memory until then.
pub struct Export {
    host: Arc<dyn Host>,
    key: ExportId,
    desc: AllocationDesc,
}

impl Drop for Export {
    fn drop(&mut self) {
        self.host.drop_export(self.key);
    }
}

impl fmt::Debug for Export {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Export")
            .field("size", &self.desc.size)
            .field("memory_type", &self.desc.memory_type)
            .finish_non_exhaustive()
    }
}

/// How a buffer is made over an allocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BufferDesc {
    /// Where it starts in the allocation: a multiple of
    /// [`Limits::buffer_alignment`](crate::Limits::buffer_alignment).
    pub offset: u64,
    /// Its size in bytes.
    pub size: u64,
    /// What it may be used for.
    pub usage: BufferUsage,
}

/// A typed span of an allocation, used only as its usage says. It keeps its
/// memory while it lives, whatever becomes of the allocation's handle.
pub struct Buffer {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) context: ContextKey,
    pub(crate) key: BufferKey,
    size: u64,
    usage: BufferUsage,
}

impl Buffer {
    /// Its size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// What it may be used for.
    #[must_use]
    pub const fn usage(&self) -> BufferUsage {
        self.usage
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.host.release(self.context, Object::Buffer(self.key));
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("size", &self.size)
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// A counter that only advances, set from the CPU or by completed work, which
/// work and the CPU wait on.
///
/// A value is reached only once every signal up to it has landed, so a wait
/// for it sees all the work those signals followed.
pub struct Timeline {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) context: ContextKey,
    pub(crate) key: TimelineKey,
}

impl Timeline {
    /// Where it stands.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn value(&self) -> Result<u64, Error> {
        self.host.timeline_value(self.context, self.key)
    }

    /// Set it to `value` from the CPU, readying any work that was waiting for
    /// it: that runs on the device's worker, or, where it has none, on this
    /// thread or the one already running its engine's work.
    ///
    /// # Errors
    ///
    /// [`Error::TimelineOrder`] unless `value` is past where it stands and
    /// short of every signal queued work will make.
    pub fn signal(&self, value: u64) -> Result<(), Error> {
        self.host.signal(self.context, self.key, value)
    }

    /// Wait until it reaches `value`.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`] when `timeout` elapses first, and [`Error::Lost`]
    /// when the context is lost meanwhile.
    pub fn wait(&self, value: u64, timeout: Duration64) -> Result<(), Error> {
        self.host.wait(self.context, self.key, value, timeout)
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        self.host.release(self.context, Object::Timeline(self.key));
    }
}

impl fmt::Debug for Timeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timeline")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// One of a context's queues: its work runs in the order submitted, each
/// submission seeing everything the ones before it did.
#[derive(Clone)]
pub struct Queue {
    host: Arc<dyn Host>,
    context: ContextKey,
    slot: usize,
    family: u32,
    index: u32,
}

impl Queue {
    /// Its family.
    #[must_use]
    pub const fn family(&self) -> u32 {
        self.family
    }

    /// Its index within its family.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Check `submission` whole and queue it to run once its waits are met.
    ///
    /// # Errors
    ///
    /// Refused whole, changing nothing but the room its queues may have made
    /// ahead of it, for anything of another context
    /// ([`Error::Foreign`]), any object since released ([`Error::Stale`]),
    /// any range past what it names, any use a buffer was not made for, any
    /// signal that would not advance its timeline, a deadline of zero or past
    /// the device's longest, or more than a submission carries, and for
    /// what every request that makes memory is ([`Context#memory`]); the
    /// error names the refused command where there is one.
    pub fn submit(&self, submission: &Submission<'_>) -> Result<Fence, SubmitError> {
        let mut buffers: ArrayVec<&[Command], MAX_SUBMISSION_BUFFERS> = ArrayVec::new();
        for buffer in submission.command_buffers {
            owned(&buffer.host, buffer.context, &self.host, self.context)?;
            buffers
                .try_push(&buffer.commands)
                .map_err(|_| Error::TooMany)?;
        }
        let waits = self.points(submission.waits)?;
        let signals = self.points(submission.signals)?;
        let request = Request {
            queue: self.slot,
            buffers: &buffers,
            waits: &waits,
            signals: &signals,
            span: submission.deadline.saturating_total_nanos(),
        };
        let (timeline, value) = self.host.submit(self.context, &request)?;
        Ok(Fence {
            host: Arc::clone(&self.host),
            context: self.context,
            timeline,
            value,
        })
    }

    fn points(
        &self,
        points: &[(&Timeline, u64)],
    ) -> Result<ArrayVec<(TimelineKey, u64), MAX_SUBMISSION_POINTS>, Error> {
        let mut keys = ArrayVec::new();
        for &(timeline, value) in points {
            owned(&timeline.host, timeline.context, &self.host, self.context)?;
            keys.try_push((timeline.key, value))
                .map_err(|_| Error::TooMany)?;
        }
        Ok(keys)
    }
}

impl fmt::Debug for Queue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("family", &self.family)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

/// What one submission asks a queue for.
#[derive(Copy, Clone, Debug)]
pub struct Submission<'a> {
    /// Its command buffers, run in order.
    pub command_buffers: &'a [&'a CommandBuffer],
    /// The timeline values it waits for before it starts.
    pub waits: &'a [(&'a Timeline, u64)],
    /// The timeline values it sets once it completes.
    pub signals: &'a [(&'a Timeline, u64)],
    /// The longest it may run once started before its context is lost.
    pub deadline: Duration64,
}

/// A submission's completion, as the CPU waits on it.
#[derive(Clone)]
pub struct Fence {
    host: Arc<dyn Host>,
    context: ContextKey,
    timeline: TimelineKey,
    value: u64,
}

impl Fence {
    /// Wait until the submission has completed.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`] when `timeout` elapses first, and [`Error::Lost`]
    /// when the context is lost before the submission completes.
    pub fn wait(&self, timeout: Duration64) -> Result<(), Error> {
        self.host
            .wait(self.context, self.timeline, self.value, timeout)
    }

    /// Whether the submission has completed.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn is_signalled(&self) -> Result<bool, Error> {
        self.host.reached(self.context, self.timeline, self.value)
    }
}

impl fmt::Debug for Fence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fence")
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

/// Commands recorded for one context's queues. Recording checks only that
/// what is named belongs to the context; the rest is checked whole when a
/// submission carries it, and the buffer can be submitted again.
pub struct CommandBuffer {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) context: ContextKey,
    commands: Vec<Command>,
    /// The commands they count as: a pass one, and each of its draws another.
    counted: usize,
}

impl CommandBuffer {
    /// How many commands it holds, a pass's draws each counted.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.counted
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.counted == 0
    }

    /// Drop every command, keeping the room they took.
    pub fn clear(&mut self) {
        self.commands.clear();
        self.counted = 0;
    }

    /// The commands recorded.
    #[cfg(test)]
    pub(crate) fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub(crate) fn record(&mut self, command: Command) -> Result<(), Error> {
        let counted = self
            .counted
            .checked_add(command.count())
            .filter(|&counted| counted <= MAX_SUBMISSION_COMMANDS)
            .ok_or(Error::TooMany)?;
        self.commands
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.commands.push(command);
        self.counted = counted;
        Ok(())
    }

    pub(crate) fn named(&self, buffer: &Buffer) -> Result<BufferKey, Error> {
        owned(&buffer.host, buffer.context, &self.host, self.context)?;
        Ok(buffer.key)
    }

    /// Copy `regions` of `src` into `dst`. No byte may be written twice, nor,
    /// within one memory, both read and written.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context, and
    /// [`Error::TooMany`] past [`MAX_COPY_REGIONS`] regions.
    pub fn copy_buffer(
        &mut self,
        src: &Buffer,
        dst: &Buffer,
        regions: &[CopyRegion],
    ) -> Result<(), Error> {
        let (src, dst) = (self.named(src)?, self.named(dst)?);
        if regions.len() > MAX_COPY_REGIONS {
            return Err(Error::TooMany);
        }
        let regions = tairix_util::fallible::copied(regions).ok_or(Error::OutOfMemory)?;
        self.record(Command::Copy { src, dst, regions })
    }

    /// Fill `size` bytes of `dst` at `offset`, both whole words, with `word`
    /// in little-endian byte order.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context.
    pub fn fill_buffer(
        &mut self,
        dst: &Buffer,
        offset: u64,
        size: u64,
        word: u32,
    ) -> Result<(), Error> {
        let dst = self.named(dst)?;
        self.record(Command::Fill {
            dst,
            offset,
            size,
            word,
        })
    }

    /// Write `data`, whole words and at most [`MAX_UPDATE_BYTES`], into `dst`
    /// at `offset` when the command runs. The bytes are kept, charged to the
    /// context, until the command buffer and every submission of it are done
    /// with them.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context,
    /// [`Error::TooMany`] past [`MAX_UPDATE_BYTES`], [`Error::OverLimit`] or
    /// [`Error::HeapFull`] where the bytes would pass the context's limit or
    /// the heap, and [`Error::Lost`] or [`Error::Destroyed`] for a context
    /// that can take no more.
    pub fn update_buffer(&mut self, dst: &Buffer, offset: u64, data: &[u8]) -> Result<(), Error> {
        let dst = self.named(dst)?;
        if data.len() > MAX_UPDATE_BYTES {
            return Err(Error::TooMany);
        }
        let bytes = tairix_util::fallible::copied(data).ok_or(Error::OutOfMemory)?;
        let data = self.host.record_update(self.context, bytes)?;
        self.record(Command::Update { dst, offset, data })
    }
}

impl fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandBuffer")
            .field("commands", &self.counted)
            .finish_non_exhaustive()
    }
}

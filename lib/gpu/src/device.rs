//! The seam: a device, the contexts made on it, and what a context holds and
//! runs.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_inline::ArrayVec;
use tairix_rt::sync::{Mutex, MutexGuard};

use crate::command::{Command, CopyRegion};
use crate::info::{BufferUsage, DeviceInfo};
use crate::memory::Memory;
use crate::model::{
    AllocationKey, BufferKey, ContextKey, Model, Point, Request, TimelineKey, Woken,
};
use crate::software::{self, Outcome};
use crate::{
    Error, SubmitError, MAX_COPY_REGIONS, MAX_SUBMISSION_BUFFERS, MAX_SUBMISSION_COMMANDS,
    MAX_SUBMISSION_POINTS, MAX_UPDATE_BYTES,
};

/// What every handle of one device shares.
pub(crate) struct Shared {
    model: Mutex<Model>,
    info: DeviceInfo,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
}

impl Shared {
    /// Run, on this thread, every job the device has room for and that is
    /// ready, until none is: work runs on the thread whose act made it
    /// ready, or on the one already running its engine's work.
    fn drive<'a>(&'a self, mut model: MutexGuard<'a, Model>) {
        let mut woken = Woken::new();
        while let Some(dispatch) = model.dispatch(self.clock.now_ns()) {
            drop(model);
            notify(&mut woken);
            let outcome = software::run(&dispatch.ops, dispatch.deadline, &*self.clock);
            model = self.model.lock();
            match outcome {
                Outcome::Done => woken = model.complete(dispatch.job, false),
                Outcome::Fault => woken = model.complete(dispatch.job, true),
                Outcome::Overran => model.reset(dispatch.engine, self.clock.now_ns(), &|_| true),
            }
        }
        drop(model);
        notify(&mut woken);
    }

    /// Wait until `timeline` reaches `value`, the context is lost or
    /// released, or `timeout` elapses.
    fn wait(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64,
    ) -> Result<(), Error> {
        let limit = self
            .clock
            .now_ns()
            .saturating_add(timeout.saturating_total_nanos());
        let mut model = self.model.lock();
        loop {
            let wake = match model.point(context, timeline, value)? {
                Point::Reached => return Ok(()),
                Point::Pending(wake) => wake,
            };
            let now = self.clock.now_ns();
            if now >= limit {
                return Err(Error::TimedOut);
            }
            model = wake
                .wait_timeout(model, Duration64::from_nanos(limit - now))
                .0;
        }
    }
}

fn notify(woken: &mut Woken) {
    for wake in woken.iter() {
        wake.notify_all();
    }
    woken.clear();
}

/// Whether a handle of `shared`'s device and `context` belongs where it is
/// used.
fn owned(
    shared: &Arc<Shared>,
    context: ContextKey,
    by: &Arc<Shared>,
    key: ContextKey,
) -> Result<(), Error> {
    if Arc::ptr_eq(shared, by) && context == key {
        Ok(())
    } else {
        Err(Error::Foreign)
    }
}

/// A device work is made on: a GPU, or the CPU standing in for one.
pub struct Device {
    shared: Arc<Shared>,
}

impl Device {
    pub(crate) fn from_model(model: Model, clock: Arc<dyn MonotonicClock + Send + Sync>) -> Self {
        let info = model.info().clone();
        Self {
            shared: Arc::new(Shared {
                model: Mutex::new(model),
                info,
                clock,
            }),
        }
    }

    /// What the device is.
    #[must_use]
    pub fn info(&self) -> &DeviceInfo {
        &self.shared.info
    }

    /// A context: its own memory, objects and queues, which no other context
    /// can reach.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] when its state cannot be had.
    pub fn create_context(&self, desc: &ContextDesc) -> Result<Context, Error> {
        let key = self.shared.model.lock().create_context(desc.memory_limit)?;
        Ok(Context {
            shared: Arc::clone(&self.shared),
            key,
        })
    }
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("info", &self.shared.info)
            .finish_non_exhaustive()
    }
}

/// How a context is made.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextDesc {
    /// The most bytes its allocations may take, or, when `None`, as much as
    /// the device's heaps hold.
    pub memory_limit: Option<u64>,
}

/// The unit of isolation: memory, objects and queues no other context can
/// name. Dropping it releases all of them; handles it made then refuse every
/// request with [`Error::Destroyed`].
pub struct Context {
    shared: Arc<Shared>,
    key: ContextKey,
}

impl Context {
    /// Whether the context still takes work.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once it is lost.
    pub fn status(&self) -> Result<(), Error> {
        self.shared.model.lock().status(self.key)
    }

    /// The bytes its allocations take from its memory limit, for as long as
    /// anything holds them.
    #[must_use]
    pub fn memory_used(&self) -> u64 {
        self.shared.model.lock().memory_taken(self.key).unwrap_or(0)
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
        let key = self
            .shared
            .model
            .lock()
            .allocate(self.key, desc.size, desc.memory_type)?;
        Ok(Allocation {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            size: desc.size,
            memory_type: desc.memory_type,
        })
    }

    /// Take into this context memory another context of the same device
    /// exported.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for memory of another device.
    pub fn import(&self, export: Export) -> Result<Allocation, Error> {
        if !Arc::ptr_eq(&self.shared, &export.shared) {
            return Err(Error::Foreign);
        }
        let Export {
            memory,
            memory_type,
            size,
            ..
        } = export;
        let key = self
            .shared
            .model
            .lock()
            .import(self.key, memory, memory_type, size)?;
        Ok(Allocation {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            size,
            memory_type,
        })
    }

    /// A buffer over part of one of this context's allocations.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for another context's allocation; [`Error::Empty`],
    /// [`Error::Usage`], [`Error::Misaligned`] and [`Error::OutOfRange`] for
    /// a buffer the allocation cannot hold as asked.
    pub fn create_buffer(
        &self,
        allocation: &Allocation,
        desc: &BufferDesc,
    ) -> Result<Buffer, Error> {
        owned(
            &allocation.shared,
            allocation.context,
            &self.shared,
            self.key,
        )?;
        let key = self.shared.model.lock().create_buffer(
            self.key,
            allocation.key,
            desc.offset,
            desc.size,
            desc.usage,
        )?;
        Ok(Buffer {
            shared: Arc::clone(&self.shared),
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
    /// [`Error::Lost`] once the context is lost.
    pub fn create_timeline(&self, initial: u64) -> Result<Timeline, Error> {
        let key = self
            .shared
            .model
            .lock()
            .create_timeline(self.key, initial)?;
        Ok(Timeline {
            shared: Arc::clone(&self.shared),
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
        let slot = self.shared.info.queue_slot(family, index)?;
        Ok(Queue {
            shared: Arc::clone(&self.shared),
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
            shared: Arc::clone(&self.shared),
            context: self.key,
            commands: Vec::new(),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.shared.model.lock().destroy_context(self.key);
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
    shared: Arc<Shared>,
    context: ContextKey,
    key: AllocationKey,
    size: u64,
    memory_type: u32,
}

impl Allocation {
    /// Its size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Its memory type.
    #[must_use]
    pub const fn memory_type(&self) -> u32 {
        self.memory_type
    }

    /// The memory the CPU reaches it through, and what of it `len` bytes at
    /// `offset` are.
    fn host(
        &self,
        offset: u64,
        len: usize,
    ) -> Result<(Arc<Memory>, core::ops::Range<usize>), Error> {
        let (memory, size) = self
            .shared
            .model
            .lock()
            .host_memory(self.context, self.key)?;
        let end = offset
            .checked_add(len as u64)
            .filter(|&end| end <= size)
            .ok_or(Error::OutOfRange)?;
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let end = usize::try_from(end).map_err(|_| Error::OutOfRange)?;
        Ok((memory, start..end))
    }

    /// Write `data` at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// [`Error::NotHostVisible`] for memory the CPU cannot reach, and
    /// [`Error::OutOfRange`] past its end.
    pub fn write(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        let (memory, range) = self.host(offset, data.len())?;
        let mut bytes = memory.lock();
        let target = bytes.get_mut(range).ok_or(Error::OutOfRange)?;
        target.copy_from_slice(data);
        Ok(())
    }

    /// Read `out.len()` bytes at `offset` from the CPU.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let (memory, range) = self.host(offset, out.len())?;
        let bytes = memory.lock();
        out.copy_from_slice(bytes.get(range).ok_or(Error::OutOfRange)?);
        Ok(())
    }

    /// A token another context of this device imports the memory through.
    /// The memory stays charged to this context.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn export(&self) -> Result<Export, Error> {
        let (memory, memory_type, size) =
            self.shared.model.lock().export(self.context, self.key)?;
        Ok(Export {
            shared: Arc::clone(&self.shared),
            memory,
            memory_type,
            size,
        })
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.shared.model.lock().free(self.context, self.key);
    }
}

impl fmt::Debug for Allocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Allocation")
            .field("size", &self.size)
            .field("memory_type", &self.memory_type)
            .finish_non_exhaustive()
    }
}

/// An allocation's memory on its way to another context, which
/// [`Context::import`] takes it into.
pub struct Export {
    shared: Arc<Shared>,
    memory: Arc<Memory>,
    memory_type: u32,
    size: u64,
}

impl fmt::Debug for Export {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Export")
            .field("size", &self.size)
            .field("memory_type", &self.memory_type)
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
    shared: Arc<Shared>,
    context: ContextKey,
    key: BufferKey,
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
        self.shared
            .model
            .lock()
            .destroy_buffer(self.context, self.key);
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
    shared: Arc<Shared>,
    context: ContextKey,
    key: TimelineKey,
}

impl Timeline {
    /// Where it stands.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn value(&self) -> Result<u64, Error> {
        self.shared
            .model
            .lock()
            .timeline_value(self.context, self.key)
    }

    /// Set it to `value` from the CPU, running any work that was waiting for
    /// it.
    ///
    /// # Errors
    ///
    /// [`Error::TimelineOrder`] unless `value` is past where it stands and
    /// short of every signal queued work will make.
    pub fn signal(&self, value: u64) -> Result<(), Error> {
        let mut model = self.shared.model.lock();
        model.signal(self.context, self.key, value)?.notify_all();
        self.shared.drive(model);
        Ok(())
    }

    /// Wait until it reaches `value`.
    ///
    /// # Errors
    ///
    /// [`Error::TimedOut`] when `timeout` elapses first, and [`Error::Lost`]
    /// when the context is lost meanwhile.
    pub fn wait(&self, value: u64, timeout: Duration64) -> Result<(), Error> {
        self.shared.wait(self.context, self.key, value, timeout)
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        self.shared
            .model
            .lock()
            .destroy_timeline(self.context, self.key);
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
    shared: Arc<Shared>,
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
    /// Refused whole, changing nothing, for anything of another context
    /// ([`Error::Foreign`]), any object since released ([`Error::Stale`]),
    /// any range past what it names, any use a buffer was not made for, any
    /// signal that would not advance its timeline, a deadline of zero or past
    /// the device's longest, or more than a submission carries; the error
    /// names the refused command where there is one.
    pub fn submit(&self, submission: &Submission<'_>) -> Result<Fence, SubmitError> {
        let mut buffers: ArrayVec<&[Command], MAX_SUBMISSION_BUFFERS> = ArrayVec::new();
        for buffer in submission.command_buffers {
            owned(&buffer.shared, buffer.context, &self.shared, self.context)?;
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
        let mut model = self.shared.model.lock();
        let (timeline, value) = model.submit(self.context, &request)?;
        self.shared.drive(model);
        Ok(Fence {
            shared: Arc::clone(&self.shared),
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
            owned(
                &timeline.shared,
                timeline.context,
                &self.shared,
                self.context,
            )?;
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
    shared: Arc<Shared>,
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
        self.shared
            .wait(self.context, self.timeline, self.value, timeout)
    }

    /// Whether the submission has completed.
    ///
    /// # Errors
    ///
    /// [`Error::Lost`] once the context is lost.
    pub fn is_signalled(&self) -> Result<bool, Error> {
        let reached = self
            .shared
            .model
            .lock()
            .point(self.context, self.timeline, self.value)?;
        Ok(matches!(reached, Point::Reached))
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
    shared: Arc<Shared>,
    context: ContextKey,
    commands: Vec<Command>,
}

impl CommandBuffer {
    /// How many commands it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Drop every command, keeping the room they took.
    pub fn clear(&mut self) {
        self.commands.clear();
    }

    fn record(&mut self, command: Command) -> Result<(), Error> {
        if self.commands.len() >= MAX_SUBMISSION_COMMANDS {
            return Err(Error::TooMany);
        }
        self.commands
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.commands.push(command);
        Ok(())
    }

    fn named(&self, buffer: &Buffer) -> Result<BufferKey, Error> {
        owned(&buffer.shared, buffer.context, &self.shared, self.context)?;
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
        let regions = tairix_util::fallible::collected(regions.len(), regions.iter().copied())
            .ok_or(Error::OutOfMemory)?;
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
    /// at `offset` when the command runs.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a buffer of another context, and
    /// [`Error::TooMany`] past [`MAX_UPDATE_BYTES`].
    pub fn update_buffer(&mut self, dst: &Buffer, offset: u64, data: &[u8]) -> Result<(), Error> {
        let dst = self.named(dst)?;
        if data.len() > MAX_UPDATE_BYTES {
            return Err(Error::TooMany);
        }
        let data = tairix_util::fallible::collected(data.len(), data.iter().copied())
            .ok_or(Error::OutOfMemory)?;
        self.record(Command::Update {
            dst,
            offset,
            data: Arc::new(data),
        })
    }
}

impl fmt::Debug for CommandBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandBuffer")
            .field("commands", &self.commands.len())
            .finish_non_exhaustive()
    }
}

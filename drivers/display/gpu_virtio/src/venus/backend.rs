//! The Venus backend (`plans/GPU.md` GP5.3): a [`Hardware`] device over the
//! host's renderer, reached through the desk.
//!
//! Each context is a virtio-gpu context of its own holding an instance, the
//! host's physical device, a device and one queue for each the device states,
//! each queue bound to a ring of its own. Beside them it holds shared memory
//! its commands travel through and its replies come back in, and a page of
//! feedback memory each job's last command writes its number into: a fence
//! the renderer retired proves nothing, since it retires the fences of a
//! worker that died, so a job is done only once its number is read back.
//!
//! A worker making a context, memory, an image, a view, a texel buffer or a
//! sampler waits for each answer, and asks nothing that names an object
//! before the object's making has answered: a command naming one the host
//! could not make is fatal to the context. A job
//! is one command buffer, each command after a barrier making everything
//! before it visible to it, travelling in parts through the shared memory's
//! job area in the order its context's jobs came; the thread answering the
//! desk lets each part's span go and finishes each job.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::AtomicU64;

use tairix_abi::driver::MemoryWindow;
use tairix_abi::time::MonotonicClock;
use tairix_abi::{DriverError, PAGE_SIZE};
use tairix_collections::RangeMap;
use tairix_gpu::{
    BufferViewDesc, Commands, Counts, DescriptorType, Device, Direction, DrawCount, Error,
    Features, Format, Hardware, ImageDesc, ImageViewDesc, Job, JobCommand, LayoutBinding, Loss,
    MemoryProperties, MemoryRequirements, PipelineStage, RenderStages, SampleType, SamplerDesc,
    SamplerType, SetResource, Spawn, INDEXED_DRAW_RECORD_BYTES, MAX_COPY_REGIONS,
    MAX_DISPATCH_SETS, MAX_UPDATE_BYTES,
};
use tairix_inline::ArrayVec;
use tairix_rt::sync::{Condvar, Mutex};
use tairix_shaders::indirect::{counts, records};
use tairix_spirv::{Admitted, FoldError, GuardError, Guarded, Module, Stages};
use tairix_sync::claims;
use tairix_virtio::{Part, SharedMemory};

use super::desk::{Answer, Desk, Order, Payload};
use super::encode::{
    CopyRegion, DescriptorWrite, Enabled, Encoder, GraphicsInfo, LayoutEntry, StreamSpan, Written,
};
use super::info::{
    feature_chain, host_format, narrow, states_format_features, HostDevice, Statement,
    CHECKED_RECORDS,
};
use super::reply::{HostExtensions, HostFormatFeatures, Reader, MAX_QUEUE_FAMILIES};
use super::{
    api_version, descriptor, queue, stage, Command, VkResult, BIND_POINT_COMPUTE, BUFFER_USAGE_ALL,
    MAX_QUEUE_RINGS, ONE_TIME_SUBMIT, RESET_COMMAND_BUFFER,
};
use crate::three_d::{Blob, Context};
use crate::wire::BlobSpec;

mod images;
mod render;

use images::{VenusImage, VenusSampler, VenusTexels, VenusView};

/// A context's shared memory: its reply area, the area its waited
/// exchanges travel through, then the area its jobs travel through.
const SHARED_BYTES: usize = 256 << 10;
const REPLY_LEN: usize = 16 << 10;
const EXCHANGE_AT: usize = REPLY_LEN;
const EXCHANGE_LEN: usize = 16 << 10;
const JOBS_AT: usize = EXCHANGE_AT + EXCHANGE_LEN;

/// The longest part of a job's stream: as much of the job area as leaves
/// room for another part beside it.
const PART_MAX: usize = 96 << 10;

/// The bytes the longest command a job carries takes in its stream at the
/// seam's bounds: a copy of the most regions between a buffer and an image
/// or between images, or an update with its words. Every other is shorter.
const LONGEST_COMMAND: u64 = {
    let regions = MAX_COPY_REGIONS as u64 * stream::IMAGE_REGION;
    let update = MAX_UPDATE_BYTES as u64;
    stream::COMMAND + if regions > update { regions } else { update }
};

// A part holds the longest command whole, beside room for another part.
const _: () = assert!(LONGEST_COMMAND <= PART_MAX as u64 && 2 * PART_MAX <= SHARED_BYTES - JOBS_AT);

/// The feedback memory: a sequence word for each slot, slot 0 the waited
/// zero-fill's and every other one held by a job from its queueing until
/// its end is read, then a fault word for each, then each slot's indirect
/// counts as the device checked them, then, on a device checking indirect
/// draws' records, each slot's records as it checked them.
const FEEDBACK_SLOTS: u32 = 64;
const SLOT_WORDS_BYTES: u64 = FEEDBACK_SLOTS as u64 * 4;
const FAULTS_AT: u64 = SLOT_WORDS_BYTES;
const COUNTS_AT: u64 = FAULTS_AT + SLOT_WORDS_BYTES;
const COUNTS_SLOT_BYTES: u64 = counts::SLOT_WORDS as u64 * 4;
const COUNTS_BYTES: u64 = FEEDBACK_SLOTS as u64 * COUNTS_SLOT_BYTES;
const RECORDS_AT: u64 = COUNTS_AT + COUNTS_BYTES;
const RECORDS_SLOT_BYTES: u64 = CHECKED_RECORDS as u64 * INDEXED_DRAW_RECORD_BYTES as u64;
const RECORDS_BYTES: u64 = FEEDBACK_SLOTS as u64 * RECORDS_SLOT_BYTES;
const UTILITY_SLOT: u32 = 0;

// Each part, and each slot's records, starts where any storage binding's
// offset may.
const _: () = assert!(
    FAULTS_AT.is_multiple_of(256)
        && COUNTS_AT.is_multiple_of(256)
        && RECORDS_AT.is_multiple_of(256)
        && RECORDS_SLOT_BYTES.is_multiple_of(256)
);

/// The feedback memory's bytes on a device checking indirect draws' records
/// or not.
const fn feedback_bytes(checks_records: bool) -> u64 {
    if checks_records {
        RECORDS_AT + RECORDS_BYTES
    } else {
        RECORDS_AT
    }
}

/// The jobs the device's one engine runs at once.
pub(crate) const ENGINE_DEPTH: usize = 16;

/// Bounds on the bytes of a job's stream, which the encoder's tests hold its
/// worst cases to.
mod stream {
    /// The job's own: its command buffer allocated and begun, then ended,
    /// its number written, and submitted.
    pub(super) const JOB: u64 = 512;
    /// A command with the barrier before it, its regions or words aside.
    pub(super) const COMMAND: u64 = 160;
    /// A copy's region.
    pub(super) const REGION: u64 = 24;
    /// A copy's region between a buffer and an image, or between images.
    pub(super) const IMAGE_REGION: u64 = 72;
    /// A clear's range, and the colour or depth and stencil it writes.
    pub(super) const RANGE: u64 = 24;
    /// A dispatch's binds and itself, its sets and push constants aside.
    pub(super) const DISPATCH: u64 = 160;
    /// A set a dispatch binds.
    pub(super) const HANDLE: u64 = 8;
    /// A word of push constants.
    pub(super) const WORD: u64 = 4;
    /// An indirect dispatch's counts checked before it.
    pub(super) const CHECK: u64 = 640;
    /// A rendering instance begun and ended, its attachments aside.
    pub(super) const PASS: u64 = 256;
    /// An attachment a rendering instance names.
    pub(super) const ATTACHMENT: u64 = 112;
    /// A draw's binds and itself, its sets, push constants and vertex
    /// buffers aside.
    pub(super) const DRAW: u64 = 512;
    /// A vertex buffer a draw binds.
    pub(super) const VERTEX_BUFFER: u64 = 24;
    /// An indirect draw's records checked before the rendering instance
    /// drawing them, which they may begin, their copy's regions aside.
    pub(super) const RECORDS: u64 = 640;
}

// Every job running holds a feedback slot of its own beside the zero-fill's,
// and the slots held are one word of claims.
const _: () = assert!(ENGINE_DEPTH < FEEDBACK_SLOTS as usize && FEEDBACK_SLOTS <= u64::BITS);

/// `VK_WHOLE_SIZE`: to the end of the buffer.
const WHOLE_SIZE: u64 = u64::MAX;

/// What a reply that was never written reads as: no command's.
const UNWRITTEN: [u8; 8] = [0xff; 8];

/// The ids of a context's first objects, made with it: among them the
/// indirect counts' check, its pipeline and the set binding the feedback
/// buffer.
const INSTANCE: u64 = 1;
const PHYSICAL: u64 = 2;
const DEVICE: u64 = 3;
const POOL: u64 = 4;
const UTILITY: u64 = 5;
const FEEDBACK: u64 = 6;
const FEEDBACK_MEMORY: u64 = 7;
/// A pinned check a context runs on an indirect command's arguments where
/// they lie in its feedback memory: the ids of what it is made of, and its
/// set's bindings, each the span of the feedback memory it reaches.
struct Check {
    module: u64,
    ids: PipelineIds,
    set_layout: u64,
    pool: u64,
    set: u64,
    bindings: [(u32, u64, u64); 2],
    push_words: usize,
}

const COUNTS_CHECK: Check = Check {
    module: 8,
    ids: PipelineIds {
        layout: 9,
        pipeline: 10,
    },
    set_layout: 11,
    pool: 12,
    set: 13,
    bindings: [
        (counts::COUNTS, COUNTS_AT, COUNTS_BYTES),
        (counts::FAULTS, FAULTS_AT, SLOT_WORDS_BYTES),
    ],
    push_words: counts::WORDS,
};
const RECORDS_CHECK: Check = Check {
    module: 14,
    ids: PipelineIds {
        layout: 15,
        pipeline: 16,
    },
    set_layout: 17,
    pool: 18,
    set: 19,
    bindings: [
        (records::RECORDS, RECORDS_AT, RECORDS_BYTES),
        (records::FAULTS, FAULTS_AT, SLOT_WORDS_BYTES),
    ],
    push_words: records::WORDS,
};
const FIRST_QUEUE: u64 = 20;

/// The most extensions the driver reads of a physical device.
const MAX_EXTENSIONS: u32 = 1024;

/// The bytes one extension's reply takes: its name's array size and name,
/// and its version.
const EXTENSION_REPLY_BYTES: usize = 8 + super::EXTENSION_NAME_LEN + 4;

/// A worker's wait for one answer.
#[derive(Default)]
pub(crate) struct Waiter {
    answer: Mutex<Option<Answer>>,
    came: Condvar,
}

impl Waiter {
    /// Hand it `answer`, unless it holds one: an exchange its context's loss
    /// released keeps that refusal, and one answered keeps its answer.
    fn give(&self, answer: Answer) {
        let mut held = self.answer.lock();
        if held.is_none() {
            *held = Some(answer);
        }
        drop(held);
        self.came.notify_all();
    }

    fn wait(&self) -> Answer {
        let mut answer = self.answer.lock();
        loop {
            if let Some(answered) = answer.take() {
                return answered;
            }
            answer = self.came.wait(answer);
        }
    }
}

/// What a desk answer is for.
pub(crate) enum Then {
    /// A worker waits for it.
    Wake(Arc<Waiter>),
    /// A part of `context`'s stream was decoded: its span is free.
    Part(Arc<VenusContext>, Range<u64>),
    /// A job's last part was answered: its span is free, and the job is done
    /// once its feedback reads back.
    Finish(Arc<VenusContext>, Range<u64>, Finishing),
    /// A context ended, its submissions all answered.
    Ended,
    /// A step of letting objects go, the next taken once it is answered.
    Release(Release),
}

/// A job whose last part is with the device.
pub(crate) struct Finishing {
    job: Job<VenusDevice>,
    seq: u32,
    slot: u32,
    commands: u64,
}

/// What the device's threads share.
pub(crate) struct Core {
    desk: Arc<Desk<Then>>,
    shared: Arc<Mutex<SharedMemory>>,
    statement: Statement,
    /// The host's index of the memory type feedback memory is made in.
    feedback_type: u32,
    /// The checks an indirect dispatch's counts are held to and, on a device
    /// stating no first instance, an indirect draw's records, each folded and
    /// guarded as the host is given every module.
    counts: Guarded,
    records: Option<Guarded>,
}

/// A blob of host memory, mapped into the driver.
struct Mapped {
    blob: Blob,
    part: Part,
}

impl Mapped {
    fn window(&self) -> Result<&MemoryWindow, DriverError> {
        self.part.window().ok_or(DriverError::DeviceFault)
    }
}

/// A Venus context, as a model context is one on the device.
pub(crate) struct VenusContext {
    core: Arc<Core>,
    context: Context,
    /// The resource id of the shared memory, which streams name.
    resource: u32,
    queues: ArrayVec<u64, { MAX_QUEUE_RINGS as usize }>,
    shared_memory: Mutex<Option<Mapped>>,
    feedback_memory: Mutex<Option<Mapped>>,
    /// The feedback slots held, a bit each: jobs finish in any order across
    /// queues, so none may take a slot another still holds.
    slots: AtomicU64,
    /// Held across a waited exchange, which reads the one reply area.
    exchange: Mutex<()>,
    /// Held across a zero-fill, which alone writes and reads its slot.
    utility: Mutex<()>,
    state: Mutex<ContextState>,
}

struct ContextState {
    next_id: u64,
    /// The job area's spans with the device.
    spans: RangeMap<u64, ()>,
    free_commands: Vec<u64>,
    /// Work waiting for room in the job area, in the order it came.
    pending: VecDeque<Item>,
    in_flight: usize,
    next_seq: u32,
    lost: bool,
    ended: bool,
    /// The waited exchange's wait, which the context's loss ends: an exchange
    /// fenced on a queue waits behind that queue's work, which may never end.
    waiting: Option<Arc<Waiter>>,
}

/// Whether a context's queue takes one more item.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Admits {
    /// Room is reserved for it.
    Room,
    /// The context is lost or ended: nothing more of it is sent.
    Gone,
    /// No room could be had.
    Full,
}

impl ContextState {
    /// Whether the queue takes one more item, its room reserved where it
    /// does. Every push is decided here, under the lock the push is made
    /// under, so nothing joins a context lost while the item was being made.
    fn admits(&mut self) -> Admits {
        if self.lost || self.ended {
            Admits::Gone
        } else if self.pending.try_reserve(1).is_err() {
            Admits::Full
        } else {
            Admits::Room
        }
    }
}

/// Work waiting for room in the job area.
enum Item {
    Job {
        job: Job<VenusDevice>,
        /// The part sent next.
        next: Vec<u8>,
        /// The parts after it, in order.
        rest: VecDeque<Vec<u8>>,
        /// The ring its last part is submitted on.
        ring: u8,
        seq: u32,
        slot: u32,
        commands: u64,
    },
    /// Commands that answer nothing, letting host objects go.
    Commands(Vec<u8>),
}

/// Memory made in a context: one buffer over all of it.
pub(crate) struct VenusMemory {
    context: Arc<VenusContext>,
    buffer: u64,
    memory: u64,
    /// Where it is host-visible: its blob, mapped into the driver.
    host: Option<Mutex<Mapped>>,
}

impl core::fmt::Debug for VenusMemory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusMemory")
            .field("memory", &self.memory)
            .finish_non_exhaustive()
    }
}

/// A bind set made in a context: the descriptor pool holding it alone, and
/// the set.
pub(crate) struct VenusSet {
    context: Arc<VenusContext>,
    pool: u64,
    set: u64,
}

impl core::fmt::Debug for VenusSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusSet")
            .field("set", &self.set)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusSet {
    fn drop(&mut self) {
        let mut release = Encoder::default();
        match release.destroy(Command::DestroyDescriptorPool, DEVICE, self.pool) {
            Ok(()) => queue_commands(&self.context, release.as_bytes()),
            Err(_) => lose(&self.context),
        }
    }
}

/// A compute or render pipeline made in a context: itself, the layout its
/// sets and push constants are bound with, and the stages that see its push
/// constants.
pub(crate) struct VenusPipeline {
    context: Arc<VenusContext>,
    pipeline: u64,
    layout: u64,
    push_stages: u32,
}

impl core::fmt::Debug for VenusPipeline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VenusPipeline")
            .field("pipeline", &self.pipeline)
            .finish_non_exhaustive()
    }
}

impl Drop for VenusPipeline {
    fn drop(&mut self) {
        let mut release = Encoder::default();
        let encoded = release
            .destroy(Command::DestroyPipeline, DEVICE, self.pipeline)
            .and_then(|()| release.destroy(Command::DestroyPipelineLayout, DEVICE, self.layout));
        match encoded {
            Ok(()) => queue_commands(&self.context, release.as_bytes()),
            Err(_) => lose(&self.context),
        }
    }
}

/// The device a Venus host serves.
pub(crate) struct VenusDevice(Arc<Core>);

fn try_arc<T>(value: T) -> Result<Arc<T>, DriverError> {
    Arc::try_new(value).map_err(|_| DriverError::OutOfMemory)
}

/// The seam's error for the device's `refused`.
fn device_error(refused: DriverError) -> Error {
    match refused {
        DriverError::Unsupported | DriverError::NotFound => Error::Unsupported,
        DriverError::DeviceFault | DriverError::DeviceOffline => Error::Lost(Loss::Fault),
        // A value or a stream past what the protocol carries.
        DriverError::OutOfRange | DriverError::LengthOutOfRange => Error::TooLarge,
        // Room the host, the driver or the device path could not give.
        _ => Error::OutOfMemory,
    }
}

/// The part an order lent, which a refusal to queue it hands back.
fn lent(order: Order) -> Option<Part> {
    match order {
        Order::MapBlob(_, part) | Order::UnmapBlob(_, part) => Some(part),
        _ => None,
    }
}

/// Ask `order` of the device and wait for the answer.
fn ask(desk: &Desk<Then>, order: Order) -> Answer {
    let Ok(waiter) = try_arc(Waiter::default()) else {
        return Answer::Refused(DriverError::OutOfMemory, lent(order));
    };
    match desk.order(order, Then::Wake(Arc::clone(&waiter))) {
        Ok(()) => waiter.wait(),
        Err((order, _)) => Answer::Refused(DriverError::OutOfMemory, lent(order)),
    }
}

/// What an answer lending no part says.
fn done(answer: &Answer) -> Result<(), DriverError> {
    match *answer {
        Answer::Done => Ok(()),
        Answer::Refused(refused, _) => Err(refused),
        _ => Err(DriverError::DeviceFault),
    }
}

fn refusal(answer: &Answer) -> DriverError {
    done(answer).err().unwrap_or(DriverError::DeviceFault)
}

fn succeeded(result: VkResult) -> Result<(), DriverError> {
    match result {
        VkResult::Success => Ok(()),
        // Host and device memory exhausted.
        VkResult::Failed(-1 | -2) => Err(DriverError::OutOfMemory),
        _ => Err(DriverError::DeviceFault),
    }
}

/// The few commands an order carries in itself.
fn payload(
    encode: impl FnOnce(&mut Encoder) -> Result<(), DriverError>,
) -> Result<Payload, DriverError> {
    let mut encoder = Encoder::default();
    encode(&mut encoder)?;
    Payload::try_from(encoder.as_bytes()).map_err(|_| DriverError::LengthOutOfRange)
}

fn owned(bytes: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    owned.extend_from_slice(bytes);
    Ok(owned)
}

/// What a waited bring-up made, let go in reverse where it fails.
struct Making<'d> {
    desk: &'d Desk<Then>,
    shared: &'d Mutex<SharedMemory>,
    context: Option<Context>,
    mapped: ArrayVec<Mapped, 2>,
    /// A blob made that would not end.
    stranded: bool,
}

impl<'d> Making<'d> {
    const fn new(desk: &'d Desk<Then>, shared: &'d Mutex<SharedMemory>) -> Self {
        Self {
            desk,
            shared,
            context: None,
            mapped: ArrayVec::new(),
            stranded: false,
        }
    }

    fn context(&mut self) -> Result<Context, DriverError> {
        match ask(self.desk, Order::MakeContext) {
            Answer::Context(context) => {
                self.context = Some(context);
                Ok(context)
            }
            answer => Err(refusal(&answer)),
        }
    }

    /// A blob of `spec` in `context`, mapped into the driver.
    fn mapped(&mut self, context: Context, spec: BlobSpec) -> Result<&Mapped, DriverError> {
        let blob = match ask(self.desk, Order::MakeBlob(context, spec)) {
            Answer::Blob(blob) => blob,
            answer => return Err(refusal(&answer)),
        };
        // Let go before the desk is asked again: the display loop maps parts
        // through the same memory.
        let reserved = self.shared.lock().reserve(spec.size);
        let part = match reserved {
            Ok(part) => part,
            Err(refused) => {
                self.end_blob(blob);
                return Err(refused.into());
            }
        };
        match ask(self.desk, Order::MapBlob(blob, part)) {
            Answer::Mapped(part) => {
                let windowed = part.window().is_some();
                // Kept even windowless, for its teardown to unmap it again.
                self.mapped
                    .try_push(Mapped { blob, part })
                    .map_err(|_| DriverError::NoSpace)?;
                if !windowed {
                    return Err(DriverError::OutOfMemory);
                }
            }
            Answer::Refused(refused, part) => {
                // Lent back only where the device maps nothing into it.
                if let Some(part) = part {
                    let _ = self.shared.lock().release(part);
                }
                self.end_blob(blob);
                return Err(refused);
            }
            _ => return Err(DriverError::DeviceFault),
        }
        self.mapped.last().ok_or(DriverError::DeviceFault)
    }

    /// End `blob`, unmapped, remembering one that would not end.
    fn end_blob(&mut self, blob: Blob) {
        if done(&ask(self.desk, Order::EndBlob(blob))).is_err() {
            self.stranded = true;
        }
    }

    /// What was made, kept.
    fn keep(mut self) -> ArrayVec<Mapped, 2> {
        self.context = None;
        core::mem::take(&mut self.mapped)
    }
}

impl Making<'_> {
    /// Let what was made go, in reverse, answering whether every blob ended:
    /// memory a blob still names is not the context's to free.
    fn let_go(&mut self) -> bool {
        while let Some(mapped) = self.mapped.pop() {
            // A part comes back only where the device maps nothing into it.
            match ask(self.desk, Order::UnmapBlob(mapped.blob, mapped.part)) {
                Answer::Unmapped(part) | Answer::Refused(_, Some(part)) => {
                    let _ = self.shared.lock().release(part);
                    self.end_blob(mapped.blob);
                }
                _ => self.stranded = true,
            }
        }
        if let Some(context) = self.context.take() {
            let _ = done(&ask(self.desk, Order::EndContext(context)));
        }
        !self.stranded
    }
}

impl Drop for Making<'_> {
    fn drop(&mut self) {
        self.let_go();
    }
}

/// Where a context's waited exchanges travel: its virtio-gpu context, the
/// shared memory its replies come back in and that memory's resource, and
/// the region a larger exchange's blob is placed in.
#[derive(Copy, Clone)]
struct Channel<'a> {
    desk: &'a Desk<Then>,
    shared: &'a Mutex<SharedMemory>,
    context: Context,
    resource: u32,
    area: Area<'a>,
    /// The context it is, once one is made, whose loss ends its waits.
    owner: Option<&'a VenusContext>,
}

/// Memory an exchange copies its commands into and its replies out of.
#[derive(Copy, Clone)]
enum Area<'a> {
    /// Memory only this thread reaches yet.
    Owned(&'a Mapped),
    /// A published context's shared memory, which the thread answering the
    /// desk copies its jobs into too. It is held only while copying, never
    /// across a wait for the desk, whose answers that thread delivers.
    Shared(&'a Mutex<Option<Mapped>>),
}

impl Area<'_> {
    fn copy<R>(
        self,
        copy: impl FnOnce(&MemoryWindow) -> Result<R, DriverError>,
    ) -> Result<R, DriverError> {
        match self {
            Self::Owned(mapped) => copy(mapped.window()?),
            Self::Shared(memory) => {
                let held = memory.lock();
                copy(held.as_ref().ok_or(DriverError::DeviceFault)?.window()?)
            }
        }
    }
}

/// A waited exchange: `commands` run on `ring`, answering the first
/// `reply_len` bytes of the replies they asked for. `first` points the
/// renderer at the reply area. Commands or replies too long for the shared
/// memory's areas travel through a blob of their own.
fn exchange(
    at: Channel<'_>,
    commands: &[u8],
    (ring, first, reply_len): (u8, bool, usize),
) -> Result<Vec<u8>, DriverError> {
    if commands.len() > EXCHANGE_LEN || reply_len > REPLY_LEN {
        return exchange_through_blob(at, commands, (ring, reply_len));
    }
    at.area.copy(|window| {
        window
            .write(0, &UNWRITTEN)
            .and_then(|()| window.write(EXCHANGE_AT, commands))
            .map_err(|_| DriverError::DeviceFault)
    })?;
    let span = StreamSpan {
        resource: at.resource,
        offset: EXCHANGE_AT as u64,
        size: commands.len() as u64,
    };
    let payload = payload(|encoder| {
        if first {
            encoder.set_reply_command_stream(reply_area(at.resource))?;
        }
        encoder.seek_reply_command_stream(0)?;
        encoder.execute_command_streams(&[span])
    })?;
    submit_and_read(at, (ring, payload), (at.area, 0, reply_len))
}

/// A context's reply area.
const fn reply_area(resource: u32) -> StreamSpan {
    StreamSpan {
        resource,
        offset: 0,
        size: REPLY_LEN as u64,
    }
}

/// [`exchange`] through a blob made for it: the replies first, then the
/// commands, which end by pointing the renderer back at the context's reply
/// area.
fn exchange_through_blob(
    at: Channel<'_>,
    commands: &[u8],
    (ring, reply_len): (u8, usize),
) -> Result<Vec<u8>, DriverError> {
    let mut back = Encoder::default();
    back.set_reply_command_stream(reply_area(at.resource))?;
    let reply_room = reply_len.next_multiple_of(4).max(UNWRITTEN.len());
    let stream_len = commands
        .len()
        .checked_add(back.as_bytes().len())
        .ok_or(DriverError::LengthOutOfRange)?;
    let size = reply_room
        .checked_add(stream_len)
        .and_then(|len| len.checked_next_multiple_of(PAGE_SIZE))
        .ok_or(DriverError::LengthOutOfRange)?;
    let mut making = Making::new(at.desk, at.shared);
    let spec = BlobSpec {
        blob_id: 0,
        size: size as u64,
        mappable: true,
        shareable: false,
    };
    let blob = making.mapped(at.context, spec)?;
    let resource = blob.blob.resource_id().ok_or(DriverError::DeviceFault)?;
    let area = Area::Owned(blob);
    area.copy(|window| {
        window
            .write(0, &UNWRITTEN)
            .and_then(|()| window.write(reply_room, commands))
            .and_then(|()| window.write(reply_room + commands.len(), back.as_bytes()))
            .map_err(|_| DriverError::DeviceFault)
    })?;
    let payload = payload(|encoder| {
        encoder.set_reply_command_stream(StreamSpan {
            resource,
            offset: 0,
            size: reply_room as u64,
        })?;
        encoder.seek_reply_command_stream(0)?;
        encoder.execute_command_streams(&[StreamSpan {
            resource,
            offset: reply_room as u64,
            size: stream_len as u64,
        }])
    })?;
    submit_and_read(at, (ring, payload), (area, 0, reply_len))
}

/// Submit `payload` on `ring` and wait for it, then read `len` bytes of
/// replies at `offset` of `area`. Every refusal but the device's comes
/// before anything is sent, so a caller may let go what it made before;
/// the device's leaves the context's state unknown.
fn submit_and_read(
    at: Channel<'_>,
    (ring, payload): (u8, Payload),
    (area, offset, len): (Area<'_>, usize, usize),
) -> Result<Vec<u8>, DriverError> {
    let mut replies = Vec::new();
    replies
        .try_reserve_exact(len)
        .map_err(|_| DriverError::OutOfMemory)?;
    replies.resize(len, 0);
    match at.owner {
        Some(context) => context.submit(ring, payload),
        None => done(&ask(
            at.desk,
            Order::Submit {
                context: at.context,
                ring,
                payload,
            },
        )),
    }?;
    area.copy(|window| {
        window
            .read(offset, &mut replies)
            .map_err(|_| DriverError::DeviceFault)
    })?;
    Ok(replies)
}

/// The shared memory a new context's streams and replies travel through.
const SHARED_SPEC: BlobSpec = BlobSpec {
    blob_id: 0,
    size: SHARED_BYTES as u64,
    mappable: true,
    shareable: false,
};

/// The device a Venus host states, read in a context made for the asking
/// and ended once read.
fn probe(desk: &Desk<Then>, shared: &Mutex<SharedMemory>) -> Result<Statement, DriverError> {
    let mut making = Making::new(desk, shared);
    let context = making.context()?;
    let mapped = making.mapped(context, SHARED_SPEC)?;
    let at = Channel {
        desk,
        shared,
        context,
        resource: mapped.blob.resource_id().ok_or(DriverError::DeviceFault)?,
        area: Area::Owned(mapped),
        owner: None,
    };
    let ask_one = |commands: &Encoder, first: bool, reply_len: usize| {
        exchange(at, commands.as_bytes(), (0, first, reply_len))
    };
    let mut encoder = Encoder::default();
    encoder.create_instance(INSTANCE)?;
    succeeded(
        Reader::new(&ask_one(&encoder, true, SHORT_REPLY)?)
            .made(Command::CreateInstance, INSTANCE)?,
    )?;
    encoder.clear();
    encoder.enumerate_physical_devices(INSTANCE, &[PHYSICAL])?;
    let (result, _, named) =
        Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?).physical_devices(1)?;
    if matches!(result, VkResult::Failed(_)) || named.first() != Some(&PHYSICAL) {
        return Err(DriverError::Unsupported);
    }
    encoder.clear();
    encoder.enumerate_device_extension_properties(PHYSICAL, 0)?;
    let (result, count, _) = Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?).extensions(0)?;
    succeeded(result)?;
    if count > MAX_EXTENSIONS {
        return Err(DriverError::Unsupported);
    }
    encoder.clear();
    encoder.enumerate_device_extension_properties(PHYSICAL, count)?;
    let reply_len = 64 + count as usize * EXTENSION_REPLY_BYTES;
    let (result, _, extensions) =
        Reader::new(&ask_one(&encoder, false, reply_len)?).extensions(count as usize)?;
    succeeded(result)?;
    let robustness = extensions.intersects(HostExtensions::ROBUSTNESS2);
    encoder.clear();
    encoder.get_physical_device_properties2(PHYSICAL, robustness)?;
    let families = u32::try_from(MAX_QUEUE_FAMILIES).map_err(|_| DriverError::LengthOutOfRange)?;
    encoder.get_physical_device_queue_family_properties2(PHYSICAL, families)?;
    encoder.get_physical_device_memory_properties2(PHYSICAL)?;
    let replies = ask_one(&encoder, false, REPLY_LEN)?;
    let mut reader = Reader::new(&replies);
    let properties = reader.properties(robustness)?;
    let (_, families) = reader.queue_families(MAX_QUEUE_FAMILIES)?;
    let memory = reader.memory_properties()?;
    let chain = feature_chain(properties.api_version, extensions);
    encoder.clear();
    encoder.get_physical_device_features2(PHYSICAL, &chain)?;
    let features = Reader::new(&ask_one(&encoder, false, 512)?).features(&chain)?;
    encoder.clear();
    let stated = states_format_features(properties.api_version, extensions);
    for format in Format::ALL {
        encoder.get_physical_device_format_properties2(PHYSICAL, host_format(format), stated)?;
    }
    let replies = ask_one(&encoder, false, REPLY_LEN)?;
    let mut reader = Reader::new(&replies);
    let mut formats = [HostFormatFeatures::default(); Format::ALL.len()];
    for offered in &mut formats {
        *offered = reader.format_properties(stated)?;
    }
    // Only a family running graphics work renders.
    let mut multisampled = [None; Format::ALL.len()];
    if families
        .iter()
        .any(|family| family.flags & queue::GRAPHICS != 0)
    {
        encoder.clear();
        let lists = properties.api_version >= api_version(2);
        let mut asked = [false; Format::ALL.len()];
        for ((&format, &offered), asked) in Format::ALL.iter().zip(&formats).zip(&mut asked) {
            if let Some(query) = images::four_samples(format, offered, lists)? {
                encoder.get_physical_device_image_format_properties2(PHYSICAL, &query)?;
                *asked = true;
            }
        }
        if asked.contains(&true) {
            let replies = ask_one(&encoder, false, REPLY_LEN)?;
            let mut reader = Reader::new(&replies);
            for (answer, asked) in multisampled.iter_mut().zip(asked) {
                if asked {
                    *answer = reader.image_format_properties()?;
                }
            }
        }
    }
    narrow(&HostDevice {
        properties: &properties,
        extensions,
        features: &features,
        chain: &chain,
        memory: &memory,
        families: &families,
        formats: &formats,
        multisampled: &multisampled,
    })
}

/// The host's memory type feedback memory is made in: host-visible and
/// coherent, cached where one is.
fn feedback_type(statement: &Statement) -> Result<u32, DriverError> {
    let visible = MemoryProperties::HOST_VISIBLE.with(MemoryProperties::HOST_COHERENT);
    let kinds = || {
        statement
            .info
            .memory_types
            .iter()
            .zip(&statement.memory_types)
            .filter(|(kind, _)| kind.properties.contains(visible))
    };
    kinds()
        .find(|(kind, _)| kind.properties.contains(MemoryProperties::HOST_CACHED))
        .or_else(|| kinds().next())
        .map(|(_, &host)| host)
        .ok_or(DriverError::Unsupported)
}

/// The device a Venus host serves through `desk`, whose answers a thread
/// `threads` starts hands back; its blobs placed in `shared`, its deadlines
/// read from `clock`.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for a host the backend cannot serve or a
/// thread that could not be started, else the device's refusal while it was
/// read.
pub(crate) fn device(
    desk: Arc<Desk<Then>>,
    shared: Arc<Mutex<SharedMemory>>,
    threads: &dyn Spawn,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
) -> Result<Device, DriverError> {
    let core = bring_up(desk, shared, threads)?;
    let info = core.statement.info.clone();
    Device::hardware(VenusDevice(core), info, &[ENGINE_DEPTH], clock)
        .map_err(|_| DriverError::DeviceFault)
}

/// The host probed and its statement narrowed, the pinned check folded, and
/// the thread answering the desk started.
fn bring_up(
    desk: Arc<Desk<Then>>,
    shared: Arc<Mutex<SharedMemory>>,
    threads: &dyn Spawn,
) -> Result<Arc<Core>, DriverError> {
    let answering = Arc::clone(&desk);
    let body = Box::try_new(move || {
        while let Some((then, answer)) = answering.next_answer() {
            then.run(answer);
        }
    })
    .map_err(|_| DriverError::OutOfMemory)?;
    if !threads.spawn(body) {
        return Err(DriverError::Unsupported);
    }
    let statement = probe(&desk, &shared)?;
    let feedback_type = feedback_type(&statement)?;
    let records = if statement
        .info
        .features
        .contains(Features::DRAW_INDIRECT_FIRST_INSTANCE)
    {
        None
    } else {
        Some(pinned(records::SPIRV)?)
    };
    try_arc(Core {
        desk,
        shared,
        statement,
        feedback_type,
        counts: pinned(counts::SPIRV)?,
        records,
    })
}

/// The pinned module `spirv`, folded and guarded. The pinned set is admitted
/// at build time, so one refused here is a build the driver cannot trust.
fn pinned(spirv: &[u8]) -> Result<Guarded, DriverError> {
    let module = Module::decode(spirv)
        .ok()
        .and_then(|module| Admitted::new(module).ok())
        .ok_or(DriverError::DeviceFault)?;
    module
        .specialize(&[])
        .ok()
        .and_then(|specialized| module.fold(&specialized).ok())
        .and_then(|folded| folded.guarded().ok())
        .ok_or(DriverError::DeviceFault)
}

impl VenusContext {
    /// Run `payload` on `ring` and wait for the device's answer, or for the
    /// context's loss: a lost context sends nothing. A submission lends no
    /// part, so an answer coming after a loss's has nothing to give back.
    fn submit(&self, ring: u8, payload: Payload) -> Result<(), DriverError> {
        let waiter = try_arc(Waiter::default())?;
        let order = Order::Submit {
            context: self.context,
            ring,
            payload,
        };
        {
            // Ordered under the lock a loss is marked under, as a job's part
            // is, so no exchange reaches the desk once its context is lost.
            let mut state = self.state.lock();
            if state.lost {
                return Err(DriverError::DeviceOffline);
            }
            if self
                .core
                .desk
                .order(order, Then::Wake(Arc::clone(&waiter)))
                .is_err()
            {
                return Err(DriverError::OutOfMemory);
            }
            state.waiting = Some(Arc::clone(&waiter));
        }
        let answer = waiter.wait();
        self.state.lock().waiting = None;
        done(&answer)
    }

    /// A waited exchange in this context.
    fn exchange(
        &self,
        commands: &[u8],
        ring: u8,
        reply_len: usize,
    ) -> Result<Vec<u8>, DriverError> {
        // One exchange at a time reads the one reply area.
        let _turn = self.exchange.lock();
        let at = Channel {
            desk: &self.core.desk,
            shared: &self.core.shared,
            context: self.context,
            resource: self.resource,
            area: Area::Shared(&self.shared_memory),
            owner: Some(self),
        };
        exchange(at, commands, (ring, false, reply_len))
    }

    /// The first of `count` fresh ids in a row, unique among the context's
    /// objects.
    fn id_run(&self, count: usize) -> Result<u64, DriverError> {
        let mut state = self.state.lock();
        let first = state.next_id;
        state.next_id = first
            .checked_add(count as u64)
            .ok_or(DriverError::OutOfRange)?;
        Ok(first)
    }

    /// Fresh ids, unique among the context's objects.
    fn ids<const N: usize>(&self) -> [u64; N] {
        let mut state = self.state.lock();
        core::array::from_fn(|_| {
            let id = state.next_id;
            state.next_id += 1;
            id
        })
    }

    /// Write `word` into feedback slot `slot` from the CPU.
    fn mark(&self, slot: u32, word: u32) -> Result<(), DriverError> {
        let feedback = self.feedback_memory.lock();
        let window = feedback
            .as_ref()
            .ok_or(DriverError::DeviceFault)?
            .window()?;
        window
            .write(slot as usize * 4, &word.to_le_bytes())
            .map_err(|_| DriverError::DeviceFault)
    }

    /// Clear feedback slot `slot`'s fault from the CPU.
    fn clear_fault(&self, slot: u32) -> Result<(), DriverError> {
        let feedback = self.feedback_memory.lock();
        let window = feedback
            .as_ref()
            .ok_or(DriverError::DeviceFault)?
            .window()?;
        let at =
            usize::try_from(FAULTS_AT).map_err(|_| DriverError::OutOfRange)? + slot as usize * 4;
        window
            .write(at, &0u32.to_le_bytes())
            .map_err(|_| DriverError::DeviceFault)
    }

    /// Whether feedback slot `slot` reads `word`.
    fn reads(&self, slot: u32, word: u32) -> bool {
        self.read_feedback(slot as usize * 4) == Some(word)
    }

    /// Whether feedback slot `slot`'s fault is clear: an unreadable one is
    /// not.
    fn unfaulted(&self, slot: u32) -> bool {
        usize::try_from(FAULTS_AT)
            .ok()
            .and_then(|at| self.read_feedback(at + slot as usize * 4))
            == Some(0)
    }

    /// The feedback word at `at`, where it can be read.
    fn read_feedback(&self, at: usize) -> Option<u32> {
        let feedback = self.feedback_memory.lock();
        let mut read = [0u8; 4];
        feedback
            .as_ref()
            .and_then(|mapped| mapped.part.window())
            .filter(|window| window.read(at, &mut read).is_ok())
            .map(|_| u32::from_le_bytes(read))
    }

    /// The next job number.
    fn next_seq(&self) -> u32 {
        let mut state = self.state.lock();
        let seq = state.next_seq;
        state.next_seq = seq.wrapping_add(1);
        seq
    }

    /// A feedback slot nothing holds, held until [`Self::release_slot`].
    fn claim_slot(&self) -> Option<u32> {
        let map = core::slice::from_ref(&self.slots);
        let claimed = claims::claim_lowest(map, FEEDBACK_SLOTS as usize)?;
        u32::try_from(claimed)
            .inspect_err(|_| claims::release(map, claimed))
            .ok()
    }

    /// Give back `slot`, its job's end read.
    fn release_slot(&self, slot: u32) {
        claims::release(core::slice::from_ref(&self.slots), slot as usize);
    }
}

/// The ring a queue's work is fenced on: ring 0 is the context's own.
fn ring(queue: usize) -> Result<u8, DriverError> {
    u8::try_from(queue + 1).map_err(|_| DriverError::OutOfRange)
}

/// Make a context on the device: its virtio-gpu context, shared memory,
/// instance, device and queues, command pool, feedback memory, and the check
/// an indirect dispatch's counts are held to.
fn make_context(core: &Arc<Core>) -> Result<VenusContext, DriverError> {
    let mut making = Making::new(&core.desk, &core.shared);
    let context = making.context()?;
    let mapped = making.mapped(context, SHARED_SPEC)?;
    let resource = mapped.blob.resource_id().ok_or(DriverError::DeviceFault)?;
    let at = Channel {
        desk: &core.desk,
        shared: &core.shared,
        context,
        resource,
        area: Area::Owned(mapped),
        owner: None,
    };
    let queues = make_device(core, at)?;
    let ask = |commands: &[u8], reply_len| exchange(at, commands, (0, false, reply_len));
    let size = make_feedback(core, &ask)?;
    make_check(&ask, &COUNTS_CHECK, core.counts.admitted().module().bytes())?;
    if let Some(records) = &core.records {
        make_check(&ask, &RECORDS_CHECK, records.admitted().module().bytes())?;
    }
    let spec = BlobSpec {
        blob_id: FEEDBACK_MEMORY,
        size,
        mappable: true,
        shareable: false,
    };
    making.mapped(context, spec)?;
    let mut mapped = making.keep().into_iter();
    let next_id = FIRST_QUEUE + queues.len() as u64;
    Ok(VenusContext {
        core: Arc::clone(core),
        context,
        resource,
        queues,
        shared_memory: Mutex::new(mapped.next()),
        feedback_memory: Mutex::new(mapped.next()),
        slots: AtomicU64::new(1 << UTILITY_SLOT),
        exchange: Mutex::new(()),
        utility: Mutex::new(()),
        state: Mutex::new(ContextState {
            next_id,
            spans: RangeMap::new(),
            free_commands: Vec::new(),
            pending: VecDeque::new(),
            in_flight: 0,
            next_seq: 1,
            lost: false,
            ended: false,
            waiting: None,
        }),
    })
}

/// A context's instance, the host's physical device, the device made as the
/// statement says with its queues, the command pool and the utility command
/// buffer, made through `at`, whose first exchange points the renderer at the
/// context's reply area: the queues.
fn make_device(
    core: &Core,
    at: Channel<'_>,
) -> Result<ArrayVec<u64, { MAX_QUEUE_RINGS as usize }>, DriverError> {
    let ask_one = |commands: &Encoder, first: bool, reply_len: usize| {
        exchange(at, commands.as_bytes(), (0, first, reply_len))
    };
    let family = core.statement.family;
    let queue_count = core.statement.info.queue_families[0].queues;
    let mut encoder = Encoder::default();
    encoder.create_instance(INSTANCE)?;
    succeeded(
        Reader::new(&ask_one(&encoder, true, SHORT_REPLY)?)
            .made(Command::CreateInstance, INSTANCE)?,
    )?;
    encoder.clear();
    encoder.enumerate_physical_devices(INSTANCE, &[PHYSICAL])?;
    let (result, _, named) =
        Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?).physical_devices(1)?;
    if matches!(result, VkResult::Failed(_)) || named.first() != Some(&PHYSICAL) {
        return Err(DriverError::Unsupported);
    }
    encoder.clear();
    let enables = &core.statement.enables;
    encoder.create_device(
        PHYSICAL,
        DEVICE,
        (family, queue_count),
        &Enabled {
            extensions: &enables.extensions,
            core: &enables.core,
            chain: &enables.chained(),
        },
    )?;
    succeeded(
        Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?).made(Command::CreateDevice, DEVICE)?,
    )?;
    encoder.clear();
    let mut queues = ArrayVec::new();
    for index in 0..queue_count {
        let queue = FIRST_QUEUE + u64::from(index);
        encoder.get_device_queue2(DEVICE, (family, index), index + 1, queue)?;
        queues
            .try_push(queue)
            .map_err(|_| DriverError::OutOfRange)?;
    }
    encoder.create_command_pool(DEVICE, POOL, (family, RESET_COMMAND_BUFFER))?;
    succeeded(
        Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?)
            .made(Command::CreateCommandPool, POOL)?,
    )?;
    encoder.clear();
    encoder.allocate_command_buffers(DEVICE, POOL, &[UTILITY])?;
    succeeded(
        Reader::new(&ask_one(&encoder, false, SHORT_REPLY)?)
            .allocated(Command::AllocateCommandBuffers, &[UTILITY])?,
    )?;
    Ok(queues)
}

/// The feedback buffer and the memory it is bound to: the memory's size.
fn make_feedback(core: &Core, ask: Ask<'_>) -> Result<u64, DriverError> {
    let bytes = feedback_bytes(core.records.is_some());
    let mut encoder = Encoder::default();
    encoder.create_buffer(DEVICE, FEEDBACK, (bytes, BUFFER_USAGE_ALL))?;
    succeeded(
        Reader::new(&ask(encoder.as_bytes(), SHORT_REPLY)?)
            .made(Command::CreateBuffer, FEEDBACK)?,
    )?;
    encoder.clear();
    encoder.get_buffer_memory_requirements2(DEVICE, FEEDBACK)?;
    let required = Reader::new(&ask(encoder.as_bytes(), SHORT_REPLY)?)
        .memory_requirements(Command::GetBufferMemoryRequirements2)?;
    if required.type_bits & (1 << core.feedback_type) == 0 {
        return Err(DriverError::Unsupported);
    }
    let size = required.size.max(bytes).next_multiple_of(PAGE_SIZE as u64);
    encoder.clear();
    encoder.allocate_memory(DEVICE, FEEDBACK_MEMORY, (size, core.feedback_type))?;
    let allocated = Reader::new(&ask(encoder.as_bytes(), SHORT_REPLY)?)
        .made(Command::AllocateMemory, FEEDBACK_MEMORY)?;
    succeeded(allocated)?;
    encoder.clear();
    encoder.bind_buffer_memory2(DEVICE, FEEDBACK, (FEEDBACK_MEMORY, 0))?;
    succeeded(
        Reader::new(&ask(encoder.as_bytes(), SHORT_REPLY)?)
            .result_of(Command::BindBufferMemory2)?,
    )?;
    Ok(size)
}

/// The ids of a pipeline's layout and of itself.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct PipelineIds {
    layout: u64,
    pipeline: u64,
}

/// What a pipeline runs past its layout: a compute entry point of its one
/// module, or a render pipeline's stages, each of its modules, and state.
#[derive(Copy, Clone, Debug)]
enum Shape<'a> {
    Compute(&'a str),
    Render(&'a GraphicsInfo<'a>),
}

/// A waited exchange of a context: commands, and the bytes of the replies
/// they ask for.
type Ask<'a> = &'a dyn Fn(&[u8], usize) -> Result<Vec<u8>, DriverError>;

/// The bytes a reply of a command making one object takes.
const MADE_REPLY: usize = 24;

/// Room for the reply of a command answering a result and a few words.
const SHORT_REPLY: usize = 64;

/// `check`, of the folded `module`, made in a context as it is made: its
/// pipeline, and the set binding the spans of the feedback buffer it reaches.
/// Anything made goes with the context should this fail.
fn make_check(ask: Ask<'_>, check: &Check, module: &[u8]) -> Result<(), DriverError> {
    let entries = check.bindings.map(|(binding, _, _)| LayoutEntry {
        binding,
        kind: descriptor::STORAGE_BUFFER,
        count: 1,
        stages: stage::COMPUTE,
    });
    let push_bytes =
        u32::try_from(check.push_words * 4).map_err(|_| DriverError::LengthOutOfRange)?;
    let push = (stage::COMPUTE, push_bytes);
    let release = make_pipeline(
        ask,
        (check.ids, &[(check.module, module)]),
        Shape::Compute("main"),
        (&[&entries], &[check.set_layout]),
        push,
    )
    .map_err(|(refused, _)| refused)?;
    let mut encoder = Encoder::default();
    encoder.create_descriptor_pool(DEVICE, check.pool, &[(descriptor::STORAGE_BUFFER, 2)])?;
    succeeded(
        Reader::new(&ask(encoder.as_bytes(), MADE_REPLY)?)
            .made(Command::CreateDescriptorPool, check.pool)?,
    )?;
    encoder.clear();
    encoder.allocate_descriptor_sets(DEVICE, (check.pool, check.set_layout), check.set)?;
    succeeded(
        Reader::new(&ask(encoder.as_bytes(), MADE_REPLY)?)
            .allocated(Command::AllocateDescriptorSets, &[check.set])?,
    )?;
    encoder.clear();
    let writes = check
        .bindings
        .map(|(binding, offset, range)| DescriptorWrite {
            binding,
            kind: descriptor::STORAGE_BUFFER,
            written: Written::Buffer {
                buffer: FEEDBACK,
                offset,
                range,
            },
        });
    encoder.update_descriptor_sets(DEVICE, check.set, &writes)?;
    encoder.append(&release)?;
    ask(encoder.as_bytes(), 0).map(drop)
}

/// Make the pipeline `ids.pipeline` of `modules`, each an id and its words,
/// running `shape`, its sets laid out as `sets` under `set_ids`, and its push
/// constants `push`: its modules and its sets' layouts in one exchange, then
/// its layout, then itself, each answered before anything names it.
/// Answers the commands letting go of the modules and the sets' layouts,
/// which nothing needs once the pipeline is made; refused, why and the
/// commands letting go of all that was made.
fn make_pipeline(
    ask: Ask<'_>,
    (ids, modules): (PipelineIds, &[(u64, &[u8])]),
    shape: Shape<'_>,
    (sets, set_ids): (&[&[LayoutEntry]], &[u64]),
    push: (u32, u32),
) -> Result<Encoder, (DriverError, Encoder)> {
    let mut parts = Encoder::default();
    let mut whole = Encoder::default();
    let made = (|| {
        let mut encoder = Encoder::default();
        for &(id, words) in modules {
            encoder.create_shader_module(DEVICE, id, words)?;
        }
        for (entries, &id) in sets.iter().zip(set_ids) {
            encoder.create_descriptor_set_layout(DEVICE, id, entries)?;
        }
        let replies = ask(
            encoder.as_bytes(),
            MADE_REPLY * (sets.len() + modules.len()),
        )?;
        let mut reader = Reader::new(&replies);
        let mut refused = None;
        for &(id, _) in modules {
            let module = reader.made(Command::CreateShaderModule, id)?;
            if module == VkResult::Success {
                parts.destroy(Command::DestroyShaderModule, DEVICE, id)?;
            }
            refused = refused.or(succeeded(module).err());
        }
        for &id in set_ids {
            let layout = reader.made(Command::CreateDescriptorSetLayout, id)?;
            if layout == VkResult::Success {
                parts.destroy(Command::DestroyDescriptorSetLayout, DEVICE, id)?;
            }
            refused = refused.or(succeeded(layout).err());
        }
        if let Some(refused) = refused {
            return Err(refused);
        }
        encoder.clear();
        encoder.create_pipeline_layout(DEVICE, ids.layout, set_ids, push)?;
        let layout = Reader::new(&ask(encoder.as_bytes(), MADE_REPLY)?)
            .made(Command::CreatePipelineLayout, ids.layout)?;
        succeeded(layout)?;
        whole.destroy(Command::DestroyPipelineLayout, DEVICE, ids.layout)?;
        encoder.clear();
        let command = match shape {
            Shape::Compute(entry) => {
                let &(module, _) = modules.first().ok_or(DriverError::OutOfRange)?;
                encoder.create_compute_pipelines(
                    DEVICE,
                    ids.pipeline,
                    (module, entry),
                    ids.layout,
                )?;
                Command::CreateComputePipelines
            }
            Shape::Render(info) => {
                encoder.create_graphics_pipelines(DEVICE, ids.pipeline, (info, ids.layout))?;
                Command::CreateGraphicsPipelines
            }
        };
        let pipeline = Reader::new(&ask(encoder.as_bytes(), MADE_REPLY)?)
            .allocated(command, &[ids.pipeline])?;
        succeeded(pipeline)
    })();
    match made {
        Ok(()) => Ok(parts),
        Err(refused) => {
            // Concatenating commands already encoded only grows a buffer.
            let _ = whole.append(&parts);
            Err((refused, whole))
        }
    }
}

/// The host's descriptor type for each of `bindings`, in order: one
/// descriptor apiece, as the device takes no arrays of them.
fn layout_entries(bindings: &[LayoutBinding]) -> Result<Vec<LayoutEntry>, DriverError> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(bindings.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    for binding in bindings {
        entries.push(LayoutEntry {
            binding: binding.binding,
            kind: descriptor_of(binding.kind),
            count: binding.count,
            stages: stage_flags(binding.stages),
        });
    }
    Ok(entries)
}

/// The host's descriptor type of the seam's.
const fn descriptor_of(kind: DescriptorType) -> u32 {
    match kind {
        DescriptorType::Sampler(_) => descriptor::SAMPLER,
        DescriptorType::CombinedImageSampler(..) => descriptor::COMBINED_IMAGE_SAMPLER,
        DescriptorType::SampledImage(..) => descriptor::SAMPLED_IMAGE,
        DescriptorType::StorageImage(_) => descriptor::STORAGE_IMAGE,
        DescriptorType::UniformTexelBuffer(_) => descriptor::UNIFORM_TEXEL_BUFFER,
        DescriptorType::StorageTexelBuffer(_) => descriptor::STORAGE_TEXEL_BUFFER,
        DescriptorType::UniformBuffer => descriptor::UNIFORM_BUFFER,
        DescriptorType::StorageBuffer => descriptor::STORAGE_BUFFER,
    }
}

/// What a binding of `kind` binds of `resource`: a sampler as the one
/// reading its border as the binding's texels read.
fn written(
    kind: DescriptorType,
    resource: &SetResource<'_, VenusDevice>,
) -> Result<Written, DriverError> {
    let integer = |sample: SampleType, sampler: SamplerType| {
        sampler == SamplerType::Integer || matches!(sample, SampleType::Sint | SampleType::Uint)
    };
    Ok(match (kind, resource) {
        (DescriptorType::Sampler(sampler), SetResource::Sampler(made)) => Written::Image {
            sampler: made.of(sampler == SamplerType::Integer),
            view: 0,
        },
        (
            DescriptorType::SampledImage(..) | DescriptorType::StorageImage(_),
            SetResource::Image(view),
        ) => Written::Image {
            sampler: 0,
            view: view.view,
        },
        (
            DescriptorType::CombinedImageSampler(_, sample, sampler),
            SetResource::ImageSampler(view, made),
        ) => Written::Image {
            sampler: made.of(integer(sample, sampler)),
            view: view.view,
        },
        (
            DescriptorType::UniformTexelBuffer(_) | DescriptorType::StorageTexelBuffer(_),
            SetResource::TexelBuffer(texels),
        ) => Written::Texels(texels.view),
        (
            DescriptorType::UniformBuffer | DescriptorType::StorageBuffer,
            &SetResource::Buffer {
                memory,
                offset,
                size,
            },
        ) => Written::Buffer {
            buffer: memory.buffer,
            offset,
            range: size,
        },
        // The model binds each resource to a binding of its kind.
        _ => return Err(DriverError::DeviceFault),
    })
}

/// The shader stage flags of `stages`.
fn stage_flags(stages: Stages) -> u32 {
    [
        (Stages::VERTEX, stage::VERTEX),
        (Stages::FRAGMENT, stage::FRAGMENT),
        (Stages::COMPUTE, stage::COMPUTE),
    ]
    .iter()
    .filter(|(seam, _)| stages.contains(*seam))
    .fold(0, |flags, (_, flag)| flags | flag)
}

/// End a job's command buffer: its writes made visible, then `seq` written
/// into feedback slot `slot`, then everything made visible to the host.
fn finish_commands(
    encoder: &mut Encoder,
    commands: u64,
    (seq, slot): (u32, u32),
) -> Result<(), DriverError> {
    encoder.cmd_barrier(commands)?;
    encoder.cmd_fill_buffer(commands, FEEDBACK, (u64::from(slot) * 4, 4), seq)?;
    encoder.cmd_host_barrier(commands)?;
    encoder.end_command_buffer(commands)
}

impl Hardware for VenusDevice {
    type Context = VenusContext;
    type Memory = VenusMemory;
    type Image = VenusImage;
    type View = VenusView;
    type BufferView = VenusTexels;
    type Sampler = VenusSampler;
    type Set = VenusSet;
    type Compute = VenusPipeline;
    type Render = VenusPipeline;

    fn create_context(&self) -> Result<VenusContext, Error> {
        make_context(&self.0).map_err(device_error)
    }

    fn allocate(
        &self,
        context: &Arc<VenusContext>,
        memory_type: u32,
        size: u64,
    ) -> Result<VenusMemory, Error> {
        allocate(context, memory_type, size).map_err(device_error)
    }

    fn write(&self, memory: &VenusMemory, offset: u64, data: &[u8]) -> Result<(), Error> {
        let host = memory.host.as_ref().ok_or(Error::NotHostVisible)?.lock();
        let window = host.part.window().ok_or(Error::NotHostVisible)?;
        let at = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        window.write(at, data).map_err(|_| Error::OutOfRange)
    }

    fn read(&self, memory: &VenusMemory, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let host = memory.host.as_ref().ok_or(Error::NotHostVisible)?.lock();
        let window = host.part.window().ok_or(Error::NotHostVisible)?;
        let at = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        window.read(at, out).map_err(|_| Error::OutOfRange)
    }

    fn image(
        &self,
        context: &Arc<VenusContext>,
        desc: &ImageDesc,
    ) -> Result<(VenusImage, MemoryRequirements), Error> {
        images::make_image(context, desc).map_err(|refused| images::refused(context, refused))
    }

    fn place(&self, image: &VenusImage, memory: &VenusMemory, offset: u64) -> Result<(), Error> {
        images::place_image(image, memory, offset)
            .map_err(|refused| images::refused(&image.context, refused))
    }

    fn view(
        &self,
        image: &VenusImage,
        _: &ImageDesc,
        desc: &ImageViewDesc,
    ) -> Result<VenusView, Error> {
        images::make_view(image, desc).map_err(|refused| images::refused(&image.context, refused))
    }

    fn buffer_view(
        &self,
        memory: &VenusMemory,
        offset: u64,
        desc: &BufferViewDesc,
    ) -> Result<VenusTexels, Error> {
        images::make_texels(memory, offset, desc)
            .map_err(|refused| images::refused(&memory.context, refused))
    }

    fn sampler(
        &self,
        context: &Arc<VenusContext>,
        desc: &SamplerDesc,
    ) -> Result<VenusSampler, Error> {
        images::make_sampler(context, desc).map_err(|refused| images::refused(context, refused))
    }

    fn set(
        &self,
        context: &Arc<VenusContext>,
        layout: &[LayoutBinding],
        resources: &[SetResource<'_, Self>],
    ) -> Result<VenusSet, Error> {
        make_set(context, layout, resources).map_err(|refused| {
            if refused == DriverError::DeviceFault {
                lose(context);
            }
            device_error(refused)
        })
    }

    fn compute(
        &self,
        context: &Arc<VenusContext>,
        stage: PipelineStage<'_>,
    ) -> Result<VenusPipeline, Error> {
        make_compute(context, stage)
    }

    fn render(
        &self,
        context: &Arc<VenusContext>,
        stages: RenderStages<'_>,
    ) -> Result<VenusPipeline, Error> {
        render::make_render(context, stages)
    }

    fn run(&self, job: Job<Self>) {
        let context = Arc::clone(job.context());
        match queue_job(&context, job) {
            Ok(()) => pump(&context),
            Err((job, loss)) => {
                job.finish(Err(loss));
                lose(&context);
            }
        }
    }

    fn run_bytes(commands: Commands<'_, Self>) -> u64 {
        let count = |len: usize| len as u64;
        commands.iter().fold(stream::JOB, |total, command| {
            let own = match command {
                JobCommand::Copy { spans, .. } => count(spans.len()) * stream::REGION,
                JobCommand::Update { data, .. } => count(data.len()),
                JobCommand::Fill { .. } => 0,
                JobCommand::BufferImage { regions, .. } => {
                    count(regions.len()) * stream::IMAGE_REGION
                }
                JobCommand::CopyImage { regions, .. } => {
                    count(regions.len()) * stream::IMAGE_REGION
                }
                JobCommand::ClearImage { ranges, .. } => count(ranges.len() + 1) * stream::RANGE,
                JobCommand::Dispatch {
                    sets, push, counts, ..
                } => {
                    let checked = match counts {
                        Counts::Direct(_) => 0,
                        Counts::Indirect { .. } => stream::CHECK,
                    };
                    stream::DISPATCH
                        + count(sets.len()) * stream::HANDLE
                        + count(push.len()) * stream::WORD
                        + checked
                }
                JobCommand::Pass { targets, draws, .. } => {
                    // Its colour targets, its depth and its stencil.
                    let instance = stream::PASS + count(targets.len() + 2) * stream::ATTACHMENT;
                    draws.iter().fold(instance, |total, draw| {
                        // A device checking records holds no more a draw
                        // than it checks; one not copies none.
                        let checked = match draw.count {
                            DrawCount::Direct(_) => 0,
                            DrawCount::Indirect { count, .. } => {
                                stream::RECORDS
                                    + instance
                                    + u64::from(count.min(CHECKED_RECORDS)) * stream::REGION
                            }
                        };
                        total
                            + stream::DRAW
                            + count(draw.sets.len()) * stream::HANDLE
                            + count(draw.push.len()) * stream::WORD
                            + count(draw.vertex_buffers.len()) * stream::VERTEX_BUFFER
                            + checked
                    })
                }
            };
            total + stream::COMMAND + own
        })
    }

    fn lost(&self, context: &Arc<VenusContext>) {
        lose(context);
    }
}

/// Make a bind set in `context` of `resources`, laid out as `bindings`: its
/// layout and a pool holding it alone in one exchange, then the set, then its
/// descriptors written and the layout let go, which nothing needs once the
/// set is.
fn make_set(
    context: &Arc<VenusContext>,
    bindings: &[LayoutBinding],
    resources: &[SetResource<'_, VenusDevice>],
) -> Result<VenusSet, DriverError> {
    let entries = layout_entries(bindings)?;
    if resources.len() != entries.len() || entries.iter().any(|entry| entry.count != 1) {
        return Err(DriverError::Unsupported);
    }
    let [layout, pool, set] = context.ids();
    let mut held = [0u32; descriptor::STORAGE_BUFFER as usize + 1];
    for entry in &entries {
        let slot = held
            .get_mut(entry.kind as usize)
            .ok_or(DriverError::DeviceFault)?;
        *slot = slot.checked_add(1).ok_or(DriverError::LengthOutOfRange)?;
    }
    let mut sizes: ArrayVec<(u32, u32), { descriptor::STORAGE_BUFFER as usize + 1 }> =
        ArrayVec::new();
    for (kind, &count) in (0u32..).zip(&held) {
        if count > 0 {
            sizes
                .try_push((kind, count))
                .map_err(|_| DriverError::DeviceFault)?;
        }
    }
    if sizes.is_empty() {
        // A pool holds a descriptor, though this set binds none.
        let _ = sizes.try_push((descriptor::STORAGE_BUFFER, 1));
    }
    let mut writes = Vec::new();
    writes
        .try_reserve_exact(entries.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    for ((binding, entry), resource) in bindings.iter().zip(&entries).zip(resources) {
        writes.push(DescriptorWrite {
            binding: entry.binding,
            kind: entry.kind,
            written: written(binding.kind, resource)?,
        });
    }
    // Every command is encoded before anything is made, so nothing made is
    // left behind for want of memory to say what follows.
    let mut made = Encoder::default();
    made.create_descriptor_set_layout(DEVICE, layout, &entries)?;
    made.create_descriptor_pool(DEVICE, pool, &sizes)?;
    let mut allocate = Encoder::default();
    allocate.allocate_descriptor_sets(DEVICE, (pool, layout), set)?;
    let mut layout_gone = Encoder::default();
    layout_gone.destroy(Command::DestroyDescriptorSetLayout, DEVICE, layout)?;
    let mut pool_gone = Encoder::default();
    pool_gone.destroy(Command::DestroyDescriptorPool, DEVICE, pool)?;
    let mut written = Encoder::default();
    written.update_descriptor_sets(DEVICE, set, &writes)?;
    written.append(&layout_gone)?;
    let replies = context.exchange(made.as_bytes(), 0, 2 * MADE_REPLY)?;
    let mut reader = Reader::new(&replies);
    let made_layout = reader.made(Command::CreateDescriptorSetLayout, layout)?;
    let made_pool = reader.made(Command::CreateDescriptorPool, pool)?;
    let filled = (|| {
        succeeded(made_layout)?;
        succeeded(made_pool)?;
        let replies = context.exchange(allocate.as_bytes(), 0, MADE_REPLY)?;
        succeeded(Reader::new(&replies).allocated(Command::AllocateDescriptorSets, &[set])?)?;
        context.exchange(written.as_bytes(), 0, 0).map(drop)
    })();
    if let Err(refused) = filled {
        if made_layout == VkResult::Success {
            queue_commands(context, layout_gone.as_bytes());
        }
        if made_pool == VkResult::Success {
            queue_commands(context, pool_gone.as_bytes());
        }
        return Err(refused);
    }
    Ok(VenusSet {
        context: Arc::clone(context),
        pool,
        set,
    })
}

/// Make a compute pipeline in `context` of the entry point `stage` names, its
/// module as the host is given every module.
fn make_compute(
    context: &Arc<VenusContext>,
    stage: PipelineStage<'_>,
) -> Result<VenusPipeline, Error> {
    let module = prepared(context, stage)?;
    let [module_id, layout, pipeline] = context.ids();
    make_in(
        context,
        stage,
        (
            PipelineIds { layout, pipeline },
            &[(module_id, module.admitted().module().bytes())],
        ),
        Shape::Compute(stage.entry_point()),
    )
}

/// `stage`'s module as the host is given it: folded, every specialization
/// constant and undefined value given the one value the seam reads, so the
/// host resolves none its own way; then guarded, as the host's robustness
/// bounds only buffers and images. The guard may declare a capability the
/// module did not, which the device must run as it must the module's own.
fn prepared(context: &VenusContext, stage: PipelineStage<'_>) -> Result<Guarded, Error> {
    let folded = stage
        .module()
        .fold(stage.specialized())
        .map_err(|refused| match refused {
            FoldError::OutOfMemory => Error::OutOfMemory,
            FoldError::TooLarge => Error::TooLarge,
            FoldError::Malformed | FoldError::Refused(_) => Error::Malformed,
        })?;
    let guarded = folded.guarded().map_err(|refused| match refused {
        GuardError::OutOfMemory => Error::OutOfMemory,
        GuardError::TooLarge => Error::TooLarge,
        GuardError::Refused(_) | GuardError::Unguarded => Error::Malformed,
    })?;
    if context.core.statement.info.runs_module(guarded.admitted()) {
        Ok(guarded)
    } else {
        Err(Error::Unsupported)
    }
}

/// The pipeline `ids` names in `context` of `modules` running `shape`,
/// under the sets and push constants of the layout `stage` was agreed with.
fn make_in(
    context: &Arc<VenusContext>,
    stage: PipelineStage<'_>,
    (ids, modules): (PipelineIds, &[(u64, &[u8])]),
    shape: Shape<'_>,
) -> Result<VenusPipeline, Error> {
    let mut set_entries = Vec::new();
    set_entries
        .try_reserve_exact(stage.sets().len())
        .map_err(|_| Error::OutOfMemory)?;
    for bindings in stage.sets() {
        set_entries.push(layout_entries(bindings).map_err(device_error)?);
    }
    let sets =
        tairix_util::fallible::collected(set_entries.len(), set_entries.iter().map(Vec::as_slice))
            .ok_or(Error::OutOfMemory)?;
    let first_set = context.id_run(sets.len()).map_err(device_error)?;
    let set_ids = tairix_util::fallible::collected(sets.len(), (first_set..).take(sets.len()))
        .ok_or(Error::OutOfMemory)?;
    let push = stage.push_constants();
    let push_stages = stage_flags(push.stages);
    let ask = |commands: &[u8], reply_len| context.exchange(commands, 0, reply_len);
    let made = make_pipeline(
        &ask,
        (ids, modules),
        shape,
        (&sets, &set_ids),
        (push_stages, push.bytes),
    );
    match made {
        Ok(release) => {
            queue_commands(context, release.as_bytes());
            Ok(VenusPipeline {
                context: Arc::clone(context),
                pipeline: ids.pipeline,
                layout: ids.layout,
                push_stages,
            })
        }
        Err((refused, release)) => {
            queue_commands(context, release.as_bytes());
            if refused == DriverError::DeviceFault {
                lose(context);
            }
            Err(device_error(refused))
        }
    }
}

/// Make memory of the stated type `memory_type` and `size` bytes in
/// `context`, zeroed.
fn allocate(
    context: &Arc<VenusContext>,
    memory_type: u32,
    size: u64,
) -> Result<VenusMemory, DriverError> {
    let core = &context.core;
    let index = usize::try_from(memory_type).map_err(|_| DriverError::OutOfRange)?;
    let (Some(kind), Some(&host_type)) = (
        core.statement.info.memory_types.get(index),
        core.statement.memory_types.get(index),
    ) else {
        return Err(DriverError::OutOfRange);
    };
    let visible = kind.properties.contains(MemoryProperties::HOST_VISIBLE);
    let [buffer, memory] = context.ids();
    let bytes = size
        .checked_next_multiple_of(4)
        .ok_or(DriverError::LengthOutOfRange)?;
    let mut encoder = Encoder::default();
    encoder.create_buffer(DEVICE, buffer, (bytes, BUFFER_USAGE_ALL))?;
    succeeded(
        Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .made(Command::CreateBuffer, buffer)?,
    )?;
    let mut release = Encoder::default();
    release.destroy_buffer(DEVICE, buffer)?;
    let made = (|| {
        encoder.clear();
        encoder.get_buffer_memory_requirements2(DEVICE, buffer)?;
        let required = Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .memory_requirements(Command::GetBufferMemoryRequirements2)?;
        if required.type_bits & (1 << host_type) == 0 {
            return Err(DriverError::Unsupported);
        }
        let alloc_size = if visible {
            required
                .size
                .checked_next_multiple_of(PAGE_SIZE as u64)
                .ok_or(DriverError::LengthOutOfRange)?
        } else {
            required.size
        };
        encoder.clear();
        encoder.allocate_memory(DEVICE, memory, (alloc_size, host_type))?;
        let allocated = Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .made(Command::AllocateMemory, memory)?;
        succeeded(allocated)?;
        release.free_memory(DEVICE, memory)?;
        encoder.clear();
        encoder.bind_buffer_memory2(DEVICE, buffer, (memory, 0))?;
        let bound = Reader::new(&context.exchange(encoder.as_bytes(), 0, SHORT_REPLY)?)
            .result_of(Command::BindBufferMemory2)?;
        succeeded(bound)?;
        Ok(alloc_size)
    })();
    let alloc_size = match made {
        Ok(alloc_size) => alloc_size,
        Err(refused) => {
            queue_commands(context, release.as_bytes());
            return Err(refused);
        }
    };
    let host = if visible {
        let mut making = Making::new(&core.desk, &core.shared);
        let spec = BlobSpec {
            blob_id: memory,
            size: alloc_size,
            mappable: true,
            shareable: false,
        };
        let zeroed = making
            .mapped(context.context, spec)
            .and_then(|mapped| zero_window(mapped.window()?, alloc_size));
        if let Err(refused) = zeroed {
            if making.let_go() {
                queue_commands(context, release.as_bytes());
            }
            return Err(refused);
        }
        making.keep().into_iter().next().map(Mutex::new)
    } else {
        if let Err(refused) = zero_device(context, buffer) {
            queue_commands(context, release.as_bytes());
            return Err(refused);
        }
        None
    };
    Ok(VenusMemory {
        context: Arc::clone(context),
        buffer,
        memory,
        host,
    })
}

/// Zero `len` bytes through the driver's window.
fn zero_window(window: &MemoryWindow, len: u64) -> Result<(), DriverError> {
    let zeros = [0u8; PAGE_SIZE];
    let len = usize::try_from(len).map_err(|_| DriverError::LengthOutOfRange)?;
    for at in (0..len).step_by(zeros.len()) {
        let chunk = zeros.len().min(len - at);
        window
            .write(at, &zeros[..chunk])
            .map_err(|_| DriverError::DeviceFault)?;
    }
    Ok(())
}

/// Zero `buffer` on the context's first queue, waiting until its feedback
/// says it ran.
fn zero_device(context: &VenusContext, buffer: u64) -> Result<(), DriverError> {
    utility(context, |encoder| {
        encoder.cmd_fill_buffer(UTILITY, buffer, (0, WHOLE_SIZE), 0)
    })
}

/// Run what `record` records into the utility command buffer on the
/// context's first queue, one such run at a time, waiting until its feedback
/// says it ran.
fn utility(
    context: &VenusContext,
    record: impl FnOnce(&mut Encoder) -> Result<(), DriverError>,
) -> Result<(), DriverError> {
    let queue = *context.queues.first().ok_or(DriverError::DeviceFault)?;
    let _utility = context.utility.lock();
    let seq = context.next_seq();
    context.mark(UTILITY_SLOT, !seq)?;
    let mut encoder = Encoder::default();
    encoder.begin_command_buffer(UTILITY, ONE_TIME_SUBMIT)?;
    record(&mut encoder)?;
    finish_commands(&mut encoder, UTILITY, (seq, UTILITY_SLOT))?;
    encoder.queue_submit(queue, &[UTILITY])?;
    context.exchange(encoder.as_bytes(), ring(0)?, 0)?;
    if context.reads(UTILITY_SLOT, seq) {
        Ok(())
    } else {
        Err(DriverError::DeviceFault)
    }
}

/// Queue commands answering nothing, letting host objects go, behind the
/// context's work. A context lost or ended has let them go already; one that
/// cannot hold them is lost, its virtio-gpu context ending with everything
/// it holds on the host, rather than leave them made.
fn queue_commands(context: &Arc<VenusContext>, commands: &[u8]) {
    let Ok(bytes) = owned(commands) else {
        lose(context);
        return;
    };
    let admits = {
        let mut state = context.state.lock();
        let admits = state.admits();
        if admits == Admits::Room {
            state.pending.push_back(Item::Commands(bytes));
        }
        admits
    };
    match admits {
        Admits::Room => pump(context),
        Admits::Full => lose(context),
        Admits::Gone => {}
    }
}

/// Encode `job` and queue it behind the context's work; the job again, and
/// what that loses, where it could not be.
fn queue_job(
    context: &Arc<VenusContext>,
    job: Job<VenusDevice>,
) -> Result<(), (Job<VenusDevice>, Loss)> {
    let (commands, fresh) = {
        let mut state = context.state.lock();
        if state.lost {
            drop(state);
            // A lost context's jobs answer nothing; dropped, the job says so.
            drop(job);
            return Ok(());
        }
        if let Some(commands) = state.free_commands.pop() {
            (commands, false)
        } else {
            let id = state.next_id;
            state.next_id += 1;
            (id, true)
        }
    };
    let seq = context.next_seq();
    let slot = context.claim_slot();
    let encoded = slot.ok_or(DriverError::OutOfMemory).and_then(|slot| {
        let mut parts = encode_job(context, &job, (commands, fresh), (seq, slot))?;
        let next = parts.pop_front().ok_or(DriverError::DeviceFault)?;
        let ring = ring(job.queue())?;
        context.mark(slot, !seq)?;
        context.clear_fault(slot)?;
        Ok((slot, next, parts, ring))
    });
    let mut state = context.state.lock();
    let refused = match encoded {
        Err(why) => why,
        Ok((slot, next, rest, ring)) => match state.admits() {
            Admits::Room => {
                state.pending.push_back(Item::Job {
                    job,
                    next,
                    rest,
                    ring,
                    seq,
                    slot,
                    commands,
                });
                return Ok(());
            }
            Admits::Gone => {
                drop(state);
                context.release_slot(slot);
                // Lost while it was encoded; dropped, the job says so.
                drop(job);
                return Ok(());
            }
            Admits::Full => DriverError::OutOfMemory,
        },
    };
    // Never sent, so the next job may record into it and take its slot.
    if !fresh && state.free_commands.try_reserve(1).is_ok() {
        state.free_commands.push(commands);
    }
    drop(state);
    if let Some(slot) = slot {
        context.release_slot(slot);
    }
    let loss = match refused {
        DriverError::OutOfMemory => Loss::OutOfMemory,
        _ => Loss::Fault,
    };
    Err((job, loss))
}

/// Bind `pipeline`, its sets `sets` and its push constants `push` for a
/// dispatch.
fn bind(
    one: &mut Encoder,
    commands: u64,
    pipeline: &VenusPipeline,
    (sets, push): (&[u64], &[u32]),
) -> Result<(), DriverError> {
    bind_ids(
        one,
        commands,
        (pipeline.pipeline, pipeline.layout, pipeline.push_stages),
        (sets, push),
    )
}

/// [`bind`] of the pipeline `pipeline`, laid out as `layout`, its push
/// constants seen by `stages`.
fn bind_ids(
    one: &mut Encoder,
    commands: u64,
    (pipeline, layout, stages): (u64, u64, u32),
    (sets, push): (&[u64], &[u32]),
) -> Result<(), DriverError> {
    one.cmd_bind_pipeline(commands, BIND_POINT_COMPUTE, pipeline)?;
    if !sets.is_empty() {
        one.cmd_bind_sets(commands, (BIND_POINT_COMPUTE, layout), sets)?;
    }
    if !push.is_empty() {
        one.cmd_push_constants(commands, (layout, stages), push)?;
    }
    Ok(())
}

/// Hold the three counts at `offset` into `buffer` to `most` on the device,
/// in feedback slot `slot`, answering where in the feedback buffer the
/// checked counts lie: copied there, then the check run on them, each after
/// a barrier making everything before it visible to it.
fn check_counts(
    one: &mut Encoder,
    commands: u64,
    (buffer, offset): (u64, u64),
    (slot, most): (u32, [u32; 3]),
) -> Result<u64, DriverError> {
    let at = COUNTS_AT + u64::from(slot) * COUNTS_SLOT_BYTES;
    one.cmd_copy_buffer(
        commands,
        (buffer, FEEDBACK),
        core::iter::once(CopyRegion {
            src: offset,
            dst: at,
            size: 12,
        }),
    )?;
    one.cmd_barrier(commands)?;
    run_check(
        one,
        commands,
        &COUNTS_CHECK,
        &counts::Push { slot, most }.words(),
    )?;
    one.cmd_barrier(commands)?;
    Ok(at)
}

/// Run `check` once with `push` as its push constants.
fn run_check(
    one: &mut Encoder,
    commands: u64,
    check: &Check,
    push: &[u32],
) -> Result<(), DriverError> {
    bind_check(one, commands, check)?;
    dispatch_check(one, commands, check, push)
}

/// Bind `check`'s pipeline and its set.
fn bind_check(one: &mut Encoder, commands: u64, check: &Check) -> Result<(), DriverError> {
    one.cmd_bind_pipeline(commands, BIND_POINT_COMPUTE, check.ids.pipeline)?;
    one.cmd_bind_sets(
        commands,
        (BIND_POINT_COMPUTE, check.ids.layout),
        &[check.set],
    )
}

/// Run `check`, bound, once on push constants `push`.
fn dispatch_check(
    one: &mut Encoder,
    commands: u64,
    check: &Check,
    push: &[u32],
) -> Result<(), DriverError> {
    one.cmd_push_constants(commands, (check.ids.layout, stage::COMPUTE), push)?;
    one.cmd_dispatch(commands, [1, 1, 1])
}

/// A job's stream, in parts at command boundaries, each at most
/// [`PART_MAX`].
fn encode_job(
    context: &VenusContext,
    job: &Job<VenusDevice>,
    (commands, fresh): (u64, bool),
    (seq, slot): (u32, u32),
) -> Result<VecDeque<Vec<u8>>, DriverError> {
    let queue = *context
        .queues
        .get(job.queue())
        .ok_or(DriverError::OutOfRange)?;
    let mut parts = VecDeque::new();
    let mut part = Encoder::default();
    let mut one = Encoder::default();
    let mut add = |one: &mut Encoder, part: &mut Encoder| {
        if part.as_bytes().len() + one.as_bytes().len() > PART_MAX {
            parts.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
            parts.push_back(owned(part.as_bytes())?);
            part.clear();
        }
        part.append(one)?;
        one.clear();
        Ok::<(), DriverError>(())
    };
    if fresh {
        one.allocate_command_buffers(DEVICE, POOL, &[commands])?;
    }
    one.begin_command_buffer(commands, ONE_TIME_SUBMIT)?;
    add(&mut one, &mut part)?;
    for command in job.commands().iter() {
        one.cmd_barrier(commands)?;
        encode_command(&mut one, (context, commands, slot), command, &mut |one| {
            add(one, &mut part)
        })?;
        add(&mut one, &mut part)?;
    }
    finish_commands(&mut one, commands, (seq, slot))?;
    one.queue_submit(queue, &[commands])?;
    add(&mut one, &mut part)?;
    parts.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
    parts.push_back(owned(part.as_bytes())?);
    Ok(parts)
}

/// Record `command` into `commands`, encoded into `one`: a job of
/// `context` holding feedback slot `slot`. A command of many parts hands each
/// finished to `flush`.
fn encode_command(
    one: &mut Encoder,
    (context, commands, slot): (&VenusContext, u64, u32),
    command: JobCommand<'_, VenusDevice>,
    flush: &mut dyn FnMut(&mut Encoder) -> Result<(), DriverError>,
) -> Result<(), DriverError> {
    match command {
        JobCommand::Copy { src, dst, spans } => {
            let regions = spans.iter().map(|span| CopyRegion {
                src: span.src,
                dst: span.dst,
                size: span.len,
            });
            one.cmd_copy_buffer(commands, (src.buffer, dst.buffer), regions)?;
        }
        JobCommand::Fill {
            dst,
            offset,
            size,
            word,
        } => one.cmd_fill_buffer(commands, dst.buffer, (offset, size), word)?,
        JobCommand::Update { dst, offset, data } => {
            one.cmd_update_buffer(commands, dst.buffer, offset, data)?;
        }
        JobCommand::BufferImage {
            memory,
            offset,
            image,
            regions,
            direction,
        } => {
            let regions = images::buffer_image_regions(&image.desc, offset, regions)?;
            match direction {
                Direction::ToImage => {
                    one.cmd_copy_buffer_to_image(commands, (memory.buffer, image.image), &regions)?;
                }
                Direction::ToBuffer => {
                    one.cmd_copy_image_to_buffer(commands, (image.image, memory.buffer), &regions)?;
                }
            }
        }
        JobCommand::CopyImage { src, dst, regions } => {
            one.cmd_copy_image(
                commands,
                (src.image, dst.image),
                &images::image_regions(regions)?,
            )?;
        }
        JobCommand::ClearImage {
            image,
            value,
            ranges,
        } => images::clear(one, commands, image, (value, ranges))?,
        JobCommand::Dispatch {
            pipeline,
            sets,
            push,
            counts,
        } => {
            let mut bound: ArrayVec<u64, MAX_DISPATCH_SETS> = ArrayVec::new();
            for set in sets.iter() {
                bound
                    .try_push(set.set)
                    .map_err(|_| DriverError::OutOfRange)?;
            }
            match counts {
                Counts::Direct(groups) => {
                    bind(one, commands, pipeline, (&bound, push))?;
                    one.cmd_dispatch(commands, groups)?;
                }
                Counts::Indirect { memory, offset } => {
                    let checked = check_counts(
                        one,
                        commands,
                        (memory.buffer, offset),
                        (slot, context.core.statement.info.limits.max_workgroup_count),
                    )?;
                    bind(one, commands, pipeline, (&bound, push))?;
                    one.cmd_dispatch_indirect(commands, FEEDBACK, checked)?;
                }
            }
        }
        JobCommand::Pass {
            targets,
            depth_stencil,
            area,
            draws,
        } => render::encode_pass(
            one,
            (commands, slot, context.core.records.is_some()),
            render::Pass {
                targets,
                depth_stencil,
                area: [area.x, area.y, area.width, area.height],
                draws,
            },
            flush,
        )?,
    }
    Ok(())
}

/// The next part waiting and what its answer is for, taken from the front
/// of the context's work; the job it ends finishes with it.
fn take_part(
    context: &Arc<VenusContext>,
    state: &mut ContextState,
    span: &Range<u64>,
) -> Option<(Vec<u8>, u8, Then)> {
    let part = || Then::Part(Arc::clone(context), span.clone());
    match state.pending.pop_front()? {
        Item::Job {
            job,
            next,
            mut rest,
            ring,
            seq,
            slot,
            commands,
        } => match rest.pop_front() {
            None => {
                let finishing = Finishing {
                    job,
                    seq,
                    slot,
                    commands,
                };
                Some((
                    next,
                    ring,
                    Then::Finish(Arc::clone(context), span.clone(), finishing),
                ))
            }
            Some(following) => {
                // The slot just taken is free for it again.
                state.pending.push_front(Item::Job {
                    job,
                    next: following,
                    rest,
                    ring,
                    seq,
                    slot,
                    commands,
                });
                Some((next, 0, part()))
            }
        },
        Item::Commands(bytes) => Some((bytes, 0, part())),
    }
}

/// The bytes of the part at the front of the context's work.
fn front_len(state: &ContextState) -> Option<u64> {
    let len = match state.pending.front()? {
        Item::Job { next, .. } => next.len(),
        Item::Commands(bytes) => bytes.len(),
    };
    Some(len as u64)
}

/// Send what waits in the context's job area as far as its room allows, in
/// the order it came. A part that cannot be sent loses the context.
fn pump(context: &Arc<VenusContext>) {
    loop {
        let mut state = context.state.lock();
        if state.lost {
            return;
        }
        let Some(len) = front_len(&state) else {
            return;
        };
        let Ok(Some(span)) = state
            .spans
            .place(JOBS_AT as u64..SHARED_BYTES as u64, len, ())
        else {
            // No room until a part with the device is answered.
            return;
        };
        let Some((bytes, ring, then)) = take_part(context, &mut state, &span) else {
            state.spans.remove(span.start);
            return;
        };
        let written = {
            let shared_memory = context.shared_memory.lock();
            shared_memory
                .as_ref()
                .and_then(|mapped| mapped.part.window())
                .zip(usize::try_from(span.start).ok())
                .is_some_and(|(window, at)| window.write(at, &bytes).is_ok())
        };
        let order = payload(|encoder| {
            encoder.execute_command_streams(&[StreamSpan {
                resource: context.resource,
                offset: span.start,
                size: len,
            }])
        });
        let sent = match order {
            Ok(payload) if written => context
                .core
                .desk
                .order(
                    Order::Submit {
                        context: context.context,
                        ring,
                        payload,
                    },
                    then,
                )
                .map_err(|(_, then)| then),
            _ => Err(then),
        };
        match sent {
            Ok(()) => state.in_flight += 1,
            Err(then) => {
                state.spans.remove(span.start);
                drop(state);
                // Without the context's lock: what answers a part may hold its
                // job.
                drop(then);
                lose(context);
                return;
            }
        }
    }
}

/// Lose `context`: nothing more of it is sent, the work it holds is answered
/// lost, and its virtio-gpu context ends once nothing of it is with the
/// device. The model counts a job it handed over until it is answered, so
/// work held for a lost context would keep its engine from every other.
fn lose(context: &Arc<VenusContext>) {
    let (dropped, waiting) = {
        let mut state = context.state.lock();
        state.lost = true;
        (core::mem::take(&mut state.pending), state.waiting.take())
    };
    // Without the context's lock: a job dropped answers the model.
    drop(dropped);
    if let Some(waiter) = waiting {
        waiter.give(Answer::Refused(DriverError::DeviceOffline, None));
    }
    end_if_idle(context);
}

/// End a lost context's virtio-gpu context once nothing of it is with the
/// device.
fn end_if_idle(context: &Arc<VenusContext>) {
    let end = {
        let mut state = context.state.lock();
        let idle = state.lost && state.in_flight == 0 && !state.ended;
        if idle {
            state.ended = true;
        }
        idle
    };
    if end {
        let _ = context
            .core
            .desk
            .order(Order::EndContext(context.context), Then::Ended);
    }
}

impl Then {
    /// Act on `answer`, on the thread answering the desk.
    fn run(self, answer: Answer) {
        match self {
            Self::Wake(waiter) => waiter.give(answer),
            Self::Part(context, span) => {
                if part_answered(&context, &span, &answer) {
                    pump(&context);
                    end_if_idle(&context);
                } else {
                    lose(&context);
                }
            }
            Self::Finish(context, span, finishing) => {
                // An indirect command's counts or records past the device's
                // ran nothing and lose the context, as on the software
                // device.
                let ran = part_answered(&context, &span, &answer)
                    && context.reads(finishing.slot, finishing.seq)
                    && context.unfaulted(finishing.slot);
                context.release_slot(finishing.slot);
                if ran {
                    let mut state = context.state.lock();
                    if state.free_commands.try_reserve(1).is_ok() {
                        state.free_commands.push(finishing.commands);
                    }
                }
                // Without the context's lock: the model may start the next job.
                finishing
                    .job
                    .finish(if ran { Ok(()) } else { Err(Loss::Fault) });
                if ran {
                    pump(&context);
                    end_if_idle(&context);
                } else {
                    // A retired fence proves nothing: a job whose number never
                    // landed did not run, and its context may hold no more.
                    lose(&context);
                }
            }
            Self::Ended => {}
            Self::Release(release) => release.next(answer),
        }
    }
}

/// Free a part's span, answering whether the device took the part.
fn part_answered(context: &VenusContext, span: &Range<u64>, answer: &Answer) -> bool {
    let mut state = context.state.lock();
    state.spans.remove(span.start);
    state.in_flight = state.in_flight.saturating_sub(1);
    matches!(answer, Answer::Done)
}

/// Letting blobs go, each unmapped, ended and its part given back, then the
/// commands freeing what they held and the context ending where either is
/// owed.
pub(crate) struct Release {
    core: Arc<Core>,
    blobs: ArrayVec<Mapped, 2>,
    /// The blob ended next, its part given back once it is.
    ending: Option<Blob>,
    afterwards: Afterwards,
    context: Option<Context>,
}

/// What follows once a release's blobs are gone.
enum Afterwards {
    /// Nothing: no memory is owed, or a blob still names it and it is kept.
    Nothing,
    /// The commands freeing what the blobs held, queued in its context.
    Free(Arc<VenusContext>, Vec<u8>),
    /// The context lost, the commands freeing what the blobs held not had:
    /// its end frees it.
    Lose(Arc<VenusContext>),
}

impl Release {
    /// Take the next step, or end.
    fn start(mut self) {
        if let Some(mapped) = self.blobs.pop() {
            self.ending = Some(mapped.blob);
            let desk = Arc::clone(&self.core.desk);
            let _ = desk.order(
                Order::UnmapBlob(mapped.blob, mapped.part),
                Then::Release(self),
            );
            return;
        }
        match core::mem::replace(&mut self.afterwards, Afterwards::Nothing) {
            Afterwards::Nothing => {}
            Afterwards::Free(context, commands) => queue_commands(&context, &commands),
            Afterwards::Lose(context) => lose(&context),
        }
        if let Some(context) = self.context.take() {
            let desk = Arc::clone(&self.core.desk);
            let _ = desk.order(Order::EndContext(context), Then::Release(self));
        }
    }

    fn next(mut self, answer: Answer) {
        if let Some(blob) = self.ending.take() {
            // A part comes back only where the device maps nothing into it.
            if let Answer::Unmapped(part) | Answer::Refused(_, Some(part)) = answer {
                let _ = self.core.shared.lock().release(part);
                let desk = Arc::clone(&self.core.desk);
                let _ = desk.order(Order::EndBlob(blob), Then::Release(self));
                return;
            }
            // Still mapped: its memory is kept with it.
            self.afterwards = Afterwards::Nothing;
        } else if !matches!(answer, Answer::Done) {
            // A blob that would not end still names its memory.
            self.afterwards = Afterwards::Nothing;
        }
        self.start();
    }
}

impl Drop for VenusMemory {
    fn drop(&mut self) {
        let mut release = Encoder::default();
        let commands = release
            .destroy_buffer(DEVICE, self.buffer)
            .and_then(|()| release.free_memory(DEVICE, self.memory))
            .and_then(|()| owned(release.as_bytes()));
        let context = Arc::clone(&self.context);
        let afterwards = match commands {
            Ok(commands) => Afterwards::Free(context, commands),
            Err(_) => Afterwards::Lose(context),
        };
        let mut blobs = ArrayVec::new();
        if let Some(host) = self.host.take() {
            let _ = blobs.try_push(host.into_inner());
        }
        // Its blob goes first: the memory is freed once nothing maps it.
        Release {
            core: Arc::clone(&self.context.core),
            blobs,
            ending: None,
            afterwards,
            context: None,
        }
        .start();
    }
}

impl Drop for VenusContext {
    fn drop(&mut self) {
        let ended = self.state.get_mut().ended;
        let mut blobs = ArrayVec::new();
        for mapped in [
            self.feedback_memory.get_mut().take(),
            self.shared_memory.get_mut().take(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = blobs.try_push(mapped);
        }
        Release {
            core: Arc::clone(&self.core),
            blobs,
            ending: None,
            afterwards: Afterwards::Nothing,
            context: (!ended).then_some(self.context),
        }
        .start();
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;

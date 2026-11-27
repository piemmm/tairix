//! The software device: every queue family's work run on the CPU, on a
//! worker of the device's own, or where it has none on the thread whose act
//! made the work ready.

use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_inline::ArrayVec;
use tairix_parallel::JobRunner;
use tairix_rt::sync::{Condvar, Mutex};

use crate::backend::{Backend, Prepared};
use crate::binding::{BindSetLayoutState, Bound};
use crate::command::Op;
use crate::device::{notify, Shared};
use crate::fair::{Fair, FairGuard};
use crate::format::Format;
use crate::image::{BufferViewDesc, ImageDesc, ImageViewDesc};
use crate::info::{
    DeviceInfo, Features, FormatTable, Granularity, Limits, MemoryHeap, MemoryProperties,
    MemoryType, QueueCapabilities, QueueFamily,
};
use crate::memory::Charge;
use crate::model::{Model, Woken};
use crate::pipeline::StagePlan;
use crate::render::RenderPlan;
use crate::resource::MemoryRequirements;
use crate::sampler::SamplerDesc;
use crate::{Device, Error, Loss};

pub(crate) mod dispatch;
pub(crate) mod layout;
pub(crate) mod memory;
pub(crate) mod pipeline;
pub(crate) mod render;
pub(crate) mod shader;
pub(crate) mod texel;
mod transfer;

use layout::ImageLayout;
use memory::Memory;
use texel::Srgb;
use transfer::Moved;

/// The model's checked work and states, as the software device holds them.
pub(crate) type PassWork = crate::pass::PassWork<Software>;
pub(crate) type DrawWork = crate::pass::DrawWork<Software>;
pub(crate) type ViewState = crate::resource::ViewState<Software>;
pub(crate) type ImageState = crate::resource::ImageState<Software>;
pub(crate) type BufferState = crate::model::BufferState<Software>;
pub(crate) type BindSetState = crate::binding::BindSetState<Software>;

/// What a buffer's offset, or a buffer binding's, must be a multiple of: the
/// widest vector the CPU's copies move.
const BUFFER_ALIGNMENT: u64 = 16;

/// The widest image along one or two axes: a 16K texture, past any display
/// the desktop drives.
const MAX_IMAGE_2D: u32 = 16_384;

/// The widest image along three axes.
const MAX_IMAGE_3D: u32 = 2048;

/// The most layers of one image.
const MAX_LAYERS: u32 = 2048;

/// The most descriptors of one bind set: room for a set binding every texture
/// a frame samples, and few enough that making one, which looks each up under
/// the device's lock, keeps every other context waiting no more than moments.
const MAX_BIND_SET_DESCRIPTORS: u32 = 1 << 16;

/// The most bytes of registers and private memory one invocation holds: a
/// shader's whole working set, local arrays included, which a tile of
/// invocations holds once per lane.
const MAX_INVOCATION_MEMORY: u32 = 16 << 10;

/// The most bytes an invocation reaches a word in: it addresses a buffer
/// binding, and a texel an atomic names, with a 32-bit offset.
const ADDRESSED: u64 = (u32::MAX - 3) as u64;

/// The longest a submission runs once started: past any frame or compute
/// pass a CPU runs, and the bound on how long a runaway holds the engine.
const MAX_JOB_SECS: i64 = 60;

/// How much work passes between two readings of the clock for a job's
/// deadline, counted as the bytes a transfer moves and the words a
/// dispatch's tiles compute for each lane: a reading costs a call into the
/// kernel, and this much work a thousand times that.
pub(crate) const WORK_PER_CHECK: u64 = 1 << 20;

/// How many bytes the software device's one heap offers: half the machine's
/// memory, leaving the rest to the system and to what its clients keep
/// outside the device.
pub(crate) const fn heap_bytes(total_ram_bytes: u64) -> u64 {
    total_ram_bytes / 2
}

/// Starts the thread a software device runs its work on.
pub trait Spawn {
    /// Run `body` on a thread of its own, answering whether one was started.
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> bool;
}

/// The process's own threads, `lib/rt`'s: where there are none to start (the
/// host, `wasm32`), the device runs its work on its callers' threads.
#[derive(Copy, Clone, Debug, Default)]
pub struct RuntimeThreads;

impl Spawn for RuntimeThreads {
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> bool {
        tairix_rt::thread::Thread::spawn(body).is_ok()
    }
}

/// No thread: the device runs its work on the thread whose act made it ready.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoThreads;

impl Spawn for NoThreads {
    fn spawn(&self, _: Box<dyn FnOnce() + Send>) -> bool {
        false
    }
}

/// The subgroup the software device runs at unless asked for another: the
/// commonest hardware width.
pub const DEFAULT_SUBGROUP_SIZE: u32 = 32;

/// How the software device is made.
pub struct SoftwareDesc<'a> {
    /// The machine's discovered memory, half of which its heap offers.
    pub memory_bytes: u64,
    /// What deadlines are read from.
    pub clock: Arc<dyn MonotonicClock + Send + Sync>,
    /// The process's runner, whose cores a command's work is shared across.
    pub runner: Arc<dyn JobRunner + Send>,
    /// What starts the device's worker.
    pub threads: &'a dyn Spawn,
    /// The invocations a subgroup holds: a power of two up to
    /// [`MAX_SUBGROUP_SIZE`](crate::MAX_SUBGROUP_SIZE), so the reference can
    /// run at the width of a device it is compared with.
    pub subgroup_size: u32,
}

impl Device {
    /// The software device.
    ///
    /// One memory type, the CPU's own memory, is every kind at once; one
    /// queue family runs every kind of work, one job at a time. It states every
    /// capability a module may declare, every feature, and every use of each
    /// format its encoding can mean.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] when the memory leaves no heap or the subgroup
    /// size is not one a device can have.
    pub fn software(desc: &SoftwareDesc<'_>) -> Result<Self, Error> {
        let mut info = info(desc.memory_bytes)?;
        info.limits.subgroup_size = desc.subgroup_size;
        Self::software_stating(
            info,
            Arc::clone(&desc.clock),
            Arc::clone(&desc.runner),
            desc.threads,
        )
    }

    /// The software device stating `info`, its work run on a thread `threads`
    /// starts or, where none can be started, on the thread whose act made it
    /// ready.
    pub(crate) fn software_stating(
        info: DeviceInfo,
        clock: Arc<dyn MonotonicClock + Send + Sync>,
        runner: Arc<dyn JobRunner + Send>,
        threads: &dyn Spawn,
    ) -> Result<Self, Error> {
        software_shared(info, clock, runner, threads).map(Self::local)
    }
}

/// The software device stating `info`, as [`Device::software_stating`] makes
/// it, held as itself.
pub(crate) fn software_shared(
    info: DeviceInfo,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
    runner: Arc<dyn JobRunner + Send>,
    threads: &dyn Spawn,
) -> Result<Arc<Shared<Software>>, Error> {
    let mut pool = dispatch::Workspaces::new()?;
    pool.set_width(runner.width());
    // The one worker runs a job of any engine at a time.
    let engines = info.engines(1);
    let model = Model::new(info, &engines, pool)?;
    let wake = crate::shared(Wake::new())?;
    let shared = crate::shared(Shared {
        info: model.info().clone(),
        model: Fair::new(model),
        clock,
        backend: Software {
            runner,
            srgb: Srgb::new(),
            wake: Arc::clone(&wake),
        },
    })?;
    let device = Arc::downgrade(&shared);
    let parked = Arc::clone(&wake);
    let body = Box::try_new(move || work(&device, &parked)).map_err(|_| Error::OutOfMemory)?;
    if threads.spawn(body) {
        wake.running.store(true, Ordering::Release);
    }
    Ok(shared)
}

/// The software backend: the CPU running every engine's work, on a worker of
/// the device's own or on the thread whose act made the work ready.
pub(crate) struct Software {
    /// The process's runner, whose cores a command's work is shared across.
    pub(crate) runner: Arc<dyn JobRunner + Send>,
    pub(crate) srgb: Srgb,
    wake: Arc<Wake>,
}

impl Drop for Software {
    fn drop(&mut self) {
        self.wake.poke(true);
    }
}

impl Backend for Software {
    type Context = ();
    type Memory = Memory;
    type Image = ImageLayout;
    // The device reads a view's, a texel buffer's and a sampler's state where
    // the model keeps it.
    type View = ();
    type BufferView = ();
    type Sampler = ();
    type Set = dispatch::SetMemories;
    type Compute = shader::ir::Program;
    type Render = pipeline::Render;
    type Pool = dispatch::Workspaces;

    const WATCHED: bool = false;

    fn overhead(size: u64) -> u64 {
        Memory::overhead(size)
    }

    fn create_context(&self) -> Result<(), Error> {
        Ok(())
    }

    fn allocate(
        &self,
        _: &Arc<()>,
        (_, size): (u32, u64),
        [bytes, besides]: [Charge; 2],
    ) -> Result<Memory, Error> {
        Memory::zeroed(size, bytes, besides)
    }

    fn lost(&self, _: &Arc<()>) {}

    fn write(&self, memory: &Memory, offset: u64, data: &[u8]) -> Result<(), Error> {
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let mut words = memory.lock();
        memory::write_bytes(&mut words, start, data).ok_or(Error::OutOfRange)
    }

    fn read(&self, memory: &Memory, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let words = memory.lock();
        memory::read_bytes(&words, start, out).ok_or(Error::OutOfRange)
    }

    fn image(
        &self,
        _: &Arc<()>,
        info: &DeviceInfo,
        desc: &ImageDesc,
    ) -> Result<(ImageLayout, MemoryRequirements), Error> {
        let layout = ImageLayout::of(desc)?;
        let types = info.memory_types.len();
        let memory_types = u32::try_from((1u64 << types) - 1).map_err(|_| Error::Malformed)?;
        let requirements = MemoryRequirements {
            size: layout.size,
            alignment: ImageLayout::alignment(),
            memory_types,
        };
        Ok((layout, requirements))
    }

    fn place(&self, _: &ImageLayout, memory: &Memory, offset: u64, size: u64) -> Result<(), Error> {
        let word = |byte: u64| usize::try_from(byte / 4).map_err(|_| Error::OutOfRange);
        let end = offset.checked_add(size).ok_or(Error::OutOfRange)?;
        // Placements and sizes are whole layers, every layer whole words.
        if !offset.is_multiple_of(4) || !size.is_multiple_of(4) {
            return Err(Error::Misaligned);
        }
        memory
            .lock()
            .get_mut(word(offset)?..word(end)?)
            .ok_or(Error::OutOfRange)?
            .fill(0);
        Ok(())
    }

    fn view(&self, _: &ImageLayout, _: &ImageDesc, _: &ImageViewDesc) -> Result<(), Error> {
        Ok(())
    }

    fn buffer_view(&self, _: &Memory, _: u64, _: &BufferViewDesc) -> Result<(), Error> {
        Ok(())
    }

    fn sampler(&self, _: &Arc<()>, _: &SamplerDesc) -> Result<(), Error> {
        Ok(())
    }

    fn set(
        &self,
        _: &Arc<()>,
        _: &BindSetLayoutState,
        resources: &[Bound<Self>],
    ) -> Result<(dispatch::SetMemories, u64), Error> {
        let set = dispatch::SetMemories::of(resources)?;
        let bytes = set.held_bytes();
        Ok((set, bytes))
    }

    fn compute(
        &self,
        _: &Arc<()>,
        info: &DeviceInfo,
        stage: &StagePlan<'_>,
    ) -> Result<Prepared<Self, shader::ir::Program>, Error> {
        Self::compute_form(info, stage)
    }

    fn render(
        &self,
        _: &Arc<()>,
        info: &DeviceInfo,
        plan: &RenderPlan<'_>,
    ) -> Result<Prepared<Self, pipeline::Render>, Error> {
        Self::render_form(info, plan)
    }

    fn run_bytes(pool: &dispatch::Workspaces, ops: &[Op<Self>]) -> u64 {
        let participants = pool.width();
        let most = |bytes: &dyn Fn(&Op<Self>) -> u64| ops.iter().map(bytes).max().unwrap_or(0);
        let run = most(&|op| match op {
            Op::Dispatch(work) => dispatch::run_bytes(work, participants),
            Op::Pass(pass) => render::run_bytes(pass, participants),
            _ => 0,
        });
        let placed = most(&|op| match op {
            Op::Dispatch(work) => dispatch::place_bytes(work, participants),
            _ => 0,
        });
        run.saturating_add(placed)
    }

    /// On the device's worker, or on this thread where it has none, or,
    /// where its engine is already running work, on the thread running it,
    /// which takes it next.
    fn drive(shared: &Shared<Self>, model: FairGuard<'_, Model<Self>>) {
        if shared.backend.wake.running.load(Ordering::Acquire) {
            drop(model);
            shared.backend.wake.poke(false);
        } else {
            run_ready(shared, model);
        }
    }
}

/// How work reaches the device's own worker: a pending flag it parks on,
/// whether the device is gone, and whether there is a worker at all.
pub(crate) struct Wake {
    state: Mutex<WakeState>,
    ready: Condvar,
    /// Set once the worker's thread is started; until then, and for good where
    /// none can be, work runs on the thread whose act made it ready.
    running: AtomicBool,
}

#[derive(Default)]
struct WakeState {
    pending: bool,
    closed: bool,
    /// Whether the worker waits to be poked, so a poke must wake it.
    parked: bool,
}

impl Wake {
    fn new() -> Self {
        Self {
            state: Mutex::new(WakeState::default()),
            ready: Condvar::new(),
            running: AtomicBool::new(false),
        }
    }

    fn poke(&self, close: bool) {
        let mut state = self.state.lock();
        state.pending = true;
        state.closed |= close;
        let parked = state.parked;
        drop(state);
        if parked {
            self.ready.notify_one();
        }
    }

    /// Park until poked, answering whether the device is still there.
    fn park(&self) -> bool {
        let mut state = self.state.lock();
        while !state.pending && !state.closed {
            state.parked = true;
            #[cfg(test)]
            waits::note(state.parked);
            state = self.ready.wait(state);
        }
        state.parked = false;
        state.pending = false;
        !state.closed
    }
}

/// The device's worker: it runs every job that is ready, then parks holding
/// only a weak reference, so an idle device it serves is never kept alive by
/// it; a job it runs keeps the device until that job ends.
fn work(device: &Weak<Shared<Software>>, wake: &Wake) {
    loop {
        let Some(shared) = device.upgrade() else {
            return;
        };
        run_ready(&shared, shared.model.lock());
        drop(shared);
        if !wake.park() {
            return;
        }
    }
}

/// Run every job the device has room for and that is ready, until none is.
fn run_ready<'a>(shared: &'a Shared<Software>, mut model: FairGuard<'a, Model<Software>>) {
    let mut woken = Woken::new();
    let now = || shared.clock.now_ns();
    loop {
        let (next, pruned) = model.dispatch(&now);
        drop(model);
        drop(pruned);
        notify(&mut woken);
        let Some(mut dispatch) = next else {
            return;
        };
        let outcome = run(
            &dispatch.ops.ops,
            dispatch.deadline,
            shared,
            &mut dispatch.lease,
        );
        model = shared.model.lock();
        let (engine, job) = (dispatch.engine, dispatch.job);
        match outcome {
            Outcome::Done => woken = model.complete(engine, job, None),
            Outcome::Fault => woken = model.complete(engine, job, Some(Loss::Fault)),
            Outcome::OutOfMemory => {
                woken = model.complete(engine, job, Some(Loss::OutOfMemory));
            }
            Outcome::Overran => model.reset(engine, now(), &|_| true),
        }
        let lease = core::mem::take(&mut dispatch.lease);
        let given = model.give_back(lease);
        let discarded = model.take_discarded();
        drop(model);
        // The job may have held the last of a memory, so it goes with the
        // lock let go; the next dispatch fits the pool after it.
        drop((dispatch, given, discarded));
        model = shared.model.lock();
    }
}

/// What the software device is on a machine of `total_ram_bytes`.
pub(crate) fn info(total_ram_bytes: u64) -> Result<DeviceInfo, Error> {
    let heap = heap_bytes(total_ram_bytes);
    let mut info = DeviceInfo {
        heaps: ArrayVec::new(),
        memory_types: ArrayVec::new(),
        queue_families: ArrayVec::new(),
        limits: limits(heap),
        capabilities: ArrayVec::new(),
        spirv_version: tairix_spirv::Version::NEWEST,
        features: Features::ALL,
        formats: FormatTable::from_fn(Format::features),
    };
    let stated = tairix_spirv::CAPABILITIES
        .iter()
        .all(|&capability| info.capabilities.try_push(capability).is_ok());
    let filed = stated
        && info
            .heaps
            .try_push(MemoryHeap {
                size: heap,
                device_local: true,
            })
            .is_ok()
        && info
            .memory_types
            .try_push(MemoryType {
                heap: 0,
                properties: MemoryProperties::DEVICE_LOCAL
                    .with(MemoryProperties::HOST_VISIBLE)
                    .with(MemoryProperties::HOST_COHERENT)
                    .with(MemoryProperties::HOST_CACHED),
            })
            .is_ok()
        && info
            .queue_families
            .try_push(QueueFamily {
                capabilities: QueueCapabilities::ALL,
                queues: 1,
                granularity: Granularity::TEXEL,
            })
            .is_ok();
    if filed {
        Ok(info)
    } else {
        Err(Error::Malformed)
    }
}

/// What the software device bounds a request by: its heap, and what a CPU
/// running a frame or a pass at a time can be asked for.
pub(crate) const fn limits(heap: u64) -> Limits {
    Limits {
        max_allocation_size: heap,
        buffer_alignment: BUFFER_ALIGNMENT,
        max_job_time: Duration64::from_secs(MAX_JOB_SECS),
        max_image_dimension_1d: MAX_IMAGE_2D,
        max_image_dimension_2d: MAX_IMAGE_2D,
        max_image_dimension_3d: MAX_IMAGE_3D,
        max_image_array_layers: MAX_LAYERS,
        max_sampler_anisotropy: 16,
        max_sampler_lod_bias: crate::MAX_LOD_BIAS,
        max_bind_sets: 4,
        max_binding_number: crate::MAX_BINDING_NUMBER,
        max_bind_set_descriptors: MAX_BIND_SET_DESCRIPTORS,
        max_push_constant_bytes: 256,
        uniform_buffer_alignment: BUFFER_ALIGNMENT,
        storage_buffer_alignment: BUFFER_ALIGNMENT,
        uniform_buffer_size_alignment: 1,
        storage_buffer_size_alignment: 1,
        max_uniform_buffer_range: 64 << 10,
        max_storage_buffer_range: ADDRESSED,
        max_storage_image_bytes: ADDRESSED,
        max_texel_buffer_elements: 1 << 27,
        max_workgroup_size: [1024, 1024, 64],
        max_workgroup_invocations: 1024,
        max_workgroup_count: [65_535; 3],
        max_workgroup_memory: 32 << 10,
        max_invocation_memory: MAX_INVOCATION_MEMORY,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
        max_vertex_input_attributes: 32,
        max_vertex_input_bindings: 32,
        max_vertex_input_attribute_offset: 4095,
        max_vertex_input_binding_stride: 4096,
        max_vertex_output_components: 128,
        max_fragment_input_components: 128,
        max_color_attachments: 8,
        max_viewport_dimensions: [MAX_IMAGE_2D; 2],
        viewport_bounds_range: VIEWPORT_BOUNDS,
        max_point_size: 256,
        max_line_width: 64,
        // A record is read as its pass reaches it and none is kept, so a
        // count costs time alone, which the deadline bounds.
        max_draw_indirect_count: u32::MAX,
    }
}

/// What a viewport's edges may reach: twice its widest either way.
const VIEWPORT_BOUNDS: [i32; 2] = {
    let reach = MAX_IMAGE_2D.cast_signed() * 2;
    [-reach, reach - 1]
};

/// How a run of a job ended.
pub(crate) enum Outcome {
    Done,
    /// Stopped past its deadline, between two pieces of work: what came
    /// before landed, part of a command included.
    Overran,
    /// A dispatch's counts, read as it ran, passed the device's, or a command
    /// reached past its memory or ran a program the interpreter found broken,
    /// which its checks proved it cannot.
    Fault,
    /// The host refused a table a dispatch or a pass makes as it runs.
    OutOfMemory,
}

/// The work a job, or a piece of one of its dispatches, has done since it
/// last read the clock, whether it has read it, and the flag that stops the
/// job and every piece once the deadline has passed or a piece has faulted.
/// Each piece starts from what the job had counted, and the job goes on from
/// the most any piece that claimed workgroups left uncounted — where one read
/// the clock, counting of the others only what they did after the job's
/// count, which that reading covered — so each piece reads the clock each
/// [`WORK_PER_CHECK`] of its work however the job's work is split into
/// commands. It is read only before work, so a job whose last work is done
/// is never stopped.
pub(crate) struct Meter<'d> {
    pub(crate) stop: &'d AtomicBool,
    pub(crate) clock: &'d dyn MonotonicClock,
    pub(crate) deadline: u64,
    pub(crate) unchecked: u64,
    pub(crate) read: bool,
}

impl Meter<'_> {
    /// Count `work` done, to be checked at the next [`Self::proceed`].
    pub(crate) const fn charge(&mut self, work: u64) {
        self.unchecked = self.unchecked.saturating_add(work);
    }

    /// Count `work` done, and answer whether to go on: the clock is not read
    /// once the job is stopped, as nothing it says then changes that.
    pub(crate) fn proceed(&mut self, work: u64) -> bool {
        self.charge(work);
        if self.stop.load(Ordering::Relaxed) {
            return false;
        }
        if self.unchecked < WORK_PER_CHECK {
            return true;
        }
        self.unchecked = 0;
        self.read = true;
        if self.clock.now_ns() > self.deadline {
            self.stop.store(true, Ordering::Relaxed);
        }
        !self.stop.load(Ordering::Relaxed)
    }
}

/// Run `ops` in order, stopping between two pieces of work once the deadline
/// has passed; dispatches and passes shade in `workspaces`, dispatches' tiles
/// placed in a table the job's dispatches share. Each op looks at the clock before its own
/// work, so one with none, a dispatch of no workgroups over nothing, does
/// not.
pub(crate) fn run(
    ops: &[Op<Software>],
    deadline: u64,
    device: &Shared<Software>,
    workspaces: &mut [dispatch::Workspace],
) -> Outcome {
    let stop = AtomicBool::new(false);
    let mut meter = Meter {
        stop: &stop,
        clock: &*device.clock,
        deadline,
        unchecked: 0,
        read: false,
    };
    // Shared by the job's dispatches, so it is made once.
    let mut places = Vec::new();
    for op in ops {
        let ran = match op {
            Op::Dispatch(work) => Some(dispatch::run(
                work,
                &mut meter,
                device,
                (workspaces, &mut places),
            )),
            Op::Pass(pass) => Some(render::run(pass, &mut meter, device, workspaces)),
            _ => None,
        };
        match ran {
            Some(Outcome::Done) => continue,
            Some(stopped) => return stopped,
            None => {}
        }
        let moved = transfer::apply(op, &device.backend.srgb, &mut meter)
            .unwrap_or_else(|| apply(op, &mut meter).unwrap_or(Moved::Fault));
        match moved {
            Moved::Done => {}
            Moved::Overran => return Outcome::Overran,
            Moved::Fault => return Outcome::Fault,
        }
    }
    Outcome::Done
}

/// The words of a buffer op moved at once between two looks at the clock.
const WORDS_PER_PIECE: usize = (WORK_PER_CHECK / 4) as usize;

/// Each piece of at most [`WORDS_PER_PIECE`] of the `len` words from
/// `start`, in order, counted on `meter` as the bytes it moves: stopped
/// before any piece once the deadline has passed.
fn in_pieces(
    (start, len): (usize, usize),
    meter: &mut Meter<'_>,
    mut piece: impl FnMut(Range<usize>) -> Option<()>,
) -> Option<Moved> {
    let mut at = 0;
    while at < len {
        if !meter.proceed(0) {
            return Some(Moved::Overran);
        }
        let take = WORDS_PER_PIECE.min(len - at);
        piece(start + at..start + at + take)?;
        meter.charge(take as u64 * 4);
        at += take;
    }
    Some(Moved::Done)
}

/// The words `len` bytes at byte `offset` span, both whole words.
fn span(offset: u64, len: u64) -> Option<Range<usize>> {
    if !offset.is_multiple_of(4) || !len.is_multiple_of(4) {
        return None;
    }
    let start = usize::try_from(offset / 4).ok()?;
    let end = start.checked_add(usize::try_from(len / 4).ok()?)?;
    Some(start..end)
}

/// Run a buffer `op` a piece at a time, counting the bytes each moves;
/// `None` where it reaches past its memory, which its checks proved it cannot.
fn apply(op: &Op<Software>, meter: &mut Meter<'_>) -> Option<Moved> {
    match op {
        Op::Copy { src, dst, spans } if Arc::ptr_eq(src, dst) => {
            let mut words = src.lock();
            for copy in spans {
                let from = span(copy.src, copy.len)?;
                let to = span(copy.dst, copy.len)?;
                if from.end > words.len() || to.end > words.len() {
                    return None;
                }
                // A copy within one memory never reads what it writes, so its
                // pieces may go in order.
                let shift = to.start;
                let moved = in_pieces((from.start, from.len()), meter, |piece| {
                    let at = shift + (piece.start - from.start);
                    words.copy_within(piece, at);
                    Some(())
                })?;
                if !matches!(moved, Moved::Done) {
                    return Some(moved);
                }
            }
        }
        Op::Copy { src, dst, spans } => {
            let (from, mut to) = Memory::lock_pair(src, dst);
            for copy in spans {
                let source = span(copy.src, copy.len)?;
                let target = span(copy.dst, copy.len)?;
                let moved = in_pieces((source.start, source.len()), meter, |piece| {
                    let at = target.start + (piece.start - source.start);
                    let len = piece.len();
                    to.get_mut(at..at + len)?.copy_from_slice(from.get(piece)?);
                    Some(())
                })?;
                if !matches!(moved, Moved::Done) {
                    return Some(moved);
                }
            }
        }
        Op::Fill {
            dst,
            offset,
            len,
            word,
        } => {
            let mut words = dst.lock();
            let target = span(*offset, *len)?;
            return in_pieces((target.start, target.len()), meter, |piece| {
                words.get_mut(piece)?.fill(*word);
                Some(())
            });
        }
        Op::Update { dst, offset, data } => {
            if !meter.proceed(0) {
                return Some(Moved::Overran);
            }
            let mut words = dst.lock();
            let target = words.get_mut(span(*offset, data.bytes.len() as u64)?)?;
            let (whole, rest) = data.bytes.as_chunks::<4>();
            if !rest.is_empty() {
                return None;
            }
            for (word, bytes) in target.iter_mut().zip(whole) {
                *word = u32::from_le_bytes(*bytes);
            }
            meter.charge(data.bytes.len() as u64);
        }
        Op::BufferImage { .. }
        | Op::CopyImage { .. }
        | Op::ClearImage { .. }
        | Op::Dispatch(_)
        | Op::Pass(_) => return None,
    }
    Some(Moved::Done)
}

/// How many of this thread's waits it made unflagged as parked, so a test
/// can see that a poke during any of them would have woken it.
#[cfg(test)]
mod waits {
    use core::cell::Cell;

    std::thread_local! {
        static UNFLAGGED: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn note(parked: bool) {
        if !parked {
            UNFLAGGED.with(|unflagged| unflagged.set(unflagged.get() + 1));
        }
    }

    pub(super) fn unflagged() -> usize {
        UNFLAGGED.with(Cell::get)
    }
}

#[cfg(test)]
#[path = "software/wake_tests.rs"]
mod wake_tests;

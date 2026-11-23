//! What a driver implements to serve its own device through the model: a
//! [`Hardware`] backend, and the [`Job`]s its engines run.

use alloc::sync::{Arc, Weak};
use core::convert::Infallible;
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_rt::sync::Mutex;

use crate::backend::{Backend, NoPool, Prepared};
use crate::binding::Bound;
use crate::command::{CopySpan, Op};
use crate::device::{notify, Device, Shared};
use crate::fair::{Fair, FairGuard};
use crate::format::{Format, FormatFeatures};
use crate::image::ImageDesc;
use crate::info::{DeviceInfo, Features, QueueCapabilities};
use crate::memory::Charge;
use crate::model::{Dispatch, Model};
use crate::pipeline::StagePlan;
use crate::render::RenderPlan;
use crate::resource::MemoryRequirements;
use crate::{Error, Loss};

/// A device a driver runs work on: what a context is on it, memory made in a
/// context, and the jobs its engines run. The model checks, charges, files
/// and orders everything; the backend makes what the device holds and runs
/// what is ready.
///
/// What it makes it may wait on its device for: the model never holds its
/// lock across a call. No device here can stop a job once started, so the
/// model's watchdog answers one past its deadline as overran, losing its
/// context, and [`lost`](Self::lost) tells the backend so.
pub trait Hardware: Send + Sync + Sized + 'static {
    /// A context as the device holds it, let go once the model's context and
    /// the last of its memory and jobs are.
    type Context: Send + Sync + 'static;
    /// Memory made in a context, holding the context while it lives.
    type Memory: fmt::Debug + Send + Sync + 'static;

    /// A context, as the model makes one.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot hold another.
    fn create_context(&self) -> Result<Self::Context, Error>;

    /// `size` zeroed bytes of memory type `memory_type` in `context`.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot have the memory.
    fn allocate(
        &self,
        context: &Arc<Self::Context>,
        memory_type: u32,
        size: u64,
    ) -> Result<Self::Memory, Error>;

    /// Write `data` at `offset` of host-visible memory, the range proved
    /// within it.
    ///
    /// # Errors
    ///
    /// Why the device cannot reach it.
    fn write(&self, memory: &Self::Memory, offset: u64, data: &[u8]) -> Result<(), Error>;

    /// Read `out.len()` bytes at `offset` of host-visible memory, the range
    /// proved within it.
    ///
    /// # Errors
    ///
    /// Why the device cannot reach it.
    fn read(&self, memory: &Self::Memory, offset: u64, out: &mut [u8]) -> Result<(), Error>;

    /// Start `job`, answering it with [`Job::finish`] once the device has
    /// run it. Called without the model's lock, from whatever made the job
    /// ready — another job's [`Job::finish`] among them — and never waits.
    fn run(&self, job: Job<Self>);

    /// The host bytes running `commands` makes beyond what they hold, charged
    /// to the job's context while it is queued and running.
    fn run_bytes(commands: Commands<'_, Self>) -> u64;

    /// `context` was lost: let it go once the device answers its work.
    fn lost(&self, context: &Arc<Self::Context>);
}

impl Device {
    /// A device a driver serves through `hardware`, as `info` describes it,
    /// family `i` running `depths[i]` jobs at once, its deadlines read from
    /// `clock`.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for a description past the seam's bounds, one
    /// promising more than memory and transfers, or a depth for no family or
    /// of none, and [`Error::OutOfMemory`] where the model cannot be held.
    pub fn hardware<H: Hardware>(
        hardware: H,
        info: DeviceInfo,
        depths: &[usize],
        clock: Arc<dyn MonotonicClock + Send + Sync>,
    ) -> Result<Self, Error> {
        if !transfers_only(&info) {
            return Err(Error::Malformed);
        }
        let model = Model::new(info.clone(), depths, NoPool)?;
        let shared = crate::shared(Shared {
            model: Fair::new(model),
            info,
            clock,
            backend: Driven {
                hardware,
                device: Mutex::new(Weak::new()),
            },
        })?;
        *shared.backend.device.lock() = Arc::downgrade(&shared);
        Ok(Self::local(shared))
    }
}

/// Whether `info` states only what the seam carries: no format, shader
/// capability or feature, and queues running transfers alone, so it promises
/// no image or pipeline a hardware backend could not make.
fn transfers_only(info: &DeviceInfo) -> bool {
    info.capabilities.is_empty()
        && info.features == Features::NONE
        && Format::ALL
            .iter()
            .all(|&format| info.formats.get(format) == FormatFeatures::NONE)
        && info
            .queue_families
            .iter()
            .all(|family| family.capabilities == QueueCapabilities::TRANSFER)
}

/// A hardware backend as the model drives it.
pub(crate) struct Driven<H: Hardware> {
    hardware: H,
    /// The device it is the backend of, which each job it starts answers.
    device: Mutex<Weak<Shared<Self>>>,
}

/// Memory a hardware backend made, and the charges paying for it.
pub(crate) struct Held<M> {
    memory: M,
    _charges: [Charge; 2],
}

impl<M: fmt::Debug> fmt::Debug for Held<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.memory.fmt(f)
    }
}

impl<H: Hardware> Backend for Driven<H> {
    type Context = H::Context;
    type Memory = Held<H::Memory>;
    // Its device states no format, capability or queue a use of these needs.
    type Image = Infallible;
    type Set = Infallible;
    type Compute = Infallible;
    type Render = Infallible;
    type Pool = NoPool;

    const WATCHED: bool = true;

    fn overhead(_: u64) -> u64 {
        0
    }

    fn create_context(&self) -> Result<H::Context, Error> {
        self.hardware.create_context()
    }

    fn allocate(
        &self,
        context: &Arc<H::Context>,
        (memory_type, size): (u32, u64),
        charges: [Charge; 2],
    ) -> Result<Held<H::Memory>, Error> {
        let memory = self.hardware.allocate(context, memory_type, size)?;
        Ok(Held {
            memory,
            _charges: charges,
        })
    }

    fn lost(&self, context: &Arc<H::Context>) {
        self.hardware.lost(context);
    }

    fn write(&self, memory: &Held<H::Memory>, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.hardware.write(&memory.memory, offset, data)
    }

    fn read(&self, memory: &Held<H::Memory>, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        self.hardware.read(&memory.memory, offset, out)
    }

    fn image(
        &self,
        _: &DeviceInfo,
        _: &ImageDesc,
    ) -> Result<(Infallible, MemoryRequirements), Error> {
        Err(Error::Unsupported)
    }

    fn set(&self, _: &[Bound<Self>]) -> Result<(Infallible, u64), Error> {
        Err(Error::Unsupported)
    }

    fn compute(
        &self,
        _: &DeviceInfo,
        _: &StagePlan<'_>,
    ) -> Result<Prepared<Self, Infallible>, Error> {
        Err(Error::Unsupported)
    }

    fn render(
        &self,
        _: &DeviceInfo,
        _: &RenderPlan<'_>,
    ) -> Result<Prepared<Self, Infallible>, Error> {
        Err(Error::Unsupported)
    }

    fn run_bytes(_: &NoPool, ops: &[Op<Self>]) -> u64 {
        H::run_bytes(Commands { ops })
    }

    /// Each ready job is taken under the lock and started without it.
    fn drive(shared: &Shared<Self>, mut model: FairGuard<'_, Model<Self>>) {
        let device = shared.backend.device.lock().clone();
        let now = || shared.clock.now_ns();
        let (mut next, ()) = model.dispatch(&now);
        drop(model);
        while let Some(dispatch) = next {
            shared.backend.hardware.run(Job {
                device: device.clone(),
                dispatch,
                answered: false,
            });
            (next, ()) = shared.model.lock().dispatch(&now);
        }
    }
}

/// A job an engine started: the commands it runs, the context and queue it
/// runs in, and what answers it. Dropped unanswered, it loses its context as
/// a fault.
pub struct Job<H: Hardware> {
    device: Weak<Shared<Driven<H>>>,
    dispatch: Dispatch<Driven<H>>,
    answered: bool,
}

impl<H: Hardware> Job<H> {
    /// The context it runs in.
    #[must_use]
    pub fn context(&self) -> &Arc<H::Context> {
        &self.dispatch.context
    }

    /// The queue of its context it was submitted to: an index over every
    /// family's queues in turn, as [`DeviceInfo`] lists them.
    #[must_use]
    pub const fn queue(&self) -> usize {
        self.dispatch.queue
    }

    /// When on the device's clock its deadline falls.
    #[must_use]
    pub const fn deadline(&self) -> u64 {
        self.dispatch.deadline
    }

    /// Its commands, in order.
    #[must_use]
    pub fn commands(&self) -> Commands<'_, H> {
        Commands {
            ops: &self.dispatch.ops.ops,
        }
    }

    /// Answer it: run to its end, or ended losing its context for a loss.
    /// The work it readies may start from within this call.
    pub fn finish(mut self, outcome: Result<(), Loss>) {
        self.answer(outcome.err());
    }

    fn answer(&mut self, lost: Option<Loss>) {
        if core::mem::replace(&mut self.answered, true) {
            return;
        }
        let Some(shared) = self.device.upgrade() else {
            return;
        };
        let mut model = shared.model.lock();
        let mut woken = model.complete(self.dispatch.engine, self.dispatch.job, lost);
        let given = model.give_back(());
        let discarded = model.take_discarded();
        shared.drive(model);
        notify(&mut woken);
        // Without the device's lock, as is the job's own work when it goes:
        // any may hold the last of a memory.
        drop((given, discarded));
    }
}

impl<H: Hardware> Drop for Job<H> {
    fn drop(&mut self) {
        self.answer(Some(Loss::Fault));
    }
}

impl<H: Hardware> fmt::Debug for Job<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("queue", &self.queue())
            .field("commands", &self.commands().iter().count())
            .finish_non_exhaustive()
    }
}

/// The commands of a job, in order.
pub struct Commands<'a, H: Hardware> {
    ops: &'a [Op<Driven<H>>],
}

impl<H: Hardware> Clone for Commands<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for Commands<'_, H> {}

impl<'a, H: Hardware> Commands<'a, H> {
    /// Each in turn.
    pub fn iter(&self) -> impl Iterator<Item = JobCommand<'a, H::Memory>> + 'a {
        self.ops.iter().filter_map(command)
    }
}

/// One command of a job, every range proved within the memories it names.
#[derive(Debug)]
pub enum JobCommand<'a, M> {
    /// Copy each span from `src` to `dst`, its offsets into the memories.
    Copy {
        /// What is read.
        src: &'a M,
        /// What is written.
        dst: &'a M,
        /// Its spans: no byte written twice, nor, within one memory, both
        /// read and written.
        spans: &'a [CopySpan],
    },
    /// Fill `size` bytes of `dst` at `offset`, both whole words, with `word`
    /// in little-endian byte order.
    Fill {
        /// What is written.
        dst: &'a M,
        /// Where.
        offset: u64,
        /// How many bytes.
        size: u64,
        /// What each word becomes.
        word: u32,
    },
    /// Write `data`, whole words, into `dst` at `offset`.
    Update {
        /// What is written.
        dst: &'a M,
        /// Where.
        offset: u64,
        /// What.
        data: &'a [u8],
    },
}

impl<M> Clone for JobCommand<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for JobCommand<'_, M> {}

/// `op` as the backend runs it. The image commands and a dispatch name an
/// image or pipeline a hardware backend never makes; a pass can then hold no
/// attachment and no draw, so it does nothing.
fn command<H: Hardware>(op: &Op<Driven<H>>) -> Option<JobCommand<'_, H::Memory>> {
    Some(match op {
        Op::Copy { src, dst, spans } => JobCommand::Copy {
            src: &src.memory,
            dst: &dst.memory,
            spans,
        },
        &Op::Fill {
            ref dst,
            offset,
            len,
            word,
        } => JobCommand::Fill {
            dst: &dst.memory,
            offset,
            size: len,
            word,
        },
        Op::Update { dst, offset, data } => JobCommand::Update {
            dst: &dst.memory,
            offset: *offset,
            data: &data.bytes,
        },
        Op::BufferImage { image, .. } | Op::ClearImage { image, .. } => match image.form {},
        Op::CopyImage { src, .. } => match src.form {},
        Op::Dispatch(work) => match work.pipeline.stages {},
        Op::Pass(pass) => {
            if let Some(target) = pass.colour.first() {
                match target.view.image.form {}
            }
            if let Some(depth) = &pass.depth_stencil {
                match depth.view.image.form {}
            }
            if let Some(draw) = pass.draws.first() {
                match draw.pipeline.stages.form {}
            }
            return None;
        }
    })
}

#[cfg(test)]
#[path = "hardware_tests.rs"]
mod tests;

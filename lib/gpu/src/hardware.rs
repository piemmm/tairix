//! What a driver implements to serve its own device through the model: a
//! [`Hardware`] backend, and the [`Job`]s its engines run.

use alloc::sync::{Arc, Weak};
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_rt::sync::Mutex;

use tairix_spirv::{Admitted, Specialized};

use crate::backend::{Backend, NoPool, Prepared};
use crate::binding::{BindSetLayoutState, BindSetState, Bound, LayoutBinding, PushConstants};
use crate::command::{CopySpan, Groups, Op};
use crate::device::{notify, Device, Shared};
use crate::fair::{Fair, FairGuard};
use crate::format::Format;
use crate::image::{
    BufferImageCopy, BufferViewDesc, ClearValue, ImageCopy, ImageDesc, ImageRange, ImageViewDesc,
};
use crate::info::DeviceInfo;
use crate::memory::Charge;
use crate::model::{Dispatch, Model};
use crate::pass::{
    Counted, DepthTarget, DrawCounts, DrawWork, IndexFormat, Load, Rect, Source, Target, Viewport,
};
use crate::pipeline::StagePlan;
use crate::render::{
    ColourMask, ColourTarget, DepthStencilState, LogicOp, Multisample, Rasterization, RenderPlan,
    Topology, VertexBuffer,
};
use crate::resource::MemoryRequirements;
use crate::sampler::SamplerDesc;
use crate::transfer::Direction;
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
    /// An image made in a context, let go once the last of the image, the
    /// views and sets reaching it and the work naming it is.
    type Image: fmt::Debug + Send + Sync + 'static;
    /// A view of an image as the device reads it, let go once the last of
    /// the view and the sets and work reaching it is.
    type View: fmt::Debug + Send + Sync + 'static;
    /// A texel buffer as the device reads it, let go as a view is.
    type BufferView: fmt::Debug + Send + Sync + 'static;
    /// A sampler as the device samples with it, let go as a view is.
    type Sampler: fmt::Debug + Send + Sync + 'static;
    /// A bind set's descriptors as the device reads them, let go once the
    /// last of the set and the work naming it is.
    type Set: fmt::Debug + Send + Sync + 'static;
    /// A compute pipeline as the device runs it, let go once the last of the
    /// pipeline and the work naming it is.
    type Compute: fmt::Debug + Send + Sync + 'static;
    /// A render pipeline as the device runs it, let go as a compute one is.
    type Render: fmt::Debug + Send + Sync + 'static;

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

    /// An image in `context` of `desc`, which its device's description
    /// admits, and what it needs of the memory it is placed in.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot hold another, and
    /// [`Error::Unsupported`] for one it cannot make.
    fn image(
        &self,
        context: &Arc<Self::Context>,
        desc: &ImageDesc,
    ) -> Result<(Self::Image, MemoryRequirements), Error>;

    /// Place `image` at `offset` into `memory`, where it was proved to meet
    /// its requirements, answering once every texel of it reads zero.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot place it.
    fn place(&self, image: &Self::Image, memory: &Self::Memory, offset: u64) -> Result<(), Error>;

    /// A view of `desc` of `image`, an image of `of` it was proved to see.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot hold another, and
    /// [`Error::Unsupported`] for one it cannot make.
    fn view(
        &self,
        image: &Self::Image,
        of: &ImageDesc,
        desc: &ImageViewDesc,
    ) -> Result<Self::View, Error>;

    /// A texel buffer of `desc` at `offset` into `memory`, proved within it.
    ///
    /// # Errors
    ///
    /// As [`Self::view`].
    fn buffer_view(
        &self,
        memory: &Self::Memory,
        offset: u64,
        desc: &BufferViewDesc,
    ) -> Result<Self::BufferView, Error>;

    /// A sampler in `context` of `desc`, which its device's description
    /// admits.
    ///
    /// # Errors
    ///
    /// As [`Self::view`].
    fn sampler(
        &self,
        context: &Arc<Self::Context>,
        desc: &SamplerDesc,
    ) -> Result<Self::Sampler, Error>;

    /// A bind set in `context`: a descriptor of each of `resources` for each
    /// of `layout`'s, in its order, every range proved within its memory and
    /// every resource of the type its binding states.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot hold it.
    fn set(
        &self,
        context: &Arc<Self::Context>,
        layout: &[LayoutBinding],
        resources: &[SetResource<'_, Self>],
    ) -> Result<Self::Set, Error>;

    /// A compute pipeline in `context` of the entry point `stage` names,
    /// agreed with its layout and specialized.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] where the device cannot hold it, and
    /// [`Error::TooLarge`] for a program past what it can carry.
    fn compute(
        &self,
        context: &Arc<Self::Context>,
        stage: PipelineStage<'_>,
    ) -> Result<Self::Compute, Error>;

    /// A render pipeline in `context` of `stages`, each agreed with its
    /// layout and specialized, and the state its draws run through.
    ///
    /// # Errors
    ///
    /// As [`Self::compute`].
    fn render(
        &self,
        context: &Arc<Self::Context>,
        stages: RenderStages<'_>,
    ) -> Result<Self::Render, Error>;

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
    /// [`Error::Malformed`] for a description past the seam's bounds or a
    /// depth for no family or of none, and [`Error::OutOfMemory`] where the
    /// model cannot be held.
    pub fn hardware<H: Hardware>(
        hardware: H,
        info: DeviceInfo,
        depths: &[usize],
        clock: Arc<dyn MonotonicClock + Send + Sync>,
    ) -> Result<Self, Error> {
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
    type Image = H::Image;
    type View = H::View;
    type BufferView = H::BufferView;
    type Sampler = H::Sampler;
    type Set = H::Set;
    type Compute = H::Compute;
    type Render = H::Render;
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
        context: &Arc<H::Context>,
        _: &DeviceInfo,
        desc: &ImageDesc,
    ) -> Result<(H::Image, MemoryRequirements), Error> {
        self.hardware.image(context, desc)
    }

    fn place(
        &self,
        image: &H::Image,
        memory: &Held<H::Memory>,
        offset: u64,
        _: u64,
    ) -> Result<(), Error> {
        self.hardware.place(image, &memory.memory, offset)
    }

    fn view(
        &self,
        image: &H::Image,
        of: &ImageDesc,
        desc: &ImageViewDesc,
    ) -> Result<H::View, Error> {
        self.hardware.view(image, of, desc)
    }

    fn buffer_view(
        &self,
        memory: &Held<H::Memory>,
        offset: u64,
        desc: &BufferViewDesc,
    ) -> Result<H::BufferView, Error> {
        self.hardware.buffer_view(&memory.memory, offset, desc)
    }

    fn sampler(&self, context: &Arc<H::Context>, desc: &SamplerDesc) -> Result<H::Sampler, Error> {
        self.hardware.sampler(context, desc)
    }

    fn set(
        &self,
        context: &Arc<H::Context>,
        layout: &BindSetLayoutState,
        resources: &[Bound<Self>],
    ) -> Result<(H::Set, u64), Error> {
        let mut held = alloc::vec::Vec::new();
        held.try_reserve_exact(resources.len())
            .map_err(|_| Error::OutOfMemory)?;
        for resource in resources {
            held.push(match resource {
                Bound::Buffer {
                    buffer,
                    offset,
                    size,
                } => SetResource::Buffer {
                    memory: &buffer.memory.memory,
                    offset: buffer.offset + offset,
                    size: *size,
                },
                Bound::Sampler(sampler) => SetResource::Sampler(&sampler.form),
                Bound::Image(view) => SetResource::Image(&view.form),
                Bound::ImageSampler(view, sampler) => {
                    SetResource::ImageSampler(&view.form, &sampler.form)
                }
                Bound::TexelBuffer(texels) => SetResource::TexelBuffer(&texels.form),
            });
        }
        // Held inline in the set, whose own size is charged.
        Ok((self.hardware.set(context, &layout.bindings, &held)?, 0))
    }

    fn compute(
        &self,
        context: &Arc<H::Context>,
        _: &DeviceInfo,
        stage: &StagePlan<'_>,
    ) -> Result<Prepared<Self, H::Compute>, Error> {
        Ok(Prepared {
            form: self
                .hardware
                .compute(context, PipelineStage { plan: stage })?,
            need: (),
            held: 0,
        })
    }

    fn render(
        &self,
        context: &Arc<H::Context>,
        _: &DeviceInfo,
        plan: &RenderPlan<'_>,
    ) -> Result<Prepared<Self, H::Render>, Error> {
        Ok(Prepared {
            form: self.hardware.render(context, RenderStages { plan })?,
            need: (),
            held: 0,
        })
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
    pub fn iter(&self) -> impl Iterator<Item = JobCommand<'a, H>> + 'a {
        self.ops.iter().map(command)
    }
}

/// One command of a job, every range proved within the memories it names.
pub enum JobCommand<'a, H: Hardware> {
    /// Copy each span from `src` to `dst`, its offsets into the memories.
    Copy {
        /// What is read.
        src: &'a H::Memory,
        /// What is written.
        dst: &'a H::Memory,
        /// Its spans: no byte written twice, nor, within one memory, both
        /// read and written.
        spans: &'a [CopySpan],
    },
    /// Fill `size` bytes of `dst` at `offset`, both whole words, with `word`
    /// in little-endian byte order.
    Fill {
        /// What is written.
        dst: &'a H::Memory,
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
        dst: &'a H::Memory,
        /// Where.
        offset: u64,
        /// What.
        data: &'a [u8],
    },
    /// Copy each region between `image` and the buffer at `offset` into
    /// `memory`, a region's buffer offset from the buffer's start.
    BufferImage {
        /// The buffer's memory.
        memory: &'a H::Memory,
        /// Where the buffer starts in it.
        offset: u64,
        /// The image.
        image: &'a H::Image,
        /// The regions, each within the image and the buffer.
        regions: &'a [BufferImageCopy],
        /// Which way the texels move.
        direction: Direction,
    },
    /// Copy each region from `src` to `dst`, images of texels of one size.
    CopyImage {
        /// What is read.
        src: &'a H::Image,
        /// What is written.
        dst: &'a H::Image,
        /// The regions, each within both images.
        regions: &'a [ImageCopy],
    },
    /// Set every texel of each range of `image` to `value`, of the kind its
    /// format holds.
    ClearImage {
        /// What is cleared.
        image: &'a H::Image,
        /// What its texels become.
        value: ClearValue,
        /// Its parts, each within it.
        ranges: &'a [ImageRange],
    },
    /// Run `counts` workgroups of `pipeline` with `sets` bound set by set
    /// and `push` as its push constants: each set of the layout's set's
    /// bindings and the push constants exactly the layout's.
    Dispatch {
        /// The pipeline.
        pipeline: &'a H::Compute,
        /// Its bind sets, set by set.
        sets: Sets<'a, H>,
        /// Its push constants' words.
        push: &'a [u32],
        /// How many workgroups it makes.
        counts: Counts<'a, H::Memory>,
    },
    /// Render `draws` in order into the pass's targets within `area`, each
    /// target loaded as the pass says and stored, and resolved as it ends.
    Pass {
        /// Its colour targets, by location.
        targets: Targets<'a, H>,
        /// Its depth-stencil target.
        depth_stencil: Option<PassDepthStencil<'a, H>>,
        /// The pixels it touches.
        area: Rect,
        /// Its draws, in order.
        draws: Draws<'a, H>,
    },
}

impl<H: Hardware> Clone for JobCommand<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for JobCommand<'_, H> {}

impl<H: Hardware> fmt::Debug for JobCommand<'_, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copy { src, dst, spans } => f
                .debug_struct("Copy")
                .field("src", src)
                .field("dst", dst)
                .field("spans", spans)
                .finish(),
            Self::Fill {
                dst,
                offset,
                size,
                word,
            } => f
                .debug_struct("Fill")
                .field("dst", dst)
                .field("offset", offset)
                .field("size", size)
                .field("word", word)
                .finish(),
            Self::Update { dst, offset, data } => f
                .debug_struct("Update")
                .field("dst", dst)
                .field("offset", offset)
                .field("bytes", &data.len())
                .finish(),
            Self::BufferImage {
                memory,
                offset,
                image,
                regions,
                direction,
            } => f
                .debug_struct("BufferImage")
                .field("memory", memory)
                .field("offset", offset)
                .field("image", image)
                .field("regions", &regions.len())
                .field("direction", direction)
                .finish(),
            Self::CopyImage { src, dst, regions } => f
                .debug_struct("CopyImage")
                .field("src", src)
                .field("dst", dst)
                .field("regions", &regions.len())
                .finish(),
            Self::ClearImage {
                image,
                value,
                ranges,
            } => f
                .debug_struct("ClearImage")
                .field("image", image)
                .field("value", value)
                .field("ranges", &ranges.len())
                .finish(),
            Self::Dispatch {
                pipeline,
                sets,
                push,
                counts,
            } => f
                .debug_struct("Dispatch")
                .field("pipeline", pipeline)
                .field("sets", &sets.len())
                .field("push", &push.len())
                .field("counts", counts)
                .finish(),
            Self::Pass {
                targets,
                depth_stencil,
                area,
                draws,
            } => f
                .debug_struct("Pass")
                .field("targets", &targets.len())
                .field("depth_stencil", &depth_stencil.is_some())
                .field("area", area)
                .field("draws", &draws.len())
                .finish(),
        }
    }
}

/// How many workgroups a dispatch makes: given, or three little-endian words
/// at `offset` into `memory` read as the dispatch runs, where counts past the
/// device's lose the context.
#[derive(Debug)]
pub enum Counts<'a, M> {
    /// Along each axis.
    Direct([u32; 3]),
    /// Read as it runs.
    Indirect {
        /// What holds them.
        memory: &'a M,
        /// Where, into the memory: a whole word, the words within it.
        offset: u64,
    },
}

impl<M> Clone for Counts<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for Counts<'_, M> {}

/// A dispatch's bind sets, set by set.
pub struct Sets<'a, H: Hardware> {
    sets: &'a [Arc<BindSetState<Driven<H>>>],
}

impl<H: Hardware> Clone for Sets<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for Sets<'_, H> {}

impl<'a, H: Hardware> Sets<'a, H> {
    /// How many.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.sets.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// Each in turn, as its device reads it.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'a H::Set> + 'a {
        self.sets.iter().map(|set| &set.form)
    }
}

/// One descriptor a bind set is made of.
pub enum SetResource<'a, H: Hardware> {
    /// `size` bytes at `offset` into `memory`, for a uniform or storage
    /// buffer binding.
    Buffer {
        /// What holds them.
        memory: &'a H::Memory,
        /// Where they start, into the memory.
        offset: u64,
        /// How many there are.
        size: u64,
    },
    /// A sampler, for a sampler binding.
    Sampler(&'a H::Sampler),
    /// A view, for a sampled or storage image binding.
    Image(&'a H::View),
    /// A view and the sampler it is sampled with, for a combined binding.
    ImageSampler(&'a H::View, &'a H::Sampler),
    /// A texel buffer, for a uniform or storage texel buffer binding.
    TexelBuffer(&'a H::BufferView),
}

impl<H: Hardware> fmt::Debug for SetResource<'_, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buffer {
                memory,
                offset,
                size,
            } => f
                .debug_struct("Buffer")
                .field("memory", memory)
                .field("offset", offset)
                .field("size", size)
                .finish(),
            Self::Sampler(sampler) => f.debug_tuple("Sampler").field(sampler).finish(),
            Self::Image(view) => f.debug_tuple("Image").field(view).finish(),
            Self::ImageSampler(view, sampler) => f
                .debug_tuple("ImageSampler")
                .field(view)
                .field(sampler)
                .finish(),
            Self::TexelBuffer(texels) => f.debug_tuple("TexelBuffer").field(texels).finish(),
        }
    }
}

/// One stage of a pipeline: an entry point of an admitted module, agreed with
/// its pipeline's layout, and specialized.
#[derive(Copy, Clone)]
pub struct PipelineStage<'a> {
    plan: &'a StagePlan<'a>,
}

impl<'a> PipelineStage<'a> {
    /// The module holding it.
    #[must_use]
    pub fn module(&self) -> &'a Admitted {
        self.plan.module
    }

    /// Every constant of the module, evaluated with its specialization's
    /// values.
    #[must_use]
    pub fn specialized(&self) -> &'a Specialized {
        &self.plan.specialized
    }

    /// Its name.
    #[must_use]
    pub fn entry_point(&self) -> &'a str {
        self.plan.entry.name()
    }

    /// The bindings of each bind set layout of its pipeline's layout, set by
    /// set.
    #[must_use]
    pub fn sets(&self) -> impl ExactSizeIterator<Item = &'a [LayoutBinding]> + 'a {
        self.plan
            .layout
            .sets
            .iter()
            .map(|set| set.bindings.as_slice())
    }

    /// Its pipeline layout's push constants.
    #[must_use]
    pub fn push_constants(&self) -> PushConstants {
        self.plan.layout.push_constants
    }
}

impl fmt::Debug for PipelineStage<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipelineStage")
            .field("entry_point", &self.entry_point())
            .finish_non_exhaustive()
    }
}

/// A render pipeline's stages, each agreed with its pipeline's layout and
/// specialized, and the state its draws run through, checked against what
/// its device states.
#[derive(Copy, Clone)]
pub struct RenderStages<'a> {
    plan: &'a RenderPlan<'a>,
}

impl<'a> RenderStages<'a> {
    /// Its vertex stage.
    #[must_use]
    pub const fn vertex(&self) -> PipelineStage<'a> {
        PipelineStage {
            plan: &self.plan.vertex,
        }
    }

    /// Its fragment stage, where it has one.
    #[must_use]
    pub fn fragment(&self) -> Option<PipelineStage<'a>> {
        self.plan
            .fragment
            .as_ref()
            .map(|plan| PipelineStage { plan })
    }

    /// The vertex buffers a draw binds, in order.
    #[must_use]
    pub const fn vertex_buffers(&self) -> &'a [VertexBuffer<'a>] {
        self.plan.state.vertex_buffers
    }

    /// What its draws' vertices make.
    #[must_use]
    pub const fn topology(&self) -> Topology {
        self.plan.state.topology
    }

    /// Whether an index of all ones restarts a strip or fan.
    #[must_use]
    pub const fn primitive_restart(&self) -> bool {
        self.plan.state.primitive_restart
    }

    /// How its primitives are rasterised.
    #[must_use]
    pub const fn rasterization(&self) -> Rasterization {
        self.plan.state.rasterization
    }

    /// Its depth-stencil target and tests, where it has one.
    #[must_use]
    pub const fn depth_stencil(&self) -> Option<DepthStencilState> {
        self.plan.state.depth_stencil
    }

    /// Its samples.
    #[must_use]
    pub const fn multisample(&self) -> Multisample {
        self.plan.state.multisample
    }

    /// Its colour targets, by location.
    #[must_use]
    pub const fn targets(&self) -> &'a [ColourTarget] {
        self.plan.state.targets
    }

    /// The channels a draw writes of target `at`: those its write mask names
    /// that the fragment stage writes. A channel outside them keeps what it
    /// held.
    #[must_use]
    pub fn writes(&self, at: usize) -> ColourMask {
        let written = self.plan.channels.get(at).copied().unwrap_or(0);
        let mask = self
            .plan
            .state
            .targets
            .get(at)
            .map_or(0, |target| target.write_mask.bits());
        ColourMask::from_bits(written & mask)
    }

    /// The logic operation in place of blending, where it has one.
    #[must_use]
    pub const fn logic_op(&self) -> Option<LogicOp> {
        self.plan.state.logic_op
    }
}

impl fmt::Debug for RenderStages<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderStages")
            .field("vertex", &self.vertex())
            .field("fragment", &self.fragment())
            .finish_non_exhaustive()
    }
}

/// A pass's colour targets, by location.
pub struct Targets<'a, H: Hardware> {
    targets: &'a [Target<Driven<H>>],
}

impl<H: Hardware> Clone for Targets<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for Targets<'_, H> {}

impl<'a, H: Hardware> Targets<'a, H> {
    /// How many.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Each in turn.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = PassTarget<'a, H>> + 'a {
        self.targets.iter().map(|target| PassTarget {
            view: &target.view.form,
            format: target.view.desc.format,
            resolve: target.resolve.as_ref().map(|view| &view.form),
            load: target.load,
        })
    }
}

/// A colour target of a pass: a view of one level and layer, and of one
/// sample or four.
pub struct PassTarget<'a, H: Hardware> {
    /// What it renders to.
    pub view: &'a H::View,
    /// The view's format.
    pub format: Format,
    /// A one-sample view of the same format and extent its four samples are
    /// averaged into as the pass ends.
    pub resolve: Option<&'a H::View>,
    /// What it holds as the pass begins, a clear of its format's kind.
    pub load: Load,
}

/// A pass's depth-stencil target: a view of one level and layer, and how each
/// aspect its format holds is loaded.
pub struct PassDepthStencil<'a, H: Hardware> {
    /// What it renders to.
    pub view: &'a H::View,
    /// The view's format, which names the aspects it holds.
    pub format: Format,
    /// What its depth holds as the pass begins.
    pub depth: Load,
    /// What its stencil holds as the pass begins.
    pub stencil: Load,
}

impl<H: Hardware> Clone for PassDepthStencil<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for PassDepthStencil<'_, H> {}

impl<H: Hardware> Clone for PassTarget<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for PassTarget<'_, H> {}

impl<H: Hardware> Clone for DrawCommand<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for DrawCommand<'_, H> {}

/// A pass's draws, in order.
pub struct Draws<'a, H: Hardware> {
    draws: &'a [DrawWork<Driven<H>>],
    area: Rect,
}

impl<H: Hardware> Clone for Draws<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for Draws<'_, H> {}

impl<'a, H: Hardware> Draws<'a, H> {
    /// How many.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.draws.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// Each in turn.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = DrawCommand<'a, H>> + Clone + 'a {
        let area = self.area;
        self.draws.iter().map(move |work| DrawCommand {
            pipeline: &work.pipeline.stages.form,
            sets: Sets { sets: &work.sets },
            push: work.push.as_deref().map_or(&[], alloc::vec::Vec::as_slice),
            vertex_buffers: Sources {
                sources: &work.vertex_buffers,
            },
            index: work
                .index
                .as_ref()
                .map(|(source, format)| (range(source), *format)),
            viewport: work.viewport,
            scissor: work.scissor.within(area),
            stencil_reference: work.stencil_reference,
            blend_constant: work.blend_constant,
            count: match &work.count {
                Counted::Direct(counts) => DrawCount::Direct(*counts),
                Counted::Indirect(records) => DrawCount::Indirect {
                    memory: &records.memory.memory,
                    offset: records.start,
                    count: records.count,
                    stride: records.stride,
                },
            },
        })
    }
}

/// One draw of a pass, every range proved within the memories it names: its
/// pipeline renders to the pass's targets, and its bind sets and push
/// constants are exactly its layout's.
pub struct DrawCommand<'a, H: Hardware> {
    /// Its pipeline.
    pub pipeline: &'a H::Render,
    /// Its bind sets, set by set.
    pub sets: Sets<'a, H>,
    /// Its push constants' words.
    pub push: &'a [u32],
    /// What each vertex buffer its pipeline reads binds, in order.
    pub vertex_buffers: Sources<'a, H>,
    /// Where its indices lie and how wide each is, for an indexed draw.
    pub index: Option<(BufferRange<'a, H::Memory>, IndexFormat)>,
    /// Its viewport.
    pub viewport: Viewport,
    /// The pixels it may touch: its scissor within its pass's area.
    pub scissor: Rect,
    /// The value its stencil test compares with and writes.
    pub stencil_reference: u8,
    /// The constant its blend factors read.
    pub blend_constant: [f32; 4],
    /// How many vertices and instances it makes.
    pub count: DrawCount<'a, H::Memory>,
}

/// The bytes a vertex or index buffer binds: `size` of them from `offset`
/// into `memory`, a read past which reads zero.
#[derive(Debug)]
pub struct BufferRange<'a, M> {
    /// What holds them.
    pub memory: &'a M,
    /// Where they start, into the memory.
    pub offset: u64,
    /// How many there are.
    pub size: u64,
}

impl<M> Clone for BufferRange<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for BufferRange<'_, M> {}

/// What a draw's vertex buffers bind, in order.
pub struct Sources<'a, H: Hardware> {
    sources: &'a [Source<Driven<H>>],
}

impl<H: Hardware> Clone for Sources<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Hardware> Copy for Sources<'_, H> {}

impl<'a, H: Hardware> Sources<'a, H> {
    /// How many.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Each in turn.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = BufferRange<'a, H::Memory>> + 'a {
        self.sources.iter().map(range)
    }
}

/// How many vertices and instances a draw makes: given, or read from records
/// as it runs, each of [`DRAW_RECORD_BYTES`](crate::DRAW_RECORD_BYTES) or for
/// an indexed draw [`INDEXED_DRAW_RECORD_BYTES`](crate::INDEXED_DRAW_RECORD_BYTES).
#[derive(Debug)]
pub enum DrawCount<'a, M> {
    /// Once, as given.
    Direct(DrawCounts),
    /// Once for each record, read as it runs. A record naming a first instance
    /// on a device stating none loses the context.
    Indirect {
        /// What holds them.
        memory: &'a M,
        /// Where the first starts, into the memory: a whole word.
        offset: u64,
        /// How many there are: at most the device's.
        count: u32,
        /// The bytes from one to the next: where there are several, whole
        /// words and at least a record.
        stride: u32,
    },
}

impl<M> Clone for DrawCount<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for DrawCount<'_, M> {}

/// `source` as the backend binds it.
fn range<H: Hardware>(source: &Source<Driven<H>>) -> BufferRange<'_, H::Memory> {
    BufferRange {
        memory: &source.memory.memory,
        offset: source.start,
        size: source.size,
    }
}

/// `op` as the backend runs it.
fn command<H: Hardware>(op: &Op<Driven<H>>) -> JobCommand<'_, H> {
    match op {
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
        Op::BufferImage {
            buffer,
            image,
            regions,
            direction,
        } => JobCommand::BufferImage {
            memory: &buffer.memory.memory,
            offset: buffer.offset,
            image: &image.form,
            regions,
            direction: *direction,
        },
        Op::CopyImage { src, dst, regions } => JobCommand::CopyImage {
            src: &src.form,
            dst: &dst.form,
            regions,
        },
        Op::ClearImage {
            image,
            value,
            ranges,
        } => JobCommand::ClearImage {
            image: &image.form,
            value: *value,
            ranges,
        },
        Op::Dispatch(work) => JobCommand::Dispatch {
            pipeline: &work.pipeline.stages,
            sets: Sets { sets: &work.sets },
            push: work.push.as_deref().map_or(&[], alloc::vec::Vec::as_slice),
            counts: match &work.groups {
                Groups::Direct(groups) => Counts::Direct(*groups),
                Groups::Indirect { memory, offset } => Counts::Indirect {
                    memory: &memory.memory,
                    offset: *offset,
                },
            },
        },
        Op::Pass(pass) => JobCommand::Pass {
            targets: Targets {
                targets: &pass.colour,
            },
            depth_stencil: pass
                .depth_stencil
                .as_ref()
                .map(|target: &DepthTarget<Driven<H>>| PassDepthStencil {
                    view: &target.view.form,
                    format: target.view.desc.format,
                    depth: target.depth,
                    stencil: target.stencil,
                }),
            area: pass.area,
            draws: Draws {
                draws: &pass.draws,
                area: pass.area,
            },
        },
    }
}

#[cfg(test)]
#[path = "hardware_tests.rs"]
pub(crate) mod tests;

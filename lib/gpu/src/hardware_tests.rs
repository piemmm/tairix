//! A hardware backend held to the model: a probe whose jobs the test runs by
//! hand, so what reaches a driver's device and what answers it are each seen.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_rt::sync::Mutex;

use super::{
    Commands, Counts, DrawCount, Hardware, Job, JobCommand, PipelineStage, RenderStages,
    SetResource,
};
use crate::device_tests::{Clock, RAM, SECOND};
use crate::{
    Allocation, AllocationDesc, Aspects, BindSetEntry, Buffer, BufferDesc, BufferImageCopy,
    BufferUsage, BufferViewDesc, ClearValue, ComputePipelineDesc, Context, ContextDesc, CopyRegion,
    Device, Direction, Error, Extent3d, Features, Format, FormatFeatures, Image, ImageCopy,
    ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage, ImageViewDesc,
    LayoutBinding, Loss, MemoryRequirements, Origin3d, PushConstants, QueueCapabilities, Resource,
    SamplerDesc, Submission, ViewDimension,
};

/// A context as the probe holds it: its number, and the count of the
/// probe's contexts alive, lowered as it goes.
struct Tag {
    number: u32,
    alive: Arc<AtomicU32>,
}

impl Drop for Tag {
    fn drop(&mut self) {
        self.alive.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Memory as the probe holds it, and the context it was made in.
struct Bytes {
    bytes: Mutex<Vec<u8>>,
    context: u32,
}

impl core::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bytes")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// An image as the probe holds it: its context, how it was made, and the
/// context of the memory it was placed in and where.
struct ProbeImage {
    context: u32,
    desc: ImageDesc,
    placed: Mutex<Option<(u32, u64)>>,
}

impl core::fmt::Debug for ProbeImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProbeImage")
            .field("context", &self.context)
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

/// What a probe image needs of its memory: the first type, a page apart.
const PROBE_PLACEMENT: MemoryRequirements = MemoryRequirements {
    size: 4096,
    alignment: 4096,
    memory_types: 1,
};

/// A view as the probe holds it: its image's context and how it sees it.
#[derive(Debug)]
struct ProbeView {
    context: u32,
    desc: ImageViewDesc,
}

/// A bind set as the probe holds it: its context, its layout's binding
/// numbers, where in its memory each buffer it binds lies, and what each of
/// its descriptors is.
#[derive(Debug)]
struct ProbeSet {
    context: u32,
    bindings: Vec<u32>,
    spans: Vec<(u64, u64)>,
    kinds: Vec<&'static str>,
}

/// A compute pipeline as the probe holds it: its context, its entry point,
/// and its layout's sets and push constant bytes.
#[derive(Debug)]
struct ProbePipeline {
    context: u32,
    entry: alloc::string::String,
    sets: usize,
    push_bytes: u32,
}

/// A render pipeline as the probe holds it: its context, its stages' entry
/// points, the channels it writes of each target, and its vertex buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProbeRender {
    context: u32,
    entries: (alloc::string::String, Option<alloc::string::String>),
    writes: Vec<crate::ColourMask>,
    buffers: usize,
}

/// A pass as it reached the probe: each target's view's context, its format,
/// whether it resolves, and its load; the depth-stencil's format and loads;
/// its area; and each draw.
#[derive(Debug, PartialEq)]
struct Passed {
    targets: Vec<(u32, Format, bool, crate::Load)>,
    depth_stencil: Option<(Format, crate::Load, crate::Load)>,
    area: crate::Rect,
    draws: Vec<Drew>,
}

/// A draw as it reached the probe: its pipeline's context and vertex entry
/// point, its push constants, each vertex buffer's start and size in its
/// memory, its indices' and their width, its viewport, scissor, stencil
/// reference and blend constant, and its counts or where its records lie.
#[derive(Debug, PartialEq)]
struct Drew {
    pipeline: (u32, alloc::string::String),
    push: Vec<u32>,
    vertices: Vec<(u64, u64)>,
    index: Option<(u64, u64, crate::IndexFormat)>,
    viewport: crate::Viewport,
    scissor: crate::Rect,
    stencil_reference: u8,
    blend_constant: [f32; 4],
    count: Result<crate::DrawCounts, (u64, u32, u32)>,
}

/// A dispatch as it reached the probe: its pipeline's context, entry point,
/// layout sets and push constant bytes; each set's context, binding numbers
/// and spans; its push constants; and its counts or where they lie.
#[derive(Debug, PartialEq, Eq)]
struct Dispatched {
    pipeline: PipelineSeen,
    sets: Vec<SetSeen>,
    push: Vec<u32>,
    counts: Result<[u32; 3], u64>,
}

/// A pipeline as the probe saw it: its context, its entry point, its sets'
/// layouts and its push constant bytes.
type PipelineSeen = (u32, alloc::string::String, usize, u32);

/// A set as the probe saw it: its context, its binding numbers and the span
/// of memory each binds.
type SetSeen = (u32, Vec<u32>, Vec<(u64, u64)>);

/// What reached the probe.
#[derive(Default)]
struct Log {
    made: AtomicU32,
    alive: Arc<AtomicU32>,
    started: Mutex<Vec<Job<Probe>>>,
    lost: Mutex<Vec<u32>>,
    dispatched: Mutex<Vec<Dispatched>>,
    rendered: Mutex<Vec<ProbeRender>>,
    passed: Mutex<Vec<Passed>>,
}

/// The backend a device holds: a handle on the log the test reads.
struct Probe(Arc<Log>);

impl Hardware for Probe {
    type Context = Tag;
    type Memory = Bytes;
    type Image = ProbeImage;
    type View = ProbeView;
    type BufferView = (u64, BufferViewDesc);
    type Sampler = (u32, SamplerDesc);
    type Set = ProbeSet;
    type Compute = ProbePipeline;
    type Render = ProbeRender;

    fn create_context(&self) -> Result<Tag, Error> {
        self.0.alive.fetch_add(1, Ordering::SeqCst);
        Ok(Tag {
            number: self.0.made.fetch_add(1, Ordering::SeqCst),
            alive: Arc::clone(&self.0.alive),
        })
    }

    fn allocate(&self, context: &Arc<Tag>, _: u32, size: u64) -> Result<Bytes, Error> {
        let len = usize::try_from(size).map_err(|_| Error::OutOfMemory)?;
        Ok(Bytes {
            bytes: Mutex::new(alloc::vec![0; len]),
            context: context.number,
        })
    }

    fn write(&self, memory: &Bytes, offset: u64, data: &[u8]) -> Result<(), Error> {
        let at = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        memory.bytes.lock()[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn read(&self, memory: &Bytes, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let at = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        out.copy_from_slice(&memory.bytes.lock()[at..at + out.len()]);
        Ok(())
    }

    fn image(
        &self,
        context: &Arc<Tag>,
        desc: &ImageDesc,
    ) -> Result<(ProbeImage, MemoryRequirements), Error> {
        Ok((
            ProbeImage {
                context: context.number,
                desc: *desc,
                placed: Mutex::new(None),
            },
            PROBE_PLACEMENT,
        ))
    }

    fn place(&self, image: &ProbeImage, memory: &Bytes, offset: u64) -> Result<(), Error> {
        *image.placed.lock() = Some((memory.context, offset));
        Ok(())
    }

    fn view(
        &self,
        image: &ProbeImage,
        of: &ImageDesc,
        desc: &ImageViewDesc,
    ) -> Result<ProbeView, Error> {
        assert_eq!(*of, image.desc, "the view's own image's description");
        Ok(ProbeView {
            context: image.context,
            desc: *desc,
        })
    }

    fn buffer_view(
        &self,
        _: &Bytes,
        offset: u64,
        desc: &BufferViewDesc,
    ) -> Result<(u64, BufferViewDesc), Error> {
        Ok((offset, *desc))
    }

    fn sampler(&self, context: &Arc<Tag>, desc: &SamplerDesc) -> Result<(u32, SamplerDesc), Error> {
        Ok((context.number, *desc))
    }

    fn set(
        &self,
        context: &Arc<Tag>,
        layout: &[LayoutBinding],
        resources: &[SetResource<'_, Self>],
    ) -> Result<ProbeSet, Error> {
        let spans = resources
            .iter()
            .filter_map(|resource| match resource {
                SetResource::Buffer { offset, size, .. } => Some((*offset, *size)),
                _ => None,
            })
            .collect();
        let kinds = resources
            .iter()
            .map(|resource| match resource {
                SetResource::Buffer { .. } => "buffer",
                SetResource::Sampler(_) => "sampler",
                SetResource::Image(_) => "image",
                SetResource::ImageSampler(..) => "image and sampler",
                SetResource::TexelBuffer(_) => "texels",
            })
            .collect();
        Ok(ProbeSet {
            context: context.number,
            bindings: layout.iter().map(|binding| binding.binding).collect(),
            spans,
            kinds,
        })
    }

    fn compute(
        &self,
        context: &Arc<Tag>,
        stage: PipelineStage<'_>,
    ) -> Result<ProbePipeline, Error> {
        Ok(ProbePipeline {
            context: context.number,
            entry: stage.entry_point().into(),
            sets: stage.sets().len(),
            push_bytes: stage.push_constants().bytes,
        })
    }

    fn render(&self, context: &Arc<Tag>, stages: RenderStages<'_>) -> Result<ProbeRender, Error> {
        let made = ProbeRender {
            context: context.number,
            entries: (
                stages.vertex().entry_point().into(),
                stages.fragment().map(|stage| stage.entry_point().into()),
            ),
            writes: (0..stages.targets().len())
                .map(|at| stages.writes(at))
                .collect(),
            buffers: stages.vertex_buffers().len(),
        };
        self.0.rendered.lock().push(made.clone());
        Ok(made)
    }

    fn run(&self, job: Job<Self>) {
        self.0.started.lock().push(job);
    }

    fn run_bytes(_: Commands<'_, Self>) -> u64 {
        0
    }

    fn lost(&self, context: &Arc<Tag>) {
        self.0.lost.lock().push(context.number);
    }
}

impl Log {
    /// The jobs started and not yet taken, oldest first.
    fn take(&self) -> Vec<Job<Probe>> {
        core::mem::take(&mut *self.started.lock())
    }
}

/// `at` as an index into the probe's bytes.
fn index(at: u64) -> usize {
    usize::try_from(at).expect("within the probe's bytes")
}

/// Carry out `job`'s commands on the probe's bytes, as a device would, a
/// dispatch noted in `log`.
fn execute(job: &Job<Probe>, log: &Log) {
    for command in job.commands().iter() {
        match command {
            JobCommand::Copy { src, dst, spans } => {
                let read = src.bytes.lock().clone();
                let mut written = dst.bytes.lock();
                for span in spans {
                    let (from, to, len) = (index(span.src), index(span.dst), index(span.len));
                    written[to..to + len].copy_from_slice(&read[from..from + len]);
                }
            }
            JobCommand::Fill {
                dst,
                offset,
                size,
                word,
            } => {
                let mut written = dst.bytes.lock();
                for chunk in written[index(offset)..index(offset + size)]
                    .as_chunks_mut::<4>()
                    .0
                {
                    *chunk = word.to_le_bytes();
                }
            }
            JobCommand::Update { dst, offset, data } => {
                dst.bytes.lock()[index(offset)..index(offset) + data.len()].copy_from_slice(data);
            }
            // The probe lays out no texels; what an image command names is
            // read where a test needs it.
            JobCommand::BufferImage { .. }
            | JobCommand::CopyImage { .. }
            | JobCommand::ClearImage { .. } => {}
            JobCommand::Dispatch {
                pipeline,
                sets,
                push,
                counts,
            } => log.dispatched.lock().push(Dispatched {
                pipeline: (
                    pipeline.context,
                    pipeline.entry.clone(),
                    pipeline.sets,
                    pipeline.push_bytes,
                ),
                sets: sets
                    .iter()
                    .map(|set| (set.context, set.bindings.clone(), set.spans.clone()))
                    .collect(),
                push: push.to_vec(),
                counts: match counts {
                    Counts::Direct(groups) => Ok(groups),
                    Counts::Indirect { offset, .. } => Err(offset),
                },
            }),
            JobCommand::Pass {
                targets,
                depth_stencil,
                area,
                draws,
            } => log
                .passed
                .lock()
                .push(passed(targets, depth_stencil, area, draws)),
        }
    }
}

/// A pass as the probe notes it.
fn passed(
    targets: crate::Targets<'_, Probe>,
    depth_stencil: Option<crate::PassDepthStencil<'_, Probe>>,
    area: crate::Rect,
    draws: crate::Draws<'_, Probe>,
) -> Passed {
    Passed {
        targets: targets
            .iter()
            .map(|target| {
                (
                    target.view.context,
                    target.format,
                    target.resolve.is_some(),
                    target.load,
                )
            })
            .collect(),
        depth_stencil: depth_stencil.map(|target| (target.format, target.depth, target.stencil)),
        area,
        draws: draws
            .iter()
            .map(|draw| Drew {
                pipeline: (draw.pipeline.context, draw.pipeline.entries.0.clone()),
                push: draw.push.to_vec(),
                vertices: draw
                    .vertex_buffers
                    .iter()
                    .map(|range| (range.offset, range.size))
                    .collect(),
                index: draw
                    .index
                    .map(|(range, format)| (range.offset, range.size, format)),
                viewport: draw.viewport,
                scissor: draw.scissor,
                stencil_reference: draw.stencil_reference,
                blend_constant: draw.blend_constant,
                count: match draw.count {
                    DrawCount::Direct(counts) => Ok(counts),
                    DrawCount::Indirect {
                        offset,
                        count,
                        stride,
                        ..
                    } => Err((offset, count, stride)),
                },
            })
            .collect(),
    }
}

fn device(clock: &Arc<Clock>) -> (Device, Arc<Log>) {
    let log = Arc::new(Log::default());
    let device = Device::hardware(
        Probe(Arc::clone(&log)),
        crate::software::info(RAM).expect("a description"),
        &[1],
        Arc::clone(clock) as Arc<dyn MonotonicClock + Send + Sync>,
    )
    .expect("a device");
    (device, log)
}

/// The probe's device behind `device`, to look into what it holds.
fn shared(device: &Device) -> &crate::device::Shared<super::Driven<Probe>> {
    device
        .host
        .as_any()
        .downcast_ref()
        .expect("a probe's device")
}

fn context(device: &Device) -> Context {
    device
        .create_context(&ContextDesc::default())
        .expect("a context")
}

/// A host-visible buffer of `size` bytes over memory of its own.
fn buffer(context: &Context, size: u64) -> Buffer {
    let memory = context
        .allocate(&AllocationDesc {
            size,
            memory_type: 0,
        })
        .expect("memory");
    context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage: BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST),
            },
        )
        .expect("a buffer")
}

/// Submit `commands` to the context's first queue with a deadline of a second.
fn submit(context: &Context, commands: &crate::CommandBuffer) -> crate::Fence {
    context
        .queue(0, 0)
        .expect("a queue")
        .submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted")
}

#[test]
fn a_job_reaches_the_device_whole_and_its_fence_lands_once_answered() {
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let memory = context
        .allocate(&AllocationDesc {
            size: 64,
            memory_type: 0,
        })
        .expect("memory");
    let usage = BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST);
    let low = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size: 32,
                usage,
            },
        )
        .expect("a buffer");
    let high = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 32,
                size: 32,
                usage,
            },
        )
        .expect("a buffer");
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(&low, 0, 16, 0x0101_0101)
        .expect("recorded");
    commands
        .update_buffer(&low, 16, &[7; 16])
        .expect("recorded");
    commands
        .copy_buffer(
            &low,
            &high,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 0,
                size: 32,
            }],
        )
        .expect("recorded");
    let fence = submit(&context, &commands);
    let mut started = log.take();
    assert_eq!(started.len(), 1, "one job, started on submission");
    let job = started.pop().expect("the job");
    assert_eq!(job.commands().iter().count(), 3);
    assert_eq!(job.queue(), 0);
    let kinds: Vec<&str> = job
        .commands()
        .iter()
        .map(|command| match command {
            JobCommand::Fill {
                offset: 0,
                size: 16,
                ..
            } => "fill",
            JobCommand::Update {
                offset: 16, data, ..
            } if data == [7; 16] => "update",
            JobCommand::Copy { spans, .. }
                if spans
                    == [crate::CopySpan {
                        src: 0,
                        dst: 32,
                        len: 32,
                    }] =>
            {
                "copy"
            }
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["fill", "update", "copy"],
        "in order, offsets into the memory"
    );
    assert_eq!(
        fence.is_signalled(),
        Ok(false),
        "not until the device answers"
    );
    execute(&job, &log);
    job.finish(Ok(()));
    assert_eq!(fence.wait(Duration64::ZERO), Ok(()));
    let mut read = [0u8; 64];
    memory.read(0, &mut read).expect("host-visible");
    assert_eq!(read[..16], [1; 16]);
    assert_eq!(read[16..32], [7; 16]);
    assert_eq!(read[..32], read[32..]);
}

#[test]
fn an_engine_starts_no_more_jobs_than_its_depth() {
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let target = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
    let first = submit(&context, &commands);
    let second = submit(&context, &commands);
    let mut started = log.take();
    assert_eq!(started.len(), 1, "a depth of one");
    started.pop().expect("the first").finish(Ok(()));
    assert_eq!(first.wait(Duration64::ZERO), Ok(()));
    let mut started = log.take();
    assert_eq!(
        started.len(),
        1,
        "the second starts as the first is answered"
    );
    started.pop().expect("the second").finish(Ok(()));
    assert_eq!(second.wait(Duration64::ZERO), Ok(()));
}

#[test]
fn a_job_answered_with_a_loss_or_dropped_unanswered_loses_its_context() {
    for answered in [true, false] {
        let clock = Arc::new(Clock::default());
        let (device, log) = device(&clock);
        let context = context(&device);
        let target = buffer(&context, 16);
        let mut commands = context.command_buffer();
        commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
        let fence = submit(&context, &commands);
        let job = log.take().pop().expect("the job");
        if answered {
            job.finish(Err(Loss::Fault));
        } else {
            drop(job);
        }
        assert_eq!(fence.wait(Duration64::ZERO), Err(Error::Lost(Loss::Fault)));
        assert_eq!(context.status(), Err(Error::Lost(Loss::Fault)));
    }
}

#[test]
fn the_watchdog_answers_an_overrun_job_as_overran_and_tells_the_backend() {
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let target = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
    let fence = submit(&context, &commands);
    let job = log.take().pop().expect("the job");
    let deadline = device
        .served
        .as_ref()
        .expect("in this process")
        .earliest_deadline()
        .expect("watched");
    assert_eq!(deadline, job.deadline());
    device
        .served
        .as_ref()
        .expect("in this process")
        .expire(deadline - 1);
    assert_eq!(context.status(), Ok(()), "short of its deadline");
    device
        .served
        .as_ref()
        .expect("in this process")
        .expire(deadline);
    assert_eq!(context.status(), Err(Error::Lost(Loss::Overran)));
    assert_eq!(*log.lost.lock(), [0], "told once, of that context");
    assert_eq!(
        device
            .served
            .as_ref()
            .expect("in this process")
            .earliest_deadline(),
        None,
        "nothing left running"
    );
    // The device's own answer, when it comes, finds nothing to complete.
    job.finish(Ok(()));
    assert_eq!(
        fence.wait(Duration64::ZERO),
        Err(Error::Lost(Loss::Overran))
    );
}

#[test]
fn a_context_on_the_device_goes_with_the_last_of_its_memory_and_work() {
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    assert_eq!(log.alive.load(Ordering::SeqCst), 1);
    let target = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
    let _fence = submit(&context, &commands);
    let job = log.take().pop().expect("the job");
    drop((commands, target, context));
    assert_eq!(
        log.alive.load(Ordering::SeqCst),
        1,
        "its running job holds it"
    );
    job.finish(Ok(()));
    assert_eq!(log.alive.load(Ordering::SeqCst), 0);
}

/// A four-texel square of `format`, made for `usage`.
fn square(format: Format, usage: ImageUsage) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage,
    }
}

/// Host-visible memory of `size` bytes.
fn memory(context: &Context, size: u64) -> crate::Allocation {
    context
        .allocate(&AllocationDesc {
            size,
            memory_type: 0,
        })
        .expect("memory")
}

#[test]
fn an_image_is_made_placed_and_seen_through_the_device_in_its_context() {
    let clock = Arc::new(Clock::default());
    let (device, _log) = device(&clock);
    let (_first, second) = (context(&device), context(&device));
    let desc = square(Format::Rgba8Unorm, ImageUsage::SAMPLED);
    assert_eq!(second.image_requirements(&desc), Ok(PROBE_PLACEMENT));
    let held = memory(&second, 3 * PROBE_PLACEMENT.size);
    assert_eq!(
        second
            .create_image(&held, PROBE_PLACEMENT.alignment / 2, &desc)
            .err(),
        Some(Error::Misaligned),
        "placed as the device's requirements say"
    );
    let image = second
        .create_image(&held, PROBE_PLACEMENT.alignment, &desc)
        .expect("an image");
    let view_desc = ImageViewDesc {
        format: Format::Rgba8UnormSrgb,
        dimension: ViewDimension::D2,
        range: ImageRange {
            aspects: Aspects::COLOR,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        },
    };
    let view = second
        .create_image_view(&image, &view_desc)
        .expect("a view");
    let shared = shared(&device);
    let model = shared.model.lock();
    let held_image = model
        .live(second.key)
        .expect("live")
        .images
        .table
        .get(image.key)
        .expect("filed");
    assert_eq!(held_image.form.context, 1, "made in its own context");
    assert_eq!(
        *held_image.form.placed.lock(),
        Some((1, PROBE_PLACEMENT.alignment)),
        "placed in its context's memory, where asked"
    );
    let held_view = model
        .live(second.key)
        .expect("live")
        .views
        .get(view.key)
        .expect("filed");
    assert_eq!(
        (held_view.form.context, held_view.form.desc),
        (1, view_desc)
    );
}

/// A description stating every queue, feature and use the software device
/// states — drawing among them — is served as it is.
#[test]
fn a_description_stating_draws_is_served_whole() {
    let clock = Arc::new(Clock::default());
    let (device, _) = device(&clock);
    let stated = crate::software::info(RAM).expect("a description");
    assert!(stated
        .queue_families
        .iter()
        .any(|family| family.capabilities.contains(QueueCapabilities::GRAPHICS)));
    assert!(stated.features.contains(Features::WIDE_LINES));
    assert!(stated
        .formats
        .get(Format::Rgba8Unorm)
        .contains(FormatFeatures::COLOR_ATTACHMENT.with(FormatFeatures::RESOLVED)));
    assert_eq!(*device.info(), stated);
}

/// A fragment stage writing red and green alone to its location-0 target.
const RED_GREEN: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %out_colour Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v2 = OpTypeVector %float 2
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v2 = OpTypePointer Output %v2
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_v2 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%c = OpLoad %v4 %in_colour
%rg = OpVectorShuffle %v2 %c %c 0 1
OpStore %out_colour %rg
OpReturn
OpFunctionEnd
"#;

/// The area the probe's pass touches.
const PROBE_AREA: crate::Rect = crate::Rect {
    x: 2,
    y: 2,
    width: 4,
    height: 4,
};

/// The probe's render pipeline: triangles into one float target, its
/// fragment stage writing red and green, depth-tested.
fn probe_pipeline(context: &Context) -> crate::RenderPipeline {
    use crate::render::tests::{bare, passing, pipeline, plain, Shape, FILLED, ONE_SAMPLE, VEC4};
    use crate::{CompareOp, DepthStencilState, Topology};
    let layout = bare(context);
    pipeline(
        context,
        &layout,
        (&passing(VEC4, ""), Some(RED_GREEN)),
        &Shape {
            topology: Topology::Triangles,
            targets: &[plain(Format::Rgba32Float)],
            depth_stencil: Some(DepthStencilState {
                format: Format::Depth32FloatStencil8,
                depth_compare: CompareOp::Less,
                depth_write: true,
                stencil: None,
            }),
            multisample: ONE_SAMPLE,
            rasterization: FILLED,
            restart: false,
        },
    )
    .expect("a pipeline")
}

/// The probe's pass within [`PROBE_AREA`] into `colour` cleared to `clear`
/// and `depth` cleared to `depth_clear`: a direct draw scissored wider than
/// the area, an indexed one scissored outside it, and an indirect one of two
/// records, each through `made` binding `vertices` from 32 bytes in.
fn probe_pass(
    context: &Context,
    (made, vertices): (&crate::RenderPipeline, &Buffer),
    (colour, depth): (&crate::render::tests::Target, &crate::render::tests::Target),
    (clear, depth_clear): (crate::Load, crate::Load),
) -> crate::CommandBuffer {
    use crate::render::tests::whole;
    use crate::{
        ColourAttachment, DepthStencilAttachment, Draw, IndexFormat, Indices, Indirect, Load, Pass,
        Rect,
    };
    let (viewport, _) = whole((8, 8));
    let bound = [(vertices, 32)];
    let draw = |scissor| Draw {
        pipeline: made,
        sets: &[],
        push_constants: &[],
        vertex_buffers: &bound,
        viewport,
        scissor,
        stencil_reference: 7,
        blend_constant: [0.5; 4],
    };
    let wide = Rect {
        x: 0,
        y: 0,
        width: 6,
        height: 8,
    };
    let outside = Rect {
        x: 7,
        y: 1,
        width: 1,
        height: 6,
    };
    let mut commands = context.command_buffer();
    commands
        .render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &colour.view,
                    resolve: None,
                    load: clear,
                }],
                depth_stencil: Some(DepthStencilAttachment {
                    view: &depth.view,
                    depth_load: depth_clear,
                    stencil_load: Load::Keep,
                }),
                area: PROBE_AREA,
            },
            |drawer| {
                drawer.draw(&draw(wide), 0..3, 1..2)?;
                drawer.draw_indexed(
                    &draw(outside),
                    &Indices {
                        buffer: vertices,
                        offset: 4,
                        format: IndexFormat::U16,
                    },
                    (3..6, -1),
                    0..1,
                )?;
                drawer.draw_indirect(
                    &draw(PROBE_AREA),
                    &Indirect {
                        buffer: vertices,
                        offset: 8,
                        count: 2,
                        stride: 16,
                    },
                )
            },
        )
        .expect("recorded");
    commands
}

/// A render pipeline reaches the backend with its stages and the channels it
/// writes of each target, and a pass with its targets' formats and loads and
/// each draw's sources, scissor within the pass's area, and counts or
/// records, every offset into its memory.
#[test]
fn a_render_pipeline_and_its_pass_reach_the_backend_as_checked() {
    use crate::render::tests::{corners, floats, target, whole};
    use crate::{ColourMask, DrawCounts, IndexFormat, Load, Rect};
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let made = probe_pipeline(&context);
    let colour = target(&context, Format::Rgba32Float, (8, 8), 1);
    let depth = target(&context, Format::Depth32FloatStencil8, (8, 8), 1);
    let (_held, vertices) = floats(&context, &corners(&[([0.0; 4], [0.0; 4]); 6]));
    let clear = Load::Clear(ClearValue::Float([0.25; 4]));
    let depth_clear = Load::Clear(ClearValue::DepthStencil {
        depth: 1.0,
        stencil: 3,
    });
    let commands = probe_pass(
        &context,
        (&made, &vertices),
        (&colour, &depth),
        (clear, depth_clear),
    );
    let fence = submit(&context, &commands);
    drain(&log);
    assert_eq!(fence.wait(Duration64::ZERO), Ok(()));
    let rendered = log.rendered.lock().clone();
    let number = rendered.first().map(|made| made.context).expect("made");
    assert_eq!(
        rendered,
        [ProbeRender {
            context: number,
            entries: ("main".into(), Some("main".into())),
            writes: alloc::vec![ColourMask::RED.with(ColourMask::GREEN)],
            buffers: 1,
        }],
        "only the channels its fragment stage writes"
    );
    let (viewport, _) = whole((8, 8));
    let noted = |scissor, index, count| Drew {
        pipeline: (number, "main".into()),
        push: alloc::vec![],
        vertices: alloc::vec![(32, 160)],
        index,
        viewport,
        scissor,
        stencil_reference: 7,
        blend_constant: [0.5; 4],
        count,
    };
    let passed = log.passed.lock();
    assert_eq!(
        *passed,
        [Passed {
            targets: alloc::vec![(number, Format::Rgba32Float, false, clear)],
            depth_stencil: Some((Format::Depth32FloatStencil8, depth_clear, Load::Keep)),
            area: PROBE_AREA,
            draws: alloc::vec![
                noted(
                    PROBE_AREA,
                    None,
                    Ok(DrawCounts {
                        first: 0,
                        positions: 3,
                        base_vertex: 0,
                        first_instance: 1,
                        instances: 1,
                    }),
                ),
                noted(
                    Rect {
                        width: 0,
                        height: 0,
                        ..PROBE_AREA
                    },
                    Some((4, 188, IndexFormat::U16)),
                    Ok(DrawCounts {
                        first: 3,
                        positions: 3,
                        base_vertex: -1,
                        first_instance: 0,
                        instances: 1,
                    }),
                ),
                noted(PROBE_AREA, None, Err((8, 2, 16))),
            ],
        }],
        "each scissor within the area, an empty one at its corner, every offset into its memory"
    );
}

/// Run every job the probe was handed, oldest first, answering each.
fn drain(log: &Log) {
    for job in log.take() {
        execute(&job, log);
        job.finish(Ok(()));
    }
}

/// A bind set and a compute pipeline reach the backend as the model checked
/// them, made in their context, and a dispatch names them, its push
/// constants and its counts — given, or where they lie in their memory.
#[test]
fn a_set_a_pipeline_and_their_dispatches_reach_the_backend_as_checked() {
    use crate::pipeline::tests::{doubles, storage};
    use tairix_spirv::assemble::admit;
    use tairix_spirv::Stages;

    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let memory = context
        .allocate(&AllocationDesc {
            size: 256,
            memory_type: 0,
        })
        .expect("memory");
    let upper = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 64,
                size: 192,
                usage: BufferUsage::ALL,
            },
        )
        .expect("a buffer");
    let module = context
        .create_shader_module(admit(&doubles(4)).expect("admitted"))
        .expect("a module");
    let set_layout = context
        .create_bind_set_layout(&[storage(0)])
        .expect("a set layout");
    let push = PushConstants {
        bytes: 8,
        stages: Stages::COMPUTE,
    };
    let layout = context
        .create_pipeline_layout(&[&set_layout], push)
        .expect("a pipeline layout");
    let pipeline = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let set = context
        .create_bind_set(
            &set_layout,
            &[BindSetEntry {
                binding: 0,
                resources: &[Resource::Buffer {
                    buffer: &upper,
                    offset: 32,
                    size: 64,
                }],
            }],
        )
        .expect("a set");
    let mut commands = context.command_buffer();
    let words = [7u32, 9];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    commands
        .dispatch(&pipeline, &[&set], &bytes, [2, 3, 1])
        .expect("recorded");
    commands
        .dispatch_indirect(&pipeline, &[&set], &bytes, (&upper, 4))
        .expect("recorded");
    let fence = submit(&context, &commands);
    drain(&log);
    assert_eq!(fence.wait(SECOND), Ok(()));
    let made_in = log.made.load(Ordering::SeqCst) - 1;
    let pipeline_made = (made_in, alloc::string::String::from("main"), 1, 8);
    let sets = alloc::vec![(made_in, alloc::vec![0], alloc::vec![(64 + 32, 64)])];
    let expected = [
        Dispatched {
            pipeline: pipeline_made.clone(),
            sets: sets.clone(),
            push: words.to_vec(),
            counts: Ok([2, 3, 1]),
        },
        Dispatched {
            pipeline: pipeline_made,
            sets,
            push: words.to_vec(),
            counts: Err(64 + 4),
        },
    ];
    assert_eq!(*log.dispatched.lock(), expected);
}

/// Two square colour images for every transfer and sampling, placed one
/// after the other in memory of their own.
fn probe_images(context: &Context) -> (Allocation, Image, Image) {
    let usage = ImageUsage::SAMPLED
        .with(ImageUsage::TRANSFER_SRC)
        .with(ImageUsage::TRANSFER_DST);
    let held = memory(context, 4 * PROBE_PLACEMENT.size);
    let picture = context
        .create_image(&held, 0, &square(Format::Rgba8Unorm, usage))
        .expect("an image");
    let copy = context
        .create_image(
            &held,
            PROBE_PLACEMENT.size,
            &square(Format::Rgba8Unorm, usage),
        )
        .expect("an image");
    (held, picture, copy)
}

/// A buffer for texel reads and transfers 256 bytes into memory of its own.
fn probe_texels(context: &Context) -> (Allocation, Buffer) {
    let texels_memory = memory(context, 1024);
    let texels_buffer = context
        .create_buffer(
            &texels_memory,
            &BufferDesc {
                offset: 256,
                size: 512,
                usage: BufferUsage::UNIFORM_TEXEL
                    .with(BufferUsage::TRANSFER_SRC)
                    .with(BufferUsage::TRANSFER_DST),
            },
        )
        .expect("a buffer");
    (texels_memory, texels_buffer)
}

/// Every plane, level and layer of a probe image.
const PROBE_WHOLE: ImageRange = ImageRange {
    aspects: Aspects::COLOR,
    base_level: 0,
    levels: 1,
    base_layer: 0,
    layers: 1,
};

/// A set binds each kind of descriptor, each as the device made it.
#[test]
fn a_set_binds_samplers_images_and_texel_buffers_each_as_the_device_made_it() {
    use crate::{DescriptorType, SampleType, SamplerType};
    use tairix_spirv::{Access, Component, ImageShape, Stages};

    let clock = Arc::new(Clock::default());
    let (device, _) = device(&clock);
    let context = context(&device);
    let shape = ImageShape {
        dim: tairix_spirv::enumerant::Dim::DIM_2D,
        arrayed: false,
        multisampled: false,
        component: Component::Float,
        format: tairix_spirv::enumerant::ImageFormat::UNKNOWN,
    };
    let entry = |binding, kind| LayoutBinding {
        binding,
        kind,
        count: 1,
        access: Access::READ,
        stages: Stages::COMPUTE,
    };
    let filtered =
        DescriptorType::CombinedImageSampler(shape, SampleType::Float, SamplerType::Filtering);
    let texel_shape = ImageShape {
        dim: tairix_spirv::enumerant::Dim::BUFFER,
        ..shape
    };
    let layout = context
        .create_bind_set_layout(&[
            entry(0, DescriptorType::Sampler(SamplerType::Filtering)),
            entry(1, DescriptorType::SampledImage(shape, SampleType::Float)),
            entry(2, filtered),
            entry(3, DescriptorType::UniformTexelBuffer(texel_shape)),
        ])
        .expect("a layout");
    let (_held, picture, _copy) = probe_images(&context);
    let view = context
        .create_image_view(
            &picture,
            &ImageViewDesc {
                format: Format::Rgba8Unorm,
                dimension: ViewDimension::D2,
                range: PROBE_WHOLE,
            },
        )
        .expect("a view");
    let sampler = context
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    let (_texels_memory, texels_buffer) = probe_texels(&context);
    let texels = context
        .create_buffer_view(
            &texels_buffer,
            &BufferViewDesc {
                offset: 64,
                size: 64,
                format: Format::Rgba8Unorm,
            },
        )
        .expect("a texel buffer");
    let resources = [
        Resource::Sampler(&sampler),
        Resource::Image(&view),
        Resource::ImageSampler(&view, &sampler),
        Resource::TexelBuffer(&texels),
    ];
    let entries: alloc::vec::Vec<BindSetEntry<'_>> = (0u32..)
        .zip(&resources)
        .map(|(binding, resource)| BindSetEntry {
            binding,
            resources: core::slice::from_ref(resource),
        })
        .collect();
    let set = context.create_bind_set(&layout, &entries).expect("a set");
    let shared = shared(&device);
    let model = shared.model.lock();
    let live = model.live(context.key).expect("live");
    let held_set = live.bind_sets.get(set.key).expect("filed");
    assert_eq!(
        held_set.form.kinds,
        ["sampler", "image", "image and sampler", "texels"]
    );
    let held_texels = live.buffer_views.get(texels.key).expect("filed");
    assert_eq!(held_texels.form.0, 256 + 64, "its offset into its memory");
}

/// A job carries every image command, each naming what its memory and
/// images hold where they lie.
#[test]
fn a_job_carries_every_image_command_naming_where_its_memory_and_images_lie() {
    let clock = Arc::new(Clock::default());
    let (device, log) = device(&clock);
    let context = context(&device);
    let (_held, picture, copy) = probe_images(&context);
    let (_texels_memory, texels_buffer) = probe_texels(&context);
    let region = BufferImageCopy {
        buffer_offset: 16,
        bytes_per_row: 16,
        rows_per_image: 4,
        image: ImageSubresource {
            aspect: Aspects::COLOR,
            level: 0,
            base_layer: 0,
            layers: 1,
        },
        origin: Origin3d::default(),
        extent: Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
    };
    let mut commands = context.command_buffer();
    commands
        .copy_buffer_to_image(&texels_buffer, &picture, &[region])
        .expect("recorded");
    commands
        .copy_image(
            &picture,
            &copy,
            &[ImageCopy {
                src: region.image,
                src_origin: Origin3d::default(),
                dst: region.image,
                dst_origin: Origin3d::default(),
                extent: region.extent,
            }],
        )
        .expect("recorded");
    commands
        .clear_image(&copy, ClearValue::Float([0.5; 4]), &[PROBE_WHOLE])
        .expect("recorded");
    commands
        .copy_image_to_buffer(&copy, &texels_buffer, &[region])
        .expect("recorded");
    let fence = submit(&context, &commands);
    let job = log.take().pop().expect("a job");
    let seen: alloc::vec::Vec<_> = job.commands().iter().map(described).collect();
    let page = PROBE_PLACEMENT.size;
    assert_eq!(
        seen,
        [
            alloc::format!("{:?} 0 at 256 image Some(0) 1 regions", Direction::ToImage),
            alloc::format!("copy Some(0) to Some({page}) 1 regions"),
            alloc::format!("clear Some({page}) to Float([0.5, 0.5, 0.5, 0.5]) 1 ranges"),
            alloc::format!(
                "{:?} 0 at 256 image Some({page}) 1 regions",
                Direction::ToBuffer
            ),
        ],
        "the buffer's start in its memory, the regions from it"
    );
    job.finish(Ok(()));
    assert_eq!(fence.wait(Duration64::ZERO), Ok(()));
}

/// An image command as a job carries it: what it names and where.
fn described(command: JobCommand<'_, Probe>) -> alloc::string::String {
    match command {
        JobCommand::BufferImage {
            memory,
            offset,
            image,
            regions,
            direction,
        } => alloc::format!(
            "{direction:?} {} at {offset} image {:?} {} regions",
            memory.context,
            image.placed.lock().map(|(_, at)| at),
            regions.len()
        ),
        JobCommand::CopyImage { src, dst, regions } => alloc::format!(
            "copy {:?} to {:?} {} regions",
            src.placed.lock().map(|(_, at)| at),
            dst.placed.lock().map(|(_, at)| at),
            regions.len()
        ),
        JobCommand::ClearImage {
            image,
            value,
            ranges,
        } => alloc::format!(
            "clear {:?} to {value:?} {} ranges",
            image.placed.lock().map(|(_, at)| at),
            ranges.len()
        ),
        _ => "other".into(),
    }
}

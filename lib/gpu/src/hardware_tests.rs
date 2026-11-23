//! A hardware backend held to the model: a probe whose jobs the test runs by
//! hand, so what reaches a driver's device and what answers it are each seen.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_rt::sync::Mutex;

use super::{Commands, Hardware, Job, JobCommand};
use crate::device_tests::{Clock, RAM, SECOND};
use crate::{
    AllocationDesc, Buffer, BufferDesc, BufferUsage, Context, ContextDesc, CopyRegion, Device,
    DeviceInfo, Error, Features, Format, FormatFeatures, FormatTable, ImageDesc, ImageDimension,
    ImageUsage, Loss, QueueCapabilities, Submission,
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

/// What reached the probe.
#[derive(Default)]
struct Log {
    made: AtomicU32,
    alive: Arc<AtomicU32>,
    started: Mutex<Vec<Job<Probe>>>,
    lost: Mutex<Vec<u32>>,
}

/// The backend a device holds: a handle on the log the test reads.
struct Probe(Arc<Log>);

impl Hardware for Probe {
    type Context = Tag;
    type Memory = Bytes;

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

/// Carry out `job`'s commands on the probe's bytes, as a device would.
fn execute(job: &Job<Probe>) {
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
        }
    }
}

fn device(clock: &Arc<Clock>) -> (Device, Arc<Log>) {
    let log = Arc::new(Log::default());
    let device = Device::hardware(
        Probe(Arc::clone(&log)),
        transfers_only(),
        &[1],
        Arc::clone(clock) as Arc<dyn MonotonicClock + Send + Sync>,
    )
    .expect("a device");
    (device, log)
}

/// The software device's description narrowed to what the seam carries.
fn transfers_only() -> DeviceInfo {
    let mut info = crate::software::info(RAM).expect("a description");
    info.capabilities.clear();
    info.features = Features::NONE;
    info.formats = FormatTable::from_fn(|_| FormatFeatures::NONE);
    for family in &mut info.queue_families {
        family.capabilities = QueueCapabilities::TRANSFER;
    }
    info
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
    execute(&job);
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

#[test]
fn a_device_making_no_images_states_no_format_and_refuses_one() {
    let clock = Arc::new(Clock::default());
    let (device, _log) = device(&clock);
    let context = context(&device);
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::Rgba8Unorm,
        extent: crate::Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage: ImageUsage::TRANSFER_DST,
    };
    assert_eq!(device.info().formats.get(desc.format), FormatFeatures::NONE);
    assert_eq!(context.image_requirements(&desc), Err(Error::Unsupported));
}

#[test]
fn a_description_promising_more_than_memory_and_transfers_is_refused() {
    let clock: Arc<dyn MonotonicClock + Send + Sync> = Arc::new(Clock::default());
    let capability = crate::software::info(RAM)
        .expect("a description")
        .capabilities[0];
    let promises: [&dyn Fn(&mut DeviceInfo); 4] = [
        &|info| info.capabilities.try_push(capability).expect("room"),
        &|info| info.features = Features::WIDE_LINES,
        &|info| {
            info.formats = FormatTable::from_fn(|format| {
                if format == Format::Rgba8Unorm {
                    FormatFeatures::SAMPLED
                } else {
                    FormatFeatures::NONE
                }
            });
        },
        &|info| info.queue_families[0].capabilities = QueueCapabilities::ALL,
    ];
    for promise in promises {
        let mut info = transfers_only();
        promise(&mut info);
        let made = Device::hardware(Probe(Arc::default()), info, &[1], Arc::clone(&clock));
        assert_eq!(made.err(), Some(Error::Malformed));
    }
    assert!(Device::hardware(Probe(Arc::default()), transfers_only(), &[1], clock).is_ok());
}

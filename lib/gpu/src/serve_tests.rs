//! The served device held to the device it serves: a software device served
//! in this process on a channel of the test's own, its requests run by the
//! runtime as a driver's are, its clients connected as a client is, and what
//! a client sees through it compared with what the device answers directly.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

use tairix_abi::gpu::{
    decode_reply, GpuError, GpuOp, GpuRequest, GpuStatus, GPU_REGION_MIN, MAX_CLIENT_CONTEXTS,
};
use tairix_abi::time::MonotonicClock;
use tairix_abi::{Duration64, Errno, ProcId};

use super::{Channel, Region, Server};
use crate::device_tests::{buffer, contents, context, memory, over, queue, submit};
use crate::pipeline::tests::{doubles, rig, run, words};
use crate::remote::{Empty, Link, Remote};
use crate::testing::{Bytes, Loopback};
use crate::wire::ops::{self, ExportToken};
use crate::wire::{Reader, Wire, Writer};
use crate::{
    AllocationDesc, BufferDesc, BufferImageCopy, BufferUsage, ContextDesc, CopyRegion, Device,
    Error, Extent3d, Format, ImageDesc, ImageDimension, ImageSubresource, ImageUsage, Loss,
    Origin3d, SoftwareDesc, Submission, SubmitError, DEFAULT_SUBGROUP_SIZE, MAX_UPDATE_BYTES,
};

/// The host's own clock, so a wait's timeout passes for real.
struct HostClock(std::time::Instant);

impl MonotonicClock for HostClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// The test's channel: each answer handed to the call waiting for it, each
/// region kept by its grant for the process that granted it.
#[derive(Default)]
struct TestChannel {
    replies: Mutex<HashMap<u64, Vec<u8>>>,
    answered: Condvar,
    grants: Mutex<HashMap<u64, (ProcId, Arc<Bytes>)>>,
    watched: Mutex<Vec<ProcId>>,
    tokens: AtomicU64,
    woken: Mutex<bool>,
    rouse: Condvar,
}

impl Channel for TestChannel {
    fn reply(&self, ticket: u64, frame: &[u8]) {
        self.replies
            .lock()
            .expect("unpoisoned")
            .insert(ticket, frame.to_vec());
        self.answered.notify_all();
    }

    fn map(&self, client: ProcId, grant: u64, min_len: usize) -> Result<Arc<dyn Region>, Errno> {
        let grants = self.grants.lock().expect("unpoisoned");
        let (owner, bytes) = grants.get(&grant).ok_or(Errno::NotFound)?;
        if *owner != client {
            return Err(Errno::PermissionDenied);
        }
        if bytes.len() < min_len {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(Arc::clone(bytes) as Arc<dyn Region>)
    }

    fn watch(&self, client: ProcId) -> Result<(), Errno> {
        self.watched.lock().expect("unpoisoned").push(client);
        Ok(())
    }

    fn unwatch(&self, client: ProcId) {
        self.watched
            .lock()
            .expect("unpoisoned")
            .retain(|watched| *watched != client);
    }

    // Counted, not random: these tests guess no token, and a test of a guess
    // would name one no client was handed.
    fn token(&self) -> Result<ExportToken, Errno> {
        let mut token = ExportToken::default();
        token[..8].copy_from_slice(&(self.tokens.fetch_add(1, Ordering::SeqCst) + 1).to_le_bytes());
        Ok(token)
    }

    fn wake(&self) {
        *self.woken.lock().expect("unpoisoned") = true;
        self.rouse.notify_all();
    }
}

/// A device served on the test's channel, its loop on a thread of its own and
/// its requests run on `workers` workers, or on each caller's thread where
/// there are none.
struct Served {
    device: Device,
    channel: Arc<TestChannel>,
    server: Arc<Server>,
    workers: usize,
    threads: crate::device_tests::HostThreads,
    tickets: AtomicU64,
    grants: AtomicU64,
    stop: Arc<AtomicBool>,
    looping: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Served {
    fn new(memory: u64) -> Arc<Self> {
        Self::with_workers(memory, 2)
    }

    fn with_workers(memory: u64, workers: usize) -> Arc<Self> {
        let clock: Arc<dyn MonotonicClock + Send + Sync> =
            Arc::new(HostClock(std::time::Instant::now()));
        let device = Device::software(&SoftwareDesc {
            memory_bytes: memory,
            clock: Arc::clone(&clock),
            runner: Arc::new(tairix_parallel::Serial),
            threads: &crate::NoThreads,
            subgroup_size: DEFAULT_SUBGROUP_SIZE,
        })
        .expect("a software device");
        Self::serving(device, workers, true)
    }

    /// `device` served, its requests run on `workers` workers; its loop on a
    /// thread of its own when `looped`, and otherwise turned by the test.
    fn serving(device: Device, workers: usize, looped: bool) -> Arc<Self> {
        let channel = Arc::new(TestChannel::default());
        let local = device.served.clone().expect("in this process");
        let server = Server::new(Arc::clone(&local), Arc::clone(&channel) as Arc<dyn Channel>);
        let stop = Arc::new(AtomicBool::new(false));
        let looping = looped.then(|| {
            let (server, channel, stop) =
                (Arc::clone(&server), Arc::clone(&channel), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let deadline = server.deadline();
                    let mut woken = channel.woken.lock().expect("unpoisoned");
                    if !*woken {
                        woken = match deadline {
                            Some(at) => {
                                let left = at.saturating_sub(local.now());
                                channel
                                    .rouse
                                    .wait_timeout(woken, std::time::Duration::from_nanos(left))
                                    .expect("unpoisoned")
                                    .0
                            }
                            None => channel.rouse.wait(woken).expect("unpoisoned"),
                        };
                    }
                    let moved = core::mem::take(&mut *woken);
                    drop(woken);
                    if moved {
                        server.moved();
                    }
                    server.expire(local.now());
                }
            })
        });
        let threads = crate::device_tests::HostThreads::default();
        let workers = server.start_workers(&threads, workers);
        Arc::new(Self {
            device,
            channel,
            server,
            workers,
            threads,
            tickets: AtomicU64::new(0),
            grants: AtomicU64::new(0),
            stop,
            looping: Mutex::new(looping),
        })
    }

    /// The device as process `proc` reaches it, through a region of
    /// `region` bytes.
    fn connect(self: &Arc<Self>, proc: u8, region: usize) -> Result<Device, Errno> {
        let proc = ProcId::from_raw([proc; tairix_abi::PROC_ID_LEN]);
        let bytes = Arc::new(Bytes::zeroed(region));
        let grant = self.grants.fetch_add(1, Ordering::SeqCst) + 1;
        self.channel
            .grants
            .lock()
            .expect("unpoisoned")
            .insert(grant, (proc, Arc::clone(&bytes)));
        let link = TestLink {
            served: Arc::clone(self),
            proc,
            bytes,
        };
        let remote = Remote::open(Box::new(link), grant)?;
        Ok(Device {
            host: Arc::new(remote),
            served: None,
        })
    }

    /// Process `proc` exits.
    fn exit(&self, proc: u8) {
        self.server
            .client_exited(ProcId::from_raw([proc; tairix_abi::PROC_ID_LEN]));
    }

    /// The host memory the device's heap has charged.
    fn heap_taken(&self) -> u64 {
        crate::device::as_software(&self.device.host)
            .model
            .lock()
            .work_heap_taken()
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.channel.wake();
        if let Some(looping) = self.looping.lock().expect("unpoisoned").take() {
            looping.join().expect("the loop ran to its stop");
        }
        self.server.close();
        self.threads.join();
    }
}

/// One client's channel to the served device.
struct TestLink {
    served: Arc<Served>,
    proc: ProcId,
    bytes: Arc<Bytes>,
}

impl Link for TestLink {
    fn call(&self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let ticket = self.served.tickets.fetch_add(1, Ordering::SeqCst) + 1;
        self.served
            .server
            .serve(ticket, self.proc, request, self.served.workers);
        let channel = &self.served.channel;
        let mut replies = channel.replies.lock().expect("unpoisoned");
        loop {
            if let Some(frame) = replies.remove(&ticket) {
                reply[..frame.len()].copy_from_slice(&frame);
                return Ok(frame.len());
            }
            replies = channel.answered.wait(replies).expect("unpoisoned");
        }
    }

    fn region_len(&self) -> usize {
        self.bytes.len()
    }

    fn write_region(&self, at: usize, data: &[u8]) {
        self.bytes.write(at, data);
    }

    fn read_region(&self, at: usize, out: &mut [u8]) {
        self.bytes.read(at, out);
    }

    fn grant(&self, region: u64) -> Result<u64, Errno> {
        Ok(region)
    }
}

const MEMORY: u64 = 8 << 20;
const REGION: usize = 1 << 20;

/// What `scenario` answers of the software device directly and of the same
/// device served, which must agree.
fn agrees<T: core::fmt::Debug + PartialEq>(scenario: impl Fn(&Device) -> T) {
    let served = Served::new(MEMORY);
    let direct = scenario(&served.device);
    let client = served.connect(1, REGION).expect("connected");
    assert_eq!(
        scenario(&client),
        direct,
        "the served device agrees with it"
    );
}

fn region(src_offset: u64, dst_offset: u64, size: u64) -> CopyRegion {
    CopyRegion {
        src_offset,
        dst_offset,
        size,
    }
}

#[test]
fn copies_fills_and_updates_land_alike() {
    agrees(|device| {
        let context = context(device);
        let (src_memory, src) = buffer(&context, 256);
        let (dst_memory, dst) = buffer(&context, 256);
        let mut commands = context.command_buffer();
        commands
            .fill_buffer(&src, 0, 64, 0xAABB_CCDD)
            .expect("recorded");
        let update: Vec<u8> = (0..64).collect();
        commands.update_buffer(&src, 64, &update).expect("recorded");
        commands
            .copy_buffer(&src, &dst, &[region(0, 128, 128), region(128, 0, 64)])
            .expect("recorded");
        let fence = submit(&queue(&context), &commands).expect("admitted");
        fence.wait(Duration64::from_secs(5)).expect("done");
        (contents(&src_memory), contents(&dst_memory))
    });
}

#[test]
fn refusals_are_the_same_refusals() {
    agrees(|device| {
        let context = context(device);
        let tight = device
            .create_context(&ContextDesc {
                memory_limit: Some(4096),
            })
            .expect("a context");
        let mut refused: Vec<Error> = Vec::new();
        let allocate = |context: &crate::Context, size, memory_type| {
            context
                .allocate(&AllocationDesc { size, memory_type })
                .err()
        };
        refused.extend(allocate(&context, 0, 0));
        refused.extend(allocate(&context, 64, 99));
        refused.extend(allocate(&tight, 1 << 20, 0));
        let allocation = memory(&context, 256);
        let made = |offset, size| {
            context
                .create_buffer(
                    &allocation,
                    &BufferDesc {
                        offset,
                        size,
                        usage: BufferUsage::ALL,
                    },
                )
                .err()
        };
        refused.extend(made(3, 16));
        refused.extend(made(0, 512));
        // Bits no usage names reach the device as they are, so it judges them
        // in its own order: here a description of no levels is empty first.
        let unnamed = ImageDesc {
            dimension: ImageDimension::D2,
            format: Format::Rgba8Unorm,
            extent: Extent3d {
                width: 4,
                height: 4,
                depth: 1,
            },
            mip_levels: 0,
            array_layers: 1,
            samples: 1,
            usage: ImageUsage::from_bits(1 << 6),
        };
        refused.extend(context.image_requirements(&unnamed).err());
        refused.extend(
            context
                .image_requirements(&ImageDesc {
                    mip_levels: 1,
                    ..unnamed
                })
                .err(),
        );
        refused.extend(
            context
                .create_buffer(
                    &allocation,
                    &BufferDesc {
                        offset: 0,
                        size: 16,
                        usage: BufferUsage::from_bits(1 << 30),
                    },
                )
                .err(),
        );
        let buffer = over(&context, &allocation, 0, 256);
        let mut overlapping = context.command_buffer();
        overlapping
            .copy_buffer(&buffer, &buffer, &[region(0, 8, 32)])
            .expect("recorded");
        let submitted = submit(&queue(&context), &overlapping).err();
        let timeline = context.create_timeline(5).expect("a timeline");
        refused.extend(timeline.signal(4).err());
        let late = queue(&context)
            .submit(&Submission {
                command_buffers: &[],
                waits: &[],
                signals: &[],
                deadline: Duration64::ZERO,
            })
            .err();
        (refused, submitted, late)
    });
}

#[test]
fn timelines_and_fences_move_alike() {
    agrees(|device| {
        let context = context(device);
        let timeline = context.create_timeline(5).expect("a timeline");
        let mut seen = alloc::vec![timeline.value()];
        timeline.signal(7).expect("advances");
        seen.push(timeline.value());
        let reached = timeline.wait(7, Duration64::ZERO);
        let timed_out = timeline.wait(9, Duration64::from_millis(20));
        let (_memory, target) = buffer(&context, 64);
        let mut commands = context.command_buffer();
        commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
        let fence = queue(&context)
            .submit(&Submission {
                command_buffers: &[&commands],
                waits: &[(&timeline, 7)],
                signals: &[(&timeline, 10)],
                deadline: Duration64::from_secs(1),
            })
            .expect("admitted");
        let done = fence.wait(Duration64::from_secs(5));
        seen.push(timeline.value());
        (seen, reached, timed_out, done, fence.is_signalled())
    });
}

#[test]
fn memory_crosses_between_contexts_only_once_imported_alike() {
    agrees(|device| {
        let owner = context(device);
        let other = context(device);
        let allocation = memory(&owner, 64);
        allocation.write(0, &[9; 64]).expect("host-visible");
        let imported = other
            .import(allocation.export().expect("exported"))
            .expect("imported");
        let mut seen = [0u8; 64];
        imported.read(0, &mut seen).expect("host-visible");
        (seen.to_vec(), imported.size(), imported.memory_type())
    });
}

#[test]
fn images_are_copied_in_and_out_alike() {
    agrees(|device| {
        let context = context(device);
        let desc = ImageDesc {
            dimension: ImageDimension::D2,
            format: Format::Rgba8Unorm,
            extent: Extent3d {
                width: 4,
                height: 4,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: 1,
            usage: ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST),
        };
        let requirements = context.image_requirements(&desc).expect("an image");
        let placement = memory(&context, requirements.size);
        let image = context.create_image(&placement, 0, &desc).expect("placed");
        let (src_memory, src) = buffer(&context, 64);
        let (dst_memory, dst) = buffer(&context, 64);
        src_memory
            .write(0, &(0..64).collect::<Vec<u8>>())
            .expect("host-visible");
        let copy = BufferImageCopy {
            buffer_offset: 0,
            bytes_per_row: 16,
            rows_per_image: 4,
            image: ImageSubresource {
                aspect: crate::Aspects::COLOR,
                level: 0,
                base_layer: 0,
                layers: 1,
            },
            origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: desc.extent,
        };
        let mut commands = context.command_buffer();
        commands
            .copy_buffer_to_image(&src, &image, &[copy])
            .expect("recorded");
        commands
            .copy_image_to_buffer(&image, &dst, &[copy])
            .expect("recorded");
        submit(&queue(&context), &commands)
            .expect("admitted")
            .wait(Duration64::from_secs(5))
            .expect("done");
        (requirements, contents(&dst_memory))
    });
}

/// A copy out of an image the context does not hold is refused as it is
/// uploaded: the bytes its regions write are decided by the image, so it is
/// never decided against an image the context makes under that key later.
#[test]
fn an_upload_copying_out_of_an_image_its_context_lacks_is_refused_as_it_arrives() {
    let loopback = Loopback::new(MEMORY, Arc::new(tairix_parallel::Serial)).expect("served");
    // Keys as a fresh context gives its first image and buffer.
    let typed = loopback.connect(2, REGION).expect("connected");
    let theirs = context(&typed);
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::Rgba8Unorm,
        extent: Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage: ImageUsage::TRANSFER_SRC,
    };
    let placement = memory(
        &theirs,
        theirs.image_requirements(&desc).expect("an image").size,
    );
    let image = theirs.create_image(&placement, 0, &desc).expect("placed");
    let (_memory, target) = buffer(&theirs, 64);
    let grant = loopback.grant(1, GPU_REGION_MIN);
    let open = loopback.serve(1, &frame(GpuOp::Open, &ops::Open { grant }));
    assert!(answer::<Empty>(&open.replies, open.ticket, GpuOp::Open).is_ok());
    let made = loopback.serve(
        1,
        &frame(
            GpuOp::CreateContext,
            &ops::CreateContext { memory_limit: None },
        ),
    );
    let ops::OfContext { context } =
        answer(&made.replies, made.ticket, GpuOp::CreateContext).expect("a context");
    let whole = BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row: 16,
        rows_per_image: 4,
        image: ImageSubresource {
            aspect: crate::Aspects::COLOR,
            level: 0,
            base_layer: 0,
            layers: 1,
        },
        origin: Origin3d::default(),
        extent: desc.extent,
    };
    let mut bytes = 1u32.to_le_bytes().to_vec();
    let copy = crate::command::Command::ImageToBuffer {
        src: image.key,
        dst: target.key,
        regions: alloc::vec![whole, whole],
        overlapping: false,
    };
    crate::wire::put_command(&mut Writer::new(&mut bytes), &copy).expect("encoded");
    loopback.write_region(grant, 0, &bytes);
    let span = tairix_abi::gpu::RegionSpan {
        offset: 0,
        len: u32::try_from(bytes.len()).expect("small"),
    };
    let upload = ops::Upload {
        context,
        last: true,
        span,
    };
    let uploaded = loopback.serve(1, &frame(GpuOp::Upload, &upload));
    assert_eq!(
        answer::<Empty>(&uploaded.replies, uploaded.ticket, GpuOp::Upload).err(),
        Some(GpuError::Stale)
    );
}

#[test]
fn a_compute_pipeline_dispatches_alike() {
    agrees(|device| {
        let context = context(device);
        let rig = rig(&context, &doubles(4), 64, &[]).expect("a pipeline");
        run(&context, &rig, [4, 1, 1]).expect("ran");
        words(&rig.memory, 16)
    });
}

#[test]
fn a_command_buffer_longer_than_the_region_crosses_in_parts() {
    let served = Served::new(MEMORY);
    let client = served.connect(1, GPU_REGION_MIN).expect("connected");
    let context = context(&client);
    let size = 8 * MAX_UPDATE_BYTES as u64;
    let (memory, target) = buffer(&context, size);
    let mut commands = context.command_buffer();
    for part in 0..8u8 {
        let data = alloc::vec![part; MAX_UPDATE_BYTES];
        commands
            .update_buffer(&target, u64::from(part) * MAX_UPDATE_BYTES as u64, &data)
            .expect("recorded");
    }
    submit(&queue(&context), &commands)
        .expect("admitted")
        .wait(Duration64::from_secs(5))
        .expect("done");
    let seen = contents(&memory);
    for (part, chunk) in seen.chunks(MAX_UPDATE_BYTES).enumerate() {
        assert!(
            chunk.iter().all(|&byte| usize::from(byte) == part),
            "part {part}"
        );
    }
}

#[test]
fn a_client_names_no_context_of_another_however_it_forges_the_id() {
    let served = Served::new(MEMORY);
    let first = served.connect(1, REGION).expect("connected");
    let second = served.connect(2, REGION).expect("connected");
    let theirs = context(&first);
    assert_eq!(second.host.status(theirs.key), Err(Error::Stale));
    assert_eq!(
        second.host.allocate(
            theirs.key,
            &AllocationDesc {
                size: 64,
                memory_type: 0
            }
        ),
        Err(Error::Stale)
    );
    // Their context goes on, untouched.
    assert_eq!(theirs.status(), Ok(()));
    assert_eq!(theirs.memory_used().allocations, 0);
}

#[test]
fn a_refused_request_changes_nothing() {
    let served = Served::new(MEMORY);
    let client = served.connect(1, REGION).expect("connected");
    let context = client
        .create_context(&ContextDesc {
            memory_limit: Some(64 << 10),
        })
        .expect("a context");
    let before = context.memory_used();
    let refused = context.allocate(&AllocationDesc {
        size: 1 << 20,
        memory_type: 0,
    });
    assert_eq!(refused.err(), Some(Error::OverLimit));
    assert_eq!(context.memory_used(), before);
}

#[test]
fn a_wait_is_answered_as_its_value_lands_its_context_ends_or_its_timeout_passes() {
    let served = Served::new(MEMORY);
    let client = served.connect(1, REGION).expect("connected");
    let context = context(&client);
    let timeline = Arc::new(context.create_timeline(0).expect("a timeline"));
    let waiting = {
        let timeline = Arc::clone(&timeline);
        std::thread::spawn(move || timeline.wait(3, Duration64::from_secs(60)))
    };
    timeline.signal(3).expect("advances");
    assert_eq!(waiting.join().expect("the waiter"), Ok(()));
    assert_eq!(
        timeline.wait(4, Duration64::from_millis(20)),
        Err(Error::TimedOut)
    );
    let ending = {
        let timeline = Arc::clone(&timeline);
        std::thread::spawn(move || timeline.wait(4, Duration64::from_secs(60)))
    };
    assert!(
        crate::device_tests::until(|| served.server.waiting() == 1),
        "the wait is held"
    );
    drop(context);
    assert_eq!(ending.join().expect("the waiter"), Err(Error::Destroyed));
}

#[test]
fn a_wait_on_work_an_overrun_lost_is_answered_lost_however_late_the_loop_looks() {
    let clock = Arc::new(crate::device_tests::Clock::default());
    let (device, _log) = crate::hardware::tests::device(&clock);
    let served = Served::serving(device, 0, false);
    let client = served.connect(1, REGION).expect("connected");
    let context = context(&client);
    let (_target_memory, target) = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
    let fence = submit(&queue(&context), &commands).expect("admitted");
    // Past the job's deadline, so it overruns first.
    let timeout = Duration64::from_secs(2);
    assert!(timeout > crate::device_tests::SECOND);
    let waiting = std::thread::spawn(move || fence.wait(timeout));
    assert!(
        crate::device_tests::until(|| served.server.waiting() == 1),
        "the wait is held"
    );
    // The loop looks first once both are past, the loss the overrun makes
    // not yet looked at.
    let late = timeout.saturating_total_nanos();
    clock.now.store(late, Ordering::SeqCst);
    served.server.expire(late);
    assert_eq!(
        waiting.join().expect("the waiter"),
        Err(Error::Lost(Loss::Overran))
    );
}

/// `body` framed as a request for `op`.
fn frame(op: GpuOp, body: &impl Wire) -> Vec<u8> {
    let mut bytes = Vec::new();
    body.put(&mut Writer::new(&mut bytes)).expect("encoded");
    let mut frame = alloc::vec![0; tairix_abi::gpu::GPU_MAX_REQUEST];
    let len = GpuRequest { op, body: &bytes }
        .encode(&mut frame)
        .expect("framed");
    frame.truncate(len);
    frame
}

/// The body `answered` replied to `ticket` with, read as `T`, or the error
/// the device refused it for.
fn answer<T: Wire>(answered: &[(u64, Vec<u8>)], ticket: u64, op: GpuOp) -> Result<T, GpuError> {
    let (_, reply) = answered
        .iter()
        .find(|(answered, _)| *answered == ticket)
        .expect("answered");
    let (status, body) = decode_reply(reply, op).expect("a reply");
    match status {
        GpuStatus::Ok => {
            let mut reader = Reader::new(body);
            Ok(T::take(&mut reader).expect("its body"))
        }
        GpuStatus::Device(error) => Err(error),
        GpuStatus::Refused(errno) => panic!("refused: {errno:?}"),
    }
}

#[test]
fn a_client_that_closes_has_its_held_waits_answered_as_destroyed() {
    let loopback = Loopback::new(MEMORY, Arc::new(tairix_parallel::Serial)).expect("served");
    let grant = loopback.grant(1, GPU_REGION_MIN);
    let open = loopback.serve(1, &frame(GpuOp::Open, &ops::Open { grant }));
    assert!(answer::<Empty>(&open.replies, open.ticket, GpuOp::Open).is_ok());
    let made = loopback.serve(
        1,
        &frame(
            GpuOp::CreateContext,
            &ops::CreateContext { memory_limit: None },
        ),
    );
    let ops::OfContext { context } =
        answer(&made.replies, made.ticket, GpuOp::CreateContext).expect("a context");
    let made = loopback.serve(
        1,
        &frame(
            GpuOp::CreateTimeline,
            &ops::CreateTimeline {
                context,
                initial: 0,
            },
        ),
    );
    let made: ops::Made =
        answer(&made.replies, made.ticket, GpuOp::CreateTimeline).expect("a timeline");
    let wait = ops::Wait {
        context,
        timeline: made.key().expect("its key"),
        value: 1,
        timeout: Duration64::from_secs(60),
    };
    let waiting = loopback.serve(1, &frame(GpuOp::Wait, &wait));
    assert!(waiting.replies.is_empty(), "the wait is held");
    let closed = loopback.serve(1, &frame(GpuOp::Close, &Empty));
    assert!(answer::<Empty>(&closed.replies, closed.ticket, GpuOp::Close).is_ok());
    assert_eq!(
        answer::<Empty>(&closed.replies, waiting.ticket, GpuOp::Wait).err(),
        Some(GpuError::Destroyed),
        "the client lives on, so its wait is answered"
    );
}

#[test]
fn a_clients_exit_lets_go_of_everything_it_held() {
    let served = Served::new(MEMORY);
    // The device's room for a context stays once made, as the model keeps
    // it: what the client takes is measured past it.
    drop(context(&served.device));
    let idle = served.heap_taken();
    let client = served.connect(1, REGION).expect("connected");
    let context = context(&client);
    let _held = memory(&context, 1 << 20);
    assert!(served.heap_taken() > idle);
    served.exit(1);
    assert_eq!(served.heap_taken(), idle, "its contexts and memory went");
    // What it still holds names nothing now.
    assert_eq!(context.status(), Err(Error::Lost(crate::Loss::Fault)));
}

#[test]
fn a_client_holds_no_more_than_its_bound_of_contexts() {
    let served = Served::new(MEMORY);
    let client = served.connect(1, REGION).expect("connected");
    let held: Vec<crate::Context> = (0..MAX_CLIENT_CONTEXTS).map(|_| context(&client)).collect();
    assert_eq!(
        client.create_context(&ContextDesc::default()).err(),
        Some(Error::TooMany)
    );
    drop(held);
    assert!(client.create_context(&ContextDesc::default()).is_ok());
}

#[test]
fn a_region_outside_the_channels_bounds_is_refused() {
    let served = Served::new(MEMORY);
    assert_eq!(
        served.connect(1, GPU_REGION_MIN - 1).err(),
        Some(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_submission_names_its_refused_command_alike() {
    agrees(|device| {
        let context = context(device);
        let (_memory, target) = buffer(&context, 64);
        let mut commands = context.command_buffer();
        commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
        commands.fill_buffer(&target, 60, 8, 1).expect("recorded");
        submit(&queue(&context), &commands)
            .err()
            .map(|SubmitError { command, error }| (command, error))
    });
}

#[test]
fn requests_run_on_the_callers_thread_where_there_are_no_workers() {
    let served = Served::with_workers(MEMORY, 0);
    let client = served.connect(1, REGION).expect("connected");
    let context = context(&client);
    let (memory, target) = buffer(&context, 64);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 64, 7).expect("recorded");
    submit(&queue(&context), &commands)
        .expect("admitted")
        .wait(Duration64::from_secs(5))
        .expect("done");
    assert!(contents(&memory)
        .chunks(4)
        .all(|word| word == 7u32.to_le_bytes()));
}

#[test]
fn clients_served_at_once_each_see_only_their_own_work() {
    let served = Served::new(MEMORY);
    let clients: Vec<_> = (1..=4u8)
        .map(|proc| {
            let client = served.connect(proc, REGION).expect("connected");
            std::thread::spawn(move || {
                let context = context(&client);
                let (memory, target) = buffer(&context, 256);
                for round in 0..8u32 {
                    let mut commands = context.command_buffer();
                    let word = u32::from(proc) << 8 | round;
                    commands
                        .fill_buffer(&target, 0, 256, word)
                        .expect("recorded");
                    submit(&queue(&context), &commands)
                        .expect("admitted")
                        .wait(Duration64::from_secs(5))
                        .expect("done");
                    let seen = contents(&memory);
                    assert!(seen.chunks(4).all(|bytes| bytes == word.to_le_bytes()));
                }
            })
        })
        .collect();
    for client in clients {
        client.join().expect("a client ran to its end");
    }
}

#[test]
fn every_command_but_a_pass_fits_the_smallest_region_at_the_seams_bounds() {
    use crate::wire::put_command;
    let served = Served::new(MEMORY);
    let context = context(&served.device);
    let (_memory, buffer) = buffer(&context, 1 << 20);
    let desc = ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::Rgba8Unorm,
        extent: Extent3d {
            width: 64,
            height: 64,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage: ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST),
    };
    let placement = memory(
        &context,
        context.image_requirements(&desc).expect("an image").size,
    );
    let image = context.create_image(&placement, 0, &desc).expect("placed");
    let subresource = ImageSubresource {
        aspect: crate::Aspects::COLOR,
        level: 0,
        base_layer: 0,
        layers: 1,
    };
    let to_image = BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row: 0,
        rows_per_image: 0,
        image: subresource,
        origin: Origin3d { x: 0, y: 0, z: 0 },
        extent: Extent3d {
            width: 1,
            height: 1,
            depth: 1,
        },
    };
    let image_copy = crate::ImageCopy {
        src: subresource,
        src_origin: Origin3d { x: 0, y: 0, z: 0 },
        dst: subresource,
        dst_origin: Origin3d { x: 1, y: 0, z: 0 },
        extent: to_image.extent,
    };
    let range = crate::ImageRange {
        aspects: crate::Aspects::COLOR,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let mut commands = context.command_buffer();
    commands
        .update_buffer(&buffer, 0, &alloc::vec![0; MAX_UPDATE_BYTES])
        .expect("recorded");
    commands
        .copy_buffer(
            &buffer,
            &buffer,
            &alloc::vec![region(0, 8, 4); crate::MAX_COPY_REGIONS],
        )
        .expect("recorded");
    commands
        .copy_buffer_to_image(
            &buffer,
            &image,
            &alloc::vec![to_image; crate::MAX_COPY_REGIONS],
        )
        .expect("recorded");
    commands
        .copy_image_to_buffer(
            &image,
            &buffer,
            &alloc::vec![to_image; crate::MAX_COPY_REGIONS],
        )
        .expect("recorded");
    commands
        .copy_image(
            &image,
            &image,
            &alloc::vec![image_copy; crate::MAX_COPY_REGIONS],
        )
        .expect("recorded");
    commands
        .clear_image(
            &image,
            crate::ClearValue::Uint([0; 4]),
            &alloc::vec![range; crate::MAX_COPY_REGIONS],
        )
        .expect("recorded");
    let room = GPU_REGION_MIN - size_of::<u32>();
    for command in commands.commands() {
        let mut bytes = Vec::new();
        put_command(&mut Writer::new(&mut bytes), command).expect("encoded");
        assert!(bytes.len() <= room, "{} bytes", bytes.len());
    }
}

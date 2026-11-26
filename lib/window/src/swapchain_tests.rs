use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::display::DamageRect;
use tairix_abi::time::MonotonicClock;
use tairix_abi::window_ipc::CursorShape;
use tairix_gpu::testing::Loopback as ServedGpu;
use tairix_gpu::{ContextDesc, NoThreads, QueueCapabilities, SoftwareDesc, DEFAULT_SUBGROUP_SIZE};
use tairix_parallel::Serial;

use super::*;
use crate::client::PayloadRegion;
use crate::tests::{connected, Client, Loopback, EVENTS_A};

/// A clock that never moves: no deadline in these tests is reached.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// The window's client size.
const SIZE: (u32, u32) = (8, 6);
/// Bytes of one row of its frames.
const STRIDE: usize = 8 * 4;
/// Bytes of one frame.
const FRAME: usize = STRIDE * 6;
/// The handle the loopback grants the swapchain's first region, and the next.
const FIRST_GRANT: u64 = 0x44;

/// The software device, a context on it, and a queue that copies.
fn gpu() -> (Device, Context, Queue) {
    let device = Device::software(&SoftwareDesc {
        memory_bytes: 1 << 26,
        clock: Arc::new(Still),
        runner: Arc::new(Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device");
    on(device)
}

/// A context on `device`, and a queue of it that copies.
fn on(device: Device) -> (Device, Context, Queue) {
    let context = device
        .create_context(&ContextDesc::default())
        .expect("a context");
    let family = device
        .info()
        .queue_families
        .iter()
        .position(|family| family.capabilities.contains(QueueCapabilities::TRANSFER))
        .expect("a family that copies");
    let queue = context
        .queue(u32::try_from(family).expect("small"), 0)
        .expect("a queue");
    (device, context, queue)
}

/// Pixel bytes of a frame whose row `y`, column `x` holds `(x, y, salt, 255)`.
fn picture(salt: u8) -> Vec<u8> {
    let mut bytes = vec![0; FRAME];
    for (at, pixel) in bytes.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (at % 8, at / 8);
        pixel.copy_from_slice(&[
            u8::try_from(x).expect("small"),
            u8::try_from(y).expect("small"),
            salt,
            255,
        ]);
    }
    bytes
}

/// Draw `bytes`, a whole frame's pixels, into `image` on `queue`.
fn draw(context: &Context, queue: &Queue, image: &Image, bytes: &[u8]) {
    let size = u64::try_from(bytes.len()).expect("small");
    let memory = context
        .allocate(&AllocationDesc {
            size,
            memory_type: 0,
        })
        .expect("memory");
    memory.write(0, bytes).expect("written");
    let source = context
        .create_buffer(
            &memory,
            &BufferDesc {
                offset: 0,
                size,
                usage: BufferUsage::TRANSFER_SRC,
            },
        )
        .expect("a buffer");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer_to_image(
            &source,
            image,
            &[BufferImageCopy {
                buffer_offset: 0,
                bytes_per_row: u32::try_from(STRIDE).expect("small"),
                rows_per_image: SIZE.1,
                image: ImageSubresource {
                    aspect: Aspects::COLOR,
                    level: 0,
                    base_layer: 0,
                    layers: 1,
                },
                origin: Origin3d::default(),
                extent: Extent3d {
                    width: SIZE.0,
                    height: SIZE.1,
                    depth: 1,
                },
            }],
        )
        .expect("recorded");
    queue
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline: deadline(),
        })
        .expect("submitted")
        .wait(deadline())
        .expect("drawn");
}

/// How long the tests' work may run: the clock never moves, so any bound.
const fn deadline() -> Duration64 {
    Duration64::from_secs(1)
}

/// What a swapchain over `loopback` opens.
fn opened(
    loopback: &Rc<RefCell<Loopback>>,
    (device, context): (&Device, &Context),
    present_mode: PresentMode,
) -> (Client, Swapchain<Vec<u8>>) {
    let mut client = connected(loopback);
    let spec = WindowSpec {
        event_endpoint: EVENTS_A,
        title: "gpu",
        sizing: WindowSizing::default(),
        size: SIZE,
        format: DisplayFormat::Bgra8888,
    };
    let waits = Waits {
        name: "test",
        set: 0,
        token: 0,
    };
    let chain = Swapchain::open(&mut client, (device, context), (&spec, present_mode, waits))
        .expect("opened");
    (client, chain)
}

/// The whole of the window.
fn whole() -> DamageList {
    DamageList::new(&[DamageRect {
        x: 0,
        y: 0,
        width_px: SIZE.0,
        height_px: SIZE.1,
    }])
    .expect("one rectangle")
}

/// The process the served tests' application is.
const APP: u8 = 1;

/// A window transport whose regions the served GPU's driver maps too.
struct Served {
    window: Rc<RefCell<Loopback>>,
    gpu: Arc<ServedGpu>,
}

/// A frame region granted to the session and to the GPU's driver. Its bytes
/// here are not the driver's, which the test reads through the loopback.
struct Granted {
    grant: u64,
    bytes: Vec<u8>,
}

impl PayloadRegion for Granted {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Attachable for Granted {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    fn region(&self) -> Option<u64> {
        Some(self.grant)
    }
}

impl WindowTransport for Served {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        self.window.call(request, reply)
    }

    fn post(&mut self, request: &[u8]) -> Result<u64, Errno> {
        self.window.post(request)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        self.window.reap(ticket, reply)
    }

    fn await_reply(&mut self, ticket: u64, reply: &mut [u8]) -> Result<usize, Errno> {
        self.window.await_reply(ticket, reply)
    }

    type Region = Granted;

    fn grant_region(&mut self, len: usize) -> Result<(Granted, u64), Errno> {
        let (_, handle) = self.window.grant_region(len)?;
        let grant = self.gpu.grant(APP, len);
        Ok((
            Granted {
                grant,
                bytes: vec![0; len],
            },
            handle,
        ))
    }
}

#[test]
fn a_fill_reads_the_rows_a_frame_lacks_straight_from_the_image_into_the_frame() {
    let (device, context, queue) = gpu();
    let mode = frames_mode(SIZE, DisplayFormat::Bgra8888).expect("a shape");
    let (target, staging) = target(&device, &context, &mode).expect("a target");
    draw(&context, &queue, &target.image, &picture(9));
    let mut filler = Filler {
        frames: Some(device.attach(vec![0u8; 2 * FRAME]).expect("attached")),
        staging: Some(staging),
    };
    let deadline = deadline();
    let mut job = fill_job::<Vec<u8>>((&target, &mode, deadline), (1, (2, 5)), (&context, &queue))
        .expect("the copy queued");
    assert_eq!(work(&mut filler, &mut job), Some(Ok(())));
    let frames = filler.frames.as_ref().expect("held").memory();
    let mut want = vec![0u8; 2 * FRAME];
    want[FRAME + 2 * STRIDE..FRAME + 5 * STRIDE]
        .copy_from_slice(&picture(9)[2 * STRIDE..5 * STRIDE]);
    assert_eq!(
        frames, &want,
        "the second frame's rows 2 to 4, and nothing else"
    );
    let mut released = Job::Adopt(None);
    assert_eq!(work(&mut filler, &mut released), None);
    let mut after =
        fill_job::<Vec<u8>>((&target, &mode, deadline), (0, (0, 1)), (&context, &queue))
            .expect("the copy queued");
    assert_eq!(
        work(&mut filler, &mut after),
        Some(Err(Error::Stale)),
        "a fill with the frames let go fills nothing"
    );
}

#[test]
fn fifo_hands_the_image_out_again_only_once_the_last_present_finished() {
    let loopback = Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME)]);
    let (device, context, queue) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Fifo);
    assert!(chain.idle(), "nothing presented");
    let image = chain
        .acquire(&mut client)
        .expect("the first frame may be drawn");
    draw(&context, &queue, image, &picture(1));
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Ok(true),
        "filled here, no worker running to wake the loop, and presented"
    );
    assert_eq!(chain.service(&mut client, false), Ok(false));
    assert!(!chain.idle(), "its fate is awaited");
    assert!(
        chain.acquire(&mut client).is_none(),
        "the last present has not finished"
    );
    loopback.borrow_mut().latch();
    let &(window, serial) = loopback
        .borrow()
        .host
        .latched
        .last()
        .expect("the session latched it");
    assert_eq!(window, chain.window());
    let foreign = WindowEvent::FramePresented {
        window_id: window + 1,
        serial,
        at_ns: 5,
        shown: true,
    };
    assert!(!chain.event(&foreign), "another window's fate");
    let fate = WindowEvent::FramePresented {
        window_id: window,
        serial,
        at_ns: 5,
        shown: true,
    };
    assert!(chain.event(&fate));
    assert!(chain.idle(), "every present finished");
    assert_eq!(
        chain.fate(),
        Some(FrameFate {
            serial,
            at_ns: 5,
            shown: true
        })
    );
    let repeated = WindowEvent::FramePresented {
        window_id: window,
        serial,
        at_ns: 9,
        shown: false,
    };
    assert!(chain.event(&repeated), "its window's, so taken");
    assert_eq!(
        chain.fate().map(|fate| fate.at_ns),
        Some(5),
        "a fate no newer than the newest leaves it"
    );
    assert!(chain.acquire(&mut client).is_some(), "on to the next frame");
}

#[test]
fn mailbox_hands_the_image_out_whenever_the_session_has_a_frame_to_give() {
    let loopback = Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME)]);
    let (device, context, queue) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Mailbox);
    for salt in 0..3 {
        let image = chain.acquire(&mut client).expect("a frame to draw");
        draw(&context, &queue, image, &picture(salt));
        assert_eq!(
            chain.present(&mut client, (&context, &queue), whole()),
            Ok(true)
        );
        loopback.borrow_mut().latch();
    }
    let latched: Vec<u64> = loopback
        .borrow()
        .host
        .latched
        .iter()
        .map(|&(_, serial)| serial)
        .collect();
    assert_eq!(latched.len(), 3, "one present a desktop frame");
    assert!(
        latched.windows(2).all(|pair| pair[0] < pair[1]),
        "{latched:?}"
    );
}

#[test]
fn released_frames_draw_nothing_until_reattached_and_a_resize_drops_a_fill_in_flight() {
    let loopback = Loopback::with_regions(&[
        (FIRST_GRANT, 2 * FRAME),
        (FIRST_GRANT + 1, 2 * FRAME),
        (FIRST_GRANT + 2, 2 * 4 * 4 * 3),
    ]);
    let (device, context, queue) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Mailbox);
    chain.release().expect("released");
    assert!(chain.acquire(&mut client).is_none(), "nothing to draw into");
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Err(Refused::Window(Errno::WouldBlock))
    );
    chain
        .reattach(&mut client, (&device, &context))
        .expect("re-attached");
    let image = chain.acquire(&mut client).expect("drawn again");
    draw(&context, &queue, image, &picture(4));
    // No worker runs here, so a fill is in flight only as one is between its
    // answer landing and the loop collecting it.
    let rows = (0, chain.mode.height_px);
    let job = fill_job(
        (&chain.target, &chain.mode, chain.deadline),
        (0, rows),
        (&context, &queue),
    )
    .expect("the copy queued");
    assert_eq!(chain.worker.submit(job).ok(), Some(true), "answered here");
    chain.filling = Some((0, whole()));
    chain
        .resize(&mut client, (&device, &context), (4, 3))
        .expect("resized");
    assert_eq!(
        chain.service(&mut client, false),
        Ok(false),
        "the fill of the frames let go is presented by nobody"
    );
    assert_eq!(chain.mode().width_px, 4);
    assert!(chain.acquire(&mut client).is_some(), "the new frames await");
}

/// A present waiting for room at the endpoint goes with the client's next
/// drain, whenever that runs, so nothing is filled into the frame it names
/// until it has gone: the image is not handed out and a present is put off.
#[test]
fn a_present_waiting_for_room_holds_the_next_frame_back() {
    let loopback = Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME)]);
    let (device, context, queue) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Mailbox);
    {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_posts = true;
        inner.lane = Some(1);
    }
    client
        .set_cursor(chain.window(), CursorShape::Text)
        .expect("posted");
    let image = chain.acquire(&mut client).expect("a frame to draw");
    draw(&context, &queue, image, &picture(1));
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Ok(true)
    );
    assert!(
        matches!(
            client.present_state(chain.window()),
            PresentState::Waiting(_)
        ),
        "with no room it waits"
    );
    assert!(
        chain.acquire(&mut client).is_none(),
        "nothing drawn behind it"
    );
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Err(Refused::Window(Errno::WouldBlock))
    );
    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    assert!(
        chain.acquire(&mut client).is_some(),
        "gone and answered, the next may be drawn"
    );
}

/// A fill that fails leaves every frame to be drawn whole, and the frame drawn
/// next where it was: the other is still the session's.
#[test]
fn a_failed_fill_keeps_the_next_frame_off_the_one_the_session_holds() {
    let loopback = Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME)]);
    let (device, context, queue) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Mailbox);
    let image = chain.acquire(&mut client).expect("a frame to draw");
    draw(&context, &queue, image, &picture(1));
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Ok(true)
    );
    let next = chain.ring.back();
    assert_eq!(next, 1, "frame 0 is the session's");
    let job = fill_job(
        (&chain.target, &chain.mode, chain.deadline),
        (next, (0, chain.mode.height_px)),
        (&context, &queue),
    )
    .expect("the copy queued");
    // Its frames let go under it, the fill fills nothing.
    assert_eq!(chain.worker.submit(Job::Adopt(None)).ok(), Some(true));
    assert_eq!(chain.worker.submit(job).ok(), Some(true));
    chain.filling = Some((next, whole()));
    assert!(
        chain.service(&mut client, false).is_err(),
        "the fill failed"
    );
    assert_eq!(
        chain.ring.back(),
        next,
        "drawn again whole, into that frame"
    );
}

/// A resize the worker has no room for is refused before the session is told,
/// so the window keeps the geometry it had.
#[test]
fn a_resize_with_no_room_for_its_frames_tells_the_session_nothing() {
    let loopback =
        Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME), (FIRST_GRANT + 1, 2 * 4 * 4 * 3)]);
    let (device, context, _) = gpu();
    let (mut client, mut chain) = opened(&loopback, (&device, &context), PresentMode::Mailbox);
    while chain.worker.room() {
        assert!(chain.worker.submit(Job::Adopt(None)).is_ok());
    }
    assert_eq!(
        chain.resize(&mut client, (&device, &context), (4, 3)),
        Err(Refused::Window(Errno::WouldBlock))
    );
    assert!(
        loopback.borrow().host.resized.is_empty(),
        "the session was told nothing"
    );
    assert_eq!(chain.mode().width_px, SIZE.0);
}

#[test]
fn a_served_devices_frame_reaches_the_region_its_driver_maps_as_the_image_held_it() {
    let window = Loopback::with_regions(&[(FIRST_GRANT, 2 * FRAME)]);
    let gpu = ServedGpu::new(1 << 26, Arc::new(Serial)).expect("served");
    let (device, context, queue) = on(gpu.connect(APP, 1 << 20).expect("connected"));
    let mut client = WindowClient::new(Served {
        window: Rc::clone(&window),
        gpu: Arc::clone(&gpu),
    });
    client.connect().expect("connected");
    let spec = WindowSpec {
        event_endpoint: EVENTS_A,
        title: "gpu",
        sizing: WindowSizing::default(),
        size: SIZE,
        format: DisplayFormat::Bgra8888,
    };
    let waits = Waits {
        name: "test",
        set: 0,
        token: 0,
    };
    let mut chain = Swapchain::open(
        &mut client,
        (&device, &context),
        (&spec, PresentMode::Fifo, waits),
    )
    .expect("opened");
    let image = chain
        .acquire(&mut client)
        .expect("the first frame may be drawn");
    draw(&context, &queue, image, &picture(7));
    assert_eq!(
        chain.present(&mut client, (&context, &queue), whole()),
        Ok(true),
        "filled and presented"
    );
    // The frames' grant is the last the application made.
    let grant = u64::try_from(gpu.grants().len()).expect("small");
    let mut frames = vec![0u8; 2 * FRAME];
    gpu.read_region(grant, 0, &mut frames);
    let filled = frames
        .as_chunks::<FRAME>()
        .0
        .iter()
        .filter(|frame| frame[..] == picture(7)[..])
        .count();
    assert_eq!(
        filled, 1,
        "one frame holds the image, copied once by the driver"
    );
}

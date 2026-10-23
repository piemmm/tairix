//! Host tests: the server engine against mock seams, and the client half
//! wired to a real [`DisplayServer`] through a loopback transport, so both
//! halves are proven against the one definition of the protocol.

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::display_ipc::{
    decode_buffer_reply, decode_configure_reply, decode_mode_reply, decode_planes_reply,
    decode_present_reply, decode_stats_reply, ConfiguredFrames, DisplayEvent, DisplayRequest,
    GrantedBuffer, LayerList, Planes, PresentReply, DISPLAY_MAX_BUFFERS, DISPLAY_MAX_FRAMES,
};
use tairix_abi::driver::display::{
    AccelCaps, AccelLayer, AcceleratedDisplay, AllocatedBuffer, BufferId, CursorCaps, DamageList,
    DamageRect, Display, DisplayFormat, DisplayMemory, DisplayMode, DisplayPower, HardwareCursor,
    OutputMode, Presented, RefreshRate, Serviced,
};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::seat::{DisplayLease, ReleaseSurface};
use tairix_abi::time::MonotonicClock;
use tairix_abi::{CapabilityId, DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_geometry::{Rect, Region};

use crate::client::{BufferMapper, DisplayClient, DisplayTransport, RemoteDisplay};
use crate::driver_error_from_errno;
use crate::presenter::{Base, Presenter};
use crate::server::{
    Delivery, DisplayServer, Latch, PeerFacts, DISPLAY_REPLY_MAX, PRESENT_LATCH_DEADLINE_NS,
};

/// 4×3 BGRA test mode, stride one scanline.
const MODE: DisplayMode = DisplayMode {
    width_px: 4,
    height_px: 3,
    stride_bytes: 16,
    format: DisplayFormat::Bgra8888,
};

/// Bytes one MODE frame occupies.
const FRAME_LEN: usize = 48;

/// A narrower mode the output moves to.
const NARROW: DisplayMode = DisplayMode {
    width_px: 2,
    height_px: 3,
    stride_bytes: 8,
    format: DisplayFormat::Bgra8888,
};

const SEAT: u64 = 0;
const TICKET: u64 = 7;
const PORT: u64 = 0x9047_0001;

/// The driver instance that grants the buffers.
const DRIVER: ProcId = ProcId::from_raw([0xD1; PROC_ID_LEN]);

/// What a grant handle adds to the region it names, so a test can tell the
/// two apart.
const GRANT_TAG: u64 = 0x1_0000;

const CAPS: AccelCaps = AccelCaps {
    max_layers: 3,
    max_width_px: 64,
    max_height_px: 64,
    per_layer_opacity: false,
    hw_scaling: false,
};

const CURSOR: CursorCaps = CursorCaps {
    max_width_px: 8,
    max_height_px: 8,
};

/// Nanoseconds the rig's clock advances per reading.
const CLOCK_STEP_NS: u64 = 1_000;

/// A caller oracle scripted per test.
struct MockSeat {
    answer: Result<u64, Errno>,
    holds: Result<bool, Errno>,
    owns_port: bool,
    asked: Vec<(u64, u64)>,
}

impl MockSeat {
    fn live(generation: u64) -> Self {
        Self {
            answer: Ok(generation),
            holds: Ok(false),
            owns_port: true,
            asked: Vec::new(),
        }
    }
}

impl PeerFacts for MockSeat {
    fn live_generation(&mut self, ticket: u64, seat_id: u64) -> Result<u64, Errno> {
        self.asked.push((ticket, seat_id));
        self.answer
    }

    fn holds_capability(&mut self, _ticket: u64, cap: CapabilityId) -> Result<bool, Errno> {
        assert_eq!(cap, CapabilityId::SYSINFO_HW);
        self.holds
    }

    fn owns_port(&mut self, _ticket: u64, port: u64) -> bool {
        self.owns_port && port == PORT
    }
}

/// The kernel's grants and posts, recorded.
#[derive(Default)]
struct MockDelivery {
    grants: Vec<(u64, u64)>,
    refuse_grant: Option<Errno>,
    posts: Vec<(u64, DisplayEvent)>,
    refuse_post: Option<Errno>,
}

impl Delivery for MockDelivery {
    fn grant(&mut self, region: u64, ticket: u64) -> Result<u64, Errno> {
        if let Some(err) = self.refuse_grant {
            return Err(err);
        }
        self.grants.push((region, ticket));
        Ok(region + GRANT_TAG)
    }

    fn grantor(&self) -> ProcId {
        DRIVER
    }

    fn post(&mut self, port: u64, event: &DisplayEvent) -> Result<(), Errno> {
        if let Some(err) = self.refuse_post {
            return Err(err);
        }
        self.posts.push((port, *event));
        Ok(())
    }
}

/// A clock a test advances by hand; every reading also advances one step,
/// so bracketing a present costs exactly one step.
#[derive(Clone)]
struct StepClock {
    now: Rc<RefCell<u64>>,
}

impl StepClock {
    fn new() -> Self {
        Self {
            now: Rc::new(RefCell::new(0)),
        }
    }

    fn advance(&self, ns: u64) {
        *self.now.borrow_mut() += ns;
    }
}

impl MonotonicClock for StepClock {
    fn now_ns(&self) -> u64 {
        let mut now = self.now.borrow_mut();
        let reading = *now;
        *now = reading.saturating_add(CLOCK_STEP_NS);
        reading
    }
}

/// One buffer the mock driver allocated.
#[derive(Copy, Clone, Debug)]
struct MockBuffer {
    id: BufferId,
    width_px: u32,
    height_px: u32,
}

/// A display driver double: it allocates ids, shows what it is told, and
/// says what it was asked.
struct MockDisplay {
    output: Option<DisplayMode>,
    refresh: Option<RefreshRate>,
    frame_count: u32,
    /// Presents answer `Queued` rather than `Shown`.
    queues: bool,
    next: u32,
    live: Vec<MockBuffer>,
    /// Buffers allocated beyond which a further allocation is refused.
    room: usize,
    shown: Vec<(BufferId, Vec<DamageRect>)>,
    layers: Vec<Vec<AccelLayer>>,
    freed: Vec<BufferId>,
    released: Vec<ReleaseSurface>,
    powered: Vec<DisplayPower>,
    power_refusal: Option<DriverError>,
    accel: Option<AccelCaps>,
    cursor: Option<CursorCaps>,
    cursor_ops: Vec<CursorOp>,
    memory: DisplayMemory,
    /// Queued presents the server gave up on.
    abandoned: usize,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CursorOp {
    Set(BufferId, u32, u32),
    Move(i32, i32),
    Hide,
}

impl MockDisplay {
    fn new() -> Self {
        Self {
            output: Some(MODE),
            refresh: None,
            frame_count: 2,
            queues: false,
            next: 1,
            live: Vec::new(),
            room: usize::MAX,
            shown: Vec::new(),
            layers: Vec::new(),
            freed: Vec::new(),
            released: Vec::new(),
            powered: Vec::new(),
            power_refusal: None,
            accel: None,
            cursor: None,
            cursor_ops: Vec::new(),
            memory: DisplayMemory::NONE,
            abandoned: 0,
        }
    }

    fn buffer(&self, id: BufferId) -> Option<MockBuffer> {
        self.live.iter().copied().find(|buffer| buffer.id == id)
    }

    fn outcome(&self) -> Presented {
        if self.queues {
            Presented::Queued
        } else {
            Presented::Shown
        }
    }
}

impl Display for MockDisplay {
    fn output(&self) -> Option<DisplayMode> {
        self.output
    }

    fn refresh(&self) -> Option<RefreshRate> {
        self.refresh
    }

    fn abandon_queued(&mut self) {
        self.abandoned += 1;
    }

    fn memory(&self) -> DisplayMemory {
        self.memory
    }

    fn frame_count(&self) -> u32 {
        self.frame_count
    }

    fn alloc_buffer(
        &mut self,
        width_px: u32,
        height_px: u32,
    ) -> Result<AllocatedBuffer, DriverError> {
        if self.live.len() >= self.room {
            return Err(DriverError::NoSpace);
        }
        let id = BufferId::new(self.next).ok_or(DriverError::NoSpace)?;
        self.next += 1;
        self.live.push(MockBuffer {
            id,
            width_px,
            height_px,
        });
        Ok(AllocatedBuffer {
            id,
            region: u64::from(id.get()) * 0x10,
            stride_bytes: width_px * 4,
        })
    }

    fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
        let at = self
            .live
            .iter()
            .position(|buffer| buffer.id == id)
            .ok_or(DriverError::NotFound)?;
        self.live.remove(at);
        self.freed.push(id);
        Ok(())
    }

    fn present(&mut self, id: BufferId, damage: &[DamageRect]) -> Result<Presented, DriverError> {
        let mode = self.output.ok_or(DriverError::Stale)?;
        DamageRect::validate_list(damage, &mode)?;
        self.buffer(id).ok_or(DriverError::NotFound)?;
        self.shown.push((id, damage.to_vec()));
        Ok(self.outcome())
    }

    fn released(&mut self, surface: ReleaseSurface) {
        self.released.push(surface);
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if let Some(refusal) = self.power_refusal {
            return Err(refusal);
        }
        self.powered.push(power);
        Ok(())
    }

    fn as_accelerated(&mut self) -> Option<&mut dyn AcceleratedDisplay> {
        if self.accel.is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn hardware_cursor(&mut self) -> Option<&mut dyn HardwareCursor> {
        if self.cursor.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl AcceleratedDisplay for MockDisplay {
    fn accel_caps(&self) -> AccelCaps {
        self.accel.unwrap_or(CAPS)
    }

    fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<Presented, DriverError> {
        self.layers.push(layers.to_vec());
        Ok(self.outcome())
    }
}

impl HardwareCursor for MockDisplay {
    fn cursor_caps(&self) -> CursorCaps {
        self.cursor.unwrap_or(CURSOR)
    }

    fn set_cursor(&mut self, id: BufferId, hot_x: u32, hot_y: u32) -> Result<(), DriverError> {
        self.cursor_ops.push(CursorOp::Set(id, hot_x, hot_y));
        Ok(())
    }

    fn move_cursor(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        self.cursor_ops.push(CursorOp::Move(x, y));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), DriverError> {
        self.cursor_ops.push(CursorOp::Hide);
        Ok(())
    }
}

/// A server rig: the engine, the driver double, the caller oracle and the
/// kernel's deliveries.
struct Rig {
    server: DisplayServer<StepClock>,
    clock: StepClock,
    display: MockDisplay,
    seat: MockSeat,
    delivery: MockDelivery,
}

impl Rig {
    fn new() -> Self {
        let clock = StepClock::new();
        Self {
            server: DisplayServer::new(clock.clone()),
            clock,
            display: MockDisplay::new(),
            seat: MockSeat::live(1),
            delivery: MockDelivery::default(),
        }
    }

    fn serve(&mut self, request: &DisplayRequest) -> Vec<u8> {
        let mut reply = [0u8; DISPLAY_REPLY_MAX];
        let len = self.server.serve(
            &mut self.display,
            &mut self.seat,
            &mut self.delivery,
            TICKET,
            &request.to_le_bytes(),
            &mut reply,
        );
        reply[..len].to_vec()
    }

    fn status(&mut self, request: &DisplayRequest) -> Result<(), Errno> {
        decode_status_reply(&self.serve(request))
    }

    fn query(&mut self) -> Result<Option<OutputMode>, Errno> {
        decode_mode_reply(&self.serve(&DisplayRequest::Query { seat_id: SEAT }))
    }

    fn configure_for(&mut self, mode: DisplayMode) -> Result<ConfiguredFrames, Errno> {
        decode_configure_reply(&self.serve(&DisplayRequest::Configure {
            seat_id: SEAT,
            width_px: mode.width_px,
            height_px: mode.height_px,
            format: mode.format,
            events_port: PORT,
        }))
    }

    fn configure(&mut self) -> Result<ConfiguredFrames, Errno> {
        self.configure_for(MODE)
    }

    fn frames(&mut self) -> Vec<BufferId> {
        self.configure()
            .expect("configured")
            .frames()
            .map(|frame| frame.buffer)
            .collect()
    }

    fn present(&mut self, buffer: BufferId, damage: &[DamageRect]) -> Result<PresentReply, Errno> {
        decode_present_reply(&self.serve(&DisplayRequest::Present {
            seat_id: SEAT,
            buffer,
            damage: DamageList::new(damage)?,
        }))
    }

    fn present_layers(&mut self, layers: &[AccelLayer]) -> Result<PresentReply, Errno> {
        decode_present_reply(&self.serve(&DisplayRequest::PresentLayers {
            seat_id: SEAT,
            layers: LayerList::new(layers)?,
        }))
    }

    fn alloc(&mut self, width_px: u32, height_px: u32) -> Result<GrantedBuffer, Errno> {
        decode_buffer_reply(&self.serve(&DisplayRequest::AllocBuffer {
            seat_id: SEAT,
            width_px,
            height_px,
        }))
    }

    fn free(&mut self, buffer: BufferId) -> Result<(), Errno> {
        self.status(&DisplayRequest::FreeBuffer {
            seat_id: SEAT,
            buffer,
        })
    }

    fn planes(&mut self) -> Result<Planes, Errno> {
        decode_planes_reply(&self.serve(&DisplayRequest::QueryPlanes { seat_id: SEAT }))
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), Errno> {
        self.status(&DisplayRequest::SetPower {
            seat_id: SEAT,
            power,
        })
    }

    fn latch(&mut self) -> Latch {
        self.server
            .serviced(Serviced { latched: true }, &mut self.delivery)
    }

    fn lease_moved(&mut self, lease: DisplayLease) {
        self.server.lease_moved(&mut self.display, lease);
    }
}

fn full() -> DamageRect {
    DamageRect::full(&MODE)
}

fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width_px,
        height_px,
    }
}

fn layer(buffer: BufferId, width_px: u32, height_px: u32) -> AccelLayer {
    AccelLayer {
        buffer,
        src: rect(0, 0, width_px, height_px),
        dst_x: 0,
        dst_y: 0,
        dst_width_px: width_px,
        dst_height_px: height_px,
        opacity: u8::MAX,
    }
}

#[test]
fn query_returns_the_mode_to_the_live_owner_only() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.query(),
        Ok(Some(OutputMode {
            mode: MODE,
            refresh: None
        }))
    );
    assert_eq!(rig.seat.asked, vec![(TICKET, SEAT)]);
    for refused in [Errno::SeatNotOwner, Errno::SeatRevoked, Errno::NotFound] {
        rig.seat.answer = Err(refused);
        assert_eq!(rig.query(), Err(refused));
    }
}

#[test]
fn a_query_carries_the_rate_the_device_states_and_none_without_an_output() {
    let mut rig = Rig::new();
    let rate = RefreshRate::from_mhz(144_000);
    rig.display.refresh = rate;
    assert_eq!(
        rig.query(),
        Ok(Some(OutputMode {
            mode: MODE,
            refresh: rate
        }))
    );
    rig.display.output = None;
    assert_eq!(rig.query(), Ok(None), "a rate without an output is nothing");
}

#[test]
fn a_device_read_is_gated_on_hardware_authority_not_on_a_lease() {
    let mut rig = Rig::new();
    rig.seat.answer = Err(Errno::SeatNotOwner);
    rig.display.accel = Some(CAPS);
    rig.display.cursor = Some(CURSOR);
    rig.display.memory = DisplayMemory {
        resident_bytes: 4096,
        total_bytes: 1 << 20,
    };
    let stats = |rig: &mut Rig| decode_stats_reply(&rig.serve(&DisplayRequest::QueryStats));
    assert_eq!(stats(&mut rig).err(), Some(Errno::PermissionDenied));
    rig.seat.holds = Err(Errno::NotFound);
    assert_eq!(stats(&mut rig).err(), Some(Errno::NotFound));
    rig.seat.holds = Ok(true);
    let read = stats(&mut rig).expect("the device read");
    assert!(rig.seat.asked.is_empty(), "no lease was asked about");
    assert_eq!(read.device.accel, Some(CAPS));
    assert_eq!(read.device.cursor, Some(CURSOR));
    assert_eq!(read.device.mem_total_bytes, 1 << 20);
    assert_eq!(read.mode, Some(MODE));
    assert_eq!((read.busy_ns, read.idle_ns, read.seat_id), (0, 0, 0));
}

#[test]
fn device_busy_time_counts_only_the_drivers_own_present() {
    let mut rig = Rig::new();
    rig.seat.holds = Ok(true);
    let frames = rig.frames();
    assert_eq!(
        rig.present(frames[0], &[full()]).map(|r| r.presented),
        Ok(Presented::Shown)
    );
    rig.clock.advance(50_000);
    let read = decode_stats_reply(&rig.serve(&DisplayRequest::QueryStats)).expect("read");
    assert_eq!(read.busy_ns, CLOCK_STEP_NS, "one bracketed present");
    assert_eq!(read.seat_id, SEAT);
    assert!(
        read.idle_ns >= 50_000,
        "the window partitions into busy and idle"
    );
}

#[test]
fn a_malformed_request_is_refused_before_the_seat_is_asked() {
    let mut rig = Rig::new();
    let mut reply = [0u8; DISPLAY_REPLY_MAX];
    let len = rig.server.serve(
        &mut rig.display,
        &mut rig.seat,
        &mut rig.delivery,
        TICKET,
        &[0u8; 8],
        &mut reply,
    );
    assert_eq!(
        decode_status_reply(&reply[..len]),
        Err(Errno::BufferTooSmall)
    );
    assert!(rig.seat.asked.is_empty());
}

#[test]
fn configure_allocates_the_drivers_frames_and_grants_each_to_the_caller() {
    let mut rig = Rig::new();
    rig.display.frame_count = 3;
    let configured = rig.configure().expect("configured");
    assert_eq!(configured.grantor, DRIVER);
    assert_eq!(configured.stride_bytes, MODE.stride_bytes);
    let frames: Vec<_> = configured.frames().collect();
    assert_eq!(frames.len(), 3);
    for (frame, &(region, ticket)) in frames.iter().zip(&rig.delivery.grants) {
        assert_eq!(ticket, TICKET, "granted to the caller in service");
        assert_eq!(frame.grant, region + GRANT_TAG);
    }
    assert!(rig
        .display
        .live
        .iter()
        .all(|b| (b.width_px, b.height_px) == (4, 3)));
    assert!(rig.server.is_configured());
}

#[test]
fn configure_refuses_another_mode_no_output_and_a_port_the_caller_does_not_own() {
    let mut rig = Rig::new();
    assert_eq!(rig.configure_for(NARROW).err(), Some(Errno::Stale));
    rig.display.output = None;
    assert_eq!(rig.configure().err(), Some(Errno::Stale));
    rig.display.output = Some(MODE);
    rig.seat.owns_port = false;
    assert_eq!(rig.configure().err(), Some(Errno::PermissionDenied));
    assert!(rig.display.live.is_empty(), "nothing was allocated");
    assert!(!rig.server.is_configured());
}

#[test]
fn a_configure_that_cannot_finish_frees_what_it_allocated() {
    let mut rig = Rig::new();
    rig.delivery.refuse_grant = Some(Errno::OutOfMemory);
    assert_eq!(rig.configure().err(), Some(Errno::OutOfMemory));
    assert!(rig.display.live.is_empty());
    rig.delivery.refuse_grant = None;
    rig.display.room = 1;
    assert_eq!(rig.configure().err(), Some(Errno::NoSpace));
    assert!(rig.display.live.is_empty());
    rig.display.frame_count = DISPLAY_MAX_FRAMES + 1;
    rig.display.room = usize::MAX;
    assert_eq!(rig.configure().err(), Some(Errno::DeviceFault));
    assert!(!rig.server.is_configured());
}

#[test]
fn a_present_shows_only_a_frame_of_the_live_configuration() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    let extra = rig.alloc(2, 2).expect("an extra buffer").grant.buffer;
    assert_eq!(rig.present(extra, &[full()]).err(), Some(Errno::NotFound));
    let stranger = BufferId::new(99).expect("an id");
    assert_eq!(
        rig.present(stranger, &[full()]).err(),
        Some(Errno::NotFound)
    );
    assert_eq!(
        rig.present(frames[1], &[rect(3, 0, 2, 1)]).err(),
        Some(Errno::LengthOutOfRange),
        "damage outside the mode"
    );
    let reply = rig.present(frames[1], &[rect(1, 1, 2, 1)]).expect("shown");
    assert_eq!(
        reply,
        PresentReply {
            presented: Presented::Shown,
            seq: 1
        }
    );
    assert_eq!(rig.display.shown, vec![(frames[1], vec![rect(1, 1, 2, 1)])]);
    assert_eq!(rig.present(frames[0], &[full()]).map(|r| r.seq), Ok(2));
}

#[test]
fn frames_go_stale_while_the_output_runs_another_mode_and_serve_again_after() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    rig.display.output = Some(NARROW);
    assert_eq!(
        rig.present(frames[0], &[rect(0, 0, 1, 1)]).err(),
        Some(Errno::Stale)
    );
    rig.display.output = Some(MODE);
    assert!(
        rig.present(frames[0], &[full()]).is_ok(),
        "the configuration stood"
    );
}

#[test]
fn frames_are_bound_to_the_lease_that_configured_them() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    rig.seat.answer = Ok(2);
    assert_eq!(
        rig.present(frames[0], &[full()]).err(),
        Some(Errno::NotFound)
    );
    rig.seat.answer = Err(Errno::SeatRevoked);
    assert_eq!(
        rig.present(frames[0], &[full()]).err(),
        Some(Errno::SeatRevoked)
    );
    rig.seat.answer = Err(Errno::SeatNotOwner);
    assert_eq!(rig.alloc(2, 2).err(), Some(Errno::SeatNotOwner));
    rig.seat.answer = Ok(1);
    assert!(
        rig.present(frames[0], &[full()]).is_ok(),
        "refusing a stranger left the owner presenting"
    );
}

#[test]
fn a_queued_present_holds_the_next_until_its_latch_is_posted() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let frames = rig.frames();
    let first = rig.present(frames[0], &[full()]).expect("queued");
    assert_eq!(first.presented, Presented::Queued);
    assert_eq!(
        rig.present(frames[1], &[full()]).err(),
        Some(Errno::WouldBlock),
        "the frame it replaces is still being read"
    );
    assert!(rig.server.deadline_ns().is_some());
    assert_eq!(
        rig.server.serviced(Serviced::default(), &mut rig.delivery),
        Latch::Idle
    );
    assert_eq!(rig.latch(), Latch::Posted);
    assert_eq!(
        rig.delivery.posts,
        vec![(PORT, DisplayEvent::Presented { seq: first.seq })]
    );
    assert_eq!(rig.server.deadline_ns(), None);
    assert!(rig.present(frames[1], &[full()]).is_ok());
}

#[test]
fn a_present_that_never_latches_is_released_at_its_deadline() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let frames = rig.frames();
    let queued = rig.present(frames[0], &[full()]).expect("queued");
    assert_eq!(
        rig.server.expire(&mut rig.display, &mut rig.delivery),
        Latch::Idle,
        "not yet due"
    );
    assert_eq!(rig.display.abandoned, 0);
    rig.clock.advance(PRESENT_LATCH_DEADLINE_NS);
    assert_eq!(
        rig.server.expire(&mut rig.display, &mut rig.delivery),
        Latch::Overdue
    );
    assert_eq!(
        rig.display.abandoned, 1,
        "the display stops waiting on it too, so the next present is not refused"
    );
    assert_eq!(
        rig.delivery.posts,
        vec![(PORT, DisplayEvent::Presented { seq: queued.seq })]
    );
    assert_eq!(rig.latch(), Latch::Idle, "nothing is queued any more");
}

#[test]
fn a_latch_its_port_refuses_still_releases_the_presenter() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let frames = rig.frames();
    assert!(rig.present(frames[0], &[full()]).is_ok());
    rig.delivery.refuse_post = Some(Errno::NoSpace);
    assert_eq!(rig.latch(), Latch::Undelivered(Errno::NoSpace));
    assert!(rig.present(frames[1], &[full()]).is_ok());
}

#[test]
fn buffers_beyond_the_frames_are_the_configurations_to_free() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.alloc(2, 2).err(),
        Some(Errno::NotFound),
        "nothing configured"
    );
    let frames = rig.frames();
    let granted = rig.alloc(5, 2).expect("allocated");
    assert_eq!((granted.stride_bytes, granted.grantor), (20, DRIVER));
    assert_eq!(
        rig.free(frames[0]).err(),
        Some(Errno::OutOfRange),
        "a frame stays"
    );
    assert_eq!(rig.free(granted.grant.buffer), Ok(()));
    assert_eq!(rig.free(granted.grant.buffer).err(), Some(Errno::NotFound));
    assert!(rig.display.freed.contains(&granted.grant.buffer));
}

#[test]
fn a_configuration_holds_no_more_buffers_than_its_table() {
    let mut rig = Rig::new();
    let frames = rig.frames().len();
    for _ in frames..DISPLAY_MAX_BUFFERS {
        assert!(rig.alloc(1, 1).is_ok());
    }
    let live = rig.display.live.len();
    assert_eq!(rig.alloc(1, 1).err(), Some(Errno::NoSpace));
    assert_eq!(
        rig.display.live.len(),
        live,
        "refused before the driver allocated"
    );
}

#[test]
fn layers_reach_only_a_compositor_and_only_buffers_it_can_show() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    assert_eq!(
        rig.present_layers(&[layer(frames[0], 4, 3)]).err(),
        Some(Errno::NotSupported)
    );
    rig.display.accel = Some(CAPS);
    let window = rig.alloc(16, 16).expect("a layer").grant.buffer;
    let stack = [layer(frames[0], 4, 3), layer(window, 16, 16)];
    let reply = rig.present_layers(&stack).expect("shown");
    assert_eq!(reply.presented, Presented::Shown);
    assert_eq!(rig.display.layers, vec![stack.to_vec()]);
    for refused in [
        (layer(window, 17, 16), Errno::LengthOutOfRange),
        (layer(BufferId::new(99).expect("id"), 1, 1), Errno::NotFound),
        (
            AccelLayer {
                opacity: 9,
                ..layer(window, 1, 1)
            },
            Errno::NotImplemented,
        ),
    ] {
        assert_eq!(rig.present_layers(&[refused.0]).err(), Some(refused.1));
    }
    let deep = [layer(window, 1, 1); 4];
    assert_eq!(
        rig.present_layers(&deep).err(),
        Some(Errno::LengthOutOfRange)
    );
    assert_eq!(
        rig.display.layers.len(),
        1,
        "no refused stack reached the device"
    );
}

#[test]
fn the_pointer_is_drawn_by_a_cursor_plane_from_a_buffer_it_fits() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    let set = |rig: &mut Rig, buffer, hot_x, hot_y| {
        rig.status(&DisplayRequest::SetCursor {
            seat_id: SEAT,
            buffer,
            hot_x,
            hot_y,
        })
    };
    let image = rig.alloc(8, 8).expect("an image").grant.buffer;
    assert_eq!(set(&mut rig, image, 1, 1), Err(Errno::NotSupported));
    rig.display.cursor = Some(CURSOR);
    assert_eq!(
        rig.status(&DisplayRequest::MoveCursor {
            seat_id: SEAT,
            x: 1,
            y: 1
        }),
        Err(Errno::NotFound),
        "no pointer is shown"
    );
    assert_eq!(
        set(&mut rig, frames[0], 0, 0),
        Err(Errno::NotFound),
        "a frame"
    );
    assert_eq!(
        set(&mut rig, image, 8, 0),
        Err(Errno::LengthOutOfRange),
        "hot spot"
    );
    let big = rig.alloc(9, 1).expect("a big image").grant.buffer;
    assert_eq!(set(&mut rig, big, 0, 0), Err(Errno::LengthOutOfRange));
    assert_eq!(set(&mut rig, image, 3, 4), Ok(()));
    assert_eq!(
        rig.status(&DisplayRequest::MoveCursor {
            seat_id: SEAT,
            x: -2,
            y: 7
        }),
        Ok(())
    );
    assert_eq!(
        rig.free(image),
        Ok(()),
        "freeing the pointer's buffer hides it"
    );
    assert_eq!(
        rig.display.cursor_ops,
        vec![
            CursorOp::Set(image, 3, 4),
            CursorOp::Move(-2, 7),
            CursorOp::Hide
        ]
    );
}

#[test]
fn planes_report_what_the_device_draws_in_hardware() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.planes(),
        Ok(Planes {
            accel: None,
            cursor: None
        })
    );
    rig.display.accel = Some(CAPS);
    rig.display.cursor = Some(CURSOR);
    assert_eq!(
        rig.planes(),
        Ok(Planes {
            accel: Some(CAPS),
            cursor: Some(CURSOR)
        })
    );
    rig.seat.answer = Err(Errno::SeatNotOwner);
    assert_eq!(rig.planes().err(), Some(Errno::SeatNotOwner));
}

#[test]
fn the_configured_owner_switches_its_display_off_and_on() {
    let mut rig = Rig::new();
    assert_eq!(rig.set_power(DisplayPower::Off), Err(Errno::NotFound));
    rig.frames();
    assert_eq!(rig.set_power(DisplayPower::Off), Ok(()));
    assert_eq!(rig.set_power(DisplayPower::On), Ok(()));
    rig.seat.answer = Ok(5);
    assert_eq!(rig.set_power(DisplayPower::Off), Err(Errno::NotFound));
    rig.display.power_refusal = Some(DriverError::Unsupported);
    rig.seat.answer = Ok(1);
    assert_eq!(rig.set_power(DisplayPower::Off), Err(Errno::NotImplemented));
}

#[test]
fn the_display_is_lit_before_the_first_presenters_first_frame() {
    let mut rig = Rig::new();
    rig.frames();
    assert_eq!(rig.display.powered, vec![DisplayPower::On]);
}

#[test]
fn an_ended_lease_releases_its_configuration_and_lights_the_display() {
    let mut rig = Rig::new();
    let frames = rig.frames();
    let extra = rig.alloc(2, 2).expect("allocated").grant.buffer;
    rig.display.cursor = Some(CURSOR);
    assert_eq!(
        rig.status(&DisplayRequest::SetCursor {
            seat_id: SEAT,
            buffer: extra,
            hot_x: 0,
            hot_y: 0
        }),
        Ok(())
    );
    assert_eq!(rig.set_power(DisplayPower::Off), Ok(()));
    rig.lease_moved(DisplayLease::ended(1, ReleaseSurface::Text));
    assert!(!rig.server.is_configured());
    assert_eq!(rig.display.released, vec![ReleaseSurface::Text]);
    assert!(rig.display.live.is_empty(), "every buffer freed");
    for buffer in frames.iter().chain([&extra]) {
        assert!(rig.display.freed.contains(buffer));
    }
    assert_eq!(rig.display.cursor_ops.last(), Some(&CursorOp::Hide));
    assert_eq!(rig.display.powered.last(), Some(&DisplayPower::On));
}

#[test]
fn the_same_presenter_reconfiguring_keeps_its_screen_and_a_new_one_takes_it_over() {
    let mut rig = Rig::new();
    let first = rig.frames();
    let second = rig.frames();
    assert!(rig.display.released.is_empty(), "the screen stayed its own");
    assert!(first.iter().all(|frame| rig.display.freed.contains(frame)));
    assert!(second
        .iter()
        .all(|frame| !rig.display.freed.contains(frame)));
    rig.seat.answer = Ok(2);
    rig.frames();
    assert_eq!(rig.display.released, vec![ReleaseSurface::Handover]);
}

#[test]
fn a_display_that_will_not_light_again_is_refused_to_the_next_presenter() {
    let mut rig = Rig::new();
    rig.frames();
    assert_eq!(rig.set_power(DisplayPower::Off), Ok(()));
    rig.display.power_refusal = Some(DriverError::DeviceFault);
    assert_eq!(rig.configure().err(), Some(Errno::DeviceFault));
    assert!(!rig.server.is_configured());
}

#[test]
fn error_conversions_preserve_the_seat_busy_and_fault_vocabulary() {
    assert_eq!(
        driver_error_from_errno(Errno::SeatRevoked),
        DriverError::SeatRevoked
    );
    assert_eq!(
        driver_error_from_errno(Errno::SeatNotOwner),
        DriverError::PermissionDenied
    );
    assert_eq!(
        driver_error_from_errno(Errno::WouldBlock),
        DriverError::Busy
    );
    assert_eq!(driver_error_from_errno(Errno::Busy), DriverError::Busy);
    assert_eq!(driver_error_from_errno(Errno::Stale), DriverError::Stale);
    assert_eq!(
        driver_error_from_errno(Errno::NotSupported),
        DriverError::Unsupported
    );
    assert_eq!(
        driver_error_from_errno(Errno::OutOfMemory),
        DriverError::OutOfMemory
    );
    assert_eq!(
        driver_error_from_errno(Errno::TimedOut),
        DriverError::DeviceFault
    );
}

// --- client + server end to end --------------------------------------

/// A loopback transport: each call runs one serve pass of a real
/// [`DisplayServer`] over the shared rig.
struct Loopback {
    rig: Rc<RefCell<Rig>>,
}

impl DisplayTransport for Loopback {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut rig = self.rig.borrow_mut();
        let mut buf = [0u8; DISPLAY_REPLY_MAX];
        let Rig {
            server,
            display,
            seat,
            delivery,
            ..
        } = &mut *rig;
        let len = server.serve(display, seat, delivery, TICKET, request, &mut buf);
        reply[..len].copy_from_slice(&buf[..len]);
        Ok(len)
    }
}

/// Maps a grant to a buffer of the presenter's own, recording what it was
/// asked for.
#[derive(Default)]
struct MockMapper {
    mapped: Rc<RefCell<Vec<(ProcId, u64, usize)>>>,
}

impl BufferMapper for MockMapper {
    type Mapping = Vec<u8>;

    fn map(&mut self, grantor: ProcId, grant: u64, least: usize) -> Result<Vec<u8>, Errno> {
        if grant < GRANT_TAG {
            return Err(Errno::NotFound);
        }
        self.mapped.borrow_mut().push((grantor, grant, least));
        Ok(vec![0u8; least])
    }
}

type Remote = RemoteDisplay<MockMapper, Loopback>;

fn remote(rig: Rig) -> (Rc<RefCell<Rig>>, Remote) {
    let rig = Rc::new(RefCell::new(rig));
    let client = DisplayClient::new(
        Loopback {
            rig: Rc::clone(&rig),
        },
        SEAT,
    );
    let remote =
        RemoteDisplay::configure(client, MockMapper::default(), MODE, PORT).expect("configured");
    (rig, remote)
}

/// A frame whose every byte names its own offset.
fn numbered() -> Vec<u8> {
    (0..FRAME_LEN)
        .map(|at| u8::try_from(at).expect("small"))
        .collect()
}

#[test]
fn a_remote_display_copies_the_frame_into_the_drivers_frame_and_names_it() {
    let (rig, mut remote) = remote(Rig::new());
    let frame = numbered();
    remote.present(&frame, &[full()]).expect("shown");
    assert_eq!(remote.frame(0), &frame[..]);
    let shown = rig.borrow().display.shown.clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].1, vec![full()]);
    assert_eq!(Presenter::mode(&remote), MODE);
}

#[test]
fn a_remote_display_refreshes_what_each_frame_missed_across_the_ring() {
    let (_rig, mut remote) = remote(Rig::new());
    let mut frame = vec![0u8; FRAME_LEN];
    remote.present(&frame, &[full()]).expect("frame 0");
    remote.present(&frame, &[full()]).expect("frame 1");
    frame[16..20].fill(0xAB);
    remote
        .present(&frame, &[rect(0, 1, 1, 1)])
        .expect("frame 0 again");
    assert_eq!(remote.frame(0)[16..20], [0xAB; 4]);
    frame[44..48].fill(0xCD);
    remote
        .present(&frame, &[rect(3, 2, 1, 1)])
        .expect("frame 1 again");
    assert_eq!(
        remote.frame(1),
        &frame[..],
        "the frame-0 change was owed to it"
    );
}

/// A frame drawn in place is handed the back frame and what it owes: the
/// damage, and what that frame missed while the other was shown, and is then
/// shown with the damage it named.
#[test]
fn a_frame_drawn_in_place_is_handed_what_the_back_frame_owes() {
    let (rig, mut remote) = remote(Rig::new());
    let mut owed = alloc::vec::Vec::new();
    let mut draw = |memory: &mut [u8], stride: u32, refresh: &Region| {
        owed.push((refresh.rects().to_vec(), stride));
        for rect in refresh.rects() {
            for row in rect.top()..rect.bottom() {
                let at = usize::try_from(row).expect("on screen") * stride as usize
                    + usize::try_from(rect.left()).expect("on screen") * 4;
                memory[at..at + rect.width as usize * 4].fill(0x5A);
            }
        }
        Ok(())
    };
    let whole = Region::from(Rect::new(0, 0, 4, 3));
    remote.present_drawn(&whole, &mut draw).expect("frame 0");
    let corner = Region::from(Rect::new(1, 1, 2, 1));
    remote.present_drawn(&corner, &mut draw).expect("frame 1");
    remote
        .present_drawn(&corner, &mut draw)
        .expect("frame 0 again");
    assert_eq!(owed[0], (whole.rects().to_vec(), 16));
    assert_eq!(
        owed[1],
        (whole.rects().to_vec(), 16),
        "frame 1 missed frame 0's"
    );
    assert_eq!(
        owed[2],
        (corner.rects().to_vec(), 16),
        "frame 0 missed frame 1's"
    );
    assert!(remote.frame(1).iter().all(|byte| *byte == 0x5A));
    let shown = rig.borrow().display.shown.clone();
    assert_eq!(shown.len(), 3);
    assert_eq!(shown[2].1, vec![rect(1, 1, 2, 1)]);
}

/// A draw that refuses shows nothing and hands its refusal back, and the
/// frame it was handed owes the whole region still, where no other frame owes
/// what was never shown.
#[test]
fn a_refused_draw_shows_nothing_and_its_frame_stays_owed() {
    let (rig, mut remote) = remote(Rig::new());
    let mut owed = Vec::new();
    let refusing = core::cell::Cell::new(false);
    let mut draw = |_: &mut [u8], _: u32, refresh: &Region| {
        owed.push(refresh.rects().to_vec());
        if refusing.get() {
            Err(DriverError::OutOfRange)
        } else {
            Ok(())
        }
    };
    let whole = Region::from(Rect::new(0, 0, 4, 3));
    let (a, b, c) = (
        Region::from(Rect::new(0, 0, 1, 1)),
        Region::from(Rect::new(2, 1, 1, 1)),
        Region::from(Rect::new(3, 2, 1, 1)),
    );
    for damage in [&whole, &whole, &a, &a] {
        remote.present_drawn(damage, &mut draw).expect("shown");
    }
    refusing.set(true);
    assert_eq!(
        remote.present_drawn(&b, &mut draw).err(),
        Some(DriverError::OutOfRange)
    );
    refusing.set(false);
    assert_eq!(
        rig.borrow().display.shown.len(),
        4,
        "the refused frame never shown"
    );
    remote.present_drawn(&c, &mut draw).expect("frame 0");
    remote.present_drawn(&c, &mut draw).expect("frame 1");
    let mut kept = a.clone();
    kept.add_region(&b);
    kept.add_region(&c);
    assert_eq!(owed[5], kept.rects(), "frame 0 still owes what it refused");
    assert_eq!(owed[6], c.rects(), "frame 1 owes nothing never shown");
    let shown = rig.borrow().display.shown.clone();
    assert_eq!(shown.len(), 6);
    assert_eq!(shown[4].1, vec![rect(3, 2, 1, 1)]);
}

/// A present still waiting for its latch refuses one drawn in place without
/// handing out a frame the display may yet be reading, and nothing changed
/// shows nothing.
#[test]
fn a_frame_drawn_in_place_waits_for_the_latch_and_nothing_shows_nothing() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let (rig, mut remote) = remote(rig);
    let mut drawn = 0;
    let mut draw = |_: &mut [u8], _: u32, _: &Region| {
        drawn += 1;
        Ok(())
    };
    remote
        .present_drawn(&Region::new(), &mut draw)
        .expect("nothing to show");
    assert_eq!(rig.borrow().display.shown.len(), 0);
    let whole = Region::from(Rect::new(0, 0, 4, 3));
    remote.present_drawn(&whole, &mut draw).expect("queued");
    assert_eq!(
        remote.present_drawn(&whole, &mut draw).err(),
        Some(DriverError::Busy)
    );
    assert_eq!(drawn, 1, "the frame being read is never handed out");
}

#[test]
fn a_scattered_frame_copies_its_rectangles_not_the_box_between_them() {
    let mut rig = Rig::new();
    rig.display.frame_count = 1;
    let (_rig, mut remote) = remote(rig);
    remote
        .present(&[0u8; FRAME_LEN], &[full()])
        .expect("the frame, whole");
    let marked = vec![0x99u8; FRAME_LEN];
    remote
        .present(&marked, &[rect(0, 0, 1, 1), rect(3, 2, 1, 1)])
        .expect("two corners");
    let target = remote.frame(0);
    assert_eq!(target[0..4], [0x99; 4]);
    assert_eq!(target[44..48], [0x99; 4]);
    assert_eq!(target[20..24], [0; 4], "the middle was never copied");
}

#[test]
fn damage_from_a_refused_present_is_still_owed_to_the_other_frames() {
    let (rig, mut remote) = remote(Rig::new());
    remote
        .present(&[0u8; FRAME_LEN], &[full()])
        .expect("frame 0");
    remote
        .present(&[0u8; FRAME_LEN], &[full()])
        .expect("frame 1");
    rig.borrow_mut().display.output = Some(NARROW);
    let changed = vec![0x77u8; FRAME_LEN];
    assert_eq!(
        remote.present(&changed, &[rect(0, 0, 1, 1)]),
        Err(DriverError::Stale)
    );
    rig.borrow_mut().display.output = Some(MODE);
    remote
        .present(&changed, &[rect(3, 2, 1, 1)])
        .expect("frame 0");
    remote
        .present(&changed, &[rect(3, 2, 1, 1)])
        .expect("frame 1");
    assert_eq!(
        remote.frame(1)[0..4],
        [0x77; 4],
        "the refused rectangle reached it"
    );
}

#[test]
fn a_remote_display_waits_for_a_queued_frames_latch() {
    let mut rig = Rig::new();
    rig.display.queues = true;
    let (rig, mut remote) = remote(rig);
    remote
        .present(&[0u8; FRAME_LEN], &[full()])
        .expect("queued");
    assert!(!remote.ready(), "the next waits for its latch");
    assert_eq!(
        remote.present(&[1u8; FRAME_LEN], &[full()]),
        Err(DriverError::Busy)
    );
    assert_eq!(rig.borrow().display.shown.len(), 1, "nothing was sent");
    assert!(
        !remote.latched(DisplayEvent::Presented { seq: 9 }),
        "another present's"
    );
    let latch = rig.borrow_mut().latch();
    assert_eq!(latch, Latch::Posted);
    let event = rig.borrow().delivery.posts[0].1;
    assert!(remote.latched(event));
    assert!(remote.present(&[1u8; FRAME_LEN], &[full()]).is_ok());
}

#[test]
fn a_remote_display_reaches_layers_and_the_pointer_only_where_the_device_has_them() {
    let (_rig, mut plain) = remote(Rig::new());
    assert!(plain.layers().is_none());
    assert!(plain.cursor().is_none());

    let mut rig = Rig::new();
    rig.display.accel = Some(CAPS);
    rig.display.cursor = Some(CURSOR);
    let (rig, mut remote) = remote(rig);
    let layers = remote.layers().expect("a compositor");
    assert_eq!(layers.caps(), CAPS);
    let window = layers.alloc(2, 2).expect("a layer");
    let (pixels, stride) = layers.pixels(window).expect("mapped");
    assert_eq!((pixels.len(), stride), (16, 8));
    pixels.fill(0x42);
    layers
        .present_layers(None, &[layer(window, 2, 2)])
        .expect("shown");
    assert_eq!(rig.borrow().display.layers, vec![vec![layer(window, 2, 2)]]);
    layers.free(window).expect("freed");
    assert_eq!(layers.free(window), Err(DriverError::NotFound));

    let cursor = remote.cursor().expect("a cursor plane");
    let image = [0xFFu8; 4 * 4 * 4];
    cursor.show(&image, 4, 4, 16, 1, 1).expect("shown");
    cursor.show(&image, 4, 4, 16, 2, 2).expect("shown again");
    assert_eq!(
        cursor.show(&image, 9, 1, 36, 0, 0),
        Err(DriverError::LengthOutOfRange),
        "larger than the plane"
    );
    cursor.move_to(5, 6).expect("moved");
    cursor.hide().expect("hidden");
    let ops = rig.borrow().display.cursor_ops.clone();
    let (CursorOp::Set(first, 1, 1), CursorOp::Set(second, 2, 2)) = (ops[0], ops[1]) else {
        panic!("two images set: {ops:?}");
    };
    assert_eq!(
        first, second,
        "the device took the first image, so its buffer is rewritten"
    );
    assert_eq!(&ops[2..], [CursorOp::Move(5, 6), CursorOp::Hide]);
}

#[test]
fn a_pointer_of_any_size_is_drawn_into_the_one_buffer_the_plane_s_largest_fits() {
    let mut rig = Rig::new();
    rig.display.cursor = Some(CURSOR);
    let (rig, mut remote) = remote(rig);
    let cursor = remote.cursor().expect("a cursor plane");
    cursor
        .show(&[0xFF; 4 * 4 * 4], 4, 4, 16, 0, 0)
        .expect("shown");
    cursor
        .show(&[0xFF; 2 * 2 * 4], 2, 2, 8, 1, 1)
        .expect("smaller");
    let display = &rig.borrow().display;
    let (CursorOp::Set(first, ..), CursorOp::Set(second, 1, 1)) =
        (display.cursor_ops[0], display.cursor_ops[1])
    else {
        panic!("two images set: {:?}", display.cursor_ops);
    };
    assert_eq!(first, second, "one buffer for both");
    assert!(display.freed.is_empty());
    let held = display
        .live
        .iter()
        .find(|buffer| buffer.id == first)
        .expect("the pointer's buffer");
    assert_eq!(
        (held.width_px, held.height_px),
        (CURSOR.max_width_px, CURSOR.max_height_px)
    );
}

#[test]
fn a_layered_present_lays_the_output_frame_beneath_its_layers() {
    let mut rig = Rig::new();
    rig.display.accel = Some(CAPS);
    let (rig, mut remote) = remote(rig);
    let frames: Vec<BufferId> = rig
        .borrow()
        .display
        .live
        .iter()
        .map(|held| held.id)
        .collect();
    let window = remote
        .layers()
        .expect("a compositor")
        .alloc(2, 2)
        .expect("a layer");
    let mut frame = vec![0u8; FRAME_LEN];
    frame[0..4].fill(0x11);
    let base = Base {
        frame: &frame,
        damage: &[rect(0, 0, 1, 1)],
    };
    remote
        .layers()
        .expect("a compositor")
        .present_layers(Some(base), &[layer(window, 2, 2)])
        .expect("shown");
    let stack = rig.borrow().display.layers[0].clone();
    assert_eq!(stack.len(), 2);
    assert_eq!(
        (
            stack[0].buffer,
            stack[0].src,
            stack[0].dst_x,
            stack[0].dst_y,
            stack[0].opacity
        ),
        (frames[0], full(), 0, 0, u8::MAX),
        "the frame is the whole output, opaque"
    );
    assert_eq!(stack[1], layer(window, 2, 2));
    assert_eq!(
        remote.frame(0)[0..4],
        [0x11; 4],
        "the base damage reached the frame"
    );
    frame[44..48].fill(0x22);
    remote
        .present(&frame, &[rect(3, 2, 1, 1)])
        .expect("the next frame");
    assert_eq!(
        remote.frame(1)[0..4],
        [0x11; 4],
        "the layered frame's damage was owed to it"
    );
    assert_eq!(rig.borrow().display.shown[0].0, frames[1]);
}

#[test]
fn a_layered_present_waits_for_a_queued_frames_latch_as_a_frame_does() {
    let mut rig = Rig::new();
    rig.display.accel = Some(CAPS);
    rig.display.queues = true;
    let (rig, mut remote) = remote(rig);
    assert!(remote.ready());
    let window = remote
        .layers()
        .expect("a compositor")
        .alloc(2, 2)
        .expect("a layer");
    remote
        .layers()
        .expect("a compositor")
        .present_layers(None, &[layer(window, 2, 2)])
        .expect("queued");
    assert!(!remote.ready());
    assert_eq!(
        remote
            .layers()
            .expect("a compositor")
            .present_layers(None, &[layer(window, 2, 2)]),
        Err(DriverError::Busy)
    );
    assert_eq!(rig.borrow_mut().latch(), Latch::Posted);
    let event = rig.borrow().delivery.posts[0].1;
    assert!(remote.latched(event));
    assert!(remote.ready());
}

#[test]
fn a_layered_present_with_nothing_new_in_the_base_names_the_frame_on_screen() {
    let mut rig = Rig::new();
    rig.display.accel = Some(CAPS);
    let (rig, mut remote) = remote(rig);
    let frames: Vec<BufferId> = rig
        .borrow()
        .display
        .live
        .iter()
        .map(|held| held.id)
        .collect();
    let window = remote
        .layers()
        .expect("a compositor")
        .alloc(2, 2)
        .expect("a layer");
    let frame = vec![0x33u8; FRAME_LEN];
    let whole = [full()];
    let engine = remote.layers().expect("a compositor");
    for damage in [&whole[..], &[]] {
        let base = Base {
            frame: &frame,
            damage,
        };
        engine
            .present_layers(Some(base), &[layer(window, 2, 2)])
            .expect("shown");
    }
    let stacks = rig.borrow().display.layers.clone();
    assert_eq!(stacks[0][0].buffer, frames[0]);
    assert_eq!(
        stacks[1][0].buffer, frames[0],
        "an unchanged frame is named again, never copied into another"
    );
}

#[test]
fn a_remote_display_holds_only_the_layers_it_allocated_and_knows_its_pointer() {
    let mut rig = Rig::new();
    rig.display.accel = Some(CAPS);
    rig.display.cursor = Some(CURSOR);
    let (_rig, mut remote) = remote(rig);
    let engine = remote.layers().expect("a compositor");
    let window = engine.alloc(2, 2).expect("a layer");
    assert!(engine.holds(window));
    assert!(
        !engine.holds(BufferId::new(999).expect("an id")),
        "a buffer from another display"
    );
    let cursor = remote.cursor().expect("a cursor plane");
    assert!(!cursor.showing(), "nothing shown yet");
    cursor
        .show(&[0xFF; 4 * 4 * 4], 4, 4, 16, 0, 0)
        .expect("shown");
    assert!(cursor.showing());
    cursor.hide().expect("hidden");
    assert!(!cursor.showing());
}

#[test]
fn a_remote_display_validates_its_construction_and_inputs() {
    let rig = Rc::new(RefCell::new(Rig::new()));
    let client = |rig: &Rc<RefCell<Rig>>| {
        DisplayClient::new(
            Loopback {
                rig: Rc::clone(rig),
            },
            SEAT,
        )
    };
    let thin = DisplayMode {
        stride_bytes: 15,
        ..MODE
    };
    assert_eq!(
        RemoteDisplay::configure(client(&rig), MockMapper::default(), thin, PORT).err(),
        Some(Errno::LengthOutOfRange)
    );
    assert_eq!(
        RemoteDisplay::configure(client(&rig), MockMapper::default(), NARROW, PORT).err(),
        Some(Errno::Stale)
    );
    let mut remote =
        RemoteDisplay::configure(client(&rig), MockMapper::default(), MODE, PORT).expect("ok");
    assert_eq!(
        remote.present(&[0u8; FRAME_LEN - 1], &[full()]),
        Err(DriverError::BufferTooSmall)
    );
    assert_eq!(
        remote.present(&[0u8; FRAME_LEN], &[rect(4, 0, 1, 1)]),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_revoked_client_sees_the_typed_teardown_signal() {
    let (rig, mut remote) = remote(Rig::new());
    rig.borrow_mut().seat.answer = Err(Errno::SeatRevoked);
    assert_eq!(
        remote.present(&[0u8; FRAME_LEN], &[full()]),
        Err(DriverError::SeatRevoked)
    );
    assert_eq!(
        remote.set_power(DisplayPower::Off),
        Err(DriverError::SeatRevoked)
    );
}

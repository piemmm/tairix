//! The server half: the engine a display driver's `Run` binary hosts.
//!
//! [`DisplayServer::serve`] handles exactly one received request:
//! decode → live-lease check → validation → action → encoded reply. The
//! hosting binary owns the transport; the engine owns every decision in
//! between, so the semantics are host-testable against mock seams.
//!
//! # Security shape
//!
//! * **Identity is kernel-attested.** [`PeerFacts::live_generation`] asks the
//!   kernel whether the *in-flight caller of this endpoint* holds the seat's
//!   live lease, and every request that acts for a seat — `Query` included —
//!   is gated on it before any state is read or mutated. `QueryStats` acts for
//!   no seat and is gated on the caller's `CAP_SYSINFO_HW` instead.
//! * **Buffers belong to the configuration that allocated them.** Every
//!   reference is checked against the configuration's own table, under the
//!   lease generation that configured it: one owner's buffer can never be shown
//!   under another's lease, and a stale or foreign id is `NotFound`.
//! * **A buffer is granted only to the caller that asked.** The grant is made
//!   to the in-flight call (`shm_grant_peer`), and events go only to a port the
//!   configuring caller owns.
//! * **A configuration ends with its lease, and only then.** A refused
//!   request changes nothing: a refusal cannot tell the owner that lost its
//!   lease from a stranger, so letting one release the configuration would let
//!   any process take the desktop's screen away.
//! * **One principal's frames never reach the next.** Releasing or replacing
//!   a configuration tells the display where the screen went first, then
//!   frees its buffers, and switches a dark display back on.
//! * **One present is in flight.** A present the device queues holds the next
//!   until its latch, which the driver reports or the deadline forces, so a
//!   presenter can never write a frame the device is still reading.

use tairix_abi::display_ipc::{
    encode_buffer_reply, encode_configure_reply, encode_mode_reply, encode_planes_reply,
    encode_present_reply, encode_stats_reply, BufferGrant, ConfiguredFrames, DisplayEvent,
    DisplayRequest, DisplayStats, GrantedBuffer, Planes, PresentReply, DISPLAY_BUFFER_REPLY_LEN,
    DISPLAY_CONFIGURE_REPLY_LEN, DISPLAY_MAX_BUFFERS, DISPLAY_MAX_FRAMES, DISPLAY_MODE_REPLY_LEN,
    DISPLAY_PLANES_REPLY_LEN, DISPLAY_PRESENT_REPLY_LEN, DISPLAY_STATS_REPLY_LEN,
};
use tairix_abi::driver::display::{
    AccelLayer, BufferId, DamageList, DamageRect, Display, DisplayDeviceReport, DisplayFormat,
    DisplayMode, DisplayPower, Presented, Serviced,
};
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::seat::{DisplayLease, ReleaseSurface, SEAT_PRIMARY};
use tairix_abi::time::MonotonicClock;
use tairix_abi::{CapabilityId, DriverError, Errno, ProcId};
use tairix_inline::ArrayVec;

/// Upper bound, in bytes, of any reply [`DisplayServer::serve`] writes.
pub const DISPLAY_REPLY_MAX: usize = max(
    max(DISPLAY_STATS_REPLY_LEN, DISPLAY_CONFIGURE_REPLY_LEN),
    max(
        max(DISPLAY_MODE_REPLY_LEN, DISPLAY_BUFFER_REPLY_LEN),
        max(DISPLAY_PRESENT_REPLY_LEN, DISPLAY_PLANES_REPLY_LEN),
    ),
);

const fn max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

/// How long a queued present may wait for its latch before the engine
/// releases the presenter anyway: several frame periods at the slowest
/// refresh a sink runs, a containment bound against a latch that never comes.
pub const PRESENT_LATCH_DEADLINE_NS: u64 = 200_000_000;

/// The kernel's own facts about the in-flight caller, behind a seam — so the
/// engine is host-testable and never trusts a claim the caller made about
/// itself. Every question is about the caller of this endpoint identified by
/// `ticket`.
pub trait PeerFacts {
    /// The live lease generation of `seat_id` **if and only if** the
    /// in-flight caller identified by `ticket` holds it.
    ///
    /// # Errors
    ///
    /// * [`Errno::SeatNotOwner`] — the caller does not hold the live lease.
    /// * [`Errno::SeatRevoked`] — the caller's lease was forcibly revoked.
    /// * [`Errno::NotFound`] — no such seat.
    fn live_generation(&mut self, ticket: u64, seat_id: u64) -> Result<u64, Errno>;

    /// Whether the in-flight caller identified by `ticket` holds `cap`.
    ///
    /// # Errors
    ///
    /// Any error the kernel's attestation reports, which the engine treats
    /// as a refusal.
    fn holds_capability(&mut self, ticket: u64, cap: CapabilityId) -> Result<bool, Errno>;

    /// Whether the in-flight caller identified by `ticket` owns `port`.
    fn owns_port(&mut self, ticket: u64, port: u64) -> bool;
}

/// What the engine has the kernel do on its own account: grant a buffer to
/// the caller it serves, and post an event to a configuration's port.
pub trait Delivery {
    /// Grant shared region `region` to the in-flight caller `ticket`,
    /// answering the handle the caller maps it through.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn grant(&mut self, region: u64, ticket: u64) -> Result<u64, Errno>;

    /// This driver instance, which a presenter names when it maps a grant.
    fn grantor(&self) -> ProcId;

    /// Post `event` to `port`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal: a full port, or one gone with its owner.
    fn post(&mut self, port: u64, event: &DisplayEvent) -> Result<(), Errno>;
}

/// What the engine knows of the display's power.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Power {
    /// Not switched by this engine yet: a service restarted after its
    /// predecessor switched the display off inherits a dark screen, so the
    /// display is lit before the first presenter's first frame.
    Unverified,
    /// Switched on, or a display with no power control, which is never dark.
    On,
    /// Switched off by the configured presenter.
    Off,
}

/// A buffer a configuration holds.
#[derive(Copy, Clone, Debug)]
struct Held {
    id: BufferId,
    width_px: u32,
    height_px: u32,
    /// One of the output frames, freed only with the configuration.
    frame: bool,
}

/// A present the device queued, waiting for its latch.
#[derive(Copy, Clone, Debug)]
struct Queued {
    seq: u64,
    deadline_ns: u64,
}

/// One lease's buffers.
struct Configured {
    seat_id: u64,
    generation: u64,
    /// The mode the frames were allocated for.
    mode: DisplayMode,
    events_port: u64,
    buffers: ArrayVec<Held, DISPLAY_MAX_BUFFERS>,
    cursor: Option<BufferId>,
    next_seq: u64,
    queued: Option<Queued>,
}

impl Configured {
    fn held(&self, id: BufferId) -> Option<Held> {
        self.buffers.iter().copied().find(|held| held.id == id)
    }
}

/// What a latch that came too late, or never, left behind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Latch {
    /// No present is queued.
    Idle,
    /// The queued present latched, and its presenter was told.
    Posted,
    /// The queued present was released at its deadline, unlatched.
    Overdue,
    /// The presenter could not be told: its port refused the event.
    Undelivered(Errno),
}

/// The display-service engine: one instance serves one [`Display`].
pub struct DisplayServer<C: MonotonicClock> {
    clock: C,
    configured: Option<Configured>,
    /// The instant the device was first driven, which `busy_ns` and the idle
    /// time partition; [`None`] until then.
    window_start_ns: Option<u64>,
    /// Cumulative nanoseconds spent inside the driver's present calls.
    busy_ns: u64,
    /// A presenter's frame has reached the device at least once.
    presented: bool,
    power: Power,
}

impl<C: MonotonicClock> DisplayServer<C> {
    /// An engine with no configuration, timing the device against `clock`.
    pub const fn new(clock: C) -> Self {
        Self {
            clock,
            configured: None,
            window_start_ns: None,
            busy_ns: 0,
            presented: false,
            power: Power::Unverified,
        }
    }

    /// Serve one received request and encode its reply into `reply`,
    /// returning the reply length (`<= DISPLAY_REPLY_MAX`). Every outcome —
    /// a malformed request included — is a well-formed reply.
    pub fn serve(
        &mut self,
        display: &mut dyn Display,
        peer: &mut dyn PeerFacts,
        delivery: &mut dyn Delivery,
        ticket: u64,
        request: &[u8],
        reply: &mut [u8; DISPLAY_REPLY_MAX],
    ) -> usize {
        let request = match DisplayRequest::from_bytes(request) {
            Ok(request) => request,
            Err(err) => return status(reply, Err(err)),
        };
        match request {
            DisplayRequest::QueryStats => {
                let result = self.stats_for(display, peer, ticket);
                frame(reply, &encode_stats_reply(result))
            }
            DisplayRequest::Query { seat_id } => {
                let result = leased(peer, ticket, seat_id).map(|_| display.output());
                frame(reply, &encode_mode_reply(result))
            }
            DisplayRequest::Configure {
                seat_id,
                width_px,
                height_px,
                format,
                events_port,
            } => {
                let result = leased(peer, ticket, seat_id).and_then(|lease| {
                    if !peer.owns_port(ticket, events_port) {
                        return Err(Errno::PermissionDenied);
                    }
                    self.configure(
                        display,
                        delivery,
                        ticket,
                        lease,
                        (width_px, height_px, format),
                        events_port,
                    )
                });
                frame(reply, &encode_configure_reply(result))
            }
            DisplayRequest::QueryPlanes { seat_id } => {
                let result = leased(peer, ticket, seat_id).map(|_| Planes {
                    accel: display.as_accelerated().map(|accel| accel.accel_caps()),
                    cursor: display.hardware_cursor().map(|cursor| cursor.cursor_caps()),
                });
                frame(reply, &encode_planes_reply(result))
            }
            DisplayRequest::AllocBuffer {
                seat_id,
                width_px,
                height_px,
            } => {
                let result = leased(peer, ticket, seat_id).and_then(|lease| {
                    self.alloc(display, delivery, ticket, lease, width_px, height_px)
                });
                frame(reply, &encode_buffer_reply(result))
            }
            DisplayRequest::FreeBuffer { seat_id, buffer } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.free(display, lease, buffer));
                status(reply, result)
            }
            DisplayRequest::Present {
                seat_id,
                buffer,
                damage,
            } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.present(display, lease, buffer, damage));
                frame(reply, &encode_present_reply(result))
            }
            DisplayRequest::PresentLayers { seat_id, layers } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.present_layers(display, lease, layers.layers()));
                frame(reply, &encode_present_reply(result))
            }
            DisplayRequest::SetCursor {
                seat_id,
                buffer,
                hot_x,
                hot_y,
            } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.set_cursor(display, lease, buffer, hot_x, hot_y));
                status(reply, result)
            }
            DisplayRequest::MoveCursor { seat_id, x, y } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.move_cursor(display, lease, x, y));
                status(reply, result)
            }
            DisplayRequest::HideCursor { seat_id } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.hide_cursor(display, lease));
                status(reply, result)
            }
            DisplayRequest::SetPower { seat_id, power } => {
                let result = leased(peer, ticket, seat_id)
                    .and_then(|lease| self.set_power(display, delivery, lease, power));
                status(reply, result)
            }
        }
    }

    /// Adopt the boot seat's lease as the kernel announced it.
    ///
    /// A configuration of that seat made under any other lease is dead: its
    /// owner released the seat, lost it, or died. It is released here, and
    /// the display switched back on if it was switched off.
    pub fn lease_moved(&mut self, display: &mut dyn Display, lease: DisplayLease) {
        let dead = self.configured.as_ref().is_none_or(|c| {
            c.seat_id == SEAT_PRIMARY && lease.live_generation() != Some(c.generation)
        });
        if dead {
            // A lease already held anew ended with the edge unseen: the
            // newcomer is taking over.
            let surface = lease.released_to().unwrap_or(ReleaseSurface::Handover);
            // Nobody asked for this, so nobody is owed the answer; a display
            // that stays dark is retried on the next lease edge.
            let _ = self.release(display, Some(surface));
        }
    }

    /// Take what the display's event sources reported: a queued present that
    /// latched is posted to its presenter.
    pub fn serviced(&mut self, serviced: Serviced, delivery: &mut dyn Delivery) -> Latch {
        if !serviced.latched {
            return Latch::Idle;
        }
        self.post_latch(delivery).unwrap_or(Latch::Idle)
    }

    /// When the queued present's deadline falls, if one is queued: the
    /// hosting loop wakes then at the latest.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.configured
            .as_ref()
            .and_then(|c| c.queued)
            .map(|queued| queued.deadline_ns)
    }

    /// Release a queued present whose deadline has passed, as though it
    /// latched: its presenter is never left waiting on a latch that does not
    /// come, nor is `display` left waiting on it.
    pub fn expire(&mut self, display: &mut dyn Display, delivery: &mut dyn Delivery) -> Latch {
        let now = self.clock.now_ns();
        let due = self.deadline_ns().is_some_and(|deadline| now >= deadline);
        if !due {
            return Latch::Idle;
        }
        display.abandon_queued();
        match self.post_latch(delivery) {
            Some(Latch::Posted) => Latch::Overdue,
            other => other.unwrap_or(Latch::Idle),
        }
    }

    fn post_latch(&mut self, delivery: &mut dyn Delivery) -> Option<Latch> {
        let configured = self.configured.as_mut()?;
        let queued = configured.queued.take()?;
        let event = DisplayEvent::Presented { seq: queued.seq };
        Some(match delivery.post(configured.events_port, &event) {
            Ok(()) => Latch::Posted,
            Err(err) => Latch::Undelivered(err),
        })
    }

    /// Drop the configuration — the display told where the screen went first
    /// when it leaves its presenter, then every buffer freed, each once the
    /// device has stopped reading it — and light the display unless it is
    /// known to be lit.
    ///
    /// # Errors
    ///
    /// The driver's refusal to switch on a display this engine switched off;
    /// the configuration is dropped regardless.
    fn release(
        &mut self,
        display: &mut dyn Display,
        surface: Option<ReleaseSurface>,
    ) -> Result<(), Errno> {
        if let Some(configured) = self.configured.take() {
            if let Some(surface) = surface {
                display.released(surface);
            }
            if configured.cursor.is_some() {
                if let Some(cursor) = display.hardware_cursor() {
                    // A cursor plane that will not hide shows a buffer the
                    // next presenter's own pointer replaces.
                    let _ = cursor.hide_cursor();
                }
            }
            for held in &configured.buffers {
                // An id this driver allocated always frees.
                let _ = display.free_buffer(held.id);
            }
        }
        if self.power == Power::On {
            return Ok(());
        }
        match display.set_power(DisplayPower::On) {
            Ok(()) | Err(DriverError::Unsupported) => {
                self.power = Power::On;
                Ok(())
            }
            Err(_) if self.power == Power::Unverified => Ok(()),
            Err(err) => Err(err.as_errno()),
        }
    }

    /// The device's statistics, for a caller holding `CAP_SYSINFO_HW`.
    fn stats_for(
        &self,
        display: &mut dyn Display,
        peer: &mut dyn PeerFacts,
        ticket: u64,
    ) -> Result<DisplayStats, Errno> {
        // Nothing — not even the clock — is read before the authority
        // answers, so a caller without it moves no state.
        match peer.holds_capability(ticket, CapabilityId::SYSINFO_HW) {
            Ok(true) => Ok(self.device_stats(display)),
            Ok(false) => Err(Errno::PermissionDenied),
            Err(err) => Err(err),
        }
    }

    /// The device's own statistics: the occupancy this engine measured, what
    /// the device is, and the mode it scans out.
    fn device_stats(&self, display: &mut dyn Display) -> DisplayStats {
        let window_ns = self
            .window_start_ns
            .map_or(0, |start| self.clock.now_ns().saturating_sub(start));
        DisplayStats {
            seat_id: self.configured.as_ref().map_or(0, |c| c.seat_id),
            busy_ns: self.busy_ns,
            idle_ns: window_ns.saturating_sub(self.busy_ns),
            device: DisplayDeviceReport::of(display),
            mode: display.output(),
        }
    }

    /// Whether the engine holds a configuration.
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        self.configured.is_some()
    }

    /// Whether a presenter's frame has reached the device at least once —
    /// the witness the hosting service logs once. No decision reads it.
    #[must_use]
    pub const fn has_presented(&self) -> bool {
        self.presented
    }

    /// The configuration `lease` made, or `NotFound`.
    fn configured(&mut self, (seat_id, generation): (u64, u64)) -> Result<&mut Configured, Errno> {
        self.configured
            .as_mut()
            .filter(|c| c.seat_id == seat_id && c.generation == generation)
            .ok_or(Errno::NotFound)
    }

    /// Allocate the output frames for `lease` at the mode it expects,
    /// replacing any earlier configuration.
    fn configure(
        &mut self,
        display: &mut dyn Display,
        delivery: &mut dyn Delivery,
        ticket: u64,
        (seat_id, generation): (u64, u64),
        (width_px, height_px, format): (u32, u32, DisplayFormat),
        events_port: u64,
    ) -> Result<ConfiguredFrames, Errno> {
        let mode = display
            .output()
            .filter(|mode| {
                (mode.width_px, mode.height_px, mode.format) == (width_px, height_px, format)
            })
            .ok_or(Errno::Stale)?;
        // The same presenter reconfiguring keeps its screen; another one takes
        // it over.
        let handover = self
            .configured
            .as_ref()
            .is_some_and(|c| (c.seat_id, c.generation) != (seat_id, generation))
            .then_some(ReleaseSurface::Handover);
        // A display that cannot be lit again is refused here, loudly, rather
        // than handed to a presenter whose every frame would be invisible.
        self.release(display, handover)?;
        let count = display.frame_count();
        if count == 0 || count > DISPLAY_MAX_FRAMES {
            return Err(Errno::DeviceFault);
        }
        let mut configured = Configured {
            seat_id,
            generation,
            mode,
            events_port,
            buffers: ArrayVec::new(),
            cursor: None,
            next_seq: 1,
            queued: None,
        };
        let mut grants = ArrayVec::<BufferGrant, { DISPLAY_MAX_FRAMES as usize }>::new();
        let mut stride = None;
        let built = (0..count).try_for_each(|_| {
            let (grant, buffer_stride) = grant_buffer(
                display,
                delivery,
                ticket,
                &mut configured,
                mode.width_px,
                mode.height_px,
                true,
            )?;
            if *stride.get_or_insert(buffer_stride) != buffer_stride {
                return Err(Errno::DeviceFault);
            }
            grants.try_push(grant).map_err(|_| Errno::DeviceFault)
        });
        let answer = built
            .and_then(|()| ConfiguredFrames::new(stride.unwrap_or(0), delivery.grantor(), &grants));
        match answer {
            Ok(frames) => {
                self.configured = Some(configured);
                Ok(frames)
            }
            Err(err) => {
                for held in &configured.buffers {
                    let _ = display.free_buffer(held.id);
                }
                Err(err)
            }
        }
    }

    /// Allocate a further buffer for `lease`'s configuration.
    fn alloc(
        &mut self,
        display: &mut dyn Display,
        delivery: &mut dyn Delivery,
        ticket: u64,
        lease: (u64, u64),
        width_px: u32,
        height_px: u32,
    ) -> Result<GrantedBuffer, Errno> {
        let grantor = delivery.grantor();
        let configured = self.configured(lease)?;
        let (grant, stride_bytes) = grant_buffer(
            display, delivery, ticket, configured, width_px, height_px, false,
        )?;
        Ok(GrantedBuffer {
            grant,
            stride_bytes,
            grantor,
        })
    }

    /// Free a buffer `lease`'s configuration allocated beyond its frames.
    fn free(
        &mut self,
        display: &mut dyn Display,
        lease: (u64, u64),
        buffer: BufferId,
    ) -> Result<(), Errno> {
        let configured = self.configured(lease)?;
        let at = configured
            .buffers
            .iter()
            .position(|held| held.id == buffer)
            .ok_or(Errno::NotFound)?;
        if configured.buffers[at].frame {
            return Err(Errno::OutOfRange);
        }
        if configured.cursor == Some(buffer) {
            if let Some(cursor) = display.hardware_cursor() {
                cursor.hide_cursor().map_err(DriverError::as_errno)?;
            }
            configured.cursor = None;
        }
        configured.buffers.swap_remove(at);
        display.free_buffer(buffer).map_err(DriverError::as_errno)
    }

    /// Show output frame `buffer` for `lease`.
    fn present(
        &mut self,
        display: &mut dyn Display,
        lease: (u64, u64),
        buffer: BufferId,
        damage: DamageList,
    ) -> Result<PresentReply, Errno> {
        let output = display.output();
        let configured = self.configured(lease)?;
        if configured.queued.is_some() {
            return Err(Errno::WouldBlock);
        }
        if !configured.held(buffer).is_some_and(|held| held.frame) {
            return Err(Errno::NotFound);
        }
        if output != Some(configured.mode) {
            return Err(Errno::Stale);
        }
        DamageRect::validate_list(damage.rects(), &configured.mode)
            .map_err(DriverError::as_errno)?;
        let started_ns = self.clock.now_ns();
        let shown = display.present(buffer, damage.rects());
        self.settle(lease, started_ns, shown)
    }

    /// Composite `layers` in hardware for `lease`.
    fn present_layers(
        &mut self,
        display: &mut dyn Display,
        lease: (u64, u64),
        layers: &[AccelLayer],
    ) -> Result<PresentReply, Errno> {
        let output = display.output();
        let configured = self.configured(lease)?;
        if configured.queued.is_some() {
            return Err(Errno::WouldBlock);
        }
        if output != Some(configured.mode) {
            return Err(Errno::Stale);
        }
        let accel = display.as_accelerated().ok_or(Errno::NotSupported)?;
        let caps = accel.accel_caps();
        if layers.len() > caps.max_layers as usize {
            return Err(Errno::LengthOutOfRange);
        }
        for layer in layers {
            let held = configured.held(layer.buffer).ok_or(Errno::NotFound)?;
            layer
                .validate(&caps, held.width_px, held.height_px)
                .map_err(DriverError::as_errno)?;
        }
        let started_ns = self.clock.now_ns();
        let shown = accel.present_layers(layers);
        self.settle(lease, started_ns, shown)
    }

    /// Account the present that started at `started_ns`, and queue it for its
    /// latch where the device took it at its next frame.
    fn settle(
        &mut self,
        lease: (u64, u64),
        started_ns: u64,
        shown: Result<Presented, DriverError>,
    ) -> Result<PresentReply, Errno> {
        let now = self.clock.now_ns();
        self.window_start_ns.get_or_insert(started_ns);
        self.busy_ns = self.busy_ns.saturating_add(now.saturating_sub(started_ns));
        let presented = shown.map_err(DriverError::as_errno)?;
        self.presented = true;
        let configured = self.configured(lease)?;
        let seq = configured.next_seq;
        configured.next_seq = seq.wrapping_add(1);
        if presented == Presented::Queued {
            configured.queued = Some(Queued {
                seq,
                deadline_ns: now.saturating_add(PRESENT_LATCH_DEADLINE_NS),
            });
        }
        Ok(PresentReply { presented, seq })
    }

    /// Show `buffer` as `lease`'s pointer.
    fn set_cursor(
        &mut self,
        display: &mut dyn Display,
        lease: (u64, u64),
        buffer: BufferId,
        hot_x: u32,
        hot_y: u32,
    ) -> Result<(), Errno> {
        let configured = self.configured(lease)?;
        let held = configured
            .held(buffer)
            .filter(|held| !held.frame)
            .ok_or(Errno::NotFound)?;
        let cursor = display.hardware_cursor().ok_or(Errno::NotSupported)?;
        let caps = cursor.cursor_caps();
        if held.width_px > caps.max_width_px
            || held.height_px > caps.max_height_px
            || hot_x >= held.width_px
            || hot_y >= held.height_px
        {
            return Err(Errno::LengthOutOfRange);
        }
        cursor
            .set_cursor(buffer, hot_x, hot_y)
            .map_err(DriverError::as_errno)?;
        configured.cursor = Some(buffer);
        Ok(())
    }

    /// Move `lease`'s pointer.
    fn move_cursor(
        &mut self,
        display: &mut dyn Display,
        lease: (u64, u64),
        x: i32,
        y: i32,
    ) -> Result<(), Errno> {
        if self.configured(lease)?.cursor.is_none() {
            return Err(Errno::NotFound);
        }
        let cursor = display.hardware_cursor().ok_or(Errno::NotSupported)?;
        cursor.move_cursor(x, y).map_err(DriverError::as_errno)
    }

    /// Take `lease`'s pointer off the screen.
    fn hide_cursor(&mut self, display: &mut dyn Display, lease: (u64, u64)) -> Result<(), Errno> {
        let configured = self.configured(lease)?;
        let cursor = display.hardware_cursor().ok_or(Errno::NotSupported)?;
        cursor.hide_cursor().map_err(DriverError::as_errno)?;
        configured.cursor = None;
        Ok(())
    }

    /// Switch the display on or off for `lease`'s configuration.
    fn set_power(
        &mut self,
        display: &mut dyn Display,
        delivery: &mut dyn Delivery,
        lease: (u64, u64),
        power: DisplayPower,
    ) -> Result<(), Errno> {
        self.configured(lease)?;
        display.set_power(power).map_err(DriverError::as_errno)?;
        self.power = match power {
            DisplayPower::On => Power::On,
            DisplayPower::Off => Power::Off,
        };
        // Outputs switched off latch the present they were to take, so its
        // presenter is told now rather than at its deadline.
        if let Ok(serviced) = display.service_events() {
            let _ = self.serviced(serviced, delivery);
        }
        Ok(())
    }
}

/// Allocate a `width_px` × `height_px` buffer into `configured` and grant it
/// to the caller `ticket`, answering the grant and the buffer's stride. A
/// buffer that cannot be granted is freed again.
fn grant_buffer(
    display: &mut dyn Display,
    delivery: &mut dyn Delivery,
    ticket: u64,
    configured: &mut Configured,
    width_px: u32,
    height_px: u32,
    frame: bool,
) -> Result<(BufferGrant, u32), Errno> {
    if configured.buffers.is_full() {
        return Err(Errno::NoSpace);
    }
    let allocated = display
        .alloc_buffer(width_px, height_px)
        .map_err(DriverError::as_errno)?;
    let granted = delivery.grant(allocated.region, ticket);
    let held = Held {
        id: allocated.id,
        width_px,
        height_px,
        frame,
    };
    match granted.and_then(|grant| {
        configured
            .buffers
            .try_push(held)
            .map(|()| grant)
            .map_err(|_| Errno::NoSpace)
    }) {
        Ok(grant) => Ok((
            BufferGrant {
                buffer: allocated.id,
                grant,
            },
            allocated.stride_bytes,
        )),
        Err(err) => {
            let _ = display.free_buffer(allocated.id);
            Err(err)
        }
    }
}

/// The lease `seat_id`'s live owner holds, when the caller `ticket` is it.
fn leased(peer: &mut dyn PeerFacts, ticket: u64, seat_id: u64) -> Result<(u64, u64), Errno> {
    peer.live_generation(ticket, seat_id)
        .map(|generation| (seat_id, generation))
}

/// Copy an encoded reply frame into `reply`, returning its length.
fn frame(reply: &mut [u8; DISPLAY_REPLY_MAX], encoded: &[u8]) -> usize {
    reply[..encoded.len()].copy_from_slice(encoded);
    encoded.len()
}

/// Encode a status outcome into `reply`, returning the frame length.
fn status(reply: &mut [u8; DISPLAY_REPLY_MAX], result: Result<(), Errno>) -> usize {
    frame(reply, &encode_status_reply(result)[..STATUS_REPLY_LEN])
}

//! The display service's `Run` loop (feature `service`): the one bring-up and
//! serve loop every display driver's service binary runs, so the rendezvous,
//! the park, the kernel-attested caller facts and the lease handling cannot
//! differ between drivers.
//!
//! A driver's `Run` builds its driver host from the grants the kernel minted
//! for it, brings its surface up ([`open_surface`] for a granted linear
//! scan-out surface), and hands the resulting [`Display`] to [`serve`] with
//! the display's own event sources, which never returns while the service is
//! healthy.
//!
//! [`serve`] parks on one wait-set holding the reserved [`DISPLAY_ENDPOINT`],
//! the kernel's [`NoticeTopic::DisplayLease`] announcement, and the display's
//! event sources: a request is served through the [`DisplayServer`] engine; a
//! lease edge is handed to [`DisplayServer::lease_moved`], which releases a
//! configuration whose lease ended and lights the display it left dark; and a
//! display event is handed to [`Display::service_events`], after which an
//! output that moved is announced on [`NoticeTopic::DisplayOutput`] before
//! another request is served (`plans/DISPLAY.md` D12). An idle service
//! therefore costs no CPU, and no presenter can leave the machine dark behind
//! it.

use tairix_abi::display_ipc::{DisplayEvent, DISPLAY_ENDPOINT, DISPLAY_MAX_REQUEST};
use tairix_abi::driver::display::{Display, OutputMode, RefreshRate};
use tairix_abi::driver::{sole_framebuffer, DeviceWake};
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::{CapabilityId, Errno, ProcId, WaitSetOp, WaitSourceKind};
use tairix_caps::CapabilitySet;
use tairix_drvrt::exit::{end, error, step, DriverExit};
use tairix_drvrt::{GrantSyscalls, RtDriverHost};
use tairix_log::{EventId, Field, FieldValue, Level};
use tairix_rt::changenotice::ChangeNotice;
use tairix_rt::{ClockDelay, LogSink};

use crate::events::{
    EVENTS_FAILED, FIRST_PRESENT, FIRST_PRESENT_MESSAGE, LATCH_OVERDUE, LATCH_UNDELIVERED,
    OUTPUT_MOVED, OUTPUT_UNANNOUNCED,
};
use crate::rt::RtBufferMemory;
use crate::server::{Delivery, DisplayServer, Latch, PeerFacts, DISPLAY_REPLY_MAX};
use crate::{Framebuffer, FramebufferConfig};

/// Outstanding calls the endpoint queues. A fail-closed memory bound: one
/// session presents synchronously, so a small queue is ample.
const CAPACITY: usize = 4;

/// Wait-set token of the endpoint.
const ENDPOINT_TOKEN: u64 = 1;

/// Wait-set token of the lease announcement.
const LEASE_TOKEN: u64 = 2;

/// Wait-set token every one of the display's event sources carries: the
/// display reads all of its causes on any one of them.
const EVENT_TOKEN: u64 = 3;

/// Map the one scan-out surface the kernel granted `host`.
///
/// # Errors
///
/// The exit status to end with, its reason recorded: grants naming no single
/// valid surface, or a window that will not map.
pub fn open_surface<S: GrantSyscalls>(
    host: &RtDriverHost<S>,
) -> Result<Framebuffer<'_, RtBufferMemory>, i32> {
    let (phys_base, mode) = sole_framebuffer(host.resources()).map_err(|err| {
        end(
            DriverExit::NoResources,
            &[
                step("display: the node grants no single valid scan-out surface"),
                error(err),
            ],
        )
    })?;
    let config = FramebufferConfig {
        phys_base,
        width_px: mode.width_px,
        height_px: mode.height_px,
        stride_bytes: mode.stride_bytes,
        format: mode.format,
    };
    // The host wires no seat gate: the per-request lease check is the
    // engine's kernel-attested `call_peer_seat`.
    Framebuffer::open(host, config, RtBufferMemory).map_err(|err| {
        end(
            DriverExit::BringupFailed,
            &[
                step("display: the scan-out surface would not map"),
                error(err),
            ],
        )
    })
}

/// Serve the display endpoint over `display`, whose output moves on `wakes`,
/// for the life of the service, ending it with its reason recorded the
/// moment it cannot go on.
///
/// A display whose output never moves names no wake.
pub fn serve(display: &mut dyn Display, wakes: &[DeviceWake]) -> i32 {
    let wait_set = match bind(wakes) {
        Ok(wait_set) => wait_set,
        Err(code) => return code,
    };
    // Announced once the rendezvous is bound, which is the authority, and
    // before the first request is served: a client of an earlier instance
    // converges onto this one.
    let mut output = Announced::resume(crate::server::output_mode(display));

    let Ok(grantor) = tairix_rt::self_origin().map(|origin| origin.proc_id()) else {
        return end(
            DriverExit::NoService,
            &[step(
                "display: the service could not learn its own identity",
            )],
        );
    };
    let mut server = DisplayServer::new(ClockDelay);
    let mut peer = RtPeerFacts;
    let mut delivery = RtDelivery { grantor };
    let mut request = [0u8; DISPLAY_MAX_REQUEST];
    let mut reply = [0u8; DISPLAY_REPLY_MAX];
    let mut token = 0u64;
    let mut first_present_logged = false;
    loop {
        // A queued present bounds the wait: its latch, or its deadline.
        let timeout = server.deadline_ns().map_or(u64::MAX, |deadline| {
            deadline.saturating_sub(tairix_rt::clock_get())
        });
        match tairix_rt::waitset_wait(wait_set, timeout, &mut token) {
            0 => {}
            ret if Errno::from_syscall(ret) == Errno::TimedOut => {
                note_latch(server.expire(display, &mut delivery));
                continue;
            }
            _ => {
                return end(
                    DriverExit::ServeFailed,
                    &[step("display: the serve wait set faulted")],
                );
            }
        }
        if token == LEASE_TOKEN {
            if let Some(lease) = read_lease() {
                server.lease_moved(display, lease);
            }
            continue;
        }
        if token == EVENT_TOKEN {
            service_events(display, &mut server, &mut delivery, &mut output);
            continue;
        }
        let mut ticket = 0u64;
        // Non-blocking: the park point is the wait-set, and the call the wake
        // reported may have been cancelled by its poster's exit.
        let Ok(len) = tairix_rt::call_recv_nonblock(DISPLAY_ENDPOINT, &mut request, &mut ticket)
        else {
            continue;
        };
        let n = server.serve(
            display,
            &mut peer,
            &mut delivery,
            ticket,
            &request[..len],
            &mut reply,
        );
        let _ = tairix_rt::call_reply(DISPLAY_ENDPOINT, ticket, &reply[..n]);
        if !first_present_logged && server.has_presented() {
            first_present_logged = true;
            record(Level::Info, FIRST_PRESENT, FIRST_PRESENT_MESSAGE, &[]);
        }
        // A request that waited on the device may have taken the wake one of
        // its event sources raised.
        if display.requests_take_wakes() {
            service_events(display, &mut server, &mut delivery, &mut output);
        }
    }
}

/// Hand what woke the display's event sources to `server`, and announce an
/// output that moved.
fn service_events(
    display: &mut dyn Display,
    server: &mut DisplayServer<ClockDelay>,
    delivery: &mut RtDelivery,
    output: &mut Announced,
) {
    match display.service_events() {
        Ok(serviced) => note_latch(server.serviced(serviced, delivery)),
        Err(err) => record(
            Level::Warn,
            EVENTS_FAILED,
            "display: its event sources could not be read",
            &[Field {
                key: "error",
                value: FieldValue::SignedInt(i64::from(err.as_i32())),
            }],
        ),
    }
    output.follow(crate::server::output_mode(display));
}

/// Bind the display endpoint and the wait set the service parks on: the
/// endpoint, the lease's notices, and `wakes`.
///
/// # Errors
///
/// The exit the service ends with, its reason recorded.
fn bind(wakes: &[DeviceWake]) -> Result<u64, i32> {
    // Unrestricted-sender: the engine gates every request — `Query`
    // included — on the caller's live seat lease, so an unentitled sender
    // is answered with a typed refusal.
    let empty = CapabilitySet::empty();
    if tairix_rt::call_create(
        DISPLAY_ENDPOINT,
        &empty,
        &empty,
        DISPLAY_MAX_REQUEST,
        DISPLAY_REPLY_MAX,
        CAPACITY,
    ) != 0
    {
        return Err(end(
            DriverExit::NoService,
            &[step("display: the display endpoint could not be bound")],
        ));
    }
    let Ok(wait_set) = u64::try_from(tairix_rt::waitset_create()) else {
        return Err(end(
            DriverExit::NoService,
            &[step("display: the serve wait set could not be created")],
        ));
    };
    let members = [
        (WaitSourceKind::Endpoint, DISPLAY_ENDPOINT, ENDPOINT_TOKEN),
        (
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::DisplayLease.as_u32()),
            LEASE_TOKEN,
        ),
    ];
    let events = wakes.iter().map(|wake| {
        let (kind, id) = wake.member();
        (kind, id, EVENT_TOKEN)
    });
    for (kind, id, token) in members.into_iter().chain(events) {
        if tairix_rt::waitset_ctl(wait_set, WaitSetOp::Add, kind, id, token) != 0 {
            return Err(end(
                DriverExit::NoService,
                &[step(
                    "display: an event source could not join the serve wait set",
                )],
            ));
        }
    }
    Ok(wait_set)
}

/// The output as last announced on [`NoticeTopic::DisplayOutput`], and the
/// count of moves announced with it.
struct Announced {
    notice: ChangeNotice,
    changes: u64,
    output: Option<OutputMode>,
}

impl Announced {
    /// Take the topic up for `output`, announcing it at once.
    fn resume(output: Option<OutputMode>) -> Self {
        let (notice, changes) = ChangeNotice::resume(NoticeTopic::DisplayOutput);
        let mut announced = Self {
            notice,
            changes,
            output,
        };
        announced.announce();
        announced
    }

    /// Announce `output` if it is not the output last announced: a new mode,
    /// or the same mode at a new rate.
    fn follow(&mut self, output: Option<OutputMode>) {
        if output == self.output {
            return;
        }
        self.output = output;
        self.changes = self.changes.wrapping_add(1);
        let (width, height) = output.map_or((0, 0), |output| {
            (output.mode.width_px, output.mode.height_px)
        });
        let refresh = output
            .and_then(|output| output.refresh)
            .map_or(0, RefreshRate::mhz);
        record(
            Level::Info,
            OUTPUT_MOVED,
            "display: the output moved",
            &[
                Field {
                    key: "width",
                    value: FieldValue::UnsignedInt(u64::from(width)),
                },
                Field {
                    key: "height",
                    value: FieldValue::UnsignedInt(u64::from(height)),
                },
                Field {
                    key: "refresh_mhz",
                    value: FieldValue::UnsignedInt(u64::from(refresh)),
                },
            ],
        );
        self.announce();
    }

    fn announce(&mut self) {
        if self.notice.announce(self.changes).is_err() {
            record(
                Level::Warn,
                OUTPUT_UNANNOUNCED,
                "display: the output could not be announced",
                &[],
            );
        }
    }
}

/// Write one of the service's records.
fn record(level: Level, id: EventId, message: &str, fields: &[Field<'_>]) {
    tairix_log::log(
        &LogSink,
        &tairix_log::Event {
            level,
            id,
            message,
            fields,
        },
    );
}

/// The boot seat's lease as the kernel publishes it, or `None` for a read
/// the kernel refused or a payload that does not decode — which leaves the
/// configuration to the next edge rather than acting on a guess.
fn read_lease() -> Option<tairix_abi::seat::DisplayLease> {
    let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
    let read = usize::try_from(tairix_rt::notice_read(NoticeTopic::DisplayLease, &mut buf)).ok()?;
    match Notice::decode(NoticeTopic::DisplayLease, buf.get(..read)?) {
        Ok(Notice::DisplayLease(lease)) => Some(lease),
        _ => None,
    }
}

/// The kernel's `call_peer_seat` and `call_peer_origin` on the served
/// endpoint: every authority fact is about the in-flight caller of this
/// service, never a claim its request carried.
struct RtPeerFacts;

impl PeerFacts for RtPeerFacts {
    fn live_generation(&mut self, ticket: u64, seat_id: u64) -> Result<u64, Errno> {
        let ret = tairix_rt::call_peer_seat(DISPLAY_ENDPOINT, ticket, seat_id);
        u64::try_from(ret)
            .ok()
            .filter(|generation| *generation >= 1)
            .ok_or_else(|| Errno::from_syscall(ret))
    }

    fn holds_capability(&mut self, ticket: u64, cap: CapabilityId) -> Result<bool, Errno> {
        Ok(tairix_rt::peer_origin(DISPLAY_ENDPOINT, ticket)?
            .capabilities()
            .holds_cap(cap))
    }

    fn owns_port(&mut self, ticket: u64, port: u64) -> bool {
        tairix_rt::call_peer_port(DISPLAY_ENDPOINT, ticket, port).is_ok()
    }
}

/// The kernel acting for the service: grants to the caller in service, and
/// posts to a presenter's port.
struct RtDelivery {
    grantor: ProcId,
}

impl Delivery for RtDelivery {
    fn grant(&mut self, region: u64, ticket: u64) -> Result<u64, Errno> {
        let ret = tairix_rt::shm_grant_peer(region, DISPLAY_ENDPOINT, ticket);
        u64::try_from(ret)
            .ok()
            .filter(|handle| *handle != 0)
            .ok_or_else(|| Errno::from_syscall(ret))
    }

    fn grantor(&self) -> ProcId {
        self.grantor
    }

    fn post(&mut self, port: u64, event: &DisplayEvent) -> Result<(), Errno> {
        match tairix_rt::ipc_send(port, &event.to_le_bytes()) {
            0 => Ok(()),
            ret => Err(Errno::from_syscall(ret)),
        }
    }
}

/// Record a latch that came due unlatched or could not be told.
fn note_latch(latch: Latch) {
    match latch {
        Latch::Idle | Latch::Posted => {}
        Latch::Overdue => record(
            Level::Warn,
            LATCH_OVERDUE,
            "display: a queued present never latched; its presenter is released",
            &[],
        ),
        Latch::Undelivered(err) => record(
            Level::Warn,
            LATCH_UNDELIVERED,
            "display: a latch could not reach its presenter",
            &[Field {
                key: "error",
                value: FieldValue::SignedInt(i64::from(err.as_i32())),
            }],
        ),
    }
}

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
//! it. While a latch is owed to a presenter whose port had no room, the set
//! also watches that port's room, and the retry of one the kernel had no
//! memory for bounds the park.

use tairix_abi::display_ipc::{DisplayEvent, DISPLAY_ENDPOINT, DISPLAY_MAX_REQUEST};
use tairix_abi::driver::display::{Display, OutputMode, RefreshRate};
use tairix_abi::driver::{sole_framebuffer, DeviceWake};
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::seat::DisplayLease;
use tairix_abi::{CapabilityId, Errno, ProcId, WaitSetOp, WaitSourceKind};
use tairix_caps::CapabilitySet;
use tairix_drvrt::exit::{end, error, step, DriverExit};
use tairix_drvrt::{GrantSyscalls, RtDriverHost};
use tairix_log::{EventId, Field, FieldValue, Level, Sink};
use tairix_rt::changenotice::ChangeNotice;
use tairix_rt::room::RoomMember;
use tairix_rt::{ClockDelay, LogSink};

use crate::events::{OUTPUT_MOVED, OUTPUT_UNANNOUNCED};
use crate::rt::RtBufferMemory;
use crate::server::{Delivery, DisplayServer, PeerFacts, DISPLAY_REPLY_MAX};
use crate::serving::{Rendezvous, Serving, Woke};
use crate::{Framebuffer, FramebufferConfig};

/// Calls one caller may hold at the endpoint. A fail-closed memory bound: a
/// session presents synchronously, so it holds one at a time.
const CALLER_DEPTH: usize = 4;

/// Wait-set token of the endpoint.
const ENDPOINT_TOKEN: u64 = 1;

/// Wait-set token of the lease announcement.
const LEASE_TOKEN: u64 = 2;

/// Wait-set token every one of the display's event sources carries: the
/// display reads all of its causes on any one of them.
const EVENT_TOKEN: u64 = 3;

/// Wait-set token of the room member watching the port an owed latch waits
/// on.
const ROOM_TOKEN: u64 = 4;

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
    let output = Announced::resume(crate::server::output_mode(display));

    let Ok(grantor) = tairix_rt::self_origin().map(|origin| origin.proc_id()) else {
        return end(
            DriverExit::NoService,
            &[step(
                "display: the service could not learn its own identity",
            )],
        );
    };
    let mut serving = Serving::new(DisplayServer::new(ClockDelay), RtRendezvous { output });
    let mut peer = RtPeerFacts;
    let mut delivery = RtDelivery { grantor };
    let mut token = 0u64;
    let mut room = RoomMember::new(wait_set, ROOM_TOKEN);
    loop {
        serving.report_undelivered();
        // A port whose room cannot be watched has its latch posted again on
        // the retry instead.
        if !room.keep_on(serving.server.room_port()) {
            serving.server.room_unwatched();
        }
        // A queued present bounds the wait, its latch or its deadline, and so
        // does the retry of a latch the kernel had no memory for.
        let timeout = serving.server.deadline_ns().map_or(u64::MAX, |deadline| {
            deadline.saturating_sub(tairix_rt::clock_get())
        });
        let woke = match tairix_rt::waitset_wait(wait_set, timeout, &mut token) {
            0 => match token {
                ROOM_TOKEN => Woke::Room,
                LEASE_TOKEN => Woke::Lease,
                EVENT_TOKEN => Woke::Events,
                _ => Woke::Request,
            },
            ret if Errno::from_syscall(ret) == Errno::TimedOut => Woke::Due,
            _ => {
                return end(
                    DriverExit::ServeFailed,
                    &[step("display: the serve wait set faulted")],
                );
            }
        };
        serving.on(display, &mut peer, &mut delivery, woke);
    }
}

/// The kernel's side of the serve loop.
struct RtRendezvous {
    output: Announced,
}

impl Sink for RtRendezvous {
    fn write_event(&self, event: &tairix_log::Event<'_>) {
        LogSink.write_event(event);
    }
}

impl Rendezvous for RtRendezvous {
    fn take_request(&mut self, request: &mut [u8]) -> Option<(usize, u64)> {
        let mut ticket = 0u64;
        let len = tairix_rt::call_recv_nonblock(DISPLAY_ENDPOINT, request, &mut ticket).ok()?;
        Some((len, ticket))
    }

    fn reply(&mut self, ticket: u64, reply: &[u8]) {
        let _ = tairix_rt::call_reply(DISPLAY_ENDPOINT, ticket, reply);
    }

    /// The boot seat's lease as the kernel publishes it, or `None` for a read
    /// the kernel refused or a payload that does not decode — which leaves the
    /// configuration to the next edge rather than acting on a guess.
    fn lease(&mut self) -> Option<DisplayLease> {
        let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
        let read =
            usize::try_from(tairix_rt::notice_read(NoticeTopic::DisplayLease, &mut buf)).ok()?;
        match Notice::decode(NoticeTopic::DisplayLease, buf.get(..read)?) {
            Ok(Notice::DisplayLease(lease)) => Some(lease),
            _ => None,
        }
    }

    fn follow(&mut self, output: Option<OutputMode>) {
        self.output.follow(output);
    }
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
        CALLER_DEPTH,
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
        tairix_rt::ipc_send(port, &event.to_le_bytes())
    }
}

//! The display service's `Run` loop (feature `service`): the one bring-up and
//! serve loop every display driver's service binary runs, so the rendezvous,
//! the park, the kernel-attested caller facts and the lease handling cannot
//! differ between drivers.
//!
//! A driver's `Run` builds its driver host from the grants the kernel minted
//! for it, brings its surface up ([`open_surface`] for a granted linear
//! scan-out surface), and hands the resulting [`Display`] to [`serve`], which
//! never returns while the service is healthy.
//!
//! [`serve`] parks on one wait-set holding the reserved [`DISPLAY_ENDPOINT`]
//! and the kernel's [`NoticeTopic::DisplayLease`] announcement: a request is
//! served through the [`DisplayServer`] engine, and a lease edge is handed to
//! [`DisplayServer::lease_moved`], which releases a configuration whose lease
//! ended and lights the display it left dark. An idle service therefore costs
//! no CPU, and no presenter can leave the machine dark behind it.

use tairix_abi::display_ipc::{DISPLAY_ENDPOINT, DISPLAY_MAX_REQUEST};
use tairix_abi::driver::display::Display;
use tairix_abi::driver::sole_framebuffer;
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::origin::{Origin, ORIGIN_WIRE_LEN};
use tairix_abi::time::MonotonicClock;
use tairix_abi::{CapabilityId, Errno, WaitSetOp, WaitSourceKind};
use tairix_caps::CapabilitySet;
use tairix_drvrt::{GrantSyscalls, RtDriverHost};
use tairix_log::EventId;
use tairix_rt::LogSink;

use crate::rt::RtShmMapper;
use crate::server::{DisplayServer, PeerFacts, DISPLAY_REPLY_MAX};
use crate::{Framebuffer, FramebufferConfig};

/// Exit code when the driver host could not be built from the
/// kernel-delivered grants. A reserved, fail-closed value.
pub const EXIT_NO_HOST: i32 = 80;

/// Exit code when the delivered grants do not name exactly one valid
/// scan-out surface — an unbound or mis-provisioned node. The service never
/// scans out a guessed geometry.
pub const EXIT_NO_RESOURCES: i32 = 81;

/// Exit code when the surface could not be brought up (the scan-out window
/// could not be mapped).
pub const EXIT_BRINGUP_FAILED: i32 = 82;

/// Exit code when the reserved [`DISPLAY_ENDPOINT`] could not be bound
/// (already bound, or the manifest lacks the privileged bind right): exiting
/// leaves the seat without a display service, never a squattable one.
pub const EXIT_NO_ENDPOINT: i32 = 83;

/// Exit code when the wait-set the loop parks on could not be created,
/// populated, or waited on: the service exits rather than degrade into a busy
/// re-poll.
pub const EXIT_WAIT_FAILED: i32 = 84;

/// Range start (inclusive) reserved for the display service's event
/// identifiers, per the `lib/log` convention of one 1 000-wide range per
/// subsystem. Once shipped the values are never re-used or re-numbered.
pub const DISPLAY_SERVICE_RANGE_START: u32 = 15_000;

/// Range end (exclusive) reserved for display-service event identifiers.
pub const DISPLAY_SERVICE_RANGE_END: u32 = 16_000;

/// One-shot: the first client frame reached the scan-out surface since this
/// service started — the witness that the session → service → surface path
/// is live end to end. Emitted after the reply, so the present path pays
/// nothing.
pub const FIRST_PRESENT: EventId = EventId(15_001);

/// The exact message [`FIRST_PRESENT`] is emitted with. A log consumer keys
/// on this rendered text, so it is defined once beside the id.
pub const FIRST_PRESENT_MESSAGE: &str = "first client frame presented to scan-out";

const _: () = assert!(
    FIRST_PRESENT.0 >= DISPLAY_SERVICE_RANGE_START && FIRST_PRESENT.0 < DISPLAY_SERVICE_RANGE_END,
    "a display-service event lies in the display service's range"
);

/// Outstanding calls the endpoint queues. A fail-closed memory bound: one
/// session presents synchronously, so a small queue is ample.
const CAPACITY: usize = 4;

/// Wait-set token of the endpoint.
const ENDPOINT_TOKEN: u64 = 1;

/// Wait-set token of the lease announcement.
const LEASE_TOKEN: u64 = 2;

/// Map the one scan-out surface the kernel granted `host`.
///
/// # Errors
///
/// [`EXIT_NO_RESOURCES`] when the grants name no single valid surface, and
/// [`EXIT_BRINGUP_FAILED`] when its window cannot be mapped.
pub fn open_surface<S: GrantSyscalls>(host: &RtDriverHost<S>) -> Result<Framebuffer<'_>, i32> {
    let (phys_base, mode) = sole_framebuffer(host.resources()).map_err(|_| EXIT_NO_RESOURCES)?;
    let config = FramebufferConfig {
        phys_base,
        width_px: mode.width_px,
        height_px: mode.height_px,
        stride_bytes: mode.stride_bytes,
        format: mode.format,
    };
    // The host wires no seat gate: the per-request lease check is the
    // engine's kernel-attested `call_peer_seat`.
    Framebuffer::open(host, config).map_err(|_| EXIT_BRINGUP_FAILED)
}

/// Serve the display endpoint over `display` for the life of the service,
/// answering the reserved exit code the moment it cannot go on.
pub fn serve(display: &mut dyn Display) -> i32 {
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
        return EXIT_NO_ENDPOINT;
    }
    let Ok(wait_set) = u64::try_from(tairix_rt::waitset_create()) else {
        return EXIT_WAIT_FAILED;
    };
    let members = [
        (WaitSourceKind::Endpoint, DISPLAY_ENDPOINT, ENDPOINT_TOKEN),
        (
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::DisplayLease.as_u32()),
            LEASE_TOKEN,
        ),
    ];
    for (kind, id, token) in members {
        if tairix_rt::waitset_ctl(wait_set, WaitSetOp::Add, kind, id, token) != 0 {
            return EXIT_WAIT_FAILED;
        }
    }

    let mut server = DisplayServer::new(RtShmMapper, RtClock);
    let mut peer = RtPeerFacts;
    let mut request = [0u8; DISPLAY_MAX_REQUEST];
    let mut reply = [0u8; DISPLAY_REPLY_MAX];
    let mut token = 0u64;
    let mut first_present_logged = false;
    loop {
        if tairix_rt::waitset_wait(wait_set, u64::MAX, &mut token) != 0 {
            return EXIT_WAIT_FAILED;
        }
        if token == LEASE_TOKEN {
            if let Some(lease) = read_lease() {
                server.lease_moved(display, lease);
            }
            continue;
        }
        let mut ticket = 0u64;
        // Non-blocking: the park point is the wait-set, and the call the wake
        // reported may have been cancelled by its poster's exit.
        let Ok(len) = tairix_rt::call_recv_nonblock(DISPLAY_ENDPOINT, &mut request, &mut ticket)
        else {
            continue;
        };
        let n = server.serve(display, &mut peer, ticket, &request[..len], &mut reply);
        let _ = tairix_rt::call_reply(DISPLAY_ENDPOINT, ticket, &reply[..n]);
        if !first_present_logged && server.has_presented() {
            first_present_logged = true;
            tairix_log::log(
                &LogSink,
                &tairix_log::Event {
                    level: tairix_log::Level::Info,
                    id: FIRST_PRESENT,
                    message: FIRST_PRESENT_MESSAGE,
                    fields: &[],
                },
            );
        }
    }
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
        let mut bytes = [0u8; ORIGIN_WIRE_LEN];
        let len = tairix_rt::call_peer_origin(DISPLAY_ENDPOINT, ticket, &mut bytes)
            .map_err(Errno::from_syscall)?;
        if len != bytes.len() {
            return Err(Errno::BufferTooSmall);
        }
        Ok(Origin::from_bytes(&bytes)?.capabilities().holds_cap(cap))
    }
}

/// The unprivileged `clock_get`, which the engine brackets each present with
/// so the utilisation a monitor reads is measured where presents happen.
struct RtClock;

impl MonotonicClock for RtClock {
    fn now_ns(&self) -> u64 {
        tairix_rt::clock_get()
    }
}

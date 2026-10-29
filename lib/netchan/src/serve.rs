//! The freestanding driver-process serve loop of the `netchan-v1` device
//! channel (`plans/NETWORK.md` N4d).
//!
//! This is the I/O half of the contract's driver side: everything a NIC
//! driver process must do *around* an opened [`Net`] device to serve the
//! network stack, written once for every such driver rather than copied per
//! device. It claims a
//! reserved device-channel endpoint bound restricted-sender, publishes the
//! [`NETCHAN_NODE_COMPATIBLE`] node so `devmgr` hands the endpoint to the
//! stack, and then parks — never busy-polls — on a wait set over two
//! sources:
//!
//! * a **call** wake decodes one request, admits its caller, and drives the
//!   pure [`NetChannelServer`]; `Attach` maps the granted frame region,
//!   `Service` drives one device doorbell over it, `Detach` unmaps it;
//! * an **interrupt** wake masks the device's completion sources, harvests
//!   the rings into the shared region itself, and wakes the stack with a
//!   single notify.
//!
//! # Why the interrupt path harvests, and why it masks first
//!
//! Both halves of this exist because the naive shape — acknowledge, notify,
//! re-park — is pathological on real hardware.
//!
//! *Masking.* A DMA engine's completion status is a latch over a **level**
//! condition ("completed descriptors are waiting"). Acknowledging clears the
//! latch, but with frames still undrained the condition re-latches at once,
//! and the kernel re-arms the line every time this process parks. The driver
//! then spins interrupt → acknowledge → notify → park at the speed of a
//! context switch until the stack catches up — measurable as a permanently
//! busy core on an otherwise idle machine. So the completion sources are
//! masked on entry and unmasked only once the device has nothing left and
//! the shared ring has room; a burst costs one interrupt instead of one per
//! frame, which is the coalescing a fixed frame threshold cannot give.
//!
//! *Harvesting.* The frame region is already mapped here, so making the
//! stack ask for the frames with a blocking call costs two extra process
//! switches per batch for nothing. The interrupt fills the ring and rings
//! the doorbell once; the stack reads the ring in its own time and calls
//! back only when it has transmit work or the driver reported
//! back-pressure. The ring's atomic counters are what make that safe.
//!
//! Compiled only for the bare-metal targets a driver binary is built for.

use tairix_abi::driver::net::Net;
use tairix_abi::driver::net_channel::{
    AttachParams, NetChannelNotify, NetChannelRequest, NETCHAN_NODE_COMPATIBLE,
    NET_CHANNEL_ENDPOINTS, NET_CHANNEL_MAX_REPLY, NET_CHANNEL_MAX_REQUEST,
};
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
use tairix_abi::{CapabilityId, Errno, HwDeviceClass, ProcId, PROC_ID_HEX_LEN};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::{LogSink, ServedCall};

use crate::{Drain, DrainAction, Drained, NetChannelServer};
use tairix_drvrt::exit::{end, step, DriverExit};
use tairix_drvrt::serve::{instance_field, log_event, publish_channel, DeviceChannel};

/// Diagnostic event id: the one-shot "device channel published, serving"
/// beacon a NIC driver emits once its device is live and its endpoint is
/// bound.
const NETCHAN_READY: EventId = EventId(29_180);

/// Audit event id: a call refused because its caller is not the network
/// stack, or not the stack instance attached.
const NETCHAN_REFUSED: EventId = EventId(29_181);

/// Wait-set token for a device-channel call doorbell on the claimed
/// endpoint.
const CALL_TOKEN: u64 = 1;

/// Wait-set token for "the device interrupt fired".
const IRQ_TOKEN: u64 = 2;

/// Outstanding-call capacity of the device-channel endpoint. The stack
/// issues one control request at a time (it blocks on the reply); a small
/// queue absorbs a doorbell racing the previous reply — a fail-closed
/// memory bound.
const ENDPOINT_CAPACITY: usize = 4;

/// The device channel, served restricted-sender to holders of `CAP_NET_RAW`.
/// The tools that open echo sockets hold it too, so every call is also
/// admitted by [`NetChannelServer::admit`]: the network stack's alone.
const CHANNEL: DeviceChannel = DeviceChannel {
    block: NET_CHANNEL_ENDPOINTS,
    gate: CapabilityId::NET_RAW,
    max_request: NET_CHANNEL_MAX_REQUEST,
    max_reply: NET_CHANNEL_MAX_REPLY,
    capacity: ENDPOINT_CAPACITY,
};

/// Device doorbells one wake may drive before returning to the wait set.
///
/// A fixed containment bound, not a capacity: a saturating flood must not
/// pin this process in the drain loop and starve its call endpoint. Nothing
/// is lost by stopping — the completion sources stay masked while the device
/// still has frames, so the remaining work is picked up without another
/// interrupt being needed to find it.
const SERVICE_ROUNDS: u32 = 16;

/// One mapping of the shared frame region the stack granted in `Attach`.
struct Region {
    /// Base virtual address of the [`shm_map`](tairix_rt::shm_map)ping.
    base: u64,
    /// Full mapped byte length — page-rounded by the kernel, so possibly
    /// larger than the ring geometry — released verbatim by the matching
    /// `shm_unmap`.
    len: usize,
    /// The exclusive ring view: the first `geometry.region_len()` bytes of
    /// the mapping (a subset of `len`), which the `Service` doorbell binds
    /// the frame rings across.
    bytes: &'static mut [u8],
}

/// Serve the `netchan-v1` device channel over the opened device `net` for
/// the life of the driver process.
///
/// `irq_handle` is the bound device interrupt (from `irq_bind` on the line
/// the driver's matched node granted) the loop parks on alongside the call
/// endpoint. Never returns on the success path; every set-up refusal ends
/// the driver fail-closed with its reason recorded rather than degrading
/// into a busy re-poll.
pub fn serve<N: Net>(net: N, irq_handle: u64) -> i32 {
    let Some(endpoint) = CHANNEL.bind() else {
        return end(
            DriverExit::NoService,
            &[step(
                "netchan: the node's device-channel endpoint could not be bound",
            )],
        );
    };
    if publish_channel(
        HwDeviceClass::Network,
        NETCHAN_NODE_COMPATIBLE,
        endpoint,
        &[],
    )
    .is_none()
    {
        return end(
            DriverExit::NoService,
            &[step(
                "netchan: the device-channel node could not be published",
            )],
        );
    }

    let set = tairix_rt::waitset_create();
    if set < 0 {
        return end(
            DriverExit::NoService,
            &[step("netchan: the serve wait set could not be created")],
        );
    }
    #[allow(clippy::cast_sign_loss)] // `set >= 0` is the wait-set handle.
    let set = set as u64;
    if tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Endpoint,
        endpoint,
        CALL_TOKEN,
    ) != 0
        || tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Irq,
            irq_handle,
            IRQ_TOKEN,
        ) != 0
    {
        return end(
            DriverExit::NoService,
            &[step(
                "netchan: the call endpoint and the interrupt could not join the wait set",
            )],
        );
    }

    log(
        &LogSink,
        &Event {
            level: Level::Info,
            id: NETCHAN_READY,
            message: "netchan: device channel published, serving",
            fields: &[],
        },
    );

    let mut server = NetChannelServer::new(net);
    // The channel starts detached, so there is nowhere to harvest into yet.
    // Bring-up left the device's completion sources enabled; mask them until
    // an `Attach` gives this driver a region, or a device already receiving
    // traffic would storm a driver that can only drop it.
    let _ = server.net_mut().set_completion_interrupts(false);
    serve_loop(server, set, endpoint)
}

/// Park on the wait set and serve device-channel doorbells and device
/// interrupts for the life of the driver. Never returns on the success
/// path; a wait-set fault exits fail-closed.
fn serve_loop<N: Net>(mut server: NetChannelServer<N>, set: u64, endpoint: u64) -> i32 {
    let mut region: Option<Region> = None;
    let mut request = [0u8; NET_CHANNEL_MAX_REQUEST];
    loop {
        let mut token = 0u64;
        let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
        if woke < 0 {
            return end(
                DriverExit::ServeFailed,
                &[step("netchan: the serve wait set faulted")],
            );
        }
        if woke != 0 {
            // A spurious/lapsed wake with no ready source; re-park.
            continue;
        }
        match token {
            IRQ_TOKEN => on_interrupt(&mut server, &mut region),
            CALL_TOKEN => serve_call(&mut server, endpoint, &mut request, &mut region),
            _ => {}
        }
    }
}

/// Serve one device interrupt: mask the completion sources, acknowledge the
/// device, harvest the rings into the shared region, and wake the stack once
/// if anything moved.
///
/// The mask comes first because the acknowledgement below clears a latch
/// over a still-asserted level condition; leaving the source unmasked across
/// the drain is what makes the driver spin.
fn on_interrupt<N: Net>(server: &mut NetChannelServer<N>, region: &mut Option<Region>) {
    let _ = server.net_mut().set_completion_interrupts(false);
    server.net_mut().ack_interrupt();

    let Some(region) = region.as_mut() else {
        // Detached: there is no region to harvest into and no stack to wake.
        // The sources stay masked until an `Attach` re-enables them, so a
        // device left running cannot storm a driver with nowhere to put
        // frames.
        return;
    };
    let outcome = drain(server, region.bytes);
    // A link change must reach the stack even when no frame moved — an
    // idle interface's cable pull is exactly that case, and a bond failover
    // keys on the report. A masked source must too: the sources are masked
    // and the stack's `Service` is the only thing that can release or
    // diagnose it.
    if outcome.moved || outcome.link_changed || outcome.masked.needs_release() {
        if let Some(notify_endpoint) = server.notify_endpoint() {
            let notify = NetChannelNotify {
                link: outcome.link,
                back_pressure: outcome.masked.needs_release(),
                // The stack reads the pre-filter's count from here: a pure
                // receive rings no doorbell, so this notify is the only
                // report of it the stack will ever see.
                filtered: outcome.filtered,
            };
            let _ = tairix_rt::ipc_send(notify_endpoint, &notify.encode());
        }
    }
}

/// Drive device doorbells over `bytes` under the [`Drain`] policy until it
/// says to stop, performing the mask-register writes it asks for.
///
/// The I/O shell only: which pass unmasks, which re-masks, and what the
/// stack must be told are all decided by the host-tested state machine.
///
/// The caller enters with the completion sources **masked**; the returned
/// [`Drained::masked`] then states whether they were left that way, which is
/// what the notify's back-pressure flag means.
fn drain<N: Net>(server: &mut NetChannelServer<N>, bytes: &mut [u8]) -> Drained {
    let mut policy = Drain::new(server.reported_link(), SERVICE_ROUNDS);
    loop {
        let Ok(report) = server.service(bytes) else {
            // A typed fault (a device error, a corrupt ring) cannot be
            // reported from here, and re-arming into a device that just
            // faulted would storm. So the sources are held masked — but the
            // stack is told, because a masked source with nobody coming to
            // release it is a permanently and *silently* dead interface.
            // Its `Service` carries the reason.
            let _ = server.net_mut().set_completion_interrupts(false);
            policy.fault();
            return policy.outcome();
        };
        match policy.observe(&report) {
            DrainAction::Service => {}
            // Re-arm, then look **once more** before believing the device
            // idle. A completion that landed between the service above and
            // this re-arm raised no interrupt of its own: a source that
            // signals only on a *new* completion (virtio's used-ring event
            // suppression) would then never wake this driver again and the
            // frame would sit there for good. virtio 1.2 §2.7.14 re-checks
            // the used ring after re-enabling notifications for exactly
            // this reason.
            DrainAction::UnmaskAndService => {
                let _ = server.net_mut().set_completion_interrupts(true);
            }
            DrainAction::MaskAndService => {
                let _ = server.net_mut().set_completion_interrupts(false);
            }
            DrainAction::Stop => return policy.outcome(),
        }
    }
}

/// Drain on the doorbell path, then release the completion sources unless
/// the device faulted.
///
/// The reply is the only channel back on this path — `rx_ring_full` in a
/// `ServiceReport` asks for nothing, unlike the notify's back-pressure flag
/// — so a source left masked here has nothing scheduled to lift it and the
/// interface silently stops being interrupt-driven. Releasing is safe and
/// self-healing instead: the stack has just drained its ring, and a source
/// still asserted merely re-interrupts, which *is* the path that carries a
/// release request.
fn drain_and_release<N: Net>(server: &mut NetChannelServer<N>, bytes: &mut [u8]) {
    if drain(server, bytes).masked.may_rearm() {
        let _ = server.net_mut().set_completion_interrupts(true);
    }
}

/// Serve one device-channel doorbell on the claimed endpoint: receive the
/// request, admit its caller, drive the pure [`NetChannelServer`], and reply.
/// A transient recv error simply drops the doorbell (the stack retries); a
/// call the kernel cannot attribute, a frame that does not decode, and a
/// caller [`NetChannelServer::admit`] refuses are answered with the typed
/// refusal, the last recorded.
fn serve_call<N: Net>(
    server: &mut NetChannelServer<N>,
    endpoint: u64,
    request: &mut [u8; NET_CHANNEL_MAX_REQUEST],
    region: &mut Option<Region>,
) {
    let Ok(Some(ServedCall {
        ticket,
        len: request_len,
    })) = tairix_rt::call_recv_ready(endpoint, request)
    else {
        return;
    };
    let caller = match tairix_rt::peer_origin(endpoint, ticket) {
        Ok(caller) => caller,
        Err(err) => return refuse(endpoint, ticket, err),
    };
    let decoded = match NetChannelRequest::decode(&request[..request_len]) {
        Ok(decoded) => decoded,
        Err(err) => return refuse(endpoint, ticket, err),
    };
    if let Err(reason) = server.admit(&decoded, &caller) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        log_event(
            Level::Warn,
            NETCHAN_REFUSED,
            "netchan: a call from outside the attached network stack refused",
            &[
                instance_field("caller", caller.proc_id(), &mut hex),
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                },
            ],
        );
        return refuse(endpoint, ticket, reason);
    }
    match decoded {
        NetChannelRequest::Facts => {
            let reply = server.facts_reply();
            let _ = tairix_rt::call_reply(endpoint, ticket, &reply);
        }
        NetChannelRequest::Attach(params) => {
            let status = attach(server, params, caller.proc_id(), region);
            let _ = tairix_rt::call_reply(endpoint, ticket, &status);
        }
        NetChannelRequest::Service => {
            let reply = match region.as_mut() {
                Some(region) => {
                    let reply = server.service_reply(region.bytes);
                    // The stack calls here after draining its ring, so this
                    // is where a source masked for back-pressure gets its
                    // chance to come back. Masked first because a doorbell
                    // rung for a *transmit* arrives with the sources still
                    // up, and `drain` reports whether it left them down.
                    let _ = server.net_mut().set_completion_interrupts(false);
                    drain_and_release(server, region.bytes);
                    reply
                }
                // Detached (or region lost): the server answers
                // `NotConnected` before it ever touches the slice.
                None => server.service_reply(&mut []),
            };
            let _ = tairix_rt::call_reply(endpoint, ticket, &reply);
        }
        NetChannelRequest::SetMulticast(groups) => {
            let reply = server.set_multicast_reply(&groups);
            let _ = tairix_rt::call_reply(endpoint, ticket, &reply);
        }
        NetChannelRequest::SetRxFilter(policy) => {
            let reply = server.set_rx_filter_reply(policy);
            let _ = tairix_rt::call_reply(endpoint, ticket, &reply);
        }
        NetChannelRequest::Detach => {
            // Mask before releasing the region: a device left running with
            // nowhere to harvest into would otherwise storm this driver.
            let _ = server.net_mut().set_completion_interrupts(false);
            let reply = server.detach();
            if let Some(region) = region.take() {
                let _ = tairix_rt::shm_unmap(region.base, region.len);
            }
            let _ = tairix_rt::call_reply(endpoint, ticket, &reply);
        }
    }
}

/// Answer the call `ticket` on `endpoint` with `reason`.
fn refuse(endpoint: u64, ticket: u64, reason: Errno) {
    let _ = tairix_rt::call_reply(endpoint, ticket, &encode_status_reply(Err(reason)));
}

/// Map the frame region `stack` — the attested caller — granted, validate its
/// length against the agreed geometry, and attach the pure server. An attach
/// replaces any before it, accepted or not, so the old region and attachment
/// go first; on any refusal the new region is unmapped and the channel is
/// left detached (fail closed — a rejected attach never half-binds).
fn attach<N: Net>(
    server: &mut NetChannelServer<N>,
    params: AttachParams,
    stack: ProcId,
    region: &mut Option<Region>,
) -> [u8; STATUS_REPLY_LEN] {
    if let Some(previous) = region.take() {
        // Masked first: a device left running with nowhere to harvest into
        // would storm this driver.
        let _ = server.net_mut().set_completion_interrupts(false);
        let _ = server.detach();
        let _ = tairix_rt::shm_unmap(previous.base, previous.len);
    }
    let mut len_out = 0u64;
    let mapped = tairix_rt::shm_map_from(params.region_grant, stack, &mut len_out);
    if mapped < 0 {
        return encode_status_reply(Err(Errno::from_syscall(mapped)));
    }
    // A non-negative result is the base virtual address of the mapping;
    // `len_out` is the kernel's own record of the mapped byte length.
    // The kernel maps whole pages, so a region whose agreed geometry is
    // not a page multiple is mapped rounded *up* — `map_len` is that
    // actual mapped length (used for the exact `shm_unmap`), which must
    // be at least the geometry needs.
    let (Ok(base), Ok(addr), Ok(map_len)) = (
        u64::try_from(mapped),
        usize::try_from(mapped),
        usize::try_from(len_out),
    ) else {
        return encode_status_reply(Err(Errno::DeviceFault));
    };
    let expected = params.geometry.region_len();
    if map_len < expected {
        let _ = tairix_rt::shm_unmap(base, map_len);
        return encode_status_reply(Err(Errno::BufferTooSmall));
    }
    // SAFETY: `shm_map` mapped `map_len` bytes (>= `expected`, verified
    // above) of zeroed, cacheable, RW (non-executable) memory into this
    // process at `addr`. The ring view binds only the first `expected`
    // bytes — exactly the agreed geometry — so the exclusive `&mut [u8]`
    // over `expected` bytes is a sound subset of the mapping (any
    // page-rounding tail beyond it is left untouched). The region is
    // owned by this process until the matching `shm_unmap` (on detach, a
    // re-attach, or a rejected attach below) releases the full `map_len`,
    // and nothing else in this address space aliases it. The stack maps
    // the same frames through its own grant and never touches ring bytes
    // across a `Service` doorbell.
    let bytes = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, expected) };
    let status = server.attach(params, stack);
    if server.is_attached() {
        *region = Some(Region {
            base,
            len: map_len,
            bytes,
        });
        // There is somewhere to put frames again, so the completion sources
        // — masked whenever the channel is detached — are re-enabled.
        let _ = server.net_mut().set_completion_interrupts(true);
    } else {
        // The server refused (geometry too small for the device); drop the
        // mapping it will never use.
        let _ = tairix_rt::shm_unmap(base, map_len);
    }
    status
}

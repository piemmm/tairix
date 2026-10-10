//! The freestanding driver-process serve loop of the `audiochan-v1` device
//! channel (`plans/SOUND.md` SND4).
//!
//! This is the I/O half of the contract's driver side: everything an audio
//! driver process must do *around* an opened [`Audio`] device to serve the
//! mixer, written once for every such driver rather than copied per device.
//! It claims a reserved device-channel endpoint bound restricted-sender on
//! `CAP_AUDIO_DEVICE`, publishes the [`AUDIOCHAN_NODE_COMPATIBLE`] node so
//! `devmgr` hands the endpoint to `audiod`, and then parks — never
//! busy-polls — on a wait set over the call endpoint and the device's own
//! event sources:
//!
//! * a **call** wake answers one request — `Attach` maps the granted PCM
//!   region, `Service` drives one device doorbell over it, `Detach` unmaps it
//!   — and then reads the device's causes;
//! * an **event** wake — the device's interrupt line, a port a bus driver
//!   reports the device's progress on, or the answer to a call the device
//!   posted to a supplier, a DMA controller's period wait — reads the device's
//!   causes, moves a period for every endpoint whose boundary passed, and
//!   wakes the mixer with one notify carrying the clock pair.
//!
//! Both are the host-tested [`Dispatcher`]; this file is the wait-set and the
//! syscalls behind its [`ChannelIo`].
//!
//! # Why the interrupt path services rather than merely notifying
//!
//! The period interrupt *means* "the device has consumed a period; refill
//! it". The region is already mapped here, so making the mixer ask for the
//! refill with a blocking call would cost two extra process switches per
//! period on the one path in the system whose whole job is not to have any
//! jitter — and would put the refill deadline behind the mixer's scheduling
//! latency instead of the driver's. So the driver moves the period itself
//! and then reports the clock pair; the mixer keeps its linear fit current
//! and writes ahead in its own time. The ring's atomic counters are what
//! make that safe.
//!
//! Compiled only for the bare-metal targets a driver binary is built for.

use tairix_abi::driver::audio::Audio;
use tairix_abi::driver::audio_channel::{
    is_audio_channel_endpoint, AUDIOCHAN_NODE_COMPATIBLE, AUDIO_CHANNEL_ENDPOINT_BASE,
    AUDIO_CHANNEL_MAX_REPLY, AUDIO_CHANNEL_MAX_REQUEST,
};
use tairix_abi::hwtree::HW_NODE_ROOT;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
use tairix_abi::{
    CapabilityId, DriverError, Errno, HwDeviceClass, HwMatchKey, HwNode, HwResource, ProcId,
};
use tairix_caps::CapabilitySet;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::{LogSink, ServedCall};

use crate::exit;
use crate::{ChannelIo, Dispatcher, MappedRegion};

/// Diagnostic event id: the one-shot "device channel published, serving"
/// beacon an audio driver emits once its device is live and its endpoint is
/// bound.
const AUDIOCHAN_READY: EventId = EventId(4210);

/// Diagnostic event id: an audio driver process giving up, carrying the
/// reason it could not serve its device.
const AUDIOCHAN_FAILED: EventId = EventId(4211);

/// Diagnostic event id: an attach the driver refused because the region the
/// mixer granted is smaller than the geometry the two agreed.
const AUDIOCHAN_SHORT_REGION: EventId = EventId(4212);

/// Record why this driver process is ending abnormally, then return `code`
/// for the runtime to exit with.
///
/// An autoloaded driver is detached, so nothing reads its `stderr`; without
/// this an operator sees only the supervisor's exit code, which names the
/// stage that gave up but never what refused it. `detail` carries the typed
/// refusal where the failure had one.
///
/// One definition for every audio driver process, beside the codes it
/// reports, so two drivers cannot describe the same failure differently.
#[must_use]
pub fn fail(code: i32, reason: &'static str, detail: Option<DriverError>) -> i32 {
    let field = detail.map(|err| Field {
        key: "error",
        value: FieldValue::SignedInt(i64::from(err as i32)),
    });
    log(
        &LogSink,
        &Event {
            level: Level::Error,
            id: AUDIOCHAN_FAILED,
            message: reason,
            fields: field.as_slice(),
        },
    );
    code
}

/// Wait-set token for a device-channel call doorbell on the claimed endpoint.
const CALL_TOKEN: u64 = 1;

/// Wait-set token for "one of the device's event sources is ready".
const EVENT_TOKEN: u64 = 2;

/// Something that wakes the serve loop to read the device's causes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Wake {
    /// A bound interrupt line: the handle `irq_bind` answered.
    Irq(u64),
    /// A message port this process bound, which a bus driver reports the
    /// device's progress on. The device engine drains it when it reads its
    /// causes, so the port is quiet again before the loop re-parks.
    Port(u64),
    /// The answer to a call the device posted on this call endpoint, a DMA
    /// controller's period wait. The device engine collects it when it reads
    /// its causes, so the source is quiet again before the loop re-parks.
    CallReply(u64),
}

impl Wake {
    /// The wait-set member this source is.
    const fn member(self) -> (WaitSourceKind, u64) {
        match self {
            Self::Irq(handle) => (WaitSourceKind::Irq, handle),
            Self::Port(port) => (WaitSourceKind::Port, port),
            Self::CallReply(endpoint) => (WaitSourceKind::CallReply, endpoint),
        }
    }
}

/// Outstanding-call capacity of the device-channel endpoint. The mixer issues
/// one control request at a time (it blocks on the reply); a small queue
/// absorbs a doorbell racing the previous reply — a fail-closed memory bound.
const ENDPOINT_CAPACITY: usize = 4;

/// Serve the `audiochan-v1` device channel over the opened device `audio`
/// for the life of the driver process.
///
/// `wakes` are the device's event sources the loop parks on alongside the
/// call endpoint; it needs at least one, or nothing would ever tell it a
/// period passed. Never returns on the success path; every set-up refusal
/// returns a reserved [`exit`](crate::exit) code so the driver ends
/// fail-closed with a diagnosable reason rather than degrading into a busy
/// re-poll.
pub fn serve<A: Audio>(audio: A, wakes: &[Wake]) -> i32 {
    if wakes.is_empty() {
        return fail(
            exit::NO_SERVICE,
            "audiochan: the device names no event source to wait on",
            None,
        );
    }
    let Some(endpoint) = claim_channel_endpoint() else {
        return fail(
            exit::NO_SERVICE,
            "audiochan: no reserved device-channel endpoint could be claimed and bound",
            None,
        );
    };
    if emit_audiochan_node(endpoint).is_none() {
        return fail(
            exit::NO_SERVICE,
            "audiochan: the device-channel node could not be published to the hardware tree",
            None,
        );
    }

    let set = tairix_rt::waitset_create();
    if set < 0 {
        return fail(
            exit::NO_SERVICE,
            "audiochan: the serve wait set could not be created",
            None,
        );
    }
    #[allow(clippy::cast_sign_loss)] // `set >= 0` is the wait-set handle.
    let set = set as u64;
    let joined = tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Endpoint,
        endpoint,
        CALL_TOKEN,
    ) == 0
        && wakes.iter().all(|wake| {
            let (kind, id) = wake.member();
            tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, EVENT_TOKEN) == 0
        });
    if !joined {
        return fail(
            exit::NO_SERVICE,
            "audiochan: the call endpoint and the device's event sources could not be joined into the serve wait set",
            None,
        );
    }

    log(
        &LogSink,
        &Event {
            level: Level::Info,
            id: AUDIOCHAN_READY,
            message: "audiochan: device channel published, serving",
            fields: &[],
        },
    );

    serve_loop(Dispatcher::new(audio, RtChannelIo { endpoint }), set)
}

/// Claim the first free id in the reserved device-channel endpoint block and
/// bind it **restricted-sender requiring `CAP_AUDIO_DEVICE`**: the kernel
/// admits a caller only if it holds that capability, so only the mixer can
/// post to this driver (defence in depth atop the `CAP_IPC_BIND_PRIVILEGED`
/// gate the reserved-id bind already demands). `recv_caps` is empty —
/// endpoint ownership already restricts receive to this task. Returns the
/// claimed id, or [`None`] if the whole block was already taken (every id
/// squatted on — fail closed).
fn claim_channel_endpoint() -> Option<u64> {
    let mut send_caps = CapabilitySet::empty();
    send_caps.insert(CapabilityId::AUDIO_DEVICE);
    let recv_caps = CapabilitySet::empty();
    let mut id = AUDIO_CHANNEL_ENDPOINT_BASE;
    while is_audio_channel_endpoint(id) {
        let bound = tairix_rt::call_create(
            id,
            &send_caps,
            &recv_caps,
            AUDIO_CHANNEL_MAX_REQUEST,
            AUDIO_CHANNEL_MAX_REPLY,
            ENDPOINT_CAPACITY,
        );
        if bound == 0 {
            return Some(id);
        }
        id += 1;
    }
    None
}

/// Publish the `audiochan` hardware-tree node carrying the claimed
/// device-channel endpoint as a grant request, so `devmgr` observes it (a
/// hardware-tree generation bump) and hands the endpoint to the mixer.
/// Returns the kernel-assigned node id, or [`None`] on any refusal.
///
/// The node names [`HW_NODE_ROOT`] as its parent; the kernel re-parents it
/// under the *discovered node this driver was loaded for*, which is what lets
/// `devmgr` recover the device's stable bus location from the published
/// channel.
fn emit_audiochan_node(endpoint: u64) -> Option<u32> {
    let mut node = HwNode::new(0, HW_NODE_ROOT, HwDeviceClass::Audio);
    let key = HwMatchKey::compatible(AUDIOCHAN_NODE_COMPATIBLE).ok()?;
    node.push_match_key(key).ok()?;
    node.push_resource(HwResource::endpoint(endpoint)).ok()?;
    let emit = tairix_rt::hw_emit_node(&node);
    if emit < 0 {
        return None;
    }
    // `emit >= 0` is the kernel-assigned node id.
    u32::try_from(emit).ok()
}

/// Park on the wait set and serve device-channel doorbells and device events
/// for the life of the driver. Never returns on the success path; a wait-set
/// fault exits fail-closed.
fn serve_loop<A: Audio>(mut dispatcher: Dispatcher<A, RtChannelIo>, set: u64) -> i32 {
    loop {
        let mut token = 0u64;
        let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
        if woke < 0 {
            return fail(
                exit::NO_SERVICE,
                "audiochan: the serve wait set faulted; the device is no longer being served",
                None,
            );
        }
        if woke != 0 {
            continue;
        }
        match token {
            EVENT_TOKEN => dispatcher.on_event(),
            CALL_TOKEN => dispatcher.on_call(),
            _ => {}
        }
    }
}

/// The device channel's I/O over the runtime's syscalls.
struct RtChannelIo {
    /// The claimed device-channel endpoint.
    endpoint: u64,
}

/// One mapping of a shared PCM region the mixer granted in `Attach`.
struct RtRegion {
    /// Base address of the mapping.
    base: usize,
    /// Its full length, page-rounded by the kernel, released verbatim by the
    /// matching `shm_unmap`.
    len: usize,
}

impl MappedRegion for RtRegion {
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `shm_map_from` mapped `len` bytes of zeroed, RW,
        // non-executable memory at `base`, and only `ChannelIo::unmap`, which
        // consumes this region, gives it back, so the mapping outlives every
        // view of it. The view's borrow is tied to `&mut self`, so no two views
        // alias, and no other region in this process maps the same grant. The
        // mixer maps the same frames through its own grant; the ring's atomic
        // positions order the two sides' accesses to the samples.
        unsafe {
            core::slice::from_raw_parts_mut(
                core::ptr::with_exposed_provenance_mut::<u8>(self.base),
                self.len,
            )
        }
    }
}

impl ChannelIo for RtChannelIo {
    type Region = RtRegion;

    fn recv(&mut self, request: &mut [u8]) -> Option<(u64, usize)> {
        let ServedCall { ticket, len } =
            tairix_rt::call_recv_ready(self.endpoint, request).ok()??;
        Some((ticket, len))
    }

    fn reply(&mut self, ticket: u64, reply: &[u8]) {
        let _ = tairix_rt::call_reply(self.endpoint, ticket, reply);
    }

    fn caller(&mut self, ticket: u64) -> Result<ProcId, Errno> {
        tairix_rt::peer_origin(self.endpoint, ticket).map(|origin| origin.proc_id())
    }

    fn map(&mut self, grant: u64, owner: ProcId) -> Result<RtRegion, Errno> {
        let mut len_out = 0u64;
        let mapped = tairix_rt::shm_map_from(grant, owner, &mut len_out);
        let base = usize::try_from(mapped).map_err(|_| Errno::from_syscall(mapped))?;
        let len = usize::try_from(len_out).map_err(|_| Errno::DeviceFault)?;
        Ok(RtRegion { base, len })
    }

    fn unmap(&mut self, region: RtRegion) {
        let _ = tairix_rt::shm_unmap(region.base as u64, region.len);
    }

    fn notify(&mut self, port: u64, frame: &[u8]) {
        let _ = tairix_rt::ipc_send(port, frame);
    }

    fn short_region(&mut self, mapped: usize, expected: usize, ring_frames: u32) {
        // Both sizes, because "too small" alone names no defect: the mixer
        // sizes the region and the driver checks it, and which is wrong shows
        // only in the pair.
        log(
            &LogSink,
            &Event {
                level: Level::Error,
                id: AUDIOCHAN_SHORT_REGION,
                message: "audiochan: the granted region is smaller than the agreed ring",
                fields: &[
                    Field {
                        key: "mapped",
                        value: FieldValue::UnsignedInt(mapped as u64),
                    },
                    Field {
                        key: "expected",
                        value: FieldValue::UnsignedInt(expected as u64),
                    },
                    Field {
                        key: "ring_frames",
                        value: FieldValue::UnsignedInt(u64::from(ring_frames)),
                    },
                ],
            },
        );
    }
}

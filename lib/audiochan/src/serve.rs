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
    AUDIOCHAN_NODE_COMPATIBLE, AUDIO_CHANNEL_ENDPOINTS, AUDIO_CHANNEL_MAX_REPLY,
    AUDIO_CHANNEL_MAX_REQUEST,
};
use tairix_abi::driver::DeviceWake;
use tairix_abi::hwtree::HW_NODE_ROOT;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
use tairix_abi::{CapabilityId, Errno, HwDeviceClass, HwMatchKey, HwNode, HwResource, ProcId};
use tairix_caps::CapabilitySet;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::{LogSink, ServedCall};

use crate::{ChannelIo, Dispatcher, MappedRegion};
use tairix_drvrt::exit::{end, step, DriverExit};

/// Diagnostic event id: the one-shot "device channel published, serving"
/// beacon an audio driver emits once its device is live and its endpoint is
/// bound.
const AUDIOCHAN_READY: EventId = EventId(29_010);

/// Diagnostic event id: an attach the driver refused because the region the
/// mixer granted is smaller than the geometry the two agreed.
const AUDIOCHAN_SHORT_REGION: EventId = EventId(29_012);

/// Wait-set token for a device-channel call doorbell on the claimed endpoint.
const CALL_TOKEN: u64 = 1;

/// Wait-set token for "one of the device's event sources is ready".
const EVENT_TOKEN: u64 = 2;

/// Outstanding-call capacity of the device-channel endpoint. The mixer issues
/// one control request at a time (it blocks on the reply); a small queue
/// absorbs a doorbell racing the previous reply — a fail-closed memory bound.
const ENDPOINT_CAPACITY: usize = 4;

/// A device channel claimed, published and served: the endpoint the mixer
/// calls and the dispatcher its wakes go to, driven by [`serve`]'s loop or by
/// a driver whose loop serves another protocol of the device beside it.
pub struct Channel<A: Audio> {
    dispatcher: Dispatcher<A, RtChannelIo>,
    endpoint: u64,
}

impl<A: Audio> Channel<A> {
    /// Claim a device-channel endpoint, publish the node carrying it, and
    /// serve `audio` on it.
    ///
    /// # Errors
    ///
    /// The exit status to end with, its reason recorded.
    pub fn publish(audio: A) -> Result<Self, i32> {
        let Some(endpoint) = bind_channel_endpoint() else {
            return Err(end(
                DriverExit::NoService,
                &[step(
                    "audiochan: the node's device-channel endpoint could not be bound",
                )],
            ));
        };
        if emit_audiochan_node(endpoint).is_none() {
            return Err(end(DriverExit::NoService, &[step("audiochan: the device-channel node could not be published to the hardware tree")]));
        }
        // The channel is the one child this driver publishes. A refused
        // settle only leaves the device manager to wait out its bound.
        let _ = tairix_rt::hw_node_settle();
        log(
            &LogSink,
            &Event {
                level: Level::Info,
                id: AUDIOCHAN_READY,
                message: "audiochan: device channel published, serving",
                fields: &[],
            },
        );
        Ok(Self {
            dispatcher: Dispatcher::new(audio, RtChannelIo { endpoint }),
            endpoint,
        })
    }

    /// The endpoint the mixer calls.
    #[must_use]
    pub const fn endpoint(&self) -> u64 {
        self.endpoint
    }

    /// The endpoint woke: answer the call waiting.
    pub fn on_call(&mut self) {
        self.dispatcher.on_call();
    }

    /// One of the device's event sources woke, or something else the device
    /// does may have changed what it reports: read its causes.
    pub fn on_event(&mut self) {
        self.dispatcher.on_event();
    }
}

/// Serve the `audiochan-v1` device channel over the opened device `audio`
/// for the life of the driver process.
///
/// `wakes` are the device's event sources the loop parks on alongside the
/// call endpoint; it needs at least one, or nothing would ever tell it a
/// period passed. Never returns on the success path; every set-up refusal
/// ends the driver fail-closed with its reason recorded rather than degrading
/// into a busy re-poll.
pub fn serve<A: Audio>(audio: A, wakes: &[DeviceWake]) -> i32 {
    if wakes.is_empty() {
        return end(
            DriverExit::NoService,
            &[step(
                "audiochan: the device names no event source to wait on",
            )],
        );
    }
    let channel = match Channel::publish(audio) {
        Ok(channel) => channel,
        Err(code) => return code,
    };
    let set = tairix_rt::waitset_create();
    if set < 0 {
        return end(
            DriverExit::NoService,
            &[step("audiochan: the serve wait set could not be created")],
        );
    }
    #[allow(clippy::cast_sign_loss)] // `set >= 0` is the wait-set handle.
    let set = set as u64;
    let joined = tairix_rt::waitset_ctl(
        set,
        WaitSetOp::Add,
        WaitSourceKind::Endpoint,
        channel.endpoint(),
        CALL_TOKEN,
    ) == 0
        && wakes.iter().all(|wake| {
            let (kind, id) = wake.member();
            tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, EVENT_TOKEN) == 0
        });
    if !joined {
        return end(DriverExit::NoService, &[step("audiochan: the call endpoint and the device's event sources could not be joined into the serve wait set")]);
    }
    serve_loop(channel, set)
}

/// Bind the device-channel endpoint of the node this driver was loaded for,
/// **restricted-sender requiring `CAP_AUDIO_DEVICE`**, so only the mixer can
/// post to it. `recv_caps` is empty — endpoint ownership already restricts
/// receive to this task. [`None`] on any refusal (fail closed).
fn bind_channel_endpoint() -> Option<u64> {
    let id = AUDIO_CHANNEL_ENDPOINTS.endpoint(tairix_rt::hw_self_node().ok()?.node());
    let mut send_caps = CapabilitySet::empty();
    send_caps.insert(CapabilityId::AUDIO_DEVICE);
    let bound = tairix_rt::call_create(
        id,
        &send_caps,
        &CapabilitySet::empty(),
        AUDIO_CHANNEL_MAX_REQUEST,
        AUDIO_CHANNEL_MAX_REPLY,
        ENDPOINT_CAPACITY,
    );
    (bound == 0).then_some(id)
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
fn serve_loop<A: Audio>(mut channel: Channel<A>, set: u64) -> i32 {
    loop {
        let mut token = 0u64;
        let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
        if woke < 0 {
            return end(
                DriverExit::NoService,
                &[step(
                    "audiochan: the serve wait set faulted; the device is no longer being served",
                )],
            );
        }
        if woke != 0 {
            continue;
        }
        match token {
            EVENT_TOKEN => channel.on_event(),
            CALL_TOKEN => channel.on_call(),
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

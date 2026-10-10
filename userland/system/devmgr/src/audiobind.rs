//! Handing a discovered sound device's channel to the audio service.
//!
//! An audio driver process, once the device manager has autoloaded it for a
//! matched audio node, brings its device online and publishes a child
//! *device-channel* hardware-tree node: `compatible = "tairix,audiochan"`,
//! carrying the reserved call-endpoint id it bound as an
//! [`HwResourceKind::Endpoint`] grant request. Emitting that node bumps the
//! hardware-tree generation, waking the device manager's reactive loop.
//!
//! This module is the pure policy for that reaction, the audio twin of
//! [`netbind`](crate::netbind): recognise an `audiochan` node, read its
//! endpoint, and — for each channel not already handed over — ask the audio
//! service to adopt it, over the [`AudiodBind`] seam so the loop stays
//! host-testable. The service becomes the channel's client, enumerates the
//! device's sinks and sources, and owns every shared PCM region from there;
//! the device manager only names *which* endpoint.
//!
//! Each endpoint is handed over exactly once (tracked in [`AudioBindState`]):
//! the node persists across every later generation bump while the driver
//! lives, so a re-bind would provision a duplicate device. A hand-off that
//! fails (the service is not up yet, or refuses) is fail-soft — logged and
//! retried at the next reaction, exactly like an unavailable driver store — never
//! fatal to the observe loop.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::hash::Hasher;

use tairix_abi::audio::AudioBaseline;
use tairix_abi::driver::audio_channel::AUDIOCHAN_NODE_COMPATIBLE;
use tairix_abi::hwtree::{HwMatchKind, HwResourceKind, HW_NODE_ROOT};
use tairix_abi::{Errno, HwNode};
use tairix_hash::{HashSeed, SipHash13};
use tairix_log::{log as log_event, Event, EventId, Field, FieldValue, Level, Sink};

use crate::events;

/// The device manager's call into the audio service to adopt one audio
/// driver's device channel.
///
/// The production implementation (the freestanding `devmgr` `Run` binary)
/// backs this with an `ipc_call` to the reserved
/// [`AUDIO_ENDPOINT`](tairix_abi::audio::AUDIO_ENDPOINT) carrying an
/// [`AudioRequest::BindDriver`](tairix_abi::audio::AudioRequest::BindDriver);
/// the audio service checks the call against the device manager's attested
/// `CAP_DRV_LOAD`, so the seam adds no authority. It is abstracted here so
/// the reactive loop is host-testable against a recording double.
pub trait AudiodBind {
    /// Ask the audio service to adopt the driver's device-channel
    /// `endpoint_id` as a sound device at `location`.
    ///
    /// # Errors
    ///
    /// The service's typed refusal, or a transport failure — treated
    /// fail-soft by the caller (retried on the next generation bump).
    fn bind_driver(&mut self, endpoint_id: u64, location: u64) -> Result<(), Errno>;

    /// Tell the audio service the channel `endpoint_id`'s node has left the
    /// hardware tree.
    ///
    /// # Errors
    ///
    /// The service's typed refusal — [`Errno::NotFound`] for a device it
    /// already let go — or a transport failure.
    fn unbind_driver(&mut self, endpoint_id: u64) -> Result<(), Errno>;

    /// Hand the audio service the machine's baseline.
    ///
    /// # Errors
    ///
    /// The service's typed refusal, or a transport failure.
    fn deliver_baseline(&mut self, baseline: AudioBaseline) -> Result<(), Errno>;
}

/// The device manager's read of the machine's audio baseline from the
/// system-configuration store.
///
/// Returns [`None`] while the store cannot be read, which leaves the audio
/// service on the baseline nobody configured and is retried on the next
/// reaction.
pub trait AudioBaselineSource {
    /// Load the current baseline.
    fn load(&mut self) -> Option<AudioBaseline>;
}

/// The key a location is hashed under. A location is a name for a place,
/// not a secret, so the key is fixed and the same place answers the same
/// location on every boot.
const LOCATION_KEY: HashSeed = HashSeed::from_words(0x7461_6972_6978_2e61, 0x7564_696f_2e6c_6f63);

/// Where the device behind the channel node `channel` sits in the hardware
/// tree: the device half of each of its endpoints' locations.
///
/// The device is the channel node's parent — the kernel re-parents a
/// driver's emission under the node it was loaded for — and its place is the
/// chain from it to the root, each step spelled by the node's class, its
/// bus-local address, its first register window, and its rank among the
/// siblings that share all three. Node ids are not part of it: they are
/// handed out in discovery order, and a place must not move because
/// something else was found first.
///
/// [`None`] for a channel with no parent in `nodes`. Never zero.
#[must_use]
pub fn device_location(nodes: &[HwNode], channel: &HwNode) -> Option<u64> {
    let mut at = find(nodes, channel.parent())?;
    let mut hasher = SipHash13::new(LOCATION_KEY);
    // Bounded by the tree, so a malformed cycle of parents ends.
    for _ in 0..nodes.len() {
        let rank = nodes
            .iter()
            .filter(|sibling| sibling.parent() == at.parent() && place(sibling) == place(at))
            .take_while(|sibling| sibling.id() != at.id())
            .count();
        let (class, address, window) = place(at);
        hasher.write_u16(class);
        hasher.write_u32(address);
        hasher.write_u64(window);
        hasher.write_usize(rank);
        if at.parent() == HW_NODE_ROOT {
            return Some(hasher.finish().max(1));
        }
        at = find(nodes, at.parent())?;
    }
    None
}

/// The node `id` names in `nodes`.
fn find(nodes: &[HwNode], id: u32) -> Option<&HwNode> {
    nodes.iter().find(|node| node.id() == id)
}

/// What a node's place is spelled by: its class, its bus-local address, and
/// the base of its first register window.
fn place(node: &HwNode) -> (u16, u32, u64) {
    let class = node
        .class()
        .map_or(u16::MAX, tairix_abi::HwDeviceClass::as_u16);
    let window = node
        .resources()
        .iter()
        .find(|resource| resource.kind() == Some(HwResourceKind::Mmio))
        .map_or(0, tairix_abi::HwResource::base);
    (class, node.address(), window)
}

/// The device manager's memory of which audio channels it has already handed
/// to the audio service.
///
/// An `audiochan` node persists across every generation bump for as long as
/// its driver lives, so binding is idempotent: an endpoint already handed
/// over is skipped.
#[derive(Default)]
pub struct AudioBindState {
    bound: BTreeSet<u64>,
    deferred: bool,
    delivered: Option<AudioBaseline>,
    baseline_deferred: bool,
}

impl AudioBindState {
    /// A fresh state with nothing bound.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the channel `endpoint_id` has already been handed over.
    #[must_use]
    pub fn is_bound(&self, endpoint_id: u64) -> bool {
        self.bound.contains(&endpoint_id)
    }

    /// Whether the last pass left a discovered channel unbound or retired
    /// unreported, or a readable baseline undelivered.
    ///
    /// The audio service becoming reachable is not a hardware-tree mutation,
    /// so a caller that parks for one would never retry the hand-off.
    #[must_use]
    pub fn has_deferred_work(&self) -> bool {
        self.deferred || self.baseline_deferred
    }
}

/// Deliver the machine's audio baseline whenever it differs from the one the
/// service last accepted, so an administrator's edit on the writable root
/// reaches it once that volume is mounted.
pub fn deliver_audio_baseline(
    source: &mut dyn AudioBaselineSource,
    state: &mut AudioBindState,
    audiod: &mut dyn AudiodBind,
    sink: &dyn Sink,
) {
    state.baseline_deferred = false;
    let Some(baseline) = source.load() else {
        return;
    };
    if state.delivered == Some(baseline) {
        return;
    }
    match audiod.deliver_baseline(baseline) {
        Ok(()) => {
            state.delivered = Some(baseline);
            log_event(
                sink,
                &Event {
                    level: Level::Info,
                    id: events::AUDIO_BASELINE_DELIVERED,
                    message: "audio baseline delivered to the audio service",
                    fields: &[],
                },
            );
        }
        Err(err) => {
            state.baseline_deferred = true;
            log_event(
                sink,
                &Event {
                    level: Level::Warn,
                    id: events::AUDIO_BASELINE_DELIVERY_FAILED,
                    message: "audio baseline delivery to the audio service failed; will retry",
                    fields: &[Field {
                        key: "error",
                        value: FieldValue::Error(err),
                    }],
                },
            );
        }
    }
}

/// If `node` is an audio device-channel node — its match keys carry the
/// [`AUDIOCHAN_NODE_COMPATIBLE`] `compatible` string — return the
/// call-endpoint id it published as an [`HwResourceKind::Endpoint`] grant
/// request.
///
/// Returns [`None`] for any other node, and for an `audiochan` node that
/// carries no endpoint resource (a malformed emission — never guessed at).
#[must_use]
pub fn audiochan_endpoint(node: &HwNode) -> Option<u64> {
    let is_audiochan = node.match_keys().iter().any(|key| {
        key.kind() == Some(HwMatchKind::Compatible)
            && key.compatible_bytes() == AUDIOCHAN_NODE_COMPATIBLE
    });
    if !is_audiochan {
        return None;
    }
    node.resources()
        .iter()
        .find(|resource| resource.kind() == Some(HwResourceKind::Endpoint))
        .map(tairix_abi::HwResource::base)
}

/// Hand every not-yet-adopted audio device channel in `nodes` to the audio
/// service through `audiod`, recording each success in `state`.
///
/// An endpoint already in `state` is skipped (idempotent across generation
/// bumps). A hand-off the service refuses is fail-soft: logged and recorded
/// on the state as deferred work, never fatal to the observe loop. The caller
/// retries it under a bounded deadline — the service claiming its rendezvous
/// bumps no generation, so a caller waiting only for one would leave the
/// sound card unattached for the life of the boot.
pub fn bind_new_channels(
    nodes: &[HwNode],
    state: &mut AudioBindState,
    audiod: &mut dyn AudiodBind,
    sink: &dyn Sink,
) {
    state.deferred = false;
    retire_vanished(nodes, state, audiod, sink);
    for node in nodes {
        let Some(endpoint) = audiochan_endpoint(node) else {
            continue;
        };
        if state.bound.contains(&endpoint) {
            continue;
        }
        let Some(location) = device_location(nodes, node) else {
            // A channel the tree does not place has no device behind it to
            // name; a later generation may complete it.
            state.deferred = true;
            continue;
        };
        match audiod.bind_driver(endpoint, location) {
            Ok(()) => {
                state.bound.insert(endpoint);
                audit(
                    sink,
                    events::AUDIOD_BOUND,
                    Level::Info,
                    "audiochan device channel bound to audio service",
                    endpoint,
                    None,
                );
            }
            Err(err) => {
                state.deferred = true;
                audit(
                    sink,
                    events::AUDIOD_BIND_FAILED,
                    Level::Warn,
                    "audiochan device-channel bind to audio service failed; will retry",
                    endpoint,
                    Some(err),
                );
            }
        }
    }
}

/// Tell the audio service of every channel handed over whose node is no
/// longer in `nodes`, and forget it, so a replugged device reusing the
/// endpoint is handed over again.
fn retire_vanished(
    nodes: &[HwNode],
    state: &mut AudioBindState,
    audiod: &mut dyn AudiodBind,
    sink: &dyn Sink,
) {
    let present: BTreeSet<u64> = nodes.iter().filter_map(audiochan_endpoint).collect();
    let gone: Vec<u64> = state
        .bound
        .iter()
        .copied()
        .filter(|endpoint| !present.contains(endpoint))
        .collect();
    for endpoint in gone {
        match audiod.unbind_driver(endpoint) {
            // A device the service already let go needs no word from here.
            Ok(()) | Err(Errno::NotFound) => {
                state.bound.remove(&endpoint);
                audit(
                    sink,
                    events::AUDIOD_UNBOUND,
                    Level::Info,
                    "audiochan device channel left the tree; retired from the audio service",
                    endpoint,
                    None,
                );
            }
            Err(err) => {
                state.deferred = true;
                audit(
                    sink,
                    events::AUDIOD_BIND_FAILED,
                    Level::Warn,
                    "audiochan device-channel retirement failed; will retry",
                    endpoint,
                    Some(err),
                );
            }
        }
    }
}

/// Emit one audit record carrying the channel endpoint the decision was
/// about, so an operator can correlate it with the driver that published it,
/// and — on a refusal — what the service actually said, without which the
/// record names a failure but not its cause.
fn audit(
    sink: &dyn Sink,
    id: EventId,
    level: Level,
    message: &'static str,
    endpoint: u64,
    error: Option<Errno>,
) {
    let endpoint = Field {
        key: "endpoint",
        value: FieldValue::UnsignedInt(endpoint),
    };
    let fields = match error {
        Some(err) => &[
            endpoint,
            Field {
                key: "error",
                value: FieldValue::Error(err),
            },
        ][..],
        None => &[endpoint][..],
    };
    log_event(
        sink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

#[cfg(test)]
#[path = "audiobind_tests.rs"]
mod tests;

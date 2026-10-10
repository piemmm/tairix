//! The `audio-v1` server: devices, streams, and every decision the service
//! takes on a client's behalf (`plans/SOUND.md`).
//!
//! Pure over injected seams — the region host, the client notifier and the
//! monotonic clock every period pair is stamped against, which it owns, and
//! the device-channel transport each bound driver is reached through and the
//! audit sink, which it is handed — so the whole authority is exercised on a
//! host with no machine attached.
//!
//! # Authority
//!
//! Playback needs no capability: the authorisation is that the caller's
//! login session holds the seat whose room the device serves, decided by the
//! one routing policy from the kernel-attested origin — at open, and again for
//! every live stream each time the seat's lease moves. Opening a *source*
//! additionally demands `CAP_AUDIO_CAPTURE`, read from the caller's attested
//! capability summary and never from anything the caller said. A stream id is
//! a service-issued token checked against the attested pid, so a guessed id
//! reaches nothing.
//!
//! A device's controls are the room's: a change is admitted from a caller
//! whose session holds the room, from anyone while it is unclaimed, and from
//! nobody while it is withheld, and each tenant's are kept apart
//! ([`crate::controls`]). Binding and retiring a device, and the machine's
//! baseline, are the device manager's, on its attested `CAP_DRV_LOAD`; the
//! stream listing is the System Information service's, on its attested
//! `CAP_SYSINFO_INTROSPECT`.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::appinfo::BundleId;
use tairix_abi::audio::{
    encode_clock_reply, encode_enumerate_reply, encode_open_reply, encode_state_reply,
    encode_streams_reply, notify_endpoint_for, AudioBaseline, AudioDeviceDescriptor, AudioGain,
    AudioNotify, AudioRequest, ControlAccess, DefaultChoice, OpenParams, StreamDescriptor,
    StreamGrant, StreamReport, StreamState, AUDIO_MAX_REPLY, MAX_CLIENT_STREAM_SLOTS,
};
use tairix_abi::driver::audio::{ring_bounds, Frames, StreamDirection, MAX_DEVICE_ENDPOINTS};
use tairix_abi::driver::audio_channel::AudioChannelNotify;
use tairix_abi::driver::audio_ring::PcmGeometry;
use tairix_abi::origin::Origin;
use tairix_abi::reply::encode_status_reply;
use tairix_abi::seat::DisplayLease;
use tairix_abi::time::MonotonicClock;
use tairix_abi::{CapabilityId, Errno};
use tairix_audio::route::{self, Admission, Roles, Room, Routing, SinkState, StreamRequest};
use tairix_audio::volume::{endpoint_level, stream_multiply, EndpointLevel, VolumeRequest};
use tairix_log::{log, Event, Field, FieldValue, Level, Sink};
use tairix_util::fallible;

use crate::channel::AudioChannelTransport;
use crate::controls::{direction_slot, Controls, Tenant};
use crate::device::{self, Device, Stream, StreamOpen};
use crate::events;
use crate::region::RegionHost;

/// The client wake-up transport.
///
/// One `ipc_send` to a stream's notify mailbox: the live service backs it
/// with `tairix_rt::ipc_send` and host tests with a recording double. A wake
/// that does not land costs a late refill, never correctness, so it has no
/// result.
pub trait Notifier {
    /// Deliver `frame` to `endpoint`, best effort.
    fn notify(&mut self, endpoint: u64, frame: &[u8]);
}

/// The one mixer, router and audio authority.
pub struct AudioService<H: RegionHost, N: Notifier, C: MonotonicClock> {
    regions: H,
    notifier: N,
    clock: C,
    /// Bound devices by slot; a reaped device leaves its slot vacant for the
    /// next bind, so a slot's notify port and wake token stay its own.
    devices: Vec<Option<Device>>,
    streams: Vec<Stream>,
    next_device_id: u32,
    next_stream_id: u64,
    /// Each direction's default endpoint, playback first.
    defaults: [Option<u32>; 2],
    /// Every tenant's controls, over the machine's baseline.
    controls: Controls,
    /// Changes to the devices or their controls so far: what the
    /// `AudioDevices` notice carries.
    changes: u64,
    /// Whose room the seat's devices serve.
    room: Room,
}

impl<H: RegionHost, N: Notifier, C: MonotonicClock> AudioService<H, N, C> {
    /// A service with no device bound and no stream open, whose room is
    /// nobody's until the seat's lease is first read — fail closed.
    pub fn new(regions: H, notifier: N, clock: C) -> Self {
        Self {
            regions,
            notifier,
            clock,
            devices: Vec::new(),
            streams: Vec::new(),
            next_device_id: 1,
            next_stream_id: 1,
            defaults: [None; 2],
            controls: Controls::new(),
            changes: 0,
            room: Room::Withheld,
        }
    }

    /// Follow the seat's display lease: hold every stream outside the room it
    /// now describes at its frame position, release every stream back inside
    /// it, and bring each endpoint's clock in line with what is left live.
    pub fn seat_changed(&mut self, lease: DisplayLease, sink: &dyn Sink) {
        let room = Room::from(lease);
        if self.room == room {
            return;
        }
        // What a source captured before the room moved is the old room's, so
        // it goes to the streams the old room admitted.
        self.harvest_sources(sink);
        let previous = core::mem::replace(&mut self.room, room);
        for index in 0..self.streams.len() {
            self.readmit(index, previous);
        }
        // The new tenant's controls are in force before any stream it holds
        // moves a frame, so nothing resumes at another tenant's level.
        self.forget_tenancies();
        self.apply_all_levels(sink);
        self.refresh_defaults(sink);
        self.changed();
        self.settle_endpoints(sink);
        let held = self.streams.iter().filter(|stream| stream.held).count();
        audit(
            sink,
            events::ROOM_CHANGED,
            Level::Info,
            "the seat's room moved; audio follows it",
            &[
                Field {
                    key: "room",
                    value: FieldValue::Str(match room {
                        Room::Unclaimed => "unclaimed",
                        Room::Session(_) => "session",
                        Room::Withheld => "withheld",
                    }),
                },
                Field {
                    key: "held",
                    value: FieldValue::UnsignedInt(u64::try_from(held).unwrap_or(u64::MAX)),
                },
            ],
        );
    }

    /// Capture streams moving frames: what the recording indicator shows.
    #[must_use]
    pub fn live_captures(&self) -> u32 {
        let live = self
            .streams
            .iter()
            .filter(|stream| stream.direction == StreamDirection::Capture && stream.is_live())
            .count();
        u32::try_from(live).unwrap_or(u32::MAX)
    }

    /// What the room does with the `index`th stream.
    fn admission(&self, index: usize, room: Room) -> Admission {
        let stream = &self.streams[index];
        route::admit(stream.direction, stream.role, stream.session, room)
    }

    /// Hold or release the `index`th stream for the room, which was
    /// `previous`.
    fn readmit(&mut self, index: usize, previous: Room) {
        let now = self.admission(index, self.room);
        // A notification outside the room queues nothing: what it held when
        // the room left it, and whatever it wrote while outside, is dropped
        // rather than played late.
        if now == Admission::Drop || self.admission(index, previous) == Admission::Drop {
            self.drop_queued(index);
        }
        let Self {
            streams, notifier, ..
        } = self;
        let at = streams[index].position;
        streams[index].set_held(now != Admission::Mix, at, notifier);
    }

    /// Discard everything the `index`th stream has queued, filtered or not;
    /// a drain it had begun is thereby over.
    ///
    /// A ring that cannot be read here is the pump's to fault when the stream
    /// next moves, so it is left as it is.
    fn drop_queued(&mut self, index: usize) {
        let Ok(queued) = self.queued_frames(index) else {
            return;
        };
        if self.discard(index, u64::from(queued)).is_err() {
            return;
        }
        self.streams[index].reset_filter();
        if self.streams[index].is_draining() {
            let Self {
                streams, notifier, ..
            } = self;
            let at = streams[index].position;
            streams[index].set_state(StreamState::Idle, at, notifier);
        }
    }

    /// Bring every endpoint carrying streams in line with what is live on it:
    /// its gains rebalanced, clocking for the live ones, idle where none is
    /// left.
    fn settle_endpoints(&mut self, sink: &dyn Sink) {
        for device_index in 0..self.devices.len() {
            for slot in 0..self.endpoint_count(device_index) {
                if !self
                    .streams
                    .iter()
                    .any(|stream| stream.on(device_index, slot))
                {
                    continue;
                }
                if self.any_live(device_index, slot) {
                    self.resolve_gains(device_index, slot);
                    self.keep_running(device_index, slot, sink);
                } else {
                    self.quiesce_endpoint(device_index, slot, sink);
                }
            }
        }
    }

    /// Take what every clocking source has captured, for the streams it
    /// serves now.
    fn harvest_sources(&mut self, sink: &dyn Sink) {
        for device_index in 0..self.devices.len() {
            for slot in 0..self.endpoint_count(device_index) {
                let clocking = self.devices[device_index]
                    .as_ref()
                    .and_then(|device| device.endpoints.get(slot))
                    .filter(|endpoint| endpoint.facts.direction == StreamDirection::Capture)
                    .and_then(|endpoint| endpoint.active.as_ref())
                    .is_some_and(|active| active.running);
                if clocking {
                    self.pump(device_index, slot, sink);
                }
            }
        }
    }

    /// Whether any stream on `(device_index, slot)` is moving frames.
    fn any_live(&self, device_index: usize, slot: usize) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.on(device_index, slot) && stream.is_live())
    }

    /// Run the endpoint for the streams live on it. A device that will not
    /// run is lost, rather than left holding streams that believe they play.
    fn keep_running(&mut self, device_index: usize, slot: usize, sink: &dyn Sink) {
        if let Err(err) = self.run_endpoint(device_index, slot, sink) {
            self.lose_device(device_index, err, sink);
        }
    }

    /// Bind one driver's discovered device channel, at `location` in the
    /// hardware tree, into [`bind_slot`](Self::bind_slot), and enumerate what
    /// it presents.
    ///
    /// Idempotent in the endpoint id while the device is there: a repeated
    /// hand-off of a channel already bound is a no-op rather than a duplicate
    /// device, because the hardware-tree node persists for as long as the
    /// driver lives and the device manager re-offers it on every generation
    /// bump. A lost device's endpoint is a new driver's to take.
    ///
    /// # Errors
    ///
    /// The driver's typed refusal, or [`Errno::OutOfMemory`].
    pub fn bind_device(
        &mut self,
        channel_endpoint: u64,
        location: u64,
        notify_endpoint: u64,
        transport: Box<dyn AudioChannelTransport>,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        if self
            .present()
            .any(|(_, device)| device.channel_endpoint == channel_endpoint && !device.lost)
        {
            return Ok(());
        }
        let device = match Device::bind(
            channel_endpoint,
            location,
            notify_endpoint,
            transport,
            self.next_device_id,
        ) {
            Ok(device) => device,
            Err(err) => {
                audit(
                    sink,
                    events::DEVICE_BIND_FAILED,
                    Level::Warn,
                    "audio device channel could not be bound",
                    &[
                        Field {
                            key: "endpoint",
                            value: FieldValue::UnsignedInt(channel_endpoint),
                        },
                        Field {
                            key: "error",
                            value: FieldValue::Error(err),
                        },
                    ],
                );
                return Err(err);
            }
        };
        let endpoints = u32::try_from(device.endpoints.len()).unwrap_or(0);
        let slot = self.bind_slot();
        if slot == self.devices.len() {
            if !fallible::reserve(&mut self.devices, 1) {
                return Err(Errno::OutOfMemory);
            }
            self.devices.push(Some(device));
        } else {
            self.devices[slot] = Some(device);
        }
        self.next_device_id = self.next_device_id.saturating_add(endpoints);
        for endpoint in 0..self.endpoint_count(slot) {
            self.apply_level(slot, endpoint, sink);
        }
        self.refresh_defaults(sink);
        self.changed();
        audit(
            sink,
            events::DEVICE_BOUND,
            Level::Info,
            "audio device channel bound",
            &[
                Field {
                    key: "endpoint",
                    value: FieldValue::UnsignedInt(channel_endpoint),
                },
                Field {
                    key: "endpoints",
                    value: FieldValue::UnsignedInt(u64::from(endpoints)),
                },
            ],
        );
        Ok(())
    }

    /// The slot the next [`bind_device`](Self::bind_device) fills: the first
    /// a reaped device left vacant, else a new one at the end.
    #[must_use]
    pub fn bind_slot(&self) -> usize {
        self.devices
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.devices.len())
    }

    /// Devices bound and not yet reaped.
    #[must_use]
    pub fn device_count(&self) -> usize {
        self.present().count()
    }

    /// The notify port the device in slot `device` wakes this service on.
    #[must_use]
    pub fn device_notify_endpoint(&self, device: usize) -> Option<u64> {
        self.devices
            .get(device)
            .and_then(Option::as_ref)
            .map(|device| device.notify_endpoint)
    }

    /// Streams open.
    #[must_use]
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Changes to the devices or their controls so far, for the
    /// `AudioDevices` notice.
    #[must_use]
    pub const fn changes(&self) -> u64 {
        self.changes
    }

    /// Every bound device, with its slot.
    fn present(&self) -> impl Iterator<Item = (usize, &Device)> {
        self.devices
            .iter()
            .enumerate()
            .filter_map(|(slot, device)| device.as_ref().map(|device| (slot, device)))
    }

    /// Every endpoint of a device that is still there, in bind order within
    /// each device.
    fn live_endpoints(&self) -> impl Iterator<Item = &device::Endpoint> {
        self.present()
            .filter(|(_, device)| !device.lost)
            .flat_map(|(_, device)| device.endpoints.iter())
    }

    /// How many endpoints the device in slot `device` has; none for a
    /// vacant slot.
    fn endpoint_count(&self, device: usize) -> usize {
        self.devices
            .get(device)
            .and_then(Option::as_ref)
            .map_or(0, |device| device.endpoints.len())
    }

    /// Record that the devices or their controls moved.
    fn changed(&mut self) {
        self.changes = self.changes.wrapping_add(1);
    }

    /// Choose each direction's default again: the live endpoint the room's
    /// tenant prefers, else the one the machine prefers, else the first bound.
    /// Recorded when it moves, because "which device does audio come out of"
    /// is a decision a user is entitled to see.
    fn refresh_defaults(&mut self, sink: &dyn Sink) {
        for direction in [StreamDirection::Playback, StreamDirection::Capture] {
            let chosen = self.choose_default(direction);
            let held = &mut self.defaults[direction_slot(direction)];
            if *held == chosen {
                continue;
            }
            *held = chosen;
            self.changed();
            audit(
                sink,
                events::DEFAULT_DEVICE_CHANGED,
                Level::Info,
                "default audio device chosen",
                &[
                    Field {
                        key: "device",
                        value: FieldValue::UnsignedInt(u64::from(chosen.unwrap_or(0))),
                    },
                    Field {
                        key: "direction",
                        value: FieldValue::Str(direction_name(direction)),
                    },
                ],
            );
        }
    }

    /// The endpoint `direction`'s default lands on now.
    fn choose_default(&self, direction: StreamDirection) -> Option<u32> {
        let candidates = || {
            self.live_endpoints()
                .filter(move |endpoint| endpoint.facts.direction == direction)
        };
        let tenant = Tenant::of(self.room);
        for preference in self
            .controls
            .preferences(tenant, direction)
            .into_iter()
            .flatten()
        {
            if let Some(found) = candidates().find(|endpoint| endpoint.location == preference) {
                return Some(found.device_id);
            }
        }
        // Device ids rise with each bind, so the least is the first bound.
        candidates().map(|endpoint| endpoint.device_id).min()
    }

    /// Serve one `audio-v1` request from `caller`, writing the reply into
    /// `reply` and answering its length.
    ///
    /// Total: an undecodable frame, an unknown operation, a stream that is
    /// not the caller's, or any refusal below is a fully-encoded reply
    /// carrying a typed error. Never a panic.
    pub fn handle(
        &mut self,
        caller: &Origin,
        request: &[u8],
        reply: &mut [u8; AUDIO_MAX_REPLY],
        sink: &dyn Sink,
    ) -> usize {
        let decoded = match AudioRequest::decode(request) {
            Ok(decoded) => decoded,
            Err(err) => return put(reply, &encode_status_reply(Err(err))),
        };
        match decoded {
            AudioRequest::Enumerate { direction, after } => put(
                reply,
                &encode_enumerate_reply(self.enumerate(caller, direction, after)),
            ),
            AudioRequest::Open(params) => {
                put(reply, &encode_open_reply(self.open(caller, &params, sink)))
            }
            AudioRequest::Attach {
                stream_id,
                region_grant,
            } => put(
                reply,
                &encode_status_reply(self.attach(caller, stream_id, region_grant)),
            ),
            AudioRequest::Start { stream_id, at } => put(
                reply,
                &encode_status_reply(self.start(caller, stream_id, at, sink)),
            ),
            AudioRequest::Stop { stream_id, at } => put(
                reply,
                &encode_status_reply(self.stop(caller, stream_id, at, sink)),
            ),
            AudioRequest::Drain { stream_id } => put(
                reply,
                &encode_status_reply(self.drain(caller, stream_id, sink)),
            ),
            AudioRequest::Flush { stream_id } => {
                put(reply, &encode_status_reply(self.flush(caller, stream_id)))
            }
            AudioRequest::Clock { stream_id } => put(
                reply,
                &encode_clock_reply(self.clock_report(caller, stream_id)),
            ),
            AudioRequest::Gain { stream_id, gain } => put(
                reply,
                &encode_status_reply(self.set_level(caller, stream_id, Some(gain), None)),
            ),
            AudioRequest::Mute { stream_id, muted } => put(
                reply,
                &encode_status_reply(self.set_level(caller, stream_id, None, Some(muted))),
            ),
            AudioRequest::State { stream_id } => {
                put(reply, &encode_state_reply(self.report(caller, stream_id)))
            }
            AudioRequest::Close { stream_id } => put(
                reply,
                &encode_status_reply(self.close(caller, stream_id, sink)),
            ),
            // Adopting a driver channel needs a transport and a bound notify
            // port, neither of which the pure engine can build; the process
            // half intercepts it before dispatch and calls `bind_device`.
            AudioRequest::BindDriver { .. } => {
                put(reply, &encode_status_reply(Err(Errno::NotSupported)))
            }
            AudioRequest::UnbindDriver { endpoint_id } => put(
                reply,
                &encode_status_reply(self.unbind(caller, endpoint_id, sink)),
            ),
            AudioRequest::Baseline(baseline) => put(
                reply,
                &encode_status_reply(self.adopt_baseline(caller, baseline, sink)),
            ),
            AudioRequest::SetDefault { device_id } => put(
                reply,
                &encode_status_reply(self.set_default(caller, device_id, sink)),
            ),
            AudioRequest::SetLevel { device_id, level } => put(
                reply,
                &encode_status_reply(self.set_controls(caller, device_id, Some(level), None, sink)),
            ),
            AudioRequest::SetMute { device_id, muted } => put(
                reply,
                &encode_status_reply(self.set_controls(caller, device_id, None, Some(muted), sink)),
            ),
            AudioRequest::ListStreams { after } => put(
                reply,
                &encode_streams_reply(self.list_stream(caller, after)),
            ),
        }
    }

    /// Retire the device whose channel is `endpoint_id`: the device manager
    /// saw its node leave the hardware tree.
    fn unbind(&mut self, caller: &Origin, endpoint_id: u64, sink: &dyn Sink) -> Result<(), Errno> {
        if !caller.capabilities().holds_cap(CapabilityId::DRV_LOAD) {
            return Err(Errno::PermissionDenied);
        }
        let slot = self
            .present()
            .find(|(_, device)| device.channel_endpoint == endpoint_id && !device.lost)
            .map(|(slot, _)| slot)
            .ok_or(Errno::NotFound)?;
        self.lose_device(slot, Errno::DeviceOffline, sink);
        Ok(())
    }

    /// Adopt the machine's baseline beneath every tenant's controls.
    fn adopt_baseline(
        &mut self,
        caller: &Origin,
        baseline: AudioBaseline,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        if !caller.capabilities().holds_cap(CapabilityId::DRV_LOAD) {
            return Err(Errno::PermissionDenied);
        }
        if !self.controls.set_baseline(baseline) {
            return Ok(());
        }
        self.apply_all_levels(sink);
        self.refresh_defaults(sink);
        self.changed();
        audit(
            sink,
            events::BASELINE_ADOPTED,
            Level::Info,
            "the machine's audio baseline was adopted",
            &[Field {
                key: "level",
                value: FieldValue::SignedInt(i64::from(baseline.level.millibel())),
            }],
        );
        Ok(())
    }

    /// The tenant a control from `caller` acts for: the room's, when the room
    /// admits the caller's change.
    fn tenant_for(&self, caller: &Origin) -> Result<Tenant, Errno> {
        match self.room {
            Room::Unclaimed => Ok(Tenant::Unclaimed),
            Room::Session(holder) if caller.login_session() == Some(holder) => {
                Ok(Tenant::Session(holder))
            }
            Room::Session(_) | Room::Withheld => Err(Errno::SeatNotOwner),
        }
    }

    /// The tenant a control from `caller` acts for, the refusal recorded
    /// against who tried.
    fn admit_control(&self, caller: &Origin, sink: &dyn Sink) -> Result<Tenant, Errno> {
        self.tenant_for(caller).inspect_err(|err| {
            audit(
                sink,
                events::CONTROL_REFUSED,
                Level::Warn,
                "a device control was refused: the room is not the caller's",
                &[
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                    },
                    Field {
                        key: "pid",
                        value: FieldValue::UnsignedInt(caller.pid()),
                    },
                    Field {
                        key: "error",
                        value: FieldValue::Error(*err),
                    },
                ],
            );
        })
    }

    /// Make the endpoint `device_id` names its direction's default, for the
    /// room's tenant.
    fn set_default(
        &mut self,
        caller: &Origin,
        device_id: u32,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        let tenant = self.admit_control(caller, sink)?;
        let (device_index, slot) = self.find_endpoint(device_id)?;
        let endpoint = self.endpoint(device_index, slot).ok_or(Errno::NotFound)?;
        let (direction, location) = (endpoint.facts.direction, endpoint.location);
        if self.controls.prefer(tenant, direction, location)? {
            Self::audit_control(sink, caller, device_id, "default");
            self.refresh_defaults(sink);
        }
        Ok(())
    }

    /// Set an endpoint's level, its mute, or both, for the room's tenant.
    fn set_controls(
        &mut self,
        caller: &Origin,
        device_id: u32,
        level: Option<AudioGain>,
        muted: Option<bool>,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        let tenant = self.admit_control(caller, sink)?;
        let (device_index, slot) = self.find_endpoint(device_id)?;
        let location = self
            .endpoint(device_index, slot)
            .ok_or(Errno::NotFound)?
            .location;
        let mut moved = false;
        if let Some(level) = level {
            moved |= self.controls.set_level(tenant, location, level)?;
        }
        if let Some(muted) = muted {
            moved |= self.controls.set_muted(tenant, location, muted)?;
        }
        if moved {
            Self::audit_control(sink, caller, device_id, "level");
            self.apply_level(device_index, slot, sink);
            self.changed();
        }
        Ok(())
    }

    /// Record a device control that took effect, and whose it was.
    fn audit_control(sink: &dyn Sink, caller: &Origin, device_id: u32, what: &'static str) {
        audit(
            sink,
            events::CONTROL_CHANGED,
            Level::Info,
            "a device control changed",
            &[
                Field {
                    key: "control",
                    value: FieldValue::Str(what),
                },
                Field {
                    key: "device",
                    value: FieldValue::UnsignedInt(u64::from(device_id)),
                },
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                },
            ],
        );
    }

    /// The first open stream whose id is above `after`, for the System
    /// Information service alone.
    fn list_stream(&self, caller: &Origin, after: u64) -> Result<StreamDescriptor, Errno> {
        if !caller
            .capabilities()
            .holds_cap(CapabilityId::SYSINFO_INTROSPECT)
        {
            return Err(Errno::PermissionDenied);
        }
        // Streams are kept in the order their ids were issued.
        let stream = self
            .streams
            .iter()
            .find(|stream| stream.id > after)
            .ok_or(Errno::NotFound)?;
        let device_id = self
            .endpoint(stream.device, stream.endpoint)
            .map_or(0, |endpoint| endpoint.device_id);
        Ok(StreamDescriptor {
            stream_id: stream.id,
            device_id,
            direction: stream.direction,
            role: stream.role,
            state: stream.reported(),
            position: stream.position,
            xruns: stream.xruns,
            xrun_frames: stream.xrun_frames,
            owner_uid: stream.owner_uid,
            owner_pid: stream.owner_pid,
            owner_app: stream.owner_app,
        })
    }

    /// Deliver the controls in force on one endpoint: program its device's
    /// own control where it has one, and resolve every stream's multiply on
    /// it. A device that refuses its control keeps the whole level in
    /// software; one whose channel fails is lost.
    fn apply_level(&mut self, device_index: usize, slot: usize, sink: &dyn Sink) {
        let tenant = Tenant::of(self.room);
        let Some(device) = self.devices.get_mut(device_index).and_then(Option::as_mut) else {
            return;
        };
        if device.lost {
            return;
        }
        let Some(endpoint) = device.endpoints.get_mut(slot) else {
            return;
        };
        let level = self.controls.level(tenant, endpoint.location);
        let muted = self.controls.muted(tenant, endpoint.location);
        let mut delivered = endpoint_level(level, muted, endpoint.control);
        if let Some(setting) = delivered.hardware_millibel {
            let programmed =
                endpoint.level.hardware_millibel == Some(setting) && endpoint.level.muted == muted;
            let index = endpoint.index;
            if !programmed {
                match device.channel.gain(index, setting, muted) {
                    Ok(()) => {}
                    Err(Errno::NotSupported) => {
                        if let Some(endpoint) = device.endpoints.get_mut(slot) {
                            endpoint.control = None;
                        }
                        delivered = endpoint_level(level, muted, None);
                    }
                    Err(err) => {
                        self.lose_device(device_index, err, sink);
                        return;
                    }
                }
            }
        }
        if let Some(endpoint) = self.endpoint_mut(device_index, slot) {
            endpoint.level = delivered;
        }
        self.resolve_gains(device_index, slot);
    }

    /// Deliver the controls in force on every endpoint.
    fn apply_all_levels(&mut self, sink: &dyn Sink) {
        for device_index in 0..self.devices.len() {
            for slot in 0..self.endpoint_count(device_index) {
                self.apply_level(device_index, slot, sink);
            }
        }
    }

    /// Forget the controls of every session that neither holds the room nor
    /// owns a stream: nothing could be heard at them.
    fn forget_tenancies(&mut self) {
        let Self {
            controls,
            streams,
            room,
            ..
        } = self;
        let holder = match room {
            Room::Session(holder) => Some(*holder),
            Room::Unclaimed | Room::Withheld => None,
        };
        controls.retain(|session| {
            holder == Some(session) || streams.iter().any(|stream| stream.session == Some(session))
        });
    }

    /// Give back the device in slot `device_index` once it is lost and no
    /// stream rides it, leaving the slot for the next bind.
    fn reap(&mut self, device_index: usize) {
        let lost = self
            .devices
            .get(device_index)
            .and_then(Option::as_ref)
            .is_some_and(|device| device.lost);
        if lost
            && !self
                .streams
                .iter()
                .any(|stream| stream.device == device_index)
        {
            self.devices[device_index] = None;
        }
    }

    /// The endpoint in slot `slot` of the device in slot `device`.
    fn endpoint(&self, device: usize, slot: usize) -> Option<&device::Endpoint> {
        self.devices
            .get(device)
            .and_then(Option::as_ref)?
            .endpoints
            .get(slot)
    }

    /// The endpoint in slot `slot` of the device in slot `device`, mutably.
    fn endpoint_mut(&mut self, device: usize, slot: usize) -> Option<&mut device::Endpoint> {
        self.devices
            .get_mut(device)
            .and_then(Option::as_mut)?
            .endpoints
            .get_mut(slot)
    }

    /// The endpoint a client-visible identity names, whichever its direction.
    fn find_endpoint(&self, device_id: u32) -> Result<(usize, usize), Errno> {
        for (index, device) in self.present() {
            if let Some(slot) = device
                .endpoints
                .iter()
                .position(|endpoint| endpoint.device_id == device_id)
            {
                if device.lost {
                    return Err(Errno::DeviceOffline);
                }
                return Ok((index, slot));
            }
        }
        Err(Errno::NotFound)
    }

    /// A driver's notify frame arrived on the `device`th channel's port.
    ///
    /// A frame that does not decode is dropped: the port is the driver's own
    /// and a malformed wake is a driver defect, not something to act on.
    pub fn on_device_notify(&mut self, device: usize, frame: &[u8], sink: &dyn Sink) {
        let Ok(notify) = AudioChannelNotify::decode(frame) else {
            return;
        };
        match notify {
            AudioChannelNotify::PeriodElapsed {
                endpoint,
                position,
                sampled_at,
            } => {
                let Some(slot) = self.endpoint_slot(device, endpoint) else {
                    return;
                };
                if let Some(active) = self.active_mut(device, slot) {
                    active.position = position;
                    let _ = active.clock.observe(position, sampled_at);
                }
                self.pump(device, slot, sink);
            }
            AudioChannelNotify::Xrun {
                endpoint,
                position,
                lost_frames,
            } => {
                let Some(slot) = self.endpoint_slot(device, endpoint) else {
                    return;
                };
                if let Some(active) = self.active_mut(device, slot) {
                    active.position = position;
                    active.xrun_frames = active.xrun_frames.saturating_add(lost_frames);
                }
                if let Some(endpoint) = self.endpoint_mut(device, slot) {
                    endpoint.lost_frames = endpoint.lost_frames.saturating_add(lost_frames);
                }
                for index in 0..self.streams.len() {
                    if !self.streams[index].on(device, slot) || !self.streams[index].is_live() {
                        continue;
                    }
                    let stream = &self.streams[index];
                    let at = stream.position;
                    let notify = AudioNotify::Xrun {
                        stream_id: stream.id,
                        at,
                        lost_frames,
                    };
                    let port = stream.grant.notify_endpoint;
                    self.streams[index].xrun_frames =
                        self.streams[index].xrun_frames.saturating_add(lost_frames);
                    self.streams[index].xruns = self.streams[index].xruns.saturating_add(1);
                    device::send(&mut self.notifier, port, notify);
                }
            }
            AudioChannelNotify::Drained { endpoint, position } => {
                let Some(slot) = self.endpoint_slot(device, endpoint) else {
                    return;
                };
                if let Some(active) = self.active_mut(device, slot) {
                    active.position = position;
                    active.running = false;
                }
                self.settle_drain(device, slot);
                // A stream that went live while the device drained — started,
                // or released by the room — is not left on a stopped clock.
                if self.any_live(device, slot) {
                    self.keep_running(device, slot, sink);
                }
            }
            AudioChannelNotify::JackChanged { endpoint, jack } => {
                let Some(slot) = self.endpoint_slot(device, endpoint) else {
                    return;
                };
                if let Some(endpoint) = self.endpoint_mut(device, slot) {
                    endpoint.facts.jack = jack;
                }
                self.changed();
            }
            AudioChannelNotify::Faulted { endpoint, reason } => {
                if self.endpoint_slot(device, endpoint).is_some() {
                    self.lose_device(device, reason, sink);
                }
            }
        }
    }

    /// Move whatever frames the endpoint can take or give, and pass a
    /// completed drain down to the device.
    ///
    /// A pump that faults marks the device lost: retrying into a device that
    /// just faulted would storm it, and the streams' positions are intact.
    fn pump(&mut self, device_index: usize, slot: usize, sink: &dyn Sink) {
        let Self {
            regions,
            notifier,
            devices,
            streams,
            ..
        } = self;
        let Some(device) = devices.get_mut(device_index).and_then(Option::as_mut) else {
            return;
        };
        if device.lost {
            return;
        }
        let pumped = match device::pump_endpoint(
            device,
            device_index,
            slot,
            streams,
            regions,
            notifier,
            sink,
        ) {
            Ok(pumped) => pumped,
            Err(err) => {
                self.lose_device(device_index, err, sink);
                return;
            }
        };
        if self.settle_stops(device_index, slot) {
            self.quiesce_endpoint(device_index, slot, sink);
        }
        if pumped.drained {
            self.finish_drain(device_index, slot);
        }
    }

    /// Mark the `device_index`th device lost for `err`. Nothing reaches a
    /// lost device again, so every stream on it — whichever endpoint it rides
    /// — holds its position as [`StreamState::DeviceLost`] rather than
    /// waiting on a wake that will never come.
    fn lose_device(&mut self, device_index: usize, err: Errno, sink: &dyn Sink) {
        let Self {
            notifier,
            devices,
            streams,
            ..
        } = self;
        let Some(device) = devices.get_mut(device_index).and_then(Option::as_mut) else {
            return;
        };
        if device.lost {
            return;
        }
        device.lost = true;
        for stream in streams.iter_mut().filter(|s| s.device == device_index) {
            let at = stream.position;
            stream.set_state(StreamState::DeviceLost, at, notifier);
        }
        audit(
            sink,
            events::DEVICE_LOST,
            Level::Error,
            "audio device faulted; its streams hold their positions",
            &[
                Field {
                    key: "endpoint",
                    value: FieldValue::UnsignedInt(device.channel_endpoint),
                },
                Field {
                    key: "error",
                    value: FieldValue::Error(err),
                },
            ],
        );
        self.refresh_defaults(sink);
        self.changed();
        self.reap(device_index);
    }

    /// Pause every stream on the endpoint that has reached its scheduled
    /// stop, at exactly the frame it named, answering whether any did.
    fn settle_stops(&mut self, device_index: usize, slot: usize) -> bool {
        let Self {
            notifier, streams, ..
        } = self;
        let mut settled = false;
        for stream in streams.iter_mut() {
            if !stream.on(device_index, slot) {
                continue;
            }
            let Some(at) = stream.stop_at else { continue };
            if stream.state == StreamState::Running && stream.position >= at {
                stream.stop_at = None;
                stream.set_state(StreamState::Paused, at, notifier);
                settled = true;
            }
        }
        settled
    }

    /// Every draining stream on the endpoint has given up its last frame:
    /// pass the drain down and settle the streams.
    fn finish_drain(&mut self, device_index: usize, slot: usize) {
        let Self {
            devices,
            streams,
            notifier,
            ..
        } = self;
        let Some(device) = devices.get_mut(device_index).and_then(Option::as_mut) else {
            return;
        };
        let Some(endpoint) = device.endpoints.get(slot) else {
            return;
        };
        let draining = streams.iter().any(|stream| {
            stream.on(device_index, slot) && stream.is_live() && stream.is_draining()
        });
        if !draining {
            return;
        }
        let index = endpoint.index;
        if device.lost {
            // Nothing will ever play these out, so the wait would never end.
            Self::settle_streams(device_index, slot, streams, notifier);
            return;
        }
        // Asking the device to drain is not the drain finishing: the frames
        // already handed over are in the driver's own transfers, and only it
        // can say when the last one was heard. The streams stay `Draining`
        // until its `Drained` notify arrives.
        let _ = device.channel.drain(index);
    }

    /// Complete every draining stream on `(device, slot)` — the device has
    /// played out what it held.
    fn settle_drain(&mut self, device_index: usize, slot: usize) {
        let Self {
            streams, notifier, ..
        } = self;
        Self::settle_streams(device_index, slot, streams, notifier);
    }

    /// Move each draining stream on `(device_index, slot)` to `Idle`; one the
    /// room holds has not played out and keeps draining.
    fn settle_streams(device_index: usize, slot: usize, streams: &mut [Stream], notifier: &mut N) {
        for stream in streams.iter_mut() {
            if !stream.on(device_index, slot) || stream.held || !stream.is_draining() {
                continue;
            }
            let at = stream.position;
            stream.set_state(StreamState::Idle, at, notifier);
        }
    }

    /// The `index`th sink or source of a device still there, as `caller`
    /// sees it.
    fn enumerate(
        &self,
        caller: &Origin,
        direction: StreamDirection,
        after: u32,
    ) -> Result<AudioDeviceDescriptor, Errno> {
        let default = self.defaults[direction_slot(direction)];
        let tenant = Tenant::of(self.room);
        let admitted = self.tenant_for(caller).ok();
        let [preferred, _] = self.controls.preferences(tenant, direction);
        self.live_endpoints()
            .filter(|endpoint| endpoint.facts.direction == direction && endpoint.device_id > after)
            .min_by_key(|endpoint| endpoint.device_id)
            .map(|endpoint| {
                let active = endpoint.active.as_ref();
                AudioDeviceDescriptor {
                    device_id: endpoint.device_id,
                    direction,
                    jack: endpoint.facts.jack,
                    default: if default != Some(endpoint.device_id) {
                        DefaultChoice::No
                    } else if preferred == Some(endpoint.location) {
                        DefaultChoice::Preferred
                    } else {
                        DefaultChoice::Inherited
                    },
                    formats: endpoint.facts.formats,
                    channel_map: endpoint.facts.channel_map,
                    rates: endpoint.facts.rates,
                    gain: endpoint.control,
                    name: endpoint.facts.name,
                    location: endpoint.location,
                    level: self.controls.level(tenant, endpoint.location),
                    own_level: self.controls.own_level(tenant, endpoint.location),
                    muted: self.controls.muted(tenant, endpoint.location),
                    access: match admitted {
                        None => ControlAccess::Shown,
                        Some(Tenant::Unclaimed) => ControlAccess::Shared,
                        Some(Tenant::Session(_)) => ControlAccess::Own,
                    },
                    clock_millihertz: active
                        .filter(|active| active.running)
                        .and_then(|active| active.clock.report())
                        .map_or(0, |report| report.rate_millihertz),
                    lost_frames: endpoint.lost_frames,
                }
            })
            .ok_or(Errno::NotFound)
    }

    /// Open a stream, and answer what was granted.
    fn open(
        &mut self,
        caller: &Origin,
        params: &OpenParams,
        sink: &dyn Sink,
    ) -> Result<StreamGrant, Errno> {
        let capture = params.direction == StreamDirection::Capture;
        if capture && !caller.capabilities().holds_cap(CapabilityId::AUDIO_CAPTURE) {
            Self::audit_capture(
                sink,
                caller,
                events::CAPTURE_REFUSED,
                "no capture capability",
            );
            return Err(Errno::PermissionDenied);
        }
        let resolved = self.resolve_target(caller, params);
        let (device_index, slot) = match resolved {
            Ok(target) => target,
            Err(err) => {
                if capture {
                    Self::audit_capture(sink, caller, events::CAPTURE_REFUSED, "no such source");
                }
                Self::audit_refusal(sink, params, "no endpoint to route to", err);
                return Err(err);
            }
        };
        let opened = self.build_stream(caller, params, device_index, slot);
        match opened {
            Ok(grant) => {
                if capture {
                    Self::audit_capture(
                        sink,
                        caller,
                        events::CAPTURE_OPENED,
                        "capture stream open",
                    );
                }
                Ok(grant)
            }
            Err(err) => {
                if capture {
                    Self::audit_capture(
                        sink,
                        caller,
                        events::CAPTURE_REFUSED,
                        "source unavailable",
                    );
                }
                Self::audit_refusal(sink, params, "the endpoint could not be programmed", err);
                Err(err)
            }
        }
    }

    /// Record a device that could not be primed, naming what refused it.
    fn audit_refusal_device(sink: &dyn Sink, channel_endpoint: u64, err: Errno) {
        audit(
            sink,
            events::STREAM_REFUSED,
            Level::Warn,
            "the device would not take the stream's first periods",
            &[
                Field {
                    key: "endpoint",
                    value: FieldValue::UnsignedInt(channel_endpoint),
                },
                Field {
                    key: "error",
                    value: FieldValue::Error(err),
                },
            ],
        );
    }

    /// Record a refused open with what refused it, so a machine with no
    /// sound names its reason instead of failing silently.
    fn audit_refusal(sink: &dyn Sink, params: &OpenParams, stage: &'static str, err: Errno) {
        audit(
            sink,
            events::STREAM_REFUSED,
            Level::Warn,
            stage,
            &[
                Field {
                    key: "device",
                    value: FieldValue::UnsignedInt(u64::from(params.device_id)),
                },
                Field {
                    key: "capture",
                    value: FieldValue::Bool(params.direction == StreamDirection::Capture),
                },
                Field {
                    key: "error",
                    value: FieldValue::Error(err),
                },
            ],
        );
    }

    /// Which endpoint a request lands on: the routing policy for a sink, the
    /// named-or-default source for a capture.
    fn resolve_target(
        &mut self,
        caller: &Origin,
        params: &OpenParams,
    ) -> Result<(usize, usize), Errno> {
        let device_id = match params.direction {
            StreamDirection::Playback => {
                let sinks = self.sink_states()?;
                let request = StreamRequest {
                    role: params.role,
                    requested_device: (params.device_id != 0).then_some(params.device_id),
                    session: caller.login_session(),
                };
                match route::route(&request, &sinks) {
                    Routing::Play { device_id } | Routing::Pause { device_id } => device_id,
                    // A notification the router would discard is refused at
                    // open rather than accepted and silently dropped: a
                    // client is owed the truth about where its sound went.
                    Routing::Drop => return Err(Errno::SeatNotOwner),
                    Routing::Refuse(err) => return Err(err),
                }
            }
            StreamDirection::Capture => {
                if params.device_id != 0 {
                    params.device_id
                } else {
                    self.defaults[direction_slot(StreamDirection::Capture)]
                        .ok_or(Errno::DeviceOffline)?
                }
            }
        };
        self.locate(device_id, params.direction)
    }

    /// The sinks the routing policy sees: every sink of a device still there.
    fn sink_states(&self) -> Result<Vec<SinkState>, Errno> {
        let default = self.defaults[direction_slot(StreamDirection::Playback)];
        let sinks = || {
            self.live_endpoints()
                .filter(|endpoint| endpoint.facts.direction == StreamDirection::Playback)
        };
        let states = sinks().map(|endpoint| SinkState {
            device_id: endpoint.device_id,
            is_default: default == Some(endpoint.device_id),
            // Every sink is the boot seat's: no device is assigned to a seat
            // of its own.
            room: self.room,
        });
        fallible::collected(sinks().count(), states).ok_or(Errno::OutOfMemory)
    }

    /// The (device, endpoint) a client-visible identity names, for a stream
    /// in `direction`.
    fn locate(&self, device_id: u32, direction: StreamDirection) -> Result<(usize, usize), Errno> {
        let (index, slot) = self.find_endpoint(device_id)?;
        match self.endpoint(index, slot) {
            Some(endpoint) if endpoint.facts.direction == direction => Ok((index, slot)),
            Some(_) => Err(Errno::NotSupported),
            None => Err(Errno::NotFound),
        }
    }

    /// Program the endpoint if it is not already, size the client's ring, and
    /// record the stream.
    fn build_stream(
        &mut self,
        caller: &Origin,
        params: &OpenParams,
        device_index: usize,
        slot: usize,
    ) -> Result<StreamGrant, Errno> {
        let pid = caller.pid();
        let stream_slot = self.free_slot(pid).ok_or(Errno::LimitExceeded)?;
        let seed = self.clock.now_ns();
        let Self {
            devices, regions, ..
        } = self;
        let device = devices
            .get_mut(device_index)
            .and_then(Option::as_mut)
            .ok_or(Errno::NotFound)?;
        device::configure_endpoint(
            device,
            slot,
            regions,
            params.rate,
            params.format,
            params.latency_target_frames,
            seed,
        )?;
        let clock_domain = device
            .endpoints
            .get(slot)
            .map_or(0, |endpoint| endpoint.device_id);
        let active = device
            .endpoints
            .get_mut(slot)
            .and_then(|endpoint| endpoint.active.as_mut())
            .ok_or(Errno::NotConnected)?;
        let period_at_client =
            device::scale_frames(active.period_frames(), active.grant.rate, params.rate);
        let ring_frames = client_ring_frames(params.latency_target_frames, period_at_client)?;
        // The granted latency *is* the ring: one device period of it is the
        // mixer's own contribution and the rest is what the client may have
        // queued, so the figure a client reasons about is the whole of what
        // it can have in flight.
        let latency_frames = ring_frames;
        let geometry = PcmGeometry::new(ring_frames, params.format, params.channel_map.channels())?;
        let stream_id = self.next_stream_id;
        let grant = StreamGrant {
            stream_id,
            notify_endpoint: notify_endpoint_for(pid, stream_slot),
            rate: params.rate,
            format: params.format,
            channel_map: params.channel_map,
            ring_frames,
            granted_latency_frames: latency_frames,
            granted_latency: Frames::new(u64::from(latency_frames)).duration(params.rate),
            clock_domain,
        };
        let session = caller.login_session();
        let held =
            route::admit(params.direction, params.role, session, self.room) != Admission::Mix;
        let stream = Stream::open(
            active,
            &StreamOpen {
                id: stream_id,
                owner_pid: pid,
                owner_uid: caller.uid(),
                owner_app: caller
                    .app()
                    .and_then(|app| BundleId::new(app.bundle_id()).ok()),
                device: device_index,
                endpoint: slot,
                direction: params.direction,
                role: params.role,
                session,
                held,
                grant,
                geometry,
            },
        )?;
        if !fallible::reserve(&mut self.streams, 1) {
            return Err(Errno::OutOfMemory);
        }
        self.streams.push(stream);
        self.next_stream_id = self.next_stream_id.saturating_add(1);
        self.resolve_gains(device_index, slot);
        Ok(grant)
    }

    /// The lowest per-process stream slot `pid` is not already using.
    fn free_slot(&self, pid: u64) -> Option<u64> {
        (0..MAX_CLIENT_STREAM_SLOTS).find(|slot| {
            let endpoint = notify_endpoint_for(pid, *slot);
            !self
                .streams
                .iter()
                .any(|stream| stream.owner_pid == pid && stream.grant.notify_endpoint == endpoint)
        })
    }

    /// The index of a stream the caller genuinely owns.
    fn owned(&self, caller: &Origin, stream_id: u64) -> Result<usize, Errno> {
        self.streams
            .iter()
            .position(|stream| stream.id == stream_id)
            .filter(|index| self.streams[*index].owner_pid == caller.pid())
            .ok_or(Errno::NotFound)
    }

    /// Adopt the caller's granted ring for its stream.
    fn attach(&mut self, caller: &Origin, stream_id: u64, region_grant: u64) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        let len = self.streams[index].geometry.region_len();
        let adopted = self.regions.adopt(caller.proc_id(), region_grant, len)?;
        if let Some(previous) = self.streams[index].region.replace(adopted) {
            self.regions.release(previous);
        }
        Ok(())
    }

    /// Begin moving frames at an exact position.
    fn start(
        &mut self,
        caller: &Origin,
        stream_id: u64,
        at: Frames,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        if self.streams[index].region.is_none() {
            return Err(Errno::NotAttached);
        }
        self.usable(index)?;
        // Refused rather than accepted and dropped, exactly as at open.
        if self.admission(index, self.room) == Admission::Drop {
            return Err(Errno::SeatNotOwner);
        }
        let position = self.streams[index].position;
        // A start behind the position the stream has already reached names a
        // frame that is gone; there is no honest answer but a refusal.
        let Some(skip) = at.since(position) else {
            return Err(Errno::OutOfRange);
        };
        if skip != 0 {
            self.discard(index, skip)?;
        }
        // A stop scheduled ahead of where the stream now starts still names a
        // frame it will reach; one at or behind it is stale.
        self.streams[index].stop_at = self.streams[index].stop_at.filter(|stop| *stop > at);
        let (device_index, slot) = (self.streams[index].device, self.streams[index].endpoint);
        let at = self.streams[index].position;
        {
            let Self {
                streams, notifier, ..
            } = self;
            streams[index].set_state(StreamState::Running, at, notifier);
        }
        self.resolve_gains(device_index, slot);
        if !self.streams[index].is_live() {
            // Held for the room: it starts moving when the room is its own.
            return Ok(());
        }
        self.run_endpoint(device_index, slot, sink)
    }

    /// Refuse a request that would revive a stream which can no longer move:
    /// its device is gone, or its own ring broke the protocol.
    fn usable(&self, index: usize) -> Result<(), Errno> {
        match self.streams[index].state {
            StreamState::DeviceLost => Err(Errno::DeviceOffline),
            StreamState::Faulted => Err(Errno::BadMagic),
            _ => Ok(()),
        }
    }

    /// Prime (playback) or arm (capture) the endpoint and set it clocking.
    fn run_endpoint(
        &mut self,
        device_index: usize,
        slot: usize,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        let direction = self
            .devices
            .get(device_index)
            .and_then(Option::as_ref)
            .and_then(|device| device.endpoints.get(slot))
            .map(|endpoint| endpoint.facts.direction)
            .ok_or(Errno::NotFound)?;
        let already = self
            .devices
            .get(device_index)
            .and_then(Option::as_ref)
            .and_then(|device| device.endpoints.get(slot))
            .and_then(|endpoint| endpoint.active.as_ref())
            .is_some_and(|active| active.running);
        // A sink is primed before it is clocked, or its first period is a
        // gap; a source has nothing to prime and is clocked first. Filling
        // the ring is only half of it: the driver has to be told to move
        // those frames onto the device, because nothing has interrupted yet
        // to make it do so on its own.
        if direction == StreamDirection::Playback && !already {
            self.pump(device_index, slot, sink);
            let device = self
                .devices
                .get_mut(device_index)
                .and_then(Option::as_mut)
                .ok_or(Errno::NotFound)?;
            if let Err(err) = device::prime_endpoint(device, slot) {
                Self::audit_refusal_device(sink, device.channel_endpoint, err);
                return Err(err);
            }
        }
        if !already {
            let device = self
                .devices
                .get_mut(device_index)
                .and_then(Option::as_mut)
                .ok_or(Errno::NotFound)?;
            let endpoint_index = device
                .endpoints
                .get(slot)
                .map(|endpoint| endpoint.index)
                .ok_or(Errno::NotFound)?;
            let position = device
                .endpoints
                .get(slot)
                .and_then(|endpoint| endpoint.active.as_ref())
                .map_or(Frames::ZERO, |active| active.position);
            device.channel.start(endpoint_index, position)?;
            if let Some(active) = device
                .endpoints
                .get_mut(slot)
                .and_then(|endpoint| endpoint.active.as_mut())
            {
                active.running = true;
            }
        } else if direction == StreamDirection::Playback {
            self.pump(device_index, slot, sink);
        }
        // A stop scheduled inside the first frames primed has already
        // settled, leaving the device to play them out and nothing behind.
        self.quiesce_endpoint(device_index, slot, sink);
        Ok(())
    }

    /// Stop at an exact position, holding it so a resume is exact.
    fn stop(
        &mut self,
        caller: &Origin,
        stream_id: u64,
        at: Frames,
        sink: &dyn Sink,
    ) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        self.usable(index)?;
        let position = self.streams[index].position;
        let (device_index, slot) = (self.streams[index].device, self.streams[index].endpoint);
        if at > position {
            // A stop scheduled ahead: the pump stops pulling this stream at
            // exactly that frame and settles it there.
            self.streams[index].stop_at = Some(at);
            return Ok(());
        }
        self.streams[index].stop_at = None;
        {
            let Self {
                streams, notifier, ..
            } = self;
            streams[index].set_state(StreamState::Paused, position, notifier);
        }
        self.quiesce_endpoint(device_index, slot, sink);
        Ok(())
    }

    /// Stop the device's clock once nothing on the endpoint is live, or
    /// rebalance the gains of what is.
    ///
    /// A sink first plays out what the device already holds: those frames
    /// were mixed before the last stream stopped, so a resume is exact, and
    /// held in the device they would play into whatever starts next. A source
    /// stops at once and drops what it captured past its streams' positions —
    /// sound from after they stopped, or from a room they are no longer in.
    /// The configuration stays while any stream is open on the endpoint; the
    /// last stream's close gives it back.
    fn quiesce_endpoint(&mut self, device_index: usize, slot: usize, sink: &dyn Sink) {
        if self.any_live(device_index, slot) {
            self.resolve_gains(device_index, slot);
            return;
        }
        let Self {
            devices, regions, ..
        } = self;
        let Some(device) = devices.get_mut(device_index).and_then(Option::as_mut) else {
            return;
        };
        let Some(endpoint) = device.endpoints.get_mut(slot) else {
            return;
        };
        let (index, direction) = (endpoint.index, endpoint.facts.direction);
        let Some(active) = endpoint.active.as_mut().filter(|active| active.running) else {
            return;
        };
        if device.lost {
            return;
        }
        let stopped = match direction {
            StreamDirection::Playback => device.channel.drain(index),
            StreamDirection::Capture => {
                active.running = false;
                let position = active.position;
                device
                    .channel
                    .stop(index, position)
                    .and_then(|()| device::discard_captured(device, slot, regions))
            }
        };
        if let Err(err) = stopped {
            self.lose_device(device_index, err, sink);
        }
    }

    /// Play out everything queued, then stop.
    fn drain(&mut self, caller: &Origin, stream_id: u64, sink: &dyn Sink) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        if self.streams[index].state != StreamState::Running {
            return Err(Errno::NotConnected);
        }
        let (device_index, slot) = (self.streams[index].device, self.streams[index].endpoint);
        let at = self.streams[index].position;
        {
            let Self {
                streams, notifier, ..
            } = self;
            streams[index].set_state(StreamState::Draining, at, notifier);
        }
        self.pump(device_index, slot, sink);
        Ok(())
    }

    /// Discard everything queued; the position advances over the discarded
    /// frames, so what follows still describes where it belongs.
    fn flush(&mut self, caller: &Origin, stream_id: u64) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        let queued = self.queued_frames(index)?;
        self.discard(index, u64::from(queued))?;
        self.streams[index].reset_filter();
        Ok(())
    }

    /// The device clock this stream is in the domain of.
    fn clock_report(
        &self,
        caller: &Origin,
        stream_id: u64,
    ) -> Result<tairix_abi::audio::ClockReport, Errno> {
        let index = self.owned(caller, stream_id)?;
        let stream = &self.streams[index];
        self.devices
            .get(stream.device).and_then(Option::as_ref)
            .and_then(|device| device.endpoints.get(stream.endpoint))
            .and_then(|endpoint| endpoint.active.as_ref())
            .ok_or(Errno::NotConnected)?
            .clock
            .report()
            // No period has been reported yet, so there is no fit to hand
            // back and no honest value to invent.
            .ok_or(Errno::WouldBlock)
    }

    /// Set this stream's level, its mute, or both.
    fn set_level(
        &mut self,
        caller: &Origin,
        stream_id: u64,
        gain: Option<AudioGain>,
        muted: Option<bool>,
    ) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        if let Some(gain) = gain {
            self.streams[index].gain = gain;
        }
        if let Some(muted) = muted {
            self.streams[index].muted = muted;
        }
        let (device_index, slot) = (self.streams[index].device, self.streams[index].endpoint);
        self.resolve_gains(device_index, slot);
        Ok(())
    }

    /// Where the stream stands, and what it has lost.
    fn report(&self, caller: &Origin, stream_id: u64) -> Result<StreamReport, Errno> {
        let index = self.owned(caller, stream_id)?;
        let stream = &self.streams[index];
        Ok(StreamReport {
            state: stream.reported(),
            changed_at: stream.state_at,
            xruns: stream.xruns,
            xrun_frames: stream.xrun_frames,
        })
    }

    /// Close the stream and release its region; the endpoint follows when it
    /// was the last one on it.
    fn close(&mut self, caller: &Origin, stream_id: u64, sink: &dyn Sink) -> Result<(), Errno> {
        let index = self.owned(caller, stream_id)?;
        let stream = self.streams.remove(index);
        if let Some(region) = stream.region {
            self.regions.release(region);
        }
        let (device_index, slot) = (stream.device, stream.endpoint);
        if let Some(active) = self.active_mut(device_index, slot) {
            device::release_stream_bank(active, &stream);
        }
        if stream.direction == StreamDirection::Capture {
            Self::audit_capture(
                sink,
                caller,
                events::CAPTURE_CLOSED,
                "capture stream closed",
            );
        }
        self.quiesce_endpoint(device_index, slot, sink);
        if !self
            .streams
            .iter()
            .any(|other| other.on(device_index, slot))
        {
            let Self {
                devices, regions, ..
            } = self;
            if let Some(device) = devices.get_mut(device_index).and_then(Option::as_mut) {
                device::release_endpoint(device, slot, regions);
            }
        }
        self.forget_tenancies();
        self.reap(device_index);
        Ok(())
    }

    /// Re-resolve every stream's one multiply on the endpoint, because the
    /// ducking rule reads the roles of the streams live beside it and the
    /// endpoint's own level lies beneath them all.
    fn resolve_gains(&mut self, device_index: usize, slot: usize) {
        let endpoint = self
            .endpoint(device_index, slot)
            .map_or(EndpointLevel::UNITY, |endpoint| endpoint.level);
        let live: Roles = self
            .streams
            .iter()
            .filter(|stream| stream.on(device_index, slot) && stream.is_live())
            .map(|stream| stream.role)
            .collect();
        for stream in &mut self.streams {
            if !stream.on(device_index, slot) {
                continue;
            }
            let request = VolumeRequest {
                stream_millibel: stream.gain.millibel(),
                duck_millibel: route::duck_millibel(stream.role, live),
                muted: stream.muted,
            };
            stream.software_gain = stream_multiply(&request, endpoint);
        }
    }

    /// Frames queued in a stream's client ring.
    fn queued_frames(&mut self, index: usize) -> Result<u32, Errno> {
        let Some(region) = self.streams[index].region else {
            return Ok(0);
        };
        let geometry = self.streams[index].geometry;
        let bytes = self.regions.bytes(region)?;
        let ring = tairix_abi::driver::audio_ring::PcmRing::bind(bytes, geometry)?;
        match self.streams[index].direction {
            StreamDirection::Playback => ring.readable_frames(),
            StreamDirection::Capture => Ok(0),
        }
    }

    /// Drop `frames` queued frames of a playback stream, advancing its
    /// position over them.
    fn discard(&mut self, index: usize, frames: u64) -> Result<(), Errno> {
        let Some(region) = self.streams[index].region else {
            return Ok(());
        };
        if self.streams[index].direction != StreamDirection::Playback {
            return Ok(());
        }
        let geometry = self.streams[index].geometry;
        let wanted = u32::try_from(frames).unwrap_or(u32::MAX);
        let dropped = {
            let bytes = self.regions.bytes(region)?;
            let mut ring = tairix_abi::driver::audio_ring::PcmRing::bind(bytes, geometry)?;
            ring.discard(wanted)?
        };
        self.streams[index].position = self.streams[index]
            .position
            .checked_add(u64::from(dropped))
            .unwrap_or(self.streams[index].position);
        Ok(())
    }

    /// The endpoint slot a driver's endpoint index names.
    fn endpoint_slot(&self, device: usize, endpoint: u16) -> Option<usize> {
        if endpoint >= MAX_DEVICE_ENDPOINTS {
            return None;
        }
        self.devices
            .get(device)
            .and_then(Option::as_ref)?
            .endpoints
            .iter()
            .position(|candidate| candidate.index == endpoint)
    }

    /// The programmed state of one endpoint.
    fn active_mut(&mut self, device: usize, slot: usize) -> Option<&mut crate::device::Active> {
        self.devices
            .get_mut(device)
            .and_then(Option::as_mut)?
            .endpoints
            .get_mut(slot)?
            .active
            .as_mut()
    }

    /// Record a capture decision against the principal that took it.
    fn audit_capture(
        sink: &dyn Sink,
        caller: &Origin,
        id: tairix_log::EventId,
        message: &'static str,
    ) {
        let level = if id == events::CAPTURE_REFUSED {
            Level::Warn
        } else {
            Level::Info
        };
        audit(
            sink,
            id,
            level,
            message,
            &[
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                },
                Field {
                    key: "pid",
                    value: FieldValue::UnsignedInt(caller.pid()),
                },
            ],
        );
    }
}

/// The client ring a latency target of `target` frames earns, at least two
/// device periods deep and inside the ring vocabulary's fixed bounds.
///
/// A power of two, because the ring's slot index is a mask of the monotone
/// position rather than a division on the per-period path. Two periods is
/// the floor because one of them is the mixer's own contribution, so a
/// shallower ring would leave the client nothing to queue.
fn client_ring_frames(target: u32, period_at_client_rate: u32) -> Result<u32, Errno> {
    let floor = period_at_client_rate
        .max(1)
        .saturating_mul(2)
        .max(ring_bounds::MIN_FRAMES)
        .checked_next_power_of_two()
        .ok_or(Errno::OutOfRange)?;
    let wanted = target
        .max(ring_bounds::MIN_FRAMES)
        .checked_next_power_of_two()
        .ok_or(Errno::OutOfRange)?;
    let frames = wanted.max(floor).min(ring_bounds::MAX_FRAMES);
    if frames < floor {
        // The floor itself is past the vocabulary's ceiling: this device's
        // period cannot be served by any ring the contract admits.
        return Err(Errno::OutOfRange);
    }
    Ok(frames)
}

/// Copy `body` into the reply buffer and answer its length.
fn put(reply: &mut [u8; AUDIO_MAX_REPLY], body: &[u8]) -> usize {
    let len = body.len().min(reply.len());
    reply[..len].copy_from_slice(&body[..len]);
    len
}

/// The direction's name for an audit field.
const fn direction_name(direction: StreamDirection) -> &'static str {
    match direction {
        StreamDirection::Playback => "sink",
        StreamDirection::Capture => "source",
    }
}

/// Emit one audit record.
pub(crate) fn audit(
    sink: &dyn Sink,
    id: tairix_log::EventId,
    level: Level,
    message: &'static str,
    fields: &[Field<'_>],
) {
    log(
        sink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

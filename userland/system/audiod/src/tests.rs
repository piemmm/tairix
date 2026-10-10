//! Host tests of the audio service: the whole authority and the whole period
//! path, driven against an in-process device (`plans/SOUND.md` SND4).
//!
//! The doubles are the three I/O seams and nothing else — regions are owned
//! `Vec`s, the device-channel transport dispatches into a real
//! `tairix_audiochan::AudioChannelServer` over a recording device engine, and
//! the notifier keeps the frames it was handed. Everything between is the
//! production code, so a test that plays a signal genuinely drives the
//! router, the matrix, the volume model, the mixer and the rings.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::audio::{
    decode_enumerate_reply, decode_open_reply, decode_state_reply, decode_streams_reply,
    AudioBaseline, AudioDeviceDescriptor, AudioGain, AudioLocation, AudioNotify, AudioRequest,
    ControlAccess, DefaultChoice, OpenParams, StreamGrant, StreamRole, StreamState,
    AUDIO_MAX_REPLY, AUDIO_MAX_REQUEST,
};
use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName, AudioServiced,
    ChannelMap, Frames, GainRange, JackState, Rate, RateSet, RateSupport, SampleFormat,
    SampleFormats, StreamDirection,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, PcmRing, REGION_ALIGN_PADDING};
use tairix_abi::origin::{CapabilitySummary, Origin, ProcId, TrustDomain};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::seat::{DisplayLease, ReleaseSurface};
use tairix_abi::time::{MonotonicClock, Time64};
use tairix_abi::{CapabilityId, DriverError, Errno};
use tairix_audio::route::Room;
use tairix_audiochan::AudioChannelServer;
use tairix_log::DiscardSink;

use crate::{AudioChannelTransport, AudioService, Notifier, RegionHost, RegionId};

/// The rate the fixture device is clocked at.
const DEVICE_HZ: u32 = 48_000;
/// Frames the fixture device interrupts on.
const PERIOD: u32 = 256;
/// Frames the fixture device will hold in flight.
const MAX_RING: u32 = 4_096;
/// Where the fixture device sits in the hardware tree.
const FIXTURE_LOCATION: u64 = 0x9f3a_1c00_42de_7701;

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a rate inside the vocabulary")
}

// ---------------------------------------------------------------- doubles

/// A recording in-process audio device: it accepts one stereo `s16` sink and
/// one stereo `s16` source, and keeps every frame it was handed.
struct FixtureDevice {
    configured: Option<ConfigureGrant>,
    running: bool,
    /// Draining: take no more, and stop once what is queued has gone —
    /// the real driver's behaviour, which is what makes a drain complete
    /// only after playout.
    draining: bool,
    position: Frames,
    played: Vec<u8>,
    /// Frames a capture endpoint hands back, drawn from here in order.
    captured: Vec<u8>,
    xrun_frames: u64,
    now: Time64,
    /// Whether the sink endpoint refuses every rate but the device's own, so
    /// a test can force the resampled path.
    fixed_rate: bool,
    /// The sink's own gain control, where a test gives it one.
    control: Option<GainRange>,
    /// Every setting the mixer programmed: endpoint, level, mute.
    programmed: Vec<(u16, i32, bool)>,
}

impl FixtureDevice {
    fn new() -> Self {
        Self {
            configured: None,
            running: false,
            draining: false,
            position: Frames::ZERO,
            played: Vec::new(),
            captured: Vec::new(),
            xrun_frames: 0,
            now: Time64::from_secs(1),
            fixed_rate: true,
            control: None,
            programmed: Vec::new(),
        }
    }
}

impl Audio for FixtureDevice {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: 2,
            name: AudioName::new("fixture").expect("a short name"),
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        let direction = match endpoint {
            0 => StreamDirection::Playback,
            1 => StreamDirection::Capture,
            _ => return Err(DriverError::NotFound),
        };
        let rates = if self.fixed_rate {
            RateSupport::Discrete(
                RateSet::new(&[rate(DEVICE_HZ)]).map_err(|_| DriverError::DeviceFault)?,
            )
        } else {
            RateSupport::Continuous {
                min: rate(8_000),
                max: rate(DEVICE_HZ),
            }
        };
        Ok(AudioEndpointFacts {
            index: endpoint,
            direction,
            jack: JackState::Unknown,
            formats: SampleFormats::EMPTY.with(SampleFormat::S16),
            channel_map: ChannelMap::STEREO,
            rates,
            min_period_frames: PERIOD,
            max_period_frames: PERIOD,
            max_ring_frames: MAX_RING,
            gain: if direction == StreamDirection::Playback {
                self.control
            } else {
                None
            },
            name: AudioName::new("fixture").expect("a short name"),
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        let facts = self.endpoint_facts(endpoint)?;
        let grant = ConfigureGrant {
            rate: facts.rates.nearest(params.rate),
            format: SampleFormat::S16,
            channel_map: ChannelMap::STEREO,
            period_frames: PERIOD,
            max_ring_frames: MAX_RING,
        };
        grant.validate().map_err(|_| DriverError::DeviceFault)?;
        self.configured = Some(grant);
        Ok(grant)
    }

    fn start(&mut self, _endpoint: u16, at: Frames) -> Result<(), DriverError> {
        self.running = true;
        self.position = at;
        Ok(())
    }

    fn stop(&mut self, _endpoint: u16, at: Frames) -> Result<(), DriverError> {
        self.running = false;
        self.position = at;
        Ok(())
    }

    fn drain(&mut self, _endpoint: u16) -> Result<(), DriverError> {
        self.draining = true;
        Ok(())
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        let frame_bytes = ring.geometry().frame_bytes();
        let transferred = if endpoint == 0 {
            let queued = ring.readable_frames().map_err(|_| DriverError::BadMagic)?;
            let mut block = vec![0u8; queued as usize * frame_bytes];
            let taken = ring.read(&mut block).map_err(|_| DriverError::BadMagic)?;
            self.played
                .extend_from_slice(&block[..taken as usize * frame_bytes]);
            taken
        } else {
            let room = ring.writable_frames().map_err(|_| DriverError::BadMagic)?;
            let offer = (room as usize * frame_bytes).min(self.captured.len());
            let written = ring
                .write(&self.captured[..offer - offer % frame_bytes])
                .map_err(|_| DriverError::BadMagic)?;
            self.captured.drain(..written as usize * frame_bytes);
            written
        };
        self.position = Frames::new(self.position.get() + u64::from(transferred));
        if self.draining && ring.readable_frames().unwrap_or(0) == 0 {
            self.running = false;
            self.draining = false;
        }
        self.now =
            Time64::new(self.now.secs(), self.now.subsec_nanos() + 1_000_000).unwrap_or(self.now);
        Ok(AudioServiced {
            transferred,
            running: self.running,
            position: self.position,
            xrun_frames: self.xrun_frames,
            sampled_at: self.now,
        })
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        if endpoint != 0 || self.control.is_none() {
            return Err(DriverError::NotImplemented);
        }
        self.programmed.push((endpoint, millibel, mute));
        Ok(())
    }

    fn release(&mut self, _endpoint: u16) -> Result<(), DriverError> {
        self.configured = None;
        self.running = false;
        Ok(())
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        Ok(AudioInterrupt::NONE)
    }

    fn set_event_interrupts(&mut self, _enabled: bool) -> Result<(), DriverError> {
        Ok(())
    }
}

/// The device-channel transport: dispatches straight into the real driver
/// side of the contract over the fixture device.
struct LoopbackChannel {
    server: Rc<RefCell<AudioChannelServer<FixtureDevice>>>,
    regions: Rc<RefCell<FixtureRegions>>,
}

impl AudioChannelTransport for LoopbackChannel {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        use tairix_abi::driver::audio_channel::AudioChannelRequest as Req;
        let decoded = Req::decode(request)?;
        let mut server = self.server.borrow_mut();
        let body: Vec<u8> = match decoded {
            Req::Facts => server.facts_reply().to_vec(),
            Req::EndpointFacts { endpoint } => server.endpoint_facts_reply(endpoint).to_vec(),
            Req::Configure(params) => server.configure_reply(&params).to_vec(),
            Req::Attach(params) => {
                // The grant handle is the region id here, because the two
                // halves share one address space; the driver side's
                // "mapping" is the same bytes the service holds, which a
                // `Service` binds its ring over.
                let id = RegionId(u32::try_from(params.region_grant).unwrap_or(0));
                let mut regions = self.regions.borrow_mut();
                regions.bytes(id)?;
                regions
                    .device_rings
                    .retain(|(at, _)| *at != params.endpoint);
                regions.device_rings.push((params.endpoint, id));
                server.attach(&params).to_vec()
            }
            Req::Start { endpoint, at } => server.start(endpoint, at).to_vec(),
            Req::Stop { endpoint, at } => server.stop(endpoint, at).to_vec(),
            Req::Drain { endpoint } => server.drain(endpoint).to_vec(),
            Req::Service { endpoint } => {
                let mut regions = self.regions.borrow_mut();
                let id = regions.device_ring(endpoint).ok_or(Errno::NotAttached)?;
                let bytes = regions.bytes(id)?;
                server.service_reply(endpoint, bytes).to_vec()
            }
            Req::Gain {
                endpoint,
                millibel,
                mute,
            } => server.set_gain(endpoint, millibel, mute).to_vec(),
            Req::Detach { endpoint } => server.detach(endpoint).to_vec(),
        };
        let len = body.len().min(reply.len());
        reply[..len].copy_from_slice(&body[..len]);
        Ok(len)
    }
}

/// In-process shared regions: owned buffers, with the grant handle being the
/// region id, since both halves live in one address space here.
struct FixtureRegions {
    /// Each region's id, its backing store, and the aligned `offset..offset +
    /// len` view that *is* the region — exactly the geometry both halves
    /// agreed, since a ring binds nothing larger.
    buffers: Vec<(RegionId, Vec<u8>, usize, usize)>,
    next: u32,
    /// Each endpoint's device ring, as the service attached it, which the
    /// loopback channel's `Service` binds over.
    device_rings: Vec<(u16, RegionId)>,
    /// The client each adopted ring was named as granted by.
    adopted_from: Vec<ProcId>,
    /// A region the host no longer holds, as if its mapping were gone.
    withdrawn: Option<RegionId>,
}

impl FixtureRegions {
    fn new() -> Self {
        Self {
            buffers: Vec::new(),
            next: 1,
            device_rings: Vec::new(),
            adopted_from: Vec::new(),
            withdrawn: None,
        }
    }

    fn slot(&self, region: RegionId) -> Option<usize> {
        self.buffers.iter().position(|(id, ..)| *id == region)
    }

    /// The device ring attached to `endpoint`.
    fn device_ring(&self, endpoint: u16) -> Option<RegionId> {
        self.device_rings
            .iter()
            .find(|(at, _)| *at == endpoint)
            .map(|(_, region)| *region)
    }

    /// Add a buffer of `len` usable bytes, aligned for the ring header.
    fn add(&mut self, len: usize) -> RegionId {
        let id = RegionId(self.next);
        self.next += 1;
        let mut backing = vec![0u8; len + REGION_ALIGN_PADDING];
        let base = backing.as_ptr() as usize;
        let offset = aligned_region(&mut backing, len)
            .map(|view| view.as_ptr() as usize)
            .expect("an aligned view fits the over-allocated buffer")
            - base;
        self.buffers.push((id, backing, offset, len));
        id
    }
}

impl RegionHost for FixtureRegions {
    fn create(&mut self, len: usize) -> Result<RegionId, Errno> {
        Ok(self.add(len))
    }

    fn adopt(&mut self, grantor: ProcId, grant: u64, len: usize) -> Result<RegionId, Errno> {
        let id = RegionId(u32::try_from(grant).map_err(|_| Errno::NotFound)?);
        let slot = self.slot(id).ok_or(Errno::NotFound)?;
        if self.buffers[slot].3 < len {
            return Err(Errno::BufferTooSmall);
        }
        self.adopted_from.push(grantor);
        Ok(id)
    }

    fn grant(&mut self, region: RegionId, _endpoint: u64) -> Result<u64, Errno> {
        self.slot(region).ok_or(Errno::NotFound)?;
        Ok(u64::from(region.0))
    }

    fn bytes(&mut self, region: RegionId) -> Result<&mut [u8], Errno> {
        if self.withdrawn == Some(region) {
            return Err(Errno::NotFound);
        }
        let slot = self.slot(region).ok_or(Errno::NotFound)?;
        let (_, backing, offset, len) = &mut self.buffers[slot];
        let (offset, len) = (*offset, *len);
        Ok(&mut backing[offset..offset + len])
    }

    fn release(&mut self, _region: RegionId) {
        // The fixture keeps every buffer so a test can read what was played
        // after the stream that produced it has gone.
    }
}

/// Every frame the service notified, as (endpoint, frame).
type Notified = Rc<RefCell<Vec<(u64, Vec<u8>)>>>;

/// A notifier that keeps every frame it was handed where the test can read
/// them.
struct Recorder(Notified);

impl Notifier for Recorder {
    fn notify(&mut self, endpoint: u64, frame: &[u8]) {
        self.0.borrow_mut().push((endpoint, frame.to_vec()));
    }
}

/// A monotonic clock that advances a millisecond per reading.
struct TickClock {
    now: RefCell<u64>,
}

impl MonotonicClock for TickClock {
    fn now_ns(&self) -> u64 {
        let mut now = self.now.borrow_mut();
        *now += 1_000_000;
        *now
    }
}

/// The whole fixture: the service plus the halves a test reaches into.
struct Fixture {
    service: AudioService<SharedRegions, Recorder, TickClock>,
    regions: Rc<RefCell<FixtureRegions>>,
    device: Rc<RefCell<AudioChannelServer<FixtureDevice>>>,
    notified: Notified,
    /// Each stream's client ring, by stream id.
    rings: Vec<(u64, RegionId)>,
}

/// The region host the service holds: a handle onto the shared fixture
/// buffers, so the loopback channel can bind the same bytes.
struct SharedRegions(Rc<RefCell<FixtureRegions>>);

impl RegionHost for SharedRegions {
    fn create(&mut self, len: usize) -> Result<RegionId, Errno> {
        self.0.borrow_mut().create(len)
    }

    fn adopt(&mut self, grantor: ProcId, grant: u64, len: usize) -> Result<RegionId, Errno> {
        self.0.borrow_mut().adopt(grantor, grant, len)
    }

    fn grant(&mut self, region: RegionId, endpoint: u64) -> Result<u64, Errno> {
        self.0.borrow_mut().grant(region, endpoint)
    }

    fn bytes(&mut self, region: RegionId) -> Result<&mut [u8], Errno> {
        let mut regions = self.0.borrow_mut();
        let bytes = regions.bytes(region)?;
        let (ptr, len) = (bytes.as_mut_ptr(), bytes.len());
        // SAFETY: `ptr`/`len` name a live subrange of one region's own heap
        // buffer. That buffer outlives the fixture — `FixtureRegions` never
        // drops or reallocates it (`release` is a no-op, and pushing to
        // `buffers` moves only the `Vec` headers, never the allocations they
        // point at) — so the pointer stays valid for the whole test. The
        // engine borrows one region at a time and never holds a previous
        // slice across a later `bytes` call, so no second live `&mut` to
        // these bytes exists. This is the single-address-space stand-in for
        // the two independent `shm` mappings the live service and driver
        // hold of one region, which the `RefCell` borrow alone cannot
        // express because the lifetime would not escape it.
        Ok(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
    }

    fn release(&mut self, region: RegionId) {
        self.0.borrow_mut().release(region);
    }
}

impl Fixture {
    /// A fixture whose seat no presenter holds, so its room is anybody's.
    fn new() -> Self {
        let mut fixture = Self::with_no_seat_read();
        fixture.seat(DisplayLease::UNHELD);
        fixture
    }

    /// A fixture whose sink has a gain control of `range`.
    fn with_control(range: GainRange) -> Self {
        let fixture = Self::new();
        fixture.device.borrow_mut().audio_mut().control = Some(range);
        fixture
    }

    /// A fixture whose service has not yet read the seat's lease.
    fn with_no_seat_read() -> Self {
        let regions = Rc::new(RefCell::new(FixtureRegions::new()));
        let device = Rc::new(RefCell::new(AudioChannelServer::new(FixtureDevice::new())));
        let notified = Notified::default();
        let service = AudioService::new(
            SharedRegions(Rc::clone(&regions)),
            Recorder(Rc::clone(&notified)),
            TickClock {
                now: RefCell::new(0),
            },
        );
        Self {
            service,
            regions,
            device,
            notified,
            rings: Vec::new(),
        }
    }

    /// Move the seat's lease to `lease`.
    fn seat(&mut self, lease: DisplayLease) {
        self.service.seat_changed(lease, &DiscardSink);
    }

    /// The states the service told `grant`'s client of, in order.
    fn told(&self, grant: &StreamGrant) -> Vec<(StreamState, Frames)> {
        self.notified
            .borrow()
            .iter()
            .filter(|(endpoint, _)| *endpoint == grant.notify_endpoint)
            .filter_map(|(_, frame)| match AudioNotify::decode(frame) {
                Ok(AudioNotify::StateChanged {
                    stream_id,
                    state,
                    at,
                    ..
                }) if stream_id == grant.stream_id => Some((state, at)),
                _ => None,
            })
            .collect()
    }

    /// The state `grant`'s stream reports to `owner`.
    fn state(&mut self, owner: &Origin, grant: &StreamGrant) -> StreamState {
        let reply = self.call(
            owner,
            &AudioRequest::State {
                stream_id: grant.stream_id,
            },
        );
        decode_state_reply(&reply).expect("a report").state
    }

    /// Ask for `request` on `owner`'s behalf and expect it granted.
    fn grant(&mut self, owner: &Origin, request: &AudioRequest) {
        let reply = self.call(owner, request);
        decode_status_reply(&reply).expect("granted");
    }

    /// Bind the fixture's one device channel.
    fn bind(&mut self) {
        let transport = LoopbackChannel {
            server: Rc::clone(&self.device),
            regions: Rc::clone(&self.regions),
        };
        self.service
            .bind_device(
                0x4143_4841_4E00_0000,
                FIXTURE_LOCATION,
                0x0A55_0001,
                Box::new(transport),
                &DiscardSink,
            )
            .expect("the fixture channel binds");
    }

    /// Bind a second device, on channel `endpoint` at `location`.
    fn bind_another(&mut self, endpoint: u64, location: u64) {
        let transport = LoopbackChannel {
            server: Rc::new(RefCell::new(AudioChannelServer::new(FixtureDevice::new()))),
            regions: Rc::clone(&self.regions),
        };
        self.service
            .bind_device(
                endpoint,
                location,
                0x0A55_0002,
                Box::new(transport),
                &DiscardSink,
            )
            .expect("another channel binds");
    }

    /// The `direction` endpoint `caller` is shown next after id `after`.
    fn next_device(
        &mut self,
        caller: &Origin,
        direction: StreamDirection,
        after: u32,
    ) -> Result<AudioDeviceDescriptor, Errno> {
        decode_enumerate_reply(&self.call(caller, &AudioRequest::Enumerate { direction, after }))
    }

    /// Every `direction` endpoint `caller` is shown.
    fn devices(
        &mut self,
        caller: &Origin,
        direction: StreamDirection,
    ) -> Vec<AudioDeviceDescriptor> {
        let mut shown: Vec<AudioDeviceDescriptor> = Vec::new();
        while let Ok(device) = self.next_device(
            caller,
            direction,
            shown.last().map_or(0, |device| device.device_id),
        ) {
            shown.push(device);
        }
        shown
    }

    /// The sink `caller` is shown with id `device_id`.
    fn sink(&mut self, caller: &Origin, device_id: u32) -> AudioDeviceDescriptor {
        self.devices(caller, StreamDirection::Playback)
            .into_iter()
            .find(|device| device.device_id == device_id)
            .expect("the sink is shown")
    }

    /// Ask for a device control on `caller`'s behalf.
    fn control(&mut self, caller: &Origin, request: &AudioRequest) -> Result<(), Errno> {
        decode_status_reply(&self.call(caller, request))
    }

    /// Serve one request and answer the reply bytes.
    fn call(&mut self, caller: &Origin, request: &AudioRequest) -> Vec<u8> {
        let mut frame = [0u8; AUDIO_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("encoded");
        let mut reply = [0u8; AUDIO_MAX_REPLY];
        let reply_len = self
            .service
            .handle(caller, &frame[..len], &mut reply, &DiscardSink);
        reply[..reply_len].to_vec()
    }

    /// Open a playback stream and adopt a ring for it.
    fn open_playback(&mut self, caller: &Origin, hz: u32, latency: u32) -> StreamGrant {
        self.open_role(caller, StreamRole::Media, hz, latency)
    }

    /// Open a playback stream of `role` and adopt a ring for it.
    fn open_role(
        &mut self,
        caller: &Origin,
        role: StreamRole,
        hz: u32,
        latency: u32,
    ) -> StreamGrant {
        let reply = self.call(
            caller,
            &AudioRequest::Open(OpenParams {
                device_id: 0,
                direction: StreamDirection::Playback,
                format: SampleFormat::S16,
                rate: rate(hz),
                channel_map: ChannelMap::STEREO,
                role,
                latency_target_frames: latency,
            }),
        );
        let grant = decode_open_reply(&reply).expect("the open is granted");
        let geometry = PcmGeometry::new(grant.ring_frames, grant.format, 2).expect("geometry");
        let region = self.regions.borrow_mut().add(geometry.region_len());
        let reply = self.call(
            caller,
            &AudioRequest::Attach {
                stream_id: grant.stream_id,
                region_grant: u64::from(region.0),
            },
        );
        decode_status_reply(&reply).expect("the ring is adopted");
        assert_eq!(
            self.regions.borrow().adopted_from.last(),
            Some(&caller.proc_id()),
            "the ring is adopted as the calling client's own"
        );
        self.rings.push((grant.stream_id, region));
        grant
    }

    /// The client ring `grant`'s stream was opened with.
    fn ring_of(&self, grant: &StreamGrant) -> RegionId {
        self.rings
            .iter()
            .find(|(stream_id, _)| *stream_id == grant.stream_id)
            .map(|(_, region)| *region)
            .expect("a client ring")
    }

    /// Write `samples` into the stream's ring, which the client owns.
    fn client_write(&mut self, grant: &StreamGrant, samples: &[u8]) -> u32 {
        let geometry = PcmGeometry::new(grant.ring_frames, grant.format, 2).expect("geometry");
        let id = self.ring_of(grant);
        let mut regions = self.regions.borrow_mut();
        let bytes = regions.bytes(id).expect("mapped");
        let mut ring = PcmRing::bind(bytes, geometry).expect("ring");
        ring.write(samples).expect("written")
    }

    /// Run one device period exactly as the driver's interrupt path does:
    /// the driver services its own ring and *then* wakes the mixer with the
    /// clock pair, which is why a period notify means "refill", not "move
    /// frames for me". A device that is not clocking interrupts nobody.
    fn period(&mut self) {
        self.period_on(0);
    }

    /// One device period of `endpoint`.
    fn period_on(&mut self, endpoint: u16) {
        let Some(serviced) = self.service_only(endpoint) else {
            return;
        };
        let frame = tairix_abi::driver::audio_channel::AudioChannelNotify::PeriodElapsed {
            endpoint,
            position: serviced.report.position,
            sampled_at: serviced.report.sampled_at,
        }
        .encode();
        self.service.on_device_notify(0, &frame, &DiscardSink);
        if !serviced.report.running {
            let done = tairix_abi::driver::audio_channel::AudioChannelNotify::Drained {
                endpoint,
                position: serviced.report.position,
            }
            .encode();
            self.service.on_device_notify(0, &done, &DiscardSink);
        }
    }

    /// The driver servicing `endpoint`'s ring with the mixer not yet told: a
    /// device moving frames in the moment before its notification lands.
    fn service_only(&mut self, endpoint: u16) -> Option<tairix_audiochan::Serviced> {
        if !self.device.borrow().audio().running {
            return None;
        }
        let mut regions = self.regions.borrow_mut();
        let id = regions.device_ring(endpoint)?;
        let bytes = regions.bytes(id).ok()?;
        self.device.borrow_mut().service(endpoint, bytes).ok()
    }

    /// Adopt a client ring for an opened capture stream.
    fn attach_capture(&mut self, owner: &Origin, grant: &StreamGrant) {
        let geometry = PcmGeometry::new(grant.ring_frames, grant.format, 2).expect("geometry");
        let region = self.regions.borrow_mut().add(geometry.region_len());
        self.grant(
            owner,
            &AudioRequest::Attach {
                stream_id: grant.stream_id,
                region_grant: u64::from(region.0),
            },
        );
        self.rings.push((grant.stream_id, region));
    }

    /// Everything the stream's client ring holds, as its client reads it.
    fn client_read(&mut self, grant: &StreamGrant) -> Vec<u8> {
        let geometry = PcmGeometry::new(grant.ring_frames, grant.format, 2).expect("geometry");
        let id = self.ring_of(grant);
        let mut regions = self.regions.borrow_mut();
        let bytes = regions.bytes(id).expect("mapped");
        let mut ring = PcmRing::bind(bytes, geometry).expect("ring");
        let mut out = vec![0u8; ring.readable_frames().expect("frames") as usize * 4];
        let taken = ring.read(&mut out).expect("read") as usize;
        out.truncate(taken * 4);
        out
    }

    /// What the device was handed.
    fn played(&self) -> Vec<u8> {
        self.device.borrow().audio().played.clone()
    }
}

/// A distinct attested instance for each pid.
fn instance_of(pid: u64) -> ProcId {
    let mut raw = [0x5Au8; tairix_abi::origin::PROC_ID_LEN];
    raw[..8].copy_from_slice(&pid.to_le_bytes());
    ProcId::from_raw(raw)
}

/// A caller with the given pid and capability set, in no login session.
fn caller(pid: u64, caps: &[CapabilityId]) -> Origin {
    let mut summary = CapabilitySummary::EMPTY;
    for cap in caps {
        summary.insert(*cap);
    }
    Origin::new(
        TrustDomain::User,
        1_000,
        1_000,
        pid,
        instance_of(pid),
        summary,
        0,
    )
}

/// The login session tagged `tag`.
fn session(tag: u8) -> ProcId {
    ProcId::from_raw([tag; tairix_abi::origin::PROC_ID_LEN])
}

/// A caller with no capability, in the login session tagged `tag`.
fn in_session(pid: u64, tag: u8) -> Origin {
    caller(pid, &[]).with_login_session(session(tag))
}

/// A second deterministic stereo `s16` signal, unlike [`signal`] in every
/// frame, so whose frames reached the device is unambiguous.
fn other_signal(frames: usize) -> Vec<u8> {
    let mut samples = Vec::with_capacity(frames * 4);
    for frame in 0..frames {
        let value = 3_000 + i16::try_from(frame % 500).unwrap_or(0);
        samples.extend_from_slice(&value.to_le_bytes());
        samples.extend_from_slice(&value.to_le_bytes());
    }
    samples
}

/// A deterministic stereo `s16` signal of `frames` frames.
fn signal(frames: usize) -> Vec<u8> {
    let mut samples = Vec::with_capacity(frames * 4);
    for frame in 0..frames {
        let left = i16::try_from(i32::try_from(frame % 2_000).unwrap_or(0) - 1_000).unwrap_or(0);
        let right = left.wrapping_neg();
        samples.extend_from_slice(&left.to_le_bytes());
        samples.extend_from_slice(&right.to_le_bytes());
    }
    samples
}

// ------------------------------------------------------------------ tests

#[test]
fn a_bound_channel_enumerates_its_sink_and_its_source() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let sink = fixture
        .next_device(&caller, StreamDirection::Playback, 0)
        .expect("a sink");
    assert_eq!(sink.direction, StreamDirection::Playback);
    assert!(
        sink.default.is_default(),
        "the only sink is the machine's default"
    );
    assert_eq!(
        fixture.next_device(&caller, StreamDirection::Playback, sink.device_id),
        Err(Errno::NotFound)
    );
    let source = fixture
        .next_device(&caller, StreamDirection::Capture, 0)
        .expect("a source");
    assert_ne!(source.device_id, sink.device_id);
}

/// A walk is keyed by id, so a device lost partway through it costs the walk
/// that device and no other.
#[test]
fn a_device_lost_during_a_walk_costs_it_no_other() {
    let mut fixture = Fixture::new();
    fixture.bind();
    fixture.bind_another(0x4143_4841_4E00_0002, 0x51e7);
    let anyone = caller(9, &[]);
    let first = fixture
        .next_device(&anyone, StreamDirection::Playback, 0)
        .expect("the first sink");
    let faulted = tairix_abi::driver::audio_channel::AudioChannelNotify::Faulted {
        endpoint: 0,
        reason: Errno::DeviceFault,
    }
    .encode();
    fixture.service.on_device_notify(0, &faulted, &DiscardSink);
    let second = fixture
        .next_device(&anyone, StreamDirection::Playback, first.device_id)
        .expect("the sink that stayed");
    assert_ne!(second.location, first.location);
    assert_eq!(
        fixture.next_device(&anyone, StreamDirection::Playback, second.device_id),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_repeated_hand_off_of_one_channel_is_not_a_second_device() {
    let mut fixture = Fixture::new();
    fixture.bind();
    fixture.bind();
    assert_eq!(fixture.service.device_count(), 1);
}

#[test]
fn a_single_stream_at_unity_reaches_the_device_bit_exact() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    assert_eq!(grant.rate, rate(DEVICE_HZ));
    assert_eq!(grant.format, SampleFormat::S16);
    assert_eq!(grant.ring_frames, 2_048);

    let samples = signal(1_500);
    assert_eq!(fixture.client_write(&grant, &samples), 1_500);
    let reply = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");
    let reply = fixture.call(
        &caller,
        &AudioRequest::Drain {
            stream_id: grant.stream_id,
        },
    );
    decode_status_reply(&reply).expect("draining");
    // Each period the device consumes wakes the service, which refills.
    for _ in 0..16 {
        fixture.period();
    }
    let played = fixture.played();
    assert_eq!(
        played.len(),
        samples.len(),
        "the drain's tail is the frames the stream had, never a padded period"
    );
    assert!(
        played == samples,
        "a stereo s16 source at the device's own rate and unity gain must \
         reach the hardware byte for byte"
    );
    let reply = fixture.call(
        &caller,
        &AudioRequest::State {
            stream_id: grant.stream_id,
        },
    );
    let report = decode_state_reply(&reply).expect("a report");
    assert_eq!(report.state, StreamState::Idle, "the drain completed");
    assert_eq!(report.xrun_frames, 0, "nothing was lost");
}

#[test]
fn a_running_stream_with_nothing_queued_takes_the_underrun() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    fixture.client_write(&grant, &signal(100));
    let reply = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");
    fixture.period();
    let reply = fixture.call(
        &caller,
        &AudioRequest::State {
            stream_id: grant.stream_id,
        },
    );
    let report = decode_state_reply(&reply).expect("a report");
    assert!(
        report.xrun_frames >= u64::from(PERIOD) - 100,
        "the frames the stream could not supply are accounted, not hidden: \
         {report:?}"
    );
    assert!(report.xruns >= 1);
}

#[test]
fn a_rate_the_device_cannot_meet_is_filtered_rather_than_refused() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    // The fixture sink admits 48 kHz alone, so a 24 kHz client is resampled.
    let grant = fixture.open_playback(&caller, 24_000, 2_048);
    assert_eq!(grant.rate, rate(24_000), "the client keeps its own rate");
    fixture.client_write(&grant, &signal(1_024));
    let reply = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");
    for _ in 0..16 {
        fixture.period();
    }
    let played = fixture.played().len() / 4;
    assert!(
        played >= 1_800,
        "1024 frames at 24 kHz should reach the 48 kHz device as about 2048 \
         frames, not {played}"
    );
}

#[test]
fn a_capture_open_is_refused_without_the_capability_and_granted_with_it() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let params = OpenParams {
        device_id: 0,
        direction: StreamDirection::Capture,
        format: SampleFormat::S16,
        rate: rate(DEVICE_HZ),
        channel_map: ChannelMap::STEREO,
        role: StreamRole::Media,
        latency_target_frames: 2_048,
    };
    let plain = caller(7, &[]);
    let reply = fixture.call(&plain, &AudioRequest::Open(params));
    assert_eq!(decode_open_reply(&reply), Err(Errno::PermissionDenied));

    let recorder = caller(8, &[CapabilityId::AUDIO_CAPTURE]);
    let reply = fixture.call(&recorder, &AudioRequest::Open(params));
    let grant = decode_open_reply(&reply).expect("the capture open is granted");
    assert_eq!(grant.rate, rate(DEVICE_HZ));
}

#[test]
fn a_stream_that_is_not_the_callers_is_unreachable() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let owner = caller(7, &[]);
    let grant = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    let stranger = caller(9, &[]);
    for request in [
        AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
        AudioRequest::Close {
            stream_id: grant.stream_id,
        },
        AudioRequest::State {
            stream_id: grant.stream_id,
        },
    ] {
        let reply = fixture.call(&stranger, &request);
        assert_eq!(
            decode_status_reply(&reply),
            Err(Errno::NotFound),
            "a guessed stream id must reach nothing"
        );
    }
}

#[test]
fn the_driver_bind_operation_is_not_reachable_through_the_client_surface() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let reply = fixture.call(
        &caller,
        &AudioRequest::BindDriver {
            endpoint_id: 0x4143_4841_4E00_0001,
            location: FIXTURE_LOCATION,
        },
    );
    assert_eq!(decode_status_reply(&reply), Err(Errno::NotSupported));
    assert_eq!(fixture.service.device_count(), 1);
}

#[test]
fn a_latency_target_earns_a_power_of_two_ring_that_holds_it() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    // A target that is not a power of two rounds up; the grant reports both
    // the ring and the latency it bought.
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 3_000);
    assert_eq!(grant.ring_frames, 4_096);
    // One device period of the ring is the mixer's own contribution and the
    // rest is the client's queue, so the granted latency is the whole ring.
    assert_eq!(grant.granted_latency_frames, 4_096);
    assert_eq!(
        grant.granted_latency,
        Frames::new(4_096).duration(rate(DEVICE_HZ))
    );
}

#[test]
fn a_clock_read_before_the_first_period_refuses_rather_than_inventing_one() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    let reply = fixture.call(
        &caller,
        &AudioRequest::Clock {
            stream_id: grant.stream_id,
        },
    );
    assert_eq!(
        tairix_abi::audio::decode_clock_reply(&reply),
        Err(Errno::WouldBlock)
    );
}

#[test]
fn closing_the_last_stream_releases_the_endpoint() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    assert_eq!(fixture.service.stream_count(), 1);
    let reply = fixture.call(
        &caller,
        &AudioRequest::Close {
            stream_id: grant.stream_id,
        },
    );
    decode_status_reply(&reply).expect("closed");
    assert_eq!(fixture.service.stream_count(), 0);
    assert!(
        fixture.device.borrow().audio().configured.is_none(),
        "the device is released with its last stream"
    );
}

#[test]
fn starting_a_sink_hands_the_device_its_first_periods() {
    // The device only interrupts once it has completed a transfer, and the
    // driver only moves frames when it is serviced. Filling the shared ring
    // is therefore not priming: unless `Start` also posts, the device is
    // clocked with an empty queue and nothing ever wakes anybody. Note that
    // no `period()` is driven here — the fixture's period stands in for the
    // driver's own interrupt path, which cannot fire before a first post.
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    let samples = signal(1_500);
    assert_eq!(fixture.client_write(&grant, &samples), 1_500);

    let reply = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");

    assert!(
        !fixture.played().is_empty(),
        "the device was clocked with nothing queued, so it can never interrupt"
    );
}

#[test]
fn a_drain_completes_only_once_the_device_has_played_out() {
    // The frames the mixer hands over sit in the driver's own transfers
    // long after the shared ring runs dry. Completing the drain when the
    // ring empties cuts the tail off every sound; the stream stays
    // `Draining` until the device says it played out.
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    let samples = signal(1_500);
    assert_eq!(fixture.client_write(&grant, &samples), 1_500);

    let reply = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");
    let reply = fixture.call(
        &caller,
        &AudioRequest::Drain {
            stream_id: grant.stream_id,
        },
    );
    decode_status_reply(&reply).expect("draining");

    let state = decode_state_reply(&fixture.call(
        &caller,
        &AudioRequest::State {
            stream_id: grant.stream_id,
        },
    ))
    .expect("state");
    assert_eq!(
        state.state,
        StreamState::Draining,
        "asking the device to drain is not the device having drained"
    );

    for _ in 0..16 {
        fixture.period();
    }
    let state = decode_state_reply(&fixture.call(
        &caller,
        &AudioRequest::State {
            stream_id: grant.stream_id,
        },
    ))
    .expect("state");
    assert_eq!(state.state, StreamState::Idle, "the device played it out");
}

#[test]
fn a_paused_stream_resumes_into_the_configuration_it_left() {
    // Pausing the only stream on an endpoint stops the device's clock once it
    // has played out what it was handed; the configuration is what the resume
    // plays into, so it is kept until the stream closes, and every frame plays
    // once, in order, across the pause.
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    let samples = signal(1_500);
    let (before, after) = samples.split_at(1_000 * 4);
    assert_eq!(fixture.client_write(&grant, before), 1_000);
    let started = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&started).expect("started");
    let paused = fixture.call(
        &caller,
        &AudioRequest::Stop {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&paused).expect("paused");
    let state = |fixture: &mut Fixture| {
        decode_state_reply(&fixture.call(
            &caller,
            &AudioRequest::State {
                stream_id: grant.stream_id,
            },
        ))
        .expect("a report")
    };
    assert_eq!(state(&mut fixture).state, StreamState::Paused);
    assert!(
        fixture.device.borrow().audio().configured.is_some(),
        "a paused stream keeps the device it resumes into"
    );
    assert!(
        fixture.device.borrow().audio().draining,
        "what the device was handed before the pause plays out"
    );
    run(&mut fixture, 1);
    assert!(
        !fixture.device.borrow().audio().running,
        "then its clock stops while the only stream is paused"
    );
    let heard = fixture.played().len();
    run(&mut fixture, 4);
    assert_eq!(fixture.played().len(), heard, "and nothing more plays");

    assert_eq!(fixture.client_write(&grant, after), 500);
    let paused_at = state(&mut fixture).changed_at;
    let resumed = fixture.call(
        &caller,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: paused_at,
        },
    );
    decode_status_reply(&resumed).expect("resumed");
    let drained = fixture.call(
        &caller,
        &AudioRequest::Drain {
            stream_id: grant.stream_id,
        },
    );
    decode_status_reply(&drained).expect("draining");
    for _ in 0..16 {
        fixture.period();
    }
    assert!(
        fixture.played() == samples,
        "every frame plays once and in order across the pause"
    );
    let report = state(&mut fixture);
    assert_eq!(report.state, StreamState::Idle);
    assert_eq!(
        report.xrun_frames, 0,
        "nothing ran dry, so nothing was padded"
    );
}

#[test]
fn the_last_stream_closing_gives_the_device_back() {
    // Holding a configured stream and its shared region after the last
    // client has gone keeps the hardware open for the life of the driver.
    let mut fixture = Fixture::new();
    fixture.bind();
    let caller = caller(7, &[]);
    let grant = fixture.open_playback(&caller, DEVICE_HZ, 2_048);
    assert!(fixture.device.borrow().audio().configured.is_some());

    let reply = fixture.call(
        &caller,
        &AudioRequest::Close {
            stream_id: grant.stream_id,
        },
    );
    decode_status_reply(&reply).expect("closed");

    assert!(
        fixture.device.borrow().audio().configured.is_none(),
        "the device stream was released"
    );
}

/// A capture stream for `recorder` on the fixture device.
fn open_capture(fixture: &mut Fixture, recorder: &Origin) -> StreamGrant {
    let reply = fixture.call(
        recorder,
        &AudioRequest::Open(OpenParams {
            device_id: 0,
            direction: StreamDirection::Capture,
            format: SampleFormat::S16,
            rate: rate(DEVICE_HZ),
            channel_map: ChannelMap::STEREO,
            role: StreamRole::Media,
            latency_target_frames: 2_048,
        }),
    );
    decode_open_reply(&reply).expect("the capture open is granted")
}

/// Every stream `streams` names, each asked about by its owner, is lost.
fn assert_lost(fixture: &mut Fixture, streams: &[(&Origin, u64)]) {
    for &(owner, stream_id) in streams {
        let state = decode_state_reply(&fixture.call(owner, &AudioRequest::State { stream_id }))
            .expect("state");
        assert_eq!(
            state.state,
            StreamState::DeviceLost,
            "stream {stream_id:#x}: nothing will wake its device again"
        );
    }
}

#[test]
fn a_driver_fault_loses_the_device_and_every_stream_on_it() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let player = caller(7, &[]);
    let playback = fixture.open_playback(&player, DEVICE_HZ, 2_048);
    let recorder = caller(8, &[CapabilityId::AUDIO_CAPTURE]);
    let capture = open_capture(&mut fixture, &recorder);
    let fault = tairix_abi::driver::audio_channel::AudioChannelNotify::Faulted {
        endpoint: 0,
        reason: Errno::NoBandwidth,
    }
    .encode();
    fixture.service.on_device_notify(0, &fault, &DiscardSink);
    assert_lost(
        &mut fixture,
        &[
            (&player, playback.stream_id),
            (&recorder, capture.stream_id),
        ],
    );
}

#[test]
fn a_pump_fault_loses_the_streams_on_the_devices_other_endpoints_too() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let player = caller(7, &[]);
    let playback = fixture.open_playback(&player, DEVICE_HZ, 2_048);
    let playback_ring = fixture.regions.borrow().device_ring(0);
    let recorder = caller(8, &[CapabilityId::AUDIO_CAPTURE]);
    let capture = open_capture(&mut fixture, &recorder);
    fixture.client_write(&playback, &signal(4_096));
    let reply = fixture.call(
        &player,
        &AudioRequest::Start {
            stream_id: playback.stream_id,
            at: Frames::ZERO,
        },
    );
    decode_status_reply(&reply).expect("started");
    fixture.regions.borrow_mut().withdrawn = playback_ring;
    let frame = tairix_abi::driver::audio_channel::AudioChannelNotify::PeriodElapsed {
        endpoint: 0,
        position: Frames::ZERO,
        sampled_at: tairix_abi::time::Time64::from_nanos(1),
    }
    .encode();
    fixture.service.on_device_notify(0, &frame, &DiscardSink);
    assert_lost(
        &mut fixture,
        &[
            (&player, playback.stream_id),
            (&recorder, capture.stream_id),
        ],
    );
}

/// Bytes of `frames` stereo `s16` frames.
fn bytes(frames: Frames) -> usize {
    usize::try_from(frames.get()).expect("a test-sized position") * 4
}

/// Start `grant`'s stream at its first frame on `owner`'s behalf.
fn start(fixture: &mut Fixture, owner: &Origin, grant: &StreamGrant) {
    fixture.grant(
        owner,
        &AudioRequest::Start {
            stream_id: grant.stream_id,
            at: Frames::ZERO,
        },
    );
}

/// Drain `grant`'s stream on `owner`'s behalf.
fn drain(fixture: &mut Fixture, owner: &Origin, grant: &StreamGrant) {
    fixture.grant(
        owner,
        &AudioRequest::Drain {
            stream_id: grant.stream_id,
        },
    );
}

/// Run `periods` device periods.
fn run(fixture: &mut Fixture, periods: usize) {
    for _ in 0..periods {
        fixture.period();
    }
}

/// A switch away holds the departing session's music on a frame, plays out
/// only what the device was already handed, lets the arriving session play,
/// and a switch back resumes on the very frame: every frame reaches the
/// device once, in order, and none of it in the other session's turn.
#[test]
fn a_session_outside_the_room_is_held_at_a_frame_and_resumes_exactly() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (alice, bob) = (in_session(7, 0xA1), in_session(8, 0xB0));
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    let samples = signal(3_000);
    assert_eq!(fixture.client_write(&music, &samples), 3_000);
    start(&mut fixture, &alice, &music);
    run(&mut fixture, 1);

    fixture.seat(DisplayLease::held(2, session(0xB0)));
    let &(state, held_at) = fixture.told(&music).last().expect("told");
    assert_eq!(state, StreamState::SeatInactive);
    assert!(
        held_at.get() > 0 && held_at.get() < 3_000,
        "held mid-stream"
    );
    assert!(
        fixture.device.borrow().audio().draining,
        "what the device holds plays out rather than waiting for the next room"
    );
    run(&mut fixture, 4);
    assert!(
        fixture.played() == samples[..bytes(held_at)],
        "the device played exactly the frames before the hold"
    );

    let reply = fixture.call(
        &bob,
        &AudioRequest::State {
            stream_id: music.stream_id,
        },
    );
    assert_eq!(
        decode_state_reply(&reply),
        Err(Errno::NotFound),
        "not Bob's"
    );
    let call = fixture.open_playback(&bob, DEVICE_HZ, 4_096);
    let speech = other_signal(1_000);
    fixture.client_write(&call, &speech);
    start(&mut fixture, &bob, &call);
    drain(&mut fixture, &bob, &call);
    run(&mut fixture, 8);
    assert_eq!(fixture.state(&bob, &call), StreamState::Idle);
    assert_eq!(fixture.state(&alice, &music), StreamState::SeatInactive);

    fixture.seat(DisplayLease::held(3, session(0xA1)));
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::Running, held_at)),
        "resumed on the frame it was held at"
    );
    drain(&mut fixture, &alice, &music);
    run(&mut fixture, 16);
    assert_eq!(fixture.state(&alice, &music), StreamState::Idle);

    let mut expected = samples[..bytes(held_at)].to_vec();
    expected.extend_from_slice(&speech);
    expected.extend_from_slice(&samples[bytes(held_at)..]);
    assert!(
        fixture.played() == expected,
        "every frame once, in order, and none in the other session's turn"
    );
}

/// A stop scheduled before the start ends the segment on the frame it named,
/// so a client can bound what plays without racing the device; a stop the
/// start has already passed is stale and ends nothing.
#[test]
fn a_stop_scheduled_before_the_start_ends_the_segment_on_its_frame() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let player = caller(7, &[]);
    let music = fixture.open_playback(&player, DEVICE_HZ, 4_096);
    let samples = signal(3_000);
    fixture.client_write(&music, &samples);
    let end = Frames::new(1_000);
    fixture.grant(
        &player,
        &AudioRequest::Stop {
            stream_id: music.stream_id,
            at: end,
        },
    );
    start(&mut fixture, &player, &music);
    run(&mut fixture, 8);
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::Paused, end))
    );
    assert!(
        fixture.played() == samples[..bytes(end)],
        "exactly the segment named"
    );

    let other = caller(8, &[]);
    let late = fixture.open_playback(&other, DEVICE_HZ, 4_096);
    fixture.client_write(&late, &samples);
    fixture.grant(
        &other,
        &AudioRequest::Stop {
            stream_id: late.stream_id,
            at: end,
        },
    );
    fixture.grant(
        &other,
        &AudioRequest::Start {
            stream_id: late.stream_id,
            at: Frames::new(2_000),
        },
    );
    run(&mut fixture, 8);
    assert_eq!(
        fixture.state(&other, &late),
        StreamState::Running,
        "a stop behind the start is stale"
    );
}

/// A paused stream holds nothing in flight the room could play, yet its
/// owner is told the room left and told again when it returns: a paused
/// player can say why playing on would wait.
#[test]
fn a_paused_stream_is_told_the_room_left_and_returned() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let alice = in_session(7, 0xA1);
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(3_000));
    start(&mut fixture, &alice, &music);
    let at = Frames::new(512);
    fixture.grant(
        &alice,
        &AudioRequest::Stop {
            stream_id: music.stream_id,
            at,
        },
    );
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::Paused, at))
    );

    fixture.seat(DisplayLease::ended(1, ReleaseSurface::Handover));
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::SeatInactive, at))
    );
    fixture.grant(
        &alice,
        &AudioRequest::Start {
            stream_id: music.stream_id,
            at,
        },
    );
    assert_eq!(
        fixture.state(&alice, &music),
        StreamState::SeatInactive,
        "played on outside the room, it waits for the room"
    );
    fixture.seat(DisplayLease::held(2, session(0xA1)));
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::Running, at))
    );
}

/// A notification outside the room is noise by the time the room is its own
/// again: what it had queued, and what it wrote while outside, never plays,
/// and the jump in its position says exactly which frames went.
#[test]
fn a_notification_outside_the_room_is_dropped_not_queued() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (alice, bob) = (in_session(7, 0xA1), in_session(8, 0xB0));
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let chime = fixture.open_role(&alice, StreamRole::Notification, DEVICE_HZ, 4_096);
    let samples = signal(3_000);
    fixture.client_write(&chime, &samples);
    start(&mut fixture, &alice, &chime);
    let heard = fixture.played().len();
    assert!(heard > 0 && heard < samples.len());

    fixture.seat(DisplayLease::held(2, session(0xB0)));
    assert_eq!(
        fixture.told(&chime).last(),
        Some(&(StreamState::SeatInactive, Frames::new(3_000))),
        "everything queued was dropped"
    );
    fixture.client_write(&chime, &signal(500));
    run(&mut fixture, 4);

    fixture.seat(DisplayLease::held(3, session(0xA1)));
    assert_eq!(
        fixture.told(&chime).last(),
        Some(&(StreamState::Running, Frames::new(3_500))),
        "and so was what it wrote outside the room"
    );
    run(&mut fixture, 4);
    assert!(
        fixture.played()[..heard] == samples[..heard]
            && fixture.played()[heard..].iter().all(|&byte| byte == 0),
        "nothing it queued outside the room is heard"
    );

    // Opened in its own room, a notification may not start in another's.
    fixture.seat(DisplayLease::held(4, session(0xB0)));
    let ping = fixture.open_role(&bob, StreamRole::Notification, DEVICE_HZ, 4_096);
    fixture.seat(DisplayLease::held(5, session(0xA1)));
    let reply = fixture.call(
        &bob,
        &AudioRequest::Start {
            stream_id: ping.stream_id,
            at: Frames::ZERO,
        },
    );
    assert_eq!(decode_status_reply(&reply), Err(Errno::SeatNotOwner));
}

/// A notification the room leaves while it plays out its tail has nothing
/// left to play: its drain is over, not held.
#[test]
fn a_notification_draining_when_the_room_leaves_it_is_over() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let alice = in_session(7, 0xA1);
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let chime = fixture.open_role(&alice, StreamRole::Notification, DEVICE_HZ, 4_096);
    fixture.client_write(&chime, &signal(3_000));
    start(&mut fixture, &alice, &chime);
    drain(&mut fixture, &alice, &chime);
    fixture.seat(DisplayLease::ended(1, ReleaseSurface::Handover));
    assert_eq!(fixture.state(&alice, &chime), StreamState::Idle);
}

/// Between one presenter and the next nobody plays, and a presenter no login
/// encloses — the login screen — claims the room for nobody.
#[test]
fn nobody_plays_through_a_handover_or_under_the_login_screen() {
    for lease in [
        DisplayLease::ended(1, ReleaseSurface::Handover),
        DisplayLease::held(2, ProcId::KERNEL),
    ] {
        let mut fixture = Fixture::new();
        fixture.bind();
        let alice = in_session(7, 0xA1);
        fixture.seat(lease);
        let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
        fixture.client_write(&music, &signal(1_000));
        start(&mut fixture, &alice, &music);
        assert_eq!(fixture.state(&alice, &music), StreamState::SeatInactive);
        assert!(fixture.played().is_empty(), "{lease:?}");
    }
}

/// Until the seat has been read the room is nobody's: a stream opens, and is
/// held rather than played into a room it may not be in.
#[test]
fn a_service_that_has_not_read_the_seat_holds_every_stream() {
    let mut fixture = Fixture::with_no_seat_read();
    fixture.bind();
    let player = caller(7, &[]);
    let music = fixture.open_playback(&player, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(1_000));
    start(&mut fixture, &player, &music);
    assert_eq!(fixture.state(&player, &music), StreamState::SeatInactive);
    assert!(!fixture.device.borrow().audio().running);
    fixture.seat(DisplayLease::UNHELD);
    assert_eq!(fixture.state(&player, &music), StreamState::Running);
}

/// A stream that goes live while the device is playing out another's drain
/// is clocked again when the drain completes, rather than left on a stopped
/// device it would wait on for ever.
#[test]
fn a_stream_that_goes_live_while_the_device_drains_is_clocked_again() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (first, second) = (caller(7, &[]), caller(8, &[]));
    let ending = fixture.open_playback(&first, DEVICE_HZ, 4_096);
    fixture.client_write(&ending, &signal(600));
    start(&mut fixture, &first, &ending);
    drain(&mut fixture, &first, &ending);
    assert!(fixture.device.borrow().audio().draining);

    let starting = fixture.open_playback(&second, DEVICE_HZ, 4_096);
    let speech = other_signal(2_000);
    fixture.client_write(&starting, &speech);
    start(&mut fixture, &second, &starting);
    run(&mut fixture, 1);
    assert_eq!(fixture.state(&first, &ending), StreamState::Idle);
    assert!(fixture.device.borrow().audio().running, "clocked again");

    drain(&mut fixture, &second, &starting);
    run(&mut fixture, 16);
    assert_eq!(fixture.state(&second, &starting), StreamState::Idle);
}

/// A client that corrupts its own ring stops its own stream and nothing
/// else: every other principal's sound, and the device, carry on.
#[test]
fn a_corrupt_client_ring_faults_its_own_stream_and_nothing_else() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (vandal, bystander) = (caller(7, &[]), caller(8, &[]));
    let broken = fixture.open_playback(&vandal, DEVICE_HZ, 4_096);
    let music = fixture.open_playback(&bystander, DEVICE_HZ, 4_096);
    fixture.client_write(&broken, &signal(2_000));
    fixture.client_write(&music, &other_signal(2_000));
    start(&mut fixture, &vandal, &broken);
    start(&mut fixture, &bystander, &music);
    let ring = fixture.ring_of(&broken);
    // A producer position past everything the ring could hold.
    fixture.regions.borrow_mut().bytes(ring).expect("mapped")[..8].fill(0xFF);
    run(&mut fixture, 1);

    assert_eq!(fixture.state(&vandal, &broken), StreamState::Faulted);
    assert_eq!(fixture.state(&bystander, &music), StreamState::Running);
    for request in [
        AudioRequest::Start {
            stream_id: broken.stream_id,
            at: Frames::ZERO,
        },
        AudioRequest::Stop {
            stream_id: broken.stream_id,
            at: Frames::ZERO,
        },
    ] {
        let reply = fixture.call(&vandal, &request);
        assert_eq!(decode_status_reply(&reply), Err(Errno::BadMagic));
    }
    drain(&mut fixture, &bystander, &music);
    run(&mut fixture, 16);
    assert_eq!(fixture.state(&bystander, &music), StreamState::Idle);
}

/// A stream on a lost device cannot be stopped back into a state that
/// claims it could play again.
#[test]
fn a_lost_stream_cannot_be_stopped_back_to_life() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let player = caller(7, &[]);
    let music = fixture.open_playback(&player, DEVICE_HZ, 2_048);
    let fault = tairix_abi::driver::audio_channel::AudioChannelNotify::Faulted {
        endpoint: 0,
        reason: Errno::NoBandwidth,
    }
    .encode();
    fixture.service.on_device_notify(0, &fault, &DiscardSink);
    let reply = fixture.call(
        &player,
        &AudioRequest::Stop {
            stream_id: music.stream_id,
            at: Frames::ZERO,
        },
    );
    assert_eq!(decode_status_reply(&reply), Err(Errno::DeviceOffline));
    assert_eq!(fixture.state(&player, &music), StreamState::DeviceLost);
}

/// The capture count the recording indicator is drawn from is the streams
/// genuinely hearing the room: started, not held, not stopped.
#[test]
fn the_capture_count_is_the_streams_hearing_the_room() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let recorder = caller(8, &[CapabilityId::AUDIO_CAPTURE]).with_login_session(session(0xA1));
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let capture = open_capture(&mut fixture, &recorder);
    fixture.attach_capture(&recorder, &capture);
    assert_eq!(fixture.service.live_captures(), 0, "opened, not started");
    start(&mut fixture, &recorder, &capture);
    assert_eq!(fixture.service.live_captures(), 1);
    fixture.seat(DisplayLease::held(2, session(0xB0)));
    assert_eq!(fixture.service.live_captures(), 0, "held outside its room");
    assert_eq!(
        fixture.state(&recorder, &capture),
        StreamState::SeatInactive
    );
    fixture.seat(DisplayLease::held(3, session(0xA1)));
    assert_eq!(fixture.service.live_captures(), 1);
    fixture.grant(
        &recorder,
        &AudioRequest::Close {
            stream_id: capture.stream_id,
        },
    );
    assert_eq!(fixture.service.live_captures(), 0);
}

#[test]
fn the_rooms_a_lease_describes_are_the_policys() {
    assert_eq!(Room::from(DisplayLease::UNHELD), Room::Unclaimed);
    assert_eq!(
        Room::from(DisplayLease::held(1, session(0xA1))),
        Room::Session(session(0xA1))
    );
}

/// A stream that reaches the stop it scheduled lets the device play out what
/// it was handed and then stops its clock, rather than leaving it to tick
/// with nothing to play.
#[test]
fn a_scheduled_stop_lets_the_device_play_out_and_stop() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let player = caller(7, &[]);
    let music = fixture.open_playback(&player, DEVICE_HZ, 4_096);
    let samples = signal(3_000);
    fixture.client_write(&music, &samples);
    start(&mut fixture, &player, &music);
    let end = Frames::new(1_500);
    fixture.grant(
        &player,
        &AudioRequest::Stop {
            stream_id: music.stream_id,
            at: end,
        },
    );
    run(&mut fixture, 8);
    assert_eq!(
        fixture.told(&music).last(),
        Some(&(StreamState::Paused, end))
    );
    assert!(
        fixture.played() == samples[..bytes(end)],
        "everything before the stop, and nothing after it"
    );
    assert!(
        !fixture.device.borrow().audio().running,
        "nothing live is left to clock the device for"
    );
}

/// Media ducked under speech comes back up when the speech reaches the stop
/// it scheduled, not only when something else happens to rebalance it.
#[test]
fn media_comes_back_up_when_speech_reaches_its_scheduled_stop() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (listener, talker) = (caller(7, &[]), caller(8, &[]));
    let music = fixture.open_playback(&listener, DEVICE_HZ, 4_096);
    let call = fixture.open_role(&talker, StreamRole::Communication, DEVICE_HZ, 4_096);
    let samples = signal(4_000);
    fixture.client_write(&music, &samples);
    fixture.client_write(&call, &vec![0u8; 4_000 * 4]);
    let end = Frames::new(1_024);
    fixture.grant(
        &talker,
        &AudioRequest::Stop {
            stream_id: call.stream_id,
            at: end,
        },
    );
    start(&mut fixture, &listener, &music);
    start(&mut fixture, &talker, &call);
    run(&mut fixture, 6);
    assert_eq!(
        fixture.told(&call).last(),
        Some(&(StreamState::Paused, end))
    );
    let played = fixture.played();
    let last_period = played.len() - bytes(Frames::new(u64::from(PERIOD)))..played.len();
    assert!(
        played[last_period.clone()] == samples[last_period],
        "alone again, the music plays at unity, bit for bit"
    );
}

/// A capture stopped and resumed gives its client what it heard before the
/// stop and what it hears after the resume, never what the device captured
/// past the frame it stopped on.
#[test]
fn a_paused_capture_drops_what_it_captured_past_its_stop() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let recorder = caller(8, &[CapabilityId::AUDIO_CAPTURE]);
    let capture = open_capture(&mut fixture, &recorder);
    fixture.attach_capture(&recorder, &capture);
    let frames = PERIOD as usize;
    let heard = signal(frames);
    fixture
        .device
        .borrow_mut()
        .audio_mut()
        .captured
        .extend_from_slice(&heard);
    start(&mut fixture, &recorder, &capture);
    fixture.period_on(1);

    let past_the_stop = other_signal(frames);
    fixture
        .device
        .borrow_mut()
        .audio_mut()
        .captured
        .extend_from_slice(&past_the_stop);
    assert!(fixture.service_only(1).is_some(), "the device captured on");
    fixture.grant(
        &recorder,
        &AudioRequest::Stop {
            stream_id: capture.stream_id,
            at: Frames::ZERO,
        },
    );
    let stopped_at = Frames::new(frames as u64);
    assert_eq!(
        fixture.told(&capture).last(),
        Some(&(StreamState::Paused, stopped_at))
    );

    let resumed = signal(frames * 2)[frames * 4..].to_vec();
    fixture
        .device
        .borrow_mut()
        .audio_mut()
        .captured
        .extend_from_slice(&resumed);
    fixture.grant(
        &recorder,
        &AudioRequest::Start {
            stream_id: capture.stream_id,
            at: stopped_at,
        },
    );
    fixture.period_on(1);
    let mut expected = heard;
    expected.extend_from_slice(&resumed);
    assert!(
        fixture.client_read(&capture) == expected,
        "what it heard, then what it heard after the resume"
    );
}

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

/// The fixture sink's id: the first device's first endpoint.
const SINK: u32 = 1;

#[test]
fn a_rooms_tenant_controls_its_devices_and_nobody_else_does() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (alice, bob) = (in_session(7, 0xA1), in_session(8, 0xB0));
    // An unclaimed room is anybody's.
    assert!(fixture.sink(&bob, SINK).access.may_change());
    fixture
        .control(
            &bob,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-600),
            },
        )
        .expect("anybody's room");

    fixture.seat(DisplayLease::held(1, session(0xA1)));
    assert!(fixture.sink(&alice, SINK).access.may_change());
    assert!(!fixture.sink(&bob, SINK).access.may_change());
    assert_eq!(
        fixture.control(
            &bob,
            &AudioRequest::SetMute {
                device_id: SINK,
                muted: true
            }
        ),
        Err(Errno::SeatNotOwner),
        "a session outside the room cannot touch it"
    );
    fixture
        .control(
            &alice,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-1_200),
            },
        )
        .expect("the tenant's");
    assert_eq!(fixture.sink(&bob, SINK).level, level(-1_200));

    // While the seat changes hands nobody may.
    fixture.seat(DisplayLease::ended(2, ReleaseSurface::Handover));
    assert_eq!(
        fixture.control(&alice, &AudioRequest::SetDefault { device_id: SINK }),
        Err(Errno::SeatNotOwner)
    );
    // A device control never names "the default".
    assert_eq!(
        decode_status_reply(&fixture.call(
            &alice,
            &AudioRequest::SetLevel {
                device_id: 99,
                level: level(-100),
            }
        )),
        Err(Errno::SeatNotOwner)
    );
}

/// A returning user's held music resumes at their own controls, not at
/// whatever the session that held the room meanwhile left.
#[test]
fn a_returning_session_resumes_at_its_own_controls() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let (alice, bob) = (in_session(7, 0xA1), in_session(8, 0xB0));
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    fixture
        .control(
            &alice,
            &AudioRequest::SetMute {
                device_id: SINK,
                muted: true,
            },
        )
        .expect("the tenant's");
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(3_000));
    start(&mut fixture, &alice, &music);
    run(&mut fixture, 1);

    fixture.seat(DisplayLease::held(2, session(0xB0)));
    assert!(!fixture.sink(&bob, SINK).muted, "Bob's room is his own");
    run(&mut fixture, 4);
    let before = fixture.played().len();
    let call = fixture.open_playback(&bob, DEVICE_HZ, 4_096);
    let speech = other_signal(1_000);
    fixture.client_write(&call, &speech);
    start(&mut fixture, &bob, &call);
    drain(&mut fixture, &bob, &call);
    run(&mut fixture, 8);
    let played = fixture.played();
    assert!(played[before..] == speech[..], "Bob is heard, unmuted");

    fixture.seat(DisplayLease::held(3, session(0xA1)));
    assert!(
        fixture.sink(&alice, SINK).muted,
        "Alice's controls are back"
    );
    let resumed = fixture.played().len();
    drain(&mut fixture, &alice, &music);
    run(&mut fixture, 16);
    assert!(
        fixture.played()[resumed..].iter().all(|byte| *byte == 0),
        "her music resumed at her own mute, not Bob's"
    );
}

#[test]
fn a_default_follows_its_tenants_preference_through_a_device_going() {
    let mut fixture = Fixture::new();
    fixture.bind();
    fixture.bind_another(0x4143_4841_4E00_0002, 0x51e7);
    let anyone = caller(9, &[]);
    let manager = caller(3, &[CapabilityId::DRV_LOAD]);
    let sinks = fixture.devices(&anyone, StreamDirection::Playback);
    assert_eq!(sinks.len(), 2);
    assert!(
        sinks[0].default.is_default(),
        "the first bound, with no preference"
    );
    let second = sinks[1].device_id;

    fixture
        .control(&anyone, &AudioRequest::SetDefault { device_id: second })
        .expect("anybody's room");
    assert!(fixture.sink(&anyone, second).default.is_default());

    // The preferred device goes: the default is never left on a device that
    // is gone, and a gone device is not shown.
    fixture
        .control(
            &manager,
            &AudioRequest::UnbindDriver {
                endpoint_id: 0x4143_4841_4E00_0002,
            },
        )
        .expect("the device manager retires it");
    let sinks = fixture.devices(&anyone, StreamDirection::Playback);
    assert_eq!(sinks.len(), 1);
    assert!(sinks[0].default.is_default());

    // It comes back where it was, on the channel it had: the preference is
    // by place, so it is the default again.
    fixture.bind_another(0x4143_4841_4E00_0002, 0x51e7);
    let sinks = fixture.devices(&anyone, StreamDirection::Playback);
    assert_eq!(sinks.len(), 2);
    let back = sinks
        .iter()
        .find(|sink| sink.location == AudioLocation::new(0x51e7, 0).expect("a place"))
        .expect("the replugged sink");
    assert!(back.default.is_default());
}

/// A device whose driver faulted is lost; the same channel bound again is a
/// new device, never swallowed as one already bound (D245).
#[test]
fn a_rebound_channel_replaces_its_lost_device_in_the_slot_it_left() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let anyone = caller(9, &[]);
    let faulted = tairix_abi::driver::audio_channel::AudioChannelNotify::Faulted {
        endpoint: 0,
        reason: Errno::DeviceFault,
    }
    .encode();
    fixture.service.on_device_notify(0, &faulted, &DiscardSink);
    assert!(fixture
        .devices(&anyone, StreamDirection::Playback)
        .is_empty());
    assert_eq!(fixture.service.device_count(), 0, "reaped: nothing rode it");
    assert_eq!(
        fixture.service.bind_slot(),
        0,
        "its slot is the next bind's"
    );

    fixture.bind();
    assert_eq!(fixture.service.device_count(), 1);
    let sinks = fixture.devices(&anyone, StreamDirection::Playback);
    assert_eq!(sinks.len(), 1, "the channel is a device again");
    assert!(sinks[0].default.is_default());
}

#[test]
fn a_lost_device_with_streams_waits_for_them_before_it_is_reaped() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let owner = caller(7, &[]);
    let grant = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    let manager = caller(3, &[CapabilityId::DRV_LOAD]);
    fixture
        .control(
            &manager,
            &AudioRequest::UnbindDriver {
                endpoint_id: 0x4143_4841_4E00_0000,
            },
        )
        .expect("retired");
    assert_eq!(fixture.state(&owner, &grant), StreamState::DeviceLost);
    assert_eq!(
        fixture.service.bind_slot(),
        1,
        "held while a stream rides it"
    );
    fixture.grant(
        &owner,
        &AudioRequest::Close {
            stream_id: grant.stream_id,
        },
    );
    assert_eq!(
        fixture.service.bind_slot(),
        0,
        "reaped with its last stream"
    );
    // Only the device manager retires a device.
    assert_eq!(
        fixture.control(
            &owner,
            &AudioRequest::UnbindDriver {
                endpoint_id: 0x4143_4841_4E00_0000
            }
        ),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn the_baseline_is_the_device_managers_and_lies_beneath_every_tenant() {
    let mut fixture = Fixture::new();
    fixture.bind();
    fixture.bind_another(0x4143_4841_4E00_0002, 0x51e7);
    let (alice, bob) = (in_session(7, 0xA1), in_session(8, 0xB0));
    let baseline = AudioBaseline {
        output: Some(AudioLocation::new(0x51e7, 0).expect("a place")),
        input: None,
        level: level(-900),
    };
    assert_eq!(
        fixture.control(&alice, &AudioRequest::Baseline(baseline)),
        Err(Errno::PermissionDenied)
    );
    let manager = caller(3, &[CapabilityId::DRV_LOAD]);
    fixture
        .control(&manager, &AudioRequest::Baseline(baseline))
        .expect("the device manager's");
    let sinks = fixture.devices(&alice, StreamDirection::Playback);
    assert_eq!(
        sinks[1].default,
        DefaultChoice::Inherited,
        "the machine's preference"
    );
    assert!(sinks.iter().all(|sink| sink.level == level(-900)));
    assert!(
        sinks
            .iter()
            .all(|sink| !sink.own_level && sink.access == ControlAccess::Shared),
        "the machine's choices are nobody's own, and an unclaimed room is no session's"
    );

    fixture.seat(DisplayLease::held(1, session(0xA1)));
    fixture
        .control(
            &alice,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-300),
            },
        )
        .expect("the tenant's");
    fixture
        .control(&alice, &AudioRequest::SetDefault { device_id: SINK })
        .expect("the tenant's");
    let sinks = fixture.devices(&alice, StreamDirection::Playback);
    assert!(
        sinks[0].default.is_default(),
        "the tenant's preference first"
    );
    assert_eq!(sinks[0].level, level(-300));
    assert_eq!(sinks[1].level, level(-900));
    assert!(
        sinks.iter().all(|sink| sink.access == ControlAccess::Own),
        "the room is Alice's"
    );
    assert!(sinks[0].own_level);
    assert_eq!(sinks[0].default, DefaultChoice::Preferred);
    assert!(!sinks[1].own_level, "the baseline's");
    assert_eq!(sinks[1].default, DefaultChoice::No);
    assert!(
        fixture
            .devices(&bob, StreamDirection::Playback)
            .iter()
            .all(|sink| sink.access == ControlAccess::Shown),
        "Bob is shown Alice's room, not given it"
    );

    fixture.seat(DisplayLease::held(2, session(0xB0)));
    let sinks = fixture.devices(&bob, StreamDirection::Playback);
    assert!(
        sinks[1].default.is_default(),
        "Bob has no preference of his own"
    );
    assert_eq!(sinks[0].level, level(-900));
    assert!(!sinks[0].own_level);
    assert_eq!(sinks[0].default, DefaultChoice::No);
    assert_eq!(
        sinks[1].default,
        DefaultChoice::Inherited,
        "the machine's preference"
    );
}

#[test]
fn the_stream_listing_is_the_system_information_services_alone() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let owner = caller(7, &[]);
    let first = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    let second = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    assert_eq!(
        decode_streams_reply(&fixture.call(&owner, &AudioRequest::ListStreams { after: 0 })),
        Err(Errno::PermissionDenied)
    );
    let broker = caller(2, &[CapabilityId::SYSINFO_INTROSPECT]);
    let listed =
        decode_streams_reply(&fixture.call(&broker, &AudioRequest::ListStreams { after: 0 }))
            .expect("the first stream");
    assert_eq!(listed.stream_id, first.stream_id);
    assert_eq!((listed.owner_pid, listed.owner_uid), (7, 1_000));
    assert_eq!(listed.device_id, SINK);
    let next = decode_streams_reply(&fixture.call(
        &broker,
        &AudioRequest::ListStreams {
            after: first.stream_id,
        },
    ))
    .expect("the next");
    assert_eq!(next.stream_id, second.stream_id);
    assert_eq!(
        decode_streams_reply(&fixture.call(
            &broker,
            &AudioRequest::ListStreams {
                after: second.stream_id,
            }
        )),
        Err(Errno::NotFound)
    );
}

/// The point of the device's own control: a level on its grid costs the mix
/// nothing, so the bytes still reach the device unaltered.
#[test]
fn a_level_the_device_can_take_is_its_and_the_mix_stays_exact() {
    let mut fixture = Fixture::with_control(GainRange::new(-6_400, 0, 50).expect("valid"));
    fixture.bind();
    let owner = caller(7, &[]);
    assert_eq!(
        fixture.device.borrow().audio().programmed,
        vec![(0, 0, false)],
        "programmed at bind, so the device's state is known"
    );
    fixture
        .control(
            &owner,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-1_000),
            },
        )
        .expect("anybody's room");
    assert_eq!(
        fixture.device.borrow().audio().programmed.last(),
        Some(&(0, -1_000, false))
    );
    let grant = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    let samples = signal(1_500);
    fixture.client_write(&grant, &samples);
    start(&mut fixture, &owner, &grant);
    drain(&mut fixture, &owner, &grant);
    run(&mut fixture, 16);
    assert!(fixture.played() == samples, "the hardware took all of it");

    fixture
        .control(
            &owner,
            &AudioRequest::SetMute {
                device_id: SINK,
                muted: true,
            },
        )
        .expect("anybody's room");
    assert_eq!(
        fixture.device.borrow().audio().programmed.last(),
        Some(&(0, -6_400, true)),
        "muted at the control too"
    );
}

#[test]
fn a_level_a_device_without_a_control_cannot_take_is_the_mixers() {
    let mut fixture = Fixture::new();
    fixture.bind();
    let owner = caller(7, &[]);
    fixture
        .control(
            &owner,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-600),
            },
        )
        .expect("anybody's room");
    assert!(fixture.device.borrow().audio().programmed.is_empty());
    let grant = fixture.open_playback(&owner, DEVICE_HZ, 2_048);
    let samples = signal(1_500);
    fixture.client_write(&grant, &samples);
    start(&mut fixture, &owner, &grant);
    drain(&mut fixture, &owner, &grant);
    run(&mut fixture, 16);
    let played = fixture.played();
    assert_eq!(played.len(), samples.len());
    let peak = |bytes: &[u8]| {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair).unsigned_abs())
            .max()
            .unwrap_or(0)
    };
    assert_eq!(peak(&samples), 1_000);
    // -6 dB is a factor of 0.501, give or take the last bit's rounding.
    let quieter = peak(&played);
    assert!(
        (500..=502).contains(&quieter),
        "six decibels down in the mix: {quieter}"
    );
}

#[test]
fn the_change_count_moves_with_each_change_and_not_otherwise() {
    let mut fixture = Fixture::new();
    let before = fixture.service.changes();
    fixture.bind();
    let bound = fixture.service.changes();
    assert_ne!(bound, before);
    let owner = caller(7, &[]);
    fixture
        .control(
            &owner,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-600),
            },
        )
        .expect("anybody's room");
    let set = fixture.service.changes();
    assert_ne!(set, bound);
    fixture
        .control(
            &owner,
            &AudioRequest::SetLevel {
                device_id: SINK,
                level: level(-600),
            },
        )
        .expect("anybody's room");
    assert_eq!(fixture.service.changes(), set, "nothing moved");
}

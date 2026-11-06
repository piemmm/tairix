//! The bound devices, their configured endpoints, the live streams, and the
//! period pump that moves frames between them (`plans/SOUND.md`).
//!
//! # What a period costs
//!
//! Everything a period touches is allocated when a stream opens or an
//! endpoint is configured, and reused: the client-format scratch a stream's
//! frames are read into, its pivot and resampled buffers, the endpoint's one
//! device period, and the mixer's own working set. The pump borrows one
//! shared region at a time — each client ring in turn, then the device ring —
//! which is the discipline [`RegionHost::bytes`] states in its signature, and
//! it folds the live streams into the mixer as an iterator so there is no
//! per-period collection to build either.
//!
//! # Silence is accounted, never hidden
//!
//! A running stream with nothing queued contributes silence for exactly the
//! frames it missed and takes the underrun; the device would otherwise run
//! dry, which is strictly worse and is what its driver would report instead.
//! A *draining* stream is different: the pump emits only the frames it
//! genuinely has, so a drain's tail is a short chunk rather than a padded
//! one, and the drain is then passed down to the device — unless a monitor
//! keeps the sink clocking, when the tail is padded with silence and the
//! drain is over once the device reaches the frame its last one ends on.
//!
//! # A sink's monitors
//!
//! A monitor is a capture stream riding a sink, handed each period the device
//! is given as it was before the sink's own level and mute: the device's
//! period itself where the mixer took none of that level, otherwise the period
//! mixed again by a mixer of its own, so a monitor never moves the device's
//! dither. A live monitor keeps its sink clocking, silent when nothing plays
//! and never drained beneath it, so it hears the sink's own timeline. It
//! never chooses how its sink runs: a
//! sink monitors alone ride runs as nobody asked, and the first stream to play
//! has it configured afresh as that stream alone would, the monitors moving
//! onto it where they stand. A tap that cannot make their mix faults them
//! alone.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::appinfo::BundleId;
use tairix_abi::audio::{
    AudioGain, AudioLocation, AudioNotify, OpenParams, StreamGrant, StreamRole, StreamState,
    MONITOR_FLAG,
};
use tairix_abi::driver::audio::{
    AudioEndpointFacts, ChannelMap, Frames, GainRange, Rate, RateSupport, SampleFormat,
    StreamDirection,
};
use tairix_abi::driver::audio_channel::{AttachParams, ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_audio::channel::ChannelMatrix;
use tairix_audio::clock::ClockModel;
use tairix_audio::convert;
use tairix_audio::mix::{Mixer, SinkFormat, StreamMix};
use tairix_audio::resample::{FilterBank, Ratio, Resampler};
use tairix_audio::route::Principal;
use tairix_audio::volume::EndpointLevel;
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_util::fallible;

use crate::channel::{AudioChannelClient, AudioChannelTransport};
use crate::events;
use crate::region::{RegionHost, RegionId};
use crate::service::audit;
use crate::wakes::Wake;

/// The encoding a resampled contribution is staged in for the mixer.
///
/// The resampler answers the pivot and the mixer takes bytes, so a resampled
/// stream is staged in the pivot's own encoding rather than re-quantised back
/// to the client's, which would throw away resolution the filter produced.
const RESAMPLED_FORMAT: SampleFormat = SampleFormat::F32;

/// Device sample encodings an endpoint is run in when the client's own is not
/// among the ones it accepts, or no client asks, widest first.
///
/// Widest first because a wider device encoding never costs the client bits:
/// the pivot carries twenty-four and every scale factor is a power of two, so
/// a sixteen-bit source through a thirty-two-bit device is still exact. The
/// order is fixed, so two identical machines configure identically.
const FORMAT_PREFERENCE: &[SampleFormat] = &[
    SampleFormat::F32,
    SampleFormat::S32,
    SampleFormat::S24In32,
    SampleFormat::S24,
    SampleFormat::S16,
    SampleFormat::U8,
];

/// One driver's device channel, and the endpoints it presents.
pub(crate) struct Device {
    /// The reserved `audiochan-v1` call endpoint `devmgr` handed over — this
    /// channel's identity, so a repeated hand-off is recognised rather than
    /// provisioning a duplicate device.
    pub(crate) channel_endpoint: u64,
    /// The port this service bound and the driver sends its notifies to.
    pub(crate) notify_endpoint: u64,
    /// The control-plane client.
    pub(crate) channel: AudioChannelClient<Box<dyn AudioChannelTransport>>,
    /// Its sinks and sources.
    pub(crate) endpoints: Vec<Endpoint>,
    /// Set once the channel has faulted: its streams hold their positions and
    /// every other device is untouched.
    pub(crate) lost: bool,
}

/// One sink or source of a device, as this service tracks it.
pub(crate) struct Endpoint {
    /// Its index on the device.
    pub(crate) index: u16,
    /// The service-assigned identity a client opens a stream against.
    pub(crate) device_id: u32,
    /// Where it is, across boots.
    pub(crate) location: AudioLocation,
    /// What the endpoint said it can do.
    pub(crate) facts: AudioEndpointFacts,
    /// Its own control, until the driver refuses to program it; then the
    /// mixer takes the whole level.
    pub(crate) control: Option<GainRange>,
    /// Its own level as delivered: the control's setting and the mixer's
    /// remainder.
    pub(crate) level: EndpointLevel,
    /// Frames the device reported losing on it since it was bound.
    pub(crate) lost_frames: u64,
    /// Present once a stream has caused the endpoint to be programmed.
    pub(crate) active: Option<Active>,
}

/// One phase bank, shared by every stream on this endpoint converting between
/// one pair of rates: a playback stream's to the device's, or the device's to
/// a capture stream's.
///
/// Reference-counted rather than kept for the endpoint's life: the
/// coefficients depend on the rate pair alone, so one bank serves all of that
/// pair's streams, and the last stream leaving takes it with it — otherwise a
/// client naming a fresh rate each time would grow the table without bound.
struct BankSlot {
    bank: FilterBank,
    users: u32,
}

/// A programmed endpoint: the device ring, the mixer, the clock, and the
/// buffers a period reuses.
pub(crate) struct Active {
    /// What it was asked to run at: a stream opening where only monitors ride
    /// has the sink configured afresh unless it asks the same.
    pub(crate) asked: ConfigureParams,
    /// What the device answered it would run at.
    pub(crate) grant: ConfigureGrant,
    /// The device ring's shape.
    geometry: PcmGeometry,
    /// The device ring this service created and granted to the driver.
    pub(crate) region: RegionId,
    /// When it was attached, on the monotonic clock the driver stamps its
    /// reports with: one sampled before then is an earlier configuration's,
    /// however late it arrives.
    pub(crate) attached_at: Time64,
    /// The measured device clock, from the `(position, sampled_at)` pairs the
    /// driver reports.
    pub(crate) clock: ClockModel,
    /// This endpoint's mixer, sized for one period.
    mixer: Mixer,
    /// The format the mixer quantises to.
    sink: SinkFormat,
    /// One device period of frames, reused by every chunk.
    period: Vec<u8>,
    /// One device period decoded, for the capture fan-out.
    pivot: Vec<f32>,
    /// The phase banks this endpoint's streams share.
    banks: Vec<Option<BankSlot>>,
    /// The mix made again for monitors, while one rides the sink.
    tap: Option<Tap>,
    /// Whether the device is clocking.
    pub(crate) running: bool,
    /// The device's own position as last reported.
    pub(crate) position: Frames,
    /// Cumulative frames the device lost, as the driver reports them.
    pub(crate) xrun_frames: u64,
}

impl Active {
    /// Frames the device interrupts on.
    pub(crate) const fn period_frames(&self) -> u32 {
        self.grant.period_frames
    }

    /// Mix onto the resolution `facts`, read again, give the device's
    /// encoding: a sink replugged on the same connector may resolve more or
    /// fewer bits.
    pub(crate) fn follow(&mut self, facts: &AudioEndpointFacts) -> Result<(), Errno> {
        let bits = facts.resolved_bits(self.sink.format);
        self.mixer.set_resolution(bits)?;
        if let Some(tap) = self.tap.as_mut() {
            tap.mixer.set_resolution(bits)?;
        }
        self.sink.resolution = bits;
        Ok(())
    }

    /// Reserve (or find) the phase bank carrying `from` to `to`, and answer
    /// its slot.
    fn acquire_bank(&mut self, from: Rate, to: Rate) -> Result<usize, Errno> {
        let ratio = Ratio::between(from, to);
        if let Some(index) = self
            .banks
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|held| held.bank.ratio() == ratio))
        {
            if let Some(held) = self.banks[index].as_mut() {
                held.users = held.users.saturating_add(1);
            }
            return Ok(index);
        }
        let bank = FilterBank::new(from, to)?;
        let slot = BankSlot { bank, users: 1 };
        if let Some(index) = self.banks.iter().position(Option::is_none) {
            self.banks[index] = Some(slot);
            return Ok(index);
        }
        if !fallible::reserve(&mut self.banks, 1) {
            return Err(Errno::OutOfMemory);
        }
        self.banks.push(Some(slot));
        Ok(self.banks.len() - 1)
    }

    /// Drop a stream's hold on bank slot `index`, freeing the coefficients
    /// with the last user.
    fn release_bank(&mut self, index: usize) {
        let Some(slot) = self.banks.get_mut(index) else {
            return;
        };
        let spent = slot.as_mut().is_some_and(|held| {
            held.users = held.users.saturating_sub(1);
            held.users == 0
        });
        if spent {
            *slot = None;
        }
    }

    /// Mix for monitors too: a mixer and a period of the tap's own beside the
    /// device's. Nothing changes while a tap is held.
    pub(crate) fn add_tap(&mut self, dither_seed: u64) -> Result<(), Errno> {
        if self.tap.is_some() {
            return Ok(());
        }
        let period = self.period_frames() as usize;
        self.tap = Some(Tap {
            mixer: Mixer::new(self.sink, period, dither_seed)?,
            period: fallible::filled(period * self.sink.frame_bytes(), 0u8)
                .ok_or(Errno::OutOfMemory)?,
        });
        Ok(())
    }

    /// Give the tap back: no monitor rides the sink any more.
    pub(crate) fn remove_tap(&mut self) {
        self.tap = None;
    }
}

/// A sink's mix made again without the sink's own level, for its monitors,
/// where the mixer takes part of that level.
///
/// A mixer of its own, so a monitor never moves the device's dither and the
/// device is given the same frames whether or not it is monitored.
struct Tap {
    /// The monitors' mixer, sized for one device period.
    mixer: Mixer,
    /// One device period of that mix, in the device's encoding.
    period: Vec<u8>,
}

impl Tap {
    /// `frames` of the sink's mix of `streams` made again at the multiply its
    /// monitors hear each at, without the sink's own level.
    fn remix(
        &mut self,
        streams: &[Stream],
        device_index: usize,
        slot: usize,
        frames: usize,
    ) -> Result<&[u8], Errno> {
        let contributions = streams
            .iter()
            .filter(|stream| stream.feeds(device_index, slot))
            .filter_map(|stream| stream.contribution(stream.monitor_gain));
        let written = self.mixer.mix(contributions, frames, &mut self.period)?;
        Ok(&self.period[..written])
    }
}

/// One live `audio-v1` stream.
pub(crate) struct Stream {
    /// The service-issued token every later request names.
    pub(crate) id: u64,
    /// The caller's kernel-attested pid; a request naming this stream from
    /// any other process is refused.
    pub(crate) owner_pid: u64,
    /// The user it runs as.
    pub(crate) owner_uid: u32,
    /// The application it belongs to, where the kernel attests one.
    pub(crate) owner_app: Option<BundleId>,
    /// Index of the device it landed on.
    pub(crate) device: usize,
    /// Index of the endpoint within that device.
    pub(crate) endpoint: usize,
    /// Playback or capture.
    pub(crate) direction: StreamDirection,
    /// A capture stream hearing its sink's mix rather than a source.
    pub(crate) monitor: bool,
    /// What the sound is for.
    pub(crate) role: StreamRole,
    /// Whom it plays for, which the seat's room is arbitrated on.
    pub(crate) principal: Principal,
    /// What the client was told.
    pub(crate) grant: StreamGrant,
    /// The client ring's shape.
    pub(crate) geometry: PcmGeometry,
    /// The client's ring, once attached.
    pub(crate) region: Option<RegionId>,
    /// What it derives from its endpoint's configuration.
    work: Working,
    /// This stream's own level.
    pub(crate) gain: AudioGain,
    /// Whether it is muted, independently of its gain.
    pub(crate) muted: bool,
    /// The multiply the mix applies, resolved from every gain in force.
    pub(crate) software_gain: f32,
    /// The multiply the sink's monitors hear a playback stream at: its own
    /// gains and the ducking, without the sink's level.
    pub(crate) monitor_gain: f32,
    /// Where the client's own requests leave it; never
    /// [`StreamState::SeatInactive`], which is the room's word, not the
    /// client's.
    pub(crate) state: StreamState,
    /// The seat's room is not its owner's, so it moves no frames whatever
    /// its own state, and a client with anything in flight is told so.
    pub(crate) held: bool,
    /// The position the state it was last told of took effect at.
    pub(crate) state_at: Frames,
    /// The frame a scheduled stop settles on, where one was named ahead of
    /// the position the stream had reached.
    pub(crate) stop_at: Option<Frames>,
    /// The device frame a drain's last frame ends on, once it is handed to a
    /// sink a monitor keeps clocking: the drain is over when the device
    /// reaches it.
    pub(crate) drain_end: Option<Frames>,
    /// Frames this service has taken from (or given to) the client ring.
    pub(crate) position: Frames,
    /// Frames lost to under- or over-run.
    pub(crate) xrun_frames: u64,
    /// Distinct under- or over-runs.
    pub(crate) xruns: u32,
}

/// What a stream needs from its caller to be opened.
pub(crate) struct StreamOpen {
    /// The service-issued token.
    pub(crate) id: u64,
    /// The caller's kernel-attested pid.
    pub(crate) owner_pid: u64,
    /// The user it runs as.
    pub(crate) owner_uid: u32,
    /// The application it belongs to, where the kernel attests one.
    pub(crate) owner_app: Option<BundleId>,
    /// Index of the device the router chose.
    pub(crate) device: usize,
    /// Index of the endpoint within it.
    pub(crate) endpoint: usize,
    /// Playback or capture.
    pub(crate) direction: StreamDirection,
    /// Whether it hears the sink's mix rather than a source.
    pub(crate) monitor: bool,
    /// What the sound is for.
    pub(crate) role: StreamRole,
    /// Whom it plays for.
    pub(crate) principal: Principal,
    /// Whether the room holds it from the moment it opens.
    pub(crate) held: bool,
    /// What the client is being told, which fixes its rate, format, layout
    /// and ring.
    pub(crate) grant: StreamGrant,
    /// The client ring's shape, derived from that grant.
    pub(crate) geometry: PcmGeometry,
}

/// What a stream derives from its endpoint's configuration, allocated whole
/// so no period allocates: built when the stream opens, and again when its
/// sink is configured afresh beneath it.
struct Working {
    /// This stream's layout onto the device's (playback) or the device's onto
    /// this stream's (capture).
    matrix: ChannelMatrix,
    /// Present only where the stream's rate differs from the device's.
    resampler: Option<Resampler>,
    /// Which of the endpoint's shared banks that resampler runs over, while
    /// the stream holds a share of it.
    bank: Option<usize>,
    /// Client-encoding frames moved this period.
    bytes: Vec<u8>,
    /// Those frames decoded, in this stream's own layout.
    pivot: Vec<f32>,
    /// Filtered frames awaiting their mix (playback) or their client ring
    /// (capture), in this stream's layout.
    pending: Vec<f32>,
    /// Frames held in `pending`.
    pending_frames: usize,
    /// `pending` staged in the pivot's encoding for the mixer (a resampled
    /// playback stream only).
    staged: Vec<u8>,
    /// Device frames this stream contributes to the chunk being mixed.
    covered: usize,
    /// Client-rate frames pulled per top-up.
    want_in: usize,
}

impl Working {
    /// The working set of a stream moving frames `direction` at what `grant`
    /// fixes, against `active`, holding a share of its phase bank where the
    /// stream's rate is not the device's.
    ///
    /// # Errors
    ///
    /// [`Errno::NotSupported`] for a pair of channel layouts with no defined
    /// relationship, [`Errno::OutOfMemory`] for a buffer that would not fit,
    /// or whatever the resampler's bank refuses; a refusal holds no share.
    fn new(
        active: &mut Active,
        direction: StreamDirection,
        grant: &StreamGrant,
    ) -> Result<Self, Errno> {
        let client_map = grant.channel_map;
        let matrix = matrix_for(direction, &client_map, &active.grant.channel_map)?;
        let channels = usize::from(client_map.channels());
        let (source, sink_rate) = match direction {
            StreamDirection::Playback => (grant.rate, active.grant.rate),
            StreamDirection::Capture => (active.grant.rate, grant.rate),
        };
        let resampled = if source == sink_rate {
            None
        } else {
            let bank = active.acquire_bank(source, sink_rate)?;
            let built = active
                .banks
                .get(bank)
                .and_then(Option::as_ref)
                .ok_or(Errno::NotFound)
                .and_then(|slot| Resampler::new(&slot.bank, channels));
            match built {
                Ok(resampler) => Some((resampler, bank)),
                Err(err) => {
                    active.release_bank(bank);
                    return Err(err);
                }
            }
        };
        let bank = resampled.as_ref().map(|(_, bank)| *bank);
        let period = active.period_frames() as usize;
        let ratio = Ratio::between(source, sink_rate);
        let work = Self::sized(&matrix, resampled, direction, grant, period, ratio);
        if let (Err(_), Some(bank)) = (&work, bank) {
            active.release_bank(bank);
        }
        work
    }

    /// The buffers of a stream moving frames `direction` at what `grant`
    /// fixes, filtered by `resampled` where its rate is not the device's,
    /// sized as upper bounds over a device period of `period` frames at
    /// `ratio` so none ever has to grow: what a playback stream's pull loop
    /// reads for one period and what that makes, and what a capture makes of
    /// the one period it is handed.
    fn sized(
        matrix: &ChannelMatrix,
        filtering: Option<(Resampler, usize)>,
        direction: StreamDirection,
        grant: &StreamGrant,
        period: usize,
        ratio: Ratio,
    ) -> Result<Self, Errno> {
        let channels = usize::from(grant.channel_map.channels());
        let frame_bytes = channels * grant.format.bytes_per_sample();
        let (resampler, bank) = filtering.unzip();
        let want_in = period
            .saturating_mul(ratio.input() as usize)
            .div_ceil((ratio.output() as usize).max(1))
            .saturating_add(1);
        let made_from = |frames: usize| {
            resampler
                .as_ref()
                .map_or(frames, |filter| filter.max_output_frames(frames))
        };
        let (bytes_frames, pivot_frames, pending_frames, staged_frames) = match direction {
            StreamDirection::Playback => {
                let produced = made_from(want_in);
                (want_in, want_in, period + produced, period + produced)
            }
            StreamDirection::Capture => {
                let heard = made_from(period);
                (heard, period, heard, 0)
            }
        };
        let bytes = |count: usize| fallible::filled(count, 0u8).ok_or(Errno::OutOfMemory);
        let pivot = |count: usize| fallible::filled(count, 0.0f32).ok_or(Errno::OutOfMemory);
        Ok(Self {
            matrix: *matrix,
            resampler,
            bank,
            bytes: bytes(bytes_frames * frame_bytes)?,
            pivot: pivot(pivot_frames * channels)?,
            pending: pivot(pending_frames * channels)?,
            pending_frames: 0,
            staged: bytes(staged_frames * channels * RESAMPLED_FORMAT.bytes_per_sample())?,
            covered: 0,
            want_in,
        })
    }
}

impl Stream {
    /// Allocate a stream's whole working set against `active`, so no later
    /// period allocates.
    ///
    /// # Errors
    ///
    /// [`Errno::NotSupported`] for a pair of channel layouts with no defined
    /// relationship, [`Errno::OutOfMemory`] for a buffer that would not fit,
    /// or whatever the resampler's bank refuses.
    pub(crate) fn open(active: &mut Active, params: &StreamOpen) -> Result<Self, Errno> {
        Ok(Self {
            id: params.id,
            owner_pid: params.owner_pid,
            owner_uid: params.owner_uid,
            owner_app: params.owner_app,
            device: params.device,
            endpoint: params.endpoint,
            direction: params.direction,
            monitor: params.monitor,
            role: params.role,
            principal: params.principal,
            grant: params.grant,
            geometry: params.geometry,
            region: None,
            work: Working::new(active, params.direction, &params.grant)?,
            gain: AudioGain::UNITY,
            muted: false,
            software_gain: 1.0,
            monitor_gain: 1.0,
            state: StreamState::Idle,
            held: params.held,
            state_at: Frames::ZERO,
            stop_at: None,
            drain_end: None,
            position: Frames::ZERO,
            xrun_frames: 0,
            xruns: 0,
        })
    }

    /// Build the working set again against `active`, a configuration its
    /// sink was given afresh beneath it: the frames in flight and the
    /// filter's memory went with the old one, and with them the frame a
    /// drain was to end on.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], the old working set kept.
    pub(crate) fn rebind(&mut self, active: &mut Active) -> Result<(), Errno> {
        self.work = Working::new(active, self.direction, &self.grant)?;
        self.drain_end = None;
        Ok(())
    }

    /// Whether the stream is moving frames this period.
    ///
    /// A draining stream still is: it is playing out what it queued, and the
    /// difference is only that the pump stops padding for it, unless a
    /// monitor keeps its sink clocking.
    pub(crate) fn is_live(&self) -> bool {
        self.moving() && !self.held && self.region.is_some()
    }

    /// Whether its own requests have it moving frames, room or not.
    fn moving(&self) -> bool {
        matches!(self.state, StreamState::Running | StreamState::Draining)
    }

    /// Whether the stream is playing out what it has and then stopping.
    pub(crate) fn is_draining(&self) -> bool {
        self.state == StreamState::Draining
    }

    /// Whether it holds its endpoint to the configuration it runs under:
    /// every stream but a monitor, which hears its sink without choosing how
    /// it runs, and one lost with that configuration.
    pub(crate) fn holds_configuration(&self) -> bool {
        !self.monitor && self.state != StreamState::DeviceLost
    }

    /// Whether it may move frames again: neither lost with its device nor
    /// stopped for a fault.
    pub(crate) fn may_move(&self) -> bool {
        !matches!(self.state, StreamState::DeviceLost | StreamState::Faulted)
    }

    /// The state its client sees: its own, unless the room holds a stream
    /// that is moving or would resume — an idle stream has nothing in flight,
    /// and a lost or faulted one never moves again.
    pub(crate) fn reported(&self) -> StreamState {
        let in_flight = self.moving() || self.state == StreamState::Paused;
        if self.held && in_flight {
            StreamState::SeatInactive
        } else {
            self.state
        }
    }

    /// Frames this stream may still take before a scheduled stop settles it,
    /// or [`None`] when no stop is pending.
    fn stop_limit(&self) -> Option<usize> {
        let at = self.stop_at?;
        Some(usize::try_from(at.since(self.position).unwrap_or(0)).unwrap_or(usize::MAX))
    }

    /// Discard the filter's memory: the frames that were in flight are gone,
    /// so the history they would have coloured goes with them.
    pub(crate) fn reset_filter(&mut self) {
        if let Some(filter) = self.work.resampler.as_mut() {
            filter.reset();
        }
        self.work.pending_frames = 0;
        self.work.covered = 0;
    }

    /// Whether this stream belongs to `endpoint` of `device`.
    pub(crate) fn on(&self, device: usize, endpoint: usize) -> bool {
        self.device == device && self.endpoint == endpoint
    }

    /// Whether it is a playback stream live on `endpoint` of `device`, its
    /// frames mixed there.
    pub(crate) fn feeds(&self, device: usize, endpoint: usize) -> bool {
        self.direction == StreamDirection::Playback && self.on(device, endpoint) && self.is_live()
    }

    /// Whether it is a monitor live on `endpoint` of `device`, hearing its
    /// mix.
    pub(crate) fn hears(&self, device: usize, endpoint: usize) -> bool {
        self.monitor && self.on(device, endpoint) && self.is_live()
    }

    /// Whether the device is given it at the very multiply a monitor hears it
    /// at, bit for bit: the mixer takes none of the sink's level.
    fn heard_as_played(&self) -> bool {
        self.software_gain.to_bits() == self.monitor_gain.to_bits()
    }

    /// Interleaved channels one client frame carries.
    fn channels(&self) -> usize {
        usize::from(self.grant.channel_map.channels())
    }

    /// Move the stream to `state` at `at`, and tell the client the exact
    /// frame what it sees changed on.
    pub(crate) fn set_state(&mut self, state: StreamState, at: Frames, notifier: &mut dyn Wake) {
        let told = self.reported();
        self.state = state;
        // A drain's end belongs to the drain it was marked in.
        self.drain_end = None;
        self.tell(told, at, notifier);
    }

    /// Hold the stream for the room, or release it, at `at`.
    pub(crate) fn set_held(&mut self, held: bool, at: Frames, notifier: &mut dyn Wake) {
        let told = self.reported();
        self.held = held;
        self.tell(told, at, notifier);
    }

    /// Tell the client what it now sees, if that is not `told`.
    fn tell(&mut self, told: StreamState, at: Frames, notifier: &mut dyn Wake) {
        let state = self.reported();
        if state == told {
            return;
        }
        self.state_at = at;
        notifier.wake(
            self.grant.notify_endpoint,
            AudioNotify::StateChanged {
                stream_id: self.id,
                state,
                at,
            },
        );
    }

    /// Stop moving the stream, tell the client, and record whose it was and
    /// `why`. Every other stream, and the device, carry on.
    fn fault(&mut self, err: Errno, why: &'static str, notifier: &mut dyn Wake, sink: &dyn Sink) {
        let at = self.position;
        self.set_state(StreamState::Faulted, at, notifier);
        audit(
            sink,
            events::STREAM_FAULTED,
            Level::Warn,
            why,
            &[
                Field {
                    key: "pid",
                    value: FieldValue::UnsignedInt(self.owner_pid),
                },
                Field {
                    key: "stream",
                    value: FieldValue::UnsignedInt(self.id),
                },
                Field {
                    key: "error",
                    value: FieldValue::Error(err),
                },
            ],
        );
    }

    /// Record `lost` missed frames at the stream's position and tell the
    /// client, so it resynchronises exactly rather than drifting.
    fn account_loss(&mut self, lost: u64, notifier: &mut dyn Wake) {
        if lost == 0 {
            return;
        }
        self.xrun_frames = self.xrun_frames.saturating_add(lost);
        self.xruns = self.xruns.saturating_add(1);
        notifier.wake(
            self.grant.notify_endpoint,
            AudioNotify::Xrun {
                stream_id: self.id,
                at: self.position,
                lost_frames: lost,
            },
        );
    }

    /// This stream's contribution at `gain` to the chunk being mixed, or
    /// [`None`] when it had no frames.
    fn contribution(&self, gain: f32) -> Option<StreamMix<'_>> {
        let work = &self.work;
        if work.covered == 0 {
            return None;
        }
        let channels = self.channels();
        let (format, samples) = if work.resampler.is_some() {
            (
                RESAMPLED_FORMAT,
                &work.staged[..work.covered * channels * RESAMPLED_FORMAT.bytes_per_sample()],
            )
        } else {
            (
                self.grant.format,
                &work.bytes[..work.covered * channels * self.grant.format.bytes_per_sample()],
            )
        };
        Some(StreamMix {
            format,
            matrix: &work.matrix,
            gain,
            resampled: work.resampler.is_some(),
            samples,
        })
    }
}

/// The matrix carrying a stream's layout `client` onto its sink's `device`
/// (playback), or its source's `device` onto the stream's (capture).
///
/// # Errors
///
/// [`Errno::NotSupported`] for a pair of layouts with no defined relationship.
pub(crate) fn matrix_for(
    direction: StreamDirection,
    client: &ChannelMap,
    device: &ChannelMap,
) -> Result<ChannelMatrix, Errno> {
    match direction {
        StreamDirection::Playback => ChannelMatrix::derive(client, device),
        StreamDirection::Capture => ChannelMatrix::derive(device, client),
    }
}

/// The device encoding an endpoint is programmed in for a client asking for
/// `wanted`: the client's own where the endpoint accepts it — which is what
/// keeps the bit-exact path bit-exact — and otherwise, or for nobody asking,
/// the widest it does.
fn device_format(
    facts: &AudioEndpointFacts,
    wanted: Option<SampleFormat>,
) -> Result<SampleFormat, Errno> {
    if let Some(wanted) = wanted.filter(|format| facts.formats.contains(*format)) {
        return Ok(wanted);
    }
    FORMAT_PREFERENCE
        .iter()
        .copied()
        .find(|format| facts.formats.contains(*format))
        .ok_or(Errno::NotSupported)
}

/// What endpoint `index`, offering `facts`, is programmed with for the stream
/// `asked` opens — or, with nobody asking, as it runs with no stream to suit:
/// at its rate nearest the stack's default, in its widest encoding, with its
/// longest period. A monitor asks nothing, so a sink it hears runs as it
/// would without it.
///
/// # Errors
///
/// [`Errno::NotSupported`] for an endpoint offering no encoding this service
/// mixes.
pub(crate) fn configuration(
    facts: &AudioEndpointFacts,
    index: u16,
    asked: Option<&OpenParams>,
) -> Result<ConfigureParams, Errno> {
    let wanted = asked.map_or(Rate::HZ_48000, |open| open.rate);
    let rate = facts.rates.nearest(wanted);
    let target = asked.map_or(u32::MAX, |open| {
        scale_frames(open.latency_target_frames.max(2), open.rate, rate)
    });
    Ok(ConfigureParams {
        endpoint: index,
        rate,
        format: device_format(facts, asked.map(|open| open.format))?,
        channel_map: facts.channel_map,
        period_frames: device_period(facts, target, rate),
    })
}

/// The longest period, in milliseconds, an endpoint is programmed with.
///
/// The period is every stream's on the endpoint, and the first client fixes
/// it, so a client buffering deeply — a notification queueing its whole sound,
/// a player saving power — must not make every later stream that late. This
/// is a latency bound, not a capacity: a client's own ring still holds all it
/// asked for.
const MAX_PERIOD_MS: u32 = 20;

/// The period an endpoint running at `rate` is programmed with for a first
/// client wanting `target_frames` of latency there.
///
/// Half the target, so a ring two periods deep holds it, and never longer
/// than [`MAX_PERIOD_MS`], clamped into what the endpoint will actually do.
fn device_period(facts: &AudioEndpointFacts, target_frames: u32, rate: Rate) -> u32 {
    let longest =
        u32::try_from(u64::from(rate.hz()) * u64::from(MAX_PERIOD_MS) / 1_000).unwrap_or(u32::MAX);
    (target_frames / 2)
        .min(longest)
        .max(1)
        .clamp(facts.min_period_frames, facts.max_period_frames)
}

/// The longest period, in frames at `rate`, any stream could have endpoint
/// `facts` programmed with, rounded up: what a monitor's ring holds two of,
/// since the first stream to play has its sink configured afresh beneath it.
pub(crate) fn longest_period(facts: &AudioEndpointFacts, rate: Rate) -> u32 {
    // A period the endpoint's bounds clamp lasts longest at its slowest clock.
    let slowest = match facts.rates {
        RateSupport::Discrete(set) => set.rates().first().copied().unwrap_or(rate),
        RateSupport::Continuous { min, .. } => min,
    };
    let scaled = u64::from(rate.hz()) * u64::from(MAX_PERIOD_MS);
    let bound = u32::try_from(scaled.div_ceil(1_000)).unwrap_or(u32::MAX);
    bound
        .max(scale_frames(facts.min_period_frames, slowest, rate))
        .min(scale_frames(facts.max_period_frames, slowest, rate))
}

/// The power-of-two device ring `grant` admits: two periods where the
/// ceiling allows it, one otherwise.
///
/// Two periods is the least depth at which the driver can consume one while
/// the mixer fills the next; a ceiling that admits only one still yields a
/// working (if tighter) ring rather than a refusal.
fn device_ring_frames(grant: &ConfigureGrant) -> Result<u32, Errno> {
    let smallest = grant
        .period_frames
        .checked_next_power_of_two()
        .ok_or(Errno::OutOfRange)?;
    let frames = match smallest.checked_mul(2) {
        Some(doubled) if doubled <= grant.max_ring_frames => doubled,
        _ => smallest,
    };
    if frames > grant.max_ring_frames {
        return Err(Errno::OutOfRange);
    }
    Ok(frames)
}

/// `value` frames at `from` hertz, expressed at `to` hertz and rounded up so
/// a conversion never asks for less audio than was meant.
pub(crate) fn scale_frames(value: u32, from: Rate, to: Rate) -> u32 {
    if from == to {
        return value;
    }
    let scaled = (u64::from(value) * u64::from(to.hz())).div_ceil(u64::from(from.hz()).max(1));
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

impl Device {
    /// Bind a discovered device channel: read its facts and every endpoint's.
    ///
    /// # Errors
    ///
    /// The driver's typed refusal, or [`Errno::NotFound`] when it presents no
    /// endpoint at all — a device with nothing to play or record is a
    /// malformed emission, not a silent sink.
    pub(crate) fn bind(
        channel_endpoint: u64,
        location: u64,
        notify_endpoint: u64,
        transport: Box<dyn AudioChannelTransport>,
        first_device_id: u32,
    ) -> Result<Self, Errno> {
        let mut channel = AudioChannelClient::new(transport);
        let facts = channel.facts()?;
        // Before the endpoints are read, so a connector that changes between
        // the two is still told.
        channel.watch(notify_endpoint)?;
        let mut endpoints = Vec::new();
        for index in 0..facts.endpoints {
            let endpoint_facts = channel.endpoint_facts(index)?;
            endpoint_facts.validate()?;
            if !fallible::reserve(&mut endpoints, 1) {
                return Err(Errno::OutOfMemory);
            }
            endpoints.push(Endpoint {
                index,
                // An id carrying the monitor flag would name a sink's mix.
                device_id: first_device_id
                    .checked_add(u32::from(index))
                    .filter(|id| id & MONITOR_FLAG == 0)
                    .ok_or(Errno::OutOfRange)?,
                location: AudioLocation::new(location, index)?,
                control: endpoint_facts.gain,
                facts: endpoint_facts,
                level: EndpointLevel::UNITY,
                lost_frames: 0,
                active: None,
            });
        }
        if endpoints.is_empty() {
            return Err(Errno::NotFound);
        }
        Ok(Self {
            channel_endpoint,
            notify_endpoint,
            channel,
            endpoints,
            lost: false,
        })
    }
}

/// Program endpoint `slot` as `params` ask, create the device ring, and
/// attach it, recorded as attached at `attached_at`. One already programmed is
/// left as it runs: a reconfiguration is a device-wide event, so its caller
/// releases the endpoint first.
///
/// # Errors
///
/// The driver's typed refusal, or the region host's.
pub(crate) fn configure_endpoint<H: RegionHost>(
    device: &mut Device,
    slot: usize,
    regions: &mut H,
    params: ConfigureParams,
    dither_seed: u64,
    attached_at: Time64,
) -> Result<(), Errno> {
    let endpoint = device.endpoints.get_mut(slot).ok_or(Errno::NotFound)?;
    if endpoint.active.is_some() {
        return Ok(());
    }
    let facts = endpoint.facts;
    let grant = device.channel.configure(params)?;
    let resolution = facts.resolved_bits(grant.format);
    match attach_ring(
        device,
        regions,
        params,
        grant,
        resolution,
        dither_seed,
        attached_at,
    ) {
        Ok(active) => {
            if let Some(endpoint) = device.endpoints.get_mut(slot) {
                endpoint.active = Some(active);
            }
            Ok(())
        }
        Err(err) => {
            // The driver holds a configuration until it is detached.
            let _ = device.channel.detach(params.endpoint);
            Err(err)
        }
    }
}

/// Create the ring `grant` shapes, attach it to the endpoint `asked` named,
/// and build the state the endpoint is mixed through — onto the grid of
/// `resolution` bits, attached at `attached_at` — the ring released on any
/// refusal.
fn attach_ring<H: RegionHost>(
    device: &mut Device,
    regions: &mut H,
    asked: ConfigureParams,
    grant: ConfigureGrant,
    resolution: u32,
    dither_seed: u64,
    attached_at: Time64,
) -> Result<Active, Errno> {
    let ring_frames = device_ring_frames(&grant)?;
    let geometry = grant.geometry(ring_frames)?;
    let region = regions.create(geometry.region_len())?;
    let active = regions
        .grant(region, device.channel_endpoint)
        .and_then(|region_grant| {
            device.channel.attach(AttachParams {
                endpoint: asked.endpoint,
                ring_frames,
                region_grant,
                notify_endpoint: device.notify_endpoint,
            })
        })
        .and_then(|()| {
            let sink = SinkFormat {
                format: grant.format,
                resolution,
                rate: grant.rate,
                channel_map: grant.channel_map,
            };
            let period = grant.period_frames as usize;
            Ok(Active {
                asked,
                grant,
                geometry,
                region,
                attached_at,
                clock: ClockModel::new(grant.rate),
                mixer: Mixer::new(sink, period, dither_seed)?,
                sink,
                period: fallible::filled(period * sink.frame_bytes(), 0u8)
                    .ok_or(Errno::OutOfMemory)?,
                pivot: fallible::filled(period * sink.channels(), 0.0f32)
                    .ok_or(Errno::OutOfMemory)?,
                banks: Vec::new(),
                tap: None,
                running: false,
                position: Frames::ZERO,
                xrun_frames: 0,
            })
        });
    if active.is_err() {
        regions.release(region);
    }
    active
}

/// Whether `facts` still admit the configuration `grant` runs under.
pub(crate) fn admits(facts: &AudioEndpointFacts, grant: &ConfigureGrant) -> bool {
    facts.formats.contains(grant.format)
        && facts.channel_map == grant.channel_map
        && facts.rates.nearest(grant.rate) == grant.rate
}

/// Release a programmed endpoint: detach the driver's view of the ring and
/// drop this service's.
pub(crate) fn release_endpoint<H: RegionHost>(device: &mut Device, slot: usize, regions: &mut H) {
    let Some(endpoint) = device.endpoints.get_mut(slot) else {
        return;
    };
    let Some(active) = endpoint.active.take() else {
        return;
    };
    if !device.lost {
        let _ = device.channel.detach(endpoint.index);
    }
    regions.release(active.region);
}

/// Give a stream's share of its endpoint's phase bank back, so it holds
/// nothing of `active` any more.
pub(crate) fn release_stream_bank(active: &mut Active, stream: &mut Stream) {
    if let Some(bank) = stream.work.bank.take() {
        active.release_bank(bank);
    }
}

/// What one pump of an endpoint did.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Pumped {
    /// Device frames published into (or taken from) the device ring.
    pub(crate) moved: u32,
    /// Every live stream on the endpoint is draining and has nothing left,
    /// so the drain may be passed down to the device: never while a monitor
    /// keeps it clocking.
    pub(crate) drained: bool,
    /// A stream was faulted, so what is live on the endpoint changed.
    pub(crate) faulted: bool,
}

/// Move frames for one endpoint: refill a sink, its monitors hearing what the
/// device is given, or harvest a source.
///
/// A stream whose own ring fails is faulted alone and never fails the pump,
/// as are a sink's monitors when the mix they hear cannot be made.
///
/// # Errors
///
/// A corrupt device ring or a device region the host no longer holds, as a
/// typed [`Errno`]. The caller treats a fault as the device being lost rather
/// than retrying into it.
pub(crate) fn pump_endpoint<H: RegionHost>(
    device: &mut Device,
    device_index: usize,
    slot: usize,
    streams: &mut [Stream],
    regions: &mut H,
    notifier: &mut dyn Wake,
    sink: &dyn Sink,
) -> Result<Pumped, Errno> {
    let endpoint = device.endpoints.get_mut(slot).ok_or(Errno::NotFound)?;
    let Some(active) = endpoint.active.as_mut() else {
        return Ok(Pumped::default());
    };
    let pump = Pump {
        device_index,
        slot,
        notifier,
        sink,
        faulted: false,
    };
    match endpoint.facts.direction {
        StreamDirection::Playback => refill_sink(active, streams, regions, pump),
        StreamDirection::Capture => harvest_source(active, streams, regions, pump),
    }
}

/// Which endpoint a pump serves, where what it does is told, and whether it
/// faulted a stream.
struct Pump<'a> {
    device_index: usize,
    slot: usize,
    notifier: &'a mut dyn Wake,
    sink: &'a dyn Sink,
    faulted: bool,
}

impl Pump<'_> {
    /// Stop `stream` alone for `err`, recorded as `why`.
    fn fault(&mut self, stream: &mut Stream, err: Errno, why: &'static str) {
        stream.fault(err, why, self.notifier, self.sink);
        self.faulted = true;
    }
}

/// What a stream whose own ring broke the protocol is recorded as.
const RING_BROKEN: &str = "a client's ring broke the protocol; its stream alone was stopped";

/// Move what the ring now holds onto the device, and adopt what the driver
/// reports.
///
/// The steady state is driver-driven: a period interrupt wakes the driver,
/// it moves a period itself and notifies the mixer. A stream that has never
/// run has interrupted nothing, so unless the first periods are posted here
/// the device is clocked with an empty queue, never completes a transfer,
/// and so never interrupts — the ring stays full and the stream never
/// advances.
pub(crate) fn prime_endpoint(device: &mut Device, slot: usize) -> Result<(), Errno> {
    let index = device.endpoints.get(slot).ok_or(Errno::NotFound)?.index;
    let report = device.channel.service(index)?;
    if let Some(active) = device
        .endpoints
        .get_mut(slot)
        .and_then(|endpoint| endpoint.active.as_mut())
    {
        active.position = report.position;
        let _ = active.clock.observe(report.position, report.sampled_at);
    }
    Ok(())
}

/// Drop everything a source captured that the mixer has not taken.
///
/// # Errors
///
/// A corrupt device ring or a region the host no longer holds: the device's
/// fault.
pub(crate) fn discard_captured<H: RegionHost>(
    device: &mut Device,
    slot: usize,
    regions: &mut H,
) -> Result<(), Errno> {
    let Some(active) = device
        .endpoints
        .get_mut(slot)
        .and_then(|endpoint| endpoint.active.as_mut())
    else {
        return Ok(());
    };
    let bytes = regions.bytes(active.region)?;
    let mut ring = PcmRing::bind(bytes, active.geometry)?;
    let captured = ring.readable_frames()?;
    ring.discard(captured)?;
    Ok(())
}

/// What the playback streams live on a sink gave one chunk.
struct Pulled {
    /// Any was live.
    live: bool,
    /// Every one is draining or stops inside the chunk, so the chunk is cut
    /// to what they had rather than padded, unless a monitor keeps the sink
    /// clocking.
    finishing: bool,
    /// The most frames any of them gave.
    covered: usize,
}

/// Fill the device ring with mixed periods while it has room and a live
/// stream has something to say, handing each to the sink's live monitors.
///
/// A live monitor keeps the sink clocking, so every chunk is whole: with
/// nothing playing it is the device's silence, and a draining stream that
/// runs dry is padded rather than cut, the device frame its last one ends on
/// marked for its drain to be settled at.
fn refill_sink<H: RegionHost>(
    active: &mut Active,
    streams: &mut [Stream],
    regions: &mut H,
    mut pump: Pump<'_>,
) -> Result<Pumped, Errno> {
    let mut pumped = Pumped::default();
    loop {
        let room = ring_room(active, regions)?;
        let chunk = usize::try_from(active.period_frames().min(room)).unwrap_or(0);
        if chunk == 0 {
            break;
        }
        let pulled = pull_streams(active, streams, regions, &mut pump, chunk);
        let monitored = streams
            .iter()
            .any(|stream| stream.hears(pump.device_index, pump.slot));
        if !pulled.live && !monitored {
            break;
        }
        let frames = if pulled.live && pulled.finishing && !monitored {
            pulled.covered.min(chunk)
        } else {
            chunk
        };
        if frames == 0 {
            pumped.drained = true;
            break;
        }
        if monitored {
            // The device reaches this chunk once it has taken what the ring
            // already holds.
            let queued = active.geometry.frames().saturating_sub(room);
            let start = advance(active.position, u64::from(queued));
            mark_drain_ends(streams, &pump, start, frames);
        }
        let Active {
            mixer,
            period,
            sink,
            ..
        } = active;
        let bytes = frames * sink.frame_bytes();
        let contributions = streams
            .iter()
            .filter(|stream| stream.feeds(pump.device_index, pump.slot))
            .filter_map(|stream| stream.contribution(stream.software_gain));
        mixer.mix(contributions, frames, &mut period[..bytes])?;
        let published = write_device_ring(active, regions, bytes)?;
        pumped.moved = pumped.moved.saturating_add(published);
        if monitored {
            hear_mix(active, streams, regions, &mut pump, published as usize);
        }
        account_shortfall(active, streams, &mut pump, frames);
        if published == 0 {
            break;
        }
    }
    pumped.faulted = pump.faulted;
    wake_clients(pump.device_index, pump.slot, streams, pump.notifier);
    Ok(pumped)
}

/// Pull up to `chunk` frames from every playback stream live on the sink. A
/// stream whose own ring fails is faulted alone.
fn pull_streams<H: RegionHost>(
    active: &Active,
    streams: &mut [Stream],
    regions: &mut H,
    pump: &mut Pump<'_>,
    chunk: usize,
) -> Pulled {
    let mut pulled = Pulled {
        live: false,
        finishing: true,
        covered: 0,
    };
    for stream in streams.iter_mut() {
        if !stream.feeds(pump.device_index, pump.slot) {
            continue;
        }
        // A stream stopping at a named frame takes no more than the frames
        // left before it, so the stop lands exactly there.
        let cap = stream.stop_limit().map_or(chunk, |limit| limit.min(chunk));
        match pull_playback(stream, active, regions, cap) {
            Ok(covered) => {
                pulled.live = true;
                pulled.finishing &= stream.is_draining() || cap < chunk;
                pulled.covered = pulled.covered.max(covered);
            }
            Err(err) => pump.fault(stream, err, RING_BROKEN),
        }
    }
    pulled
}

/// Account what each running playback stream on the sink could not give a
/// chunk of `frames`: it was mixed as silence for them. A draining or
/// stopping stream asked for exactly what it had.
fn account_shortfall(active: &Active, streams: &mut [Stream], pump: &mut Pump<'_>, frames: usize) {
    for stream in streams.iter_mut() {
        if !stream.feeds(pump.device_index, pump.slot) {
            continue;
        }
        let covered = stream.work.covered;
        if !stream.is_draining() && stream.stop_at.is_none() && covered < frames {
            let short = u32::try_from(frames - covered).unwrap_or(u32::MAX);
            let lost = scale_frames(short, active.grant.rate, stream.grant.rate);
            stream.account_loss(u64::from(lost), pump.notifier);
        }
        stream.work.covered = 0;
    }
}

/// Mark where each draining stream fed into a chunk of `frames` beginning at
/// device frame `start` ran dry: the frame its last one ends on, which its
/// drain is settled at once the device reaches it. One that filled the chunk
/// has not ended, and one that gave nothing ended where its mark already
/// stands, or by the chunk's start.
fn mark_drain_ends(streams: &mut [Stream], pump: &Pump<'_>, start: Frames, frames: usize) {
    for stream in streams.iter_mut() {
        if !stream.feeds(pump.device_index, pump.slot) || !stream.is_draining() {
            continue;
        }
        stream.drain_end = match stream.work.covered {
            0 => stream.drain_end.or(Some(start)),
            covered if covered >= frames => None,
            covered => Some(advance(start, covered as u64)),
        };
    }
}

/// Give every live monitor of the sink the `frames` the device was just given,
/// as they sounded before the sink's own level and mute: the device's own
/// period where the mixer took none of that level, else the period mixed again
/// without it. A reader that is behind takes the over-run, so the device never
/// waits on a monitor; one whose ring fails is faulted alone, and a mix the
/// tap cannot make faults the monitors and nothing the device plays.
fn hear_mix<H: RegionHost>(
    active: &mut Active,
    streams: &mut [Stream],
    regions: &mut H,
    pump: &mut Pump<'_>,
    frames: usize,
) {
    let (device_index, slot) = (pump.device_index, pump.slot);
    let as_played = streams
        .iter()
        .filter(|stream| stream.feeds(device_index, slot))
        .all(Stream::heard_as_played);
    let Active {
        sink: format,
        period,
        pivot,
        tap,
        ..
    } = active;
    let heard = if as_played {
        Ok(&period[..frames * format.frame_bytes()])
    } else {
        tap.as_mut()
            .ok_or(Errno::NotConnected)
            .and_then(|tap| tap.remix(streams, device_index, slot, frames))
    };
    let heard = match heard {
        Ok(heard) => heard,
        Err(err) => {
            for stream in streams
                .iter_mut()
                .filter(|stream| stream.hears(device_index, slot))
            {
                pump.fault(
                    stream,
                    err,
                    "a sink's monitors could not be given its mix; they alone were stopped",
                );
            }
            return;
        }
    };
    let decoded = convert::decode(
        format.format,
        heard,
        &mut pivot[..frames * format.channels()],
    );
    for stream in streams.iter_mut() {
        if !stream.hears(device_index, slot) {
            continue;
        }
        if let Err(err) = push_capture(stream, active, regions, decoded, pump.notifier) {
            pump.fault(stream, err, RING_BROKEN);
        }
    }
}

/// Take the device's captured periods and fan them out to every live capture
/// stream on the endpoint.
fn harvest_source<H: RegionHost>(
    active: &mut Active,
    streams: &mut [Stream],
    regions: &mut H,
    mut pump: Pump<'_>,
) -> Result<Pumped, Errno> {
    let (device_index, slot) = (pump.device_index, pump.slot);
    let mut pumped = Pumped::default();
    loop {
        let period_bytes = active.period_frames() as usize * active.sink.frame_bytes();
        let taken = {
            let bytes = regions.bytes(active.region)?;
            let mut ring = PcmRing::bind(bytes, active.geometry)?;
            ring.read(&mut active.period[..period_bytes])?
        };
        if taken == 0 {
            break;
        }
        pumped.moved = pumped.moved.saturating_add(taken);
        let frames = taken as usize;
        let decoded = convert::decode(
            active.sink.format,
            &active.period[..frames * active.sink.frame_bytes()],
            &mut active.pivot[..frames * active.sink.channels()],
        );
        for stream in streams.iter_mut() {
            if !stream.on(device_index, slot) || !stream.is_live() {
                continue;
            }
            if let Err(err) = push_capture(stream, active, regions, decoded, pump.notifier) {
                pump.fault(stream, err, RING_BROKEN);
            }
        }
        if streams
            .iter()
            .all(|stream| !(stream.on(device_index, slot) && stream.is_live()))
        {
            break;
        }
    }
    pumped.drained = streams
        .iter()
        .filter(|stream| stream.on(device_index, slot) && stream.is_live())
        .all(Stream::is_draining);
    pumped.faulted = pump.faulted;
    wake_clients(device_index, slot, streams, pump.notifier);
    Ok(pumped)
}

/// Frames the device ring can still take.
fn ring_room<H: RegionHost>(active: &mut Active, regions: &mut H) -> Result<u32, Errno> {
    let bytes = regions.bytes(active.region)?;
    let ring = PcmRing::bind(bytes, active.geometry)?;
    ring.writable_frames()
}

/// Publish the first `bytes` of the endpoint's staged period into the device
/// ring, answering the frames the ring took.
fn write_device_ring<H: RegionHost>(
    active: &mut Active,
    regions: &mut H,
    bytes: usize,
) -> Result<u32, Errno> {
    let staged = &active.period[..bytes];
    let region = regions.bytes(active.region)?;
    let mut ring = PcmRing::bind(region, active.geometry)?;
    ring.write(staged)
}

/// Read up to one chunk's worth of a playback stream's frames, filtering them
/// to the device's rate where the two differ, and answer the device frames it
/// can contribute.
fn pull_playback<H: RegionHost>(
    stream: &mut Stream,
    active: &Active,
    regions: &mut H,
    chunk: usize,
) -> Result<usize, Errno> {
    let Some(region) = stream.region else {
        stream.work.covered = 0;
        return Ok(0);
    };
    let channels = stream.channels();
    let format = stream.grant.format;
    let frame_bytes = channels * format.bytes_per_sample();
    let Stream {
        geometry,
        position,
        work,
        ..
    } = stream;
    if work.resampler.is_none() {
        let wanted = (chunk * frame_bytes).min(work.bytes.len());
        let got = {
            let bytes = regions.bytes(region)?;
            let mut ring = PcmRing::bind(bytes, *geometry)?;
            ring.read(&mut work.bytes[..wanted])?
        };
        *position = advance(*position, u64::from(got));
        work.covered = got as usize;
        return Ok(work.covered);
    }
    let bank = work
        .bank
        .and_then(|bank| active.banks.get(bank))
        .and_then(Option::as_ref)
        .ok_or(Errno::NotFound)?;
    while work.pending_frames < chunk {
        let wanted = work.want_in * frame_bytes;
        let got = {
            let bytes = regions.bytes(region)?;
            let mut ring = PcmRing::bind(bytes, *geometry)?;
            ring.read(&mut work.bytes[..wanted])?
        } as usize;
        if got == 0 {
            break;
        }
        *position = advance(*position, got as u64);
        let Working {
            bytes,
            pivot,
            pending,
            pending_frames,
            resampler,
            ..
        } = &mut *work;
        let decoded = convert::decode(
            format,
            &bytes[..got * frame_bytes],
            &mut pivot[..got * channels],
        );
        let Some(filter) = resampler.as_mut() else {
            break;
        };
        let (_, made) = filter.process(
            &bank.bank,
            &pivot[..decoded],
            &mut pending[*pending_frames * channels..],
        )?;
        *pending_frames += made;
        if made == 0 {
            break;
        }
    }
    let take = work.pending_frames.min(chunk);
    let samples = take * channels;
    convert::encode(
        RESAMPLED_FORMAT,
        &work.pending[..samples],
        &mut work.staged[..samples * RESAMPLED_FORMAT.bytes_per_sample()],
    );
    work.pending
        .copy_within(samples..work.pending_frames * channels, 0);
    work.pending_frames -= take;
    work.covered = take;
    Ok(take)
}

/// Map, filter and encode one captured period for a capture stream, and give
/// it to that stream's client ring.
fn push_capture<H: RegionHost>(
    stream: &mut Stream,
    active: &Active,
    regions: &mut H,
    decoded: usize,
    notifier: &mut dyn Wake,
) -> Result<(), Errno> {
    let Some(region) = stream.region else {
        return Ok(());
    };
    let device_channels = active.sink.channels();
    if device_channels == 0 {
        return Err(Errno::OutOfRange);
    }
    let frames = decoded / device_channels;
    let channels = stream.channels();
    let format = stream.grant.format;
    let work = &mut stream.work;
    work.matrix.map(
        frames,
        stream.software_gain,
        &active.pivot[..frames * device_channels],
        &mut work.pivot[..frames * channels],
    )?;
    let produced = if let Some(filter) = work.resampler.as_mut() {
        let bank = work
            .bank
            .and_then(|bank| active.banks.get(bank))
            .and_then(Option::as_ref)
            .ok_or(Errno::NotFound)?;
        let (_, made) = filter.process(
            &bank.bank,
            &work.pivot[..frames * channels],
            &mut work.pending[..],
        )?;
        convert::encode(
            format,
            &work.pending[..made * channels],
            &mut work.bytes[..],
        );
        made
    } else {
        convert::encode(
            format,
            &work.pivot[..frames * channels],
            &mut work.bytes[..],
        );
        frames
    };
    let frame_bytes = channels * format.bytes_per_sample();
    let written = {
        let bytes = regions.bytes(region)?;
        let mut ring = PcmRing::bind(bytes, stream.geometry)?;
        ring.write(&work.bytes[..produced * frame_bytes])?
    } as usize;
    stream.position = advance(stream.position, written as u64);
    // The client's ring could not hold everything the device gave: the frames
    // that did not fit are over-run and are counted, never silently dropped.
    stream.account_loss((produced - written) as u64, notifier);
    Ok(())
}

/// Tell every live stream on the endpoint that its ring moved.
fn wake_clients(device_index: usize, slot: usize, streams: &[Stream], notifier: &mut dyn Wake) {
    for stream in streams {
        if !stream.on(device_index, slot) || !stream.is_live() {
            continue;
        }
        notifier.wake(
            stream.grant.notify_endpoint,
            AudioNotify::SpaceAvailable {
                stream_id: stream.id,
                position: stream.position,
            },
        );
    }
}

/// `position` advanced by `frames`, saturating rather than wrapping — a real
/// stream cannot reach the end of a `u64` frame counter, and pinning it there
/// is a stopped clock rather than a position that lies.
fn advance(position: Frames, frames: u64) -> Frames {
    position
        .checked_add(frames)
        .unwrap_or_else(|| Frames::new(u64::MAX))
}

#[cfg(test)]
mod tests {
    use tairix_abi::audio::{OpenParams, StreamRole, Tenancy};
    use tairix_abi::driver::audio::{
        AudioEndpointFacts, AudioName, ChannelMap, JackState, Rate, RateSupport, SampleFormat,
        SampleFormats, StreamDirection,
    };

    use super::{configuration, device_period, longest_period, scale_frames};

    fn rate(hz: u32) -> Rate {
        Rate::new(hz).expect("a rate inside the vocabulary")
    }

    fn facts(min: u32, max: u32) -> AudioEndpointFacts {
        AudioEndpointFacts {
            index: 0,
            direction: StreamDirection::Playback,
            jack: JackState::Present,
            formats: SampleFormats::EMPTY.with(SampleFormat::S16),
            resolution: None,
            channel_map: ChannelMap::STEREO,
            rates: RateSupport::Continuous {
                min: Rate::HZ_48000,
                max: Rate::HZ_48000,
            },
            min_period_frames: min,
            max_period_frames: max,
            max_ring_frames: 1 << 16,
            gain: None,
            name: AudioName::new("period").expect("a short name"),
        }
    }

    /// A first client buffering a whole notification — 36 480 frames, as the
    /// 2026-10-17 boot's cue did — no longer programs a 16 384-frame
    /// period every later stream on the endpoint waits behind.
    #[test]
    fn a_deep_first_client_does_not_make_the_period_long() {
        assert_eq!(
            device_period(&facts(16, 16_384), 36_480, Rate::HZ_48000),
            960
        );
    }

    #[test]
    fn a_shallow_client_keeps_its_short_period_within_the_endpoints_bounds() {
        let endpoint = facts(64, 16_384);
        assert_eq!(device_period(&endpoint, 512, Rate::HZ_48000), 256);
        assert_eq!(device_period(&endpoint, 2, Rate::HZ_48000), 64);
        assert_eq!(
            device_period(&facts(2_048, 4_096), 36_480, Rate::HZ_48000),
            2_048
        );
    }

    /// A stream at `hz` wanting `latency` frames, as a client opens one.
    fn asking(hz: u32, latency: u32) -> OpenParams {
        OpenParams {
            device_id: 0,
            direction: StreamDirection::Playback,
            format: SampleFormat::S16,
            rate: rate(hz),
            channel_map: ChannelMap::STEREO,
            role: StreamRole::Media,
            latency_target_frames: latency,
            tenancy: Tenancy::Caller,
        }
    }

    /// With nobody asking — a monitor opening on an idle sink — an endpoint
    /// runs at its rate nearest the stack's default, in its widest encoding,
    /// with its longest period; a stream asking has its own.
    #[test]
    fn an_endpoint_nobody_asks_of_runs_as_its_own() {
        let mut endpoint = facts(64, 4_096);
        endpoint.formats = SampleFormats::EMPTY
            .with(SampleFormat::S16)
            .with(SampleFormat::S32);
        endpoint.rates = RateSupport::Continuous {
            min: rate(8_000),
            max: rate(96_000),
        };
        let own = configuration(&endpoint, 0, None).expect("a configuration");
        assert_eq!(
            (own.rate, own.format, own.period_frames),
            (Rate::HZ_48000, SampleFormat::S32, 960)
        );
        let theirs =
            configuration(&endpoint, 0, Some(&asking(24_000, 512))).expect("a configuration");
        assert_eq!(
            (theirs.rate, theirs.format, theirs.period_frames),
            (rate(24_000), SampleFormat::S16, 256)
        );
    }

    /// No period any stream could have an endpoint run at outlasts the bound
    /// a monitor's ring is sized for, at whatever rate the monitor hears.
    #[test]
    fn no_period_outlasts_the_bound_a_monitors_ring_holds() {
        for (min, max) in [(64, 4_096), (2_048, 4_096), (256, 256)] {
            let mut endpoint = facts(min, max);
            endpoint.rates = RateSupport::Continuous {
                min: rate(8_000),
                max: rate(48_000),
            };
            for heard in [8_000, 24_000, 44_100, 48_000] {
                let bound = longest_period(&endpoint, rate(heard));
                for hz in [8_000, 11_025, 22_050, 44_100, 48_000] {
                    for target in [2, 512, 8_192, u32::MAX] {
                        let period = device_period(&endpoint, target, rate(hz));
                        let at_heard = scale_frames(period, rate(hz), rate(heard));
                        assert!(
                            at_heard <= bound,
                            "{min}..{max}: {at_heard} past {bound} at {heard} Hz"
                        );
                    }
                }
            }
        }
        assert_eq!(longest_period(&facts(256, 256), Rate::HZ_48000), 256);
    }
}

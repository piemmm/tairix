use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::driver::audio::{ChannelMap, SampleFormat};
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, REGION_ALIGN_PADDING};
use tairix_abi::usb_urb::{IsoPacket, IsoPacketStatus};

use super::*;
use crate::fixtures::{headset_v2, implicit_v2, qemu_usb_audio, shared_clock_v2};

/// The host controller every test stream's region is delegated by.
const HCD: ProcId = ProcId::from_raw([0x7E; tairix_abi::PROC_ID_LEN]);

/// A process that is not the host controller.
const STRANGER: ProcId = ProcId::from_raw([0x51; tairix_abi::PROC_ID_LEN]);

/// The fixtures number their clock entities from here and their units and
/// terminals below it, which is how the mock tells a clock's control from a
/// unit's sharing its selector code.
const CLOCK_IDS: u8 = 0x20;

/// One stream the mock host controller runs.
struct MockStream {
    endpoint: u8,
    number: NonZeroU32,
    layout: IsoLayout,
    region: Vec<u8>,
    queued: VecDeque<u16>,
}

/// A device and its host controller, as the engine sees them.
#[derive(Default)]
struct Mock {
    speed: Option<UsbSpeed>,
    /// Every control request, with the data it carried or asked for.
    requests: Vec<([u8; 8], Vec<u8>)>,
    /// Every claim made, in order.
    claims: Vec<u8>,
    /// The interfaces the host controller holds claimed now: a reset forgets
    /// them, and selecting a setting needs one.
    claimed: Vec<u8>,
    /// Every setting selected, in order.
    settings: Vec<(u8, u8)>,
    /// The setting each interface holds now, which a reset forgets: a stream
    /// starts only on an interface holding one with endpoints.
    live: Vec<(u8, u8)>,
    /// Version 1.0 volume `(min, max, res)`.
    volume_range: (i16, i16, i16),
    /// Each `(unit, selector, channel)` set, and its value.
    set: Vec<((u8, u8, u8), Vec<u8>)>,
    /// Version 1.0: each endpoint's frequency.
    endpoint_frequency: Vec<(u8, u32)>,
    /// Version 2.0: each clock source's frequency.
    clock_frequency: Vec<(u8, u32)>,
    /// Version 2.0: the frequency subranges every clock source offers.
    clock_ranges: Vec<(u32, u32, u32)>,
    /// Requests to answer with a STALL: `(bRequest, selector)`.
    stalls: Vec<(u8, u8)>,
    /// Requests to answer with a STALL once each, in turn.
    stall_next: Vec<(u8, u8)>,
    /// Version 2.0: every clock source reports itself not running.
    clock_invalid: bool,
    streams: Vec<MockStream>,
    started: Vec<IsoStartParams>,
    stopped: Vec<u8>,
    /// What every slot queued is refused with, if anything.
    queue_refusal: Option<Errno>,
    /// Endpoints whose streams the host controller refuses to start.
    start_refusals: Vec<u8>,
    /// The number the latest stream took.
    last_stream: u32,
    notices: VecDeque<(ProcId, [u8; ISO_NOTIFY_LEN])>,
    now: u64,
}

impl Mock {
    fn new(speed: UsbSpeed) -> Self {
        Self {
            speed: Some(speed),
            volume_range: (-32767, 0x0800, 0x0088),
            clock_ranges: vec![(44_100, 44_100, 0), (48_000, 96_000, 48_000)],
            ..Self::default()
        }
    }

    fn stream(&mut self, endpoint: u8) -> &mut MockStream {
        self.streams
            .iter_mut()
            .find(|stream| stream.endpoint == endpoint)
            .expect("a running stream")
    }

    /// Finish the oldest queued slot of `endpoint`, every interval moved and
    /// an IN interval carrying `frames` frames of `fill`, reported `skipped`
    /// intervals late by `from`.
    fn finish_from(
        &mut self,
        from: ProcId,
        endpoint: u8,
        skipped: u32,
        frames: u32,
        fill: u8,
    ) -> u16 {
        let now = self.now;
        let stream = self.stream(endpoint);
        let number = stream.number;
        let slot = stream.queued.pop_front().expect("a queued slot");
        if endpoint & 0x80 != 0 {
            for packet in 0..stream.layout.packets {
                let bytes = frames * 4;
                let data = stream
                    .layout
                    .data_mut(&mut stream.region, slot, packet)
                    .expect("in place");
                data[..bytes as usize].fill(fill);
                stream
                    .layout
                    .set_record(
                        &mut stream.region,
                        slot,
                        packet,
                        IsoPacket {
                            length: bytes,
                            status: IsoPacketStatus::Moved,
                        },
                    )
                    .expect("in place");
            }
        }
        let notice = IsoNotify::SlotDone {
            endpoint,
            stream: number,
            slot,
            skipped,
            microframe: 0,
            completed_at: now,
        };
        self.notices.push_back((from, notice.encode()));
        self.now += 1_000_000;
        slot
    }

    fn finish(&mut self, endpoint: u8) -> u16 {
        self.finish_from(HCD, endpoint, 0, 48, 0)
    }

    fn halt(&mut self, endpoint: u8, reason: Errno) {
        let stream = self.stream(endpoint).number;
        self.notices.push_back((
            HCD,
            IsoNotify::Halted {
                endpoint,
                stream,
                reason,
            }
            .encode(),
        ));
    }

    /// A controller reset: every stream ends reissuably, and the device
    /// forgets its claims, its settings and its controls.
    fn reset(&mut self) {
        for stream in core::mem::take(&mut self.streams) {
            self.notices.push_back((
                HCD,
                IsoNotify::Halted {
                    endpoint: stream.endpoint,
                    stream: stream.number,
                    reason: Errno::WouldBlock,
                }
                .encode(),
            ));
        }
        self.claimed.clear();
        self.live.clear();
        self.set.clear();
        self.endpoint_frequency.clear();
        self.clock_frequency.clear();
    }

    /// Whether `(request, control)` is to be answered with a STALL.
    fn stalls(&mut self, request: u8, control: u8) -> bool {
        if let Some(at) = self
            .stall_next
            .iter()
            .position(|&next| next == (request, control))
        {
            self.stall_next.remove(at);
            return true;
        }
        self.stalls.contains(&(request, control))
    }

    fn value(&self, unit: u8, control: u8, channel: u8) -> Option<&[u8]> {
        self.set
            .iter()
            .rev()
            .find(|(at, _)| *at == (unit, control, channel))
            .map(|(_, value)| value.as_slice())
    }
}

impl UacTransport for Mock {
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        let [kind, request, channel, control, _, entity, _, _] = setup;
        self.requests.push((setup, data.to_vec()));
        if self.stalls(request, control) {
            return Err(Errno::EndpointStalled);
        }
        let mut answer: Vec<u8> = match (kind, request) {
            // Version 1.0 volume range.
            (0xA1, requests::v1::GET_MIN) => self.volume_range.0.to_le_bytes().to_vec(),
            (0xA1, requests::v1::GET_MAX) => self.volume_range.1.to_le_bytes().to_vec(),
            (0xA1, requests::v1::GET_RES) => self.volume_range.2.to_le_bytes().to_vec(),
            (0xA2, requests::v1::GET_CUR) => {
                let hz = self
                    .endpoint_frequency
                    .iter()
                    .rev()
                    .find(|(endpoint, _)| *endpoint == setup[4])
                    .map_or(48_000, |(_, hz)| *hz);
                requests::frequency_v1(hz).to_vec()
            }
            (0xA1, requests::v2::RANGE) if control == selector::CLOCK_FREQUENCY => {
                let mut block = u16::try_from(self.clock_ranges.len())
                    .expect("few")
                    .to_le_bytes()
                    .to_vec();
                for (min, max, res) in &self.clock_ranges {
                    block.extend_from_slice(&min.to_le_bytes());
                    block.extend_from_slice(&max.to_le_bytes());
                    block.extend_from_slice(&res.to_le_bytes());
                }
                block
            }
            (0xA1, requests::v2::RANGE) if control == selector::VOLUME => {
                let (min, max, res) = self.volume_range;
                let mut block = 1u16.to_le_bytes().to_vec();
                for value in [min, max, res] {
                    block.extend_from_slice(&value.to_le_bytes());
                }
                block
            }
            (0xA1, requests::v2::CUR) if entity >= CLOCK_IDS => match (control, data.len()) {
                (selector::CLOCK_FREQUENCY, 4) => {
                    let hz = self
                        .clock_frequency
                        .iter()
                        .rev()
                        .find(|(clock, _)| *clock == entity)
                        .map_or(48_000, |(_, hz)| *hz);
                    hz.to_le_bytes().to_vec()
                }
                (selector::CLOCK_VALID, 1) => vec![u8::from(!self.clock_invalid)],
                // A selector reads the pin last set, or its first.
                _ => self
                    .value(entity, control, channel)
                    .map_or_else(|| vec![1], <[u8]>::to_vec),
            },
            (0xA1, requests::v2::CUR) => self
                .value(entity, control, channel)
                .map_or_else(|| vec![0], <[u8]>::to_vec),
            _ => return Err(Errno::EndpointStalled),
        };
        answer.truncate(data.len());
        data[..answer.len()].copy_from_slice(&answer);
        Ok(answer.len())
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        let [kind, request, channel, control, _, entity, _, _] = setup;
        self.requests.push((setup, data.to_vec()));
        if self.stalls(request, control) {
            return Err(Errno::EndpointStalled);
        }
        match kind {
            0x22 => self.endpoint_frequency.push((
                setup[4],
                requests::read_frequency_v1(data).expect("three bytes"),
            )),
            0x21 if entity >= CLOCK_IDS
                && control == selector::CLOCK_FREQUENCY
                && data.len() == 4 =>
            {
                self.clock_frequency
                    .push((entity, requests::read_u32(data).expect("four bytes")));
            }
            _ => self.set.push(((entity, control, channel), data.to_vec())),
        }
        Ok(())
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), Errno> {
        self.claims.push(interface);
        if !self.claimed.contains(&interface) {
            self.claimed.push(interface);
        }
        Ok(())
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) -> Result<(), Errno> {
        if !self.claimed.contains(&interface) {
            return Err(Errno::NotFound);
        }
        self.settings.push((interface, alternate));
        self.live.retain(|&(held, _)| held != interface);
        self.live.push((interface, alternate));
        Ok(())
    }

    fn iso_start(&mut self, params: IsoStartParams) -> Result<IsoGrant, Errno> {
        let speed = self.speed.expect("a speed");
        if self.start_refusals.contains(&params.endpoint) {
            return Err(Errno::NoBandwidth);
        }
        // Every fixture numbers an endpoint after the interface carrying it.
        let interface = params.endpoint & 0x0F;
        if !self
            .live
            .iter()
            .any(|&(held, alternate)| held == interface && alternate != 0)
        {
            return Err(Errno::NotFound);
        }
        self.started.push(params);
        self.last_stream += 1;
        let number = NonZeroU32::new(self.last_stream).expect("counted from one");
        self.streams
            .retain(|stream| stream.endpoint != params.endpoint);
        self.streams.push(MockStream {
            endpoint: params.endpoint,
            number,
            layout: params.layout,
            region: vec![0; params.layout.region_len()],
            queued: VecDeque::new(),
        });
        // The interval the host controller reads from the endpoint: the
        // fixtures' every endpoint states `bInterval` 1, but for the
        // headset's feedback endpoint, which states 4.
        let b_interval = if params.endpoint == 0x81 { 4 } else { 1 };
        Ok(IsoGrant {
            region_grant: 1,
            grantor: HCD,
            notify: 0,
            interval_microframes: ServiceInterval::isochronous(speed, b_interval)
                .expect("valid")
                .microframes(),
            speed,
            stream: number,
        })
    }

    fn iso_queue(&mut self, endpoint: u8, slot: u16) -> Result<(), Errno> {
        if let Some(errno) = self.queue_refusal {
            return Err(errno);
        }
        let stream = self
            .streams
            .iter_mut()
            .find(|stream| stream.endpoint == endpoint)
            .ok_or(Errno::NotFound)?;
        stream.queued.push_back(slot);
        Ok(())
    }

    fn iso_stop(&mut self, endpoint: u8) -> Result<(), Errno> {
        self.stopped.push(endpoint);
        self.streams.retain(|stream| stream.endpoint != endpoint);
        Ok(())
    }

    fn region(&mut self, endpoint: u8) -> Option<&mut [u8]> {
        self.streams
            .iter_mut()
            .find(|stream| stream.endpoint == endpoint)
            .map(|stream| stream.region.as_mut_slice())
    }

    fn next_notice(&mut self) -> Result<Option<(ProcId, [u8; ISO_NOTIFY_LEN])>, Errno> {
        Ok(self.notices.pop_front())
    }

    fn now_ns(&self) -> u64 {
        self.now
    }
}

/// A ring of `frames` frames in `format` and `channels`.
struct Ring {
    bytes: Vec<u8>,
    geometry: PcmGeometry,
}

impl Ring {
    fn new(frames: u32, format: SampleFormat, channels: u8) -> Self {
        let geometry = PcmGeometry::new(frames, format, channels).expect("valid");
        Self {
            bytes: vec![0u8; geometry.region_len() + REGION_ALIGN_PADDING],
            geometry,
        }
    }

    fn stereo() -> Self {
        Self::new(4096, SampleFormat::S16, 2)
    }

    fn bind(&mut self) -> PcmRing<'_> {
        let len = self.geometry.region_len();
        PcmRing::bind(
            aligned_region(&mut self.bytes, len).expect("padded"),
            self.geometry,
        )
        .expect("binds")
    }

    /// Queue `frames` frames of a counting ramp from `first`, one 16-bit
    /// count per sample.
    fn ramp(&mut self, first: u32, frames: u32) {
        let frame = self.geometry.frame_bytes();
        let channels = frame / 2;
        let mut bytes = Vec::new();
        for n in first..first + frames {
            for _ in 0..channels {
                bytes.extend_from_slice(&u16::try_from(n % 65_536).expect("held").to_le_bytes());
            }
        }
        assert_eq!(self.bind().write(&bytes), Ok(frames));
    }
}

fn params(
    endpoint: u16,
    rate: u32,
    format: SampleFormat,
    map: ChannelMap,
    period: u32,
) -> ConfigureParams {
    ConfigureParams {
        endpoint,
        rate: Rate::new(rate).expect("a rate"),
        format,
        channel_map: map,
        period_frames: period,
    }
}

fn qemu() -> UsbAudio<Mock> {
    UsbAudio::open(
        &qemu_usb_audio(),
        0,
        UsbSpeed::Full,
        Mock::new(UsbSpeed::Full),
    )
    .expect("QEMU's function opens")
}

/// QEMU's device configured for 48 kHz `s16` stereo in 480-frame periods.
fn qemu_configured() -> UsbAudio<Mock> {
    let mut audio = qemu();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio
}

/// The ramp values one OUT slot carries, interval by interval.
fn slot_samples(audio: &mut UsbAudio<Mock>, endpoint: u8, slot: u16) -> Vec<u16> {
    let stream = audio.transport_mut().stream(endpoint);
    let mut samples = Vec::new();
    for packet in 0..stream.layout.packets {
        let record = stream
            .layout
            .record(&stream.region, slot, packet)
            .expect("in place");
        let data = stream
            .layout
            .data(&stream.region, slot, packet)
            .expect("in place");
        for frame in data[..record.length as usize].as_chunks::<4>().0 {
            samples.push(u16::from_le_bytes([frame[0], frame[1]]));
        }
    }
    samples
}

#[test]
fn qemus_device_presents_one_speaker_with_its_feature_units_gain() {
    let audio = qemu();
    assert_eq!(audio.transport().claims, [1]);
    let device = audio.device_facts().expect("facts");
    assert_eq!(device.endpoints, 1);
    assert_eq!(device.name.as_str(), "USB Audio");
    let facts = audio.endpoint_facts(0).expect("facts");
    assert_eq!(facts.direction, StreamDirection::Playback);
    assert_eq!(facts.name.as_str(), "Speaker");
    assert!(facts.formats.contains(SampleFormat::S16));
    assert_eq!(facts.channel_map, ChannelMap::STEREO);
    assert!(facts.rates.admits(Rate::HZ_48000));
    let gain = facts.gain.expect("a volume control");
    assert_eq!(gain.min_millibel(), -12_800);
    assert_eq!(gain.max_millibel(), 800);
    assert_eq!(gain.step_millibel(), 54);
    assert_eq!(audio.endpoint_facts(1).err(), Some(DriverError::NotFound));
}

#[test]
fn configuring_selects_the_setting_and_sizes_slots_to_the_period() {
    let audio = qemu_configured();
    assert_eq!(audio.transport().settings, [(1, 1)]);
    // QEMU states no frequency control, so no request sets a rate.
    assert!(audio
        .transport()
        .requests
        .iter()
        .all(|(setup, _)| setup[0] != 0x22));
    let config = audio.interfaces[0].config.expect("configured");
    assert_eq!(config.grant.period_frames, 480);
    assert_eq!(config.grant.format, SampleFormat::S16);
    assert_eq!(config.grant.rate, Rate::HZ_48000);
    assert_eq!(config.interval, 8, "1 ms frames");
    // Ten 1 ms intervals a slot, and enough slots to stay ahead of the bus.
    assert_eq!((config.layout.slots, config.layout.packets), (3, 10));
    assert_eq!(config.layout.packet_bytes, 192);
}

#[test]
fn a_rate_or_format_the_device_lacks_is_answered_with_what_it_runs() {
    let mut audio = qemu();
    let grant = audio
        .configure(
            0,
            &params(0, 44_100, SampleFormat::S24, ChannelMap::STEREO, 256),
        )
        .expect("configures");
    assert_eq!(grant.rate, Rate::HZ_48000);
    assert_eq!(grant.format, SampleFormat::S16);
}

/// Fill QEMU's speaker ahead of its start with `frames` of the ramp, as the
/// mixer does, then start it at `at`.
fn qemu_primed(frames: u32, at: Frames) -> (UsbAudio<Mock>, Ring) {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    ring.ramp(0, frames);
    audio.service(0, &mut ring.bind()).expect("primes");
    audio.start(0, at).expect("starts");
    (audio, ring)
}

#[test]
fn playback_moves_every_frame_in_order_at_exactly_48_a_frame() {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    ring.ramp(0, 1_440);
    let report = audio.service(0, &mut ring.bind()).expect("primes");
    assert_eq!(report.transferred, 1_440, "three slots of ten intervals");
    assert!(!report.running);
    assert_eq!(report.position, Frames::ZERO);
    assert_eq!(audio.transport().started.len(), 1);
    assert!(
        audio.transport_mut().stream(0x01).queued.is_empty(),
        "held until the start"
    );
    audio.start(0, Frames::ZERO).expect("starts");
    assert_eq!(audio.transport().started.len(), 1, "the primed stream");
    assert_eq!(audio.transport_mut().stream(0x01).queued, [0, 1, 2]);
    assert!(audio.transport().stopped.is_empty());
    // Every interval is exactly QEMU's 192 bytes, and the ramp is unbroken.
    let samples: Vec<u16> = (0..3)
        .flat_map(|slot| slot_samples(&mut audio, 0x01, slot))
        .collect();
    assert_eq!(samples, (0..1_440).collect::<Vec<u16>>());

    ring.ramp(1_440, 480);
    let finished = audio.transport_mut().finish(0x01);
    assert_eq!(finished, 0);
    let causes = audio.take_interrupt().expect("read");
    assert!(AudioInterrupt::names(causes.period_elapsed, 0));
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.position, Frames::new(480));
    assert_eq!(report.xrun_frames, 0);
    assert_eq!(report.transferred, 480, "the slot refilled");
    assert_eq!(report.sampled_at, Time64::from_nanos(0));
    assert_eq!(
        slot_samples(&mut audio, 0x01, 0),
        (1_440..1_920).collect::<Vec<u16>>()
    );
}

#[test]
fn a_dry_ring_with_nothing_in_flight_is_padded_and_counted() {
    let (mut audio, mut ring) = qemu_primed(480, Frames::ZERO);
    ring.ramp(480, 100);
    audio.transport_mut().finish(0x01);
    audio.take_interrupt().expect("read");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.transferred, 100);
    assert_eq!(report.xrun_frames, 380, "the rest of the slot is silence");
    assert_eq!(
        audio.transport_mut().stream(0x01).queued,
        [1],
        "with a slot in flight the next waits for the mixer"
    );
}

#[test]
fn a_prime_holds_only_whole_slots_and_short_frames_wait_for_the_clock() {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    ring.ramp(0, 100);
    let report = audio.service(0, &mut ring.bind()).expect("primes");
    assert_eq!(
        report.transferred, 0,
        "more may come before the device asks"
    );
    assert_eq!(report.xrun_frames, 0);
    audio.start(0, Frames::ZERO).expect("starts");
    assert_eq!(audio.transport_mut().stream(0x01).queued, [0]);
    assert!(slot_samples(&mut audio, 0x01, 0)
        .iter()
        .all(|&sample| sample == 0));
    audio.transport_mut().finish(0x01);
    audio.take_interrupt().expect("read");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.transferred, 100);
    assert_eq!(report.xrun_frames, 480 + 380);
    assert_eq!(
        &slot_samples(&mut audio, 0x01, 1)[..100],
        (0..100).collect::<Vec<u16>>()
    );
}

#[test]
fn a_start_with_nothing_held_clocks_a_slot_of_silence_counted_lost() {
    let mut audio = qemu_configured();
    audio.start(0, Frames::ZERO).expect("starts");
    assert_eq!(audio.transport().started.len(), 1);
    assert_eq!(audio.transport_mut().stream(0x01).queued, [0]);
    let samples = slot_samples(&mut audio, 0x01, 0);
    assert_eq!(
        samples.len(),
        480,
        "a whole slot, so the device never waits"
    );
    assert!(samples.iter().all(|&sample| sample == 0));
    let mut ring = Ring::stereo();
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert!(report.running);
    assert_eq!(report.xrun_frames, 480);
}

#[test]
fn a_service_with_nothing_to_hold_sets_nothing_up() {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.transferred, 0);
    assert!(!report.running);
    assert!(audio.transport().started.is_empty());
}

#[test]
fn reconfiguring_after_a_prime_stops_the_stream_it_set_up() {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    ring.ramp(0, 480);
    audio.service(0, &mut ring.bind()).expect("primes");
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 240),
        )
        .expect("configures");
    assert_eq!(audio.transport().stopped, [0x01]);
    audio.start(0, Frames::ZERO).expect("starts");
    assert_eq!(
        audio.transport().started.len(),
        2,
        "a stream of the new size"
    );
}

#[test]
fn losses_count_from_the_configuration_across_a_restart() {
    let mut audio = qemu_configured();
    audio.start(0, Frames::ZERO).expect("starts");
    audio.stop(0, Frames::new(480)).expect("stops");
    let mut ring = Ring::stereo();
    ring.ramp(0, 480);
    let report = audio.service(0, &mut ring.bind()).expect("primes");
    assert_eq!(report.position, Frames::new(480), "held where it stopped");
    audio.start(0, Frames::new(480)).expect("starts");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(
        report.xrun_frames, 480,
        "the first run's silence still counts"
    );

    audio.stop(0, Frames::new(960)).expect("stops");
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    ring.ramp(960, 480);
    let report = audio.service(0, &mut ring.bind()).expect("primes");
    assert_eq!(report.xrun_frames, 0, "a configuration starts the tally");
}

#[test]
fn a_stream_whose_slots_cannot_all_be_set_out_is_stopped_again() {
    let mut audio = qemu_configured();
    let mut ring = Ring::stereo();
    ring.ramp(0, 480);
    audio.service(0, &mut ring.bind()).expect("primes");
    audio.transport_mut().queue_refusal = Some(Errno::WouldBlock);
    assert_eq!(audio.start(0, Frames::ZERO).err(), Some(DriverError::Busy));
    assert_eq!(audio.transport().stopped, [0x01]);
    assert!(audio.transport().streams.is_empty());

    let mut audio = headset();
    audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::MONO, 48),
        )
        .expect("configures");
    audio.transport_mut().queue_refusal = Some(Errno::WouldBlock);
    assert_eq!(audio.start(1, Frames::ZERO).err(), Some(DriverError::Busy));
    assert_eq!(
        audio.transport().stopped,
        [0x82],
        "not left holding a region"
    );
    assert!(audio.transport().streams.is_empty());
}

#[test]
fn intervals_the_bus_skipped_advance_the_position_and_count_as_lost() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::new(1_000));
    audio.transport_mut().finish_from(HCD, 0x01, 3, 48, 0);
    audio.take_interrupt().expect("read");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.position, Frames::new(1_000 + 480 + 3 * 48));
    assert_eq!(report.xrun_frames, 3 * 48);
}

#[test]
fn a_notice_from_anyone_but_the_streams_grantor_is_not_believed() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::ZERO);
    audio.transport_mut().finish_from(STRANGER, 0x01, 0, 48, 0);
    let causes = audio.take_interrupt().expect("read");
    assert!(causes.is_empty());
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.position, Frames::ZERO, "nothing finished");
    // A frame that is no notice at all is dropped too.
    audio
        .transport_mut()
        .notices
        .push_back((HCD, [0u8; ISO_NOTIFY_LEN]));
    assert!(audio.take_interrupt().expect("read").is_empty());
}

#[test]
fn a_drain_plays_out_what_is_queued_then_reports_stopped() {
    let (mut audio, mut ring) = qemu_primed(600, Frames::ZERO);
    assert_eq!(
        audio.transport_mut().stream(0x01).queued,
        [0],
        "only whole slots are held while more may come"
    );
    audio.drain(0).expect("drains");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(report.transferred, 120, "the short slot");
    assert!(report.running);
    assert_eq!(
        slot_samples(&mut audio, 0x01, 1).len(),
        120,
        "nothing past the end"
    );
    for _ in 0..2 {
        audio.transport_mut().finish(0x01);
    }
    audio.take_interrupt().expect("read");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert!(!report.running);
    assert_eq!(report.position, Frames::new(600));
    assert_eq!(audio.transport().stopped, [0x01], "the stream went with it");
}

#[test]
fn a_stream_whose_device_went_faults_and_one_reset_under_it_restarts() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::ZERO);
    audio.transport_mut().halt(0x01, Errno::WouldBlock);
    let causes = audio.take_interrupt().expect("read");
    assert!(AudioInterrupt::names(causes.period_elapsed, 0));
    ring.ramp(1_440, 480);
    let report = audio.service(0, &mut ring.bind()).expect("restarted");
    assert_eq!(audio.transport().started.len(), 2);
    assert_eq!(
        audio.transport().settings,
        [(1, 1), (1, 1), (1, 1)],
        "configured, established by the prime, and selected again"
    );
    assert_eq!(
        report.xrun_frames, 1_440,
        "what the halted stream held is lost"
    );
    assert_eq!(report.transferred, 480);

    audio.transport_mut().halt(0x01, Errno::NotFound);
    audio.take_interrupt().expect("read");
    assert_eq!(
        audio.service(0, &mut ring.bind()).err(),
        Some(DriverError::DeviceFault)
    );
}

#[test]
fn gain_is_set_on_every_volume_channel_to_the_step_above_and_mute_on_the_master() {
    let mut audio = qemu();
    audio.set_gain(0, 0, false).expect("set");
    let mock = audio.transport();
    for channel in [1, 2] {
        let value = mock.value(2, selector::VOLUME, channel).expect("set");
        let volume = i16::from_le_bytes([value[0], value[1]]);
        assert!((0..0x88).contains(&volume), "{volume}");
    }
    assert_eq!(mock.value(2, selector::MUTE, 0), Some(&[0u8][..]));
    audio.set_gain(0, -600, true).expect("set");
    assert_eq!(
        audio.transport().value(2, selector::MUTE, 0),
        Some(&[1u8][..])
    );
}

#[test]
fn releasing_an_endpoint_frees_the_bus_and_forgets_its_configuration() {
    let mut audio = qemu_configured();
    audio.release(0).expect("released");
    assert_eq!(audio.transport().settings, [(1, 1), (1, 0)]);
    assert_eq!(
        audio.start(0, Frames::ZERO).err(),
        Some(DriverError::DeviceFault)
    );
}

fn headset() -> UsbAudio<Mock> {
    UsbAudio::open(&headset_v2(), 0, UsbSpeed::High, Mock::new(UsbSpeed::High))
        .expect("the headset opens")
}

#[test]
fn a_version_two_headset_reads_its_rates_from_its_clock() {
    let audio = headset();
    assert_eq!(audio.transport().claims, [1, 2]);
    let playback = audio.endpoint_facts(0).expect("facts");
    assert_eq!(playback.name.as_str(), "Headphones");
    let rates: Vec<u32> = match playback.rates {
        RateSupport::Discrete(set) => set.rates().iter().map(|rate| rate.hz()).collect(),
        RateSupport::Continuous { .. } => Vec::new(),
    };
    assert_eq!(
        rates,
        [44_100, 48_000, 96_000],
        "the clock's ranges, read as standard rates"
    );
    let capture = audio.endpoint_facts(1).expect("facts");
    assert_eq!(capture.direction, StreamDirection::Capture);
    assert_eq!(capture.name.as_str(), "Microphone");
    assert_eq!(capture.gain, None, "its feature unit offers mute alone");
}

#[test]
fn configuring_a_version_two_rate_steers_the_selector_and_sets_the_clock() {
    let mut audio = headset();
    audio
        .configure(
            0,
            &params(0, 96_000, SampleFormat::S24, ChannelMap::STEREO, 96),
        )
        .expect("configures");
    let mock = audio.transport();
    assert_eq!(
        mock.value(0x28, selector::CLOCK_SELECTOR, 0),
        Some(&[1u8][..])
    );
    assert_eq!(mock.clock_frequency.last(), Some(&(0x29, 96_000)));
    let config = audio.interfaces[0].config.expect("configured");
    // 96 frames at 96 kHz over 125 µs intervals of 12 frames: 8 intervals.
    assert_eq!(config.layout.packets, 8);
    assert_eq!(config.interval, 1);
    assert_eq!(config.grant.period_frames, 96);
}

#[test]
fn explicit_feedback_paces_the_playback_endpoint() {
    let mut audio = headset();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    let started: Vec<u8> = audio
        .transport()
        .started
        .iter()
        .map(|p| p.endpoint)
        .collect();
    assert_eq!(started, [0x01, 0x81], "data, then its feedback");
    // The device asks for 6.25 frames a microframe: 16.16 at high speed.
    let report = (6u32 << 16) | (1 << 14);
    {
        let mock = audio.transport_mut();
        let stream = mock.stream(0x81);
        let number = stream.number;
        let slot = stream.queued.pop_front().expect("queued");
        for packet in 0..stream.layout.packets {
            stream
                .layout
                .data_mut(&mut stream.region, slot, packet)
                .expect("in place")[..4]
                .copy_from_slice(&report.to_le_bytes());
            stream
                .layout
                .set_record(
                    &mut stream.region,
                    slot,
                    packet,
                    IsoPacket {
                        length: 4,
                        status: IsoPacketStatus::Moved,
                    },
                )
                .expect("in place");
        }
        mock.notices.push_back((
            HCD,
            IsoNotify::SlotDone {
                endpoint: 0x81,
                stream: number,
                slot,
                skipped: 0,
                microframe: 0,
                completed_at: 0,
            }
            .encode(),
        ));
    }
    audio.take_interrupt().expect("read");
    assert_eq!(
        audio.transport_mut().stream(0x81).queued.len(),
        usize::from(FEEDBACK_SLOTS),
        "the feedback slot is queued again"
    );
    let mut ring = Ring::new(4096, SampleFormat::S24, 2);
    let silence = vec![0u8; 4000 * 6];
    assert_eq!(ring.bind().write(&silence), Ok(4000));
    audio.service(0, &mut ring.bind()).expect("services");
    let frames = |audio: &mut UsbAudio<Mock>, slot: u16| -> Vec<u32> {
        let stream = audio.transport_mut().stream(0x01);
        (0..stream.layout.packets)
            .map(|packet| {
                stream
                    .layout
                    .record(&stream.region, slot, packet)
                    .expect("in place")
                    .length
                    / 6
            })
            .collect()
    };
    assert_eq!(
        frames(&mut audio, 0),
        [6; 8],
        "the start's slot, before any report"
    );
    // At 6.25 a microframe, every fourth interval carries a seventh frame.
    assert_eq!(frames(&mut audio, 1), [6, 6, 6, 7, 6, 6, 6, 7]);
}

#[test]
fn capture_delivers_what_the_device_sent_and_counts_what_the_ring_could_not_hold() {
    let mut audio = headset();
    audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::MONO, 48),
        )
        .expect("configures");
    audio.start(1, Frames::ZERO).expect("starts");
    let queued = audio.transport_mut().stream(0x82).queued.len();
    assert!(queued >= 2, "every slot set out to receive");
    // `finish_from` counts four-byte frames, so three of them are the 12
    // bytes of six mono frames, inside the endpoint's 14.
    audio.transport_mut().finish_from(HCD, 0x82, 0, 3, 0x11);
    let causes = audio.take_interrupt().expect("read");
    assert!(AudioInterrupt::names(causes.period_elapsed, 1));
    let mut ring = Ring::new(64, SampleFormat::S16, 1);
    let report = audio.service(1, &mut ring.bind()).expect("services");
    let packets = audio.interfaces[1]
        .config
        .expect("configured")
        .layout
        .packets;
    let received = 6 * u32::from(packets);
    assert_eq!(report.transferred, received.min(64));
    assert_eq!(report.position, Frames::new(u64::from(received)));
    assert_eq!(report.xrun_frames, u64::from(received.saturating_sub(64)));
    assert_eq!(
        audio.transport_mut().stream(0x82).queued.len(),
        queued,
        "the slot is set out again"
    );
}

fn implicit() -> UsbAudio<Mock> {
    UsbAudio::open(&implicit_v2(), 0, UsbSpeed::Full, Mock::new(UsbSpeed::Full))
        .expect("the interface opens")
}

#[test]
fn implicit_feedback_runs_the_capture_stream_to_pace_playback() {
    let mut audio = implicit();
    assert_eq!(audio.interfaces[0].implicit, Some(1));
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    let started: Vec<u8> = audio
        .transport()
        .started
        .iter()
        .map(|p| p.endpoint)
        .collect();
    assert_eq!(
        started,
        [0x01, 0x82],
        "playback, then the capture pacing it"
    );
    assert!(audio.transport().settings.contains(&(2, 1)));
    // The device's clock runs a little fast: 49 frames a 1 ms frame.
    let queued = audio.transport_mut().stream(0x82).queued.len();
    audio.transport_mut().finish_from(HCD, 0x82, 0, 49, 0);
    let causes = audio.take_interrupt().expect("read");
    assert!(causes.is_empty(), "nobody listens to the capture endpoint");
    assert_eq!(
        audio.transport_mut().stream(0x82).queued.len(),
        queued,
        "its slot is set out again at once"
    );
    let mut ring = Ring::stereo();
    ring.ramp(0, 2_000);
    audio.service(0, &mut ring.bind()).expect("services");
    let frames = |audio: &mut UsbAudio<Mock>, slot: u16| -> Vec<u32> {
        let stream = audio.transport_mut().stream(0x01);
        (0..stream.layout.packets)
            .map(|packet| {
                stream
                    .layout
                    .record(&stream.region, slot, packet)
                    .expect("in place")
                    .length
                    / 4
            })
            .collect()
    };
    assert_eq!(
        frames(&mut audio, 0),
        [48; 10],
        "the start's slot, before the device was heard"
    );
    let paced = frames(&mut audio, 1);
    assert!(paced.iter().all(|&frames| frames == 49), "{paced:?}");

    audio.stop(0, Frames::new(490)).expect("stops");
    assert!(
        audio.transport().stopped.contains(&0x82),
        "nothing else held it"
    );
    assert!(audio.transport().settings.contains(&(2, 0)));
}

#[test]
fn a_shared_clock_is_not_retuned_under_another_endpoint() {
    let mut audio = implicit();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    assert_eq!(
        audio
            .configure(
                1,
                &params(1, 44_100, SampleFormat::S16, ChannelMap::STEREO, 441)
            )
            .err(),
        Some(DriverError::Busy)
    );
    assert!(audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480)
        )
        .is_ok());
}

#[test]
fn a_setting_selected_for_a_rate_the_clock_then_refused_is_still_freed() {
    let mut audio = headset();
    audio.transport_mut().clock_invalid = true;
    assert_eq!(
        audio
            .configure(
                0,
                &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48)
            )
            .err(),
        Some(DriverError::DeviceFault)
    );
    assert_eq!(
        audio.transport().settings,
        [(1, 1), (1, 0)],
        "the bandwidth is given back at once"
    );
    audio.release(0).expect("released");
    assert_eq!(
        audio.transport().settings,
        [(1, 1), (1, 0)],
        "and nothing is left to release"
    );
}

#[test]
fn a_restart_after_a_halt_starts_the_feedback_stream_with_the_data() {
    let mut audio = headset();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    audio.transport_mut().halt(0x01, Errno::WouldBlock);
    audio.take_interrupt().expect("read");
    let mut ring = Ring::new(4096, SampleFormat::S24, 2);
    audio.service(0, &mut ring.bind()).expect("restarted");
    let started: Vec<u8> = audio
        .transport()
        .started
        .iter()
        .map(|p| p.endpoint)
        .collect();
    assert_eq!(started, [0x01, 0x81, 0x01, 0x81]);
    assert_eq!(
        audio.transport_mut().stream(0x81).queued.len(),
        usize::from(FEEDBACK_SLOTS),
        "the fresh feedback stream is set out to receive"
    );
}

#[test]
fn a_halted_implicit_feedback_source_is_started_again() {
    let mut audio = implicit();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    audio.transport_mut().halt(0x82, Errno::WouldBlock);
    let causes = audio.take_interrupt().expect("read");
    assert!(
        AudioInterrupt::names(causes.period_elapsed, 0),
        "the playback it paced is told"
    );
    let mut ring = Ring::stereo();
    audio.service(0, &mut ring.bind()).expect("services");
    let started: Vec<u8> = audio
        .transport()
        .started
        .iter()
        .map(|p| p.endpoint)
        .collect();
    assert_eq!(started, [0x01, 0x82, 0x82]);
    audio.transport_mut().halt(0x82, Errno::NotFound);
    audio.take_interrupt().expect("read");
    assert_eq!(
        audio.service(0, &mut ring.bind()).err(),
        Some(DriverError::DeviceFault),
        "a source whose device went takes the playback with it"
    );
}

/// The entity the clock graphs below start from.
const TOP_CLOCK: u8 = 0x40;

/// A high-speed 2.0 stereo `s16` speaker whose terminal runs from clock
/// entity [`TOP_CLOCK`], `clocks` standing between it and oscillator 0x29.
fn speaker_clocked_by(clocks: &[&[u8]]) -> Vec<u8> {
    use crate::fixtures::{association, configuration, endpoint, interface};
    let top = TOP_CLOCK;
    let control = interface(0, 0, 0, 0x01_01_20);
    let header: [u8; 9] = [9, 0x24, 0x01, 0x00, 0x02, 0x01, 0, 0, 0];
    let source: [u8; 8] = [8, 0x24, 0x0A, 0x29, 0x03, 0x07, 0, 0];
    let terminal: [u8; 17] = [
        17, 0x24, 0x02, 0x01, 0x01, 0x01, 0, top, 2, 3, 0, 0, 0, 0, 0, 0, 0,
    ];
    let speaker: [u8; 12] = [12, 0x24, 0x03, 0x03, 0x01, 0x03, 0, 0x01, top, 0, 0, 0];
    let zero = interface(1, 0, 0, 0x01_02_20);
    let one = interface(1, 1, 1, 0x01_02_20);
    let general: [u8; 16] = [16, 0x24, 0x01, 0x01, 0, 0x01, 1, 0, 0, 0, 2, 3, 0, 0, 0, 0];
    let format: [u8; 6] = [6, 0x24, 0x02, 0x01, 0x02, 0x10];
    let data = endpoint(0x01, 0x0D, 24, 1);
    let iad = association(2);
    let mut body: Vec<&[u8]> = vec![&iad, &control, &header, &source];
    body.extend_from_slice(clocks);
    body.extend_from_slice(&[&terminal, &speaker, &zero, &one, &general, &format, &data]);
    configuration(&body)
}

/// The id of the clock entity after `at` on a chain from [`TOP_CLOCK`] of
/// `length` entities, the last running from `bottom`.
fn next_on_chain(at: u8, length: usize, bottom: u8) -> u8 {
    if usize::from(at - TOP_CLOCK) + 1 == length {
        bottom
    } else {
        at + 1
    }
}

/// A 2.0 speaker whose terminal's clock is the top of a 24-deep lattice of
/// two-pin selectors, both pins of each naming the next and the last's naming
/// `bottom`: there are 2^24 paths through it.
fn selector_lattice(bottom: u8) -> Vec<u8> {
    let depth: u8 = 24;
    let selectors: Vec<[u8; 9]> = (TOP_CLOCK..TOP_CLOCK + depth)
        .map(|id| {
            let next = next_on_chain(id, usize::from(depth), bottom);
            [9, 0x24, 0x0B, id, 2, next, next, 0x03, 0]
        })
        .collect();
    let clocks: Vec<&[u8]> = selectors.iter().map(<[u8; 9]>::as_slice).collect();
    speaker_clocked_by(&clocks)
}

/// A 2.0 speaker whose terminal's clock is the first of a chain of clock
/// multipliers, each running from the next and the last from oscillator 0x29,
/// each reading its `(numerator, denominator)` from `chain`.
fn multiplied(chain: &[(u16, u16)]) -> Result<UsbAudio<Mock>, DriverError> {
    let mut mock = Mock::new(UsbSpeed::High);
    let mut multipliers: Vec<[u8; 7]> = Vec::new();
    for (at, &(numerator, denominator)) in (TOP_CLOCK..).zip(chain) {
        let next = next_on_chain(at, chain.len(), 0x29);
        // Numerator and denominator both readable.
        multipliers.push([7, 0x24, 0x0C, at, next, 0x05, 0]);
        mock.set.push((
            (at, selector::NUMERATOR, 0),
            numerator.to_le_bytes().to_vec(),
        ));
        mock.set.push((
            (at, selector::DENOMINATOR, 0),
            denominator.to_le_bytes().to_vec(),
        ));
    }
    let clocks: Vec<&[u8]> = multipliers.iter().map(<[u8; 7]>::as_slice).collect();
    UsbAudio::open(&speaker_clocked_by(&clocks), 0, UsbSpeed::High, mock)
}

#[test]
fn a_clock_multiplier_scales_its_sources_rates_and_is_set_through() {
    // The terminal runs at half the oscillator, which offers 44.1, 48 and
    // 96 kHz.
    let mut audio = multiplied(&[(1, 2)]).expect("opens");
    let rates: Vec<u32> = match audio.endpoint_facts(0).expect("facts").rates {
        RateSupport::Discrete(set) => set.rates().iter().map(|rate| rate.hz()).collect(),
        RateSupport::Continuous { .. } => Vec::new(),
    };
    assert_eq!(rates, [22_050, 48_000]);
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    assert_eq!(
        audio.transport().clock_frequency.last(),
        Some(&(0x29, 96_000))
    );
}

#[test]
fn a_multiplier_whose_ratio_cannot_be_carried_offers_no_route() {
    let zero: &[(u16, u16)] = &[(0, 1)];
    // The true ratio is two thirds, past 32 bits on the way there.
    let overflowing: &[(u16, u16)] = &[(65_535, 65_535), (65_535, 65_535), (2, 3)];
    for chain in [zero, overflowing] {
        assert_eq!(
            multiplied(chain).err(),
            Some(DriverError::NotFound),
            "{chain:?}: no rate, so no endpoint"
        );
    }
}

#[test]
fn a_lattice_of_clock_selectors_is_walked_within_a_bound() {
    let audio = UsbAudio::open(
        &selector_lattice(0x29),
        0,
        UsbSpeed::High,
        Mock::new(UsbSpeed::High),
    )
    .expect("opens");
    assert!(audio
        .endpoint_facts(0)
        .expect("facts")
        .rates
        .admits(Rate::HZ_48000));
    let routes = audio.interfaces[0].routes.len();
    assert!(
        (1..=crate::controls::MAX_CLOCK_ROUTES).contains(&routes),
        "{routes}"
    );
    assert!(
        audio.transport().requests.len() < 200,
        "{} control requests",
        audio.transport().requests.len()
    );
}

#[test]
fn a_lattice_reaching_no_clock_source_is_given_up_within_a_bound() {
    // No route is ever found, so only the step bound ends the walk.
    let opened = UsbAudio::open(
        &selector_lattice(0x3F),
        0,
        UsbSpeed::High,
        Mock::new(UsbSpeed::High),
    );
    assert_eq!(
        opened.err(),
        Some(DriverError::NotFound),
        "no rate, so no endpoint"
    );
}

fn shared() -> UsbAudio<Mock> {
    UsbAudio::open(
        &shared_clock_v2(),
        0,
        UsbSpeed::High,
        Mock::new(UsbSpeed::High),
    )
    .expect("the shared-clock function opens")
}

#[test]
fn a_controller_reset_under_a_running_stream_is_recovered_from_scratch() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::ZERO);
    audio.set_gain(0, -600, false).expect("set");
    audio.transport_mut().reset();
    let causes = audio.take_interrupt().expect("read");
    assert!(AudioInterrupt::names(causes.period_elapsed, 0));
    ring.ramp(1_440, 480);
    audio.service(0, &mut ring.bind()).expect("recovered");
    let mock = audio.transport();
    assert_eq!(mock.claimed, [1], "claimed again");
    assert_eq!(mock.live, [(1, 1)], "its setting selected again");
    assert_eq!(mock.started.len(), 2);
    assert!(
        mock.value(2, selector::VOLUME, 1).is_some(),
        "the gain the device forgot set again"
    );
}

#[test]
fn an_endpoint_idle_through_a_reset_is_established_before_it_starts() {
    let mut audio = qemu_configured();
    audio.transport_mut().reset();
    let mut ring = Ring::stereo();
    ring.ramp(0, 480);
    audio.service(0, &mut ring.bind()).expect("primes");
    audio.start(0, Frames::ZERO).expect("starts");
    assert_eq!(audio.transport_mut().stream(0x01).queued, [0]);
}

#[test]
fn a_restart_that_fails_faults_the_endpoint_until_the_mixer_stops_it() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::ZERO);
    audio.transport_mut().reset();
    audio.transport_mut().start_refusals.push(0x01);
    audio.take_interrupt().expect("read");
    assert_eq!(
        audio.service(0, &mut ring.bind()).err(),
        Some(DriverError::NoBandwidth)
    );
    assert_eq!(
        audio.service(0, &mut ring.bind()).err(),
        Some(DriverError::NoBandwidth),
        "and to every service after, never a frozen running position"
    );
    audio.stop(0, Frames::ZERO).expect("stops");
    assert!(
        audio.service(0, &mut ring.bind()).is_ok(),
        "a stop clears it"
    );
}

#[test]
fn a_notice_a_stopped_stream_left_behind_is_not_read_as_its_successors() {
    let (mut audio, mut ring) = qemu_primed(1_440, Frames::ZERO);
    // The controller finishes a slot just as the mixer stops the stream.
    audio.transport_mut().finish(0x01);
    audio.stop(0, Frames::ZERO).expect("stops");
    ring.ramp(1_440, 1_440);
    audio.service(0, &mut ring.bind()).expect("primes");
    audio.start(0, Frames::ZERO).expect("starts");
    // Only now is the old stream's notice read.
    audio.take_interrupt().expect("read");
    let report = audio.service(0, &mut ring.bind()).expect("services");
    assert_eq!(
        report.position,
        Frames::ZERO,
        "nothing of the new stream finished"
    );
    assert_eq!(
        audio.transport_mut().stream(0x01).queued,
        [0, 1, 2],
        "its slots are still the controller's"
    );
}

#[test]
fn a_feedback_stream_the_controller_ended_is_started_again() {
    let mut audio = headset();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    audio.transport_mut().halt(0x81, Errno::WouldBlock);
    let causes = audio.take_interrupt().expect("read");
    assert!(AudioInterrupt::names(causes.period_elapsed, 0));
    let mut ring = Ring::new(4096, SampleFormat::S24, 2);
    audio.service(0, &mut ring.bind()).expect("services");
    let started: Vec<u8> = audio
        .transport()
        .started
        .iter()
        .map(|p| p.endpoint)
        .collect();
    assert_eq!(started, [0x01, 0x81, 0x81], "the feedback stream alone");
    assert_eq!(
        audio.transport_mut().stream(0x81).queued.len(),
        usize::from(FEEDBACK_SLOTS)
    );
    audio.transport_mut().halt(0x81, Errno::NotFound);
    audio.take_interrupt().expect("read");
    assert_eq!(
        audio.service(0, &mut ring.bind()).err(),
        Some(DriverError::DeviceFault),
        "one whose device went faults the endpoint"
    );
}

#[test]
fn a_capture_slot_that_cannot_be_delivered_still_goes_back_to_the_controller() {
    let mut audio = headset();
    audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::MONO, 48),
        )
        .expect("configures");
    audio.start(1, Frames::ZERO).expect("starts");
    let queued = audio.transport_mut().stream(0x82).queued.len();
    let slot = audio.transport_mut().finish_from(HCD, 0x82, 0, 3, 0x11);
    {
        // A record whose status names nothing.
        let stream = audio.transport_mut().stream(0x82);
        let at = stream.layout.record_offset(slot, 0) + 4;
        stream.region[at] = 9;
    }
    audio.take_interrupt().expect("read");
    let mut ring = Ring::new(64, SampleFormat::S16, 1);
    assert_eq!(
        audio.service(1, &mut ring.bind()).err(),
        Some(DriverError::DeviceFault)
    );
    assert_eq!(
        audio.transport_mut().stream(0x82).queued.len(),
        queued,
        "set out again all the same"
    );
}

#[test]
fn a_stopped_capture_still_pacing_a_playback_keeps_its_slots_queued() {
    let mut audio = implicit();
    audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.start(1, Frames::ZERO).expect("starts");
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("the capture paces it");
    let queued = audio.transport_mut().stream(0x82).queued.len();
    audio.transport_mut().finish_from(HCD, 0x82, 0, 48, 0);
    audio.take_interrupt().expect("read");
    audio.stop(1, Frames::ZERO).expect("stops");
    let mut ring = Ring::stereo();
    audio.service(1, &mut ring.bind()).expect("services");
    assert_eq!(
        audio.transport_mut().stream(0x82).queued.len(),
        queued,
        "the playback it paces still needs it"
    );
}

#[test]
fn a_capture_configured_while_pacing_a_playback_is_granted_its_running_slots() {
    let mut audio = implicit();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.start(0, Frames::ZERO).expect("starts");
    let grant = audio
        .configure(
            1,
            &params(1, 48_000, SampleFormat::S16, ChannelMap::STEREO, 960),
        )
        .expect("configures");
    assert_eq!(grant.period_frames, 480, "the slots the running stream has");
    let layout = audio.transport_mut().stream(0x82).layout;
    assert_eq!(audio.interfaces[1].config.map(|c| c.layout), Some(layout));
}

#[test]
fn a_shared_selector_is_not_steered_under_another_endpoint() {
    let mut audio = shared();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    let pin = |audio: &UsbAudio<Mock>| {
        audio
            .transport()
            .value(0x28, selector::CLOCK_SELECTOR, 0)
            .map(<[u8]>::to_vec)
    };
    let steered = pin(&audio);
    assert_eq!(
        audio
            .configure(
                1,
                &params(1, 44_100, SampleFormat::S16, ChannelMap::MONO, 44)
            )
            .err(),
        Some(DriverError::Busy)
    );
    assert_eq!(pin(&audio), steered, "the playback's pin stands");
    assert!(
        audio
            .configure(
                1,
                &params(1, 48_000, SampleFormat::S16, ChannelMap::MONO, 48)
            )
            .is_ok(),
        "the clock it already runs at is shared"
    );
}

#[test]
fn a_pacing_source_that_cannot_start_gives_its_setting_back() {
    let mut audio = implicit();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 480),
        )
        .expect("configures");
    audio.transport_mut().start_refusals.push(0x82);
    assert_eq!(
        audio.start(0, Frames::ZERO).err(),
        Some(DriverError::NoBandwidth)
    );
    let capture = audio
        .transport()
        .settings
        .iter()
        .rev()
        .find(|&&(interface, _)| interface == 2)
        .copied();
    assert_eq!(capture, Some((2, 0)), "its bandwidth given back");
}

#[test]
fn a_reconfiguration_that_fails_puts_the_previous_setting_back() {
    let mut audio = shared();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    // Refused on both of the selector's routes, then accepted again.
    let frequency = (requests::v2::CUR, selector::CLOCK_FREQUENCY);
    audio.transport_mut().stall_next = vec![frequency, frequency];
    assert!(audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 48)
        )
        .is_err());
    assert_eq!(
        audio.transport().live,
        [(1, 1)],
        "the 24-bit setting the grant describes"
    );
    audio
        .start(0, Frames::ZERO)
        .expect("the grant the channel still holds starts");

    let mut audio = shared();
    audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S24, ChannelMap::STEREO, 48),
        )
        .expect("configures");
    audio.transport_mut().clock_invalid = true;
    assert!(audio
        .configure(
            0,
            &params(0, 48_000, SampleFormat::S16, ChannelMap::STEREO, 48)
        )
        .is_err());
    assert_eq!(
        audio.start(0, Frames::ZERO).err(),
        Some(DriverError::DeviceFault),
        "with no way back, nothing streams in a setting its grant does not describe"
    );
}

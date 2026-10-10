//! The interface composed over the block's model, the modelled DMA channel,
//! and a clock controller and a codec that decode every frame they are sent,
//! all writing to one log so a test reads the order the parts were driven in.

use std::cell::Cell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{
    Audio, ChannelMap, Frames, GainRange, Rate, RateSet, RateSupport, SampleFormat,
};
use tairix_abi::driver::audio_channel::ConfigureParams;
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::driver::clock::{
    encode_describe_reply as encode_clock_state, encode_error_reply as encode_clock_error,
    encode_release_reply, encode_run_reply, ClockRequest, ClockState, CLOCK_CONTROLLER_ENDPOINTS,
};
use tairix_abi::driver::codec::{
    encode_describe_reply, encode_done_reply, encode_error_reply, encode_gain_reply,
    refusal_reason, ClockInversion, CodecFacts, CodecOp, CodecRequest, DaiFormat, DaiFormats,
    DaiLink, SampleWidths, CODEC_ENDPOINTS,
};
use tairix_abi::driver::dmaengine::WaitReport;
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::time::Time64;
use tairix_abi::{DriverError, Errno};
use tairix_audiochan::cyclic::mock::Channel;
use tairix_audiochan::cyclic::{DmaPort, PERIODS};
use tairix_linkclient::{ClockClient, CodecClient, LinkCall};

use super::{Interface, MIN_PERIOD_FRAMES};
use crate::pcm::model::{Block, Event, Log};
use crate::pcm::{Framing, Pcm, CS_TXCLR, CS_TXON, MODE};

const PERIOD: u32 = 256;

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a rate")
}

/// The clock controller: it runs a clock `skew_ppm` off the rate asked, or
/// refuses.
struct Clock {
    log: Log,
    skew_ppm: u64,
    refuse: Option<Errno>,
}

impl LinkCall for Clock {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        match ClockRequest::decode(request).expect("a canonical frame") {
            ClockRequest::Run { hz, .. } => {
                self.log.borrow_mut().push(Event::ClockRun(hz));
                match self.refuse {
                    Some(reason) => encode_clock_error(reply, reason),
                    None => encode_run_reply(reply, hz + hz * self.skew_ppm / 1_000_000),
                }
            }
            ClockRequest::Release(_) => {
                self.log.borrow_mut().push(Event::ClockRelease);
                encode_release_reply(reply)
            }
            ClockRequest::Describe(_) => encode_clock_state(
                reply,
                ClockState {
                    hz: 0,
                    held_elsewhere: false,
                },
            ),
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        panic!("no clock request is posted")
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        panic!("no clock request is posted")
    }
}

/// The codec: it states `facts`, records what it is configured for, and,
/// where it drives the bit clock, starts it as its output comes up.
struct Codec {
    log: Log,
    facts: CodecFacts,
    configured: Rc<Cell<Option<(u32, u8)>>>,
    bit_clock: Option<Rc<Cell<bool>>>,
}

impl LinkCall for Codec {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let request = CodecRequest::decode(request).expect("a canonical frame");
        self.log.borrow_mut().push(Event::Codec(request.op()));
        match request {
            CodecRequest::Describe(_) => encode_describe_reply(reply, &self.facts),
            CodecRequest::Configure { rate, width, .. } => {
                self.configured.set(Some((rate.hz(), width)));
                encode_done_reply(reply)
            }
            CodecRequest::Gain { millibel, .. } => match self.facts.gain {
                Some(_) => encode_gain_reply(reply, -(-millibel).div_euclid(50) * 50),
                None => encode_error_reply(reply, refusal_reason(DriverError::NotImplemented)),
            },
            CodecRequest::Start(_) => {
                if let Some(clock) = &self.bit_clock {
                    clock.set(true);
                }
                encode_done_reply(reply)
            }
            CodecRequest::Stop(_) => encode_done_reply(reply),
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        panic!("no codec request is posted")
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        panic!("no codec request is posted")
    }
}

/// The modelled channel, its starts and stops logged.
struct Dma {
    channel: Channel,
    log: Log,
}

impl DmaPort for Dma {
    fn prepare(&mut self, period_bytes: u32, periods: u32) -> Result<(), DriverError> {
        self.channel.prepare(period_bytes, periods)
    }
    fn buffer(&mut self) -> &mut [u8] {
        self.channel.buffer()
    }
    fn start(&mut self) -> Result<(), DriverError> {
        self.log.borrow_mut().push(Event::DmaStart);
        self.channel.start()
    }
    fn stop(&mut self) -> Result<(), DriverError> {
        self.log.borrow_mut().push(Event::DmaStop);
        self.channel.stop()
    }
    fn wait(&mut self, after: u64) -> Result<WaitReport, DriverError> {
        self.channel.wait(after)
    }
    fn post_wait(&mut self, after: u64, deadline_ns: u64) -> Result<(), DriverError> {
        self.channel.post_wait(after, deadline_ns)
    }
    fn reap_wait(&mut self) -> Result<Option<WaitReport>, DriverError> {
        self.channel.reap_wait()
    }
    fn is_waiting(&self) -> bool {
        self.channel.is_waiting()
    }
    fn now(&self) -> Time64 {
        self.channel.now()
    }
}

/// A PCM5122's statement: discrete rates, one past what the block carries,
/// and a gain in half-decibel steps.
fn stepped() -> CodecFacts {
    let rates = [44_100, 48_000, 96_000, 192_000, 384_000, 768_000].map(rate);
    CodecFacts {
        rates: RateSupport::Discrete(RateSet::new(&rates).expect("a set")),
        widths: SampleWidths::of(&[16, 24, 32]).expect("widths"),
        formats: DaiFormats::EMPTY
            .with(DaiFormat::I2s)
            .with(DaiFormat::LeftJustified),
        drives_clocks: false,
        gain: Some(GainRange::new(-10_300, 2_400, 50).expect("range")),
    }
}

/// A PCM5102A's: any rate it converts, and no gain.
fn continuous() -> CodecFacts {
    CodecFacts {
        rates: RateSupport::Continuous {
            min: rate(4_000),
            max: rate(768_000),
        },
        widths: SampleWidths::of(&[16, 24, 32]).expect("widths"),
        formats: DaiFormats::EMPTY.with(DaiFormat::I2s),
        drives_clocks: false,
        gain: None,
    }
}

/// A link on which this side drives both clocks, unless `codec_clocks`.
fn dai(format: DaiFormat, codec_clocks: bool) -> DaiLink {
    DaiLink {
        format,
        codec_drives_bit_clock: codec_clocks,
        codec_drives_frame_clock: codec_clocks,
        inversion: ClockInversion::Normal,
        cpu_dai: 0,
        codec_dai: 0,
    }
}

type Under<'a> = Interface<'a, Block, Dma, Clock, Codec>;

/// The parts a test builds an interface from.
struct Parts {
    log: Log,
    block: Block,
    configured: Rc<Cell<Option<(u32, u8)>>>,
}

impl Parts {
    fn new(clocked: bool) -> Self {
        let log = Log::default();
        Self {
            block: Block::logged(&log, clocked),
            log,
            configured: Rc::new(Cell::new(None)),
        }
    }

    fn build(
        &self,
        link: DaiLink,
        facts: CodecFacts,
        clock: Option<Clock>,
    ) -> Result<Under<'_>, DriverError> {
        let codec_link =
            LinkRequest::new(CODEC_ENDPOINTS.endpoint(40), 0, &link.to_cells(), b"").expect("ok");
        let clock_link =
            LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[31], b"").expect("ok");
        let codec = Codec {
            log: self.log.clone(),
            facts,
            configured: self.configured.clone(),
            bit_clock: link
                .codec_drives_bit_clock
                .then(|| self.block.clocked.clone()),
        };
        Interface::new(
            Pcm::new(&self.block).expect("a window"),
            Dma {
                channel: Channel::default(),
                log: self.log.clone(),
            },
            clock.map(|clock| ClockClient::new(clock, clock_link)),
            CodecClient::new(codec, codec_link).expect("a codec link"),
        )
    }

    fn clock(&self) -> Clock {
        Clock {
            log: self.log.clone(),
            skew_ppm: 0,
            refuse: None,
        }
    }

    fn standard(&self) -> Under<'_> {
        self.build(dai(DaiFormat::I2s, false), stepped(), Some(self.clock()))
            .expect("brought up")
    }

    /// What happened since the last call.
    fn events(&self) -> Vec<Event> {
        self.log.borrow_mut().drain(..).collect()
    }
}

fn params(hz: u32, period_frames: u32) -> ConfigureParams {
    ConfigureParams {
        endpoint: 0,
        rate: rate(hz),
        format: SampleFormat::S32,
        channel_map: ChannelMap::STEREO,
        period_frames,
    }
}

/// The mode register `framing` programs, read off a block of its own.
fn mode_of(framing: &Framing) -> Option<u32> {
    let block = Block::new(true, 0);
    Pcm::new(&block)
        .expect("a window")
        .frame(framing)
        .expect("framed");
    block.last(MODE)
}

/// Whether the latest control write left transmit on.
fn transmitting(events: &[Event]) -> Option<bool> {
    events.iter().rev().find_map(|event| match event {
        Event::Control(value) => Some(value & CS_TXON != 0),
        _ => None,
    })
}

/// Stage `frames` frames from a ring of S32 stereo.
fn stage(interface: &mut Under<'_>, frames: u32) {
    let geometry = PcmGeometry::new(PERIOD * 8, SampleFormat::S32, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    let mut samples = Vec::new();
    for frame in 0..frames {
        samples.extend_from_slice(&i32::try_from(frame).expect("small").to_le_bytes());
        samples.extend_from_slice(&(-1_i32).to_le_bytes());
    }
    ring.write(&samples).expect("written");
    interface.service(0, &mut ring).expect("staged");
}

#[test]
fn the_endpoint_offers_the_codecs_widest_word_the_rates_the_block_carries_and_the_codecs_gain() {
    let parts = Parts::new(true);
    let interface = parts.standard();
    let facts = interface.endpoint_facts(0).expect("facts");
    assert!(facts.validate().is_ok());
    assert!(facts.formats.contains(SampleFormat::S32));
    assert!(!facts.formats.contains(SampleFormat::S16), "one format");
    let RateSupport::Discrete(set) = facts.rates else {
        panic!("discrete rates");
    };
    assert_eq!(
        set.rates(),
        [44_100, 48_000, 96_000, 192_000, 384_000].map(rate),
        "768 kHz is past the block"
    );
    assert_eq!(facts.gain, stepped().gain);
    assert_eq!(facts.min_period_frames, MIN_PERIOD_FRAMES);
    assert!(matches!(
        interface.endpoint_facts(1),
        Err(DriverError::NotFound)
    ));

    for (widths, format) in [
        (&[16, 24][..], SampleFormat::S24In32),
        (&[16, 20][..], SampleFormat::S16),
    ] {
        let parts = Parts::new(true);
        let facts = CodecFacts {
            widths: SampleWidths::of(widths).expect("widths"),
            ..continuous()
        };
        let interface = parts
            .build(dai(DaiFormat::I2s, false), facts, Some(parts.clock()))
            .expect("brought up");
        let facts = interface.endpoint_facts(0).expect("facts");
        assert!(facts.formats.contains(format), "{format:?}");
        assert_eq!(
            facts.rates,
            RateSupport::Continuous {
                min: rate(8_000),
                max: rate(384_000),
            }
        );
    }
}

#[test]
fn bring_up_refuses_a_link_its_codec_or_its_clock_cannot_serve() {
    let parts = Parts::new(true);
    for (link, facts, clock) in [
        (dai(DaiFormat::DspA, false), stepped(), true),
        (dai(DaiFormat::I2s, true), stepped(), true),
        (
            dai(DaiFormat::I2s, false),
            CodecFacts {
                widths: SampleWidths::of(&[20]).expect("widths"),
                ..stepped()
            },
            true,
        ),
    ] {
        let clock = clock.then(|| parts.clock());
        assert!(
            matches!(
                parts.build(link, facts, clock),
                Err(DriverError::Unsupported)
            ),
            "{link:?}"
        );
    }
    assert!(matches!(
        parts.build(dai(DaiFormat::I2s, false), stepped(), None),
        Err(DriverError::NotFound)
    ));
}

#[test]
fn configuring_runs_the_bit_clock_for_two_slots_a_frame_and_the_codec_in_the_links_framing() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    parts.events();
    let grant = interface
        .configure(0, &params(44_000, 5))
        .expect("configured");
    assert_eq!(grant.rate, rate(44_100), "the codec's nearest");
    assert_eq!(grant.format, SampleFormat::S32);
    assert_eq!(grant.period_frames, MIN_PERIOD_FRAMES, "clamped");
    assert!(grant.validate().is_ok());
    let events = parts.events();
    assert!(events.contains(&Event::ClockRun(44_100 * 64)));
    assert_eq!(parts.configured.get(), Some((44_100, 32)));
    let framing = Framing::new(&dai(DaiFormat::I2s, false), 32).expect("a framing");
    assert_eq!(parts.block.last(MODE), mode_of(&framing));
    assert_eq!(transmitting(&events), Some(false));
    assert_eq!(
        interface.dma_mut().channel.period_bytes,
        MIN_PERIOD_FRAMES * 8
    );
}

#[test]
fn a_clock_made_too_far_off_or_held_elsewhere_is_refused() {
    for (skew_ppm, refuse, refused) in [
        (101, None, DriverError::Unsupported),
        (0, Some(Errno::Busy), DriverError::Busy),
    ] {
        let parts = Parts::new(true);
        let clock = Clock {
            skew_ppm,
            refuse,
            ..parts.clock()
        };
        let mut interface = parts
            .build(dai(DaiFormat::I2s, false), stepped(), Some(clock))
            .expect("brought up");
        assert_eq!(
            interface.configure(0, &params(48_000, PERIOD)),
            Err(refused)
        );
        assert_eq!(parts.configured.get(), None, "the codec left alone");
        assert!(matches!(
            interface.start(0, Frames::ZERO),
            Err(DriverError::DeviceFault)
        ));
    }
    let parts = Parts::new(true);
    let clock = Clock {
        skew_ppm: 99,
        ..parts.clock()
    };
    let mut interface = parts
        .build(dai(DaiFormat::I2s, false), stepped(), Some(clock))
        .expect("brought up");
    assert!(interface.configure(0, &params(48_000, PERIOD)).is_ok());
}

#[test]
fn a_stream_starts_from_a_cleared_fifo_then_transmits_then_unmutes() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    stage(&mut interface, PERIOD * PERIODS);
    interface.set_event_interrupts(true).expect("unmasked");
    parts.events();
    interface.start(0, Frames::new(100)).expect("started");
    let events = parts.events();
    let clear = events
        .iter()
        .position(|event| matches!(event, Event::Control(value) if value & CS_TXCLR != 0))
        .expect("the FIFO cleared");
    let Event::Control(cleared) = events[clear] else {
        unreachable!()
    };
    assert_eq!(cleared & CS_TXON, 0, "with transmit off");
    let started = events
        .iter()
        .position(|event| *event == Event::DmaStart)
        .expect("the channel started");
    let on = events
        .iter()
        .position(|event| matches!(event, Event::Control(value) if value & CS_TXON != 0))
        .expect("transmitting");
    let up = events
        .iter()
        .position(|event| *event == Event::Codec(CodecOp::Start))
        .expect("the codec up");
    assert!(clear < started && started < on && on < up, "{events:?}");
    assert_eq!(parts.block.cleared.get(), 1);
}

#[test]
fn a_codec_driving_the_bit_clock_comes_up_before_the_clear_that_needs_it() {
    let parts = Parts::new(false);
    let facts = CodecFacts {
        drives_clocks: true,
        ..continuous()
    };
    let mut interface = parts
        .build(dai(DaiFormat::I2s, true), facts, None)
        .expect("brought up");
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    let framing = Framing::new(&dai(DaiFormat::I2s, true), 32).expect("a framing");
    assert_eq!(parts.block.last(MODE), mode_of(&framing), "following both");
    interface.set_event_interrupts(true).expect("unmasked");
    parts.events();
    interface.start(0, Frames::ZERO).expect("started");
    let events = parts.events();
    assert_eq!(events[0], Event::Codec(CodecOp::Start), "{events:?}");
    assert!(!events
        .iter()
        .any(|event| matches!(event, Event::ClockRun(_))));
    assert_eq!(transmitting(&events), Some(true));
}

#[test]
fn a_start_whose_fifo_cannot_clear_leaves_nothing_running() {
    let parts = Parts::new(false);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    parts.events();
    assert!(matches!(
        interface.start(0, Frames::ZERO),
        Err(DriverError::DeviceFault)
    ));
    let events = parts.events();
    assert!(!events.contains(&Event::DmaStart), "{events:?}");
    assert!(!interface.dma_mut().channel.running);
    assert_eq!(transmitting(&events), Some(false));
}

#[test]
fn stopping_mutes_first_and_turns_transmit_off_once_silence_has_played() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    stage(&mut interface, PERIOD * PERIODS);
    interface.set_event_interrupts(true).expect("unmasked");
    interface.start(0, Frames::ZERO).expect("started");
    parts.events();
    interface.stop(0, Frames::new(9)).expect("stopped");
    let events = parts.events();
    assert_eq!(
        events.first(),
        Some(&Event::Codec(CodecOp::Stop)),
        "muted first"
    );
    assert!(transmitting(&events).is_none(), "still transmitting");
    assert!(interface.dma_mut().channel.running, "the park is playing");
    interface.dma_mut().channel.advance(1);
    interface.take_interrupt().expect("the park's boundary");
    let events = parts.events();
    assert_eq!(events[0], Event::DmaStop, "{events:?}");
    assert_eq!(transmitting(&events), Some(false));
    assert!(!events.contains(&Event::Codec(CodecOp::Stop)), "once only");
}

#[test]
fn a_drain_plays_out_and_the_output_goes_down_when_the_channel_halts() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    stage(&mut interface, PERIOD * 2);
    interface.set_event_interrupts(true).expect("unmasked");
    interface.start(0, Frames::ZERO).expect("started");
    interface.drain(0).expect("draining");
    parts.events();
    let mut halted = false;
    for _ in 0..8 {
        interface.dma_mut().channel.advance(1);
        interface.take_interrupt().expect("a boundary");
        let events = parts.events();
        if events.contains(&Event::DmaStop) {
            assert_eq!(transmitting(&events), Some(false));
            assert!(events.contains(&Event::Codec(CodecOp::Stop)));
            halted = true;
            break;
        }
        assert!(transmitting(&events).is_none(), "on until the halt");
    }
    assert!(halted, "the channel halted after the drain");
}

#[test]
fn the_gain_is_the_codecs_and_one_without_leaves_it_to_the_mixer() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    assert_eq!(interface.set_gain(0, -625, false), Ok(()));
    let parts = Parts::new(true);
    let mut interface = parts
        .build(
            dai(DaiFormat::I2s, false),
            continuous(),
            Some(parts.clock()),
        )
        .expect("brought up");
    assert_eq!(
        interface.set_gain(0, -625, false),
        Err(DriverError::NotImplemented)
    );
    assert!(interface.endpoint_facts(0).expect("facts").gain.is_none());
}

#[test]
fn a_reconfiguration_while_streaming_is_busy_and_touches_nothing() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    interface.set_event_interrupts(true).expect("unmasked");
    interface.start(0, Frames::ZERO).expect("started");
    parts.events();
    assert_eq!(
        interface.configure(0, &params(96_000, PERIOD)),
        Err(DriverError::Busy)
    );
    assert!(parts.events().is_empty());
}

#[test]
fn releasing_takes_the_output_down_halts_the_channel_and_gives_the_clock_up() {
    let parts = Parts::new(true);
    let mut interface = parts.standard();
    interface
        .configure(0, &params(48_000, PERIOD))
        .expect("configured");
    interface.set_event_interrupts(true).expect("unmasked");
    interface.start(0, Frames::ZERO).expect("started");
    parts.events();
    interface.release(0).expect("released");
    let events = parts.events();
    assert_eq!(events[0], Event::Codec(CodecOp::Stop), "{events:?}");
    assert!(events.contains(&Event::DmaStop));
    assert_eq!(transmitting(&events), Some(false));
    assert_eq!(events.last(), Some(&Event::ClockRelease));
    assert!(!interface.dma_mut().channel.running);
    assert!(matches!(
        interface.start(0, Frames::ZERO),
        Err(DriverError::DeviceFault)
    ));
}

//! Host tests for the serve loop's work: calls answered and device events
//! serviced, against the mock device and a mock channel that maps regions in
//! memory and records what it was sent.

extern crate std;

use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{AudioInterrupt, ChannelMap, Frames, Rate, SampleFormat};
use tairix_abi::driver::audio_channel::{
    decode_configure_reply, AttachParams, AudioChannelNotify, AudioChannelRequest, ConfigureParams,
    AUDIO_CHANNEL_MAX_REQUEST,
};
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, REGION_ALIGN_PADDING};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::{DriverError, Errno, ProcId};

use super::{ChannelIo, Dispatcher, MappedRegion};
use crate::mock_audio::{MockAudio, MOCK_PERIOD, MOCK_RATE_HZ, SINK, SOURCE};

const MIXER: ProcId = ProcId::from_raw([9; 16]);
const NOTIFY_PORT: u64 = 0xACE0;
const RING_FRAMES: u32 = 512;

fn ring_len() -> usize {
    PcmGeometry::new(RING_FRAMES, SampleFormat::S16, 2)
        .expect("a valid geometry")
        .region_len()
}

/// An in-memory mapping, cut aligned out of padded storage.
struct MockRegion {
    storage: Vec<u8>,
    len: usize,
}

impl MappedRegion for MockRegion {
    fn bytes(&mut self) -> &mut [u8] {
        aligned_region(&mut self.storage, self.len).expect("padded for alignment")
    }
}

/// The mixer's side of the channel.
struct MockIo {
    calls: VecDeque<(u64, Vec<u8>)>,
    replies: Vec<(u64, Vec<u8>)>,
    notified: Vec<(u64, AudioChannelNotify)>,
    next_ticket: u64,
    /// Bytes every mapping holds.
    region_len: usize,
    live: usize,
    short: Vec<(usize, usize, u32)>,
}

impl MockIo {
    fn new() -> Self {
        Self {
            calls: VecDeque::new(),
            replies: Vec::new(),
            notified: Vec::new(),
            next_ticket: 1,
            region_len: ring_len(),
            live: 0,
            short: Vec::new(),
        }
    }

    fn call(&mut self, request: &AudioChannelRequest) -> u64 {
        let mut frame = [0u8; AUDIO_CHANNEL_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("a request fits");
        self.call_bytes(&frame[..len])
    }

    fn call_bytes(&mut self, frame: &[u8]) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.calls.push_back((ticket, frame.to_vec()));
        ticket
    }

    fn reply(&self, ticket: u64) -> &[u8] {
        &self
            .replies
            .iter()
            .find(|(answered, _)| *answered == ticket)
            .expect("the call was answered")
            .1
    }
}

impl ChannelIo for MockIo {
    type Region = MockRegion;

    fn recv(&mut self, request: &mut [u8]) -> Option<(u64, usize)> {
        let (ticket, frame) = self.calls.pop_front()?;
        request[..frame.len()].copy_from_slice(&frame);
        Some((ticket, frame.len()))
    }

    fn reply(&mut self, ticket: u64, reply: &[u8]) {
        self.replies.push((ticket, reply.to_vec()));
    }

    fn caller(&mut self, _ticket: u64) -> Result<ProcId, Errno> {
        Ok(MIXER)
    }

    fn map(&mut self, _grant: u64, owner: ProcId) -> Result<MockRegion, Errno> {
        assert_eq!(owner, MIXER, "only the attested caller's grant is mapped");
        self.live += 1;
        Ok(MockRegion {
            storage: vec![0; self.region_len + REGION_ALIGN_PADDING],
            len: self.region_len,
        })
    }

    fn unmap(&mut self, _region: MockRegion) {
        self.live -= 1;
    }

    fn notify(&mut self, port: u64, frame: &[u8]) {
        let notify = AudioChannelNotify::decode(frame).expect("a whole notify frame");
        self.notified.push((port, notify));
    }

    fn short_region(&mut self, mapped: usize, expected: usize, ring_frames: u32) {
        self.short.push((mapped, expected, ring_frames));
    }
}

type TestDispatcher = Dispatcher<MockAudio, MockIo>;

fn dispatcher() -> TestDispatcher {
    Dispatcher::new(MockAudio::new(), MockIo::new())
}

fn configure(endpoint: u16) -> AudioChannelRequest {
    AudioChannelRequest::Configure(ConfigureParams {
        endpoint,
        rate: Rate::new(MOCK_RATE_HZ).expect("in range"),
        format: SampleFormat::S16,
        channel_map: ChannelMap::STEREO,
        period_frames: MOCK_PERIOD,
    })
}

fn attach(endpoint: u16, ring_frames: u32) -> AudioChannelRequest {
    AudioChannelRequest::Attach(AttachParams {
        endpoint,
        ring_frames,
        region_grant: 0x5AFE,
        notify_endpoint: NOTIFY_PORT + u64::from(endpoint),
    })
}

/// Configure and attach `endpoint`, and start it.
fn ready(dispatcher: &mut TestDispatcher, endpoint: u16) {
    let configured = dispatcher.io.call(&configure(endpoint));
    dispatcher.on_call();
    assert!(decode_configure_reply(dispatcher.io.reply(configured)).is_ok());
    let attached = dispatcher.io.call(&attach(endpoint, RING_FRAMES));
    dispatcher.on_call();
    assert_eq!(decode_status_reply(dispatcher.io.reply(attached)), Ok(()));
    let started = dispatcher.io.call(&AudioChannelRequest::Start {
        endpoint,
        at: Frames::ZERO,
    });
    dispatcher.on_call();
    assert_eq!(decode_status_reply(dispatcher.io.reply(started)), Ok(()));
}

fn periods_reported(dispatcher: &TestDispatcher, endpoint: u16) -> usize {
    dispatcher
        .io
        .notified
        .iter()
        .filter(|(port, notify)| {
            *port == NOTIFY_PORT + u64::from(endpoint)
                && matches!(notify, AudioChannelNotify::PeriodElapsed { endpoint: e, .. } if *e == endpoint)
        })
        .count()
}

#[test]
fn an_event_moves_a_period_and_reports_the_clock_pair() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().pending.period_elapsed = 1 << SINK;
    dispatcher.on_event();
    assert_eq!(periods_reported(&dispatcher, SINK), 1);
    assert_eq!(periods_reported(&dispatcher, SOURCE), 0);
}

/// A call that waited on the device can consume the wake an elapsed period
/// raised; with nothing else queued the stream would then stall for good, so
/// the device's causes are read when the call is answered.
#[test]
fn a_period_that_elapsed_while_a_call_was_answered_is_serviced_with_it() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().pending.period_elapsed = 1 << SINK;
    let gain = dispatcher.io.call(&AudioChannelRequest::Gain {
        endpoint: SINK,
        millibel: -600,
        mute: false,
    });
    dispatcher.on_call();
    assert_eq!(decode_status_reply(dispatcher.io.reply(gain)), Ok(()));
    assert_eq!(periods_reported(&dispatcher, SINK), 1);
    assert!(dispatcher.server.audio().pending.is_empty());
}

/// The old region was let go before the new one was offered, so a re-attach
/// the server refuses leaves nothing mapped and nothing serviced.
#[test]
fn a_refused_re_attach_unmaps_the_offered_region_and_services_nothing() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    assert_eq!(dispatcher.io.live, 1);
    // A ring the wire admits but the device's grant does not: shorter than
    // one of its periods.
    let refused = dispatcher.io.call(&attach(SINK, MOCK_PERIOD / 2));
    dispatcher.on_call();
    assert_eq!(
        decode_status_reply(dispatcher.io.reply(refused)),
        Err(Errno::OutOfRange)
    );
    assert_eq!(dispatcher.io.live, 0);
    assert!(!dispatcher.server.is_attached(SINK));
    dispatcher.server.audio_mut().pending.period_elapsed = 1 << SINK;
    dispatcher.on_event();
    assert_eq!(periods_reported(&dispatcher, SINK), 0);
    assert!(!dispatcher.server.audio().events_enabled);
}

#[test]
fn a_region_short_of_the_agreed_ring_is_refused_unmapped_and_recorded() {
    let mut dispatcher = dispatcher();
    dispatcher.io.region_len = ring_len() - 1;
    dispatcher.io.call(&configure(SINK));
    dispatcher.on_call();
    let attached = dispatcher.io.call(&attach(SINK, RING_FRAMES));
    dispatcher.on_call();
    assert_eq!(
        decode_status_reply(dispatcher.io.reply(attached)),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(dispatcher.io.live, 0);
    assert!(!dispatcher.server.is_attached(SINK));
    assert_eq!(
        dispatcher.io.short,
        vec![(ring_len() - 1, ring_len(), RING_FRAMES)]
    );
}

#[test]
fn a_detach_unmaps_the_region_and_holds_the_sources_down() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    assert!(dispatcher.server.audio().events_enabled);
    let detached = dispatcher
        .io
        .call(&AudioChannelRequest::Detach { endpoint: SINK });
    dispatcher.on_call();
    assert_eq!(decode_status_reply(dispatcher.io.reply(detached)), Ok(()));
    assert_eq!(dispatcher.io.live, 0);
    assert!(!dispatcher.server.audio().events_enabled);
}

#[test]
fn a_call_that_does_not_decode_is_answered_with_its_refusal() {
    let mut dispatcher = dispatcher();
    let garbled = dispatcher.io.call_bytes(&[0xFF; 8]);
    dispatcher.on_call();
    assert!(decode_status_reply(dispatcher.io.reply(garbled)).is_err());
}

#[test]
fn a_device_whose_causes_cannot_be_read_holds_its_sources_down_and_says_so() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().fault = Some(DriverError::DeviceFault);
    dispatcher.on_event();
    assert!(dispatcher.io.notified.iter().any(|(port, notify)| {
        *port == NOTIFY_PORT
            && *notify
                == AudioChannelNotify::Faulted {
                    endpoint: SINK,
                    reason: Errno::DeviceFault,
                }
    }));
}

#[test]
fn a_device_left_clocking_starts_with_its_sources_held_down() {
    let mut audio = MockAudio::new();
    audio.events_enabled = true;
    let dispatcher = Dispatcher::new(audio, MockIo::new());
    assert!(!dispatcher.server.audio().events_enabled);
    assert!(
        dispatcher.server.audio().pending == AudioInterrupt::NONE,
        "nothing is reported before a region is attached"
    );
}

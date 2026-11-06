//! Host tests for the serve loop's work: calls answered and device events
//! serviced, against the mock device and a mock channel that maps regions in
//! memory and records what it was sent.

extern crate std;

use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{
    AudioInterrupt, ChannelMap, Frames, JackState, Rate, SampleFormat,
};
use tairix_abi::driver::audio_channel::{
    decode_configure_reply, notify_endpoint_for, AttachParams, AudioChannelNotify,
    AudioChannelRequest, ConfigureParams, AUDIO_CHANNEL_MAX_REQUEST,
};
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, REGION_ALIGN_PADDING};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::waitset::WaitSetOp;
use tairix_abi::{DriverError, Errno, ProcId};

use super::{ChannelIo, Dispatcher, MappedRegion};
use crate::mock_audio::{MockAudio, MOCK_PERIOD, MOCK_RATE_HZ, SINK, SOURCE};

const MIXER: ProcId = ProcId::from_raw([9; 16]);
const NOTIFY_PORT: u64 = notify_endpoint_for(9, 0).expect("a slot");
const WATCH_PORT: u64 = notify_endpoint_for(9, 0x80).expect("a slot");
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
    /// What the next mapping is refused with, standing for a grant gone.
    refuse_map: Option<Errno>,
    /// What the next notifications are refused with, one send each.
    refuse_notify: VecDeque<Errno>,
    /// Notifications offered, taken or refused.
    offered: usize,
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
            refuse_map: None,
            refuse_notify: VecDeque::new(),
            offered: 0,
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
        if let Some(err) = self.refuse_map.take() {
            return Err(err);
        }
        self.live += 1;
        Ok(MockRegion {
            storage: vec![0; self.region_len + REGION_ALIGN_PADDING],
            len: self.region_len,
        })
    }

    fn unmap(&mut self, _region: MockRegion) {
        self.live -= 1;
    }

    fn notify(&mut self, port: u64, frame: &[u8]) -> Result<(), Errno> {
        let notify = AudioChannelNotify::decode(frame).expect("a whole notify frame");
        self.offered += 1;
        if let Some(refused) = self.refuse_notify.pop_front() {
            return Err(refused);
        }
        self.notified.push((port, notify));
        Ok(())
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

fn watch(dispatcher: &mut TestDispatcher) {
    let watched = dispatcher.io.call(&AudioChannelRequest::Watch {
        notify_endpoint: WATCH_PORT,
    });
    dispatcher.on_call();
    assert_eq!(decode_status_reply(dispatcher.io.reply(watched)), Ok(()));
}

fn jack_changes(dispatcher: &TestDispatcher) -> Vec<(u64, AudioChannelNotify)> {
    dispatcher
        .io
        .notified
        .iter()
        .filter(|(_, notify)| matches!(notify, AudioChannelNotify::JackChanged { .. }))
        .copied()
        .collect()
}

/// A connector changes whether or not anything plays through it, so the
/// change reaches the mixer's watch with nothing attached, and only there.
#[test]
fn a_jack_change_reaches_the_watch_attached_or_not() {
    let mut dispatcher = dispatcher();
    watch(&mut dispatcher);
    dispatcher.server.audio_mut().pending.jack_changed = 1 << SINK;
    dispatcher.on_event();
    let changed = AudioChannelNotify::JackChanged {
        endpoint: SINK,
        jack: JackState::Present,
    };
    assert_eq!(jack_changes(&dispatcher), [(WATCH_PORT, changed)]);
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().pending.jack_changed = 1 << SINK;
    dispatcher.on_event();
    assert_eq!(
        jack_changes(&dispatcher),
        [(WATCH_PORT, changed), (WATCH_PORT, changed)],
        "not the attachment's port"
    );
}

#[test]
fn a_jack_change_with_no_watch_is_told_to_nobody() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().pending.jack_changed = 1 << SINK;
    dispatcher.on_event();
    assert!(jack_changes(&dispatcher).is_empty());
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

/// A re-attach whose mapping is refused leaves the endpoint detached, its old
/// region gone, and the device's events masked once nothing is attached,
/// never attached over no region.
#[test]
fn a_re_attach_refused_its_mapping_leaves_the_endpoint_detached() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, 0);
    assert!(dispatcher.server.audio().events_enabled);
    dispatcher.io.refuse_map = Some(Errno::NotFound);
    let attached = dispatcher.io.call(&attach(0, RING_FRAMES));
    dispatcher.on_call();
    assert_eq!(
        decode_status_reply(dispatcher.io.reply(attached)),
        Err(Errno::NotFound)
    );
    assert!(!dispatcher.server.is_attached(0));
    assert_eq!(dispatcher.io.live, 0, "the old region was given back");
    assert!(!dispatcher.server.audio().events_enabled);
}

/// The serve loop's room members, as a test sees them.
#[derive(Default)]
struct Members {
    armed: Vec<u64>,
    /// Whether the kernel refuses every member.
    refuse: bool,
}

impl Members {
    /// Add or delete the room member of `port`, answering whether it took.
    fn apply(&mut self, op: WaitSetOp, port: u64) -> bool {
        match op {
            WaitSetOp::Add if self.refuse => false,
            WaitSetOp::Add => {
                assert!(!self.armed.contains(&port), "a port's room is armed once");
                self.armed.push(port);
                true
            }
            WaitSetOp::Del => {
                let at = self
                    .armed
                    .iter()
                    .position(|armed| *armed == port)
                    .expect("only an armed room is disarmed");
                self.armed.remove(at);
                true
            }
        }
    }
}

/// Bring the room members in line with what `dispatcher` holds, answering
/// whether every one it needs is armed.
fn arm_rooms(dispatcher: &mut TestDispatcher, members: &mut Members) -> bool {
    dispatcher.watch(&mut |op, port| members.apply(op, port))
}

/// Fault the device, so it tells the attached sink its endpoint faulted.
fn fault_sink(dispatcher: &mut TestDispatcher) {
    dispatcher.server.audio_mut().fault = Some(DriverError::DeviceFault);
    dispatcher.on_event();
}

/// The sink's fault, as told at its attachment's port.
fn faulted() -> (u64, AudioChannelNotify) {
    (
        NOTIFY_PORT,
        AudioChannelNotify::Faulted {
            endpoint: SINK,
            reason: Errno::DeviceFault,
        },
    )
}

/// A fault the mixer's port refused for want of room, or this driver's
/// budget, is held, its port's room watched, and told once room comes; then
/// the room is let go. Regression: the refusal used to drop the fault, so the
/// mixer never learned the endpoint would raise nothing more.
#[test]
fn a_notification_refused_for_room_or_budget_is_told_once_room_comes() {
    for refused in [Errno::WouldBlock, Errno::LimitExceeded] {
        let mut dispatcher = dispatcher();
        ready(&mut dispatcher, SINK);
        dispatcher.io.refuse_notify.push_back(refused);
        fault_sink(&mut dispatcher);
        assert!(dispatcher.io.notified.is_empty(), "{refused:?}: held");
        let mut members = Members::default();
        assert!(arm_rooms(&mut dispatcher, &mut members), "{refused:?}");
        assert_eq!(
            members.armed,
            [NOTIFY_PORT],
            "{refused:?}: its room is watched"
        );
        assert!(!dispatcher.starved(), "{refused:?}");
        dispatcher.on_room();
        assert_eq!(dispatcher.io.notified, [faulted()], "{refused:?}");
        assert!(arm_rooms(&mut dispatcher, &mut members), "{refused:?}");
        assert!(
            members.armed.is_empty(),
            "{refused:?}: nothing owed, nothing watched"
        );
    }
}

/// A port's held notifications go in the order they happened, a later one
/// held behind the first rather than sent around it, and a held loss absorbs
/// the next, its frames summed.
#[test]
fn held_notifications_keep_their_order_and_a_held_loss_absorbs_the_next() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.server.audio_mut().add_xrun = 5;
    dispatcher.io.refuse_notify.push_back(Errno::WouldBlock);
    for _ in 0..2 {
        dispatcher.server.audio_mut().pending.xrun = 1 << SINK;
        dispatcher.on_event();
    }
    fault_sink(&mut dispatcher);
    assert!(
        dispatcher.io.notified.is_empty(),
        "held behind the first refusal"
    );
    dispatcher.on_room();
    let told: Vec<AudioChannelNotify> = dispatcher
        .io
        .notified
        .iter()
        .map(|(_, told)| *told)
        .collect();
    assert!(
        matches!(
            told.as_slice(),
            [
                AudioChannelNotify::Xrun {
                    lost_frames: 10,
                    ..
                },
                AudioChannelNotify::Faulted { .. },
            ]
        ),
        "{told:?}"
    );
}

/// A notification the kernel had no memory for waits on the pause the serve
/// loop times, with no room member armed for it — one would report room at
/// once, for a send the kernel would refuse again — and is told when the
/// pause has run. Regression: the refusal used to drop it.
#[test]
fn a_notification_refused_for_memory_waits_on_the_pause_with_no_room_watched() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.io.refuse_notify.push_back(Errno::OutOfMemory);
    fault_sink(&mut dispatcher);
    assert!(dispatcher.starved());
    let mut members = Members::default();
    assert!(arm_rooms(&mut dispatcher, &mut members));
    assert!(members.armed.is_empty(), "nothing armed for kernel memory");
    dispatcher.on_room();
    assert_eq!(dispatcher.io.notified, [faulted()]);
    assert!(!dispatcher.starved());
}

/// A notification refused for good is given up once — counted, reported and
/// never offered again — and one to a port gone with its mixer is dropped and
/// reported to nobody.
#[test]
fn a_refusal_for_good_is_reported_once_and_a_port_gone_is_not() {
    for (refused, reported) in [(Errno::PermissionDenied, Some(1)), (Errno::NotFound, None)] {
        let mut dispatcher = dispatcher();
        ready(&mut dispatcher, SINK);
        dispatcher.io.refuse_notify.push_back(refused);
        fault_sink(&mut dispatcher);
        assert_eq!(dispatcher.given_up(), reported, "{refused:?}");
        assert!(!dispatcher.starved(), "{refused:?}: nothing held");
        let mut members = Members::default();
        assert!(arm_rooms(&mut dispatcher, &mut members), "{refused:?}");
        assert!(members.armed.is_empty(), "{refused:?}");
        dispatcher.on_room();
        assert_eq!(dispatcher.io.offered, 1, "{refused:?}: never offered again");
        assert!(dispatcher.io.notified.is_empty(), "{refused:?}");
    }
}

/// A port whose room cannot be watched is answered, so the serve loop offers
/// what it is owed again once a pause has run rather than waiting on a wake
/// that never comes.
#[test]
fn a_port_whose_room_cannot_be_watched_is_left_to_the_pause() {
    let mut dispatcher = dispatcher();
    ready(&mut dispatcher, SINK);
    dispatcher.io.refuse_notify.push_back(Errno::WouldBlock);
    fault_sink(&mut dispatcher);
    let mut members = Members {
        armed: Vec::new(),
        refuse: true,
    };
    assert!(!arm_rooms(&mut dispatcher, &mut members));
    assert!(members.armed.is_empty());
}

/// A notification held for an attachment the mixer let go or replaced is
/// never told to the attachment after it, while a connector's change held for
/// the watch still is. Regression: a held report of the old configuration was
/// told to the next, whose clock then took a stale position.
#[test]
fn a_re_attach_hears_nothing_held_for_the_attachment_it_replaced() {
    for (detached, reconfigured) in [(false, false), (false, true), (true, true)] {
        let mut dispatcher = dispatcher();
        watch(&mut dispatcher);
        ready(&mut dispatcher, SINK);
        dispatcher
            .io
            .refuse_notify
            .extend([Errno::WouldBlock, Errno::WouldBlock]);
        fault_sink(&mut dispatcher);
        dispatcher.server.audio_mut().fault = None;
        dispatcher.server.audio_mut().pending.jack_changed = 1 << SINK;
        dispatcher.on_event();
        assert!(dispatcher.io.notified.is_empty(), "both held");
        if detached {
            let detach = dispatcher
                .io
                .call(&AudioChannelRequest::Detach { endpoint: SINK });
            dispatcher.on_call();
            assert_eq!(decode_status_reply(dispatcher.io.reply(detach)), Ok(()));
        }
        if reconfigured {
            let configured = dispatcher.io.call(&configure(SINK));
            dispatcher.on_call();
            assert!(decode_configure_reply(dispatcher.io.reply(configured)).is_ok());
        }
        let attached = dispatcher.io.call(&attach(SINK, RING_FRAMES));
        dispatcher.on_call();
        assert_eq!(decode_status_reply(dispatcher.io.reply(attached)), Ok(()));
        dispatcher.on_room();
        let changed = AudioChannelNotify::JackChanged {
            endpoint: SINK,
            jack: JackState::Present,
        };
        assert_eq!(
            dispatcher.io.notified,
            [(WATCH_PORT, changed)],
            "detached {detached}, reconfigured {reconfigured}"
        );
    }
}

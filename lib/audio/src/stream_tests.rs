//! Unit tests for the `audio-v1` client.
//!
//! The service is a mock that decodes what the client sent and answers with
//! the real wire encoders, so the exchange is checked against the protocol
//! rather than against an agreement between the test and the code under it.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    devices, set_control, streams, AudioTransport, ControlQueue, DeviceControl, NotifyDrain,
    StreamClient,
};
use tairix_abi::audio::{
    encode_clock_reply, encode_enumerate_reply, encode_open_reply, encode_state_reply,
    encode_streams_reply, notify_endpoint_for, AudioDeviceDescriptor, AudioGain, AudioLocation,
    AudioNotify, AudioRequest, ClockReport, ControlAccess, DefaultChoice, OpenParams,
    StreamDescriptor, StreamGrant, StreamReport, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{
    AudioName, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormat, SampleFormats,
    StreamDirection,
};
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, PcmRing, REGION_ALIGN_PADDING};
use tairix_abi::reply::encode_status_reply;
use tairix_abi::time::{Duration64, Time64};
use tairix_abi::Errno;

const STREAM_ID: u64 = 0x1234;
const RING_FRAMES: u32 = 8;

fn grant() -> StreamGrant {
    StreamGrant {
        stream_id: STREAM_ID,
        notify_endpoint: notify_endpoint_for(42, 0),
        rate: Rate::HZ_48000,
        format: SampleFormat::S16,
        channel_map: ChannelMap::STEREO,
        ring_frames: RING_FRAMES,
        // The grant's latency cannot exceed its ring, which the protocol's
        // own decoder enforces.
        granted_latency_frames: RING_FRAMES,
        granted_latency: Duration64::new(0, 166_666).expect("eight frames at 48 kHz"),
        clock_domain: 1,
    }
}

fn params(direction: StreamDirection) -> OpenParams {
    OpenParams {
        device_id: 7,
        direction,
        format: SampleFormat::S16,
        rate: Rate::HZ_48000,
        channel_map: ChannelMap::STEREO,
        role: StreamRole::Media,
        latency_target_frames: 480,
    }
}

/// The mock service's two sinks and one source with the least id above
/// `after`, or `NotFound` past them.
fn device(direction: StreamDirection, after: u32) -> Result<AudioDeviceDescriptor, Errno> {
    let count = match direction {
        StreamDirection::Playback => 2,
        StreamDirection::Capture => 1,
    };
    let index = u16::try_from(after)
        .ok()
        .filter(|&index| index < count)
        .ok_or(Errno::NotFound)?;
    Ok(AudioDeviceDescriptor {
        device_id: after + 1,
        direction,
        jack: JackState::Present,
        default: if index == 0 {
            DefaultChoice::Inherited
        } else {
            DefaultChoice::No
        },
        formats: SampleFormats::EMPTY.with(SampleFormat::S16),
        channel_map: ChannelMap::STEREO,
        rates: RateSupport::Continuous {
            min: Rate::HZ_48000,
            max: Rate::HZ_48000,
        },
        gain: None,
        name: AudioName::new("Device").expect("a short name"),
        location: AudioLocation::new(0x51, index).expect("a place"),
        level: AudioGain::UNITY,
        muted: false,
        own_level: false,
        access: ControlAccess::Shared,
        clock_millihertz: 0,
        lost_frames: 0,
    })
}

/// The mock service's two streams with the least id above `after`.
fn stream(after: u64) -> Option<StreamDescriptor> {
    let stream_id = after.max(STREAM_ID - 1) + 1;
    (stream_id <= STREAM_ID + 1).then_some(StreamDescriptor {
        stream_id,
        device_id: 1,
        direction: StreamDirection::Playback,
        role: StreamRole::Media,
        state: StreamState::Running,
        position: Frames::new(0),
        xruns: 0,
        xrun_frames: 0,
        owner_uid: 1_000,
        owner_pid: 9,
        owner_app: None,
    })
}

/// A service that records what it was asked and answers from the protocol's
/// own encoders.
struct MockService {
    seen: Vec<AudioRequest>,
    notifications: VecDeque<AudioNotify>,
    refuse: Option<Errno>,
}

impl MockService {
    fn new() -> Self {
        Self {
            seen: Vec::new(),
            notifications: VecDeque::new(),
            refuse: None,
        }
    }

    fn last(&self) -> AudioRequest {
        *self.seen.last().expect("a request was sent")
    }
}

impl AudioTransport for MockService {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let decoded = AudioRequest::decode(request)?;
        self.seen.push(decoded);
        let frame: Vec<u8> = match decoded {
            AudioRequest::Open(_) => match self.refuse {
                Some(err) => encode_open_reply(Err(err)).to_vec(),
                None => encode_open_reply(Ok(grant())).to_vec(),
            },
            AudioRequest::Enumerate { direction, after } => {
                encode_enumerate_reply(device(direction, after)).to_vec()
            }
            AudioRequest::ListStreams { after } => {
                encode_streams_reply(stream(after).ok_or(Errno::NotFound)).to_vec()
            }
            AudioRequest::Clock { .. } => encode_clock_reply(Ok(ClockReport {
                position: Frames::new(9_600),
                rate_millihertz: 47_998_600,
                sampled_at: Time64::new(5, 0).expect("a canonical time"),
            }))
            .to_vec(),
            AudioRequest::State { .. } => encode_state_reply(Ok(StreamReport {
                state: StreamState::SeatInactive,
                changed_at: Frames::new(4_800),
                xruns: 2,
                xrun_frames: 128,
            }))
            .to_vec(),
            _ => encode_status_reply(self.refuse.map_or(Ok(()), Err)).to_vec(),
        };
        let Some(slot) = reply.get_mut(..frame.len()) else {
            return Err(Errno::BufferTooSmall);
        };
        slot.copy_from_slice(&frame);
        Ok(frame.len())
    }

    fn wait_notify(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
        self.try_notify(out)?.ok_or(Errno::WouldBlock)
    }

    fn try_notify(&mut self, out: &mut [u8]) -> Result<Option<usize>, Errno> {
        let Some(notify) = self.notifications.pop_front() else {
            return Ok(None);
        };
        let frame = notify.encode();
        let Some(slot) = out.get_mut(..frame.len()) else {
            return Err(Errno::BufferTooSmall);
        };
        slot.copy_from_slice(&frame);
        Ok(Some(frame.len()))
    }
}

/// A region and its ring, in one place so a test does not have to repeat the
/// alignment dance.
struct Region {
    buffer: Vec<u8>,
    geometry: PcmGeometry,
}

impl Region {
    fn new() -> Self {
        let geometry =
            PcmGeometry::new(RING_FRAMES, SampleFormat::S16, 2).expect("a valid geometry");
        Self {
            buffer: vec![0u8; geometry.region_len() + REGION_ALIGN_PADDING],
            geometry,
        }
    }

    fn ring(&mut self) -> PcmRing<'_> {
        let region = aligned_region(&mut self.buffer, self.geometry.region_len())
            .expect("an aligned region");
        PcmRing::bind(region, self.geometry).expect("binds")
    }
}

/// Interleaved stereo frames.
fn frames(count: usize) -> Vec<u8> {
    (0..count * 2)
        .flat_map(|n| i16::try_from(n).unwrap_or(0).to_le_bytes())
        .collect()
}

fn open(service: &mut MockService) -> StreamClient {
    StreamClient::open(service, &params(StreamDirection::Playback)).expect("opened")
}

#[test]
fn opening_adopts_the_grant_the_service_answered() {
    let mut service = MockService::new();
    let client = open(&mut service);
    assert_eq!(client.grant(), grant());
    assert_eq!(client.state(), StreamState::Idle);
    assert_eq!(client.direction(), StreamDirection::Playback);
    assert_eq!(
        service.last(),
        AudioRequest::Open(params(StreamDirection::Playback))
    );
}

#[test]
fn a_refused_open_is_the_services_own_error() {
    let mut service = MockService::new();
    service.refuse = Some(Errno::PermissionDenied);
    assert_eq!(
        StreamClient::open(&mut service, &params(StreamDirection::Capture)).err(),
        Some(Errno::PermissionDenied)
    );
}

#[test]
fn the_transport_control_operations_carry_the_stream_and_its_position() {
    let mut service = MockService::new();
    let mut client = open(&mut service);

    client.attach(&mut service, 0xABCD).expect("attached");
    assert_eq!(
        service.last(),
        AudioRequest::Attach {
            stream_id: STREAM_ID,
            region_grant: 0xABCD
        }
    );

    client
        .start(&mut service, Frames::new(100))
        .expect("started");
    assert_eq!(client.state(), StreamState::Running);
    assert_eq!(client.changed_at(), Frames::new(100));

    client
        .stop(&mut service, Frames::new(200))
        .expect("stopped");
    assert_eq!(client.state(), StreamState::Paused);
    assert_eq!(client.changed_at(), Frames::new(200));

    client.drain(&mut service).expect("drained");
    assert_eq!(client.state(), StreamState::Draining);

    client.flush(&mut service).expect("flushed");
    assert_eq!(
        service.last(),
        AudioRequest::Flush {
            stream_id: STREAM_ID
        }
    );

    let gain = AudioGain::new(-600).expect("attenuation");
    client.set_gain(&mut service, gain).expect("gain set");
    assert_eq!(
        service.last(),
        AudioRequest::Gain {
            stream_id: STREAM_ID,
            gain
        }
    );

    client.set_mute(&mut service, true).expect("muted");
    assert_eq!(
        service.last(),
        AudioRequest::Mute {
            stream_id: STREAM_ID,
            muted: true
        }
    );

    client.close(&mut service).expect("closed");
    assert_eq!(
        service.last(),
        AudioRequest::Close {
            stream_id: STREAM_ID
        }
    );
}

#[test]
fn a_refused_operation_surfaces_the_services_error() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    service.refuse = Some(Errno::NotAttached);
    assert_eq!(
        client.start(&mut service, Frames::ZERO).err(),
        Some(Errno::NotAttached)
    );
    // And the local state was not advanced on a refusal.
    assert_eq!(client.state(), StreamState::Idle);
}

#[test]
fn the_clock_and_the_state_come_back_decoded() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    let clock = client.clock(&mut service).expect("clock");
    assert_eq!(clock.rate_millihertz, 47_998_600);
    assert_eq!(clock.position, Frames::new(9_600));

    let report = client.report(&mut service).expect("state");
    assert_eq!(report.state, StreamState::SeatInactive);
    assert_eq!(report.xruns, 2);
    // Reading the state adopts it, so a client that asks knows where it is.
    assert_eq!(client.state(), StreamState::SeatInactive);
    assert_eq!(client.changed_at(), Frames::new(4_800));
}

#[test]
fn writing_at_the_ring_position_publishes_the_samples_unchanged() {
    let mut service = MockService::new();
    let client = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    let payload = frames(4);
    let written = client
        .write_at(&mut ring, Frames::ZERO, &payload)
        .expect("written");
    assert_eq!(written.silence_frames, 0);
    assert_eq!(written.sample_frames, 4);
    let mut back = vec![0u8; payload.len()];
    assert_eq!(ring.read(&mut back), Ok(4));
    assert_eq!(back, payload);
}

/// The position never lies about where the samples that follow it belong,
/// which is what makes gapless playback exact arithmetic.
#[test]
fn a_gap_ahead_of_the_ring_is_closed_with_silence_first() {
    let mut service = MockService::new();
    let client = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    let payload = frames(2);
    let written = client
        .write_at(&mut ring, Frames::new(3), &payload)
        .expect("written");
    assert_eq!(written.silence_frames, 3);
    assert_eq!(written.sample_frames, 2);
    assert_eq!(ring.producer_position(), Ok(Frames::new(5)));
    let mut back = vec![0u8; 5 * 4];
    assert_eq!(ring.read(&mut back), Ok(5));
    assert!(
        back[..12].iter().all(|byte| *byte == 0),
        "the gap is not silent"
    );
    assert_eq!(&back[12..], &payload[..]);
}

/// Those frames are published and may already be audible; dropping the
/// request quietly would leave the caller believing they were not.
#[test]
fn writing_behind_the_ring_position_is_refused() {
    let mut service = MockService::new();
    let client = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    client
        .write_at(&mut ring, Frames::ZERO, &frames(4))
        .expect("written");
    assert_eq!(
        client.write_at(&mut ring, Frames::new(2), &frames(1)),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_full_ring_takes_what_it_can_and_the_caller_offers_the_rest() {
    let mut service = MockService::new();
    let client = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    let payload = frames(12);
    let written = client
        .write_at(&mut ring, Frames::ZERO, &payload)
        .expect("written");
    assert_eq!(written.sample_frames, RING_FRAMES);
    assert!(!written.is_empty());
    // Nothing further fits until the service takes some.
    let again = client
        .write_at(&mut ring, Frames::new(u64::from(RING_FRAMES)), &payload)
        .expect("written");
    assert!(again.is_empty(), "a full ring took {again:?}");
}

/// A gap larger than the ring stops inside the gap rather than publishing
/// the caller's samples at the wrong position.
#[test]
fn a_gap_that_fills_the_ring_leaves_the_samples_for_the_next_offer() {
    let mut service = MockService::new();
    let client = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    let written = client
        .write_at(&mut ring, Frames::new(100), &frames(2))
        .expect("written");
    assert_eq!(written.silence_frames, RING_FRAMES);
    assert_eq!(written.sample_frames, 0);
}

#[test]
fn a_direction_the_stream_is_not_is_refused_on_both_halves() {
    let mut service = MockService::new();
    let playback = open(&mut service);
    let mut region = Region::new();
    let mut ring = region.ring();
    let mut out = vec![0u8; 16];
    assert_eq!(
        playback.read_into(&mut ring, &mut out),
        Err(Errno::NotSupported)
    );

    let capture =
        StreamClient::open(&mut service, &params(StreamDirection::Capture)).expect("opened");
    assert_eq!(
        capture.write_at(&mut ring, Frames::ZERO, &frames(1)),
        Err(Errno::NotSupported)
    );
}

#[test]
fn a_capture_stream_takes_the_frames_the_service_left() {
    let mut service = MockService::new();
    let capture =
        StreamClient::open(&mut service, &params(StreamDirection::Capture)).expect("opened");
    let mut region = Region::new();
    let mut ring = region.ring();
    let payload = frames(3);
    assert_eq!(ring.write(&payload), Ok(3));
    let mut out = vec![0u8; payload.len()];
    assert_eq!(capture.read_into(&mut ring, &mut out), Ok(3));
    assert_eq!(out, payload);
}

#[test]
fn a_state_notification_is_adopted_and_another_streams_is_not() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    service.notifications.push_back(AudioNotify::StateChanged {
        stream_id: STREAM_ID,
        state: StreamState::SeatInactive,
        at: Frames::new(777),
    });
    let notify = client.await_notify(&mut service).expect("woken");
    assert!(matches!(notify, AudioNotify::StateChanged { .. }));
    assert_eq!(client.state(), StreamState::SeatInactive);
    assert_eq!(client.changed_at(), Frames::new(777));

    // A notification naming a different stream is handed back untouched for
    // the caller to demultiplex, and changes nothing here.
    service.notifications.push_back(AudioNotify::StateChanged {
        stream_id: STREAM_ID + 1,
        state: StreamState::DeviceLost,
        at: Frames::new(1),
    });
    client.await_notify(&mut service).expect("woken");
    assert_eq!(client.state(), StreamState::SeatInactive);
}

#[test]
fn a_space_available_notification_changes_no_state() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    client.start(&mut service, Frames::ZERO).expect("started");
    service
        .notifications
        .push_back(AudioNotify::SpaceAvailable {
            stream_id: STREAM_ID,
            position: Frames::new(64),
        });
    client.await_notify(&mut service).expect("woken");
    assert_eq!(client.state(), StreamState::Running);
}

#[test]
fn a_malformed_notification_frame_is_refused() {
    struct Garbage;
    impl AudioTransport for Garbage {
        fn call(&mut self, _: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            let frame = encode_open_reply(Ok(grant()));
            reply[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        }
        fn wait_notify(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
            out[..8].fill(0xAA);
            Ok(8)
        }
        fn try_notify(&mut self, out: &mut [u8]) -> Result<Option<usize>, Errno> {
            self.wait_notify(out).map(Some)
        }
    }
    let mut garbage = Garbage;
    let mut client = open(&mut MockService::new());
    assert!(client.await_notify(&mut garbage).is_err());
    assert!(client
        .take_notify(&mut garbage, &mut NotifyDrain::new(4))
        .is_err());
}

/// A transport that claims more bytes than it was given room for.
struct Overlong;

impl AudioTransport for Overlong {
    fn call(&mut self, _: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        Ok(reply.len() + 1)
    }
    fn wait_notify(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
        Ok(out.len() + 1)
    }
    fn try_notify(&mut self, out: &mut [u8]) -> Result<Option<usize>, Errno> {
        Ok(Some(out.len() + 1))
    }
}

#[test]
fn a_transport_claiming_more_than_its_buffer_is_refused_not_trusted() {
    let mut overlong = Overlong;
    assert!(matches!(
        StreamClient::open(&mut overlong, &params(StreamDirection::Playback)),
        Err(Errno::LengthOutOfRange)
    ));
    let mut client = open(&mut MockService::new());
    assert_eq!(client.report(&mut overlong), Err(Errno::LengthOutOfRange));
    assert_eq!(
        client.await_notify(&mut overlong),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        client.take_notify(&mut overlong, &mut NotifyDrain::new(4)),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_walk_lists_each_direction_by_id_to_its_end() {
    let mut service = MockService::new();
    let sinks: Vec<u32> = devices(&mut service, StreamDirection::Playback)
        .expect("listed")
        .iter()
        .map(|device| device.device_id)
        .collect();
    assert_eq!(sinks, vec![1, 2]);
    assert_eq!(
        service.last(),
        AudioRequest::Enumerate {
            direction: StreamDirection::Playback,
            after: 2
        }
    );
    let sources = devices(&mut service, StreamDirection::Capture).expect("listed");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].direction, StreamDirection::Capture);
    let held: Vec<u64> = streams(&mut service)
        .expect("listed")
        .iter()
        .map(|stream| stream.stream_id)
        .collect();
    assert_eq!(held, vec![STREAM_ID, STREAM_ID + 1]);
}

/// A service answering an id that does not ascend would keep a walk going
/// for ever; the walk refuses it, and a refusal ends it with the reason.
#[test]
fn a_walk_refuses_an_id_that_does_not_ascend_and_ends_on_a_refusal() {
    struct Stuck;
    impl AudioTransport for Stuck {
        fn call(&mut self, _: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            let frame = encode_enumerate_reply(device(StreamDirection::Playback, 0));
            reply[..frame.len()].copy_from_slice(&frame);
            Ok(frame.len())
        }
        fn wait_notify(&mut self, _: &mut [u8]) -> Result<usize, Errno> {
            Err(Errno::WouldBlock)
        }
        fn try_notify(&mut self, _: &mut [u8]) -> Result<Option<usize>, Errno> {
            Ok(None)
        }
    }
    assert_eq!(
        devices(&mut Stuck, StreamDirection::Playback),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        devices(&mut Overlong, StreamDirection::Playback),
        Err(Errno::LengthOutOfRange)
    );
}

/// Only a drain that took a whole mailbox's worth can follow a dropped
/// notification, so only that one costs a read-back.
#[test]
fn a_drain_of_a_full_mailbox_and_only_that_reads_the_state_back() {
    let mut drain = NotifyDrain::new(4);
    for _ in 0..3 {
        drain.took();
    }
    assert!(!drain.emptied(), "a mailbox never full dropped nothing");
    for _ in 0..4 {
        drain.took();
    }
    assert!(drain.emptied());
    assert!(!drain.emptied(), "each drain is counted afresh");
    for _ in 0..9 {
        drain.took();
    }
    assert!(
        drain.emptied(),
        "more than a mailbox's worth arrived during the drain"
    );
}

/// A full mailbox may have dropped the stream's last word, so the drain that
/// empties it hands over the state the service reports, adopted.
#[test]
fn a_drain_that_emptied_a_full_mailbox_hands_over_the_reported_state() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    let mut drain = NotifyDrain::new(2);
    for position in [64, 128] {
        service
            .notifications
            .push_back(AudioNotify::SpaceAvailable {
                stream_id: STREAM_ID,
                position: Frames::new(position),
            });
    }
    let mut taken = Vec::new();
    while let Some(notify) = client.take_notify(&mut service, &mut drain).expect("taken") {
        taken.push(notify);
    }
    assert_eq!(
        taken.last(),
        Some(&AudioNotify::StateChanged {
            stream_id: STREAM_ID,
            state: StreamState::SeatInactive,
            at: Frames::new(4_800),
        })
    );
    assert_eq!(taken.len(), 3);
    assert_eq!(client.state(), StreamState::SeatInactive);
    assert_eq!(client.changed_at(), Frames::new(4_800));
}

#[test]
fn a_drain_short_of_a_full_mailbox_asks_the_service_nothing() {
    let mut service = MockService::new();
    let mut client = open(&mut service);
    let mut drain = NotifyDrain::new(3);
    service.notifications.push_back(AudioNotify::StateChanged {
        stream_id: STREAM_ID,
        state: StreamState::Running,
        at: Frames::new(16),
    });
    let asked = service.seen.len();
    assert!(client
        .take_notify(&mut service, &mut drain)
        .expect("taken")
        .is_some());
    assert_eq!(client.take_notify(&mut service, &mut drain), Ok(None));
    assert_eq!(service.seen.len(), asked);
    assert_eq!(client.state(), StreamState::Running);
}

#[test]
fn a_device_control_is_the_request_it_names_and_answers_the_services_status() {
    let mut service = MockService::new();
    let level = AudioGain::new(-1_200).expect("attenuation");
    for (control, sent) in [
        (
            DeviceControl::Default,
            AudioRequest::SetDefault { device_id: 2 },
        ),
        (
            DeviceControl::Level(level),
            AudioRequest::SetLevel {
                device_id: 2,
                level,
            },
        ),
        (
            DeviceControl::Mute(true),
            AudioRequest::SetMute {
                device_id: 2,
                muted: true,
            },
        ),
    ] {
        assert_eq!(set_control(&mut service, 2, control), Ok(()));
        assert_eq!(service.last(), sent);
    }
    service.refuse = Some(Errno::SeatNotOwner);
    assert_eq!(
        set_control(&mut service, 2, DeviceControl::Mute(false)),
        Err(Errno::SeatNotOwner)
    );
}

#[test]
fn a_control_queue_keeps_one_trip_in_flight_and_the_latest_of_each_kind() {
    let level = |millibel| DeviceControl::Level(AudioGain::new(millibel).expect("attenuation"));
    let mut queue = ControlQueue::new();
    assert_eq!(queue.next_trip(), None, "nothing owed");
    queue.refresh();
    assert_eq!(queue.next_trip(), Some(Vec::new()), "a listing alone");
    for millibel in [-3_000, -2_000, -1_000] {
        queue.ask(3, level(millibel));
    }
    queue.ask(3, DeviceControl::Mute(true));
    queue.refresh();
    assert_eq!(queue.next_trip(), None, "one in flight");
    queue.landed();
    queue.ask_first(vec![(4, DeviceControl::Default)]);
    assert_eq!(
        queue.next_trip(),
        Some(vec![
            (4, DeviceControl::Default),
            (3, level(-1_000)),
            (3, DeviceControl::Mute(true)),
        ])
    );
    queue.landed();
    assert_eq!(
        queue.next_trip(),
        None,
        "what was owed went with the last trip"
    );
    queue.refresh();
    let _ = queue.next_trip();
    queue.refresh();
    queue.landed();
    assert_eq!(
        queue.next_trip(),
        Some(Vec::new()),
        "a change while in flight is listed again after it"
    );
}

#[test]
fn the_default_chosen_last_is_the_one_applied_last() {
    let mut queue = ControlQueue::new();
    for device_id in [4, 5, 4] {
        queue.ask(device_id, DeviceControl::Default);
    }
    assert_eq!(
        queue.next_trip(),
        Some(vec![
            (5, DeviceControl::Default),
            (4, DeviceControl::Default)
        ])
    );
}

#[test]
fn a_control_asked_first_gives_way_to_a_newer_one_of_its_kind() {
    let level = |millibel| DeviceControl::Level(AudioGain::new(millibel).expect("attenuation"));
    let mut queue = ControlQueue::new();
    queue.ask(3, level(-2_000));
    queue.ask_first(vec![(3, level(-500)), (3, DeviceControl::Mute(true))]);
    queue.ask(3, level(-1_000));
    assert_eq!(
        queue.next_trip(),
        Some(vec![(3, DeviceControl::Mute(true)), (3, level(-1_000))]),
        "the newest level is the one applied, and applied once"
    );
}

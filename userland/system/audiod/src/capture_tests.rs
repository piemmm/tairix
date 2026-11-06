//! Host tests of what a capture stream hears beside a microphone: a sink's
//! monitor — its gate, the mix it is given and what it costs the device — and
//! a source heard at a client's own rate and role (`plans/CAPTURE.md` CP9).

use tairix_abi::audio::{CaptureSource, MONITOR_FLAG};
use tairix_audio::route::{Admission, Principal};
use tairix_log::{Event, EventId};

use super::*;
use crate::events;
use crate::service::monitor_admission;

/// A sink keeping the id of every record the service makes.
#[derive(Default)]
struct Audit(RefCell<Vec<EventId>>);

impl Audit {
    /// Whether a record of `id` was made.
    fn saw(&self, id: EventId) -> bool {
        self.0.borrow().contains(&id)
    }
}

impl Sink for Audit {
    fn write_event(&self, event: &Event<'_>) {
        self.0.borrow_mut().push(event.id);
    }
}

/// A caller holding the capture capability, in the session tagged `tag`.
fn recording(pid: u64, tag: u8) -> Origin {
    caller(pid, &[CapabilityId::AUDIO_CAPTURE]).with_login_session(session(tag))
}

/// The fixture's device bound, its seat held by the session tagged `tag`.
fn held_by(tag: u8) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.bind();
    fixture.seat(DisplayLease::held(1, session(tag)));
    fixture
}

/// The device id a capture stream opens to hear the sink `sink` names.
fn monitor_of(sink: u32) -> u32 {
    CaptureSource::Monitor(sink)
        .device_id()
        .expect("an id below the flag")
}

/// What a client hearing the fixture sink's mix at the device's rate asks
/// for, its ring holding `latency` frames.
fn monitor_params(latency: u32) -> OpenParams {
    capture_params(monitor_of(SINK), StreamRole::Media, DEVICE_HZ, latency)
}

/// Open what `params` ask for on `owner`'s behalf, the service's records made
/// into `audit`, and attach its ring once granted.
fn listen(
    fixture: &mut Fixture,
    owner: &Origin,
    params: OpenParams,
    audit: &Audit,
) -> Result<StreamGrant, Errno> {
    let reply = fixture.call_logged(owner, &AudioRequest::Open(params), audit);
    let grant = decode_open_reply(&reply)?;
    fixture.attach_capture(owner, &grant);
    Ok(grant)
}

/// Open, attach and start what `params` ask for on `owner`'s behalf.
fn started(fixture: &mut Fixture, owner: &Origin, params: OpenParams) -> StreamGrant {
    let grant = listen(fixture, owner, params, &Audit::default()).expect("the open is granted");
    start(fixture, owner, &grant);
    grant
}

/// The bytes the monitor's start primes an idle sink with — two periods of
/// silence — which the device and the monitor both begin with.
fn primed() -> usize {
    bytes(Frames::new(u64::from(PERIOD) * 2))
}

/// A monitor needs the capture capability: the room alone is not it.
#[test]
fn a_monitor_is_refused_without_the_capture_capability() {
    let mut fixture = held_by(0xA1);
    let audit = Audit::default();
    let plain = in_session(7, 0xA1);
    let refused = listen(&mut fixture, &plain, monitor_params(2_048), &audit);
    assert_eq!(refused, Err(Errno::PermissionDenied));
    assert!(audit.saw(events::MONITOR_REFUSED), "who tried is recorded");
    assert_eq!(fixture.service.stream_count(), 0);
    assert!(
        fixture.device.borrow().audio().configured.is_none(),
        "a refusal programs nothing"
    );
}

/// A monitor hears only its own session's room: not another session's, not
/// an unclaimed one anybody may play into, not one changing hands, and never
/// for a caller in no session.
#[test]
fn a_monitor_is_refused_outside_its_own_sessions_room() {
    let recorder = recording(8, 0xA1);
    for lease in [
        DisplayLease::held(1, session(0xB0)),
        DisplayLease::UNHELD,
        DisplayLease::ended(1, ReleaseSurface::Handover),
    ] {
        let mut fixture = Fixture::new();
        fixture.bind();
        fixture.seat(lease);
        let audit = Audit::default();
        let refused = listen(&mut fixture, &recorder, monitor_params(2_048), &audit);
        assert_eq!(refused, Err(Errno::SeatNotOwner), "{lease:?}");
        assert!(audit.saw(events::MONITOR_REFUSED), "{lease:?}");
        assert!(fixture.device.borrow().audio().configured.is_none());
    }
    let mut fixture = held_by(0xA1);
    let daemon = caller(9, &[CapabilityId::AUDIO_CAPTURE]);
    let audit = Audit::default();
    let refused = listen(&mut fixture, &daemon, monitor_params(2_048), &audit);
    assert_eq!(refused, Err(Errno::SeatNotOwner), "a daemon has no room");
}

/// With the capability and the room a monitor opens on its sink's own clock,
/// recorded as a monitor rather than a microphone. Zero names the default
/// sink's; a monitor names a sink, never a source or a missing device; and it
/// is the caller's own, never the machine's.
#[test]
fn a_monitor_with_both_is_granted_and_told_apart_from_a_microphone() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let audit = Audit::default();
    let of_default = capture_params(monitor_of(0), StreamRole::Media, DEVICE_HZ, 2_048);
    let mix = listen(&mut fixture, &recorder, of_default, &audit).expect("both");
    assert!(audit.saw(events::MONITOR_OPENED));
    assert!(
        !audit.saw(events::CAPTURE_OPENED),
        "told apart from a microphone"
    );
    assert_eq!(mix.clock_domain, SINK, "on the sink's own clock");
    for (named, refusal) in [(2, Errno::NotSupported), (99, Errno::NotFound)] {
        let audit = Audit::default();
        let params = capture_params(monitor_of(named), StreamRole::Media, DEVICE_HZ, 2_048);
        let refused = listen(&mut fixture, &recorder, params, &audit);
        assert_eq!(refused, Err(refusal), "a monitor of device {named}");
        assert!(audit.saw(events::MONITOR_REFUSED));
    }
    let machine = AudioRequest::Open(OpenParams {
        tenancy: Tenancy::Machine,
        ..monitor_params(2_048)
    });
    assert_eq!(
        decode_open_reply(&fixture.call(&recorder, &machine)),
        Err(Errno::PermissionDenied),
        "a monitor is the caller's own"
    );
}

/// Each sink advertises its monitor, and no monitor is listed among the
/// sources, where a list of inputs would offer a speaker's mix as a
/// microphone; a monitor has no controls of its own.
#[test]
fn a_sink_advertises_its_monitor_and_the_sources_list_none() {
    let mut fixture = held_by(0xA1);
    let alice = in_session(7, 0xA1);
    assert_eq!(
        fixture.sink(&alice, SINK).monitor(),
        Some(CaptureSource::Monitor(SINK))
    );
    let sources = fixture.devices(&alice, StreamDirection::Capture);
    assert_eq!(sources.len(), 1, "the microphone alone");
    assert!(sources
        .iter()
        .all(|source| source.monitor().is_none() && source.device_id & MONITOR_FLAG == 0));
    let mute = AudioRequest::SetMute {
        device_id: monitor_of(SINK),
        muted: true,
    };
    assert_eq!(fixture.control(&alice, &mute), Err(Errno::NotFound));
}

/// At the sink's own level a monitor hears the very frames the device was
/// given, two streams summed and dithered onto its grid, bit for bit.
#[test]
fn a_monitor_hears_the_frames_the_device_was_given() {
    let mut fixture = held_by(0xA1);
    let mix = started(&mut fixture, &recording(8, 0xA1), monitor_params(4_096));
    let (alice, also) = (in_session(7, 0xA1), in_session(6, 0xA1));
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    let voice = fixture.open_playback(&also, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(1_000));
    fixture.client_write(&voice, &other_signal(1_000));
    start(&mut fixture, &alice, &music);
    start(&mut fixture, &also, &voice);
    run(&mut fixture, 4);
    let played = fixture.played();
    let heard = fixture.client_read(&mix);
    assert!(
        played[primed()..].iter().any(|byte| *byte != 0),
        "the mix reached the device"
    );
    assert!(
        heard.len() >= played.len(),
        "everything the device was given"
    );
    assert!(
        heard[..played.len()] == played[..],
        "the device's own frames, bit for bit"
    );
}

/// A monitor hears the mix before the sink's own level and mute, so a
/// recording does not follow the speaker's volume: the room hears the music
/// six decibels down, or not at all, and the monitor hears it as it is.
#[test]
fn a_monitor_hears_the_mix_before_the_sinks_level_and_mute() {
    for muted in [false, true] {
        let mut fixture = held_by(0xA1);
        let alice = in_session(7, 0xA1);
        let quieter = AudioRequest::SetLevel {
            device_id: SINK,
            level: level(-600),
        };
        fixture.control(&alice, &quieter).expect("the tenant's");
        let silenced = AudioRequest::SetMute {
            device_id: SINK,
            muted,
        };
        fixture.control(&alice, &silenced).expect("the tenant's");
        let mix = started(&mut fixture, &recording(8, 0xA1), monitor_params(4_096));
        let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
        let samples = signal(1_000);
        fixture.client_write(&music, &samples);
        start(&mut fixture, &alice, &music);
        run(&mut fixture, 4);
        let at = primed()..primed() + samples.len();
        assert!(
            fixture.client_read(&mix)[at.clone()] == samples[..],
            "muted: {muted}"
        );
        let room = if muted { 0 } else { 501 };
        assert!(
            peak(&fixture.played()[at]).abs_diff(room) <= 1,
            "muted: {muted}"
        );
    }
}

/// Where the sink's own control takes its whole level the device is given the
/// music untouched, and the monitor hears those same frames.
#[test]
fn a_level_the_sinks_control_takes_leaves_the_monitor_the_devices_frames() {
    let mut fixture = Fixture::with_control(GainRange::new(-6_400, 0, 50).expect("valid"));
    fixture.bind();
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let alice = in_session(7, 0xA1);
    let quieter = AudioRequest::SetLevel {
        device_id: SINK,
        level: level(-1_000),
    };
    fixture.control(&alice, &quieter).expect("the tenant's");
    let mix = started(&mut fixture, &recording(8, 0xA1), monitor_params(4_096));
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    let samples = signal(1_000);
    fixture.client_write(&music, &samples);
    start(&mut fixture, &alice, &music);
    run(&mut fixture, 4);
    let at = primed()..primed() + samples.len();
    assert!(
        fixture.played()[at.clone()] == samples[..],
        "the control took the level"
    );
    assert_eq!(fixture.client_read(&mix)[at], samples[..]);
}

/// A monitor never changes what the device is given: where the mixer takes
/// part of the sink's level the monitor's mix is made by a mixer of its own,
/// so even the device's dither is untouched.
#[test]
fn a_monitor_changes_nothing_the_device_is_given() {
    let played = |monitored: bool| {
        let mut fixture = held_by(0xA1);
        let (alice, also) = (in_session(7, 0xA1), in_session(6, 0xA1));
        let quieter = AudioRequest::SetLevel {
            device_id: SINK,
            level: level(-600),
        };
        fixture.control(&alice, &quieter).expect("the tenant's");
        let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
        let voice = fixture.open_playback(&also, DEVICE_HZ, 4_096);
        fixture.client_write(&music, &signal(2_000));
        fixture.client_write(&voice, &other_signal(2_000));
        start(&mut fixture, &alice, &music);
        start(&mut fixture, &also, &voice);
        run(&mut fixture, 1);
        if monitored {
            let mix = started(&mut fixture, &recording(8, 0xA1), monitor_params(4_096));
            run(&mut fixture, 4);
            assert!(
                !fixture.client_read(&mix).is_empty(),
                "the monitor heard the mix"
            );
        } else {
            run(&mut fixture, 4);
        }
        fixture.played()
    };
    assert_eq!(played(true), played(false));
}

/// A monitor whose reader stalls takes the over-run and the device never
/// waits on it: every frame still plays, and a drain beside it completes with
/// the sink still clocking for the monitor.
#[test]
fn the_output_plays_on_while_a_monitors_reader_stalls() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let mix = started(&mut fixture, &recorder, monitor_params(512));
    assert_eq!(mix.ring_frames, 512, "a ring the start alone fills");
    let alice = in_session(7, 0xA1);
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    let samples = signal(3_000);
    assert_eq!(fixture.client_write(&music, &samples), 3_000);
    start(&mut fixture, &alice, &music);
    drain(&mut fixture, &alice, &music);
    run(&mut fixture, 12);
    let at = primed()..primed() + samples.len();
    assert!(fixture.played()[at] == samples[..], "every frame played");
    assert_eq!(
        fixture.state(&alice, &music),
        StreamState::Idle,
        "its drain completed"
    );
    assert!(
        fixture.device.borrow().audio().running,
        "still clocking for the monitor"
    );
    let state = AudioRequest::State {
        stream_id: mix.stream_id,
    };
    let report = decode_state_reply(&fixture.call(&recorder, &state)).expect("a report");
    assert!(
        report.xrun_frames >= 3_000,
        "what did not fit is counted: {report:?}"
    );
    assert!(
        fixture.client_read(&mix) == alloc::vec![0u8; primed()],
        "the ring kept what fitted"
    );
}

/// A drain beside a live monitor never stops its sink: the drained sound's
/// tail is padded with silence, the monitor is given a period for every
/// period the device takes, unbroken across the sound's end, and the sound is
/// over once the device has taken its last frame. Regression: the drain was
/// passed down to the device, which played out, stopped and was clocked again
/// for the monitor, so a recording's sound fell behind its picture by the
/// gap.
#[test]
fn a_drain_beside_a_monitor_keeps_its_sink_clocking() {
    let mut fixture = held_by(0xA1);
    let mix = started(&mut fixture, &recording(8, 0xA1), monitor_params(4_096));
    let alice = in_session(7, 0xA1);
    let cue = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    let samples = signal(1_000);
    fixture.client_write(&cue, &samples);
    start(&mut fixture, &alice, &cue);
    drain(&mut fixture, &alice, &cue);
    let mut heard = fixture.client_read(&mix);
    for period in 0..6 {
        let given = fixture.played().len();
        fixture.period();
        let taken = fixture.played().len() - given;
        let run = fixture.client_read(&mix);
        assert_eq!(run.len(), taken, "period {period}: one unbroken run");
        assert!(
            !fixture.device.borrow().audio().draining,
            "period {period}: the sink was never drained"
        );
        heard.extend_from_slice(&run);
    }
    assert_eq!(fixture.state(&alice, &cue), StreamState::Idle, "played out");
    let played = fixture.played();
    let at = primed()..primed() + samples.len();
    assert!(played[at.clone()] == samples[..], "the sound");
    assert!(
        played[at.end..].iter().all(|byte| *byte == 0),
        "its tail padded with silence"
    );
    assert!(
        heard.starts_with(&played),
        "every frame the device was given"
    );
}

/// A live monitor counts among the captures the recording indicator is drawn
/// from, beside a microphone, and each close is recorded as what it was.
#[test]
fn a_monitor_counts_among_the_captures_and_its_close_is_recorded() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let audit = Audit::default();
    let mix = listen(&mut fixture, &recorder, monitor_params(2_048), &audit).expect("granted");
    assert_eq!(fixture.service.live_captures(), 0, "opened, not started");
    start(&mut fixture, &recorder, &mix);
    assert_eq!(fixture.service.live_captures(), 1);
    let microphone = open_capture(&mut fixture, &recorder);
    fixture.attach_capture(&recorder, &microphone);
    start(&mut fixture, &recorder, &microphone);
    assert_eq!(
        fixture.service.live_captures(),
        2,
        "the microphone beside it"
    );
    for (grant, closed) in [
        (mix, events::MONITOR_CLOSED),
        (microphone, events::CAPTURE_CLOSED),
    ] {
        let audit = Audit::default();
        let close = AudioRequest::Close {
            stream_id: grant.stream_id,
        };
        decode_status_reply(&fixture.call_logged(&recorder, &close, &audit)).expect("closed");
        assert!(audit.saw(closed), "{closed:?}");
    }
    assert_eq!(fixture.service.live_captures(), 0);
}

/// A monitor follows its session's room: held outside it, it hears none of
/// another session's sound, and an unclaimed room — anybody's to play into —
/// is not its session's either.
#[test]
fn a_monitor_hears_nothing_outside_its_sessions_room() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let mix = started(&mut fixture, &recorder, monitor_params(4_096));
    assert_eq!(fixture.service.live_captures(), 1);
    let _ = fixture.client_read(&mix);
    fixture.seat(DisplayLease::held(2, session(0xB0)));
    assert_eq!(fixture.state(&recorder, &mix), StreamState::SeatInactive);
    assert_eq!(fixture.service.live_captures(), 0, "held, so not recording");
    let bob = in_session(9, 0xB0);
    let music = fixture.open_playback(&bob, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(1_000));
    start(&mut fixture, &bob, &music);
    run(&mut fixture, 4);
    assert!(
        fixture.played().iter().any(|byte| *byte != 0),
        "Bob's room heard him"
    );
    assert!(
        fixture.client_read(&mix).is_empty(),
        "none of Bob's sound reached Alice's monitor"
    );
    fixture.seat(DisplayLease::UNHELD);
    assert_eq!(
        fixture.state(&recorder, &mix),
        StreamState::SeatInactive,
        "an unclaimed room is anybody's, so no session's to monitor"
    );
    fixture.seat(DisplayLease::held(3, session(0xA1)));
    assert_eq!(fixture.state(&recorder, &mix), StreamState::Running);
    assert_eq!(fixture.service.live_captures(), 1);
}

/// A monitor's drain is over at once, and closing it — its sink's last
/// stream — gives the device back: nothing is left programmed, clocking or
/// counted.
#[test]
fn closing_a_monitor_releases_everything_it_held() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let mix = started(&mut fixture, &recorder, monitor_params(2_048));
    run(&mut fixture, 2);
    assert!(
        fixture.device.borrow().audio().running,
        "clocked for the monitor alone"
    );
    let silence = alloc::vec![0u8; bytes(Frames::new(u64::from(PERIOD) * 6))];
    assert!(
        fixture.client_read(&mix) == silence,
        "the silent sink's own timeline"
    );
    drain(&mut fixture, &recorder, &mix);
    assert_eq!(
        fixture.state(&recorder, &mix),
        StreamState::Idle,
        "it owes nothing past its ring"
    );
    assert_eq!(fixture.service.live_captures(), 0);
    fixture.grant(
        &recorder,
        &AudioRequest::Close {
            stream_id: mix.stream_id,
        },
    );
    assert_eq!(fixture.service.stream_count(), 0);
    let device = fixture.device.borrow();
    assert!(
        device.audio().configured.is_none(),
        "the sink was given back"
    );
    assert!(!device.audio().running);
}

/// The monitor's rule over every principal and room: a sink's mix is heard
/// through its own session's room and no other, and a monitor outside it is
/// held, never dropped.
#[test]
fn a_monitor_is_admitted_to_its_own_sessions_room_alone() {
    let (alice, bob) = (session(0xA1), session(0xB0));
    let principals = [
        Principal::Machine,
        Principal::Session(None),
        Principal::Session(Some(alice)),
        Principal::Session(Some(bob)),
    ];
    for room in [
        Room::Unclaimed,
        Room::Session(alice),
        Room::Session(bob),
        Room::Withheld,
    ] {
        for principal in principals {
            let own = [alice, bob].iter().any(|holder| {
                principal == Principal::Session(Some(*holder)) && room == Room::Session(*holder)
            });
            let expected = if own { Admission::Mix } else { Admission::Hold };
            assert_eq!(
                monitor_admission(principal, room),
                expected,
                "{principal:?} in {room:?}"
            );
        }
    }
}

/// A client hearing a source at a rate of its own is resampled from the
/// device's, up or down, and given every frame the filter makes: a microphone
/// heard at twice its device's rate is given about twice its frames, and a
/// sink's mix heard at half the device's rate about half. Regression: the
/// phase bank was built from the device's rate to itself, so a capture was
/// copied through at the device's rate, and its buffers were sized for a
/// playback stream's input, so one at a higher rate lost most of each period.
#[test]
fn a_capture_at_a_rate_of_its_own_is_resampled_from_the_devices() {
    let recorder = recording(8, 0xA1);
    let mut fixture = Fixture::new();
    fixture.device.borrow_mut().audio_mut().fixed_rate = false;
    fixture.bind();
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    let at_half = capture_params(0, StreamRole::Media, DEVICE_HZ / 2, 4_096);
    let at_full = capture_params(0, StreamRole::Media, DEVICE_HZ, 4_096);
    // The first client's rate is the one the device runs at.
    let slow = started(&mut fixture, &recorder, at_half);
    let fast = started(&mut fixture, &recorder, at_full);
    fixture
        .device
        .borrow_mut()
        .audio_mut()
        .captured
        .extend_from_slice(&signal(512));
    fixture.period_on(1);
    assert_eq!(
        fixture.client_read(&slow).len() / 4,
        512,
        "at the device's own rate"
    );
    let up = fixture.client_read(&fast).len() / 4;
    assert!(
        (850..=1_024).contains(&up),
        "512 frames at 24 kHz are some 1024 at 48 kHz, not {up}"
    );

    let mut fixture = held_by(0xA1);
    let mix_at_half = capture_params(monitor_of(SINK), StreamRole::Media, DEVICE_HZ / 2, 4_096);
    let mix = started(&mut fixture, &recorder, mix_at_half);
    run(&mut fixture, 3);
    let down = fixture.client_read(&mix).len() / 4;
    assert!(
        (900..=1_024).contains(&down),
        "2048 frames mixed at 48 kHz are some 1024 at 24 kHz, not {down}"
    );
}

/// Media steps aside for speech on a sink and nowhere else: a recorder beside
/// a call capturing the same microphone hears it as it is. Regression: the
/// ducking rule read every live stream on an endpoint, so a source ducked its
/// media captures twenty decibels under a conversation.
#[test]
fn a_capture_is_never_ducked_under_a_conversation() {
    let mut fixture = held_by(0xA1);
    let recorder = recording(8, 0xA1);
    let media = capture_params(0, StreamRole::Media, DEVICE_HZ, 2_048);
    let speech = capture_params(0, StreamRole::Communication, DEVICE_HZ, 2_048);
    let memo = started(&mut fixture, &recorder, media);
    let call = started(&mut fixture, &recorder, speech);
    let heard = signal(PERIOD as usize);
    fixture
        .device
        .borrow_mut()
        .audio_mut()
        .captured
        .extend_from_slice(&heard);
    fixture.period_on(1);
    assert!(
        fixture.client_read(&memo) == heard,
        "the recorder hears the microphone as it is"
    );
    assert_eq!(fixture.client_read(&call), heard);
}

/// A monitor never chooses how its sink runs. Opened on an idle sink it has
/// the sink run as nobody asked — at its rate nearest 48 kHz, in its widest
/// encoding, with its longest period — and the first stream to play has it
/// configured as that stream alone would, the monitor hearing on.
/// Regression: the monitor's own rate, encoding and latency programmed the
/// sink, and every later stream was converted to them while any stayed open.
#[test]
fn a_monitor_never_chooses_how_its_sink_runs() {
    let music = OpenParams {
        device_id: 0,
        direction: StreamDirection::Playback,
        format: SampleFormat::S24In32,
        rate: rate(44_100),
        channel_map: ChannelMap::STEREO,
        role: StreamRole::Media,
        latency_target_frames: 8_192,
        tenancy: Tenancy::Caller,
    };
    let configured = |monitored: bool| {
        let mut fixture = Fixture::new();
        {
            let mut device = fixture.device.borrow_mut();
            let audio = device.audio_mut();
            audio.fixed_rate = false;
            audio.formats = SampleFormats::EMPTY
                .with(SampleFormat::S16)
                .with(SampleFormat::S24In32)
                .with(SampleFormat::S32);
            audio.periods = (64, 4_096);
        }
        fixture.bind();
        fixture.seat(DisplayLease::held(1, session(0xA1)));
        let recorder = recording(8, 0xA1);
        let quiet = capture_params(monitor_of(SINK), StreamRole::Media, DEVICE_HZ / 2, 512);
        let mix = monitored.then(|| started(&mut fixture, &recorder, quiet));
        if let Some(mix) = &mix {
            let own = ConfigureGrant {
                rate: rate(DEVICE_HZ),
                format: SampleFormat::S32,
                channel_map: ChannelMap::STEREO,
                period_frames: 960,
                max_ring_frames: MAX_RING,
            };
            assert_eq!(
                fixture.device.borrow().audio().configured,
                Some(own),
                "as nobody asked"
            );
            let _ = fixture.client_read(mix);
        }
        let alice = in_session(7, 0xA1);
        let opened = fixture.call(&alice, &AudioRequest::Open(music));
        decode_open_reply(&opened).expect("the music opens");
        if let Some(mix) = &mix {
            run(&mut fixture, 2);
            assert_eq!(fixture.state(&recorder, mix), StreamState::Running);
            assert!(!fixture.client_read(mix).is_empty(), "the monitor hears on");
        }
        let device = fixture.device.borrow();
        device.audio().configured
    };
    assert_eq!(configured(true), configured(false));
}

/// A tap that cannot make its monitors' mix faults the monitors alone: the
/// device plays on, and the music with it. Regression: the tap's failure
/// failed the pump, which lost the device and every stream on it.
#[test]
fn a_tap_that_fails_faults_its_monitors_and_the_device_plays_on() {
    let mut fixture = held_by(0xA1);
    let alice = in_session(7, 0xA1);
    let quieter = AudioRequest::SetLevel {
        device_id: SINK,
        level: level(-600),
    };
    fixture.control(&alice, &quieter).expect("the tenant's");
    let recorder = recording(8, 0xA1);
    let mix = started(&mut fixture, &recorder, monitor_params(4_096));
    let music = fixture.open_playback(&alice, DEVICE_HZ, 4_096);
    fixture.client_write(&music, &signal(2_000));
    start(&mut fixture, &alice, &music);
    fixture.service.drop_tap_for_test(SINK);
    let before = fixture.played().len();
    run(&mut fixture, 4);
    assert_eq!(fixture.state(&recorder, &mix), StreamState::Faulted);
    assert_eq!(fixture.state(&alice, &music), StreamState::Running);
    assert!(
        fixture.played().len() > before + bytes(Frames::new(u64::from(PERIOD) * 4)),
        "the device plays on"
    );
}

/// The fixture's device bound and its seat held by the session tagged
/// `0xA1`, its sink taking any rate up to 48 kHz in `s16` or `s32` at periods
/// of `periods`, and granting the period `fixed` whatever it is asked where
/// one is given.
fn sink_with_periods(periods: (u32, u32), fixed: Option<u32>) -> Fixture {
    let mut fixture = Fixture::new();
    {
        let mut device = fixture.device.borrow_mut();
        let audio = device.audio_mut();
        audio.fixed_rate = false;
        audio.formats = SampleFormats::EMPTY
            .with(SampleFormat::S16)
            .with(SampleFormat::S32);
        audio.periods = periods;
        audio.fixed_period = fixed;
    }
    fixture.bind();
    fixture.seat(DisplayLease::held(1, session(0xA1)));
    fixture
}

/// A stream at 768 kHz in `s16`, a rate no sink here runs: the periods it is
/// mixed in are sixteen times as long at its own rate.
fn racing() -> OpenParams {
    OpenParams {
        device_id: 0,
        direction: StreamDirection::Playback,
        format: SampleFormat::S16,
        rate: rate(768_000),
        channel_map: ChannelMap::STEREO,
        role: StreamRole::Media,
        latency_target_frames: 2_048,
        tenancy: Tenancy::Caller,
    }
}

/// An open its record would refuse is refused before its sink is touched: a
/// sink only a monitor rides runs on as it did, its monitor undisturbed.
/// Regression: the sink was configured afresh as the stream asked before its
/// ring was sized, no ring could hold two of its periods, and the sink was
/// left in the refused stream's configuration, its monitor rebuilt for
/// nothing.
#[test]
fn an_open_its_record_would_refuse_leaves_the_sink_as_it_ran() {
    let mut fixture = sink_with_periods((4_096, 4_096), None);
    let recorder = recording(8, 0xA1);
    let mix = started(&mut fixture, &recorder, monitor_params(2_048));
    let before = fixture.device.borrow().audio().configured;
    let asked = fixture.device.borrow().audio().configurations;
    let refused = fixture.call(&in_session(7, 0xA1), &AudioRequest::Open(racing()));
    assert_eq!(decode_open_reply(&refused), Err(Errno::OutOfRange));
    {
        let device = fixture.device.borrow();
        assert_eq!(device.audio().configured, before, "as it ran");
        assert_eq!(
            device.audio().configurations,
            asked,
            "nothing was asked of it"
        );
    }
    run(&mut fixture, 2);
    assert_eq!(fixture.state(&recorder, &mix), StreamState::Running);
    assert!(
        !fixture.client_read(&mix).is_empty(),
        "the monitor hears on"
    );
}

/// An open its device grants other than it asked, so that its ring cannot be
/// shaped, is undone once the sink was configured for it: a sink only a
/// monitor rides is configured again as it ran, its monitor hearing on, and
/// one nothing rides is given back. Regression: the sink a monitor rode was
/// left in the refused stream's configuration.
#[test]
fn an_open_refused_once_its_sink_was_configured_is_undone() {
    for monitored in [true, false] {
        let mut fixture = sink_with_periods((64, 4_096), Some(4_096));
        let recorder = recording(8, 0xA1);
        let mix = monitored.then(|| started(&mut fixture, &recorder, monitor_params(2_048)));
        let before = fixture.device.borrow().audio().configured;
        let refused = fixture.call(&in_session(7, 0xA1), &AudioRequest::Open(racing()));
        assert_eq!(
            decode_open_reply(&refused),
            Err(Errno::OutOfRange),
            "monitored: {monitored}"
        );
        assert_eq!(
            fixture.device.borrow().audio().configured,
            before,
            "monitored: {monitored}"
        );
        if let Some(mix) = mix {
            run(&mut fixture, 2);
            assert_eq!(fixture.state(&recorder, &mix), StreamState::Running);
            assert!(
                !fixture.client_read(&mix).is_empty(),
                "the monitor hears on"
            );
        }
    }
}

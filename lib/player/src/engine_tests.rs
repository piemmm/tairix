use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::num::NonZeroU32;

use tairix_abi::audio::{
    AudioGain, AudioNotify, OpenParams, StreamGrant, StreamReport, StreamRole, StreamState,
};
use tairix_abi::driver::audio::{ChannelMap, Frames, SampleFormat};
use tairix_abi::time::Duration64;
use tairix_abi::Errno;
use tairix_audio::stream::{OpenFailure, Written};
use tairix_hash::HashSeed;
use tairix_log::DiscardSink;
use tairix_sandbox::audiodecode::AudioDecodeService;
use tairix_sandbox::loopback::LoopbackSessionLauncher;
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_test_audio_wire as wire;

use super::{
    Control, Engine, Failure, FileRefusal, Files, Note, Outcome, Settings, Skip, Speaker, Transport,
};
use crate::programme::{Advance, EntryId, Extent, List, Passes, Programme};
use crate::Span;

const PERIOD: usize = 480;
const STEP_NS: u64 = 10_000_000;

#[allow(
    clippy::unnecessary_wraps,
    reason = "the engine's seed source may have no seed to give; this one always has"
)]
fn seed() -> Option<HashSeed> {
    Some(HashSeed::from_bytes([7; HashSeed::LEN]))
}

/// A 16-bit PCM WAV of `frames` frames whose samples spell each frame's own
/// index, so where a frame came from can be read back from what played.
fn counting_wav(rate: u32, channels: u16, frames: u32) -> Vec<u8> {
    counting_wav_from(rate, channels, 0, frames)
}

/// A counting WAV whose first frame spells `first`, so files of one shape can
/// be told apart in what played.
fn counting_wav_from(rate: u32, channels: u16, first: u32, frames: u32) -> Vec<u8> {
    let data = frames * u32::from(channels) * 2;
    let mut file = wire::wav_header(rate, channels, data).to_vec();
    for frame in first..first + frames {
        for channel in 0..channels {
            let word = if channel == 0 {
                frame & 0x7FFF
            } else {
                frame >> 15
            };
            file.extend_from_slice(&(u16::try_from(word).unwrap_or(0)).to_le_bytes());
        }
    }
    file
}

/// The frame index a counting stereo frame spells.
fn spelled(frame: [u8; 4]) -> u32 {
    let low = u32::from(u16::from_le_bytes([frame[0], frame[1]]));
    let high = u32::from(u16::from_le_bytes([frame[2], frame[3]]));
    low | high << 15
}

/// The frame index each counting stereo frame of `bytes` spells.
fn spelled_frames(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(spelled)
        .collect()
}

fn signal_wav() -> Vec<u8> {
    let mut file = vec![0u8; wire::SIGNAL_WAV_LEN];
    assert_eq!(wire::write_signal_wav(&mut file), wire::SIGNAL_WAV_LEN);
    file
}

fn signal_flac() -> Vec<u8> {
    wire::signal_flac().expect("the signal encodes")
}

fn pcm_of(file: &[u8]) -> &[u8] {
    &file[wire::WAV_HEADER_LEN..]
}

/// Files held in memory, keyed by path.
struct MemFiles {
    files: BTreeMap<String, Vec<u8>>,
    open: Option<Vec<u8>>,
    /// Reads reaching past this offset fail, as a dying disk's do.
    fails_from: Option<u64>,
}

impl MemFiles {
    fn of(files: &[(&str, Vec<u8>)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), bytes.clone()))
                .collect(),
            open: None,
            fails_from: None,
        }
    }
}

impl Files<str> for MemFiles {
    fn open(&mut self, path: &str) -> Result<u64, FileRefusal> {
        let bytes = self
            .files
            .get(path)
            .ok_or(FileRefusal::Os(Errno::NotFound))?;
        self.open = Some(bytes.clone());
        Ok(bytes.len() as u64)
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let file = self.open.as_ref().ok_or(Errno::NotFound)?;
        if self
            .fails_from
            .is_some_and(|from| offset + buf.len() as u64 > from)
        {
            return Err(Errno::DeviceFault);
        }
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let held = file.get(start..).unwrap_or(&[]);
        let read = held.len().min(buf.len());
        buf[..read].copy_from_slice(&held[..read]);
        Ok(read)
    }
}

/// One stream the fake service opened, and everything done to it.
#[derive(Default)]
struct Played {
    params: Option<OpenParams>,
    /// Frames the device took from the ring, in order.
    heard: Vec<u8>,
    starts: Vec<u64>,
    pauses: usize,
    flushes: usize,
    gains: Vec<AudioGain>,
    drained: bool,
    closed: bool,
}

/// The audio service and its device, played a period at a time.
struct Service {
    streams: Vec<Played>,
    ring_frames: u32,
    frame_bytes: usize,
    queued: VecDeque<u8>,
    position: u64,
    state: StreamState,
    notices: VecDeque<AudioNotify>,
    refuse_open: Option<OpenFailure>,
    /// The current stream's underruns, as the service counts them.
    xruns: u32,
    xrun_frames: u64,
    /// The stream's mailbox holds a frame that is not a notification.
    unreadable: bool,
}

impl Service {
    fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            streams: Vec::new(),
            ring_frames: 0,
            frame_bytes: 0,
            queued: VecDeque::new(),
            position: 0,
            state: StreamState::Idle,
            notices: VecDeque::new(),
            refuse_open: None,
            xruns: 0,
            xrun_frames: 0,
            unreadable: false,
        }))
    }

    fn id(&self) -> u64 {
        self.streams.len() as u64
    }

    fn current(&mut self) -> &mut Played {
        self.streams.last_mut().expect("a stream is open")
    }

    /// One device period: take what the ring holds, as a running device does.
    fn period(&mut self) {
        if !matches!(self.state, StreamState::Running | StreamState::Draining) {
            return;
        }
        let take = (PERIOD * self.frame_bytes).min(self.queued.len());
        let frames: Vec<u8> = self.queued.drain(..take).collect();
        self.position += (take / self.frame_bytes.max(1)) as u64;
        self.current().heard.extend_from_slice(&frames);
        let id = self.id();
        self.notices.push_back(AudioNotify::SpaceAvailable {
            stream_id: id,
            position: Frames::new(self.position),
        });
        if self.state == StreamState::Draining && self.queued.is_empty() {
            self.state = StreamState::Idle;
            self.notices.push_back(AudioNotify::StateChanged {
                stream_id: id,
                state: StreamState::Idle,
                at: Frames::new(self.position),
            });
        }
    }
}

struct FakeSpeaker(Rc<RefCell<Service>>);

impl Speaker for FakeSpeaker {
    fn open(&mut self, params: &OpenParams) -> Result<StreamGrant, OpenFailure> {
        let mut service = self.0.borrow_mut();
        if let Some(failure) = service.refuse_open {
            return Err(failure);
        }
        service.streams.push(Played {
            params: Some(*params),
            ..Played::default()
        });
        service.ring_frames = params.latency_target_frames;
        service.frame_bytes =
            params.format.bytes_per_sample() * usize::from(params.channel_map.channels());
        service.queued.clear();
        service.position = 0;
        service.state = StreamState::Idle;
        service.xruns = 0;
        service.xrun_frames = 0;
        Ok(StreamGrant {
            stream_id: service.id(),
            notify_endpoint: 1,
            rate: params.rate,
            format: params.format,
            channel_map: params.channel_map,
            ring_frames: params.latency_target_frames,
            granted_latency_frames: params.latency_target_frames,
            granted_latency: Duration64::new(0, 0).expect("zero"),
            clock_domain: 1,
        })
    }

    fn write(&mut self, at: Frames, samples: &[u8]) -> Result<Written, Errno> {
        let mut service = self.0.borrow_mut();
        let frame_bytes = service.frame_bytes;
        let producer = service.position + (service.queued.len() / frame_bytes) as u64;
        if at.get() != producer {
            return Err(Errno::OutOfRange);
        }
        let room = service.ring_frames as usize * frame_bytes - service.queued.len();
        let take = room.min(samples.len()) / frame_bytes * frame_bytes;
        service.queued.extend(&samples[..take]);
        Ok(Written {
            silence_frames: 0,
            sample_frames: u32::try_from(take / frame_bytes).unwrap_or(0),
        })
    }

    fn start(&mut self, at: Frames) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        if at.get() < service.position {
            return Err(Errno::OutOfRange);
        }
        service.state = StreamState::Running;
        service.current().starts.push(at.get());
        Ok(())
    }

    fn pause(&mut self) -> Result<Frames, Errno> {
        let mut service = self.0.borrow_mut();
        service.state = StreamState::Paused;
        service.current().pauses += 1;
        Ok(Frames::new(service.position))
    }

    fn drain(&mut self) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        service.state = StreamState::Draining;
        service.current().drained = true;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        let frames = (service.queued.len() / service.frame_bytes) as u64;
        service.queued.clear();
        service.position += frames;
        service.current().flushes += 1;
        Ok(())
    }

    fn set_gain(&mut self, gain: AudioGain) -> Result<(), Errno> {
        self.0.borrow_mut().current().gains.push(gain);
        Ok(())
    }

    fn close(&mut self) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        service.current().closed = true;
        service.state = StreamState::Idle;
        Ok(())
    }

    fn take_notify(&mut self) -> Result<Option<AudioNotify>, Errno> {
        let mut service = self.0.borrow_mut();
        if service.unreadable {
            return Err(Errno::BadMagic);
        }
        Ok(service.notices.pop_front())
    }

    fn report(&mut self) -> Result<StreamReport, Errno> {
        let service = self.0.borrow();
        Ok(StreamReport {
            state: service.state,
            changed_at: Frames::new(service.position),
            xruns: service.xruns,
            xrun_frames: service.xrun_frames,
        })
    }
}

type TestEngine<P, F> = Engine<P, LoopbackSessionLauncher<F>, DiscardSink, MemFiles, FakeSpeaker>;

/// An engine and the fake service it plays into.
type Harness<P, F> = (TestEngine<P, F>, Rc<RefCell<Service>>);

type RealDecoder = fn() -> AudioDecodeService;

const FIRST: EntryId = EntryId::new(0);

/// What a test playback is asked: its list, and how it is heard.
struct Ask {
    files: Vec<String>,
    extent: Extent,
    passes: Passes,
    settings: Settings,
}

fn ask(files: &[&str]) -> Ask {
    Ask {
        files: files.iter().map(|path| (*path).to_string()).collect(),
        extent: Extent::default(),
        passes: Passes::ONCE,
        settings: Settings::default(),
    }
}

impl Ask {
    fn list(self) -> (List, Settings) {
        (
            List::new(self.files, self.extent, self.passes),
            self.settings,
        )
    }
}

fn engine(ask: Ask, files: &[(&str, Vec<u8>)]) -> Harness<List, RealDecoder> {
    engine_over(ask, files, AudioDecodeService::new as RealDecoder)
}

fn engine_over<F: FnMut() -> S, S: SessionService>(
    ask: Ask,
    files: &[(&str, Vec<u8>)],
    factory: F,
) -> Harness<List, F> {
    engine_on(ask, MemFiles::of(files), factory)
}

fn engine_on<F: FnMut() -> S, S: SessionService>(
    ask: Ask,
    files: MemFiles,
    factory: F,
) -> Harness<List, F> {
    let (list, settings) = ask.list();
    engine_playing(list, settings, files, factory)
}

fn engine_playing<P: Programme<Item = str>, F: FnMut() -> S, S: SessionService>(
    programme: P,
    settings: Settings,
    files: MemFiles,
    factory: F,
) -> Harness<P, F> {
    let service = Service::new();
    let engine = Engine::new(
        programme,
        settings,
        LoopbackSessionLauncher::new(factory),
        DiscardSink,
        files,
        FakeSpeaker(Rc::clone(&service)),
        seed,
    )
    .expect("an engine");
    (engine, service)
}

/// Run the engine and the device a period at a time until `done` holds or
/// playback ends, answering the instant it stopped at.
fn run<P: Programme<Item = str>, F: FnMut() -> S, S: SessionService>(
    engine: &mut TestEngine<P, F>,
    service: &Rc<RefCell<Service>>,
    mut now: u64,
    mut done: impl FnMut(&TestEngine<P, F>, &Service) -> bool,
) -> u64 {
    for _ in 0..200_000 {
        if engine.outcome().is_some() || done(engine, &service.borrow()) {
            return now;
        }
        engine.on_decoder(now, true, true);
        service.borrow_mut().period();
        engine.on_notify(now);
        now += STEP_NS;
        engine.on_timer(now);
    }
    panic!("playback never settled");
}

fn play_out<P: Programme<Item = str>, F: FnMut() -> S, S: SessionService>(
    engine: &mut TestEngine<P, F>,
    service: &Rc<RefCell<Service>>,
) -> Outcome {
    engine.begin(0);
    run(engine, service, 0, |_, _| false);
    engine.outcome().expect("it ended")
}

fn heard(service: &Rc<RefCell<Service>>) -> Vec<u8> {
    service
        .borrow()
        .streams
        .iter()
        .flat_map(|stream| stream.heard.iter().copied())
        .collect()
}

#[test]
fn a_file_is_heard_whole_and_exact_on_the_default_sink() {
    let file = signal_wav();
    let (mut engine, service) = engine(ask(&["signal.wav"]), &[("signal.wav", file.clone())]);
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm_of(&file));
    let service = service.borrow();
    assert_eq!(service.streams.len(), 1);
    let params = service.streams[0].params.expect("opened");
    assert_eq!(params.device_id, 0);
    assert_eq!(params.format, SampleFormat::S16);
    assert_eq!(params.channel_map, ChannelMap::STEREO);
    assert_eq!(params.rate.hz(), wire::RATE_HZ);
    assert_eq!(params.role, StreamRole::Media);
    assert!(service.streams[0].drained && service.streams[0].closed);
    assert_eq!(engine.status().heard_frames, wire::STREAM_FRAMES as u64);
    assert!(matches!(
        engine.take_notes()[..],
        [Note::Opened { entry: FIRST, .. }]
    ));
}

#[test]
fn files_of_one_shape_follow_each_other_into_one_stream() {
    let file = signal_wav();
    let (mut engine, service) = engine(
        ask(&["a.wav", "b.wav"]),
        &[("a.wav", file.clone()), ("b.wav", file.clone())],
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(
        service.borrow().streams.len(),
        1,
        "no gap between the files"
    );
    let mut twice = pcm_of(&file).to_vec();
    twice.extend_from_slice(pcm_of(&file));
    assert_eq!(heard(&service), twice);
}

/// A FLAC file, decoded a frame at a time, is heard as exactly the signal,
/// and follows a WAV file of its shape into the one stream.
#[test]
fn a_flac_file_is_heard_exact_and_gapless_after_a_wav_one() {
    let wav = signal_wav();
    let (mut engine, service) = engine(
        ask(&["a.wav", "b.flac"]),
        &[("a.wav", wav.clone()), ("b.flac", signal_flac())],
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(
        service.borrow().streams.len(),
        1,
        "no gap between the files"
    );
    let mut twice = pcm_of(&wav).to_vec();
    twice.extend_from_slice(pcm_of(&wav));
    assert_eq!(heard(&service), twice);
    assert_eq!(engine.status().heard_frames, 2 * wire::STREAM_FRAMES as u64);
}

/// Every frame the first stream queued is heard before the second opens.
#[test]
fn a_file_of_another_shape_waits_for_the_stream_before_it_to_play_out() {
    let first = signal_wav();
    let second = counting_wav(8_000, 1, 3_000);
    let (mut engine, service) = engine(
        ask(&["a.wav", "b.wav"]),
        &[("a.wav", first.clone()), ("b.wav", second.clone())],
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    let service = service.borrow();
    assert_eq!(service.streams.len(), 2);
    assert!(service.streams[0].drained && service.streams[0].closed);
    assert_eq!(service.streams[0].heard, pcm_of(&first));
    assert_eq!(service.streams[1].heard, pcm_of(&second));
    assert_eq!(service.streams[1].params.expect("opened").rate.hz(), 8_000);
}

#[test]
fn a_start_and_a_duration_bound_what_is_heard_to_the_frame() {
    let file = counting_wav(8_000, 2, 16_000);
    let bounded = Ask {
        extent: Extent {
            start: Span::from_nanos(250_000_000),
            duration: Some(Span::from_nanos(500_000_000)),
        },
        ..ask(&["x.wav"])
    };
    let (mut engine, service) = engine(bounded, &[("x.wav", file)]);
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(
        spelled_frames(&heard(&service)),
        (2_000..6_000).collect::<Vec<u32>>()
    );
}

#[test]
fn the_list_plays_as_many_passes_as_asked() {
    let file = counting_wav(8_000, 2, 1_000);
    let thrice = Ask {
        passes: Passes::Times(NonZeroU32::new(3).expect("nonzero")),
        ..ask(&["x.wav"])
    };
    let (mut engine, service) = engine(thrice, &[("x.wav", file)]);
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    let once: Vec<u32> = (0..1_000).collect();
    assert_eq!(
        spelled_frames(&heard(&service)),
        [once.clone(), once.clone(), once].concat()
    );
    assert_eq!(
        service.borrow().streams.len(),
        1,
        "a pass follows the last gapless"
    );
    assert_eq!(
        engine.status().pass,
        2,
        "the last pass heard, counted from zero"
    );
}

/// The decoder runs ahead into the next pass before the last of this one is
/// heard; a seek back in what is heard returns to the pass it is heard in, so
/// the next pass is still played whole.
#[test]
fn a_seek_back_across_a_pass_boundary_keeps_every_pass() {
    let frames = 8_000 * 30;
    let file = counting_wav(8_000, 2, frames);
    let twice = Ask {
        passes: Passes::Times(NonZeroU32::new(2).expect("nonzero")),
        ..ask(&["x.wav"])
    };
    let (mut engine, service) = engine(twice, &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().pass == 0 && engine.status().position >= u64::from(frames) - 2_000
    });
    let before = service.borrow().streams[0].heard.len();
    let heard_at = u32::try_from(engine.status().position).expect("small");
    engine.on_control(now, Control::Back);
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    let after = spelled_frames(&service.borrow().streams[0].heard[before..]);
    let replayed: Vec<u32> = (heard_at - 80_000..frames).collect();
    let second: Vec<u32> = (0..frames).collect();
    assert_eq!(after, [replayed, second].concat());
}

#[test]
fn a_file_that_cannot_be_opened_is_left_out_and_the_rest_plays() {
    let file = signal_wav();
    let (mut engine, service) = engine(
        ask(&["missing.wav", "signal.wav"]),
        &[("signal.wav", file.clone())],
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm_of(&file));
    let notes = engine.take_notes();
    assert_eq!(
        notes[0],
        Note::Skipped {
            entry: FIRST,
            why: Skip::Open(FileRefusal::Os(Errno::NotFound))
        }
    );
}

#[test]
fn a_file_no_decoder_reads_is_left_out_with_the_decoders_reason() {
    let file = signal_wav();
    let (mut engine, service) = engine(
        ask(&["notes.txt", "signal.wav"]),
        &[
            ("notes.txt", b"not a sound at all, only words".to_vec()),
            ("signal.wav", file.clone()),
        ],
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm_of(&file));
    assert!(matches!(
        engine.take_notes()[0],
        Note::Skipped {
            entry: FIRST,
            why: Skip::Refused(_)
        }
    ));
}

#[test]
fn a_list_of_which_nothing_plays_is_a_failure_and_not_a_loop() {
    let forever = Ask {
        passes: Passes::Forever,
        ..ask(&["a.wav", "b.wav"])
    };
    let (mut engine, service) = engine(forever, &[]);
    assert_eq!(
        play_out(&mut engine, &service),
        Outcome::Failed(Failure::NothingPlayable)
    );
    assert!(service.borrow().streams.is_empty());
}

#[test]
fn a_pause_holds_the_frame_and_play_goes_on_from_it() {
    let file = counting_wav(8_000, 2, 12_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file.clone())]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service
            .streams
            .first()
            .is_some_and(|s| s.heard.len() >= 4 * 2_000)
    });
    engine.on_control(now, Control::TogglePause);
    let paused_at = service.borrow().position;
    let now = run(&mut engine, &service, now, |_, _| true);
    for _ in 0..50 {
        service.borrow_mut().period();
    }
    assert_eq!(
        service.borrow().position,
        paused_at,
        "nothing plays while paused"
    );
    engine.on_control(now, Control::TogglePause);
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(heard(&service), pcm_of(&file), "every frame once, in order");
    let service = service.borrow();
    assert_eq!(service.streams[0].pauses, 1);
    assert_eq!(service.streams[0].starts, vec![0, paused_at]);
}

#[test]
fn a_seek_discards_what_is_queued_and_plays_on_ten_seconds_later() {
    let file = counting_wav(8_000, 2, 8_000 * 30);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().position >= 8_000
    });
    let before = service.borrow().streams[0].heard.len();
    let heard_at = engine.status().position;
    engine.on_control(now, Control::Forward);
    run(&mut engine, &service, now, |_, service| {
        service.streams[0].heard.len() >= before + 4 * 4
    });
    let service = service.borrow();
    let after = &service.streams[0].heard[before..];
    assert_eq!(
        spelled_frames(after).first().copied(),
        Some(u32::try_from(heard_at).expect("small") + 80_000)
    );
    assert_eq!(service.streams[0].flushes, 1);
}

#[test]
fn the_level_steps_three_decibels_and_never_past_unity() {
    let file = counting_wav(8_000, 2, 40_000);
    let quiet = Ask {
        settings: Settings {
            gain: AudioGain::new(-400).expect("attenuation"),
            ..Settings::default()
        },
        ..ask(&["x.wav"])
    };
    let (mut engine, service) = engine(quiet, &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        !service.streams.is_empty()
    });
    for control in [
        Control::Quieter,
        Control::Louder,
        Control::Louder,
        Control::Louder,
    ] {
        engine.on_control(now, control);
    }
    let gains: Vec<i32> = service.borrow().streams[0]
        .gains
        .iter()
        .map(|gain| gain.millibel())
        .collect();
    assert_eq!(gains, vec![-400, -700, -400, -100, 0]);
    assert_eq!(engine.status().gain, AudioGain::UNITY);
}

#[test]
fn a_lost_device_ends_playback_with_the_reason() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    service
        .borrow_mut()
        .notices
        .push_back(AudioNotify::StateChanged {
            stream_id: 1,
            state: StreamState::DeviceLost,
            at: Frames::new(10),
        });
    engine.on_notify(now);
    assert_eq!(engine.outcome(), Some(Outcome::Failed(Failure::DeviceLost)));
    assert!(service.borrow().streams[0].closed);
}

#[test]
fn a_faulted_ring_ends_playback_with_the_reason() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    service
        .borrow_mut()
        .notices
        .push_back(AudioNotify::StateChanged {
            stream_id: 1,
            state: StreamState::Faulted,
            at: Frames::new(10),
        });
    engine.on_notify(now);
    assert_eq!(
        engine.outcome(),
        Some(Outcome::Failed(Failure::RingFaulted))
    );
    assert!(service.borrow().streams[0].closed);
}

/// A stream held while it played out its tail is released draining, not
/// running, and is playing again all the same.
#[test]
fn a_stream_released_from_the_seat_mid_drain_is_playing() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    for (state, transport) in [
        (StreamState::SeatInactive, Transport::Held),
        (StreamState::Draining, Transport::Playing),
    ] {
        service
            .borrow_mut()
            .notices
            .push_back(AudioNotify::StateChanged {
                stream_id: 1,
                state,
                at: Frames::new(10),
            });
        engine.on_notify(now);
        assert_eq!(engine.status().transport, transport, "{state:?}");
    }
}

/// Paused by the user and then held by the seat, the player is paused again
/// — not playing — when the room comes back; resumed while held, it waits.
#[test]
fn a_paused_player_the_seat_holds_is_paused_again_when_the_room_returns() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    engine.on_control(now, Control::TogglePause);
    assert_eq!(engine.status().transport, Transport::Paused);
    let tell = |state| AudioNotify::StateChanged {
        stream_id: 1,
        state,
        at: Frames::new(10),
    };
    service
        .borrow_mut()
        .notices
        .push_back(tell(StreamState::SeatInactive));
    engine.on_notify(now);
    assert_eq!(engine.status().transport, Transport::Held);
    service
        .borrow_mut()
        .notices
        .push_back(tell(StreamState::Paused));
    engine.on_notify(now);
    assert_eq!(engine.status().transport, Transport::Paused);

    service
        .borrow_mut()
        .notices
        .push_back(tell(StreamState::SeatInactive));
    engine.on_notify(now);
    engine.on_control(now, Control::TogglePause);
    assert_eq!(
        engine.status().transport,
        Transport::Held,
        "played on outside the room, it waits for the room"
    );
}

/// The service's notifications are best effort; its own count is not.
#[test]
fn the_underrun_totals_are_the_services_own_count_whichever_notifications_arrived() {
    let file = counting_wav(8_000, 1, 3_000);
    let (mut engine, service) = engine(
        ask(&["a.wav", "b.wav"]),
        &[("a.wav", signal_wav()), ("b.wav", file)],
    );
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    {
        let mut service = service.borrow_mut();
        service.xruns = 2;
        service.xrun_frames = 300;
        service.notices.push_back(AudioNotify::Xrun {
            stream_id: 1,
            at: Frames::new(10),
            lost_frames: 100,
        });
    }
    engine.on_notify(now);
    assert_eq!(
        (engine.status().underruns, engine.status().lost_frames),
        (1, 100)
    );
    run(&mut engine, &service, now, |_, service| {
        service.streams.len() == 2
    });
    assert_eq!(
        (engine.status().underruns, engine.status().lost_frames),
        (2, 300),
        "the dropped notification is counted once the stream closes"
    );
    service.borrow_mut().xruns = 1;
    service.borrow_mut().xrun_frames = 40;
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(
        (engine.status().underruns, engine.status().lost_frames),
        (3, 340),
        "each stream's own count is added to those before it"
    );
}

/// A mailbox that cannot be read leaves the stream's end unknowable, so the
/// playback ends with that reason rather than waiting for ever.
#[test]
fn a_mailbox_that_cannot_be_read_ends_playback_with_the_reason() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    service.borrow_mut().unreadable = true;
    engine.on_notify(now);
    assert_eq!(
        engine.outcome(),
        Some(Outcome::Failed(Failure::Wait {
            what: "read the stream's notifications",
            errno: Errno::BadMagic,
        }))
    );
    assert!(service.borrow().streams[0].closed);
}

#[test]
fn a_wait_the_host_cannot_make_ends_playback_once_and_keeps_an_earlier_end() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    let lost = Failure::Wait {
        what: "watch the decoder",
        errno: Errno::NotFound,
    };
    engine.abandon(lost);
    assert_eq!(engine.outcome(), Some(Outcome::Failed(lost)));
    assert!(service.borrow().streams[0].closed);
    engine.abandon(Failure::OutOfMemory);
    assert_eq!(
        engine.outcome(),
        Some(Outcome::Failed(lost)),
        "the first reason stands"
    );
}

#[test]
fn a_stream_the_service_refuses_ends_playback_with_its_refusal() {
    let (mut engine, service) = engine(ask(&["s.wav"]), &[("s.wav", signal_wav())]);
    service.borrow_mut().refuse_open = Some(OpenFailure::Refused(Errno::NotFound));
    assert_eq!(
        play_out(&mut engine, &service),
        Outcome::Failed(Failure::Stream(OpenFailure::Refused(Errno::NotFound)))
    );
}

#[test]
fn stopping_gives_the_stream_back() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|s| !s.heard.is_empty())
    });
    engine.on_control(now, Control::Stop);
    assert_eq!(engine.outcome(), Some(Outcome::Stopped));
    assert!(service.borrow().streams[0].closed);
}

/// Serves as the decoder does until its first worker's `fail_at`th request,
/// where that worker dies.
struct DiesOnce {
    inner: AudioDecodeService,
    left: Option<usize>,
}

impl SessionService for DiesOnce {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        if let Some(left) = &mut self.left {
            if *left == 0 {
                return SessionStep::Finished;
            }
            *left -= 1;
        }
        self.inner.handle(request, out)
    }
}

#[test]
fn a_decoder_that_dies_part_way_is_replaced_and_the_file_still_plays_exact() {
    let file = counting_wav(8_000, 2, 60_000);
    let launches = Rc::new(Cell::new(0));
    let counted = Rc::clone(&launches);
    let (mut engine, service) =
        engine_over(ask(&["x.wav"]), &[("x.wav", file.clone())], move || {
            counted.set(counted.get() + 1);
            DiesOnce {
                inner: AudioDecodeService::new(),
                left: (counted.get() == 1).then_some(12),
            }
        });
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(launches.get(), 2, "one replacement");
    assert_eq!(heard(&service), pcm_of(&file));
}

/// What a seek discards was never heard, though the service counts it read.
#[test]
fn frames_a_seek_discards_are_not_counted_as_heard() {
    let file = counting_wav(8_000, 2, 8_000 * 20);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file)]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().position >= 8_000
    });
    engine.on_control(now, Control::Forward);
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    let device = (heard(&service).len() / 4) as u64;
    assert_eq!(engine.status().heard_frames, device);
}

/// A read that fails part-way through a file is stated once, as the read's
/// own failure, and what was read before it still plays.
#[test]
fn a_file_whose_reads_fail_part_way_is_cut_once_with_the_reason() {
    let file = counting_wav(8_000, 2, 8_000 * 60);
    let mut files = MemFiles::of(&[("x.wav", file)]);
    files.fails_from = Some(600_000);
    let (mut engine, service) = engine_on(
        ask(&["x.wav"]),
        files,
        AudioDecodeService::new as fn() -> AudioDecodeService,
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    let notes = engine.take_notes();
    assert!(
        matches!(notes[0], Note::Opened { entry: FIRST, .. }),
        "{notes:?}"
    );
    assert_eq!(
        notes[1..],
        [Note::Cut {
            entry: FIRST,
            why: Skip::Unreadable(Errno::DeviceFault)
        }]
    );
    let heard = spelled_frames(&heard(&service));
    assert!(!heard.is_empty());
    assert!(heard
        .iter()
        .copied()
        .eq(0..u32::try_from(heard.len()).expect("small")));
}

/// Two presses of "next" before the first is heard move two entries on: the
/// listener's place is where the last command left it, not the last frame the
/// service reported.
#[test]
fn rapid_skips_each_move_one_entry_on() {
    let names = ["a.wav", "b.wav", "c.wav", "d.wav"];
    let files: Vec<(&str, Vec<u8>)> = names
        .iter()
        .map(|&name| (name, counting_wav(8_000, 2, 16_000)))
        .collect();
    let (mut engine, service) = engine(ask(&names), &files);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().heard.is_some()
    });
    engine.on_control(now, Control::Next);
    engine.on_control(now, Control::Next);
    run(&mut engine, &service, now, |engine, _| {
        engine
            .status()
            .heard
            .is_some_and(|(entry, _)| entry != FIRST)
    });
    assert_eq!(
        engine.status().heard.map(|(entry, _)| entry),
        Some(EntryId::new(2)),
        "two presses, two entries on"
    );
}

/// However many entries in a row cannot be opened, they are walked past in
/// one loop, not one call deep for each.
#[test]
fn a_long_run_of_entries_that_cannot_be_opened_is_walked_not_recursed() {
    let mut names = vec!["missing.wav".to_string(); 100_000];
    names.push("signal.wav".to_string());
    let file = signal_wav();
    let (mut engine, service) = engine_playing(
        List::new(names, Extent::default(), Passes::ONCE),
        Settings::default(),
        MemFiles::of(&[("signal.wav", file.clone())]),
        AudioDecodeService::new as RealDecoder,
    );
    assert_eq!(play_out(&mut engine, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm_of(&file));
    assert_eq!(engine.take_notes().len(), 100_001);
}

/// A programme a test edits while it plays.
struct Shelf {
    order: Vec<EntryId>,
    paths: BTreeMap<EntryId, String>,
    /// What followed each entry an edit removed, until the engine settles.
    removed: BTreeMap<EntryId, Option<EntryId>>,
}

impl Shelf {
    fn of(names: &[&str]) -> Self {
        let order: Vec<EntryId> = (0..names.len() as u64).map(EntryId::new).collect();
        let paths = order
            .iter()
            .zip(names)
            .map(|(&entry, name)| (entry, (*name).to_string()))
            .collect();
        Self {
            order,
            paths,
            removed: BTreeMap::new(),
        }
    }

    fn at(&self, entry: EntryId) -> Option<usize> {
        self.order.iter().position(|&held| held == entry)
    }

    fn remove(&mut self, entry: EntryId) {
        if let Some(at) = self.at(entry) {
            self.removed.insert(entry, self.order.get(at + 1).copied());
            self.order.remove(at);
        }
        self.paths.remove(&entry);
    }

    fn push(&mut self, name: &str) -> EntryId {
        let entry = EntryId::new(self.paths.len() as u64 + 100);
        self.order.push(entry);
        self.paths.insert(entry, name.to_string());
        entry
    }
}

impl Programme for Shelf {
    type Item = str;

    fn first_of_pass(&self, pass: u32) -> Option<EntryId> {
        (pass == 0).then(|| self.order.first().copied()).flatten()
    }

    fn next(&self, entry: EntryId, _why: Advance) -> Option<EntryId> {
        match self.at(entry) {
            Some(at) => self.order.get(at + 1).copied(),
            None => *self.removed.get(&entry)?,
        }
    }

    fn previous(&self, entry: EntryId) -> Option<EntryId> {
        self.order.get(self.at(entry)?.checked_sub(1)?).copied()
    }

    fn item(&self, entry: EntryId) -> Option<&str> {
        self.paths.get(&entry).map(String::as_str)
    }

    fn extent(&self, _entry: EntryId) -> Extent {
        Extent::default()
    }

    fn settle(&mut self) {
        self.removed.clear();
    }
}

/// Three counting files of one shape, told apart by the frames they spell:
/// `a` from 0, `b` from 100 000 and `c` from 200 000.
fn three(lengths: [u32; 3]) -> (MemFiles, [Vec<u32>; 3]) {
    let names = ["a.wav", "b.wav", "c.wav"];
    let firsts = [0, 100_000, 200_000];
    let files: Vec<(&str, Vec<u8>)> = names
        .iter()
        .zip(firsts.iter().zip(lengths))
        .map(|(&name, (&first, frames))| (name, counting_wav_from(8_000, 2, first, frames)))
        .collect();
    let spelled = core::array::from_fn(|at| (firsts[at]..firsts[at] + lengths[at]).collect());
    (MemFiles::of(&files), spelled)
}

fn shelf_engine(lengths: [u32; 3]) -> (Harness<Shelf, RealDecoder>, [Vec<u32>; 3]) {
    let (files, spelled) = three(lengths);
    let harness = engine_playing(
        Shelf::of(&["a.wav", "b.wav", "c.wav"]),
        Settings::default(),
        files,
        AudioDecodeService::new as RealDecoder,
    );
    (harness, spelled)
}

/// Drive the engine without the device taking a frame, until the decoder has
/// opened `entry`.
fn decode_until_opened<P: Programme<Item = str>, F: FnMut() -> S, S: SessionService>(
    engine: &mut TestEngine<P, F>,
    entry: EntryId,
) {
    for _ in 0..1_000 {
        engine.on_decoder(0, true, true);
        engine.on_notify(0);
        let opened = engine
            .take_notes()
            .iter()
            .any(|note| matches!(note, Note::Opened { entry: at, .. } if *at == entry));
        if opened {
            return;
        }
    }
    panic!("the decoder never opened {entry:?}");
}

#[test]
fn an_edit_ahead_of_the_decoder_is_followed() {
    let ((mut engine, service), [a, b, c]) = shelf_engine([8_000, 8_000, 8_000]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().position >= 1_000
    });
    engine.edit(now, |shelf| {
        shelf.order.swap(1, 2);
    });
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(spelled_frames(&heard(&service)), [a, c, b].concat());
}

/// An entry the decoder has already moved on to, but whose frames wait behind
/// a full ring, is dropped unheard when it leaves the programme.
#[test]
fn an_entry_removed_while_it_waits_to_reach_the_stream_is_never_heard() {
    let ((mut engine, service), [a, _, c]) = shelf_engine([4_000, 8_000, 8_000]);
    engine.begin(0);
    decode_until_opened(&mut engine, EntryId::new(1));
    assert!(
        !spelled_frames(&service.borrow().queued.iter().copied().collect::<Vec<u8>>())
            .iter()
            .any(|&frame| frame >= 100_000),
        "the ring holds only a"
    );
    engine.edit(0, |shelf| shelf.remove(EntryId::new(1)));
    run(&mut engine, &service, 0, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(spelled_frames(&heard(&service)), [a, c].concat());
    assert_eq!(service.borrow().streams[0].flushes, 0, "the ring was right");
}

/// Frames of a removed entry already in the stream are given up by stopping on
/// the frame being heard and refilling from exactly there.
#[test]
fn an_entry_removed_once_in_the_stream_is_never_heard_and_nothing_else_is_lost() {
    let ((mut engine, service), [a, _, c]) = shelf_engine([3_000, 8_000, 8_000]);
    engine.begin(0);
    decode_until_opened(&mut engine, EntryId::new(1));
    for _ in 0..10 {
        engine.on_decoder(0, true, true);
    }
    assert!(
        spelled_frames(&service.borrow().queued.iter().copied().collect::<Vec<u8>>())
            .iter()
            .any(|&frame| frame >= 100_000),
        "b is in the ring behind a"
    );
    engine.edit(0, |shelf| shelf.remove(EntryId::new(1)));
    run(&mut engine, &service, 0, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(spelled_frames(&heard(&service)), [a, c].concat());
    assert_eq!(service.borrow().streams[0].flushes, 1);
}

/// A listener already hearing the removed entry when it goes keeps what was
/// heard and goes on with what the programme now has after the one before.
#[test]
fn a_listener_already_into_a_removed_entry_goes_on_to_what_follows() {
    let ((mut engine, service), [a, b, c]) = shelf_engine([3_000, 8_000, 8_000]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.position >= 3_000 + 960
    });
    engine.edit(now, |shelf| shelf.remove(EntryId::new(1)));
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    let heard = spelled_frames(&heard(&service));
    let into_b = heard.len() - a.len() - c.len();
    assert!(into_b >= 960, "b was being heard");
    assert_eq!(heard, [&a[..], &b[..into_b], &c[..]].concat());
}

/// An entry added once decoding had run out is played after what is queued.
#[test]
fn an_entry_added_after_the_programme_ran_out_still_plays() {
    let (files, [a, b, _]) = three([2_000, 8_000, 1]);
    let (mut engine, service) = engine_playing(
        Shelf::of(&["a.wav"]),
        Settings::default(),
        files,
        AudioDecodeService::new as RealDecoder,
    );
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |_, service| {
        service.streams.first().is_some_and(|stream| stream.drained)
    });
    let added = engine.edit(now, |shelf| shelf.push("b.wav"));
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(spelled_frames(&heard(&service)), [a, b].concat());
    assert!(engine
        .take_notes()
        .iter()
        .any(|note| matches!(note, Note::Opened { entry, .. } if *entry == added)));
}

/// A new device takes the playback on from the frame the old one stopped on.
#[test]
fn a_device_change_reopens_the_stream_where_the_listener_is() {
    let file = counting_wav(8_000, 2, 40_000);
    let (mut engine, service) = engine(ask(&["x.wav"]), &[("x.wav", file.clone())]);
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().position >= 8_000
    });
    engine.on_control(now, Control::SetDevice(7));
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    assert_eq!(heard(&service), pcm_of(&file), "every frame once, in order");
    assert_eq!(engine.settings().device_id, 7);
    let service = service.borrow();
    assert_eq!(service.streams.len(), 2);
    assert!(service.streams[0].closed);
    assert_eq!(service.streams[1].params.expect("opened").device_id, 7);
}

/// After a playback ends a jump begins another, with the settings changed in
/// between.
#[test]
fn a_jump_after_playback_has_ended_begins_a_new_one() {
    let first = counting_wav_from(8_000, 2, 0, 16_000);
    let second = counting_wav_from(8_000, 2, 100_000, 4_000);
    let (mut engine, service) = engine(
        ask(&["a.wav", "b.wav"]),
        &[("a.wav", first), ("b.wav", second.clone())],
    );
    engine.begin(0);
    let now = run(&mut engine, &service, 0, |engine, _| {
        engine.status().heard.is_some()
    });
    engine.on_control(now, Control::Stop);
    assert_eq!(engine.outcome(), Some(Outcome::Stopped));
    assert_eq!(engine.status().transport, Transport::Stopped);
    let quieter = AudioGain::new(-600).expect("attenuation");
    engine.on_control(now, Control::SetGain(quieter));
    engine.on_control(now, Control::Next);
    assert_eq!(
        engine.outcome(),
        Some(Outcome::Stopped),
        "only a jump begins one"
    );
    engine.on_control(now, Control::Jump(EntryId::new(1)));
    assert_eq!(engine.outcome(), None);
    run(&mut engine, &service, now, |_, _| false);
    assert_eq!(engine.outcome(), Some(Outcome::Played));
    let service = service.borrow();
    let last = service.streams.last().expect("a second stream");
    assert_eq!(last.heard, pcm_of(&second));
    assert_eq!(last.gains, vec![quieter]);
}

/// The signal as a FLAC file carrying `fields` as its Vorbis comments.
fn tagged_signal(fields: &[&str]) -> Vec<u8> {
    use tairix_sound::flac_encode::{encode, Options, Params};
    let samples: Vec<i32> = pcm_of(&signal_wav())
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&bytes| i32::from(i16::from_le_bytes(bytes)))
        .collect();
    let params = Params {
        rate: wire::RATE_HZ,
        channels: 2,
        bits: 16,
    };
    let mut writer = encode(params, &samples, Options::default()).expect("encodes");
    writer.comments("tairix", fields);
    writer.finish()
}

fn normalised(on: bool) -> Ask {
    Ask {
        settings: Settings {
            normalise: on,
            ..Settings::default()
        },
        ..ask(&["x.flac"])
    }
}

/// A track's own stated gain is applied before the ring, dithered back onto
/// its grid; with normalisation off, or with no gain stated, the track is
/// heard bit for bit.
#[test]
fn a_tracks_stated_loudness_is_applied_only_when_asked_and_only_where_stated() {
    let tagged = tagged_signal(&[
        "REPLAYGAIN_TRACK_GAIN=-6.02 dB",
        "REPLAYGAIN_TRACK_PEAK=1.0",
    ]);
    let pcm = pcm_of(&signal_wav()).to_vec();

    let (mut player, service) = engine(normalised(true), &[("x.flac", tagged.clone())]);
    assert_eq!(play_out(&mut player, &service), Outcome::Played);
    let mut pivot = vec![0.0f32; pcm.len() / 2];
    let samples = tairix_audio::convert::decode(SampleFormat::S16, &pcm, &mut pivot);
    let gain = tairix_audio::volume::millibel_to_linear(-602);
    for value in &mut pivot[..samples] {
        *value *= gain;
    }
    tairix_audio::convert::DitherSource::new(super::DITHER_SEED)
        .apply(SampleFormat::S16, &mut pivot[..samples]);
    let mut expected = vec![0u8; pcm.len()];
    tairix_audio::convert::encode(SampleFormat::S16, &pivot[..samples], &mut expected);
    assert_eq!(heard(&service), expected);
    assert_ne!(expected, pcm);

    let (mut player, service) = engine(normalised(false), &[("x.flac", tagged)]);
    assert_eq!(play_out(&mut player, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm, "normalisation off");

    let (mut player, service) = engine(normalised(true), &[("x.flac", signal_flac())]);
    assert_eq!(play_out(&mut player, &service), Outcome::Played);
    assert_eq!(heard(&service), pcm, "no gain stated");
}

//! The movie player over in-process decoders, each handed a ring of its own,
//! and a fake audio service, driven on a fake clock: a movie opens paused on
//! its first picture, each picture is handed out at its time on the sound's
//! clock, the pictures follow the sound when its device's clock drifts, the
//! sound is decoded far ahead of the pictures the ring holds, a picture
//! overtaken by the next is dropped, a slot the window holds keeps the decoder
//! from drawing past it, pausing holds the clock and a seek shows the keyframe
//! it lands on, the end stops playback once the sound has played out, a
//! decoder that dies is replaced with a ring of its own, a movie too large for
//! the decoder is refused with the reason, and a player closed or abandoned
//! lets its stream go.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::audio::{
    AudioGain, AudioNotify, OpenParams, StreamGrant, StreamReport, StreamState,
};
use tairix_abi::driver::audio::Frames;
use tairix_abi::time::Duration64;
use tairix_abi::Errno;
use tairix_audio::stream::{OpenFailure, Written};
use tairix_hash::HashSeed;
use tairix_log::DiscardSink;
use tairix_player::{Failure, Speaker};
use tairix_sandbox::loopback::{HeapShare, LoopbackShareLauncher};
use tairix_sandbox::moviedecode::fixture::{decoded, recording, Recording, PICTURE_NS, SIZE};
use tairix_sandbox::moviedecode::{movie_bytes, MovieDecodeService, MovieRefusal, RingLayout};
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep, Share};
use tairix_sandbox::supervise::SessionLauncher;

use super::{Note, Order, Picture, Player, Stop, Transport};

const MS: u64 = 1_000_000;

/// A ceiling no movie here comes near.
const ROOMY: u64 = 1 << 30;

/// A step of the fake clock, and the frames a device at its nominal rate
/// reads in one.
const STEP_NS: u64 = 10 * MS;
const STEP_FRAMES: u64 = 480;

/// A ring whose slots hold the pictures at their own size: the screen.
fn layout() -> RingLayout {
    RingLayout::new(SIZE.0, SIZE.1).expect("a box")
}

/// An honest decoder drawing into `ring`.
fn honest(ring: HeapShare) -> MovieDecodeService<HeapShare> {
    MovieDecodeService::new(Some(ring))
}

/// The pixels of `picture`, copied out of its slot.
fn pixels(picture: &Picture) -> Vec<u8> {
    let (width, height) = picture.slot.size();
    let mut out = vec![0u8; width as usize * height as usize * 4];
    assert!(picture.slot.copy_out(&mut out), "the slot lies in its ring");
    out
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the player's seed source may have no seed to give; this one always has"
)]
fn seed() -> Option<HashSeed> {
    Some(HashSeed::from_bytes([7; HashSeed::LEN]))
}

/// The audio service and its device, read as each test says.
#[derive(Default)]
struct Service {
    opened: Option<OpenParams>,
    refuse_open: bool,
    ring_frames: u64,
    /// Frames in the ring not yet read.
    queued: u64,
    /// Frames the device has read.
    position: u64,
    /// The stream's state, while one is open.
    state: Option<StreamState>,
    notices: VecDeque<AudioNotify>,
    starts: Vec<u64>,
}

impl Service {
    /// The device reads up to `frames` frames, as a running device does.
    fn period(&mut self, frames: u64) {
        if !matches!(
            self.state,
            Some(StreamState::Running | StreamState::Draining)
        ) {
            return;
        }
        let took = frames.min(self.queued);
        self.queued -= took;
        self.position += took;
        self.notices.push_back(AudioNotify::SpaceAvailable {
            stream_id: 1,
            position: Frames::new(self.position),
        });
        self.drained();
    }

    /// A draining stream with nothing left goes idle, and says so.
    fn drained(&mut self) {
        if self.state == Some(StreamState::Draining) && self.queued == 0 {
            self.state = Some(StreamState::Idle);
            self.notices.push_back(AudioNotify::StateChanged {
                stream_id: 1,
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
        if service.refuse_open {
            return Err(OpenFailure::Refused(Errno::NotFound));
        }
        service.opened = Some(*params);
        service.ring_frames = u64::from(params.latency_target_frames);
        service.state = Some(StreamState::Idle);
        Ok(StreamGrant {
            stream_id: 1,
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
        if at.get() != service.position + service.queued {
            return Err(Errno::OutOfRange);
        }
        let channels = service
            .opened
            .map_or(2, |params| usize::from(params.channel_map.channels()));
        let offered = (samples.len() / (channels * 4)) as u64;
        let took = (service.ring_frames - service.queued).min(offered);
        service.queued += took;
        Ok(Written {
            silence_frames: 0,
            sample_frames: u32::try_from(took).expect("a ring's worth"),
        })
    }

    fn start(&mut self, at: Frames) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        service.state = Some(StreamState::Running);
        service.starts.push(at.get());
        Ok(())
    }

    fn pause(&mut self) -> Result<Frames, Errno> {
        let mut service = self.0.borrow_mut();
        service.state = Some(StreamState::Paused);
        Ok(Frames::new(service.position))
    }

    fn drain(&mut self) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        service.state = Some(StreamState::Draining);
        service.drained();
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Errno> {
        let mut service = self.0.borrow_mut();
        service.position += service.queued;
        service.queued = 0;
        Ok(())
    }

    fn set_gain(&mut self, _gain: AudioGain) -> Result<(), Errno> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), Errno> {
        self.0.borrow_mut().state = None;
        Ok(())
    }

    fn take_notify(&mut self) -> Result<Option<AudioNotify>, Errno> {
        Ok(self.0.borrow_mut().notices.pop_front())
    }

    fn report(&mut self) -> Result<StreamReport, Errno> {
        let service = self.0.borrow();
        Ok(StreamReport {
            state: service.state.unwrap_or(StreamState::Idle),
            changed_at: Frames::new(service.position),
            xruns: 0,
            xrun_frames: 0,
        })
    }
}

/// Serves as the movie service does, but ends its stream on its `left`-th
/// frame: a decoder that crashes part-way.
struct Dying {
    inner: MovieDecodeService<HeapShare>,
    left: usize,
}

impl SessionService for Dying {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        if self.left == 0 {
            return SessionStep::Finished;
        }
        self.left -= 1;
        self.inner.handle(request, out)
    }
}

/// A player of `movie` whose decoders `factory` builds, each handed a ring of
/// [`layout`], allowed `ceiling`, its sound into `service`.
fn player<F: FnMut(HeapShare) -> S, S: SessionService>(
    movie: &Recording,
    factory: F,
    service: &Rc<RefCell<Service>>,
    ceiling: u64,
) -> Tested<LoopbackShareLauncher<F>, FileRead> {
    let file = movie.file.clone();
    let len = u64::try_from(file.len()).expect("small");
    let read: FileRead = Box::new(move |offset: u64, buf: &mut [u8]| {
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let held = file.get(start..).unwrap_or_default();
        let count = held.len().min(buf.len());
        buf[..count].copy_from_slice(&held[..count]);
        Ok(count)
    });
    let speaker = FakeSpeaker(Rc::clone(service));
    let launcher = LoopbackShareLauncher::new(layout().ring_bytes(), factory);
    let room = (ceiling, layout());
    Player::new(launcher, DiscardSink, (read, len), room, speaker, seed).expect("bounds")
}

type Tested<L, R> = Player<L, DiscardSink, R, FakeSpeaker>;

/// How a test player reads its recording.
type FileRead = Box<dyn FnMut(u64, &mut [u8]) -> Result<usize, Errno>>;

/// Let the decoder answer everything it is asked at `now`.
fn settle<L, R>(player: &mut Tested<L, R>, now: u64)
where
    L: SessionLauncher,
    R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
{
    for _ in 0..64 {
        player.on_decoder(now, true, true);
    }
}

/// A player of `movie`, opened and settled on its first picture, which is
/// taken and its slot handed back.
fn opened<L, R>(player: &mut Tested<L, R>) -> u64
where
    L: SessionLauncher,
    R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
{
    player.begin(0);
    settle(player, 0);
    let picture = player.take_picture().expect("the first picture");
    player.release(0, picture.slot.release());
    picture.at_ns
}

/// Run the clock from `from` to `until` a step at a time, the device reading
/// `frames` frames a step, answering the times of the pictures handed out,
/// whose slots are handed back.
fn run<L, R>(
    player: &mut Tested<L, R>,
    service: &Rc<RefCell<Service>>,
    (from, until): (u64, u64),
    frames: u64,
) -> Vec<u64>
where
    L: SessionLauncher,
    R: FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
{
    let mut shown = Vec::new();
    let mut now = from;
    while now < until {
        now += STEP_NS;
        service.borrow_mut().period(frames);
        player.on_notify(now);
        player.on_timer(now);
        settle(player, now);
        while let Some(picture) = player.take_picture() {
            shown.push(picture.at_ns);
            player.release(now, picture.slot.release());
        }
    }
    shown
}

/// The times of pictures `first..end` of a movie.
fn times(first: u64, end: u64) -> Vec<u64> {
    (first..end).map(|index| index * PICTURE_NS).collect()
}

#[test]
fn a_movie_opens_paused_on_its_first_picture() {
    let movie = recording(6, 3, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    player.begin(0);
    settle(&mut player, 0);
    let notes = player.take_notes();
    assert!(
        matches!(notes.as_slice(), [Note::Opened(info)] if (info.width, info.height) == SIZE),
        "{notes:?}"
    );
    let picture = player.take_picture().expect("the first picture");
    assert_eq!((picture.at_ns, picture.slot.size()), (0, SIZE));
    assert_eq!(
        tairix_fuzzseed::first_difference(&pixels(&picture), &decoded(&movie.pictures)[0]),
        None
    );
    assert_eq!(player.status().transport, Transport::Paused);
    assert_eq!(player.deadline(), None, "paused, nothing is timed");
    assert!(
        service.borrow().starts.is_empty(),
        "the sound waits for play"
    );
}

#[test]
fn playing_hands_out_each_picture_at_its_time_on_the_sounds_clock() {
    let movie = recording(12, 6, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    assert_eq!(
        service.borrow().starts,
        [0],
        "the stream starts once primed"
    );
    assert_eq!(
        player.deadline(),
        Some(PICTURE_NS),
        "one shot, to the next picture"
    );
    let shown = run(&mut player, &service, (0, 440 * MS), STEP_FRAMES);
    assert_eq!(shown, times(1, 12), "every picture, in order, at its time");
    assert_eq!(player.status().late, 0);
}

#[test]
fn the_pictures_follow_the_sound_when_its_device_runs_slow() {
    let movie = recording(12, 6, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    // A device at nine tenths of its rate: 200 ms of the machine's clock play
    // 180 ms of sound, so the picture at 200 ms is not yet due.
    let slow = STEP_FRAMES * 9 / 10;
    let shown = run(&mut player, &service, (0, 200 * MS), slow);
    assert_eq!(shown, times(1, 5));
    let later = run(&mut player, &service, (200 * MS, 230 * MS), slow);
    assert_eq!(later, [5 * PICTURE_NS], "due once the sound reaches it");
}

/// The regression: the sound was walked with the pictures, so it was decoded
/// no further ahead than the two pictures the ring holds, and a device reading
/// a tenth of a second at once ran dry at three quarters of it.
#[test]
fn the_sound_is_decoded_ahead_of_the_pictures_the_ring_holds() {
    let movie = recording(40, 10, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let queued = service.borrow().queued;
    assert!(
        queued >= 4 * STEP_FRAMES * 10,
        "{queued} frames are ready to play"
    );
}

#[test]
fn a_picture_overtaken_by_the_next_is_dropped() {
    let movie = recording(12, 6, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    // A tenth of a second passes in one wake: both pictures the ring holds
    // ahead are due, so the earlier is dropped and the later shown.
    service.borrow_mut().period(STEP_FRAMES * 10);
    player.on_notify(100 * MS);
    player.on_timer(100 * MS);
    let picture = player.take_picture().expect("the later picture");
    assert_eq!(picture.at_ns, 2 * PICTURE_NS);
    assert_eq!(player.status().late, 1);
}

#[test]
fn a_slot_the_window_holds_keeps_the_decoder_from_drawing_past_it() {
    let movie = recording(12, 6, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let mut held = Vec::new();
    let mut now = 0;
    while now < 400 * MS {
        now += STEP_NS;
        service.borrow_mut().period(STEP_FRAMES);
        player.on_notify(now);
        player.on_timer(now);
        settle(&mut player, now);
        held.extend(player.take_picture());
    }
    let times: Vec<u64> = held.iter().map(|picture| picture.at_ns).collect();
    assert_eq!(
        times,
        self::times(1, 4),
        "the ring's three slots, and no more"
    );
    let first = held.remove(0);
    player.release(now, first.slot.release());
    let shown = run(&mut player, &service, (now, now + 200 * MS), STEP_FRAMES);
    assert!(!shown.is_empty(), "a slot handed back is drawn into again");
}

#[test]
fn pausing_holds_the_clock_and_playing_resumes_where_it_paused() {
    let movie = recording(12, 6, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let shown = run(&mut player, &service, (0, 120 * MS), STEP_FRAMES);
    assert_eq!(shown, times(1, 4));
    player.order(120 * MS, Order::Pause);
    assert_eq!(player.status().transport, Transport::Paused);
    assert_eq!(player.deadline(), None);
    let paused = run(&mut player, &service, (120 * MS, 400 * MS), STEP_FRAMES);
    assert!(paused.is_empty(), "nothing moves while paused");
    assert_eq!(player.status().position_ns, 3 * PICTURE_NS);
    player.order(400 * MS, Order::Play);
    let resumed = run(&mut player, &service, (400 * MS, 440 * MS), STEP_FRAMES);
    assert_eq!(
        resumed,
        [4 * PICTURE_NS],
        "on from the picture it paused on"
    );
}

#[test]
fn a_seek_shows_the_keyframe_it_lands_on() {
    let movie = recording(15, 5, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Seek(300 * MS));
    settle(&mut player, 0);
    let picture = player.take_picture().expect("the keyframe");
    assert_eq!(
        picture.at_ns,
        5 * PICTURE_NS,
        "the latest keyframe before 300 ms"
    );
    assert_eq!(
        tairix_fuzzseed::first_difference(&pixels(&picture), &decoded(&movie.pictures)[5]),
        None
    );
    player.release(0, picture.slot.release());
    assert_eq!(
        (player.status().transport, player.status().position_ns),
        (Transport::Paused, 5 * PICTURE_NS)
    );
    player.order(0, Order::Play);
    settle(&mut player, 0);
    // The sound after the seek starts at its first packet past the codec
    // delay, so the pictures run on its clock from there.
    let shown = run(&mut player, &service, (0, 100 * MS), STEP_FRAMES);
    assert_eq!(shown, times(6, 8), "and plays on from there");
}

#[test]
fn a_status_counts_the_seeks_it_was_ordered() {
    let movie = recording(15, 5, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    assert_eq!(player.status().seeks, 0);
    player.order(0, Order::Seek(300 * MS));
    player.order(0, Order::Seek(100 * MS));
    assert_eq!(player.status().seeks, 2, "each order counts, served or not");
}

#[test]
fn a_closed_player_lets_its_stream_go() {
    let movie = recording(6, 3, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let live = service.borrow().state;
    assert!(
        matches!(live, Some(StreamState::Running | StreamState::Draining)),
        "the stream plays: {live:?}"
    );
    player.close();
    assert_eq!(service.borrow().state, None, "the audio service is told");
}

#[test]
fn an_abandoned_playback_stops_for_good() {
    let movie = recording(6, 3, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let failure = Failure::Wait {
        what: "wait for playback",
        errno: Errno::BrokenPipe,
    };
    player.abandon(failure);
    let stopped = Transport::Stopped(Stop::Abandoned(failure));
    assert_eq!(player.status().transport, stopped);
    assert_eq!(service.borrow().state, None, "its stream is let go");
    player.order(0, Order::Play);
    player.order(0, Order::Seek(0));
    assert_eq!(player.status().transport, stopped, "it does not play again");
}

#[test]
fn the_movie_stops_once_its_sound_has_played_out() {
    for (cut, stop) in [(false, Stop::Ended), (true, Stop::CutShort)] {
        let mut movie = recording(10, 5, true, true);
        if cut {
            let len = movie.file.len() * 3 / 4;
            movie.file.truncate(len);
        }
        let service = Rc::new(RefCell::new(Service::default()));
        let mut player = player(&movie, honest, &service, ROOMY);
        opened(&mut player);
        player.order(0, Order::Play);
        settle(&mut player, 0);
        let shown = run(&mut player, &service, (0, 800 * MS), STEP_FRAMES);
        assert!(!shown.is_empty() && shown.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            player.status().transport,
            Transport::Stopped(stop),
            "cut {cut}"
        );
        assert_eq!(player.deadline(), None, "nothing is timed once stopped");
    }
}

#[test]
fn a_decoder_that_dies_is_replaced_with_a_ring_of_its_own_and_the_pictures_go_on() {
    let movie = recording(20, 5, true, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut launches = 0;
    let factory = move |ring| {
        launches += 1;
        Dying {
            inner: honest(ring),
            left: if launches == 1 { 25 } else { usize::MAX },
        }
    };
    let mut player = player(&movie, factory, &service, ROOMY);
    player.begin(0);
    settle(&mut player, 0);
    let first = player.take_picture().expect("the first picture");
    let first_ring: Arc<dyn Share> = Arc::clone(&first.slot.ring);
    player.release(0, first.slot.release());
    drop(first);
    player.order(0, Order::Play);
    settle(&mut player, 0);
    let mut shown = Vec::new();
    let mut now = 0;
    while now < 1_200 * MS {
        now += STEP_NS;
        service.borrow_mut().period(STEP_FRAMES);
        player.on_notify(now);
        player.on_timer(now);
        settle(&mut player, now);
        while let Some(picture) = player.take_picture() {
            let fresh = !Arc::ptr_eq(&picture.slot.ring, &first_ring);
            shown.push((picture.at_ns, fresh));
            player.release(now, picture.slot.release());
        }
    }
    let notes = player.take_notes();
    assert!(notes.contains(&Note::Replaced), "{notes:?}");
    let times: Vec<u64> = shown.iter().map(|(at, _)| *at).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] < pair[1]),
        "no picture comes twice: {times:?}"
    );
    assert_eq!(times.last(), Some(&(19 * PICTURE_NS)));
    assert!(
        shown.iter().any(|(_, fresh)| *fresh),
        "drawn into a new ring"
    );
    assert_eq!(Arc::strong_count(&first_ring), 1, "the old ring is let go");
    assert_eq!(player.status().transport, Transport::Stopped(Stop::Ended));
}

#[test]
fn a_movie_too_large_for_the_decoder_is_refused_with_the_reason() {
    let movie = recording(3, 3, true, true);
    let needed = movie_bytes(SIZE.0, SIZE.1, 2, layout()).expect("a size a stream declares");
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, needed - 1);
    player.begin(0);
    settle(&mut player, 0);
    let refused = Stop::Refused(MovieRefusal::TooLarge);
    assert_eq!(player.take_notes(), [Note::Unopened(refused)]);
    assert_eq!(player.status().transport, Transport::Stopped(refused));
    assert!(
        service.borrow().opened.is_none(),
        "no stream was opened for it"
    );
}

#[test]
fn a_silent_movie_plays_on_the_monotonic_clock() {
    let movie = recording(6, 3, false, true);
    let service = Rc::new(RefCell::new(Service::default()));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    player.order(0, Order::Play);
    assert_eq!(player.deadline(), Some(PICTURE_NS));
    let shown = run(&mut player, &service, (0, 200 * MS), 0);
    assert_eq!(shown, times(1, 6));
    assert_eq!(player.status().transport, Transport::Stopped(Stop::Ended));
}

#[test]
fn a_movie_whose_sound_cannot_play_goes_on_silent_and_says_so() {
    let movie = recording(6, 3, true, true);
    let service = Rc::new(RefCell::new(Service {
        refuse_open: true,
        ..Service::default()
    }));
    let mut player = player(&movie, honest, &service, ROOMY);
    opened(&mut player);
    let lost = Note::SoundLost(Failure::Stream(OpenFailure::Refused(Errno::NotFound)));
    assert!(player.take_notes().contains(&lost));
    player.order(0, Order::Play);
    let shown = run(&mut player, &service, (0, 200 * MS), 0);
    assert_eq!(shown, times(1, 6), "the pictures keep their own time");
}

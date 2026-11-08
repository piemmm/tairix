//! The sandboxed movie decode end to end, over in-process workers each handed
//! a ring of their own: recordings muxed here from VP8 frames the bitstream
//! fixture writes and Opus packets the encoder codes come out unit by unit,
//! pictures drawn into the ring and sound over the pipe, each as an
//! in-process decode gives it; a held slot is never drawn into and one handed
//! back is; a picture larger than a slot is scaled down to fit it; a seek moves
//! both walks to the latest keyframe at or before its target; a recording its
//! writer never finished, and one cut short, play as far as they hold; a
//! stream too large for the ceiling is refused, and an open no ring was handed
//! for; a replaced worker comes back at the latest keyframe in a ring of its
//! own; and no answer an honest worker would not give is believed.

extern crate std;

use std::sync::Arc;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::ChannelMap;
use tairix_hash::HashSeed;
use tairix_raster::{resample_window, Region, Rgba8Image};

use super::fixture::{decoded, decoded_sound, recording, PICTURE_NS, SIZE, SOUND_NS};
use super::{
    encode_picture, is_movie, movie_bytes, session_bounds, Ending, MovieDecodeClient,
    MovieDecodeError, MovieDecodeService, MovieEvent, MovieRefusal, PictureHead, RingLayout,
    ALL_SLOTS, CHANNEL_MAP_WIRE_LEN, FROM_NEED, FROM_OPENED, FROM_REFUSED, FROM_SOUND,
    REFUSED_NOT_OPEN, RING_SLOTS, SIGNATURE_LEN, TO_NEXT,
};
use crate::loopback::{HeapShare, LoopbackShareLauncher};
use crate::proto::ProtoError;
use crate::session::{FrameOut, SessionError, SessionService, SessionStep, Share};
use crate::supervise::{send_waiting, Sent, SupervisedSession};
use crate::testing::NullSink;
use crate::wire::Writer;

const SEED: HashSeed = HashSeed::from_words(0x0123_4567, 0x89ab_cdef);

#[test]
fn a_recording_is_known_by_its_signature() {
    let movie = recording(3, 3, true, true);
    assert!(is_movie(&movie.file[..SIGNATURE_LEN]));
    assert!(!is_movie(b"\x89PNG\r\n\x1a\n"), "a picture is no movie");
    assert!(
        !is_movie(&movie.file[..SIGNATURE_LEN - 1]),
        "nor is a file too short to say"
    );
}

/// A ceiling no recording here comes near.
const ROOMY: u64 = 1 << 30;

const MS: u64 = 1_000_000;

/// A ring whose slots hold the recordings' pictures at their own size.
fn screen() -> RingLayout {
    RingLayout::new(SIZE.0, SIZE.1).expect("a box")
}

/// A unit the worker answered, owned: a picture as copied out of its slot.
#[derive(Clone, Debug, PartialEq)]
enum Got {
    Opened,
    Picture(PictureHead, Vec<u8>),
    Skipped(u64),
    Sound(u64, Vec<u8>),
    Ended(Ending),
    Sought,
    Refused(MovieRefusal),
    Resumed,
}

type Session<F> = SupervisedSession<LoopbackShareLauncher<F>, NullSink>;

/// A session whose workers `factory` builds, each handed a ring of `layout`,
/// its first already started.
fn supervised<F: FnMut(HeapShare) -> S, S: SessionService>(
    layout: RingLayout,
    factory: F,
) -> Session<F> {
    let bounds = session_bounds().expect("bounds");
    let launcher = LoopbackShareLauncher::new(layout.ring_bytes(), factory);
    let mut session = SupervisedSession::new(launcher, bounds, NullSink);
    session.start(0).expect("a worker starts");
    session
}

/// A session over honest workers, each handed a ring of `layout`.
fn honest(layout: RingLayout) -> Session<impl FnMut(HeapShare) -> MovieDecodeService<HeapShare>> {
    supervised(layout, |ring| MovieDecodeService::new(Some(ring)))
}

/// Why a turn stopped short of an answer.
enum Failed {
    Worker,
    Client(MovieDecodeError),
}

/// The one way a session fails these workers: a worker that failed.
fn worker(err: SessionError) -> Failed {
    assert_eq!(err, SessionError::WorkerFailed, "the session itself failed");
    Failed::Worker
}

/// The owner of one decode: the file it reads for the worker, the ring it
/// states, the clock its session is paced by, and whether it hands each slot
/// back once it has copied its picture out.
struct Owner<'f> {
    file: &'f [u8],
    layout: RingLayout,
    now: u64,
    release: bool,
}

impl<'f> Owner<'f> {
    fn new(file: &'f [u8], layout: RingLayout) -> Self {
        Self {
            file,
            layout,
            now: 0,
            release: true,
        }
    }

    /// An owner keeping every slot it is handed.
    fn holding(file: &'f [u8], layout: RingLayout) -> Self {
        Self {
            release: false,
            ..Self::new(file, layout)
        }
    }

    /// Carry the waiting request to its answer, replacing failed workers.
    fn answer<F: FnMut(HeapShare) -> S, S: SessionService>(
        &mut self,
        session: &mut Session<F>,
        client: &mut MovieDecodeClient,
    ) -> Result<Got, MovieDecodeError> {
        for _ in 0..100_000 {
            match self.turn(session, client) {
                Ok(Some(got)) => return Ok(got),
                Ok(None) => {}
                Err(Failed::Worker) => {
                    self.now = session.restart_deadline().expect("a replacement is due");
                    session.start(self.now).expect("a replacement starts");
                    client.restart(SEED)?;
                }
                Err(Failed::Client(err)) => return Err(err),
            }
        }
        panic!("the request was never answered");
    }

    fn turn<F: FnMut(HeapShare) -> S, S: SessionService>(
        &mut self,
        session: &mut Session<F>,
        client: &mut MovieDecodeClient,
    ) -> Result<Option<Got>, Failed> {
        if send_waiting(client, session, self.now) == Sent::Lost {
            return Err(Failed::Worker);
        }
        session.on_readable(self.now).map_err(worker)?;
        let ring = session.share().cloned();
        let mut answered = None;
        loop {
            let now = self.now;
            let taken = session
                .recv(now, |frame| self.take(client, ring.as_deref(), frame))
                .map_err(worker)?;
            match taken {
                None => return Ok(answered),
                Some(Ok(Some(got))) => answered = Some(got),
                Some(Ok(None)) => {}
                Some(Err(MovieDecodeError::Unbelievable)) => {
                    session.condemn(now, "an unbelievable answer");
                    return Err(Failed::Worker);
                }
                Some(Err(err)) => return Err(Failed::Client(err)),
            }
        }
    }

    /// Act on one frame: serve a need, copy a picture out of its slot of
    /// `ring`, or own an answer.
    fn take(
        &mut self,
        client: &mut MovieDecodeClient,
        ring: Option<&dyn Share>,
        frame: &[u8],
    ) -> Result<Option<Got>, MovieDecodeError> {
        let got = match client.on_frame(frame)? {
            MovieEvent::Need { offset, len } => {
                let start = usize::try_from(offset).expect("small");
                client.supply(&self.file[start..start + len])?;
                return Ok(None);
            }
            MovieEvent::Resuming => return Ok(None),
            MovieEvent::Picture(head) => {
                let mut pixels = vec![0u8; head.pixel_len()];
                let at = self.layout.slot_at(head.slot);
                let ring = ring.expect("the worker was handed a ring");
                assert!(ring.copy_out(at, &mut pixels), "the slot lies in the ring");
                if self.release {
                    client.release(head.slot)?;
                }
                Got::Picture(head, pixels)
            }
            MovieEvent::Opened => Got::Opened,
            MovieEvent::Skipped { at_ns } => Got::Skipped(at_ns),
            MovieEvent::Sound { at_ns, pcm } => Got::Sound(at_ns, pcm.to_vec()),
            MovieEvent::Ended(ending) => Got::Ended(ending),
            MovieEvent::Sought => Got::Sought,
            MovieEvent::Refused(refusal) => Got::Refused(refusal),
            MovieEvent::Resumed => Got::Resumed,
        };
        Ok(Some(got))
    }
}

/// Open the owner's file in a worker that may map `ceiling`, answering the
/// client and how the open was answered.
fn opened<F: FnMut(HeapShare) -> S, S: SessionService>(
    session: &mut Session<F>,
    owner: &mut Owner<'_>,
    ceiling: u64,
) -> (MovieDecodeClient, Result<Got, MovieDecodeError>) {
    let mut client = MovieDecodeClient::new();
    let len = u64::try_from(owner.file.len()).expect("small");
    client.open(len, ceiling, owner.layout, SEED).expect("free");
    let got = owner.answer(session, &mut client);
    (client, got)
}

/// Every picture up to the walk's end or a refusal, drawn unless due before
/// `skip_before_ns`.
fn walk<F: FnMut(HeapShare) -> S, S: SessionService>(
    session: &mut Session<F>,
    owner: &mut Owner<'_>,
    client: &mut MovieDecodeClient,
    skip_before_ns: u64,
) -> Vec<Got> {
    let mut got = Vec::new();
    loop {
        client.next(skip_before_ns).expect("idle, with a slot free");
        let unit = owner.answer(session, client).expect("answered");
        let done = matches!(unit, Got::Ended(_) | Got::Refused(_));
        got.push(unit);
        if done {
            return got;
        }
    }
}

/// Every block of sound to the walk's end.
fn listen<F: FnMut(HeapShare) -> S, S: SessionService>(
    session: &mut Session<F>,
    owner: &mut Owner<'_>,
    client: &mut MovieDecodeClient,
) -> Vec<(u64, Vec<u8>)> {
    let mut heard = Vec::new();
    loop {
        client.sound().expect("idle, with sound");
        match owner.answer(session, client).expect("answered") {
            Got::Sound(at, bytes) => heard.push((at, bytes)),
            Got::Ended(Ending::Complete) => return heard,
            other => panic!("not sound: {other:?}"),
        }
    }
}

/// The pictures among `got`.
fn drawn(got: &[Got]) -> Vec<(PictureHead, Vec<u8>)> {
    got.iter()
        .filter_map(|unit| match unit {
            Got::Picture(head, pixels) => Some((*head, pixels.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn pictures_and_sound_come_in_order_each_as_an_in_process_decode_gives_it() {
    let movie = recording(12, 5, true, true);
    let mut session = honest(screen());
    let mut owner = Owner::new(&movie.file, screen());
    let (mut client, opened) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(opened, Ok(Got::Opened));
    let info = client.info().expect("open");
    assert_eq!((info.width, info.height), SIZE);
    assert_eq!(info.sound, Some(ChannelMap::STEREO));
    assert!(info.indexed && info.duration_ns.is_some());

    // All of the sound first, as a player decodes it far ahead of the
    // pictures: the walks are apart.
    let heard = listen(&mut session, &mut owner, &mut client);
    assert!(
        heard == decoded_sound(&movie.packets),
        "every packet, at its time, decoded past the codec delay"
    );

    let got = walk(&mut session, &mut owner, &mut client, 0);
    assert_eq!(got.last(), Some(&Got::Ended(Ending::Complete)));
    let pictures = drawn(&got);
    let times: Vec<u64> = pictures.iter().map(|(head, _)| head.at_ns).collect();
    let shown: Vec<u64> = movie.pictures.iter().map(|(at, _, _)| *at).collect();
    assert_eq!(times, shown, "every picture, in order, at its time");
    for ((head, pixels), (expected, (_, keyframe, _))) in pictures
        .iter()
        .zip(decoded(&movie.pictures).iter().zip(&movie.pictures))
    {
        assert_eq!(
            ((head.width, head.height), head.keyframe),
            (SIZE, *keyframe)
        );
        assert_eq!(
            tairix_fuzzseed::first_difference(pixels, expected),
            None,
            "the picture at {}",
            head.at_ns
        );
    }
}

#[test]
fn a_seek_moves_both_walks_to_the_latest_keyframe_at_or_before_its_target() {
    let movie = recording(30, 10, true, true);
    let mut session = honest(screen());
    let mut owner = Owner::new(&movie.file, screen());
    let (mut client, _) = opened(&mut session, &mut owner, ROOMY);
    for (target, landing) in [(700 * MS, 400 * MS), (800 * MS, 800 * MS), (0, 0)] {
        client.seek(target).expect("idle");
        assert_eq!(owner.answer(&mut session, &mut client), Ok(Got::Sought));
        client.sound().expect("idle");
        let Ok(Got::Sound(sounded, _)) = owner.answer(&mut session, &mut client) else {
            panic!("sound after the seek");
        };
        assert!(
            sounded.abs_diff(landing) <= 2 * SOUND_NS,
            "sound from {sounded}"
        );
        let got = walk(&mut session, &mut owner, &mut client, 0);
        let (head, pixels) = drawn(&got).first().cloned().expect("a picture");
        assert_eq!(
            (head.at_ns, head.keyframe),
            (landing, true),
            "seek to {target}"
        );
        let index = usize::try_from(landing / PICTURE_NS).expect("small");
        assert_eq!(
            tairix_fuzzseed::first_difference(&pixels, &decoded(&movie.pictures)[index]),
            None
        );
    }
}

#[test]
fn a_held_slot_is_never_drawn_into_and_one_handed_back_is() {
    let movie = recording(8, 8, false, true);
    let mut session = honest(screen());
    let mut owner = Owner::holding(&movie.file, screen());
    let (mut client, _) = opened(&mut session, &mut owner, ROOMY);
    let ring = session.share().cloned().expect("a ring");
    let mut slots = Vec::new();
    for _ in 0..RING_SLOTS {
        client.next(0).expect("a slot is free");
        let Ok(Got::Picture(head, _)) = owner.answer(&mut session, &mut client) else {
            panic!("a picture");
        };
        slots.push(head.slot);
    }
    slots.sort_unstable();
    assert_eq!(slots, [0, 1, 2], "each into a slot of its own");
    assert_eq!(
        client.next(0),
        Err(MovieDecodeError::OutOfTurn),
        "none is free"
    );

    let layout = screen();
    let mut before = vec![0u8; layout.ring_bytes()];
    assert!(ring.copy_out(0, &mut before));
    client.release(1).expect("held");
    assert_eq!(
        client.release(1),
        Err(MovieDecodeError::OutOfTurn),
        "handed back once"
    );
    client.next(0).expect("a slot was handed back");
    let Ok(Got::Picture(head, pixels)) = owner.answer(&mut session, &mut client) else {
        panic!("a picture");
    };
    assert_eq!(head.slot, 1, "drawn into the slot handed back");
    assert_eq!(
        tairix_fuzzseed::first_difference(&pixels, &decoded(&movie.pictures)[3]),
        None
    );
    let mut after = vec![0u8; layout.ring_bytes()];
    assert!(ring.copy_out(0, &mut after));
    for held in [0, 2] {
        let slot = layout.slot_at(held)..layout.slot_at(held) + layout.slot_bytes();
        assert!(
            before[slot.clone()] == after[slot],
            "slot {held} is untouched"
        );
    }
}

#[test]
fn a_picture_larger_than_a_slot_is_scaled_down_to_fit_it() {
    let movie = recording(3, 3, false, true);
    let small = RingLayout::new(24, 20).expect("a box");
    assert_eq!(small.fit(SIZE.0, SIZE.1), (24, 16), "its aspect is kept");
    let mut session = honest(small);
    let mut owner = Owner::new(&movie.file, small);
    let (mut client, _) = opened(&mut session, &mut owner, ROOMY);
    client.next(0).expect("idle");
    let Ok(Got::Picture(head, pixels)) = owner.answer(&mut session, &mut client) else {
        panic!("a picture");
    };
    assert_eq!((head.width, head.height), (24, 16));
    let own = decoded(&movie.pictures).swap_remove(0);
    let source = Rgba8Image::new(SIZE.0, SIZE.1, &own).expect("an image");
    let window = Region {
        x: 0,
        y: 0,
        width: 24,
        height: 16,
    };
    let mut scaled = vec![0u8; head.pixel_len()];
    resample_window(&source, source.whole(), 24, 16, window, &mut scaled).expect("resamples");
    assert_eq!(
        tairix_fuzzseed::first_difference(&pixels, &scaled),
        None,
        "scaled as the shared resampler scales"
    );
}

#[test]
fn an_open_no_ring_was_handed_for_is_refused() {
    let movie = recording(3, 3, false, true);
    let small = RingLayout::new(8, 8).expect("a box");
    let mut session = supervised(small, |ring| MovieDecodeService::new(Some(ring)));
    let mut owner = Owner::new(&movie.file, screen());
    let (_, refused) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(
        refused,
        Ok(Got::Refused(MovieRefusal::NoRing)),
        "a ring too small"
    );

    let mut client = MovieDecodeClient::new();
    client.open(1 << 20, ROOMY, screen(), SEED).expect("free");
    let request = client.outgoing().expect("an open").to_vec();
    client.sent();
    let mut ringless = MovieDecodeService::<HeapShare>::new(None);
    let mut emitted = Emitted(Vec::new());
    assert_eq!(
        ringless.handle(&request, &mut emitted),
        SessionStep::Continue
    );
    let answer = emitted.0.first().expect("an answer");
    assert_eq!(
        client.on_frame(answer),
        Ok(MovieEvent::Refused(MovieRefusal::NoRing)),
        "no ring at all"
    );
}

#[test]
fn a_recording_its_writer_never_finished_plays_from_its_start() {
    let movie = recording(25, 10, true, false);
    let mut session = honest(screen());
    let mut owner = Owner::new(&movie.file, screen());
    let (mut client, opened) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(opened, Ok(Got::Opened));
    let info = client.info().expect("open");
    assert_eq!((info.duration_ns, info.indexed), (None, false));
    let got = walk(&mut session, &mut owner, &mut client, 0);
    assert!(matches!(got.last(), Some(Got::Ended(_))));
    let pictures = drawn(&got);
    assert!(!pictures.is_empty(), "what was written plays");
    let expected = decoded(&movie.pictures);
    for (index, (head, pixels)) in pictures.iter().enumerate() {
        assert_eq!(head.at_ns, movie.pictures[index].0);
        assert_eq!(
            tairix_fuzzseed::first_difference(pixels, &expected[index]),
            None
        );
    }
    client.seek(500 * MS).expect("idle");
    assert_eq!(owner.answer(&mut session, &mut client), Ok(Got::Sought));
    let after = walk(&mut session, &mut owner, &mut client, 0);
    let first = drawn(&after)
        .first()
        .map(|(head, _)| (head.at_ns, head.keyframe));
    assert_eq!(first, Some((400 * MS, true)), "the walk found the keyframe");
}

#[test]
fn a_file_cut_short_plays_as_far_as_it_holds_and_ends() {
    let movie = recording(20, 5, true, true);
    let cut = &movie.file[..movie.file.len() * 2 / 3];
    let mut session = honest(screen());
    let mut owner = Owner::new(cut, screen());
    let (mut client, opened) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(opened, Ok(Got::Opened));
    let got = walk(&mut session, &mut owner, &mut client, 0);
    assert_eq!(got.last(), Some(&Got::Ended(Ending::Truncated)));
    let pictures = drawn(&got);
    assert!(!pictures.is_empty() && pictures.len() < movie.pictures.len());
    let expected = decoded(&movie.pictures);
    for (index, (head, pixels)) in pictures.iter().enumerate() {
        assert_eq!(head.at_ns, movie.pictures[index].0);
        assert_eq!(
            tairix_fuzzseed::first_difference(pixels, &expected[index]),
            None
        );
    }
}

#[test]
fn a_stream_too_large_for_the_ceiling_is_refused_at_its_open() {
    let movie = recording(3, 3, true, true);
    let needed = movie_bytes(SIZE.0, SIZE.1, 2, screen()).expect("a size a stream declares");
    let mut session = honest(screen());
    let mut owner = Owner::new(&movie.file, screen());
    let (_, refused) = opened(&mut session, &mut owner, needed - 1);
    assert_eq!(refused, Ok(Got::Refused(MovieRefusal::TooLarge)));
    let (_, admitted) = opened(&mut session, &mut owner, needed);
    assert_eq!(admitted, Ok(Got::Opened));
}

#[test]
fn pictures_due_before_the_instant_asked_are_decoded_and_not_drawn() {
    let movie = recording(10, 10, false, true);
    let mut session = honest(screen());
    let mut owner = Owner::new(&movie.file, screen());
    let (mut client, _) = opened(&mut session, &mut owner, ROOMY);
    let got = walk(&mut session, &mut owner, &mut client, 200 * MS);
    let skipped: Vec<u64> = got
        .iter()
        .filter_map(|unit| match unit {
            Got::Skipped(at) => Some(*at),
            _ => None,
        })
        .collect();
    assert_eq!(skipped, [0, 40 * MS, 80 * MS, 120 * MS, 160 * MS]);
    let pictures = drawn(&got);
    let (head, pixels) = pictures.first().expect("a picture past the instant");
    assert_eq!(head.at_ns, 200 * MS);
    assert!(
        *pixels == decoded(&movie.pictures)[5],
        "the skipped pictures were still decoded for those after them"
    );
}

/// Serves as the movie service does, but ends its stream on its `left`-th
/// frame: a worker that crashes part-way.
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

#[test]
fn a_replaced_worker_comes_back_at_the_latest_keyframe_in_a_ring_of_its_own() {
    let movie = recording(15, 5, true, true);
    let mut launches = 0;
    let mut session = supervised(screen(), move |ring| {
        launches += 1;
        Dying {
            inner: MovieDecodeService::new(Some(ring)),
            left: if launches == 1 { 12 } else { usize::MAX },
        }
    });
    let first_ring = session.share().cloned().expect("a ring");
    let mut owner = Owner::new(&movie.file, screen());
    let (mut client, opened) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(opened, Ok(Got::Opened));
    let got = walk(&mut session, &mut owner, &mut client, 0);
    let ring = session.share().cloned().expect("a ring");
    assert!(
        !Arc::ptr_eq(&first_ring, &ring),
        "the replacement draws into a ring of its own"
    );
    assert_eq!(got.last(), Some(&Got::Ended(Ending::Complete)));
    let resumed = got
        .iter()
        .position(|unit| *unit == Got::Resumed)
        .expect("the worker was replaced");
    let before = drawn(&got[..resumed]);
    let after = drawn(&got[resumed..]);
    let (last, _) = before.last().expect("a picture before the failure");
    let (again, _) = after.first().expect("a picture after it");
    assert!(again.keyframe && again.at_ns <= last.at_ns);
    assert_eq!(again.at_ns % (5 * PICTURE_NS), 0, "a keyframe's time");
    let (end, _) = after.last().expect("the last picture");
    assert_eq!(end.at_ns, movie.pictures.last().expect("pictures").0);
}

#[test]
fn a_movie_that_fails_its_worker_twice_at_one_place_is_given_up() {
    let movie = recording(5, 5, false, true);
    let mut session = supervised(screen(), |ring| Dying {
        inner: MovieDecodeService::new(Some(ring)),
        left: 1,
    });
    let mut owner = Owner::new(&movie.file, screen());
    let (_, opened) = opened(&mut session, &mut owner, ROOMY);
    assert_eq!(opened, Err(MovieDecodeError::CannotResume));
}

/// A frame `build` writes.
fn frame(build: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut out = Writer::new();
    build(&mut out);
    out.finish()
}

/// An honest answer to the open of a silent movie of [`SIZE`].
fn honest_opened() -> Vec<u8> {
    frame(|out| {
        out.u8(FROM_OPENED);
        out.u8(1);
        out.u64(1_000 * MS);
        out.u8(1);
        out.u32(SIZE.0);
        out.u32(SIZE.1);
        out.u8(0);
        out.raw(&[0; CHANNEL_MAP_WIRE_LEN]);
    })
}

/// A client whose open of a 1 MiB file has been sent, and, with `open`,
/// answered and a next sent.
fn asking(open: bool) -> MovieDecodeClient {
    let mut client = MovieDecodeClient::new();
    client.open(1 << 20, ROOMY, screen(), SEED).expect("free");
    client.sent();
    if open {
        assert_eq!(client.on_frame(&honest_opened()), Ok(MovieEvent::Opened));
        client.next(0).expect("idle");
        client.sent();
    }
    client
}

/// A picture's head naming `slot`, at `size`.
fn picture(slot: u8, (width, height): (u32, u32)) -> Vec<u8> {
    frame(|out| {
        encode_picture(
            out,
            PictureHead {
                at_ns: 0,
                keyframe: true,
                slot,
                width,
                height,
            },
        );
    })
}

#[test]
fn answers_no_honest_worker_gives_are_not_believed() {
    let unbelievable = Err(MovieDecodeError::Unbelievable);
    let need = |offset: u64, len: u32| {
        frame(|out| {
            out.u8(FROM_NEED);
            out.u64(offset);
            out.u32(len);
        })
    };
    assert_eq!(asking(false).on_frame(&picture(0, SIZE)), unbelievable);
    assert_eq!(asking(false).on_frame(&need(1, 4096)), unbelievable);
    assert_eq!(asking(false).on_frame(&need(0, 0)), unbelievable);
    let mut empty = honest_opened();
    empty[11..15].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(asking(false).on_frame(&empty), unbelievable, "no width");
    let refused = frame(|out| {
        out.u8(FROM_REFUSED);
        out.u8(REFUSED_NOT_OPEN);
        out.u8(0);
        out.u32(0);
    });
    assert_eq!(asking(false).on_frame(&refused), unbelievable);
    let past = u8::try_from(RING_SLOTS).expect("a slot number");
    assert_eq!(
        asking(true).on_frame(&picture(past, SIZE)),
        unbelievable,
        "past the ring"
    );
    let wide = (SIZE.0 + 1, SIZE.1);
    assert_eq!(
        asking(true).on_frame(&picture(0, wide)),
        unbelievable,
        "wider than a slot"
    );
    let tall = (SIZE.0, SIZE.1 + 1);
    assert_eq!(
        asking(true).on_frame(&picture(0, tall)),
        unbelievable,
        "taller than a slot"
    );
    let empty = (0, SIZE.1);
    assert_eq!(
        asking(true).on_frame(&picture(0, empty)),
        unbelievable,
        "no width"
    );
    let sound = frame(|out| {
        out.u8(FROM_SOUND);
        out.u64(0);
        out.u32(1);
        out.raw(&[0; 8]);
    });
    assert_eq!(
        asking(true).on_frame(&sound),
        unbelievable,
        "sound to a next"
    );
    let mut client = asking(true);
    assert!(matches!(
        client.on_frame(&picture(0, SIZE)),
        Ok(MovieEvent::Picture(_))
    ));
    client.next(0).expect("two slots are free");
    client.sent();
    assert_eq!(
        client.on_frame(&picture(0, SIZE)),
        unbelievable,
        "a slot the owner holds"
    );
}

/// What a service emits.
struct Emitted(Vec<Vec<u8>>);

impl FrameOut for Emitted {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

#[test]
fn a_next_with_every_slot_held_is_refused_on_either_side() {
    let mut client = asking(true);
    for slot in 0..u8::try_from(RING_SLOTS).expect("a slot number") {
        if slot > 0 {
            client.next(0).expect("a slot is free");
            client.sent();
        }
        assert!(matches!(
            client.on_frame(&picture(slot, SIZE)),
            Ok(MovieEvent::Picture(_))
        ));
    }
    assert_eq!(client.next(0), Err(MovieDecodeError::OutOfTurn));
    let ring = HeapShare::new(screen().ring_bytes()).expect("a ring");
    let mut service = MovieDecodeService::new(Some(ring));
    let request = frame(|out| {
        out.u8(TO_NEXT);
        out.u64(0);
        out.u8(ALL_SLOTS);
    });
    let mut emitted = Emitted(Vec::new());
    assert_eq!(
        service.handle(&request, &mut emitted),
        SessionStep::Continue
    );
    let (kind, code, detail) = MovieRefusal::MalformedRequest.to_wire();
    let refused = frame(|out| {
        out.u8(FROM_REFUSED);
        out.u8(kind);
        out.u8(code);
        out.u32(detail);
    });
    assert_eq!(emitted.0, [refused]);
}

/// Watch `movie` to its end through `link`, handing each slot back once its
/// picture is drawn.
fn watched(movie: &[u8], link: &mut impl crate::testing::Link) -> Result<(), MovieDecodeError> {
    let mut client = MovieDecodeClient::new();
    client.open(movie.len() as u64, ROOMY, screen(), SEED)?;
    loop {
        if let Some(request) = client.outgoing() {
            link.request(request);
            client.sent();
        }
        let frame = link.frame().ok_or(MovieDecodeError::Unbelievable)?;
        match client.on_frame(frame)? {
            MovieEvent::Need { offset, len } => {
                let start = usize::try_from(offset).expect("small");
                client.supply(&movie[start..start + len])?;
            }
            MovieEvent::Opened | MovieEvent::Skipped { .. } => client.next(0)?,
            MovieEvent::Picture(head) => {
                client.release(head.slot)?;
                client.next(0)?;
            }
            MovieEvent::Ended(_) => return Ok(()),
            other => panic!("a watch answered {other:?}"),
        }
    }
}

#[test]
fn a_movie_this_side_has_no_memory_for_is_refused_as_that() {
    let movie = recording(3, 3, false, true);
    let ring = HeapShare::new(screen().ring_bytes()).expect("a ring");
    let mut recorder = crate::testing::Recorder::new(MovieDecodeService::new(Some(ring)));
    watched(&movie.file, &mut recorder).expect("an honest watch");
    tairix_fuzzseed::meter::refused_at_each(
        || watched(&movie.file, &mut crate::testing::Replay::of(&recorder.kept)),
        |failure| *failure == MovieDecodeError::OutOfMemory,
    );
}

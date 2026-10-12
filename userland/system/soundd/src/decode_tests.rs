use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::audio::{ChannelMap, SampleFormat};
use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_log::DiscardSink;
use tairix_sandbox::audiodecode::{session_bounds, AudioDecodeService};
use tairix_sandbox::loopback::{LoopbackSession, LoopbackSessionLauncher};
use tairix_sandbox::session::{
    FrameOut, SessionDescriptors, SessionService, SessionStep, SessionTransport,
};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_sound::SoundFormat;
use tairix_soundtheme::{MAX_CUE_SAMPLES, MAX_CUE_SECONDS};

use super::{Decoded, Decoder, DECODE_BUDGET_NS};
use crate::testkit::{chime, flac, flac_of};

fn le_bytes(samples: &[i32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| i16::try_from(s).expect("16-bit").to_le_bytes())
        .collect()
}

/// A worker that is either the real decoder or one that answers every
/// request with nonsense.
enum Worker {
    Real(Box<AudioDecodeService>),
    Garbage,
}

impl SessionService for Worker {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        match self {
            Self::Real(service) => service.handle(request, out),
            Self::Garbage => {
                let _ = out.frame(&[0xEE; 12]);
                SessionStep::Continue
            }
        }
    }
}

type TestDecoder<F> = Decoder<LoopbackSessionLauncher<F>, DiscardSink>;

const SEED: HashSeed = HashSeed::from_words(0x5344_0001, 0x5344_0002);

fn decoder<F: FnMut() -> Worker>(factory: F) -> TestDecoder<F> {
    let bounds = session_bounds().expect("bounds");
    Decoder::new(
        SupervisedSession::new(LoopbackSessionLauncher::new(factory), bounds, DiscardSink),
        || Some(SEED),
    )
}

fn real() -> Worker {
    Worker::Real(Box::new(AudioDecodeService::new()))
}

/// Turns a drive takes before it is taken to have stalled.
const ROUNDS: usize = 1_000;

/// Drive the decode to its end at `now`, a turn per exchange as the serve
/// loop takes them, or `None` if it does not end.
fn run<F: FnMut() -> Worker>(decoder: &mut TestDecoder<F>, now: u64) -> Option<(u64, Decoded)> {
    for _ in 0..ROUNDS {
        if let Some(done) = decoder.pump(now) {
            return Some(done);
        }
        decoder.on_ready(now, true, true);
    }
    None
}

/// A pipe's room in one write: less than a supply of the decoder's largest
/// read and its header, as on a machine.
const PIPE_ROOM: usize = tairix_sound::MAX_READ;

/// What the worker's pipes hold, read by the test as a wait-set would.
#[derive(Default)]
struct PipeState {
    /// Answer bytes the owner has not read.
    unread: usize,
    /// Reads of an empty pipe, each of which parks its caller on a machine.
    parked: usize,
}

/// The real worker behind a model of the kernel's pipe pair: a write takes at
/// most [`PIPE_ROOM`], and a read of an empty pipe is counted rather than
/// answered.
struct ModelPipe {
    worker: LoopbackSession<Worker>,
    answers: Vec<u8>,
    state: Rc<RefCell<PipeState>>,
}

impl SessionTransport for ModelPipe {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.answers.is_empty() {
            self.state.borrow_mut().parked += 1;
            return Err(Errno::WouldBlock);
        }
        let take = buf.len().min(self.answers.len());
        buf[..take].copy_from_slice(&self.answers[..take]);
        self.answers.drain(..take);
        self.state.borrow_mut().unread = self.answers.len();
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        let taken = buf.len().min(PIPE_ROOM);
        self.worker.write(&buf[..taken])?;
        let mut chunk = [0u8; 4096];
        while let Ok(read @ 1..) = self.worker.read(&mut chunk) {
            self.answers.extend_from_slice(&chunk[..read]);
        }
        self.state.borrow_mut().unread = self.answers.len();
        Ok(taken)
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

struct ModelLauncher(Rc<RefCell<PipeState>>);

impl SessionLauncher for ModelLauncher {
    type Transport = ModelPipe;

    fn launch(&mut self) -> Result<ModelPipe, Errno> {
        Ok(ModelPipe {
            worker: LoopbackSession::new(real()),
            answers: Vec::new(),
            state: Rc::clone(&self.0),
        })
    }
}

/// Stereo white noise, which FLAC cannot shrink.
fn noise(frames: usize) -> Vec<i32> {
    let mut state = 0x2545_F491_u32;
    (0..2 * frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [low, high, ..] = state.to_le_bytes();
            i32::from(i16::from_le_bytes([low, high]))
        })
        .collect()
}

/// A file larger than the pipe is supplied across several writes, and the
/// decode reads only once the worker has answered: a read of an empty pipe
/// with a request still part-written parks the service for ever.
#[test]
fn a_sound_larger_than_the_pipe_is_never_read_ahead_of_its_answer() {
    let samples = noise(48_000);
    let bytes = flac(&samples);
    assert!(bytes.len() > 2 * PIPE_ROOM, "{} bytes", bytes.len());
    let state = Rc::new(RefCell::new(PipeState::default()));
    let bounds = session_bounds().expect("bounds");
    let session = SupervisedSession::new(ModelLauncher(Rc::clone(&state)), bounds, DiscardSink);
    let mut decoder = Decoder::new(session, || Some(SEED));
    decoder
        .begin(0, 5, bytes, Some(SoundFormat::Flac))
        .expect("idle");
    let mut done = None;
    for _ in 0..ROUNDS {
        done = decoder.pump(1);
        if done.is_some() {
            break;
        }
        let (_, read, write) = decoder.members();
        let readable = read && state.borrow().unread > 0;
        decoder.on_ready(1, readable, write);
    }
    let Some((5, Decoded::Pcm(pcm))) = done else {
        panic!("the sound decodes: {done:?}");
    };
    assert_eq!(pcm.bytes, le_bytes(&samples));
    assert_eq!(state.borrow().parked, 0, "no read of an empty pipe");
}

#[test]
fn a_sound_decodes_whole_to_exactly_its_samples() {
    let samples = chime(10_000);
    let mut decoder = decoder(real);
    decoder
        .begin(0, 7, flac(&samples), Some(SoundFormat::Flac))
        .expect("idle");
    assert!(decoder.is_busy());
    let Some((7, Decoded::Pcm(pcm))) = run(&mut decoder, 1) else {
        panic!("the sound decodes");
    };
    assert_eq!(pcm.format, SampleFormat::S16);
    assert_eq!(pcm.rate.hz(), 48_000);
    assert_eq!(pcm.channel_map, ChannelMap::STEREO);
    assert_eq!(pcm.frames(), 10_000);
    assert_eq!(pcm.bytes, le_bytes(&samples));
    assert!(!decoder.is_busy());
}

/// A stream that does not declare its length grows as it decodes, rather
/// than reserving the whole bound for a sound a fraction of it.
#[test]
fn a_sound_of_undeclared_length_grows_as_it_decodes() {
    let samples = chime(10_000);
    let mut bytes = flac(&samples);
    // STREAMINFO's 36-bit total is the low bits of the eight bytes after
    // the frame sizes; zero there says the length is unknown.
    let word = u64::from_be_bytes(bytes[18..26].try_into().expect("eight bytes"));
    assert_eq!(
        word & ((1 << 36) - 1),
        10_000,
        "the total is where it is read"
    );
    bytes[21] &= 0xF0;
    bytes[22..26].fill(0);
    let mut decoder = decoder(real);
    decoder
        .begin(0, 9, bytes, Some(SoundFormat::Flac))
        .expect("idle");
    let Some((9, Decoded::Pcm(pcm))) = run(&mut decoder, 1) else {
        panic!("the sound decodes");
    };
    assert_eq!(pcm.bytes, le_bytes(&samples));
    assert!(
        pcm.bytes.capacity() <= 2 * pcm.bytes.len(),
        "{} bytes held for a {}-byte sound",
        pcm.bytes.capacity(),
        pcm.bytes.len()
    );
}

#[test]
fn one_sound_at_a_time_and_the_next_on_the_same_worker() {
    let mut decoder = decoder(real);
    decoder
        .begin(0, 1, flac(&chime(500)), Some(SoundFormat::Flac))
        .expect("idle");
    assert!(
        decoder
            .begin(0, 2, flac(&chime(600)), Some(SoundFormat::Flac))
            .is_err(),
        "a second sound waits for the first"
    );
    assert!(matches!(run(&mut decoder, 1), Some((1, Decoded::Pcm(_)))));
    decoder
        .begin(2, 2, flac(&chime(600)), Some(SoundFormat::Flac))
        .expect("idle again");
    let Some((2, Decoded::Pcm(pcm))) = run(&mut decoder, 3) else {
        panic!("the second sound decodes");
    };
    assert_eq!(pcm.frames(), 600);
}

#[test]
fn a_file_that_is_not_its_format_is_refused() {
    let mut decoder = decoder(real);
    decoder
        .begin(
            0,
            3,
            b"RIFF this is no FLAC at all".to_vec(),
            Some(SoundFormat::Flac),
        )
        .expect("idle");
    assert_eq!(run(&mut decoder, 1), Some((3, Decoded::Refused)));
}

/// Decode `channels` channels of `frames` silent frames, answering whether
/// the sound was admitted.
fn admits(channels: u8, frames: usize) -> bool {
    let bytes = flac_of(channels, &vec![0i32; frames * usize::from(channels)]);
    assert!(
        bytes.len() < 64 * 1024,
        "silence compresses to almost nothing"
    );
    let mut decoder = decoder(real);
    decoder
        .begin(0, 4, bytes, Some(SoundFormat::Flac))
        .expect("idle");
    match run(&mut decoder, 1) {
        Some((4, Decoded::Pcm(_))) => true,
        Some((4, Decoded::Refused)) => false,
        other => panic!("{other:?}"),
    }
}

/// A FLAC holds a long silence in a few hundred bytes, so the byte bound
/// alone does not bound a cue: a sound one frame longer than a cue may sound
/// is refused, and so is one of more samples than a cue may hold, which is
/// the tighter of the two for a sound of many channels.
#[test]
fn a_sound_past_either_cue_bound_is_refused_however_few_its_bytes() {
    let seconds = MAX_CUE_SECONDS as usize * 48_000;
    assert!(admits(2, seconds), "exactly as long as a cue may sound");
    assert!(!admits(2, seconds + 1));
    let samples = MAX_CUE_SAMPLES / 8;
    assert!(
        samples < seconds,
        "eight channels meet the sample bound first"
    );
    assert!(admits(8, samples));
    assert!(!admits(8, samples + 1));
}

/// A worker that answers nonsense is contained and the sound refused; the
/// next sound is decoded by a fresh worker once the pacing allows one.
#[test]
fn a_worker_breaking_the_protocol_refuses_its_sound_and_is_replaced() {
    let mut launches = 0;
    let mut decoder = decoder(move || {
        launches += 1;
        if launches == 1 {
            Worker::Garbage
        } else {
            real()
        }
    });
    decoder
        .begin(0, 6, flac(&chime(300)), Some(SoundFormat::Flac))
        .expect("idle");
    assert_eq!(run(&mut decoder, 1), Some((6, Decoded::Refused)));
    decoder
        .begin(2, 7, flac(&chime(300)), Some(SoundFormat::Flac))
        .expect("idle");
    assert_eq!(run(&mut decoder, 3), None, "the replacement is paced");
    let restart = decoder.deadline().expect("a restart is due");
    assert!(matches!(
        run(&mut decoder, restart),
        Some((7, Decoded::Pcm(_)))
    ));
}

#[test]
fn a_decode_past_its_budget_is_refused() {
    let mut decoder = decoder(real);
    decoder
        .begin(10, 8, flac(&chime(300)), Some(SoundFormat::Flac))
        .expect("idle");
    assert_eq!(
        decoder.deadline(),
        Some(0),
        "a sound waiting for its first worker wants a turn at once"
    );
    assert_eq!(
        decoder.pump(10 + DECODE_BUDGET_NS),
        Some((8, Decoded::Refused))
    );
}

/// `samples` as an Ogg FLAC link of serial `serial`, at `rate`.
fn ogg_link(rate: u32, samples: &[i32], serial: u32) -> Vec<u8> {
    use tairix_sound::flac_encode::{encode, Options, Params};
    let params = Params {
        rate,
        channels: 2,
        bits: 16,
    };
    encode(params, samples, Options::default())
        .expect("encodes")
        .finish_ogg(serial, 4000)
}

/// A cue chained in links of one shape is heard whole, not cut at its first
/// link; one whose links change shape cannot be one sound and is refused.
#[test]
fn a_chained_cue_is_read_through_its_links_of_one_shape() {
    let (first, second) = (chime(3_000), chime(2_000));
    let chained = [ogg_link(48_000, &first, 1), ogg_link(48_000, &second, 2)].concat();
    let mut decoder = decoder(real);
    decoder
        .begin(0, 11, chained, Some(SoundFormat::Ogg))
        .expect("idle");
    let Some((11, Decoded::Pcm(pcm))) = run(&mut decoder, 1) else {
        panic!("the chained cue decodes");
    };
    assert_eq!(pcm.bytes, le_bytes(&[first, second].concat()));
    let reshaped = [
        ogg_link(48_000, &chime(1_000), 3),
        ogg_link(44_100, &chime(1_000), 4),
    ]
    .concat();
    decoder
        .begin(2, 12, reshaped, Some(SoundFormat::Ogg))
        .expect("idle");
    assert_eq!(run(&mut decoder, 3), Some((12, Decoded::Refused)));
}

/// A file named by no format is decoded as whichever it is, as a user's own
/// sound is: here a WAV, where the shipped theme is FLAC throughout.
#[test]
fn a_file_of_no_stated_format_is_decoded_as_what_it_is() {
    let mut decoder = decoder(real);
    decoder
        .begin(0, 13, crate::testkit::wav(&chime(400)), None)
        .expect("idle");
    match run(&mut decoder, 1) {
        Some((13, Decoded::Pcm(pcm))) => assert_eq!(pcm.frames(), 400),
        other => panic!("not decoded: {other:?}"),
    }
}

/// A worker the decoder cannot key is the machine's failure, not the sound's:
/// the sound is never judged, so it is not one to remember as undecodable.
#[test]
fn a_decode_that_cannot_be_keyed_fails_without_judging_the_sound() {
    let bounds = session_bounds().expect("bounds");
    let mut decoder: TestDecoder<_> = Decoder::new(
        SupervisedSession::new(LoopbackSessionLauncher::new(real), bounds, DiscardSink),
        || None,
    );
    assert!(decoder
        .begin(0, 3, flac(&chime(100)), Some(SoundFormat::Flac))
        .is_ok());
    assert_eq!(
        run(&mut decoder, 0),
        Some((3, Decoded::Failed(Errno::EntropyNotReady)))
    );
}

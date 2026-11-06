//! The tag reader over the real decoder behind a model of the kernel's pipes,
//! driven as a host's wait-set drives it.

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_hash::HashSeed;
use tairix_log::DiscardSink;
use tairix_sandbox::audiodecode::{session_bounds, AudioDecodeService};
use tairix_sandbox::loopback::{PipeGauge, PipeSession, PipeSessionLauncher, PIPE_ROOM};
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_sandbox::supervise::{SessionLauncher, SupervisedSession};
use tairix_sound::flac_encode::{encode, Options, Params};
use tairix_test_audio_wire as wire;

use super::{TagReader, Tags, BUDGET_NS};

const SEED: HashSeed = HashSeed::from_words(0x5441_4701, 0x5441_4702);

/// Rounds a drive takes before it is taken to have stalled.
const ROUNDS: usize = 1_000;

/// A worker that decodes, one that takes every request and answers none, or
/// one that ends its session at the first.
enum Worker {
    Decodes(Box<AudioDecodeService>),
    Hangs,
    Crashes,
}

impl SessionService for Worker {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        match self {
            Self::Decodes(service) => service.handle(request, out),
            Self::Hangs => SessionStep::Continue,
            Self::Crashes => SessionStep::Finished,
        }
    }
}

fn decodes() -> Worker {
    Worker::Decodes(Box::new(AudioDecodeService::new()))
}

type Reader<F> = TagReader<PipeSessionLauncher<F>, DiscardSink>;

/// A reader whose workers `factory` builds behind the model pipes, keying
/// each one's cache by `seed`, and the gauge of those pipes.
fn reader_keyed<F: FnMut() -> Worker>(
    factory: F,
    seed: fn() -> Option<HashSeed>,
) -> (Reader<F>, PipeGauge) {
    let launcher = PipeSessionLauncher::new(factory);
    let pipes = launcher.gauge();
    let bounds = session_bounds().expect("bounds");
    let session = SupervisedSession::new(launcher, bounds, DiscardSink);
    (TagReader::new(session, seed), pipes)
}

fn reader<F: FnMut() -> Worker>(factory: F) -> (Reader<F>, PipeGauge) {
    reader_keyed(factory, || Some(SEED))
}

/// A launcher refusing its first `refusals` launches, then launching workers
/// behind the model pipes.
struct Refusing<F> {
    pipes: PipeSessionLauncher<F>,
    refusals: usize,
}

impl<F: FnMut() -> Worker> SessionLauncher for Refusing<F> {
    type Transport = PipeSession<Worker>;

    fn launch(&mut self) -> Result<PipeSession<Worker>, Errno> {
        if self.refusals > 0 {
            self.refusals -= 1;
            return Err(Errno::OutOfMemory);
        }
        self.pipes.launch()
    }
}

/// `file` read as a descriptor reads it: short at its end.
fn bytes_of(file: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<usize, Errno> + '_ {
    move |offset: u64, buf: &mut [u8]| {
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let held = file.get(start..).unwrap_or(&[]);
        let take = held.len().min(buf.len());
        buf[..take].copy_from_slice(&held[..take]);
        Ok(take)
    }
}

/// Drive `reader` from `now` until its reading ends, as a host parked on a
/// wait-set would: the reply pipe reported only while watched and readable,
/// the request pipe only while watched, and with neither the clock moved on to
/// the reader's deadline. Answers how the reading ended, and when.
fn drive<L: SessionLauncher>(
    reader: &mut TagReader<L, DiscardSink>,
    pipes: &PipeGauge,
    read: &mut impl FnMut(u64, &mut [u8]) -> Result<usize, Errno>,
    mut now: u64,
) -> (Tags, u64) {
    for _ in 0..ROUNDS {
        if let Some(tags) = reader.pump(now, read) {
            return (tags, now);
        }
        let (_, watch_read, watch_write) = reader.members();
        let readable = watch_read && pipes.readable();
        if readable || watch_write {
            reader.on_ready(now, readable, watch_write);
        } else {
            let at = reader.deadline().expect("a wait with nothing to end it");
            assert!(at > now, "a deadline that passed was not acted on");
            now = at;
        }
    }
    panic!("the reading never ended");
}

/// A short FLAC file titled `title`.
fn titled(title: &str) -> Vec<u8> {
    let samples: Vec<i32> = (0..4_000).map(|at| at % 64 * 100 - 3_200).collect();
    let params = Params {
        rate: 48_000,
        channels: 2,
        bits: 16,
    };
    let mut writer = encode(params, &samples, Options::default()).expect("encodes");
    let field = format!("TITLE={title}");
    writer.comments("tests", &[field.as_str()]);
    writer.finish()
}

/// Whether `tags` read a file titled `title`.
fn read_as(tags: &Tags, title: &str) -> bool {
    matches!(tags, Tags::Read(_, metadata) if metadata.tags.iter().any(|tag| tag.value == title))
}

/// A track's tags are read when readiness arrives only as a wait-set reports
/// it. A reader that parked holding the bytes it was asked for would never
/// wake: the worker waits on those bytes, and nothing would be queued to its
/// pipe.
#[test]
fn a_tracks_tags_are_read_under_the_readiness_a_wait_set_reports() {
    let file = titled("Dawn Chorus");
    let (mut reader, pipes) = reader(decodes);
    reader.begin(0, file.len() as u64);
    let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    assert!(read_as(&tags, "Dawn Chorus"), "{tags:?}");
    assert_eq!(at, 0, "read with no deadline passing");
    let Tags::Read(info, _) = tags else {
        panic!("unread");
    };
    assert_eq!(info.rate.hz(), 48_000);
    assert_eq!(info.frames, Some(2_000));
    assert_eq!(pipes.parked(), 0, "no read of an empty pipe");
}

/// A file whose first window is larger than the pipe takes several writes to
/// hand over, each made when the pipe has room.
#[test]
fn a_file_larger_than_the_pipe_is_read() {
    let frames: u32 = 48_000;
    let mut file = wire::wav_header(48_000, 2, frames * 4).to_vec();
    file.resize(file.len() + usize::try_from(frames * 4).expect("small"), 0);
    assert!(file.len() > 2 * PIPE_ROOM, "{} bytes", file.len());
    let (mut reader, pipes) = reader(decodes);
    reader.begin(0, file.len() as u64);
    let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    let Tags::Read(info, _) = tags else {
        panic!("unread");
    };
    assert_eq!(info.frames, Some(u64::from(frames)));
    assert_eq!(at, 0, "read with no deadline passing");
    assert_eq!(pipes.parked(), 0, "no read of an empty pipe");
}

/// A worker that never answers is killed when the budget runs out, its file
/// left unread, and the next file is read by a replacement once the pacing
/// allows one.
#[test]
fn a_hung_worker_is_killed_at_the_budget_and_replaced() {
    let mut launches = 0;
    let (mut reader, pipes) = reader(move || {
        launches += 1;
        if launches == 1 {
            Worker::Hangs
        } else {
            decodes()
        }
    });
    let file = titled("Late");
    reader.begin(0, file.len() as u64);
    let unread = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    assert_eq!(unread, (Tags::Unread, BUDGET_NS), "at the budget");
    assert_eq!(pipes.ended(), 1, "the hung worker is killed");
    reader.begin(BUDGET_NS, file.len() as u64);
    let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&file), BUDGET_NS);
    assert!(read_as(&tags, "Late"), "{tags:?}");
    assert!(at > BUDGET_NS, "the replacement is paced");
    assert_eq!((pipes.launched(), pipes.ended()), (2, 1));
}

/// A reading given up part-way through a request kills its worker, which
/// would otherwise answer the next file with this one's frames, as does a
/// file begun over one still being read; a reading given up once it ended
/// leaves the worker serving.
#[test]
fn a_reading_abandoned_mid_request_kills_its_worker() {
    let (mut reader, pipes) = reader(decodes);
    let (first, second) = (titled("First"), titled("Second"));
    reader.begin(0, first.len() as u64);
    assert_eq!(reader.pump(0, &mut bytes_of(&first)), None);
    assert!(pipes.readable(), "the worker asked for bytes");
    reader.abandon(0);
    assert_eq!(pipes.ended(), 1, "killed part-way");

    reader.begin(0, second.len() as u64);
    let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&second), 0);
    assert!(read_as(&tags, "Second"), "{tags:?}");
    reader.abandon(at);
    assert_eq!(pipes.ended(), 1, "nothing was outstanding");

    reader.begin(at, first.len() as u64);
    assert_eq!(reader.pump(at, &mut bytes_of(&first)), None);
    reader.begin(at, second.len() as u64);
    assert_eq!(pipes.ended(), 2, "begun over a reading part-way");
    let (tags, _) = drive(&mut reader, &pipes, &mut bytes_of(&second), at);
    assert!(read_as(&tags, "Second"), "{tags:?}");
    assert_eq!(pipes.launched(), 3);
}

/// A file no decoder reads is unread, and the worker that refused it reads
/// the next: a refusal is an answer, not a failure.
#[test]
fn a_file_the_decoder_refuses_is_unread_and_its_worker_kept() {
    let (mut reader, pipes) = reader(decodes);
    let words = b"not a sound at all, only words";
    reader.begin(0, words.len() as u64);
    let refused = drive(&mut reader, &pipes, &mut bytes_of(words), 0);
    assert_eq!(refused, (Tags::Unread, 0));
    let file = titled("Kept");
    reader.begin(0, file.len() as u64);
    let (tags, _) = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    assert!(read_as(&tags, "Kept"), "{tags:?}");
    assert_eq!((pipes.launched(), pipes.ended()), (1, 0));
}

/// Bytes that cannot be read leave the file unread, refused by the worker,
/// which stays in step for the next.
#[test]
fn a_file_whose_bytes_cannot_be_read_is_unread_and_its_worker_kept() {
    let (mut reader, pipes) = reader(decodes);
    let file = titled("Lost");
    reader.begin(0, file.len() as u64);
    let mut failing = |_: u64, _: &mut [u8]| Err::<usize, Errno>(Errno::DeviceFault);
    let failed = drive(&mut reader, &pipes, &mut failing, 0);
    assert_eq!(failed, (Tags::Unread, 0));
    reader.begin(0, file.len() as u64);
    let (tags, _) = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    assert!(read_as(&tags, "Lost"), "{tags:?}");
    assert_eq!((pipes.launched(), pipes.ended()), (1, 0));
}

/// A reader with no seed to key a worker's cache by reads nothing, rather
/// than letting a file choose which of its pages collide.
#[test]
fn a_reader_with_no_seed_reads_nothing() {
    let (mut reader, pipes) = reader_keyed(decodes, || None);
    let file = titled("Unkeyed");
    reader.begin(0, file.len() as u64);
    let unkeyed = drive(&mut reader, &pipes, &mut bytes_of(&file), 0);
    assert_eq!(unkeyed, (Tags::Unread, 0));
}

/// A run of decoder crashes holds the next replacement back past a file's
/// budget, and a good file begun then is still read once a worker comes up:
/// the wait costs it time, never its tags. Regression: the budget ran from the
/// file's begin, so it was spent waiting for the worker and the file was left
/// unread.
#[test]
fn a_replacement_held_back_past_the_budget_costs_a_file_time_not_its_tags() {
    const CRASHES: usize = 8;
    let mut launches = 0;
    let (mut reader, pipes) = reader(move || {
        launches += 1;
        if launches <= CRASHES {
            Worker::Crashes
        } else {
            decodes()
        }
    });
    let file = titled("Patient");
    let mut now = 0;
    for _ in 0..CRASHES {
        reader.begin(now, file.len() as u64);
        let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&file), now);
        assert_eq!(tags, Tags::Unread, "a file that broke its decoder");
        now = at;
    }
    reader.begin(now, file.len() as u64);
    assert_eq!(
        reader.pump(now, &mut bytes_of(&file)),
        None,
        "no worker yet"
    );
    let restart = reader.deadline().expect("a replacement is due");
    assert!(restart > now + BUDGET_NS, "held back past the budget");
    let (tags, at) = drive(&mut reader, &pipes, &mut bytes_of(&file), now);
    assert!(read_as(&tags, "Patient"), "{tags:?}");
    assert_eq!(at, restart, "read once the worker came up");
    assert_eq!(pipes.launched(), CRASHES + 1);
}

/// A worker that cannot be launched leaves its file unread at once, as there
/// is no decoder to wait for, and the next file is read by one the pacing then
/// allows.
#[test]
fn a_worker_that_cannot_be_launched_leaves_its_file_unread() {
    let pipes = PipeSessionLauncher::new(decodes);
    let gauge = pipes.gauge();
    let launcher = Refusing { pipes, refusals: 1 };
    let session = SupervisedSession::new(launcher, session_bounds().expect("bounds"), DiscardSink);
    let mut reader = TagReader::new(session, || Some(SEED));
    let file = titled("Second Try");
    reader.begin(0, file.len() as u64);
    assert_eq!(reader.pump(0, &mut bytes_of(&file)), Some(Tags::Unread));
    reader.begin(0, file.len() as u64);
    let (tags, at) = drive(&mut reader, &gauge, &mut bytes_of(&file), 0);
    assert!(read_as(&tags, "Second Try"), "{tags:?}");
    assert!(at > 0, "the next launch is paced");
    assert_eq!(gauge.launched(), 1);
}

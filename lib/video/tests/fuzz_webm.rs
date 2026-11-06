//! Fuzz harness for the `WebM` demuxer: untrusted `WebM` bytes.
//!
//! Invariants, for every input:
//!
//! 1. Opening, reading every frame and seeking never panic.
//! 2. Every frame lies inside the file, holds at most `MAX_FRAME_BYTES`, and
//!    belongs to a track the file declares.
//! 3. Read through an input that answers "not yet" at random, each call asked
//!    again once its bytes are supplied, a file yields exactly what a direct
//!    read does: a call that meets missing bytes changes nothing it answers.
//! 4. A demux holds no more memory than its fixed bounds allow, and one refused
//!    its memory answers rather than aborting.
//! 5. A file the muxer finished whole yields exactly the frames it was given.
//!
//! Inputs are arbitrary bytes, the same behind a `WebM` header, and files the
//! muxer wrote from random recordings (finished, left as their writer died, or
//! cut short by a full disk), then mutated: bit flips, boundary bytes,
//! truncations, insertions, deletions, and all-ones size fields.

#![allow(
    clippy::cast_possible_truncation,
    reason = "the generators narrow lengths and counts they keep small"
)]

use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_fuzzseed::meter::{metered, refusing_after, Metered};
use tairix_fuzzseed::Prng;
use tairix_sound::{InputError, SoundInput, MAX_READ};
use tairix_video::webm::{
    DemuxError, Demuxer, Ending, Frame, Layout, Muxer, OpusChannels, OpusTrack, MAX_FRAME_BYTES,
};

#[global_allocator]
static ALLOC: Metered = Metered;

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 200;

/// Most frames one walk reads.
const MAX_WALK: usize = 2048;

/// A walk seeks once each this many frames.
const SEEK_EVERY: usize = 32;

/// The most a demux of these inputs may hold: its scratch at the largest
/// element read whole while it grows, the tracks' codec data, the index and
/// lace buffers, and the allocator's rounding.
const PEAK_BOUND: usize = 4 * MAX_READ + 256 * 1024;

const MS: u64 = 1_000_000;

/// Bytes a supplied page holds.
const PAGE: usize = 64;

/// A file the muxer writes; past `capacity` bytes a write puts down what fits
/// and is refused, as a full disk refuses it.
#[derive(Default)]
struct Disk {
    bytes: Vec<u8>,
    capacity: Option<usize>,
}

impl Target for Disk {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let end = start + bytes.len();
        let fits = self.capacity.map_or(end, |capacity| end.min(capacity));
        if fits > start {
            if self.bytes.len() < fits {
                self.bytes.resize(fits, 0);
            }
            self.bytes[start..fits].copy_from_slice(&bytes[..fits - start]);
        }
        if fits < end {
            Err(Errno::NoSpace)
        } else {
            Ok(())
        }
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| Errno::OutOfRange)?;
        self.bytes.resize(len, 0);
        Ok(())
    }
}

/// An input that holds some pages of its file from the start and is given
/// each page a read misses, as the sandbox's worker is: the read that missed
/// answers "not yet", and the same read succeeds when asked again.
struct Cache<'a> {
    file: &'a [u8],
    held: Vec<bool>,
}

impl<'a> Cache<'a> {
    fn new(file: &'a [u8], rng: &mut Prng) -> Self {
        let held = (0..file.len().div_ceil(PAGE))
            .map(|_| rng.below(2) == 0)
            .collect();
        Self { file, held }
    }
}

impl SoundInput for Cache<'_> {
    fn len(&self) -> u64 {
        self.file.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.file.len());
        let end = start.saturating_add(buf.len()).min(self.file.len());
        let pages = start / PAGE..end.div_ceil(PAGE);
        if !self.held[pages.clone()].iter().all(|&held| held) {
            self.held[pages].fill(true);
            return Err(InputError::Unavailable);
        }
        let mut file = self.file;
        file.read_at(offset, buf)
    }
}

/// What a walk through a file answered: a digest of its frames, how many, and
/// how it ended, or the refusal that stopped it.
#[derive(Debug, Eq, PartialEq)]
struct Walked {
    digest: u64,
    frames: usize,
    end: Result<Option<Ending>, DemuxError>,
}

/// `call`'s answer once the input holds what it reads.
fn retried<I: SoundInput + ?Sized, T>(
    input: &mut I,
    mut call: impl FnMut(&mut I) -> Result<T, DemuxError>,
) -> Result<T, DemuxError> {
    loop {
        match call(input) {
            Err(DemuxError::InputUnavailable) => {}
            answered => return answered,
        }
    }
}

/// Hold `frame` to the invariants: inside the file, within the frame bound,
/// of a declared track.
fn check(demuxer: &Demuxer, frame: &Frame, file_len: u64) {
    let end = frame.offset + u64::from(frame.len);
    assert!(end <= file_len, "a frame past the file's end: {frame:?}");
    assert!(
        frame.len <= MAX_FRAME_BYTES,
        "a frame past the bound: {frame:?}"
    );
    let declared = demuxer
        .tracks()
        .iter()
        .any(|track| track.number == frame.track);
    assert!(declared, "a frame of an undeclared track: {frame:?}");
}

/// `digest` with `frame` folded in.
fn folded(digest: u64, frame: &Frame) -> u64 {
    [
        frame.track,
        frame.timestamp_ns,
        frame.offset,
        u64::from(frame.len),
        u64::from(frame.keyframe),
        frame.discard_padding_ns.cast_unsigned(),
    ]
    .into_iter()
    .fold(digest, |digest, word| {
        tairix_fuzzseed::splitmix64(digest ^ word)
    })
}

/// Walk `input`'s frames, seeking to each of `targets` along the way, and hold
/// every frame to the invariants.
fn walk<I: SoundInput + ?Sized>(input: &mut I, targets: &[u64]) -> Walked {
    let mut walked = Walked {
        digest: 0,
        frames: 0,
        end: Ok(None),
    };
    let mut demuxer = match retried(input, Demuxer::open) {
        Ok(demuxer) => demuxer,
        Err(refused) => {
            walked.end = Err(refused);
            return walked;
        }
    };
    let file_len = input.len();
    let mut seeks = targets.iter();
    while walked.frames < MAX_WALK {
        let frame = match retried(input, |input| demuxer.next_frame(input)) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                walked.end = Ok(demuxer.ending());
                return walked;
            }
            Err(refused) => {
                walked.end = Err(refused);
                return walked;
            }
        };
        check(&demuxer, &frame, file_len);
        walked.digest = folded(walked.digest, &frame);
        walked.frames += 1;
        let target = if walked.frames.is_multiple_of(SEEK_EVERY) {
            seeks.next()
        } else {
            None
        };
        if let Some(&target) = target {
            match retried(input, |input| demuxer.seek(input, target)) {
                Ok(()) | Err(DemuxError::BadIndex) => {}
                Err(refused) => {
                    walked.end = Err(refused);
                    return walked;
                }
            }
        }
    }
    walked
}

/// Every invariant over `bytes`.
fn exercise(bytes: &[u8], rng: &mut Prng) {
    let targets: Vec<u64> = (0..8).map(|_| rng.next_u64() % (120_000 * MS)).collect();
    let (direct, metering) = metered(|| walk(&mut { bytes }, &targets));
    assert!(
        metering.peak <= PEAK_BOUND,
        "a demux held {} bytes past a bound of {PEAK_BOUND}",
        metering.peak
    );
    let mut cache = Cache::new(bytes, rng);
    assert_eq!(
        walk(&mut cache, &targets),
        direct,
        "a walk through the cache differs"
    );
    let grants = rng.below(64);
    refusing_after(grants, || walk(&mut { bytes }, &targets));
}

/// A frame pushed to the muxer.
#[derive(Clone, Debug)]
struct Pushed {
    video: bool,
    timestamp_ns: u64,
    keyframe: bool,
    payload: Vec<u8>,
}

fn pushed(rng: &mut Prng, video: bool, timestamp_ns: u64, keyframe: bool) -> Pushed {
    let mut payload = vec![0; 1 + rng.below(if video && keyframe { 1500 } else { 200 })];
    rng.fill(&mut payload);
    Pushed {
        video,
        timestamp_ns,
        keyframe,
        payload,
    }
}

/// A few seconds of video at random times, now and then a gap past a block's
/// reach, and with `sound` its packets, in timestamp order with video first
/// at a tie.
fn recording(rng: &mut Prng, sound: bool) -> Vec<Pushed> {
    let mut frames = Vec::new();
    let mut at = 0;
    for index in 0..rng.below(120) {
        let keyframe = index == 0 || rng.below(16) == 0;
        frames.push(pushed(rng, true, at, keyframe));
        at += rng.below(80) as u64 * MS + rng.below(1000) as u64;
        if rng.below(48) == 0 {
            at += 40_000 * MS;
        }
    }
    if sound {
        for packet in 0..=at / (20 * MS) {
            frames.push(pushed(rng, false, packet * 20 * MS, true));
        }
    }
    frames.sort_by_key(|frame| (frame.timestamp_ns, !frame.video));
    frames
}

fn random_layout(rng: &mut Prng, sound: bool) -> Layout {
    let channels = if rng.below(2) == 0 {
        OpusChannels::Mono
    } else {
        OpusChannels::Stereo
    };
    let opus = OpusTrack {
        channels,
        pre_skip: rng.next_u16(),
        input_rate: rng.next_u32(),
        output_gain: 0,
    };
    Layout {
        width: 1 + rng.below(1920) as u32,
        height: 1 + rng.below(1080) as u32,
        audio: sound.then_some(opus),
        indexed_keyframes: rng.below(8) as u32,
    }
}

/// A file the muxer wrote of a random recording: finished, left as its writer
/// died, or cut short by a full disk; with the frames it must yield where it
/// was finished whole.
fn muxed(rng: &mut Prng) -> (Vec<u8>, Option<Vec<Pushed>>) {
    let sound = rng.below(2) == 0;
    let layout = random_layout(rng, sound);
    let frames = recording(rng, sound);
    let mut disk = Disk::default();
    let mut muxer = Muxer::begin(&mut disk, &layout).expect("a layout of some size begins");
    if rng.below(4) == 0 {
        disk.capacity = Some(disk.bytes.len() + rng.below(16 * 1024));
    }
    let taken = frames.iter().all(|frame| {
        let taken = if frame.video {
            muxer.push_video(
                &mut disk,
                frame.timestamp_ns,
                frame.keyframe,
                &frame.payload,
            )
        } else {
            muxer.push_audio(&mut disk, frame.timestamp_ns, &frame.payload)
        };
        taken.is_ok()
    });
    if rng.below(4) == 0 {
        return (disk.bytes, None);
    }
    let end = frames.last().map_or(0, |frame| frame.timestamp_ns);
    let finished = muxer
        .finish(&mut disk, end)
        .expect("finishes in the space it has");
    let whole = taken && !finished.cut_short;
    (disk.bytes, whole.then_some(frames))
}

/// Assert the file the muxer finished whole yields `frames` and then ends.
fn yields_what_was_muxed(bytes: &[u8], frames: &[Pushed]) {
    let mut input = bytes;
    let mut demuxer = Demuxer::open(&mut input).expect("a muxed file opens");
    for (index, pushed) in frames.iter().enumerate() {
        let frame = demuxer
            .next_frame(&mut input)
            .expect("a muxed file reads")
            .unwrap_or_else(|| panic!("frame {index} is missing"));
        let track = if pushed.video { 1 } else { 2 };
        let want = (track, pushed.timestamp_ns / MS * MS, pushed.keyframe);
        assert_eq!(
            (frame.track, frame.timestamp_ns, frame.keyframe),
            want,
            "frame {index}"
        );
        let start = usize::try_from(frame.offset).expect("small");
        let payload = &bytes[start..start + frame.len as usize];
        assert_eq!(payload, &pushed.payload[..], "frame {index}'s payload");
    }
    assert_eq!(demuxer.next_frame(&mut input).expect("reads"), None);
    assert_eq!(demuxer.ending(), Some(Ending::Complete));
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "arbitrary_bytes_never_panic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let len = rng.below(512);
            let bytes = rng.noise(len);
            exercise(&bytes, &mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn garbage_behind_a_webm_header_never_panics() {
    // Random bytes essentially never open with an EBML header, so without
    // this the walk past the header would hardly ever be entered.
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "garbage_behind_a_webm_header_never_panics",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let sound = rng.below(2) == 0;
            let layout = random_layout(&mut rng, sound);
            let mut disk = Disk::default();
            Muxer::begin(&mut disk, &layout).expect("a layout of some size begins");
            let len = rng.below(1024);
            disk.bytes.extend_from_slice(&rng.noise(len));
            exercise(&disk.bytes, &mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn muxed_files_yield_the_frames_they_were_given() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "muxed_files_yield_the_frames_they_were_given",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let (bytes, whole) = muxed(&mut rng);
            if let Some(frames) = whole {
                yields_what_was_muxed(&bytes, &frames);
            }
            exercise(&bytes, &mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn mutated_muxed_files_never_panic() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "mutated_muxed_files_never_panic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let (pristine, _) = muxed(&mut rng);
            let bytes = rng.mutated(&pristine);
            exercise(&bytes, &mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

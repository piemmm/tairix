//! The sandboxed decode end to end, over in-process workers: what crosses is
//! exactly what an in-process decode gives, the owner reads the file a window
//! at a time, a replaced worker is brought back to where the stream was, and
//! every frame either side could be handed that is not an honest one is
//! refused.

extern crate std;

use std::cell::Cell;
use std::format;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use tairix_hash::HashSeed;
use tairix_sound::{CoverRange, DecodeError, Metadata, PcmSource, SoundFormat};

use super::{
    decode_from_wire, decode_to_wire, encode_info, encode_metadata, session_bounds,
    AudioDecodeClient, AudioDecodeError, AudioDecodeService, AudioRefusal, DecodeEvent,
    CACHE_PAGES, COVER_LEN, FROM_BLOCK, FROM_NEED, FROM_OPENED, FROM_REFUSED, FROM_SOUGHT, LIMITS,
    MAX_BLOCK_FRAMES, MAX_NEED_BYTES, MAX_OPENED_LEN, PAGE_BYTES, TO_DECODE, TO_OPEN, TO_SUPPLY,
};
use crate::loopback::LoopbackSessionLauncher;
use crate::proto::ProtoError;
use crate::session::{FrameOut, SessionError, SessionService, SessionStep};
use crate::supervise::SupervisedSession;
use crate::testing::NullSink;
use crate::wire::Writer;

const SEED: HashSeed = HashSeed::from_words(0x0123_4567, 0x89ab_cdef);

fn xorshift(count: usize, mut state: u32) -> Vec<u8> {
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend(u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A 16-bit PCM WAVE file of `channels` channels over `data`, its chunks
/// `before` and `after` the data.
fn wave(channels: u16, data: &[u8], before: &[Vec<u8>], after: &[Vec<u8>]) -> Vec<u8> {
    let mut format = Vec::new();
    format.extend(1u16.to_le_bytes());
    format.extend(channels.to_le_bytes());
    format.extend(44_100u32.to_le_bytes());
    format.extend((44_100 * 2 * u32::from(channels)).to_le_bytes());
    format.extend((2 * channels).to_le_bytes());
    format.extend(16u16.to_le_bytes());
    let mut chunks = vec![chunk(b"fmt ", &format)];
    chunks.extend_from_slice(before);
    chunks.push(chunk(b"data", data));
    chunks.extend_from_slice(after);
    let body = chunks.concat();
    let mut file = b"RIFF".to_vec();
    file.extend(u32::try_from(body.len() + 4).expect("small").to_le_bytes());
    file.extend(b"WAVE");
    file.extend(body);
    file
}

/// An AU file of G.721 codes: any bytes are a stream, and one that cannot
/// be entered except from its start.
fn g721(codes: &[u8]) -> Vec<u8> {
    let mut file = b".snd".to_vec();
    for field in [
        24u32,
        u32::try_from(codes.len()).expect("small"),
        23,
        8000,
        1,
    ] {
        file.extend(field.to_be_bytes());
    }
    file.extend(codes);
    file
}

fn tags(count: usize) -> Vec<u8> {
    let mut info = b"INFO".to_vec();
    info.extend(chunk(b"INAM", b"A tune"));
    for index in 0..count {
        info.extend(chunk(b"ICMT", format!("note {index}").as_bytes()));
    }
    chunk(b"LIST", &info)
}

fn cues(count: u32) -> Vec<u8> {
    let mut body = count.to_le_bytes().to_vec();
    for id in 0..count {
        body.extend(id.to_le_bytes());
        body.extend([0; 16]);
        body.extend((id * 3).to_le_bytes());
    }
    chunk(b"cue ", &body)
}

/// What decoding `file` in this process gives: its metadata and every frame.
fn direct(file: &[u8]) -> (Metadata, Vec<u8>) {
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut block = vec![0u8; MAX_BLOCK_FRAMES as usize * source.info().frame_bytes()];
    let mut pcm = Vec::new();
    loop {
        let written = source.next_block(&mut input, &mut block).expect("decodes");
        if written == 0 {
            return (source.metadata().clone(), pcm);
        }
        pcm.extend_from_slice(&block[..written * source.info().frame_bytes()]);
    }
}

/// A reply, owned so it outlives the frame it came in.
#[derive(Debug, Eq, PartialEq)]
enum Answered {
    Opened,
    Block(u64, Vec<u8>),
    Ended(u64),
    Sought(u64),
    Refused(AudioRefusal),
    Resumed(u64),
}

/// The owner of one decode: the file it reads for the worker, what that
/// cost, and the clock its session is paced by.
struct Owner<'f> {
    file: &'f [u8],
    needs: usize,
    supplied: usize,
    now: u64,
    /// Report the next need's read as failed.
    fail_next: bool,
}

impl<'f> Owner<'f> {
    fn new(file: &'f [u8]) -> Self {
        Self {
            file,
            needs: 0,
            supplied: 0,
            now: 0,
            fail_next: false,
        }
    }

    /// Carry one request to its answer, replacing failed workers and
    /// bringing each back as it goes.
    fn exchange<F: FnMut() -> S, S: SessionService>(
        &mut self,
        session: &mut SupervisedSession<LoopbackSessionLauncher<F>, NullSink>,
        client: &mut AudioDecodeClient,
    ) -> Result<Answered, AudioDecodeError> {
        for _ in 0..100_000 {
            match self.turn(session, client) {
                Ok(Some(answered)) => return Ok(answered),
                Ok(None) => {}
                Err(Stop::Worker) => {
                    self.now = session.restart_deadline().expect("a replacement is due");
                    session.start(self.now).expect("a replacement starts");
                    client.restart(SEED)?;
                }
                Err(Stop::Client(err)) => return Err(err),
            }
        }
        panic!("the request was never answered");
    }

    fn turn<F: FnMut() -> S, S: SessionService>(
        &mut self,
        session: &mut SupervisedSession<LoopbackSessionLauncher<F>, NullSink>,
        client: &mut AudioDecodeClient,
    ) -> Result<Option<Answered>, Stop> {
        if let Some(frame) = client.outgoing() {
            session.send(frame).map_err(worker)?;
            client.sent();
        }
        while session.wants_write() {
            session.on_writable(self.now).map_err(worker)?;
        }
        session.on_readable(self.now).map_err(worker)?;
        let mut answered = None;
        while let Some(event) = session
            .recv(self.now, |frame| client.on_frame(frame).map(own))
            .map_err(worker)?
        {
            match event {
                Ok(Owned::Need(offset, len)) => self.answer(client, offset, len),
                Ok(Owned::Resuming) => {}
                Ok(Owned::Answer(answer)) => answered = Some(answer),
                Err(AudioDecodeError::Unbelievable) => {
                    session.condemn(self.now, "an unbelievable answer");
                    return Err(Stop::Worker);
                }
                Err(err) => return Err(Stop::Client(err)),
            }
        }
        Ok(answered)
    }

    fn answer(&mut self, client: &mut AudioDecodeClient, offset: u64, len: usize) {
        self.needs += 1;
        assert_eq!(offset % PAGE_BYTES as u64, 0, "a need starts on a page");
        assert!(len <= MAX_NEED_BYTES);
        if core::mem::take(&mut self.fail_next) {
            client.unreadable().expect("a need is outstanding");
            return;
        }
        let start = usize::try_from(offset).expect("small");
        let bytes = &self.file[start..start + len];
        self.supplied += len;
        client.supply(bytes).expect("the need's own length");
    }
}

/// Why a turn stopped short of an answer.
enum Stop {
    /// The worker failed, and was reaped.
    Worker,
    /// The client gave up on the stream.
    Client(AudioDecodeError),
}

/// The one way a session fails these workers: a worker that failed.
fn worker(err: SessionError) -> Stop {
    assert_eq!(err, SessionError::WorkerFailed, "the session itself failed");
    Stop::Worker
}

/// What one frame told the owner, owned.
enum Owned {
    Need(u64, usize),
    Resuming,
    Answer(Answered),
}

fn own(event: DecodeEvent<'_>) -> Owned {
    Owned::Answer(match event {
        DecodeEvent::Need { offset, len } => return Owned::Need(offset, len),
        DecodeEvent::Resuming => return Owned::Resuming,
        DecodeEvent::Opened => Answered::Opened,
        DecodeEvent::Block { position, pcm } => Answered::Block(position, pcm.to_vec()),
        DecodeEvent::Ended { position } => Answered::Ended(position),
        DecodeEvent::Sought { position } => Answered::Sought(position),
        DecodeEvent::Refused(refusal) => Answered::Refused(refusal),
        DecodeEvent::Resumed { position } => Answered::Resumed(position),
    })
}

type Healthy = SupervisedSession<LoopbackSessionLauncher<fn() -> AudioDecodeService>, NullSink>;

fn healthy() -> Healthy {
    let factory: fn() -> AudioDecodeService = AudioDecodeService::new;
    supervised(factory)
}

fn supervised<F: FnMut() -> S, S: SessionService>(
    factory: F,
) -> SupervisedSession<LoopbackSessionLauncher<F>, NullSink> {
    let bounds = session_bounds().expect("the protocol's bounds are workable");
    let mut session =
        SupervisedSession::new(LoopbackSessionLauncher::new(factory), bounds, NullSink);
    session.start(0).expect("the first worker starts");
    session
}

/// Open `file` and decode all of it in blocks of `frames`, answering every
/// replacement with a resume.
fn play<F: FnMut() -> S, S: SessionService>(
    owner: &mut Owner<'_>,
    session: &mut SupervisedSession<LoopbackSessionLauncher<F>, NullSink>,
    client: &mut AudioDecodeClient,
    frames: u32,
) -> Vec<u8> {
    client
        .open(owner.file.len() as u64, None, SEED)
        .expect("idle");
    assert_eq!(owner.exchange(session, client), Ok(Answered::Opened));
    let mut pcm = Vec::new();
    // No file this crate claims holds more than two frames a byte.
    for _ in 0..4 * owner.file.len() + 64 {
        client.decode(frames).expect("idle");
        match owner.exchange(session, client).expect("an honest answer") {
            Answered::Block(position, block) => {
                assert_eq!(
                    position * client.info().expect("open").frame_bytes() as u64,
                    pcm.len() as u64
                );
                pcm.extend(block);
            }
            Answered::Resumed(position) => {
                assert_eq!(position, client.position());
            }
            Answered::Ended(position) => {
                assert_eq!(position, client.position());
                return pcm;
            }
            other => panic!("a decode answered {other:?}"),
        }
    }
    panic!("the stream never ended");
}

#[test]
fn a_file_decodes_through_the_worker_exactly_as_it_does_in_this_process() {
    let wave_file = wave(
        2,
        &xorshift(600_000, 7),
        &[tags(3)],
        &[cues(4), chunk(b"note", &[1; 9000])],
    );
    let au_file = g721(&xorshift(70_000, 9));
    for file in [wave_file, au_file] {
        let (metadata, expected) = direct(&file);
        for frames in [MAX_BLOCK_FRAMES, 1000, 1] {
            let mut owner = Owner::new(&file);
            let mut client = AudioDecodeClient::new();
            let pcm = play(&mut owner, &mut healthy(), &mut client, frames);
            assert_eq!(pcm, expected, "decoded in blocks of {frames}");
            assert_eq!(client.metadata(), Some(&metadata));
        }
    }
}

#[test]
fn a_stream_costs_one_need_a_window_and_its_file_read_about_once() {
    let file = wave(2, &xorshift(2_000_000, 3), &[], &[]);
    let mut owner = Owner::new(&file);
    let pcm = play(
        &mut owner,
        &mut healthy(),
        &mut AudioDecodeClient::new(),
        MAX_BLOCK_FRAMES,
    );
    assert_eq!(pcm.len(), 2_000_000);
    let blocks = pcm.len().div_ceil(MAX_BLOCK_FRAMES as usize * 4);
    assert!(
        owner.needs * 4 <= blocks,
        "{} needs for {blocks} blocks: each need should serve a run of them",
        owner.needs
    );
    assert!(
        owner.supplied <= file.len().next_multiple_of(PAGE_BYTES),
        "{} bytes read of {}",
        owner.supplied,
        file.len()
    );
}

#[test]
fn a_seek_enters_a_seekable_stream_where_asked_and_a_stream_that_cannot_says_so() {
    let file = wave(2, &xorshift(40_000, 5), &[], &[]);
    let (_, expected) = direct(&file);
    let mut owner = Owner::new(&file);
    let mut session = healthy();
    let mut client = AudioDecodeClient::new();
    client
        .open(file.len() as u64, Some(SoundFormat::Wav), SEED)
        .expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Opened)
    );
    for target in [7_000u64, 3, 9_999, 10_000] {
        client.seek(target).expect("idle");
        assert_eq!(
            owner.exchange(&mut session, &mut client),
            Ok(Answered::Sought(target))
        );
        client.decode(16).expect("idle");
        let at = usize::try_from(target).expect("small") * 4;
        match owner.exchange(&mut session, &mut client).expect("honest") {
            Answered::Block(position, pcm) => {
                assert_eq!(position, target);
                assert_eq!(pcm, expected[at..at + pcm.len()]);
            }
            Answered::Ended(position) => assert_eq!(position, 10_000),
            other => panic!("{other:?}"),
        }
    }
    client.seek(10_001).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Refused(AudioRefusal::Decode(
            DecodeError::SeekPastEnd
        )))
    );

    let file = g721(&xorshift(4_000, 1));
    let mut owner = Owner::new(&file);
    client.open(file.len() as u64, None, SEED).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Opened)
    );
    client.seek(100).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Refused(AudioRefusal::Decode(
            DecodeError::SeekUnsupported
        )))
    );
    assert_eq!(client.position(), 0);
    client.decode(8).expect("still open");
    assert!(matches!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Block(0, _))
    ));
}

#[test]
fn a_file_whose_structure_outgrows_the_cache_is_refused_not_asked_for_for_ever() {
    let spread: Vec<Vec<u8>> = (0..CACHE_PAGES + 8)
        .map(|_| chunk(b"pad ", &[0; PAGE_BYTES]))
        .collect();
    let file = wave(1, &[0; 64], &spread, &[]);
    let mut owner = Owner::new(&file);
    let mut client = AudioDecodeClient::new();
    client.open(file.len() as u64, None, SEED).expect("idle");
    assert_eq!(
        owner.exchange(&mut healthy(), &mut client),
        Ok(Answered::Refused(AudioRefusal::WorkingSetExceeded))
    );
    assert!(owner.needs <= CACHE_PAGES, "{} needs", owner.needs);
    assert!(
        client.info().is_none(),
        "a refused open leaves nothing open"
    );
    assert_eq!(client.decode(1), Err(AudioDecodeError::OutOfTurn));
}

#[test]
fn a_read_the_owner_cannot_make_fails_the_request_it_was_for() {
    let file = wave(1, &xorshift(400_000, 2), &[], &[]);
    let mut session = healthy();
    let mut client = AudioDecodeClient::new();
    let mut owner = Owner::new(&file);
    owner.fail_next = true;
    client.open(file.len() as u64, None, SEED).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Refused(AudioRefusal::Decode(
            DecodeError::InputFailed
        )))
    );
    assert!(client.info().is_none());

    client
        .open(file.len() as u64, None, SEED)
        .expect("closed again");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Opened)
    );
    let mut decoded = 0;
    loop {
        client.decode(MAX_BLOCK_FRAMES).expect("idle");
        match owner.exchange(&mut session, &mut client).expect("honest") {
            Answered::Block(_, pcm) => decoded += pcm.len(),
            Answered::Refused(refusal) => {
                assert_eq!(refusal, AudioRefusal::Decode(DecodeError::InputFailed));
                break;
            }
            other => panic!("{other:?}"),
        }
        owner.fail_next = decoded > 100_000;
    }
    let position = client.position();
    client
        .decode(MAX_BLOCK_FRAMES)
        .expect("a refused decode leaves the stream open");
    match owner.exchange(&mut session, &mut client).expect("honest") {
        Answered::Block(at, _) => assert_eq!(at, position, "the stream did not move"),
        other => panic!("{other:?}"),
    }
}

/// Serves as the real worker does, but ends its stream unanswered on the
/// decode request that finds its stream at `at`: a decoder that crashes
/// where a file drives it.
struct Crashing {
    inner: AudioDecodeService,
    at: Option<u64>,
}

impl SessionService for Crashing {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let position = self
            .inner
            .file
            .as_ref()
            .and_then(|file| file.source.as_ref())
            .map(PcmSource::position);
        if request.first() == Some(&TO_DECODE) && position.is_some() && position == self.at {
            return SessionStep::Finished;
        }
        self.inner.handle(request, out)
    }
}

#[test]
fn a_replaced_worker_is_brought_back_to_where_the_stream_was() {
    let seekable = wave(2, &xorshift(300_000, 4), &[tags(1)], &[]);
    let unseekable = g721(&xorshift(30_000, 6));
    for file in [seekable, unseekable] {
        let (_, expected) = direct(&file);
        let generations = Rc::new(Cell::new(0));
        let started = Rc::clone(&generations);
        let mut session = supervised(move || {
            started.set(started.get() + 1);
            Crashing {
                inner: AudioDecodeService::new(),
                at: (started.get() == 1).then_some(u64::from(MAX_BLOCK_FRAMES) * 3),
            }
        });
        let mut owner = Owner::new(&file);
        let pcm = play(
            &mut owner,
            &mut session,
            &mut AudioDecodeClient::new(),
            MAX_BLOCK_FRAMES,
        );
        assert_eq!(pcm, expected);
        assert_eq!(
            generations.get(),
            2,
            "one worker failed and one replaced it"
        );
    }
}

#[test]
fn a_stream_that_fails_a_worker_twice_at_one_place_is_given_up() {
    let file = wave(2, &xorshift(300_000, 8), &[], &[]);
    let mut session = supervised(|| Crashing {
        inner: AudioDecodeService::new(),
        at: Some(8192),
    });
    let mut owner = Owner::new(&file);
    let mut client = AudioDecodeClient::new();
    client.open(file.len() as u64, None, SEED).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Opened)
    );
    let mut outcome = None;
    for _ in 0..100 {
        client.decode(MAX_BLOCK_FRAMES).expect("idle");
        match owner.exchange(&mut session, &mut client) {
            Ok(Answered::Block(..) | Answered::Resumed(_)) => {}
            other => {
                outcome = Some(other);
                break;
            }
        }
    }
    let outcome = outcome.expect("the stream was given up rather than resumed for ever");
    assert_eq!(outcome, Err(AudioDecodeError::CannotResume));
    assert!(client.info().is_none(), "the stream is closed");
}

/// Serves as the real worker does, but the first such worker ends its
/// stream on its first decode.
struct FailsFirst {
    inner: AudioDecodeService,
    first: bool,
}

impl SessionService for FailsFirst {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        if self.first && request.first() == Some(&TO_DECODE) {
            return SessionStep::Finished;
        }
        self.inner.handle(request, out)
    }
}

#[test]
fn a_replacement_that_finds_another_stream_is_not_believed_to_be_the_same() {
    let file = wave(2, &xorshift(40_000, 8), &[], &[]);
    let other = wave(1, &xorshift(40_000, 8), &[], &[]);
    let generations = Rc::new(Cell::new(0));
    let started = Rc::clone(&generations);
    let mut session = supervised(move || {
        started.set(started.get() + 1);
        FailsFirst {
            inner: AudioDecodeService::new(),
            first: started.get() == 1,
        }
    });
    let mut owner = Owner::new(&file);
    let mut client = AudioDecodeClient::new();
    client.open(file.len() as u64, None, SEED).expect("idle");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Ok(Answered::Opened)
    );
    client.decode(16).expect("idle");
    owner.file = &other;
    assert_eq!(other.len(), file.len(), "the file rewritten in place");
    assert_eq!(
        owner.exchange(&mut session, &mut client),
        Err(AudioDecodeError::CannotResume)
    );
}

/// Records every frame a worker emits.
#[derive(Default)]
struct Frames(Vec<Vec<u8>>);

impl FrameOut for Frames {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

/// The frame a worker answers `request` with.
fn served(worker: &mut AudioDecodeService, request: &[u8]) -> Vec<u8> {
    let mut frames = Frames::default();
    assert_eq!(worker.handle(request, &mut frames), SessionStep::Continue);
    assert_eq!(frames.0.len(), 1, "one answer to one frame");
    frames.0.remove(0)
}

fn refused(refusal: AudioRefusal) -> Vec<u8> {
    let (code, detail) = refusal.to_wire();
    let mut frame = vec![FROM_REFUSED, code];
    frame.extend(detail.to_le_bytes());
    frame
}

fn open_frame(len: u64) -> Vec<u8> {
    let mut client = AudioDecodeClient::new();
    client.open(len, None, SEED).expect("idle");
    client.outgoing().expect("an open").to_vec()
}

#[test]
fn every_frame_the_protocol_does_not_admit_is_refused_and_closes_the_file() {
    let file = wave(1, &[0; 64], &[], &[]);
    let malformed = refused(AudioRefusal::MalformedRequest);
    let mut worker = AudioDecodeService::new();
    assert_eq!(
        served(&mut worker, &[TO_DECODE, 1, 0, 0, 0]),
        refused(AudioRefusal::NotOpen)
    );
    for frame in [
        &[][..],
        &[0x7f],
        &[TO_DECODE, 1, 0, 0],
        &[TO_OPEN, 0, 0],
        &[TO_SUPPLY, 0, 0, 0, 0, 0, 0, 0, 0, 1],
    ] {
        assert_eq!(served(&mut worker, frame), malformed, "{frame:?}");
    }
    let mut open = open_frame(file.len() as u64);
    *open.last_mut().expect("a format") = 9;
    assert_eq!(served(&mut worker, &open), malformed, "an unknown format");

    let need = served(&mut worker, &open_frame(file.len() as u64));
    assert_eq!(need[0], FROM_NEED);
    assert_eq!(
        served(&mut worker, &[TO_DECODE, 1, 0, 0, 0]),
        malformed,
        "a request while one waits"
    );
    assert_eq!(
        served(&mut worker, &[TO_DECODE, 1, 0, 0, 0]),
        refused(AudioRefusal::NotOpen)
    );

    for supply in [
        |file: &[u8]| [&[TO_SUPPLY][..], &4096u64.to_le_bytes(), file].concat(),
        |file: &[u8]| [&[TO_SUPPLY][..], &0u64.to_le_bytes(), &file[1..]].concat(),
    ] {
        assert_eq!(
            served(&mut worker, &open_frame(file.len() as u64))[0],
            FROM_NEED
        );
        assert_eq!(
            served(&mut worker, &supply(&file)),
            malformed,
            "a supply of other bytes"
        );
    }

    assert_eq!(
        served(&mut worker, &open_frame(file.len() as u64))[0],
        FROM_NEED
    );
    let supply = [&[TO_SUPPLY][..], &0u64.to_le_bytes(), &file].concat();
    assert_eq!(served(&mut worker, &supply)[0], FROM_OPENED);
    for frames in [0u32, MAX_BLOCK_FRAMES + 1] {
        let decode = [&[TO_DECODE][..], &frames.to_le_bytes()].concat();
        assert_eq!(served(&mut worker, &decode), malformed, "{frames} frames");
    }
}

/// Every refusal a decoder can make.
const EVERY_REFUSAL: [DecodeError; 75] = [
    DecodeError::UnknownFormat,
    DecodeError::InputUnavailable,
    DecodeError::InputFailed,
    DecodeError::OutOfMemory,
    DecodeError::ChannelsExceedLimit,
    DecodeError::NoChannels,
    DecodeError::ChannelLayoutUnsupported,
    DecodeError::RateOutOfRange,
    DecodeError::BufferTooSmall,
    DecodeError::SeekUnsupported,
    DecodeError::SeekPastEnd,
    DecodeError::AuBadMagic,
    DecodeError::AuHeaderTruncated,
    DecodeError::AuDataOffsetBad,
    DecodeError::AuFragmentedData,
    DecodeError::AuNestedSound,
    DecodeError::AuDspProgram,
    DecodeError::AuDisplayData,
    DecodeError::AuDspCommands,
    DecodeError::AuUnspecifiedEncoding,
    DecodeError::AuUnknownEncoding(0xDEAD_BEEF),
    DecodeError::AuAdpcmChannels,
    DecodeError::AuG722Rate,
    DecodeError::WavBadMagic,
    DecodeError::WavChunkTruncated,
    DecodeError::WavTooManyChunks,
    DecodeError::WavMissingDs64,
    DecodeError::WavMissingFormat,
    DecodeError::WavMissingData,
    DecodeError::WavDuplicateFormat,
    DecodeError::WavDuplicateData,
    DecodeError::WavFormatTruncated,
    DecodeError::WavMpegAudio,
    DecodeError::WavGsm610,
    DecodeError::WavUnknownFormatTag(0x0055),
    DecodeError::WavBadBlockAlign,
    DecodeError::WavBadBitDepth,
    DecodeError::WavBadExtensible,
    DecodeError::WavUnknownSubformat,
    DecodeError::WavChannelMask,
    DecodeError::WavBadAdpcmFormat,
    DecodeError::WavAdpcmBlockCorrupt,
    DecodeError::FlacBadMarker,
    DecodeError::FlacMissingStreamInfo,
    DecodeError::FlacBadStreamInfo,
    DecodeError::FlacDuplicateBlock,
    DecodeError::FlacForbiddenBlock,
    DecodeError::FlacMetadataTruncated,
    DecodeError::FlacBadSeekTable,
    DecodeError::FlacBadComment,
    DecodeError::FlacBadCuesheet,
    DecodeError::FlacBadPicture,
    DecodeError::FlacBadApplication,
    DecodeError::FlacChannelMask,
    DecodeError::FlacNoSync,
    DecodeError::FlacHeaderCrc,
    DecodeError::FlacFrameCrc,
    DecodeError::FlacReserved,
    DecodeError::FlacFrameInvalid,
    DecodeError::FlacFrameMismatch,
    DecodeError::FlacFrameOutOfOrder,
    DecodeError::FlacFrameTooLarge,
    DecodeError::FlacTruncated,
    DecodeError::FlacDigestMismatch,
    DecodeError::OggNoCapture,
    DecodeError::OggBadPage,
    DecodeError::OggPageTruncated,
    DecodeError::OggPageCrc,
    DecodeError::OggPageLost,
    DecodeError::OggBadPacket,
    DecodeError::OggPacketTooLarge,
    DecodeError::OggNoFlacStream,
    DecodeError::OggBadFlacMapping,
    DecodeError::OggNoAudio,
    DecodeError::OggChained,
];

#[test]
fn every_decoder_refusal_crosses_as_itself() {
    let every = EVERY_REFUSAL;
    let mut codes = Vec::new();
    for err in every {
        let (code, detail) = decode_to_wire(err);
        assert_eq!(decode_from_wire(code, detail), Some(err));
        codes.push(code);
    }
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), every.len(), "every refusal has its own code");
    for code in 0..=u8::MAX {
        if let Some(err) = decode_from_wire(code, 0) {
            assert_eq!(decode_to_wire(err), (code, 0));
        }
        if code != super::AU_UNKNOWN_ENCODING && code != super::WAV_UNKNOWN_FORMAT_TAG {
            assert_eq!(
                decode_from_wire(code, 1),
                None,
                "a detail no refusal carries"
            );
        }
    }
    assert_eq!(
        decode_from_wire(super::WAV_UNKNOWN_FORMAT_TAG, 0x1_0000),
        None
    );
}

#[test]
fn an_open_answered_at_the_limits_fits_the_frame_the_owner_admits() {
    let file = wave(1, &[0; 64], &[tags(4000)], &[cues(2000)]);
    let mut worker = AudioDecodeService::new();
    let mut answer = served(&mut worker, &open_frame(file.len() as u64));
    while answer[0] == FROM_NEED {
        let offset = u64::from_le_bytes(answer[1..9].try_into().expect("eight"));
        let len = u32::from_le_bytes(answer[9..13].try_into().expect("four"));
        let start = usize::try_from(offset).expect("small");
        let bytes = &file[start..start + len as usize];
        answer = served(
            &mut worker,
            &[&[TO_SUPPLY][..], &offset.to_le_bytes(), bytes].concat(),
        );
    }
    let opened = answer;
    assert_eq!(opened[0], FROM_OPENED);
    let source = worker
        .file
        .as_ref()
        .and_then(|file| file.source.as_ref())
        .expect("open");
    let metadata = source.metadata();
    assert!(metadata.omitted, "the file holds more than the limits keep");
    assert_eq!(metadata.cues.len(), LIMITS.max_markers() as usize);
    assert!(
        opened.len() <= MAX_OPENED_LEN,
        "{} > {MAX_OPENED_LEN}",
        opened.len()
    );
}

/// One way to make an honest answer dishonest, and what it claims.
type Lie = (&'static str, fn(&mut Vec<u8>));

/// A client that has sent an open of `len` bytes and awaits its answer.
fn opening(len: u64) -> AudioDecodeClient {
    let mut client = AudioDecodeClient::new();
    client.open(len, None, SEED).expect("idle");
    client.sent();
    client
}

/// The open answer the real worker gives `file`, its metadata passed through
/// `edit`.
fn opened(file: &[u8], edit: impl FnOnce(&mut Metadata)) -> Vec<u8> {
    let mut input = file;
    let source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut metadata = source.metadata().clone();
    edit(&mut metadata);
    let mut out = Writer::new();
    out.u8(FROM_OPENED);
    encode_info(&mut out, source.info());
    encode_metadata(&mut out, &metadata);
    out.finish()
}

/// The open answer the real worker gives `file`.
fn honest_opened(file: &[u8]) -> Vec<u8> {
    opened(file, |_| {})
}

#[test]
fn an_open_answer_that_is_not_an_honest_one_is_not_believed() {
    let file = wave(2, &xorshift(4_000, 1), &[tags(2)], &[cues(3)]);
    let honest = honest_opened(&file);
    assert_eq!(
        opening(file.len() as u64).on_frame(&honest),
        Ok(DecodeEvent::Opened)
    );
    let lies: [Lie; 14] = [
        ("no format", |frame| frame[1] = 0),
        ("an unknown format", |frame| frame[1] = 9),
        ("an unknown encoding", |frame| frame[2] = 0),
        ("a width no sample has", |frame| frame[3] = 33),
        ("no rate", |frame| frame[4..8].fill(0)),
        ("no channels", |frame| frame[8] = 0),
        ("more channels than a map names", |frame| frame[8] = 9),
        ("an unknown sample format", |frame| frame[17] = 0xEE),
        ("a flag that is neither", |frame| frame[18] = 2),
        ("a count that is not stated", |frame| frame[18] = 0),
        ("seekability that is neither", |frame| frame[27] = 2),
        ("a disagreement that agrees", |frame| frame[28] = 1),
        ("an unknown tag kind", |frame| frame[49] = 0),
        ("an omission that is neither", |frame| {
            let at = frame.len() - COVER_LEN - 1;
            frame[at] = 2;
        }),
    ];
    for (lie, mutate) in lies {
        let mut frame = honest.clone();
        mutate(&mut frame);
        let mut client = opening(file.len() as u64);
        assert_eq!(
            client.on_frame(&frame),
            Err(AudioDecodeError::Unbelievable),
            "{lie}"
        );
    }
    let mut trailing = honest.clone();
    trailing.push(0);
    assert_eq!(
        opening(file.len() as u64).on_frame(&trailing),
        Err(AudioDecodeError::Unbelievable)
    );
    assert_eq!(
        opening(file.len() as u64).on_frame(&honest[..honest.len() - 1]),
        Err(AudioDecodeError::Unbelievable)
    );
    let mut note = honest.clone();
    let at = note.len() - COVER_LEN - 2;
    note[at] = 200;
    assert_eq!(
        opening(file.len() as u64).on_frame(&note),
        Err(AudioDecodeError::Unbelievable)
    );

    let mut client = AudioDecodeClient::new();
    client
        .open(file.len() as u64, Some(SoundFormat::Au), SEED)
        .expect("idle");
    client.sent();
    assert_eq!(
        client.on_frame(&honest),
        Err(AudioDecodeError::Unbelievable),
        "the format asked for"
    );

    let mut over = Metadata::default();
    let long = "x".repeat(40_000);
    for _ in 0..2 {
        over.tags.push(tairix_sound::Tag {
            kind: tairix_sound::TagKind::Comment,
            value: long.clone(),
        });
    }
    assert_eq!(
        opening(file.len() as u64).on_frame(&opened(&file, |metadata| *metadata = over)),
        Err(AudioDecodeError::Unbelievable),
        "tags past the budget"
    );
}

/// A cover crosses as the range the file holds it in, and a range no file
/// of that length could hold, or one spelled two ways, is not believed.
#[test]
fn a_cover_crosses_as_its_range_within_the_file() {
    let file = wave(2, &xorshift(400, 5), &[], &[]);
    let len = file.len() as u64;
    let range = CoverRange {
        offset: 12,
        len: len - 12,
    };
    let mut client = opening(len);
    assert_eq!(
        client.on_frame(&opened(&file, |metadata| metadata.cover = Some(range))),
        Ok(DecodeEvent::Opened)
    );
    assert_eq!(
        client.metadata().and_then(|metadata| metadata.cover),
        Some(range)
    );

    let past = CoverRange {
        offset: 13,
        len: len - 12,
    };
    let wrapping = CoverRange {
        offset: u64::MAX,
        len: 2,
    };
    for cover in [past, wrapping] {
        assert_eq!(
            opening(len).on_frame(&opened(&file, |metadata| metadata.cover = Some(cover))),
            Err(AudioDecodeError::Unbelievable),
            "{cover:?}"
        );
    }
    let honest = honest_opened(&file);
    let flag_at = honest.len() - COVER_LEN;
    let lies: [Lie; 3] = [
        ("a presence that is neither", |frame| {
            let at = frame.len() - COVER_LEN;
            frame[at] = 2;
        }),
        ("an empty cover", |frame| {
            let at = frame.len() - COVER_LEN;
            frame[at] = 1;
        }),
        ("an absent cover with a range", |frame| {
            *frame.last_mut().expect("a length") = 1;
        }),
    ];
    assert_eq!(honest[flag_at], 0);
    for (lie, mutate) in lies {
        let mut frame = honest.clone();
        mutate(&mut frame);
        assert_eq!(
            opening(len).on_frame(&frame),
            Err(AudioDecodeError::Unbelievable),
            "{lie}"
        );
    }
}

/// A tag keyed in its file's own vocabulary crosses with its key, and a key
/// no file could spell is not believed.
#[test]
fn a_tag_keyed_in_its_files_vocabulary_crosses_with_its_key() {
    let file = wave(2, &xorshift(400, 3), &[], &[]);
    let key = tairix_sound::TagKey::new(b"REPLAYGAIN_TRACK_GAIN").expect("a key");
    let mut metadata = Metadata::default();
    metadata.tags.push(tairix_sound::Tag {
        kind: tairix_sound::TagKind::Other(key),
        value: "-6.1 dB".into(),
    });
    let honest = opened(&file, |opened| *opened = metadata.clone());
    let mut client = opening(file.len() as u64);
    assert_eq!(client.on_frame(&honest), Ok(DecodeEvent::Opened));
    assert_eq!(client.metadata(), Some(&metadata));
    let key_at = honest
        .windows(5)
        .position(|window| window == b"REPLA")
        .expect("the key is on the wire");
    let mut forged = honest.clone();
    forged[key_at] = b'=';
    assert_eq!(
        opening(file.len() as u64).on_frame(&forged),
        Err(AudioDecodeError::Unbelievable)
    );
}

fn need(offset: u64, len: u32) -> Vec<u8> {
    [&[FROM_NEED][..], &offset.to_le_bytes(), &len.to_le_bytes()].concat()
}

#[test]
fn a_need_that_is_not_an_honest_one_is_not_believed() {
    let len = 3 * PAGE_BYTES as u64 + 100;
    assert!(matches!(
        opening(len).on_frame(&need(0, 4096)),
        Ok(DecodeEvent::Need {
            offset: 0,
            len: 4096
        })
    ));
    assert!(matches!(
        opening(len).on_frame(&need(12_288, 100)),
        Ok(DecodeEvent::Need { .. })
    ));
    for lie in [
        need(1, 4096),
        need(0, 0),
        need(0, 100),
        need(12_288, 4096),
        need(16_384, 4096),
        need(0, u32::try_from(MAX_NEED_BYTES).expect("small") + 4096),
        need(u64::MAX - 4095, 4096),
    ] {
        assert_eq!(
            opening(len).on_frame(&lie),
            Err(AudioDecodeError::Unbelievable),
            "{lie:?}"
        );
    }
    let mut client = opening(len);
    assert!(client.on_frame(&need(0, 4096)).is_ok());
    assert_eq!(
        client.on_frame(&need(0, 4096)),
        Err(AudioDecodeError::Unbelievable),
        "a second need before the supply"
    );
    assert_eq!(client.supply(&[0; 10]), Err(AudioDecodeError::WrongLength));
    assert_eq!(client.supply(&[0; 4096]), Ok(()));
    assert_eq!(
        client.on_frame(&need(0, 4096)),
        Err(AudioDecodeError::Unbelievable),
        "a frame before the supply is sent"
    );
}

#[test]
fn a_worker_that_asks_more_often_than_a_request_could_need_is_not_believed() {
    let len = 2 * PAGE_BYTES as u64;
    let mut client = opening(len);
    for _ in 0..=CACHE_PAGES {
        assert!(client.on_frame(&need(0, 4096)).is_ok());
        client.supply(&[0; 4096]).expect("the need's length");
        client.sent();
    }
    assert_eq!(
        client.on_frame(&need(4096, 4096)),
        Err(AudioDecodeError::Unbelievable),
        "every honest request is answered or refused by then"
    );
    let mut client = opening(len);
    for _ in 0..CACHE_PAGES {
        assert!(client.on_frame(&need(0, 4096)).is_ok());
        client.supply(&[0; 4096]).expect("the need's length");
        client.sent();
    }
    assert!(client
        .on_frame(&refused(AudioRefusal::WorkingSetExceeded))
        .is_ok());
    assert!(
        client.open(len, None, SEED).is_ok(),
        "a new request counts afresh"
    );
    client.sent();
    assert!(client.on_frame(&need(0, 4096)).is_ok());
}

fn block(position: u64, frames: u32, pcm: &[u8]) -> Vec<u8> {
    [
        &[FROM_BLOCK][..],
        &position.to_le_bytes(),
        &frames.to_le_bytes(),
        pcm,
    ]
    .concat()
}

#[test]
fn a_block_or_seek_that_is_not_an_honest_one_is_not_believed() {
    let file = wave(2, &xorshift(400, 1), &[], &[]);
    let opened = |client: &mut AudioDecodeClient| {
        assert_eq!(
            client.on_frame(&honest_opened(&file)),
            Ok(DecodeEvent::Opened)
        );
    };
    let decoding = |frames: u32| {
        let mut client = opening(file.len() as u64);
        opened(&mut client);
        client.decode(frames).expect("idle");
        client.sent();
        client
    };
    assert!(matches!(
        decoding(10).on_frame(&block(0, 10, &[0; 40])),
        Ok(DecodeEvent::Block { position: 0, .. })
    ));
    assert!(matches!(
        decoding(10).on_frame(&block(0, 0, &[])),
        Ok(DecodeEvent::Ended { position: 0 })
    ));
    for lie in [
        block(1, 10, &[0; 40]),
        block(0, 11, &[0; 44]),
        block(0, 10, &[0; 39]),
        block(0, 0, &[0; 4]),
        [&[FROM_BLOCK][..], &[0; 11]].concat(),
    ] {
        assert_eq!(
            decoding(10).on_frame(&lie),
            Err(AudioDecodeError::Unbelievable)
        );
    }
    let mut client = decoding(100);
    assert!(client.on_frame(&block(0, 100, &[0; 400])).is_ok());
    client.decode(100).expect("idle");
    client.sent();
    assert_eq!(
        client.on_frame(&block(100, 1, &[0; 4])),
        Err(AudioDecodeError::Unbelievable),
        "past the stated frames"
    );

    let seeking = |frame: u64| {
        let mut client = opening(file.len() as u64);
        opened(&mut client);
        client.seek(frame).expect("idle");
        client.sent();
        client
    };
    let sought = |position: u64| [&[FROM_SOUGHT][..], &position.to_le_bytes()].concat();
    assert_eq!(
        seeking(50).on_frame(&sought(50)),
        Ok(DecodeEvent::Sought { position: 50 })
    );
    assert_eq!(
        seeking(50).on_frame(&sought(51)),
        Err(AudioDecodeError::Unbelievable)
    );
    assert_eq!(
        seeking(500).on_frame(&sought(500)),
        Err(AudioDecodeError::Unbelievable),
        "past the stated frames"
    );

    let unseekable = g721(&[0; 100]);
    let mut client = opening(unseekable.len() as u64);
    assert_eq!(
        client.on_frame(&honest_opened(&unseekable)),
        Ok(DecodeEvent::Opened)
    );
    client.seek(5).expect("idle");
    client.sent();
    assert_eq!(
        client.on_frame(&sought(5)),
        Err(AudioDecodeError::Unbelievable),
        "a stream that cannot seek"
    );
}

#[test]
fn a_refusal_the_client_could_not_have_earned_is_not_believed() {
    let mut client = opening(100);
    assert_eq!(
        client.on_frame(&refused(AudioRefusal::Decode(DecodeError::WavBadMagic))),
        Ok(DecodeEvent::Refused(AudioRefusal::Decode(
            DecodeError::WavBadMagic
        )))
    );
    for lie in [
        refused(AudioRefusal::NotOpen),
        refused(AudioRefusal::MalformedRequest),
        refused(AudioRefusal::Decode(DecodeError::InputUnavailable)),
        vec![FROM_REFUSED, 0xEE, 0, 0, 0, 0],
        vec![FROM_REFUSED, 16, 1, 0, 0, 0],
    ] {
        assert_eq!(
            opening(100).on_frame(&lie),
            Err(AudioDecodeError::Unbelievable),
            "{lie:?}"
        );
    }
    let mut idle = AudioDecodeClient::new();
    assert_eq!(
        idle.on_frame(&refused(AudioRefusal::WorkingSetExceeded)),
        Err(AudioDecodeError::Unbelievable),
        "unasked"
    );
}

#[test]
fn a_request_out_of_turn_is_refused_before_it_is_sent() {
    let mut client = AudioDecodeClient::new();
    assert_eq!(client.decode(1), Err(AudioDecodeError::OutOfTurn));
    assert_eq!(client.seek(1), Err(AudioDecodeError::OutOfTurn));
    assert_eq!(client.supply(&[]), Err(AudioDecodeError::OutOfTurn));
    assert_eq!(client.unreadable(), Err(AudioDecodeError::OutOfTurn));
    assert_eq!(client.restart(SEED), Ok(()), "nothing to bring back");
    client.open(10, None, SEED).expect("idle");
    assert_eq!(
        client.open(10, None, SEED),
        Err(AudioDecodeError::OutOfTurn)
    );
    client.sent();
    assert_eq!(client.decode(1), Err(AudioDecodeError::OutOfTurn));
    let file = wave(1, &[0; 8], &[], &[]);
    let mut client = opening(file.len() as u64);
    assert_eq!(
        client.on_frame(&honest_opened(&file)),
        Ok(DecodeEvent::Opened)
    );
    assert_eq!(client.decode(0), Err(AudioDecodeError::BlockSize));
    assert_eq!(
        client.decode(MAX_BLOCK_FRAMES + 1),
        Err(AudioDecodeError::BlockSize)
    );
}

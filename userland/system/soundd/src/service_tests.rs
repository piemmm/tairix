//! The whole authority over in-memory seams and the real decoder: who may cue
//! what, every path to silence, the bounds, and a cue sounding to its end.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams, StreamRole, StreamState, Tenancy};
use tairix_abi::capability::CapabilityId;
use tairix_abi::driver::audio::{Frames, StreamDirection};
use tairix_abi::fs::FileId;
use tairix_abi::origin::{CapabilitySummary, Origin, ProcId, TrustDomain};
use tairix_abi::process::INIT_PID;
use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::sound_cue::{
    decode_cue_reply, Answer, CueOutcome, CueRequest, Silence, SoundEvent, CUE_REQUEST_MAX,
    DEVMGR_UID, LOGIN_UID,
};
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_hash::{BuildSipHash13, HashSeed};
use tairix_log::{DiscardSink, Event, EventId, Sink};
use tairix_reclaim::Unpressured;
use tairix_sandbox::audiodecode::{session_bounds, AudioDecodeService};
use tairix_sandbox::loopback::LoopbackSessionLauncher;
use tairix_sandbox::supervise::SupervisedSession;
use tairix_soundtheme::{asset_path, SoundAsset, DEFAULT_THEME, DRAIN_SLACK_NS, MACHINE_SETTINGS};

use super::{CueAuthority, Tally};
use crate::bounds::{CUE_BURST, CUE_REFILL_NS, MAX_CONCURRENT_CUES, MIN_RETRIGGER_NS};
use crate::events;
use crate::pcm::cue_cache;
use crate::speaker::Speakers;
use crate::store::{FileIdentity, SoundStore};
use crate::testkit::{chime, flac, wav};
use crate::Decoder;

// ---------------------------------------------------------------- seams

/// The volume: files by path, and how often each was read; the delegations
/// sessions minted, by handle and grantor, and how many of them are held open;
/// and a failure the next read meets.
#[derive(Default)]
struct Volume {
    files: Vec<(String, FileIdentity, Vec<u8>)>,
    reads: Vec<String>,
    grants: Vec<(u64, ProcId, String)>,
    open: Rc<RefCell<usize>>,
    fail: Option<Errno>,
}

/// A file a session handed over, counted closed when dropped.
struct HandedFile {
    path: String,
    open: Rc<RefCell<usize>>,
}

impl Drop for HandedFile {
    fn drop(&mut self) {
        *self.open.borrow_mut() -= 1;
    }
}

#[derive(Clone, Default)]
struct Store(Rc<RefCell<Volume>>);

impl Store {
    fn put(&self, path: &str, bytes: Vec<u8>) {
        let mut volume = self.0.borrow_mut();
        let generation = volume.files.len() as u64 + 1;
        volume.files.retain(|(held, ..)| held != path);
        volume.files.push((
            String::from(path),
            FileIdentity {
                id: FileId {
                    volume: [7; 16],
                    node: generation,
                },
                content_gen: generation,
                len: bytes.len() as u64,
                modified: Time64::from_secs(i64::try_from(generation).expect("small")),
            },
            bytes,
        ));
    }

    fn reads(&self, path: &str) -> usize {
        self.0
            .borrow()
            .reads
            .iter()
            .filter(|read| *read == path)
            .count()
    }

    /// `grantor` delegates the file at `path` to the authority as `handle`.
    fn grant(&self, handle: u64, grantor: ProcId, path: &str) {
        self.0
            .borrow_mut()
            .grants
            .push((handle, grantor, String::from(path)));
    }

    /// Delegations minted and not yet redeemed.
    fn pending(&self) -> usize {
        self.0.borrow().grants.len()
    }

    /// Handed files held open.
    fn open(&self) -> usize {
        *self.0.borrow().open.borrow()
    }

    /// The next read fails with `err`.
    fn fail_next_read(&self, err: Errno) {
        self.0.borrow_mut().fail = Some(err);
    }
}

impl SoundStore for Store {
    type Handed = HandedFile;

    fn identify(&mut self, path: &str) -> Result<Option<FileIdentity>, Errno> {
        Ok(self
            .0
            .borrow()
            .files
            .iter()
            .find(|(held, ..)| held == path)
            .map(|(_, identity, _)| *identity))
    }

    fn read(&mut self, path: &str, expected: FileIdentity, max: usize) -> Result<Vec<u8>, Errno> {
        let mut volume = self.0.borrow_mut();
        volume.reads.push(String::from(path));
        if let Some(err) = volume.fail.take() {
            return Err(err);
        }
        let (_, identity, bytes) = volume
            .files
            .iter()
            .find(|(held, ..)| held == path)
            .ok_or(Errno::NotFound)?;
        if *identity != expected {
            return Err(Errno::Stale);
        }
        if bytes.len() > max {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(bytes.clone())
    }

    fn redeem(&mut self, handle: u64, grantor: ProcId) -> Result<HandedFile, Errno> {
        let mut volume = self.0.borrow_mut();
        let at = volume
            .grants
            .iter()
            .position(|(held, by, _)| *held == handle && *by == grantor)
            .ok_or(Errno::NotFound)?;
        let (_, _, path) = volume.grants.remove(at);
        *volume.open.borrow_mut() += 1;
        Ok(HandedFile {
            path,
            open: Rc::clone(&volume.open),
        })
    }

    fn identify_handed(&mut self, file: &HandedFile) -> Result<FileIdentity, Errno> {
        self.identify(&file.path)?.ok_or(Errno::NotFound)
    }

    fn read_handed(
        &mut self,
        file: &HandedFile,
        expected: FileIdentity,
        max: usize,
    ) -> Result<Vec<u8>, Errno> {
        self.read(&file.path, expected, max)
    }
}

/// One stream the fake mixer opened.
struct Line {
    params: OpenParams,
    played: Vec<u8>,
    queued: u32,
    ring: u32,
    started: bool,
    draining: bool,
    gain: Option<AudioGain>,
    closed: bool,
    notes: VecDeque<AudioNotify>,
}

/// The audio service: every stream it opened, and what to refuse.
#[derive(Default)]
struct Mixer {
    lines: Vec<Line>,
    refuse: Option<Errno>,
    ring: u32,
}

#[derive(Clone)]
struct Fake(Rc<RefCell<Mixer>>);

impl Fake {
    fn new(ring: u32) -> Self {
        Self(Rc::new(RefCell::new(Mixer {
            ring,
            ..Mixer::default()
        })))
    }

    fn line<R>(&self, at: usize, read: impl FnOnce(&mut Line) -> R) -> R {
        read(&mut self.0.borrow_mut().lines[at])
    }

    /// The device plays `frames` of what line `at` queued, freeing room.
    fn consume(&self, at: usize, frames: u32) {
        self.line(at, |line| {
            line.queued -= frames.min(line.queued);
            line.notes.push_back(AudioNotify::SpaceAvailable {
                stream_id: at as u64 + 1,
                position: Frames::ZERO,
            });
        });
    }

    /// Line `at` reports `state`.
    fn report(&self, at: usize, state: StreamState) {
        self.line(at, |line| {
            line.notes.push_back(AudioNotify::StateChanged {
                stream_id: at as u64 + 1,
                state,
                at: Frames::ZERO,
            });
        });
    }

    fn opened(&self) -> usize {
        self.0.borrow().lines.len()
    }
}

impl Speakers for Fake {
    type Stream = usize;

    fn open(&mut self, params: &OpenParams) -> Result<usize, Errno> {
        let mut mixer = self.0.borrow_mut();
        if let Some(err) = mixer.refuse {
            return Err(err);
        }
        let ring = mixer.ring;
        mixer.lines.push(Line {
            params: *params,
            played: Vec::new(),
            queued: 0,
            ring,
            started: false,
            draining: false,
            gain: None,
            closed: false,
            notes: VecDeque::new(),
        });
        Ok(mixer.lines.len() - 1)
    }

    fn mailbox(&self, stream: &usize) -> u64 {
        0x4155_0000_0000_0000 + *stream as u64
    }

    fn stream_id(&self, stream: &usize) -> u64 {
        *stream as u64 + 1
    }

    fn write(&mut self, stream: &mut usize, at: Frames, samples: &[u8]) -> Result<u32, Errno> {
        self.line(*stream, |line| {
            let frame_bytes = 4;
            assert_eq!(
                at.get(),
                (line.played.len() / frame_bytes) as u64,
                "written contiguously"
            );
            let room = line.ring - line.queued;
            let frames = u32::try_from(samples.len() / frame_bytes)
                .unwrap_or(u32::MAX)
                .min(room);
            line.played
                .extend_from_slice(&samples[..frames as usize * frame_bytes]);
            line.queued += frames;
            Ok(frames)
        })
    }

    fn set_gain(&mut self, stream: &mut usize, gain: AudioGain) -> Result<(), Errno> {
        self.line(*stream, |line| line.gain = Some(gain));
        Ok(())
    }

    fn start(&mut self, stream: &mut usize) -> Result<(), Errno> {
        self.line(*stream, |line| line.started = true);
        Ok(())
    }

    fn drain(&mut self, stream: &mut usize) -> Result<(), Errno> {
        self.line(*stream, |line| line.draining = true);
        Ok(())
    }

    fn next_notify(&mut self, stream: &mut usize) -> Result<Option<AudioNotify>, Errno> {
        Ok(self.line(*stream, |line| line.notes.pop_front()))
    }

    fn close(&mut self, stream: usize) {
        self.line(stream, |line| line.closed = true);
    }
}

/// Every audited event id.
#[derive(Clone, Default)]
struct Audit(Rc<RefCell<Vec<EventId>>>);

impl Sink for Audit {
    fn write_event(&self, event: &Event<'_>) {
        self.0.borrow_mut().push(event.id);
    }
}

impl Audit {
    fn count(&self, id: EventId) -> usize {
        self.0.borrow().iter().filter(|seen| **seen == id).count()
    }
}

type Authority =
    CueAuthority<Store, Fake, LoopbackSessionLauncher<fn() -> AudioDecodeService>, Audit>;

/// An authority over `store` playing into `mixer`.
fn authority(store: &Store, mixer: &Fake, audit: &Audit) -> Authority {
    seeded(store, mixer, audit, || Some(HashSeed::from_words(1, 2)))
}

/// [`authority`], its decoder keyed by what `seed` draws.
fn seeded(store: &Store, mixer: &Fake, audit: &Audit, seed: fn() -> Option<HashSeed>) -> Authority {
    let launcher: LoopbackSessionLauncher<fn() -> AudioDecodeService> =
        LoopbackSessionLauncher::new(AudioDecodeService::new);
    let decoder = Decoder::new(
        SupervisedSession::new(launcher, session_bounds().expect("bounds"), audit.clone()),
        seed,
    );
    let cache = cue_cache(
        1 << 32,
        &Unpressured,
        &DiscardSink,
        BuildSipHash13::with_seed(HashSeed::from_words(3, 4)),
    );
    CueAuthority::new(store.clone(), mixer.clone(), decoder, cache, audit.clone())
}

/// A volume holding the shipped theme, every asset a short chime of its own
/// length.
fn shipped() -> Store {
    let store = Store::default();
    for (at, asset) in SoundAsset::ALL.into_iter().enumerate() {
        let path = asset_path(DEFAULT_THEME, asset).expect("a legal theme");
        store.put(&path, flac(&chime(1_000 + at * 100)));
    }
    store
}

fn origin(uid: u32, pid: u64, caps: &[CapabilityId]) -> Origin {
    let mut summary = CapabilitySummary::EMPTY;
    for cap in caps {
        summary.insert(*cap);
    }
    Origin::new(
        TrustDomain::User,
        uid,
        uid,
        pid,
        ProcId::from_raw([u8::try_from(pid % 251).expect("small"); 16]),
        summary,
        0,
    )
}

fn session(tag: u8) -> ProcId {
    ProcId::from_raw([tag; 16])
}

/// An application in the login session tagged `tag`.
fn app(uid: u32, tag: u8) -> Origin {
    origin(uid, 0x2000 + u64::from(tag), &[]).with_login_session(session(tag))
}

fn frame(request: &CueRequest<'_>) -> Vec<u8> {
    let mut out = [0u8; CUE_REQUEST_MAX];
    let len = request.encode(&mut out).expect("encodes");
    out[..len].to_vec()
}

/// Ask for `event` at `now`, with nobody at the seat, running any decode to
/// its end, and answer what the authority said — at once, or once the decode
/// let it.
fn ask(
    authority: &mut Authority,
    now: u64,
    caller: &Origin,
    event: SoundEvent,
    answer: Answer,
) -> Option<Result<CueOutcome, Errno>> {
    ask_at(authority, now, caller, event, answer, None)
}

/// [`ask`], while `seat` is the login session at the seat.
fn ask_at(
    authority: &mut Authority,
    now: u64,
    caller: &Origin,
    event: SoundEvent,
    answer: Answer,
    seat: Option<ProcId>,
) -> Option<Result<CueOutcome, Errno>> {
    let request = frame(&CueRequest::Cue { event, answer });
    if let Some(reply) = authority.serve(now, caller, 99, &request, seat) {
        return Some(decode_cue_reply(reply.as_bytes()));
    }
    for _ in 0..1_000 {
        authority.on_decoder(now, true, true);
        if let Some(reply) = taken(authority) {
            return Some(reply);
        }
    }
    None
}

/// The deferred answer to call 99, if one is due.
fn taken(authority: &mut Authority) -> Option<Result<CueOutcome, Errno>> {
    authority
        .take_replies()
        .find(|(ticket, _)| *ticket == 99)
        .map(|(_, reply)| decode_cue_reply(&reply))
}

const ALICE: u32 = 1000;
const BIG_RING: u32 = 1 << 16;

// ---------------------------------------------------------------- authority

/// A sound claiming the machine did something can only have come from the
/// principal that did it.
#[test]
fn a_lifecycle_sound_is_refused_to_anyone_but_its_owner() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let lifecycle = [
        SoundEvent::Startup,
        SoundEvent::Shutdown,
        SoundEvent::Login,
        SoundEvent::Logout,
        SoundEvent::DeviceAttached,
        SoundEvent::DeviceDetached,
    ];
    for event in lifecycle {
        assert_eq!(
            ask(&mut authority, 0, &app(ALICE, 0xA1), event, Answer::Started),
            Some(Err(Errno::PermissionDenied)),
            "{event:?}"
        );
    }
    assert_eq!(audit.count(events::CUE_REFUSED), lifecycle.len());
    assert_eq!(authority.tally().refused, lifecycle.len() as u64);
    assert_eq!(mixer.opened(), 0, "nothing was played for the impostor");

    let owners = [
        (SoundEvent::Startup, origin(0, INIT_PID, &[])),
        (
            SoundEvent::Shutdown,
            origin(0, 0x3000, &[CapabilityId::SYSTEM_POWER]),
        ),
        (SoundEvent::Login, origin(LOGIN_UID, 0x3001, &[])),
        (SoundEvent::Logout, origin(LOGIN_UID, 0x3001, &[])),
        (SoundEvent::DeviceAttached, origin(DEVMGR_UID, 0x3002, &[])),
        (SoundEvent::DeviceDetached, origin(DEVMGR_UID, 0x3002, &[])),
    ];
    for (at, (event, owner)) in owners.iter().enumerate() {
        let now = (at as u64 + 1) * 10 * MIN_RETRIGGER_NS;
        assert_eq!(
            ask(&mut authority, now, owner, *event, Answer::Started),
            Some(Ok(CueOutcome::Played)),
            "{event:?}"
        );
        let opened = mixer.line(at, |line| line.params);
        assert_eq!(opened.tenancy, Tenancy::Machine, "{event:?}");
        assert_eq!(opened.role, StreamRole::Notification);
        assert_eq!(opened.direction, StreamDirection::Playback);
        mixer.report(at, StreamState::DeviceLost);
        authority.on_stream(0, mixer.mailbox(&at));
    }
}

/// An application's sound is its session's, which the audio service admits
/// only to the room that session holds; a daemon's is no session's.
#[test]
fn an_application_sound_plays_for_its_callers_session() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 0xA1),
            SoundEvent::Bell,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(
        mixer.line(0, |line| line.params.tenancy),
        Tenancy::Session(session(0xA1))
    );
    let daemon = origin(30, 0x4000, &[]);
    assert_eq!(
        ask(
            &mut authority,
            1,
            &daemon,
            SoundEvent::Error,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(mixer.line(1, |line| line.params.tenancy), Tenancy::Caller);

    mixer.0.borrow_mut().refuse = Some(Errno::SeatNotOwner);
    assert_eq!(
        ask(
            &mut authority,
            2,
            &app(ALICE, 0xB0),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Err(Errno::SeatNotOwner)),
        "the room's answer is the caller's"
    );
}

// ---------------------------------------------------------------- silence

#[test]
fn a_sound_turned_off_on_the_machine_is_silent_and_says_so() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    store.put(MACHINE_SETTINGS, b"sound.bell.enabled = false\n".to_vec());
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Bell,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled)))
    );
    assert_eq!(
        ask(
            &mut authority,
            1,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played)),
        "only the one event is off"
    );
    // The document is read again only when it changes.
    store.put(MACHINE_SETTINGS, b"sound.enabled = off\n".to_vec());
    assert_eq!(
        ask(
            &mut authority,
            2,
            &app(ALICE, 1),
            SoundEvent::Error,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled)))
    );
    assert_eq!(store.reads(MACHINE_SETTINGS), 2);
    // A document that is not text leaves the last one read in force.
    store.put(MACHINE_SETTINGS, alloc::vec![0xFF, 0xFE, b'\n']);
    assert_eq!(
        ask(
            &mut authority,
            3,
            &app(ALICE, 1),
            SoundEvent::Warning,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled)))
    );
    assert_eq!(audit.count(events::SETTINGS_REFUSED), 1);
    assert_eq!(authority.tally().disabled, 3);
}

#[test]
fn a_sound_no_theme_holds_is_silent_and_recorded() {
    let (store, mixer, audit) = (Store::default(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Bell,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::NoAsset)))
    );
    assert_eq!(audit.count(events::CUE_NO_ASSET), 1);
    assert_eq!(authority.tally().no_asset, 1);
}

/// The active theme's asset first, the shipped theme's where it has none.
#[test]
fn a_theme_missing_an_asset_falls_back_to_the_shipped_one() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let own = asset_path("Bright", SoundAsset::Error).expect("legal");
    store.put(&own, flac(&chime(321)));
    store.put(MACHINE_SETTINGS, b"sound.theme = Bright\n".to_vec());
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Error,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(store.reads(&own), 1, "the active theme's own asset");
    assert_eq!(mixer.line(0, |line| line.played.len()), 321 * 4);
    assert_eq!(
        ask(
            &mut authority,
            1,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    let shipped_info = asset_path(DEFAULT_THEME, SoundAsset::Info).expect("legal");
    assert_eq!(store.reads(&shipped_info), 1);
}

/// A sound that does not decode is reported rather than covered up, and the
/// broken file is not decoded again for every cue.
#[test]
fn an_undecodable_sound_is_silent_and_not_decoded_again() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let bell = asset_path(DEFAULT_THEME, SoundAsset::NotificationSoft).expect("legal");
    store.put(&bell, b"fLaC but nothing after it".to_vec());
    let mut authority = authority(&store, &mixer, &audit);
    for at in 0..3 {
        assert_eq!(
            ask(
                &mut authority,
                at * MIN_RETRIGGER_NS,
                &app(ALICE, 1),
                SoundEvent::Bell,
                Answer::Started
            ),
            Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
        );
    }
    assert_eq!(store.reads(&bell), 1);
    assert_eq!(audit.count(events::CUE_UNDECODABLE), 1);
    assert_eq!(authority.tally().undecodable, 3);
    assert_eq!(mixer.opened(), 0);
}

#[test]
fn a_machine_with_no_output_is_silent_and_says_so() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    mixer.0.borrow_mut().refuse = Some(Errno::DeviceOffline);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::NoDevice)))
    );
    assert_eq!(authority.tally().no_device, 1);
}

// ---------------------------------------------------------------- bounds

/// End every cue still sounding, as the room taking them would.
fn end_all(mixer: &Fake, authority: &mut Authority) {
    for line in 0..mixer.opened() {
        if !mixer.line(line, |line| line.closed) {
            mixer.report(line, StreamState::SeatInactive);
            authority.on_stream(0, mixer.mailbox(&line));
        }
    }
}

/// A repeat and a cue past the concurrent bound are dropped and counted,
/// never queued.
#[test]
fn a_repeat_and_a_cue_past_the_cap_are_dropped_and_counted() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let alice = app(ALICE, 1);
    assert_eq!(
        ask(&mut authority, 0, &alice, SoundEvent::Info, Answer::Started),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(
        ask(&mut authority, 1, &alice, SoundEvent::Info, Answer::Started),
        Some(Ok(CueOutcome::Silent(Silence::Coalesced)))
    );
    for event in [
        SoundEvent::Error,
        SoundEvent::Warning,
        SoundEvent::Notification,
    ] {
        assert_eq!(
            ask(&mut authority, 2, &alice, event, Answer::Started),
            Some(Ok(CueOutcome::Played))
        );
    }
    let bob = app(ALICE + 1, 2);
    assert_eq!(
        ask(&mut authority, 3, &bob, SoundEvent::Bell, Answer::Started),
        Some(Ok(CueOutcome::Silent(Silence::TooMany))),
        "{MAX_CONCURRENT_CUES} are already sounding"
    );
    end_all(&mixer, &mut authority);
    assert_eq!(
        ask(&mut authority, 4, &bob, SoundEvent::Bell, Answer::Started),
        Some(Ok(CueOutcome::Silent(Silence::Coalesced))),
        "the repeat is the occurrence that was dropped"
    );
    assert_eq!(
        ask(
            &mut authority,
            3 + MIN_RETRIGGER_NS,
            &bob,
            SoundEvent::Bell,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    let Tally {
        coalesced,
        too_many,
        played,
        ..
    } = authority.tally();
    assert_eq!((coalesced, too_many, played), (2, 1, 5));
}

/// A principal past its burst is refused, and the flood recorded once.
#[test]
fn a_principal_past_its_rate_is_refused_and_recorded_once() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let alice = app(ALICE, 1);
    let mut events = SoundEvent::ALL
        .into_iter()
        .filter(|event| event.authority() == tairix_abi::sound_cue::CueAuthority::Application)
        .cycle();
    // A burst's worth of distinct cues, the last a repeat just past its
    // retrigger interval and still short of the first cue earned back.
    const { assert!(MIN_RETRIGGER_NS + 2 < CUE_REFILL_NS) };
    let burst = u64::from(CUE_BURST);
    for at in 0..burst {
        let event = events.next().expect("cycles");
        let now = if at + 1 == burst { MIN_RETRIGGER_NS } else { 0 };
        assert_eq!(
            ask(&mut authority, now, &alice, event, Answer::Started),
            Some(Ok(CueOutcome::Played)),
            "{event:?}"
        );
        end_all(&mixer, &mut authority);
    }
    for at in 1..=2 {
        let event = events.next().expect("cycles");
        assert_eq!(
            ask(
                &mut authority,
                MIN_RETRIGGER_NS + at,
                &alice,
                event,
                Answer::Started
            ),
            Some(Err(Errno::LimitExceeded))
        );
    }
    assert_eq!(authority.tally().limited, 2);
    assert_eq!(audit.count(events::CUE_LIMITED), 1);
}

// ---------------------------------------------------------------- playing

/// A sound longer than its ring is fed as the device frees room, drained once
/// all of it is queued, and closed once it has played out: every sample
/// reaches the stream once, in order, at unity.
#[test]
fn a_long_sound_is_refilled_drained_and_closed() {
    let (store, mixer, audit) = (Store::default(), Fake::new(1_024), Audit::default());
    let samples = chime(5_000);
    let path = asset_path(DEFAULT_THEME, SoundAsset::Info).expect("legal");
    store.put(&path, flac(&samples));
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::PlayedOut
        ),
        None,
        "answered once it has played out"
    );
    assert!(mixer.line(0, |line| line.started && !line.draining));
    assert_eq!(mixer.line(0, |line| line.gain), None, "unity is left alone");
    while !mixer.line(0, |line| line.draining) {
        mixer.consume(0, 1_024);
        authority.on_stream(0, mixer.mailbox(&0));
    }
    let expected: Vec<u8> = samples
        .iter()
        .flat_map(|&s| i16::try_from(s).expect("16-bit").to_le_bytes())
        .collect();
    assert_eq!(mixer.line(0, |line| line.played.clone()), expected);
    assert_eq!(taken(&mut authority), None, "still playing out");
    mixer.report(0, StreamState::Idle);
    authority.on_stream(0, mixer.mailbox(&0));
    assert_eq!(taken(&mut authority), Some(Ok(CueOutcome::Played)));
    assert!(mixer.line(0, |line| line.closed));
    assert_eq!(authority.mailboxes().count(), 0);
}

/// A machine's sound that plays out is recorded, so the machine announcing a
/// transition aloud leaves a trace; one that broke off, and a session's
/// sound, leave none.
#[test]
fn a_machine_sound_that_played_out_is_recorded_and_no_other() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let init = origin(0, INIT_PID, &[]);
    let login = origin(LOGIN_UID, 0x3001, &[]);
    let apart = 10 * MIN_RETRIGGER_NS;
    let cues = [
        (0, &init, SoundEvent::Startup, StreamState::Idle),
        (apart, &login, SoundEvent::Login, StreamState::DeviceLost),
        (
            2 * apart,
            &app(ALICE, 1),
            SoundEvent::Info,
            StreamState::Idle,
        ),
    ];
    for (line, (now, caller, event, ending)) in cues.into_iter().enumerate() {
        assert_eq!(
            ask(&mut authority, now, caller, event, Answer::Started),
            Some(Ok(CueOutcome::Played)),
            "{event:?}"
        );
        assert!(
            mixer.line(line, |line| line.draining),
            "the whole sound is queued"
        );
        mixer.report(line, ending);
        authority.on_stream(0, mixer.mailbox(&line));
        assert!(mixer.line(line, |line| line.closed));
    }
    assert_eq!(audit.count(events::MACHINE_CUE_PLAYED), 1);
}

/// The cached decode serves the next cue: the file is read and decoded once.
#[test]
fn a_sound_decodes_once_and_plays_from_the_cache_after() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let path = asset_path(DEFAULT_THEME, SoundAsset::Warning).expect("legal");
    for at in 0..3 {
        assert_eq!(
            ask(
                &mut authority,
                at * MIN_RETRIGGER_NS,
                &app(ALICE, 1),
                SoundEvent::Warning,
                Answer::Started
            ),
            Some(Ok(CueOutcome::Played))
        );
        mixer.report(usize::try_from(at).expect("small"), StreamState::DeviceLost);
        authority.on_stream(0, mixer.mailbox(&usize::try_from(at).expect("small")));
    }
    assert_eq!(store.reads(&path), 1);
    // A file replaced under its name is a different sound.
    store.put(&path, flac(&chime(777)));
    assert_eq!(
        ask(
            &mut authority,
            10 * MIN_RETRIGGER_NS,
            &app(ALICE, 1),
            SoundEvent::Warning,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(store.reads(&path), 2);
    assert_eq!(mixer.line(3, |line| line.played.len()), 777 * 4);
}

/// A sound that does not play out by its deadline is stopped, and whoever
/// waited on it is answered: shutdown proceeds regardless.
#[test]
fn a_sound_overrunning_its_deadline_is_stopped_and_its_waiter_answered() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let power = origin(0, 0x3000, &[CapabilityId::SYSTEM_POWER]);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &power,
            SoundEvent::Shutdown,
            Answer::PlayedOut
        ),
        None
    );
    let deadline = authority
        .deadline()
        .expect("a playing sound has a deadline");
    assert!(deadline > DRAIN_SLACK_NS);
    authority.on_timer(deadline - 1);
    assert_eq!(taken(&mut authority), None);
    authority.on_timer(deadline);
    assert_eq!(taken(&mut authority), Some(Ok(CueOutcome::Played)));
    assert!(mixer.line(0, |line| line.closed));
    assert_eq!(audit.count(events::CUE_CUT), 1);
}

/// A sound that played out while the authority was busy elsewhere is retired
/// as played when its deadline comes, not cut: what its mailbox holds is read
/// before the deadline is kept, from the timer and from the mailbox's own
/// wake alike.
#[test]
fn a_sound_that_played_out_unread_is_never_cut() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let power = origin(0, 0x3000, &[CapabilityId::SYSTEM_POWER]);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &power,
            SoundEvent::Shutdown,
            Answer::PlayedOut
        ),
        None
    );
    let deadline = authority
        .deadline()
        .expect("a playing sound has a deadline");
    mixer.report(0, StreamState::Idle);
    authority.on_timer(deadline);
    assert_eq!(taken(&mut authority), Some(Ok(CueOutcome::Played)));
    assert_eq!(audit.count(events::MACHINE_CUE_PLAYED), 1);

    assert_eq!(
        ask(
            &mut authority,
            deadline,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    let late = authority
        .deadline()
        .expect("a playing sound has a deadline");
    mixer.report(1, StreamState::Idle);
    authority.on_stream(late, mixer.mailbox(&1));
    assert!(mixer.line(1, |line| line.closed));
    assert_eq!(audit.count(events::CUE_CUT), 0, "neither sound was cut");
}

/// A burst of one sound — the nine disks attaching at once on the 2026-10-17
/// Pi 4 boot — is heard once: each repeat joins the sound already sounding
/// rather than opening a stream over it, and every caller waiting on it is
/// answered when it plays out.
#[test]
fn a_burst_of_one_sound_is_heard_once_and_every_waiter_answered() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    for uid in 0..3 {
        assert_eq!(
            ask(
                &mut authority,
                0,
                &app(ALICE + uid, 1),
                SoundEvent::Info,
                Answer::PlayedOut
            ),
            None
        );
    }
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE + 3, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::Coalesced))),
        "a caller waiting only for the start is answered at once"
    );
    assert_eq!(mixer.opened(), 1, "one stream sounds the burst");

    mixer.report(0, StreamState::Idle);
    authority.on_stream(0, mixer.mailbox(&0));
    let answered: Vec<_> = authority
        .take_replies()
        .map(|(_, reply)| decode_cue_reply(&reply))
        .collect();
    assert_eq!(answered, vec![Ok(CueOutcome::Played); 3]);
    assert_eq!(authority.tally().coalesced, 3);
    assert_eq!(audit.count(events::CUE_CUT), 0);
}

/// A repeat for another room is that room's own sound, never joined to one
/// it would not be heard in.
#[test]
fn a_repeat_for_another_room_sounds_on_its_own() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    for tag in [1, 2] {
        assert_eq!(
            ask(
                &mut authority,
                0,
                &app(ALICE + u32::from(tag), tag),
                SoundEvent::Info,
                Answer::Started
            ),
            Some(Ok(CueOutcome::Played))
        );
    }
    assert_eq!(mixer.opened(), 2);
}

/// A notification a closed stream left in a mailbox ends nothing: it names
/// the stream it was about, and a later cue reads only its own.
#[test]
fn a_closed_streams_notification_ends_no_later_cue() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let alice = app(ALICE, 1);
    assert_eq!(
        ask(&mut authority, 0, &alice, SoundEvent::Info, Answer::Started),
        Some(Ok(CueOutcome::Played))
    );
    mixer.report(0, StreamState::Idle);
    authority.on_stream(0, mixer.mailbox(&0));
    assert!(mixer.line(0, |line| line.closed));
    assert_eq!(
        ask(
            &mut authority,
            0,
            &alice,
            SoundEvent::Warning,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );

    mixer.line(1, |line| {
        line.notes.push_back(AudioNotify::StateChanged {
            stream_id: 1,
            state: StreamState::Idle,
            at: Frames::ZERO,
        });
    });
    authority.on_stream(0, mixer.mailbox(&1));

    assert!(
        !mixer.line(1, |line| line.closed),
        "the first stream's Idle ended nothing of the second"
    );
}

#[test]
fn the_settings_gain_is_the_streams() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    store.put(MACHINE_SETTINGS, b"sound.gain = -6dB\n".to_vec());
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(
        mixer.line(0, |line| line.gain),
        Some(AudioGain::new(-600).expect("attenuation"))
    );
}

#[test]
fn a_malformed_request_is_refused() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let reply = authority
        .serve(0, &app(ALICE, 1), 1, b"not a cue", None)
        .expect("answered at once");
    assert!(decode_cue_reply(reply.as_bytes()).is_err());
    assert_eq!(mixer.opened(), 0);
}

// ---------------------------------------------------------------- the user's layer

/// The desktop that anchors the login session tagged `tag`: the session is
/// named by its own instance.
fn desktop(uid: u32, tag: u8) -> Origin {
    Origin::new(
        TrustDomain::User,
        uid,
        uid,
        0x5000 + u64::from(tag),
        session(tag),
        CapabilitySummary::EMPTY,
        0,
    )
    .with_login_session(session(tag))
}

/// Push `document` and `files` from `caller` while `seat` is at the seat.
fn push(
    authority: &mut Authority,
    caller: &Origin,
    seat: Option<ProcId>,
    document: &str,
    files: [u64; 13],
) -> Result<(), Errno> {
    let request = frame(&CueRequest::Policy { document, files });
    let reply = authority
        .serve(0, caller, 1, &request, seat)
        .expect("a policy is answered at once");
    decode_status_reply(reply.as_bytes())
}

fn with_file(event: SoundEvent, handle: u64) -> [u64; 13] {
    let mut files = [0u64; 13];
    files[event.index()] = handle;
    files
}

const A: u8 = 0xA1;
const B: u8 = 0xB2;

/// The session at the seat decides every sound heard while it holds it, but
/// not the machine's start or a session's start and end, which sound when no
/// session's choices are in force.
#[test]
fn the_seats_session_governs_every_sound_but_the_machines_moments() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let seat = Some(session(A));
    assert_eq!(
        push(
            &mut authority,
            &desktop(ALICE, A),
            seat,
            "sound.enabled = false\n",
            [0; 13]
        ),
        Ok(())
    );
    assert!(authority.holds_choices());
    assert_eq!(audit.count(events::POLICY_ADOPTED), 1);
    let silent = Some(Ok(CueOutcome::Silent(Silence::Disabled)));
    let bell = ask_at(
        &mut authority,
        0,
        &app(ALICE, A),
        SoundEvent::Bell,
        Answer::Started,
        seat,
    );
    assert_eq!(bell, silent);
    let devmgr = origin(DEVMGR_UID, 0x3002, &[]);
    let attached = ask_at(
        &mut authority,
        1,
        &devmgr,
        SoundEvent::DeviceAttached,
        Answer::Started,
        seat,
    );
    assert_eq!(attached, silent, "a machine sound heard at the seat");
    let login = origin(LOGIN_UID, 0x3001, &[]);
    assert_eq!(
        ask_at(
            &mut authority,
            2,
            &login,
            SoundEvent::Login,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Played)),
        "a session's start is the machine's to sound"
    );
}

/// Only the anchor of the login session the lease names may push, and a
/// refused push leaves none of its delegations pending.
#[test]
fn only_the_anchor_of_the_session_at_the_seat_may_push() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    store.put("/Users/ada/klaxon.wav", wav(&chime(600)));
    let pushers = [
        (app(ALICE, A), Some(session(A))),
        (desktop(ALICE, B), Some(session(A))),
        (desktop(ALICE, A), None),
        (desktop(ALICE, A), Some(session(B))),
    ];
    for (at, (caller, seat)) in pushers.iter().enumerate() {
        let handle = at as u64 + 1;
        store.grant(handle, caller.proc_id(), "/Users/ada/klaxon.wav");
        assert_eq!(
            push(
                &mut authority,
                caller,
                *seat,
                "",
                with_file(SoundEvent::Error, handle)
            ),
            Err(Errno::SeatNotOwner),
            "{at}"
        );
        assert_eq!(
            store.pending(),
            0,
            "the refused push's delegation was consumed"
        );
        assert_eq!(store.open(), 0, "and closed");
    }
    assert!(!authority.holds_choices());
    assert_eq!(audit.count(events::POLICY_REFUSED), pushers.len());
}

/// A pushed document naming a file, or anything of a sound no user's choices
/// reach, is refused whole, and the choices already in force stay so.
#[test]
fn a_push_naming_a_file_or_a_machine_moment_is_refused_whole() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "sound.bell.enabled = false\n",
            [0; 13]
        ),
        Ok(())
    );
    store.grant(5, anchor.proc_id(), "/Users/ada/hello.wav");
    for (document, files) in [
        ("sound.bell.file = /Users/ada/bell.wav\n", [0; 13]),
        ("sound.login.enabled = false\n", [0; 13]),
        ("sound.gain = 6dB\n", [0; 13]),
        ("sound.info.gain = -6dB\n", [0; 13]),
        ("", with_file(SoundEvent::Login, 5)),
    ] {
        assert_eq!(
            push(&mut authority, &anchor, seat, document, files),
            Err(Errno::OutOfRange),
            "{document:?}"
        );
    }
    assert_eq!(store.pending(), 0);
    assert_eq!(
        ask_at(
            &mut authority,
            0,
            &app(ALICE, A),
            SoundEvent::Bell,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled))),
        "the choices in force before the refusals"
    );
}

/// A file the session handed over plays in place of the theme's, under the
/// session's level, in whatever format it is in; when the session leaves the
/// seat its files close and its sounds are forgotten.
#[test]
fn a_handed_file_plays_in_place_of_the_theme_and_leaves_with_the_session() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let path = "/Users/ada/klaxon.wav";
    store.put(path, wav(&chime(600)));
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    store.grant(7, anchor.proc_id(), path);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "sound.gain = -6dB\n",
            with_file(SoundEvent::Error, 7)
        ),
        Ok(())
    );
    assert_eq!((store.pending(), store.open()), (0, 1));
    let error = |authority: &mut Authority, now, seat| {
        ask_at(
            authority,
            now,
            &app(ALICE, A),
            SoundEvent::Error,
            Answer::Started,
            seat,
        )
    };
    assert_eq!(error(&mut authority, 0, seat), Some(Ok(CueOutcome::Played)));
    assert_eq!(mixer.line(0, |line| line.played.len()), 600 * 4);
    assert_eq!(
        mixer.line(0, |line| line.gain),
        Some(AudioGain::new(-600).expect("attenuation"))
    );
    assert_eq!(store.reads(path), 1);
    end_all(&mixer, &mut authority);

    authority.seat_moved(0, Some(session(B)));
    assert_eq!(store.open(), 0, "its files closed");
    assert_eq!(audit.count(events::POLICY_DROPPED), 1);
    let theirs = ask_at(
        &mut authority,
        10 * MIN_RETRIGGER_NS,
        &app(ALICE, B),
        SoundEvent::Error,
        Answer::Started,
        Some(session(B)),
    );
    assert_eq!(theirs, Some(Ok(CueOutcome::Played)));
    assert_ne!(
        mixer.line(1, |line| line.played.len()),
        600 * 4,
        "the theme's sound, at the machine's level"
    );
    assert_eq!(mixer.line(1, |line| line.gain), None);
    end_all(&mixer, &mut authority);

    // Back at the seat, its file is read afresh: nothing of it was kept.
    store.grant(8, anchor.proc_id(), path);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::Error, 8)
        ),
        Ok(())
    );
    assert_eq!(
        error(&mut authority, 20 * MIN_RETRIGGER_NS, seat),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(store.reads(path), 2);
}

/// The holder is read again before every request, so a cue arriving after the
/// seat moved is never governed by the session that left it.
#[test]
fn the_seats_holder_is_read_again_before_every_request() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let seat = Some(session(A));
    assert_eq!(
        push(
            &mut authority,
            &desktop(ALICE, A),
            seat,
            "sound.bell.enabled = off\n",
            [0; 13]
        ),
        Ok(())
    );
    let moved = Some(session(B));
    assert_eq!(
        ask_at(
            &mut authority,
            0,
            &app(ALICE, B),
            SoundEvent::Bell,
            Answer::Started,
            moved
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert!(!authority.holds_choices());
}

/// What the user left is the machine's: a change to the machine's document is
/// seen through the choices laid over it.
#[test]
fn the_machines_document_changing_beneath_the_choices_is_seen_through_them() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let seat = Some(session(A));
    assert_eq!(
        push(
            &mut authority,
            &desktop(ALICE, A),
            seat,
            "sound.gain = -6dB\n",
            [0; 13]
        ),
        Ok(())
    );
    store.put(
        MACHINE_SETTINGS,
        b"sound.info.enabled = false\nsound.gain = -3dB\n".to_vec(),
    );
    assert_eq!(
        ask_at(
            &mut authority,
            0,
            &app(ALICE, A),
            SoundEvent::Info,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled)))
    );
    assert_eq!(
        ask_at(
            &mut authority,
            1,
            &app(ALICE, A),
            SoundEvent::Bell,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(
        mixer.line(0, |line| line.gain),
        Some(AudioGain::new(-600).expect("attenuation")),
        "the user's own level over the machine's"
    );
}

/// A file the machine's document names for an event is the administrator's
/// replacement, read by the authority itself and played for everyone.
#[test]
fn a_replacement_the_machine_names_plays_and_a_missing_one_is_said() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let path = "/System/Settings/Sound/info.wav";
    store.put(path, wav(&chime(500)));
    store.put(
        MACHINE_SETTINGS,
        b"sound.info.file = /System/Settings/Sound/info.wav\nsound.bell.file = /nope.wav\n"
            .to_vec(),
    );
    let mut authority = authority(&store, &mixer, &audit);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &app(ALICE, A),
            SoundEvent::Info,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Played))
    );
    assert_eq!(mixer.line(0, |line| line.played.len()), 500 * 4);
    assert_eq!(store.reads(path), 1);
    assert_eq!(
        ask(
            &mut authority,
            1,
            &app(ALICE, A),
            SoundEvent::Bell,
            Answer::Started
        ),
        Some(Ok(CueOutcome::Silent(Silence::Undecodable))),
        "reported, never covered by the theme's"
    );
    assert_eq!(audit.count(events::CUE_UNDECODABLE), 1);
}

/// A handed file that cannot be looked at is silent and said, as a theme's
/// would be.
#[test]
fn a_handed_file_that_cannot_be_read_is_silent_and_said() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    store.grant(3, anchor.proc_id(), "/Users/ada/gone.wav");
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::Warning, 3)
        ),
        Ok(())
    );
    assert_eq!(
        ask_at(
            &mut authority,
            0,
            &app(ALICE, A),
            SoundEvent::Warning,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
    );
    assert_eq!(audit.count(events::CUE_UNDECODABLE), 1);
    assert_eq!(mixer.opened(), 0);
}

/// A session pushes again on every change its user makes: what was known of a
/// file it hands over unchanged — its decode, or that it does not decode — is
/// kept, and a file changed since is read afresh.
#[test]
fn a_session_pushing_again_keeps_what_was_known_of_its_unchanged_files() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let chosen = [
        (SoundEvent::Error, "/Users/ada/klaxon.wav"),
        (SoundEvent::Warning, "/Users/ada/chirp.wav"),
        (SoundEvent::Info, "/Users/ada/broken.wav"),
    ];
    store.put(chosen[0].1, wav(&chime(600)));
    store.put(chosen[1].1, wav(&chime(300)));
    store.put(chosen[2].1, b"RIFF and nothing after it".to_vec());
    let hand_over = |first: u64| {
        let mut files = [0u64; 13];
        for (handle, (event, path)) in (first..).zip(chosen) {
            store.grant(handle, anchor.proc_id(), path);
            files[event.index()] = handle;
        }
        files
    };
    let mut now = 0;
    let mut sound_each = |authority: &mut Authority| {
        chosen.map(|(event, _)| {
            now += CUE_REFILL_NS.max(MIN_RETRIGGER_NS);
            let outcome = ask_at(authority, now, &app(ALICE, A), event, Answer::Started, seat);
            end_all(&mixer, authority);
            outcome
        })
    };
    let (played, refused) = (
        Some(Ok(CueOutcome::Played)),
        Some(Ok(CueOutcome::Silent(Silence::Undecodable))),
    );
    assert_eq!(
        push(&mut authority, &anchor, seat, "", hand_over(1)),
        Ok(())
    );
    assert_eq!(sound_each(&mut authority), [played, played, refused]);

    store.put(chosen[1].1, wav(&chime(450)));
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "sound.gain = -6dB\n",
            hand_over(10)
        ),
        Ok(())
    );
    assert_eq!(
        (store.pending(), store.open()),
        (0, 3),
        "the earlier delegations closed"
    );
    assert_eq!(sound_each(&mut authority), [played, played, refused]);
    let reads = chosen.map(|(_, path)| store.reads(path));
    assert_eq!(reads, [1, 2, 1], "only the changed file is read again");
    assert_eq!(audit.count(events::CUE_UNDECODABLE), 1);
    assert_eq!(mixer.line(2, |line| line.played.len()), 600 * 4);
    assert_eq!(
        mixer.line(2, |line| line.gain),
        Some(AudioGain::new(-600).expect("attenuation")),
        "the kept decode plays under the new level"
    );
    assert_eq!(mixer.line(3, |line| line.played.len()), 450 * 4);
}

/// A decode that lands after its session left the seat is not held against
/// the session's return: nothing of the session was kept.
#[test]
fn a_refusal_landing_after_its_session_left_is_not_held_against_its_return() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let broken = "/Users/ada/broken.wav";
    store.put(broken, b"RIFF and nothing after it".to_vec());
    store.grant(1, anchor.proc_id(), broken);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::Warning, 1)
        ),
        Ok(())
    );
    let request = frame(&CueRequest::Cue {
        event: SoundEvent::Warning,
        answer: Answer::Started,
    });
    assert_eq!(
        authority.serve(0, &app(ALICE, A), 99, &request, seat),
        None,
        "the decode is under way"
    );
    authority.seat_moved(0, Some(session(B)));
    let mut answered = None;
    for _ in 0..1_000 {
        authority.on_decoder(0, true, true);
        answered = answered.or_else(|| taken(&mut authority));
    }
    assert_eq!(answered, Some(Ok(CueOutcome::Silent(Silence::Undecodable))));

    store.grant(2, anchor.proc_id(), broken);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::Warning, 2)
        ),
        Ok(())
    );
    assert_eq!(
        ask_at(
            &mut authority,
            MIN_RETRIGGER_NS,
            &app(ALICE, A),
            SoundEvent::Warning,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
    );
    assert_eq!(store.reads(broken), 2, "back at the seat, read afresh");
    assert_eq!(mixer.opened(), 0);
}

/// A refused push is answered with the status word alone, as every push is,
/// so its sender reads the refusal rather than a cue reply it never asked for.
#[test]
fn a_malformed_push_is_refused_in_a_pushes_shape() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let mut request = frame(&CueRequest::Policy {
        document: "",
        files: [0; 13],
    });
    request.push(b'x');
    let reply = authority
        .serve(0, &desktop(ALICE, A), 1, &request, Some(session(A)))
        .expect("refused at once");
    assert_eq!(reply.as_bytes().len(), STATUS_REPLY_LEN);
    assert_eq!(
        decode_status_reply(reply.as_bytes()),
        Err(Errno::LengthOutOfRange)
    );
    assert!(!authority.holds_choices());
}

/// One file chosen for two events is one delegation: it is redeemed once and
/// plays for both.
#[test]
fn a_file_chosen_for_two_events_is_redeemed_once_and_plays_for_both() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let path = "/Users/ada/ding.wav";
    store.put(path, wav(&chime(500)));
    store.grant(5, anchor.proc_id(), path);
    let mut files = [0u64; 13];
    files[SoundEvent::Notification.index()] = 5;
    files[SoundEvent::Bell.index()] = 5;
    assert_eq!(push(&mut authority, &anchor, seat, "", files), Ok(()));
    assert_eq!((store.pending(), store.open()), (0, 1));
    for (line, event) in [SoundEvent::Notification, SoundEvent::Bell]
        .into_iter()
        .enumerate()
    {
        let now = u64::try_from(line).expect("small") * MIN_RETRIGGER_NS;
        assert_eq!(
            ask_at(
                &mut authority,
                now,
                &app(ALICE, A),
                event,
                Answer::Started,
                seat
            ),
            Some(Ok(CueOutcome::Played)),
            "{event:?}"
        );
        assert_eq!(mixer.line(line, |line| line.played.len()), 500 * 4);
    }
}

/// A session's own file is that session's data, so even a machine's moment
/// sounds it only in the session's room; a theme's sound stays the machine's.
#[test]
fn a_machines_moment_sounds_a_sessions_own_file_only_in_its_room() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let path = "/Users/ada/plug.wav";
    store.put(path, wav(&chime(600)));
    store.grant(1, anchor.proc_id(), path);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::DeviceAttached, 1)
        ),
        Ok(())
    );
    let devmgr = origin(DEVMGR_UID, 0x3002, &[]);
    for event in [SoundEvent::DeviceAttached, SoundEvent::DeviceDetached] {
        assert_eq!(
            ask_at(&mut authority, 0, &devmgr, event, Answer::Started, seat),
            Some(Ok(CueOutcome::Played)),
            "{event:?}"
        );
    }
    assert_eq!(mixer.line(0, |line| line.played.len()), 600 * 4);
    assert_eq!(
        mixer.line(0, |line| line.params.tenancy),
        Tenancy::Session(session(A))
    );
    assert_eq!(mixer.line(1, |line| line.params.tenancy), Tenancy::Machine);
}

/// A machine's moment still waiting on a session's file when the seat moves
/// sounds as the seat now decides, never the departed session's file.
#[test]
fn a_machines_moment_waiting_on_a_departed_sessions_file_sounds_as_the_seat_decides() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let path = "/Users/ada/plug.wav";
    store.put(path, wav(&chime(600)));
    store.grant(1, anchor.proc_id(), path);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "",
            with_file(SoundEvent::DeviceAttached, 1)
        ),
        Ok(())
    );
    let request = frame(&CueRequest::Cue {
        event: SoundEvent::DeviceAttached,
        answer: Answer::Started,
    });
    let devmgr = origin(DEVMGR_UID, 0x3002, &[]);
    assert_eq!(authority.serve(0, &devmgr, 99, &request, seat), None);
    authority.seat_moved(0, Some(session(B)));
    let mut answered = None;
    for _ in 0..1_000 {
        authority.on_decoder(0, true, true);
        answered = answered.or_else(|| taken(&mut authority));
    }
    assert_eq!(answered, Some(Ok(CueOutcome::Played)));
    assert_eq!(mixer.opened(), 1);
    assert_ne!(
        mixer.line(0, |line| line.played.len()),
        600 * 4,
        "the machine's sound"
    );
    assert_eq!(mixer.line(0, |line| line.params.tenancy), Tenancy::Machine);
}

/// The choices of the session at the seat decide only what is heard at the
/// seat: another session's cue neither learns them nor reaches their files.
#[test]
fn another_sessions_cue_neither_learns_nor_reaches_the_seats_choices() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let (anchor, seat) = (desktop(ALICE, A), Some(session(A)));
    let path = "/Users/ada/klaxon.wav";
    store.put(path, wav(&chime(600)));
    store.grant(1, anchor.proc_id(), path);
    assert_eq!(
        push(
            &mut authority,
            &anchor,
            seat,
            "sound.bell.enabled = false\n",
            with_file(SoundEvent::Error, 1)
        ),
        Ok(())
    );
    let elsewhere = app(ALICE + 1, B);
    for (at, event) in [SoundEvent::Bell, SoundEvent::Error]
        .into_iter()
        .enumerate()
    {
        let now = u64::try_from(at).expect("small") * MIN_RETRIGGER_NS;
        assert_eq!(
            ask_at(
                &mut authority,
                now,
                &elsewhere,
                event,
                Answer::Started,
                seat
            ),
            Some(Ok(CueOutcome::Played)),
            "{event:?} under the machine's choices"
        );
    }
    assert_eq!(
        store.reads(path),
        0,
        "the seat's own file is never read for it"
    );
    assert_eq!(
        ask_at(
            &mut authority,
            0,
            &app(ALICE, A),
            SoundEvent::Bell,
            Answer::Started,
            seat
        ),
        Some(Ok(CueOutcome::Silent(Silence::Disabled))),
        "the seat's own cue under its own choices"
    );
}

/// A rendezvous kept busy does not hold a sound past its end: whatever wakes
/// the authority keeps the deadlines that have come.
#[test]
fn a_busy_rendezvous_does_not_hold_a_sound_past_its_end() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let power = origin(0, 0x3000, &[CapabilityId::SYSTEM_POWER]);
    assert_eq!(
        ask(
            &mut authority,
            0,
            &power,
            SoundEvent::Shutdown,
            Answer::PlayedOut
        ),
        None
    );
    let deadline = authority.deadline().expect("a sounding cue's deadline");
    assert!(authority
        .serve(deadline, &app(ALICE, 1), 7, b"not a cue", None)
        .is_some());
    assert_eq!(taken(&mut authority), Some(Ok(CueOutcome::Played)));
    assert!(mixer.line(0, |line| line.closed));
    assert_eq!(audit.count(events::CUE_CUT), 1);
}

/// A sound the machine could not read or decode this time is tried again;
/// only one that is itself past a cue's bounds is remembered.
#[test]
fn a_failure_of_the_machine_is_tried_again_and_only_the_sounds_own_is_remembered() {
    let (store, mixer, audit) = (shipped(), Fake::new(BIG_RING), Audit::default());
    let mut authority = authority(&store, &mixer, &audit);
    let info = asset_path(DEFAULT_THEME, SoundAsset::Info).expect("legal");
    let cue = |authority: &mut Authority, at: u64| {
        ask(
            authority,
            at * MIN_RETRIGGER_NS,
            &app(ALICE, 1),
            SoundEvent::Info,
            Answer::Started,
        )
    };
    store.fail_next_read(Errno::MediumError);
    assert_eq!(
        cue(&mut authority, 0),
        Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
    );
    assert_eq!(cue(&mut authority, 1), Some(Ok(CueOutcome::Played)));
    assert_eq!(store.reads(&info), 2);
    end_all(&mixer, &mut authority);

    let warning = asset_path(DEFAULT_THEME, SoundAsset::Warning).expect("legal");
    store.fail_next_read(Errno::LengthOutOfRange);
    for at in 2..4 {
        assert_eq!(
            ask(
                &mut authority,
                at * MIN_RETRIGGER_NS,
                &app(ALICE, 1),
                SoundEvent::Warning,
                Answer::Started
            ),
            Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
        );
    }
    assert_eq!(
        store.reads(&warning),
        1,
        "too long to be a cue, until it changes"
    );

    let mut seedless = seeded(&store, &mixer, &audit, || None);
    let bell = asset_path(DEFAULT_THEME, SoundAsset::NotificationSoft).expect("legal");
    let before = store.reads(&bell);
    for at in 0..2 {
        assert_eq!(
            ask(
                &mut seedless,
                at * MIN_RETRIGGER_NS,
                &app(ALICE, 1),
                SoundEvent::Bell,
                Answer::Started
            ),
            Some(Ok(CueOutcome::Silent(Silence::Undecodable)))
        );
    }
    assert_eq!(store.reads(&bell) - before, 2, "a worker it could not key");
}

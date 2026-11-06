//! Host tests of a recording over a fake recorder: the one order the
//! conversation runs in, the slots granted and released, pacing,
//! back-pressure, the recorder's death and its refusals, a full disk, the
//! lock, the end, and blanking.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::num::NonZeroU64;

use tairix_abi::driver::display::DamageRect;
use tairix_abi::elevate::WiredLaunch;
use tairix_abi::origin::PROC_ID_LEN;
use tairix_abi::recorder_ipc::{
    RecordEnding, RecordSound, RecorderMessage, SessionMessage, RECORDER_FINISH_DEADLINE_NS,
    RECORDER_FRAME_SPACING_NS, RECORDER_MESSAGE_MAX, RECORDER_RUN_PATH, RECORDER_SLOTS,
    RECORDER_START_DEADLINE_NS,
};
use tairix_abi::{Errno, ProcId, LOAD_NOT_FOUND};
use tairix_greeter::{AccountTile, Ticket, Tickets};
use tairix_wm::{Color, Compositor, Point, Rect, Region, Surface};

use super::{
    administrator_request, take_output, Allowed, Lost, RecordEnd, RecordStep, RecorderHost,
    Recording, RecordingFile, StopReason,
};
use crate::capture::{
    CaptureNotice, CaptureTarget, KeptFolder, RecordPlan, Screened, RECORDING_ENDED,
    RECORDING_STARTED,
};
use crate::elevate::{Answer, Asker, Elevation, Elevator, Listing, Request};
use crate::keep::Unkept;
use crate::saver::fixtures::{Pipes, Records};
use crate::saver::SaverMemory;
use crate::tests::{compositor, opaque_window};

/// The recorder's process, as the kernel attests the child the session
/// started.
const RECORDER: ProcId = ProcId::from_raw([7; PROC_ID_LEN]);
const AREA: Rect = Rect::new(0, 0, 64, 48);
const HOME: Option<&str> = Some("/Users/ada");
/// The scratch file the session made the recording's file.
const SCRATCH: &str = "/Users/ada/Home/Videos/.keeping.webm";

type Rec = Recording<Pipes, Records>;

/// A recorder whose pipes are held in memory, attested as [`RECORDER`] — or,
/// one an administrator allowed, as the broker said — whatever it says, and
/// granted slots the session holds on the heap.
#[derive(Default)]
struct FakeRecorder {
    pipes: Pipes,
    refuse: Option<Errno>,
    /// How many recorders it started.
    launched: usize,
    /// The recorders administrators allowed that it took up.
    adopted: Vec<WiredLaunch>,
    /// The grant refused, counted from nought, and why.
    refuse_grant: Option<(usize, Errno)>,
    /// Every slot granted: to which process, under which handle.
    granted: Vec<(ProcId, u64)>,
    /// How many of the slots granted the session still holds.
    held: Rc<Cell<usize>>,
}

/// A slot the session made, on the heap, counted while the session holds it.
struct HeldSlot {
    bytes: Vec<u8>,
    held: Rc<Cell<usize>>,
}

impl SaverMemory for HeldSlot {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl Drop for HeldSlot {
    fn drop(&mut self) {
        self.held.set(self.held.get() - 1);
    }
}

impl RecorderHost for FakeRecorder {
    type Transport = Pipes;

    fn launch(&mut self) -> Result<(Pipes, ProcId), Errno> {
        if let Some(errno) = self.refuse {
            return Err(errno);
        }
        self.launched += 1;
        Ok((self.pipes.clone(), RECORDER))
    }

    fn adopt(&mut self, launch: WiredLaunch) -> Result<(Pipes, ProcId), Errno> {
        if let Some(errno) = self.refuse {
            return Err(errno);
        }
        self.adopted.push(launch);
        Ok((self.pipes.clone(), launch.instance))
    }

    fn grant_slot(
        &mut self,
        recorder: ProcId,
        len: usize,
    ) -> Result<(u64, Box<dyn SaverMemory>), Errno> {
        let refused = self
            .refuse_grant
            .filter(|(at, _)| *at == self.granted.len());
        if let Some((_, errno)) = refused {
            return Err(errno);
        }
        let handle = 11 + self.granted.len() as u64;
        self.granted.push((recorder, handle));
        self.held.set(self.held.get() + 1);
        let slot = HeldSlot {
            bytes: vec![0; len],
            held: Rc::clone(&self.held),
        };
        Ok((handle, Box::new(slot)))
    }
}

fn plan() -> RecordPlan {
    RecordPlan {
        target: CaptureTarget::Area,
        area: AREA,
        window: None,
        sound: RecordSound::Desktop,
        pointer: false,
        clicks: false,
        duration_ns: None,
        folder: KeptFolder::Videos,
    }
}

/// The recording's file, granted under `handle`.
fn file(handle: u64) -> RecordingFile {
    RecordingFile {
        folder: KeptFolder::Videos,
        scratch: String::from(SCRATCH),
        handle,
    }
}

/// Have the recorder say `message`.
fn say(pipes: &Pipes, message: &RecorderMessage) {
    let mut out = [0u8; RECORDER_MESSAGE_MAX];
    let len = message.encode(&mut out).expect("encodes");
    let mut wire = pipes.0.borrow_mut();
    wire.said
        .extend(u32::try_from(len).expect("small").to_le_bytes());
    wire.said.extend(&out[..len]);
}

/// Every message the session sent, each checked to decode.
fn heard(pipes: &Pipes) -> Vec<SessionMessage> {
    let wire = pipes.0.borrow();
    let mut at = 0;
    let mut heard = Vec::new();
    while at + 4 <= wire.sent.len() {
        let len = u32::from_le_bytes(wire.sent[at..at + 4].try_into().expect("four")) as usize;
        heard.push(SessionMessage::decode(&wire.sent[at + 4..at + 4 + len]).expect("a message"));
        at += 4 + len;
    }
    heard
}

fn frames(pipes: &Pipes) -> Vec<(u8, Vec<DamageRect>)> {
    heard(pipes)
        .into_iter()
        .filter_map(|message| match message {
            SessionMessage::Frame { slot, damage, .. } => Some((slot, damage.rects().to_vec())),
            _ => None,
        })
        .collect()
}

/// One turn of the loop at `now_ns`: what the recorder wrote is read, the
/// recording served, and what it queued written.
fn turn(
    recording: &mut Rec,
    host: &mut FakeRecorder,
    comp: &mut Compositor,
    now_ns: u64,
) -> Option<RecordStep> {
    recording.on_readable();
    let step = recording.serve(host, comp, now_ns);
    recording.on_writable();
    step
}

/// A recording started at nought and its recorder greeted at one: what it
/// asks for is its file, granted to the recorder the kernel attests.
fn greeted(
    host: &mut FakeRecorder,
    comp: &mut Compositor,
    records: &Records,
    plan: RecordPlan,
) -> Rec {
    let mut recording = Recording::start(
        host,
        plan,
        (records.clone(), Screened::default()),
        HOME,
        comp,
        0,
    )
    .expect("starts");
    say(&host.pipes, &RecorderMessage::Hello);
    assert_eq!(
        turn(&mut recording, host, comp, 1),
        Some(RecordStep::WantOutput {
            recorder: RECORDER,
            folder: KeptFolder::Videos
        })
    );
    recording
}

/// A recording greeted, given its file and its slots, its first frame sent at
/// three nanoseconds.
fn running(
    host: &mut FakeRecorder,
    comp: &mut Compositor,
    records: &Records,
    plan: RecordPlan,
) -> Rec {
    let mut recording = greeted(host, comp, records, plan);
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), comp, 2),
        None
    );
    recording.on_writable();
    say(&host.pipes, &RecorderMessage::Ready);
    assert_eq!(turn(&mut recording, host, comp, 3), None);
    recording
}

/// The step a recording that made its file ends with.
fn ended(lost: Option<Lost>, reason: Option<StopReason>, kept: bool) -> RecordStep {
    RecordStep::Ended(RecordEnd {
        reason,
        lost,
        scratch: Some(String::from(SCRATCH)),
        kept,
        folder: KeptFolder::Videos,
    })
}

/// The step a recording that never made its file ends with.
fn unopened(lost: Option<Lost>, reason: Option<StopReason>) -> RecordStep {
    RecordStep::Ended(RecordEnd {
        reason,
        lost,
        scratch: None,
        kept: false,
        folder: KeptFolder::Videos,
    })
}

#[test]
fn a_recording_runs_in_the_protocols_one_order() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let _recording = running(&mut host, &mut comp, &records, plan());
    let heard = heard(&host.pipes);
    assert!(
        matches!(heard[0], SessionMessage::Start(start)
            if start.output == 9
                && start.slots == [11, 12]
                && start.sound == RecordSound::Desktop
                && (start.slot.width(), start.slot.height()) == (64, 48)),
        "{heard:?}"
    );
    assert_eq!(
        frames(&host.pipes),
        [(
            0,
            vec![DamageRect {
                x: 0,
                y: 0,
                width_px: 64,
                height_px: 48
            }]
        )],
        "the first frame is the whole area"
    );
    assert_eq!(host.granted, [(RECORDER, 11), (RECORDER, 12)]);
    assert_eq!(records.of(RECORDING_STARTED).len(), 1);
}

#[test]
fn frames_come_only_when_something_changed_and_at_most_thirty_a_second() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 4), None);
    assert_eq!(
        recording.park_deadline_ns(4, u64::MAX),
        u64::MAX,
        "nothing changed, so nothing is armed"
    );
    opaque_window(&mut comp, Point::new(8, 8), 16, 16);
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 5), None);
    assert_eq!(
        frames(&host.pipes).len(),
        1,
        "too soon after the last frame"
    );
    assert_eq!(
        recording.park_deadline_ns(5, u64::MAX),
        3 + RECORDER_FRAME_SPACING_NS - 5
    );
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            3 + RECORDER_FRAME_SPACING_NS
        ),
        None
    );
    assert_eq!(
        frames(&host.pipes)[1],
        (
            0,
            vec![DamageRect {
                x: 0,
                y: 0,
                width_px: 32,
                height_px: 32
            }]
        )
    );
}

#[test]
fn while_both_slots_are_busy_damage_merges_into_the_next_frame() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    let mut now = 3 + RECORDER_FRAME_SPACING_NS;
    opaque_window(&mut comp, Point::new(2, 2), 8, 8);
    assert_eq!(turn(&mut recording, &mut host, &mut comp, now), None);
    assert_eq!(frames(&host.pipes).len(), 2, "the second slot");
    now += 3 * RECORDER_FRAME_SPACING_NS;
    opaque_window(&mut comp, Point::new(30, 20), 8, 8);
    assert_eq!(turn(&mut recording, &mut host, &mut comp, now), None);
    assert_eq!(frames(&host.pipes).len(), 2, "both slots are busy");
    assert_eq!(
        recording.park_deadline_ns(now, u64::MAX),
        u64::MAX,
        "a release wakes it"
    );
    opaque_window(&mut comp, Point::new(40, 30), 4, 4);
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    assert_eq!(turn(&mut recording, &mut host, &mut comp, now), None);
    let (slot, damage) = frames(&host.pipes).pop().expect("a third frame");
    assert_eq!(slot, 0);
    // Each change is covered by the frame's rectangles together: one may
    // straddle the tiles they are cut along.
    for (x0, y0, w, h) in [(30u32, 20u32, 8u32, 8u32), (40, 30, 4, 4)] {
        let covered = (y0..y0 + h).all(|y| {
            (x0..x0 + w).all(|x| {
                damage.iter().any(|named| {
                    (named.x..named.x + named.width_px).contains(&x)
                        && (named.y..named.y + named.height_px).contains(&y)
                })
            })
        });
        assert!(covered, "{:?} merged into {damage:?}", (x0, y0, w, h));
    }
    let missed = recording.slot_bytes(0).expect("slot 0");
    let at = (4 * 64 + 4) * 4;
    assert_eq!(
        missed[at..at + 4],
        [0, 120, 255, 255],
        "the slot was brought up to date with what it missed while busy"
    );
}

#[test]
fn a_frame_the_pipe_has_no_room_for_waits_rather_than_queueing() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    let mut now = 3;
    for step in 0..8 {
        say(&host.pipes, &RecorderMessage::Released { slot: 0 });
        say(&host.pipes, &RecorderMessage::Released { slot: 1 });
        opaque_window(&mut comp, Point::new(step * 6, 40), 4, 4);
        now += RECORDER_FRAME_SPACING_NS;
        recording.on_readable();
        assert_eq!(recording.serve(&mut host, &mut comp, now), None);
        if step > 0 {
            assert_eq!(
                recording.park_deadline_ns(now, u64::MAX),
                u64::MAX,
                "a frame waiting on room in the pipe arms nothing: the room wakes it"
            );
        }
    }
    recording.on_writable();
    let accepted = frames(&host.pipes).len();
    assert!(accepted < 9, "{accepted} frames: the queue is bounded");
    now += RECORDER_FRAME_SPACING_NS;
    assert_eq!(turn(&mut recording, &mut host, &mut comp, now), None);
    let (_, damage) = frames(&host.pipes).pop().expect("the held frame");
    assert!(
        damage
            .iter()
            .any(|named| named.x <= 42 && named.x + named.width_px >= 46),
        "the change held back goes with the next frame: {damage:?}"
    );
}

#[test]
fn a_recorder_that_dies_ends_the_recording_and_keeps_what_it_wrote() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    host.pipes.0.borrow_mut().closed = true;
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 10),
        Some(ended(Some(Lost::RecorderFailed), None, true))
    );
    assert!(recording.ended());
    let logged = records.of(RECORDING_ENDED);
    assert!(logged[0]
        .iter()
        .any(|(key, value)| key == "lost" && value == "recorder-failed"));
}

#[test]
fn a_recorder_that_says_no_message_is_ended() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    host.pipes.garble();
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 10),
        Some(ended(Some(Lost::RecorderFailed), None, true))
    );
}

#[test]
fn a_full_disk_finishes_the_file_and_says_so() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    say(
        &host.pipes,
        &RecorderMessage::Finished {
            bytes: 4096,
            ending: RecordEnding::DiskFull,
        },
    );
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 10),
        Some(ended(Some(Lost::DiskFull), None, true))
    );
    let (title, body) = crate::capture::CaptureNotice::Lost(Lost::DiskFull).text();
    assert_eq!(title, "Screen recording ended");
    assert!(body.contains("disk is full"), "{body}");
}

#[test]
fn locking_stops_the_frames_and_finishes_the_file() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    assert_eq!(recording.stop(StopReason::Screened, &mut comp, 10), None);
    recording.on_writable();
    assert_eq!(heard(&host.pipes).last(), Some(&SessionMessage::Stop));
    opaque_window(&mut comp, Point::new(8, 8), 16, 16);
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            10 + 2 * RECORDER_FRAME_SPACING_NS
        ),
        None
    );
    assert_eq!(
        frames(&host.pipes).len(),
        1,
        "no frame is taken once stopped"
    );
    say(
        &host.pipes,
        &RecorderMessage::Finished {
            bytes: 4096,
            ending: RecordEnding::Stopped,
        },
    );
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            11 + 2 * RECORDER_FRAME_SPACING_NS
        ),
        Some(ended(None, Some(StopReason::Screened), true))
    );
}

/// An output made for an earlier recording's recorder that lands late is not
/// this recording's: it starts nothing, its never-written file is answered
/// back to be settled, and the recording waits for its own.
#[test]
fn an_output_made_for_another_recorder_is_not_this_recordings() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    let earlier = ProcId::from_raw([8; PROC_ID_LEN]);
    assert_eq!(
        take_output(Some(&mut recording), (earlier, Ok(file(5))), &mut comp, 2),
        Some(RecordStep::Unused(file(5)))
    );
    recording.on_writable();
    assert!(
        heard(&host.pipes).is_empty(),
        "nothing started on another's file"
    );
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), &mut comp, 3),
        None
    );
    recording.on_writable();
    assert!(
        matches!(heard(&host.pipes).as_slice(), [SessionMessage::Start(start)] if start.output == 9),
        "started on its own"
    );
}

/// A file landing once its recording has gone was never written, so it is
/// answered back to be settled rather than left behind.
#[test]
fn a_file_landing_after_its_recording_is_answered_back() {
    let mut comp = compositor();
    assert_eq!(
        take_output::<Pipes, Records>(None, (RECORDER, Ok(file(5))), &mut comp, 2),
        Some(RecordStep::Unused(file(5)))
    );
    assert_eq!(
        take_output::<Pipes, Records>(None, (RECORDER, Err(Unkept::Busy)), &mut comp, 2),
        None
    );
}

#[test]
fn a_recording_refused_its_sound_keeps_nothing_and_says_why() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), &mut comp, 2),
        None
    );
    say(
        &host.pipes,
        &RecorderMessage::Unstarted(Errno::PermissionDenied),
    );
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 3),
        Some(ended(
            Some(Lost::Refused(Errno::PermissionDenied)),
            None,
            false
        ))
    );
    assert_eq!(
        alloc::format!("{}", Lost::Refused(Errno::PermissionDenied)),
        "This account may not record sound"
    );
    assert!(records.of(RECORDING_STARTED).is_empty());
}

#[test]
fn a_recorder_that_never_answers_is_given_up_on() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = Recording::start(
        &mut host,
        plan(),
        (records.clone(), Screened::default()),
        HOME,
        &mut comp,
        0,
    )
    .expect("starts");
    assert_eq!(
        recording.park_deadline_ns(0, u64::MAX),
        RECORDER_START_DEADLINE_NS
    );
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            RECORDER_START_DEADLINE_NS
        ),
        Some(unopened(Some(Lost::Unanswered), None))
    );
}

#[test]
fn a_recorder_that_cannot_start_is_no_recording() {
    let mut comp = compositor();
    let mut host = FakeRecorder {
        refuse: Some(Errno::OutOfMemory),
        ..FakeRecorder::default()
    };
    let started = Recording::start(
        &mut host,
        plan(),
        (Records::default(), Screened::default()),
        HOME,
        &mut comp,
        0,
    );
    assert!(matches!(started, Err(Lost::Unstarted(Errno::OutOfMemory))));
}

/// With no home to keep a recording in, that is said before any recorder is
/// started.
#[test]
fn with_no_home_no_recorder_is_started() {
    let (mut comp, mut host) = (compositor(), FakeRecorder::default());
    let started = Recording::start(
        &mut host,
        plan(),
        (Records::default(), Screened::default()),
        None,
        &mut comp,
        0,
    );
    assert!(matches!(started, Err(Lost::NoOutput(Unkept::NoHome))));
    assert_eq!(host.launched, 0);
}

#[test]
fn the_duration_stops_the_recording_when_it_runs_out() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let timed = RecordPlan {
        duration_ns: Some(1_000_000_000),
        ..plan()
    };
    let mut recording = running(&mut host, &mut comp, &records, timed);
    assert!(recording.park_deadline_ns(4, u64::MAX) <= 1_000_000_000);
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 1_000_000_003),
        None
    );
    assert_eq!(heard(&host.pipes).last(), Some(&SessionMessage::Stop));
}

#[test]
fn the_authorisation_sheet_is_blanked_in_every_frame_once_it_shows() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let sheet = opaque_window(&mut comp, Point::new(0, 0), 64, 48);
    let mut recording = running(&mut host, &mut comp, &records, plan());
    let at = (10 * 64 + 10) * 4;
    assert_eq!(
        recording.slot_bytes(0).expect("slot 0")[at..at + 4],
        [0, 120, 255, 255]
    );
    assert_eq!(
        recording.screen(
            Screened {
                lock: None,
                sheet: Some(sheet),
            },
            false,
            &mut comp,
            3,
        ),
        None
    );
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            3 + RECORDER_FRAME_SPACING_NS
        ),
        None
    );
    assert_eq!(frames(&host.pipes).len(), 2);
    assert_eq!(
        recording.slot_bytes(1).expect("slot 1")[at..at + 4],
        [0, 0, 0, 255],
        "the sheet is shown black"
    );
}

/// A sheet the tap cannot be re-armed to blank stops the recording, and what
/// that asks is answered: one not yet started ends at once.
#[test]
fn a_sheet_that_cannot_be_blanked_ends_a_recording_not_yet_started() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let sheet = opaque_window(&mut comp, Point::new(0, 0), 8, 8);
    let mut recording = Recording::start(
        &mut host,
        plan(),
        (records.clone(), Screened::default()),
        HOME,
        &mut comp,
        0,
    )
    .expect("starts");
    assert!(comp.set_desktop_size(32, 24), "the area leaves the desktop");
    let screened = Screened {
        lock: None,
        sheet: Some(sheet),
    };
    assert_eq!(
        recording.screen(screened, false, &mut comp, 5),
        Some(unopened(None, Some(StopReason::Screened)))
    );
    assert!(recording.ended());
}

/// A recording stopped for a sheet it cannot blank is given its whole finish
/// deadline from then, never one long past.
#[test]
fn a_sheet_that_cannot_be_blanked_finishes_the_recording_in_its_time() {
    const LATE: u64 = 60_000_000_000;
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let sheet = opaque_window(&mut comp, Point::new(0, 0), 8, 8);
    let mut recording = running(&mut host, &mut comp, &records, plan());
    assert!(comp.set_desktop_size(32, 24));
    let screened = Screened {
        lock: None,
        sheet: Some(sheet),
    };
    assert_eq!(recording.screen(screened, false, &mut comp, LATE), None);
    recording.on_writable();
    assert_eq!(heard(&host.pipes).last(), Some(&SessionMessage::Stop));
    assert_eq!(
        recording.park_deadline_ns(LATE, u64::MAX),
        RECORDER_FINISH_DEADLINE_NS
    );
}

/// A due frame whose picture cannot be composed is tried again a frame later,
/// never on every turn of the loop.
#[test]
fn a_frame_that_cannot_be_composed_waits_a_frame() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    // More windows than a composed rectangle lists without the heap, so
    // composing asks it for room.
    for _ in 0..20 {
        opaque_window(&mut comp, Point::new(0, 0), 64, 48);
    }
    let mut recording = running(&mut host, &mut comp, &records, plan());
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    opaque_window(&mut comp, Point::new(0, 0), 64, 48);
    let due = 3 + RECORDER_FRAME_SPACING_NS;
    recording.on_readable();
    let step =
        tairix_fuzzseed::meter::refusing_above(64, || recording.serve(&mut host, &mut comp, due));
    assert_eq!(step, None);
    assert_eq!(frames(&host.pipes).len(), 1, "nothing was framed");
    assert_eq!(
        recording.park_deadline_ns(due, u64::MAX),
        RECORDER_FRAME_SPACING_NS,
        "tried again a frame later"
    );
    let later = due + RECORDER_FRAME_SPACING_NS;
    assert_eq!(turn(&mut recording, &mut host, &mut comp, later), None);
    assert_eq!(frames(&host.pipes).len(), 2, "framed once the heap gives");
}

/// Changes scattered too widely for a frame's rectangles are covered by the
/// tiles around them, never by the box around them all.
#[test]
fn scattered_changes_are_named_without_the_box_around_them() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let wide = RecordPlan {
        area: Rect::new(0, 0, 512, 384),
        ..plan()
    };
    let mut recording = running(&mut host, &mut comp, &records, wide);
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    for k in 0..50 {
        let (column, row) = (k % 10 * 32, k / 10 * 32);
        opaque_window(&mut comp, Point::new(column + 4, row + 4), 2, 2);
        opaque_window(&mut comp, Point::new(column + 196, row + 228), 2, 2);
    }
    let due = 3 + RECORDER_FRAME_SPACING_NS;
    assert_eq!(turn(&mut recording, &mut host, &mut comp, due), None);
    let (_, damage) = frames(&host.pipes).pop().expect("a second frame");
    let covered: u64 = damage
        .iter()
        .map(|rect| u64::from(rect.width_px) * u64::from(rect.height_px))
        .sum();
    assert!(damage.len() > 1 && covered < 512 * 384, "{damage:?}");
}

/// While the screensaver is up, however many scattered changes come, keeping
/// them owed costs no memory.
#[test]
fn changes_owed_while_paused_cost_no_memory() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let wide = RecordPlan {
        area: Rect::new(0, 0, 512, 384),
        ..plan()
    };
    let mut recording = running(&mut host, &mut comp, &records, wide);
    assert_eq!(
        recording.screen(Screened::default(), true, &mut comp, 4),
        None
    );
    let mut allocated = 0;
    for k in 0..200 {
        opaque_window(
            &mut comp,
            Point::new(k % 20 * 25 + 3, k / 20 * 37 + 3),
            2,
            2,
        );
        let (step, metering) =
            tairix_fuzzseed::meter::metered(|| recording.serve(&mut host, &mut comp, 5));
        assert_eq!(step, None);
        allocated += metering.allocations;
    }
    assert_eq!(allocated, 0);
}

/// A recorder whose program never loaded says why, and nothing is said to
/// have been saved.
#[test]
fn a_recorder_that_never_loaded_says_why() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = Recording::start(
        &mut host,
        plan(),
        (records.clone(), Screened::default()),
        HOME,
        &mut comp,
        0,
    )
    .expect("starts");
    recording.exited(LOAD_NOT_FOUND);
    host.pipes.0.borrow_mut().closed = true;
    let reason = tairix_abi::load_failure_reason(LOAD_NOT_FOUND).expect("a reason");
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 1),
        Some(unopened(Some(Lost::Unloaded(reason)), None))
    );
    assert_eq!(
        alloc::format!("{}", Lost::Unloaded(reason)),
        alloc::format!("The recorder could not be started: {reason}")
    );
}

/// A recorder that fails before it records anything says nothing was kept,
/// never that it was saved as far as it got.
#[test]
fn a_recorder_that_fails_before_it_records_says_nothing_was_kept() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), &mut comp, 2),
        None
    );
    host.pipes.0.borrow_mut().closed = true;
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 3),
        None,
        "its exit is waited for"
    );
    assert_eq!(recording.park_deadline_ns(3, u64::MAX), super::EXIT_WAIT_NS);
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            3 + super::EXIT_WAIT_NS
        ),
        Some(ended(Some(Lost::Unrecorded), None, false))
    );
    assert_eq!(
        recording.park_deadline_ns(3 + super::EXIT_WAIT_NS, u64::MAX),
        u64::MAX,
        "nothing more is waited for"
    );
    let (_, body) = CaptureNotice::Lost(Lost::Unrecorded).text();
    assert!(!body.contains("as far as it got"), "{body}");
}

/// A recorder whose pipes close before its exit is reaped is waited for, so
/// one whose program never loaded is still told by why.
#[test]
fn a_recorder_gone_before_its_reap_is_told_by_its_exit() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = Recording::start(
        &mut host,
        plan(),
        (records.clone(), Screened::default()),
        HOME,
        &mut comp,
        0,
    )
    .expect("starts");
    host.pipes.0.borrow_mut().closed = true;
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 1), None);
    recording.exited(LOAD_NOT_FOUND);
    assert_eq!(
        recording.park_deadline_ns(1, u64::MAX),
        0,
        "served as soon as it is reaped"
    );
    let reason = tairix_abi::load_failure_reason(LOAD_NOT_FOUND).expect("a reason");
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 2),
        Some(unopened(Some(Lost::Unloaded(reason)), None))
    );
}

/// A recorder's `Ready` crossing the session's `Stop` is let go, its
/// `Finished` ends the recording cleanly, and the slots are released.
#[test]
fn a_ready_crossing_a_stop_is_let_go() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), &mut comp, 2),
        None
    );
    assert_eq!(recording.stop(StopReason::Asked, &mut comp, 3), None);
    say(&host.pipes, &RecorderMessage::Ready);
    say(
        &host.pipes,
        &RecorderMessage::Finished {
            bytes: 0,
            ending: RecordEnding::Stopped,
        },
    );
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 4),
        Some(ended(None, Some(StopReason::Asked), false))
    );
    assert_eq!(host.held.get(), 0, "its slots are released");
}

/// A refusal crossing the session's `Stop` is said, not taken for a fault.
#[test]
fn a_refusal_crossing_a_stop_is_said() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    assert_eq!(
        take_output(Some(&mut recording), (RECORDER, Ok(file(9))), &mut comp, 2),
        None
    );
    assert_eq!(recording.stop(StopReason::Asked, &mut comp, 3), None);
    say(
        &host.pipes,
        &RecorderMessage::Unstarted(Errno::PermissionDenied),
    );
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 4),
        Some(ended(
            Some(Lost::Refused(Errno::PermissionDenied)),
            Some(StopReason::Asked),
            false
        ))
    );
}

/// A slot the session cannot make or grant ends the attempt at the
/// recorder's `Hello`: no file is asked for, no `Start` is sent, and every
/// slot made before the refusal is released.
#[test]
fn a_refused_grant_starts_nothing() {
    for refused in 0..RECORDER_SLOTS {
        let (mut comp, records) = (compositor(), Records::default());
        let mut host = FakeRecorder {
            refuse_grant: Some((refused, Errno::LimitExceeded)),
            ..FakeRecorder::default()
        };
        let mut recording = Recording::start(
            &mut host,
            plan(),
            (records.clone(), Screened::default()),
            HOME,
            &mut comp,
            0,
        )
        .expect("starts");
        say(&host.pipes, &RecorderMessage::Hello);
        assert_eq!(
            turn(&mut recording, &mut host, &mut comp, 1),
            Some(unopened(Some(Lost::Unstarted(Errno::LimitExceeded)), None)),
            "grant {refused} refused"
        );
        assert_eq!(host.granted.len(), refused, "granted up to the refusal");
        assert_eq!(host.held.get(), 0, "every slot made is released");
        assert!(heard(&host.pipes).is_empty(), "nothing is started");
        assert!(records.of(RECORDING_STARTED).is_empty());
    }
}

/// One way a running recording ends, played by the test.
type Ending = fn(&mut Rec, &mut FakeRecorder, &mut Compositor) -> Option<RecordStep>;

/// The slots are the session's memory: held while the recording runs, and
/// released however it ends.
#[test]
fn the_slots_are_released_however_the_recording_ends() {
    let endings: [Ending; 4] = [
        |recording, host, comp| {
            assert_eq!(recording.stop(StopReason::Asked, comp, 10), None);
            let finished = RecorderMessage::Finished {
                bytes: 1,
                ending: RecordEnding::Stopped,
            };
            say(&host.pipes, &finished);
            turn(recording, host, comp, 11)
        },
        |recording, host, comp| {
            let full = RecorderMessage::Finished {
                bytes: 1,
                ending: RecordEnding::DiskFull,
            };
            say(&host.pipes, &full);
            turn(recording, host, comp, 10)
        },
        |recording, host, comp| {
            host.pipes.0.borrow_mut().closed = true;
            turn(recording, host, comp, 10)
        },
        |recording, host, comp| {
            host.pipes.garble();
            turn(recording, host, comp, 10)
        },
    ];
    for ending in endings {
        let (mut comp, records) = (compositor(), Records::default());
        let mut host = FakeRecorder::default();
        let mut recording = running(&mut host, &mut comp, &records, plan());
        assert_eq!(host.held.get(), RECORDER_SLOTS, "held while it runs");
        let step = ending(&mut recording, &mut host, &mut comp);
        assert!(matches!(step, Some(RecordStep::Ended(_))), "{step:?}");
        assert_eq!(host.held.get(), 0, "released at its end");
    }
}

/// A file the worker never makes ends the recording within its start's time.
#[test]
fn a_file_never_made_ends_the_recording_in_time() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = greeted(&mut host, &mut comp, &records, plan());
    assert_eq!(
        recording.park_deadline_ns(1, u64::MAX),
        RECORDER_START_DEADLINE_NS - 1
    );
    assert_eq!(
        turn(
            &mut recording,
            &mut host,
            &mut comp,
            RECORDER_START_DEADLINE_NS
        ),
        Some(unopened(Some(Lost::OutputLate), None))
    );
    assert_eq!(host.held.get(), 0, "its slots are released");
}

/// What changed since the last frame goes with a stop the user asks for,
/// however soon after that frame.
#[test]
fn the_last_changes_go_with_a_stop() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 4), None);
    opaque_window(&mut comp, Point::new(8, 8), 16, 16);
    assert_eq!(recording.stop(StopReason::Asked, &mut comp, 5), None);
    recording.on_writable();
    let heard = heard(&host.pipes);
    assert!(
        matches!(
            heard.as_slice(),
            [.., SessionMessage::Frame { .. }, SessionMessage::Stop]
        ),
        "{heard:?}"
    );
    assert_eq!(frames(&host.pipes).len(), 2);
}

/// A recording asked to stop is no longer live while its file is finished,
/// so the capture chords take captures again.
#[test]
fn a_recording_finishing_its_file_is_no_longer_live() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut recording = running(&mut host, &mut comp, &records, plan());
    assert!(recording.is_live());
    assert_eq!(recording.stop(StopReason::Asked, &mut comp, 5), None);
    assert!(!recording.is_live() && !recording.ended());
}

/// A window recording shows that window alone however others open over it,
/// and owes only what they changed rather than its whole area.
#[test]
fn a_window_recording_owes_only_what_another_window_changed() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let window = opaque_window(&mut comp, Point::new(0, 0), 64, 48);
    let alone = RecordPlan {
        target: CaptureTarget::Window,
        window: Some(window),
        ..plan()
    };
    let mut recording = running(&mut host, &mut comp, &records, alone);
    say(&host.pipes, &RecorderMessage::Released { slot: 0 });
    let over = Surface::filled(8, 8, Color::rgb(250, 0, 0).premultiply()).expect("a surface");
    comp.add_window(Point::new(40, 30), over);
    let due = 3 + RECORDER_FRAME_SPACING_NS;
    assert_eq!(turn(&mut recording, &mut host, &mut comp, due), None);
    let (slot, damage) = frames(&host.pipes).pop().expect("a second frame");
    assert_eq!(
        damage,
        [DamageRect {
            x: 32,
            y: 16,
            width_px: 16,
            height_px: 32
        }],
        "only the tiles the other window touched"
    );
    let at = (34 * 64 + 44) * 4;
    assert_eq!(
        recording.slot_bytes(usize::from(slot)).expect("the slot")[at..at + 4],
        [0, 120, 255, 255],
        "the window over it is left out"
    );
}

/// The recorder an administrator allowed: the broker's child, attested by
/// the broker, never the session's own [`RECORDER`].
const ALLOWED_RECORDER: ProcId = ProcId::from_raw([9; PROC_ID_LEN]);

/// The broker as a recording asks it: each question kept, each answer left
/// to the test.
#[derive(Default)]
struct Broker {
    asked: Vec<(Ticket, String)>,
    tickets: Tickets,
}

impl Elevator for Broker {
    fn authorisers(&mut self, program: &str) -> Ticket {
        let ticket = self.tickets.issue();
        self.asked.push((ticket, String::from(program)));
        ticket
    }

    fn offer(&mut self, _: &str, _: &str, _: &Request) -> Ticket {
        self.tickets.issue()
    }
}

/// What the broker answers an administrator's leave with.
fn wired() -> WiredLaunch {
    WiredLaunch {
        instance: ALLOWED_RECORDER,
        input: NonZeroU64::new(3).expect("non-zero"),
        output: NonZeroU64::new(4).expect("non-zero"),
    }
}

/// A recording with sound, its question whether the account may record it
/// asked of `broker`.
fn authorising(broker: &mut Broker, records: &Records) -> Rec {
    Recording::authorising(plan(), (records.clone(), Screened::default()), HOME, broker)
        .expect("prepared")
}

/// The broker's answer to the recording's question: `own` when the account
/// may record the sound itself.
fn listing(broker: &Broker, own: bool) -> Answer {
    let tile = AccountTile::new("Ada Lovelace", "ada");
    let (own, others) = if own {
        (Some(tile), Vec::new())
    } else {
        (None, vec![AccountTile::new("Grace Hopper", "grace")])
    };
    Answer::Listing(broker.asked[0].0, Some(Listing { own, others }))
}

#[test]
fn a_recording_with_sound_asks_before_it_starts_anything() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    assert_eq!(broker.asked.len(), 1);
    assert_eq!(broker.asked[0].1, RECORDER_RUN_PATH);
    assert!(recording.is_authorising());
    assert!(
        !recording.is_live(),
        "nothing is recorded yet, so no indicator"
    );
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 1), None);
    assert_eq!(
        recording.park_deadline_ns(1, u64::MAX),
        u64::MAX,
        "no deadline runs"
    );
    assert_eq!(host.launched, 0);
    let mut changed = Region::new();
    assert!(!comp.take_capture_changes(&mut changed), "no tap is armed");
}

#[test]
fn an_account_that_may_record_sound_starts_its_own_recorder() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    let own = listing(&broker, true);
    assert_eq!(recording.answered(&own, &mut host, &mut comp, 0), None);
    assert_eq!(host.launched, 1);
    assert!(host.adopted.is_empty());
    assert!(recording.is_live());
    say(&host.pipes, &RecorderMessage::Hello);
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 1),
        Some(RecordStep::WantOutput {
            recorder: RECORDER,
            folder: KeptFolder::Videos
        })
    );
}

#[test]
fn an_administrators_recorder_runs_the_protocol_over_the_pipes_it_was_handed() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    let others = listing(&broker, false);
    let request = administrator_request(RecordSound::Desktop);
    assert_eq!(
        recording.answered(&others, &mut host, &mut comp, 0),
        Some(RecordStep::Authorise(request.clone()))
    );
    assert_eq!(request.program, RECORDER_RUN_PATH);
    assert_eq!(request.elevation, Elevation::Wired);
    assert_eq!(request.asker, Some(Asker::Recording));
    assert_eq!(
        request.purpose,
        "wants to record the sound this computer plays."
    );
    assert_eq!(
        host.launched, 0,
        "nothing starts before an administrator answers"
    );
    let allowed = Allowed::Administrator(wired());
    assert_eq!(recording.authorised(&mut host, allowed, &mut comp, 1), None);
    assert_eq!(host.adopted, [wired()]);
    assert_eq!(host.launched, 0, "the broker started it, not the session");
    say(&host.pipes, &RecorderMessage::Hello);
    assert_eq!(
        turn(&mut recording, &mut host, &mut comp, 2),
        Some(RecordStep::WantOutput {
            recorder: ALLOWED_RECORDER,
            folder: KeptFolder::Videos
        }),
        "its file goes to the process the broker attested"
    );
    let made = (ALLOWED_RECORDER, Ok(file(9)));
    assert_eq!(take_output(Some(&mut recording), made, &mut comp, 3), None);
    recording.on_writable();
    assert!(
        matches!(heard(&host.pipes)[0], SessionMessage::Start(start) if start.slots == [11, 12]),
        "its start carries the slots' grants"
    );
    assert_eq!(
        host.granted,
        [(ALLOWED_RECORDER, 11), (ALLOWED_RECORDER, 12)],
        "its slots go to the process the broker attested"
    );
    say(&host.pipes, &RecorderMessage::Ready);
    assert_eq!(turn(&mut recording, &mut host, &mut comp, 4), None);
    assert_eq!(records.of(RECORDING_STARTED).len(), 1);
}

#[test]
fn a_recording_nobody_allowed_starts_nothing_and_says_so() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    let others = listing(&broker, false);
    let _ = recording.answered(&others, &mut host, &mut comp, 0);
    assert_eq!(
        recording.authorised(&mut host, Allowed::Declined, &mut comp, 1),
        Some(unopened(Some(Lost::Declined), None))
    );
    assert!(recording.ended());
    assert_eq!(host.launched, 0);
    assert!(host.adopted.is_empty());
    assert_eq!(
        CaptureNotice::Lost(Lost::Declined).text(),
        (
            "Screen recording ended",
            String::from("Sound needs an administrator's permission, so nothing was recorded.")
        )
    );
    assert!(records.of(RECORDING_STARTED).is_empty());
}

#[test]
fn a_lock_while_authorising_ends_it_with_nothing_started() {
    let (mut comp, records) = (compositor(), Records::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    assert_eq!(
        recording.stop(StopReason::Screened, &mut comp, 0),
        Some(unopened(None, Some(StopReason::Screened)))
    );
}

#[test]
fn an_answer_the_recording_is_not_waiting_on_is_released() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    // An administrator's recorder reaching a recording still asking whether
    // its own account may: taken up only to be closed.
    let allowed = Allowed::Administrator(wired());
    assert_eq!(recording.authorised(&mut host, allowed, &mut comp, 0), None);
    assert_eq!(host.adopted, [wired()]);
    assert!(recording.is_authorising(), "and not taken for its own");
    // Nor does a broker's answer to a question it did not ask start anything.
    let stray = Answer::Listing(broker.tickets.issue(), None);
    assert_eq!(recording.answered(&stray, &mut host, &mut comp, 0), None);
    assert_eq!(host.launched, 0);
    assert!(recording.is_authorising());
}

#[test]
fn an_administrators_recorder_that_cannot_be_taken_up_is_said() {
    let (mut comp, records, mut host) = (compositor(), Records::default(), FakeRecorder::default());
    let mut broker = Broker::default();
    let mut recording = authorising(&mut broker, &records);
    let others = listing(&broker, false);
    let _ = recording.answered(&others, &mut host, &mut comp, 0);
    host.refuse = Some(Errno::LimitExceeded);
    let allowed = Allowed::Administrator(wired());
    assert_eq!(
        recording.authorised(&mut host, allowed, &mut comp, 1),
        Some(unopened(Some(Lost::Unstarted(Errno::LimitExceeded)), None))
    );
}

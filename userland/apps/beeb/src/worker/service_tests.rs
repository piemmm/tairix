use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::ProtoError;

use super::*;
use crate::media::disc::ImageFormat;
use crate::media::tape::Tone;
use crate::worker::protocol::{Configuration, Reply, Request, Slot, MAX_SLICE, SLOTS};

/// A tape holding nothing.
const BLANK_TAPE: &[u8] = b"UEF File!\0\x0A\x00";

struct Collected(Vec<Vec<u8>>);

impl FrameOut for Collected {
    fn frame(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.0.push(payload.to_vec());
        Ok(())
    }
}

struct Rig {
    service: MachineService<HeapShare>,
    replies: Collected,
}

impl Rig {
    fn new() -> Self {
        let share = HeapShare::new(SHARE_BYTES).expect("a share");
        Self {
            service: MachineService::new(share).expect("a share large enough"),
            replies: Collected(Vec::new()),
        }
    }

    /// The reply to `request`, as its bytes.
    fn ask(&mut self, request: &Request<'_>) -> Vec<u8> {
        assert_eq!(
            self.service.handle(&request.encode(), &mut self.replies),
            SessionStep::Continue
        );
        assert_eq!(self.replies.0.len(), 1, "one reply a request");
        self.replies.0.remove(0)
    }

    /// The machine built with BASIC and a slot of sideways RAM.
    fn configure(&mut self) {
        let os = crate::rom::mos::build().expect("the MOS").bytes;
        let basic = crate::rom::basic::build().expect("BASIC").bytes;
        let mut slots = [Slot::Empty; SLOTS];
        slots[15] = Slot::Rom(&basic);
        slots[0] = Slot::Ram;
        let reply = self.ask(&Request::Configure(Box::new(Configuration {
            os: &os,
            slots,
            links: 0,
        })));
        assert_eq!(Reply::decode(&reply), Ok(Reply::Done));
    }

    fn configured() -> Self {
        let mut rig = Self::new();
        rig.configure();
        rig
    }

    /// The deck's tape replaced by a blank one recorded on.
    fn record_tape(&mut self) {
        let mut tape = Tape::decode(BLANK_TAPE).expect("a tape");
        let mut recorded = Tape::default();
        recorded.push(Tone::High, 1000).expect("a tone");
        tape.record_over(0, &recorded).expect("recorded");
        self.service
            .machine
            .as_mut()
            .expect("a machine")
            .insert_tape(tape);
    }

    /// Drive 0's disc replaced by one with a track written.
    fn write_disc(&mut self) {
        let image = crate::scenario::discs::named("testdisc").expect("a disc");
        let mut disc = Disc::decode(&image, ImageFormat::Ssd, false).expect("a disc");
        disc.track_mut(0, 1).expect("a track");
        self.service
            .machine
            .as_mut()
            .expect("a machine")
            .insert_disc(PhysicalDrive::Zero, disc);
    }
}

fn media(reply: &[u8]) -> Media<'_> {
    match Reply::decode(reply) {
        Ok(Reply::Media(media)) => media,
        other => panic!("not media: {other:?}"),
    }
}

fn insert(image: &[u8]) -> Request<'_> {
    Request::InsertDisc {
        drive: PhysicalDrive::Zero,
        format: ImageFormat::Ssd,
        write_protected: false,
        image,
    }
}

fn run(cycles: u32, sound: bool) -> Request<'static> {
    Request::Run {
        cycles,
        sound,
        inputs: Vec::new(),
    }
}

#[test]
fn a_share_too_small_for_the_picture_is_refused() {
    assert!(MachineService::new(HeapShare::new(SHARE_BYTES - 1).expect("a share")).is_none());
}

#[test]
fn nothing_runs_until_a_machine_is_configured() {
    let mut rig = Rig::new();
    for request in [
        run(10, false),
        Request::PowerOn,
        Request::TakeSnapshot,
        Request::CollectDiscs([true; 2]),
        Request::EjectTape,
    ] {
        let reply = rig.ask(&request);
        assert_eq!(
            Reply::decode(&reply),
            Ok(Reply::Refused(Refusal::Unconfigured)),
            "{request:?}"
        );
    }
    assert_eq!(
        Reply::decode(&rig.ask(&Request::Configure(Box::new(Configuration {
            os: &[1, 2, 3],
            slots: [Slot::Empty; SLOTS],
            links: 0
        })))),
        Ok(Reply::Refused(Refusal::Rom))
    );
}

#[test]
fn a_slice_reports_its_picture_sound_and_lights() {
    let mut rig = Rig::configured();
    let reply = rig.ask(&run(MAX_SLICE, true));
    let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
        panic!("a slice ran")
    };
    assert_eq!(ran.cycles, MAX_SLICE);
    assert!(
        (0..CANVAS_HEIGHT).all(|row| ran.row_changed(row)),
        "every row, the first time"
    );
    let samples = ran.sound.len() / SAMPLE_BYTES;
    assert!(
        (47_500..=48_002).contains(&samples),
        "a second's sound: {samples}"
    );
    assert!(ran.sound.iter().any(|&byte| byte != 0), "the power-on bell");
    assert!(ran.lights.caps_lock, "CAPS LOCK lit as the MOS starts");
    let reply = rig.ask(&run(40_000, false));
    let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
        panic!("a slice ran")
    };
    assert!(ran.sound.is_empty(), "no sound asked for");
    assert!(
        !(0..CANVAS_HEIGHT).all(|row| ran.row_changed(row)),
        "only what changed after"
    );
}

#[test]
fn media_insert_and_refuse_what_does_not_decode() {
    let mut rig = Rig::configured();
    let image = crate::scenario::discs::named("testdisc").expect("a disc");
    assert_eq!(
        media(&rig.ask(&insert(&image))),
        Media::default(),
        "the drive was empty"
    );
    assert_eq!(
        Reply::decode(&rig.ask(&insert(&[]))),
        Ok(Reply::Refused(Refusal::Disc))
    );
    assert_eq!(
        media(&rig.ask(&Request::CollectDiscs([true; 2]))),
        Media::default(),
        "nothing written"
    );
    assert_eq!(
        Reply::decode(&rig.ask(&Request::InsertTape(&[0; 4]))),
        Ok(Reply::Refused(Refusal::Tape))
    );
    assert_eq!(
        media(&rig.ask(&Request::EjectTape)),
        Media::default(),
        "no tape in"
    );
    assert_eq!(
        media(&rig.ask(&Request::InsertTape(BLANK_TAPE))),
        Media::default()
    );
    assert_eq!(Reply::decode(&rig.ask(&Request::PowerOn)), Ok(Reply::Done));
}

/// The regression: every tape taken out was handed over, recorded on or
/// not, so the owner could not tell which held what no file does.
#[test]
fn a_tape_is_handed_over_only_when_it_was_recorded_on() {
    let mut rig = Rig::configured();
    assert_eq!(
        media(&rig.ask(&Request::InsertTape(BLANK_TAPE))),
        Media::default()
    );
    assert_eq!(
        media(&rig.ask(&Request::InsertTape(BLANK_TAPE))).tape,
        None,
        "the unrecorded tape an insert replaces"
    );
    assert_eq!(
        media(&rig.ask(&Request::EjectTape)).tape,
        None,
        "an unrecorded tape ejected"
    );
    rig.record_tape();
    assert_eq!(Reply::decode(&rig.ask(&Request::PowerOn)), Ok(Reply::Done));
    let reply = rig.ask(&Request::EjectTape);
    let tape = media(&reply)
        .tape
        .expect("recorded on, and kept across power-on");
    assert!(Tape::decode(tape).is_ok_and(|tape| !tape.is_empty()));
    rig.record_tape();
    assert!(
        media(&rig.ask(&Request::InsertTape(BLANK_TAPE)))
            .tape
            .is_some(),
        "the recorded tape an insert replaces"
    );
}

#[test]
fn a_slice_reports_the_record_key() {
    let mut rig = Rig::configured();
    let loaded = |rig: &mut Rig| {
        let reply = rig.ask(&run(10, false));
        let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
            panic!("a slice ran")
        };
        ran.loaded
    };
    assert!(!loaded(&mut rig).recording);
    assert_eq!(
        Reply::decode(&rig.ask(&Request::RecordTape(true))),
        Ok(Reply::Done)
    );
    assert!(loaded(&mut rig).recording);
}

#[test]
fn a_written_disc_is_handed_over_whatever_takes_it_out() {
    let mut rig = Rig::configured();
    rig.write_disc();
    assert_eq!(Reply::decode(&rig.ask(&Request::PowerOn)), Ok(Reply::Done));
    rig.configure();
    assert_eq!(
        media(&rig.ask(&Request::CollectDiscs([false, true]))),
        Media::default(),
        "drive 0 not asked for"
    );
    let reply = rig.ask(&Request::CollectDiscs([true, false]));
    let collected = media(&reply);
    assert_eq!(
        collected.discs.len(),
        1,
        "kept across a rebuild, and still unsaved when its drive was not asked for"
    );
    assert_eq!(collected.discs[0].drive, PhysicalDrive::Zero);
    assert_eq!(collected.discs[0].format, ImageFormat::Ssd);
    assert_eq!(collected.tape, None);
    let image = collected.discs[0].image.to_vec();
    assert_eq!(
        media(&rig.ask(&Request::CollectDiscs([true; 2]))),
        Media::default(),
        "collected once"
    );

    rig.write_disc();
    assert_eq!(
        media(&rig.ask(&insert(&image))).discs.len(),
        1,
        "the disc an insert replaces"
    );
    rig.write_disc();
    let reply = rig.ask(&Request::EjectDisc(PhysicalDrive::Zero));
    assert_eq!(media(&reply).discs.len(), 1, "the disc ejected");
    assert_eq!(
        media(&rig.ask(&Request::EjectDisc(PhysicalDrive::Zero))),
        Media::default(),
        "the drive is empty"
    );
}

#[test]
fn a_restore_hands_over_the_media_it_replaces() {
    let mut rig = Rig::configured();
    let reply = rig.ask(&Request::TakeSnapshot);
    let Ok(Reply::Snapshot(snapshot)) = Reply::decode(&reply) else {
        panic!("a snapshot")
    };
    let snapshot = snapshot.to_vec();
    rig.write_disc();
    rig.record_tape();
    let reply = rig.ask(&Request::RestoreSnapshot(&snapshot));
    let taken = media(&reply);
    assert_eq!(taken.discs.len(), 1, "the written disc");
    assert!(taken.tape.is_some(), "the recorded tape");
    let machine = rig.service.machine.as_mut().expect("a machine");
    assert!(
        machine.disc(PhysicalDrive::Zero).is_none() && machine.tape().is_none(),
        "the snapshot's empty drives"
    );
}

#[test]
fn a_snapshot_restores_and_a_torn_one_is_refused() {
    let mut rig = Rig::configured();
    rig.ask(&run(100_000, false));
    let reply = rig.ask(&Request::TakeSnapshot);
    let Ok(Reply::Snapshot(snapshot)) = Reply::decode(&reply) else {
        panic!("a snapshot")
    };
    let snapshot = snapshot.to_vec();
    assert_eq!(
        media(&rig.ask(&Request::RestoreSnapshot(&snapshot))),
        Media::default(),
        "nothing to hand over"
    );
    assert_eq!(
        Reply::decode(&rig.ask(&Request::RestoreSnapshot(&snapshot[..8]))),
        Ok(Reply::Refused(Refusal::Snapshot))
    );
    let reply = rig.ask(&run(10, false));
    let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
        panic!("a slice ran")
    };
    assert!(
        (0..CANVAS_HEIGHT).all(|row| ran.row_changed(row)),
        "every row after a restore"
    );
}

#[test]
fn a_request_that_does_not_decode_is_refused_and_the_session_goes_on() {
    let mut rig = Rig::new();
    assert_eq!(
        rig.service.handle(&[0xEE, 1], &mut rig.replies),
        SessionStep::Continue
    );
    assert_eq!(
        Reply::decode(&rig.replies.0.remove(0)),
        Ok(Reply::Refused(Refusal::Malformed))
    );
}

#[test]
fn slices_tile_the_timeline_whatever_each_overruns_by() {
    let mut rig = Rig::configured();
    for _ in 0..1000 {
        rig.ask(&run(7, false));
    }
    let ran = rig.service.machine.as_ref().expect("a machine").cycles();
    assert!((7000..7016).contains(&ran), "{ran} cycles for 7000 asked");
}

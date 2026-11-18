use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::ProtoError;

use super::*;
use crate::machine::electron::keyboard::Key as ElectronKey;
use crate::machine::Model;
use crate::media::tape::{Tape, Tone};
use crate::worker::protocol::DiscFormat;
use crate::worker::protocol::{Configuration, Reply, Request, Slot, MAX_SLICE, SLOTS};
use tairix_dfs::image::Layout;

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
        let os = crate::rom::mos::model_b().expect("the MOS").bytes;
        let basic = crate::rom::basic::build().expect("BASIC").bytes;
        let mut slots = [Slot::Empty; SLOTS];
        slots[15] = Slot::Rom(&basic);
        slots[0] = Slot::Ram;
        let reply = self.ask(&Request::Configure(Box::new(Configuration::ModelB {
            os: &os,
            slots: Box::new(slots),
            links: 0,
        })));
        assert_eq!(Reply::decode(&reply), Ok(Reply::Done));
    }

    fn configured() -> Self {
        let mut rig = Self::new();
        rig.configure();
        rig
    }

    /// An Electron, with the Plus 1 and its ROM if `plus1`; the reply.
    fn configure_electron(&mut self, plus1: bool, cartridge: Option<&[u8]>) -> Vec<u8> {
        let os = crate::rom::mos::electron().expect("the OS").bytes;
        let basic = crate::rom::basic::build().expect("BASIC").bytes;
        let rom = crate::rom::plus1::build().expect("the Plus 1 ROM").bytes;
        let plus1 = plus1.then_some(Plus1 {
            rom: &rom,
            cartridges: [[cartridge, None], [None, None]],
        });
        self.ask(&Request::Configure(Box::new(Configuration::Electron {
            os: &os,
            basic: &basic,
            plus1,
        })))
    }

    fn electron(plus1: bool) -> Self {
        let mut rig = Self::new();
        let reply = rig.configure_electron(plus1, None);
        assert_eq!(Reply::decode(&reply), Ok(Reply::Done));
        rig
    }

    /// The Model B the service runs.
    fn model_b(&mut self) -> &mut bbc::Machine {
        match self.service.machine.as_mut() {
            Some(Machine::ModelB(machine)) => machine,
            _ => panic!("a Model B"),
        }
    }

    /// The slice `inputs` run in, as the service answers it.
    fn run_with(&mut self, inputs: Vec<Input>) -> Vec<u8> {
        self.ask(&Request::Run {
            cycles: 40_000,
            sound: false,
            inputs,
        })
    }

    /// The deck's tape replaced by a blank one recorded on.
    fn record_tape(&mut self) {
        let mut tape = Tape::decode(BLANK_TAPE).expect("a tape");
        let mut recorded = Tape::default();
        recorded.push(Tone::High, 1000).expect("a tone");
        tape.record_over(0, &recorded).expect("recorded");
        self.model_b().insert_tape(tape);
    }

    /// Drive 0's disc replaced by one with a track written.
    fn write_disc(&mut self) {
        let image = crate::scenario::discs::named("testdisc").expect("a disc");
        let mut disc = Disc::decode(&image, Layout::Single, false).expect("a disc");
        disc.track_mut(0, 1).expect("a track");
        self.model_b().insert_disc(PhysicalDrive::Zero, disc);
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
        format: DiscFormat::Acorn(Layout::Single),
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
        Reply::decode(
            &rig.ask(&Request::Configure(Box::new(Configuration::ModelB {
                os: &[1, 2, 3],
                slots: Box::new([Slot::Empty; SLOTS]),
                links: 0
            })))
        ),
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
    assert_eq!(collected.discs[0].format, DiscFormat::Acorn(Layout::Single));
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
    let machine = rig.model_b();
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

#[test]
fn an_electron_runs_with_its_own_lamps_and_no_drives() {
    let mut rig = Rig::electron(true);
    let reply = rig.ask(&run(MAX_SLICE, true));
    let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
        panic!("a slice ran")
    };
    assert!(ran.lights.caps_lock, "CAPS LOCK lit as the OS starts");
    assert!(!ran.lights.shift_lock && ran.drives == [false; 2] && ran.loaded.discs == [false; 2]);
    assert!(ran.sound.iter().any(|&byte| byte != 0), "the power-on bell");
    assert_eq!(
        rig.service.machine.as_ref().map(Machine::model),
        Some(Model::Electron)
    );
}

#[test]
fn an_electron_refuses_what_only_a_model_b_has() {
    let mut rig = Rig::electron(true);
    let image = crate::scenario::discs::named("testdisc").expect("a disc");
    let model_b_key = Input {
        at: 0,
        change: Change::Key(
            ModelKey::ModelB(crate::machine::bbc::keyboard::Key::A),
            true,
        ),
    };
    let cycles = rig.service.machine.as_ref().expect("a machine").cycles();
    for reply in [
        rig.ask(&insert(&image)),
        rig.ask(&Request::EjectDisc(PhysicalDrive::One)),
        rig.ask(&Request::CollectDiscs([true; 2])),
        rig.run_with(alloc::vec![model_b_key]),
        rig.ask(&Request::Configure(Box::new(Configuration::ModelB {
            os: &[0; 0x4000],
            slots: Box::new([Slot::Empty; SLOTS]),
            links: 0,
        }))),
    ] {
        assert_eq!(Reply::decode(&reply), Ok(Reply::Refused(Refusal::Model)));
    }
    assert_eq!(
        rig.service.machine.as_ref().expect("a machine").cycles(),
        cycles,
        "nothing ran"
    );
    let electron_key = Input {
        at: 0,
        change: Change::Key(ModelKey::Electron(ElectronKey::A), true),
    };
    assert!(matches!(
        Reply::decode(&rig.run_with(alloc::vec![electron_key])),
        Ok(Reply::Ran(_))
    ));
}

#[test]
fn the_analogue_port_is_the_plus_1s_on_an_electron() {
    let analogue = || {
        alloc::vec![
            Input {
                at: 0,
                change: Change::Analogue(1, 0x8000),
            },
            Input {
                at: 1,
                change: Change::Fire(0b01),
            },
        ]
    };
    let mut bare = Rig::electron(false);
    assert_eq!(
        Reply::decode(&bare.run_with(analogue())),
        Ok(Reply::Refused(Refusal::Model))
    );
    let mut fitted = Rig::electron(true);
    assert!(matches!(
        Reply::decode(&fitted.run_with(analogue())),
        Ok(Reply::Ran(_))
    ));
    let mut model_b = Rig::configured();
    assert!(matches!(
        Reply::decode(&model_b.run_with(analogue())),
        Ok(Reply::Ran(_))
    ));
}

#[test]
fn the_plus_1s_cartridges_are_fitted_with_it() {
    let mut rig = Rig::new();
    let cartridge = [0xEA; 0x2000];
    assert_eq!(
        Reply::decode(&rig.configure_electron(true, Some(&cartridge))),
        Ok(Reply::Done)
    );
    assert_eq!(
        Reply::decode(&rig.configure_electron(true, Some(&[1, 2, 3]))),
        Ok(Reply::Refused(Refusal::Rom)),
        "a cartridge image no socket takes"
    );
}

#[test]
fn an_electron_snapshot_restores_and_a_model_bs_is_refused() {
    let mut electron = Rig::electron(true);
    electron.ask(&run(100_000, false));
    let reply = electron.ask(&Request::TakeSnapshot);
    let Ok(Reply::Snapshot(snapshot)) = Reply::decode(&reply) else {
        panic!("a snapshot")
    };
    let snapshot = snapshot.to_vec();
    assert_eq!(
        media(&electron.ask(&Request::RestoreSnapshot(&snapshot))),
        Media::default()
    );
    let mut model_b = Rig::configured();
    let reply = model_b.ask(&Request::TakeSnapshot);
    let Ok(Reply::Snapshot(model_b_snapshot)) = Reply::decode(&reply) else {
        panic!("a snapshot")
    };
    assert_eq!(
        Reply::decode(&electron.ask(&Request::RestoreSnapshot(model_b_snapshot))),
        Ok(Reply::Refused(Refusal::Snapshot))
    );
}

/// The regression: an instruction running past a slice's end, as one waiting
/// out the Electron's display window in mode 0 does, put its sound in that
/// slice, more than its cycles make, and the owner condemned the worker.
#[test]
fn a_slices_sound_is_its_cycles_whatever_the_last_instruction_overruns_by() {
    let mut rig = Rig::electron(false);
    let mut client = crate::worker::MachineClient::new();
    let configure = Request::Configure(Box::new(Configuration::Electron {
        os: &[],
        basic: &[],
        plus1: None,
    }));
    client.ask(&configure).expect("idle");
    tairix_sandbox::supervise::Requests::sent(&mut client);
    client.answer(&Reply::Done.encode()).expect("configured");
    // Past the resampler's start-up delay, the samples are the cycles'.
    let (warming, slices, cycles) = (100, 300, 997);
    let mut samples = 0;
    for slice in 0..warming + slices {
        let request = run(cycles, true);
        client.ask(&request).expect("idle");
        tairix_sandbox::supervise::Requests::sent(&mut client);
        let reply = rig.ask(&request);
        let Ok(Reply::Ran(ran)) = client.answer(&reply) else {
            panic!("a slice believed")
        };
        if slice >= warming {
            samples += ran.sound.len() / SAMPLE_BYTES;
        }
    }
    let exact = u64::from(cycles) * slices * u64::from(SAMPLE_RATE) / crate::machine::CLOCK;
    let delivered = u64::try_from(samples).expect("a count");
    assert!(
        delivered.abs_diff(exact) <= 2,
        "{delivered} samples for {exact}"
    );
}

mod commodore {
    use super::*;
    use crate::firmware::{
        C1541_DOS_IMAGE, C64_BASIC_IMAGE, C64_CHARACTER_IMAGE, C64_KERNAL_IMAGE,
    };
    use crate::machine::c64::keyboard::Keycap;
    use crate::machine::c64::{self, joystick, sid, Standard};
    use crate::media::c1541::d64;
    use crate::worker::protocol::{Change, DeckKey, ModelKey, Refusal};

    const PAL_SECOND: u32 = 985_248;

    fn configure(rig: &mut Rig, standard: Standard, drives: [bool; 2]) -> Vec<u8> {
        rig.ask(&Request::Configure(Box::new(Configuration::C64 {
            standard,
            sid: sid::Model::Mos8580,
            kernal: C64_KERNAL_IMAGE,
            basic: C64_BASIC_IMAGE,
            character: C64_CHARACTER_IMAGE,
            drives: drives.map(|fitted| fitted.then_some(C1541_DOS_IMAGE)),
        })))
    }

    fn rig(drives: [bool; 2]) -> Rig {
        let mut rig = Rig::new();
        let reply = configure(&mut rig, Standard::Pal, drives);
        assert_eq!(Reply::decode(&reply), Ok(Reply::Done));
        rig
    }

    fn machine(rig: &mut Rig) -> &mut c64::Machine {
        match rig.service.machine.as_mut() {
            Some(Machine::C64(machine)) => machine,
            _ => panic!("a Commodore 64"),
        }
    }

    fn refused(reply: &[u8]) -> Refusal {
        match Reply::decode(reply) {
            Ok(Reply::Refused(refusal)) => refusal,
            other => panic!("not refused: {other:?}"),
        }
    }

    fn ran(reply: &[u8]) -> Ran<'_> {
        match Reply::decode(reply) {
            Ok(Reply::Ran(ran)) => ran,
            other => panic!("not a slice: {other:?}"),
        }
    }

    fn disk(format: DiscFormat, image: &[u8]) -> Request<'_> {
        Request::InsertDisc {
            drive: PhysicalDrive::Zero,
            format,
            write_protected: false,
            image,
        }
    }

    #[test]
    fn a_commodore_runs_a_second_of_its_own_cycles_and_sounds_at_the_output_rate() {
        let mut rig = rig([false; 2]);
        let slice = ran(&rig.ask(&run(PAL_SECOND, true))).sound.len() / SAMPLE_BYTES;
        assert!(
            (47_500..=48_002).contains(&slice),
            "a second's sound, the resampler's start aside: {slice}"
        );
        assert_eq!(
            refused(&rig.ask(&run(PAL_SECOND + 1, false))),
            Refusal::Model,
            "past a second of its own"
        );
        let reply = rig.ask(&run(1000, false));
        let lit = ran(&reply);
        assert_eq!(
            (lit.lights.caps_lock, lit.lights.shift_lock),
            (false, false)
        );
    }

    /// Past the resampler's start, an NTSC machine's slices carry exactly
    /// the sound their cycles make, at a ratio no small denominator holds.
    #[test]
    fn an_ntsc_commodore_s_samples_are_its_cycles() {
        let mut rig = Rig::new();
        configure(&mut rig, Standard::Ntsc, [false; 2]);
        let (warming, slices, cycles) = (100, 300, 997);
        let mut samples = 0;
        for slice in 0..warming + slices {
            let length = ran(&rig.ask(&run(cycles, true))).sound.len() / SAMPLE_BYTES;
            if slice >= warming {
                samples += length;
            }
        }
        let exact =
            u64::from(cycles) * slices * u64::from(SAMPLE_RATE) / Standard::Ntsc.timing().clock();
        let delivered = u64::try_from(samples).expect("a count");
        assert!(
            delivered.abs_diff(exact) <= 2,
            "{delivered} samples for {exact}"
        );
    }

    #[test]
    fn a_commodore_s_drive_takes_its_disks_and_hands_back_one_written() {
        let mut rig = rig([true, false]);
        let image = d64::formatted(b"GAMES", *b"GA");
        assert!(media(&rig.ask(&disk(DiscFormat::D64, &image)))
            .discs
            .is_empty());
        assert_eq!(
            refused(&rig.ask(&disk(DiscFormat::Acorn(Layout::Single), &image))),
            Refusal::Model,
            "an Acorn's"
        );
        assert_eq!(
            refused(&rig.ask(&disk(DiscFormat::D64, &image[1..]))),
            Refusal::Disc
        );
        let unfitted = Request::InsertDisc {
            drive: PhysicalDrive::One,
            format: DiscFormat::D64,
            write_protected: false,
            image: &image,
        };
        assert_eq!(refused(&rig.ask(&unfitted)), Refusal::Model, "device 9");
        let track = machine(&mut rig)
            .drive_mut(c64::drive::Unit::Eight)
            .and_then(c64::drive::Drive::disk_mut)
            .and_then(|disk| disk.track_to_write(0, 3))
            .expect("track 1");
        let bit = track.bit(8 * 100);
        track.set_bit(8 * 100, !bit);
        let reply = rig.ask(&Request::CollectDiscs([true, false]));
        let collected = media(&reply);
        assert_eq!(collected.discs.len(), 1);
        assert_eq!(
            collected.discs[0].format,
            DiscFormat::G64,
            "no sector dump holds it now"
        );
        assert!(media(&rig.ask(&Request::CollectDiscs([true, false])))
            .discs
            .is_empty());
        let reply = rig.ask(&run(1000, false));
        assert_eq!(ran(&reply).loaded.discs, [true, false]);
    }

    #[test]
    fn a_configure_leaving_a_disk_without_its_drive_is_refused() {
        let mut rig = rig([true, false]);
        let image = d64::formatted(b"KEEP", *b"KP");
        rig.ask(&disk(DiscFormat::D64, &image));
        let reply = configure(&mut rig, Standard::Pal, [false; 2]);
        assert_eq!(refused(&reply), Refusal::Model);
        assert!(
            machine(&mut rig)
                .drive(c64::drive::Unit::Eight)
                .and_then(c64::drive::Drive::disk)
                .is_some(),
            "the disk still in its drive"
        );
        let reply = configure(&mut rig, Standard::Ntsc, [true, true]);
        assert_eq!(Reply::decode(&reply), Ok(Reply::Done), "a drive kept");
        assert_eq!(
            ran(&rig.ask(&run(1_022_727, false))).cycles,
            1_022_727,
            "an NTSC second now"
        );
    }

    #[test]
    fn a_commodore_takes_its_tapes_deck_keys_cartridges_and_programs() {
        let mut rig = rig([false; 2]);
        let mut tap = Vec::from(&b"C64-TAPE-RAW"[..]);
        tap.extend([1, 0, 0, 0, 1, 0, 0, 0, 0x30]);
        assert!(media(&rig.ask(&Request::InsertTape(&tap))).tape.is_none());
        assert_eq!(
            refused(&rig.ask(&Request::InsertTape(BLANK_TAPE))),
            Refusal::Tape
        );
        assert_eq!(
            refused(&rig.ask(&Request::RecordTape(true))),
            Refusal::Model
        );
        assert_eq!(
            Reply::decode(&rig.ask(&Request::Deck(DeckKey::Play))),
            Ok(Reply::Done)
        );
        assert!(ran(&rig.ask(&run(1000, false))).loaded.playing);
        rig.ask(&Request::Deck(DeckKey::Stop));
        assert!(!ran(&rig.ask(&run(1000, false))).loaded.playing);
        let crt = crate::media::crt::tests::image(0, 0, 1, &[(0, 0x8000, &[0x55; 0x2000])]);
        assert_eq!(
            Reply::decode(&rig.ask(&Request::InsertCartridge(&crt))),
            Ok(Reply::Done)
        );
        assert!(ran(&rig.ask(&run(1000, false))).loaded.cartridge);
        assert_eq!(
            refused(&rig.ask(&Request::InsertCartridge(&crt[1..]))),
            Refusal::Cartridge
        );
        rig.ask(&Request::RemoveCartridge);
        assert!(!ran(&rig.ask(&run(1000, false))).loaded.cartridge);
        assert_eq!(
            Reply::decode(&rig.ask(&Request::LoadProgram(&[0x00, 0xC0, 0xAB]))),
            Ok(Reply::Done)
        );
        assert_eq!(machine(&mut rig).ram(0xC000), 0xAB);
        assert_eq!(
            refused(&rig.ask(&Request::LoadProgram(&[0x00]))),
            Refusal::Program
        );
    }

    #[test]
    fn a_commodore_takes_its_own_keys_and_joysticks_and_no_break() {
        let mut rig = rig([false; 2]);
        let restore = Change::Key(ModelKey::C64(Keycap::Restore), true);
        let stick = Change::Joystick(1, joystick::FIRE);
        let at = |change| Input { at: 0, change };
        assert!(matches!(
            Reply::decode(&rig.run_with(alloc::vec![at(restore), at(stick)])),
            Ok(Reply::Ran(_))
        ));
        assert_eq!(
            refused(&rig.run_with(alloc::vec![at(Change::Break(true))])),
            Refusal::Model
        );
        assert_eq!(
            refused(&rig.run_with(alloc::vec![at(Change::Fire(1))])),
            Refusal::Model
        );
        let mut model_b = Rig::configured();
        assert_eq!(
            refused(&model_b.run_with(alloc::vec![at(stick)])),
            Refusal::Model,
            "a Model B has no joystick port"
        );
    }

    #[test]
    fn a_restore_hands_back_the_written_disk_and_the_recorded_tape() {
        let mut rig = rig([true, false]);
        let image = d64::formatted(b"GAMES", *b"GA");
        rig.ask(&disk(DiscFormat::D64, &image));
        let mut tap = Vec::from(&b"C64-TAPE-RAW"[..]);
        tap.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        rig.ask(&Request::InsertTape(&tap));
        let reply = rig.ask(&Request::TakeSnapshot);
        let snapshot = match Reply::decode(&reply) {
            Ok(Reply::Snapshot(snapshot)) => snapshot.to_vec(),
            other => panic!("no snapshot: {other:?}"),
        };
        let machine = machine(&mut rig);
        let track = machine
            .drive_mut(c64::drive::Unit::Eight)
            .and_then(c64::drive::Drive::disk_mut)
            .and_then(|disk| disk.track_to_write(0, 3))
            .expect("track 1");
        let last = track.len() - 1;
        track.set_bit(last, false);
        let mut recorded = crate::media::tap::decode(&tap).expect("a TAP");
        recorded.record(0, &[0x180, 0x180]);
        machine.insert_tape(recorded);
        let reply = rig.ask(&Request::RestoreSnapshot(&snapshot));
        let taken = media(&reply);
        assert_eq!(taken.discs.len(), 1, "the written disk");
        assert_eq!(taken.discs[0].format, DiscFormat::D64);
        let tape =
            crate::media::tap::decode(taken.tape.expect("the recorded tape")).expect("a TAP image");
        assert_eq!(tape.pulses(), [0x180, 0x180]);
    }
}

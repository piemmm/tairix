use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

const ZERO: PhysicalDrive = PhysicalDrive::Zero;
const ONE: PhysicalDrive = PhysicalDrive::One;
const SSD: DiscFormat = DiscFormat::Acorn(ImageFormat::Ssd);
const DSD: DiscFormat = DiscFormat::Acorn(ImageFormat::Dsd);
const BOTH: [bool; 2] = [true; 2];

fn disc(name: &str, medium: Option<u32>) -> Disc {
    Disc {
        name: String::from(name),
        format: format_of(name),
        medium,
        of_folder: false,
        image: vec![1; 4],
    }
}

fn tape(name: &str, medium: Option<u32>) -> Tape {
    Tape {
        name: String::from(name),
        medium,
        of_folder: false,
        image: vec![2; 4],
    }
}

const fn written(drive: PhysicalDrive, format: DiscFormat, image: &[u8]) -> Written<'_> {
    Written {
        drive,
        format,
        image,
        lost: None,
    }
}

/// A Model B's shelf with `discs` put in and `tape` in the deck.
fn shelf(discs: [Option<Disc>; 2], tape: Option<Tape>) -> Rack {
    shelf_of(Model::ModelB, discs, tape)
}

/// A `model`'s shelf with `discs` put in and `tape` in the deck.
fn shelf_of(model: Model, discs: [Option<Disc>; 2], tape: Option<Tape>) -> Rack {
    let mut shelf = Rack::new(model);
    let mut work = Vec::new();
    for (drive, disc) in PhysicalDrive::ALL.into_iter().zip(discs) {
        if let Some(disc) = disc {
            shelf.handed(
                &Media::default(),
                Handover::InsertDisc(drive, disc),
                &mut work,
            );
        }
    }
    if let Some(tape) = tape {
        shelf.handed(&Media::default(), Handover::InsertTape(tape), &mut work);
    }
    assert!(work.is_empty());
    shelf
}

fn saved(shelf: &mut Rack) -> Option<(String, Holding, Vec<u8>)> {
    let (suggested, holding) = shelf.choose_to_save()?;
    match shelf.chosen() {
        Some(Chosen::Save(bytes)) => Some((suggested, holding, bytes)),
        other => panic!("a save's picker: {other:?}"),
    }
}

#[test]
fn a_file_is_a_disc_or_a_tape_by_its_extension() {
    let model_b = |name| Purpose::of(name, Model::ModelB, BOTH);
    assert_eq!(model_b("Elite.SSD"), Some(Purpose::Disc(ZERO)));
    assert_eq!(model_b("games.dsd"), Some(Purpose::Disc(ZERO)));
    assert_eq!(model_b("Repton.uef"), Some(Purpose::Tape));
    assert_eq!(model_b("side-a.CSW"), Some(Purpose::Tape));
    assert_eq!(model_b("notes.txt"), None);
    assert_eq!(model_b("dsd"), None);
    assert_eq!(model_b("Game.d64"), None, "another model's");
    assert_eq!(
        Purpose::of("Elite.ssd", Model::Electron, [false; 2]),
        None,
        "an Electron has no drive"
    );
    assert_eq!(
        Purpose::of("Elite.uef", Model::Electron, [false; 2]),
        Some(Purpose::Tape)
    );
    assert_eq!(format_of("Games.DSD"), DSD);
    assert_eq!(format_of("Elite.ssd"), SSD);
}

#[test]
fn a_commodore_takes_its_own_disks_tapes_cartridges_and_programs() {
    let c64 = |name, drives| Purpose::of(name, Model::C64, drives);
    assert_eq!(c64("Game.D64", BOTH), Some(Purpose::Disc(ZERO)));
    assert_eq!(c64("Copy.g64", BOTH), Some(Purpose::Disc(ZERO)));
    assert_eq!(
        c64("Game.d64", [false, true]),
        Some(Purpose::Disc(ONE)),
        "the first drive fitted"
    );
    assert_eq!(c64("Game.d64", [false; 2]), None, "no drive");
    assert_eq!(c64("Loader.TAP", [false; 2]), Some(Purpose::Tape));
    assert_eq!(c64("Game.crt", [false; 2]), Some(Purpose::Cartridge));
    assert_eq!(c64("Demo.prg", [false; 2]), Some(Purpose::Program));
    assert_eq!(c64("Tapes.t64", [false; 2]), Some(Purpose::Program));
    assert_eq!(c64("Elite.uef", BOTH), None, "an Acorn's");
    assert_eq!(format_of("Game.d64"), DiscFormat::D64);
    assert_eq!(format_of("Copy.G64"), DiscFormat::G64);
}

/// A 1541 disk whose sectors no longer all decode comes back as a G64,
/// which its D64's file cannot hold: the file is let go and the disk
/// offered under its new ending, collected or taken out.
#[test]
fn a_disk_its_file_cannot_hold_any_longer_is_offered_and_its_file_let_go() {
    let mut shelf = shelf_of(Model::C64, [Some(disc("Game.d64", Some(5))), None], None);
    let image = [6u8; 4];
    let media = Media {
        discs: vec![written(ZERO, DiscFormat::G64, &image)],
        tape: None,
    };
    let mut work = Vec::new();
    shelf.handed(&media, Handover::Collect, &mut work);
    assert_eq!(work, [FileWork::Forget(5)], "never a G64 over a D64");
    assert_eq!(
        saved(&mut shelf),
        Some((
            String::from("Game.g64"),
            Holding::Disc(DiscFormat::G64),
            image.to_vec()
        ))
    );
    assert_eq!(shelf.filed(), [false, false], "unfiled from now on");
    let held = shelf.disc(ZERO).expect("still in");
    assert_eq!(
        (held.name.as_str(), held.format),
        ("Game.g64", DiscFormat::G64)
    );
}

#[test]
fn a_commodore_s_recorded_tape_goes_back_to_a_tap_and_is_offered_as_one_otherwise() {
    let recorded = [3u8; 5];
    let media = Media {
        discs: Vec::new(),
        tape: Some(&recorded),
    };
    let mut tap = shelf_of(Model::C64, [None, None], Some(tape("Mine.tap", Some(1))));
    let mut work = Vec::new();
    tap.handed(&media, Handover::EjectTape, &mut work);
    assert_eq!(
        work,
        [
            FileWork::WriteBack {
                medium: 1,
                bytes: recorded.to_vec()
            },
            FileWork::Forget(1)
        ]
    );
    let mut unnamed = Rack::new(Model::C64);
    unnamed.handed(&media, Handover::Restore, &mut work);
    assert_eq!(
        saved(&mut unnamed),
        Some((
            String::from("Tape.tap"),
            Holding::Tape(Model::C64),
            recorded.to_vec()
        ))
    );
}

#[test]
fn only_filed_discs_are_collected_and_written_back() {
    let mut shelf = shelf(
        [Some(disc("A.ssd", Some(3))), Some(disc("B.ssd", None))],
        None,
    );
    assert_eq!(shelf.filed(), [true, false]);
    let image = [9u8; 8];
    let mut work = Vec::new();
    let media = Media {
        discs: vec![written(ZERO, SSD, &image)],
        tape: None,
    };
    shelf.handed(&media, Handover::Collect, &mut work);
    assert_eq!(
        work,
        [FileWork::WriteBack {
            medium: 3,
            bytes: image.to_vec()
        }]
    );
    assert_eq!(
        shelf.disc(ZERO).map(|disc| &disc.image[..]),
        Some(&image[..])
    );
    assert!(shelf.is_settled(), "nothing offered");
}

/// The regression: Save Disc As offered the record as last collected, so a
/// disc with no file, written since, saved without what was written; the
/// drive is collected first, and the save is of what that brings back.
#[test]
fn a_disc_saved_as_is_what_its_collect_brought_back() {
    let mut shelf = shelf([None, Some(disc("Game.d64", None))], None);
    let image = [7u8; 8];
    let mut work = Vec::new();
    let media = Media {
        discs: vec![Written {
            drive: ONE,
            format: DiscFormat::D64,
            image: &image,
            lost: None,
        }],
        tape: None,
    };
    shelf.handed(&media, Handover::Collect, &mut work);
    assert!(work.is_empty(), "no file to write back to");
    let saving = shelf.disc_saving(ONE).expect("a disc in drive 1");
    assert_eq!(
        (saving.suggested.as_str(), saving.holding, &saving.bytes[..]),
        ("Game.d64", Holding::Disc(DiscFormat::D64), &image[..])
    );
    assert!(
        shelf.disc_saving(ZERO).is_none(),
        "an empty drive saves nothing"
    );
}

/// The regression: a disc the user never named — a snapshot's — was dropped
/// when collected and never offered, and offered as single-sided whatever it
/// was, so a double-sided one reopened as nonsense.
#[test]
fn a_written_disc_with_no_file_is_offered_as_it_is_taken_out_in_its_own_format() {
    let mut shelf = Rack::new(Model::ModelB);
    assert_eq!(shelf.filed(), [false, false], "the snapshot's discs");
    let image = [7u8; 6];
    let mut work = Vec::new();
    let media = Media {
        discs: vec![written(ONE, DSD, &image)],
        tape: None,
    };
    shelf.handed(&media, Handover::EjectDisc(ONE), &mut work);
    assert!(work.is_empty());
    assert_eq!(
        saved(&mut shelf),
        Some((String::from("Disc.dsd"), Holding::Disc(DSD), image.to_vec()))
    );
}

#[test]
fn a_filed_disc_taken_out_is_written_back_before_its_file_is_let_go() {
    let mut shelf = shelf([Some(disc("A.ssd", Some(3))), None], None);
    let image = [5u8; 4];
    let media = Media {
        discs: vec![written(ZERO, SSD, &image)],
        tape: None,
    };
    let mut work = Vec::new();
    shelf.handed(
        &media,
        Handover::InsertDisc(ZERO, disc("B.ssd", Some(4))),
        &mut work,
    );
    assert_eq!(
        work,
        [
            FileWork::WriteBack {
                medium: 3,
                bytes: image.to_vec()
            },
            FileWork::Forget(3)
        ]
    );
    assert_eq!(shelf.filed(), [true, false]);
    assert_eq!(
        shelf.disc(ZERO).map(|disc| disc.name.as_str()),
        Some("B.ssd")
    );
    work.clear();
    shelf.handed(&Media::default(), Handover::EjectDisc(ZERO), &mut work);
    assert_eq!(work, [FileWork::Forget(4)], "unwritten, let go");
    assert_eq!(shelf.disc(ZERO), None);
}

/// The regression: an eject that handed nothing over kept the tape's record,
/// its file held, as though the tape were still in.
#[test]
fn a_tape_taken_out_unrecorded_lets_its_file_go() {
    let mut shelf = shelf([None, None], Some(tape("Game.uef", Some(8))));
    let mut work = Vec::new();
    shelf.handed(&Media::default(), Handover::EjectTape, &mut work);
    assert_eq!(work, [FileWork::Forget(8)]);
    assert_eq!(shelf.tape(), None);
    assert!(shelf.is_settled());
}

#[test]
fn a_recorded_tape_goes_back_to_a_uef_and_is_offered_as_one_otherwise() {
    let recorded = [3u8; 5];
    let media = Media {
        discs: Vec::new(),
        tape: Some(&recorded),
    };
    let mut uef = shelf([None, None], Some(tape("Mine.uef", Some(1))));
    let mut work = Vec::new();
    uef.handed(&media, Handover::EjectTape, &mut work);
    assert_eq!(
        work,
        [
            FileWork::WriteBack {
                medium: 1,
                bytes: recorded.to_vec()
            },
            FileWork::Forget(1)
        ]
    );
    let mut csw = shelf([None, None], Some(tape("Side A.csw", Some(2))));
    work.clear();
    csw.handed(
        &media,
        Handover::InsertTape(tape("Next.uef", None)),
        &mut work,
    );
    assert_eq!(work, [FileWork::Forget(2)], "never UEF over a CSW file");
    assert_eq!(
        saved(&mut csw),
        Some((
            String::from("Side A.uef"),
            Holding::Tape(Model::ModelB),
            recorded.to_vec()
        ))
    );
    assert_eq!(csw.tape().map(|tape| tape.name.as_str()), Some("Next.uef"));
    let mut unnamed = Rack::new(Model::ModelB);
    unnamed.handed(&media, Handover::Restore, &mut work);
    assert_eq!(
        saved(&mut unnamed),
        Some((
            String::from("Tape.uef"),
            Holding::Tape(Model::ModelB),
            recorded.to_vec()
        ))
    );
}

#[test]
fn a_restore_takes_every_record_out_and_lets_every_file_go() {
    let mut shelf = shelf(
        [Some(disc("A.ssd", Some(1))), Some(disc("B.dsd", None))],
        Some(tape("T.uef", Some(2))),
    );
    let image = [4u8; 3];
    let media = Media {
        discs: vec![written(ZERO, SSD, &image)],
        tape: None,
    };
    let mut work = Vec::new();
    shelf.handed(&media, Handover::Restore, &mut work);
    assert_eq!(
        work,
        [
            FileWork::WriteBack {
                medium: 1,
                bytes: image.to_vec()
            },
            FileWork::Forget(2),
            FileWork::Forget(1)
        ]
    );
    assert_eq!(
        (shelf.disc(ZERO), shelf.disc(ONE), shelf.tape()),
        (None, None, None)
    );
}

/// The regression: a machine put away kept its media's files open, one more
/// each time it was suspended or stopped.
#[test]
fn a_machine_put_away_lets_every_file_go() {
    let mut shelf = shelf(
        [Some(disc("A.ssd", Some(1))), Some(disc("B.dsd", None))],
        Some(tape("T.uef", Some(2))),
    );
    let mut work = Vec::new();
    shelf.let_go(&mut work);
    assert_eq!(work, [FileWork::Forget(1), FileWork::Forget(2)]);
    assert_eq!(
        (shelf.disc(ZERO), shelf.disc(ONE), shelf.tape()),
        (None, None, None)
    );
}

/// The regression: a second offer replaced the bytes of the first while its
/// picker was still open, so one of two discs taken out together was lost.
#[test]
fn saves_wait_their_turn_for_the_one_picker() {
    let mut shelf = Rack::new(Model::ModelB);
    let media = Media {
        discs: vec![written(ZERO, SSD, &[1]), written(ONE, DSD, &[2])],
        tape: Some(&[3]),
    };
    let mut work = Vec::new();
    shelf.handed(&media, Handover::Restore, &mut work);
    assert!(shelf.choose_to_open(Purpose::Tape), "no picker open");
    assert_eq!(shelf.choose_to_save(), None, "the open picker first");
    assert!(
        !shelf.choose_to_open(Purpose::Snapshot),
        "one picker at a time"
    );
    assert_eq!(shelf.chosen(), Some(Chosen::Open(Purpose::Tape)));
    let mut offered = Vec::new();
    while let Some((suggested, _, bytes)) = saved(&mut shelf) {
        offered.push((suggested, bytes));
    }
    assert_eq!(
        offered,
        [
            (String::from("Disc.ssd"), vec![1]),
            (String::from("Disc.dsd"), vec![2]),
            (String::from("Tape.uef"), vec![3])
        ]
    );
    assert!(shelf.is_settled());
}

#[test]
fn an_offer_waits_while_a_save_is_being_chosen() {
    let mut shelf = Rack::new(Model::ModelB);
    shelf.offer(Saving {
        suggested: String::from("BBC Micro.snapshot"),
        holding: Holding::Snapshot,
        bytes: vec![1],
    });
    assert_eq!(
        shelf.choose_to_save(),
        Some((String::from("BBC Micro.snapshot"), Holding::Snapshot))
    );
    shelf.offer(Saving {
        suggested: String::from("Disc.ssd"),
        holding: Holding::Disc(SSD),
        bytes: vec![2],
    });
    assert_eq!(shelf.choose_to_save(), None);
    assert!(!shelf.is_settled());
    assert_eq!(shelf.chosen(), Some(Chosen::Save(vec![1])));
    assert_eq!(
        saved(&mut shelf),
        Some((String::from("Disc.ssd"), Holding::Disc(SSD), vec![2]))
    );
}

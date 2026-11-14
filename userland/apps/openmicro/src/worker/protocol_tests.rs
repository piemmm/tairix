use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::machine::{bbc, electron};

fn round_trip(request: &Request<'_>) {
    assert_eq!(
        Request::decode(&request.encode()).as_ref(),
        Ok(request),
        "{request:?}"
    );
}

#[test]
fn every_request_survives_its_wire_form() {
    let rom = [0xA5u8; 0x2000];
    let mut slots = [Slot::Empty; SLOTS];
    slots[15] = Slot::Rom(&rom);
    slots[4] = Slot::Ram;
    let electron = |plus1| {
        Request::Configure(Box::new(Configuration::Electron {
            os: &rom,
            basic: &rom[..0x100],
            plus1,
        }))
    };
    for request in [
        Request::Configure(Box::new(Configuration::ModelB {
            os: &rom,
            slots: Box::new(slots),
            links: 0x87,
        })),
        electron(None),
        electron(Some(Plus1 {
            rom: &rom[..0x10],
            cartridges: [[Some(&rom[..3]), None], [None, Some(&[])]],
        })),
        Request::InsertDisc {
            drive: PhysicalDrive::One,
            format: ImageFormat::Dsd,
            write_protected: true,
            image: &[1, 2, 3],
        },
        Request::EjectDisc(PhysicalDrive::Zero),
        Request::InsertTape(&[9; 40]),
        Request::EjectTape,
        Request::RewindTape,
        Request::RecordTape(true),
        Request::Run {
            cycles: 40_000,
            sound: true,
            inputs: vec![
                Input {
                    at: 0,
                    change: Change::Key(ModelKey::ModelB(bbc::keyboard::Key::A), true),
                },
                Input {
                    at: 0,
                    change: Change::Key(ModelKey::Electron(electron::keyboard::Key::FUNC), false),
                },
                Input {
                    at: 0,
                    change: Change::Break(true),
                },
                Input {
                    at: 10,
                    change: Change::Analogue(3, 0xFFFF),
                },
                Input {
                    at: 39_999,
                    change: Change::Fire(0b10),
                },
            ],
        },
        Request::PowerOn,
        Request::TakeSnapshot,
        Request::RestoreSnapshot(&[7; 100]),
        Request::CollectDiscs([false, true]),
        Request::CollectDiscs([true, true]),
    ] {
        round_trip(&request);
    }
}

#[test]
fn every_reply_survives_its_wire_form() {
    let mut rows = [0; ROW_MAP];
    rows[0] = 0b101;
    rows[ROW_MAP - 1] = 0x80;
    let sound = [0x34u8, 0x12, 0xFF, 0x7F];
    for reply in [
        Reply::Done,
        Reply::Ran(Ran {
            cycles: 2,
            rows,
            sound: &sound,
            lights: Lights {
                caps_lock: true,
                shift_lock: false,
                motor: true,
            },
            drives: [false, true],
            loaded: Loaded {
                discs: [true, false],
                tape: true,
                recording: true,
            },
        }),
        Reply::Snapshot(&[1, 2]),
        Reply::Media(Media {
            discs: vec![
                Written {
                    drive: PhysicalDrive::One,
                    format: ImageFormat::Dsd,
                    image: &[5; 3],
                    lost: Some((1, LAST_TRACK)),
                },
                Written {
                    drive: PhysicalDrive::Zero,
                    format: ImageFormat::Ssd,
                    image: &[],
                    lost: None,
                },
            ],
            tape: Some(&[4; 8]),
        }),
        Reply::Media(Media::default()),
        Reply::Refused(Refusal::Snapshot),
        Reply::Refused(Refusal::Malformed),
        Reply::Refused(Refusal::Model),
    ] {
        assert_eq!(
            Reply::decode(&reply.encode()).as_ref(),
            Ok(&reply),
            "{reply:?}"
        );
    }
}

#[test]
fn a_ran_reply_says_which_rows_changed() {
    let mut rows = [0; ROW_MAP];
    rows[1] = 0b10;
    let ran = Ran {
        cycles: 1,
        rows,
        sound: &[],
        lights: Lights::default(),
        drives: [false; 2],
        loaded: Loaded::default(),
    };
    assert!(ran.row_changed(9));
    assert!(!ran.row_changed(8));
    assert!(!ran.row_changed(CANVAS_HEIGHT + 3));
}

fn refused(bytes: &[u8]) -> bool {
    Request::decode(bytes).is_err()
}

#[test]
fn a_request_out_of_bounds_does_not_decode() {
    let run = |cycles: u32, inputs: &[(u32, u8)]| {
        let mut bytes = vec![8];
        bytes.extend_from_slice(&cycles.to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&u16::try_from(inputs.len()).unwrap().to_le_bytes());
        for &(at, kind) in inputs {
            bytes.extend_from_slice(&at.to_le_bytes());
            bytes.push(kind);
            bytes.push(1);
        }
        bytes
    };
    assert!(refused(&run(0, &[])), "no cycles");
    assert!(refused(&run(MAX_SLICE + 1, &[])), "past a second");
    assert!(refused(&run(10, &[(5, 1), (4, 1)])), "out of order");
    assert!(refused(&run(10, &[(10, 1)])), "at the slice's end");
    assert!(refused(&run(10, &[(0, 9)])), "no such change");
    assert!(!refused(&run(10, &[(0, 1), (9, 1)])));
    let mut many = run(10, &[]);
    many[6..8].copy_from_slice(&u16::try_from(MAX_INPUTS + 1).unwrap().to_le_bytes());
    assert!(refused(&many), "too many changes");
    assert!(refused(&[3, 2]), "drive 2");
    assert!(refused(&[7, 2]), "a flag of 2");
    assert!(!refused(&[12, 3]), "both drives collected");
    assert!(refused(&[12, 4]), "a third drive collected");
    assert!(refused(&[9, 0]), "a byte past the end");
    assert!(refused(&[0]), "no such request");
    assert!(refused(&[]), "nothing");
    let key = |model: u8, number: u8| {
        let mut key = run(10, &[]);
        key[6..8].copy_from_slice(&1u16.to_le_bytes());
        key.extend_from_slice(&0u32.to_le_bytes());
        key.extend_from_slice(&[0, model, number, 1]);
        key
    };
    assert!(!refused(&key(0, 0x10)), "the Model B's Q");
    assert!(refused(&key(0, 0x0F)), "no Model B key numbered &0F");
    assert!(!refused(&key(1, 0)), "the Electron's first key");
    assert!(refused(&key(1, 56)), "past the Electron's keys");
    assert!(refused(&key(2, 0)), "no third model");
}

#[test]
fn a_configure_out_of_bounds_does_not_decode() {
    let image = |bytes: &mut Vec<u8>, len: u32| {
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.resize(bytes.len() + len as usize, 0xEA);
    };
    let electron = |plus1: &[u8]| {
        let mut bytes = vec![1, 1];
        image(&mut bytes, 4);
        image(&mut bytes, 4);
        bytes.extend_from_slice(plus1);
        bytes
    };
    assert!(!refused(&electron(&[0])), "no Plus 1");
    assert!(refused(&electron(&[2])), "a Plus 1 flag of 2");
    let mut fitted = electron(&[1]);
    image(&mut fitted, 4);
    fitted.extend_from_slice(&[0, 0, 0, 0]);
    assert!(!refused(&fitted), "a Plus 1 with no cartridges");
    let last = fitted.len() - 1;
    fitted[last] = 2;
    assert!(refused(&fitted), "a cartridge flag of 2");
    let mut large = vec![1, 1];
    image(&mut large, u32::try_from(MAX_ROM + 1).unwrap());
    image(&mut large, 4);
    large.push(0);
    assert!(refused(&large), "an OS past a ROM's size");
    assert!(refused(&[1, 2]), "no third model");
}

#[test]
fn a_reply_out_of_bounds_does_not_decode() {
    let ran = |rows_last: u8, sound: &[u8], lights: u8| {
        let mut bytes = vec![2];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let mut rows = [0; ROW_MAP];
        rows[ROW_MAP - 1] = rows_last;
        bytes.extend_from_slice(&rows);
        bytes.extend_from_slice(&u32::try_from(sound.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(sound);
        bytes.extend_from_slice(&[lights, 0, 0]);
        bytes
    };
    assert!(Reply::decode(&ran(0xFF, &[0; 4], 7)).is_ok());
    assert!(Reply::decode(&ran(0, &[0; 3], 0)).is_err(), "half a sample");
    assert!(Reply::decode(&ran(0, &[], 8)).is_err(), "a fourth light");
    let mut loaded = ran(0, &[], 0);
    *loaded.last_mut().unwrap() = 15;
    assert!(
        Reply::decode(&loaded).is_ok(),
        "both drives, the tape and RECORD"
    );
    *loaded.last_mut().unwrap() = 16;
    assert!(Reply::decode(&loaded).is_err(), "a bit past RECORD");
    let samples: Vec<u8> = vec![0; (MAX_SAMPLES + 1) * SAMPLE_BYTES];
    assert!(
        Reply::decode(&ran(0, &samples, 0)).is_err(),
        "too much sound"
    );
    assert!(Reply::decode(&[4, 3]).is_err(), "three discs");
    assert!(Reply::decode(&[4, 0, 0]).is_ok(), "nothing taken out");
    assert!(Reply::decode(&[4, 0, 2]).is_err(), "a tape flag of 2");
    assert!(
        Reply::decode(&[4, 1, 0, 0, 0, 0, 0, 0, 0, 0]).is_ok(),
        "drive 0 once, empty"
    );
    assert!(
        Reply::decode(&[4, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(),
        "drive 0 twice"
    );
    assert!(
        Reply::decode(&[4, 1, 0, 2, 0, 0, 0, 0, 0, 0]).is_err(),
        "no such format"
    );
    assert!(
        Reply::decode(&[4, 1, 0, 1, 1, 1, LAST_TRACK, 0, 0, 0, 0, 0]).is_ok(),
        "the last track of a second side lost"
    );
    assert!(
        Reply::decode(&[4, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0]).is_err(),
        "a single-sided disc's second side lost"
    );
    assert!(
        Reply::decode(&[4, 1, 0, 1, 1, 2, 0, 0, 0, 0, 0, 0]).is_err(),
        "a third side lost"
    );
    assert!(
        Reply::decode(&[4, 1, 0, 1, 1, 0, LAST_TRACK + 1, 0, 0, 0, 0, 0]).is_err(),
        "a track past the last lost"
    );
    let single = |len: usize| {
        let mut bytes = vec![4, 1, 0, 0, 0];
        bytes.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        bytes.resize(bytes.len() + len, 0xE5);
        bytes.push(0);
        bytes
    };
    let largest = ImageFormat::Ssd.max_bytes();
    assert!(Reply::decode(&single(largest)).is_ok());
    assert!(
        Reply::decode(&single(largest + 1)).is_err(),
        "a single-sided image as large as a double-sided one"
    );
    assert!(Reply::decode(&[5, 9]).is_err(), "no such refusal");
    assert!(Reply::decode(&[1, 0]).is_err(), "a byte past the end");
}

#[test]
fn a_session_holds_a_whole_bulk_message_each_way() {
    assert!(session_bounds().is_ok());
}

/// The bound is the largest message exactly, so a field added to it cannot
/// leave a reply the queues were not committed to.
#[test]
fn the_largest_message_is_both_drives_and_the_deck_taken_out_whole() {
    let disc = vec![0xE5; MAX_DISC];
    let tape = vec![0x55; MAX_BULK];
    let written = |drive| Written {
        drive,
        format: ImageFormat::Dsd,
        image: &disc,
        lost: Some((1, LAST_TRACK)),
    };
    let reply = Reply::Media(Media {
        discs: vec![written(PhysicalDrive::Zero), written(PhysicalDrive::One)],
        tape: Some(&tape),
    });
    let encoded = reply.encode();
    assert_eq!(encoded.len(), MAX_MESSAGE);
    assert_eq!(Reply::decode(&encoded), Ok(reply));
    let rom = vec![0xEA; MAX_ROM];
    let configure = Request::Configure(Box::new(Configuration::ModelB {
        os: &rom,
        slots: Box::new([Slot::Rom(&rom); SLOTS]),
        links: 0,
    }));
    assert!(
        configure.encode().len() < MAX_MESSAGE,
        "a full Model B fitting"
    );
}

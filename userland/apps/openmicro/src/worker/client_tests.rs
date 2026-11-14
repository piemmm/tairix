use alloc::vec::Vec;

use alloc::boxed::Box;

use super::*;
use crate::media::disc::ImageFormat;
use crate::worker::protocol::{
    Configuration, Lights, Loaded, Media, Ran, Refusal, Slot, Written, ROW_MAP, SLOTS,
};

fn sent(client: &mut MachineClient, request: &Request<'_>) {
    client.ask(request).expect("idle");
    assert!(client.outgoing().is_some());
    client.sent();
}

fn configure(model: Model) -> Request<'static> {
    Request::Configure(Box::new(match model {
        Model::ModelB => Configuration::ModelB {
            os: &[],
            slots: Box::new([Slot::Empty; SLOTS]),
            links: 0,
        },
        Model::Electron => Configuration::Electron {
            os: &[],
            basic: &[],
            plus1: None,
        },
    }))
}

/// A client whose worker said it is a `model`.
fn configured(model: Model) -> MachineClient {
    let mut client = MachineClient::new();
    sent(&mut client, &configure(model));
    assert_eq!(client.answer(&Reply::Done.encode()), Ok(Reply::Done));
    client
}

fn ran_lit(cycles: u32, sound: &[u8], lights: Lights, drives: [bool; 2]) -> Vec<u8> {
    Reply::Ran(Ran {
        cycles,
        rows: [0; ROW_MAP],
        sound,
        lights,
        drives,
        loaded: Loaded {
            discs: drives,
            ..Loaded::default()
        },
    })
    .encode()
}

fn ran(cycles: u32, sound: &[u8]) -> Vec<u8> {
    ran_lit(cycles, sound, Lights::default(), [false; 2])
}

const RUN: Request<'static> = Request::Run {
    cycles: 40_000,
    sound: true,
    inputs: Vec::new(),
};

#[test]
fn one_request_waits_for_its_answer_at_a_time() {
    let mut client = MachineClient::new();
    assert!(client.is_idle());
    client.ask(&Request::PowerOn).expect("idle");
    assert_eq!(
        client.ask(&Request::PowerOn),
        Err(Busy),
        "waiting to be sent"
    );
    client.sent();
    assert_eq!(client.outgoing(), None);
    assert_eq!(
        client.ask(&Request::PowerOn),
        Err(Busy),
        "waiting for its answer"
    );
    assert_eq!(client.answer(&Reply::Done.encode()), Ok(Reply::Done));
    assert!(client.is_idle());
}

#[test]
fn a_reply_is_believed_only_if_it_answers_what_was_asked() {
    let mut client = MachineClient::new();
    assert!(
        client.answer(&Reply::Done.encode()).is_err(),
        "nothing asked"
    );
    sent(&mut client, &RUN);
    assert!(
        client.answer(&ran(40_000, &[])).is_err(),
        "no machine configured yet"
    );
    let mut client = configured(Model::ModelB);
    sent(&mut client, &RUN);
    assert!(
        client.answer(&Reply::Done.encode()).is_err(),
        "a slice is answered by Ran"
    );
    sent(&mut client, &RUN);
    assert!(client.answer(&ran(39_999, &[])).is_err(), "another slice's");
    sent(&mut client, &RUN);
    let most = 40_000 * 48_000 / 2_000_000 + 2;
    assert!(
        client
            .answer(&ran(40_000, &alloc::vec![0; (most + 1) * 2]))
            .is_err(),
        "more sound than it makes"
    );
    sent(&mut client, &RUN);
    assert!(client
        .answer(&ran(40_000, &alloc::vec![0; most * 2]))
        .is_ok());
    sent(
        &mut client,
        &Request::Run {
            cycles: 10,
            sound: false,
            inputs: Vec::new(),
        },
    );
    assert!(
        client.answer(&ran(10, &[0, 0])).is_err(),
        "sound not asked for"
    );
    sent(&mut client, &Request::TakeSnapshot);
    assert!(
        client
            .answer(&Reply::Media(Media::default()).encode())
            .is_err(),
        "not a snapshot"
    );
    sent(&mut client, &Request::CollectDiscs([true; 2]));
    assert!(
        client
            .answer(&Reply::Refused(Refusal::Unconfigured).encode())
            .is_ok(),
        "a refusal answers anything"
    );
    sent(&mut client, &Request::EjectTape);
    assert!(
        client.answer(&Reply::Done.encode()).is_err(),
        "an eject is answered with what it took out"
    );
    sent(&mut client, &Request::EjectTape);
    assert!(
        client
            .answer(&Reply::Media(Media::default()).encode())
            .is_ok(),
        "no tape was in"
    );
    sent(&mut client, &Request::PowerOn);
    assert!(client.answer(&[0xEE]).is_err(), "no such reply");
}

#[test]
fn a_restart_forgets_what_was_asked() {
    let mut client = MachineClient::new();
    sent(&mut client, &Request::TakeSnapshot);
    client.restart();
    assert!(client.is_idle());
    assert!(client.answer(&Reply::Snapshot(&[]).encode()).is_err());
}

fn media(drives: &[PhysicalDrive], tape: bool) -> Vec<u8> {
    Reply::Media(Media {
        discs: drives
            .iter()
            .map(|&drive| Written {
                drive,
                format: ImageFormat::Ssd,
                image: &[1, 2],
                lost: None,
            })
            .collect(),
        tape: tape.then_some(&[3, 4][..]),
    })
    .encode()
}

#[test]
fn media_are_believed_only_from_the_drives_and_deck_a_request_reaches() {
    let (zero, one) = (PhysicalDrive::Zero, PhysicalDrive::One);
    let cases: [(Request<'_>, Vec<u8>, bool); 10] = [
        (Request::EjectDisc(zero), media(&[zero], false), true),
        (Request::EjectDisc(zero), media(&[one], false), false),
        (Request::EjectDisc(one), media(&[one], true), false),
        (
            Request::CollectDiscs([true; 2]),
            media(&[zero, one], false),
            true,
        ),
        (Request::CollectDiscs([true; 2]), media(&[], true), false),
        (
            Request::CollectDiscs([false, true]),
            media(&[one], false),
            true,
        ),
        (
            Request::CollectDiscs([false, true]),
            media(&[zero], false),
            false,
        ),
        (Request::InsertTape(&[]), media(&[], true), true),
        (Request::InsertTape(&[]), media(&[zero], true), false),
        (
            Request::RestoreSnapshot(&[]),
            media(&[zero, one], true),
            true,
        ),
    ];
    for (request, reply, believed) in cases {
        let mut client = configured(Model::ModelB);
        sent(&mut client, &request);
        assert_eq!(client.answer(&reply).is_ok(), believed, "{request:?}");
    }
}

#[test]
fn an_electrons_answers_are_believed_only_with_its_own_lamps_and_media() {
    let lit = |shift_lock| Lights {
        caps_lock: true,
        shift_lock,
        motor: true,
    };
    for (model, believed) in [(Model::ModelB, true), (Model::Electron, false)] {
        let mut client = configured(model);
        sent(&mut client, &RUN);
        assert!(client
            .answer(&ran_lit(40_000, &[], lit(false), [false; 2]))
            .is_ok());
        sent(&mut client, &RUN);
        let shift = client
            .answer(&ran_lit(40_000, &[], lit(true), [false; 2]))
            .is_ok();
        assert_eq!(shift, believed, "{model:?}'s SHIFT LOCK light");
        sent(&mut client, &RUN);
        let drive = client
            .answer(&ran_lit(40_000, &[], lit(false), [true, false]))
            .is_ok();
        assert_eq!(drive, believed, "{model:?}'s drive");
        sent(&mut client, &Request::RestoreSnapshot(&[]));
        let discs = client.answer(&media(&[PhysicalDrive::Zero], false)).is_ok();
        assert_eq!(discs, believed, "{model:?}'s discs");
    }
}

#[test]
fn the_model_is_the_last_configure_the_worker_took_and_a_restart_forgets_it() {
    let mut client = configured(Model::ModelB);
    sent(&mut client, &configure(Model::Electron));
    assert!(client
        .answer(&Reply::Refused(Refusal::Model).encode())
        .is_ok());
    sent(&mut client, &RUN);
    assert!(
        client
            .answer(&ran_lit(40_000, &[], Lights::default(), [true; 2]))
            .is_ok(),
        "still a Model B"
    );
    client.restart();
    sent(&mut client, &RUN);
    assert!(
        client.answer(&ran(40_000, &[])).is_err(),
        "no model since the restart"
    );
}

use super::*;
use crate::window_ipc::{WindowEvent, WindowRequest, WINDOW_ID_WIRE_LEN};

fn words(args: &[&str]) -> ElevateWords {
    ElevateWords::new(args).expect("within bounds")
}

fn authorise(task: &SystemTask) -> WindowRequest {
    WindowRequest::Authorise {
        window_id: 7,
        task: *task,
    }
}

/// `request` encoded, with one byte of room past its end.
fn encoded(request: &WindowRequest) -> ([u8; WindowRequest::MAX_WIRE_LEN + 1], usize) {
    let mut frame = [0u8; WindowRequest::MAX_WIRE_LEN + 1];
    let len = request.encode(&mut frame).expect("encodes");
    (frame, len)
}

#[test]
fn every_task_round_trips_and_costs_what_it_carries() {
    for task in [
        SystemTask::Configure(words(&["cache.all", "off"])),
        SystemTask::ChangeAccount(words(&["--lock", "--", "ada"])),
        SystemTask::SetPassword(words(&["--record", "r", "--", "ada"])),
        SystemTask::ShowAddressing,
        SystemTask::ShowAccounts,
        SystemTask::SetClock,
    ] {
        let request = authorise(&task);
        let (frame, len) = encoded(&request);
        assert_eq!(len, WINDOW_ID_WIRE_LEN + wire_len(&task));
        assert_eq!(WindowRequest::from_bytes(&frame[..len]), Ok(request));
    }
    assert_eq!(wire_len(&SystemTask::SetClock), WORDS_AT);
}

#[test]
fn a_task_that_takes_words_is_refused_without_one_either_way() {
    for task in [
        SystemTask::Configure(ElevateWords::NONE),
        SystemTask::ChangeAccount(ElevateWords::NONE),
        SystemTask::SetPassword(ElevateWords::NONE),
    ] {
        let mut frame = [0u8; WindowRequest::MAX_WIRE_LEN];
        assert_eq!(authorise(&task).encode(&mut frame), Err(Errno::OutOfRange));
    }
    let (mut frame, len) = encoded(&authorise(&SystemTask::SetClock));
    frame[WINDOW_ID_WIRE_LEN + KIND_AT] = KIND_CONFIGURE;
    assert_eq!(
        WindowRequest::from_bytes(&frame[..len]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_task_that_takes_no_words_is_refused_with_some() {
    let (mut frame, len) = encoded(&authorise(&SystemTask::Configure(words(&["k", "v"]))));
    for kind in [KIND_SHOW_ADDRESSING, KIND_SHOW_ACCOUNTS, KIND_SET_CLOCK] {
        frame[WINDOW_ID_WIRE_LEN + KIND_AT] = kind;
        assert_eq!(
            WindowRequest::from_bytes(&frame[..len]),
            Err(Errno::OutOfRange)
        );
    }
}

#[test]
fn a_malformed_task_is_refused_whole() {
    let (frame, len) = encoded(&authorise(&SystemTask::Configure(words(&["k", "v"]))));
    let block = WINDOW_ID_WIRE_LEN;

    let mut unknown = frame;
    unknown[block + KIND_AT] = 0;
    assert_eq!(
        WindowRequest::from_bytes(&unknown[..len]),
        Err(Errno::OutOfRange)
    );
    unknown[block + KIND_AT] = KIND_SET_CLOCK + 1;
    assert_eq!(
        WindowRequest::from_bytes(&unknown[..len]),
        Err(Errno::OutOfRange)
    );

    let mut reserved = frame;
    reserved[block + RESERVED_AT] = 1;
    assert_eq!(
        WindowRequest::from_bytes(&reserved[..len]),
        Err(Errno::BadMagic)
    );

    // A count the words do not hold.
    let mut miscounted = frame;
    miscounted[block + COUNT_AT] = 3;
    assert_eq!(
        WindowRequest::from_bytes(&miscounted[..len]),
        Err(Errno::LengthOutOfRange)
    );

    // A word that is not text.
    let mut dirty = frame;
    dirty[len - 1] = 0xFF;
    assert_eq!(
        WindowRequest::from_bytes(&dirty[..len]),
        Err(Errno::OutOfRange)
    );

    // Short is truncation and long is smuggling, whatever the length says.
    assert_eq!(
        WindowRequest::from_bytes(&frame[..len - 1]),
        Err(Errno::BufferTooSmall)
    );
    assert_eq!(
        WindowRequest::from_bytes(&frame[..=len]),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        WindowRequest::from_bytes(&frame[..block + WORDS_AT - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn an_authorisation_names_a_window() {
    let (mut frame, len) = encoded(&authorise(&SystemTask::SetClock));
    frame[8..16].fill(0);
    assert_eq!(
        WindowRequest::from_bytes(&frame[..len]),
        Err(Errno::OutOfRange)
    );
    let (mut take, take_len) = encoded(&WindowRequest::TakeAuthorisedOutput { window_id: 7 });
    take[8..16].fill(0);
    assert_eq!(
        WindowRequest::from_bytes(&take[..take_len]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn every_outcome_round_trips_in_one_event() {
    for outcome in [
        AuthorisedOutcome::Ran { exit_code: 0 },
        AuthorisedOutcome::Ran { exit_code: -3 },
        AuthorisedOutcome::Launched,
        AuthorisedOutcome::Captured {
            exit_code: 0,
            len: 0,
        },
        AuthorisedOutcome::Captured {
            exit_code: 1,
            len: u32::try_from(ELEVATE_MAX_OUTPUT).expect("small"),
        },
        AuthorisedOutcome::Overran { exit_code: 2 },
        AuthorisedOutcome::Declined,
    ] {
        let event = WindowEvent::Authorised {
            window_id: 7,
            outcome,
        };
        assert_eq!(event.window_id(), Some(7));
        assert_eq!(WindowEvent::from_bytes(&event.to_le_bytes()), Ok(event));
    }
}

#[test]
fn an_outcome_states_only_what_its_case_carries() {
    let frame = |outcome| {
        WindowEvent::Authorised {
            window_id: 7,
            outcome,
        }
        .to_le_bytes()
    };
    // A capture longer than one answer holds.
    let mut long = frame(AuthorisedOutcome::Captured {
        exit_code: 0,
        len: 1,
    });
    put_u32(
        &mut long,
        AUTHORISED_LEN_OFFSET,
        u32::try_from(ELEVATE_MAX_OUTPUT + 1).expect("small"),
    );
    assert_eq!(WindowEvent::from_bytes(&long), Err(Errno::OutOfRange));

    // A run that names a length, and outcomes that ran nothing naming an exit
    // code or a length.
    let mut ran = frame(AuthorisedOutcome::Ran { exit_code: 0 });
    put_u32(&mut ran, AUTHORISED_LEN_OFFSET, 1);
    assert_eq!(WindowEvent::from_bytes(&ran), Err(Errno::OutOfRange));
    for nothing in [AuthorisedOutcome::Launched, AuthorisedOutcome::Declined] {
        let mut coded = frame(nothing);
        put_i32(&mut coded, AUTHORISED_EXIT_OFFSET, 1);
        assert_eq!(WindowEvent::from_bytes(&coded), Err(Errno::OutOfRange));
    }

    // An unknown outcome, padding inside the block, and bytes past it.
    let mut unknown = frame(AuthorisedOutcome::Declined);
    unknown[AUTHORISED_OUTCOME_OFFSET] = OUTCOME_DECLINED + 1;
    assert_eq!(WindowEvent::from_bytes(&unknown), Err(Errno::OutOfRange));
    let mut padded = frame(AuthorisedOutcome::Declined);
    padded[AUTHORISED_OUTCOME_OFFSET + 1] = 1;
    assert_eq!(WindowEvent::from_bytes(&padded), Err(Errno::BadMagic));
    let mut trailing = frame(AuthorisedOutcome::Declined);
    trailing[AUTHORISED_WIRE_END] = 1;
    assert_eq!(WindowEvent::from_bytes(&trailing), Err(Errno::BadMagic));
}

#[test]
fn a_captured_output_reply_round_trips_at_every_length() {
    let mut out = [0u8; WINDOW_AUTHORISED_OUTPUT_REPLY_MAX];
    let widest = [b'x'; ELEVATE_MAX_OUTPUT];
    for output in [&[][..], b"wan.ipv4.method dhcp\n", &widest[..]] {
        let len = encode_authorised_output_reply(&mut out, Ok(output));
        assert_eq!(len, OUTPUT_REPLY_BYTES_OFFSET + output.len());
        assert_eq!(decode_authorised_output_reply(&out[..len]), Ok(output));
    }
}

#[test]
fn a_captured_output_reply_refuses_what_it_could_not_have_carried() {
    let mut out = [0u8; WINDOW_AUTHORISED_OUTPUT_REPLY_MAX];
    let past = [b'x'; ELEVATE_MAX_OUTPUT + 1];
    let len = encode_authorised_output_reply(&mut out, Ok(&past));
    assert_eq!(
        decode_authorised_output_reply(&out[..len]),
        Err(Errno::LengthOutOfRange)
    );
    let len = encode_authorised_output_reply(&mut out, Err(Errno::NotFound));
    assert_eq!(
        decode_authorised_output_reply(&out[..len]),
        Err(Errno::NotFound)
    );

    let len = encode_authorised_output_reply(&mut out, Ok(b"listing"));
    assert_eq!(
        decode_authorised_output_reply(&out[..len - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        decode_authorised_output_reply(&out[..OUTPUT_REPLY_BYTES_OFFSET - 1]),
        Err(Errno::BufferTooSmall)
    );
}

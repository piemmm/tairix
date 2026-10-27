//! Unit tests for the client connection core, against a scripted server and
//! against a Windows server's sealed answer.

use alloc::vec;
use alloc::vec::Vec;

use super::{Connection, Outgoing, Received, Ticket, MAX_CREDITS};
use crate::crypt::{
    KeyInputs, PreauthHash, SessionKeys, TRANSFORM_HEADER_LEN, TRANSFORM_PROTOCOL_ID,
};
use crate::frame::{self, PREFIX_LEN};
use crate::header::{Address, Command, Flags, Header};
use crate::negotiate::{Cipher, Dialect, SigningAlgorithm};
use crate::simple::encode_empty;
use crate::status::NtStatus;
use crate::vectors::{self, bytes};
use crate::wire::Encoder;
use crate::{Role, SmbError};

const SESSION: u64 = 0x0000_0400_0000_0019;

fn request(command: Command, session_id: u64) -> Outgoing<'static> {
    Outgoing {
        command,
        session_id,
        tree_id: 3,
        body: &encode_empty,
        credit_charge: u16::from(command != Command::Negotiate),
        flags: Flags::NONE,
    }
}

fn response(
    command: Command,
    id: u64,
    credits: u16,
    status: NtStatus,
    address: Address,
) -> Vec<u8> {
    let mut enc = Encoder::new();
    Header {
        credit_charge: 1,
        status,
        command,
        credits,
        flags: Flags::SERVER_TO_REDIR,
        next_command: 0,
        message_id: id,
        address,
        session_id: SESSION,
        signature: [0; 16],
    }
    .encode(&mut enc);
    encode_empty(&mut enc);
    enc.finish().expect("encodes")
}

const SYNC: Address = Address::Sync { tree_id: 3 };

fn framed(frame_bytes: &[u8]) -> Vec<u8> {
    let mut out = frame::prefix(frame_bytes.len()).expect("fits").to_vec();
    out.extend_from_slice(frame_bytes);
    out
}

/// A connection past `NEGOTIATE`, holding `credits`.
fn negotiated(credits: u16) -> Connection {
    let mut conn = Connection::new();
    conn.send(&[request(Command::Negotiate, 0)]).expect("sends");
    conn.take_output();
    let answer = response(Command::Negotiate, 0, credits, NtStatus::SUCCESS, SYNC);
    conn.receive(&framed(&answer)).expect("receives");
    assert!(matches!(
        conn.next_received(),
        Some(Received::Response { .. })
    ));
    conn
}

fn keys(role: Role) -> SessionKeys {
    let preauth = PreauthHash::new();
    let inputs = KeyInputs {
        dialect: Dialect::Smb311,
        cipher: Cipher::Aes128Gcm,
        signing: SigningAlgorithm::AesCmac,
        session_key: &[0x5a; 16],
        preauth: &preauth,
    };
    SessionKeys::derive(&inputs, role, &[7; 12]).expect("derives")
}

/// A connection past `NEGOTIATE` whose session [`SESSION`] holds keys.
fn keyed(credits: u16) -> Connection {
    let mut conn = negotiated(credits);
    conn.install_keys(SESSION, keys(Role::Client))
        .expect("installs");
    conn
}

/// `plaintext` as the server seals it.
fn sealed_by(server: &mut SessionKeys, session_id: u64, plaintext: &[u8]) -> Vec<u8> {
    let mut frame = vec![0; TRANSFORM_HEADER_LEN];
    frame.extend_from_slice(plaintext);
    server.seal(session_id, &mut frame).expect("seals");
    frame
}

/// The frames the client wrote, each without its prefix.
fn frames(mut output: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !output.is_empty() {
        let len = usize::try_from(u32::from_be_bytes([0, output[1], output[2], output[3]]))
            .expect("fits");
        out.push(output[PREFIX_LEN..PREFIX_LEN + len].to_vec());
        output = &output[PREFIX_LEN + len..];
    }
    out
}

#[test]
fn negotiate_spends_the_one_credit_a_connection_starts_with() {
    let mut conn = Connection::new();
    let sent = conn.send(&[request(Command::Negotiate, 0)]).expect("sends");
    assert_eq!(sent.tickets, [Ticket(0)]);
    assert_eq!(conn.credits(), 0);
    let output = conn.take_output();
    assert_eq!(
        output[..PREFIX_LEN],
        frame::prefix(sent.clear.len()).expect("fits")
    );
    assert_eq!(
        output[PREFIX_LEN..],
        sent.clear[..],
        "what went in the clear is reported"
    );
    let header = Header::decode(&sent.clear).expect("header");
    assert_eq!((header.message_id, header.credit_charge), (0, 0));
    assert_eq!(
        conn.send(&[request(Command::Echo, 0)]).map(|_| ()),
        Err(SmbError::NoCredit)
    );
}

#[test]
fn a_response_is_matched_and_grants_its_credits() {
    let mut conn = negotiated(10);
    assert_eq!(conn.credits(), 10);
    let sent = conn.send(&[request(Command::Echo, 0)]).expect("sends");
    assert_eq!(sent.tickets, [Ticket(1)]);
    let answer = response(Command::Echo, 1, 3, NtStatus::SUCCESS, SYNC);
    conn.receive(&framed(&answer)).expect("receives");
    assert_eq!(conn.credits(), 12);
    match conn.next_received() {
        Some(Received::Response {
            ticket,
            header,
            message,
            sealed,
        }) => {
            assert_eq!(ticket, Ticket(1));
            assert_eq!(header, Header::decode(&answer).expect("header"));
            assert_eq!(*message, answer[..]);
            assert!(!sealed);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(conn.outstanding(), 0);
}

#[test]
fn a_server_leaving_no_credit_and_nothing_outstanding_ends_the_connection() {
    let mut conn = negotiated(1);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    let starved = response(Command::Echo, 1, 0, NtStatus::SUCCESS, SYNC);
    assert_eq!(conn.receive(&framed(&starved)), Err(SmbError::Protocol));

    let mut conn = negotiated(2);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    conn.receive(&framed(&response(
        Command::Echo,
        1,
        0,
        NtStatus::SUCCESS,
        SYNC,
    )))
    .expect("a request still outstanding can bring credits");
    conn.receive(&framed(&response(
        Command::Echo,
        2,
        1,
        NtStatus::SUCCESS,
        SYNC,
    )))
    .expect("and does");
    assert_eq!(conn.credits(), 1);

    let mut conn = Connection::new();
    conn.send(&[request(Command::Negotiate, 0)]).expect("sends");
    let none = response(Command::Negotiate, 0, 0, NtStatus::SUCCESS, SYNC);
    assert_eq!(
        conn.receive(&framed(&none)),
        Err(SmbError::Protocol),
        "a negotiation granting nothing"
    );
}

#[test]
fn a_response_split_across_reads_is_reassembled() {
    let mut conn = negotiated(4);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    for byte in framed(&response(Command::Echo, 1, 1, NtStatus::SUCCESS, SYNC)) {
        conn.receive(&[byte]).expect("receives");
    }
    assert!(matches!(
        conn.next_received(),
        Some(Received::Response { .. })
    ));
}

#[test]
fn a_response_to_nothing_or_the_wrong_command_ends_the_connection() {
    for bad in [
        response(Command::Echo, 7, 1, NtStatus::SUCCESS, SYNC),
        response(Command::Read, 1, 1, NtStatus::SUCCESS, SYNC),
    ] {
        let mut conn = negotiated(4);
        conn.send(&[request(Command::Echo, 0)]).expect("sends");
        assert_eq!(conn.receive(&framed(&bad)), Err(SmbError::Protocol));
        assert_eq!(
            conn.send(&[request(Command::Echo, 0)]).map(|_| ()),
            Err(SmbError::Protocol),
            "the first failure is every later answer"
        );
        assert_eq!(conn.failure(), Some(SmbError::Protocol));
    }
}

#[test]
fn a_request_from_the_server_is_a_protocol_violation() {
    let mut conn = negotiated(4);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    let mut message = response(Command::Echo, 1, 1, NtStatus::SUCCESS, SYNC);
    message[16] &= !1;
    assert_eq!(conn.receive(&framed(&message)), Err(SmbError::Protocol));
}

#[test]
fn an_interim_response_defers_the_final_one_and_routes_the_cancel() {
    let mut conn = negotiated(4);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    conn.take_output();
    let async_id = Address::Async { async_id: 99 };
    let interim = response(Command::Echo, 1, 1, NtStatus::PENDING, async_id);
    conn.receive(&framed(&interim)).expect("interim");
    assert_eq!(conn.next_received(), None);
    assert_eq!(conn.outstanding(), 1);

    conn.cancel(Ticket(1)).expect("cancels");
    let cancel = &frames(&conn.take_output())[0];
    let header = Header::decode(cancel).expect("header");
    assert_eq!(header.command, Command::Cancel);
    assert_eq!(header.address, async_id);
    assert_eq!((header.message_id, header.credit_charge), (1, 0));

    let last = response(Command::Echo, 1, 1, NtStatus::CANCELLED, async_id);
    conn.receive(&framed(&last)).expect("final");
    assert!(matches!(
        conn.next_received(),
        Some(Received::Response { .. })
    ));
    assert_eq!(
        conn.cancel(Ticket(1)),
        Err(SmbError::Protocol),
        "nothing left to cancel"
    );
}

#[test]
fn a_cancel_before_any_interim_names_the_requests_tree() {
    let mut conn = negotiated(4);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    conn.take_output();
    conn.cancel(Ticket(1)).expect("cancels");
    let header = Header::decode(&frames(&conn.take_output())[0]).expect("header");
    assert_eq!(header.address, SYNC);
}

#[test]
fn a_request_has_at_most_one_interim_response_and_keeps_its_async_id() {
    let async_id = Address::Async { async_id: 99 };
    let interim = response(Command::Echo, 1, 1, NtStatus::PENDING, async_id);
    for second in [
        interim.clone(),
        response(
            Command::Echo,
            1,
            1,
            NtStatus::SUCCESS,
            Address::Async { async_id: 98 },
        ),
        response(Command::Echo, 1, 1, NtStatus::SUCCESS, SYNC),
        response(Command::Echo, 1, 1, NtStatus::PENDING, SYNC),
    ] {
        let mut conn = negotiated(4);
        conn.send(&[request(Command::Echo, 0)]).expect("sends");
        conn.receive(&framed(&interim)).expect("interim");
        assert_eq!(conn.receive(&framed(&second)), Err(SmbError::Protocol));
    }
    let mut conn = negotiated(4);
    conn.send(&[request(Command::Echo, 0)]).expect("sends");
    let sync_interim = response(Command::Echo, 1, 1, NtStatus::PENDING, SYNC);
    assert_eq!(
        conn.receive(&framed(&sync_interim)),
        Err(SmbError::Protocol)
    );
}

#[test]
fn keys_are_installed_once_and_never_for_session_zero() {
    let mut conn = keyed(4);
    assert_eq!(
        conn.install_keys(SESSION, keys(Role::Client)),
        Err(SmbError::Protocol)
    );
    assert_eq!(
        conn.install_keys(0, keys(Role::Client)),
        Err(SmbError::Protocol)
    );
    conn.remove_session(SESSION);
    assert_eq!(conn.install_keys(SESSION, keys(Role::Client)), Ok(()));
}

#[test]
fn a_sealed_request_is_answered_only_under_the_same_sessions_keys() {
    let mut server = keys(Role::Server);
    let reply = response(Command::TreeConnect, 1, 1, NtStatus::SUCCESS, SYNC);

    let mut conn = keyed(4);
    let sent = conn
        .send(&[request(Command::TreeConnect, SESSION)])
        .expect("sends");
    assert!(sent.clear.is_empty(), "nothing sealed is reported as clear");
    let mut sealed = frames(&conn.take_output()).remove(0);
    assert_eq!(sealed[..4], TRANSFORM_PROTOCOL_ID);
    let plain = server.open(&mut sealed).expect("the server opens it");
    assert_eq!(
        Header::decode(plain).expect("header").command,
        Command::TreeConnect
    );
    assert_eq!(
        conn.receive(&framed(&reply)),
        Err(SmbError::Integrity),
        "in the clear"
    );

    let mut conn = keyed(4);
    conn.send(&[request(Command::TreeConnect, SESSION)])
        .expect("sends");
    let stranger = sealed_by(&mut server, SESSION + 1, &reply);
    assert_eq!(
        conn.receive(&framed(&stranger)),
        Err(SmbError::Integrity),
        "another session"
    );

    let mut conn = keyed(4);
    conn.send(&[request(Command::TreeConnect, SESSION)])
        .expect("sends");
    conn.receive(&framed(&sealed_by(&mut server, SESSION, &reply)))
        .expect("receives");
    assert!(matches!(
        conn.next_received(),
        Some(Received::Response { sealed: true, .. })
    ));
}

#[test]
fn a_sealed_answer_must_name_its_session() {
    let mut server = keys(Role::Server);
    let mut reply = response(Command::TreeConnect, 1, 1, NtStatus::SUCCESS, SYNC);
    reply[40..48].copy_from_slice(&(SESSION + 1).to_le_bytes());
    let mut conn = keyed(4);
    conn.send(&[request(Command::TreeConnect, SESSION)])
        .expect("sends");
    let frame = sealed_by(&mut server, SESSION, &reply);
    assert_eq!(conn.receive(&framed(&frame)), Err(SmbError::Protocol));
}

#[test]
fn each_sealed_frame_takes_a_fresh_nonce() {
    let mut conn = keyed(8);
    conn.send(&[request(Command::Echo, SESSION)])
        .expect("sends");
    conn.send(&[request(Command::Echo, SESSION)])
        .expect("sends");
    let sent = frames(&conn.take_output());
    assert_ne!(sent[0][20..36], sent[1][20..36]);
}

#[test]
fn a_windows_servers_sealed_answer_is_accepted() {
    let session = &vectors::SMB311_GCM;
    let key = bytes(session.session_key);
    let mut preauth = PreauthHash::new();
    for (message, _) in vectors::SMB311_GCM_PREAUTH {
        preauth.update(&bytes(message));
    }
    let inputs = KeyInputs {
        dialect: session.dialect,
        cipher: session.cipher,
        signing: SigningAlgorithm::AesCmac,
        session_key: &key,
        preauth: &preauth,
    };
    let mut conn = negotiated(16);
    conn.install_keys(
        session.session_id,
        SessionKeys::derive(&inputs, Role::Client, &[0; 12]).expect("derives"),
    )
    .expect("installs");
    // Windows wrote at message id 5, the client's sixth request.
    for _ in 1..5 {
        conn.send(&[request(Command::Echo, session.session_id)])
            .expect("sends");
    }
    let sent = conn
        .send(&[request(Command::Write, session.session_id)])
        .expect("sends");
    assert_eq!(sent.tickets, [Ticket(5)]);
    conn.receive(&framed(&bytes(session.write.sealed_response)))
        .expect("receives");
    match conn.next_received() {
        Some(Received::Response {
            ticket,
            message,
            sealed,
            ..
        }) => {
            assert_eq!(ticket, Ticket(5));
            assert_eq!(*message, bytes(session.write.response)[..]);
            assert!(sealed);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_compound_is_one_frame_and_its_answers_are_split() {
    let mut conn = negotiated(8);
    let sent = conn
        .send(&[request(Command::Echo, 0), request(Command::Echo, 0)])
        .expect("sends");
    assert_eq!(sent.tickets, [Ticket(1), Ticket(2)]);
    let output = frames(&conn.take_output());
    assert_eq!(output.len(), 1);
    let ranges = frame::messages(&output[0]).expect("splits");
    assert_eq!(ranges, [0..72, 72..140]);
    assert_eq!(Header::decode(&output[0]).expect("header").next_command, 72);
    assert_eq!(
        Header::decode(&output[0][72..]).expect("header").message_id,
        2
    );

    let mut both = response(Command::Echo, 1, 1, NtStatus::SUCCESS, SYNC);
    both.resize(72, 0);
    both[20..24].copy_from_slice(&72u32.to_le_bytes());
    let second = response(Command::Echo, 2, 1, NtStatus::SUCCESS, SYNC);
    both.extend_from_slice(&second);
    conn.receive(&framed(&both)).expect("receives");
    assert!(matches!(
        conn.next_received(),
        Some(Received::Response {
            ticket: Ticket(1),
            ..
        })
    ));
    match conn.next_received() {
        Some(Received::Response {
            ticket, message, ..
        }) => {
            assert_eq!(ticket, Ticket(2));
            assert_eq!(*message, second[..]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_compound_may_not_span_sessions_or_be_empty() {
    let mut conn = negotiated(8);
    assert_eq!(
        conn.send(&[request(Command::Echo, 0), request(Command::Echo, SESSION)])
            .map(|_| ()),
        Err(SmbError::Protocol)
    );
    assert_eq!(conn.send(&[]).map(|_| ()), Err(SmbError::Protocol));
    assert_eq!(
        conn.outstanding(),
        0,
        "a refused send leaves nothing behind"
    );
}

#[test]
fn an_unsolicited_break_is_delivered_and_nothing_else_may_borrow_its_id() {
    let mut conn = negotiated(4);
    let notice = response(Command::OplockBreak, u64::MAX, 0, NtStatus::SUCCESS, SYNC);
    conn.receive(&framed(&notice)).expect("receives");
    assert!(matches!(conn.next_received(), Some(Received::Break { .. })));
    let borrowed = response(Command::Echo, u64::MAX, 0, NtStatus::SUCCESS, SYNC);
    assert_eq!(conn.receive(&framed(&borrowed)), Err(SmbError::Protocol));
}

#[test]
fn a_malformed_or_oversized_frame_ends_the_connection() {
    let mut conn = negotiated(4);
    assert_eq!(
        conn.receive(&[1, 0, 0, 4, 0, 0, 0, 0]),
        Err(SmbError::Malformed)
    );
    let mut conn = negotiated(4);
    assert_eq!(
        conn.receive(&[0, 0xff, 0xff, 0xff]),
        Err(SmbError::TooLarge)
    );
    let mut conn = negotiated(4);
    assert_eq!(
        conn.receive(&framed(b"\xfcSMB compressed")),
        Err(SmbError::Protocol)
    );
}

#[test]
fn credits_never_exceed_the_bound() {
    let conn = negotiated(u16::MAX);
    assert!(conn.credits() <= MAX_CREDITS);
}

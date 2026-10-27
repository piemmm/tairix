//! Unit tests for the client, driven against the scripted server, which
//! checks independently what a real one would: the NTLMv2 proof, the MIC, the
//! service name, the mechListMIC, and — through the keys it derives — the
//! pre-auth hash.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{Client, Event, Mechanism, Request, Tree, MAX_TREES};
use crate::connection::Ticket;
use crate::crypt::{NONCE_SEED_LEN, TRANSFORM_HEADER_LEN, TRANSFORM_PROTOCOL_ID};
use crate::frame::{self, PREFIX_LEN};
use crate::header::{Address, Command, Flags, Header};
use crate::negotiate::{Cipher, Dialect, Offer, SigningAlgorithm};
use crate::ntlm::{NtlmCredential, NtlmInitiator, NtlmInputs};
use crate::peer::{Fault, Hints, Peer, PeerConfig, DISK_SHARE};
use crate::session::SessionSetupRequest;
use crate::simple::encode_empty;
use crate::spnego::{NegState, Reply};
use crate::status::NtStatus;
use crate::tree::ShareType;
use crate::vectors;
use crate::wire::Encoder;
use crate::{Refusal, SmbError};

const SEED: [u8; NONCE_SEED_LEN] = [0x3c; NONCE_SEED_LEN];

fn offer(host: &str) -> Offer {
    Offer {
        floor: Dialect::Smb300,
        client_guid: [0x11; 16],
        salt: [0x22; 32],
        server_name: String::from(host),
    }
}

fn ntlm(config: &PeerConfig, password: &str) -> Mechanism {
    let credential = NtlmCredential::new(config.user, config.domain, password).expect("names fit");
    let mut target = String::from("cifs/");
    target.push_str(config.host);
    let inputs = NtlmInputs {
        client_challenge: [0xcc; 8],
        now: Time64::from_secs(1_790_000_000),
    };
    Mechanism::Ntlm(NtlmInitiator::new(credential, &target, inputs).expect("target fits"))
}

/// The frames in `output`, each without its prefix.
fn frames(mut output: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while !output.is_empty() {
        let len = usize::try_from(u32::from_be_bytes([0, output[1], output[2], output[3]]))
            .expect("fits");
        out.push(&output[PREFIX_LEN..PREFIX_LEN + len]);
        output = &output[PREFIX_LEN + len..];
    }
    out
}

/// Carry bytes both ways until neither side has more to say, keeping what
/// the client wrote; the first failure the client meets.
fn pump_into(client: &mut Client, peer: &mut Peer, written: &mut Vec<u8>) -> Result<(), SmbError> {
    loop {
        let to_peer = client.take_output();
        peer.receive(&to_peer)
            .expect("the server reads what the client sends");
        written.extend_from_slice(&to_peer);
        let to_client = peer.take_output();
        if to_peer.is_empty() && to_client.is_empty() {
            return Ok(());
        }
        client.receive(&to_client)?;
    }
}

fn pump(client: &mut Client, peer: &mut Peer) -> Result<(), SmbError> {
    pump_into(client, peer, &mut Vec::new())
}

fn events(client: &mut Client) -> Vec<Event> {
    core::iter::from_fn(|| client.next_event()).collect()
}

/// A client whose negotiation with `config`'s server has settled.
fn negotiated(config: &PeerConfig) -> (Client, Peer) {
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(offer(config.host)).expect("encodes");
    pump(&mut client, &mut peer).expect("negotiates");
    assert_eq!(events(&mut client), [Event::Negotiated]);
    (client, peer)
}

/// A client with an established session on `config`'s server.
fn established(config: &PeerConfig) -> (Client, Peer) {
    let (mut client, mut peer) = negotiated(config);
    client
        .authenticate(ntlm(config, config.password), SEED)
        .expect("starts");
    pump(&mut client, &mut peer).expect("sets up");
    let session_id = peer.session_id().expect("the server keyed a session");
    assert_eq!(events(&mut client), [Event::Established { session_id }]);
    (client, peer)
}

/// A client with the disk share connected on `config`'s server.
fn connected(config: &PeerConfig) -> (Client, Peer, Tree) {
    let (mut client, mut peer) = established(config);
    let ticket = client.connect_tree(DISK_SHARE).expect("asks");
    pump(&mut client, &mut peer).expect("connects");
    match events(&mut client)[..] {
        [Event::TreeConnected {
            ticket: answered,
            tree,
        }] if answered == ticket => (client, peer, tree),
        ref other => panic!("{other:?}"),
    }
}

const ECHO: Request<'static> = Request {
    command: Command::Echo,
    body: &encode_empty,
    credit_charge: 1,
    flags: Flags::NONE,
};

/// The status of the response `event` carries for `ticket`.
fn status_of(event: &Event, ticket: Ticket) -> NtStatus {
    match event {
        Event::Response {
            ticket: answered,
            message,
        } if *answered == ticket => Header::decode(message).expect("a header").status,
        other => panic!("{other:?}"),
    }
}

fn config(dialect: Dialect, cipher: Cipher, signing: Option<SigningAlgorithm>) -> PeerConfig {
    PeerConfig {
        cipher,
        signing,
        ..PeerConfig::new(dialect)
    }
}

#[test]
fn every_dialect_and_cipher_establishes_a_session_that_seals_everything() {
    for config in [
        PeerConfig::new(Dialect::Smb300),
        PeerConfig::new(Dialect::Smb302),
        config(Dialect::Smb311, Cipher::Aes128Ccm, None),
        config(
            Dialect::Smb311,
            Cipher::Aes128Gcm,
            Some(SigningAlgorithm::AesCmac),
        ),
        config(
            Dialect::Smb311,
            Cipher::Aes256Ccm,
            Some(SigningAlgorithm::AesGmac),
        ),
        config(
            Dialect::Smb311,
            Cipher::Aes256Gcm,
            Some(SigningAlgorithm::AesGmac),
        ),
    ] {
        let (mut client, mut peer, tree) = connected(&config);
        assert_eq!(tree.share.share_type, ShareType::Disk);
        assert_eq!(
            client.negotiated().map(|n| (n.dialect, n.server_guid)),
            Some((config.dialect, crate::peer::SERVER_GUID))
        );
        assert_eq!(
            peer.validations(),
            usize::from(config.dialect != Dialect::Smb311),
            "{config:?}"
        );

        let tickets = client.send(tree.id, &[ECHO]).expect("sends");
        let output = client.take_output();
        for frame in frames(&output) {
            assert_eq!(frame[..4], TRANSFORM_PROTOCOL_ID, "everything is sealed");
        }
        peer.receive(&output).expect("the server opens it");
        client.receive(&peer.take_output()).expect("receives");
        let answered = events(&mut client);
        assert_eq!(answered.len(), 1);
        assert_eq!(status_of(&answered[0], tickets[0]), NtStatus::SUCCESS);
    }
}

#[test]
fn a_wrong_password_is_rejected_and_another_attempt_may_follow() {
    let config = PeerConfig::new(Dialect::Smb311);
    let (mut client, mut peer) = negotiated(&config);
    client
        .authenticate(ntlm(&config, "Tr0ub4dor&3"), SEED)
        .expect("starts");
    pump(&mut client, &mut peer).expect("a refusal is an answer");
    assert_eq!(
        events(&mut client),
        [Event::Rejected(NtStatus::LOGON_FAILURE)]
    );
    assert_eq!(client.failure(), None);
    assert_eq!(
        client.connect_tree(DISK_SHARE),
        Err(SmbError::Protocol),
        "no session"
    );

    client
        .authenticate(ntlm(&config, config.password), SEED)
        .expect("starts again");
    pump(&mut client, &mut peer).expect("sets up");
    let session_id = peer.session_id().expect("keyed");
    assert_eq!(events(&mut client), [Event::Established { session_id }]);
}

#[test]
fn an_unprotected_forged_or_mismatched_setup_ends_the_connection() {
    for (fault, failure) in [
        (Fault::Guest, SmbError::Refused(Refusal::Guest)),
        (Fault::UnsignedFinal, SmbError::Integrity),
        (Fault::ForgedFinal, SmbError::Integrity),
        (Fault::ForgedMechListMic, SmbError::Integrity),
        (Fault::EarlySuccess, SmbError::Protocol),
        (Fault::OtherMechanism, SmbError::Refused(Refusal::Mechanism)),
        (Fault::WeakChallenge, SmbError::Refused(Refusal::Ntlm)),
        (Fault::OtherSession, SmbError::Protocol),
    ] {
        for dialect in [Dialect::Smb302, Dialect::Smb311] {
            let config = PeerConfig {
                fault,
                ..PeerConfig::new(dialect)
            };
            let (mut client, mut peer) = negotiated(&config);
            client
                .authenticate(ntlm(&config, config.password), SEED)
                .expect("starts");
            assert_eq!(
                pump(&mut client, &mut peer),
                Err(failure),
                "{fault:?} {dialect:?}"
            );
            assert!(events(&mut client).is_empty(), "{fault:?}");
            assert_eq!(client.failure(), Some(failure));
            assert_eq!(client.connect_tree(DISK_SHARE), Err(failure));
            assert_eq!(client.receive(&[]), Err(failure));
        }
    }
}

#[test]
fn a_server_sending_no_mechlistmic_or_no_hints_is_still_authenticated() {
    for config in [
        PeerConfig {
            fault: Fault::NoMechListMic,
            ..PeerConfig::new(Dialect::Smb311)
        },
        PeerConfig {
            hints: Hints::Empty,
            ..PeerConfig::new(Dialect::Smb300)
        },
        PeerConfig {
            timestamp: None,
            ..PeerConfig::new(Dialect::Smb311)
        },
    ] {
        established(&config);
    }
}

#[test]
fn a_server_offering_only_kerberos_is_refused_before_anything_is_sent() {
    let config = PeerConfig {
        hints: Hints::Kerberos,
        ..PeerConfig::new(Dialect::Smb311)
    };
    let (mut client, _peer) = negotiated(&config);
    assert_eq!(
        client.authenticate(ntlm(&config, config.password), SEED),
        Err(SmbError::Refused(Refusal::Mechanism))
    );
    assert!(client.take_output().is_empty());
}

#[test]
fn on_3_0_trees_are_held_until_validation_and_released_in_order() {
    let config = PeerConfig::new(Dialect::Smb302);
    let (mut client, mut peer) = established(&config);
    let disk = client.connect_tree(DISK_SHARE).expect("asks");
    let pipe = client.connect_tree("IPC$").expect("asks");
    peer.receive(&client.take_output()).expect("reads");
    client.receive(&peer.take_output()).expect("both connect");
    assert!(events(&mut client).is_empty(), "held");
    assert_eq!(
        client.send(1, &[ECHO]),
        Err(SmbError::Protocol),
        "a held tree"
    );
    assert_eq!(peer.validations(), 0);

    peer.receive(&client.take_output()).expect("reads");
    assert_eq!(peer.validations(), 1);
    client.receive(&peer.take_output()).expect("validated");
    match events(&mut client)[..] {
        [Event::TreeConnected {
            ticket: first,
            tree: disk_tree,
        }, Event::TreeConnected {
            ticket: second,
            tree: pipe_tree,
        }] => {
            assert_eq!((first, second), (disk, pipe));
            assert_eq!(disk_tree.share.share_type, ShareType::Disk);
            assert_eq!(pipe_tree.share.share_type, ShareType::Pipe);
        }
        ref other => panic!("{other:?}"),
    }

    client.connect_tree(DISK_SHARE).expect("asks");
    pump(&mut client, &mut peer).expect("connects");
    assert!(matches!(
        events(&mut client)[..],
        [Event::TreeConnected { .. }]
    ));
    assert_eq!(peer.validations(), 1, "validated once per connection");
}

#[test]
fn a_validation_owed_for_want_of_credit_goes_once_one_frees() {
    let config = PeerConfig {
        grant: 2,
        fault: Fault::WithholdTreeCredit,
        ..PeerConfig::new(Dialect::Smb300)
    };
    let (mut client, mut peer) = established(&config);
    let mut asked = Vec::new();
    while let Ok(ticket) = client.connect_tree(DISK_SHARE) {
        asked.push(ticket);
    }
    assert_eq!(
        client.connect_tree(DISK_SHARE),
        Err(SmbError::NoCredit),
        "every credit is spent"
    );
    assert!(asked.len() >= 2);
    peer.receive(&client.take_output()).expect("reads");
    let answers = peer.take_output();
    let first = PREFIX_LEN + frames(&answers)[0].len();
    client
        .receive(&answers[..first])
        .expect("the first connects");
    assert!(
        client.take_output().is_empty(),
        "no credit to validate with"
    );
    client.receive(&answers[first..]).expect("the rest connect");
    pump(&mut client, &mut peer).expect("validates");
    assert_eq!(peer.validations(), 1);
    let connected: Vec<Ticket> = events(&mut client)
        .into_iter()
        .map(|event| match event {
            Event::TreeConnected { ticket, .. } => ticket,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(connected, asked);
}

#[test]
fn a_validation_that_fails_or_is_refused_ends_the_connection() {
    for (fault, failure) in [
        (Fault::Downgrade, SmbError::Refused(Refusal::Downgrade)),
        (
            Fault::RefuseValidation,
            SmbError::Refused(Refusal::Validation),
        ),
    ] {
        let config = PeerConfig {
            fault,
            ..PeerConfig::new(Dialect::Smb300)
        };
        let (mut client, mut peer) = established(&config);
        client.connect_tree(DISK_SHARE).expect("asks");
        assert_eq!(pump(&mut client, &mut peer), Err(failure));
        assert!(events(&mut client).is_empty(), "no tree is handed out");
    }
}

/// The counter each sealed frame in `output` took its nonce from.
fn counters(output: &[u8]) -> Vec<u64> {
    frames(output)
        .into_iter()
        .filter(|frame| frame.get(..4) == Some(&TRANSFORM_PROTOCOL_ID[..]))
        .map(|frame| {
            let mut counter = [0u8; 8];
            counter.copy_from_slice(&frame[20..28]);
            u64::from_le_bytes(counter)
        })
        .collect()
}

#[test]
fn an_expired_session_is_reported_once_and_renewed_under_its_own_keys() {
    let config = PeerConfig {
        fault: Fault::Expire,
        ..PeerConfig::new(Dialect::Smb311)
    };
    let (mut client, mut peer, tree) = connected(&config);
    let mut written = Vec::new();
    let first = client.send(tree.id, &[ECHO]).expect("sends");
    let second = client.send(tree.id, &[ECHO]).expect("sends");
    pump_into(&mut client, &mut peer, &mut written).expect("answered");
    let answered = events(&mut client);
    assert_eq!(answered.len(), 3, "{answered:?}");
    assert_eq!(answered[0], Event::Expired);
    assert_eq!(
        status_of(&answered[1], first[0]),
        NtStatus::NETWORK_SESSION_EXPIRED
    );
    assert_eq!(
        status_of(&answered[2], second[0]),
        NtStatus::NETWORK_SESSION_EXPIRED
    );

    client
        .reauthenticate(ntlm(&config, config.password))
        .expect("renews");
    assert_eq!(
        client.reauthenticate(ntlm(&config, config.password)),
        Err(SmbError::Protocol),
        "one renewal at a time"
    );
    assert_eq!(
        client.logoff(),
        Err(SmbError::Protocol),
        "not while renewing"
    );
    pump_into(&mut client, &mut peer, &mut written)
        .expect("the server opened every leg under the session's first keys");
    assert_eq!(events(&mut client), [Event::Renewed]);

    let third = client.send(tree.id, &[ECHO]).expect("sends");
    pump_into(&mut client, &mut peer, &mut written).expect("answered");
    let answered = events(&mut client);
    assert_eq!(status_of(&answered[0], third[0]), NtStatus::SUCCESS);

    let counters = counters(&written);
    assert!(counters.len() >= 5);
    assert!(
        counters.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "one nonce sequence, never restarted: {counters:?}"
    );
}

#[test]
fn a_renewal_the_server_refuses_leaves_the_session_expired() {
    let config = PeerConfig {
        fault: Fault::Expire,
        ..PeerConfig::new(Dialect::Smb302)
    };
    let (mut client, mut peer, tree) = connected(&config);
    client.send(tree.id, &[ECHO]).expect("sends");
    pump(&mut client, &mut peer).expect("answered");
    assert_eq!(events(&mut client)[0], Event::Expired);
    client
        .reauthenticate(ntlm(&config, "not it"))
        .expect("renews");
    pump(&mut client, &mut peer).expect("refused");
    assert_eq!(
        events(&mut client),
        [Event::Rejected(NtStatus::LOGON_FAILURE)]
    );
    client.send(tree.id, &[ECHO]).expect("sends");
    pump(&mut client, &mut peer).expect("answered");
    let answered = events(&mut client);
    assert_eq!(answered.len(), 1, "expiry is reported once: {answered:?}");
}

#[test]
fn calls_out_of_turn_are_refused() {
    let config = PeerConfig::new(Dialect::Smb311);
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(offer(config.host)).expect("encodes");
    assert_eq!(
        client.authenticate(ntlm(&config, config.password), SEED),
        Err(SmbError::Protocol),
        "before negotiating"
    );
    pump(&mut client, &mut peer).expect("negotiates");
    events(&mut client);
    for refused in [
        client.connect_tree(DISK_SHARE).map(|_| ()),
        client.send(1, &[ECHO]).map(|_| ()),
        client.logoff(),
        client.disconnect_tree(1),
        client.reauthenticate(ntlm(&config, config.password)),
    ] {
        assert_eq!(refused, Err(SmbError::Protocol), "with no session");
    }
    client
        .authenticate(ntlm(&config, config.password), SEED)
        .expect("starts");
    assert_eq!(
        client.authenticate(ntlm(&config, config.password), SEED),
        Err(SmbError::Protocol),
        "twice"
    );
    pump(&mut client, &mut peer).expect("sets up");
    let ticket = client.connect_tree(DISK_SHARE).expect("asks");
    assert_eq!(
        client.cancel(ticket),
        Err(SmbError::Protocol),
        "the client's own request"
    );
    pump(&mut client, &mut peer).expect("connects");
    let Some(Event::TreeConnected { tree, .. }) = events(&mut client).pop() else {
        panic!("no tree");
    };
    let tree_connect = Request {
        command: Command::TreeConnect,
        ..ECHO
    };
    assert_eq!(
        client.send(tree.id, &[tree_connect]),
        Err(SmbError::Protocol)
    );
    assert_eq!(client.send(tree.id + 1, &[ECHO]), Err(SmbError::Protocol));
    assert_eq!(client.send(tree.id, &[]), Err(SmbError::Protocol));
    assert_eq!(
        client.send(tree.id, &[ECHO; 33]),
        Err(SmbError::TooLarge),
        "past a compound's bound"
    );
    let ticket = client.send(tree.id, &[ECHO]).expect("sends");
    assert_eq!(
        client.logoff(),
        Err(SmbError::Protocol),
        "with a request outstanding"
    );
    client
        .cancel(ticket[0])
        .expect("a caller's request cancels");
}

#[test]
fn share_names_that_could_leave_the_tree_path_are_refused() {
    let (mut client, _peer) = established(&PeerConfig::new(Dialect::Smb311));
    for name in ["", r"a\b", "a/b", "a\0b", "a\nb", "\u{85}"] {
        assert_eq!(
            client.connect_tree(name),
            Err(SmbError::Malformed),
            "{name:?}"
        );
    }
    assert!(client.take_output().is_empty());
}

#[test]
fn a_refused_share_is_reported_and_the_session_goes_on() {
    let config = PeerConfig::new(Dialect::Smb311);
    let (mut client, mut peer) = established(&config);
    let ticket = client.connect_tree("missing").expect("asks");
    pump(&mut client, &mut peer).expect("refused");
    assert_eq!(
        events(&mut client),
        [Event::TreeRefused {
            ticket,
            status: NtStatus::BAD_NETWORK_NAME
        }]
    );
    assert_eq!(client.failure(), None);
}

#[test]
fn a_tree_disconnects_and_the_session_logs_off() {
    let config = PeerConfig::new(Dialect::Smb311);
    let (mut client, mut peer, tree) = connected(&config);
    client.disconnect_tree(tree.id).expect("disconnects");
    assert_eq!(
        client.send(tree.id, &[ECHO]),
        Err(SmbError::Protocol),
        "a disconnecting tree"
    );
    pump(&mut client, &mut peer).expect("answered");
    assert_eq!(
        events(&mut client),
        [Event::TreeDisconnected { tree_id: tree.id }]
    );
    client.logoff().expect("logs off");
    pump(&mut client, &mut peer).expect("answered");
    assert_eq!(events(&mut client), [Event::LoggedOff]);
    assert_eq!(peer.session_id(), None);
    assert_eq!(client.connect_tree(DISK_SHARE), Err(SmbError::Protocol));
    assert_eq!(
        client.authenticate(ntlm(&config, config.password), SEED),
        Err(SmbError::Protocol),
        "a connection serves one session"
    );
}

#[test]
fn trees_are_bounded() {
    let (mut client, mut peer) = established(&PeerConfig::new(Dialect::Smb311));
    for _ in 0..MAX_TREES {
        client.connect_tree(DISK_SHARE).expect("asks");
        pump(&mut client, &mut peer).expect("connects");
    }
    assert_eq!(events(&mut client).len(), MAX_TREES);
    assert_eq!(client.connect_tree(DISK_SHARE), Err(SmbError::TooLarge));
}

#[test]
fn a_server_starving_the_client_of_credit_ends_the_connection() {
    let config = PeerConfig {
        fault: Fault::Starve,
        ..PeerConfig::new(Dialect::Smb311)
    };
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(offer(config.host)).expect("encodes");
    assert_eq!(pump(&mut client, &mut peer), Err(SmbError::Protocol));
}

#[test]
fn a_server_speaking_none_of_the_dialects_offered_says_so() {
    let config = PeerConfig::new(Dialect::Smb302);
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(Offer {
        floor: Dialect::Smb311,
        ..offer(config.host)
    })
    .expect("encodes");
    assert_eq!(
        pump(&mut client, &mut peer),
        Err(SmbError::Refused(Refusal::Dialect))
    );
}

/// Deliver the server's answers with the byte at `at` of the `nth` frame it
/// sends changed, then carry on as [`pump`] does.
fn pump_tampering(
    client: &mut Client,
    peer: &mut Peer,
    nth: usize,
    at: usize,
) -> Result<(), SmbError> {
    let mut seen = 0;
    loop {
        let to_peer = client.take_output();
        peer.receive(&to_peer).expect("the server reads it");
        let mut to_client = peer.take_output();
        if to_peer.is_empty() && to_client.is_empty() {
            return Ok(());
        }
        let mut offset = 0;
        while offset < to_client.len() {
            let len = usize::try_from(u32::from_be_bytes([
                0,
                to_client[offset + 1],
                to_client[offset + 2],
                to_client[offset + 3],
            ]))
            .expect("fits");
            if seen == nth {
                to_client[offset + PREFIX_LEN + at] ^= 0x01;
            }
            seen += 1;
            offset += PREFIX_LEN + len;
        }
        client.receive(&to_client)?;
    }
}

/// Where a header's credit field lies: a byte the client reads nothing
/// security-relevant from, which on 3.1.1 only the pre-auth hash covers.
const CREDITS_AT: usize = 14;

#[test]
fn on_3_1_1_any_change_to_the_negotiation_or_a_setup_leg_breaks_the_keys() {
    let config = PeerConfig::new(Dialect::Smb311);
    let mut peer = Peer::new(config.clone());
    let mut client = Client::new(offer(config.host)).expect("encodes");
    pump_tampering(&mut client, &mut peer, 0, CREDITS_AT).expect("negotiates");
    events(&mut client);
    client
        .authenticate(ntlm(&config, config.password), SEED)
        .expect("starts");
    assert_eq!(
        pump(&mut client, &mut peer),
        Err(SmbError::Integrity),
        "the NEGOTIATE response"
    );

    let (mut client, mut peer) = negotiated(&config);
    client
        .authenticate(ntlm(&config, config.password), SEED)
        .expect("starts");
    assert_eq!(
        pump_tampering(&mut client, &mut peer, 0, CREDITS_AT),
        Err(SmbError::Integrity),
        "the first SESSION_SETUP answer"
    );
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut out = frame::prefix(message.len()).expect("fits").to_vec();
    out.extend_from_slice(message);
    out
}

#[test]
fn a_samba_servers_answers_are_taken_and_its_challenge_answered() {
    let mut client = Client::new(offer("office")).expect("encodes");
    client.take_output();
    client
        .receive(&framed(&vectors::bytes(vectors::QTS_NEGOTIATE_RESPONSE)))
        .expect("negotiates");
    assert_eq!(events(&mut client), [Event::Negotiated]);
    let negotiated = client.negotiated().expect("settled");
    assert_eq!(
        (negotiated.dialect, negotiated.cipher, negotiated.signing),
        (
            Dialect::Smb311,
            Cipher::Aes128Ccm,
            SigningAlgorithm::AesCmac
        )
    );
    assert_eq!(negotiated.server_guid, *b"office\0\0\0\0\0\0\0\0\0\0");

    let credential = NtlmCredential::new("guest-free", "", "unsent").expect("fits");
    let inputs = NtlmInputs {
        client_challenge: [0xcc; 8],
        now: Time64::from_secs(1_790_000_000),
    };
    let ntlm = NtlmInitiator::new(credential, "cifs/office", inputs).expect("fits");
    client
        .authenticate(Mechanism::Ntlm(ntlm), SEED)
        .expect("starts");
    client.take_output();
    client
        .receive(&framed(&vectors::bytes(vectors::QTS_SETUP_RESPONSE)))
        .expect("the challenge is answered");
    let output = client.take_output();
    let answer = frames(&output)[0];
    let header = Header::decode(answer).expect("a header");
    assert_eq!(header.command, Command::SessionSetup);
    assert_eq!(header.session_id, 0x1519_d771, "the session Samba named");
    let request = SessionSetupRequest::decode(answer).expect("decodes");
    let reply = Reply::decode(&request.security_buffer).expect("a negTokenResp");
    assert_eq!(reply.state, Some(NegState::AcceptIncomplete));
    assert_eq!(reply.mic.map(<[u8]>::len), Some(16));
    let authenticate = reply.token.expect("the answer");
    assert_eq!(authenticate[..12], *b"NTLMSSP\0\x03\0\0\0");
}

#[test]
fn an_unsolicited_break_is_delivered() {
    let (mut client, _peer, _tree) = connected(&PeerConfig::new(Dialect::Smb311));
    let mut enc = Encoder::new();
    Header {
        credit_charge: 0,
        status: NtStatus::SUCCESS,
        command: Command::OplockBreak,
        credits: 0,
        flags: Flags::SERVER_TO_REDIR,
        next_command: 0,
        message_id: u64::MAX,
        address: Address::Sync { tree_id: 0 },
        session_id: 0,
        signature: [0; 16],
    }
    .encode(&mut enc);
    enc.bytes(&[0x18, 0, 1, 0, 0, 0, 0, 0]);
    let notice = enc.finish().expect("encodes");
    let mut framed = frame::prefix(notice.len()).expect("fits").to_vec();
    framed.extend_from_slice(&notice);
    client.receive(&framed).expect("receives");
    match &events(&mut client)[..] {
        [Event::Break { message }] => assert_eq!(**message, notice[..]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failure_drops_the_sessions_keys() {
    let (mut client, mut peer, tree) = connected(&PeerConfig::new(Dialect::Smb311));
    client.send(tree.id, &[ECHO]).expect("sends");
    peer.receive(&client.take_output()).expect("reads");
    let mut answer = peer.take_output();
    answer[PREFIX_LEN + TRANSFORM_HEADER_LEN + 2] ^= 0x01;
    assert_eq!(client.receive(&answer), Err(SmbError::Integrity));
    assert_eq!(client.send(tree.id, &[ECHO]), Err(SmbError::Integrity));
    assert!(matches!(client.session, super::Session::None));
}

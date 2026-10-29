//! Unit tests for the NTLMv2 initiator, held to MS-NLMP §4.2.4's worked
//! example and to a Windows exchange captured with its session key, and
//! answered by the test server's own reading of the protocol.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_crypto::ntlm::{ntlm_checksum, ntlm_hmac, ntlm_signing_key, NtlmDirection};

use super::{
    av, flag, message_integrity, ntlmv2_response, upper_simple, Challenge, NtlmCredential,
    NtlmInitiator, NtlmInputs, ACCEPTED, MAX_NAME_BYTES, MIC, NEGOTIATE_FLAGS, SIGNATURE_LEN,
    VERSION,
};
use crate::peer::{challenge_message, CHALLENGE_FLAGS};
use crate::vectors::{bytes, qts_tokens, windows_ntlm, windows_session_key};
use crate::wire::{filetime_floor, from_utf16le, Encoder, Reader};
use crate::{Refusal, SmbError};

const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

const INPUTS: NtlmInputs = NtlmInputs {
    client_challenge: [0xaa; 8],
    now: Time64::from_secs(1_790_000_000),
};

/// MS-NLMP §4.2.4's server target information: `Domain`, `Server`.
fn example_target_info() -> Vec<u8> {
    bytes(concat!(
        "02000c0044006f006d00610069006e00",
        "01000c00530065007200760065007200",
        "00000000",
    ))
}

#[test]
fn a_credential_keyed_by_its_derived_key_is_the_one_its_password_makes() {
    let typed = NtlmCredential::new("User", "Domain", "Password").expect("fits");
    let keyed = NtlmCredential::keyed("User", "Domain", typed.response_key()).expect("fits");
    assert!(keyed.matches(&typed));
    assert_eq!((keyed.user(), keyed.domain()), ("User", "Domain"));
    for other in [
        NtlmCredential::new("User", "Domain", "password").expect("fits"),
        NtlmCredential::keyed("USER", "Domain", typed.response_key()).expect("fits"),
        NtlmCredential::keyed("User", "DOMAIN", typed.response_key()).expect("fits"),
    ] {
        assert!(!other.matches(&typed), "{other:?}");
    }
    assert_eq!(
        NtlmCredential::keyed("", "Domain", typed.response_key()).map(|_| ()),
        Err(SmbError::Refused(Refusal::Guest))
    );
    let long = "u".repeat(MAX_NAME_BYTES);
    assert_eq!(
        NtlmCredential::keyed(&long, "", typed.response_key()).map(|_| ()),
        Err(SmbError::TooLarge)
    );
}

#[test]
fn the_ntlmv2_response_matches_the_worked_example() {
    let credential = NtlmCredential::new("User", "Domain", "Password").expect("fits");
    assert_eq!(
        credential.response_key(),
        &<[u8; 16]>::try_from(bytes("0c868a403bfd7a93a3001ef22ef02e3f")).expect("16")
    );
    let (response, session) = ntlmv2_response(
        credential.response_key(),
        SERVER_CHALLENGE,
        [0; 8],
        [0xaa; 8],
        &example_target_info(),
    )
    .expect("encodes");
    assert_eq!(
        response[..16],
        bytes("68cd0ab851e51c96aabc927bebef6a1c")[..]
    );
    assert_eq!(session[..], bytes("8de40ccadbc14a82f15cb0ad0de95ca3")[..]);
    let mut temp = bytes("01010000000000000000000000000000aaaaaaaaaaaaaaaa00000000");
    temp.extend_from_slice(&example_target_info());
    temp.extend_from_slice(&[0; 4]);
    assert_eq!(response[16..], temp[..]);
}

#[test]
fn a_windows_challenge_decodes() {
    let windows = windows_ntlm();
    let challenge = Challenge::decode(&windows.challenge).expect("decodes");
    assert_eq!(challenge.flags, 0xe28a_8215);
    assert_eq!(
        challenge.server_challenge[..],
        bytes("5fc0cb7f886e93d6")[..]
    );
    let ids: Vec<u16> = challenge.pairs.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [2, 1, 4, 3, 7]);
    assert_eq!(
        from_utf16le(challenge.pairs[0].1).expect("UTF-16"),
        "SUT311"
    );
    assert_eq!(
        challenge.timestamp.map(|stamp| stamp.to_vec()),
        Some(bytes("248d5c6ccdaed001"))
    );
    assert_eq!(challenge.av_flags, None);
}

#[test]
fn a_samba_challenge_decodes_and_agrees_to_what_was_asked() {
    let (_, challenge) = qts_tokens();
    let decoded = Challenge::decode(&challenge).expect("decodes");
    assert_eq!(decoded.flags, 0x228a_8215);
    assert_eq!(decoded.flags & 0x4000_0000, 0, "no key exchange, as asked");
    let ids: Vec<u16> = decoded.pairs.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [2, 1, 4, 3, 7]);
    assert_eq!(from_utf16le(decoded.pairs[3].1).expect("UTF-16"), "office");
    assert!(decoded.timestamp.is_some());

    let mut ntlm = initiator();
    ntlm.negotiate().expect("encodes");
    let authenticate = ntlm.authenticate(&challenge).expect("answers");
    let flags = u32::from_le_bytes(authenticate[60..64].try_into().expect("4"));
    assert_eq!(flags, 0x228a_8215 & ACCEPTED);
}

#[test]
fn the_mic_is_made_as_windows_makes_it() {
    let windows = windows_ntlm();
    let mic = message_integrity(
        &windows_session_key(),
        &windows.negotiate,
        &windows.challenge,
        &windows.authenticate,
    )
    .expect("long enough");
    assert_eq!(mic[..], windows.authenticate[MIC]);
}

/// The `(len, max, offset)` field `index` of an `AUTHENTICATE_MESSAGE`.
fn field(message: &[u8], index: usize) -> &[u8] {
    let mut r = Reader::at(message, 12 + 8 * index).expect("in the message");
    let len = usize::from(r.u16().expect("len"));
    assert_eq!(usize::from(r.u16().expect("max")), len);
    let offset = usize::try_from(r.u32().expect("offset")).expect("fits");
    &message[offset..offset + len]
}

fn initiator() -> NtlmInitiator {
    let credential = NtlmCredential::new("alice", "WORKGROUP", "hunter2").expect("fits");
    NtlmInitiator::new(credential, "cifs/SUT311", INPUTS).expect("fits")
}

#[test]
fn the_answer_binds_the_servers_target_information_the_service_and_the_mic() {
    let windows = windows_ntlm();
    let mut ntlm = initiator();
    let negotiate = ntlm.negotiate().expect("encodes");
    let authenticate = ntlm.authenticate(&windows.challenge).expect("answers");

    assert_eq!(field(&authenticate, 0), [0; 24], "no LMv2 response");
    assert_eq!(
        from_utf16le(field(&authenticate, 2)).expect("UTF-16"),
        "WORKGROUP"
    );
    assert_eq!(
        from_utf16le(field(&authenticate, 3)).expect("UTF-16"),
        "alice"
    );
    assert!(field(&authenticate, 4).is_empty(), "no workstation");
    assert!(field(&authenticate, 5).is_empty(), "no exchanged key");
    let flags = u32::from_le_bytes(authenticate[60..64].try_into().expect("4"));
    assert_eq!(flags, 0xe28a_8215 & ACCEPTED);
    assert_eq!(authenticate[64..72], VERSION);

    let nt = field(&authenticate, 1);
    let (proof, blob) = nt.split_at(16);
    assert_eq!(blob[..2], [1, 1]);
    assert_eq!(
        blob[8..16],
        bytes("248d5c6ccdaed001")[..],
        "the server's time"
    );
    assert_eq!(blob[16..24], INPUTS.client_challenge);
    let mut expected = Encoder::new();
    let challenge = Challenge::decode(&windows.challenge).expect("decodes");
    for (id, value) in &challenge.pairs {
        expected.u16(*id);
        expected.u16(u16::try_from(value.len()).expect("fits"));
        expected.bytes(value);
    }
    expected.bytes(&[6, 0, 4, 0, 2, 0, 0, 0]);
    expected.bytes(&[10, 0, 16, 0]);
    expected.zeros(16);
    expected.bytes(&[9, 0, 22, 0]);
    crate::wire::utf16le("cifs/SUT311", &mut expected);
    expected.zeros(4);
    let expected = expected.finish().expect("encodes");
    assert_eq!(blob[28..blob.len() - 4], expected[..]);
    assert_eq!(blob[blob.len() - 4..], [0; 4]);

    let credential = NtlmCredential::new("alice", "WORKGROUP", "hunter2").expect("fits");
    let key = credential.response_key();
    assert_eq!(
        ntlm_hmac(key, &[&challenge.server_challenge, blob])[..],
        proof[..]
    );
    let session = ntlm_hmac(key, &[proof]);
    assert_eq!(ntlm.session_key(), Some(&session));
    assert_eq!(
        message_integrity(&session, &negotiate, &windows.challenge, &authenticate)
            .expect("long enough")[..],
        authenticate[MIC]
    );
    assert_eq!(ntlm.credential.response_key, [0; 16], "wiped once answered");
}

#[test]
fn the_negotiate_message_asks_for_ntlmv2_and_no_key_exchange() {
    let negotiate = initiator().negotiate().expect("encodes");
    assert_eq!(negotiate.len(), 40);
    assert_eq!(negotiate[..12], *b"NTLMSSP\0\x01\0\0\0");
    let flags = u32::from_le_bytes(negotiate[12..16].try_into().expect("4"));
    assert_eq!(flags, NEGOTIATE_FLAGS);
    for withheld in [0x4000_0000, 0x0000_0080, 0x0000_0020, 0x0000_0002] {
        assert_eq!(flags & withheld, 0, "{withheld:#x}");
    }
    assert_eq!(negotiate[32..], VERSION);
}

/// The target information a test challenge carries: a domain, and the time
/// unless `timestamp` is `None`.
fn target_info(timestamp: Option<u64>, extra: &[u8]) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.bytes(&[2, 0, 4, 0]);
    crate::wire::utf16le("WG", &mut enc);
    if let Some(stamp) = timestamp {
        enc.bytes(&[7, 0, 8, 0]);
        enc.u64(stamp);
    }
    enc.bytes(extra);
    enc.bytes(&[0, 0, 0, 0]);
    enc.finish().expect("encodes")
}

#[test]
fn without_the_servers_time_the_hosts_is_answered_with() {
    let challenge = challenge_message(
        CHALLENGE_FLAGS,
        SERVER_CHALLENGE,
        "WG",
        &target_info(None, &[]),
    )
    .expect("encodes");
    let mut ntlm = initiator();
    ntlm.negotiate().expect("encodes");
    let authenticate = ntlm.authenticate(&challenge).expect("answers");
    let blob = &field(&authenticate, 1)[16..];
    assert_eq!(
        blob[8..16],
        filetime_floor(INPUTS.now).expect("in range").to_le_bytes()
    );

    let mut ntlm = NtlmInitiator::new(
        NtlmCredential::new("alice", "WG", "pw").expect("fits"),
        "cifs/h",
        NtlmInputs {
            now: Time64::from_secs(-12_000_000_000),
            ..INPUTS
        },
    )
    .expect("fits");
    ntlm.negotiate().expect("encodes");
    assert_eq!(
        ntlm.authenticate(&challenge),
        Err(SmbError::TimeOutOfRange),
        "before 1601"
    );
}

#[test]
fn a_server_withholding_ntlmv2s_protections_is_refused() {
    for withheld in [
        flag::UNICODE,
        flag::SIGN,
        flag::NTLM,
        flag::EXTENDED_SESSION_SECURITY,
        flag::TARGET_INFO,
        flag::KEY_128,
    ] {
        let challenge = challenge_message(
            CHALLENGE_FLAGS & !withheld,
            SERVER_CHALLENGE,
            "WG",
            &target_info(Some(1), &[]),
        )
        .expect("encodes");
        let mut ntlm = initiator();
        ntlm.negotiate().expect("encodes");
        assert_eq!(
            ntlm.authenticate(&challenge),
            Err(SmbError::Refused(Refusal::Ntlm)),
            "{withheld:#x}"
        );
        assert_eq!(ntlm.session_key(), None);
    }
}

#[test]
fn the_servers_flags_and_the_clients_own_pairs_are_answered_by_the_client() {
    // A server's MsvAvFlags is kept and the MIC announced beside it; pairs
    // that only a client may send are dropped from what the server sent.
    let extra = [
        &[6u8, 0, 4, 0, 1, 0, 0, 0][..],
        &[9, 0, 2, 0, b'x', 0],
        &[10, 0, 16, 0],
        &[0xee; 16],
    ]
    .concat();
    let challenge = challenge_message(
        CHALLENGE_FLAGS,
        SERVER_CHALLENGE,
        "WG",
        &target_info(Some(1), &extra),
    )
    .expect("encodes");
    let mut ntlm = initiator();
    ntlm.negotiate().expect("encodes");
    let authenticate = ntlm.authenticate(&challenge).expect("answers");
    let blob = &field(&authenticate, 1)[16..];
    let pairs = super::av_pairs(&blob[28..blob.len() - 4]).expect("well formed");
    let only = |id: u16| {
        let found: Vec<&[u8]> = pairs
            .iter()
            .filter(|(pair, _)| *pair == id)
            .map(|(_, value)| *value)
            .collect();
        assert_eq!(found.len(), 1, "{id}");
        found[0]
    };
    assert_eq!(only(av::FLAGS), [3, 0, 0, 0]);
    assert_eq!(only(av::CHANNEL_BINDINGS), [0; 16]);
    assert_eq!(
        from_utf16le(only(av::TARGET_NAME)).expect("UTF-16"),
        "cifs/SUT311"
    );
}

/// A test challenge whose target information is `info`, verbatim.
fn with_info(info: &[u8]) -> Vec<u8> {
    challenge_message(CHALLENGE_FLAGS, SERVER_CHALLENGE, "WG", info).expect("encodes")
}

#[test]
fn a_malformed_challenge_is_refused() {
    let good = with_info(&target_info(Some(1), &[]));
    assert!(Challenge::decode(&good).is_ok());

    let mut cases: Vec<(Vec<u8>, SmbError)> = Vec::new();
    let mut signature = good.clone();
    signature[0] ^= 1;
    cases.push((signature, SmbError::Malformed));
    let mut kind = good.clone();
    kind[8] = 3;
    cases.push((kind, SmbError::Malformed));
    cases.push((good[..47].to_vec(), SmbError::Malformed));
    let mut outside = good.clone();
    outside[44..48].copy_from_slice(&4096u32.to_le_bytes());
    cases.push((outside, SmbError::Malformed));
    for (info, err) in [
        (vec![2, 0, 0, 0], SmbError::Malformed),
        (vec![0, 0, 1, 0, 0], SmbError::Malformed),
        (vec![0, 0, 0, 0, 0, 0], SmbError::Malformed),
        (
            target_info(Some(1), &[7, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            SmbError::Malformed,
        ),
        (
            target_info(None, &[7, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0]),
            SmbError::Malformed,
        ),
        (
            target_info(None, &[6, 0, 4, 0, 0, 0, 0, 0, 6, 0, 4, 0, 0, 0, 0, 0]),
            SmbError::Malformed,
        ),
        (vec![0; 4100], SmbError::TooLarge),
        (Vec::new(), SmbError::Malformed),
    ] {
        cases.push((with_info(&info), err));
    }
    for (message, err) in cases {
        assert_eq!(Challenge::decode(&message), Err(err), "{message:02x?}");
        let mut ntlm = initiator();
        ntlm.negotiate().expect("encodes");
        assert_eq!(ntlm.authenticate(&message).map(|_| ()), Err(err));
        assert_ne!(
            ntlm.credential.response_key, [0; 16],
            "nothing is spent on a refused challenge"
        );
    }
}

/// An NTLM signature over `data` at `sequence` under `key`.
fn signed(key: &[u8; 16], sequence: u32, data: &[u8]) -> [u8; SIGNATURE_LEN] {
    let mut out = [0u8; SIGNATURE_LEN];
    out[..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..12].copy_from_slice(&ntlm_checksum(key, sequence, data));
    out[12..].copy_from_slice(&sequence.to_le_bytes());
    out
}

#[test]
fn each_direction_signs_under_its_own_key_and_sequence() {
    let windows = windows_ntlm();
    let mut ntlm = initiator();
    ntlm.negotiate().expect("encodes");
    ntlm.authenticate(&windows.challenge).expect("answers");
    let session = *ntlm.session_key().expect("established");
    let client_key = ntlm_signing_key(&session, NtlmDirection::ClientToServer);
    let server_key = ntlm_signing_key(&session, NtlmDirection::ServerToClient);

    assert_eq!(ntlm.sign(b"one"), Ok(signed(&client_key, 0, b"one")));
    assert_eq!(ntlm.sign(b"two"), Ok(signed(&client_key, 1, b"two")));

    let first = signed(&server_key, 0, b"three");
    assert_eq!(
        ntlm.verify(b"three", &signed(&client_key, 0, b"three")),
        Err(SmbError::Integrity),
        "the other direction's key"
    );
    assert_eq!(
        ntlm.verify(b"three", &first[..15]),
        Err(SmbError::Integrity)
    );
    let mut forged = first;
    forged[6] ^= 1;
    assert_eq!(ntlm.verify(b"three", &forged), Err(SmbError::Integrity));
    assert_eq!(ntlm.verify(b"three", &first), Ok(()));
    assert_eq!(
        ntlm.verify(b"three", &first),
        Err(SmbError::Integrity),
        "a replay names a spent sequence number"
    );
    assert_eq!(
        ntlm.verify(b"four", &signed(&server_key, 1, b"four")),
        Ok(())
    );
}

#[test]
fn calls_out_of_turn_are_refused() {
    let windows = windows_ntlm();
    let mut ntlm = initiator();
    assert_eq!(
        ntlm.authenticate(&windows.challenge).map(|_| ()),
        Err(SmbError::Protocol),
        "before negotiating"
    );
    assert_eq!(ntlm.sign(b"m").map(|_| ()), Err(SmbError::Protocol));
    assert_eq!(ntlm.verify(b"m", &[0; 16]), Err(SmbError::Protocol));
    ntlm.negotiate().expect("encodes");
    assert_eq!(ntlm.negotiate().map(|_| ()), Err(SmbError::Protocol));
    ntlm.authenticate(&windows.challenge).expect("answers");
    assert_eq!(
        ntlm.authenticate(&windows.challenge).map(|_| ()),
        Err(SmbError::Protocol),
        "twice"
    );
}

#[test]
fn the_user_name_is_upper_cased_one_unit_for_one_as_windows_does() {
    for (lower, upper) in [
        ('a', 'A'),
        ('é', 'É'),
        ('ı', 'I'),
        ('ǆ', 'Ǆ'),
        ('ß', 'ß'),
        ('ﬀ', 'ﬀ'),
        ('ŉ', 'ŉ'),
        ('\u{1f80}', '\u{1f88}'),
        ('\u{1fa7}', '\u{1faf}'),
        ('\u{1fb3}', '\u{1fbc}'),
        ('\u{1fc3}', '\u{1fcc}'),
        ('\u{1ff3}', '\u{1ffc}'),
        ('\u{1fb2}', '\u{1fb2}'),
        ('\u{10428}', '\u{10428}'),
        ('A', 'A'),
        ('7', '7'),
    ] {
        assert_eq!(upper_simple(lower), upper, "{lower:?}");
    }
    let key = |user: &str, domain: &str| {
        *NtlmCredential::new(user, domain, "pw")
            .expect("fits")
            .response_key()
    };
    assert_eq!(key("alice", "WG"), key("ALICE", "WG"));
    assert_ne!(
        key("alice", "WG"),
        key("alice", "wg"),
        "the domain keeps its case"
    );
}

#[test]
fn an_empty_or_oversized_name_is_refused() {
    assert_eq!(
        NtlmCredential::new("", "WG", "pw").map(|_| ()),
        Err(SmbError::Refused(Refusal::Guest)),
        "an anonymous logon"
    );
    let long = "x".repeat(257);
    assert_eq!(
        NtlmCredential::new(&long, "WG", "pw").map(|_| ()),
        Err(SmbError::TooLarge)
    );
    assert_eq!(
        NtlmCredential::new("alice", &long, "pw").map(|_| ()),
        Err(SmbError::TooLarge)
    );
    let credential = NtlmCredential::new("alice", "WG", "pw").expect("fits");
    assert_eq!(
        NtlmInitiator::new(credential, &long, INPUTS).map(|_| ()),
        Err(SmbError::TooLarge)
    );
}

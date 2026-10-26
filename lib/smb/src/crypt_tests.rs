//! Unit tests for SMB 3's key derivation, signing, encryption transform and
//! nonces, held to Microsoft's published Windows traffic wherever it exists.
//! The AES-256 derivation, which no published Windows trace covers, is held to
//! `cryptography`'s `KBKDFHMAC`, an implementation independent of this one.

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    derive_keys, KeyInputs, Nonces, PreauthHash, SessionKeys, Transform, NONCE_SEED_LEN,
    TRANSFORM_HEADER_LEN,
};
use crate::header::{Address, Command, Flags, Header};
use crate::negotiate::{Cipher, Dialect, SigningAlgorithm};
use crate::status::NtStatus;
use crate::vectors::{self, array, bytes, Capture};
use crate::wire::Encoder;
use crate::{Role, SmbError};

const SEED: [u8; NONCE_SEED_LEN] = [
    0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xa1, 0xb2, 0xc3, 0xd4,
];

const WINDOWS: [&Capture; 3] = [&vectors::SMB300, &vectors::SMB311_GCM, &vectors::SMB311_CCM];

fn preauth_of(session: &Capture) -> PreauthHash {
    if session.preauth.is_empty() {
        PreauthHash::new()
    } else {
        PreauthHash(array(session.preauth))
    }
}

fn windows_keys(session: &Capture, role: Role) -> SessionKeys {
    let key = bytes(session.session_key);
    let preauth = preauth_of(session);
    let inputs = KeyInputs {
        dialect: session.dialect,
        cipher: session.cipher,
        signing: SigningAlgorithm::AesCmac,
        session_key: &key,
        preauth: &preauth,
    };
    SessionKeys::derive(&inputs, role, &SEED).expect("derives")
}

/// `plaintext` sealed for `session_id`.
fn sealed(keys: &mut SessionKeys, session_id: u64, plaintext: &[u8]) -> Vec<u8> {
    let mut frame = vec![0; TRANSFORM_HEADER_LEN];
    frame.extend_from_slice(plaintext);
    keys.seal(session_id, &mut frame).expect("seals");
    frame
}

#[test]
fn the_preauth_hash_chains_to_the_value_windows_reached() {
    let mut hash = PreauthHash::new();
    assert_eq!(hash.value(), &[0; 64]);
    for (message, after) in vectors::SMB311_GCM_PREAUTH {
        hash.update(&bytes(message));
        assert_eq!(hash.value(), &array::<64>(after));
    }
    assert_eq!(hash, preauth_of(&vectors::SMB311_GCM));
}

#[test]
fn keys_derive_as_windows_derived_them() {
    for session in WINDOWS {
        let key = bytes(session.session_key);
        let preauth = preauth_of(session);
        let keys = derive_keys(&KeyInputs {
            dialect: session.dialect,
            cipher: session.cipher,
            signing: SigningAlgorithm::AesCmac,
            session_key: &key,
            preauth: &preauth,
        })
        .expect("derives");
        assert_eq!(keys.signing, array(session.signing_key));
        assert_eq!(keys.c2s[..16], array::<16>(session.client_to_server));
        assert_eq!(keys.s2c[..16], array::<16>(session.server_to_client));
        assert_eq!(keys.application, array(session.application_key));
        assert_eq!(
            keys.c2s[16..],
            [0; 16],
            "a 128-bit cipher keys from 16 bytes"
        );
    }
}

#[test]
fn an_aes_256_cipher_keys_from_the_whole_session_key() {
    let mut preauth = PreauthHash::new();
    preauth.update(b"abc");
    let wide: Vec<u8> = (0u8..32).collect();
    let keys = derive_keys(&KeyInputs {
        dialect: Dialect::Smb311,
        cipher: Cipher::Aes256Gcm,
        signing: SigningAlgorithm::AesCmac,
        session_key: &wide,
        preauth: &preauth,
    })
    .expect("derives");
    assert_eq!(
        keys.c2s,
        array::<32>("cb79a4663962b3abef85c63970288e0bd3fde3050a4ef688b83d27d3db74e315")
    );
    assert_eq!(keys.signing, array("e96f10b11459c34ed9d4c97642e66e27"));
}

#[test]
fn an_empty_session_key_is_refused() {
    let preauth = PreauthHash::new();
    let inputs = KeyInputs {
        dialect: Dialect::Smb300,
        cipher: Cipher::Aes128Ccm,
        signing: SigningAlgorithm::AesCmac,
        session_key: &[],
        preauth: &preauth,
    };
    assert_eq!(
        SessionKeys::derive(&inputs, Role::Client, &SEED).map(|_| ()),
        Err(SmbError::Protocol)
    );
}

#[test]
fn windows_traffic_opens_under_the_derived_keys() {
    for session in WINDOWS {
        let client = windows_keys(session, Role::Client);
        let server = windows_keys(session, Role::Server);
        for exchange in [&session.write, &session.read] {
            let mut frame = bytes(exchange.sealed_request);
            assert_eq!(server.open(&mut frame), Ok(&bytes(exchange.request)[..]));
            let mut frame = bytes(exchange.sealed_response);
            assert_eq!(client.open(&mut frame), Ok(&bytes(exchange.response)[..]));
            // A key opens only what the other direction sealed.
            let mut frame = bytes(exchange.sealed_request);
            assert_eq!(client.open(&mut frame), Err(SmbError::Integrity));
        }
    }
}

#[test]
fn windows_signed_setup_responses_verify() {
    for (session, signed) in [
        (&vectors::SMB311_GCM, vectors::SMB311_GCM_FINAL_SETUP),
        (&vectors::SMB311_CCM, vectors::SMB311_CCM_FINAL_SETUP),
    ] {
        let client = windows_keys(session, Role::Client);
        let message = bytes(signed);
        assert_eq!(client.verify(&message), Ok(()));
        let mut tampered = message;
        *tampered.last_mut().expect("a body") ^= 1;
        assert_eq!(client.verify(&tampered), Err(SmbError::Integrity));
    }
}

fn pair(dialect: Dialect, cipher: Cipher, signing: SigningAlgorithm) -> (SessionKeys, SessionKeys) {
    let mut preauth = PreauthHash::new();
    preauth.update(b"abc");
    let key: Vec<u8> = (0u8..32).collect();
    let inputs = KeyInputs {
        dialect,
        cipher,
        signing,
        session_key: &key,
        preauth: &preauth,
    };
    (
        SessionKeys::derive(&inputs, Role::Client, &SEED).expect("client keys"),
        SessionKeys::derive(&inputs, Role::Server, &SEED).expect("server keys"),
    )
}

#[test]
fn each_cipher_seals_one_way_and_opens_the_other() {
    for (dialect, cipher) in [
        (Dialect::Smb300, Cipher::Aes128Ccm),
        (Dialect::Smb311, Cipher::Aes128Ccm),
        (Dialect::Smb311, Cipher::Aes128Gcm),
        (Dialect::Smb311, Cipher::Aes256Ccm),
        (Dialect::Smb311, Cipher::Aes256Gcm),
    ] {
        let (mut client, server) = pair(dialect, cipher, SigningAlgorithm::AesCmac);
        let plaintext = b"\xfeSMB the rest of a message";
        let mut frame = sealed(&mut client, 0x1234, plaintext);
        assert_eq!(frame.len(), TRANSFORM_HEADER_LEN + plaintext.len());
        assert_ne!(&frame[TRANSFORM_HEADER_LEN..], plaintext, "{cipher:?}");
        assert_eq!(Transform::parse(&frame).map(|t| t.session_id), Ok(0x1234));
        let mut replay = frame.clone();
        assert_eq!(server.open(&mut frame), Ok(&plaintext[..]), "{cipher:?}");
        assert_eq!(
            client.open(&mut replay),
            Err(SmbError::Integrity),
            "{cipher:?}"
        );
    }
}

#[test]
fn nonces_count_from_the_seed_and_carry_its_random_bytes() {
    let (mut gcm, _) = pair(
        Dialect::Smb311,
        Cipher::Aes128Gcm,
        SigningAlgorithm::AesCmac,
    );
    let first = sealed(&mut gcm, 1, b"\xfeSMB");
    let second = sealed(&mut gcm, 1, b"\xfeSMB");
    assert_eq!(
        first[20..36],
        [0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xa1, 0xb2, 0xc3, 0xd4, 0, 0, 0, 0]
    );
    assert_eq!(second[20..28], 0xfedc_ba98_7654_3211_u64.to_le_bytes());
    assert_eq!(second[28..36], first[28..36]);

    let (mut ccm, _) = pair(
        Dialect::Smb300,
        Cipher::Aes128Ccm,
        SigningAlgorithm::AesCmac,
    );
    let frame = sealed(&mut ccm, 1, b"\xfeSMB");
    assert_eq!(
        frame[28..36],
        [0xa1, 0xb2, 0xc3, 0, 0, 0, 0, 0],
        "CCM's nonce is eleven bytes"
    );

    let mut seen = BTreeSet::new();
    for _ in 0..1000 {
        assert!(seen.insert(sealed(&mut gcm, 1, b"\xfeSMB")[20..36].to_vec()));
    }
}

#[test]
fn a_counter_starting_near_its_top_wraps_and_a_spent_sequence_ends() {
    let mut top = SEED;
    top[..8].fill(0xff);
    let mut nonces = Nonces::new(&top, 12);
    assert_eq!(nonces.next().map(|n| n[..8].to_vec()), Ok(vec![0xff; 8]));
    assert_eq!(nonces.next().map(|n| n[..8].to_vec()), Ok(vec![0; 8]));

    nonces.used = u64::MAX;
    assert_eq!(nonces.next(), Err(SmbError::TooLarge));
    assert_eq!(nonces.next(), Err(SmbError::TooLarge), "and stays ended");
}

#[test]
fn a_tampered_transform_never_opens() {
    let (mut client, server) = pair(
        Dialect::Smb311,
        Cipher::Aes128Gcm,
        SigningAlgorithm::AesCmac,
    );
    let frame = sealed(&mut client, 9, b"\xfeSMB payload");
    // The tag, the nonce, the session id and the ciphertext are all bound.
    for at in [10, 25, 48, 60] {
        let mut tampered = frame.clone();
        tampered[at] ^= 1;
        assert!(server.open(&mut tampered).is_err(), "byte {at}");
    }
}

#[test]
fn a_malformed_transform_is_refused() {
    let (mut client, _) = pair(
        Dialect::Smb300,
        Cipher::Aes128Ccm,
        SigningAlgorithm::AesCmac,
    );
    let frame = sealed(&mut client, 9, b"\xfeSMB payload");
    let mut short = frame.clone();
    short.pop();
    assert_eq!(Transform::parse(&short), Err(SmbError::Malformed));
    let mut bad_flags = frame.clone();
    bad_flags[42] = 2;
    assert_eq!(Transform::parse(&bad_flags), Err(SmbError::Malformed));
    assert_eq!(
        Transform::parse(&frame[..TRANSFORM_HEADER_LEN]),
        Err(SmbError::Malformed)
    );
    assert_eq!(Transform::parse(&frame[..20]), Err(SmbError::Malformed));
}

#[test]
fn nothing_to_seal_is_refused() {
    let (mut client, _) = pair(
        Dialect::Smb311,
        Cipher::Aes128Gcm,
        SigningAlgorithm::AesCmac,
    );
    assert_eq!(
        client.seal(1, &mut [0; TRANSFORM_HEADER_LEN]),
        Err(SmbError::Malformed)
    );
    assert_eq!(client.seal(1, &mut [0; 10]), Err(SmbError::Malformed));
}

fn message(command: Command, flags: Flags) -> Vec<u8> {
    let mut enc = Encoder::new();
    Header {
        credit_charge: 1,
        status: NtStatus::SUCCESS,
        command,
        credits: 1,
        flags,
        next_command: 0,
        message_id: 5,
        address: Address::Sync { tree_id: 0 },
        session_id: 0x1234,
        signature: [0; 16],
    }
    .encode(&mut enc);
    enc.bytes(b"a body to sign");
    enc.finish().expect("encodes")
}

#[test]
fn signatures_verify_and_refuse_tampering_for_both_algorithms() {
    for algorithm in [SigningAlgorithm::AesCmac, SigningAlgorithm::AesGmac] {
        let (client, server) = pair(Dialect::Smb311, Cipher::Aes128Gcm, algorithm);
        let mut signed = message(Command::SessionSetup, Flags::SERVER_TO_REDIR);
        server.sign(&mut signed).expect("signs");
        assert!(Header::decode(&signed)
            .expect("header")
            .flags
            .contains(Flags::SIGNED));
        assert_eq!(client.verify(&signed), Ok(()));
        let mut tampered = signed.clone();
        *tampered.last_mut().expect("body") ^= 1;
        assert_eq!(
            client.verify(&tampered),
            Err(SmbError::Integrity),
            "{algorithm:?}"
        );
        let mut moved = signed.clone();
        moved[24] ^= 1;
        assert_eq!(
            client.verify(&moved),
            Err(SmbError::Integrity),
            "{algorithm:?}"
        );
    }
}

#[test]
fn an_unsigned_message_never_verifies() {
    let (client, _) = pair(
        Dialect::Smb300,
        Cipher::Aes128Ccm,
        SigningAlgorithm::AesCmac,
    );
    assert_eq!(
        client.verify(&message(Command::SessionSetup, Flags::SERVER_TO_REDIR)),
        Err(SmbError::Integrity)
    );
}

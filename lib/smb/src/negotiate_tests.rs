//! Unit tests for `NEGOTIATE`, its contexts, the client's acceptance policy,
//! and validate-negotiate.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    check_validate_negotiate, validate_negotiate_input, validate_negotiate_output, Capabilities,
    Cipher, Dialect, NegotiateContext, NegotiateRequest, NegotiateResponse, Offer, SecurityMode,
    SigningAlgorithm, SHA512,
};
use crate::header::{Address, Command, Flags, Header, HEADER_LEN};
use crate::status::NtStatus;
use crate::vectors;
use crate::wire::Encoder;
use crate::{Refusal, SmbError};

fn offer(floor: Dialect) -> Offer {
    Offer {
        floor,
        client_guid: [0x11; 16],
        salt: [0x22; 32],
        server_name: String::from("nas.local"),
    }
}

fn message(body: impl FnOnce(&mut Encoder)) -> Vec<u8> {
    let mut enc = Encoder::new();
    Header {
        credit_charge: 0,
        status: NtStatus::SUCCESS,
        command: Command::Negotiate,
        credits: 1,
        flags: Flags::NONE,
        next_command: 0,
        message_id: 0,
        address: Address::Sync { tree_id: 0 },
        session_id: 0,
        signature: [0; 16],
    }
    .encode(&mut enc);
    body(&mut enc);
    enc.finish().expect("encodes")
}

#[test]
fn a_request_offers_every_dialect_from_the_floor_with_its_contexts() {
    let request = offer(Dialect::Smb300).request();
    assert_eq!(request.dialects, [0x0300, 0x0302, 0x0311]);
    assert!(request
        .security_mode
        .contains(SecurityMode::SIGNING_REQUIRED));
    assert!(request.capabilities.contains(Capabilities::ENCRYPTION));
    let bytes = message(|enc| request.encode(enc));
    let context_offset = u32::from_le_bytes([bytes[92], bytes[93], bytes[94], bytes[95]]);
    assert_eq!(context_offset % 8, 0, "contexts are eight-byte aligned");
    assert_eq!(NegotiateRequest::decode(&bytes), Ok(request));

    assert_eq!(offer(Dialect::Smb302).request().dialects, [0x0302, 0x0311]);
    assert_eq!(offer(Dialect::Smb311).request().dialects, [0x0311]);
}

#[test]
fn a_request_without_3_1_1_carries_no_contexts() {
    let request = NegotiateRequest {
        dialects: vec![0x0300, 0x0302],
        security_mode: SecurityMode::SIGNING_ENABLED,
        capabilities: Capabilities::ENCRYPTION,
        client_guid: [3; 16],
        contexts: Vec::new(),
    };
    let bytes = message(|enc| request.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 36 + 4);
    assert_eq!(NegotiateRequest::decode(&bytes), Ok(request));
}

fn response_311(contexts: Vec<NegotiateContext>) -> NegotiateResponse {
    NegotiateResponse {
        security_mode: SecurityMode::SIGNING_ENABLED,
        dialect: Dialect::Smb311.code(),
        server_guid: [0x33; 16],
        capabilities: Capabilities::LEASING.with(Capabilities::LARGE_MTU),
        max_transact_size: 8 << 20,
        max_read_size: 8 << 20,
        max_write_size: 8 << 20,
        system_time: 133_000_000_000_000_000,
        server_start_time: 0,
        security_buffer: vec![0x60, 0x00],
        contexts,
    }
}

fn chosen(cipher: u16, signing: Option<u16>) -> Vec<NegotiateContext> {
    let mut contexts = vec![
        NegotiateContext::PreauthIntegrity {
            hash_algorithms: vec![SHA512],
            salt: vec![0x44; 32],
        },
        NegotiateContext::Encryption {
            ciphers: vec![cipher],
        },
    ];
    if let Some(id) = signing {
        contexts.push(NegotiateContext::Signing {
            algorithms: vec![id],
        });
    }
    contexts
}

#[test]
fn a_response_round_trips_with_its_contexts() {
    let response = response_311(chosen(4, Some(2)));
    let bytes = message(|enc| response.encode(enc));
    assert_eq!(NegotiateResponse::decode(&bytes), Ok(response));
}

#[test]
fn a_3_1_1_response_settles_its_cipher_and_signing() {
    let negotiated = offer(Dialect::Smb300)
        .accept(&response_311(chosen(Cipher::Aes256Gcm.code(), Some(2))))
        .expect("accepted");
    assert_eq!(negotiated.dialect, Dialect::Smb311);
    assert_eq!(negotiated.cipher, Cipher::Aes256Gcm);
    assert_eq!(negotiated.signing, SigningAlgorithm::AesGmac);
    assert_eq!(negotiated.server_guid, [0x33; 16]);

    let negotiated = offer(Dialect::Smb300)
        .accept(&response_311(chosen(Cipher::Aes128Ccm.code(), None)))
        .expect("accepted");
    assert_eq!(
        negotiated.signing,
        SigningAlgorithm::AesCmac,
        "no context means CMAC"
    );
}

#[test]
fn a_3_1_1_response_that_cannot_be_protected_is_refused() {
    for (contexts, refusal) in [
        (chosen(0, Some(2)), SmbError::Refused(Refusal::Encryption)),
        (
            vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![SHA512],
                salt: vec![0; 32],
            }],
            SmbError::Refused(Refusal::Encryption),
        ),
        (
            vec![NegotiateContext::Encryption { ciphers: vec![2] }],
            SmbError::Refused(Refusal::Integrity),
        ),
        (
            vec![
                NegotiateContext::PreauthIntegrity {
                    hash_algorithms: vec![0x0002],
                    salt: vec![0; 32],
                },
                NegotiateContext::Encryption { ciphers: vec![2] },
            ],
            SmbError::Refused(Refusal::Integrity),
        ),
        (chosen(2, Some(0)), SmbError::Refused(Refusal::Signing)),
        (chosen(9, Some(2)), SmbError::Protocol),
    ] {
        assert_eq!(
            offer(Dialect::Smb300).accept(&response_311(contexts.clone())),
            Err(refusal),
            "{contexts:?}"
        );
    }
}

#[test]
fn a_3_1_1_response_choosing_twice_is_a_protocol_violation() {
    let mut twice = chosen(2, Some(2));
    twice.push(NegotiateContext::Encryption { ciphers: vec![4] });
    assert_eq!(
        offer(Dialect::Smb300).accept(&response_311(twice)),
        Err(SmbError::Protocol)
    );
    let both = vec![
        NegotiateContext::PreauthIntegrity {
            hash_algorithms: vec![SHA512],
            salt: vec![0; 32],
        },
        NegotiateContext::Encryption {
            ciphers: vec![2, 4],
        },
    ];
    assert_eq!(
        offer(Dialect::Smb300).accept(&response_311(both)),
        Err(SmbError::Protocol)
    );
}

#[test]
fn an_unknown_context_is_ignored() {
    let mut contexts = chosen(2, Some(1));
    contexts.push(NegotiateContext::Other {
        context_type: 0x0003,
    });
    let response = response_311(contexts);
    assert_eq!(
        offer(Dialect::Smb300).accept(&response).map(|n| n.cipher),
        Ok(Cipher::Aes128Gcm)
    );
}

fn response_30(dialect: u16, capabilities: Capabilities) -> NegotiateResponse {
    NegotiateResponse {
        dialect,
        capabilities,
        contexts: Vec::new(),
        ..response_311(Vec::new())
    }
}

#[test]
fn a_3_0_response_needs_the_encryption_capability() {
    let negotiated = offer(Dialect::Smb300)
        .accept(&response_30(0x0302, Capabilities::ENCRYPTION))
        .expect("accepted");
    assert_eq!(negotiated.dialect, Dialect::Smb302);
    assert_eq!(negotiated.cipher, Cipher::Aes128Ccm);
    assert_eq!(
        offer(Dialect::Smb300).accept(&response_30(0x0300, Capabilities::LEASING)),
        Err(SmbError::Refused(Refusal::Encryption))
    );
}

#[test]
fn a_dialect_below_the_floor_is_refused() {
    for (floor, dialect) in [
        (Dialect::Smb300, 0x0210),
        (Dialect::Smb300, 0x0202),
        (Dialect::Smb300, 0x02ff),
        (Dialect::Smb311, 0x0302),
    ] {
        assert_eq!(
            offer(floor).accept(&response_30(dialect, Capabilities::ENCRYPTION)),
            Err(SmbError::Refused(Refusal::Dialect)),
            "{dialect:#06x}"
        );
    }
}

#[test]
fn validate_negotiate_restates_the_request_and_checks_the_answer() {
    let request = offer(Dialect::Smb300).request();
    let input = validate_negotiate_input(&request).expect("encodes");
    assert_eq!(input[..4], CLIENT_CAPS_BYTES);
    assert_eq!(input[4..20], [0x11; 16]);
    assert_eq!(input[22..24], 3u16.to_le_bytes());
    assert_eq!(input.len(), 24 + 2 * 3);

    let response = response_30(0x0302, Capabilities::ENCRYPTION);
    let negotiated = offer(Dialect::Smb300).accept(&response).expect("accepted");
    let output = validate_negotiate_output(&response);
    assert_eq!(check_validate_negotiate(&output, &negotiated), Ok(()));
    for at in [0, 4, 19, 20, 22] {
        let mut changed = output;
        changed[at] ^= 0x01;
        assert_eq!(
            check_validate_negotiate(&changed, &negotiated),
            Err(SmbError::Refused(Refusal::Downgrade)),
            "byte {at}"
        );
    }
    assert_eq!(
        check_validate_negotiate(&output[..23], &negotiated),
        Err(SmbError::Malformed)
    );
    let mut long = output.to_vec();
    long.push(0);
    assert_eq!(
        check_validate_negotiate(&long, &negotiated),
        Err(SmbError::Malformed),
        "an answer with more after it"
    );
}

const CLIENT_CAPS_BYTES: [u8; 4] = super::CLIENT_CAPABILITIES.bits().to_le_bytes();

#[test]
fn a_windows_negotiate_request_decodes_and_reencodes_byte_for_byte() {
    let message = vectors::bytes(vectors::SMB311_GCM_PREAUTH[0].0);
    let request = NegotiateRequest::decode(&message).expect("decodes");
    assert_eq!(request.dialects, [0x0202, 0x0210, 0x0300, 0x0302, 0x0311]);
    assert_eq!(request.capabilities.bits(), 0x66);
    match &request.contexts[..] {
        [NegotiateContext::PreauthIntegrity {
            hash_algorithms,
            salt,
        }, NegotiateContext::Encryption { ciphers }] => {
            assert_eq!(hash_algorithms[..], [SHA512]);
            assert_eq!(salt.len(), 32);
            assert_eq!(ciphers[..], [2, 1]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        vectors::reencoded(|enc| request.encode(enc))[HEADER_LEN..],
        message[HEADER_LEN..]
    );
}

#[test]
fn a_windows_negotiate_response_decodes_reencodes_and_is_accepted() {
    let message = vectors::bytes(vectors::SMB311_GCM_PREAUTH[1].0);
    let response = NegotiateResponse::decode(&message).expect("decodes");
    assert_eq!(response.dialect, 0x0311);
    assert_eq!(response.security_buffer.len(), 320);
    assert_eq!(
        (
            response.max_transact_size,
            response.max_read_size,
            response.max_write_size
        ),
        (8 << 20, 8 << 20, 8 << 20)
    );
    assert_eq!(
        vectors::reencoded(|enc| response.encode(enc))[HEADER_LEN..],
        message[HEADER_LEN..]
    );

    let negotiated = offer(Dialect::Smb300).accept(&response).expect("accepted");
    assert_eq!(
        (negotiated.dialect, negotiated.cipher, negotiated.signing),
        (
            Dialect::Smb311,
            Cipher::Aes128Gcm,
            SigningAlgorithm::AesCmac
        )
    );
}

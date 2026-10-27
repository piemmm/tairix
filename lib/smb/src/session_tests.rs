//! Unit tests for the `SESSION_SETUP` codec.

use alloc::vec;

use super::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use crate::header::HEADER_LEN;
use crate::negotiate::{Capabilities, SecurityMode};
use crate::vectors;
use crate::SmbError;

#[test]
fn a_request_carries_its_token_after_the_fixed_part() {
    let request = SessionSetupRequest {
        binding: false,
        security_mode: SecurityMode::SIGNING_REQUIRED,
        capabilities: Capabilities::DFS,
        previous_session_id: 0,
        security_buffer: vec![0x60, 0x48, 0x06, 0x06],
    };
    let bytes = vectors::message(|enc| request.encode(enc));
    assert_eq!(
        u16::from_le_bytes([bytes[76], bytes[77]]),
        88,
        "buffer offset"
    );
    assert_eq!(&bytes[88..], [0x60, 0x48, 0x06, 0x06]);
    assert_eq!(SessionSetupRequest::decode(&bytes), Ok(request));
}

#[test]
fn a_response_carries_its_flags_and_token() {
    let response = SessionSetupResponse {
        session_flags: SessionFlags::ENCRYPT_DATA,
        security_buffer: vec![0xa1, 0x07],
    };
    let bytes = vectors::message(|enc| response.encode(enc));
    let decoded = SessionSetupResponse::decode(&bytes).expect("decodes");
    assert!(decoded.session_flags.contains(SessionFlags::ENCRYPT_DATA));
    assert!(!decoded.session_flags.contains(SessionFlags::IS_GUEST));
    assert_eq!(decoded, response);
}

#[test]
fn a_token_outside_the_message_is_refused() {
    let response = SessionSetupResponse {
        session_flags: SessionFlags::default(),
        security_buffer: vec![1, 2, 3],
    };
    let mut bytes = vectors::message(|enc| response.encode(enc));
    bytes.truncate(bytes.len() - 1);
    assert_eq!(
        SessionSetupResponse::decode(&bytes),
        Err(SmbError::Malformed)
    );
    let mut wrong_size = vectors::message(|enc| response.encode(enc));
    wrong_size[64] = 8;
    assert_eq!(
        SessionSetupResponse::decode(&wrong_size),
        Err(SmbError::Malformed)
    );
}

#[test]
fn a_windows_authentication_decodes_and_reencodes_byte_for_byte() {
    let legs = vectors::SMB311_GCM_PREAUTH;
    for (message, token) in [(legs[2].0, 74), (legs[4].0, 463)] {
        let message = vectors::bytes(message);
        let request = SessionSetupRequest::decode(&message).expect("decodes");
        assert_eq!(request.security_buffer.len(), token);
        assert!(!request.binding);
        assert_eq!(request.previous_session_id, 0);
        assert_eq!(
            vectors::message(|enc| request.encode(enc))[HEADER_LEN..],
            message[HEADER_LEN..]
        );
    }
    for (message, token) in [(legs[3].0, 179), (vectors::SMB311_GCM_FINAL_SETUP, 29)] {
        let message = vectors::bytes(message);
        let response = SessionSetupResponse::decode(&message).expect("decodes");
        assert_eq!(response.security_buffer.len(), token);
        assert_eq!(response.session_flags, SessionFlags::default());
        assert_eq!(
            vectors::message(|enc| response.encode(enc))[HEADER_LEN..],
            message[HEADER_LEN..]
        );
    }
}

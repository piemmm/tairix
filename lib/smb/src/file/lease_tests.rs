//! Unit tests for leases, breaks, and their acknowledgment.

use alloc::vec::Vec;

use super::{BreakNotice, Lease, LeaseAck, LeaseBreak, LeaseState, OplockBreak, LEASE_CONTEXT_LEN};
use crate::header::HEADER_LEN;
use crate::vectors;
use crate::wire::Encoder;
use crate::SmbError;

const KEY: [u8; 16] = *b"lease key 0123ab";
const PARENT: [u8; 16] = *b"parent key 45678";

fn lease(parent: Option<[u8; 16]>, breaking: bool) -> Lease {
    Lease {
        key: KEY,
        state: LeaseState::READ.with(LeaseState::HANDLE),
        parent,
        epoch: 7,
        breaking,
    }
}

fn encoded(lease: &Lease) -> Vec<u8> {
    let mut enc = Encoder::new();
    lease.encode(&mut enc);
    enc.finish().expect("encodes")
}

#[test]
fn a_lease_context_lays_out_its_key_state_parent_and_epoch() {
    let bytes = encoded(&lease(Some(PARENT), false));
    assert_eq!(bytes.len(), LEASE_CONTEXT_LEN);
    assert_eq!(bytes[..16], KEY);
    assert_eq!(bytes[16..20], 3_u32.to_le_bytes(), "read and handle");
    assert_eq!(bytes[20..24], 4_u32.to_le_bytes(), "the parent key is set");
    assert_eq!(bytes[24..32], [0; 8], "no lease duration");
    assert_eq!(bytes[32..48], PARENT);
    assert_eq!(bytes[48..], [7, 0, 0, 0]);
}

#[test]
fn a_lease_round_trips_with_or_without_a_parent_or_a_break() {
    for (parent, breaking) in [(None, false), (Some(PARENT), false), (None, true)] {
        let lease = lease(parent, breaking);
        assert_eq!(Lease::decode(&encoded(&lease)), Ok(lease));
    }
}

#[test]
fn a_parent_key_without_its_flag_is_not_read() {
    let mut bytes = encoded(&lease(Some(PARENT), false));
    bytes[20] = 0;
    assert_eq!(Lease::decode(&bytes), Ok(lease(None, false)));
}

#[test]
fn a_lease_context_of_any_other_length_is_refused() {
    let bytes = encoded(&lease(None, false));
    assert_eq!(
        Lease::decode(&bytes[..LEASE_CONTEXT_LEN - 1]),
        Err(SmbError::Malformed)
    );
    let mut long = bytes;
    long.push(0);
    assert_eq!(Lease::decode(&long), Err(SmbError::Malformed));
}

#[test]
fn a_lease_break_reads_its_states_and_ignores_the_server_hints() {
    let notice = BreakNotice::Lease(LeaseBreak {
        epoch: 9,
        ack_required: true,
        key: KEY,
        current: LeaseState::READ.with(LeaseState::WRITE),
        new: LeaseState::READ,
    });
    let mut bytes = vectors::message(|enc| notice.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 44);
    assert_eq!(bytes[HEADER_LEN + 4..HEADER_LEN + 8], 1_u32.to_le_bytes());
    bytes[HEADER_LEN + 32..].copy_from_slice(&[0xee; 12]);
    assert_eq!(BreakNotice::decode(&bytes), Ok(notice));
}

#[test]
fn an_oplock_break_names_its_open_and_level() {
    let notice = BreakNotice::Oplock(OplockBreak {
        level: 1,
        file_id: [0x5a; 16],
    });
    let bytes = vectors::message(|enc| notice.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 24);
    assert_eq!(BreakNotice::decode(&bytes), Ok(notice));
}

#[test]
fn a_break_of_any_other_shape_is_refused() {
    let ack = vectors::message(|enc| {
        LeaseAck {
            key: KEY,
            state: LeaseState::READ,
        }
        .encode(enc);
    });
    assert_eq!(BreakNotice::decode(&ack), Err(SmbError::Malformed));
    let notice = BreakNotice::Lease(LeaseBreak {
        epoch: 1,
        ack_required: false,
        key: KEY,
        current: LeaseState::READ,
        new: LeaseState::NONE,
    });
    let bytes = vectors::message(|enc| notice.encode(enc));
    assert_eq!(
        BreakNotice::decode(&bytes[..bytes.len() - 1]),
        Err(SmbError::Malformed)
    );
}

#[test]
fn an_acknowledgment_carries_its_key_and_the_state_kept() {
    let ack = LeaseAck {
        key: KEY,
        state: LeaseState::READ,
    };
    let bytes = vectors::message(|enc| ack.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 36);
    assert_eq!(bytes[HEADER_LEN + 8..HEADER_LEN + 24], KEY);
    assert_eq!(LeaseAck::decode(&bytes), Ok(ack));
    assert_eq!(
        LeaseAck::decode(&bytes[..bytes.len() - 1]),
        Err(SmbError::Malformed)
    );
}

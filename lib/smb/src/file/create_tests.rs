//! Unit tests for `CREATE` and its context lists.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{
    Access, CreateAction, CreateOptions, CreateRequest, CreateResponse, DiskId, Disposition,
    Durable, MaximalAccess, OplockLevel, ShareAccess, MAX_CONTEXTS,
};
use crate::file::lease::{Lease, LeaseState};
use crate::file::name::SharePath;
use crate::file::{FileAttributes, FileTimes};
use crate::header::HEADER_LEN;
use crate::status::NtStatus;
use crate::vectors;
use crate::wire::Encoder;
use crate::SmbError;

const CREATE_GUID: [u8; 16] = *b"create guid 0001";

/// Where a response's context list starts: past the header and the 88 fixed
/// bytes, already eight-byte aligned.
const RESPONSE_CONTEXTS_AT: usize = HEADER_LEN + 88;

fn le16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn le32(bytes: &[u8], at: usize) -> usize {
    let mut field = [0; 4];
    field.copy_from_slice(&bytes[at..at + 4]);
    usize::try_from(u32::from_le_bytes(field)).expect("a 32-bit field")
}

fn shared(names: &[&str]) -> SharePath {
    names
        .iter()
        .try_fold(SharePath::root(), |path, name| path.join(name))
        .expect("a path")
}

fn lease() -> Lease {
    Lease {
        key: *b"lease key 0123ab",
        state: LeaseState::READ.with(LeaseState::HANDLE),
        parent: None,
        epoch: 1,
        breaking: false,
    }
}

fn request(path: SharePath) -> CreateRequest {
    CreateRequest {
        path,
        access: Access::READ_DATA.with(Access::READ_ATTRIBUTES),
        attributes: FileAttributes::NONE,
        share: ShareAccess::READ.with(ShareAccess::WRITE),
        disposition: Disposition::Open,
        options: CreateOptions::NON_DIRECTORY_FILE,
        lease: None,
        durable: None,
        maximal_access: false,
        disk_id: false,
    }
}

fn response() -> CreateResponse {
    let time = Time64::from_secs(1_700_000_000);
    CreateResponse {
        oplock: OplockLevel::None,
        reparse_point: false,
        action: CreateAction::Opened,
        times: FileTimes {
            created: time,
            accessed: time,
            written: time,
            changed: time,
        },
        allocation_size: 4096,
        end_of_file: 23,
        attributes: FileAttributes::ARCHIVE,
        file_id: [0x15; 16],
        lease: None,
        durable_timeout_ms: None,
        maximal_access: None,
        disk_id: None,
    }
}

/// The names in the context list at `offset`, walked by each `Next`, which
/// must keep every context eight-byte aligned from the header.
fn context_names(message: &[u8], offset: usize) -> Vec<[u8; 4]> {
    let mut names = Vec::new();
    let mut at = offset;
    loop {
        assert!(at.is_multiple_of(8), "a context at {at}");
        let name_at = at + le16(message, at + 4);
        names.push(message[name_at..name_at + 4].try_into().expect("a name"));
        match le32(message, at) {
            0 => return names,
            next => at += next,
        }
    }
}

/// One context as a server lays it out, its `Next` zero.
fn context(name: &[u8], data: &[u8]) -> Vec<u8> {
    let data_at = (16 + name.len()).next_multiple_of(8);
    let data_offset = if data.is_empty() { 0 } else { data_at };
    let mut out = Vec::new();
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(&u16::try_from(name.len()).expect("a name").to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&u16::try_from(data_offset).expect("an offset").to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).expect("data").to_le_bytes());
    out.extend_from_slice(name);
    out.resize(data_at, 0);
    out.extend_from_slice(data);
    out
}

/// `contexts` as one list, each padded to eight bytes and naming the next.
fn list(contexts: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, (name, data)) in contexts.iter().enumerate() {
        let mut one = context(name, data);
        if index + 1 < contexts.len() {
            one.resize(one.len().next_multiple_of(8), 0);
            let next = u32::try_from(one.len()).expect("a context");
            one[..4].copy_from_slice(&next.to_le_bytes());
        }
        out.extend_from_slice(&one);
    }
    out
}

/// A response carrying `list` as its context list, at `oplock`.
fn with_contexts(oplock: OplockLevel, list: &[u8]) -> Vec<u8> {
    let bare = CreateResponse {
        oplock,
        ..response()
    };
    let mut bytes = vectors::message(|enc| bare.encode(enc));
    bytes.truncate(RESPONSE_CONTEXTS_AT);
    let offset = u32::try_from(RESPONSE_CONTEXTS_AT).expect("an offset");
    let len = u32::try_from(list.len()).expect("a list");
    bytes[HEADER_LEN + 80..HEADER_LEN + 84].copy_from_slice(&offset.to_le_bytes());
    bytes[HEADER_LEN + 84..HEADER_LEN + 88].copy_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(list);
    bytes
}

fn lease_data() -> Vec<u8> {
    let mut enc = Encoder::new();
    lease().encode(&mut enc);
    enc.finish().expect("encodes")
}

#[test]
fn a_request_names_its_path_after_the_fixed_part() {
    let create = request(shared(&["docs", "a.txt"]));
    let bytes = vectors::message(|enc| create.encode(enc));
    assert_eq!(bytes[HEADER_LEN + 3], 0, "no oplock");
    assert_eq!(le32(&bytes, HEADER_LEN + 4), 2, "impersonation");
    assert_eq!(le16(&bytes, HEADER_LEN + 44), 120, "name offset");
    assert_eq!(le16(&bytes, HEADER_LEN + 46), 20, "name length");
    assert_eq!(le32(&bytes, HEADER_LEN + 48), 0, "no contexts");
    assert_eq!(bytes.len(), 140);
    assert_eq!(CreateRequest::decode(&bytes), Ok(create));
}

#[test]
fn a_request_for_the_root_carries_one_byte_of_buffer() {
    let create = request(SharePath::root());
    let bytes = vectors::message(|enc| create.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 57);
    assert_eq!(le16(&bytes, HEADER_LEN + 46), 0, "name length");
    assert_eq!(CreateRequest::decode(&bytes), Ok(create));
}

#[test]
fn a_request_s_contexts_are_aligned_from_its_header_and_each_named_once() {
    let create = CreateRequest {
        lease: Some(lease()),
        durable: Some(Durable::Request {
            timeout_ms: 0,
            create_guid: CREATE_GUID,
        }),
        maximal_access: true,
        disk_id: true,
        ..request(shared(&["a"]))
    };
    let bytes = vectors::message(|enc| create.encode(enc));
    assert_eq!(bytes[HEADER_LEN + 3], 0xff, "the lease oplock");
    let offset = le32(&bytes, HEADER_LEN + 48);
    assert_eq!(offset, 128, "past the name, aligned");
    assert_eq!(offset + le32(&bytes, HEADER_LEN + 52), bytes.len());
    assert_eq!(
        context_names(&bytes, offset),
        [*b"RqLs", *b"DH2Q", *b"MxAc", *b"QFid"]
    );
    assert_eq!(CreateRequest::decode(&bytes), Ok(create));
}

#[test]
fn a_request_reads_the_same_inside_a_compound() {
    let create = CreateRequest {
        maximal_access: true,
        ..request(shared(&["abc"]))
    };
    let alone = vectors::message(|enc| create.encode(enc));
    let mut enc = Encoder::new();
    enc.zeros(40);
    enc.start_message();
    enc.zeros(HEADER_LEN);
    create.encode(&mut enc);
    let compounded = enc.finish().expect("encodes");
    assert_eq!(compounded[40..], alone[..]);
}

#[test]
fn a_reconnect_names_the_open_and_the_create_that_made_it() {
    let create = CreateRequest {
        durable: Some(Durable::Reconnect {
            file_id: [0x42; 16],
            create_guid: CREATE_GUID,
        }),
        ..request(shared(&["a.txt"]))
    };
    let bytes = vectors::message(|enc| create.encode(enc));
    let offset = le32(&bytes, HEADER_LEN + 48);
    assert_eq!(context_names(&bytes, offset), [*b"DH2C"]);
    let data = offset + le16(&bytes, offset + 10);
    assert_eq!(le32(&bytes, offset + 12), 36);
    assert_eq!(bytes[data..data + 16], [0x42; 16]);
    assert_eq!(bytes[data + 16..data + 32], CREATE_GUID);
    assert_eq!(CreateRequest::decode(&bytes), Ok(create));
}

#[test]
fn a_request_asking_two_durabilities_is_refused() {
    let create = CreateRequest {
        durable: Some(Durable::Request {
            timeout_ms: 60_000,
            create_guid: CREATE_GUID,
        }),
        ..request(shared(&["a"]))
    };
    let mut bytes = vectors::message(|enc| create.encode(enc));
    let offset = le32(&bytes, HEADER_LEN + 48);
    let mut reconnect = vec![0; 36];
    reconnect[..16].copy_from_slice(&[0x42; 16]);
    let second = context(b"DH2C", &reconnect);
    let next = u32::try_from(bytes.len() - offset).expect("a context");
    bytes[offset..offset + 4].copy_from_slice(&next.to_le_bytes());
    bytes.extend_from_slice(&second);
    let len = u32::try_from(bytes.len() - offset).expect("a list");
    bytes[HEADER_LEN + 52..HEADER_LEN + 56].copy_from_slice(&len.to_le_bytes());
    assert_eq!(CreateRequest::decode(&bytes), Err(SmbError::Malformed));
}

#[test]
fn a_response_carries_what_the_create_did_and_the_contexts_granted() {
    let granted = CreateResponse {
        oplock: OplockLevel::Lease,
        action: CreateAction::Created,
        lease: Some(Lease {
            breaking: true,
            ..lease()
        }),
        durable_timeout_ms: Some(120_000),
        maximal_access: Some(MaximalAccess {
            status: NtStatus::SUCCESS,
            access: Access::from_bits(0x001f_01ff),
        }),
        disk_id: Some(DiskId {
            file: 0x0001_0000_0000_0042,
            volume: 0x1234,
        }),
        ..response()
    };
    let bytes = vectors::message(|enc| granted.encode(enc));
    assert_eq!(le32(&bytes, HEADER_LEN + 80), RESPONSE_CONTEXTS_AT);
    assert_eq!(
        context_names(&bytes, RESPONSE_CONTEXTS_AT),
        [*b"RqLs", *b"DH2Q", *b"MxAc", *b"QFid"]
    );
    assert_eq!(CreateResponse::decode(&bytes), Ok(granted));
}

#[test]
fn a_bare_response_carries_one_byte_of_buffer() {
    let bare = CreateResponse {
        reparse_point: true,
        ..response()
    };
    let bytes = vectors::message(|enc| bare.encode(enc));
    assert_eq!(bytes.len(), HEADER_LEN + 89);
    assert_eq!(CreateResponse::decode(&bytes), Ok(bare));
}

#[test]
fn a_context_this_client_never_asks_for_is_passed_over() {
    let maximal = [0, 0, 0, 0, 0xff, 0x01, 0x1f, 0x00];
    let bytes = with_contexts(
        OplockLevel::None,
        &list(&[(b"app instance id!", &[0x77; 20]), (b"MxAc", &maximal)]),
    );
    let decoded = CreateResponse::decode(&bytes).expect("decodes");
    assert_eq!(
        decoded.maximal_access,
        Some(MaximalAccess {
            status: NtStatus::SUCCESS,
            access: Access::from_bits(0x001f_01ff),
        })
    );
}

#[test]
fn a_lease_and_the_lease_oplock_come_together_or_not_at_all() {
    let lease = lease_data();
    let unflagged = with_contexts(OplockLevel::None, &list(&[(b"RqLs", &lease)]));
    assert_eq!(CreateResponse::decode(&unflagged), Err(SmbError::Malformed));
    let flagged = with_contexts(OplockLevel::Lease, &[]);
    assert_eq!(CreateResponse::decode(&flagged), Err(SmbError::Malformed));
    let both = with_contexts(OplockLevel::Lease, &list(&[(b"RqLs", &lease)]));
    assert!(CreateResponse::decode(&both).is_ok());
}

#[test]
fn a_known_context_of_the_wrong_length_is_refused() {
    for (name, len) in [
        (*b"MxAc", 7),
        (*b"DH2Q", 16),
        (*b"QFid", 31),
        (*b"RqLs", 32),
    ] {
        let bytes = with_contexts(OplockLevel::None, &list(&[(&name, &vec![0; len])]));
        assert_eq!(
            CreateResponse::decode(&bytes),
            Err(SmbError::Malformed),
            "{name:?}"
        );
    }
}

#[test]
fn a_context_list_that_breaks_its_framing_is_refused() {
    let maximal = [0; 8];
    let twice = list(&[(b"MxAc", &maximal), (b"MxAc", &maximal)]);
    let mut misaligned = list(&[(b"MxAc", &maximal), (b"QFid", &[0; 32])]);
    misaligned[0] += 1;
    let mut past = list(&[(b"MxAc", &maximal), (b"QFid", &[0; 32])]);
    past[..4].copy_from_slice(&0x1000_u32.to_le_bytes());
    let mut nameless = context(b"MxAc", &maximal);
    nameless[6] = 0;
    let mut spilling = context(b"MxAc", &maximal);
    spilling[12] = 9;
    for (what, broken) in [
        ("a name twice", twice),
        ("a misaligned successor", misaligned),
        ("a successor past the list", past),
        ("an empty name", nameless),
        ("data past the context", spilling),
    ] {
        let bytes = with_contexts(OplockLevel::None, &broken);
        assert_eq!(
            CreateResponse::decode(&bytes),
            Err(SmbError::Malformed),
            "{what}"
        );
    }
}

#[test]
fn a_context_list_past_its_bound_is_refused() {
    let names: Vec<[u8; 4]> = (0..=MAX_CONTEXTS)
        .map(|index| [b'X', b'x', b'0', u8::try_from(index).expect("a few")])
        .collect();
    let contexts: Vec<(&[u8], &[u8])> = names.iter().map(|name| (&name[..], &[][..])).collect();
    let bytes = with_contexts(OplockLevel::None, &list(&contexts));
    assert_eq!(CreateResponse::decode(&bytes), Err(SmbError::TooLarge));
    let bytes = with_contexts(OplockLevel::None, &list(&contexts[1..]));
    assert!(CreateResponse::decode(&bytes).is_ok());
}

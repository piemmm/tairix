//! Unit tests for `QUERY_INFO`, `SET_INFO`, and the classes they carry.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use super::{
    BasicInfo, BasicUpdate, Class, DispositionInfo, EndOfFileInfo, FsAbilities, FsAttribute,
    FsFullSize, FsVolume, InternalInfo, NetworkOpenInfo, QueryInfoRequest, QueryInfoResponse,
    RenameInfo, SetInfoRequest, SetInfoResponse, StandardInfo, MAX_INFO,
};
use crate::file::name::SharePath;
use crate::file::{FileAttributes, FileTimes};
use crate::header::HEADER_LEN;
use crate::vectors;
use crate::wire::{utf16le, Encoder};
use crate::SmbError;

fn time(secs: i64, nanos: u32) -> Time64 {
    Time64::new(secs, nanos).expect("a valid time")
}

fn times() -> FileTimes {
    FileTimes {
        created: time(-2_208_988_800, 0),
        accessed: time(0, 100),
        written: time(2_147_483_648, 9_999_900),
        changed: time(32_503_680_000, 0),
    }
}

fn encoded(encode: impl FnOnce(&mut Encoder)) -> Result<Vec<u8>, SmbError> {
    let mut enc = Encoder::new();
    encode(&mut enc);
    enc.finish()
}

fn utf16(text: &str) -> Vec<u8> {
    encoded(|enc| utf16le(text, enc)).expect("encodes")
}

/// `decode` refuses `bytes` one byte short and one byte long.
fn exact<T: core::fmt::Debug>(bytes: &[u8], decode: impl Fn(&[u8]) -> Result<T, SmbError>) {
    assert!(decode(bytes).is_ok());
    assert_eq!(
        decode(&bytes[..bytes.len() - 1]).err(),
        Some(SmbError::Malformed)
    );
    let mut long = bytes.to_vec();
    long.push(0);
    assert_eq!(decode(&long).err(), Some(SmbError::Malformed));
}

#[test]
fn a_query_names_its_class_by_type_and_number() {
    for (class, info_type, code) in [
        (Class::Basic, 1, 4),
        (Class::Standard, 1, 5),
        (Class::Internal, 1, 6),
        (Class::Rename, 1, 10),
        (Class::Disposition, 1, 13),
        (Class::EndOfFile, 1, 20),
        (Class::NetworkOpen, 1, 34),
        (Class::FsVolume, 2, 1),
        (Class::FsAttribute, 2, 5),
        (Class::FsFullSize, 2, 7),
    ] {
        let query = QueryInfoRequest {
            class,
            output_length: 4096,
            file_id: [6; 16],
        };
        let bytes = vectors::message(|enc| query.encode(enc));
        assert_eq!(bytes.len(), HEADER_LEN + 41);
        assert_eq!(bytes[HEADER_LEN + 2..HEADER_LEN + 4], [info_type, code]);
        assert_eq!(QueryInfoRequest::decode(&bytes), Ok(query));
    }
}

#[test]
fn a_query_of_a_class_this_client_never_asks_is_refused() {
    let query = QueryInfoRequest {
        class: Class::Basic,
        output_length: 40,
        file_id: [6; 16],
    };
    let bytes = vectors::message(|enc| query.encode(enc));
    for (info_type, code) in [(1, 7), (2, 4), (3, 1), (0, 4)] {
        let mut other = bytes.clone();
        other[HEADER_LEN + 2] = info_type;
        other[HEADER_LEN + 3] = code;
        assert_eq!(
            QueryInfoRequest::decode(&other),
            Err(SmbError::Malformed),
            "{info_type}, {code}"
        );
    }
    let greedy = QueryInfoRequest {
        output_length: u32::try_from(MAX_INFO + 1).expect("a length"),
        ..query
    };
    assert_eq!(encoded(|enc| greedy.encode(enc)), Err(SmbError::TooLarge));
}

#[test]
fn a_query_response_is_bounded() {
    let output = [0x11; 40];
    let bytes = vectors::message(|enc| QueryInfoResponse { output: &output }.encode(enc));
    assert_eq!(
        QueryInfoResponse::decode(&bytes),
        Ok(QueryInfoResponse { output: &output })
    );
    let mut long = bytes;
    let len = u32::try_from(MAX_INFO + 1).expect("a length");
    long[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(QueryInfoResponse::decode(&long), Err(SmbError::TooLarge));
}

#[test]
fn a_set_carries_its_class_s_bytes_after_the_fixed_part() {
    let update = BasicUpdate {
        written: Some(time(1_700_000_000, 0)),
        attributes: Some(FileAttributes::HIDDEN),
        ..BasicUpdate::default()
    };
    let set = SetInfoRequest::new(Class::Basic, [2; 16], |enc| update.encode(enc)).expect("a set");
    assert_eq!(set.buffer.len(), BasicInfo::LEN);
    let bytes = vectors::message(|enc| set.encode(enc));
    assert_eq!(
        bytes[HEADER_LEN + 8..HEADER_LEN + 10],
        [96, 0],
        "buffer offset"
    );
    assert_eq!(bytes[96..], set.buffer[..]);
    let decoded = SetInfoRequest::decode(&bytes).expect("decodes");
    assert_eq!(decoded, set);
    assert_eq!(BasicUpdate::decode(&decoded.buffer), Ok(update));

    let mut long = bytes;
    let len = u32::try_from(MAX_INFO + 1).expect("a length");
    long[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(SetInfoRequest::decode(&long), Err(SmbError::TooLarge));
}

#[test]
fn a_set_is_answered_with_its_status_alone() {
    let bytes = vectors::message(|enc| SetInfoResponse.encode(enc));
    assert_eq!(bytes[HEADER_LEN..], [2, 0]);
    assert_eq!(SetInfoResponse::decode(&bytes), Ok(SetInfoResponse));
    let mut other = bytes;
    other[HEADER_LEN] = 4;
    assert_eq!(SetInfoResponse::decode(&other), Err(SmbError::Malformed));
}

#[test]
fn a_set_that_cannot_be_written_is_refused_before_it_is_sent() {
    let sub_interval = BasicUpdate {
        changed: Some(time(0, 1)),
        ..BasicUpdate::default()
    };
    assert_eq!(
        SetInfoRequest::new(Class::Basic, [2; 16], |enc| sub_interval.encode(enc)),
        Err(SmbError::TimeOutOfRange)
    );
    let epoch = BasicUpdate {
        created: Some(time(-11_644_473_600, 0)),
        ..BasicUpdate::default()
    };
    assert_eq!(
        SetInfoRequest::new(Class::Basic, [2; 16], |enc| epoch.encode(enc)),
        Err(SmbError::TimeOutOfRange),
        "FILETIME zero reads as unchanged"
    );
}

#[test]
fn an_update_leaves_what_it_does_not_name_unchanged() {
    let unchanged = encoded(|enc| BasicUpdate::default().encode(enc)).expect("encodes");
    assert_eq!(unchanged, [0; BasicInfo::LEN]);
    assert_eq!(BasicUpdate::decode(&unchanged), Ok(BasicUpdate::default()));
    exact(&unchanged, BasicUpdate::decode);
}

#[test]
fn each_fixed_class_round_trips_at_exactly_its_length() {
    let basic = BasicInfo {
        times: times(),
        attributes: FileAttributes::READONLY,
    };
    let bytes = encoded(|enc| basic.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), BasicInfo::LEN);
    assert_eq!(BasicInfo::decode(&bytes), Ok(basic));
    exact(&bytes, BasicInfo::decode);

    let standard = StandardInfo {
        allocation_size: 8192,
        end_of_file: 5000,
        links: 2,
        delete_pending: true,
        directory: false,
    };
    let bytes = encoded(|enc| standard.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), StandardInfo::LEN);
    assert_eq!(StandardInfo::decode(&bytes), Ok(standard));
    exact(&bytes, StandardInfo::decode);

    let internal = InternalInfo { index: u64::MAX };
    let bytes = encoded(|enc| internal.encode(enc)).expect("encodes");
    assert_eq!(InternalInfo::decode(&bytes), Ok(internal));
    exact(&bytes, InternalInfo::decode);

    let open = NetworkOpenInfo {
        times: times(),
        allocation_size: 1 << 47,
        end_of_file: (1 << 47) - 1,
        attributes: FileAttributes::SPARSE_FILE,
    };
    let bytes = encoded(|enc| open.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), NetworkOpenInfo::LEN);
    assert_eq!(NetworkOpenInfo::decode(&bytes), Ok(open));
    exact(&bytes, NetworkOpenInfo::decode);

    let size = EndOfFileInfo {
        end_of_file: 150 << 40,
    };
    let bytes = encoded(|enc| size.encode(enc)).expect("encodes");
    assert_eq!(EndOfFileInfo::decode(&bytes), Ok(size));
    exact(&bytes, EndOfFileInfo::decode);

    let delete = DispositionInfo { delete: true };
    let bytes = encoded(|enc| delete.encode(enc)).expect("encodes");
    assert_eq!(bytes, [1]);
    assert_eq!(DispositionInfo::decode(&bytes), Ok(delete));
    exact(&bytes, DispositionInfo::decode);

    let full = FsFullSize {
        total_units: 1 << 40,
        caller_available_units: 1 << 39,
        available_units: 1 << 39,
        sectors_per_unit: 8,
        bytes_per_sector: 512,
    };
    let bytes = encoded(|enc| full.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), FsFullSize::LEN);
    assert_eq!(FsFullSize::decode(&bytes), Ok(full));
    exact(&bytes, FsFullSize::decode);
}

#[test]
fn a_rename_names_its_target_from_the_root_padded_to_its_least_size() {
    let short = RenameInfo {
        replace: true,
        target: SharePath::root().join("a").expect("a name"),
    };
    let bytes = encoded(|enc| short.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), 24, "padded");
    assert_eq!(bytes[..8], [1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(bytes[16..22], [2, 0, 0, 0, b'a', 0]);
    assert_eq!(RenameInfo::decode(&bytes), Ok(short));

    let deep = RenameInfo {
        replace: false,
        target: ["docs", "2026", "report.txt"]
            .iter()
            .try_fold(SharePath::root(), |path, name| path.join(name))
            .expect("a path"),
    };
    let bytes = encoded(|enc| deep.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), 20 + 2 * 20);
    assert_eq!(RenameInfo::decode(&bytes), Ok(deep));
}

#[test]
fn a_rename_to_the_root_or_beside_a_handle_is_refused() {
    let mut root = Vec::from([0; 20]);
    assert_eq!(RenameInfo::decode(&root), Err(SmbError::Name));
    root[8] = 1;
    assert_eq!(
        RenameInfo::decode(&root),
        Err(SmbError::Malformed),
        "a root handle"
    );
    let mut stream = Vec::from([0; 16]);
    stream.extend_from_slice(&6_u32.to_le_bytes());
    stream.extend_from_slice(&utf16("a:b"));
    assert_eq!(RenameInfo::decode(&stream), Err(SmbError::Name));
}

#[test]
fn a_filesystem_names_itself_within_its_bound() {
    let attribute = FsAttribute {
        abilities: FsAbilities::CASE_PRESERVED_NAMES.with(FsAbilities::UNICODE_ON_DISK),
        max_name_length: 255,
        name: String::from("NTFS"),
    };
    let bytes = encoded(|enc| attribute.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), 12 + 8);
    assert_eq!(FsAttribute::decode(&bytes), Ok(attribute));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(FsAttribute::decode(&trailing), Err(SmbError::Malformed));

    let mut long = Vec::from([0; 8]);
    long.extend_from_slice(&514_u32.to_le_bytes());
    long.extend_from_slice(&[b'a', 0].repeat(257));
    assert_eq!(FsAttribute::decode(&long), Err(SmbError::TooLarge));
}

#[test]
fn a_volume_s_label_is_read_without_its_padding() {
    let volume = FsVolume {
        created: time(1_262_304_000, 0),
        serial: 0x1234_5678,
        label: String::from("Backup"),
    };
    let bytes = encoded(|enc| volume.encode(enc)).expect("encodes");
    assert_eq!(bytes.len(), 18 + 12);
    assert_eq!(FsVolume::decode(&bytes), Ok(volume.clone()));

    let mut padded = bytes[..18].to_vec();
    padded[12..16].copy_from_slice(&18_u32.to_le_bytes());
    padded.extend_from_slice(&utf16("Backup  \0"));
    assert_eq!(FsVolume::decode(&padded), Ok(volume));

    let mut long = bytes[..18].to_vec();
    long[12..16].copy_from_slice(&514_u32.to_le_bytes());
    long.extend_from_slice(&[b'a', 0].repeat(257));
    assert_eq!(FsVolume::decode(&long), Err(SmbError::TooLarge));
}

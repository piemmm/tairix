use super::*;

#[test]
fn hints_round_trip_and_unknown_reads_as_none() {
    let info = FsInfo {
        free: Some(1234),
        next: None,
    };
    let sector = info.encode();
    assert_eq!(&sector[at::NEXT..at::NEXT + 4], &[0xFF; 4]);
    assert_eq!(FsInfo::decode(&sector), Some(info));
    assert_eq!(
        FsInfo::decode(&FsInfo::UNKNOWN.encode()),
        Some(FsInfo::UNKNOWN)
    );
}

#[test]
fn a_sector_without_every_signature_is_no_fsinfo() {
    for at in [at::LEAD, at::STRUCTURE, at::TRAIL] {
        let mut sector = FsInfo::UNKNOWN.encode();
        sector[at] ^= 1;
        assert_eq!(FsInfo::decode(&sector), None);
    }
}

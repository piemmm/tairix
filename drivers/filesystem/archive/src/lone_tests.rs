//! How a lone compressed file is named: from its header, else from its
//! archive's name as gzip and bzip2 name what they decompress.

use super::*;

#[test]
fn an_archives_ending_is_replaced_as_its_compressor_replaces_it() {
    let cases: [(Compression, &[u8], &[u8]); 9] = [
        (Compression::Gzip, b"notes.txt.gz", b"notes.txt"),
        (Compression::Gzip, b"NOTES.GZ", b"NOTES"),
        (Compression::Gzip, b"backup.tgz", b"backup.tar"),
        (Compression::Gzip, b"old.taz", b"old.tar"),
        (Compression::Gzip, b"report-gz", b"report"),
        (Compression::Bzip2, b"notes.bz2", b"notes"),
        (Compression::Bzip2, b"src.tbz2", b"src.tar"),
        (Compression::Bzip2, b"notes.gz", b"notes.gz.out"),
        (Compression::Gzip, b".gz", b".gz.out"),
    ];
    for (compression, archive, expected) in cases {
        assert_eq!(
            decompressed_name(compression, archive).unwrap(),
            expected,
            "{:?}",
            core::str::from_utf8(archive)
        );
    }
}

#[test]
fn a_name_too_long_to_grow_is_kept_as_it_is() {
    let longest = [b'n'; tairix_abi::fs::FS_NAME_MAX];
    assert_eq!(
        decompressed_name(Compression::Bzip2, &longest).unwrap(),
        longest
    );
}

#[test]
fn a_tar_is_named_by_its_ending_compressed_either_way() {
    for tar in [
        &b"a.tar.gz"[..],
        b"a.TGZ",
        b"a.taz",
        b"a.tar.bz2",
        b"a.tbz2",
        b"a.tbz",
        b"a.tar.z",
    ] {
        assert!(names_tar(tar), "{:?}", core::str::from_utf8(tar));
    }
    for other in [&b"a.gz"[..], b"a.bz2", b"tar.gz", b"a.tar", b"a.zip"] {
        assert!(!names_tar(other), "{:?}", core::str::from_utf8(other));
    }
}

#[test]
fn a_recorded_name_is_its_last_component_read_as_latin_1() {
    assert_eq!(recorded_name(b"notes.txt"), Some(b"notes.txt".to_vec()));
    assert_eq!(
        recorded_name(b"/etc/../dir/notes.txt"),
        Some(b"notes.txt".to_vec())
    );
    assert_eq!(recorded_name(b"caf\xe9"), Some(b"caf\xc3\xa9".to_vec()));
    for nothing in [&b""[..], b"..", b"dir/", b"."] {
        assert_eq!(recorded_name(nothing), None, "{nothing:?}");
    }
}

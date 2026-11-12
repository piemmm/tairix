use super::*;

#[test]
fn a_raw_image_of_a_diskettes_size_goes_in_as_its_sectors() {
    assert_eq!(
        form_of(1_474_560, &[0xEB, 0x3C, 0x90, 0x4D], true),
        Ok(Form::Sectors {
            count: 2880,
            read_only: false
        })
    );
    assert_eq!(
        form_of(368_640, b"\0\0\0\0", false),
        Ok(Form::Sectors {
            count: 720,
            read_only: true
        }),
        "handed over read-only, it stays so"
    );
}

#[test]
fn an_archive_goes_in_by_its_magic_whatever_its_size() {
    assert_eq!(
        form_of(12_345, b"IMD ", true),
        Ok(Form::Archive { bytes: 12_345 })
    );
    assert_eq!(
        form_of(u64::from(MAX_ARCHIVE) + 1, b"IMD ", true),
        Err(Unfit::TooLarge)
    );
}

#[test]
fn anything_else_goes_into_no_drive() {
    for len in [0, 511, 1_474_561, 30 << 20] {
        assert_eq!(
            form_of(len, b"\0\0\0\0", true),
            Err(Unfit::NotADiskette),
            "{len}"
        );
    }
    assert!(
        form_of(1_474_560, b"IM", true).is_ok(),
        "a short head is no archive"
    );
}

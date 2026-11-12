use super::*;

#[test]
fn twelve_bit_entries_share_their_middle_byte() {
    let mut fat = [0u8; 8];
    for (cluster, value) in [0xFF0u32, 0xFFF, 0x003, 0x004, 0xFFF]
        .into_iter()
        .enumerate()
    {
        assert!(set(
            Kind::Fat12,
            &mut fat,
            u32::try_from(cluster).unwrap(),
            value
        ));
    }
    assert_eq!(fat, [0xF0, 0xFF, 0xFF, 0x03, 0x40, 0x00, 0xFF, 0x0F]);
    assert!(set(Kind::Fat12, &mut fat, 3, 0xABC));
    assert_eq!(get(Kind::Fat12, &fat, 2), Some(0x003), "the neighbour kept");
    assert_eq!(get(Kind::Fat12, &fat, 3), Some(0xABC));
    assert_eq!(get(Kind::Fat12, &fat, 4), Some(0xFFF));
    assert_eq!(get(Kind::Fat12, &fat, 5), None, "past the table");
    assert!(!set(Kind::Fat12, &mut fat, 5, 0));
}

#[test]
fn sixteen_and_thirty_two_bit_entries_and_the_reserved_top_four_bits() {
    let mut fat = [0u8; 16];
    assert!(set(Kind::Fat16, &mut fat, 3, 0x1234_5678));
    assert_eq!(get(Kind::Fat16, &fat, 3), Some(0x5678));
    let mut fat = [0xFFu8; 16];
    assert!(set(Kind::Fat32, &mut fat, 2, 0x0000_0005));
    assert_eq!(
        &fat[8..12],
        &[0x05, 0x00, 0x00, 0xF0],
        "the top four bits kept"
    );
    assert_eq!(get(Kind::Fat32, &fat, 2), Some(5));
    assert_eq!(span(Kind::Fat32, 2), (8, 4));
    assert_eq!(span(Kind::Fat12, 3), (4, 2));
}

#[test]
fn each_kind_reads_free_next_end_and_bad_from_its_own_values() {
    for (kind, max) in [
        (Kind::Fat12, 4000),
        (Kind::Fat16, 60_000),
        (Kind::Fat32, 1_000_000),
    ] {
        assert_eq!(classify(kind, 0, max), Link::Free);
        assert_eq!(classify(kind, 1, max), Link::Bad, "names no cluster");
        assert_eq!(classify(kind, 2, max), Link::Next(2));
        assert_eq!(classify(kind, max, max), Link::Next(max));
        assert_eq!(classify(kind, max + 1, max), Link::Bad, "past the volume");
        assert_eq!(classify(kind, bad(kind), max), Link::Bad);
        assert_eq!(classify(kind, end(kind), max), Link::End);
        assert_eq!(
            classify(kind, mask(kind) - 7, max),
            Link::End,
            "every end value"
        );
    }
    assert_eq!(
        classify(Kind::Fat32, 0xF000_0003, 10),
        Link::Next(3),
        "the top bits ignored"
    );
    assert_eq!(head(Kind::Fat12, 0xF0), [0xFF0, 0xFFF]);
    assert_eq!(head(Kind::Fat32, 0xF8), [0x0FFF_FFF8, 0x0FFF_FFFF]);
}

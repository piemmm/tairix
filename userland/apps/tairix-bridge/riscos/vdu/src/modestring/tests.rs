use alloc::vec::Vec;

use super::*;

/// A selector's width, height, depth, frame rate and pairs.
type Words = (u32, u32, u32, u32, Vec<(u32, u32)>);

fn words(text: &str) -> Option<Words> {
    parse(text.as_bytes()).map(|s| {
        (
            s.width,
            s.height,
            s.log2_bpp,
            s.frame_rate,
            s.pairs().to_vec(),
        )
    })
}

#[test]
fn mode_strings_read_as_genuine_risc_os_5_read_them() {
    assert_eq!(
        words("X640 Y480 C256"),
        Some((640, 480, 3, u32::MAX, alloc::vec![(3, 255), (0, 0x80)]))
    );
    assert_eq!(
        words("X640,Y480,C16 EX0 EY0"),
        Some((
            640,
            480,
            2,
            u32::MAX,
            alloc::vec![(3, 15), (0, 0), (4, 0), (5, 0)]
        ))
    );
    assert_eq!(
        words("28 C16M"),
        Some((
            640,
            480,
            5,
            u32::MAX,
            alloc::vec![(3, 0xFFFF_FFFF), (0, 0), (4, 1), (5, 1)]
        ))
    );
    assert_eq!(
        words("X640 Y480 C32K F60"),
        Some((640, 480, 4, 60, alloc::vec![(3, 0xFFFF), (0, 0)]))
    );
    assert_eq!(
        words("X640 Y480 G256"),
        Some((640, 480, 3, u32::MAX, alloc::vec![(3, 255), (0, 0x280)]))
    );
    assert_eq!(
        words("X640 Y480 C16M LTRGB").map(|w| w.4[1]),
        Some((0, 0x4000))
    );
    assert_eq!(
        words("X640 Y480 T16 TX40 TY25"),
        Some((
            640,
            480,
            2,
            u32::MAX,
            alloc::vec![(3, 15), (0, 6), (1, 39), (2, 24)]
        ))
    );
}

#[test]
fn a_mode_string_missing_or_repeating_an_attribute_is_refused() {
    for bad in [
        "X640 Y480",
        "X640 Y480 C16 G16",
        "bad",
        "X640 X640 Y480 C2",
        "X640 Y480 C64K LABGR",
        "X640 Y480 C16 EX4",
    ] {
        assert_eq!(parse(bad.as_bytes()), None, "{bad}");
    }
}

#[test]
fn a_selector_is_written_back_as_a_string() {
    let selector = parse(b"X640 Y480 C256 F60 EX1 EY1").expect("read");
    let text = write(&selector).expect("room");
    assert_eq!(text, "X640 Y480 C256 EX1 EY1 F60");
    assert_eq!(parse(text.as_bytes()), Some(selector));
    assert_eq!(
        parse(b"x640 y480 c16m ltrgb"),
        parse(b"X640 Y480 C16M LTRGB"),
        "attributes read whatever their case"
    );
}

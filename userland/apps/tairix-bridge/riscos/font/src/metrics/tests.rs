use super::*;
use alloc::vec;

/// A metrics file of version 0: `chars` each `(code, box, x offset)`, the
/// slots in the order given, slot 0 unused.
fn plain(name: &[u8], chars: &[(u8, [i16; 4], i16)]) -> Vec<u8> {
    let slots = chars.len() + 1;
    let mut out = name.to_vec();
    out.resize(NAME, b'\r');
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&[u8::try_from(slots).expect("few"), PLAIN, 0, 0]);
    let mut map = [0u8; 256];
    for (slot, &(code, _, _)) in chars.iter().enumerate() {
        map[usize::from(code)] = u8::try_from(slot + 1).expect("few");
    }
    out.extend_from_slice(&map);
    for corner in 0..4 {
        out.extend_from_slice(&0i16.to_le_bytes());
        for &(_, bbox, _) in chars {
            out.extend_from_slice(&bbox[corner].to_le_bytes());
        }
    }
    for column in [true, false] {
        out.extend_from_slice(&0i16.to_le_bytes());
        for &(_, _, x) in chars {
            out.extend_from_slice(&(if column { x } else { 0 }).to_le_bytes());
        }
    }
    out
}

#[test]
fn a_plain_file_measures_each_character_by_its_slot() {
    let file = plain(
        b"Alpha",
        &[(b'A', [50, 0, 650, 700], 700), (b' ', [0, 0, 0, 0], 250)],
    );
    let metrics = Metrics::read(&file).expect("sound");
    assert_eq!(metrics.name, b"Alpha");
    let a = metrics.measure(u32::from(b'A')).expect("A");
    assert_eq!(a.x_offset, 700);
    assert_eq!(
        a.bbox,
        Some(Box {
            x0: 50,
            y0: 0,
            x1: 650,
            y1: 700
        })
    );
    assert_eq!(
        metrics.measure(u32::from(b' ')).map(|m| m.x_offset),
        Some(250)
    );
    assert_eq!(metrics.measure(u32::from(b'B')), None);
    assert_eq!(metrics.measure(300), None);
}

/// A flagged file with no map, no boxes and no y offsets: its x offsets,
/// then its miscellaneous area and kern pairs.
fn flagged(offsets: &[i16], kerns: &[u8], wide: bool) -> Vec<u8> {
    let slots = offsets.len();
    let mut flags = flag::MAP_SIZED | flag::NO_BOXES | flag::NO_Y_OFFSETS | flag::MORE;
    if wide {
        flags |= flag::WIDE_KERNS;
    }
    let mut out = b"Flagged".to_vec();
    out.resize(NAME, b'\r');
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&[
        u8::try_from(slots & 0xFF).expect("low"),
        FLAGGED,
        flags,
        u8::try_from(slots >> 8).expect("high"),
    ]);
    out.extend_from_slice(&0u16.to_le_bytes());
    for &x in offsets {
        out.extend_from_slice(&x.to_le_bytes());
    }
    let misc: [i16; 12] = [
        -10, -20, 900, 800, 500, 0, -170, 0x7F_F6, 300, 200, -200, 750,
    ];
    let misc_bytes: Vec<u8> = misc
        .iter()
        .enumerate()
        .flat_map(|(i, v)| {
            if i == 7 {
                vec![0xF6u8, 0x10]
            } else {
                v.to_le_bytes().to_vec()
            }
        })
        .chain([0u8; 4])
        .collect();
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(
        &u16::try_from(8 + misc_bytes.len())
            .expect("short")
            .to_le_bytes(),
    );
    let reserved = u16::try_from(8 + misc_bytes.len() + kerns.len()).expect("short");
    out.extend_from_slice(&reserved.to_le_bytes());
    out.extend_from_slice(&reserved.to_le_bytes());
    out.extend_from_slice(&misc_bytes);
    out.extend_from_slice(kerns);
    out
}

#[test]
fn a_flagged_file_gives_its_misc_area_and_kern_pairs() {
    let mut kerns = vec![b'A', b'V'];
    kerns.extend_from_slice(&(-80i16).to_le_bytes());
    kerns.push(b'W');
    kerns.extend_from_slice(&(-60i16).to_le_bytes());
    kerns.extend_from_slice(&[0, 0]);
    let offsets: Vec<i16> = (0..300).map(|i| i16::try_from(i).expect("small")).collect();
    let metrics = Metrics::read(&flagged(&offsets, &kerns, false)).expect("sound");
    assert!(!metrics.has_boxes());
    assert_eq!(
        metrics.measure(299).map(|m| m.x_offset),
        Some(299),
        "no map: the code is the slot"
    );
    let misc = metrics.misc.expect("misc");
    assert_eq!((misc.bbox.x0, misc.bbox.y1, misc.italic), (-10, 800, -170));
    assert_eq!(
        (misc.underline_position, misc.underline_thickness),
        (-10, 16)
    );
    assert_eq!(
        metrics.measure(5).map(|m| m.y_offset),
        Some(0),
        "the default y offset"
    );
    assert_eq!(
        metrics.kern(u16::from(b'A'), u16::from(b'V')).map(|k| k.x),
        Some(-80)
    );
    assert_eq!(
        metrics.kern(u16::from(b'A'), u16::from(b'W')).map(|k| k.x),
        Some(-60)
    );
    assert_eq!(metrics.kern(u16::from(b'V'), u16::from(b'A')), None);
    let mut wide = Vec::new();
    for v in [300u16, 301] {
        wide.extend_from_slice(&v.to_le_bytes());
    }
    wide.extend_from_slice(&(-5i16).to_le_bytes());
    wide.extend_from_slice(&[0, 0, 0, 0]);
    let metrics = Metrics::read(&flagged(&offsets, &wide, true)).expect("sound");
    assert_eq!(metrics.kern(300, 301).map(|k| k.x), Some(-5));
}

#[test]
fn a_file_that_does_not_hold_together_is_refused_whole() {
    let file = plain(b"Alpha", &[(b'A', [50, 0, 650, 700], 700)]);
    for cut in 0..file.len() {
        assert_eq!(Metrics::read(&file[..cut]), Err(Malformed), "cut at {cut}");
    }
    let mut version_1 = file.clone();
    version_1[VERSION] = 1;
    assert_eq!(Metrics::read(&version_1), Err(Malformed));
    let mut plain_with_flags = file.clone();
    plain_with_flags[FLAGS] = 1;
    assert_eq!(Metrics::read(&plain_with_flags), Err(Malformed));
    let mut reserved = file;
    reserved[VERSION] = FLAGGED;
    reserved[FLAGS] = flag::RESERVED;
    assert_eq!(Metrics::read(&reserved), Err(Malformed));
}

#[test]
fn every_byte_changed_to_every_value_is_read_without_fault() {
    let mut kerns = vec![b'A', b'V'];
    kerns.extend_from_slice(&(-80i16).to_le_bytes());
    kerns.extend_from_slice(&[0, 0]);
    let offsets: Vec<i16> = (0..20).collect();
    for file in [
        plain(
            b"Alpha",
            &[(b'A', [50, 0, 650, 700], 700), (b'B', [1, 2, 3, 4], 5)],
        ),
        flagged(&offsets, &kerns, false),
    ] {
        for at in 0..file.len() {
            for value in [0, 1, 0x7F, 0x80, 0xFF] {
                let mut bytes = file.clone();
                bytes[at] = value;
                if let Ok(metrics) = Metrics::read(&bytes) {
                    for code in 0..300 {
                        let _ = metrics.measure(code);
                    }
                }
            }
        }
    }
}

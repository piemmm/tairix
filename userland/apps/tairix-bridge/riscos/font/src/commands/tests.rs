use super::*;

#[test]
fn sizes_show_whole_points_then_quarters_and_a_plus_past_them() {
    let shown = |x, y| points_text((x, y));
    assert_eq!(shown(192, 192), b"12 point");
    assert_eq!(shown(320, 160), b"20x10 point");
    assert_eq!(shown(100, 100), b"6\xBC point");
    assert_eq!(shown(97, 97), b"6+ point");
    assert_eq!(shown(101, 101), b"6\xBC+ point");
    assert_eq!(shown(104, 104), b"6\xBD point");
    assert_eq!(shown(12, 12), b"0\xBE point");
    assert_eq!(shown(17, 17), b"1+ point");
    assert_eq!(shown(160_000, 160_000), b"10000 point");
}

#[test]
fn a_list_line_lays_its_columns_out_as_risc_os_5_30_s_do() {
    let line = |resolution: &[u8], size: Option<&[u8]>| {
        ListLine {
            handle: 25,
            identifier: b"Alpha".to_vec(),
            size: size.map(<[u8]>::to_vec),
            resolution: size.is_some().then(|| resolution.to_vec()),
            face: false,
            uses: 1,
            memory: 452,
        }
        .text(b"master, a TAIRiX face", b"master, outlines")
    };
    assert_eq!(
        line(b"90x90", Some(b"12 point")),
        b" 25. Alpha                  12 point             90x90        1    452 bytes"
    );
    assert_eq!(
        line(b"4294967295x4294967295", Some(b"12 point")),
        b" 25. Alpha                  12 point             4294967295x4294967295   1   452 bytes"
    );
    assert_eq!(
        line(b"", None),
        b" 25. Alpha                  master, outlines                  1    452 bytes"
    );
}

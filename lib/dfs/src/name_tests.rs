use super::*;

#[test]
fn a_name_dfs_makes_holds_printable_bytes_but_those_its_commands_read() {
    assert_eq!(spelled(b"ELITE"), Some(*b"ELITE  "));
    assert_eq!(spelled(b"A/B"), Some(*b"A/B    "));
    assert_eq!(spelled(b"!BOOT"), Some(*b"!BOOT  "));
    assert_eq!(spelled(b"SEVENCH"), Some(*b"SEVENCH"));
    for refused in [
        &b""[..],
        b"EIGHTCHR",
        b"A.B",
        b"A:B",
        b"A\"B",
        b"A#B",
        b"A*B",
        b"A B",
        b"A\x7FB",
        b"\x80",
    ] {
        assert_eq!(spelled(refused), None, "{refused:?}");
    }
}

#[test]
fn a_name_read_is_believed_of_any_printable_bytes_its_top_bits_aside() {
    assert!(readable(b' '));
    assert!(readable(b'.'));
    assert!(readable(b'A' | 0x80));
    assert!(!readable(0x0D));
    assert!(!readable(0x7F));
    assert!(!readable(0xFF));
}

#[test]
fn a_spelling_s_padding_is_taken_off_its_top_bits_aside() {
    assert_eq!(unpadded_len(b"ELITE  "), 5);
    assert_eq!(unpadded_len(b"SEVENCH"), 7);
    assert_eq!(
        unpadded_len(&[b'A', b' ' | 0x80, b' ', b' ', b' ', b' ', b' ']),
        1
    );
    assert_eq!(unpadded_len(&[b' '; NAME_LEN]), 0);
}

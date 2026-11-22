use super::*;

fn info(kind: u32, attributes: u32) -> Info {
    Info {
        kind,
        load: 0xFFFF_FD45,
        exec: 0x6789_0ABC,
        length: 1000,
        attributes,
    }
}

#[test]
fn attributes_show_as_risc_os_5_shows_them() {
    assert_eq!(attributes(&info(A_FILE, 0x33)), b"WR/WR");
    assert_eq!(attributes(&info(A_FILE, 0x03)), b"WR/");
    assert_eq!(attributes(&info(A_FILE, 0x3B)), b"LWR/WR");
    assert_eq!(attributes(&info(DIRECTORY, 0x03)), b"DWR/");
    assert_eq!(attributes(&info(call::IMAGE, 0x33)), b"DWR/WR");
    assert_eq!(attributes(&info(A_FILE, 0)), b"/");
}

#[test]
fn a_name_column_holds_its_longest_name_and_a_space() {
    assert_eq!(name_column(4), NAME_COLUMN);
    assert_eq!(name_column(20), 21);
}

#[test]
fn numbers_show_in_decimal() {
    let mut digits = Vec::new();
    decimal(&mut digits, 0);
    decimal(&mut digits, 4_294_967_295);
    assert_eq!(digits, b"04294967295");
    assert_eq!(two_digits(2), *b"02");
    assert_eq!(two_digits(31), *b"31");
}

#[test]
fn padding_never_cuts_what_it_pads() {
    let mut line = Vec::new();
    pad(&mut line, b"onef", 13);
    assert_eq!(line.len(), 13);
    pad(&mut line, b"longer than its field", 4);
    assert_eq!(line.len(), 13 + 21);
}

#[test]
fn only_a_typed_load_address_holds_a_stamp() {
    assert!(typed(&info(A_FILE, 0)));
    let mut untyped = info(A_FILE, 0);
    untyped.load = 0x8000;
    assert!(!typed(&untyped));
}

use super::*;

#[test]
fn a_types_variable_names_it_in_three_capital_hex_digits() {
    assert_eq!(&variable_of(0xFFF), b"File$Type_FFF");
    assert_eq!(&variable_of(0x0AB), b"File$Type_0AB");
    assert_eq!(&variable_of(0x000), b"File$Type_000");
}

#[test]
fn a_type_is_up_to_three_hex_digits_with_or_without_an_ampersand() {
    assert_eq!(hex(b"&FEB"), Some(0xFEB));
    assert_eq!(hex(b"FEB"), Some(0xFEB));
    assert_eq!(hex(b"fe"), Some(0xFE));
    assert_eq!(hex(b"&FFFF"), None);
    assert_eq!(hex(b"&"), None);
    assert_eq!(hex(b""), None);
    assert_eq!(hex(b"XYZ"), None);
}

//! The pure rules of whole files: stamps and the types `OS_File` 20 gives.

use super::{object_type, type_of, typed_stamp, APPLICATION_TYPE, DIRECTORY_TYPE, UNTYPED};
use crate::fileswitch::call::{DIRECTORY, FILE, NOT_FOUND};

#[test]
fn a_typed_stamp_holds_the_type_and_the_five_byte_time() {
    assert_eq!(
        typed_stamp(0xFFD, 0x5D_393A_D30C),
        (0xFFFF_FD5D, 0x393A_D30C)
    );
    assert_eq!(typed_stamp(0x1FFD, 0), (0xFFFF_FD00, 0));
}

#[test]
fn a_load_address_types_a_file_only_where_its_top_bits_say_so() {
    assert_eq!(type_of(0xFFFF_FD45), 0xFFD);
    assert_eq!(type_of(0x0000_8000), UNTYPED);
}

#[test]
fn osfile_20_types_directories_applications_and_untyped_files() {
    assert_eq!(object_type(FILE, 0xFFFF_FB00, b"x"), 0xFFB);
    assert_eq!(object_type(FILE, 0x8000, b"x"), UNTYPED);
    assert_eq!(object_type(DIRECTORY, 0, b"!App"), APPLICATION_TYPE);
    assert_eq!(object_type(DIRECTORY, 0, b"Dir"), DIRECTORY_TYPE);
    assert_eq!(object_type(NOT_FOUND, 0, b"x"), UNTYPED);
    assert_eq!(UNTYPED, u32::MAX);
}

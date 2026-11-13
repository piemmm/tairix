//! The DFS's own checks; its behaviour against DFS 1.20 is the recorded
//! corpus's, replayed in `tests/corpus.rs`.

use super::build;

mod timing;

#[test]
fn the_header_names_a_service_rom() {
    let image = build().expect("the DFS assembles");
    let bytes = &image.bytes;
    assert_eq!(bytes.len(), 0x4000);
    assert_eq!(&bytes[..3], &[0, 0, 0], "no language entry");
    assert_eq!(bytes[6], 0x82, "a service ROM of 6502 code");
    assert_eq!(&bytes[9..12], b"DFS");
    let copyright = usize::from(bytes[7]);
    assert_eq!(&bytes[copyright..copyright + 4], b"\0(C)");
}

#[test]
fn the_nmi_routine_fits_below_the_extended_vectors() {
    let nmi = super::nmi::build().expect("the NMI routine assembles");
    assert!(nmi.bytes.len() <= usize::from(crate::rom::os::page_d::EXTENDED_VECTORS - 0x0D00));
}

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

/// The pause a drive found stopped is given is its loops' cycles, which a
/// branch taken across a page would stretch.
#[test]
fn the_spin_up_pause_loops_without_crossing_a_page() {
    let image = build().expect("the DFS assembles");
    // Each loop's label, and the bytes from it to the instruction after its
    // branch back.
    for (label, to_next) in [("spin_up_inner", 3), ("spin_up_outer", 8)] {
        let at = image.symbol(label).expect("the pause is placed");
        assert_eq!(
            at >> 8,
            (at + to_next) >> 8,
            "{label} at &{at:04X} branches across a page"
        );
    }
}

#[test]
fn the_nmi_routine_and_what_a_transfer_holds_fit_below_the_extended_vectors() {
    let nmi = super::nmi::build().expect("the NMI routine assembles");
    assert!(nmi.held.attempts < crate::rom::os::page_d::EXTENDED_VECTORS);
}

/// A transfer copies the sectors, address and bytes stored in one loop, so
/// they lie in what it holds as they do in the operation's workspace.
#[test]
fn the_held_run_lies_as_the_operation_s() {
    use super::ws::page;
    let nmi = super::nmi::build().expect("the NMI routine assembles");
    let held = &nmi.held;
    assert_eq!(
        held.address - held.sectors,
        page::OP_ADDRESS - page::OP_SECTORS
    );
    assert_eq!(
        held.wanted - held.sectors,
        page::OP_STORE - page::OP_SECTORS
    );
    assert_eq!(held.count - held.sectors, u16::from(page::OP_RUN_LEN));
}

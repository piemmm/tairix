//! BASIC from outside, on the emulated machine with the clean-room KERNAL.

mod programs;
mod reals;

#[test]
fn chrget_reads_at_chrgot_from_txtptr() {
    use super::chrget;
    use super::ws::zp;
    let code = chrget::code().expect("CHRGET assembles");
    assert_eq!(code.len(), usize::from(zp::RNDX - zp::CHRGET));
    let chrgot = usize::from(zp::CHRGOT - zp::CHRGET);
    assert_eq!(code[chrgot], 0xAD, "CHRGOT loads absolute");
    assert_eq!(
        usize::from(zp::TXTPTR - zp::CHRGET),
        chrgot + 1,
        "TXTPTR is its operand"
    );
}

#[test]
fn spaces_are_copied_past_the_keyword_search() {
    use alloc::format;

    use crate::machine::c64::keyboard::Key;
    use crate::machine::c64::Standard;
    use crate::rom::c64::system;
    use crate::rom::c64::testing::{machine, run_for, type_text, JIFFY};
    let system = system().expect("the BASIC and KERNAL ROMs assemble");
    let mut machine = machine(Standard::Pal, system.basic(), system.kernal());
    run_for(&mut machine, 4 * Standard::Pal.clock());
    type_text(&mut machine, &format!("10 A{}=1", " ".repeat(60)));
    machine.set_key(Key::RETURN, true);
    // Searching the keywords for each space would take some 350 000 cycles.
    let start = machine.cycles();
    let stored =
        |machine: &crate::machine::c64::Machine| machine.ram(0x0801) | machine.ram(0x0802) != 0;
    while !stored(&machine) {
        assert!(
            machine.cycles() - start < 4 * JIFFY,
            "the line stored within four jiffies"
        );
        run_for(&mut machine, 100);
    }
}

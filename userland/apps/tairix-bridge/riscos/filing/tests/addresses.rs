//! FileSwitch handed a buffer at the top of the address space: the transfer
//! fails as an access past the end of memory does, and the session goes on.

mod common;

use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::os::GBPB;
use tairix_riscos_kernel::swi::X;

const TOP: u32 = 0xFFFF_FF00;

#[test]
fn a_directory_read_into_the_top_of_memory_fails_and_the_session_goes_on() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    let name = os.rma_claim(16).expect("room for a name");
    os.write_terminated(name, b"Resources:$", 0, 12)
        .expect("the name");
    for reason in [10, 12] {
        let mut regs = Regs::default();
        regs.r[..7].copy_from_slice(&[reason, name, TOP, 4, 0, 0x1000, 0]);
        assert!(os.perform(X | GBPB, &mut regs).is_err(), "reason {reason}");
    }
    let buffer = os.rma_claim(256).expect("a buffer");
    let mut regs = Regs::default();
    regs.r[..7].copy_from_slice(&[12, name, buffer, 1, 0, 200, 0]);
    os.perform(X | GBPB, &mut regs).expect("a listing");
    assert_eq!(regs.r[3], 1);
}

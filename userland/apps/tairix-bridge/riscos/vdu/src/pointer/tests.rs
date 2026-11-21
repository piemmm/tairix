use tairix_riscos_filing::session::{session, Setup, SLOT_BYTES};

use super::word_21;
use crate::state::with;

#[test]
fn a_shape_block_ending_the_address_space_reads_on_from_its_start() {
    let mut s = session(&Setup {
        vdu: Some(crate::DRIVERS),
        modules: &crate::MODULES,
        bytes: SLOT_BYTES,
        keys: &[],
        disc: None,
    });
    let mut os = s.kernel.os();
    let before = with(&mut os, |vdu, _| Ok(vdu.pointer.clone())).expect("the pointer");
    assert_eq!(
        word_21(&mut os, 0, u32::MAX),
        Ok(()),
        "the header past the block's last byte is the null page's, as the processor wraps"
    );
    let after = with(&mut os, |vdu, _| Ok(vdu.pointer.clone())).expect("the pointer");
    assert_eq!(after, before, "the null page names no shape");
}

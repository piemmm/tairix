extern crate std;

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::vec::Vec;

use super::*;

/// A file whose pending enabled identities `claim` takes lowest-numbered
/// first, the order of priority `stopei` follows.
#[derive(Default)]
struct Model {
    regs: RefCell<BTreeMap<u32, u64>>,
    writes: RefCell<Vec<(u32, u64)>>,
}

impl Model {
    fn register(&self, register: u32) -> u64 {
        self.regs.borrow().get(&register).copied().unwrap_or(0)
    }

    fn write_raw(&self, register: u32, value: u64) {
        self.regs.borrow_mut().insert(register, value);
    }

    fn raise(&self, id: u32) {
        let register = reg::holding(reg::EIP0, id);
        let bits = self.register(register) | 1 << (id % 64);
        self.regs.borrow_mut().insert(register, bits);
    }
}

impl InterruptFile for &Model {
    fn read(&self, register: u32) -> u64 {
        self.register(register)
    }

    fn write(&self, register: u32, value: u64) {
        self.writes.borrow_mut().push((register, value));
        self.regs.borrow_mut().insert(register, value);
    }

    fn claim(&self) -> u32 {
        for id in 1..=MAX_IDENTITIES {
            let (pending, enabled) = (
                self.register(reg::holding(reg::EIP0, id)),
                self.register(reg::holding(reg::EIE0, id)),
            );
            let bit = 1 << (id % 64);
            if pending & enabled & bit != 0 {
                self.regs
                    .borrow_mut()
                    .insert(reg::holding(reg::EIP0, id), pending & !bit);
                return id;
            }
        }
        0
    }
}

#[test]
fn a_file_is_taken_over_cleared_and_open_then_delivers() {
    let model = Model::default();
    model.write_raw(reg::holding(reg::EIP0, 70), u64::MAX);
    let imsic = Imsic::take(&model, 255).unwrap();
    assert_eq!(imsic.ids(), 255);
    let writes = model.writes.borrow();
    assert_eq!(writes.first(), Some(&(reg::EIDELIVERY, 0)));
    assert_eq!(writes.last(), Some(&(reg::EIDELIVERY, 1)));
    for group in 0..4 {
        assert!(
            writes.contains(&(0x80 + 2 * group, 0)),
            "pending group {group} cleared"
        );
        assert!(
            writes.contains(&(0xC0 + 2 * group, u64::MAX)),
            "group {group} open"
        );
    }
    assert!(writes.contains(&(reg::EITHRESHOLD, 0)));
}

#[test]
fn pending_identities_are_claimed_lowest_first() {
    let model = Model::default();
    let imsic = Imsic::take(&model, 255).unwrap();
    for id in [200, 5, 130] {
        model.raise(id);
    }
    assert_eq!(imsic.claim(), Some(5));
    assert_eq!(imsic.claim(), Some(130));
    assert_eq!(imsic.claim(), Some(200));
    assert_eq!(imsic.claim(), None);
    assert_eq!(reg::holding(reg::EIE0, 130), 0xC4);
}

#[test]
fn a_count_outside_the_architectures_is_refused() {
    for ids in [0, 62, MAX_IDENTITIES + 1] {
        assert!(Imsic::take(&Model::default(), ids).is_none(), "{ids}");
    }
}

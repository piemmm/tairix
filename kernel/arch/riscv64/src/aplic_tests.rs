extern crate std;

use core::cell::RefCell;
use std::collections::BTreeMap;
use std::vec::Vec;

use super::*;

/// A domain delegated sources `1..=delegated`, logging every write.
struct Model {
    regs: RefCell<BTreeMap<usize, u32>>,
    writes: RefCell<Vec<(usize, u32)>>,
    delegated: u32,
    msi: bool,
}

impl Model {
    fn new(delegated: u32, msi: bool) -> Self {
        Self {
            regs: RefCell::new(BTreeMap::new()),
            writes: RefCell::new(Vec::new()),
            delegated,
            msi,
        }
    }

    fn register(&self, offset: usize) -> u32 {
        self.regs.borrow().get(&offset).copied().unwrap_or(0)
    }
}

impl AplicMmio for &Model {
    fn read32(&self, offset: usize) -> u32 {
        self.register(offset)
    }

    fn write32(&self, offset: usize, value: u32) {
        self.writes.borrow_mut().push((offset, value));
        let stored = match offset {
            regs::DOMAINCFG if !self.msi => value & !DOMAINCFG_DM,
            regs::DOMAINCFG => value,
            o if (4..0x1000).contains(&o) && o / 4 > self.delegated as usize => 0,
            _ => value,
        };
        self.regs.borrow_mut().insert(offset, stored);
    }
}

#[test]
fn a_domain_is_taken_over_in_msi_mode_with_every_source_quiet() {
    let model = Model::new(96, true);
    let aplic = Aplic::take_msi(&model, 96).unwrap();
    assert_eq!(aplic.sources(), 96);
    assert_eq!(model.register(regs::DOMAINCFG), DOMAINCFG_DM | DOMAINCFG_IE);
    let writes = model.writes.borrow();
    assert_eq!(
        writes.first(),
        Some(&(regs::DOMAINCFG, DOMAINCFG_DM)),
        "delivery off first"
    );
    assert_eq!(
        writes.last(),
        Some(&(regs::DOMAINCFG, DOMAINCFG_DM | DOMAINCFG_IE)),
        "on last"
    );
    for source in 1..=96 {
        assert!(writes.contains(&(regs::CLRIENUM, source)));
        assert!(writes.contains(&(regs::sourcecfg(source), 0)));
    }
}

#[test]
fn a_domain_that_will_not_deliver_by_msi_is_refused() {
    let model = Model::new(96, false);
    assert_eq!(Aplic::take_msi(&model, 96).err(), Some(AplicError::NotMsi));
    for sources in [0, MAX_APLIC_SOURCES + 1] {
        let model = Model::new(96, true);
        assert_eq!(
            Aplic::take_msi(&model, sources).err(),
            Some(AplicError::SourceOutOfRange)
        );
    }
}

#[test]
fn a_route_names_the_harts_file_and_identity_and_leaves_the_source_disabled() {
    let model = Model::new(96, true);
    let aplic = Aplic::take_msi(&model, 96).unwrap();
    model.writes.borrow_mut().clear();
    aplic.route(10, Sense::Level, 3, 10).unwrap();
    assert_eq!(model.register(regs::sourcecfg(10)), 6);
    assert_eq!(model.register(regs::target(10)), (3 << 18) + 10);
    assert_eq!(
        model.writes.borrow()[0],
        (regs::CLRIENUM, 10),
        "disabled before it is changed"
    );
    aplic.route(11, Sense::Edge, 0, 200).unwrap();
    assert_eq!(model.register(regs::sourcecfg(11)), 4);
    aplic.enable(10).unwrap();
    aplic.retrigger(10).unwrap();
    aplic.disable(10).unwrap();
    let writes = model.writes.borrow();
    let tail: Vec<_> = writes.iter().rev().take(3).rev().copied().collect();
    assert_eq!(
        tail,
        [
            (regs::SETIENUM, 10),
            (regs::SETIPNUM_LE, 10),
            (regs::CLRIENUM, 10)
        ]
    );
}

#[test]
fn a_source_not_delegated_or_out_of_range_and_a_bad_target_are_refused() {
    let model = Model::new(8, true);
    let aplic = Aplic::take_msi(&model, 96).unwrap();
    assert_eq!(
        aplic.route(9, Sense::Level, 0, 9),
        Err(AplicError::NotDelegated)
    );
    for source in [0, 97] {
        assert_eq!(
            aplic.route(source, Sense::Level, 0, 1),
            Err(AplicError::SourceOutOfRange)
        );
        assert_eq!(aplic.enable(source), Err(AplicError::SourceOutOfRange));
        assert_eq!(aplic.disable(source), Err(AplicError::SourceOutOfRange));
        assert_eq!(aplic.retrigger(source), Err(AplicError::SourceOutOfRange));
    }
    for (hart, identity) in [(1 << 14, 1), (0, 0), (0, 2048)] {
        assert_eq!(
            aplic.route(1, Sense::Edge, hart, identity),
            Err(AplicError::BadTarget)
        );
    }
}

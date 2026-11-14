use alloc::boxed::Box;

use super::Machine;
use crate::firmware::{BASIC_IMAGE, ELECTRON_OS_IMAGE, MOS_IMAGE};
use crate::machine::{bbc, electron, Model, CLOCK};
use crate::media::tape::Tape;
use crate::snapshot::SnapshotError;

fn model_b() -> Machine {
    let mut machine = bbc::Machine::new(MOS_IMAGE).expect("the MOS fits");
    machine.insert_rom(15, BASIC_IMAGE).expect("BASIC fits");
    Machine::ModelB(Box::new(machine))
}

fn electron() -> Machine {
    let mut machine = electron::Machine::new(ELECTRON_OS_IMAGE).expect("the OS fits");
    machine.fit_basic(BASIC_IMAGE).expect("BASIC fits");
    Machine::Electron(Box::new(machine))
}

#[test]
fn each_model_runs_and_lights_only_the_lamps_it_has() {
    for (mut machine, model) in [(model_b(), Model::ModelB), (electron(), Model::Electron)] {
        assert_eq!(machine.model(), model);
        machine.run_until(CLOCK);
        assert!(machine.cycles() >= CLOCK);
        let (caps, shift) = machine.lock_lights();
        assert!(caps, "{model:?} starts with CAPS LOCK on");
        assert!(!shift || model.has_shift_lock());
        assert!(!machine.shown().is_empty());
    }
}

#[test]
fn a_snapshot_restores_onto_its_own_model_alone() {
    let (mut b, mut e) = (model_b(), electron());
    b.run_until(CLOCK / 10);
    e.run_until(CLOCK / 10);
    let (from_b, from_e) = (b.snapshot().expect("taken"), e.snapshot().expect("taken"));
    assert_eq!(e.restore(&from_b), Err(SnapshotError::Model));
    assert_eq!(b.restore(&from_e), Err(SnapshotError::Model));
    assert_eq!(b.restore(&from_b), Ok(()));
    assert_eq!(e.restore(&from_e), Ok(()));
}

#[test]
fn the_deck_is_every_models() {
    for mut machine in [model_b(), electron()] {
        assert!(machine.insert_tape(Tape::default()).is_none());
        assert!(machine.holds_tape());
        machine.set_tape_record(true);
        assert!(machine.tape_record().0);
        assert!(machine.eject_tape().is_some());
        assert!(!machine.holds_tape());
    }
}

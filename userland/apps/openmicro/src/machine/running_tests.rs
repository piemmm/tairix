use alloc::boxed::Box;
use alloc::vec::Vec;

use super::{BadTape, Machine};
use crate::firmware::{
    BASIC_IMAGE, C64_BASIC_IMAGE, C64_CHARACTER_IMAGE, C64_KERNAL_IMAGE, ELECTRON_OS_IMAGE,
    MOS_IMAGE,
};
use crate::machine::c64::{self, sid, Standard};
use crate::machine::{bbc, electron, Model};
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

fn commodore(standard: Standard) -> Machine {
    let config = c64::Config {
        standard,
        sid: sid::Model::Mos6581,
    };
    let roms = c64::Roms {
        basic: C64_BASIC_IMAGE,
        kernal: C64_KERNAL_IMAGE,
        character: C64_CHARACTER_IMAGE,
    };
    Machine::C64(Box::new(
        c64::Machine::new(config, roms).expect("the ROMs fit"),
    ))
}

fn every_model() -> [Machine; 3] {
    [model_b(), electron(), commodore(Standard::Pal)]
}

/// A TAP image of `units`.
fn tap(units: &[u8]) -> Vec<u8> {
    let mut image = Vec::from(&b"C64-TAPE-RAW"[..]);
    image.extend([1, 0, 0, 0]);
    image.extend(u32::try_from(units.len()).expect("short").to_le_bytes());
    image.extend_from_slice(units);
    image
}

#[test]
fn each_model_runs_in_its_own_time_and_lights_only_the_lamps_it_has() {
    for mut machine in every_model() {
        let (model, second) = (machine.model(), machine.timing().clock());
        machine.run_until(second);
        assert!(machine.cycles() >= second);
        let (caps, shift) = machine.lock_lights();
        assert_eq!(caps, model != Model::C64, "{model:?}'s CAPS LOCK");
        assert!(!shift || model.has_shift_lock());
        assert!(!machine.shown().is_empty());
    }
    assert_eq!(
        commodore(Standard::Ntsc).timing(),
        Standard::Ntsc.timing(),
        "a C64 keeps its standard's time"
    );
}

#[test]
fn a_snapshot_restores_onto_its_own_model_alone() {
    let mut machines = every_model();
    let snapshots: Vec<Vec<u8>> = machines
        .iter_mut()
        .map(|machine| {
            machine.run_until(machine.timing().clock() / 10);
            machine.snapshot().expect("taken")
        })
        .collect();
    for (at, machine) in machines.iter_mut().enumerate() {
        for (from, snapshot) in snapshots.iter().enumerate() {
            let restored = machine.restore(snapshot);
            if at == from {
                assert_eq!(restored, Ok(()));
            } else {
                assert_eq!(restored, Err(SnapshotError::Model), "{at} from {from}");
            }
        }
    }
}

#[test]
fn every_deck_takes_its_own_model_s_tapes_and_hands_back_what_it_recorded() {
    let uef = Tape::default().encode_uef();
    for mut machine in every_model() {
        let (own, other) = if machine.model() == Model::C64 {
            (tap(&[0x30; 4]), uef.clone())
        } else {
            (uef.clone(), tap(&[0x30; 4]))
        };
        assert_eq!(machine.insert_tape(&other), Err(BadTape));
        assert!(!machine.holds_tape(), "nothing changed");
        assert_eq!(machine.insert_tape(&own), Ok(()));
        assert!(machine.holds_tape());
        assert_eq!(machine.recorded_tape(), None, "not recorded on");
        machine.rewind_tape();
        machine.eject_tape();
        assert!(!machine.holds_tape());
    }
}

#[test]
fn a_tape_moves_to_a_machine_of_its_model() {
    let mut old = commodore(Standard::Pal);
    let mut new = commodore(Standard::Pal);
    old.insert_tape(&tap(&[0x30])).expect("a TAP");
    old.move_tape(&mut new);
    assert!(!old.holds_tape());
    assert!(new.holds_tape());
}

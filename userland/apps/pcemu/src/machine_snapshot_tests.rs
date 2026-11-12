use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;
use crate::dos::harness::{floppy, image_of};
use crate::dos::setup::default_disk;
use crate::dos::Diskette;
use crate::storage::sparse::SparseDisk;
use crate::storage::{BlockDevice, MediaError, SECTOR};
use crate::time::NANOS_PER_SECOND as SECOND;

const FINGERPRINT: &[u8] = b"a machine of the tests";

const TEMPLATES: [Template; 4] = [
    Template::IbmPc,
    Template::IbmXt,
    Template::TurboXt,
    Template::Workstation186,
];

/// A disk two machines can be handed, so one may be given a copy of it as
/// the other has it.
#[derive(Clone)]
struct Shared(Rc<RefCell<SparseDisk>>);

impl BlockDevice for Shared {
    fn sectors(&self) -> u64 {
        self.0.borrow().sectors()
    }

    fn read_only(&self) -> bool {
        false
    }

    fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), MediaError> {
        self.0.borrow_mut().read(sector, out)
    }

    fn write(&mut self, sector: u64, data: &[u8; SECTOR]) -> Result<(), MediaError> {
        self.0.borrow_mut().write(sector, data)
    }
}

/// A machine of `template`, with the default disk where it has a fixed-disk
/// controller, holding `contents` where given; and that disk.
fn machine(template: Template, contents: Option<SparseDisk>) -> (Machine, Option<Shared>) {
    let config = template.config();
    let disk = config
        .fixed
        .map(|adapter| default_disk(adapter.recording, 0x2026_1111).unwrap());
    let mut m = crate::bios::machine(&config, [disk.as_ref().map(|d| d.kind), None]).unwrap();
    let shared = disk.map(|disk| {
        let shared = Shared(Rc::new(RefCell::new(contents.unwrap_or(disk.device))));
        m.board_mut()
            .attach_fixed(0, disk.kind, Box::new(shared.clone()), disk.low_level)
            .unwrap();
        shared
    });
    (m, shared)
}

/// Where two snapshots first differ.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

fn taken(m: &mut Machine) -> Vec<u8> {
    m.snapshot(FINGERPRINT).unwrap()
}

#[test]
fn a_restored_machine_runs_on_exactly_as_the_one_it_was_taken_of() {
    for template in TEMPLATES {
        let (mut a, disk) = machine(template, None);
        crate::bios::run_until(&mut a, 4 * SECOND);
        let snapshot = taken(&mut a);
        let copy = disk.map(|disk| disk.0.borrow().clone());
        let (mut b, _) = machine(template, copy);
        b.restore(&snapshot, FINGERPRINT).unwrap();
        assert_eq!(
            first_difference(&taken(&mut b), &snapshot),
            None,
            "{template:?} is restored as it was taken"
        );
        for until in [5 * SECOND, 7 * SECOND] {
            crate::bios::run_until(&mut a, until);
            crate::bios::run_until(&mut b, until);
            assert_eq!(
                first_difference(&taken(&mut a), &taken(&mut b)),
                None,
                "{template:?} runs on alike to {until}"
            );
        }
    }
}

#[test]
fn a_snapshot_of_another_build_or_cut_short_is_refused() {
    let (mut a, _) = machine(Template::IbmXt, None);
    let snapshot = taken(&mut a);
    let (mut b, _) = machine(Template::IbmXt, None);
    assert_eq!(b.restore(&snapshot, b"another"), Err(Torn::Mismatch));
    let mut later = snapshot.clone();
    later[4] ^= 1;
    assert_eq!(b.restore(&later, FINGERPRINT), Err(Torn::Mismatch));
    for len in [0, 3, 6, snapshot.len() / 2, snapshot.len() - 1] {
        assert!(b.restore(&snapshot[..len], FINGERPRINT).is_err(), "{len}");
    }
    let (mut other, _) = machine(Template::TurboXt, None);
    assert!(other.restore(&snapshot, FINGERPRINT).is_err());
}

#[test]
fn a_diskette_in_a_drive_is_not_kept_and_its_drive_says_it_changed() {
    use crate::cpu::Bus;
    let template = Template::TurboXt;
    let (mut a, disk) = machine(template, None);
    let diskette = floppy(image_of(&Diskette::K1440.volume(7), &[]));
    a.board_mut().insert_floppy(0, diskette).unwrap();
    crate::bios::run_until(&mut a, 3 * SECOND);
    let snapshot = taken(&mut a);
    let (mut b, _) = machine(template, disk.map(|disk| disk.0.borrow().clone()));
    b.restore(&snapshot, FINGERPRINT).unwrap();
    assert!(
        b.board_mut().eject_floppy(0).is_none(),
        "the drive is empty"
    );
    let at = b.cpu().cycles();
    // Drive A: selected and turning, as a program reads its change line.
    b.board_mut().output(0x3F2, 0x1C, at);
    assert_ne!(
        b.board_mut().input(0x3F7, at) & 0x80,
        0,
        "its change line is set"
    );
}

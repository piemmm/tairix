extern crate std;

use std::boxed::Box;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::vec::Vec;

use tairix_arch_riscv64::aplic::regs;
use tairix_kernel_irq::IrqTable;
use tairix_kernel_sec::ProcessId;

use super::*;

/// An APLIC domain whose registers hold what was written, and an IMSIC file
/// whose `stopei` hands back what a test raised, lowest first.
#[derive(Default)]
struct Hart {
    aplic: Mutex<BTreeMap<usize, u32>>,
    writes: Mutex<Vec<(usize, u32)>>,
    pending: Mutex<Vec<u32>>,
    rung: Mutex<Vec<u32>>,
}

impl AplicMmio for &'static Hart {
    fn read32(&self, offset: usize) -> u32 {
        self.aplic
            .lock()
            .unwrap()
            .get(&offset)
            .copied()
            .unwrap_or(0)
    }

    fn write32(&self, offset: usize, value: u32) {
        self.writes.lock().unwrap().push((offset, value));
        self.aplic.lock().unwrap().insert(offset, value);
    }
}

impl InterruptFile for &'static Hart {
    fn read(&self, _register: u32) -> u64 {
        0
    }

    fn write(&self, _register: u32, _value: u64) {}

    fn claim(&self) -> u32 {
        let mut pending = self.pending.lock().unwrap();
        pending.sort_unstable();
        if pending.is_empty() {
            0
        } else {
            pending.remove(0)
        }
    }
}

impl Doorbell for &'static Hart {
    fn ring(&self, identity: u32) {
        self.rung.lock().unwrap().push(identity);
    }
}

type Controller = AiaIrqController<&'static Hart, &'static Hart, &'static Hart>;

fn controller(sources: u32, ids: u32) -> (&'static Hart, &'static Controller) {
    let hart: &'static Hart = Box::leak(Box::default());
    let aplic = Aplic::take_msi(hart, sources).unwrap();
    let imsic = Imsic::take(hart, ids).unwrap();
    let controller = Box::leak(Box::new(
        AiaIrqController::new(aplic, imsic, hart, 2).unwrap(),
    ));
    (hart, controller)
}

fn files(count: usize) -> &'static [Mrif] {
    let files: Vec<Mrif> = (0..count)
        .map(|_| Mrif {
            words: core::array::from_fn(|_| AtomicU64::new(u64::MAX)),
        })
        .collect();
    Box::leak(files.into_boxed_slice())
}

#[test]
fn a_source_is_given_an_identity_when_first_armed_and_raises_its_line() {
    let (hart, controller) = controller(96, 63);
    controller.set_trigger(10, Trigger::Level).unwrap();
    controller.set_trigger(36, Trigger::Edge).unwrap();
    assert_eq!(
        hart.read32(regs::target(10)),
        (2 << 18) + 1,
        "the first identity, to hart 2"
    );
    assert_eq!(hart.read32(regs::target(36)), (2 << 18) + 2);
    assert_eq!(hart.read32(regs::sourcecfg(10)), 6);
    assert_eq!(hart.read32(regs::sourcecfg(36)), 4);
    controller.set_trigger(10, Trigger::Level).unwrap();
    assert_eq!(
        hart.read32(regs::target(10)),
        (2 << 18) + 1,
        "kept on re-routing"
    );

    let table = IrqTable::new(controller.max_line());
    let bound = table.bind(10, ProcessId(7)).unwrap();
    hart.pending.lock().unwrap().extend([2, 1, 40]);
    assert!(controller.dispatch(&table));
    assert!(table.ready_for(bound.handle), "identity 1 raised line 10");
    assert!(
        hart.writes.lock().unwrap().contains(&(regs::CLRIENUM, 10)),
        "masked as it fired"
    );
}

#[test]
fn rearming_a_level_source_pends_it_again_and_an_edge_does_not() {
    let (hart, controller) = controller(96, 63);
    controller.set_trigger(5, Trigger::Level).unwrap();
    controller.set_trigger(6, Trigger::Edge).unwrap();
    hart.writes.lock().unwrap().clear();
    controller.rearm(5).unwrap();
    controller.rearm(6).unwrap();
    assert_eq!(
        *hart.writes.lock().unwrap(),
        [
            (regs::SETIENUM, 5),
            (regs::SETIPNUM_LE, 5),
            (regs::SETIENUM, 6)
        ]
    );
}

#[test]
fn identities_run_out_rather_than_alias() {
    let (_, controller) = controller(96, 63);
    for source in 1..=63 {
        controller.set_trigger(source, Trigger::Level).unwrap();
    }
    assert_eq!(
        controller.set_trigger(64, Trigger::Level),
        Err(MaskError::Unsupported)
    );
    assert_eq!(controller.rearm(64), Err(MaskError::Unsupported));
    for line in [0, 97, MESSAGE_LINE_BASE] {
        assert_eq!(controller.mask(line), Err(MaskError::OutOfRange), "{line}");
    }
}

#[test]
fn a_devices_file_raises_its_vector_only_while_enabled_and_never_another() {
    let (hart, controller) = controller(96, 255);
    controller.set_trigger(3, Trigger::Level).unwrap();
    let files = files(2);
    let first = controller.take_files(files).unwrap();
    assert_eq!(first, 2, "after the source's identity");
    assert_eq!(controller.max_line(), MESSAGE_LINE_BASE + 1);
    assert!(controller.take_files(files).is_none(), "taken once");
    assert_eq!(
        files[0].enable().load(Ordering::Relaxed),
        0,
        "every vector starts disabled"
    );

    let table = IrqTable::new(controller.max_line());
    let a = table.bind(MESSAGE_LINE_BASE, ProcessId(1)).unwrap();
    let b = table.bind(MESSAGE_LINE_BASE + 1, ProcessId(2)).unwrap();
    controller.rearm(MESSAGE_LINE_BASE).unwrap();
    controller.rearm(MESSAGE_LINE_BASE + 1).unwrap();

    // Device 0 writes an identity it was not given: its file's notice comes,
    // and nothing is raised.
    files[0].pending().fetch_or(1 << 7, Ordering::SeqCst);
    hart.pending.lock().unwrap().push(first);
    assert!(!controller.dispatch(&table));
    assert!(!table.ready_for(a.handle));
    // Its own vector raises its line alone, and is masked as it fires.
    files[0].pending().fetch_or(VECTOR_BIT, Ordering::SeqCst);
    hart.pending.lock().unwrap().push(first);
    assert!(controller.dispatch(&table));
    assert!(table.ready_for(a.handle) && !table.ready_for(b.handle));
    assert_eq!(
        files[0].pending().load(Ordering::SeqCst) & VECTOR_BIT,
        0,
        "taken"
    );
    assert_eq!(files[0].enable().load(Ordering::SeqCst), 0, "masked");

    // Raised while masked, it sends no notice the controller acts on; the
    // rearm rings the notice so it is not lost.
    files[0].pending().fetch_or(VECTOR_BIT, Ordering::SeqCst);
    hart.pending.lock().unwrap().push(first);
    assert!(!controller.dispatch(&table));
    controller.rearm(MESSAGE_LINE_BASE).unwrap();
    assert_eq!(*hart.rung.lock().unwrap(), [first]);
}

#[test]
fn files_beyond_the_identities_left_are_refused() {
    let (_, controller) = controller(96, 63);
    assert!(controller.take_files(files(64)).is_none());
    assert!(controller.take_files(&[]).is_none());
    assert_eq!(
        controller.take_files(files(63)),
        Some(1),
        "every identity a notice"
    );
    assert_eq!(
        controller.set_trigger(1, Trigger::Level),
        Err(MaskError::Unsupported)
    );
}

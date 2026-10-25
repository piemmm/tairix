//! The line server driven over a register model of the controller, answered
//! through the shared fake kernel.

use std::cell::Cell;
use std::vec::Vec;

use tairix_abi::driver::irqline::{
    decode_reply, IrqLineRequest, INTERRUPT_LINE_ENDPOINTS, IRQLINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{
    CapabilityId, DriverError, DriverHost, DriverKind, Errno, ProcId, RegisterBlock, PROC_ID_LEN,
};
use tairix_drvrt::irqline::LineRecord;
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::Supplier;

use crate::controller::{Controller, Layout, LINES};
use crate::{register, BIND_KEYS, LAYOUTS};

const HDMI: ProcId = ProcId::from_raw([1; PROC_ID_LEN]);
const OTHER: ProcId = ProcId::from_raw([2; PROC_ID_LEN]);

fn endpoint() -> u64 {
    INTERRUPT_LINE_ENDPOINTS.endpoint(21)
}

fn link(line: u32) -> LinkRequest {
    LinkRequest::new(endpoint(), 4, &[line], b"").expect("valid")
}

/// The controller's registers: latched status (edge) or the live inputs
/// (level), and the output mask.
struct L2 {
    layout: Layout,
    edge: bool,
    status: Cell<u32>,
    inputs: Cell<u32>,
    mask: Cell<u32>,
    /// Fault every unmask, as a bus that refuses the write would.
    refuse_unmask: Cell<bool>,
}

impl L2 {
    fn new(layout: Layout) -> Self {
        Self {
            layout,
            edge: layout == Layout::EDGE,
            status: Cell::new(0),
            inputs: Cell::new(0),
            mask: Cell::new(0),
            refuse_unmask: Cell::new(false),
        }
    }

    /// Raise `line`: an edge latches; a level holds until lowered.
    fn raise(&self, line: u32) {
        if self.edge {
            self.status.set(self.status.get() | 1 << line);
        } else {
            self.inputs.set(self.inputs.get() | 1 << line);
        }
    }

    fn lower(&self, line: u32) {
        self.inputs.set(self.inputs.get() & !(1 << line));
    }

    fn latched(&self) -> u32 {
        if self.edge {
            self.status.get()
        } else {
            self.inputs.get()
        }
    }

    /// Whether the controller drives its own line.
    fn asserted(&self) -> bool {
        self.latched() & !self.mask.get() != 0
    }

    fn masked(&self, line: u32) -> bool {
        self.mask.get() & 1 << line != 0
    }
}

impl RegisterBlock for L2 {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        Ok(if offset == 0 {
            self.latched()
        } else if offset == mask_status(self.layout) {
            self.mask.get()
        } else {
            panic!("a read of {offset:#x}")
        })
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        match (self.edge, offset) {
            (true, 0x08) => self.status.set(self.status.get() & !value),
            (true, 0x10) | (false, 0x08) => self.mask.set(self.mask.get() | value),
            (true, 0x14) | (false, 0x0C) if self.refuse_unmask.get() => {
                return Err(DriverError::DeviceFault);
            }
            (true, 0x14) | (false, 0x0C) => self.mask.set(self.mask.get() & !value),
            _ => panic!("a write of {value:#x} to {offset:#x}"),
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.layout.window_len()
    }
}

fn mask_status(layout: Layout) -> usize {
    if layout == Layout::EDGE {
        0x0C
    } else {
        0x04
    }
}

struct Rig<'r> {
    kernel: FakeKernel,
    records: Records<LineRecord>,
    controller: Controller<'r, FakeKernel, Records<LineRecord>>,
}

fn rig(l2: &L2) -> Rig<'_> {
    let kernel = FakeKernel::new();
    kernel.grant(HDMI, HwResource::request(&link(4)));
    kernel.grant(HDMI, HwResource::request(&link(5)));
    kernel.grant(OTHER, HwResource::request(&link(4)));
    let records = Records::default();
    let controller = Controller::new(l2, l2.layout, kernel.clone(), records.clone(), endpoint())
        .expect("the registers answer");
    Rig {
        kernel,
        records,
        controller,
    }
}

impl Rig<'_> {
    fn post(&mut self, caller: ProcId, line: LinkRequest) -> u64 {
        let ticket = self.kernel.call(caller);
        let mut frame = [0u8; IRQLINE_MAX_REQUEST];
        let len = IrqLineRequest::Wait(line)
            .encode(&mut frame)
            .expect("encodes");
        self.controller.serve(ticket, &frame[..len]);
        ticket
    }

    fn answer(&self, ticket: u64) -> Option<Result<(), Errno>> {
        self.kernel.answer(ticket).map(|frame| decode_reply(&frame))
    }

    fn records(&self) -> Vec<LineRecord> {
        self.records.all()
    }
}

#[test]
fn bring_up_masks_every_line_and_clears_what_a_predecessor_latched() {
    let l2 = L2::new(Layout::EDGE);
    l2.raise(3);
    l2.mask.set(0);
    let _rig = rig(&l2);
    assert_eq!(l2.mask.get(), u32::MAX);
    assert_eq!(l2.status.get(), 0);
    assert!(!l2.asserted());
}

#[test]
fn a_wait_unmasks_its_line_and_is_answered_when_it_fires() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    let ticket = rig.post(HDMI, link(4));
    assert_eq!(rig.answer(ticket), None);
    assert!(!l2.masked(4));
    assert!(l2.masked(5), "only the waited line is unmasked");
    l2.raise(4);
    assert!(l2.asserted());
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(ticket), Some(Ok(())));
    assert!(l2.masked(4), "masked again until the next wait");
    assert_eq!(l2.status.get(), 0, "the edge acknowledged");
    assert!(!l2.asserted());
}

#[test]
fn an_edge_arriving_while_masked_answers_the_next_wait_at_once() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    let first = rig.post(HDMI, link(4));
    l2.raise(4);
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(first), Some(Ok(())));
    l2.raise(4);
    assert!(!l2.asserted(), "latched, but masked");
    let second = rig.post(HDMI, link(4));
    assert!(l2.asserted(), "the wait's unmask raises it");
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(second), Some(Ok(())));
}

#[test]
fn a_level_line_cannot_storm_before_its_consumer_waits_again() {
    let l2 = L2::new(Layout::LEVEL);
    let mut rig = rig(&l2);
    let first = rig.post(HDMI, link(4));
    l2.raise(4);
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(first), Some(Ok(())));
    assert!(!l2.asserted(), "still high, but masked");
    rig.controller
        .woke(0)
        .expect("a spurious service finds nothing");
    l2.lower(4);
    let second = rig.post(HDMI, link(4));
    assert_eq!(rig.answer(second), None);
    assert!(!l2.asserted());
}

#[test]
fn a_wait_counts_only_for_a_line_the_kernel_attests_the_caller_holds() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    let unheld = rig.post(HDMI, link(6));
    assert_eq!(rig.answer(unheld), Some(Err(Errno::PermissionDenied)));
    let foreign =
        LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(3), 0, &[4], b"").expect("valid");
    let elsewhere = rig.post(HDMI, foreign);
    assert_eq!(rig.answer(elsewhere), Some(Err(Errno::OutOfRange)));
    for selector in [&[32][..], &[4, 1][..]] {
        let odd = LinkRequest::new(endpoint(), 0, selector, b"").expect("valid");
        rig.kernel.grant(HDMI, HwResource::request(&odd));
        let ticket = rig.post(HDMI, odd);
        assert_eq!(rig.answer(ticket), Some(Err(Errno::OutOfRange)));
    }
    assert_eq!(l2.mask.get(), u32::MAX, "nothing was unmasked");
    assert!(rig.records().iter().all(|record| matches!(
        record,
        LineRecord::Refused {
            caller: Some(HDMI),
            ..
        }
    )));
}

#[test]
fn a_line_stays_with_a_live_holder_and_is_reclaimed_from_an_ended_one() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    let held = rig.post(HDMI, link(4));
    let refused = rig.post(OTHER, link(4));
    assert_eq!(rig.answer(refused), Some(Err(Errno::Busy)));
    rig.kernel.end(HDMI);
    let taken = rig.post(OTHER, link(4));
    assert_eq!(rig.answer(taken), None, "held for the line");
    assert_eq!(rig.answer(held), None, "the ended holder is not answered");
    assert!(rig.records().contains(&LineRecord::Reclaimed {
        line: 4,
        from: HDMI
    }));
    l2.raise(4);
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(taken), Some(Ok(())));
}

#[test]
fn a_wait_refused_because_its_unmask_failed_leaves_the_line_free_to_wait_on() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    l2.refuse_unmask.set(true);
    let refused = rig.post(HDMI, link(4));
    assert_eq!(rig.answer(refused), Some(Err(Errno::DeviceFault)));
    assert!(l2.masked(4));
    l2.refuse_unmask.set(false);
    let retried = rig.post(HDMI, link(4));
    assert_eq!(rig.answer(retried), None, "accepted, not busy");
    l2.raise(4);
    rig.controller.woke(0).expect("serviced");
    assert_eq!(rig.answer(retried), Some(Ok(())));
}

#[test]
fn a_second_wait_beside_a_posted_one_is_busy() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    rig.post(HDMI, link(4));
    let second = rig.post(HDMI, link(4));
    assert_eq!(rig.answer(second), Some(Err(Errno::Busy)));
}

#[test]
fn an_ended_consumer_has_its_lines_masked_and_released() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    rig.post(HDMI, link(4));
    rig.post(HDMI, link(5));
    assert!(!l2.masked(4) && !l2.masked(5));
    rig.controller.peer_exited(HDMI);
    assert!(l2.masked(4) && l2.masked(5));
    assert!(rig.records().contains(&LineRecord::Released { line: 4 }));
    assert!(rig.records().contains(&LineRecord::Released { line: 5 }));
    assert!(!rig.kernel.watched().contains(&HDMI));
    let next = rig.post(OTHER, link(4));
    assert_eq!(
        rig.answer(next),
        None,
        "the line is free for its next holder"
    );
}

#[test]
fn a_firing_its_consumer_cannot_hear_leaves_the_line_masked() {
    let l2 = L2::new(Layout::EDGE);
    let mut rig = rig(&l2);
    let ticket = rig.post(HDMI, link(4));
    rig.kernel.retire(ticket);
    l2.raise(4);
    rig.controller.woke(0).expect("serviced");
    assert!(l2.masked(4));
    assert!(rig.records().contains(&LineRecord::Unanswered { line: 4 }));
}

#[test]
fn the_bind_table_pairs_every_key_with_its_layout() {
    assert_eq!(BIND_KEYS.len(), LAYOUTS.len());
    let node = [
        tairix_abi::HwMatchKey::compatible(b"brcm,bcm2711-l2-intc").expect("fits"),
        tairix_abi::HwMatchKey::compatible(b"brcm,l2-intc").expect("fits"),
    ];
    let matched = tairix_devmatch::matched_bind_key(&node, BIND_KEYS).expect("matches");
    assert_eq!(LAYOUTS[matched], Layout::EDGE);
    let level = [tairix_abi::HwMatchKey::compatible(b"brcm,bcm7271-l2-intc").expect("fits")];
    let matched = tairix_devmatch::matched_bind_key(&level, BIND_KEYS).expect("matches");
    assert_eq!(LAYOUTS[matched], Layout::LEVEL);
    assert_eq!(LINES, 32);
}

#[test]
fn register_requires_drv_load() {
    struct Host(bool);
    impl DriverHost for Host {
        fn has_capability(&self, cap: CapabilityId) -> bool {
            self.0 && cap == CapabilityId::DRV_LOAD
        }
        fn kind(&self) -> DriverKind {
            DriverKind::UserSpace
        }
    }
    assert_eq!(
        register(&Host(false)).err(),
        Some(DriverError::PermissionDenied)
    );
    assert!(register(&Host(true)).is_ok());
}

//! The line holders driven over a model controller, answered through the
//! fake kernel.

use std::vec::Vec;

use tairix_abi::driver::irqline::{
    decode_reply, IrqLineRequest, INTERRUPT_LINE_ENDPOINTS, IRQLINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};

use crate::irqline::{IrqLines, LineHardware, LineRecord};
use crate::testkit::{FakeKernel, Records};

const CONSUMER: ProcId = ProcId::from_raw([1; PROC_ID_LEN]);
const OTHER: ProcId = ProcId::from_raw([2; PROC_ID_LEN]);

/// A controller of eight lines that records what it was asked.
#[derive(Default)]
struct Model {
    /// Each arming: the line, and whether its firing answers the wait.
    armed: Vec<(u8, bool)>,
    released: Vec<u8>,
    /// Refuse every arming, as a register that will not take the write.
    refuse_arm: bool,
}

impl LineHardware for Model {
    type Trigger = ();

    fn select(&self, selector: &[u32]) -> Result<(u8, ()), Errno> {
        let [line] = *selector else {
            return Err(Errno::OutOfRange);
        };
        u8::try_from(line)
            .ok()
            .filter(|&line| line < 8)
            .map(|line| (line, ()))
            .ok_or(Errno::OutOfRange)
    }

    fn arm(&mut self, line: u8, (): (), posted: bool) -> Result<(), Errno> {
        if self.refuse_arm {
            return Err(Errno::DeviceFault);
        }
        self.armed.push((line, posted));
        Ok(())
    }

    fn release(&mut self, line: u8, (): ()) {
        self.released.push(line);
    }
}

fn link(line: u32) -> LinkRequest {
    LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(5), 0, &[line], b"").expect("valid")
}

struct Rig {
    kernel: FakeKernel,
    records: Records<LineRecord>,
    model: Model,
    lines: IrqLines<(), 8>,
}

impl Rig {
    fn new() -> Self {
        let kernel = FakeKernel::new();
        for holder in [CONSUMER, OTHER] {
            kernel.grant(holder, HwResource::request(&link(3)));
        }
        Self {
            kernel,
            records: Records::default(),
            model: Model::default(),
            lines: IrqLines::new(INTERRUPT_LINE_ENDPOINTS.endpoint(5)),
        }
    }

    fn serve(&mut self, ticket: u64, request: LinkRequest) {
        let mut frame = [0u8; IRQLINE_MAX_REQUEST];
        let len = IrqLineRequest::Wait(request)
            .encode(&mut frame)
            .expect("fits");
        self.lines.serve(
            ticket,
            &frame[..len],
            &mut self.model,
            &mut self.kernel,
            &mut self.records,
        );
    }

    /// `caller` waits on `request`'s line; answers its ticket.
    fn wait(&mut self, caller: ProcId, request: LinkRequest) -> u64 {
        let ticket = self.kernel.call(caller);
        self.serve(ticket, request);
        ticket
    }

    fn answer(&self, ticket: u64) -> Option<Result<(), Errno>> {
        self.kernel.answer(ticket).map(|frame| decode_reply(&frame))
    }

    fn fire(&mut self, line: u8) {
        self.lines
            .fired(1 << line, &mut self.kernel, &mut self.records);
    }
}

#[test]
fn a_firing_answers_the_posted_wait_and_one_unwaited_answers_the_next_at_once() {
    let mut rig = Rig::new();
    let first = rig.wait(CONSUMER, link(3));
    assert_eq!(rig.answer(first), None);
    assert_eq!(rig.lines.held(3), Some(((), true)));
    rig.fire(3);
    assert_eq!(rig.answer(first), Some(Ok(())));
    rig.fire(3);
    let second = rig.wait(CONSUMER, link(3));
    assert_eq!(
        rig.answer(second),
        Some(Ok(())),
        "the latched firing answers at once"
    );
    assert_eq!(
        rig.model.armed,
        [(3, true), (3, false)],
        "and the line is armed again"
    );
    assert_eq!(rig.lines.held(3), Some(((), false)));
}

#[test]
fn a_line_stays_with_a_live_holder_and_is_taken_from_an_ended_one() {
    let mut rig = Rig::new();
    rig.wait(CONSUMER, link(3));
    let refused = rig.wait(OTHER, link(3));
    assert_eq!(rig.answer(refused), Some(Err(Errno::Busy)));
    rig.kernel.end(CONSUMER);
    let taken = rig.wait(OTHER, link(3));
    assert_eq!(rig.answer(taken), None, "held for the line");
    assert_eq!(rig.model.released, [3]);
    assert!(rig.records.all().ends_with(&[
        LineRecord::Released { line: 3 },
        LineRecord::Reclaimed {
            line: 3,
            from: CONSUMER
        },
        LineRecord::Claimed {
            line: 3,
            holder: OTHER
        },
    ]));
}

#[test]
fn a_wait_the_kernel_does_not_vouch_for_is_refused_before_anything_is_held() {
    let mut rig = Rig::new();
    rig.serve(FakeKernel::OUT_OF_SERVICE, link(3));
    let unheld = rig.wait(CONSUMER, link(4));
    let foreign =
        LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(6), 0, &[3], b"").expect("valid");
    let elsewhere = rig.wait(CONSUMER, foreign);
    assert_eq!(rig.answer(unheld), Some(Err(Errno::PermissionDenied)));
    assert_eq!(rig.answer(elsewhere), Some(Err(Errno::OutOfRange)));
    assert_eq!(
        rig.records.all(),
        [
            LineRecord::Refused {
                caller: None,
                reason: Errno::NotFound
            },
            LineRecord::Refused {
                caller: Some(CONSUMER),
                reason: Errno::PermissionDenied
            },
            LineRecord::Refused {
                caller: Some(CONSUMER),
                reason: Errno::OutOfRange
            },
        ]
    );
    assert!(rig.model.armed.is_empty() && !rig.lines.holds(CONSUMER));
}

#[test]
fn a_wait_refused_because_its_line_would_not_arm_leaves_the_line_free_to_wait_on() {
    let mut rig = Rig::new();
    rig.model.refuse_arm = true;
    let refused = rig.wait(CONSUMER, link(3));
    assert_eq!(rig.answer(refused), Some(Err(Errno::DeviceFault)));
    rig.model.refuse_arm = false;
    let retried = rig.wait(CONSUMER, link(3));
    assert_eq!(rig.answer(retried), None, "accepted, not busy");
    rig.fire(3);
    assert_eq!(rig.answer(retried), Some(Ok(())));
}

#[test]
fn an_ended_consumer_has_its_lines_released_and_is_no_longer_watched() {
    let mut rig = Rig::new();
    rig.kernel.grant(CONSUMER, HwResource::request(&link(4)));
    rig.wait(CONSUMER, link(3));
    rig.wait(CONSUMER, link(4));
    assert!(rig.lines.holds(CONSUMER) && rig.kernel.watched().contains(&CONSUMER));
    rig.lines
        .peer_exited(CONSUMER, &mut rig.model, &mut rig.kernel, &mut rig.records);
    assert_eq!(rig.model.released, [3, 4]);
    assert!(!rig.lines.holds(CONSUMER) && !rig.kernel.watched().contains(&CONSUMER));
}

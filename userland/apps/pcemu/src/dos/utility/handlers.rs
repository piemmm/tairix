//! The INT 23h and INT 24h handlers more than one utility installs.

use crate::dos::device::extended;
use crate::dos::engine::Saved;
use crate::dos::fs::critical;
use crate::dos::program::{self, Flow, Id, Program, Routine};
use crate::frames::{locals, Outcome};

locals! {
    /// INT 23h while a utility runs that must not be broken into: ignored,
    /// the call it broke carried on.
    pub struct IgnoreBreak {}
}

impl Routine for IgnoreBreak {
    const ID: Id = Id::IgnoreBreak;

    fn step(&mut self, _p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        Flow::Return(Ok(0))
    }
}

locals! {
    /// INT 23h while a utility runs whose work a break ends: the work is
    /// abandoned, its main routine resuming broken to end it as it would.
    pub struct AbandonOnBreak {}
}

impl Routine for AbandonOnBreak {
    const ID: Id = Id::AbandonOnBreak;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        p.abandon()
    }
}

locals! {
    /// INT 24h while a utility runs that says itself what failed: the call
    /// failed, or ignored where a failure may not be, or aborted where
    /// neither may. The device's error is kept at the program's
    /// [`program::at::CRITICAL`], since the call's own reports only that
    /// the handler failed it.
    pub struct FailCritical {}
}

impl Routine for FailCritical {
    const ID: Id = Id::FailCritical;

    fn step(&mut self, p: &mut Program<'_, '_>, _phase: &mut u8, _child: Outcome) -> Flow {
        let flags = p.caller(Saved::Ax).to_le_bytes()[1];
        let failed = extended(p.caller(Saved::Di)).to_le_bytes()[0];
        p.set8(program::at::CRITICAL, failed);
        let action = if flags & critical::FAIL != 0 {
            critical::ACTION_FAIL
        } else if flags & critical::IGNORE != 0 {
            critical::ACTION_IGNORE
        } else {
            critical::ACTION_ABORT
        };
        p.set_caller(Saved::Ax, u16::from_le_bytes([action, flags]));
        Flow::Return(Ok(0))
    }
}

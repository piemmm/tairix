//! A routine of the table that runs the machine for guest code: `EXPR`, and
//! `LVBLNK` of an array's element. The caller's registers and the machine's
//! own place are kept in a [`Kind::Routine`] record on the caller's stack,
//! so the machine evaluates the caller's text as any other and returns to
//! it, with what the routine gives, when the value is ready.

use tairix_riscos_kernel::os::Regs;

use super::next::{Delivered, Next, Payload};
use super::statements::cont;
use super::{Leave, Machine, Mode, Step};
use crate::messages::Error;
use crate::stack::Kind;

/// What a routine run for guest code gives back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum RoutineCall {
    /// `EXPR`: a value.
    Expr = 1,
    /// `LVBLNK`: an l-value.
    Lvalue = 2,
}

/// The words a routine record keeps: the machine's pointer, line, mode and
/// switches, then the caller's R0 to R14.
const KEPT: usize = 4 + 15;

impl Machine<'_, '_> {
    /// A routine for the code whose registers are `caller` begun at the text
    /// at `at`: its stack the caller's own, the machine's place kept.
    pub(crate) fn begin_routine(
        &mut self,
        _regs: &mut Regs,
        caller: [u32; 15],
        kind: RoutineCall,
        at: u32,
    ) -> Step<Next> {
        let sp = caller[13];
        if sp > self.sp || sp < self.fsa.saturating_add(super::MARGIN) {
            return Err(Error::StackCorrupt.into());
        }
        self.sp = sp;
        let mut kept = [0; KEPT];
        kept[..4].copy_from_slice(&[self.ptr, self.line, self.mode as u32, self.flags]);
        kept[4..].copy_from_slice(&caller);
        self.push(Kind::Routine, kind as u8, &kept)?;
        self.ptr = at;
        match kind {
            RoutineCall::Expr => self.evaluate(cont::ROUTINE, Payload::default()),
            RoutineCall::Lvalue => self.lvalue(cont::ROUTINE, Payload::default()),
        }
    }

    /// A routine's result: the machine's place taken up again and control
    /// handed back to the caller with what the routine gives.
    pub(crate) fn routine_done(&mut self, delivered: &Delivered) -> Step<Next> {
        let after = self.ptr;
        let mut kept = [0; KEPT];
        let header = self.pop_record(Kind::Routine, &mut kept)?;
        let kind = match header.aux {
            1 => RoutineCall::Expr,
            2 => RoutineCall::Lvalue,
            _ => return Err(Error::StackCorrupt.into()),
        };
        self.ptr = kept[0];
        self.line = kept[1];
        self.mode = super::Mode::of(kept[2]).unwrap_or(Mode::Away);
        self.flags = kept[3];
        let mut caller = [0; 15];
        caller.copy_from_slice(&kept[4..]);
        let mut regs = Regs::default();
        regs.r.copy_from_slice(&caller[..13]);
        let mut flags = 0;
        match (kind, delivered) {
            (RoutineCall::Expr, Delivered::Value(value)) => {
                crate::table::give(self, value, &mut regs)?;
                let next = self.os.read_byte(after)?;
                regs.r[10] = u32::from(next);
                regs.r[11] = after.wrapping_add(1);
                flags = crate::table::expr_flags(value);
            }
            (RoutineCall::Lvalue, Delivered::Lvalue(lvalue)) => {
                regs.r[0] = lvalue.address;
                regs.r[9] = lvalue.place.call_type(self.dialect());
            }
            _ => return Err(Error::StackCorrupt.into()),
        }
        let mut registers = caller;
        registers[..13].copy_from_slice(&regs.r);
        registers[13] = self.sp;
        Ok(Next::Leave(Leave::Return {
            pc: caller[14],
            regs: registers,
            flags,
        }))
    }
}

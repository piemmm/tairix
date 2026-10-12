//! The lines of one L2 controller, served over `irqline-v1`.
//!
//! The controller latches each of its thirty-two inputs in a status register
//! and gates its output by a mask, so a line is unmasked only while its
//! consumer waits: it fires, the controller is masked there and acknowledged,
//! and the wait is answered. An edge arriving while masked stays latched and
//! fires the moment the next wait unmasks it.

use tairix_abi::driver::irqline::{encode_reply, IrqLineRequest, IRQLINE_MAX_REPLY};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, RegisterBlock};
use tairix_drvrt::{Recorder, Supplier, SupplierHost};

/// The lines one controller collects.
pub const LINES: usize = 32;

/// Where one layout of the controller keeps its registers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    status: usize,
    /// Write one to clear a latched edge; a level controller has none.
    clear: Option<usize>,
    mask_status: usize,
    mask_set: usize,
    mask_clear: usize,
}

impl Layout {
    /// The edge-latching controller (`brcm,l2-intc` and its kin).
    pub const EDGE: Self = Self {
        status: 0x00,
        clear: Some(0x08),
        mask_status: 0x0C,
        mask_set: 0x10,
        mask_clear: 0x14,
    };

    /// The level controller (`brcm,bcm7271-l2-intc`).
    pub const LEVEL: Self = Self {
        status: 0x00,
        clear: None,
        mask_status: 0x04,
        mask_set: 0x08,
        mask_clear: 0x0C,
    };

    /// Bytes of register window the layout needs.
    #[must_use]
    pub const fn window_len(&self) -> usize {
        let end = if self.mask_clear > self.mask_set {
            self.mask_clear
        } else {
            self.mask_set
        };
        end + 4
    }
}

/// A decision the controller records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A line was claimed by its consumer.
    Claimed {
        /// The line.
        line: u8,
        /// Its consumer.
        owner: ProcId,
    },
    /// A line an ended consumer held was taken by its link's next holder.
    Reclaimed {
        /// The line.
        line: u8,
        /// The consumer that had held it.
        from: ProcId,
    },
    /// A request was refused.
    Refused {
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
    /// A line's consumer ended; the line was masked and released.
    Abandoned {
        /// The line.
        line: u8,
    },
    /// A line fired while its consumer could not be answered; it stays
    /// masked until the consumer waits again.
    Unanswered {
        /// The line.
        line: u8,
    },
}

struct Line {
    owner: ProcId,
    request: LinkRequest,
    posted: Option<u64>,
}

/// One controller's lines and the consumers waiting on them.
pub struct Controller<'r, S: SupplierHost, L: Recorder<Record>> {
    regs: &'r dyn RegisterBlock,
    layout: Layout,
    supplier: S,
    log: L,
    endpoint: u64,
    lines: [Option<Line>; LINES],
}

impl<'r, S: SupplierHost, L: Recorder<Record>> Controller<'r, S, L> {
    /// The controller behind `regs` in `layout`, serving `endpoint` and
    /// asking about its callers through `supplier`: every line masked and
    /// every latched edge cleared, so nothing a predecessor left armed can
    /// fire before its consumer waits.
    ///
    /// # Errors
    ///
    /// The register write that failed.
    pub fn new(
        regs: &'r dyn RegisterBlock,
        layout: Layout,
        supplier: S,
        log: L,
        endpoint: u64,
    ) -> Result<Self, DriverError> {
        regs.write32(layout.mask_set, u32::MAX)?;
        if let Some(clear) = layout.clear {
            regs.write32(clear, u32::MAX)?;
        }
        Ok(Self {
            regs,
            layout,
            supplier,
            log,
            endpoint,
            lines: [const { None }; LINES],
        })
    }

    fn wait(&mut self, ticket: u64, caller: ProcId, request: &LinkRequest) -> Result<(), Errno> {
        if request.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .supplier
            .caller_holds(ticket, &HwResource::request(request))?
        {
            return Err(Errno::PermissionDenied);
        }
        let line = match *request.selector() {
            [cell] => usize::try_from(cell)
                .ok()
                .filter(|&index| index < LINES)
                .ok_or(Errno::OutOfRange)?,
            _ => return Err(Errno::OutOfRange),
        };
        self.claim(line, caller, request)?;
        let held = self.lines[line].as_mut().ok_or(Errno::NotFound)?;
        if held.posted.is_some() {
            return Err(Errno::Busy);
        }
        held.posted = Some(ticket);
        // Unmasked only once the wait is held, so its firing finds it; a wait
        // refused here is answered by the refusal, never again by a firing.
        if let Err(err) = self.regs.write32(self.layout.mask_clear, 1 << line) {
            held.posted = None;
            return Err(err.as_errno());
        }
        Ok(())
    }

    /// Make `line` `caller`'s, as its link `request` allows.
    fn claim(&mut self, line: usize, caller: ProcId, request: &LinkRequest) -> Result<(), Errno> {
        let index = u8::try_from(line).map_err(|_| Errno::OutOfRange)?;
        if let Some(held) = &self.lines[line] {
            if held.owner == caller {
                return if held.request == *request {
                    Ok(())
                } else {
                    Err(Errno::Busy)
                };
            }
            // Two nodes may name the same line; it stays with a holder that
            // lives, and a watch refused as gone is the kernel saying it does
            // not.
            let from = held.owner;
            match self.supplier.watch(from) {
                Ok(()) => return Err(Errno::Busy),
                Err(Errno::NotFound) => {}
                Err(reason) => return Err(reason),
            }
            self.release(line);
            self.log.record(Record::Reclaimed { line: index, from });
        }
        self.supplier.watch(caller)?;
        self.lines[line] = Some(Line {
            owner: caller,
            request: *request,
            posted: None,
        });
        self.log.record(Record::Claimed {
            line: index,
            owner: caller,
        });
        Ok(())
    }

    fn release(&mut self, line: usize) {
        if self.lines[line].take().is_some() {
            let _ = self.regs.write32(self.layout.mask_set, 1 << line);
            if let Ok(line) = u8::try_from(line) {
                self.log.record(Record::Abandoned { line });
            }
        }
    }

    fn answer(&mut self, ticket: u64, outcome: Result<(), Errno>) -> bool {
        let mut out = [0u8; IRQLINE_MAX_REPLY];
        encode_reply(&mut out, outcome)
            .is_ok_and(|len| self.supplier.reply(ticket, &out[..len]).is_ok())
    }

    fn refuse(&mut self, ticket: u64, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { caller, reason });
        // A caller that ends before its answer arrives has nobody left to tell.
        let _ = self.answer(ticket, Err(reason));
    }
}

impl<S: SupplierHost, L: Recorder<Record>> Supplier for Controller<'_, S, L> {
    /// Answers at once unless the call is a wait, answered when its line
    /// fires.
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.supplier.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, reason),
        };
        let outcome = IrqLineRequest::decode(frame).and_then(|request| match request {
            IrqLineRequest::Wait(request) => self.wait(ticket, caller, &request),
        });
        if let Err(reason) = outcome {
            self.refuse(ticket, Some(caller), reason);
        }
    }

    /// The controller's own line, its only one: answer the wait on every
    /// unmasked line that fired, masking and acknowledging each first.
    fn woke(&mut self, _index: usize) -> Result<(), DriverError> {
        let fired =
            self.regs.read32(self.layout.status)? & !self.regs.read32(self.layout.mask_status)?;
        if fired == 0 {
            return Ok(());
        }
        self.regs.write32(self.layout.mask_set, fired)?;
        if let Some(clear) = self.layout.clear {
            self.regs.write32(clear, fired)?;
        }
        for line in bits(fired) {
            let Some(posted) = self.lines[usize::from(line)]
                .as_mut()
                .and_then(|held| held.posted.take())
            else {
                continue;
            };
            if !self.answer(posted, Ok(())) {
                self.log.record(Record::Unanswered { line });
            }
        }
        Ok(())
    }

    /// Release every line `peer` held, masking each.
    fn peer_exited(&mut self, peer: ProcId) {
        for index in 0..LINES {
            if self.lines[index]
                .as_ref()
                .is_some_and(|held| held.owner == peer)
            {
                self.release(index);
            }
        }
        self.supplier.unwatch(peer);
    }
}

/// The line numbers whose bits are set in `mask`, lowest first.
fn bits(mut mask: u32) -> impl Iterator<Item = u8> {
    core::iter::from_fn(move || {
        if mask == 0 {
            return None;
        }
        let line = mask.trailing_zeros();
        mask &= mask - 1;
        u8::try_from(line).ok()
    })
}

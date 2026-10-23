//! The controller's lines, served to two kinds of holder: a device the
//! hardware tree links to a line waits on it over `irqline-v1`, and a system
//! service holding `CAP_GPIO` drives a header line over `gpio-header-v1`. A
//! line has one holder at a time, of either kind.
//!
//! A level line is masked by disabling its detection as it fires and re-armed
//! by its consumer's next wait. An edge line's detection stays enabled while
//! held, and one that fires with no wait posted is latched here and answers
//! the next wait at once, so no edge is lost and no level can storm.

use tairix_abi::driver::gpio::{
    encode_lines, encode_status, HeaderLines, HeaderRequest, Holder, LineEntry, LineState,
    LineTable, Polarity, HEADER_MAX_REPLY, HEADER_REQUEST_LEN,
};
use tairix_abi::driver::irqline::{encode_reply, IrqLineRequest, IRQLINE_MAX_REPLY};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_drvrt::{refusal, Recorder, Supplier, SupplierHost};

use crate::block::{Block, Function, Trigger};

/// The most lines a controller of the family has.
pub const LINES_MAX: usize = 64;

/// The bank interrupts a controller raises: GPIO 0–27, 28–45, and 46 up.
pub const BANK_INTERRUPTS: usize = 3;

/// Header calls one wake serves before returning to the wait set, which
/// wakes again while more are queued: a flood cannot starve the banks.
pub const HEADER_CALLS_PER_WAKE: usize = 8;

/// The first line of each bank interrupt's group.
const BANK_FIRST_LINE: [u8; BANK_INTERRUPTS] = [0, 28, 46];

/// The header endpoint, as the controller serves it.
pub trait HeaderPort {
    /// Take the next call queued, answering its ticket and length; [`None`]
    /// when none is.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; the endpoint is lost.
    fn receive(&mut self, frame: &mut [u8]) -> Result<Option<(u64, usize)>, Errno>;

    /// The instance whose call `ticket` is in service.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, for a call no longer in service.
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno>;

    /// Answer the call `ticket` with `frame`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal; a caller that has ended cannot be answered.
    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno>;
}

/// Which protocol a line is held through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Role {
    /// An interrupt, over `irqline-v1`.
    Interrupt,
    /// A header output, over `gpio-header-v1`.
    Output,
}

/// A decision the controller records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A line was taken.
    Claimed {
        /// The line.
        line: u8,
        /// Its holder.
        holder: ProcId,
        /// How it is held.
        role: Role,
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
    /// A line was given back, by its holder or with its end.
    Released {
        /// The line.
        line: u8,
    },
    /// A line fired while its consumer could not be answered.
    Unanswered {
        /// The line.
        line: u8,
    },
}

enum Held {
    Interrupt {
        owner: ProcId,
        request: LinkRequest,
        trigger: Trigger,
        posted: Option<u64>,
        latched: bool,
    },
    Output {
        holder: ProcId,
        polarity: Polarity,
        asserted: bool,
    },
}

impl Held {
    const fn holder(&self) -> ProcId {
        match self {
            Self::Interrupt { owner, .. } => *owner,
            Self::Output { holder, .. } => *holder,
        }
    }
}

/// One controller's lines and who holds each.
pub struct Controller<'r, S: SupplierHost, H: HeaderPort, L: Recorder<Record>> {
    block: Block<'r>,
    links: S,
    header: Option<H>,
    lent: HeaderLines,
    banks: usize,
    interruptible: u64,
    log: L,
    endpoint: u64,
    lines: [Option<Held>; LINES_MAX],
}

impl<'r, S: SupplierHost, H: HeaderPort, L: Recorder<Record>> Controller<'r, S, H, L> {
    /// The controller behind `block`, serving its node's interrupt-line
    /// `endpoint` through `links` for the lines of its first `banks` bank
    /// interrupts, and lending `lent` through `header`: every detection
    /// disabled and every latched event cleared first.
    ///
    /// # Errors
    ///
    /// The register write that failed.
    pub fn new(
        block: Block<'r>,
        links: S,
        endpoint: u64,
        banks: usize,
        header: Option<(H, HeaderLines)>,
        log: L,
    ) -> Result<Self, DriverError> {
        block.quiesce()?;
        let (header, lent) = match header {
            Some((port, lent)) => (Some(port), lent),
            None => (None, HeaderLines::default()),
        };
        let last = block.lines().saturating_sub(1);
        let lines_mask = HeaderLines::span(0, last).mask();
        let interruptible = BANK_FIRST_LINE
            .iter()
            .zip(
                BANK_FIRST_LINE
                    .iter()
                    .skip(1)
                    .map(|&next| next - 1)
                    .chain([last]),
            )
            .take(banks)
            .fold(0, |mask, (&first, last)| {
                mask | HeaderLines::span(first, last).mask()
            })
            & lines_mask;
        Ok(Self {
            block,
            links,
            header,
            lent: HeaderLines::from_mask(lent.mask() & lines_mask),
            banks: banks.min(BANK_INTERRUPTS),
            interruptible,
            log,
            endpoint,
            lines: [const { None }; LINES_MAX],
        })
    }

    fn wait(&mut self, ticket: u64, caller: ProcId, request: &LinkRequest) -> Result<(), Errno> {
        if request.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .links
            .caller_holds(ticket, &HwResource::request(request))?
        {
            return Err(Errno::PermissionDenied);
        }
        let [line, trigger] = *request.selector() else {
            return Err(Errno::OutOfRange);
        };
        let line = u8::try_from(line)
            .ok()
            .filter(|&line| line < self.block.lines())
            .ok_or(Errno::OutOfRange)?;
        let trigger = Trigger::from_cell(trigger).ok_or(Errno::NotSupported)?;
        if self.interruptible & (1 << line) == 0 {
            return Err(Errno::NotSupported);
        }
        self.claim_interrupt(line, caller, request, trigger)?;
        let slot = usize::from(line);
        let (level, latched) = match &mut self.lines[slot] {
            Some(Held::Interrupt {
                trigger,
                posted,
                latched,
                ..
            }) => {
                if posted.is_some() {
                    return Err(Errno::Busy);
                }
                let latched = core::mem::take(latched);
                if !latched {
                    *posted = Some(ticket);
                }
                (trigger.is_level().then_some(*trigger), latched)
            }
            _ => return Err(Errno::NotFound),
        };
        if latched {
            self.answer_wait(ticket, Ok(()));
            return Ok(());
        }
        if let Some(level) = level {
            // Armed only once the wait is held, so its firing finds it.
            if let Err(err) = self.block.detect(line, Some(level)) {
                if let Some(Held::Interrupt { posted, .. }) = &mut self.lines[slot] {
                    *posted = None;
                }
                return Err(refusal(err));
            }
        }
        Ok(())
    }

    /// Make `line` `caller`'s interrupt, as its link `request` allows.
    fn claim_interrupt(
        &mut self,
        line: u8,
        caller: ProcId,
        request: &LinkRequest,
        trigger: Trigger,
    ) -> Result<(), Errno> {
        match &self.lines[usize::from(line)] {
            Some(Held::Interrupt {
                owner,
                request: held,
                ..
            }) if *owner == caller => {
                return if held == request {
                    Ok(())
                } else {
                    Err(Errno::Busy)
                };
            }
            Some(Held::Interrupt { owner, .. }) => {
                // Two nodes may name one line; it stays with a holder that
                // lives, and a watch refused as gone says it does not.
                let from = *owner;
                match self.links.watch(from) {
                    Ok(()) => return Err(Errno::Busy),
                    Err(Errno::NotFound) => {}
                    Err(reason) => return Err(reason),
                }
                self.release(line);
                self.log.record(Record::Reclaimed { line, from });
            }
            Some(Held::Output { .. }) => return Err(Errno::Busy),
            None => {}
        }
        if let Function::Alternate(_) = self.block.function(line).map_err(refusal)? {
            return Err(Errno::Busy);
        }
        self.links.watch(caller)?;
        self.block.set_direction(line, false).map_err(refusal)?;
        let detect = (!trigger.is_level()).then_some(trigger);
        self.block.detect(line, detect).map_err(refusal)?;
        self.lines[usize::from(line)] = Some(Held::Interrupt {
            owner: caller,
            request: *request,
            trigger,
            posted: None,
            latched: false,
        });
        self.log.record(Record::Claimed {
            line,
            holder: caller,
            role: Role::Interrupt,
        });
        Ok(())
    }

    /// Give `line` back: its detection off and its latch cleared, or an
    /// output returned to an input.
    fn release(&mut self, line: u8) {
        let Some(held) = self.lines[usize::from(line)].take() else {
            return;
        };
        // A register that will not take the write leaves nothing to do: the
        // line is no longer anybody's either way.
        let _ = match held {
            Held::Interrupt { .. } => self
                .block
                .detect(line, None)
                .and_then(|()| self.block.clear_events(1 << line)),
            Held::Output { .. } => self.block.set_direction(line, false),
        };
        self.log.record(Record::Released { line });
    }

    fn answer_wait(&mut self, ticket: u64, outcome: Result<(), Errno>) -> bool {
        let mut out = [0u8; IRQLINE_MAX_REPLY];
        encode_reply(&mut out, outcome)
            .is_ok_and(|len| self.links.reply(ticket, &out[..len]).is_ok())
    }

    /// Answer every held line whose event has latched.
    fn fired(&mut self) -> Result<(), DriverError> {
        let fired = self.block.events()?;
        if fired == 0 {
            return Ok(());
        }
        // A level stands until serviced, so its detection goes off before its
        // status is cleared, or it would latch again at once.
        for line in HeaderLines::from_mask(fired).iter() {
            let level = match &self.lines[usize::from(line)] {
                Some(Held::Interrupt { trigger, .. }) => trigger.is_level(),
                _ => true,
            };
            if level {
                self.block.detect(line, None)?;
            }
        }
        self.block.clear_events(fired)?;
        for line in HeaderLines::from_mask(fired).iter() {
            let Some(Held::Interrupt {
                posted, latched, ..
            }) = &mut self.lines[usize::from(line)]
            else {
                continue;
            };
            let Some(ticket) = posted.take() else {
                *latched = true;
                continue;
            };
            if !self.answer_wait(ticket, Ok(())) {
                self.log.record(Record::Unanswered { line });
            }
        }
        Ok(())
    }

    fn serve_header(&mut self, ticket: u64, frame: &[u8]) {
        let Some(port) = &self.header else {
            return;
        };
        let caller = match port.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse_header(ticket, None, reason),
        };
        let request = match HeaderRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse_header(ticket, Some(caller), reason),
        };
        let mut out = [0u8; HEADER_MAX_REPLY];
        let outcome = if request == HeaderRequest::Lines {
            self.table(caller)
                .map(|table| encode_lines(&mut out, Ok(&table)))
        } else {
            self.change(caller, request)
                .map(|()| encode_status(&mut out, Ok(())))
        };
        let answered = match outcome {
            Ok(answered) => answered,
            Err(reason) => return self.refuse_header(ticket, Some(caller), reason),
        };
        if let (Ok(len), Some(port)) = (answered, self.header.as_mut()) {
            // A caller that ends before its answer arrives has nobody left to
            // tell; its lines are released with its end.
            let _ = port.reply(ticket, &out[..len]);
        }
    }

    /// Every lent line and how it stands, as `caller` sees it.
    fn table(&self, caller: ProcId) -> Result<LineTable, Errno> {
        let mut table = LineTable::default();
        for line in self.lent.iter() {
            let state = match &self.lines[usize::from(line)] {
                Some(Held::Interrupt { .. }) => LineState::Interrupt,
                Some(Held::Output {
                    holder, asserted, ..
                }) => LineState::Output {
                    holder: if *holder == caller {
                        Holder::Caller
                    } else {
                        Holder::Other
                    },
                    asserted: *asserted,
                },
                None => match self.block.function(line).map_err(refusal)? {
                    Function::Alternate(function) => LineState::Peripheral { function },
                    Function::Input | Function::Output => LineState::Free,
                },
            };
            table.push(LineEntry { line, state })?;
        }
        Ok(table)
    }

    fn change(&mut self, caller: ProcId, request: HeaderRequest) -> Result<(), Errno> {
        match request {
            HeaderRequest::Lines => Ok(()),
            HeaderRequest::Claim {
                line,
                polarity,
                asserted,
            } => self.claim_output(caller, line, polarity, asserted),
            HeaderRequest::Drive { line, asserted } => {
                let polarity = self.own_output(caller, line)?;
                self.block
                    .drive(line, polarity.level(asserted))
                    .map_err(refusal)?;
                if let Some(Held::Output { asserted: held, .. }) =
                    &mut self.lines[usize::from(line)]
                {
                    *held = asserted;
                }
                Ok(())
            }
            HeaderRequest::Release { line } => {
                self.own_output(caller, line)?;
                self.release(line);
                self.unwatch_if_idle(caller);
                Ok(())
            }
        }
    }

    fn claim_output(
        &mut self,
        caller: ProcId,
        line: u8,
        polarity: Polarity,
        asserted: bool,
    ) -> Result<(), Errno> {
        if !self.lent.contains(line) {
            return Err(Errno::NotFound);
        }
        if self.lines[usize::from(line)].is_some() {
            return Err(Errno::Busy);
        }
        if let Function::Alternate(_) = self.block.function(line).map_err(refusal)? {
            return Err(Errno::Busy);
        }
        self.links.watch(caller)?;
        // The level first, so the line never shows a glitch as it turns.
        self.block
            .drive(line, polarity.level(asserted))
            .and_then(|()| self.block.set_direction(line, true))
            .map_err(refusal)?;
        self.lines[usize::from(line)] = Some(Held::Output {
            holder: caller,
            polarity,
            asserted,
        });
        self.log.record(Record::Claimed {
            line,
            holder: caller,
            role: Role::Output,
        });
        Ok(())
    }

    /// The polarity of `caller`'s output `line`.
    fn own_output(&self, caller: ProcId, line: u8) -> Result<Polarity, Errno> {
        if !self.lent.contains(line) {
            return Err(Errno::NotFound);
        }
        match &self.lines[usize::from(line)] {
            Some(Held::Output {
                holder, polarity, ..
            }) if *holder == caller => Ok(*polarity),
            Some(_) => Err(Errno::PermissionDenied),
            None => Err(Errno::NotFound),
        }
    }

    fn unwatch_if_idle(&mut self, peer: ProcId) {
        if !self
            .lines
            .iter()
            .flatten()
            .any(|held| held.holder() == peer)
        {
            self.links.unwatch(peer);
        }
    }

    fn refuse_header(&mut self, ticket: u64, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { caller, reason });
        let mut out = [0u8; HEADER_MAX_REPLY];
        if let (Ok(len), Some(port)) = (encode_status(&mut out, Err(reason)), self.header.as_mut())
        {
            let _ = port.reply(ticket, &out[..len]);
        }
    }

    fn refuse_wait(&mut self, ticket: u64, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { caller, reason });
        // A caller that ends before its answer arrives has nobody left to tell.
        let _ = self.answer_wait(ticket, Err(reason));
    }

    /// Serve the header calls queued, at most [`HEADER_CALLS_PER_WAKE`].
    fn header_calls(&mut self) -> Result<(), DriverError> {
        let mut frame = [0u8; HEADER_REQUEST_LEN];
        for _ in 0..HEADER_CALLS_PER_WAKE {
            let Some(port) = self.header.as_mut() else {
                return Ok(());
            };
            match port.receive(&mut frame) {
                Ok(Some((ticket, len))) => {
                    self.serve_header(ticket, frame.get(..len).unwrap_or(&[]));
                }
                Ok(None) => return Ok(()),
                Err(_) => return Err(DriverError::DeviceFault),
            }
        }
        Ok(())
    }
}

impl<S: SupplierHost, H: HeaderPort, L: Recorder<Record>> Supplier for Controller<'_, S, H, L> {
    /// An interrupt-line call: a wait, answered when its line fires.
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.links.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse_wait(ticket, None, reason),
        };
        let outcome = IrqLineRequest::decode(frame).and_then(|request| match request {
            IrqLineRequest::Wait(request) => self.wait(ticket, caller, &request),
        });
        if let Err(reason) = outcome {
            self.refuse_wait(ticket, Some(caller), reason);
        }
    }

    /// The bank interrupts come first, then the header endpoint.
    fn woke(&mut self, index: usize) -> Result<(), DriverError> {
        if index < self.banks {
            self.fired()
        } else {
            self.header_calls()
        }
    }

    /// Give back every line `peer` held.
    fn peer_exited(&mut self, peer: ProcId) {
        for line in 0..self.block.lines() {
            if self.lines[usize::from(line)]
                .as_ref()
                .is_some_and(|held| held.holder() == peer)
            {
                self.release(line);
            }
        }
        self.links.unwatch(peer);
    }
}

//! The controller's lines, served to two kinds of holder: a device the
//! hardware tree links to a line waits on it over `irqline-v1`, and a system
//! service holding `CAP_GPIO` drives a header line over `gpio-header-v1`. A
//! line has one holder at a time, of either kind.
//!
//! A level line's detection is enabled by its consumer's wait and disabled as
//! it fires. An edge line's is enabled by every wait and disabled only when an
//! edge fires with no wait posted: that edge is latched here and answers the
//! next wait at once, so no edge is lost and a line that keeps firing wakes
//! the driver at most once between waits.

use tairix_abi::driver::gpio::{
    encode_lines, encode_status, HeaderLines, HeaderRequest, Holder, LineEntry, LineState,
    LineTable, Polarity, HEADER_MAX_REPLY, HEADER_REQUEST_LEN,
};
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_drvrt::irqline::{IrqLines, LineHardware, LineRecord};
use tairix_drvrt::{refusal, Recorder, Supplier, SupplierHost};

use crate::block::{Block, Function, Trigger, LINES};

/// The bank interrupts a controller raises: GPIO 0–27, 28–45, and 46 up.
pub const BANK_INTERRUPTS: usize = 3;

/// Header calls one wake serves before returning to the wait set, which
/// wakes again while more are queued: a flood cannot starve the banks.
pub const HEADER_CALLS_PER_WAKE: usize = 8;

/// The first line of each bank interrupt's group.
const BANK_FIRST_LINE: [u8; BANK_INTERRUPTS] = [0, 28, 46];

/// A holder's slot for every line.
const SLOTS: usize = LINES as usize;

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

/// A decision the controller records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A decision about a line held as an interrupt.
    Interrupt(LineRecord),
    /// A header line was claimed as an output.
    Claimed {
        /// The line.
        line: u8,
        /// Its holder.
        holder: ProcId,
    },
    /// A header line was returned to an input, by its holder or with its end.
    Released {
        /// The line.
        line: u8,
    },
    /// A header request was refused.
    Refused {
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
}

impl From<LineRecord> for Record {
    fn from(record: LineRecord) -> Self {
        Self::Interrupt(record)
    }
}

/// A header line driven as an output.
struct Output {
    holder: ProcId,
    polarity: Polarity,
    asserted: bool,
}

/// The block, as the holders of its lines drive it.
struct Pins<'r> {
    block: Block<'r>,
    /// The lines whose bank interrupt is bound, a bit per line.
    interruptible: u64,
    outputs: [Option<Output>; SLOTS],
}

impl Pins<'_> {
    /// The header output `line` is held as.
    fn output(&self, line: u8) -> Option<&Output> {
        self.outputs.get(usize::from(line))?.as_ref()
    }
}

impl LineHardware for Pins<'_> {
    type Trigger = Trigger;

    /// The binding's two cells, the line and its trigger, on a line whose
    /// bank interrupt is bound.
    fn select(&self, selector: &[u32]) -> Result<(u8, Trigger), Errno> {
        let [line, trigger] = *selector else {
            return Err(Errno::OutOfRange);
        };
        let line = u8::try_from(line)
            .ok()
            .filter(|&line| line < LINES)
            .ok_or(Errno::OutOfRange)?;
        let trigger = Trigger::from_cell(trigger).ok_or(Errno::NotSupported)?;
        if self.interruptible & (1 << line) == 0 {
            return Err(Errno::NotSupported);
        }
        Ok((line, trigger))
    }

    /// Set to an input, unless driven as a header output or routed to a
    /// peripheral.
    fn take(&mut self, line: u8) -> Result<(), Errno> {
        if self.output(line).is_some() {
            return Err(Errno::Busy);
        }
        if let Function::Alternate(_) = self.block.function(line).map_err(refusal)? {
            return Err(Errno::Busy);
        }
        self.block.set_direction(line, false).map_err(refusal)
    }

    /// An edge is armed by every wait, so the edge after a latched one is
    /// caught; a level only for a wait its firing answers, as a level that
    /// still stands would latch again at once.
    fn arm(&mut self, line: u8, trigger: Trigger, posted: bool) -> Result<(), Errno> {
        if trigger.is_level() && !posted {
            return Ok(());
        }
        self.block.detect(line, trigger, true).map_err(refusal)
    }

    fn release(&mut self, line: u8, trigger: Trigger) {
        // A register that will not take the write leaves nothing to do: the
        // line is no longer anybody's either way.
        let _ = self
            .block
            .detect(line, trigger, false)
            .and_then(|()| self.block.clear_events(1 << line));
    }
}

/// One controller's lines and who holds each.
pub struct Controller<'r, S: SupplierHost, H: HeaderPort, L: Recorder<Record>> {
    pins: Pins<'r>,
    lines: IrqLines<Trigger, SLOTS>,
    links: S,
    header: Option<H>,
    lent: HeaderLines,
    banks: usize,
    log: L,
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
        let last = LINES - 1;
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
            pins: Pins {
                block,
                interruptible,
                outputs: [const { None }; SLOTS],
            },
            lines: IrqLines::new(endpoint),
            links,
            header,
            lent: HeaderLines::from_mask(lent.mask() & lines_mask),
            banks: banks.min(BANK_INTERRUPTS),
            log,
        })
    }

    /// Answer every held line whose event has latched.
    fn fired(&mut self) -> Result<(), DriverError> {
        let fired = self.pins.block.events()?;
        if fired == 0 {
            return Ok(());
        }
        // Disarmed before its status is cleared: a level, which stands until
        // serviced and would latch again at once, and an edge no wait is
        // posted for, which stays latched here until its next wait re-arms it.
        for line in HeaderLines::from_mask(fired).iter() {
            let disarm = self
                .lines
                .held(line)
                .filter(|&(trigger, posted)| trigger.is_level() || !posted);
            if let Some((trigger, _)) = disarm {
                self.pins.block.detect(line, trigger, false)?;
            }
        }
        self.pins.block.clear_events(fired)?;
        self.lines.fired(fired, &mut self.links, &mut self.log);
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
        for routed in self.pins.block.functions(self.lent) {
            let (line, function) = routed.map_err(refusal)?;
            let state = if self.lines.held(line).is_some() {
                LineState::Interrupt
            } else if let Some(output) = self.pins.output(line) {
                LineState::Output {
                    holder: if output.holder == caller {
                        Holder::Caller
                    } else {
                        Holder::Other
                    },
                    asserted: output.asserted,
                }
            } else {
                match function {
                    Function::Alternate(function) => LineState::Peripheral { function },
                    Function::Input | Function::Output => LineState::Free,
                }
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
                self.pins
                    .block
                    .drive(line, polarity.level(asserted))
                    .map_err(refusal)?;
                if let Some(Some(output)) = self.pins.outputs.get_mut(usize::from(line)) {
                    output.asserted = asserted;
                }
                Ok(())
            }
            HeaderRequest::Release { line } => {
                self.own_output(caller, line)?;
                self.release_output(line);
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
        if self.lines.held(line).is_some() || self.pins.output(line).is_some() {
            return Err(Errno::Busy);
        }
        if let Function::Alternate(_) = self.pins.block.function(line).map_err(refusal)? {
            return Err(Errno::Busy);
        }
        self.links.watch(caller)?;
        // The level first, so the line never shows a glitch as it turns.
        self.pins
            .block
            .drive(line, polarity.level(asserted))
            .and_then(|()| self.pins.block.set_direction(line, true))
            .map_err(refusal)?;
        let slot = self
            .pins
            .outputs
            .get_mut(usize::from(line))
            .ok_or(Errno::NotFound)?;
        *slot = Some(Output {
            holder: caller,
            polarity,
            asserted,
        });
        self.log.record(Record::Claimed {
            line,
            holder: caller,
        });
        Ok(())
    }

    /// The polarity of `caller`'s output `line`.
    fn own_output(&self, caller: ProcId, line: u8) -> Result<Polarity, Errno> {
        if !self.lent.contains(line) {
            return Err(Errno::NotFound);
        }
        match (self.pins.output(line), self.lines.held(line)) {
            (Some(output), _) if output.holder == caller => Ok(output.polarity),
            (None, None) => Err(Errno::NotFound),
            _ => Err(Errno::PermissionDenied),
        }
    }

    /// Return the header output `line` to an input.
    fn release_output(&mut self, line: u8) {
        let held = self
            .pins
            .outputs
            .get_mut(usize::from(line))
            .and_then(Option::take);
        if held.is_some() {
            // A register that will not take the write leaves nothing to do:
            // the line is no longer anybody's either way.
            let _ = self.pins.block.set_direction(line, false);
            self.log.record(Record::Released { line });
        }
    }

    fn unwatch_if_idle(&mut self, peer: ProcId) {
        let drives = self
            .pins
            .outputs
            .iter()
            .flatten()
            .any(|output| output.holder == peer);
        if !drives && !self.lines.holds(peer) {
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
        self.lines.serve(
            ticket,
            frame,
            &mut self.pins,
            &mut self.links,
            &mut self.log,
        );
    }

    /// The bank interrupts come first, then the header endpoint.
    fn woke(&mut self, index: usize) -> Result<(), DriverError> {
        if index < self.banks {
            self.fired()
        } else {
            self.header_calls()
        }
    }

    /// Give back every line `peer` held, of either kind.
    fn peer_exited(&mut self, peer: ProcId) {
        for line in 0..LINES {
            if self
                .pins
                .output(line)
                .is_some_and(|output| output.holder == peer)
            {
                self.release_output(line);
            }
        }
        self.lines
            .peer_exited(peer, &mut self.pins, &mut self.links, &mut self.log);
    }
}

//! The line holders every `irqline-v1` supplier keeps (`plans/GPIO.md` G1):
//! which consumer holds each line and the wait it has posted, and the rules
//! by which a wait is believed, a line is taken from a consumer that has
//! ended, and a firing is answered. The controller's registers stay its own,
//! behind [`LineHardware`].

use tairix_abi::driver::irqline::{encode_reply, IrqLineRequest, IRQLINE_MAX_REPLY};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId};

use crate::{Recorder, SupplierHost};

/// A decision about a line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LineRecord {
    /// A consumer took a line.
    Claimed {
        /// The line.
        line: u8,
        /// Its consumer.
        holder: ProcId,
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
    /// A line was masked and given back with its ended consumer.
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

/// A controller's lines, as the holders of them drive its registers.
pub trait LineHardware {
    /// What a selector asks of its line besides the line itself.
    type Trigger: Copy;

    /// The line `selector` names and the trigger it asks for.
    ///
    /// # Errors
    ///
    /// The refusal the wait is answered with: a selector of the wrong shape
    /// or naming a line the controller lacks, or a trigger it cannot detect.
    fn select(&self, selector: &[u32]) -> Result<(u8, Self::Trigger), Errno>;

    /// Ready `line`, which no consumer holds, to be one's.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] for a line put to another use, or the hardware's
    /// refusal.
    fn take(&mut self, _line: u8) -> Result<(), Errno> {
        Ok(())
    }

    /// Arm `line` to fire on `trigger` for its consumer's wait: `posted` when
    /// the firing answers the wait, not when a latched firing just has.
    ///
    /// # Errors
    ///
    /// The hardware's refusal, which the wait is refused with.
    fn arm(&mut self, line: u8, trigger: Self::Trigger, posted: bool) -> Result<(), Errno>;

    /// Mask `line` and drop anything it latched: its consumer has ended.
    fn release(&mut self, line: u8, trigger: Self::Trigger);
}

/// A consumer holding a line.
struct Holder<T> {
    owner: ProcId,
    request: LinkRequest,
    trigger: T,
    /// The wait the next firing answers.
    posted: Option<u64>,
    /// A firing no wait was posted for, which answers the next at once.
    latched: bool,
}

/// One controller's lines, `N` at most, and the consumers holding them.
pub struct IrqLines<T, const N: usize> {
    endpoint: u64,
    lines: [Option<Holder<T>>; N],
}

impl<T: Copy, const N: usize> IrqLines<T, N> {
    /// The lines of the controller serving `endpoint`, none held.
    #[must_use]
    pub fn new(endpoint: u64) -> Self {
        const { assert!(N <= 64, "a line is a bit of a 64-bit mask") };
        Self {
            endpoint,
            lines: [const { None }; N],
        }
    }

    /// The trigger `line`'s consumer holds it with and whether its wait is
    /// posted; [`None`] while nobody holds it.
    #[must_use]
    pub fn held(&self, line: u8) -> Option<(T, bool)> {
        self.holder(line)
            .map(|holder| (holder.trigger, holder.posted.is_some()))
    }

    /// Whether `peer` holds any line.
    #[must_use]
    pub fn holds(&self, peer: ProcId) -> bool {
        self.lines
            .iter()
            .flatten()
            .any(|holder| holder.owner == peer)
    }

    /// Serve the call `ticket` carrying `frame`: a wait, believed only for a
    /// line the kernel attests its caller holds and answered when the line
    /// fires; anything else is refused at once and recorded.
    pub fn serve<H, S, L, R>(
        &mut self,
        ticket: u64,
        frame: &[u8],
        hardware: &mut H,
        links: &mut S,
        log: &mut L,
    ) where
        H: LineHardware<Trigger = T>,
        S: SupplierHost,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        let caller = match links.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return refuse(ticket, None, reason, links, log),
        };
        let outcome = IrqLineRequest::decode(frame).and_then(|request| match request {
            IrqLineRequest::Wait(request) => {
                self.wait(ticket, caller, &request, hardware, links, log)
            }
        });
        if let Err(reason) = outcome {
            refuse(ticket, Some(caller), reason, links, log);
        }
    }

    /// The lines `mask` sets fired: answer each holder's posted wait, or
    /// latch the firing for its next wait when none is posted.
    pub fn fired<S, L, R>(&mut self, mask: u64, links: &mut S, log: &mut L)
    where
        S: SupplierHost,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        let mut pending = mask;
        while pending != 0 {
            let bit = pending.trailing_zeros();
            pending &= pending - 1;
            let Ok(line) = u8::try_from(bit) else {
                break;
            };
            let Some(holder) = self.holder_mut(line) else {
                continue;
            };
            match holder.posted.take() {
                Some(ticket) => {
                    if !answer(ticket, Ok(()), links) {
                        log.record(LineRecord::Unanswered { line }.into());
                    }
                }
                None => holder.latched = true,
            }
        }
    }

    /// Release every line `peer` held, then stop watching it.
    pub fn peer_exited<H, S, L, R>(
        &mut self,
        peer: ProcId,
        hardware: &mut H,
        links: &mut S,
        log: &mut L,
    ) where
        H: LineHardware<Trigger = T>,
        S: SupplierHost,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        for line in (0u8..).take(N) {
            if self.holder(line).is_some_and(|holder| holder.owner == peer) {
                self.release(line, hardware, log);
            }
        }
        links.unwatch(peer);
    }

    fn wait<H, S, L, R>(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &LinkRequest,
        hardware: &mut H,
        links: &mut S,
        log: &mut L,
    ) -> Result<(), Errno>
    where
        H: LineHardware<Trigger = T>,
        S: SupplierHost,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        if request.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !links.caller_holds(ticket, &HwResource::request(request))? {
            return Err(Errno::PermissionDenied);
        }
        let (line, trigger) = hardware.select(request.selector())?;
        let claimant = Holder {
            owner: caller,
            request: *request,
            trigger,
            posted: None,
            latched: false,
        };
        self.claim(line, claimant, hardware, links, log)?;
        let holder = self.holder_mut(line).ok_or(Errno::NotFound)?;
        if holder.posted.is_some() {
            return Err(Errno::Busy);
        }
        let latched = core::mem::take(&mut holder.latched);
        if !latched {
            holder.posted = Some(ticket);
        }
        // Armed only once the wait is held, so its firing finds it; a wait
        // refused here is answered by the refusal, never again by a firing.
        if let Err(reason) = hardware.arm(line, holder.trigger, !latched) {
            holder.posted = None;
            holder.latched = latched;
            return Err(reason);
        }
        if latched {
            // A caller that ends before its answer arrives has nobody left to
            // tell.
            let _ = answer(ticket, Ok(()), links);
        }
        Ok(())
    }

    /// Make `line` `claimant`'s, as its link allows.
    fn claim<H, S, L, R>(
        &mut self,
        line: u8,
        claimant: Holder<T>,
        hardware: &mut H,
        links: &mut S,
        log: &mut L,
    ) -> Result<(), Errno>
    where
        H: LineHardware<Trigger = T>,
        S: SupplierHost,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        let slot = self.lines.get(usize::from(line)).ok_or(Errno::OutOfRange)?;
        if let Some(held) = slot {
            if held.owner == claimant.owner {
                return if held.request == claimant.request {
                    Ok(())
                } else {
                    Err(Errno::Busy)
                };
            }
            // Two nodes may name one line; it stays with a holder that lives,
            // and a watch refused as gone says it does not.
            let from = held.owner;
            match links.watch(from) {
                Ok(()) => return Err(Errno::Busy),
                Err(Errno::NotFound) => {}
                Err(reason) => return Err(reason),
            }
            self.release(line, hardware, log);
            log.record(LineRecord::Reclaimed { line, from }.into());
        }
        hardware.take(line)?;
        links.watch(claimant.owner)?;
        let holder = claimant.owner;
        if let Some(slot) = self.lines.get_mut(usize::from(line)) {
            *slot = Some(claimant);
        }
        log.record(LineRecord::Claimed { line, holder }.into());
        Ok(())
    }

    fn release<H, L, R>(&mut self, line: u8, hardware: &mut H, log: &mut L)
    where
        H: LineHardware<Trigger = T>,
        L: Recorder<R>,
        R: From<LineRecord>,
    {
        let Some(holder) = self.lines.get_mut(usize::from(line)).and_then(Option::take) else {
            return;
        };
        hardware.release(line, holder.trigger);
        log.record(LineRecord::Released { line }.into());
    }

    fn holder(&self, line: u8) -> Option<&Holder<T>> {
        self.lines.get(usize::from(line))?.as_ref()
    }

    fn holder_mut(&mut self, line: u8) -> Option<&mut Holder<T>> {
        self.lines.get_mut(usize::from(line))?.as_mut()
    }
}

/// Answer the wait `ticket` with `outcome`, answering whether its caller
/// heard.
fn answer<S: SupplierHost>(ticket: u64, outcome: Result<(), Errno>, links: &mut S) -> bool {
    let mut out = [0u8; IRQLINE_MAX_REPLY];
    encode_reply(&mut out, outcome).is_ok_and(|len| links.reply(ticket, &out[..len]).is_ok())
}

/// Refuse the call `ticket` for `reason`, and record it.
fn refuse<S, L, R>(ticket: u64, caller: Option<ProcId>, reason: Errno, links: &mut S, log: &mut L)
where
    S: SupplierHost,
    L: Recorder<R>,
    R: From<LineRecord>,
{
    log.record(LineRecord::Refused { caller, reason }.into());
    // A caller that ends before its answer arrives has nobody left to tell.
    let _ = answer(ticket, Err(reason), links);
}

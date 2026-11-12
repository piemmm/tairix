//! The debugger's side of the host (`plans/PCEMU.md` §3.1): its requests
//! held in order with the rest of the credited input, and every report
//! checked against what was asked — a stop only of a processor running
//! under the probe, an answer only to the look sent, its listing no longer
//! than asked and beginning where asked, its memory as long as asked.
//! Reports made before the worker took a detach or a reset the host sent
//! are dropped until the worker says it took it: they describe a debugger,
//! or a stop, that is gone.

use alloc::boxed::Box;
use core::ops::Range;

use tairix_inline::ArrayVec;

use super::{Held, Host, HostEvent, Kind, Refused};
use crate::cpu::Family;
use crate::debug::probe::{Breakpoint, Go, MOST_BREAKPOINTS};
use crate::link::debug::{Debug, Listing, Look, Said, Seen};
use tairix_log::Sink;
use tairix_sandbox::session::SessionTransport;

/// What the host holds of a debugger attached.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Debugging {
    /// The processor stopped, as the worker last said.
    stopped: bool,
    /// The look sent and not yet answered.
    look: Option<Look>,
    next_look: u32,
    /// Resets sent that the worker has yet to say it took.
    resetting: u32,
    /// The breakpoints the probe holds, as the host has asked.
    held: ArrayVec<Breakpoint, MOST_BREAKPOINTS>,
}

/// A look's answer, checked: where its parts lie in the event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Answered {
    number: u32,
    before: u8,
    after: u8,
    listing: Range<usize>,
    memory: Range<usize>,
    stack: Range<usize>,
}

/// Where `part`, a slice of `whole`, lies in it.
fn within(whole: &[u8], part: &[u8]) -> Range<usize> {
    let start = part.as_ptr().addr().wrapping_sub(whole.as_ptr().addr());
    start..start.saturating_add(part.len())
}

impl<T: SessionTransport, S: Sink> Host<T, S> {
    /// Puts the debugger's probe on the machine's board: the machine runs
    /// under it, stopped only as it says.
    ///
    /// # Errors
    ///
    /// [`Refused`] when the worker is gone or not taking input.
    pub fn attach_debugger(&mut self) -> Result<(), Refused> {
        if self.debugging.is_none() {
            self.hold(Held::Debug(Debug::Attach))?;
            self.debugging = Some(Debugging::default());
        }
        Ok(())
    }

    /// Takes the probe off, the machine going on from any stop.
    ///
    /// # Errors
    ///
    /// As [`Self::attach_debugger`].
    pub fn detach_debugger(&mut self) -> Result<(), Refused> {
        if self.debugging.is_some() {
            self.hold(Held::Debug(Debug::Detach))?;
            self.debugging = None;
            self.detaching = self.detaching.saturating_add(1);
        }
        Ok(())
    }

    /// Whether the processor is stopped under the debugger.
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.debugging
            .as_ref()
            .is_some_and(|debugging| debugging.stopped)
    }

    fn attached(&self) -> Result<&Debugging, Refused> {
        self.debugging.as_ref().ok_or(Refused::Detached)
    }

    fn debugging(&mut self) -> Option<&mut Debugging> {
        self.debugging.as_mut()
    }

    /// Has the probe hold `breakpoint` too.
    ///
    /// # Errors
    ///
    /// [`Refused::Breakpoint`] for one a probe cannot hold, one held
    /// already, or more than a probe holds; [`Refused::Detached`] with no
    /// debugger; as [`Self::attach_debugger`] otherwise.
    pub fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), Refused> {
        let mut held = self.attached()?.held.clone();
        if !breakpoint.is_valid()
            || held.contains(&breakpoint)
            || held.try_push(breakpoint).is_err()
        {
            return Err(Refused::Breakpoint);
        }
        self.hold(Held::Debug(Debug::Add(breakpoint)))?;
        if let Some(debugging) = self.debugging() {
            debugging.held = held;
        }
        Ok(())
    }

    /// Has the probe hold `breakpoint` no more.
    ///
    /// # Errors
    ///
    /// [`Refused::Breakpoint`] for one not held; as
    /// [`Self::add_breakpoint`] otherwise.
    pub fn remove_breakpoint(&mut self, breakpoint: Breakpoint) -> Result<(), Refused> {
        let held = &self.attached()?.held;
        let Some(at) = held.iter().position(|held| *held == breakpoint) else {
            return Err(Refused::Breakpoint);
        };
        self.hold(Held::Debug(Debug::Remove(breakpoint)))?;
        if let Some(debugging) = self.debugging() {
            debugging.held.remove(at);
        }
        Ok(())
    }

    /// Stops the running processor at its next instruction; nothing for
    /// one stopped.
    ///
    /// # Errors
    ///
    /// As [`Self::add_breakpoint`] for a missing debugger or a worker not
    /// taking input.
    pub fn debug_break(&mut self) -> Result<(), Refused> {
        if self.attached()?.stopped {
            return Ok(());
        }
        self.hold(Held::Debug(Debug::Break))
    }

    /// Sends the stopped processor on as `go` says.
    ///
    /// # Errors
    ///
    /// [`Refused::Busy`] when it is not stopped; as [`Self::debug_break`]
    /// otherwise.
    pub fn debug_go(&mut self, go: Go) -> Result<(), Refused> {
        if !self.attached()?.stopped {
            return Err(Refused::Busy);
        }
        self.hold(Held::Debug(Debug::Go(go)))?;
        if let Some(debugging) = self.debugging() {
            debugging.stopped = false;
        }
        Ok(())
    }

    /// Asks for what `look` names, under a number of the host's: the
    /// number [`HostEvent::Seen`] answers with.
    ///
    /// # Errors
    ///
    /// [`Refused::Busy`] while an earlier look is unanswered; as
    /// [`Self::debug_break`] otherwise.
    pub fn look(&mut self, look: Look) -> Result<u32, Refused> {
        let debugging = self.attached()?;
        if debugging.look.is_some() {
            return Err(Refused::Busy);
        }
        let look = Look {
            number: debugging.next_look,
            ..look
        };
        self.hold(Held::Debug(Debug::Look(look)))?;
        if let Some(debugging) = self.debugging() {
            debugging.look = Some(look);
            debugging.next_look = look.number.wrapping_add(1);
        }
        Ok(look.number)
    }

    /// A reset sent: the stop, and any look at it, go with the machine the
    /// worker powers off.
    pub(super) fn debug_reset(&mut self) {
        if let Some(debugging) = self.debugging() {
            debugging.stopped = false;
            debugging.look = None;
            debugging.resetting = debugging.resetting.saturating_add(1);
        }
    }

    /// The debugger's report `said`, from `payload`, checked: what to hand
    /// the owner, none for a report made before a detach or a reset taken.
    pub(super) fn said(
        &mut self,
        payload: &[u8],
        said: Said<'_>,
    ) -> Result<Option<Kind>, &'static str> {
        let chips = self.chips;
        let gone = self.detaching > 0;
        let debugging = self.debugging.as_mut();
        let resetting = debugging
            .as_ref()
            .is_some_and(|debugging| debugging.resetting > 0);
        let kind = match (said, debugging) {
            (Said::Detached, _) => {
                self.detaching = self
                    .detaching
                    .checked_sub(1)
                    .ok_or("a detach never asked")?;
                return Ok(None);
            }
            _ if gone => return Ok(None),
            (_, None) => return Err("a debugger report with no debugger"),
            (Said::Reset, Some(debugging)) => {
                debugging.resetting = debugging
                    .resetting
                    .checked_sub(1)
                    .ok_or("a reset never sent")?;
                return Ok(None);
            }
            _ if resetting => return Ok(None),
            (Said::Stopped { reason, registers }, Some(debugging)) => {
                if core::mem::replace(&mut debugging.stopped, true) {
                    return Err("a stop of a processor already stopped");
                }
                let nec = chips.model.family() == Family::Nec;
                if registers.coprocessor.is_some() != chips.coprocessor
                    || (registers.emulating && !nec)
                {
                    return Err("registers of another machine");
                }
                Kind::Stopped(reason, Box::new(registers))
            }
            (Said::Seen(seen), Some(debugging)) => {
                let Some(look) = debugging.look.take_if(|look| look.number == seen.number) else {
                    return Err("an answer to a look never sent");
                };
                if !answers(&look, &seen) {
                    return Err("an answer that is not the one asked for");
                }
                Kind::Seen(Answered {
                    number: seen.number,
                    before: seen.listing.before,
                    after: seen.listing.after,
                    listing: within(payload, seen.listing.encoded()),
                    memory: within(payload, seen.memory),
                    stack: within(payload, seen.stack),
                })
            }
        };
        Ok(Some(kind))
    }

    /// The answer `answered` lends from the event.
    pub(super) fn lend_seen(&self, answered: &Answered) -> HostEvent<'_> {
        let part = |range: &Range<usize>| self.event.get(range.clone()).unwrap_or_default();
        HostEvent::Seen(Seen {
            number: answered.number,
            listing: Listing::new(answered.before, answered.after, part(&answered.listing)),
            memory: part(&answered.memory),
            stack: part(&answered.stack),
        })
    }
}

/// Whether `seen` answers `look`: no more lines than asked either side of
/// the code address, those from it beginning there, and memory and stack
/// as long as asked.
fn answers(look: &Look, seen: &Seen<'_>) -> bool {
    let listing = &seen.listing;
    let first_after = listing.lines().nth(usize::from(listing.before));
    listing.before <= look.lines.0
        && listing.after <= look.lines.1
        && (listing.after == 0 || first_after.is_some_and(|line| line.ip == look.code.1))
        && seen.memory.len() == usize::from(look.bytes)
        && seen.stack.len() == usize::from(look.stack) * 2
}

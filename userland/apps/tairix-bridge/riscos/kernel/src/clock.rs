//! Time (`plans/RISCOS-EMULATOR.md` §4.2, decision 12): the centisecond
//! clock the host hands in, the interrupt that takes it, and what each
//! interrupt does — the monotonic count software reads in zero page, the
//! interval timer, `TickerV` and the timed calls of `OS_CallAfter` and
//! `OS_CallEvery`.
//!
//! The clock is the host's monotonic reading, so a guest that masks
//! interrupts for a while sees the count jump when it unmasks them; due
//! timed calls then run once each, never a burst.

use tairix_collections::OrderedMap;
use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::error::Error;
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::memory::Bits;
use crate::os::{Entry, Fail, Os, Regs, Target, Unwind};
use crate::vectors;
use crate::zeropage;

/// The interval timer's width: five bytes.
pub(crate) const FIVE_BYTES: u64 = 1 << 40;
/// Centiseconds in a day, and from 1900, five-byte time's base, to 1970,
/// Unix time's.
pub(crate) const CENTISECONDS_PER_DAY: u64 = 8_640_000;
pub(crate) const UNIX_EPOCH: u64 = 25_567 * CENTISECONDS_PER_DAY;
/// The delay a timed call is refused from, as genuine RISC OS 5 refuses it.
const LONGEST_DELAY: u32 = 0xFFFF_FC00;
/// A timed call's system heap node: room for its code, its R12, its period,
/// when it is due and a link.
const NODE: u32 = 20;
/// The event the interval timer raises when it passes zero.
const INTERVAL_EVENT: u32 = 5;
/// The most centiseconds of `TickerV` a late interrupt makes up: a host
/// that fell behind by up to a second still delivers each one, and one held
/// up longer drops the rest rather than replaying them in a burst.
const TICKER_CATCH_UP: u64 = 100;

/// "Invalid time interval".
const BAD_TIME: Error = Error {
    number: 0x1E5,
    token: "BadTimeInterval",
};

/// A timed call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ticker {
    address: u32,
    workspace: u32,
    bits: Bits,
    /// `OS_CallEvery`'s period; none for `OS_CallAfter`.
    period: Option<u64>,
    node: u32,
}

/// The clock's state.
pub(crate) struct Clock {
    /// The monotonic reading the last interrupt took.
    taken: u64,
    /// TIME less the monotonic reading, modulo five bytes.
    time_offset: u64,
    /// The interval timer as last written, and when.
    interval: u64,
    interval_at: u64,
    /// When the interval timer next passes zero.
    interval_due: Option<u64>,
    /// When the zone next changes its offset, while daylight saving follows
    /// it.
    pub(crate) zone_due: Option<u64>,
    /// The guest's real time less the host's.
    real_offset: i64,
    /// Timed calls by when they fall due and the order they were made, so
    /// calls due together run first come, first served.
    tickers: OrderedMap<(u64, u64), Ticker>,
    sequence: u64,
}

impl Clock {
    pub(crate) const fn new() -> Self {
        Self {
            taken: 0,
            time_offset: 0,
            interval: 0,
            interval_at: 0,
            interval_due: None,
            zone_due: None,
            real_offset: 0,
            tickers: OrderedMap::new(),
            sequence: 0,
        }
    }
}

impl Os<'_> {
    /// The monotonic clock, in centiseconds.
    pub(crate) fn now(&mut self) -> u64 {
        self.world.host.monotonic()
    }

    /// The monotonic time as the guest reads it, wrapping every 497 days.
    pub(crate) fn now32(&mut self) -> u32 {
        (self.now() & 0xFFFF_FFFF) as u32
    }

    /// Between slices: if the clock has moved on, the interrupt is
    /// asserted, to be taken when the guest's mask allows.
    pub(crate) fn tick(&mut self) {
        if self.now() > self.world.clock.taken {
            self.world.processor.set_irq(true);
        }
    }

    /// When the next timed work falls due, for a guest with nothing to do
    /// until then.
    pub(crate) fn next_deadline(&mut self) -> Option<u64> {
        let ticker = self
            .world
            .clock
            .tickers
            .first_key_value()
            .map(|(&(due, _), _)| due);
        let interval = self.world.clock.interval_due;
        let key = self.world.keyboard.due();
        let zone = self.world.clock.zone_due;
        [ticker, interval, key, zone].into_iter().flatten().min()
    }

    /// The IRQ vector's entry: the clock's work, then a return to the
    /// interrupted code, and the callbacks due if that is User mode.
    pub(crate) fn interrupt(&mut self) -> Result<(), Unwind> {
        self.world.processor.set_irq(false);
        let outer = core::mem::replace(&mut self.world.in_interrupt, true);
        let _ = self.poke_word(HIGH_ZERO_PAGE + zeropage::IRQSEMA, 1);
        let worked = self.interrupt_work();
        self.world.in_interrupt = outer;
        let _ = self.poke_word(HIGH_ZERO_PAGE + zeropage::IRQSEMA, u32::from(outer));
        worked?;
        self.return_from_interrupt()
    }

    /// The work on an interrupt: the keyboard's input, then the clock's —
    /// `MetroGnome`, the interval timer's event, `TickerV` once for each
    /// centisecond since the last, the timed calls and `UnthreadV`.
    fn interrupt_work(&mut self) -> Result<(), Unwind> {
        self.take_input()?;
        let now = self.now();
        let elapsed = now.saturating_sub(self.world.clock.taken);
        self.world.clock.taken = now;
        let _ = self.poke_word(
            HIGH_ZERO_PAGE + zeropage::METROGNOME,
            (now & 0xFFFF_FFFF) as u32,
        );
        if self.world.clock.interval_due.is_some_and(|due| now >= due) {
            self.world.clock.interval_due = None;
            self.raise_event(&mut Regs::new([
                INTERVAL_EVENT,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]))?;
        }
        self.keyboard_tick(now)?;
        if self.world.clock.zone_due.is_some_and(|due| now >= due) {
            // The changeover issues a service: it waits for a callback.
            self.world.clock.zone_due = None;
            self.world.callbacks.request_zone_change();
        }
        for _ in 0..elapsed.min(TICKER_CATCH_UP) {
            self.call_vector(vectors::TICKER, &mut Regs::default(), Entry::Irq)?;
        }
        self.run_tickers(now)?;
        let mut regs = Regs::default();
        self.call_vector(vectors::UNTHREAD, &mut regs, Entry::Irq)
    }

    /// Runs the timed calls due at `now`, each in Supervisor mode with
    /// interrupts disabled, an `OS_CallEvery` re-armed a period after.
    fn run_tickers(&mut self, now: u64) -> Result<(), Unwind> {
        while let Some((&(due, _), _)) = self.world.clock.tickers.first_key_value() {
            if due > now {
                break;
            }
            let Some((_, ticker)) = self.world.clock.tickers.pop_first() else {
                break;
            };
            match ticker.period {
                Some(period) => {
                    let sequence = self.world.clock.sequence;
                    self.world.clock.sequence += 1;
                    if self
                        .world
                        .clock
                        .tickers
                        .try_insert((now + period, sequence), ticker)
                        .is_err()
                    {
                        self.free_system_node(ticker.node);
                    }
                }
                None => self.free_system_node(ticker.node),
            }
            let mut regs = Regs::default();
            regs.r[12] = ticker.workspace;
            let target = Target {
                address: ticker.address,
                bits: ticker.bits,
            };
            self.call(target, &mut regs, Entry::SupervisorNoIrq)?;
        }
        Ok(())
    }

    /// Returns from the IRQ exception to the code it interrupted, running
    /// the callbacks due when that is User mode with interrupts enabled.
    fn return_from_interrupt(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let back = cpu.reg(Reg::LR).wrapping_sub(4);
        let Some(saved) = cpu.spsr() else {
            return Err(Unwind::Ended(crate::os::Ended::Fault));
        };
        if cpu.set_cpsr(saved).is_err() {
            return Err(Unwind::Ended(crate::os::Ended::Fault));
        }
        let back = if cpu.mode().is_26bit() {
            back & psr::R15_PC
        } else {
            back
        };
        cpu.set_reg(Reg::PC, back);
        self.callbacks_after_interrupt()
    }

    /// `OS_CallAfter`: a call `R0` centiseconds from now.
    pub(crate) fn call_after(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.add_ticker(regs, false)
    }

    /// `OS_CallEvery`: a call every `R0 + 1` centiseconds.
    pub(crate) fn call_every(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.add_ticker(regs, true)
    }

    fn add_ticker(&mut self, regs: &Regs, every: bool) -> Result<(), Fail> {
        let delay = regs.r[0];
        if delay == 0 || delay >= LONGEST_DELAY {
            return Err(self.error(BAD_TIME));
        }
        let node = self.system_node(NODE)?;
        let ticks = u64::from(delay) + 1;
        let ticker = Ticker {
            address: regs.r[1],
            workspace: regs.r[2],
            bits: self.caller_bits(),
            period: every.then_some(ticks),
            node,
        };
        let due = self.now() + ticks;
        let sequence = self.world.clock.sequence;
        self.world.clock.sequence += 1;
        if self
            .world
            .clock
            .tickers
            .try_insert((due, sequence), ticker)
            .is_err()
        {
            self.free_system_node(node);
            return Err(self.error(crate::error::SYSTEM_HEAP_FULL));
        }
        Ok(())
    }

    /// `OS_RemoveTickerEvent`: every timed call of the routine and R12 gone.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn remove_ticker_event(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (address, workspace) = (regs.r[0], regs.r[1]);
        let mut freed = alloc::vec::Vec::new();
        self.world.clock.tickers.retain(|_, ticker| {
            let matched = ticker.address == address && ticker.workspace == workspace;
            if matched && freed.try_reserve(1).is_ok() {
                freed.push(ticker.node);
                return false;
            }
            !matched
        });
        for node in freed {
            self.free_system_node(node);
        }
        Ok(())
    }

    /// `OS_ReadMonotonicTime`.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn read_monotonic_time(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        regs.r[0] = self.now32();
        Ok(())
    }

    /// TIME, the five-byte centisecond count `OS_Word` 1 reads.
    pub(crate) fn time(&mut self) -> u64 {
        (self.now() + self.world.clock.time_offset) % FIVE_BYTES
    }

    /// Sets TIME (`OS_Word` 2).
    pub(crate) fn set_time(&mut self, time: u64) {
        let now = self.now() % FIVE_BYTES;
        self.world.clock.time_offset = (time % FIVE_BYTES + FIVE_BYTES - now) % FIVE_BYTES;
    }

    /// The interval timer (`OS_Word` 3).
    pub(crate) fn interval_timer(&mut self) -> u64 {
        let elapsed = self.now() - self.world.clock.interval_at;
        (self.world.clock.interval + elapsed) % FIVE_BYTES
    }

    /// Sets the interval timer (`OS_Word` 4): it raises its event when it
    /// passes zero.
    pub(crate) fn set_interval_timer(&mut self, value: u64) {
        let now = self.now();
        let value = value % FIVE_BYTES;
        self.world.clock.interval = value;
        self.world.clock.interval_at = now;
        self.world.clock.interval_due = Some(now + (FIVE_BYTES - value));
    }

    /// The guest's real time: centiseconds since 1900, UTC.
    pub fn real_time(&mut self) -> u64 {
        let host = self.world.host.real_time();
        host.saturating_add_signed(self.world.clock.real_offset) % FIVE_BYTES
    }

    /// Sets the guest's real time; the host's is never touched.
    pub(crate) fn set_real_time(&mut self, time: u64) {
        let host = self.world.host.real_time();
        self.world.clock.real_offset = i64::try_from(time % FIVE_BYTES)
            .unwrap_or(0)
            .saturating_sub(i64::try_from(host).unwrap_or(i64::MAX));
    }

    /// `OS_ResyncTime`: the real time taken from the host's clock again.
    pub(crate) fn resync_time(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        self.world.clock.real_offset = 0;
        let mut regs = Regs::new([0, 0xDD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.service(&mut regs)?;
        Ok(())
    }
}

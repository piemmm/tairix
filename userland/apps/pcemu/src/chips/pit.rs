//! The Intel 8253 programmable interval timer, evaluated lazily.
//!
//! A counter's state at any tick follows from the tick its count was loaded
//! at, so the timer costs nothing until it is read or an output edge matters:
//! the board asks when channel 0's output next rises, and how often channel
//! 1's rose over a span.
//!
//! Ticks are of the PIT's input clock. A count written during tick `w` is
//! loaded at `w + 1` and first decremented a tick later, as on the part.

/// How a counter's count is read and written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Lsb,
    Msb,
    Word,
}

/// Which byte of a word count the next read returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadByte {
    Low,
    High,
}

/// A count in progress: the tick it was loaded at, and how far into its
/// period it already was then.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Run {
    loaded: u64,
    phase: u64,
}

impl Run {
    /// Ticks into the period at `tick`, once loaded.
    fn position(self, tick: u64) -> Option<u64> {
        tick.checked_sub(self.loaded).map(|e| e + self.phase)
    }
}

/// Modes 2 and 3 load a newly written count at the end of the current period
/// (mode 2) or half-period (mode 3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reload {
    at: u64,
    /// Mode 3 reloads at the falling edge too, and starts the new count low.
    low_half: bool,
}

/// The high half of a mode 3 square wave: the larger half of an odd count.
const fn high_half(count: u32) -> u32 {
    count.div_ceil(2)
}

/// A counter's output `e` ticks into a count of `n` in `mode`.
fn output(mode: u8, n: u32, e: u64) -> bool {
    let n = u64::from(n);
    match mode {
        0 | 1 => e >= n,
        2 => e % n != n - 1,
        3 => e % n < n.div_ceil(2),
        _ => e != n,
    }
}

/// The count register's value `elapsed` ticks into a count of `length` in
/// `mode`.
fn count(mode: u8, length: u32, elapsed: u64, modulus: u32) -> u32 {
    let into = |period: u32| u32::try_from(elapsed % u64::from(period)).unwrap_or(0);
    match mode {
        2 => length - into(length),
        3 => {
            // Mode 3 counts down by two, reloading at each half; an odd count
            // takes one off its high half's first tick and three off its low
            // half's.
            let place = into(length);
            let high = high_half(length);
            let (ticks, first) = if place < high {
                (place, 1)
            } else {
                (place - high, 3)
            };
            match (length % 2, ticks) {
                (_, 0) => length,
                (0, ticks) => length - 2 * ticks,
                (_, ticks) => length + 2 - first - 2 * ticks,
            }
        }
        _ => (length + modulus - into(modulus)) % modulus,
    }
}

/// The first tick after `after` at which a fixed count of `n` running as
/// `run` changes its output from `now`, which is its level at `after`.
fn change_after(mode: u8, n: u32, run: Run, after: u64, now: bool) -> Option<u64> {
    if after < run.loaded {
        if output(mode, n, run.phase) != now {
            return Some(run.loaded);
        }
        return change_after(mode, n, run, run.loaded, now);
    }
    let e = after - run.loaded + run.phase;
    let n = u64::from(n);
    let delta = match mode {
        0 | 1 => (e < n).then(|| n - e),
        2 | 3 if n == 1 => None,
        2 => {
            let p = e % n;
            Some(if p < n - 1 { n - 1 - p } else { 1 })
        }
        3 => {
            let p = e % n;
            let h = n.div_ceil(2);
            Some(if p < h { h - p } else { n - p })
        }
        _ if e < n => Some(n - e),
        _ => (e == n).then_some(1),
    };
    delta.map(|d| after + d)
}

/// Rising edges a periodic count of `n` running as `run` makes in
/// `(a, b]`. Modes 2 and 3 rise only at the end of a period.
fn periodic_rises(n: u32, run: Run, a: u64, b: u64) -> u64 {
    let Some(end) = run.position(b) else { return 0 };
    if n < 2 {
        return 0;
    }
    let start = run.position(a).unwrap_or(run.phase);
    let n = u64::from(n);
    end / n - start / n
}

fn to_bcd(value: u16) -> u16 {
    let mut out = 0;
    let mut v = value % 10_000;
    for shift in [0, 4, 8, 12] {
        out |= (v % 10) << shift;
        v /= 10;
    }
    out
}

fn from_bcd(value: u16) -> u32 {
    let mut out = 0;
    for shift in [12, 8, 4, 0] {
        out = out * 10 + u32::from((value >> shift) & 0xF).min(9);
    }
    out
}

/// One counter.
#[derive(Clone, Debug)]
struct Counter {
    mode: u8,
    bcd: bool,
    access: Access,
    /// The count register: the count last written, 1 to 65 536 (10 000 in
    /// BCD); `None` from a control word until a count is written.
    cr: Option<u32>,
    /// The count in force.
    ce: u32,
    run: Option<Run>,
    reload: Option<Reload>,
    /// Modes 0 and 4 stopped by their gate keep their place.
    paused: Option<u64>,
    /// What the counter shows between a count's write and its load.
    held: u32,
    gate: bool,
    /// The output while not counting.
    idle_out: bool,
    latch: Option<u16>,
    read_next: ReadByte,
    /// A word count's low byte, written and waiting for its high byte.
    low_written: Option<u8>,
}

impl Counter {
    const fn new() -> Self {
        Self {
            mode: 0,
            bcd: false,
            access: Access::Word,
            cr: None,
            ce: 0x1_0000,
            run: None,
            reload: None,
            paused: None,
            held: 0,
            gate: true,
            idle_out: false,
            latch: None,
            read_next: ReadByte::Low,
            low_written: None,
        }
    }

    const fn modulus(&self) -> u32 {
        if self.bcd {
            10_000
        } else {
            0x1_0000
        }
    }

    /// The count in force and its run at `tick`, with a reload due by then
    /// applied.
    fn at(&self, tick: u64) -> (u32, Option<Run>) {
        match self.reload {
            Some(reload) if tick >= reload.at => {
                let n = self.cr.unwrap_or(self.ce);
                let phase = if reload.low_half {
                    u64::from(high_half(n))
                } else {
                    0
                };
                (
                    n,
                    Some(Run {
                        loaded: reload.at,
                        phase,
                    }),
                )
            }
            _ => (self.ce, self.run),
        }
    }

    /// Commits a reload whose tick has come.
    fn settle(&mut self, tick: u64) {
        if self.reload.is_some_and(|r| tick >= r.at) {
            (self.ce, self.run) = self.at(tick);
            self.reload = None;
        }
    }

    fn out_at(&self, tick: u64) -> bool {
        let (n, run) = self.at(tick);
        match run.and_then(|r| r.position(tick)) {
            Some(e) => output(self.mode, n, e),
            None => self.idle_out,
        }
    }

    /// The count's value at `tick`, before any BCD encoding.
    fn value_at(&self, tick: u64) -> u32 {
        let (n, run) = self.at(tick);
        match run.and_then(|r| r.position(tick)).or(self.paused) {
            Some(e) => count(self.mode, n, e, self.modulus()),
            None if run.is_some() => self.held,
            None => n % self.modulus(),
        }
    }

    fn count_at(&self, tick: u64) -> u16 {
        let value = u16::try_from(self.value_at(tick)).unwrap_or(0);
        if self.bcd {
            to_bcd(value)
        } else {
            value
        }
    }

    fn next_change(&self, after: u64) -> Option<u64> {
        let now = self.out_at(after);
        let (n, run) = self.at(after);
        let change = run.and_then(|r| change_after(self.mode, n, r, after, now));
        match self.reload {
            Some(reload) if reload.at > after && change.is_none_or(|c| c >= reload.at) => {
                let (n, run) = self.at(reload.at);
                let run = run?;
                if output(self.mode, n, run.phase) == now {
                    change_after(self.mode, n, run, reload.at, now)
                } else {
                    Some(reload.at)
                }
            }
            _ => change,
        }
    }

    fn next_rise(&self, after: u64) -> Option<u64> {
        let change = self.next_change(after)?;
        if self.out_at(change) {
            Some(change)
        } else {
            self.next_change(change)
        }
    }

    fn rises_between(&self, a: u64, b: u64) -> u64 {
        if b <= a {
            return 0;
        }
        if matches!(self.mode, 2 | 3) {
            return match self.reload {
                Some(reload) if reload.at > a && reload.at <= b => {
                    let (n, run) = self.at(a);
                    let (m, next) = self.at(reload.at);
                    run.map_or(0, |r| periodic_rises(n, r, a, reload.at))
                        + next.map_or(0, |r| periodic_rises(m, r, reload.at, b))
                }
                _ => {
                    let (n, run) = self.at(b);
                    run.map_or(0, |r| periodic_rises(n, r, a, b))
                }
            };
        }
        // The one-shot modes rise at most once per count.
        let mut rises = 0;
        let mut t = a;
        while let Some(rise) = self.next_rise(t).filter(|&r| r <= b) {
            rises += 1;
            t = rise;
        }
        rises
    }

    fn control(&mut self, value: u8) {
        self.access = match (value >> 4) & 3 {
            1 => Access::Lsb,
            2 => Access::Msb,
            _ => Access::Word,
        };
        self.mode = match (value >> 1) & 7 {
            6 => 2,
            7 => 3,
            m => m,
        };
        self.bcd = value & 1 != 0;
        self.cr = None;
        self.run = None;
        self.reload = None;
        self.paused = None;
        self.latch = None;
        self.read_next = ReadByte::Low;
        self.low_written = None;
        self.idle_out = self.mode != 0;
    }

    /// Loads the count register's count at the tick after `tick`; until then
    /// the counter shows and outputs what it did at `tick`.
    fn restart(&mut self, tick: u64) {
        if let Some(n) = self.cr {
            self.held = self.value_at(tick);
            self.idle_out = self.out_at(tick);
            self.ce = n;
            self.run = Some(Run {
                loaded: tick + 1,
                phase: 0,
            });
            self.paused = None;
        }
    }

    fn load(&mut self, raw: u16, tick: u64) {
        let value = if self.bcd {
            from_bcd(raw)
        } else {
            u32::from(raw)
        };
        let n = if value == 0 { self.modulus() } else { value };
        self.cr = Some(n);
        let counting = self.run.and_then(|r| r.position(tick));
        match self.mode {
            2 | 3 if counting.is_some() => {
                if self.reload.is_none() {
                    self.reload = Some(self.boundary(tick));
                }
            }
            1 | 5 => {}
            2 | 3 => {
                if self.gate {
                    self.restart(tick);
                }
            }
            _ => {
                if self.gate {
                    self.restart(tick);
                } else {
                    self.ce = n;
                    self.run = None;
                    self.paused = Some(0);
                }
                if self.mode == 0 {
                    // Writing a count drives mode 0's output low at once.
                    self.idle_out = false;
                }
            }
        }
    }

    /// The next period end (mode 2) or half-period end (mode 3) after
    /// `tick`, where a count written now takes over.
    fn boundary(&self, tick: u64) -> Reload {
        let n = u64::from(self.ce);
        let e = self.run.and_then(|r| r.position(tick)).unwrap_or(0);
        let p = e % n;
        let h = n.div_ceil(2);
        if self.mode == 3 && p < h {
            Reload {
                at: tick + (h - p),
                low_half: true,
            }
        } else {
            Reload {
                at: tick + (n - p),
                low_half: false,
            }
        }
    }

    fn write(&mut self, value: u8, tick: u64) {
        self.settle(tick);
        match self.access {
            Access::Lsb => self.load(u16::from(value), tick),
            Access::Msb => self.load(u16::from(value) << 8, tick),
            Access::Word => {
                if let Some(low) = self.low_written.take() {
                    self.load(crate::bits::word(low, value), tick);
                } else {
                    self.low_written = Some(value);
                    if self.mode == 0 {
                        // Mode 0 stops counting between the two bytes, with
                        // its output low.
                        self.paused = self.run.and_then(|r| r.position(tick)).or(self.paused);
                        self.run = None;
                        self.idle_out = false;
                    }
                }
            }
        }
    }

    fn read(&mut self, tick: u64) -> u8 {
        self.settle(tick);
        let value = self.latch.unwrap_or_else(|| self.count_at(tick));
        let high = match self.access {
            Access::Lsb => false,
            Access::Msb => true,
            Access::Word => self.read_next == ReadByte::High,
        };
        if self.access == Access::Word {
            self.read_next = if high { ReadByte::Low } else { ReadByte::High };
        }
        if high || self.access == Access::Lsb {
            self.latch = None;
        }
        if high {
            crate::bits::hi(value)
        } else {
            crate::bits::lo(value)
        }
    }

    fn set_gate(&mut self, high: bool, tick: u64) {
        self.settle(tick);
        if high == self.gate {
            return;
        }
        self.gate = high;
        match (self.mode, high) {
            (1 | 2 | 3 | 5, true) => self.restart(tick),
            (2 | 3, false) => {
                // The output goes high at once and the count waits for the
                // next rising edge.
                self.run = None;
                self.reload = None;
                self.idle_out = true;
            }
            (0 | 4, false) => {
                if let Some(run) = self.run.take() {
                    self.idle_out = self.out_at_run(run, tick);
                    self.paused = Some(run.position(tick).unwrap_or(0));
                }
            }
            (0 | 4, true) => {
                if let Some(e) = self.paused.take() {
                    self.run = Some(Run {
                        loaded: tick,
                        phase: e,
                    });
                }
            }
            _ => {}
        }
    }

    fn out_at_run(&self, run: Run, tick: u64) -> bool {
        run.position(tick)
            .map_or(self.idle_out, |e| output(self.mode, self.ce, e))
    }
}

/// The three-counter 8253.
#[derive(Clone, Debug)]
pub struct Pit {
    counters: [Counter; 3],
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}

impl Pit {
    /// A timer as it powers up: every counter idle in mode 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counters: [Counter::new(), Counter::new(), Counter::new()],
        }
    }

    /// A write to port 40h–43h (`port` 0–3) during `tick`.
    pub fn write(&mut self, port: u8, value: u8, tick: u64) {
        match port & 3 {
            3 => {
                let select = usize::from(value >> 6);
                // The 8253 has no read-back command: select 3 is ignored.
                let Some(counter) = self.counters.get_mut(select) else {
                    return;
                };
                counter.settle(tick);
                if value & 0x30 == 0 {
                    if counter.latch.is_none() {
                        counter.latch = Some(counter.count_at(tick));
                        counter.read_next = ReadByte::Low;
                    }
                } else {
                    counter.control(value);
                }
            }
            n => self.counters[usize::from(n)].write(value, tick),
        }
    }

    /// A read of port 40h–42h (`port` 0–2); the control port reads high.
    pub fn read(&mut self, port: u8, tick: u64) -> u8 {
        match port & 3 {
            3 => 0xFF,
            n => self.counters[usize::from(n)].read(tick),
        }
    }

    /// Drives counter `n`'s gate input during `tick`.
    pub fn set_gate(&mut self, n: usize, high: bool, tick: u64) {
        if let Some(counter) = self.counters.get_mut(n) {
            counter.set_gate(high, tick);
        }
    }

    /// Counter `n`'s output at `tick`.
    #[must_use]
    pub fn out(&self, n: usize, tick: u64) -> bool {
        self.counters.get(n).is_some_and(|c| c.out_at(tick))
    }

    /// The first tick after `after` at which counter `n`'s output changes.
    #[must_use]
    pub fn next_change(&self, n: usize, after: u64) -> Option<u64> {
        self.counters.get(n)?.next_change(after)
    }

    /// Counter `n`'s output at `tick` while it repeats, in modes 2 and 3:
    /// the ticks of each period it is high, and the period. A count written
    /// since stands from its reload.
    #[must_use]
    pub fn duty(&self, n: usize, tick: u64) -> Option<(u32, u32)> {
        let counter = self.counters.get(n)?;
        let (count, run) = counter.at(tick);
        run.and_then(|r| r.position(tick))?;
        match counter.mode {
            2 => Some((count - 1, count)),
            3 => Some((high_half(count), count)),
            _ => None,
        }
    }

    /// The tick a count written to counter `n` takes over, if one waits.
    #[must_use]
    pub fn reload_at(&self, n: usize) -> Option<u64> {
        self.counters.get(n)?.reload.map(|r| r.at)
    }

    /// The first tick after `after` at which counter `n`'s output rises.
    #[must_use]
    pub fn next_rise(&self, n: usize, after: u64) -> Option<u64> {
        self.counters.get(n)?.next_rise(after)
    }

    /// How many times counter `n`'s output rose in the ticks `(a, b]`.
    #[must_use]
    pub fn rises_between(&self, n: usize, a: u64, b: u64) -> u64 {
        self.counters.get(n).map_or(0, |c| c.rises_between(a, b))
    }
}

crate::snapshot::state_enum!(Access {
    0 => Lsb,
    1 => Msb,
    2 => Word,
});

crate::snapshot::state_enum!(ReadByte {
    0 => Low,
    1 => High,
});

crate::snapshot::state!(Run { loaded, phase });

crate::snapshot::state!(Reload { at, low_half });

/// Whether `n` is a count a counter can hold: one to a full binary period.
fn holdable(n: u32) -> bool {
    (1..=0x1_0000).contains(&n)
}

crate::snapshot::state!(
    Counter {
        mode,
        bcd,
        access,
        cr,
        ce,
        run,
        reload,
        paused,
        held,
        gate,
        idle_out,
        latch,
        read_next,
        low_written,
    }
    check |counter: &Counter| {
        counter.mode <= 5
            && holdable(counter.ce)
            && counter.cr.is_none_or(holdable)
            && counter.held <= 0x1_0000
    }
);

crate::snapshot::state!(Pit { counters });

#[cfg(test)]
#[path = "pit_tests.rs"]
mod tests;

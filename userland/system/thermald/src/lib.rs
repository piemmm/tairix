//! TAIRiX cooling service (`thermald`) — the policy half (`plans/COOLING.md`
//! CL6).
//!
//! `thermald` reads the processor's temperature sensors, switches the CPU fan
//! on the board's header line by the machine's `cooling.fan.*` policy, and
//! answers the ungated `thermal-v1` status. This crate is that service over
//! an injected [`Machine`], so all of it is host-tested; `src/run.rs` wires
//! the endpoints, the settings directory's watch and the hardware tree.
//!
//! # Nothing here polls
//!
//! An automatic fan is sampled once a [`SAMPLE_PERIOD_US`] on a deadline
//! [`Thermald::deadline_us`] states, and a call the header left unanswered is
//! asked again on the same cadence until it is answered; a fan that is off,
//! always on or has no line to switch, and owes the header no call, states
//! none, so the service sleeps until its policy, the hardware tree or a
//! caller wakes it. A status is answered from readings at most
//! [`READING_MAX_AGE_US`] old, so no number of callers reads a sensor more
//! often than that.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod fan;
pub mod inventory;

#[cfg(test)]
mod tests;

use tairix_abi::driver::gpio::{HeaderLines, Holder, LineState, LineTable, Polarity};
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::thermal_ipc::{FanLines, FanPolicy, ThermalStatus};
use tairix_abi::Errno;

pub use fan::{Entry, Fan, Header, Journal, Reading};
pub use inventory::Inventory;

/// How often an automatic fan is decided, and a call its header left
/// unanswered asked again, in microseconds: a second, quick against how
/// slowly a board's temperature moves.
pub const SAMPLE_PERIOD_US: u64 = 1_000_000;

/// The oldest a reading a status reports may be, in microseconds.
pub const READING_MAX_AGE_US: u64 = 1_000_000;

/// The machine the service reaches through its endpoints.
pub trait Machine {
    /// The sensor serving `endpoint`'s reading.
    ///
    /// # Errors
    ///
    /// The sensor's refusal, or the call's.
    fn read_sensor(&mut self, endpoint: u64) -> Result<Millicelsius, Errno>;

    /// Ask the header serving `endpoint` to lend `line`.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn claim(
        &mut self,
        endpoint: u64,
        line: u8,
        polarity: Polarity,
        asserted: bool,
    ) -> Result<(), Errno>;

    /// Ask the header serving `endpoint` to switch the lent `line`.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn drive(&mut self, endpoint: u64, line: u8, asserted: bool) -> Result<(), Errno>;

    /// Give `line` back to the header serving `endpoint`.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn release(&mut self, endpoint: u64, line: u8) -> Result<(), Errno>;

    /// Every line the header serving `endpoint` lends, and how each stands.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn lines(&mut self, endpoint: u64) -> Result<LineTable, Errno>;

    /// The monotonic clock, in microseconds.
    fn now_us(&self) -> u64;
}

/// The header serving `endpoint`, as the fan drives it.
struct HeaderAt<'m> {
    machine: &'m mut dyn Machine,
    endpoint: u64,
}

impl Header for HeaderAt<'_> {
    fn claim(&mut self, line: u8, polarity: Polarity, asserted: bool) -> Result<(), Errno> {
        self.machine.claim(self.endpoint, line, polarity, asserted)
    }

    fn drive(&mut self, line: u8, asserted: bool) -> Result<(), Errno> {
        self.machine.drive(self.endpoint, line, asserted)
    }

    fn release(&mut self, line: u8) -> Result<(), Errno> {
        self.machine.release(self.endpoint, line)
    }
}

/// A reading a status reports, and when it was taken.
#[derive(Copy, Clone, Debug)]
struct Taken<T> {
    at_us: u64,
    value: T,
}

impl<T: Copy> Taken<T> {
    /// The reading, unless it is older than [`READING_MAX_AGE_US`] at `now`.
    fn fresh(slot: Option<Self>, now: u64) -> Option<T> {
        slot.filter(|taken| now.saturating_sub(taken.at_us) < READING_MAX_AGE_US)
            .map(|taken| taken.value)
    }
}

/// The cooling service: the fan, what the tree holds for it, and the
/// readings its status reports.
#[derive(Debug, Default)]
pub struct Thermald {
    fan: Fan,
    inventory: Inventory,
    temperature: Option<Taken<Option<Millicelsius>>>,
    lines: Option<Taken<Option<FanLines>>>,
    next_due_us: Option<u64>,
}

impl Thermald {
    /// The fan, as the service drives it.
    #[must_use]
    pub const fn fan(&self) -> &Fan {
        &self.fan
    }

    /// Follow `policy` from now on.
    pub fn policy_changed(
        &mut self,
        policy: FanPolicy,
        machine: &mut dyn Machine,
        journal: &mut dyn Journal,
    ) {
        let header = self.inventory.header;
        self.fan.apply(
            policy,
            header_at(machine, header).as_mut().map(as_header),
            journal,
        );
        self.settled(machine);
    }

    /// The hardware tree now holds `inventory`.
    pub fn inventory_changed(
        &mut self,
        inventory: Inventory,
        machine: &mut dyn Machine,
        journal: &mut dyn Journal,
    ) {
        if inventory.header != self.inventory.header {
            self.fan.header_gone(journal);
        }
        self.inventory = inventory;
        let header = self.inventory.header;
        self.fan
            .reconcile(header_at(machine, header).as_mut().map(as_header), journal);
        self.settled(machine);
    }

    /// When, on [`Machine::now_us`]'s clock, the fan is next due a sample, or
    /// a call its header left unanswered is due again; [`None`] while it owes
    /// neither.
    #[must_use]
    pub const fn deadline_us(&self) -> Option<u64> {
        self.next_due_us
    }

    /// The deadline came: decide an automatic fan, or ask the header again
    /// what it left unanswered, and arm the next.
    pub fn expired(&mut self, machine: &mut dyn Machine, journal: &mut dyn Journal) {
        let header = self.inventory.header;
        if self.fan.samples() {
            let reading = self.read(machine);
            self.fan.sample(
                reading,
                header_at(machine, header).as_mut().map(as_header),
                journal,
            );
            self.temperature = Some(Taken {
                at_us: machine.now_us(),
                value: reading.temperature(),
            });
        } else if self.fan.unanswered() {
            self.fan
                .reconcile(header_at(machine, header).as_mut().map(as_header), journal);
        }
        let now = machine.now_us();
        self.next_due_us = (self.fan.samples() || self.fan.unanswered())
            .then(|| now.saturating_add(SAMPLE_PERIOD_US));
    }

    /// The status a caller is answered: readings at most
    /// [`READING_MAX_AGE_US`] old.
    pub fn status(&mut self, machine: &mut dyn Machine) -> ThermalStatus {
        let now = machine.now_us();
        let temperature = Taken::fresh(self.temperature, now).unwrap_or_else(|| {
            let value = self.read(machine).temperature();
            self.temperature = Some(Taken { at_us: now, value });
            value
        });
        let lines = Taken::fresh(self.lines, now).unwrap_or_else(|| {
            let value = self
                .inventory
                .header
                .and_then(|header| machine.lines(header).ok())
                .and_then(|table| fan_lines(&table));
            self.lines = Some(Taken { at_us: now, value });
            value
        });
        ThermalStatus {
            temperature,
            policy: self.fan.policy(),
            running: self.fan.running(),
            fault: self.fan.fault(),
            lines,
        }
    }

    fn read(&self, machine: &mut dyn Machine) -> Reading {
        Reading::hottest(
            self.inventory
                .sensors
                .iter()
                .map(|&sensor| machine.read_sensor(sensor)),
        )
    }

    /// The fan was brought into step with its policy and the tree: what a
    /// status reported of the lines is stale, an automatic fan is due a
    /// sample at once, and a call the header left unanswered is due again a
    /// period later.
    fn settled(&mut self, machine: &dyn Machine) {
        self.lines = None;
        let now = machine.now_us();
        self.next_due_us = if self.fan.samples() {
            Some(now)
        } else {
            self.fan
                .unanswered()
                .then(|| now.saturating_add(SAMPLE_PERIOD_US))
        };
    }
}

/// The header's `table`, as the service's own caller sees it, told as the
/// fan may use it: a line the fan may take is free, or the fan's already.
fn fan_lines(table: &LineTable) -> Option<FanLines> {
    let (lent, free) = table
        .entries()
        .iter()
        .fold((0u64, 0u64), |(lent, free), entry| {
            let bit = 1u64.checked_shl(u32::from(entry.line)).unwrap_or(0);
            let takes = matches!(
                entry.state,
                LineState::Free
                    | LineState::Output {
                        holder: Holder::Caller,
                        ..
                    }
            );
            (lent | bit, if takes { free | bit } else { free })
        });
    FanLines::new(HeaderLines::from_mask(lent), HeaderLines::from_mask(free))
}

fn header_at(machine: &mut dyn Machine, endpoint: Option<u64>) -> Option<HeaderAt<'_>> {
    endpoint.map(|endpoint| HeaderAt { machine, endpoint })
}

fn as_header<'a>(header: &'a mut HeaderAt<'_>) -> &'a mut dyn Header {
    header
}

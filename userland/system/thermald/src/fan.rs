//! The CPU fan: the line its policy names, held through the board's header,
//! and the decision whether it runs.
//!
//! [`Fan`] is the one place the policy meets the hardware. It holds the line
//! only while the policy switches the fan, releasing it the moment the policy
//! names another line, another polarity or no fan at all, and decides each
//! automatic sample with the policy's hysteresis. A reading it cannot take,
//! or one no processor could be at, runs the fan: a fan that runs needlessly
//! costs noise, one that stops needlessly costs the processor.
//!
//! A call the header leaves unanswered says nothing of what the header did,
//! so it is never taken for a refusal: the claim is kept, the fan is decided
//! as if it may be running, and the call is asked again a period later. A
//! line is claimed only once the header has answered that it holds none of
//! the fan's, so a claim that landed unanswered is never refused as busy.

use core::ops::RangeInclusive;

use tairix_abi::driver::gpio::Polarity;
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::thermal_ipc::{FanFault, FanMode, FanPolicy};
use tairix_abi::Errno;

/// The temperatures a working processor die can read: no colder than the
/// coldest ambient silicon is rated to run in (−55 °C, the military grade)
/// and no hotter than its absolute-maximum junction temperature (150 °C).
/// Outside them a sensor is stuck or mis-calibrated, not reading the die.
const PLAUSIBLE: RangeInclusive<Millicelsius> = Millicelsius(-55_000)..=Millicelsius(150_000);

/// Whether `refusal` says the header never answered — the call's deadline
/// passed, or it could not be queued — so what the header did is unknown.
const fn went_unanswered(refusal: Errno) -> bool {
    matches!(refusal, Errno::TimedOut | Errno::WouldBlock)
}

/// The header the fan's line is lent by, as the fan drives it.
pub trait Header {
    /// Take `line` as an output with `polarity`, its load `asserted`.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn claim(&mut self, line: u8, polarity: Polarity, asserted: bool) -> Result<(), Errno>;

    /// Switch the held `line`'s load on (`asserted`) or off.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn drive(&mut self, line: u8, asserted: bool) -> Result<(), Errno>;

    /// Give `line` back.
    ///
    /// # Errors
    ///
    /// The header's refusal, or the call's.
    fn release(&mut self, line: u8) -> Result<(), Errno>;
}

/// The processor's temperature, as one sample found it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reading {
    /// The hottest processor sensor's reading.
    Measured(Millicelsius),
    /// The machine has no processor sensor.
    NoSensor,
    /// A processor sensor could not be read, for this reason, or read what no
    /// processor could be at ([`Errno::OutOfRange`]).
    Failed(Errno),
}

impl Reading {
    /// The sample `readings`, one per processor sensor, make: the hottest,
    /// unless any could not be read or is implausible, which a hotter one
    /// might have been.
    pub fn hottest(readings: impl IntoIterator<Item = Result<Millicelsius, Errno>>) -> Self {
        let mut hottest = None;
        for reading in readings {
            match reading {
                Ok(reading) if PLAUSIBLE.contains(&reading) => {
                    hottest = hottest.max(Some(reading));
                }
                Ok(_) => return Self::Failed(Errno::OutOfRange),
                Err(refusal) => return Self::Failed(refusal),
            }
        }
        hottest.map_or(Self::NoSensor, Self::Measured)
    }

    /// The temperature, when one was measured.
    #[must_use]
    pub const fn temperature(self) -> Option<Millicelsius> {
        match self {
            Self::Measured(temperature) => Some(temperature),
            Self::NoSensor | Self::Failed(_) => None,
        }
    }
}

/// What the fan did, as the service's log records it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    /// The fan's line was taken.
    Claimed {
        /// The line.
        line: u8,
    },
    /// The fan's line was given back.
    Released {
        /// The line.
        line: u8,
    },
    /// The header refused to switch the fan's line, so it no longer honours
    /// the claim: its controller restarted holding none, or ended, or a claim
    /// it left unanswered never landed.
    Lost {
        /// The line.
        line: u8,
        /// The refusal.
        refusal: Errno,
    },
    /// The fan was switched on or off.
    Switched {
        /// Whether it now runs.
        running: bool,
    },
    /// The fan stopped being as its policy asks.
    Fault {
        /// Why.
        fault: FanFault,
        /// What is behind it: the refusal of the call that met it, or
        /// [`Errno::OutOfRange`] for a reading no processor could be at.
        refusal: Option<Errno>,
    },
    /// The fan is as its policy asks again.
    Recovered,
}

/// Where the fan's [`Entry`]s go.
pub trait Journal {
    /// Record `entry`.
    fn record(&mut self, entry: Entry);
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Claim {
    line: u8,
    polarity: Polarity,
}

/// A fault, and what is behind it where something is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Fault {
    fault: FanFault,
    refusal: Option<Errno>,
}

impl Fault {
    const fn stated(fault: FanFault) -> Self {
        Self {
            fault,
            refusal: None,
        }
    }

    const fn refused(fault: FanFault, refusal: Errno) -> Self {
        Self {
            fault,
            refusal: Some(refusal),
        }
    }
}

/// The CPU fan and the policy it follows.
#[derive(Debug, Default)]
pub struct Fan {
    policy: FanPolicy,
    held: Option<Claim>,
    running: bool,
    /// The header left the last call about `held` unanswered, so whether it
    /// holds the claim, and at what level, is unknown.
    unanswered: bool,
    line_fault: Option<Fault>,
    sensor_fault: Option<Fault>,
}

impl Fan {
    /// The policy in effect.
    #[must_use]
    pub const fn policy(&self) -> FanPolicy {
        self.policy
    }

    /// Whether the fan's line is driven on, as the header last confirmed.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// The line held for the fan, or whose claim the header has yet to
    /// answer.
    #[must_use]
    pub fn line(&self) -> Option<u8> {
        self.held.map(|claim| claim.line)
    }

    /// Why the fan is not as its policy asks: what keeps the line from being
    /// driven first, then what keeps the policy from deciding.
    #[must_use]
    pub fn fault(&self) -> Option<FanFault> {
        self.standing().map(|standing| standing.fault)
    }

    /// Whether the fan is automatic and has a line to switch, so samples
    /// decide something.
    #[must_use]
    pub fn samples(&self) -> bool {
        self.policy.mode() == FanMode::Automatic && self.held.is_some()
    }

    /// Whether the header left a call about the fan's line unanswered, so it
    /// is asked again a period later.
    #[must_use]
    pub const fn unanswered(&self) -> bool {
        self.unanswered
    }

    /// Follow `policy` from now on, through `header` if one serves.
    pub fn apply(
        &mut self,
        policy: FanPolicy,
        header: Option<&mut dyn Header>,
        journal: &mut dyn Journal,
    ) {
        self.policy = policy;
        if policy.mode() != FanMode::Automatic {
            self.sensor_fault = None;
        }
        self.reconcile(header, journal);
    }

    /// Bring the line into step with the policy: release a claim it no longer
    /// wants, and take or re-assert the one it does where `header` lends it.
    pub fn reconcile(&mut self, mut header: Option<&mut dyn Header>, journal: &mut dyn Journal) {
        let before = self.standing();
        let wanted = self.policy.line().filter(|_| self.policy.mode().drives());
        if let Some(claim) = self.held {
            if Some(claim.line) != wanted || claim.polarity != self.policy.polarity() {
                if let Some(header) = header.as_deref_mut() {
                    if let Some(fault) = self.give_back(claim, header, journal) {
                        // Nothing is claimed until the header answers that the
                        // old line is no longer the fan's.
                        self.line_fault = Some(fault);
                        self.note(before, journal);
                        return;
                    }
                } else {
                    self.forget(journal);
                }
            }
        }
        self.line_fault = match (self.policy.mode().drives(), wanted, header) {
            (false, _, _) => None,
            (true, None, _) => Some(Fault::stated(FanFault::NoLine)),
            (true, Some(_), None) => Some(Fault::stated(FanFault::NoHeader)),
            // A held line is driven again rather than trusted, so a controller
            // that restarted holding no claim is found out.
            (true, Some(_), Some(header)) if self.held.is_some() => {
                self.drive(self.level(), header, journal)
            }
            (true, Some(line), Some(header)) => self.claim(line, header, journal),
        };
        self.note(before, journal);
    }

    /// The header went with its controller's driver: whatever was held went
    /// with it.
    pub fn header_gone(&mut self, journal: &mut dyn Journal) {
        if self.held.is_some() {
            self.forget(journal);
        }
        if self.policy.mode().drives() && self.policy.line().is_some() {
            let before = self.standing();
            self.line_fault = Some(Fault::stated(FanFault::NoHeader));
            self.note(before, journal);
        }
    }

    /// Decide an automatic fan from `reading`.
    pub fn sample(
        &mut self,
        reading: Reading,
        header: Option<&mut dyn Header>,
        journal: &mut dyn Journal,
    ) {
        if self.policy.mode() != FanMode::Automatic {
            return;
        }
        let before = self.standing();
        let run = match reading {
            Reading::Measured(temperature) => {
                self.sensor_fault = None;
                self.policy.runs_at(temperature, self.may_run())
            }
            Reading::NoSensor => {
                self.sensor_fault = Some(Fault::stated(FanFault::NoSensor));
                true
            }
            Reading::Failed(refusal) => {
                self.sensor_fault = Some(Fault::refused(FanFault::SensorFailed, refusal));
                true
            }
        };
        // Recorded before the switch, so a reading that cannot be taken is
        // logged ahead of the fan it starts.
        self.note(before, journal);
        if self.held.is_none() || (run == self.running && !self.unanswered) {
            return;
        }
        let before = self.standing();
        self.line_fault = match header {
            Some(header) => self.drive(run, header, journal),
            None => Some(Fault::stated(FanFault::NoHeader)),
        };
        self.note(before, journal);
    }

    /// Whether the fan may be running: it was switched on, or the header has
    /// yet to answer what it is.
    const fn may_run(&self) -> bool {
        self.running || self.unanswered
    }

    /// The level the held line is driven at: on while the fan is always on or
    /// may be running, off otherwise.
    fn level(&self) -> bool {
        self.policy.mode() == FanMode::On || self.may_run()
    }

    /// Take `line`, answering the fault that stops the fan being driven. A
    /// claim left unanswered may have landed, so it is held until the header
    /// says.
    fn claim(
        &mut self,
        line: u8,
        header: &mut dyn Header,
        journal: &mut dyn Journal,
    ) -> Option<Fault> {
        let claim = Claim {
            line,
            polarity: self.policy.polarity(),
        };
        // An automatic fan runs until its first sample decides: a reading not
        // yet taken is not one that says it may stop.
        match header.claim(line, claim.polarity, true) {
            Ok(()) => {
                self.held = Some(claim);
                self.running = true;
                self.unanswered = false;
                journal.record(Entry::Claimed { line });
                journal.record(Entry::Switched { running: true });
                None
            }
            Err(refusal) => {
                if went_unanswered(refusal) {
                    self.held = Some(claim);
                    self.unanswered = true;
                }
                Some(Fault::refused(FanFault::LineUnavailable, refusal))
            }
        }
    }

    /// Drive the held line `run`, answering the fault that stops the fan
    /// being driven. A drive left unanswered keeps the claim; one refused
    /// means the header no longer honours it, so the line is given back and
    /// taken again.
    fn drive(
        &mut self,
        run: bool,
        header: &mut dyn Header,
        journal: &mut dyn Journal,
    ) -> Option<Fault> {
        let claim = self.held?;
        match header.drive(claim.line, run) {
            Ok(()) => {
                self.unanswered = false;
                if run != self.running {
                    self.running = run;
                    journal.record(Entry::Switched { running: run });
                }
                None
            }
            Err(refusal) if went_unanswered(refusal) => {
                self.unanswered = true;
                Some(Fault::refused(FanFault::LineUnavailable, refusal))
            }
            Err(refusal) => {
                journal.record(Entry::Lost {
                    line: claim.line,
                    refusal,
                });
                // Given back first in case the controller still records it.
                if let Some(fault) = self.give_back(claim, header, journal) {
                    return Some(fault);
                }
                self.claim(claim.line, header, journal)
            }
        }
    }

    /// Give `claim` back, answering the fault while the header leaves the
    /// release unanswered: the claim is kept, since the line may still be
    /// the fan's.
    fn give_back(
        &mut self,
        claim: Claim,
        header: &mut dyn Header,
        journal: &mut dyn Journal,
    ) -> Option<Fault> {
        match header.release(claim.line) {
            Err(refusal) if went_unanswered(refusal) => {
                self.unanswered = true;
                Some(Fault::refused(FanFault::LineUnavailable, refusal))
            }
            // Any answer leaves the line none of the fan's: the claim is gone
            // either way.
            _ => {
                self.forget(journal);
                None
            }
        }
    }

    fn forget(&mut self, journal: &mut dyn Journal) {
        if let Some(claim) = self.held.take() {
            journal.record(Entry::Released { line: claim.line });
        }
        self.running = false;
        self.unanswered = false;
    }

    /// What stands between the fan and its policy: the line first, then the
    /// sensors.
    fn standing(&self) -> Option<Fault> {
        self.line_fault.or(self.sensor_fault)
    }

    /// Record the fault's change from `before`: once as it begins, with what
    /// is behind it, and once as it clears.
    fn note(&self, before: Option<Fault>, journal: &mut dyn Journal) {
        match (before, self.standing()) {
            (Some(_), None) => journal.record(Entry::Recovered),
            (previous, Some(now)) if previous.map(|fault| fault.fault) != Some(now.fault) => {
                journal.record(Entry::Fault {
                    fault: now.fault,
                    refusal: now.refusal,
                });
            }
            _ => {}
        }
    }
}

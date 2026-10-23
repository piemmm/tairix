//! The CPU fan: the line its policy names, held through the board's header,
//! and the decision whether it runs.
//!
//! [`Fan`] is the one place the policy meets the hardware. It holds the line
//! only while the policy switches the fan, releasing it the moment the policy
//! names another line, another polarity or no fan at all, and decides each
//! automatic sample with the policy's hysteresis. A reading it cannot take
//! runs the fan: a fan that runs needlessly costs noise, one that stops
//! needlessly costs the processor.

use tairix_abi::driver::gpio::Polarity;
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::thermal_ipc::{FanFault, FanMode, FanPolicy};
use tairix_abi::Errno;

/// The header the fan's line is lent by, as the fan drives it.
pub trait Header {
    /// Take `line` as an output with `polarity`, its load `asserted`.
    ///
    /// # Errors
    ///
    /// The header's refusal.
    fn claim(&mut self, line: u8, polarity: Polarity, asserted: bool) -> Result<(), Errno>;

    /// Switch the held `line`'s load on (`asserted`) or off.
    ///
    /// # Errors
    ///
    /// The header's refusal.
    fn drive(&mut self, line: u8, asserted: bool) -> Result<(), Errno>;

    /// Give `line` back.
    ///
    /// # Errors
    ///
    /// The header's refusal.
    fn release(&mut self, line: u8) -> Result<(), Errno>;
}

/// The processor's temperature, as one sample found it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reading {
    /// The hottest processor sensor's reading.
    Measured(Millicelsius),
    /// The machine has no processor sensor.
    NoSensor,
    /// A processor sensor could not be read.
    Failed,
}

impl Reading {
    /// The sample `readings`, one per processor sensor, make: the hottest,
    /// unless any could not be read, which a hotter one might have been.
    pub fn hottest(readings: impl IntoIterator<Item = Result<Millicelsius, Errno>>) -> Self {
        let mut hottest = None;
        for reading in readings {
            match reading {
                Ok(reading) => hottest = hottest.max(Some(reading)),
                Err(_) => return Self::Failed,
            }
        }
        hottest.map_or(Self::NoSensor, Self::Measured)
    }

    /// The temperature, when one was measured.
    #[must_use]
    pub const fn temperature(self) -> Option<Millicelsius> {
        match self {
            Self::Measured(temperature) => Some(temperature),
            Self::NoSensor | Self::Failed => None,
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
    /// the claim: its controller restarted holding none, or ended.
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
        /// The refusal behind it, where a call was refused.
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

/// The CPU fan and the policy it follows.
#[derive(Debug, Default)]
pub struct Fan {
    policy: FanPolicy,
    held: Option<Claim>,
    running: bool,
    line_fault: Option<FanFault>,
    sensor_fault: Option<FanFault>,
}

impl Fan {
    /// The policy in effect.
    #[must_use]
    pub const fn policy(&self) -> FanPolicy {
        self.policy
    }

    /// Whether the fan's line is driven on.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.running
    }

    /// The line held for the fan.
    #[must_use]
    pub fn line(&self) -> Option<u8> {
        self.held.map(|claim| claim.line)
    }

    /// Why the fan is not as its policy asks: what keeps the line from being
    /// driven first, then what keeps the policy from deciding.
    #[must_use]
    pub fn fault(&self) -> Option<FanFault> {
        self.line_fault.or(self.sensor_fault)
    }

    /// Whether the fan is automatic and has a line to switch, so samples
    /// decide something.
    #[must_use]
    pub fn samples(&self) -> bool {
        self.policy.mode() == FanMode::Automatic && self.held.is_some()
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
        let before = self.fault();
        let wanted = self.policy.line().filter(|_| self.policy.mode().drives());
        if let Some(claim) = self.held {
            if Some(claim.line) != wanted || claim.polarity != self.policy.polarity() {
                if let Some(header) = header.as_deref_mut() {
                    // A refusal leaves the line nobody's: the claim is gone
                    // either way.
                    let _ = header.release(claim.line);
                }
                self.forget(journal);
            }
        }
        self.line_fault = match (self.policy.mode().drives(), wanted, header) {
            (false, _, _) => None,
            (true, None, _) => Some(FanFault::NoLine),
            (true, Some(_), None) => Some(FanFault::NoHeader),
            // A held line is driven again rather than trusted, so a controller
            // that restarted holding no claim is found out.
            (true, Some(_), Some(header)) if self.held.is_some() => {
                self.drive(self.running, header, journal)
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
            let before = self.fault();
            self.line_fault = Some(FanFault::NoHeader);
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
        let before = self.fault();
        let run = match reading {
            Reading::Measured(temperature) => {
                self.sensor_fault = None;
                self.policy.runs_at(temperature, self.running)
            }
            Reading::NoSensor => {
                self.sensor_fault = Some(FanFault::NoSensor);
                true
            }
            Reading::Failed => {
                self.sensor_fault = Some(FanFault::SensorFailed);
                true
            }
        };
        // Recorded before the switch, so a reading that cannot be taken is
        // logged ahead of the fan it starts.
        self.note(before, journal);
        if self.held.is_none() || run == self.running {
            return;
        }
        let before = self.fault();
        self.line_fault = match header {
            Some(header) => self.drive(run, header, journal),
            None => Some(FanFault::NoHeader),
        };
        self.note(before, journal);
    }

    /// Take `line`, answering the fault that stops the fan being driven.
    fn claim(
        &mut self,
        line: u8,
        header: &mut dyn Header,
        journal: &mut dyn Journal,
    ) -> Option<FanFault> {
        // An automatic fan runs until its first sample decides: a reading not
        // yet taken is not one that says it may stop.
        let run = self.policy.mode().drives();
        match header.claim(line, self.policy.polarity(), run) {
            Ok(()) => {
                self.held = Some(Claim {
                    line,
                    polarity: self.policy.polarity(),
                });
                self.running = run;
                journal.record(Entry::Claimed { line });
                journal.record(Entry::Switched { running: run });
                None
            }
            Err(refusal) => {
                journal.record(Entry::Fault {
                    fault: FanFault::LineUnavailable,
                    refusal: Some(refusal),
                });
                Some(FanFault::LineUnavailable)
            }
        }
    }

    /// Drive the held line `run`, answering the fault that stops the fan
    /// being driven. A refusal means the header no longer honours the claim,
    /// so the line is given back and taken again.
    fn drive(
        &mut self,
        run: bool,
        header: &mut dyn Header,
        journal: &mut dyn Journal,
    ) -> Option<FanFault> {
        let claim = self.held?;
        match header.drive(claim.line, run) {
            Ok(()) => {
                if run != self.running {
                    self.running = run;
                    journal.record(Entry::Switched { running: run });
                }
                None
            }
            Err(refusal) => {
                self.held = None;
                self.running = false;
                journal.record(Entry::Lost {
                    line: claim.line,
                    refusal,
                });
                // Given back first in case the controller still records it.
                let _ = header.release(claim.line);
                self.claim(claim.line, header, journal)
            }
        }
    }

    fn forget(&mut self, journal: &mut dyn Journal) {
        if let Some(claim) = self.held.take() {
            journal.record(Entry::Released { line: claim.line });
        }
        self.running = false;
    }

    /// Record the fault's change from `before`; a refusal is recorded where
    /// it happens, with its reason.
    fn note(&self, before: Option<FanFault>, journal: &mut dyn Journal) {
        match (before, self.fault()) {
            (Some(_), None) => journal.record(Entry::Recovered),
            (previous, Some(fault))
                if previous != Some(fault) && fault != FanFault::LineUnavailable =>
            {
                journal.record(Entry::Fault {
                    fault,
                    refusal: None,
                });
            }
            _ => {}
        }
    }
}

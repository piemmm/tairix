//! The clocks the endpoint serves, as `clock-v1` drives them.
//!
//! The audio blocks' PCM and PWM clocks are run. The VPU clock is described
//! alone, for the peripherals whose dividers derive from it: the firmware
//! sets it as the core clock. The rest are the firmware's: it retunes the
//! cores', the memory's and the buses' as it pleases, and the board's own
//! wiring may depend on the general-purpose ones.

use tairix_abi::{DriverError, RegisterBlock};
use tairix_clock::{Applied, ClockHardware, Halt, Policy};

use crate::cprman::{Cprman, Generator, Halted, Setting, PCM, PWM};

/// The binding's id of the PWM blocks' clock.
pub const CLOCK_PWM: u32 = 30;

/// The binding's id of the PCM block's clock.
pub const CLOCK_PCM: u32 = 31;

/// The binding's id of the VPU clock.
pub const CLOCK_VPU: u32 = 20;

/// A clock the endpoint serves.
#[derive(Copy, Clone)]
enum Served {
    /// One the endpoint runs at an exact rate.
    Run(Generator),
    /// The VPU clock, which it describes and never changes.
    Vpu,
}

/// Every clock the endpoint serves, by its binding id.
const SERVED: [(u32, Served); 3] = [
    (CLOCK_PWM, Served::Run(PWM)),
    (CLOCK_PCM, Served::Run(PCM)),
    (CLOCK_VPU, Served::Vpu),
];

const fn halt(halted: Halted) -> Halt {
    match halted {
        Halted::Finished => Halt::Clean,
        Halted::Killed => Halt::Cut,
    }
}

/// The clock manager's served clocks: each generator run at an exact rate, so
/// one consumer's stream is never retimed under it by another, and the VPU
/// clock described.
pub struct Clocks<'r, R: RegisterBlock + ?Sized>(pub Cprman<'r, R>);

impl<R: RegisterBlock + ?Sized> Clocks<'_, R> {
    /// The generator clock `at` runs from, which only a run clock has.
    fn generator(at: usize) -> Result<Generator, DriverError> {
        match SERVED[at].1 {
            Served::Run(generator) => Ok(generator),
            Served::Vpu => Err(DriverError::Unsupported),
        }
    }
}

impl<R: RegisterBlock + ?Sized> ClockHardware for Clocks<'_, R> {
    /// A generator setting, with the rate it makes.
    type Setting = (Setting, u64);

    fn count(&self) -> usize {
        SERVED.len()
    }

    fn served(&self, selector: &[u32]) -> Option<usize> {
        let &[id] = selector else {
            return None;
        };
        SERVED.iter().position(|&(served, _)| served == id)
    }

    fn id(&self, at: usize) -> u32 {
        SERVED[at].0
    }

    fn policy(&self, at: usize) -> Policy {
        match SERVED[at].1 {
            Served::Run(_) => Policy::Exact,
            Served::Vpu => Policy::Fixed,
        }
    }

    fn rate(&self, at: usize) -> Result<u64, DriverError> {
        match SERVED[at].1 {
            Served::Run(generator) => self.0.rate(generator),
            Served::Vpu => self.0.vpu_rate(),
        }
    }

    fn max_rate(&self, at: usize) -> Result<u64, DriverError> {
        match SERVED[at].1 {
            Served::Run(_) => self.0.max_rate(),
            Served::Vpu => self.0.vpu_rate(),
        }
    }

    fn nearest(&self, _at: usize, hz: u64) -> Result<(Setting, u64), DriverError> {
        self.0.nearest(hz)
    }

    fn run(&mut self, at: usize, (setting, hz): (Setting, u64)) -> Result<Applied, DriverError> {
        let halt = self.0.run(Self::generator(at)?, setting).map(halt)?;
        Ok(Applied { halt, hz })
    }

    fn stop(&mut self, at: usize) -> Result<Halt, DriverError> {
        self.0.stop(Self::generator(at)?).map(halt)
    }
}

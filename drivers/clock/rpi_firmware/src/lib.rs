//! Raspberry Pi firmware clocks (`raspberrypi,firmware-clocks`).
//!
//! The `VideoCore` firmware owns the clocks this node names: no register a
//! driver can write reaches them, only a property exchange asking the
//! firmware to run one at a rate. So this driver maps no MMIO and takes no
//! interrupt; its only path to them is the mailbox service, which admits the
//! clocks this node's driver drives and no others.
//!
//! It serves two roles over them:
//!
//! - **The frequency mechanism.** The ARM core clock is the machine's DVFS
//!   seam: the kernel's governor publishes a target and [`ArmClock`] applies
//!   it. Without it the firmware leaves the cores at `arm_freq_min` — 600 MHz
//!   on a Pi 4B rated at 1.5 GHz — however much work there is.
//! - **The display's clocks.** The core clock the display engine runs from
//!   and the HDMI blocks' state-machine and pixel-path clocks are served over
//!   `clock-v1` to the nodes that link to them ([`LinkedClocks`]), each a
//!   floored clock its consumers share.
//!
//! The firmware clamps a request to the clock's range and rounds it to a rate
//! it can synthesise, so every rate reported is the firmware's own answer,
//! never the request echoed.
//!
//! Reference: the Raspberry Pi firmware's mailbox property interface (the
//! `raspberrypi/firmware` wiki, *Mailbox property interface*).

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use tairix_abi::cpufreq::CpuFreqLimits;
use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost};
use tairix_clock::{Applied, ClockHardware, Halt, Policy};
use tairix_vcmailbox::{
    query_clock_rate, set_clock_rate, ClockRateQuery, FirmwareClock, MailboxError,
    FIRMWARE_CLOCKS_KEY, LINKED_CLOCKS,
};

#[cfg(test)]
mod tests;

/// Per-driver `DriverHandle` marker returned by [`register`]; the host
/// re-issues its own. The bytes spell `"RPCF"`.
const REGISTER_HANDLE_MARKER: u64 = 0x5250_4346_0000_0001;

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table — the single source both the installed
/// bundle's signed manifest and the autoload match are built from. The device
/// is the mailbox service's own definition of it, so this driver binds
/// exactly the device its requests are admitted for.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(BIND_PRIORITY, FIRMWARE_CLOCKS_KEY)];

/// The capabilities the driver runs with, which its signed manifest requests:
/// the mailbox its clocks are reached through, the frequency mechanism role,
/// the node's clock endpoint, and the log its decisions go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MAILBOX,
    CapabilityId::CPUFREQ,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::LOG_EMIT,
];

/// Granularity the governor is asked to quantise its targets to, in Hz.
///
/// The firmware accepts any rate and rounds, so this is not a hardware
/// constraint but the interval worth *asking* at: finer would spend a
/// firmware round trip on a change too small to matter, coarser would leave
/// usable rates unreachable.
const RATE_INTERVAL_HZ: u64 = 100_000_000;

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`] to load. Taking the mechanism role
/// needs `CAP_CPUFREQ`, which the kernel checks at the bind trap, and serving
/// the clocks the node's clock `LinkDuty`.
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// Ask the firmware behind `channel` for `clock`'s rate of kind `which`.
fn rate_of<C: MailboxChannel + ?Sized>(
    channel: &C,
    clock: FirmwareClock,
    which: ClockRateQuery,
) -> Result<u64, DriverError> {
    query_clock_rate(&mut &*channel, clock, which)
        .map(u64::from)
        .map_err(MailboxError::as_driver_error)
}

/// Ask the firmware behind `channel` to run `clock` at `hz`, answering the
/// rate it applied. Zero is how the property interface spells "no such
/// clock", so it is refused rather than read as a stopped one.
fn set_rate<C: MailboxChannel + ?Sized>(
    channel: &C,
    clock: FirmwareClock,
    hz: u64,
) -> Result<u64, DriverError> {
    let hz = u32::try_from(hz).map_err(|_| DriverError::OutOfRange)?;
    match set_clock_rate(&mut &*channel, clock, hz).map_err(MailboxError::as_driver_error)? {
        0 => Err(DriverError::DeviceFault),
        applied => Ok(u64::from(applied)),
    }
}

/// The ARM core clock, as the frequency mechanism drives it.
pub struct ArmClock<'c, C: MailboxChannel + ?Sized> {
    channel: &'c C,
}

impl<'c, C: MailboxChannel + ?Sized> ArmClock<'c, C> {
    /// The ARM clock the firmware behind `channel` owns. There is no bring-up
    /// step: the clock runs whenever the board is powered, and a channel that
    /// cannot reach the firmware surfaces at the first query.
    pub const fn new(channel: &'c C) -> Self {
        Self { channel }
    }

    /// One of the clock's rates; zero, "no such clock", is a fault.
    fn query(&self, which: ClockRateQuery) -> Result<u64, DriverError> {
        match rate_of(self.channel, FirmwareClock::Arm, which)? {
            0 => Err(DriverError::DeviceFault),
            hz => Ok(hz),
        }
    }

    /// The operating range to declare to the kernel governor, both ends the
    /// firmware's own, so a board whose `config.txt` moves either is driven
    /// over the range it has.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when the firmware refuses a tag or answers
    /// a zero rate, [`DriverError::OutOfRange`] for a range no governor could
    /// pick from, and otherwise as [`MailboxError`] maps.
    pub fn limits(&self) -> Result<CpuFreqLimits, DriverError> {
        let min_hz = self.query(ClockRateQuery::Min)?;
        let max_hz = self.query(ClockRateQuery::Max)?;
        // A range narrower than one interval is driven at a single rate, so
        // the step is irrelevant and any legal value serves.
        let step_hz = if max_hz.saturating_sub(min_hz) < RATE_INTERVAL_HZ {
            max_hz.saturating_sub(min_hz).max(1)
        } else {
            RATE_INTERVAL_HZ
        };
        CpuFreqLimits::new(min_hz, max_hz, step_hz).map_err(|_| DriverError::OutOfRange)
    }

    /// Ask the firmware to run the clock at `rate_hz`, answering the rate it
    /// applied.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a target wider than the firmware's
    /// 32-bit rate field, [`DriverError::DeviceFault`] when the firmware
    /// refuses the tag or reports a zero rate, and otherwise as
    /// [`MailboxError`] maps.
    pub fn apply(&self, rate_hz: u64) -> Result<u64, DriverError> {
        set_rate(self.channel, FirmwareClock::Arm, rate_hz)
    }
}

/// One linked clock, as the firmware reported it.
#[derive(Copy, Clone)]
struct Linked {
    /// The rates the firmware accepts; `None` for a clock it does not know.
    range: Option<(u64, u64)>,
    /// The rate it ran at before the endpoint first changed it.
    before: Option<u64>,
}

/// The clocks served over `clock-v1` to the nodes linking to them
/// ([`LINKED_CLOCKS`]), each floored: it runs at the rate the firmware makes
/// for the highest floor its holders state, never below the least the
/// firmware accepts, and returns to the rate it ran at before once its last
/// holder leaves.
pub struct LinkedClocks<'c, C: MailboxChannel + ?Sized> {
    channel: &'c C,
    clocks: [Linked; LINKED_CLOCKS.len()],
}

impl<'c, C: MailboxChannel + ?Sized> LinkedClocks<'c, C> {
    /// The linked clocks behind `channel`, each with the range the firmware
    /// reports for it. One it reports no range for is not served, so a link
    /// naming it is refused.
    ///
    /// # Errors
    ///
    /// The channel's failure: a firmware that cannot be asked.
    pub fn new(channel: &'c C) -> Result<Self, DriverError> {
        let mut clocks = [Linked {
            range: None,
            before: None,
        }; LINKED_CLOCKS.len()];
        for (linked, clock) in clocks.iter_mut().zip(LINKED_CLOCKS) {
            let max_hz = rate_of(channel, clock, ClockRateQuery::Max)?;
            let min_hz = rate_of(channel, clock, ClockRateQuery::Min)?.max(clock.least_hz());
            if max_hz != 0 && min_hz <= max_hz {
                linked.range = Some((min_hz, max_hz));
            }
        }
        Ok(Self { channel, clocks })
    }

    fn range(&self, at: usize) -> Result<(u64, u64), DriverError> {
        self.clocks[at].range.ok_or(DriverError::Unsupported)
    }
}

impl<C: MailboxChannel + ?Sized> ClockHardware for LinkedClocks<'_, C> {
    /// The rate asked of the firmware.
    type Setting = u64;

    fn count(&self) -> usize {
        LINKED_CLOCKS.len()
    }

    fn served(&self, selector: &[u32]) -> Option<usize> {
        let &[id] = selector else {
            return None;
        };
        LINKED_CLOCKS
            .iter()
            .position(|clock| clock.as_u32() == id)
            .filter(|&at| self.clocks[at].range.is_some())
    }

    fn id(&self, at: usize) -> u32 {
        LINKED_CLOCKS[at].as_u32()
    }

    fn policy(&self, _at: usize) -> Policy {
        Policy::Floor
    }

    fn rate(&self, at: usize) -> Result<u64, DriverError> {
        rate_of(self.channel, LINKED_CLOCKS[at], ClockRateQuery::Current)
    }

    fn max_rate(&self, at: usize) -> Result<u64, DriverError> {
        self.range(at).map(|(_, max_hz)| max_hz)
    }

    fn nearest(&self, at: usize, hz: u64) -> Result<u64, DriverError> {
        let (min_hz, max_hz) = self.range(at)?;
        if hz > max_hz {
            return Err(DriverError::OutOfRange);
        }
        Ok(hz.max(min_hz))
    }

    fn run(&mut self, at: usize, hz: u64) -> Result<Applied, DriverError> {
        let clock = LINKED_CLOCKS[at];
        if self.clocks[at].before.is_none() {
            self.clocks[at].before = Some(rate_of(self.channel, clock, ClockRateQuery::Current)?);
        }
        Ok(Applied {
            halt: Halt::Clean,
            hz: set_rate(self.channel, clock, hz)?,
        })
    }

    fn stop(&mut self, at: usize) -> Result<Halt, DriverError> {
        let Some(before) = self.clocks[at].before else {
            return Ok(Halt::Clean);
        };
        let (min_hz, _) = self.range(at)?;
        // The property interface cannot stop a clock, and a zero rate is how
        // it answers "no such clock": one the firmware had not started is left
        // at the least it accepts, or where it runs when that least is zero.
        let rest_hz = before.max(min_hz);
        if rest_hz != 0 {
            set_rate(self.channel, LINKED_CLOCKS[at], rest_hz)?;
        }
        self.clocks[at].before = None;
        Ok(Halt::Clean)
    }
}

//! What the board says first: whether anything on the machine needs
//! attention.
//!
//! Read from the monitor's own verdicts — its pressure latches, the kernel's
//! band, each device's health, its recovery count — and never from a
//! threshold of the board's own, so the wall and the tray icon cannot
//! disagree about whether something is wrong.

use alloc::format;
use alloc::string::String;

use tairix_abi::switchboard_ipc::{MachineReport, MachineStorage};
use tairix_abi::sysinfo::{MemoryBand, VolumeHealth};
use tairix_theme::SignalRole;

/// One thing the board names as needing attention, in the order of how much
/// it matters: data at risk first, then what the machine is short of, then
/// what is merely unwell.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Concern {
    /// Storage devices the monitor reports failing.
    StorageFailing(u16),
    /// Memory held under pressure, in the band the kernel reports.
    MemoryPressured(Option<MemoryBand>),
    /// The processors held under pressure.
    CpuPressured,
    /// Storage devices the monitor reports degraded.
    StorageDegraded(u16),
    /// Tasks stopped or no longer answering.
    AwaitingRecovery(u16),
}

impl Concern {
    /// How the concern reads.
    fn text(self) -> String {
        match self {
            Self::StorageFailing(1) => String::from("A storage device is failing"),
            Self::StorageFailing(count) => format!("{count} storage devices are failing"),
            Self::MemoryPressured(Some(band)) => {
                format!("Memory under pressure ({})", band.name())
            }
            Self::MemoryPressured(None) => String::from("Memory under pressure"),
            Self::CpuPressured => String::from("Processors saturated"),
            Self::StorageDegraded(1) => String::from("A storage device is degraded"),
            Self::StorageDegraded(count) => format!("{count} storage devices are degraded"),
            Self::AwaitingRecovery(1) => String::from("A task needs recovery"),
            Self::AwaitingRecovery(count) => format!("{count} tasks need recovery"),
        }
    }
}

/// What the board's verdict says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// No reading has come yet.
    Waiting,
    /// No reading came in the time a monitor takes to start answering.
    Silent,
    /// The monitor that reads the machine is not running.
    Unmonitored,
    /// Readings stopped coming; the last came at the minute `at` spells.
    Stale {
        /// The minute the last reading came, as the clock spells it.
        at: String,
    },
    /// Nothing the monitor holds a verdict on needs attention.
    Calm,
    /// Something does: the worst concern, and how many others stand beside it.
    Attention {
        /// The concern that matters most.
        worst: Concern,
        /// How many more there are.
        others: usize,
    },
}

impl Verdict {
    /// The verdict `report` reaches.
    #[must_use]
    pub fn of(report: &MachineReport) -> Self {
        let mut concerns = concerns(report);
        concerns.sort_unstable();
        match concerns.split_first() {
            None => Self::Calm,
            Some((worst, rest)) => Self::Attention {
                worst: *worst,
                others: rest.len(),
            },
        }
    }

    /// The signal the verdict is drawn in, or `None` for one that is neither
    /// good nor bad news.
    #[must_use]
    pub fn tone(&self) -> Option<SignalRole> {
        match self {
            Self::Waiting => None,
            Self::Calm => Some(SignalRole::Success),
            Self::Attention {
                worst: Concern::StorageFailing(_),
                ..
            } => Some(SignalRole::for_volume_health(VolumeHealth::Failing)),
            Self::Silent | Self::Unmonitored | Self::Stale { .. } | Self::Attention { .. } => {
                Some(SignalRole::Warning)
            }
        }
    }

    /// How the verdict reads.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Waiting => String::from("Waiting for readings"),
            Self::Silent => String::from("The system monitor is not answering"),
            Self::Unmonitored => String::from("The system monitor is not running"),
            Self::Stale { at } => format!("Readings stopped at {at}"),
            Self::Calm => String::from("All readings normal"),
            Self::Attention { worst, others: 0 } => worst.text(),
            Self::Attention { worst, others } => format!("{} · {others} more", worst.text()),
        }
    }
}

/// Every concern `report` raises, in no particular order.
fn concerns(report: &MachineReport) -> alloc::vec::Vec<Concern> {
    let mut concerns = alloc::vec::Vec::new();
    let (mut failing, mut degraded) = (0u16, 0u16);
    for device in report.storage.iter().flat_map(MachineStorage::devices) {
        match device.availability.health() {
            VolumeHealth::Healthy => {}
            VolumeHealth::Degraded => degraded = degraded.saturating_add(1),
            VolumeHealth::Failing => failing = failing.saturating_add(1),
        }
    }
    if failing > 0 {
        concerns.push(Concern::StorageFailing(failing));
    }
    if degraded > 0 {
        concerns.push(Concern::StorageDegraded(degraded));
    }
    if report.memory.pressured {
        concerns.push(Concern::MemoryPressured(report.memory.band));
    }
    if report.cpu.pressured {
        concerns.push(Concern::CpuPressured);
    }
    if report.tasks.recovery() > 0 {
        concerns.push(Concern::AwaitingRecovery(report.tasks.recovery()));
    }
    concerns
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;

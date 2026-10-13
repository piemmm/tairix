//! How a driver process ends: the one vocabulary every driver states its
//! ending in, and the kernel's record on a node of how the driver that last
//! held it ended.

use crate::process::load_failure_reason;

/// Why a driver process ends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DriverExit {
    /// The node offers nothing the driver serves; it is left unbound.
    Declined,
    /// The device left while it was served.
    Removed,
    /// The node's grants could not be read.
    NoHost,
    /// The node lacks a resource, duty or link the driver needs.
    NoResources,
    /// The device would not come up.
    BringupFailed,
    /// The service the driver offers could not be stood up.
    NoService,
    /// The loop the driver serves from failed.
    ServeFailed,
    /// The device failed while it was served.
    DeviceFault,
}

impl DriverExit {
    /// Every kind.
    pub const ALL: &'static [Self] = &[
        Self::Declined,
        Self::Removed,
        Self::NoHost,
        Self::NoResources,
        Self::BringupFailed,
        Self::NoService,
        Self::ServeFailed,
        Self::DeviceFault,
    ];

    /// The process exit status: zero for an ending that is no failure.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Declined | Self::Removed => 0,
            Self::NoHost => 80,
            Self::NoResources => 81,
            Self::BringupFailed => 82,
            Self::NoService => 83,
            Self::ServeFailed => 84,
            Self::DeviceFault => 85,
        }
    }

    /// The word a record names the ending by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Declined => "declined",
            Self::Removed => "removed",
            Self::NoHost => "no_host",
            Self::NoResources => "no_resources",
            Self::BringupFailed => "bringup_failed",
            Self::NoService => "no_service",
            Self::ServeFailed => "serve_failed",
            Self::DeviceFault => "device_fault",
        }
    }

    /// The reason the ending states.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Declined => "driver: the node offers nothing this driver serves; left unbound",
            Self::Removed => "driver: the device was removed",
            Self::NoHost => "driver: the node's grants could not be read",
            Self::NoResources => "driver: the node lacks what this driver needs",
            Self::BringupFailed => "driver: the device would not come up",
            Self::NoService => "driver: the driver's service could not be stood up",
            Self::ServeFailed => "driver: the loop serving the device failed",
            Self::DeviceFault => "driver: the device failed while served",
        }
    }

    /// Whether a fresh instance may succeed where this ending's did: one
    /// that failed while it served, or could not stand its service up. A
    /// refusal at bring-up meets the same node and device again.
    #[must_use]
    pub const fn cured_by_restart(self) -> bool {
        matches!(
            self,
            Self::NoService | Self::ServeFailed | Self::DeviceFault
        )
    }
}

/// How the driver that last held a node ended, as the kernel read its exit
/// status.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum DriverEnding {
    /// No driver has ended since one was last admitted.
    #[default]
    None = 0,
    /// It was unloaded.
    Unloaded = 1,
    /// It exited cleanly: it declined its node, or its device went.
    Clean = 2,
    /// It ended for a failure a fresh instance would meet again: its node
    /// lacks what it needs, its device would not come up, or its image would
    /// not load.
    Refused = 3,
    /// It ended for a failure a fresh instance may not meet
    /// ([`DriverExit::cured_by_restart`]).
    Failed = 4,
    /// It was killed, or exited with a status outside the vocabulary.
    Crashed = 5,
}

impl DriverEnding {
    /// Every ending.
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Unloaded,
        Self::Clean,
        Self::Refused,
        Self::Failed,
        Self::Crashed,
    ];

    /// The ending an exit `status` records, an unload owing no status.
    #[must_use]
    pub const fn of_status(status: Option<i32>) -> Self {
        let Some(status) = status else {
            return Self::Unloaded;
        };
        if status == 0 {
            return Self::Clean;
        }
        if load_failure_reason(status).is_some() {
            return Self::Refused;
        }
        let mut at = 0;
        while at < DriverExit::ALL.len() {
            let exit = DriverExit::ALL[at];
            if exit.code() == status {
                return if exit.cured_by_restart() {
                    Self::Failed
                } else {
                    Self::Refused
                };
            }
            at += 1;
        }
        Self::Crashed
    }

    /// Whether a fresh instance of the driver may succeed.
    #[must_use]
    pub const fn merits_restart(self) -> bool {
        matches!(self, Self::Failed | Self::Crashed)
    }

    /// The word a record names the ending by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unloaded => "unloaded",
            Self::Clean => "clean",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
        }
    }

    /// The wire byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The ending a wire byte names, if it names one.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Unloaded),
            2 => Some(Self::Clean),
            3 => Some(Self::Refused),
            4 => Some(Self::Failed),
            5 => Some(Self::Crashed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{load_failure_status, FAULT_EXIT_STATUS};
    use crate::Errno;

    #[test]
    fn a_failure_is_never_a_clean_status_and_no_two_failures_share_one() {
        let mut failures = DriverExit::ALL
            .iter()
            .map(|exit| exit.code())
            .filter(|&code| code != 0);
        for expected in 80..=85 {
            assert_eq!(failures.next(), Some(expected));
        }
        assert_eq!(failures.next(), None);
        assert_eq!(DriverExit::Declined.code(), 0);
        assert_eq!(DriverExit::Removed.code(), 0);
    }

    #[test]
    fn each_status_records_the_ending_a_restart_can_or_cannot_cure() {
        assert_eq!(DriverEnding::of_status(None), DriverEnding::Unloaded);
        assert_eq!(DriverEnding::of_status(Some(0)), DriverEnding::Clean);
        for exit in DriverExit::ALL.iter().filter(|exit| exit.code() != 0) {
            let ending = DriverEnding::of_status(Some(exit.code()));
            assert_eq!(ending.merits_restart(), exit.cured_by_restart(), "{exit:?}");
        }
        for errno in [Errno::NotFound, Errno::SignatureInvalid, Errno::OutOfMemory] {
            assert_eq!(
                DriverEnding::of_status(Some(load_failure_status(errno))),
                DriverEnding::Refused,
                "an image that would not load is not retried"
            );
        }
        assert_eq!(
            DriverEnding::of_status(Some(FAULT_EXIT_STATUS)),
            DriverEnding::Crashed
        );
        assert_eq!(DriverEnding::of_status(Some(-1)), DriverEnding::Crashed);
    }

    #[test]
    fn an_ending_round_trips_its_byte_and_no_other_byte_names_one() {
        for ending in DriverEnding::ALL {
            assert_eq!(DriverEnding::from_u8(ending.as_u8()), Some(ending));
        }
        let named = DriverEnding::ALL.len();
        for byte in u8::try_from(named).unwrap_or(u8::MAX)..=u8::MAX {
            assert_eq!(DriverEnding::from_u8(byte), None);
        }
    }
}

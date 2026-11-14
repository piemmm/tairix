//! How a machine stands, as the library shows it and as what may be done to
//! it follows.

/// Where a machine stands, as the library shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Turned off.
    Off,
    /// Running in a window of its own.
    Running,
    /// Being kept as it suspends.
    Saving,
    /// Suspended: started, it resumes where it stopped.
    Suspended,
}

impl Standing {
    /// How the library's status band names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Running => "running",
            Self::Saving => "saving",
            Self::Suspended => "suspended",
        }
    }

    /// Why its hardware cannot be changed, where it cannot: only a machine
    /// turned off can be.
    #[must_use]
    pub const fn locked(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Running => Some("The machine is running: shut it down to change its hardware."),
            Self::Saving => Some("The machine is being kept as it suspends."),
            Self::Suspended => {
                Some("The machine is suspended: start it and shut it down to change its hardware.")
            }
        }
    }

    /// Whether it may be copied or deleted: not under its window, nor while
    /// it is being kept.
    #[must_use]
    pub const fn at_rest(self) -> bool {
        matches!(self, Self::Off | Self::Suspended)
    }

    /// Whether it may be started or its window raised: not while it is being
    /// kept, which a start would race.
    #[must_use]
    pub const fn startable(self) -> bool {
        !matches!(self, Self::Saving)
    }
}

//! What backing a not-resident user page came to: the one answer every
//! demand-paging resolver gives, which a kernel copy turns into its errno and
//! a hardware fault into the fault's end.

use tairix_abi::Errno;

/// Why a page of a process's address space could not be backed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unbacked {
    /// No mapping the process holds reaches the page for the access: the
    /// process's own fault.
    Invalid,
    /// The page is the process's, but the machine had no memory to back it.
    NoMemory,
    /// The storage behind the page could not be read.
    Unreadable,
    /// The page's only copy failed its integrity check: its contents are gone.
    Lost,
    /// A death is owed to the faulting thread, so the backing was abandoned.
    Interrupted,
}

impl Unbacked {
    /// What a kernel copy that met the page answers, so a caller can tell a
    /// buffer of its own making from a machine short of memory or a failing
    /// disk.
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Invalid => Errno::BadAddress,
            Self::NoMemory => Errno::OutOfMemory,
            Self::Unreadable => Errno::DeviceFault,
            Self::Lost => Errno::MediumError,
            Self::Interrupted => Errno::Interrupted,
        }
    }

    /// The cause a fault-kill record names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NoMemory => "no_memory",
            Self::Unreadable => "unreadable",
            Self::Lost => "lost",
            Self::Interrupted => "interrupted",
        }
    }
}

/// What one resolver made of a not-resident page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backing {
    /// The page is resident: retry the access.
    Backed,
    /// No mapping of this resolver's kind covers the page.
    Elsewhere,
    /// A mapping of this resolver's kind covers the page and could not back
    /// it.
    Refused(Unbacked),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each cause answers a copy with its own errno, so no two of them read
    /// alike, and only the process's own mistake reads as a bad address.
    #[test]
    fn every_cause_answers_a_copy_distinctly() {
        let causes = [
            Unbacked::Invalid,
            Unbacked::NoMemory,
            Unbacked::Unreadable,
            Unbacked::Lost,
            Unbacked::Interrupted,
        ];
        for (i, a) in causes.iter().enumerate() {
            for b in &causes[i + 1..] {
                assert_ne!(a.errno(), b.errno(), "{a:?} and {b:?}");
                assert_ne!(a.as_str(), b.as_str(), "{a:?} and {b:?}");
            }
        }
        assert_eq!(Unbacked::Invalid.errno(), Errno::BadAddress);
        assert_eq!(Unbacked::NoMemory.errno(), Errno::OutOfMemory);
    }
}

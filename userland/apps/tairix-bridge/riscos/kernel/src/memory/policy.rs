//! The memory policy (`plans/RISCOS-EMULATOR.md` §3): how much RAM a
//! session is given, which is all the RAM its guest is told it has.

use tairix_riscos_arm::mmu::PAGE_SIZE;

/// The share of the machine's usable RAM a session may take: a quarter, so
/// the shared session and the isolated ones beside it leave the rest of the
/// desktop its memory. A session uses only what its guest asks for, so the
/// share bounds a guest that grabs everything, not an ordinary one.
const SHARE: u64 = 4;

/// The most pages a guest can be shown: its physical addresses are 32 bits.
const MOST_PAGES: u64 = 1 << 20;

/// A session's ceiling: the RAM its guest has, in pages.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Ceiling {
    pages: u32,
}

impl Ceiling {
    /// The ceiling of a session on a machine with `usable` bytes of RAM.
    #[must_use]
    pub fn derive(usable: u64) -> Self {
        Self::of_bytes(usable / SHARE)
    }

    /// This ceiling, lowered to `bytes` if that is less: the user's limit on
    /// the worker, or the ceiling the settings choose. Nothing raises it.
    #[must_use]
    pub fn lowered(self, bytes: u64) -> Self {
        self.min(Self::of_bytes(bytes))
    }

    fn of_bytes(bytes: u64) -> Self {
        let pages = (bytes / u64::from(PAGE_SIZE)).min(MOST_PAGES);
        Self {
            pages: u32::try_from(pages).unwrap_or(u32::MAX),
        }
    }

    /// The pages it allows.
    #[must_use]
    pub const fn pages(self) -> u32 {
        self.pages
    }

    /// The bytes it allows: the guest's RAM size.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.pages as u64 * PAGE_SIZE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn a_session_takes_a_quarter_and_is_only_ever_lowered() {
        let ceiling = Ceiling::derive(8 * 1024 * MIB);
        assert_eq!(ceiling.bytes(), 2 * 1024 * MIB);
        assert_eq!(ceiling.lowered(512 * MIB).bytes(), 512 * MIB);
        assert_eq!(ceiling.lowered(64 * 1024 * MIB), ceiling, "never raised");
        assert_eq!(
            Ceiling::derive(64 * MIB).lowered(10 * MIB + 100).pages(),
            2560,
            "whole pages"
        );
    }

    #[test]
    fn a_guest_is_never_shown_more_than_32_bits_of_physical_address() {
        assert_eq!(Ceiling::derive(1 << 40).bytes(), 4 * 1024 * MIB);
        assert_eq!(Ceiling::derive(0).pages(), 0);
    }
}

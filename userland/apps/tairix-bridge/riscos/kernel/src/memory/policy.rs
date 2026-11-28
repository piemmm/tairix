//! The memory policy (`plans/RISCOS-EMULATOR.md` §3): how much of the
//! machine a session may take, and how much RAM its guest is told it has.

use tairix_riscos_arm::cpu::MEMO_PAGE_BYTES;
use tairix_riscos_arm::mmu::PAGE_SIZE;

use crate::os::PROFILES;

/// The share of the machine's usable RAM a session may take: a quarter, so
/// the shared session and the isolated ones beside it leave the rest of the
/// desktop its memory. A session uses only what its guest asks for, so the
/// share bounds a guest that grabs everything, not an ordinary one.
const SHARE: u64 = 4;

/// The most pages a guest can be shown: its physical addresses are 32 bits.
pub const MOST_PAGES: u64 = 1 << 20;

/// The most a session may take on a machine with `usable` bytes of RAM: its
/// guest and everything its worker keeps beside it.
#[must_use]
pub const fn session_bytes(usable: u64) -> u64 {
    usable / SHARE
}

/// The most a session's decoded instructions hold when each of its
/// processors keeps `memo_pages` pages.
#[must_use]
pub const fn memo_bytes(memo_pages: u32) -> u64 {
    PROFILES as u64 * memo_pages as u64 * MEMO_PAGE_BYTES as u64
}

/// A session's ceiling: the RAM its guest has, in pages.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Ceiling {
    pages: u32,
}

impl Ceiling {
    /// The ceiling of a guest given all its session may take on a machine
    /// with `usable` bytes of RAM.
    #[must_use]
    pub fn derive(usable: u64) -> Self {
        Self::of_bytes(session_bytes(usable))
    }

    /// This ceiling, lowered to `bytes` if that is less: the user's limit on
    /// the worker, or the ceiling the settings choose. Nothing raises it.
    #[must_use]
    pub fn lowered(self, bytes: u64) -> Self {
        self.min(Self::of_bytes(bytes))
    }

    /// A ceiling of `pages` pages: none past what a guest can be shown.
    #[must_use]
    pub fn of_pages(pages: u32) -> Option<Self> {
        (u64::from(pages) <= MOST_PAGES).then_some(Self { pages })
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

/// The budget each of a session's caches is held to in a session of
/// `ceiling` bytes: a share of its RAM, as every reclaimable cache's is of
/// what backs it.
#[must_use]
pub fn cache_budget(ceiling: u64) -> tairix_reclaim::CacheBudget {
    tairix_reclaim::CacheBudget::from_backing(usize::try_from(ceiling).unwrap_or(usize::MAX))
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
        assert_eq!(session_bytes(1 << 40), 1 << 38, "the session is not");
    }

    #[test]
    fn decoded_pages_cost_a_page_of_each_instruction_set_on_each_processor() {
        assert_eq!(memo_bytes(0), 0);
        assert_eq!(memo_bytes(3), 3 * PROFILES as u64 * MEMO_PAGE_BYTES as u64);
        assert!(
            MEMO_PAGE_BYTES > PAGE_SIZE as usize,
            "a decoding outweighs its word"
        );
    }
}

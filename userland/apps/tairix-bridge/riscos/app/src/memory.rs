//! How one session's memory is divided (`plans/RISCOS-EMULATOR.md` §15.3):
//! the ceiling its worker is held to, the guest's RAM within it, the decoded
//! pages each processor keeps, and the slot each launch is given.
//!
//! The ceiling is all a session may take of the machine, lowered by the
//! user's address-space limit and the setting. The guest is given the most
//! the ceiling holds beside what the worker keeps for it: the sandbox's
//! baseline, the damage ring and the link's frames, the frames' bookkeeping,
//! the decoded pages, and the caches HostFS and the Font Manager hold.

use tairix_riscos_arm::mmu::{BOOKKEEPING_SHARE, PAGE_SIZE};
use tairix_riscos_kernel::memory::layout::{APPLICATION, SHARED};
use tairix_riscos_kernel::memory::policy::{
    cache_budget, memo_bytes, session_bytes, Ceiling, MOST_PAGES,
};
use tairix_riscos_session::protocol::{FRAME_MOST, MEMO_MOST};
use tairix_riscos_session::ring::RING_BYTES;
use tairix_sandbox::memory::WORKER_BASELINE_BYTES;

/// What the worker keeps whatever its guest: the sandbox's baseline, the
/// damage ring, and its link's frames — the one coming in, the one going
/// out, a relay's reply and the next encoded.
const FIXED_BYTES: u64 = WORKER_BASELINE_BYTES + RING_BYTES as u64 + 4 * FRAME_MOST as u64;

/// The least slot a launch is given: a RiscPC's default `WimpSlot`.
const SLOT_LEAST: u32 = 640 << 10;

/// The share of the guest's RAM each launch's slot is.
const SLOT_SHARE: u64 = 8;

/// One session's division of its memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    /// The most the worker may map.
    pub worker_bytes: u64,
    /// The guest's RAM.
    pub guest: Ceiling,
    /// The decoded pages each processor keeps.
    pub memo_pages: u32,
    /// The slot every launch is given: whole pages, within a 26-bit slot.
    pub slot_bytes: u32,
}

impl Plan {
    /// The division for a machine of `usable` bytes, the worker held to the
    /// user's address-space `limit` and the `setting`, each where known:
    /// none where none is, or the guest could not hold one slot.
    #[must_use]
    pub fn of(usable: Option<u64>, limit: Option<u64>, setting: Option<u64>) -> Option<Self> {
        let worker_bytes = [usable.map(session_bytes), limit, setting]
            .into_iter()
            .flatten()
            .min()?;
        let guest = largest_guest(worker_bytes)?;
        let slot_bytes = slot_bytes(guest);
        (guest.bytes() >= u64::from(slot_bytes)).then_some(Self {
            worker_bytes,
            guest,
            memo_pages: memo_pages(guest),
            slot_bytes,
        })
    }
}

/// The largest guest whose RAM and what the worker keeps for it fit in
/// `worker_bytes`, of no pages where nothing does. What is kept grows with
/// the guest, so the pages are searched for rather than solved.
fn largest_guest(worker_bytes: u64) -> Option<Ceiling> {
    let fits = |pages: u32| {
        Ceiling::of_pages(pages)
            .is_some_and(|guest| guest.bytes().saturating_add(kept(guest)) <= worker_bytes)
    };
    let (mut fitting, mut beyond) = (0, u32::try_from(MOST_PAGES + 1).ok()?);
    while beyond - fitting > 1 {
        let middle = fitting + (beyond - fitting) / 2;
        if fits(middle) {
            fitting = middle;
        } else {
            beyond = middle;
        }
    }
    Ceiling::of_pages(fitting)
}

/// What the worker keeps beside a guest of `guest`.
fn kept(guest: Ceiling) -> u64 {
    let bytes = guest.bytes();
    FIXED_BYTES
        .saturating_add(bytes / u64::from(BOOKKEEPING_SHARE))
        .saturating_add(memo_bytes(memo_pages(guest)))
        .saturating_add(tairix_riscos_filing::hostfs::host_bytes(bytes))
        .saturating_add(tairix_riscos_font::host_bytes(bytes))
}

/// The decoded pages each processor of a guest of `guest` keeps: as many as
/// one cache's share of its RAM holds, one at least.
fn memo_pages(guest: Ceiling) -> u32 {
    let budget = cache_budget(guest.bytes()).hard() as u64;
    let pages = budget / memo_bytes(1);
    u32::try_from(pages.clamp(1, u64::from(MEMO_MOST))).unwrap_or(MEMO_MOST)
}

/// The slot each launch in a guest of `guest` is given: its share of the
/// RAM, no less than the least and no more than a 26-bit slot, in whole
/// pages.
fn slot_bytes(guest: Ceiling) -> u32 {
    let most = SHARED.top_26bit - APPLICATION;
    let share = u32::try_from(guest.bytes() / SLOT_SHARE).unwrap_or(most);
    share.clamp(SLOT_LEAST, most) / PAGE_SIZE * PAGE_SIZE
}

#[cfg(test)]
mod tests;

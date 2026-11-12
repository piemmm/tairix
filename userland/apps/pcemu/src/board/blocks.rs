//! The board as translated code runs in it: the page map over its memory,
//! kept as the memory changes, and the part of its bus a running block
//! reaches — DRAM refresh — borrowed apart from the memory the block runs
//! over.

use super::memory::{self, Memory, PAGE_SIZE};
use super::{refresh_deadline, refresh_until, Board, Planar, DMA_CYCLES, KIBS, MIXED};
use crate::chips::dma::Dma;
use crate::chips::pit::Pit;
use crate::cpu::jit::cache::Watch;
use crate::cpu::jit::dispatch::{Env, Host, Waits};
use crate::cpu::jit::page::{self, PageMap};
use crate::time::{Clock, Hertz};

/// The board's refresh, as [`Bus::take_wait_states`] charges it, over the
/// parts of the board it reaches.
///
/// [`Bus::take_wait_states`]: crate::cpu::Bus::take_wait_states
pub(super) struct Refresh<'a> {
    pub(super) clock: &'a Clock,
    pub(super) pit: &'a Pit,
    pub(super) dma: &'a mut Dma,
    /// The timer tick through which refresh is accounted.
    pub(super) accounted: &'a mut u64,
    /// The CPU cycle by which the next refresh takes the bus.
    pub(super) deadline: &'a mut u64,
    pub(super) stolen: &'a mut u64,
    pub(super) held: &'a mut u64,
}

impl Refresh<'_> {
    /// The waits taken by cycle `at`: refresh charged where it fell due.
    pub(super) fn take(&mut self, at: u64) -> u32 {
        if at >= *self.deadline {
            let tick = Hertz::PIT.ticks_at(self.clock.time_of(at));
            *self.stolen += refresh_until(self.pit, self.dma, self.accounted, tick) * DMA_CYCLES;
            *self.deadline = refresh_deadline(self.clock, self.pit, self.dma, *self.accounted);
        }
        *self.held = 0;
        u32::try_from(core::mem::take(self.stolen)).unwrap_or(u32::MAX)
    }
}

impl Waits for Refresh<'_> {
    fn take(&mut self, at: u64) -> u32 {
        Refresh::take(self, at)
    }

    fn next_refresh(&self) -> u64 {
        *self.deadline
    }
}

impl Board {
    /// The board's refresh, apart from its memory.
    pub(super) fn refresh(&mut self) -> Refresh<'_> {
        let Self {
            clock,
            planar,
            stolen,
            held,
            next_refresh,
            ..
        } = self;
        Refresh {
            clock,
            pit: &planar.pit,
            dma: &mut planar.dma,
            accounted: &mut planar.refresh_tick,
            deadline: next_refresh,
            stolen,
            held,
        }
    }

    /// Makes again the page-map entries of every page whose memory, wait
    /// states or watch changed since they were last made.
    fn update_page_map(&mut self) {
        let mut dirty = self.planar.memory.take_dirty();
        if core::mem::take(&mut self.waits_changed) {
            dirty = [u64::MAX; memory::PAGES / 64];
        }
        let Self {
            planar,
            waits,
            pages,
            ..
        } = self;
        for (word, bits) in (0..).zip(dirty) {
            let mut bits = bits;
            while bits != 0 {
                let page = word * 64 + bits.trailing_zeros();
                bits &= bits - 1;
                let base = page * PAGE_SIZE;
                for addr in (base..base + PAGE_SIZE).step_by(page::PAGE_BYTES as usize) {
                    entry(pages, &planar.memory, waits, addr);
                }
            }
        }
    }
}

/// Makes the page-map entry of the page at `addr`: where its bytes lie,
/// their wait states, whether translated code may store there.
fn entry(pages: &mut PageMap, memory: &Memory, waits: &[u8; KIBS], addr: u32) {
    let at = (addr >> page::PAGE_BITS) as usize;
    let wait = waits[(addr >> 10) as usize % KIBS];
    match memory.backing(addr).filter(|_| wait != MIXED) {
        Some(offset) => {
            let writable = memory.writable(addr) && !memory.is_watched(offset / page::PAGE_BYTES);
            pages.set(at, memory.host_address(offset), wait, writable);
        }
        None => pages.set(at, 0, 0, false),
    }
}

impl Host for Board {
    fn watched(&mut self) -> &mut dyn Watch {
        &mut self.planar.memory
    }

    fn with_blocks(&mut self, run: &mut dyn FnMut(&PageMap, &mut Env<'_>)) {
        self.update_page_map();
        let Self {
            clock,
            planar,
            stolen,
            held,
            next_refresh,
            pages,
            ..
        } = self;
        let Planar {
            memory,
            pit,
            dma,
            refresh_tick,
            ..
        } = planar;
        let base = memory.host_address(0);
        let mut refresh = Refresh {
            clock,
            pit,
            dma,
            accounted: refresh_tick,
            deadline: next_refresh,
            stolen,
            held,
        };
        let mut env = Env {
            store: memory.store_for_blocks(),
            base,
            waits: &mut refresh,
        };
        run(pages, &mut env);
    }
}

#[cfg(test)]
#[path = "blocks_tests.rs"]
mod tests;

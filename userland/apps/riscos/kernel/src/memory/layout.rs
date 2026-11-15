//! Where a session keeps each part of its address space
//! (`plans/RISCOS-EMULATOR.md` §3).
//!
//! A session is made with one of two layouts. The shared one admits 26-bit
//! code, so everything such code can be handed lies below 64 MiB: the RMA,
//! the system heap, the stacks and the host pages at the top of that window,
//! the areas 26-bit callers make below them, and application space under
//! those. The other is RISC OS 5's own, for a session whose program is
//! 32-bit: application space to 512 MiB and everything shared above it, and
//! 26-bit code refused.

use core::ops::Range;

/// The bottom of application space, where every task's slot starts.
pub const APPLICATION: u32 = 0x8000;
/// The top of the 26-bit address space.
pub const WINDOW_TOP: u32 = 0x0400_0000;
/// Zero page's size: the kernel workspace software reads, the processor
/// vectors at its start, and the VDU driver's workspace from &1000.
pub const ZERO_PAGE_SIZE: u32 = 0x4000;
/// Where the 32-bit profile has its vectors, and zero page with them.
pub const HIGH_ZERO_PAGE: u32 = 0xFFFF_0000;
/// ScratchSpace, the workspace modules borrow while a call lasts.
pub const SCRATCH: Range<u32> = 0x4000..0x8000;
/// The 32-bit profile's page at zero, which reads as RISC OS 5's
/// compatibility page does and is never written.
pub const COMPATIBILITY: Range<u32> = 0..0x1000;

/// Each processor mode's stack: where it lies, its top being where it
/// starts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Stacks {
    /// Supervisor mode's.
    pub supervisor: Range<u32>,
    /// IRQ mode's.
    pub irq: Range<u32>,
    /// Abort mode's.
    pub abort: Range<u32>,
    /// Undefined mode's.
    pub undefined: Range<u32>,
}

impl Stacks {
    /// Every stack.
    #[must_use]
    pub const fn all(&self) -> [&Range<u32>; 4] {
        [&self.supervisor, &self.irq, &self.abort, &self.undefined]
    }
}

/// One of the two layouts a session is made with.
#[derive(Debug)]
pub struct Layout {
    /// Whether a 26-bit program may run.
    pub admits_26bit: bool,
    /// The highest a 26-bit slot's top may reach; nothing in a layout that
    /// admits no 26-bit code.
    pub top_26bit: u32,
    /// The highest a 32-bit slot's top may reach.
    pub top_32bit: u32,
    /// The RMA, growing up from the start.
    pub rma: Range<u32>,
    /// The system heap, growing up from the start.
    pub system_heap: Range<u32>,
    /// The stacks, each with an unmapped page below it to catch an overflow.
    pub stacks: Stacks,
    /// The host pages.
    pub host: Range<u32>,
    /// Where the areas 26-bit callers make are placed, downward from the top:
    /// the window between 26-bit application space and the shared parts. A
    /// 32-bit slot grows into it only while no 26-bit code is resident.
    pub window: Option<Range<u32>>,
    /// Where every other dynamic area is placed, upward from the start or
    /// the application-space limit, around the kernel's parts within it.
    pub areas: Range<u32>,
}

/// The shared session's layout, 26-bit code welcome.
///
/// Application space reaches 28 MiB for a 26-bit task, as on the RiscPC. The
/// window's other 36 MiB is shared out: 14 MiB for 26-bit callers' areas, 16
/// MiB for the RMA, 4 MiB for the system heap, a mebibyte of stacks and one
/// of host pages.
pub static SHARED: Layout = Layout {
    admits_26bit: true,
    top_26bit: 0x01C0_0000,
    top_32bit: 0x02A0_0000,
    rma: 0x02A0_0000..0x03A0_0000,
    system_heap: 0x03A0_0000..0x03E0_0000,
    stacks: Stacks {
        irq: 0x03E0_E000..0x03E1_0000,
        supervisor: 0x03E1_8000..0x03E2_0000,
        abort: 0x03E2_E000..0x03E3_0000,
        undefined: 0x03E3_E000..0x03E4_0000,
    },
    host: 0x03F0_0000..WINDOW_TOP,
    window: Some(0x01C0_0000..0x02A0_0000),
    areas: WINDOW_TOP..0xFA00_0000,
};

/// RISC OS 5's own layout, for a session whose program declares 32-bit:
/// its addresses, so software that peeks where RISC OS 5 keeps things finds
/// them there.
pub static RISC_OS_5: Layout = Layout {
    admits_26bit: false,
    top_26bit: 0,
    top_32bit: 0x2000_0000,
    rma: 0x2000_0000..0x3000_0000,
    system_heap: 0x3000_0000..0x3200_0000,
    stacks: Stacks {
        irq: 0xFA10_0000..0xFA10_2000,
        supervisor: 0xFA20_0000..0xFA20_8000,
        abort: 0xFA30_0000..0xFA30_2000,
        undefined: 0xFA40_0000..0xFA40_2000,
    },
    host: 0xFC00_0000..0xFD00_0000,
    window: None,
    areas: WINDOW_TOP..0xFA00_0000,
};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const PAGE: u32 = 0x1000;

    fn disjoint(parts: &[Range<u32>]) -> bool {
        parts.iter().enumerate().all(|(i, a)| {
            parts[i + 1..]
                .iter()
                .all(|b| a.end <= b.start || b.end <= a.start)
        })
    }

    #[test]
    fn every_part_of_each_layout_is_whole_pages_and_apart() {
        for layout in [&SHARED, &RISC_OS_5] {
            let [s, i, a, u] = layout.stacks.all();
            let mut parts = vec![
                APPLICATION..layout.top_32bit.max(layout.top_26bit),
                layout.rma.clone(),
                layout.system_heap.clone(),
                // A stack's guard page below it.
                s.start - PAGE..s.end,
                i.start - PAGE..i.end,
                a.start - PAGE..a.end,
                u.start - PAGE..u.end,
                layout.host.clone(),
                HIGH_ZERO_PAGE..HIGH_ZERO_PAGE + ZERO_PAGE_SIZE,
            ];
            parts.extend(layout.window.clone());
            if let Some(window) = &layout.window {
                // The window is where 32-bit slots may grow: apart from all
                // but application space, which it continues.
                assert_eq!(window.start, layout.top_26bit);
                assert_eq!(window.end, layout.top_32bit);
                parts[0] = APPLICATION..window.start;
            }
            for part in &parts {
                assert!(part.start % PAGE == 0 && part.end % PAGE == 0, "{part:X?}");
                assert!(part.start < part.end, "{part:X?}");
            }
            assert!(disjoint(&parts), "{layout:X?}");
            // Areas are placed around the RMA and system heap, never over
            // the rest.
            let [s, i, a, u] = layout.stacks.all();
            for part in [s, i, a, u, &layout.host] {
                assert!(
                    part.end <= layout.areas.start || layout.areas.end <= part.start,
                    "{part:X?}"
                );
            }
        }
    }

    #[test]
    fn what_26bit_code_is_handed_lies_below_64_mib() {
        let shared = &SHARED;
        for part in [
            &shared.rma,
            &shared.system_heap,
            &shared.host,
            &shared.stacks.supervisor,
            &shared.stacks.irq,
            &shared.stacks.abort,
            &shared.stacks.undefined,
        ] {
            assert!(part.end <= WINDOW_TOP, "{part:X?}");
        }
        assert_eq!(
            shared.top_26bit - APPLICATION,
            28 * 1024 * 1024 - APPLICATION
        );
        assert!(!RISC_OS_5.admits_26bit);
        assert_eq!(RISC_OS_5.top_32bit, 512 * 1024 * 1024);
    }
}

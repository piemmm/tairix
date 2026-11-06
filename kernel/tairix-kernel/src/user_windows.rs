//! Discovered-RAM sizing policy for a task's *dynamic* user virtual
//! windows: the non-`FIXED` anonymous-heap window, the heap region above it
//! that the runtime's heap is told of, and the demand-paged file-mapping
//! window above that.
//!
//! The windows' *base* anchor and the other user-space layout offsets live
//! in [`crate::spawn_layout`]; their *sizes* are a function of the machine,
//! not hand-wired constants, and live here so the pure arithmetic is unit
//! tested on the CI host without dragging the freestanding-only layout
//! constants into the host build. Each port computes the split at spawn
//! time from the frame allocator's discovered total and its own user-VA
//! ceiling (`USER_VA_TOP`), then hands both windows to
//! [`tairix_kernel_mem::LiveSpace`].
//!
//! The split reflects what bounds each window:
//!
//! * The **anonymous heap** window and the **heap region** can never
//!   usefully exceed physical RAM — every anonymous page must be backable by
//!   a frame — so each spans `ram_frames`, floored at
//!   [`ANON_WINDOW_MIN_PAGES`] and capped at a quarter of the address space
//!   above the anchor so the file window keeps at least half of it. The heap
//!   region is in no window: a `FIXED` map is refused over a window's free
//!   space, so the runtime grows its heap there.
//! * A **file mapping** is bounded by *address space alone*: its pages are
//!   backed one at a time by the fault path and reclaimed under pressure,
//!   so a mapping legitimately exceeds RAM by orders of magnitude (a
//!   multi-terabyte file on a small machine). The file window therefore
//!   takes all remaining address space above the heap window.
//!
//! Address space costs no RAM until backed, and both windows fail closed
//! (deterministic OOM) at their reservation when exhausted.

use tairix_abi::HeapRegion;
use tairix_kernel_mem::PAGE_SIZE;

/// Floor for the anonymous-heap window, in pages (16 MiB).
///
/// A machine with very little RAM still gets a usable heap window: the
/// window costs no RAM until the frame allocator backs a mapping (which
/// fails closed as a deterministic OOM), so a floor above tiny RAM is free
/// and over-commits nothing. Clamped down only if the address space above
/// the window base genuinely cannot hold even this (a degenerate
/// misconfiguration).
pub const ANON_WINDOW_MIN_PAGES: usize = 0x1000;

/// The dynamic-window split for one task: how many pages the anonymous
/// heap window spans, the heap region above it, and where the file-mapping
/// window above that begins and ends. Produced by [`user_windows`]; consumed
/// by every port's spawn path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserWindows {
    /// Pages of address space the non-`FIXED` anonymous-heap window spans,
    /// starting at the anchor the port passed in.
    pub anon_pages: usize,
    /// The address space set aside for the process's heap, directly above the
    /// anonymous-heap window and as large.
    pub heap: HeapRegion,
    /// Base of the file-mapping window: the first page above the heap
    /// region's top.
    pub file_base: u64,
    /// Pages of address space the file-mapping window spans — everything
    /// remaining up to the port's user-VA ceiling. Zero on a degenerate
    /// configuration with no room (the live space then refuses file
    /// mappings fail-closed rather than refusing to spawn).
    pub file_pages: usize,
}

/// Split the user address space above `anon_window_base` between the
/// anonymous-heap window, the heap region and the file-mapping window for a
/// machine with `ram_frames` total frames and a per-architecture
/// first-non-addressable user virtual address `user_va_top`.
///
/// Every capacity is **derived from discovered hardware**, never a
/// hard-wired ceiling. The heap window and the heap region each track
/// physical RAM (the true upper bound on backable anonymous pages), floored
/// at [`ANON_WINDOW_MIN_PAGES`] so a tiny machine still gets a workable heap,
/// and capped at a quarter of the available span so a RAM-rich machine never
/// starves the file window; the file window takes the whole remainder,
/// because a demand-paged file view is bounded by address space, not RAM.
/// On a 1 GiB machine the heap window is the same 1 GiB it always was; on
/// aarch64's 512 GiB user span the file window spans hundreds of
/// gigabytes, and on x86_64's 128 TiB span it comfortably holds a
/// multi-terabyte file view.
///
/// `user_va_top` is supplied by each port: the paging mode dictates it, so
/// it is genuinely target-specific and this neutral policy never names an
/// architecture.
#[must_use]
pub fn user_windows(ram_frames: u64, anon_window_base: u64, user_va_top: u64) -> UserWindows {
    let page = PAGE_SIZE as u64;
    let va_fit_pages = user_va_top.saturating_sub(anon_window_base) / page;
    // Two RAM-sized spans must fit above the base, so the floor yields to
    // half the span, and the cap leaves the file window at least half.
    let floor = (ANON_WINDOW_MIN_PAGES as u64).min(va_fit_pages / 2);
    let cap = (va_fit_pages / 4).max(floor);
    let ram_sized = ram_frames.clamp(floor, cap);
    let heap = HeapRegion {
        base: anon_window_base + ram_sized * page,
        len: ram_sized * page,
    };
    let file_pages = va_fit_pages - 2 * ram_sized;
    // The bare-metal ports are 64-bit, so these never truncate; clamp to
    // the platform word width rather than ever panicking on the cast.
    UserWindows {
        anon_pages: usize::try_from(ram_sized).unwrap_or(usize::MAX),
        heap,
        file_base: heap.base + heap.len,
        file_pages: usize::try_from(file_pages).unwrap_or(usize::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::{user_windows, ANON_WINDOW_MIN_PAGES};
    use tairix_kernel_mem::PAGE_SIZE;

    // A representative per-arch user-VA ceiling (aarch64's 512 GiB TTBR0
    // window); the exact value only has to be far above the window base.
    const USER_VA_TOP: u64 = 1 << 39;
    // The window sits 4 GiB above a notional image bias of 0 (the topmost
    // user region — `spawn_layout::ANON_WINDOW_OFFSET`).
    const BASE: u64 = 0x1_0000_0000;

    #[test]
    fn heap_scales_with_ram_on_a_large_machine() {
        // 64 GiB of RAM: the heap window tracks physical frames, not a
        // 1 GiB cap, and the file window takes everything else.
        let ram_frames = 64 * 1024 * 1024 * 1024 / PAGE_SIZE as u64;
        let w = user_windows(ram_frames, BASE, USER_VA_TOP);
        assert_eq!(w.anon_pages as u64, ram_frames);
        let ram_bytes = ram_frames * PAGE_SIZE as u64;
        assert_eq!(w.heap.base, BASE + ram_bytes);
        assert_eq!(w.heap.len, ram_bytes);
        assert_eq!(w.file_base, BASE + 2 * ram_bytes);
        let va_fit = (USER_VA_TOP - BASE) / PAGE_SIZE as u64;
        assert_eq!(w.file_pages as u64, va_fit - 2 * ram_frames);
    }

    #[test]
    fn one_gib_machine_matches_the_former_fixed_window() {
        // The former hand-wired constant was 0x4_0000 pages = 1 GiB; a 1 GiB
        // machine still yields exactly that, so nothing regressed for the
        // common case while larger machines now scale up.
        let ram_frames = 1024 * 1024 * 1024 / PAGE_SIZE as u64;
        let w = user_windows(ram_frames, BASE, USER_VA_TOP);
        assert_eq!(w.anon_pages, 0x4_0000);
        assert_eq!(w.heap.base, BASE + (0x4_0000u64) * PAGE_SIZE as u64);
        assert_eq!(w.file_base, BASE + (0x8_0000u64) * PAGE_SIZE as u64);
    }

    #[test]
    fn the_file_window_spans_far_beyond_ram() {
        // The point of the split: a 1 GiB machine's file window still spans
        // hundreds of gigabytes of address space, so a file view is bounded
        // by the file (and the VA span), never by RAM.
        let ram_frames = 1024 * 1024 * 1024 / PAGE_SIZE as u64;
        let w = user_windows(ram_frames, BASE, USER_VA_TOP);
        let file_bytes = (w.file_pages as u64) * PAGE_SIZE as u64;
        assert!(file_bytes > 100 * (1u64 << 30), "file window ≥ 100 GiB");
    }

    #[test]
    fn ram_rivalling_the_address_space_still_leaves_the_file_window_half() {
        // A machine whose RAM covers the whole user span: the heap window and
        // the heap region are capped at a quarter each so file mappings keep
        // the other half.
        let va_fit = (USER_VA_TOP - BASE) / PAGE_SIZE as u64;
        let w = user_windows(va_fit * 2, BASE, USER_VA_TOP);
        assert_eq!(w.anon_pages as u64, va_fit / 4);
        assert_eq!(w.heap.len, va_fit / 4 * PAGE_SIZE as u64);
        assert_eq!(w.file_pages as u64, va_fit - 2 * (va_fit / 4));
    }

    #[test]
    fn the_heap_region_lies_between_the_heap_window_and_the_file_window() {
        for ram_frames in [100, 1 << 18, 1 << 24, 1 << 40] {
            let w = user_windows(ram_frames, BASE, USER_VA_TOP);
            let anon_top = BASE + w.anon_pages as u64 * PAGE_SIZE as u64;
            assert_eq!(w.heap.base, anon_top, "{ram_frames}");
            assert_eq!(w.heap.len, w.anon_pages as u64 * PAGE_SIZE as u64);
            assert_eq!(w.file_base, w.heap.base + w.heap.len);
            let file_top = w.file_base + w.file_pages as u64 * PAGE_SIZE as u64;
            assert!(file_top <= USER_VA_TOP, "{ram_frames}");
            assert!(w.heap.is_well_formed());
        }
    }

    #[test]
    fn tiny_machine_gets_the_floor() {
        // Far less RAM than the floor: the window costs no RAM until backed,
        // so a tiny board still gets a usable heap window (the floor), and a
        // map beyond real RAM still fails closed at frame exhaustion.
        let w = user_windows(100, BASE, USER_VA_TOP);
        assert_eq!(w.anon_pages, ANON_WINDOW_MIN_PAGES);
    }

    #[test]
    fn clamped_to_addressable_user_space() {
        // Enormous RAM but a deliberately tiny VA window above the base: the
        // heap window and region can never exceed what the address space can
        // hold (the floor yields to half the span), and the file window
        // degrades to zero rather than wrapping.
        let tiny_top = BASE + 256 * PAGE_SIZE as u64;
        let huge_ram = 1u64 << 40;
        let w = user_windows(huge_ram, BASE, tiny_top);
        assert_eq!(w.anon_pages, 128);
        assert_eq!(w.heap.base + w.heap.len, tiny_top);
        assert_eq!(w.file_pages, 0);
    }

    #[test]
    fn degenerate_base_at_or_above_ceiling_yields_zero() {
        // A base at (or past) the user-VA top leaves no room: both windows
        // are empty, which `LiveSpace::new` then rejects fail-closed for the
        // heap (and skips for the file window) rather than wrapping into a
        // bogus span.
        let w = user_windows(1 << 30, USER_VA_TOP, USER_VA_TOP);
        assert_eq!(w.anon_pages, 0);
        assert_eq!(w.heap.len, 0);
        assert_eq!(w.file_pages, 0);
        let w = user_windows(1 << 30, USER_VA_TOP + 1, USER_VA_TOP);
        assert_eq!(w.anon_pages, 0);
        assert_eq!(w.file_pages, 0);
    }
}

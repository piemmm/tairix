//! Host tests for the hosted console: surfaces of host memory, a port that
//! reaches them directly, and the lock discipline exercised on one thread.

extern crate std;

use alloc::boxed::Box;
use alloc::vec;
use core::cell::Cell as Slot;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::driver::display::{DisplayFormat, DisplayMode};
use tairix_sync::NopInterruptControl;

use super::{AdoptRefused, GridSource};
use crate::port::{ConsolePort, ConsoleSurface};
use crate::{Cell, Geometry, Surface, CELL_HEIGHT, CELL_WIDTH, DEFAULT_BACKGROUND};

/// Grids on the test's heap.
struct HeapGrids;

impl GridSource for HeapGrids {
    type Grid = alloc::vec::Vec<Cell>;

    fn blank(cells: usize) -> Option<Self::Grid> {
        Some(vec![Cell::BLANK; cells])
    }
}

/// A console over the test's heap.
type FramebufferConsole<P> = super::FramebufferConsole<P, HeapGrids>;

std::thread_local! {
    /// The one block the host port's direct map covers, by its address.
    static DIRECT_MAP: Slot<Option<NonNull<u32>>> = const { Slot::new(None) };
    /// How many times this thread's consoles called their damage hook.
    static WAKES: Slot<usize> = const { Slot::new(0) };
}

fn wake() {
    WAKES.with(|wakes| wakes.set(wakes.get() + 1));
}

/// The damage hook's calls since the last read.
fn wakes() -> usize {
    WAKES.with(|wakes| wakes.replace(0))
}

/// A port whose memory is the host's own; its direct map covers the block a
/// test registered.
struct HostPort;

impl ConsolePort for HostPort {
    type Irq = NopInterruptControl;

    const CLEANS: bool = false;

    fn reachable(_base: NonNull<u32>, _bytes: usize) -> bool {
        true
    }

    fn direct_map(phys: u64, _len: u64) -> Option<NonNull<u32>> {
        DIRECT_MAP
            .with(Slot::get)
            .filter(|block| block.as_ptr().addr() as u64 == phys)
    }

    fn publish(_first: NonNull<u32>, _bytes: usize) {}
}

/// A port none of whose memory translates.
struct UnmappedPort;

impl ConsolePort for UnmappedPort {
    type Irq = NopInterruptControl;

    const CLEANS: bool = true;

    fn reachable(_base: NonNull<u32>, _bytes: usize) -> bool {
        false
    }

    fn direct_map(_phys: u64, _len: u64) -> Option<NonNull<u32>> {
        None
    }

    fn publish(_first: NonNull<u32>, _bytes: usize) {}
}

/// Scan-out memory for a test surface. Declared before the console that
/// paints it, so it is dropped after that console, as a firmware or kernel
/// block outlives everything that paints it.
struct Memory {
    base: NonNull<u32>,
    len: usize,
}

impl Memory {
    /// The pixels, read through the address the console writes through.
    fn read(&self) -> alloc::vec::Vec<u32> {
        // SAFETY: `len` initialised pixels live at `base` until `drop`; the
        // test runs on one thread and nothing writes them during the read.
        unsafe { core::slice::from_raw_parts(self.base.as_ptr(), self.len) }.to_vec()
    }

    fn lit(&self) -> bool {
        self.read().iter().any(|&pixel| pixel != DEFAULT_BACKGROUND)
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let slice = core::ptr::slice_from_raw_parts_mut(self.base.as_ptr(), self.len);
        // SAFETY: `base`/`len` came from `Box::into_raw` in `surface`, and
        // every console painting them was dropped first.
        drop(unsafe { Box::from_raw(slice) });
    }
}

/// Scan-out memory for a `cols`×`rows`-cell surface, and its placement.
fn surface(cols: u32, rows: u32) -> (Memory, ConsoleSurface) {
    let mode = DisplayMode {
        width_px: cols * CELL_WIDTH,
        height_px: rows * CELL_HEIGHT,
        stride_bytes: cols * CELL_WIDTH * 4,
        format: DisplayFormat::Bgra8888,
    };
    let len = (mode.width_px * mode.height_px) as usize;
    let raw = Box::into_raw(vec![0x1234_5678u32; len].into_boxed_slice());
    let base = NonNull::new(raw.cast::<u32>()).unwrap_or(NonNull::dangling());
    (
        Memory { base, len },
        ConsoleSurface {
            phys: base.as_ptr().addr() as u64,
            mode,
            kernel_owned: false,
        },
    )
}

fn grid(cols: u32, rows: u32) -> Geometry {
    Geometry {
        width_px: cols * CELL_WIDTH,
        height_px: rows * CELL_HEIGHT,
        stride_px: cols * CELL_WIDTH,
    }
}

/// A console attached to a published `cols`×`rows` surface. The memory comes
/// first so a caller binding `(memory, console, ..)` drops it last.
fn attached(cols: u32, rows: u32) -> (Memory, FramebufferConsole<HostPort>, ConsoleSurface) {
    let console = FramebufferConsole::<HostPort>::new();
    let (memory, placement) = surface(cols, rows);
    // SAFETY: one thread, before any other call; the memory is the test's.
    assert!(unsafe { console.publish(memory.base, placement) });
    assert!(console.attach(None));
    (memory, console, placement)
}

#[test]
fn a_published_surface_is_cleared_and_written_to_once_attached() {
    let (memory, console, placement) = attached(8, 2);
    assert!(
        memory.read().iter().all(|&p| p != 0x1234_5678),
        "whatever the memory held is gone"
    );
    console.on_damage(wake);
    console.write_bytes(b"\x1b[?25l");
    assert_eq!(wakes(), 0, "no host asked to be told");
    assert!(memory.read().iter().all(|&p| p == DEFAULT_BACKGROUND));
    console.write_bytes(b"A");
    assert!(memory.lit());
    assert_eq!(console.placement(), Some(placement));
}

#[test]
fn a_second_attach_is_refused() {
    let (_memory, console, _) = attached(4, 2);
    assert!(!console.attach(None));
    assert!(!console.attach(Some(grid(4, 2))));
}

#[test]
fn with_nothing_published_and_no_host_to_wait_for_nothing_attaches() {
    let console = FramebufferConsole::<HostPort>::new();
    console.on_damage(wake);
    assert!(!console.attach(None));
    assert!(!console.is_active());
    console.write_bytes(b"lost");
    assert_eq!(wakes(), 0);
    assert_eq!(console.text_grid(), None);
}

#[test]
fn an_unhosted_console_keeps_what_it_is_told_until_a_driver_hosts_it() {
    let console = FramebufferConsole::<HostPort>::new();
    console.on_damage(wake);
    assert!(console.attach(Some(grid(8, 2))));
    console.write_output_bytes(b"\x1b[?25lboot");
    assert_eq!(wakes(), 0, "nothing painted, nothing owed");
    assert_eq!(console.placement(), None);

    let (memory, placement) = surface(12, 3);
    assert_eq!(console.adopt(memory.base, placement, true, true), Ok(()));
    assert_eq!(wakes(), 1, "the first paint is owed");
    assert!(memory.lit(), "the boot's text is what appears");
    assert_eq!(
        console.text_grid().map(|g| (g.rows(), g.cols())),
        Some((3, 12))
    );
    assert_eq!(
        console.take_damage(),
        Some((0, placement.mode.height_px)),
        "the first paint is the whole surface"
    );
    assert_eq!(console.take_damage(), None);
}

#[test]
fn a_host_is_told_once_rows_are_owed_and_not_again_until_it_takes_them() {
    let console = FramebufferConsole::<HostPort>::new();
    assert!(console.attach(Some(grid(8, 2))));
    let (memory, placement) = surface(8, 2);
    assert_eq!(console.adopt(memory.base, placement, true, true), Ok(()));
    assert!(console.owes_damage());
    assert!(console.take_damage().is_some());
    assert!(!console.owes_damage());
    console.on_damage(wake);
    console.write_bytes(b"a");
    assert_eq!(wakes(), 1, "the first owed rows wake the host");
    assert!(console.owes_damage(), "a peek takes nothing");
    console.write_bytes(b"b");
    assert_eq!(wakes(), 0, "rows already owed wake nobody");
    assert_eq!(console.take_damage(), Some((0, CELL_HEIGHT)));
    console.write_bytes(b"c");
    assert_eq!(wakes(), 1);
}

#[test]
fn a_host_that_did_not_ask_to_be_told_is_never_woken() {
    let console = FramebufferConsole::<HostPort>::new();
    console.on_damage(wake);
    assert!(console.attach(Some(grid(8, 2))));
    let (memory, placement) = surface(8, 2);
    assert_eq!(console.adopt(memory.base, placement, false, true), Ok(()));
    console.write_bytes(b"scanned directly");
    assert_eq!(wakes(), 0);
    assert_eq!(console.take_damage(), None);
}

#[test]
fn a_console_handed_to_a_session_paints_and_owes_nothing_until_it_is_shown() {
    let console = FramebufferConsole::<HostPort>::new();
    assert!(console.attach(Some(grid(8, 2))));
    console.set_surface(Surface::Hidden);
    let (memory, placement) = surface(8, 2);
    console.on_damage(wake);
    assert_eq!(console.adopt(memory.base, placement, true, true), Ok(()));
    console.write_bytes(b"x");
    assert_eq!(wakes(), 0);
    assert_eq!(console.take_damage(), None);
    assert!(memory.read().iter().all(|&p| p == 0x1234_5678), "untouched");
    console.set_surface(Surface::Shown);
    assert_eq!(wakes(), 1, "taking the screen back owes all of it");
    assert!(memory.lit());
}

#[test]
fn a_new_host_surface_of_another_size_re_cuts_the_grid() {
    let console = FramebufferConsole::<HostPort>::new();
    assert!(console.attach(Some(grid(4, 4))));
    let (first, at_first) = surface(4, 4);
    assert_eq!(console.adopt(first.base, at_first, false, true), Ok(()));
    let (second, at_second) = surface(10, 2);
    assert_eq!(console.adopt(second.base, at_second, false, true), Ok(()));
    assert_eq!(
        console.text_grid().map(|g| (g.rows(), g.cols())),
        Some((2, 10))
    );
    assert_eq!(console.placement(), Some(at_second));
}

#[test]
fn a_host_surface_that_does_not_translate_is_refused() {
    let console = FramebufferConsole::<UnmappedPort>::new();
    assert!(console.attach(Some(grid(4, 2))));
    let (memory, placement) = surface(4, 2);
    assert_eq!(
        console.adopt(memory.base, placement, false, true),
        Err(AdoptRefused::Unreachable)
    );
    let mut degenerate = placement;
    degenerate.mode.width_px = 1;
    degenerate.mode.stride_bytes = 4;
    assert_eq!(
        console.adopt(memory.base, degenerate, false, true),
        Err(AdoptRefused::Geometry)
    );
}

#[test]
fn a_console_never_attached_cannot_be_hosted() {
    let console = FramebufferConsole::<HostPort>::new();
    let (memory, placement) = surface(4, 2);
    assert_eq!(
        console.adopt(memory.base, placement, false, true),
        Err(AdoptRefused::NoConsole)
    );
}

#[test]
fn a_published_surface_the_regime_does_not_map_is_never_attached() {
    let (memory, placement) = surface(4, 2);
    let console = FramebufferConsole::<UnmappedPort>::new();
    // SAFETY: one thread, before any other call; the memory is the test's.
    assert!(unsafe { console.publish(memory.base, placement) });
    assert!(!console.attach(None));
    assert!(!console.is_active());
}

#[test]
fn a_vacated_console_paints_nothing_and_keeps_its_text_for_its_next_host() {
    let (first, at_first) = surface(8, 2);
    let (second, at_second) = surface(8, 2);
    let console = FramebufferConsole::<HostPort>::new();
    assert!(!console.vacate(at_first.phys), "nothing attached");
    console.on_damage(wake);
    assert!(console.attach(Some(grid(8, 2))));
    assert_eq!(console.adopt(first.base, at_first, true, true), Ok(()));
    assert_eq!(wakes(), 1);

    assert!(
        !console.vacate(at_second.phys),
        "only the surface it paints"
    );
    assert!(console.vacate(at_first.phys));
    assert!(!console.vacate(at_first.phys), "vacated once");
    assert_eq!(console.placement(), None);
    assert!(
        !console.owes_damage(),
        "what was owed went with the surface"
    );

    let before = first.read();
    console.write_output_bytes(b"\x1b[?25lkept");
    assert_eq!(first.read(), before, "the vacated surface is left alone");
    assert_eq!(wakes(), 0, "nothing painted, nothing owed");

    assert_eq!(console.adopt(second.base, at_second, true, true), Ok(()));
    assert!(second.lit(), "the text written meanwhile appears");
    assert_eq!(wakes(), 1);
}

/// The regression for the report that hung on its own lock: a fatal path
/// that stopped every other CPU busts the lock, and the record renders
/// although this CPU still holds it.
#[test]
fn a_busted_console_renders_with_its_lock_held() {
    let (memory, console, _) = attached(8, 2);
    let held = console.lock.lock();
    console.fatal(true);
    console.write_bytes(b"\x1b[?25lpanic");
    assert!(memory.lit());
    drop(held);
}

/// A fatal path that could not stop every CPU may not render over a lock a
/// live peer holds, nor wait for it: a report raised inside the renderer
/// would wait for its own frame. The screen copy is dropped instead.
#[test]
fn a_fatal_console_with_a_peer_alive_drops_a_write_it_cannot_lock() {
    let (memory, console, _) = attached(8, 2);
    console.write_bytes(b"\x1b[?25l");
    let before = memory.read();
    console.fatal(false);
    let held = console.lock.lock();
    console.write_bytes(b"panic");
    assert_eq!(memory.read(), before, "nothing painted under a held lock");
    drop(held);
    console.write_bytes(b"panic");
    assert!(memory.lit(), "rendered once the lock is free");
}

/// A fatal reclaim that loses the lock to the reporting CPU's own frame is
/// owed, not dropped: the report's first busted paint takes the screen back.
#[test]
fn a_contended_fatal_reclaim_is_taken_by_the_report_s_first_paint() {
    let (memory, console, _) = attached(4, 2);
    console.set_surface(Surface::Hidden);
    console.write_bytes(b"hidden");
    let before = memory.read();
    let held = console.lock.lock();
    console.reclaim_surface();
    assert_eq!(memory.read(), before, "nothing painted while contended");
    console.fatal(true);
    console.write_bytes(b"panic");
    assert_ne!(memory.read(), before, "the record takes the screen back");
    drop(held);
}

#[test]
fn a_free_fatal_reclaim_takes_the_screen_back_at_once() {
    let (memory, console, _) = attached(4, 2);
    console.set_surface(Surface::Hidden);
    console.write_bytes(b"hidden");
    let before = memory.read();
    console.reclaim_surface();
    assert_ne!(memory.read(), before);
}

/// A device that does not snoop the caches reads stale lines unless the port
/// cleans them; a port that cannot is never handed such a surface.
#[test]
fn a_port_that_cannot_clean_refuses_a_surface_its_device_does_not_snoop() {
    let console = FramebufferConsole::<HostPort>::new();
    assert!(console.attach(Some(grid(4, 2))));
    let (memory, placement) = surface(4, 2);
    assert_eq!(
        console.adopt(memory.base, placement, true, false),
        Err(AdoptRefused::Incoherent)
    );
    assert!(!console.paints());
    assert_eq!(console.adopt(memory.base, placement, true, true), Ok(()));
    assert!(console.paints());
}

/// A console no engine will ever host stops being attached, so the port
/// keeps its serial console rather than a login on a screen nobody sees.
#[test]
fn a_retired_console_is_no_longer_attached_and_paints_nothing() {
    let (memory, console, placement) = attached(8, 2);
    assert!(console.paints());
    assert!(
        !console.retire(placement.phys + 4),
        "only the surface it paints"
    );
    assert!(console.retire(placement.phys));
    assert!(!console.is_active());
    assert!(!console.paints());
    let before = memory.read();
    console.write_bytes(b"after");
    assert_eq!(memory.read(), before);
}

#[test]
fn an_unhosted_console_paints_nothing_until_a_driver_hosts_it() {
    let console = FramebufferConsole::<HostPort>::new();
    assert!(console.attach(Some(grid(8, 2))));
    assert!(console.is_active() && !console.paints());
    let (memory, placement) = surface(8, 2);
    assert_eq!(console.adopt(memory.base, placement, true, true), Ok(()));
    assert!(console.paints());
    assert!(console.vacate(placement.phys));
    assert!(console.is_active() && !console.paints());
}

#[test]
fn a_paint_never_runs_before_attach() {
    let wrote = AtomicBool::new(false);
    let console = FramebufferConsole::<HostPort>::new();
    let painted = console.paint(|_, _| {
        wrote.store(true, Ordering::Relaxed);
        None
    });
    assert!(!painted && !wrote.load(Ordering::Relaxed));
}
